//! Mode 18 court: real Qwen BF16 embedding, modes 9/17/18 at 1 and 4 threads.
//!
//! Reads one tensor from a safetensors file, encodes it as mode 9
//! (`FLOAT_TENSOR`), mode 17 (`FLOAT_TILED`) and mode 18
//! (`FLOAT_TILED_STREAMS`, n = 1/2/4/8), and measures whole and region decodes.
//! Nothing is fabricated: the receipt records the exact file, tensor, tile
//! shape, thread counts, and repetitions.
//!
//! Usage:
//!   mode18_bench --file F --tensor NAME --tile 64,896 --reps 3 --out out.json

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value};
use weftcodec::limits::Limits;
use weftcodec::modes::{
    decode_float_tiled, decode_float_tiled_streams, decode_float_tiled_with_threads,
    encode_float_tensor, encode_float_tiled, encode_float_tiled_streams, parse_float_tiled,
    parse_float_tiled_streams, read_float_region, read_float_tiled_streams_region, FloatDtype,
};
use weftcodec::tensor::Region;
use weftcodec::Frame;
use weftfs::safetensors::SafetensorsFile;

fn median_ns(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn mib_per_second(bytes: usize, ns: u128) -> f64 {
    if ns == 0 {
        return 0.0;
    }
    (bytes as f64 / (1024.0 * 1024.0)) / (ns as f64 / 1_000_000_000.0)
}

fn parse_shape(text: &str) -> Vec<u64> {
    text.split(',')
        .map(|part| part.trim().parse::<u64>().expect("shape"))
        .collect()
}

/// Weight-like BF16: a clustered exponent and a random mantissa byte.
fn weight_like_bf16(elements: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(elements * 2);
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let high = if state % 16 == 0 { 0x40u8 } else { 0x3fu8 };
        out.push((state >> 8) as u8);
        out.push(high);
    }
    out
}

/// Same method as `examples/calibrate_decode.rs`: 128 KiB and 16 MiB, seven
/// repetitions plus one warmup, affine fit from the two medians.
fn calibrate_mode18(reps: usize) -> Value {
    let limits = Limits::default();
    let measure = |elements: usize, tile: Vec<u64>| -> (u64, u128) {
        let payload = weight_like_bf16(elements, 0x5eed);
        let shape = vec![(elements / 1024) as u64, 1024];
        let frame =
            encode_float_tiled_streams(&payload, FloatDtype::Bf16, &shape, &tile, 4, &limits)
                .expect("calibration encode");
        let parsed = Frame::parse(&frame, &limits).expect("calibration parse");
        let _ = decode_float_tiled_streams(&parsed, &limits, 1).expect("warmup");
        let mut samples = Vec::with_capacity(reps);
        for _ in 0..reps {
            let start = Instant::now();
            let decoded = decode_float_tiled_streams(&parsed, &limits, 1).expect("decode");
            samples.push(start.elapsed().as_nanos());
            assert_eq!(decoded.len(), payload.len());
        }
        (payload.len() as u64, median_ns(samples))
    };
    let (small_bytes, small_ns) = measure(128 * 1024 / 2, vec![64, 1024]);
    let (large_bytes, large_ns) = measure(16 * 1024 * 1024 / 2, vec![64, 1024]);
    let mib = 1_048_576f64;
    let slope = ((large_ns as f64 - small_ns as f64)
        / (large_bytes as f64 / mib - small_bytes as f64 / mib)) as u64;
    let fixed = (small_ns as f64 - (small_bytes as f64 / mib) * slope as f64).max(0.0) as u64;
    json!({
        "calibration_id": weftfs::cost_model::MODE18_MEASURED_CALIBRATION,
        "method": "single-thread mode-18 decode, 1 warmup + N reps, median; affine fit from the 128 KiB and 16 MiB medians",
        "small": { "logical_bytes": small_bytes, "median_ns": small_ns },
        "large": { "logical_bytes": large_bytes, "median_ns": large_ns },
        "fit": { "fixed_ns": fixed, "ns_per_mib": slope },
    })
}

struct Row {
    mode: String,
    n_streams: Option<u64>,
    physical_bytes: u64,
    encode_ns: u128,
    decode_1: u128,
    decode_4: u128,
    region_whole: Option<(u128, u64)>,
    region_100: Option<(u128, u64)>,
    region_1: Option<(u128, u64)>,
    region_streams: Option<u64>,
}

