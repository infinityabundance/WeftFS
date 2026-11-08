//! Prepare/commit checkpoint write: N ranks publish immutable blobs, exactly
//! one committer publishes the single commit.
//!
//! The identity proof is a deterministic fixture: the same container written
//! sequentially and through the split path, with the same fixed clock and the
//! same checkpoint seed, must publish byte-identical state — the same head, the
//! same checkpoint manifest bytes, the same object ids.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

use weftfs::checkpoint::{
    CheckpointWriteOptions, ParallelCheckpointOptions, ShardStrategy, MAX_PARALLEL_RANKS,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SEED: [u8; 32] = [0x5a; 32];
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

/// Two tensors so more than one worker rank has real work.
fn fixture(dir: &Path) -> (tempfile::TempDir, PathBuf) {
    let guard = tempdir().unwrap();
    let path = write_container(&guard.path().join(dir_file_name(dir)));
    (guard, path)
}

fn dir_file_name(dir: &Path) -> &str {
    dir.file_name().and_then(|name| name.to_str()).unwrap_or("src")
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let a = random_bytes(32 * 16 * 2, 0x5eed_1234);
    let b = random_bytes(16 * 16 * 2, 0x5eed_5678);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: "alpha",
                dtype: "bf16",
                shape: &[32, 16],
                data: &a,
            },
            SafetensorsEntry {
                name: "beta",
                dtype: "bf16",
                shape: &[16, 16],
                data: &b,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("pair.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn write_options() -> CheckpointWriteOptions {
    CheckpointWriteOptions {
        shards: 8,
        axis: 0,
        strategy: ShardStrategy::TensorSharded,
        deterministic_seed: Some(SEED),
        ..CheckpointWriteOptions::default()
    }
}

fn parallel(ranks: usize) -> ParallelCheckpointOptions {
    ParallelCheckpointOptions {
        ranks,
        ..ParallelCheckpointOptions::default()
    }
}

fn manifest_bytes(store: &Store, name: &str) -> Vec<u8> {
    let id = store
        .resolve_path(&format!("/checkpoints/{name}/manifest"))
        .unwrap();
    store.read_object(id).unwrap()
}

#[test]
fn parallel_and_sequential_publish_identical_state() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));

    let seq_dir = dir.path().join("seq");
    let mut seq = Store::init(&seq_dir, opts()).unwrap();
    let seq_report = seq
        .write_checkpoint("ckpt", &source, &write_options())
        .unwrap();

    let par_dir = dir.path().join("par");
    let mut par = Store::init(&par_dir, opts()).unwrap();
    let par_report = par
        .write_checkpoint_parallel("ckpt", &source, &write_options(), &parallel(4))
        .unwrap();

    assert_eq!(
        seq_report.head, par_report.head,
        "sequential and 4-rank head must be byte-identical"
    );
    assert_eq!(seq_report.checkpoint_id, par_report.checkpoint_id);
    assert_eq!(seq_report.manifest_id, par_report.manifest_id);
    assert_eq!(seq_report.manifest_root, par_report.manifest_root);
    assert_eq!(
        manifest_bytes(&seq, "ckpt"),
        manifest_bytes(&par, "ckpt"),
        "checkpoint manifest bytes must be identical"
    );
    for (a, b) in seq_report.tensors.iter().zip(par_report.tensors.iter()) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.object_ids, b.object_ids);
        assert_eq!(a.logical_root, b.logical_root);
    }

    // The split path's own reopen/verify must be clean.
    drop(par);
    let reopened = Store::open_with_options(&par_dir, OpenMode::ReadOnly, opts()).unwrap();
    reopened.verify(true).unwrap();
}

#[test]
fn every_rank_count_publishes_the_same_head() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));
    let mut expected: Option<Vec<u8>> = None;
    for ranks in [1usize, 2, 4, 8] {
        let path = dir.path().join(format!("r{ranks}"));
        let mut store = Store::init(&path, opts()).unwrap();
        store
            .write_checkpoint_parallel("ckpt", &source, &write_options(), &parallel(ranks))
            .unwrap();
        let bytes = manifest_bytes(&store, "ckpt");
        match &expected {
            None => expected = Some(bytes),
            Some(first) => assert_eq!(first, &bytes, "ranks={ranks} changed the manifest"),
        }
    }
}

#[test]
fn a_refused_prepare_publishes_nothing_and_gc_reclaims_orphans() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));
    let path = dir.path().join("store");
    let mut store = Store::init(&path, opts()).unwrap();
    let generation = store.generation();
    let head = store.head();

    // A per-rank budget far below one tensor makes the first shard's task fail
    // after nothing has been published. Blobs already written are orphans.
    let mut bounds = ParallelCheckpointOptions {
        ranks: 2,
        per_rank_bytes: 1,
        ..ParallelCheckpointOptions::default()
    };
    let err = store
        .write_checkpoint_parallel("ckpt", &source, &write_options(), &bounds)
        .unwrap_err();
    assert!(
        err.to_string().contains("per-rank") || err.to_string().contains("budget"),
        "unexpected error: {err}"
    );
    assert_eq!(store.generation(), generation, "no commit may be published");
    assert_eq!(store.head(), head, "head must not move");
    let checkpoints = store.resolve_path("/checkpoints");
    assert!(
        checkpoints.is_err() || store.list_checkpoints().unwrap().is_empty(),
        "no checkpoint directory may exist"
    );

    // Nothing above needs GC: the budget is checked before the first blob. Now
    // prove the general orphan case with a deadline that fires after a shard.
    bounds.per_rank_bytes = u64::MAX;
    bounds.deadline_ms = Some(0);
    let err = store
        .write_checkpoint_parallel("ckpt", &source, &write_options(), &bounds)
        .unwrap_err();
    assert!(
        err.to_string().contains("deadline"),
        "unexpected error: {err}"
    );
    assert_eq!(store.generation(), generation);
    let gc = store.gc(true).unwrap();
    assert!(gc.applied);
    store.verify(true).unwrap();
}

