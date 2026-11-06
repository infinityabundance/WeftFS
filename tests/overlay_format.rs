//! `weft.manifest.v4` immutable-overlay format tests.
//!
//! This file covers only the additive format: encoding, parsing, and structural
//! validation. The read path (base-chain resolution, range reads, GC) is tested
//! in `overlay_read.rs`. The write path does not exist yet; every v4 manifest
//! here is built by hand.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use weftfs::format::{
    ChunkRef, ContentIdentity, Manifest, OverlayExtent, OverlayRepresentation, Representation,
    RepresentationKind, TensorRepresentation, MANIFEST_SCHEMA, MANIFEST_SCHEMA_V2,
    MANIFEST_SCHEMA_V3, MANIFEST_SCHEMA_V4,
};
use weftfs::identity::{self, Hash32};
use weftfs::representation::{TensorElementDtype, MAX_OVERLAY_CHAIN_DEPTH};

const CHUNK: u64 = 8;

fn chunk_ref(bytes: &[u8]) -> ChunkRef {
    ChunkRef {
        hash: identity::h(identity::CHUNK_DOMAIN, bytes),
        len: bytes.len() as u64,
    }
}

fn chunk_refs(bytes: &[u8], chunk_size: u64) -> Vec<ChunkRef> {
    let step = usize::try_from(chunk_size).unwrap();
    bytes.chunks(step).map(chunk_ref).collect()
}

fn merkle(chunks: &[ChunkRef]) -> Hash32 {
    identity::merkle_root_from_chunks(chunks.iter().map(|chunk| (chunk.len, chunk.hash)))
}

fn base_bytes() -> Vec<u8> {
    (0..32u8).collect()
}

fn base_manifest() -> Manifest {
    let bytes = base_bytes();
    let chunks = chunk_refs(&bytes, CHUNK);
    Manifest {
        size: bytes.len() as u64,
        chunk_size: CHUNK,
        content_hash: merkle(&chunks),
        chunks,
        representation: None,
        identity: ContentIdentity::MerkleV3,
    }
}

/// One valid v4 overlay over a four-chunk v3 base: the second 8-byte chunk is
/// replaced, so the canonical table is `base[0], new, base[2], base[3]`.
fn sample_overlay() -> (Manifest, Manifest, Vec<ChunkRef>) {
    let base = base_manifest();
    let base_id = identity::manifest_id(&base.encode().unwrap());
    let mut table = base.chunks.clone();
    let new = chunk_ref(b"NEWVALUE");
    table[1] = new.clone();
    let extent = OverlayExtent {
        offset: CHUNK,
        length: CHUNK,
        chunks: vec![new],
    };
    let overlay = Manifest {
        size: base.size,
        chunk_size: CHUNK,
        chunks: Vec::new(),
        content_hash: merkle(&table),
        representation: Some(Representation {
            kind: RepresentationKind::Overlay,
            frame: None,
            dependencies: Vec::new(),
            strategy: "overlay".to_owned(),
            decoded_size: base.size,
            tensor: None,
            overlay: Some(OverlayRepresentation {
                base: base_id,
                base_content_root: base.content_hash,
                base_size: base.size,
                depth: 1,
                overlay_bytes: CHUNK,
                extents: vec![extent],
            }),
        }),
        identity: ContentIdentity::MerkleV3,
    };
    (base, overlay, table)
}

fn map_of(manifest: &Manifest) -> BTreeMap<String, Value> {
    match manifest.to_value() {
        Value::Map(map) => map,
        other => panic!("manifest did not encode to a map: {other:?}"),
    }
}

fn chunk_value(chunk: &ChunkRef) -> Value {
    weftfs::wcb_util::map([
        ("hash", weftfs::wcb_util::hash_value(chunk.hash)),
        ("len", Value::U64(chunk.len)),
    ])
}

fn extent_value(offset: u64, length: u64, chunks: &[ChunkRef]) -> Value {
    weftfs::wcb_util::map([
        ("offset", Value::U64(offset)),
        ("length", Value::U64(length)),
        (
            "chunks",
            Value::Array(chunks.iter().map(chunk_value).collect()),
        ),
    ])
}

fn expect_encode_class(manifest: &Manifest, class: &str) {
    let err = manifest
        .encode()
        .expect_err("manifest unexpectedly encoded");
    assert_eq!(err.class_name(), class, "unexpected error: {err}");
}

fn expect_parse_class(value: &Value, class: &str) {
    let err = Manifest::from_wcb(value).expect_err("manifest unexpectedly parsed");
    assert_eq!(err.class_name(), class, "unexpected error: {err}");
}

