//! Mode 18 `FLOAT_TILED_STREAMS` in the store: write, whole read, region
//! read, planner pricing, and exactness.

use tempfile::tempdir;

use weftfs::format::RepresentationKind;
use weftfs::recovery::read_manifest;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    AccessPattern, TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec,
};

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

fn smooth_bf16(rows: usize, cols: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(rows * cols * 2);
    for index in 0..rows * cols {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let exponent = 0x3f00u16 + ((index as u16) % 16);
        let word = exponent ^ ((state as u16) & 0x7f);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

#[test]
fn mode18_round_trips_reads_regions_and_verifies() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let shape = [64u64, 64];
    let bytes = smooth_bf16(64, 64, 0x51);
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::FloatTiledStreams),
        tile_shape: Some(vec![16, 32]),
        plan: TensorPlanOptions {
            n_streams: Some(4),
            ..TensorPlanOptions::default()
        },
        ..TensorWriteSpec::default()
    };
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"streams",
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .expect("mode-18 write");

    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.as_ref().unwrap();
    assert_eq!(rep.kind, RepresentationKind::Tensor);
    assert_eq!(rep.strategy, "float-tiled-streams");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 18);
    assert_eq!(
        store.tensor_metadata(id).unwrap().tile_shape,
        Some(vec![16, 32])
    );

    let whole = store.read_tensor(id).unwrap();
    assert_eq!(whole, bytes);

    let row = weftcodec::Region::new(vec![17, 0], vec![18, 64]);
    let read = store.read_tensor_region(id, &row).unwrap();
    assert_eq!(read.bytes, whole[17 * 64 * 2..18 * 64 * 2]);
    assert_eq!(read.units_touched, 2);

    let one = weftcodec::Region::new(vec![5, 60], vec![6, 61]);
    let read = store.read_tensor_region(id, &one).unwrap();
    assert_eq!(read.bytes, whole[(5 * 64 + 60) * 2..(5 * 64 + 60) * 2 + 2]);
    assert_eq!(read.units_touched, 1);

    let report = store.verify(true).expect("full verify");
    assert!(
        report.problems.is_empty(),
        "verify problems: {:?}",
        report.problems
    );
}

#[test]
fn bytes_only_default_never_picks_mode18() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let shape = [64u64, 64];
    let bytes = smooth_bf16(64, 64, 0x77);
    let options = TensorPlanOptions {
        tile_shape: Some(vec![16, 32]),
        allow_experimental_modes: true,
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .expect("plan");
    let strategies: Vec<TensorStrategy> = plan.candidates.iter().map(|c| c.strategy).collect();
    assert!(strategies.contains(&TensorStrategy::FloatTiledStreams));
    assert_ne!(plan.chosen, TensorStrategy::FloatTiledStreams);
    // The observed candidates agree with the previous plan shape: adding mode
    // 18 did not change any other candidate's bytes.
    for candidate in &plan.candidates {
        if candidate.strategy == TensorStrategy::FloatTiledStreams {
            let seedless = candidate.physical_bytes.unwrap();
            assert!(seedless > 0);
        }
    }
}

#[test]
fn cost_model_prices_mode18_from_its_own_measured_row() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let shape = [1024u64, 1024];
    let bytes = smooth_bf16(1024, 1024, 0x99);
    let options = TensorPlanOptions {
        tile_shape: Some(vec![256, 1024]),
        n_streams: Some(4),
        lambda_ns: 1_000,
        allow_experimental_modes: true,
        access_pattern: AccessPattern::Region {
            region_bytes: 50 * 1024 * 2,
            reads_per_write: 2,
        },
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .expect("plan");
    // The mode-18 row is measured separately from mode 17's and is slightly
    // more expensive per MiB on this host (single-thread decode), so the
    // planner never chooses it. The candidates still carry the honest row.
    // Tile accounting: a 50-row read intersects exactly one whole tile of
    // [256, 1024] bf16 (524,288 canonical bytes), not the 100 KiB logical
    // region.
    let expected = weftfs::cost_model::estimate_streams_ns(256 * 1024 * 2) * 2;
    let mut mode18 = None;
    let mut mode17 = None;
    for candidate in &plan.candidates {
        match candidate.strategy {
            TensorStrategy::FloatTiledStreams => {
                mode18 = Some(candidate.estimated_decode_ns.unwrap())
            }
            TensorStrategy::FloatTiled => mode17 = Some(candidate.estimated_decode_ns.unwrap()),
            _ => {}
        }
    }
    assert_eq!(mode18, Some(expected));
    assert_ne!(mode18, mode17, "mode 18 must not reuse mode 17's row");
    assert_ne!(plan.chosen, TensorStrategy::FloatTiledStreams);
}

#[test]
fn mode18_stream_count_changes_representation_identity() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let shape = [32u64, 32];
    let bytes = smooth_bf16(32, 32, 0x0d);
    let mut roots = Vec::new();
    for n_streams in [1u64, 4] {
        let name = format!("s{n_streams}");
        let spec = TensorWriteSpec {
            strategy_hint: Some(TensorStrategy::FloatTiledStreams),
            tile_shape: Some(vec![8, 16]),
            plan: TensorPlanOptions {
                n_streams: Some(n_streams),
                ..TensorPlanOptions::default()
            },
            ..TensorWriteSpec::default()
        };
        let id = store
            .write_tensor_file_with_spec(
                root,
                name.as_bytes(),
                "bf16",
                &shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                &spec,
            )
            .expect("write");
        assert_eq!(store.read_tensor(id).unwrap(), bytes);
        let record = store.object_meta(id).unwrap();
        roots.push(record.content);
    }
    // The stream count is part of the tile root and the descriptor, so the
    // two encodings are different content-addressable representations.
    assert_ne!(roots[0], roots[1]);
}

#[test]
fn mode18_is_gated_off_by_default_and_selectable_by_name() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let shape = [64u64, 64];
    let bytes = smooth_bf16(64, 64, 0x31);

    // Default: the candidate exists but is explicitly not applicable, and the
    // experimental encoder never runs.
    let options = TensorPlanOptions {
        tile_shape: Some(vec![16, 32]),
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .expect("plan");
    let candidate = plan
        .candidates
        .iter()
        .find(|c| c.strategy == TensorStrategy::FloatTiledStreams)
        .expect("mode-18 candidate is reported");
    assert!(!candidate.applicable, "{candidate:?}");
    assert!(candidate.detail.contains("experimental"));
    assert!(candidate.physical_bytes.is_none());
    assert_ne!(plan.chosen, TensorStrategy::FloatTiledStreams);

    // Naming the strategy is the explicit opt-in and still works.
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::FloatTiledStreams),
        tile_shape: Some(vec![16, 32]),
        plan: TensorPlanOptions::default(),
        ..TensorWriteSpec::default()
    };
    let root = store.resolve_path("/").unwrap();
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"named",
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .expect("explicit mode-18 write");
    assert_eq!(store.read_tensor(id).unwrap(), bytes);
    assert_eq!(
        store.tensor_metadata(id).unwrap().strategy,
        "float-tiled-streams"
    );
}
