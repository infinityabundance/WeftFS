//! Streaming checkpoint ingest: bounded reads must publish the exact same
//! checkpoint as the whole-tensor path.
//!
//! The writer hashes the source tensor in fixed windows and reads one shard at
//! a time. Its identity inputs are the frozen one-shot digests
//! (`identity::content_hash` and `weftcodec::tensor::values_hash`), so the test
//! recomputes those from the whole tensor and requires every emitted manifest
//! field to match, then requires two writes with different ingest bounds and
//! the same deterministic seed to publish byte-identical state.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftcodec::tensor as tc;
use weftcodec::Limits;

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::identity;
use weftfs::safetensors::{self, SafetensorsEntry, SafetensorsFile};
use weftfs::store::{Options, Store};

const TENSORS: [(&str, &str, [u64; 2]); 2] =
    [("alpha", "bf16", [16, 12]), ("beta", "f32", [16, 12])];

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

fn write_container(dir: &Path) -> (PathBuf, Vec<Vec<u8>>) {
    fs::create_dir_all(dir).unwrap();
    let mut payloads = Vec::new();
    for (index, (_name, dtype, shape)) in TENSORS.iter().enumerate() {
        let element = weftcodec::TensorDtype::from_name(dtype)
            .unwrap()
            .element_size() as usize;
        let len = shape.iter().product::<u64>() as usize * element;
        payloads.push(random_bytes(len, 0x51ee_0000 + index as u64));
    }
    let entries: Vec<SafetensorsEntry<'_>> = TENSORS
        .iter()
        .zip(&payloads)
        .map(|((name, dtype, shape), data)| SafetensorsEntry {
            name,
            dtype,
            shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None).unwrap();
    let path = dir.join("streamed.safetensors");
    fs::write(&path, &file).unwrap();
    (path, payloads)
}

fn write(
    store: &mut Store,
    source: &Path,
    seed: [u8; 32],
    max_ingest_bytes: u64,
    shards: u32,
    axis: u32,
) -> weftfs::checkpoint::CheckpointWriteReport {
    store
        .write_checkpoint(
            "streamed",
            source,
            &CheckpointWriteOptions {
                shards,
                axis,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                deterministic_seed: Some(seed),
                max_ingest_bytes,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap()
}

#[test]
fn streaming_ingest_publishes_the_one_shot_identity() {
    let dir = tempdir().unwrap();
    let source_dir = dir.path().join("src");
    let (source, payloads) = write_container(&source_dir);
    let file = SafetensorsFile::parse(&source).unwrap();
    let limits = Limits::default();
    let seed = [0x42u8; 32];

    // The smallest accepted bound is the largest tensor's byte length; the
    // large bound is the default. Both must publish the same state.
    let tiny = payloads.iter().map(|p| p.len() as u64).max().unwrap();
    let mut small_store = Store::init(&dir.path().join("small"), opts()).unwrap();
    let small = write(&mut small_store, &source, seed, tiny, 4, 0);
    let mut large_store = Store::init(&dir.path().join("large"), opts()).unwrap();
    let large = write(&mut large_store, &source, seed, 1024 * 1024 * 1024, 4, 0);

    assert_eq!(small.head, large.head, "head must be seed-determined");
    assert_eq!(small.manifest_id, large.manifest_id);
    assert_eq!(small.manifest_root, large.manifest_root);
    assert_eq!(small.total_element_bytes, large.total_element_bytes);
    assert_eq!(small.shard_objects, large.shard_objects);

    let small_manifest = small_store.load_checkpoint("streamed").unwrap();
    let large_manifest = large_store.load_checkpoint("streamed").unwrap();
    assert_eq!(
        small_manifest.canonical_bytes().unwrap(),
        large_manifest.canonical_bytes().unwrap(),
        "manifest bytes must be identical"
    );
    assert_eq!(
        small_manifest.root().unwrap(),
        large_manifest.root().unwrap()
    );

    // The streaming writer must emit exactly the one-shot whole-tensor
    // identities the previous path computed.
    for ((name, dtype, shape), payload) in TENSORS.iter().zip(&payloads) {
        let physical = weftcodec::TensorDtype::from_name(dtype).unwrap();
        let expected_content = identity::content_hash(payload);
        let expected_logical = tc::logical_root(physical, shape, payload, &limits).unwrap();
        let tensor = small_manifest
            .tensors
            .iter()
            .find(|tensor| tensor.name == *name)
            .unwrap();
        assert_eq!(tensor.content_root, expected_content, "{name} content root");
        assert_eq!(tensor.logical_root, expected_logical, "{name} logical root");
        assert_eq!(tensor.element_bytes, payload.len() as u64);
        // And the source read really is the declared payload.
        let parsed = file.tensor(name).unwrap();
        assert_eq!(file.read_tensor(parsed).unwrap(), *payload);
    }
}

#[test]
fn streaming_shard_reads_match_shard_bytes_and_manifest_roots() {
    let dir = tempdir().unwrap();
    let source_dir = dir.path().join("src");
    let (source, payloads) = write_container(&source_dir);
    let file = SafetensorsFile::parse(&source).unwrap();
    let limits = Limits::default();

    // Axis 1 of both tensors is divisible by 2 and 3.
    let mut store = Store::init(&dir.path().join("store"), opts()).unwrap();
    let report = write(&mut store, &source, [0x17; 32], u64::MAX, 3, 1);
    let manifest = store.load_checkpoint("streamed").unwrap();
    assert_eq!(report.shard_objects, TENSORS.len() * 3);

    for (index, ((name, dtype, shape), payload)) in TENSORS.iter().zip(&payloads).enumerate() {
        let physical = weftcodec::TensorDtype::from_name(dtype).unwrap();
        let parsed = file.tensor(name).unwrap();
        let tensor = manifest
            .tensors
            .iter()
            .find(|tensor| tensor.name == *name)
            .unwrap();
        assert_eq!(tensor.shards.len(), 3);
        for shard in &tensor.shards {
            let direct =
                tc::shard_bytes(physical, shape, 1, 3, shard.index, payload, &limits).unwrap();
            let streamed = file
                .read_tensor_shard(parsed, physical, 1, 3, shard.index, u64::MAX)
                .unwrap();
            assert_eq!(streamed, direct, "{name} shard {}", shard.index);
            assert_eq!(shard.byte_len, streamed.len() as u64);
            assert_eq!(shard.content_root, identity::content_hash(&streamed));
            let shard_shape = tc::shard_shape(shape, 1, 3, shard.index).unwrap();
            assert_eq!(
                shard.logical_root,
                tc::logical_root(physical, &shard_shape, &streamed, &limits).unwrap()
            );
        }
        assert_eq!(tensor.content_root, identity::content_hash(payload));
        let _ = index;
    }
}

#[test]
fn ingest_bound_refuses_without_publishing() {
    let dir = tempdir().unwrap();
    let source_dir = dir.path().join("src");
    let (source, payloads) = write_container(&source_dir);
    let largest = payloads.iter().map(|p| p.len() as u64).max().unwrap();
    let mut store = Store::init(&dir.path().join("store"), opts()).unwrap();
    let generation = store.generation();
    let err = store
        .write_checkpoint(
            "streamed",
            &source,
            &CheckpointWriteOptions {
                shards: 4,
                axis: 0,
                deterministic_seed: Some([0x99; 32]),
                max_ingest_bytes: largest - 1,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.class(), weftfs::error::Class::LimitExceeded);
    assert_eq!(store.generation(), generation, "nothing may be published");
    assert!(store.load_checkpoint("streamed").is_err());
}
