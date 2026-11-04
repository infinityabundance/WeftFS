//! Group commit: many mutations into exactly one durable commit.
//!
//! Covers the API contract in `Store::group`: one generation per successful
//! group, read-your-writes inside the closure, nothing published when the
//! closure fails, nesting refusal, read-only refusal, and a process-fault crash
//! proxy at each publication phase. The crash proxy re-executes this test binary
//! (`group_crash_helper`) so the abort happens in a child process, exactly like
//! `tests/crash_proxy.rs`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::State;
use weftfs::store::{OpenMode, Options, Store, MAX_FILE_BYTES};

const OBJECTS: usize = 100;
const CRASH_GROUP_FILES: usize = 5;
const CRASH_STORE_ENV: &str = "WEFT_GROUP_CRASH_STORE";

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        // Fixed clock: version ids and mtimes are stable across the two stores.
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
    }
}

fn seed(index: usize) -> Vec<u8> {
    format!("group-commit-object-{index:03}").into_bytes()
}

fn payload(index: usize) -> Vec<u8> {
    format!("payload {index:03}\n").into_bytes()
}

fn name(index: usize) -> Vec<u8> {
    format!("file-{index:03}").into_bytes()
}

/// Commit files currently on disk, sorted by generation.
fn commit_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(root.join("commits"))
        .expect("commits directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    out.sort();
    out
}

/// Compare the state maps without the generation counter, which is the one
/// field allowed to differ between "one group" and "N single commits".
fn assert_same_records(left: &State, right: &State, what: &str) {
    assert_eq!(left.objects, right.objects, "{what}: objects differ");
    assert_eq!(left.entries, right.entries, "{what}: entries differ");
    assert_eq!(left.relations, right.relations, "{what}: relations differ");
    assert_eq!(left.snapshots, right.snapshots, "{what}: snapshots differ");
    assert_eq!(left.tips, right.tips, "{what}: tips differ");
}

fn create_seeded(store: &mut Store, parent: Hash32, index: usize) -> Hash32 {
    store
        .create_file_seeded(
            parent,
            &name(index),
            &payload(index),
            0o644,
            0,
            0,
            BTreeMap::new(),
            &seed(index),
        )
        .expect("create_file_seeded")
}

#[test]
fn one_group_of_100_creates_matches_100_single_creates() {
    let grouped_dir = tempdir().unwrap();
    let single_dir = tempdir().unwrap();

    let mut grouped = Store::init(grouped_dir.path(), opts()).unwrap();
    let grouped_root = grouped.resolve_path("/").unwrap();
    let generation_before = grouped.generation();
    let commits_before = commit_files(grouped_dir.path());
    assert_eq!(commits_before.len(), 1, "genesis commit only");

    let (ids, commit) = grouped
        .group(|store| {
            let mut ids = Vec::with_capacity(OBJECTS);
            for index in 0..OBJECTS {
                ids.push(create_seeded(store, grouped_root, index));
            }
            Ok(ids)
        })
        .expect("group of creates");

    assert_eq!(
        grouped.generation(),
        generation_before + 1,
        "a group publishes exactly one generation"
    );
    assert_eq!(grouped.snapshot_root(), commit, "group returns its commit");
    let commits_after = commit_files(grouped_dir.path());
    assert_eq!(
        commits_after.len(),
        2,
        "exactly one new commit file: {:?}",
        commits_after
    );

    // 100 objects, 100 entries, 100 tips, all readable.
    let state = grouped.current_state();
    assert_eq!(state.objects.len(), 1 + OBJECTS);
    assert_eq!(state.entries.len(), OBJECTS);
    assert_eq!(state.tips.len(), 1 + OBJECTS, "root plus each file tip");
    let mut listed: Vec<Vec<u8>> = grouped
        .list_dir(grouped_root)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    listed.sort();
    for index in 0..OBJECTS {
        assert_eq!(grouped.read_object(ids[index]).unwrap(), payload(index));
        assert_eq!(listed[index], name(index));
    }

    let report = grouped.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    assert_eq!(report.generation, generation_before + 1);

    // Same content created one commit at a time.
    let mut single = Store::init(single_dir.path(), opts()).unwrap();
    let single_root = single.resolve_path("/").unwrap();
    let mut single_ids = Vec::with_capacity(OBJECTS);
    for index in 0..OBJECTS {
        single_ids.push(create_seeded(&mut single, single_root, index));
    }
    assert_eq!(
        single.generation(),
        generation_before + OBJECTS as u64,
        "the comparison baseline pays one generation per create"
    );
    assert_eq!(single_ids, ids, "fixed seeds give identical object ids");
    assert_same_records(
        single.current_state(),
        grouped.current_state(),
        "group vs single",
    );
    let single_report = single.verify(true).unwrap();
    assert!(single_report.ok, "problems: {:?}", single_report.problems);
}

