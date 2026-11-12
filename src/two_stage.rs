//! Two-stage planned-write planning (R18): **analyse, select, materialise**.
//!
//! The R17 ledger measured the planned write of 101 real Qwen tensors at
//! 24,229 ms, of which two candidate encodes were pure cost on that fixture:
//! level-19 ZSTD took 20,353 ms and was selected for **zero** tensors, and
//! mode-9 `FLOAT_TENSOR` took 1,743 ms although only 35 of 101 tensors chose it.
//! Encoding every candidate and then discarding the losers is the defect this
//! module removes.
//!
//! # The three stages
//!
//! 1. **Analyse** ([`analyse_tensor`]). Cheap, deterministic, compressor-light:
//!    a bounded deterministic sample of whole codec chunks is compressed at the
//!    levels being priced, and a mode-9 size is *predicted exactly* from a full
//!    exponent histogram by the codec itself
//!    ([`weftcodec::modes::float_tensor_encoded_len_from_histogram`]). No full
//!    `ZSTD` frame of the tensor is built in this stage.
//! 2. **Select** ([`select`]). Every candidate carries a physical-size
//!    interval. A candidate is encoded only when its interval could change the
//!    winner, or could change the mode-9 `1/64` gate. Everything else is
//!    skipped with the numeric bound that made the skip safe, which is stored in
//!    the plan (`detail`) and in the decision stream.
//! 3. **Materialise**. The selected candidate is encoded; its actual size is
//!    checked against the planned interval. A violation is recorded in the plan
//!    and forces the tensor's remaining candidates to be encoded exhaustively
//!    before selection, so a representation is never published that was chosen
//!    on a contradicted prediction.
//!
//! # Why the intervals are the whole safety argument
//!
//! The rule `predicted_lower > incumbent_upper => skip` is sound **relative to
//! the interval**: if the actual size of the skipped candidate is inside its
//! interval and the incumbent's actual is inside its own, the skipped candidate
//! cannot be cheaper. The intervals are therefore calibrated empirically and
//! must be *checked*: [`SampleEstimate::contains`] is asserted on every
//! candidate that is actually encoded, and the decision-diff court compares the
//! two-stage planner against the exhaustive planner per tensor on the frozen
//! fixture and on other model families. A skipped candidate cannot be
//! re-measured by construction, so that court comparison — not this module — is
//! the evidence that no skip changed a decision.
//!
//! # What is *not* analysed
//!
//! Candidates that have no cheap predictor (mode 10 for `q8_0`, tiled modes
//! 15/17/18, mode 20 engram, the causal deltas 11/14/16, and the zstd patch)
//! are prepared exactly as before. They are opt-in (a tile shape, engram
//! metadata, a causal parent) or the only candidate for the dtype, and the R17
//! ledger shows they cost 41 ms of 24,229 ms. Restricting the two-stage rule to
//! the four candidates that dominate the ledger keeps the change auditable: it
//! cannot alter the decision among the others.
//!
//! # Not a wire format
//!
//! Nothing here is persisted, hashed, or referenced by a manifest, commit, or
//! index. It changes which encoders run, never what a chosen representation
//! looks like.

use std::collections::BTreeMap;
use std::sync::Arc;

use weftcodec::modes::{
    encode_zstd, float_tensor_encoded_len_from_histogram, FloatDtype,
};
use weftcodec::Limits;

use crate::error::Result;

/// Relative error floor for the level-3 sampled estimate, in basis points.
///
/// Calibrated on the frozen R18 fixture (101 Qwen2.5-0.5B-Instruct tensors) and
/// re-checked on the Qwen2.5-0.5B base/instruct pair and the pythia-70m
/// trajectory; `bench/RESULTS-planner-two-stage.md` records the observed maximum
/// deviation. The floor widens the interval when the sampled chunks happen to
/// agree with each other, which is the dangerous case: four like-looking chunks
/// must not be treated as proof about the other thousands.
pub const ZSTD_SAMPLE_ERROR_BPS: u64 = 500;

/// Relative error floor for the level-19 / level-3 size *ratio*, in basis
/// points. The ratio is what the level-19 gate prices: level 19 is predicted as
/// `ratio * <level-3 size>`, so the level-3 interval propagates as well.
pub const ZSTD19_RATIO_ERROR_BPS: u64 = 500;

