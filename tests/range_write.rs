//! Multi-range writes and the advisory manifest cache.
//!
//! `Store::write_file_ranges` applies several ranges against one manifest read,
//! one manifest write, and one commit. `Store::write_file_cached` reuses a
//! manifest the caller already holds, but only when it still names the current
//! content id. These tests pin result equivalence, overlap order, the cache
//! validation rule, and the one-commit cost.

use std::collections::BTreeMap;

use tempfile::tempdir;

use weftfs::format::ContentIdentity;
use weftfs::store::{ManifestCache, Options, Store};

fn opts(identity: ContentIdentity) -> Options {
    Options {
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        content_identity: identity,
        ..Options::default()
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn file(store: &mut Store, name: &[u8], bytes: &[u8]) -> weftfs::identity::Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, name, bytes, 0o644, 0, 0, BTreeMap::new(), name)
        .unwrap()
}

#[test]
fn ranges_match_sequential_writes_byte_for_byte() {
    let base = pattern(20_000);
    let ranges: Vec<(u64, Vec<u8>)> = (0..40)
        .map(|i| {
            let len = 1 + (i * 37) % 2_000;
            let offset = (i * 4_111) % (base.len() as u64 - len as u64);
            (offset, vec![(i * 13) as u8; len as usize])
        })
        .collect();

    let dir_a = tempdir().unwrap();
    let mut sequential = Store::init(dir_a.path(), opts(ContentIdentity::WholeFileV1)).unwrap();
    let id_a = file(&mut sequential, b"f", &base);
    for (offset, bytes) in &ranges {
        sequential.write_file(id_a, *offset, bytes).unwrap();
    }
    let bytes_a = sequential.read_object(id_a).unwrap();

    let dir_b = tempdir().unwrap();
    let mut batched = Store::init(dir_b.path(), opts(ContentIdentity::WholeFileV1)).unwrap();
    let id_b = file(&mut batched, b"f", &base);
    let borrowed: Vec<(u64, &[u8])> = ranges
        .iter()
        .map(|(offset, bytes)| (*offset, bytes.as_slice()))
        .collect();
    let outcome = batched.write_file_ranges(id_b, &borrowed).unwrap();
    let bytes_b = batched.read_object(id_b).unwrap();

    assert_eq!(bytes_a, bytes_b);
    assert_eq!(outcome.size, bytes_b.len() as u64);
    let record = batched.object_meta(id_b).unwrap();
    assert_eq!(record.content, outcome.content);
}

#[test]
fn one_multi_range_write_publishes_one_commit() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(ContentIdentity::MerkleV3)).unwrap();
    let id = file(&mut store, b"f", &pattern(64 * 1024));
    let before = store.generation();
    let ranges: Vec<(u64, &[u8])> = vec![
        (0, b"alpha".as_slice()),
        (4096, b"beta".as_slice()),
        (50_000, b"gamma".as_slice()),
        (65_000, b"delta".as_slice()),
    ];
    store.write_file_ranges(id, &ranges).unwrap();
    assert_eq!(store.generation(), before + 1);
    let bytes = store.read_object(id).unwrap();
    assert_eq!(&bytes[..5], b"alpha");
    assert_eq!(&bytes[4096..4100], b"beta");
    assert_eq!(&bytes[50_000..50_005], b"gamma");
    assert_eq!(&bytes[65_000..65_001], b"d");
}

#[test]
fn overlapping_ranges_apply_in_call_order() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(ContentIdentity::MerkleV3)).unwrap();
    let id = file(&mut store, b"f", &[0u8; 128]);
    // Later ranges win, exactly as they would as separate calls.
    let ranges: Vec<(u64, &[u8])> = vec![(16, &[1u8; 64]), (32, &[2u8; 32]), (40, &[3u8; 8])];
    store.write_file_ranges(id, &ranges).unwrap();
    let bytes = store.read_object(id).unwrap();
    assert!(bytes[16..32].iter().all(|byte| *byte == 1));
    assert!(bytes[32..40].iter().all(|byte| *byte == 2));
    assert!(bytes[40..48].iter().all(|byte| *byte == 3));
    assert!(bytes[48..64].iter().all(|byte| *byte == 2));
    assert!(bytes[64..80].iter().all(|byte| *byte == 1));
    assert!(bytes[80..128].iter().all(|byte| *byte == 0));
}

#[test]
fn empty_range_list_publishes_nothing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(ContentIdentity::MerkleV3)).unwrap();
    let id = file(&mut store, b"f", b"payload");
    let before = (store.generation(), store.head());
    let outcome = store.write_file_ranges(id, &[(4, b"".as_slice())]).unwrap();
    assert_eq!((store.generation(), store.head()), before);
    assert_eq!(outcome.version, store.object_meta(id).unwrap().version);
    assert_eq!(store.read_object(id).unwrap(), b"payload");
}

#[test]
fn a_current_manifest_cache_is_equivalent_to_a_fresh_read() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(ContentIdentity::MerkleV3)).unwrap();
    let id = file(&mut store, b"f", &pattern(8 * 1024));
    let mut cache = ManifestCache {
        content: store.object_meta(id).unwrap().content,
        manifest: store
            .manifest_of(store.object_meta(id).unwrap().content)
            .unwrap(),
    };
    for offset in [0u64, 100, 4_000, 8_000] {
        let outcome = store
            .write_file_cached(id, offset, &[9u8; 32], Some(&cache))
            .unwrap();
        // Refresh exactly as the FUSE handle does.
        cache = ManifestCache {
            content: outcome.content,
            manifest: store.manifest_of(outcome.content).unwrap(),
        };
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes[offset as usize..offset as usize + 32]
                .iter()
                .all(|byte| *byte == 9),
            "cached write at {offset} did not land"
        );
    }
    assert_eq!(store.object_meta(id).unwrap().size, 8 * 1024);
    assert_eq!(store.object_meta(id).unwrap().content, cache.content);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "{report:?}");
}

#[test]
fn a_stale_manifest_cache_is_ignored_not_trusted() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(ContentIdentity::MerkleV3)).unwrap();
    let id = file(&mut store, b"f", &pattern(4 * 1024));
    let stale = ManifestCache {
        content: store.object_meta(id).unwrap().content,
        manifest: store
            .manifest_of(store.object_meta(id).unwrap().content)
            .unwrap(),
    };
    // Change the object so the cached content id no longer names it.
    store.truncate(id, 2 * 1024).unwrap();
    let outcome = store
        .write_file_cached(id, 0, b"fresh", Some(&stale))
        .unwrap();
    let bytes = store.read_object(id).unwrap();
    assert_eq!(&bytes[..5], b"fresh");
    assert_eq!(bytes.len(), 2 * 1024);
    assert_ne!(outcome.content, stale.content);
    assert!(store.verify(true).unwrap().ok);
}
