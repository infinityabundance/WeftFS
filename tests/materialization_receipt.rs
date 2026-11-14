//! T059: store-derived materialization receipts and their independent
//! verification.
//!
//! The receipt is derived by re-reading the store; the verifier re-derives it
//! and compares field by field. The adapter receipt binds framework entries to
//! materialization rows and is checked the same way. Every tamper class is
//! refused, including a canonical rewrite that keeps the receipt's own set
//! root consistent.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::materialization::{
    verify_receipt, AdapterEntry, AdapterReceipt, MaterializationReceipt,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

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

fn setup() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let shape = [32u64, 16];
    let bias_shape = [32u64];
    let body = random_bytes(32 * 16 * 2, 0x1234);
    let bias = random_bytes(32 * 2, 0x5678);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: "layer.weight",
                dtype: "bf16",
                shape: &shape,
                data: &body,
            },
            SafetensorsEntry {
                name: "layer.bias",
                dtype: "bf16",
                shape: &bias_shape,
                data: &bias,
            },
        ],
        None,
    )
    .unwrap();
    let source = dir.path().join("src.safetensors");
    fs::write(&source, &file).unwrap();
    store
        .write_checkpoint(
            "receipts",
            &source,
            &CheckpointWriteOptions {
                shards: 4,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    (dir, source, store)
}

fn adapter_for(receipt: &MaterializationReceipt) -> AdapterReceipt {
    let entries: Vec<AdapterEntry> = receipt
        .tensors
        .iter()
        .enumerate()
        .map(|(index, row)| AdapterEntry {
            fqn: format!("state.{}", row.name),
            tensor_name: row.name.clone(),
            dtype: row.dtype.clone(),
            shape: row.shape.clone(),
            bytes: row.bytes,
            sha256: row.sha256,
            content_root: row.content_root,
        })
        .map(|mut entry| {
            if entry.fqn.is_empty() {
                entry.fqn = "state".to_owned();
            }
            entry
        })
        .collect();
    let set_root = AdapterReceipt::set_root(
        &entries
            .iter()
            .map(|entry| {
                (
                    entry.fqn.clone(),
                    entry.tensor_name.clone(),
                    entry.content_root,
                )
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    AdapterReceipt {
        adapter: "dcp".to_owned(),
        checkpoint: receipt.checkpoint.clone(),
        materialization_root: receipt.root().unwrap(),
        manifest_root: receipt.manifest_root,
        producer: "test".to_owned(),
        entries,
        set_root,
        wall_ms: 0,
    }
}

#[test]
fn a_receipt_is_store_derived_and_verifies_independently() {
    let (_dir, _source, store) = setup();
    let receipt = store.materialization_receipt("receipts", "court").unwrap();
    assert_eq!(receipt.replica, "court");
    assert_eq!(receipt.tensors.len(), 2);
    // Sorted by name, so the canonical bytes are deterministic.
    assert_eq!(receipt.tensors[0].name, "layer.bias");
    assert_eq!(receipt.tensors[1].name, "layer.weight");
    for row in &receipt.tensors {
        assert_eq!(row.verification, "exact");
        assert!(!row.source_objects.is_empty());
        assert_eq!(row.bytes, row.shape.iter().product::<u64>() * 2);
        // The shard objects are recorded with their recomputed roots.
        for object in &row.source_objects {
            assert_ne!(object.object_id, weftfs::identity::Hash32::ZERO);
            assert_ne!(object.content_root, weftfs::identity::Hash32::ZERO);
        }
    }
    let bytes = receipt.encode().unwrap();
    let decoded = MaterializationReceipt::decode(&bytes).unwrap();
    assert_eq!(decoded, receipt);

    let report = verify_receipt(&store, &bytes, None).unwrap();
    assert!(report.ok(), "{:?}", report.mismatches);
    assert_eq!(report.tensors_checked, 2);
    assert!(report.objects_checked > 2);

    let adapter = adapter_for(&receipt);
    let adapter_bytes = adapter.encode().unwrap();
    let report = verify_receipt(&store, &bytes, Some(&adapter_bytes)).unwrap();
    assert!(report.ok(), "{:?}", report.mismatches);
    assert_eq!(report.adapter_entries_checked, 2);
}

#[test]
fn a_canonical_rewrite_is_refused() {
    let (_dir, _source, store) = setup();
    let receipt = store.materialization_receipt("receipts", "court").unwrap();
    let mut tampered = receipt.clone();
    // Change one recorded payload hash and keep the receipt canonically
    // encodable (the set root covers logical roots, not sha256).
    tampered.tensors[0].sha256[0] ^= 0xff;
    let bytes = tampered.encode().unwrap();
    MaterializationReceipt::decode(&bytes).expect("the rewrite is still canonical");
    let report = verify_receipt(&store, &bytes, None).unwrap();
    assert!(!report.ok());
    assert!(report
        .mismatches
        .iter()
        .any(|message| message.contains("sha256")));

    // A rewritten source object id is refused too.
    let mut tampered = receipt.clone();
    tampered.tensors[1].source_objects[0].object_id = weftfs::identity::Hash32::ZERO;
    let bytes = tampered.encode().unwrap();
    let report = verify_receipt(&store, &bytes, None).unwrap();
    assert!(!report.ok());

    // A rewritten destination offset is refused.
    let mut tampered = receipt.clone();
    tampered.tensors[0].destination_offset += 1;
    let bytes = tampered.encode().unwrap();
    let report = verify_receipt(&store, &bytes, None).unwrap();
    assert!(!report.ok());
    assert!(report
        .mismatches
        .iter()
        .any(|message| message.contains("destination")));

    // A rewritten logical root breaks the receipt's own roster check at decode.
    let mut tampered = receipt.clone();
    tampered.tensors[0].logical_root = weftfs::identity::Hash32::ZERO;
    let bytes = tampered.encode().unwrap();
    let err = MaterializationReceipt::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt);
}

#[test]
fn a_store_side_tamper_is_refused() {
    let (dir, _source, store) = setup();
    let receipt = store.materialization_receipt("receipts", "court").unwrap();
    let bytes = receipt.encode().unwrap();
    // Resolve the shard's manifest-namespace blob through the recorded object
    // id, before the store is dropped.
    let object = &receipt.tensors[1].source_objects[0];
    let record = store.object_meta(object.object_id).unwrap();
    let hex = record.content.to_hex();
    drop(store);

    // Mutate one stored shard blob under the receipt's own object id, then
    // re-open. The verifying read path fails closed before the receipt can be
    // compared, which is the correct outcome: the store no longer holds the
    // bytes the receipt names.
    let path = dir
        .path()
        .join("store/manifests")
        .join(&hex[..2])
        .join(&hex);
    assert!(path.is_file(), "missing blob {}", path.display());
    let mut bytes_on_disk = fs::read(&path).unwrap();
    let last = bytes_on_disk.len() - 1;
    bytes_on_disk[last] ^= 0x01;
    fs::write(&path, &bytes_on_disk).unwrap();

    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly);
    match store {
        // The store may still open (the tamper is in an object, not control
        // state); if it does, the derivation refuses.
        Ok(store) => {
            let err = store.derive_materialization_rows("receipts").unwrap_err();
            // The class depends on where the corrupted bytes are first
            // detected (a blob hash mismatch is `Corrupt`; a manifest that no
            // longer resolves is `NotFound`). The property under test is that
            // the derivation fails closed rather than returning a row.
            assert!(
                matches!(
                    err.class(),
                    weftfs::Class::Corrupt | weftfs::Class::Io | weftfs::Class::NotFound
                ),
                "{err}"
            );
        }
        Err(err) => {
            assert!(
                matches!(
                    err.class(),
                    weftfs::Class::Corrupt | weftfs::Class::Io | weftfs::Class::NotFound
                ),
                "{err}"
            );
        }
    }
}

#[test]
fn the_cli_writes_and_independently_verifies_a_receipt() {
    let (dir, _source, _store) = setup();
    let store_path = dir.path().join("store");
    let receipt_path = dir.path().join("receipt.wcb");
    let adapter_path = dir.path().join("adapter.wcb");
    let bin = env!("CARGO_BIN_EXE_weftfs");

    let output = Command::new(bin)
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "--json",
            "checkpoint",
            "receipt",
            "--name",
            "receipts",
            "--out",
            receipt_path.to_str().unwrap(),
            "--replica",
            "cli",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["tensors"], 2);
    assert!(receipt_path.is_file());

    // Write an adapter binding through the library, as an adapter would.
    let store = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
    let receipt = MaterializationReceipt::decode(&fs::read(&receipt_path).unwrap()).unwrap();
    let adapter = adapter_for(&receipt);
    fs::write(&adapter_path, adapter.encode().unwrap()).unwrap();
    drop(store);

    let output = Command::new(bin)
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "--json",
            "receipt",
            "verify",
            "--receipt",
            receipt_path.to_str().unwrap(),
            "--adapter",
            adapter_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["adapter_entries_checked"], 2);

    // A tampered adapter entry exits non-zero.
    let mut bad = adapter.clone();
    bad.entries[0].sha256[0] ^= 0xff;
    bad.set_root = AdapterReceipt::set_root(
        &bad.entries
            .iter()
            .map(|entry| {
                (
                    entry.fqn.clone(),
                    entry.tensor_name.clone(),
                    entry.content_root,
                )
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(&adapter_path, bad.encode().unwrap()).unwrap();
    let output = Command::new(bin)
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "receipt",
            "verify",
            "--receipt",
            receipt_path.to_str().unwrap(),
            "--adapter",
            adapter_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
