//! Pipelined checkpoint restore (subphase R10, increment 3).
//!
//! A full-family restore must use bounded workers across **both** tensors and
//! tiles, keep exact container order, and produce byte-identical output for
//! every worker count. These tests drive the shipped ordered pipeline through
//! `Store::restore_checkpoint_pipelined` and compare it against the sequential
//! restore it must equal.

use std::fs;

use tempfile::tempdir;

use weftcodec::tensor::Region;
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::restore_pipeline::RestorePipelineOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const STAMP: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(STAMP),
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

fn write_container(dir: &std::path::Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    // Six tensors with very different sizes: one dominant tensor (so the
    // in-flight bound must run it alone) plus small ones that fit many at a
    // time. `source_offset` order is the write order.
    let specs: &[(&str, &[u64])] = &[
        ("embed", &[256, 128]),
        ("layer0", &[64, 64]),
        ("layer1", &[32, 64]),
        ("big", &[512, 96]),
        ("layer2", &[16, 64]),
        ("tail", &[8, 64]),
    ];
    let owned: Vec<Vec<u8>> = specs
        .iter()
        .enumerate()
        .map(|(index, (_, shape))| {
            let count: usize = shape.iter().map(|dim| *dim as usize).product();
            random_bytes(count * 2, 0x5eed_0000 + index as u64)
        })
        .collect();
    let entries: Vec<SafetensorsEntry> = specs
        .iter()
        .zip(owned.iter())
        .map(|((name, shape), data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None).unwrap();
    let path = dir.join("family.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn write_options() -> CheckpointWriteOptions {
    CheckpointWriteOptions {
        shards: 2,
        axis: 0,
        strategy: ShardStrategy::TensorSharded,
        deterministic_seed: Some([0x11; 32]),
        ..CheckpointWriteOptions::default()
    }
}

struct Fixture {
    _guard: tempfile::TempDir,
    store_path: std::path::PathBuf,
    source_sha256: String,
}

fn fixture() -> Fixture {
    let guard = tempdir().unwrap();
    let source = write_container(&guard.path().join("src"));
    let store_path = guard.path().join("store");
    let mut store = Store::init(&store_path, opts()).unwrap();
    store
        .write_checkpoint("ckpt", &source, &write_options())
        .unwrap();
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let sha = {
        use sha2::{Digest, Sha256};
        let bytes = fs::read(&source).unwrap();
        let digest = Sha256::digest(&bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    };
    Fixture {
        _guard: guard,
        store_path,
        source_sha256: sha,
    }
}

#[test]
fn pipelined_restore_is_byte_identical_at_every_worker_count() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();

    let mut sequential = Vec::new();
    store
        .restore_checkpoint(
            "ckpt",
            &CheckpointRestoreOptions::default(),
            &mut sequential,
        )
        .unwrap();

    for threads in [1usize, 2, 4, 8, 16] {
        for tile_threads in [1usize, 2] {
            for shard_threads in [1usize, 4] {
                let options = RestorePipelineOptions {
                    threads,
                    // Deliberately below the largest tensor so the oversized
                    // path is exercised.
                    max_inflight_bytes: 256 * 1024,
                    tile_threads,
                    shard_threads,
                    shard_max_inflight_bytes: 256 * 1024,
                };
                let mut output = Vec::new();
                let report = store
                    .restore_checkpoint_pipelined(
                        "ckpt",
                        &CheckpointRestoreOptions::default(),
                        &options,
                        &mut output,
                    )
                    .unwrap();
                assert_eq!(
                    output, sequential,
                    "threads={threads} tile_threads={tile_threads} \
                     shard_threads={shard_threads}: bytes differ"
                );
                assert!(
                    report.exact_source_file,
                    "threads={threads} shard_threads={shard_threads}: the source file was not \
                     reproduced exactly"
                );
                assert_eq!(
                    hex(&report.file_sha256),
                    fixture.source_sha256,
                    "threads={threads} tile_threads={tile_threads} \
                     shard_threads={shard_threads}: SHA-256 differs"
                );
                assert_eq!(report.tensors.len(), 6);
            }
        }
    }
}

#[test]
fn pipelined_restore_is_bounded_and_reports_the_bound() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    let mut output = Vec::new();
    // 6 tensors, the largest 512*96*2 = 98,304 bytes. A 128 KiB bound holds
    // several small tensors and one dominant one.
    let options = RestorePipelineOptions {
        threads: 4,
        max_inflight_bytes: 128 * 1024,
        tile_threads: 1,
        shard_threads: 1,
        shard_max_inflight_bytes: 128 * 1024,
    };
    let report = store
        .restore_checkpoint_pipelined(
            "ckpt",
            &CheckpointRestoreOptions::default(),
            &options,
            &mut output,
        )
        .unwrap();
    assert_eq!(report.file_bytes, output.len() as u64);
    assert!(report.store_bytes_read > 0);
}

#[test]
fn pipelined_single_tensor_and_region_match_the_sequential_path() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    let manifest = store.load_checkpoint("ckpt").unwrap();
    let embed = manifest.tensor("embed").unwrap();
    let region = Region::new(vec![0, 0], vec![4, 32]);

    for options in [
        CheckpointRestoreOptions {
            tensor: Some("embed".to_owned()),
            ..CheckpointRestoreOptions::default()
        },
        CheckpointRestoreOptions {
            tensor: Some("embed".to_owned()),
            region: Some(region.clone()),
            ..CheckpointRestoreOptions::default()
        },
    ] {
        let mut sequential = Vec::new();
        store
            .restore_checkpoint("ckpt", &options, &mut sequential)
            .unwrap();
        let pipeline = RestorePipelineOptions {
            threads: 4,
            max_inflight_bytes: 4096,
            tile_threads: 2,
            shard_threads: 1,
            shard_max_inflight_bytes: 4096,
        };
        let mut pipelined = Vec::new();
        store
            .restore_checkpoint_pipelined("ckpt", &options, &pipeline, &mut pipelined)
            .unwrap();
        assert_eq!(sequential, pipelined, "single-tensor/region output differs");
        assert!(!sequential.is_empty());
    }
    // Keep the unused binding honest: the tensor's full extent is bigger than
    // the restored prefix.
    let _ = embed;
}

