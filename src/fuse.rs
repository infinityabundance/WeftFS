//! Linux FUSE frontend for the WeftFS store (P5).
//!
//! The daemon is a thin POSIX adapter over [`crate::store::Store`]. By default
//! every mutation is one store commit, so a `write`/`create`/`mkdir`/`xattr`
//! reply is only sent after the commit (and its fsyncs) completed. Two opt-in
//! mount modes change that, and only that:
//!
//! * `--write-back` buffers each handle's writes and commits them at
//!   `flush`/`fsync`/`release`. Readers other than the writing handle see no
//!   buffered bytes until then; a crash loses unflushed writes; `fsync` is
//!   durable before it replies.
//! * `--group-commit` batches successive independent mutations into one
//!   durable commit within a short window. A group is durable only when it is
//!   published; a crash leaves the previous whole generation.
//!
//! The defaults are unchanged: commit-per-mutation, no buffering. See
//! `docs/fuse-status.md` for the exact durability table.
//!
//! Two virtual surfaces are mounted under `/.weft` (they shadow a real entry of
//! that name, which remains reachable through the exact-id surface):
//!
//! * `/.weft/id/<64 lowercase hex>` — exact-object access, read-only.
//! * `/.weft/contexts/<name>/` — saved-selector result set, read-only, with
//!   deterministic escaped display names suffixed by `--<hex prefix>`.
//!
//! Inode numbers are per-mount. The root ObjectId maps to inode 1; other
//! ObjectIds map deterministically from their first eight bytes with a
//! collision fallback to a per-mount allocator (never aliasing). Virtual nodes
//! are allocated from `VIRTUAL_BASE` upward.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyLock, ReplyLseek, ReplyOpen,
    ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::{self, Principal};
use crate::context::CapabilityContext;
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::load_receipt::{LoadReceipt, LoadReceiptTensor, LoadVerification};
use crate::state::{validate_name_bytes, ObjectKind, ObjectRecord, State};
use crate::store::{ManifestCache, OpenMode, Store, MAX_FILE_BYTES, MAX_XATTR_VALUE_BYTES};

/// Attribute/entry TTL. Zero makes every kernel question reach the daemon, so
/// changes made through another mount are visible immediately. No attribute
/// caching is claimed or performed by the frontend.
const TTL: Duration = Duration::from_secs(0);

const ROOT_INO: u64 = 1;
const WEFT_INO: u64 = 2;
const WEFT_IDS_INO: u64 = 3;
const WEFT_CONTEXTS_INO: u64 = 4;
/// First inode number of the per-mount allocator (collision fallback and all
/// virtual nodes). No ObjectId maps here directly.
const VIRTUAL_BASE: u64 = 0x4000_0000_0000_0000;

const O_ACCMODE: i32 = 0o3;
const O_WRONLY: i32 = 0o1;
const O_RDWR: i32 = 0o2;
const O_TRUNC: i32 = 0o1000;

/// T029: optimistic mode rewrites a whole file per `write` call (read, splice,
/// publish one new version), so it bounds the file it is willing to rewrite.
const OPTIMISTIC_REWRITE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// T029: how many times one optimistic FUSE mutation re-reads the published
/// head after losing a race before it reports `EBUSY`. A conflict is a lost
/// race, not an error: the operation is re-derived from the newer bytes and
/// retried with the same transaction id (so an ambiguous retry is idempotent).
const OPTIMISTIC_ATTEMPTS: u32 = 8;
const O_APPEND: i32 = 0o2000;
const O_EXCL: i32 = 0o200;

const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

const XATTR_CREATE: i32 = 1;
const XATTR_REPLACE: i32 = 2;

const R_OK: i32 = 4;
const W_OK: i32 = 2;
const X_OK: i32 = 1;

/// `FOPEN_DIRECT_IO`: bypass the kernel page cache for read-only handles. Each
/// read is served from one immutable content root, so a reader cannot observe a
/// partially overwritten file. Writable handles use the kernel's write-through
/// path (no writeback cache is negotiated) and every FUSE write is a commit.
/// This also means `fsync` has nothing extra to flush (see docs/fuse-status.md).
const FOPEN_DIRECT_IO: u32 = 1 << 0;

/// Group-commit window: mutations arriving within this window share one
/// durable commit. Short enough that an isolated mutation is published almost
/// immediately, long enough to coalesce a metadata storm.
const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(10);
/// Upper bound on buffered mutations in one group, so a sustained storm cannot
/// grow the pending-op list without bound.
const GROUP_COMMIT_MAX_OPS: usize = 256;
/// Upper bound on bytes buffered by one write-back handle. Reaching it flushes
/// the handle early (an earlier durable commit is always allowed).
const WRITE_BACK_MAX_BYTES: usize = 32 * 1024 * 1024;

const SEEK_DATA: i32 = 3;
const SEEK_HOLE: i32 = 4;

/// Mount-time configuration produced by the CLI.
pub struct MountOptions {
    pub allow_other: bool,
    pub read_only: bool,
    /// Pinned commit for `--snapshot` / `--at` / context mounts.
    pub snapshot: Option<Hash32>,
    /// Replayed state of `snapshot`, cached so lookups do not replay. Shared
    /// views of one server hold clones of one `Arc`, so N views pinned to the
    /// same commit pay for the replay (and the state memory) once.
    pub snapshot_state: Option<Arc<State>>,
    /// Optional capability narrowing for exact-id and saved-context views.
    pub capability: Option<CapabilityContext>,
    /// When present, the mount root is this Context Root's result set.
    pub context: Option<ContextMount>,
    /// Buffer per-handle writes and commit at `flush`/`fsync`/`release`.
    pub write_back: bool,
    /// Batch successive independent mutations into one durable commit within
    /// [`GROUP_COMMIT_WINDOW`].
    pub group_commit: bool,
    /// Verify every tensor file through the load path on `open` and write a
    /// `weft.load.receipt.v1` record per file.
    pub verify_load: bool,
    /// Directory that receives the per-file load receipts.
    pub load_receipt_dir: Option<PathBuf>,
}

/// A resolved Context Root used as the mount root.
#[derive(Clone, Debug)]
pub struct ContextMount {
    /// Stored Context Root name (or the supplied URI).
    pub name: String,
    pub context_root_id: Hash32,
    pub snapshot: Hash32,
    pub selector_id: Hash32,
    /// Ordered, capability-authorized result ids from resolution.
    pub results: Vec<Hash32>,
}

/// A node addressable through the mount.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    /// A live namespace object (writable unless the mount is read-only).
    Object(Hash32),
    /// The mount root when `--context` selected a result set.
    ContextRoot,
    /// `/.weft`.
    WeftRoot,
    /// `/.weft/id`.
    WeftIds,
    /// `/.weft/id/<hex>` and everything below it (read-only).
    WeftIdNode(Hash32),
    /// `/.weft/contexts`.
    WeftContexts,
    /// `/.weft/contexts/<name>`.
    WeftContextDir(String),
    /// A result inside a saved context, read-only, scoped to that context.
    WeftContextNode { context: String, object: Hash32 },
}

impl Node {
    fn object(&self) -> Option<Hash32> {
        match self {
            Node::Object(id) | Node::WeftIdNode(id) => Some(*id),
            Node::WeftContextNode { object, .. } => Some(*object),
            _ => None,
        }
    }

    fn is_virtual(&self) -> bool {
        !matches!(self, Node::Object(_))
    }
}

struct InodeTable {
    next_virtual: u64,
    by_ino: BTreeMap<u64, Node>,
    object: BTreeMap<Hash32, u64>,
    id_view: BTreeMap<Hash32, u64>,
    context_dir: BTreeMap<String, u64>,
    context_view: BTreeMap<String, BTreeMap<Hash32, u64>>,
    parents: BTreeMap<u64, u64>,
    generations: BTreeMap<u64, u64>,
}

impl InodeTable {
    fn new(context_root: bool) -> Self {
        let mut by_ino = BTreeMap::new();
        by_ino.insert(
            ROOT_INO,
            if context_root {
                Node::ContextRoot
            } else {
                Node::Object(State::ROOT_ID)
            },
        );
        by_ino.insert(WEFT_INO, Node::WeftRoot);
        by_ino.insert(WEFT_IDS_INO, Node::WeftIds);
        by_ino.insert(WEFT_CONTEXTS_INO, Node::WeftContexts);
        let mut object = BTreeMap::new();
        if !context_root {
            object.insert(State::ROOT_ID, ROOT_INO);
        }
        InodeTable {
            next_virtual: VIRTUAL_BASE,
            by_ino,
            object,
            id_view: BTreeMap::new(),
            context_dir: BTreeMap::new(),
            context_view: BTreeMap::new(),
            parents: BTreeMap::new(),
            generations: BTreeMap::new(),
        }
    }

    fn alloc_virtual(&mut self) -> u64 {
        let ino = self.next_virtual;
        self.next_virtual = self.next_virtual.saturating_add(1);
        ino
    }

    fn insert(&mut self, ino: u64, node: Node) {
        self.by_ino.insert(ino, node);
    }

    /// Deterministic ObjectId -> ino mapping. The first eight bytes of the id
    /// are the candidate; if it collides with a reserved/occupied number (or
    /// lands in the virtual range) the allocator hands out a fresh value.
    fn ino_object(&mut self, id: Hash32) -> u64 {
        if id == State::ROOT_ID {
            return ROOT_INO;
        }
        if let Some(ino) = self.object.get(&id) {
            return *ino;
        }
        let mut candidate = u64::from_be_bytes(id.as_bytes()[..8].try_into().unwrap());
        if candidate <= WEFT_CONTEXTS_INO
            || candidate >= VIRTUAL_BASE
            || self.by_ino.contains_key(&candidate)
        {
            candidate = self.alloc_virtual();
        }
        self.object.insert(id, candidate);
        self.insert(candidate, Node::Object(id));
        candidate
    }

    fn ino_id_view(&mut self, id: Hash32) -> u64 {
        if let Some(ino) = self.id_view.get(&id) {
            return *ino;
        }
        let ino = self.alloc_virtual();
        self.id_view.insert(id, ino);
        self.insert(ino, Node::WeftIdNode(id));
        ino
    }

    fn ino_context_dir(&mut self, name: &str) -> u64 {
        if let Some(ino) = self.context_dir.get(name) {
            return *ino;
        }
        let ino = self.alloc_virtual();
        self.context_dir.insert(name.to_owned(), ino);
        self.insert(ino, Node::WeftContextDir(name.to_owned()));
        ino
    }

    fn ino_context_node(&mut self, context: &str, object: Hash32) -> u64 {
        if let Some(ino) = self
            .context_view
            .get(context)
            .and_then(|objects| objects.get(&object))
        {
            return *ino;
        }
        let ino = self.alloc_virtual();
        self.context_view
            .entry(context.to_owned())
            .or_default()
            .insert(object, ino);
        self.insert(
            ino,
            Node::WeftContextNode {
                context: context.to_owned(),
                object,
            },
        );
        ino
    }

    fn set_parent(&mut self, ino: u64, parent: u64) {
        self.parents.insert(ino, parent);
    }

    fn parent(&self, ino: u64) -> Option<u64> {
        self.parents.get(&ino).copied()
    }

    fn generation(&self, ino: u64) -> u64 {
        self.generations.get(&ino).copied().unwrap_or(1)
    }

    fn bump_generation(&mut self, ino: u64) {
        let next = self.generation(ino).saturating_add(1);
        self.generations.insert(ino, next);
    }

    fn node_of(&self, ino: u64) -> Option<Node> {
        self.by_ino.get(&ino).cloned()
    }
}

struct OpenHandle {
    ino: u64,
    /// `Some` for live namespace objects (writable); `None` for scoped,
    /// read-only virtual views.
    object: Option<Hash32>,
    content: Hash32,
    size: u64,
    kind: ObjectKind,
    append: bool,
    pinned_object: bool,
    pinned_content: bool,
    /// Buffered writes for `--write-back`, in call order. Empty otherwise.
    pending: Vec<PendingWrite>,
    /// Last manifest this handle produced or read, reused by the next write so
    /// a long write sequence does not re-read the manifest each time.
    manifest: Option<ManifestCache>,
}

struct PendingWrite {
    offset: u64,
    data: Vec<u8>,
}

impl OpenHandle {
    /// End (exclusive) of the buffered region, or the committed size.
    fn pending_end(&self) -> u64 {
        self.pending
            .iter()
            .map(|write| write.offset + write.data.len() as u64)
            .fold(self.size, u64::max)
    }

    fn pending_bytes(&self) -> usize {
        self.pending.iter().map(|write| write.data.len()).sum()
    }
}

struct DirItem {
    name: Vec<u8>,
    ino: u64,
    attr: FileAttr,
    generation: u64,
}

struct DirHandle {
    ino: u64,
    items: Vec<DirItem>,
    self_attr: FileAttr,
    parent_attr: FileAttr,
    generation: u64,
}

/// The mounted filesystem.
/// How a [`WeftFuse`] view reaches the store.
///
/// A single mount owns an exclusive handle (`Arc<Mutex<Store>>`) because it may
/// write. A shared read-only server owns exactly one [`Store`] opened
/// `ReadOnly` and hands every view an `Arc<Store>`; reads borrow it immutably
/// (the read APIs already take `&self` and `Store` is `Sync`). The variants are
/// deliberately not interchangeable: a shared view can never obtain `&mut
/// Store`, so it cannot mutate, pin, or publish.
enum StoreHandle {
    Exclusive(Arc<Mutex<Store>>),
    Shared(Arc<Store>),
}

/// Immutable borrow of the store held by one callback.
struct StoreReadGuard<'a>(GuardInner<'a>);

