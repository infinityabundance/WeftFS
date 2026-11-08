//! Mode-10 vs mode-15 checkpoint region-read benchmark (subphase Q2).
//!
//! The workload is one real 2-D BF16 tensor from a safetensors container. Two
//! single-shard checkpoints are written: the default mode-10 `TENSOR_SHARDED`
//! byte transport and a mode-15 `TENSOR_TILED` checkpoint. For a 100-row and a
//! 1-row region the harness times `Store::restore_checkpoint_bytes` (decode
//! included) over repeated runs and records the backend byte counters and
//! touched units.
//!
//! This is an in-process measurement of the shipped store API. It is not a
//! claim about other hardware or a remote object store.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::json;
use weftcodec::Region;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{Options, Store};

fn parse_u64_list(text: &str) -> Vec<u64> {
    text.split(',')
        .map(|part| part.trim().parse::<u64>().expect("u64 list"))
        .collect()
}

fn p50(values: &[u128]) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

struct Run {
    strategy: &'static str,
    region: &'static str,
    region_extent: Vec<u64>,
    samples_ns: Vec<u128>,
    p50_ns: u128,
    bytes: u64,
    store_bytes_read: u64,
    store_reads: u64,
    units_touched: u64,
    shards_read: usize,
}

fn measure(
    store: &Store,
    checkpoint: &str,
    tensor: &str,
    label: &'static str,
    name: &'static str,
    region: Region,
    reps: usize,
) -> Run {
    let options = CheckpointRestoreOptions {
        tensor: Some(tensor.to_owned()),
        region: Some(region.clone()),
        topology: None,
    };
    // Warm-up so the reported samples are not first-touch page faults.
    let _ = store
        .restore_checkpoint_bytes(checkpoint, &options)
        .expect("warm-up restore");
    let mut samples = Vec::with_capacity(reps);
    let mut last = None;
    for _ in 0..reps {
        let started = Instant::now();
        let (bytes, report) = store
            .restore_checkpoint_bytes(checkpoint, &options)
            .expect("restore");
        samples.push(started.elapsed().as_nanos());
        last = Some((bytes.len() as u64, report));
    }
    let (bytes, report) = last.expect("at least one rep");
    Run {
        strategy: label,
        region: name,
        region_extent: region.extent(),
        p50_ns: p50(&samples),
        samples_ns: samples,
        bytes,
        store_bytes_read: report.store_bytes_read,
        store_reads: report.store_reads,
        units_touched: report.units_touched,
        shards_read: report.shards_read,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut source = PathBuf::from(".docker-cache/real-corpus/Qwen2.5-0.5B.safetensors");
    let mut tensor = "model.layers.0.mlp.down_proj.weight".to_owned();
    let mut tile_shape = vec![32u64, 256];
    let mut work = PathBuf::from(".docker-cache/tensor-tiles");
    let mut out = PathBuf::from("bench/receipts/tensor-tiles.json");
    let mut reps = 10usize;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                source = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--tensor" => {
                tensor = args[index + 1].clone();
                index += 2;
            }
            "--tile-shape" => {
                tile_shape = parse_u64_list(&args[index + 1]);
                index += 2;
            }
            "--work" => {
                work = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--reps" => {
                reps = args[index + 1].parse().expect("reps");
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }

    let file = SafetensorsFile::parse(&source).expect("parse source");
    let entry = file.tensor(&tensor).expect("tensor present").clone();
    for (axis, tile) in tile_shape.iter().enumerate() {
        assert!(
            entry.shape[axis] % *tile == 0,
            "tile shape {tile} does not divide tensor extent {}",
            entry.shape[axis]
        );
    }
    let rows = entry.shape[0];
    let cols = entry.shape[1];

    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("work dir");
    let m10_dir = work.join("m10");
    let tiled_dir = work.join("tiled");

    let mut m10 = Store::init(&m10_dir, options()).expect("init m10 store");
    let m10_report = m10
        .write_checkpoint(
            "tiles",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: vec![tensor.clone()],
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .expect("write m10");

    let mut tiled = Store::init(&tiled_dir, options()).expect("init tiled store");
    let tiled_report = tiled
        .write_checkpoint(
            "tiles",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: vec![tensor.clone()],
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(tile_shape.clone()),
                ..CheckpointWriteOptions::default()
            },
        )
        .expect("write tiled");

    let regions = [
        ("100_rows", Region::new(vec![0, 0], vec![100, cols])),
        ("1_row", Region::new(vec![0, 0], vec![1, cols])),
    ];
    let mut runs = Vec::new();
    for (name, region) in regions.iter() {
        runs.push(measure(
            &m10,
            "tiles",
            &tensor,
            "mode-10",
            name,
            region.clone(),
            reps,
        ));
        runs.push(measure(
            &tiled,
            "tiles",
            &tensor,
            "mode-15",
            name,
            region.clone(),
            reps,
        ));
    }

    // Exactness: the 100-row slices from both strategies must be identical.
    let options_100 = CheckpointRestoreOptions {
        tensor: Some(tensor.clone()),
        region: Some(Region::new(vec![0, 0], vec![100, cols])),
        topology: None,
    };
    let (m10_bytes, _) = m10
        .restore_checkpoint_bytes("tiles", &options_100)
        .expect("m10 slice");
    let (tiled_bytes, _) = tiled
        .restore_checkpoint_bytes("tiles", &options_100)
        .expect("tiled slice");
    assert_eq!(m10_bytes, tiled_bytes, "mode 10/15 100-row slices differ");

    let bytes_per_element = 2u64;
    let mut rows_json = Vec::new();
    for run in &runs {
        rows_json.push(json!({
            "strategy": run.strategy,
            "region": run.region,
            "region_extent": run.region_extent,
            "samples_ns": run.samples_ns,
            "p50_ns": run.p50_ns,
            "p50_ms": run.p50_ns as f64 / 1_000_000.0,
            "output_bytes": run.bytes,
            "store_bytes_read": run.store_bytes_read,
            "store_reads": run.store_reads,
            "units_touched": run.units_touched,
            "shards_read": run.shards_read,
        }));
    }
    let value = json!({
        "schema": "weft.bench.tensor-tiles.v1",
        "label": "single-host Docker court, one real public checkpoint tensor, in-process store API; not a fleet or remote-store claim",
        "source": source.display().to_string(),
        "tensor": tensor,
        "shape": entry.shape,
        "dtype": entry.dtype,
        "tile_shape": tile_shape,
        "reps": reps,
        "p50_definition": "lower median of the sorted per-rep durations (index len/2)",
        "rows": runs.len(),
        "reps_per_row": reps,
        "logical_tensor_bytes": rows * cols * bytes_per_element,
        "checkpoints": {
            "mode10": {
                "shard_objects": m10_report.shard_objects,
                "element_bytes": m10_report.total_element_bytes,
                "shard_strategy": "tensor-sharded",
            },
            "mode15": {
                "shard_objects": tiled_report.shard_objects,
                "element_bytes": tiled_report.total_element_bytes,
                "shard_strategy": "tensor-tiled",
                "tile_shape": tile_shape,
            },
        },
        "exact_slices_equal": m10_bytes == tiled_bytes,
        "runs": rows_json,
    });
    fs::write(&out, serde_json::to_string_pretty(&value).unwrap() + "\n").expect("write receipt");
    println!("tensor-tiles bench: wrote {}", out.display());
    for run in &runs {
        println!(
            "{:<8} {:<9} p50={:>8.3} ms bytes_read={:>10} units={:>4}",
            run.strategy,
            run.region,
            run.p50_ns as f64 / 1_000_000.0,
            run.store_bytes_read,
            run.units_touched
        );
    }
}
