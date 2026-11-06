//! Physical storage backends for the WeftFS store core.
//!
//! The store never manipulates filesystem paths directly. It addresses every
//! durable artifact through a [`Key`] and performs physical I/O through a
//! [`StorageBackend`]. The commit protocol (write blobs, fsync, publish the
//! commit record, publish the superblock slot, fsync the root) is unchanged;
//! only the publication primitive differs per backend.
//!
//! [`StorageBackend`] is the store's unified I/O surface. Since subphase R10
//! it is also the adapter over two narrower contracts in [`crate::planes`]:
//! [`crate::planes::BlobPlane`] (immutable bytes) and
//! [`crate::planes::CommitPlane`] (versioned control state). A
//! [`BackendKind::Split`] runs both planes on different physical systems; the
//! [`CompositeBackend`] below routes each [`Key`] to one of them.
//!
//! Three single backends ship today:
//!
//! * [`LocalBackend`] — the normative layout from `WEFT-MODEL.md` §2:
//!   `tmp/` staging plus `fsync` + `rename(2)` + directory `fsync`.
//! * [`ObjectBackend`] — an immutable-object namespace where every key maps to
//!   `objects/<2-hex>/<name>`. Publication is `fsync` + `link(2)` + unlink of
//!   the temporary name, so it does not depend on POSIX `rename` replacement.
//!   If `link(2)` is unavailable the backend uses `rename(2)` and says so in
//!   its [`Capabilities::publication`] text and diagnostics.
//! * [`S3Backend`] — a real S3-compatible endpoint (see `src/s3.rs`).
//!
//! A caller may also declare a [`BackendKind::Posix`] backend with weaker
//! capabilities (for example a network filesystem that does not honour
//! `fsync`). The store consults [`Capabilities`] and fails closed: a writable
//! open requires `durable_fsync`, any publication requires `atomic_publish`,
//! and a writable open requires `conditional_head`. No degraded mode exists.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use fs2::FileExt as Fs2FileExt;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::format::MAX_CHUNK_SIZE;
use crate::gcs::{GcsBackend, GcsConfig};
use crate::identity::Hash32;
use crate::s3::{S3Backend, S3Config};

/// Logical namespace of a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyKind {
    /// Store root directory (used for directory fsync).
    Root,
    /// `FORMAT` marker.
    Format,
    /// Writer lock file.
    Lock,
    /// Two-slot superblock.
    Superblock,
    /// `commits/<generation>.commit`.
    Commit,
    /// `objects/<ab>/<hash>` raw chunk.
    Object,
    /// `manifests/<ab>/<hash>` canonical WCB manifest.
    Manifest,
    /// `indexes/<ab>/<commit>.idx` advisory selector index.
    Index,
    /// `refs/snapshots/<name>` pointer file.
    SnapshotRef,
    /// `refs/tags/<name>` pointer file.
    Tag,
    /// Staging file under `tmp/`.
    Tmp,
    /// A physical entry that does not follow the kind's naming rule. Only
    /// produced by `list_prefix`, so garbage collection can report it.
    Other,
    /// `quota` store-root record: the persisted write-budget policy and the
    /// usage counters derived from committed state.
    Quota,
    /// `quota.migrated` store-root record: the legacy combined
    /// policy+usage file, preserved when it is imported into committed state.
    QuotaMigrated,
    /// `access/counts.v1`: advisory per-object read counters.
    Access,
    /// `packs/<ab>/<hash>`: a physical pack of small immutable blobs
    /// (subphase R11). Always on the blob plane.
    Pack,
    /// `pack-catalog/<ab>/<hash>`: an immutable content-addressed catalog
    /// segment naming pack members. Always on the blob plane.
    PackCatalog,
    /// `pack-catalog.root`: the mutable catalog root control record. Always on
    /// the commit plane and replaced by compare-and-swap.
    PackCatalogRoot,
}

impl KeyKind {
    /// Stable spelling used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Root => "root",
            KeyKind::Format => "format",
            KeyKind::Lock => "lock",
            KeyKind::Superblock => "superblock",
            KeyKind::Commit => "commit",
            KeyKind::Object => "object",
            KeyKind::Manifest => "manifest",
            KeyKind::Index => "index",
            KeyKind::SnapshotRef => "snapshot-ref",
            KeyKind::Tag => "tag",
            KeyKind::Tmp => "tmp",
            KeyKind::Other => "other",
            KeyKind::Quota => "quota",
            KeyKind::QuotaMigrated => "quota-migrated",
            KeyKind::Access => "access",
            KeyKind::Pack => "pack",
            KeyKind::PackCatalog => "pack-catalog",
            KeyKind::PackCatalogRoot => "pack-catalog-root",
        }
    }
}

/// A small typed store key: a kind plus the logical path components that name
/// it. Backends are never handed a raw filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Root,
    Format,
    Lock,
    Superblock,
    Commit {
        generation: u64,
    },
    Object {
        hash: Hash32,
    },
    Manifest {
        hash: Hash32,
    },
    Index {
        commit: Hash32,
    },
    SnapshotRef {
        name: String,
    },
    Tag {
        name: String,
    },
    Tmp {
        name: String,
    },
    /// The single mutable `quota` record at the store root.
    Quota,
    /// The preserved legacy `quota` record (`quota.migrated`).
    QuotaMigrated,
    /// Advisory read-counter snapshot (`access/counts.v1`).
    AccessCounts,
    /// Advisory read-counter journal (`access/journal.v1`).
    AccessJournal,
    /// One physical pack object (subphase R11).
    Pack {
        hash: Hash32,
    },
    /// One immutable catalog segment object (subphase R11).
    PackCatalog {
        hash: Hash32,
    },
    /// The catalog root control record (subphase R11).
    PackCatalogRoot,
    /// Prefix selector accepted by [`StorageBackend::list_prefix`].
    Prefix(KeyKind),
    /// An entry discovered by `list_prefix` that does not follow the naming
    /// rule for its kind. `name` is the backend's relative physical name.
    Other {
        kind: KeyKind,
        name: String,
    },
}

impl Key {
    pub fn root() -> Key {
        Key::Root
    }

    pub fn format() -> Key {
        Key::Format
    }

    pub fn lock() -> Key {
        Key::Lock
    }

    pub fn superblock() -> Key {
        Key::Superblock
    }

    pub fn commit(generation: u64) -> Key {
        Key::Commit { generation }
    }

    pub fn object(hash: Hash32) -> Key {
        Key::Object { hash }
    }

    pub fn manifest(hash: Hash32) -> Key {
        Key::Manifest { hash }
    }

    pub fn index(commit: Hash32) -> Key {
        Key::Index { commit }
    }

    pub fn snapshot_ref(name: impl Into<String>) -> Key {
        Key::SnapshotRef { name: name.into() }
    }

    pub fn tag(name: impl Into<String>) -> Key {
        Key::Tag { name: name.into() }
    }

    pub fn tmp(name: impl Into<String>) -> Key {
        Key::Tmp { name: name.into() }
    }

    pub fn quota() -> Key {
        Key::Quota
    }

    pub fn quota_migrated() -> Key {
        Key::QuotaMigrated
    }

    pub fn access_counts() -> Key {
        Key::AccessCounts
    }

    pub fn access_journal() -> Key {
        Key::AccessJournal
    }

    pub fn pack(hash: Hash32) -> Key {
        Key::Pack { hash }
    }

    pub fn pack_catalog(hash: Hash32) -> Key {
        Key::PackCatalog { hash }
    }

    pub fn pack_catalog_root() -> Key {
        Key::PackCatalogRoot
    }

    pub fn prefix(kind: KeyKind) -> Key {
        Key::Prefix(kind)
    }

    pub fn kind(&self) -> KeyKind {
        match self {
            Key::Root => KeyKind::Root,
            Key::Format => KeyKind::Format,
            Key::Lock => KeyKind::Lock,
            Key::Superblock => KeyKind::Superblock,
            Key::Commit { .. } => KeyKind::Commit,
            Key::Object { .. } => KeyKind::Object,
            Key::Manifest { .. } => KeyKind::Manifest,
            Key::Index { .. } => KeyKind::Index,
            Key::SnapshotRef { .. } => KeyKind::SnapshotRef,
            Key::Tag { .. } => KeyKind::Tag,
            Key::Tmp { .. } => KeyKind::Tmp,
            Key::Quota => KeyKind::Quota,
            Key::QuotaMigrated => KeyKind::QuotaMigrated,
            Key::AccessCounts | Key::AccessJournal => KeyKind::Access,
            Key::Pack { .. } => KeyKind::Pack,
            Key::PackCatalog { .. } => KeyKind::PackCatalog,
            Key::PackCatalogRoot => KeyKind::PackCatalogRoot,
            Key::Prefix(kind) => *kind,
            Key::Other { kind, .. } => *kind,
        }
    }

    pub fn is_prefix(&self) -> bool {
        matches!(self, Key::Prefix(_))
    }

    /// Shard directory component (first two hex characters) for hash-addressed
    /// keys, `None` otherwise.
    pub fn shard(&self) -> Option<String> {
        match self {
            Key::Object { hash }
            | Key::Manifest { hash }
            | Key::Pack { hash }
            | Key::PackCatalog { hash } => Some(hash.to_hex()[0..2].to_owned()),
            Key::Index { commit } => Some(commit.to_hex()[0..2].to_owned()),
            _ => None,
        }
    }

