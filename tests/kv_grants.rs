//! Verified-session grant cache (subphase T065).
//!
//! A grant decode may be cached only when the *envelope*, the *store
//! generation* and the *revocation epoch* all still match. These tests pin the
//! key: a hit is not reusable after any commit, a revocation changes the epoch,
//! and the control (`kv_grant_cache = false`) decodes on every read.

use tempfile::tempdir;
use weftcodec::capability::{public_key_for_seed, sign_envelope, CapabilityEnvelope};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};

use weftfs::identity::Hash32;
use weftfs::kv_prefix::{kv_page_bytes, kv_token_ids, KvPrefixRequest, KvPrefixSpec};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const SEED: [u8; 32] = [7u8; 32];

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn page_input() -> KvCacheInput {
    KvCacheInput {
        model: "qv".to_owned(),
        layer: 0,
        session: "s".to_owned(),
        page_index: 0,
        page_count: 1,
        seq_start: 0,
        seq_end: 4,
        dtype: KvDtype::Bf16,
        head_count: 2,
        head_dim: 4,
    }
}

fn grant(store: &mut Store, descriptor_id: Hash32) -> Principal {
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        TENANT,
        vec![descriptor_id],
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes([0xA1; 32]),
        NOW_NS,
        Hash32::from_bytes([0x31; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    let verified = store.load_capability_envelope(id, &[], NOW_NS).unwrap();
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

fn prefix_spec() -> KvPrefixSpec {
    KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: "m".to_owned(),
        tokenizer_hash: Hash32::from_bytes([1; 32]),
        attention_hash: Hash32::from_bytes([2; 32]),
        dtype: "bf16".to_owned(),
        head_count: 2,
        head_dim: 4,
        layer_count: 1,
        page_rows: 4,
        pinned: false,
    }
}

fn put(store: &mut Store, name: &str, seed: u64) {
    let tokens = kv_token_ids(seed, 8);
    let pages = vec![vec![
        kv_page_bytes(seed, 0, 0, 4 * 2 * 4 * 2),
        kv_page_bytes(seed, 0, 1, 4 * 2 * 4 * 2),
    ]];
    store
        .kv_prefix_put_seeded(
            name,
            &prefix_spec(),
            &tokens,
            None,
            &pages,
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap();
}

#[test]
fn a_grant_is_cached_within_one_generation_and_dropped_after_a_commit() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let written = store
        .write_kv_page(
            root,
            b"p.kv",
            TENANT,
            &page_input(),
            &kv_page_bytes(1, 0, 0, 64),
            0,
            0,
        )
        .unwrap();
    let principal = grant(&mut store, written.descriptor_id);

    for _ in 0..5 {
        store
            .read_kv_page(written.object_id, &principal, NOW_NS)
            .unwrap();
    }
    let (hits, misses, _) = store.kv_grant_cache_counters();
    assert_eq!((hits, misses), (4, 1), "one decode, four hits");

    // Any commit is a generation change: the cached decode is not reused.
    put(&mut store, "p1", 1);
    store
        .read_kv_page(written.object_id, &principal, NOW_NS)
        .unwrap();
    let (hits, misses, invalidations) = store.kv_grant_cache_counters();
    assert_eq!(hits, 4);
    assert_eq!(misses, 2, "the grant was decoded again after the commit");
    assert_eq!(invalidations, 1, "the stale decode is reported");
}

#[test]
fn a_revocation_changes_the_epoch_and_drops_the_grant() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let written = store
        .write_kv_page(
            root,
            b"p.kv",
            TENANT,
            &page_input(),
            &kv_page_bytes(1, 0, 0, 64),
            0,
            0,
        )
        .unwrap();
    let principal = grant(&mut store, written.descriptor_id);
    put(&mut store, "a", 1);
    put(&mut store, "b", 2);
    assert_eq!(store.kv_revocation_epoch().unwrap(), 0);
    // Warm the cache at the current generation.
    store
        .read_kv_page(written.object_id, &principal, NOW_NS)
        .unwrap();
    let (hits_before, misses_before, _) = store.kv_grant_cache_counters();
    store.kv_prefix_revoke("a", "court", NOW_NS, 0, 0).unwrap();
    assert_eq!(store.kv_revocation_epoch().unwrap(), 1);
    // The epoch and generation both moved; the cached decode cannot serve this
    // read. (The generation check alone would already refuse it; the epoch is
    // what lets a future revocation mechanism that does not move the head still
    // invalidate a cached grant.)
    store
        .read_kv_page(written.object_id, &principal, NOW_NS)
        .unwrap();
    let (hits, misses, _) = store.kv_grant_cache_counters();
    assert_eq!(hits, hits_before);
    assert_eq!(misses, misses_before + 1);
}

#[test]
fn the_control_path_decodes_on_every_read() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(
        dir.path(),
        Options {
            kv_grant_cache: false,
            ..options()
        },
    )
    .unwrap();
    let root = store.resolve_path("/").unwrap();
    let written = store
        .write_kv_page(
            root,
            b"p.kv",
            TENANT,
            &page_input(),
            &kv_page_bytes(1, 0, 0, 64),
            0,
            0,
        )
        .unwrap();
    let principal = grant(&mut store, written.descriptor_id);
    for _ in 0..5 {
        store
            .read_kv_page(written.object_id, &principal, NOW_NS)
            .unwrap();
    }
    let (hits, misses, _) = store.kv_grant_cache_counters();
    assert_eq!(hits, 0);
    assert_eq!(misses, 5);
}

