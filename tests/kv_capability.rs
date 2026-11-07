//! Zero-trust KV-cache pages: an authenticated capability is the only way to
//! obtain bytes, and every pre-byte failure is the same non-disclosing denial.
//!
//! The page is a frozen mode-12 `KV_CACHE` frame; the authorization surface is
//! the canonical `weft.kv.capability.v1` descriptor (tenant, model version,
//! session, layer, page position, token range) plus an Ed25519-signed
//! `weft.capability.envelope.v1` whose `namespaces` contain `kv.<tenant>` and
//! whose `roots` contain this descriptor's id.

use std::collections::BTreeMap;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::kv_capability::{KvDescriptor, ATTR_KV_DESCRIPTOR, ATTR_KV_DESCRIPTOR_ID, KV_DENIED};
use weftfs::store::{Options, SetAttr, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
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

fn grantee() -> Hash32 {
    Hash32::from_bytes([9u8; 32])
}

fn revocation(index: u8) -> Hash32 {
    Hash32::from_bytes([index; 32])
}

fn page_input(rows: u64) -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: "session-42".to_owned(),
        page_index: 0,
        page_count: 1,
        seq_start: 0,
        seq_end: rows,
        dtype: KvDtype::Bf16,
        head_count: 4,
        head_dim: 64,
    }
}

/// Deterministic page bytes: every BF16 bit pattern is legal, including the
/// sign/exponent edges.
fn page_bytes(rows: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity((rows * 4 * 64 * 2) as usize);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..(rows * 4 * 64) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

struct Fixture {
    store: Store,
    root: Hash32,
    object: Hash32,
    descriptor_id: Hash32,
    page: Vec<u8>,
}

fn fixture(dir: &Path, rows: u64) -> Fixture {
    let mut store = Store::init(dir, options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let input = page_input(rows);
    let page = page_bytes(rows);
    let written = store
        .write_kv_page(root, b"page-0.kv", TENANT, &input, &page, 1000, 1000)
        .unwrap();
    assert_eq!(written.page_bytes, page.len() as u64);
    Fixture {
        store,
        root,
        object: written.object_id,
        descriptor_id: written.descriptor_id,
        page,
    }
}

/// Mint, sign, persist and load an authenticated grant.
///
/// `tenant`, `namespace`, `roots` and `expires` are the fields the read path
/// actually consults; the caller chooses them so a test can get exactly one
/// component wrong.
fn grant(
    store: &mut Store,
    tenant: &str,
    namespace: &str,
    roots: Vec<Hash32>,
    expires: Option<i64>,
    revocation_index: u8,
) -> (Hash32, weftcodec::capability::VerifiedEnvelope) {
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        grantee(),
        tenant,
        roots,
        vec![namespace.to_owned()],
        Vec::new(),
        expires,
        None,
        revocation(revocation_index),
        NOW_NS,
        revocation(0xA1),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let envelope_id = store.save_capability_envelope(&blob).unwrap();
    // Prove the stored blob round-trips through the ordinary verification path.
    let verified = store
        .load_capability_envelope(envelope_id, &[], NOW_NS)
        .unwrap();
    (envelope_id, verified)
}

fn principal_with(verified: weftcodec::capability::VerifiedEnvelope) -> Principal {
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

fn assert_denied(err: &weftfs::Error) {
    assert_eq!(err.class(), Class::Unauthorized, "class: {err}");
    assert_eq!(err.message(), KV_DENIED, "text: {err}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn authorized_reader_gets_exact_bytes() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (_, verified) = grant(
        &mut fx.store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![fx.descriptor_id],
        Some(EXPIRES_NS),
        0x01,
    );
    let principal = principal_with(verified);
    let decoded = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap();
    assert_eq!(decoded.bytes, fx.page, "the page must be byte-exact");
    assert_eq!(decoded.meta.model, page_input(8).model);
    assert_eq!(decoded.meta.session, "session-42");
    assert_eq!((decoded.meta.seq_start, decoded.meta.seq_end), (0, 8));
    assert_eq!(decoded.meta.layer, 3);
    assert_eq!(decoded.meta.shape, vec![8, 4, 64]);

    // The descriptor is what the grant names, and it round-trips.
    let descriptor = fx.store.kv_descriptor(fx.object).unwrap();
    assert_eq!(descriptor.id().unwrap(), fx.descriptor_id);
    assert_eq!(descriptor.namespace(), format!("kv.{TENANT}"));
    assert_eq!(descriptor.page_bytes, fx.page.len() as u64);
}

#[test]
fn an_absent_envelope_is_denied() {
    let dir = tempdir().unwrap();
    let fx = fixture(dir.path(), 8);
    let principal = Principal::new(1001, 1001, Vec::new());
    let err = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap_err();
    assert_denied(&err);
}

#[test]
fn a_foreign_tenant_grant_is_denied() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (_, verified) = grant(
        &mut fx.store,
        "tenant-b",
        "kv.tenant-b",
        vec![fx.descriptor_id],
        Some(EXPIRES_NS),
        0x02,
    );
    let principal = principal_with(verified);
    let err = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap_err();
    assert_denied(&err);
}

#[test]
fn a_grant_that_does_not_name_the_descriptor_is_denied() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    // Right tenant and namespace, wrong artifact.
    let (_, verified) = grant(
        &mut fx.store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![Hash32::from_bytes([0x44; 32])],
        Some(EXPIRES_NS),
        0x03,
    );
    let principal = principal_with(verified);
    let err = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap_err();
    assert_denied(&err);
}

#[test]
fn a_grant_without_the_tenant_namespace_is_denied() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (_, verified) = grant(
        &mut fx.store,
        TENANT,
        "kv.other",
        vec![fx.descriptor_id],
        Some(EXPIRES_NS),
        0x04,
    );
    let principal = principal_with(verified);
    let err = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap_err();
    assert_denied(&err);
}

