//! Adversarial court for provenance-bound KV caches (mode 19).
//!
//! The claim under test is structural, not statistical:
//!
//! * two KV pages with byte-identical element bytes but different provenance
//!   have different identity roots, and a resolver asked under the other's
//!   scope emits zero bytes;
//! * the sharing-domain salt makes the roots of otherwise identical pages
//!   differ (identifier unlinkability only; nothing here claims timing
//!   privacy);
//! * an adapter set is a canonical set: `{A,B}` and `{B,A}` are one identity,
//!   `{A}` and `{A,B}` are not;
//! * a forged descriptor dimension root is rejected by the schema/derived-root
//!   checks, and a swapped payload is rejected by the payload hash;
//! * a `KV_CACHE_PROV` object written through the store is readable only under a
//!   capability whose `roots` name the provenance-bound descriptor id, and the
//!   mode-12 reader cannot decode it.
//!
//! Every test prints `COURT ...` lines with the raw values it observed so the
//! court log is the receipt's primary evidence.

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::frame::{encode_frame, Frame, FrameParts, Mode, DOMAIN_PAYLOAD};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{
    decode_kv_prov_page, encode_kv_prov_page, KvAttention, KvLayout, KvPositional,
    KvProvDescriptor, KvProvenanceInput,
};
use weftcodec::limits::Limits;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::kv_capability::{KvPageMode, KV_DENIED};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const GRANTOR_SEED: [u8; 32] = [7u8; 32];

fn limits() -> Limits {
    Limits::default()
}

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

fn page_input() -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: "session-42".to_owned(),
        page_index: 0,
        page_count: 1,
        seq_start: 0,
        seq_end: 8,
        dtype: KvDtype::Bf16,
        head_count: 4,
        head_dim: 64,
    }
}

/// Deterministic element bytes: every BF16 bit pattern is legal, including the
/// sign/exponent edges. Identical across every provenance variant.
fn page_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * 4 * 64 * 2);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..(8 * 4 * 64) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

fn provenance(adapters: &[u8], salt: u8) -> KvProvenanceInput {
    let mut provenance = KvProvenanceInput::new(
        root(0x11),
        "qwen-bpe",
        151_936,
        attention(),
        KvPositional::Rope,
        layout(),
        root(salt),
    );
    for adapter in adapters {
        provenance = provenance.with_adapter(root(*adapter));
    }
    provenance
}

fn court(line: &str) {
    println!("COURT {line}");
}

// ---------------------------------------------------------------------------
// 1. Collision: identical bytes, different provenance
// ---------------------------------------------------------------------------

#[test]
fn collision_two_adapter_roots_are_two_identities() {
    let elements = page_bytes();
    let frame_a = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x21], 0xA0),
        &limits(),
    )
    .expect("frame a");
    let frame_b = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x22], 0xA0),
        &limits(),
    )
    .expect("frame b");
    let page_a =
        decode_kv_prov_page(&Frame::parse(&frame_a, &limits()).unwrap(), &limits()).unwrap();
    let page_b =
        decode_kv_prov_page(&Frame::parse(&frame_b, &limits()).unwrap(), &limits()).unwrap();

    assert_eq!(page_a.bytes, page_b.bytes, "payloads must be identical");
    assert_eq!(
        page_a.descriptor.payload_hash, page_b.descriptor.payload_hash,
        "payload hashes must be identical"
    );
    assert_ne!(page_a.root, page_b.root, "provenance must change the root");
    court(&format!(
        "collision payload_bytes={} payload_hash={} root_a={} root_b={} distinct=true",
        page_a.bytes.len(),
        page_a.descriptor.payload_hash.to_hex(),
        page_a.root.to_hex(),
        page_b.root.to_hex()
    ));

    // A resolver that names A's root must refuse B with zero bytes, and vice
    // versa. The refusal is a typed error.
    let (emitted, outcome) = resolve_scoped(&frame_b, page_a.root);
    assert_eq!(emitted, 0, "cross-scope read must emit zero bytes");
    assert_eq!(
        outcome, "Unauthorized",
        "cross-scope read must be a typed error"
    );
    court(&format!(
        "collision cross_resolve scope={} frame_root={} emitted_bytes={} class={}",
        page_a.root.to_hex(),
        page_b.root.to_hex(),
        emitted,
        outcome
    ));
    let (emitted, outcome) = resolve_scoped(&frame_a, page_a.root);
    assert_eq!(emitted, elements.len());
    court(&format!(
        "collision matching_resolve scope={} emitted_bytes={} class={}",
        page_a.root.to_hex(),
        emitted,
        outcome
    ));
}

