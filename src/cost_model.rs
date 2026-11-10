//! Decode cost model shared by the tensor planner, the rebase planner, and the
//! Go implementation in `WeftFS-Go/store/cost_model.go`.
//!
//! Every representation is priced with an affine model
//!
//! ```text
//! estimated_decode_ns = fixed_ns + decoded_bytes * ns_per_mib / 1_048_576
//! ```
//!
//! Two coefficient tables exist:
//!
//! * [`CostModelKind::Declared`] — the hand-written linear model shipped before
//!   R1. It is kept only so historical comparisons can be reproduced and so a
//!   `--planner-cost-model=declared` run is possible.
//! * [`CostModelKind::Measured`] — a table calibrated in the WeftFS bench court
//!   with `examples/calibrate_decode.rs` (median of 7 repetitions on 16 MiB and
//!   1 MiB payloads). It is host-calibrated, not a portable constant: a
//!   different CPU, court image, or allocator will shift it. The calibration
//!   receipt records the exact revision, image, kernel, and limits.
//!
//! The arithmetic is integer-only with the same operation order in Rust and Go,
//! so both languages produce bit-identical estimates.

/// Which coefficient table prices a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostModelKind {
    /// Historical hand-written linear estimate.
    Declared,
    /// Host-calibrated table from the bench court.
    Measured,
}

impl CostModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CostModelKind::Declared => "declared",
            CostModelKind::Measured => "measured",
        }
    }

    pub fn parse(text: &str) -> crate::Result<Self> {
        match text {
            "declared" => Ok(CostModelKind::Declared),
            "measured" => Ok(CostModelKind::Measured),
            other => Err(crate::Error::invalid_argument(format!(
                "planner cost model `{other}` is not `declared|measured`"
            ))),
        }
    }
}

impl Default for CostModelKind {
    fn default() -> Self {
        CostModelKind::Measured
    }
}

/// One representation class in the cost table.
///
/// The names are the canonical planner strategy names. `FloatTensor` is the
/// R1 addition; `TensorTiled` prices a full-frame decode (region reads are
/// cheaper and are reported separately by the tiled court).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostClass {
    Raw,
    Zstd,
    FloatTensor,
    TensorSharded,
    TensorTiled,
    WeightDelta,
    SparseDelta,
    SemanticDelta,
    KvCache,
    Activation,
}

impl CostClass {
    /// Every class in canonical order. The on-disk calibration record must
    /// carry exactly one row for each of these.
    pub const ALL: [CostClass; 10] = [
        CostClass::Raw,
        CostClass::Zstd,
        CostClass::FloatTensor,
        CostClass::TensorSharded,
        CostClass::TensorTiled,
        CostClass::WeightDelta,
        CostClass::SparseDelta,
        CostClass::SemanticDelta,
        CostClass::KvCache,
        CostClass::Activation,
    ];

    /// Position in [`CostClass::ALL`]. Used as the index into a [`CostTable`].
    pub fn index(self) -> usize {
        CostClass::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .expect("every CostClass is listed in CostClass::ALL")
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CostClass::Raw => "raw",
            CostClass::Zstd => "zstd",
            CostClass::FloatTensor => "float-tensor",
            CostClass::TensorSharded => "tensor-sharded",
            CostClass::TensorTiled => "tensor-tiled",
            CostClass::WeightDelta => "weight-delta",
            CostClass::SparseDelta => "sparse-delta",
            CostClass::SemanticDelta => "semantic-delta",
            CostClass::KvCache => "kv-cache",
            CostClass::Activation => "activation",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "raw" => CostClass::Raw,
            "zstd" => CostClass::Zstd,
            "float-tensor" => CostClass::FloatTensor,
            "tensor-sharded" => CostClass::TensorSharded,
            "tensor-tiled" => CostClass::TensorTiled,
            "weight-delta" => CostClass::WeightDelta,
            "sparse-delta" => CostClass::SparseDelta,
            "semantic-delta" => CostClass::SemanticDelta,
            "kv-cache" => CostClass::KvCache,
            "activation" => CostClass::Activation,
            _ => return None,
        })
    }
}

/// One affine row: a per-call fixed cost plus a per-MiB term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coefficients {
    pub fixed_ns: u64,
    pub ns_per_mib: u64,
}

impl Coefficients {
    const fn new(fixed_ns: u64, ns_per_mib: u64) -> Self {
        Coefficients {
            fixed_ns,
            ns_per_mib,
        }
    }
}