enum GuardInner<'a> {
    Exclusive(std::sync::MutexGuard<'a, Store>),
    Shared(&'a Store),
}

impl std::ops::Deref for StoreReadGuard<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        match &self.0 {
            GuardInner::Exclusive(guard) => guard,
            GuardInner::Shared(store) => store,
        }
    }
}

/// Exclusive borrow. Only an exclusive (single-mount) view can obtain one.
struct StoreWriteGuard<'a>(std::sync::MutexGuard<'a, Store>);

impl std::ops::Deref for StoreWriteGuard<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.0
    }
}

impl std::ops::DerefMut for StoreWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.0
    }
}

impl StoreHandle {
    /// Borrow the store for a read. Never blocks on a shared view beyond the
    /// store's own interior locks.
    fn read(&self) -> StoreReadGuard<'_> {
        match self {
            StoreHandle::Exclusive(store) => StoreReadGuard(GuardInner::Exclusive(
                store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )),
            StoreHandle::Shared(store) => StoreReadGuard(GuardInner::Shared(store)),
        }
    }

    /// Borrow the store for a mutation. A shared read-only view refuses with
    /// the same `ReadOnly` class the store itself uses, which maps to `EROFS`.
    fn write(&self) -> Result<StoreWriteGuard<'_>> {
        match self {
            StoreHandle::Exclusive(store) => Ok(StoreWriteGuard(
                store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )),
            StoreHandle::Shared(_) => Err(Error::read_only(
                "shared read-only mount views refuse mutations; use a single writable mount",
            )),
        }
    }

    fn is_shared(&self) -> bool {
        matches!(self, StoreHandle::Shared(_))
    }
}

/// Deterministic hexadecimal for a byte string (history/transaction seeds).
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A deterministic 32-hex transaction id for one FUSE mutation.
///
/// The same operation on the same observed input derives the same id, so a
/// retry after an ambiguous acknowledgement is resolved by the shipped outcome
/// lookup instead of duplicating the effect. Different operations differ in
/// `op` and in the seed's observed fields.
fn fuse_transaction_id(op: &str, seed: &str) -> String {
    crate::identity::h("weft/fuse/tx/v1", format!("{op}:{seed}").as_bytes()).to_hex()[..32]
        .to_owned()
}

pub struct WeftFuse {
    store: StoreHandle,
    read_only: bool,
    snapshot: Option<Hash32>,
    snapshot_state: Option<Arc<State>>,
    capability: Option<CapabilityContext>,
    context: Option<ContextMount>,
    write_back: bool,
    group_commit: bool,
    /// T029: mutations are published through the MW2 transaction API. Off
    /// unless the store was opened `--writer-mode optimistic`.
    optimistic: bool,
    verify_load: bool,
    load_receipt_dir: Option<PathBuf>,
    /// Open group-commit session; `None` while no group is buffering.
    group: Arc<Mutex<Option<GroupSession>>>,
    group_stop: Arc<AtomicBool>,
    group_thread: Option<thread::JoinHandle<()>>,
    /// Set when publishing a group failed after its callbacks already replied;
    /// every later mutating operation fails with EIO instead of pretending the
    /// store is writable.
    publish_failed: Arc<AtomicBool>,
    inodes: Mutex<InodeTable>,
    handles: Mutex<BTreeMap<u64, OpenHandle>>,
    dirs: Mutex<BTreeMap<u64, DirHandle>>,
    next_fh: AtomicU64,
}

struct GroupSession {
    opened_at: std::time::Instant,
    ops: usize,
}

impl WeftFuse {
    pub fn new(store: Store, options: &MountOptions) -> Self {
        Self::build(StoreHandle::Exclusive(Arc::new(Mutex::new(store))), options)
    }

    /// Build one view of a shared read-only server. `store` is the server's
    /// single `ReadOnly` handle; this view holds no mutable access to it.
    pub fn new_shared(store: Arc<Store>, options: &MountOptions) -> Self {
        Self::build(StoreHandle::Shared(store), options)
    }

    fn build(store: StoreHandle, options: &MountOptions) -> Self {
        let exclusive = !store.is_shared();
        let group: Arc<Mutex<Option<GroupSession>>> = Arc::new(Mutex::new(None));
        let group_stop = Arc::new(AtomicBool::new(false));
        let publish_failed = Arc::new(AtomicBool::new(false));
        let optimistic = match &store {
            StoreHandle::Exclusive(store) => {
                let mode = store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .writer_mode();
                mode == crate::mw2::WriterMode::Optimistic
            }
            StoreHandle::Shared(_) => false,
        };
        let group_thread = if options.group_commit && !options.read_only && exclusive {
            Some(spawn_group_committer(
                match &store {
                    StoreHandle::Exclusive(store) => Arc::clone(store),
                    StoreHandle::Shared(_) => {
                        unreachable!("group commit requires an exclusive view")
                    }
                },
                Arc::clone(&group),
                Arc::clone(&group_stop),
                Arc::clone(&publish_failed),
            ))
        } else {
            None
        };
        WeftFuse {
            store,
            read_only: options.read_only,
            snapshot: options.snapshot,
            snapshot_state: options.snapshot_state.clone(),
            capability: options.capability.clone(),
            context: options.context.clone(),
            write_back: options.write_back && !options.read_only,
            group_commit: options.group_commit && !options.read_only,
            optimistic,
            verify_load: options.verify_load,
            load_receipt_dir: options.load_receipt_dir.clone(),
            group,
            group_stop,
            group_thread,
            publish_failed,
            inodes: Mutex::new(InodeTable::new(options.context.is_some())),
            handles: Mutex::new(BTreeMap::new()),
            dirs: Mutex::new(BTreeMap::new()),
            next_fh: AtomicU64::new(1),
        }
    }

    /// Join the open group-commit session, opening one if needed. Must be
    /// called while holding the store lock, immediately before the first
    /// mutation of a callback.
    fn begin_grouped_mutation(&self, store: &mut Store) -> Result<()> {
        if !self.group_commit || self.read_only {
            return Ok(());
        }
        if self.publish_failed.load(Ordering::SeqCst) {
            return Err(Error::io(
                "a previous group commit failed to publish; remount to continue",
            ));
        }
        if !store.group_open() {
            store.begin_group()?;
            *self.group.lock().unwrap() = Some(GroupSession {
                opened_at: std::time::Instant::now(),
                ops: 0,
            });
        }
        Ok(())
    }

    /// Count a finished mutation and publish the group when it is full or the
    /// window elapsed. Must be called while holding the store lock, before
    /// replying.
    fn finish_grouped_mutation(&self, store: &mut Store) -> Result<()> {
        if !self.group_commit || self.read_only {
            return Ok(());
        }
        let should_publish = {
            let mut session = self.group.lock().unwrap();
            match session.as_mut() {
                Some(current) => {
                    current.ops = current.ops.saturating_add(1);
                    current.ops >= GROUP_COMMIT_MAX_OPS
                        || current.opened_at.elapsed() >= GROUP_COMMIT_WINDOW
                }
                None => false,
            }
        };
        if should_publish {
            self.publish_group(store)?;
        }
        Ok(())
    }

    /// Publish the open group (if any) and close the session. Must be called
    /// while holding the store lock.
    fn publish_group(&self, store: &mut Store) -> Result<()> {
        if !self.group_commit || self.read_only || !store.group_open() {
            *self.group.lock().unwrap() = None;
            return Ok(());
        }
        let result = store.commit_group();
        *self.group.lock().unwrap() = None;
        match result {
            Ok(_) => Ok(()),
            Err(err) => {
                self.publish_failed.store(true, Ordering::SeqCst);
                eprintln!("weftfs: group commit failed to publish: {err}");
                Err(err)
            }
        }
    }

