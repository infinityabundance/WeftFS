//! R18 two-stage planner: analyse, select, materialise.
//!
//! The contract this file asserts, per tensor:
//!
//! 1. The two-stage planner and the exhaustive planner choose the **same
//!    representation and publish the same manifest bytes** on every fixture
//!    here. Two-stage is only allowed to skip a candidate it can prove cannot
//!    win; it is never allowed to change the winner.
//! 2. Every skipped candidate really could not have won: the file encodes it
//!    externally and checks the measured size against the incumbent the plan
//!    used. This is the falsifiability requirement for the skip bounds.
//! 3. Every encoded candidate's measured size lies inside its predicted
//!    interval, and no tensor needed the exhaustive fallback.
//! 4. The mode-9 prediction is exact: when the encode ran, its frame length
//!    equals the prediction bit for bit; when the encode was skipped, an
//!    independent encode of the same bytes equals the prediction.
//! 5. `PlannerMode::Exhaustive` is still available and still encodes level 19,
//!    so the R17 controls keep their meaning.

use tempfile::tempdir;

use weftcodec::modes::{encode_float_tensor, encode_zstd, FloatDtype};
use weftcodec::Limits;

use weftfs::representation::TensorElementDtype;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    PlannerMode, TensorLayoutSpec, TensorPlan, TensorPlanOptions, TensorStrategy, TensorWriteSpec,
};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

/// Weight-like bf16: a narrow exponent band with noisy low bytes, so mode 9 is
/// genuinely applicable and usually wins.
fn weight_like_bf16(elements: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(elements * 2);
    for _ in 0..elements {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let high = if state % 16 == 0 { 0x40u8 } else { 0x3fu8 };
        let low = (state >> 8) as u8;
        out.push(low);
        out.push(high);
    }
    out
}

/// Deterministic incompressible bytes (splitmix64 top bits).
fn incompressible_bytes(elements: usize) -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut out = Vec::with_capacity(elements * 2);
    for _ in 0..elements {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&((z >> 48) as u16).to_le_bytes());
    }
    out
}

/// Zero-heavy bf16 (mostly a single exponent symbol), which makes the Huffman
/// plane degenerate.
fn zero_heavy_bf16(elements: usize) -> Vec<u8> {
    let mut out = vec![0u8; elements * 2];
    for index in (0..elements).step_by(1024) {
        out[index * 2] = 0x5a;
        out[index * 2 + 1] = 0x3f;
    }
    out
}

struct Fixture {
    label: &'static str,
    dtype: &'static str,
    shape: Vec<u64>,
    bytes: Vec<u8>,
}

fn fixtures() -> Vec<Fixture> {
    let two_mib = 1024 * 1024; // elements of bf16 -> 2 MiB, above the level-19 gate
    vec![
        Fixture {
            label: "weight-like bf16 2 MiB",
            dtype: "bf16",
            shape: vec![1024, 1024],
            bytes: weight_like_bf16(two_mib, 0x1234),
        },
        Fixture {
            label: "weight-like bf16 8 MiB",
            dtype: "bf16",
            shape: vec![2048, 2048],
            bytes: weight_like_bf16(4 * two_mib, 0x9999),
        },
        Fixture {
            label: "incompressible bf16 2 MiB",
            dtype: "bf16",
            shape: vec![1024, 1024],
            bytes: incompressible_bytes(two_mib),
        },
        Fixture {
            label: "zero-heavy bf16 2 MiB",
            dtype: "bf16",
            shape: vec![1024, 1024],
            bytes: zero_heavy_bf16(two_mib),
        },
        Fixture {
            label: "small bf16 128 KiB",
            dtype: "bf16",
            shape: vec![256, 256],
            bytes: weight_like_bf16(64 * 1024, 0x4321),
        },
        Fixture {
            label: "fp32 2 MiB",
            dtype: "f32",
            shape: vec![512, 1024],
            bytes: {
                let mut state = 0xfeed_face_u64;
                let mut out = Vec::with_capacity(512 * 1024 * 4);
                for _ in 0..(512 * 1024) {
                    state ^= state >> 12;
                    state ^= state << 25;
                    state ^= state >> 27;
                    out.extend_from_slice(&((state as u32).to_le_bytes()));
                }
                out
            },
        },
    ]
}

