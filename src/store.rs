//! The WeftFS transactional store core (no FUSE frontend).
//!
//! Layout, commit publication order, recovery, and identity rules follow
//! `spec/WEFT-MODEL.md`. All mutation methods publish exactly one commit. The
//! store is single-writer (`flock`); readers pin the generation they opened.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as Fs2FileExt;
use weftcodec::selector::Selector;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::Principal;
use crate::error::{Error, Result};
use crate::format::{
    encode_commit_file, encode_state, ChunkRef, Commit, Manifest, SuperblockSlot, FORMAT_CONTENTS,
    SLOT_BYTES, SUPERBLOCK_BYTES,
};
use crate::identity::{self, Hash32};
use crate::recovery::{self, BlobKind};
use crate::state::{
    derive_entry_id, derive_version_id, validate_name_bytes, validate_snapshot_name, EntryRecord,
    ObjectKind, ObjectRecord, Op, RelationKind, RelationRecord, RelationTarget, State,
    ATTR_SYMLINK_TARGET, SAVED_CONTEXT_PREDICATE, SAVED_CONTEXT_SELECTOR_ATTR, XATTR_PREFIX,
};

/// Maximum current file size for `write_file`/`truncate` (256 MiB).
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Default chunk size.
pub const DEFAULT_CHUNK_SIZE: u64 = 65_536;
/// Default checkpoint interval (commits).
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 256;
/// Default GC grace period (seconds).
pub const DEFAULT_GC_GRACE_SECS: u64 = 3_600;
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
    /// In-memory open-handle pins by object id. Consulted by `unlink` (an
    /// unlinked object record survives while pinned) and by GC (pinned content
    /// is never swept). Pins are process-local and do not survive a crash.
    pins: BTreeMap<Hash32, u64>,
    /// In-memory content pins by manifest id, for readers that hold a pinned
    /// `(content, size)` pair (`Store::read_content`).
    content_pins: BTreeMap<Hash32, u64>,
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
            pins: BTreeMap::new(),
            content_pins: BTreeMap::new(),
        };
        store.write_genesis()?;
        fsync_dir(&store.root)?;
        Ok(store)
    }

    /// Open an existing store.
    pub fn open(path: impl AsRef<Path>, mode: OpenMode) -> Result<Store> {
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

        let recovery = recovery::recover(&root)?;
        let highest_file_generation = recovery::scan_commit_files(&root)?
            .keys()
            .copied()
            .max()
            .unwrap_or(0);
        let next_generation = recovery
            .generation
            .max(highest_file_generation)
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
            pins: BTreeMap::new(),
            content_pins: BTreeMap::new(),
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
                self.read_manifest_bytes(&manifest, 0, manifest.size, true)
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
                        self.read_manifest_bytes(&manifest, 0, manifest.size, true)
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
        self.read_manifest_bytes(&manifest, offset, length, false)
    }

    /// Read a byte range directly from a content root (manifest id). Safe for
    /// snapshot materialization because it does not consult live object state.
    pub fn read_content(&self, content_root: Hash32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let manifest = recovery::read_manifest(&self.root, content_root)?;
        let whole = offset == 0 && length >= manifest.size;
        self.read_manifest_bytes(&manifest, offset, length, whole)
    }

    fn read_manifest_bytes(
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
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
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
            let local_start = offset.saturating_sub(chunk_start);
            let local_end = (end - chunk_start).min(chunk.len);
            out.extend_from_slice(&bytes[local_start as usize..local_end as usize]);
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
        self.require_writable()?;
        self.require_dir(parent)?;
        validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        check_mode(mode)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded("file exceeds the 256 MiB v1 bound"));
        }
        let (manifest_id, _content_hash, size) = self.store_file_content(bytes)?;
        let object_id = identity::object_id_from_seed(seed);
        if self.state.objects.contains_key(&object_id) {
            return Err(Error::exists("object id already exists"));
        }
        let mtime = self.now_ns();
        let version = derive_version_id(object_id, manifest_id, size, mtime, None)?;
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
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("write_file requires a file object"));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
        if end > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded("file exceeds the 256 MiB v1 bound"));
        }
        let current = self.read_object(id)?;
        if current.len() as u64 != old.size {
            return Err(Error::corrupt("file size changed while writing"));
        }
        let new_size = end.max(old.size);
        let mut buffer = vec![0u8; usize::try_from(new_size).unwrap_or(0)];
        buffer[..current.len()].copy_from_slice(&current);
        buffer[offset as usize..end as usize].copy_from_slice(bytes);
        let (manifest_id, _content, _size) = self.store_file_content(&buffer)?;
        let mtime = self.now_ns();
        let version = derive_version_id(id, manifest_id, new_size, mtime, Some(old.version))?;
        let record = ObjectRecord {
            version,
            mtime_ns: mtime,
            content: manifest_id,
            size: new_size,
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
            return Err(Error::limit_exceeded("file exceeds the 256 MiB v1 bound"));
        }
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("truncate requires a file object"));
        }
        let current = self.read_object(id)?;
        let mut buffer = vec![0u8; usize::try_from(size).unwrap_or(0)];
        let copy = current.len().min(buffer.len());
        buffer[..copy].copy_from_slice(&current[..copy]);
        let (manifest_id, _content, _size) = self.store_file_content(&buffer)?;
        let mtime = self.now_ns();
        let version = derive_version_id(id, manifest_id, size, mtime, Some(old.version))?;
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
        record.version = derive_version_id(
            record.id,
            record.content,
            record.size,
            record.mtime_ns,
            Some(old.version),
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
        let mut out = Vec::new();
        for relation in self.state.relations.values() {
            if !is_saved_context(relation) {
                continue;
            }
            let RelationTarget::Value(Value::Str(name)) = &relation.target else {
                return Err(Error::corrupt("saved_context target is not text"));
            };
            out.push(self.context_from_relation(name, relation)?);
        }
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Ok(out)
    }

    /// Load a saved context selector by name.
    pub fn load_context(&self, name: &str) -> Result<Selector> {
        let existing = self.context_relations(name)?;
        let relation = match existing.as_slice() {
            [relation] => relation,
            [] => return Err(Error::not_found("no such saved context")),
            _ => {
                return Err(Error::corrupt(
                    "more than one saved context relation has the same name",
                ))
            }
        };
        let saved = self.context_from_relation(name, relation)?;
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
        let mut out = Vec::new();
        for relation in self.state.relations.values() {
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

    fn context_from_relation(&self, name: &str, relation: &RelationRecord) -> Result<SavedContext> {
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
        const BLOCK: u64 = 4_096;
        let mut content_bytes: u64 = 0;
        for record in self.state.objects.values() {
            content_bytes = content_bytes.saturating_add(record.size);
        }
        // One metadata block per object plus the content blocks.
        let blocks =
            content_bytes.saturating_add(BLOCK - 1) / BLOCK + self.state.objects.len() as u64;
        StatFs {
            block_size: BLOCK,
            blocks,
            blocks_free: 0,
            blocks_available: 0,
            files: self.state.objects.len() as u64,
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
    pub fn checkpoint(&mut self) -> Result<Hash32> {
        self.require_writable()?;
        self.commit_ops(Vec::new(), true)
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
                    if full {
                        let mut assembled =
                            Vec::with_capacity(usize::try_from(manifest.size).unwrap_or(0));
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
                                    assembled.extend_from_slice(&bytes);
                                }
                                Err(err) => report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!("chunk {}: {err}", chunk.hash),
                                }),
                            }
                        }
                        if assembled.len() as u64 == manifest.size
                            && identity::content_hash(&assembled) != manifest.content_hash
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
            marked_manifests.insert(content);
            match recovery::read_manifest(&self.root, content) {
                Ok(manifest) => {
                    for chunk in &manifest.chunks {
                        marked_chunks.insert(chunk.hash);
                    }
                }
                Err(err) => report
                    .problems
                    .push(format!("pinned content {content} is unreadable: {err}")),
            }
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

    fn store_file_content(&self, bytes: &[u8]) -> Result<(Hash32, Hash32, u64)> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded("file exceeds the 256 MiB v1 bound"));
        }
        let chunk_size = self.options.chunk_size;
        if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
            return Err(Error::invalid_argument("chunk size is out of range"));
        }
        let content_hash = identity::content_hash(bytes);
        let mut chunks = Vec::new();
        let step = usize::try_from(chunk_size)
            .map_err(|_| Error::limit_exceeded("chunk size does not fit usize"))?;
        for chunk in bytes.chunks(step) {
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, chunk)?;
            chunks.push(ChunkRef {
                hash,
                len: chunk.len() as u64,
            });
        }
        let manifest = Manifest {
            size: bytes.len() as u64,
            chunk_size,
            chunks,
            content_hash,
        };
        let wcb = manifest.encode()?;
        if wcb.len() as u64 > crate::format::MAX_CHUNK_SIZE * 64 {
            return Err(Error::limit_exceeded(
                "manifest encoding is implausibly large",
            ));
        }
        let manifest_id = self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &wcb)?;
        Ok((manifest_id, content_hash, bytes.len() as u64))
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
        if record.kind != ObjectKind::File {
            continue;
        }
        manifests.insert(record.content);
        match recovery::read_manifest(root, record.content) {
            Ok(manifest) => {
                for chunk in manifest.chunks {
                    chunks.insert(chunk.hash);
                }
            }
            Err(err) => problems.push(format!(
                "cannot read manifest {} for object {}: {err}",
                record.content, record.id
            )),
        }
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