/// Historical hand-written estimate, expressed as an affine table.
///
/// The original formulas were `size/200`, `size`, `size*3`, `size*4`; here
/// `1_048_576/200` is rounded to `5_243` ns/MiB so the same integer arithmetic
/// can run in both languages.
const DECLARED: [(CostClass, Coefficients); 10] = [
    (CostClass::Raw, Coefficients::new(20_000, 5_243)),
    (CostClass::Zstd, Coefficients::new(50_000, 1_048_576)),
    (
        CostClass::FloatTensor,
        Coefficients::new(150_000, 4_194_304),
    ),
    (
        CostClass::TensorSharded,
        Coefficients::new(100_000, 4_194_304),
    ),
    (
        CostClass::TensorTiled,
        Coefficients::new(100_000, 4_194_304),
    ),
    (
        CostClass::WeightDelta,
        Coefficients::new(100_000, 3_145_728),
    ),
    // SPARSE_DELTA reuses the WEIGHT_DELTA row: both decode one exact
    // word-wise delta against the same base frame, and the run-stream decode
    // was not separately calibrated. The row is labelled as reused, not
    // measured.
    (
        CostClass::SparseDelta,
        Coefficients::new(100_000, 3_145_728),
    ),
    (
        CostClass::SemanticDelta,
        Coefficients::new(150_000, 3_145_728),
    ),
    (CostClass::KvCache, Coefficients::new(100_000, 1_048_576)),
    (CostClass::Activation, Coefficients::new(100_000, 1_048_576)),
];

/// Host-calibrated decode cost, median of three independent 7-repetition runs
/// in the bench court (`examples/calibrate_decode.rs`, 128 KiB and 16 MiB, warm
/// cache, synthetic weight-like BF16).
///
/// Delta rows are *incremental*: the measured base read is subtracted, so the
/// rebase planner can price a dependency chain by recursion. Mode 9, mode 10,
/// and mode 15 have a fixed term below the 128 KiB measurement resolution; the
/// reported 0 means "smaller than the measurement can resolve", not "no cost".
/// Modes 12 and 13 are never produced by the store; their rows retain the
/// declared values and are labelled as such in `docs/STATUS.md`.
/// `MEASURED_CALIBRATION` names the exact measurement. See
/// `bench/RESULTS-real-model.md` and `bench/receipts/planner-float-*.json` for
/// the raw rows.
pub const MEASURED_CALIBRATION: &str = "weftfs-r2/decode-calibration/v1";

/// Measured whole-checkpoint restore overhead that the affine decode rows do
/// not cover.
///
/// The decode rows are fitted on single-object `Store::read_tensor` calls. A
/// checkpoint restore does more work per version: it loads the checkpoint
/// manifest, then for every tensor re-verifies the shard content root, the
/// shard logical root, the reassembled content root, and the reassembled
/// logical root, hashes the tensor for the report, and hashes the assembled
/// container. Those passes are proportional to the version's logical bytes and
/// carry a small per-tensor and per-version cost.
///
/// The values are measured by `examples/calibrate_restore.rs` on real
/// `Store::restore_checkpoint` calls (see
/// [`RESTORE_OVERHEAD_CALIBRATION`]); they are not declared. The S13 court's
/// model-and-measurement gap is the defect this term fixes: before S13b the
/// graph under-predicted the all-independent restore floor by 73.7 %.
pub const RESTORE_OVERHEAD_CALIBRATION: &str = "weftfs-s13b/restore-overhead/v1";

/// One additive term for the work a whole-checkpoint restore does beyond the
/// affine decode rows.
///
/// `restore_ns(buf) = per_version_ns + per_tensor_ns * tensors
/// + per_mib_ns * logical_mib`.
///
/// The term is per *version restore*, not per dependency hop: a chain restore
/// decodes one frame per hop (already in the decode rows) and then verifies the
/// final tensor once. Adding it per hop would double-count it and over-predict
/// long chains.
///
/// S13b measurements (`weft.s13b.restore-floor.v1`,
/// `bench/receipts/s13b-restore-floor-20261108T230417Z.json`): the per-MiB term
/// is 2,114,993 ns over the 4 KiB-64 MiB sweep and the nine independent
/// pythia-70m checkpoints validate at 778-793 ms against a 781.6 ms prediction
/// (-1.5 %..+0.4 %); the per-tensor term is 29,205 ns; the per-version term is
/// 13,285 ns, the measured manifest read of a one-tensor checkpoint. The
/// The residue does **not** scale with object count for tensors of 1 MiB or more
/// (one 64 MiB tensor and sixty-four 1 MiB tensors measure within 7 % of each
/// other at the same total), so hypothesis 1 of S13b is refuted in that range;
/// the per-tensor term is the small-object fixed work that appears below
/// roughly 256 KiB.
///
/// S13c adds [`Self::margin_ns_per_mib`], the **measured SLA margin**. It is
/// derived, not tuned: 390,622 ns/MiB is the measured worst under-prediction of
/// the all-independent floor in the nine-checkpoint court (predicted 781.6 ms,
/// cold-measured 886.5 ms over 281,706,496 logical bytes) and 227,136 ns/MiB is
/// 10.74 % — the largest absolute fitted residual over the 22 calibration
/// points at tensor sizes of 1 MiB or more — applied to `per_mib_ns`. The
/// sub-256-KiB fit artifact is excluded because hypothesis 1 above refutes a
/// per-object term in that regime and every parent-graph tensor is MiB-scale.
/// The margin bounds a *measured local, warm-page-cache* environment; see
/// `docs/PARENT-GRAPH.md` for what it cannot promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestoreOverhead {
    pub per_version_ns: u64,
    pub per_tensor_ns: u64,
    pub per_mib_ns: u64,
    /// Additive per-logical-MiB SLA safety margin (see
    /// [`SLA_MARGIN_CALIBRATION`]). Charged only when checking an SLA, never to
    /// the objective or the reported point prediction.
    pub margin_ns_per_mib: u64,
}

