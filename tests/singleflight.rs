//! R8 reconstruction single-flight: the store coalesces identical concurrent
//! reads, isolates distinct identities, honors bounded degradation, and lets a
//! waiter cancel without cancelling the work other waiters depend on.
//!
//! The store-level tests use the documented test-only leader dwell
//! (`Store::set_single_flight_test_dwell`) so coalescing is a property of the
//! layer, not of scheduler timing.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use tempfile::tempdir;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::recovery::{object_path, read_manifest};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::CancelToken;

const CHUNK: u64 = 64 * 1024;

fn opts(single_flight: bool) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        single_flight,
        single_flight_max_entries: 64,
        single_flight_max_bytes: 256 * 1024 * 1024,
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn seeded(store: &mut Store, name: &[u8], data: &[u8], seed: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, name, data, 0o644, 0, 0, BTreeMap::new(), seed)
        .unwrap()
}

fn auth(store: &Store, id: Hash32) -> weftfs::AuthorizedRead {
    store
        .authorize_read(id, &weftfs::Principal::root(), store.clock_now_ns())
        .unwrap()
}

#[test]
fn concurrent_identical_reads_decode_once() {
    let data = bytes(256 * 1024, 1);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let id = seeded(&mut store, b"shared", &data, b"sf-same");
    let store = Arc::new(store);
    store.set_single_flight_test_dwell(200_000_000);

    let threads = 8usize;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for _ in 0..threads {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let token = auth(&store, id);
            barrier.wait();
            store.read_object_authorized(&token).unwrap()
        }));
    }
    let mut observed = Vec::new();
    for handle in handles {
        observed.push(handle.join().unwrap());
    }
    for got in &observed {
        assert_eq!(got, &data);
    }

    let stats = store.read_layer_stats();
    assert_eq!(
        stats.decode_calls, 1,
        "identical concurrent reads must share one reconstruction"
    );
    assert_eq!(stats.single_flight.leaders, 1);
    assert_eq!(stats.single_flight.waiters as usize, threads - 1);
    assert_eq!(stats.single_flight.degraded, 0);
    assert_eq!(stats.single_flight.in_flight, 0);
}

#[test]
fn distinct_objects_are_not_coalesced() {
    let first = bytes(256 * 1024, 2);
    let second = bytes(256 * 1024, 3);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let a = seeded(&mut store, b"a", &first, b"sf-a");
    let b = seeded(&mut store, b"b", &second, b"sf-b");
    let store = Arc::new(store);
    store.set_single_flight_test_dwell(150_000_000);

    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        for (id, expected) in [(a, first.clone()), (b, second.clone())] {
            let store = &store;
            let barrier = &barrier;
            scope.spawn(move || {
                let token = auth(store, id);
                barrier.wait();
                assert_eq!(store.read_object_authorized(&token).unwrap(), expected);
            });
        }
    });
    let stats = store.read_layer_stats();
    assert_eq!(stats.decode_calls, 2);
    assert_eq!(stats.single_flight.leaders, 2);
    assert_eq!(stats.single_flight.waiters, 0);
}

#[test]
fn a_cancelled_waiter_does_not_cancel_the_leader() {
    let data = bytes(256 * 1024, 4);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let id = seeded(&mut store, b"slow", &data, b"sf-cancel");
    let store = Arc::new(store);
    store.set_single_flight_test_dwell(500_000_000);

    let leader_store = Arc::clone(&store);
    let leader = std::thread::spawn(move || {
        let token = auth(&leader_store, id);
        leader_store.read_object_authorized(&token).unwrap()
    });
    // Give the leader time to register its flight.
    std::thread::sleep(Duration::from_millis(80));

    let waiter_store = Arc::clone(&store);
    let cancel = CancelToken::new();
    let waiter_cancel = cancel.clone();
    let waiter = std::thread::spawn(move || {
        let token = auth(&waiter_store, id);
        waiter_store.read_object_authorized_cancellable(&token, Some(&waiter_cancel))
    });
    std::thread::sleep(Duration::from_millis(40));
    cancel.cancel();

    let err = waiter.join().unwrap().unwrap_err();
    assert_eq!(err.class(), Class::Cancelled);
    assert_eq!(leader.join().unwrap(), data);

    let stats = store.read_layer_stats();
    assert_eq!(stats.decode_calls, 1, "the leader still finished once");
    assert_eq!(stats.single_flight.cancellations, 1);
    assert_eq!(stats.single_flight.in_flight, 0);
}

