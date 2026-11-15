//! X21 court harness (Rust side) — restoration-shaped blob reads with separated
//! counters, on the same fixture the Go `poolbench` reads.
//!
//! Modes:
//!
//! * `--build <dir> --objects N --ids <file> [--packed] [--chunk BYTES]` creates
//!   a store of raw tensor objects and writes their ids.
//! * `--run ... --rounds N --seed S [--packed]` reads every id once per round in
//!   a seeded random order and reports wall, logical bytes, the backend counters
//!   the store's own `BackendStats` provides, and the read-pool counters.
//! * `--vanish --store <dir>` deletes one stored chunk blob so both languages can
//!   be shown to fail closed.
//!
//! Nothing here measures the Go side; the court compares the two JSON documents.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use weftfs::state::State;
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy};
use weftfs::{BackendKind, OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const ROWS: u64 = 128;
const COLS: u64 = 512;

fn options(chunk: u64) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: chunk,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn pack_options() -> weftfs::packs::PackOptions {
    weftfs::packs::PackOptions::default()
}

fn tensor(seed: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity((ROWS * COLS * 2) as usize);
    for _ in 0..(ROWS * COLS) / 4 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn build(dir: &Path, objects: usize, chunk: u64, packed: bool) -> Vec<weftfs::Hash32> {
    let _ = fs::remove_dir_all(dir);
    let kind = if packed {
        BackendKind::packed(BackendKind::Local, pack_options()).unwrap()
    } else {
        BackendKind::Local
    };
    let mut store = Store::init_with_backend(dir, options(chunk), kind).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let mut ids = Vec::new();
    for index in 0..objects {
        let data = tensor(index as u32);
        let id = store
            .write_tensor_file(
                State::ROOT_ID,
                format!("o{index:03}").as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &data,
                &layout,
                Some(TensorStrategy::Raw),
            )
            .unwrap();
        ids.push(id);
    }
    assert!(store.verify(true).unwrap().ok);
    ids
}

fn ids_file(path: &Path, ids: &[weftfs::Hash32]) {
    let mut text = String::new();
    for id in ids {
        text.push_str(&id.to_hex());
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

fn read_ids(path: &Path) -> Vec<weftfs::Hash32> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| weftfs::Hash32::from_hex(line.trim()).unwrap())
        .collect()
}

fn vanish_one_chunk(dir: &Path) -> Option<PathBuf> {
    // Delete exactly one stored chunk blob. Which object it belongs to is not
    // recorded on purpose: the claim is that *a* vanished blob is refused, and
    // the arm reports how many reads failed.
    let objects = dir.join("objects");
    let mut stack = vec![objects];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            fs::remove_file(&path).unwrap();
            return Some(path);
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut build_dir: Option<PathBuf> = None;
    let mut store_dir: Option<PathBuf> = None;
    let mut ids_path: Option<PathBuf> = None;
    let mut json_path: Option<PathBuf> = None;
    let mut objects = 64usize;
    let mut rounds = 1usize;
    let mut seed = 1u64;
    let mut chunk = 4096u64;
    let mut packed = false;
    let mut vanish = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--build" => {
                build_dir = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--store" => {
                store_dir = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--ids" => {
                ids_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--objects" => {
                objects = args[index + 1].parse().unwrap();
                index += 2;
            }
            "--rounds" => {
                rounds = args[index + 1].parse().unwrap();
                index += 2;
            }
            "--seed" => {
                seed = args[index + 1].parse().unwrap();
                index += 2;
            }
            "--chunk" => {
                chunk = args[index + 1].parse().unwrap();
                index += 2;
            }
            "--packed" => {
                packed = true;
                index += 1;
            }
            "--vanish" => {
                vanish = true;
                index += 1;
            }
            other => panic!("unknown argument {other}"),
        }
    }

    if let Some(dir) = &build_dir {
        let ids = build(dir, objects, chunk, packed);
        let ids_file_path = ids_path.clone().unwrap_or_else(|| dir.join("ids.txt"));
        ids_file(&ids_file_path, &ids);
        println!(
            "{}",
            json!({"built": dir.display().to_string(), "objects": ids.len(), "ids": ids_file_path.display().to_string(), "packed": packed, "chunk": chunk})
        );
        return;
    }

    let store_dir = store_dir.expect("--store is required for a run");
    let ids_path = ids_path.expect("--ids is required for a run");

    if vanish {
        let removed = vanish_one_chunk(&store_dir);
        println!(
            "{}",
            json!({"vanished": removed.map(|path| path.display().to_string())})
        );
        return;
    }

    let ids = read_ids(&ids_path);
    let kind = if packed {
        BackendKind::packed(BackendKind::Local, pack_options()).unwrap()
    } else {
        BackendKind::Local
    };
    let store =
        Store::open_with_options_and_backend(&store_dir, OpenMode::ReadOnly, options(chunk), kind)
            .unwrap();
    let before = store.backend_stats();
    let started = std::time::Instant::now();
    let mut reads = 0usize;
    let mut failures = 0usize;
    let mut first_error = String::new();
    let mut bytes = 0u64;
    let mut order: Vec<usize> = (0..ids.len()).collect();
    for round in 0..rounds {
        // Seeded Fisher-Yates, the same schedule the Go bench derives from
        // `seed + round`.
        let mut state = (seed.wrapping_add(round as u64)) | 1;
        for position in (1..order.len()).rev() {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            let swap = (value % (position as u64 + 1)) as usize;
            order.swap(position, swap);
        }
        for index in &order {
            reads += 1;
            match store.read_object(ids[*index]) {
                Ok(data) => bytes += data.len() as u64,
                Err(err) => {
                    failures += 1;
                    if first_error.is_empty() {
                        first_error = format!("{}: {}", err.class_name(), err.message());
                    }
                }
            }
        }
    }
    let wall = started.elapsed().as_secs_f64();
    let after = store.backend_stats();
    let delta = after.delta_since(&before);
    let pool = store.read_pool_stats();
    let mut pool_json = serde_json::Map::new();
    pool_json.insert("reads".to_owned(), json!(pool.reads));
    pool_json.insert("bytes".to_owned(), json!(pool.bytes));
    pool_json.insert("handles_created".to_owned(), json!(pool.handles_created));
    pool_json.insert("handles_reused".to_owned(), json!(pool.handles_reused));
    pool_json.insert("idle_handles".to_owned(), json!(pool.idle_handles));
    pool_json.insert("poolable".to_owned(), json!(pool.poolable));
    let document = json!({
        "schema": "weft.rust-pool-counters.v1",
        "language": "rust",
        "store": store_dir.display().to_string(),
        "packed": packed,
        "objects": ids.len(),
        "rounds": rounds,
        "reads": reads,
        "failures": failures,
        "first_error": first_error,
        "wall_ms": wall * 1000.0,
        "logical_mib_per_s": (bytes as f64 / (1024.0 * 1024.0)) / wall,
        "counters": {
            "total_blob_reads": reads,
            "total_blob_bytes": bytes,
            "backend_reads": delta.reads + delta.range_reads,
            "backend_bytes": delta.bytes_read,
        },
        "pool_stats": pool_json,
        "notes": [
            "Rust has no separate plane-level counter: backend_reads is the store's own BackendStats delta",
            "total_blob_reads counts the read_object calls this harness made"
        ]
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(path, text).unwrap(),
        None => println!("{text}"),
    }
    if failures != 0 {
        std::process::exit(1);
    }
    let _: BTreeMap<String, u64> = BTreeMap::new();
    let _ = std::io::stdout().flush();
}
