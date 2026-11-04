//! Emit the exact corpus used by `bench_store.rs` as a JSON manifest for the
//! external SQLite baseline.
//!
//! The payload bytes, ObjectId seeds, one-hop relations, tags, snapshot names,
//! and commit generations are the same definitions used by
//! `examples/bench_store.rs` at 100 / 1,000 / 10,000 objects. ObjectIds are
//! derived with the real `weftfs::identity::object_id_from_seed` and content
//! hashes with `weftfs::identity::content_hash`, so the external baseline uses
//! the same hashes, not substitutes.
//!
//! Usage: bench_external_manifest [--sizes 100,1000,10000] [--out PATH]
//! Run through scripts/docker-court.sh bench.

use std::env;
use std::fs;

use serde_json::{json, Value};

const FIXED_TIMESTAMP_NS: i64 = 1_767_225_600_000_000_000; // 2026-01-01T00:00:00Z

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Deterministic 32–256 byte payload, identical to `bench_store::payload`.
fn payload(index: usize) -> Vec<u8> {
    let len = 32 + (mix(index as u64) as usize % 225);
    let mut out = format!(
        "weft-bench-object {:06} {:016x}\n",
        index,
        mix(index as u64 ^ 0xA5A5_5A5A_1234_5678)
    )
    .into_bytes();
    let mut state = mix(index as u64 + 1) | 1;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xFF) as u8);
    }
    out.truncate(len);
    out
}

fn manifest_for(size: usize) -> Value {
    let mut objects = Vec::with_capacity(size);
    let mut object_ids = Vec::with_capacity(size);
    for index in 0..size {
        let name = format!("obj-{index:06}.dat");
        let seed = format!("bench-{size}-{index}");
        let bytes = payload(index);
        let object_id = weftfs::identity::object_id_from_seed(seed.as_bytes());
        let content_hash = weftfs::identity::content_hash(&bytes);
        object_ids.push(object_id);
        objects.push(json!({
            "index": index,
            "name": name,
            "seed": seed,
            "object_id": object_id.to_hex(),
            "content_blake3": content_hash.to_hex(),
            "bytes": bytes.len(),
            "generation": index + 2,
            "payload_hex": hex::encode(&bytes),
        }));
    }

    // Generation accounting mirrors the commit order in bench_store:
    // init=1, one commit per create, the objects-only snapshot, then the
    // relation loop (with the half snapshot inside it), then the final
    // snapshot.
    let mut generation = size + 1; // init + creates
    let mut relations = Vec::new();
    let mut tagged_even = 0u64;
    let mut snapshots = Vec::new();

    generation += 1;
    snapshots.push(json!({
        "name": "objects-only",
        "generation": generation,
        "object_count": size,
    }));

    for index in 0..size {
        let subject = object_ids[index].to_hex();
        let target = object_ids[(index + 1) % size].to_hex();
        generation += 1;
        relations.push(json!({
            "subject": subject,
            "predicate": "refers_to",
            "target_kind": "object",
            "target_value": target,
            "generation": generation,
        }));

        if index % 5 != 0 {
            let label = if index % 2 == 0 { "even" } else { "odd" };
            generation += 1;
            relations.push(json!({
                "subject": object_ids[index].to_hex(),
                "predicate": "tagged_as",
                "target_kind": "value",
                "target_value": label,
                "generation": generation,
            }));
            if label == "even" {
                tagged_even += 1;
            }
        }

        if index % 3 == 0 && index % 5 != 0 {
            generation += 1;
            relations.push(json!({
                "subject": object_ids[index].to_hex(),
                "predicate": "owner",
                "target_kind": "value",
                "target_value": "team-3",
                "generation": generation,
            }));
        }

        if index == size / 2 {
            generation += 1;
            snapshots.push(json!({
                "name": "half",
                "generation": generation,
                "object_count": size,
            }));
        }
    }

    generation += 1;
    snapshots.push(json!({
        "name": "final",
        "generation": generation,
        "object_count": size,
    }));

    json!({
        "size": size,
        "fixed_timestamp_ns": FIXED_TIMESTAMP_NS,
        "objects_only_generation": size + 1,
        "generation_after_ingest": generation,
        "objects": objects,
        "relations": relations,
        "relations_count": relations.len(),
        "tagged_even": tagged_even,
        "snapshots": snapshots,
    })
}

fn main() {
    let mut sizes = vec![100usize, 1_000, 10_000];
    let mut out: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sizes" => {
                let value = args.next().expect("sizes");
                sizes = value
                    .split(',')
                    .map(|part| part.trim().parse::<usize>().expect("integer size"))
                    .filter(|size| *size > 1)
                    .collect();
            }
            "--out" => out = Some(args.next().expect("path")),
            other => panic!("unknown argument {other}"),
        }
    }
    let report = json!({
        "schema": "weft-external-corpus-v1",
        "source": "examples/bench_store.rs definitions",
        "fixed_timestamp_ns": FIXED_TIMESTAMP_NS,
        "sizes": sizes.iter().map(|size| manifest_for(*size)).collect::<Vec<_>>(),
        "notes": [
            "Payload bytes, ObjectId seeds, relations, tags, snapshot names, and commit generations match examples/bench_store.rs.",
            "ObjectIds use the normative weft/object/v1 BLAKE3 domain; content hashes use weft/chunk/v1.",
            "payload_hex is included so the external baseline cannot silently drift from the Rust corpus."
        ],
    });
    let text = serde_json::to_string(&report).expect("serialize manifest");
    match out {
        Some(path) => {
            fs::write(&path, text).expect("write manifest");
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
}
