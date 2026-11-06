//! `weft.manifest.v4` overlay read path.
//!
//! The write path does not exist yet, so every fixture here is constructed by
//! hand: chunk blobs and canonical manifests are written through the public
//! recovery paths, and the store reads the result by content root. These tests
//! cover exact bytes, nested layers, adjacent extents, range-read chunk
//! accounting (thread counter and backend byte counter), every fail-closed
//! case, and GC retention/collection through a real v4 object.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::format::{
    ChunkRef, ContentIdentity, Manifest, OverlayExtent, OverlayRepresentation, Representation,
    RepresentationKind,
};
use weftfs::identity::{self, Hash32};
use weftfs::recovery::{manifest_path, object_path};
use weftfs::representation::{
    overlay_chunk_read_count, reset_overlay_chunk_read_count, MAX_OVERLAY_CHAIN_DEPTH,
};
use weftfs::store::{Options, Store};

const CHUNK: u64 = 8;

fn opts(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        content_identity: ContentIdentity::MerkleV3,
        ..Options::default()
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| ((index as u32 * 31 + u32::from(seed)) % 251) as u8)
        .collect()
}

fn put_chunk(root: &Path, bytes: &[u8]) -> ChunkRef {
    let hash = identity::h(identity::CHUNK_DOMAIN, bytes);
    let path = object_path(root, hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    ChunkRef {
        hash,
        len: bytes.len() as u64,
    }
}

fn put_manifest(root: &Path, manifest: &Manifest) -> Hash32 {
    let bytes = manifest.encode().unwrap();
    put_manifest_bytes(root, &bytes)
}

/// Write a manifest without running `Manifest::encode` validation. Used only to
/// build malformed v4 fixtures that a reader must reject; content addressing is
/// still exact.
fn put_manifest_value(root: &Path, value: &Value) -> Hash32 {
    let bytes = weftfs::wcb_util::encode(value).unwrap();
    put_manifest_bytes(root, &bytes)
}

fn put_manifest_bytes(root: &Path, bytes: &[u8]) -> Hash32 {
    let id = identity::manifest_id(bytes);
    let path = manifest_path(root, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    id
}

fn merkle(chunks: &[ChunkRef]) -> Hash32 {
    identity::merkle_root_from_chunks(chunks.iter().map(|chunk| (chunk.len, chunk.hash)))
}

/// A v3 raw base manifest plus its chunk blobs.
fn raw_base(root: &Path, bytes: &[u8], chunk_size: u64) -> Manifest {
    let step = usize::try_from(chunk_size).unwrap();
    let chunks: Vec<ChunkRef> = bytes
        .chunks(step)
        .map(|chunk| put_chunk(root, chunk))
        .collect();
    Manifest {
        size: bytes.len() as u64,
        chunk_size,
        content_hash: merkle(&chunks),
        chunks,
        representation: None,
        identity: ContentIdentity::MerkleV3,
    }
}

fn apply_extents(base: &[ChunkRef], chunk_size: u64, extents: &[OverlayExtent]) -> Vec<ChunkRef> {
    let mut table = base.to_vec();
    for extent in extents {
        let start = usize::try_from(extent.offset / chunk_size).unwrap();
        let end = start + extent.chunks.len();
        if end > table.len() {
            table.splice(start..table.len(), extent.chunks.iter().cloned());
        } else {
            table.splice(start..end, extent.chunks.iter().cloned());
        }
    }
    table
}

/// Build a v4 manifest over `base` from `extents`, returning the manifest and
/// the canonical table it claims.
#[allow(clippy::too_many_arguments)]
fn make_overlay(
    base_id: Hash32,
    base: &Manifest,
    base_table: &[ChunkRef],
    extents: Vec<OverlayExtent>,
    size: u64,
    depth: u64,
    base_overlay_bytes: u64,
) -> (Manifest, Vec<ChunkRef>) {
    let table = apply_extents(base_table, base.chunk_size, &extents);
    let extent_total: u64 = extents.iter().map(|extent| extent.length).sum();
    let overlay_bytes = base_overlay_bytes + extent_total;
    let manifest = Manifest {
        size,
        chunk_size: base.chunk_size,
        chunks: Vec::new(),
        content_hash: merkle(&table),
        representation: Some(Representation {
            kind: RepresentationKind::Overlay,
            frame: None,
            dependencies: Vec::new(),
            strategy: "overlay".to_owned(),
            decoded_size: size,
            tensor: None,
            overlay: Some(OverlayRepresentation {
                base: base_id,
                base_content_root: base.content_hash,
                base_size: base.size,
                depth,
                overlay_bytes,
                extents,
            }),
        }),
        identity: ContentIdentity::MerkleV3,
    };
    (manifest, table)
}

fn expect_read_class(store: &Store, id: Hash32, class: &str) {
    let err = store
        .read_content(id, 0, 1)
        .expect_err("overlay read unexpectedly succeeded");
    assert_eq!(err.class_name(), class, "unexpected error: {err}");
}

#[test]
fn v4_overlay_round_trips_exact_bytes_and_full_read_verifies() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 1);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);

    let replacement = pattern(8, 2);
    let new_chunk = put_chunk(&root, &replacement);
    let extent = OverlayExtent {
        offset: CHUNK,
        length: CHUNK,
        chunks: vec![new_chunk],
    };
    let (overlay, table) = make_overlay(base_id, &base, &base.chunks, vec![extent], 32, 1, 0);
    let overlay_id = put_manifest(&root, &overlay);
    assert_eq!(overlay.content_hash, merkle(&table));

    let mut expected = base_bytes.clone();
    expected[8..16].copy_from_slice(&replacement);
    assert_eq!(store.read_content(overlay_id, 0, 32).unwrap(), expected);
    assert_eq!(store.read_content(overlay_id, 8, 8).unwrap(), replacement);
    assert_eq!(
        store.read_content(overlay_id, 0, 8).unwrap(),
        &base_bytes[0..8]
    );
    assert!(store.read_content(overlay_id, 32, 8).unwrap().is_empty());
    assert_eq!(
        store.read_content(overlay_id, 9, 100).unwrap(),
        &expected[9..32]
    );
}

