//! Checkpoint object model, sharded write, and topology-independent restore.
//!
//! The synthetic fixture is one rank-3 `bf16` tensor `[64, 32, 16]`. It is
//! written 8-way on axis 0 and restored under 8/4/2/1-way sharding, row-major,
//! column-major, and blocked layouts. Every restore must reproduce the exact
//! source safetensors file (header included) and the same
//! `LogicalTensorRoot`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value as Json;
use tempfile::tempdir;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use weftfs::checkpoint::{
    CheckpointRestoreOptions, CheckpointRestoreReport, CheckpointWriteOptions, RestoreTopology,
    ShardStrategy,
};
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

/// Deterministic pseudo-random bytes (xorshift64*).
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

fn tensor_len() -> usize {
    SHAPE.iter().product::<u64>() as usize * 2
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let data = random_bytes(tensor_len(), 0x5eed_1234);
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

fn write_eight_way(store: &mut Store, source: &Path) -> weftfs::checkpoint::CheckpointWriteReport {
    store
        .write_checkpoint(
            "rank3",
            source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
            },
        )
        .unwrap()
}

fn full_options() -> CheckpointRestoreOptions {
    CheckpointRestoreOptions::default()
}

fn restore(
    store: &Store,
    options: &CheckpointRestoreOptions,
) -> (Vec<u8>, CheckpointRestoreReport) {
    store.restore_checkpoint_bytes("rank3", options).unwrap()
}

fn single_then(
    store: &Store,
    options: CheckpointRestoreOptions,
) -> Result<(Vec<u8>, CheckpointRestoreReport), weftfs::Error> {
    store.restore_checkpoint_bytes("rank3", &options)
}

#[test]
fn topology_matrix_reproduces_the_original_container() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    let source_file = fs::read(&source).unwrap();
    let source_sha = safetensors::sha256_file(&source).unwrap();
    let data = random_bytes(tensor_len(), 0x5eed_1234);
    let expected_root = weftcodec::tensor::logical_root(
        weftcodec::TensorDtype::Bf16,
        &SHAPE,
        &data,
        &Limits::default(),
    )
    .unwrap();

    let written = write_eight_way(&mut store, &source);
    assert_eq!(written.shard_objects, 8);
    assert_eq!(written.total_element_bytes, tensor_len() as u64);
    assert!(written.complete);

    let manifest = store.checkpoint_manifest("rank3").unwrap();
    assert_eq!(manifest.tensors.len(), 1);
    let tensor = &manifest.tensors[0];
    assert_eq!(tensor.name, TENSOR);
    assert_eq!(tensor.shape, SHAPE.to_vec());
    assert_eq!(tensor.dtype, "bf16");
    assert_eq!(tensor.logical_root, expected_root);
    assert_eq!(tensor.shards.len(), 8);
    let mut indices: Vec<u32> = tensor.shards.iter().map(|shard| shard.index).collect();
    indices.sort_unstable();
    assert_eq!(indices, (0..8).collect::<Vec<u32>>());
    for (index, shard) in tensor.shards.iter().enumerate() {
        assert_eq!(shard.axis, 0);
        assert_eq!(shard.shards, 8);
        assert_eq!(shard.index as usize, index);
        assert_eq!(shard.byte_len, (tensor_len() / 8) as u64);
        assert_ne!(shard.object_id, weftfs::identity::Hash32::ZERO);
    }
    // Distinct object ids: every shard is its own stored object.
    let ids: std::collections::BTreeSet<_> =
        tensor.shards.iter().map(|shard| shard.object_id).collect();
    assert_eq!(ids.len(), 8);

    let cases: Vec<(&str, Option<RestoreTopology>)> = vec![
        (
            "sharded-8-axis0",
            Some(RestoreTopology::Sharded { axis: 0, shards: 8 }),
        ),
        (
            "sharded-4-axis0",
            Some(RestoreTopology::Sharded { axis: 0, shards: 4 }),
        ),
        (
            "sharded-2-axis1",
            Some(RestoreTopology::Sharded { axis: 1, shards: 2 }),
        ),
        (
            "sharded-1",
            Some(RestoreTopology::Sharded { axis: 0, shards: 1 }),
        ),
        ("row-major", Some(RestoreTopology::RowMajor)),
        ("column-major", Some(RestoreTopology::ColumnMajor)),
        (
            "blocked-16x8x4",
            Some(RestoreTopology::Blocked {
                block_shape: vec![16, 8, 4],
            }),
        ),
        ("stored-topology", None),
    ];
    let mut roots = Vec::new();
    for (label, topology) in cases {
        let options = CheckpointRestoreOptions {
            tensor: None,
            region: None,
            topology: topology.clone(),
        };
        let (out, report) = restore(&store, &options);
        assert_eq!(out, source_file, "{label}: container bytes differ");
        assert!(report.exact_source_file, "{label}: not exact");
        assert!(report.header_reproduced, "{label}: header not reproduced");
        assert_eq!(report.file_sha256, source_sha, "{label}: SHA-256 differs");
        assert_eq!(report.file_bytes, source_file.len() as u64);
        assert_eq!(report.shards_read, 8);
        let restored = &report.tensors[0];
        assert!(restored.full_root_verified && restored.shard_roots_verified);
        assert_eq!(restored.full_logical_root, expected_root);
        if topology.is_none() {
            assert!(
                restored.topology_root.is_none(),
                "{label}: unexpected topology root"
            );
        } else {
            let root = restored
                .topology_root
                .unwrap_or_else(|| panic!("{label}: topology root missing"));
            assert_eq!(root, expected_root, "{label}: topology root differs");
            roots.push((label, root));
        }
    }
    assert!(roots.iter().all(|(_, root)| *root == expected_root));
    assert_eq!(store.verify(true).unwrap().ok, true);
}

