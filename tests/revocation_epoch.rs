//! T078: revocation epochs, stale-client refusal, tenant scoping, and
//! deny-before-decode.
//!
//! The court in this file proves the order of operations: a denied read fetches
//! no object bytes and decodes nothing from the target object, and a client
//! that observed an older epoch refuses instead of serving.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};

use weftfs::auth::Principal;
use weftfs::error::Class;
use weftfs::identity::{self, Hash32};
use weftfs::revocation_epoch::{
    RevocationEntry, RevocationKind, RevocationObservation, REVOCATION_DENIED,
};
use weftfs::state::ObjectKind;
use weftfs::store::{Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    id: Hash32,
    content: Hash32,
    bytes: Vec<u8>,
}

fn fixture(bytes: usize) -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let payload = random_bytes(bytes, 0x600d);
    let id = store
        .create_file_seeded(
            root,
            b"artifact.bin",
            &payload,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-artifact",
        )
        .unwrap();
    let content = store.object_meta(id).unwrap().content;
    Fixture {
        _dir: dir,
        store,
        id,
        content,
        bytes: payload,
    }
}

fn counters_after(store: &Store) -> (u64, u64, u64, u64) {
    let layers = store.read_layer_stats();
    let backend = store.backend_stats();
    (
        layers.decode_calls,
        layers.decode_bytes,
        backend.bytes_read,
        backend.reads + backend.range_reads,
    )
}

#[test]
fn epoch_records_chain_canonically_and_republishing_adds_an_epoch() {
    let mut f = fixture(1024);
    let first = f
        .store
        .publish_revocation_epoch(
            &[RevocationEntry::new("tenant-a", RevocationKind::Content, f.content).unwrap()],
            60_000_000_000,
        )
        .unwrap();
    assert_eq!(first.epoch, 1);
    assert_eq!(first.prev, None);
    assert!(first
        .revokes(&RevocationEntry::new("tenant-a", RevocationKind::Content, f.content).unwrap()));
    let bytes = first.canonical_bytes().unwrap();
    let decoded = weftfs::revocation_epoch::RevocationEpoch::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(decoded, first);
    assert_eq!(decoded.id().unwrap(), first.id().unwrap());

    let second = f
        .store
        .publish_revocation_epoch(
            &[RevocationEntry::new(
                "tenant-a",
                RevocationKind::Capability,
                Hash32::from_bytes([0x21; 32]),
            )
            .unwrap()],
            60_000_000_000,
        )
        .unwrap();
    assert_eq!(second.epoch, 2);
    assert_eq!(second.prev, Some(first.id().unwrap()));
    assert_eq!(second.revocations.len(), 2);
    let chain = f.store.verify_revocation_chain().unwrap().unwrap();
    assert_eq!(chain, second);

    // Non-canonical epochs are refused.
    let mut mutated = second.canonical_bytes().unwrap();
    let last = mutated.len() - 1;
    mutated[last] ^= 0x01;
    assert!(weftfs::revocation_epoch::RevocationEpoch::from_canonical_bytes(&mutated).is_err());
}

#[test]
fn a_stale_client_refuses_before_touching_the_target_object() {
    let mut f = fixture(1024 * 1024);
    // An epoch exists with an unrelated entry.
    f.store
        .publish_revocation_epoch(
            &[RevocationEntry::new(
                "other",
                RevocationKind::Content,
                Hash32::from_bytes([0x31; 32]),
            )
            .unwrap()],
            600_000_000_000,
        )
        .unwrap();
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    // The store advances while the client is away.
    f.store
        .publish_revocation_epoch(&[], 600_000_000_000)
        .unwrap();

    let before = counters_after(&f.store);
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    let after = counters_after(&f.store);
    assert_eq!(err.class(), Class::StaleEpoch);
    // The gate read the small epoch record, never the 1 MiB target.
    assert_eq!(after.0, before.0, "stale refusal decoded no object frames");
    assert!(
        after.1.saturating_sub(before.1) < 8192,
        "decoded bytes delta {} is too large for a control-record read",
        after.1.saturating_sub(before.1)
    );
    assert!(
        after.2.saturating_sub(before.2) < 8192,
        "backend bytes delta {} is too large for a control-record read",
        after.2.saturating_sub(before.2)
    );
}

