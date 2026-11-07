//! Mode 17 `FLOAT_TILED` in the store: write, whole read, region read,
//! checkpoint shards, and the planner.

use tempfile::tempdir;

use weftfs::format::RepresentationKind;
use weftfs::recovery::read_manifest;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

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

/// BF16-like words with a skewed exponent and a smooth mantissa.
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

fn write_tiled(
    store: &mut Store,
    root: weftfs::identity::Hash32,
    name: &[u8],
) -> weftfs::identity::Hash32 {
    let shape = [64u64, 64];
    let bytes = smooth_bf16(64, 64, 0x51);
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::FloatTiled),
        tile_shape: Some(vec![16, 32]),
        ..TensorWriteSpec::default()
    };
    store
        .write_tensor_file_with_spec(
            root,
            name,
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .expect("mode-17 write")
}

#[test]
fn mode17_round_trips_and_reads_regions() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = write_tiled(&mut store, root, b"tiled");

    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.as_ref().unwrap();
    assert_eq!(rep.kind, RepresentationKind::Tensor);
    assert_eq!(rep.strategy, "float-tiled");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 17);
    assert_eq!(
        store.tensor_metadata(id).unwrap().tile_shape,
        Some(vec![16, 32])
    );

    let whole = store.read_tensor(id).unwrap();
    assert_eq!(whole, smooth_bf16(64, 64, 0x51));

    let row = weftcodec::Region::new(vec![17, 0], vec![18, 64]);
    let read = store.read_tensor_region(id, &row).unwrap();
    assert_eq!(read.bytes, whole[17 * 64 * 2..18 * 64 * 2]);
    // Rows 17..18 crosses two tile columns and one tile row.
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
fn mode17_planner_offers_it_and_bytes_only_prefers_mode9() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let shape = [64u64, 64];
    let bytes = smooth_bf16(64, 64, 0x77);
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
    let strategies: Vec<TensorStrategy> = plan.candidates.iter().map(|c| c.strategy).collect();
    assert!(strategies.contains(&TensorStrategy::FloatTiled));
    // The bytes-only objective never picks the per-tile form here: per-tile
    // tables can only add bytes over the single mode-9 frame.
    assert_ne!(plan.chosen, TensorStrategy::FloatTiled);
}

#[test]
fn checkpoint_float_tiled_shards_restore_exactly() {
    use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
    use weftfs::safetensors::{encode_file, SafetensorsEntry};

    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    let shape = [64u64, 64];
    let payload = smooth_bf16(64, 64, 0x33);
    let container = encode_file(
        &[SafetensorsEntry {
            name: "w",
            dtype: "bf16",
            shape: &shape,
            data: &payload,
        }],
        None,
    )
    .unwrap();
    let path = dir.path().join("model.safetensors");
    std::fs::write(&path, &container).unwrap();

    let report = store
        .write_checkpoint(
            "ft",
            &path,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::FloatTiled,
                tile_shape: Some(vec![16, 32]),
            },
        )
        .expect("checkpoint write");
    assert_eq!(report.tensors.len(), 1);
    assert_eq!(report.tensors[0].object_ids.len(), 1);
    let shard_id = report.tensors[0].object_ids[0];
    let record = store.object_meta(shard_id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.as_ref().unwrap();
    assert_eq!(rep.strategy, "float-tiled");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 17);

    let (restored, restore_report) = store
        .restore_checkpoint_bytes("ft", &CheckpointRestoreOptions::default())
        .expect("restore");
    assert_eq!(restored, container);
    assert!(restore_report.exact_source_file);
    assert_eq!(restore_report.tensors.len(), 1);
}
