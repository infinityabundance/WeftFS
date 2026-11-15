//! T073 court harness — portable authenticated range proofs.
//!
//! Builds a store per (tensor size, chunk size) case, writes a raw chunk-table
//! checkpoint, signs one anchor per shard, proves a set of regions, and writes
//! each proof bundle plus deliberately invalid variants to `--out`. It also
//! measures, in-process, the verifier's cost against the cost of hashing the
//! whole tensor, so the shell court can publish both and hand the bundles to
//! the independent Python verifier.
//!
//! Nothing here is a benchmark of a backend: it is a cost comparison between
//! two verification strategies on the same bytes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::range_proof::{RangeAnchor, RangeProof};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SIGNER_SEED: [u8; 32] = [0x5a; 32];

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

fn write_container(dir: &Path, bytes: &[u8], elements: u64) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let shape = [elements, 1u64];
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "bf16",
            shape: &shape,
            data: bytes,
        }],
        None,
    )
    .unwrap();
    let path = dir.join("case.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn median_ns(samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    let mut out = PathBuf::from("/tmp/range-proof-court");
    let mut json_path: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    fs::create_dir_all(&out).unwrap();

    // (label, total tensor bytes, chunk size, shards)
    let cases = [
        ("nano-16k-c4k", 16 * 1024usize, 4096u64, 1u32),
        ("tiny-64k-c4k", 64 * 1024usize, 4096u64, 1u32),
        ("mid-4m-c4k", 4 * 1024 * 1024, 4096, 1),
        ("mid-32m-c64k", 32 * 1024 * 1024, 64 * 1024, 1),
        ("wide-8m-c64k-s8", 8 * 1024 * 1024, 64 * 1024, 8),
    ];
    // Region sizes as fractions, always at least one byte and never past EOF.
    let regions = [
        ("r64b", 64u64, 64u64),
        ("r4k", 4096u64, 4096u64),
        ("r64k", 64 * 1024, 64 * 1024),
        ("r1m", 1024 * 1024, 1024 * 1024),
        ("r8m", 8 * 1024 * 1024, 8 * 1024 * 1024),
    ];

    let mut rows = Vec::new();
    let mut signer_public_key = String::new();
    for (label, bytes_len, chunk_size, shards) in cases {
        let case_dir = out.join(label);
        fs::create_dir_all(&case_dir).unwrap();
        let mut options = Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size,
            checkpoint_interval: 4096,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        };
        options.chunk_size = chunk_size;
        let mut store = Store::init(case_dir.join("store"), options).unwrap();
        let bytes = random_bytes(bytes_len, 0x9e37_79b9_7f4a_7c15);
        let source = write_container(&case_dir, &bytes, (bytes_len / 2) as u64);
        let report = store
            .write_checkpoint(
                "cp",
                &source,
                &CheckpointWriteOptions {
                    shards,
                    axis: 0,
                    filters: Vec::new(),
                    strategy: ShardStrategy::Raw,
                    tile_shape: None,
                    ..CheckpointWriteOptions::default()
                },
            )
            .unwrap();
        let (written, refused) = store
            .write_range_anchors("cp", None, Some(&SIGNER_SEED), None)
            .unwrap();
        assert!(refused.is_empty(), "a raw shard must anchor cleanly");
        assert_eq!(written.len(), shards as usize);
        let anchor_id = written[0].anchor_id;
        let (_, anchor) = store.range_anchor("cp", anchor_id).unwrap();
        let signer = anchor.signature.as_ref().unwrap().public_key;
        if signer_public_key.is_empty() {
            signer_public_key = signer.iter().map(|byte| format!("{byte:02x}")).collect();
        }
        let shard_bytes = anchor.bytes;
        let checkpoint_bytes = store.load_checkpoint_manifest("cp").unwrap();
        let tensor_manifest_root = checkpoint_bytes.manifest_root;

        // Cost of the whole-tensor strategy on the same bytes.
        let drops = Store::open(case_dir.join("store"), OpenMode::ReadOnly).unwrap();
        let object_id = checkpoint_bytes.manifest.tensor("weight").unwrap().shards[0].object_id;
        let mut full_ns = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            let bytes = drops.read_object(object_id).unwrap();
            std::hint::black_box(&bytes);
            full_ns.push(started.elapsed().as_nanos() as u64);
        }
        let full_median = median_ns(&mut full_ns);

        let mut seen_lengths: Vec<u64> = Vec::new();
        for (region_label, requested, max_len) in regions {
            let length = requested.min(max_len).min(shard_bytes);
            if length == 0 || seen_lengths.contains(&length) {
                continue;
            }
            seen_lengths.push(length);
            // Offset chosen to be deliberately unaligned to the chunk grid.
            let offset = (chunk_size / 3).min(shard_bytes.saturating_sub(length));
            if offset + length > shard_bytes {
                continue;
            }
            let bundle = store
                .prove_range("cp", "weight", 0, offset, length, anchor_id)
                .unwrap();
            let proof_bytes = bundle.proof.encode().unwrap().len() as u64;
            let chunk_bytes: u64 = bundle.chunks.iter().map(|c| c.len() as u64).sum();
            // Verify from supplied bytes (portable mode), 7 reps.
            let mut verify_ns = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                let report = store
                    .verify_range_proof(&bundle.proof, &anchor, Some(&bundle.chunks), &[signer])
                    .unwrap();
                std::hint::black_box(&report);
                verify_ns.push(started.elapsed().as_nanos() as u64);
            }
            // Verify with the dependency re-read from the store.
            let mut store_verify_ns = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                let report = store
                    .verify_range_proof(&bundle.proof, &anchor, None, &[signer])
                    .unwrap();
                std::hint::black_box(&report);
                store_verify_ns.push(started.elapsed().as_nanos() as u64);
            }
            let verify_median = median_ns(&mut verify_ns);
            let store_verify_median = median_ns(&mut store_verify_ns);
            let region_dir = case_dir.join(format!("{region_label}-{offset}"));
            write_bundle(&region_dir, &bundle);
            rows.push(json!({
                "case": label,
                "tensor_bytes": bytes_len,
                "chunk_size": chunk_size,
                "shards": shards,
                "shard_bytes": shard_bytes,
                "offset": offset,
                "length": length,
                "leaves": bundle.proof.leaves.len(),
                "path_nodes": bundle.proof.paths.iter().map(|path| path.len()).sum::<usize>(),
                "proof_bytes": proof_bytes,
                "chunk_bytes": chunk_bytes,
                "proof_plus_chunks_bytes": proof_bytes + chunk_bytes,
                "verify_ns": verify_median,
                "verify_store_ns": store_verify_median,
                "full_tensor_ns": full_median,
                "advantage": full_median as f64 / verify_median.max(1) as f64,
                "bundle": region_dir.strip_prefix(&out).unwrap().display().to_string(),
            }));
        }

        // Deliberately invalid bundles for the independent verifier.
        let length = 4096u64.min(shard_bytes);
        let bundle = store
            .prove_range("cp", "weight", 0, 0, length.max(1), anchor_id)
            .unwrap();
        let bad = case_dir.join("invalid");
        // 1. A different range.
        let mut forged = bundle.proof.clone();
        forged.offset = 1;
        write_mutated(&bad.join("different-range"), &forged, &bundle, &anchor);
        // 2. A substituted leaf.
        let mut forged = bundle.proof.clone();
        if forged.leaves.len() > 0 {
            forged.leaves[0].hash = weftfs::identity::Hash32::from_bytes([3u8; 32]);
        }
        write_mutated(&bad.join("substituted-leaf"), &forged, &bundle, &anchor);
        // 3. A root that is not the checkpoint's.
        let mut forged_anchor = anchor.clone();
        forged_anchor.chunk_table_root = weftfs::identity::Hash32::from_bytes([4u8; 32]);
        write_anchor_bundle(&bad.join("wrong-root"), &bundle, &forged_anchor);
        // 4. A truncated path.
        let mut forged = bundle.proof.clone();
        if forged.paths[0].len() > 1 {
            forged.paths[0].pop();
        } else {
            forged.paths[0].push(weftfs::range_proof::PathNode {
                side: weftfs::range_proof::PathSide::Left,
                hash: weftfs::identity::Hash32::from_bytes([5u8; 32]),
            });
        }
        write_mutated(&bad.join("truncated-path"), &forged, &bundle, &anchor);
        // 5. A two-chunk region that claims one chunk.
        let span_len = (chunk_size + 64).min(shard_bytes);
        let span_start = chunk_size
            .saturating_sub(32)
            .min(shard_bytes - span_len.min(shard_bytes));
        if span_start + span_len <= shard_bytes {
            let spanning = store
                .prove_range("cp", "weight", 0, span_start, span_len, anchor_id)
                .unwrap();
            let mut forged = spanning.proof.clone();
            forged.leaves.pop();
            forged.paths.pop();
            write_mutated(&bad.join("cross-boundary"), &forged, &spanning, &anchor);
        }
        // 6. Non-canonical bytes (one flipped byte in the proof record).
        let bytes = bundle.proof.encode().unwrap();
        let mut corrupt = bytes.clone();
        let position = corrupt.len() / 2;
        corrupt[position] ^= 0x01;
        let dir = bad.join("noncanonical");
        write_bundle(&dir, &bundle);
        fs::write(dir.join("proof.wcb"), &corrupt).unwrap();
        // A guaranteed non-canonical record: one trailing byte.
        let dir = bad.join("trailing-bytes");
        write_bundle(&dir, &bundle);
        let mut extended = bytes;
        extended.push(0);
        fs::write(dir.join("proof.wcb"), &extended).unwrap();

        std::hint::black_box(tensor_manifest_root);
        std::hint::black_box(report);
    }

    let document = json!({
        "schema": "weft.range-proof-court/v1",
        "signer_public_key": signer_public_key,
        "rows": rows,
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(path, text).unwrap(),
        None => println!("{text}"),
    }
}

