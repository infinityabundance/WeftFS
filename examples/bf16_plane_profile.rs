//! R17 experiment 5: separate the BF16 plane split, the Huffman stages, and
//! the hashing inside the mode-9 `FLOAT_TENSOR` encoder.
//!
//! The stage timings come from a **harness-local replica** of the normative
//! algorithm (`huffman-canonical-v1` in `WeftCodec/src/huffman.rs` and
//! `split_float_tensor_planes` in `WeftCodec/src/modes.rs`). The replica's
//! bytes are compared with the shipped `weftcodec::huffman::encode` and
//! `weftcodec::modes::split_float_tensor_payload` on every measured tensor;
//! a mismatch is a hard failure, so no number here is reported from an
//! algorithm that differs from the codec.
//!
//! Scope label: one host, warm page cache, bench court hard limits, one public
//! model checkpoint. Not a fleet, media, or power-loss claim.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use weftcodec::hash::{hash_domain, DOMAIN_FLOAT_TENSOR_PAYLOAD};
use weftcodec::modes::FloatDtype;
use weftcodec::Limits;
use weftfs::safetensors::SafetensorsFile;

const FIXTURE_SHA256: &str = "fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe";
const EMBEDDING: &str = "model.embed_tokens.weight";

// --- replica of the normative Huffman algorithm --------------------------

#[derive(Clone, Copy)]
struct Node {
    weight: u64,
    min_symbol: u8,
    left: i32,
    right: i32,
    symbol: i32,
    active: bool,
}

fn node_key(node: &Node, index: usize) -> (u64, u8, usize) {
    (node.weight, node.min_symbol, index)
}

fn min_active(nodes: &[Node], exclude: Option<usize>) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, node) in nodes.iter().enumerate() {
        if !node.active || exclude == Some(index) {
            continue;
        }
        match best {
            None => best = Some(index),
            Some(current) => {
                if node_key(node, index) < node_key(&nodes[current], current) {
                    best = Some(index);
                }
            }
        }
    }
    best
}

fn huffman_lengths(weights: &[u64; 256], present: &[usize]) -> [u8; 256] {
    let mut nodes: Vec<Node> = present
        .iter()
        .map(|&symbol| Node {
            weight: weights[symbol],
            min_symbol: symbol as u8,
            left: -1,
            right: -1,
            symbol: symbol as i32,
            active: true,
        })
        .collect();
    let mut active = present.len();
    while active > 1 {
        let first = min_active(&nodes, None).expect("two active nodes");
        let second = min_active(&nodes, Some(first)).expect("second active node");
        nodes[first].active = false;
        nodes[second].active = false;
        let weight = nodes[first].weight + nodes[second].weight;
        let min_symbol = nodes[first].min_symbol.min(nodes[second].min_symbol);
        nodes.push(Node {
            weight,
            min_symbol,
            left: first as i32,
            right: second as i32,
            symbol: -1,
            active: true,
        });
        active -= 1;
    }
    let root = nodes
        .iter()
        .position(|node| node.active)
        .expect("one active root");
    let mut lengths = [0u8; 256];
    let mut stack = vec![(root, 0u16)];
    while let Some((index, depth)) = stack.pop() {
        let node = nodes[index];
        if node.symbol >= 0 {
            lengths[node.symbol as usize] = depth as u8;
        } else {
            stack.push((node.left as usize, depth + 1));
            stack.push((node.right as usize, depth + 1));
        }
    }
    lengths
}

fn build_lengths(weights: &[u64; 256]) -> [u8; 256] {
    let present: Vec<usize> = (0..256).filter(|&symbol| weights[symbol] > 0).collect();
    let mut lengths = [0u8; 256];
    if present.is_empty() {
        return lengths;
    }
    if present.len() == 1 {
        lengths[present[0]] = 1;
        return lengths;
    }
    let mut scaled = *weights;
    loop {
        let candidate = huffman_lengths(&scaled, &present);
        let max_len = candidate.iter().copied().max().unwrap_or(0) as usize;
        if max_len <= 24 {
            return candidate;
        }
        for &symbol in &present {
            let halved = scaled[symbol] >> 1;
            scaled[symbol] = if halved == 0 { 1 } else { halved };
        }
    }
}

fn canonical_codes(lengths: &[u8; 256]) -> [u32; 256] {
    let mut order: Vec<(u8, u8)> = (0..256)
        .filter(|&symbol| lengths[symbol] > 0)
        .map(|symbol| (lengths[symbol], symbol as u8))
        .collect();
    order.sort_unstable();
    let mut codes = [0u32; 256];
    let mut code = 0u32;
    let mut previous = 0u8;
    for (length, symbol) in order {
        code <<= u32::from(length - previous);
        codes[symbol as usize] = code;
        code += 1;
        previous = length;
    }
    codes
}