#[test]
fn v4_overlay_round_trips_without_a_top_level_chunk_table() {
    let (base, overlay, table) = sample_overlay();
    let bytes = overlay.encode().unwrap();
    let value = weftfs::wcb_util::decode(&bytes).unwrap();
    let map = match &value {
        Value::Map(map) => map,
        other => panic!("v4 manifest is not a WCB map: {other:?}"),
    };
    assert_eq!(
        map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA_V4.to_owned()))
    );
    assert!(
        !map.contains_key("chunks"),
        "v4 has no top-level chunk table: {map:?}"
    );
    assert_eq!(
        map.get("content_identity").cloned(),
        Some(Value::Str("merkle-v1".to_owned()))
    );
    assert_eq!(overlay.content_hash, merkle(&table));

    let decoded = Manifest::from_wcb(&value).unwrap();
    assert_eq!(decoded, overlay);
    assert_eq!(decoded.identity, ContentIdentity::MerkleV3);
    assert!(decoded.chunks.is_empty());
    assert_eq!(
        decoded.encode().unwrap(),
        bytes,
        "canonical re-encode changed bytes"
    );

    // The base's own v3 bytes are untouched by building an overlay over it.
    assert_eq!(base.identity, ContentIdentity::MerkleV3);
    let base_value = base.to_value();
    assert_eq!(
        Manifest::from_wcb(&base_value).unwrap().encode().unwrap(),
        base.encode().unwrap()
    );
}

#[test]
fn v4_accepts_adjacent_non_overlapping_extents() {
    let (base, overlay, _) = sample_overlay();
    let base_id = overlay
        .representation
        .as_ref()
        .unwrap()
        .overlay
        .as_ref()
        .unwrap()
        .base;
    let first = chunk_ref(&[0xAA; 8]);
    let second = chunk_ref(&[0xBB; 8]);
    let mut table = base.chunks.clone();
    table[0] = first.clone();
    table[1] = second.clone();
    let adjacent = Manifest {
        size: base.size,
        chunk_size: CHUNK,
        chunks: Vec::new(),
        content_hash: merkle(&table),
        representation: Some(Representation {
            kind: RepresentationKind::Overlay,
            frame: None,
            dependencies: Vec::new(),
            strategy: "overlay".to_owned(),
            decoded_size: base.size,
            tensor: None,
            overlay: Some(OverlayRepresentation {
                base: base_id,
                base_content_root: base.content_hash,
                base_size: base.size,
                depth: 1,
                overlay_bytes: 2 * CHUNK,
                extents: vec![
                    OverlayExtent {
                        offset: 0,
                        length: CHUNK,
                        chunks: vec![first],
                    },
                    OverlayExtent {
                        offset: CHUNK,
                        length: CHUNK,
                        chunks: vec![second],
                    },
                ],
            }),
        }),
        identity: ContentIdentity::MerkleV3,
    };
    let bytes = adjacent.encode().unwrap();
    let decoded = Manifest::from_wcb(&weftfs::wcb_util::decode(&bytes).unwrap()).unwrap();
    assert_eq!(decoded, adjacent);
    assert_eq!(decoded.encode().unwrap(), bytes);
}

/// v4 is additive: the frozen v1, v2, and v3 writers still emit exactly their
/// own key sets and schema strings, and re-encoding is byte-stable.
#[test]
fn v1_v2_v3_encoding_is_unchanged() {
    let base = base_manifest();
    let bytes = base_bytes();

    let v1 = Manifest {
        size: bytes.len() as u64,
        chunk_size: CHUNK,
        chunks: chunk_refs(&bytes, CHUNK),
        content_hash: identity::content_hash(&bytes),
        representation: None,
        identity: ContentIdentity::WholeFileV1,
    };
    let v1_value = v1.to_value();
    let Value::Map(v1_map) = &v1_value else {
        panic!("v1 is not a map")
    };
    assert_eq!(
        v1_map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA.to_owned()))
    );
    let mut v1_keys: Vec<&str> = v1_map.keys().map(String::as_str).collect();
    v1_keys.sort_unstable();
    assert_eq!(
        v1_keys,
        ["chunk_size", "chunks", "content_hash", "schema", "size"]
    );
    let v1_bytes = v1.encode().unwrap();
    assert_eq!(
        Manifest::from_wcb(&v1_value).unwrap().encode().unwrap(),
        v1_bytes
    );

    let v2 = Manifest {
        representation: Some(Representation {
            kind: RepresentationKind::Raw,
            frame: None,
            dependencies: Vec::new(),
            strategy: "raw".to_owned(),
            decoded_size: bytes.len() as u64,
            tensor: None,
            overlay: None,
        }),
        ..v1.clone()
    };
    let v2_value = v2.to_value();
    let Value::Map(v2_map) = &v2_value else {
        panic!("v2 is not a map")
    };
    assert_eq!(
        v2_map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA_V2.to_owned()))
    );
    let v2_bytes = v2.encode().unwrap();
    assert_eq!(
        Manifest::from_wcb(&v2_value).unwrap().encode().unwrap(),
        v2_bytes
    );

    let v3_value = base.to_value();
    let Value::Map(v3_map) = &v3_value else {
        panic!("v3 is not a map")
    };
    assert_eq!(
        v3_map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA_V3.to_owned()))
    );
    assert!(!v3_map.contains_key("representation"));
    let v3_bytes = base.encode().unwrap();
    assert_eq!(
        Manifest::from_wcb(&v3_value).unwrap().encode().unwrap(),
        v3_bytes
    );

    // The additive change must not leak a v4 key into the older schemas.
    for map in [v1_map, v2_map, v3_map] {
        assert!(!map.contains_key("representation") || !map.contains_key("content_root"));
        assert!(!map.contains_key("base"));
        assert!(!map.contains_key("extents"));
    }
}

