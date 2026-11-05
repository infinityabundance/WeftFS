//! D2 authenticated capability envelope tests: canonical form, offline
//! verification, the reject matrix, store attachment, enforcement, GC, and
//! `verify`.
//!
//! These tests complement `tests/context.rs` (unsigned capability scopes) and
//! must not weaken them. Enforcement is always *in addition to* POSIX/ACL.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope, Rejection,
};
use weftcodec::context::{Budget, CapabilityScope, ContextRoot};
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::context::CapabilityContext;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};

const T0: i64 = 1_700_000_000_000_000_000;
const EXPIRES: i64 = 2_000_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(T0),
        ..Options::default()
    }
}

fn seed(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn key(byte: u8) -> Hash32 {
    public_key_for_seed(&seed(byte)).unwrap()
}

fn hash(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    tenant: &str,
    grantee: Hash32,
    roots: Vec<Hash32>,
    namespaces: Vec<String>,
    predicates: Vec<String>,
    expires: Option<i64>,
    budget: Option<Budget>,
    revocation_byte: u8,
    nonce_byte: u8,
) -> CapabilityEnvelope {
    CapabilityEnvelope::new(
        key(7),
        grantee,
        tenant,
        roots,
        namespaces,
        predicates,
        expires,
        budget,
        hash(revocation_byte),
        T0,
        hash(nonce_byte),
    )
    .unwrap()
}

fn signed(envelope: &CapabilityEnvelope) -> Vec<u8> {
    sign_envelope(envelope, &seed(7)).unwrap().encode().unwrap()
}

fn verified(
    envelope: &CapabilityEnvelope,
    revocations: &[Hash32],
    now: i64,
) -> weftcodec::capability::VerifiedEnvelope {
    verify_envelope(&signed(envelope), revocations, now, None).unwrap()
}

fn sample_store() -> (tempfile::TempDir, Store, Hash32, Hash32) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let file = store
        .create_file(
            root,
            b"a.txt",
            b"alpha",
            0o644,
            0,
            0,
            attrs(&[("project", "demo"), ("tenant", "a")]),
        )
        .unwrap();
    let snapshot = store.snapshot_create("s1").unwrap();
    let selector = selector::parse("select attrs.project == \"demo\"").unwrap();
    let context = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("ctx", &context, None, false)
        .unwrap();
    let context_id = context.id().unwrap();
    (dir, store, file, context_id)
}

#[test]
fn envelope_round_trip_and_store_attachment() {
    let (_dir, mut store, _file, _context) = sample_store();
    let envelope = envelope(
        "tenant-a",
        hash(0x42),
        Vec::new(),
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let blob = sign_envelope(&envelope, &seed(7)).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    assert_eq!(id, envelope.id().unwrap());

    let stored = store
        .list_capability_envelopes_at(store.current_state())
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].envelope_id, id);
    assert_eq!(stored[0].grantee, hash(0x42));
    assert_eq!(stored[0].tenant, "tenant-a");

    let loaded = store.load_capability_envelope(id, &[], T0).unwrap();
    assert_eq!(loaded.envelope, envelope);
    assert_eq!(loaded.envelope_id, id);

    // Idempotent: a second save publishes nothing.
    let generation = store.generation();
    let again = store.save_capability_envelope(&blob).unwrap();
    assert_eq!(again, id);
    assert_eq!(store.generation(), generation);

    // GC retains the blob while the attachment relation exists, and `verify`
    // checks the signature and canonical form.
    store.gc(true).unwrap();
    store
        .load_capability_envelope(id, &[], T0)
        .expect("envelope blob must survive GC");
    let report = store.verify(true).unwrap();
    assert!(report.ok);
    assert_eq!(report.checked_capability_envelopes, 1);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[test]
