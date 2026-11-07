//! Real-weight decode-shape court: mode 9 vs mode 17 on one tensor.
//!
//!   decode_regions_bench --store DIR --safetensors FILE --tensor NAME \
//!       --tile-shape A,B --reps N --out FILE
//!
//! Writes the named tensor twice into a fresh store (mode 9 and mode 17),
//! then measures whole-tensor, 100-row and 1-row reads at 1 and 4 worker
//! threads. Every read is checked byte-identical to the source tensor. The
//! physical byte counts are the frame blob sizes on disk.
//!
//! Single host, warm cache, one tensor. This is not a model-wide claim.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use weftfs::format::RepresentationKind;
use weftfs::recovery::{manifest_path, read_manifest};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use weftfs::TileDecodeOptions;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

fn frame_bytes(store: &Store, id: weftfs::identity::Hash32) -> u64 {
    let record = store.object_meta(id).expect("object meta");
    let manifest = read_manifest(store.root(), record.content).expect("manifest");
    let frame = manifest
        .representation
        .as_ref()
        .and_then(|rep| rep.frame)
        .expect("frame id");
    std::fs::metadata(manifest_path(store.root(), frame))
        .expect("frame metadata")
        .len()
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store_dir = arg(&args, "--store").expect("--store");
    let source_path = arg(&args, "--safetensors").expect("--safetensors");
    let tensor_name = arg(&args, "--tensor").expect("--tensor");
    let tile_text = arg(&args, "--tile-shape").expect("--tile-shape");
    let reps: usize = arg(&args, "--reps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let out_path = arg(&args, "--out");

    std::fs::create_dir_all(&store_dir).expect("store dir");
    let mut store = Store::init(Path::new(&store_dir), options()).expect("store init");
    let root = store.resolve_path("/").expect("root");

    let file =
        weftfs::safetensors::SafetensorsFile::parse(Path::new(&source_path)).expect("safetensors");
    let tensor = file
        .tensors
        .iter()
        .find(|tensor| tensor.name == tensor_name)
        .expect("named tensor");
    let dtype = tensor.dtype.clone();
    let shape = tensor.shape.clone();
    let bytes = file.read_tensor(tensor).expect("tensor bytes");
    let tile_shape: Vec<u64> = tile_text
        .split(',')
        .map(|part| part.trim().parse().expect("tile extent"))
        .collect();

    let mut ids = Vec::new();
    for (name, strategy, tile) in [
        (b"mode9".as_slice(), TensorStrategy::FloatTensor, None),
        (
            b"mode17".as_slice(),
            TensorStrategy::FloatTiled,
            Some(tile_shape.clone()),
        ),
    ] {
        let spec = TensorWriteSpec {
            strategy_hint: Some(strategy),
            tile_shape: tile,
            ..TensorWriteSpec::default()
        };
        let start = Instant::now();
        let id = store
            .write_tensor_file_with_spec(
                root,
                name,
                &dtype,
                &shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                &spec,
            )
            .expect("write");
        let write_ms = start.elapsed().as_secs_f64() * 1e3;
        let record = store.object_meta(id).expect("record");
        let manifest = read_manifest(store.root(), record.content).expect("manifest");
        let rep = manifest.representation.as_ref().expect("representation");
        assert_eq!(rep.kind, RepresentationKind::Tensor);
        println!(
            "{}",
            json!({
                "schema": "weft.decode-regions-bench.v1",
                "event": "write",
                "strategy": strategy.as_str(),
                "tensor": tensor_name,
                "dtype": dtype,
                "shape": shape,
                "decoded_bytes": bytes.len(),
                "physical_bytes": frame_bytes(&store, id),
                "tile_shape": if strategy == TensorStrategy::FloatTiled { Some(&tile_shape) } else { None::<&Vec<u64>> },
                "write_ms": write_ms,
            })
        );
        ids.push((strategy, id));
    }

    let mut comparisons = Vec::new();
    for (strategy, id) in &ids {
        for threads in [1usize, 4] {
            let read_options = TileDecodeOptions {
                threads,
                ..TileDecodeOptions::default()
            };
            // Whole tensor.
            let mut whole_ms = Vec::new();
            for _ in 0..reps {
                let start = Instant::now();
                let decoded = store
                    .read_tensor_with_options(*id, &read_options)
                    .expect("whole read");
                whole_ms.push(start.elapsed().as_secs_f64() * 1e3);
                assert_eq!(decoded.as_slice(), bytes.as_slice(), "whole read exact");
            }

            // 100 rows and 1 row at the top of the tensor.
            for (label, rows) in [("region_100_rows", 100u64), ("region_1_row", 1)] {
                let extent = [rows, shape[1]];
                let region = weftcodec::Region::new(vec![0, 0], vec![extent[0], extent[1]]);
                let mut samples = Vec::new();
                let mut units = 0;
                let mut read_bytes = 0u64;
                let expect = &bytes[..(rows * shape[1]) as usize * 2];
                for _ in 0..reps {
                    let start = Instant::now();
                    let read = store
                        .read_tensor_region_with_options(*id, &region, &read_options)
                        .expect("region read");
                    samples.push(start.elapsed().as_secs_f64() * 1e3);
                    assert_eq!(read.bytes.as_slice(), expect, "{label} exact");
                    units = read.units_touched;
                    read_bytes = read.bytes.len() as u64;
                }
                println!(
                    "{}",
                    json!({
                        "schema": "weft.decode-regions-bench.v1",
                        "event": "read",
                        "strategy": strategy.as_str(),
                        "threads": threads,
                        "shape": label,
                        "rows": rows,
                        "region_bytes": read_bytes,
                        "units_touched": units,
                        "median_ms": median(samples.clone()),
                        "p99_ms": samples.iter().cloned().fold(0.0f64, f64::max),
                        "samples_ms": samples,
                    })
                );
            }
            let whole = median(whole_ms.clone());
            println!(
                "{}",
                json!({
                    "schema": "weft.decode-regions-bench.v1",
                    "event": "read",
                    "strategy": strategy.as_str(),
                    "threads": threads,
                    "shape": "whole",
                    "rows": shape[0],
                    "region_bytes": bytes.len(),
                    "units_touched": 0,
                    "median_ms": whole,
                    "p99_ms": whole_ms.iter().cloned().fold(0.0f64, f64::max),
                    "samples_ms": whole_ms,
                })
            );
            comparisons.push((strategy.as_str(), threads, whole));
        }
    }
    let _ = comparisons;

    if let Some(path) = out_path {
        std::fs::write(path, "{\"schema\":\"weft.decode-regions-bench.v1\"}\n").ok();
    }
}
