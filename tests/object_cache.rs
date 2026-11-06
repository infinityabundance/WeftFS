//! R8 dependency-aware decoded-object cache.
//!
//! Every hit must be exact (the same verified decoded bytes a fresh
//! reconstruction would produce), version-separated, deterministically
//! evicting by `expected reconstruction cost avoided per byte`, pin-aware, and
//! never reachable without authorization.

use std::collections::BTreeMap;

use tempfile::tempdir;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};
use weftfs::Principal;

const CHUNK: u64 = 64 * 1024;

fn opts(cache_bytes: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        cache_bytes,
        cache_entries: 65_536,
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(seed);
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
        .authorize_read(id, &Principal::root(), store.clock_now_ns())
        .unwrap()
}

fn derived_from(store: &mut Store, dependent: Hash32, base: Hash32) {
    store
        .relation_add(
            dependent,
            "derived_from",
            RelationTarget::Object(base),
            None,
            BTreeMap::new(),
        )
        .unwrap();
}

#[test]
fn cache_is_disabled_by_default() {
    let data = bytes(128 * 1024, 1);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), Options::default()).unwrap();
    let id = seeded(&mut store, b"plain", &data, b"cache-default");
    let token = auth(&store, id);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    let stats = store.cache_stats();
    assert!(!stats.enabled);
    assert_eq!(stats.max_bytes, 0);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 0);
    assert_eq!(stats.entries, 0);
    assert_eq!(store.decode_calls(), 2);
}

#[test]
fn hits_are_exact_and_counted() {
    let data = bytes(256 * 1024, 2);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(1024 * 1024)).unwrap();
    let id = seeded(&mut store, b"cached", &data, b"cache-hit");
    let token = auth(&store, id);

    assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    let stats = store.cache_stats();
    assert!(stats.enabled);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.admissions, 1);
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.bytes, data.len() as u64);
    assert_eq!(store.decode_calls(), 1);

    // Exact hit: the same bytes, one fewer reconstruction.
    for _ in 0..3 {
        assert_eq!(store.read_object_authorized(&token).unwrap(), data);
    }
    let stats = store.cache_stats();
    assert_eq!(stats.hits, 3);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.bytes_served, 3 * data.len() as u64);
    assert_eq!(store.decode_calls(), 1);

    // A partial range read still returns the exact slice (single-flight off).
    let range = store.read_range_authorized(&token, 100_000, 4096).unwrap();
    assert_eq!(range, &data[100_000..104_096]);

    // The cache never changes what verification sees.
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn a_new_version_misses_and_removes_the_old_entry() {
    let v1 = bytes(256 * 1024, 3);
    let mut v2 = v1.clone();
    v2[100..104].copy_from_slice(b"R8!!");
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(1024 * 1024)).unwrap();
    let id = seeded(&mut store, b"versioned", &v1, b"cache-version");
    let first = auth(&store, id);
    assert_eq!(store.read_object_authorized(&first).unwrap(), v1);
    assert_eq!(store.cache_stats().entries, 1);

    store.write_file(id, 100, b"R8!!").unwrap();
    let second = auth(&store, id);
    assert_ne!(first.content(), second.content());
    assert_eq!(store.read_object_authorized(&second).unwrap(), v2);
    let stats = store.cache_stats();
    assert_eq!(stats.misses, 2, "the new version must miss the old entry");
    assert_eq!(stats.admissions, 2);
    assert_eq!(stats.stale_removals, 1);
    assert_eq!(stats.entries, 1);
    assert_eq!(store.decode_calls(), 2);

    // The new version hits; the old content can never be served again.
    assert_eq!(store.read_object_authorized(&second).unwrap(), v2);
    assert_eq!(store.cache_stats().hits, 1);
    assert_eq!(store.decode_calls(), 2);
}

