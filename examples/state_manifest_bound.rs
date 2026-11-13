//! Where the checkpoint state manifest stops encoding.
//!
//! `format::encode_state` is a canonical WCB value encoded with the codec's
//! default limits, whose `max_items` is 1,000,000. This probe builds a state
//! with N file objects (the aged court's shape) and reports the encoded item
//! count and whether it encodes. It exists because the aged-store court cannot
//! write a checkpoint above the ceiling, and the ceiling must be measured, not
//! guessed.
//!
//!   cargo run --release --example state_manifest_bound
//!
//! Output is one line per size plus the largest size that encodes.

use std::collections::BTreeMap;

use weftfs::state::{ObjectKind, ObjectRecord, State};

fn object(index: u64) -> ObjectRecord {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "team".to_owned(),
        weftcodec::wcb::Value::Str(["red", "green", "blue"][(index % 3) as usize].to_owned()),
    );
    attrs.insert("score".to_owned(), weftcodec::wcb::Value::U64(index % 16));
    attrs.insert(
        "xattr.rev".to_owned(),
        weftcodec::wcb::Value::Bytes(index.to_le_bytes().to_vec()),
    );
    // A deterministic 32-byte id that is never the root id.
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&index.to_le_bytes());
    id[8..16].copy_from_slice(&(index ^ 0x9e37_79b9_7f4a_7c15).to_le_bytes());
    id[31] = 1;
    ObjectRecord {
        id: weftfs::identity::Hash32::from_bytes(id),
        kind: ObjectKind::File,
        mode: 0o644,
        uid: 0,
        gid: 0,
        size: 8,
        version: weftfs::identity::Hash32::from_bytes([(index as u8) ^ 0x5a; 32]),
        content: weftfs::identity::Hash32::from_bytes([index as u8; 32]),
        mtime_ns: 1_700_000_000_000_000_000,
        attrs,
    }
}

fn main() {
    let sizes: Vec<u64> = std::env::var("WEFT_BOUND_SIZES")
        .ok()
        .map(|text| {
            text.split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![10_000, 50_000, 100_000, 125_000, 150_000, 175_000, 200_000]);
    let mut largest = 0u64;
    for size in sizes {
        let mut state = State {
            generation: 256,
            ..State::default()
        };
        for index in 0..size {
            let record = object(index);
            state.objects.insert(record.id, record);
        }
        let started = std::time::Instant::now();
        match weftfs::format::encode_state(&state, 256) {
            Ok(bytes) => {
                largest = size;
                println!(
                    "objects={size} ok bytes={} encode_ms={:.1}",
                    bytes.len(),
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(err) => {
                println!(
                    "objects={size} rejected class={:?} error={err}",
                    err.class()
                );
            }
        }
    }
    println!("largest_encoded_objects={largest}");
}