    /// Logical file name within the kind's namespace.
    pub fn name(&self) -> Option<&str> {
        match self {
            Key::Commit { .. } => None,
            Key::Object { .. }
            | Key::Manifest { .. }
            | Key::Index { .. }
            | Key::Pack { .. }
            | Key::PackCatalog { .. }
            | Key::PackCatalogRoot => None,
            Key::SnapshotRef { name } | Key::Tag { name } | Key::Tmp { name } => Some(name),
            Key::Other { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Logical relative path used in diagnostics and receipts.
    pub fn display_path(&self) -> String {
        match self {
            Key::Root => ".".to_owned(),
            Key::Format => "FORMAT".to_owned(),
            Key::Lock => "LOCK".to_owned(),
            Key::Superblock => "superblock".to_owned(),
            Key::Commit { generation } => format!("commits/{generation}.commit"),
            Key::Object { hash } => {
                let hex = hash.to_hex();
                format!("objects/{}/{}", &hex[0..2], hex)
            }
            Key::Manifest { hash } => {
                let hex = hash.to_hex();
                format!("manifests/{}/{}", &hex[0..2], hex)
            }
            Key::Index { commit } => {
                let hex = commit.to_hex();
                format!("indexes/{}/{hex}.idx", &hex[0..2])
            }
            Key::SnapshotRef { name } => format!("refs/snapshots/{name}"),
            Key::Tag { name } => format!("refs/tags/{name}"),
            Key::Tmp { name } => format!("tmp/{name}"),
            Key::Quota => "quota".to_owned(),
            Key::QuotaMigrated => "quota.migrated".to_owned(),
            Key::AccessCounts => "access/counts.v1".to_owned(),
            Key::AccessJournal => "access/journal.v1".to_owned(),
            Key::Pack { hash } => {
                let hex = hash.to_hex();
                format!("packs/{}/{hex}", &hex[0..2])
            }
            Key::PackCatalog { hash } => {
                let hex = hash.to_hex();
                format!("pack-catalog/{}/{hex}", &hex[0..2])
            }
            Key::PackCatalogRoot => "pack-catalog.root".to_owned(),
            Key::Prefix(kind) => format!("{}/*", kind.as_str()),
            Key::Other { name, .. } => name.clone(),
        }
    }
}

/// Logical prefix of one listed kind, in the normative on-disk layout.
///
/// Shared by every backend that lists object names, so a remote listing cannot
/// drift from the layout a local store writes.
pub(crate) fn list_prefix_path(kind: KeyKind) -> Option<&'static str> {
    match kind {
        KeyKind::Commit => Some("commits/"),
        KeyKind::Object => Some("objects/"),
        KeyKind::Manifest => Some("manifests/"),
        KeyKind::Index => Some("indexes/"),
        KeyKind::SnapshotRef => Some("refs/snapshots/"),
        KeyKind::Tag => Some("refs/tags/"),
        KeyKind::Tmp => Some("tmp/"),
        KeyKind::Access => Some("access/"),
        KeyKind::Pack => Some("packs/"),
        KeyKind::PackCatalog => Some("pack-catalog/"),
        _ => None,
    }
}

/// Classify one listed object name (prefix already stripped) as a [`Key`].
///
/// `None` means the name does not follow the kind's naming rule; the caller
/// reports it as [`Key::Other`] so GC can see the stranger.
pub(crate) fn classify_listed_key(kind: KeyKind, relative: &str) -> Option<Key> {
    match kind {
        KeyKind::Object | KeyKind::Manifest => {
            let (shard, name) = relative.split_once('/')?;
            let hash = Hash32::from_lower_hex(name).ok()?;
            if hash.to_hex()[0..2] != *shard {
                return None;
            }
            Some(if kind == KeyKind::Object {
                Key::object(hash)
            } else {
                Key::manifest(hash)
            })
        }
        KeyKind::Index => {
            let (shard, name) = relative.split_once('/')?;
            let stem = name.strip_suffix(".idx")?;
            let commit = Hash32::from_lower_hex(stem).ok()?;
            if commit.to_hex()[0..2] != *shard {
                return None;
            }
            Some(Key::index(commit))
        }
        KeyKind::Commit => {
            let stem = relative.strip_suffix(".commit")?;
            if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(Key::commit(stem.parse::<u64>().ok()?))
        }
        KeyKind::SnapshotRef => Some(Key::snapshot_ref(relative)),
        KeyKind::Tag => Some(Key::tag(relative)),
        KeyKind::Tmp => Some(Key::tmp(relative)),
        KeyKind::Access => match relative {
            "counts.v1" => Some(Key::access_counts()),
            "journal.v1" => Some(Key::access_journal()),
            _ => None,
        },
        KeyKind::Pack | KeyKind::PackCatalog => {
            let (shard, name) = relative.split_once('/')?;
            let hash = Hash32::from_lower_hex(name).ok()?;
            if hash.to_hex()[0..2] != *shard {
                return None;
            }
            Some(if kind == KeyKind::Pack {
                Key::pack(hash)
            } else {
                Key::pack_catalog(hash)
            })
        }
        _ => None,
    }
}

/// Declared guarantees of a backend.
///
/// The store treats these as mandatory contracts, not hints. Missing
/// guarantees fail operations closed; they never silently downgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// A write becomes visible all-or-nothing under its final key. Satisfied by
    /// POSIX `rename(2)` over a fully written and fsynced temporary file, or
    /// by `link(2)` of such a file (write-once).
    pub atomic_publish: bool,
    /// `sync`/`put_atomic`/`write_at` reach stable storage before returning.
    pub durable_fsync: bool,
    /// `read(key, offset, len)` reads only the requested range instead of
    /// materialising the whole object.
    pub true_range_reads: bool,
    /// Largest single object the backend accepts.
    pub max_object_bytes: u64,
    /// Conditional control-state publication: the backend can replace the
    /// superblock/head only if it still holds the record the caller read. The
    /// local and object backends satisfy this under the exclusive writer lock
    /// plus a content-token check; S3 satisfies it with `If-Match` and proves
    /// it at open.
    pub conditional_head: bool,
    /// Human-readable consistency description. No machine interpretation.
    pub consistency: String,
    /// The publication primitive actually exercised, for receipts.
    pub publication: String,
}

impl Capabilities {
    /// Normative local POSIX namespace.
    pub fn local_posix() -> Capabilities {
        Capabilities {
            atomic_publish: true,
            durable_fsync: true,
            true_range_reads: true,
            max_object_bytes: MAX_CHUNK_SIZE * 64,
            conditional_head: true,
            consistency: "single-host POSIX namespace; read-after-write; rename(2) is atomic; \
                          parent directory fsync orders the new name"
                .to_owned(),
            publication: "tmp file + fsync + rename(2) + directory fsync".to_owned(),
        }
    }

    /// Immutable-object namespace published with `link(2)`.
    pub fn immutable_object() -> Capabilities {
        Capabilities {
            atomic_publish: true,
            durable_fsync: true,
            true_range_reads: true,
            max_object_bytes: MAX_CHUNK_SIZE * 64,
            conditional_head: true,
            consistency: "single-host immutable object namespace; a key appears only after its \
                          bytes are fsynced; read-after-write"
                .to_owned(),
            publication: "tmp file + fsync + link(2) + unlink(tmp) + directory fsync".to_owned(),
        }
    }

    /// Refuse an operation that requires atomic publication.
    pub fn require_atomic_publish(&self, what: &str) -> Result<()> {
        if !self.atomic_publish {
            return Err(Error::unsupported_version(format!(
                "storage backend does not provide atomic_publish; refusing {what}"
            )));
        }
        Ok(())
    }

    /// Refuse an operation that requires durable fsync.
    pub fn require_durable_fsync(&self, what: &str) -> Result<()> {
        if !self.durable_fsync {
            return Err(Error::unsupported_version(format!(
                "storage backend does not provide durable_fsync; refusing {what}"
            )));
        }
        Ok(())
    }

    /// Refuse an operation that requires conditional head publication.
    ///
    /// Only the declared conditional-head capability is checked here; atomic
    /// publication is checked where a publication actually happens, so a
    /// backend that declares weak publication but supports a conditional head
    /// still opens and is refused at the first write (the historical, tested
    /// behaviour).
    pub fn require_conditional_head(&self, what: &str) -> Result<()> {
        if !self.conditional_head {
            return Err(Error::unsupported_version(format!(
                "storage backend does not provide conditional_update for control state; refusing \
                 {what}"
            )));
        }
        Ok(())
    }
}

/// Which physical backend the store uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// Normative local layout (`WEFT-MODEL.md` §2).
    Local,
    /// Immutable-object namespace published with `link(2)`.
    Object,
    /// A real S3-compatible object store (SigV4 over HTTPS/HTTP).
    S3 { config: S3Config },
    /// A real GCS-compatible object store (JSON API with generation
    /// preconditions over HTTPS/HTTP).
    Gcs { config: GcsConfig },
    /// A POSIX-named backend whose guarantees are declared by the caller.
    /// Used to represent storage such as 3FS before its consistency and
    /// durability claims have been measured. The declared capabilities are
    /// enforced exactly.
    Posix { capabilities: Capabilities },
    /// Split planes: immutable blobs on one backend, commit/control state on
    /// another. Each plane is a single (non-split) backend. `blob_root` names a
    /// second local directory when both planes are POSIX-shaped; `None` means
    /// "use the store path", which is what a remote blob plane needs.
    Split {
        blobs: Box<BackendKind>,
        commits: Box<BackendKind>,
        blob_root: Option<PathBuf>,
    },
    /// Physical object packs (subphase R11): small immutable blobs are grouped
    /// into larger pack objects, with membership recorded in a commit-plane
    /// catalog. The inner kind supplies the blob plane, and its commit plane is
    /// also where the catalog lives.
    Packed {
        inner: Box<BackendKind>,
        options: crate::packs::PackOptions,
    },
}

impl BackendKind {
    /// Build a split configuration, rejecting nested splits. The blob plane
    /// uses the store path.
    pub fn split(blobs: BackendKind, commits: BackendKind) -> Result<BackendKind> {
        BackendKind::split_with_blob_root(blobs, commits, None)
    }

    /// Build a split configuration whose blob plane lives at `blob_root`.
    pub fn split_with_blob_root(
        blobs: BackendKind,
        commits: BackendKind,
        blob_root: Option<PathBuf>,
    ) -> Result<BackendKind> {
        if matches!(blobs, BackendKind::Split { .. })
            || matches!(commits, BackendKind::Split { .. })
        {
            return Err(Error::invalid_argument(
                "a split backend takes exactly one blob plane and one commit plane; nesting is not \
                 a supported configuration",
            ));
        }
        Ok(BackendKind::Split {
            blobs: Box::new(blobs),
            commits: Box::new(commits),
            blob_root,
        })
    }

