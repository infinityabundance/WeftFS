//! The prepare / finish split publishes **one** commit.
//!
//! Before this change the DCP adapter committed the tensor state inside
//! `write_data()` and the DCP metadata in `finish()`, so a failure between the
//! two left a checkpoint directory without metadata. Now `PREPARE` publishes
//! every immutable blob and applies the state ops in memory, and `FINISH`
//! adds the metadata blob and publishes exactly one commit.
//!
//! These tests use the store API directly (the service is a thin wrapper over
//! it) and the process-fault proxy (`WEFT_CRASH_PHASE`), which aborts the child
//! after a durability phase. A crash is a process fault, not physical power
//! loss.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const NAME: &str = "atomic";
const METADATA: &str = "/checkpoints/atomic/dcp.metadata";
const CHILD_STORE: &str = "WEFT_ATOMIC_STORE";
const CHILD_CONTAINER: &str = "WEFT_ATOMIC_CONTAINER";
const CHILD_FINISH: &str = "WEFT_ATOMIC_FINISH";

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn container_bytes() -> Vec<u8> {
    let data = (0..(16 * 32 * 4u32))
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 3) as u8)
        .collect::<Vec<u8>>();
    safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "u8",
            shape: &[16, 32, 4],
            data: &data,
        }],
        None,
    )
    .unwrap()
}

fn options(defer: bool) -> CheckpointWriteOptions {
    CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy: ShardStrategy::Zstd,
        defer_commit: defer,
        ..CheckpointWriteOptions::default()
    }
}

fn names(store: &Store) -> Vec<String> {
    store.list_checkpoints().unwrap_or_default()
}

fn metadata_exists(store: &Store) -> bool {
    match store.resolve_path(METADATA) {
        Ok(id) => store.object_meta(id).is_ok(),
        Err(_) => false,
    }
}

/// The child half: prepare a deferred checkpoint and optionally finish it.
#[test]
fn atomic_checkpoint_child() {
    let Ok(store_path) = std::env::var(CHILD_STORE) else {
        return;
    };
    let container_path = std::env::var(CHILD_CONTAINER).expect("container path");
    let mut store = Store::open_with_options(&store_path, OpenMode::ReadWrite, opts()).unwrap();
    store
        .write_checkpoint(NAME, Path::new(&container_path), &options(true))
        .expect("prepare");
    if std::env::var(CHILD_FINISH).ok().as_deref() == Some("1") {
        // `WEFT_CRASH_PHASE` aborts inside this call when set.
        store
            .commit_deferred_checkpoint_metadata(METADATA, b"{\"probe\":1}")
            .expect("finish");
    }
}

fn child(store: &Path, container_path: &Path, finish: bool, phase: Option<&str>) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut command = Command::new(exe);
    command
        .args(["atomic_checkpoint_child", "--exact", "--nocapture"])
        .env(CHILD_STORE, store)
        .env(CHILD_CONTAINER, container_path)
        .env(CHILD_FINISH, if finish { "1" } else { "0" });
    if let Some(phase) = phase {
        command.env("WEFT_CRASH_PHASE", phase);
    }
    let status = command.status().expect("spawn atomic checkpoint child");
    if phase.is_some() {
        assert!(
            !status.success(),
            "child was expected to abort at phase {:?}, but exited {status:?}",
            phase
        );
    } else {
        assert!(status.success(), "child failed: {status:?}");
    }
}

fn seed_store(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, u64) {
    let store = dir.join("store");
    let mut handle = Store::init(&store, opts()).unwrap();
    let root = handle.resolve_path("/").unwrap();
    handle
        .create_file(
            root,
            b"baseline",
            b"baseline bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let generation = handle.generation();
    let container = dir.join("container.safetensors");
    fs::write(&container, container_bytes()).unwrap();
    drop(handle);
    (store, container, generation)
}

/// The old window: tensors committed by `write_data`, metadata missing.
#[test]
fn a_prepare_that_is_never_finished_publishes_nothing() {
    let dir = tempdir().unwrap();
    let (store_path, container, generation) = seed_store(dir.path());
    child(&store_path, &container, false, None);

    let store = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.generation(),
        generation,
        "a deferred checkpoint must not publish a generation"
    );
    assert!(names(&store).is_empty(), "no checkpoint may exist");
    assert!(!metadata_exists(&store));
    assert!(store.verify(true).unwrap().problems.is_empty());
}