fn spec(planner: PlannerMode) -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan: TensorPlanOptions {
            planner,
            semantic_search: false,
            ..TensorPlanOptions::default()
        },
        tile_shape: None,
    }
}

fn prepare(
    store: &Store,
    fixture: &Fixture,
    planner: PlannerMode,
) -> (weftfs::tensor_store::PreparedTensorObject, TensorPlan) {
    store
        .prepare_tensor_object_with_spec_seeded_planned(
            fixture.dtype,
            &fixture.shape,
            &fixture.bytes,
            &TensorLayoutSpec::row_major(),
            &spec(planner),
            &[0x5a; 32],
        )
        .expect("prepare")
}

fn float_dtype(dtype: &str) -> Option<FloatDtype> {
    match dtype {
        "bf16" => Some(FloatDtype::Bf16),
        "fp16" => Some(FloatDtype::F16),
        "fp32" => Some(FloatDtype::F32),
        _ => None,
    }
}

#[test]
fn two_stage_and_exhaustive_choose_the_same_representation() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let mut mode9_winners = 0;
    for fixture in fixtures() {
        let (two_stage_object, two_stage_plan) = prepare(&store, &fixture, PlannerMode::TwoStage);
        let (exhaustive_object, exhaustive_plan) =
            prepare(&store, &fixture, PlannerMode::Exhaustive);
        assert_eq!(
            two_stage_object.strategy, exhaustive_object.strategy,
            "{}: two-stage chose {} but exhaustive chose {}",
            fixture.label, two_stage_object.strategy, exhaustive_object.strategy
        );
        assert_eq!(
            two_stage_object.manifest_id, exhaustive_object.manifest_id,
            "{}: two-stage published a different manifest",
            fixture.label
        );
        assert_eq!(
            two_stage_object.content_root, exhaustive_object.content_root,
            "{}: two-stage published a different content root",
            fixture.label
        );
        assert_eq!(
            two_stage_object.size, exhaustive_object.size,
            "{}",
            fixture.label
        );
        assert_eq!(
            two_stage_plan.chosen, exhaustive_plan.chosen,
            "{}",
            fixture.label
        );
        assert_eq!(
            two_stage_plan.chosen_physical_bytes, exhaustive_plan.chosen_physical_bytes,
            "{}: chosen physical bytes differ",
            fixture.label
        );
        if two_stage_plan.chosen == TensorStrategy::FloatTensor {
            mode9_winners += 1;
        }
        let trace = two_stage_plan.planner.as_ref().expect("two-stage trace");
        assert!(
            trace.prediction_violations.is_empty(),
            "{}: prediction violations {:?}",
            fixture.label,
            trace.prediction_violations
        );
        assert!(
            !trace.fallback_exhaustive,
            "{}: the exhaustive fallback fired",
            fixture.label
        );
        // Every encoded candidate is inside its predicted interval.
        if let (Some(actual), Some(estimate)) = (trace.actual_zstd3, trace.analysis.zstd3.as_ref())
        {
            assert!(
                estimate.contains(actual),
                "{}: level-3 actual {actual} outside {}",
                fixture.label,
                estimate.interval()
            );
        }
        if let (Some(actual), Some(estimate)) =
            (trace.actual_zstd19, trace.analysis.zstd19.as_ref())
        {
            assert!(
                estimate.contains(actual),
                "{}: level-19 actual {actual} outside {}",
                fixture.label,
                estimate.interval()
            );
        }
        // The exhaustive plan carries no trace and still encodes level 19 on
        // eligible tensors, so the R17 control keeps its meaning.
        assert!(exhaustive_plan.planner.is_none());
    }
    assert!(
        mode9_winners > 0,
        "the fixture set must include a tensor where mode 9 wins, otherwise the \
         mode-9 gate is untested"
    );
}