#[test]
fn range_reads_fetch_only_intersecting_chunks() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(64, 3);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);
    let new_chunk = put_chunk(&root, &pattern(8, 9));
    let extent = OverlayExtent {
        offset: 40,
        length: 8,
        chunks: vec![new_chunk],
    };
    let (overlay, _) = make_overlay(base_id, &base, &base.chunks, vec![extent], 64, 1, 0);
    let overlay_id = put_manifest(&root, &overlay);
    let overlay_manifest_len = fs::metadata(manifest_path(&root, overlay_id))
        .unwrap()
        .len();

    // Warm the advisory table cache. A cold read also fetches the base manifest.
    store.read_content(overlay_id, 0, 1).unwrap();
    let before = store.backend_stats().bytes_read;

    reset_overlay_chunk_read_count();
    let bytes = store.read_content(overlay_id, 40, 8).unwrap();
    assert_eq!(bytes, pattern(8, 9));
    assert_eq!(
        overlay_chunk_read_count(),
        1,
        "a one-chunk range must fetch exactly one content chunk"
    );
    let read_bytes = store.backend_stats().bytes_read - before;
    assert_eq!(
        read_bytes,
        overlay_manifest_len + 8,
        "range read should fetch the overlay manifest plus one 8-byte chunk"
    );

    reset_overlay_chunk_read_count();
    let two = store.read_content(overlay_id, 32, 16).unwrap();
    assert_eq!(two, [&base_bytes[32..40], &pattern(8, 9)].concat());
    assert_eq!(overlay_chunk_read_count(), 2);

    reset_overlay_chunk_read_count();
    let full = store.read_content(overlay_id, 0, 64).unwrap();
    assert_eq!(
        overlay_chunk_read_count(),
        8,
        "full read touches every chunk"
    );
    let mut expected = base_bytes.clone();
    expected[40..48].copy_from_slice(&pattern(8, 9));
    assert_eq!(full, expected);
}

