//! Generate the frozen `weft.checkpoint.v1` identity vectors.
//!
//! Run inside the rust court:
//!
//! ```sh
//! scripts/docker-court.sh rust \
//!   "cargo run --release --quiet --example gen_checkpoint_vector -- conformance/checkpoint/v1"
//! ```
//!
//! The vectors freeze canonical WCB bytes and the manifest root
//! `H("weft/checkpoint/manifest/v1", canonical_bytes)`. They contain no store
//! and no weights: only the identity record and invalid variants.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};
use weftcodec::wcb::Value;
use weftfs::checkpoint::{
    CheckpointManifest, CheckpointShard, CheckpointTensor, CHECKPOINT_SCHEMA,
};
use weftfs::Hash32;

fn hash(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

fn manifest() -> CheckpointManifest {
    CheckpointManifest {
        name: "conformance-bf16-1shard".to_owned(),
        container: "safetensors".to_owned(),
        complete: false,
        element_bytes: 8,
        source_header: None,
        source_file_bytes: None,
        source_sha256: None,
        tensors: vec![CheckpointTensor {
            name: "model.embed.weight".to_owned(),
            dtype: "bf16".to_owned(),
            shape: vec![2, 2],
            logical_root: hash(0x11),
            content_root: hash(0x22),
            element_bytes: 8,
            source_offset: 0,
            shards: vec![CheckpointShard {
                axis: 0,
                shards: 1,
                index: 0,
                object_name: "t00000.s000".to_owned(),
                object_id: hash(0x33),
                content_root: hash(0x44),
                logical_root: hash(0x55),
                byte_len: 8,
            }],
        }],
    }
}

fn map_mut(value: &mut Value) -> &mut BTreeMap<String, Value> {
    match value {
        Value::Map(map) => map,
        _ => panic!("expected a WCB map"),
    }
}

fn tensor_map_mut(value: &mut Value) -> &mut BTreeMap<String, Value> {
    map_mut(value)
}

fn tensors_mut(value: &mut Value) -> &mut Vec<Value> {
    match value {
        Value::Map(map) => match map.get_mut("tensors") {
            Some(Value::Array(items)) => items,
            _ => panic!("expected a tensors array"),
        },
        _ => panic!("expected a WCB map"),
    }
}

fn write_vector(path: &Path, vector: &Json) {
    let text = serde_json::to_string_pretty(vector).expect("serialize vector");
    fs::write(path, format!("{text}\n")).expect("write vector");
}

fn valid_vector(manifest: &CheckpointManifest) -> Json {
    let canonical = manifest.canonical_bytes().expect("canonical bytes");
    let root = manifest.root().expect("manifest root");
    json!({
        "schema": "weft.conformance.checkpoint.v1",
        "name": "001_manifest_bf16_1shard",
        "description": "Canonical weft.checkpoint.v1 manifest: one bf16 2x2 tensor stored as one shard. The referenced shard bytes are not part of this identity vector.",
        "canonical_hex": hex::encode(&canonical),
        "manifest_root": root.to_hex(),
        "manifest_name": manifest.name,
        "container": manifest.container,
        "complete": manifest.complete,
        "element_bytes": manifest.element_bytes,
        "tensor_count": manifest.tensors.len(),
        "expect": { "ok": true }
    })
}

fn reject_vector(name: &str, description: &str, value: &Value, expect_error: &str) -> Json {
    let bytes = weftfs::wcb_util::encode(value).expect("encode reject record");
    // The generator must not emit a record that the strict decoder accepts.
    let error = CheckpointManifest::decode(&bytes).expect_err("reject vector decoded");
    assert_eq!(
        error.class().as_str(),
        expect_error,
        "{name} produced the wrong class"
    );
    json!({
        "schema": "weft.conformance.checkpoint.v1",
        "name": name,
        "description": description,
        "record_hex": hex::encode(&bytes),
        "expect_error": expect_error,
    })
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("conformance/checkpoint/v1"));
    fs::create_dir_all(&dir).expect("create checkpoint vector directory");
    let manifest = manifest();
    write_vector(
        &dir.join("001_manifest_bf16_1shard.json"),
        &valid_vector(&manifest),
    );

    let mut unknown_schema = manifest.to_value();
    map_mut(&mut unknown_schema).insert(
        "schema".to_owned(),
        Value::Str("weft.checkpoint.v2".to_owned()),
    );
    write_vector(
        &dir.join("101_unknown_schema.json"),
        &reject_vector(
            "101_unknown_schema",
            "The schema string is not weft.checkpoint.v1.",
            &unknown_schema,
            "UnknownSchema",
        ),
    );

    let mut wrong_total = manifest.to_value();
    if let Value::Map(map) = &mut wrong_total {
        assert_eq!(
            map.get("schema"),
            Some(&Value::Str(CHECKPOINT_SCHEMA.to_owned()))
        );
        map.insert("element_bytes".to_owned(), Value::U64(9));
    }
    write_vector(
        &dir.join("102_element_bytes_mismatch.json"),
        &reject_vector(
            "102_element_bytes_mismatch",
            "Declared element_bytes does not equal the tensor sum.",
            &wrong_total,
            "Corrupt",
        ),
    );

    let mut duplicate = manifest.to_value();
    let items = tensors_mut(&mut duplicate);
    let first = items[0].clone();
    items.push(first);
    write_vector(
        &dir.join("103_duplicate_tensor.json"),
        &reject_vector(
            "103_duplicate_tensor",
            "Two tensors share the same name.",
            &duplicate,
            "Malformed",
        ),
    );

    let mut shard_len = manifest.to_value();
    let items = tensors_mut(&mut shard_len);
    let shards = match tensor_map_mut(&mut items[0]).get_mut("shards") {
        Some(Value::Array(shards)) => shards,
        _ => panic!("expected a shards array"),
    };
    match &mut shards[0] {
        Value::Map(shard) => {
            shard.insert("byte_len".to_owned(), Value::U64(9));
        }
        _ => panic!("expected a shard map"),
    }
    write_vector(
        &dir.join("104_shard_byte_len_mismatch.json"),
        &reject_vector(
            "104_shard_byte_len_mismatch",
            "A one-shard tensor whose shard byte_len is not element_bytes.",
            &shard_len,
            "Corrupt",
        ),
    );

    eprintln!("wrote checkpoint vectors to {}", dir.display());
}
