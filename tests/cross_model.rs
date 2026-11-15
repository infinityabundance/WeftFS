//! Cross-model lossless structural delta (T051): accept and decline cases.

use weftcodec::frame::Frame;
use weftcodec::Limits;

use weftfs::cross_model::{
    parse_delta_ops, plan_cross_model_delta, tensor_mode_eligibility, CrossModelDecision,
    CrossModelPolicy, DeclineReason,
};
use weftfs::error::Class;

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Reconstruct a DELTA frame's bytes, which proves the COPY/INSERT stream is
/// exact and not merely small.
fn decode(frame: &[u8], base: &[u8]) -> Vec<u8> {
    let limits = Limits::default();
    let parsed = Frame::parse(frame, &limits).unwrap();
    weftcodec::modes::decode_delta_ops(parsed.body, base, parsed.header.decoded_len, &limits)
        .unwrap()
}

#[test]
fn an_identical_pair_is_a_tiny_exact_delta() {
    let base = bytes(256 * 1024, 1);
    let decision = plan_cross_model_delta(&base, &base, CrossModelPolicy::default());
    let delta = decision
        .accepted()
        .expect("identical pair must be accepted");
    assert_eq!(delta.ops.insert_bytes, 0);
    assert_eq!(delta.ops.copy_bytes, base.len() as u64);
    assert!(delta.frame_bytes < 4096, "{}", delta.frame_bytes);
    let frame = weftcodec::modes::encode_delta(&base, &base, &Limits::default()).unwrap();
    assert_eq!(decode(&frame, &base), base);
}

#[test]
fn a_sparse_edit_is_accepted_and_exact() {
    let base = bytes(256 * 1024, 2);
    let mut target = base.clone();
    // Change 4 bytes in the middle: an exact residual, not an approximation.
    target[131_072..131_076].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    let decision = plan_cross_model_delta(&target, &base, CrossModelPolicy::default());
    let delta = decision.accepted().expect("sparse edit must be accepted");
    assert_eq!(delta.ops.insert_bytes, 4);
    assert!(delta.ops.matched_fraction() > 0.99);
    let frame = weftcodec::modes::encode_delta(&target, &base, &Limits::default()).unwrap();
    assert_eq!(decode(&frame, &base), target);
}

#[test]
fn a_disjoint_pair_declines_instead_of_storing_a_bloated_delta() {
    let base = bytes(128 * 1024, 3);
    let target = bytes(128 * 1024, 4);
    let decision = plan_cross_model_delta(&target, &base, CrossModelPolicy::default());
    match decision {
        CrossModelDecision::Decline(DeclineReason::NothingInCommon {
            matched_fraction,
            required,
        }) => {
            assert!(matched_fraction < required);
        }
        CrossModelDecision::Decline(other) => panic!("unexpected decline reason: {other:?}"),
        CrossModelDecision::Accept(delta) => panic!(
            "a disjoint pair must decline, got frame {} / target {}",
            delta.frame_bytes, delta.target_bytes
        ),
    }
}

#[test]
fn the_frame_ratio_guard_declines_a_delta_that_is_barely_smaller() {
    let base = bytes(64 * 1024, 5);
    // Half the bytes match in order (so the match fraction passes a 0.2 gate)
    // but the file also carries a large incompressible insert.
    let mut target = base[..32 * 1024].to_vec();
    target.extend_from_slice(&bytes(30 * 1024, 6));
    let policy = CrossModelPolicy {
        min_matched_fraction: 0.2,
        max_frame_ratio: 0.4,
    };
    match plan_cross_model_delta(&target, &base, policy) {
        CrossModelDecision::Decline(DeclineReason::FrameTooLarge {
            ratio, max_ratio, ..
        }) => {
            assert!(ratio >= max_ratio);
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

#[test]
fn different_dtypes_and_shapes_are_ineligible_for_the_tensor_modes() {
    let err = tensor_mode_eligibility("bf16", &[8, 16], "f32", &[8, 16]).unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("dtype"));
    let err = tensor_mode_eligibility("bf16", &[8, 16], "bf16", &[16, 8]).unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("shape"));
    tensor_mode_eligibility("bf16", &[8, 16], "bf16", &[8, 16]).unwrap();

    // A bf16 tensor and an f32 tensor of the same shape have no byte-identical
    // region large enough to matter: the byte-level delta declines too.
    let base = bytes(64 * 1024, 7);
    let target = bytes(64 * 1024, 8);
    let decision = plan_cross_model_delta(&target, &base, CrossModelPolicy::default());
    assert!(
        matches!(decision, CrossModelDecision::Decline(_)),
        "dtype-mismatched bytes must decline"
    );
}

#[test]
fn an_empty_target_is_incomparable() {
    let base = bytes(1024, 9);
    match plan_cross_model_delta(&[], &base, CrossModelPolicy::default()) {
        CrossModelDecision::Decline(DeclineReason::Incomparable { .. }) => {}
        other => panic!("expected Incomparable, got {other:?}"),
    }
}

#[test]
fn a_malformed_op_stream_is_refused() {
    assert!(parse_delta_ops(&[0x01, 0x05, 0x00]).is_err());
    assert!(parse_delta_ops(&[0x00, 0x00]).is_err());
    assert!(parse_delta_ops(&[0x7f]).is_err());
    let stats = parse_delta_ops(&[0x01, 0x02, 0xaa, 0xbb]).unwrap();
    assert_eq!(stats.insert_bytes, 2);
    assert_eq!(stats.copy_bytes, 0);
}

#[test]
fn policies_are_validated() {
    assert!(CrossModelPolicy {
        min_matched_fraction: 1.5,
        ..CrossModelPolicy::default()
    }
    .validate()
    .is_err());
    assert!(CrossModelPolicy {
        max_frame_ratio: 1.5,
        ..CrossModelPolicy::default()
    }
    .validate()
    .is_err());
    CrossModelPolicy::default().validate().unwrap();
}
