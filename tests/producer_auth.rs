//! T074: producer-authenticated provenance, key rotation and revocation.
//!
//! Every adversarial case in the subphase brief is one test assertion: invalid
//! signer, corrupted input root, mismatched output root, revoked key, key
//! rotated after signing, and a valid signature whose declared inputs do not
//! match the store's actual content. Each is a typed refusal.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::Limits;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::producer_auth::{
    public_key_for_seed, rotate_key_set, verify_attestation, verify_attestation_against_store,
    Determinism, ProducerAttestation, ProducerInput, ProducerKey, ProducerKeySet, Rejection,
    VerifyMode,
};
use weftfs::store::{Options, Store};

const SEED_A: [u8; 32] = [0x11; 32];
const SEED_B: [u8; 32] = [0x22; 32];
const SEED_C: [u8; 32] = [0x33; 32];
const SEED_D: [u8; 32] = [0x44; 32];

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

fn key(seed: &[u8; 32], label: &str, from: u64) -> ProducerKey {
    ProducerKey {
        key: public_key_for_seed(seed),
        label: label.to_owned(),
        valid_from_epoch: from,
        valid_until_epoch: None,
        revoked_at_epoch: None,
        rotated_at_epoch: None,
    }
}

fn key_set(epoch: u64) -> ProducerKeySet {
    let mut a = key(&SEED_A, "run-a", 0);
    a.rotated_at_epoch = Some(4);
    let b = key(&SEED_B, "run-b", 4);
    let mut c = key(&SEED_C, "run-c", 0);
    c.revoked_at_epoch = Some(5);
    ProducerKeySet::new(epoch, None, vec![a, b, c]).unwrap()
}

fn attestation(
    seed: &[u8; 32],
    set: &ProducerKeySet,
    epoch: u64,
    inputs: Vec<ProducerInput>,
    output: Hash32,
) -> ProducerAttestation {
    let mut attestation = ProducerAttestation::new(
        "trainer-1",
        "run-001",
        "weftfs",
        "1.4.2",
        Determinism::Exact,
        epoch,
        set.root().unwrap(),
        inputs,
        output,
        None,
        1_700_000_000_000_000_000,
    )
    .unwrap();
    attestation.sign(seed).unwrap();
    attestation
}

fn code(err: &weftfs::producer_auth::AttestationError) -> &'static str {
    err.code()
}

#[test]
fn a_current_key_authenticates_a_declaration() {
    let set = key_set(5);
    let attestation = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: Hash32::from_bytes([0x10; 32]),
        }],
        Hash32::from_bytes([0x20; 32]),
    );
    let bytes = attestation.canonical_bytes().unwrap();
    let verified = verify_attestation(&bytes, &set, VerifyMode::Current, None).unwrap();
    assert_eq!(verified.signer, public_key_for_seed(&SEED_B));
    assert_eq!(verified.key_status.as_str(), "current");
    assert!(verified.currently_trusted);
    assert!(verified.valid_at_signing);
    assert_eq!(verified.attestation_id, attestation.id().unwrap());
    assert_eq!(verified.determinism, Determinism::Exact);
}

#[test]
fn an_unknown_signer_is_refused() {
    let set = key_set(5);
    // Seed D is not in the key set.
    let attestation = attestation(&SEED_D, &set, 5, vec![], Hash32::from_bytes([0x20; 32]));
    let bytes = attestation.canonical_bytes().unwrap();
    let err = verify_attestation(&bytes, &set, VerifyMode::Current, None).unwrap_err();
    assert_eq!(code(&err), Rejection::UnknownSigner.as_str());
    assert_eq!(err.error.class(), Class::Unauthorized);
}