#[test]
fn nested_overlays_resolve_in_order_and_keep_old_versions_readable() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 4);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);

    let first_chunk = put_chunk(&root, &pattern(8, 5));
    let (first, first_table) = make_overlay(
        base_id,
        &base,
        &base.chunks,
        vec![OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![first_chunk],
        }],
        32,
        1,
        0,
    );
    let first_id = put_manifest(&root, &first);

    let second_chunk = put_chunk(&root, &pattern(8, 6));
    let (second, second_table) = make_overlay(
        first_id,
        &first,
        &first_table,
        vec![OverlayExtent {
            offset: 24,
            length: 8,
            chunks: vec![second_chunk],
        }],
        32,
        2,
        first
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .overlay_bytes,
    );
    let second_id = put_manifest(&root, &second);

    let mut first_expected = base_bytes.clone();
    first_expected[0..8].copy_from_slice(&pattern(8, 5));
    let mut second_expected = first_expected.clone();
    second_expected[24..32].copy_from_slice(&pattern(8, 6));
    assert_eq!(second.content_hash, merkle(&second_table));

    assert_eq!(store.read_content(first_id, 0, 32).unwrap(), first_expected);
    assert_eq!(
        store.read_content(second_id, 0, 32).unwrap(),
        second_expected
    );
    assert_eq!(store.read_content(second_id, 0, 8).unwrap(), pattern(8, 5));
    assert_eq!(store.read_content(second_id, 24, 8).unwrap(), pattern(8, 6));

    reset_overlay_chunk_read_count();
    store.read_content(second_id, 24, 8).unwrap();
    assert_eq!(overlay_chunk_read_count(), 1);
}

#[test]
fn adjacent_non_overlapping_extents_in_one_layer_are_exact() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 7);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);
    let first = put_chunk(&root, &pattern(8, 8));
    let second = put_chunk(&root, &pattern(8, 9));
    let (overlay, _) = make_overlay(
        base_id,
        &base,
        &base.chunks,
        vec![
            OverlayExtent {
                offset: 0,
                length: 8,
                chunks: vec![first],
            },
            OverlayExtent {
                offset: 8,
                length: 8,
                chunks: vec![second],
            },
        ],
        32,
        1,
        0,
    );
    let overlay_id = put_manifest(&root, &overlay);

    let mut expected = base_bytes;
    expected[0..8].copy_from_slice(&pattern(8, 8));
    expected[8..16].copy_from_slice(&pattern(8, 9));
    assert_eq!(store.read_content(overlay_id, 0, 32).unwrap(), expected);

    reset_overlay_chunk_read_count();
    store.read_content(overlay_id, 0, 8).unwrap();
    assert_eq!(overlay_chunk_read_count(), 1);
    reset_overlay_chunk_read_count();
    store.read_content(overlay_id, 0, 16).unwrap();
    assert_eq!(overlay_chunk_read_count(), 2);
}

#[test]
fn overlay_fails_closed_for_broken_base_chains_and_accounting() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 10);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);
    let chunk = put_chunk(&root, &pattern(8, 11));
    let extent = OverlayExtent {
        offset: 0,
        length: 8,
        chunks: vec![chunk.clone()],
    };
    let (good, _) = make_overlay(base_id, &base, &base.chunks, vec![extent], 32, 1, 0);

    // Missing base manifest.
    let mut missing = good.clone();
    {
        let overlay = missing
            .representation
            .as_mut()
            .unwrap()
            .overlay
            .as_mut()
            .unwrap();
        overlay.base = identity::h(identity::MANIFEST_DOMAIN, b"no such manifest");
    }
    expect_read_class(&store, put_manifest(&root, &missing), "Corrupt");

    // Base content root mismatch.
    let mut wrong_root = good.clone();
    wrong_root
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .base_content_root = identity::h(identity::MANIFEST_DOMAIN, b"other root");
    expect_read_class(&store, put_manifest(&root, &wrong_root), "Corrupt");

    // Base size mismatch.
    let mut wrong_size = good.clone();
    wrong_size
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .base_size = 7;
    expect_read_class(&store, put_manifest(&root, &wrong_size), "Corrupt");

    // Depth disagreement: a v3 base must be depth 1.
    let mut wrong_depth = good.clone();
    wrong_depth
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .depth = 2;
    expect_read_class(&store, put_manifest(&root, &wrong_depth), "Corrupt");

    // Chain byte accounting must be exact, not merely >=.
    let mut wrong_bytes = good.clone();
    wrong_bytes
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .overlay_bytes = 16;
    expect_read_class(&store, put_manifest(&root, &wrong_bytes), "Corrupt");

    // The base's chunk grid must agree with the overlay's.
    let mut wide = good.clone();
    wide.chunk_size = 16;
    wide.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .overlay_bytes = 16;
    wide.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![OverlayExtent {
        offset: 0,
        length: 16,
        chunks: vec![put_chunk(&root, &pattern(16, 12))],
    }];
    expect_read_class(&store, put_manifest(&root, &wide), "Corrupt");

    // A v2 whole-file-identity base is not composable.
    let v2 = Manifest {
        size: 32,
        chunk_size: CHUNK,
        chunks: base.chunks.clone(),
        content_hash: identity::content_hash(&base_bytes),
        representation: Some(Representation {
            kind: RepresentationKind::Raw,
            frame: None,
            dependencies: Vec::new(),
            strategy: "raw".to_owned(),
            decoded_size: 32,
            tensor: None,
            overlay: None,
        }),
        identity: ContentIdentity::WholeFileV1,
    };
    let v2_id = put_manifest(&root, &v2);
    let (over_v2, _) = make_overlay(
        v2_id,
        &v2,
        &v2.chunks,
        vec![OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![chunk],
        }],
        32,
        1,
        0,
    );
    expect_read_class(&store, put_manifest(&root, &over_v2), "Corrupt");
}