    /// Build a packed configuration. Refuses up front when the inner blob
    /// plane does not provide true range reads: a packed read fetches only the
    /// member's byte range, so a plane that must materialise the whole pack is
    /// never used for packing.
    pub fn packed(inner: BackendKind, options: crate::packs::PackOptions) -> Result<BackendKind> {
        if matches!(inner, BackendKind::Packed { .. }) {
            return Err(Error::invalid_argument(
                "a packed backend takes exactly one blob plane; nesting is not a supported \
                 configuration",
            ));
        }
        options.validate()?;
        let capabilities = inner.capabilities();
        if !capabilities.true_range_reads {
            return Err(Error::unsupported_version(format!(
                "packed blobs need true range reads from the inner blob plane; backend `{}` \
                 does not provide them, so packing is refused rather than approximated by \
                 reading whole packs",
                inner.label()
            )));
        }
        Ok(BackendKind::Packed {
            inner: Box::new(inner),
            options,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            BackendKind::Local => "local",
            BackendKind::Object => "object",
            BackendKind::S3 { .. } => "s3",
            BackendKind::Gcs { .. } => "gcs",
            BackendKind::Posix { .. } => "posix",
            BackendKind::Split { .. } => "split",
            BackendKind::Packed { .. } => "packed",
        }
    }

    /// Stable description used in receipts: `s3-blobs/local-commits` for a
    /// split, the plain label otherwise.
    pub fn describe(&self) -> String {
        match self {
            BackendKind::Split { blobs, commits, .. } => {
                format!("{}-blobs/{}-commits", blobs.label(), commits.label())
            }
            BackendKind::Packed { inner, .. } => format!("packed-{}", inner.describe()),
            other => other.label().to_owned(),
        }
    }

    /// Physical path of the blob plane for this kind.
    fn blob_root(&self, root: &Path) -> PathBuf {
        match self {
            BackendKind::Split {
                blob_root: Some(path),
                ..
            } => path.clone(),
            BackendKind::Split {
                blobs,
                blob_root: None,
                ..
            } => blobs.blob_root(root),
            BackendKind::Packed { inner, .. } => inner.blob_root(root),
            _ => root.to_path_buf(),
        }
    }

    /// Declared capabilities without touching disk. For a split this is the
    /// intersection: a store is only as strong as its weaker plane.
    pub fn capabilities(&self) -> Capabilities {
        match self {
            BackendKind::Local => Capabilities::local_posix(),
            BackendKind::Object => Capabilities::immutable_object(),
            BackendKind::S3 { .. } => S3Backend::capabilities(),
            BackendKind::Gcs { .. } => GcsBackend::capabilities(),
            BackendKind::Posix { capabilities } => capabilities.clone(),
            BackendKind::Split { blobs, commits, .. } => combine_capabilities(
                &blobs.capabilities(),
                &commits.capabilities(),
                self.describe(),
            ),
            BackendKind::Packed { inner, .. } => inner.capabilities(),
        }
    }

    /// The two planes' declared capabilities, when this is a split.
    pub fn split_planes(&self) -> Option<(Capabilities, Capabilities)> {
        match self {
            BackendKind::Split { blobs, commits, .. } => {
                Some((blobs.capabilities(), commits.capabilities()))
            }
            BackendKind::Packed { inner, .. } => inner.split_planes(),
            _ => None,
        }
    }

    /// Refuse a split whose planes cannot honour the store's contract.
    ///
    /// Blobs must be published atomically and durably (a manifest or chunk that
    /// appears half-written would break exactness). Control state must be
    /// published atomically, durably, and conditionally, because the head is
    /// what makes a commit visible.
    pub fn require_plane_contract(&self) -> Result<()> {
        if let BackendKind::Packed { inner, .. } = self {
            inner.require_plane_contract()?;
        }
        let Some((blobs, commits)) = self.split_planes() else {
            return Ok(());
        };
        if !blobs.atomic_publish {
            return Err(Error::unsupported_version(
                "split store: blob plane does not provide atomic_publish; refusing to store \
                 manifest and chunk bytes that could appear half-written",
            ));
        }
        if !blobs.durable_fsync {
            return Err(Error::unsupported_version(
                "split store: blob plane does not provide durable_fsync; refusing to publish \
                 manifest and chunk bytes that may not reach storage",
            ));
        }
        if !commits.atomic_publish {
            return Err(Error::unsupported_version(
                "split store: commit plane does not provide atomic_publish; refusing to publish a \
                 commit record that could be observed torn",
            ));
        }
        if !(commits.conditional_head && commits.durable_fsync) {
            return Err(Error::unsupported_version(
                "split store: commit plane does not provide conditional_update and \
                 durable_fsync; refusing a writable store whose head could be overwritten or \
                 lost",
            ));
        }
        Ok(())
    }

    /// Whether the backend stores commit/control bytes outside the local
    /// filesystem, so the store must not require or create a local directory.
    pub fn is_remote(&self) -> bool {
        match self {
            BackendKind::S3 { .. } => true,
            BackendKind::Gcs { .. } => true,
            BackendKind::Split { commits, .. } => commits.is_remote(),
            BackendKind::Packed { inner, .. } => inner.is_remote(),
            _ => false,
        }
    }

    /// Construct the backend rooted at `root`.
    pub fn open(&self, root: impl AsRef<Path>) -> Result<Box<dyn StorageBackend + Send>> {
        let root = root.as_ref().to_path_buf();
        Ok(match self {
            BackendKind::Local => Box::new(LocalBackend::new(root)),
            BackendKind::Object => Box::new(ObjectBackend::new(root)),
            BackendKind::S3 { config } => Box::new(S3Backend::new(root, config.clone())),
            BackendKind::Gcs { config } => Box::new(GcsBackend::new(root, config.clone())),
            BackendKind::Posix { capabilities } => {
                Box::new(LocalBackend::with_capabilities(root, capabilities.clone()))
            }
            BackendKind::Split {
                blobs,
                commits,
                blob_root,
            } => {
                let blob_path = blob_root.as_deref().unwrap_or(&root);
                Box::new(CompositeBackend::new(
                    blobs.open_blob_plane(blob_path)?,
                    commits.open_commit_plane(&root)?,
                    format!("{}-blobs/{}-commits", blobs.label(), commits.label()),
                ))
            }
            BackendKind::Packed { .. } => Box::new(CompositeBackend::new(
                self.open_blob_plane(&root)?,
                self.open_commit_plane(&root)?,
                self.describe(),
            )),
        })
    }

    /// Construct the blob plane as a [`crate::planes::BlobPlane`].
    pub fn open_blob_plane(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<Box<dyn crate::planes::BlobPlane + Send>> {
        let root = root.as_ref().to_path_buf();
        Ok(match self {
            BackendKind::Local => Box::new(LocalBackend::new(root)),
            BackendKind::Object => Box::new(ObjectBackend::new(root)),
            BackendKind::S3 { config } => Box::new(S3Backend::new(root, config.clone())),
            BackendKind::Gcs { config } => Box::new(GcsBackend::new(root, config.clone())),
            BackendKind::Posix { capabilities } => {
                Box::new(LocalBackend::with_capabilities(root, capabilities.clone()))
            }
            BackendKind::Split { blobs, .. } => blobs.open_blob_plane(&root)?,
            BackendKind::Packed { inner, options } => {
                // The catalog lives on the same commit plane the store uses, so
                // a split store keeps its pack metadata next to its control
                // state rather than on the blob system.
                let blob_root = inner.blob_root(&root);
                let blobs = inner.open_blob_plane(&blob_root)?;
                let catalog = inner.open_commit_plane(&root)?;
                Box::new(crate::packs::PackedBlobPlane::new(
                    blobs, catalog, *options,
                )?)
            }
        })
    }

    /// Construct the commit plane as a [`crate::planes::CommitPlane`].
    pub fn open_commit_plane(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<Box<dyn crate::planes::CommitPlane + Send>> {
        let root = root.as_ref().to_path_buf();
        Ok(match self {
            BackendKind::Local => Box::new(LocalBackend::new(root)),
            BackendKind::Object => Box::new(ObjectBackend::new(root)),
            BackendKind::S3 { config } => Box::new(S3Backend::new(root, config.clone())),
            BackendKind::Gcs { config } => Box::new(GcsBackend::new(root, config.clone())),
            BackendKind::Posix { capabilities } => {
                Box::new(LocalBackend::with_capabilities(root, capabilities.clone()))
            }
            BackendKind::Split { commits, .. } => commits.open_commit_plane(&root)?,
            BackendKind::Packed { inner, .. } => inner.open_commit_plane(&root)?,
        })
    }
}

/// Intersect two declared capability sets into one description of a split.
fn combine_capabilities(
    blobs: &Capabilities,
    commits: &Capabilities,
    description: String,
) -> Capabilities {
    Capabilities {
        atomic_publish: blobs.atomic_publish && commits.atomic_publish,
        durable_fsync: blobs.durable_fsync && commits.durable_fsync,
        true_range_reads: blobs.true_range_reads,
        max_object_bytes: blobs.max_object_bytes.min(commits.max_object_bytes),
        conditional_head: commits.conditional_head,
        consistency: format!(
            "split planes ({description}): blobs [{}]; commit/control [{}]",
            blobs.consistency, commits.consistency
        ),
        publication: format!(
            "blobs: {}; commits: {}",
            blobs.publication, commits.publication
        ),
    }
}

/// Counters for receipts and tests. Backends update these with relaxed atomics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BackendStats {
    pub reads: u64,
    pub range_reads: u64,
    pub bytes_read: u64,
    pub writes: u64,
    pub bytes_written: u64,
    pub link_publications: u64,
    pub rename_publications: u64,
    pub rename_fallbacks: u64,
}

impl BackendStats {
    /// Element-wise sum, used to report two planes as one request stream.
    pub fn merged(&self, other: &BackendStats) -> BackendStats {
        BackendStats {
            reads: self.reads.saturating_add(other.reads),
            range_reads: self.range_reads.saturating_add(other.range_reads),
            bytes_read: self.bytes_read.saturating_add(other.bytes_read),
            writes: self.writes.saturating_add(other.writes),
            bytes_written: self.bytes_written.saturating_add(other.bytes_written),
            link_publications: self
                .link_publications
                .saturating_add(other.link_publications),
            rename_publications: self
                .rename_publications
                .saturating_add(other.rename_publications),
            rename_fallbacks: self.rename_fallbacks.saturating_add(other.rename_fallbacks),
        }
    }
}

#[derive(Debug, Default)]
struct AtomicStats {
    reads: AtomicU64,
    range_reads: AtomicU64,
    bytes_read: AtomicU64,
    writes: AtomicU64,
    bytes_written: AtomicU64,
    link_publications: AtomicU64,
    rename_publications: AtomicU64,
    rename_fallbacks: AtomicU64,
}

impl AtomicStats {
    fn snapshot(&self) -> BackendStats {
        BackendStats {
            reads: self.reads.load(Ordering::Relaxed),
            range_reads: self.range_reads.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            link_publications: self.link_publications.load(Ordering::Relaxed),
            rename_publications: self.rename_publications.load(Ordering::Relaxed),
            rename_fallbacks: self.rename_fallbacks.load(Ordering::Relaxed),
        }
    }
}

/// The head a caller expected to be published when it began a commit.
///
/// `generation == 0` is the sentinel for "no head yet": the superblock is still
/// all zeros. Generation 1 is the genesis commit, so zero is never a real head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadExpectation {
    pub generation: u64,
    pub commit: Hash32,
}

impl HeadExpectation {
    /// The expectation for a store that has no published commit yet.
    pub fn none() -> HeadExpectation {
        HeadExpectation {
            generation: 0,
            commit: Hash32::ZERO,
        }
    }

