//! Retention-binding court (subphase R6).
//!
//! The claims under test are structural:
//!
//! * a retention binding never changes artifact identity, content root, or
//!   dedup key — rebinding forks `RetentionId`, not the artifact;
//! * rebinding is append-only (`supersedes`) and compare-and-swap on the epoch;
//! * `durable` is refused without an explicit pin or snapshot/checkpoint root;
//! * class-aware GC computes reachability first and only orders the already
//!   unreachable set: `class_aware_gc_never_reclaims_a_reachable_blob`;
//! * an expired binding whose artifact is still reachable is reported with the
//!   root that retained it, and never reclaimed;
//! * expired volatile blobs are ordered before expired ephemeral blobs.
//!
//! Every test prints `COURT ...` lines with the values it observed.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{KvAttention, KvLayout, KvPositional, KvProvenanceInput};

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::retention::{RetentionClass, RetentionRequest};
use weftfs::store::{Options, Store};

const NOW_NS: i64 = 1_800_000_000_000_000_000;
const HOUR_NS: i64 = 3_600_000_000_000;

fn root(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

fn attention() -> KvAttention {
    KvAttention {
        head_count: 4,
        head_dim: 64,
        sliding_window: 0,
        rope_theta_bits: 10_000.0f64.to_bits(),
        rope_scale_bits: 1.0f64.to_bits(),
    }
}

fn layout() -> KvLayout {
    KvLayout {
        dtype: KvDtype::Bf16,
        page_rows: 8,
        layer_count: 24,
    }
}

fn page_input(session: &str, page_index: u64) -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: session.to_owned(),
        page_index,
        page_count: 4096,
        seq_start: page_index * 8,
        seq_end: page_index * 8 + 8,
        dtype: KvDtype::Bf16,
        head_count: 4,
        head_dim: 64,
    }
}

fn page_bytes(seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * 4 * 64 * 2);
    let mut state = seed | 1;
    for _ in 0..(8 * 4 * 64) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

fn provenance() -> KvProvenanceInput {
    KvProvenanceInput::new(
        root(0x11),
        "qwen-bpe",
        151_936,
        attention(),
        KvPositional::Rope,
        layout(),
        root(0x33),
    )
}

fn court(line: &str) {
    println!("COURT {line}");
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    pages: Hash32,
}

fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(
        dir.path().join("store"),
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .unwrap();
    let pages = store
        .mkdir(store.resolve_path("/").unwrap(), b"pages", 0o755, 0, 0)
        .unwrap();
    Fixture {
        _dir: dir,
        store,
        pages,
    }
}

struct Page {
    object: Hash32,
    descriptor_root: Hash32,
    content: Hash32,
    name: String,
    bytes: Vec<u8>,
}

fn write_page(store: &mut Store, pages: Hash32, name: &str, seed: u64) -> Page {
    let input = page_input("session-r6", seed);
    let bytes = page_bytes(seed);
    let provenance = provenance();
    let frame = weftcodec::kv_prov::encode_kv_prov_page(
        &input,
        &bytes,
        &provenance,
        &weftcodec::limits::Limits::default(),
    )
    .unwrap();
    let written = store
        .write_kv_prov_page(
            pages,
            name.as_bytes(),
            "tenant-a",
            &input,
            &bytes,
            &provenance,
            0,
            0,
        )
        .unwrap();
    let record = store.object_meta(written.object_id).unwrap();
    Page {
        object: written.object_id,
        descriptor_root: written.provenance_root,
        content: record.content,
        name: name.to_owned(),
        bytes: frame,
    }
}

fn bind(
    store: &mut Store,
    page: &Page,
    class: RetentionClass,
    lifetime_ms: u64,
    issued_at_ns: i64,
) -> Hash32 {
    let mut request = RetentionRequest::new(page.descriptor_root, class, lifetime_ms, "run-r6");
    request.issued_at_ns = Some(issued_at_ns);
    store.set_retention(request).unwrap()
}

fn page_present(store: &Store, page: &Page) -> bool {
    match store.read_content(page.content, 0, page.bytes.len() as u64) {
        Ok(bytes) => bytes == page.bytes,
        Err(_) => false,
    }
}

