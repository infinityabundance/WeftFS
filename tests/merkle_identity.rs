//! Merkle content identity (`weft.manifest.v3`).
//!
//! These tests pin the tree construction, the exact root for edge sizes, the
//! chunk-granular range-write cost model, and fail-closed tamper detection.
//! The default whole-file identity and every existing fixture are exercised by
//! their own suites and stay byte-identical.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::format::{ContentIdentity, Manifest, MANIFEST_SCHEMA_V3};
use weftfs::identity::{self, Hash32};
use weftfs::recovery::{manifest_path, object_path, read_manifest};
use weftfs::store::{OpenMode, Options, Store};

fn opts(chunk_size: u64, identity: ContentIdentity) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        content_identity: identity,
        ..Options::default()
    }
}

fn manifest_of(store: &Store, id: Hash32) -> Manifest {
    let record = store.object_meta(id).unwrap();
    read_manifest(store.root(), record.content).unwrap()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn create(store: &mut Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, name, bytes, 0o644, 0, 0, BTreeMap::new(), seed)
        .unwrap()
}

#[test]
fn root_is_deterministic_and_chunk_size_is_part_of_the_identity() {
    let bytes = pattern(5_000);
    let dir_a = tempdir().unwrap();
    let mut a = Store::init(dir_a.path(), opts(1024, ContentIdentity::MerkleV3)).unwrap();
    let id_a = create(&mut a, b"a.bin", &bytes, b"seed-a");
    let manifest_a = manifest_of(&a, id_a);

    let dir_b = tempdir().unwrap();
    let mut b = Store::init(dir_b.path(), opts(1024, ContentIdentity::MerkleV3)).unwrap();
    let id_b = create(&mut b, b"b.bin", &bytes, b"seed-b");
    let manifest_b = manifest_of(&b, id_b);

    assert_eq!(manifest_a.identity, ContentIdentity::MerkleV3);
    assert_eq!(manifest_b.identity, ContentIdentity::MerkleV3);
    assert_eq!(
        manifest_a.content_hash, manifest_b.content_hash,
        "same bytes and chunk size must produce the same Merkle root"
    );
    assert_eq!(
        manifest_a.content_hash,
        identity::merkle_root_from_chunks(manifest_a.chunks.iter().map(|c| (c.len, c.hash)))
    );
    assert_ne!(
        manifest_a.content_hash,
        identity::content_hash(&bytes),
        "a Merkle root must not equal the whole-file ContentId"
    );

    // The canonical schema value and content_identity field.
    let value = manifest_a.to_value();
    let map = weftfs::wcb_util::as_map(&value).unwrap();
    assert_eq!(
        map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA_V3.to_owned()))
    );
    assert_eq!(
        map.get("content_identity"),
        Some(&Value::Str("merkle-v1".to_owned()))
    );
    assert!(map.contains_key("content_root"));
    assert!(!map.contains_key("content_hash"));

    // Chunk size is part of the manifest, so changing it changes the root.
    let dir_c = tempdir().unwrap();
    let mut c = Store::init(dir_c.path(), opts(512, ContentIdentity::MerkleV3)).unwrap();
    let id_c = create(&mut c, b"c.bin", &bytes, b"seed-c");
    let manifest_c = manifest_of(&c, id_c);
    assert_ne!(manifest_a.content_hash, manifest_c.content_hash);

    // Default whole-file identity is untouched.
    let dir_raw = tempdir().unwrap();
    let mut raw = Store::init(dir_raw.path(), opts(1024, ContentIdentity::WholeFileV1)).unwrap();
    let id_raw = create(&mut raw, b"raw.bin", &bytes, b"seed-raw");
    let manifest_raw = manifest_of(&raw, id_raw);
    assert_eq!(manifest_raw.identity, ContentIdentity::WholeFileV1);
    assert_eq!(manifest_raw.content_hash, identity::content_hash(&bytes));
    let raw_value = manifest_raw.to_value();
    let raw_map = weftfs::wcb_util::as_map(&raw_value).unwrap();
    assert_eq!(
        raw_map.get("schema"),
        Some(&Value::Str("weft.manifest.v1".to_owned()))
    );
}