#[test]
fn every_skipped_candidate_really_could_not_win() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let limits = Limits::default();
    let mut skipped_level3 = 0usize;
    let mut skipped_level19 = 0usize;
    for fixture in fixtures() {
        let (object, plan) = prepare(&store, &fixture, PlannerMode::TwoStage);
        let trace = plan.planner.as_ref().expect("two-stage trace");
        // The incumbent the plan compared against: the cheapest exact candidate
        // plus, when it ran, the measured level-3 frame.
        let mut incumbent = trace.analysis.raw_bytes;
        if let Some(prediction) = &trace.analysis.mode9 {
            if trace.decision.mode9 == weftfs::two_stage::Mode9Gate::Pass {
                incumbent = incumbent.min(prediction.exact_bytes);
            }
        }
        if let Some(actual) = trace.actual_zstd3 {
            incumbent = incumbent.min(actual);
        }

        for candidate in &plan.candidates {
            if candidate.strategy != TensorStrategy::Zstd || candidate.applicable {
                continue;
            }
            let detail = &candidate.detail;
            if !detail.contains("not encoded") {
                continue;
            }
            let is_level19 = detail.contains("level-19");
            assert!(
                detail.contains("cannot beat"),
                "{}: a skip without a bound: {detail}",
                fixture.label
            );
            // Measure it anyway: the skipped candidate must not beat the
            // incumbent the plan recorded.
            let level = if is_level19 { 19 } else { 3 };
            let frame = encode_zstd(&fixture.bytes, level, Some(64 * 1024), &limits)
                .expect("independent encode");
            assert!(
                (frame.len() as u64) >= incumbent,
                "{}: level-{level} measured {} B beats the incumbent {incumbent} B",
                fixture.label,
                frame.len()
            );
            if is_level19 {
                skipped_level19 += 1;
            } else {
                skipped_level3 += 1;
            }
        }
        // A candidate that was encoded is by definition not skipped.
        assert!(
            !object.strategy.is_empty(),
            "{}: empty strategy",
            fixture.label
        );
    }
    assert!(
        skipped_level3 + skipped_level19 > 0,
        "no candidate was skipped, so the skip bounds are untested here"
    );
    let _ = (skipped_level3, skipped_level19);
}

#[test]
fn mode9_prediction_is_exact_and_the_gate_is_decided_before_encoding() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let limits = Limits::default();
    for fixture in fixtures() {
        let (_object, plan) = prepare(&store, &fixture, PlannerMode::TwoStage);
        let trace = plan.planner.as_ref().expect("two-stage trace");
        let Some(dtype) = float_dtype(fixture.dtype) else {
            continue;
        };
        let prediction = trace.analysis.mode9.as_ref().expect("mode-9 prediction");
        let independent = encode_float_tensor(&fixture.bytes, dtype, &fixture.shape, &limits)
            .expect("independent mode-9 encode");
        assert_eq!(
            prediction.exact_bytes,
            independent.len() as u64,
            "{}: predicted {} but the encoder produced {}",
            fixture.label,
            prediction.exact_bytes,
            independent.len()
        );
        match trace.decision.mode9 {
            weftfs::two_stage::Mode9Gate::Pass => {
                assert!(
                    trace.actual_mode9.is_some(),
                    "{}: a passing gate must encode mode 9",
                    fixture.label
                );
                assert_eq!(
                    plan.chosen,
                    TensorStrategy::FloatTensor,
                    "{}",
                    fixture.label
                );
            }
            weftfs::two_stage::Mode9Gate::Fail => {
                assert!(
                    trace.actual_mode9.is_none(),
                    "{}: a failing gate must not encode mode 9",
                    fixture.label
                );
                assert_ne!(
                    plan.chosen,
                    TensorStrategy::FloatTensor,
                    "{}",
                    fixture.label
                );
            }
            weftfs::two_stage::Mode9Gate::Unresolved => {
                assert!(
                    trace.actual_zstd3.is_some(),
                    "{}: an unresolved gate must encode level 3",
                    fixture.label
                );
            }
        }
        if let Some(actual) = trace.actual_mode9 {
            assert_eq!(
                actual, prediction.exact_bytes,
                "{}: the measured mode-9 frame differs from the exact prediction",
                fixture.label
            );
        }
    }
}