/// Prepare + finish: exactly one generation, and both the checkpoint and its
/// metadata are visible at that generation.
#[test]
fn finish_publishes_the_tensor_state_and_the_metadata_together() {
    let dir = tempdir().unwrap();
    let (store_path, container, generation) = seed_store(dir.path());
    child(&store_path, &container, true, None);

    let store = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), generation + 1, "one commit, not two");
    assert_eq!(names(&store), vec![NAME.to_owned()]);
    assert!(metadata_exists(&store), "metadata is in the same commit");
    let manifest = store.checkpoint_manifest(NAME).unwrap();
    assert!(manifest.complete);
    assert_eq!(manifest.tensors.len(), 1);
    let problems = store.verify(true).unwrap();
    assert!(problems.problems.is_empty(), "verify --full: {problems:?}");
}

/// A crash during the single publication is old-or-new, never "tensors without
/// metadata".
#[test]
fn a_crash_during_the_final_publication_is_old_or_new_whole() {
    for phase in ["after_objects", "after_commit_file"] {
        let dir = tempdir().unwrap();
        let (store_path, container, generation) = seed_store(dir.path());
        child(&store_path, &container, true, Some(phase));

        let store = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
        let names = names(&store);
        let metadata = metadata_exists(&store);
        let generation_now = store.generation();
        let old = generation_now == generation && names.is_empty() && !metadata;
        let new = generation_now == generation + 1 && names == vec![NAME.to_owned()] && metadata;
        assert!(
            old || new,
            "phase {phase}: generation {generation_now} (was {generation}), \
             checkpoints {names:?}, metadata {metadata} is neither old nor new"
        );
        // A crash between the commit-file fsync and the superblock slot leaves
        // an fsynced but unpublished commit file. Recovery ignores it and
        // `verify` names it as an unpublished tail; nothing else may be wrong.
        let report = store.verify(false).unwrap();
        let unexpected: Vec<_> = report
            .problems
            .iter()
            .filter(|problem| !problem.detail.starts_with("commit file for generation"))
            .collect();
        assert!(
            unexpected.is_empty(),
            "phase {phase} reported non-tail problems: {unexpected:?}"
        );
    }
}

/// The historical two-commit shape is still available and takes two
/// generations; the atomic shape takes one for the same bytes.
#[test]
fn atomic_publication_costs_one_generation_and_the_old_shape_two() {
    let dir = tempdir().unwrap();
    let (store_path, container, generation) = seed_store(dir.path());

    let mut store = Store::open_with_options(&store_path, OpenMode::ReadWrite, opts()).unwrap();
    store
        .write_checkpoint(NAME, &container, &options(false))
        .unwrap();
    assert_eq!(store.generation(), generation + 1, "tensors commit");
    let root = store.resolve_path("/checkpoints/atomic").unwrap();
    store
        .create_file(
            root,
            b"dcp.metadata",
            b"{\"probe\":1}",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        store.generation(),
        generation + 2,
        "the metadata commit is a second generation"
    );
}

/// A finished checkpoint is byte-identical to a two-commit one: deferring the
/// commit changes when state becomes visible, never what is stored.
#[test]
fn deferring_the_commit_changes_no_bytes() {
    let dir = tempdir().unwrap();
    let container = dir.path().join("container.safetensors");
    fs::write(&container, container_bytes()).unwrap();

    let atomic_store = dir.path().join("atomic-store");
    let mut store = Store::init(&atomic_store, opts()).unwrap();
    let report = store
        .write_checkpoint(NAME, &container, &options(true))
        .unwrap();
    assert_eq!(report.head, weftfs::identity::Hash32::ZERO);
    store
        .commit_deferred_checkpoint_metadata(METADATA, b"{\"probe\":1}")
        .unwrap();
    let atomic_head = store.head();
    let atomic_root = store.resolve_path(METADATA).unwrap();
    let atomic_metadata = store.read_object(atomic_root).unwrap();
    drop(store);

    let two_store = dir.path().join("two-store");
    let mut store = Store::init(&two_store, opts()).unwrap();
    store
        .write_checkpoint(NAME, &container, &options(false))
        .unwrap();
    let parent = store.resolve_path("/checkpoints/atomic").unwrap();
    store
        .create_file(
            parent,
            b"dcp.metadata",
            b"{\"probe\":1}",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let two_head = store.head();
    let two_root = store.resolve_path(METADATA).unwrap();
    let two_metadata = store.read_object(two_root).unwrap();
    drop(store);

    assert_eq!(
        atomic_metadata, two_metadata,
        "the metadata blob is content-addressed and identical"
    );
    assert_ne!(
        atomic_head, two_head,
        "the commit histories differ (one commit versus two), as designed"
    );
    assert!(Store::open(&atomic_store, OpenMode::ReadOnly)
        .unwrap()
        .verify(true)
        .unwrap()
        .problems
        .is_empty());
    assert!(Store::open(&two_store, OpenMode::ReadOnly)
        .unwrap()
        .verify(true)
        .unwrap()
        .problems
        .is_empty());
}
