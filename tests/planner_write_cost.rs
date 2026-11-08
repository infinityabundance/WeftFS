//! Access-pattern objective: the write term, exact tile-touch accounting, and
//! the mode-18 experimental gate.
//!
//! These tests are the code-level contract behind the access-pattern court.
//! They assert the three review findings directly:
//!
//! 1. a plan pays for its one-time encode (`lambda_write_ns`), so a single
//!    region read cannot select a representation whose encode is 3.25x the
//!    single-frame encode;
//! 2. a region read is priced by the whole tiles it intersects, not by the
//!    logical region bytes;
//! 3. mode 18 is never auto-selected (its fitted fixed term is a measured
//!    lower bound, and the candidate is gated off by default).

use tempfile::tempdir;

use weftfs::identity::Hash32;
use weftfs::representation::TensorElementDtype;
use weftfs::state::State;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{AccessPattern, TensorPlanOptions, TensorStrategy, TileGeometry};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

/// Weight-like bf16: a narrow exponent band with noisy low bytes, so mode 9 and
/// the tiled float modes are genuinely applicable.
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

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    (dir, store)
}

fn plan(
    store: &Store,
    shape: &[u64],
    bytes: &[u8],
    tile_shape: &[u64],
    pattern: AccessPattern,
    lambda: u64,
    lambda_write: Option<u64>,
) -> weftfs::tensor_store::TensorPlan {
    let options = TensorPlanOptions {
        tile_shape: Some(tile_shape.to_vec()),
        lambda_ns: lambda,
        lambda_write_ns: lambda_write.unwrap_or(lambda),
        lambda_write_ns_set: lambda_write.is_some(),
        access_pattern: pattern,
        ..TensorPlanOptions::default()
    };
    store
        .plan_tensor(
            "bf16",
            shape,
            bytes,
            &weftfs::tensor_store::TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .expect("plan")
}

#[test]
fn tile_intersection_counts_whole_tiles() {
    // The Qwen embedding the court uses: [151936, 896] bf16, tile [64, 896].
    let geometry = TileGeometry::new(&[151936, 896], &[64, 896], TensorElementDtype::Bf16).unwrap();
    assert_eq!(geometry.tile_bytes(), 64 * 896 * 2);
    assert_eq!(geometry.total_tiles(), 151936 / 64);
    // One row (1,792 bytes) is one tile.
    assert_eq!(geometry.tiles_intersected(1792), 1);
    // 100 rows (179,200 bytes) crosses into the second tile row.
    assert_eq!(geometry.tiles_intersected(179_200), 2);
    // The whole tensor touches every tile.
    assert_eq!(geometry.tiles_intersected(272_269_312), 2374);
    // A 64-byte prefix is still one tile.
    assert_eq!(geometry.tiles_intersected(64), 1);

    // A tile that splits the columns: one row touches four tiles, not one.
    let split =
        TileGeometry::new(&[151936, 896], &[128, 224], TensorElementDtype::Bf16).expect("geometry");
    assert_eq!(split.tiles_intersected(1792), 4);
    // 100 rows stay inside one 128-row tile band, so it is still 4 tiles;
    // 200 rows cross into the second band and become 8.
    assert_eq!(split.tiles_intersected(179_200), 4);
    assert_eq!(split.tiles_intersected(179_200 * 2), 8);

    // A small tensor with a 32-column tile: 64 bytes is 32 elements, exactly
    // one tile, not all 16 column tiles of the row.
    let small = TileGeometry::new(&[512, 512], &[16, 32], TensorElementDtype::Bf16).unwrap();
    assert_eq!(small.tiles_intersected(64), 1);
    assert_eq!(small.tiles_intersected(32), 1);
    assert_eq!(small.tiles_intersected(33), 1); // 17 elements stay in column tile 0
    assert_eq!(small.tiles_intersected(65), 2); // 33 elements cross into column tile 1
    assert_eq!(small.tiles_intersected(512 * 2), 16);
}

#[test]
fn write_term_prevents_a_single_read_selecting_the_tiled_form() {
    let (_dir, store) = store();
    let shape = [512u64, 512];
    let bytes = weight_like_bf16(512 * 512, 0x1234);
    let tile = [16u64, 32];

    // Bytes-only, whole tensor: the single mode-9 frame wins.
    let whole = plan(&store, &shape, &bytes, &tile, AccessPattern::Whole, 0, None);
    assert_eq!(whole.chosen, TensorStrategy::FloatTensor);

    // One read with a latency-priced objective. The tiled form's decode is
    // cheaper, but its one-time encode is 3.25x, so raw wins.
    let once = plan(
        &store,
        &shape,
        &bytes,
        &tile,
        AccessPattern::Region {
            region_bytes: 64,
            reads_per_write: 1,
        },
        1,
        None,
    );
    assert_eq!(
        once.chosen,
        TensorStrategy::Raw,
        "candidates: {:?}",
        once.candidates
    );
    // The write term is present and nonzero.
    assert!(once.chosen_estimated_encode_ns > 0);

    // Many reads amortise the encode and the tiled float form wins.
    let many = plan(
        &store,
        &shape,
        &bytes,
        &tile,
        AccessPattern::Region {
            region_bytes: 64,
            reads_per_write: 1000,
        },
        1,
        None,
    );
    assert_eq!(
        many.chosen,
        TensorStrategy::FloatTiled,
        "candidates: {:?}",
        many.candidates
    );

    // Find the break-even read count by scanning the same objective. The
    // decision must flip exactly once and stay flipped.
    let mut flips = Vec::new();
    let mut previous = None;
    for reads in 1..=200u64 {
        let plan = plan(
            &store,
            &shape,
            &bytes,
            &tile,
            AccessPattern::Region {
                region_bytes: 64,
                reads_per_write: reads,
            },
            1,
            None,
        );
        if previous != Some(plan.chosen) {
            flips.push((reads, plan.chosen));
            previous = Some(plan.chosen);
        }
    }
    assert!(
        flips.len() >= 2,
        "the objective never changed its mind: {flips:?}"
    );
    let tiled_at = flips
        .iter()
        .find(|(_, strategy)| *strategy == TensorStrategy::FloatTiled)
        .map(|(reads, _)| *reads)
        .expect("tiled must eventually win");
    assert!(tiled_at > 1, "a single read must not select the tiled form");
    // The flip is monotone: once a tiled form is chosen it stays chosen.
    for reads in tiled_at..=200u64 {
        let later = plan(
            &store,
            &shape,
            &bytes,
            &tile,
            AccessPattern::Region {
                region_bytes: 64,
                reads_per_write: reads,
            },
            1,
            None,
        );
        assert!(
            matches!(
                later.chosen,
                TensorStrategy::FloatTiled | TensorStrategy::FloatTiledStreams
            ),
            "reads={reads} regressed to {:?}",
            later.chosen
        );
    }
}

#[test]
fn explicit_write_lambda_is_honoured_and_zero_write_lambda_is_the_old_objective() {
    let (_dir, store) = store();
    let shape = [512u64, 512];
    let bytes = weight_like_bf16(512 * 512, 0x5678);
    let tile = [16u64, 32];
    let pattern = AccessPattern::Region {
        region_bytes: 64,
        reads_per_write: 1000,
    };

    // Write term disabled: the historical decode-only objective picks the
    // tiled form (this is the old behaviour, kept as a control).
    let old = plan(&store, &shape, &bytes, &tile, pattern, 1, Some(0));
    assert!(matches!(
        old.chosen,
        TensorStrategy::FloatTiled | TensorStrategy::FloatTiledStreams
    ));

    // A large explicit write lambda makes the expensive encode dominate even
    // for 1,000 reads.
    let expensive = plan(&store, &shape, &bytes, &tile, pattern, 1, Some(1_000));
    assert_ne!(expensive.chosen, TensorStrategy::FloatTiled);
}

#[test]
fn mode18_fixed_term_is_a_positive_measured_lower_bound() {
    let measured = weftfs::cost_model::streams_measured_coefficients();
    assert!(
        measured.fixed_ns > 0,
        "the mode-18 fixed term must never be zero"
    );
    // The floor is the measured small-region intercept, not an invented value.
    assert_eq!(measured.fixed_ns, 96_601);
    // Every measured decode row is positive, so no tiny-region estimate is
    // free.
    for (class, row) in weftfs::cost_model::table(weftfs::cost_model::CostModelKind::Measured) {
        assert!(row.fixed_ns > 0, "{class:?} has a zero fixed term");
    }
}

#[test]
fn mode18_is_not_auto_selected_even_with_a_tile_shape() {
    let (_dir, store) = store();
    let shape = [64u64, 64];
    let bytes = weight_like_bf16(64 * 64, 0x99);
    let plan = plan(
        &store,
        &shape,
        &bytes,
        &[16, 32],
        AccessPattern::Region {
            region_bytes: 1792,
            reads_per_write: 1,
        },
        1,
        None,
    );
    let candidate = plan
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::FloatTiledStreams)
        .expect("mode-18 candidate is reported");
    assert!(!candidate.applicable);
    assert!(candidate.detail.contains("experimental"));
    assert_ne!(plan.chosen, TensorStrategy::FloatTiledStreams);
}

