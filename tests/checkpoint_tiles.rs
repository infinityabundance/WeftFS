//! Mode-15 tiled checkpoint shards: exact restore and per-tile region reads.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::Region;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::{self, SafetensorsEntry, SafetensorsFile};
use weftfs::store::{Options, Store};

const SHAPE: [u64; 3] = [64, 32, 16];
const TENSOR: &str = "weight";

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

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let len = SHAPE.iter().product::<u64>() as usize * 2;
    let data = random_bytes(len, 0x5eed_1234);
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let path = dir.join("rank3.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn container_payload(container: &[u8]) -> &[u8] {
    let header_len = u64::from_le_bytes(container[0..8].try_into().unwrap()) as usize;
    &container[8 + header_len..]
}

fn expected_slice(source: &Path, start: [u64; 3], end: [u64; 3]) -> Vec<u8> {
    let file = SafetensorsFile::parse(source).unwrap();
    let bytes = file.read_tensor(&file.tensors[0]).unwrap();
    let mut out = Vec::new();
    for row in start[0]..end[0] {
        for col in start[1]..end[1] {
            for depth in start[2]..end[2] {
                let at = (((row * SHAPE[1] + col) * SHAPE[2] + depth) * 2) as usize;
                out.extend_from_slice(&bytes[at..at + 2]);
            }
        }
    }
    out
}

#[test]
fn tiled_checkpoint_restores_exactly_and_reads_only_intersecting_tiles() {
    let dir = tempdir().unwrap();
    let source = write_container(dir.path());
    let source_bytes = fs::read(&source).unwrap();

    let tiled_dir = dir.path().join("tiled");
    let mut tiled = Store::init(&tiled_dir, opts()).unwrap();
    let report = tiled
        .write_checkpoint(
            "rank3",
            &source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(vec![4, 8, 16]),
            },
        )
        .unwrap();
    assert_eq!(report.shard_objects, 8);
    for id in &report.tensors[0].object_ids {
        let meta = tiled.tensor_metadata(*id).unwrap();
        assert_eq!(meta.strategy, "tensor-tiled");
        assert_eq!(meta.shape, vec![8, 32, 16]);
    }

    // A whole-checkpoint restore reproduces the source container byte for byte.
    let (restored, full) = tiled
        .restore_checkpoint_bytes("rank3", &CheckpointRestoreOptions::default())
        .unwrap();
    assert_eq!(restored, source_bytes);
    assert!(full.exact_source_file);

    // The same checkpoint written with the default mode-10 shards.
    let m10_dir = dir.path().join("m10");
    let mut m10 = Store::init(&m10_dir, opts()).unwrap();
    m10.write_checkpoint(
        "rank3",
        &source,
        &CheckpointWriteOptions {
            shards: 8,
            axis: 0,
            filters: Vec::new(),
            strategy: ShardStrategy::TensorSharded,
            tile_shape: None,
        },
    )
    .unwrap();

    // Rows 10..12 land in shard 1; columns 4..12 cross two column tiles;
    // depth 2..6 stays in one depth tile. 2 tiles in a 1 x 4 x 1 grid row.
    let start = [10u64, 4, 2];
    let end = [12u64, 12, 6];
    let region = Region::new(start.to_vec(), end.to_vec());
    let options = CheckpointRestoreOptions {
        tensor: Some(TENSOR.to_owned()),
        region: Some(region),
        topology: None,
    };
    let (tiled_bytes, tiled_report) = tiled.restore_checkpoint_bytes("rank3", &options).unwrap();
    assert_eq!(container_payload(&tiled_bytes), expected_slice(&source, start, end));
    assert_eq!(tiled_report.tensors[0].units_touched, 2);
    assert_eq!(tiled_report.shards_read, 1);

    let (m10_bytes, m10_report) = m10.restore_checkpoint_bytes("rank3", &options).unwrap();
    assert_eq!(container_payload(&m10_bytes), container_payload(&tiled_bytes));
    assert!(
        tiled_report.store_bytes_read < m10_report.store_bytes_read,
        "tiled region read {} bytes, mode 10 {} bytes",
        tiled_report.store_bytes_read,
        m10_report.store_bytes_read
    );

    // A full-shard slice through one tile is one touched unit.
    let one_tile = Region::new(vec![10, 4, 2], vec![11, 8, 6]);
    let (bytes, report) = tiled
        .restore_checkpoint_bytes(
            "rank3",
            &CheckpointRestoreOptions {
                tensor: Some(TENSOR.to_owned()),
                region: Some(one_tile),
                topology: None,
            },
        )
        .unwrap();
    assert_eq!(container_payload(&bytes), expected_slice(&source, [10, 4, 2], [11, 8, 6]));
    assert_eq!(report.tensors[0].units_touched, 1);
}

#[test]
fn tiled_checkpoint_rejects_a_non_dividing_tile_shape() {
    let dir = tempdir().unwrap();
    let source = write_container(dir.path());
    let mut store = Store::init(&dir.path().join("store"), opts()).unwrap();
    let err = store
        .write_checkpoint(
            "bad",
            &source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(vec![3, 8, 16]),
            },
        )
        .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument");
    assert!(err.message().contains("no silent padding"));
}

#[test]
fn tiled_checkpoint_requires_a_tile_shape() {
    let dir = tempdir().unwrap();
    let source = write_container(dir.path());
    let mut store = Store::init(&dir.path().join("store"), opts()).unwrap();
    let err = store
        .write_checkpoint(
            "bad",
            &source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument");
}