    pub fn is_none(&self) -> bool {
        self.generation == 0
    }
}

/// Physical I/O abstraction used by the store core.
pub trait StorageBackend: Send {
    /// Backend label used in receipts and diagnostics.
    fn label(&self) -> &'static str;

    /// Declared capabilities (cloned so a backend may revise them after a
    /// publication-primitive probe).
    fn capabilities(&self) -> Capabilities;

    /// Read at most `len` bytes starting at `offset`. Returns fewer bytes at
    /// end of object. `offset` beyond the object is an error.
    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// Read the whole object.
    fn read_all(&self, key: &Key) -> Result<Vec<u8>>;

    /// Write `bytes` at `offset` in an existing object and make them durable.
    /// Used only for the two-slot superblock; content-addressed artifacts are
    /// write-once.
    fn write_at(&mut self, key: &Key, offset: u64, bytes: &[u8]) -> Result<()>;

    /// Publish `bytes` under `key` all-or-nothing. An existing key with
    /// different bytes is an error; identical bytes are a no-op.
    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()>;

    /// Atomically **replace** a mutable store record (only the `quota` record
    /// uses this). A reader observes either the complete previous bytes or the
    /// complete new bytes, never a partial write. The bytes are fully fsynced
    /// before the replacement becomes visible.
    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()>;

    /// Create an object of `len` zero bytes. Fails if the key exists.
    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()>;

    /// Delete one key. Missing keys are `NotFound`.
    fn delete(&mut self, key: &Key) -> Result<()>;

    /// List every key under a [`Key::Prefix`] selector.
    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>>;

    /// Make the object's bytes durable.
    fn sync(&mut self, key: &Key) -> Result<()>;

    /// Make the directory containing `key` durable. `Key::Root` fsyncs the
    /// backend root.
    fn sync_dir(&mut self, key: &Key) -> Result<()>;

    /// Object length in bytes.
    fn len(&self, key: &Key) -> Result<u64>;

    /// Whether the key exists as a regular object.
    fn exists(&self, key: &Key) -> Result<bool>;

    /// Whether the key exists as a regular file. Defaults to [`Self::exists`].
    fn is_file(&self, key: &Key) -> Result<bool> {
        self.exists(key)
    }

    /// Last modification time, when the backend can report one.
    fn modified(&self, key: &Key) -> Result<Option<SystemTime>>;

    /// Create the backend's physical skeleton for a new store.
    fn init_layout(&mut self) -> Result<()>;

    /// Recreate missing skeleton directories for an existing store.
    fn ensure_layout(&mut self) -> Result<()>;

    /// Acquire the exclusive single-writer lock. The handle is retained by the
    /// backend until it is dropped.
    fn acquire_writer_lock(&mut self) -> Result<()>;

    /// Remove stale staging files older than `grace_secs`.
    fn clean_stale_tmp(&mut self, grace_secs: u64) -> Result<()>;

    /// Flush buffered blob publications.
    ///
    /// A plane that groups small blobs into larger physical objects may hold
    /// the most recent publications in memory. The store calls this before it
    /// publishes a commit record, so the protocol keeps its guarantee: every
    /// blob named by a published commit is durable and visible. The default
    /// implementation has nothing buffered.
    fn flush_buffered_blobs(&mut self) -> Result<()> {
        Ok(())
    }

    /// Reclaim physical pack objects whose every member has been deleted.
    ///
    /// Default: the plane has no packs. A packed blob plane returns the exact
    /// number of packs and bytes deleted, so GC can report them.
    fn reclaim_unreferenced_packs(&mut self) -> Result<crate::packs::PackReclaim> {
        Ok(crate::packs::PackReclaim::default())
    }

    /// Report structural problems in the blob plane's own metadata (the pack
    /// catalog, for example). Default: the plane has no metadata of its own.
    fn verify_blob_plane(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Delete several keys. The default deletes one at a time; a packed blob
    /// plane overrides it so one catalog segment is republished per touched
    /// segment instead of once per member.
    fn delete_many(&mut self, keys: &[Key]) -> Result<()> {
        for key in keys {
            self.delete(key)?;
        }
        Ok(())
    }

    /// Backend root directory.
    fn root(&self) -> &Path;

    /// Publication/read counters.
    fn stats(&self) -> BackendStats;

    /// Backend diagnostics (for example a rename fallback).
    fn diagnostics(&self) -> Vec<String>;

    /// Atomically replace `key` with `bytes` iff it still holds the record
    /// whose [`crate::planes::control_token`] is `expected`. `None` means the
    /// record must be absent. A lost race is `Busy`.
    ///
    /// The default implementation reads the record, compares content tokens,
    /// then replaces it. Under the exclusive writer lock the store holds, that
    /// is a genuine compare-and-swap for a single-writer POSIX namespace. A
    /// backend with a native conditional write (S3) overrides it.
    fn compare_and_swap(&mut self, key: &Key, expected: Option<&str>, bytes: &[u8]) -> Result<()> {
        let current = match self.read_all(key) {
            Ok(current) => Some(crate::planes::control_token(&current)),
            Err(err) if err.class() == crate::error::Class::NotFound => None,
            Err(err) => return Err(err),
        };
        if current.as_deref() != expected {
            return Err(Error::busy(format!(
                "control record {} changed since it was read; compare-and-swap refused",
                key.display_path()
            )));
        }
        self.put_replace(key, bytes)
    }

    /// Publish the superblock slot for `generation` iff the published head is
    /// still `expected`.
    ///
    /// A single backend under the writer lock may use the unconditional slot
    /// write (the default). A split store's commit plane uses
    /// [`crate::planes::CommitPlane::compare_exchange_head`] instead, so a
    /// lost head race is refused rather than silently applied.
    fn publish_head(
        &mut self,
        expected: HeadExpectation,
        generation: u64,
        commit: Hash32,
        commit_len: u64,
        timestamp_ns: i64,
        offset: u64,
        slot_bytes: &[u8],
    ) -> Result<()> {
        let _ = (expected, generation, commit, commit_len, timestamp_ns);
        self.write_at(&Key::superblock(), offset, slot_bytes)
    }

    /// Per-plane capabilities when this backend is a split over two planes.
    fn plane_capabilities(&self) -> Option<crate::planes::PlaneCapabilities> {
        None
    }

    /// Per-plane counters when this backend is a split over two planes.
    fn plane_stats(&self) -> Option<crate::planes::PlaneStats> {
        None
    }

    /// Probe the plane capabilities on a writable open.
    ///
    /// Returns `None` for a backend that has no separate planes. A probe that
    /// contradicts a declared capability fails closed.
    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Local backend
// ---------------------------------------------------------------------------

/// The normative `std::fs` backend.
pub struct LocalBackend {
    root: PathBuf,
    capabilities: Capabilities,
    lock: Option<File>,
    stats: AtomicStats,
}

impl LocalBackend {
    pub fn new(root: impl Into<PathBuf>) -> LocalBackend {
        LocalBackend::with_capabilities(root, Capabilities::local_posix())
    }

    pub fn with_capabilities(root: impl Into<PathBuf>, capabilities: Capabilities) -> LocalBackend {
        LocalBackend {
            root: root.into(),
            capabilities,
            lock: None,
            stats: AtomicStats::default(),
        }
    }

    fn path(&self, key: &Key) -> PathBuf {
        match key {
            Key::Root => self.root.clone(),
            Key::Format => self.root.join("FORMAT"),
            Key::Lock => self.root.join("LOCK"),
            Key::Superblock => self.root.join("superblock"),
            Key::Commit { generation } => self
                .root
                .join("commits")
                .join(format!("{generation}.commit")),
            Key::Object { hash } => {
                let hex = hash.to_hex();
                self.root.join("objects").join(&hex[0..2]).join(hex)
            }
            Key::Manifest { hash } => {
                let hex = hash.to_hex();
                self.root.join("manifests").join(&hex[0..2]).join(hex)
            }
            Key::Index { commit } => {
                let hex = commit.to_hex();
                self.root
                    .join("indexes")
                    .join(&hex[0..2])
                    .join(format!("{hex}.idx"))
            }
            Key::SnapshotRef { name } => self.root.join("refs").join("snapshots").join(name),
            Key::Tag { name } => self.root.join("refs").join("tags").join(name),
            Key::Tmp { name } => self.root.join("tmp").join(name),
            Key::Quota => self.root.join("quota"),
            Key::QuotaMigrated => self.root.join("quota.migrated"),
            Key::AccessCounts => self.root.join("access").join("counts.v1"),
            Key::AccessJournal => self.root.join("access").join("journal.v1"),
            Key::Pack { hash } => {
                let hex = hash.to_hex();
                self.root.join("packs").join(&hex[0..2]).join(hex)
            }
            Key::PackCatalog { hash } => {
                let hex = hash.to_hex();
                self.root.join("pack-catalog").join(&hex[0..2]).join(hex)
            }
            Key::PackCatalogRoot => self.root.join("pack-catalog.root"),
            Key::Prefix(_) | Key::Other { .. } => self.root.clone(),
        }
    }

    fn dir_for_kind(&self, kind: KeyKind) -> Option<PathBuf> {
        match kind {
            KeyKind::Root => Some(self.root.clone()),
            KeyKind::Commit => Some(self.root.join("commits")),
            KeyKind::Object => Some(self.root.join("objects")),
            KeyKind::Manifest => Some(self.root.join("manifests")),
            KeyKind::Index => Some(self.root.join("indexes")),
            KeyKind::SnapshotRef => Some(self.root.join("refs").join("snapshots")),
            KeyKind::Tag => Some(self.root.join("refs").join("tags")),
            KeyKind::Tmp => Some(self.root.join("tmp")),
            KeyKind::Quota => Some(self.root.clone()),
            KeyKind::QuotaMigrated => Some(self.root.clone()),
            KeyKind::Access => Some(self.root.join("access")),
            KeyKind::Pack => Some(self.root.join("packs")),
            KeyKind::PackCatalog => Some(self.root.join("pack-catalog")),
            KeyKind::PackCatalogRoot => Some(self.root.clone()),
            _ => None,
        }
    }

    fn ensure_parent(&self, key: &Key) -> Result<()> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            let existed = parent.exists();
            make_dir(parent)?;
            if !existed {
                if let Some(grandparent) = parent.parent() {
                    fsync_dir(grandparent)?;
                }
            }
        }
        Ok(())
    }

