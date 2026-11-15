//! Lossless cross-model structural delta and its decline rule.
//!
//! "Near-equal regions across models" are expressed with the **frozen** mode-3
//! `DELTA` payload: a COPY/INSERT op stream where a COPY is only ever emitted
//! for bytes that are byte-identical in the base, and every other byte is
//! carried as an INSERT (an exact residual). Nothing is approximated, and the
//! decoder needs the base to reconstruct the target exactly.
//!
//! This module does **not** add a wire mode. It adds:
//!
//! 1. a policy ([`CrossModelPolicy`]) that decides whether a delta is worth
//!    storing at all, and
//! 2. the typed reasons a delta is declined, so a pair with nothing in common
//!    produces a refusal rather than a bloated delta.
//!
//! The existing per-tensor `WEIGHT_DELTA` (mode 11) and `SPARSE_DELTA` (mode
//! 16) modes are shape- and dtype-exact: they cannot express a delta between
//! tensors with different shapes or dtypes. [`tensor_mode_eligibility`] says so
//! with a typed error instead of a guess.

use weftcodec::Limits;

use crate::error::{Error, Result};

/// Default minimum fraction of the target that must be covered by byte-identical
/// base regions for a delta to be accepted.
pub const DEFAULT_MIN_MATCHED_FRACTION: f64 = 0.20;
/// Default maximum accepted ratio of (delta frame bytes) / (target bytes).
pub const DEFAULT_MAX_FRAME_RATIO: f64 = 0.95;

/// How much of a pair must match, and how small the delta must be, before the
/// delta is stored.
#[derive(Clone, Copy, Debug)]
pub struct CrossModelPolicy {
    pub min_matched_fraction: f64,
    pub max_frame_ratio: f64,
}

impl Default for CrossModelPolicy {
    fn default() -> Self {
        CrossModelPolicy {
            min_matched_fraction: DEFAULT_MIN_MATCHED_FRACTION,
            max_frame_ratio: DEFAULT_MAX_FRAME_RATIO,
        }
    }
}

impl CrossModelPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.min_matched_fraction) {
            return Err(Error::invalid_argument(
                "min_matched_fraction must be within 0..=1",
            ));
        }
        if !(0.0..1.0).contains(&self.max_frame_ratio) || self.max_frame_ratio <= 0.0 {
            return Err(Error::invalid_argument(
                "max_frame_ratio must be within (0, 1)",
            ));
        }
        Ok(())
    }
}

/// Statistics of one DELTA op stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaOpStats {
    pub copy_ops: u64,
    pub insert_ops: u64,
    pub copy_bytes: u64,
    pub insert_bytes: u64,
}

impl DeltaOpStats {
    pub fn ops(&self) -> u64 {
        self.copy_ops + self.insert_ops
    }

    pub fn target_bytes(&self) -> u64 {
        self.copy_bytes + self.insert_bytes
    }

    /// Fraction of the target carried by byte-identical COPY regions.
    pub fn matched_fraction(&self) -> f64 {
        let total = self.target_bytes();
        if total == 0 {
            0.0
        } else {
            self.copy_bytes as f64 / total as f64
        }
    }
}

/// Parse a mode-3 COPY/INSERT op stream into counts.
///
/// This is the same encoding `encode_delta_ops` produces: `0x00 varint(pos)
/// varint(len)` is COPY, `0x01 varint(len) bytes` is INSERT. A malformed stream
/// is a typed error, never a silent partial count.
pub fn parse_delta_ops(body: &[u8]) -> Result<DeltaOpStats> {
    let mut stats = DeltaOpStats::default();
    let mut position = 0usize;
    while position < body.len() {
        let opcode = body[position];
        position += 1;
        match opcode {
            0x00 => {
                let (_, next) = weftcodec::varint::decode_at(body, position)?;
                position = next;
                let (len, next) = weftcodec::varint::decode_at(body, position)?;
                position = next;
                stats.copy_ops += 1;
                stats.copy_bytes += len;
            }
            0x01 => {
                let (len, next) = weftcodec::varint::decode_at(body, position)?;
                position = next;
                let end = position
                    .checked_add(len as usize)
                    .ok_or_else(|| Error::malformed("delta INSERT length overflows"))?;
                if end > body.len() {
                    return Err(Error::malformed(
                        "delta INSERT runs past the end of the op stream",
                    ));
                }
                position = end;
                stats.insert_ops += 1;
                stats.insert_bytes += len;
            }
            other => {
                return Err(Error::malformed(format!(
                    "unknown delta opcode {other:#04x}"
                )))
            }
        }
    }
    Ok(stats)
}

/// One measured cross-model delta.
#[derive(Clone, Debug)]
pub struct CrossModelDelta {
    /// Exact decoded target length.
    pub target_bytes: u64,
    /// Base length the frame depends on.
    pub base_bytes: u64,
    /// Whole mode-3 frame, header and dependency entry included.
    pub frame_bytes: u64,
    pub ops: DeltaOpStats,
    /// `frame_bytes / target_bytes`.
    pub frame_ratio: f64,
}

/// The decision the policy reaches for one pair.
#[derive(Clone, Debug)]
pub enum CrossModelDecision {
    Accept(CrossModelDelta),
    Decline(DeclineReason),
}

impl CrossModelDecision {
    pub fn accepted(&self) -> Option<&CrossModelDelta> {
        match self {
            CrossModelDecision::Accept(delta) => Some(delta),
            CrossModelDecision::Decline(_) => None,
        }
    }
}

