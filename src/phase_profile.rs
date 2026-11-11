//! Phase-level wall-clock profiler (subphase R10).
//!
//! The write path is a sequence of phases: read the source bytes, hash them,
//! encode a representation, publish immutable blobs, publish a commit, write an
//! advisory index segment, verify. End-to-end wall time alone cannot say which
//! of those dominates, and the answer is different for a planned encode and a
//! fast raw save. This module records an *opt-in* split of that time.
//!
//! # Contract
//!
//! * **Off by default.** [`enabled`] is false unless a caller (a court harness)
//!   called [`begin`] or set `WEFT_PHASE_PROFILE=1`. Every instrumented site
//!   pays one relaxed atomic load when profiling is off and allocates nothing.
//! * **Wall clock only.** A section is timed with [`std::time::Instant`] around
//!   the real code; nothing is estimated, and no counter is incremented for a
//!   section that did not run.
//! * **Thread-safe and additive.** The parallel checkpoint path measures from
//!   several threads; every thread appends to one process-wide collector under a
//!   short mutex. Totals are sums across threads, so the sum of a phase's
//!   `total_ns` can exceed wall time when several workers run it concurrently.
//!   The report always says so.
//! * **Bounded.** At most [`MAX_SAMPLES_PER_PHASE`] durations are retained per
//!   phase for percentiles; beyond that only count/total/max grow.
//! * **Not a wire format.** Nothing here is persisted, hashed, or referenced by
//!   a manifest, commit, or receipt schema. A profiling run publishes exactly
//!   the bytes a non-profiling run publishes.
//!
//! # What the phases cover
//!
//! | phase | instrumented site |
//! | --- | --- |
//! | `source_read` | `SafetensorsFile::read_tensor_shard`, `read_tensor_range`, harness source reads |
//! | `digest` | domain hashing inside `Store::store_blob`, `identity::content_hash` in the tensor writer, streamed tensor digests |
//! | `encode` | `Store::prepare_candidates` (planner candidates and the chosen encoder) |
//! | `blob_publish` | the backend `put_atomic` inside `Store::store_blob` (and the existence probe before it) |
//! | `manifest_commit` | `Store::publish_commit` (commit record + superblock publication) |
//! | `index_segment` | `Store::index_attestation_for` and `Store::persist_built_index` |
//! | `verify` | `Store::verify` and committer fragment verification |
//! | `store_open` | the whole `Store::open_with_options_and_backend` call at a call site |
//! | `state_replay` | `recovery::recover*` inside that open (nested in `store_open`) |
//! | `plane_probe` | `StorageBackend::probe_planes` inside a writable open (nested) |
//! | `index_build` | `StateIndex::build` inside `index_attestation_for` (nested) |
//! | `index_load` | `Store::load_attested_chain` inside `index_attestation_for` (nested) |
//! | `index_attest` | `encode_full`/`encode_delta` and the chain roots (nested) |
//! | `metadata_commit` | the store commit that publishes a small metadata file |
//! | `encode_zstd` / `encode_zstd19` / `encode_float_tensor` | one candidate encoder call inside `encode` (nested) |
//!
//! The phases are deliberately not a partition of the wall time: chunking,
//! planner cost arithmetic, in-memory state application, and the harness's own
//! bookkeeping are outside every section. A report therefore states the
//! measured total per phase and leaves the residual to the caller rather than
//! inventing an "other" bucket. Four phases are explicitly **nested** inside
//! another (`state_replay` and `plane_probe` in `store_open`; `index_build`,
//! `index_load` and `index_attest` in `index_segment`) and are labelled by
//! [`Phase::nested_in`] so a reader never adds them to a wall time twice.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use serde::Serialize;

/// Durations retained per phase for percentile reporting.
pub const MAX_SAMPLES_PER_PHASE: usize = 250_000;