    fn publish_file(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        let capabilities = self.capabilities.clone();
        capabilities.require_atomic_publish("atomic publication")?;
        if bytes.len() as u64 > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {}-byte bound",
                bytes.len(),
                capabilities.max_object_bytes
            )));
        }
        let final_path = self.path(key);
        if final_path.exists() {
            let existing = fs::read(&final_path)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", final_path.display())))?;
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::exists(format!(
                    "key {} already exists with different bytes",
                    key.display_path()
                )))
            };
        }
        self.ensure_parent(key)?;
        let parent = final_path
            .parent()
            .ok_or_else(|| Error::io("key path has no parent"))?;
        let tmp = self.tmp_path("put");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        if final_path.exists() {
            let existing = fs::read(&final_path)?;
            fs::remove_file(&tmp).ok();
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::exists(format!(
                    "key {} appeared with different bytes",
                    key.display_path()
                )))
            };
        }
        fs::rename(&tmp, &final_path).map_err(|err| {
            fs::remove_file(&tmp).ok();
            Error::io(format!(
                "cannot rename into {}: {err}",
                final_path.display()
            ))
        })?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.stats
            .rename_publications
            .fetch_add(1, Ordering::Relaxed);
        fsync_dir(parent)?;
        Ok(())
    }

    fn tmp_path(&self, prefix: &str) -> PathBuf {
        self.root.join("tmp").join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl StorageBackend for LocalBackend {
    fn label(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        let path = self.path(key);
        let file = File::open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        let total = file
            .metadata()
            .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?
            .len();
        if offset > total {
            return Err(Error::invalid_argument(format!(
                "read offset {offset} is beyond {} ({total} bytes)",
                key.display_path()
            )));
        }
        let available = total - offset;
        let wanted = available.min(len);
        let mut out = vec![
            0u8;
            usize::try_from(wanted).map_err(|_| {
                Error::limit_exceeded("requested range does not fit the address space")
            })?
        ];
        file.read_exact_at(&mut out, offset)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.range_reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(out.len() as u64, Ordering::Relaxed);
        Ok(out)
    }

    fn read_all(&self, key: &Key) -> Result<Vec<u8>> {
        let path = self.path(key);
        let bytes = fs::read(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                Error::not_found(format!("key {} does not exist: {err}", key.display_path()))
            }
            _ => Error::io(format!("cannot read {}: {err}", path.display())),
        })?;
        if bytes.len() as u64 > self.capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object {} is {} bytes, above the backend's {}-byte bound",
                key.display_path(),
                bytes.len(),
                self.capabilities.max_object_bytes
            )));
        }
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    fn write_at(&mut self, key: &Key, offset: u64, bytes: &[u8]) -> Result<()> {
        let path = self.path(key);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        file.write_all_at(bytes, offset)
            .map_err(|err| Error::io(format!("cannot write {}: {err}", path.display())))?;
        file.sync_all()
            .map_err(|err| Error::io(format!("cannot fsync {}: {err}", path.display())))?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        self.publish_file(key, bytes)
    }

    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        let capabilities = self.capabilities.clone();
        capabilities.require_atomic_publish("mutable record publication")?;
        capabilities.require_durable_fsync("mutable record publication")?;
        if bytes.len() as u64 > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {}-byte bound",
                bytes.len(),
                capabilities.max_object_bytes
            )));
        }
        self.ensure_parent(key)?;
        let final_path = self.path(key);
        let parent = final_path
            .parent()
            .ok_or_else(|| Error::io("key path has no parent"))?
            .to_path_buf();
        let tmp = self.tmp_path("replace");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        // rename(2) atomically replaces the previous record on POSIX.
        fs::rename(&tmp, &final_path).map_err(|err| {
            fs::remove_file(&tmp).ok();
            Error::io(format!("cannot replace {}: {err}", final_path.display()))
        })?;
        fsync_dir(&parent)?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.stats
            .rename_publications
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()> {
        let bytes = vec![
            0u8;
            usize::try_from(len).map_err(|_| Error::limit_exceeded(
                "zeroed object does not fit the address space"
            ))?
        ];
        self.publish_file(key, &bytes)
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        fs::remove_file(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                Error::not_found(format!("key {} does not exist", key.display_path()))
            }
            _ => Error::io(format!("cannot delete {}: {err}", path.display())),
        })
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list_prefix requires a Key::Prefix selector",
            ));
        };
        let Some(dir) = self.dir_for_kind(*kind) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let sharded = matches!(
            *kind,
            KeyKind::Object
                | KeyKind::Manifest
                | KeyKind::Index
                | KeyKind::Pack
                | KeyKind::PackCatalog
        );
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(Error::io(format!("cannot read {}: {err}", dir.display()))),
        };
        for entry in entries {
            let entry = entry.map_err(|err| Error::io(format!("cannot read dir entry: {err}")))?;
            let file_type = entry
                .file_type()
                .map_err(|err| Error::io(format!("cannot stat dir entry: {err}")))?;
            if sharded {
                if !file_type.is_dir() {
                    continue;
                }
                let shard = entry.file_name();
                let shard = match shard.to_str() {
                    Some(shard) => shard.to_owned(),
                    None => continue,
                };
                for child in fs::read_dir(entry.path())
                    .map_err(|err| Error::io(format!("cannot read shard: {err}")))?
                {
                    let child = child
                        .map_err(|err| Error::io(format!("cannot read shard entry: {err}")))?;
                    if !child
                        .file_type()
                        .map_err(|err| Error::io(format!("cannot stat shard entry: {err}")))?
                        .is_file()
                    {
                        continue;
                    }
                    let name = child.file_name();
                    let name = match name.to_str() {
                        Some(name) => name.to_owned(),
                        None => continue,
                    };
                    match classify_local(*kind, &shard, &name) {
                        Some(key) => out.push(key),
                        None => out.push(Key::Other {
                            kind: *kind,
                            name: format!("{shard}/{name}"),
                        }),
                    }
                }
            } else if file_type.is_file() {
                let name = entry.file_name();
                let name = match name.to_str() {
                    Some(name) => name.to_owned(),
                    None => continue,
                };
                match classify_local(*kind, "", &name) {
                    Some(key) => out.push(key),
                    None => out.push(Key::Other { kind: *kind, name }),
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn sync(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        let file = File::open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        file.sync_all()
            .map_err(|err| Error::io(format!("cannot fsync {}: {err}", path.display())))
    }

    fn sync_dir(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        let dir = if key.kind() == KeyKind::Root || path.is_dir() {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| Error::io("key path has no parent"))?
        };
        fsync_dir(&dir)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        let path = self.path(key);
        fs::metadata(&path)
            .map(|metadata| metadata.len())
            .map_err(|err| match err.kind() {
                ErrorKind::NotFound => {
                    Error::not_found(format!("key {} does not exist", key.display_path()))
                }
                _ => Error::io(format!("cannot stat {}: {err}", path.display())),
            })
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        Ok(self.path(key).is_file())
    }

    fn modified(&self, key: &Key) -> Result<Option<SystemTime>> {
        match fs::metadata(self.path(key)) {
            Ok(metadata) => Ok(metadata.modified().ok()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::io(format!("cannot stat key: {err}"))),
        }
    }

    fn init_layout(&mut self) -> Result<()> {
        make_dir(&self.root)?;
        for relative in [
            "commits",
            "objects",
            "manifests",
            "indexes",
            "refs",
            "refs/snapshots",
            "refs/tags",
            "tmp",
        ] {
            make_dir(&self.root.join(relative))?;
        }
        create_lock_file(&self.root.join("LOCK"))?;
        Ok(())
    }

    fn ensure_layout(&mut self) -> Result<()> {
        self.init_layout()
    }

    fn acquire_writer_lock(&mut self) -> Result<()> {
        let path = self.root.join("LOCK");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(&path)
            .map_err(|err| Error::io(format!("cannot open LOCK: {err}")))?;
        Fs2FileExt::try_lock_exclusive(&file)
            .map_err(|err| Error::busy(format!("store is locked by another writer: {err}")))?;
        self.lock = Some(file);
        Ok(())
    }

    fn clean_stale_tmp(&mut self, grace_secs: u64) -> Result<()> {
        clean_stale_tmp_dir(&self.root.join("tmp"), grace_secs)
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn stats(&self) -> BackendStats {
        self.stats.snapshot()
    }

    fn diagnostics(&self) -> Vec<String> {
        Vec::new()
    }

    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        let blobs = crate::planes::probe_blob_plane(self)?;
        let commits = crate::planes::probe_commit_plane(self)?;
        Ok(Some(crate::planes::PlaneProbes { blobs, commits }))
    }
}

fn classify_local(kind: KeyKind, shard: &str, name: &str) -> Option<Key> {
    match kind {
        KeyKind::Object | KeyKind::Manifest => {
            let hash = Hash32::from_lower_hex(name).ok()?;
            if &hash.to_hex()[0..2] != shard {
                return None;
            }
            Some(if kind == KeyKind::Object {
                Key::object(hash)
            } else {
                Key::manifest(hash)
            })
        }
        KeyKind::Index => {
            let stem = name.strip_suffix(".idx")?;
            let commit = Hash32::from_lower_hex(stem).ok()?;
            if &commit.to_hex()[0..2] != shard {
                return None;
            }
            Some(Key::index(commit))
        }
        KeyKind::Commit => {
            let stem = name.strip_suffix(".commit")?;
            if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(Key::commit(stem.parse::<u64>().ok()?))
        }
        KeyKind::SnapshotRef => Some(Key::snapshot_ref(name)),
        KeyKind::Tag => Some(Key::tag(name)),
        KeyKind::Tmp => Some(Key::tmp(name)),
        KeyKind::Quota => {
            if name == "quota" {
                Some(Key::quota())
            } else {
                None
            }
        }
        KeyKind::QuotaMigrated => {
            if name == "quota.migrated" {
                Some(Key::quota_migrated())
            } else {
                None
            }
        }
        KeyKind::Access => match name {
            "counts.v1" => Some(Key::access_counts()),
            "journal.v1" => Some(Key::access_journal()),
            _ => None,
        },
        KeyKind::Pack | KeyKind::PackCatalog => {
            let hash = Hash32::from_lower_hex(name).ok()?;
            if &hash.to_hex()[0..2] != shard {
                return None;
            }
            Some(if kind == KeyKind::Pack {
                Key::pack(hash)
            } else {
                Key::pack_catalog(hash)
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Object backend
// ---------------------------------------------------------------------------

/// Immutable-object backend published with `link(2)`.
///
/// Every key maps to `<root>/objects/<2-hex>/<name>`:
///
/// | key | name |
/// |---|---|
/// | object `<hash>` | `<hash>` |
/// | manifest `<hash>` | `m-<hash>` |
/// | index `<hash>` | `i-<hash>.idx` |
/// | commit `<gen>` | `c-<gen>.commit` |
/// | snapshot `<name>` | `s-<name>` |
/// | tag `<name>` | `t-<name>` |
/// | FORMAT / LOCK / superblock | `FORMAT` / `LOCK` / `superblock` |
///
/// Content identities (hashes) are exactly those of the local backend; the
/// kind tag keeps the flat namespace unambiguous. The superblock is the only
/// mutable object and is updated in place through `write_at`, preserving the
/// two-slot protocol.
pub struct ObjectBackend {
    root: PathBuf,
    capabilities: std::sync::Mutex<Capabilities>,
    lock: Option<File>,
    stats: AtomicStats,
    link_supported: std::sync::Mutex<Option<bool>>,
    diagnostics: std::sync::Mutex<Vec<String>>,
}

impl ObjectBackend {
    pub fn new(root: impl Into<PathBuf>) -> ObjectBackend {
        ObjectBackend {
            root: root.into(),
            capabilities: std::sync::Mutex::new(Capabilities::immutable_object()),
            lock: None,
            stats: AtomicStats::default(),
            link_supported: std::sync::Mutex::new(None),
            diagnostics: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn physical_name(&self, key: &Key) -> String {
        match key {
            Key::Format => "FORMAT".to_owned(),
            Key::Lock => "LOCK".to_owned(),
            Key::Superblock => "superblock".to_owned(),
            Key::Commit { generation } => format!("c-{generation}.commit"),
            Key::Object { hash } => hash.to_hex(),
            Key::Manifest { hash } => format!("m-{}", hash.to_hex()),
            Key::Index { commit } => format!("i-{}.idx", commit.to_hex()),
            Key::SnapshotRef { name } => format!("s-{name}"),
            Key::Tag { name } => format!("t-{name}"),
            Key::Quota => "q-quota".to_owned(),
            Key::QuotaMigrated => "q-quota.migrated".to_owned(),
            Key::AccessCounts => "a-counts.v1".to_owned(),
            Key::AccessJournal => "a-journal.v1".to_owned(),
            Key::Tmp { name } => TMP_PREFIX.to_owned() + name,
            Key::Pack { hash } => format!("p-{}", hash.to_hex()),
            Key::PackCatalog { hash } => format!("pc-{}", hash.to_hex()),
            Key::PackCatalogRoot | Key::Root | Key::Prefix(_) | Key::Other { .. } => String::new(),
        }
    }

    fn shard_for(&self, key: &Key) -> String {
        key.shard().unwrap_or_else(|| "00".to_owned())
    }

    fn path(&self, key: &Key) -> PathBuf {
        match key {
            Key::Root => self.root.join("objects"),
            Key::Tmp { .. } => self.root.join("tmp").join(self.physical_name(key)),
            _ => self
                .root
                .join("objects")
                .join(self.shard_for(key))
                .join(self.physical_name(key)),
        }
    }

    fn tmp_path(&self, prefix: &str) -> PathBuf {
        self.root.join("tmp").join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn set_capability_publication(&self, text: &str) {
        let mut capabilities = self
            .capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        capabilities.publication = text.to_owned();
    }

    fn note(&self, text: String) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if diagnostics.len() < 16 {
            diagnostics.push(text);
        }
    }

    /// Probe `link(2)` once. Returns whether hard links are usable.
    fn link_supported(&self) -> bool {
        {
            let state = self
                .link_supported
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(value) = *state {
                return value;
            }
        }
        let dir = self.root.join("tmp");
        let _ = fs::create_dir_all(&dir);
        let a = dir.join(format!("link-probe-a-{}", std::process::id()));
        let b = dir.join(format!("link-probe-b-{}", std::process::id()));
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
        let supported = fs::write(&a, b"probe")
            .and_then(|()| fs::hard_link(&a, &b))
            .is_ok();
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
        let mut state = self
            .link_supported
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = Some(supported);
        if !supported {
            self.set_capability_publication(
                "tmp file + fsync + rename(2) fallback (link(2) unavailable) + directory fsync",
            );
            self.note(
                "link(2) is unavailable in this environment; ObjectBackend publishes with \
                 rename(2) and does not claim a link-published run"
                    .to_owned(),
            );
        }
        supported
    }

    fn ensure_parent(&self, key: &Key) -> Result<()> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            let existed = parent.exists();
            make_dir(parent)?;
            if !existed {
                if let Some(grandparent) = parent.parent() {
                    fsync_dir(grandparent)?;
                }
            }
        }
        Ok(())
    }

    fn publish(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        let capabilities = self.capabilities();
        capabilities.require_atomic_publish("atomic publication")?;
        if bytes.len() as u64 > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {}-byte bound",
                bytes.len(),
                capabilities.max_object_bytes
            )));
        }
        let final_path = self.path(key);
        if final_path.exists() {
            let existing = fs::read(&final_path)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", final_path.display())))?;
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::exists(format!(
                    "key {} already exists with different bytes (write-once)",
                    key.display_path()
                )))
            };
        }
        self.ensure_parent(key)?;
        let parent = final_path
            .parent()
            .ok_or_else(|| Error::io("key path has no parent"))?;
        let tmp = self.tmp_path("put");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;

        if final_path.exists() {
            let existing = fs::read(&final_path)?;
            fs::remove_file(&tmp).ok();
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::exists(format!(
                    "key {} appeared with different bytes (write-once)",
                    key.display_path()
                )))
            };
        }

        let used_link = self.link_supported();
        let result = if used_link {
            match fs::hard_link(&tmp, &final_path) {
                Ok(()) => {
                    fs::remove_file(&tmp).ok();
                    self.stats.link_publications.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    let existing = fs::read(&final_path)?;
                    fs::remove_file(&tmp).ok();
                    if existing == bytes {
                        Ok(())
                    } else {
                        Err(Error::exists(format!(
                            "key {} appeared with different bytes (write-once)",
                            key.display_path()
                        )))
                    }
                }
                Err(err) if link_unavailable(&err) => {
                    // The probe succeeded but this call did not; fall back and
                    // record it rather than claiming a link publication.
                    self.stats.rename_fallbacks.fetch_add(1, Ordering::Relaxed);
                    self.set_capability_publication(
                        "tmp file + fsync + rename(2) fallback (link(2) call failed) + directory fsync",
                    );
                    self.note(format!(
                        "hard_link failed with {err}; falling back to rename(2) for {}",
                        key.display_path()
                    ));
                    fs::rename(&tmp, &final_path).map_err(|err| {
                        fs::remove_file(&tmp).ok();
                        Error::io(format!("cannot publish {}: {err}", final_path.display()))
                    })?;
                    self.stats
                        .rename_publications
                        .fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
                Err(err) => {
                    fs::remove_file(&tmp).ok();
                    Err(Error::io(format!(
                        "cannot link {} into place: {err}",
                        final_path.display()
                    )))
                }
            }
        } else {
            fs::rename(&tmp, &final_path).map_err(|err| {
                fs::remove_file(&tmp).ok();
                Error::io(format!("cannot publish {}: {err}", final_path.display()))
            })?;
            self.stats
                .rename_publications
                .fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        result?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        fsync_dir(parent)?;
        Ok(())
    }
}

