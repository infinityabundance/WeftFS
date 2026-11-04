//! The WeftFS transactional store core (no FUSE frontend).
//!
//! Layout, commit publication order, recovery, and identity rules follow
//! `spec/WEFT-MODEL.md`. All mutation methods publish exactly one commit. The
//! store is single-writer (`flock`); readers pin the generation they opened.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as Fs2FileExt;
use weftcodec::context::CausalClosure;
use weftcodec::dependency::MapResolver;
use weftcodec::frame::{Frame, Mode};
use weftcodec::selector::Selector;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::Principal;
use crate::error::{Error, Result};
use crate::format::{
    encode_commit_file, encode_state, ChunkRef, Commit, Manifest, Representation,
    RepresentationKind, SuperblockSlot, FORMAT_CONTENTS, SLOT_BYTES, SUPERBLOCK_BYTES,
};
use crate::identity::{self, Hash32};
use crate::index::{StateIndex, StateIndexCache, DEFAULT_INDEX_CAPACITY};
use crate::recovery::{self, BlobKind};
use crate::representation::{self, MAX_CODEC_DEPENDENCY_DEPTH};
use crate::state::{
    derive_entry_id, derive_version_id, derive_version_id_with_identity, validate_name_bytes,
    validate_snapshot_name, EntryRecord, ObjectKind, ObjectRecord, Op, RelationKind,
    RelationRecord, RelationTarget, State, ATTR_SYMLINK_TARGET, SAVED_CONTEXT_PREDICATE,
    SAVED_CONTEXT_SELECTOR_ATTR, XATTR_PREFIX,
};

/// Maximum logical file size accepted by every create/write/append path.
///
/// 1 TiB is a deliberate format/resource bound, not a product preference. A
/// v1 manifest carries one `ChunkRef` per `chunk_size` bytes; at the default
/// 64 KiB chunk size a full 1 TiB file still encodes to a manifest below the
/// `MAX_CHUNK_SIZE * 64` defensive cap enforced by [`Store::store_manifest`].
/// The bound is enforced with `LimitExceeded` on create, buffered write,
/// range write, append, truncate, zero/collapse/insert, and on every streaming
/// chunk while it is read.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
/// Maximum number of chunk references in one manifest (16 Mi, i.e. exactly
/// the 1 TiB file bound at the default 64 KiB chunk size). A smaller
/// configured `chunk_size` reaches this resource limit first and fails with
/// `LimitExceeded`; it bounds the in-memory and encoded manifest rather than
/// silently growing with file size.
pub const MAX_CHUNKS_PER_FILE: u64 = 16_777_216;
/// Streaming source reads are performed in units of at least this many bytes
/// (rounded up to a multiple of the manifest chunk size, and never below one
/// chunk). This bounds the streaming reader/writer buffer independently of
/// file size.
pub const STREAM_IO_BYTES: usize = 1024 * 1024;
/// Default chunk size.
pub const DEFAULT_CHUNK_SIZE: u64 = 65_536;
/// Default checkpoint interval (commits).
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 256;
/// Default GC grace period (seconds).
pub const DEFAULT_GC_GRACE_SECS: u64 = 3_600;
/// Default threshold below which `CodecAuto` leaves a file on the raw path.
pub const DEFAULT_CODEC_MIN_BYTES: u64 = 4_096;
/// Default margin by which a DELTA frame must beat a ZSTD frame, in percent.
pub const DEFAULT_CODEC_DELTA_MARGIN_PERCENT: u8 = 10;
/// Maximum name length of an extended attribute, in bytes (Linux `XATTR_NAME_MAX`
/// is 255; the stored key adds the `xattr.` prefix).
pub const MAX_XATTR_NAME_BYTES: usize = 255;
/// Maximum value length of an extended attribute, in bytes (Linux
/// `XATTR_SIZE_MAX`).
pub const MAX_XATTR_VALUE_BYTES: u64 = 65_536;
/// Maximum number of extended attributes on one object.
pub const MAX_XATTR_COUNT: usize = 1_024;

/// Open mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    ReadWrite,
    ReadOnly,
}

/// How the writer physically represents file content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepresentationMode {
    /// Frozen v1 chunk manifests. The default for every existing store,
    /// fixture, test, and handoff.
    Raw,
    /// Experimental: encode large files as WeftCodec frames (and DELTA when a
    /// causal base exists), falling back to a v2 raw manifest when the frame
    /// would not be smaller. Rust-only for now.
    CodecAuto,
}

impl Default for RepresentationMode {
    fn default() -> Self {
        RepresentationMode::Raw
    }
}

impl RepresentationMode {
    /// Canonical CLI spelling (`raw` or `codec-auto`).
    pub fn as_str(self) -> &'static str {
        match self {
            RepresentationMode::Raw => "raw",
            RepresentationMode::CodecAuto => "codec-auto",
        }
    }
}

/// Store options.
#[derive(Clone, Debug)]
pub struct Options {
    pub owner_uid: u64,
    pub owner_gid: u64,
    pub chunk_size: u64,
    pub checkpoint_interval: u64,
    pub gc_grace_secs: u64,
    /// Test/fixture clock override: every generated mtime and commit timestamp.
    pub fixed_timestamp_ns: Option<i64>,
    /// Physical representation policy. `Raw` (the default) writes the frozen
    /// v1 manifest; `CodecAuto` is experimental, opt-in, and Rust-only.
    pub representation: RepresentationMode,
    /// `CodecAuto` leaves files smaller than this on the raw path.
    pub codec_min_bytes: u64,
    /// A DELTA frame must be at least this percent smaller than the ZSTD frame
    /// to be selected. `0` disables the margin (any strictly smaller DELTA wins).
    pub codec_delta_margin_percent: u8,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
            gc_grace_secs: DEFAULT_GC_GRACE_SECS,
            fixed_timestamp_ns: None,
            representation: RepresentationMode::Raw,
            codec_min_bytes: DEFAULT_CODEC_MIN_BYTES,
            codec_delta_margin_percent: DEFAULT_CODEC_DELTA_MARGIN_PERCENT,
        }
    }
}

/// A directory entry returned by [`Store::list_dir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub entry_id: Hash32,
    pub target: Hash32,
}

/// One commit on the published parent chain (newest first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineageEntry {
    pub commit: Hash32,
    pub generation: u64,
    pub timestamp_ns: i64,
}

/// Mutable attribute set for [`Store::setattr`].
#[derive(Clone, Debug, Default)]
pub struct SetAttr {
    pub mode: Option<u64>,
    pub uid: Option<u64>,
    pub gid: Option<u64>,
    pub mtime_ns: Option<i64>,
    /// Replace the whole attribute map when `Some`.
    pub attrs: Option<BTreeMap<String, Value>>,
}

/// A verification problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyProblem {
    pub class: String,
    pub detail: String,
}

/// Result of [`Store::verify`].
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub ok: bool,
    pub full: bool,
    pub generation: u64,
    pub commit: Option<Hash32>,
    pub checked_commits: u64,
    pub checked_objects: u64,
    pub checked_manifests: u64,
    pub checked_chunks: u64,
    pub checked_context_roots: u64,
    pub checked_causal: u64,
    /// Codec representations fully decoded and re-hashed by `verify --full`.
    pub checked_representations: u64,
    pub checked_capabilities: u64,
    pub rejected_generations: Vec<u64>,
    pub problems: Vec<VerifyProblem>,
}

/// A GC candidate/deletion record.
#[derive(Clone, Debug, Default)]
pub struct GcReport {
    pub applied: bool,
    pub marked_manifests: u64,
    pub marked_chunks: u64,
    pub candidates: Vec<String>,
    pub deleted: Vec<String>,
    pub deleted_bytes: u64,
    pub retained: u64,
    /// Unreferenced object records removed in a dedicated commit (a crash left
    /// an unlinked-but-pinned object behind).
    pub removed_orphans: Vec<String>,
    pub problems: Vec<String>,
}

/// Result of [`Store::statfs`], all counters derived from replayed state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatFs {
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    pub max_name_len: u64,
}

/// A saved selector context (`WEFT-MODEL.md` §2.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedContext {
    pub name: String,
    /// Lowercase hex of the canonical selector bytes.
    pub selector_hex: String,
    pub selector_id: Hash32,
    pub relation_id: Hash32,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn crash_point(phase: &str) {
    if let Ok(configured) = std::env::var("WEFT_CRASH_PHASE") {
        if configured == phase {
            std::process::abort();
        }
    }
}

fn fsync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path)
        .map_err(|err| Error::io(format!("cannot open directory {}: {err}", path.display())))?;
    dir.sync_all()
        .map_err(|err| Error::io(format!("cannot fsync directory {}: {err}", path.display())))
}

fn make_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|err| Error::io(format!("cannot chmod {}: {err}", path.display())))?;
    Ok(())
}

/// Restore one journalled state key: re-insert the previous value, or remove
/// the key when it did not exist before the staged op.
fn restore<K: Ord, V>(map: &mut BTreeMap<K, V>, key: K, previous: Option<V>) {
    match previous {
        Some(value) => {
            map.insert(key, value);
        }
        None => {
            map.remove(&key);
        }
    }
}

/// Undo record for one in-memory state key touched by a staged group op.
///
/// Group commit applies each mutation to the live [`State`] immediately so
/// reads inside the closure observe their own writes. Rolling a group back
/// therefore needs a cheap journal instead of a whole-`State` clone: one
/// record per touched key, holding the value that was there before the op.
#[derive(Debug)]
enum Undo {
    Object(Hash32, Option<ObjectRecord>),
    Entry(Hash32, Option<EntryRecord>),
    Relation(Hash32, Option<RelationRecord>),
    Snapshot(String, Option<Hash32>),
    Tip(Hash32, Option<Hash32>),
}

/// The open store.
pub struct Store {
    root: PathBuf,
    mode: OpenMode,
    options: Options,
    _lock: Option<File>,
    state: State,
    head: Hash32,
    generation: u64,
    /// Next commit generation to write. It can exceed `generation + 1` after
    /// recovery rejected a torn commit file, so rejected data is never
    /// overwritten.
    next_generation: u64,
    rejected: Vec<u64>,
    notes: Vec<String>,
    /// Non-zero while [`Store::group`] has a closure open. Mutation methods
    /// buffer their ops instead of publishing, and `group` refuses to nest.
    group_depth: u32,
    /// Ops buffered by the open group, in the exact order they were applied.
    pending_ops: Vec<Op>,
    /// Undo journal for the open group (see [`Undo`]).
    group_undo: Vec<Undo>,
    /// A `checkpoint()` request raised inside the group.
    group_force_checkpoint: bool,
    /// In-memory open-handle pins by object id. Consulted by `unlink` (an
    /// unlinked object record survives while pinned) and by GC (pinned content
    /// is never swept). Pins are process-local and do not survive a crash.
    pins: BTreeMap<Hash32, u64>,
    /// In-memory content pins by manifest id, for readers that hold a pinned
    /// `(content, size)` pair (`Store::read_content`).
    content_pins: BTreeMap<Hash32, u64>,
    /// Bounded cache of selector indexes keyed by snapshot commit hash. The
    /// index is derived data only; correctness is checked by `StateIndex`
    /// shape and no store mutation is ever written from it.
    index_cache: Mutex<StateIndexCache>,
}

impl Store {
    /// Initialize a new store. The directory must be absent or empty.
    pub fn init(path: impl AsRef<Path>, options: Options) -> Result<Store> {
        validate_options(&options)?;
        let root = path.as_ref().to_path_buf();

        if root.exists() {
            if !root.is_dir() {
                return Err(Error::exists("store path exists and is not a directory"));
            }
            if root.join("FORMAT").exists() {
                return Err(Error::exists("a WeftFS store already exists at this path"));
            }
            let mut entries = fs::read_dir(&root)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", root.display())))?;
            if entries.next().is_some() {
                return Err(Error::exists("store path is not empty"));
            }
        } else {
            make_dir(&root)?;
        }
        make_dir(&root.join("commits"))?;
        make_dir(&root.join("objects"))?;
        make_dir(&root.join("manifests"))?;
        make_dir(&root.join("refs"))?;
        make_dir(&root.join("refs").join("snapshots"))?;
        make_dir(&root.join("refs").join("tags"))?;
        make_dir(&root.join("tmp"))?;

        write_new_file(&root.join("FORMAT"), FORMAT_CONTENTS.as_bytes(), 0o644)?;

        let lock_path = root.join("LOCK");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(&lock_path)
            .map_err(|err| Error::io(format!("cannot open LOCK: {err}")))?;
        Fs2FileExt::try_lock_exclusive(&lock)
            .map_err(|err| Error::busy(format!("store is locked by another writer: {err}")))?;

        let superblock = root.join("superblock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&superblock)
            .map_err(|err| Error::io(format!("cannot create superblock: {err}")))?;
        file.set_len(SUPERBLOCK_BYTES as u64)
            .map_err(|err| Error::io(format!("cannot size superblock: {err}")))?;
        file.sync_all()?;
        drop(file);

        let mut store = Store {
            root,
            mode: OpenMode::ReadWrite,
            options,
            _lock: Some(lock),
            state: State::default(),
            head: Hash32::ZERO,
            generation: 0,
            next_generation: 2,
            rejected: Vec::new(),
            notes: Vec::new(),
            group_depth: 0,
            pending_ops: Vec::new(),
            group_undo: Vec::new(),
            group_force_checkpoint: false,
            pins: BTreeMap::new(),
            content_pins: BTreeMap::new(),
            index_cache: Mutex::new(StateIndexCache::new(DEFAULT_INDEX_CAPACITY)),
        };
        store.write_genesis()?;
        fsync_dir(&store.root)?;
        Ok(store)
    }

    /// Open an existing store.
    pub fn open(path: impl AsRef<Path>, mode: OpenMode) -> Result<Store> {
        Self::open_impl(path, mode, false)
    }

    /// Benchmark-only: open through the original full commit-directory scan so
    /// the fast path's cost can be measured against it.
    #[doc(hidden)]
    pub fn open_full_scan_for_bench(path: impl AsRef<Path>, mode: OpenMode) -> Result<Store> {
        Self::open_impl(path, mode, true)
    }

    fn open_impl(path: impl AsRef<Path>, mode: OpenMode, full_scan: bool) -> Result<Store> {
        let root = path.as_ref().to_path_buf();
        let format = fs::read_to_string(root.join("FORMAT"))
            .map_err(|err| Error::corrupt(format!("cannot read FORMAT: {err}")))?;
        if format != FORMAT_CONTENTS {
            return Err(Error::unsupported_version(format!(
                "FORMAT is `{}`, expected `{FORMAT_CONTENTS}`",
                format.escape_debug()
            )));
        }
        let lock = if mode == OpenMode::ReadWrite {
            let lock_path = root.join("LOCK");
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(false)
                .open(&lock_path)
                .map_err(|err| Error::io(format!("cannot open LOCK: {err}")))?;
            Fs2FileExt::try_lock_exclusive(&lock)
                .map_err(|err| Error::busy(format!("store is locked by another writer: {err}")))?;
            Some(lock)
        } else {
            None
        };
        if mode == OpenMode::ReadWrite {
            ensure_layout(&root)?;
            clean_stale_tmp(&root, DEFAULT_GC_GRACE_SECS)?;
        }

        // Read-only opens use the directory-free fast path. Writable opens
        // must observe the highest commit generation so `next_generation`
        // never reuses a rejected generation; they still bound commit decodes
        // to the checkpoint window.
        let recovery = if full_scan {
            recovery::recover_full_scan(&root)?
        } else if mode == OpenMode::ReadWrite {
            recovery::recover_for_write(&root)?
        } else {
            recovery::recover(&root)?
        };
        let next_generation = recovery
            .generation
            .max(recovery.highest_file_generation)
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        Ok(Store {
            root,
            mode,
            options: Options::default(),
            _lock: lock,
            state: recovery.state,
            head: recovery.head,
            generation: recovery.generation,
            next_generation,
            rejected: recovery.rejected,
            notes: recovery.notes,
            group_depth: 0,
            pending_ops: Vec::new(),
            group_undo: Vec::new(),
            group_force_checkpoint: false,
            pins: BTreeMap::new(),
            content_pins: BTreeMap::new(),
            index_cache: Mutex::new(StateIndexCache::new(DEFAULT_INDEX_CAPACITY)),
        })
    }

    /// Open with explicit runtime options (chunk size affects new writes only;
    /// existing manifests carry their own chunk size).
    pub fn open_with_options(
        path: impl AsRef<Path>,
        mode: OpenMode,
        options: Options,
    ) -> Result<Store> {
        validate_options(&options)?;
        let mut store = Self::open(path, mode)?;
        store.options = options;
        Ok(store)
    }

    /// Replace runtime options on an open store.
    pub fn set_options(&mut self, options: Options) -> Result<()> {
        validate_options(&options)?;
        self.options = options;
        Ok(())
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn mode(&self) -> OpenMode {
        self.mode
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Current published generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current commit root hash (the live snapshot root).
    pub fn snapshot_root(&self) -> Hash32 {
        self.head
    }

    /// Alias for [`Store::snapshot_root`].
    pub fn head(&self) -> Hash32 {
        self.head
    }

    /// Generations rejected during open-time recovery.
    pub fn rejected_generations(&self) -> &[u64] {
        &self.rejected
    }

    /// Non-fatal recovery notes (torn slots, ignored checkpoints, …).
    pub fn recovery_notes(&self) -> &[String] {
        &self.notes
    }

    /// Current replayed state.
    pub fn current_state(&self) -> &State {
        &self.state
    }

    /// The selector index for `state`, cached under `snapshot` (the commit
    /// whose replay produced it). The cache holds at most
    /// `DEFAULT_INDEX_CAPACITY` entries and evicts oldest-first. The index is
    /// only used to narrow candidates; `eval_expr` is still authoritative.
    pub(crate) fn state_index(&self, snapshot: Hash32, state: &State) -> Arc<StateIndex> {
        // Inside a group the state changes while `head` does not, so a cached
        // index keyed by the commit hash can no longer be validated by
        // `matches_state` alone (an attribute-only edit keeps every count and
        // the generation unchanged). Rebuild per call instead of serving a
        // stale candidate set; the index may only ever over-approximate.
        if self.group_depth != 0 {
            return Arc::new(StateIndex::build(state));
        }
        let mut cache = self
            .index_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = cache.get(snapshot) {
            if index.matches_state(state) {
                return index;
            }
        }
        let index = Arc::new(StateIndex::build(state));
        cache.insert(snapshot, index.clone());
        index
    }

    /// Rebuild the state at an arbitrary commit by replay.
    pub fn state_at(&self, commit: Hash32) -> Result<State> {
        if commit == self.head {
            return Ok(self.state.clone());
        }
        if !self.is_published_ancestor(commit)? {
            return Err(Error::not_found(
                "commit is not on the published parent chain",
            ));
        }
        let (state, _notes) = recovery::load_state_at(&self.root, commit, u64::MAX, false)?;
        Ok(state)
    }

    /// Only commits reachable from the recovered head are addressable. This
    /// keeps an uncommitted crash-tail commit file from exposing state that the
    /// superblock never published.
    fn is_published_ancestor(&self, target: Hash32) -> Result<bool> {
        let index = recovery::commit_index(&self.root, u64::MAX)?;
        let mut cursor = Some(self.head);
        let mut seen = BTreeSet::new();
        while let Some(hash) = cursor {
            if hash == target {
                return Ok(true);
            }
            if !seen.insert(hash) {
                break;
            }
            cursor = index.get(&hash).and_then(|loaded| loaded.commit.parent);
        }
        Ok(false)
    }

    /// The committed ancestry of `base`, newest first, down to the genesis
    /// commit. This is the *published* parent chain: every entry was reachable
    /// from the recovered head when `base` was selected.
    pub fn commit_lineage(&self, base: Hash32) -> Result<Vec<LineageEntry>> {
        let index = recovery::commit_index(&self.root, u64::MAX)?;
        let mut out = Vec::new();
        let mut cursor = Some(base);
        let mut seen = BTreeSet::new();
        while let Some(hash) = cursor {
            if !seen.insert(hash) {
                return Err(Error::corrupt("commit parent chain contains a cycle"));
            }
            let loaded = index.get(&hash).ok_or_else(|| {
                Error::not_found("commit is not present on the store's parent chain")
            })?;
            out.push(LineageEntry {
                commit: hash,
                generation: loaded.generation,
                timestamp_ns: loaded.commit.timestamp_ns,
            });
            cursor = loaded.commit.parent;
        }
        Ok(out)
    }

    /// The newest commit in `base`'s lineage whose timestamp is `<= at_ns`.
    ///
    /// Ties on timestamp are broken by the higher generation. `None` means the
    /// requested time predates the first commit in the lineage; callers must
    /// fail closed rather than substitute a different state.
    pub fn commit_at_or_before(&self, base: Hash32, at_ns: i64) -> Result<Option<LineageEntry>> {
        let mut best: Option<LineageEntry> = None;
        for entry in self.commit_lineage(base)? {
            if entry.timestamp_ns > at_ns {
                continue;
            }
            let better = match best {
                None => true,
                Some(current) => {
                    (entry.timestamp_ns, entry.generation)
                        > (current.timestamp_ns, current.generation)
                }
            };
            if better {
                best = Some(entry);
            }
        }
        Ok(best)
    }

    // ---- reads -----------------------------------------------------------

    /// Look up `(parent, name)` and return the target object id.
    pub fn lookup(&self, parent: Hash32, name: &[u8]) -> Result<Hash32> {
        self.state.lookup(parent, name)
    }

    /// List a directory, sorted by unsigned byte-wise name.
    pub fn list_dir(&self, parent: Hash32) -> Result<Vec<DirEntry>> {
        let entries = self.state.list_dir(parent)?;
        Ok(entries
            .into_iter()
            .map(|entry| DirEntry {
                name: entry.name,
                entry_id: entry.id,
                target: entry.target,
            })
            .collect())
    }

    /// Object metadata.
    pub fn object_meta(&self, id: Hash32) -> Result<ObjectRecord> {
        self.state
            .objects
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::not_found("no such object"))
    }

    /// Resolve a `/`-separated path against the current state. No symlink
    /// following is performed at this layer.
    pub fn resolve_path(&self, path: &str) -> Result<Hash32> {
        resolve_in(&self.state, path)
    }

    /// Resolve a path against a pinned commit.
    pub fn resolve_path_at(&self, snapshot: Option<Hash32>, path: &str) -> Result<Hash32> {
        match snapshot {
            None => self.resolve_path(path),
            Some(commit) => {
                let state = self.state_at(commit)?;
                resolve_in(&state, path)
            }
        }
    }

    /// Resolve pre-split byte components.
    pub fn resolve_components(&self, components: &[&[u8]]) -> Result<Hash32> {
        let mut current = State::ROOT_ID;
        for component in components {
            validate_name_bytes(component)?;
            let object = self
                .state
                .objects
                .get(&current)
                .ok_or_else(|| Error::not_found("no such parent object"))?;
            if object.kind != ObjectKind::Dir {
                return Err(Error::not_dir("path component is not a directory"));
            }
            current = self.state.lookup(current, component)?;
        }
        Ok(current)
    }

    /// Read a complete object. Files verify every chunk hash and the
    /// whole-content hash; symlinks return their target bytes; directories are
    /// `IsDir`.
    pub fn read_object(&self, id: Hash32) -> Result<Vec<u8>> {
        let record = self.object_meta(id)?;
        match record.kind {
            ObjectKind::Dir => Err(Error::is_dir("object is a directory")),
            ObjectKind::Symlink => record
                .symlink_target()
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| Error::corrupt("symlink object has no target attribute")),
            ObjectKind::File => {
                let manifest = recovery::read_manifest(&self.root, record.content)?;
                if manifest.size != record.size {
                    return Err(Error::corrupt("object size disagrees with manifest"));
                }
                self.read_manifest_bytes(&manifest, record.content, 0, manifest.size, true)
            }
        }
    }

    /// Read an object in a pinned snapshot state.
    pub fn read_object_at(&self, snapshot: Option<Hash32>, id: Hash32) -> Result<Vec<u8>> {
        match snapshot {
            None => self.read_object(id),
            Some(commit) => {
                let state = self.state_at(commit)?;
                let record = state
                    .objects
                    .get(&id)
                    .ok_or_else(|| Error::not_found("no such object at this snapshot"))?;
                match record.kind {
                    ObjectKind::Dir => Err(Error::is_dir("object is a directory")),
                    ObjectKind::Symlink => record
                        .symlink_target()
                        .map(|bytes| bytes.to_vec())
                        .ok_or_else(|| Error::corrupt("symlink object has no target attribute")),
                    ObjectKind::File => {
                        let manifest = recovery::read_manifest(&self.root, record.content)?;
                        if manifest.size != record.size {
                            return Err(Error::corrupt("object size disagrees with manifest"));
                        }
                        self.read_manifest_bytes(&manifest, record.content, 0, manifest.size, true)
                    }
                }
            }
        }
    }

    /// Read a byte range from a file object.
    pub fn read_range(&self, id: Hash32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let record = self.object_meta(id)?;
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument("read_range requires a file object"));
        }
        let manifest = recovery::read_manifest(&self.root, record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        self.read_manifest_bytes(&manifest, record.content, offset, length, false)
    }