impl RestoreOverhead {
    /// The baked measured term. See [`RESTORE_OVERHEAD_CALIBRATION`] and
    /// [`SLA_MARGIN_CALIBRATION`].
    pub const MEASURED: RestoreOverhead = RestoreOverhead {
        per_version_ns: 13_285,
        per_tensor_ns: 29_205,
        per_mib_ns: 2_114_993,
        // 390,622 (measured cold-versus-warm under-prediction per logical MiB)
        // + 227,136 (10.74 % fit-residual allowance on the per-MiB overhead).
        margin_ns_per_mib: 617_758,
    };

    /// The zero term. Never use it for an SLA claim; it exists so a caller can
    /// measure the omission explicitly in a diagnostic.
    pub const ZERO: RestoreOverhead = RestoreOverhead {
        per_version_ns: 0,
        per_tensor_ns: 0,
        per_mib_ns: 0,
        margin_ns_per_mib: 0,
    };

    /// Estimated restore overhead in nanoseconds. Integer arithmetic only.
    pub fn estimate_ns(&self, tensors: u64, logical_bytes: u64) -> u64 {
        let scaled = (u128::from(logical_bytes) * u128::from(self.per_mib_ns)) / 1_048_576u128;
        let total = u128::from(self.per_version_ns)
            .saturating_add(u128::from(self.per_tensor_ns).saturating_mul(u128::from(tensors)))
            .saturating_add(scaled);
        u64::try_from(total).unwrap_or(u64::MAX)
    }

    /// The SLA safety margin for a version of `logical_bytes`. Integer
    /// arithmetic only, and the same order of operations as [`Self::estimate_ns`].
    pub fn estimate_margin_ns(&self, logical_bytes: u64) -> u64 {
        let scaled =
            (u128::from(logical_bytes) * u128::from(self.margin_ns_per_mib)) / 1_048_576u128;
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }
}

/// The baked measured restore overhead for `tensors` and `logical_bytes`.
pub fn restore_overhead_ns(tensors: u64, logical_bytes: u64) -> u64 {
    RestoreOverhead::MEASURED.estimate_ns(tensors, logical_bytes)
}

// ---------------------------------------------------------------------------
// S13c: measured SLA margin and graph-hop decode rows
// ---------------------------------------------------------------------------

/// Identifier for the S13c measured SLA safety margin.
///
/// The margin is an additive per-logical-MiB term charged to the SLA check of a
/// parent-graph plan. It bounds the measured under-prediction of a restore in
/// the environment the plan is made for; it is not a fudge factor and it does
/// not alter the point prediction or the objective.
pub const SLA_MARGIN_CALIBRATION: &str = "weftfs-s13c/sla-margin/v1";

/// Identifier for the S13c graph-hop decode rows.
pub const CHAIN_HOP_CALIBRATION: &str = "weftfs-s13c/chain-hop/v1";

/// Measured per-hop decode rows, used **only** by the parent-graph planner.
///
/// A graph edge is decoded against an already-reconstructed base, so its cost
/// is the *increment* of one more frame on the chain. The S13b court measured
/// that the affine rows in [`MEASURED`] over-price those increments: `raw`,
/// `zstd`, `float-tensor` and the tiled rows are fitted on single-object reads
/// (base included), while `SPARSE_DELTA` reuses the `WEIGHT_DELTA` row and
/// `ZSTD_PATCH` reuses it too. The chain was therefore predicted 31.7 % high.
///
/// These rows are the controlled-chain measurement (`examples/calibrate_restore
/// .rs --chains-only --chain-sizes-mib 4,8,16`, 24 samples per class per run,
/// two independent runs unioned to 48 samples per class at 4/8/16 MiB, F32),
/// least-squares through the origin. They have **no fixed term**: the hop
/// increment cancels the per-version restore overhead and every per-tensor cost
/// that both versions pay, so only the per-MiB rate is identifiable.
///
/// Deliberately a separate table from [`MEASURED`]: `TensorPlanOptions` still
/// prices write-time candidates with the decode table, so this correction can
/// never change which representation the writer stores. Only the parent-graph
/// planner's edge pricing uses it. See `docs/PARENT-GRAPH.md`.
const HOP_MEASURED: [(CostClass, Coefficients); 2] = [
    (CostClass::WeightDelta, Coefficients::new(0, 4_690_851)),
    (CostClass::SparseDelta, Coefficients::new(0, 2_071_283)),
];

