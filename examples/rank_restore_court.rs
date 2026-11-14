//! T053 court: rank-directed cross-topology restore on a real checkpoint.
//!
//! The store is written 8-way on axis 0 from a real safetensors container. For
//! every target topology the harness asks each rank for exactly the extents it
//! owns, compares every materialised byte with the corresponding slice of a
//! full-tensor restore, and reports the physical read amplification.
//!
//! Usage (inside the court, after the checkpoint exists):
//!
//! ```sh
//! cargo run --release --example rank_restore_court -- \
//!   --store DIR --checkpoint qwen --source FILE --receipt OUT.json \
//!   --stamp 20261114T000000Z --topologies 1,2,4,8 --axis 0
//! ```

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Map, Value};
use weftfs::checkpoint::{CheckpointManifest, RestoreTopology};
use weftfs::safetensors::sha256_file;
use weftfs::store::{OpenMode, Store};

fn arg_value(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next();
        }
    }
    None
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// Copy the region `[start, end)` out of a row-major tensor.
fn region_bytes(
    shape: &[u64],
    element_bytes: usize,
    payload: &[u8],
    start: &[u64],
    end: &[u64],
) -> Vec<u8> {
    let rank = shape.len();
    let out_dims: Vec<usize> = start
        .iter()
        .zip(end.iter())
        .map(|(s, e)| (*e - *s) as usize)
        .collect();
    if rank == 1 {
        let base = start[0] as usize * element_bytes;
        let len = out_dims[0] * element_bytes;
        return payload[base..base + len].to_vec();
    }
    let strides: Vec<usize> = {
        let mut strides = vec![1usize; rank];
        for axis in (0..rank - 1).rev() {
            strides[axis] = strides[axis + 1] * shape[axis + 1] as usize;
        }
        strides
    };
    let total: usize = out_dims.iter().product();
    let mut out = Vec::with_capacity(total * element_bytes);
    let mut index = vec![0usize; rank];
    loop {
        let mut offset = 0usize;
        for axis in 0..rank {
            offset += (start[axis] as usize + index[axis]) * strides[axis];
        }
        let base = offset * element_bytes;
        out.extend_from_slice(&payload[base..base + out_dims[rank - 1] * element_bytes]);
        // Advance all axes except the last (the last was copied whole).
        let mut axis = rank as isize - 2;
        while axis >= 0 {
            let a = axis as usize;
            index[a] += 1;
            if index[a] < out_dims[a] {
                break;
            }
            index[a] = 0;
            axis -= 1;
        }
        if axis < 0 {
            break;
        }
    }
    out
}

struct TensorAggregate {
    needed: u64,
    representation: u64,
    backend: u64,
    reads: u64,
    ranks: u64,
    predicted: Vec<u32>,
    shards: Vec<u32>,
    exact: bool,
}

struct RankAggregate {
    needed: u64,
    representation: u64,
    backend: u64,
    reads: u64,
    tensors: u64,
    shards: u64,
    exact: bool,
}

struct Outcome {
    config: Value,
    checks: Vec<(String, bool)>,
    ok: bool,
}