#[test]
fn overlay_extent_rejects_bad_grids_and_bounds() {
    let (_, overlay, _) = sample_overlay();
    let good = overlay.representation.clone().unwrap().overlay.unwrap();

    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .offset = 4;
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .length = 0;
    expect_encode_class(&bad, "Malformed");

    // 24 + 16 ends at 40, past the 32-byte file.
    let mut bad = overlay.clone();
    let mut extent = good.extents[0].clone();
    extent.offset = 24;
    extent.length = 16;
    extent.chunks = vec![chunk_ref(&[1; 8]), chunk_ref(&[2; 8])];
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![extent];
    expect_encode_class(&bad, "Corrupt");

    // Declared length needs one chunk, two are listed.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .chunks = vec![chunk_ref(&[1; 8]), chunk_ref(&[2; 8])];
    expect_encode_class(&bad, "Corrupt");

    // One full chunk is 8 bytes; a 4-byte chunk cannot cover it.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents[0]
        .chunks = vec![chunk_ref(&[1; 4])];
    expect_encode_class(&bad, "Corrupt");

    // Overlapping and unsorted extents are rejected inside one layer.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![
        good.extents[0].clone(),
        OverlayExtent {
            offset: 0,
            length: CHUNK,
            chunks: vec![chunk_ref(&[3; 8])],
        },
    ];
    expect_encode_class(&bad, "Malformed");
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = vec![
        OverlayExtent {
            offset: 0,
            length: 2 * CHUNK,
            chunks: vec![chunk_ref(&[3; 8]), chunk_ref(&[4; 8])],
        },
        OverlayExtent {
            offset: CHUNK,
            length: CHUNK,
            chunks: vec![chunk_ref(&[5; 8])],
        },
    ];
    expect_encode_class(&bad, "Malformed");

    // overlay_bytes smaller than the extent total.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .overlay_bytes = CHUNK - 1;
    expect_encode_class(&bad, "Corrupt");

    // No extents at all is not a representation.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .extents = Vec::new();
    expect_encode_class(&bad, "Malformed");

    // Depth 0 and a depth beyond the reader's hard bound.
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .depth = 0;
    expect_encode_class(&bad, "Malformed");
    let mut bad = overlay.clone();
    bad.representation
        .as_mut()
        .unwrap()
        .overlay
        .as_mut()
        .unwrap()
        .depth = MAX_OVERLAY_CHAIN_DEPTH + 1;
    expect_encode_class(&bad, "LimitExceeded");
}

#[test]
fn overlay_struct_validation_is_enforced_before_encoding() {
    let (_, overlay, _) = sample_overlay();

    let mut bad = overlay.clone();
    bad.identity = ContentIdentity::WholeFileV1;
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.chunks = vec![chunk_ref(&[9; 8])];
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.representation.as_mut().unwrap().decoded_size = 0;
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.representation.as_mut().unwrap().frame = Some(Hash32::ZERO);
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.representation.as_mut().unwrap().dependencies = vec![Hash32::ZERO];
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay.clone();
    bad.representation.as_mut().unwrap().tensor = Some(TensorRepresentation {
        tensor_mode: 10,
        dtype: TensorElementDtype::U8,
        shape: vec![1],
        logical_root: Hash32::ZERO,
        layout: Value::Null,
    });
    expect_encode_class(&bad, "Malformed");

    let mut bad = overlay;
    bad.representation.as_mut().unwrap().overlay = None;
    expect_encode_class(&bad, "Malformed");
}