/// Chunks sampled at level 3 per tensor. Bounded work: at most
/// `4 x codec chunk` (256 KiB with the 64 KiB codec chunk) compressed per
/// tensor.
pub const SAMPLE_CHUNKS: usize = 4;

/// Sampled chunks re-used for the level-19 ratio. Level-19 compression runs at
/// roughly 28 MiB/s, so its sample is deliberately smaller than the level-3 one.
pub const SAMPLE_CHUNKS_19: usize = 2;

const BPS: u64 = 10_000;

/// One sampled chunk: which byte range was compressed, and how large it became
/// at each measured level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampledChunk {
    pub index: u64,
    pub offset: u64,
    pub len: u64,
    /// Compressed body length at level 3.
    pub level3: u64,
    /// Compressed body length at level 19, when that level was priced.
    pub level19: Option<u64>,
}

/// A physical-size prediction with an explicit interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleEstimate {
    /// `"exact"` when every chunk was measured, otherwise `"sampled-chunks"`
    /// (level 3) or `"sampled-ratio"` (level 19).
    pub method: &'static str,
    pub level: i32,
    /// Point prediction of the whole frame length, in bytes.
    pub predicted: u64,
    /// Lower end of the trusted interval.
    pub lower: u64,
    /// Upper end of the trusted interval.
    pub upper: u64,
    /// Bytes actually compressed to produce the prediction.
    pub sampled_bytes: u64,
    /// Chunks actually compressed.
    pub sampled_chunks: usize,
    /// Total chunks in the frame.
    pub total_chunks: u64,
}

impl SampleEstimate {
    /// Whether a measured actual size is inside the trusted interval.
    pub fn contains(&self, actual: u64) -> bool {
        actual >= self.lower && actual <= self.upper
    }

    /// Human-readable interval, used in plan details and the decision stream.
    pub fn interval(&self) -> String {
        format!("{}..{} B", self.lower, self.upper)
    }
}

/// Exact mode-9 prediction for one tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode9Prediction {
    /// Exact encoded frame length, not an estimate.
    pub exact_bytes: u64,
    pub dtype: FloatDtype,
    /// Symbols present in the exponent histogram.
    pub exponent_symbols: usize,
    /// The exact exponent histogram this prediction was computed from.
    ///
    /// The analyser already paid for the full traversal, so the encoder is
    /// handed the same counts instead of re-counting the same bytes. The
    /// histogram is the only input to the canonical table, so a wrong
    /// histogram would change the emitted bitstream; the store passes the one
    /// computed over the identical slice in the same call, and the plan's
    /// `exact_bytes` is cross-checked against the frame the encoder returns.
    pub exponent_histogram: Arc<[u64; 256]>,
}

/// Everything the select stage needs, produced by the analyse stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub logical_bytes: u64,
    pub chunk_bytes: u64,
    /// The raw candidate is exactly the logical size.
    pub raw_bytes: u64,
    pub zstd3: Option<SampleEstimate>,
    pub zstd19: Option<SampleEstimate>,
    pub mode9: Option<Mode9Prediction>,
    pub samples: Vec<SampledChunk>,
}

/// Whether the exact mode-9 prediction satisfies the shipped `1/64` rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode9Gate {
    /// It passes even at the smallest plausible level-3 size, so the rule cannot
    /// have rejected it.
    Pass,
    /// It fails even at the largest plausible level-3 size.
    Fail,
    /// The level-3 interval straddles the rule, so level 3 must be measured.
    /// `Decision.need_zstd3` is then necessarily true.
    Unresolved,
}

/// The select stage's verdict for the levels that are decided *before* any
/// candidate is materialised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub need_zstd3: bool,
    /// Recorded when `need_zstd3` is false and level 3 was offered: the numeric
    /// bound that made the skip safe.
    pub zstd3_skip_bound: Option<String>,
    pub mode9: Mode9Gate,
}

/// The incumbent level 19 has to beat, built from **measured or exact** sizes
/// only. Estimating the incumbent would make the skip bound meaningless: a
/// lower bound of zero (a heterogeneous tensor the sampler cannot predict)
/// compares against "0 B" and skips nothing while looking safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Incumbent {
    pub physical: u64,
    pub cost: u128,
    pub name: &'static str,
}

