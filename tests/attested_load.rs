//! Signed load attestations (`weft.load-attestation.v1`).
//!
//! The fixture is a two-tensor `bf16` container written as a checkpoint. Every
//! test measures a real store: the attestation is built from decoded bytes, and
//! verification re-reads them.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::load_attestation::{
    attest_checkpoint_load, verify_against_store, AttestOptions, AttestStatus, LoadAttestation,
    VerifyOptions,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SHAPE: [u64; 2] = [32, 16];
const TENSOR_A: &str = "alpha";
const TENSOR_B: &str = "beta";
const SEED: [u8; 32] = [7u8; 32];

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16 * 1024,
        checkpoint_interval: 64,
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
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let len = SHAPE.iter().product::<u64>() as usize * 2;
    let a = random_bytes(len, 0xa1);
    let b = random_bytes(len, 0xb2);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: TENSOR_A,
                dtype: "bf16",
                shape: &SHAPE,
                data: &a,
            },
            SafetensorsEntry {
                name: TENSOR_B,
                dtype: "bf16",
                shape: &SHAPE,
                data: &b,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("pair.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn store_with_checkpoint(root: &Path) -> Store {
    let mut store = Store::init(root.join("store"), opts()).unwrap();
    let source = write_container(&root.join("src"));
    store
        .write_checkpoint(
            "pair",
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                strategy: ShardStrategy::TensorSharded,
                deterministic_seed: Some([9u8; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    store
}

fn defaults() -> AttestOptions {
    AttestOptions {
        replica: "replica-0".to_owned(),
        principal: "test".to_owned(),
        issued_at_ns: Some(1_700_000_000_000_000_000),
        seed: Some(SEED),
        ..AttestOptions::default()
    }
}

#[test]
fn a_signed_attestation_round_trips_and_reverifies() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();

    assert!(attestation.signature.is_some());
    assert_eq!(attestation.totals.loaded_tensors, 2);
    assert_eq!(attestation.totals.unverified_tensors, 0);
    for tensor in &attestation.tensors {
        assert_eq!(tensor.status, AttestStatus::Loaded);
        assert_eq!(tensor.ranges.len(), 1);
        assert_eq!(tensor.groups.len(), 2, "two-way sharding, two groups");
        assert!(tensor.full_root_verified);
        assert!(tensor.bytes_loaded > 0);
    }

    let bytes = attestation.encode().unwrap();
    let decoded = LoadAttestation::decode(&bytes).unwrap();
    assert_eq!(decoded, attestation);
    decoded.verify_signature().unwrap();
    assert_eq!(decoded.root().unwrap(), attestation.root().unwrap());

    let report = verify_against_store(&store, &decoded, &VerifyOptions::default()).unwrap();
    assert_eq!(report.verified_tensors, 2);
    assert_eq!(report.unverified_tensors, 0);
    assert_eq!(report.ranges_replayed, 2);
    assert!(report.replayed_bytes > 0);
}

#[test]
fn tampering_with_a_recorded_range_breaks_the_signature() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();

    let mut tampered = attestation.clone();
    tampered.tensors[0].ranges[0].digest = weftfs::Hash32::from_bytes([0xab; 32]);
    let bytes = tampered.encode().unwrap();
    let decoded = LoadAttestation::decode(&bytes).unwrap();
    let err = verify_against_store(&store, &decoded, &VerifyOptions::default()).unwrap_err();
    assert!(
        err.message().contains("signature"),
        "expected a signature failure, got: {err}"
    );
}

#[test]
fn tampering_with_the_store_fails_the_replay() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();

    // Corrupt the manifest blob of the first shard group. The store's own
    // read path hashes it, so the replay must fail closed.
    let shard = attestation.tensors[0].groups[0];
    let record = store.object_meta(shard.object).unwrap();
    let hex = record.content.to_hex();
    let path = dir
        .path()
        .join("store")
        .join("manifests")
        .join(&hex[..2])
        .join(&hex);
    let mut bytes = fs::read(&path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&path, &bytes).unwrap();

    let err = verify_against_store(&store, &attestation, &VerifyOptions::default()).unwrap_err();
    assert_ne!(err.class().as_str(), "NotFound");
    assert!(
        err.class().as_str() == "Corrupt" || err.class().as_str() == "Malformed",
        "expected a corrupt read, got {}: {err}",
        err.class().as_str()
    );
}

#[test]
fn an_unloaded_tensor_is_reported_and_never_counted() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let options = AttestOptions {
        tensors: Some(vec![TENSOR_A.to_owned()]),
        ..defaults()
    };
    let attestation = attest_checkpoint_load(&store, "pair", &options).unwrap();
    assert_eq!(attestation.totals.loaded_tensors, 1);
    assert_eq!(attestation.totals.unverified_tensors, 1);
    assert!(attestation.totals.unverified_bytes > 0);
    let missing = attestation
        .tensors
        .iter()
        .find(|tensor| tensor.name == TENSOR_B)
        .unwrap();
    assert_eq!(missing.status, AttestStatus::Unverified);
    assert_eq!(missing.reason.as_deref(), Some("not requested"));
    assert!(missing.ranges.is_empty());
    assert!(!missing.full_root_verified);

    let report = verify_against_store(&store, &attestation, &VerifyOptions::default()).unwrap();
    assert_eq!(report.verified_tensors, 1);
    assert_eq!(report.unverified_tensors, 1);
    assert_eq!(report.unverified.len(), 1);
    assert_eq!(report.unverified[0].0, TENSOR_B);
}