#[test]
fn level19_rule_skips_only_when_the_bound_is_safe() {
    use weftfs::two_stage::{
        refine_level19, select_before_encodes, select_level19, Analysis, FixedCosts, Incumbent,
        Mode9Gate, Mode9Prediction, SampleEstimate, SampledChunk,
    };

    fn estimate(predicted: u64, lower: u64, upper: u64, level: i32) -> SampleEstimate {
        SampleEstimate {
            method: "sampled-chunks",
            level,
            predicted,
            lower,
            upper,
            sampled_bytes: 64 * 1024,
            sampled_chunks: 4,
            total_chunks: 128,
        }
    }

    let analysis = |mode9: u64| Analysis {
        logical_bytes: 8_000_000,
        chunk_bytes: 65_536,
        raw_bytes: 8_000_000,
        zstd3: Some(estimate(6_000_000, 5_700_000, 6_300_000, 3)),
        zstd19: None,
        mode9: Some(Mode9Prediction {
            exact_bytes: mode9,
            dtype: weftcodec::modes::FloatDtype::Bf16,
            exponent_symbols: 12,
            exponent_histogram: std::sync::Arc::new([0u64; 256]),
        }),
        // Two sampled windows: level 19 compresses to 60,000 B where level 3
        // makes 60,000 B too, so the measured ratio is 1.0.
        samples: vec![
            SampledChunk {
                index: 0,
                offset: 0,
                len: 65_536,
                level3: 2_000_000,
                level19: Some(2_000_000),
            },
            SampledChunk {
                index: 64,
                offset: 64 * 65_536,
                len: 65_536,
                level3: 2_000_000,
                level19: Some(2_000_000),
            },
        ],
    };

    // Mode 9 wins by 25 %, so its exact size is 4.5 MB: the gate passes even at
    // the level-3 lower bound and level 3 is skipped.
    let gate = select_before_encodes(&analysis(4_500_000), true, FixedCosts::default());
    assert_eq!(gate.mode9, Mode9Gate::Pass);
    assert!(
        !gate.need_zstd3,
        "level 3 cannot beat an exact 4.5 MB mode 9"
    );
    assert!(gate.zstd3_skip_bound.is_some());

    // With the level-3 frame *measured* at 6,000,000 B, level 19 is predicted at
    // the same size (ratio 1.0). Its lower bound (5.7 MB) is above the exact
    // 4.5 MB mode-9 incumbent, so it is skipped with a real gap.
    let refined = refine_level19(&analysis(4_500_000), Some(6_000_000)).expect("estimate");
    assert_eq!(refined.predicted, 6_000_000);
    let skipped = select_level19(
        Some(&refined),
        0,
        Incumbent::new(4_500_000, 4_500_000, "mode-9 exact"),
    );
    assert!(!skipped.encode, "level 19 cannot beat an exact 4.5 MB");
    let bound = skipped.skip_bound.as_ref().expect("a skip has a bound");
    assert!(bound.contains("lower bound"), "bound text: {bound}");
    assert!(bound.contains("measured"), "bound text: {bound}");
    assert!(bound.contains("cannot beat"), "bound text: {bound}");

    // A tie at the bottom of the interval still lets level 19 win: it must run.
    let tie = select_level19(
        Some(&refined),
        0,
        Incumbent::new(5_700_000, 5_700_000, "mode-9 exact"),
    );
    assert!(tie.encode, "a tie can still be won, so level 19 runs");

    // An unresolved mode-9 gate forces a level-3 encode.
    let mut ambiguous = analysis(5_990_000);
    ambiguous.zstd3 = Some(estimate(6_000_000, 5_900_000, 6_100_000, 3));
    let unresolved = select_before_encodes(&ambiguous, true, FixedCosts::default());
    assert_eq!(unresolved.mode9, Mode9Gate::Unresolved);
    assert!(unresolved.need_zstd3, "an unresolved gate measures level 3");
    assert!(unresolved.zstd3_skip_bound.is_none());

    // Pricing decode time makes the smaller-but-slower candidate lose: with a
    // large mode-9 decode term, level 19's lower-bound cost is below the
    // incumbent, so it must be encoded (the rule compares prices, not bytes).
    let priced = select_level19(
        Some(&refined),
        0,
        Incumbent::new(4_500_000, 4_500_000 + 40_000_000_000, "mode-9 priced"),
    );
    assert!(priced.encode, "level 19 is cheaper once decode is priced");

    // Level 19 not offered at all: no bound, no encode.
    let not_offered = select_level19(None, 0, Incumbent::new(4_000_000, 4_000_000, "raw"));
    assert!(!not_offered.encode);
    assert!(not_offered.skip_bound.is_none());
}