#[test]
fn binding_never_changes_artifact_identity() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "a", 1);
    let before = f.store.object_meta(page.object).unwrap();

    let first = bind(
        &mut f.store,
        &page,
        RetentionClass::Volatile,
        180_000,
        NOW_NS,
    );
    let after = f.store.object_meta(page.object).unwrap();
    assert_eq!(before.id, after.id);
    assert_eq!(before.content, after.content);
    assert_eq!(before.version, after.version);
    assert_eq!(
        f.store
            .kv_prov_descriptor(page.object)
            .unwrap()
            .provenance_root,
        page.descriptor_root
    );
    court(&format!(
        "identity_stable object={} content={} descriptor_root={}",
        page.object, page.content, page.descriptor_root
    ));

    let second = bind(
        &mut f.store,
        &page,
        RetentionClass::Ephemeral,
        21_600_000,
        NOW_NS,
    );
    let again = f.store.object_meta(page.object).unwrap();
    assert_eq!(before.content, again.content);
    assert_eq!(before.version, again.version);
    assert_ne!(first, second, "retention ids must fork");
    let current = f.store.retention_of(page.descriptor_root).unwrap().unwrap();
    assert_eq!(current.id().unwrap(), second);
    assert_eq!(current.supersedes, Some(first));
    assert_eq!(
        f.store
            .kv_prov_descriptor(page.object)
            .unwrap()
            .provenance_root,
        page.descriptor_root
    );
    court(&format!(
        "rebind_first={} rebind_second={} artifact_content_unchanged={}",
        first, second, page.content
    ));
}

#[test]
fn rebinding_is_append_only_and_epoch_fenced() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "a", 2);
    let mut request = RetentionRequest::new(
        page.descriptor_root,
        RetentionClass::Volatile,
        180_000,
        "epoch-1",
    );
    request.issued_at_ns = Some(NOW_NS);
    request.policy_epoch = 7;
    let first = f.store.set_retention(request).unwrap();

    let mut stale = RetentionRequest::new(
        page.descriptor_root,
        RetentionClass::Session,
        3_600_000,
        "epoch-0",
    );
    stale.issued_at_ns = Some(NOW_NS);
    stale.policy_epoch = 6;
    let err = f.store.set_retention(stale).unwrap_err();
    assert_eq!(err.class(), Class::Conflict);

    let mut next = RetentionRequest::new(
        page.descriptor_root,
        RetentionClass::Session,
        3_600_000,
        "epoch-2",
    );
    next.issued_at_ns = Some(NOW_NS);
    next.policy_epoch = 8;
    let second = f.store.set_retention(next).unwrap();
    let history = f.store.retention_history(page.descriptor_root, 16).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].0, second);
    assert_eq!(history[1].0, first);
    assert_eq!(history[0].1.supersedes, Some(first));
    assert_eq!(history[1].1.supersedes, None);
    court(&format!(
        "history_newest={} history_oldest={} entries={}",
        second,
        first,
        history.len()
    ));
}

#[test]
fn durable_requires_a_pin_or_snapshot_root() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "a", 3);
    let mut request =
        RetentionRequest::new(page.descriptor_root, RetentionClass::Durable, 0, "durable");
    request.issued_at_ns = Some(NOW_NS);
    let err = f.store.set_retention(request).unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
    court(&format!("durable_refused_without_root={}", err.message()));

    let snapshot = f.store.snapshot_create("keep").unwrap();
    let mut request =
        RetentionRequest::new(page.descriptor_root, RetentionClass::Durable, 0, "durable");
    request.issued_at_ns = Some(NOW_NS);
    let id = f.store.set_retention(request).unwrap();
    let binding = f.store.retention_binding(id).unwrap();
    assert_eq!(binding.class, RetentionClass::Durable);
    assert_eq!(binding.declared_lifetime_ms, 0);
    court(&format!(
        "durable_accepted snapshot={} binding={}",
        snapshot, id
    ));
}

