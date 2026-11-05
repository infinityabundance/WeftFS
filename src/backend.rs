//! Physical storage backends for the WeftFS store core.
//!
//! The store never manipulates filesystem paths directly. It addresses every
//! durable artifact through a [`Key`] and performs physical I/O through a
//! [`StorageBackend`]. The commit protocol (write blobs, fsync, publish the
//! commit record, publish the superblock slot, fsync the root) is unchanged;
//! only the publication primitive differs per backend.
//!
//! Two backends ship today:
//!
//! * [`LocalBackend`] — the normative layout from `WEFT-MODEL.md` §2:
//!   `tmp/` staging plus `fsync` + `rename(2)` + directory `fsync`.
//! * [`ObjectBackend`] — an immutable-object namespace where every key maps to
//!   `objects/<2-hex>/<name>`. Publication is `fsync` + `link(2)` + unlink of
//!   the temporary name, so it does not depend on POSIX `rename` replacement.
//!   If `link(2)` is unavailable the backend uses `rename(2)` and says so in
//!   its [`Capabilities::publication`] text and diagnostics.
//!
//! A caller may also declare a [`BackendKind::Posix`] backend with weaker
//! capabilities (for example a network filesystem that does not honour
//! `fsync`). The store consults [`Capabilities`] and fails closed: a writable
//! open requires `durable_fsync`, and any publication requires
//! `atomic_publish`. No degraded mode exists.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use fs2::FileExt as Fs2FileExt;

use crate::error::{Error, Result};
use crate::format::MAX_CHUNK_SIZE;
use crate::identity::Hash32;

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
            Key::Object { hash } | Key::Manifest { hash } => Some(hash.to_hex()[0..2].to_owned()),
            Key::Index { commit } => Some(commit.to_hex()[0..2].to_owned()),
            _ => None,
        }
    }

    /// Logical file name within the kind's namespace.
    pub fn name(&self) -> Option<&str> {
        match self {
            Key::Commit { .. } => None,
            Key::Object { .. } | Key::Manifest { .. } | Key::Index { .. } => None,
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
            Key::Prefix(kind) => format!("{}/*", kind.as_str()),
            Key::Other { name, .. } => name.clone(),
        }
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
}

/// Which physical backend the store uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// Normative local layout (`WEFT-MODEL.md` §2).
    Local,
    /// Immutable-object namespace published with `link(2)`.
    Object,
    /// A POSIX-named backend whose guarantees are declared by the caller.
    /// Used to represent storage such as 3FS before its consistency and
    /// durability claims have been measured. The declared capabilities are
    /// enforced exactly.
    Posix { capabilities: Capabilities },
}

impl BackendKind {
    pub fn label(&self) -> &'static str {
        match self {
            BackendKind::Local => "local",
            BackendKind::Object => "object",
            BackendKind::Posix { .. } => "posix",
        }
    }

    /// Declared capabilities without touching disk.
    pub fn capabilities(&self) -> Capabilities {
        match self {
            BackendKind::Local => Capabilities::local_posix(),
            BackendKind::Object => Capabilities::immutable_object(),
            BackendKind::Posix { capabilities } => capabilities.clone(),
        }
    }

    /// Construct the backend rooted at `root`.
    pub fn open(&self, root: impl AsRef<Path>) -> Box<dyn StorageBackend + Send> {
        let root = root.as_ref().to_path_buf();
        match self {
            BackendKind::Local => Box::new(LocalBackend::new(root)),
            BackendKind::Object => Box::new(ObjectBackend::new(root)),
            BackendKind::Posix { capabilities } => {
                Box::new(LocalBackend::with_capabilities(root, capabilities.clone()))
            }
        }
    }
}

/// Counters for receipts and tests. Backends update these with relaxed atomics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

/// Physical I/O abstraction used by the store core.
pub trait StorageBackend: Send {
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

    /// Backend root directory.
    fn root(&self) -> &Path;

    /// Publication/read counters.
    fn stats(&self) -> BackendStats;

    /// Backend diagnostics (for example a rename fallback).
    fn diagnostics(&self) -> Vec<String>;
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
        let sharded = matches!(*kind, KeyKind::Object | KeyKind::Manifest | KeyKind::Index);
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
            Key::Tmp { name } => TMP_PREFIX.to_owned() + name,
            Key::Root | Key::Prefix(_) | Key::Other { .. } => String::new(),
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
}

fn classify_object(kind: KeyKind, shard: &str, name: &str) -> Option<Key> {
    let hex = match kind {
        KeyKind::Object => name,
        KeyKind::Manifest => name.strip_prefix("m-")?,
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
        _ => return None,
    };
    let hash = Hash32::from_lower_hex(hex).ok()?;
    if &hash.to_hex()[0..2] != shard {
        return None;
    }
    Some(match kind {
        KeyKind::Object => Key::object(hash),
        KeyKind::Manifest => Key::manifest(hash),
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
