//! The KV fast path's decode caches must never become an authorization bypass.
//!
//! R27 added two bounded caches to the mode-19 read path: the validated
//! capability descriptor of an object record, and the decoded scope of a
//! verified capability envelope. Both hold *decode results*. This court proves
//! that the surrounding rules still decide every read:
//!
//! * an expired grant is refused on every read, cache or no cache;
//! * a principal whose grant does not name the descriptor id is refused, even
//!   when another principal's identical read is warm in the cache;
//! * a `VerifiedEnvelope` whose id fields claim another grant's identity cannot
//!   borrow that grant's decoded scope (the cache hit is proved against the
//!   envelope itself, not against the claimed ids);
//! * a write fences the descriptor cache, and a rewritten object is authorised
//!   by its own descriptor, not by the name it occupies.
//!
//! Every test prints `COURT ...` lines with the values it observed.

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{KvAttention, KvLayout, KvPositional, KvProvenanceInput};

use weftfs::identity::Hash32;
use weftfs::kv_capability::KV_DENIED;
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-cache";
const GRANTOR_SEED: [u8; 32] = [7u8; 32];

fn root(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

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

fn provenance(salt: u8) -> KvProvenanceInput {
    KvProvenanceInput::new(
        root(0x11),
        "qwen-bpe",
        151_936,
        KvAttention {
            head_count: 4,
            head_dim: 64,
            sliding_window: 0,
            rope_theta_bits: 10_000.0f64.to_bits(),
            rope_scale_bits: 1.0f64.to_bits(),
        },
        KvPositional::Rope,
        KvLayout {
            dtype: KvDtype::Bf16,
            page_rows: 8,
            layer_count: 24,
        },
        root(salt),
    )
}

fn grantee(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

/// Grant `roots` to `grantee_byte`, expiring at `expires_ns`, through the
/// store's own envelope publishing path.
fn grant(
    store: &mut Store,
    grantee_byte: u8,
    roots: Vec<Hash32>,
    expires_ns: i64,
) -> VerifiedEnvelope {
    grant_issued(store, grantee_byte, roots, expires_ns, NOW_NS)
}

/// The same grant with an explicit issue time, so an already-expired envelope
/// is expressible (`expires_at_ns` must not precede `issued_at_ns`).
fn grant_issued(
    store: &mut Store,
    grantee_byte: u8,
    roots: Vec<Hash32>,
    expires_ns: i64,
    issued_ns: i64,
) -> VerifiedEnvelope {
    let mut roots = roots;
    roots.sort();
    roots.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        grantee(grantee_byte),
        TENANT,
        roots,
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(expires_ns),
        None,
        root(0xA1),
        issued_ns,
        root(0xA2),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let envelope_id = store.save_capability_envelope(&blob).unwrap();
    store
        .load_capability_envelope(envelope_id, &[], issued_ns)
        .unwrap()
}

fn principal_with(verified: VerifiedEnvelope) -> Principal {
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

fn attempt(store: &Store, object: Hash32, principal: &Principal) -> (usize, String) {
    match store.read_kv_prov_page(object, principal, NOW_NS) {
        Ok(page) => (page.bytes.len(), "ok".to_owned()),
        Err(error) => {
            assert_eq!(
                error.message(),
                KV_DENIED,
                "every pre-byte denial is the same text"
            );
            (0, error.class().as_str().to_owned())
        }
    }
}

fn court(line: &str) {
    println!("COURT {line}");
}

struct Fixture {
    store: Store,
    object: Hash32,
    descriptor_id: Hash32,
    elements: Vec<u8>,
}

fn fixture() -> (tempfile::TempDir, Fixture) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root_id = store.resolve_path("/").unwrap();
    let elements = page_bytes(0x5EED_1234);
    let written = store
        .write_kv_prov_page(
            root_id,
            b"page.kv",
            TENANT,
            &page_input(),
            &elements,
            &provenance(0xA0),
            1000,
            1000,
        )
        .unwrap();
    (
        dir,
        Fixture {
            store,
            object: written.object_id,
            descriptor_id: written.descriptor_id,
            elements,
        },
    )
}

#[test]
fn descriptor_decode_is_cached_across_reads() {
    let (_dir, mut fixture) = fixture();
    let verified = grant(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        EXPIRES_NS,
    );
    let principal = principal_with(verified);
    assert_eq!(fixture.store.kv_read_cache_descriptors(), 0);
    let (first, outcome) = attempt(&fixture.store, fixture.object, &principal);
    assert_eq!(outcome, "ok");
    assert_eq!(first, fixture.elements.len());
    let after_first = fixture.store.kv_read_cache_descriptors();
    assert_eq!(after_first, 1, "one record, one cached descriptor");
    let (second, outcome) = attempt(&fixture.store, fixture.object, &principal);
    assert_eq!(outcome, "ok");
    assert_eq!(second, first);
    assert_eq!(
        fixture.store.kv_read_cache_descriptors(),
        after_first,
        "the second read must not grow the cache"
    );
    court(&format!(
        "cache descriptor_entries={} reads=2 emitted={} exact={}",
        after_first,
        second,
        second == fixture.elements.len()
    ));
}

#[test]
fn an_expired_grant_is_refused_on_every_read() {
    let (_dir, mut fixture) = fixture();
    let verified = grant_issued(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        NOW_NS - 1,
        NOW_NS - 10,
    );
    let principal = principal_with(verified);
    for attempt_index in 1..=3 {
        let (emitted, outcome) = attempt(&fixture.store, fixture.object, &principal);
        assert_eq!(outcome, "Unauthorized", "read {attempt_index}");
        assert_eq!(emitted, 0, "read {attempt_index}");
    }
    court("expired grant reads=3 emitted=0 class=Unauthorized");
}

#[test]
fn a_second_grant_cannot_borrow_the_first_grants_decode() {
    let (_dir, mut fixture) = fixture();
    let allowed = principal_with(grant(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        EXPIRES_NS,
    ));
    // Same tenant and namespace, same object, but its roots name something else.
    let denied = principal_with(grant(
        &mut fixture.store,
        b'B',
        vec![root(0xC3)],
        EXPIRES_NS,
    ));
    // Interleave, so the allowed grant's decode is warm when the denied grant
    // asks. Caching a decision rather than a decode would show up here.
    for round in 1..=3 {
        let (emitted, outcome) = attempt(&fixture.store, fixture.object, &allowed);
        assert_eq!(outcome, "ok", "round {round}");
        assert_eq!(emitted, fixture.elements.len());
        let (emitted, outcome) = attempt(&fixture.store, fixture.object, &denied);
        assert_eq!(outcome, "Unauthorized", "round {round}");
        assert_eq!(emitted, 0, "round {round}");
    }
    court(&format!(
        "cache capability_entries={} interleaved_rounds=3 allowed=ok denied=Unauthorized",
        fixture.store.kv_read_cache_capabilities()
    ));
}

#[test]
fn a_forged_envelope_identity_cannot_borrow_another_grants_decode() {
    let (_dir, mut fixture) = fixture();
    let allowed = grant(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        EXPIRES_NS,
    );
    // Warm the cache with the broad grant's decode.
    let (emitted, outcome) = attempt(
        &fixture.store,
        fixture.object,
        &principal_with(allowed.clone()),
    );
    assert_eq!(outcome, "ok");
    assert_eq!(emitted, fixture.elements.len());

    // A narrow grant that names nothing, presented with the broad grant's
    // identity fields. The cache must not answer for it, because the decoded
    // scope is only reused when the envelope itself is equal.
    let narrow = grant(&mut fixture.store, b'B', Vec::new(), EXPIRES_NS);
    let forged = VerifiedEnvelope {
        envelope: narrow.envelope.clone(),
        envelope_id: allowed.envelope_id,
        blob_id: allowed.blob_id,
        signature: narrow.signature.clone(),
    };
    let (emitted, outcome) = attempt(&fixture.store, fixture.object, &principal_with(forged));
    assert_eq!(outcome, "Unauthorized");
    assert_eq!(emitted, 0);
    court(&format!(
        "cache capability_entries={} forged_envelope_id emitted={} class={}",
        fixture.store.kv_read_cache_capabilities(),
        emitted,
        outcome
    ));
}

#[test]
fn a_write_fences_the_descriptor_cache() {
    let (_dir, mut fixture) = fixture();
    let principal = principal_with(grant(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        EXPIRES_NS,
    ));
    let (_, outcome) = attempt(&fixture.store, fixture.object, &principal);
    assert_eq!(outcome, "ok");
    assert_eq!(fixture.store.kv_read_cache_descriptors(), 1);
    let root_id = fixture.store.resolve_path("/").unwrap();
    fixture
        .store
        .write_kv_prov_page(
            root_id,
            b"other.kv",
            TENANT,
            &page_input(),
            &page_bytes(0x1111_2222),
            &provenance(0xB0),
            1000,
            1000,
        )
        .unwrap();
    assert_eq!(
        fixture.store.kv_read_cache_descriptors(),
        0,
        "a new head generation drops every descriptor decoded under the old one"
    );
    // The read still works: the cache is a fast path, never a prerequisite.
    let (emitted, outcome) = attempt(&fixture.store, fixture.object, &principal);
    assert_eq!(outcome, "ok");
    assert_eq!(emitted, fixture.elements.len());
    court(&format!(
        "cache descriptor_entries_after_commit={} reread={}",
        fixture.store.kv_read_cache_descriptors(),
        outcome
    ));
}

#[test]
fn a_rewritten_object_is_authorised_by_its_own_descriptor() {
    let (_dir, mut fixture) = fixture();
    let old_grant = principal_with(grant(
        &mut fixture.store,
        b'A',
        vec![fixture.descriptor_id],
        EXPIRES_NS,
    ));
    let root_id = fixture.store.resolve_path("/").unwrap();
    // The store never overwrites a name in place, so a rewrite is unlink+create.
    fixture.store.unlink(root_id, b"page.kv").unwrap();
    let replacement = page_bytes(0x9999_8888);
    let written = fixture
        .store
        .write_kv_prov_page(
            root_id,
            b"page.kv",
            TENANT,
            &page_input(),
            &replacement,
            &provenance(0xC0),
            1000,
            1000,
        )
        .unwrap();
    assert_ne!(written.descriptor_id, fixture.descriptor_id);
    let (emitted, outcome) = attempt(&fixture.store, written.object_id, &old_grant);
    assert_eq!(
        outcome, "Unauthorized",
        "the old grant names the old identity"
    );
    assert_eq!(emitted, 0);
    let new_grant = principal_with(grant(
        &mut fixture.store,
        b'A',
        vec![written.descriptor_id],
        EXPIRES_NS,
    ));
    let (emitted, outcome) = attempt(&fixture.store, written.object_id, &new_grant);
    assert_eq!(outcome, "ok");
    assert_eq!(emitted, replacement.len());
    court(&format!(
        "rewrite old_grant={} new_grant={} emitted={}",
        "Unauthorized", outcome, emitted
    ));
}