#[test]
fn single_tensor_restore_of_a_one_tensor_checkpoint_is_byte_exact() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    let source_sha = safetensors::sha256_file(&source).unwrap();
    write_eight_way(&mut store, &source);

    let options = CheckpointRestoreOptions {
        tensor: Some(TENSOR.to_owned()),
        region: None,
        topology: Some(RestoreTopology::Sharded { axis: 0, shards: 4 }),
    };
    let (out, report) = restore(&store, &options);
    // The container has one tensor, so the synthesized header is identical to
    // the source header; the report measures that instead of assuming it.
    assert!(report.header_reproduced);
    assert!(report.exact_source_file);
    assert_eq!(report.file_sha256, source_sha);
    assert_eq!(report.tensors.len(), 1);
    assert_eq!(report.tensors[0].bytes, tensor_len() as u64);

    let path = dir.path().join("single.safetensors");
    fs::write(&path, &out).unwrap();
    let parsed = SafetensorsFile::parse(&path).unwrap();
    assert_eq!(parsed.tensors.len(), 1);
    let tensor = parsed.tensor(TENSOR).unwrap().clone();
    assert_eq!(tensor.shape, SHAPE.to_vec());
    assert_eq!(tensor.dtype, "bf16");
    let expected = fs::read(&source).unwrap()
        [SafetensorsFile::parse(&source).unwrap().data_start as usize..]
        .to_vec();
    assert_eq!(parsed.read_tensor(&tensor).unwrap(), expected);
}

#[test]
fn partial_restore_of_a_two_tensor_container_rebuilds_the_header() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let weight = random_bytes(tensor_len(), 0x1234);
    let bias = random_bytes(32, 0x9999);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: TENSOR,
                dtype: "bf16",
                shape: &SHAPE,
                data: &weight,
            },
            SafetensorsEntry {
                name: "bias",
                dtype: "bf16",
                shape: &[16],
                data: &bias,
            },
        ],
        None,
    )
    .unwrap();
    let source = dir.path().join("two.safetensors");
    fs::write(&source, &file).unwrap();
    store
        .write_checkpoint(
            "two",
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
            },
        )
        .unwrap();

    let options = CheckpointRestoreOptions {
        tensor: Some(TENSOR.to_owned()),
        region: None,
        topology: None,
    };
    let (out, report) = store.restore_checkpoint_bytes("two", &options).unwrap();
    assert!(!report.header_reproduced, "header must be rebuilt");
    assert!(!report.exact_source_file);
    let path = dir.path().join("partial.safetensors");
    fs::write(&path, &out).unwrap();
    let parsed = SafetensorsFile::parse(&path).unwrap();
    assert_eq!(parsed.tensors.len(), 1);
    let tensor = parsed.tensor(TENSOR).unwrap().clone();
    assert_eq!(parsed.read_tensor(&tensor).unwrap(), weight);

    // The full restore of the same checkpoint is still byte-exact.
    let (full, full_report) = store
        .restore_checkpoint_bytes("two", &CheckpointRestoreOptions::default())
        .unwrap();
    assert!(full_report.exact_source_file);
    assert!(full_report.header_reproduced);
    assert_eq!(full, file);
}

