//! WeftFS v1 transactional store core.
//!
//! This crate implements the portable on-disk store from `spec/WEFT-MODEL.md`
//! and the WeftQL resolver from `spec/WEFT-SELECTORS.md`, without a FUSE
//! frontend. The FUSE layer (P5) is expected to call these APIs.
//!
//! * [`store::Store`] — init/open, atomic commits, object I/O, snapshots.
//! * [`state`] — canonical records, derived identities, replay ops.
//! * [`format`] — superblock, commit, manifest, and checkpoint byte formats.
//! * [`recovery`] — parent-chain replay, torn-tail handling, catalog checks.
//! * [`evaluate`] — authorized selector evaluation and `require_one`.
//! * [`auth`] — POSIX mode + `weft.acl.read` principals.
//! * [`conformance`] — deterministic WeftFS-Go handoff fixture.
//!
//! All parsing of untrusted files returns `Result`. On-disk bytes are canonical
//! WCB; `serde` is used only for CLI JSON.
//!
//! # `unsafe`
//!
//! The crate is `#![deny(unsafe_code)]`. Exactly four modules opt out:
//! [`threefs::ffi`], the 3FS USRBIO C-ABI boundary,
//! `checkpoint_service::ancillary`, the `recvmsg`/`sendmsg` descriptor-passing
//! boundary of the persistent checkpoint service, [`writer_lock`], the
//! `flock(2)`/`fcntl(2)` `F_OFD_SETLK` writer-lock boundary (each transcribes a
//! syscall boundary with the invariant list at the top of the file), and the
//! single inert `raise(SIGSTOP)` court hook in `fence_pause`. Every other
//! module is safe Rust, and `tests/threefs.rs` grows this into a failure if an
//! `unsafe` block appears anywhere else in `src/`.

#![deny(unsafe_code)]

pub mod access;
pub mod auth;
pub mod azure;
pub mod b64;
pub mod backend;
pub mod calibration;
pub mod checkpoint;
pub mod checkpoint_service;
pub mod conformance;
pub mod context;
pub mod cost_model;
pub mod error;
pub mod evaluate;
pub(crate) mod fence_pause;
pub mod format;
#[cfg(feature = "fuse")]
pub mod fuse;
pub mod gcs;
pub mod identity;
pub(crate) mod index;
pub(crate) mod index_chain;
pub(crate) mod index_incremental;
pub mod kv_capability;
pub mod kv_prefix;
pub mod load_attestation;
pub mod load_receipt;
pub mod lock_probe;
pub mod mw2;
pub mod object_cache;
pub mod packs;
pub mod parallel;
pub mod phase_profile;
pub mod plan_graph;
pub mod planes;
pub mod provenance;
pub mod quota;
pub mod read_pool;
pub mod rebase;
pub mod receipt_bundle;
pub mod recovery;
pub mod recovery_first;
pub mod representation;
pub mod restore_pipeline;
pub mod retention;
pub mod s3;
pub mod safetensors;
pub mod service_account;
pub mod singleflight;
pub mod state;
pub mod store;
pub mod tensor_store;
pub mod threefs;
pub mod timeparse;
pub mod training_ready;
pub mod transcode;
pub mod two_stage;
pub mod wcb_util;
pub mod writer_fence;
pub mod writer_lock;