#[test]
fn a_pre_cancelled_token_declines_to_start() {
    let data = bytes(64 * 1024, 5);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let id = seeded(&mut store, b"pre", &data, b"sf-pre");
    let token = auth(&store, id);
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = store
        .read_object_authorized_cancellable(&token, Some(&cancel))
        .unwrap_err();
    assert_eq!(err.class(), Class::Cancelled);
    assert_eq!(store.decode_calls(), 0);
    // A later read is unaffected.
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
}

#[test]
fn a_decode_error_reaches_every_waiter() {
    let data = bytes(256 * 1024, 6);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let id = seeded(&mut store, b"broken", &data, b"sf-error");
    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let chunk = manifest.chunks[0].hash;
    // Tamper with the chunk blob: every reconstruction must fail, never return
    // partial bytes.
    let path = object_path(store.root(), chunk);
    fs::write(&path, b"tampered").unwrap();

    let store = Arc::new(store);
    store.set_single_flight_test_dwell(200_000_000);
    let threads = 4usize;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for _ in 0..threads {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let token = auth(&store, id);
            barrier.wait();
            store
                .read_object_authorized(&token)
                .expect_err("tampered chunk must fail")
                .class()
        }));
    }
    for handle in handles {
        assert_eq!(handle.join().unwrap(), Class::Corrupt);
    }
    let stats = store.read_layer_stats();
    assert_eq!(
        stats.decode_calls, 1,
        "the leader ran once; waiters got its error"
    );
    assert_eq!(stats.single_flight.waiters as usize, threads - 1);
    assert_eq!(stats.cache.entries, 0, "a failed read is never cached");
}

#[test]
fn entry_bound_degrades_to_independent_reads() {
    let first = bytes(128 * 1024, 7);
    let second = bytes(128 * 1024, 8);
    let dir = tempdir().unwrap();
    let mut options = opts(true);
    options.single_flight_max_entries = 1;
    let mut store = Store::init(dir.path(), options).unwrap();
    let a = seeded(&mut store, b"a", &first, b"sf-bound-a");
    let b = seeded(&mut store, b"b", &second, b"sf-bound-b");
    let store = Arc::new(store);
    store.set_single_flight_test_dwell(200_000_000);

    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        for (id, expected) in [(a, first.clone()), (b, second.clone())] {
            let store = &store;
            let barrier = &barrier;
            scope.spawn(move || {
                let token = auth(store, id);
                barrier.wait();
                assert_eq!(store.read_object_authorized(&token).unwrap(), expected);
            });
        }
    });
    let stats = store.read_layer_stats();
    assert_eq!(stats.decode_calls, 2);
    assert_eq!(stats.single_flight.leaders, 1);
    assert_eq!(stats.single_flight.degraded, 1);
    assert_eq!(stats.single_flight.max_observed_entries, 1);
    assert_eq!(stats.single_flight.in_flight, 0);
}

#[test]
fn byte_bound_degrades_instead_of_reserving_without_limit() {
    let data = bytes(256 * 1024, 9);
    let dir = tempdir().unwrap();
    let mut options = opts(true);
    options.single_flight_max_bytes = 1024;
    let mut store = Store::init(dir.path(), options).unwrap();
    let id = seeded(&mut store, b"big", &data, b"sf-bytes");
    let token = auth(&store, id);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    let stats = store.read_layer_stats();
    assert_eq!(stats.decode_calls, 1);
    assert_eq!(stats.single_flight.leaders, 0);
    assert_eq!(stats.single_flight.degraded, 1);
}

#[test]
fn single_flight_is_off_by_default() {
    let data = bytes(128 * 1024, 10);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(false)).unwrap();
    let id = seeded(&mut store, b"plain", &data, b"sf-off");
    let token = auth(&store, id);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    let stats = store.read_layer_stats();
    assert!(!stats.single_flight.enabled);
    assert_eq!(stats.decode_calls, 2);
    assert_eq!(stats.single_flight.leaders, 0);
    assert_eq!(stats.single_flight.waiters, 0);
    assert_eq!(stats.cache.hits, 0);
}

#[test]
fn a_cancelled_read_leaves_the_store_usable() {
    let data = bytes(128 * 1024, 11);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(true)).unwrap();
    let id = seeded(&mut store, b"after", &data, b"sf-after");
    store.set_single_flight_test_dwell(100_000_000);
    let cancel = CancelToken::new();
    cancel.cancel();
    let token = auth(&store, id);
    assert_eq!(
        store
            .read_object_authorized_cancellable(&token, Some(&cancel))
            .unwrap_err()
            .class(),
        Class::Cancelled
    );
    assert_eq!(store.single_flight_stats().in_flight, 0);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    let reopened = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reopened.read_object(id).unwrap(), data);
    assert!(reopened.verify(true).unwrap().ok);
}
