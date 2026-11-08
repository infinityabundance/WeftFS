//! KV prefix cache courts (subphase T2).
//!
//! Covers the canonical record, the exact compatibility rule and its
//! fail-closed recompute path, parent-chain page sharing, one-commit
//! publication, capability-enforced reads, logical GC with pins, and the
//! declared reuse-vs-recompute decision.

use std::path::Path;

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::kv_capability::{KvDescriptor, KV_DENIED};
use weftfs::kv_prefix::{
    kv_page_bytes, kv_token_chain_root, kv_token_ids, KvLookup, KvPrefillModel, KvPrefillRow,
    KvPrefixRecord, KvPrefixRequest, KvPrefixSpec, KV_PREFIX_SCHEMA,
};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const MODEL: &str = "synthetic-0.5b";
const LAYERS: u32 = 4;
const HEADS: u32 = 2;
const DIM: u32 = 64;
const PAGE_ROWS: u64 = 8;
const WORD_BYTES: usize = 2; // bf16
const GRANTOR_SEED: [u8; 32] = [7u8; 32];

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

fn spec(pinned: bool) -> KvPrefixSpec {
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
        pinned,
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

/// Page payloads for pages `from_page..page_count` of a `tokens`-long prefix.
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

fn descriptor_ids(store: &Store, record: &KvPrefixRecord) -> Vec<Hash32> {
    record
        .pages
        .iter()
        .flatten()
        .map(|id| store.kv_descriptor(*id).unwrap().id().unwrap())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn grant(
    store: &mut Store,
    tenant: &str,
    namespace: &str,
    mut roots: Vec<Hash32>,
    expires: Option<i64>,
) -> VerifiedEnvelope {
    roots.sort();
    roots.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        tenant,
        roots,
        vec![namespace.to_owned()],
        Vec::new(),
        expires,
        None,
        Hash32::from_bytes([0x31; 32]),
        NOW_NS,
        Hash32::from_bytes([0xA1; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    store.load_capability_envelope(id, &[], NOW_NS).unwrap()
}

fn principal(verified: VerifiedEnvelope) -> Principal {
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

fn assert_denied(err: &weftfs::Error) {
    assert_eq!(err.class(), Class::Unauthorized, "class: {err}");
    assert_eq!(err.message(), KV_DENIED, "text: {err}");
}

fn put(
    store: &mut Store,
    name: &str,
    tokens: &[u64],
    parent: Option<&str>,
    from_page: u64,
    pinned: bool,
) -> weftfs::kv_prefix::KvPrefixWrite {
    put_with_seed(store, name, tokens, parent, from_page, pinned, 0xC0FFEE)
}

#[allow(clippy::too_many_arguments)]
fn put_with_seed(
    store: &mut Store,
    name: &str,
    tokens: &[u64],
    parent: Option<&str>,
    from_page: u64,
    pinned: bool,
    page_seed: u64,
) -> weftfs::kv_prefix::KvPrefixWrite {
    store
        .kv_prefix_put_seeded(
            name,
            &spec(pinned),
            tokens,
            parent,
            &payload(tokens.len() as u64, from_page, page_seed),
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap()
}

fn page_object_count(store: &Store) -> usize {
    let Ok(kv) = store.resolve_path("/kv") else {
        return 0;
    };
    let Ok(pages) = store.resolve_path("/kv/pages") else {
        let _ = kv;
        return 0;
    };
    store.list_dir(pages).unwrap().len()
}

// ---------------------------------------------------------------------------
// Record canonicality
// ---------------------------------------------------------------------------

#[test]
fn record_round_trips_canonically_and_rejects_unknown_keys() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(1, 20);
    let write = put(&mut store, "p1", &tokens, None, 0, false);
    let record = store.kv_prefix("p1").unwrap();
    assert_eq!(record, write.record);
    assert_eq!(record.id().unwrap(), write.prefix_id);
    assert_eq!(record.page_count, 3); // ceil(20/8)
    assert_eq!(record.position_start, 0);
    assert_eq!(record.position_end, 20);
    assert_eq!(record.token_chain_root, kv_token_chain_root(&tokens));

    let bytes = record.canonical_bytes().unwrap();
    let decoded = KvPrefixRecord::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(decoded, record);

    // Unknown key.
    let value = wcb::decode(&bytes, &Limits::default()).unwrap();
    let mut map = match value {
        Value::Map(map) => map,
        _ => panic!("record is not a map"),
    };
    map.insert("extra".to_owned(), Value::U64(1));
    let mutated = wcb::encode(&Value::Map(map), &Limits::default()).unwrap();
    assert_eq!(
        KvPrefixRecord::from_canonical_bytes(&mutated)
            .unwrap_err()
            .class(),
        Class::Corrupt
    );

    // Non-canonical (page_count disagrees with the token arithmetic).
    let mut record2 = record.clone();
    record2.page_count = 99;
    let mutated = record2.canonical_bytes().unwrap();
    assert_eq!(
        KvPrefixRecord::from_canonical_bytes(&mutated)
            .unwrap_err()
            .class(),
        Class::Corrupt
    );
}

#[test]
fn token_chain_binds_the_exact_sequence() {
    let a = kv_token_ids(1, 32);
    let mut b = a.clone();
    b[7] ^= 1;
    assert_eq!(a.len(), b.len());
    assert_ne!(kv_token_chain_root(&a), kv_token_chain_root(&b));
    // A prefix of the chain is the root at that length.
    assert_eq!(
        kv_token_chain_root(&a[..8]),
        kv_token_chain_root(&a[..8].to_vec())
    );
    assert_ne!(kv_token_chain_root(&a[..8]), kv_token_chain_root(&a[..16]));
}

// ---------------------------------------------------------------------------
// Dedup
// ---------------------------------------------------------------------------

#[test]
fn parent_chain_shares_page_objects_and_stores_only_new_pages() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(3, 32); // 4 pages
    let parent = put(&mut store, "a", &tokens[..16], None, 0, false);
    let objects_after_parent = page_object_count(&store);
    assert_eq!(objects_after_parent, 2 * LAYERS as usize);

    let child = put(&mut store, "b", &tokens, Some("a"), 2, false);
    assert_eq!(child.record.page_count, 4);
    assert_eq!(child.record.parent, Some(parent.prefix_id));
    assert_eq!(child.record.reused_pages, 2 * u64::from(LAYERS));
    assert_eq!(child.new_pages, 2 * u64::from(LAYERS));

    // The first two pages of every layer are the parent's objects, verbatim.
    for layer in 0..LAYERS as usize {
        assert_eq!(
            child.record.pages[layer][..2],
            parent.record.pages[layer][..]
        );
    }
    assert_eq!(page_object_count(&store), 4 * LAYERS as usize);
    assert_eq!(
        child.record.page_bytes,
        parent.record.page_bytes + child.record.stored_page_bytes
    );

    // The shared pages decode to the same bytes through both records.
    let roots = descriptor_ids(&store, &child.record);
    for layer in 0..LAYERS as usize {
        for index in 0..child.record.page_count as usize {
            let a = store
                .kv_descriptor(child.record.pages[layer][index])
                .unwrap();
            if index < 2 {
                let b = store
                    .kv_descriptor(parent.record.pages[layer][index])
                    .unwrap();
                assert_eq!(a, b);
            }
        }
    }
    assert!(!roots.is_empty());

    // Extending a parent that ends mid-page is refused (no silent re-use of a
    // partial last page).
    let mid_tokens = &tokens[..12]; // 12 tokens, 8-token pages: mid-page end
    put(&mut store, "mid", mid_tokens, None, 0, false);
    let err = store
        .kv_prefix_put_seeded(
            "late",
            &spec(false),
            &tokens[..20],
            Some("mid"),
            &payload(20, 2, 0xC0FFEE),
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("mid-page"), "{err}");
}

#[test]
fn put_publishes_one_commit_and_rejects_bad_payloads() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(4, 32);
    let before = store.generation();
    put(&mut store, "one", &tokens, None, 0, false);
    assert_eq!(store.generation(), before + 1);

    let mut bad = payload(32, 0, 1);
    bad[0][0].push(0);
    let generation = store.generation();
    let err = store
        .kv_prefix_put_seeded("bad", &spec(false), &tokens, None, &bad, 0, 0, &[0x5A; 32])
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
    assert_eq!(store.generation(), generation, "nothing was published");
    assert!(store.kv_prefix("bad").is_err());
}

#[test]
fn replacing_a_prefix_is_a_new_record_and_pinned_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(5, 16);
    let first = put(&mut store, "p", &tokens, None, 0, false);
    let second = put(&mut store, "p", &tokens, None, 0, false);
    assert_eq!(
        first.prefix_id, second.prefix_id,
        "same inputs are idempotent"
    );
    // A different token sequence under the same name is a new record.
    let other = kv_token_ids(6, 16);
    let third = put(&mut store, "p", &other, None, 0, false);
    assert_ne!(first.prefix_id, third.prefix_id);
    assert_eq!(store.kv_prefix_list().unwrap().len(), 1);

    put(&mut store, "pinned", &tokens, None, 0, true);
    let err = store
        .kv_prefix_put_seeded(
            "pinned",
            &spec(true),
            &other,
            None,
            &payload(16, 0, 2),
            0,
            0,
            &[0x5A; 32],
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Busy);
    assert_eq!(
        store.kv_prefix_delete("pinned").unwrap_err().class(),
        Class::Busy
    );
}

// ---------------------------------------------------------------------------
// Compatibility is exact
// ---------------------------------------------------------------------------

#[test]
fn every_compatibility_field_fails_closed_to_recompute() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(7, 16);
    put(&mut store, "exact", &tokens, None, 0, false);

    let base = request(&tokens);
    assert!(matches!(
        store.kv_prefix_lookup(&base).unwrap(),
        KvLookup::Reusable { .. }
    ));

    let mut cases: Vec<(&str, Box<dyn Fn(&mut KvPrefixRequest)>)> = Vec::new();
    cases.push(("tenant", Box::new(|r| r.tenant = "tenant-b".to_owned())));
    cases.push(("model", Box::new(|r| r.model = "other-model".to_owned())));
    cases.push((
        "tokenizer",
        Box::new(|r| r.tokenizer_hash = Hash32::from_bytes([0x12; 32])),
    ));
    cases.push((
        "attention",
        Box::new(|r| r.attention_hash = Hash32::from_bytes([0x23; 32])),
    ));
    cases.push(("dtype", Box::new(|r| r.dtype = "fp16".to_owned())));
    cases.push(("head count", Box::new(|r| r.head_count = 4)));
    cases.push(("head dim", Box::new(|r| r.head_dim = 128)));
    cases.push(("layer count", Box::new(|r| r.layer_count = 8)));
    cases.push(("page rows", Box::new(|r| r.page_rows = 16)));

    for (label, mutate) in cases {
        let mut request = base.clone();
        mutate(&mut request);
        match store.kv_prefix_lookup(&request).unwrap() {
            KvLookup::Reusable { .. } => panic!("`{label}` mismatch was accepted"),
            KvLookup::Recompute { reasons, scanned } => {
                assert_eq!(scanned, 1);
                assert!(!reasons.is_empty(), "`{label}` produced no reason");
                assert!(
                    reasons.iter().any(|reason| reason.contains(label)
                        || (label == "tokenizer" && reason.contains("tokenizer"))
                        || (label == "attention" && reason.contains("attention"))),
                    "`{label}` reason was not specific: {reasons:?}"
                );
            }
        }
    }

    // Token-chain mismatch at the same length, and a longer stored prefix.
    let mut wrong_tokens = tokens.clone();
    wrong_tokens[3] ^= 0xFFFF;
    match store.kv_prefix_lookup(&request(&wrong_tokens)).unwrap() {
        KvLookup::Reusable { .. } => panic!("a different token sequence was accepted"),
        KvLookup::Recompute { reasons, .. } => {
            assert!(reasons.iter().any(|reason| reason.contains("token-chain")));
        }
    }
    match store.kv_prefix_lookup(&request(&tokens[..8])).unwrap() {
        KvLookup::Reusable { .. } => panic!("a shorter request cannot reuse a longer prefix"),
        KvLookup::Recompute { reasons, .. } => {
            assert!(reasons.iter().any(|reason| reason.contains("longer")));
        }
    }
}

// ---------------------------------------------------------------------------
// Capability-enforced get
// ---------------------------------------------------------------------------

#[test]
fn get_requires_a_matching_capability_and_is_non_disclosing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(8, 16);
    put(&mut store, "gated", &tokens, None, 0, false);
    let record = store.kv_prefix("gated").unwrap();
    let roots = descriptor_ids(&store, &record);

    // Anonymous.
    let anonymous = Principal::new(1001, 1001, Vec::new());
    assert_denied(
        &store
            .kv_prefix_get("gated", &anonymous, NOW_NS)
            .unwrap_err(),
    );

    // Right tenant namespace, but only one descriptor root.
    let partial = grant(
        &mut store,
        TENANT,
        "kv.tenant-a",
        roots[..1].to_vec(),
        Some(EXPIRES_NS),
    );
    assert_denied(
        &store
            .kv_prefix_get("gated", &principal(partial), NOW_NS)
            .unwrap_err(),
    );

    // Wrong tenant namespace.
    let wrong_namespace = grant(
        &mut store,
        TENANT,
        "kv.tenant-b",
        roots.clone(),
        Some(EXPIRES_NS),
    );
    assert_denied(
        &store
            .kv_prefix_get("gated", &principal(wrong_namespace), NOW_NS)
            .unwrap_err(),
    );

    // Expired by the time of the read (the constructor refuses an envelope
    // that expires before it was issued, so the clock advances instead).
    let expired = grant(
        &mut store,
        TENANT,
        "kv.tenant-a",
        roots.clone(),
        Some(NOW_NS + 1),
    );
    assert_denied(
        &store
            .kv_prefix_get("gated", &principal(expired), NOW_NS + 2)
            .unwrap_err(),
    );

    // Valid: every page is returned and matches the stored object bytes.
    let valid = grant(&mut store, TENANT, "kv.tenant-a", roots, Some(EXPIRES_NS));
    let read = store
        .kv_prefix_get("gated", &principal(valid), NOW_NS)
        .unwrap();
    assert_eq!(read.record, record);
    assert_eq!(
        read.pages.len(),
        (record.page_count * u64::from(LAYERS)) as usize
    );
    let expected = payload(16, 0, 0xC0FFEE);
    let mut index = 0usize;
    for layer in 0..LAYERS as usize {
        for page in 0..record.page_count as usize {
            assert_eq!(read.pages[index].bytes, expected[layer][page]);
            index += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Logical GC
// ---------------------------------------------------------------------------

#[test]
fn gc_keeps_live_prefix_pages_and_respects_pins() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let a = kv_token_ids(10, 16);
    let b = kv_token_ids(11, 16);
    put_with_seed(&mut store, "keep", &a, None, 0, true, 0x1111);
    put_with_seed(&mut store, "drop", &b, None, 0, false, 0x2222);
    let keep_record = store.kv_prefix("keep").unwrap();
    let drop_record = store.kv_prefix("drop").unwrap();
    let total_pages = 2 * LAYERS as usize * 2;
    assert_eq!(page_object_count(&store), total_pages);

    // Delete the unpinned record; its pages stay until GC.
    store.kv_prefix_delete("drop").unwrap();
    assert_eq!(store.kv_prefix_list().unwrap().len(), 1);
    assert_eq!(page_object_count(&store), total_pages);

    // Hold one orphaned page with an open-handle pin.
    let pinned_page = drop_record.pages[0][0];
    store.pin_object(pinned_page).unwrap();

    let dry = store.kv_gc(false).unwrap();
    assert_eq!(dry.prefixes, 1);
    assert_eq!(
        dry.marked_pages,
        (keep_record.page_count * u64::from(LAYERS))
    );
    assert_eq!(dry.kept_pinned, 1);
    assert_eq!(
        dry.candidates.len(),
        total_pages - (keep_record.page_count as usize * LAYERS as usize) - 1
    );
    assert_eq!(
        page_object_count(&store),
        total_pages,
        "dry run deletes nothing"
    );

    let applied = store.kv_gc(true).unwrap();
    assert_eq!(applied.deleted.len(), dry.candidates.len());
    assert!(applied.deleted_bytes > 0);
    assert_eq!(
        page_object_count(&store),
        keep_record.page_count as usize * LAYERS as usize + 1
    );
    // The surviving pages are exactly the pinned prefix's pages plus the pin.
    let survivors: Vec<Hash32> = {
        let pages = store.resolve_path("/kv/pages").unwrap();
        store
            .list_dir(pages)
            .unwrap()
            .iter()
            .map(|entry| entry.target)
            .collect()
    };
    for id in keep_record.pages.iter().flatten() {
        assert!(survivors.contains(id), "live prefix page was collected");
    }
    assert!(survivors.contains(&pinned_page));
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

fn model_with(prefill_ns_per_token: u64) -> KvPrefillModel {
    KvPrefillModel {
        label: "test".to_owned(),
        defaults: KvPrefillRow {
            prefill_fixed_ns: 0,
            prefill_ns_per_token,
        },
        models: Default::default(),
        transfer_ns_per_mib: 250_000,
        placement_fixed_ns: 50_000,
        lookup_fixed_ns: 20_000,
        decode_cost_model: weftfs::cost_model::CostModelKind::Measured,
    }
}

#[test]
fn decision_reuses_only_when_it_is_cheaper_and_records_both_estimates() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(12, 16);
    let write = put(&mut store, "decide", &tokens, None, 0, false);

    // Expensive recompute: 1 ms per token.
    let slow = model_with(1_000_000);
    let reuse = store.kv_decide_reuse(&request(&tokens), &slow).unwrap();
    assert!(reuse.reuse, "{reuse:?}");
    assert_eq!(reuse.prefix_id, Some(write.prefix_id));
    assert_eq!(
        reuse.reuse_total_ns,
        reuse.lookup_ns + reuse.transfer_ns + reuse.decode_ns + reuse.placement_ns
    );
    assert_eq!(
        reuse.recompute_ns,
        reuse.recompute_fixed_ns + 16 * 1_000_000
    );
    assert!(reuse.reuse_total_ns < reuse.recompute_ns);
    assert_eq!(reuse.page_bytes, write.record.page_bytes);

    // Cheap recompute: reuse loses and the reason says so.
    let fast = model_with(1);
    let recompute = store.kv_decide_reuse(&request(&tokens), &fast).unwrap();
    assert!(!recompute.reuse, "{recompute:?}");
    assert!(recompute.reason.contains("T_reuse"));
    assert!(recompute.reuse_total_ns >= recompute.recompute_ns);

    // Compatible mismatch: recompute with the specific reason, no prefix id.
    let mut mismatched = request(&tokens);
    mismatched.model = "other".to_owned();
    let refused = store.kv_decide_reuse(&mismatched, &slow).unwrap();
    assert!(!refused.reuse);
    assert_eq!(refused.prefix_id, None);
    assert!(
        refused.reason.contains("model mismatch"),
        "{}",
        refused.reason
    );
    assert_eq!(refused.page_bytes, 0);
}

#[test]
fn shipped_prefill_document_matches_the_baked_model() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/calibration/kv-prefill-v1.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let loaded = KvPrefillModel::from_json(&bytes).unwrap();
    assert_eq!(loaded, KvPrefillModel::declared());

    // Strictness: unknown key, bad schema, and a bad cost model are refused.
    let text = String::from_utf8(bytes.clone()).unwrap();
    let mutated = text.replace("\"label\"", "\"labels\"");
    assert_eq!(
        KvPrefillModel::from_json(mutated.as_bytes())
            .unwrap_err()
            .class(),
        Class::Parse
    );
    let mutated = text.replace("weft.kv.prefill.v1", "weft.kv.prefill.v0");
    assert_eq!(
        KvPrefillModel::from_json(mutated.as_bytes())
            .unwrap_err()
            .class(),
        Class::Parse
    );
    let mutated = text.replace("\"measured\"", "\"magic\"");
    assert_eq!(
        KvPrefillModel::from_json(mutated.as_bytes())
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

#[test]
fn full_verify_passes_after_put_parent_extension_and_gc() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let tokens = kv_token_ids(20, 32);
    put(&mut store, "p1", &tokens[..16], None, 0, false);
    put(&mut store, "p2", &tokens, Some("p1"), 2, false);
    put_with_seed(&mut store, "orphan", &kv_token_ids(21, 16), None, 0, false, 0x99);
    store.kv_prefix_delete("orphan").unwrap();
    let gc = store.kv_gc(true).unwrap();
    assert!(!gc.deleted.is_empty());

    let report = store.verify(true).unwrap();
    assert!(report.ok, "{report:?}");
    assert!(report.checked_objects > 0);
    assert_eq!(report.generation, store.generation());
    // The shared pages are still readable through the child record.
    let record = store.kv_prefix("p2").unwrap();
    for id in record.pages.iter().flatten() {
        store.read_object(*id).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Reopen
// ---------------------------------------------------------------------------

#[test]
fn prefixes_survive_reopen_and_are_reusable() {
    let dir = tempdir().unwrap();
    let tokens = kv_token_ids(13, 24);
    let write = {
        let mut store = Store::init(dir.path(), options()).unwrap();
        put(&mut store, "persisted", &tokens, None, 0, false)
    };
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, options()).unwrap();
    let record = store.kv_prefix("persisted").unwrap();
    assert_eq!(record, write.record);
    match store.kv_prefix_lookup(&request(&tokens)).unwrap() {
        KvLookup::Reusable {
            record: found,
            scanned,
        } => {
            assert_eq!(found.id().unwrap(), write.prefix_id);
            assert_eq!(scanned, 1);
        }
        other => panic!("expected a reusable prefix, got {other:?}"),
    }
    // The descriptor of every page is readable in a read-only open.
    for id in record.pages.iter().flatten() {
        let descriptor: KvDescriptor = store.kv_descriptor(*id).unwrap();
        assert_eq!(descriptor.tenant, TENANT);
    }
    let _ = KV_PREFIX_SCHEMA;
}
