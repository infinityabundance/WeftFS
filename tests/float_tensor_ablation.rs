//! R17 ablation controls: `TensorPlanOptions::{zstd19,float_tensor}`.
//!
//! This test proves the two additive controls remove exactly one candidate:
//!
//! * the default option set builds and prices both candidates (one level-19
//!   ZSTD encode and one mode-9 `FLOAT_TENSOR` encode);
//! * `zstd19 = false` / `float_tensor = false` build neither, so the counted
//!   encodes are exactly zero;
//! * the chosen strategy and the published manifest bytes are unchanged when
//!   the removed candidate was not selected (the control is not a silent
//!   planner change).
//!
//! Phase counts come from the shipped `phase_profile` collector. This test
//! binary contains only this test, so nothing else can add samples.

use tempfile::tempdir;

use weftfs::phase_profile::{self, Phase};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec, WritePolicy,
};

const ELEMENTS: usize = 786_432; // 1.5 MiB of bf16, above the 1 MiB zstd-19 gate

/// Deterministic incompressible bytes: zstd loses to the raw candidate, so
/// neither ablation can change the chosen strategy. splitmix64's **top** 16
/// bits are used; xorshift's low bits are weak and compress, which would make
/// zstd win and confound the test.
fn incompressible_bytes() -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut out = Vec::with_capacity(ELEMENTS * 2);
    for _ in 0..ELEMENTS {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&((z >> 48) as u16).to_le_bytes());
    }
    out
}

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1 << 20,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn count(snapshot: &phase_profile::PhaseSnapshot, phase: Phase) -> u64 {
    snapshot
        .phases
        .get(phase.as_str())
        .map(|totals| totals.count)
        .unwrap_or(0)
}

#[test]
fn ablation_controls_remove_exactly_one_candidate_and_leave_defaults_alone() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let bytes = incompressible_bytes();
    let shape = [ELEMENTS as u64];

    // Default options: both candidates are encoded once.
    phase_profile::begin();
    let default_plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let default_counts = phase_profile::finish();
    assert_eq!(
        count(&default_counts, Phase::EncodeZstd19),
        1,
        "the default plan must run exactly one level-19 encode"
    );
    assert_eq!(
        count(&default_counts, Phase::EncodeFloatTensor),
        1,
        "the default plan must run exactly one mode-9 encode"
    );
    assert_eq!(default_plan.chosen, TensorStrategy::Raw);

    // zstd-19 off.
    let mut no_zstd19 = TensorPlanOptions::default();
    no_zstd19.zstd19 = false;
    phase_profile::begin();
    let zstd19_off = store
        .plan_tensor("bf16", &shape, &bytes, &layout, &[], &no_zstd19)
        .unwrap();
    assert_eq!(zstd19_off.chosen, TensorStrategy::Raw);
    let zstd19_counts = phase_profile::finish();
    assert_eq!(
        count(&zstd19_counts, Phase::EncodeZstd19),
        0,
        "zstd19 = false must run no level-19 encode"
    );
    assert_eq!(
        count(&zstd19_counts, Phase::EncodeFloatTensor),
        1,
        "disabling zstd-19 must not disable mode 9"
    );

    // mode 9 off.
    let mut no_float = TensorPlanOptions::default();
    no_float.float_tensor = false;
    phase_profile::begin();
    let float_off = store
        .plan_tensor("bf16", &shape, &bytes, &layout, &[], &no_float)
        .unwrap();
    assert_eq!(float_off.chosen, TensorStrategy::Raw);
    let float_counts = phase_profile::finish();
    assert_eq!(
        count(&float_counts, Phase::EncodeFloatTensor),
        0,
        "float_tensor = false must run no mode-9 encode"
    );
    assert_eq!(
        count(&float_counts, Phase::EncodeZstd19),
        1,
        "disabling mode 9 must not disable zstd-19"
    );

    // Both off.
    let mut both = TensorPlanOptions::default();
    both.zstd19 = false;
    both.float_tensor = false;
    phase_profile::begin();
    let both_plan = store
        .plan_tensor("bf16", &shape, &bytes, &layout, &[], &both)
        .unwrap();
    let both_counts = phase_profile::finish();
    assert_eq!(count(&both_counts, Phase::EncodeZstd19), 0);
    assert_eq!(count(&both_counts, Phase::EncodeFloatTensor), 0);

    // The skipped candidates name the control, not a generic failure.
    let float_entry = both_plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::FloatTensor)
        .expect("the disabled candidate is still reported");
    assert!(!float_entry.applicable);
    assert!(
        float_entry.detail.contains("disabled"),
        "{}",
        float_entry.detail
    );
    let zstd_entries: Vec<_> = both_plan
        .candidates
        .iter()
        .filter(|candidate| candidate.strategy == TensorStrategy::Zstd)
        .collect();
    assert!(
        zstd_entries
            .iter()
            .any(|candidate| candidate.detail.contains("zstd-19 disabled")),
        "{:?}",
        zstd_entries
            .iter()
            .map(|candidate| candidate.detail.as_str())
            .collect::<Vec<_>>()
    );

    // Choosing is unchanged, and the published identity is unchanged. One
    // prepared object per option set, same seed.
    let seed = [7u8; 32];
    let spec = |plan: TensorPlanOptions| TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan,
        tile_shape: None,
    };
    let prepared_default = store
        .prepare_tensor_object_with_spec_seeded(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &spec(TensorPlanOptions::default()),
            &seed,
        )
        .unwrap();
    let prepared_both = store
        .prepare_tensor_object_with_spec_seeded("bf16", &shape, &bytes, &layout, &spec(both), &seed)
        .unwrap();
    assert_eq!(prepared_default.strategy, "raw");
    assert_eq!(prepared_both.strategy, prepared_default.strategy);
    assert_eq!(prepared_both.manifest_id, prepared_default.manifest_id);
    assert_eq!(prepared_both.content_root, prepared_default.content_root);
    assert_eq!(prepared_both.size, prepared_default.size);
    assert_eq!(prepared_both.attrs, prepared_default.attrs);

    // The traced prepare reports the same decision as the plain prepare and
    // writes the same bytes.
    let (prepared_traced, plan) = store
        .prepare_tensor_object_with_spec_seeded_planned(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &spec(TensorPlanOptions::default()),
            &seed,
        )
        .unwrap();
    assert_eq!(plan.chosen, TensorStrategy::Raw);
    assert_eq!(prepared_traced, prepared_default);
    assert!(!prepared_traced.attrs.is_empty());

    // A Fast/raw write does not consult either option, which is why the B
    // configuration is unaffected by this control.
    let fast = store
        .prepare_tensor_object_with_spec_seeded(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::Raw),
                causal: Vec::new(),
                plan: TensorPlanOptions {
                    write_policy: WritePolicy::Fast,
                    ..TensorPlanOptions::default()
                },
                tile_shape: None,
            },
            &seed,
        )
        .unwrap();
    assert_eq!(fast.strategy, "raw");
    assert_eq!(fast.manifest_id, prepared_default.manifest_id);
}