#[test]
fn an_expired_grant_is_denied_with_the_same_text() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (_, verified) = grant(
        &mut fx.store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![fx.descriptor_id],
        Some(NOW_NS + 1),
        0x05,
    );
    let principal = principal_with(verified);
    // One nanosecond after the inclusive expiry.
    let err = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS + 2)
        .unwrap_err();
    assert_denied(&err);
    // One nanosecond before it still works, so the failure is expiry and not
    // something else in the same envelope.
    assert!(fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS + 1)
        .is_ok());
}

#[test]
fn a_revoked_envelope_cannot_be_loaded() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (envelope_id, _) = grant(
        &mut fx.store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![fx.descriptor_id],
        Some(EXPIRES_NS),
        0x06,
    );
    let err = fx
        .store
        .load_capability_envelope(envelope_id, &[revocation(0x06)], NOW_NS)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
}

#[test]
fn a_content_hash_is_not_a_credential() {
    let dir = tempdir().unwrap();
    let fx = fixture(dir.path(), 8);
    // The attacker knows everything public about the object.
    let record = fx.store.object_meta(fx.object).unwrap();
    let content_id = record.content;
    let content_digest = fx.store.manifest_of(content_id).unwrap().content_hash;
    assert_ne!(content_id, Hash32::ZERO);
    assert_ne!(content_digest, Hash32::ZERO);

    // Knowing both identities authorizes nothing.
    let anonymous = Principal::new(1001, 1001, Vec::new());
    let err = fx
        .store
        .read_kv_page(fx.object, &anonymous, NOW_NS)
        .unwrap_err();
    assert_denied(&err);

    // A grant for a *different* descriptor is no better.
    let mut store_with_grant = fx.store;
    let (_, verified) = grant(
        &mut store_with_grant,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![content_id],
        Some(EXPIRES_NS),
        0x07,
    );
    let err = store_with_grant
        .read_kv_page(fx.object, &principal_with(verified), NOW_NS)
        .unwrap_err();
    assert_denied(&err);
}

#[test]
fn absence_and_denial_are_indistinguishable() {
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let fx = fixture(dir_a.path(), 8);
    let empty = Store::init(dir_b.path(), options()).unwrap();
    let principal = Principal::new(1001, 1001, Vec::new());
    let missing = empty
        .read_kv_page(Hash32::from_bytes([0x5A; 32]), &principal, NOW_NS)
        .unwrap_err();
    let denied = fx
        .store
        .read_kv_page(fx.object, &principal, NOW_NS)
        .unwrap_err();
    assert_denied(&missing);
    assert_denied(&denied);
    assert_eq!(missing.class_name(), denied.class_name());
    assert_eq!(missing.message(), denied.message());
    assert_eq!(format!("{missing}"), format!("{denied}"));
}

#[test]
fn a_tampered_descriptor_fails_closed_before_bytes() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    // A well-formed mode-12 frame with attributes that do not agree with each
    // other: the descriptor bytes say one thing, the recorded id another.
    let input = page_input(8);
    let page = page_bytes(8);
    let descriptor = KvDescriptor::from_input(&input, TENANT, page.len() as u64).unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert(
        ATTR_KV_DESCRIPTOR.to_owned(),
        weftcodec::wcb::Value::Str(hex::encode(descriptor.canonical_bytes().unwrap())),
    );
    attrs.insert(
        ATTR_KV_DESCRIPTOR_ID.to_owned(),
        weftcodec::wcb::Value::Str(Hash32::from_bytes([0x77; 32]).to_hex()),
    );
    let frame =
        weftcodec::kv_cache::encode_kv_page(&input, &page, &weftcodec::Limits::default()).unwrap();
    let object = store
        .create_file(root, b"tampered.kv", &frame, 0o644, 1000, 1000, attrs)
        .unwrap();
    // Even a grant that names the honest descriptor id cannot see bytes.
    let (_, verified) = grant(
        &mut store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![descriptor.id().unwrap()],
        Some(EXPIRES_NS),
        0x08,
    );
    let err = store
        .read_kv_page(object, &principal_with(verified), NOW_NS)
        .unwrap_err();
    assert_denied(&err);
    assert!(store.kv_descriptor(object).is_err());
}

