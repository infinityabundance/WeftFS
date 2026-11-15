//! T073 — portable authenticated range proofs.
//!
//! Everything here uses the shipped store API and the shipped checkpoint
//! writer. The fixture is a synthetic `bf16` tensor written as a **raw**
//! (chunk-table) shard, which is the only representation a Merkle range proof
//! can exist over. A frame-backed shard is refused and that refusal is tested
//! too: it is the honest boundary of the mechanism, not a fallback.

use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::identity::{self, Hash32};
use weftfs::range_proof::{
    build_path, verify_path, PathNode, PathSide, RangeAnchor, RangeProof, RANGE_DOMAIN,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SHAPE: [u64; 3] = [64, 32, 32];
const TENSOR: &str = "weight";
const CHUNK: u64 = 4096;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK,
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
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn tensor_bytes() -> Vec<u8> {
    let len = SHAPE.iter().product::<u64>() as usize * 2;
    random_bytes(len, 0xabcd_1234)
}

fn write_container(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: bytes,
        }],
        None,
    )
    .unwrap();
    let path = dir.join(format!("{name}.safetensors"));
    fs::write(&path, &file).unwrap();
    path
}

fn write_raw_checkpoint(
    store: &mut Store,
    name: &str,
    source: &Path,
    shards: u32,
) -> weftfs::checkpoint::CheckpointWriteReport {
    store
        .write_checkpoint(
            name,
            source,
            &CheckpointWriteOptions {
                shards,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Raw,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, Store, Vec<u8>) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let bytes = tensor_bytes();
    let source = write_container(&dir.path().join("src"), "one", &bytes);
    write_raw_checkpoint(&mut store, "cp", &source, 1);
    store.write_range_anchors("cp", None, None, None).unwrap();
    (dir, store, bytes)
}

fn only_anchor(store: &Store) -> (Hash32, RangeAnchor) {
    let anchors = store.range_anchors("cp").unwrap();
    assert_eq!(anchors.len(), 1, "one tensor, one shard, one anchor");
    let (id, _object, anchor) = anchors.into_iter().next().unwrap();
    (id, anchor)
}

fn prove(store: &Store, offset: u64, length: u64) -> weftfs::range_proof::RangeProofBundle {
    let (id, _) = only_anchor(store);
    store
        .prove_range("cp", TENSOR, 0, offset, length, id)
        .unwrap()
}

/// The oracle: slice the original tensor bytes.
fn region(bytes: &[u8], offset: u64, length: u64) -> &[u8] {
    &bytes[offset as usize..(offset + length) as usize]
}

#[test]
fn a_proof_verifies_for_regions_that_do_not_align_to_chunks() {
    let (_dir, mut store, bytes) = fixture();
    let (anchor_id, anchor) = only_anchor(&store);
    assert_eq!(anchor.chunk_size, CHUNK);
    assert_eq!(
        anchor.chunk_count,
        anchor.bytes.div_ceil(CHUNK),
        "a {}-byte shard at {CHUNK} bytes per chunk",
        anchor.bytes
    );
    // Aligned, unaligned, single-byte and whole-object regions.
    let cases = [
        (0u64, 512u64),
        (1, 1),
        (CHUNK - 7, 14),          // crosses one boundary
        (CHUNK * 3 + 17, CHUNK * 2 + 5), // crosses two boundaries
        (0, anchor.bytes),
    ];
    for (offset, length) in cases {
        let bundle = store
            .prove_range("cp", TENSOR, 0, offset, length, anchor_id)
            .unwrap();
        assert_eq!(
            bundle.proof.region_root,
            identity::h(RANGE_DOMAIN, region(&bytes, offset, length)),
            "the proof region digest must be the shipped domain digest of the exact bytes"
        );
        // Verify from the supplied chunk bytes (portable mode).
        let report = store
            .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[])
            .unwrap();
        assert!(report.manifest_rechecked);
        assert_eq!(report.region_bytes, length);
        assert_eq!(report.leaves_verified, bundle.proof.leaves.len() as u64);
        assert!(!report.anchor_signature_verified);
        // Verify by re-reading the store (no supplied bytes).
        let report = store
            .verify_range_proof(&bundle.proof, &anchor, None, &[])
            .unwrap();
        assert!(report.manifest_rechecked);
        assert!(report.chunk_bytes >= length);
    }
}

