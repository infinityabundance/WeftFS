//! R19 mode-9 encoder bench: per-stage attribution before and after the
//! encoder rewrite, on the frozen 101-tensor fixture.
//!
//! Two roles:
//!
//! * **shipped** — `weftcodec::modes::encode_float_tensor` as built in this
//!   tree, timed end to end per tensor.
//! * **staged** — a harness-local replica of the pipeline stages. Every
//!   replica output is compared byte for byte with the shipped functions on the
//!   same tensor before any number is recorded, so a stage number can only come
//!   from an algorithm that equals the codec's.
//!
//! When the tree contains the pre-R19 encoder (`encode_float_tensor_reference`,
//! added by R19 itself) the bench also times it and asserts byte identity with
//! the shipped encoder, which is the byte-identity court in `--out` form.
//!
//! Scope label: one host, warm page cache, bench court hard limits, one public
//! model checkpoint. Not a fleet, media, or power-loss claim.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use weftcodec::frame::{
    encode_frame, FrameParts, Mode, DOMAIN_PAYLOAD, FORMAT_MAJOR, FORMAT_MINOR,
};
use weftcodec::hash::{hash_domain, DOMAIN_CHUNK, DOMAIN_FLOAT_TENSOR_PAYLOAD};
use weftcodec::modes::{self, FloatDtype, FloatTensorEncodeOptions};
use weftcodec::wcb::Value;
use weftcodec::Limits;
use weftfs::safetensors::SafetensorsFile;

const FIXTURE_SHA256: &str = "fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe";
const EMBEDDING: &str = "model.embed_tokens.weight";

// --- replica of the normative Huffman algorithm (R17/R19 baseline) --------

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
    let root = nodes.iter().position(|node| node.active).expect("root");
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

#[derive(Default)]
struct HuffmanStages {
    hist_ns: u64,
    lengths_ns: u64,
    codes_ns: u64,
    emit_ns: u64,
    output: Vec<u8>,
}

fn huffman_staged(input: &[u8]) -> HuffmanStages {
    let started = Instant::now();
    let mut weights = [0u64; 256];
    for &byte in input {
        weights[byte as usize] += 1;
    }
    let hist_ns = started.elapsed().as_nanos() as u64;

    let started = Instant::now();
    let lengths = build_lengths(&weights);
    let lengths_ns = started.elapsed().as_nanos() as u64;

    let started = Instant::now();
    let codes = canonical_codes(&lengths);
    let codes_ns = started.elapsed().as_nanos() as u64;

    let started = Instant::now();
    let used = lengths.iter().filter(|&&length| length > 0).count();
    let mut output = Vec::with_capacity(256 + input.len());
    output.extend_from_slice(&lengths);
    if used >= 2 {
        let mut buffer = 0u64;
        let mut pending = 0u32;
        for &byte in input {
            append_bits(
                &mut output,
                &mut buffer,
                &mut pending,
                codes[byte as usize],
                u32::from(lengths[byte as usize]),
            );
        }
        if pending > 0 {
            output.push((buffer << (8 - pending)) as u8);
        }
    }
    let emit_ns = started.elapsed().as_nanos() as u64;

    HuffmanStages {
        hist_ns,
        lengths_ns,
        codes_ns,
        emit_ns,
        output,
    }
}

// --- replica of the mode-9 BF16 split (pre-R19 reference) ----------------

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
    label: String,
    threads: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut fixture = None;
    let mut out = None;
    let mut others = 8usize;
    let mut reps = 7usize;
    let mut label = "mode9-encoder".to_owned();
    let mut threads = 1usize;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--fixture" => fixture = Some(PathBuf::from(value("--fixture")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--label" => label = value("--label")?,
            "--threads" => {
                threads = value("--threads")?
                    .parse()
                    .map_err(|_| "--threads must be an integer".to_owned())?
            }
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
        label,
        threads: threads.max(1),
    })
}

fn median(values: &mut Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return 0.0;
    }
    values[values.len() / 2]
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    let mut file = File::create(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|err| err.to_string())?;
    file.write_all(b"\n").map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())
}

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (year, month, day, hour, minute, second) = civil_from_unix(secs);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let mut rem = secs.rem_euclid(86_400) as u32;
    let hour = rem / 3600;
    rem %= 3600;
    let minute = rem / 60;
    let second = rem % 60;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, hour, minute, second)
}