fn write_bundle(dir: &Path, bundle: &weftfs::range_proof::RangeProofBundle) {
    fs::create_dir_all(dir.join("chunks")).unwrap();
    fs::write(dir.join("proof.wcb"), bundle.proof.encode().unwrap()).unwrap();
    fs::write(dir.join("anchor.wcb"), &bundle.anchor_bytes).unwrap();
    fs::write(dir.join("checkpoint.wcb"), &bundle.checkpoint_bytes).unwrap();
    for (index, bytes) in bundle.chunks.iter().enumerate() {
        fs::write(dir.join("chunks").join(format!("{index:06}.bin")), bytes).unwrap();
    }
}

fn write_mutated(
    dir: &Path,
    proof: &RangeProof,
    bundle: &weftfs::range_proof::RangeProofBundle,
    _anchor: &RangeAnchor,
) {
    fs::create_dir_all(dir.join("chunks")).unwrap();
    fs::write(dir.join("proof.wcb"), proof.encode().unwrap()).unwrap();
    fs::write(dir.join("anchor.wcb"), &bundle.anchor_bytes).unwrap();
    fs::write(dir.join("checkpoint.wcb"), &bundle.checkpoint_bytes).unwrap();
    for (index, bytes) in bundle.chunks.iter().enumerate() {
        fs::write(dir.join("chunks").join(format!("{index:06}.bin")), bytes).unwrap();
    }
}

fn write_anchor_bundle(
    dir: &Path,
    bundle: &weftfs::range_proof::RangeProofBundle,
    anchor: &RangeAnchor,
) {
    fs::create_dir_all(dir.join("chunks")).unwrap();
    fs::write(dir.join("proof.wcb"), bundle.proof.encode().unwrap()).unwrap();
    fs::write(dir.join("anchor.wcb"), anchor.encode().unwrap()).unwrap();
    fs::write(dir.join("checkpoint.wcb"), &bundle.checkpoint_bytes).unwrap();
    for (index, bytes) in bundle.chunks.iter().enumerate() {
        fs::write(dir.join("chunks").join(format!("{index:06}.bin")), bytes).unwrap();
    }
}