#[test]
fn a_bad_signature_is_refused() {
    let set = key_set(5);
    let attestation = attestation(&SEED_B, &set, 4, vec![], Hash32::from_bytes([0x20; 32]));
    let mut bytes = attestation.canonical_bytes().unwrap();
    // Flip one byte inside the 64-byte signature value. The record stays
    // canonical WCB; only the signature changes. `bytes(n)` is tag 0x06.
    let signature_marker = [0x06, 0x40]; // bytes(64)
    let position = bytes
        .windows(2)
        .position(|window| window == signature_marker)
        .expect("signature tag");
    bytes[position + 2] ^= 0x01;
    let err = verify_attestation(&bytes, &set, VerifyMode::Current, None).unwrap_err();
    assert_eq!(code(&err), Rejection::BadSignature.as_str());
    assert_eq!(err.error.class(), Class::Corrupt);
}

#[test]
fn a_revoked_key_refuses_in_every_mode() {
    let set = key_set(5);
    let attestation = attestation(&SEED_C, &set, 4, vec![], Hash32::from_bytes([0x20; 32]));
    let bytes = attestation.canonical_bytes().unwrap();
    for mode in [VerifyMode::Current, VerifyMode::Historical] {
        let err = verify_attestation(&bytes, &set, mode, None).unwrap_err();
        assert_eq!(code(&err), Rejection::RevokedKey.as_str());
        assert_eq!(err.error.class(), Class::Unauthorized);
    }
}

#[test]
fn a_key_rotated_after_signing_is_refused_currently_and_audited_historically() {
    let set = key_set(5);
    let attestation = attestation(&SEED_A, &set, 2, vec![], Hash32::from_bytes([0x20; 32]));
    let bytes = attestation.canonical_bytes().unwrap();
    let err = verify_attestation(&bytes, &set, VerifyMode::Current, None).unwrap_err();
    assert_eq!(code(&err), Rejection::KeyRotated.as_str());
    assert_eq!(err.error.class(), Class::Unauthorized);

    let verified = verify_attestation(&bytes, &set, VerifyMode::Historical, None).unwrap();
    assert!(verified.valid_at_signing);
    assert!(!verified.currently_trusted);
    assert_eq!(verified.key_status.as_str(), "retired");
}

#[test]
fn a_key_that_was_not_yet_valid_at_signing_is_refused() {
    let set = key_set(5);
    // Key B starts at epoch 4; sign at epoch 2.
    let attestation = attestation(&SEED_B, &set, 2, vec![], Hash32::from_bytes([0x20; 32]));
    let bytes = attestation.canonical_bytes().unwrap();
    let err = verify_attestation(&bytes, &set, VerifyMode::Historical, None).unwrap_err();
    assert_eq!(code(&err), Rejection::KeyNotYetValid.as_str());
}

#[test]
fn a_key_set_root_mismatch_is_refused() {
    let set = key_set(5);
    let attestation = attestation(&SEED_B, &set, 4, vec![], Hash32::from_bytes([0x20; 32]));
    let bytes = attestation.canonical_bytes().unwrap();
    let other = key_set(6);
    let err = verify_attestation(&bytes, &other, VerifyMode::Current, None).unwrap_err();
    assert_eq!(code(&err), Rejection::KeySetRootMismatch.as_str());
}

#[test]
fn rotation_records_the_previous_root_and_advances_the_epoch() {
    let first = key_set(5);
    let second = rotate_key_set(&first, 6, vec![key(&SEED_B, "run-b", 0)]).unwrap();
    assert_eq!(second.rotated_from, Some(first.root().unwrap()));
    assert_ne!(second.root().unwrap(), first.root().unwrap());
    assert!(rotate_key_set(&first, 5, vec![key(&SEED_B, "run-b", 0)]).is_err());
}

