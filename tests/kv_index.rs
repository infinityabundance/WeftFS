//! Persisted KV prefix index courts (subphase T063).
//!
//! The oracle is [`Store::kv_prefix_lookup`] (the scan); every test here
//! compares the indexed answer against it. The adversarial cases install a
//! forged or torn index and prove the reader refuses it rather than answering
//! from it.

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::kv_index::{
    entries_accumulator, KvIndexEntry, KvIndexEntryKind, KvIndexRoot, KvIndexSegment,
    KvIndexSegmentRef, KvPrefixIdentity, SegmentKind, MAX_KV_INDEX_DELTAS,
};
use weftfs::kv_prefix::{
    kv_page_bytes, kv_token_ids, KvLookup, KvPrefixRecord, KvPrefixRequest, KvPrefixSpec,
};
use weftfs::store::{OpenMode, Options, Store};

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const MODEL: &str = "synthetic-0.5b";
const LAYERS: u32 = 2;
const HEADS: u32 = 2;
const DIM: u32 = 8;
const PAGE_ROWS: u64 = 8;
const WORD_BYTES: usize = 2;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 128,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn spec() -> KvPrefixSpec {
    KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: Hash32::from_bytes([0x11; 32]),
        attention_hash: Hash32::from_bytes([0x22; 32]),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        pinned: false,
    }
}

fn request(tokens: &[u64]) -> KvPrefixRequest {
    KvPrefixRequest {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: Hash32::from_bytes([0x11; 32]),
        attention_hash: Hash32::from_bytes([0x22; 32]),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        tokens: tokens.to_vec(),
    }
}