#[test]
fn eviction_prefers_the_lowest_cost_avoided_per_byte() {
    let a_bytes = bytes(64 * 1024, 4);
    let b_bytes = bytes(64 * 1024, 5);
    let c_bytes = bytes(64 * 1024, 6);
    let dir = tempdir().unwrap();
    // Exactly two 64 KiB entries fit.
    let mut store = Store::init(dir.path(), opts(128 * 1024)).unwrap();
    let a = seeded(&mut store, b"a", &a_bytes, b"cache-a");
    let b = seeded(&mut store, b"b", &b_bytes, b"cache-b");
    let c = seeded(&mut store, b"c", &c_bytes, b"cache-c");
    // a is a shared base (two dependents); c has one; b has none. Duplicate
    // assertions have the same relation id, so each dependent needs its own
    // object.
    let x = seeded(&mut store, b"x", b"x", b"cache-x");
    let y = seeded(&mut store, b"y", b"y", b"cache-y");
    let z = seeded(&mut store, b"z", b"z", b"cache-z");
    derived_from(&mut store, x, a);
    derived_from(&mut store, y, a);
    derived_from(&mut store, z, c);

    let a_token = auth(&store, a);
    let b_token = auth(&store, b);
    let c_token = auth(&store, c);
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.read_object_authorized(&b_token).unwrap(), b_bytes);
    assert_eq!(store.read_object_authorized(&c_token).unwrap(), c_bytes);

    let stats = store.cache_stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.evictions, 1);
    // b has zero dependents and the same size/class, so it has the lowest
    // expected cost avoided per byte and is the deterministic victim.
    let before = store.decode_calls();
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.read_object_authorized(&c_token).unwrap(), c_bytes);
    assert_eq!(
        store.decode_calls(),
        before,
        "the retained entries must still hit"
    );
    assert_eq!(store.read_object_authorized(&b_token).unwrap(), b_bytes);
    assert_eq!(store.decode_calls(), before + 1, "b was evicted");
}

#[test]
fn a_pinned_object_survives_eviction_and_an_unpinned_one_does_not() {
    let a_bytes = bytes(64 * 1024, 7);
    let b_bytes = bytes(64 * 1024, 8);
    let c_bytes = bytes(64 * 1024, 9);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(128 * 1024)).unwrap();
    let a = seeded(&mut store, b"a", &a_bytes, b"cache-pin-a");
    let b = seeded(&mut store, b"b", &b_bytes, b"cache-pin-b");
    let c = seeded(&mut store, b"c", &c_bytes, b"cache-pin-c");
    let x = seeded(&mut store, b"x", b"x", b"cache-pin-x");
    let y = seeded(&mut store, b"y", b"y", b"cache-pin-y");
    let z = seeded(&mut store, b"z", b"z", b"cache-pin-z");
    derived_from(&mut store, x, a); // a = 1 dependent
    derived_from(&mut store, y, c);
    derived_from(&mut store, z, c); // c = 2 dependents

    let a_token = auth(&store, a);
    let b_token = auth(&store, b);
    let c_token = auth(&store, c);
    store.pin_cache_object(a).unwrap();
    assert!(store.is_cache_object_pinned(a));
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.read_object_authorized(&b_token).unwrap(), b_bytes);
    assert_eq!(store.read_object_authorized(&c_token).unwrap(), c_bytes);

    // a is pinned, so b (0 dependents) was evicted.
    let before = store.decode_calls();
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.decode_calls(), before, "pinned a must still hit");
    assert_eq!(store.read_object_authorized(&b_token).unwrap(), b_bytes);
    assert_eq!(store.decode_calls(), before + 1, "b was evicted");

    // Unpin a; pin c. Admitting a fresh object must now evict a (the only
    // unpinned resident), never the pinned c.
    store.unpin_cache_object(a);
    assert!(!store.is_cache_object_pinned(a));
    store.pin_cache_object(c).unwrap();
    let before = store.decode_calls();
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.decode_calls(), before, "a is still resident and hits");
    // c was evicted when b was admitted above; re-admitting it evicts b (the
    // lowest score), not the pinned a.
    assert_eq!(store.read_object_authorized(&c_token).unwrap(), c_bytes);
    assert_eq!(store.decode_calls(), before + 1);
    let d = seeded(&mut store, b"d", &bytes(64 * 1024, 10), b"cache-pin-d");
    let d_token = auth(&store, d);
    assert_eq!(
        store.read_object_authorized(&d_token).unwrap(),
        bytes(64 * 1024, 10)
    );
    let before = store.decode_calls();
    assert_eq!(store.read_object_authorized(&c_token).unwrap(), c_bytes);
    assert_eq!(store.decode_calls(), before, "pinned c must still hit");
    assert_eq!(store.read_object_authorized(&a_token).unwrap(), a_bytes);
    assert_eq!(store.decode_calls(), before + 1, "unpinned a was evicted");
}

