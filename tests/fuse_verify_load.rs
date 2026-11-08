//! Stage 3 FUSE wiring: `mount --verify-load` verifies every tensor file on
//! open and writes one canonical `weft.load.receipt.v1` record per file.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::fs;

use fuse_common::Fixture;
use weftfs::load_receipt::{LoadReceipt, LoadVerification};

/// Deterministic BF16 payload.
fn bf16_payload(elements: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let word = ((state >> 48) as u16) & 0x3f80;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

#[test]
fn mount_verify_load_writes_a_receipt_per_tensor_file() {
    let fx = Fixture::new();
    let payload = fx.dir.path().join("payload.bin");
    fs::write(&payload, bf16_payload(64 * 32)).unwrap();
    fx.weft_ok(&[
        "write".as_ref(),
        "/weights".as_ref(),
        "--file".as_ref(),
        payload.as_os_str(),
        "--tensor".as_ref(),
        "--dtype".as_ref(),
        "bf16".as_ref(),
        "--shape".as_ref(),
        "64,32".as_ref(),
        "--strategy".as_ref(),
        "zstd".as_ref(),
    ]);

    let receipts = fx.dir.path().join("receipts");
    fs::create_dir(&receipts).unwrap();
    let receipts_arg = receipts.to_string_lossy().to_string();
    let _mount = fx.mount(&["--verify-load", "--load-receipt-dir", &receipts_arg]);

    let bytes = fs::read(fx.path("weights")).expect("read tensor through the mount");
    assert_eq!(bytes.len(), 64 * 32 * 2);

    let entries: Vec<_> = fs::read_dir(&receipts).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "one receipt per verified tensor file");
    let encoded = fs::read(entries[0].path()).unwrap();
    let receipt = LoadReceipt::decode(&encoded).expect("canonical receipt");
    assert_eq!(receipt.verification, LoadVerification::Lazy);
    assert_eq!(receipt.tensors.len(), 1);
    assert_eq!(receipt.loaded_set_root, receipt.attested_set_root);
    assert_eq!(
        receipt.tensors[0].loaded_root,
        receipt.tensors[0].attested_root
    );
    assert_eq!(receipt.tensors[0].bytes, 64 * 32 * 2);
}