#[test]
fn a_revoked_content_root_denies_without_bytes_and_without_existence_evidence() {
    let mut f = fixture(1024 * 1024);
    f.store
        .publish_revocation_epoch(
            &[RevocationEntry::new("tenant-a", RevocationKind::Content, f.content).unwrap()],
            600_000_000_000,
        )
        .unwrap();
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();

    let before = counters_after(&f.store);
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    let after = counters_after(&f.store);
    assert_eq!(err.class(), Class::CapabilityRevoked);
    assert_eq!(err.message(), REVOCATION_DENIED);
    assert_eq!(after.0, before.0);
    assert!(after.1.saturating_sub(before.1) < 8192);
    assert!(after.2.saturating_sub(before.2) < 8192);

    // An id that does not exist gets the ordinary absent answer, not the
    // revocation answer: authorization happens before the content check, so a
    // caller who may not read an object cannot use revocation as an existence
    // oracle. The ordinary answer is identical for every absent id.
    let absent = f
        .store
        .check_read_at_epoch(
            None,
            Hash32::from_bytes([0x77; 32]),
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(absent.class(), Class::NotFound);
    assert_ne!(absent.class(), err.class());
    let other_absent = f
        .store
        .check_read_at_epoch(
            None,
            Hash32::from_bytes([0x78; 32]),
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(other_absent.class(), absent.class());
    assert_eq!(other_absent.message(), absent.message());
    // The revocation denial itself carries no id, size or count.
    assert!(!err.message().contains(&f.content.to_hex()));
    assert!(!err.message().contains(&f.id.to_hex()));
}

#[test]
fn revocation_is_tenant_scoped_over_shared_deduplicated_content() {
    let mut f = fixture(4096);
    // One physical object content, two tenants.
    f.store
        .publish_revocation_epoch(
            &[RevocationEntry::new("tenant-a", RevocationKind::Content, f.content).unwrap()],
            600_000_000_000,
        )
        .unwrap();
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    let denied = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(denied.class(), Class::CapabilityRevoked);
    let allowed = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-b",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap();
    let bytes = f.store.read_object_authorized(&allowed).unwrap();
    assert_eq!(bytes, f.bytes);
}

#[test]
fn a_revocation_arriving_mid_session_refuses_the_old_observation() {
    let mut f = fixture(4096);
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    // No epoch was published yet (epoch 0); the read is authorized.
    let allowed = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(f.store.read_object_authorized(&allowed).unwrap(), f.bytes);

    f.store
        .publish_revocation_epoch(
            &[RevocationEntry::new("tenant-a", RevocationKind::Content, f.content).unwrap()],
            600_000_000_000,
        )
        .unwrap();
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::StaleEpoch);

    // Refreshing the observation yields the revocation.
    let refreshed = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &refreshed,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::CapabilityRevoked);
}

#[test]
fn an_observation_past_its_staleness_bound_refuses() {
    let f = fixture(1024);
    let observation = RevocationObservation::new(
        Hash32::ZERO,
        0,
        f.store.clock_now_ns() - 10_000_000_000,
        1_000_000_000,
    );
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::StaleEpoch);
}

#[test]
fn a_forged_or_rolled_back_record_is_corrupt() {
    let mut f = fixture(1024);
    let first = f
        .store
        .publish_revocation_epoch(&[], 600_000_000_000)
        .unwrap();
    let first_bytes = first.canonical_bytes().unwrap();
    f.store
        .publish_revocation_epoch(&[], 600_000_000_000)
        .unwrap();
    assert_eq!(f.store.verify_revocation_chain().unwrap().unwrap().epoch, 2);

    // Roll the current pointer back to epoch 1 while history still holds 2.
    let current = f.store.resolve_path("/revocation/epoch").unwrap();
    f.store.write_file(current, 0, &first_bytes).unwrap();
    f.store
        .truncate(current, first_bytes.len() as u64)
        .unwrap();
    let err = f.store.verify_revocation_chain().unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);

    // A missing history record is a broken chain too.
    let mut f = fixture(1024);
    f.store
        .publish_revocation_epoch(&[], 600_000_000_000)
        .unwrap();
    f.store
        .publish_revocation_epoch(&[], 600_000_000_000)
        .unwrap();
    let history = f.store.resolve_path("/revocation/history").unwrap();
    f.store.unlink(history, b"2").unwrap();
    let err = f.store.verify_revocation_chain().unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
}

#[test]
fn a_revoked_capability_denies_even_when_the_envelope_is_still_valid() {
    let mut f = fixture(4096);
    let seed = [0x5a; 32];
    let grantor = public_key_for_seed(&seed).unwrap();
    let revocation_id = Hash32::from_bytes([0x44; 32]);
    let envelope = CapabilityEnvelope::new(
        grantor,
        Hash32::from_bytes([0x55; 32]),
        "tenant-a",
        Vec::new(),
        vec!["default".to_owned()],
        Vec::new(),
        None,
        None,
        revocation_id,
        1_700_000_000_000_000_000,
        Hash32::from_bytes([0x66; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &seed).unwrap();
    let verified = verify_envelope(&blob.encode().unwrap(), &[], i64::MAX, None).unwrap();
    let principal = Principal::root().with_envelope(verified);

    f.store
        .publish_revocation_epoch(
            &[
                RevocationEntry::new("tenant-a", RevocationKind::Capability, revocation_id)
                    .unwrap(),
            ],
            600_000_000_000,
        )
        .unwrap();
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &principal,
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::CapabilityRevoked);
    assert_eq!(err.message(), REVOCATION_DENIED);

    // A tenant argument that disagrees with the verified envelope is refused
    // rather than overriding it.
    let err = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &principal,
            "tenant-b",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
}

#[test]
fn an_authorized_read_returns_the_exact_object() {
    let mut f = fixture(64 * 1024);
    f.store
        .publish_revocation_epoch(
            &[RevocationEntry::new(
                "other",
                RevocationKind::Content,
                identity::content_hash(b"unrelated"),
            )
            .unwrap()],
            600_000_000_000,
        )
        .unwrap();
    let observation = f
        .store
        .observe_revocation_epoch(600_000_000_000, f.store.clock_now_ns())
        .unwrap();
    let authorized = f
        .store
        .check_read_at_epoch(
            None,
            f.id,
            &Principal::root(),
            "tenant-a",
            &observation,
            f.store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(authorized.kind(), ObjectKind::File);
    assert_eq!(
        f.store.read_object_authorized(&authorized).unwrap(),
        f.bytes
    );
}