#[test]
fn overlay_format_rejects_are_typed_before_any_chunk_is_read() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 13);
    let base = raw_base(&root, &base_bytes, CHUNK);
    let base_id = put_manifest(&root, &base);
    let chunk = put_chunk(&root, &pattern(8, 14));
    let (good, _) = make_overlay(
        base_id,
        &base,
        &base.chunks,
        vec![OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![chunk],
        }],
        32,
        1,
        0,
    );

    // Past EOF.
    let mut past_eof = good.clone();
    past_eof
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![OverlayExtent {
        offset: 24,
        length: 16,
        chunks: vec![put_chunk(&root, &pattern(16, 15))],
    }];
    expect_read_class(
        &store,
        put_manifest_value(&root, &past_eof.to_value()),
        "Corrupt",
    );

    // Chunk count disagrees with the declared length.
    let mut wrong_count = good.clone();
    wrong_count
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .chunks = vec![
        put_chunk(&root, &pattern(8, 16)),
        put_chunk(&root, &pattern(8, 17)),
    ];
    expect_read_class(
        &store,
        put_manifest_value(&root, &wrong_count.to_value()),
        "Corrupt",
    );

    // Chunk length disagrees with the grid.
    let mut wrong_len = good.clone();
    wrong_len
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .chunks = vec![put_chunk(&root, &pattern(4, 18))];
    expect_read_class(
        &store,
        put_manifest_value(&root, &wrong_len.to_value()),
        "Corrupt",
    );

    // Unaligned offset.
    let mut unaligned = good.clone();
    unaligned
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .offset = 4;
    expect_read_class(
        &store,
        put_manifest_value(&root, &unaligned.to_value()),
        "Malformed",
    );

    // Zero-length extent.
    let mut zero = good.clone();
    zero.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .length = 0;
    expect_read_class(
        &store,
        put_manifest_value(&root, &zero.to_value()),
        "Malformed",
    );

    // Unsorted and overlapping extents in one layer.
    let mut unordered = good.clone();
    unordered
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![
        OverlayExtent {
            offset: 8,
            length: 8,
            chunks: vec![put_chunk(&root, &pattern(8, 19))],
        },
        OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![put_chunk(&root, &pattern(8, 20))],
        },
    ];
    expect_read_class(
        &store,
        put_manifest_value(&root, &unordered.to_value()),
        "Malformed",
    );
    let mut overlapping = good.clone();
    overlapping
        .representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![
        OverlayExtent {
            offset: 0,
            length: 16,
            chunks: vec![
                put_chunk(&root, &pattern(8, 21)),
                put_chunk(&root, &pattern(8, 22)),
            ],
        },
        OverlayExtent {
            offset: 8,
            length: 8,
            chunks: vec![put_chunk(&root, &pattern(8, 23))],
        },
    ];
    expect_read_class(
        &store,
        put_manifest_value(&root, &overlapping.to_value()),
        "Malformed",
    );

    // Zero-length chunk entry.
    let mut zero_chunk = good.to_value();
    if let Value::Map(map) = &mut zero_chunk {
        if let Some(Value::Map(rep)) = map.get_mut("representation") {
            if let Some(Value::Array(extents)) = rep.get_mut("extents") {
                if let Value::Map(extent) = &mut extents[0] {
                    if let Some(Value::Array(chunks)) = extent.get_mut("chunks") {
                        if let Value::Map(chunk_map) = &mut chunks[0] {
                            chunk_map.insert("len".to_owned(), Value::U64(0));
                        }
                    }
                }
            }
        }
    }
    expect_read_class(&store, put_manifest_value(&root, &zero_chunk), "Corrupt");

    // A top-level chunk table is not legal in v4.
    let mut with_chunks = good.to_value();
    if let Value::Map(map) = &mut with_chunks {
        map.insert("chunks".to_owned(), Value::Array(Vec::new()));
    }
    expect_read_class(&store, put_manifest_value(&root, &with_chunks), "Malformed");

    // Missing representation.
    let mut no_rep = good.to_value();
    if let Value::Map(map) = &mut no_rep {
        map.remove("representation");
    }
    expect_read_class(&store, put_manifest_value(&root, &no_rep), "Malformed");

    // A v4 manifest may not carry a raw representation.
    let raw = Representation {
        kind: RepresentationKind::Raw,
        frame: None,
        dependencies: Vec::new(),
        strategy: "raw".to_owned(),
        decoded_size: 32,
        tensor: None,
        overlay: None,
    };
    let mut raw_rep = good.to_value();
    if let Value::Map(map) = &mut raw_rep {
        map.insert("representation".to_owned(), raw.to_value());
    }
    expect_read_class(&store, put_manifest_value(&root, &raw_rep), "Corrupt");

    // kind:"overlay" under the v2 schema is not legal.
    let mut v2_overlay = good.to_value();
    if let Value::Map(map) = &mut v2_overlay {
        map.insert(
            "schema".to_owned(),
            Value::Str("weft.manifest.v2".to_owned()),
        );
        map.insert(
            "content_hash".to_owned(),
            weftfs::wcb_util::hash_value(identity::content_hash(&base_bytes)),
        );
        map.insert("chunks".to_owned(), Value::Array(Vec::new()));
    }
    expect_read_class(&store, put_manifest_value(&root, &v2_overlay), "Malformed");
}