#[test]
fn root_object_exists_is_reachable_from_the_store_documented_api() {
    // Guard against a silently empty store fixture: the plan path must be able
    // to resolve the root exactly as the court does.
    let (_dir, store) = store();
    assert_eq!(store.resolve_path("/").unwrap(), State::ROOT_ID);
    let _: Hash32 = State::ROOT_ID;
}

#[test]
fn zstd19_candidate_is_gated_by_size_budget_and_policy() {
    let (_dir, store) = store();
    let shape = [1024u64, 1024];
    let bytes = weight_like_bf16(1024 * 1024, 0xab);

    let base = plan(&store, &shape, &bytes, &[16, 32], AccessPattern::Whole, 0, None);
    let levels: Vec<&str> = base
        .candidates
        .iter()
        .filter(|item| item.strategy == TensorStrategy::Zstd && item.applicable)
        .map(|item| item.detail.as_str())
        .collect();
    assert!(
        levels.iter().any(|detail| detail.contains("level-19")),
        "no level-19 candidate: {levels:?}"
    );

    // Disabled: no level-19 detail at all.
    let options = TensorPlanOptions {
        tile_shape: Some(vec![16, 32]),
        zstd19: false,
        ..TensorPlanOptions::default()
    };
    let disabled = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &weftfs::tensor_store::TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .expect("plan");
    assert!(!disabled
        .candidates
        .iter()
        .any(|item| item.detail.contains("level-19")));

    // Below the size threshold: no level-19 candidate.
    let small_shape = [64u64, 64];
    let small = weight_like_bf16(64 * 64, 0xcd);
    let small_plan = plan(
        &store,
        &small_shape,
        &small,
        &[16, 32],
        AccessPattern::Whole,
        0,
        None,
    );
    assert!(!small_plan
        .candidates
        .iter()
        .any(|item| item.detail.contains("level-19")));
}