fn run_config(
    store: &Store,
    checkpoint: &str,
    manifest: &CheckpointManifest,
    stored_axis: u32,
    config_axis: u32,
    shards: u32,
    label: &str,
) -> Outcome {
    let topology = RestoreTopology::Sharded {
        axis: config_axis,
        shards,
    };
    let mut per_tensor: Vec<TensorAggregate> = manifest
        .tensors
        .iter()
        .map(|_| TensorAggregate {
            needed: 0,
            representation: 0,
            backend: 0,
            reads: 0,
            ranks: 0,
            predicted: Vec::new(),
            shards: Vec::new(),
            exact: true,
        })
        .collect();
    let mut per_rank: Vec<RankAggregate> = (0..shards)
        .map(|_| RankAggregate {
            needed: 0,
            representation: 0,
            backend: 0,
            reads: 0,
            tensors: 0,
            shards: 0,
            exact: true,
        })
        .collect();
    let mut fetched_set_matches_plan = true;
    let mut mismatch_rows = Vec::new();
    let config_started = Instant::now();
    for (tensor_index, tensor) in manifest.tensors.iter().enumerate() {
        // One oracle tensor at a time: the full-tensor restore the topology
        // proof already ships.
        let oracle = store
            .restore_named_tensor_at(None, checkpoint, &tensor.name, None, None)
            .expect("oracle tensor");
        assert_eq!(
            oracle.report.shape, tensor.shape,
            "oracle shape for {}",
            tensor.name
        );
        let elements: u64 = tensor.shape.iter().product();
        let element_bytes = (tensor.element_bytes / elements) as usize;
        for rank in 0..shards {
            let plan = store
                .plan_rank_restore(checkpoint, &topology, rank)
                .expect("plan");
            let planned = plan
                .tensors
                .iter()
                .find(|row| row.name == tensor.name)
                .expect("planned tensor");
            let mut payload = Vec::new();
            let report = store
                .restore_checkpoint_rank(
                    checkpoint,
                    &topology,
                    rank,
                    Some(&tensor.name),
                    |row, bytes| {
                        payload = bytes.to_vec();
                        let _ = row;
                        Ok(())
                    },
                )
                .expect("rank restore");
            let row = &report.tensors[0];
            let expected = region_bytes(
                &tensor.shape,
                element_bytes,
                &oracle.bytes,
                &row.start,
                &row.end,
            );
            let exact = payload == expected
                && row.content_root == weftfs::identity::content_hash(&expected)
                && row.needed_bytes == planned.needed_bytes
                && row.predicted_shards == planned.stored_shards
                && row.shard_indices == planned.stored_shards
                && row.representation_bytes == planned.stored_shard_bytes;
            if !exact && mismatch_rows.len() < 20 {
                mismatch_rows.push(format!("{} rank {rank}/{shards}", tensor.name));
            }
            if !report.fetched_set_matches_plan {
                fetched_set_matches_plan = false;
            }
            let aggregate = &mut per_tensor[tensor_index];
            aggregate.needed += row.needed_bytes;
            aggregate.representation += row.representation_bytes;
            aggregate.backend += row.backend_bytes_read;
            aggregate.reads += row.store_reads;
            aggregate.ranks += 1;
            aggregate.predicted = row.predicted_shards.clone();
            aggregate.shards = row.shard_indices.clone();
            aggregate.exact &= exact;
            let rank_row = &mut per_rank[rank as usize];
            rank_row.needed += row.needed_bytes;
            rank_row.representation += row.representation_bytes;
            rank_row.backend += row.backend_bytes_read;
            rank_row.reads += row.store_reads;
            rank_row.tensors += 1;
            rank_row.shards += row.shard_indices.len() as u64;
            rank_row.exact &= exact;
        }
    }
    let wall_ms = config_started.elapsed().as_secs_f64() * 1000.0;
    let needed: u64 = per_tensor.iter().map(|row| row.needed).sum();
    let representation: u64 = per_tensor.iter().map(|row| row.representation).sum();
    let backend: u64 = per_tensor.iter().map(|row| row.backend).sum();
    let reads: u64 = per_tensor.iter().map(|row| row.reads).sum();
    let exact_tensors = per_tensor.iter().filter(|row| row.exact).count();
    let tensor_rows: Vec<Value> = manifest
        .tensors
        .iter()
        .zip(per_tensor.iter())
        .map(|(tensor, row)| {
            json!({
                "name": tensor.name,
                "shape": tensor.shape,
                "ranks": row.ranks,
                "needed_bytes": row.needed,
                "representation_bytes": row.representation,
                "backend_bytes_read": row.backend,
                "store_reads": row.reads,
                "read_amplification": if row.needed == 0 { Value::Null } else {
                    json!(row.backend as f64 / row.needed as f64)
                },
                "representation_amplification": if row.needed == 0 { Value::Null } else {
                    json!(row.representation as f64 / row.needed as f64)
                },
                "shard_indices_per_rank": row.shards,
                "predicted_shards": row.predicted,
                "exact": row.exact,
            })
        })
        .collect();
    let rank_rows: Vec<Value> = per_rank
        .iter()
        .enumerate()
        .map(|(rank, row)| {
            json!({
                "rank": rank,
                "tensors": row.tensors,
                "needed_bytes": row.needed,
                "representation_bytes": row.representation,
                "backend_bytes_read": row.backend,
                "store_reads": row.reads,
                "shards_read": row.shards,
                "read_amplification": if row.needed == 0 { Value::Null } else {
                    json!(row.backend as f64 / row.needed as f64)
                },
                "exact": row.exact,
            })
        })
        .collect();
    let exact = exact_tensors == manifest.tensors.len() && mismatch_rows.is_empty();
    let ok = exact && fetched_set_matches_plan;
    Outcome {
        config: json!({
            "label": label,
            "axis": config_axis,
            "stored_axis": stored_axis,
            "shards": shards,
            "ranks": shards,
            "wall_ms": wall_ms,
            "needed_bytes": needed,
            "representation_bytes": representation,
            "backend_bytes_read": backend,
            "store_reads": reads,
            "read_amplification": if needed == 0 { Value::Null } else {
                json!(backend as f64 / needed as f64)
            },
            "representation_amplification": if needed == 0 { Value::Null } else {
                json!(representation as f64 / needed as f64)
            },
            "exact_tensors": exact_tensors,
            "tensors": manifest.tensors.len(),
            "mismatched_rows": mismatch_rows,
            "fetched_set_matches_plan": fetched_set_matches_plan,
            "per_rank": rank_rows,
            "per_tensor": tensor_rows,
        }),
        checks: vec![
            (format!("{label}_exact"), exact),
            (
                format!("{label}_fetched_set_matches_plan"),
                fetched_set_matches_plan,
            ),
        ],
        ok,
    }
}

