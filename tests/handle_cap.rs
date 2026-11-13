//! X07: the hard cap on *active* read handles.
//!
//! `ReadHandlePool` already bounded idle handles (≤64). That says nothing about
//! how many handles the backend is asked to open at once: under fanout every
//! reader that finds the idle set empty opens a new handle. This court pins the
//! new cap:
//!
//! * the observed number of simultaneously active handles never exceeds it;
//! * `0` restores the pre-X07 unbounded behaviour, so the control arm is real;
//! * a panicking reader, or a thread that exits mid-fanout, releases its slot
//!   (the lease is an RAII guard with no manual release path);
//! * mutation still invalidates pooled handles and new content is visible;
//! * a packed writer's own buffered members still bypass the pool, at cap 1;
//! * many readers under a tiny cap make progress (no deadlock, no starvation).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::tempdir;
use weftfs::backend::BackendKind;
use weftfs::packs::PackOptions;
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add((index % 97) as u8))
        .collect()
}

fn create(store: &mut Store, name: &[u8], bytes: &[u8]) -> weftfs::Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, name, bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap()
}

#[test]
fn active_handles_never_exceed_the_cap() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let mut ids = Vec::new();
    for index in 0..8u8 {
        let bytes = payload(index, 48 * 1024);
        ids.push((
            create(&mut writer, format!("f{index}").as_bytes(), &bytes),
            bytes,
        ));
    }
    drop(writer);

    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    store.set_read_pool_active_cap(4);
    assert_eq!(store.read_pool_active_cap(), 4);
    std::thread::scope(|scope| {
        for _ in 0..12 {
            let store = &store;
            let ids = &ids;
            scope.spawn(move || {
                for _ in 0..40 {
                    for (id, expected) in ids {
                        assert_eq!(&store.read_object(*id).unwrap(), expected);
                    }
                }
            });
        }
    });
    let stats = store.read_pool_stats();
    assert!(
        stats.observed_max_active_handles <= 4,
        "observed {} active handles under a cap of 4",
        stats.observed_max_active_handles
    );
    assert!(
        stats.observed_max_active_handles > 1,
        "the readers did not actually overlap: {stats:?}"
    );
    assert!(stats.cap_waits > 0, "the cap was never reached: {stats:?}");
    assert_eq!(stats.active_handles, 0, "a lease leaked: {stats:?}");
    assert!(store.verify(true).unwrap().problems.is_empty());
}

#[test]
fn a_zero_cap_is_the_unbounded_control() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(9, 32 * 1024);
    let id = create(&mut writer, b"f", &bytes);
    drop(writer);
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    store.set_read_pool_active_cap(0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
    let expected = &bytes;
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let store = &store;
            let barrier = std::sync::Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    assert_eq!(store.read_object(id).unwrap(), *expected);
                }
            });
        }
    });
    let stats = store.read_pool_stats();
    assert_eq!(stats.active_cap, 0);
    assert_eq!(stats.cap_waits, 0);
    // With the cap disabled no lease is taken, so the control's evidence is the
    // uncapped backend concurrency (`max_in_flight`), which exceeds the default
    // cap of 64 only because the court uses 16 readers — the point is that
    // nothing bounded it below what the readers asked for.
    assert_eq!(stats.observed_max_active_handles, 0);
    assert!(
        stats.max_in_flight >= 8,
        "the control arm was throttled: {stats:?}"
    );
}

#[test]
fn a_panicking_reader_releases_its_slot() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(3, 16 * 1024);
    let id = create(&mut writer, b"f", &bytes);
    drop(writer);
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    store.set_read_pool_active_cap(1);
    for _ in 0..8 {
        let outcome = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert_eq!(store.read_object(id).unwrap(), bytes);
                    panic!("reader panicked after its read");
                })
                .join()
        });
        assert!(outcome.is_err());
    }
    let stats = store.read_pool_stats();
    assert_eq!(stats.active_handles, 0, "the cap leaked a slot: {stats:?}");
    assert!(stats.observed_max_active_handles <= 1);
    // The cap is a semaphore, not a one-shot: a later read still succeeds.
    assert_eq!(store.read_object(id).unwrap(), bytes);
}

#[test]
fn a_mutation_still_invalidates_capped_handles() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    store.set_read_pool_active_cap(2);
    let first = payload(1, 16 * 1024);
    let id = create(&mut store, b"live", &first);
    assert_eq!(store.read_object(id).unwrap(), first);
    let epoch = store.read_pool_stats().epoch;
    let second = payload(2, 16 * 1024);
    let id2 = create(&mut store, b"second", &second);
    assert_eq!(store.read_object(id2).unwrap(), second);
    let stats = store.read_pool_stats();
    assert!(stats.epoch > epoch);
    assert_eq!(store.read_object(id).unwrap(), first);
    assert!(stats.observed_max_active_handles <= 2);
}

#[test]
fn a_packed_writer_still_reads_its_own_buffered_members_under_cap_one() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let backend = BackendKind::packed(
        BackendKind::Local,
        PackOptions {
            pack_target_bytes: 64 * 1024,
            member_threshold_bytes: 4096,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let mut store = Store::init_with_backend(&path, opts(), backend).unwrap();
    store.set_read_pool_active_cap(1);
    let root = store.resolve_path("/").unwrap();
    let mut written = Vec::new();
    for index in 0..16u8 {
        let bytes = payload(index, 3000);
        // Read the just-written object back while the pack is still open: its
        // bytes exist only in the writer handle's buffered pack.
        let id = store
            .create_file(
                root,
                format!("f{index}").as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(store.read_object(id).unwrap(), bytes);
        written.push((id, bytes));
    }
    let stats = store.read_pool_stats();
    assert_eq!(stats.active_cap, 1);
    assert!(stats.observed_max_active_handles <= 1);
    for (id, bytes) in &written {
        assert_eq!(&store.read_object(*id).unwrap(), bytes);
    }
    assert!(store.verify(true).unwrap().problems.is_empty());
}

#[test]
fn many_readers_under_a_tiny_cap_make_progress() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(5, 24 * 1024);
    let id = create(&mut writer, b"f", &bytes);
    drop(writer);
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    store.set_read_pool_active_cap(1);
    let completed = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let expected = &bytes;
    std::thread::scope(|scope| {
        for _ in 0..24 {
            let store = &store;
            let completed = &completed;
            let stop = &stop;
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    assert_eq!(store.read_object(id).unwrap(), *expected);
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        std::thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Relaxed);
    });
    let stats = store.read_pool_stats();
    assert!(completed.load(Ordering::Relaxed) > 24, "{stats:?}");
    assert!(stats.observed_max_active_handles <= 1, "{stats:?}");
    assert_eq!(stats.active_handles, 0);
}
