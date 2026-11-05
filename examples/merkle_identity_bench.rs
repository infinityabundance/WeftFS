//! Merkle vs whole-file identity write benchmark.
//!
//! Workload: one 128 MiB file at 64 KiB chunks, then 500 deterministic random
//! 4 KiB overwrites. Each identity runs in its own process so peak RSS and
//! `/proc/self/io` counters describe one policy. The fixture is created through
//! the streaming writer, so the harness never holds the file bytes in memory.
//!
//! Run only inside the OOM-protected bench court:
//!   scripts/docker-court.sh bench "bash bench/scripts/merkle_identity_court.sh"

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::format::ContentIdentity;
use weftfs::recovery::read_manifest;
use weftfs::store::{Options, Store};

const FILE_BYTES: u64 = 128 * 1024 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024;
const OVERWRITE_BYTES: usize = 4 * 1024;
const OVERWRITES: usize = 500;

struct GeneratedFile {
    remaining: u64,
    chunk_size: u64,
    value: u8,
    in_chunk: u64,
}

impl GeneratedFile {
    fn new(len: u64, chunk_size: u64) -> Self {
        GeneratedFile {
            remaining: len,
            chunk_size,
            value: 0,
            in_chunk: 0,
        }
    }
}

impl Read for GeneratedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let take = (buf.len() as u64).min(self.remaining) as usize;
        for byte in buf[..take].iter_mut() {
            *byte = self.value;
            self.in_chunk += 1;
            if self.in_chunk == self.chunk_size {
                self.in_chunk = 0;
                self.value = self.value.wrapping_add(1);
            }
        }
        self.remaining -= take as u64;
        Ok(take)
    }
}

#[derive(Default, Clone, Copy)]
struct ProcIo {
    rchar: u64,
    wchar: u64,
}

fn read_proc_io() -> ProcIo {
    let mut out = ProcIo::default();
    let Ok(text) = fs::read_to_string("/proc/self/io") else {
        return out;
    };
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        match key {
            "rchar:" => out.rchar = value,
            "wchar:" => out.wchar = value,
            _ => {}
        }
    }
    out
}

fn proc_status_kib(field: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/self/status").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let rest = rest.trim_start_matches(':').trim();
            return rest.trim_end_matches(" kB").parse::<u64>().ok();
        }
    }
    None
}

fn reset_peak_rss() -> bool {
    fs::write("/proc/self/clear_refs", b"5\n").is_ok()
}

fn parse_arg(args: &[String], name: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == name {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_owned());
        }
    }
    None
}

fn summary(values: &[u64]) -> Json {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return json!({"count": 0});
    }
    let pick = |p: f64| -> u64 {
        let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[index]
    };
    let sum: u64 = sorted.iter().sum();
    json!({
        "count": sorted.len(),
        "p50_ns": pick(0.50),
        "p95_ns": pick(0.95),
        "max_ns": *sorted.last().unwrap(),
        "mean_ns": sum / sorted.len() as u64,
    })
}

fn next_random(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn run(identity: ContentIdentity) -> Result<Json, String> {
    let dir = PathBuf::from(format!(
        "/tmp/weft-merkle-bench-rust-{}-{}",
        identity.as_str(),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        content_identity: identity,
        ..Options::default()
    };
    let mut store = Store::init(&dir, options).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let reader = GeneratedFile::new(FILE_BYTES, CHUNK_SIZE);
    let id = store
        .create_file_streaming_seeded(
            root,
            b"large.bin",
            reader,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"merkle-bench",
        )
        .map_err(|err| err.to_string())?;

    let first = store
        .object_meta(id)
        .and_then(|record| read_manifest(store.root(), record.content))
        .map_err(|err| err.to_string())?;
    let _ = reset_peak_rss();

    let mut latencies = Vec::with_capacity(OVERWRITES);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let payload = vec![0xA5u8; OVERWRITE_BYTES];
    let range = FILE_BYTES - OVERWRITE_BYTES as u64 + 1;
    let io_before = read_proc_io();
    store.reset_range_write_chunk_reads();
    let wall = Instant::now();
    for _ in 0..OVERWRITES {
        let offset = next_random(&mut state) % range;
        let started = Instant::now();
        store
            .write_file(id, offset, &payload)
            .map_err(|err| err.to_string())?;
        latencies.push(started.elapsed().as_nanos() as u64);
    }
    let wall_ns = wall.elapsed().as_nanos() as u64;
    let io_after = read_proc_io();
    let chunk_reads = store.range_write_chunk_reads();

    let last = store
        .object_meta(id)
        .and_then(|record| read_manifest(store.root(), record.content))
        .map_err(|err| err.to_string())?;
    let verify = store.verify(true).map_err(|err| err.to_string())?;
    let peak = proc_status_kib("VmHWM");
    let identity_ok = last.identity == identity;
    let root_changed = first.content_hash != last.content_hash;
    let _ = fs::remove_dir_all(&dir);

    Ok(json!({
        "harness": "rust",
        "revision": env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
        "identity": identity.as_str(),
        "file_bytes": FILE_BYTES,
        "chunk_size": CHUNK_SIZE,
        "chunks": first.chunks.len(),
        "overwrites": OVERWRITES,
        "overwrite_bytes": OVERWRITE_BYTES,
        "write_latency": summary(&latencies),
        "write_loop_wall_ns": wall_ns,
        "bytes_read_rchar": io_after.rchar.saturating_sub(io_before.rchar),
        "bytes_written_wchar": io_after.wchar.saturating_sub(io_before.wchar),
        "chunk_blobs_read": chunk_reads,
        "peak_rss_kib": peak,
        "peak_rss_reset": true,
        "root_hex": last.content_hash.to_hex(),
        "root_changed": root_changed,
        "identity_preserved": identity_ok,
        "verify_ok": verify.ok,
        "verify_problems": verify.problems.len(),
    }))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let identity = match parse_arg(&args, "--identity").as_deref() {
        Some("whole-file") => ContentIdentity::WholeFileV1,
        Some("merkle") => ContentIdentity::MerkleV3,
        other => {
            eprintln!("--identity must be whole-file|merkle (got {other:?})");
            std::process::exit(64);
        }
    };
    let out = parse_arg(&args, "--out").map(PathBuf::from);
    match run(identity) {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).expect("serialize");
            match out.as_deref() {
                Some(path) => {
                    if let Err(err) = fs::write(path, text.as_bytes()) {
                        eprintln!("cannot write {}: {err}", path.display());
                        std::process::exit(1);
                    }
                    println!("rust {} receipt: {}", identity.as_str(), path.display());
                }
                None => println!("{text}"),
            }
        }
        Err(err) => {
            eprintln!("bench failed: {err}");
            std::process::exit(1);
        }
    }
}