#[test]
fn partial_region_reads_only_intersecting_shards() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    let data = fs::read(&source).unwrap()
        [SafetensorsFile::parse(&source).unwrap().data_start as usize..]
        .to_vec();
    write_eight_way(&mut store, &source);

    // 8-way on axis 0 means rows [0,8), [8,16), ... . Rows 10..20 intersect
    // shards 1 and 2 only.
    let full = restore(
        &store,
        &CheckpointRestoreOptions {
            tensor: Some(TENSOR.to_owned()),
            region: None,
            topology: None,
        },
    );
    assert_eq!(full.1.shards_read, 8);
    assert_eq!(full.1.units_touched, 8);

    let options = CheckpointRestoreOptions {
        tensor: Some(TENSOR.to_owned()),
        region: Some(weftcodec::Region::new(
            vec![10, 0, 0],
            vec![20, SHAPE[1], SHAPE[2]],
        )),
        topology: None,
    };
    let (out, report) = restore(&store, &options);
    assert_eq!(report.shards_read, 2, "expected only shards 1 and 2");
    assert_eq!(report.units_touched, 2);
    assert!(!report.tensors[0].full_root_verified);
    assert!(report.store_bytes_read < full.1.store_bytes_read);

    let path = dir.path().join("slice.safetensors");
    fs::write(&path, &out).unwrap();
    let parsed = SafetensorsFile::parse(&path).unwrap();
    let tensor = parsed.tensor(TENSOR).unwrap().clone();
    assert_eq!(tensor.shape, vec![10, 32, 16]);
    let row = (SHAPE[1] * SHAPE[2] * 2) as usize;
    let got = parsed.read_tensor(&tensor).unwrap();
    let want = data[10 * row..20 * row].to_vec();
    assert_eq!(got, want);

    // A region wholly inside one shard touches exactly one object.
    let inner = CheckpointRestoreOptions {
        tensor: Some(TENSOR.to_owned()),
        region: Some(weftcodec::Region::new(
            vec![9, 0, 0],
            vec![13, SHAPE[1], SHAPE[2]],
        )),
        topology: None,
    };
    let (out, report) = restore(&store, &inner);
    assert_eq!(report.shards_read, 1);
    assert_eq!(report.units_touched, 1);
    fs::write(dir.path().join("inner.safetensors"), &out).unwrap();
    let parsed = SafetensorsFile::parse(&dir.path().join("inner.safetensors")).unwrap();
    let tensor = parsed.tensor(TENSOR).unwrap().clone();
    assert_eq!(
        parsed.read_tensor(&tensor).unwrap(),
        data[9 * row..13 * row].to_vec()
    );
}

#[test]
fn missing_shard_fails_closed_naming_tensor_and_shard() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    write_eight_way(&mut store, &source);
    let checkpoint_dir = store
        .resolve_components(&[b"checkpoints", b"rank3"])
        .unwrap();
    let manifest = store.checkpoint_manifest("rank3").unwrap();
    let shard = manifest.tensors[0].shards[3].clone();
    store
        .unlink(checkpoint_dir, shard.object_name.as_bytes())
        .unwrap();

    let err = single_then(&store, full_options()).unwrap_err();
    assert_eq!(err.class_name(), "NotFound");
    assert!(
        err.message().contains(TENSOR),
        "error must name the tensor: {}",
        err.message()
    );
    assert!(
        err.message().contains("shard 3/8"),
        "error must name the shard: {}",
        err.message()
    );
}

#[test]
fn corrupt_shard_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    write_eight_way(&mut store, &source);
    let manifest = store.checkpoint_manifest("rank3").unwrap();
    let shard = manifest.tensors[0].shards[2].clone();
    let record = store.object_meta(shard.object_id).unwrap();
    let blob = weftfs::recovery::manifest_path(store.root(), record.content);
    let mut bytes = fs::read(&blob).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&blob, &bytes).unwrap();

    let err = single_then(&store, full_options()).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt");
    assert!(err.message().contains(TENSOR), "{}", err.message());
    assert!(err.message().contains("shard 2/8"), "{}", err.message());
}

