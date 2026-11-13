//! Manifest reuse across several named-tensor restores.
//!
//! `Store::restore_named_tensor_with_manifest` exists so a serving loop that
//! restores several tensors from one checkpoint resolves the manifest once. It
//! must be *exactly* the operation `Store::restore_named_tensor_at` performs,
//! minus the repeated manifest read: same bytes, same per-tensor verification,
//! same region and topology results, and no weaker trust in the manifest.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::Region;
use weftfs::checkpoint::{
    CheckpointWriteOptions, LoadedCheckpointManifest, RestoreTopology, ShardStrategy,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};

const TENSORS: [(&str, [u64; 2], u64); 2] = [
    ("alpha.weight", [32, 8], 0x51ed_0001),
    ("beta.weight", [16, 12], 0x51ed_0002),
];

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

fn tensor_len(shape: [u64; 2]) -> usize {
    shape.iter().product::<u64>() as usize * 2
}

fn write_container(dir: &Path) -> (PathBuf, Vec<(&'static str, Vec<u8>)>) {
    fs::create_dir_all(dir).unwrap();
    let mut payloads = Vec::new();
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for (_, shape, seed) in TENSORS {
        let data = random_bytes(tensor_len(shape), seed);
        blobs.push(data);
    }
    let shapes: Vec<[u64; 2]> = TENSORS.iter().map(|entry| entry.1).collect();
    let entries: Vec<SafetensorsEntry<'_>> = TENSORS
        .iter()
        .enumerate()
        .map(|(index, (name, _, _))| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape: &shapes[index],
            data: &blobs[index],
        })
        .collect();
    for (index, (name, _shape, _seed)) in TENSORS.iter().enumerate() {
        payloads.push((*name, blobs[index].clone()));
    }
    let file = safetensors::encode_file(&entries, None).unwrap();
    let path = dir.join("pair.safetensors");
    fs::write(&path, &file).unwrap();
    (path, payloads)
}

fn write_checkpoint(store: &mut Store, source: &Path) {
    store
        .write_checkpoint(
            "pair",
            source,
            &CheckpointWriteOptions {
                shards: 4,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
}

/// The backend bytes and reads one manifest load costs on this store,
/// measured rather than assumed (a manifest blob is not one flat read).
fn manifest_read_cost(store: &Store, name: &str) -> (u64, u64) {
    let loaded = store.load_checkpoint_manifest(name).unwrap();
    let before = store.backend_stats();
    let bytes = store.read_object_at(None, loaded.manifest_id).unwrap();
    let after = store.backend_stats();
    assert!(!bytes.is_empty());
    (
        after.bytes_read - before.bytes_read,
        (after.reads + after.range_reads) - (before.reads + before.range_reads),
    )
}

#[test]
fn reuse_returns_exactly_what_the_per_call_path_returns() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, payloads) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);

    let loaded: LoadedCheckpointManifest = store.load_checkpoint_manifest("pair").unwrap();
    assert_eq!(loaded.name, "pair");
    assert_eq!(loaded.manifest_root, loaded.manifest.root().unwrap());

    for (name, expected) in &payloads {
        let reused = store
            .restore_named_tensor_with_manifest(None, "pair", &loaded.manifest, name, None, None)
            .unwrap();
        let per_call = store
            .restore_named_tensor_at(None, "pair", name, None, None)
            .unwrap();
        assert_eq!(reused.bytes, *expected);
        assert_eq!(reused.bytes, per_call.bytes);
        assert_eq!(reused.report.sha256, per_call.report.sha256);
        assert_eq!(
            reused.report.full_logical_root,
            per_call.report.full_logical_root
        );
        assert_eq!(
            reused.report.restored_content_root,
            per_call.report.restored_content_root
        );
        // Every per-tensor verification still ran.
        assert!(reused.report.full_root_verified);
        assert!(reused.report.shard_roots_verified);
    }
}

#[test]
fn reuse_covers_regions_and_topologies() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, payloads) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);
    let loaded = store.load_checkpoint_manifest("pair").unwrap();

    let region = Region::new(vec![2, 1], vec![6, 5]);
    let reused = store
        .restore_named_tensor_with_manifest(
            None,
            "pair",
            &loaded.manifest,
            "alpha.weight",
            Some(&region),
            None,
        )
        .unwrap();
    let per_call = store
        .restore_named_tensor_at(None, "pair", "alpha.weight", Some(&region), None)
        .unwrap();
    assert_eq!(reused.bytes, per_call.bytes);
    // A region is a slice, not the whole tensor: its root is deliberately not
    // claimed as a full-tensor root.
    assert!(!reused.report.full_root_verified);
    assert!(!reused.bytes.is_empty());

    let topology = RestoreTopology::ColumnMajor;
    for (name, _) in &payloads {
        let reused = store
            .restore_named_tensor_with_manifest(
                None,
                "pair",
                &loaded.manifest,
                name,
                None,
                Some(&topology),
            )
            .unwrap();
        let per_call = store
            .restore_named_tensor_at(None, "pair", name, None, Some(&topology))
            .unwrap();
        assert_eq!(reused.bytes, per_call.bytes);
        assert_eq!(reused.report.topology_root, per_call.report.topology_root);
        assert!(reused.report.full_root_verified);
    }
}