#[test]
fn authorization_is_checked_before_the_lookup() {
    let data = bytes(128 * 1024, 11);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(1024 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"secret",
            &data,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"cache-auth",
        )
        .unwrap();
    let owner = auth(&store, id);
    assert_eq!(store.read_object_authorized(&owner).unwrap(), data);
    assert_eq!(store.cache_stats().entries, 1);

    // Remove all read bits. The bytes are still cached, but a non-owner read
    // must be denied without ever consulting the cache.
    store
        .setattr(
            id,
            weftfs::store::SetAttr {
                mode: Some(0o400),
                ..Default::default()
            },
        )
        .unwrap();
    let outsider = Principal::new(1000, 1000, Vec::new());
    let err = store
        .authorize_read(id, &outsider, store.clock_now_ns())
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert_eq!(
        store.cache_stats().hits,
        0,
        "a denial must not reveal a cached entry"
    );

    // The owner still hits the cache.
    let owner = auth(&store, id);
    assert_eq!(store.read_object_authorized(&owner).unwrap(), data);
    assert_eq!(store.cache_stats().hits, 1);
}

#[test]
fn a_token_cannot_read_a_different_content_root() {
    let data = bytes(64 * 1024, 12);
    let other = bytes(64 * 1024, 13);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(1024 * 1024)).unwrap();
    let id = seeded(&mut store, b"one", &data, b"cache-token-one");
    let other_id = seeded(&mut store, b"two", &other, b"cache-token-two");
    let token = auth(&store, id);
    let other_record = store.object_meta(other_id).unwrap();
    let err = store
        .read_content_authorized(&token, other_record.content, 0, other_record.size)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    // The authorized root still works.
    assert_eq!(
        store
            .read_content_authorized(&token, token.content(), 0, token.size())
            .unwrap(),
        data
    );
}

#[test]
fn counters_and_clear_are_honest() {
    let a = bytes(64 * 1024, 14);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(1024 * 1024)).unwrap();
    let id = seeded(&mut store, b"counters", &a, b"cache-counters");
    let token = auth(&store, id);
    store.read_object_authorized(&token).unwrap();
    store.read_object_authorized(&token).unwrap();
    let stats = store.cache_stats();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.admissions, 1);
    assert_eq!(stats.evictions, 0);
    assert_eq!(stats.rejections, 0);
    assert_eq!(stats.bytes_served, a.len() as u64);

    store.clear_object_cache();
    let stats = store.cache_stats();
    assert_eq!(stats.entries, 0);
    assert_eq!(stats.bytes, 0);
    // Counters are cumulative, not reset by `clear`.
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);

    store.read_object_authorized(&token).unwrap();
    let stats = store.cache_stats();
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.admissions, 2);
}

#[test]
fn single_flight_shares_a_miss_but_the_cache_alone_does_not() {
    let data = bytes(256 * 1024, 15);
    let dir = tempdir().unwrap();
    let mut options = opts(1024 * 1024);
    options.single_flight = true;
    let mut store = Store::init(dir.path(), options).unwrap();
    let id = seeded(&mut store, b"shared", &data, b"cache-sf");
    let store = std::sync::Arc::new(store);
    store.set_single_flight_test_dwell(200_000_000);

    let threads = 4usize;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(threads));
    let mut handles = Vec::new();
    for _ in 0..threads {
        let store = std::sync::Arc::clone(&store);
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let token = auth(&store, id);
            barrier.wait();
            store.read_object_authorized(&token).unwrap()
        }));
    }
    for handle in handles {
        assert_eq!(handle.join().unwrap(), data);
    }
    let stats = store.cache_stats();
    assert_eq!(
        stats.misses, threads as u64,
        "each waiter looked up before the hit"
    );
    assert_eq!(stats.admissions, 1, "the leader admits the result once");
    assert_eq!(store.decode_calls(), 1);
}