fn append_bits(out: &mut Vec<u8>, buffer: &mut u64, pending: &mut u32, code: u32, length: u32) {
    *buffer = (*buffer << length) | u64::from(code);
    *pending += length;
    while *pending >= 8 {
        *pending -= 8;
        out.push((*buffer >> *pending) as u8);
    }
    if *pending == 0 {
        *buffer = 0;
    } else {
        *buffer &= (1u64 << *pending) - 1;
    }
}

struct HuffmanStages {
    hist_ns: u128,
    lengths_ns: u128,
    codes_ns: u128,
    emit_ns: u128,
    output: Vec<u8>,
}

fn huffman_staged(input: &[u8]) -> HuffmanStages {
    let started = Instant::now();
    let mut weights = [0u64; 256];
    for &byte in input {
        weights[byte as usize] += 1;
    }
    let hist_ns = started.elapsed().as_nanos();

    let started = Instant::now();
    let lengths = build_lengths(&weights);
    let lengths_ns = started.elapsed().as_nanos();

    let used = lengths.iter().filter(|&&length| length > 0).count();
    let mut out = Vec::with_capacity(256 + input.len());
    out.extend_from_slice(&lengths);
    if used < 2 {
        return HuffmanStages {
            hist_ns,
            lengths_ns,
            codes_ns: 0,
            emit_ns: 0,
            output: out,
        };
    }

    let started = Instant::now();
    let codes = canonical_codes(&lengths);
    let codes_ns = started.elapsed().as_nanos();

    let started = Instant::now();
    let mut buffer = 0u64;
    let mut pending = 0u32;
    for &byte in input {
        let length = u32::from(lengths[byte as usize]);
        append_bits(
            &mut out,
            &mut buffer,
            &mut pending,
            codes[byte as usize],
            length,
        );
    }
    if pending > 0 {
        out.push((buffer << (8 - pending)) as u8);
    }
    let emit_ns = started.elapsed().as_nanos();
    HuffmanStages {
        hist_ns,
        lengths_ns,
        codes_ns,
        emit_ns,
        output: out,
    }
}

// --- replica of the mode-9 BF16 split ------------------------------------

fn push_bits(out: &mut Vec<u8>, bit_len: &mut usize, value: u64, count: usize) {
    for shift in (0..count).rev() {
        let bit = ((value >> shift) & 1) as u8;
        let byte_index = *bit_len / 8;
        if byte_index == out.len() {
            out.push(0);
        }
        out[byte_index] |= bit << (7 - (*bit_len % 8));
        *bit_len += 1;
    }
}

fn split_bf16(payload: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if payload.len() % 2 != 0 {
        return Err("bf16 payload is not an even number of bytes".to_owned());
    }
    let count = payload.len() / 2;
    let mut exponent_plane = Vec::with_capacity(count.min(1 << 20));
    let mut mantissa_plane = Vec::new();
    let mut bit_len = 0usize;
    for chunk in payload.chunks_exact(2) {
        let word = u64::from(chunk[0]) | (u64::from(chunk[1]) << 8);
        let exponent = (word >> 7) & 0xff;
        let mantissa = word & 0x7f;
        let sign = (word >> 15) & 1;
        exponent_plane.push(exponent as u8);
        push_bits(&mut mantissa_plane, &mut bit_len, sign, 1);
        push_bits(&mut mantissa_plane, &mut bit_len, mantissa, 7);
    }
    Ok((exponent_plane, mantissa_plane))
}

// --- harness --------------------------------------------------------------

#[derive(Clone, Debug)]
struct Args {
    fixture: PathBuf,
    out: PathBuf,
    others: usize,
    reps: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut fixture = None;
    let mut out = None;
    let mut others = 8usize;
    let mut reps = 5usize;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--fixture" => fixture = Some(PathBuf::from(value("--fixture")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--others" => {
                others = value("--others")?
                    .parse()
                    .map_err(|_| "--others must be an integer".to_owned())?
            }
            "--reps" => {
                reps = value("--reps")?
                    .parse()
                    .map_err(|_| "--reps must be an integer".to_owned())?
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    Ok(Args {
        fixture: fixture.ok_or("--fixture is required")?,
        out: out.ok_or("--out is required")?,
        others,
        reps: reps.max(1),
    })
}

fn median(values: &mut Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return 0.0;
    }
    values[values.len() / 2]
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    let mut file = File::create(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|err| err.to_string())?;
    file.write_all(b"\n").map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())
}