#[test]
fn reuse_reads_the_manifest_once() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, _) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);
    let (manifest_len, manifest_reads) = manifest_read_cost(&store, "pair");
    assert!(manifest_len > 0);

    // Warm every read path once so the counts below are pure reads.
    store
        .restore_named_tensor_at(None, "pair", "alpha.weight", None, None)
        .unwrap();

    let before = store.backend_stats();
    let loaded = store.load_checkpoint_manifest("pair").unwrap();
    for (name, _, _) in &TENSORS {
        store
            .restore_named_tensor_with_manifest(None, "pair", &loaded.manifest, name, None, None)
            .unwrap();
    }
    let after = store.backend_stats();
    let reused_bytes = after.bytes_read - before.bytes_read;
    let reused_reads = (after.reads + after.range_reads) - (before.reads + before.range_reads);

    let before = store.backend_stats();
    for (name, _, _) in &TENSORS {
        store
            .restore_named_tensor_at(None, "pair", name, None, None)
            .unwrap();
    }
    let after = store.backend_stats();
    let per_call_bytes = after.bytes_read - before.bytes_read;
    let per_call_reads = (after.reads + after.range_reads) - (before.reads + before.range_reads);

    // Two tensors, four shards each: the same object reads either way, and the
    // only difference is the manifest, loaded once instead of once per tensor.
    assert_eq!(
        per_call_reads,
        reused_reads + manifest_reads,
        "reads: reused={reused_reads} per_call={per_call_reads} bytes: reused={reused_bytes} \
         per_call={per_call_bytes} one manifest load costs {manifest_len} B / {manifest_reads} reads"
    );
    assert_eq!(
        per_call_bytes - reused_bytes,
        manifest_len,
        "reads: reused={reused_reads} per_call={per_call_reads} bytes: reused={reused_bytes} \
         per_call={per_call_bytes}"
    );
}

#[test]
fn a_manifest_for_another_checkpoint_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, _) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);
    let loaded = store.load_checkpoint_manifest("pair").unwrap();

    let err = store
        .restore_named_tensor_with_manifest(
            None,
            "someone-else",
            &loaded.manifest,
            "alpha.weight",
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt);
    assert!(err.message().contains("names"));
}

#[test]
fn verification_is_not_weakened_by_reuse() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, _) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);
    let loaded = store.load_checkpoint_manifest("pair").unwrap();

    // A manifest whose declared shard content root is not the stored shard's
    // root must fail, exactly as the per-call path fails.
    let mut forged = loaded.manifest.clone();
    let tensor = forged
        .tensors
        .iter_mut()
        .find(|tensor| tensor.name == "alpha.weight")
        .unwrap();
    tensor.shards[0].content_root = weftfs::identity::content_hash(b"not the shard");
    let err = store
        .restore_named_tensor_with_manifest(None, "pair", &forged, "alpha.weight", None, None)
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt);

    // A manifest whose declared byte length is wrong must fail too.
    let mut forged = loaded.manifest.clone();
    let tensor = forged
        .tensors
        .iter_mut()
        .find(|tensor| tensor.name == "alpha.weight")
        .unwrap();
    tensor.shards[0].byte_len += 8;
    let err = store
        .restore_named_tensor_with_manifest(None, "pair", &forged, "alpha.weight", None, None)
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt);

    // The unmodified manifest still restores: the refusals above are the
    // verification, not a broken fixture.
    store
        .restore_named_tensor_with_manifest(
            None,
            "pair",
            &loaded.manifest,
            "alpha.weight",
            None,
            None,
        )
        .unwrap();
}

#[test]
fn a_pinned_manifest_matches_the_unpinned_one() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let (source, payloads) = write_container(&dir.path().join("src"));
    write_checkpoint(&mut store, &source);
    let head = store.head();

    let loaded = store.load_checkpoint_manifest_at(head, "pair").unwrap();
    let live = store.load_checkpoint_manifest("pair").unwrap();
    assert_eq!(loaded.manifest_root, live.manifest_root);
    assert_eq!(loaded.manifest_id, live.manifest_id);

    for (name, expected) in &payloads {
        let restored = store
            .restore_named_tensor_with_manifest(
                Some(head),
                "pair",
                &loaded.manifest,
                name,
                None,
                None,
            )
            .unwrap();
        assert_eq!(restored.bytes, *expected);
    }
}
