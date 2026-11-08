//! Stage 2 court harness: a whole training trajectory in near-final size.
//!
//! Each step is one `weft.checkpoint.v1` checkpoint written with the previous
//! step as its causal checkpoint, so the planner can price weight/sparse deltas
//! against the matching tensor of the previous step. The harness records, per
//! step:
//!
//! * marginal and cumulative physical bytes (objects + manifests);
//! * the exact-source restore, per-tensor SHA-256 and the strategy mix;
//! * full / single-tensor / 1-row restore time and the deepest dependency
//!   chain;
//! * three independent baselines computed with the external `zstd` CLI:
//!   per-step full `zstd -19`, byte-wise XOR deltas compressed with `zstd -19`,
//!   and whole-file `zstd --patch-from` of the previous step.
//!
//! `gc` then unlinks every checkpoint except the final one and reports what the
//! dependency-aware GC retains, with and without a snapshot pinned at each
//! step. `assemble` merges the phase files into the receipt.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{OpenMode, Options, Store};

fn main() {
    if let Err(err) = run() {
        eprintln!("trajectory_court: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(phase) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match phase {
        "gen" => gen(&args[1..]),
        "series" => series(&args[1..]),
        "gc" => gc(&args[1..]),
        "assemble" => assemble(&args[1..]),
        other => Err(format!("unknown phase `{other}`\n{}", usage())),
    }
}

fn usage() -> String {
    "usage: trajectory_court <gen|series|gc|assemble> ...".to_owned()
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|item| item == name)
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn data_bytes(store: &Path) -> u64 {
    dir_bytes(&store.join("objects")) + dir_bytes(&store.join("manifests"))
}

/// Run `zstd` and return the compressed size.
fn zstd_size(args: &[&str]) -> Result<u64, String> {
    let output = Command::new("zstd")
        .args(args)
        .output()
        .map_err(|err| format!("zstd: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "zstd failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // `-c` writes the frame to stdout.
    Ok(output.stdout.len() as u64)
}

fn zstd_file_size(input: &Path, level: &str, extra: &[&str]) -> Result<u64, String> {
    let mut args = vec!["-q", "-c", level];
    args.extend_from_slice(extra);
    let path = input.to_string_lossy().to_string();
    args.push(&path);
    zstd_size(&args)
}

/// Try `zstd -19`; if the level cannot run inside the court's memory bound,
/// fall back to `-12` and report which level produced the number.
fn zstd_adaptive(input: &Path, extra: &[&str]) -> (Option<u64>, String, Option<String>) {
    match zstd_file_size(input, "-19", extra) {
        Ok(size) => (Some(size), "-19".to_owned(), None),
        Err(err19) => match zstd_file_size(input, "-12", extra) {
            Ok(size) => (
                Some(size),
                "-12".to_owned(),
                Some(format!("-19 unavailable: {err19}")),
            ),
            Err(err12) => (
                None,
                "none".to_owned(),
                Some(format!("-19: {err19}; -12: {err12}")),
            ),
        },
    }
}

fn tensor_payloads(
    file: &SafetensorsFile,
) -> Result<BTreeMap<String, (String, Vec<u64>, Vec<u8>)>, String> {
    let mut out = BTreeMap::new();
    for tensor in &file.tensors {
        let bytes = file
            .read_tensor(tensor)
            .map_err(|err| format!("{}: {err}", tensor.name))?;
        out.insert(
            tensor.name.clone(),
            (tensor.dtype.clone(), tensor.shape.clone(), bytes),
        );
    }
    Ok(out)
}

/// Declared synthetic series: `steps` checkpoints of `tensors` BF16 tensors
/// evolved by one documented deterministic rule.
///
/// Rule: element `i` of tensor `t` starts at a seeded value. At step `s > 0`
/// every element with `(i + 131 * t) % 4093 == s % 4093` is replaced by
/// `value + 2^-3` (rounded to BF16), i.e. about 0.024% of the elements move per
/// step. The file header is the canonical safetensors header from this crate,
/// so the source SHA-256 is recorded per step.
fn gen(args: &[String]) -> Result<(), String> {
    let dir = PathBuf::from(arg(args, "--dir").ok_or("gen needs --dir")?);
    let steps: usize = arg(args, "--steps").unwrap_or("50").parse().map_err(|_| "bad --steps")?;
    let tensors: usize = arg(args, "--tensors").unwrap_or("64").parse().map_err(|_| "bad --tensors")?;
    let rows: usize = arg(args, "--rows").unwrap_or("256").parse().map_err(|_| "bad --rows")?;
    let cols: usize = arg(args, "--cols").unwrap_or("256").parse().map_err(|_| "bad --cols")?;
    let seed: u64 = arg(args, "--seed").unwrap_or("20261107").parse().map_err(|_| "bad --seed")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut base: Vec<Vec<u16>> = Vec::with_capacity(tensors);
    let mut files = Vec::new();
    for tensor in 0..tensors {
        let mut state = seed ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(tensor as u64 + 1));
        let mut words = Vec::with_capacity(rows * cols);
        for _ in 0..rows * cols {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            words.push(((state >> 48) as u16) & 0x3f80);
        }
        base.push(words);
    }
    let shape = vec![rows as u64, cols as u64];
    let mut state = seed;
    for step in 0..steps {
        if step > 0 {
            for (tensor, words) in base.iter_mut().enumerate() {
                let phase = (step as u64) % 4093;
                for (index, word) in words.iter_mut().enumerate() {
                    if ((index as u64 + 131 * tensor as u64) % 4093) == phase {
                        // Add 1/8 in BF16 exponent space by incrementing the
                        // mantissa of a bounded value; deterministic and exact.
                        *word = word.wrapping_add(0x0100);
                    }
                }
            }
        }
        let mut entries = Vec::with_capacity(tensors);
        let mut owned: Vec<(String, Vec<u8>)> = Vec::with_capacity(tensors);
        for (tensor, words) in base.iter().enumerate() {
            let mut bytes = Vec::with_capacity(words.len() * 2);
            for word in words {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            owned.push((format!("synthetic.block{tensor:02}.weight"), bytes));
        }
        // Silence the unused `state`.
        state = state.wrapping_add(1);
        for (name, bytes) in &owned {
            entries.push(weftfs::safetensors::SafetensorsEntry {
                name,
                dtype: "bf16",
                shape: &shape,
                data: bytes,
            });
        }
        let file = weftfs::safetensors::encode_file(&entries, None)
            .map_err(|e| e.to_string())?;
        let path = dir.join(format!("step{:03}.safetensors", step + 1));
        fs::write(&path, file).map_err(|e| e.to_string())?;
        files.push(path);
    }
    let list = files
        .iter()
        .enumerate()
        .map(|(index, path)| format!("synthetic-step{:03}={}", index + 1, path.display()))
        .collect::<Vec<_>>()
        .join(",");
    let report = json!({
        "phase": "gen",
        "dir": dir.display().to_string(),
        "steps": steps,
        "tensors": tensors,
        "shape": [rows, cols],
        "rule": "(i + 131*t) % 4093 == step % 4093 -> BF16 word + 0x0100",
        "files": files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "steps_arg": list,
    });
    let out = dir.join("gen.json");
    fs::write(&out, serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("gen wrote {}", out.display());
    let _ = state;
    Ok(())
}

fn series(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(arg(args, "--store").ok_or("series needs --store")?);
    let out = PathBuf::from(arg(args, "--out").ok_or("series needs --out")?);
    let steps: Vec<&str> = arg(args, "--steps")
        .ok_or("series needs --steps name=FILE,name=FILE,...")?
        .split(',')
        .collect();
    let shards: u32 = arg(args, "--shards").unwrap_or("1").parse().map_err(|_| "bad --shards")?;
    let strategy = arg(args, "--strategy").unwrap_or("planned");
    let snapshots = flag(args, "--snapshots");
    let baselines = !flag(args, "--no-baselines");
    let semantic = !flag(args, "--no-semantic");
    let mut single = arg(args, "--single").unwrap_or_default().to_owned();
    let region_text = arg(args, "--range").unwrap_or("0:1");
    let region = {
        let mut start = Vec::new();
        let mut end = Vec::new();
        for pair in region_text.split(',') {
            let (a, b) = pair.split_once(':').ok_or("bad --range")?;
            start.push(a.parse::<u64>().map_err(|_| "bad range start")?);
            end.push(b.parse::<u64>().map_err(|_| "bad range end")?);
        }
        weftcodec::Region::new(start, end)
    };

    if store_path.exists() {
        fs::remove_dir_all(&store_path).map_err(|e| e.to_string())?;
    }
    let mut store = Store::init(
        &store_path,
        Options {
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .map_err(|e| e.to_string())?;

    let parsed: Vec<(&str, &str)> = steps
        .iter()
        .map(|entry| entry.split_once('=').ok_or("step must be name=FILE"))
        .collect::<Result<_, _>>()?;
    let mut previous_checkpoint: Option<String> = None;
    let mut previous_file: Option<PathBuf> = None;
    let mut rows = Vec::new();

    for (index, (name, path)) in parsed.iter().enumerate() {
        let source = PathBuf::from(path);
        let file = SafetensorsFile::parse(&source).map_err(|e| e.to_string())?;
        if single.is_empty() {
            single = file
                .tensors
                .first()
                .map(|tensor| tensor.name.clone())
                .ok_or("the step container has no tensors")?;
        }
        let before = data_bytes(&store_path);
        let started = Instant::now();
        let report = store
            .write_checkpoint(
                name,
                &source,
                &CheckpointWriteOptions {
                    shards,
                    axis: 0,
                    filters: Vec::new(),
                    strategy: ShardStrategy::parse(strategy).map_err(|e| e.to_string())?,
                    tile_shape: None,
                    causal_checkpoint: previous_checkpoint.clone(),
                    semantic_search: semantic,
                    ..CheckpointWriteOptions::default()
                },
            )
            .map_err(|e| e.to_string())?;
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = data_bytes(&store_path);
        let snapshot = if snapshots {
            Some(
                store
                    .snapshot_create(name)
                    .map_err(|e| e.to_string())?
                    .to_hex(),
            )
        } else {
            None
        };
        eprintln!(
            "trajectory: step {index} {name}: write {write_ms:.1} ms, {} shards, data {after} B",
            report.shard_objects
        );

        // Strategy mix and deepest dependency chain.
        let mut strategies: BTreeMap<String, u64> = BTreeMap::new();
        let mut max_depth = 0u32;
        for tensor in &report.tensors {
            for id in &tensor.object_ids {
                let meta = store.tensor_metadata(*id).map_err(|e| e.to_string())?;
                *strategies.entry(meta.strategy).or_insert(0) += 1;
                max_depth = max_depth.max(
                    store
                        .shard_dependency_depth(*id)
                        .map_err(|e| e.to_string())?,
                );
            }
        }

        // Exact restore of this checkpoint.
        let mut bytes = Vec::new();
        let full_started = Instant::now();
        let restored = store
            .restore_checkpoint(name, &CheckpointRestoreOptions::default(), &mut bytes)
            .map_err(|e| e.to_string())?;
        let full_ms = full_started.elapsed().as_secs_f64() * 1000.0;
        drop(bytes);
        let source_sha =
            weftfs::safetensors::sha256(&fs::read(&source).map_err(|e| e.to_string())?);
        // Read one source tensor at a time: holding a whole container in memory
        // is what pushed the 0.5B pair and pythia over the 3 GiB court bound.
        let mut all_tensors_exact = true;
        let mut tensor_rows = Vec::with_capacity(restored.tensors.len());
        for tensor in &restored.tensors {
            let expected = file
                .tensors
                .iter()
                .find(|item| item.name == tensor.name)
                .ok_or_else(|| format!("{} is not in the source container", tensor.name))?;
            let payload = file.read_tensor(expected).map_err(|e| e.to_string())?;
            let exact = weftfs::safetensors::sha256(&payload) == tensor.sha256;
            all_tensors_exact &= exact;
            tensor_rows.push(json!({
                "name": tensor.name,
                "sha256": hex::encode(tensor.sha256),
                "exact": exact,
            }));
        }

        let single_started = Instant::now();
        let single_bytes = store
            .restore_named_tensor_at(None, name, &single, None, None)
            .map_err(|e| e.to_string())?;
        let single_ms = single_started.elapsed().as_secs_f64() * 1000.0;
        let row_started = Instant::now();
        let row_bytes = store
            .restore_named_tensor_at(None, name, &single, Some(&region), None)
            .map_err(|e| e.to_string())?;
        let row_ms = row_started.elapsed().as_secs_f64() * 1000.0;

        let mut baseline_row = json!({"skipped": "--no-baselines"});
        if baselines {
            // Baselines are independent evidence: a level that cannot run
            // inside the court memory bound is recorded, falls back to -12,
            // and never aborts the trajectory.
            let mut full_zstd19 = 0u64;
            let mut full_level = "-19".to_owned();
            let mut full_note: Option<String> = None;
            for tensor in &file.tensors {
                let payload = file.read_tensor(tensor).map_err(|e| e.to_string())?;
                let tmp = store_path.join(format!("zstd-tmp-{index}.bin"));
                fs::write(&tmp, &payload).map_err(|e| e.to_string())?;
                let (size, level, note) = zstd_adaptive(&tmp, &[]);
                let _ = fs::remove_file(&tmp);
                if let Some(size) = size {
                    full_zstd19 = full_zstd19.saturating_add(size);
                }
                if level != full_level {
                    full_level = level;
                }
                if full_note.is_none() {
                    full_note = note;
                }
            }
            let mut xor_delta = 0u64;
            let mut xor_level = "-19".to_owned();
            let mut xor_note: Option<String> = None;
            if let Some(previous) = &previous_file {
                let previous = SafetensorsFile::parse(previous).map_err(|e| e.to_string())?;
                for tensor in &file.tensors {
                    let Some(base_tensor) = previous
                        .tensors
                        .iter()
                        .find(|item| item.name == tensor.name)
                    else {
                        continue;
                    };
                    if base_tensor.dtype != tensor.dtype || base_tensor.shape != tensor.shape {
                        continue;
                    }
                    let current = file.read_tensor(tensor).map_err(|e| e.to_string())?;
                    let base = previous
                        .read_tensor(base_tensor)
                        .map_err(|e| e.to_string())?;
                    if base.len() != current.len() {
                        continue;
                    }
                    let delta: Vec<u8> =
                        current.iter().zip(base.iter()).map(|(a, b)| a ^ b).collect();
                    let tmp = store_path.join(format!(
                        "xor-{index}-{}.bin",
                        tensor.name.replace('/', "_")
                    ));
                    fs::write(&tmp, &delta).map_err(|e| e.to_string())?;
                    let (size, level, note) = zstd_adaptive(&tmp, &[]);
                    let _ = fs::remove_file(&tmp);
                    if let Some(size) = size {
                        xor_delta = xor_delta.saturating_add(size);
                    }
                    if level != xor_level {
                        xor_level = level;
                    }
                    if xor_note.is_none() {
                        xor_note = note;
                    }
                }
            }
            let (patch_from, patch_level, patch_note) = match &previous_file {
                None => (Some(0u64), "-19".to_owned(), None),
                Some(previous) => zstd_adaptive(
                    &source,
                    &["--patch-from", &previous.to_string_lossy()],
                ),
            };
            baseline_row = json!({
                "full_zstd19_bytes": full_zstd19,
                "full_level": full_level,
                "full_note": full_note,
                "xor_delta_zstd19_bytes": xor_delta,
                "xor_delta_level": xor_level,
                "xor_delta_note": xor_note,
                "patch_from_zstd19_bytes": patch_from,
                "patch_from_level": patch_level,
                "patch_from_note": patch_note,
                "final_plus_xor_deltas_bytes": full_zstd19.saturating_add(xor_delta),
                "oldest_full_plus_patch_from_bytes": full_zstd19
                    .saturating_add(patch_from.unwrap_or(0)),
            });
        }

        rows.push(json!({
            "step": index,
            "name": name,
            "source": source.display().to_string(),
            "source_bytes": report.source_file_bytes,
            "source_sha256": hex::encode(source_sha),
            "write_ms": write_ms,
            "tensors": report.tensors.len(),
            "shard_objects": report.shard_objects,
            "marginal_physical_bytes": after.saturating_sub(before),
            "cumulative_physical_bytes": after,
            "index_bytes": dir_bytes(&store_path.join("indexes")),
            "store_bytes": dir_bytes(&store_path),
            "strategies": strategies,
            "max_dependency_depth": max_depth,
            "snapshot": snapshot,
            "restore": {
                "full_ms": full_ms,
                "exact_source_file": restored.exact_source_file,
                "file_sha256": hex::encode(restored.file_sha256),
                "all_tensors_exact": all_tensors_exact,
                "per_tensor": tensor_rows,
                "single_ms": single_ms,
                "single_bytes": single_bytes.report.bytes,
                "single_root_verified": single_bytes.report.full_root_verified,
                "row_ms": row_ms,
                "row_bytes": row_bytes.report.bytes,
            },
            "baselines": baseline_row,
        }));
        previous_file = Some(source);
        previous_checkpoint = Some(name.to_string());
        // Write the partial receipt after every step so a later baseline
        // failure cannot discard the trajectory evidence.
        let partial = json!({
            "phase": "series",
            "partial": true,
            "store": store_path.display().to_string(),
            "strategy": strategy,
            "shards": shards,
            "snapshots": snapshots,
            "single_tensor": single,
            "range": region_text,
            "steps": rows,
        });
        fs::write(&out, serde_json::to_string_pretty(&partial).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    let full = store.verify(true).map_err(|e| e.to_string())?;
    let receipt = json!({
        "phase": "series",
        "store": store_path.display().to_string(),
        "strategy": strategy,
        "shards": shards,
        "snapshots": snapshots,
        "single_tensor": single,
        "range": region_text,
        "steps": rows,
        "verify_full_problems": full.problems.len(),
        "final_data_bytes": data_bytes(&store_path),
    });
    fs::write(&out, serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("series wrote {}", out.display());
    Ok(())
}

/// Recursively remove one store directory's contents (ids, not paths).
fn remove_tree(store: &mut Store, id: weftfs::Hash32) -> Result<(), weftfs::Error> {
    let children = store.list_dir(id)?;
    for child in children {
        let is_dir = store
            .object_meta(child.target)
            .map(|record| record.kind == weftfs::state::ObjectKind::Dir)
            .unwrap_or(false);
        if is_dir {
            remove_tree(store, child.target)?;
            store.rmdir(id, &child.name)?;
        } else {
            store
                .unlink(id, &child.name)
                .map_err(|e| {
                    weftfs::Error::new(e.class(), format!("unlink {:?}: {}", String::from_utf8_lossy(&child.name), e.message()))
                })?;
        }
    }
    Ok(())
}

fn gc(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(arg(args, "--store").ok_or("gc needs --store")?);
    let out = PathBuf::from(arg(args, "--out").ok_or("gc needs --out")?);
    let keep = arg(args, "--keep").unwrap_or_default();
    let mut store = Store::open_with_options(
        &store_path,
        OpenMode::ReadWrite,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|e| e.to_string())?;
    // Unlink every checkpoint except `--keep`. The store has no recursive
    // delete verb, so walk each directory and unlink its files first.
    let checkpoints = store.list_checkpoints().map_err(|e| e.to_string())?;
    let mut unlinked = Vec::new();
    for name in &checkpoints {
        if name == keep {
            continue;
        }
        let id = store
            .resolve_path(&format!("/checkpoints/{name}"))
            .map_err(|e| e.to_string())?;
        remove_tree(&mut store, id).map_err(|e| format!("remove {name}: {e}"))?;
        let root = store.resolve_path("/checkpoints").map_err(|e| e.to_string())?;
        store
            .rmdir(root, name.as_bytes())
            .map_err(|e| format!("rmdir {name}: {e}"))?;
        unlinked.push(name.clone());
    }
    let before = data_bytes(&store_path);
    let mark = store.gc(false).map_err(|e| e.to_string())?;
    let applied = store.gc(true).map_err(|e| e.to_string())?;
    let after = data_bytes(&store_path);
    let mut restored = Vec::new();
    let exact = store
        .restore_checkpoint(&keep, &CheckpointRestoreOptions::default(), &mut restored)
        .map(|report| report.exact_source_file)
        .unwrap_or(false);
    let report = json!({
        "phase": "gc",
        "store": store_path.display().to_string(),
        "keep": keep,
        "unlinked": unlinked,
        "snapshots_pinned": store.snapshot_list().len(),
        "bytes_before_gc": before,
        "bytes_after_gc": after,
        "retained_union_bytes": mark.retained,
        "marked_manifests": mark.marked_manifests,
        "marked_chunks": mark.marked_chunks,
        "deleted_blobs": applied.deleted.len(),
        "deleted_indexes": applied.deleted_indexes.len(),
        "deleted_bytes": applied.deleted_bytes,
        "reclaimed_packs": applied.reclaimed_packs,
        "problems": applied.problems,
        "final_restore_exact": exact,
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("gc wrote {}", out.display());
    Ok(())
}

fn assemble(args: &[String]) -> Result<(), String> {
    let work = PathBuf::from(arg(args, "--work").ok_or("assemble needs --work")?);
    let out = PathBuf::from(arg(args, "--out").ok_or("assemble needs --out")?);
    let mut phases: BTreeMap<String, Json> = BTreeMap::new();
    let mut entries: Vec<_> = fs::read_dir(&work)
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            continue;
        }
        let value: Json = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("assemble: skipping {} ({err})", path.display());
                continue;
            }
        };
        let key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("phase")
            .to_owned();
        phases.insert(key, value);
    }
    let receipt = json!({
        "schema": "weft.trajectory.court.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_default(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
        },
        "revisions": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "phases": phases,
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("assembled {}", out.display());
    Ok(())
}