impl Incumbent {
    /// `cost` is the complete price (physical bytes plus any fixed terms).
    pub fn new(physical: u64, cost: u128, name: &'static str) -> Self {
        Incumbent {
            physical,
            cost,
            name,
        }
    }

    /// Price a measured physical size with its fixed (lambda) terms.
    ///
    /// Prefer this over [`Self::new`] whenever a fixed term is involved: passing
    /// the fixed term where a full cost is expected makes the incumbent look
    /// free, which turns a skip bound into a meaningless number. That exact
    /// mistake was made and caught by the R18 decision diff (the embedding's
    /// bound read "safe by 200,599,584 B" against a "0 B" incumbent).
    pub fn priced(physical: u64, fixed: u128, name: &'static str) -> Self {
        Incumbent {
            physical,
            cost: u128::from(physical) + fixed,
            name,
        }
    }
}

/// The level-19 verdict, decided after the cheaper candidates are measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level19Decision {
    pub encode: bool,
    pub skip_bound: Option<String>,
    pub estimate: Option<SampleEstimate>,
}

/// The non-physical part of each candidate's price (decode and encode lambdas).
/// All zero under the bytes-only default, which is what the R17 court prices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedCosts {
    pub raw: u128,
    pub zstd3: u128,
    pub zstd19: u128,
    pub mode9: u128,
}

fn chunk_count(size: u64, chunk: u64) -> u64 {
    if size == 0 || chunk == 0 {
        0
    } else {
        size.div_ceil(chunk)
    }
}

/// Deterministic chunk indices: every full chunk when the tensor is small
/// enough, otherwise evenly spaced indices that always include the first and the
/// last full chunk. Public so the rule can be tested directly.
pub fn sample_indices(full_chunks: u64, want: usize) -> Vec<u64> {
    if full_chunks == 0 {
        return Vec::new();
    }
    if full_chunks <= want as u64 {
        return (0..full_chunks).collect();
    }
    let want = want.max(2);
    let span = full_chunks - 1;
    let mut out = Vec::with_capacity(want);
    for slot in 0..want {
        let index = (slot as u64 * span) / (want as u64 - 1);
        if out.last().copied() != Some(index) {
            out.push(index);
        }
    }
    out
}

/// `value` widened by `rel_bps` on both sides, then further by `extra` bytes.
fn widen(value: u64, rel_bps: u64, extra: u64) -> (u64, u64) {
    let rel_bps = rel_bps.min(BPS);
    let down = value.saturating_mul(BPS - rel_bps) / BPS;
    let up = value.saturating_mul(BPS + rel_bps).div_ceil(BPS);
    (down.saturating_sub(extra), up.saturating_add(extra))
}

/// Spread of a set of per-chunk ratios, in basis points, floored at zero.
/// `values` are `(numerator, denominator)` pairs.
fn ratio_spread_bps(values: &[(u64, u64)]) -> u64 {
    let mut lo = u128::MAX;
    let mut hi = 0u128;
    let mut sum = 0u128;
    let mut count = 0u128;
    for (numerator, denominator) in values {
        if *denominator == 0 {
            continue;
        }
        let ratio = u128::from(*numerator) * BPS as u128 / u128::from(*denominator);
        lo = lo.min(ratio);
        hi = hi.max(ratio);
        sum += ratio;
        count += 1;
    }
    if count == 0 || lo == u128::MAX {
        return 0;
    }
    let mean = sum / count;
    if mean == 0 {
        return 0;
    }
    u64::try_from((hi - lo) * BPS as u128 / mean).unwrap_or(u64::MAX)
}

/// Compress one chunk with the production single-chunk call and return the
/// compressed *body* length, which is what a chunked frame stores.
///
/// A failure returns `0`; the same input will fail again in materialise, and a
/// zero-length sample can only make the estimate smaller (never a false skip).
fn compressed_chunk_len(chunk: &[u8], level: i32, limits: &Limits) -> u64 {
    match encode_zstd(chunk, level, None, limits) {
        Ok(frame) => (frame.len() as u64)
            .saturating_sub((weftcodec::frame::HEADER_LEN + weftcodec::frame::TRAILER_LEN) as u64),
        Err(_) => 0,
    }
}