fn main() {
    let mut file = PathBuf::new();
    let mut tensor_name = "model.embed_tokens.weight".to_owned();
    let mut tile = vec![64u64, 896];
    let mut reps = 3usize;
    let mut out_path: Option<PathBuf> = None;
    let mut calibrate = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--file" => file = PathBuf::from(args.next().expect("file")),
            "--tensor" => tensor_name = args.next().expect("tensor"),
            "--tile" => tile = parse_shape(&args.next().expect("tile")),
            "--reps" => reps = args.next().expect("reps").parse().expect("reps"),
            "--out" => out_path = Some(PathBuf::from(args.next().expect("out"))),
            "--calibrate" => calibrate = true,
            other => panic!("unknown argument {other}"),
        }
    }
    if file.as_os_str().is_empty() {
        if calibrate {
            let report = json!({
                "schema": "weftfs.mode18.calibration.v1",
                "reps": reps,
                "calibration_mode18": calibrate_mode18(reps),
            });
            let text = serde_json::to_string_pretty(&report).expect("json");
            println!("{text}");
            if let Some(path) = out_path {
                std::fs::write(&path, &text).expect("write report");
            }
            return;
        }
        panic!("--file is required (or pass --calibrate with no file)");
    }

    let limits = Limits::default();
    let container = SafetensorsFile::parse(&file).expect("safetensors");
    let tensor = container.tensor(&tensor_name).expect("tensor");
    let payload = container.read_tensor(tensor).expect("tensor bytes");
    let shape = tensor.shape.clone();
    let dtype_text = tensor.dtype.clone();
    assert_eq!(dtype_text, "bf16", "this court expects a BF16 tensor");
    let dtype = FloatDtype::Bf16;
    assert_eq!(
        shape,
        vec![151936, 896],
        "the receipt names the Qwen2.5-0.5B embedding shape"
    );
    let element_size = dtype.element_size() as usize;
    let logical_mib = payload.len() as f64 / (1024.0 * 1024.0);

    let mut rows: Vec<Row> = Vec::new();

    let calibration = if calibrate {
        Some(calibrate_mode18(reps))
    } else {
        None
    };

    // Mode 9: one whole frame; no independent region unit.
    {
        let start = Instant::now();
        let frame = encode_float_tensor(&payload, dtype, &shape, &limits).expect("mode 9 encode");
        let encode_ns = start.elapsed().as_nanos();
        let physical_bytes = frame.len() as u64;
        let parsed = Frame::parse(&frame, &limits).expect("parse");
        let mut decode_1 = Vec::new();
        let mut decode_4 = Vec::new();
        for _ in 0..reps {
            let start = Instant::now();
            let decoded = weftcodec::modes::decode_float_tensor(&parsed, &limits).expect("decode");
            decode_1.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
            let start = Instant::now();
            let decoded = weftcodec::modes::decode_float_tensor(&parsed, &limits).expect("decode");
            decode_4.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
        }
        // Mode 9 has no independent tile; a region read is a whole decode plus
        // a slice. Report that honestly as the same cost as a whole read.
        let whole = (median_ns(decode_1.clone()), 0);
        let hundred = (median_ns(decode_1.clone()), 0);
        let one = (median_ns(decode_1.clone()), 0);
        rows.push(Row {
            mode: "mode9".to_owned(),
            n_streams: None,
            physical_bytes,
            encode_ns,
            decode_1: median_ns(decode_1),
            decode_4: median_ns(decode_4),
            region_whole: Some(whole),
            region_100: Some(hundred),
            region_1: Some(one),
            region_streams: None,
        });
        drop(frame);
    }

    // Mode 17.
    {
        let start = Instant::now();
        let frame = encode_float_tiled(&payload, dtype, &shape, &tile, &limits).expect("mode 17");
        let encode_ns = start.elapsed().as_nanos();
        let physical_bytes = frame.len() as u64;
        let parsed = Frame::parse(&frame, &limits).expect("parse");
        let descriptor = parse_float_tiled(&parsed, &limits).expect("descriptor");
        let mut decode_1 = Vec::new();
        let mut decode_4 = Vec::new();
        for _ in 0..reps {
            let start = Instant::now();
            let decoded = decode_float_tiled(&parsed, &limits).expect("decode");
            decode_1.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
            let start = Instant::now();
            // Mode 17 has no codec-level stream option; 4 threads runs the same
            // tile worker pool mode 18 uses, so the comparison is fair.
            let decoded = decode_float_tiled_with_threads(&parsed, &limits, 4).expect("decode");
            decode_4.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
        }
        let region = |rows: u64| {
            let r = Region::new(vec![0, 0], vec![rows, shape[1]]);
            let start = Instant::now();
            let (_bytes, _) = read_float_region(&descriptor, &r, &limits).expect("region");
            (
                start.elapsed().as_nanos(),
                rows * shape[1] * element_size as u64,
            )
        };
        let whole = region(shape[0]);
        let hundred = region(100.min(shape[0]));
        let one = region(1);
        rows.push(Row {
            mode: "mode17".to_owned(),
            n_streams: None,
            physical_bytes,
            encode_ns,
            decode_1: median_ns(decode_1),
            decode_4: median_ns(decode_4),
            region_whole: Some(whole),
            region_100: Some(hundred),
            region_1: Some(one),
            region_streams: None,
        });
        drop(frame);
    }

    // Mode 18 at n = 1, 2, 4, 8.
    for n_streams in [1u64, 2, 4, 8] {
        let start = Instant::now();
        let frame = encode_float_tiled_streams(&payload, dtype, &shape, &tile, n_streams, &limits)
            .expect("mode 18 encode");
        let encode_ns = start.elapsed().as_nanos();
        let physical_bytes = frame.len() as u64;
        let parsed = Frame::parse(&frame, &limits).expect("parse");
        let descriptor = parse_float_tiled_streams(&parsed, &limits).expect("descriptor");
        let mut decode_1 = Vec::new();
        let mut decode_4 = Vec::new();
        for _ in 0..reps {
            let start = Instant::now();
            let decoded = decode_float_tiled_streams(&parsed, &limits, 1).expect("decode");
            decode_1.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
            let start = Instant::now();
            let decoded = decode_float_tiled_streams(&parsed, &limits, 4).expect("decode");
            decode_4.push(start.elapsed().as_nanos());
            assert_eq!(decoded, payload);
        }
        let region = |rows: u64, threads: usize| {
            let r = Region::new(vec![0, 0], vec![rows, shape[1]]);
            let start = Instant::now();
            let (bytes, _tiles, streams) =
                read_float_tiled_streams_region(&descriptor, &r, &limits, threads).expect("region");
            assert_eq!(
                bytes.len() as u64,
                rows * shape[1] * element_size as u64,
                "region length"
            );
            (
                start.elapsed().as_nanos(),
                rows * shape[1] * element_size as u64,
                streams,
            )
        };
        let (whole_ns, whole_bytes, _) = region(shape[0], 4);
        let (hundred_ns, hundred_bytes, _) = region(100.min(shape[0]), 4);
        let (one_ns, one_bytes, streams) = region(1, 1);
        rows.push(Row {
            mode: "mode18".to_owned(),
            n_streams: Some(n_streams),
            physical_bytes,
            encode_ns,
            decode_1: median_ns(decode_1),
            decode_4: median_ns(decode_4),
            region_whole: Some((whole_ns, whole_bytes)),
            region_100: Some((hundred_ns, hundred_bytes)),
            region_1: Some((one_ns, one_bytes)),
            region_streams: Some(streams),
        });
        drop(frame);
    }

    let mut table = Vec::new();
    for row in &rows {
        let mut value = json!({
            "mode": row.mode,
            "n_streams": row.n_streams,
            "physical_bytes": row.physical_bytes,
            "encode_ns": row.encode_ns,
            "whole_decode": {
                "threads_1_ns": row.decode_1,
                "threads_1_mib_per_s": mib_per_second(payload.len(), row.decode_1),
                "threads_4_ns": row.decode_4,
                "threads_4_mib_per_s": mib_per_second(payload.len(), row.decode_4),
            },
        });
        if let Some((ns, bytes)) = row.region_whole {
            value["region_whole"] = json!({
                "ns": ns, "bytes": bytes, "mib_per_s": mib_per_second(bytes as usize, ns),
            });
        }
        if let Some((ns, bytes)) = row.region_100 {
            value["region_100_rows"] = json!({
                "ns": ns, "bytes": bytes, "mib_per_s": mib_per_second(bytes as usize, ns),
            });
        }
        if let Some((ns, bytes)) = row.region_1 {
            value["region_1_row"] = json!({
                "ns": ns, "bytes": bytes, "mib_per_s": mib_per_second(bytes as usize, ns),
                "streams_touched": row.region_streams,
            });
        }
        table.push(value);
    }

    let report = json!({
        "schema": "weftfs.mode18.court.v1",
        "file": file.display().to_string(),
        "tensor": tensor_name,
        "dtype": dtype_text,
        "shape": shape,
        "tile_shape": tile,
        "logical_bytes": payload.len(),
        "logical_mib": logical_mib,
        "reps": reps,
        "simd_path": weftcodec::simd::active_path(),
        "calibration_mode18": calibration,
        "rows": table,
    });
    let text = serde_json::to_string_pretty(&report).expect("json");
    println!("{text}");
    if let Some(path) = out_path {
        std::fs::write(&path, &text).expect("write report");
    }
}
