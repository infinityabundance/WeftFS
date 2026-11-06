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

/// The affine coefficients for one class under one model.
pub fn coefficients(class: CostClass, model: CostModelKind) -> Coefficients {
    let table = match model {
        CostModelKind::Declared => &DECLARED,
        CostModelKind::Measured => &MEASURED,
    };
    table
        .iter()
        .find(|(candidate, _)| *candidate == class)
        .map(|(_, row)| *row)
        .unwrap_or(Coefficients::new(0, 1_048_576))
}

/// Estimated decode nanoseconds for `decoded_bytes` logical bytes.
pub fn estimate_ns(class: CostClass, decoded_bytes: u64, model: CostModelKind) -> u64 {
    let row = coefficients(class, model);
    let scaled = (u128::from(decoded_bytes) * u128::from(row.ns_per_mib)) / 1_048_576u128;
    u64::try_from(scaled)
        .unwrap_or(u64::MAX)
        .saturating_add(row.fixed_ns)
}

/// Every row for a model, in canonical order. Used by the CLI and tests so the
/// table has exactly one definition.
pub fn table(model: CostModelKind) -> Vec<(CostClass, Coefficients)> {
    let rows = match model {
        CostModelKind::Declared => &DECLARED,
        CostModelKind::Measured => &MEASURED,
    };
    rows.to_vec()
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
}