#[test]
fn a_proof_for_a_different_range_is_rejected() {
    let (_dir, store, _bytes) = fixture();
    let bundle = prove(&store, 1024, 2048);
    let (_, anchor) = only_anchor(&store);
    let mut forged = bundle.proof.clone();
    forged.offset += 1;
    let err = store
        .verify_range_proof(&forged, &anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(
        err.message().contains("does not cover chunk")
            || err.message().contains("region digest"),
        "unexpected message: {err}"
    );
}

#[test]
fn a_proof_with_a_substituted_leaf_is_rejected() {
    let (_dir, store, _bytes) = fixture();
    let bundle = prove(&store, 0, CHUNK * 2);
    let (_, anchor) = only_anchor(&store);
    let mut forged = bundle.proof.clone();
    assert!(forged.leaves.len() >= 2);
    // Substitute leaf 0 with the hash of a different chunk of the same table.
    let other = forged.leaves[1].hash;
    forged.leaves[0].hash = other;
    let err = store
        .verify_range_proof(&forged, &anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn a_proof_whose_root_does_not_match_the_checkpoint_is_rejected() {
    let (_dir, mut store, _bytes) = fixture();
    let bundle = prove(&store, 0, 1024);
    let (_, anchor) = only_anchor(&store);
    // A second checkpoint with different bytes and its own anchor: neither
    // anchor may verify the other checkpoint's proof.
    let second = random_bytes(
        SHAPE.iter().product::<u64>() as usize * 2,
        0x9999_0001,
    );
    let source = write_container(&_dir.path().join("src2"), "two", &second);
    write_raw_checkpoint(&mut store, "cp2", &source, 1);
    store.write_range_anchors("cp2", None, None, None).unwrap();
    let anchors = store.range_anchors("cp2").unwrap();
    let (_, second_anchor) = (anchors[0].0, anchors[0].2.clone());
    let err = store
        .verify_range_proof(&bundle.proof, &second_anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    // The untouched proof still verifies against its own anchor.
    store
        .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[])
        .unwrap();
    // Now forge the anchor's chunk-table root and re-sign nothing: the
    // dependency re-read must reject it.
    let mut forged_anchor = anchor.clone();
    forged_anchor.chunk_table_root = Hash32::from_bytes([7u8; 32]);
    let err = store
        .verify_range_proof(&bundle.proof, &forged_anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn a_truncated_path_is_rejected() {
    let (_dir, store, _bytes) = fixture();
    let bundle = prove(&store, 0, 1024);
    let (_, anchor) = only_anchor(&store);
    let mut forged = bundle.proof.clone();
    forged.paths[0].pop();
    let err = store
        .verify_range_proof(&forged, &anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(err.message().contains("truncated"), "{err}");
}

#[test]
fn a_region_that_crosses_a_chunk_boundary_incorrectly_is_rejected() {
    let (_dir, store, _bytes) = fixture();
    // Ask for a region spanning two chunks, then drop the second leaf so the
    // proof claims one chunk covers a two-chunk region.
    let bundle = prove(&store, CHUNK - 16, 32);
    assert_eq!(bundle.proof.leaves.len(), 2);
    let (_, anchor) = only_anchor(&store);
    let mut forged = bundle.proof.clone();
    forged.leaves.pop();
    forged.paths.pop();
    let err = store
        .verify_range_proof(&forged, &anchor, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(
        err.message().contains("declared region requires"),
        "unexpected message: {err}"
    );
}

#[test]
fn chunk_bytes_that_do_not_hash_to_the_leaf_are_rejected() {
    let (_dir, store, _bytes) = fixture();
    let bundle = prove(&store, 0, 1024);
    let (_, anchor) = only_anchor(&store);
    let mut chunks = bundle.chunks.clone();
    chunks[0][0] ^= 0xff;
    let err = store
        .verify_range_proof(&bundle.proof, &anchor, Some(&chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(
        err.message().contains("do not hash to the proof leaf"),
        "unexpected message: {err}"
    );
}

#[test]
fn a_signed_anchor_is_required_when_a_trust_set_is_configured() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let bytes = tensor_bytes();
    let source = write_container(&dir.path().join("src"), "signed", &bytes);
    write_raw_checkpoint(&mut store, "cp", &source, 1);
    let seed = [0x42u8; 32];
    let (written, refusals) = store
        .write_range_anchors("cp", None, Some(&seed), None)
        .unwrap();
    assert_eq!(written.len(), 1);
    assert!(refusals.is_empty());
    let (anchor_id, anchor) = only_anchor(&store);
    let signer = anchor.signature.as_ref().unwrap().public_key;
    let bundle = store
        .prove_range("cp", TENSOR, 0, 0, 256, anchor_id)
        .unwrap();
    let report = store
        .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[signer])
        .unwrap();
    assert!(report.anchor_signature_verified);
    // Untrusted signer.
    let err = store
        .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[[9u8; 32]])
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized, "{err}");
    // Tampered anchor: the signature no longer covers the bytes.
    let mut tampered = anchor.clone();
    tampered.bytes += 1;
    let err = store
        .verify_range_proof(&bundle.proof, &tampered, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn a_frame_backed_shard_is_refused_rather_than_approximated() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let bytes = tensor_bytes();
    let source = write_container(&dir.path().join("src"), "framed", &bytes);
    // The default strategy stores a mode-10 frame; there is no chunk table.
    store
        .write_checkpoint(
            "framed",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let (written, refused) = store
        .write_range_anchors("framed", None, None, None)
        .unwrap();
    assert!(written.is_empty(), "a framed shard has no chunk table");
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0].reason.contains("chunk table"),
        "unexpected reason: {}",
        refused[0].reason
    );
}

#[test]
fn a_region_spanning_shards_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let bytes = tensor_bytes();
    let source = write_container(&dir.path().join("src"), "eight", &bytes);
    write_raw_checkpoint(&mut store, "eight", &source, 8);
    store.write_range_anchors("eight", None, None, None).unwrap();
    let anchors = store.range_anchors("eight").unwrap();
    assert_eq!(anchors.len(), 8, "one anchor per raw shard");
    let (first_id, _, first) = &anchors[0];
    let shard_bytes = first.bytes;
    // Straddles shard 0 and shard 1.
    let err = store
        .prove_range("eight", TENSOR, 0, shard_bytes - 16, 64, *first_id)
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("not contained in shard"), "{err}");
    // A region wholly inside shard 1 is fine and verifies against shard 1's
    // anchor, not shard 0's.
    let (second_id, _, second) = &anchors[1];
    let bundle = store
        .prove_range("eight", TENSOR, 1, shard_bytes, 128, *second_id)
        .unwrap();
    let report = store
        .verify_range_proof(&bundle.proof, second, Some(&bundle.chunks), &[])
        .unwrap();
    assert_eq!(report.region_bytes, 128);
    let err = store
        .verify_range_proof(&bundle.proof, first, Some(&bundle.chunks), &[])
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn proof_and_anchor_records_round_trip_and_reject_changed_bytes() {
    let (_dir, store, _bytes) = fixture();
    let bundle = prove(&store, 64, 64);
    let encoded = bundle.proof.encode().unwrap();
    let decoded = RangeProof::decode(&encoded).unwrap();
    assert_eq!(decoded, bundle.proof);
    assert_eq!(decoded.encode().unwrap(), encoded);
    let anchor = RangeAnchor::decode(&bundle.anchor_bytes).unwrap();
    assert_eq!(anchor.id().unwrap(), bundle.proof.anchor);
    // A truncated record and a record with an appended byte are both refused:
    // the decoder never interprets a byte string that is not exactly canonical.
    assert!(RangeProof::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut extended = encoded.clone();
    extended.push(0);
    assert!(RangeProof::decode(&extended).is_err());
}

#[test]
fn path_builder_matches_the_frozen_promotion_rule() {
    // Seven leaves: level 0 has 7 nodes, node 6 is promoted; level 1 has 4,
    // nodes 2 and 3 pair; level 2 has 2.
    let table: Vec<(u64, Hash32)> = (0..7u8)
        .map(|index| (4, Hash32::from_bytes([index + 1; 32])))
        .collect();
    let root = identity::merkle_root_from_chunks(table.iter().copied());
    for index in 0..7u64 {
        let path: Vec<PathNode> = build_path(&table, index).unwrap();
        verify_path(
            7,
            index,
            identity::merkle_leaf(4, table[index as usize].1),
            &path,
            root,
        )
        .unwrap();
    }
    // A promoted leaf carries no node for its own level.
    assert_eq!(build_path(&table, 6).unwrap()[0].side, PathSide::Left);
}

#[test]
fn a_fixture_written_through_a_read_only_handle_can_still_be_proved() {
    let (_dir, store, bytes) = fixture();
    drop(store);
    // Reopen read-only: proving must not require a writer.
    let store =
        Store::open(_dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    let (anchor_id, anchor) = only_anchor(&store);
    let bundle = store
        .prove_range("cp", TENSOR, 0, 0, 32, anchor_id)
        .unwrap();
    let report = store
        .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[])
        .unwrap();
    assert_eq!(report.region_bytes, 32);
    assert_eq!(
        bundle.proof.region_root,
        identity::h(RANGE_DOMAIN, region(&bytes, 0, 32))
    );
}