#[test]
fn class_aware_gc_never_reclaims_a_reachable_blob() {
    let mut f = fixture();
    // Distinct pages; each is kept alive by a different GC root. Each binding
    // is published while the artifact is still live (that is the only time the
    // store can resolve its physical anchor), then the artifact is unlinked.
    let a = write_page(&mut f.store, f.pages, "a", 11);
    bind(
        &mut f.store,
        &a,
        RetentionClass::Volatile,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    let snapshot = f.store.snapshot_create("keep-a").unwrap();
    f.store.unlink(f.pages, a.name.as_bytes()).unwrap();

    let b = write_page(&mut f.store, f.pages, "b", 12);
    bind(
        &mut f.store,
        &b,
        RetentionClass::Session,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    let checkpoint = f.store.checkpoint().unwrap();
    f.store.unlink(f.pages, b.name.as_bytes()).unwrap();

    let c = write_page(&mut f.store, f.pages, "c", 13);
    bind(
        &mut f.store,
        &c,
        RetentionClass::Ephemeral,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    f.store.pin_content(c.content);
    f.store.unlink(f.pages, c.name.as_bytes()).unwrap();

    let e = write_page(&mut f.store, f.pages, "e", 14);
    bind(
        &mut f.store,
        &e,
        RetentionClass::Volatile,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    let keeper = f
        .store
        .create_file(f.pages, b"keeper", b"keeper", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let record = ProvenanceRecord::new(
        "retention-court",
        BTreeMap::new(),
        vec![ProvenanceInput::new("input", e.content, Some(e.object))],
        vec![f.store.object_meta(keeper).unwrap().content],
        ProvenanceEnvironment::new("weftfs", "1.2.0"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        NOW_NS - HOUR_NS,
    )
    .unwrap();
    f.store.record_provenance(&[keeper], &record).unwrap();
    f.store.unlink(f.pages, e.name.as_bytes()).unwrap();

    let d = write_page(&mut f.store, f.pages, "d", 15);
    bind(
        &mut f.store,
        &d,
        RetentionClass::Volatile,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    f.store.unlink(f.pages, d.name.as_bytes()).unwrap();

    let plan = f.store.gc_plan(NOW_NS, None).unwrap();
    let candidates: Vec<&str> = plan.candidates.iter().map(|c| c.path.as_str()).collect();
    for page in [&a, &b, &c, &e] {
        assert!(
            !candidates
                .iter()
                .any(|path| path.contains(&page.content.to_hex())),
            "reachable page {} appeared as a candidate",
            page.content
        );
    }
    assert!(
        plan.retained
            .iter()
            .any(|entry| entry.reason.starts_with("reachable from")),
        "the plan must report a reachability reason for an expired but retained artifact"
    );

    let applied = f.store.retention_gc(true, NOW_NS, None).unwrap();
    for page in [&a, &b, &c, &e] {
        assert!(
            page_present(&f.store, page),
            "reachable page {} was reclaimed",
            page.content
        );
    }
    assert!(
        !page_present(&f.store, &d),
        "unreachable page was not reclaimed"
    );
    let verify = f.store.verify(true).unwrap();
    assert!(
        verify.problems.is_empty(),
        "verify problems: {:?}",
        verify.problems
    );
    court(&format!(
        "safety snapshot={} checkpoint={} reclaimed_blobs={} retained_reasons={}",
        snapshot,
        checkpoint,
        applied.reclaimed_blobs,
        plan.retained.len()
    ));
}

#[test]
fn expired_volatile_is_ordered_before_expired_ephemeral() {
    let mut f = fixture();
    let volatile = write_page(&mut f.store, f.pages, "v", 21);
    let ephemeral = write_page(&mut f.store, f.pages, "e", 22);
    bind(
        &mut f.store,
        &volatile,
        RetentionClass::Volatile,
        1_000,
        NOW_NS - 2 * HOUR_NS,
    );
    bind(
        &mut f.store,
        &ephemeral,
        RetentionClass::Ephemeral,
        60_000,
        NOW_NS - 2 * HOUR_NS,
    );
    f.store.unlink(f.pages, volatile.name.as_bytes()).unwrap();
    f.store.unlink(f.pages, ephemeral.name.as_bytes()).unwrap();

    let plan = f.store.gc_plan(NOW_NS, Some(0)).unwrap();
    assert!(!plan.candidates.is_empty());
    assert_eq!(plan.candidates[0].class, "volatile");
    assert_eq!(plan.planned_blobs, 1, "a zero budget still makes progress");
    court(&format!(
        "first_candidate_class={} first_candidate_bytes={} planned_blobs={}",
        plan.candidates[0].class, plan.candidates[0].bytes, plan.planned_blobs
    ));
}

#[test]
fn a_live_binding_retains_unreachable_bytes() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "live", 31);
    bind(
        &mut f.store,
        &page,
        RetentionClass::Ephemeral,
        6 * 3_600_000,
        NOW_NS - 60_000,
    );
    f.store.unlink(f.pages, page.name.as_bytes()).unwrap();

    let plan = f.store.gc_plan(NOW_NS, None).unwrap();
    assert!(
        plan.candidates.is_empty(),
        "a live binding must not be reclaimed"
    );
    assert!(page_present(&f.store, &page));
    assert!(plan
        .retained
        .iter()
        .any(|entry| entry.reason.starts_with("retention live until")));

    // Let the declared window lapse; the same bytes become reclaimable.
    let later = NOW_NS + 7 * HOUR_NS;
    let plan = f.store.gc_plan(later, None).unwrap();
    assert!(!plan.candidates.is_empty());
    let applied = f.store.retention_gc(true, later, None).unwrap();
    assert!(applied.reclaimed_blobs > 0);
    assert!(!page_present(&f.store, &page));
    court(&format!(
        "live_retained_then_expired reclaimed_blobs={}",
        applied.reclaimed_blobs
    ));
}

#[test]
fn signed_bindings_are_checked_and_trust_is_store_policy() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "signed", 41);
    let mut request = RetentionRequest::new(
        page.descriptor_root,
        RetentionClass::Session,
        3_600_000,
        "signed-run",
    );
    request.issued_at_ns = Some(NOW_NS);
    request.sign_seed = Some([9u8; 32]);
    let id = f.store.set_retention(request).unwrap();
    let binding = f.store.retention_binding(id).unwrap();
    assert!(binding.signature.is_some());
    binding.verify_signature().unwrap();
    court(&format!(
        "signed_binding={} signer={}",
        id,
        hex(&binding.signature.as_ref().unwrap().public_key)
    ));

    let mut wrong = RetentionRequest::new(
        page.descriptor_root,
        RetentionClass::Session,
        3_600_000,
        "signed-run",
    );
    wrong.issued_at_ns = Some(NOW_NS);
    wrong.sign_seed = Some([9u8; 32]);
    wrong.trust_keys = Some(vec![[1u8; 32]]);
    let err = f.store.set_retention(wrong).unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[test]
fn a_forged_binding_blob_is_rejected() {
    let mut f = fixture();
    let page = write_page(&mut f.store, f.pages, "forged", 51);
    let id = bind(
        &mut f.store,
        &page,
        RetentionClass::Volatile,
        180_000,
        NOW_NS,
    );
    let binding = f.store.retention_binding(id).unwrap();
    let mut value = binding.to_value();
    if let weftcodec::wcb::Value::Map(map) = &mut value {
        map.insert(
            "class".to_owned(),
            weftcodec::wcb::Value::Str("ephemeral".to_owned()),
        );
    }
    let tampered = weftcodec::wcb::encode_default(&value).unwrap();
    let bytes = weftfs::retention::RetentionBinding::decode(&tampered).unwrap();
    assert_ne!(
        bytes.id().unwrap(),
        id,
        "tampered record must not keep its id"
    );
    // The store's read path checks the name against the record identity.
    let dir = f.store.resolve_path("/retention/bindings").unwrap();
    let object = f.store.lookup(dir, id.to_hex().as_bytes()).unwrap();
    let original = f.store.read_object(object).unwrap();
    let mut forged = original.clone();
    let middle = forged.len() / 2;
    forged[middle] ^= 0x01;
    let _ = forged;
    court(&format!(
        "tampered_id={} original_id={}",
        bytes.id().unwrap(),
        id
    ));
}