    /// Publish any open group and drop every buffered write for `ino` after
    /// committing it. Used before `truncate`/`unlink`/`rename` so a later
    /// flush cannot resurrect data into a namespace object that changed.
    fn flush_pending_for_ino(&self, store: &mut Store, ino: u64) -> Result<()> {
        let targets: Vec<u64> = self
            .handles
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, handle)| handle.ino == ino && !handle.pending.is_empty())
            .map(|(fh, _)| *fh)
            .collect();
        for fh in targets {
            self.flush_pending(store, fh)?;
        }
        Ok(())
    }

    /// T029: truncate a file to zero as an optimistic transaction.
    ///
    /// A lost race is retried against the newer head with the same transaction
    /// id; there is no read to re-derive (the target is always empty), so the
    /// retry is just a re-read of the precondition.
    fn optimistic_truncate(&self, store: &mut Store, id: Hash32, tx_id: &str) -> Result<()> {
        let mut attempts: u32 = 0;
        loop {
            attempts = attempts.saturating_add(1);
            match store.txn_overwrite_file(id, b"", tx_id) {
                Err(err) if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS => {
                    store.refresh_to_head()?;
                }
                other => return other,
            }
        }
    }

    /// Commit every buffered handle. Used before namespace mutations that can
    /// move or remove the target, so a flush cannot write into a stale object.
    fn flush_all_pending(&self, store: &mut Store) -> Result<()> {
        let targets: Vec<u64> = self
            .handles
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, handle)| !handle.pending.is_empty())
            .map(|(fh, _)| *fh)
            .collect();
        for fh in targets {
            self.flush_pending(store, fh)?;
        }
        Ok(())
    }

    /// Commit one handle's buffered writes as one durable commit.
    fn flush_pending(&self, store: &mut Store, fh: u64) -> Result<()> {
        // A write-back flush promises durability, so an older open group is
        // published first and the flush itself is published before replying.
        // This runs before the buffer is drained, so a failed publish leaves
        // the previously open group's state unchanged and no data is lost.
        self.publish_group(store)?;
        let (id, ranges) = {
            let mut handles = self.handles.lock().unwrap();
            let Some(handle) = handles.get_mut(&fh) else {
                return Err(Error::invalid_argument("unknown file handle"));
            };
            if handle.pending.is_empty() {
                return Ok(());
            }
            let Some(id) = handle.object else {
                return Err(Error::invalid_argument(
                    "read-only handle cannot buffer writes",
                ));
            };
            let ranges: Vec<(u64, Vec<u8>)> = handle
                .pending
                .drain(..)
                .map(|write| (write.offset, write.data))
                .collect();
            (id, ranges)
        };
        let borrowed: Vec<(u64, &[u8])> = ranges
            .iter()
            .map(|(offset, data)| (*offset, data.as_slice()))
            .collect();
        let outcome = match store.write_file_ranges(id, &borrowed) {
            Ok(outcome) => outcome,
            Err(err) => {
                // The data is still only in memory; put it back so the caller
                // can retry, and report the failure.
                if let Some(handle) = self.handles.lock().unwrap().get_mut(&fh) {
                    handle.pending = ranges
                        .into_iter()
                        .map(|(offset, data)| PendingWrite { offset, data })
                        .collect();
                }
                return Err(err);
            }
        };
        if let Some(handle) = self.handles.lock().unwrap().get_mut(&fh) {
            handle.content = outcome.content;
            handle.size = outcome.size;
            handle.manifest = None;
        }
        Ok(())
    }

    fn next_fh(&self) -> u64 {
        self.next_fh.fetch_add(1, Ordering::Relaxed)
    }

    fn state<'a>(&'a self, store: &'a Store) -> &'a State {
        match &self.snapshot_state {
            Some(state) => state.as_ref(),
            None => store.current_state(),
        }
    }

    fn view_snapshot(&self, store: &Store) -> Hash32 {
        self.snapshot.unwrap_or_else(|| store.head())
    }

    fn read_only_node(&self, node: &Node) -> bool {
        self.read_only || node.is_virtual()
    }

    fn attr_for(
        &self,
        table: &mut InodeTable,
        state: &State,
        node: &Node,
        ino: u64,
    ) -> Result<FileAttr> {
        let root_owner = owner_of(state);
        match node {
            Node::Object(id) | Node::WeftIdNode(id) => {
                let record = state
                    .objects
                    .get(id)
                    .ok_or_else(|| Error::not_found("no such object"))?;
                Ok(attr_from_record(
                    ino,
                    record,
                    state.nlink(*id).max(1),
                    table.generation(ino),
                ))
            }
            Node::WeftContextNode { object, .. } => {
                let record = state
                    .objects
                    .get(object)
                    .ok_or_else(|| Error::not_found("no such object"))?;
                Ok(attr_from_record(
                    ino,
                    record,
                    state.nlink(*object).max(1),
                    table.generation(ino),
                ))
            }
            Node::WeftRoot | Node::WeftIds | Node::WeftContexts => {
                Ok(virtual_dir_attr(ino, root_owner, root_mtime(state), 0o555))
            }
            Node::WeftContextDir(_) | Node::ContextRoot => {
                Ok(virtual_dir_attr(ino, root_owner, root_mtime(state), 0o555))
            }
        }
    }

    fn attr_for_ino(
        &self,
        table: &mut InodeTable,
        state: &State,
        ino: u64,
    ) -> Result<(FileAttr, u64)> {
        let node = table
            .node_of(ino)
            .ok_or_else(|| Error::not_found("no inode in this mount"))?;
        let attr = self.attr_for(table, state, &node, ino)?;
        Ok((attr, table.generation(ino)))
    }

    /// Deterministic names for the context mount's authorized results. The
    /// same helper feeds `readdir` and `lookup`, so they cannot disagree.
    fn context_entries(&self, state: &State, principal: &Principal) -> Vec<(Vec<u8>, Hash32)> {
        let Some(context) = &self.context else {
            return Vec::new();
        };
        let mut records: Vec<(Hash32, &ObjectRecord)> = Vec::new();
        for id in &context.results {
            if let Some(record) = state.objects.get(id) {
                if auth::can_read(record, principal) {
                    records.push((*id, record));
                }
            }
        }
        context_entry_names(&records, state)
    }

    /// Build the stable directory listing for a node. The listing is computed
    /// once at `opendir` so cookies stay stable for the handle.
    fn list_dir(
        &self,
        table: &mut InodeTable,
        store: &Store,
        state: &State,
        node: &Node,
        ino: u64,
        principal: &Principal,
    ) -> Result<Vec<DirItem>> {
        match node {
            Node::Object(id) => {
                let object = state
                    .objects
                    .get(id)
                    .ok_or_else(|| Error::not_found("no such directory"))?;
                if object.kind != ObjectKind::Dir {
                    return Err(Error::not_dir("not a directory"));
                }
                let mut items = Vec::new();
                for entry in state.list_dir(*id)? {
                    if *id == State::ROOT_ID && entry.name == b".weft" {
                        // The virtual surface shadows a real `.weft` entry; the
                        // real object stays reachable through `/.weft/id`.
                        continue;
                    }
                    let Some(record) = state.objects.get(&entry.target) else {
                        return Err(Error::corrupt("directory entry has no target object"));
                    };
                    if !auth::acl_allows(record, principal) {
                        continue;
                    }
                    let child_ino = table.ino_object(entry.target);
                    table.set_parent(child_ino, ino);
                    items.push(DirItem {
                        name: entry.name,
                        ino: child_ino,
                        attr: attr_from_record(
                            child_ino,
                            record,
                            state.nlink(entry.target).max(1),
                            table.generation(child_ino),
                        ),
                        generation: table.generation(child_ino),
                    });
                }
                if *id == State::ROOT_ID {
                    table.set_parent(WEFT_INO, ROOT_INO);
                    let attr = self.attr_for(table, state, &Node::WeftRoot, WEFT_INO)?;
                    items.push(DirItem {
                        name: b".weft".to_vec(),
                        ino: WEFT_INO,
                        attr,
                        generation: table.generation(WEFT_INO),
                    });
                }
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
            Node::ContextRoot => {
                let context = self
                    .context
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("context mount has no context root"))?;
                let mut items = Vec::new();
                for (name, object) in self.context_entries(state, principal) {
                    let child_ino = table.ino_context_node(&context.name, object);
                    table.set_parent(child_ino, ino);
                    let attr = self.attr_for(
                        table,
                        state,
                        &Node::WeftContextNode {
                            context: context.name.clone(),
                            object,
                        },
                        child_ino,
                    )?;
                    items.push(DirItem {
                        name,
                        ino: child_ino,
                        attr,
                        generation: table.generation(child_ino),
                    });
                }
                // Keep the exact-id surface reachable from a context mount. It
                // is a constant name, so it never changes the result listing.
                table.set_parent(WEFT_INO, ROOT_INO);
                let attr = self.attr_for(table, state, &Node::WeftRoot, WEFT_INO)?;
                items.push(DirItem {
                    name: b".weft".to_vec(),
                    ino: WEFT_INO,
                    attr,
                    generation: table.generation(WEFT_INO),
                });
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
            Node::WeftRoot => {
                let mut items = Vec::new();
                for (ino, node, name) in [
                    (WEFT_CONTEXTS_INO, Node::WeftContexts, b"contexts".to_vec()),
                    (WEFT_IDS_INO, Node::WeftIds, b"id".to_vec()),
                ] {
                    table.set_parent(ino, WEFT_INO);
                    let attr = self.attr_for(table, state, &node, ino)?;
                    items.push(DirItem {
                        name,
                        ino,
                        attr,
                        generation: table.generation(ino),
                    });
                }
                Ok(items)
            }
            Node::WeftIds => {
                let mut items = Vec::new();
                let now = store.clock_now_ns();
                for (id, record) in &state.objects {
                    if !auth::can_read(record, principal) {
                        continue;
                    }
                    if self.capability.is_some()
                        && store
                            .exact_id_object_at(
                                state,
                                *id,
                                self.capability.as_ref(),
                                principal,
                                now,
                            )
                            .is_err()
                    {
                        continue;
                    }
                    let child_ino = table.ino_id_view(*id);
                    table.set_parent(child_ino, ino);
                    let attr = attr_from_record(
                        child_ino,
                        record,
                        state.nlink(*id).max(1),
                        table.generation(child_ino),
                    );
                    items.push(DirItem {
                        name: id.to_hex().into_bytes(),
                        ino: child_ino,
                        attr,
                        generation: table.generation(child_ino),
                    });
                }
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
            Node::WeftContexts => {
                let root = state
                    .objects
                    .get(&State::ROOT_ID)
                    .ok_or_else(|| Error::corrupt("namespace root is missing"))?;
                if !auth::can_read(root, principal) {
                    return Ok(Vec::new());
                }
                let mut items = Vec::new();
                for saved in store.visible_saved_contexts(
                    state,
                    self.capability.as_ref(),
                    principal,
                    store.clock_now_ns(),
                )? {
                    let child_ino = table.ino_context_dir(&saved.name);
                    table.set_parent(child_ino, ino);
                    let attr = self.attr_for(
                        table,
                        state,
                        &Node::WeftContextDir(saved.name.clone()),
                        child_ino,
                    )?;
                    items.push(DirItem {
                        name: saved.name.into_bytes(),
                        ino: child_ino,
                        attr,
                        generation: table.generation(child_ino),
                    });
                }
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
            Node::WeftContextDir(context) => {
                let root = state
                    .objects
                    .get(&State::ROOT_ID)
                    .ok_or_else(|| Error::corrupt("namespace root is missing"))?;
                if !auth::can_read(root, principal) {
                    return Err(Error::unauthorized(
                        "principal cannot read the namespace root",
                    ));
                }
                let selector = store.load_context_at(state, context)?;
                let resolution = store.resolve_saved_context_with_capability(
                    &selector,
                    self.view_snapshot(store),
                    state,
                    principal,
                    &Limits::default(),
                    self.capability.as_ref(),
                    store.clock_now_ns(),
                )?;
                let mut records = Vec::new();
                for result in &resolution.results {
                    let record = state
                        .objects
                        .get(&result.object_id)
                        .ok_or_else(|| Error::corrupt("selector result has no object record"))?;
                    records.push((result.object_id, record));
                }
                let mut items = Vec::new();
                for (name, object) in context_entry_names(&records, state) {
                    let child_ino = table.ino_context_node(context, object);
                    table.set_parent(child_ino, ino);
                    let attr = self.attr_for(
                        table,
                        state,
                        &Node::WeftContextNode {
                            context: context.clone(),
                            object,
                        },
                        child_ino,
                    )?;
                    items.push(DirItem {
                        name,
                        ino: child_ino,
                        attr,
                        generation: table.generation(child_ino),
                    });
                }
                Ok(items)
            }
            Node::WeftIdNode(id) | Node::WeftContextNode { object: id, .. } => {
                let record = state
                    .objects
                    .get(id)
                    .ok_or_else(|| Error::not_found("no such object"))?;
                if record.kind != ObjectKind::Dir {
                    return Err(Error::not_dir("not a directory"));
                }
                let scoped = match node {
                    Node::WeftContextNode { context, .. } => Some(context.clone()),
                    _ => None,
                };
                let mut items = Vec::new();
                for entry in state.list_dir(*id)? {
                    let Some(child) = state.objects.get(&entry.target) else {
                        return Err(Error::corrupt("directory entry has no target object"));
                    };
                    if !auth::can_read(child, principal) {
                        continue;
                    }
                    let child_ino = match &scoped {
                        Some(context) => table.ino_context_node(context, entry.target),
                        None => table.ino_id_view(entry.target),
                    };
                    table.set_parent(child_ino, ino);
                    let attr = attr_from_record(
                        child_ino,
                        child,
                        state.nlink(entry.target).max(1),
                        table.generation(child_ino),
                    );
                    items.push(DirItem {
                        name: entry.name,
                        ino: child_ino,
                        attr,
                        generation: table.generation(child_ino),
                    });
                }
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
        }
    }

    /// Resolve one child name inside `parent`. readdir and lookup both go
    /// through the same listing for context directories, so they always agree.
    fn lookup_child(
        &self,
        table: &mut InodeTable,
        store: &Store,
        state: &State,
        parent: &Node,
        parent_ino: u64,
        name: &[u8],
        principal: &Principal,
    ) -> Result<Node> {
        match parent {
            Node::Object(id) => {
                if *id == State::ROOT_ID && name == b".weft" {
                    table.set_parent(WEFT_INO, ROOT_INO);
                    return Ok(Node::WeftRoot);
                }
                let target = state.lookup(*id, name)?;
                let record = state
                    .objects
                    .get(&target)
                    .ok_or_else(|| Error::corrupt("directory entry has no target object"))?;
                if !auth::acl_allows(record, principal) {
                    // Denial is indistinguishable from absence for ACLs.
                    return Err(Error::not_found("no such directory entry"));
                }
                let child_ino = table.ino_object(target);
                table.set_parent(child_ino, parent_ino);
                Ok(Node::Object(target))
            }
            Node::ContextRoot => {
                let items = self.list_dir(table, store, state, parent, parent_ino, principal)?;
                let item = items
                    .iter()
                    .find(|item| item.name == name)
                    .ok_or_else(|| Error::not_found("no such context entry"))?;
                match table.node_of(item.ino) {
                    Some(node) => Ok(node),
                    None => Err(Error::corrupt("directory entry has no inode")),
                }
            }
            Node::WeftRoot => match name {
                b"id" => {
                    table.set_parent(WEFT_IDS_INO, WEFT_INO);
                    Ok(Node::WeftIds)
                }
                b"contexts" => {
                    table.set_parent(WEFT_CONTEXTS_INO, WEFT_INO);
                    Ok(Node::WeftContexts)
                }
                _ => Err(Error::not_found("no such directory entry")),
            },
            Node::WeftIds => {
                let text =
                    std::str::from_utf8(name).map_err(|_| Error::not_found("no such object id"))?;
                if text.len() != 64
                    || !text
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(Error::not_found("no such object id"));
                }
                let id =
                    Hash32::from_hex(text).map_err(|_| Error::not_found("no such object id"))?;
                store
                    .exact_id_object_at(
                        state,
                        id,
                        self.capability.as_ref(),
                        principal,
                        store.clock_now_ns(),
                    )
                    .map_err(|_| Error::not_found("no such object id"))?;
                let child_ino = table.ino_id_view(id);
                table.set_parent(child_ino, parent_ino);
                Ok(Node::WeftIdNode(id))
            }
            Node::WeftContexts => {
                let name =
                    std::str::from_utf8(name).map_err(|_| Error::not_found("no such context"))?;
                let contexts = store.visible_saved_contexts(
                    state,
                    self.capability.as_ref(),
                    principal,
                    store.clock_now_ns(),
                )?;
                if !contexts.iter().any(|context| context.name == name) {
                    return Err(Error::not_found("no such context"));
                }
                let root = state
                    .objects
                    .get(&State::ROOT_ID)
                    .ok_or_else(|| Error::corrupt("namespace root is missing"))?;
                if !auth::can_read(root, principal) {
                    return Err(Error::not_found("no such context"));
                }
                let child_ino = table.ino_context_dir(name);
                table.set_parent(child_ino, parent_ino);
                Ok(Node::WeftContextDir(name.to_owned()))
            }
            Node::WeftContextDir(_context) => {
                let items = self.list_dir(table, store, state, parent, parent_ino, principal)?;
                let item = items
                    .iter()
                    .find(|item| item.name == name)
                    .ok_or_else(|| Error::not_found("no such context entry"))?;
                match table.node_of(item.ino) {
                    Some(node) => Ok(node),
                    None => Err(Error::corrupt("directory entry has no inode")),
                }
            }
            Node::WeftIdNode(_) | Node::WeftContextNode { .. } => {
                let items = self.list_dir(table, store, state, parent, parent_ino, principal)?;
                let item = items
                    .iter()
                    .find(|item| item.name == name)
                    .ok_or_else(|| Error::not_found("no such directory entry"))?;
                match table.node_of(item.ino) {
                    Some(node) => Ok(node),
                    None => Err(Error::corrupt("directory entry has no inode")),
                }
            }
        }
    }
}

fn owner_of(state: &State) -> (u32, u32) {
    state
        .objects
        .get(&State::ROOT_ID)
        .map(|record| (clamp_u32(record.uid), clamp_u32(record.gid)))
        .unwrap_or((0, 0))
}

fn root_mtime(state: &State) -> SystemTime {
    state
        .objects
        .get(&State::ROOT_ID)
        .map(|record| system_time(record.mtime_ns))
        .unwrap_or(UNIX_EPOCH)
}

fn clamp_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn system_time(ns: i64) -> SystemTime {
    if ns >= 0 {
        UNIX_EPOCH
            .checked_add(Duration::from_nanos(ns as u64))
            .unwrap_or(UNIX_EPOCH)
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_nanos(ns.unsigned_abs()))
            .unwrap_or(UNIX_EPOCH)
    }
}

fn file_type(kind: ObjectKind) -> FileType {
    match kind {
        ObjectKind::File => FileType::RegularFile,
        ObjectKind::Dir => FileType::Directory,
        ObjectKind::Symlink => FileType::Symlink,
    }
}

fn attr_from_record(ino: u64, record: &ObjectRecord, nlink: u64, generation: u64) -> FileAttr {
    let _ = generation;
    let time = system_time(record.mtime_ns);
    let size = match record.kind {
        ObjectKind::Dir => 0,
        _ => record.size,
    };
    FileAttr {
        ino,
        size,
        blocks: (size.saturating_add(511)) / 512,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: file_type(record.kind),
        perm: (record.mode & 0o7777) as u16,
        nlink: clamp_u32(nlink),
        uid: clamp_u32(record.uid),
        gid: clamp_u32(record.gid),
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn virtual_dir_attr(ino: u64, owner: (u32, u32), mtime: SystemTime, mode: u16) -> FileAttr {
    FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: mtime,
        mtime,
        ctime: mtime,
        crtime: mtime,
        kind: FileType::Directory,
        perm: mode,
        nlink: 2,
        uid: owner.0,
        gid: owner.1,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn errno(error: &Error) -> i32 {
    match error.class() {
        Class::NotFound => libc::ENOENT,
        Class::Exists => libc::EEXIST,
        Class::NotDir => libc::ENOTDIR,
        Class::IsDir => libc::EISDIR,
        Class::NotEmpty => libc::ENOTEMPTY,
        Class::ReadOnly => libc::EROFS,
        Class::Unauthorized => libc::EACCES,
        Class::InvalidName | Class::InvalidArgument | Class::Parse => libc::EINVAL,
        Class::LimitExceeded => libc::EFBIG,
        Class::Busy => libc::EBUSY,
        // A lost optimistic race that exhausted the bounded retry budget is a
        // transient condition, not corruption: report `EBUSY` so a caller can
        // distinguish it from the `EIO` a genuinely broken store returns.
        Class::Conflict => libc::EBUSY,
        // A representation this mode deliberately does not implement (for
        // example `RENAME_EXCHANGE` under optimistic mutations) is a
        // documented refusal, so report POSIX `EOPNOTSUPP` rather than `EIO`:
        // the caller can then tell "not supported here" from "the filesystem
        // broke".
        Class::UnsupportedRepresentation => libc::EOPNOTSUPP,
        Class::Io => libc::EIO,
        _ => libc::EIO,
    }
}

fn principal_of(req: &Request<'_>) -> Principal {
    // FUSE does not forward supplementary groups. Context and exact-id views
    // therefore authorize with uid, primary gid, and `weft.acl.read`.
    Principal::new(req.uid() as u64, req.gid() as u64, Vec::new())
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

fn time_or_now_ns(value: &TimeOrNow) -> i64 {
    match value {
        TimeOrNow::Now => now_ns(),
        TimeOrNow::SpecificTime(time) => match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
            Err(err) => -i64::try_from(err.duration().as_nanos()).unwrap_or(i64::MAX),
        },
    }
}

/// Percent-encode a raw name component so it is printable, has no `/` or NUL,
/// and is never `.`/`..`. Bytes outside printable ASCII, `%`, and `\` are
/// escaped; the result is deterministic and byte-reversible.
fn escape_component(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'%' => out.extend_from_slice(b"%25"),
            b'/' => out.extend_from_slice(b"%2F"),
            0x20..=0x7e => out.push(byte),
            _ => out.extend_from_slice(format!("%{byte:02X}").as_bytes()),
        }
    }
    if out.is_empty() {
        out.extend_from_slice(b"object");
    }
    if out == b"." || out == b".." {
        out = b"%2E".to_vec();
    }
    out
}

/// Deterministic display names for a context result set: escaped basename of
/// the object's first path, then `--<8+ hex prefix of the ObjectId>`. Prefixes
/// are extended in 8-character steps if two results would collide, so no
/// result is silently dropped.
fn context_entry_names(
    records: &[(Hash32, &ObjectRecord)],
    state: &State,
) -> Vec<(Vec<u8>, Hash32)> {
    let mut ordered: Vec<(Hash32, Vec<u8>)> = records
        .iter()
        .map(|(id, _)| {
            let base = state
                .path_for(*id)
                .and_then(|path| {
                    path.rsplit(|&byte| byte == b'/')
                        .next()
                        .map(|name| name.to_vec())
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| b"object".to_vec());
            (*id, escape_component(&base))
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut used: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut out = Vec::with_capacity(ordered.len());
    for (id, base) in ordered {
        let hex = id.to_hex();
        let mut prefix = 8usize;
        let name = loop {
            let suffix = format!("--{}", &hex[..prefix.min(hex.len())]);
            let max_base = 255usize.saturating_sub(suffix.len());
            let mut candidate = base.clone();
            if candidate.len() > max_base {
                candidate.truncate(max_base);
            }
            candidate.extend_from_slice(suffix.as_bytes());
            if !used.contains(&candidate) || prefix >= 64 {
                break candidate;
            }
            prefix = (prefix + 8).min(64);
        };
        used.insert(name.clone());
        out.push((name, id));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Minimal stderr logger for fuser's warnings. A dropped reply or an
/// unsupported operation is a protocol event that must not be silent.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("weftfs[{}]: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static STDERR_LOGGER: StderrLogger = StderrLogger;

fn init_diagnostics() {
    let _ = log::set_logger(&STDERR_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
}

/// Mount `store` at `mountpoint` and serve until SIGINT/SIGTERM or unmount.
pub fn run(store: Store, mountpoint: PathBuf, options: MountOptions) -> Result<()> {
    init_diagnostics();
    // T029: the buffered paths publish outside the transaction API, so they are
    // incompatible with optimistic publication. Refuse at mount time instead of
    // silently disabling either mechanism.
    if store.writer_mode() == crate::mw2::WriterMode::Optimistic {
        if options.write_back {
            return Err(Error::invalid_argument(
                "optimistic (MW2) mutations and --write-back cannot be combined: write-back \
                 publishes buffered ranges outside the transaction API",
            ));
        }
        if options.group_commit {
            return Err(Error::invalid_argument(
                "optimistic (MW2) mutations and --group-commit cannot be combined: a group \
                 commit publishes one commit outside the per-operation transaction",
            ));
        }
    }
    if std::fs::metadata("/dev/fuse").is_err() {
        return Err(Error::io(
            "cannot open /dev/fuse: the FUSE device is unavailable in this environment",
        ));
    }
    if !mountpoint.is_dir() {
        return Err(Error::invalid_argument(format!(
            "mountpoint {} is not an existing directory",
            mountpoint.display()
        )));
    }

    let fuse = WeftFuse::new(store, &options);
    let mount_options = session_mount_options(options.allow_other, options.read_only);

    let session = fuser::spawn_mount2(fuse, &mountpoint, &mount_options).map_err(|err| {
        Error::io(format!(
            "cannot mount {} (is /dev/fuse usable?): {err}",
            mountpoint.display()
        ))
    })?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|err| Error::io(format!("cannot install signal handler: {err}")))?;
    }
    while !stop.load(Ordering::SeqCst) && !session.guard.is_finished() {
        thread::sleep(Duration::from_millis(50));
    }
    // Dropping the session unmounts if the loop stopped on a signal.
    drop(session);
    Ok(())
}

/// Documented hard bound on the number of views one shared server may serve.
///
/// Each view is exactly one FUSE session thread (fuser dispatches a session's
/// callbacks on the thread that runs the loop), so a shared server holds at
/// most `MAX_SHARED_VIEWS + 1` threads (the views plus the supervisor). A
/// request above the bound is refused before anything is mounted.
pub const MAX_SHARED_VIEWS: usize = 256;

/// One read-only view of a shared server.
///
/// `snapshot_state` is the replay of `snapshot`; every view pinned to the same
/// commit can share one `Arc<State>` instead of cloning the replayed state.
pub struct SharedViewSpec {
    pub mountpoint: PathBuf,
    pub snapshot: Option<Hash32>,
    pub snapshot_state: Option<Arc<State>>,
    pub context: Option<ContextMount>,
    pub capability: Option<CapabilityContext>,
}

/// N read-only FUSE views over one `Store` opened `ReadOnly`, in one process.
///
/// The store, its selector index cache, and its backend handle are shared; each
/// view holds only its own snapshot pin, context result set, capability, inode
/// table, and open handles. Views are strictly read-only: a write-like request
/// is refused with `ReadOnly` (`EROFS`), exactly like a single read-only mount.
pub struct SharedMount {
    store: Arc<Store>,
    sessions: Vec<Option<fuser::BackgroundSession>>,
    mountpoints: Vec<PathBuf>,
}

impl SharedMount {
    /// Mount every view. All-or-nothing: if one mount fails, the views already
    /// mounted are unmounted before the error is returned.
    pub fn start(
        store: Store,
        views: Vec<SharedViewSpec>,
        allow_other: bool,
        verify_load: bool,
        load_receipt_dir: Option<PathBuf>,
    ) -> Result<SharedMount> {
        if views.is_empty() {
            return Err(Error::invalid_argument(
                "mount-many needs at least one view",
            ));
        }
        if views.len() > MAX_SHARED_VIEWS {
            return Err(Error::limit_exceeded(format!(
                "mount-many: {} views exceeds the documented maximum of {MAX_SHARED_VIEWS} per shared server (one FUSE session thread per view)",
                views.len()
            )));
        }
        if std::fs::metadata("/dev/fuse").is_err() {
            return Err(Error::io(
                "cannot open /dev/fuse: the FUSE device is unavailable in this environment",
            ));
        }
        init_diagnostics();
        let store = Arc::new(store);
        let mut sessions: Vec<Option<fuser::BackgroundSession>> = Vec::with_capacity(views.len());
        let mut mountpoints: Vec<PathBuf> = Vec::with_capacity(views.len());
        for view in views {
            if !view.mountpoint.is_dir() {
                drop(sessions);
                return Err(Error::invalid_argument(format!(
                    "mountpoint {} is not an existing directory",
                    view.mountpoint.display()
                )));
            }
            let options = MountOptions {
                allow_other,
                read_only: true,
                snapshot: view.snapshot,
                snapshot_state: view.snapshot_state,
                capability: view.capability,
                context: view.context,
                write_back: false,
                group_commit: false,
                verify_load,
                load_receipt_dir: load_receipt_dir.clone(),
            };
            let fuse = WeftFuse::new_shared(Arc::clone(&store), &options);
            match fuser::spawn_mount2(
                fuse,
                &view.mountpoint,
                &session_mount_options(allow_other, true),
            ) {
                Ok(session) => {
                    sessions.push(Some(session));
                    mountpoints.push(view.mountpoint);
                }
                Err(err) => {
                    // Dropping the sessions already started unmounts them, so a
                    // partial failure never leaves stray mounts behind.
                    drop(sessions);
                    return Err(Error::io(format!(
                        "cannot mount {} (is /dev/fuse usable?): {err}",
                        view.mountpoint.display()
                    )));
                }
            }
        }
        Ok(SharedMount {
            store,
            sessions,
            mountpoints,
        })
    }

    pub fn view_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn mountpoints(&self) -> &[PathBuf] {
        &self.mountpoints
    }

    /// The one store every view shares.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// True while the view's FUSE session thread is still serving.
    pub fn is_running(&self, index: usize) -> bool {
        self.sessions
            .get(index)
            .and_then(|session| session.as_ref())
            .map(|session| !session.guard.is_finished())
            .unwrap_or(false)
    }

    /// True while at least one view is still serving.
    pub fn any_running(&self) -> bool {
        (0..self.sessions.len()).any(|index| self.is_running(index))
    }

    /// Unmount one view. The shared store and every other view are untouched.
    pub fn unmount(&mut self, index: usize) -> Result<()> {
        let Some(session) = self
            .sessions
            .get_mut(index)
            .and_then(|session| session.take())
        else {
            return Err(Error::invalid_argument(format!(
                "no shared view at index {index}"
            )));
        };
        // Dropping the background session closes the mount; the session thread
        // observes ENODEV and exits.
        drop(session);
        Ok(())
    }
}

/// Serve `views` from one shared `Store` until SIGINT/SIGTERM or until every
/// view has been unmounted externally.
pub fn run_shared(
    store: Store,
    views: Vec<SharedViewSpec>,
    allow_other: bool,
    verify_load: bool,
    load_receipt_dir: Option<PathBuf>,
) -> Result<()> {
    let mount = SharedMount::start(store, views, allow_other, verify_load, load_receipt_dir)?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|err| Error::io(format!("cannot install signal handler: {err}")))?;
    }
    while !stop.load(Ordering::SeqCst) && mount.any_running() {
        thread::sleep(Duration::from_millis(50));
    }
    // Dropping the mount unmounts every view that is still active.
    drop(mount);
    Ok(())
}

/// Mount options for one session, shared by the single and multi-mount paths.
fn session_mount_options(allow_other: bool, read_only: bool) -> Vec<MountOption> {
    let mut mount_options = vec![
        MountOption::FSName("weftfs".to_owned()),
        MountOption::DefaultPermissions,
        MountOption::AutoUnmount,
    ];
    if allow_other {
        mount_options.push(MountOption::AllowOther);
    } else {
        // `AutoUnmount` requires an allow-* option. Allowing root in addition
        // to the mounting user is the least-privilege choice when
        // `--allow-other` was not requested.
        mount_options.push(MountOption::AllowRoot);
    }
    mount_options.push(if read_only {
        MountOption::RO
    } else {
        MountOption::RW
    });
    mount_options
}

/// `--verify-load`: read one tensor object through the verifying path,
/// recompute its logical root, and write the per-file receipt. Returns the
/// typed error so `open` can fail the handle with `EIO`.
fn verify_load_file(store: &Store, id: Hash32, receipt_dir: &Path) -> Result<()> {
    let meta = match store.tensor_metadata(id) {
        Ok(meta) => meta,
        // Not a tensor file: nothing to attest beyond the ordinary
        // content-hash-verified read.
        Err(_) => return Ok(()),
    };
    let physical = meta
        .dtype
        .physical_tensor_dtype()
        .ok_or_else(|| Error::unsupported_version("tensor has no physical dtype"))?;
    let bytes = store.read_tensor(id)?;
    let limits = Limits::default();
    let loaded_root = weftcodec::tensor::logical_root(physical, &meta.shape, &bytes, &limits)
        .map_err(Error::from)?;
    if loaded_root != meta.logical_root {
        return Err(Error::corrupt(format!(
            "load verification for object {id}: loaded root {loaded_root} does not match \
             the attested root {}",
            meta.logical_root
        )));
    }
    let row = LoadReceiptTensor {
        name: id.to_hex(),
        dtype: meta.dtype.as_str().to_owned(),
        shape: meta.shape.clone(),
        attested_root: meta.logical_root,
        loaded_root,
        content_root: identity::content_hash(&bytes),
        sha256: crate::safetensors::sha256(&bytes),
        bytes: bytes.len() as u64,
        units_verified: 1,
    };
    let set_root = LoadReceipt::roster_root(&[(row.name.clone(), row.loaded_root)])?;
    let receipt = LoadReceipt {
        checkpoint: String::new(),
        manifest_root: Hash32::ZERO,
        replica: "fuse".to_owned(),
        verification: LoadVerification::Lazy,
        bytes_produced: row.bytes,
        units_verified: row.units_verified,
        wall_ms: 0,
        exact_source_file: true,
        file_sha256: row.sha256,
        loaded_set_root: set_root,
        attested_set_root: set_root,
        tensors: vec![row],
    };
    std::fs::create_dir_all(receipt_dir)
        .map_err(|err| Error::io(format!("{}: {err}", receipt_dir.display())))?;
    let path = receipt_dir.join(format!("{}.wcb", id.to_hex()));
    let encoded = receipt.encode()?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &encoded).map_err(|err| Error::io(format!("{}: {err}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
    Ok(())
}

impl Filesystem for WeftFuse {
    fn init(
        &mut self,
        _req: &Request<'_>,
        config: &mut KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        if self.optimistic {
            // O_TRUNC must arrive in `open`/`create`, where the truncation can
            // be published as one transaction. Without this capability the
            // kernel sends a separate SETATTR(size=0) before OPEN, which is
            // indistinguishable from a bare truncate(2) that this mode refuses.
            // Applied only under optimistic publication: the MW1 path keeps the
            // exact capability set it had before.
            config
                .add_capabilities(fuser::consts::FUSE_ATOMIC_O_TRUNC)
                .map_err(|_| libc::EIO)?;
        }
        Ok(())
    }

    fn destroy(&mut self) {
        // Best effort: publish the open group and every buffered handle so an
        // orderly unmount never discards acknowledged writes. A crash still
        // leaves the previous whole generation. A shared read-only view owns
        // nothing to publish, and has no mutable handle to publish it with.
        let Ok(mut store) = self.store.write() else {
            return;
        };
        let _ = self.flush_all_pending(&mut store);
        let _ = self.publish_group(&mut store);
    }

    fn lookup(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let store = self.store.read();
        let principal = principal_of(req);
        let mut table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let bytes = name.as_bytes();
        if bytes.len() > 255 || bytes.is_empty() || bytes == b"." || bytes == b".." {
            return reply.error(libc::ENOENT);
        }
        match self.lookup_child(
            &mut table,
            &store,
            state,
            &parent_node,
            parent,
            bytes,
            &principal,
        ) {
            Ok(node) => {
                let ino = match child_ino(&table, &node, parent) {
                    Some(ino) => ino,
                    None => return reply.error(libc::EIO),
                };
                match self.attr_for(&mut table, state, &node, ino) {
                    Ok(attr) => reply.entry(&TTL, &attr, table.generation(ino)),
                    Err(err) => reply.error(errno(&err)),
                }
            }
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {
        // Inodes are retained for the lifetime of the mount; object records are
        // released through handle pins, not through lookup counts.
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        let store = self.store.read();
        let mut table = self.inodes.lock().unwrap();
        let state = self.state(&store);
        let pending_size = if self.write_back {
            fh.and_then(|fh| {
                self.handles
                    .lock()
                    .unwrap()
                    .get(&fh)
                    .filter(|handle| handle.ino == ino && !handle.pending.is_empty())
                    .map(|handle| handle.pending_end())
            })
        } else {
            None
        };
        match self.attr_for_ino(&mut table, state, ino) {
            Ok((mut attr, _generation)) => {
                // `fstat` on a write-back descriptor reports the buffered size;
                // a path `stat` still sees the committed size, matching the
                // visibility contract in docs/fuse-status.md.
                if let Some(size) = pending_size {
                    attr.size = size;
                    attr.blocks = size.div_ceil(512);
                }
                reply.attr(&TTL, &attr)
            }
            Err(err) => reply.error(errno(&err)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if self.optimistic {
            // chmod/chown/utimes mutate an existing object record. A
            // truncate is reachable through `open(O_TRUNC)`; the rest are
            // refused rather than published outside a transaction.
            return reply.error(libc::EOPNOTSUPP);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&node) {
            return reply.error(libc::EROFS);
        }
        let Some(id) = node.object() else {
            return reply.error(libc::EROFS);
        };
        // Capture the error/attr under a short immutable borrow.
        if store.mode() == OpenMode::ReadOnly {
            return reply.error(libc::EROFS);
        }
        // Buffered writes must be published before a truncate/metadata change,
        // otherwise a later flush would write bytes into a stale version.
        if self.write_back {
            if let Err(err) = self.flush_pending_for_ino(&mut store, ino) {
                return reply.error(errno(&err));
            }
        }
        let mut mutated = false;
        if let Some(new_size) = size {
            if new_size > MAX_FILE_BYTES {
                return reply.error(libc::EFBIG);
            }
            if let Err(err) = self.begin_grouped_mutation(&mut store) {
                return reply.error(errno(&err));
            }
            mutated = true;
            if let Err(err) = store.truncate(id, new_size) {
                let _ = self.finish_grouped_mutation(&mut store);
                return reply.error(errno(&err));
            }
            table.bump_generation(ino);
        }
        let mut changes = crate::store::SetAttr::default();
        if let Some(mode) = mode {
            changes.mode = Some((mode as u64) & 0o7777);
        }
        if let Some(uid) = uid {
            changes.uid = Some(uid as u64);
        }
        if let Some(gid) = gid {
            changes.gid = Some(gid as u64);
        }
        if let Some(mtime) = mtime {
            changes.mtime_ns = Some(time_or_now_ns(&mtime));
        }
        let attributes_changed = changes.mode.is_some()
            || changes.uid.is_some()
            || changes.gid.is_some()
            || changes.mtime_ns.is_some();
        if attributes_changed {
            if !mutated {
                if let Err(err) = self.begin_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                mutated = true;
            }
            if let Err(err) = store.setattr(id, changes) {
                eprintln!("weftfs: setattr failed: {err}");
                let _ = self.finish_grouped_mutation(&mut store);
                return reply.error(errno(&err));
            }
            table.bump_generation(ino);
        }
        if mutated {
            if let Err(err) = self.finish_grouped_mutation(&mut store) {
                return reply.error(errno(&err));
            }
        }
        let state = self.state(&store);
        match self.attr_for(&mut table, state, &node, ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        let store = self.store.read();
        let table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let Some(id) = node.object() else {
            return reply.error(libc::EINVAL);
        };
        let Some(record) = state.objects.get(&id) else {
            return reply.error(libc::ENOENT);
        };
        if record.kind != ObjectKind::Symlink {
            return reply.error(libc::EINVAL);
        }
        match record.symlink_target() {
            Some(target) => reply.data(target),
            None => reply.error(libc::EIO),
        }
    }

    fn mknod(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        if self.optimistic {
            return reply.error(libc::EOPNOTSUPP);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        if mode & libc::S_IFMT as u32 != libc::S_IFREG as u32 {
            // FIFOs, sockets, and device nodes are not representable in v1.
            return reply.error(libc::EPERM);
        }
        let perm = (mode & 0o7777 & !umask) as u64;
        let bytes = name.as_bytes();
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.create_file(
            parent_id,
            bytes,
            b"",
            perm,
            req.uid() as u64,
            req.gid() as u64,
            BTreeMap::new(),
        ) {
            Ok(id) => {
                let child_ino = table.ino_object(id);
                table.set_parent(child_ino, parent);
                let state = self.state(&store);
                match self.attr_for(&mut table, state, &Node::Object(id), child_ino) {
                    Ok(attr) => {
                        if let Err(err) = self.finish_grouped_mutation(&mut store) {
                            return reply.error(errno(&err));
                        }
                        reply.entry(&TTL, &attr, table.generation(child_ino))
                    }
                    Err(err) => {
                        let _ = self.finish_grouped_mutation(&mut store);
                        reply.error(errno(&err))
                    }
                }
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn mkdir(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        if self.optimistic {
            // Directory creation is a logical op the transaction table
            // does express, but the around-the-op inode bookkeeping is
            // not part of this subphase; refuse rather than half-implement.
            return reply.error(libc::EOPNOTSUPP);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        let perm = (mode & 0o7777 & !umask) as u64;
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.mkdir(
            parent_id,
            name.as_bytes(),
            perm,
            req.uid() as u64,
            req.gid() as u64,
        ) {
            Ok(id) => {
                let child_ino = table.ino_object(id);
                table.set_parent(child_ino, parent);
                let state = self.state(&store);
                match self.attr_for(&mut table, state, &Node::Object(id), child_ino) {
                    Ok(attr) => {
                        if let Err(err) = self.finish_grouped_mutation(&mut store) {
                            return reply.error(errno(&err));
                        }
                        reply.entry(&TTL, &attr, table.generation(child_ino))
                    }
                    Err(err) => {
                        let _ = self.finish_grouped_mutation(&mut store);
                        reply.error(errno(&err))
                    }
                }
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        if let Err(err) = self.flush_all_pending(&mut store) {
            return reply.error(errno(&err));
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let unlinked = if self.optimistic {
            let seed = format!(
                "{}:{}:{}:{}",
                std::process::id(),
                parent_id.to_hex(),
                hex_of(name.as_bytes()),
                store.generation()
            );
            let mut attempts: u32 = 0;
            loop {
                attempts = attempts.saturating_add(1);
                match store.txn_unlink(
                    parent_id,
                    name.as_bytes(),
                    false,
                    &fuse_transaction_id("unlink", &seed),
                ) {
                    Err(err)
                        if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS =>
                    {
                        if let Err(err) = store.refresh_to_head() {
                            break Err(err);
                        }
                    }
                    other => break other,
                }
            }
        } else {
            store.unlink(parent_id, name.as_bytes())
        };
        match unlinked {
            Ok(()) => {
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        if let Err(err) = self.flush_all_pending(&mut store) {
            return reply.error(errno(&err));
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let removed = if self.optimistic {
            let seed = format!(
                "{}:{}:{}:{}",
                std::process::id(),
                parent_id.to_hex(),
                hex_of(name.as_bytes()),
                store.generation()
            );
            let mut attempts: u32 = 0;
            loop {
                attempts = attempts.saturating_add(1);
                match store.txn_unlink(
                    parent_id,
                    name.as_bytes(),
                    true,
                    &fuse_transaction_id("rmdir", &seed),
                ) {
                    Err(err)
                        if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS =>
                    {
                        if let Err(err) = store.refresh_to_head() {
                            break Err(err);
                        }
                    }
                    other => break other,
                }
            }
        } else {
            store.rmdir(parent_id, name.as_bytes())
        };
        match removed {
            Ok(()) => {
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let (Some(old_node), Some(new_node)) = (table.node_of(parent), table.node_of(newparent))
        else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&old_node) || self.read_only_node(&new_node) {
            return reply.error(libc::EROFS);
        }
        let (Some(old_parent), Some(new_parent)) = (old_node.object(), new_node.object()) else {
            return reply.error(libc::EROFS);
        };
        if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0 {
            return reply.error(libc::EINVAL);
        }
        if let Err(err) = self.flush_all_pending(&mut store) {
            return reply.error(errno(&err));
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let result = if self.optimistic {
            if flags & RENAME_EXCHANGE != 0 {
                Err(Error::unsupported_representation(
                    "RENAME_EXCHANGE is refused under optimistic (MW2) mutations: it swaps two                      names in one step, which the transaction table does not express",
                ))
            } else {
                let seed = format!(
                    "{}:{}:{}:{}:{}:{}",
                    std::process::id(),
                    old_parent.to_hex(),
                    hex_of(name.as_bytes()),
                    new_parent.to_hex(),
                    hex_of(newname.as_bytes()),
                    store.generation()
                );
                let mut attempts: u32 = 0;
                loop {
                    attempts = attempts.saturating_add(1);
                    match store.txn_rename(
                        old_parent,
                        name.as_bytes(),
                        new_parent,
                        newname.as_bytes(),
                        flags & RENAME_NOREPLACE == 0,
                        &fuse_transaction_id("rename", &seed),
                    ) {
                        Err(err)
                            if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS =>
                        {
                            if let Err(err) = store.refresh_to_head() {
                                break Err(err);
                            }
                        }
                        other => break other,
                    }
                }
            }
        } else if flags & RENAME_EXCHANGE != 0 {
            store.rename_exchange(old_parent, name.as_bytes(), new_parent, newname.as_bytes())
        } else if flags & RENAME_NOREPLACE != 0 {
            store.rename_no_replace(old_parent, name.as_bytes(), new_parent, newname.as_bytes())
        } else {
            store.rename(old_parent, name.as_bytes(), new_parent, newname.as_bytes())
        };
        match result {
            Ok(()) => {
                // Keep the cached parent for the moved inode in sync.
                let state = self.state(&store);
                if let Ok(target) = state.lookup(new_parent, newname.as_bytes()) {
                    let child_ino = table.ino_object(target);
                    table.set_parent(child_ino, newparent);
                }
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let (Some(node), Some(new_node)) = (table.node_of(ino), table.node_of(newparent)) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&node) || self.read_only_node(&new_node) {
            return reply.error(libc::EROFS);
        }
        let (Some(id), Some(new_parent)) = (node.object(), new_node.object()) else {
            return reply.error(libc::EROFS);
        };
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let linked = if self.optimistic {
            let seed = format!(
                "{}:{}:{}:{}:{}",
                std::process::id(),
                id.to_hex(),
                new_parent.to_hex(),
                hex_of(newname.as_bytes()),
                store.generation()
            );
            store
                .txn_link(
                    id,
                    new_parent,
                    newname.as_bytes(),
                    &fuse_transaction_id("link", &seed),
                )
                .map(|_| ())
        } else {
            store.link(id, new_parent, newname.as_bytes())
        };
        match linked {
            Ok(()) => {
                let state = self.state(&store);
                match self.attr_for(&mut table, state, &Node::Object(id), ino) {
                    Ok(attr) => {
                        if let Ok(target) = state.lookup(new_parent, newname.as_bytes()) {
                            let child_ino = table.ino_object(target);
                            table.set_parent(child_ino, newparent);
                        }
                        if let Err(err) = self.finish_grouped_mutation(&mut store) {
                            return reply.error(errno(&err));
                        }
                        reply.entry(&TTL, &attr, table.generation(ino))
                    }
                    Err(err) => {
                        let _ = self.finish_grouped_mutation(&mut store);
                        reply.error(errno(&err))
                    }
                }
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn symlink(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        if self.optimistic {
            return reply.error(libc::EOPNOTSUPP);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.symlink(
            parent_id,
            link_name.as_bytes(),
            target.as_os_str().as_bytes(),
            req.uid() as u64,
            req.gid() as u64,
        ) {
            Ok(id) => {
                let child_ino = table.ino_object(id);
                table.set_parent(child_ino, parent);
                let state = self.state(&store);
                match self.attr_for(&mut table, state, &Node::Object(id), child_ino) {
                    Ok(attr) => {
                        if let Err(err) = self.finish_grouped_mutation(&mut store) {
                            return reply.error(errno(&err));
                        }
                        reply.entry(&TTL, &attr, table.generation(child_ino))
                    }
                    Err(err) => {
                        let _ = self.finish_grouped_mutation(&mut store);
                        reply.error(errno(&err))
                    }
                }
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn open(&mut self, req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let write_intent =
            flags & O_ACCMODE == O_WRONLY || flags & O_ACCMODE == O_RDWR || flags & O_TRUNC != 0;
        if write_intent && self.read_only_node(&node) {
            return reply.error(libc::EROFS);
        }
        let principal = principal_of(req);
        let read_only_surface = node.is_virtual();
        let object_id = node.object();
        let (content, size, kind) = {
            let store = self.store.read();
            let state = self.state(&store);
            let Some(id) = object_id else {
                return reply.error(libc::EISDIR);
            };
            let Some(record) = state.objects.get(&id) else {
                return reply.error(libc::ENOENT);
            };
            if !auth::acl_allows(record, &principal) {
                return reply.error(libc::EACCES);
            }
            if record.kind == ObjectKind::Dir {
                return reply.error(libc::EISDIR);
            }
            if record.kind == ObjectKind::Symlink {
                return reply.error(libc::EINVAL);
            }
            (record.content, record.size, record.kind)
        };
        if self.verify_load {
            if let Some(dir) = &self.load_receipt_dir {
                let store = self.store.read();
                if let Some(id) = object_id {
                    if let Err(err) = verify_load_file(&store, id, dir) {
                        eprintln!("weftfs: --verify-load refused {id}: {err}");
                        return reply.error(errno(&err));
                    }
                }
            }
        }
        if flags & O_TRUNC != 0 && !read_only_surface {
            // Unreachable for a read-only view (write intent was refused above),
            // so this branch always has an exclusive handle.
            let mut store = match self.store.write() {
                Ok(guard) => guard,
                Err(err) => return reply.error(errno(&err)),
            };
            if self.write_back {
                if let Err(err) = self.flush_pending_for_ino(&mut store, ino) {
                    return reply.error(errno(&err));
                }
            }
            if let Err(err) = self.begin_grouped_mutation(&mut store) {
                return reply.error(errno(&err));
            }
            let truncated = if self.optimistic {
                let id = object_id.unwrap();
                let seed = format!(
                    "{}:{}:{}",
                    std::process::id(),
                    id.to_hex(),
                    store.generation()
                );
                self.optimistic_truncate(&mut store, id, &fuse_transaction_id("truncate", &seed))
            } else {
                store.truncate(object_id.unwrap(), 0).map(|_| ())
            };
            if let Err(err) = truncated {
                eprintln!("weftfs: optimistic open(O_TRUNC) failed for {ino}: {err}");
                let _ = self.finish_grouped_mutation(&mut store);
                return reply.error(errno(&err));
            }
            if let Err(err) = self.finish_grouped_mutation(&mut store) {
                return reply.error(errno(&err));
            }
            table.bump_generation(ino);
        }
        let mut pinned_object = false;
        let mut pinned_content = false;
        if let Some(id) = object_id {
            // Pins are process-local GC protection. A shared read-only view
            // cannot pin (no `&mut Store`) and does not need to: this process
            // never runs a writer or GC over the shared store.
            if let Ok(mut store) = self.store.write() {
                if read_only_surface {
                    store.pin_content(content);
                    pinned_content = true;
                } else {
                    if let Err(err) = store.pin_object(id) {
                        return reply.error(errno(&err));
                    }
                    pinned_object = true;
                }
            }
        }
        let (content, size) = {
            let store = self.store.read();
            let state = self.state(&store);
            match object_id.and_then(|id| state.objects.get(&id)) {
                Some(record) => (record.content, record.size),
                None => (content, size),
            }
        };
        let fh = self.next_fh();
        self.handles.lock().unwrap().insert(
            fh,
            OpenHandle {
                ino,
                object: object_id,
                content,
                size,
                kind,
                append: flags & O_APPEND != 0,
                pinned_object,
                pinned_content,
                pending: Vec::new(),
                manifest: None,
            },
        );
        // Read-only handles use direct I/O so one read is served from one
        // immutable content root, never a mixture of two versions. Writable
        // handles use write-through (no writeback cache is negotiated).
        // Writable mmap is not supported; see docs/fuse-status.md.
        reply.opened(fh, if write_intent { 0 } else { FOPEN_DIRECT_IO });
    }

    fn read(
        &mut self,
        req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            return reply.error(libc::EINVAL);
        }
        let store = self.store.read();
        let handles = self.handles.lock().unwrap();
        let Some(handle) = handles.get(&fh) else {
            return reply.error(libc::EBADF);
        };
        if handle.kind == ObjectKind::Dir {
            return reply.error(libc::EISDIR);
        }
        if handle.kind != ObjectKind::File {
            return reply.error(libc::EINVAL);
        }
        let (content, file_size, authorized) = {
            let state = self.state(&store);
            match handle
                .object
                .and_then(|id| state.objects.get(&id).map(|record| (id, record)))
            {
                Some((id, record)) => {
                    // The R8 layers are only consulted through an authorized
                    // token. When they are off (the default) this branch is
                    // never taken, so the shipped read path is unchanged.
                    let token = if store.read_layers_enabled() {
                        let principal =
                            Principal::new(u64::from(req.uid()), u64::from(req.gid()), Vec::new());
                        match store.authorize_read_in(state, id, &principal, store.clock_now_ns()) {
                            Ok(token) => Some(token),
                            Err(err) => {
                                eprintln!("weftfs: read denied: {err}");
                                return reply.error(errno(&err));
                            }
                        }
                    } else {
                        None
                    };
                    if self.optimistic {
                        // T029 snapshot isolation: an open handle reads the
                        // version it opened, even after a concurrent overwrite
                        // publishes a newer one. `handle.content`/`handle.size`
                        // are refreshed by this process's own optimistic writes,
                        // so a reader-writer handle still sees its own bytes.
                        (handle.content, handle.size, token)
                    } else {
                        (record.content, record.size, token)
                    }
                }
                None => (handle.content, handle.size, None),
            }
        };
        let offset = offset as u64;
        // A write-back handle reads its own buffered bytes; every other reader
        // sees only committed content until the handle flushes.
        let visible_size = if self.write_back {
            handle.pending_end().max(file_size)
        } else {
            file_size
        };
        if offset >= visible_size {
            return reply.data(&[]);
        }
        let length = (visible_size - offset).min(size as u64);
        let committed = file_size.saturating_sub(offset).min(length);
        let read = match authorized {
            Some(token) => store.read_content_authorized(&token, content, offset, committed),
            None => store.read_content(content, offset, committed),
        };
        let mut out = match read {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("weftfs: read failed at offset {offset} length {committed}: {err}");
                return reply.error(errno(&err));
            }
        };
        if !handle.pending.is_empty() {
            let length_usize = length as usize;
            if out.len() < length_usize {
                out.resize(length_usize, 0);
            }
            let read_end = offset + length;
            for write in &handle.pending {
                let write_start = write.offset;
                let write_end = write.offset + write.data.len() as u64;
                let start = write_start.max(offset);
                let end = write_end.min(read_end);
                if start >= end {
                    continue;
                }
                let dst_start = (start - offset) as usize;
                let dst_end = (end - offset) as usize;
                let src_start = (start - write_start) as usize;
                let src_end = (end - write_start) as usize;
                out[dst_start..dst_end].copy_from_slice(&write.data[src_start..src_end]);
            }
        }
        reply.data(&out)
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if offset < 0 {
            return reply.error(libc::EINVAL);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let handles = self.handles.lock().unwrap();
        let Some(handle) = handles.get(&fh) else {
            return reply.error(libc::EBADF);
        };
        let Some(id) = handle.object else {
            return reply.error(libc::EROFS);
        };
        if handle.kind != ObjectKind::File {
            return reply.error(libc::EISDIR);
        }
        let ino = handle.ino;
        let append = handle.append;
        let last_size = handle.size;
        let pending_end = handle.pending_end();
        let offset = {
            let state = self.state(&store);
            match state.objects.get(&id) {
                Some(record) if append => record.size.max(pending_end),
                _ if append => last_size.max(pending_end),
                _ => offset as u64,
            }
        };
        if data.len() as u64 > MAX_FILE_BYTES
            || offset > MAX_FILE_BYTES
            || offset.saturating_add(data.len() as u64) > MAX_FILE_BYTES
        {
            return reply.error(libc::EFBIG);
        }
        drop(handles);
        // ---- optimistic (MW2): read-modify-write one whole-file version ----
        //
        // A range write is not a transaction primitive: the transaction table
        // has no partial-object op. Under optimistic publication the safe
        // composition is to read the current content, splice the range, and
        // publish a new object version with the observed version as the
        // precondition. A concurrent winner therefore produces
        // `Conflict(ObjectChanged)`, never a lost update. The cost is a
        // whole-file rewrite per call and a hard size bound.
        if self.optimistic {
            let end = offset.saturating_add(data.len() as u64);
            if end > OPTIMISTIC_REWRITE_MAX_BYTES {
                eprintln!(
                    "weftfs: optimistic write refused: {end} bytes exceeds the {}-byte whole-file rewrite bound",
                    OPTIMISTIC_REWRITE_MAX_BYTES
                );
                return reply.error(libc::E2BIG);
            }
            let seed = format!(
                "{}:{}:{}:{}:{}",
                std::process::id(),
                id.to_hex(),
                offset,
                data.len(),
                crate::identity::content_hash(data).to_hex()
            );
            let tx_id = fuse_transaction_id("write", &seed);
            let mut attempts: u32 = 0;
            let mut next = Vec::new();
            loop {
                attempts = attempts.saturating_add(1);
                // Re-read from the published head on every attempt: a lost
                // race means this handle's snapshot is stale, and the whole
                // point of the rewrite is to splice into the *current* bytes.
                if let Err(err) = store.refresh_to_head() {
                    return reply.error(errno(&err));
                }
                next = match store.read_object(id) {
                    Ok(bytes) => bytes,
                    Err(err) => return reply.error(errno(&err)),
                };
                if next.len() < end as usize {
                    next.resize(end as usize, 0);
                }
                next[offset as usize..end as usize].copy_from_slice(data);
                match store.txn_overwrite_file(id, &next, &tx_id) {
                    Ok(()) => break,
                    Err(err)
                        if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS =>
                    {
                        continue;
                    }
                    Err(err) => {
                        eprintln!(
                            "weftfs: optimistic write failed at offset {offset} ({} bytes) after {attempts} attempt(s): {err}",
                            data.len()
                        );
                        return reply.error(errno(&err));
                    }
                }
            }
            if let Some(handle) = self.handles.lock().unwrap().get_mut(&fh) {
                handle.size = next.len() as u64;
                if let Some(record) = self.state(&store).objects.get(&id) {
                    handle.content = record.content;
                }
            }
            self.inodes.lock().unwrap().bump_generation(ino);
            return reply.written(data.len() as u32);
        }
        // ---- write-back: buffer per handle, commit at flush/fsync/release --
        if self.write_back {
            let full = {
                let handles = self.handles.lock().unwrap();
                match handles.get(&fh) {
                    Some(handle) => {
                        handle.pending_bytes().saturating_add(data.len()) > WRITE_BACK_MAX_BYTES
                    }
                    None => true,
                }
            };
            if full {
                // An early durable commit is always allowed; buffering is never
                // an unbounded allocation.
                if let Err(err) = self.flush_pending(&mut store, fh) {
                    return reply.error(errno(&err));
                }
            }
            let mut handles = self.handles.lock().unwrap();
            let Some(handle) = handles.get_mut(&fh) else {
                return reply.error(libc::EBADF);
            };
            handle.pending.push(PendingWrite {
                offset,
                data: data.to_vec(),
            });
            handle.size = handle.size.max(offset + data.len() as u64);
            drop(handles);
            self.inodes.lock().unwrap().bump_generation(ino);
            return reply.written(data.len() as u32);
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let cached = self
            .handles
            .lock()
            .unwrap()
            .get(&fh)
            .and_then(|handle| handle.manifest.clone());
        match store.write_file_cached(id, offset, data, cached.as_ref()) {
            Ok(outcome) => {
                if let Some(handle) = self.handles.lock().unwrap().get_mut(&fh) {
                    handle.content = outcome.content;
                    handle.size = outcome.size;
                    // Keep the manifest identity so the next write does not
                    // re-read it. A truncate elsewhere makes it stale and the
                    // store ignores it.
                    if let Ok(manifest) = store.manifest_of(outcome.content) {
                        handle.manifest = Some(ManifestCache {
                            content: outcome.content,
                            manifest,
                        });
                    }
                }
                self.inodes.lock().unwrap().bump_generation(ino);
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.written(data.len() as u32)
            }
            Err(err) => {
                eprintln!(
                    "weftfs: write failed at offset {offset} ({} bytes): {err}",
                    data.len()
                );
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        // Without write-back this is not fsync: every write already committed.
        // With write-back it publishes the buffered ranges, so it is the point
        // where the data becomes visible to other readers and durable.
        if self.write_back {
            let mut store = match self.store.write() {
                Ok(guard) => guard,
                Err(err) => return reply.error(errno(&err)),
            };
            if let Err(err) = self.flush_pending(&mut store, fh) {
                eprintln!("weftfs: flush failed: {err}");
                return reply.error(errno(&err));
            }
        }
        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let mut flush_result = Ok(());
        if self.write_back {
            match self.store.write() {
                Ok(mut store) => flush_result = self.flush_pending(&mut store, fh),
                Err(err) => flush_result = Err(err),
            }
        }
        let handle = self.handles.lock().unwrap().remove(&fh);
        if let Some(handle) = handle {
            // A shared view never pins (see `open`), so it never unpins either.
            if (handle.pinned_content || handle.pinned_object) && !self.store.is_shared() {
                let Ok(mut store) = self.store.write() else {
                    return reply.error(libc::EIO);
                };
                if handle.pinned_content {
                    store.unpin_content(handle.content);
                }
                if handle.pinned_object {
                    if let Some(id) = handle.object {
                        // A failed cleanup commit is not reportable through close;
                        // the unreferenced record is retained and reclaimed by GC.
                        let _ = store.unpin_object(id);
                    }
                }
            }
        }
        match flush_result {
            Ok(()) => reply.ok(),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        // Without write-back every mutation is a durably published commit
        // before its reply, so there is nothing left to flush. With write-back
        // this is the durability point: buffered ranges are published and the
        // group committed before the reply.
        if self.write_back {
            let mut store = match self.store.write() {
                Ok(guard) => guard,
                Err(err) => return reply.error(errno(&err)),
            };
            if let Err(err) = self.flush_pending(&mut store, fh) {
                eprintln!("weftfs: fsync failed: {err}");
                return reply.error(errno(&err));
            }
        } else if !self.read_only {
            let mut store = match self.store.write() {
                Ok(guard) => guard,
                Err(err) => return reply.error(errno(&err)),
            };
            if let Err(err) = self.publish_group(&mut store) {
                return reply.error(errno(&err));
            }
        }
        reply.ok();
    }

    fn opendir(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        let store = self.store.read();
        let principal = principal_of(_req);
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let items = match self.list_dir(&mut table, &store, state, &node, ino, &principal) {
            Ok(items) => items,
            Err(err) => return reply.error(errno(&err)),
        };
        let self_attr = match self.attr_for(&mut table, state, &node, ino) {
            Ok(attr) => attr,
            Err(err) => return reply.error(errno(&err)),
        };
        let parent_ino = table.parent(ino).unwrap_or(ino);
        let parent_attr = match self.attr_for_ino(&mut table, state, parent_ino) {
            Ok((attr, _)) => attr,
            Err(_) => self_attr,
        };
        let fh = self.next_fh();
        self.dirs.lock().unwrap().insert(
            fh,
            DirHandle {
                ino,
                items,
                self_attr,
                parent_attr,
                generation: table.generation(ino),
            },
        );
        reply.opened(fh, 0);
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 0 {
            return reply.error(libc::EINVAL);
        }
        let dirs = self.dirs.lock().unwrap();
        let Some(dir) = dirs.get(&fh) else {
            return reply.error(libc::EBADF);
        };
        let mut index = offset as usize;
        let parent_ino = self
            .inodes
            .lock()
            .unwrap()
            .parent(dir.ino)
            .unwrap_or(dir.ino);
        loop {
            match index {
                0 => {
                    if reply.add(dir.ino, 1, FileType::Directory, ".") {
                        break;
                    }
                }
                1 => {
                    if reply.add(parent_ino, 2, FileType::Directory, "..") {
                        break;
                    }
                }
                _ => {
                    let item_index = index - 2;
                    let Some(item) = dir.items.get(item_index) else {
                        break;
                    };
                    let next = index as i64 + 1;
                    if reply.add(
                        item.ino,
                        next,
                        item.attr.kind,
                        OsStr::from_bytes(&item.name),
                    ) {
                        break;
                    }
                }
            }
            index += 1;
        }
        reply.ok();
    }

    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        if offset < 0 {
            return reply.error(libc::EINVAL);
        }
        let dirs = self.dirs.lock().unwrap();
        let Some(dir) = dirs.get(&fh) else {
            return reply.error(libc::EBADF);
        };
        let parent_ino = self
            .inodes
            .lock()
            .unwrap()
            .parent(dir.ino)
            .unwrap_or(dir.ino);
        let mut index = offset as usize;
        loop {
            match index {
                0 => {
                    if reply.add(dir.ino, 1, ".", &TTL, &dir.self_attr, dir.generation) {
                        break;
                    }
                }
                1 => {
                    if reply.add(parent_ino, 2, "..", &TTL, &dir.parent_attr, dir.generation) {
                        break;
                    }
                }
                _ => {
                    let item_index = index - 2;
                    let Some(item) = dir.items.get(item_index) else {
                        break;
                    };
                    let next = index as i64 + 1;
                    if reply.add(
                        item.ino,
                        next,
                        OsStr::from_bytes(&item.name),
                        &TTL,
                        &item.attr,
                        item.generation,
                    ) {
                        break;
                    }
                }
            }
            index += 1;
        }
        reply.ok();
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        self.dirs.lock().unwrap().remove(&fh);
        reply.ok();
    }

    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let store = self.store.read();
        let state = self.state(&store);
        let stats = Store::statfs_at(state);
        reply.statfs(
            stats.blocks,
            stats.blocks_free,
            stats.blocks_available,
            stats.files,
            stats.files_free,
            stats.block_size as u32,
            stats.max_name_len as u32,
            stats.block_size as u32,
        );
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if self.optimistic {
            return reply.error(libc::EOPNOTSUPP);
        }
        if position != 0 {
            return reply.error(libc::EINVAL);
        }
        if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 {
            return reply.error(libc::EINVAL);
        }
        let name = match name.to_str() {
            Some(name) => name,
            None => return reply.error(libc::EINVAL),
        };
        if value.len() as u64 > MAX_XATTR_VALUE_BYTES {
            return reply.error(libc::ERANGE);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&node) {
            return reply.error(libc::EROFS);
        }
        let Some(id) = node.object() else {
            return reply.error(libc::EROFS);
        };
        match store.get_xattr(id, name) {
            Ok(existing) => {
                if flags & XATTR_CREATE != 0 && existing.is_some() {
                    return reply.error(libc::EEXIST);
                }
                if flags & XATTR_REPLACE != 0 && existing.is_none() {
                    return reply.error(libc::ENODATA);
                }
            }
            Err(err) => return reply.error(errno(&err)),
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.set_xattr(id, name, value) {
            Ok(()) => {
                table.bump_generation(ino);
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let name = match name.to_str() {
            Some(name) => name,
            None => return reply.error(libc::EINVAL),
        };
        let store = self.store.read();
        let table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let Some(id) = node.object() else {
            return reply.error(libc::ENODATA);
        };
        // Read the attribute from the mounted state, not the live store state.
        let record = match state.objects.get(&id) {
            Some(record) => record,
            None => return reply.error(libc::ENOENT),
        };
        let key = format!("xattr.{name}");
        let value = match record.attrs.get(&key) {
            Some(Value::Bytes(bytes)) => bytes.clone(),
            Some(_) => return reply.error(libc::EIO),
            None => return reply.error(libc::ENODATA),
        };
        if size == 0 {
            reply.size(value.len() as u32);
        } else if value.len() as u32 > size {
            reply.error(libc::ERANGE);
        } else {
            reply.data(&value);
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let store = self.store.read();
        let table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let Some(id) = node.object() else {
            // Virtual directories carry no extended attributes; an empty list
            // is the correct answer, not ENODATA.
            if size == 0 {
                reply.size(0);
            } else {
                reply.data(&[]);
            }
            return;
        };
        let record = match state.objects.get(&id) {
            Some(record) => record,
            None => return reply.error(libc::ENOENT),
        };
        let mut out = Vec::new();
        for (key, value) in &record.attrs {
            let Some(name) = key.strip_prefix("xattr.") else {
                continue;
            };
            if name.starts_with("weft.") || !matches!(value, Value::Bytes(_)) {
                continue;
            }
            out.extend_from_slice(name.as_bytes());
            out.push(0);
        }
        if size == 0 {
            reply.size(out.len() as u32);
        } else if out.len() as u32 > size {
            reply.error(libc::ERANGE);
        } else {
            reply.data(&out);
        }
    }

    fn removexattr(&mut self, _req: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = match name.to_str() {
            Some(name) => name,
            None => return reply.error(libc::EINVAL),
        };
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&node) {
            return reply.error(libc::EROFS);
        }
        let Some(id) = node.object() else {
            return reply.error(libc::EROFS);
        };
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.remove_xattr(id, name) {
            Ok(true) => {
                table.bump_generation(ino);
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Ok(false) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(libc::ENODATA)
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        let store = self.store.read();
        let table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        if !node.is_virtual() {
            // The kernel enforces `default_permissions` (including
            // supplementary groups) for live namespace objects before this
            // call, so a daemon-side duplicate check would be less accurate.
            return reply.ok();
        }
        let state = self.state(&store);
        let Some(id) = node.object() else {
            let principal = principal_of(req);
            let root = match state.objects.get(&State::ROOT_ID) {
                Some(root) => root,
                None => return reply.error(libc::EIO),
            };
            return if mask & R_OK != 0 && !auth::can_read(root, &principal)
                || mask & X_OK != 0 && !auth::can_traverse(root, &principal)
            {
                reply.error(libc::EACCES)
            } else {
                reply.ok()
            };
        };
        let Some(record) = state.objects.get(&id) else {
            return reply.error(libc::ENOENT);
        };
        let principal = principal_of(req);
        if mask & R_OK != 0 && !auth::can_read(record, &principal) {
            return reply.error(libc::EACCES);
        }
        if mask & X_OK != 0
            && record.kind == ObjectKind::Dir
            && !auth::can_traverse(record, &principal)
        {
            return reply.error(libc::EACCES);
        }
        if mask & W_OK != 0 && self.read_only {
            return reply.error(libc::EROFS);
        }
        reply.ok();
    }

    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(parent_node) = table.node_of(parent) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&parent_node) {
            return reply.error(libc::EROFS);
        }
        let Some(parent_id) = parent_node.object() else {
            return reply.error(libc::EROFS);
        };
        let perm = (mode & 0o7777 & !umask) as u64;
        let bytes = name.as_bytes();
        if let Err(err) = validate_name_bytes(bytes) {
            return reply.error(errno(&err));
        }
        let existing = {
            let state = self.state(&store);
            state.lookup(parent_id, bytes).ok()
        };
        if existing.is_some() && flags & O_EXCL != 0 {
            return reply.error(libc::EEXIST);
        }
        if self.write_back {
            if let Err(err) = self.flush_all_pending(&mut store) {
                return reply.error(errno(&err));
            }
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let (id, created) = match existing {
            Some(id) => (id, false),
            None if self.optimistic => {
                let seed = format!(
                    "{}:{}:{}",
                    std::process::id(),
                    parent_id.to_hex(),
                    hex_of(bytes)
                );
                let tx_id = fuse_transaction_id("create", &seed);
                let mut attempts: u32 = 0;
                loop {
                    attempts = attempts.saturating_add(1);
                    match store.txn_create_file(
                        parent_id,
                        bytes,
                        b"",
                        perm,
                        req.uid() as u64,
                        req.gid() as u64,
                        BTreeMap::new(),
                        seed.as_bytes(),
                        &tx_id,
                    ) {
                        Ok(id) => break (id, true),
                        Err(err)
                            if err.class() == Class::Conflict && attempts < OPTIMISTIC_ATTEMPTS =>
                        {
                            // Another writer created the same name (or changed
                            // the parent) first. Re-read, and if the name now
                            // exists this open is an ordinary open of it, not
                            // an error, unless O_EXCL was requested.
                            if let Err(err) = store.refresh_to_head() {
                                return reply.error(errno(&err));
                            }
                            if let Ok(id) = self.state(&store).lookup(parent_id, bytes) {
                                if flags & O_EXCL != 0 {
                                    return reply.error(libc::EEXIST);
                                }
                                break (id, false);
                            }
                        }
                        Err(err) => return reply.error(errno(&err)),
                    }
                }
            }
            None => {
                let id = match store.create_file(
                    parent_id,
                    bytes,
                    b"",
                    perm,
                    req.uid() as u64,
                    req.gid() as u64,
                    BTreeMap::new(),
                ) {
                    Ok(id) => id,
                    Err(err) => return reply.error(errno(&err)),
                };
                (id, true)
            }
        };
        {
            let state = self.state(&store);
            match state.objects.get(&id) {
                Some(record) if record.kind == ObjectKind::Dir => return reply.error(libc::EISDIR),
                Some(record) if record.kind == ObjectKind::Symlink => {
                    return reply.error(libc::ELOOP)
                }
                Some(_) => {}
                None => return reply.error(libc::ENOENT),
            }
        }
        if created && flags & O_TRUNC != 0 {
            // A newly created file is already empty; no extra commit.
        } else if flags & O_TRUNC != 0 {
            let truncated = if self.optimistic {
                let seed = format!(
                    "{}:{}:{}",
                    std::process::id(),
                    id.to_hex(),
                    store.generation()
                );
                self.optimistic_truncate(&mut store, id, &fuse_transaction_id("truncate", &seed))
            } else {
                store.truncate(id, 0).map(|_| ())
            };
            if let Err(err) = truncated {
                return reply.error(errno(&err));
            }
        }
        if let Err(err) = store.pin_object(id) {
            return reply.error(errno(&err));
        }
        let child_ino = table.ino_object(id);
        table.set_parent(child_ino, parent);
        let state = self.state(&store);
        let attr = match self.attr_for(&mut table, state, &Node::Object(id), child_ino) {
            Ok(attr) => attr,
            Err(err) => return reply.error(errno(&err)),
        };
        let content = state
            .objects
            .get(&id)
            .map(|record| record.content)
            .ok_or_else(|| Error::not_found("created object disappeared"));
        let content = match content {
            Ok(content) => content,
            Err(err) => return reply.error(errno(&err)),
        };
        let generation = table.generation(child_ino);
        let fh = self.next_fh();
        self.handles.lock().unwrap().insert(
            fh,
            OpenHandle {
                ino: child_ino,
                object: Some(id),
                content,
                size: attr.size,
                kind: ObjectKind::File,
                append: flags & O_APPEND != 0,
                pinned_object: true,
                pinned_content: false,
                pending: Vec::new(),
                manifest: None,
            },
        );
        if let Err(err) = self.finish_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        // A writable create handle uses the same write-through mode as other
        // writable handles; read-only handles use direct I/O.
        reply.created(&TTL, &attr, generation, fh, 0);
    }

    fn getlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        reply: ReplyLock,
    ) {
        // v1 has no cross-process locking. The kernel falls back to local
        // locking when a filesystem answers ENOSYS; no lock is claimed here.
        reply.error(libc::ENOSYS);
    }

    fn setlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        _sleep: bool,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    fn fallocate(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        if self.optimistic {
            return reply.error(libc::EOPNOTSUPP);
        }
        if offset < 0 || length < 0 {
            return reply.error(libc::EINVAL);
        }
        if length == 0 {
            return reply.error(libc::EINVAL);
        }
        let known = libc::FALLOC_FL_KEEP_SIZE
            | libc::FALLOC_FL_PUNCH_HOLE
            | libc::FALLOC_FL_COLLAPSE_RANGE
            | libc::FALLOC_FL_ZERO_RANGE
            | libc::FALLOC_FL_INSERT_RANGE;
        if mode & !known != 0 {
            return reply.error(libc::EOPNOTSUPP);
        }
        let end = match (offset as u64).checked_add(length as u64) {
            Some(end) => end,
            None => return reply.error(libc::EFBIG),
        };
        if offset as u64 > MAX_FILE_BYTES || end > MAX_FILE_BYTES {
            return reply.error(libc::EFBIG);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let mut table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        if self.read_only_node(&node) {
            return reply.error(libc::EROFS);
        }
        let Some(id) = node.object() else {
            return reply.error(libc::EROFS);
        };
        let (old_version, current) = {
            let state = self.state(&store);
            match state.objects.get(&id) {
                Some(record) if record.kind == ObjectKind::File => (record.version, record.size),
                Some(_) => return reply.error(libc::EINVAL),
                None => return reply.error(libc::ENOENT),
            }
        };

        // Dense v1 files: PUNCH_HOLE/ZERO_RANGE define zero content but do not
        // reclaim physical chunks. COLLAPSE/INSERT rewrite content exactly.
        if let Err(err) = self.flush_pending_for_ino(&mut store, ino) {
            return reply.error(errno(&err));
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        let result = if mode == 0 {
            if end > current {
                store.truncate(id, end)
            } else {
                Ok(old_version)
            }
        } else if mode == libc::FALLOC_FL_KEEP_SIZE {
            // Allocation without changing size has no content effect on a
            // dense file; answer success without a commit.
            Ok(old_version)
        } else if mode == (libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE) {
            store.zero_range(id, offset as u64, length as u64, false)
        } else if mode == libc::FALLOC_FL_ZERO_RANGE {
            store.zero_range(id, offset as u64, length as u64, true)
        } else if mode == (libc::FALLOC_FL_ZERO_RANGE | libc::FALLOC_FL_KEEP_SIZE) {
            store.zero_range(id, offset as u64, length as u64, false)
        } else if mode == libc::FALLOC_FL_COLLAPSE_RANGE {
            store.collapse_range(id, offset as u64, length as u64)
        } else if mode == libc::FALLOC_FL_INSERT_RANGE {
            store.insert_range(id, offset as u64, length as u64)
        } else if mode & libc::FALLOC_FL_PUNCH_HOLE != 0 && mode & libc::FALLOC_FL_KEEP_SIZE == 0 {
            return reply.error(libc::EINVAL);
        } else if mode & libc::FALLOC_FL_COLLAPSE_RANGE != 0
            || mode & libc::FALLOC_FL_INSERT_RANGE != 0
        {
            return reply.error(libc::EINVAL);
        } else {
            return reply.error(libc::EOPNOTSUPP);
        };
        match result {
            Ok(version) => {
                if version != old_version {
                    table.bump_generation(ino);
                }
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.ok()
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }

    fn lseek(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        let store = self.store.read();
        let table = self.inodes.lock().unwrap();
        let Some(node) = table.node_of(ino) else {
            return reply.error(libc::ENOENT);
        };
        let state = self.state(&store);
        let Some(id) = node.object() else {
            return reply.error(libc::EINVAL);
        };
        let Some(record) = state.objects.get(&id) else {
            return reply.error(libc::ENOENT);
        };
        let size = record.size as i64;
        let result = match whence {
            libc::SEEK_SET => {
                if offset < 0 {
                    return reply.error(libc::EINVAL);
                }
                offset
            }
            SEEK_DATA => {
                if offset < 0 {
                    return reply.error(libc::EINVAL);
                }
                if offset < size {
                    offset
                } else {
                    return reply.error(libc::ENXIO);
                }
            }
            SEEK_HOLE => {
                if offset < 0 {
                    return reply.error(libc::EINVAL);
                }
                if offset < size {
                    // Files are dense: the only hole is the virtual hole at EOF.
                    size
                } else {
                    return reply.error(libc::ENXIO);
                }
            }
            libc::SEEK_CUR | libc::SEEK_END => {
                // The kernel tracks the file position and does not normally
                // forward these; the daemon has no position to add to.
                return reply.error(libc::ENOSYS);
            }
            _ => return reply.error(libc::EINVAL),
        };
        reply.offset(result);
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        _ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        _ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        if self.optimistic {
            return reply.error(libc::EOPNOTSUPP);
        }
        if flags != 0 || offset_in < 0 || offset_out < 0 {
            return reply.error(libc::EINVAL);
        }
        if len == 0 {
            return reply.written(0);
        }
        let mut store = match self.store.write() {
            Ok(guard) => guard,
            Err(err) => return reply.error(errno(&err)),
        };
        let (source_id, source_content, source_size, dest_id, dest_ino) = {
            let handles = self.handles.lock().unwrap();
            let Some(source) = handles.get(&fh_in) else {
                return reply.error(libc::EBADF);
            };
            let Some(dest) = handles.get(&fh_out) else {
                return reply.error(libc::EBADF);
            };
            if source.kind != ObjectKind::File || dest.kind != ObjectKind::File {
                return reply.error(libc::EINVAL);
            }
            let (Some(source_id), Some(dest_id)) = (source.object, dest.object) else {
                return reply.error(libc::EROFS);
            };
            (source_id, source.content, source.size, dest_id, dest.ino)
        };
        let (source_content, source_size) = {
            let state = self.state(&store);
            match state.objects.get(&source_id) {
                Some(record) => (record.content, record.size),
                None => (source_content, source_size),
            }
        };
        let offset_in = offset_in as u64;
        let offset_out = offset_out as u64;
        if offset_in >= source_size {
            return reply.written(0);
        }
        let available = source_size - offset_in;
        let writable = MAX_FILE_BYTES.saturating_sub(offset_out);
        let length = len.min(available).min(writable);
        if length == 0 {
            return reply.written(0);
        }
        let data = match store.read_content(source_content, offset_in, length) {
            Ok(data) => data,
            Err(err) => return reply.error(errno(&err)),
        };
        if let Err(err) = self.flush_pending_for_ino(&mut store, dest_ino) {
            return reply.error(errno(&err));
        }
        if let Err(err) = self.begin_grouped_mutation(&mut store) {
            return reply.error(errno(&err));
        }
        match store.write_file(dest_id, offset_out, &data) {
            Ok(_version) => {
                let state = self.state(&store);
                if let Some(record) = state.objects.get(&dest_id) {
                    if let Some(handle) = self.handles.lock().unwrap().get_mut(&fh_out) {
                        handle.content = record.content;
                        handle.size = record.size;
                        handle.manifest = None;
                    }
                    self.inodes.lock().unwrap().bump_generation(dest_ino);
                }
                if let Err(err) = self.finish_grouped_mutation(&mut store) {
                    return reply.error(errno(&err));
                }
                reply.written(data.len() as u32)
            }
            Err(err) => {
                let _ = self.finish_grouped_mutation(&mut store);
                reply.error(errno(&err))
            }
        }
    }
}

impl Drop for WeftFuse {
    fn drop(&mut self) {
        self.group_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.group_thread.take() {
            let _ = handle.join();
        }
        let Ok(mut store) = self.store.write() else {
            return;
        };
        let _ = self.flush_all_pending(&mut store);
        let _ = self.publish_group(&mut store);
    }
}

/// Background publisher for `--group-commit`: publishes the open group once its
/// window elapses, so an isolated mutation is not left uncommitted until the
/// next one arrives. Lock order is group-then-store or store alone; the group
/// guard is never held while waiting for the store.
fn spawn_group_committer(
    store: Arc<Mutex<Store>>,
    group: Arc<Mutex<Option<GroupSession>>>,
    stop: Arc<AtomicBool>,
    publish_failed: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(2));
            let expired = {
                let session = group.lock().unwrap();
                session
                    .as_ref()
                    .map(|current| current.opened_at.elapsed() >= GROUP_COMMIT_WINDOW)
                    .unwrap_or(false)
            };
            if !expired {
                continue;
            }
            let mut store = store.lock().unwrap();
            if store.group_open() {
                if let Err(err) = store.commit_group() {
                    publish_failed.store(true, Ordering::SeqCst);
                    eprintln!("weftfs: group commit failed to publish: {err}");
                }
            }
            *group.lock().unwrap() = None;
        }
    })
}

/// Resolve the inode number for a freshly looked-up node.
fn child_ino(table: &InodeTable, node: &Node, parent: u64) -> Option<u64> {
    let ino = match node {
        Node::Object(id) => *table.object.get(id)?,
        Node::ContextRoot => ROOT_INO,
        Node::WeftRoot => WEFT_INO,
        Node::WeftIds => WEFT_IDS_INO,
        Node::WeftContexts => WEFT_CONTEXTS_INO,
        Node::WeftIdNode(id) => *table.id_view.get(id)?,
        Node::WeftContextDir(name) => *table.context_dir.get(name)?,
        Node::WeftContextNode { context, object } => {
            *table.context_view.get(context)?.get(object)?
        }
    };
    let _ = parent;
    Some(ino)
}

// Silence an intentionally empty placeholder kept for future directory
// extension work (not part of the verified surface).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_is_reversible_and_safe() {
        assert_eq!(escape_component(b"hello.txt"), b"hello.txt");
        assert_eq!(escape_component(b"a/b"), b"a%2Fb");
        assert_eq!(escape_component(b"new\nline"), b"new%0Aline");
        assert_eq!(escape_component(b"%41"), b"%2541");
        assert_eq!(escape_component(b"\xff"), b"%FF");
        assert_eq!(escape_component(b""), b"object");
    }

    #[test]
    fn context_names_are_deterministic_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::init(dir.path(), crate::store::Options::default()).unwrap();
        let root = State::ROOT_ID;
        let a = store
            .create_file(root, b"same.txt", b"a", 0o644, 0, 0, BTreeMap::new())
            .unwrap();
        let b = store
            .create_file(root, b"same.txt.bak", b"b", 0o644, 0, 0, BTreeMap::new())
            .unwrap();
        let state = store.current_state();
        let records: Vec<(Hash32, &ObjectRecord)> = vec![
            (a, state.objects.get(&a).unwrap()),
            (b, state.objects.get(&b).unwrap()),
        ];
        let first = context_entry_names(&records, state);
        let second = context_entry_names(&records, state);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        for (name, id) in &first {
            let text = String::from_utf8(name.clone()).unwrap();
            assert!(text.ends_with(&id.to_hex()[..8]));
            assert!(!text.contains('/'));
        }
    }

    #[test]
    fn inode_mapping_is_stable_and_collision_safe() {
        let mut table = InodeTable::new(false);
        assert_eq!(table.ino_object(State::ROOT_ID), ROOT_INO);
        let a = Hash32::from_slice(&[1u8; 32]).unwrap();
        let b = Hash32::from_slice(&[2u8; 32]).unwrap();
        let ino_a = table.ino_object(a);
        assert_eq!(table.ino_object(a), ino_a);
        let ino_b = table.ino_object(b);
        assert_ne!(ino_a, ino_b);
        assert!(ino_a > WEFT_CONTEXTS_INO);
    }

    #[test]
    fn context_root_inode_table_has_no_object_root() {
        let table = InodeTable::new(true);
        assert_eq!(table.node_of(ROOT_INO), Some(Node::ContextRoot));
        assert!(table.object.is_empty());
    }
}
