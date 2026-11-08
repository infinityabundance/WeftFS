//! Stage 3: end-to-end load receipts (`weft.load.receipt.v1`).
//!
//! A loader restores a checkpoint, computes the logical root of the produced
//! bytes, compares it to the attested root, and emits one canonical record per
//! replica. The tests cover:
//!
//! * lazy and full verification producing identical set roots on a healthy
//!   store;
//! * a real corrupted tile in the store: the loader refuses with a typed error
//!   naming the tile and has written **no** bytes for that restore;
//! * a tampered receipt failing its own canonical checks.

use std::fs;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::load_receipt::{LoadReceipt, LoadReceiptOptions, LoadVerification};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};

const SHAPE: [u64; 3] = [96, 32, 16];
const TENSOR: &str = "model.layers.0.mlp.down_proj.weight";

fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
    let path = std::env::temp_dir().join(format!(
        "weft-load-receipt-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    let store = Store::init(
        &path,
        Options {
            chunk_size: 64 * 1024,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .unwrap();
    (path, store)
}

fn bf16_payload(elements: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32);
        let value = (unit * 2.0 - 1.0) * 0.5;
        out.extend_from_slice(&((value.to_bits() >> 16) as u16).to_le_bytes());
    }
    out
}

fn write_container(path: &std::path::Path) -> std::path::PathBuf {
    let elements = SHAPE.iter().product::<u64>() as usize;
    let data = bf16_payload(elements);
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let container = path.join("weights.safetensors");
    fs::write(&container, file).unwrap();
    container
}

#[test]
fn lazy_and_full_receipts_agree_on_a_healthy_store() {
    let (path, mut store) = temp_store("healthy");
    let container = write_container(&path);
    store
        .write_checkpoint(
            "model",
            &container,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(vec![4, 8, 16]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();

    let mut receipts = Vec::new();
    for verification in [LoadVerification::Lazy, LoadVerification::Full] {
        let mut out = Vec::new();
        let (report, receipt) = store
            .restore_checkpoint_with_load_receipt(
                "model",
                &CheckpointRestoreOptions::default(),
                &LoadReceiptOptions {
                    replica: format!("replica-{}", verification.as_str()),
                    verification,
                },
                &mut out,
            )
            .unwrap();
        assert!(report.exact_source_file);
        assert_eq!(receipt.tensors.len(), 1);
        assert_eq!(
            receipt.tensors[0].attested_root,
            receipt.tensors[0].loaded_root
        );
        assert_eq!(receipt.loaded_set_root, receipt.attested_set_root);
        assert_eq!(receipt.bytes_produced, report.file_bytes);
        assert!(receipt.tensors[0].units_verified > 1, "tiles are counted");
        assert_eq!(
            receipt.file_sha256.to_vec(),
            fs::read(&container)
                .map(|bytes| safetensors::sha256(&bytes).to_vec())
                .unwrap()
        );
        // The canonical record round-trips and re-verifies its own roots.
        let encoded = receipt.encode().unwrap();
        let decoded = LoadReceipt::decode(&encoded).unwrap();
        assert_eq!(decoded.loaded_set_root, receipt.loaded_set_root);
        assert_eq!(decoded.root().unwrap(), receipt.root().unwrap());
        receipts.push(receipt);
    }
    assert_eq!(
        receipts[0].loaded_set_root, receipts[1].loaded_set_root,
        "lazy and full must prove the same loaded root"
    );
    store.verify(true).unwrap();
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

/// Corrupt one stored tile byte in the on-disk mode-15 frame and prove the
/// loader refuses without emitting bytes.
#[test]
fn corrupted_tile_is_refused_and_named() {
    let (path, mut store) = temp_store("corrupt");
    let container = write_container(&path);
    let report = store
        .write_checkpoint(
            "model",
            &container,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(vec![4, 8, 16]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let shard_id = report.tensors[0].object_ids[0];
    let record = store.object_meta(shard_id).unwrap();
    let manifest = weftfs::recovery::read_manifest(&path, record.content).unwrap();
    let representation = manifest
        .representation
        .as_ref()
        .expect("tensor representation");
    let frame_id = representation.frame.expect("mode-15 frame id");

    // Local layout: manifests/<ab>/<hex>.
    let hex = frame_id.to_hex();
    let frame_path = path.join("manifests").join(&hex[0..2]).join(&hex);
    let mut frame = fs::read(&frame_path).unwrap();
    let limits = weftcodec::Limits::default();
    let (header, _) = weftcodec::frame::parse_prefix(&frame, &limits).unwrap();
    let meta_len = u32::from_le_bytes([frame[96], frame[97], frame[98], frame[99]]) as usize;
    let descriptor = &frame[100..100 + meta_len];
    let stored_payload_len = header.body_len as usize - 4 - meta_len;
    let meta = weftcodec::modes::parse_tiled_descriptor(
        descriptor,
        header.decoded_len,
        stored_payload_len as u64,
        &limits,
    )
    .unwrap();
    let tile = meta.tiles.first().expect("at least one tile");
    let tile_start = 100 + meta_len + tile.offset as usize;
    assert!(tile.compressed_len > 0);
    // Flip a byte in the middle of the first tile's stored bytes.
    let at = tile_start + (tile.compressed_len as usize) / 2;
    frame[at] ^= 0xff;
    fs::write(&frame_path, &frame).unwrap();

    // The lazy loader reads only intersecting tile ranges, so the whole-frame
    // hash is not re-checked; the tile's own root must catch the mutation.
    let mut out = Vec::new();
    let err = store
        .restore_checkpoint_with_load_receipt(
            "model",
            &CheckpointRestoreOptions::default(),
            &LoadReceiptOptions {
                replica: "corrupt-replica".to_owned(),
                verification: LoadVerification::Lazy,
            },
            &mut out,
        )
        .unwrap_err();
    assert_eq!(err.class(), weftfs::error::Class::Corrupt);
    assert!(
        err.message().contains("tile"),
        "error must name the tile: {err}"
    );
    assert!(
        out.is_empty(),
        "no unverified byte may reach the caller's writer ({} bytes)",
        out.len()
    );
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn tampered_receipt_is_rejected() {
    let (path, mut store) = temp_store("tamper");
    let container = write_container(&path);
    store
        .write_checkpoint(
            "model",
            &container,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Raw,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let mut out = Vec::new();
    let (_, receipt) = store
        .restore_checkpoint_with_load_receipt(
            "model",
            &CheckpointRestoreOptions::default(),
            &LoadReceiptOptions::default(),
            &mut out,
        )
        .unwrap();
    let encoded = receipt.encode().unwrap();
    let mut tampered = receipt.clone();
    tampered.tensors[0].loaded_root = weftfs::Hash32::ZERO;
    let bytes = tampered.encode().unwrap();
    let err = LoadReceipt::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), weftfs::error::Class::Corrupt);
    // The original bytes are still valid.
    assert!(LoadReceipt::decode(&encoded).is_ok());
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