/// Models a resolver that is asked for a page under a scope naming one root.
/// It emits nothing unless the recomputed root matches the scope.
fn resolve_scoped(frame_bytes: &[u8], scope: Hash32) -> (usize, &'static str) {
    let Ok(parsed) = Frame::parse(frame_bytes, &limits()) else {
        return (0, "Malformed");
    };
    let Ok(page) = decode_kv_prov_page(&parsed, &limits()) else {
        return (0, "Corrupt");
    };
    if page.root != scope {
        return (0, "Unauthorized");
    }
    (page.bytes.len(), "ok")
}

// ---------------------------------------------------------------------------
// 2. Sharing-domain salt
// ---------------------------------------------------------------------------

#[test]
fn salt_makes_identical_pages_unlinkable_by_root() {
    let elements = page_bytes();
    let frame_a =
        encode_kv_prov_page(&page_input(), &elements, &provenance(&[], 0xB0), &limits()).unwrap();
    let frame_b =
        encode_kv_prov_page(&page_input(), &elements, &provenance(&[], 0xB1), &limits()).unwrap();
    let page_a =
        decode_kv_prov_page(&Frame::parse(&frame_a, &limits()).unwrap(), &limits()).unwrap();
    let page_b =
        decode_kv_prov_page(&Frame::parse(&frame_b, &limits()).unwrap(), &limits()).unwrap();
    assert_eq!(page_a.bytes, page_b.bytes);
    assert_eq!(
        page_a.descriptor.payload_hash,
        page_b.descriptor.payload_hash
    );
    assert_ne!(page_a.root, page_b.root);
    court(&format!(
        "salt salt_a={} salt_b={} root_a={} root_b={} payload_hash_equal=true roots_distinct=true",
        page_a.descriptor.provenance.salt.to_hex(),
        page_b.descriptor.provenance.salt.to_hex(),
        page_a.root.to_hex(),
        page_b.root.to_hex()
    ));
    court("salt proven=roots_differ not_proven=timing_or_access_pattern_privacy");
}

// ---------------------------------------------------------------------------
// 3. Adapter-set ordering
// ---------------------------------------------------------------------------

#[test]
fn adapter_set_is_canonical() {
    let elements = page_bytes();
    let ab = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x21, 0x22], 0xA0),
        &limits(),
    )
    .unwrap();
    let ba = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x22, 0x21], 0xA0),
        &limits(),
    )
    .unwrap();
    let only_a = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x21], 0xA0),
        &limits(),
    )
    .unwrap();
    assert_eq!(ab, ba, "{{A,B}} and {{B,A}} must be one frame");
    assert_ne!(ab, only_a, "{{A}} and {{A,B}} must differ");
    let page_ab = decode_kv_prov_page(&Frame::parse(&ab, &limits()).unwrap(), &limits()).unwrap();
    let page_a =
        decode_kv_prov_page(&Frame::parse(&only_a, &limits()).unwrap(), &limits()).unwrap();
    court(&format!(
        "adapter_set ab_equals_ba=true ab_equals_a=false set_root_ab={} set_root_a={} root_ab={} root_a={}",
        page_ab.descriptor.provenance.adapter_set_root.to_hex(),
        page_a.descriptor.provenance.adapter_set_root.to_hex(),
        page_ab.root.to_hex(),
        page_a.root.to_hex()
    ));
}

// ---------------------------------------------------------------------------
// 4. Forgery
// ---------------------------------------------------------------------------