#[test]
fn edge_sizes_have_the_specified_roots() {
    let cases: &[(usize, u64, usize)] = &[
        (0, 16, 0),
        (1, 16, 1),
        (15, 16, 1),
        (16, 16, 1),
        (17, 16, 2),
        (64, 16, 4),
        (65, 16, 5),
    ];
    for (len, chunk_size, expected_chunks) in cases {
        let dir = tempdir().unwrap();
        let mut store =
            Store::init(dir.path(), opts(*chunk_size, ContentIdentity::MerkleV3)).unwrap();
        let bytes = pattern(*len);
        let id = create(&mut store, b"f", &bytes, b"edge");
        let manifest = manifest_of(&store, id);
        assert_eq!(manifest.chunks.len(), *expected_chunks, "len {len}");
        assert_eq!(manifest.content_hash, manifest_root(&manifest));
        if bytes.is_empty() {
            assert_eq!(manifest.content_hash, identity::merkle_empty_root());
        }
        assert_eq!(store.read_object(id).unwrap(), bytes);
        let report = store.verify(true).unwrap();
        assert!(
            report.ok,
            "verify failed for len {len}: {:?}",
            report.problems
        );
    }
}

fn manifest_root(manifest: &Manifest) -> Hash32 {
    identity::merkle_root_from_chunks(manifest.chunks.iter().map(|c| (c.len, c.hash)))
}

fn large_bytes(len: usize, chunk_size: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for (index, chunk) in out.chunks_mut(chunk_size).enumerate() {
        let value = (index % 251) as u8;
        chunk.fill(value);
    }
    out
}

#[test]
fn four_kib_overwrite_of_128_mib_does_not_read_the_whole_file() {
    const SIZE: usize = 128 * 1024 * 1024;
    const CHUNK: u64 = 64 * 1024;
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(CHUNK, ContentIdentity::MerkleV3)).unwrap();
    let bytes = large_bytes(SIZE, CHUNK as usize);
    let id = create(&mut store, b"large.bin", &bytes, b"large");
    let before = manifest_of(&store, id);
    assert_eq!(before.chunks.len(), SIZE / CHUNK as usize);

    let offset: u64 = 33_000_001;
    let payload = vec![0xA5u8; 4 * 1024];
    store.reset_range_write_chunk_reads();
    store.write_file(id, offset, &payload).unwrap();
    let read_chunks = store.range_write_chunk_reads();

    let after = manifest_of(&store, id);
    assert_eq!(after.identity, ContentIdentity::MerkleV3);
    assert_ne!(before.content_hash, after.content_hash);
    assert_eq!(after.content_hash, manifest_root(&after));

    let first = (offset / CHUNK) as usize;
    let last = ((offset + payload.len() as u64 - 1) / CHUNK) as usize;
    for (index, (old, new)) in before.chunks.iter().zip(after.chunks.iter()).enumerate() {
        if index < first || index > last {
            assert_eq!(old, new, "untouched chunk {index} must keep its reference");
        }
    }
    assert!(
        read_chunks <= 2,
        "a {}-chunk file must not stream every chunk for a 4 KiB write (read {read_chunks})",
        before.chunks.len()
    );
    assert_eq!(
        store.read_range(id, offset, payload.len() as u64).unwrap(),
        payload
    );

    let content_id = store.object_meta(id).unwrap().content;
    let report = store
        .verify_region(content_id, offset, payload.len() as u64)
        .unwrap();
    assert_eq!(report.content_root, after.content_hash);
    assert_eq!(report.chunks_verified, last - first + 1);
    assert_eq!(report.bytes_verified, (last - first + 1) as u64 * CHUNK);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn tampered_chunk_blob_fails_range_read_and_verify_naming_the_index() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(16, ContentIdentity::MerkleV3)).unwrap();
    let bytes = pattern(40);
    let id = create(&mut store, b"t.bin", &bytes, b"tamper");
    let manifest = manifest_of(&store, id);
    assert_eq!(manifest.chunks.len(), 3);

    // Flip one byte inside chunk index 1.
    let path = object_path(store.root(), manifest.chunks[1].hash);
    let mut blob = fs::read(&path).unwrap();
    blob[3] ^= 0x01;
    fs::write(&path, &blob).unwrap();

    let err = store.read_range(id, 16, 4).unwrap_err();
    assert!(
        format!("{err}").contains("chunk 1"),
        "range read must name chunk 1: {err}"
    );
    let content_id = store.object_meta(id).unwrap().content;
    let err = store.verify_region(content_id, 16, 4).unwrap_err();
    assert!(
        format!("{err}").contains("chunk 1"),
        "verify_region must name chunk 1: {err}"
    );
    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    assert!(report
        .problems
        .iter()
        .any(|problem| problem.detail.contains("chunk 1")));
}