    /// Read a byte range directly from a content root (manifest id). Safe for
    /// snapshot materialization because it does not consult live object state.
    pub fn read_content(&self, content_root: Hash32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let manifest = recovery::read_manifest(&self.root, content_root)?;
        let whole = offset == 0 && length >= manifest.size;
        self.read_manifest_bytes(&manifest, content_root, offset, length, whole)
    }

    fn read_manifest_bytes(
        &self,
        manifest: &Manifest,
        content_root: Hash32,
        offset: u64,
        length: u64,
        verify_whole: bool,
    ) -> Result<Vec<u8>> {
        let mut seen = vec![content_root];
        self.read_manifest_bytes_inner(manifest, offset, length, verify_whole, 0, &mut seen)
    }

    fn read_manifest_bytes_inner(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
    ) -> Result<Vec<u8>> {
        match &manifest.representation {
            None => self.read_raw_manifest_bytes(manifest, offset, length, verify_whole),
            Some(rep) if rep.kind == RepresentationKind::Raw => {
                self.read_raw_manifest_bytes(manifest, offset, length, verify_whole)
            }
            Some(rep) => self.read_codec_manifest_bytes(
                manifest,
                rep,
                offset,
                length,
                verify_whole,
                depth,
                seen,
            ),
        }
    }

