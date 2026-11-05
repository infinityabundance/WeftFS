//! D2 cross-tenant isolation court.
//!
//! Two tenants share one store. Some content is identical (so the chunk store
//! deduplicates it) and some is tenant-specific. Isolation is enforced by
//! verified capability envelopes layered on top of POSIX permissions:
//!
//! * tenant A cannot resolve, exact-id (`stat`/read), or enumerate tenant B's
//!   objects, and the denial class/message is identical whether B's objects
//!   exist or not;
//! * a shared, deduplicated content root does not reveal B's names, counts, or
//!   existence: A already holds authorized content with that content id, so
//!   using the content id returns A's own object. Deduplication across tenants
//!   is therefore allowed exactly when each tenant is independently authorized
//!   for that content root. No claim is made about timing channels;
//!   `WEFT-CAPABILITIES.md` §6.6 and §9.7 state that explicitly.
//!
//! Work accounting and budget outcomes are computed over the authorized
//! universe only. The fixture is built twice with different B populations
//! (0 vs 5 extra objects) to prove A's work units and budget outcomes do not
//! depend on the other tenant.

use std::collections::{BTreeMap, BTreeSet};

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope, Rejection,
};
use weftcodec::context::{Budget, ContextRoot};
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::store::{Options, SetAttr, Store};

const T0: i64 = 1_700_000_000_000_000_000;
const EXPIRES: i64 = 2_000_000_000_000_000_000;
const A_UID: u64 = 1001;
const B_UID: u64 = 1002;

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