const TMP_PREFIX: &str = "tmp-";

fn link_unavailable(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::Unsupported | ErrorKind::PermissionDenied
    ) || err.raw_os_error() == Some(95) // EOPNOTSUPP
}

impl StorageBackend for ObjectBackend {
    fn label(&self) -> &'static str {
        "object"
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        let path = self.path(key);
        let file = File::open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        let total = file
            .metadata()
            .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?
            .len();
        if offset > total {
            return Err(Error::invalid_argument(format!(
                "read offset {offset} is beyond {} ({total} bytes)",
                key.display_path()
            )));
        }
        let wanted = (total - offset).min(len);
        let mut out = vec![
            0u8;
            usize::try_from(wanted).map_err(|_| {
                Error::limit_exceeded("requested range does not fit the address space")
            })?
        ];
        file.read_exact_at(&mut out, offset)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.range_reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(out.len() as u64, Ordering::Relaxed);
        Ok(out)
    }

    fn read_all(&self, key: &Key) -> Result<Vec<u8>> {
        let path = self.path(key);
        let bytes = fs::read(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                Error::not_found(format!("key {} does not exist: {err}", key.display_path()))
            }
            _ => Error::io(format!("cannot read {}: {err}", path.display())),
        })?;
        if bytes.len() as u64 > self.capabilities().max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object {} exceeds the backend's object bound",
                key.display_path()
            )));
        }
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    fn write_at(&mut self, key: &Key, offset: u64, bytes: &[u8]) -> Result<()> {
        let path = self.path(key);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        file.write_all_at(bytes, offset)
            .map_err(|err| Error::io(format!("cannot write {}: {err}", path.display())))?;
        file.sync_all()
            .map_err(|err| Error::io(format!("cannot fsync {}: {err}", path.display())))?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        self.publish(key, bytes)
    }

    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        // Only the mutable `quota` record takes this path. It still uses a fully
        // written and fsynced temporary name, then `rename(2)` so a reader sees
        // the old bytes or the new bytes, never a torn record.
        let capabilities = self.capabilities();
        capabilities.require_atomic_publish("mutable record publication")?;
        capabilities.require_durable_fsync("mutable record publication")?;
        if bytes.len() as u64 > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {}-byte bound",
                bytes.len(),
                capabilities.max_object_bytes
            )));
        }
        self.ensure_parent(key)?;
        let final_path = self.path(key);
        let parent = final_path
            .parent()
            .ok_or_else(|| Error::io("key path has no parent"))?
            .to_path_buf();
        let tmp = self.tmp_path("replace");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, &final_path).map_err(|err| {
            fs::remove_file(&tmp).ok();
            Error::io(format!("cannot replace {}: {err}", final_path.display()))
        })?;
        fsync_dir(&parent)?;
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()> {
        let bytes = vec![
            0u8;
            usize::try_from(len).map_err(|_| {
                Error::limit_exceeded("zeroed object does not fit the address space")
            })?
        ];
        self.publish(key, &bytes)
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        fs::remove_file(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                Error::not_found(format!("key {} does not exist", key.display_path()))
            }
            _ => Error::io(format!("cannot delete {}: {err}", path.display())),
        })
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list_prefix requires a Key::Prefix selector",
            ));
        };
        if *kind == KeyKind::Tmp {
            return list_flat(&self.root.join("tmp"), KeyKind::Tmp);
        }
        if *kind == KeyKind::Root {
            return Ok(Vec::new());
        }
        let base = self.root.join("objects");
        let mut out = Vec::new();
        let shards = match fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(Error::io(format!("cannot read {}: {err}", base.display()))),
        };
        for shard in shards {
            let shard = shard.map_err(|err| Error::io(format!("cannot read shard: {err}")))?;
            if !shard
                .file_type()
                .map_err(|err| Error::io(format!("cannot stat shard: {err}")))?
                .is_dir()
            {
                continue;
            }
            let shard_name = shard.file_name();
            let shard_name = match shard_name.to_str() {
                Some(name) => name.to_owned(),
                None => continue,
            };
            for entry in fs::read_dir(shard.path())
                .map_err(|err| Error::io(format!("cannot read shard: {err}")))?
            {
                let entry =
                    entry.map_err(|err| Error::io(format!("cannot read shard entry: {err}")))?;
                if !entry
                    .file_type()
                    .map_err(|err| Error::io(format!("cannot stat shard entry: {err}")))?
                    .is_file()
                {
                    continue;
                }
                let name = entry.file_name();
                let name = match name.to_str() {
                    Some(name) => name.to_owned(),
                    None => continue,
                };
                if let Some(key) = classify_object(*kind, &shard_name, &name) {
                    out.push(key);
                } else if *kind == KeyKind::Object && !known_object_name(&name) {
                    // Report unknown names once, from the object sweep.
                    out.push(Key::Other {
                        kind: *kind,
                        name: format!("{shard_name}/{name}"),
                    });
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn sync(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        let file = File::open(&path)
            .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
        file.sync_all()
            .map_err(|err| Error::io(format!("cannot fsync {}: {err}", path.display())))
    }

    fn sync_dir(&mut self, key: &Key) -> Result<()> {
        let path = self.path(key);
        let dir = if key.kind() == KeyKind::Root || path.is_dir() {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| Error::io("key path has no parent"))?
        };
        fsync_dir(&dir)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        let path = self.path(key);
        fs::metadata(&path)
            .map(|metadata| metadata.len())
            .map_err(|err| match err.kind() {
                ErrorKind::NotFound => {
                    Error::not_found(format!("key {} does not exist", key.display_path()))
                }
                _ => Error::io(format!("cannot stat {}: {err}", path.display())),
            })
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        Ok(self.path(key).is_file())
    }

    fn modified(&self, key: &Key) -> Result<Option<SystemTime>> {
        match fs::metadata(self.path(key)) {
            Ok(metadata) => Ok(metadata.modified().ok()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::io(format!("cannot stat key: {err}"))),
        }
    }

    fn init_layout(&mut self) -> Result<()> {
        make_dir(&self.root)?;
        make_dir(&self.root.join("objects"))?;
        make_dir(&self.root.join("objects").join("00"))?;
        make_dir(&self.root.join("tmp"))?;
        create_lock_file(&self.root.join("objects").join("00").join("LOCK"))?;
        // Probe before the first publication so `capabilities()` reports the
        // primitive that will actually be used.
        let _ = self.link_supported();
        Ok(())
    }

    fn ensure_layout(&mut self) -> Result<()> {
        make_dir(&self.root)?;
        make_dir(&self.root.join("objects"))?;
        make_dir(&self.root.join("objects").join("00"))?;
        make_dir(&self.root.join("tmp"))?;
        Ok(())
    }

    fn acquire_writer_lock(&mut self) -> Result<()> {
        let path = self.root.join("objects").join("00").join("LOCK");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(&path)
            .map_err(|err| Error::io(format!("cannot open LOCK: {err}")))?;
        Fs2FileExt::try_lock_exclusive(&file)
            .map_err(|err| Error::busy(format!("store is locked by another writer: {err}")))?;
        self.lock = Some(file);
        Ok(())
    }

    fn clean_stale_tmp(&mut self, grace_secs: u64) -> Result<()> {
        clean_stale_tmp_dir(&self.root.join("tmp"), grace_secs)
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn stats(&self) -> BackendStats {
        self.stats.snapshot()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        let blobs = crate::planes::probe_blob_plane(self)?;
        let commits = crate::planes::probe_commit_plane(self)?;
        Ok(Some(crate::planes::PlaneProbes { blobs, commits }))
    }
}

fn classify_object(kind: KeyKind, shard: &str, name: &str) -> Option<Key> {
    let hex = match kind {
        KeyKind::Object => name,
        KeyKind::Manifest => name.strip_prefix("m-")?,
        KeyKind::Pack => name.strip_prefix("p-")?,
        KeyKind::PackCatalog => name.strip_prefix("pc-")?,
        KeyKind::Index => {
            let stem = name.strip_prefix("i-")?.strip_suffix(".idx")?;
            let commit = Hash32::from_lower_hex(stem).ok()?;
            if &commit.to_hex()[0..2] != shard {
                return None;
            }
            return Some(Key::index(commit));
        }
        KeyKind::Commit => {
            let stem = name.strip_prefix("c-")?.strip_suffix(".commit")?;
            if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            return Some(Key::commit(stem.parse::<u64>().ok()?));
        }
        KeyKind::SnapshotRef => {
            let name = name.strip_prefix("s-")?;
            return Some(Key::snapshot_ref(name));
        }
        KeyKind::Tag => {
            let name = name.strip_prefix("t-")?;
            return Some(Key::tag(name));
        }
        KeyKind::Quota => {
            if name == "q-quota" {
                return Some(Key::quota());
            }
            return None;
        }
        KeyKind::QuotaMigrated => {
            if name == "q-quota.migrated" {
                return Some(Key::quota_migrated());
            }
            return None;
        }
        KeyKind::Access => {
            return match name {
                "a-counts.v1" => Some(Key::access_counts()),
                "a-journal.v1" => Some(Key::access_journal()),
                _ => None,
            };
        }
        _ => return None,
    };
    let hash = Hash32::from_lower_hex(hex).ok()?;
    if &hash.to_hex()[0..2] != shard {
        return None;
    }
    Some(match kind {
        KeyKind::Object => Key::object(hash),
        KeyKind::Manifest => Key::manifest(hash),
        KeyKind::Pack => Key::pack(hash),
        KeyKind::PackCatalog => Key::pack_catalog(hash),
        _ => return None,
    })
}

fn known_object_name(name: &str) -> bool {
    if name == "q-quota" || name == "q-quota.migrated" {
        return true;
    }
    if Hash32::from_lower_hex(name).is_ok() {
        return true;
    }
    if let Some(hex) = name.strip_prefix("m-") {
        if Hash32::from_lower_hex(hex).is_ok() {
            return true;
        }
    }
    if let Some(stem) = name
        .strip_prefix("i-")
        .and_then(|name| name.strip_suffix(".idx"))
    {
        if Hash32::from_lower_hex(stem).is_ok() {
            return true;
        }
    }
    if let Some(hex) = name.strip_prefix("pc-") {
        if Hash32::from_lower_hex(hex).is_ok() {
            return true;
        }
    }
    if let Some(hex) = name.strip_prefix("p-") {
        if Hash32::from_lower_hex(hex).is_ok() {
            return true;
        }
    }
    if let Some(stem) = name
        .strip_prefix("c-")
        .and_then(|name| name.strip_suffix(".commit"))
    {
        if !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit()) {
            return true;
        }
    }
    if name.starts_with("s-") || name.starts_with("t-") {
        return true;
    }
    matches!(name, "FORMAT" | "LOCK" | "superblock")
}

fn list_flat(dir: &Path, kind: KeyKind) -> Result<Vec<Key>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(Error::io(format!("cannot read {}: {err}", dir.display()))),
    };
    for entry in entries {
        let entry = entry.map_err(|err| Error::io(format!("cannot read entry: {err}")))?;
        if !entry
            .file_type()
            .map_err(|err| Error::io(format!("cannot stat entry: {err}")))?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        out.push(match kind {
            KeyKind::Tmp => {
                let name = name.strip_prefix(TMP_PREFIX).unwrap_or(&name);
                Key::tmp(name)
            }
            _ => Key::Other { kind, name },
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn create_lock_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)
        .map_err(|err| Error::io(format!("cannot create LOCK {}: {err}", path.display())))?;
    file.sync_all()
        .map_err(|err| Error::io(format!("cannot fsync LOCK {}: {err}", path.display())))?;
    Ok(())
}

pub(crate) fn make_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|err| Error::io(format!("cannot chmod {}: {err}", path.display())))?;
    Ok(())
}