#[test]
fn duplicate_checkpoint_name_fails_closed() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));
    let path = dir.path().join("store");
    let mut store = Store::init(&path, opts()).unwrap();
    let first = store
        .write_checkpoint_parallel("ckpt", &source, &write_options(), &parallel(2))
        .unwrap();
    let generation = store.generation();

    // A second write of the same name (same bytes or not) must fail closed and
    // publish nothing: state already owns the directory name.
    let err = store
        .write_checkpoint_parallel("ckpt", &source, &write_options(), &parallel(2))
        .unwrap_err();
    assert_eq!(
        err.class(),
        weftfs::error::Class::Exists,
        "unexpected error: {err}"
    );
    assert_eq!(store.generation(), generation);
    assert_eq!(store.head(), first.head);
    store.verify(true).unwrap();
}

#[test]
fn rank_and_byte_bounds_are_enforced() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));
    let mut store = Store::init(&dir.path().join("store"), opts()).unwrap();

    let mut bad = ParallelCheckpointOptions::default();
    bad.ranks = 0;
    assert_eq!(
        store
            .write_checkpoint_parallel("ckpt", &source, &write_options(), &bad)
            .unwrap_err()
            .class(),
        weftfs::error::Class::InvalidArgument
    );

    bad.ranks = MAX_PARALLEL_RANKS + 1;
    assert_eq!(
        store
            .write_checkpoint_parallel("ckpt", &source, &write_options(), &bad)
            .unwrap_err()
            .class(),
        weftfs::error::Class::LimitExceeded
    );

    bad.ranks = 2;
    bad.max_in_flight_bytes = 0;
    assert_eq!(
        store
            .write_checkpoint_parallel("ckpt", &source, &write_options(), &bad)
            .unwrap_err()
            .class(),
        weftfs::error::Class::InvalidArgument
    );
}

#[test]
fn a_killed_process_mid_prepare_leaves_no_publication() {
    let dir = tempdir().unwrap();
    let (_source_guard, source) = fixture(Path::new("src"));
    let store_path = dir.path().join("store");
    {
        let store = Store::init(&store_path, opts()).unwrap();
        drop(store);
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .env("WEFT_CHECKPOINT_PREPARE_PAUSE_MS", "400")
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "checkpoint",
            "write",
            "--file",
            source.to_str().unwrap(),
            "--name",
            "ckpt",
            "--shards",
            "8",
            "--checkpoint-seed",
            &hex_32(SEED),
            "--ranks",
            "1",
        ])
        .spawn()
        .expect("spawn weftfs");
    // Wait until the prepare pause is running, then kill the whole process.
    std::thread::sleep(std::time::Duration::from_millis(700));
    let _ = child.kill();
    let _ = child.wait();

    let mut store = Store::open_with_options(&store_path, OpenMode::ReadWrite, opts()).unwrap();
    assert_eq!(store.generation(), 1, "only the genesis commit may exist");
    assert!(store.list_checkpoints().unwrap().is_empty());
    let gc = store.gc(true).unwrap();
    // The killed prepare published create-once chunk blobs (and possibly a
    // manifest) before dying; GC must reclaim every unreferenced one.
    assert!(
        !gc.deleted.is_empty(),
        "the killed worker's orphan blobs must be reclaimed"
    );
    store.verify(true).unwrap();
}

fn hex_32(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn blob_worker_can_publish_blobs_but_not_control_state() {
    use weftfs::backend::BackendKind;
    use weftfs::tensor_store::{TensorLayoutSpec, TensorWriteSpec};

    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    drop(Store::init(&path, opts()).unwrap());

    let bytes = random_bytes(8 * 8 * 2, 0x1234);
    let layout = TensorLayoutSpec::row_major();
    let spec = TensorWriteSpec::default();

    // A plain read-only handle may not publish a blob: read-only means
    // read-only, and the read path never needs this.
    let read_only = Store::open_with_options(&path, OpenMode::ReadOnly, opts()).unwrap();
    let err = read_only
        .prepare_tensor_object_with_spec("bf16", &[8, 8], &bytes, &layout, &spec)
        .unwrap_err();
    assert_eq!(
        err.class(),
        weftfs::error::Class::ReadOnly,
        "unexpected error: {err}"
    );

    // A blob worker may publish the immutable blobs ...
    let mut worker = Store::open_blob_worker(&path, opts(), BackendKind::Local).unwrap();
    let prepared = worker
        .prepare_tensor_object_with_spec("bf16", &[8, 8], &bytes, &layout, &spec)
        .unwrap();
    assert_ne!(prepared.manifest_id, weftfs::identity::Hash32::ZERO);

    // ... but it still cannot publish control state.
    let err = worker
        .create_file(
            weftfs::identity::Hash32::ZERO,
            b"nope",
            b"x",
            0o644,
            0,
            0,
            Default::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.class(),
        weftfs::error::Class::ReadOnly,
        "unexpected error: {err}"
    );
    assert_eq!(worker.generation(), 1);
}