/// Identifier for the measured per-hop uncertainty bounds.
///
/// The bound is the largest **under**-prediction of the baked row over every
/// fitted `(bytes, ns)` hop sample, expressed in parts per million of the
/// prediction. It is measured from the raw samples in the two committed
/// `weft.s13c.chain-hop.v1` receipts (`bench/receipts/s13c-chain-hop-*.json`),
/// unioned per class:
///
/// * sparse-delta: 153,302 ppm (15.33 %) over 48 samples
/// * weight-delta: 178,209 ppm (17.82 %) over 48 samples
///
/// Only the under-prediction direction matters for an SLA: an over-prediction
/// is conservative. The bound is warm-cache, single-host, local-backend.
pub const HOP_UNCERTAINTY_CALIBRATION: &str = "weftfs-r9/hop-uncertainty/v1";

/// Measured under-prediction bound per hop class, in parts per million.
const HOP_UNCERTAINTY_PPM: [(CostClass, u64); 2] = [
    (CostClass::WeightDelta, 178_209),
    (CostClass::SparseDelta, 153_302),
];

/// Identifier for the measured decode-row uncertainty bounds.
///
/// The persisted decode rows are two-point fits, so they have no within-fit
/// residual. The uncertainty recorded here is the observed cross-run spread of
/// the same host calibration: the maximum absolute deviation from 1.0 of
/// `this_run_vs_baked_ns_per_mib_ratio` across the two committed
/// `weftfs-r1` / `weftfs-r2` planner receipts, with a 1 % floor for classes the
/// receipts did not repeat. It bounds run-to-run drift, not a within-run
/// residual; that is the measured quantity available for these rows.
pub const DECODE_UNCERTAINTY_CALIBRATION: &str = "weftfs-r9/decode-uncertainty/v1";

/// Measured run-to-run under-prediction bound per decode class, in ppm.
const DECODE_UNCERTAINTY_PPM: [(CostClass, u64); 10] = [
    (CostClass::Raw, 10_000),
    (CostClass::Zstd, 10_560),
    (CostClass::FloatTensor, 10_000),
    (CostClass::TensorSharded, 10_000),
    (CostClass::TensorTiled, 10_000),
    (CostClass::WeightDelta, 10_000),
    (CostClass::SparseDelta, 10_000),
    (CostClass::SemanticDelta, 10_000),
    (CostClass::KvCache, 10_000),
    (CostClass::Activation, 10_000),
];

/// The graph-hop measured row for `class`, when one was measured.
///
/// `None` means the class has no controlled-chain measurement. The parent-graph
/// planner refuses to price such an edge instead of borrowing a neighbouring
/// row.
pub fn measured_hop_row(class: CostClass) -> Option<Coefficients> {
    HOP_MEASURED
        .iter()
        .find(|(candidate, _)| *candidate == class)
        .map(|(_, row)| *row)
}

/// Identifier for the measured zstd-patch hop row.
///
/// `ZSTD_PATCH` (mode-4 `DICTIONARY` against the whole base) has no
/// [`CostClass`] of its own: the persisted calibration schema is frozen at ten
/// classes. It nevertheless gets its own measured controlled-chain row, because
/// the pythia graph selects it on adjacent fine-tune steps and pricing it with
/// the weight-delta row is exactly the substitution this planner refuses.
pub const ZSTD_PATCH_HOP_CALIBRATION: &str = "weftfs-r9/zstd-patch-hop/v1";

/// Measured zstd-patch hop row: 1,675,326 ns/MiB through the origin over 48
/// samples (4/8/16 MiB, two change shapes, depth 8, forced strategy).
/// Receipt: `bench/receipts/s13c-chain-hop-zstd-patch-20261110T090000Z.json`.
pub const ZSTD_PATCH_HOP: Coefficients = Coefficients::new(0, 1_675_326);

/// Measured under-prediction bound for the zstd-patch hop row: 312,958 ppm
/// (31.30 %) over the same 48 samples.
pub const ZSTD_PATCH_HOP_UNCERTAINTY_PPM: u64 = 312_958;