pub(crate) fn fsync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path)
        .map_err(|err| Error::io(format!("cannot open directory {}: {err}", path.display())))?;
    dir.sync_all()
        .map_err(|err| Error::io(format!("cannot fsync directory {}: {err}", path.display())))
}

pub(crate) fn clean_stale_tmp_dir(dir: &Path, grace_secs: u64) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(Error::io(format!("cannot read {}: {err}", dir.display()))),
    };
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(name) => name,
            None => continue,
        };
        if !(name.starts_with("blob-")
            || name.starts_with("snap-")
            || name.starts_with("index-")
            || name.starts_with("put-")
            || name.starts_with(TMP_PREFIX))
        {
            continue;
        }
        if grace_secs > 0 {
            let age = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| now.duration_since(modified).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            if age < grace_secs {
                continue;
            }
        }
        let _ = fs::remove_file(entry.path());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Composite backend: blobs on one plane, control state on another
// ---------------------------------------------------------------------------

/// A [`StorageBackend`] that routes every [`Key`] to one of two planes.
///
/// Blob keys (`objects/`, `manifests/`, `indexes/`) go to the blob plane; every
/// other key (`FORMAT`, `LOCK`, `superblock`, `commits/`, `refs/`, `quota`,
/// `access/`, `tmp/`) goes to the commit plane. The on-disk bytes, logical key
/// names, and commit protocol are identical to a single backend; only the
/// physical system differs.
///
/// The advisory `access/` counters live on the commit plane because they are
/// small mutable records; they are never consulted by commits, recovery,
/// verification, or GC.
pub struct CompositeBackend {
    blobs: Box<dyn crate::planes::BlobPlane + Send>,
    commits: Box<dyn crate::planes::CommitPlane + Send>,
    description: String,
}