/// Why a delta was declined. Each variant is a distinct, actionable reason.
#[derive(Clone, Debug, PartialEq)]
pub enum DeclineReason {
    /// Fewer than `min_matched_fraction` of the target bytes are byte-identical
    /// to a base region.
    NothingInCommon {
        matched_fraction: f64,
        required: f64,
    },
    /// The delta frame is not smaller than the target by the required margin.
    FrameTooLarge {
        frame_bytes: u64,
        target_bytes: u64,
        ratio: f64,
        max_ratio: f64,
    },
    /// The pair cannot be compared at all (empty target, or a base that is not
    /// addressable).
    Incomparable { reason: String },
    /// The codec refused the frame (a typed limit or malformation).
    Refused { class: String, message: String },
    /// The per-tensor modes cannot express this pair.
    TensorModeIneligible { reason: String },
}

impl DeclineReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeclineReason::NothingInCommon { .. } => "nothing-in-common",
            DeclineReason::FrameTooLarge { .. } => "frame-too-large",
            DeclineReason::Incomparable { .. } => "incomparable",
            DeclineReason::Refused { .. } => "refused",
            DeclineReason::TensorModeIneligible { .. } => "tensor-mode-ineligible",
        }
    }
}

/// Build a mode-3 DELTA frame for `target` against `base` and decide whether it
/// should be stored under `policy`.
///
/// The frame is built even when the policy will decline it, because the
/// decision must be based on measured bytes rather than a heuristic. A caller
/// that only needs a cheap pre-check can compare lengths first; the court
/// measures the real frame.
pub fn plan_cross_model_delta(
    target: &[u8],
    base: &[u8],
    policy: CrossModelPolicy,
) -> CrossModelDecision {
    if let Err(err) = policy.validate() {
        return CrossModelDecision::Decline(DeclineReason::Incomparable {
            reason: err.message().to_owned(),
        });
    }
    if target.is_empty() {
        return CrossModelDecision::Decline(DeclineReason::Incomparable {
            reason: "the target is empty".to_owned(),
        });
    }
    let limits = Limits::default();
    let body = match weftcodec::modes::encode_delta_ops(target, base, &limits) {
        Ok(body) => body,
        Err(err) => {
            return CrossModelDecision::Decline(DeclineReason::Refused {
                class: err.class().as_str().to_owned(),
                message: err.message().to_owned(),
            })
        }
    };
    let ops = match parse_delta_ops(&body) {
        Ok(stats) => stats,
        Err(err) => {
            return CrossModelDecision::Decline(DeclineReason::Refused {
                class: err.class().as_str().to_owned(),
                message: err.message().to_owned(),
            })
        }
    };
    if ops.target_bytes() != target.len() as u64 {
        return CrossModelDecision::Decline(DeclineReason::Refused {
            class: "Corrupt".to_owned(),
            message: format!(
                "op stream covers {} of {} target bytes",
                ops.target_bytes(),
                target.len()
            ),
        });
    }
    let frame = match weftcodec::modes::encode_delta(target, base, &limits) {
        Ok(frame) => frame,
        Err(err) => {
            return CrossModelDecision::Decline(DeclineReason::Refused {
                class: err.class().as_str().to_owned(),
                message: err.message().to_owned(),
            })
        }
    };
    let frame_bytes = frame.len() as u64;
    let delta = CrossModelDelta {
        target_bytes: target.len() as u64,
        base_bytes: base.len() as u64,
        frame_bytes,
        ops,
        frame_ratio: frame_bytes as f64 / target.len() as f64,
    };
    let matched = delta.ops.matched_fraction();
    if matched < policy.min_matched_fraction {
        return CrossModelDecision::Decline(DeclineReason::NothingInCommon {
            matched_fraction: matched,
            required: policy.min_matched_fraction,
        });
    }
    if delta.frame_ratio >= policy.max_frame_ratio {
        return CrossModelDecision::Decline(DeclineReason::FrameTooLarge {
            frame_bytes,
            target_bytes: target.len() as u64,
            ratio: delta.frame_ratio,
            max_ratio: policy.max_frame_ratio,
        });
    }
    CrossModelDecision::Accept(delta)
}

/// Whether the per-tensor `WEIGHT_DELTA` (11) / `SPARSE_DELTA` (16) modes can
/// express a delta between two tensors.
///
/// Both modes address element words under a shared dtype and shape, so a pair
/// with a different shape or dtype is ineligible. This is a typed refusal, not
/// a silent fallback to a byte-level delta.
pub fn tensor_mode_eligibility(
    base_dtype: &str,
    base_shape: &[u64],
    target_dtype: &str,
    target_shape: &[u64],
) -> Result<()> {
    if base_dtype != target_dtype {
        return Err(Error::invalid_argument(format!(
            "weight-delta/sparse-delta need one dtype: base is `{base_dtype}`, target is `{target_dtype}`"
        )));
    }
    if base_shape != target_shape {
        return Err(Error::invalid_argument(format!(
            "weight-delta/sparse-delta need one shape: base is {base_shape:?}, target is {target_shape:?}"
        )));
    }
    Ok(())
}

/// Total bytes retained for a family that stores `base_frame_bytes` once and
/// one delta frame per target. Metadata is included: it is part of the frame.
pub fn family_retained_bytes(base_frame_bytes: u64, delta_frame_bytes: &[u64]) -> u64 {
    base_frame_bytes + delta_frame_bytes.iter().sum::<u64>()
}
