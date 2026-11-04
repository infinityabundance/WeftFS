//! Streaming vs buffered 256 MiB file creation.
//!
//! The orchestrator spawns one child process per mode so `VmHWM` is an honest
//! per-mode peak (the streaming run must not inherit the buffered run's
//! high-water mark). Both children write the same deterministic synthetic
//! bytes into a fresh store on the court's tmpfs.
//!
//! Run in the Docker court:
//!   cargo run --release --example streaming_bench
//!
//! Receipt: `bench/receipts/streaming-<UTC ISO compact>.json`.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;

use weftfs::store::{Options, Store};

const SIZE: u64 = 256 * 1024 * 1024;
const CHUNK_SIZE: u64 = 65_536;
const BLOCK: u64 = 1024 * 1024;
const FIXED_TIMESTAMP_NS: i64 = 1_767_225_600_000_000_000; // 2026-01-01T00:00:00Z

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn fill_model(out: &mut [u8], offset: u64) {
    let mut pos = 0usize;
    while pos < out.len() {
        let absolute = offset + pos as u64;
        let lane = (absolute % 8) as usize;
        let block_salt = (absolute / BLOCK).wrapping_mul(0xD1B5_4A32_D192_ED03);
        let word = mix64((absolute / 8) ^ block_salt).to_le_bytes();
        let take = (8 - lane).min(out.len() - pos);
        out[pos..pos + take].copy_from_slice(&word[lane..lane + take]);
        pos += take;
    }
}

struct PatternReader {
    offset: u64,
    remaining: u64,
}

impl Read for PatternReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        let want = out
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        fill_model(&mut out[..want], self.offset);
        self.offset += want as u64;
        self.remaining -= want as u64;
        Ok(want)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Stats {
    mode: String,
    wall_ms: f64,
    rchar_bytes: u64,
    wchar_bytes: u64,
    peak_rss_kib: u64,
    size_bytes: u64,
    generation: u64,
    read_sample_ok: bool,
    verify_basic_ok: bool,
}

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        ..Options::default()
    }
}

fn io_bytes() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/io").expect("read /proc/self/io");
    let mut rchar = 0;
    let mut wchar = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rchar:") {
            rchar = rest.trim().parse().expect("rchar");
        } else if let Some(rest) = line.strip_prefix("wchar:") {
            wchar = rest.trim().parse().expect("wchar");
        }
    }
    (rchar, wchar)
}

fn vm_hwm_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
                .expect("parse VmHWM");
        }
    }
    panic!("VmHWM missing from /proc/self/status");
}