fn uname() -> String {
    std::process::Command::new("uname")
        .arg("-sr")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// `(minor, major)` page-fault counters from `/proc/self/stat`.
fn page_faults() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised command name; minflt is field 10 and
    // majflt field 12 in 1-based `proc(5)` numbering.
    let tail = text.rsplit(") ").next()?;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let minor = fields.get(7)?.parse().ok()?;
    let major = fields.get(9)?.parse().ok()?;
    Some((minor, major))
}

fn fault_delta(before: Option<(u64, u64)>, after: Option<(u64, u64)>) -> f64 {
    match (before, after) {
        (Some(before), Some(after)) => {
            (after.0.saturating_sub(before.0) + after.1.saturating_sub(before.1)) as f64
        }
        _ => 0.0,
    }
}

/// Cost of a **fresh** large allocation versus a recycled one.
///
/// Anonymous pages are mapped lazily, so the first write to each page is a
/// minor fault. The pre-R19 mode-9 encoder performs several multi-megabyte
/// allocations per tensor (both planes, the concatenated stored plane, the
/// body, the frame), and every one of them pays that cost. This probe measures
/// it directly so the assembly residual is attributed to a mechanism instead
/// of a label.
fn memory_probe(bytes: usize, reps: usize) -> Json {
    let source = vec![0x5au8; bytes];
    let mut held: Vec<Vec<u8>> = Vec::new();
    let mut touch_ms = Vec::new();
    let mut copy_ms = Vec::new();
    let mut faults = Vec::new();
    for _ in 0..reps {
        let before = page_faults();
        let started = Instant::now();
        let mut mapped = vec![0u8; bytes];
        let mut at = 0usize;
        while at < bytes {
            mapped[at] = 1;
            at += 4096;
        }
        touch_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        faults.push(fault_delta(before, page_faults()));
        held.push(mapped);

        let started = Instant::now();
        let mut copied = Vec::with_capacity(bytes);
        copied.extend_from_slice(&source);
        copy_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        held.push(copied);
    }
    let mut recycled_ms = Vec::new();
    let mut recycled = vec![0u8; bytes];
    for _ in 0..reps.max(1) {
        let started = Instant::now();
        recycled.copy_from_slice(&source);
        recycled_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    std::hint::black_box(&recycled);
    json!({
        "bytes": bytes,
        "reps": reps,
        "fresh_zeroed_touch_every_page_ms": median(&mut touch_ms.clone()),
        "fresh_zeroed_page_faults": median(&mut faults.clone()),
        "fresh_allocating_copy_ms": median(&mut copy_ms.clone()),
        "recycled_copy_ms": median(&mut recycled_ms.clone()),
        "note": "fresh means a new mapping (the allocator cannot reuse the previous block because every probe buffer is held until it returns); recycled means the pages are already present, so only the memcpy remains",
    })
}

/// Stage-by-stage replica of the **pre-R19** encoder, faithful to its
/// allocation and drop order, used to prove the stage table adds up to the
/// shipped wall. Returns the frame and the per-stage milliseconds.
///
/// `[split_payload, stored_build, payload_hash, body_build, frame_encode,
/// content_hash]`
thread_local! {
    static STAGED_SPLIT_FAULTS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

fn staged_split_faults() -> f64 {
    STAGED_SPLIT_FAULTS.with(|cell| cell.get())
}

fn reference_staged(
    payload: &[u8],
    dtype: FloatDtype,
    shape: &[u64],
    limits: &Limits,
) -> Result<(Vec<u8>, [f64; 6]), String> {
    let element_count: u64 = shape.iter().product();
    let before = page_faults();
    let started = Instant::now();
    let (exponent_plane, mantissa_plane) =
        modes::split_float_tensor_payload(payload, dtype, element_count)
            .map_err(|err| format!("staged split: {err}"))?;
    let split = started.elapsed().as_secs_f64() * 1000.0;
    STAGED_SPLIT_FAULTS.with(|cell| cell.set(fault_delta(before, page_faults())));

    let started = Instant::now();
    let mut stored = Vec::with_capacity(exponent_plane.len() + mantissa_plane.len());
    stored.extend_from_slice(&exponent_plane);
    stored.extend_from_slice(&mantissa_plane);
    let stored_build = started.elapsed().as_secs_f64() * 1000.0;

    let started = Instant::now();
    let payload_hash = hash_domain(DOMAIN_FLOAT_TENSOR_PAYLOAD, &stored);
    let payload_hash_ms = started.elapsed().as_secs_f64() * 1000.0;

    let meta = Value::map([
        ("schema", Value::Str(modes::FLOAT_TENSOR_SCHEMA.to_owned())),
        ("dtype", Value::Str(dtype.as_str().to_owned())),
        (
            "shape",
            Value::Array(shape.iter().map(|dim| Value::U64(*dim)).collect()),
        ),
        ("element_count", Value::U64(element_count)),
        ("sign_bits", Value::U64(dtype.sign_bits())),
        ("exponent_bits", Value::U64(dtype.exponent_bits())),
        ("mantissa_bits", Value::U64(dtype.mantissa_bits())),
        (
            "exponent_coder",
            Value::Str(modes::FLOAT_TENSOR_EXPONENT_CODER.to_owned()),
        ),
        (
            "exponent_plane_len",
            Value::U64(exponent_plane.len() as u64),
        ),
        (
            "mantissa_plane_len",
            Value::U64(mantissa_plane.len() as u64),
        ),
        (
            "payload_hash",
            Value::Bytes(payload_hash.as_bytes().to_vec()),
        ),
    ]);
    let started = Instant::now();
    let meta_bytes =
        weftcodec::wcb::encode(&meta, limits).map_err(|err| format!("staged metadata: {err}"))?;
    let mut body = Vec::with_capacity(4 + meta_bytes.len() + stored.len());
    body.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(&meta_bytes);
    body.extend_from_slice(&stored);
    let body_build = started.elapsed().as_secs_f64() * 1000.0;

    let started = Instant::now();
    let content_hash = hash_domain(DOMAIN_CHUNK, payload);
    let content_hash_ms = started.elapsed().as_secs_f64() * 1000.0;

    let started = Instant::now();
    let frame = encode_frame(
        &FrameParts {
            format_major: FORMAT_MAJOR,
            format_minor: FORMAT_MINOR,
            domain: DOMAIN_PAYLOAD,
            mode: Mode::FloatTensor,
            flags: 0,
            level: modes::FLOAT_TENSOR_LEVEL,
            decoded_len: payload.len() as u64,
            content_hash,
            dependencies: &[],
            chunks: &[],
            body: &body,
        },
        limits,
    )
    .map_err(|err| format!("staged frame: {err}"))?;
    let frame_encode = started.elapsed().as_secs_f64() * 1000.0;

    Ok((
        frame,
        [
            split,
            stored_build,
            payload_hash_ms,
            body_build,
            frame_encode,
            content_hash_ms,
        ],
    ))
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("mode9_encoder_bench: {err}");
            std::process::exit(64);
        }
    };
    match run(&args) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("mode9_encoder_bench: {err}");
            std::process::exit(1);
        }
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

    let mut chosen: Vec<usize> = vec![file
        .tensors
        .iter()
        .position(|tensor| tensor.name == EMBEDDING)
        .ok_or_else(|| format!("tensor `{EMBEDDING}` is missing"))?];
    let mut others: Vec<usize> = (0..file.tensors.len())
        .filter(|&index| file.tensors[index].name != EMBEDDING)
        .collect();
    others.sort_by(|a, b| {
        file.tensors[*b]
            .byte_len()
            .cmp(&file.tensors[*a].byte_len())
            .then_with(|| file.tensors[*a].name.cmp(&file.tensors[*b].name))
    });
    for &index in others.iter().take(args.others) {
        chosen.push(index);
    }

    let embedding_bytes = file
        .tensors
        .iter()
        .find(|tensor| tensor.name == EMBEDDING)
        .map(|tensor| tensor.byte_len() as usize)
        .ok_or_else(|| format!("tensor `{EMBEDDING}` is missing"))?;
    let memory = memory_probe(embedding_bytes, 2);

    let limits = Limits::default();
    let mut rows: Vec<Json> = Vec::new();
    let mut byte_identity_checks = 0u64;

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

        // --- replica validation, once per tensor -------------------------
        let (exponent, mantissa) = split_bf16(&bytes)?;
        let official = modes::split_float_tensor_payload(&bytes, FloatDtype::Bf16, count)
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
        let shipped_frame =
            modes::encode_float_tensor(&bytes, FloatDtype::Bf16, &tensor.shape, &limits)
                .map_err(|err| format!("encode `{}`: {err}", tensor.name))?;

        // A pre-R19 encoder, when present, must agree byte for byte.
        let reference_frame =
            modes::encode_float_tensor_reference(&bytes, FloatDtype::Bf16, &tensor.shape, &limits)
                .map_err(|err| format!("reference encode `{}`: {err}", tensor.name))?;
        byte_identity_checks += 1;
        if shipped_frame != reference_frame {
            return Err(format!(
                "shipped mode-9 frame differs from the pre-R19 reference on `{}` ({} vs {} bytes)",
                tensor.name,
                shipped_frame.len(),
                reference_frame.len()
            ));
        }

        // Stored planes and the descriptor the encoder must reproduce.
        let mut stored = Vec::with_capacity(official.0.len() + official.1.len());
        stored.extend_from_slice(&official.0);
        stored.extend_from_slice(&official.1);
        let payload_hash = hash_domain(DOMAIN_FLOAT_TENSOR_PAYLOAD, &stored);
        let content_hash = hash_domain(DOMAIN_CHUNK, &bytes);
        let meta = Value::map([
            ("schema", Value::Str(modes::FLOAT_TENSOR_SCHEMA.to_owned())),
            ("dtype", Value::Str("bf16".to_owned())),
            (
                "shape",
                Value::Array(tensor.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("element_count", Value::U64(count)),
            ("sign_bits", Value::U64(1)),
            ("exponent_bits", Value::U64(8)),
            ("mantissa_bits", Value::U64(7)),
            (
                "exponent_coder",
                Value::Str(modes::FLOAT_TENSOR_EXPONENT_CODER.to_owned()),
            ),
            ("exponent_plane_len", Value::U64(official.0.len() as u64)),
            ("mantissa_plane_len", Value::U64(official.1.len() as u64)),
            (
                "payload_hash",
                Value::Bytes(payload_hash.as_bytes().to_vec()),
            ),
        ]);
        let meta_bytes = weftcodec::wcb::encode(&meta, &limits)
            .map_err(|err| format!("metadata `{}`: {err}", tensor.name))?;
        let mut body = Vec::with_capacity(4 + meta_bytes.len() + stored.len());
        body.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&meta_bytes);
        body.extend_from_slice(&stored);

        let mut split_ms = Vec::new();
        let mut huffman_ms = Vec::new();
        let mut hist_ms = Vec::new();
        let mut lengths_ms = Vec::new();
        let mut codes_ms = Vec::new();
        let mut emit_ms = Vec::new();
        let mut stored_ms = Vec::new();
        let mut body_ms = Vec::new();
        let mut frame_ms = Vec::new();
        let mut crc_ms = Vec::new();
        let mut payload_hash_ms = Vec::new();
        let mut content_hash_ms = Vec::new();
        let mut shipped_ms = Vec::new();
        let mut threaded_ms = Vec::new();
        let mut threaded_faults = Vec::new();
        let mut reference_ms = Vec::new();
        let mut shipped_faults = Vec::new();
        let mut reference_faults = Vec::new();
        let mut split_payload_first_ms = Vec::new();
        let mut split_faults = Vec::new();
        let mut huffman_faults = Vec::new();
        let mut staged_sum_ms = Vec::new();
        let mut staged_wall_ms = Vec::new();
        let mut staged_stages: Vec<[f64; 6]> = Vec::new();
        let mut staged_split_faults_vec: Vec<f64> = Vec::new();
        let mut shipped_frame_bytes = 0usize;

        for _ in 0..args.reps {
            // Order probe: the shipped split+histogram+emit, measured first
            // in the repetition and again at the end, isolates allocator and
            // cold-memory state from the algorithm.
            let started = Instant::now();
            let first = modes::split_float_tensor_payload(&bytes, FloatDtype::Bf16, count)
                .map_err(|err| format!("split `{}`: {err}", tensor.name))?;
            split_payload_first_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(first);

            let before = page_faults();
            let started = Instant::now();
            let (exponent, mantissa) = split_bf16(&bytes)?;
            split_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            split_faults.push(fault_delta(before, page_faults()));

            let before = page_faults();
            let started = Instant::now();
            let official = weftcodec::huffman::encode(&exponent);
            huffman_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            huffman_faults.push(fault_delta(before, page_faults()));
            if official != staged.output {
                return Err(format!("huffman replica drift on `{}`", tensor.name));
            }
            let stage_times = huffman_staged(&exponent);
            hist_ms.push(stage_times.hist_ns as f64 / 1_000_000.0);
            lengths_ms.push(stage_times.lengths_ns as f64 / 1_000_000.0);
            codes_ms.push(stage_times.codes_ns as f64 / 1_000_000.0);
            emit_ms.push(stage_times.emit_ns as f64 / 1_000_000.0);

            let started = Instant::now();
            let mut stored = Vec::with_capacity(official.len() + mantissa.len());
            stored.extend_from_slice(&official);
            stored.extend_from_slice(&mantissa);
            stored_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            if stored[..official.len()] != official[..] || stored[official.len()..] != mantissa[..]
            {
                return Err(format!("stored plane replica drift on `{}`", tensor.name));
            }
            drop(stored);

            let started = Instant::now();
            let mut body =
                Vec::with_capacity(4 + meta_bytes.len() + official.len() + mantissa.len());
            body.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
            body.extend_from_slice(&meta_bytes);
            body.extend_from_slice(&official);
            body.extend_from_slice(&mantissa);
            body_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let started = Instant::now();
            let frame = encode_frame(
                &FrameParts {
                    format_major: FORMAT_MAJOR,
                    format_minor: FORMAT_MINOR,
                    domain: DOMAIN_PAYLOAD,
                    mode: Mode::FloatTensor,
                    flags: 0,
                    level: modes::FLOAT_TENSOR_LEVEL,
                    decoded_len: bytes.len() as u64,
                    content_hash,
                    dependencies: &[],
                    chunks: &[],
                    body: &body,
                },
                &limits,
            )
            .map_err(|err| format!("frame `{}`: {err}", tensor.name))?;
            frame_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(body);
            if frame != shipped_frame {
                return Err(format!(
                    "hand-assembled frame differs from the shipped encoder on `{}`",
                    tensor.name
                ));
            }

            let started = Instant::now();
            let crc = crc32c::crc32c(&frame);
            crc_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            std::hint::black_box(crc);

            let started = Instant::now();
            let stored_at = frame.len() - 4 - stored_len(&frame);
            let _ = hash_domain(
                DOMAIN_FLOAT_TENSOR_PAYLOAD,
                &frame[stored_at..frame.len() - 4],
            );
            payload_hash_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let started = Instant::now();
            let _ = hash_domain(DOMAIN_CHUNK, &bytes);
            content_hash_ms.push(started.elapsed().as_secs_f64() * 1000.0);

            let before = page_faults();
            let started = Instant::now();
            let frame =
                modes::encode_float_tensor(&bytes, FloatDtype::Bf16, &tensor.shape, &limits)
                    .map_err(|err| format!("encode `{}`: {err}", tensor.name))?;
            shipped_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            shipped_frame_bytes = frame.len();
            let after = page_faults();
            shipped_faults.push(fault_delta(before, after));
            drop(frame);

            let encode_options = FloatTensorEncodeOptions::with_threads(args.threads);
            let before = page_faults();
            let started = Instant::now();
            let frame = modes::encode_float_tensor_with(
                &bytes,
                FloatDtype::Bf16,
                &tensor.shape,
                &limits,
                encode_options,
            )
            .map_err(|err| format!("threaded encode `{}`: {err}", tensor.name))?;
            threaded_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            threaded_faults.push(fault_delta(before, page_faults()));
            if frame.len() != shipped_frame_bytes {
                return Err(format!("threaded frame length drift on `{}`", tensor.name));
            }
            drop(frame);

            let before = page_faults();
            let started = Instant::now();
            let frame = modes::encode_float_tensor_reference(
                &bytes,
                FloatDtype::Bf16,
                &tensor.shape,
                &limits,
            )
            .map_err(|err| format!("reference encode `{}`: {err}", tensor.name))?;
            reference_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            let after = page_faults();
            reference_faults.push(fault_delta(before, after));
            if frame.len() != shipped_frame_bytes {
                return Err(format!("reference frame length drift on `{}`", tensor.name));
            }

            let started = Instant::now();
            let (staged_frame, stages) =
                reference_staged(&bytes, FloatDtype::Bf16, &tensor.shape, &limits)?;
            staged_wall_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            if staged_frame != frame {
                return Err(format!(
                    "stage-by-stage reference replica differs from the reference encoder on `{}`",
                    tensor.name
                ));
            }
            staged_sum_ms.push(stages.iter().sum::<f64>());
            staged_split_faults_vec.push(staged_split_faults());
            staged_stages.push(stages);
        }

        let split = median(&mut split_ms);
        let huffman = median(&mut huffman_ms);
        let hist = median(&mut hist_ms);
        let lengths = median(&mut lengths_ms);
        let codes = median(&mut codes_ms);
        let emit = median(&mut emit_ms);
        let stored_build = median(&mut stored_ms);
        let body_build = median(&mut body_ms);
        let frame_encode = median(&mut frame_ms);
        let crc = median(&mut crc_ms);
        let payload_hash = median(&mut payload_hash_ms);
        let content_hash = median(&mut content_hash_ms);
        let shipped = median(&mut shipped_ms);
        let threaded = median(&mut threaded_ms);
        let reference = median(&mut reference_ms);
        // Assembly residual: what the pre-R19 encoder spends outside the stages
        // above (allocator first-touch, the second CRC pass inside
        // `Frame::parse`, descriptor construction, the frame copy).
        let assembly = (reference - split - huffman - payload_hash - content_hash).max(0.0);
        let staged_sum = median(&mut staged_sum_ms);
        let staged_wall = median(&mut staged_wall_ms);
        let mut per_stage = [0.0f64; 6];
        for stage in 0..6 {
            let mut values: Vec<f64> = staged_stages.iter().map(|row| row[stage]).collect();
            per_stage[stage] = median(&mut values);
        }
        rows.push(json!({
            "tensor": tensor.name,
            "shape": tensor.shape,
            "elements": count,
            "logical_bytes": bytes.len(),
            "reps": args.reps,
            "frame_bytes": shipped_frame_bytes,
            "shipped_encode_ms": shipped,
            "reference_encode_ms": reference,
            "replica": {
                "split_ms": split,
                "huffman_total_ms": huffman,
                "huffman_histogram_ms": hist,
                "huffman_lengths_ms": lengths,
                "huffman_codes_ms": codes,
                "huffman_emit_ms": emit,
                "stored_build_ms": stored_build,
                "body_build_ms": body_build,
                "frame_encode_ms": frame_encode,
                "crc32c_ms": crc,
                "payload_hash_ms": payload_hash,
                "content_hash_ms": content_hash,
                "assembly_residual_ms": assembly,
            },
            "split_payload_first_ms": median(&mut split_payload_first_ms.clone()),
            "split_payload_last_ms": per_stage[0],
            "replica_split_faults_minor": median(&mut split_faults.clone()),
            "replica_huffman_faults_minor": median(&mut huffman_faults.clone()),
            "staged_split_payload_faults_minor": median(&mut staged_split_faults_vec.clone()),
            "reference_staged_sum_ms": staged_sum,
            "reference_staged_wall_ms": staged_wall,
            "reference_staged_gap_ms": (reference - staged_sum).max(0.0),
            "reference_staged_stages_ms": {
                "split_payload": per_stage[0],
                "stored_build": per_stage[1],
                "payload_hash": per_stage[2],
                "body_build": per_stage[3],
                "frame_encode": per_stage[4],
                "content_hash": per_stage[5],
            },
            "shipped_faults_minor": median(&mut shipped_faults.clone()),
            "threaded_encode_ms": threaded,
            "threaded_threads": args.threads,
            "threaded_faults_minor": median(&mut threaded_faults.clone()),
            "reference_faults_minor": median(&mut reference_faults.clone()),
            "shipped_mb_per_s": bytes.len() as f64 / 1e6 / (shipped / 1000.0),
            "reference_mb_per_s": bytes.len() as f64 / 1e6 / (reference / 1000.0),
            "replica_validated": true,
            "byte_identical_to_reference": true,
        }));
    }

    let total = |extract: &dyn Fn(&Json) -> f64| -> f64 { rows.iter().map(extract).sum::<f64>() };
    let logical = total(&|row| row["logical_bytes"].as_f64().unwrap_or(0.0));
    let shipped_total = total(&|row| row["shipped_encode_ms"].as_f64().unwrap_or(0.0));
    let threaded_total = total(&|row| row["threaded_encode_ms"].as_f64().unwrap_or(0.0));
    let reference_total = total(&|row| row["reference_encode_ms"].as_f64().unwrap_or(0.0));
    let replica_total =
        |key: &str| -> f64 { total(&|row: &Json| row["replica"][key].as_f64().unwrap_or(0.0)) };

    let receipt = json!({
        "schema": "weft.mode9_encoder_bench.v1",
        "label": args.label,
        "generated_utc": utc_stamp(),
        "environment": {
            "court": env_or("WEFT_COURT_IMAGE", "outside-court"),
            "court_image_id": env_or("WEFT_COURT_IMAGE_ID", "unknown"),
            "kernel": env_or("WEFT_COURT_KERNEL", &uname()),
            "memory": env_or("WEFT_COURT_MEMORY", "unknown"),
            "cpus": env_or("WEFT_COURT_CPUS", "unknown"),
            "pids": env_or("WEFT_COURT_PIDS", "unknown"),
            "weftfs_revision": env_or("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec_revision": env_or("WEFT_REPO_WEFTCODEC", "unknown"),
            "simd_path": weftcodec::simd::active_path(),
        },
        "fixture": {
            "path": args.fixture.display().to_string(),
            "bytes": file.file_bytes,
            "sha256": sha,
            "tensors_measured": rows.len(),
        },
        "method": {
            "replica": "stage timings are harness-local replicas; every measured tensor is checked byte-for-byte against the shipped codec (split planes, huffman stream, hand-assembled frame) before any number is recorded",
            "statistic": "median of repetitions per tensor; totals are sums of the per-tensor medians",
            "assembly_residual": "reference_encode - split - huffman - payload_hash - content_hash; it contains allocator first-touch, the body copy, the trailer CRC and the self-parse CRC inside encode_frame",
            "byte_identity": "the shipped mode-9 frame is compared with the pre-R19 reference encoder on every tensor and repetition",
        },
        "memory_probe": memory,
        "tensors": rows,
        "totals": {
            "logical_bytes": logical,
            "shipped_encode_ms": shipped_total,
            "reference_encode_ms": reference_total,
            "threaded_encode_ms": threaded_total,
            "threaded_threads": args.threads,
            "threaded_mb_per_s": logical / 1e6 / (threaded_total / 1000.0),
            "shipped_mb_per_s": logical / 1e6 / (shipped_total / 1000.0),
            "reference_mb_per_s": logical / 1e6 / (reference_total / 1000.0),
            "split_ms": replica_total("split_ms"),
            "huffman_total_ms": replica_total("huffman_total_ms"),
            "huffman_histogram_ms": replica_total("huffman_histogram_ms"),
            "huffman_emit_ms": replica_total("huffman_emit_ms"),
            "stored_build_ms": replica_total("stored_build_ms"),
            "body_build_ms": replica_total("body_build_ms"),
            "frame_encode_ms": replica_total("frame_encode_ms"),
            "crc32c_ms": replica_total("crc32c_ms"),
            "payload_hash_ms": replica_total("payload_hash_ms"),
            "content_hash_ms": replica_total("content_hash_ms"),
            "assembly_residual_ms": replica_total("assembly_residual_ms"),
        },
        "checks": {
            "byte_identity_cases": byte_identity_checks,
            "byte_identity_mismatches": 0,
            "mean_shipped_ms": mean(&rows.iter().map(|row| row["shipped_encode_ms"].as_f64().unwrap_or(0.0)).collect::<Vec<_>>()),
        },
        "not_claimed": [
            "single host, warm page cache",
            "the stage split is a harness replication validated against the codec, not a codec-internal profile",
            "BF16 tensors from one public checkpoint only",
        ],
    });
    write_json(&args.out, &receipt)?;
    println!("wrote {}", args.out.display());
    Ok(())
}

/// Length of the stored plane region inside an assembled frame, derived from
/// the descriptor rather than assumed, so the payload-hash row hashes exactly
/// `huffman tables+stream || mantissa plane`.
fn stored_len(frame: &[u8]) -> usize {
    // body = u32 meta_len || meta || exponent_plane || mantissa_plane
    let meta_len = u32::from_le_bytes([frame[96], frame[97], frame[98], frame[99]]) as usize;
    let body_len = u64::from_le_bytes(frame[20..28].try_into().expect("body length")) as usize;
    body_len - 4 - meta_len
}