#[test]
fn v4_parser_rejects_malformed_maps_and_values() {
    let (_, overlay, _) = sample_overlay();
    let good = map_of(&overlay);

    // Missing representation.
    let mut map = good.clone();
    map.remove("representation");
    expect_parse_class(&Value::Map(map), "Malformed");

    // A stray top-level chunk table.
    let mut map = good.clone();
    map.insert("chunks".to_owned(), Value::Array(Vec::new()));
    expect_parse_class(&Value::Map(map), "Malformed");

    let rep = match good.get("representation") {
        Some(Value::Map(map)) => map.clone(),
        _ => panic!("representation is not a map"),
    };
    let with_rep = |bad_rep: BTreeMap<String, Value>| {
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        map
    };

    // Zero base ids and roots (inside the representation map).
    for key in ["base", "base_content_root"] {
        let mut bad_rep = rep.clone();
        bad_rep.insert(key.to_owned(), weftfs::wcb_util::hash_value(Hash32::ZERO));
        expect_parse_class(&Value::Map(with_rep(bad_rep)), "Malformed");
    }
    let mut map = good.clone();
    map.insert(
        "content_root".to_owned(),
        weftfs::wcb_util::hash_value(Hash32::ZERO),
    );
    expect_parse_class(&Value::Map(map), "Malformed");

    // Zero overlay_bytes.
    let mut bad_rep = rep.clone();
    bad_rep.insert("overlay_bytes".to_owned(), Value::U64(0));
    expect_parse_class(&Value::Map(with_rep(bad_rep)), "Malformed");

    // Depth 0 / beyond the bound.
    let mut bad_rep = rep.clone();
    bad_rep.insert("depth".to_owned(), Value::U64(0));
    expect_parse_class(&Value::Map(with_rep(bad_rep)), "Malformed");
    let mut bad_rep = rep.clone();
    bad_rep.insert("depth".to_owned(), Value::U64(MAX_OVERLAY_CHAIN_DEPTH + 1));
    expect_parse_class(&Value::Map(with_rep(bad_rep)), "LimitExceeded");

    // Wrong strategy text.
    let mut bad_rep = rep.clone();
    bad_rep.insert("strategy".to_owned(), Value::Str("raw".to_owned()));
    expect_parse_class(&Value::Map(with_rep(bad_rep)), "Malformed");

    // Extent array shape and chunk list shape.
    {
        let mut bad_rep = rep.clone();
        bad_rep.insert("extents".to_owned(), Value::Array(Vec::new()));
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        expect_parse_class(&Value::Map(map), "Malformed");
    }
    {
        let mut bad_rep = rep.clone();
        bad_rep.insert(
            "extents".to_owned(),
            Value::Array(vec![extent_value(0, CHUNK, &[])]),
        );
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        expect_parse_class(&Value::Map(map), "Malformed");
    }
    {
        let mut bad_rep = rep.clone();
        let zero = ChunkRef {
            hash: identity::h(identity::CHUNK_DOMAIN, b"x"),
            len: 0,
        };
        bad_rep.insert(
            "extents".to_owned(),
            Value::Array(vec![extent_value(0, CHUNK, &[zero])]),
        );
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        expect_parse_class(&Value::Map(map), "Corrupt");
    }
    {
        let mut bad_rep = rep.clone();
        let zero_hash = ChunkRef {
            hash: Hash32::ZERO,
            len: CHUNK,
        };
        bad_rep.insert(
            "extents".to_owned(),
            Value::Array(vec![extent_value(0, CHUNK, &[zero_hash])]),
        );
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        expect_parse_class(&Value::Map(map), "Malformed");
    }

    // More extents than the per-manifest bound.
    {
        let mut bad_rep = rep.clone();
        let chunks = vec![chunk_ref(&[7; 8])];
        let extents: Vec<Value> = (0..=weftfs::representation::MAX_OVERLAY_EXTENTS_PER_MANIFEST)
            .map(|_| extent_value(0, CHUNK, &chunks))
            .collect();
        bad_rep.insert("extents".to_owned(), Value::Array(extents));
        let mut map = good.clone();
        map.insert("representation".to_owned(), Value::Map(bad_rep));
        expect_parse_class(&Value::Map(map), "LimitExceeded");
    }

    // A v4 manifest may carry only kind:"overlay".
    {
        let raw = Representation {
            kind: RepresentationKind::Raw,
            frame: None,
            dependencies: Vec::new(),
            strategy: "raw".to_owned(),
            decoded_size: overlay.size,
            tensor: None,
            overlay: None,
        };
        let mut map = good.clone();
        map.insert("representation".to_owned(), raw.to_value());
        expect_parse_class(&Value::Map(map), "Corrupt");
    }

    // kind:"overlay" is not legal in the v2 schema.
    {
        let mut map = good.clone();
        map.insert(
            "schema".to_owned(),
            Value::Str(MANIFEST_SCHEMA_V2.to_owned()),
        );
        map.insert(
            "content_hash".to_owned(),
            weftfs::wcb_util::hash_value(identity::content_hash(&base_bytes())),
        );
        map.insert("chunks".to_owned(), Value::Array(Vec::new()));
        expect_parse_class(&Value::Map(map), "Malformed");
    }
}