    /// The frozen v1 (and v2 `kind:"raw"`) assembly path.
    fn read_raw_manifest_bytes(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
    ) -> Result<Vec<u8>> {
        if offset >= manifest.size {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(length).min(manifest.size);
        let capacity = checked_usize(end - offset, "range length")?;
        let mut out = Vec::with_capacity(capacity);
        let mut cursor: u64 = 0;
        for chunk in &manifest.chunks {
            let chunk_start = cursor;
            let chunk_end = cursor
                .checked_add(chunk.len)
                .ok_or_else(|| Error::corrupt("chunk offset overflow"))?;
            cursor = chunk_end;
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            let bytes = recovery::read_blob_verified(
                &self.root,
                BlobKind::Object,
                chunk.hash,
                identity::CHUNK_DOMAIN,
            )?;
            if bytes.len() as u64 != chunk.len {
                return Err(Error::corrupt("chunk length disagrees with manifest"));
            }
            let local_start =
                checked_usize(offset.saturating_sub(chunk_start), "chunk-local read start")?;
            let local_end =
                checked_usize((end - chunk_start).min(chunk.len), "chunk-local read end")?;
            out.extend_from_slice(&bytes[local_start..local_end]);
        }
        if out.len() as u64 != end - offset {
            return Err(Error::corrupt("assembled range length mismatch"));
        }
        if verify_whole && offset == 0 && end == manifest.size {
            let digest = identity::content_hash(&out);
            if digest != manifest.content_hash {
                return Err(Error::corrupt("whole-content hash mismatch"));
            }
        }
        Ok(out)
    }

    /// Decode a v2 `kind:"codec"` representation.
    ///
    /// The frame blob is fetched and parsed with default codec limits. Its
    /// payload mode must match the manifest's informational `strategy`; its
    /// decoded length must match the manifest size; and every dependency is
    /// materialised from its content root (recursively, bounded depth, cycle
    /// checked) and verified against the frame's dependency table. A missing,
    /// unreadable, or mismatched dependency returns `Corrupt`; no partial or
    /// approximate bytes are ever returned.
    #[allow(clippy::too_many_arguments)]
    fn read_codec_manifest_bytes(
        &self,
        manifest: &Manifest,
        rep: &Representation,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
    ) -> Result<Vec<u8>> {
        if depth > MAX_CODEC_DEPENDENCY_DEPTH {
            return Err(Error::corrupt(format!(
                "codec representation dependency depth exceeds {MAX_CODEC_DEPENDENCY_DEPTH}"
            )));
        }
        let frame_id = rep
            .frame
            .ok_or_else(|| Error::corrupt("codec representation has no frame id"))?;
        if rep.decoded_size != manifest.size {
            return Err(Error::corrupt(
                "codec representation decoded_size disagrees with manifest size",
            ));
        }
        let frame_bytes = recovery::read_blob_verified(
            &self.root,
            BlobKind::Manifest,
            frame_id,
            identity::MANIFEST_DOMAIN,
        )?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected_mode = match rep.strategy.as_str() {
            "zstd" => Mode::Zstd,
            "delta" => Mode::Delta,
            "reference" => Mode::Reference,
            other => {
                return Err(Error::corrupt(format!(
                    "codec representation has unknown strategy `{other}`"
                )))
            }
        };
        if frame.header.mode != expected_mode {
            return Err(Error::corrupt(format!(
                "representation strategy `{}` disagrees with frame mode `{}`",
                rep.strategy,
                frame.header.mode.as_str()
            )));
        }
        if frame.header.decoded_len != manifest.size {
            return Err(Error::corrupt(format!(
                "frame decoded_len {} disagrees with manifest size {}",
                frame.header.decoded_len, manifest.size
            )));
        }
        if frame.dependencies.len() != rep.dependencies.len() {
            return Err(Error::corrupt(
                "representation dependency count disagrees with the frame",
            ));
        }

        let mut resolver = MapResolver::new();
        for (index, dependency) in frame.dependencies.iter().enumerate() {
            let dep_root = rep.dependencies[index];
            if seen.contains(&dep_root) {
                return Err(Error::corrupt("codec representation dependency cycle"));
            }
            let dep_manifest = recovery::read_manifest(&self.root, dep_root)?;
            seen.push(dep_root);
            let bytes = self.read_manifest_bytes_inner(
                &dep_manifest,
                0,
                dep_manifest.size,
                true,
                depth + 1,
                seen,
            )?;
            seen.pop();
            if bytes.len() as u64 != dependency.decoded_len {
                return Err(Error::corrupt(format!(
                    "dependency {dep_root} has length {} but the frame declares {}",
                    bytes.len(),
                    dependency.decoded_len
                )));
            }
            if identity::content_hash(&bytes) != dependency.hash {
                return Err(Error::corrupt(format!(
                    "dependency {dep_root} content hash disagrees with the frame"
                )));
            }
            resolver.insert(dependency.hash, bytes);
        }

        if offset >= manifest.size {
            return Ok(Vec::new());
        }
        let wanted = (manifest.size - offset).min(length);
        let end = offset
            .checked_add(wanted)
            .ok_or_else(|| Error::corrupt("range end overflow"))?;
        let out =
            if frame.header.is_chunked() && matches!(frame.header.mode, Mode::Raw | Mode::Zstd) {
                self.read_codec_chunk_range(&frame, offset, end)?
            } else {
                // DELTA/REFERENCE and unchunked frames need the whole plaintext to
                // decode; the codec verifies the whole-content hash in the process.
                let plaintext = weftcodec::modes::decode_bytes(&frame_bytes, &resolver, &limits)?;
                let start = usize::try_from(offset)
                    .map_err(|_| Error::limit_exceeded("range offset does not fit usize"))?;
                let stop = usize::try_from(end)
                    .map_err(|_| Error::limit_exceeded("range end does not fit usize"))?;
                plaintext[start..stop].to_vec()
            };
        if out.len() as u64 != wanted {
            return Err(Error::corrupt(
                "codec range read produced an unexpected length",
            ));
        }
        if verify_whole && offset == 0 && end == manifest.size {
            if identity::content_hash(&out) != manifest.content_hash {
                return Err(Error::corrupt("whole-content hash mismatch"));
            }
        }
        Ok(out)
    }

    /// Decode only the frame chunks intersecting `[offset, end)`.
    fn read_codec_chunk_range(&self, frame: &Frame<'_>, offset: u64, end: u64) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(checked_usize(end - offset, "codec range length")?);
        let mut chunk_start: u64 = 0;
        for index in 0..frame.chunks.len() {
            let chunk_len = u64::from(frame.chunks[index].decoded_len);
            let chunk_end = chunk_start
                .checked_add(chunk_len)
                .ok_or_else(|| Error::corrupt("codec chunk extent overflow"))?;
            if chunk_end <= offset {
                chunk_start = chunk_end;
                continue;
            }
            if chunk_start >= end {
                break;
            }
            representation::note_codec_chunk_decode();
            let decoded = weftcodec::modes::decode_chunk(frame, index)?;
            let from = offset.max(chunk_start) - chunk_start;
            let to = end.min(chunk_end) - chunk_start;
            let from = usize::try_from(from)
                .map_err(|_| Error::limit_exceeded("range slice does not fit usize"))?;
            let to = usize::try_from(to)
                .map_err(|_| Error::limit_exceeded("range slice does not fit usize"))?;
            if to > decoded.len() || from > to {
                return Err(Error::corrupt("codec chunk decoded extent is inconsistent"));
            }
            out.extend_from_slice(&decoded[from..to]);
            chunk_start = chunk_end;
        }
        Ok(out)
    }

    // ---- mutations -------------------------------------------------------

    /// Create a regular file and commit atomically.
    pub fn create_file(
        &mut self,
        parent: Hash32,
        name: &[u8],
        bytes: &[u8],
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
    ) -> Result<Hash32> {
        let seed = identity::os_random_32()?;
        self.create_file_seeded(parent, name, bytes, mode, uid, gid, attrs, &seed)
    }

    /// Create a regular file with an explicit ObjectId seed.
    #[allow(clippy::too_many_arguments)]
    pub fn create_file_seeded(
        &mut self,
        parent: Hash32,
        name: &[u8],
        bytes: &[u8],
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
        seed: &[u8],
    ) -> Result<Hash32> {
        self.create_file_seeded_causal(parent, name, bytes, mode, uid, gid, attrs, seed, &[])
    }

    /// Create a regular file with an explicit ObjectId seed and optional
    /// `causal_exact` ancestry. A non-empty `causal` selects `causal_exact`;
    /// an empty slice preserves the frozen `bytes_only` v1 behaviour.
    #[allow(clippy::too_many_arguments)]
    pub fn create_file_seeded_causal(
        &mut self,
        parent: Hash32,
        name: &[u8],
        bytes: &[u8],
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
        seed: &[u8],
        causal: &[Hash32],
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        check_mode(mode)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let base = self.delta_base(causal)?;
        let (manifest_id, content_hash, size) = self.store_file_content_with_base(
            bytes,
            base.as_ref().map(|(root, bytes)| (*root, bytes.as_slice())),
        )?;
        self.commit_created_file(
            parent,
            name,
            seed,
            manifest_id,
            content_hash,
            size,
            mode,
            uid,
            gid,
            attrs,
            causal,
        )
    }

    /// Create a regular file from a streaming reader without buffering it.
    ///
    /// The source is read in bounded units (`max(STREAM_IO_BYTES, chunk_size)`,
    /// rounded up to a chunk multiple). Each `chunk_size` slice is stored as a
    /// content-addressed object blob and fed to the frozen v1 whole-content
    /// hasher, so peak memory is O(io buffer + chunk count + chunk size), not
    /// O(file size). Exactly one durable commit is published with the same
    /// records, version derivation, attributes, and causal handling as
    /// [`Store::create_file`].
    ///
    /// `Options.representation = Raw` writes a frozen v1 chunk manifest. With
    /// `CodecAuto`, a stream that ends inside the bounded preamble
    /// (`max(codec_min_bytes, chunk_size)`, capped at 16 MiB) is buffered and
    /// uses the existing codec path; a larger stream is written as a v2
    /// `kind:"raw"` chunk manifest. Streaming ZSTD/DELTA is deliberately not
    /// implemented (see `docs/store-status.md`).
    ///
    /// A read error or a size-bound violation publishes nothing: the commit
    /// chain and all four state maps are unchanged. Chunk blobs already written
    /// are unreferenced, `verify` still passes, and `gc` reclaims them.
    pub fn create_file_streaming<R: Read>(
        &mut self,
        parent: Hash32,
        name: &[u8],
        reader: R,
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
    ) -> Result<Hash32> {
        let seed = identity::os_random_32()?;
        self.create_file_streaming_seeded(parent, name, reader, mode, uid, gid, attrs, &seed)
    }

    /// [`Store::create_file_streaming`] with an explicit ObjectId seed.
    #[allow(clippy::too_many_arguments)]
    pub fn create_file_streaming_seeded<R: Read>(
        &mut self,
        parent: Hash32,
        name: &[u8],
        reader: R,
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
        seed: &[u8],
    ) -> Result<Hash32> {
        self.create_file_streaming_seeded_causal(
            parent,
            name,
            reader,
            mode,
            uid,
            gid,
            attrs,
            seed,
            &[],
        )
    }

    /// [`Store::create_file_streaming`] with an ObjectId seed and causal
    /// ancestry. An empty `causal` preserves the frozen `bytes_only` identity.
    #[allow(clippy::too_many_arguments)]
    pub fn create_file_streaming_seeded_causal<R: Read>(
        &mut self,
        parent: Hash32,
        name: &[u8],
        mut reader: R,
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
        seed: &[u8],
        causal: &[Hash32],
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        check_mode(mode)?;
        let chunk_size = self.options.chunk_size;
        validate_chunk_size(chunk_size)?;

        let (manifest_id, content_hash, size) = match self.options.representation {
            RepresentationMode::Raw => {
                let (chunks, size, content_hash) =
                    self.stream_raw_chunks(&mut reader, chunk_size, Vec::new())?;
                self.store_streamed_manifest(chunk_size, chunks, size, content_hash, false)?
            }
            RepresentationMode::CodecAuto => {
                let preamble_limit =
                    stream_preamble_limit(self.options.codec_min_bytes, chunk_size)?;
                let mut preamble = vec![0u8; preamble_limit];
                let filled = read_fill(&mut reader, &mut preamble)?;
                preamble.truncate(filled);
                if filled < preamble_limit {
                    // The whole file fits in the bounded preamble: use the
                    // existing buffered codec path unchanged.
                    let base = self.delta_base(causal)?;
                    self.store_content_bytes(
                        &preamble,
                        chunk_size,
                        base.as_ref().map(|(root, bytes)| (*root, bytes.as_slice())),
                        true,
                    )?
                } else {
                    // Larger stream: keep the preamble (a chunk multiple) and
                    // continue streaming into a v2 `kind:"raw"` manifest.
                    // Codec-backed streaming is not implemented; buffering the
                    // file to reach the codec encoder would violate the memory
                    // goal, and a streaming DELTA/ZSTD writer does not exist.
                    let (chunks, size, content_hash) =
                        self.stream_raw_chunks(&mut reader, chunk_size, preamble)?;
                    self.store_streamed_manifest(chunk_size, chunks, size, content_hash, true)?
                }
            }
        };
        self.commit_created_file(
            parent,
            name,
            seed,
            manifest_id,
            content_hash,
            size,
            mode,
            uid,
            gid,
            attrs,
            causal,
        )
    }

    /// Build the object/entry/tip records for a newly created file and publish
    /// exactly one commit. Shared by the buffered and streaming create paths so
    /// both produce byte-identical state transitions.
    #[allow(clippy::too_many_arguments)]
    fn commit_created_file(
        &mut self,
        parent: Hash32,
        name: &[u8],
        seed: &[u8],
        manifest_id: Hash32,
        _content_hash: Hash32,
        size: u64,
        mode: u64,
        uid: u64,
        gid: u64,
        attrs: BTreeMap<String, Value>,
        causal: &[Hash32],
    ) -> Result<Hash32> {
        let object_id = identity::object_id_from_seed(seed);
        if self.state.objects.contains_key(&object_id) {
            return Err(Error::exists("object id already exists"));
        }
        let (attrs, identity_mode, causal_hash, causal_closure) = crate::context::prepare_causal(
            self.current_state(),
            attrs,
            causal,
            &Limits::default(),
        )?;
        if let Some(closure) = causal_closure {
            crate::context::store_causal_closure(self, &closure)?;
        }
        let mtime = self.now_ns();
        let version = derive_version_id_with_identity(
            object_id,
            manifest_id,
            size,
            mtime,
            None,
            identity_mode,
            causal_hash,
        )?;
        let record = ObjectRecord {
            id: object_id,
            kind: ObjectKind::File,
            mode,
            uid,
            gid,
            mtime_ns: mtime,
            version,
            content: manifest_id,
            size,
            attrs,
        };
        let entry = EntryRecord {
            id: derive_entry_id(parent, name)?,
            parent,
            name: name.to_vec(),
            target: object_id,
        };
        let ops = vec![
            Op::PutObject(record),
            Op::PutEntry(entry),
            Op::SetTip {
                object: object_id,
                version,
            },
        ];
        self.commit_ops(ops, false)?;
        Ok(object_id)
    }

    /// Replace a byte range in a file, zero-filling a gap past EOF.
    pub fn write_file(&mut self, id: Hash32, offset: u64, bytes: &[u8]) -> Result<Hash32> {
        self.write_file_causal(id, offset, bytes, &[])
    }

    /// Replace a byte range and optionally set `causal_exact` ancestry.
    ///
    /// This is chunk-granular copy-on-write: only chunks whose bytes actually
    /// change are re-encoded and stored. Chunks that the write does not touch
    /// keep their old references verbatim, and their blobs are not rewritten.
    /// The frozen v1 whole-file ContentId still needs one digest pass over the
    /// file; see `Store::store_file_range` for the exact cost model.
    ///
    /// A zero-length write is a POSIX no-op: it returns the current version id
    /// and publishes no commit. (The previous whole-file implementation would
    /// grow a zero-filled gap when `offset > EOF`; that corner is deliberately
    /// dropped because a zero-byte write must not change a regular file.)
    ///
    /// An empty `causal` slice preserves an existing `causal_exact` closure
    /// (identity mode is sticky) and keeps `bytes_only` objects byte-identical
    /// to the frozen v1 identity.
    pub fn write_file_causal(
        &mut self,
        id: Hash32,
        offset: u64,
        bytes: &[u8],
        causal: &[Hash32],
    ) -> Result<Hash32> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("write_file requires a file object"));
        }
        if bytes.is_empty() {
            return Ok(old.version);
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
        if end > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let old_manifest = recovery::read_manifest(&self.root, old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        let new_size = end.max(old.size);
        let old_is_codec = old_manifest
            .representation
            .as_ref()
            .map(|rep| rep.kind == RepresentationKind::Codec)
            .unwrap_or(false);
        // CodecAuto, or writing back to a store whose current version is a v2
        // codec representation, materialises the whole object once. This keeps
        // the experimental path simple; the frozen Raw mode still uses the
        // chunk-granular COW path below.
        if self.options.representation == RepresentationMode::CodecAuto || old_is_codec {
            let old_bytes =
                self.read_manifest_bytes(&old_manifest, old.content, 0, old_manifest.size, true)?;
            let offset_usize = checked_usize(offset, "write offset")?;
            let end_usize = checked_usize(end, "write end")?;
            let mut buffer = vec![0u8; checked_usize(new_size, "file size")?];
            buffer[..old_bytes.len()].copy_from_slice(&old_bytes);
            buffer[offset_usize..end_usize].copy_from_slice(bytes);
            // A new explicit causal parent wins; otherwise an existing
            // causal-exact object keeps using its recorded parents as bases.
            let parent_versions: Vec<Hash32> = if causal.is_empty() {
                crate::state::object_causal_closure(&old)?
                    .map(|closure| closure.parents)
                    .unwrap_or_default()
            } else {
                causal.to_vec()
            };
            let base = self.delta_base(&parent_versions)?;
            let (manifest_id, _content, _size) = self.store_content_bytes(
                &buffer,
                self.options.chunk_size,
                base.as_ref().map(|(root, bytes)| (*root, bytes.as_slice())),
                self.options.representation == RepresentationMode::CodecAuto,
            )?;
            return self.publish_file_version(id, old, manifest_id, new_size, causal);
        }
        let (manifest_id, _content, _size) =
            self.store_file_range(&old_manifest, offset, bytes, new_size)?;
        self.publish_file_version(id, old, manifest_id, new_size, causal)
    }

    /// Append a reader to an existing file without buffering the file.
    ///
    /// The file's existing full chunks are reused verbatim; when the current
    /// size is not chunk-aligned, the old tail is combined with the first new
    /// bytes. New bytes are stored one chunk at a time. Exactly one commit is
    /// published with the same version/identity rules as [`Store::write_file`].
    /// A zero-byte append is a POSIX no-op: it returns the current version and
    /// publishes nothing.
    ///
    /// The frozen v1 `ContentId` covers the whole file, so the existing chunks
    /// are re-read to feed the streaming hasher; peak memory is one I/O buffer
    /// plus the chunk list, never the file bytes. Appending to a codec-backed
    /// (`CodecAuto` v2 `kind:"codec"`) object is not implemented and fails
    /// with `InvalidArgument` rather than silently materialising the file.
    pub fn append_file_streaming<R: Read>(&mut self, id: Hash32, mut reader: R) -> Result<Hash32> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "append_file_streaming requires a file object",
            ));
        }
        let old_manifest = recovery::read_manifest(&self.root, old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if old_manifest
            .representation
            .as_ref()
            .map(|rep| rep.kind == RepresentationKind::Codec)
            .unwrap_or(false)
        {
            return Err(Error::invalid_argument(
                "streaming append to a codec-backed object is not implemented",
            ));
        }
        let chunk_size = old_manifest.chunk_size;
        validate_chunk_size(chunk_size)?;
        let step = checked_usize(chunk_size, "chunk size")?;
        let declared = old_manifest
            .chunks
            .iter()
            .try_fold(0u64, |acc, chunk| acc.checked_add(chunk.len))
            .ok_or_else(|| Error::corrupt("manifest size overflows"))?;
        if declared != old_manifest.size {
            return Err(Error::corrupt(
                "manifest chunk lengths disagree with its size",
            ));
        }
        let full_chunks = old.size / chunk_size;
        let tail_len = old.size % chunk_size;
        let expected_chunks = full_chunks + u64::from(tail_len != 0);
        if old_manifest.chunks.len() as u64 != expected_chunks {
            return Err(Error::corrupt(
                "manifest chunk count disagrees with its size",
            ));
        }
        if expected_chunks > MAX_CHUNKS_PER_FILE {
            return Err(Error::limit_exceeded(format!(
                "manifest exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
            )));
        }

        let mut chunks: Vec<ChunkRef> = Vec::new();
        let mut hasher = identity::content_hash_hasher();
        for index in 0..full_chunks {
            let reference = old_manifest
                .chunks
                .get(checked_usize(index, "chunk index")?)
                .ok_or_else(|| Error::corrupt("manifest chunk is missing"))?;
            let bytes = recovery::read_blob_verified(
                &self.root,
                BlobKind::Object,
                reference.hash,
                identity::CHUNK_DOMAIN,
            )?;
            if bytes.len() as u64 != reference.len {
                return Err(Error::corrupt("chunk length disagrees with manifest"));
            }
            hasher.update(&bytes);
            chunks.push(reference.clone());
        }

        let mut pending: Vec<u8> = Vec::new();
        if tail_len != 0 {
            let reference = old_manifest
                .chunks
                .get(checked_usize(full_chunks, "chunk index")?)
                .ok_or_else(|| Error::corrupt("manifest chunk is missing"))?;
            let bytes = recovery::read_blob_verified(
                &self.root,
                BlobKind::Object,
                reference.hash,
                identity::CHUNK_DOMAIN,
            )?;
            if bytes.len() as u64 != reference.len {
                return Err(Error::corrupt("chunk length disagrees with manifest"));
            }
            pending.extend_from_slice(&bytes);
        }

        let io_len = stream_io_len(step)?;
        let mut buffer = vec![0u8; io_len];
        let mut added: u64 = 0;
        loop {
            let filled = read_fill(&mut reader, &mut buffer)?;
            if filled == 0 {
                break;
            }
            added = added
                .checked_add(filled as u64)
                .ok_or_else(|| Error::limit_exceeded("append size overflows"))?;
            let new_size = old
                .size
                .checked_add(added)
                .ok_or_else(|| Error::limit_exceeded("append size overflows"))?;
            if new_size > MAX_FILE_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "file exceeds the {MAX_FILE_BYTES}-byte bound"
                )));
            }
            let mut consumed = 0usize;
            if !pending.is_empty() {
                let take = (step - pending.len()).min(filled);
                pending.extend_from_slice(&buffer[..take]);
                consumed = take;
                if pending.len() == step {
                    hasher.update(&pending);
                    let hash =
                        self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, &pending)?;
                    push_chunk_ref(&mut chunks, hash, pending.len() as u64)?;
                    pending.clear();
                }
            }
            // `consumed` is now chunk-aligned relative to the new data because
            // `pending.len() + consumed` is a multiple of `step`.
            let data = &buffer[consumed..filled];
            let full = data.len() / step * step;
            let mut offset = 0usize;
            while offset < full {
                let part = &data[offset..offset + step];
                hasher.update(part);
                let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, part)?;
                push_chunk_ref(&mut chunks, hash, part.len() as u64)?;
                offset += step;
            }
            if offset < data.len() {
                pending.extend_from_slice(&data[offset..]);
            }
            if filled < io_len {
                break;
            }
        }
        if !pending.is_empty() {
            hasher.update(&pending);
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, &pending)?;
            push_chunk_ref(&mut chunks, hash, pending.len() as u64)?;
        }
        if added == 0 {
            return Ok(old.version);
        }
        let new_size = old.size + added;
        if new_size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let content_hash = identity::finish_content_hash(hasher);
        debug_assert!(chunks.iter().map(|chunk| chunk.len).sum::<u64>() == new_size);
        let representation = if self.options.representation == RepresentationMode::CodecAuto {
            Some(Representation {
                kind: RepresentationKind::Raw,
                frame: None,
                dependencies: Vec::new(),
                strategy: "raw".to_owned(),
                decoded_size: new_size,
            })
        } else {
            None
        };
        let (manifest_id, _content, _size) = self.store_manifest(Manifest {
            size: new_size,
            chunk_size,
            chunks,
            content_hash,
            representation,
        })?;
        self.publish_file_version(id, old, manifest_id, new_size, &[])
    }

    /// Bench/test-only reproduction of the pre-COW write path.
    ///
    /// It reads the whole object, splices the range in memory, and re-stores
    /// every chunk through `Store::store_file_content` (which re-reads each
    /// existing blob to verify it). Exposed under `#[doc(hidden)]` solely so
    /// `examples/extent_cow_bench.rs` can measure the honest baseline in the
    /// same process. Not part of the supported API and not used by the
    /// filesystem.
    #[doc(hidden)]
    pub fn write_file_whole_file_for_bench(
        &mut self,
        id: Hash32,
        offset: u64,
        bytes: &[u8],
    ) -> Result<Hash32> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "write_file_whole_file_for_bench requires a file object",
            ));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
        if end > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let current = self.read_object(id)?;
        if current.len() as u64 != old.size {
            return Err(Error::corrupt("file size changed while writing"));
        }
        let new_size = end.max(old.size);
        let offset_usize = checked_usize(offset, "write offset")?;
        let end_usize = checked_usize(end, "write end")?;
        let mut buffer = vec![0u8; checked_usize(new_size, "file size")?];
        buffer[..current.len()].copy_from_slice(&current);
        buffer[offset_usize..end_usize].copy_from_slice(bytes);
        let (manifest_id, _content, _size) = self.store_file_content(&buffer)?;
        self.publish_file_version(id, old, manifest_id, new_size, &[])
    }

    /// Derive the new version, preserve the identity mode / causal closure, and
    /// publish one atomic commit. Shared by the COW write path and the
    /// whole-file baseline.
    fn publish_file_version(
        &mut self,
        id: Hash32,
        old: ObjectRecord,
        manifest_id: Hash32,
        new_size: u64,
        causal: &[Hash32],
    ) -> Result<Hash32> {
        let (attrs, identity_mode, causal_hash, closure) = if causal.is_empty() {
            let (mode, hash) = crate::state::identity_from_attrs(&old)?;
            (old.attrs.clone(), mode, hash, None)
        } else {
            crate::context::prepare_causal(
                self.current_state(),
                old.attrs.clone(),
                causal,
                &Limits::default(),
            )?
        };
        if let Some(closure) = closure {
            crate::context::store_causal_closure(self, &closure)?;
        }
        let mtime = self.now_ns();
        let version = derive_version_id_with_identity(
            id,
            manifest_id,
            new_size,
            mtime,
            Some(old.version),
            identity_mode,
            causal_hash,
        )?;
        let record = ObjectRecord {
            version,
            mtime_ns: mtime,
            content: manifest_id,
            size: new_size,
            attrs,
            ..old
        };
        let ops = vec![
            Op::PutObject(record),
            Op::SetTip {
                object: id,
                version,
            },
        ];
        self.commit_ops(ops, false)?;
        Ok(version)
    }

    /// Write zeros into `[offset, offset + length)`. When `extend` is true the
    /// file grows to cover the range; otherwise bytes past EOF are ignored.
    /// v1 files are dense, so this defines content but does not reclaim
    /// physical chunks.
    pub fn zero_range(
        &mut self,
        id: Hash32,
        offset: u64,
        length: u64,
        extend: bool,
    ) -> Result<Hash32> {
        self.require_writable()?;
        if length == 0 {
            return Err(Error::invalid_argument("zero range length must be nonzero"));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::limit_exceeded("zero range overflows"))?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("zero range requires a file object"));
        }
        if extend && end > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let new_size = if extend { old.size.max(end) } else { old.size };
        let zero_end = end.min(new_size);
        if zero_end <= offset {
            return Ok(old.version);
        }
        let current = self.read_object(id)?;
        if current.len() as u64 != old.size {
            return Err(Error::corrupt("file size changed while zeroing"));
        }
        let offset_usize = checked_usize(offset, "zero range offset")?;
        let zero_end_usize = checked_usize(zero_end, "zero range end")?;
        let mut buffer = vec![0u8; checked_usize(new_size, "file size")?];
        buffer[..current.len()].copy_from_slice(&current);
        buffer[offset_usize..zero_end_usize].fill(0);
        self.commit_file_buffer(id, old, &buffer)
    }

    /// Remove `[offset, offset + length)` and shift later bytes left.
    pub fn collapse_range(&mut self, id: Hash32, offset: u64, length: u64) -> Result<Hash32> {
        self.require_writable()?;
        if length == 0 {
            return Err(Error::invalid_argument("collapse length must be nonzero"));
        }
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("collapse requires a file object"));
        }
        if offset > old.size || length > old.size - offset {
            return Err(Error::invalid_argument("collapse range lies past EOF"));
        }
        let current = self.read_object(id)?;
        if current.len() as u64 != old.size {
            return Err(Error::corrupt("file size changed while collapsing"));
        }
        let offset_usize = checked_usize(offset, "collapse offset")?;
        let resume_usize = checked_usize(offset + length, "collapse resume")?;
        let mut buffer = Vec::with_capacity(checked_usize(old.size - length, "collapsed size")?);
        buffer.extend_from_slice(&current[..offset_usize]);
        buffer.extend_from_slice(&current[resume_usize..]);
        self.commit_file_buffer(id, old, &buffer)
    }

    /// Insert `length` zero bytes at `offset`, shifting later bytes right.
    pub fn insert_range(&mut self, id: Hash32, offset: u64, length: u64) -> Result<Hash32> {
        self.require_writable()?;
        if length == 0 {
            return Err(Error::invalid_argument("insert length must be nonzero"));
        }
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("insert requires a file object"));
        }
        if offset > old.size {
            return Err(Error::invalid_argument("insert offset lies past EOF"));
        }
        let new_size = old
            .size
            .checked_add(length)
            .ok_or_else(|| Error::limit_exceeded("file size overflows"))?;
        if new_size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let current = self.read_object(id)?;
        if current.len() as u64 != old.size {
            return Err(Error::corrupt("file size changed while inserting"));
        }
        let offset_usize = checked_usize(offset, "insert offset")?;
        let resume_usize = checked_usize(offset + length, "insert resume")?;
        let mut buffer = vec![0u8; checked_usize(new_size, "file size")?];
        buffer[..offset_usize].copy_from_slice(&current[..offset_usize]);
        buffer[resume_usize..].copy_from_slice(&current[offset_usize..]);
        self.commit_file_buffer(id, old, &buffer)
    }

    /// Publish a new file version from an in-memory buffer, preserving the
    /// existing attributes and identity mode (as `truncate` and `write_file` do).
    fn commit_file_buffer(
        &mut self,
        id: Hash32,
        old: ObjectRecord,
        buffer: &[u8],
    ) -> Result<Hash32> {
        let (manifest_id, _content, _size) = self.store_file_content(buffer)?;
        let (identity_mode, causal_hash) = crate::state::identity_from_attrs(&old)?;
        let mtime = self.now_ns();
        let size = buffer.len() as u64;
        let version = derive_version_id_with_identity(
            id,
            manifest_id,
            size,
            mtime,
            Some(old.version),
            identity_mode,
            causal_hash,
        )?;
        let record = ObjectRecord {
            version,
            mtime_ns: mtime,
            content: manifest_id,
            size,
            ..old
        };
        let ops = vec![
            Op::PutObject(record),
            Op::SetTip {
                object: id,
                version,
            },
        ];
        self.commit_ops(ops, false)?;
        Ok(version)
    }

    /// Resize a file to `size`, zero-filling when growing.
    pub fn truncate(&mut self, id: Hash32, size: u64) -> Result<Hash32> {
        self.require_writable()?;
        if size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("truncate requires a file object"));
        }
        let current = self.read_object(id)?;
        let mut buffer = vec![0u8; checked_usize(size, "file size")?];
        let copy = current.len().min(buffer.len());
        buffer[..copy].copy_from_slice(&current[..copy]);
        let (manifest_id, _content, _size) = self.store_file_content(&buffer)?;
        let (identity_mode, causal_hash) = crate::state::identity_from_attrs(&old)?;
        let mtime = self.now_ns();
        let version = derive_version_id_with_identity(
            id,
            manifest_id,
            size,
            mtime,
            Some(old.version),
            identity_mode,
            causal_hash,
        )?;
        let record = ObjectRecord {
            version,
            mtime_ns: mtime,
            content: manifest_id,
            size,
            ..old
        };
        let ops = vec![
            Op::PutObject(record),
            Op::SetTip {
                object: id,
                version,
            },
        ];
        self.commit_ops(ops, false)?;
        Ok(version)
    }

    /// Remove a directory entry. When the last entry for an object disappears,
    /// the object record is deleted in the same commit; snapshot states retain
    /// their own copy for replay. A pinned object (open handle) keeps its record
    /// until the last pin is released so the handle stays readable/writable.
    pub fn unlink(&mut self, parent: Hash32, name: &[u8]) -> Result<()> {
        self.require_writable()?;
        self.require_dir(parent)?;
        let target = self.state.lookup(parent, name)?;
        let object = self.object_meta(target)?;
        if object.kind == ObjectKind::Dir {
            return Err(Error::is_dir("unlink does not remove directories"));
        }
        let entry_id = derive_entry_id(parent, name)?;
        let remaining = self
            .state
            .entries
            .values()
            .filter(|entry| entry.target == target && entry.id != entry_id)
            .count();
        let mut ops = vec![Op::DelEntry(entry_id)];
        if remaining == 0 && !self.is_object_pinned(target) {
            ops.push(Op::DeleteObject(target));
        }
        self.commit_ops(ops, false)?;
        Ok(())
    }

    /// Remove an empty directory. A directory with any child entry is
    /// `NotEmpty` (POSIX `ENOTEMPTY`). A pinned directory object (an open
    /// `opendir` handle) keeps its object record until the pin is released.
    /// The removed object's manifest/chunks are reclaimed by GC, not here.
    pub fn rmdir(&mut self, parent: Hash32, name: &[u8]) -> Result<()> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        let target = self.state.lookup(parent, name)?;
        if target == State::ROOT_ID {
            return Err(Error::invalid_argument("cannot remove the namespace root"));
        }
        let object = self.object_meta(target)?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("rmdir requires a directory entry"));
        }
        if self
            .state
            .entries
            .values()
            .any(|entry| entry.parent == target)
        {
            return Err(Error::not_empty("directory is not empty"));
        }
        let entry_id = derive_entry_id(parent, name)?;
        let remaining = self
            .state
            .entries
            .values()
            .filter(|entry| entry.target == target && entry.id != entry_id)
            .count();
        let mut ops = vec![Op::DelEntry(entry_id)];
        if remaining == 0 && !self.is_object_pinned(target) {
            ops.push(Op::DeleteObject(target));
        }
        self.commit_ops(ops, false)?;
        Ok(())
    }

    /// Atomically move (and optionally replace) a directory entry.
    ///
    /// POSIX replacement rules: a directory may only replace an empty
    /// directory, and a non-directory may only replace a non-directory. The
    /// replaced object keeps its ObjectId until the replacement commit deletes
    /// its last entry. All the delete/insert ops plus object reclamation are in
    /// one commit.
    pub fn rename(
        &mut self,
        old_parent: Hash32,
        old_name: &[u8],
        new_parent: Hash32,
        new_name: &[u8],
    ) -> Result<()> {
        self.rename_with(old_parent, old_name, new_parent, new_name, false)
    }

    /// [`Store::rename`] with `RENAME_NOREPLACE` semantics: replacing an
    /// existing destination is `Exists`.
    pub fn rename_no_replace(
        &mut self,
        old_parent: Hash32,
        old_name: &[u8],
        new_parent: Hash32,
        new_name: &[u8],
    ) -> Result<()> {
        self.rename_with(old_parent, old_name, new_parent, new_name, true)
    }

    /// Shared rename implementation. `no_replace` rejects an existing
    /// destination instead of replacing it.
    pub fn rename_with(
        &mut self,
        old_parent: Hash32,
        old_name: &[u8],
        new_parent: Hash32,
        new_name: &[u8],
        no_replace: bool,
    ) -> Result<()> {
        self.require_writable()?;
        self.require_dir(old_parent)?;
        self.require_dir(new_parent)?;
        validate_name_bytes(old_name)?;
        validate_name_bytes(new_name)?;
        let target = self.state.lookup(old_parent, old_name)?;
        if old_parent == new_parent && old_name == new_name {
            return Ok(());
        }
        let target_kind = self.object_meta(target)?.kind;
        let old_entry_id = derive_entry_id(old_parent, old_name)?;
        let new_entry_id = derive_entry_id(new_parent, new_name)?;
        let existing = self.state.entries.get(&new_entry_id).cloned();
        let mut ops = Vec::new();
        if let Some(existing) = &existing {
            if existing.target == target {
                // POSIX rename between hard links to the same inode is a no-op.
                return Ok(());
            }
            if no_replace {
                return Err(Error::exists("rename destination already exists"));
            }
            let dest = self.object_meta(existing.target)?;
            match (target_kind, dest.kind) {
                (ObjectKind::Dir, ObjectKind::Dir) => {
                    if self
                        .state
                        .entries
                        .values()
                        .any(|entry| entry.parent == dest.id)
                    {
                        return Err(Error::not_empty(
                            "rename would replace a non-empty directory",
                        ));
                    }
                }
                (ObjectKind::Dir, _) => {
                    return Err(Error::not_dir(
                        "rename would replace a non-directory with a directory",
                    ));
                }
                (_, ObjectKind::Dir) => {
                    return Err(Error::is_dir(
                        "rename would replace a directory with a non-directory",
                    ));
                }
                (_, _) => {}
            }
            ops.push(Op::DelEntry(existing.id));
            let remaining = self
                .state
                .entries
                .values()
                .filter(|entry| entry.target == existing.target && entry.id != existing.id)
                .count();
            if remaining == 0 && !self.is_object_pinned(existing.target) {
                ops.push(Op::DeleteObject(existing.target));
            }
        }
        if target_kind == ObjectKind::Dir && self.state.is_within(target, new_parent) {
            return Err(Error::invalid_argument(
                "cannot move a directory into itself or a descendant",
            ));
        }
        ops.push(Op::DelEntry(old_entry_id));
        ops.push(Op::PutEntry(EntryRecord {
            id: new_entry_id,
            parent: new_parent,
            name: new_name.to_vec(),
            target,
        }));
        self.commit_ops(ops, false)?;
        Ok(())
    }

    /// Atomically exchange two existing directory entries (`RENAME_EXCHANGE`).
    ///
    /// Directories are rejected in v1: exchanging a directory with a node in
    /// its own subtree can create a parent cycle, and v1 does not implement the
    /// extra cycle validation for exchanges. The rejection is explicit
    /// (`InvalidArgument`, POSIX `EINVAL`), never a silent swap.
    pub fn rename_exchange(
        &mut self,
        a_parent: Hash32,
        a_name: &[u8],
        b_parent: Hash32,
        b_name: &[u8],
    ) -> Result<()> {
        self.require_writable()?;
        self.require_dir(a_parent)?;
        self.require_dir(b_parent)?;
        validate_name_bytes(a_name)?;
        validate_name_bytes(b_name)?;
        let a_entry = self
            .state
            .entries
            .get(&derive_entry_id(a_parent, a_name)?)
            .cloned();
        let a_entry =
            a_entry.ok_or_else(|| Error::not_found("rename exchange source does not exist"))?;
        let b_entry = self
            .state
            .entries
            .get(&derive_entry_id(b_parent, b_name)?)
            .cloned();
        let b_entry =
            b_entry.ok_or_else(|| Error::not_found("rename exchange target does not exist"))?;
        if a_entry.id == b_entry.id {
            return Ok(());
        }
        if self.object_meta(a_entry.target)?.kind == ObjectKind::Dir
            || self.object_meta(b_entry.target)?.kind == ObjectKind::Dir
        {
            return Err(Error::invalid_argument(
                "RENAME_EXCHANGE of directories is not supported in v1",
            ));
        }
        let ops = vec![
            Op::DelEntry(a_entry.id),
            Op::DelEntry(b_entry.id),
            Op::PutEntry(EntryRecord {
                id: a_entry.id,
                parent: a_parent,
                name: a_name.to_vec(),
                target: b_entry.target,
            }),
            Op::PutEntry(EntryRecord {
                id: b_entry.id,
                parent: b_parent,
                name: b_name.to_vec(),
                target: a_entry.target,
            }),
        ];
        self.commit_ops(ops, false)?;
        Ok(())
    }

    /// Create a hard link to an existing non-directory object.
    pub fn link(&mut self, target: Hash32, parent: Hash32, name: &[u8]) -> Result<()> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        let object = self.object_meta(target)?;
        if object.kind == ObjectKind::Dir {
            return Err(Error::invalid_argument(
                "hard links to directories are not allowed",
            ));
        }
        if self
            .state
            .entries
            .contains_key(&derive_entry_id(parent, name)?)
        {
            return Err(Error::exists("directory entry already exists"));
        }
        let entry = EntryRecord {
            id: derive_entry_id(parent, name)?,
            parent,
            name: name.to_vec(),
            target,
        };
        self.commit_ops(vec![Op::PutEntry(entry)], false)?;
        Ok(())
    }

    /// Create a directory.
    pub fn mkdir(
        &mut self,
        parent: Hash32,
        name: &[u8],
        mode: u64,
        uid: u64,
        gid: u64,
    ) -> Result<Hash32> {
        let seed = identity::os_random_32()?;
        self.mkdir_seeded(parent, name, mode, uid, gid, &seed)
    }

    /// Create a directory with an explicit ObjectId seed.
    pub fn mkdir_seeded(
        &mut self,
        parent: Hash32,
        name: &[u8],
        mode: u64,
        uid: u64,
        gid: u64,
        seed: &[u8],
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        check_mode(mode)?;
        let object_id = identity::object_id_from_seed(seed);
        if self.state.objects.contains_key(&object_id) {
            return Err(Error::exists("object id already exists"));
        }
        let mtime = self.now_ns();
        let content = identity::content_hash(b"");
        let version = derive_version_id(object_id, content, 0, mtime, None)?;
        let record = ObjectRecord {
            id: object_id,
            kind: ObjectKind::Dir,
            mode,
            uid,
            gid,
            mtime_ns: mtime,
            version,
            content,
            size: 0,
            attrs: BTreeMap::new(),
        };
        let entry = EntryRecord {
            id: derive_entry_id(parent, name)?,
            parent,
            name: name.to_vec(),
            target: object_id,
        };
        self.commit_ops(
            vec![
                Op::PutObject(record),
                Op::PutEntry(entry),
                Op::SetTip {
                    object: object_id,
                    version,
                },
            ],
            false,
        )?;
        Ok(object_id)
    }

    /// Create a symlink whose target is stored as bytes.
    pub fn symlink(
        &mut self,
        parent: Hash32,
        name: &[u8],
        target_bytes: &[u8],
        uid: u64,
        gid: u64,
    ) -> Result<Hash32> {
        let seed = identity::os_random_32()?;
        self.symlink_seeded(parent, name, target_bytes, uid, gid, &seed)
    }

    /// Create a symlink with an explicit ObjectId seed.
    pub fn symlink_seeded(
        &mut self,
        parent: Hash32,
        name: &[u8],
        target_bytes: &[u8],
        uid: u64,
        gid: u64,
        seed: &[u8],
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        let object_id = identity::object_id_from_seed(seed);
        if self.state.objects.contains_key(&object_id) {
            return Err(Error::exists("object id already exists"));
        }
        let mtime = self.now_ns();
        let content = identity::content_hash(target_bytes);
        let size = target_bytes.len() as u64;
        let version = derive_version_id(object_id, content, size, mtime, None)?;
        let mut attrs = BTreeMap::new();
        attrs.insert(
            ATTR_SYMLINK_TARGET.to_owned(),
            Value::Bytes(target_bytes.to_vec()),
        );
        let record = ObjectRecord {
            id: object_id,
            kind: ObjectKind::Symlink,
            mode: 0o777,
            uid,
            gid,
            mtime_ns: mtime,
            version,
            content,
            size,
            attrs,
        };
        let entry = EntryRecord {
            id: derive_entry_id(parent, name)?,
            parent,
            name: name.to_vec(),
            target: object_id,
        };
        self.commit_ops(
            vec![
                Op::PutObject(record),
                Op::PutEntry(entry),
                Op::SetTip {
                    object: object_id,
                    version,
                },
            ],
            false,
        )?;
        Ok(object_id)
    }

    /// Update mode, ownership, mtime, and/or attributes in one commit.
    pub fn setattr(&mut self, id: Hash32, changes: SetAttr) -> Result<Hash32> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if let Some(mode) = changes.mode {
            check_mode(mode)?;
        }
        let mut record = old.clone();
        if let Some(mode) = changes.mode {
            record.mode = mode;
        }
        if let Some(uid) = changes.uid {
            record.uid = uid;
        }
        if let Some(gid) = changes.gid {
            record.gid = gid;
        }
        if let Some(mtime) = changes.mtime_ns {
            record.mtime_ns = mtime;
        }
        if let Some(attrs) = changes.attrs {
            record.attrs = attrs;
        }
        let (identity_mode, causal_hash) = crate::state::identity_from_attrs(&record)?;
        record.version = derive_version_id_with_identity(
            record.id,
            record.content,
            record.size,
            record.mtime_ns,
            Some(old.version),
            identity_mode,
            causal_hash,
        )?;
        let ops = vec![
            Op::PutObject(record.clone()),
            Op::SetTip {
                object: id,
                version: record.version,
            },
        ];
        self.commit_ops(ops, false)?;
        Ok(record.version)
    }

    /// Set (or replace) an extended attribute. Stored as
    /// `attrs["xattr.<name>"] = bytes` per `WEFT-MODEL.md` §2.5. Reserved
    /// `weft.*` names cannot be written through this API.
    pub fn set_xattr(&mut self, id: Hash32, name: &str, value: &[u8]) -> Result<()> {
        validate_xattr_name(name)?;
        if value.len() as u64 > MAX_XATTR_VALUE_BYTES {
            return Err(Error::limit_exceeded(
                "extended attribute value exceeds the 64 KiB bound",
            ));
        }
        let record = self.object_meta(id)?;
        let key = format!("{XATTR_PREFIX}{name}");
        let mut attrs = record.attrs.clone();
        if !attrs.contains_key(&key)
            && attrs.keys().filter(|k| k.starts_with(XATTR_PREFIX)).count() >= MAX_XATTR_COUNT
        {
            return Err(Error::limit_exceeded(
                "object has reached the extended attribute count bound",
            ));
        }
        attrs.insert(key, Value::Bytes(value.to_vec()));
        self.setattr(
            id,
            SetAttr {
                attrs: Some(attrs),
                ..SetAttr::default()
            },
        )?;
        Ok(())
    }

    /// Read an extended attribute. `Ok(None)` means absent.
    pub fn get_xattr(&self, id: Hash32, name: &str) -> Result<Option<Vec<u8>>> {
        validate_xattr_name(name)?;
        let record = self.object_meta(id)?;
        match record.attrs.get(&format!("{XATTR_PREFIX}{name}")) {
            Some(Value::Bytes(bytes)) => Ok(Some(bytes.clone())),
            Some(_) => Err(Error::corrupt("extended attribute is not stored as bytes")),
            None => Ok(None),
        }
    }

    /// List extended attribute names (the suffixes, sorted). Reserved `weft.*`
    /// keys are never exposed.
    pub fn list_xattr(&self, id: Hash32) -> Result<Vec<String>> {
        let record = self.object_meta(id)?;
        let mut names = Vec::new();
        for (key, value) in &record.attrs {
            let Some(name) = key.strip_prefix(XATTR_PREFIX) else {
                continue;
            };
            if name.starts_with("weft.") {
                continue;
            }
            if !matches!(value, Value::Bytes(_)) {
                return Err(Error::corrupt("extended attribute is not stored as bytes"));
            }
            names.push(name.to_owned());
        }
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        Ok(names)
    }

    /// Remove an extended attribute; returns whether it existed.
    pub fn remove_xattr(&mut self, id: Hash32, name: &str) -> Result<bool> {
        validate_xattr_name(name)?;
        let record = self.object_meta(id)?;
        let key = format!("{XATTR_PREFIX}{name}");
        if !record.attrs.contains_key(&key) {
            return Ok(false);
        }
        let mut attrs = record.attrs.clone();
        attrs.remove(&key);
        self.setattr(
            id,
            SetAttr {
                attrs: Some(attrs),
                ..SetAttr::default()
            },
        )?;
        Ok(true)
    }

    // ---- saved contexts --------------------------------------------------

    /// Save (or replace) a named selector context as a root-subject
    /// `saved_context` relation. Replacement is delete+add in one commit.
    /// Returns the relation id.
    pub fn save_context(&mut self, name: &str, selector: &Selector) -> Result<Hash32> {
        self.require_writable()?;
        validate_snapshot_name(name)?;
        let limits = Limits::default();
        let bytes = selector.canonical_bytes_with_limits(&limits)?;
        let selector_hex = hex::encode(&bytes);
        let mut attrs = BTreeMap::new();
        attrs.insert(
            SAVED_CONTEXT_SELECTOR_ATTR.to_owned(),
            Value::Str(selector_hex),
        );
        let record = RelationRecord::new(
            State::ROOT_ID,
            SAVED_CONTEXT_PREDICATE,
            RelationTarget::Value(Value::Str(name.to_owned())),
            None,
            attrs,
        )?;
        let existing = self.context_relations(name)?;
        match existing.as_slice() {
            [] => {
                self.commit_ops(vec![Op::PutRelation(record.clone())], false)?;
                Ok(record.id)
            }
            [current] => {
                if current.id == record.id {
                    return Ok(record.id);
                }
                self.commit_ops(
                    vec![Op::DelRelation(current.id), Op::PutRelation(record.clone())],
                    false,
                )?;
                Ok(record.id)
            }
            _ => Err(Error::corrupt(
                "more than one saved context relation has the same name",
            )),
        }
    }

    /// Delete a saved context by name.
    pub fn delete_context(&mut self, name: &str) -> Result<()> {
        self.require_writable()?;
        let existing = self.context_relations(name)?;
        match existing.as_slice() {
            [] => Err(Error::not_found("no such saved context")),
            [current] => {
                self.commit_ops(vec![Op::DelRelation(current.id)], false)?;
                Ok(())
            }
            _ => Err(Error::corrupt(
                "more than one saved context relation has the same name",
            )),
        }
    }

    /// All saved contexts, sorted by name. Fails closed on a malformed entry.
    pub fn list_contexts(&self) -> Result<Vec<SavedContext>> {
        self.list_contexts_at(&self.state)
    }

    /// Saved contexts in an explicit state (a snapshot mount reads a pinned
    /// state, not the live one).
    pub fn list_contexts_at(&self, state: &State) -> Result<Vec<SavedContext>> {
        let mut out = Vec::new();
        for relation in state.relations.values() {
            if !is_saved_context(relation) {
                continue;
            }
            let RelationTarget::Value(Value::Str(name)) = &relation.target else {
                return Err(Error::corrupt("saved_context target is not text"));
            };
            out.push(Self::context_from_relation(name, relation)?);
        }
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Ok(out)
    }

    /// Load a saved context selector by name.
    pub fn load_context(&self, name: &str) -> Result<Selector> {
        self.load_context_at(&self.state, name)
    }

    /// Load a saved context selector from an explicit state.
    pub fn load_context_at(&self, state: &State, name: &str) -> Result<Selector> {
        let existing = Self::context_relations_at(state, name)?;
        let relation = match existing.as_slice() {
            [relation] => relation,
            [] => return Err(Error::not_found("no such saved context")),
            _ => {
                return Err(Error::corrupt(
                    "more than one saved context relation has the same name",
                ))
            }
        };
        let saved = Self::context_from_relation(name, relation)?;
        let bytes = hex::decode(&saved.selector_hex)
            .map_err(|_| Error::corrupt("saved context selector is not hex"))?;
        let selector = Selector::from_canonical_bytes(&bytes, &Limits::default())?;
        if selector.canonical_bytes_with_limits(&Limits::default())? != bytes {
            return Err(Error::non_canonical(
                "saved context selector bytes are not canonical",
            ));
        }
        Ok(selector)
    }

    fn context_relations(&self, name: &str) -> Result<Vec<&RelationRecord>> {
        Self::context_relations_at(&self.state, name)
    }

    fn context_relations_at<'a>(state: &'a State, name: &str) -> Result<Vec<&'a RelationRecord>> {
        let mut out = Vec::new();
        for relation in state.relations.values() {
            if !is_saved_context(relation) {
                continue;
            }
            if matches!(&relation.target, RelationTarget::Value(Value::Str(text)) if text == name) {
                out.push(relation);
            }
        }
        if out.len() > 1 {
            return Err(Error::corrupt(
                "more than one saved context relation has the same name",
            ));
        }
        Ok(out)
    }

    fn context_from_relation(name: &str, relation: &RelationRecord) -> Result<SavedContext> {
        let selector_hex = match relation.attrs.get(SAVED_CONTEXT_SELECTOR_ATTR) {
            Some(Value::Str(text)) => text.clone(),
            _ => {
                return Err(Error::corrupt(
                    "saved context relation has no selector_hex attribute",
                ))
            }
        };
        let bytes = hex::decode(&selector_hex)
            .map_err(|_| Error::corrupt("saved context selector is not hex"))?;
        let selector = Selector::from_canonical_bytes(&bytes, &Limits::default())?;
        Ok(SavedContext {
            name: name.to_owned(),
            selector_hex,
            selector_id: selector.selector_id_with_limits(&Limits::default())?,
            relation_id: relation.id,
        })
    }

    // ---- open-handle pins ------------------------------------------------

    /// Pin an object (open handle). A pinned object's record survives `unlink`
    /// and `rmdir` until the last pin is released, so an open-unlinked file
    /// stays readable and writable. Pins are in-memory and process-local; a
    /// crash loses them and the commit chain retains an unreferenced object
    /// record until GC.
    pub fn pin_object(&mut self, id: Hash32) -> Result<u64> {
        self.object_meta(id)?;
        let count = self.pins.entry(id).or_insert(0);
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("pin count overflow"))?;
        Ok(*count)
    }

    /// Release an object pin. When the count reaches zero and the object has no
    /// remaining directory entry, the object record is deleted in one commit
    /// (a durable cleanup; it is not a GC sweep).
    pub fn unpin_object(&mut self, id: Hash32) -> Result<u64> {
        let count = match self.pins.get_mut(&id) {
            Some(count) => {
                *count = count.saturating_sub(1);
                let remaining = *count;
                if remaining == 0 {
                    self.pins.remove(&id);
                }
                remaining
            }
            None => 0,
        };
        if count == 0 && self.mode == OpenMode::ReadWrite {
            let has_entry = self.state.entries.values().any(|entry| entry.target == id);
            let has_children = self.state.entries.values().any(|entry| entry.parent == id);
            let exists = self.state.objects.contains_key(&id);
            if exists && !has_entry && !has_children && id != State::ROOT_ID {
                self.commit_ops(vec![Op::DeleteObject(id)], false)?;
            }
        }
        Ok(count)
    }

    /// Current pin count for an object.
    pub fn object_pin_count(&self, id: Hash32) -> u64 {
        self.pins.get(&id).copied().unwrap_or(0)
    }

    /// Objects pinned in this process, sorted by id.
    pub fn pinned_objects(&self) -> Vec<(Hash32, u64)> {
        self.pins.iter().map(|(id, count)| (*id, *count)).collect()
    }

    /// Pin a content root (manifest id) for readers that hold a
    /// `(content, size)` pair. GC never sweeps a pinned manifest or its chunks.
    pub fn pin_content(&mut self, content: Hash32) -> u64 {
        let count = self.content_pins.entry(content).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    /// Release a content pin.
    pub fn unpin_content(&mut self, content: Hash32) -> u64 {
        match self.content_pins.get_mut(&content) {
            Some(count) => {
                *count = count.saturating_sub(1);
                let remaining = *count;
                if remaining == 0 {
                    self.content_pins.remove(&content);
                }
                remaining
            }
            None => 0,
        }
    }

    /// Content roots pinned in this process, sorted by id.
    pub fn pinned_contents(&self) -> Vec<(Hash32, u64)> {
        self.content_pins
            .iter()
            .map(|(id, count)| (*id, *count))
            .collect()
    }

    fn is_object_pinned(&self, id: Hash32) -> bool {
        self.pins.get(&id).copied().unwrap_or(0) > 0
    }

    // ---- counters --------------------------------------------------------

    /// POSIX link count for an object: entries naming a non-directory, and
    /// `2 + subdirectories` for a directory.
    pub fn nlink(&self, id: Hash32) -> Result<u64> {
        let record = self.object_meta(id)?;
        if record.kind == ObjectKind::Dir {
            let subdirs = self
                .state
                .entries
                .values()
                .filter(|entry| entry.parent == id)
                .filter(|entry| {
                    self.state
                        .objects
                        .get(&entry.target)
                        .map(|object| object.kind == ObjectKind::Dir)
                        .unwrap_or(false)
                })
                .count();
            Ok(2 + subdirs as u64)
        } else {
            Ok(self
                .state
                .entries
                .values()
                .filter(|entry| entry.target == id)
                .count() as u64)
        }
    }

    /// Counters derived from replayed state. The store has no independent
    /// capacity (it grows with its host filesystem), so free counters are `0`
    /// and `blocks` is the state's used size. Documented in `docs/fuse-status.md`.
    pub fn statfs(&self) -> StatFs {
        Self::statfs_at(&self.state)
    }

    /// [`Store::statfs`] for an explicit state (a snapshot mount).
    pub fn statfs_at(state: &State) -> StatFs {
        const BLOCK: u64 = 4_096;
        let mut content_bytes: u64 = 0;
        for record in state.objects.values() {
            content_bytes = content_bytes.saturating_add(record.size);
        }
        // One metadata block per object plus the content blocks.
        let blocks = content_bytes.saturating_add(BLOCK - 1) / BLOCK + state.objects.len() as u64;
        StatFs {
            block_size: BLOCK,
            blocks,
            blocks_free: 0,
            blocks_available: 0,
            files: state.objects.len() as u64,
            files_free: 0,
            max_name_len: crate::state::MAX_NAME_BYTES as u64,
        }
    }

    /// The state pinned by this mount (current state for a live store).
    pub fn mounted_state(&self) -> &State {
        &self.state
    }

    /// Add a relation. Idempotent by RelationId.
    pub fn relation_add(
        &mut self,
        subject: Hash32,
        predicate: impl Into<String>,
        target: RelationTarget,
        provenance: Option<BTreeMap<String, Value>>,
        attrs: BTreeMap<String, Value>,
    ) -> Result<Hash32> {
        self.require_writable()?;
        if !self.state.objects.contains_key(&subject) {
            return Err(Error::not_found("relation subject object does not exist"));
        }
        let predicate = predicate.into();
        if predicate.is_empty() {
            return Err(Error::invalid_argument(
                "relation predicate must not be empty",
            ));
        }
        let record = RelationRecord::new(subject, predicate, target, provenance, attrs)?;
        if self.state.relations.contains_key(&record.id) {
            return Ok(record.id);
        }
        self.commit_ops(vec![Op::PutRelation(record.clone())], false)?;
        Ok(record.id)
    }

    /// Delete a relation by id.
    pub fn relation_delete(&mut self, relation_id: Hash32) -> Result<()> {
        self.require_writable()?;
        if !self.state.relations.contains_key(&relation_id) {
            return Err(Error::not_found("no such relation"));
        }
        self.commit_ops(vec![Op::DelRelation(relation_id)], false)?;
        Ok(())
    }

    /// Snapshot the current commit under `name`. Returns the pinned commit hash
    /// (the v1 `SnapshotId`; see docs/store-status.md for the interpretation).
    pub fn snapshot_create(&mut self, name: &str) -> Result<Hash32> {
        self.require_writable()?;
        validate_snapshot_name(name)?;
        if self.state.snapshots.contains_key(name) {
            return Err(Error::exists("snapshot name already exists"));
        }
        let pinned = self.head;
        self.commit_ops(
            vec![Op::SetSnapshot {
                name: name.to_owned(),
                commit: pinned,
            }],
            false,
        )?;
        self.write_snapshot_pointer(name, pinned)?;
        Ok(pinned)
    }

    /// Snapshot names and pinned commit hashes.
    pub fn snapshot_list(&self) -> Vec<(String, Hash32)> {
        self.state
            .snapshots
            .iter()
            .map(|(name, hash)| (name.clone(), *hash))
            .collect()
    }

    /// Pin an object at the current or a named commit.
    pub fn pin(
        &self,
        object: Hash32,
        snapshot: Option<Hash32>,
    ) -> Result<crate::evaluate::ResolvedRef> {
        let pin = snapshot.unwrap_or(self.head);
        let state = self.state_at(pin)?;
        let record = state
            .objects
            .get(&object)
            .ok_or_else(|| Error::not_found("no such object at this snapshot"))?;
        Ok(crate::evaluate::ResolvedRef {
            namespace: crate::state::DEFAULT_NAMESPACE.to_owned(),
            snapshot: pin,
            object_id: record.id,
            version_id: record.version,
            content_id: record.content,
        })
    }

    /// Force a checkpoint commit (state manifest describing the state after
    /// this commit's empty op list).
    ///
    /// Inside [`Store::group`] this only marks the group commit for a
    /// checkpoint state manifest and returns the current (pre-group) head;
    /// nothing is published until the group closes.
    pub fn checkpoint(&mut self) -> Result<Hash32> {
        self.require_writable()?;
        self.commit_ops(Vec::new(), true)
    }

    /// Group commit: run `f` with this store and publish **one** durable commit
    /// containing every mutation it performed, in application order.
    ///
    /// # Contract
    ///
    /// * Inside `f`, every mutation method (`create_file*`, `mkdir*`,
    ///   `symlink*`, `write_file*`, `truncate`, `zero_range`, `collapse_range`,
    ///   `insert_range`, `unlink`, `rmdir`, `rename*`, `link`,
    ///   `setattr`, `set_xattr`/`remove_xattr`,
    ///   `relation_add`/`relation_delete`, `save_context*`,
    ///   `save_context_root`, `snapshot_create`, …) works
    ///   unchanged: validation, name checks, `MAX_FILE_BYTES`, chunk-size
    ///   checks and causal handling are exactly the single-mutation ones.
    /// * Reads inside `f` (including `read_range`, `read_object`, `list_dir`,
    ///   `object_meta`, selector queries and `state_at(head)`) see the mutations
    ///   already made, because each buffered op is applied to the live
    ///   in-memory state immediately.
    ///   Version ids, `mtime_ns` and the fixed fixture clock use the existing
    ///   helpers and do not change.
    /// * A mutation that fails inside `f` fails exactly as it would on its own
    ///   and leaves no partial op batch applied.
    /// * On `Ok`, the accumulated ops become the content of exactly one commit;
    ///   the generation increases by exactly 1 and the commit hash is returned
    ///   with the closure value. Buffered per-mutation commit hashes are not
    ///   meaningful (the helpers return the current head); the group hash is.
    /// * On `Err`, **nothing** is published: the commit-chain generation, the
    ///   head, and the in-memory state (objects, entries, relations, snapshots,
    ///   tips) are byte-identical to the state before `group`. Content chunks,
    ///   manifests and causal blobs written before the failure may remain as
    ///   unreferenced blobs; `verify` still passes and `gc` reclaims them.
    /// * A group with no buffered ops publishes nothing and returns the current
    ///   head unchanged.
    /// * Nested groups are `InvalidArgument`, never flattened into the outer
    ///   group.
    /// * `OpenMode::ReadOnly` stores refuse `group` with `ReadOnly`.
    /// * `gc` (the only operation that physically deletes blobs) is
    ///   `InvalidArgument` inside a group, because a later rollback would leave
    ///   state referencing deleted blobs.
    ///
    /// # Durability
    ///
    /// A group is durable only when this call returns `Ok`: the single commit
    /// is written and superblock-published at the end of `f`, so a crash at any
    /// point inside the closure leaves the previous generation exactly as it
    /// was before the group — the same rule the single-mutation path has. A
    /// crash during publication is the ordinary commit crash set: either the
    /// old generation or the whole new generation, never a partial one.
    /// Snapshots taken inside a group pin the generation that was head when the
    /// group started (a commit cannot pin itself without a cycle).
    ///
    /// # Panics
    ///
    /// A panic inside `f` is propagated after the in-memory state is rolled back
    /// to the pre-group value; nothing is published.
    pub fn group<F, T>(&mut self, f: F) -> Result<(T, Hash32)>
    where
        F: FnOnce(&mut Store) -> Result<T>,
    {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "group commit cannot be nested inside another group",
            ));
        }
        self.group_depth = 1;
        self.pending_ops.clear();
        self.group_undo.clear();
        self.group_force_checkpoint = false;

        // Roll back on panic as well as on `Err`, so a panicking closure cannot
        // leave the live state ahead of the published commit chain.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *self)));
        let value = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(err)) => {
                self.abort_group();
                return Err(err);
            }
            Err(panic) => {
                self.abort_group();
                std::panic::resume_unwind(panic);
            }
        };

        let ops = std::mem::take(&mut self.pending_ops);
        let undo = std::mem::take(&mut self.group_undo);
        let force_checkpoint = std::mem::replace(&mut self.group_force_checkpoint, false);
        self.group_depth = 0;

        if ops.is_empty() {
            return Ok((value, self.head));
        }
        match self.publish_staged(ops, force_checkpoint) {
            Ok(hash) => Ok((value, hash)),
            Err(err) => {
                // Nothing is published on an error return, so the in-memory
                // state must go back to the pre-group value.
                self.rollback(&undo);
                Err(err)
            }
        }
    }

    // ---- selector resolution --------------------------------------------

    /// Evaluate a selector against the authorized universe of a snapshot.
    pub fn resolve(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Option<Hash32>,
        principal: &Principal,
        limits: &weftcodec::Limits,
    ) -> Result<crate::evaluate::Resolution> {
        crate::evaluate::resolve(self, selector, snapshot, principal, limits)
    }

    /// Evaluate a selector against an explicit state pinned by the caller
    /// (`snapshot` is the view identity recorded in the result). Used by the
    /// FUSE frontend so a snapshot mount does not replay per lookup.
    ///
    /// `snapshot` must be the commit whose replay produced `state`; the derived
    /// selector index is cached under that key.
    pub fn resolve_state(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Hash32,
        state: &State,
        principal: &Principal,
        limits: &weftcodec::Limits,
    ) -> Result<crate::evaluate::Resolution> {
        let index = self.state_index(snapshot, state);
        crate::evaluate::resolve_state_with_budget_indexed(
            selector,
            snapshot,
            state,
            principal,
            limits,
            None,
            None,
            None,
            index.as_ref(),
        )
    }

    /// Differential-test oracle: the same resolution with candidate selection
    /// forced to the full authorized scan (no index). `#[doc(hidden)]` because
    /// it exists only to prove indexed/scan equivalence.
    #[doc(hidden)]
    pub fn resolve_scan(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Option<Hash32>,
        principal: &Principal,
        limits: &weftcodec::Limits,
    ) -> Result<crate::evaluate::Resolution> {
        crate::evaluate::resolve_scan(self, selector, snapshot, principal, limits)
    }

    /// Differential-test oracle for an explicit state (no index).
    #[doc(hidden)]
    pub fn resolve_state_scan(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Hash32,
        state: &State,
        principal: &Principal,
        limits: &weftcodec::Limits,
    ) -> Result<crate::evaluate::Resolution> {
        crate::evaluate::resolve_state_scan_with_budget(
            selector, snapshot, state, principal, limits, None, None, None,
        )
    }

    /// Resolve a saved-context view under an optional capability context.
    /// Saved contexts have no ContextRootId, so the capability must grant the
    /// selector namespace (a root grant alone cannot authorize them).
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_saved_context_with_capability(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Hash32,
        state: &State,
        principal: &Principal,
        limits: &weftcodec::Limits,
        capability: Option<&crate::context::CapabilityContext>,
        now_ns: i64,
    ) -> Result<crate::evaluate::Resolution> {
        if let Some(cap) = capability {
            cap.check_valid_at(now_ns)?;
        }
        let index = self.state_index(snapshot, state);
        crate::evaluate::resolve_state_with_budget_indexed(
            selector,
            snapshot,
            state,
            principal,
            limits,
            capability,
            None,
            None,
            index.as_ref(),
        )
    }

    /// Resolve exactly one object, rejecting zero (`NotFound`) and many
    /// (`Ambiguous`) using the full authorized count.
    pub fn require_one(
        &self,
        selector: &weftcodec::selector::Selector,
        snapshot: Option<Hash32>,
        principal: &Principal,
        limits: &weftcodec::Limits,
    ) -> Result<crate::evaluate::ResolvedRef> {
        crate::evaluate::require_one(self, selector, snapshot, principal, limits)
    }

    // ---- verification and GC --------------------------------------------

    /// Verify the superblock, commit chain, manifests, and (optionally) every
    /// chunk and whole-file content hash.
    pub fn verify(&self, full: bool) -> Result<VerifyReport> {
        let mut report = VerifyReport {
            full,
            generation: self.generation,
            commit: Some(self.head),
            ..VerifyReport::default()
        };

        let view = recovery::read_superblock(&self.root)?;
        for note in &view.notes {
            report.problems.push(VerifyProblem {
                class: "Corrupt".to_owned(),
                detail: note.clone(),
            });
        }
        let valid_slots: Vec<SuperblockSlot> = view.slots.iter().flatten().copied().collect();
        if valid_slots.is_empty() {
            report.problems.push(VerifyProblem {
                class: "Corrupt".to_owned(),
                detail: "no valid superblock slot".to_owned(),
            });
        }
        let max_visible = valid_slots
            .iter()
            .map(|slot| slot.generation)
            .max()
            .unwrap_or(0);
        let slot_hashes: BTreeMap<u64, Hash32> = valid_slots
            .iter()
            .map(|slot| (slot.generation, slot.commit))
            .collect();

        let files = recovery::scan_commit_files(&self.root)?;
        for (generation, path) in files {
            match recovery::load_commit_file(&path, Some(generation)) {
                Ok(loaded) => {
                    report.checked_commits += 1;
                    if generation > max_visible {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: format!(
                                "commit file for generation {generation} exists above the \
                                 highest published superblock generation {max_visible}"
                            ),
                        });
                    }
                    if let Some(expected) = slot_hashes.get(&generation) {
                        if *expected != loaded.hash {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!(
                                    "superblock hash for generation {generation} does not \
                                     match the commit record"
                                ),
                            });
                        }
                    }
                    if let Some(slot) = valid_slots
                        .iter()
                        .find(|slot| slot.generation == generation)
                    {
                        if slot.commit_len != loaded.wcb_len {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!(
                                    "superblock length for generation {generation} does not \
                                     match the commit record"
                                ),
                            });
                        }
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("commit file {}: {err}", path.display()),
                }),
            }
        }

        match recovery::recover(&self.root) {
            Ok(recovered) => {
                report.rejected_generations = recovered.rejected.clone();
                // A rejected generation means recovery could not validate that
                // commit's state. Surface the rejection reason itself (for
                // example a missing codec dependency) instead of only the
                // resulting generation gap.
                for note in &recovered.notes {
                    if note.contains("rejected:") {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: note.clone(),
                        });
                    }
                }
                if recovered.generation < max_visible {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: format!(
                            "recovery selected generation {} but the superblock published {}",
                            recovered.generation, max_visible
                        ),
                    });
                }
                match recovery::load_state_at(&self.root, recovered.head, max_visible, true) {
                    Ok((state, notes)) => {
                        for note in notes {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: note,
                            });
                        }
                        self.verify_state(&state, full, &mut report);
                        self.verify_context_state(&state, full, &mut report);
                    }
                    Err(err) => report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!("state replay failed: {err}"),
                    }),
                }
            }
            Err(err) => report.problems.push(VerifyProblem {
                class: err.class_name().to_owned(),
                detail: format!("recovery failed: {err}"),
            }),
        }

        // Verify every checkpoint manifest named by a parsed commit.
        for commit in recovery::all_commits(&self.root, max_visible)? {
            if let Some(checkpoint) = commit.commit.checkpoint {
                report.checked_manifests += 1;
                match recovery::read_state_manifest(&self.root, checkpoint) {
                    Ok(state) => {
                        if state.generation != commit.generation {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!(
                                    "checkpoint {checkpoint} declares generation {} but is \
                                     named by generation {}",
                                    state.generation, commit.generation
                                ),
                            });
                        }
                    }
                    Err(err) => report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!("checkpoint {checkpoint}: {err}"),
                    }),
                }
            }
        }

        // Snapshot pointer files are caches; report mismatches, not absence.
        for (name, hash) in &self.state.snapshots {
            let path = self.root.join("refs").join("snapshots").join(name);
            if let Ok(text) = fs::read_to_string(&path) {
                let text = text.trim();
                if Hash32::from_hex(text).ok() != Some(*hash) {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: format!("snapshot pointer `{name}` does not match state"),
                    });
                }
            }
        }

        report.ok = report.problems.is_empty();
        Ok(report)
    }

    /// Validate Context Root records, causal attributes/closure records, and
    /// stored capability hashes. In `full` mode, re-resolve every Context Root
    /// and require it to reproduce the recorded selector/snapshot/root ids.
    fn verify_context_state(&self, state: &State, full: bool, report: &mut VerifyReport) {
        let limits = Limits::default();
        let principal = match state.objects.get(&State::ROOT_ID) {
            Some(root) => Principal::new(root.uid, root.gid, Vec::new()),
            None => {
                report.problems.push(VerifyProblem {
                    class: "Corrupt".to_owned(),
                    detail: "context verification: namespace root is missing".to_owned(),
                });
                return;
            }
        };
        let now = self.now_ns();

        // Stored capability records: decode and re-hash.
        for relation in state.relations.values() {
            if !crate::context::is_capability_relation(relation) {
                continue;
            }
            let target = match crate::context::relation_target_text(relation) {
                Some(text) => text.to_owned(),
                None => {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: "capability relation target is not text".to_owned(),
                    });
                    continue;
                }
            };
            let hash = match Hash32::from_hex(&target) {
                Ok(hash) => hash,
                Err(_) => {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: "capability target is not a hash".to_owned(),
                    });
                    continue;
                }
            };
            match self.load_capability_at(state, hash) {
                Ok(scope) => {
                    report.checked_capabilities += 1;
                    if scope.id().ok() != Some(hash) {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: "stored capability hash does not match its record".to_owned(),
                        });
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("capability {hash}: {err}"),
                }),
            }
        }

        let contexts = match self.list_context_roots_at(state) {
            Ok(contexts) => contexts,
            Err(err) => {
                report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("context-root records: {err}"),
                });
                return;
            }
        };
        for stored in contexts {
            report.checked_context_roots += 1;
            if stored.root.id().ok() != Some(stored.context_root_id) {
                report.problems.push(VerifyProblem {
                    class: "Corrupt".to_owned(),
                    detail: format!(
                        "context root `{}` id does not match its record",
                        stored.name
                    ),
                });
                continue;
            }
            if let Err(err) = self.state_at(stored.root.snapshot) {
                report.problems.push(VerifyProblem {
                    class: "SnapshotMissing".to_owned(),
                    detail: format!(
                        "context root `{}` pins an unavailable snapshot: {err}",
                        stored.name
                    ),
                });
                continue;
            }
            if !full {
                // Non-full verify still checks that a bound capability record is
                // present when the store holds it; absence is not an error for a
                // capability supplied only by the caller.
                if let Some(hash) = stored.root.capability_hash {
                    if self.load_capability_at(state, hash).is_ok() {
                        report.checked_capabilities += 1;
                    }
                }
                continue;
            }
            let capability = match stored.root.capability_hash {
                Some(hash) => match self.load_capability_at(state, hash) {
                    Ok(scope) => match crate::context::CapabilityContext::new(scope) {
                        Ok(cap) => Some(cap),
                        Err(err) => {
                            report.problems.push(VerifyProblem {
                                class: err.class_name().to_owned(),
                                detail: format!("context root `{}`: {err}", stored.name),
                            });
                            continue;
                        }
                    },
                    Err(err) => {
                        report.problems.push(VerifyProblem {
                            class: "CapabilityMissing".to_owned(),
                            detail: format!(
                                "context root `{}` binds an unavailable capability: {err}",
                                stored.name
                            ),
                        });
                        continue;
                    }
                },
                None => None,
            };
            match self.resolve_context_at(
                state,
                &stored.name,
                capability.as_ref(),
                &principal,
                &limits,
                now,
            ) {
                Ok(resolved) => {
                    if resolved.resolution.selector_id != stored.root.selector_id
                        || resolved.resolution.snapshot_id != stored.root.snapshot
                        || resolved.stored.context_root_id != stored.context_root_id
                    {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: format!(
                                "context root `{}` resolution does not reproduce its recorded ids",
                                stored.name
                            ),
                        });
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("context root `{}` resolution failed: {err}", stored.name),
                }),
            }
        }

        for record in state.objects.values() {
            match crate::state::object_causal_closure(record) {
                Ok(None) => {}
                Ok(Some(closure)) => {
                    report.checked_causal += 1;
                    if !full {
                        continue;
                    }
                    let hash = match closure.id() {
                        Ok(hash) => hash,
                        Err(err) => {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!("object {} causal hash: {err}", record.id),
                            });
                            continue;
                        }
                    };
                    match recovery::read_blob_verified(
                        &self.root,
                        BlobKind::Manifest,
                        hash,
                        weftcodec::hash::DOMAIN_CAUSAL,
                    ) {
                        Ok(bytes) => match CausalClosure::from_canonical_bytes(&bytes, &limits) {
                            Ok(stored_closure) if stored_closure == closure => {}
                            Ok(_) => report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!(
                                    "object {} causal closure blob disagrees with its attrs",
                                    record.id
                                ),
                            }),
                            Err(err) => report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!("object {} causal closure blob: {err}", record.id),
                            }),
                        },
                        Err(err) => report.problems.push(VerifyProblem {
                            class: err.class_name().to_owned(),
                            detail: format!("object {} causal closure blob: {err}", record.id),
                        }),
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("object {} causal attrs: {err}", record.id),
                }),
            }
        }
    }

    fn verify_state(&self, state: &State, full: bool, report: &mut VerifyReport) {
        for record in state.objects.values() {
            if record.kind != ObjectKind::File {
                continue;
            }
            report.checked_objects += 1;
            match recovery::read_manifest(&self.root, record.content) {
                Ok(manifest) => {
                    report.checked_manifests += 1;
                    if manifest.size != record.size {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: format!("object {} size disagrees with manifest", record.id),
                        });
                    }
                    if manifest
                        .representation
                        .as_ref()
                        .map(|rep| rep.kind == RepresentationKind::Codec)
                        .unwrap_or(false)
                    {
                        if full {
                            let frame = match manifest.representation.as_ref().and_then(|r| r.frame)
                            {
                                Some(hash) => hash.to_hex(),
                                None => "<none>".to_owned(),
                            };
                            let mut seen = vec![record.content];
                            match self.read_manifest_bytes_inner(
                                &manifest,
                                0,
                                manifest.size,
                                true,
                                0,
                                &mut seen,
                            ) {
                                Ok(_) => report.checked_representations += 1,
                                Err(err) => report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!(
                                        "codec representation for object {} manifest {} frame {}: {err}",
                                        record.id, record.content, frame
                                    ),
                                }),
                            }
                        }
                        continue;
                    }
                    if full {
                        // Stream chunks through the frozen v1 hasher instead of
                        // assembling the whole file. Verification must stay
                        // bounded for objects near MAX_FILE_BYTES.
                        let mut hasher = identity::content_hash_hasher();
                        let mut total: u64 = 0;
                        for chunk in &manifest.chunks {
                            report.checked_chunks += 1;
                            match recovery::read_blob_verified(
                                &self.root,
                                BlobKind::Object,
                                chunk.hash,
                                identity::CHUNK_DOMAIN,
                            ) {
                                Ok(bytes) => {
                                    if bytes.len() as u64 != chunk.len {
                                        report.problems.push(VerifyProblem {
                                            class: "Corrupt".to_owned(),
                                            detail: format!(
                                                "chunk {} length disagrees with manifest",
                                                chunk.hash
                                            ),
                                        });
                                    }
                                    total = total.saturating_add(bytes.len() as u64);
                                    hasher.update(&bytes);
                                }
                                Err(err) => report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!("chunk {}: {err}", chunk.hash),
                                }),
                            }
                        }
                        if total == manifest.size
                            && identity::finish_content_hash(hasher) != manifest.content_hash
                        {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!("object {} whole-content hash mismatch", record.id),
                            });
                        }
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("manifest for object {}: {err}", record.id),
                }),
            }
        }
    }

    /// Mark-and-sweep GC over unreachable chunks and manifests.
    ///
    /// Roots: current state, every snapshot commit state, and every checkpoint
    /// state manifest. A candidate is only deleted when `apply` is true and it
    /// is older than `gc_grace_secs` (zero disables the age gate).
    pub fn gc(&mut self, apply: bool) -> Result<GcReport> {
        self.require_writable()?;
        if self.group_depth != 0 {
            // GC physically deletes blobs; a later group rollback would leave
            // the restored state pointing at deleted content.
            return Err(Error::invalid_argument(
                "gc cannot run inside a group commit",
            ));
        }
        let mut report = GcReport {
            applied: apply,
            ..GcReport::default()
        };

        // A pinned unlinked object is a normal open-handle retention. If the
        // process crashed with such a handle, an unreferenced object record can
        // remain in the commit chain; `gc --apply` removes those records in a
        // commit so the blobs become unreachable. Pinned and relation-targeted
        // objects are never removed here.
        if apply {
            let orphan_ids: Vec<Hash32> = self
                .state
                .objects
                .keys()
                .copied()
                .filter(|id| *id != State::ROOT_ID)
                .filter(|id| !self.is_object_pinned(*id))
                .filter(|id| !self.state.entries.values().any(|entry| entry.target == *id))
                .filter(|id| !self.state.entries.values().any(|entry| entry.parent == *id))
                .filter(|id| {
                    !self.state.relations.values().any(|relation| {
                        matches!(relation.target, RelationTarget::Object(target) if target == *id)
                    })
                })
                .collect();
            if !orphan_ids.is_empty() {
                let ops: Vec<Op> = orphan_ids.iter().map(|id| Op::DeleteObject(*id)).collect();
                self.commit_ops(ops, false)?;
                report.removed_orphans = orphan_ids.iter().map(|id| id.to_hex()).collect();
            }
        }

        let mut marked_manifests: BTreeSet<Hash32> = BTreeSet::new();
        let mut marked_chunks: BTreeSet<Hash32> = BTreeSet::new();

        mark_state(
            &self.root,
            &self.state,
            &mut marked_manifests,
            &mut marked_chunks,
            &mut report.problems,
        );

        let snapshot_commits: Vec<Hash32> = self.state.snapshots.values().copied().collect();
        for commit in snapshot_commits {
            match self.state_at(commit) {
                Ok(state) => mark_state(
                    &self.root,
                    &state,
                    &mut marked_manifests,
                    &mut marked_chunks,
                    &mut report.problems,
                ),
                Err(err) => report
                    .problems
                    .push(format!("snapshot {commit} state is unreadable: {err}")),
            }
        }

        // Checkpoints are roots even for rejected/unpublished generations:
        // they are immutable evidence and are never swept by inference.
        for commit in recovery::all_commits(&self.root, u64::MAX)? {
            if let Some(checkpoint) = commit.commit.checkpoint {
                marked_manifests.insert(checkpoint);
                match recovery::read_state_manifest(&self.root, checkpoint) {
                    Ok(state) => mark_state(
                        &self.root,
                        &state,
                        &mut marked_manifests,
                        &mut marked_chunks,
                        &mut report.problems,
                    ),
                    Err(err) => report
                        .problems
                        .push(format!("checkpoint {checkpoint} is unreadable: {err}")),
                }
            }
        }

        // Content pins held by open read handles are roots even if the object
        // record has already been removed from state.
        let pinned_contents: Vec<Hash32> = self.content_pins.keys().copied().collect();
        for content in pinned_contents {
            let mut seen = BTreeSet::new();
            mark_manifest_tree(
                &self.root,
                content,
                &mut marked_manifests,
                &mut marked_chunks,
                &mut report.problems,
                0,
                &mut seen,
            );
        }

        report.marked_manifests = marked_manifests.len() as u64;
        report.marked_chunks = marked_chunks.len() as u64;

        let now = SystemTime::now();
        let grace = self.options.gc_grace_secs;
        let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
        candidates.extend(collect_unmarked(
            &self.root.join("manifests"),
            &marked_manifests,
            now,
            grace,
            &mut report,
        )?);
        candidates.extend(collect_unmarked(
            &self.root.join("objects"),
            &marked_chunks,
            now,
            grace,
            &mut report,
        )?);
        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        for (path, len) in &candidates {
            let display = path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .display()
                .to_string();
            report.candidates.push(display.clone());
            if apply {
                match fs::remove_file(path) {
                    Ok(()) => {
                        report.deleted.push(display);
                        report.deleted_bytes = report.deleted_bytes.saturating_add(*len);
                    }
                    Err(err) => {
                        return Err(Error::io(format!(
                            "cannot delete {}: {err}",
                            path.display()
                        )))
                    }
                }
            }
        }
        Ok(report)
    }

    // ---- internals -------------------------------------------------------

    /// Current store clock in UTC nanoseconds (the fixed fixture clock when
    /// configured). Capability expiry is enforced against this clock.
    pub fn clock_now_ns(&self) -> i64 {
        self.now_ns()
    }

    /// Crate-internal commit hook used by the Context Root module so a single
    /// commit can publish a capability record and a context-root relation
    /// together.
    pub(crate) fn commit_ops_pub(
        &mut self,
        ops: Vec<Op>,
        force_checkpoint: bool,
    ) -> Result<Hash32> {
        self.commit_ops(ops, force_checkpoint)
    }

    /// Store a causal-closure record as a manifest-domain blob. The causal
    /// record's `CausalClosureId` is exactly `H("weft/causal/v1", bytes)`.
    pub(crate) fn store_causal_blob(&self, bytes: &[u8]) -> Result<Hash32> {
        self.store_blob(BlobKind::Manifest, weftcodec::hash::DOMAIN_CAUSAL, bytes)
    }

    fn require_writable(&self) -> Result<()> {
        if self.mode != OpenMode::ReadWrite {
            return Err(Error::read_only(
                "store is opened read-only; mutation refused",
            ));
        }
        Ok(())
    }

    fn require_dir(&self, id: Hash32) -> Result<()> {
        let object = self
            .state
            .objects
            .get(&id)
            .ok_or_else(|| Error::not_found("no such parent object"))?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("parent object is not a directory"));
        }
        Ok(())
    }

    fn require_free_name(&self, parent: Hash32, name: &[u8]) -> Result<()> {
        if self
            .state
            .entries
            .contains_key(&derive_entry_id(parent, name)?)
        {
            return Err(Error::exists("directory entry already exists"));
        }
        Ok(())
    }

    fn now_ns(&self) -> i64 {
        if let Some(fixed) = self.options.fixed_timestamp_ns {
            return fixed;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
            .unwrap_or(0)
    }

    fn write_genesis(&mut self) -> Result<()> {
        let mtime = self.now_ns();
        let content = identity::content_hash(b"");
        let version = derive_version_id(State::ROOT_ID, content, 0, mtime, None)?;
        let root = ObjectRecord {
            id: State::ROOT_ID,
            kind: ObjectKind::Dir,
            mode: 0o755,
            uid: self.options.owner_uid,
            gid: self.options.owner_gid,
            mtime_ns: mtime,
            version,
            content,
            size: 0,
            attrs: BTreeMap::new(),
        };
        let ops = vec![
            Op::PutObject(root),
            Op::SetTip {
                object: State::ROOT_ID,
                version,
            },
        ];
        let mut state = State::default();
        state.apply_ops(&ops)?;
        state.generation = 1;
        let commit = Commit {
            generation: 1,
            parent: None,
            timestamp_ns: mtime,
            ops,
            checkpoint: None,
        };
        self.publish_commit(&commit)?;
        self.state = state;
        self.head = commit.id()?;
        self.generation = 1;
        self.next_generation = 2;
        Ok(())
    }

    fn commit_ops(&mut self, ops: Vec<Op>, force_checkpoint: bool) -> Result<Hash32> {
        self.require_writable()?;
        if self.group_depth != 0 {
            if force_checkpoint {
                self.group_force_checkpoint = true;
            }
            self.stage_ops(ops)?;
            // Nothing is published until the group closes, so no commit hash
            // exists yet. Callers inside a group must use the hash `group`
            // returns; this value is the pre-group head.
            return Ok(self.head);
        }
        let new_generation = self.next_generation;
        let timestamp = self.now_ns();
        let mut next_state = self.state.clone();
        next_state.apply_ops(&ops)?;
        next_state.generation = new_generation;

        let interval = self.options.checkpoint_interval;
        let wants_checkpoint = force_checkpoint || (interval > 0 && new_generation % interval == 0);
        let checkpoint = if wants_checkpoint {
            let bytes = encode_state(&next_state, new_generation)?;
            Some(self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &bytes)?)
        } else {
            None
        };

        // Every content blob and checkpoint manifest is durable here.
        crash_point("after_objects");

        let commit = Commit {
            generation: new_generation,
            parent: Some(self.head),
            timestamp_ns: timestamp,
            ops,
            checkpoint,
        };
        let hash = self.publish_commit(&commit)?;

        // Keep the in-memory state consistent with the published commit.
        self.state = next_state;
        self.head = hash;
        self.generation = new_generation;
        self.next_generation = new_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        Ok(hash)
    }

    /// Apply one mutation's ops to the live state and buffer them for the open
    /// group.
    ///
    /// The ops are applied one at a time so read-your-writes works from the
    /// next statement in the closure; the batch is still all-or-nothing because
    /// every touched key is journalled first and unwound if any op rejects it.
    fn stage_ops(&mut self, ops: Vec<Op>) -> Result<()> {
        let mark = self.group_undo.len();
        for op in &ops {
            self.journal(op);
        }
        for op in &ops {
            if let Err(err) = self.state.apply_op(op) {
                self.rollback_from(mark);
                return Err(err);
            }
        }
        self.pending_ops.extend(ops);
        Ok(())
    }

    /// Record the pre-op value of every state key `op` can touch.
    fn journal(&mut self, op: &Op) {
        match op {
            Op::PutObject(record) => self.group_undo.push(Undo::Object(
                record.id,
                self.state.objects.get(&record.id).cloned(),
            )),
            Op::DeleteObject(id) => {
                self.group_undo
                    .push(Undo::Object(*id, self.state.objects.get(id).cloned()));
                // `DeleteObject` also drops the tip for the object.
                self.group_undo
                    .push(Undo::Tip(*id, self.state.tips.get(id).copied()));
            }
            Op::PutEntry(record) => self.group_undo.push(Undo::Entry(
                record.id,
                self.state.entries.get(&record.id).cloned(),
            )),
            Op::DelEntry(id) => self
                .group_undo
                .push(Undo::Entry(*id, self.state.entries.get(id).cloned())),
            Op::PutRelation(record) => self.group_undo.push(Undo::Relation(
                record.id,
                self.state.relations.get(&record.id).cloned(),
            )),
            Op::DelRelation(id) => self
                .group_undo
                .push(Undo::Relation(*id, self.state.relations.get(id).cloned())),
            Op::SetSnapshot { name, .. } => self.group_undo.push(Undo::Snapshot(
                name.clone(),
                self.state.snapshots.get(name).copied(),
            )),
            Op::SetTip { object, .. } => self
                .group_undo
                .push(Undo::Tip(*object, self.state.tips.get(object).copied())),
        }
    }

    /// Discard the open group without publishing and restore the pre-group
    /// in-memory state.
    fn abort_group(&mut self) {
        let undo = std::mem::take(&mut self.group_undo);
        self.rollback(&undo);
        self.pending_ops.clear();
        self.group_force_checkpoint = false;
        self.group_depth = 0;
    }

    /// Unwind a whole undo journal (newest entry first).
    fn rollback(&mut self, undo: &[Undo]) {
        for entry in undo.iter().rev() {
            match entry {
                Undo::Object(id, previous) => {
                    restore(&mut self.state.objects, *id, previous.clone())
                }
                Undo::Entry(id, previous) => {
                    restore(&mut self.state.entries, *id, previous.clone())
                }
                Undo::Relation(id, previous) => {
                    restore(&mut self.state.relations, *id, previous.clone())
                }
                Undo::Snapshot(name, previous) => {
                    restore(&mut self.state.snapshots, name.clone(), *previous)
                }
                Undo::Tip(id, previous) => restore(&mut self.state.tips, *id, *previous),
            }
        }
    }

    /// Unwind journal entries recorded after `mark` and forget them.
    fn rollback_from(&mut self, mark: usize) {
        let undo: Vec<Undo> = self.group_undo.drain(mark..).collect();
        self.rollback(&undo);
    }

    /// Publish the buffered ops as exactly one commit. The live state already
    /// contains every op, so unlike [`Store::commit_ops`] nothing is replayed.
    fn publish_staged(&mut self, ops: Vec<Op>, force_checkpoint: bool) -> Result<Hash32> {
        let new_generation = self.next_generation;
        let next_generation = new_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        let previous_state_generation = self.state.generation;
        self.state.generation = new_generation;
        match self.try_publish_staged(new_generation, ops, force_checkpoint) {
            Ok(hash) => {
                self.next_generation = next_generation;
                Ok(hash)
            }
            Err(err) => {
                self.state.generation = previous_state_generation;
                // A commit file may already exist for `new_generation`, so that
                // number is never reused even when nothing was published.
                self.next_generation = next_generation;
                Err(err)
            }
        }
    }

    fn try_publish_staged(
        &mut self,
        new_generation: u64,
        ops: Vec<Op>,
        force_checkpoint: bool,
    ) -> Result<Hash32> {
        let timestamp = self.now_ns();
        let interval = self.options.checkpoint_interval;
        let wants_checkpoint = force_checkpoint || (interval > 0 && new_generation % interval == 0);
        let checkpoint = if wants_checkpoint {
            let bytes = encode_state(&self.state, new_generation)?;
            Some(self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &bytes)?)
        } else {
            None
        };

        // Every content blob and checkpoint manifest is durable here. Nothing
        // is visible until the superblock slot below is written, so a fault at
        // this point still leaves the previous generation whole.
        crash_point("after_objects");

        let commit = Commit {
            generation: new_generation,
            parent: Some(self.head),
            timestamp_ns: timestamp,
            ops,
            checkpoint,
        };
        let hash = self.publish_commit(&commit)?;

        self.head = hash;
        self.generation = new_generation;
        Ok(hash)
    }

    /// Write the commit file, fsync it and its directory, publish the
    /// superblock slot, then fsync the store root.
    fn publish_commit(&self, commit: &Commit) -> Result<Hash32> {
        let wcb = commit.encode()?;
        let hash = identity::commit_id(&wcb);
        let file_bytes = encode_commit_file(&wcb)?;
        let path = crate::recovery::commit_path(&self.root, commit.generation);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
            .map_err(|err| {
                Error::corrupt(format!(
                    "cannot create commit file {}: {err}",
                    path.display()
                ))
            })?;
        file.write_all(&file_bytes)?;
        file.sync_all()?;
        drop(file);
        fsync_dir(&self.root.join("commits"))?;

        crash_point("after_commit_file");

        let slot = SuperblockSlot {
            generation: commit.generation,
            commit: hash,
            commit_len: wcb.len() as u64,
            timestamp_ns: commit.timestamp_ns,
        };
        let superblock_path = self.root.join("superblock");
        let superblock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&superblock_path)
            .map_err(|err| Error::io(format!("cannot open superblock: {err}")))?;
        let offset = (commit.generation % 2) * SLOT_BYTES as u64;
        superblock.write_all_at(&slot.encode(), offset)?;
        superblock.sync_all()?;
        drop(superblock);

        crash_point("after_superblock_write");

        fsync_dir(&self.root)?;
        Ok(hash)
    }

    /// Stream `reader` into content-addressed chunk blobs and hash the same
    /// bytes incrementally with the frozen v1 whole-content hasher.
    ///
    /// `prefix` must already contain whole chunks (it is the buffered codec
    /// preamble). The I/O buffer is at least `STREAM_IO_BYTES` and a multiple
    /// of the chunk size, so every emitted chunk is exactly `chunk_size` bytes
    /// except the final one. Neither the prefix nor the I/O buffer is retained
    /// after return; peak memory is O(io buffer + chunk refs).
    fn stream_raw_chunks<R: Read>(
        &self,
        reader: &mut R,
        chunk_size: u64,
        prefix: Vec<u8>,
    ) -> Result<(Vec<ChunkRef>, u64, Hash32)> {
        validate_chunk_size(chunk_size)?;
        let step = checked_usize(chunk_size, "chunk size")?;
        if !prefix.is_empty() && prefix.len() % step != 0 {
            return Err(Error::invalid_argument(
                "streaming prefix is not chunk-aligned",
            ));
        }
        let mut chunks: Vec<ChunkRef> = Vec::new();
        let mut hasher = identity::content_hash_hasher();
        let mut total: u64 = 0;

        if !prefix.is_empty() {
            total = u64::try_from(prefix.len())
                .map_err(|_| Error::limit_exceeded("prefix length does not fit u64"))?;
            if total > MAX_FILE_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "file exceeds the {MAX_FILE_BYTES}-byte bound"
                )));
            }
            hasher.update(&prefix);
            for part in prefix.chunks(step) {
                let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, part)?;
                push_chunk_ref(&mut chunks, hash, part.len() as u64)?;
            }
        }

        let io_len = stream_io_len(step)?;
        let mut buffer = vec![0u8; io_len];
        loop {
            let filled = read_fill(reader, &mut buffer)?;
            if filled == 0 {
                break;
            }
            total = total
                .checked_add(filled as u64)
                .ok_or_else(|| Error::limit_exceeded("file size overflows"))?;
            if total > MAX_FILE_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "file exceeds the {MAX_FILE_BYTES}-byte bound"
                )));
            }
            hasher.update(&buffer[..filled]);
            for part in buffer[..filled].chunks(step) {
                let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, part)?;
                push_chunk_ref(&mut chunks, hash, part.len() as u64)?;
            }
            if filled < io_len {
                break;
            }
        }
        Ok((chunks, total, identity::finish_content_hash(hasher)))
    }

    /// Store a manifest assembled by the streaming paths. `v2_raw` selects the
    /// experimental v2 `kind:"raw"` representation used by `CodecAuto`;
    /// otherwise the frozen v1 manifest is written byte-for-byte.
    fn store_streamed_manifest(
        &self,
        chunk_size: u64,
        chunks: Vec<ChunkRef>,
        size: u64,
        content_hash: Hash32,
        v2_raw: bool,
    ) -> Result<(Hash32, Hash32, u64)> {
        let representation = if v2_raw {
            Some(Representation {
                kind: RepresentationKind::Raw,
                frame: None,
                dependencies: Vec::new(),
                strategy: "raw".to_owned(),
                decoded_size: size,
            })
        } else {
            None
        };
        self.store_manifest(Manifest {
            size,
            chunk_size,
            chunks,
            content_hash,
            representation,
        })
    }

    /// Encode and store a complete in-memory buffer. Used for initial file
    /// content (`create_file*`), `truncate`, and the whole-file bench baseline.
    /// Range writes go through `Store::store_file_range` instead.
    fn store_file_content(&self, bytes: &[u8]) -> Result<(Hash32, Hash32, u64)> {
        self.store_file_content_with_base(bytes, None)
    }

    fn store_file_content_with_base(
        &self,
        bytes: &[u8],
        base: Option<(Hash32, &[u8])>,
    ) -> Result<(Hash32, Hash32, u64)> {
        self.store_content_bytes(
            bytes,
            self.options.chunk_size,
            base,
            self.options.representation == RepresentationMode::CodecAuto,
        )
    }

    /// Store a whole content buffer under an explicit chunk size.
    ///
    /// `codec_auto == false` is the frozen Raw path and produces exactly the
    /// pre-representation v1 manifest bytes. `codec_auto == true` may store a
    /// v2 codec representation or a v2 `kind:"raw"` fallback.
    fn store_content_bytes(
        &self,
        bytes: &[u8],
        chunk_size: u64,
        base: Option<(Hash32, &[u8])>,
        codec_auto: bool,
    ) -> Result<(Hash32, Hash32, u64)> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
            return Err(Error::invalid_argument("chunk size is out of range"));
        }
        let content_hash = identity::content_hash(bytes);
        if codec_auto {
            if bytes.len() as u64 >= self.options.codec_min_bytes {
                if let Some((frame_bytes, representation)) =
                    self.try_codec_representation(bytes, chunk_size, base)?
                {
                    let frame_id = self.store_blob(
                        BlobKind::Manifest,
                        identity::MANIFEST_DOMAIN,
                        &frame_bytes,
                    )?;
                    let representation = Representation {
                        frame: Some(frame_id),
                        ..representation
                    };
                    return self.store_manifest(Manifest {
                        size: bytes.len() as u64,
                        chunk_size,
                        chunks: Vec::new(),
                        content_hash,
                        representation: Some(representation),
                    });
                }
            }
            // CodecAuto always writes a v2 manifest, even when it falls back to
            // the raw chunk layout.
            let manifest = self.build_raw_manifest(bytes, chunk_size, content_hash)?;
            let manifest = Manifest {
                representation: Some(Representation {
                    kind: RepresentationKind::Raw,
                    frame: None,
                    dependencies: Vec::new(),
                    strategy: "raw".to_owned(),
                    decoded_size: bytes.len() as u64,
                }),
                ..manifest
            };
            return self.store_manifest(manifest);
        }
        let manifest = self.build_raw_manifest(bytes, chunk_size, content_hash)?;
        self.store_manifest(manifest)
    }

    /// Write the chunk blobs for a raw layout and return a v1-shaped manifest
    /// value (with no representation).
    fn build_raw_manifest(
        &self,
        bytes: &[u8],
        chunk_size: u64,
        content_hash: Hash32,
    ) -> Result<Manifest> {
        let mut chunks = Vec::new();
        let step = usize::try_from(chunk_size)
            .map_err(|_| Error::limit_exceeded("chunk size does not fit usize"))?;
        for chunk in bytes.chunks(step) {
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, chunk)?;
            push_chunk_ref(&mut chunks, hash, chunk.len() as u64)?;
        }
        Ok(Manifest {
            size: bytes.len() as u64,
            chunk_size,
            chunks,
            content_hash,
            representation: None,
        })
    }

    fn store_manifest(&self, manifest: Manifest) -> Result<(Hash32, Hash32, u64)> {
        let size = manifest.size;
        let content_hash = manifest.content_hash;
        let wcb = manifest.encode()?;
        if wcb.len() as u64 > crate::format::MAX_CHUNK_SIZE * 64 {
            return Err(Error::limit_exceeded(
                "manifest encoding is implausibly large",
            ));
        }
        let manifest_id = self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &wcb)?;
        Ok((manifest_id, content_hash, size))
    }

    /// Try to build a codec representation. Returns `None` when no encoding is
    /// available or when the chosen frame would not be strictly smaller than
    /// the raw chunk bytes (in which case the caller falls back to v2 raw).
    fn try_codec_representation(
        &self,
        bytes: &[u8],
        chunk_size: u64,
        base: Option<(Hash32, &[u8])>,
    ) -> Result<Option<(Vec<u8>, Representation)>> {
        let limits = Limits::default();
        let codec_chunk = representation::codec_chunk_size(chunk_size);
        let codec_chunk_usize = usize::try_from(codec_chunk)
            .map_err(|_| Error::limit_exceeded("codec chunk size does not fit usize"))?;
        let zstd = weftcodec::modes::encode_zstd(
            bytes,
            representation::CODEC_ZSTD_LEVEL,
            Some(codec_chunk_usize),
            &limits,
        )
        .ok();
        let mut chosen: Option<(Vec<u8>, String, Vec<Hash32>)> =
            zstd.map(|frame| (frame, "zstd".to_owned(), Vec::new()));

        if let Some((base_root, base_bytes)) = base {
            if let Ok(delta) = weftcodec::modes::encode_delta(bytes, base_bytes, &limits) {
                let margin = u64::from(self.options.codec_delta_margin_percent.min(90));
                let wins = match &chosen {
                    Some((zstd_frame, _, _)) => {
                        (delta.len() as u64) * 100
                            <= (zstd_frame.len() as u64) * (100u64.saturating_sub(margin))
                    }
                    // No zstd frame: still require the delta to beat raw bytes.
                    None => delta.len() < bytes.len(),
                };
                if wins {
                    chosen = Some((delta, "delta".to_owned(), vec![base_root]));
                }
            }
        }

        let Some((frame, strategy, dependencies)) = chosen else {
            return Ok(None);
        };
        if frame.len() >= bytes.len() {
            return Ok(None);
        }
        Ok(Some((
            frame,
            Representation {
                kind: RepresentationKind::Codec,
                frame: None,
                dependencies,
                strategy,
                decoded_size: bytes.len() as u64,
            },
        )))
    }

    /// Resolve the first causal parent to a live file version and return
    /// `(content root, plaintext bytes)` for use as a DELTA base.
    fn delta_base(&self, causal: &[Hash32]) -> Result<Option<(Hash32, Vec<u8>)>> {
        if self.options.representation != RepresentationMode::CodecAuto || causal.is_empty() {
            return Ok(None);
        }
        for parent in causal {
            for record in self.state.objects.values() {
                if record.kind != ObjectKind::File || record.version != *parent {
                    continue;
                }
                match self.read_content(record.content, 0, record.size) {
                    Ok(bytes) => return Ok(Some((record.content, bytes))),
                    // A base that cannot be materialised is not usable; the
                    // writer falls back to ZSTD rather than failing the write.
                    Err(_) => continue,
                }
            }
        }
        Ok(None)
    }

    /// Publish a new manifest for a range write, reusing every untouched chunk
    /// reference verbatim.
    ///
    /// `new_size` must satisfy `new_size >= old_manifest.size` and
    /// `offset + bytes.len() <= new_size`; the caller has already applied the
    /// v1 size bound. The byte range replaces the old content at `offset` and
    /// any gap between the old EOF and `offset` is zero-filled.
    ///
    /// Only chunks intersecting `[offset, offset + len)` (plus the old tail and
    /// any gap chunks when the write extends EOF) are reconstructed. A
    /// reconstructed chunk is stored only if its bytes differ from the old
    /// chunk; otherwise the old reference is reused. Unchanged chunks are
    /// never read for materialisation and never re-written.
    ///
    /// Cost model: physical writes and peak memory are O(changed chunks +
    /// chunk_size). The digest is the exception: the frozen v1 identity is a
    /// single whole-file `ContentId = H("weft/chunk/v1", file_bytes)`, so it
    /// can only be produced by streaming the whole file. That pass is O(file
    /// size) by construction; it is done with one chunk-sized buffer at a time
    /// so memory never becomes O(file size). It cannot be replaced by a Merkle
    /// root without breaking v1 identity.
    pub(crate) fn store_file_range(
        &self,
        old_manifest: &Manifest,
        offset: u64,
        bytes: &[u8],
        new_size: u64,
    ) -> Result<(Hash32, Hash32, u64)> {
        let chunk_size = old_manifest.chunk_size;
        if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
            return Err(Error::invalid_argument("chunk size is out of range"));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
        if new_size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        if end > new_size || new_size < old_manifest.size {
            return Err(Error::invalid_argument(
                "range write exceeds the target file size",
            ));
        }
        let declared = old_manifest
            .chunks
            .iter()
            .try_fold(0u64, |acc, chunk| acc.checked_add(chunk.len))
            .ok_or_else(|| Error::corrupt("manifest size overflows"))?;
        if declared != old_manifest.size {
            return Err(Error::corrupt(
                "manifest chunk lengths disagree with its size",
            ));
        }

        let chunk_count = (new_size + chunk_size - 1) / chunk_size;
        if chunk_count > MAX_CHUNKS_PER_FILE {
            return Err(Error::limit_exceeded(format!(
                "manifest exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
            )));
        }
        let mut chunks: Vec<ChunkRef> = Vec::with_capacity(
            usize::try_from(chunk_count)
                .map_err(|_| Error::limit_exceeded("chunk count does not fit usize"))?,
        );
        for index in 0..chunk_count {
            let chunk_start = index * chunk_size;
            let chunk_end = (chunk_start + chunk_size).min(new_size);
            let old_ref = old_manifest
                .chunks
                .get(checked_usize(index, "chunk index")?);
            let touches_write = chunk_start < end && offset < chunk_end;
            if let Some(old_ref) = old_ref {
                if chunk_end <= old_manifest.size && !touches_write {
                    // Unchanged chunk: reuse the old reference verbatim. Its
                    // blob is neither read nor re-written here.
                    chunks.push(old_ref.clone());
                    continue;
                }
            }
            // Reconstruct this chunk from the old bytes (if any), the written
            // range, and zeros for any gap past the old EOF.
            let mut assembled = vec![0u8; checked_usize(chunk_end - chunk_start, "chunk length")?];
            let mut old_bytes: Option<Vec<u8>> = None;
            if let Some(old_ref) = old_ref {
                let keep = checked_usize(
                    old_ref.len.min(chunk_end - chunk_start),
                    "chunk keep length",
                )?;
                if keep > 0 {
                    let stored = recovery::read_blob_verified(
                        &self.root,
                        BlobKind::Object,
                        old_ref.hash,
                        identity::CHUNK_DOMAIN,
                    )?;
                    if stored.len() as u64 != old_ref.len {
                        return Err(Error::corrupt("chunk length disagrees with manifest"));
                    }
                    assembled[..keep].copy_from_slice(&stored[..keep]);
                    old_bytes = Some(stored);
                }
            }
            let write_start = offset.max(chunk_start);
            let write_end = end.min(chunk_end);
            if write_start < write_end {
                let dst_start = checked_usize(write_start - chunk_start, "chunk write start")?;
                let dst_end = checked_usize(write_end - chunk_start, "chunk write end")?;
                let src_start = checked_usize(write_start - offset, "range write start")?;
                let src_end = checked_usize(write_end - offset, "range write end")?;
                assembled[dst_start..dst_end].copy_from_slice(&bytes[src_start..src_end]);
            }
            if let (Some(old_ref), Some(old_bytes)) = (old_ref, old_bytes.as_deref()) {
                // A write that reproduces the old bytes must not add a blob.
                if old_ref.len == chunk_end - chunk_start && assembled == old_bytes {
                    chunks.push(old_ref.clone());
                    continue;
                }
            }
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, &assembled)?;
            push_chunk_ref(&mut chunks, hash, assembled.len() as u64)?;
        }

        // Stream the new content in chunk order into the frozen v1 whole-file
        // digest. Peak memory is one chunk, never the whole file.
        let mut hasher = identity::content_hash_hasher();
        for chunk in &chunks {
            let data = recovery::read_blob_verified(
                &self.root,
                BlobKind::Object,
                chunk.hash,
                identity::CHUNK_DOMAIN,
            )?;
            if data.len() as u64 != chunk.len {
                return Err(Error::corrupt("chunk length disagrees with manifest"));
            }
            hasher.update(&data);
        }
        let content_hash = identity::finish_content_hash(hasher);
        let manifest = Manifest {
            size: new_size,
            chunk_size,
            chunks,
            content_hash,
            representation: None,
        };
        let wcb = manifest.encode()?;
        if wcb.len() as u64 > crate::format::MAX_CHUNK_SIZE * 64 {
            return Err(Error::limit_exceeded(
                "manifest encoding is implausibly large",
            ));
        }
        let manifest_id = self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &wcb)?;
        Ok((manifest_id, content_hash, new_size))
    }

    /// Content-addressed, crash-safe blob write: `tmp/` → fsync → rename →
    /// fsync directory. An existing file is verified, never overwritten.
    fn store_blob(&self, kind: BlobKind, domain: &str, bytes: &[u8]) -> Result<Hash32> {
        let hash = identity::h(domain, bytes);
        let final_path = match kind {
            BlobKind::Object => recovery::object_path(&self.root, hash),
            BlobKind::Manifest => recovery::manifest_path(&self.root, hash),
        };
        if final_path.exists() {
            let existing = fs::read(&final_path)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", final_path.display())))?;
            if identity::h(domain, &existing) != hash {
                return Err(Error::corrupt(format!(
                    "existing blob {} does not match its name",
                    final_path.display()
                )));
            }
            return Ok(hash);
        }
        let parent = final_path
            .parent()
            .ok_or_else(|| Error::io("blob path has no parent"))?;
        if !parent.exists() {
            make_dir(parent)?;
            // A newly created two-hex shard directory needs its own parent
            // fsynced, or a power loss could lose the shard entry even though
            // the blob rename inside it was fsynced.
            if let Some(grandparent) = parent.parent() {
                fsync_dir(grandparent)?;
            }
        } else {
            make_dir(parent)?;
        }
        let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join("tmp").join(format!(
            "blob-{}-{}-{counter}",
            std::process::id(),
            hash.to_hex()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if final_path.exists() {
            let existing = fs::read(&final_path)?;
            fs::remove_file(&tmp).ok();
            if identity::h(domain, &existing) != hash {
                return Err(Error::corrupt("blob appeared with a mismatched hash"));
            }
            return Ok(hash);
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, &final_path).map_err(|err| {
            cleanup_tmp(&tmp);
            Error::io(format!(
                "cannot rename {} into place: {err}",
                final_path.display()
            ))
        })?;
        fsync_dir(parent)?;
        Ok(hash)
    }

    fn write_snapshot_pointer(&self, name: &str, commit: Hash32) -> Result<()> {
        let dir = self.root.join("refs").join("snapshots");
        make_dir(&dir)?;
        let path = dir.join(name);
        let tmp = self.root.join("tmp").join(format!(
            "snap-{}-{}-{}",
            std::process::id(),
            name,
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)
            .map_err(|err| Error::io(format!("cannot create snapshot pointer: {err}")))?;
        file.write_all(format!("{}\n", commit.to_hex()).as_bytes())?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            fs::remove_file(&tmp).ok();
            return Err(Error::exists("snapshot pointer already exists"));
        }
        fs::rename(&tmp, &path)
            .map_err(|err| Error::io(format!("cannot publish snapshot pointer: {err}")))?;
        fsync_dir(&dir)?;
        Ok(())
    }
}

fn cleanup_tmp(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Checked `u64 -> usize` conversion for offsets, lengths, counts, and sizes.
/// A value that does not fit the platform address space is `LimitExceeded`,
/// never a silent truncation.
fn checked_usize(value: u64, what: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| Error::limit_exceeded(format!("{what} ({value}) does not fit usize")))
}

fn validate_chunk_size(chunk_size: u64) -> Result<()> {
    if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
        return Err(Error::invalid_argument("chunk size is out of range"));
    }
    Ok(())
}

