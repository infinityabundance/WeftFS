//! Streaming mode-9 encoder court (subphase X04).
//!
//! One process per measurement, so peak RSS is honest:
//!
//! * `oneshot`  — read the whole tensor into memory and call
//!                `encode_float_tensor` (the historical encoder).
//! * `stream`   — encode the same tensor through
//!                `encode_float_tensor_stream` over a bounded file source:
//!                the tensor is never resident.
//! * `analysis` — only the streamed analysis pass (histogram + content hash +
//!                exact frame length), to separate the counting pass from
//!                emission.
//!
//! Every mode prints the frame length and its SHA-256; the caller compares them
//! and requires equality. The report carries wall time and peak RSS, so the
//! memory saving and any slowdown are measured, not asserted.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use weftcodec::limits::Limits;
use weftcodec::modes::{
    analyse_float_tensor_stream, encode_float_tensor, encode_float_tensor_stream, FloatDtype,
    FloatTensorStreamOptions,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("mode9_stream_bench: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = value_of(&args, "--phase")
        .or_else(|| args.first().cloned())
        .unwrap_or_default();
    let source = PathBuf::from(required(&args, "--source")?);
    let tensor_name = required(&args, "--tensor")?;
    let out = PathBuf::from(required(&args, "--out")?);
    let window = value_of(&args, "--window-bytes")
        .map(|text| text.parse::<usize>())
        .transpose()
        .map_err(|_| "--window-bytes must be an integer".to_owned())?
        .unwrap_or(1 << 20);

    let file =
        weftfs::safetensors::SafetensorsFile::parse(&source).map_err(|err| err.to_string())?;
    let tensor = file
        .tensors
        .iter()
        .find(|candidate| candidate.name == tensor_name)
        .ok_or_else(|| format!("no tensor `{tensor_name}` in {}", source.display()))?
        .clone();
    let dtype = float_dtype(&tensor.dtype)
        .ok_or_else(|| format!("dtype `{}` has no mode-9 layout", tensor.dtype))?;
    let limits = Limits::default();
    let options = FloatTensorStreamOptions {
        window_bytes: window,
        ..FloatTensorStreamOptions::default()
    };

    let started = Instant::now();
    let (frame_bytes, frame_digest, analysis_only) = match phase.as_str() {
        "oneshot" => {
            let payload = read_tensor(&file, &tensor)?;
            let frame = encode_float_tensor(&payload, dtype, &tensor.shape, &limits)
                .map_err(|err| err.to_string())?;
            let len = frame.len();
            let digest = hex::encode(Sha256::digest(&frame));
            std::mem::drop(frame);
            std::mem::drop(payload);
            (len, digest, None)
        }
        "stream" => {
            let mut bytes_source = file
                .open_tensor_source(&tensor)
                .map_err(|err| err.to_string())?;
            let frame = encode_float_tensor_stream(
                &mut bytes_source,
                dtype,
                &tensor.shape,
                &limits,
                &options,
                None,
            )
            .map_err(|err| err.to_string())?;
            let len = frame.len();
            let digest = hex::encode(Sha256::digest(&frame));
            std::mem::drop(frame);
            (len, digest, None)
        }
        "analysis" => {
            let mut bytes_source = file
                .open_tensor_source(&tensor)
                .map_err(|err| err.to_string())?;
            let analysis = analyse_float_tensor_stream(
                &mut bytes_source,
                dtype,
                &tensor.shape,
                &limits,
                &options,
            )
            .map_err(|err| err.to_string())?;
            (
                analysis.frame_bytes as usize,
                hex::encode(analysis.content_hash.as_bytes()),
                Some(analysis),
            )
        }
        other => {
            return Err(format!(
                "unknown phase `{other}` (expected oneshot, stream, analysis)"
            ))
        }
    };
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    let report = json!({
        "phase": phase,
        "status": "ok",
        "source": source.display().to_string(),
        "tensor": tensor.name,
        "dtype": tensor.dtype,
        "shape": tensor.shape,
        "logical_bytes": tensor.byte_len(),
        "window_bytes": window,
        "frame_bytes": frame_bytes,
        "frame_sha256": frame_digest,
        "frame_ratio": if tensor.byte_len() > 0 {
            (frame_bytes as f64) / (tensor.byte_len() as f64)
        } else {
            0.0
        },
        "exponent_plane_len": analysis_only.as_ref().map(|analysis| analysis.exponent_plane_len),
        "mantissa_plane_len": analysis_only.as_ref().map(|analysis| analysis.mantissa_plane_len),
        "wall_ms": (wall_ms * 1000.0).round() / 1000.0,
        "mb_per_s": if wall_ms > 0.0 {
            ((tensor.byte_len() as f64 / 1_000_000.0) / (wall_ms / 1000.0) * 1000.0).round() / 1000.0
        } else {
            0.0
        },
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &report)?;
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    value_of(args, flag).ok_or_else(|| format!("missing {flag}"))
}

fn float_dtype(name: &str) -> Option<FloatDtype> {
    match name {
        "bf16" | "BF16" => Some(FloatDtype::Bf16),
        "f16" | "F16" | "fp16" => Some(FloatDtype::F16),
        "f32" | "F32" | "fp32" => Some(FloatDtype::F32),
        _ => None,
    }
}

fn read_tensor(
    file: &weftfs::safetensors::SafetensorsFile,
    tensor: &weftfs::safetensors::SafetensorsTensor,
) -> Result<Vec<u8>, String> {
    use std::io::{Seek, SeekFrom};
    let mut handle = File::open(&file.path).map_err(|err| err.to_string())?;
    handle
        .seek(SeekFrom::Start(file.data_start + tensor.begin))
        .map_err(|err| err.to_string())?;
    let mut bytes = vec![0u8; tensor.byte_len() as usize];
    handle
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

fn peak_rss_kib() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kib) = rest.split_whitespace().next() {
                return kib.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}
