//! R22 chunk-parallel ZSTD bench: the level-3 (and optional level-19) frame of
//! one real tensor, sequential against chunk-parallel.
//!
//! The claim this harness measures is narrow and exact: **the same frame**, built
//! by encoding the chunk frames on `threads` workers in ordered batches. Every
//! parallel frame is compared byte for byte with the sequential frame of the
//! same input before its time is recorded, so a timing row can only come from
//! bytes that equal the shipped encoder's output.
//!
//! Scope label: one host, warm page cache, bench court hard limits (3 GiB /
//! 4 CPUs), one public model checkpoint. Not a fleet or media claim.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use weftcodec::modes::{self, ZstdEncodeOptions};
use weftcodec::Limits;
use weftfs::safetensors::SafetensorsFile;

/// Codec frame chunk size the store derives from a 1 MiB store chunk.
const DEFAULT_CODEC_CHUNK: usize = 64 * 1024;

struct Args {
    fixture: PathBuf,
    out: PathBuf,
    tensors: Vec<String>,
    max_others: usize,
    levels: Vec<i32>,
    threads: Vec<usize>,
    batch_kib: Vec<u64>,
    chunk: usize,
    reps: usize,
    stamp: String,
}

fn stamp_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil(seconds);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

fn civil(mut secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = secs % 60;
    secs /= 60;
    let mi = secs % 60;
    secs /= 60;
    let h = secs % 24;
    let mut days = secs / 24;
    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if days < len {
            break;
        }
        days -= len;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= months[month] {
        days -= months[month];
        month += 1;
    }
    (year, month as u64 + 1, days + 1, h, mi, s)
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut fixture = PathBuf::new();
    let mut out = PathBuf::from("/tmp/zstd-chunk-bench.json");
    let mut tensors = vec!["model.embed_tokens.weight".to_owned()];
    let mut max_others = 0usize;
    let mut levels = vec![3i32];
    let mut threads = vec![1usize, 2, 3, 4];
    let mut batch_kib = vec![32 * 1024u64];
    let mut chunk = DEFAULT_CODEC_CHUNK;
    let mut reps = 3usize;
    let mut stamp = String::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let mut value = || -> Result<String, String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--fixture" => fixture = PathBuf::from(value()?),
            "--out" => out = PathBuf::from(value()?),
            "--tensors" => {
                tensors = value()?
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            "--max-others" => max_others = value()?.parse().map_err(|_| "--max-others")?,
            "--levels" => {
                levels = value()?
                    .split(',')
                    .map(|part| part.parse::<i32>().map_err(|_| "--levels"))
                    .collect::<Result<Vec<_>, _>>()?
            }
            "--threads" => {
                threads = value()?
                    .split(',')
                    .map(|part| part.parse::<usize>().map_err(|_| "--threads"))
                    .collect::<Result<Vec<_>, _>>()?
            }
            "--batch-kib" => {
                batch_kib = value()?
                    .split(',')
                    .map(|part| part.parse::<u64>().map_err(|_| "--batch-kib"))
                    .collect::<Result<Vec<_>, _>>()?
            }
            "--chunk" => chunk = value()?.parse().map_err(|_| "--chunk")?,
            "--reps" => reps = value()?.parse().map_err(|_| "--reps")?,
            "--stamp" => stamp = value()?,
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    if fixture.as_os_str().is_empty() {
        return Err("--fixture is required".to_owned());
    }
    if stamp.is_empty() {
        stamp = stamp_now();
    }
    Ok(Args {
        fixture,
        out,
        tensors,
        max_others,
        levels,
        threads,
        batch_kib,
        chunk,
        reps,
        stamp,
    })
}

fn peak_rss_kib() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
        }
    }
    0
}