/// Enforce the per-manifest chunk-count resource bound before a push, so a
/// pathological configured chunk size cannot grow an O(file size) chunk list.
fn push_chunk_ref(chunks: &mut Vec<ChunkRef>, hash: Hash32, len: u64) -> Result<()> {
    if chunks.len() as u64 >= MAX_CHUNKS_PER_FILE {
        return Err(Error::limit_exceeded(format!(
            "manifest exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
        )));
    }
    chunks.push(ChunkRef { hash, len });
    Ok(())
}

/// Source-read size for streaming paths: at least [`STREAM_IO_BYTES`], rounded
/// up to a whole number of chunks so manifest chunk boundaries stay aligned.
fn stream_io_len(step: usize) -> Result<usize> {
    let target = STREAM_IO_BYTES.max(step);
    let chunks_per_read = target.div_ceil(step);
    chunks_per_read
        .checked_mul(step)
        .ok_or_else(|| Error::limit_exceeded("stream I/O buffer size overflows usize"))
}

/// Bounded buffer used to decide whether a `CodecAuto` stream is small enough
/// for the existing whole-buffer codec path. The limit is the larger of the
/// configured threshold and one chunk, rounded up to a chunk multiple, and
/// capped at the format's maximum chunk size (16 MiB). A stream that does not
/// end inside this buffer is written as a v2 `kind:"raw"` manifest.
fn stream_preamble_limit(codec_min_bytes: u64, chunk_size: u64) -> Result<usize> {
    validate_chunk_size(chunk_size)?;
    let step = checked_usize(chunk_size, "chunk size")?;
    let bounded = codec_min_bytes
        .max(chunk_size)
        .min(crate::format::MAX_CHUNK_SIZE);
    let bounded = checked_usize(bounded, "stream preamble limit")?;
    bounded
        .div_ceil(step)
        .checked_mul(step)
        .ok_or_else(|| Error::limit_exceeded("stream preamble size overflows usize"))
}