fn uname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("bf16_plane_profile: {err}");
            std::process::exit(64);
        }
    };
    if let Err(err) = run(&args) {
        eprintln!("bf16_plane_profile: {err}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let file = SafetensorsFile::parse(&args.fixture)
        .map_err(|err| format!("{}: {err}", args.fixture.display()))?;
    let sha = hex::encode(file.sha256);
    if sha != FIXTURE_SHA256 {
        return Err(format!(
            "fixture SHA-256 mismatch: expected {FIXTURE_SHA256}, got {sha}"
        ));
    }

    if !file.tensors.iter().any(|tensor| tensor.name == EMBEDDING) {
        return Err(format!("tensor `{EMBEDDING}` is missing"));
    }
    let mut chosen: Vec<usize> = vec![file
        .tensors
        .iter()
        .position(|tensor| tensor.name == EMBEDDING)
        .unwrap()];
    let others: Vec<usize> = (0..file.tensors.len())
        .filter(|&index| file.tensors[index].name != EMBEDDING)
        .collect();
    // Pick the largest real tensors, not the first eight by header order: the
    // header groups the layernorm vectors together, and a sample of 1,792-byte
    // tensors would not exercise the encoder.
    let mut others = others;
    others.sort_by(|a, b| {
        file.tensors[*b]
            .byte_len()
            .cmp(&file.tensors[*a].byte_len())
    });
    let take = args.others.min(others.len());
    for slot in 0..take {
        let index = if take == 0 { 0 } else { slot };
        chosen.push(others[index]);
    }

    let mut rows: Vec<Json> = Vec::new();
    for index in chosen {
        let tensor = &file.tensors[index];
        if tensor.dtype != "bf16" {
            return Err(format!(
                "tensor `{}` is {} (this experiment is BF16-only)",
                tensor.name, tensor.dtype
            ));
        }
        let mut bytes = vec![0u8; tensor.byte_len() as usize];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read `{}`: {err}", tensor.name))?;
        let count = (bytes.len() / 2) as u64;
        let limits = Limits::default();

        // Validate the replicas once on this exact tensor.
        let (exponent, mantissa) = split_bf16(&bytes)?;
        let official =
            weftcodec::modes::split_float_tensor_payload(&bytes, FloatDtype::Bf16, count)
                .map_err(|err| format!("codec split `{}`: {err}", tensor.name))?;
        if official.0 != weftcodec::huffman::encode(&exponent) || official.1 != mantissa {
            return Err(format!(
                "harness split replica differs from the codec on `{}`",
                tensor.name
            ));
        }
        let staged = huffman_staged(&exponent);
        if staged.output != official.0 {
            return Err(format!(
                "harness huffman replica differs from the codec on `{}`",
                tensor.name
            ));
        }

        let mut split_ms = Vec::new();
        let mut huffman_ms = Vec::new();
        let mut hist_ms = Vec::new();
        let mut lengths_ms = Vec::new();
        let mut codes_ms = Vec::new();
        let mut emit_ms = Vec::new();
        let mut values_hash_ms = Vec::new();
        let mut payload_hash_ms = Vec::new();
        let mut content_hash_ms = Vec::new();
        let mut codec_total_ms = Vec::new();
        let mut codec_frame_bytes = 0u64;

        for _ in 0..args.reps {
            let started = Instant::now();
            let (exponent, mantissa) = split_bf16(&bytes)?;
            split_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let started = Instant::now();
            let official = weftcodec::huffman::encode(&exponent);
            huffman_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            if official != staged.output {
                return Err(format!("huffman replica drift on `{}`", tensor.name));
            }

            let stage_times = huffman_staged(&exponent);
            hist_ms.push(stage_times.hist_ns as f64 / 1_000_000.0);
            lengths_ms.push(stage_times.lengths_ns as f64 / 1_000_000.0);
            codes_ms.push(stage_times.codes_ns as f64 / 1_000_000.0);
            emit_ms.push(stage_times.emit_ns as f64 / 1_000_000.0);

            let started = Instant::now();
            let _ = weftcodec::tensor::values_hash(&bytes);
            values_hash_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let stored: Vec<u8> = official.iter().chain(mantissa.iter()).copied().collect();
            let started = Instant::now();
            let _ = hash_domain(DOMAIN_FLOAT_TENSOR_PAYLOAD, &stored);
            payload_hash_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(stored);

            let started = Instant::now();
            let _ = hash_domain(weftcodec::hash::DOMAIN_CHUNK, &bytes);
            content_hash_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let started = Instant::now();
            let frame = weftcodec::modes::encode_float_tensor(
                &bytes,
                FloatDtype::Bf16,
                &tensor.shape,
                &limits,
            )
            .map_err(|err| format!("encode `{}`: {err}", tensor.name))?;
            codec_total_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            codec_frame_bytes = frame.len() as u64;
        }

        let split = median(&mut split_ms);
        let huffman = median(&mut huffman_ms);
        let hist = median(&mut hist_ms);
        let lengths = median(&mut lengths_ms);
        let codes = median(&mut codes_ms);
        let emit = median(&mut emit_ms);
        let values_hash = median(&mut values_hash_ms);
        let payload_hash = median(&mut payload_hash_ms);
        let content_hash = median(&mut content_hash_ms);
        let codec_total = median(&mut codec_total_ms);
        rows.push(json!({
            "tensor": tensor.name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "elements": count,
            "logical_bytes": bytes.len(),
            "reps": args.reps,
            "split_ms": split,
            "huffman_total_ms": huffman,
            "huffman_histogram_ms": hist,
            "huffman_lengths_ms": lengths,
            "huffman_codes_ms": codes,
            "huffman_emit_ms": emit,
            "huffman_stage_sum_ms": hist + lengths + codes + emit,
            "values_hash_ms": values_hash,
            "payload_hash_ms": payload_hash,
            "content_hash_ms": content_hash,
            "codec_total_ms": codec_total,
            "codec_frame_overhead_ms": (codec_total - split - huffman - payload_hash - content_hash - values_hash).max(0.0),
            "exponent_huffman_bytes": official.0.len(),
            "mantissa_bytes": mantissa.len(),
            "codec_frame_bytes": codec_frame_bytes,
            "huffman_ratio": (official.0.len() + mantissa.len()) as f64 / bytes.len() as f64,
            "replica_validated": true,
        }));
    }

    let totals: f64 = rows
        .iter()
        .map(|row| row["logical_bytes"].as_f64().unwrap_or(0.0))
        .sum();
    let split_total: f64 = rows
        .iter()
        .map(|row| row["split_ms"].as_f64().unwrap_or(0.0))
        .sum();
    let huffman_total: f64 = rows
        .iter()
        .map(|row| row["huffman_total_ms"].as_f64().unwrap_or(0.0))
        .sum();
    let hist_total: f64 = rows
        .iter()
        .map(|row| row["huffman_histogram_ms"].as_f64().unwrap_or(0.0))
        .sum();
    let codes_total: f64 = rows
        .iter()
        .map(|row| row["huffman_codes_ms"].as_f64().unwrap_or(0.0))
        .sum();
    let emit_total: f64 = rows
        .iter()
        .map(|row| row["huffman_emit_ms"].as_f64().unwrap_or(0.0))
        .sum();
    let lengths_total: f64 = rows
        .iter()
        .map(|row| row["huffman_lengths_ms"].as_f64().unwrap_or(0.0))
        .sum();

    let receipt = json!({
        "schema": "weft.bf16_plane_profile.v1",
        "generated_utc": utc_stamp(),
        "environment": {
            "court": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "outside-court".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| uname()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftfs_revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftcodec_revision": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "fixture": {
            "path": args.fixture.display().to_string(),
            "bytes": file.file_bytes,
            "sha256": sha,
        },
        "method": {
            "replica": "stage timings are harness-local replicas of the normative huffman-canonical-v1 and bf16 plane split; every measured tensor is checked byte-for-byte against the shipped codec before any number is recorded",
            "reps": args.reps,
            "statistic": "median of repetitions per tensor",
            "codec_total": "weftcodec::modes::encode_float_tensor, which includes split, huffman, payload/content hashing and frame assembly",
        },
        "tensors": rows,
        "totals": {
            "logical_bytes": totals,
            "split_ms": split_total,
            "huffman_total_ms": huffman_total,
            "huffman_histogram_ms": hist_total,
            "huffman_lengths_ms": lengths_total,
            "huffman_codes_ms": codes_total,
            "huffman_emit_ms": emit_total,
            "huffman_stage_sum_ms": hist_total + lengths_total + codes_total + emit_total,
        },
        "not_claimed": [
            "single host, warm page cache",
            "the stage split is a harness replication validated against the codec, not a codec-internal profile",
            "no encoder optimisation is proposed or measured here",
        ],
    });
    write_json(&args.out, &receipt)?;
    println!("wrote {}", args.out.display());
    Ok(())
}
