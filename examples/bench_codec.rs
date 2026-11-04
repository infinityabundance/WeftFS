//! Codec benchmark court. Runs real encodes/decodes, verifies every decode, and
//! emits raw nanosecond samples. No comparative claim is embedded here; the
//! analysis and scoped claims live in bench/receipts and bench/CONTRACTS.md.
//!
//! Usage: bench_codec [--iterations N] [--out PATH]
//! Requires the release build; run through scripts/docker-court.sh rust.

use std::env;
use std::fs;
use std::time::Instant;

use serde_json::{json, Value};
use weftcodec::dependency::MapResolver;
use weftcodec::hash::{hash_domain, DOMAIN_CHUNK};
use weftcodec::modes::{decode_bytes, encode_delta, encode_dictionary, encode_raw, encode_zstd};
use weftcodec::Limits;

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let word = xorshift(&mut state).to_le_bytes();
        let take = (len - out.len()).min(8);
        out.extend_from_slice(&word[..take]);
    }
    out
}

fn repetitive(len: usize) -> Vec<u8> {
    let pattern = b"weft codec benchmark: context is not a free lunch\n";
    pattern.iter().copied().cycle().take(len).collect()
}

fn near_duplicate(base: &[u8], edits: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..edits {
        let pos = (xorshift(&mut state) as usize) % out.len();
        out[pos] = out[pos].wrapping_add(1);
    }
    out
}

fn samples<F, T>(iterations: usize, mut op: F) -> (Vec<u128>, T)
where
    F: FnMut() -> T,
{
    let mut times = Vec::with_capacity(iterations);
    let mut last = None;
    for _ in 0..iterations {
        let start = Instant::now();
        let value = op();
        times.push(start.elapsed().as_nanos());
        last = Some(value);
    }
    (times, last.expect("iterations > 0"))
}

fn vm_hwm_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().trim_end_matches(" kB").parse::<u64>().ok();
        }
    }
    None
}

fn main() {
    let mut iterations = 7usize;
    let mut out_path: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                iterations = args.next().expect("value").parse().expect("integer");
            }
            "--out" => out_path = Some(args.next().expect("path")),
            other => panic!("unknown argument {other}"),
        }
    }
    if iterations == 0 {
        panic!("iterations must be positive");
    }
    let limits = Limits::default();

    let base = random_bytes(1 << 20, 0xA11CE);
    let corpus: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("one_byte", vec![0x42]),
        ("random_4m", random_bytes(4 << 20, 0xBEEF)),
        ("repetitive_4m", repetitive(4 << 20)),
        ("near_dup_base_1m", base.clone()),
        ("near_dup_1pct", near_duplicate(&base, 10_000)),
    ];

    let mut results: Vec<Value> = Vec::new();
    for (name, data) in &corpus {
        let input_hash = hash_domain(DOMAIN_CHUNK, data).to_hex();
        let mut cases: Vec<(&str, i32, Vec<u8>)> = vec![
            ("raw", 0, encode_raw(data, None, &limits).expect("raw")),
            (
                "zstd",
                3,
                encode_zstd(data, 3, None, &limits).expect("zstd3"),
            ),
            (
                "zstd",
                9,
                encode_zstd(data, 9, None, &limits).expect("zstd9"),
            ),
            (
                "zstd",
                19,
                encode_zstd(data, 19, None, &limits).expect("zstd19"),
            ),
        ];
        if name.starts_with("near_dup_1pct") {
            cases.push((
                "delta",
                0,
                encode_delta(data, &base, &limits).expect("delta"),
            ));
            cases.push((
                "dictionary",
                3,
                encode_dictionary(data, &base, 3, &limits).expect("dictionary"),
            ));
        }
        for (mode, level, frame) in cases {
            let mut resolver = MapResolver::new();
            resolver.insert(hash_domain(DOMAIN_CHUNK, &base), base.clone());
            let decoded = decode_bytes(&frame, &resolver, &limits).expect("decode");
            assert_eq!(&decoded, data, "round-trip mismatch for {name}/{mode}");
            let (encode_ns, _) = samples(iterations, || {
                let bytes = match mode {
                    "raw" => encode_raw(data, None, &limits).unwrap(),
                    "zstd" => encode_zstd(data, level, None, &limits).unwrap(),
                    "delta" => encode_delta(data, &base, &limits).unwrap(),
                    "dictionary" => encode_dictionary(data, &base, level, &limits).unwrap(),
                    _ => unreachable!(),
                };
                bytes.len()
            });
            let (decode_ns, _) = samples(iterations, || {
                decode_bytes(&frame, &resolver, &limits).unwrap()
            });
            results.push(json!({
                "corpus": name,
                "input_bytes": data.len(),
                "input_hash": input_hash,
                "mode": mode,
                "level": level,
                "frame_bytes": frame.len(),
                "overhead_bytes": frame.len() as i64 - data.len() as i64,
                "encode_ns": encode_ns,
                "decode_ns": decode_ns,
            }));
        }
    }

    let report = json!({
        "schema": "weft-codec-bench-v1",
        "kind": "codec-microbench",
        "iterations": iterations,
        "environment": {
            "kernel": env::var("WEFT_COURT_KERNEL").ok(),
            "court_image": env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": env::var("WEFT_COURT_IMAGE_ID").ok(),
            "court_memory": env::var("WEFT_COURT_MEMORY").ok(),
            "court_cpus": env::var("WEFT_COURT_CPUS").ok(),
            "court_pids": env::var("WEFT_COURT_PIDS").ok(),
            "docker_client": env::var("WEFT_DOCKER_CLIENT").ok(),
            "docker_server": env::var("WEFT_DOCKER_SERVER").ok(),
            "revision": env::var("WEFT_REPO_WEFTCODEC").ok(),
        },
        "note": "Frame bytes include WFT1 framing. ZSTD results use the codec's own zstd mode as the context-free control, not an external zstd binary. No throughput claim is made beyond these raw samples.",
        "vm_hwm_kib": vm_hwm_kib(),
        "results": results,
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize");
    match out_path {
        Some(path) => {
            fs::write(&path, text).expect("write report");
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
}