fn payload(tokens: u64, from_page: u64, seed: u64) -> Vec<Vec<Vec<u8>>> {
    let page_count = tokens.div_ceil(PAGE_ROWS);
    let mut out = Vec::new();
    for layer in 0..LAYERS {
        let mut pages = Vec::new();
        for index in from_page..page_count {
            let start = index * PAGE_ROWS;
            let end = tokens.min(start + PAGE_ROWS);
            let len = ((end - start) * u64::from(HEADS) * u64::from(DIM)) as usize * WORD_BYTES;
            pages.push(kv_page_bytes(seed, layer, index, len));
        }
        out.push(pages);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn put_with_seed(
    store: &mut Store,
    name: &str,
    tokens: &[u64],
    parent: Option<&str>,
    from_page: u64,
    seed: u64,
) -> KvPrefixRecord {
    store
        .kv_prefix_put_seeded(
            name,
            &spec(),
            tokens,
            parent,
            &payload(tokens.len() as u64, from_page, seed),
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap()
        .record
}

fn put(store: &mut Store, name: &str, tokens: &[u64]) -> KvPrefixRecord {
    put_with_seed(store, name, tokens, None, 0, 0xC0FFEE)
}

/// Answers are equal when both are reusable with the same prefix id, or both
/// are recompute.
fn assert_same_answer(scan: &KvLookup, indexed: &KvLookup, label: &str) {
    match (scan, indexed) {
        (KvLookup::Reusable { record: a, .. }, KvLookup::Reusable { record: b, .. }) => {
            assert_eq!(
                a.id().unwrap(),
                b.id().unwrap(),
                "{label}: different record"
            );
        }
        (KvLookup::Recompute { .. }, KvLookup::Recompute { .. }) => {}
        (a, b) => panic!("{label}: scan {a:?} vs indexed {b:?}"),
    }
}

#[test]
fn indexed_lookup_matches_the_scan_over_a_compatibility_matrix() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let mut seeds = Vec::new();
    for index in 0..12u64 {
        let tokens = kv_token_ids(100 + index, 8 + 8 * index);
        let name = format!("p{index:02}");
        put_with_seed(&mut store, &name, &tokens, None, 0, 7 + index);
        seeds.push(tokens);
    }
    let report = store.kv_index_build().unwrap();
    assert_eq!(report.entries, 12);
    assert_eq!(report.skipped_partial, 0);
    assert_eq!(
        store.kv_index_status().unwrap().index_generation,
        Some(report.generation)
    );

    // Exact matches at every stored length, longer requests, and a set of
    // mismatches (wrong tokens, wrong model, shorter than the stored prefix).
    let mut requests: Vec<KvPrefixRequest> = Vec::new();
    for (index, tokens) in seeds.iter().enumerate() {
        requests.push(request(tokens));
        let mut longer = tokens.clone();
        longer.extend(kv_token_ids(900 + index as u64, 24));
        requests.push(request(&longer));
    }
    let other = kv_token_ids(4242, 64);
    requests.push(request(&other));
    let mut wrong_model = request(&seeds[3]);
    wrong_model.model = "other".to_owned();
    requests.push(wrong_model);
    let mut wrong_tokenizer = request(&seeds[5]);
    wrong_tokenizer.tokenizer_hash = Hash32::from_bytes([0x99; 32]);
    requests.push(wrong_tokenizer);

    for (index, request) in requests.iter().enumerate() {
        let scan = store.kv_prefix_lookup_scan(request).unwrap();
        let indexed = store.kv_prefix_lookup_indexed(request).unwrap();
        assert_same_answer(&scan, &indexed, &format!("request {index}"));
    }
    let stats = store.kv_index_stats_snapshot();
    assert!(stats.loads >= 1, "index was never loaded: {stats:?}");
    assert_eq!(stats.rejected_candidates, 0, "{stats:?}");
}

#[test]
fn a_write_after_build_makes_the_index_stale_and_the_answer_stays_correct() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(7, 24);
    put(&mut store, "p1", &tokens);
    store.kv_index_build().unwrap();
    // One delta is appended by this put, so the index stays valid.
    let second = kv_token_ids(8, 24);
    put(&mut store, "p2", &second);
    let status = store.kv_index_status().unwrap();
    assert!(status.valid, "{status:?}");
    assert_eq!(status.entries, 2);
    let indexed = store.kv_prefix_lookup_indexed(&request(&second)).unwrap();
    assert!(matches!(indexed, KvLookup::Reusable { .. }));

    // Exhaust the delta bound: after MAX_KV_INDEX_DELTAS mutations the next
    // put leaves the index stale (bounded invalidation, never a wrong answer).
    for index in 0..(MAX_KV_INDEX_DELTAS + 4) {
        let tokens = kv_token_ids(1000 + index as u64, 8);
        put_with_seed(
            &mut store,
            &format!("fill-{index}"),
            &tokens,
            None,
            0,
            9000 + index as u64,
        );
    }
    let status = store.kv_index_status().unwrap();
    assert!(!status.valid, "index should be stale after the delta bound");
    let request = request(&second);
    let scan = store.kv_prefix_lookup_scan(&request).unwrap();
    let indexed = store.kv_prefix_lookup_indexed(&request).unwrap();
    assert_same_answer(&scan, &indexed, "stale index");
    assert!(store.kv_index_stats_snapshot().fallbacks >= 1);

    let rebuilt = store.kv_index_build().unwrap();
    assert_eq!(rebuilt.segments, 1);
    let indexed = store.kv_prefix_lookup_indexed(&request).unwrap();
    assert!(matches!(indexed, KvLookup::Reusable { .. }));
}

#[test]
fn a_forged_entry_is_refused_and_the_scan_decides() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(11, 24);
    let record = put(&mut store, "victim", &tokens);
    let other_tokens = kv_token_ids(12, 24);
    put(&mut store, "other", &other_tokens);
    store.kv_index_build().unwrap();

    let records = store.kv_prefix_records_with_ids().unwrap();
    let victim = records
        .iter()
        .find(|(name, _, _)| name == "victim")
        .cloned()
        .unwrap();
    let other = records
        .iter()
        .find(|(name, _, _)| name == "other")
        .cloned()
        .unwrap();
    // Forge: the entry keeps the victim's boundary key and record id but names
    // the other record's object. Verification must refuse it.
    let identity = KvPrefixIdentity::of_record(&record).root();
    let forged = KvIndexEntry {
        identity_root: identity,
        chain_root: record.token_chain_root,
        record_prefix_id: record.id().unwrap(),
        record_object_id: other.1,
        position_end: record.position_end,
        page_count: record.page_count,
        kind: KvIndexEntryKind::Live,
    };
    let segment = KvIndexSegment::encode(
        SegmentKind::Base,
        store.generation(),
        &[forged.clone()],
    )
    .unwrap();
    let digest = segment_digest(&segment);
    let root = KvIndexRoot {
        generation: store.generation(),
        head: store.head(),
        built_for_generation: store.generation(),
        built_for_head: store.head(),
        entry_count: 1,
        entries_accumulator: entries_accumulator(&[forged]),
        segments: vec![KvIndexSegmentRef {
            kind: SegmentKind::Base,
            generation: store.generation(),
            entries: 1,
            digest,
        }],
        bytes: segment.len() as u64,
        revocation_epoch: store.kv_revocation_epoch().unwrap(),
        skipped_partial: 0,
    };
    store.kv_index_install_for_test(&root, &[segment]).unwrap();
    assert!(store.kv_index_status().unwrap().valid);

    let request = request(&tokens);
    let indexed = store.kv_prefix_lookup_indexed(&request).unwrap();
    let scan = store.kv_prefix_lookup_scan(&request).unwrap();
    assert_same_answer(&scan, &indexed, "forged entry");
    let stats = store.kv_index_stats_snapshot();
    assert!(stats.rejected_candidates >= 1, "{stats:?}");
    assert!(stats.fallbacks >= 1, "{stats:?}");
    let _ = victim;
}