#[test]
fn zstd_patch_round_trips_exactly_against_a_causal_base() {
    let (_dir, mut store) = store();
    let shape = [512u64, 4096];
    let elements = (shape[0] * shape[1]) as usize;
    let base = weight_like_bf16(elements, 0x11);
    let mut target = base.clone();
    // A single-region change: the case the COPY/INSERT delta loses on when
    // the rest of the tensor is dense.
    for byte in &mut target[512 * 2..512 * 2 + 256] {
        *byte ^= 0x5a;
    }

    let root = store.resolve_path("/").unwrap();
    let v1 = store
        .write_tensor_file(
            root,
            b"weights",
            "bf16",
            &shape,
            &base,
            &weftfs::tensor_store::TensorLayoutSpec::row_major(),
            None,
        )
        .expect("v1");
    let version = store.object_meta(v1).unwrap().version;

    // The planner offers the patch candidate for a causal base.
    let options = TensorPlanOptions {
        zstd_patch: true,
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &target,
            &weftfs::tensor_store::TensorLayoutSpec::row_major(),
            &[version],
            &options,
        )
        .expect("plan");
    let patch = plan
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::ZstdPatch)
        .expect("patch candidate");
    assert!(patch.applicable, "{patch:?}");
    assert!(patch.physical_bytes.unwrap() > 0);

    // Forced write: the patch frame is persisted and decodes exactly.
    let spec = weftfs::tensor_store::TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::ZstdPatch),
        causal: vec![version],
        plan: options,
        ..weftfs::tensor_store::TensorWriteSpec::default()
    };
    // A different path is fine: the causal parent is the version id.
    let v2 = store
        .write_tensor_file_with_spec(
            root,
            b"weights-v2",
            "bf16",
            &shape,
            &target,
            &weftfs::tensor_store::TensorLayoutSpec::row_major(),
            &spec,
        )
        .expect("patch write");
    assert_eq!(store.read_tensor(v2).unwrap(), target);
    let meta = store.tensor_metadata(v2).unwrap();
    assert_eq!(meta.strategy, "zstd-patch");
    let report = store.verify(true).expect("verify");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}