#[test]
fn descriptor_disagreement_after_authorization_is_corrupt() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    // The descriptor claims a longer token range than the frame carries. The
    // descriptor and its recorded id agree with each other, so a grant can name
    // it; the mismatch is discovered only after authorization.
    let input = page_input(8);
    let page = page_bytes(8);
    let mut lying = KvDescriptor::from_input(&input, TENANT, page.len() as u64).unwrap();
    lying.seq_end = 16;
    lying.shape = vec![16, 4, 64];
    let mut attrs = BTreeMap::new();
    attrs.insert(
        ATTR_KV_DESCRIPTOR.to_owned(),
        weftcodec::wcb::Value::Str(hex::encode(lying.canonical_bytes().unwrap())),
    );
    attrs.insert(
        ATTR_KV_DESCRIPTOR_ID.to_owned(),
        weftcodec::wcb::Value::Str(lying.id().unwrap().to_hex()),
    );
    let frame =
        weftcodec::kv_cache::encode_kv_page(&input, &page, &weftcodec::Limits::default()).unwrap();
    let object = store
        .create_file(root, b"lying.kv", &frame, 0o644, 1000, 1000, attrs)
        .unwrap();
    let (_, verified) = grant(
        &mut store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![lying.id().unwrap()],
        Some(EXPIRES_NS),
        0x09,
    );
    let err = store
        .read_kv_page(object, &principal_with(verified), NOW_NS)
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
    assert!(
        err.message().contains("does not match its frame metadata"),
        "unexpected text: {err}"
    );
}

#[test]
fn a_page_is_not_readable_without_the_posix_bit_even_with_a_grant() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    // Write the page private to uid 0, then grant uid 1001 — the capability
    // cannot widen POSIX permissions.
    let input = page_input(8);
    let page = page_bytes(8);
    let written = store
        .write_kv_page(root, b"private.kv", TENANT, &input, &page, 0, 0)
        .unwrap();
    store
        .setattr(
            written.object_id,
            SetAttr {
                mode: Some(0o600),
                ..SetAttr::default()
            },
        )
        .expect("set mode");
    let (_, verified) = grant(
        &mut store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![written.descriptor_id],
        Some(EXPIRES_NS),
        0x0A,
    );
    let err = store
        .read_kv_page(written.object_id, &principal_with(verified), NOW_NS)
        .unwrap_err();
    assert_denied(&err);
    // The owner can still read it.
    let owner_grant = grant(
        &mut store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![written.descriptor_id],
        Some(EXPIRES_NS),
        0x0B,
    );
    let owner = Principal::new(0, 0, Vec::new()).with_envelope(owner_grant.1);
    assert_eq!(
        store
            .read_kv_page(written.object_id, &owner, NOW_NS)
            .unwrap()
            .bytes,
        page
    );
}

#[test]
fn the_stored_blob_is_a_canonical_envelope() {
    let dir = tempdir().unwrap();
    let mut fx = fixture(dir.path(), 8);
    let (envelope_id, verified) = grant(
        &mut fx.store,
        TENANT,
        &format!("kv.{TENANT}"),
        vec![fx.descriptor_id],
        Some(EXPIRES_NS),
        0x0C,
    );
    assert_eq!(verified.envelope_id, envelope_id);
    assert_eq!(verified.tenant(), TENANT);
    // Verification is offline and deterministic: the same blob verifies twice.
    let again = verify_envelope(
        &verified_blob(&mut fx.store, envelope_id),
        &[],
        NOW_NS,
        None,
    )
    .unwrap();
    assert_eq!(again.envelope_id, envelope_id);
}

fn verified_blob(store: &mut Store, envelope_id: Hash32) -> Vec<u8> {
    // Re-sign deterministically: the blob encoded from the stored envelope and
    // signature is canonical, so this is the same byte string the store holds.
    let verified = store
        .load_capability_envelope(envelope_id, &[], NOW_NS)
        .unwrap();
    weftcodec::capability::CapabilityBlob::new(verified.envelope.clone(), verified.signature)
        .unwrap()
        .encode()
        .unwrap()
}