fn reject_matrix_is_typed_and_stable() {
    let envelope = envelope(
        "tenant-a",
        hash(0x42),
        Vec::new(),
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let blob = sign_envelope(&envelope, &seed(7)).unwrap();
    let bytes = blob.encode().unwrap();

    // Bad signature.
    let mut bad = blob.clone();
    bad.signature[0] ^= 0xff;
    let err = verify_envelope(&bad.encode().unwrap(), &[], T0, None).unwrap_err();
    assert_eq!(err.rejection(), Rejection::BadSignature);

    // Wrong grantee.
    let err = verify_envelope(&bytes, &[], T0, Some(hash(0xAA))).unwrap_err();
    assert_eq!(err.rejection(), Rejection::WrongGrantee);

    // Expired (inclusive boundary: valid at EXPIRES, invalid one ns later).
    assert!(verify_envelope(&bytes, &[], EXPIRES, None).is_ok());
    let err = verify_envelope(&bytes, &[], EXPIRES + 1, None).unwrap_err();
    assert_eq!(err.rejection(), Rejection::Expired);

    // Revoked.
    let err = verify_envelope(&bytes, &[envelope.revocation_id], T0, None).unwrap_err();
    assert_eq!(err.rejection(), Rejection::Revoked);

    // Non-canonical outer blob.
    let mut noncanonical = bytes.clone();
    noncanonical.push(0);
    let err = verify_envelope(&noncanonical, &[], T0, None).unwrap_err();
    assert_eq!(err.rejection(), Rejection::NonCanonical);

    // Non-canonical inner grant order reaches the same typed rejection.
    let mut unordered = envelope.clone();
    unordered.namespaces = vec!["zz".to_owned(), "aa".to_owned()];
    let err = unordered.canonical_bytes().unwrap_err();
    assert_eq!(err.class().as_str(), "NonCanonical");
}

#[test]
fn envelope_narrows_posix_and_intersects_explicit_capability() {
    let (_dir, mut store, file, context_id) = sample_store();
    let envelope = envelope(
        "tenant-a",
        hash(0x42),
        vec![context_id],
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let blob = sign_envelope(&envelope, &seed(7)).unwrap();
    store.save_capability_envelope(&blob).unwrap();
    let principal = Principal::root().with_envelope(verified(&envelope, &[], T0));

    // The envelope grants both the namespace and the root, so resolution works.
    let resolved = store
        .resolve_context("ctx", None, &principal, &weftcodec::Limits::default(), T0)
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);
    assert_eq!(resolved.resolution.results[0].object_id, file);

    // Exact-id access under the envelope is allowed for the same object.
    store
        .exact_id_object(file, None, &principal, T0)
        .expect("envelope must authorize the object it grants");

    // An explicit capability that shares the namespace but restricts to a
    // disjoint root fails closed: both grants must allow the operation.
    let other_root = hash(0x99);
    let explicit = CapabilityContext::new(
        CapabilityScope::new(
            "agent",
            vec!["default".to_owned()],
            vec![other_root],
            Vec::new(),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let err = store
        .resolve_context(
            "ctx",
            Some(&explicit),
            &principal,
            &weftcodec::Limits::default(),
            T0,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // Expiry is enforced at the operation clock even though the envelope was
    // verified earlier.
    let err = store
        .resolve_context(
            "ctx",
            None,
            &principal,
            &weftcodec::Limits::default(),
            EXPIRES + 1,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert_eq!(err.message(), "capability is not valid");

    // A wrong namespace grant denies resolution with the same class.
    let other_ns = CapabilityContext::new(
        CapabilityScope::new(
            "agent",
            vec!["other".to_owned()],
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let err = store
        .resolve_context(
            "ctx",
            Some(&other_ns),
            &principal,
            &weftcodec::Limits::default(),
            T0,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert!(!err.message().contains("ctx"));
}

#[test]
fn revoked_or_expired_envelope_cannot_be_attached_or_used() {
    let (_dir, mut store, file, _context_id) = sample_store();
    let envelope = envelope(
        "tenant-a",
        hash(0x42),
        Vec::new(),
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let blob = sign_envelope(&envelope, &seed(7)).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();

    // Revocation is an explicit caller-supplied set. The same stored bytes
    // (same hash) cannot be revived by re-reading them.
    let err = store
        .load_capability_envelope(id, &[envelope.revocation_id], T0)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    let err = store
        .load_capability_envelope(id, &[envelope.revocation_id], T0 - 1)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // Expiry is likewise enforced by the loader.
    let err = store
        .load_capability_envelope(id, &[], EXPIRES + 1)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // A forged signature is refused at attachment time.
    let mut forged = blob.clone();
    forged.signature[7] ^= 0xff;
    let err = store.save_capability_envelope(&forged).unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);

    // The legitimate envelope still authorizes the object under POSIX.
    let principal = Principal::root().with_envelope(verified(&envelope, &[], T0));
    store.exact_id_object(file, None, &principal, T0).unwrap();
}

#[test]
fn workspace_grant_alone_does_not_authorize_an_unrelated_root() {
    let (_dir, mut store, _file, context_id) = sample_store();
    // Grant a root that is not this Context Root, and no namespace.
    let envelope = envelope(
        "tenant-a",
        hash(0x42),
        vec![hash(0x88)],
        Vec::new(),
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let blob = sign_envelope(&envelope, &seed(7)).unwrap();
    store.save_capability_envelope(&blob).unwrap();
    let principal = Principal::root().with_envelope(verified(&envelope, &[], T0));
    let err = store
        .resolve_context("ctx", None, &principal, &weftcodec::Limits::default(), T0)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert!(!err.message().contains(&context_id.to_hex()));
}

#[test]
fn relation_selector_requires_the_envelope_predicate_grant() {
    let (_dir, mut store, file, _context_id) = sample_store();
    let root = store.resolve_path("/").unwrap();
    store
        .relation_add(
            root,
            "derived_from",
            RelationTarget::Object(file),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    let snapshot = store.snapshot_create("s2").unwrap();
    let selector = selector::parse(&format!(
        "select relation(derived_from, obj:{})",
        file.to_hex()
    ))
    .unwrap();
    let context = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("relation", &context, None, false)
        .unwrap();

    // No predicate grant: the relation selector is a non-disclosing denial.
    let no_predicates = envelope(
        "tenant-a",
        hash(0x42),
        Vec::new(),
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES),
        None,
        0x22,
        0x33,
    );
    let principal = Principal::root().with_envelope(verified(&no_predicates, &[], T0));
    let err = store
        .resolve_context(
            "relation",
            None,
            &principal,
            &weftcodec::Limits::default(),
            T0,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // With the predicate granted, the same selector resolves.
    let with_predicates = envelope(
        "tenant-a",
        hash(0x42),
        Vec::new(),
        vec!["default".to_owned()],
        vec!["derived_from".to_owned()],
        Some(EXPIRES),
        None,
        0x23,
        0x34,
    );
    let principal = Principal::root().with_envelope(verified(&with_predicates, &[], T0));
    let resolved = store
        .resolve_context(
            "relation",
            None,
            &principal,
            &weftcodec::Limits::default(),
            T0,
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);
    assert_eq!(resolved.resolution.results[0].object_id, root);
}
