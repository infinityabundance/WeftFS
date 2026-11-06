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
//! All parsing of untrusted files returns `Result`; the crate forbids unsafe
//! code. On-disk bytes are canonical WCB; `serde` is used only for CLI JSON.

#![forbid(unsafe_code)]

pub mod auth;
pub mod backend;
pub mod checkpoint;
pub mod conformance;
pub mod context;
pub mod error;
pub mod evaluate;
pub mod format;
#[cfg(feature = "fuse")]
pub mod fuse;
pub mod identity;
pub(crate) mod index;
pub mod provenance;
pub mod quota;
pub mod receipt_bundle;
pub mod recovery;
pub mod representation;
pub mod s3;
pub mod safetensors;
pub mod state;
pub mod store;
pub mod tensor_store;
pub mod timeparse;
pub mod training_ready;
pub mod wcb_util;

pub use auth::Principal;
pub use backend::{BackendKind, BackendStats, Capabilities, Key, KeyKind, StorageBackend};
pub use checkpoint::{
    CheckpointManifest, CheckpointRestoreOptions, CheckpointRestoreReport, CheckpointShard,
    CheckpointTensor, CheckpointWriteOptions, CheckpointWriteReport, RestoreTopology,
    ShardStrategy, WrittenTensor,
};
pub use context::{CapabilityContext, ContextResolution, MaterializationReport, StoredContextRoot};
pub use error::{Class, Error, Result};
pub use evaluate::{Evidence, EvidenceEntry, Resolution, ResolvedRef};
pub use identity::Hash32;
pub use provenance::{
    choose_recovery, Determinism, NoEligibleStrategy, ProvenanceClosure, ProvenanceEnvironment,
    ProvenanceInput, ProvenanceNode, ProvenancePrincipal, ProvenanceRecord, RecoveryCandidate,
    RecoveryChoice, RecoveryCosts, RecoveryRejection, RecoveryStrategy,
};
pub use s3::{S3Backend, S3Config};
pub use state::{ObjectKind, ObjectRecord, RelationRecord, RelationTarget};
pub use store::{
    GcReport, LineageEntry, OpenMode, Options, RepresentationMode, SavedContext, SetAttr, StatFs,
    Store, VerifyReport,
};
pub use tensor_store::{
    StoredTensorMetadata, TensorLayoutSpec, TensorPlan, TensorPlanCandidate, TensorPlanOptions,
    TensorStrategy, TensorWriteSpec,
};
pub use training_ready::{
    verify_training_ready_output, PreconditionReport, TrainingReadyCheckpoint, TrainingReadyCost,
    TrainingReadyFile, TrainingReadyOptions, TrainingReadyReport, TrainingReadyTensor,
    TrainingReadyVerification,
};