/// One measured section of the write or read path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Reading tensor bytes out of the source container.
    SourceRead,
    /// Hashing bytes (content root, domain-separated blob id, streamed digest).
    Digest,
    /// Building and running representation encoders.
    Encode,
    /// Publishing immutable blobs to the backend.
    BlobPublish,
    /// Publishing the commit record and the superblock slot.
    ManifestCommit,
    /// Computing or writing the advisory selector index segment.
    IndexSegment,
    /// Verifying objects, fragments, or a whole store.
    Verify,
    /// One whole `Store::open_with_options_and_backend` call.
    StoreOpen,
    /// Commit replay inside a store open (nested in [`Phase::StoreOpen`]).
    StateReplay,
    /// Capability probing of the blob and commit planes (nested in
    /// [`Phase::StoreOpen`]).
    PlaneProbe,
    /// `StateIndex::build` (nested in [`Phase::IndexSegment`]).
    IndexBuild,
    /// Resolving the previous attested index chain (nested in
    /// [`Phase::IndexSegment`]).
    IndexLoad,
    /// Encoding the full/delta segment and deriving its roots (nested in
    /// [`Phase::IndexSegment`]).
    IndexAttest,
    /// The store commit that publishes one small metadata file.
    MetadataCommit,
    /// The level-3 ZSTD frame encode (nested in [`Phase::Encode`]).
    EncodeZstd,
    /// The level-19 ZSTD frame encode (nested in [`Phase::Encode`]).
    EncodeZstd19,
    /// A mode-9 FLOAT_TENSOR encode (nested in [`Phase::Encode`]).
    EncodeFloatTensor,
    /// The mode-14 semantic-candidate search (nested in [`Phase::Encode`]).
    EncodeSemantic,
}

impl Phase {
    /// Stable label used in receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::SourceRead => "source_read",
            Phase::Digest => "digest",
            Phase::Encode => "encode",
            Phase::BlobPublish => "blob_publish",
            Phase::ManifestCommit => "manifest_commit",
            Phase::IndexSegment => "index_segment",
            Phase::Verify => "verify",
            Phase::StoreOpen => "store_open",
            Phase::StateReplay => "state_replay",
            Phase::PlaneProbe => "plane_probe",
            Phase::IndexBuild => "index_build",
            Phase::IndexLoad => "index_load",
            Phase::IndexAttest => "index_attest",
            Phase::MetadataCommit => "metadata_commit",
            Phase::EncodeZstd => "encode_zstd",
            Phase::EncodeZstd19 => "encode_zstd19",
            Phase::EncodeFloatTensor => "encode_float_tensor",
            Phase::EncodeSemantic => "encode_semantic",
        }
    }

    /// The enclosing phase when this one is measured inside another.
    ///
    /// A receipt must never add a nested phase to its parent as if the two were
    /// disjoint; this is the machine-readable statement of the nesting.
    pub fn nested_in(self) -> Option<Phase> {
        match self {
            Phase::StateReplay | Phase::PlaneProbe => Some(Phase::StoreOpen),
            Phase::IndexBuild | Phase::IndexLoad | Phase::IndexAttest => Some(Phase::IndexSegment),
            Phase::EncodeZstd
            | Phase::EncodeZstd19
            | Phase::EncodeFloatTensor
            | Phase::EncodeSemantic => Some(Phase::Encode),
            _ => None,
        }
    }

    /// Every phase, in report order.
    pub const ALL: [Phase; 18] = [
        Phase::SourceRead,
        Phase::Digest,
        Phase::Encode,
        Phase::BlobPublish,
        Phase::ManifestCommit,
        Phase::IndexSegment,
        Phase::Verify,
        Phase::StoreOpen,
        Phase::StateReplay,
        Phase::PlaneProbe,
        Phase::IndexBuild,
        Phase::IndexLoad,
        Phase::IndexAttest,
        Phase::MetadataCommit,
        Phase::EncodeZstd,
        Phase::EncodeZstd19,
        Phase::EncodeFloatTensor,
        Phase::EncodeSemantic,
    ];
}

/// Aggregated durations for one phase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PhaseTotals {
    /// Number of measured sections.
    pub count: u64,
    /// Sum of every section duration.
    pub total_ns: u128,
    /// Longest single section.
    pub max_ns: u64,
    /// Retained samples (bounded by [`MAX_SAMPLES_PER_PHASE`]).
    #[serde(skip)]
    pub samples: Vec<u64>,
}

impl PhaseTotals {
    /// Mean section duration in nanoseconds, or zero when nothing was measured.
    pub fn mean_ns(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.count as f64
        }
    }

    /// Nearest-rank percentile of the retained samples (`quantile` in `0..=1`).
    pub fn percentile_ns(&self, quantile: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let clamped = quantile.clamp(0.0, 1.0);
        let rank = ((sorted.len() as f64 - 1.0) * clamped).round() as usize;
        sorted[rank.min(sorted.len() - 1)]
    }
}