/// The measured under-prediction bound for a graph hop, in ppm of the point
/// prediction. `None` when the class has no measured hop row.
pub fn hop_uncertainty_ppm(class: CostClass) -> Option<u64> {
    HOP_UNCERTAINTY_PPM
        .iter()
        .find(|(candidate, _)| *candidate == class)
        .map(|(_, ppm)| *ppm)
}

/// The measured run-to-run under-prediction bound for a decode row, in ppm.
pub fn decode_uncertainty_ppm(class: CostClass) -> u64 {
    DECODE_UNCERTAINTY_PPM
        .iter()
        .find(|(candidate, _)| *candidate == class)
        .map(|(_, ppm)| *ppm)
        .unwrap_or(10_000)
}

/// The graph-hop row for `class`: a measured hop override when one exists,
/// otherwise the [`CostModelKind::Measured`] decode row unchanged.
pub fn hop_coefficients(class: CostClass) -> Coefficients {
    if let Some((_, row)) = HOP_MEASURED
        .iter()
        .find(|(candidate, _)| *candidate == class)
    {
        return *row;
    }
    coefficients(class, CostModelKind::Measured)
}

/// Estimated graph-hop decode nanoseconds for `decoded_bytes`.
pub fn hop_estimate_ns(class: CostClass, decoded_bytes: u64) -> u64 {
    estimate_with(hop_coefficients(class), decoded_bytes)
}

/// Every graph-hop row in canonical class order, for the CLI and receipts.
pub fn hop_table() -> Vec<(CostClass, Coefficients)> {
    CostClass::ALL
        .iter()
        .map(|class| (*class, hop_coefficients(*class)))
        .collect()
}

/// Measured mode-18 `FLOAT_TILED_STREAMS` decode row.
///
/// Mode 18 has its own affine row measured with the same harness as the other
/// classes (`examples/calibrate_decode.rs`: median of three 7-repetition runs,
/// 128 KiB and 16 MiB, warm cache, synthetic weight-like BF16, four interleaved
/// streams). The persisted `weft.calibration.v1` schema has exactly ten rows and
/// no mode-18 class, so a host calibration file prices mode 18 with the
/// `tensor-tiled` row; the baked default is this measured row. The row is
/// deliberately **not** a copy of `TensorTiled`: the four-stream decode was
/// measured separately.
pub const MODE18_MEASURED_CALIBRATION: &str = "weftfs-mode18/decode-calibration/v1";

/// Measured mode-18 row (see [`MODE18_MEASURED_CALIBRATION`]).
///
/// The two-point affine fit at 128 KiB and 16 MiB produced a *negative*
/// intercept, which was previously clamped to zero. A zero fixed term made a
/// one-tile region read look free, so a tiny-region plan could select mode 18
/// even though the mode-18 court measured it 1.5-3.0x slower than mode 17 on
/// exactly those regions. A fitted fixed term is never allowed to be zero
/// (see [`ENCODE_FLOOR_NOTE`] and the `every_measured_fixed_term_is_positive`
/// test); the floor below is the intercept of the measured *small-region*
/// affine (`region_1_row` and `region_100_rows` medians at tile `[64,896]`,
/// 114,688 canonical tile bytes each):
///
/// ```text
/// (415,555 - 256,078) ns for one extra 114,688-byte tile  =>  1,457,700 ns/MiB
/// 256,078 ns - 159,477 ns                                  =>  96,601 ns
/// ```
///
/// The per-MiB term stays the whole-frame measured rate, because the persisted
/// calibration schema has one rate per class; the fixed term is the measured
/// lower bound, so a small-region estimate can only become more expensive.
const MODE18_MEASURED: Coefficients = Coefficients::new(96_601, 3_674_824);

/// Estimated mode-18 decode nanoseconds for `decoded_bytes` logical bytes.
pub fn estimate_streams_ns(decoded_bytes: u64) -> u64 {
    estimate_with(MODE18_MEASURED, decoded_bytes)
}

/// The measured mode-18 coefficients.
pub fn streams_measured_coefficients() -> Coefficients {
    MODE18_MEASURED
}

/// Measured rows (median of three independent 7-repetition runs).
/// Re-measured after the R2 Huffman and plane-join work; the previous table
/// was `float-tensor 15_403_165`, `tensor-sharded 13_788_933`,
/// `tensor-tiled 10_056_794`, `semantic-delta 7_228_350`.
const MEASURED: [(CostClass, Coefficients); 10] = [
    (CostClass::Raw, Coefficients::new(11_757, 307_019)),
    (CostClass::Zstd, Coefficients::new(21_285, 779_888)),
    (CostClass::FloatTensor, Coefficients::new(18_264, 4_683_726)),
    (
        CostClass::TensorSharded,
        Coefficients::new(16_172, 9_123_307),
    ),
    (CostClass::TensorTiled, Coefficients::new(14_131, 3_366_772)),
    (CostClass::WeightDelta, Coefficients::new(30_725, 5_293_987)),
    // SPARSE_DELTA reuses the measured WEIGHT_DELTA row (not separately
    // calibrated): the run-stream decoder does the same base read plus a
    // varint walk, so mode 11's per-MiB cost is the conservative choice.
    (CostClass::SparseDelta, Coefficients::new(30_725, 5_293_987)),
    (
        CostClass::SemanticDelta,
        Coefficients::new(31_891, 6_458_523),
    ),
    (CostClass::KvCache, Coefficients::new(100_000, 1_048_576)),
    (CostClass::Activation, Coefficients::new(100_000, 1_048_576)),
];