/// Fill `buf` from `reader` until it is full or the reader reports EOF.
/// Interrupted reads are retried; any other error becomes an `Io` error, which
/// the streaming mutation paths propagate before publishing a commit.
fn read_fill<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(Error::io(format!("stream read failed: {err}"))),
        }
    }
    Ok(filled)
}

/// `saved_context` relations have exactly this shape (`WEFT-MODEL.md` §2.5):
/// root subject, value target, no provenance.
fn is_saved_context(relation: &RelationRecord) -> bool {
    relation.subject == State::ROOT_ID
        && relation.predicate == SAVED_CONTEXT_PREDICATE
        && relation.kind == RelationKind::Value
        && relation.provenance.is_none()
}

/// Extended-attribute names are UTF-8 text without NUL or `/`, at most 255
/// bytes, and may not use the reserved `weft.` namespace.
fn validate_xattr_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_name(
            "extended attribute name must not be empty",
        ));
    }
    if name.len() > MAX_XATTR_NAME_BYTES {
        return Err(Error::invalid_name(
            "extended attribute name exceeds 255 bytes",
        ));
    }
    if name.contains('\0') || name.contains('/') {
        return Err(Error::invalid_name(
            "extended attribute name must not contain NUL or '/'",
        ));
    }
    if name.starts_with("weft.") {
        return Err(Error::invalid_name(
            "the weft.* attribute namespace is reserved",
        ));
    }
    Ok(())
}