fn run_child(mode: &str) -> Stats {
    let dir: TempDir = tempfile::Builder::new()
        .prefix("weft-streaming-bench-")
        .tempdir_in(std::env::temp_dir())
        .expect("temp dir on the court tmpfs");
    let mut store = Store::init(dir.path(), opts()).expect("init store");
    let root = store.resolve_path("/").expect("root");

    // The buffered baseline's caller-side allocation is deliberately part of
    // its peak RSS, but generation time is excluded from both wall times.
    let buffered: Option<Vec<u8>> = if mode == "buffered" {
        let mut bytes = vec![0u8; SIZE as usize];
        fill_model(&mut bytes, 0);
        Some(bytes)
    } else {
        None
    };

    let (rchar_before, wchar_before) = io_bytes();
    let start = Instant::now();
    let id = match mode {
        "stream" => store
            .create_file_streaming(
                root,
                b"bench.bin",
                PatternReader {
                    offset: 0,
                    remaining: SIZE,
                },
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("streaming create"),
        "buffered" => store
            .create_file(
                root,
                b"bench.bin",
                buffered.as_deref().expect("buffered payload"),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("buffered create"),
        other => panic!("unknown child mode `{other}`"),
    };
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (rchar_after, wchar_after) = io_bytes();
    let peak_rss_kib = vm_hwm_kib();

    let meta = store.object_meta(id).expect("object meta");
    let sample = model_at(SIZE / 2, 4096);
    let read_sample_ok = store
        .read_range(id, SIZE / 2, 4096)
        .map(|bytes| bytes == sample)
        .unwrap_or(false);
    let verify_basic_ok = store.verify(false).map(|report| report.ok).unwrap_or(false);

    Stats {
        mode: mode.to_owned(),
        wall_ms,
        rchar_bytes: rchar_after.saturating_sub(rchar_before),
        wchar_bytes: wchar_after.saturating_sub(wchar_before),
        peak_rss_kib,
        size_bytes: meta.size,
        generation: store.generation(),
        read_sample_ok,
        verify_basic_ok,
    }
}

fn model_at(offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    fill_model(&mut out, offset);
    out
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn spawn_child(mode: &str) -> Stats {
    let exe = std::env::current_exe().expect("current executable");
    let output = Command::new(&exe)
        .arg(mode)
        .output()
        .unwrap_or_else(|err| panic!("spawn {mode} child: {err}"));
    assert!(
        output.status.success(),
        "{mode} child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "parse {mode} child JSON: {err}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn orchestrate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let streaming = spawn_child("stream");
    let buffered = spawn_child("buffered");

    let revision = env_or(
        "WEFT_REPO_WEFTFS",
        &command_stdout("git", &["rev-parse", "HEAD"]),
    );
    let timestamp = command_stdout("date", &["-u", "+%Y%m%dT%H%M%SZ"]);
    let receipt = json!({
        "label": "tmpfs (court /tmp, 1 GiB), synthetic splitmix64 generator, single host, Rust court; \
                  not a device, filesystem-mount, or production-workload claim",
        "schema": "weft.streaming-create.v1",
        "revision": revision,
        "revision_note": "measured on the streaming-change working tree whose base revision is this value; the commit that adds these files is the next HEAD",
        "court": {
            "image": env_or("WEFT_COURT_IMAGE", "unknown"),
            "image_id": env_or("WEFT_COURT_IMAGE_ID", "unknown"),
            "kernel": env_or("WEFT_COURT_KERNEL", &command_stdout("uname", &["-r"])),
            "limits": {
                "memory": env_or("WEFT_COURT_MEMORY", "unknown"),
                "cpus": env_or("WEFT_COURT_CPUS", "unknown"),
                "pids": env_or("WEFT_COURT_PIDS", "unknown"),
            },
        },
        "workload": {
            "file_size_bytes": SIZE,
            "chunk_size_bytes": CHUNK_SIZE,
            "generator": "splitmix64 words, 1 MiB block salt, every chunk distinct",
            "mode": "one fresh store per child; no fsync tuning; wall time around the create call only",
        },
        "streaming": streaming,
        "buffered": buffered,
        "peak_rss_ratio": if streaming.peak_rss_kib == 0 { 0.0 } else {
            buffered.peak_rss_kib as f64 / streaming.peak_rss_kib as f64
        },
        "wchar_ratio": if streaming.wchar_bytes == 0 { 0.0 } else {
            buffered.wchar_bytes as f64 / streaming.wchar_bytes as f64
        },
        "non_claims": [
            "Codec-backed streaming (ZSTD/DELTA) is not implemented; large CodecAuto streams use a v2 raw chunk manifest.",
            "read_object still materialises the whole file; range reads are chunk-bounded.",
            "Wall time includes this store's per-chunk blob fsync/rename publication on tmpfs."
        ],
        "receipt_timestamp_utc": timestamp,
    });

    let receipts = root.join("bench").join("receipts");
    std::fs::create_dir_all(&receipts).expect("create receipts dir");
    let path = receipts.join(format!("streaming-{timestamp}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!("{}", path.display());
    println!("{}", serde_json::to_string_pretty(&receipt).unwrap());
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("stream") => println!("{}", serde_json::to_string(&run_child("stream")).unwrap()),
        Some("buffered") => println!("{}", serde_json::to_string(&run_child("buffered")).unwrap()),
        Some(other) => panic!("unknown argument `{other}` (expected stream|buffered or none)"),
        None => orchestrate(),
    }
}