#[test]
fn reads_inside_the_group_see_staged_writes() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let generation_before = store.generation();

    let (id, commit) = store
        .group(|store| {
            let id = store.create_file_seeded(
                root,
                b"rw",
                b"",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"group-read-your-writes",
            )?;
            // The staged object and entry are visible immediately.
            assert_eq!(store.lookup(root, b"rw").unwrap(), id);
            assert_eq!(store.object_meta(id).unwrap().size, 0);

            store.write_file(id, 0, b"hello group")?;
            assert_eq!(store.read_range(id, 0, 11).unwrap(), b"hello group");
            assert_eq!(store.read_object(id).unwrap(), b"hello group");
            assert_eq!(store.object_meta(id).unwrap().size, 11);

            // An append after the first write sees the grown file.
            store.write_file(id, 11, b"!")?;
            assert_eq!(store.read_object(id).unwrap(), b"hello group!");
            Ok(id)
        })
        .expect("group with read-your-writes");

    assert_eq!(store.generation(), generation_before + 1);
    assert_eq!(store.snapshot_root(), commit);
    assert_eq!(store.read_object(id).unwrap(), b"hello group!");
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn failed_group_publishes_nothing_and_restores_state() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let baseline = create_seeded(&mut store, root, 0);
    store
        .create_file_seeded(
            root,
            b"existing",
            b"existing bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"group-existing",
        )
        .unwrap();

    let generation_before = store.generation();
    let state_before = store.current_state().clone();
    let commits_before = commit_files(dir.path());

    // Duplicate name inside the group: the first create is rolled back too.
    let err = store
        .group(|store| {
            store.create_file_seeded(
                root,
                b"staged-a",
                b"staged a",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"group-staged-a",
            )?;
            store.create_file_seeded(
                root,
                b"existing",
                b"duplicate",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"group-staged-dup",
            )?;
            Ok(())
        })
        .expect_err("duplicate name must fail the group");
    assert_eq!(err.class(), Class::Exists, "unexpected error: {err}");

    assert_eq!(store.generation(), generation_before, "no new generation");
    assert_same_records(store.current_state(), &state_before, "after duplicate");
    assert_eq!(
        commit_files(dir.path()).len(),
        commits_before.len(),
        "no new commit file"
    );
    assert!(
        !dir.path()
            .join("commits")
            .join(format!("{:020}.commit", generation_before + 1))
            .exists(),
        "the unpublished generation must not exist"
    );
    assert!(store.lookup(root, b"staged-a").is_err(), "no stale entry");
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);

    // Writing past MAX_FILE_BYTES is also a clean failure.
    let err = store
        .group(|store| {
            store.write_file(baseline, MAX_FILE_BYTES, b"x")?;
            Ok(())
        })
        .expect_err("write past the v1 bound must fail the group");
    assert_eq!(err.class(), Class::LimitExceeded, "unexpected error: {err}");
    assert_eq!(store.generation(), generation_before);
    assert_same_records(store.current_state(), &state_before, "after limit");
    assert_eq!(commit_files(dir.path()).len(), commits_before.len());

    // A subsequent mutation still works and publishes one generation.
    let (id, commit) = store
        .group(|store| {
            let id = store.create_file_seeded(
                root,
                b"after-failure",
                b"ok",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"group-after-failure",
            )?;
            Ok(id)
        })
        .expect("store remains usable");
    assert_eq!(store.generation(), generation_before + 1);
    assert_eq!(store.snapshot_root(), commit);
    assert_eq!(store.read_object(id).unwrap(), b"ok");
}

#[test]
fn nested_group_is_invalid_argument_and_store_stays_usable() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let generation_before = store.generation();

    let ((inner_class, id), commit) = store
        .group(|store| {
            let inner = store.group(|_| Ok(()));
            let class = inner.expect_err("nested group must fail").class();
            let id = store.create_file_seeded(
                root,
                b"outer",
                b"outer bytes",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"group-outer",
            )?;
            Ok((class, id))
        })
        .expect("the outer group still publishes");

    assert_eq!(inner_class, Class::InvalidArgument);
    assert_eq!(store.generation(), generation_before + 1);
    assert_eq!(store.snapshot_root(), commit);
    assert_eq!(store.read_object(id).unwrap(), b"outer bytes");
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn read_only_store_refuses_group() {
    let dir = tempdir().unwrap();
    {
        let mut store = Store::init(dir.path(), opts()).unwrap();
        let root = store.resolve_path("/").unwrap();
        create_seeded(&mut store, root, 0);
    }

    let mut store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    let generation_before = store.generation();
    let commits_before = commit_files(dir.path());
    let ran = Cell::new(false);
    let err = store
        .group(|_| {
            ran.set(true);
            Ok(())
        })
        .expect_err("read-only store must refuse group");
    assert_eq!(err.class(), Class::ReadOnly, "unexpected error: {err}");
    assert!(!ran.get(), "the closure must not run");
    assert_eq!(store.generation(), generation_before);
    assert_eq!(commit_files(dir.path()).len(), commits_before.len());
}