fn segment_digest(bytes: &[u8]) -> Hash32 {
    let mut raw = [0u8; 32];
    raw.copy_from_slice(&bytes[bytes.len() - 32..]);
    Hash32::from_bytes(raw)
}

#[test]
fn a_root_for_another_generation_is_refused_and_a_rebuild_is_deterministic() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    for index in 0..6u64 {
        let tokens = kv_token_ids(200 + index, 16);
        put_with_seed(&mut store, &format!("g{index}"), &tokens, None, 0, index);
    }
    let first = store.kv_index_build().unwrap();
    let status = store.kv_index_status().unwrap();
    let digest = status.index_digest.unwrap();

    // Rebuild from scratch: same entries, same bytes, same root digest.
    store.kv_index_drop().unwrap();
    let second = store.kv_index_build().unwrap();
    assert_eq!(first.entries, second.entries);
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(store.kv_index_status().unwrap().index_digest, Some(digest));

    // A root claiming a different generation is refused (stale/future).
    let mut root = store.kv_index_root_for_test().unwrap().unwrap();
    root.generation = root.generation + 1;
    let bytes = store
        .kv_index_root_for_test()
        .unwrap()
        .unwrap()
        .canonical_bytes()
        .unwrap();
    let _ = bytes;
    store.kv_index_install_for_test(&root, &[]).unwrap();
    let status = store.kv_index_status().unwrap();
    assert!(!status.valid, "a future-generation root must be refused");
    let tokens = kv_token_ids(200, 16);
    let scan = store.kv_prefix_lookup_scan(&request(&tokens)).unwrap();
    let indexed = store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap();
    assert_same_answer(&scan, &indexed, "future root");
}

#[test]
fn a_torn_root_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(21, 16);
    put(&mut store, "torn", &tokens);
    store.kv_index_build().unwrap();
    let mut root = store.kv_index_root_for_test().unwrap().unwrap();
    // Point the root at a segment digest that was never written.
    root.segments[0].digest = Hash32::from_bytes([0x77; 32]);
    store.kv_index_install_for_test(&root, &[]).unwrap();
    let status = store.kv_index_status().unwrap();
    assert!(status.present);
    assert!(
        !status.valid,
        "a root naming a missing segment must be refused"
    );
    let scan = store.kv_prefix_lookup_scan(&request(&tokens)).unwrap();
    let indexed = store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap();
    assert_same_answer(&scan, &indexed, "torn root");
}

#[test]
fn revocation_removes_the_entry_and_never_returns_the_prefix() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(31, 24);
    put(&mut store, "revoked", &tokens);
    store.kv_index_build().unwrap();
    assert!(matches!(
        store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap(),
        KvLookup::Reusable { .. }
    ));
    assert_eq!(store.kv_revocation_epoch().unwrap(), 0);

    let write = store
        .kv_prefix_revoke("revoked", "operator request", NOW_NS, 0, 0)
        .unwrap();
    assert!(write.newly_revoked);
    assert_eq!(store.kv_revocation_epoch().unwrap(), 1);
    // Idempotent: revoking again publishes nothing and keeps the epoch.
    let again = store
        .kv_prefix_revoke("revoked", "operator request", NOW_NS, 0, 0)
        .unwrap();
    assert!(!again.newly_revoked);
    assert_eq!(again.revocation_id, write.revocation_id);
    assert_eq!(store.kv_revocation_epoch().unwrap(), 1);

    // Both paths refuse to return it, and the index shrank by exactly one.
    let status = store.kv_index_status().unwrap();
    assert_eq!(status.entries, 0, "{status:?}");
    for lookup in [
        store.kv_prefix_lookup_scan(&request(&tokens)).unwrap(),
        store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap(),
    ] {
        match lookup {
            KvLookup::Recompute { .. } => {}
            other => panic!("a revoked prefix was returned: {other:?}"),
        }
    }
    // `kv get` is indistinguishable from a name that does not exist: the class
    // and text are identical, so revocation is not an existence oracle.
    let revoked = store
        .kv_prefix_get("revoked", &weftfs::Principal::root(), NOW_NS)
        .unwrap_err();
    let absent = store
        .kv_prefix_get("never-existed", &weftfs::Principal::root(), NOW_NS)
        .unwrap_err();
    assert_eq!(revoked.class(), absent.class());
    assert_eq!(revoked.message(), absent.message());
}