#[test]
fn manifest_logical_root_mismatch_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    write_eight_way(&mut store, &source);
    let manifest = store.checkpoint_manifest("rank3").unwrap();
    let manifest_id = store
        .resolve_components(&[b"checkpoints", b"rank3", b"manifest"])
        .unwrap();
    let mut value = weftfs::wcb_util::decode(&manifest.canonical_bytes().unwrap()).unwrap();
    if let Value::Map(map) = &mut value {
        if let Some(Value::Array(tensors)) = map.get_mut("tensors") {
            if let Value::Map(tensor) = &mut tensors[0] {
                tensor.insert("logical_root".to_owned(), Value::Bytes(vec![0u8; 32]));
            }
        }
    }
    let tampered = weftfs::wcb_util::encode(&value).unwrap();
    store.write_file(manifest_id, 0, &tampered).unwrap();

    let err = single_then(&store, full_options()).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt");
    assert!(
        err.message().contains("logical root"),
        "error must name the root mismatch: {}",
        err.message()
    );
    assert!(err.message().contains(TENSOR), "{}", err.message());
}

#[test]
fn non_divisible_split_is_rejected_without_publishing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    let err = store
        .write_checkpoint(
            "bad",
            &source,
            &CheckpointWriteOptions {
                shards: 5,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.class_name(), "NonCanonical");
    assert!(
        err.message().contains("no silent padding"),
        "{}",
        err.message()
    );
    assert!(store.list_checkpoints().unwrap().is_empty());
    // Nothing was published for the failed checkpoint.
    assert!(store.resolve_components(&[b"checkpoints", b"bad"]).is_err());
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn cli_checkpoint_write_restore_verify() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    let source = write_container(&dir.path().join("src"));
    let source_bytes = fs::read(&source).unwrap();
    let source_sha = safetensors::sha256_file(&source).unwrap();
    let source_dir = source.parent().unwrap();

    ok(&store, &["init"]);
    let written: Json = json(
        &store,
        &[
            "--json",
            "checkpoint",
            "write",
            "--from-dir",
            source_dir.to_str().unwrap(),
            "--name",
            "cli8",
            "--shards",
            "8",
            "--axis",
            "0",
        ],
    );
    assert_eq!(written["shard_objects"], 8);
    assert_eq!(written["tensors"][0]["name"], TENSOR);

    let restored: Json = json(
        &store,
        &[
            "--json",
            "checkpoint",
            "restore",
            "--name",
            "cli8",
            "--out",
            "/restored",
            "--topology",
            "4",
            "--axis",
            "0",
        ],
    );
    assert_eq!(restored["exact_source_file"], true, "{restored}");
    assert_eq!(restored["file_sha256"], hex::encode(source_sha));
    assert_eq!(restored["store_path"], "/restored/cli8.safetensors");

    let output = ok(&store, &["cat", "/restored/cli8.safetensors"]);
    assert_eq!(output.stdout, source_bytes);

    let listed: Json = json(&store, &["--json", "checkpoint", "list"]);
    assert_eq!(listed["checkpoints"][0]["name"], "cli8");
    assert_eq!(listed["checkpoints"][0]["tensors"], 1);

    let verified: Json = json(
        &store,
        &["--json", "checkpoint", "verify", "--name", "cli8"],
    );
    assert_eq!(verified["tensors_verified"], 1);

    let shown: Json = json(&store, &["--json", "checkpoint", "show", "--name", "cli8"]);
    assert_eq!(shown["manifest"]["schema"], "weft.checkpoint.v1");
    assert_eq!(
        shown["manifest"]["tensors"][0]["shards"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
}

// ---------------------------------------------------------------------------
// CLI helpers
// ---------------------------------------------------------------------------

fn run(store: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
    command.arg("--store").arg(store);
    command.args(args);
    command.output().expect("run weftfs")
}

fn ok(store: &Path, args: &[&str]) -> Output {
    let output = run(store, args);
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(store: &Path, args: &[&str]) -> Json {
    let output = ok(store, args);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid JSON from {args:?}: {err}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}