/// Frame length of a chunked ZSTD frame from its body length.
fn chunked_frame_len(body: u64, chunks: u64) -> u64 {
    (weftcodec::frame::HEADER_LEN as u64)
        .saturating_add(chunks.saturating_mul(weftcodec::frame::CHUNK_ENTRY_LEN as u64))
        .saturating_add(body)
        .saturating_add(weftcodec::frame::TRAILER_LEN as u64)
}

/// Stage 1, level 3 and (optionally) level 19: bounded deterministic sampling.
///
/// `chunk_bytes` must be the codec chunk size the real encoder will use
/// ([`crate::representation::codec_chunk_size`]), because the prediction is
/// about a chunked frame.
pub fn analyse(
    logical: &[u8],
    chunk_bytes: u64,
    price_zstd19: bool,
    limits: &Limits,
) -> Result<Analysis> {
    let size = logical.len() as u64;
    let chunk = chunk_bytes.max(1);
    let chunks = chunk_count(size, chunk);
    let full_chunks = size / chunk;
    // The trailing partial chunk is not sampled (sampling covers whole chunks),
    // so the interval carries its worst case: it cannot compress to more bytes
    // than it spans.
    let remainder = size - full_chunks * chunk;
    let indices = sample_indices(full_chunks, SAMPLE_CHUNKS);

    let mut samples: Vec<SampledChunk> = Vec::with_capacity(indices.len());
    let mut level3_values: Vec<(u64, u64)> = Vec::new();
    let mut ratio_values: Vec<(u64, u64)> = Vec::new();
    let mut sampled_bytes = 0u64;

    for (slot, index) in indices.iter().enumerate() {
        let offset = index.saturating_mul(chunk);
        let end = offset.saturating_add(chunk).min(size);
        if end <= offset {
            continue;
        }
        let window = &logical[offset as usize..end as usize];
        let level3 = compressed_chunk_len(window, crate::representation::CODEC_ZSTD_LEVEL, limits);
        let level19 = if price_zstd19 && slot < SAMPLE_CHUNKS_19 {
            Some(compressed_chunk_len(window, 19, limits))
        } else {
            None
        };
        level3_values.push((level3, window.len() as u64));
        if let Some(value) = level19 {
            ratio_values.push((value, level3.max(1)));
        }
        sampled_bytes = sampled_bytes.saturating_add(window.len() as u64);
        samples.push(SampledChunk {
            index: *index,
            offset,
            len: window.len() as u64,
            level3,
            level19,
        });
    }

    let zstd3 = if size == 0 {
        // The codec rejects an empty ZSTD payload outright (R: 544 fuzz
        // mismatches settled it), so there is nothing to price.
        None
    } else if indices.len() as u64 == full_chunks {
        // Every full chunk was measured; the frame length then follows exactly
        // from the measured chunks plus the trailing partial chunk, which is
        // measured too (at most one extra chunk of work).
        let measured: u64 = samples.iter().map(|sample| sample.level3).sum();
        let tail = if remainder > 0 {
            let offset = full_chunks * chunk;
            compressed_chunk_len(
                &logical[offset as usize..],
                crate::representation::CODEC_ZSTD_LEVEL,
                limits,
            )
        } else {
            0
        };
        let total = chunked_frame_len(measured + tail, chunks);
        Some(SampleEstimate {
            method: "exact",
            level: crate::representation::CODEC_ZSTD_LEVEL,
            predicted: total,
            lower: total,
            upper: total,
            sampled_bytes,
            sampled_chunks: samples.len(),
            total_chunks: chunks,
        })
    } else if level3_values.is_empty() {
        None
    } else {
        let sum_compressed: u64 = level3_values
            .iter()
            .map(|(compressed, _)| *compressed)
            .sum();
        let sum_raw: u64 = level3_values.iter().map(|(_, raw)| *raw).sum();
        if sum_raw == 0 {
            None
        } else {
            // Mean per-chunk ratio applied to the whole tensor.
            let body = (sum_compressed as u128 * size as u128 / sum_raw as u128) as u64;
            let predicted = chunked_frame_len(body, chunks);
            let rel = ZSTD_SAMPLE_ERROR_BPS.max(ratio_spread_bps(&level3_values));
            let (down, up) = widen(predicted, rel, remainder);
            Some(SampleEstimate {
                method: "sampled-chunks",
                level: crate::representation::CODEC_ZSTD_LEVEL,
                predicted,
                lower: down.min(predicted),
                upper: up.max(predicted),
                sampled_bytes,
                sampled_chunks: samples.len(),
                total_chunks: chunks,
            })
        }
    };

    let zstd19 = match (&zstd3, price_zstd19) {
        (Some(base), true) if !ratio_values.is_empty() => {
            let sum_both: u64 = samples
                .iter()
                .filter(|sample| sample.level19.is_some())
                .map(|sample| sample.level3)
                .sum();
            let sum19: u64 = ratio_values.iter().map(|(value, _)| *value).sum();
            if sum_both == 0 || sum19 == 0 {
                None
            } else {
                // The same windows at both levels: the ratio cancels the data's
                // heterogeneity and the level-3 estimate scales it to the frame.
                let scale = |value: u64| -> u64 {
                    (value as u128 * sum19 as u128 / sum_both as u128) as u64
                };
                let predicted = scale(base.predicted).max(1);
                let rel = ZSTD19_RATIO_ERROR_BPS.max(ratio_spread_bps(&ratio_values));
                // The level-3 interval propagates (it already carries the tail
                // slack), and the ratio's own error widens it further.
                let lower_base = widen(scale(base.lower), rel, 0).0;
                let upper_base = widen(scale(base.upper), rel, 0).1;
                let lower = lower_base.min(predicted).max(1);
                let upper = upper_base.max(predicted);
                Some(SampleEstimate {
                    method: "sampled-ratio",
                    level: 19,
                    predicted,
                    lower,
                    upper,
                    sampled_bytes: samples
                        .iter()
                        .filter(|sample| sample.level19.is_some())
                        .map(|sample| sample.len)
                        .sum(),
                    sampled_chunks: ratio_samples(&samples),
                    total_chunks: chunks,
                })
            }
        }
        _ => None,
    };

    Ok(Analysis {
        logical_bytes: size,
        chunk_bytes: chunk,
        raw_bytes: size,
        zstd3,
        zstd19,
        mode9: None,
        samples,
    })
}