#[test]
fn tampered_manifest_chunk_hash_is_a_root_mismatch() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(16, ContentIdentity::MerkleV3)).unwrap();
    let bytes = pattern(40);
    let id = create(&mut store, b"m.bin", &bytes, b"root");
    let manifest = manifest_of(&store, id);

    let mut corrupted = manifest.clone();
    corrupted.chunks[0].hash = identity::h("weft/test/tamper/v1", b"not-the-chunk");
    // Keep the old declared root so the chunk table no longer folds to it.
    let value = corrupted.to_value();
    let err = Manifest::from_wcb(&value).unwrap_err();
    assert_eq!(err.class().to_string(), "Corrupt");
    assert!(format!("{err}").contains("content_root"));

    // An unknown schema fails closed rather than guessing.
    let mut unknown = weftfs::wcb_util::as_map(&manifest.to_value())
        .unwrap()
        .clone();
    unknown.insert(
        "schema".to_owned(),
        Value::Str("weft.manifest.v9".to_owned()),
    );
    let err = Manifest::from_wcb(&Value::Map(unknown)).unwrap_err();
    assert_eq!(err.class().to_string(), "UnsupportedVersion");
}

#[test]
fn reopen_gc_and_verify_preserve_merkle_files() {
    let dir = tempdir().unwrap();
    let chunk: u64 = 256;
    {
        let mut store = Store::init(dir.path(), opts(chunk, ContentIdentity::MerkleV3)).unwrap();
        let bytes = pattern(1_000);
        let id = create(&mut store, b"keep.bin", &bytes, b"reopen");
        let manifest = manifest_of(&store, id);
        assert_eq!(manifest.identity, ContentIdentity::MerkleV3);
        assert!(store.verify(true).unwrap().ok);
    }

    // Reopen with the default identity: the existing v3 manifest must be read
    // and preserved, not silently downgraded.
    let mut store = Store::open_with_options(
        dir.path(),
        OpenMode::ReadWrite,
        opts(chunk, ContentIdentity::WholeFileV1),
    )
    .unwrap();
    let id = store.resolve_path("/keep.bin").unwrap();
    let bytes = pattern(1_000);
    assert_eq!(store.read_object(id).unwrap(), bytes);
    let manifest = manifest_of(&store, id);
    assert_eq!(manifest.identity, ContentIdentity::MerkleV3);

    let before = store.gc(true).unwrap();
    assert!(before.deleted.is_empty(), "nothing is unreachable yet");
    let after = manifest_of(&store, id);
    assert_eq!(after, manifest);
    assert_eq!(store.read_object(id).unwrap(), bytes);
    assert!(store.verify(true).unwrap().ok);

    // A rewrite through a whole-file-configured store keeps v3.
    store.write_file(id, 0, b"XY").unwrap();
    let rewritten = manifest_of(&store, id);
    assert_eq!(rewritten.identity, ContentIdentity::MerkleV3);
    let mut expected = bytes.clone();
    expected[0..2].copy_from_slice(b"XY");
    assert_eq!(store.read_object(id).unwrap(), expected);
    assert_eq!(rewritten.content_hash, manifest_root(&rewritten));
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn manifest_path_is_content_address_keyed() {
    // A reader must obtain a v3 manifest from `manifests/<ab>/<id>`, exactly
    // like v1/v2; there is no side index to lose.
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(64, ContentIdentity::MerkleV3)).unwrap();
    let id = create(&mut store, b"p.bin", &pattern(300), b"path");
    let record = store.object_meta(id).unwrap();
    assert!(manifest_path(store.root(), record.content).is_file());
}