fn main() {
    let store_path = PathBuf::from(arg_value("--store").expect("--store"));
    let checkpoint = arg_value("--checkpoint").expect("--checkpoint");
    let source = PathBuf::from(arg_value("--source").expect("--source"));
    let receipt_path = PathBuf::from(arg_value("--receipt").expect("--receipt"));
    let stamp = arg_value("--stamp").unwrap_or_else(|| "unknown".to_owned());
    let topologies: Vec<u32> = arg_value("--topologies")
        .unwrap_or_else(|| "1,2,4,8".to_owned())
        .split(',')
        .map(|value| value.trim().parse().expect("topology count"))
        .collect();
    let axis: u32 = arg_value("--axis")
        .unwrap_or_else(|| "0".to_owned())
        .parse()
        .expect("axis");
    let cross_axis: Option<u32> = arg_value("--cross-axis").and_then(|value| value.parse().ok());
    let cross_shards: u32 = arg_value("--cross-shards")
        .unwrap_or_else(|| "4".to_owned())
        .parse()
        .expect("cross shards");
    let non_divisible: u32 = arg_value("--non-divisible")
        .unwrap_or_else(|| "3".to_owned())
        .parse()
        .expect("non-divisible shards");

    let store = Store::open(&store_path, OpenMode::ReadOnly).expect("open store");
    let manifest = store.checkpoint_manifest(&checkpoint).expect("manifest");
    let manifest_root = manifest.root().expect("manifest root").to_hex();
    let stored_axis = manifest.tensors[0].shards[0].axis;
    let stored_shards = manifest.tensors[0].shards[0].shards;
    let source_sha = sha256_file(&source).expect("source sha");
    let full_started = Instant::now();
    let (_container, full_report) = store
        .restore_checkpoint_bytes(&checkpoint, &Default::default())
        .expect("full restore");
    let full_ms = full_started.elapsed().as_secs_f64() * 1000.0;

    let mut configs = Vec::new();
    let mut checks: Map<String, Value> = Map::new();
    let mut losses: Vec<String> = Vec::new();
    let mut all_ok = true;

    for &shards in &topologies {
        let outcome = run_config(
            &store,
            &checkpoint,
            &manifest,
            stored_axis,
            axis,
            shards,
            &format!("axis{axis}-{shards}way"),
        );
        for (name, value) in outcome.checks {
            checks.insert(name, json!(value));
        }
        all_ok &= outcome.ok;
        configs.push(outcome.config);
    }
    if let Some(other_axis) = cross_axis {
        let outcome = run_config(
            &store,
            &checkpoint,
            &manifest,
            stored_axis,
            other_axis,
            cross_shards,
            &format!("axis{other_axis}-{cross_shards}way"),
        );
        for (name, value) in outcome.checks {
            checks.insert(name, json!(value));
        }
        all_ok &= outcome.ok;
        configs.push(outcome.config);
        losses.push(format!(
            "A target split on axis {other_axis} while the checkpoint is stored on axis {stored_axis} \
             makes every destination rank intersect every stored shard; the measured amplification is \
             printed, not excused."
        ));
    }

    // Non-divisible target: the writer's typed refusal, before any payload read.
    let err = store
        .plan_rank_restore(
            &checkpoint,
            &RestoreTopology::Sharded {
                axis,
                shards: non_divisible,
            },
            0,
        )
        .expect_err("a non-divisible topology must be refused");
    let mut emitted = 0usize;
    let _ = store.restore_checkpoint_rank(
        &checkpoint,
        &RestoreTopology::Sharded {
            axis,
            shards: non_divisible,
        },
        0,
        None,
        |_row, _bytes| {
            emitted += 1;
            Ok(())
        },
    );
    let refused = err.class() == weftfs::Class::NonCanonical
        && err.to_string().contains("no silent padding")
        && emitted == 0;
    checks.insert("non_divisible_refused".to_owned(), json!(refused));
    all_ok &= refused;

    let receipt = json!({
        "schema": "weft.rank-restore.v1",
        "stamp": stamp,
        "court": {
            "image": env_or("WEFT_COURT_IMAGE", "unknown"),
            "kernel": env_or("WEFT_COURT_KERNEL", "unknown"),
            "memory": env_or("WEFT_COURT_MEMORY", "unknown"),
            "cpus": env_or("WEFT_COURT_CPUS", "unknown"),
            "pids": env_or("WEFT_COURT_PIDS", "unknown"),
            "cpu_only": true,
            "single_host": true,
            "note": "amplification and latency only; no GPU, no multi-host, no fleet claim",
        },
        "revision": {
            "weftfs": env_or("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec": env_or("WEFT_REPO_WEFTCODEC", "unknown"),
        },
        "fixture": {
            "source": source.display().to_string(),
            "source_sha256": hex::encode(source_sha),
            "checkpoint": checkpoint,
            "manifest_root": manifest_root,
            "tensors": manifest.tensors.len(),
            "element_bytes": manifest.element_bytes,
            "stored_axis": stored_axis,
            "stored_shards": stored_shards,
        },
        "full_restore": {
            "exact_source_file": full_report.exact_source_file,
            "file_sha256": hex::encode(full_report.file_sha256),
            "wall_ms": full_ms,
            "backend_bytes_read": full_report.store_bytes_read,
        },
        "non_divisible": {
            "shards": non_divisible,
            "axis": axis,
            "class": err.class().as_str(),
            "message": err.to_string(),
        },
        "configs": configs,
        "checks": checks,
        "losses": losses,
        "result": if all_ok { "pass" } else { "fail" },
    });
    fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).expect("write receipt");
    println!(
        "rank-restore court: {} ({} configs, {} tensors) -> {}",
        if all_ok { "PASS" } else { "FAIL" },
        topologies.len() + cross_axis.map(|_| 1).unwrap_or(0),
        manifest.tensors.len(),
        receipt_path.display()
    );
    if !all_ok {
        std::process::exit(1);
    }
}