fn ratio_samples(samples: &[SampledChunk]) -> usize {
    samples
        .iter()
        .filter(|sample| sample.level19.is_some())
        .count()
}

/// Exact mode-9 prediction: the codec's own exact frame-length function over a
/// full exponent histogram.
pub fn predict_mode9(
    logical: &[u8],
    dtype: FloatDtype,
    shape: &[u64],
    limits: &Limits,
) -> Result<Mode9Prediction> {
    predict_mode9_with(logical, dtype, shape, limits, 1, usize::MAX)
}

/// [`predict_mode9`] with the histogram accumulated on `threads` workers.
///
/// The histogram is the only full-tensor pass in the analyse stage and the
/// predicted length is a pure function of it, so the window split cannot change
/// the prediction: `tests/histogram_parallel.rs` in the codec proves the counts
/// and the predicted length equal the single-pass result, and the R18 decision
/// court (`tests/two_stage_planner.rs`) is unchanged.
pub fn predict_mode9_with(
    logical: &[u8],
    dtype: FloatDtype,
    shape: &[u64],
    limits: &Limits,
    threads: usize,
    min_parallel_bytes: usize,
) -> Result<Mode9Prediction> {
    let histogram = weftcodec::modes::float_tensor_exponent_histogram_with(
        logical,
        dtype,
        threads,
        min_parallel_bytes,
    )?;
    let exact_bytes = float_tensor_encoded_len_from_histogram(dtype, shape, &histogram, limits)?;
    Ok(Mode9Prediction {
        exact_bytes,
        dtype,
        exponent_symbols: histogram.iter().filter(|&&count| count > 0).count(),
        exponent_histogram: Arc::new(histogram),
    })
}

/// The full analyse stage used by the store.
pub fn analyse_tensor(
    logical: &[u8],
    chunk_bytes: u64,
    price_zstd19: bool,
    mode9_dtype: Option<FloatDtype>,
    shape: &[u64],
    limits: &Limits,
) -> Result<Analysis> {
    analyse_tensor_with(
        logical,
        chunk_bytes,
        price_zstd19,
        mode9_dtype,
        shape,
        limits,
        1,
        usize::MAX,
    )
}

