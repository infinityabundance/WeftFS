//! Read and verify the committed Rust-generated `weft.manifest.v4` fixture.
//!
//! The fixture is a real portable store built by
//! `examples/gen_overlay_fixture.rs`; the Go handoff test consumes the same
//! directory. There is no v4 write path, so this is the cross-language handoff
//! contract for the read path.

use std::fs;
use std::path::Path;

use serde_json::Value;

use weftfs::format::RepresentationKind;
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Store};

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/overlay/v1")
}

fn expected() -> Value {
    let bytes = fs::read(fixture_dir().join("EXPECTED.json")).expect("read EXPECTED.json");
    serde_json::from_slice(&bytes).expect("parse EXPECTED.json")
}

#[test]
fn rust_reads_the_committed_overlay_fixture_exactly() {
    let dir = fixture_dir();
    let expected = expected();
    assert_eq!(expected["format"], "weftfs-overlay-fixture-v1");
    let store = Store::open(&dir, OpenMode::ReadOnly).unwrap();

    for object in expected["objects"].as_array().unwrap() {
        let path = object["path"].as_str().unwrap();
        let expected_bytes = object["expected_hex"].as_str().unwrap();
        let expected_root = Hash32::from_hex(object["content_root"].as_str().unwrap()).unwrap();
        let id = store.resolve_path(path).unwrap();
        let record = store.object_meta(id).unwrap();
        assert_eq!(record.content, expected_root, "{path} content root");
        let bytes = store.read_object(id).unwrap();
        assert_eq!(hex::encode(&bytes), expected_bytes, "{path} bytes");
        assert_eq!(
            store.read_range(id, 8, 8).unwrap(),
            &bytes[8..16],
            "{path} range"
        );
    }

    let base_id = Hash32::from_hex(expected["base_manifest"].as_str().unwrap()).unwrap();
    let overlay_id = Hash32::from_hex(expected["overlay_manifest"].as_str().unwrap()).unwrap();
    let nested_id = Hash32::from_hex(expected["nested_manifest"].as_str().unwrap()).unwrap();
    let base = store.manifest_of(base_id).unwrap();
    let overlay = store.manifest_of(overlay_id).unwrap();
    let nested = store.manifest_of(nested_id).unwrap();
    assert_eq!(base.representation, None);
    for (manifest, depth) in [(&overlay, 1u64), (&nested, 2u64)] {
        let rep = manifest.representation.as_ref().unwrap();
        assert_eq!(rep.kind, RepresentationKind::Overlay);
        let overlay_rep = rep.overlay.as_ref().unwrap();
        assert_eq!(overlay_rep.depth, depth);
        assert_eq!(overlay_rep.base_size, 32);
    }
    assert_eq!(
        overlay
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .base,
        base_id
    );
    assert_eq!(
        nested
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .base,
        overlay_id
    );

    let report = store.verify(false).unwrap();
    assert!(report.ok, "shallow verify problems: {:?}", report.problems);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "full verify problems: {:?}", report.problems);
    assert!(report.checked_representations >= 2);

    // A nested range reads only the layer's chunk, and the bytes are exact.
    let nested_object = store.resolve_path("/nested").unwrap();
    assert_eq!(
        store.read_range(nested_object, 24, 8).unwrap(),
        &hex::decode(expected["objects"][2]["expected_hex"].as_str().unwrap()).unwrap()[24..32]
    );
}
