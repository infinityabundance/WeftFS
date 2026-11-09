//! Generate the committed mode-20 store handoff fixture.
//!
//! Usage:
//!   cargo run --example gen_engram_store -- <empty-or-absent-dir>
//!
//! The fixture is a deterministic Rust-written store containing one engram
//! object, a snapshot, and one incremental update. `EXPECTED.json` records the
//! object id, logical root, table digests and a region expectation so the
//! independent Go store can read, verify and region-read it.

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use weftcodec::Region;
use weftfs::format::RepresentationKind;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    EngramGroupUpdate, EngramWriteSpec, TensorLayoutSpec, TensorPlanOptions, TensorStrategy,
    TensorWriteSpec,
};

const ROWS: u64 = 32;
const DIM: u64 = 4;
const ROW_BYTES: usize = (DIM as usize) * 2;

fn table(seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..ROWS as usize * ROW_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xff) as u8
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn main() {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: gen_engram_store <dir>"),
    );
    if path.exists() {
        let mut entries = fs::read_dir(&path).expect("read fixture dir");
        if entries.next().is_some() {
            panic!("fixture directory {} is not empty", path.display());
        }
    }
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };
    let mut store = Store::init(&path, options).expect("init store");
    let root = store.resolve_path("/").unwrap();
    let layout = TensorLayoutSpec::row_major();
    let original = table(7);
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::EngramShard),
        causal: Vec::new(),
        plan: TensorPlanOptions {
            engram: Some(EngramWriteSpec {
                row_group_edges: vec![0, 8, 16, 24, 32],
                hash_function: "ngram-blake3-v1".to_owned(),
                vocab_size: ROWS,
                precision: None,
                quantization: None,
            }),
            ..TensorPlanOptions::default()
        },
        tile_shape: None,
    };
    let seed = [0x11u8; 32];
    let id = store
        .write_tensor_file_with_spec_seeded(
            root,
            b"engram",
            "bf16",
            &[ROWS, DIM],
            &original,
            &layout,
            &spec,
            &seed,
        )
        .expect("write engram object");
    let record = store.object_meta(id).unwrap();
    let manifest =
        weftfs::recovery::read_manifest(store.root(), record.content).expect("read manifest");
    let rep = manifest.representation.expect("representation");
    assert_eq!(rep.kind, RepresentationKind::Tensor);
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 20);

    let snapshot = store.snapshot_create("before-engram-update").unwrap();

    let mut replacement = vec![0u8; (24 - 16) as usize * ROW_BYTES];
    for (index, byte) in replacement.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(11).wrapping_add(3);
    }
    let mut updated = original.clone();
    updated[16 * ROW_BYTES..24 * ROW_BYTES].copy_from_slice(&replacement);
    let new_version = store
        .update_engram_groups(
            id,
            &[EngramGroupUpdate {
                group: 2,
                bytes: &replacement,
            }],
        )
        .expect("update engram groups");
    assert_eq!(store.read_tensor(id).unwrap(), updated);
    assert_eq!(store.read_object_at(Some(snapshot), id).unwrap(), original);

    // A row region strictly inside group 1.
    let region = Region::new(vec![9, 0], vec![14, DIM]);
    let read = store
        .read_tensor_region_with_options(id, &region, &Default::default())
        .expect("region read");
    assert_eq!(read.bytes, updated[9 * ROW_BYTES..14 * ROW_BYTES]);
    assert_eq!(read.units_touched, 1);

    let meta = store.tensor_metadata(id).unwrap();
    let expected = serde_json::json!({
        "object_id": id.to_hex(),
        "old_version": record.version.to_hex(),
        "new_version": new_version.to_hex(),
        "snapshot": snapshot.to_hex(),
        "logical_root": meta.logical_root.to_hex(),
        "table_sha256": sha256_hex(&updated),
        "original_sha256": sha256_hex(&original),
        "region": {
            "start": [9, 0],
            "end": [14, DIM],
            "sha256": sha256_hex(&read.bytes),
            "groups_touched": read.units_touched,
        },
        "table_hex": hex::encode(&updated),
    });
    fs::write(
        path.join("EXPECTED.json"),
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .expect("write EXPECTED.json");
    println!("engram store fixture written to {}", path.display());
}
