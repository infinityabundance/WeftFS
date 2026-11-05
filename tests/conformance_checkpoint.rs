//! Replay the frozen `weft.checkpoint.v1` identity vectors.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use weftfs::checkpoint::CheckpointManifest;
use weftfs::Hash32;

fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/checkpoint/v1")
}

#[test]
fn checkpoint_identity_vectors() {
    let dir = vector_dir();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();
    files.sort();
    assert!(files.len() >= 5, "expected the checkpoint vector set");

    let mut valid = 0;
    let mut rejected = 0;
    for path in files {
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let vector: Json =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let name = vector["name"].as_str().unwrap_or("?");

        if let Some(expected) = vector["expect_error"].as_str() {
            let bytes = hex::decode(vector["record_hex"].as_str().expect("record_hex"))
                .expect("record hex");
            let error =
                CheckpointManifest::decode(&bytes).expect_err(&format!("{name} must be rejected"));
            assert_eq!(error.class().as_str(), expected, "{name}: {error}");
            rejected += 1;
            continue;
        }

        let bytes =
            hex::decode(vector["canonical_hex"].as_str().expect("canonical_hex")).expect("hex");
        let manifest = CheckpointManifest::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            manifest.canonical_bytes().expect("canonical bytes"),
            bytes,
            "{name} canonical bytes"
        );
        let root = Hash32::from_hex(vector["manifest_root"].as_str().expect("manifest_root"))
            .expect("manifest root hex");
        assert_eq!(
            manifest.root().expect("root").to_hex(),
            root.to_hex(),
            "{name} root"
        );
        valid += 1;
    }
    assert!(valid >= 1, "expected at least one valid checkpoint vector");
    assert!(
        rejected >= 1,
        "expected at least one checkpoint reject vector"
    );
}