#[test]
fn a_region_load_does_not_claim_the_whole_tensor_root() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let options = AttestOptions {
        tensors: Some(vec![TENSOR_A.to_owned()]),
        region: Some(weftcodec::tensor::Region::new(
            vec![0, 0],
            vec![4, SHAPE[1]],
        )),
        ..defaults()
    };
    let attestation = attest_checkpoint_load(&store, "pair", &options).unwrap();
    let loaded = attestation
        .tensors
        .iter()
        .find(|tensor| tensor.name == TENSOR_A)
        .unwrap();
    assert_eq!(loaded.status, AttestStatus::Loaded);
    assert!(
        !loaded.full_root_verified,
        "a region cannot prove the whole root"
    );
    assert_eq!(loaded.bytes_loaded, 4 * SHAPE[1] * 2);
    assert_eq!(loaded.ranges[0].offset, 0);
    assert!(!loaded.groups.is_empty(), "the intersecting group is named");
    assert!(loaded.groups.len() < 2, "only one two-way shard intersects");

    let report = verify_against_store(&store, &attestation, &VerifyOptions::default()).unwrap();
    assert_eq!(report.verified_tensors, 1);
    assert_eq!(report.ranges_replayed, 1);
}

#[test]
fn a_trust_set_is_enforced() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();
    let public = *attestation.verifier.public_key.as_bytes();
    let options = VerifyOptions {
        trust: vec![public],
        require_signature: true,
    };
    verify_against_store(&store, &attestation, &options).unwrap();

    let other = VerifyOptions {
        trust: vec![[0x11; 32]],
        require_signature: true,
    };
    let err = verify_against_store(&store, &attestation, &other).unwrap_err();
    assert_eq!(err.class().as_str(), "Unauthorized");
}

#[test]
fn a_record_with_the_wrong_totals_is_rejected_on_decode() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();
    let mut tampered = attestation.clone();
    tampered.totals.loaded_bytes = tampered.totals.loaded_bytes.saturating_add(1);
    let bytes = tampered.encode().unwrap();
    let err = LoadAttestation::decode(&bytes).unwrap_err();
    assert_eq!(err.class().as_str(), "Corrupt");
}

#[test]
fn an_unverified_entry_without_a_reason_is_rejected() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();
    let mut tampered = attestation.clone();
    tampered.tensors[0].reason = None;
    tampered.tensors[0].status = AttestStatus::Unverified;
    tampered.tensors[0].ranges.clear();
    tampered.tensors[0].groups.clear();
    let bytes = tampered.encode().unwrap();
    let err = LoadAttestation::decode(&bytes).unwrap_err();
    assert_eq!(err.class().as_str(), "Corrupt");
}

#[test]
fn restore_reads_back_the_same_bytes_the_attestation_recorded() {
    let dir = tempdir().unwrap();
    let store = store_with_checkpoint(dir.path());
    let attestation = attest_checkpoint_load(&store, "pair", &defaults()).unwrap();
    let opened = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    for tensor in &attestation.tensors {
        let (bytes, report) = opened
            .restore_checkpoint_bytes(
                "pair",
                &weftfs::checkpoint::CheckpointRestoreOptions {
                    tensor: Some(tensor.name.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let header = container_header(&bytes) as usize;
        assert_eq!(bytes.len() as u64 - header as u64, tensor.bytes_loaded);
        let payload = &bytes[header..];
        assert_eq!(
            weftfs::identity::h(weftfs::load_attestation::LOAD_RANGE_DOMAIN, payload),
            tensor.ranges[0].digest
        );
        assert!(report.tensors[0].full_root_verified);
    }
}

fn container_header(bytes: &[u8]) -> u64 {
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    8 + u64::from_le_bytes(prefix)
}
