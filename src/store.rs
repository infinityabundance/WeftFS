//! The WeftFS transactional store core (no FUSE frontend).
//!
//! Layout, commit publication order, recovery, and identity rules follow
//! `spec/WEFT-MODEL.md`. All mutation methods publish exactly one commit. The
//! store is single-writer (`flock`); readers pin the generation they opened.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use weftcodec::context::CausalClosure;
use weftcodec::dependency::MapResolver;
use weftcodec::frame::{Frame, Mode};
use weftcodec::modes::ByteSource;
use weftcodec::selector::Selector;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::Principal;
use crate::backend::{
    BackendKind, BackendStats, Capabilities, HeadExpectation, Key, KeyKind, StorageBackend,
};
use crate::context::CapabilityContext;
use crate::cost_model::CostClass;
use crate::error::{Class, Error, Result};
use crate::format::IndexAttestation;
use crate::format::{
    encode_commit_file, encode_state, ChunkRef, Commit, ContentIdentity, Manifest, OverlayExtent,
    OverlayRepresentation, Representation, RepresentationKind, SuperblockSlot, FORMAT_CONTENTS,
    SLOT_BYTES, SUPERBLOCK_BYTES,
};
use crate::identity::{self, Hash32};
use crate::index::{
    StateIndex, StateIndexCache, DEFAULT_INDEX_CAPACITY, INDEX_STATE_DOMAIN, MAX_INDEX_FILE_BYTES,
};
use crate::index_chain::{IndexChain, SegmentKind};
use crate::object_cache::{
    CacheKey, ObjectCache, ObjectCacheOptions, ObjectCacheStats, MAX_CACHE_ENTRIES,
};
use crate::phase_profile::{self, Phase};
use crate::quota::{
    normalize_policy, QuotaBudget, QuotaCounters, QuotaPolicyRecord, QuotaPrincipal, QuotaRecord,
    QuotaState, QuotaUsageOp, QUOTA_MAGIC, QUOTA_POLICY_MAGIC,
};
use crate::recovery::{self, BlobKind};
use crate::representation::{self, MAX_CODEC_DEPENDENCY_DEPTH};
use crate::singleflight::{
    CancelToken, ReconstructionKey, RegionKey, SingleFlight, SingleFlightOptions, SingleFlightStats,
};
use crate::state::ATTR_TENANT;
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
    /// Immutable write overlays (`weft.manifest.v4`, `WEFT-MODEL.md` §2.3.3).
    /// A range write publishes a new manifest naming the previous version as an
    /// immutable base plus one extent, instead of rewriting the chunk table.
    /// Requires a Merkle (`weft.manifest.v3`) base; bounded by
    /// [`Options::max_overlay_depth`] and [`Options::max_overlay_bytes`].
    Overlay,
}

impl Default for RepresentationMode {
    fn default() -> Self {
        RepresentationMode::Raw
    }
}

impl RepresentationMode {
    /// Canonical CLI spelling (`raw`, `codec-auto`, or `overlay`).
    pub fn as_str(self) -> &'static str {
        match self {
            RepresentationMode::Raw => "raw",
            RepresentationMode::CodecAuto => "codec-auto",
            RepresentationMode::Overlay => "overlay",
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
    /// T024: lifetime of one pre-publication reservation. A writer that stalls
    /// past its reservation is abandoned exactly like a prepared transaction
    /// past `gc_grace_secs`. See `src/gc_epoch.rs`.
    pub gc_reservation_secs: u64,
    /// Test/fixture clock override: every generated mtime and commit timestamp.
    pub fixed_timestamp_ns: Option<i64>,
    /// Physical representation policy. `Raw` (the default) writes the frozen
    /// v1 manifest; `CodecAuto` is experimental, opt-in, and Rust-only.
    pub representation: RepresentationMode,
    /// Content identity for newly written chunk manifests. `WholeFileV1` is
    /// the frozen default; `MerkleV3` writes `weft.manifest.v3` so range writes
    /// cost O(changed chunks + tree hashing) instead of O(file size).
    pub content_identity: ContentIdentity,
    /// Writer bound on chained overlay manifests. A write that would exceed it
    /// fails with `LimitExceeded` and asks the caller to compact.
    pub max_overlay_depth: u64,
    /// Writer bound on the total logical bytes carried by an overlay chain.
    pub max_overlay_bytes: u64,
    /// `CodecAuto` leaves files smaller than this on the raw path.
    pub codec_min_bytes: u64,
    /// A DELTA frame must be at least this percent smaller than the ZSTD frame
    /// to be selected. `0` disables the margin (any strictly smaller DELTA wins).
    pub codec_delta_margin_percent: u8,
    /// Write budgets to install (or replace) on open. An empty list leaves the
    /// persisted policy untouched. A writable open with a non-empty list is an
    /// admin operation: it rewrites the `quota` record, retaining usage.
    pub quotas: Vec<QuotaBudget>,
    /// Coalesce identical concurrent reconstructions (object id + content
    /// version + region). Off by default: with it disabled every read does the
    /// shipped work. See `src/singleflight.rs`.
    pub single_flight: bool,
    /// Maximum number of reconstructions that may be in flight at once.
    pub single_flight_max_entries: usize,
    /// Maximum decoded bytes reserved by in-flight reconstructions.
    pub single_flight_max_bytes: u64,
    /// Bound on the verified decoded-object cache. `0` (the default) disables
    /// the cache entirely; reads then behave exactly as they did before R8.
    pub cache_bytes: u64,
    /// Bound on the number of cached entries (books keeping). The byte bound is
    /// the primary limit; this bounds the eviction scan for tiny objects.
    pub cache_entries: usize,
    /// Attest the advisory selector index in the commit record
    /// (`weftfs.commit.v2`). Default on. Turning it off writes `weftfs.commit.v1`
    /// and no index segment: a segment without an attestation is never trusted,
    /// so it exists only to reproduce the frozen v1 fixture bytes.
    pub attest_indexes: bool,
    /// Writer default: force a full WFX3 segment after this many deltas. The
    /// hard reader bound is `index_chain::HARD_MAX_CHAIN_DELTAS` (256).
    pub index_chain_max_deltas: u32,
    /// Optional cap on the total bytes held in `indexes/`. `None` (the default)
    /// means no policy; a configured cap skips an index write that would exceed
    /// it and records a diagnostic note. The commit is unaffected.
    pub index_bytes_budget: Option<u64>,
    /// Write incremental WFX3 segments. Default on. Turning it off writes a
    /// full WFX3 segment for every attested commit (the chain depth is always
    /// 0); WFX2 segments are still readable either way.
    pub incremental_index: bool,
    /// Derive the next advisory index from the committed operation stream
    /// instead of rebuilding every row from state. Default on: the commit path
    /// touches only the rows the ops changed, while `StateIndex::build` remains
    /// the oracle and the fallback. Turning it off reproduces the pre-incremental
    /// writer (a whole-state build on every publish) for differential courts.
    pub incremental_index_maintenance: bool,
    /// T026: append transaction/read history to this path as JSON Lines
    /// (`weft.history.v1`). `None` (the default) emits nothing; `WEFT_HISTORY`
    /// overrides at open. An observation channel only — no store decision
    /// reads it and a failed write never fails an operation.
    pub history_path: Option<String>,
    /// MW2 writer policy. `Single` (the default) is MW1: an exclusive writer
    /// holds `LOCK` and no concurrent committer is admitted. `Optimistic`
    /// publishes through a conditional head compare-and-swap and is implemented
    /// for the explicit transaction API only. See `spec/WEFT-MULTIWRITER.md`.
    pub writer_mode: crate::mw2::WriterMode,
    /// One group commit is one durability barrier: blob publications inside a
    /// group defer their per-blob `fsync` to the barrier the commit path
    /// performs before the commit record. Default on. Turning it off restores
    /// the eager per-blob barrier inside a group, which is the R17/R19 control
    /// used to price the batching itself.
    pub defer_group_blob_sync: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
            gc_grace_secs: DEFAULT_GC_GRACE_SECS,
            gc_reservation_secs: crate::gc_epoch::DEFAULT_RESERVATION_SECS,
            fixed_timestamp_ns: None,
            representation: RepresentationMode::Raw,
            content_identity: ContentIdentity::WholeFileV1,
            max_overlay_depth: crate::representation::DEFAULT_MAX_OVERLAY_DEPTH,
            max_overlay_bytes: crate::representation::DEFAULT_MAX_OVERLAY_BYTES,
            codec_min_bytes: DEFAULT_CODEC_MIN_BYTES,
            codec_delta_margin_percent: DEFAULT_CODEC_DELTA_MARGIN_PERCENT,
            quotas: Vec::new(),
            single_flight: false,
            single_flight_max_entries: crate::singleflight::DEFAULT_MAX_ENTRIES,
            single_flight_max_bytes: crate::singleflight::DEFAULT_MAX_BYTES,
            cache_bytes: 0,
            cache_entries: MAX_CACHE_ENTRIES,
            attest_indexes: true,
            index_chain_max_deltas: crate::index_chain::DEFAULT_MAX_CHAIN_DELTAS,
            index_bytes_budget: None,
            incremental_index: true,
            incremental_index_maintenance: true,
            writer_mode: crate::mw2::WriterMode::Single,
            history_path: None,
            defer_group_blob_sync: true,
        }
    }
}

impl SingleFlightOptions {
    fn from_options(options: &Options) -> Self {
        SingleFlightOptions {
            enabled: options.single_flight,
            max_entries: options.single_flight_max_entries,
            max_bytes: options.single_flight_max_bytes,
        }
    }
}

impl ObjectCacheOptions {
    fn from_options(options: &Options) -> Self {
        ObjectCacheOptions {
            max_bytes: options.cache_bytes,
            max_entries: options.cache_entries,
            ..ObjectCacheOptions::default()
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

/// Outcome of [`Store::verify_region`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionVerification {
    /// The Merkle root the manifest declares (and which was recomputed).
    pub content_root: Hash32,
    /// Number of chunk blobs actually fetched and verified.
    pub chunks_verified: usize,
    /// Number of logical bytes covered by the verified chunks.
    pub bytes_verified: u64,
}

/// Outcome of a range write: the new version, the new manifest id, and the
/// new logical size. Callers that keep a [`ManifestCache`] refresh it from
/// these fields instead of re-reading the object record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub version: Hash32,
    pub content: Hash32,
    pub size: u64,
}

/// Result of [`Store::compact`]: an overlay-represented object rewritten as one
/// canonical `weft.manifest.v3` chunk table with the same content root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReport {
    pub object: Hash32,
    /// The overlay manifest that was compacted.
    pub previous_content: Hash32,
    /// The canonical single-table manifest published in its place.
    pub content: Hash32,
    pub version: Hash32,
    pub size: u64,
    /// The logical content root, which compaction must not change.
    pub content_root: Hash32,
    pub chunks: u64,
    /// Chained overlay manifests before compaction (always >= 1).
    pub overlay_depth: u64,
    pub overlay_bytes: u64,
    /// Logical bytes re-read and re-hashed while proving the rebuild exact.
    pub verified_bytes: u64,
}

/// A manifest a caller already holds for one object version. Passing it to
/// [`Store::write_file_cached`] skips re-reading that manifest when it is still
/// current. It is advisory only: a mismatch is ignored, never trusted.
#[derive(Clone, Debug)]
pub struct ManifestCache {
    pub content: Hash32,
    pub manifest: Manifest,
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
    /// Authenticated capability envelopes whose blob hash, canonical form, and
    /// Ed25519 signature were verified (expiry/revocation are caller policy).
    pub checked_capability_envelopes: u64,
    /// `provenance` relations validated (record hash, canonical form, and that
    /// the record declares the subject object's content root).
    pub checked_provenance: u64,
    /// Commits replayed by the `verify --full` quota assertion check.
    pub checked_quota_commits: u64,
    pub rejected_generations: Vec<u64>,
    pub problems: Vec<VerifyProblem>,
}

/// A writer's pre-publication retention reservation (T024).
///
/// Written **before** the first blob of a transaction is published and removed
/// by [`Store::release_reservation`] after the prepared record (or the commit)
/// names those blobs. While the record exists, a GC pass that acquired its epoch
/// at or after the reservation protects a blob by the reservation floor. The
/// handle deliberately does not borrow the store: a writer must be able to take
/// `&mut Store` to publish while the reservation is still on disk. A handle that
/// is never released is harmless — the record expires at its deadline and a
/// later pass removes it.
#[derive(Clone, Debug)]
pub struct PublicationReservation {
    key_id: String,
    record: crate::gc_epoch::Reservation,
}

impl PublicationReservation {
    /// The epoch this writer observed before registering.
    pub fn epoch(&self) -> u64 {
        self.record.epoch
    }

    /// The instant this reservation was taken; the pass's floor is the
    /// earliest such instant among live reservations.
    pub fn started_ns(&self) -> i64 {
        self.record.started_ns
    }

    /// The reservation record's on-disk identity (a digest, not the tx id).
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

/// A GC candidate/deletion record.
#[derive(Clone, Debug, Default)]
pub struct GcReport {
    pub applied: bool,
    pub marked_manifests: u64,
    pub marked_chunks: u64,
    pub candidates: Vec<String>,
    pub deleted: Vec<String>,
    /// Advisory index files deleted because no retained root names their
    /// commit. Deleting an index never deletes data.
    pub deleted_indexes: Vec<String>,
    /// Physical bytes of **data blobs** (objects + manifests) actually
    /// unlinked by this pass. This is the counter that corresponds to the
    /// store's data tree (`objects/` + `manifests/`): for a flat plane the
    /// tree shrinks by exactly this many bytes. It deliberately excludes
    /// advisory index segments (see `deleted_index_bytes`) and packed-plane
    /// pack reclamation (see `reclaimed_pack_bytes`).
    pub deleted_bytes: u64,
    /// Physical bytes of advisory index segments unlinked by this pass. An
    /// index segment is a rebuildable accelerator; its bytes are real on
    /// disk but are not part of the data tree and must never be presented as
    /// data reclaimed to an economic planner or dashboard.
    pub deleted_index_bytes: u64,
    /// `deleted_bytes + deleted_index_bytes + reclaimed_pack_bytes`: every
    /// byte this pass unlinked or reclaimed, from any directory. Reported so
    /// the two components above can be reconciled against one whole-tree
    /// measurement without ever being conflated again.
    pub deleted_total_bytes: u64,
    pub retained: u64,
    /// Commit records carrying a checkpoint state manifest, i.e. the roots that
    /// bound a cold open's replay. Counted by the same streaming scan GC uses,
    /// so a court can say whether a cell's cold open used one.
    pub checkpoints_retained: u64,
    /// Physical pack objects reclaimed because every member had been deleted
    /// (subphase R11). Zero for a plane without packs.
    pub reclaimed_packs: u64,
    pub reclaimed_pack_bytes: u64,
    pub reclaimed_catalog_segments: u64,
    /// Unreferenced object records removed in a dedicated commit (a crash left
    /// an unlinked-but-pinned object behind).
    pub removed_orphans: Vec<String>,
    /// T024: the epoch this pass acquired. `0` when no pass has ever acquired
    /// one and this was a dry run.
    pub gc_epoch: u64,
    /// T024: identity of this pass, or `-` for a dry run.
    pub gc_run_id: String,
    /// T024: live publication reservations observed at the acquired epoch.
    pub reservations_live: u64,
    /// T024: the earliest live reservation start, if one protected blobs.
    pub reservation_floor_ns: Option<i64>,
    /// T024: unreferenced data blobs retained because they are newer than the
    /// epoch acquisition instant (a concurrent publisher).
    pub retained_newer_than_cut: u64,
    /// T024: unreferenced data blobs retained because a live reservation that
    /// predates the epoch acquisition protects them.
    pub retained_reservation_floor: u64,
    /// T024: expired reservation records removed by an applied pass.
    pub deleted_reservations: u64,
    /// MW3: domain heads marked by a coordinated pass (0 for MW1/MW2).
    pub domain_heads_marked: u64,
    pub problems: Vec<String>,
}

/// The reachable blob set of one GC pass, with the root that first retained
/// each blob.
///
/// `origins` is evidence, not state: it lets a retention report say *why* an
/// expired artifact was kept (`reachable from snapshot:<commit>`). Marking is
/// idempotent and every reachable root is recorded for a blob, so the report
/// can pick a deterministic label regardless of traversal order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reachability {
    pub manifests: BTreeSet<Hash32>,
    pub chunks: BTreeSet<Hash32>,
    pub origins: BTreeMap<Hash32, BTreeSet<String>>,
    /// MW3: domain heads whose state was marked as a root by this pass.
    pub domain_heads: u64,
}

impl Reachability {
    pub(crate) fn mark_manifest(&mut self, hash: Hash32, origin: &str) -> bool {
        self.origins
            .entry(hash)
            .or_default()
            .insert(origin.to_owned());
        self.manifests.insert(hash)
    }

    pub(crate) fn mark_chunk(&mut self, hash: Hash32, origin: &str) -> bool {
        self.origins
            .entry(hash)
            .or_default()
            .insert(origin.to_owned());
        self.chunks.insert(hash)
    }

    pub(crate) fn contains(&self, hash: &Hash32) -> bool {
        self.manifests.contains(hash) || self.chunks.contains(hash)
    }

    /// The smallest root label that retained `hash`; deterministic and
    /// independent of traversal order.
    pub(crate) fn origin_of(&self, hash: &Hash32) -> Option<&str> {
        self.origins
            .get(hash)
            .and_then(|labels| labels.iter().next())
            .map(String::as_str)
    }
}

/// Result of [`Store::merge_branches`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeReport {
    pub left: Hash32,
    pub right: Hash32,
    /// Non-root paths in the union (identical paths counted once).
    pub paths: u64,
    pub added_objects: u64,
    pub added_entries: u64,
    pub head: Hash32,
    pub generation: u64,
}

/// Diagnostics for the advisory on-disk selector index. It is never consulted
/// for correctness; tests and benchmarks use it to prove that a reopen loaded
/// a persisted segment rather than rebuilding from state.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct IndexStats {
    /// Index files decoded, validated, and used for candidate narrowing.
    pub disk_loads: u64,
    /// `StateIndex::build` calls from replayed state.
    pub builds: u64,
    /// Commits whose index was derived from the previous index and the op
    /// stream (the incremental fast path).
    pub maintenance_updates: u64,
    /// Commits where incremental maintenance could not be proven exact and the
    /// whole-state oracle ran instead. Never a wrong answer: a fallback rebuild.
    pub maintenance_fallbacks: u64,
    /// Index files ignored because of a decode, integrity, or shape failure.
    pub rejects: u64,
    /// Advisory index files written after a commit.
    pub writes: u64,
    /// Total bytes of the advisory index segments written.
    pub write_bytes: u64,
    /// Index writes that failed (the commit still succeeded).
    pub write_errors: u64,
    /// Index files removed by `gc --apply`.
    pub gc_deleted: u64,
    /// Human-readable reject/write-failure notes (bounded).
    pub notes: Vec<String>,
}

/// A fully encoded segment plus the index it resolves to. The commit hash is
/// not known when the segment is encoded (WFX3 carries no commit field), so the
/// writer prepares the bytes first and publishes them after the commit.
struct PreparedIndex {
    bytes: Vec<u8>,
    index: StateIndex,
    /// The attested candidate root of `index`, computed once for the commit.
    /// Cached with the in-memory chain so the next commit's cheap validation
    /// does not re-encode the whole index.
    candidates_root: Hash32,
    /// The attested chain the bytes belong to, so the writer can keep the next
    /// commit's diff base in memory instead of re-reading the chain.
    chain: Option<IndexChain>,
}

/// One resolved chain held by the writer: the full index at `commit` and the
/// segment chain that attests it.
struct AttestedCache {
    commit: Hash32,
    index: Arc<StateIndex>,
    /// The attested candidate root of `index`. Comparison against the published
    /// commit record is cheap; the disk path still recomputes it.
    candidates_root: Hash32,
    chain: IndexChain,
}

/// The base a new delta is derived against.
struct AttestedBase {
    index: Arc<StateIndex>,
    chain: IndexChain,
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

// ---------------------------------------------------------------------------
// Branch merge helpers
// ---------------------------------------------------------------------------

/// Maximum number of non-root paths collected from one branch state.
pub const MAX_MERGE_PATHS: usize = 1_000_000;

#[derive(Clone, Debug)]
struct PathNode {
    id: Hash32,
    record: ObjectRecord,
}

/// Every path reachable from the root in `state`, keyed by absolute byte path
/// (`/`, `/a`, `/a/b`) in ascending order. A path that cannot be formed (a
/// missing object, a cycle, a parent name collision) is `Corrupt`, never
/// silently dropped.
fn collect_state_paths(state: &State) -> Result<BTreeMap<Vec<u8>, PathNode>> {
    let root = state
        .objects
        .get(&State::ROOT_ID)
        .ok_or_else(|| Error::corrupt("state has no namespace root"))?;
    let mut out: BTreeMap<Vec<u8>, PathNode> = BTreeMap::new();
    out.insert(
        b"/".to_vec(),
        PathNode {
            id: State::ROOT_ID,
            record: root.clone(),
        },
    );
    let mut queue: std::collections::VecDeque<(Vec<u8>, Hash32)> =
        std::collections::VecDeque::new();
    queue.push_back((b"/".to_vec(), State::ROOT_ID));
    let mut visited: BTreeSet<Hash32> = BTreeSet::new();
    visited.insert(State::ROOT_ID);
    while let Some((parent_path, parent_id)) = queue.pop_front() {
        let mut children: Vec<&EntryRecord> = state
            .entries
            .values()
            .filter(|entry| entry.parent == parent_id)
            .collect();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in children {
            let record = state
                .objects
                .get(&entry.target)
                .ok_or_else(|| Error::corrupt("directory entry targets a missing object"))?;
            let mut path = parent_path.clone();
            if path.as_slice() != b"/" {
                path.push(b'/');
            }
            path.extend_from_slice(&entry.name);
            if out.len() >= MAX_MERGE_PATHS {
                return Err(Error::limit_exceeded(format!(
                    "branch has more than {MAX_MERGE_PATHS} paths"
                )));
            }
            out.insert(
                path.clone(),
                PathNode {
                    id: entry.target,
                    record: record.clone(),
                },
            );
            if record.kind == ObjectKind::Dir && visited.insert(entry.target) {
                queue.push_back((path, entry.target));
            }
        }
    }
    Ok(out)
}

/// Union-merge identity of one path: kinds agree and the content identity is
/// equal. For a directory that is always true. Metadata (mode, uid, gid, mtime,
/// xattrs) is deliberately not part of this test.
fn same_path_content(left: &PathNode, right: &PathNode) -> bool {
    if left.record.kind != right.record.kind {
        return false;
    }
    match left.record.kind {
        ObjectKind::Dir => true,
        ObjectKind::File => {
            left.record.content == right.record.content && left.record.size == right.record.size
        }
        ObjectKind::Symlink => left.record.symlink_target() == right.record.symlink_target(),
    }
}

/// Split `/a/b` into (`/a`, `b`); the root has no name.
fn split_path(path: &[u8]) -> Result<(Vec<u8>, &[u8])> {
    if path == b"/" {
        return Err(Error::invalid_argument("the root path has no name"));
    }
    let position = path
        .iter()
        .rposition(|byte| *byte == b'/')
        .ok_or_else(|| Error::corrupt("absolute path has no separator"))?;
    let parent = path[..position].to_vec();
    let parent = if parent.is_empty() {
        b"/".to_vec()
    } else {
        parent
    };
    Ok((parent, &path[position + 1..]))
}

/// True when `index` and `chain` are exactly the ones `attestation` names.
///
/// `candidates_root` is passed in so a caller that already computed it (or
/// recorded it when the index was written) never re-encodes the whole index
/// just to compare.
fn attestation_matches(
    attestation: &IndexAttestation,
    index: &StateIndex,
    candidates_root: Hash32,
    chain: &IndexChain,
) -> bool {
    attestation.is_v3()
        && index.object_count() as u64 == attestation.object_count
        && index.relation_count() as u64 == attestation.relation_count
        && index.entry_count() as u64 == attestation.entry_count
        && candidates_root == attestation.candidates_root
        && Some(chain.kind) == attestation.segment_kind
        && chain.base() == attestation.segment_base
        && chain.depth() == attestation.segment_depth
        && chain.root(index.generation()).ok() == Some(attestation.segment_root)
}

fn crash_point(phase: &str) {
    if let Ok(configured) = std::env::var("WEFT_CRASH_PHASE") {
        if configured == phase {
            std::process::abort();
        }
    }
}

/// Wrap a failed chunk read without erasing its class.
///
/// A transport failure while fetching a chunk is `Io`, not `Corrupt`: an
/// unreachable store must fail closed with a transport error, and an operator
/// must not be told that a damaged store is the cause.
fn chunk_read_failure(index: usize, hash: Hash32, err: Error) -> Error {
    if err.class() == Class::Io {
        Error::io(format!("chunk {index} ({hash}): {err}"))
    } else {
        Error::corrupt(format!("chunk {index} ({hash}): {err}"))
    }
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

/// Publication ledger: exactly which durable artifacts a write produced.
///
/// Every field is counted at the point the artifact is handed to the backend,
/// so the ledger closes against [`crate::backend::BackendStats`] instead of
/// being reconstructed from a directory walk. It is diagnostic only: nothing
/// in the store's format, commit protocol, or recovery reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicationLedger {
    /// Chunk/frame payload blobs published (`objects/<ab>/<hash>`).
    pub object_blobs: u64,
    pub object_blob_bytes: u64,
    /// Manifest blobs published (`manifests/<ab>/<hash>`), which carry tensor
    /// frames and checkpoint state records.
    pub manifest_blobs: u64,
    pub manifest_blob_bytes: u64,
    /// Blob publications that found identical bytes already durable, so the
    /// bytes were re-read for the collision check but not written again.
    pub duplicate_blobs: u64,
    /// Commit records published (`commits/<gen>.commit`).
    pub commit_records: u64,
    pub commit_record_bytes: u64,
    /// Superblock slot publications (one per published commit).
    pub superblock_writes: u64,
    /// Advisory index segments and their bytes.
    pub index_segments: u64,
    pub index_bytes: u64,
    /// Durability barriers by kind, when the backend reports them:
    /// `(file data barriers, directory barriers)`. `None` for a backend whose
    /// durability is a provider acknowledgement.
    pub file_barriers: Option<u64>,
    pub dir_barriers: Option<u64>,
}

/// Atomic counters behind [`PublicationLedger`].
#[derive(Debug, Default)]
struct PublicationCounters {
    object_blobs: std::sync::atomic::AtomicU64,
    object_blob_bytes: std::sync::atomic::AtomicU64,
    manifest_blobs: std::sync::atomic::AtomicU64,
    manifest_blob_bytes: std::sync::atomic::AtomicU64,
    duplicate_blobs: std::sync::atomic::AtomicU64,
    commit_records: std::sync::atomic::AtomicU64,
    commit_record_bytes: std::sync::atomic::AtomicU64,
    superblock_writes: std::sync::atomic::AtomicU64,
}

impl PublicationCounters {
    fn snapshot(
        &self,
        index_segments: u64,
        index_bytes: u64,
        barriers: Option<(u64, u64)>,
    ) -> PublicationLedger {
        use std::sync::atomic::Ordering::Relaxed;
        PublicationLedger {
            object_blobs: self.object_blobs.load(Relaxed),
            object_blob_bytes: self.object_blob_bytes.load(Relaxed),
            manifest_blobs: self.manifest_blobs.load(Relaxed),
            manifest_blob_bytes: self.manifest_blob_bytes.load(Relaxed),
            duplicate_blobs: self.duplicate_blobs.load(Relaxed),
            commit_records: self.commit_records.load(Relaxed),
            commit_record_bytes: self.commit_record_bytes.load(Relaxed),
            superblock_writes: self.superblock_writes.load(Relaxed),
            index_segments,
            index_bytes,
            file_barriers: barriers.map(|(file, _)| file),
            dir_barriers: barriers.map(|(_, dir)| dir),
        }
    }
}

/// The open store.
pub struct Store {
    root: PathBuf,
    mode: OpenMode,
    options: Options,
    /// All physical I/O for durable artifacts goes through this backend. The
    /// mutex is non-reentrant: never call a `with_backend*` helper from a
    /// closure that already holds the guard.
    backend: Mutex<Box<dyn StorageBackend + Send>>,
    backend_kind: BackendKind,
    /// True only for a handle created by [`Store::open_blob_worker`]. Such a
    /// handle may publish immutable content-addressed blobs but still cannot
    /// publish control state, because `require_writable` checks `mode`.
    blob_worker: bool,
    /// MW3: the domain this handle publishes to and reads from. `None` for an
    /// MW1/MW2 handle (the global head). Set only by
    /// [`Store::open_domain_with_options_and_backend`].
    domain: Option<Hash32>,
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
    /// The chain this writer last published, so the next commit derives its
    /// delta from memory instead of re-reading up to `index_chain_max_deltas`
    /// segments. Validated against the previous commit record on every use; a
    /// stale or forged entry is ignored and the disk path runs.
    attested_cache: Mutex<Option<AttestedCache>>,
    /// Advisory index diagnostics and reject/write-failure notes.
    index_stats: Mutex<IndexStats>,
    /// Set by [`Store::compact_index`] so the next commit writes a full
    /// segment regardless of the base chain.
    force_full_index: std::sync::atomic::AtomicBool,
    /// Capability evidence from the writable open: two plane probes for a
    /// split backend, a blob+commit probe of the single backend otherwise.
    /// `None` on a read-only open, which never writes a probe.
    plane_probes: Option<crate::planes::PlaneProbes>,
    /// Write-budget policy and cumulative admitted usage, derived from the
    /// committed state and the published commit chain.
    quota: QuotaState,
    /// Lazily built set of chunk/manifest blob hashes reachable from committed
    /// state, with their on-disk byte lengths. Quota charging consults it so
    /// reused content costs zero. Derived data only; rebuilt on demand.
    blob_index: BTreeMap<Hash32, u64>,
    blob_index_ready: bool,
    /// Pre-group object records for ids touched by the open group. Needed so a
    /// group commit's quota delta is measured against the state before the
    /// group, not the live state that already contains the ops.
    group_pre_objects: BTreeMap<Hash32, Option<ObjectRecord>>,
    /// The same pre-group image for relation ids, so the incremental index
    /// maintainer can remove exactly the rows the group replaced or deleted.
    group_pre_relations: BTreeMap<Hash32, Option<RelationRecord>>,
    /// Number of chunk blobs read by the most recent range write, for tests
    /// and receipts that prove Merkle identity does not stream the whole file.
    range_write_chunk_reads: AtomicU64,
    /// Advisory per-object read counters (`src/access.rs`). Never consulted by
    /// commits, recovery, verification, or GC.
    access: Mutex<crate::access::AccessCounts>,
    /// R8 single-flight registry for identical reconstructions. Disabled unless
    /// `Options::single_flight` is set.
    single_flight: SingleFlight<ReconstructionKey, Arc<Vec<u8>>>,
    /// R8 verified decoded-object cache. Disabled unless `Options::cache_bytes`
    /// is non-zero; a disabled cache is never consulted on the read path.
    object_cache: Mutex<ObjectCache>,
    /// Fast flag mirroring `cache_bytes > 0` so the read path does not take the
    /// cache mutex just to learn that the cache is off.
    cache_enabled: std::sync::atomic::AtomicBool,
    /// Reconstructions actually executed (cache miss + single-flight leader).
    decode_calls: AtomicU64,
    /// Logical bytes those reconstructions produced.
    decode_bytes: AtomicU64,
    /// Lazily built, head-keyed reverse index of the relation graph and the
    /// content-root -> object map. Derived data only; rebuilt when the head
    /// changes. Used for cache admission scoring.
    content_index: Mutex<Option<(Hash32, ContentIndex)>>,
    /// Test-only leader dwell. `0` in production; see
    /// [`Store::set_single_flight_test_dwell`].
    single_flight_test_dwell_nanos: AtomicU64,
    /// Derived, bounded cache of canonical chunk tables for overlay manifests.
    /// A cold resolution costs one manifest read per chained overlay; caching
    /// the table makes a writer's next append O(1) in manifest reads. The
    /// cache is advisory: every hit is a table that was fully validated (root,
    /// lengths, base cross-checks) before insertion.
    overlay_tables: Mutex<OverlayTableCache>,
    /// Derived decode results for the KV read fast path: the validated
    /// descriptor of an object record, and the decoded capability of a verified
    /// envelope. Both are pure functions of content-addressed inputs; no
    /// authorization decision is retained. See [`crate::kv_capability::KvReadCache`].
    kv_read_cache: Mutex<crate::kv_capability::KvReadCache>,
    /// Opt-in instrumentation of the backend mutex (subphase R13). Off unless
    /// a court harness enables it; never consulted by a commit, receipt, or
    /// format decision.
    lock_probe: crate::lock_probe::LockProbe,
    /// Handles for immutable content-addressed blob reads (subphase R13).
    /// Reads through this pool never take [`Store::backend`].
    read_pool: crate::read_pool::ReadHandlePool,
    /// Mask of blob kinds this handle is holding that no other handle can
    /// resolve: a batching blob plane's open pack (or its deferred members) is
    /// readable only through the handle that buffered it. Immutable reads of
    /// those kinds use this handle instead of the pool, so a write can read back
    /// what it just published before the catalog names it.
    buffered_blob_kinds: std::sync::atomic::AtomicU8,
    /// Immutable reads that used this handle because it held unpublished
    /// members. Reported so a receipt can say the pool was bypassed and why.
    buffered_blob_reads: AtomicU64,
    /// Durable-artifact counters for [`Store::publication_ledger`].
    publication: PublicationCounters,
    /// T021: bounded cache of parsed intervening commit records, keyed by
    /// generation. Commits are immutable, so a cache hit cannot be stale; the
    /// replay still verifies the parent link and falls back to a full reload on
    /// any mismatch. This is what stops a lost race from re-reading the same
    /// gap commits on every attempt.
    commit_delta_cache: Mutex<CommitDeltaCache>,
    /// T021: consecutive-retry and gap-read statistics for one handle.
    rebase_stats: Mutex<RebaseStats>,
    /// T026: optional JSONL history sink, opened once at open.
    history: Option<crate::history::HistorySink>,
    /// Identity of this handle in the history (stable for the handle's life).
    history_handle: String,
}

/// One parsed commit kept for a bounded rebase cache.
#[derive(Clone)]
struct CachedCommit {
    commit: crate::format::Commit,
    hash: Hash32,
    wcb_len: u64,
    ops_bytes: usize,
}

/// A bounded cache of intervening commit records.
struct CommitDeltaCache {
    entries: std::collections::BTreeMap<u64, CachedCommit>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl CommitDeltaCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        CommitDeltaCache {
            entries: std::collections::BTreeMap::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn get(&self, generation: u64) -> Option<&CachedCommit> {
        self.entries.get(&generation)
    }

    fn insert(&mut self, generation: u64, value: CachedCommit) {
        if let Some(previous) = self.entries.insert(generation, value) {
            self.bytes = self.bytes.saturating_sub(previous.ops_bytes);
        }
        if let Some(entry) = self.entries.get(&generation) {
            self.bytes = self.bytes.saturating_add(entry.ops_bytes);
        }
        // Evict whole entries while over a bound. Commits are immutable, so the
        // eviction order only affects the hit rate, never correctness.
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some((&oldest, _)) = self.entries.iter().next() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.ops_bytes);
            }
        }
    }
}

/// Retry/gap statistics for one MW2 handle.
#[derive(Clone, Debug, Default)]
pub struct RebaseStats {
    /// Commits the handle read from the backend after losing a race.
    pub gap_commits_read: u64,
    /// Gap commits that were served from the bounded cache instead.
    pub gap_commits_cached: u64,
    /// Gap commits whose changed keys intersected the transaction's touched set.
    pub touched_intersections: u64,
    /// Largest number of consecutive lost publication attempts by one transaction.
    pub max_consecutive_retries: u64,
    /// Publication attempts across every transaction this handle executed.
    pub total_attempts: u64,
    /// Transactions executed through the optimistic loop.
    pub transactions: u64,
}

/// Per-tensor attribution guard used by the restore pipeline when the
/// backend-lock probe is enabled. Publishes one row on drop.
pub(crate) struct TensorProbeGuard<'a> {
    _guard: crate::lock_probe::TensorGuard<'a>,
}

/// Map a blob-read backend error to the store's established classification:
/// an I/O failure keeps its class, and everything else (including a missing
/// blob) is a `Corrupt` result naming the key.
fn blob_read_failure(key: &Key, err: Error) -> Error {
    let message = format!("missing blob {}: {}", key.display_path(), err.message());
    if err.class() == Class::Io {
        Error::io(message)
    } else {
        Error::corrupt(message)
    }
}

/// Bounded most-recently-used cache of validated overlay chunk tables.
#[derive(Default)]
struct OverlayTableCache {
    entries: BTreeMap<Hash32, Arc<Vec<ChunkRef>>>,
    order: Vec<Hash32>,
    bytes: u64,
}

/// Maximum bytes of cached chunk-table entries retained by one store handle.
const MAX_OVERLAY_TABLE_CACHE_BYTES: u64 = 8 * 1024 * 1024;

impl OverlayTableCache {
    fn get(&mut self, id: &Hash32) -> Option<Arc<Vec<ChunkRef>>> {
        let hit = self.entries.get(id).cloned();
        if hit.is_some() {
            self.order.retain(|entry| entry != id);
            self.order.push(*id);
        }
        hit
    }

    fn insert(&mut self, id: Hash32, table: Arc<Vec<ChunkRef>>) {
        let bytes = (table.len() as u64).saturating_mul(40);
        if let Some(previous) = self.entries.insert(id, table) {
            self.bytes = self
                .bytes
                .saturating_sub((previous.len() as u64).saturating_mul(40));
            self.order.retain(|entry| *entry != id);
        }
        self.order.push(id);
        self.bytes = self.bytes.saturating_add(bytes);
        while self.bytes > MAX_OVERLAY_TABLE_CACHE_BYTES && self.order.len() > 1 {
            let victim = self.order.remove(0);
            if let Some(table) = self.entries.remove(&victim) {
                self.bytes = self
                    .bytes
                    .saturating_sub((table.len() as u64).saturating_mul(40));
            }
        }
    }
}

/// Reverse index used by cache admission scoring (derived data only).
#[derive(Default)]
struct ContentIndex {
    /// Lowest ObjectId for each content (manifest) identity.
    object_by_content: BTreeMap<Hash32, Hash32>,
    /// `derived_from` relation edges that target each object.
    dependents: BTreeMap<Hash32, u64>,
}

/// An authorized read of one object version.
///
/// The token is the only way into the cache and single-flight paths. Its
/// fields are private; it can only be produced by [`Store::authorize_read`] or
/// [`Store::authorize_read_at`], which both evaluate POSIX mode, the
/// `weft.acl.read` ACL, and any attached capability envelope. A cache hit can
/// therefore never stand in for authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizedRead {
    id: Hash32,
    content: Hash32,
    size: u64,
    kind: ObjectKind,
}

impl AuthorizedRead {
    pub fn object_id(&self) -> Hash32 {
        self.id
    }

    pub fn content(&self) -> Hash32 {
        self.content
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn kind(&self) -> ObjectKind {
        self.kind
    }
}

/// Read-path instrumentation: reconstruction work, single-flight activity, and
/// cache outcomes. Counters are cumulative over the open store handle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadLayerStats {
    pub decode_calls: u64,
    pub decode_bytes: u64,
    pub single_flight: SingleFlightStats,
    pub cache: ObjectCacheStats,
}

/// Accepted quota outcome for one commit. `usage_after` is the cumulative
/// admitted usage after this commit's ops; it is published as the commit's
/// `set_quota_usage` op, so no separate durable record is needed.
pub(crate) struct QuotaPlan {
    pub additions: BTreeMap<Hash32, u64>,
    pub usage_after: BTreeMap<QuotaPrincipal, QuotaCounters>,
}

impl Store {
    /// Initialize a new store with the normative local backend. The directory
    /// must be absent or empty.
    pub fn init(path: impl AsRef<Path>, options: Options) -> Result<Store> {
        Self::init_with_backend(path, options, BackendKind::Local)
    }

    /// Initialize a new store on an explicit backend.
    ///
    /// The store refuses before touching disk when the declared backend cannot
    /// publish atomically or cannot make writes durable; there is no degraded
    /// mode.
    pub fn init_with_backend(
        path: impl AsRef<Path>,
        options: Options,
        backend_kind: BackendKind,
    ) -> Result<Store> {
        validate_options(&options)?;
        if options.writer_mode == crate::mw2::WriterMode::Optimistic {
            return Err(Error::invalid_argument(
                "initialize the store in single mode, then reopen it with --writer-mode \
                 optimistic; the writer-mode policy record is created by the first optimistic \
                 writable open",
            ));
        }
        // A split is refused by *plane*, with a message that names the plane
        // that cannot honour the contract, before the generic capability gate.
        backend_kind.require_plane_contract()?;
        let capabilities = backend_kind.capabilities();
        capabilities.require_durable_fsync("a writable store")?;
        capabilities.require_atomic_publish("initial store publication")?;
        capabilities.require_conditional_head("a writable store")?;
        let root = path.as_ref().to_path_buf();

        if backend_kind.is_remote() {
            // An S3 bucket is the store root. There is no local directory to
            // create or inspect; refuse if the bucket already holds a store.
            let probe = backend_kind.open(&root)?;
            if probe.exists(&Key::format())? {
                return Err(Error::exists(
                    "a WeftFS store already exists at this S3 bucket",
                ));
            }
        } else if root.exists() {
            if !root.is_dir() {
                return Err(Error::exists("store path exists and is not a directory"));
            }
            let probe = backend_kind.open(&root)?;
            if probe.exists(&Key::format())? {
                return Err(Error::exists("a WeftFS store already exists at this path"));
            }
            let mut entries = fs::read_dir(&root)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", root.display())))?;
            if entries.next().is_some() {
                return Err(Error::exists("store path is not empty"));
            }
        } else {
            crate::backend::make_dir(&root)?;
        }

        let mut backend = backend_kind.open(&root)?;
        backend.init_layout()?;
        backend.put_atomic(&Key::format(), FORMAT_CONTENTS.as_bytes())?;
        backend.create_zeroed(&Key::superblock(), SUPERBLOCK_BYTES as u64)?;
        backend.acquire_writer_lock()?;
        // Prove the physical capabilities this handle will rely on. A probe
        // that contradicts a declared capability fails closed here, before any
        // commit is published.
        let plane_probes = backend.probe_planes()?;

        let cache_enabled = options.cache_bytes > 0;
        let single_flight_options = SingleFlightOptions::from_options(&options);
        let cache_options = ObjectCacheOptions::from_options(&options);
        let read_pool = crate::read_pool::ReadHandlePool::new(backend_kind.clone(), root.clone());
        let mut store = Store {
            root,
            mode: OpenMode::ReadWrite,
            options,
            backend: Mutex::new(backend),
            backend_kind,
            blob_worker: false,
            domain: None,
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
            attested_cache: Mutex::new(None),
            index_stats: Mutex::new(IndexStats::default()),
            force_full_index: std::sync::atomic::AtomicBool::new(false),
            quota: QuotaState::default(),
            blob_index: BTreeMap::new(),
            blob_index_ready: false,
            group_pre_objects: BTreeMap::new(),
            group_pre_relations: BTreeMap::new(),
            range_write_chunk_reads: AtomicU64::new(0),
            plane_probes,
            access: Mutex::new(crate::access::AccessCounts::default()),
            single_flight: SingleFlight::new(single_flight_options),
            object_cache: Mutex::new(ObjectCache::new(cache_options)),
            cache_enabled: std::sync::atomic::AtomicBool::new(cache_enabled),
            decode_calls: AtomicU64::new(0),
            decode_bytes: AtomicU64::new(0),
            content_index: Mutex::new(None),
            single_flight_test_dwell_nanos: AtomicU64::new(0),
            overlay_tables: Mutex::new(OverlayTableCache::default()),
            kv_read_cache: Mutex::new(crate::kv_capability::KvReadCache::default()),
            lock_probe: crate::lock_probe::LockProbe::new(),
            read_pool,
            buffered_blob_kinds: std::sync::atomic::AtomicU8::new(0),
            buffered_blob_reads: AtomicU64::new(0),
            publication: PublicationCounters::default(),
            commit_delta_cache: Mutex::new(CommitDeltaCache::new(4096, 32 * 1024 * 1024)),
            rebase_stats: Mutex::new(RebaseStats::default()),
            history: None,
            history_handle: format!("pid:{}:{}", std::process::id(), self_wall_ns()),
        };
        let history_options = store.options.clone();
        store.attach_history(&history_options)?;
        store.write_genesis()?;
        if !store.options.quotas.is_empty() {
            store.install_quota_policy(store.options.quotas.clone())?;
        }
        store.with_backend_mut(|backend| backend.sync_dir(&Key::root()))?;
        Ok(store)
    }

    /// Open an existing store with the normative local backend.
    pub fn open(path: impl AsRef<Path>, mode: OpenMode) -> Result<Store> {
        Self::open_impl_mode(
            path,
            mode,
            false,
            BackendKind::Local,
            crate::mw2::WriterMode::Single,
        )
    }

    /// Open an existing store on an explicit backend.
    pub fn open_with_backend(
        path: impl AsRef<Path>,
        mode: OpenMode,
        backend_kind: BackendKind,
    ) -> Result<Store> {
        Self::open_impl_mode(
            path,
            mode,
            false,
            backend_kind,
            crate::mw2::WriterMode::Single,
        )
    }

    /// Benchmark-only: open through the original full commit-directory scan so
    /// the fast path's cost can be measured against it.
    #[doc(hidden)]
    pub fn open_full_scan_for_bench(path: impl AsRef<Path>, mode: OpenMode) -> Result<Store> {
        Self::open_impl_mode(
            path,
            mode,
            true,
            BackendKind::Local,
            crate::mw2::WriterMode::Single,
        )
    }

    fn open_impl_mode(
        path: impl AsRef<Path>,
        mode: OpenMode,
        full_scan: bool,
        backend_kind: BackendKind,
        writer_mode: crate::mw2::WriterMode,
    ) -> Result<Store> {
        // `store_open` encloses the whole open, including the two nested phases
        // below (`state_replay`, `plane_probe`). A receipt must not add the
        // nested totals to it.
        let _open = crate::phase_profile::scope(crate::phase_profile::Phase::StoreOpen);
        let root = path.as_ref().to_path_buf();
        if mode == OpenMode::ReadWrite {
            backend_kind.require_plane_contract()?;
            let capabilities = backend_kind.capabilities();
            capabilities.require_durable_fsync("a read-write store")?;
            capabilities.require_conditional_head("a read-write store")?;
        }
        let mut backend = backend_kind.open(&root)?;
        if mode == OpenMode::ReadWrite
            && writer_mode == crate::mw2::WriterMode::Optimistic
            && !matches!(backend.label(), "local" | "object" | "3fs" | "s3" | "azure")
        {
            return Err(Error::unsupported_version(format!(
                "optimistic (MW2) publication is implemented for the local, object, 3fs, s3, and \
                 azure commit planes; this backend is `{}`",
                backend.label()
            )));
        }
        // MW3 needs a per-domain head key. A plane that cannot address
        // `domains/<id>/superblock` is refused, never downgraded to the global
        // head.
        if writer_mode == crate::mw2::WriterMode::Sharded
            && !backend.supports_domain_heads()
            && !matches!(backend.label(), "object")
        {
            return Err(Error::unsupported_version(format!(
                "sharded (MW3) publication is implemented for the local commit plane in this \
                 prototype; this backend is `{}`",
                backend.label()
            )));
        }
        let format_bytes = backend.read_all(&Key::format()).map_err(|err| {
            // A transport or authorization failure is not corruption. Keep the
            // I/O class so an operator (and the distributed court) can
            // distinguish an unreachable endpoint from a damaged store, and
            // keep an authorization refusal classified as `Unauthorized` so a
            // bad credential is never reported as damaged data. Every other
            // failure to read FORMAT means this is not a usable store.
            match err.class() {
                Class::Io => Error::io(format!("cannot read FORMAT: {err}")),
                Class::Unauthorized => Error::unauthorized(format!("cannot read FORMAT: {err}")),
                _ => Error::corrupt(format!("cannot read FORMAT: {err}")),
            }
        })?;
        let format = std::str::from_utf8(&format_bytes)
            .map_err(|_| Error::unsupported_version("FORMAT is not valid UTF-8".to_owned()))?;
        if format != FORMAT_CONTENTS {
            return Err(Error::unsupported_version(format!(
                "FORMAT is `{}`, expected `{FORMAT_CONTENTS}`",
                format.escape_debug()
            )));
        }
        if mode == OpenMode::ReadWrite {
            // Mode identity is checked *before* the exclusive writer lock is
            // taken. A single-writer open that would be refused anyway must not
            // first take a lock the store's recorded mode does not permit, and
            // the refusal must name the mode rather than surface as `Busy`.
            if let Some(stored) = crate::mw2::WriterMode::policy(&*backend)? {
                if stored != writer_mode {
                    return Err(Error::unsupported_version(format!(
                        "store writer mode is `{}`; this handle requested `{}`. Writer modes \
                         are not mixed. Use --writer-mode {} or a separate store.",
                        stored.as_str(),
                        writer_mode.as_str(),
                        stored.as_str()
                    )));
                }
            }
            if writer_mode != crate::mw2::WriterMode::Single {
                // MW2/MW3: no exclusive writer. The publication lock is taken
                // for the short publication critical section only.
                backend.ensure_layout()?;
                backend.clean_stale_tmp(DEFAULT_GC_GRACE_SECS)?;
            } else {
                backend.acquire_writer_lock()?;
                backend.ensure_layout()?;
                backend.clean_stale_tmp(DEFAULT_GC_GRACE_SECS)?;
            }
        }
        // A plane may implement `supports_domain_heads` without the full key
        // set (the object plane's flat naming). The prototype refuses it above
        // rather than claim a head it cannot list.
        //
        // A read-only open must not silently observe an empty namespace when a
        // store is partitioned into MW3 domains: the global head is undefined
        // there. Refuse with a typed error instead. A writable single/optimistic
        // open is already refused by `ensure_writer_policy`.
        if mode == OpenMode::ReadOnly && writer_mode != crate::mw2::WriterMode::Sharded {
            if let Ok(Some(stored)) = crate::mw2::WriterMode::policy(&*backend) {
                if stored == crate::mw2::WriterMode::Sharded {
                    return Err(Error::unsupported_version(
                        "this store is partitioned into MW3 conflict domains and has no global \
                         head; name a domain with --domain (an MW1 reader refuses rather than \
                         report an empty namespace)",
                    ));
                }
            }
        }
        // A read-only open never writes, so it never probes. A writable open
        // proves both planes before it is trusted with a commit.
        let plane_probes = if mode == OpenMode::ReadWrite {
            crate::phase_profile::timed(crate::phase_profile::Phase::PlaneProbe, || {
                backend.probe_planes()
            })?
        } else {
            None
        };

        // Read-only opens use the directory-free fast path. Writable opens
        // must observe the highest commit generation so `next_generation`
        // never reuses a rejected generation; they still bound commit decodes
        // to the checkpoint window.
        let recovery = {
            let _replay = crate::phase_profile::scope(crate::phase_profile::Phase::StateReplay);
            if full_scan {
                recovery::recover_full_scan_via(&*backend)?
            } else if mode == OpenMode::ReadWrite {
                recovery::recover_for_write_via(&*backend)?
            } else {
                recovery::recover_via(&*backend)?
            }
        };
        // A writable open repairs the superblock cache from the writer lock
        // record: the record is the publication point, the cache is a cache.
        // An optimistic store never writes that record, so recovery must treat
        // the superblock as authoritative (see `mw2::WriterMode::policy`).
        if mode == OpenMode::ReadWrite && writer_mode == crate::mw2::WriterMode::Single {
            if let Ok(Some(attested)) = backend.attested_head() {
                if attested.generation != 0 && attested.commit_len != 0 {
                    let _ = backend.refresh_head_cache(
                        attested.generation,
                        attested.commit,
                        attested.commit_len,
                        0,
                    );
                }
            }
        }
        let access = crate::access::AccessCounts::load(&*backend);
        let next_generation = recovery
            .generation
            .max(recovery.highest_file_generation)
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        let read_pool = crate::read_pool::ReadHandlePool::new(backend_kind.clone(), root.clone());
        let mut store = Store {
            root,
            mode,
            options: Options::default(),
            backend: Mutex::new(backend),
            backend_kind,
            blob_worker: false,
            domain: None,
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
            attested_cache: Mutex::new(None),
            index_stats: Mutex::new(IndexStats::default()),
            force_full_index: std::sync::atomic::AtomicBool::new(false),
            quota: QuotaState::default(),
            blob_index: BTreeMap::new(),
            blob_index_ready: false,
            group_pre_objects: BTreeMap::new(),
            group_pre_relations: BTreeMap::new(),
            range_write_chunk_reads: AtomicU64::new(0),
            plane_probes,
            access: Mutex::new(access),
            single_flight: SingleFlight::new(SingleFlightOptions::default()),
            object_cache: Mutex::new(ObjectCache::new(ObjectCacheOptions::default())),
            cache_enabled: std::sync::atomic::AtomicBool::new(false),
            decode_calls: AtomicU64::new(0),
            decode_bytes: AtomicU64::new(0),
            content_index: Mutex::new(None),
            single_flight_test_dwell_nanos: AtomicU64::new(0),
            overlay_tables: Mutex::new(OverlayTableCache::default()),
            kv_read_cache: Mutex::new(crate::kv_capability::KvReadCache::default()),
            lock_probe: crate::lock_probe::LockProbe::new(),
            read_pool,
            buffered_blob_kinds: std::sync::atomic::AtomicU8::new(0),
            buffered_blob_reads: AtomicU64::new(0),
            publication: PublicationCounters::default(),
            commit_delta_cache: Mutex::new(CommitDeltaCache::new(4096, 32 * 1024 * 1024)),
            rebase_stats: Mutex::new(RebaseStats::default()),
            history: None,
            history_handle: format!("pid:{}:{}", std::process::id(), self_wall_ns()),
        };
        store.load_quota_record();
        if mode == OpenMode::ReadWrite {
            store.ensure_writer_policy(writer_mode)?;
        }
        store.options.writer_mode = writer_mode;
        let history_options = store.options.clone();
        store.attach_history(&history_options)?;
        Ok(store)
    }

    /// Open with explicit runtime options (chunk size affects new writes only;
    /// existing manifests carry their own chunk size).
    pub fn open_with_options(
        path: impl AsRef<Path>,
        mode: OpenMode,
        options: Options,
    ) -> Result<Store> {
        Self::open_with_options_and_backend(path, mode, options, BackendKind::Local)
    }

    /// Open an isolated **blob worker** handle: a read-only store handle that
    /// may also publish immutable, content-addressed blobs.
    ///
    /// This is the capability the parallel checkpoint writer gives each worker
    /// rank. Control-state publication stays refused (`require_writable` checks
    /// the mode), so the single-writer/lease/fence proof is untouched; all a
    /// worker can do is create-once blob publication on an immutable key.
    pub fn open_blob_worker(
        path: impl AsRef<Path>,
        options: Options,
        backend_kind: BackendKind,
    ) -> Result<Store> {
        let mut store =
            Self::open_with_options_and_backend(path, OpenMode::ReadOnly, options, backend_kind)?;
        store.blob_worker = true;
        Ok(store)
    }

    /// Open with explicit runtime options and backend.
    pub fn open_with_options_and_backend(
        path: impl AsRef<Path>,
        mode: OpenMode,
        options: Options,
        backend_kind: BackendKind,
    ) -> Result<Store> {
        validate_options(&options)?;
        let mut store = Self::open_impl_mode(path, mode, false, backend_kind, options.writer_mode)?;
        if !options.quotas.is_empty() {
            store.install_quota_policy(options.quotas.clone())?;
        }
        store.options = options;
        store.apply_read_layer_options();
        let history_options = store.options.clone();
        store.attach_history(&history_options)?;
        Ok(store)
    }

    // ---- MW3 conflict domains -------------------------------------------

    /// Open a handle bound to one MW3 conflict domain.
    ///
    /// `options.writer_mode` **must** be [`crate::mw2::WriterMode::Sharded`];
    /// nothing is silently enabled. The handle reads and publishes the domain's
    /// own two-slot head and per-domain commit chain while sharing the global
    /// content-addressed blob namespace.
    ///
    /// The domain's index attestation is disabled in this prototype
    /// (`options.attest_indexes` is forced off), so its commits are
    /// `weftfs.commit.v1` records. This is stated in
    /// `spec/WEFT-MULTIWRITER.md` §15.10 and is not a silent downgrade: the
    /// handle records the reason in its diagnostics.
    pub fn open_domain_with_options_and_backend(
        path: impl AsRef<Path>,
        mode: OpenMode,
        mut options: Options,
        backend_kind: BackendKind,
        domain: Hash32,
    ) -> Result<Store> {
        if options.writer_mode != crate::mw2::WriterMode::Sharded {
            return Err(Error::invalid_argument(
                "a domain handle requires --writer-mode sharded; nothing is enabled silently",
            ));
        }
        validate_options(&options)?;
        let mut store = Self::open_impl_mode(
            path,
            mode,
            false,
            backend_kind,
            crate::mw2::WriterMode::Sharded,
        )?;
        if mode == OpenMode::ReadOnly {
            let stored = store.with_backend(|backend| crate::mw2::WriterMode::policy(backend))?;
            match stored {
                Some(crate::mw2::WriterMode::Sharded) => {}
                other => {
                    return Err(Error::unsupported_version(format!(
                        "this store is `{}`, not a sharded (MW3) store; --domain is only valid \
                         on a sharded store",
                        other.map(|mode| mode.as_str()).unwrap_or("single")
                    )))
                }
            }
        }
        store.domain = Some(domain);
        options.attest_indexes = false;
        store.options = options;
        store.apply_read_layer_options();
        let history_options = store.options.clone();
        store.attach_history(&history_options)?;
        if store.mode == OpenMode::ReadWrite {
            store.with_backend_mut(|backend| backend.ensure_domain_layout(&domain))?;
        }
        store.recover_domain()?;
        if store.mode == OpenMode::ReadWrite && store.generation == 0 {
            return Err(Error::not_found(format!(
                "MW3 domain {} has no genesis commit; register it with `weftfs mw3 register` \
                 before opening a writable domain handle",
                domain.to_hex()
            )));
        }
        store.notes.push(
            "MW3 domain handle: index attestation is disabled in this prototype; commits are \
             weftfs.commit.v1"
                .to_owned(),
        );
        Ok(store)
    }

    /// The MW3 domain this handle is bound to, or `None` for a global handle.
    pub fn domain(&self) -> Option<Hash32> {
        self.domain
    }

    fn head_key(&self) -> Key {
        match self.domain {
            Some(domain) => Key::domain_superblock(domain),
            None => Key::superblock(),
        }
    }

    fn commit_key(&self, generation: u64) -> Key {
        match self.domain {
            Some(domain) => Key::domain_commit(domain, generation),
            None => Key::commit(generation),
        }
    }

    /// The published head this handle publishes against.
    fn load_head_from_plane(&self) -> Result<Option<(u64, Hash32)>> {
        match self.domain {
            Some(domain) => self.with_backend(|backend| backend.load_domain_head(&domain)),
            None => self.with_backend(|backend| backend.load_published_head()),
        }
    }

    /// T021: load one commit through the bounded intervening-change cache.
    ///
    /// The second element is `true` when the record came from the cache. A
    /// cached record is immutable and still parent-checked by the caller, so a
    /// hit cannot change correctness.
    fn load_commit_cached(&self, generation: u64) -> Result<(recovery::LoadedCommit, bool)> {
        {
            let cache = self
                .commit_delta_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(generation) {
                return Ok((
                    recovery::LoadedCommit {
                        generation,
                        hash: cached.hash,
                        wcb_len: cached.wcb_len,
                        commit: cached.commit.clone(),
                    },
                    true,
                ));
            }
        }
        let loaded = self.with_backend(|backend| {
            recovery::load_commit_key_via(backend, &self.commit_key(generation), Some(generation))
        })?;
        {
            let mut cache = self
                .commit_delta_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.insert(
                generation,
                CachedCommit {
                    commit: loaded.commit.clone(),
                    hash: loaded.hash,
                    wcb_len: loaded.wcb_len,
                    ops_bytes: loaded.wcb_len as usize,
                },
            );
        }
        Ok((loaded, false))
    }

    /// T021: retry, gap-read, and starvation statistics for this handle.
    pub fn rebase_stats(&self) -> RebaseStats {
        self.rebase_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Replay one domain's commit chain from the newest usable checkpoint.
    fn recover_domain(&mut self) -> Result<()> {
        let Some(domain) = self.domain else {
            return Ok(());
        };
        let head = self.with_backend(|backend| backend.load_domain_head(&domain))?;
        let Some((generation, head_id)) = head else {
            self.state = State::default();
            self.head = Hash32::ZERO;
            self.generation = 0;
            self.next_generation = 1;
            self.blob_index.clear();
            self.blob_index_ready = false;
            return Ok(());
        };
        // Bounded checkpoint search: the domain checkpoint interval is a writer
        // option, so a missing checkpoint is a longer replay, never an error.
        const CHECKPOINT_SCAN: u64 = 256;
        let floor = generation.saturating_sub(CHECKPOINT_SCAN).max(1);
        let mut state = State::default();
        let mut hash = Hash32::ZERO;
        let mut next = 1u64;
        for candidate in (floor..=generation).rev() {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(backend, &self.commit_key(candidate), Some(candidate))
            })?;
            let Some(checkpoint) = loaded.commit.checkpoint else {
                continue;
            };
            let bytes =
                self.with_backend(|backend| backend.read_all(&Key::manifest(checkpoint)))?;
            let value = crate::wcb_util::decode(&bytes)?;
            let decoded = crate::format::decode_state(&value)?;
            if decoded.generation != candidate {
                continue;
            }
            state = decoded;
            hash = loaded.hash;
            next = candidate + 1;
            break;
        }
        for candidate in next..=generation {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(backend, &self.commit_key(candidate), Some(candidate))
            })?;
            let expected_parent = if hash == Hash32::ZERO {
                None
            } else {
                Some(hash)
            };
            if loaded.commit.parent != expected_parent {
                return Err(Error::corrupt(format!(
                    "domain {} commit {candidate} does not name the previous commit as parent",
                    domain.to_hex()
                )));
            }
            state.apply_ops(&loaded.commit.ops)?;
            state.generation = candidate;
            hash = loaded.hash;
        }
        if hash != head_id {
            return Err(Error::corrupt(format!(
                "domain {} head {} does not match the replayed chain ({})",
                domain.to_hex(),
                head_id.to_hex(),
                hash.to_hex()
            )));
        }
        self.state = state;
        self.head = head_id;
        self.generation = generation;
        self.next_generation = generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        self.blob_index.clear();
        self.blob_index_ready = false;
        self.load_quota_record();
        Ok(())
    }

    /// Read the MW3 discovery record. An absent record means epoch 0 and no
    /// registered domains; a domain head remains authoritative regardless.
    pub fn discover_domains(&self) -> Result<crate::mw3::DomainIndex> {
        match self.with_backend(|backend| backend.read_all(&Key::domain_index())) {
            Ok(bytes) => crate::mw3::DomainIndex::decode(&bytes),
            Err(err) if err.class() == Class::NotFound => Ok(crate::mw3::DomainIndex::default()),
            Err(err) => Err(err),
        }
    }

    /// Resolve the discovery record into `domain -> head` pairs.
    ///
    /// The record enumerates the domains (and names each head's key); this call
    /// reads each head once. A domain whose head image is absent is reported as
    /// `None` rather than dropped, because the discovery record is not the
    /// source of truth.
    pub fn discover_heads(&self) -> Result<Vec<(crate::mw3::DomainEntry, Option<(u64, Hash32)>)>> {
        let index = self.discover_domains()?;
        let mut out = Vec::with_capacity(index.domains.len());
        for entry in index.domains {
            let head = self.with_backend(|backend| backend.load_domain_head(&entry.id))?;
            out.push((entry, head));
        }
        Ok(out)
    }

    /// Publish an append-only `weft.domain.link.v1` record under the
    /// successor's key.
    ///
    /// Create-once: the same record is idempotent and a different record for
    /// the same successor is `Conflict`. It never moves data and never picks a
    /// winner; it records ancestry for a split, supersede, or merge.
    pub fn write_domain_link(&mut self, link: &crate::mw3::DomainLink) -> Result<Hash32> {
        self.require_writable()?;
        if self.options.writer_mode != crate::mw2::WriterMode::Sharded {
            return Err(Error::invalid_argument(
                "domain links require --writer-mode sharded",
            ));
        }
        if self
            .with_backend(|backend| backend.load_domain_head(&link.successor))?
            .is_none()
        {
            return Err(Error::not_found(format!(
                "successor domain {} has no head; register it before linking it",
                link.successor.to_hex()
            )));
        }
        let bytes = link.encode()?;
        let id = link.id()?;
        let key = Key::domain_link(link.successor);
        match self.with_backend_mut(|backend| backend.put_atomic(&key, &bytes)) {
            Ok(()) => Ok(id),
            Err(err) if err.class() == Class::Exists => {
                let stored = self.with_backend(|backend| backend.read_all(&key))?;
                if stored == bytes {
                    Ok(id)
                } else {
                    Err(Error::conflict(
                        "this successor domain already has a different domain link record; \
                         links are append-only but a successor has exactly one",
                    ))
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Read the link record of one successor domain, if any.
    pub fn domain_link(&self, successor: Hash32) -> Result<Option<crate::mw3::DomainLink>> {
        match self.with_backend(|backend| backend.read_all(&Key::domain_link(successor))) {
            Ok(bytes) => {
                let value = crate::wcb_util::decode(&bytes)?;
                let link = crate::mw3::DomainLink::from_wcb(&value)?;
                if link.successor != successor {
                    return Err(Error::corrupt(
                        "a domain link record names a different successor than its key",
                    ));
                }
                Ok(Some(link))
            }
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Register an MW3 domain.
    ///
    /// One compare-and-swap on the discovery record, requiring the observed
    /// epoch; the per-domain head image is created first, so a crash leaves an
    /// unregistered head rather than a discovery entry that cannot be read.
    /// Registering the same record twice is idempotent and does not change the
    /// epoch.
    pub fn register_domain(&mut self, record: crate::mw3::DomainRecord) -> Result<Hash32> {
        self.require_writable()?;
        if self.options.writer_mode != crate::mw2::WriterMode::Sharded {
            return Err(Error::invalid_argument(
                "registering an MW3 domain requires --writer-mode sharded",
            ));
        }
        let entry = crate::mw3::DomainEntry::new(record)?;
        let id = entry.id;
        self.with_backend_mut(|backend| backend.ensure_domain_layout(&id))?;
        // A domain starts with the same namespace-root genesis an MW1 store has,
        // published through the ordinary commit path (no second implementation).
        // A domain that already has a head is left alone.
        if self
            .with_backend(|backend| backend.load_domain_head(&id))?
            .is_none()
        {
            let saved = (
                self.domain,
                std::mem::take(&mut self.state),
                self.head,
                self.generation,
                self.next_generation,
                self.options.attest_indexes,
            );
            self.domain = Some(id);
            self.state = State::default();
            self.head = Hash32::ZERO;
            self.generation = 0;
            self.next_generation = 1;
            // Domain commits carry no index attestation in this prototype.
            self.options.attest_indexes = false;
            let genesis = self.write_genesis();
            self.domain = saved.0;
            self.state = saved.1;
            self.head = saved.2;
            self.generation = saved.3;
            self.next_generation = saved.4;
            self.options.attest_indexes = saved.5;
            match genesis {
                Ok(()) => {}
                // A concurrent registration published the genesis first.
                Err(err) if err.class() == Class::Busy => {
                    if self
                        .with_backend(|backend| backend.load_domain_head(&id))?
                        .is_none()
                    {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        const MAX_ATTEMPTS: u32 = 64;
        for _ in 0..MAX_ATTEMPTS {
            let existing = match self.with_backend(|backend| backend.read_all(&Key::domain_index()))
            {
                Ok(bytes) => Some(bytes),
                Err(err) if err.class() == Class::NotFound => None,
                Err(err) => return Err(err),
            };
            let mut index = match &existing {
                Some(bytes) => crate::mw3::DomainIndex::decode(bytes)?,
                None => crate::mw3::DomainIndex::default(),
            };
            if !index.insert(entry.clone())? {
                return Ok(id);
            }
            let bytes = index.encode()?;
            let expected = existing
                .as_ref()
                .map(|bytes| crate::planes::control_token(bytes));
            let result = self.with_backend_mut(|backend| {
                backend.begin_publication()?;
                let out =
                    backend.compare_and_swap(&Key::domain_index(), expected.as_deref(), &bytes);
                let _ = backend.end_publication();
                out
            });
            match result {
                Ok(()) => return Ok(id),
                Err(err) if err.class() == Class::Busy => continue,
                Err(err) => return Err(err),
            }
        }
        Err(Error::busy(
            "domain registration lost its discovery-record race 64 times",
        ))
    }

    /// Capture a declared read view over the given domains.
    ///
    /// Each head is read **once**, in the declared order. The result is a
    /// capture, not a global linearizable point; §15.4 states the isolation
    /// sentence. A domain with no head is `NotFound` and names the DomainId.
    pub fn capture_read_view(&self, domains: &[Hash32]) -> Result<crate::mw3::ReadView> {
        let mut pairs = Vec::with_capacity(domains.len());
        for domain in domains {
            let head = self.with_backend(|backend| backend.load_domain_head(domain))?;
            let Some((generation, head)) = head else {
                return Err(Error::not_found(format!(
                    "no such MW3 domain {} (no head image)",
                    domain.to_hex()
                )));
            };
            pairs.push(crate::mw3::ViewPair {
                domain: *domain,
                generation,
                head,
            });
        }
        crate::mw3::ReadView::capture(pairs)
    }

    /// Store a durable cross-domain view pin (`weft.view.pin.v1`) as a
    /// content-addressed blob. The record states what was observed; it is not a
    /// retention guarantee in this prototype (§15.5(a)).
    pub fn write_view_pin(
        &self,
        view: &crate::mw3::ReadView,
        principal: &str,
        pinned_ns: i64,
    ) -> Result<Hash32> {
        self.require_writable()?;
        let bytes = crate::mw3::encode_view_pin(view, principal, pinned_ns)?;
        self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &bytes)
    }

    /// Confirm or create the writer-mode policy record.
    ///
    /// An absent record means `single`, and a single-mode open never writes one
    /// (frozen fixtures stay byte-identical). The first optimistic open creates
    /// it create-once. A mode mismatch is refused `UnsupportedVersion`: mixing
    /// an exclusive writer with optimistic committers on one store is not a
    /// supported configuration.
    fn ensure_writer_policy(&mut self, mode: crate::mw2::WriterMode) -> Result<()> {
        let key = Key::writers();
        let existing = self.with_backend(|backend| -> Result<Option<Vec<u8>>> {
            match backend.read_all(&key) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(err) if err.class() == Class::NotFound => Ok(None),
                Err(err) => Err(err),
            }
        })?;
        if let Some(bytes) = existing {
            let stored = crate::mw2::decode_writers_mode(&bytes)?;
            if stored != mode {
                return Err(Error::unsupported_version(format!(
                    "store writer mode is `{}`; this handle requested `{}`. Writer modes are \
                     not mixed. Use --writer-mode {} or a separate store.",
                    stored.as_str(),
                    mode.as_str(),
                    stored.as_str()
                )));
            }
            return Ok(());
        }
        if mode == crate::mw2::WriterMode::Single {
            return Ok(());
        }
        let bytes = crate::mw2::encode_writers_record(
            mode,
            concat!("weftfs ", env!("CARGO_PKG_VERSION")),
            self.now_ns(),
        )?;
        match self.with_backend_mut(|backend| backend.put_atomic(&key, &bytes)) {
            Ok(()) => Ok(()),
            Err(err) if err.class() == Class::Exists => {
                let stored = self.with_backend(|backend| backend.read_all(&key))?;
                if crate::mw2::decode_writers_mode(&stored)? == mode {
                    Ok(())
                } else {
                    Err(Error::unsupported_version(
                        "a concurrent open created a different writer-mode policy record",
                    ))
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Open or replace the T026 history sink from the final options.
    ///
    /// An explicitly named path that cannot be opened fails the open: a caller
    /// that asked for a history must not silently get none.
    fn attach_history(&mut self, options: &Options) -> Result<()> {
        self.history = crate::history::HistorySink::from_options(options)?;
        Ok(())
    }

    /// Append one history record when a sink is configured.
    pub(crate) fn emit_history(&self, mut event: serde_json::Value) {
        let Some(sink) = self.history.as_ref() else {
            return;
        };
        if let serde_json::Value::Object(map) = &mut event {
            map.entry("handle".to_owned())
                .or_insert_with(|| serde_json::json!(self.history_handle));
            if let Some(domain) = self.domain {
                map.entry("domain".to_owned())
                    .or_insert_with(|| serde_json::json!(domain.to_hex()));
            }
        }
        sink.emit(event);
    }

    /// The handle identity recorded in the history.
    pub fn history_handle(&self) -> &str {
        &self.history_handle
    }

    /// The writer mode recorded in the store, if any. An absent record means
    /// `single` (MW1).
    pub fn recorded_writer_mode(&self) -> Result<Option<crate::mw2::WriterMode>> {
        self.with_backend(|backend| crate::mw2::WriterMode::policy(backend))
    }

    /// Explicitly migrate the store's recorded writer mode (T028).
    ///
    /// **One compare-and-swap** on the `writers` record is the only
    /// publication: before it the store is `from`, after it the store is `to`,
    /// and there is no instant at which two modes are both recorded. A crash
    /// before the CAS leaves `from`; a crash after it leaves `to`. The caller
    /// then reopens normally, and the recorded mode decides what is allowed.
    ///
    /// Allowed transitions:
    /// * `single` -> `optimistic` (the same state a first optimistic open
    ///   creates, made explicit);
    /// * `optimistic` -> `sharded` (adding conflict domains).
    ///
    /// Refused with a typed error, never approximated:
    /// * any downgrade (`optimistic`/`sharded` -> `single`);
    /// * `sharded` -> `optimistic`;
    /// * `single` -> `sharded` (a global head cannot be promoted into a domain
    ///   automatically; register a domain and publish an explicit link);
    /// * `optimistic` -> `sharded` on a plane without per-domain heads.
    ///
    /// Quiescence: a live prepared transaction blocks the migration (`Busy`,
    /// naming the transaction), and `quiesce` must pass with the published head
    /// unchanged. That is a **one-host heuristic**, not a barrier: a writer that
    /// is idle for the whole window and publishes afterwards is not detected.
    /// The residual risk is stated in `spec/WEFT-MULTIWRITER.md` §16.4.
    pub fn migrate_writer_mode(
        &mut self,
        from: crate::mw2::WriterMode,
        to: crate::mw2::WriterMode,
        quiesce: std::time::Duration,
    ) -> Result<crate::mw2::WriterMode> {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "a writer-mode migration cannot run inside a group commit",
            ));
        }
        let recorded = self
            .recorded_writer_mode()?
            .unwrap_or(crate::mw2::WriterMode::Single);
        if recorded != from {
            return Err(Error::unsupported_version(format!(
                "store writer mode is `{}`, not `{}`; a migration must name the mode the store \
                 actually records",
                recorded.as_str(),
                from.as_str()
            )));
        }
        if from == to {
            return Ok(recorded);
        }
        match (from, to) {
            (crate::mw2::WriterMode::Single, crate::mw2::WriterMode::Optimistic) => {}
            (crate::mw2::WriterMode::Optimistic, crate::mw2::WriterMode::Sharded) => {
                if !self
                    .with_backend(|backend| Ok(backend.supports_domain_heads()))
                    .unwrap_or(false)
                {
                    return Err(Error::unsupported_version(
                        "this commit plane has no per-domain heads; `sharded` is refused rather \
                         than downgraded to a global head",
                    ));
                }
            }
            (crate::mw2::WriterMode::Single, crate::mw2::WriterMode::Sharded) => {
                return Err(Error::unsupported_version(
                    "a global (MW1) head cannot be promoted into an MW3 conflict domain \
                     automatically; register a domain with `weftfs mw3 register`, publish the \
                     state into it explicitly, and record the lineage with `weftfs mw3 link`",
                ))
            }
            (_, crate::mw2::WriterMode::Single) => {
                return Err(Error::unsupported_version(format!(
                    "downgrading `{}` to `single` is refused: the recorded mode is what prevents \
                     a single-writer binary from believing it is the only authority on a store \
                     that has run independent committers",
                    from.as_str()
                )))
            }
            _ => {
                return Err(Error::unsupported_version(format!(
                    "the migration `{}` -> `{}` is not supported; MW3 domains are not a superset \
                     of a global head",
                    from.as_str(),
                    to.as_str()
                )))
            }
        }

        // A live prepared transaction blocks the transition: it was prepared
        // against the old mode and may publish under it after the record moved.
        let tx_keys =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Tx)))?;
        let mut live = Vec::new();
        for key in tx_keys {
            let bytes = self.with_backend(|backend| backend.read_all(&key))?;
            let record = crate::mw2::decode_tx_record(&bytes)?;
            if record.state == crate::mw2::TxRecordState::Prepared {
                live.push(record.id);
            }
        }
        if !live.is_empty() {
            live.sort();
            return Err(Error::busy(format!(
                "{} prepared transaction(s) are unfinished ({}); resolve them by transaction id \
                 before migrating the writer mode",
                live.len(),
                live.join(", ")
            )));
        }

        // Head stability over the quiescence window (one host; see the note).
        if !quiesce.is_zero() {
            let before = self.with_backend(|backend| backend.load_published_head())?;
            std::thread::sleep(quiesce);
            let after = self.with_backend(|backend| backend.load_published_head())?;
            if before != after {
                return Err(Error::busy(
                    "the published head advanced during the migration's quiescence window; \
                     quiesce the committers and retry",
                ));
            }
        }
        crash_point("mode_after_validate");

        let bytes = crate::mw2::encode_writers_record(
            to,
            concat!("weftfs ", env!("CARGO_PKG_VERSION")),
            self.wall_ns(),
        )?;
        let current = match self.with_backend(|backend| backend.read_all(&Key::writers())) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.class() == Class::NotFound => None,
            Err(err) => return Err(err),
        };
        let token = current.as_deref().map(crate::planes::control_token);
        let result = self.with_backend_mut(|backend| {
            backend.begin_publication()?;
            let out = backend.compare_and_swap(&Key::writers(), token.as_deref(), &bytes);
            let _ = backend.end_publication();
            out
        });
        match result {
            Ok(()) => {}
            Err(err) if err.class() == Class::Busy => {
                return Err(Error::busy(
                    "the writer-mode record changed during the migration; reopen and retry",
                ))
            }
            Err(err) => return Err(err),
        }
        self.options.writer_mode = to;
        crash_point("mode_after_cas");
        Ok(to)
    }

    /// Re-derive the R8 read layers from `self.options`.
    ///
    /// Called once at open (and by [`Store::set_read_layer_options`]). Never
    /// called while reads are in flight: the cache is resized and the flight
    /// configuration is replaced in place.
    fn apply_read_layer_options(&mut self) {
        self.single_flight
            .set_options(SingleFlightOptions::from_options(&self.options));
        self.object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_options(ObjectCacheOptions::from_options(&self.options));
        self.cache_enabled.store(
            self.options.cache_bytes > 0,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Replace the R8 read-layer configuration on an open store.
    ///
    /// Must not be called concurrently with reads: shrinking the cache bound is
    /// applied by evicting entries (never by returning a stale value), and the
    /// single-flight configuration is replaced, not merged. The CLI applies the
    /// options at open, so this exists for tests and long-lived embedders that
    /// quiesce their readers first.
    pub fn set_read_layer_options(&mut self, options: Options) -> Result<()> {
        validate_options(&options)?;
        self.options = options;
        self.apply_read_layer_options();
        Ok(())
    }

    /// Enable or disable the decoded-object cache on an open store.
    ///
    /// Shrinking evicts down to the new bound immediately. Disabling clears
    /// the cache (pins are retained). Must not run concurrently with reads.
    pub fn set_cache_bytes(&mut self, bytes: u64) {
        self.options.cache_bytes = bytes;
        self.apply_read_layer_options();
    }

    /// Enable or disable single-flight on an open store.
    pub fn set_single_flight(&mut self, enabled: bool) {
        self.options.single_flight = enabled;
        self.apply_read_layer_options();
    }

    /// The backend selected for this store.
    pub fn backend_kind(&self) -> &BackendKind {
        &self.backend_kind
    }

    /// Declared capabilities of the active backend.
    pub fn backend_capabilities(&self) -> Capabilities {
        self.with_backend(|backend| Ok(backend.capabilities()))
            .unwrap_or_else(|_| self.backend_kind.capabilities())
    }

    /// Whether a durable publication is the backend's *provider acknowledgement*
    /// rather than a local file system barrier.
    ///
    /// The planned writer's early-barrier overlap only exists for a backend that
    /// can be told to `fsync` a published file before the commit record. A
    /// remote object backend has no such file: its `PUT` returns after the
    /// provider accepted the bytes, so there is nothing to drain early and no
    /// barrier may be claimed. Read from the open backend, not from the
    /// configuration, so a probed capability (a split store's actual blob plane)
    /// is what the writer reports.
    pub fn backend_durability_is_provider_ack(&self) -> bool {
        self.with_backend(|backend| Ok(backend.durability_is_provider_ack()))
            .unwrap_or_else(|_| self.backend_kind.blobs_are_provider_acked())
    }

    /// Physical I/O counters for receipts and tests.
    pub fn backend_stats(&self) -> BackendStats {
        let control = self
            .with_backend(|backend| Ok(backend.stats()))
            .unwrap_or_default();
        // Reads served by pooled immutable handles are counted on those
        // handles; merge their harvested deltas so a receipt reports one
        // request stream.
        control.merged(&self.read_pool.harvested())
    }

    /// Backend diagnostics (for example a publication-primitive fallback).
    pub fn backend_diagnostics(&self) -> Vec<String> {
        self.with_backend(|backend| Ok(backend.diagnostics()))
            .unwrap_or_default()
    }

    /// Physical packing counters when a batching blob plane is active.
    ///
    /// `None` for every store that publishes one physical object per blob.
    pub fn pack_stats(&self) -> Option<crate::packs::PackStats> {
        self.with_backend(|backend| Ok(backend.blob_plane_stats()))
            .ok()
            .flatten()
    }

    /// Seal and publish every blob a batching plane is holding in memory.
    ///
    /// The commit path already calls this before a commit record is published,
    /// so a caller needs it only when it must make blob publication visible
    /// *before* a commit: the prepare/commit checkpoint writer seals each
    /// worker's batch so the committer can verify the members it names.
    /// Idempotent, and a no-op for a plane that publishes eagerly.
    pub fn flush_blob_batch(&self) -> Result<()> {
        self.with_backend_mut(|backend| backend.flush_buffered_blobs())
    }

    /// Drop a batching plane's cached catalog so the next access re-reads it.
    ///
    /// Blobs buffered in an unsealed open pack are kept; only metadata is
    /// dropped. A no-op for a plane without cached metadata.
    pub fn refresh_blob_catalog(&self) -> Result<()> {
        self.with_backend_mut(|backend| backend.refresh_blob_metadata())
    }

    /// Switch this handle's blob plane into deferred pack-catalog mode.
    ///
    /// Returns `true` when the plane packs blobs and will now publish packs
    /// without publishing catalog metadata; the caller must hand the deferred
    /// members to one owner with [`Store::take_pending_pack_members`] and
    /// [`Store::adopt_pending_pack_members`]. Returns `false` for a store that
    /// does not pack, which is a no-op.
    pub fn defer_pack_catalog(&self) -> Result<bool> {
        self.with_backend_mut(|backend| backend.defer_pack_catalog())
    }

    /// Take every pack member this handle published without cataloguing it.
    ///
    /// `None` for a handle that is not in deferred mode. The open pack is
    /// sealed first, so the returned members always name published packs.
    pub fn take_pending_pack_members(&self) -> Result<Option<crate::packs::DeferredPackEntries>> {
        self.with_backend_mut(|backend| backend.take_pending_pack_members())
    }

    /// Merge another handle's deferred members into this handle's catalog.
    ///
    /// Exactly one handle (the committer) calls this, so the catalog root is
    /// advanced by a single writer: no control-record race exists.
    pub fn adopt_pending_pack_members(
        &self,
        members: crate::packs::DeferredPackEntries,
    ) -> Result<()> {
        self.with_backend_mut(|backend| backend.adopt_pending_pack_members(members))
    }

    /// Per-plane capabilities when the store runs a split backend.
    pub fn plane_capabilities(&self) -> Option<crate::planes::PlaneCapabilities> {
        self.with_backend(|backend| Ok(backend.plane_capabilities()))
            .ok()
            .flatten()
    }

    /// Per-plane request and byte counters when the store runs a split
    /// backend. A single backend reports `None`, because its one request
    /// stream is already exposed by [`Store::backend_stats`].
    pub fn plane_stats(&self) -> Option<crate::planes::PlaneStats> {
        let mut stats = self
            .with_backend(|backend| Ok(backend.plane_stats()))
            .ok()
            .flatten()?;
        // Every pooled read addresses an immutable blob (`objects/` or
        // `manifests/`), so its harvested counters belong to the blob plane.
        stats.blobs = stats.blobs.merged(&self.read_pool.harvested());
        Some(stats)
    }

    /// Read and verify an immutable content-addressed blob.
    ///
    /// `objects/` and `manifests/` bytes never change once published. This
    /// helper is the single read path for them: it takes a handle from the
    /// read pool (so the store's state mutex is not involved), releases the
    /// handle, and only then verifies the domain-separated hash. The bytes are
    /// attributed to the tensor being decoded when the probe is enabled.
    pub(crate) fn read_blob_verified_keyed(
        &self,
        kind: BlobKind,
        hash: Hash32,
        domain: &str,
    ) -> Result<Vec<u8>> {
        let key = match kind {
            BlobKind::Object => Key::object(hash),
            BlobKind::Manifest => Key::manifest(hash),
        };
        let (io_phase, hash_phase) = match kind {
            BlobKind::Object => (Phase::BlobIoObject, Phase::BlobHashObject),
            BlobKind::Manifest => (Phase::BlobIoManifest, Phase::BlobHashManifest),
        };
        let (bytes, io_ns) = {
            let _io = phase_profile::scope(io_phase);
            let started = Instant::now();
            // A batching blob plane keeps a member it just published in its own
            // open pack (or its deferred list) until the catalog names it. That
            // member is resolvable only through this handle, so while one is
            // buffered the read must not go to the pool: a pooled handle is a
            // different plane instance and would report the key as missing.
            // Restores read only committed blobs, so they keep the pool.
            let buffered = self
                .buffered_blob_kinds
                .load(std::sync::atomic::Ordering::Acquire)
                & match kind {
                    BlobKind::Object => crate::packs::BUFFERED_OBJECT,
                    BlobKind::Manifest => crate::packs::BUFFERED_MANIFEST,
                }
                != 0;
            let outcome = if self.read_pool.poolable() && !buffered {
                self.read_pool.read_all(&key)
            } else {
                if buffered {
                    self.buffered_blob_reads
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.with_backend(|backend| backend.read_all(&key))
                    .map(|bytes| (bytes, started.elapsed().as_nanos() as u64))
            };
            outcome.map_err(|err| blob_read_failure(&key, err))?
        };
        if self.lock_probe.enabled() {
            self.lock_probe.note_io(io_ns);
        }
        self.note_probe_bytes(bytes.len() as u64);
        let actual = phase_profile::timed(hash_phase, || identity::h(domain, &bytes));
        if actual != hash {
            return Err(Error::corrupt(format!(
                "blob {} fails its hash check (expected {hash}, got {actual})",
                key.display_path()
            )));
        }
        Ok(bytes)
    }

    /// Read an immutable manifest by id and decode it. The blob hash is
    /// verified against the manifest id before the canonical bytes are parsed.
    pub(crate) fn read_manifest_keyed(&self, id: Hash32) -> Result<Manifest> {
        let bytes =
            self.read_blob_verified_keyed(BlobKind::Manifest, id, identity::MANIFEST_DOMAIN)?;
        let _decode = phase_profile::scope(Phase::ManifestDecode);
        let value = crate::wcb_util::decode(&bytes)?;
        Manifest::from_wcb(&value)
    }

    /// Read a checkpoint state manifest by id and decode it.
    pub(crate) fn read_state_manifest_keyed(&self, id: Hash32) -> Result<State> {
        let bytes =
            self.read_blob_verified_keyed(BlobKind::Manifest, id, identity::MANIFEST_DOMAIN)?;
        let value = crate::wcb_util::decode(&bytes)?;
        crate::format::decode_state(&value)
    }

    /// Capability evidence from the writable open, if any.
    /// The handle's KV fast-path decode cache (crate-internal; the caches hold
    /// decode results only and are fenced by the head generation).
    pub(crate) fn kv_read_cache(&self) -> &Mutex<crate::kv_capability::KvReadCache> {
        &self.kv_read_cache
    }

    pub fn plane_probes(&self) -> Option<&crate::planes::PlaneProbes> {
        self.plane_probes.as_ref()
    }

    /// Read a backend key. Test/diagnostic surface: the store's normal API
    /// addresses content through object and manifest identities.
    #[doc(hidden)]
    pub fn backend_read_key(&self, key: &Key) -> Result<Vec<u8>> {
        self.with_backend(|backend| backend.read_all(key))
    }

    pub(crate) fn with_backend<T>(
        &self,
        f: impl FnOnce(&dyn StorageBackend) -> Result<T>,
    ) -> Result<T> {
        if !self.lock_probe.enabled() {
            let guard = self
                .backend
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            return f(&**guard);
        }
        let _demand = self.lock_probe.enter();
        let mut timer = crate::lock_probe::SectionTimer::new(&self.lock_probe);
        let guard = self
            .backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        timer.acquired();
        let out = f(&**guard);
        drop(timer);
        out
    }

    pub(crate) fn with_backend_mut<T>(
        &self,
        f: impl FnOnce(&mut dyn StorageBackend) -> Result<T>,
    ) -> Result<T> {
        if !self.lock_probe.enabled() {
            let mut guard = self
                .backend
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Publish first, observe after: a reader racing this mutation must
            // never see a narrow mask while a member is unbuffered-but-not-yet
            // published. Widening is conservative (it can only route a read
            // through this handle, never away from it).
            self.buffered_blob_kinds.store(
                crate::packs::BUFFERED_OBJECT | crate::packs::BUFFERED_MANIFEST,
                std::sync::atomic::Ordering::Release,
            );
            let out = f(&mut **guard);
            let buffered = guard.buffered_blob_kinds();
            drop(guard);
            self.buffered_blob_kinds
                .store(buffered, std::sync::atomic::Ordering::Release);
            // Every mutation can add or replace a content-addressed blob, so
            // pooled reader handles must not be reused across it (a packed
            // handle caches the catalog as of its creation).
            self.read_pool.invalidate();
            return out;
        }
        let _demand = self.lock_probe.enter();
        let mut timer = crate::lock_probe::SectionTimer::new(&self.lock_probe);
        let mut guard = self
            .backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        timer.acquired();
        self.buffered_blob_kinds.store(
            crate::packs::BUFFERED_OBJECT | crate::packs::BUFFERED_MANIFEST,
            std::sync::atomic::Ordering::Release,
        );
        let out = f(&mut **guard);
        let buffered = guard.buffered_blob_kinds();
        drop(timer);
        drop(guard);
        self.buffered_blob_kinds
            .store(buffered, std::sync::atomic::Ordering::Release);
        self.read_pool.invalidate();
        out
    }

    /// Enable or disable the backend-lock probe (subphase R13). Off by
    /// default; a probe run publishes exactly the bytes a non-probe run does.
    pub fn set_lock_probe(&self, enabled: bool) {
        self.lock_probe.set_enabled(enabled);
    }

    /// Whether the backend-lock probe is recording.
    pub fn lock_probe_enabled(&self) -> bool {
        self.lock_probe.enabled()
    }

    /// Discard every probe sample and per-tensor row.
    pub fn reset_lock_probe(&self) {
        self.lock_probe.reset();
    }

    /// Everything the backend-lock probe has recorded since the last reset.
    pub fn lock_probe_report(&self) -> crate::lock_probe::LockProbeReport {
        self.lock_probe.report()
    }

    /// Immutable-read pool counters (subphase R13).
    pub fn read_pool_stats(&self) -> crate::read_pool::ReadPoolStats {
        let mut stats = self.read_pool.stats();
        stats.buffered_blob_reads = self
            .buffered_blob_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        stats
    }

    /// Set the cap on simultaneously active immutable-read handles (X07).
    ///
    /// `0` disables the cap and restores the pre-X07 behaviour; the default is
    /// [`crate::read_pool::MAX_ACTIVE_HANDLES`]. Applies to reads that start
    /// after the call; leases already held finish under the cap they acquired.
    /// Epoch invalidation, the buffered-pack bypass and every error class are
    /// unchanged.
    pub fn set_read_pool_active_cap(&self, cap: usize) {
        self.read_pool.set_active_cap(cap);
    }

    /// The configured active-handle cap (`0` = unbounded).
    pub fn read_pool_active_cap(&self) -> usize {
        self.read_pool.active_cap()
    }

    /// Begin attributing backend entries on this thread to one tensor input.
    /// The returned guard publishes a row on drop; a no-op when the probe is
    /// disabled.
    pub(crate) fn begin_tensor_probe(&self, index: usize) -> Option<TensorProbeGuard<'_>> {
        if self.lock_probe.enabled() {
            Some(TensorProbeGuard {
                _guard: self.lock_probe.begin_tensor(index),
            })
        } else {
            None
        }
    }

    /// Attribute bytes returned by a backend read to the tensor being decoded
    /// on this thread.
    pub(crate) fn note_probe_bytes(&self, bytes: u64) {
        if self.lock_probe.enabled() {
            self.lock_probe.note_bytes(bytes);
        }
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

    /// Chunk blobs read by the most recent range write. `#[doc(hidden)]`
    /// test/receipt hook; it is not part of the format.
    #[doc(hidden)]
    pub fn range_write_chunk_reads(&self) -> u64 {
        self.range_write_chunk_reads.load(Ordering::Relaxed)
    }

    /// Reset [`Store::range_write_chunk_reads`].
    #[doc(hidden)]
    pub fn reset_range_write_chunk_reads(&self) {
        self.range_write_chunk_reads.store(0, Ordering::Relaxed);
    }

    fn note_range_write_chunk_read(&self) {
        self.range_write_chunk_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// True when this handle was opened with the opt-in overlay representation,
    /// so a write that creates a new version may publish a `weft.manifest.v4`
    /// layer.
    fn overlay_mode(&self) -> bool {
        self.options.representation == RepresentationMode::Overlay
    }

    /// Record one successful object read in the advisory counter table and
    /// flush periodically. Never fails and never affects the read result.
    pub(crate) fn note_object_read(&self, id: Hash32) {
        let mut guard = self
            .access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.note_read(id);
        if guard.should_flush() && self.mode == OpenMode::ReadWrite {
            let _ = self.with_backend_mut(|backend| guard.flush(backend));
        }
    }

    /// Persist any pending read counters. Read-only opens never write, so this
    /// is a no-op for them; callers that want counters to survive a read-only
    /// process must reopen read-write (the bench harness does).
    pub fn flush_access_counts(&self) -> Result<()> {
        if self.mode != OpenMode::ReadWrite {
            return Ok(());
        }
        let mut guard = self
            .access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.with_backend_mut(|backend| guard.flush(backend))?;
        Ok(())
    }

    /// Number of observed reads retained for one object.
    pub fn access_reads(&self, id: Hash32) -> u64 {
        self.access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reads(id)
    }

    /// Every retained counter, ascending by object id.
    pub fn access_entries(&self) -> std::collections::BTreeMap<Hash32, u64> {
        self.access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries()
            .clone()
    }

    /// Consume (reset) one object's counter. Used by rebase, which must be
    /// idempotent: a second run sees zero reads and does nothing.
    pub fn consume_access_reads(&self, id: Hash32) -> u64 {
        self.access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .consume(id)
    }

    /// Diagnostics from the access-counter load path.
    pub fn access_notes(&self) -> Vec<String> {
        self.access
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes()
            .to_vec()
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
    ///
    /// On a cache miss the advisory segment `indexes/<ab>/<commit>.idx` is
    /// tried first. It is used only when the commit key, generation, all three
    /// counts, and the integrity checksum agree with `state`; any failure is
    /// ignored, recorded as a diagnostic note, and the index is rebuilt.
    pub(crate) fn state_index(&self, snapshot: Hash32, state: &State) -> Arc<StateIndex> {
        // Inside a group the state changes while `head` does not, so a cached
        // index keyed by the commit hash can no longer be validated by
        // `matches_state` alone (an attribute-only edit keeps every count and
        // the generation unchanged). Rebuild per call instead of serving a
        // stale candidate set; the index may only ever over-approximate.
        if self.group_depth != 0 {
            self.bump_index_stat(|stats| stats.builds += 1);
            return Arc::new(StateIndex::build(state));
        }
        let cached = {
            let mut cache = self
                .index_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.get(snapshot)
        };
        if let Some(index) = cached {
            if index.matches_state(state) {
                return index;
            }
        }
        if let Some(index) = self.load_persisted_index(snapshot, state) {
            self.index_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(snapshot, index.clone());
            return index;
        }
        let index = Arc::new(StateIndex::build(state));
        self.bump_index_stat(|stats| stats.builds += 1);
        self.index_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(snapshot, index.clone());
        index
    }

    /// Load, validate, and return the advisory index for `commit`.
    ///
    /// `None` means "rebuild from state": the file is absent, unreadable,
    /// truncated, fails its integrity digest, decodes non-canonically, names a
    /// different commit, or disagrees with the state's generation/counts.
    fn load_persisted_index(&self, commit: Hash32, state: &State) -> Option<Arc<StateIndex>> {
        let key = Key::index(commit);
        let displayed = key.display_path();
        let bytes = match self.with_backend(|backend| backend.read_all(&key)) {
            Ok(bytes) => bytes,
            Err(err) if err.class() == crate::error::Class::NotFound => return None,
            Err(err) => {
                self.note_index_reject(format!("cannot read index {displayed}: {err}"));
                return None;
            }
        };
        if bytes.len() >= 4 && &bytes[0..4] == crate::index_chain::MAGIC_V3 {
            return self.load_wfx3_index(commit, state, &bytes, &displayed);
        }
        self.load_wfx2_index(commit, state, &bytes, &displayed)
    }

    /// Legacy WFX2 full segment (read-only compatibility).
    fn load_wfx2_index(
        &self,
        commit: Hash32,
        state: &State,
        bytes: &[u8],
        displayed: &str,
    ) -> Option<Arc<StateIndex>> {
        if bytes.len() as u64 > MAX_INDEX_FILE_BYTES {
            self.note_index_reject(format!(
                "index {displayed} is {} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap",
                bytes.len()
            ));
            return None;
        }
        let index = match StateIndex::decode_file(bytes) {
            Ok(index) => index,
            Err(err) => {
                self.note_index_reject(format!("ignoring index {displayed}: {err}"));
                return None;
            }
        };
        if index.commit() != commit {
            self.note_index_reject(format!(
                "ignoring index {displayed}: declares commit {} but is stored under {}",
                index.commit(),
                commit
            ));
            return None;
        }
        if !index.matches_state(state) {
            self.note_index_reject(format!(
                "ignoring index {displayed}: generation/counts disagree with the replayed state"
            ));
            return None;
        }
        // Trust requires the commit to attest this index. A self-consistent
        // segment (valid digest, matching generation and counts) can still be a
        // forged *subset*; only the commit's `candidates_root` can refute that.
        let Some(attestation) = self.attested_index(commit, index.generation()) else {
            self.note_index_reject(format!(
                "ignoring index {displayed}: the commit carries no index attestation (v1 \
                 commit or unverifiable record); rebuild-only"
            ));
            return None;
        };
        if attestation.is_v3() {
            self.note_index_reject(format!(
                "ignoring index {displayed}: a v3 commit names a segment chain, not a WFX2 file"
            ));
            return None;
        }
        if !self.wfx_attestation_matches(
            &index,
            commit,
            index.generation(),
            &attestation,
            displayed,
        ) {
            return None;
        }
        self.bump_index_stat(|stats| stats.disk_loads += 1);
        Some(Arc::new(index))
    }

    /// WFX3 full or delta segment, resolved against its attested chain.
    fn load_wfx3_index(
        &self,
        commit: Hash32,
        state: &State,
        bytes: &[u8],
        displayed: &str,
    ) -> Option<Arc<StateIndex>> {
        if bytes.len() as u64 > MAX_INDEX_FILE_BYTES {
            self.note_index_reject(format!(
                "index {displayed} is {} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap",
                bytes.len()
            ));
            return None;
        }
        let segment = match crate::index_chain::decode_segment(bytes) {
            Ok(segment) => segment,
            Err(err) => {
                self.note_index_reject(format!("ignoring index {displayed}: {err}"));
                return None;
            }
        };
        let header = segment.header;
        let resolved =
            match crate::index_chain::resolve_chain_segment(segment, |base_commit, _base_id| {
                self.with_backend(|backend| backend.read_all(&Key::index(base_commit)))
            }) {
                Ok(resolved) => resolved,
                Err(err) => {
                    self.note_index_reject(format!(
                        "ignoring index {displayed}: cannot resolve its segment chain: {err}"
                    ));
                    return None;
                }
            };
        let index = resolved.index.with_resolved_commit(commit);
        if header.generation != index.generation() {
            self.note_index_reject(format!(
                "ignoring index {displayed}: header generation differs from the resolved index"
            ));
            return None;
        }
        if !index.matches_state(state) {
            self.note_index_reject(format!(
                "ignoring index {displayed}: generation/counts disagree with the replayed state"
            ));
            return None;
        }
        let Some(attestation) = self.attested_index(commit, index.generation()) else {
            self.note_index_reject(format!(
                "ignoring index {displayed}: the commit carries no index attestation"
            ));
            return None;
        };
        if !attestation.is_v3() {
            self.note_index_reject(format!(
                "ignoring index {displayed}: a v1/v2 commit cannot attest a WFX3 chain"
            ));
            return None;
        }
        if !self.wfx_attestation_matches(
            &index,
            commit,
            index.generation(),
            &attestation,
            displayed,
        ) {
            return None;
        }
        // The chain itself is attested: kind, immediate base, depth, and the
        // root over every segment id from the head to the full ancestor.
        if attestation.segment_kind != Some(resolved.chain.kind)
            || attestation.segment_base != resolved.chain.base()
            || attestation.segment_depth != resolved.chain.depth()
        {
            self.note_index_reject(format!(
                "ignoring index {displayed}: segment chain shape disagrees with the commit \
                 attestation (forged or partial chain); rebuild-only"
            ));
            return None;
        }
        let root = match resolved.chain.root(index.generation()) {
            Ok(root) => root,
            Err(err) => {
                self.note_index_reject(format!(
                    "ignoring index {displayed}: cannot derive the chain root: {err}"
                ));
                return None;
            }
        };
        if root != attestation.segment_root {
            self.note_index_reject(format!(
                "ignoring index {displayed}: segment root disagrees with the commit attestation \
                 (forged or partial chain); rebuild-only"
            ));
            return None;
        }
        self.bump_index_stat(|stats| stats.disk_loads += 1);
        Some(Arc::new(index))
    }

    /// Shared count/candidates/state-root checks for a loaded segment.
    fn wfx_attestation_matches(
        &self,
        index: &StateIndex,
        commit: Hash32,
        generation: u64,
        attestation: &IndexAttestation,
        displayed: &str,
    ) -> bool {
        if attestation.object_count != index.object_count() as u64
            || attestation.relation_count != index.relation_count() as u64
            || attestation.entry_count != index.entry_count() as u64
        {
            self.note_index_reject(format!(
                "ignoring index {displayed}: counts disagree with the commit attestation"
            ));
            return false;
        }
        let candidates = match index.candidates_root() {
            Ok(value) => value,
            Err(err) => {
                self.note_index_reject(format!(
                    "ignoring index {displayed}: cannot re-derive its candidate root: {err}"
                ));
                return false;
            }
        };
        if candidates != attestation.candidates_root {
            self.note_index_reject(format!(
                "ignoring index {displayed}: candidate root disagrees with the commit \
                 attestation (forged or incomplete index); rebuild-only"
            ));
            return false;
        }
        let (_, record) = match self.read_commit_record(generation) {
            Some(record) => record,
            None => {
                self.note_index_reject(format!(
                    "ignoring index {displayed}: cannot re-read its commit record"
                ));
                return false;
            }
        };
        if record.id().ok() != Some(commit) {
            self.note_index_reject(format!(
                "ignoring index {displayed}: commit record hash mismatch"
            ));
            return false;
        }
        let state_root = match record.encode_fields() {
            Ok(bytes) => identity::h(INDEX_STATE_DOMAIN, &bytes),
            Err(err) => {
                self.note_index_reject(format!(
                    "ignoring index {displayed}: cannot recompute the attested state root: {err}"
                ));
                return false;
            }
        };
        if state_root != attestation.state_root {
            self.note_index_reject(format!(
                "ignoring index {displayed}: attested state root disagrees with the commit record"
            ));
            return false;
        }
        true
    }

    /// Read and validate the commit record for `generation`.
    fn read_commit_record(&self, generation: u64) -> Option<(Vec<u8>, crate::format::Commit)> {
        let key = self.commit_key(generation);
        let bytes = self.with_backend(|backend| backend.read_all(&key)).ok()?;
        crate::format::decode_commit_file(&bytes).ok()
    }

    /// The attestation `commit` carries, only when the on-disk record re-hashes
    /// to that commit id.
    fn attested_index(&self, commit: Hash32, generation: u64) -> Option<IndexAttestation> {
        let (_, record) = self.read_commit_record(generation)?;
        if record.id().ok()? != commit {
            return None;
        }
        record.index_attestation
    }

    /// Build the index attestation for a commit that is about to publish, and
    /// return the index it describes so the segment write does not rebuild it.
    ///
    /// `commit` must not carry an attestation yet: `state_root` is computed over
    /// the six authoritative fields only, which is exactly what the loader can
    /// recompute from the published record.
    fn index_attestation_for(
        &self,
        state: &State,
        commit: &crate::format::Commit,
        ops: &[Op],
        pre: &crate::index_incremental::PreImages,
    ) -> Result<(Option<IndexAttestation>, Option<PreparedIndex>)> {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::IndexSegment);
        if !self.options.attest_indexes || self.mode != OpenMode::ReadWrite {
            return Ok((None, None));
        }
        let generation = state.generation;
        // A delta can only name the immediately previous segment.
        let can_delta = !self
            .force_full_index
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.options.incremental_index
            && self.head != Hash32::ZERO
            && generation > 1;
        let maintain = self.options.incremental_index_maintenance;

        let mut index: StateIndex;
        // A delta derived from the operation stream.
        let mut changed: Option<(crate::index_incremental::IndexDelta, IndexChain)> = None;
        // A base index that has to be diffed with the whole-state encoder (the
        // pre-incremental writer, kept as a control).
        let mut full_diff: Option<(Arc<StateIndex>, IndexChain)> = None;

        if maintain && self.head != Hash32::ZERO && generation > 1 {
            // The base is the previous head's index, validated against the
            // published commit record before any use.
            match self.take_head_index() {
                Some((base_index, base_chain)) => {
                    let usable = can_delta
                        && base_chain.depth() < u64::from(self.options.index_chain_max_deltas);
                    match crate::index_incremental::apply(base_index, state, ops, pre) {
                        Some((derived, delta)) if usable => {
                            self.bump_index_stat(|stats| stats.maintenance_updates += 1);
                            index = derived;
                            changed = Some((delta, base_chain));
                        }
                        Some((derived, _)) => {
                            // A full segment is forced (compaction or
                            // `incremental_index = false`), but the index is
                            // still derived rather than rebuilt.
                            self.bump_index_stat(|stats| stats.maintenance_updates += 1);
                            index = derived;
                        }
                        None => {
                            // The ops and the base disagreed: rebuild from the
                            // post state. Nothing partial is ever attested.
                            self.bump_index_stat(|stats| stats.maintenance_fallbacks += 1);
                            index = self.build_index_phase(state);
                            if can_delta {
                                let _load = crate::phase_profile::scope(
                                    crate::phase_profile::Phase::IndexLoad,
                                );
                                full_diff = self.load_attested_chain(self.head).map(|base| {
                                    let AttestedBase { index, chain } = base;
                                    (index, chain)
                                });
                            }
                        }
                    }
                }
                None => {
                    // No usable in-memory chain: the head carries no attested
                    // v3 segment, or resolving it failed. Build, and diff only
                    // when a delta could still name the on-disk chain.
                    index = self.build_index_phase(state);
                    if can_delta {
                        let _load =
                            crate::phase_profile::scope(crate::phase_profile::Phase::IndexLoad);
                        full_diff = self
                            .load_attested_chain(self.head)
                            .map(|base| (base.index, base.chain));
                    }
                }
            }
        } else if !maintain && can_delta {
            // Pre-change control path: whole-state build and whole-state diff.
            index = self.build_index_phase(state);
            let _load = crate::phase_profile::scope(crate::phase_profile::Phase::IndexLoad);
            full_diff = self
                .load_attested_chain(self.head)
                .map(|base| (base.index, base.chain));
        } else {
            index = self.build_index_phase(state);
        }

        if !index.complete() {
            // An incomplete index is never persisted; without an attestation
            // the commit is v1 and no segment can be trusted.
            return Ok((None, None));
        }
        let state_root = identity::h(INDEX_STATE_DOMAIN, &commit.encode_fields()?);
        let candidates_root = index.candidates_root()?;

        let (bytes, chain) = {
            let _attest = crate::phase_profile::scope(crate::phase_profile::Phase::IndexAttest);
            // Derive the delta first: a full-state encode is only needed when a
            // delta is not chosen (or cannot be built), so the common case no
            // longer pays for both.
            let mut chosen: Option<(Vec<u8>, Vec<Hash32>)> = None;
            let rows = index.object_count() + index.relation_count() + index.entry_count();
            if let Some((delta, base_chain)) = changed {
                let delta_bytes = crate::index_chain::encode_delta_changed(
                    &delta,
                    &index,
                    base_chain.chain[0],
                    self.head,
                )?;
                // A tiny delta on a state big enough that any full segment must
                // dwarf it is chosen without encoding the full segment;
                // otherwise the comparison is exact, as before. 256 rows of
                // names + ids exceed 1 KiB by a wide margin, so this is a bound,
                // not a heuristic guess.
                let small = delta_bytes.len() <= 1024 && rows >= 256;
                let wins =
                    small || delta_bytes.len() < crate::index_chain::encode_full(&index)?.len();
                if wins {
                    chosen = Some((delta_bytes, base_chain.chain.clone()));
                }
            } else if let Some((base, base_chain)) = full_diff {
                if base.generation().checked_add(1) == Some(generation)
                    && base_chain.depth() < u64::from(self.options.index_chain_max_deltas)
                {
                    let base_id = base_chain.chain[0];
                    let delta =
                        crate::index_chain::encode_delta(&base, &index, base_id, self.head)?;
                    let small = delta.len() <= 1024 && rows >= 256;
                    let wins =
                        small || delta.len() < crate::index_chain::encode_full(&index)?.len();
                    if wins {
                        chosen = Some((delta, base_chain.chain.clone()));
                    }
                }
            }
            match chosen {
                Some((delta, base_chain)) => {
                    let id = crate::index_chain::file_segment_id(&delta)?;
                    let mut chain = Vec::with_capacity(base_chain.len() + 1);
                    chain.push(id);
                    chain.extend(base_chain);
                    let chain = IndexChain {
                        kind: SegmentKind::Delta,
                        chain,
                    };
                    (delta, chain)
                }
                None => {
                    let full_bytes = crate::index_chain::encode_full(&index)?;
                    let full_id = crate::index_chain::file_segment_id(&full_bytes)?;
                    (
                        full_bytes,
                        IndexChain {
                            kind: SegmentKind::Full,
                            chain: vec![full_id],
                        },
                    )
                }
            }
        };
        // Byte policy: a configured cap may skip the write. The commit then
        // carries no attestation, exactly like `attest_indexes = false`; the
        // reader rebuilds.
        if let Some(budget) = self.options.index_bytes_budget {
            let total = self.index_bytes_total().unwrap_or(u64::MAX);
            if total.saturating_add(bytes.len() as u64) > budget {
                self.note_index_write_error(format!(
                    "index for generation {generation} skipped: {} bytes would exceed the \
                     {budget}-byte index budget (currently {total})",
                    bytes.len()
                ));
                return Ok((None, None));
            }
        }
        let segment_root = chain.root(generation)?;
        let attestation = IndexAttestation {
            object_count: index.object_count() as u64,
            relation_count: index.relation_count() as u64,
            entry_count: index.entry_count() as u64,
            state_root,
            candidates_root,
            segment_kind: Some(chain.kind),
            segment_base: chain.base(),
            segment_depth: chain.depth(),
            segment_root,
        };
        Ok((
            Some(attestation),
            Some(PreparedIndex {
                bytes,
                index,
                candidates_root,
                chain: Some(chain),
            }),
        ))
    }

    /// Whole-state index build, counted and attributed. This is the oracle, not
    /// the fast path: only a cold start, a forced full segment without a usable
    /// base, or a failed maintenance step reaches it.
    fn build_index_phase(&self, state: &State) -> StateIndex {
        let _build = crate::phase_profile::scope(crate::phase_profile::Phase::IndexBuild);
        self.bump_index_stat(|stats| stats.builds += 1);
        StateIndex::build(state)
    }

    /// The attested v3 record for `commit`, if this generation's commit file
    /// re-hashes to it.
    fn attested_record(&self, commit: Hash32) -> Option<IndexAttestation> {
        let (_, record) = self.read_commit_record(self.generation)?;
        if record.id().ok()? != commit {
            return None;
        }
        let attestation = record.index_attestation?;
        attestation.is_v3().then_some(attestation)
    }

    /// Take ownership of the previous head's index for incremental maintenance.
    ///
    /// The in-memory cache is trusted only after its counts, chain and the
    /// candidate root recorded when it was written agree with the *published*
    /// commit record. The candidates root is compared, not recomputed: the
    /// cached object was produced by this writer and its root was placed in the
    /// commit, and every read path re-derives and checks the root from the
    /// segment bytes it actually loaded.
    ///
    /// `None` means "no trustworthy base": the caller rebuilds from state.
    fn take_head_index(&self) -> Option<(StateIndex, IndexChain)> {
        // Drop the read cache's reference so the cached index can move out by
        // value instead of being copied.
        self.index_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(self.head);
        if let Some(cached) = self
            .attested_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let attested = self
                .attested_record(self.head)
                .map(|attestation| {
                    attestation_matches(
                        &attestation,
                        &cached.index,
                        cached.candidates_root,
                        &cached.chain,
                    )
                })
                .unwrap_or(false);
            if cached.commit == self.head && attested {
                return Some(match Arc::try_unwrap(cached.index) {
                    Ok(index) => (index, cached.chain),
                    // A reader still holds a clone: one copy is cheaper than a
                    // rebuild, and the copy is what gets maintained.
                    Err(shared) => ((*shared).clone(), cached.chain),
                });
            }
        }
        let base = self.load_attested_chain(self.head)?;
        // `load_attested_chain` repopulates the cache; drop it so this handle
        // owns the only reference to the index.
        self.attested_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        Some(match Arc::try_unwrap(base.index) {
            Ok(index) => (index, base.chain),
            Err(shared) => ((*shared).clone(), base.chain),
        })
    }

    /// Resolve the attested chain for an already-published commit. Used when
    /// the next commit wants to extend it with a delta.
    ///
    /// The writer validates the in-memory chain first: the previous commit's
    /// attestation must name the same counts, candidates root, kind, base,
    /// depth and chain root. Only a cache miss (or a mismatch, which is treated
    /// as untrusted) falls back to reading and re-resolving the segment chain
    /// from the store. That is what keeps a commit's index work flat as the
    /// chain deepens, without weakening the attestation: the same checks run on
    /// both paths.
    fn load_attested_chain(&self, commit: Hash32) -> Option<AttestedBase> {
        let attestation = self.attested_record(commit)?;
        let matches = |index: &StateIndex, chain: &IndexChain| -> Option<()> {
            let candidates_root = index.candidates_root().ok()?;
            if attestation_matches(&attestation, index, candidates_root, chain) {
                Some(())
            } else {
                None
            }
        };
        if let Some(cached) = self
            .attested_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            if cached.commit == commit && matches(&cached.index, &cached.chain).is_some() {
                return Some(AttestedBase {
                    index: Arc::clone(&cached.index),
                    chain: cached.chain.clone(),
                });
            }
        }
        let bytes = self
            .with_backend(|backend| backend.read_all(&Key::index(commit)))
            .ok()?;
        let resolved = crate::index_chain::resolve_chain(&bytes, |base_commit, _base_id| {
            self.with_backend(|backend| backend.read_all(&Key::index(base_commit)))
        })
        .ok()?;
        matches(&resolved.index, &resolved.chain)?;
        let candidates_root = resolved.index.candidates_root().ok()?;
        let index = Arc::new(resolved.index);
        let chain = resolved.chain;
        *self
            .attested_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(AttestedCache {
            commit,
            index: Arc::clone(&index),
            candidates_root,
            chain: chain.clone(),
        });
        Some(AttestedBase { index, chain })
    }

    /// The advisory index segments a retained root still names: the current
    /// head, every snapshot, every commit carrying a checkpoint manifest, and
    /// the chain ancestors of each, resolved with the same per-root bound and
    /// fail-closed rules GC uses.
    ///
    /// Diagnostic only (benchmarks and tests). It exists so a court can check
    /// that GC kept exactly what a retained root needs instead of inferring it
    /// from a directory diff, and so the cost of that root set is measurable on
    /// an aged store. It reads commit records; it never mutates anything.
    pub fn retained_index_segment_ids(&self) -> Result<Vec<Hash32>> {
        let mut retained: BTreeSet<Hash32> = BTreeSet::new();
        retained.insert(self.head);
        retained.extend(self.state.snapshots.values().copied());
        {
            let mut found = Vec::new();
            self.with_backend(|backend| {
                recovery::for_each_commit_via(backend, u64::MAX, |commit| {
                    if commit.commit.checkpoint.is_some() {
                        found.push(commit.hash);
                    }
                    Ok(())
                })
            })?;
            retained.extend(found);
        }
        self.expand_retained_index_chains(&mut retained)?;
        Ok(retained.into_iter().collect())
    }

    /// Total bytes currently held in `indexes/`. Only called when a byte budget
    /// is configured or by the diagnostic CLI.
    pub fn index_bytes_total(&self) -> Result<u64> {
        let keys =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Index)))?;
        let mut total = 0u64;
        for key in keys {
            if matches!(key, Key::Index { .. }) {
                if let Ok(len) = self.with_backend(|backend| backend.len(&key)) {
                    total = total.saturating_add(len);
                }
            }
        }
        Ok(total)
    }

    /// Add every ancestor segment named by a retained commit's WFX3 chain to
    /// the retained set. A failure is returned so GC can retain everything
    /// rather than guess.
    ///
    /// **Invariant (per-root chain bound).** For every retained root, the walk
    /// from that root reaches a `Full` segment (or a commit with no WFX3
    /// segment) after at most `HARD_MAX_CHAIN_DELTAS` delta hops. The bound is
    /// checked against that root's own cumulative depth; it is never the size
    /// of the union of every retained root's chain. Commits shared by several
    /// roots are read once, and the distance from a commit to its full
    /// ancestor is memoised, so a second root reaching that commit is still
    /// measured against its own depth. Any root that violates the bound (too
    /// deep, or a cycle) fails the whole pass closed: nothing is deleted.
    fn expand_retained_index_chains(&self, retained: &mut BTreeSet<Hash32>) -> Result<()> {
        let roots: Vec<Hash32> = retained.iter().copied().collect();
        // Delta hops still to walk from a commit before its full ancestor: 0
        // when the commit's segment is full, is not WFX3, or does not exist.
        let mut distance: BTreeMap<Hash32, u32> = BTreeMap::new();
        for root in roots {
            let mut path: Vec<Hash32> = Vec::new();
            let mut on_path: BTreeSet<Hash32> = BTreeSet::new();
            let mut current = root;
            loop {
                if let Some(&rest) = distance.get(&current) {
                    // Resolved by an earlier root. This root's own cumulative
                    // depth still has to fit the same per-chain bound.
                    if path.len() as u64 + u64::from(rest)
                        > u64::from(crate::index_chain::HARD_MAX_CHAIN_DELTAS)
                    {
                        return Err(Error::limit_exceeded(
                            "index segment chain exceeds the hard depth bound",
                        ));
                    }
                    break;
                }
                if !on_path.insert(current) {
                    return Err(Error::limit_exceeded(
                        "index segment chain contains a cycle",
                    ));
                }
                let bytes =
                    match self.with_backend(|backend| backend.read_all(&Key::index(current))) {
                        Ok(bytes) => bytes,
                        Err(err) if err.class() == crate::error::Class::NotFound => {
                            // No segment (index disabled, or a write that did
                            // not happen): there is nothing left to retain.
                            distance.insert(current, 0);
                            break;
                        }
                        Err(err) => return Err(err),
                    };
                if bytes.len() < 4 || &bytes[0..4] != crate::index_chain::MAGIC_V3 {
                    distance.insert(current, 0);
                    break;
                }
                let header = crate::index_chain::decode_segment(&bytes)?.header;
                if header.kind == SegmentKind::Full {
                    distance.insert(current, 0);
                    break;
                }
                if path.len() as u64 >= u64::from(crate::index_chain::HARD_MAX_CHAIN_DELTAS) {
                    return Err(Error::limit_exceeded(
                        "index segment chain exceeds the hard depth bound",
                    ));
                }
                path.push(current);
                retained.insert(header.base_commit);
                current = header.base_commit;
            }
            // Fill in this walk's distances backwards from the terminal commit.
            let mut rest = distance.get(&current).copied().unwrap_or(0);
            for commit in path.iter().rev() {
                rest = rest.saturating_add(1);
                distance.insert(*commit, rest);
            }
        }
        Ok(())
    }

    /// Publish an index that was built and encoded for the attestation.
    fn write_built_index(&self, commit: Hash32, prepared: PreparedIndex) -> Result<u64> {
        let bytes = prepared.bytes;
        if bytes.len() as u64 > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index encoding is {} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap",
                bytes.len()
            )));
        }
        self.with_backend_mut(|backend| backend.put_atomic(&Key::index(commit), &bytes))
            .map_err(|err| {
                Error::io(format!(
                    "cannot publish index {}: {err}",
                    Key::index(commit).display_path()
                ))
            })?;
        let index = Arc::new(prepared.index.with_commit(commit));
        self.index_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(commit, Arc::clone(&index));
        if let Some(chain) = prepared.chain {
            // Keep the chain the next commit will extend. It is validated
            // against the published commit record before any use, so a cache
            // entry can never launder a mismatch.
            *self
                .attested_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(AttestedCache {
                commit,
                index,
                candidates_root: prepared.candidates_root,
                chain,
            });
        }
        self.bump_index_stat(|stats| {
            stats.writes += 1;
            stats.write_bytes += bytes.len() as u64;
        });
        Ok(bytes.len() as u64)
    }

    /// Best-effort index write after a commit has published. The index is
    /// derived data: a failure is a diagnostic and never fails the commit.
    fn persist_built_index(&self, commit: Hash32, prepared: PreparedIndex) {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::IndexSegment);
        if self.mode != OpenMode::ReadWrite {
            return;
        }
        if let Err(err) = self.write_built_index(commit, prepared) {
            self.note_index_write_error(format!("index for {commit} was not written: {err}"));
        }
    }

    fn bump_index_stat(&self, update: impl FnOnce(&mut IndexStats)) {
        let mut stats = self
            .index_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut stats);
    }

    fn note_index_reject(&self, note: String) {
        let mut stats = self
            .index_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.rejects += 1;
        if stats.notes.len() < 64 {
            stats.notes.push(note);
        }
    }

    fn note_index_write_error(&self, note: String) {
        let mut stats = self
            .index_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.write_errors += 1;
        if stats.notes.len() < 64 {
            stats.notes.push(note);
        }
    }

    /// Advisory index diagnostics (loads, rebuilds, rejects, writes).
    #[doc(hidden)]
    pub fn index_stats(&self) -> IndexStats {
        self.index_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Durable artifacts this handle has published: blob counts and bytes by
    /// kind, commit records, superblock slots, and advisory index segments.
    ///
    /// Diagnostic only. It exists so a write-cost ledger can say where the
    /// bytes and the durability barriers went instead of inferring it from a
    /// directory walk, and so a batching change is judged on measured counts.
    pub fn publication_ledger(&self) -> PublicationLedger {
        let index = self.index_stats();
        let barriers = self
            .with_backend(|backend| Ok(backend.durability_barriers()))
            .unwrap_or(None);
        self.publication
            .snapshot(index.writes, index.write_bytes, barriers)
    }

    /// Reset advisory index diagnostics (tests and benchmarks only).
    #[doc(hidden)]
    pub fn reset_index_stats(&self) {
        let mut stats = self
            .index_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *stats = IndexStats::default();
    }

    /// Test hook: read one physical blob by key, counting its requests.
    ///
    /// The pack plane's read amplification is a property of the plane, not of a
    /// store verb, so the court and its tests need one unopinionated read.
    #[doc(hidden)]
    pub fn read_blob_for_bench(&self, key: &crate::backend::Key) -> Result<Vec<u8>> {
        self.with_backend(|backend| backend.read_all(key))
    }

    /// Rewrite the advisory segment for the current head (benchmark only).
    ///
    /// The bytes are the attested chain's head segment, re-published; this
    /// measures the index write without changing the attestation. A benchmark
    /// that wants to measure full-state encoding uses `StateIndex::encode_file`
    /// through `build_index_for_bench` + a direct write.
    #[doc(hidden)]
    pub fn write_index_for_bench(&self) -> Result<u64> {
        self.require_writable()?;
        let bytes = self.with_backend(|backend| backend.read_all(&Key::index(self.head)))?;
        self.with_backend_mut(|backend| backend.put_atomic(&Key::index(self.head), &bytes))?;
        self.bump_index_stat(|stats| {
            stats.writes += 1;
            stats.write_bytes += bytes.len() as u64;
        });
        Ok(bytes.len() as u64)
    }

    /// Test hook: forge the head index by removing one tag row and re-encoding
    /// a *valid* full segment (correct digest and counts). The commit
    /// attestation's candidate root is the only remaining refutation.
    #[doc(hidden)]
    pub fn forge_head_index_remove_tag_for_test(&self, tag: &str) -> Result<()> {
        self.require_writable()?;
        let bytes = self.with_backend(|backend| backend.read_all(&Key::index(self.head)))?;
        let forged = crate::index_chain::forge_remove_tag_for_test(&bytes, tag)?;
        let key = Key::index(self.head);
        // `put_atomic` is create-once; a test forgery deliberately replaces the
        // file, so delete the original first.
        self.with_backend_mut(|backend| backend.delete(&key))?;
        self.with_backend_mut(|backend| backend.put_atomic(&key, &forged))?;
        Ok(())
    }

    /// Diagnostic: the head segment's kind (`full`/`delta`) and chain depth.
    pub fn index_head_chain(&self) -> Result<(String, u64)> {
        let bytes = match self.with_backend(|backend| backend.read_all(&Key::index(self.head))) {
            Ok(bytes) => bytes,
            Err(err) if err.class() == crate::error::Class::NotFound => {
                return Ok(("none".to_owned(), 0))
            }
            Err(err) => return Err(err),
        };
        if bytes.len() < 4 || &bytes[0..4] != crate::index_chain::MAGIC_V3 {
            return Ok(("wfx2".to_owned(), 0));
        }
        let segment = crate::index_chain::decode_segment(&bytes)?;
        let head_kind = segment.header.kind;
        let mut depth = 0u64;
        let mut current = segment;
        while current.header.kind == SegmentKind::Delta {
            depth += 1;
            if depth > u64::from(crate::index_chain::HARD_MAX_CHAIN_DELTAS) {
                return Err(Error::limit_exceeded(
                    "index head chain exceeds the hard depth bound",
                ));
            }
            let base_bytes = self.with_backend(|backend| {
                backend.read_all(&Key::index(current.header.base_commit))
            })?;
            current = crate::index_chain::decode_segment(&base_bytes)?;
        }
        Ok((head_kind.as_str().to_owned(), depth))
    }

    /// Compact the advisory index: publish a no-op commit whose segment is a
    /// full state snapshot, so the previous chain can be dropped by GC.
    ///
    /// The commit has no state ops; replay is a no-op. A snapshot of the old
    /// head keeps the old chain, so history is never broken. Returns the new
    /// head commit.
    pub fn compact_index(&mut self) -> Result<Hash32> {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "index compaction cannot run inside a group commit",
            ));
        }
        self.force_full_index
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let result = self.commit_ops(Vec::new(), false);
        self.force_full_index
            .store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    /// Build an index from `state` without writing it (benchmark only).
    #[doc(hidden)]
    pub fn build_index_for_bench(&self, state: &State) -> usize {
        let index = StateIndex::build(state);
        self.bump_index_stat(|stats| stats.builds += 1);
        index.object_count()
    }

    /// Test hook: the candidate sections of the index the writer currently
    /// holds for its head (the incrementally maintained one).
    #[doc(hidden)]
    pub fn head_index_sections_for_test(&self) -> Result<Vec<u8>> {
        let cached = self
            .attested_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cached = cached.as_ref().ok_or_else(|| {
            Error::invalid_argument("the writer holds no attested in-memory index")
        })?;
        if cached.commit != self.head {
            return Err(Error::invalid_argument(
                "the in-memory index is not for the current head",
            ));
        }
        cached.index.encode_sections()
    }

    /// Test hook: the oracle — a full `StateIndex::build` over the live state,
    /// encoded with the same canonical section layout.
    #[doc(hidden)]
    pub fn rebuilt_index_sections_for_test(&self) -> Result<Vec<u8>> {
        self.build_index_phase(&self.state).encode_sections()
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
        let (state, _notes) = self.with_backend(|backend| {
            recovery::load_state_at_via(backend, commit, u64::MAX, false)
        })?;
        Ok(state)
    }

    /// Only commits reachable from the recovered head are addressable. This
    /// keeps an uncommitted crash-tail commit file from exposing state that the
    /// superblock never published.
    fn is_published_ancestor(&self, target: Hash32) -> Result<bool> {
        let index = self.with_backend(|backend| recovery::commit_index_via(backend, u64::MAX))?;
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
        let index = self.with_backend(|backend| recovery::commit_index_via(backend, u64::MAX))?;
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
        let recording = self.history.is_some();
        let key = format!("entry:{}:{}", parent.to_hex(), hex_bytes(name));
        if recording {
            self.emit_history(serde_json::json!({
                "event": "read-invoke",
                "kind": "lookup",
                "key": key,
                "read_generation": self.generation,
            }));
        }
        let result = self.lookup_inner(parent, name);
        if recording {
            match &result {
                Ok(target) => self.emit_history(serde_json::json!({
                    "event": "read-response",
                    "kind": "lookup",
                    "key": key,
                    "value": format!("target:{}", target.to_hex()),
                    "read_generation": self.generation,
                })),
                Err(err) if err.class() == Class::NotFound => self.emit_history(
                    serde_json::json!({
                        "event": "read-response",
                        "kind": "lookup",
                        "key": key,
                        "value": serde_json::Value::Null,
                        "read_generation": self.generation,
                    }),
                ),
                Err(err) => self.emit_history(serde_json::json!({
                    "event": "read-response",
                    "kind": "lookup",
                    "key": key,
                    "error": err.to_string(),
                    "class": err.class().as_str(),
                    "read_generation": self.generation,
                })),
            }
        }
        result
    }

    fn lookup_inner(&self, parent: Hash32, name: &[u8]) -> Result<Hash32> {
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
    ///
    /// This is the **internal** read: it never consults the R8 read layers
    /// (cache / single-flight), so verification, planning, recovery, and GC see
    /// exactly the shipped path. Callers that have authorized a read should use
    /// [`Store::read_object_authorized`].
    pub fn read_object(&self, id: Hash32) -> Result<Vec<u8>> {
        let recording = self.history.is_some();
        let key = format!("object:{}", id.to_hex());
        let value = self.state.objects.get(&id).map(|record| {
            format!(
                "content:{}:{}:{}",
                record.content.to_hex(),
                record.size,
                record.version.to_hex()
            )
        });
        if recording {
            self.emit_history(serde_json::json!({
                "event": "read-invoke",
                "kind": "object",
                "key": key,
                "read_generation": self.generation,
            }));
        }
        let result = self.read_object_unrecorded(id);
        if recording {
            match &result {
                Ok(bytes) => self.emit_history(serde_json::json!({
                    "event": "read-response",
                    "kind": "object",
                    "key": key,
                    "value": value,
                    "byte_len": bytes.len(),
                    "read_generation": self.generation,
                })),
                Err(err) => self.emit_history(serde_json::json!({
                    "event": "read-response",
                    "kind": "object",
                    "key": key,
                    "error": err.to_string(),
                    "class": err.class().as_str(),
                    "read_generation": self.generation,
                })),
            }
        }
        result
    }

    fn read_object_unrecorded(&self, id: Hash32) -> Result<Vec<u8>> {
        let bytes = self.read_object_inner(id, None, None)?;
        self.note_object_read(id);
        Ok(bytes)
    }

    /// Authorize a read and return the evidence token.
    ///
    /// Evaluates POSIX mode, the `weft.acl.read` ACL, and (when the principal
    /// carries one) the capability envelope's validity window, revocation, and
    /// scope-independent authorization rules. Denials are `Unauthorized` and
    /// never mention whether any cached bytes exist.
    pub fn authorize_read(
        &self,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<AuthorizedRead> {
        let state = &self.state;
        self.authorize_read_in(state, id, principal, now_ns)
    }

    /// [`Store::authorize_read`] against an explicit (snapshot) state.
    pub fn authorize_read_at(
        &self,
        snapshot: Hash32,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<AuthorizedRead> {
        let state = self.state_at(snapshot)?;
        self.authorize_read_in(&state, id, principal, now_ns)
    }

    /// [`Store::authorize_read`] against a state the caller already holds.
    pub fn authorize_read_in(
        &self,
        state: &State,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<AuthorizedRead> {
        let capability = crate::context::principal_capability(principal)?;
        self.authorize_read_in_with_capability(state, id, principal, now_ns, capability.as_ref())
    }

    /// [`Store::authorize_read`] with a capability the caller already decoded.
    ///
    /// The ordering is identical to [`Store::authorize_read_in`]: the envelope's
    /// validity window is checked before the object lookup, then the POSIX mode
    /// and the `weft.acl.read` ACL. Passing an already-decoded context only
    /// avoids decoding the same immutable envelope twice; every check still runs.
    pub fn authorize_read_with_capability(
        &self,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
        capability: Option<&CapabilityContext>,
    ) -> Result<AuthorizedRead> {
        self.authorize_read_in_with_capability(&self.state, id, principal, now_ns, capability)
    }

    /// The shared body of the two authorization entry points.
    pub fn authorize_read_in_with_capability(
        &self,
        state: &State,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
        capability: Option<&CapabilityContext>,
    ) -> Result<AuthorizedRead> {
        if let Some(capability) = capability {
            capability.check_valid_at(now_ns)?;
        }
        let record = state
            .objects
            .get(&id)
            .ok_or_else(|| Error::not_found("no such object"))?;
        if !crate::auth::can_read(record, principal) {
            return Err(Error::new(
                Class::Unauthorized,
                "object is not readable by this principal",
            ));
        }
        Ok(AuthorizedRead {
            id: record.id,
            content: record.content,
            size: record.size,
            kind: record.kind,
        })
    }

    /// Read a complete object after authorization, through the R8 read layers.
    pub fn read_object_authorized(&self, authorized: &AuthorizedRead) -> Result<Vec<u8>> {
        self.read_object_authorized_cancellable(authorized, None)
    }

    /// [`Store::read_object_authorized`] with cooperative cancellation.
    ///
    /// Cancellation only applies while the caller waits for an identical
    /// in-flight read; a leader always finishes so other waiters still get
    /// their result.
    pub fn read_object_authorized_cancellable(
        &self,
        authorized: &AuthorizedRead,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        let bytes = self.read_object_inner(authorized.id, Some(authorized), cancel)?;
        self.note_object_read(authorized.id);
        Ok(bytes)
    }

    /// Read without touching the advisory access counters. Internal store
    /// work (planning, rebase dry runs, verification) must not inflate them.
    pub(crate) fn read_object_uncounted(&self, id: Hash32) -> Result<Vec<u8>> {
        self.read_object_inner(id, None, None)
    }

    fn read_object_inner(
        &self,
        id: Hash32,
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        let record = self.object_meta(id)?;
        let bytes = match record.kind {
            ObjectKind::Dir => return Err(Error::is_dir("object is a directory")),
            ObjectKind::Symlink => record
                .symlink_target()
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| Error::corrupt("symlink object has no target attribute"))?,
            ObjectKind::File => {
                let manifest = self.read_manifest_keyed(record.content)?;
                if manifest.size != record.size {
                    return Err(Error::corrupt("object size disagrees with manifest"));
                }
                self.read_manifest_bytes_gated(
                    &manifest,
                    record.content,
                    0,
                    manifest.size,
                    true,
                    gate,
                    cancel,
                )?
            }
        };
        Ok(bytes)
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
                let bytes = match record.kind {
                    ObjectKind::Dir => return Err(Error::is_dir("object is a directory")),
                    ObjectKind::Symlink => record
                        .symlink_target()
                        .map(|bytes| bytes.to_vec())
                        .ok_or_else(|| Error::corrupt("symlink object has no target attribute"))?,
                    ObjectKind::File => {
                        let manifest = self.read_manifest_keyed(record.content)?;
                        if manifest.size != record.size {
                            return Err(Error::corrupt("object size disagrees with manifest"));
                        }
                        self.read_manifest_bytes(&manifest, record.content, 0, manifest.size, true)?
                    }
                };
                self.note_object_read(id);
                Ok(bytes)
            }
        }
    }

    /// Read a byte range after authorization, through the R8 read layers.
    pub fn read_range_authorized(
        &self,
        authorized: &AuthorizedRead,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        self.read_range_authorized_cancellable(authorized, offset, length, None)
    }

    /// [`Store::read_range_authorized`] with cooperative cancellation.
    pub fn read_range_authorized_cancellable(
        &self,
        authorized: &AuthorizedRead,
        offset: u64,
        length: u64,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        if authorized.kind != ObjectKind::File {
            return Err(Error::invalid_argument("read_range requires a file object"));
        }
        let record = self.object_meta(authorized.id)?;
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument("read_range requires a file object"));
        }
        let manifest = self.read_manifest_keyed(record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        let bytes = self.read_manifest_bytes_gated(
            &manifest,
            record.content,
            offset,
            length,
            false,
            Some(authorized),
            cancel,
        )?;
        self.note_object_read(authorized.id);
        Ok(bytes)
    }

    /// Read a byte range from a file object (internal path).
    pub fn read_range(&self, id: Hash32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let record = self.object_meta(id)?;
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument("read_range requires a file object"));
        }
        let manifest = self.read_manifest_keyed(record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        let bytes = self.read_manifest_bytes(&manifest, record.content, offset, length, false)?;
        self.note_object_read(id);
        Ok(bytes)
    }

    /// Read a byte range directly from a content root (manifest id). Safe for
    /// snapshot materialization because it does not consult live object state.
    pub fn read_content(&self, content_root: Hash32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let manifest = self.read_manifest_keyed(content_root)?;
        let whole = offset == 0 && length >= manifest.size;
        self.read_manifest_bytes(&manifest, content_root, offset, length, whole)
    }

    /// Read from a content root after authorization, through the R8 read
    /// layers.
    ///
    /// The authorization token carries the object that produced this content
    /// root; the read itself uses the content root so it can never mix two
    /// versions. A token from [`Store::authorize_read`] for a different content
    /// identity than the current object version still authorizes the object,
    /// but the cache key is the *current* content root, so an old token cannot
    /// make stale bytes hit.
    pub fn read_content_authorized(
        &self,
        authorized: &AuthorizedRead,
        content_root: Hash32,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        if authorized.content != content_root {
            return Err(Error::new(
                Class::Unauthorized,
                "authorization token does not cover this content root",
            ));
        }
        let manifest = self.read_manifest_keyed(content_root)?;
        let whole = offset == 0 && length >= manifest.size;
        self.read_manifest_bytes_gated(
            &manifest,
            content_root,
            offset,
            length,
            whole,
            Some(authorized),
            None,
        )
    }

    /// Cumulative R8 read-layer counters.
    pub fn read_layer_stats(&self) -> ReadLayerStats {
        ReadLayerStats {
            decode_calls: self.decode_calls.load(Ordering::Relaxed),
            decode_bytes: self.decode_bytes.load(Ordering::Relaxed),
            single_flight: self.single_flight.stats(),
            cache: self
                .object_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .stats(),
        }
    }

    /// Verified decoded-object cache counters only.
    pub fn cache_stats(&self) -> ObjectCacheStats {
        self.read_layer_stats().cache
    }

    /// Single-flight counters only.
    pub fn single_flight_stats(&self) -> SingleFlightStats {
        self.single_flight.stats()
    }

    /// Number of reconstructions the store actually executed.
    pub fn decode_calls(&self) -> u64 {
        self.decode_calls.load(Ordering::Relaxed)
    }

    /// True when at least one R8 read layer is enabled.
    pub fn read_layers_enabled(&self) -> bool {
        self.cache_enabled.load(Ordering::Relaxed) || self.single_flight.enabled()
    }

    /// Pin an object so cached versions of it are never evicted.
    pub fn pin_cache_object(&self, id: Hash32) -> Result<()> {
        self.object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pin(id)
    }

    /// Remove a cache pin. The entry stays until normal eviction selects it.
    pub fn unpin_cache_object(&self, id: Hash32) {
        self.object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .unpin(id);
    }

    /// True when `id` is pinned in the decoded-object cache.
    pub fn is_cache_object_pinned(&self, id: Hash32) -> bool {
        self.object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_pinned(id)
    }

    /// Test-only: make a single-flight leader dwell before it starts decoding.
    ///
    /// This makes coalescing observable without depending on scheduler timing;
    /// production code never calls it and the default is zero. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn set_single_flight_test_dwell(&self, nanos: u64) {
        self.single_flight_test_dwell_nanos
            .store(nanos, Ordering::Relaxed);
    }

    /// Drop every cached decoded object (pins are retained).
    pub fn clear_object_cache(&self) {
        self.object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Verify only the chunks a range touches for a `weft.manifest.v3` file.
    ///
    /// `content_root` is the store's content root, i.e. the manifest id stored
    /// in `ObjectRecord::content` (the same value every other `ReadContent`
    /// entry point takes). The v3 manifest's own `content_root` field is the
    /// Merkle root and is returned as [`RegionVerification::content_root`].
    ///
    /// The touched blob hashes are re-derived from the blob bytes
    /// (`H("weft/chunk/v1", bytes)` equals the declared chunk hash), their
    /// lengths are checked, and the Merkle root is recomputed from *all* chunk
    /// hashes in the manifest and compared with the declared root. No explicit
    /// sibling table is stored: the manifest already carries every leaf
    /// preimage, so the recomputation is O(#chunks) small hashes and never
    /// reads untouched blobs.
    ///
    /// A whole-file (`v1`/`v2`) content root cannot be range-verified without
    /// reading every byte, so it is rejected with `InvalidArgument`; use
    /// `verify --full` for those. A tampered blob or hash names the chunk index.
    pub fn verify_region(
        &self,
        content_root: Hash32,
        offset: u64,
        length: u64,
    ) -> Result<RegionVerification> {
        let manifest = self.read_manifest_keyed(content_root)?;
        if manifest.identity != ContentIdentity::MerkleV3 {
            return Err(Error::invalid_argument(
                "verify_region requires a weft.manifest.v3 Merkle content root",
            ));
        }
        let declared = identity::merkle_root_from_chunks(
            manifest.chunks.iter().map(|chunk| (chunk.len, chunk.hash)),
        );
        if declared != manifest.content_hash {
            return Err(Error::corrupt(
                "merkle content_root does not match the manifest chunk hashes",
            ));
        }
        if offset >= manifest.size || length == 0 {
            return Ok(RegionVerification {
                content_root: declared,
                chunks_verified: 0,
                bytes_verified: 0,
            });
        }
        let end = offset.saturating_add(length).min(manifest.size);
        let mut cursor: u64 = 0;
        let mut chunks_verified = 0usize;
        let mut bytes_verified = 0u64;
        for (index, chunk) in manifest.chunks.iter().enumerate() {
            let chunk_start = cursor;
            let chunk_end = cursor
                .checked_add(chunk.len)
                .ok_or_else(|| Error::corrupt("chunk offset overflow"))?;
            cursor = chunk_end;
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            let bytes = self
                .read_blob_verified_keyed(BlobKind::Object, chunk.hash, identity::CHUNK_DOMAIN)
                .map_err(|err| chunk_read_failure(index, chunk.hash, err))?;
            if bytes.len() as u64 != chunk.len {
                return Err(Error::corrupt(format!(
                    "chunk {index} length disagrees with manifest"
                )));
            }
            chunks_verified += 1;
            bytes_verified = bytes_verified.saturating_add(chunk.len);
        }
        Ok(RegionVerification {
            content_root: declared,
            chunks_verified,
            bytes_verified,
        })
    }

    /// Read and validate a chunk manifest by its content id. This is the
    /// manifest the store itself would decode for an object whose `content` is
    /// `id`; callers use it to seed a [`ManifestCache`].
    pub fn manifest_of(&self, id: Hash32) -> Result<Manifest> {
        self.read_manifest_keyed(id)
    }

    /// Decode and verify a manifest's whole logical content through this
    /// handle's backend. Used by the checkpoint committer to re-derive what a
    /// prepare worker claims it published; the worker's word is never trusted.
    pub(crate) fn read_manifest_verified(
        &self,
        manifest_id: Hash32,
        content_root: Hash32,
        size: u64,
    ) -> Result<Vec<u8>> {
        let manifest = self.manifest_of(manifest_id)?;
        if manifest.size != size {
            return Err(Error::corrupt(format!(
                "manifest {manifest_id} declares {} bytes, not {size}",
                manifest.size
            )));
        }
        if manifest.content_hash != content_root {
            return Err(Error::corrupt(format!(
                "manifest {manifest_id} content root disagrees with the expected root"
            )));
        }
        self.read_manifest_bytes(&manifest, content_root, 0, size, true)
    }

    fn read_manifest_bytes(
        &self,
        manifest: &Manifest,
        content_root: Hash32,
        offset: u64,
        length: u64,
        verify_whole: bool,
    ) -> Result<Vec<u8>> {
        self.read_manifest_bytes_gated(
            manifest,
            content_root,
            offset,
            length,
            verify_whole,
            None,
            None,
        )
    }

    /// The R8 read path: cache and single-flight are consulted only when an
    /// authorized token is present, and the same token (and cancellation) is
    /// propagated to dependency reconstructions.
    #[allow(clippy::too_many_arguments)]
    fn read_manifest_bytes_gated(
        &self,
        manifest: &Manifest,
        content_root: Hash32,
        offset: u64,
        length: u64,
        verify_whole: bool,
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        let mut seen = vec![content_root];
        self.read_manifest_bytes_inner_gated(
            manifest,
            offset,
            length,
            verify_whole,
            0,
            &mut seen,
            gate,
            cancel,
        )
    }

    pub(crate) fn read_manifest_bytes_inner(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
    ) -> Result<Vec<u8>> {
        self.read_manifest_bytes_inner_gated(
            manifest,
            offset,
            length,
            verify_whole,
            depth,
            seen,
            None,
            None,
        )
    }

    /// One reconstruction step with the optional R8 layers.
    ///
    /// A whole, verified read of an authorized object is served from the
    /// decoded-object cache when possible, and otherwise coalesced with any
    /// identical in-flight reconstruction. Every other read keeps the shipped
    /// path but still coalesces when single-flight is enabled (identical region
    /// and identical verification flag).
    pub(crate) fn read_manifest_bytes_inner_gated(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        if gate.is_none() {
            return self.decode_manifest(
                manifest,
                offset,
                length,
                verify_whole,
                depth,
                seen,
                gate,
                cancel,
            );
        }
        let content = seen.last().copied().unwrap_or(manifest.content_hash);
        let whole = verify_whole && offset == 0 && length >= manifest.size;
        if whole {
            if let Some(bytes) = self.cache_lookup(content) {
                return Ok(bytes);
            }
        }
        let object = match gate {
            Some(authorized) if authorized.content == content => authorized.id,
            _ => self.object_id_for_content(content).unwrap_or(content),
        };
        let region = if offset == 0 && length >= manifest.size {
            RegionKey::Whole
        } else {
            RegionKey::Bytes { offset, length }
        };
        let key = ReconstructionKey {
            object,
            content,
            region,
            verified: verify_whole,
        };
        let hint = region.bytes_hint(manifest.size);
        let shared = self.single_flight.run(key, hint, cancel, || {
            self.decode_calls.fetch_add(1, Ordering::Relaxed);
            self.decode_bytes.fetch_add(hint, Ordering::Relaxed);
            let dwell = self.single_flight_test_dwell_nanos.load(Ordering::Relaxed);
            if dwell > 0 {
                std::thread::sleep(std::time::Duration::from_nanos(dwell));
            }
            let bytes = self.decode_manifest(
                manifest,
                offset,
                length,
                verify_whole,
                depth,
                seen,
                gate,
                cancel,
            )?;
            Ok(Arc::new(bytes))
        })?;
        if whole && gate.is_some() {
            self.cache_admit(content, manifest, Arc::clone(&shared));
        }
        Ok(shared.as_ref().clone())
    }

    /// The shipped dispatch: raw, tensor, or codec reconstruction.
    #[allow(clippy::too_many_arguments)]
    fn decode_manifest(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        match &manifest.representation {
            None => self.read_raw_manifest_bytes(manifest, offset, length, verify_whole),
            Some(rep) if rep.kind == RepresentationKind::Raw => {
                self.read_raw_manifest_bytes(manifest, offset, length, verify_whole)
            }
            Some(rep) if rep.kind == RepresentationKind::Overlay => {
                let manifest_id = seen
                    .last()
                    .copied()
                    .ok_or_else(|| Error::corrupt("overlay read has no manifest identity"))?;
                self.read_overlay_manifest_bytes(
                    manifest_id,
                    manifest,
                    rep,
                    offset,
                    length,
                    verify_whole,
                )
            }
            Some(rep) if rep.kind == RepresentationKind::Tensor => self.read_tensor_manifest_bytes(
                manifest,
                rep,
                offset,
                length,
                verify_whole,
                depth,
                seen,
                gate,
                cancel,
            ),
            Some(rep) => self.read_codec_manifest_bytes(
                manifest,
                rep,
                offset,
                length,
                verify_whole,
                depth,
                seen,
                gate,
                cancel,
            ),
        }
    }

    /// Look up a verified decoded object. A disabled cache is never consulted.
    fn cache_lookup(&self, content: Hash32) -> Option<Vec<u8>> {
        if !self.cache_enabled.load(Ordering::Relaxed) {
            return None;
        }
        let object = self.object_id_for_content(content).unwrap_or(content);
        let key = CacheKey::new(object, content);
        let mut cache = self
            .object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.lookup(key).map(|bytes| bytes.as_ref().clone())
    }

    /// Admit verified decoded bytes with the documented score.
    fn cache_admit(&self, content: Hash32, manifest: &Manifest, bytes: Arc<Vec<u8>>) {
        if !self.cache_enabled.load(Ordering::Relaxed) {
            return;
        }
        let object = self.object_id_for_content(content).unwrap_or(content);
        let (class, dependents) = self.cache_score_inputs(object, manifest);
        let reads = self.access_reads(object);
        let mut cache = self
            .object_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let score = cache.score_for(class, manifest.size, dependents, reads);
        cache.admit(CacheKey::new(object, content), bytes, score);
    }

    /// The measured cost class and relation-graph dependent count for one
    /// object version. The reverse index is rebuilt when the head changes.
    fn cache_score_inputs(&self, object: Hash32, manifest: &Manifest) -> (CostClass, u64) {
        let dependents =
            self.with_content_index(|index| index.dependents.get(&object).copied().unwrap_or(0));
        (manifest_cost_class(manifest), dependents)
    }

    /// The lowest ObjectId whose content root is `content`, if any.
    fn object_id_for_content(&self, content: Hash32) -> Option<Hash32> {
        self.with_content_index(|index| index.object_by_content.get(&content).copied())
    }

    /// Run `f` against the head-keyed reverse index, rebuilding it if stale.
    fn with_content_index<T>(&self, f: impl FnOnce(&ContentIndex) -> T) -> T {
        let mut guard = self
            .content_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stale = match guard.as_ref() {
            Some((head, _)) => *head != self.head,
            None => true,
        };
        if stale {
            let mut index = ContentIndex::default();
            for record in self.state.objects.values() {
                index
                    .object_by_content
                    .entry(record.content)
                    .or_insert(record.id);
            }
            for relation in self.state.relations.values() {
                if relation.predicate == crate::context::CONTEXT_LINEAGE_PREDICATE {
                    if let RelationTarget::Object(target) = &relation.target {
                        *index.dependents.entry(*target).or_default() += 1;
                    }
                }
            }
            *guard = Some((self.head, index));
        }
        let (_, index) = guard.as_ref().expect("content index was just built");
        f(index)
    }

    /// The frozen v1 (and v2 `kind:"raw"`) assembly path.
    fn read_raw_manifest_bytes(
        &self,
        manifest: &Manifest,
        offset: u64,
        length: u64,
        verify_whole: bool,
    ) -> Result<Vec<u8>> {
        self.read_chunk_table_range(
            &manifest.chunks,
            manifest.size,
            manifest.content_hash,
            manifest.identity,
            offset,
            length,
            verify_whole,
            false,
        )
    }

    /// Assemble a range from an explicit canonical chunk table. Used by the raw
    /// path and by the resolved overlay path; `count_overlay_reads` makes the
    /// test-only overlay chunk counter observe exactly the chunks this read
    /// fetched.
    #[allow(clippy::too_many_arguments)]
    fn read_chunk_table_range(
        &self,
        chunks: &[ChunkRef],
        size: u64,
        content_hash: Hash32,
        identity: ContentIdentity,
        offset: u64,
        length: u64,
        verify_whole: bool,
        count_overlay_reads: bool,
    ) -> Result<Vec<u8>> {
        if offset >= size {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(length).min(size);
        let capacity = checked_usize(end - offset, "range length")?;
        let mut out = Vec::with_capacity(capacity);
        let mut cursor: u64 = 0;
        for (index, chunk) in chunks.iter().enumerate() {
            let chunk_start = cursor;
            let chunk_end = cursor
                .checked_add(chunk.len)
                .ok_or_else(|| Error::corrupt("chunk offset overflow"))?;
            cursor = chunk_end;
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            let bytes = self
                .read_blob_verified_keyed(BlobKind::Object, chunk.hash, identity::CHUNK_DOMAIN)
                .map_err(|err| chunk_read_failure(index, chunk.hash, err))?;
            if count_overlay_reads {
                representation::note_overlay_chunk_read();
            }
            self.note_probe_bytes(bytes.len() as u64);
            if bytes.len() as u64 != chunk.len {
                return Err(Error::corrupt(format!(
                    "chunk {index} length disagrees with manifest"
                )));
            }
            let local_start =
                checked_usize(offset.saturating_sub(chunk_start), "chunk-local read start")?;
            let local_end =
                checked_usize((end - chunk_start).min(chunk.len), "chunk-local read end")?;
            let _assemble = phase_profile::scope(Phase::Assemble);
            out.extend_from_slice(&bytes[local_start..local_end]);
        }
        if out.len() as u64 != end - offset {
            return Err(Error::corrupt("assembled range length mismatch"));
        }
        if verify_whole && offset == 0 && end == size {
            let _verify = phase_profile::scope(Phase::ContentVerify);
            if identity == ContentIdentity::MerkleV3 {
                let root = identity::merkle_root_from_chunks(
                    chunks.iter().map(|chunk| (chunk.len, chunk.hash)),
                );
                if root != content_hash {
                    return Err(Error::corrupt(
                        "merkle content root mismatch for a fully read file",
                    ));
                }
            } else {
                let digest = identity::content_hash(&out);
                if digest != content_hash {
                    return Err(Error::corrupt("whole-content hash mismatch"));
                }
            }
        }
        Ok(out)
    }

    /// Decode a `weft.manifest.v4` `kind:"overlay"` representation.
    ///
    /// The canonical table is resolved from the immutable base chain (see
    /// [`Store::overlay_chunk_table`]) without fetching any content chunk, and
    /// then only the chunks intersecting the requested range are read. The
    /// resolved table is fully validated against the manifest's Merkle
    /// `content_root` before any content chunk is touched.
    fn read_overlay_manifest_bytes(
        &self,
        manifest_id: Hash32,
        manifest: &Manifest,
        rep: &Representation,
        offset: u64,
        length: u64,
        verify_whole: bool,
    ) -> Result<Vec<u8>> {
        if rep.decoded_size != manifest.size {
            return Err(Error::corrupt(
                "overlay representation decoded_size disagrees with manifest size",
            ));
        }
        let table = self.overlay_chunk_table(manifest_id, manifest)?;
        self.read_chunk_table_range(
            &table,
            manifest.size,
            manifest.content_hash,
            ContentIdentity::MerkleV3,
            offset,
            length,
            verify_whole,
            true,
        )
    }

    /// Resolve and validate the canonical chunk table of an overlay manifest.
    ///
    /// A cache hit is returned only for a table that was fully validated (base
    /// cross-checks, extent grid, total length, and Merkle root) before it was
    /// inserted. A miss walks the immutable base chain, reading manifest blobs
    /// only; no content chunk is read here.
    fn overlay_chunk_table(
        &self,
        manifest_id: Hash32,
        manifest: &Manifest,
    ) -> Result<Arc<Vec<ChunkRef>>> {
        if let Some(table) = self
            .overlay_tables
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&manifest_id)
        {
            return Ok(table);
        }
        let mut seen = Vec::new();
        let table = self.build_overlay_table(manifest_id, manifest, 0, &mut seen)?;
        let table = Arc::new(table);
        self.overlay_tables
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(manifest_id, Arc::clone(&table));
        Ok(table)
    }

    /// Recursively merge one overlay manifest over its base chain.
    fn build_overlay_table(
        &self,
        manifest_id: Hash32,
        manifest: &Manifest,
        depth: u64,
        seen: &mut Vec<Hash32>,
    ) -> Result<Vec<ChunkRef>> {
        if depth > representation::MAX_OVERLAY_CHAIN_DEPTH {
            return Err(Error::limit_exceeded(format!(
                "overlay chain exceeds the hard bound {}",
                representation::MAX_OVERLAY_CHAIN_DEPTH
            )));
        }
        if seen.contains(&manifest_id) {
            return Err(Error::corrupt("overlay base chain participates in a cycle"));
        }
        let rep = manifest.representation.as_ref().ok_or_else(|| {
            Error::corrupt("overlay chunk table requested for a manifest without a representation")
        })?;
        let overlay = rep
            .overlay
            .as_ref()
            .ok_or_else(|| Error::corrupt("overlay representation is missing its overlay map"))?;
        if rep.kind != RepresentationKind::Overlay {
            return Err(Error::corrupt(
                "overlay chunk table requested for a non-overlay representation",
            ));
        }
        if overlay.depth > representation::MAX_OVERLAY_CHAIN_DEPTH {
            return Err(Error::limit_exceeded(format!(
                "overlay depth {} exceeds the hard bound {}",
                overlay.depth,
                representation::MAX_OVERLAY_CHAIN_DEPTH
            )));
        }
        overlay.validate_shape(manifest.chunk_size, manifest.size)?;
        if manifest.size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "overlay file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        seen.push(manifest_id);
        // Resolve the base table first. A base must itself be a Merkle chunk
        // table (v3 raw or a v4 overlay); frame-backed and whole-file bases are
        // not composable and fail closed.
        let base_manifest = self.read_manifest_keyed(overlay.base)?;
        if base_manifest.content_hash != overlay.base_content_root {
            return Err(Error::corrupt(format!(
                "overlay base {} content root {} disagrees with the declared {}",
                overlay.base, base_manifest.content_hash, overlay.base_content_root
            )));
        }
        if base_manifest.size != overlay.base_size {
            return Err(Error::corrupt(format!(
                "overlay base {} size {} disagrees with the declared {}",
                overlay.base, base_manifest.size, overlay.base_size
            )));
        }
        if base_manifest.chunk_size != manifest.chunk_size {
            return Err(Error::corrupt(
                "overlay base chunk_size differs from the new manifest",
            ));
        }
        if manifest.size < base_manifest.size {
            return Err(Error::corrupt(
                "an overlay extent cannot make the file smaller than its base",
            ));
        }
        if base_manifest.identity != ContentIdentity::MerkleV3 {
            return Err(Error::corrupt(
                "overlay base must use the Merkle content identity",
            ));
        }
        let base_is_overlay = base_manifest
            .representation
            .as_ref()
            .map(|rep| rep.kind == RepresentationKind::Overlay)
            .unwrap_or(false);
        let base_overlay_bytes: u64;
        let base_table: Vec<ChunkRef> = if base_is_overlay {
            let base_rep = base_manifest
                .representation
                .as_ref()
                .and_then(|rep| rep.overlay.as_ref())
                .ok_or_else(|| Error::corrupt("overlay base is missing its overlay map"))?;
            if base_rep.depth + 1 != overlay.depth {
                return Err(Error::corrupt(format!(
                    "overlay depth {} does not follow the base depth {}",
                    overlay.depth, base_rep.depth
                )));
            }
            base_overlay_bytes = base_rep.overlay_bytes;
            let table = self.build_overlay_table(overlay.base, &base_manifest, depth + 1, seen)?;
            let table = Arc::new(table);
            self.overlay_tables
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(overlay.base, Arc::clone(&table));
            table.as_ref().clone()
        } else {
            match base_manifest.representation.as_ref() {
                None => {}
                Some(rep) if rep.kind == RepresentationKind::Raw => {}
                Some(_) => {
                    return Err(Error::corrupt(
                        "overlay base must be a chunk table, not a frame-backed representation",
                    ))
                }
            }
            if overlay.depth != 1 {
                return Err(Error::corrupt(format!(
                    "overlay depth {} does not follow a non-overlay base",
                    overlay.depth
                )));
            }
            base_overlay_bytes = 0;
            base_manifest.chunks.clone()
        };
        seen.pop();

        let base_total: u64 = base_table.iter().map(|chunk| chunk.len).sum();
        if base_total != base_manifest.size {
            return Err(Error::corrupt(
                "overlay base chunk table length disagrees with its size",
            ));
        }
        let mut table = base_table;
        let mut extent_total: u64 = 0;
        let mut previous_end: Option<u64> = None;
        for extent in &overlay.extents {
            if let Some(previous) = previous_end {
                if extent.offset < previous {
                    return Err(Error::corrupt(
                        "overlay extents are not canonically ordered or overlap",
                    ));
                }
            }
            previous_end = Some(extent.offset + extent.length);
            extent_total = extent_total
                .checked_add(extent.length)
                .ok_or_else(|| Error::corrupt("overlay byte total overflows"))?;
            let start = usize::try_from(extent.offset / manifest.chunk_size)
                .map_err(|_| Error::limit_exceeded("overlay extent index does not fit usize"))?;
            if start > table.len() {
                return Err(Error::corrupt(format!(
                    "overlay extent at {} leaves a gap after the base table",
                    extent.offset
                )));
            }
            let end = start
                .checked_add(extent.chunks.len())
                .ok_or_else(|| Error::corrupt("overlay extent chunk count overflows"))?;
            let old_len = table.len();
            if end > old_len {
                table.splice(start..old_len, extent.chunks.iter().cloned());
            } else {
                table.splice(start..end, extent.chunks.iter().cloned());
            }
        }
        let expected_overlay_bytes = base_overlay_bytes
            .checked_add(extent_total)
            .ok_or_else(|| Error::corrupt("overlay byte accounting overflows"))?;
        if overlay.overlay_bytes != expected_overlay_bytes {
            return Err(Error::corrupt(format!(
                "overlay_bytes is {}, expected {expected_overlay_bytes}",
                overlay.overlay_bytes
            )));
        }
        if table.len() as u64 > MAX_CHUNKS_PER_FILE {
            return Err(Error::limit_exceeded(format!(
                "resolved overlay table exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
            )));
        }
        let total: u64 = table.iter().map(|chunk| chunk.len).sum();
        if total != manifest.size {
            return Err(Error::corrupt(format!(
                "resolved overlay table sums to {total}, manifest size is {}",
                manifest.size
            )));
        }
        let root =
            identity::merkle_root_from_chunks(table.iter().map(|chunk| (chunk.len, chunk.hash)));
        if root != manifest.content_hash {
            return Err(Error::corrupt(
                "resolved overlay table does not reproduce the manifest content root",
            ));
        }
        // The grid invariant: every chunk is full except the final one.
        for (index, chunk) in table.iter().enumerate() {
            if index + 1 < table.len() && chunk.len != manifest.chunk_size {
                return Err(Error::corrupt(
                    "resolved overlay table has a short interior chunk",
                ));
            }
        }
        Ok(table)
    }

    /// The canonical chunk table of any chunk-table-backed manifest.
    ///
    /// A v3 raw table is returned directly; an overlay is resolved through its
    /// base chain. Frame-backed representations and whole-file bases are
    /// rejected because their chunk layout is not composable. An overlay table
    /// is fully validated (grid, lengths, root) by
    /// [`Store::build_overlay_table`]; a raw table is validated by
    /// [`crate::format::Manifest::from_wcb`].
    pub(crate) fn canonical_chunk_table(
        &self,
        manifest_id: Hash32,
        manifest: &Manifest,
    ) -> Result<Arc<Vec<ChunkRef>>> {
        match manifest.representation.as_ref().map(|rep| rep.kind) {
            None | Some(RepresentationKind::Raw) => {
                if manifest.identity != ContentIdentity::MerkleV3 {
                    return Err(Error::unsupported_representation(
                        "overlay writes require a Merkle (`weft.manifest.v3`) base",
                    ));
                }
                Ok(Arc::new(manifest.chunks.clone()))
            }
            Some(RepresentationKind::Overlay) => self.overlay_chunk_table(manifest_id, manifest),
            Some(_) => Err(Error::unsupported_representation(
                "overlay writes require a chunk-table base, not a frame-backed representation",
            )),
        }
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
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
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
        let frame_bytes =
            self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
        self.note_probe_bytes(frame_bytes.len() as u64);
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected_mode = match rep.strategy.as_str() {
            "zstd" => Mode::Zstd,
            "zstd-patch" => Mode::Dictionary,
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
            let dep_manifest = self.read_manifest_keyed(dep_root)?;
            seen.push(dep_root);
            let bytes = self.read_manifest_bytes_inner_gated(
                &dep_manifest,
                0,
                dep_manifest.size,
                true,
                depth + 1,
                seen,
                gate,
                cancel,
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
        if self.options.representation == RepresentationMode::Overlay
            && self.options.content_identity != ContentIdentity::MerkleV3
        {
            return Err(Error::unsupported_representation(
                "`--representation overlay` requires the Merkle content identity; \
                 pass `--content-identity merkle`",
            ));
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
        if self.options.representation == RepresentationMode::Overlay
            && self.options.content_identity != ContentIdentity::MerkleV3
        {
            return Err(Error::unsupported_representation(
                "`--representation overlay` requires the Merkle content identity; \
                 pass `--content-identity merkle`",
            ));
        }

        let (manifest_id, content_hash, size) = match self.options.representation {
            RepresentationMode::Raw | RepresentationMode::Overlay => {
                let identity_mode = self.options.content_identity;
                let (chunks, size, content_hash) =
                    self.stream_raw_chunks(&mut reader, chunk_size, Vec::new(), identity_mode)?;
                self.store_streamed_manifest(
                    chunk_size,
                    chunks,
                    size,
                    content_hash,
                    false,
                    identity_mode,
                )?
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
                        ContentIdentity::WholeFileV1,
                    )?
                } else {
                    // Larger stream: keep the preamble (a chunk multiple) and
                    // continue streaming into a v2 `kind:"raw"` manifest.
                    // Codec-backed streaming is not implemented; buffering the
                    // file to reach the codec encoder would violate the memory
                    // goal, and a streaming DELTA/ZSTD writer does not exist.
                    let (chunks, size, content_hash) = self.stream_raw_chunks(
                        &mut reader,
                        chunk_size,
                        preamble,
                        ContentIdentity::WholeFileV1,
                    )?;
                    self.store_streamed_manifest(
                        chunk_size,
                        chunks,
                        size,
                        content_hash,
                        true,
                        ContentIdentity::WholeFileV1,
                    )?
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
    pub(crate) fn commit_created_file(
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
    /// A `weft.manifest.v3` (Merkle) file also avoids the O(file size) digest
    /// pass: its root is folded from the stored chunk hashes. A v1/v2 file still
    /// needs one digest pass over the whole file to keep the frozen
    /// `ContentId`; see `Store::store_file_ranges` for the exact cost model.
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
        let outcome = self.write_file_ranges_causal(id, &[(offset, bytes)], causal, None)?;
        Ok(outcome.version)
    }

    /// Replace a byte range, reusing a manifest the caller already holds.
    ///
    /// The cache is advisory: when `cached.content` is still the object's
    /// current manifest id and `cached.manifest.size` matches the object size,
    /// the manifest is not read from disk again. A stale or mismatched cache is
    /// ignored (the manifest is read normally), so a cache can never change a
    /// result. This removes one manifest read per write from a long-lived
    /// handle; it does not affect identity, hashing, or durability.
    ///
    /// Returns the new version id, the manifest id, and the new size so the
    /// caller can refresh its cache without re-reading state.
    pub fn write_file_cached(
        &mut self,
        id: Hash32,
        offset: u64,
        bytes: &[u8],
        cached: Option<&ManifestCache>,
    ) -> Result<WriteOutcome> {
        self.write_file_ranges_causal(id, &[(offset, bytes)], &[], cached)
    }

    /// Apply several byte ranges with one manifest read, one manifest write,
    /// and one commit.
    ///
    /// Ranges are applied in the order given, so overlapping writes have the
    /// same last-writer-wins result they would have as separate calls, but the
    /// manifest is decoded and encoded once and every touched chunk is
    /// encoded at most once. Empty ranges are ignored; an all-empty input is a
    /// no-op that publishes nothing. The file's existing identity is kept (a
    /// v3 Merkle file is never downgraded).
    pub fn write_file_ranges(
        &mut self,
        id: Hash32,
        ranges: &[(u64, &[u8])],
    ) -> Result<WriteOutcome> {
        self.write_file_ranges_causal(id, ranges, &[], None)
    }

    /// Shared range-write core. `ranges` are applied in order against one
    /// preloaded manifest and published as one commit.
    fn write_file_ranges_causal(
        &mut self,
        id: Hash32,
        ranges: &[(u64, &[u8])],
        causal: &[Hash32],
        cached: Option<&ManifestCache>,
    ) -> Result<WriteOutcome> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("write_file requires a file object"));
        }
        let ranges: Vec<(u64, &[u8])> = ranges
            .iter()
            .copied()
            .filter(|(_, bytes)| !bytes.is_empty())
            .collect();
        if ranges.is_empty() {
            return Ok(WriteOutcome {
                version: old.version,
                content: old.content,
                size: old.size,
            });
        }
        let mut end = old.size;
        for (offset, bytes) in &ranges {
            let range_end = offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
            if range_end > MAX_FILE_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "file exceeds the {MAX_FILE_BYTES}-byte bound"
                )));
            }
            end = end.max(range_end);
        }
        // The caller's manifest is only a cache: it must name the object's
        // current content id and size, otherwise it is ignored.
        let old_manifest = match cached {
            Some(cache) if cache.content == old.content && cache.manifest.size == old.size => {
                cache.manifest.clone()
            }
            _ => self.read_manifest_keyed(old.content)?,
        };
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        let new_size = end.max(old.size);
        let old_is_frame_backed = old_manifest
            .representation
            .as_ref()
            .map(|rep| {
                rep.kind == RepresentationKind::Codec || rep.kind == RepresentationKind::Tensor
            })
            .unwrap_or(false);
        if manifest_is_overlay(&old_manifest) || self.overlay_mode() {
            if old_is_frame_backed {
                let kind = old_manifest
                    .representation
                    .as_ref()
                    .map(|rep| rep.kind.as_str())
                    .unwrap_or("none");
                return Err(Error::unsupported_representation(format!(
                    "range write to a {kind} representation cannot become an overlay layer; \
                     re-create the file with `--content-identity merkle --representation overlay` \
                     or write with `--representation raw`"
                )));
            }
            let edits: Vec<OverlayEdit<'_>> = ranges
                .iter()
                .map(|(offset, bytes)| OverlayEdit::bytes(*offset, bytes))
                .collect();
            return self.write_file_overlay(id, &old, &old_manifest, &edits, new_size, causal);
        }
        // CodecAuto, or writing back to a store whose current version is a
        // frame-backed (codec or tensor) representation, materialises the whole
        // object once. This keeps the experimental and tensor paths simple; the
        // frozen Raw mode still uses the chunk-granular COW path below.
        //
        // The file's existing identity wins: a `weft.manifest.v3` file is never
        // silently rewritten as v1/v2 even when the store-wide option is the
        // default. A v1/v2 file is upgraded to v3 only when the option asks for
        // it.
        let content_identity = match old_manifest.identity {
            ContentIdentity::MerkleV3 => ContentIdentity::MerkleV3,
            ContentIdentity::WholeFileV1 => self.options.content_identity,
        };
        if self.options.representation == RepresentationMode::CodecAuto || old_is_frame_backed {
            let old_bytes =
                self.read_manifest_bytes(&old_manifest, old.content, 0, old_manifest.size, true)?;
            let mut buffer = vec![0u8; checked_usize(new_size, "file size")?];
            buffer[..old_bytes.len()].copy_from_slice(&old_bytes);
            for (offset, bytes) in &ranges {
                let offset_usize = checked_usize(*offset, "write offset")?;
                let end_usize = checked_usize(*offset + bytes.len() as u64, "write end")?;
                buffer[offset_usize..end_usize].copy_from_slice(bytes);
            }
            // A new explicit causal parent wins; otherwise an existing
            // causal-exact object keeps using its recorded parents as bases.
            let parent_versions: Vec<Hash32> = if causal.is_empty() {
                crate::state::object_causal_closure(&old)?
                    .map(|closure| closure.parents)
                    .unwrap_or_default()
            } else {
                causal.to_vec()
            };
            let base = if content_identity == ContentIdentity::MerkleV3 {
                None
            } else {
                self.delta_base(&parent_versions)?
            };
            let (manifest_id, _content, _size) = self.store_content_bytes(
                &buffer,
                self.options.chunk_size,
                base.as_ref().map(|(root, bytes)| (*root, bytes.as_slice())),
                self.options.representation == RepresentationMode::CodecAuto
                    && content_identity == ContentIdentity::WholeFileV1,
                content_identity,
            )?;
            let version = self.publish_file_version(id, old, manifest_id, new_size, causal)?;
            return Ok(WriteOutcome {
                version,
                content: manifest_id,
                size: new_size,
            });
        }
        let (manifest_id, _content, _size) =
            self.store_file_ranges(&old_manifest, &ranges, new_size, content_identity)?;
        let version = self.publish_file_version(id, old, manifest_id, new_size, causal)?;
        Ok(WriteOutcome {
            version,
            content: manifest_id,
            size: new_size,
        })
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
        let old_manifest = self.read_manifest_keyed(old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if old_manifest
            .representation
            .as_ref()
            .map(|rep| {
                rep.kind == RepresentationKind::Codec
                    || rep.kind == RepresentationKind::Tensor
                    || rep.kind == RepresentationKind::Overlay
            })
            .unwrap_or(false)
        {
            return Err(Error::invalid_argument(
                "streaming append to a frame-backed or overlay object is not implemented",
            ));
        }
        let chunk_size = old_manifest.chunk_size;
        validate_chunk_size(chunk_size)?;
        let content_identity = match old_manifest.identity {
            ContentIdentity::MerkleV3 => ContentIdentity::MerkleV3,
            ContentIdentity::WholeFileV1 => self.options.content_identity,
        };
        let whole_file_identity = content_identity == ContentIdentity::WholeFileV1;
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
            // Merkle identity folds the stored chunk hashes; whole-file identity
            // must re-read every chunk to feed the streaming digest.
            if whole_file_identity {
                let bytes = self.read_blob_verified_keyed(
                    BlobKind::Object,
                    reference.hash,
                    identity::CHUNK_DOMAIN,
                )?;
                if bytes.len() as u64 != reference.len {
                    return Err(Error::corrupt("chunk length disagrees with manifest"));
                }
                hasher.update(&bytes);
            }
            chunks.push(reference.clone());
        }

        let mut pending: Vec<u8> = Vec::new();
        if tail_len != 0 {
            let reference = old_manifest
                .chunks
                .get(checked_usize(full_chunks, "chunk index")?)
                .ok_or_else(|| Error::corrupt("manifest chunk is missing"))?;
            let bytes = self.read_blob_verified_keyed(
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
                    if whole_file_identity {
                        hasher.update(&pending);
                    }
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
                if whole_file_identity {
                    hasher.update(part);
                }
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
            if whole_file_identity {
                hasher.update(&pending);
            }
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
        let content_hash = match content_identity {
            ContentIdentity::WholeFileV1 => identity::finish_content_hash(hasher),
            ContentIdentity::MerkleV3 => identity::merkle_root_from_chunks(
                chunks.iter().map(|chunk| (chunk.len, chunk.hash)),
            ),
        };
        debug_assert!(chunks.iter().map(|chunk| chunk.len).sum::<u64>() == new_size);
        let representation = if content_identity == ContentIdentity::WholeFileV1
            && self.options.representation == RepresentationMode::CodecAuto
        {
            Some(Representation {
                kind: RepresentationKind::Raw,
                frame: None,
                dependencies: Vec::new(),
                strategy: "raw".to_owned(),
                decoded_size: new_size,
                tensor: None,
                overlay: None,
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
            identity: content_identity,
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
        let (mut attrs, identity_mode, causal_hash, closure) = if causal.is_empty() {
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
        // A byte rewrite replaces the tensor representation; stale tensor
        // identity attributes must never survive it.
        strip_tensor_attrs(&mut attrs);
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

    /// Publish an already-stored manifest id as the current version of a file
    /// object. `#[doc(hidden)]` test-only hook: the write path for
    /// `weft.manifest.v4` does not exist yet, so read/GC tests construct the
    /// manifest and its chunks by hand and then make an object reference it.
    #[doc(hidden)]
    pub fn publish_manifest_for_test(
        &mut self,
        id: Hash32,
        manifest_id: Hash32,
        size: u64,
    ) -> Result<Hash32> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "test manifest publication requires a file object",
            ));
        }
        let manifest = self.read_manifest_keyed(manifest_id)?;
        if manifest.size != size {
            return Err(Error::invalid_argument(
                "test manifest publication size disagrees with the manifest",
            ));
        }
        self.publish_file_version(id, old, manifest_id, size, &[])
    }

    /// Publish a new file version with explicit attributes (the tensor range
    /// write updates the tensor identity attributes in the same commit).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_file_version_with_attrs(
        &mut self,
        id: Hash32,
        old: ObjectRecord,
        manifest_id: Hash32,
        _content_hash: Hash32,
        new_size: u64,
        causal: &[Hash32],
        mut attrs: BTreeMap<String, Value>,
    ) -> Result<Hash32> {
        let (identity_mode, causal_hash, closure) = if causal.is_empty() {
            let (mode, hash) = crate::state::identity_from_attrs(&old)?;
            (mode, hash, None)
        } else {
            let (prepared, mode, hash, closure) = crate::context::prepare_causal(
                self.current_state(),
                attrs,
                causal,
                &Limits::default(),
            )?;
            attrs = prepared;
            (mode, hash, closure)
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
    ///
    /// On an overlay-represented object this publishes one new overlay extent
    /// covering the chunk-aligned span of the zeroed range (full zero chunks
    /// are content-addressed once, so memory stays bounded by one chunk).
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
        let old_manifest = self.read_manifest_keyed(old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if manifest_is_overlay(&old_manifest) || self.overlay_mode() {
            let edit = OverlayEdit::zeros(offset, zero_end - offset);
            let outcome =
                self.write_file_overlay(id, &old, &old_manifest, &[edit], new_size, &[])?;
            return Ok(outcome.version);
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
    ///
    /// Refused with `UnsupportedRepresentation` for an overlay-represented
    /// object: shortening the file is not expressible as an immutable overlay
    /// extent (an extent can replace or extend chunks but never remove the
    /// tail), and silently materialising the object would defeat the point of
    /// the representation. Compact the object first, or use the raw
    /// representation.
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
        let manifest = self.read_manifest_keyed(old.content)?;
        if manifest_is_overlay(&manifest) || self.overlay_mode() {
            return Err(Error::unsupported_representation(
                "collapse_range on an overlay object would shorten the file and shift every \
                 following byte; run `weftfs compact <path>` first or use `--representation raw`",
            ));
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
    ///
    /// Refused with `UnsupportedRepresentation` for an overlay-represented
    /// object: an insert rewrites every byte after `offset`, which would be a
    /// whole-tail overlay extent rather than a bounded layer, so the caller
    /// must compact (or use the raw representation) rather than have the store
    /// silently materialise the file.
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
        let manifest = self.read_manifest_keyed(old.content)?;
        if manifest_is_overlay(&manifest) || self.overlay_mode() {
            return Err(Error::unsupported_representation(
                "insert_range on an overlay object rewrites every byte after the insert point; \
                 run `weftfs compact <path>` first or use `--representation raw`",
            ));
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
        let content_identity = self.content_identity_for_object(&old)?;
        let (manifest_id, _content, _size) =
            self.store_file_content_with_identity(buffer, content_identity)?;
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
        let mut attrs = old.attrs.clone();
        // A byte rewrite replaces the tensor representation; stale tensor
        // identity attributes must never survive it.
        strip_tensor_attrs(&mut attrs);
        let record = ObjectRecord {
            version,
            mtime_ns: mtime,
            content: manifest_id,
            size,
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

    /// Resize a file to `size`, zero-filling when growing.
    ///
    /// On an overlay-represented object: growing publishes one overlay extent
    /// of zeros for `[old_size, size)`; truncating to the same size is a no-op;
    /// truncating to a smaller size is refused with
    /// `UnsupportedRepresentation` because an immutable overlay extent can
    /// replace or extend chunks but never drop the tail. Compact first, or use
    /// the raw representation.
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
        let old_manifest = self.read_manifest_keyed(old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if manifest_is_overlay(&old_manifest) || self.overlay_mode() {
            if size < old.size {
                return Err(Error::unsupported_representation(
                    "truncate to a smaller size cannot be expressed as an immutable overlay \
                     extent; run `weftfs compact <path>` first or use `--representation raw`",
                ));
            }
            if size == old.size {
                return Ok(old.version);
            }
            let edit = OverlayEdit::zeros(old.size, size - old.size);
            let outcome = self.write_file_overlay(id, &old, &old_manifest, &[edit], size, &[])?;
            return Ok(outcome.version);
        }
        let current = self.read_object(id)?;
        let mut buffer = vec![0u8; checked_usize(size, "file size")?];
        let copy = current.len().min(buffer.len());
        buffer[..copy].copy_from_slice(&current[..copy]);
        let content_identity = self.content_identity_for_object(&old)?;
        let (manifest_id, _content, _size) =
            self.store_file_content_with_identity(&buffer, content_identity)?;
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

    /// Canonicalise an overlay-represented file into one
    /// `weft.manifest.v3` chunk table.
    ///
    /// Every logical chunk is re-read and re-hashed in order (bounded to one
    /// chunk of memory), the Merkle root is recomputed, and it MUST equal the
    /// pre-compaction `content_root`. Any mismatch fails closed with `Corrupt`
    /// before a manifest or commit is published. The canonical single-table
    /// manifest is then published through the normal commit protocol, so old
    /// overlay layers and their chunks stay valid for snapshots and are
    /// reclaimed only when unreferenced.
    ///
    /// A non-overlay object is refused with `UnsupportedRepresentation`; there
    /// is nothing to compact and rewriting it would be a representation change
    /// the caller did not ask for.
    pub fn compact(&mut self, id: Hash32) -> Result<CompactReport> {
        self.require_writable()?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument("compact requires a file object"));
        }
        let old_manifest = self.read_manifest_keyed(old.content)?;
        if old_manifest.size != old.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        let (overlay_depth, overlay_bytes) = match old_manifest
            .representation
            .as_ref()
            .filter(|rep| rep.kind == RepresentationKind::Overlay)
            .and_then(|rep| rep.overlay.as_ref())
        {
            Some(overlay) => (overlay.depth, overlay.overlay_bytes),
            None => {
                return Err(Error::unsupported_representation(
                    "compact requires an overlay-represented object",
                ))
            }
        };
        let table = self.canonical_chunk_table(old.content, &old_manifest)?;
        if table.len() as u64 > MAX_CHUNKS_PER_FILE {
            return Err(Error::limit_exceeded(format!(
                "manifest exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
            )));
        }
        let mut verified_bytes: u64 = 0;
        let mut leaves: Vec<(u64, Hash32)> = Vec::with_capacity(table.len());
        for (index, chunk) in table.iter().enumerate() {
            let bytes = self
                .read_blob_verified_keyed(BlobKind::Object, chunk.hash, identity::CHUNK_DOMAIN)
                .map_err(|err| chunk_read_failure(index, chunk.hash, err))?;
            if bytes.len() as u64 != chunk.len {
                return Err(Error::corrupt(format!(
                    "chunk {index} length disagrees with the overlay table"
                )));
            }
            verified_bytes = verified_bytes
                .checked_add(chunk.len)
                .ok_or_else(|| Error::corrupt("verified byte total overflows"))?;
            leaves.push((chunk.len, chunk.hash));
        }
        if verified_bytes != old_manifest.size {
            return Err(Error::corrupt(format!(
                "verified {verified_bytes} bytes, manifest size is {}",
                old_manifest.size
            )));
        }
        let content_root = identity::merkle_root_from_chunks(leaves);
        if content_root != old_manifest.content_hash {
            return Err(Error::corrupt(
                "compaction would change the content root; refusing to publish",
            ));
        }
        // No content blob is written: every chunk is content-addressed and was
        // just re-verified. Only the manifest shape changes.
        let compacted = Manifest {
            size: old_manifest.size,
            chunk_size: old_manifest.chunk_size,
            chunks: table.as_ref().clone(),
            content_hash: content_root,
            representation: None,
            identity: ContentIdentity::MerkleV3,
        };
        let encoded = compacted.encode()?;
        let round_trip = Manifest::from_wcb(&crate::wcb_util::decode(&encoded)?)?;
        if round_trip.content_hash != old_manifest.content_hash
            || round_trip.chunks != *table
            || round_trip.size != old_manifest.size
        {
            return Err(Error::corrupt(
                "compacted manifest does not reproduce the pre-compaction table",
            ));
        }
        let manifest_id =
            self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, &encoded)?;
        let previous_content = old.content;
        let version = self.publish_file_version(id, old, manifest_id, old_manifest.size, &[])?;
        Ok(CompactReport {
            object: id,
            previous_content,
            content: manifest_id,
            version,
            size: old_manifest.size,
            content_root,
            chunks: round_trip.chunks.len() as u64,
            overlay_depth,
            overlay_bytes,
            verified_bytes,
        })
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
            ops.extend(self.delete_object_ops(target));
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
            ops.extend(self.delete_object_ops(target));
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
                ops.extend(self.delete_object_ops(existing.target));
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
                let ops = self.delete_object_ops(id);
                self.commit_ops(ops, false)?;
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

    /// Resolve a snapshot name or an on-chain commit hex to its commit hash.
    /// A commit that is not reachable from the recovered head is `NotFound`
    /// (an unpublished crash-tail file is never addressable).
    pub fn resolve_snapshot_ref(&self, reference: &str) -> Result<Hash32> {
        if let Some(commit) = self.state.snapshots.get(reference) {
            return Ok(*commit);
        }
        if let Ok(hash) = Hash32::from_hex(reference) {
            if self.is_published_ancestor(hash)? {
                return Ok(hash);
            }
            return Err(Error::not_found(
                "commit is not on the published parent chain",
            ));
        }
        Err(Error::not_found("no such snapshot or commit"))
    }

    /// Branch an existing snapshot: create `name` pointing at the source
    /// snapshot's commit. History is never rewritten; the new ref is just
    /// another name for the same immutable commit, so later writes cannot move
    /// either ref.
    pub fn snapshot_branch(&mut self, source: &str, name: &str) -> Result<Hash32> {
        self.require_writable()?;
        validate_snapshot_name(name)?;
        if self.state.snapshots.contains_key(name) {
            return Err(Error::exists("snapshot name already exists"));
        }
        let commit = self.resolve_snapshot_ref(source)?;
        self.commit_ops(
            vec![Op::SetSnapshot {
                name: name.to_owned(),
                commit,
            }],
            false,
        )?;
        self.write_snapshot_pointer(name, commit)?;
        Ok(commit)
    }

    /// Union-merge two snapshot refs (or commits) into a new commit on the live
    /// head.
    ///
    /// This is deliberately a **union merge**, not a three-way content merge.
    /// Every path present in either input is copied when the live head does not
    /// already have it. A path present in both inputs, or already present at
    /// the head, must have identical content (for a regular file: the frozen
    /// `ContentId` and size; for a symlink: the target); otherwise the merge
    /// fails closed with `Conflict` naming the path and publishes nothing.
    /// Metadata of a path that already exists at the head is never overwritten.
    pub fn merge_branches(&mut self, left: &str, right: &str) -> Result<MergeReport> {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "merge cannot run inside a group commit",
            ));
        }
        let left_commit = self.resolve_snapshot_ref(left)?;
        let right_commit = self.resolve_snapshot_ref(right)?;
        let left_state = self.state_at(left_commit)?;
        let right_state = self.state_at(right_commit)?;
        let head_paths = collect_state_paths(&self.state)?;
        let left_paths = collect_state_paths(&left_state)?;
        let right_paths = collect_state_paths(&right_state)?;

        // Reject overlapping content first, so no blob or commit is written for
        // a merge that cannot succeed.
        for (path, node) in &left_paths {
            if let Some(other) = right_paths.get(path) {
                if !same_path_content(node, other) {
                    return Err(Error::conflict(format!(
                        "path `{}` has different content in the two branches",
                        String::from_utf8_lossy(path)
                    )));
                }
            }
        }
        for (path, node) in left_paths.iter().chain(right_paths.iter()) {
            if let Some(existing) = head_paths.get(path) {
                if !same_path_content(existing, node) {
                    return Err(Error::conflict(format!(
                        "path `{}` already exists with different content",
                        String::from_utf8_lossy(path)
                    )));
                }
            }
        }

        // Union in ascending path order so parents always precede children.
        let mut union: BTreeMap<Vec<u8>, PathNode> = BTreeMap::new();
        for (path, node) in &left_paths {
            union.insert(path.clone(), node.clone());
        }
        for (path, node) in &right_paths {
            union.entry(path.clone()).or_insert_with(|| node.clone());
        }

        let mut ops: Vec<Op> = Vec::new();
        let mut merged: BTreeMap<Vec<u8>, Hash32> = BTreeMap::new();
        merged.insert(b"/".to_vec(), State::ROOT_ID);
        let mut added_objects = 0u64;
        let mut added_entries = 0u64;
        let mut new_ids: BTreeSet<Hash32> = BTreeSet::new();
        for (path, node) in &union {
            if path.as_slice() == b"/" {
                continue;
            }
            if let Some(existing) = head_paths.get(path) {
                merged.insert(path.clone(), existing.id);
                continue;
            }
            let (parent_path, name) = split_path(path)?;
            let parent_id = match merged.get(&parent_path) {
                Some(id) => *id,
                None => {
                    return Err(Error::conflict(format!(
                        "merge would place `{}` under a missing parent",
                        String::from_utf8_lossy(path)
                    )))
                }
            };
            let parent = self
                .state
                .objects
                .get(&parent_id)
                .ok_or_else(|| Error::corrupt("merged parent object is missing"))?;
            if parent.kind != ObjectKind::Dir {
                return Err(Error::conflict(format!(
                    "merge needs `{}` to be a directory",
                    String::from_utf8_lossy(&parent_path)
                )));
            }
            // Reuse the source object id when it is free or names an identical
            // record; otherwise copy the record under a deterministic merge id.
            let mut record = node.record.clone();
            match self.state.objects.get(&record.id) {
                None if !new_ids.contains(&record.id) => {
                    new_ids.insert(record.id);
                }
                Some(existing) if existing == &record => {}
                _ => {
                    let mut seed = b"weft/merge/v1".to_vec();
                    seed.extend_from_slice(path);
                    seed.extend_from_slice(record.content.as_bytes());
                    record.id = identity::h(identity::OBJECT_DOMAIN, &seed);
                    if new_ids.contains(&record.id) {
                        return Err(Error::corrupt("merge id derivation collided"));
                    }
                    new_ids.insert(record.id);
                }
            }
            if !self.state.objects.contains_key(&record.id) {
                ops.push(Op::PutObject(record.clone()));
                ops.push(Op::SetTip {
                    object: record.id,
                    version: record.version,
                });
                added_objects += 1;
            }
            let entry = EntryRecord {
                id: derive_entry_id(parent_id, name)?,
                parent: parent_id,
                name: name.to_vec(),
                target: record.id,
            };
            if !self.state.entries.contains_key(&entry.id) {
                ops.push(Op::PutEntry(entry));
                added_entries += 1;
            }
            merged.insert(path.clone(), record.id);
        }

        let head = if ops.is_empty() {
            self.head
        } else {
            self.commit_ops(ops, false)?
        };
        Ok(MergeReport {
            left: left_commit,
            right: right_commit,
            paths: u64::try_from(union.len().saturating_sub(1))
                .map_err(|_| Error::limit_exceeded("path count does not fit u64"))?,
            added_objects,
            added_entries,
            head,
            generation: self.generation,
        })
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
        self.begin_group()?;
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
        let hash = self.commit_group()?;
        Ok((value, hash))
    }

    /// Open a group that spans multiple calls.
    ///
    /// This is the non-closure form of [`Store::group`], for callers that cannot
    /// keep one closure open across the mutations they want to batch (the FUSE
    /// frontend serves one request per callback). The semantics are identical:
    /// mutations buffer their ops and are applied to the live in-memory state;
    /// nothing is published until [`Store::commit_group`]. A crash before that
    /// point leaves the previous whole generation. Callers must always close
    /// the group with [`Store::commit_group`] or [`Store::abort_group`];
    /// dropping a store with an open group publishes nothing.
    pub fn begin_group(&mut self) -> Result<()> {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "group commit cannot be nested inside another group",
            ));
        }
        // Snapshot the committed blob set while the live state still equals it.
        if self.quota.enforced() {
            self.ensure_blob_index()?;
        }
        self.group_depth = 1;
        self.pending_ops.clear();
        self.group_undo.clear();
        self.group_pre_objects.clear();
        self.group_pre_relations.clear();
        self.group_force_checkpoint = false;
        if self.options.defer_group_blob_sync {
            // One group is one publication, so it is also one durability
            // barrier: blob publications inside the group write and rename their
            // bytes but leave the `fsync` to the barrier the commit path already
            // performs before it publishes the commit record. A backend that
            // cannot defer keeps its eager per-blob barriers.
            let _ = self.with_backend_mut(|backend| backend.begin_deferred_sync())?;
        }
        Ok(())
    }

    /// Publish every buffered group op as exactly one commit and close the
    /// group. Returns the current head when no op was buffered (nothing is
    /// published). On error the in-memory state is rolled back to the pre-group
    /// value and the group is closed.
    pub fn commit_group(&mut self) -> Result<Hash32> {
        if self.group_depth == 0 {
            return Err(Error::invalid_argument("no group commit is open"));
        }
        let ops = std::mem::take(&mut self.pending_ops);
        let undo = std::mem::take(&mut self.group_undo);
        let pre_objects = std::mem::take(&mut self.group_pre_objects);
        let pre_relations = std::mem::take(&mut self.group_pre_relations);
        let force_checkpoint = std::mem::replace(&mut self.group_force_checkpoint, false);
        self.group_depth = 0;
        if ops.is_empty() {
            self.end_deferred_sync_best_effort();
            return Ok(self.head);
        }
        match self.publish_staged(ops, pre_objects, pre_relations, force_checkpoint) {
            Ok(hash) => Ok(hash),
            Err(err) => {
                // Nothing is published on an error return, so the in-memory
                // state must go back to the pre-group value.
                self.rollback(&undo);
                Err(err)
            }
        }
    }

    /// Discard the open group without publishing and restore the pre-group
    /// in-memory state.
    pub fn abort_group(&mut self) {
        if self.group_depth == 0 {
            return;
        }
        let undo = std::mem::take(&mut self.group_undo);
        self.rollback(&undo);
        self.pending_ops.clear();
        self.group_pre_objects.clear();
        self.group_pre_relations.clear();
        self.group_force_checkpoint = false;
        self.group_depth = 0;
        // Nothing is published, so the blobs written by the aborted group are
        // unreferenced. Closing the batch everywhere a batch was opened keeps
        // the next publication from inheriting an unbounded sync list.
        self.end_deferred_sync_best_effort();
    }

    /// Close a deferred durability batch if one is open. Used on paths where
    /// nothing is published, so an error cannot change an outcome: the data is
    /// unreferenced and a later barrier or open re-syncs it.
    fn end_deferred_sync_best_effort(&self) {
        let _ = self.with_backend_mut(|backend| backend.end_deferred_sync());
    }

    /// True while a group opened by [`Store::begin_group`] or [`Store::group`]
    /// is buffering mutations.
    pub fn group_open(&self) -> bool {
        self.group_depth != 0
    }

    // ---- MW2 transactions ------------------------------------------------

    /// Begin an MW2 transaction against the head this handle has selected.
    ///
    /// Only an optimistic store accepts this; the default single-writer store
    /// keeps its existing mutation API. The transaction records the base head
    /// and every pre-image as the caller adds ops, so a rebase can decide the
    /// conflict table without re-reading the caller's intent.
    pub fn begin_transaction(&self, id: impl Into<String>) -> Result<crate::mw2::Transaction> {
        if self.mode != OpenMode::ReadWrite {
            return Err(Error::read_only(
                "store is opened read-only; a transaction cannot publish",
            ));
        }
        if !matches!(
            self.options.writer_mode,
            crate::mw2::WriterMode::Optimistic | crate::mw2::WriterMode::Sharded
        ) {
            return Err(Error::invalid_argument(
                "transactions require --writer-mode optimistic or sharded; the default \
                 single-writer store keeps its exclusive committer",
            ));
        }
        Ok(crate::mw2::Transaction {
            id: crate::mw2::TransactionId::new(id)?,
            base_head: self.head,
            base_generation: self.generation,
            ops: Vec::new(),
            preconditions: Vec::new(),
            auth: Vec::new(),
            pins: Vec::new(),
        })
    }

    /// Store content-addressed bytes and return `(manifest id, content hash,
    /// size)` without publishing a commit. This is the prepare half of an MW2
    /// transaction: the caller builds an `ObjectRecord` whose `content` is the
    /// returned manifest id and publishes it through the transaction.
    ///
    /// Every blob is create-once and immutable, so preparing concurrently is
    /// always safe. An unreferenced manifest is garbage for GC.
    pub fn prepare_content(&self, bytes: &[u8]) -> Result<(Hash32, Hash32, u64)> {
        self.require_writable()?;
        self.store_content_bytes(
            bytes,
            self.options.chunk_size,
            None,
            self.options.representation == RepresentationMode::CodecAuto,
            self.options.content_identity,
        )
    }

    /// Write the prepared record for a transaction without publishing.
    ///
    /// The record is what pins the transaction's manifests for GC and what a
    /// later `transaction_outcome` lookup reads. It is not a commit and is not
    /// visible to readers.
    pub fn prepare_transaction(&mut self, tx: &crate::mw2::Transaction) -> Result<()> {
        self.require_writable()?;
        if !matches!(
            self.options.writer_mode,
            crate::mw2::WriterMode::Optimistic | crate::mw2::WriterMode::Sharded
        ) {
            return Err(Error::invalid_argument(
                "preparing a transaction requires --writer-mode optimistic or sharded",
            ));
        }
        self.write_tx_record(tx, crate::mw2::TxRecordState::Prepared, None)
    }

    /// Publish one transaction with a bounded optimistic loop.
    ///
    /// Each attempt revalidates every precondition and authorization check
    /// against the current head, applies the ops to a candidate state, and
    /// publishes the commit with a conditional head compare-and-swap. A lost
    /// race retries only when the conflict table proves the ops still apply;
    /// otherwise the typed conflict is returned. The retry budget is hard.
    ///
    /// T026: when a history sink is configured this is the **only** place a
    /// transaction's invocation and response are recorded, so a multi-attempt
    /// transaction appears as one operation with its final outcome and its
    /// attempt count.
    pub fn execute_transaction(
        &mut self,
        tx: &crate::mw2::Transaction,
        options: &crate::mw2::TransactionOptions,
    ) -> Result<crate::mw2::TransactionOutcome> {
        let recording = self.history.is_some();
        if recording {
            let ops: Vec<serde_json::Value> =
                transaction_history_ops(tx).into_iter().map(|op| op.to_json()).collect();
            let preconditions: Vec<serde_json::Value> = tx
                .preconditions
                .iter()
                .map(|pre| {
                    serde_json::json!({
                        "label": pre.label,
                        "expected": describe_pre_value(&pre.expected),
                        "idempotent": pre.idempotent.as_ref().map(describe_pre_value),
                    })
                })
                .collect();
            self.emit_history(serde_json::json!({
                "event": "tx-invoke",
                "tx": tx.id.as_str(),
                "base_generation": tx.base_head_generation(),
                "ops": ops,
                "preconditions": preconditions,
            }));
        }
        let result = self.execute_transaction_inner(tx, options);
        if recording {
            match &result {
                Ok(outcome) => self.emit_history(serde_json::json!({
                    "event": "tx-response",
                    "tx": tx.id.as_str(),
                    "outcome": if outcome.idempotent { "idempotent" } else { "committed" },
                    "generation": outcome.generation,
                    "commit": outcome.commit.to_hex(),
                    "attempts": outcome.attempts,
                    "observed_generation": self.generation,
                })),
                Err(err) => self.emit_history(serde_json::json!({
                    "event": "tx-response",
                    "tx": tx.id.as_str(),
                    "outcome": "rejected",
                    "class": err.class().as_str(),
                    "error": err.to_string(),
                    "observed_generation": self.generation,
                })),
            }
        }
        result
    }

    fn execute_transaction_inner(
        &mut self,
        tx: &crate::mw2::Transaction,
        options: &crate::mw2::TransactionOptions,
    ) -> Result<crate::mw2::TransactionOutcome> {
        self.require_writable()?;
        if !matches!(
            self.options.writer_mode,
            crate::mw2::WriterMode::Optimistic | crate::mw2::WriterMode::Sharded
        ) {
            return Err(Error::invalid_argument(
                "transactions require --writer-mode optimistic or sharded",
            ));
        }
        if tx.ops.is_empty() {
            return Err(Error::invalid_argument("transaction has no ops"));
        }
        if tx.ops.len() > options.max_ops {
            return Err(Error::limit_exceeded(format!(
                "transaction has {} ops, above the {}-op bound",
                tx.ops.len(),
                options.max_ops
            )));
        }

        // Outcome lookup is the first thing a retry does. It is also what a
        // caller must do after an ambiguous timeout.
        match self.transaction_outcome(tx.id.as_str())? {
            crate::mw2::OutcomeLookup::Committed(outcome) => {
                let digest = tx.digest()?;
                if let Some(record) = self.read_tx_record(tx.id.as_str())? {
                    if record.digest != digest {
                        return Err(Error::new(
                            Class::Conflict,
                            "transaction id was reused with a different op set",
                        ));
                    }
                }
                return Ok(outcome);
            }
            crate::mw2::OutcomeLookup::Unknown => {
                // A previous attempt may have published and lost its
                // acknowledgement. Find it by the canonical op digest before
                // re-applying anything; this is what makes an ambiguous retry
                // safe for delete ops as well as creates.
                if let Some(record) = self.read_tx_record(tx.id.as_str())? {
                    if let Some((commit, generation)) =
                        self.find_tx_commit(tx, record.parent_generation, options.outcome_window)?
                    {
                        self.write_tx_record(
                            tx,
                            crate::mw2::TxRecordState::Committed,
                            Some(commit),
                        )?;
                        return Ok(crate::mw2::TransactionOutcome {
                            id: tx.id.as_str().to_owned(),
                            commit,
                            generation,
                            attempts: 0,
                            idempotent: true,
                        });
                    }
                }
            }
            crate::mw2::OutcomeLookup::Absent => {}
        }

        self.write_tx_record(tx, crate::mw2::TxRecordState::Prepared, None)?;
        crash_point("mw2_prepared");

        // Deterministic jitter seed: the transaction's own op digest. Two
        // writers that collided once therefore do not retry in lockstep, and a
        // receipt can reproduce the same schedule from the receipt's ids.
        let jitter_seed = {
            let digest = tx.digest()?;
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&digest.as_bytes()[..8]);
            u64::from_le_bytes(bytes)
        };
        let touched = crate::mw2::touched_keys(&tx.ops);
        let mut attempts: u32 = 0;
        let mut max_consecutive: u32 = 0;
        loop {
            attempts = attempts.saturating_add(1);
            max_consecutive = max_consecutive.max(attempts.saturating_sub(1));
            // Apply only the commits this handle has not seen, instead of
            // replaying the whole chain. A divergence or a damaged commit
            // falls back to the authoritative full reload. The parsed gap is
            // kept in a bounded cache, so a second attempt does not re-read
            // commits it already validated.
            let replayed = self.refresh_to_head()?;
            if !replayed.is_empty() && !touched.is_empty() {
                let mut intersects = false;
                {
                    let cache = self
                        .commit_delta_cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    for generation in &replayed {
                        if let Some(commit) = cache.get(*generation) {
                            if !crate::mw2::changed_keys(&commit.commit.ops).is_disjoint(&touched) {
                                intersects = true;
                                break;
                            }
                        }
                    }
                }
                if intersects {
                    let mut stats = self
                        .rebase_stats
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    stats.touched_intersections = stats.touched_intersections.saturating_add(1);
                }
            }

            crate::mw2::validate_preconditions(&self.state, tx)?;
            for check in &tx.auth {
                self.authorize_read_in(&self.state, check.object, &check.principal, self.now_ns())?;
            }

            let generation = self
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?
                .max(self.next_generation);
            let mut candidate = self.state.clone();
            candidate.apply_ops(&tx.ops)?;
            candidate.generation = generation;

            let pre = self.quota_pre_map_live(&tx.ops);
            let mut ops = tx.ops.clone();
            let quota = self.quota_preflight(&ops, &pre)?;
            if let Some(plan) = &quota {
                ops.push(Self::quota_usage_op(plan));
            }

            // `candidate` becomes the working state for publication, but the
            // last published state is retained. A lost race must be retried
            // from that base, never from a state that still contains this
            // attempt's own unpublished ops: quota charges, preconditions and
            // authorization would otherwise be evaluated against a state the
            // store never published.
            let base = std::mem::replace(&mut self.state, candidate);
            let pre_images = crate::index_incremental::PreImages::capture(&base, &ops);
            self.next_generation = generation
                .checked_add(1)
                .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
            match self.try_publish_staged(generation, ops, false, pre_images) {
                Ok(hash) => {
                    if let Some(plan) = quota {
                        self.record_quota_after_commit(plan);
                    }
                    self.write_tx_record(tx, crate::mw2::TxRecordState::Committed, Some(hash))?;
                    crash_point("mw2_after_ack");
                    self.record_rebase_outcome(attempts, max_consecutive);
                    return Ok(crate::mw2::TransactionOutcome {
                        id: tx.id.as_str().to_owned(),
                        commit: hash,
                        generation,
                        attempts,
                        idempotent: false,
                    });
                }
                Err(err) if err.class() == Class::Busy => {
                    // The head did not move to this attempt, so the published
                    // base is still the truth. Restore it before the retry.
                    self.state = base;
                    let forced =
                        options.fail_after_attempts > 0 && attempts >= options.fail_after_attempts;
                    if attempts >= options.max_attempts || forced {
                        self.reload_from_plane()?;
                        self.record_rebase_outcome(attempts, max_consecutive);
                        return Err(Error::busy(
                            format!(
                            "transaction {} exhausted its {attempts}-attempt budget; outcome is ",
                            tx.id.as_str()
                        ) + "unknown (resolve it by transaction id)",
                        ));
                    }
                    std::thread::sleep(options.backoff_with_jitter(attempts, jitter_seed));
                }
                Err(err) => {
                    self.reload_from_plane()?;
                    self.record_rebase_outcome(attempts, max_consecutive);
                    return Err(err);
                }
            }
        }
    }

    fn record_rebase_outcome(&self, attempts: u32, max_consecutive: u32) {
        let mut stats = self
            .rebase_stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.transactions = stats.transactions.saturating_add(1);
        stats.total_attempts = stats.total_attempts.saturating_add(u64::from(attempts));
        stats.max_consecutive_retries = stats
            .max_consecutive_retries
            .max(u64::from(max_consecutive));
    }

    /// Publish a transaction into an **explicitly named** domain.
    ///
    /// This is the entry point a grouped multi-domain request must use, and it
    /// is where the protocol refuses: the handle is bound to exactly one domain
    /// (`self.domain`), and a request naming a different one is `CrossDomain`.
    /// Nothing is re-routed silently.
    pub fn execute_transaction_in(
        &mut self,
        domain: Hash32,
        tx: &crate::mw2::Transaction,
        options: &crate::mw2::TransactionOptions,
    ) -> Result<crate::mw2::TransactionOutcome> {
        match self.domain {
            Some(bound) if bound == domain => self.execute_transaction(tx, options),
            Some(bound) => Err(Error::cross_domain(format!(
                "transaction targets domain {} but this handle is bound to domain {}; MW3 \
                 has no cross-domain commit",
                domain.to_hex(),
                bound.to_hex()
            ))),
            None => Err(Error::cross_domain(
                "a global (MW1/MW2) handle cannot publish into an MW3 domain; open with \
                 --writer-mode sharded --domain",
            )),
        }
    }

    /// Publish one grouped transaction across the named domains.
    ///
    /// **Refused for two or more domains** (`CrossDomain`): MW3 implements no
    /// two-phase prepare/commit and no cross-domain atomicity. A single named
    /// domain is accepted and goes through the ordinary domain path.
    pub fn execute_transaction_across(
        &mut self,
        domains: &[Hash32],
        tx: &crate::mw2::Transaction,
        options: &crate::mw2::TransactionOptions,
    ) -> Result<crate::mw2::TransactionOutcome> {
        match domains {
            [] => Err(Error::invalid_argument(
                "a grouped transaction must name at least one MW3 domain",
            )),
            [domain] => self.execute_transaction_in(*domain, tx, options),
            many => Err(Error::cross_domain(format!(
                "a grouped transaction names {} domains; MW3 has no cross-domain atomic \
                 publication and does not degrade to one domain",
                many.len()
            ))),
        }
    }

    /// Look a transaction id up without publishing.
    ///
    /// A `committed` record whose commit is still an ancestor of the selected
    /// head is returned. A `prepared` record whose op digest appears in a commit
    /// within the bounded window is repaired and returned. Otherwise the answer
    /// is `Unknown`: neither success nor failure.
    pub fn transaction_outcome(&self, id: &str) -> Result<crate::mw2::OutcomeLookup> {
        let Some(record) = self.read_tx_record(id)? else {
            return Ok(crate::mw2::OutcomeLookup::Absent);
        };
        if record.id != id {
            return Err(Error::corrupt(
                "transaction record id does not match its key",
            ));
        }
        if record.state == crate::mw2::TxRecordState::Committed {
            let commit = record.commit.ok_or_else(|| {
                Error::corrupt("a committed transaction record has no commit hash")
            })?;
            if self.commit_in_chain(&record, commit, crate::mw2::DEFAULT_OUTCOME_WINDOW)? {
                return Ok(crate::mw2::OutcomeLookup::Committed(
                    crate::mw2::TransactionOutcome {
                        id: id.to_owned(),
                        commit,
                        generation: record
                            .parent_generation
                            .checked_add(1)
                            .unwrap_or(self.generation),
                        attempts: 0,
                        idempotent: true,
                    },
                ));
            }
            return Ok(crate::mw2::OutcomeLookup::Unknown);
        }
        Ok(crate::mw2::OutcomeLookup::Unknown)
    }

    fn read_tx_record(&self, id: &str) -> Result<Option<crate::mw2::TxRecord>> {
        match self.with_backend(|backend| backend.read_all(&Key::tx(id))) {
            Ok(bytes) => Ok(Some(crate::mw2::decode_tx_record(&bytes)?)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn write_tx_record(
        &mut self,
        tx: &crate::mw2::Transaction,
        state: crate::mw2::TxRecordState,
        commit: Option<Hash32>,
    ) -> Result<()> {
        let digest = tx.digest()?;
        let existing = self.read_tx_record(tx.id.as_str())?;
        if let Some(record) = &existing {
            if record.digest != digest {
                return Err(Error::new(
                    Class::Conflict,
                    "transaction id was reused with a different op set",
                ));
            }
            if state == crate::mw2::TxRecordState::Prepared
                && record.state == crate::mw2::TxRecordState::Committed
            {
                return Ok(());
            }
        }
        // The parent generation belongs to the first attempt, not to a later
        // retry (a retry binds to a newer head). Outcome lookup and the
        // transaction digest are independent of it.
        let parent_generation = existing
            .as_ref()
            .map(|record| record.parent_generation)
            .unwrap_or(tx.base_generation);
        let record = crate::mw2::TxRecord {
            id: tx.id.as_str().to_owned(),
            state,
            digest,
            parent_generation,
            commit,
            updated_ns: self.now_ns(),
            pins: tx.pins.clone(),
        };
        let bytes = crate::mw2::encode_tx_record(&record)?;
        let key = Key::tx(tx.id.as_str());
        let result = match state {
            crate::mw2::TxRecordState::Prepared => {
                self.with_backend_mut(|backend| backend.put_atomic(&key, &bytes))
            }
            crate::mw2::TxRecordState::Committed => {
                self.with_backend_mut(|backend| backend.put_replace(&key, &bytes))
            }
        };
        match result {
            Ok(()) => Ok(()),
            // A concurrent writer created the same prepared record first. It
            // must carry the same digest or `execute_transaction` refuses.
            Err(err)
                if state == crate::mw2::TxRecordState::Prepared && err.class() == Class::Exists =>
            {
                match self.read_tx_record(tx.id.as_str())? {
                    Some(existing) if existing.digest == digest => Ok(()),
                    _ => Err(Error::new(
                        Class::Conflict,
                        "transaction id was reused with a different op set",
                    )),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Find a published commit whose op sequence (ignoring the trailing quota
    /// accounting op) equals this transaction's ops, within the bounded window.
    fn find_tx_commit(
        &self,
        tx: &crate::mw2::Transaction,
        parent_generation: u64,
        window: u64,
    ) -> Result<Option<(Hash32, u64)>> {
        if self.generation == 0 {
            return Ok(None);
        }
        let floor = parent_generation
            .saturating_add(1)
            .max(self.generation.saturating_sub(window));
        let mut generation = self.generation;
        while generation >= floor {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(
                    backend,
                    &self.commit_key(generation),
                    Some(generation),
                )
            })?;
            let mut ops = loaded.commit.ops.as_slice();
            // The publisher appends the quota accounting op; it is not part of
            // the caller's op set.
            if matches!(ops.last(), Some(Op::SetQuotaUsage(_))) {
                ops = &ops[..ops.len() - 1];
            }
            if crate::mw2::same_ops(ops, &tx.ops) {
                return Ok(Some((loaded.hash, generation)));
            }
            if generation == 1 {
                break;
            }
            generation -= 1;
        }
        Ok(None)
    }

    fn commit_in_chain(
        &self,
        record: &crate::mw2::TxRecord,
        commit: Hash32,
        window: u64,
    ) -> Result<bool> {
        if self.generation == 0 {
            return Ok(false);
        }
        let target_generation = record
            .parent_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("transaction generation overflow"))?;
        if target_generation > self.generation {
            return Ok(false);
        }
        let floor = self.generation.saturating_sub(window);
        if target_generation < floor {
            return Ok(false);
        }
        let mut generation = self.generation;
        let mut hash = self.head;
        while generation >= target_generation {
            if generation == target_generation {
                return Ok(hash == commit);
            }
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(
                    backend,
                    &self.commit_key(generation),
                    Some(generation),
                )
            })?;
            match loaded.commit.parent {
                Some(parent) => hash = parent,
                None => return Ok(false),
            }
            if generation == 1 {
                break;
            }
            generation -= 1;
        }
        Ok(false)
    }

    /// Catch up to the published head by applying only the missing commits.
    ///
    /// A rebase after a lost race usually has one (or a few) winner commits to
    /// apply. Replaying the whole chain there is correct but O(chain length)
    /// per retry; this path is the same replay restricted to the gap. Any
    /// parent mismatch, missing commit, or op failure falls back to the
    /// authoritative full reload, so the fast path can never invent state.
    fn refresh_to_head(&mut self) -> Result<Vec<u64>> {
        let plane = self.load_head_from_plane()?;
        let Some((generation, head)) = plane else {
            self.reload_from_plane()?;
            return Ok(Vec::new());
        };
        if generation == self.generation && head == self.head {
            return Ok(Vec::new());
        }
        if generation < self.generation || (generation == self.generation && head != self.head) {
            self.reload_from_plane()?;
            return Ok(Vec::new());
        }
        let mut state = self.state.clone();
        let mut hash = self.head;
        let mut replayed = Vec::with_capacity((generation - self.generation) as usize);
        let mut read = 0u64;
        let mut cached = 0u64;
        for next in (self.generation + 1)..=generation {
            let (loaded, from_cache) = match self.load_commit_cached(next) {
                Ok(value) => value,
                Err(_) => {
                    self.reload_from_plane()?;
                    return Ok(Vec::new());
                }
            };
            if from_cache {
                cached += 1;
            } else {
                read += 1;
            }
            if loaded.commit.parent != Some(hash) {
                self.reload_from_plane()?;
                return Ok(Vec::new());
            }
            if state.apply_ops(&loaded.commit.ops).is_err() {
                self.reload_from_plane()?;
                return Ok(Vec::new());
            }
            state.generation = next;
            hash = loaded.hash;
            replayed.push(next);
        }
        if hash != head {
            self.reload_from_plane()?;
            return Ok(Vec::new());
        }
        self.state = state;
        self.head = hash;
        self.generation = generation;
        self.next_generation = self.next_generation.max(generation + 1);
        self.blob_index.clear();
        self.blob_index_ready = false;
        self.load_quota_record();
        {
            let mut stats = self
                .rebase_stats
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            stats.gap_commits_read = stats.gap_commits_read.saturating_add(read);
            stats.gap_commits_cached = stats.gap_commits_cached.saturating_add(cached);
        }
        Ok(replayed)
    }

    /// Re-select the published head and replay its bounded commit chain.
    fn reload_from_plane(&mut self) -> Result<()> {
        if self.domain.is_some() {
            return self.recover_domain();
        }
        let recovery = self.with_backend(|backend| recovery::recover_for_write_via(backend))?;
        self.state = recovery.state;
        self.head = recovery.head;
        self.generation = recovery.generation;
        self.next_generation = recovery
            .generation
            .max(recovery.highest_file_generation)
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        self.rejected = recovery.rejected;
        self.notes.extend(recovery.notes);
        self.blob_index.clear();
        self.blob_index_ready = false;
        self.load_quota_record();
        Ok(())
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

    /// Verify one MW3 domain: the per-domain head image, the commit chain, each
    /// object's manifest, and every chunk blob. `full` re-reads every chunk and
    /// recomputes the whole-file content hash for raw v1 manifests. A
    /// non-raw representation is counted and reported as not decoded by this
    /// prototype verifier (the global verifier remains the authority for it).
    pub fn verify_domain(&self, full: bool) -> Result<VerifyReport> {
        let Some(domain) = self.domain else {
            return Err(Error::invalid_argument(
                "verify_domain requires a handle opened with --domain",
            ));
        };
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::Verify);
        let mut report = VerifyReport {
            full,
            generation: self.generation,
            commit: Some(self.head),
            ..VerifyReport::default()
        };
        let head = self.with_backend(|backend| backend.load_domain_head(&domain))?;
        let Some((generation, head_id)) = head else {
            report.problems.push(VerifyProblem {
                class: "Corrupt".to_owned(),
                detail: format!("domain {} has no valid head slot", domain.to_hex()),
            });
            return Ok(report);
        };
        report.generation = generation;
        report.commit = Some(head_id);
        let mut state = State::default();
        let mut hash = Hash32::ZERO;
        for candidate in 1..=generation {
            let loaded = match self.with_backend(|backend| {
                recovery::load_commit_key_via(backend, &self.commit_key(candidate), Some(candidate))
            }) {
                Ok(loaded) => loaded,
                Err(err) => {
                    report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: err.message().to_owned(),
                    });
                    break;
                }
            };
            let expected_parent = if hash == Hash32::ZERO {
                None
            } else {
                Some(hash)
            };
            if loaded.commit.parent != expected_parent {
                report.problems.push(VerifyProblem {
                    class: "Corrupt".to_owned(),
                    detail: format!(
                        "domain {} commit {candidate} does not name the previous commit as \
                         parent",
                        domain.to_hex()
                    ),
                });
                break;
            }
            if let Err(err) = state.apply_ops(&loaded.commit.ops) {
                report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("domain commit {candidate}: {err}"),
                });
                break;
            }
            if let Some(checkpoint) = loaded.commit.checkpoint {
                let _ = checkpoint;
            }
            state.generation = candidate;
            hash = loaded.hash;
            report.checked_commits += 1;
        }
        if hash != head_id {
            report.problems.push(VerifyProblem {
                class: "Corrupt".to_owned(),
                detail: format!(
                    "domain {} head {} does not match the replayed chain ({})",
                    domain.to_hex(),
                    head_id.to_hex(),
                    hash.to_hex()
                ),
            });
        }
        for record in state.objects.values() {
            if record.kind != crate::state::ObjectKind::File || record.content == Hash32::ZERO {
                continue;
            }
            report.checked_objects += 1;
            let manifest = match self
                .with_backend(|backend| recovery::read_manifest_via(backend, record.content))
            {
                Ok(manifest) => manifest,
                Err(err) => {
                    report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!(
                            "domain object {} manifest {}: {err}",
                            record.id.to_hex(),
                            record.content.to_hex()
                        ),
                    });
                    continue;
                }
            };
            report.checked_manifests += 1;
            let mut whole = Vec::new();
            for chunk in &manifest.chunks {
                match self.with_backend(|backend| backend.read_all(&Key::object(chunk.hash))) {
                    Ok(bytes) => {
                        if bytes.len() as u64 != chunk.len {
                            report.problems.push(VerifyProblem {
                                class: "Corrupt".to_owned(),
                                detail: format!(
                                    "domain chunk {} is {} bytes, manifest says {}",
                                    chunk.hash.to_hex(),
                                    bytes.len(),
                                    chunk.len
                                ),
                            });
                        }
                        if full {
                            whole.extend_from_slice(&bytes);
                        }
                        report.checked_chunks += 1;
                    }
                    Err(err) => report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!("domain chunk {}: {err}", chunk.hash.to_hex()),
                    }),
                }
            }
            if full && manifest.representation.is_none() && whole.len() as u64 == manifest.size {
                if identity::content_hash(&whole) != manifest.content_hash {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: format!(
                            "domain object {} whole-file content hash mismatch",
                            record.id.to_hex()
                        ),
                    });
                }
            }
        }
        report.ok = report.problems.is_empty();
        Ok(report)
    }

    /// Verify the superblock, commit chain, manifests, and (optionally) every
    /// chunk and whole-file content hash.
    pub fn verify(&self, full: bool) -> Result<VerifyReport> {
        if self.domain.is_some() {
            return self.verify_domain(full);
        }
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::Verify);
        let mut report = VerifyReport {
            full,
            generation: self.generation,
            commit: Some(self.head),
            ..VerifyReport::default()
        };

        let view = self.with_backend(recovery::read_superblock_via)?;
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

        let files = self.with_backend(recovery::scan_commit_keys_via)?;
        for (generation, key) in files {
            match self.with_backend(|backend| {
                recovery::load_commit_key_via(backend, &key, Some(generation))
            }) {
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
                    detail: format!("commit file {}: {err}", key.display_path()),
                }),
            }
        }

        match self.with_backend(recovery::recover_via) {
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
                match self.with_backend(|backend| {
                    recovery::load_state_at_via(backend, recovered.head, max_visible, true)
                }) {
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

        // Verify every checkpoint manifest named by a parsed commit. Only the
        // checkpoint rows are retained; an aged store's full history is not
        // materialised (it was the peak allocation of this pass at 1M commits).
        let checkpoint_commits =
            self.with_backend(|backend| recovery::checkpoint_commits_via(backend, max_visible))?;
        for (generation, _commit, checkpoint) in checkpoint_commits {
            report.checked_manifests += 1;
            match self.read_state_manifest_keyed(checkpoint) {
                Ok(state) => {
                    if state.generation != generation {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: format!(
                                "checkpoint {checkpoint} declares generation {} but is \
                                 named by generation {generation}",
                                state.generation
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

        // Blob-plane metadata of its own (the pack catalog). A plane without
        // packs reports nothing.
        match self.with_backend(|backend| backend.verify_blob_plane()) {
            Ok(problems) => {
                for problem in problems {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: problem,
                    });
                }
            }
            Err(err) => report.problems.push(VerifyProblem {
                class: err.class_name().to_owned(),
                detail: format!("blob plane metadata: {err}"),
            }),
        }

        // Snapshot pointer files are caches; report mismatches, not absence.
        for (name, hash) in &self.state.snapshots {
            let key = Key::snapshot_ref(name.clone());
            if let Ok(bytes) = self.with_backend(|backend| backend.read_all(&key)) {
                let text = String::from_utf8_lossy(&bytes);
                let text = text.trim();
                if Hash32::from_hex(text).ok() != Some(*hash) {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: format!("snapshot pointer `{name}` does not match state"),
                    });
                }
            }
        }

        // A present-but-unusable quota record is reported: enforcement fails
        // closed, so verification must not claim the store is fully healthy.
        if self.quota.record_present && !self.quota.valid {
            report.problems.push(VerifyProblem {
                class: "Corrupt".to_owned(),
                detail: format!(
                    "quota state is unusable: {}",
                    self.quota
                        .reason
                        .clone()
                        .unwrap_or_else(|| "no reason recorded".to_owned())
                ),
            });
        }

        // Full verification replays the chain and validates every committed
        // usage assertion against the recomputed admission totals.
        if full && (self.quota.enforced() || self.quota.usage_committed) {
            match self.replay_quota_usage_with_count() {
                Ok((usage, _, commits)) => {
                    report.checked_quota_commits = commits;
                    if usage != self.quota.usage {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: "committed quota usage disagrees with in-memory counters"
                                .to_owned(),
                        });
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("quota replay: {err}"),
                }),
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

        // Authenticated envelopes: the relation must name an EnvelopeId and a
        // blob whose content address is valid, canonical, and correctly signed
        // by the declared grantor. Expiry and revocation are caller policy and
        // are not rejected by `verify`.
        let stored_envelopes = match self.list_capability_envelopes_at(state) {
            Ok(items) => items,
            Err(err) => {
                report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("capability envelopes: {err}"),
                });
                Vec::new()
            }
        };
        for stored in stored_envelopes {
            report.checked_capability_envelopes += 1;
            let bytes = match self.read_blob_verified_keyed(
                BlobKind::Manifest,
                stored.blob_id,
                weftcodec::capability::DOMAIN_BLOB,
            ) {
                Ok(bytes) => bytes,
                Err(err) => {
                    report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!("capability envelope {}: {err}", stored.envelope_id),
                    });
                    continue;
                }
            };
            match weftcodec::capability::verify_envelope(&bytes, &[], i64::MIN, None) {
                Ok(verified) if verified.envelope_id == stored.envelope_id => {}
                Ok(_) => report.problems.push(VerifyProblem {
                    class: "Corrupt".to_owned(),
                    detail: format!(
                        "capability envelope {} does not match its stored identity",
                        stored.envelope_id
                    ),
                }),
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class().as_str().to_owned(),
                    detail: format!("capability envelope {}: {err}", stored.envelope_id),
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
                    match self.read_blob_verified_keyed(
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
            match self.read_manifest_keyed(record.content) {
                Ok(manifest) => {
                    report.checked_manifests += 1;
                    if manifest.size != record.size {
                        report.problems.push(VerifyProblem {
                            class: "Corrupt".to_owned(),
                            detail: format!("object {} size disagrees with manifest", record.id),
                        });
                    }
                    let representation_kind = manifest.representation.as_ref().map(|rep| rep.kind);
                    if matches!(
                        representation_kind,
                        Some(RepresentationKind::Codec)
                            | Some(RepresentationKind::Tensor)
                            | Some(RepresentationKind::Overlay)
                    ) {
                        if representation_kind == Some(RepresentationKind::Overlay) {
                            // Even a shallow verify resolves the immutable base
                            // chain and the extent grid; a full verify also
                            // re-reads every logical chunk and checks the root.
                            if let Err(err) = self.overlay_chunk_table(record.content, &manifest) {
                                report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!(
                                        "overlay representation for object {} manifest {}: {err}",
                                        record.id, record.content
                                    ),
                                });
                                continue;
                            }
                        }
                        if full {
                            let frame = match manifest.representation.as_ref().and_then(|r| r.frame)
                            {
                                Some(hash) => hash.to_hex(),
                                None => "<none>".to_owned(),
                            };
                            let kind = manifest
                                .representation
                                .as_ref()
                                .map(|rep| rep.kind.as_str())
                                .unwrap_or("none");
                            let mut seen = vec![record.content];
                            match self.read_manifest_bytes_inner(
                                &manifest,
                                0,
                                manifest.size,
                                true,
                                0,
                                &mut seen,
                            ) {
                                Ok(_) => {
                                    report.checked_representations += 1;
                                    if representation_kind == Some(RepresentationKind::Overlay) {
                                        if let Ok(table) =
                                            self.overlay_chunk_table(record.content, &manifest)
                                        {
                                            report.checked_chunks +=
                                                table.len() as u64;
                                        }
                                    }
                                }
                                Err(err) => report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!(
                                        "{kind} representation for object {} manifest {} frame {}: {err}",
                                        record.id, record.content, frame
                                    ),
                                }),
                            }
                        }
                        continue;
                    }
                    if full {
                        // Stream chunks through the identity hasher instead of
                        // assembling the whole file. Verification must stay
                        // bounded for objects near MAX_FILE_BYTES. A Merkle v3
                        // manifest folds its stored chunk hashes; a v1/v2
                        // manifest feeds the frozen whole-content digest.
                        let merkle = manifest.identity == ContentIdentity::MerkleV3;
                        let mut hasher = identity::content_hash_hasher();
                        let mut leaves: Vec<(u64, Hash32)> = Vec::new();
                        let mut total: u64 = 0;
                        for (index, chunk) in manifest.chunks.iter().enumerate() {
                            report.checked_chunks += 1;
                            match self.read_blob_verified_keyed(
                                BlobKind::Object,
                                chunk.hash,
                                identity::CHUNK_DOMAIN,
                            ) {
                                Ok(bytes) => {
                                    if bytes.len() as u64 != chunk.len {
                                        report.problems.push(VerifyProblem {
                                            class: "Corrupt".to_owned(),
                                            detail: format!(
                                                "chunk {index} ({}) length disagrees with manifest",
                                                chunk.hash
                                            ),
                                        });
                                    }
                                    total = total.saturating_add(bytes.len() as u64);
                                    if merkle {
                                        leaves.push((chunk.len, chunk.hash));
                                    } else {
                                        hasher.update(&bytes);
                                    }
                                }
                                Err(err) => report.problems.push(VerifyProblem {
                                    class: err.class_name().to_owned(),
                                    detail: format!("chunk {index} ({}): {err}", chunk.hash),
                                }),
                            }
                        }
                        if total == manifest.size {
                            if merkle {
                                if identity::merkle_root_from_chunks(leaves)
                                    != manifest.content_hash
                                {
                                    report.problems.push(VerifyProblem {
                                        class: "Corrupt".to_owned(),
                                        detail: format!(
                                            "object {} merkle content root mismatch",
                                            record.id
                                        ),
                                    });
                                }
                            } else if identity::finish_content_hash(hasher) != manifest.content_hash
                            {
                                report.problems.push(VerifyProblem {
                                    class: "Corrupt".to_owned(),
                                    detail: format!(
                                        "object {} whole-content hash mismatch",
                                        record.id
                                    ),
                                });
                            }
                        }
                    }
                }
                Err(err) => report.problems.push(VerifyProblem {
                    class: err.class_name().to_owned(),
                    detail: format!("manifest for object {}: {err}", record.id),
                }),
            }
        }

        // Provenance receipts: the relation target must parse to a lowercase
        // 32-byte identity, the blob must hash-verify and re-encode canonically,
        // and the receipt must still declare the subject object's content root.
        let mut provenance_cache: BTreeMap<Hash32, crate::provenance::ProvenanceRecord> =
            BTreeMap::new();
        for relation in state.relations.values() {
            if relation.predicate != crate::provenance::PROVENANCE_PREDICATE {
                continue;
            }
            report.checked_provenance += 1;
            let id = match crate::provenance::provenance_id_from_relation(relation) {
                Ok(id) => id,
                Err(err) => {
                    report.problems.push(VerifyProblem {
                        class: err.class_name().to_owned(),
                        detail: format!("provenance relation {}: {err}", relation.id),
                    });
                    continue;
                }
            };
            let subject = match state.objects.get(&relation.subject) {
                Some(record) => record,
                None => {
                    report.problems.push(VerifyProblem {
                        class: "Corrupt".to_owned(),
                        detail: format!(
                            "provenance relation {} names missing subject object {}",
                            relation.id, relation.subject
                        ),
                    });
                    continue;
                }
            };
            if !provenance_cache.contains_key(&id) {
                match self.read_provenance(id) {
                    Ok(record) => {
                        provenance_cache.insert(id, record);
                    }
                    Err(err) => {
                        report.problems.push(VerifyProblem {
                            class: err.class_name().to_owned(),
                            detail: format!("provenance {id}: {err}"),
                        });
                        continue;
                    }
                }
            }
            let record = &provenance_cache[&id];
            if !record.outputs.contains(&subject.content) {
                report.problems.push(VerifyProblem {
                    class: "Corrupt".to_owned(),
                    detail: format!(
                        "provenance {id} does not declare the content root {} of object {}",
                        subject.content, subject.id
                    ),
                });
            }
        }
    }

    // ---- T024: multi-writer GC epochs and reservations -------------------

    /// The current multi-writer GC epoch, or `None` when no pass has ever
    /// acquired one (epoch 0).
    pub fn gc_epoch(&self) -> Result<Option<crate::gc_epoch::GcEpoch>> {
        match self.with_backend(|backend| backend.read_all(&Key::gc_epoch())) {
            Ok(bytes) => Ok(Some(crate::gc_epoch::decode_epoch(&bytes)?)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Every publication reservation currently recorded, decoded and bounded.
    pub fn gc_reservations(&self) -> Result<Vec<crate::gc_epoch::Reservation>> {
        let keys =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Reservation)))?;
        if keys.len() > crate::gc_epoch::MAX_RESERVATIONS {
            return Err(Error::limit_exceeded(format!(
                "{} publication reservations exceed the {}-record bound",
                keys.len(),
                crate::gc_epoch::MAX_RESERVATIONS
            )));
        }
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let bytes = self.with_backend(|backend| backend.read_all(&key))?;
            out.push(crate::gc_epoch::decode_reservation(&bytes)?);
        }
        Ok(out)
    }

    /// Read one completed GC run record (an audit record, never a root).
    pub fn gc_run_record(&self, run_id: &str) -> Result<Option<crate::gc_epoch::GcRun>> {
        match self.with_backend(|backend| backend.read_all(&Key::gc_run(run_id))) {
            Ok(bytes) => Ok(Some(crate::gc_epoch::decode_run(&bytes)?)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Register a publication reservation **before** publishing any blob that no
    /// commit names yet (T024).
    ///
    /// The record is create-once under a key derived from the transaction id and
    /// the reservation instant, so a retry never races a previous attempt and
    /// never leaves a gap in which no record protects the writer's blobs. The
    /// returned guard removes it on drop; callers may also release it early.
    ///
    /// Only an optimistic (MW2) or sharded (MW3) writable handle may take one.
    /// A single-writer store needs no reservation: its exclusive writer is the
    /// only publisher, and GC is an administration step on the same handle.
    pub fn reserve_publication(&self, tx_id: &str) -> Result<PublicationReservation> {
        self.require_writable()?;
        if !matches!(
            self.options.writer_mode,
            crate::mw2::WriterMode::Optimistic | crate::mw2::WriterMode::Sharded
        ) {
            return Err(Error::invalid_argument(
                "publication reservations are only meaningful for --writer-mode optimistic or \
                 sharded; a single-writer store has one exclusive publisher",
            ));
        }
        if tx_id.is_empty() || tx_id.len() > 128 {
            return Err(Error::invalid_argument(
                "a reservation transaction id must be 1..=128 bytes",
            ));
        }
        let now_ns = self.wall_ns();
        let epoch = self.gc_epoch()?.map(|record| record.epoch).unwrap_or(0);
        let deadline_ns = now_ns.saturating_add(
            i64::try_from(
                self.options
                    .gc_reservation_secs
                    .saturating_mul(1_000_000_000),
            )
            .unwrap_or(i64::MAX),
        );
        let record = crate::gc_epoch::Reservation {
            tx_id: tx_id.to_owned(),
            epoch,
            started_ns: now_ns,
            deadline_ns,
            owner: format!("pid:{}", std::process::id()),
        };
        crate::gc_epoch::validate_reservation_deadline(record.started_ns, record.deadline_ns)?;
        let bytes = crate::gc_epoch::encode_reservation(&record)?;
        // The file name is a digest so an arbitrary transaction id can never
        // escape the reservation namespace.
        let key_id = identity::h(
            "weft/gc/reservation-key/v1",
            format!("{tx_id}:{now_ns}").as_bytes(),
        )
        .to_hex();
        match self
            .with_backend_mut(|backend| backend.put_atomic(&Key::reservation(&key_id), &bytes))
        {
            Ok(()) => {}
            Err(err) if err.class() == Class::Exists => {
                // Identical bytes for the same instant: idempotent.
                let stored =
                    self.with_backend(|backend| backend.read_all(&Key::reservation(&key_id)))?;
                if stored != bytes {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
        Ok(PublicationReservation { key_id, record })
    }

    /// Remove one publication reservation. Idempotent: a record already gone is
    /// success. A failure is returned, because a caller that is about to publish
    /// should know its protection was not lifted (it is still safe, only
    /// over-retained).
    pub fn release_reservation(&self, reservation: &PublicationReservation) -> Result<()> {
        self.release_publication_reservation(reservation.key_id())
    }

    fn release_publication_reservation(&self, key_id: &str) -> Result<()> {
        match self.with_backend_mut(|backend| backend.delete(&Key::reservation(key_id))) {
            Ok(()) => Ok(()),
            Err(err) if err.class() == Class::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Every domain id that has a head image in the store, sorted.
    ///
    /// `domains.root` is a discovery aid, so GC may not rely on it to decide
    /// which heads are authoritative: a head absent from the record is still a
    /// root (spec §15.3).
    pub fn domain_ids(&self) -> Result<Vec<Hash32>> {
        let keys = self
            .with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::DomainSuperblock)))?;
        let mut out = Vec::new();
        for key in keys {
            match key {
                Key::DomainSuperblock { domain } => out.push(domain),
                Key::Other { name, .. } => {
                    // `domains/<id>/` also holds `LOCK.publish` (the short
                    // publication lock) and `link` (lineage). Only an
                    // unreadable *head* name is an error.
                    if name.ends_with("/superblock") {
                        return Err(Error::corrupt(format!(
                            "unrecognized domain head entry {name}"
                        )));
                    }
                }
                _ => {}
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Replay one domain's published chain into a state, by the same rules as
    /// [`Store::recover_domain`] (bounded checkpoint scan, exact parent links,
    /// head equality). Used by the coordinated GC pass, which must mark every
    /// domain head without rebinding this handle.
    fn domain_state_for_gc(&self, domain: Hash32) -> Result<Option<State>> {
        let Some((generation, head_id)) =
            self.with_backend(|backend| backend.load_domain_head(&domain))?
        else {
            return Ok(None);
        };
        Ok(Some(self.domain_state_at(domain, generation, head_id)?))
    }

    /// Replay one domain's chain up to a specific published head.
    fn domain_state_at(&self, domain: Hash32, generation: u64, head_id: Hash32) -> Result<State> {
        const CHECKPOINT_SCAN: u64 = 256;
        let floor = generation.saturating_sub(CHECKPOINT_SCAN).max(1);
        let mut state = State::default();
        let mut hash = Hash32::ZERO;
        let mut next = 1u64;
        for candidate in (floor..=generation).rev() {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(
                    backend,
                    &Key::domain_commit(domain, candidate),
                    Some(candidate),
                )
            })?;
            let Some(checkpoint) = loaded.commit.checkpoint else {
                continue;
            };
            let bytes =
                self.with_backend(|backend| backend.read_all(&Key::manifest(checkpoint)))?;
            let value = crate::wcb_util::decode(&bytes)?;
            let decoded = crate::format::decode_state(&value)?;
            if decoded.generation != candidate {
                continue;
            }
            state = decoded;
            hash = loaded.hash;
            next = candidate + 1;
            break;
        }
        for candidate in next..=generation {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(
                    backend,
                    &Key::domain_commit(domain, candidate),
                    Some(candidate),
                )
            })?;
            let expected_parent = if hash == Hash32::ZERO {
                None
            } else {
                Some(hash)
            };
            if loaded.commit.parent != expected_parent {
                return Err(Error::corrupt(format!(
                    "domain {} commit {candidate} does not name the previous commit as parent",
                    domain.to_hex()
                )));
            }
            state.apply_ops(&loaded.commit.ops)?;
            state.generation = candidate;
            hash = loaded.hash;
        }
        if hash != head_id {
            return Err(Error::corrupt(format!(
                "domain {} head {} does not match the replayed chain ({})",
                domain.to_hex(),
                head_id.to_hex(),
                hash.to_hex()
            )));
        }
        Ok(state)
    }

    /// Find the generation of one commit hash in a domain's chain. Used to
    /// resolve a domain snapshot name to its commit for GC marking.
    fn domain_generation_of(&self, domain: Hash32, commit: Hash32) -> Result<Option<u64>> {
        let generations = self.with_backend(|backend| backend.list_domain_commits(&domain))?;
        for generation in generations {
            let loaded = self.with_backend(|backend| {
                recovery::load_commit_key_via(
                    backend,
                    &Key::domain_commit(domain, generation),
                    Some(generation),
                )
            })?;
            if loaded.hash == commit {
                return Ok(Some(generation));
            }
        }
        Ok(None)
    }

    /// Compute the reachable blob set and the root label that retained each
    /// blob, exactly as [`Store::gc`] does.
    ///
    /// Roots, in the order they are labelled: live prepared transactions
    /// (`transaction:<id>`), the current state (`head:<commit>`), snapshot
    /// commits (`snapshot:<commit>`), checkpoint state manifests
    /// (`checkpoint:<manifest>`), and content pins (`pin:<content>`). A corrupt
    /// transaction record fails the pass closed instead of hiding a pin.
    /// `expired_tx` receives transaction keys that must be removed on apply.
    pub(crate) fn compute_reachability(
        &self,
        now_ns: i64,
        expired_tx: &mut Vec<Key>,
        problems: &mut Vec<String>,
    ) -> Result<Reachability> {
        let mut reach = Reachability::default();
        let tx_grace_ns = self.options.gc_grace_secs.saturating_mul(1_000_000_000);
        let tx_keys =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Tx)))?;
        if tx_keys.len() > 4096 {
            return Err(Error::limit_exceeded(format!(
                "{} prepared transaction records exceed the 4096-record GC bound",
                tx_keys.len()
            )));
        }
        let mut seen_pin = BTreeSet::new();
        for key in tx_keys {
            let bytes = self.with_backend(|backend| backend.read_all(&key))?;
            let record = crate::mw2::decode_tx_record(&bytes)?;
            let age_ns =
                u64::try_from(now_ns.saturating_sub(record.updated_ns)).unwrap_or(u64::MAX);
            if record.state == crate::mw2::TxRecordState::Prepared && age_ns <= tx_grace_ns {
                let origin = format!("transaction:{}", record.id);
                for pin in &record.pins {
                    mark_manifest_tree(self, *pin, &mut reach, &origin, problems, 0, &mut seen_pin);
                }
            } else {
                expired_tx.push(key);
            }
        }

        mark_state(
            self,
            &self.state,
            &mut reach,
            &format!("head:{}", self.head.to_hex()),
            problems,
        );

        // MW3: every domain head is a root, whether or not `domains.root`
        // names it. Each domain's snapshot pointers and checkpoint manifests
        // are roots inside that domain exactly as they are globally.
        if self.options.writer_mode == crate::mw2::WriterMode::Sharded {
            for domain in self.domain_ids()? {
                let Some(state) = self.domain_state_for_gc(domain)? else {
                    continue;
                };
                let origin = format!("domain:{}", domain.to_hex());
                mark_state(self, &state, &mut reach, &origin, problems);
                reach.domain_heads = reach.domain_heads.saturating_add(1);

                let mut snapshot_commits: Vec<Hash32> = state.snapshots.values().copied().collect();
                snapshot_commits.sort_unstable();
                for commit in snapshot_commits {
                    match self.domain_generation_of(domain, commit)? {
                        Some(generation) => {
                            let at = self.domain_state_at(domain, generation, commit)?;
                            mark_state(
                                self,
                                &at,
                                &mut reach,
                                &format!("domain-snapshot:{}:{}", domain.to_hex(), commit.to_hex()),
                                problems,
                            );
                        }
                        None => problems.push(format!(
                            "domain {} snapshot {} is not in the domain chain",
                            domain.to_hex(),
                            commit.to_hex()
                        )),
                    }
                }

                let generations =
                    self.with_backend(|backend| backend.list_domain_commits(&domain))?;
                for generation in generations {
                    let loaded = self.with_backend(|backend| {
                        recovery::load_commit_key_via(
                            backend,
                            &Key::domain_commit(domain, generation),
                            Some(generation),
                        )
                    })?;
                    if let Some(checkpoint) = loaded.commit.checkpoint {
                        reach.mark_manifest(checkpoint, &origin);
                        match self.read_state_manifest_keyed(checkpoint) {
                            Ok(at) => mark_state(self, &at, &mut reach, &origin, problems),
                            Err(err) => problems.push(format!(
                                "domain {} checkpoint {checkpoint} is unreadable: {err}",
                                domain.to_hex()
                            )),
                        }
                    }
                }
            }
        }

        let mut snapshot_commits: Vec<Hash32> = self.state.snapshots.values().copied().collect();
        snapshot_commits.sort_unstable();
        for commit in snapshot_commits {
            match self.state_at(commit) {
                Ok(state) => mark_state(
                    self,
                    &state,
                    &mut reach,
                    &format!("snapshot:{}", commit.to_hex()),
                    problems,
                ),
                Err(err) => problems.push(format!("snapshot {commit} state is unreadable: {err}")),
            }
        }

        // Checkpoints are roots even for rejected/unpublished generations:
        // they are immutable evidence and are never swept by inference.
        let checkpoint_commits =
            self.with_backend(|backend| recovery::checkpoint_commits_via(backend, u64::MAX))?;
        for (_generation, _commit, checkpoint) in checkpoint_commits {
            let origin = format!("checkpoint:{}", checkpoint.to_hex());
            reach.mark_manifest(checkpoint, &origin);
            match self.read_state_manifest_keyed(checkpoint) {
                Ok(state) => mark_state(self, &state, &mut reach, &origin, problems),
                Err(err) => problems.push(format!("checkpoint {checkpoint} is unreadable: {err}")),
            }
        }

        // Content pins held by open read handles are roots even if the object
        // record has already been removed from state.
        let mut pinned_contents: Vec<Hash32> = self.content_pins.keys().copied().collect();
        pinned_contents.sort_unstable();
        for content in pinned_contents {
            let mut seen = BTreeSet::new();
            mark_manifest_tree(
                self,
                content,
                &mut reach,
                &format!("pin:{}", content.to_hex()),
                problems,
                0,
                &mut seen,
            );
        }
        Ok(reach)
    }

    /// Mark-and-sweep GC over unreachable chunks and manifests.
    ///
    /// Roots: the published head (refreshed first on an optimistic handle),
    /// every snapshot commit state, every checkpoint state manifest, every
    /// live prepared transaction's pins, every live publication reservation,
    /// and — on a sharded store — every domain head's state.
    ///
    /// A candidate is deleted when `apply` is true, it is older than
    /// `gc_grace_secs` (zero disables the age gate), it is older than the
    /// instant this pass acquired its GC epoch, and it is older than every live
    /// reservation taken at or before that epoch. The epoch compare-and-swap is
    /// the pass's linearization point; see `spec/WEFT-MULTIWRITER.md` §11 and
    /// `docs/MULTI-WRITER-GC.md`.
    pub fn gc(&mut self, apply: bool) -> Result<GcReport> {
        self.require_writable()?;
        if self.group_depth != 0 {
            // GC physically deletes blobs; a later group rollback would leave
            // the restored state pointing at deleted content.
            return Err(Error::invalid_argument(
                "gc cannot run inside a group commit",
            ));
        }
        let sharded = self.options.writer_mode == crate::mw2::WriterMode::Sharded;
        if self.domain.is_some() {
            return Err(Error::unsupported_version(
                "a domain-bound handle cannot run a coordinated multi-domain GC; open a global \
                 handle with --writer-mode sharded and no --domain (spec/WEFT-MULTIWRITER.md \
                 15.9)",
            ));
        }
        let mut report = GcReport {
            applied: apply,
            ..GcReport::default()
        };

        // A concurrent publisher makes the age gate insufficient on a plane
        // that cannot report a modification time: the cut and the reservation
        // floor are both expressed in that clock. Refuse rather than guess.
        if sharded || self.options.writer_mode == crate::mw2::WriterMode::Optimistic {
            let can_report_age = self
                .with_backend(|backend| backend.modified(&Key::format()))?
                .is_some();
            if !can_report_age {
                return Err(Error::unsupported_version(
                    "this backend cannot report a blob modification time, so a multi-writer GC \
                     pass cannot prove a blob is older than its epoch cut; refusing rather than \
                     reclaim data a concurrent publisher may still name",
                ));
            }
        }

        // Acquire the epoch. A dry run observes the current epoch without
        // acquiring one (it deletes nothing, so it protects nothing).
        let acquired = if apply {
            self.acquire_gc_epoch()?
        } else {
            self.gc_epoch()?.unwrap_or(crate::gc_epoch::GcEpoch {
                epoch: 0,
                run_id: "-".to_owned(),
                started_ns: self.wall_ns(),
                finished_ns: self.wall_ns(),
                owner: "dry-run".to_owned(),
            })
        };
        report.gc_epoch = acquired.epoch;
        report.gc_run_id = acquired.run_id.clone();
        let acquired_ns = acquired.started_ns;

        // Reservations taken at or before the acquired epoch are this pass's
        // protection interval. Expired ones are abandoned and removed on apply.
        //
        // Order matters and is normative:
        //   epoch (cut) -> reservations (floor) -> head (roots) -> blobs.
        // A writer releases its reservation only after its commit is durable, so
        //   * a reservation read here is still live and its floor protects the
        //     writer's blobs;
        //   * a reservation already released means the commit landed before this
        //     read, so the head read immediately below names it;
        //   * a reservation created later means its blobs are newer than the cut.
        let reservations = self.gc_reservations()?;
        let view =
            crate::gc_epoch::ReservationView::build(reservations, acquired.epoch, self.wall_ns());
        report.reservations_live = view.live.len() as u64;
        report.reservation_floor_ns = if view.floor_ns == crate::gc_epoch::ReservationView::NO_FLOOR
        {
            None
        } else {
            Some(view.floor_ns)
        };
        // The same clock slack the floor uses. A blob published just after the
        // acquisition can carry an mtime truncated below it; treating the cut
        // as `acquired - slack` retains that blob instead of reclaiming it.
        let cut = CollectUnmarkedCut {
            acquired_ns: acquired_ns
                .saturating_sub(crate::gc_epoch::ReservationView::CLOCK_SLACK_NS),
            floor_ns: view.floor_ns,
        };

        // A pass must observe the published head **after** it has read the
        // reservations: reading the head first would let a commit that landed
        // in between be neither in the root set nor behind a reservation. An
        // optimistic handle's in-memory state may lag a winner's commit; a
        // refresh failure fails the pass closed.
        if self.options.writer_mode == crate::mw2::WriterMode::Optimistic {
            self.refresh_to_head()?;
        }

        // A pinned unlinked object is a normal open-handle retention. If the
        // process crashed with such a handle, an unreferenced object record can
        // remain in the commit chain; `gc --apply` removes those records in a
        // commit so the blobs become unreachable. Pinned and relation-targeted
        // objects are never removed here. A sharded global handle has no global
        // head to publish that removal to, so it is skipped and reported.
        if apply && !sharded {
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
                // Relations whose subject is a removed object are dangling;
                // delete them in the same commit so an orphan's provenance
                // receipt and inputs can become unreachable.
                let mut ops: Vec<Op> = Vec::new();
                for id in &orphan_ids {
                    ops.extend(self.delete_object_ops(*id));
                }
                self.commit_ops(ops, false)?;
                report.removed_orphans = orphan_ids.iter().map(|id| id.to_hex()).collect();
            }
        } else if apply && sharded {
            report.problems.push(
                "sharded pass: unlinked-object-record removal is skipped (no global head to \
                 publish it to); domain states are marked instead"
                    .to_owned(),
            );
        }

        let mut expired_tx: Vec<Key> = Vec::new();
        let reach =
            self.compute_reachability(self.now_ns(), &mut expired_tx, &mut report.problems)?;

        report.marked_manifests = reach.manifests.len() as u64;
        report.marked_chunks = reach.chunks.len() as u64;
        report.domain_heads_marked = reach.domain_heads;

        let now = SystemTime::now();
        let grace = self.options.gc_grace_secs;
        let mut candidates: Vec<(Key, u64)> = Vec::new();
        candidates.extend(collect_unmarked(
            self,
            KeyKind::Manifest,
            &reach.manifests,
            now,
            grace,
            Some(&cut),
            &mut report,
        )?);
        candidates.extend(collect_unmarked(
            self,
            KeyKind::Object,
            &reach.chunks,
            now,
            grace,
            Some(&cut),
            &mut report,
        )?);
        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        for (key, len) in &candidates {
            let display = key.display_path();
            report.candidates.push(display.clone());
            if apply {
                report.deleted.push(display);
                report.deleted_bytes = report.deleted_bytes.saturating_add(*len);
            }
        }
        if apply && !candidates.is_empty() {
            // One batch so a packed plane republishes one catalog segment per
            // touched segment instead of once per deleted member.
            let keys: Vec<Key> = candidates.iter().map(|(key, _)| key.clone()).collect();
            self.with_backend_mut(|backend| backend.delete_many(&keys))?;
            // A pack is reclaimed only once no catalog entry names it; a pack
            // with one live member survives. Unreferenced packs (and catalog
            // segments left by a crash between segment and root publication)
            // are collected here.
            let reclaim = self.with_backend_mut(|backend| backend.reclaim_unreferenced_packs())?;
            report.reclaimed_packs = reclaim.deleted_packs;
            report.reclaimed_pack_bytes = reclaim.deleted_pack_bytes;
            report.reclaimed_catalog_segments = reclaim.deleted_catalog_segments;
        }

        // Remove expired transaction discovery records once the data sweep has
        // run. A committed record is only a discovery aid; a prepared record
        // past the grace is abandoned.
        if apply {
            for key in expired_tx {
                let _ = self.with_backend_mut(|backend| backend.delete(&key));
            }
            // Expired publication reservations are abandoned for the same
            // reason, and their records are removed so the floor cannot grow
            // without bound. Their blobs were already protected this pass by
            // the reservation floor, so nothing is deleted here that the floor
            // retained.
            let keys = self
                .with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Reservation)))?;
            for key in keys {
                let Ok(bytes) = self.with_backend(|backend| backend.read_all(&key)) else {
                    continue;
                };
                let Ok(record) = crate::gc_epoch::decode_reservation(&bytes) else {
                    continue;
                };
                if !record.is_live(self.wall_ns()) {
                    self.with_backend_mut(|backend| backend.delete(&key))?;
                    report.deleted_reservations = report.deleted_reservations.saturating_add(1);
                }
            }
        }

        // Advisory index segments are retained only for commits named by a
        // retained root: the current head, a snapshot, or a commit carrying a
        // checkpoint manifest. In-memory object pins are subsumed by the head.
        // Removing an index never removes data; a later query simply rebuilds.
        let mut retained_indexes: BTreeSet<Hash32> = BTreeSet::new();
        retained_indexes.insert(self.head);
        retained_indexes.extend(self.state.snapshots.values().copied());
        {
            let mut found = Vec::new();
            let mut checkpoints = 0u64;
            self.with_backend(|backend| {
                recovery::for_each_commit_via(backend, u64::MAX, |commit| {
                    if commit.commit.checkpoint.is_some() {
                        found.push(commit.hash);
                        checkpoints += 1;
                    }
                    Ok(())
                })
            })?;
            report.checkpoints_retained = checkpoints;
            retained_indexes.extend(found);
        }
        match self.expand_retained_index_chains(&mut retained_indexes) {
            Ok(()) => {
                let mut index_candidates =
                    collect_unreachable_indexes(self, &retained_indexes, now, grace, &mut report)?;
                index_candidates.sort_by(|a, b| a.0.cmp(&b.0));
                for (key, len) in &index_candidates {
                    let display = key.display_path();
                    report.candidates.push(display.clone());
                    if apply {
                        self.with_backend_mut(|backend| backend.delete(key))?;
                        report.deleted_indexes.push(display);
                        report.deleted_index_bytes =
                            report.deleted_index_bytes.saturating_add(*len);
                        self.bump_index_stat(|stats| stats.gc_deleted += 1);
                    }
                }
            }
            Err(err) => {
                // A chain that cannot be resolved is never guessed at: retain
                // every index segment rather than risk deleting an ancestor a
                // retained snapshot still needs.
                report.problems.push(format!(
                    "index GC skipped: cannot resolve a retained segment chain: {err}"
                ));
            }
        }
        // One whole-pass total so every unlinked byte is accounted exactly
        // once, while the data-only counter above stays comparable with a
        // walk of `objects/` + `manifests/`.
        report.deleted_total_bytes = report
            .deleted_bytes
            .saturating_add(report.deleted_index_bytes)
            .saturating_add(report.reclaimed_pack_bytes);

        // Record the pass and close its epoch. The run record is an audit
        // artifact: it is never a root and never repairs anything. A failure to
        // write it is reported, not turned into a data-safety failure.
        if apply {
            let run = crate::gc_epoch::GcRun {
                run_id: acquired.run_id.clone(),
                epoch: acquired.epoch,
                started_ns: acquired.started_ns,
                finished_ns: self.wall_ns(),
                applied: true,
                reservations_live: report.reservations_live,
                reservation_floor_ns: report
                    .reservation_floor_ns
                    .unwrap_or(crate::gc_epoch::ReservationView::NO_FLOOR),
                retained_newer_than_cut: report.retained_newer_than_cut,
                retained_reservation_floor: report.retained_reservation_floor,
                marked_manifests: report.marked_manifests,
                marked_chunks: report.marked_chunks,
                deleted_blobs: report.deleted.len() as u64,
                deleted_bytes: report.deleted_bytes,
                deleted_indexes: report.deleted_indexes.len() as u64,
                deleted_reservations: report.deleted_reservations,
                removed_orphans: report.removed_orphans.len() as u64,
                domain_heads_marked: report.domain_heads_marked,
            };
            match crate::gc_epoch::encode_run(&run).and_then(|bytes| {
                self.with_backend_mut(|backend| {
                    backend.put_atomic(&Key::gc_run(&run.run_id), &bytes)
                })
            }) {
                Ok(()) => {}
                Err(err) if err.class() == Class::Exists => {}
                Err(err) => report
                    .problems
                    .push(format!("cannot record the GC run: {err}")),
            }
            let finished = crate::gc_epoch::GcEpoch {
                finished_ns: self.wall_ns(),
                ..acquired
            };
            match crate::gc_epoch::encode_epoch(&finished).and_then(|bytes| {
                self.with_backend_mut(|backend| backend.put_replace(&Key::gc_epoch(), &bytes))
            }) {
                Ok(()) => {}
                Err(err) => report
                    .problems
                    .push(format!("cannot close the GC epoch: {err}")),
            }
        }
        Ok(report)
    }

    /// Acquire the next GC epoch with one compare-and-swap and return it.
    ///
    /// The CAS is the pass's linearization point: a writer that observes the
    /// old epoch and registers before this returns is covered by the reservation
    /// floor, and one that observes the new epoch registers after the acquisition
    /// instant. Bounded retries; a contended epoch returns `Busy` rather than
    /// running two passes under one epoch.
    fn acquire_gc_epoch(&self) -> Result<crate::gc_epoch::GcEpoch> {
        for attempt in 0..crate::gc_epoch::DEFAULT_EPOCH_ATTEMPTS {
            let raw = match self.with_backend(|backend| backend.read_all(&Key::gc_epoch())) {
                Ok(bytes) => Some(bytes),
                Err(err) if err.class() == Class::NotFound => None,
                Err(err) => return Err(err),
            };
            let current = match &raw {
                Some(bytes) => Some(crate::gc_epoch::decode_epoch(bytes)?),
                None => None,
            };
            let next = crate::gc_epoch::GcEpoch {
                epoch: current.as_ref().map(|record| record.epoch).unwrap_or(0) + 1,
                run_id: format!("{}-{}-{}", self.wall_ns(), std::process::id(), attempt),
                started_ns: self.wall_ns(),
                finished_ns: self.wall_ns(),
                owner: format!("pid:{}", std::process::id()),
            };
            let bytes = crate::gc_epoch::encode_epoch(&next)?;
            let token = raw.as_deref().map(crate::planes::control_token);
            // The default compare-and-swap is read-verify-write. On a POSIX
            // plane it is atomic only under the short publication lock, which
            // is exactly the critical section `begin_publication` holds (and
            // which a native conditional write does not need).
            let result = self.with_backend_mut(|backend| {
                backend.begin_publication()?;
                let out = backend.compare_and_swap(&Key::gc_epoch(), token.as_deref(), &bytes);
                let _ = backend.end_publication();
                out
            });
            match result {
                Ok(()) => return Ok(next),
                Err(err) if err.class() == Class::Busy => continue,
                Err(err) => return Err(err),
            }
        }
        Err(Error::busy(
            "another GC pass holds the epoch; retry after it closes",
        ))
    }

    // ---- write budgets (quotas) -----------------------------------------

    /// The configured budgets, sorted by canonical principal key.
    pub fn quota_policy(&self) -> Vec<QuotaBudget> {
        self.quota.policy.values().cloned().collect()
    }

    /// True when at least one budget is configured and enforced.
    pub fn quota_enforced(&self) -> bool {
        self.quota.enforced()
    }

    /// Cumulative admitted usage for one principal. Budgets of other
    /// principals are never returned by this method.
    pub fn quota_usage(&self, principal: &QuotaPrincipal) -> QuotaCounters {
        self.quota.usage.get(principal).copied().unwrap_or_default()
    }

    /// The budget for one principal, if configured.
    pub fn quota_budget(&self, principal: &QuotaPrincipal) -> Option<QuotaBudget> {
        self.quota.policy.get(principal).cloned()
    }

    /// Why the persisted record is not usable, if it is not. Writes fail
    /// closed while this is `Some` and a budget is configured.
    pub fn quota_problem(&self) -> Option<&str> {
        self.quota.reason.as_deref()
    }

    /// Total bytes of every content and manifest blob reachable from the
    /// current committed state through file objects (raw chunk tables, codec
    /// frames and their dependencies, tensor frames, and overlay extent
    /// chains).
    ///
    /// It uses the same walk quota accounting uses and performs manifest reads
    /// only. It does not include commit files, checkpoints, superblock slots,
    /// advisory index segments, or blobs retained solely by a snapshot. This is
    /// a diagnostic for benchmarks and operators, not a format feature.
    pub fn retained_blob_bytes(&self) -> Result<u64> {
        let mut index: BTreeMap<Hash32, u64> = BTreeMap::new();
        let mut seen: BTreeSet<Hash32> = BTreeSet::new();
        let manifests: Vec<Hash32> = self
            .state
            .objects
            .values()
            .filter(|record| record.kind == ObjectKind::File)
            .map(|record| record.content)
            .collect();
        for manifest in manifests {
            let mut additions = BTreeMap::new();
            self.collect_manifest_blobs(manifest, &index, &mut additions, &mut seen, 0)?;
            index.extend(additions);
        }
        Ok(index.values().copied().sum())
    }

    /// Install or replace the budget policy (admin operation). The policy is
    /// written once to the policy-only `quota` file; it is never rewritten per
    /// commit. Usage is committed state: when it is not already carried by the
    /// head commit's `set_quota_usage` op, it is recomputed from the published
    /// chain and published as one corrective op.
    pub fn install_quota_policy(&mut self, budgets: Vec<QuotaBudget>) -> Result<()> {
        self.require_writable()?;
        let budgets = normalize_policy(budgets)?;
        let mut policy = BTreeMap::new();
        for budget in budgets {
            policy.insert(budget.principal.clone(), budget);
        }
        self.quota.policy = policy;
        if !self.quota.valid || !self.quota.usage_committed {
            let (usage, blobs) = self.replay_quota_usage()?;
            self.quota.usage = usage;
            self.blob_index = blobs;
            self.blob_index_ready = true;
            self.quota.valid = true;
            self.quota.reason = None;
        }
        self.persist_quota_policy()?;
        self.quota.record_present = true;
        if !self.quota.usage_committed && !self.quota.usage.is_empty() {
            let usage = self.usage_vec();
            self.publish_quota_usage_op(
                QuotaUsageOp {
                    reset: false,
                    usage,
                },
                false,
            )?;
            self.quota.usage_committed = true;
        }
        self.quota.generation = self.generation;
        self.quota.commit = self.head;
        Ok(())
    }

    /// Zero every usage counter (explicit admin operation). Publishes exactly
    /// one `set_quota_usage` reset op; the policy is unchanged and no content
    /// is touched.
    pub fn quota_reset(&mut self) -> Result<()> {
        self.require_writable()?;
        self.publish_quota_usage_op(
            QuotaUsageOp {
                reset: true,
                usage: Vec::new(),
            },
            false,
        )?;
        self.quota.usage.clear();
        self.quota.usage_committed = true;
        self.quota.generation = self.generation;
        self.quota.commit = self.head;
        self.quota.valid = true;
        self.quota.reason = None;
        Ok(())
    }

    /// Recompute cumulative usage from the published commit chain, validate
    /// every committed usage assertion, and publish a corrective usage op.
    /// Returns the number of commits replayed. Data is never modified.
    pub fn quota_rebuild(&mut self) -> Result<u64> {
        self.require_writable()?;
        if self.quota.policy.is_empty() && self.quota.record_present {
            // A corrupt record hides the policy. Rebuilding with an empty policy
            // would silently disable enforcement, so this fails closed instead.
            return Err(Error::corrupt(
                "quota record is unusable and no policy is known; re-run with `--quota` to install one",
            ));
        }
        let (usage, blobs, commits) = self.replay_quota_usage_with_count()?;
        let usage_vec = usage
            .iter()
            .map(|(principal, counters)| (principal.clone(), *counters))
            .collect();
        self.publish_quota_usage_op(
            QuotaUsageOp {
                reset: false,
                usage: usage_vec,
            },
            false,
        )?;
        self.quota.usage = usage;
        self.blob_index = blobs;
        self.blob_index_ready = true;
        self.quota.usage_committed = true;
        self.quota.generation = self.generation;
        self.quota.commit = self.head;
        self.quota.valid = true;
        self.quota.reason = None;
        Ok(commits)
    }

    fn usage_vec(&self) -> Vec<(QuotaPrincipal, QuotaCounters)> {
        self.quota
            .usage
            .iter()
            .map(|(principal, counters)| (principal.clone(), *counters))
            .collect()
    }

    /// Publish exactly one commit containing one `set_quota_usage` op. Used by
    /// the explicit admin operations (`reset`, `rebuild`, policy install) and
    /// by migration; ordinary commits append their usage op inline.
    fn publish_quota_usage_op(
        &mut self,
        usage: QuotaUsageOp,
        force_checkpoint: bool,
    ) -> Result<Hash32> {
        self.require_writable()?;
        if self.group_depth != 0 {
            return Err(Error::invalid_argument(
                "a quota admin operation cannot run inside a group commit",
            ));
        }
        let ops = vec![Op::SetQuotaUsage(usage.normalize()?)];
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
        let pre = crate::index_incremental::PreImages::capture(&self.state, &ops);
        let mut commit = Commit {
            generation: new_generation,
            parent: Some(self.head),
            timestamp_ns: timestamp,
            ops,
            checkpoint,
            index_attestation: None,
        };
        let (attestation, built_index) =
            self.index_attestation_for(&next_state, &commit, &[], &pre)?;
        commit.index_attestation = attestation;
        let hash = self.publish_commit(&commit)?;
        self.state = next_state;
        self.head = hash;
        self.generation = new_generation;
        self.next_generation = new_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        if let Some(index) = built_index {
            self.persist_built_index(hash, index);
        }
        Ok(hash)
    }

    /// Test-only: publish an arbitrary usage assertion without enforcing
    /// admission. Used to prove that replay rejects a tampered assertion.
    #[doc(hidden)]
    pub fn publish_quota_usage_for_test(&mut self, usage: QuotaUsageOp) -> Result<Hash32> {
        self.publish_quota_usage_op(usage, false)
    }

    /// Read the committed usage carried by the head commit, when it has one.
    /// A reset op means the current usage is empty.
    fn head_usage_op(&self) -> Result<Option<QuotaUsageOp>> {
        let loaded = self.with_backend(|backend| {
            recovery::load_commit_key_via(
                backend,
                &self.commit_key(self.generation),
                Some(self.generation),
            )
        })?;
        Ok(loaded.commit.ops.iter().rev().find_map(|op| match op {
            Op::SetQuotaUsage(usage) => Some(usage.clone()),
            _ => None,
        }))
    }

    fn committed_usage_from_head(&self) -> Result<Option<BTreeMap<QuotaPrincipal, QuotaCounters>>> {
        let Some(op) = self.head_usage_op()? else {
            return Ok(None);
        };
        if op.reset {
            return Ok(Some(BTreeMap::new()));
        }
        Ok(Some(op.usage.into_iter().collect()))
    }

    fn read_store_record(&self, key: &Key) -> Result<Option<Vec<u8>>> {
        match self.with_backend(|backend| backend.read_all(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.class() == crate::error::Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Load the persisted policy and the committed usage.
    ///
    /// The `quota` file is policy-only (`WFQ2`). Usage is read from the head
    /// commit's `set_quota_usage` op; when the head has no such op the chain is
    /// replayed and every assertion is validated. A legacy combined `WFQ1`
    /// record is imported once on a writable open: its bytes are preserved as
    /// `quota.migrated` and the `quota` key is replaced by `WFQ2`. Any decode
    /// failure or disagreement leaves the runtime quota state invalid, and
    /// quota-enforcing writes then fail closed.
    fn load_quota_record(&mut self) {
        match self.load_quota_record_inner() {
            Ok(()) => {}
            Err(err) => {
                self.quota.record_present = true;
                self.quota.valid = false;
                self.quota.reason = Some(format!("quota state is unusable: {err}"));
            }
        }
    }

    fn load_quota_record_inner(&mut self) -> Result<()> {
        let current = self.read_store_record(&Key::quota())?;
        let migrated = self.read_store_record(&Key::quota_migrated())?;
        self.quota.migrated_present = migrated.is_some();

        let mut policy_budgets: Option<Vec<QuotaBudget>> = None;
        let mut legacy: Option<QuotaRecord> = None;
        let mut legacy_raw: Option<Vec<u8>> = None;
        let mut current_is_legacy = false;

        if let Some(bytes) = &current {
            self.quota.record_present = true;
            if bytes.starts_with(QUOTA_POLICY_MAGIC) {
                policy_budgets = Some(QuotaPolicyRecord::decode_bytes(bytes)?.budgets);
            } else if bytes.starts_with(QUOTA_MAGIC) {
                let record = QuotaRecord::decode_bytes(bytes)?;
                policy_budgets = Some(record.budgets.clone());
                legacy = Some(record);
                legacy_raw = Some(bytes.clone());
                current_is_legacy = true;
            } else {
                return Err(Error::corrupt("quota record has an unknown magic"));
            }
        } else if let Some(bytes) = &migrated {
            // The policy file is missing but the preserved legacy record is
            // still present. Its policy remains usable; nothing is rewritten.
            self.quota.record_present = true;
            if bytes.starts_with(QUOTA_MAGIC) {
                let record = QuotaRecord::decode_bytes(bytes)?;
                policy_budgets = Some(record.budgets.clone());
            } else {
                return Err(Error::corrupt("quota.migrated has an unknown magic"));
            }
        }

        if let Some(budgets) = policy_budgets {
            self.quota.policy = budgets
                .into_iter()
                .map(|budget| (budget.principal.clone(), budget))
                .collect();
        }

        // A legacy `quota` that has already been copied to `quota.migrated`
        // must be byte-identical; anything else is an interrupted migration
        // with conflicting evidence.
        if current_is_legacy {
            if let (Some(raw), Some(old)) = (&legacy_raw, &migrated) {
                if raw != old {
                    return Err(Error::corrupt(
                        "both quota and quota.migrated exist and disagree",
                    ));
                }
            }
        }

        // Usage: the head assertion is authoritative; otherwise replay and
        // validate the chain. A committed assertion that disagrees with the
        // recomputed admission totals is `Corrupt`.
        match self.committed_usage_from_head() {
            Ok(Some(usage)) => {
                self.quota.usage = usage;
                self.quota.usage_committed = true;
                self.quota.valid = true;
                self.quota.reason = None;
            }
            Ok(None) => {
                if self.quota.enforced() {
                    match self.replay_quota_usage_with_count() {
                        Ok((usage, blobs, _)) => {
                            self.quota.usage = usage;
                            self.blob_index = blobs;
                            self.blob_index_ready = true;
                            self.quota.usage_committed = false;
                            self.quota.valid = true;
                            self.quota.reason = None;
                        }
                        Err(err) => {
                            self.quota.valid = false;
                            self.quota.reason =
                                Some(format!("committed quota usage is inconsistent: {err}"));
                            return Ok(());
                        }
                    }
                }
            }
            Err(err) => {
                self.quota.valid = false;
                self.quota.reason = Some(format!("committed quota usage is inconsistent: {err}"));
                return Ok(());
            }
        }
        self.quota.generation = self.generation;
        self.quota.commit = self.head;

        // A preserved legacy record that names the current head must agree
        // with committed (or derived) usage. A stale one is historical
        // evidence only.
        if let Some(bytes) = &migrated {
            let record = QuotaRecord::decode_bytes(bytes)?;
            if record.generation == self.generation && record.commit == self.head {
                if self.quota.usage_committed || self.quota.enforced() {
                    let recorded: BTreeMap<QuotaPrincipal, QuotaCounters> =
                        record.usage.into_iter().collect();
                    if recorded != self.quota.usage {
                        self.quota.valid = false;
                        self.quota.reason =
                            Some("quota.migrated usage disagrees with committed usage".to_owned());
                        return Ok(());
                    }
                }
            }
        }

        if let (Some(record), Some(raw)) = (legacy, legacy_raw) {
            if current_is_legacy && self.mode == OpenMode::ReadWrite {
                self.migrate_legacy_quota(record, raw)?;
            }
        }
        Ok(())
    }

    /// Import a legacy combined record once: preserve its exact bytes under
    /// `quota.migrated`, replace `quota` with the policy-only record, and
    /// publish the recomputed usage as a committed op so later opens are O(1).
    fn migrate_legacy_quota(&mut self, record: QuotaRecord, raw: Vec<u8>) -> Result<()> {
        if record.generation == self.generation && record.commit == self.head {
            let recorded: BTreeMap<QuotaPrincipal, QuotaCounters> =
                record.usage.iter().cloned().collect();
            if recorded != self.quota.usage {
                return Err(Error::corrupt(
                    "legacy quota record usage disagrees with committed state",
                ));
            }
        } else {
            self.notes.push(format!(
                "legacy quota record named generation {} ({}); usage was recomputed from committed state",
                record.generation, record.commit
            ));
        }
        let capabilities = self.backend_capabilities();
        capabilities.require_atomic_publish("quota migration")?;
        capabilities.require_durable_fsync("quota migration")?;
        self.with_backend_mut(|backend| backend.put_atomic(&Key::quota_migrated(), &raw))?;
        let policy = QuotaPolicyRecord::new(record.budgets)?.encode_bytes()?;
        self.with_backend_mut(|backend| backend.put_replace(&Key::quota(), &policy))?;
        self.with_backend_mut(|backend| backend.sync_dir(&Key::root()))?;
        self.quota.migrated_present = true;
        self.quota.record_present = true;
        if !self.quota.usage.is_empty() && !self.quota.usage_committed {
            let usage = self.usage_vec();
            self.publish_quota_usage_op(
                QuotaUsageOp {
                    reset: false,
                    usage,
                },
                false,
            )?;
            self.quota.usage_committed = true;
        }
        self.quota.generation = self.generation;
        self.quota.commit = self.head;
        self.quota.valid = true;
        self.quota.reason = None;
        Ok(())
    }

    /// Write the policy-only record. This happens only when a policy is
    /// installed or replaced, never on an ordinary commit.
    fn persist_quota_policy(&self) -> Result<()> {
        let record = QuotaPolicyRecord::new(self.quota.policy.values().cloned().collect())?;
        let bytes = record.encode_bytes()?;
        self.with_backend_mut(|backend| {
            let capabilities = backend.capabilities();
            capabilities.require_atomic_publish("quota policy publication")?;
            capabilities.require_durable_fsync("quota policy publication")?;
            backend.put_replace(&Key::quota(), &bytes)
        })?;
        self.with_backend_mut(|backend| backend.sync_dir(&Key::root()))
    }

    /// Recompute usage from the published commit chain only (never from
    /// unpublished commit files, rejected generations, or process memory).
    fn replay_quota_usage(
        &self,
    ) -> Result<(
        BTreeMap<QuotaPrincipal, QuotaCounters>,
        BTreeMap<Hash32, u64>,
    )> {
        let (usage, blobs, _) = self.replay_quota_usage_with_count()?;
        Ok((usage, blobs))
    }

    fn replay_quota_usage_with_count(
        &self,
    ) -> Result<(
        BTreeMap<QuotaPrincipal, QuotaCounters>,
        BTreeMap<Hash32, u64>,
        u64,
    )> {
        let index = self.with_backend(|backend| recovery::commit_index_via(backend, u64::MAX))?;
        let mut chain: Vec<crate::format::Commit> = Vec::new();
        let mut cursor = Some(self.head);
        let mut seen = BTreeSet::new();
        while let Some(hash) = cursor {
            if !seen.insert(hash) {
                return Err(Error::corrupt("commit chain contains a cycle"));
            }
            let loaded = index.get(&hash).ok_or_else(|| {
                Error::corrupt(format!("commit {hash} is missing from the published chain"))
            })?;
            chain.push(loaded.commit.clone());
            cursor = loaded.commit.parent;
        }
        chain.reverse();

        let mut state = State::default();
        let mut usage: BTreeMap<QuotaPrincipal, QuotaCounters> = BTreeMap::new();
        let mut blobs: BTreeMap<Hash32, u64> = BTreeMap::new();
        let mut commits = 0u64;
        for commit in &chain {
            let pre: BTreeMap<Hash32, Option<ObjectRecord>> = commit
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::PutObject(record) => {
                        Some((record.id, state.objects.get(&record.id).cloned()))
                    }
                    _ => None,
                })
                .collect();
            let (charges, additions) = self.compute_quota_charges(&commit.ops, &pre, &blobs)?;
            for (principal, counters) in charges {
                usage.entry(principal).or_default().add(&counters)?;
            }
            // A committed assertion must equal the totals derived from the
            // object ops of this commit and every earlier commit. This is the
            // replay check that makes a tampered or buggy assertion `Corrupt`.
            let declared = commit.ops.iter().find_map(|op| match op {
                Op::SetQuotaUsage(value) => Some(value),
                _ => None,
            });
            if let Some(value) = declared {
                if value.reset {
                    if commit.ops.len() != 1 || !value.usage.is_empty() {
                        return Err(Error::corrupt(
                            "quota usage reset must be the only op and declare an empty map",
                        ));
                    }
                    usage.clear();
                } else {
                    let declared_map: BTreeMap<QuotaPrincipal, QuotaCounters> =
                        value.usage.iter().cloned().collect();
                    if declared_map != usage {
                        return Err(Error::corrupt(format!(
                            "commit {} quota usage disagrees with the recomputed admission totals",
                            commit.id()?
                        )));
                    }
                }
            }
            blobs.extend(additions);
            state.apply_ops(&commit.ops)?;
            commits += 1;
        }
        Ok((usage, blobs, commits))
    }

    /// Sum the blob bytes reachable from committed state into `index`.
    fn ensure_blob_index(&mut self) -> Result<()> {
        if self.blob_index_ready {
            return Ok(());
        }
        if self.group_depth != 0 {
            // Live group state already contains uncommitted objects, so it is
            // not a valid basis for "already reachable from committed state".
            // `group` builds the index before opening the closure.
            return Err(Error::corrupt(
                "quota blob index cannot be built from live group state",
            ));
        }
        let mut index = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let manifests: Vec<Hash32> = self
            .state
            .objects
            .values()
            .filter(|record| record.kind == ObjectKind::File)
            .map(|record| record.content)
            .collect();
        for manifest in manifests {
            let mut additions = BTreeMap::new();
            self.collect_manifest_blobs(manifest, &index, &mut additions, &mut seen, 0)?;
            index.extend(additions);
        }
        self.blob_index = index;
        self.blob_index_ready = true;
        Ok(())
    }

    /// Walk one manifest tree, inserting the blobs that are not already in
    /// `index` into `additions` and returning the added byte count. `seen`
    /// guards against cycles and duplicate counting inside one commit.
    fn collect_manifest_blobs(
        &self,
        manifest_id: Hash32,
        index: &BTreeMap<Hash32, u64>,
        additions: &mut BTreeMap<Hash32, u64>,
        seen: &mut BTreeSet<Hash32>,
        depth: usize,
    ) -> Result<u64> {
        if depth as u64 > representation::MAX_OVERLAY_CHAIN_DEPTH {
            return Err(Error::corrupt(
                "manifest tree exceeds the manifest-chain depth bound",
            ));
        }
        if !seen.insert(manifest_id) {
            return Ok(0);
        }
        if index.contains_key(&manifest_id) || additions.contains_key(&manifest_id) {
            return Ok(0);
        }
        let bytes = self.read_blob_verified_keyed(
            BlobKind::Manifest,
            manifest_id,
            identity::MANIFEST_DOMAIN,
        )?;
        let size = bytes.len() as u64;
        let value = crate::wcb_util::decode(&bytes)?;
        let manifest = Manifest::from_wcb(&value)?;
        additions.insert(manifest_id, size);
        let mut total = size;
        if let Some(representation) = &manifest.representation {
            if representation.kind == RepresentationKind::Codec
                || representation.kind == RepresentationKind::Tensor
            {
                if depth > MAX_CODEC_DEPENDENCY_DEPTH {
                    return Err(Error::corrupt(
                        "manifest tree exceeds the codec dependency depth bound",
                    ));
                }
                if let Some(frame) = representation.frame {
                    if !index.contains_key(&frame) && !additions.contains_key(&frame) {
                        let frame_len =
                            self.with_backend(|backend| backend.len(&Key::manifest(frame)))?;
                        additions.insert(frame, frame_len);
                        total += frame_len;
                    }
                }
                for dependency in &representation.dependencies {
                    total += self.collect_manifest_blobs(
                        *dependency,
                        index,
                        additions,
                        seen,
                        depth + 1,
                    )?;
                }
                return Ok(total);
            }
            if representation.kind == RepresentationKind::Overlay {
                let overlay = representation.overlay.as_ref().ok_or_else(|| {
                    Error::corrupt("overlay representation is missing its overlay map")
                })?;
                for extent in &overlay.extents {
                    for chunk in &extent.chunks {
                        if index.contains_key(&chunk.hash) || additions.contains_key(&chunk.hash) {
                            continue;
                        }
                        additions.insert(chunk.hash, chunk.len);
                        total += chunk.len;
                    }
                }
                total +=
                    self.collect_manifest_blobs(overlay.base, index, additions, seen, depth + 1)?;
                return Ok(total);
            }
        }
        for chunk in &manifest.chunks {
            if index.contains_key(&chunk.hash) || additions.contains_key(&chunk.hash) {
                continue;
            }
            additions.insert(chunk.hash, chunk.len);
            total += chunk.len;
        }
        Ok(total)
    }

    /// Attribute the additions one commit would make. `pre` holds the object
    /// record that existed for every written id before the commit (or `None`),
    /// and `index` is the blob set reachable from committed state.
    fn compute_quota_charges(
        &self,
        ops: &[Op],
        pre: &BTreeMap<Hash32, Option<ObjectRecord>>,
        index: &BTreeMap<Hash32, u64>,
    ) -> Result<(
        BTreeMap<QuotaPrincipal, QuotaCounters>,
        BTreeMap<Hash32, u64>,
    )> {
        let mut charges: BTreeMap<QuotaPrincipal, QuotaCounters> = BTreeMap::new();
        let mut additions: BTreeMap<Hash32, u64> = BTreeMap::new();
        let mut seen: BTreeSet<Hash32> = BTreeSet::new();
        for op in ops {
            let Op::PutObject(record) = op else {
                continue;
            };
            if record.id == State::ROOT_ID {
                continue;
            }
            let previous = pre.get(&record.id).and_then(|value| value.as_ref());
            if previous == Some(record) {
                continue;
            }
            let mut principals: Vec<QuotaPrincipal> = vec![QuotaPrincipal::Posix {
                uid: record.uid,
                gid: record.gid,
            }];
            if let Some(Value::Str(tenant)) = record.attrs.get(ATTR_TENANT) {
                if !tenant.is_empty() {
                    principals.push(QuotaPrincipal::Tenant(tenant.clone()));
                }
            }
            let record_bytes = crate::wcb_util::encode(&record.to_value())?.len() as u64;
            let mut content_bytes = 0u64;
            let content_changed = previous
                .map(|prior| prior.content != record.content || prior.kind != record.kind)
                .unwrap_or(true);
            if record.kind == ObjectKind::File && content_changed {
                content_bytes = self.collect_manifest_blobs(
                    record.content,
                    index,
                    &mut additions,
                    &mut seen,
                    0,
                )?;
            }
            let delta = QuotaCounters {
                used_bytes: record_bytes
                    .checked_add(content_bytes)
                    .ok_or_else(|| Error::limit_exceeded("quota charge overflow"))?,
                used_objects: u64::from(previous.is_none()),
            };
            for principal in principals {
                charges.entry(principal).or_default().add(&delta)?;
            }
        }
        Ok((charges, additions))
    }

    /// Pre-commit view of the object records touched by `ops` in live state.
    pub(crate) fn quota_pre_map_live(&self, ops: &[Op]) -> BTreeMap<Hash32, Option<ObjectRecord>> {
        let mut map = BTreeMap::new();
        for op in ops {
            if let Op::PutObject(record) = op {
                map.entry(record.id)
                    .or_insert_with(|| self.state.objects.get(&record.id).cloned());
            }
        }
        map
    }

    /// Enforce every configured budget for one commit and compute the usage
    /// snapshot the commit will carry. Returns `None` when no budget is
    /// configured. Nothing is written here; a denial leaves generation, head,
    /// and state untouched.
    pub(crate) fn quota_preflight(
        &mut self,
        ops: &[Op],
        pre: &BTreeMap<Hash32, Option<ObjectRecord>>,
    ) -> Result<Option<QuotaPlan>> {
        if !self.quota.enforced() {
            if self.quota.record_present && !self.quota.valid {
                // A present but unusable record hides the policy. Refusing is
                // the only non-silent behaviour: the operator must re-supply
                // the budgets (`--quota`) or repair the record.
                return Err(Error::corrupt(
                    "quota state is unusable; re-run with `--quota` to install a policy",
                ));
            }
            return Ok(None);
        }
        if !self.quota.valid {
            return Err(Error::corrupt(
                "quota usage is inconsistent with committed state; run `quota rebuild`",
            ));
        }
        self.ensure_blob_index()?;
        let index = std::mem::take(&mut self.blob_index);
        let result = self.compute_quota_charges(ops, pre, &index);
        self.blob_index = index;
        let (charges, additions) = result?;
        let mut usage_after = self.quota.usage.clone();
        for (principal, counters) in &charges {
            usage_after
                .entry(principal.clone())
                .or_default()
                .add(counters)?;
        }
        for (principal, budget) in &self.quota.policy {
            let after = usage_after.get(principal).copied().unwrap_or_default();
            if after.used_bytes > budget.max_new_bytes || after.used_objects > budget.max_objects {
                // Fixed, non-disclosing text: no usage, no limits, no principal.
                return Err(Error::budget_exceeded("write budget exceeded"));
            }
        }
        Ok(Some(QuotaPlan {
            additions,
            usage_after,
        }))
    }

    /// The usage op appended to a commit that carries an accepted plan.
    pub(crate) fn quota_usage_op(plan: &QuotaPlan) -> Op {
        Op::SetQuotaUsage(QuotaUsageOp {
            reset: false,
            usage: plan
                .usage_after
                .iter()
                .map(|(principal, counters)| (principal.clone(), *counters))
                .collect(),
        })
    }

    /// Fold an accepted commit's usage into the in-memory state. The usage was
    /// published as part of the commit itself, so there is no second durable
    /// write and no failure path here.
    pub(crate) fn record_quota_after_commit(&mut self, plan: QuotaPlan) {
        self.quota.usage = plan.usage_after;
        self.quota.usage_committed = true;
        self.blob_index.extend(plan.additions);
        self.quota.generation = self.generation;
        self.quota.commit = self.head;
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

    pub(crate) fn require_writable(&self) -> Result<()> {
        if self.mode != OpenMode::ReadWrite {
            return Err(Error::read_only(
                "store is opened read-only; mutation refused",
            ));
        }
        Ok(())
    }

    /// Ops that delete an object record and remove every relation whose subject
    /// is that object. Explicit ops keep replay and cross-language state
    /// identical; cascading inside `State::apply_op` would change the meaning of
    /// already-published commits.
    fn delete_object_ops(&self, id: Hash32) -> Vec<Op> {
        let mut ops: Vec<Op> = self
            .state
            .relations
            .values()
            .filter(|relation| relation.subject == id)
            .map(|relation| Op::DelRelation(relation.id))
            .collect();
        ops.push(Op::DeleteObject(id));
        ops
    }

    pub(crate) fn require_dir(&self, id: Hash32) -> Result<()> {
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

    pub(crate) fn require_free_name(&self, parent: Hash32, name: &[u8]) -> Result<()> {
        if self
            .state
            .entries
            .contains_key(&derive_entry_id(parent, name)?)
        {
            return Err(Error::exists("directory entry already exists"));
        }
        Ok(())
    }

    pub(crate) fn now_ns(&self) -> i64 {
        if let Some(fixed) = self.options.fixed_timestamp_ns {
            return fixed;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
            .unwrap_or(0)
    }

    /// The real wall clock, independent of the fixture override.
    ///
    /// T024 uses this for anything compared with a backend modification time:
    /// the GC epoch acquisition instant, a publication reservation's start and
    /// deadline, and the reservation liveness test. A fixed fixture timestamp
    /// is for produced state (mtimes, commit timestamps, record `updated_ns`),
    /// never for a comparison against the filesystem's own clock.
    pub(crate) fn wall_ns(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
            .unwrap_or(0)
    }

    /// True while a group commit is open. Retention GC refuses to run then
    /// because it physically deletes blobs a rollback could restore.
    pub(crate) fn in_group(&self) -> bool {
        self.group_depth != 0
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
        let pre = crate::index_incremental::PreImages::empty();
        let mut commit = Commit {
            generation: 1,
            parent: None,
            timestamp_ns: mtime,
            ops,
            checkpoint: None,
            index_attestation: None,
        };
        let (attestation, built_index) = self.index_attestation_for(&state, &commit, &[], &pre)?;
        commit.index_attestation = attestation;
        self.publish_commit(&commit)?;
        self.state = state;
        self.head = commit.id()?;
        self.generation = 1;
        self.next_generation = 2;
        if let Some(index) = built_index {
            self.persist_built_index(self.head, index);
        }
        Ok(())
    }

    pub(crate) fn commit_ops(&mut self, ops: Vec<Op>, force_checkpoint: bool) -> Result<Hash32> {
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
        let mut ops = ops;
        let new_generation = self.next_generation;
        let timestamp = self.now_ns();
        let mut next_state = self.state.clone();
        next_state.apply_ops(&ops)?;
        next_state.generation = new_generation;

        // Write-budget admission happens before any commit bytes exist. A denial
        // returns `BudgetExceeded` and leaves generation, head, and state
        // exactly as they were; content blobs already stored are unreferenced
        // and reclaimable by GC.
        let quota_pre = self.quota_pre_map_live(&ops);
        let quota = self.quota_preflight(&ops, &quota_pre)?;
        // The usage snapshot is part of the same commit, so it needs no
        // separate durable write.
        if let Some(plan) = &quota {
            ops.push(Self::quota_usage_op(plan));
        }

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

        // `self.state` is still the pre-op state: capture the index pre-images
        // before the in-memory state is replaced below.
        let pre = crate::index_incremental::PreImages::capture(&self.state, &ops);
        let mut commit = Commit {
            generation: new_generation,
            parent: Some(self.head),
            timestamp_ns: timestamp,
            ops,
            checkpoint,
            index_attestation: None,
        };
        let (attestation, built_index) =
            self.index_attestation_for(&next_state, &commit, &commit.ops, &pre)?;
        commit.index_attestation = attestation;
        let hash = self.publish_commit(&commit)?;

        // Keep the in-memory state consistent with the published commit.
        self.state = next_state;
        self.head = hash;
        self.generation = new_generation;
        self.next_generation = new_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        // Advisory only: the commit is already durable and visible. A failed
        // index write is recorded and ignored.
        if let Some(index) = built_index {
            self.persist_built_index(hash, index);
        }
        if let Some(plan) = quota {
            self.record_quota_after_commit(plan);
        }
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
            Op::PutObject(record) => {
                let previous = self.state.objects.get(&record.id).cloned();
                self.group_pre_objects
                    .entry(record.id)
                    .or_insert_with(|| previous.clone());
                self.group_undo.push(Undo::Object(record.id, previous))
            }
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
            Op::PutRelation(record) => {
                let previous = self.state.relations.get(&record.id).cloned();
                self.group_pre_relations
                    .entry(record.id)
                    .or_insert_with(|| previous.clone());
                self.group_undo.push(Undo::Relation(record.id, previous))
            }
            Op::DelRelation(id) => {
                let previous = self.state.relations.get(id).cloned();
                self.group_pre_relations
                    .entry(*id)
                    .or_insert_with(|| previous.clone());
                self.group_undo.push(Undo::Relation(*id, previous))
            }
            Op::SetSnapshot { name, .. } => self.group_undo.push(Undo::Snapshot(
                name.clone(),
                self.state.snapshots.get(name).copied(),
            )),
            Op::SetTip { object, .. } => self
                .group_undo
                .push(Undo::Tip(*object, self.state.tips.get(object).copied())),
            // Accounting only; it changes no namespace state to undo.
            Op::SetQuotaUsage(_) => {}
        }
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
    /// `pre_objects` holds the object records that existed before the group for
    /// every id the group wrote; the quota delta is measured against it.
    fn publish_staged(
        &mut self,
        ops: Vec<Op>,
        pre_objects: BTreeMap<Hash32, Option<ObjectRecord>>,
        pre_relations: BTreeMap<Hash32, Option<RelationRecord>>,
        force_checkpoint: bool,
    ) -> Result<Hash32> {
        let mut ops = ops;
        let new_generation = self.next_generation;
        let next_generation = new_generation
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("generation counter overflow"))?;
        let quota = self.quota_preflight(&ops, &pre_objects)?;
        if let Some(plan) = &quota {
            ops.push(Self::quota_usage_op(plan));
        }
        let pre = crate::index_incremental::PreImages {
            objects: pre_objects,
            relations: pre_relations,
        };
        let previous_state_generation = self.state.generation;
        self.state.generation = new_generation;
        match self.try_publish_staged(new_generation, ops, force_checkpoint, pre) {
            Ok(hash) => {
                self.next_generation = next_generation;
                if let Some(plan) = quota {
                    self.record_quota_after_commit(plan);
                }
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
        pre: crate::index_incremental::PreImages,
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

        let mut commit = Commit {
            generation: new_generation,
            parent: Some(self.head),
            timestamp_ns: timestamp,
            ops,
            checkpoint,
            index_attestation: None,
        };
        let (attestation, built_index) =
            self.index_attestation_for(&self.state, &commit, &commit.ops, &pre)?;
        commit.index_attestation = attestation;
        let hash = self.publish_commit(&commit)?;

        self.head = hash;
        self.generation = new_generation;
        // Advisory only: the commit is already durable and visible.
        if let Some(index) = built_index {
            self.persist_built_index(hash, index);
        }
        Ok(hash)
    }

    /// Publish one commit through the configured writer mode.
    fn publish_commit(&self, commit: &Commit) -> Result<Hash32> {
        crate::phase_profile::timed(crate::phase_profile::Phase::ManifestCommit, || {
            match self.options.writer_mode {
                crate::mw2::WriterMode::Single => self.publish_commit_single(commit),
                crate::mw2::WriterMode::Optimistic | crate::mw2::WriterMode::Sharded => {
                    self.publish_commit_optimistic(commit)
                }
            }
        })
    }

    /// Write the commit file, fsync it and its directory, publish the
    /// superblock slot, then fsync the store root.
    ///
    /// The physical primitives come from the backend; this sequence is the
    /// protocol. `put_atomic` supplies all-or-nothing visibility for the commit
    /// record and `publish_head` updates the inactive 4 KiB slot. Both are
    /// gated on `atomic_publish` and `durable_fsync`; a backend that cannot
    /// publish the head conditionally is refused for a writable open rather
    /// than downgraded.
    fn publish_commit_single(&self, commit: &Commit) -> Result<Hash32> {
        let wcb = commit.encode()?;
        let hash = identity::commit_id(&wcb);
        let file_bytes = encode_commit_file(&wcb)?;
        let key = Key::commit(commit.generation);
        self.with_backend_mut(|backend| {
            let capabilities = backend.capabilities();
            capabilities.require_atomic_publish("commit publication")?;
            capabilities.require_durable_fsync("commit publication")?;
            // Prove the writer still owns a valid lease *before* the commit
            // record is published. A fenced-out writer fails here and leaves
            // no commit record, no head change, and no mutated blob: every
            // blob is create-once.
            backend.verify_write_lease()?;
            // Court hook: suspend after the fence and before the publication.
            // See `src/fence_pause.rs`; inert unless the court arms it.
            crate::fence_pause::after_fence(self.generation, &self.head);
            // Local deadline re-check: a writer that was suspended past its own
            // lease refuses here, before it leaves an unpublished commit tail.
            // The publication write below is still the authority.
            backend.check_lease_deadline()?;
            // A blob plane may buffer small members in memory (packs). The
            // commit record must not name a blob that is not yet durable, so
            // flush before the record is published.
            backend.flush_buffered_blobs()?;
            if backend.exists(&key)? {
                return Err(Error::corrupt(format!(
                    "commit file {} already exists",
                    key.display_path()
                )));
            }
            backend.put_atomic(&key, &file_bytes)
        })?;
        self.publication
            .commit_records
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.publication.commit_record_bytes.fetch_add(
            file_bytes.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        crash_point("after_commit_file");

        let slot = SuperblockSlot {
            generation: commit.generation,
            commit: hash,
            commit_len: wcb.len() as u64,
            timestamp_ns: commit.timestamp_ns,
        };
        let offset = (commit.generation % 2) * SLOT_BYTES as u64;
        let expected = HeadExpectation {
            generation: self.generation,
            commit: self.head,
        };
        let published_in_lock = self.with_backend_mut(|backend| {
            let capabilities = backend.capabilities();
            capabilities.require_durable_fsync("superblock publication")?;
            capabilities.require_conditional_head("superblock publication")?;
            // The publication point. On an object-store commit plane the head is
            // published by **one** conditional write of the `LOCK` record that
            // carries both the fence and the new head: a successor's takeover
            // rewrites the same object, so a stale writer cannot publish even
            // when it is suspended between the fence and the head.
            //
            // A POSIX plane keeps its existing two-step publication under the
            // exclusive kernel lock, which no suspension releases.
            if backend.publish_fenced_head(commit.generation, &hash, wcb.len() as u64)? {
                return Ok(true);
            }
            backend.fence_head_publication(commit.generation, &hash)?;
            backend.publish_head(
                expected,
                commit.generation,
                hash,
                wcb.len() as u64,
                commit.timestamp_ns,
                offset,
                &slot.encode(),
            )?;
            Ok(false)
        })?;

        if published_in_lock {
            // The head is published. The two-slot superblock is now only a
            // cache for this commit plane: refresh it if this writer still owns
            // the lease and the cache is behind. A cache failure (a lost race, or
            // a successor that already advanced the cache) never turns a
            // published head into a reported failure; the backend records its
            // own diagnostic.
            crash_point("after_fenced_head");
            let _ = self.with_backend_mut(|backend| {
                backend.refresh_head_cache(
                    commit.generation,
                    hash,
                    wcb.len() as u64,
                    commit.timestamp_ns,
                )
            });
        }
        // One head publication: either the fenced head write or the two-slot
        // superblock record. Counted once per published commit.
        self.publication
            .superblock_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        crash_point("after_superblock_write");

        self.with_backend_mut(|backend| backend.sync_dir(&Key::root()))?;
        Ok(hash)
    }

    /// MW2 publication: create-once commit record plus a conditional head
    /// compare-and-swap, with no exclusive writer lease.
    ///
    /// The caller has already staged every blob and verified it. The expected
    /// head is the one this handle observed (`self.head`/`self.generation`). A
    /// lost race is `Busy`; the transaction loop reloads the new head, validates
    /// the conflict table, and retries within its budget. A commit record we
    /// published but could not make visible is deleted, because no head names
    /// it; if the delete fails the record stays as an unreferenced file and
    /// `verify` reports it, which is the honest outcome.
    fn publish_commit_optimistic(&self, commit: &Commit) -> Result<Hash32> {
        let wcb = commit.encode()?;
        let hash = identity::commit_id(&wcb);
        let file_bytes = encode_commit_file(&wcb)?;
        let key = self.commit_key(commit.generation);
        let expected = HeadExpectation {
            generation: self.generation,
            commit: self.head,
        };
        let domain = self.domain;
        let slot = SuperblockSlot {
            generation: commit.generation,
            commit: hash,
            commit_len: wcb.len() as u64,
            timestamp_ns: commit.timestamp_ns,
        };
        let slot_bytes = slot.encode();

        let published = self.with_backend_mut(|backend| -> Result<bool> {
            let capabilities = backend.capabilities();
            capabilities.require_atomic_publish("commit publication")?;
            capabilities.require_durable_fsync("commit publication")?;
            capabilities.require_conditional_head("commit publication")?;
            backend.flush_buffered_blobs()?;
            // The short publication critical section. On a POSIX plane this
            // makes the create-once check and the head compare-and-swap atomic
            // across processes; on an object-store plane it is a no-op because
            // the conditional writes are native.
            match domain {
                Some(domain) => backend.begin_domain_publication(&domain)?,
                None => backend.begin_publication()?,
            }
            let attempt = (|| -> Result<bool> {
                crash_point("mw2_before_head");
                if backend.exists(&key)? {
                    return Err(Error::busy(format!(
                        "commit generation {} already exists; retry against the new head",
                        commit.generation
                    )));
                }
                match backend.put_atomic(&key, &file_bytes) {
                    Ok(()) => {}
                    Err(err) if err.class() == Class::Exists => {
                        // Another committer used this generation first. The
                        // transaction loop reloads the new head and retries.
                        return Err(Error::busy(format!(
                            "commit generation {} already exists with different bytes; retry \
                             against the new head",
                            commit.generation
                        )));
                    }
                    Err(err) => return Err(err),
                }
                let head_result = match domain {
                    Some(domain) => backend.publish_domain_head_conditional(
                        &domain,
                        expected,
                        commit.generation,
                        hash,
                        commit.timestamp_ns,
                        &slot_bytes,
                    ),
                    None => backend.publish_head_conditional(
                        expected,
                        commit.generation,
                        hash,
                        wcb.len() as u64,
                        commit.timestamp_ns,
                        &slot_bytes,
                    ),
                };
                match head_result {
                    Ok(()) => {
                        crash_point("mw2_after_head");
                        Ok(true)
                    }
                    Err(err) if err.class() == Class::Busy => {
                        // The conditional head write was definitely refused.
                        // The record this attempt created is usually
                        // unreferenced, and removing it keeps a later
                        // generation from being blocked by an orphan. It is
                        // *not* always ours, though: create-once publication
                        // accepts identical bytes, so a twin committer may have
                        // created the same record and published it. Consult the
                        // head and only delete when it cannot name this record.
                        let observed = match domain {
                            Some(domain) => match backend.load_domain_head(&domain) {
                                Ok(Some((generation, commit))) => {
                                    Some(crate::format::SuperblockSlot {
                                        generation,
                                        commit,
                                        commit_len: 0,
                                        timestamp_ns: 0,
                                    })
                                }
                                _ => None,
                            },
                            None => backend
                                .read_all(&Key::superblock())
                                .ok()
                                .and_then(|image| crate::format::decode_superblock(&image).ok())
                                .and_then(|slots| {
                                    slots
                                        .into_iter()
                                        .flatten()
                                        .max_by_key(|slot| slot.generation)
                                }),
                        };
                        match observed {
                            Some(slot)
                                if slot.generation == commit.generation && slot.commit == hash =>
                            {
                                // The identical commit is already published:
                                // report it as this attempt's outcome.
                                Ok(true)
                            }
                            Some(slot) if slot.generation >= commit.generation => {
                                // A later head may be built on this record
                                // (a twin published it). Never delete it.
                                Err(err)
                            }
                            _ => {
                                let _ = backend.delete(&key);
                                Err(err)
                            }
                        }
                    }
                    Err(err) => {
                        // Any other failure (transport, timeout, local I/O) is
                        // ambiguous: the conditional write may have landed. A
                        // delete here could destroy a published commit, so the
                        // record is left in place. If it is unreferenced,
                        // recovery and `verify` report it; if it is the new
                        // head, the transaction is resolved by idempotent
                        // lookup instead of losing acknowledged data.
                        Err(err)
                    }
                }
            })();
            let ended = match domain {
                Some(domain) => backend.end_domain_publication(&domain),
                None => backend.end_publication(),
            };
            match attempt {
                Ok(value) => {
                    ended?;
                    Ok(value)
                }
                Err(err) => {
                    // A failure to release the publication lock must not hide
                    // the publication error; the guard is dropped with the
                    // backend, which unlocks explicitly.
                    let _ = ended;
                    Err(err)
                }
            }
        })?;
        if !published {
            return Err(Error::busy(
                "optimistic publication did not publish the head",
            ));
        }
        self.with_backend_mut(|backend| backend.sync_dir(&Key::root()))?;
        Ok(hash)
    }

    /// Stream `reader` into content-addressed chunk blobs and compute the
    /// requested content identity incrementally.
    ///
    /// `prefix` must already contain whole chunks (it is the buffered codec
    /// preamble). The I/O buffer is at least `STREAM_IO_BYTES` and a multiple
    /// of the chunk size, so every emitted chunk is exactly `chunk_size` bytes
    /// except the final one. Neither the prefix nor the I/O buffer is retained
    /// after return; peak memory is O(io buffer + chunk refs).
    ///
    /// With `ContentIdentity::WholeFileV1` the bytes also feed the frozen
    /// whole-content hasher (O(file size) hashing, bounded memory). With
    /// `ContentIdentity::MerkleV3` only the per-chunk leaf hashes are kept and
    /// the root is folded from the chunk table.
    fn stream_raw_chunks<R: Read>(
        &self,
        reader: &mut R,
        chunk_size: u64,
        prefix: Vec<u8>,
        content_identity: ContentIdentity,
    ) -> Result<(Vec<ChunkRef>, u64, Hash32)> {
        validate_chunk_size(chunk_size)?;
        let step = checked_usize(chunk_size, "chunk size")?;
        if !prefix.is_empty() && prefix.len() % step != 0 {
            return Err(Error::invalid_argument(
                "streaming prefix is not chunk-aligned",
            ));
        }
        let mut chunks: Vec<ChunkRef> = Vec::new();
        let whole_file = content_identity == ContentIdentity::WholeFileV1;
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
            if whole_file {
                hasher.update(&prefix);
            }
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
            if whole_file {
                hasher.update(&buffer[..filled]);
            }
            for part in buffer[..filled].chunks(step) {
                let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, part)?;
                push_chunk_ref(&mut chunks, hash, part.len() as u64)?;
            }
            if filled < io_len {
                break;
            }
        }
        let content_hash = match content_identity {
            ContentIdentity::WholeFileV1 => identity::finish_content_hash(hasher),
            ContentIdentity::MerkleV3 => identity::merkle_root_from_chunks(
                chunks.iter().map(|chunk| (chunk.len, chunk.hash)),
            ),
        };
        Ok((chunks, total, content_hash))
    }

    /// Store a manifest assembled by the streaming paths. `v2_raw` selects the
    /// experimental v2 `kind:"raw"` representation used by `CodecAuto`;
    /// otherwise the frozen v1 manifest is written byte-for-byte (or the v3
    /// Merkle manifest when `content_identity` is `MerkleV3`).
    #[allow(clippy::too_many_arguments)]
    fn store_streamed_manifest(
        &self,
        chunk_size: u64,
        chunks: Vec<ChunkRef>,
        size: u64,
        content_hash: Hash32,
        v2_raw: bool,
        content_identity: ContentIdentity,
    ) -> Result<(Hash32, Hash32, u64)> {
        let representation = if v2_raw {
            Some(Representation {
                kind: RepresentationKind::Raw,
                frame: None,
                dependencies: Vec::new(),
                strategy: "raw".to_owned(),
                decoded_size: size,
                tensor: None,
                overlay: None,
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
            identity: content_identity,
        })
    }

    /// Encode and store a complete in-memory buffer. Used for initial file
    /// content (`create_file*`), `truncate`, and the whole-file bench baseline.
    /// Range writes go through `Store::store_file_ranges` instead.
    fn store_file_content(&self, bytes: &[u8]) -> Result<(Hash32, Hash32, u64)> {
        self.store_file_content_with_identity(bytes, self.options.content_identity)
    }

    /// The identity a rewrite of `record` must keep: an existing v3 file stays
    /// v3 (never implicitly downgraded), while a v1/v2 file adopts the
    /// store-wide option.
    fn content_identity_for_object(&self, record: &ObjectRecord) -> Result<ContentIdentity> {
        if record.kind != ObjectKind::File {
            return Ok(self.options.content_identity);
        }
        let manifest = self.read_manifest_keyed(record.content)?;
        Ok(match manifest.identity {
            ContentIdentity::MerkleV3 => ContentIdentity::MerkleV3,
            ContentIdentity::WholeFileV1 => self.options.content_identity,
        })
    }

    fn store_file_content_with_identity(
        &self,
        bytes: &[u8],
        content_identity: ContentIdentity,
    ) -> Result<(Hash32, Hash32, u64)> {
        self.store_content_bytes(
            bytes,
            self.options.chunk_size,
            None,
            self.options.representation == RepresentationMode::CodecAuto,
            content_identity,
        )
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
            self.options.content_identity,
        )
    }

    /// Store a whole content buffer under an explicit chunk size.
    ///
    /// `codec_auto == false` is the frozen Raw path and produces exactly the
    /// pre-representation v1 manifest bytes. `codec_auto == true` may store a
    /// v2 codec representation or a v2 `kind:"raw"` fallback. With
    /// `ContentIdentity::MerkleV3` the raw chunk table is written as a v3
    /// manifest and its `content_root` is folded from the chunk hashes.
    fn store_content_bytes(
        &self,
        bytes: &[u8],
        chunk_size: u64,
        base: Option<(Hash32, &[u8])>,
        codec_auto: bool,
        content_identity: ContentIdentity,
    ) -> Result<(Hash32, Hash32, u64)> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
            return Err(Error::invalid_argument("chunk size is out of range"));
        }
        if content_identity == ContentIdentity::MerkleV3 {
            if codec_auto {
                return Err(Error::invalid_argument(
                    "content_identity `merkle` cannot be combined with codec-auto",
                ));
            }
            let manifest = self.build_merkle_manifest(bytes, chunk_size)?;
            return self.store_manifest(manifest);
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
                        identity: ContentIdentity::WholeFileV1,
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
                    tensor: None,
                    overlay: None,
                }),
                ..manifest
            };
            return self.store_manifest(manifest);
        }
        let manifest = self.build_raw_manifest(bytes, chunk_size, content_hash)?;
        self.store_manifest(manifest)
    }

    /// Build a `weft.manifest.v3` raw manifest: chunk blobs plus the Merkle
    /// root folded over the chunk table. No whole-file digest is computed.
    pub(crate) fn build_merkle_manifest(&self, bytes: &[u8], chunk_size: u64) -> Result<Manifest> {
        let mut chunks = Vec::new();
        let step = usize::try_from(chunk_size)
            .map_err(|_| Error::limit_exceeded("chunk size does not fit usize"))?;
        for chunk in bytes.chunks(step) {
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, chunk)?;
            push_chunk_ref(&mut chunks, hash, chunk.len() as u64)?;
        }
        let content_root =
            identity::merkle_root_from_chunks(chunks.iter().map(|chunk| (chunk.len, chunk.hash)));
        Ok(Manifest {
            size: bytes.len() as u64,
            chunk_size,
            chunks,
            content_hash: content_root,
            representation: None,
            identity: ContentIdentity::MerkleV3,
        })
    }

    /// Write the chunk blobs for a raw layout and return a v1-shaped manifest
    /// value (with no representation).
    pub(crate) fn build_raw_manifest(
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
            identity: ContentIdentity::WholeFileV1,
        })
    }

    /// Build a raw (chunk-table) manifest from a bounded source.
    ///
    /// Byte-identical to [`Store::build_raw_manifest`] for the same bytes: each
    /// chunk is stored as the same content-addressed blob and the table is built
    /// in the same order. It reads one chunk at a time, so the whole tensor is
    /// never resident.
    pub(crate) fn build_raw_manifest_streamed(
        &self,
        source: &mut dyn ByteSource,
        chunk_size: u64,
        content_hash: Hash32,
    ) -> Result<Manifest> {
        let step = usize::try_from(chunk_size)
            .map_err(|_| Error::limit_exceeded("chunk size does not fit usize"))?;
        if step == 0 {
            return Err(Error::invalid_argument(
                "chunk size must be greater than zero",
            ));
        }
        let size = source.len();
        let mut chunks = Vec::new();
        let mut buffer = vec![0u8; step];
        let mut offset: u64 = 0;
        while offset < size {
            let len = (size - offset).min(chunk_size) as usize;
            source.read_at(offset, &mut buffer[..len])?;
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, &buffer[..len])?;
            push_chunk_ref(&mut chunks, hash, len as u64)?;
            offset += len as u64;
        }
        Ok(Manifest {
            size,
            chunk_size,
            chunks,
            content_hash,
            representation: None,
            identity: ContentIdentity::WholeFileV1,
        })
    }

    pub(crate) fn store_manifest(&self, manifest: Manifest) -> Result<(Hash32, Hash32, u64)> {
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
                tensor: None,
                overlay: None,
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
    /// chunk_size). With `ContentIdentity::MerkleV3` the content root is folded
    /// from the manifest's stored chunk hashes, so hashing is O(#chunks) small
    /// hashes and no unchanged byte is read. With `ContentIdentity::WholeFileV1`
    /// the frozen v1 identity is a single whole-file
    /// `ContentId = H("weft/chunk/v1", file_bytes)`, so it can only be produced
    /// by streaming the whole file; that pass is O(file size) by construction,
    /// done with one chunk-sized buffer at a time so memory never becomes
    /// O(file size). It cannot be replaced by a Merkle root without breaking v1
    /// identity.
    pub(crate) fn store_file_ranges(
        &self,
        old_manifest: &Manifest,
        ranges: &[(u64, &[u8])],
        new_size: u64,
        content_identity: ContentIdentity,
    ) -> Result<(Hash32, Hash32, u64)> {
        let chunk_size = old_manifest.chunk_size;
        if chunk_size == 0 || chunk_size > crate::format::MAX_CHUNK_SIZE {
            return Err(Error::invalid_argument("chunk size is out of range"));
        }
        if new_size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        if new_size < old_manifest.size {
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

        // Which ranges touch each chunk. This bounds the per-chunk work to the
        // ranges that actually overlap it; a range list cannot make the walk
        // quadratic in the file size.
        let mut touched: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (index, (offset, bytes)) in ranges.iter().enumerate() {
            let end = offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| Error::limit_exceeded("write offset overflows"))?;
            if end > new_size {
                return Err(Error::invalid_argument(
                    "range write exceeds the target file size",
                ));
            }
            let first = *offset / chunk_size;
            let last = (end - 1) / chunk_size;
            for chunk in first..=last {
                touched.entry(chunk).or_default().push(index);
            }
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
            let overlapping = touched.get(&index);
            if let Some(old_ref) = old_ref {
                if chunk_end <= old_manifest.size && overlapping.is_none() {
                    // Unchanged chunk: reuse the old reference verbatim. Its
                    // blob is neither read nor re-written here.
                    chunks.push(old_ref.clone());
                    continue;
                }
            }
            // Reconstruct this chunk from the old bytes (if any), the written
            // ranges, and zeros for any gap past the old EOF.
            let mut assembled = vec![0u8; checked_usize(chunk_end - chunk_start, "chunk length")?];
            let mut old_bytes: Option<Vec<u8>> = None;
            if let Some(old_ref) = old_ref {
                let keep = checked_usize(
                    old_ref.len.min(chunk_end - chunk_start),
                    "chunk keep length",
                )?;
                if keep > 0 {
                    let stored = self.read_blob_verified_keyed(
                        BlobKind::Object,
                        old_ref.hash,
                        identity::CHUNK_DOMAIN,
                    )?;
                    self.note_range_write_chunk_read();
                    if stored.len() as u64 != old_ref.len {
                        return Err(Error::corrupt("chunk length disagrees with manifest"));
                    }
                    assembled[..keep].copy_from_slice(&stored[..keep]);
                    old_bytes = Some(stored);
                }
            }
            if let Some(overlapping) = overlapping {
                for &range_index in overlapping {
                    let (offset, bytes) = ranges[range_index];
                    let range_end = offset + bytes.len() as u64;
                    let write_start = offset.max(chunk_start);
                    let write_end = range_end.min(chunk_end);
                    if write_start >= write_end {
                        continue;
                    }
                    let dst_start = checked_usize(write_start - chunk_start, "chunk write start")?;
                    let dst_end = checked_usize(write_end - chunk_start, "chunk write end")?;
                    let src_start = checked_usize(write_start - offset, "range write start")?;
                    let src_end = checked_usize(write_end - offset, "range write end")?;
                    assembled[dst_start..dst_end].copy_from_slice(&bytes[src_start..src_end]);
                }
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

        // Whole-file identity needs the new content streamed in chunk order
        // into the frozen v1 digest; that is O(file size) by construction but
        // bounded to one chunk of memory. Merkle identity folds the chunk
        // hashes already in the table, so no blob is read here at all.
        let content_hash = match content_identity {
            ContentIdentity::WholeFileV1 => {
                let mut hasher = identity::content_hash_hasher();
                for chunk in &chunks {
                    let data = self.read_blob_verified_keyed(
                        BlobKind::Object,
                        chunk.hash,
                        identity::CHUNK_DOMAIN,
                    )?;
                    self.note_range_write_chunk_read();
                    if data.len() as u64 != chunk.len {
                        return Err(Error::corrupt("chunk length disagrees with manifest"));
                    }
                    hasher.update(&data);
                }
                identity::finish_content_hash(hasher)
            }
            ContentIdentity::MerkleV3 => identity::merkle_root_from_chunks(
                chunks.iter().map(|chunk| (chunk.len, chunk.hash)),
            ),
        };
        let manifest = Manifest {
            size: new_size,
            chunk_size,
            chunks,
            content_hash,
            representation: None,
            identity: content_identity,
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

    /// Apply byte edits as one immutable overlay layer over the previous
    /// version (`weft.manifest.v4`).
    ///
    /// The new manifest names the previous version (or, when the previous
    /// version is itself an overlay layer, that layer's base) as an immutable
    /// base plus one merged extent. Only chunks the edits touch are read and,
    /// when their bytes change, stored; every other chunk inside the extent
    /// keeps its old reference verbatim and its blob is not read. The canonical
    /// chunk table and the Merkle root are derived without reading any
    /// unchanged chunk, and the result is exactly the root a canonical
    /// single-table `weft.manifest.v3` over the same logical bytes at the same
    /// `chunk_size` would produce.
    ///
    /// A misaligned edit is widened to the enclosing chunk grid automatically:
    /// the untouched bytes of the widened chunks are read from the previous
    /// version, so the logical content is exact.
    ///
    /// When the previous manifest is an overlay layer whose extent list stays
    /// within [`crate::representation::MAX_OVERLAY_EXTENTS_PER_MANIFEST`] after
    /// merging, the new extent is folded into that layer and the chain depth
    /// does not grow. Otherwise a new layer is published.
    #[allow(clippy::too_many_arguments)]
    fn write_file_overlay(
        &mut self,
        id: Hash32,
        old: &ObjectRecord,
        old_manifest: &Manifest,
        edits: &[OverlayEdit<'_>],
        new_size: u64,
        causal: &[Hash32],
    ) -> Result<WriteOutcome> {
        if old_manifest.identity != ContentIdentity::MerkleV3 {
            return Err(Error::unsupported_representation(
                "overlay writes require a Merkle (`weft.manifest.v3`) base; \
                 re-create the file with `--content-identity merkle`",
            ));
        }
        let chunk_size = old_manifest.chunk_size;
        validate_chunk_size(chunk_size)?;
        if new_size < old_manifest.size {
            return Err(Error::invalid_argument(
                "range write exceeds the target file size",
            ));
        }
        if new_size > MAX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "file exceeds the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let old_table = self.canonical_chunk_table(old.content, old_manifest)?;
        let old_total = old_table
            .iter()
            .try_fold(0u64, |acc, chunk| acc.checked_add(chunk.len))
            .ok_or_else(|| Error::corrupt("base chunk table size overflows"))?;
        if old_total != old.size {
            return Err(Error::corrupt(
                "base chunk table length disagrees with the object size",
            ));
        }

        // Which chunks the edits touch. Only those are read or rewritten.
        let mut touched: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut first = u64::MAX;
        let mut last = 0u64;
        for (index, edit) in edits.iter().enumerate() {
            if edit.len == 0 {
                continue;
            }
            let end = edit.end()?;
            if end > new_size {
                return Err(Error::invalid_argument(
                    "range write exceeds the target file size",
                ));
            }
            let start_chunk = edit.offset / chunk_size;
            let end_chunk = (end - 1) / chunk_size;
            first = first.min(start_chunk);
            last = last.max(end_chunk);
            for chunk in start_chunk..=end_chunk {
                touched.entry(chunk).or_default().push(index);
            }
        }
        if first == u64::MAX {
            return Ok(WriteOutcome {
                version: old.version,
                content: old.content,
                size: old.size,
            });
        }
        let total_chunks = new_size / chunk_size + u64::from(new_size % chunk_size != 0);
        if total_chunks > MAX_CHUNKS_PER_FILE {
            return Err(Error::limit_exceeded(format!(
                "manifest exceeds the {MAX_CHUNKS_PER_FILE}-chunk bound"
            )));
        }
        let old_chunks = old.size / chunk_size + u64::from(old.size % chunk_size != 0);
        let mut span_first = first;
        let mut span_last = last;
        if new_size > old.size {
            // Growing the file must re-encode the old final chunk (it may be
            // short) and any chunk skipped by a write that starts past EOF, so
            // the extent cannot begin after them.
            span_first = span_first.min(old_chunks.saturating_sub(1));
        }

        // Publication shape: fold into the current overlay layer when its
        // extent list survives the merge, otherwise publish a new layer.
        let old_overlay = old_manifest
            .representation
            .as_ref()
            .filter(|rep| rep.kind == RepresentationKind::Overlay)
            .and_then(|rep| rep.overlay.as_ref());
        let mut kept_extents: Vec<OverlayExtent> = Vec::new();
        let (base_id, base_root, base_size, base_depth, base_chain_bytes) = match old_overlay {
            Some(overlay) => {
                let old_extent_total = overlay
                    .extents
                    .iter()
                    .try_fold(0u64, |acc, extent| acc.checked_add(extent.length))
                    .ok_or_else(|| Error::corrupt("overlay byte total overflows"))?;
                let chain_bytes = overlay
                    .overlay_bytes
                    .checked_sub(old_extent_total)
                    .ok_or_else(|| {
                        Error::corrupt("overlay_bytes is smaller than the base layer's extents")
                    })?;
                // Merge every extent that overlaps or touches the new span.
                for extent in &overlay.extents {
                    let extent_first = extent.offset / chunk_size;
                    let extent_last_exclusive = extent_first + extent.chunks.len() as u64;
                    let touches = extent_first <= span_last.saturating_add(1)
                        && span_first <= extent_last_exclusive;
                    if touches {
                        span_first = span_first.min(extent_first);
                        span_last = span_last.max(extent_last_exclusive - 1);
                    } else {
                        kept_extents.push(extent.clone());
                    }
                }
                if kept_extents.len() + 1 <= crate::representation::MAX_OVERLAY_EXTENTS_PER_MANIFEST
                {
                    (
                        overlay.base,
                        overlay.base_content_root,
                        overlay.base_size,
                        overlay.depth - 1,
                        chain_bytes,
                    )
                } else {
                    kept_extents.clear();
                    (
                        old.content,
                        old_manifest.content_hash,
                        old_manifest.size,
                        overlay.depth,
                        overlay.overlay_bytes,
                    )
                }
            }
            None => (
                old.content,
                old_manifest.content_hash,
                old_manifest.size,
                0,
                0,
            ),
        };
        let new_depth = base_depth
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("overlay depth overflows"))?;
        if new_depth > self.options.max_overlay_depth {
            return Err(Error::limit_exceeded(format!(
                "overlay depth {new_depth} exceeds the configured writer bound {}; \
                 run `weftfs compact <path>` to canonicalise the file",
                self.options.max_overlay_depth
            )));
        }

        // Re-encode exactly the chunks the merged extent covers.
        let mut extent_chunks: Vec<ChunkRef> = Vec::with_capacity(checked_usize(
            span_last - span_first + 1,
            "extent chunk count",
        )?);
        let mut changed = false;
        for index in span_first..=span_last {
            let chunk_start = index * chunk_size;
            let chunk_end = (chunk_start + chunk_size).min(new_size);
            if chunk_end <= chunk_start {
                return Err(Error::corrupt("overlay span lies past the new file size"));
            }
            let chunk_len = chunk_end - chunk_start;
            let old_ref = old_table.get(checked_usize(index, "chunk index")?);
            let overlapping = touched.get(&index);
            if let Some(old_ref) = old_ref {
                if old_ref.len == chunk_len && overlapping.is_none() {
                    // An untouched chunk inside the extent keeps its old
                    // reference verbatim; its blob is not read.
                    extent_chunks.push(old_ref.clone());
                    continue;
                }
            }
            let mut assembled = vec![0u8; checked_usize(chunk_len, "chunk length")?];
            let mut old_bytes: Option<Vec<u8>> = None;
            if let Some(old_ref) = old_ref {
                let keep = checked_usize(old_ref.len.min(chunk_len), "chunk keep length")?;
                if keep > 0 {
                    let stored = self.read_blob_verified_keyed(
                        BlobKind::Object,
                        old_ref.hash,
                        identity::CHUNK_DOMAIN,
                    )?;
                    self.note_range_write_chunk_read();
                    if stored.len() as u64 != old_ref.len {
                        return Err(Error::corrupt("chunk length disagrees with manifest"));
                    }
                    assembled[..keep].copy_from_slice(&stored[..keep]);
                    old_bytes = Some(stored);
                }
            }
            if let Some(overlapping) = overlapping {
                for &edit_index in overlapping {
                    edits[edit_index].apply(chunk_start, chunk_end, &mut assembled)?;
                }
            }
            if let (Some(old_ref), Some(old_bytes)) = (old_ref, old_bytes.as_deref()) {
                // An edit that reproduces the old bytes must not add a blob.
                if old_ref.len == chunk_len && assembled == old_bytes {
                    extent_chunks.push(old_ref.clone());
                    continue;
                }
            }
            changed = true;
            let hash = self.store_blob(BlobKind::Object, identity::CHUNK_DOMAIN, &assembled)?;
            extent_chunks.push(ChunkRef {
                hash,
                len: chunk_len,
            });
        }
        if !changed && new_size == old.size {
            return Ok(WriteOutcome {
                version: old.version,
                content: old.content,
                size: old.size,
            });
        }

        let extent_offset = span_first * chunk_size;
        let extent_length = (span_last * chunk_size
            + (new_size - span_last * chunk_size).min(chunk_size))
        .checked_sub(extent_offset)
        .ok_or_else(|| Error::corrupt("overlay extent length underflows"))?;
        let extent = OverlayExtent {
            offset: extent_offset,
            length: extent_length,
            chunks: extent_chunks,
        };
        extent.validate_shape(chunk_size, new_size)?;

        // The expected canonical table: the previous table with the extent's
        // span replaced.
        let span_first_usize = checked_usize(span_first, "extent start chunk")?;
        if span_first_usize > old_table.len() {
            return Err(Error::corrupt(
                "overlay extent leaves a gap after the base table",
            ));
        }
        let mut expected: Vec<ChunkRef> =
            Vec::with_capacity(old_table.len().max(span_first_usize + extent.chunks.len()));
        expected.extend_from_slice(&old_table[..span_first_usize]);
        expected.extend(extent.chunks.iter().cloned());
        let resume = checked_usize(span_last, "extent end chunk")? + 1;
        if resume < old_table.len() {
            expected.extend_from_slice(&old_table[resume..]);
        }
        let expected_total = expected
            .iter()
            .try_fold(0u64, |acc, chunk| acc.checked_add(chunk.len))
            .ok_or_else(|| Error::corrupt("derived chunk table size overflows"))?;
        if expected_total != new_size {
            return Err(Error::corrupt(format!(
                "derived overlay table sums to {expected_total}, new size is {new_size}"
            )));
        }
        for (index, chunk) in expected.iter().enumerate() {
            if index + 1 < expected.len() && chunk.len != chunk_size {
                return Err(Error::corrupt(
                    "derived overlay table has a short interior chunk",
                ));
            }
        }

        let mut extents = kept_extents;
        extents.push(extent);
        extents.sort_by_key(|item| item.offset);
        let extent_total = extents
            .iter()
            .try_fold(0u64, |acc, item| acc.checked_add(item.length))
            .ok_or_else(|| Error::corrupt("overlay byte total overflows"))?;
        let overlay_bytes = base_chain_bytes
            .checked_add(extent_total)
            .ok_or_else(|| Error::limit_exceeded("overlay byte accounting overflows"))?;
        if overlay_bytes > self.options.max_overlay_bytes {
            return Err(Error::limit_exceeded(format!(
                "overlay chain would carry {overlay_bytes} bytes, above the configured writer \
                 bound {}; run `weftfs compact <path>` to canonicalise the file",
                self.options.max_overlay_bytes
            )));
        }

        // Fail closed unless the manifest the reader will resolve is exactly the
        // table this writer derived.
        let base_manifest = if base_id == old.content {
            old_manifest.clone()
        } else {
            self.read_manifest_keyed(base_id)?
        };
        if base_manifest.content_hash != base_root || base_manifest.size != base_size {
            return Err(Error::corrupt(
                "overlay base disagrees with the previous version's declared base",
            ));
        }
        let base_table = self.canonical_chunk_table(base_id, &base_manifest)?;
        let derived = apply_overlay_extents(&base_table, chunk_size, &extents, new_size)?;
        if derived != expected {
            return Err(Error::corrupt(
                "overlay writer derived a different table than the reader will build",
            ));
        }
        let content_root =
            identity::merkle_root_from_chunks(expected.iter().map(|chunk| (chunk.len, chunk.hash)));

        let representation = Representation {
            kind: RepresentationKind::Overlay,
            frame: None,
            dependencies: Vec::new(),
            strategy: "overlay".to_owned(),
            decoded_size: new_size,
            tensor: None,
            overlay: Some(OverlayRepresentation {
                base: base_id,
                base_content_root: base_root,
                base_size,
                depth: new_depth,
                overlay_bytes,
                extents,
            }),
        };
        let manifest = Manifest {
            size: new_size,
            chunk_size,
            chunks: Vec::new(),
            content_hash: content_root,
            representation: Some(representation),
            identity: ContentIdentity::MerkleV3,
        };
        let (manifest_id, _content, _size) = self.store_manifest(manifest)?;
        self.overlay_tables
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(manifest_id, Arc::new(expected));
        let version = self.publish_file_version(id, old.clone(), manifest_id, new_size, causal)?;
        Ok(WriteOutcome {
            version,
            content: manifest_id,
            size: new_size,
        })
    }

    /// Content-addressed, crash-safe blob write. The backend publishes the
    /// fully written and fsynced temporary object all-or-nothing under its
    /// content key. An existing blob is verified, never overwritten.
    pub(crate) fn store_blob(&self, kind: BlobKind, domain: &str, bytes: &[u8]) -> Result<Hash32> {
        if self.mode != OpenMode::ReadWrite && !self.blob_worker {
            return Err(Error::read_only(
                "this handle may not publish blobs; use a writable store or a blob worker",
            ));
        }
        let hash = crate::phase_profile::timed(crate::phase_profile::Phase::Digest, || {
            identity::h(domain, bytes)
        });
        let key = match kind {
            BlobKind::Object => Key::object(hash),
            BlobKind::Manifest => Key::manifest(hash),
        };
        crate::phase_profile::timed(crate::phase_profile::Phase::BlobPublish, || {
            self.with_backend_mut(|backend| {
                let capabilities = backend.capabilities();
                capabilities.require_atomic_publish("blob publication")?;
                if bytes.len() as u64 > capabilities.max_object_bytes {
                    return Err(Error::limit_exceeded(format!(
                        "blob is {} bytes, above the backend's {}-byte bound",
                        bytes.len(),
                        capabilities.max_object_bytes
                    )));
                }
                match backend.read_all(&key) {
                    Ok(existing) => {
                        if identity::h(domain, &existing) != hash {
                            return Err(Error::corrupt(format!(
                                "existing blob {} does not match its name",
                                key.display_path()
                            )));
                        }
                        self.publication
                            .duplicate_blobs
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return Ok(hash);
                    }
                    Err(err) if err.class() == crate::error::Class::NotFound => {}
                    Err(err) => return Err(err),
                }
                match backend.put_atomic(&key, bytes) {
                    Ok(()) => {}
                    Err(err) if err.class() == Class::Exists => {
                        // At-least-once delivery (a retried or duplicated PUT)
                        // must be idempotent: the object is content-addressed,
                        // so an existing blob with the same bytes is success.
                        // A different body under the same name is corruption.
                        let existing = backend.read_all(&key)?;
                        if identity::h(domain, &existing) != hash {
                            return Err(Error::corrupt(format!(
                                "blob {} appeared concurrently with different bytes",
                                key.display_path()
                            )));
                        }
                        self.publication
                            .duplicate_blobs
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return Ok(hash);
                    }
                    Err(err) => return Err(err),
                }
                let (count, byte_count) = match kind {
                    BlobKind::Object => (
                        &self.publication.object_blobs,
                        &self.publication.object_blob_bytes,
                    ),
                    BlobKind::Manifest => (
                        &self.publication.manifest_blobs,
                        &self.publication.manifest_blob_bytes,
                    ),
                };
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                byte_count.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
                Ok(hash)
            })
        })
    }

    fn write_snapshot_pointer(&self, name: &str, commit: Hash32) -> Result<()> {
        let key = Key::snapshot_ref(name);
        let bytes = format!("{}\n", commit.to_hex());
        self.with_backend_mut(|backend| {
            if backend.exists(&key)? {
                return Err(Error::exists("snapshot pointer already exists"));
            }
            backend.put_atomic(&key, bytes.as_bytes())
        })
    }
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

/// The cost-model class used to price one object version's reconstruction.
///
/// The class is read from the representation the object actually uses, so a
/// cached base stored as a weight delta is priced with the delta row and a
/// plain zstd object with the zstd row. An unknown strategy falls back to the
/// conservative row for its representation kind instead of guessing a cheaper
/// one.
fn manifest_is_overlay(manifest: &Manifest) -> bool {
    manifest
        .representation
        .as_ref()
        .map(|rep| rep.kind == RepresentationKind::Overlay)
        .unwrap_or(false)
}

/// One byte edit applied to an overlay extent. `bytes == None` means `len`
/// zero bytes, so a large zero fill never materialises a large buffer.
struct OverlayEdit<'a> {
    offset: u64,
    len: u64,
    bytes: Option<&'a [u8]>,
}

impl<'a> OverlayEdit<'a> {
    fn bytes(offset: u64, bytes: &'a [u8]) -> Self {
        OverlayEdit {
            offset,
            len: bytes.len() as u64,
            bytes: Some(bytes),
        }
    }

    fn zeros(offset: u64, len: u64) -> Self {
        OverlayEdit {
            offset,
            len,
            bytes: None,
        }
    }

    fn end(&self) -> Result<u64> {
        self.offset
            .checked_add(self.len)
            .ok_or_else(|| Error::limit_exceeded("write offset overflows"))
    }

    /// Apply the part of this edit that falls inside `[chunk_start, chunk_end)`
    /// to `dst`, which is the chunk's buffer starting at `chunk_start`.
    fn apply(&self, chunk_start: u64, chunk_end: u64, dst: &mut [u8]) -> Result<()> {
        let start = self.offset.max(chunk_start);
        let end = self.end()?.min(chunk_end);
        if start >= end {
            return Ok(());
        }
        let dst_start = checked_usize(start - chunk_start, "chunk edit start")?;
        let dst_end = checked_usize(end - chunk_start, "chunk edit end")?;
        match self.bytes {
            Some(bytes) => {
                let src_start = checked_usize(start - self.offset, "edit source start")?;
                let src_end = checked_usize(end - self.offset, "edit source end")?;
                dst[dst_start..dst_end].copy_from_slice(&bytes[src_start..src_end]);
            }
            None => dst[dst_start..dst_end].fill(0),
        }
        Ok(())
    }
}

/// Apply overlay extents to a base chunk table exactly as the reader does:
/// each extent replaces the chunk references at its grid positions and may
/// append past the base end.
fn apply_overlay_extents(
    base: &[ChunkRef],
    chunk_size: u64,
    extents: &[OverlayExtent],
    size: u64,
) -> Result<Vec<ChunkRef>> {
    if chunk_size == 0 {
        return Err(Error::malformed("overlay chunk_size is out of range"));
    }
    let mut table = base.to_vec();
    for extent in extents {
        let start = checked_usize(extent.offset / chunk_size, "overlay extent index")?;
        if start > table.len() {
            return Err(Error::corrupt(
                "overlay extent leaves a gap after the base table",
            ));
        }
        let end = start
            .checked_add(extent.chunks.len())
            .ok_or_else(|| Error::corrupt("overlay extent chunk count overflows"))?;
        let old_len = table.len();
        if end > old_len {
            table.splice(start..old_len, extent.chunks.iter().cloned());
        } else {
            table.splice(start..end, extent.chunks.iter().cloned());
        }
    }
    let total = table
        .iter()
        .try_fold(0u64, |acc, chunk| acc.checked_add(chunk.len))
        .ok_or_else(|| Error::corrupt("overlay table size overflows"))?;
    if total != size {
        return Err(Error::corrupt(format!(
            "overlay table sums to {total}, declared size is {size}"
        )));
    }
    Ok(table)
}

fn manifest_cost_class(manifest: &Manifest) -> CostClass {
    let Some(rep) = manifest.representation.as_ref() else {
        return CostClass::Raw;
    };
    match rep.kind {
        RepresentationKind::Raw => CostClass::Raw,
        RepresentationKind::Overlay => CostClass::Raw,
        RepresentationKind::Tensor => match rep.tensor.as_ref().map(|t| t.tensor_mode) {
            Some(9) => CostClass::FloatTensor,
            Some(10) => CostClass::TensorSharded,
            Some(11) => CostClass::WeightDelta,
            Some(12) => CostClass::KvCache,
            Some(13) => CostClass::Activation,
            Some(14) => CostClass::SemanticDelta,
            Some(15) => CostClass::TensorTiled,
            Some(16) => CostClass::SparseDelta,
            _ => CostClass::parse(&rep.strategy).unwrap_or(CostClass::TensorSharded),
        },
        RepresentationKind::Codec => match rep.strategy.as_str() {
            "zstd" => CostClass::Zstd,
            // Codec strategies that name a base read the base first.
            "delta" | "reference" | "zstd-patch" => CostClass::WeightDelta,
            other => CostClass::parse(other).unwrap_or(CostClass::Zstd),
        },
    }
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
    if options.max_overlay_depth == 0
        || options.max_overlay_depth > crate::representation::MAX_OVERLAY_CHAIN_DEPTH
    {
        return Err(Error::invalid_argument(format!(
            "max_overlay_depth must be 1..={} (the normative reader bound)",
            crate::representation::MAX_OVERLAY_CHAIN_DEPTH
        )));
    }
    if options.max_overlay_bytes == 0 {
        return Err(Error::invalid_argument(
            "max_overlay_bytes must be positive",
        ));
    }
    // Merkle identity is chunk-based; a codec/tensor frame has no chunk table
    // for it to cover. Refuse the combination rather than silently ignoring
    // one of the two selections.
    if options.content_identity == ContentIdentity::MerkleV3
        && options.representation == RepresentationMode::CodecAuto
    {
        return Err(Error::invalid_argument(
            "content_identity `merkle` cannot be combined with representation `codec-auto`",
        ));
    }
    // Reject duplicate principals before any store I/O.
    normalize_policy(options.quotas.clone())?;
    SingleFlightOptions::from_options(options).validate()?;
    ObjectCacheOptions::from_options(options).validate()?;
    if options.cache_bytes > 0 && options.cache_entries == 0 {
        return Err(Error::invalid_argument(
            "cache_entries must be at least 1 when the decoded-object cache is enabled",
        ));
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

/// Remove reserved tensor identity attributes when an object stops being a
/// tensor representation (any ordinary byte rewrite).
fn strip_tensor_attrs(attrs: &mut BTreeMap<String, Value>) {
    attrs.retain(|key, _| !key.starts_with("weft.tensor."));
}

/// One op as the history records it: a canonical key, an optional value, and
/// the op kind. Purely observational — no store decision reads it.
struct HistoryOp {
    op: &'static str,
    key: String,
    value: Option<String>,
}

impl HistoryOp {
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({"op": self.op, "key": self.key, "value": self.value})
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn describe_pre_value(value: &crate::mw2::PreValue) -> serde_json::Value {
    match value {
        crate::mw2::PreValue::Absent => serde_json::json!({"kind": "absent"}),
        crate::mw2::PreValue::Hash(hash) => {
            serde_json::json!({"kind": "hash", "hash": hash.to_hex()})
        }
        crate::mw2::PreValue::Entry(record) => serde_json::json!({
            "kind": "entry",
            "entry": record.id.to_hex(),
            "target": record.target.to_hex(),
        }),
    }
}

/// Canonical key/value for every op in a transaction, in op order.
///
/// The keys are the registers the checker reasons about: `entry:` for a
/// directory name, `object:` for an object id, `relation:`, `snapshot:`, `tip:`.
/// A `del_entry` op carries only the entry id, so its key is recovered from the
/// op's own precondition (which holds the observed entry record).
fn transaction_history_ops(tx: &crate::mw2::Transaction) -> Vec<HistoryOp> {
    let mut out = Vec::with_capacity(tx.ops.len());
    for (index, op) in tx.ops.iter().enumerate() {
        let pre = tx.preconditions.get(index);
        let history_op = match op {
            Op::PutObject(record) => HistoryOp {
                op: "put_object",
                key: format!("object:{}", record.id.to_hex()),
                value: Some(format!(
                    "content:{}:{}:{}",
                    record.content.to_hex(),
                    record.size,
                    record.version.to_hex()
                )),
            },
            Op::DeleteObject(id) => HistoryOp {
                op: "delete_object",
                key: format!("object:{}", id.to_hex()),
                value: None,
            },
            Op::PutEntry(record) => HistoryOp {
                op: "put_entry",
                key: format!(
                    "entry:{}:{}",
                    record.parent.to_hex(),
                    hex_bytes(&record.name)
                ),
                value: Some(format!("target:{}", record.target.to_hex())),
            },
            Op::DelEntry(id) => {
                let key = match pre.map(|pre| &pre.expected) {
                    Some(crate::mw2::PreValue::Entry(record)) => format!(
                        "entry:{}:{}",
                        record.parent.to_hex(),
                        hex_bytes(&record.name)
                    ),
                    _ => format!("entry-id:{}", id.to_hex()),
                };
                HistoryOp {
                    op: "del_entry",
                    key,
                    value: None,
                }
            }
            Op::PutRelation(record) => HistoryOp {
                op: "put_relation",
                key: format!("relation:{}", record.id.to_hex()),
                value: Some(format!(
                    "assert:{}:{}:{}",
                    record.subject.to_hex(),
                    record.predicate,
                    record.kind.as_str()
                )),
            },
            Op::DelRelation(id) => HistoryOp {
                op: "del_relation",
                key: format!("relation:{}", id.to_hex()),
                value: None,
            },
            Op::SetSnapshot { name, commit } => HistoryOp {
                op: "set_snapshot",
                key: format!("snapshot:{name}"),
                value: Some(format!("commit:{}", commit.to_hex())),
            },
            Op::SetTip { object, version } => HistoryOp {
                op: "set_tip",
                key: format!("tip:{}", object.to_hex()),
                value: Some(format!("version:{}", version.to_hex())),
            },
            Op::SetQuotaUsage(_) => HistoryOp {
                op: "set_quota_usage",
                key: "quota".to_owned(),
                value: None,
            },
        };
        out.push(history_op);
    }
    out
}

/// Wall-clock nanoseconds for a handle identity (never a fixture timestamp).
fn self_wall_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn mark_state(
    store: &Store,
    state: &State,
    reach: &mut Reachability,
    origin: &str,
    problems: &mut Vec<String>,
) {
    for record in state.objects.values() {
        // A causal closure record is content-addressed under the manifest
        // domain and is retained exactly as long as the object that names it
        // (in current state, a snapshot, or a checkpoint).
        if let Some(Value::Bytes(bytes)) = record.attrs.get(crate::state::ATTR_CAUSAL_HASH) {
            if let Ok(hash) = Hash32::from_slice(bytes) {
                if hash != Hash32::ZERO {
                    reach.mark_manifest(hash, origin);
                }
            }
        }
        if record.kind != ObjectKind::File {
            continue;
        }
        let mut seen = BTreeSet::new();
        mark_manifest_tree(store, record.content, reach, origin, problems, 0, &mut seen);
    }

    // Authenticated capability envelopes are retained while their attachment
    // relation is retained. The relation names a content-addressed manifest
    // blob; an unreadable or malformed attachment is kept (fail safe) and
    // reported by `verify`.
    for relation in state.relations.values() {
        if relation.predicate != crate::state::CAPABILITY_ENVELOPE_PREDICATE {
            continue;
        }
        if let Some(Value::Str(text)) = relation
            .attrs
            .get(crate::state::CAPABILITY_ENVELOPE_BLOB_ATTR)
        {
            if let Ok(hash) = Hash32::from_hex(text) {
                reach.mark_manifest(hash, origin);
            }
        }
    }

    // Provenance receipts are retained exactly while an output object that
    // names them is retained. The receipt itself and every declared input and
    // output content root are marked, so recovery by `recompute` keeps the
    // verified inputs and the recorded outputs alive. Missing external inputs
    // (no local manifest) are skipped, not treated as errors.
    let mut provenance_seen: BTreeSet<Hash32> = BTreeSet::new();
    for relation in state.relations.values() {
        if relation.predicate != crate::provenance::PROVENANCE_PREDICATE {
            continue;
        }
        let id = match crate::provenance::provenance_id_from_relation(relation) {
            Ok(id) => id,
            Err(err) => {
                problems.push(format!(
                    "provenance relation {} has an invalid target: {err}",
                    relation.id
                ));
                continue;
            }
        };
        if !provenance_seen.insert(id) {
            continue;
        }
        // Fail safe: keep the record blob even if it cannot be parsed.
        reach.mark_manifest(id, origin);
        match store.with_backend(|backend| crate::provenance::read_provenance_via(backend, id)) {
            Ok(record) => {
                for input in &record.inputs {
                    let root = input
                        .object
                        .and_then(|object| state.objects.get(&object))
                        .map(|object| object.content)
                        .unwrap_or(input.content);
                    mark_content_root_if_present(store, root, reach, origin, problems);
                }
                for output in &record.outputs {
                    mark_content_root_if_present(store, *output, reach, origin, problems);
                }
            }
            Err(err) => problems.push(format!("cannot read provenance {id}: {err}")),
        }
    }
}

/// Mark a content root only when its manifest is present locally. Provenance
/// inputs may name external bytes; those are not GC roots in this store.
fn mark_content_root_if_present(
    store: &Store,
    root: Hash32,
    reach: &mut Reachability,
    origin: &str,
    problems: &mut Vec<String>,
) {
    if root == Hash32::ZERO {
        return;
    }
    match store.with_backend(|backend| backend.exists(&Key::manifest(root))) {
        Ok(true) => {
            let mut seen = BTreeSet::new();
            mark_manifest_tree(store, root, reach, origin, problems, 0, &mut seen);
        }
        Ok(false) => {}
        Err(err) => problems.push(format!("cannot stat manifest {root}: {err}")),
    }
}

/// Mark a manifest and everything it physically references: raw chunks, the
/// codec frame blob, and (transitively, bounded) dependency content roots.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mark_manifest_tree(
    store: &Store,
    manifest_id: Hash32,
    reach: &mut Reachability,
    origin: &str,
    problems: &mut Vec<String>,
    depth: usize,
    seen: &mut BTreeSet<Hash32>,
) {
    if depth as u64 > representation::MAX_OVERLAY_CHAIN_DEPTH {
        problems.push(format!(
            "manifest {manifest_id} exceeds the manifest-chain depth bound"
        ));
        return;
    }
    if !seen.insert(manifest_id) {
        problems.push(format!("manifest {manifest_id} participates in a cycle"));
        return;
    }
    reach.mark_manifest(manifest_id, origin);
    match store.read_manifest_keyed(manifest_id) {
        Ok(manifest) => {
            if let Some(rep) = &manifest.representation {
                if rep.kind == RepresentationKind::Codec || rep.kind == RepresentationKind::Tensor {
                    if depth > MAX_CODEC_DEPENDENCY_DEPTH {
                        problems.push(format!(
                            "manifest {manifest_id} exceeds the codec dependency depth bound"
                        ));
                        return;
                    }
                    if let Some(frame) = rep.frame {
                        reach.mark_manifest(frame, origin);
                    }
                    for dependency in &rep.dependencies {
                        mark_manifest_tree(
                            store,
                            *dependency,
                            reach,
                            origin,
                            problems,
                            depth + 1,
                            seen,
                        );
                    }
                    return;
                }
                if rep.kind == RepresentationKind::Overlay {
                    let Some(overlay) = rep.overlay.as_ref() else {
                        problems.push(format!(
                            "overlay manifest {manifest_id} is missing its overlay map"
                        ));
                        return;
                    };
                    for extent in &overlay.extents {
                        for chunk in &extent.chunks {
                            reach.mark_chunk(chunk.hash, origin);
                        }
                    }
                    mark_manifest_tree(
                        store,
                        overlay.base,
                        reach,
                        origin,
                        problems,
                        depth + 1,
                        seen,
                    );
                    return;
                }
            }
            for chunk in manifest.chunks {
                reach.mark_chunk(chunk.hash, origin);
            }
        }
        Err(err) => problems.push(format!("cannot read manifest {manifest_id}: {err}")),
    }
}

/// Collect advisory index keys whose commit is not in `retained`. Filenames
/// must be the canonical lowercase hex commit plus `.idx`; anything else is
/// reported and left untouched.
fn collect_unreachable_indexes(
    store: &Store,
    retained: &BTreeSet<Hash32>,
    now: SystemTime,
    grace_secs: u64,
    report: &mut GcReport,
) -> Result<Vec<(Key, u64)>> {
    let mut out = Vec::new();
    let keys = store.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::Index)))?;
    for key in keys {
        let commit = match key {
            Key::Index { commit } => commit,
            Key::Other { .. } => {
                report.problems.push(format!(
                    "unrecognized index filename {}",
                    key.display_path()
                ));
                continue;
            }
            _ => continue,
        };
        if retained.contains(&commit) {
            report.retained += 1;
            continue;
        }
        if grace_secs > 0 {
            let age = store
                .with_backend(|backend| backend.modified(&key))?
                .and_then(|modified| now.duration_since(modified).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            if age < grace_secs {
                report.retained += 1;
                continue;
            }
        }
        let len = match store.with_backend(|backend| backend.len(&key)) {
            Ok(len) => len,
            Err(err) => {
                report
                    .problems
                    .push(format!("cannot stat index {}: {err}", key.display_path()));
                continue;
            }
        };
        out.push((key, len));
    }
    Ok(out)
}

impl CollectUnmarkedCut {
    fn permits(&self, modified_ns: i64) -> bool {
        // Neither arm is a candidate: one is newer than the pass's acquisition
        // instant, the other is protected by an earlier live reservation.
        modified_ns < self.acquired_ns && modified_ns < self.floor_ns
    }
}

/// T024: the epoch cut a GC pass applies to unmarked blobs.
pub(crate) struct CollectUnmarkedCut {
    /// Instant the pass acquired its epoch.
    pub acquired_ns: i64,
    /// Earliest live reservation start taken at or before the acquisition.
    pub floor_ns: i64,
}

pub(crate) fn collect_unmarked(
    store: &Store,
    kind: KeyKind,
    marked: &BTreeSet<Hash32>,
    now: SystemTime,
    grace_secs: u64,
    cut: Option<&CollectUnmarkedCut>,
    report: &mut GcReport,
) -> Result<Vec<(Key, u64)>> {
    let mut out = Vec::new();
    let keys = store.with_backend(|backend| backend.list_prefix(&Key::prefix(kind)))?;
    for key in keys {
        let hash = match key {
            Key::Object { hash } | Key::Manifest { hash } => hash,
            Key::Other { .. } => {
                report
                    .problems
                    .push(format!("unrecognized blob filename {}", key.display_path()));
                continue;
            }
            _ => continue,
        };
        if marked.contains(&hash) {
            report.retained += 1;
            continue;
        }
        if let Some(cut) = cut {
            // The epoch cut is only usable on a plane that reports a real
            // modification time. A plane that cannot is refused by `gc` before
            // it gets here (see `Store::gc`), so the only `None` here is a
            // single-writer pass with no concurrent publisher.
            if let Some(modified) = store.with_backend(|backend| backend.modified(&key))? {
                if !cut.permits(system_time_ns(modified)) {
                    report.retained = report.retained.saturating_add(1);
                    if system_time_ns(modified) >= cut.acquired_ns {
                        report.retained_newer_than_cut =
                            report.retained_newer_than_cut.saturating_add(1);
                    } else {
                        report.retained_reservation_floor =
                            report.retained_reservation_floor.saturating_add(1);
                    }
                    continue;
                }
            }
        }
        if grace_secs > 0 {
            let age = store
                .with_backend(|backend| backend.modified(&key))?
                .and_then(|modified| now.duration_since(modified).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            if age < grace_secs {
                report.retained += 1;
                continue;
            }
        }
        let len = match store.with_backend(|backend| backend.len(&key)) {
            Ok(len) => len,
            Err(err) => {
                report
                    .problems
                    .push(format!("cannot stat blob {}: {err}", key.display_path()));
                continue;
            }
        };
        out.push((key, len));
    }
    Ok(out)
}

/// T024: one `SystemTime` as nanoseconds since the Unix epoch (negative before
/// it, clamped at `i64` bounds). Only used for the GC epoch cut.
pub(crate) fn system_time_ns(time: SystemTime) -> i64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
        Err(err) => i64::try_from(err.duration().as_nanos())
            .map(|value| -value)
            .unwrap_or(i64::MIN),
    }
}

// Keep the format constants referenced in this module's documentation visible
// to downstream users through the store crate.
pub use crate::format::{FORMAT_MAJOR as STORE_FORMAT_MAJOR, FORMAT_MINOR as STORE_FORMAT_MINOR};

impl Drop for Store {
    fn drop(&mut self) {
        // Advisory counters only; a failure here (for example a remote backend
        // that became unavailable) must never turn into a panic.
        if self.mode == OpenMode::ReadWrite {
            let _ = self.flush_access_counts();
        }
    }
}
