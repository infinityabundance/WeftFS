//! Process-fault crash proxy.
//!
//! `WEFT_CRASH_PHASE` makes the commit path call `std::process::abort()` after
//! a durability phase. A child process performs a write and dies; the parent
//! reopens and asserts the documented recovery set: either the old state or the
//! fully committed new state, never a partial state. This is a process-fault
//! proxy, not physical power loss.

use std::collections::BTreeMap;
use std::process::Command;

use tempfile::tempdir;

use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
    }
}

fn prepare(path: &std::path::Path) -> (u64, weftfs::identity::Hash32) {
    let mut store = Store::init(path, opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"f", b"old\n", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let generation = store.generation();
    (generation, id)
}

fn child_write(store_path: &std::path::Path, phase: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .arg("--store")
        .arg(store_path)
        .args([
            "write",
            "/f",
            "--offset",
            "0",
            "--text",
            "new content, longer than old\n",
        ])
        .env("WEFT_CRASH_PHASE", phase)
        .status()
        .expect("spawn weftfs binary");
    assert!(
        !status.success(),
        "child was expected to abort at phase {phase}, but exited {status:?}"
    );
}

#[test]
fn crash_before_commit_file_keeps_old_state() {
    for phase in ["after_objects", "after_commit_file"] {
        let dir = tempdir().unwrap();
        let (generation, id) = prepare(dir.path());
        child_write(dir.path(), phase);

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        assert_eq!(
            store.generation(),
            generation,
            "phase {phase} must not publish a new generation"
        );
        assert_eq!(
            store.read_object(id).unwrap(),
            b"old\n",
            "phase {phase} must leave the old bytes intact"
        );
        assert!(
            !store
                .recovery_notes()
                .iter()
                .any(|note| note.contains(&format!("generation {}", generation + 1))),
            "the uncommitted generation must simply be invisible"
        );
    }
}

#[test]
fn crash_after_superblock_publish_yields_new_state() {
    let dir = tempdir().unwrap();
    let (generation, id) = prepare(dir.path());
    child_write(dir.path(), "after_superblock_write");

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.generation(),
        generation + 1,
        "a published superblock slot exposes the new commit"
    );
    assert_eq!(
        store.read_object(id).unwrap(),
        b"new content, longer than old\n",
        "the committed write is fully materializable"
    );
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn crash_never_leaves_a_partial_file() {
    // Across every phase, opening and reading yields exactly one of the two
    // legal states. This is the direct statement of the recovery set.
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let dir = tempdir().unwrap();
        let (_generation, id) = prepare(dir.path());
        child_write(dir.path(), phase);
        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes == b"old\n" || bytes == b"new content, longer than old\n",
            "phase {phase} produced unexpected bytes: {bytes:?}"
        );
    }
}