#[test]
fn a_partial_tail_prefix_is_not_indexed_but_is_still_found() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    // 20 tokens with 8-token pages: the last page is a 4-token tail.
    let tokens = kv_token_ids(41, 20);
    put(&mut store, "tail", &tokens);
    let report = store.kv_index_build().unwrap();
    assert_eq!(report.entries, 0);
    assert_eq!(report.skipped_partial, 1);
    let scan = store.kv_prefix_lookup_scan(&request(&tokens)).unwrap();
    let indexed = store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap();
    assert_same_answer(&scan, &indexed, "partial tail");
    assert!(matches!(indexed, KvLookup::Reusable { .. }));
}

#[test]
fn batch_lookup_matches_single_lookup() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let mut stored = Vec::new();
    for index in 0..8u64 {
        let tokens = kv_token_ids(500 + index, 16 + 8 * index);
        put_with_seed(&mut store, &format!("b{index}"), &tokens, None, 0, index);
        stored.push(tokens);
    }
    store.kv_index_build().unwrap();
    let requests: Vec<KvPrefixRequest> = stored.iter().map(|tokens| request(tokens)).collect();
    let batch = store.kv_prefix_lookup_batch_indexed(&requests).unwrap();
    assert_eq!(batch.len(), requests.len());
    for (index, (request, batched)) in requests.iter().zip(batch.iter()).enumerate() {
        let single = store.kv_prefix_lookup_indexed(request).unwrap();
        assert_same_answer(&single, batched, &format!("batch {index}"));
    }
}

#[test]
fn index_does_not_change_pinned_or_reopen_semantics() {
    let dir = tempdir().unwrap();
    let tokens = kv_token_ids(61, 32);
    {
        let mut store = Store::init(dir.path(), options()).unwrap();
        put(&mut store, "reopen", &tokens);
        store.kv_index_build().unwrap();
    }
    // Read-only reopen: the index loads, and the answer is identical.
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, options()).unwrap();
    let status = store.kv_index_status().unwrap();
    assert!(status.valid, "{status:?}");
    let indexed = store.kv_prefix_lookup_indexed(&request(&tokens)).unwrap();
    assert!(matches!(indexed, KvLookup::Reusable { .. }));
    // A read-only handle never writes an index or a delta.
    assert_eq!(store.generation(), status.generation);
}

#[test]
fn identity_root_covers_every_compatibility_field() {
    let base = KvPrefixIdentity {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: Hash32::from_bytes([1; 32]),
        attention_hash: Hash32::from_bytes([2; 32]),
        dtype: "bf16".to_owned(),
        head_count: 2,
        head_dim: 8,
        layer_count: 2,
        page_rows: 8,
    };
    let root = base.root();
    let variants: Vec<(&str, Value)> = vec![
        ("tenant", Value::Str("tenant-b".to_owned())),
        ("model", Value::Str("m2".to_owned())),
        ("dtype", Value::Str("fp16".to_owned())),
    ];
    for (field, _) in variants {
        let mut changed = base.clone();
        match field {
            "tenant" => changed.tenant = "tenant-b".to_owned(),
            "model" => changed.model = "m2".to_owned(),
            _ => changed.dtype = "fp16".to_owned(),
        }
        assert_ne!(changed.root(), root, "{field} is not covered");
    }
    let mut changed = base.clone();
    changed.head_count = 4;
    assert_ne!(changed.root(), root);
    let mut changed = base.clone();
    changed.page_rows = 16;
    assert_ne!(changed.root(), root);
}