/// A complete affine coefficient table, one row per [`CostClass`].
///
/// The baked tables ([`CostModelKind::Declared`], [`CostModelKind::Measured`])
/// and a host-local calibration file both become a `CostTable`. Pricing always
/// goes through this type, so the integer arithmetic cannot drift between a
/// baked and a loaded table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CostTable {
    rows: [Coefficients; 10],
}

impl CostTable {
    /// Build a table from exactly ten rows in [`CostClass::ALL`] order.
    ///
    /// Crate-private: callers outside WeftFS obtain a table only from a
    /// validated [`crate::calibration::Calibration`] or [`CostTable::baked`], so
    /// an unvalidated row set cannot reach the planner through the public API.
    pub(crate) fn from_rows(rows: [Coefficients; 10]) -> Self {
        CostTable { rows }
    }

    /// The baked table for a model.
    pub fn baked(model: CostModelKind) -> Self {
        let rows = match model {
            CostModelKind::Declared => &DECLARED,
            CostModelKind::Measured => &MEASURED,
        };
        let mut table = [Coefficients::new(0, 1_048_576); 10];
        for (class, row) in rows.iter() {
            table[class.index()] = *row;
        }
        CostTable { rows: table }
    }

    /// The row for one class.
    pub fn row(&self, class: CostClass) -> Coefficients {
        self.rows[class.index()]
    }

    /// Estimated decode nanoseconds for `decoded_bytes` logical bytes.
    pub fn estimate_ns(&self, class: CostClass, decoded_bytes: u64) -> u64 {
        estimate_with(self.row(class), decoded_bytes)
    }
}

fn estimate_with(row: Coefficients, decoded_bytes: u64) -> u64 {
    let scaled = (u128::from(decoded_bytes) * u128::from(row.ns_per_mib)) / 1_048_576u128;
    u64::try_from(scaled)
        .unwrap_or(u64::MAX)
        .saturating_add(row.fixed_ns)
}

/// Estimate nanoseconds from an explicit [`Coefficients`] row. Exposed so the
/// planner's hop table and a diagnostics caller use the same integer
/// arithmetic as the baked decode table.
pub fn estimate_from_row(row: Coefficients, bytes: u64) -> u64 {
    estimate_with(row, bytes)
}

/// The affine coefficients for one class under one baked model.
pub fn coefficients(class: CostClass, model: CostModelKind) -> Coefficients {
    CostTable::baked(model).row(class)
}

/// Estimated decode nanoseconds for `decoded_bytes` logical bytes.
pub fn estimate_ns(class: CostClass, decoded_bytes: u64, model: CostModelKind) -> u64 {
    CostTable::baked(model).estimate_ns(class, decoded_bytes)
}

/// Every row for a baked model, in canonical order. Used by the CLI and tests
/// so the table has exactly one definition.
pub fn table(model: CostModelKind) -> Vec<(CostClass, Coefficients)> {
    let rows = match model {
        CostModelKind::Declared => &DECLARED,
        CostModelKind::Measured => &MEASURED,
    };
    rows.to_vec()
}

// ---------------------------------------------------------------------------
// Encoder (write) cost
// ---------------------------------------------------------------------------
//
// The access-pattern objective needs a write term: the region planner could
// otherwise select an expensive-to-encode representation for a single read.
// These rows price *one whole-tensor encode* (and its store publication), not
// a region decode. They are deliberately separate from the decode table.
//
// Provenance:
//
// * `raw` and `zstd` are measured from the T1 write-policy court's fast-path
//   CPU time (2,470 ms raw and 2,910 ms zstd-3 for a 988,097,824-byte
//   checkpoint => 2.62 / 3.09 ms per MiB).
// * `float-tensor` and `float-tiled` are measured from the float-tiled court's
//   store-level `write_ms` (2,286.7 ms and 7,434.7 ms for 259.656 MiB =>
//   8.81 / 28.63 ms per MiB). This is the 3.25x write ratio the review cites.
// * `float-tiled-streams` is *derived*: the mode-18 court measured a 1.3048x
//   codec-encode ratio over mode 17 (1,522.7 ms vs 1,167.0 ms), applied to
//   mode 17's measured store-level rate.
// * The remaining rows are **declared**, not measured: `tensor-sharded` reuses
//   the float-tensor rate, `tensor-tiled` reuses the float-tiled rate (same
//   per-tile Huffman encode), and the delta rows are declared as two
//   float-tensor encodes (one base read plus one encode).
//
// A zero fixed term is allowed here because an encode always processes the
// whole tensor; the zero-fixed hazard is a *region* decode artifact.