impl CompositeBackend {
    /// Assemble from two planes. `description` is `s3-blobs/local-commits` and
    /// similar; it is recorded in receipts and in error messages.
    pub fn new(
        blobs: Box<dyn crate::planes::BlobPlane + Send>,
        commits: Box<dyn crate::planes::CommitPlane + Send>,
        description: String,
    ) -> CompositeBackend {
        CompositeBackend {
            blobs,
            commits,
            description,
        }
    }

    /// The routing rule, stated once.
    fn key_is_blob(key: &Key) -> bool {
        matches!(
            key.kind(),
            KeyKind::Object
                | KeyKind::Manifest
                | KeyKind::Index
                | KeyKind::Pack
                | KeyKind::PackCatalog
        )
    }

    /// Plane description used in receipts.
    pub fn description(&self) -> &str {
        &self.description
    }
}

impl StorageBackend for CompositeBackend {
    fn label(&self) -> &'static str {
        "split"
    }

    fn capabilities(&self) -> Capabilities {
        let blobs = self.blobs.blob_capabilities();
        let commits = self.commits.commit_capabilities();
        Capabilities {
            atomic_publish: blobs.atomic_publish && commits.atomic_publish,
            durable_fsync: blobs.durable_fsync && commits.durable_fsync,
            true_range_reads: blobs.true_range_reads,
            max_object_bytes: blobs.max_object_bytes.min(commits.max_control_bytes),
            conditional_head: commits.conditional_update,
            consistency: format!(
                "split planes ({}) — blobs [{}]; commit/control [{}]",
                self.description, blobs.consistency, commits.consistency
            ),
            publication: format!(
                "blobs: {}; commits: {}",
                blobs.publication, commits.publication
            ),
        }
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        if Self::key_is_blob(key) {
            return self.blobs.get_range(key, offset, len);
        }
        let record = self.commits.read_control(key)?.ok_or_else(|| {
            Error::not_found(format!("key {} does not exist", key.display_path()))
        })?;
        slice_control(&record.bytes, key, offset, len)
    }

    fn read_all(&self, key: &Key) -> Result<Vec<u8>> {
        if Self::key_is_blob(key) {
            return self.blobs.get(key);
        }
        self.commits
            .read_control(key)?
            .map(|record| record.bytes)
            .ok_or_else(|| Error::not_found(format!("key {} does not exist", key.display_path())))
    }

    fn write_at(&mut self, key: &Key, offset: u64, bytes: &[u8]) -> Result<()> {
        if Self::key_is_blob(key) {
            return Err(Error::unsupported_representation(format!(
                "blob plane objects are immutable; refusing an in-place write to {}",
                key.display_path()
            )));
        }
        // Read-modify-write under a compare-and-swap. Only the two-slot
        // superblock takes this path, and the CAS makes a lost race loud.
        let record = self.commits.read_control(key)?.ok_or_else(|| {
            Error::not_found(format!("key {} does not exist", key.display_path()))
        })?;
        let start = usize::try_from(offset)
            .map_err(|_| Error::limit_exceeded("write offset does not fit memory"))?;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(|| Error::limit_exceeded("write end overflows memory"))?;
        if end > record.bytes.len() {
            return Err(Error::invalid_argument(format!(
                "write range {offset}..{end} exceeds {} bytes",
                record.bytes.len()
            )));
        }
        let mut image = record.bytes;
        image[start..end].copy_from_slice(bytes);
        self.commits
            .compare_exchange(key, record.token.as_deref(), &image)?;
        Ok(())
    }

    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        if Self::key_is_blob(key) {
            self.blobs.put_if_absent(key, bytes)
        } else {
            self.commits.put_control_if_absent(key, bytes)
        }
    }

    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        if Self::key_is_blob(key) {
            return Err(Error::unsupported_representation(format!(
                "blob plane objects are immutable; refusing to replace {}",
                key.display_path()
            )));
        }
        self.commits.write_control(key, bytes)
    }

    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()> {
        if Self::key_is_blob(key) {
            return Err(Error::unsupported_representation(format!(
                "blob plane has no zero-filled creation path; refusing {}",
                key.display_path()
            )));
        }
        self.commits.create_control(key, len)
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        if Self::key_is_blob(key) {
            self.blobs.delete(key)
        } else {
            self.commits.delete_control(key)
        }
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list_prefix requires a Key::Prefix selector",
            ));
        };
        if matches!(
            kind,
            KeyKind::Object
                | KeyKind::Manifest
                | KeyKind::Index
                | KeyKind::Pack
                | KeyKind::PackCatalog
        ) {
            self.blobs.list(prefix)
        } else {
            self.commits.list_control(prefix)
        }
    }

    fn sync(&mut self, key: &Key) -> Result<()> {
        if Self::key_is_blob(key) {
            return Ok(());
        }
        self.commits.sync_dir(key)
    }

    fn sync_dir(&mut self, key: &Key) -> Result<()> {
        if Self::key_is_blob(key) {
            // Blob publication is durable per the plane contract; there is no
            // directory to fsync in a content-addressed namespace.
            return Ok(());
        }
        self.commits.sync_dir(key)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        if Self::key_is_blob(key) {
            self.blobs.len(key)
        } else {
            self.commits.control_len(key)
        }
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        if Self::key_is_blob(key) {
            self.blobs.exists(key)
        } else {
            self.commits.control_exists(key)
        }
    }

    fn is_file(&self, key: &Key) -> Result<bool> {
        self.exists(key)
    }

    fn modified(&self, key: &Key) -> Result<Option<SystemTime>> {
        if Self::key_is_blob(key) {
            return Ok(None);
        }
        self.commits.modified(key)
    }

    fn init_layout(&mut self) -> Result<()> {
        self.blobs.init_layout()?;
        self.commits.init_layout()?;
        Ok(())
    }

    fn ensure_layout(&mut self) -> Result<()> {
        self.blobs.ensure_layout()?;
        self.commits.ensure_layout()?;
        Ok(())
    }

    fn acquire_writer_lock(&mut self) -> Result<()> {
        self.commits.acquire_lock()
    }

    fn clean_stale_tmp(&mut self, grace_secs: u64) -> Result<()> {
        self.blobs.clean_stale(grace_secs)?;
        self.commits.clean_stale(grace_secs)
    }

    fn flush_buffered_blobs(&mut self) -> Result<()> {
        self.blobs.flush()
    }

    fn reclaim_unreferenced_packs(&mut self) -> Result<crate::packs::PackReclaim> {
        self.blobs.reclaim_unreferenced_packs()
    }

    fn verify_blob_plane(&self) -> Result<Vec<String>> {
        self.blobs.verify_packs()
    }

    fn delete_many(&mut self, keys: &[Key]) -> Result<()> {
        let mut blobs: Vec<Key> = Vec::new();
        let mut commits: Vec<&Key> = Vec::new();
        for key in keys {
            if Self::key_is_blob(key) {
                blobs.push(key.clone());
            } else {
                commits.push(key);
            }
        }
        self.blobs.delete_many(&blobs)?;
        for key in commits {
            self.commits.delete_control(key)?;
        }
        Ok(())
    }

    fn root(&self) -> &Path {
        self.commits.root()
    }

    fn stats(&self) -> BackendStats {
        self.blobs.stats().merged(&self.commits.stats())
    }

    fn diagnostics(&self) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.blobs.diagnostics() {
            out.push(format!("blob plane: {line}"));
        }
        for line in self.commits.diagnostics() {
            out.push(format!("commit plane: {line}"));
        }
        out
    }

    fn compare_and_swap(&mut self, key: &Key, expected: Option<&str>, bytes: &[u8]) -> Result<()> {
        if Self::key_is_blob(key) {
            return Err(Error::unsupported_representation(format!(
                "blob plane objects are immutable; refusing a compare-and-swap on {}",
                key.display_path()
            )));
        }
        self.commits
            .compare_exchange(key, expected, bytes)
            .map(|_| ())
    }

    fn publish_head(
        &mut self,
        expected: HeadExpectation,
        generation: u64,
        commit: Hash32,
        commit_len: u64,
        timestamp_ns: i64,
        _offset: u64,
        _slot_bytes: &[u8],
    ) -> Result<()> {
        let next = crate::planes::CommitHead {
            generation,
            commit,
            commit_len,
            timestamp_ns,
        };
        let expected = if expected.is_none() {
            None
        } else {
            Some(crate::planes::CommitHead {
                generation: expected.generation,
                commit: expected.commit,
                commit_len: 0,
                timestamp_ns: 0,
            })
        };
        // `compare_exchange_head` compares generation and commit hash only, so
        // the placeholder length/timestamp above cannot cause a false match.
        self.commits.compare_exchange_head(expected.as_ref(), &next)
    }

    fn plane_capabilities(&self) -> Option<crate::planes::PlaneCapabilities> {
        Some(crate::planes::PlaneCapabilities {
            blobs: self.blobs.blob_capabilities(),
            commits: self.commits.commit_capabilities(),
        })
    }

    fn plane_stats(&self) -> Option<crate::planes::PlaneStats> {
        Some(crate::planes::PlaneStats {
            blobs: self.blobs.stats(),
            commits: self.commits.stats(),
        })
    }

    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        let bytes = crate::planes::probe_bytes();
        let blobs = self.blobs.head_part(&bytes)?;
        let commits = self.commits.probe()?;
        Ok(Some(crate::planes::PlaneProbes { blobs, commits }))
    }
}

/// Bounded slice of a control record for [`CompositeBackend::read`].
fn slice_control(bytes: &[u8], key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
    let total = bytes.len() as u64;
    if offset > total {
        return Err(Error::invalid_argument(format!(
            "read offset {offset} is beyond {} ({total} bytes)",
            key.display_path()
        )));
    }
    let start = usize::try_from(offset)
        .map_err(|_| Error::limit_exceeded("read offset does not fit memory"))?;
    let want = total.saturating_sub(offset).min(len);
    let end = start
        .checked_add(usize::try_from(want).unwrap_or(usize::MAX))
        .ok_or_else(|| Error::limit_exceeded("read range overflows memory"))?
        .min(bytes.len());
    Ok(bytes[start..end].to_vec())
}