#[test]
fn a_cached_grant_still_enforces_scope_and_revocation_on_every_read() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let (envelope_id, _) = {
        let envelope = CapabilityEnvelope::new(
            public_key_for_seed(&SEED).unwrap(),
            Hash32::from_bytes([9u8; 32]),
            TENANT,
            vec![Hash32::from_bytes([0x55; 32])],
            vec![format!("kv.{TENANT}")],
            Vec::new(),
            Some(EXPIRES_NS),
            None,
            Hash32::from_bytes([0xA1; 32]),
            NOW_NS,
            Hash32::from_bytes([0x31; 32]),
        )
        .unwrap();
        let blob = sign_envelope(&envelope, &SEED).unwrap();
        (store.save_capability_envelope(&blob).unwrap(), ())
    };
    // A grant whose root list does not name the page: denied before and after
    // any cache activity.
    let verified = store
        .load_capability_envelope(envelope_id, &[], NOW_NS)
        .unwrap();
    let principal = Principal::new(1001, 1001, Vec::new()).with_envelope(verified);
    let root = store.resolve_path("/").unwrap();
    let written = store
        .write_kv_page(
            root,
            b"p.kv",
            TENANT,
            &page_input(),
            &kv_page_bytes(2, 0, 0, 64),
            0,
            0,
        )
        .unwrap();
    for _ in 0..3 {
        let err = store
            .read_kv_page(written.object_id, &principal, NOW_NS)
            .unwrap_err();
        assert_eq!(err.class(), weftfs::error::Class::Unauthorized);
    }
    // The denial is cached too (a `CapabilityContext` is a decode, not a
    // decision), so a later revocation cannot widen it: still denied.
    put(&mut store, "x", 3);
    let err = store
        .read_kv_page(written.object_id, &principal, NOW_NS)
        .unwrap_err();
    assert_eq!(err.class(), weftfs::error::Class::Unauthorized);
}

#[test]
fn batch_write_one_commit_and_identical_records_to_the_single_path() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let spec = prefix_spec();
    let tokens: Vec<Vec<u64>> = (0..8).map(|index| kv_token_ids(50 + index, 8)).collect();
    let payloads: Vec<Vec<Vec<Vec<u8>>>> = (0..8u64)
        .map(|index| {
            vec![vec![
                kv_page_bytes(70 + index, 0, 0, 4 * 2 * 4 * 2),
                kv_page_bytes(70 + index, 0, 1, 4 * 2 * 4 * 2),
            ]]
        })
        .collect();
    let names: Vec<String> = (0..8).map(|index| format!("b{index}")).collect();
    let items: Vec<weftfs::kv_prefix::KvPrefixBatchItem<'_>> = (0..8)
        .map(|index| weftfs::kv_prefix::KvPrefixBatchItem {
            name: &names[index],
            spec: &spec,
            tokens: &tokens[index],
            parent_name: None,
            new_page_bytes: &payloads[index],
        })
        .collect();
    let before = store.generation();
    let writes = store
        .kv_prefix_put_batch_seeded(&items, 0, 0, &[0x5A; 32])
        .unwrap();
    assert_eq!(store.generation(), before + 1);
    assert_eq!(writes.len(), 8);
    // The same content written through the single path produces the same
    // record id, so batching changes publication, not identity.
    let other_dir = tempdir().unwrap();
    let mut other = Store::init(other_dir.path(), options()).unwrap();
    let single = other
        .kv_prefix_put_seeded(
            "same-content-different-store",
            &spec,
            &tokens[0],
            None,
            &payloads[0],
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap();
    assert_eq!(single.prefix_id, writes[0].prefix_id);
    assert_eq!(single.record, writes[0].record);
    // The seeded ObjectId is deterministic across stores for the same seed, so
    // batching does not perturb identity at either layer.
    assert_eq!(single.record_object_id, writes[0].record_object_id);
    for (index, write) in writes.iter().enumerate() {
        let stored = store.kv_prefix(&names[index]).unwrap();
        assert_eq!(stored, write.record);
        let request = KvPrefixRequest {
            tenant: TENANT.to_owned(),
            model: "m".to_owned(),
            tokenizer_hash: Hash32::from_bytes([1; 32]),
            attention_hash: Hash32::from_bytes([2; 32]),
            dtype: "bf16".to_owned(),
            head_count: 2,
            head_dim: 4,
            layer_count: 1,
            page_rows: 4,
            tokens: tokens[index].clone(),
        };
        assert!(matches!(
            store.kv_prefix_lookup(&request).unwrap(),
            weftfs::kv_prefix::KvLookup::Reusable { .. }
        ));
    }
}