#[test]
fn records_roundtrip_through_canonical_wcb() {
    let set = key_set(5);
    let bytes = set.canonical_bytes().unwrap();
    let decoded = ProducerKeySet::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(decoded, set);
    assert_eq!(wcb_reencode(&bytes), bytes);
    // A non-canonical key set is refused.
    let mut mutated = bytes.clone();
    let last = mutated.len() - 1;
    mutated[last] ^= 0x01;
    assert!(ProducerKeySet::from_canonical_bytes(&mutated).is_err());

    let attestation = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: Hash32::from_bytes([0x10; 32]),
        }],
        Hash32::from_bytes([0x20; 32]),
    );
    let record = attestation.canonical_bytes().unwrap();
    let decoded = ProducerAttestation::from_canonical_bytes(&record).unwrap();
    assert_eq!(decoded, attestation);
    assert_eq!(wcb_reencode(&record), record);
    // Bytes outside the signature are not self-checking: canonical WCB accepts
    // the mutated record, and the signature is what refuses it. This is the
    // reason an attestation must always be verified, never merely parsed.
    let mut mutated = record.clone();
    let position = mutated
        .windows(7)
        .position(|window| window == b"trainer")
        .unwrap();
    mutated[position] = b'T';
    assert!(ProducerAttestation::from_canonical_bytes(&mutated).is_ok());
    let err = verify_attestation(&mutated, &set, VerifyMode::Current, None).unwrap_err();
    assert_eq!(code(&err), Rejection::BadSignature.as_str());
}

fn wcb_reencode(bytes: &[u8]) -> Vec<u8> {
    let value = weftcodec::wcb::decode(bytes, &Limits::default()).unwrap();
    weftcodec::wcb::encode(&value, &Limits::default()).unwrap()
}

fn content_root(store: &Store, path: &str) -> Hash32 {
    let id = store.resolve_path(path).unwrap();
    let record = store.object_meta(id).unwrap();
    store.manifest_of(record.content).unwrap().content_hash
}

#[test]
fn the_store_cross_check_names_a_corrupted_input_and_a_wrong_output() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(
            root,
            b"dataset.bin",
            b"dataset bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-dataset",
        )
        .unwrap();
    // Pin the moment before the output exists; the store cross-check is
    // snapshot-scoped.
    let earlier = store.snapshot_create("after-dataset").unwrap();
    store
        .create_file_seeded(
            root,
            b"output.bin",
            b"output bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-output",
        )
        .unwrap();
    let dataset = content_root(&store, "/dataset.bin");
    let output = content_root(&store, "/output.bin");
    let set = key_set(5);

    let good = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: dataset,
        }],
        output,
    );
    let good_bytes = good.canonical_bytes().unwrap();
    let checked =
        verify_attestation_against_store(&store, None, &good_bytes, &set, VerifyMode::Current)
            .unwrap();
    assert!(checked.inputs_present);
    assert!(checked.output_present);

    // A corrupted declared input root: valid signature, but the root is not
    // content in this store.
    let bad_input = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: Hash32::from_bytes([0x99; 32]),
        }],
        output,
    );
    let bad_input_bytes = bad_input.canonical_bytes().unwrap();
    let err =
        verify_attestation_against_store(&store, None, &bad_input_bytes, &set, VerifyMode::Current)
            .unwrap_err();
    assert_eq!(code(&err), Rejection::InputMismatch.as_str());
    assert_eq!(err.error.class(), Class::Corrupt);

    // A mismatched output root.
    let bad_output = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: dataset,
        }],
        Hash32::from_bytes([0x88; 32]),
    );
    let bad_output_bytes = bad_output.canonical_bytes().unwrap();
    let err = verify_attestation_against_store(
        &store,
        None,
        &bad_output_bytes,
        &set,
        VerifyMode::Current,
    )
    .unwrap_err();
    assert_eq!(code(&err), Rejection::OutputMismatch.as_str());

    // The declared roots are checked at a pinned snapshot too: the output did
    // not exist at `earlier`.
    let before = attestation(
        &SEED_B,
        &set,
        4,
        vec![ProducerInput {
            role: "dataset".into(),
            content_root: dataset,
        }],
        output,
    );
    let before_bytes = before.canonical_bytes().unwrap();
    let err = verify_attestation_against_store(
        &store,
        Some(earlier),
        &before_bytes,
        &set,
        VerifyMode::Current,
    )
    .unwrap_err();
    assert_eq!(code(&err), Rejection::OutputMismatch.as_str());
}