/// [`analyse_tensor`] with the exponent histogram accumulated on `threads`
/// workers (R22.2). Semantically identical: the histogram is integer counts and
/// the mode-9 prediction is a pure function of them.
pub fn analyse_tensor_with(
    logical: &[u8],
    chunk_bytes: u64,
    price_zstd19: bool,
    mode9_dtype: Option<FloatDtype>,
    shape: &[u64],
    limits: &Limits,
    mode9_threads: usize,
    mode9_min_parallel_bytes: usize,
) -> Result<Analysis> {
    let mut analysis = analyse(logical, chunk_bytes, price_zstd19, limits)?;
    if let Some(dtype) = mode9_dtype {
        analysis.mode9 = Some(crate::phase_profile::timed(
            crate::phase_profile::Phase::AnalyzeMode9,
            || {
                predict_mode9_with(
                    logical,
                    dtype,
                    shape,
                    limits,
                    mode9_threads,
                    mode9_min_parallel_bytes,
                )
            },
        )?);
    }
    Ok(analysis)
}

/// The shipped mode-9 `1/64` worthwhileness rule, evaluated against a level-3
/// interval.
///
/// The rule (unchanged by R18) accepts mode 9 only when
/// `mode9 * 64 <= zstd * 63`, i.e. it saves at least 1/64 of the level-3 frame.
pub fn mode9_gate(mode9_bytes: u64, zstd3: &SampleEstimate) -> Mode9Gate {
    let scaled = mode9_bytes.saturating_mul(64);
    if scaled <= zstd3.lower.saturating_mul(63) {
        Mode9Gate::Pass
    } else if scaled > zstd3.upper.saturating_mul(63) {
        Mode9Gate::Fail
    } else {
        Mode9Gate::Unresolved
    }
}

#[derive(Clone, Copy)]
struct ExactBest {
    cost: u128,
    physical: u64,
    name: &'static str,
}

impl ExactBest {
    fn new(physical: u64, fixed: u128, name: &'static str) -> Self {
        ExactBest {
            cost: u128::from(physical) + fixed,
            physical,
            name,
        }
    }

    fn consider(&mut self, physical: u64, fixed: u128, name: &'static str) {
        let cost = u128::from(physical) + fixed;
        if cost < self.cost {
            self.cost = cost;
            self.physical = physical;
            self.name = name;
        }
    }
}

/// Stage 2a: decide level 3 and the mode-9 gate **before** any encode.
///
/// * Level 3 is encoded when its trusted lower bound could still beat the best
///   exact candidate, or when it is needed to resolve the mode-9 gate.
/// * Mode 9 is priced from its **exact** prediction against the level-3
///   interval: pass, fail, or unresolved (which forces the level-3 encode).
///
/// Level 19 is deliberately *not* decided here; see [`refine_level19`] and
/// [`select_level19`]. Its bound is only meaningful against a measured
/// incumbent.
pub fn select_before_encodes(
    analysis: &Analysis,
    mode9_offered: bool,
    fixed: FixedCosts,
) -> Decision {
    let mode9 = match (&analysis.mode9, mode9_offered) {
        (Some(prediction), true) => match &analysis.zstd3 {
            Some(zstd3) => mode9_gate(prediction.exact_bytes, zstd3),
            // Without a level-3 sample the historical rule cannot be evaluated;
            // keep the historical unconditional offer.
            None => Mode9Gate::Pass,
        },
        _ => Mode9Gate::Fail,
    };

    // The best candidate whose size is already exact: raw, and mode 9 when its
    // gate passes (its size is exact whether or not it is encoded).
    let mut exact_best = ExactBest::new(analysis.raw_bytes, fixed.raw, "raw");
    if mode9 == Mode9Gate::Pass {
        if let Some(prediction) = &analysis.mode9 {
            exact_best.consider(prediction.exact_bytes, fixed.mode9, "mode-9 float-tensor");
        }
    }

    let mut zstd3_skip_bound = None;
    let need_zstd3 = match &analysis.zstd3 {
        None => true,
        Some(zstd3) => {
            if mode9 == Mode9Gate::Unresolved {
                true
            } else if u128::from(zstd3.lower) + fixed.zstd3 <= exact_best.cost {
                true
            } else {
                let lower = u128::from(zstd3.lower) + fixed.zstd3;
                zstd3_skip_bound = Some(format!(
                    "level-3 not encoded: lower bound {} B ({} interval from {} of {} sampled \
                     chunks) cannot beat {} at {} B; the skip is safe by {} B",
                    zstd3.lower,
                    zstd3.interval(),
                    zstd3.sampled_chunks,
                    zstd3.total_chunks,
                    exact_best.name,
                    exact_best.physical,
                    lower - exact_best.cost,
                ));
                false
            }
        }
    };

    Decision {
        need_zstd3,
        zstd3_skip_bound,
        mode9,
    }
}