/// A point-in-time copy of the collector, cheap enough to take per tensor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PhaseSnapshot {
    /// Totals keyed by [`Phase::as_str`].
    pub phases: BTreeMap<&'static str, PhaseTotals>,
}

impl PhaseSnapshot {
    /// This snapshot minus `earlier`, for one tensor's attribution.
    ///
    /// Counters that do not increase yield an empty entry, so a diff never
    /// invents a section.
    pub fn since(&self, earlier: &PhaseSnapshot) -> PhaseSnapshot {
        let mut phases = BTreeMap::new();
        for (name, later) in &self.phases {
            let before = earlier.phases.get(name).cloned().unwrap_or_default();
            if later.count <= before.count {
                continue;
            }
            let count = later.count - before.count;
            let total_ns = later.total_ns.saturating_sub(before.total_ns);
            let max_ns = if later.max_ns >= before.max_ns {
                // The per-section maximum cannot be recovered exactly from
                // aggregates, so a diff reports the later maximum and marks the
                // total/count as the authoritative figures.
                later.max_ns
            } else {
                0
            };
            let skip = before.samples.len().min(later.samples.len());
            let samples = later.samples.iter().skip(skip).copied().collect();
            phases.insert(
                *name,
                PhaseTotals {
                    count,
                    total_ns,
                    max_ns,
                    samples,
                },
            );
        }
        PhaseSnapshot { phases }
    }

    /// Total measured milliseconds across every phase.
    pub fn total_ms(&self) -> f64 {
        self.phases
            .values()
            .map(|totals| totals.total_ns as f64)
            .sum::<f64>()
            / 1_000_000.0
    }

    /// Render the snapshot as a receipt-ready JSON object.
    ///
    /// Each phase carries count, total/mean/max/p50/p95/p99 milliseconds.
    pub fn to_json(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for phase in Phase::ALL {
            let Some(totals) = self.phases.get(phase.as_str()) else {
                continue;
            };
            if totals.count == 0 {
                continue;
            }
            out.insert(
                phase.as_str().to_owned(),
                serde_json::json!({
                    "sections": totals.count,
                    "nested_in": phase.nested_in().map(|parent| parent.as_str()),
                    "total_ms": totals.total_ns as f64 / 1_000_000.0,
                    "mean_ms": totals.mean_ns() / 1_000_000.0,
                    "max_ms": totals.max_ns as f64 / 1_000_000.0,
                    "p50_ms": totals.percentile_ns(0.50) as f64 / 1_000_000.0,
                    "p95_ms": totals.percentile_ns(0.95) as f64 / 1_000_000.0,
                    "p99_ms": totals.percentile_ns(0.99) as f64 / 1_000_000.0,
                }),
            );
        }
        serde_json::Value::Object(out)
    }
}

struct Collector {
    phases: BTreeMap<&'static str, PhaseTotals>,
}

fn collector() -> &'static Mutex<Collector> {
    static CELL: OnceLock<Mutex<Collector>> = OnceLock::new();
    CELL.get_or_init(|| {
        Mutex::new(Collector {
            phases: BTreeMap::new(),
        })
    })
}