/// Identifier for the encode table; recorded in receipts.
pub const ENCODE_CALIBRATION: &str = "weftfs-write-cost/v1";

/// Whether one encode row was measured, derived, or declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeProvenance {
    Measured,
    Derived,
    Declared,
}

impl EncodeProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            EncodeProvenance::Measured => "measured",
            EncodeProvenance::Derived => "derived",
            EncodeProvenance::Declared => "declared",
        }
    }
}

/// Encode classes. These track the planner's strategies rather than the frozen
/// decode [`CostClass`] set, because modes 17 and 18 have their own measured
/// encode cost but no decode class of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EncodeClass {
    Raw,
    Zstd,
    /// Level-19 (or measured-slow) ZSTD encode. Measured at 28.5 MiB/s by the
    /// R5 entropy-coder court.
    Zstd19,
    FloatTensor,
    TensorSharded,
    TensorTiled,
    FloatTiled,
    FloatTiledStreams,
    WeightDelta,
    SparseDelta,
    SemanticDelta,
}

impl EncodeClass {
    pub const ALL: [EncodeClass; 11] = [
        EncodeClass::Raw,
        EncodeClass::Zstd,
        EncodeClass::Zstd19,
        EncodeClass::FloatTensor,
        EncodeClass::TensorSharded,
        EncodeClass::TensorTiled,
        EncodeClass::FloatTiled,
        EncodeClass::FloatTiledStreams,
        EncodeClass::WeightDelta,
        EncodeClass::SparseDelta,
        EncodeClass::SemanticDelta,
    ];

    pub fn index(self) -> usize {
        EncodeClass::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .expect("every EncodeClass is listed in EncodeClass::ALL")
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EncodeClass::Raw => "raw",
            EncodeClass::Zstd => "zstd",
            EncodeClass::Zstd19 => "zstd-19",
            EncodeClass::FloatTensor => "float-tensor",
            EncodeClass::TensorSharded => "tensor-sharded",
            EncodeClass::TensorTiled => "tensor-tiled",
            EncodeClass::FloatTiled => "float-tiled",
            EncodeClass::FloatTiledStreams => "float-tiled-streams",
            EncodeClass::WeightDelta => "weight-delta",
            EncodeClass::SparseDelta => "sparse-delta",
            EncodeClass::SemanticDelta => "semantic-delta",
        }
    }
}

/// One encode row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeRow {
    pub coefficients: Coefficients,
    pub provenance: EncodeProvenance,
}

const fn encode(fixed_ns: u64, ns_per_mib: u64, provenance: EncodeProvenance) -> EncodeRow {
    EncodeRow {
        coefficients: Coefficients::new(fixed_ns, ns_per_mib),
        provenance,
    }
}

/// Encode cost rows in [`EncodeClass::ALL`] order.
const ENCODE: [EncodeRow; 11] = [
    // raw: measured from the T1 fast-raw checkpoint CPU (2.62 ms/MiB).
    encode(0, 2_620_000, EncodeProvenance::Measured),
    // zstd level 3: measured from the T1 fast-zstd-3 checkpoint CPU (3.09 ms/MiB).
    encode(0, 3_090_000, EncodeProvenance::Measured),
    // zstd level 19: measured at 28.5 MiB/s by the R5 entropy-coder court.
    encode(0, 35_087_719, EncodeProvenance::Measured),
    // float-tensor: measured store write, 2,286.7 ms / 259.656 MiB.
    encode(0, 8_806_000, EncodeProvenance::Measured),
    // tensor-sharded: declared reuse of the float-tensor store rate.
    encode(0, 8_806_000, EncodeProvenance::Declared),
    // tensor-tiled (mode 15): declared reuse of the float-tiled store rate.
    encode(0, 28_633_000, EncodeProvenance::Declared),
    // float-tiled (mode 17): measured store write, 7,434.7 ms / 259.656 MiB.
    encode(0, 28_633_000, EncodeProvenance::Measured),
    // float-tiled-streams (mode 18): derived from the measured codec ratio
    // (1,522.7 ms / 1,167.0 ms = 1.3048x) applied to mode 17's store rate.
    encode(0, 37_360_000, EncodeProvenance::Derived),
    // weight-delta: declared 2x float-tensor (base read + encode).
    encode(0, 17_612_000, EncodeProvenance::Declared),
    // sparse-delta: declared 2x float-tensor.
    encode(0, 17_612_000, EncodeProvenance::Declared),
    // semantic-delta: declared 2x float-tensor.
    encode(0, 17_612_000, EncodeProvenance::Declared),
];