#[test]
fn overlay_chain_depth_is_bounded() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root = store.root().to_path_buf();

    let base_bytes = pattern(32, 24);
    let mut manifest = raw_base(&root, &base_bytes, CHUNK);
    let mut manifest_id = put_manifest(&root, &manifest);
    let mut table = manifest.chunks.clone();
    let mut base_overlay_bytes = 0u64;
    let mut deepest = Hash32::ZERO;

    for depth in 1..=MAX_OVERLAY_CHAIN_DEPTH + 1 {
        let chunk = put_chunk(&root, &pattern(8, (depth % 251) as u8));
        let (next, next_table) = make_overlay(
            manifest_id,
            &manifest,
            &table,
            vec![OverlayExtent {
                offset: 0,
                length: 8,
                chunks: vec![chunk],
            }],
            32,
            depth,
            base_overlay_bytes,
        );
        base_overlay_bytes = next
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .overlay_bytes;
        table = next_table;
        manifest = next;
        // The deepest link itself is beyond the reader's hard bound, so encode
        // would reject it. Write the canonical value to prove the reader checks
        // the declared depth before resolving anything.
        if depth <= MAX_OVERLAY_CHAIN_DEPTH {
            manifest_id = put_manifest(&root, &manifest);
        } else {
            manifest_id = put_manifest_value(&root, &manifest.to_value());
        }
        deepest = manifest_id;
    }

    expect_read_class(&store, deepest, "LimitExceeded");
}