/// Child-side helper for the crash proxy. With `WEFT_GROUP_CRASH_STORE` set it
/// opens that store and publishes one group, aborting at `WEFT_CRASH_PHASE`;
/// without it, it runs the same group + publish path in-process and asserts the
/// single-generation rule, so a normal test run still checks something.
#[test]
fn group_crash_helper() {
    let root_override = std::env::var_os(CRASH_STORE_ENV).map(PathBuf::from);
    let temp;
    let mut store = match &root_override {
        Some(path) => Store::open(path, OpenMode::ReadWrite).expect("open store"),
        None => {
            temp = tempdir().unwrap();
            Store::init(temp.path(), opts()).expect("init store")
        }
    };
    let root = store.resolve_path("/").unwrap();
    let generation_before = store.generation();

    let commit = store
        .group(|store| {
            for index in 0..CRASH_GROUP_FILES {
                store.create_file_seeded(
                    root,
                    &name(index),
                    &payload(index),
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    &seed(index),
                )?;
            }
            Ok(())
        })
        .expect("group create")
        .1;

    assert_eq!(
        store.generation(),
        generation_before + 1,
        "the group helper publishes exactly one generation"
    );
    assert_eq!(store.snapshot_root(), commit);
}

fn child_group(store_path: &Path, phase: &str) {
    let exe = std::env::current_exe().expect("test binary path");
    let status = Command::new(exe)
        .args(["group_crash_helper", "--exact", "--nocapture"])
        .env(CRASH_STORE_ENV, store_path)
        .env("WEFT_CRASH_PHASE", phase)
        .status()
        .expect("spawn group crash helper");
    assert!(
        !status.success(),
        "child was expected to abort at phase {phase}, but exited {status:?}"
    );
}

fn prepare_crash_store(path: &Path) -> (u64, Hash32, State) {
    let mut store = Store::init(path, opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"baseline",
            b"baseline bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"group-crash-baseline",
        )
        .unwrap();
    let state = store.current_state().clone();
    (store.generation(), id, state)
}

#[test]
fn crash_before_group_publish_keeps_the_old_generation() {
    for phase in ["after_objects", "after_commit_file"] {
        let dir = tempdir().unwrap();
        let (generation, id, state_before) = prepare_crash_store(dir.path());
        child_group(dir.path(), phase);

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        assert_eq!(
            store.generation(),
            generation,
            "phase {phase} must not publish the group"
        );
        assert_eq!(
            store.read_object(id).unwrap(),
            b"baseline bytes",
            "phase {phase} must leave the old object intact"
        );
        assert_eq!(
            store.current_state().objects,
            state_before.objects,
            "phase {phase} must restore every object"
        );
        assert_eq!(
            store.current_state().entries,
            state_before.entries,
            "phase {phase} must restore every entry"
        );
        assert!(
            store
                .list_dir(store.resolve_path("/").unwrap())
                .unwrap()
                .len()
                == 1,
            "phase {phase} must not add entries"
        );
        let report = store.verify(true).unwrap();
        // A fault between the commit-file fsync and the superblock slot leaves
        // an fsynced but unpublished commit file. Recovery ignores it, exactly
        // as it does for the single-mutation path, and verify names it without
        // reporting any state damage. Nothing else may be wrong.
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

#[test]
fn crash_after_group_publish_yields_the_whole_new_generation() {
    let dir = tempdir().unwrap();
    let (generation, _id, _state_before) = prepare_crash_store(dir.path());
    // Genesis plus the baseline create: the group must add exactly one.
    let commits_before = commit_files(dir.path()).len();
    assert_eq!(commits_before, 2);
    child_group(dir.path(), "after_superblock_write");

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.generation(),
        generation + 1,
        "a published superblock slot exposes the group commit"
    );
    let root = store.resolve_path("/").unwrap();
    for index in 0..CRASH_GROUP_FILES {
        let id = store.lookup(root, &name(index)).expect("grouped entry");
        assert_eq!(
            store.read_object(id).unwrap(),
            payload(index),
            "every op of the group is present"
        );
    }
    // Never partial: the group's own commit is the only new one and it is
    // fully replayable.
    assert_eq!(
        commit_files(dir.path()).len(),
        commits_before + 1,
        "the whole group is one commit file"
    );
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    assert_eq!(report.generation, generation + 1);
}