/// The encode row for one class.
pub fn encode_row(class: EncodeClass) -> EncodeRow {
    ENCODE[class.index()]
}

/// Estimated encode nanoseconds for one whole-tensor encode of `bytes`.
pub fn estimate_encode_ns(class: EncodeClass, bytes: u64) -> u64 {
    estimate_with(encode_row(class).coefficients, bytes)
}

/// The whole encode table in canonical order (diagnostics and tests).
pub fn encode_table() -> Vec<(EncodeClass, EncodeRow)> {
    EncodeClass::ALL
        .iter()
        .map(|class| (*class, encode_row(*class)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_matches_the_historical_linear_model() {
        // size/200 + 20_000 with 1 MiB = 1_048_576 bytes.
        assert_eq!(
            estimate_ns(CostClass::Raw, 1_048_576, CostModelKind::Declared),
            5_243 + 20_000
        );
        assert_eq!(
            estimate_ns(CostClass::Zstd, 1_048_576, CostModelKind::Declared),
            1_048_576 + 50_000
        );
        assert_eq!(
            estimate_ns(CostClass::WeightDelta, 1_048_576, CostModelKind::Declared),
            3 * 1_048_576 + 100_000
        );
    }

    #[test]
    fn estimates_are_monotonic_and_saturating() {
        for class in [
            CostClass::Raw,
            CostClass::Zstd,
            CostClass::FloatTensor,
            CostClass::TensorSharded,
            CostClass::TensorTiled,
            CostClass::WeightDelta,
            CostClass::SparseDelta,
            CostClass::SemanticDelta,
            CostClass::KvCache,
            CostClass::Activation,
        ] {
            for model in [CostModelKind::Declared, CostModelKind::Measured] {
                let small = estimate_ns(class, 1024, model);
                let large = estimate_ns(class, 64 * 1024 * 1024, model);
                assert!(large > small, "{class:?} is not monotonic");
                let _ = estimate_ns(class, u64::MAX, model);
            }
        }
    }

    #[test]
    fn measured_is_not_the_declared_table() {
        assert_ne!(
            coefficients(CostClass::FloatTensor, CostModelKind::Measured),
            coefficients(CostClass::FloatTensor, CostModelKind::Declared)
        );
    }

    #[test]
    fn every_measured_fixed_term_is_positive() {
        // A zero (or negative, clamped) fixed term makes a tiny-region estimate
        // look free. Every measured decode row must carry a real fixed cost.
        for (class, row) in table(CostModelKind::Measured) {
            assert!(
                row.fixed_ns > 0,
                "{class:?} has a zero fixed term, which is the mode-18 artifact"
            );
        }
        assert!(streams_measured_coefficients().fixed_ns > 0);
    }

    #[test]
    fn restore_overhead_is_measured_and_monotonic() {
        let term = RestoreOverhead::MEASURED;
        assert!(term.per_mib_ns > 0);
        assert!(term.per_version_ns > 0);
        // It is a whole-checkpoint term, not a per-hop term: zero tensors and
        // zero bytes is the manifest-only floor, and both axes are monotonic.
        assert_eq!(term.estimate_ns(0, 0), term.per_version_ns);
        assert!(term.estimate_ns(1, 0) > term.estimate_ns(0, 0));
        assert!(term.estimate_ns(0, 1 << 20) > term.estimate_ns(0, 0));
        // 281,706,496 logical bytes and 76 tensors is the pythia floor this
        // term is measured to fix; the S13b receipt predicts 794-803 ms.
        let pythia = term.estimate_ns(76, 281_706_496);
        assert!(
            (570_000_000..=620_000_000).contains(&pythia),
            "unexpected pythia floor overhead {pythia}"
        );
    }

    #[test]
    fn encode_table_is_shaped_and_ordered() {
        let rows = encode_table();
        assert_eq!(rows.len(), EncodeClass::ALL.len());
        for (index, (class, row)) in rows.iter().enumerate() {
            assert_eq!(class.index(), index);
            assert!(row.coefficients.ns_per_mib > 0, "{class:?} has no rate");
            assert!(estimate_encode_ns(*class, 16 * 1024 * 1024) > 0);
        }
        // The 3.25x measured mode-17/mode-9 write ratio is preserved.
        let float_tensor = encode_row(EncodeClass::FloatTensor).coefficients.ns_per_mib;
        let float_tiled = encode_row(EncodeClass::FloatTiled).coefficients.ns_per_mib;
        assert!(float_tiled > float_tensor * 3);
        assert!(float_tiled < float_tensor * 4);
    }
}