fn validate_options(options: &Options) -> Result<()> {
    if options.chunk_size == 0 || options.chunk_size > crate::format::MAX_CHUNK_SIZE {
        return Err(Error::invalid_argument("chunk_size must be 1..=16 MiB"));
    }
    if options.checkpoint_interval > 1_000_000 {
        return Err(Error::invalid_argument(
            "checkpoint_interval must be at most 1,000,000",
        ));
    }
    if options.codec_delta_margin_percent > 90 {
        return Err(Error::invalid_argument(
            "codec_delta_margin_percent must be at most 90",
        ));
    }
    Ok(())
}

/// Recreate any layout directory missing from an otherwise valid store. This
/// keeps a git-carried fixture (which cannot represent empty directories)
/// writable without changing the normative layout.
fn ensure_layout(root: &Path) -> Result<()> {
    for relative in [
        "commits",
        "objects",
        "manifests",
        "refs",
        "refs/snapshots",
        "refs/tags",
        "tmp",
    ] {
        make_dir(&root.join(relative))?;
    }
    Ok(())
}

/// Remove unreferenced atomic-write staging files older than the grace period.
/// Only our `blob-`/`snap-` prefixes are considered, so placeholder files in a
/// carried fixture are left alone.
fn clean_stale_tmp(root: &Path, grace_secs: u64) -> Result<()> {
    let dir = root.join("tmp");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
        if !(name.starts_with("blob-") || name.starts_with("snap-")) {
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

fn check_mode(mode: u64) -> Result<()> {
    if mode > 0o7777 {
        return Err(Error::invalid_argument(
            "mode must fit POSIX permission bits (0..=0o7777)",
        ));
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn resolve_in(state: &State, path: &str) -> Result<Hash32> {
    let mut current = State::ROOT_ID;
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    for (index, component) in components.iter().enumerate() {
        if *component == "." {
            continue;
        }
        if *component == ".." {
            return Err(Error::invalid_name(
                "`..` is not accepted at the store layer",
            ));
        }
        let object = state
            .objects
            .get(&current)
            .ok_or_else(|| Error::not_found("no such parent object"))?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("path component is not a directory"));
        }
        current = state.lookup(current, component.as_bytes())?;
        if index + 1 < components.len() {
            let child = state
                .objects
                .get(&current)
                .ok_or_else(|| Error::not_found("no such object"))?;
            if child.kind != ObjectKind::Dir {
                return Err(Error::not_dir("path component is not a directory"));
            }
        }
    }
    Ok(current)
}

fn mark_state(
    root: &Path,
    state: &State,
    manifests: &mut BTreeSet<Hash32>,
    chunks: &mut BTreeSet<Hash32>,
    problems: &mut Vec<String>,
) {
    for record in state.objects.values() {
        // A causal closure record is content-addressed under the manifest
        // domain and is retained exactly as long as the object that names it
        // (in current state, a snapshot, or a checkpoint).
        if let Some(Value::Bytes(bytes)) = record.attrs.get(crate::state::ATTR_CAUSAL_HASH) {
            if let Ok(hash) = Hash32::from_slice(bytes) {
                if hash != Hash32::ZERO {
                    manifests.insert(hash);
                }
            }
        }
        if record.kind != ObjectKind::File {
            continue;
        }
        let mut seen = BTreeSet::new();
        mark_manifest_tree(
            root,
            record.content,
            manifests,
            chunks,
            problems,
            0,
            &mut seen,
        );
    }
}

/// Mark a manifest and everything it physically references: raw chunks, the
/// codec frame blob, and (transitively, bounded) dependency content roots.
fn mark_manifest_tree(
    root: &Path,
    manifest_id: Hash32,
    manifests: &mut BTreeSet<Hash32>,
    chunks: &mut BTreeSet<Hash32>,
    problems: &mut Vec<String>,
    depth: usize,
    seen: &mut BTreeSet<Hash32>,
) {
    if depth > MAX_CODEC_DEPENDENCY_DEPTH {
        problems.push(format!(
            "manifest {manifest_id} exceeds the codec dependency depth bound"
        ));
        return;
    }
    if !seen.insert(manifest_id) {
        problems.push(format!("manifest {manifest_id} participates in a cycle"));
        return;
    }
    manifests.insert(manifest_id);
    match recovery::read_manifest(root, manifest_id) {
        Ok(manifest) => {
            if let Some(rep) = &manifest.representation {
                if rep.kind == RepresentationKind::Codec {
                    if let Some(frame) = rep.frame {
                        manifests.insert(frame);
                    }
                    for dependency in &rep.dependencies {
                        mark_manifest_tree(
                            root,
                            *dependency,
                            manifests,
                            chunks,
                            problems,
                            depth + 1,
                            seen,
                        );
                    }
                    return;
                }
            }
            for chunk in manifest.chunks {
                chunks.insert(chunk.hash);
            }
        }
        Err(err) => problems.push(format!("cannot read manifest {manifest_id}: {err}")),
    }
}

fn collect_unmarked(
    dir: &Path,
    marked: &BTreeSet<Hash32>,
    now: SystemTime,
    grace_secs: u64,
    report: &mut GcReport,
) -> Result<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    let subdirs = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(Error::io(format!("cannot read {}: {err}", dir.display()))),
    };
    for subdir in subdirs {
        let subdir = subdir?;
        if !subdir.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(subdir.path())? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(name) => name,
                None => continue,
            };
            let hash = match Hash32::from_lower_hex(name) {
                Ok(hash) => hash,
                Err(_) => {
                    report.problems.push(format!(
                        "unrecognized blob filename {}",
                        entry.path().display()
                    ));
                    continue;
                }
            };
            let metadata = entry.metadata()?;
            if marked.contains(&hash) {
                report.retained += 1;
                continue;
            }
            if grace_secs > 0 {
                let age = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                if age < grace_secs {
                    report.retained += 1;
                    continue;
                }
            }
            out.push((entry.path(), metadata.len()));
        }
    }
    Ok(out)
}

// Keep the format constants referenced in this module's documentation visible
// to downstream users through the store crate.
pub use crate::format::{FORMAT_MAJOR as STORE_FORMAT_MAJOR, FORMAT_MINOR as STORE_FORMAT_MINOR};