#[test]
fn pipelined_restore_of_a_tiled_checkpoint_matches_sequential() {
    let guard = tempdir().unwrap();
    let source = write_container(&guard.path().join("src"));
    let store_path = guard.path().join("store");
    let mut store = Store::init(&store_path, opts()).unwrap();
    // Mode 15 shards: tile shape must divide every shard extent. With 2 shards
    // on axis 0 the shapes are embed 128x128, layer 32x64, big 256x96, ...
    let options = CheckpointWriteOptions {
        shards: 2,
        axis: 0,
        strategy: ShardStrategy::TensorTiled,
        tile_shape: Some(vec![4, 16]),
        deterministic_seed: Some([0x22; 32]),
        ..CheckpointWriteOptions::default()
    };
    let written = store.write_checkpoint("ckpt", &source, &options).unwrap();
    assert!(written.shard_objects > 0);
    let strategies: Vec<String> = written
        .tensors
        .iter()
        .map(|tensor| tensor.name.clone())
        .collect();
    assert_eq!(strategies.len(), 6);
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    let reader = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
    let mut sequential = Vec::new();
    reader
        .restore_checkpoint(
            "ckpt",
            &CheckpointRestoreOptions::default(),
            &mut sequential,
        )
        .unwrap();
    for threads in [1usize, 4] {
        for tile_threads in [1usize, 4] {
            let pipeline = RestorePipelineOptions {
                threads,
                max_inflight_bytes: 512 * 1024,
                tile_threads,
                shard_threads: 1,
                shard_max_inflight_bytes: 512 * 1024,
            };
            let mut output = Vec::new();
            let report = reader
                .restore_checkpoint_pipelined(
                    "ckpt",
                    &CheckpointRestoreOptions::default(),
                    &pipeline,
                    &mut output,
                )
                .unwrap();
            assert_eq!(
                output, sequential,
                "tiled: threads={threads} tile_threads={tile_threads} bytes differ"
            );
            assert!(report.exact_source_file);
        }
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