/// Recompute the level-19 estimate from a **measured** level-3 frame length
/// when one is available.
///
/// The sampled 19/3 ratio is a local, robust measurement. Multiplying it by the
/// measured level-3 size removes the tensor-wide heterogeneity error that the
/// four-chunk level-3 estimate cannot see (the 272 MB embedding is the measured
/// example: its level-3 interval degenerates to `0..324 MB`, while the ratio on
/// the same windows is within a fraction of a percent).
pub fn refine_level19(analysis: &Analysis, measured_zstd3: Option<u64>) -> Option<SampleEstimate> {
    let ratios: Vec<(u64, u64)> = analysis
        .samples
        .iter()
        .filter_map(|sample| {
            sample
                .level19
                .map(|nineteen| (nineteen, sample.level3.max(1)))
        })
        .collect();
    if ratios.is_empty() {
        return analysis.zstd19.clone();
    }
    let sum19: u64 = ratios.iter().map(|(value, _)| *value).sum();
    let sum3: u64 = ratios.iter().map(|(_, value)| *value).sum();
    if sum3 == 0 || sum19 == 0 {
        return analysis.zstd19.clone();
    }
    let rel = ZSTD19_RATIO_ERROR_BPS.max(ratio_spread_bps(&ratios));
    let base = measured_zstd3.or_else(|| analysis.zstd3.as_ref().map(|e| e.predicted))?;
    let predicted = ((base as u128 * sum19 as u128) / sum3 as u128) as u64;
    let predicted = predicted.max(1);
    let (down, up) = widen(predicted, rel, 0);
    let (lower, upper) = match measured_zstd3 {
        Some(_) => (down.min(predicted).max(1), up.max(predicted)),
        None => {
            // The base is itself an estimate: its interval propagates.
            let base = analysis.zstd3.as_ref()?;
            let lower = (base.lower as u128 * sum19 as u128 / sum3 as u128) as u64;
            let upper = (base.upper as u128 * sum19 as u128 / sum3 as u128) as u64;
            let (down, up) = widen(lower.max(1), rel, 0);
            (down.min(predicted).max(1), up.max(upper).max(predicted))
        }
    };
    Some(SampleEstimate {
        method: if measured_zstd3.is_some() {
            "sampled-ratio-on-measured-level3"
        } else {
            "sampled-ratio"
        },
        level: 19,
        predicted,
        lower,
        upper,
        sampled_bytes: analysis
            .samples
            .iter()
            .filter(|sample| sample.level19.is_some())
            .map(|sample| sample.len)
            .sum(),
        sampled_chunks: ratios.len(),
        total_chunks: analysis.zstd3.as_ref().map(|e| e.total_chunks).unwrap_or(0),
    })
}

/// Stage 2b: decide level 19 against a **measured** incumbent.
///
/// Skip iff `estimate.lower + fixed_19 > incumbent.cost`. Both sides are then
/// numbers the receipt can state, so the bound is meaningful rather than
/// vacuous. On this fixture the incumbent is the measured mode-9 frame
/// (180,547,519 B) and the level-19 lower bound is derived from the measured
/// level-3 frame, so the recorded "safe by N B" is a real gap.
pub fn select_level19(
    estimate: Option<&SampleEstimate>,
    fixed_zstd19: u128,
    incumbent: Incumbent,
) -> Level19Decision {
    select_level19_with_margin(estimate, fixed_zstd19, incumbent, 0)
}