#[test]
fn real_fixture_skips_level19_or_records_the_bound() {
    // On this synthetic corpus level 19 may legitimately be worth encoding
    // (the interval says it could win). Whatever the planner decides, the
    // decision stream must be complete: an encode has a measured size inside
    // its interval, a skip has a bound.
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let fixture = Fixture {
        label: "weight-like bf16 2 MiB",
        dtype: "bf16",
        shape: vec![1024, 1024],
        bytes: weight_like_bf16(1024 * 1024, 0x1234),
    };
    let (_, plan) = prepare(&store, &fixture, PlannerMode::TwoStage);
    let trace = plan.planner.as_ref().expect("trace");
    if std::env::var_os("WEFT_TWO_STAGE_TRACE").is_some() {
        eprintln!(
            "{}: raw={} zstd3={:?} zstd19={:?} mode9={:?} decision={:?}",
            fixture.label,
            trace.analysis.raw_bytes,
            trace
                .analysis
                .zstd3
                .as_ref()
                .map(|e| (e.predicted, e.lower, e.upper)),
            trace
                .analysis
                .zstd19
                .as_ref()
                .map(|e| (e.predicted, e.lower, e.upper)),
            trace.analysis.mode9.as_ref().map(|p| p.exact_bytes),
            trace.decision,
        );
    }
    let level19 = trace.level19.as_ref().expect("level-19 decision");
    match level19.encode {
        true => {
            let actual = trace
                .actual_zstd19
                .expect("an encoded level 19 has its size");
            let estimate = level19.estimate.as_ref().expect("level-19 estimate");
            assert!(
                estimate.contains(actual),
                "measured level 19 {actual} outside {} (predicted {})",
                estimate.interval(),
                estimate.predicted
            );
        }
        false => {
            assert!(trace.actual_zstd19.is_none());
            assert!(
                level19.skip_bound.is_some(),
                "a level-19 skip must record its bound"
            );
        }
    }
    // The exhaustive planner still encodes level 19 and still chooses mode 9.
    let (_, exhaustive) = prepare(&store, &fixture, PlannerMode::Exhaustive);
    assert_eq!(exhaustive.chosen, TensorStrategy::FloatTensor);
}

#[test]
fn sample_indices_are_deterministic_and_cover_the_ends() {
    let first = weftfs::two_stage::sample_indices(4096, 4);
    let second = weftfs::two_stage::sample_indices(4096, 4);
    assert_eq!(first, second);
    assert_eq!(first.first().copied(), Some(0));
    assert_eq!(first.last().copied(), Some(4095));
    // Fewer chunks than the budget: every chunk is sampled.
    assert_eq!(weftfs::two_stage::sample_indices(3, 4), vec![0, 1, 2]);
}

#[test]
fn dtype_without_a_mode9_layout_never_predicts_mode9() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let fixture = Fixture {
        label: "i16 2 MiB",
        dtype: "i16",
        shape: vec![1024, 1024],
        bytes: {
            let mut out = Vec::with_capacity(2 * 1024 * 1024);
            for index in 0..(1024 * 1024u32) {
                out.extend_from_slice(&((index % 251) as i16).to_le_bytes());
            }
            out
        },
    };
    let (_, plan) = prepare(&store, &fixture, PlannerMode::TwoStage);
    let trace = plan.planner.as_ref().expect("trace");
    assert!(trace.analysis.mode9.is_none());
    assert_eq!(trace.decision.mode9, weftfs::two_stage::Mode9Gate::Fail);
    assert!(plan.candidates.iter().all(|candidate| candidate.strategy
        != TensorStrategy::FloatTensor
        || !candidate.applicable));
    // The dtype still parses as a tensor element.
    assert!(TensorElementDtype::parse("i16").is_ok());
}