fn hash(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

struct Fixture {
    a_files: Vec<Hash32>,
    b_files: Vec<Hash32>,
    shared_a: Hash32,
    shared_b: Hash32,
    b_only_content: Hash32,
    a_env: CapabilityEnvelope,
    b_env: CapabilityEnvelope,
}

fn make_envelope(
    tenant: &str,
    grantee_byte: u8,
    root: Hash32,
    revocation_byte: u8,
    nonce: u8,
) -> CapabilityEnvelope {
    CapabilityEnvelope::new(
        public_key_for_seed(&seed(7)).unwrap(),
        hash(grantee_byte),
        tenant,
        vec![root],
        Vec::new(),
        Vec::new(),
        Some(EXPIRES),
        None,
        hash(revocation_byte),
        T0,
        hash(nonce),
    )
    .unwrap()
}

fn attach(store: &mut Store, envelope: &CapabilityEnvelope) -> Hash32 {
    let blob = sign_envelope(envelope, &seed(7)).unwrap();
    store.save_capability_envelope(&blob).unwrap()
}

/// Build a store with a fixed A tenant and a variable B population.
fn build(dir: &std::path::Path, b_extra: usize) -> (Store, Fixture) {
    let mut store = Store::init(dir, opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    // Executable and readable so both tenants can traverse and resolve; it
    // holds no tenant attributes, so it never matches a tenant selector.
    store
        .setattr(
            root,
            SetAttr {
                mode: Some(0o755),
                ..SetAttr::default()
            },
        )
        .unwrap();

    let a_dir = store
        .mkdir_seeded(root, b"a", 0o700, A_UID, A_UID, &seed(0xA0))
        .unwrap();
    let b_dir = store
        .mkdir_seeded(root, b"b", 0o700, B_UID, B_UID, &seed(0xB0))
        .unwrap();

    let shared_payload = b"shared-tenant-payload";
    let shared_a = store
        .create_file_seeded(
            a_dir,
            b"shared.txt",
            shared_payload,
            0o600,
            A_UID,
            A_UID,
            attrs(&[("tenant", "a"), ("project", "demo")]),
            &seed(0xA1),
        )
        .unwrap();
    let a_only = store
        .create_file_seeded(
            a_dir,
            b"only-a.txt",
            b"only-a-payload",
            0o600,
            A_UID,
            A_UID,
            attrs(&[("tenant", "a"), ("project", "demo")]),
            &seed(0xA2),
        )
        .unwrap();

    let shared_b = store
        .create_file_seeded(
            b_dir,
            b"shared.txt",
            shared_payload,
            0o600,
            B_UID,
            B_UID,
            attrs(&[("tenant", "b"), ("project", "demo")]),
            &seed(0xB1),
        )
        .unwrap();
    let b_only = store
        .create_file_seeded(
            b_dir,
            b"only-b.txt",
            b"only-b-payload",
            0o600,
            B_UID,
            B_UID,
            attrs(&[("tenant", "b"), ("project", "demo")]),
            &seed(0xB2),
        )
        .unwrap();
    let mut b_files = vec![shared_b, b_only];
    let mut b_only_content = store.object_meta(b_only).unwrap().content;
    for i in 0..b_extra {
        let payload = format!("tenant-b-extra-payload-{i}");
        let id = store
            .create_file_seeded(
                b_dir,
                format!("extra-{i}.txt").as_bytes(),
                payload.as_bytes(),
                0o600,
                B_UID,
                B_UID,
                attrs(&[("tenant", "b"), ("project", "demo")]),
                format!("cap-isolation-b-extra-{i}").as_bytes(),
            )
            .unwrap();
        if i == 0 {
            b_only_content = store.object_meta(id).unwrap().content;
        }
        b_files.push(id);
    }

    let snapshot = store.snapshot_create("s1").unwrap();
    let a_selector = selector::parse("select attrs.tenant == \"a\"").unwrap();
    let b_selector = selector::parse("select attrs.tenant == \"b\"").unwrap();
    let a_root = ContextRoot::new(
        &a_selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    let b_root = ContextRoot::new(
        &b_selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("a_view", &a_root, None, false)
        .unwrap();
    store
        .save_context_root("b_view", &b_root, None, false)
        .unwrap();
    let a_view = a_root.id().unwrap();
    let b_view = b_root.id().unwrap();

    let a_env = make_envelope("tenant-a", 0xA5, a_view, 0xE1, 0x11);
    let b_env = make_envelope("tenant-b", 0xB5, b_view, 0xE2, 0x12);
    attach(&mut store, &a_env);
    attach(&mut store, &b_env);

    let fixture = Fixture {
        a_files: vec![shared_a, a_only],
        b_files,
        shared_a,
        shared_b,
        b_only_content,
        a_env,
        b_env,
    };
    (store, fixture)
}

fn principal_for(envelope: &CapabilityEnvelope, uid: u64, now: i64) -> Principal {
    let blob = sign_envelope(envelope, &seed(7)).unwrap().encode().unwrap();
    let verified = verify_envelope(&blob, &[], now, None).unwrap();
    Principal::new(uid, uid, Vec::new()).with_envelope(verified)
}

fn resolve(
    store: &Store,
    name: &str,
    principal: &Principal,
    now: i64,
) -> weftfs::Result<weftfs::context::ContextResolution> {
    store.resolve_context(name, None, principal, &weftcodec::Limits::default(), now)
}

fn object_set(resolved: &weftfs::context::ContextResolution) -> BTreeSet<Hash32> {
    resolved
        .resolution
        .results
        .iter()
        .map(|item| item.object_id)
        .collect()
}

#[test]
fn tenant_a_cannot_resolve_stat_or_enumerate_tenant_b() {
    let dir = tempdir().unwrap();
    let (store, fixture) = build(dir.path(), 3);
    let a = principal_for(&fixture.a_env, A_UID, T0);
    let b = principal_for(&fixture.b_env, B_UID, T0);

    // A's own view is complete and contains only A's files. The resolver
    // enumerates the authorized universe; no B object id can appear.
    let a_view = resolve(&store, "a_view", &a, T0).unwrap();
    assert_eq!(
        object_set(&a_view),
        fixture.a_files.iter().copied().collect::<BTreeSet<_>>()
    );
    for id in &fixture.b_files {
        assert!(!object_set(&a_view).contains(id));
    }

    // A cannot resolve B's view: the root grant does not include it.
    let err = resolve(&store, "b_view", &a, T0).unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert!(!err.message().contains("b_view"));

    // B can resolve B's own view.
    let b_view = resolve(&store, "b_view", &b, T0).unwrap();
    assert_eq!(
        object_set(&b_view),
        fixture.b_files.iter().copied().collect::<BTreeSet<_>>()
    );

    // Exact-id access (the `stat`/read gate): A cannot stat/read any B object.
    for id in &fixture.b_files {
        let err = store.exact_id_object(*id, None, &a, T0).unwrap_err();
        assert_eq!(err.class(), Class::NotFound);
        assert_eq!(err.message(), "no such object");
    }
    // A can stat its own object.
    store
        .exact_id_object(fixture.shared_a, None, &a, T0)
        .unwrap();
}

#[test]
fn deduplicated_shared_content_does_not_leak_tenant_b() {
    let dir = tempdir().unwrap();
    let (store, fixture) = build(dir.path(), 2);
    let a = principal_for(&fixture.a_env, A_UID, T0);

    let a_meta = store.object_meta(fixture.shared_a).unwrap();
    let b_meta = store.object_meta(fixture.shared_b).unwrap();
    // The chunk store really deduplicated the identical payload.
    assert_eq!(a_meta.content, b_meta.content);
    assert_ne!(a_meta.id, b_meta.id);

    // A is independently authorized for that content root through its own
    // object; addressing the shared ContentId returns A's object, never B's.
    let record = store.exact_id_object(a_meta.content, None, &a, T0).unwrap();
    assert_eq!(record.id, fixture.shared_a);
    assert_ne!(record.id, fixture.shared_b);

    // A content root that only B holds is not authorized: the dedup store
    // cannot be used as an existence or enumeration oracle.
    let err = store
        .exact_id_object(fixture.b_only_content, None, &a, T0)
        .unwrap_err();
    assert_eq!(err.class(), Class::NotFound);
    assert_eq!(err.message(), "no such object");

    // B's ObjectId is also not addressable by A even though its bytes are
    // shared with an A object.
    let err = store
        .exact_id_object(fixture.shared_b, None, &a, T0)
        .unwrap_err();
    assert_eq!(err.class(), Class::NotFound);
    assert_eq!(err.message(), "no such object");
}

#[test]
fn work_units_and_budgets_are_independent_of_the_other_tenant() {
    // Same A tenant, different B populations, same envelopes and A content.
    let dir_small = tempdir().unwrap();
    let dir_large = tempdir().unwrap();
    let (small, small_fixture) = build(dir_small.path(), 0);
    let (large, large_fixture) = build(dir_large.path(), 7);
    let a_small = principal_for(&small_fixture.a_env, A_UID, T0);
    let a_large = principal_for(&large_fixture.a_env, A_UID, T0);

    assert!(
        large.current_state().objects.len() > small.current_state().objects.len(),
        "the large store must hold strictly more objects"
    );

    let small_view = resolve(&small, "a_view", &a_small, T0).unwrap();
    let large_view = resolve(&large, "a_view", &a_large, T0).unwrap();
    assert_eq!(small_view.work_units, large_view.work_units);
    assert_eq!(
        small_view.resolution.matched_count,
        large_view.resolution.matched_count
    );
    assert_eq!(object_set(&small_view), object_set(&large_view));

    // The same denials appear in both stores, with identical text.
    let small_err = resolve(&small, "b_view", &a_small, T0).unwrap_err();
    let large_err = resolve(&large, "b_view", &a_large, T0).unwrap_err();
    assert_eq!(small_err.class(), large_err.class());
    assert_eq!(small_err.message(), large_err.message());
    assert_eq!(small_err.class(), Class::Unauthorized);

    // A B ObjectId that exists only in the large store denies exactly like one
    // that does not exist at all.
    let missing = hash(0xFE);
    let small_err = small
        .exact_id_object(missing, None, &a_small, T0)
        .unwrap_err();
    let large_err = large
        .exact_id_object(large_fixture.b_files[0], None, &a_large, T0)
        .unwrap_err();
    assert_eq!(small_err.class(), large_err.class());
    assert_eq!(small_err.message(), large_err.message());

    // Budget outcomes use the authorized work count, so a bound below it fails
    // identically in both stores and a bound at it succeeds identically.
    let work = small_view.work_units;
    assert!(work > 0);
    for (label, max_work, expect_ok) in [("below", work - 1, false), ("at", work, true)] {
        let budget = Some(Budget {
            max_work: Some(max_work),
            max_results: None,
            max_bytes: None,
            max_depth: None,
        });
        let mut small_env = small_fixture.a_env.clone();
        small_env.budget = budget.clone();
        // Re-sign because the envelope bytes changed.
        let small_blob = sign_envelope(&small_env, &seed(7)).unwrap();
        let small_verified = verify_envelope(&small_blob.encode().unwrap(), &[], T0, None).unwrap();
        let small_principal =
            Principal::new(A_UID, A_UID, Vec::new()).with_envelope(small_verified);
        let mut large_env = large_fixture.a_env.clone();
        large_env.budget = budget;
        let large_blob = sign_envelope(&large_env, &seed(7)).unwrap();
        let large_verified = verify_envelope(&large_blob.encode().unwrap(), &[], T0, None).unwrap();
        let large_principal =
            Principal::new(A_UID, A_UID, Vec::new()).with_envelope(large_verified);

        let small_result = resolve(&small, "a_view", &small_principal, T0);
        let large_result = resolve(&large, "a_view", &large_principal, T0);
        assert_eq!(small_result.is_ok(), expect_ok, "budget {label} in small");
        assert_eq!(large_result.is_ok(), expect_ok, "budget {label} in large");
        if !expect_ok {
            let small_err = small_result.unwrap_err();
            let large_err = large_result.unwrap_err();
            assert_eq!(small_err.class(), Class::LimitExceeded);
            assert_eq!(large_err.class(), Class::LimitExceeded);
            assert_eq!(small_err.message(), "resolver work budget exceeded");
            assert_eq!(small_err.message(), large_err.message());
        }
    }
}

#[test]
fn revoked_or_expired_envelope_fails_closed_without_hash_revival() {
    let dir = tempdir().unwrap();
    let (mut store, fixture) = build(dir.path(), 1);
    let a_id = attach(&mut store, &fixture.a_env);
    assert_eq!(a_id, fixture.a_env.id().unwrap());
    let blob = sign_envelope(&fixture.a_env, &seed(7)).unwrap();
    let bytes = blob.encode().unwrap();
    let original_blob_id = blob.id().unwrap();

    // Revocation: the same stored bytes and the same EnvelopeId stay revoked.
    for _ in 0..2 {
        let err = store
            .load_capability_envelope(a_id, &[fixture.a_env.revocation_id], T0)
            .unwrap_err();
        assert_eq!(err.class(), Class::Unauthorized);
        let err = verify_envelope(&bytes, &[fixture.a_env.revocation_id], T0, None).unwrap_err();
        assert_eq!(err.rejection(), Rejection::Revoked);
    }
    // The blob is unchanged; reusing its hash does not revive it.
    let stored = store
        .list_capability_envelopes_at(store.current_state())
        .unwrap()
        .into_iter()
        .find(|item| item.envelope_id == a_id)
        .unwrap();
    assert_eq!(stored.blob_id, original_blob_id);

    // Expiry is inclusive and monotonic for the same bytes.
    let expired = CapabilityEnvelope::new(
        fixture.a_env.grantor,
        fixture.a_env.grantee,
        fixture.a_env.tenant.clone(),
        fixture.a_env.roots.clone(),
        fixture.a_env.namespaces.clone(),
        fixture.a_env.predicates.clone(),
        Some(T0 + 10),
        None,
        fixture.a_env.revocation_id,
        T0,
        hash(0x77),
    )
    .unwrap();
    let expired_blob = sign_envelope(&expired, &seed(7)).unwrap();
    let expired_bytes = expired_blob.encode().unwrap();
    assert!(verify_envelope(&expired_bytes, &[], T0 + 10, None).is_ok());
    for _ in 0..2 {
        let err = verify_envelope(&expired_bytes, &[], T0 + 11, None).unwrap_err();
        assert_eq!(err.rejection(), Rejection::Expired);
    }

    // An expired envelope attached to the store remains rejected by the loader;
    // only a newly signed, newly identified envelope can grant again.
    let expired_id = store.save_capability_envelope(&expired_blob).unwrap();
    let err = store
        .load_capability_envelope(expired_id, &[], T0 + 11)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert_eq!(expired_id, expired.id().unwrap());
    assert_ne!(expired_id, a_id);
}
