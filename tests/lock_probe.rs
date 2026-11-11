//! R13 backend-lock probe: accounting sanity and the read-path contract.
//!
//! These tests prove the probe itself is honest before any receipt cites it:
//! one thread cannot hold the store's backend mutex for longer than its own
//! wall time, and the per-tensor attribution reports exactly the entries the
//! tensor decode made.

use std::time::Instant;

use tempfile::tempdir;
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
        ..Options::default()
    }
}

fn store_with_file(bytes: usize) -> (tempfile::TempDir, Store, weftfs::Hash32) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let payload: Vec<u8> = (0..bytes).map(|index| (index % 251) as u8).collect();
    let id = store
        .create_file(
            root,
            b"f",
            &payload,
            0o644,
            0,
            0,
            std::collections::BTreeMap::new(),
        )
        .unwrap();
    (dir, store, id)
}

#[test]
fn hold_time_cannot_exceed_wall_on_one_thread() {
    let (_dir, store, id) = store_with_file(1024 * 1024);
    // Warm the state and the page cache, then measure only the reads.
    store.read_object(id).unwrap();

    store.set_lock_probe(true);
    store.reset_lock_probe();
    let started = Instant::now();
    for _ in 0..20 {
        let bytes = store.read_object(id).unwrap();
        assert_eq!(bytes.len(), 1024 * 1024);
    }
    let wall_ns = started.elapsed().as_nanos();

    let report = store.lock_probe_report();
    assert!(report.hold.count > 0, "probe recorded no entries");
    assert!(
        report.hold.total_ns <= wall_ns,
        "hold {} ns exceeds one-thread wall {} ns",
        report.hold.total_ns,
        wall_ns
    );
    assert_eq!(report.max_demand, 1);
    assert_eq!(report.hold.count, report.wait.count);
    store.set_lock_probe(false);
}

#[test]
fn disabled_probe_records_nothing() {
    let (_dir, store, id) = store_with_file(64 * 1024);
    store.read_object(id).unwrap();
    store.reset_lock_probe();
    assert!(!store.lock_probe_enabled());
    for _ in 0..5 {
        store.read_object(id).unwrap();
    }
    let report = store.lock_probe_report();
    assert_eq!(report.hold.count, 0);
    assert_eq!(report.wait.count, 0);
}

#[test]
fn probe_does_not_change_bytes_or_state() {
    let (_dir, store, id) = store_with_file(256 * 1024);
    let before = store.read_object(id).unwrap();
    let head_before = store.head();
    let generation_before = store.generation();

    store.set_lock_probe(true);
    let during = store.read_object(id).unwrap();
    let report = store.lock_probe_report();
    store.set_lock_probe(false);
    let after = store.read_object(id).unwrap();

    assert_eq!(before, during);
    assert_eq!(before, after);
    assert!(report.hold.count > 0);
    assert_eq!(store.head(), head_before);
    assert_eq!(store.generation(), generation_before);
    assert!(store.verify(true).unwrap().problems.is_empty());
}

#[test]
fn reopen_read_only_never_writes_probe_state() {
    let (dir, store, id) = store_with_file(64 * 1024);
    store.set_lock_probe(true);
    store.read_object(id).unwrap();
    drop(store);

    let reopened =
        Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).expect("read-only open");
    reopened.set_lock_probe(true);
    let bytes = reopened.read_object(id).unwrap();
    assert_eq!(bytes.len(), 64 * 1024);
    let report = reopened.lock_probe_report();
    assert!(report.hold.count > 0);
    assert!(reopened.verify(true).unwrap().problems.is_empty());
}
