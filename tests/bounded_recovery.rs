//! Bounded exhaustive recovery enumeration over the real on-disk store.
//!
//! This is NOT a formal proof and NOT model checking. It enumerates a bounded
//! space: every published generation is corrupted in three ways, and each
//! superblock slot is corrupted, asserting that recovery yields the documented
//! previous whole state and never a partial one. Combined with the process
//! crash-proxy tests, it makes the recovery contract falsifiable.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, Store};

const GENERATIONS: u64 = 5;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
    }
}

/// Build a store with: gen 1 = init, gen 2 = create file `v0`, gen 3 = `v1`,
/// gen 4 = `v2`, gen 5 = `v3`.
fn build(dir: &Path) -> Hash32 {
    let mut store = Store::init(dir, opts()).expect("init");
    let root = store.resolve_path("/").expect("root");
    let id = store
        .create_file_seeded(
            root,
            b"f",
            b"v0",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"bounded-recovery",
        )
        .expect("create");
    for generation in 3..=GENERATIONS {
        let content = format!("v{}", generation - 2);
        store
            .write_file(id, 0, content.as_bytes())
            .expect("write generation");
    }
    assert_eq!(store.generation(), GENERATIONS);
    id
}

fn expected_content(victim_generation: u64) -> Vec<u8> {
    // Corrupting generation g (>=3) makes the highest recoverable generation
    // g-1, whose file content is `v{g-3}`.
    format!("v{}", victim_generation - 3).into_bytes()
}

fn commit_path(dir: &Path, generation: u64) -> std::path::PathBuf {
    dir.join("commits").join(format!("{generation}.commit"))
}

fn corrupt(path: &Path, kind: &str) {
    let mut bytes = fs::read(path).expect("read commit");
    match kind {
        "empty" => bytes.clear(),
        "half" => bytes.truncate(bytes.len() / 2),
        "flip" => {
            let index = bytes.len() / 2;
            bytes[index] ^= 0xFF;
        }
        other => panic!("unknown corruption {other}"),
    }
    fs::write(path, bytes).expect("write corrupt commit");
}

#[test]
fn every_generation_corruption_recovers_the_previous_whole_state() {
    let mut checks = 0u32;
    for victim in 3..=GENERATIONS {
        for kind in ["empty", "half", "flip"] {
            let temp = tempdir().expect("tempdir");
            let id = build(temp.path());
            corrupt(&commit_path(temp.path(), victim), kind);

            let store = Store::open(temp.path(), OpenMode::ReadOnly)
                .unwrap_or_else(|err| panic!("victim {victim}/{kind} failed to open: {err}"));
            let recovered = store.read_object(id).expect("read recovered object");
            assert_eq!(
                recovered,
                expected_content(victim),
                "victim generation {victim} corruption {kind}"
            );
            // A second open must produce exactly the same root and bytes
            // (replay idempotence on a corrupted tail).
            let root = store.snapshot_root();
            drop(store);
            let again = Store::open(temp.path(), OpenMode::ReadOnly).expect("second open");
            assert_eq!(again.snapshot_root(), root);
            assert_eq!(again.read_object(id).unwrap(), recovered);
            checks += 1;
        }
    }
    assert_eq!(checks, 9, "bounded enumeration covered every case");
}

#[test]
fn superblock_slot_corruption_falls_back_to_the_other_slot() {
    let temp = tempdir().expect("tempdir");
    let id = build(temp.path());
    let path = temp.path().join("superblock");
    let mut bytes = fs::read(&path).expect("read superblock");
    assert_eq!(bytes.len(), 8192);
    // Generation 5 lives in slot 1 (5 % 2 == 1); corrupt its CRC at +64.
    bytes[4096 + 64] ^= 0xFF;
    fs::write(&path, &bytes).expect("write superblock");

    let store = Store::open(temp.path(), OpenMode::ReadOnly).expect("fallback open");
    // Slot 0 holds generation 4, whose content is v2.
    assert_eq!(store.read_object(id).unwrap(), b"v2");

    // Corrupting both slots leaves no valid published generation: fail closed.
    bytes[64] ^= 0xFF;
    fs::write(&path, &bytes).expect("write superblock");
    assert!(
        Store::open(temp.path(), OpenMode::ReadOnly).is_err(),
        "two corrupt slots must not invent a state"
    );
}

#[test]
fn replay_is_idempotent_without_corruption() {
    let temp = tempdir().expect("tempdir");
    let id = build(temp.path());
    let first = Store::open(temp.path(), OpenMode::ReadOnly).expect("open 1");
    let root = first.snapshot_root();
    let bytes = first.read_object(id).unwrap();
    drop(first);
    let second = Store::open(temp.path(), OpenMode::ReadOnly).expect("open 2");
    assert_eq!(second.snapshot_root(), root);
    assert_eq!(second.read_object(id).unwrap(), bytes);
    let report = second.verify(true).expect("verify");
    assert!(report.ok, "verify must be clean: {report:?}");
}