#[test]
fn forgery_is_rejected() {
    let elements = page_bytes();
    let frame = encode_kv_prov_page(
        &page_input(),
        &elements,
        &provenance(&[0x21], 0xA0),
        &limits(),
    )
    .unwrap();
    let parsed = Frame::parse(&frame, &limits()).unwrap();
    let decoded = decode_kv_prov_page(&parsed, &limits()).unwrap();

    // (a) recomputed descriptor with a tampered dimension root.
    let mut forged = decoded.descriptor.clone();
    forged.provenance.adapter_set_root = root(0x5A);
    let encoded = forged.canonical_bytes(&limits()).unwrap();
    let error = KvProvDescriptor::from_canonical_bytes(&encoded, &limits())
        .expect_err("tampered adapter_set_root");
    assert_eq!(error.class(), weftcodec::ErrorClass::Malformed);
    court(&format!(
        "forgery dimension_root tampered=adapter_set_root class={}",
        error.class().as_str()
    ));

    let mut forged = decoded.descriptor.clone();
    forged.provenance.tokenizer_root = root(0x5B);
    let encoded = forged.canonical_bytes(&limits()).unwrap();
    let error = KvProvDescriptor::from_canonical_bytes(&encoded, &limits())
        .expect_err("tampered tokenizer_root");
    assert_eq!(error.class(), weftcodec::ErrorClass::Malformed);
    court(&format!(
        "forgery dimension_root tampered=tokenizer_root class={}",
        error.class().as_str()
    ));

    // (b) a payload swapped under an otherwise valid descriptor.
    let mut other = elements.clone();
    other[0] ^= 0xFF;
    let other_frame =
        encode_kv_prov_page(&page_input(), &other, &provenance(&[0x21], 0xA0), &limits()).unwrap();
    let other_parsed = Frame::parse(&other_frame, &limits()).unwrap();
    let body = parsed.body;
    let descriptor_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let descriptor_bytes = &body[4..4 + descriptor_len];
    let other_body = other_parsed.body;
    let other_len =
        u32::from_le_bytes([other_body[0], other_body[1], other_body[2], other_body[3]]) as usize;
    let other_payload = &other_body[4 + other_len..];
    let mut body = Vec::with_capacity(4 + descriptor_bytes.len() + other_payload.len());
    body.extend_from_slice(&(descriptor_bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(descriptor_bytes);
    body.extend_from_slice(other_payload);
    let swapped = encode_frame(
        &FrameParts {
            format_major: weftcodec::frame::FORMAT_MAJOR,
            format_minor: weftcodec::frame::FORMAT_MINOR,
            domain: DOMAIN_PAYLOAD,
            mode: Mode::KvCacheProv,
            flags: 0,
            level: 0,
            decoded_len: parsed.header.decoded_len,
            content_hash: parsed.header.content_hash,
            dependencies: &[],
            chunks: &[],
            body: &body,
        },
        &limits(),
    )
    .unwrap();
    let swapped = Frame::parse(&swapped, &limits()).unwrap();
    let error = decode_kv_prov_page(&swapped, &limits()).expect_err("swapped payload");
    assert_eq!(error.class(), weftcodec::ErrorClass::Corrupt);
    court(&format!(
        "forgery payload_swap class={}",
        error.class().as_str()
    ));
}

// ---------------------------------------------------------------------------
// 5 + 6. Store scope and fail-closed byte count
// ---------------------------------------------------------------------------

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

fn grantee() -> Hash32 {
    Hash32::from_bytes([9u8; 32])
}

fn grant(
    store: &mut Store,
    tenant: &str,
    namespace: &str,
    mut roots: Vec<Hash32>,
) -> VerifiedEnvelope {
    roots.sort();
    roots.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        grantee(),
        tenant,
        roots,
        vec![namespace.to_owned()],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        root(0xA1),
        NOW_NS,
        root(0xA2),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let envelope_id = store.save_capability_envelope(&blob).unwrap();
    store
        .load_capability_envelope(envelope_id, &[], NOW_NS)
        .unwrap()
}

fn principal_with(verified: VerifiedEnvelope) -> Principal {
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

/// Perform one store read and report `(emitted_bytes, class_or_ok)`.
///
/// `emitted_bytes` is the number of payload bytes the store handed back. A
/// failed call cannot return a page value at all, so a refusal is exactly zero
/// bytes; this is the measurement the court prints.
fn attempt(store: &Store, object: Hash32, principal: &Principal) -> (usize, String) {
    match store.read_kv_prov_page(object, principal, NOW_NS) {
        Ok(page) => (page.bytes.len(), "ok".to_owned()),
        Err(error) => (0, error.class().as_str().to_owned()),
    }
}

#[test]
fn store_scope_is_the_provenance_descriptor_id() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root_id = store.resolve_path("/").unwrap();
    let elements = page_bytes();
    let written = store
        .write_kv_prov_page(
            root_id,
            b"prov-page.kv",
            TENANT,
            &page_input(),
            &elements,
            &provenance(&[0x21], 0xA0),
            1000,
            1000,
        )
        .unwrap();
    assert_eq!(written.page_bytes, elements.len() as u64);
    assert_eq!(
        store.kv_page_mode(written.object_id).unwrap(),
        KvPageMode::Provenance
    );
    let stored = store.kv_prov_descriptor(written.object_id).unwrap();
    assert_eq!(stored.provenance_root, written.provenance_root);
    court(&format!(
        "store written object={} descriptor_id={} provenance_root={} mode=19",
        written.object_id.to_hex(),
        written.descriptor_id.to_hex(),
        written.provenance_root.to_hex()
    ));

    // Correct scope: a capability naming the provenance-bound descriptor id.
    let verified = grant(
        &mut store,
        TENANT,
        "kv.tenant-a",
        vec![written.descriptor_id],
    );
    let principal = principal_with(verified);
    let (emitted, outcome) = attempt(&store, written.object_id, &principal);
    assert_eq!(outcome, "ok");
    assert_eq!(emitted, elements.len());
    court(&format!(
        "store scope=descriptor_id emitted_bytes={} class=ok exact={}",
        emitted,
        emitted == elements.len()
    ));

    // The mode-12 reader must not silently decode a mode-19 page.
    let mode12 = store.read_kv_page(written.object_id, &principal, NOW_NS);
    assert!(mode12.is_err());
    let class = mode12.unwrap_err().class();
    assert_eq!(class, Class::Corrupt);
    court(&format!(
        "store scope=mode12_reader emitted_bytes=0 class={}",
        class.as_str()
    ));

    // A capability naming the *plain* (mode-12) descriptor id, computed over the
    // same page fields but without provenance, must be denied.
    let plain = weftfs::kv_capability::KvDescriptor::from_input(
        &page_input(),
        TENANT,
        elements.len() as u64,
    )
    .unwrap();
    let verified = grant(&mut store, TENANT, "kv.tenant-a", vec![plain.id().unwrap()]);
    let principal = principal_with(verified);
    let (emitted, outcome) = attempt(&store, written.object_id, &principal);
    assert_eq!(emitted, 0);
    assert_eq!(outcome, "Unauthorized");
    court(&format!(
        "store scope=plain_descriptor_id emitted_bytes={} class={}",
        emitted, outcome
    ));

    // A capability naming the ContentId / ObjectId but not the descriptor id.
    let content_id = weftfs::identity::content_hash(&store.read_object(written.object_id).unwrap());
    let verified = grant(
        &mut store,
        TENANT,
        "kv.tenant-a",
        vec![content_id, written.object_id],
    );
    let principal = principal_with(verified);
    let (emitted, outcome) = attempt(&store, written.object_id, &principal);
    assert_eq!(emitted, 0);
    assert_eq!(outcome, "Unauthorized");
    court(&format!(
        "store scope=content_and_object_id emitted_bytes={} class={}",
        emitted, outcome
    ));

    // A capability for the same tenant naming another page's descriptor id.
    let other = store
        .write_kv_prov_page(
            root_id,
            b"prov-page-other.kv",
            TENANT,
            &page_input(),
            &elements,
            &provenance(&[0x22], 0xA0),
            1000,
            1000,
        )
        .unwrap();
    let verified = grant(&mut store, TENANT, "kv.tenant-a", vec![other.descriptor_id]);
    let principal = principal_with(verified);
    let (emitted, outcome) = attempt(&store, written.object_id, &principal);
    assert_eq!(emitted, 0);
    assert_eq!(outcome, "Unauthorized");
    court(&format!(
        "store scope=other_provenance_descriptor emitted_bytes={} class={}",
        emitted, outcome
    ));

    // No envelope at all.
    let anonymous = Principal::new(1001, 1001, Vec::new());
    let (emitted, outcome) = attempt(&store, written.object_id, &anonymous);
    assert_eq!(emitted, 0);
    assert_eq!(outcome, "Unauthorized");
    court(&format!(
        "store scope=none emitted_bytes={} class={}",
        emitted, outcome
    ));

    // The raw object is a frame, not plaintext, so a content-hash fetch cannot
    // yield a decode without the scope the store enforces.
    let raw = store.read_object(written.object_id).unwrap();
    assert_eq!(&raw[..4], b"WFT1");
    assert_ne!(raw, elements);
    court(&format!(
        "store raw_fetch bytes={} is_frame=true equals_plaintext=false",
        raw.len()
    ));

    // Two provenance descriptors for identical page bytes differ, and the
    // stored capability ids differ with them.
    assert_ne!(written.descriptor_id, other.descriptor_id);
    assert_ne!(written.provenance_root, other.provenance_root);
    court(&format!(
        "store identical_bytes_two_descriptors descriptor_a={} descriptor_b={} root_a={} root_b={}",
        written.descriptor_id.to_hex(),
        other.descriptor_id.to_hex(),
        written.provenance_root.to_hex(),
        other.provenance_root.to_hex()
    ));

    // The store's own verifier must accept mode-19 objects as ordinary files.
    let report = store.verify(true).expect("verify --full");
    assert!(report.ok, "verify failed: {:?}", report.problems);
    court(&format!(
        "store verify_full ok=true checked_objects={} checked_representations={} generation={}",
        report.checked_objects, report.checked_representations, report.generation
    ));

    // The provenance reader must not decode a plain mode-12 page either.
    let plain_page = store
        .write_kv_page(
            root_id,
            b"plain-page.kv",
            TENANT,
            &page_input(),
            &elements,
            1000,
            1000,
        )
        .unwrap();
    assert_eq!(
        store.kv_page_mode(plain_page.object_id).unwrap(),
        KvPageMode::Page12
    );
    let verified = grant(
        &mut store,
        TENANT,
        "kv.tenant-a",
        vec![plain_page.descriptor_id],
    );
    let principal = principal_with(verified);
    let error = store
        .read_kv_prov_page(plain_page.object_id, &principal, NOW_NS)
        .expect_err("mode-12 page under the provenance reader");
    assert_eq!(error.class(), Class::Corrupt);
    court(&format!(
        "store scope=prov_reader_on_mode12 emitted_bytes=0 class={}",
        error.class().as_str()
    ));

    // Keep the single-document constants honest: a plain mode-12 write does not
    // gain the mode-19 attributes, so every pre-R5 page stays readable.
    let plain_record = store.object_meta(plain_page.object_id).unwrap();
    assert!(!plain_record
        .attrs
        .contains_key(weftfs::kv_capability::ATTR_KV_MODE));
    let prov_record = store.object_meta(written.object_id).unwrap();
    assert!(prov_record
        .attrs
        .contains_key(weftfs::kv_capability::ATTR_KV_MODE));
    court(&format!(
        "store attributes plain_mode_attr=false prov_mode_attr=true plain_keys={} prov_keys={} denial_text={}",
        plain_record.attrs.len(),
        prov_record.attrs.len(),
        KV_DENIED
    ));
}