pub use auth::Principal;
pub use backend::{
    BackendKind, BackendStats, Capabilities, CompositeBackend, HeadExpectation, Key, KeyKind,
    StorageBackend,
};
pub use calibration::{
    Calibration, CalibrationMethod, CalibrationRow, Divergence, HostIdentity, MeasuredClass,
    MeasurementOptions, MeasurementOutcome, RowOrigin,
};
pub use checkpoint::{
    CheckpointManifest, CheckpointRestoreOptions, CheckpointRestoreReport, CheckpointShard,
    CheckpointTensor, CheckpointWriteOptions, CheckpointWriteReport, RestoreTopology,
    ShardStrategy, WrittenTensor,
};
pub use context::{CapabilityContext, ContextResolution, MaterializationReport, StoredContextRoot};
pub use cost_model::{CostClass, CostModelKind, CostTable};
pub use error::{Class, Error, Result};
pub use evaluate::{Evidence, EvidenceEntry, Resolution, ResolvedRef};
pub use identity::Hash32;
pub use load_receipt::{
    LoadReceipt, LoadReceiptOptions, LoadReceiptTensor, LoadVerification, LOAD_RECEIPT_SCHEMA,
    LOAD_SET_DOMAIN,
};
pub use lock_probe::{LockProbeReport, SectionSummary, TensorLockRow, ThreadProbeRow};
pub use object_cache::{CacheKey, CacheScore, ObjectCacheOptions, ObjectCacheStats};
pub use packs::{PackOptions, PackReclaim, PackStats, PackedBlobPlane};
pub use parallel::{
    effective_threads, ParallelReadOptions, ParallelReadStats, TileDecodeOptions, TileDecodeStats,
    DEFAULT_MAX_INFLIGHT_BYTES,
};
pub use phase_profile::{Phase, PhaseGuard, PhaseSnapshot, PhaseTotals};
pub use plan_graph::{
    EdgeCost, ParentGraphPlan, PlanOptions as ParentGraphPlanOptions, Solver as ParentGraphSolver,
    VersionCost, VersionGraph,
};
pub use planes::{
    BlobCapabilities, BlobPlane, BlobProbe, CommitCapabilities, CommitHead, CommitPlane,
    CommitProbe, ControlRecord, PlaneCapabilities, PlaneProbes, PlaneStats,
};
pub use provenance::{
    choose_recovery, Determinism, NoEligibleStrategy, ProvenanceClosure, ProvenanceEnvironment,
    ProvenanceInput, ProvenanceNode, ProvenancePrincipal, ProvenanceRecord, RecoveryCandidate,
    RecoveryChoice, RecoveryCosts, RecoveryRejection, RecoveryStrategy,
};
pub use read_pool::{ReadHandlePool, ReadPoolStats};
pub use rebase::{
    RebaseCandidate, RebaseOptions, RebaseOutcome, RebasePlan, RebaseReport, RebaseSkip,
};
pub use recovery_first::{
    CachedTensor, RecoveryFirstOptions, RecoveryFirstReport, RECOVERY_CACHE_SCHEMA,
};
pub use restore_pipeline::{run_ordered_pipeline, RestorePipelineOptions, RestorePipelineStats};
pub use retention::{
    RetainedArtifact, RetentionBinding, RetentionCandidate, RetentionClass, RetentionClassSummary,
    RetentionGcPlan, RetentionPointer, RetentionRequest, RetentionSignature,
};
pub use s3::{S3Backend, S3Config};
pub use singleflight::{
    CancelToken, ReconstructionKey, RegionKey, SingleFlightOptions, SingleFlightStats,
};
pub use state::{ObjectKind, ObjectRecord, RelationRecord, RelationTarget};
pub use store::{
    AuthorizedRead, GcReport, LineageEntry, OpenMode, Options, PublicationLedger, ReadLayerStats,
    RepresentationMode, SavedContext, SetAttr, StatFs, Store, VerifyReport,
};
pub use tensor_store::{
    FastCodec, ResolvedWritePolicy, StoredTensorMetadata, TensorIndependentPlan, TensorLayoutSpec,
    TensorPlan, TensorPlanCandidate, TensorPlanOptions, TensorRebase, TensorRebaseDecision,
    TensorStrategy, TensorWriteSpec, WritePolicy,
};
pub use training_ready::{
    verify_training_ready_output, PreconditionReport, TrainingReadyCheckpoint, TrainingReadyCost,
    TrainingReadyFile, TrainingReadyOptions, TrainingReadyReport, TrainingReadyTensor,
    TrainingReadyVerification,
};
pub use transcode::{
    TranscodeCandidate, TranscodeOptions, TranscodeOutcome, TranscodeReport, TranscodeSkip,
};
pub use writer_fence::PublishedLock;
