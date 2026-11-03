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
pub mod conformance;
pub mod context;
pub mod error;
pub mod evaluate;
pub mod format;
#[cfg(feature = "fuse")]
pub mod fuse;
pub mod identity;
pub mod recovery;
pub mod state;
pub mod store;
pub mod timeparse;
pub mod wcb_util;

pub use auth::Principal;
pub use context::{CapabilityContext, ContextResolution, MaterializationReport, StoredContextRoot};
pub use error::{Class, Error, Result};
pub use evaluate::{Evidence, EvidenceEntry, Resolution, ResolvedRef};
pub use identity::Hash32;
pub use state::{ObjectKind, ObjectRecord, RelationRecord, RelationTarget};
pub use store::{
    GcReport, LineageEntry, OpenMode, Options, SavedContext, SetAttr, StatFs, Store, VerifyReport,
};