/// [`select_level19`] with a byte margin on the *lower* bound (R22).
///
/// A positive margin makes the skip easier (level 19 must be predicted to win
/// by at least `margin` bytes), a negative one makes it harder (it is encoded
/// when it is within `-margin` bytes of the incumbent). `0` is the R18 rule.
/// The margin is a **planner-policy** knob: it cannot change what a frame looks
/// like, only whether an encoder runs, and every decision records the bound it
/// used.
pub fn select_level19_with_margin(
    estimate: Option<&SampleEstimate>,
    fixed_zstd19: u128,
    incumbent: Incumbent,
    margin_bytes: i64,
) -> Level19Decision {
    let Some(estimate) = estimate else {
        return Level19Decision {
            encode: false,
            skip_bound: None,
            estimate: None,
        };
    };
    let lower = if margin_bytes >= 0 {
        u128::from(estimate.lower) + fixed_zstd19 + margin_bytes as u128
    } else {
        (u128::from(estimate.lower) + fixed_zstd19)
            .saturating_sub(margin_bytes.unsigned_abs() as u128)
    };
    if lower > incumbent.cost {
        Level19Decision {
            encode: false,
            skip_bound: Some(format!(
                "level-19 not encoded: lower bound {} B ({} interval, {} of {} sampled chunks) \
                 cannot beat the measured {} at {} B; the skip is safe by {} B{}",
                estimate.lower,
                estimate.interval(),
                estimate.sampled_chunks,
                estimate.total_chunks,
                incumbent.name,
                incumbent.physical,
                lower - incumbent.cost,
                if margin_bytes == 0 {
                    String::new()
                } else {
                    format!(" with a {margin_bytes} B gate margin")
                },
            )),
            estimate: Some(estimate.clone()),
        }
    } else {
        Level19Decision {
            encode: true,
            skip_bound: None,
            estimate: Some(estimate.clone()),
        }
    }
}

/// Human-readable decision stream, for receipts and diagnostics.
pub fn decision_stream(
    analysis: &Analysis,
    decision: &Decision,
    level19: &Level19Decision,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(
        "logical_bytes".to_owned(),
        analysis.logical_bytes.to_string(),
    );
    out.insert("raw_predicted".to_owned(), analysis.raw_bytes.to_string());
    if let Some(estimate) = &analysis.zstd3 {
        out.insert("zstd3_method".to_owned(), estimate.method.to_owned());
        out.insert("zstd3_predicted".to_owned(), estimate.predicted.to_string());
        out.insert("zstd3_interval".to_owned(), estimate.interval());
        out.insert(
            "zstd3_sampled".to_owned(),
            format!(
                "{} of {} chunks, {} bytes",
                estimate.sampled_chunks, estimate.total_chunks, estimate.sampled_bytes
            ),
        );
    }
    if let Some(estimate) = &analysis.zstd19 {
        out.insert("zstd19_method".to_owned(), estimate.method.to_owned());
        out.insert(
            "zstd19_predicted".to_owned(),
            estimate.predicted.to_string(),
        );
        out.insert("zstd19_interval".to_owned(), estimate.interval());
    }
    if let Some(prediction) = &analysis.mode9 {
        out.insert("mode9_exact".to_owned(), prediction.exact_bytes.to_string());
        out.insert(
            "mode9_exponent_symbols".to_owned(),
            prediction.exponent_symbols.to_string(),
        );
    }
    out.insert("encode_zstd3".to_owned(), decision.need_zstd3.to_string());
    if let Some(bound) = &decision.zstd3_skip_bound {
        out.insert("zstd3_skip_bound".to_owned(), bound.clone());
    }
    out.insert(
        "mode9_gate".to_owned(),
        match decision.mode9 {
            Mode9Gate::Pass => "pass".to_owned(),
            Mode9Gate::Fail => "fail".to_owned(),
            Mode9Gate::Unresolved => "unresolved".to_owned(),
        },
    );
    out.insert("encode_zstd19".to_owned(), level19.encode.to_string());
    if let Some(bound) = &level19.skip_bound {
        out.insert("zstd19_skip_bound".to_owned(), bound.clone());
    }
    if let Some(estimate) = &level19.estimate {
        out.insert(
            "zstd19_refined_method".to_owned(),
            estimate.method.to_owned(),
        );
        out.insert(
            "zstd19_refined_predicted".to_owned(),
            estimate.predicted.to_string(),
        );
        out.insert("zstd19_refined_interval".to_owned(), estimate.interval());
    }
    out
}