#[test]
fn gc_retains_overlay_base_chains_and_collects_unreferenced_chunks() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root_path = store.root().to_path_buf();

    let base_bytes = pattern(32, 25);
    let base = raw_base(&root_path, &base_bytes, CHUNK);
    let base_id = put_manifest(&root_path, &base);

    let first_chunk = put_chunk(&root_path, &pattern(8, 26));
    let (first, first_table) = make_overlay(
        base_id,
        &base,
        &base.chunks,
        vec![OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![first_chunk.clone()],
        }],
        32,
        1,
        0,
    );
    let first_id = put_manifest(&root_path, &first);

    let second_chunk = put_chunk(&root_path, &pattern(8, 27));
    let (second, _) = make_overlay(
        first_id,
        &first,
        &first_table,
        vec![OverlayExtent {
            offset: 24,
            length: 8,
            chunks: vec![second_chunk.clone()],
        }],
        32,
        2,
        first
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .overlay_bytes,
    );
    let second_id = put_manifest(&root_path, &second);

    let unreferenced = put_chunk(&root_path, b"never referenced");
    let root = store.resolve_path("/").unwrap();
    let first_object = store
        .create_file(root, b"first", b"seed-first", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let second_object = store
        .create_file(
            root,
            b"second",
            b"seed-second",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .publish_manifest_for_test(first_object, first_id, 32)
        .unwrap();
    store
        .publish_manifest_for_test(second_object, second_id, 32)
        .unwrap();

    let mut first_expected = base_bytes.clone();
    first_expected[0..8].copy_from_slice(&pattern(8, 26));
    let mut second_expected = first_expected.clone();
    second_expected[24..32].copy_from_slice(&pattern(8, 27));
    assert_eq!(store.read_object(second_object).unwrap(), second_expected);

    store.gc(true).unwrap();
    assert!(manifest_path(&root_path, base_id).exists());
    assert!(manifest_path(&root_path, first_id).exists());
    assert!(manifest_path(&root_path, second_id).exists());
    assert!(object_path(&root_path, first_chunk.hash).exists());
    assert!(object_path(&root_path, second_chunk.hash).exists());
    assert!(
        !object_path(&root_path, unreferenced.hash).exists(),
        "an unreferenced chunk must be collectable"
    );
    assert_eq!(store.read_object(first_object).unwrap(), first_expected);
    assert_eq!(store.read_object(second_object).unwrap(), second_expected);

    // Drop the outer object. The first overlay is now reachable only as the
    // second object's immutable base, so its manifest and chunks stay live.
    store.unlink(root, b"first").unwrap();
    store.gc(true).unwrap();
    assert!(manifest_path(&root_path, base_id).exists());
    assert!(manifest_path(&root_path, first_id).exists());
    assert!(manifest_path(&root_path, second_id).exists());
    assert!(object_path(&root_path, first_chunk.hash).exists());
    assert!(object_path(&root_path, second_chunk.hash).exists());
    assert_eq!(store.read_object(second_object).unwrap(), second_expected);

    // Drop the last root: the whole chain becomes unreachable and collectable.
    store.unlink(root, b"second").unwrap();
    store.gc(true).unwrap();
    assert!(!manifest_path(&root_path, base_id).exists());
    assert!(!manifest_path(&root_path, first_id).exists());
    assert!(!manifest_path(&root_path, second_id).exists());
    assert!(!object_path(&root_path, first_chunk.hash).exists());
    assert!(!object_path(&root_path, second_chunk.hash).exists());
}

#[test]
fn verify_resolves_overlay_chains_and_reports_corruption() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(CHUNK)).unwrap();
    let root_path = store.root().to_path_buf();

    let base_bytes = pattern(32, 28);
    let base = raw_base(&root_path, &base_bytes, CHUNK);
    let base_id = put_manifest(&root_path, &base);
    let chunk = put_chunk(&root_path, &pattern(8, 29));
    let (overlay, _) = make_overlay(
        base_id,
        &base,
        &base.chunks,
        vec![OverlayExtent {
            offset: 0,
            length: 8,
            chunks: vec![chunk],
        }],
        32,
        1,
        0,
    );
    let overlay_id = put_manifest(&root_path, &overlay);
    let root = store.resolve_path("/").unwrap();
    let object = store
        .create_file(root, b"file", b"seed", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    store
        .publish_manifest_for_test(object, overlay_id, 32)
        .unwrap();

    let report = store.verify(false).unwrap();
    assert!(report.ok, "shallow verify problems: {:?}", report.problems);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "full verify problems: {:?}", report.problems);
    assert!(report.checked_representations >= 1);

    // Removing the base manifest turns the same object into a reported problem,
    // never a panic or a partial read. Reopen so the advisory resolved-table
    // cache cannot hide the deletion.
    drop(store);
    fs::remove_file(manifest_path(&root_path, base_id)).unwrap();
    let store = Store::open(dir.path(), weftfs::store::OpenMode::ReadOnly).unwrap();
    let err = store.read_object(object).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt");
    let report = store.verify(false).unwrap();
    assert!(!report.ok);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.detail.contains("overlay")),
        "verify did not report the broken overlay: {:?}",
        report.problems
    );
}