fn lock() -> MutexGuard<'static, Collector> {
    collector()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_enabled() -> bool {
    static CELL: OnceLock<bool> = OnceLock::new();
    *CELL.get_or_init(|| {
        std::env::var("WEFT_PHASE_PROFILE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// True while a profiler run is active.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) || env_enabled()
}

/// Clear the collector and begin profiling.
///
/// Harnesses call this once per measured phase. Calling it twice discards the
/// first run's totals; that is deliberate, because two phases with the same
/// label must not be silently summed.
pub fn begin() {
    lock().phases.clear();
    ENABLED.store(true, Ordering::Release);
}

/// Stop profiling and return the totals collected so far.
///
/// The collector is cleared, so a later [`begin`] starts from zero. Calling
/// `finish` while profiling is off returns whatever the last run left behind
/// (normally an empty map).
pub fn finish() -> PhaseSnapshot {
    ENABLED.store(false, Ordering::Release);
    let mut guard = lock();
    let phases = std::mem::take(&mut guard.phases);
    PhaseSnapshot { phases }
}

/// Copy the current totals without clearing them. Cheap: one short lock and a
/// map clone (bounded samples included).
pub fn snapshot() -> PhaseSnapshot {
    let guard = lock();
    PhaseSnapshot {
        phases: guard.phases.clone(),
    }
}

/// Record one duration. Public so a harness can attribute its own source read.
pub fn record(phase: Phase, duration_ns: u64) {
    let mut guard = lock();
    let totals = guard.phases.entry(phase.as_str()).or_default();
    totals.count = totals.count.saturating_add(1);
    totals.total_ns = totals.total_ns.saturating_add(duration_ns as u128);
    totals.max_ns = totals.max_ns.max(duration_ns);
    if totals.samples.len() < MAX_SAMPLES_PER_PHASE {
        totals.samples.push(duration_ns);
    }
}

/// A running section. Dropping it records its duration when profiling is on.
#[must_use = "the section is only recorded when the guard is dropped"]
pub struct PhaseGuard {
    phase: Phase,
    started: Instant,
    active: bool,
}

impl PhaseGuard {
    /// Start timing `phase`, or a no-op guard when profiling is off.
    #[inline]
    pub fn enter(phase: Phase) -> PhaseGuard {
        let active = enabled();
        PhaseGuard {
            phase,
            started: Instant::now(),
            active,
        }
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if self.active {
            let elapsed = self.started.elapsed().as_nanos();
            record(self.phase, elapsed.min(u64::MAX as u128) as u64);
        }
    }
}

/// Convenience: `let _guard = phase_profile::scope(Phase::Digest);`
#[inline]
pub fn scope(phase: Phase) -> PhaseGuard {
    PhaseGuard::enter(phase)
}

/// Time an arbitrary closure as `phase`.
///
/// Unlike [`scope`] this never holds a guard across an `await`-like boundary;
/// it is simply `let _g = scope(p); f()`.
#[inline]
pub fn timed<T>(phase: Phase, f: impl FnOnce() -> T) -> T {
    let _guard = PhaseGuard::enter(phase);
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The collector is process-wide by design, so these tests must not run
    // concurrently with each other.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn disabled_by_default_and_off_for_the_program() {
        let _serial = serial();
        // No harness called `begin` in a unit test process.
        if std::env::var("WEFT_PHASE_PROFILE").ok().as_deref() != Some("1") {
            assert!(!enabled());
            {
                let _guard = scope(Phase::Encode);
            }
            assert!(finish().phases.is_empty());
        }
    }

    #[test]
    fn begin_records_and_finish_clears() {
        let _serial = serial();
        begin();
        {
            let _guard = scope(Phase::Digest);
        }
        {
            let _guard = scope(Phase::Digest);
        }
        let snapshot = finish();
        let totals = snapshot.phases.get("digest").expect("digest recorded");
        assert_eq!(totals.count, 2);
        assert!(totals.total_ns > 0);
        assert!(!enabled());
        assert!(finish().phases.is_empty());
    }

    #[test]
    fn diff_attributes_only_new_sections() {
        let _serial = serial();
        begin();
        {
            let _guard = scope(Phase::BlobPublish);
        }
        let first = snapshot();
        {
            let _guard = scope(Phase::BlobPublish);
            let _guard2 = scope(Phase::ManifestCommit);
        }
        let second = snapshot();
        let diff = second.since(&first);
        assert_eq!(diff.phases.get("blob_publish").unwrap().count, 1);
        assert_eq!(diff.phases.get("manifest_commit").unwrap().count, 1);
        assert_eq!(diff.phases.get("digest"), None);
        let _ = finish();
    }

    #[test]
    fn samples_are_bounded_and_percentiles_are_sane() {
        let _serial = serial();
        let mut totals = PhaseTotals::default();
        for value in 1..=10u64 {
            totals.count += 1;
            totals.total_ns += value as u128;
            totals.max_ns = totals.max_ns.max(value);
            totals.samples.push(value);
        }
        assert_eq!(totals.percentile_ns(0.5), 6);
        assert_eq!(totals.percentile_ns(1.0), 10);
        assert_eq!(totals.percentile_ns(0.0), 1);
    }
}