fn cpu_seconds() -> f64 {
    let Ok(text) = fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(close) = text.rfind(") ") else {
        return 0.0;
    };
    let fields: Vec<&str> = text[close + 2..].split_whitespace().collect();
    let utime: u64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    let ticks = 100.0f64;
    (utime + stime) as f64 / ticks
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let position = fraction * (sorted.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    let weight = position - low as f64;
    sorted[low] * (1.0 - weight) + sorted[high] * weight
}

fn run(args: &Args) -> Result<(), String> {
    let file = SafetensorsFile::parse(&args.fixture)
        .map_err(|err| format!("{}: {err}", args.fixture.display()))?;
    let fixture_sha256 = hex::encode(file.sha256);
    let fixture_bytes = file.file_bytes;

    // The requested tensors, plus the largest others when asked.
    let mut chosen: Vec<usize> = Vec::new();
    for name in &args.tensors {
        let position = file
            .tensors
            .iter()
            .position(|tensor| &tensor.name == name)
            .ok_or_else(|| format!("tensor `{name}` is missing from the fixture"))?;
        chosen.push(position);
    }
    if args.max_others > 0 {
        let mut others: Vec<usize> = (0..file.tensors.len())
            .filter(|index| !chosen.contains(index))
            .collect();
        others.sort_by(|a, b| {
            file.tensors[*b]
                .byte_len()
                .cmp(&file.tensors[*a].byte_len())
                .then_with(|| file.tensors[*a].name.cmp(&file.tensors[*b].name))
        });
        chosen.extend(others.into_iter().take(args.max_others));
    }

    let limits = Limits::default();
    let mut rows: Vec<Json> = Vec::new();
    let mut identity_checks = 0u64;

    for &index in &chosen {
        let tensor = &file.tensors[index];
        if tensor.dtype != "bf16" {
            eprintln!(
                "zstd_chunk_bench: skipping `{}` (dtype {}); this bench is BF16",
                tensor.name, tensor.dtype
            );
            continue;
        }
        let len = tensor.byte_len() as usize;
        let mut bytes = vec![0u8; len];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read {}: {err}", tensor.name))?;

        for &level in &args.levels {
            // Sequential baseline first: it is the identity oracle.
            let mut sequential_ms: Vec<f64> = Vec::new();
            let mut sequential_frame = Vec::new();
            for _ in 0..args.reps {
                let cpu_before = cpu_seconds();
                let started = Instant::now();
                let frame = modes::encode_zstd(&bytes, level, Some(args.chunk), &limits)
                    .map_err(|err| format!("sequential encode: {err}"))?;
                sequential_ms.push(started.elapsed().as_secs_f64() * 1000.0);
                let _ = cpu_before;
                sequential_frame = frame;
            }
            sequential_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());

            for &threads in &args.threads {
                for &batch_kib in &args.batch_kib {
                    let options = ZstdEncodeOptions {
                        threads,
                        parallel_min_bytes: 0,
                        batch_decoded_bytes: (batch_kib as usize).saturating_mul(1024),
                    };
                    let mut ms: Vec<f64> = Vec::new();
                    let mut identical = true;
                    let mut frame_bytes = 0usize;
                    let mut chunks = 0usize;
                    let mut cpu_total = 0f64;
                    for _ in 0..args.reps {
                        let cpu_before = cpu_seconds();
                        let started = Instant::now();
                        let frame = modes::encode_zstd_with(
                            &bytes,
                            level,
                            Some(args.chunk),
                            &limits,
                            &options,
                        )
                        .map_err(|err| format!("parallel encode: {err}"))?;
                        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                        cpu_total += cpu_seconds() - cpu_before;
                        ms.push(elapsed);
                        frame_bytes = frame.len();
                        if frame != sequential_frame {
                            identical = false;
                        }
                        if let Ok(parsed) = weftcodec::Frame::parse(&frame, &limits) {
                            chunks = parsed.chunks.len();
                        }
                        identity_checks += 1;
                    }
                    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let sequential_median = percentile(&sequential_ms, 0.5);
                    let parallel_median = percentile(&ms, 0.5);
                    rows.push(json!({
                        "name": tensor.name,
                        "dtype": tensor.dtype,
                        "shape": tensor.shape,
                        "logical_bytes": len,
                        "level": level,
                        "codec_chunk_bytes": args.chunk,
                        "chunks": chunks,
                        "threads": threads,
                        "batch_kib": batch_kib,
                        "frame_bytes": frame_bytes,
                        "samples_ms": ms,
                        "min_ms": ms.first().copied().unwrap_or(0.0),
                        "median_ms": parallel_median,
                        "p95_ms": percentile(&ms, 0.95),
                        "cpu_seconds_per_rep": cpu_total / args.reps as f64,
                        "megabytes_per_second": if parallel_median > 0.0 {
                            (len as f64 / 1_000_000.0) / (parallel_median / 1000.0)
                        } else { 0.0 },
                        "sequential_median_ms": sequential_median,
                        "speedup_vs_sequential": if parallel_median > 0.0 {
                            sequential_median / parallel_median
                        } else { 0.0 },
                        "byte_identical": identical,
                    }));
                }
            }
        }
    }

    let all_identical = identity_checks > 0
        && rows
            .iter()
            .all(|row| row["byte_identical"].as_bool().unwrap_or(false));
    let receipt = json!({
        "schema": "weft.zstd-chunk-parallel-bench.v1",
        "stamp": args.stamp,
        "role": "R22 22.1: the measured saving of chunk-parallel ZSTD on real tensors, and the byte-identity proof of every timed frame",
        "fixture": args.fixture.display().to_string(),
        "fixture_bytes": fixture_bytes,
        "fixture_sha256": fixture_sha256,
        "reps": args.reps,
        "rows": rows,
        "identity_checks": identity_checks,
        "all_frames_byte_identical": all_identical,
        "peak_rss_kib": peak_rss_kib(),
        "environment": {
            "court": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "host".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "note": "one host, warm page cache; timings are wall clock over whole frames",
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "result": if all_identical { "pass" } else { "fail" },
    });
    if !all_identical {
        return Err("a parallel frame differed from the sequential frame".to_owned());
    }
    write_json(&args.out, &receipt)?;
    println!("zstd_chunk_bench: {}", args.out.display());
    Ok(())
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    fs::write(path, text + "\n").map_err(|err| format!("{}: {err}", path.display()))
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(raw) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("zstd_chunk_bench: {err}");
            std::process::exit(64);
        }
    };
    match run(&args) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("zstd_chunk_bench: {err}");
            std::process::exit(1);
        }
    }
}
