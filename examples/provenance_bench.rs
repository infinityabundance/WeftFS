//! Provenance recovery benchmark: synthetic agent trace with injected and
//! measured costs.
//!
//! Workload: 10 operations x 10 artifacts x 1 MiB (100 MiB logical), every
//! fifth artifact compressible so the `codec-auto` writer produces a
//! frame-backed manifest for it (decode candidate) and a raw chunked manifest
//! for the rest (fetch candidate). One full operation (10 artifacts) is
//! unlinked and collected, and the operation's receipt is retained by the
//! harness to model an external agent trace index.
//!
//! Three deterministic plans recover every artifact:
//!
//! * `injected`  — fetch 10 ms/MiB, decode 30 ms, recompute 100 ms;
//! * `measured`  — the same, but fetch/decode costs are real warm-cache
//!   medians measured against this store;
//! * `recompute_forced` — recompute for every artifact (the upper bound).
//!
//! Execution is timed for each plan and physical bytes read are taken from the
//! backend counters. Recompute executes the real input verification reads and
//! the real output digest; the operation body itself is simulated from the
//! harness's copy of the artefact bytes, which is labelled honestly in the
//! receipt. Run only inside the OOM-protected bench court.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::wcb::Value;

use weftfs::format::RepresentationKind;
use weftfs::identity::{self, Hash32};
use weftfs::provenance::{
    choose_recovery, Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal,
    ProvenanceRecord, RecoveryCandidate, RecoveryCosts, RecoveryStrategy,
};
use weftfs::recovery::read_manifest;
use weftfs::store::{Options, RepresentationMode, Store};

const OPERATIONS: usize = 10;
const PER_OPERATION: usize = 10;
const ARTIFACTS: usize = OPERATIONS * PER_OPERATION;
const ARTIFACT_BYTES: usize = 1024 * 1024;
const COMPRESSIBLE_EVERY: usize = 5;
const REMOVED_OPERATION: usize = OPERATIONS - 1;
const MIB: u64 = 1024 * 1024;
const FETCH_NS_PER_MIB: u64 = 10_000_000;
const DECODE_NS: u64 = 30_000_000;
const RECOMPUTE_NS: u64 = 100_000_000;
const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;

const OPERATION_NAMES: [&str; OPERATIONS] = [
    "fetch",
    "tokenize",
    "embed",
    "train-step",
    "eval",
    "compile",
    "index",
    "project",
    "snapshot",
    "publish",
];

struct Artifact {
    index: usize,
    operation: usize,
    name: String,
    id: Hash32,
    content: Hash32,
    content_id: Hash32,
    size: u64,
    bytes: Vec<u8>,
    fetch_capable: bool,
    decode_capable: bool,
}

struct Operation {
    provenance: Hash32,
    record: ProvenanceRecord,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = value_of(&args, "--out").unwrap_or_else(|| "provenance-bench.json".to_owned());
    if let Err(err) = run(Path::new(&out)) {
        eprintln!("provenance_bench: {err}");
        std::process::exit(1);
    }
}

fn run(out: &Path) -> Result<(), String> {
    let dir = PathBuf::from(format!("/tmp/weft-provenance-bench-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        representation: RepresentationMode::CodecAuto,
        codec_min_bytes: 4_096,
        ..Options::default()
    };
    let mut store = Store::init(&dir, options).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;

    let mut artifacts = Vec::with_capacity(ARTIFACTS);
    for operation in 0..OPERATIONS {
        for slot in 0..PER_OPERATION {
            let index = operation * PER_OPERATION + slot;
            let name = format!("op{operation}-art{slot}");
            let bytes = artifact_bytes(index);
            let id = store
                .create_file_seeded(
                    root,
                    name.as_bytes(),
                    &bytes,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("provenance-bench-{index}").as_bytes(),
                )
                .map_err(|err| err.to_string())?;
            let record = store.object_meta(id).map_err(|err| err.to_string())?;
            let manifest = read_manifest(&dir, record.content).map_err(|err| err.to_string())?;
            let kind = manifest
                .representation
                .as_ref()
                .map(|rep| rep.kind)
                .unwrap_or(RepresentationKind::Raw);
            artifacts.push(Artifact {
                index,
                operation,
                name,
                id,
                content: record.content,
                content_id: manifest.content_hash,
                size: record.size,
                bytes,
                fetch_capable: kind == RepresentationKind::Raw,
                decode_capable: kind != RepresentationKind::Raw,
            });
        }
    }

    let mut operations = Vec::with_capacity(OPERATIONS);
    for operation in 0..OPERATIONS {
        let op_artifacts: Vec<&Artifact> = artifacts
            .iter()
            .filter(|artifact| artifact.operation == operation)
            .collect();
        let inputs = if operation == 0 {
            Vec::new()
        } else {
            artifacts
                .iter()
                .filter(|artifact| artifact.operation + 1 == operation)
                .enumerate()
                .map(|(slot, artifact)| {
                    ProvenanceInput::new(
                        format!("input-{slot}"),
                        artifact.content,
                        Some(artifact.id),
                    )
                })
                .collect()
        };
        let mut outputs: Vec<Hash32> = op_artifacts.iter().map(|a| a.content).collect();
        outputs.sort_unstable();
        let mut params = BTreeMap::new();
        params.insert("step".to_owned(), Value::U64(operation as u64));
        let environment = ProvenanceEnvironment::new("synthetic-agent", "1.0")
            .with_field("host", Value::Str("bench-court".to_owned()))
            .map_err(|err| err.to_string())?;
        let record = ProvenanceRecord::new(
            OPERATION_NAMES[operation],
            params,
            inputs,
            outputs,
            environment,
            Determinism::Exact,
            ProvenancePrincipal::new(0, 0),
            FIXED_TIMESTAMP_NS + operation as i64,
        )
        .map_err(|err| err.to_string())?;
        let ids: Vec<Hash32> = op_artifacts.iter().map(|artifact| artifact.id).collect();
        let provenance = store
            .record_provenance(&ids, &record)
            .map_err(|err| err.to_string())?;
        operations.push(Operation { provenance, record });
    }

    // Remove one complete operation. Its outputs are unlinked (which also drops
    // the provenance relations naming them), then GC reclaims the receipt, the
    // manifests, and the chunks. The record survives only in the harness, as an
    // external trace index would.
    let removed: Vec<&Artifact> = artifacts
        .iter()
        .filter(|artifact| artifact.operation == REMOVED_OPERATION)
        .collect();
    for artifact in &removed {
        store
            .unlink(root, artifact.name.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let gc = store.gc(true).map_err(|err| err.to_string())?;
    if !gc.problems.is_empty() {
        return Err(format!("gc reported problems: {:?}", gc.problems));
    }
    let removed_record = operations[REMOVED_OPERATION].record.clone();
    let removed_provenance = operations[REMOVED_OPERATION].provenance;
    let removed_content_ids: Vec<String> = removed.iter().map(|a| a.content_id.to_hex()).collect();

    // Real warm-cache fetch/decode measurements, three samples, median.
    let mut measured_fetch = BTreeMap::new();
    let mut measured_decode = BTreeMap::new();
    for artifact in artifacts.iter().filter(|a| a.operation < REMOVED_OPERATION) {
        if artifact.fetch_capable {
            let samples = measure_reads(&store, artifact, 3)?;
            measured_fetch.insert(artifact.index, median(samples));
        }
        if artifact.decode_capable {
            let samples = measure_reads(&store, artifact, 3)?;
            measured_decode.insert(artifact.index, median(samples));
        }
    }

    let mut plans = Vec::new();
    for plan in ["injected", "measured", "recompute_forced"] {
        let result = run_plan(
            &store,
            &dir,
            &artifacts,
            &operations,
            &removed_record,
            removed_provenance,
            plan,
            &measured_fetch,
            &measured_decode,
        )?;
        plans.push(result);
    }

    let verify = store.verify(true).map_err(|err| err.to_string())?;

    let fetch_values: Vec<u64> = measured_fetch.values().copied().collect();
    let decode_values: Vec<u64> = measured_decode.values().copied().collect();
    let measurements = json!({
        "fetch": timing_summary(&fetch_values),
        "decode": timing_summary(&decode_values),
        "note": "per-artefact medians of three warm-cache read_content samples",
    });

    let fetch_capable = artifacts.iter().filter(|a| a.fetch_capable).count();
    let decode_capable = artifacts.iter().filter(|a| a.decode_capable).count();
    let receipt = json!({
        "schema": "weft.provenance.bench.v1",
        "label": "Synthetic agent trace, single host, Docker bench court with hard memory/CPU/pids limits and bounded /tmp. Warm-cache measurements. Not a fleet, power-loss, or production claim.",
        "environment": {
            "kernel": env("WEFT_COURT_KERNEL"),
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "court_memory": env("WEFT_COURT_MEMORY"),
            "court_cpus": env("WEFT_COURT_CPUS"),
            "court_pids": env("WEFT_COURT_PIDS"),
            "docker_client": env("WEFT_DOCKER_CLIENT"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
            "revision_weftfs": env("WEFT_REPO_WEFTFS"),
            "revision_weftcodec": env("WEFT_REPO_WEFTCODEC"),
        },
        "workload": {
            "operations": OPERATIONS,
            "artifacts_per_operation": PER_OPERATION,
            "artifacts": ARTIFACTS,
            "artifact_bytes": ARTIFACT_BYTES,
            "logical_bytes": ARTIFACTS as u64 * ARTIFACT_BYTES as u64,
            "compressible_every": COMPRESSIBLE_EVERY,
            "fetch_capable_artifacts": fetch_capable,
            "decode_capable_artifacts": decode_capable,
            "removed_operation": REMOVED_OPERATION,
            "removed_artifacts": removed.len(),
            "removed_provenance": removed_provenance.to_hex(),
            "removed_content_ids": removed_content_ids,
            "provenance_records": OPERATIONS,
            "chunk_size": 65_536,
            "representation": "codec-auto",
            "injected_costs_ns": {
                "fetch_per_mib": FETCH_NS_PER_MIB,
                "decode": DECODE_NS,
                "recompute": RECOMPUTE_NS,
            },
            "gc": {
                "deleted_blobs": gc.deleted.len(),
                "deleted_bytes": gc.deleted_bytes,
            },
        },
        "verification": {
            "verify_full_ok": verify.ok,
            "checked_objects": verify.checked_objects,
            "checked_manifests": verify.checked_manifests,
            "checked_provenance": verify.checked_provenance,
            "problems": verify.problems.iter().map(|p| json!({"class": p.class, "detail": p.detail})).collect::<Vec<_>>(),
        },
        "measurements": measurements,
        "plans": plans,
    });

    fs::write(
        out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("provenance bench receipt: {}", out.display());
    for plan in receipt["plans"].as_array().unwrap() {
        println!(
            "plan={} wall_ms={} declared_cost_ms={} physical_bytes={} strategies={}",
            plan["name"].as_str().unwrap_or("?"),
            plan["wall_ns"].as_u64().unwrap_or(0) / 1_000_000,
            plan["declared_cost_ns"].as_u64().unwrap_or(0) / 1_000_000,
            plan["physical_bytes_read"].as_u64().unwrap_or(0),
            plan["counts"],
        );
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_plan(
    store: &Store,
    dir: &Path,
    artifacts: &[Artifact],
    operations: &[Operation],
    removed_record: &ProvenanceRecord,
    removed_provenance: Hash32,
    plan: &str,
    measured_fetch: &BTreeMap<usize, u64>,
    measured_decode: &BTreeMap<usize, u64>,
) -> Result<Json, String> {
    // First build every plan so planning time is not mixed into recovery time.
    let mut chosen = Vec::with_capacity(artifacts.len());
    let mut declared_cost_ns: u64 = 0;
    for artifact in artifacts {
        let candidates = if artifact.operation == REMOVED_OPERATION {
            removed_candidates(
                store,
                dir,
                artifact,
                removed_record,
                removed_provenance,
                plan,
            )?
        } else {
            let costs = match plan {
                "injected" => RecoveryCosts {
                    fetch: artifact.size.saturating_mul(FETCH_NS_PER_MIB) / MIB,
                    decode: DECODE_NS,
                    recompute: RECOMPUTE_NS,
                },
                "measured" => RecoveryCosts {
                    fetch: measured_fetch.get(&artifact.index).copied().unwrap_or(0),
                    decode: measured_decode.get(&artifact.index).copied().unwrap_or(0),
                    recompute: RECOMPUTE_NS,
                },
                "recompute_forced" => RecoveryCosts {
                    fetch: 0,
                    decode: 0,
                    recompute: RECOMPUTE_NS,
                },
                other => return Err(format!("unknown plan `{other}`")),
            };
            let mut candidates = store
                .recovery_candidates(artifact.id, costs)
                .map_err(|err| err.to_string())?;
            if plan == "recompute_forced" {
                candidates.retain(|candidate| candidate.strategy == RecoveryStrategy::Recompute);
            }
            candidates
        };
        let choice = choose_recovery(artifact.content, &candidates)
            .map_err(|err| format!("{}: {err}", artifact.name))?;
        declared_cost_ns = declared_cost_ns.saturating_add(choice.cost);
        chosen.push((artifact, choice));
    }

    let mut wall_ns: u64 = 0;
    let mut physical_bytes_read: u64 = 0;
    let mut logical_bytes: u64 = 0;
    let mut per_strategy: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
    for (artifact, choice) in &chosen {
        let before = store.backend_stats();
        let start = Instant::now();
        let read = match choice.strategy {
            RecoveryStrategy::Fetch | RecoveryStrategy::Decode => store
                .read_content(artifact.content, 0, artifact.size)
                .map_err(|err| format!("{}: {err}", artifact.name))?,
            RecoveryStrategy::Recompute => {
                let record = if artifact.operation == REMOVED_OPERATION {
                    removed_record
                } else {
                    &operations[artifact.operation].record
                };
                for input in &record.inputs {
                    let manifest = read_manifest(dir, input.content)
                        .map_err(|err| format!("{}: {err}", artifact.name))?;
                    store
                        .read_content(input.content, 0, manifest.size)
                        .map_err(|err| format!("{}: {err}", artifact.name))?;
                }
                artifact.bytes.clone()
            }
        };
        let elapsed = start.elapsed().as_nanos();
        let after = store.backend_stats();
        if read.len() as u64 != artifact.size {
            return Err(format!("{} recovered {} bytes", artifact.name, read.len()));
        }
        if identity::content_hash(&read) != artifact.content_id {
            return Err(format!("{} recovered different content", artifact.name));
        }
        let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
        let physical = after.bytes_read.saturating_sub(before.bytes_read);
        wall_ns = wall_ns.saturating_add(elapsed);
        logical_bytes = logical_bytes.saturating_add(artifact.size);
        physical_bytes_read = physical_bytes_read.saturating_add(physical);
        let entry = per_strategy
            .entry(choice.strategy.as_str().to_owned())
            .or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(elapsed);
        entry.2 = entry.2.saturating_add(artifact.size);
        entry.3 = entry.3.saturating_add(physical);
    }

    let mut counts = serde_json::Map::new();
    let mut strategy_json = serde_json::Map::new();
    for (name, (count, elapsed, logical, physical)) in &per_strategy {
        counts.insert(name.clone(), json!(count));
        strategy_json.insert(
            name.clone(),
            json!({
                "count": count,
                "wall_ns": elapsed,
                "mean_wall_ns": elapsed / (*count).max(1),
                "logical_bytes": logical,
                "physical_bytes_read": physical,
            }),
        );
    }
    Ok(json!({
        "name": plan,
        "artifacts": chosen.len(),
        "wall_ns": wall_ns,
        "declared_cost_ns": declared_cost_ns,
        "logical_bytes": logical_bytes,
        "physical_bytes_read": physical_bytes_read,
        "counts": counts,
        "per_strategy": strategy_json,
        "plans": chosen.iter().map(|(artifact, choice)| json!({
            "artifact": artifact.name,
            "operation": artifact.operation,
            "strategy": choice.strategy.as_str(),
            "name": choice.name,
            "cost": choice.cost,
            "removed": artifact.operation == REMOVED_OPERATION,
        })).collect::<Vec<_>>(),
    }))
}

fn removed_candidates(
    store: &Store,
    dir: &Path,
    artifact: &Artifact,
    record: &ProvenanceRecord,
    provenance: Hash32,
    plan: &str,
) -> Result<Vec<RecoveryCandidate>, String> {
    let unavailable_fetch = RecoveryCandidate {
        strategy: RecoveryStrategy::Fetch,
        name: "fetch".to_owned(),
        available: false,
        determinism: Determinism::Unknown,
        missing_inputs: Vec::new(),
        fetch_cost: if plan == "injected" {
            artifact.size.saturating_mul(FETCH_NS_PER_MIB) / MIB
        } else {
            0
        },
        decode_cost: 0,
        recompute_cost: 0,
    };
    let unavailable_decode = RecoveryCandidate {
        strategy: RecoveryStrategy::Decode,
        name: "decode".to_owned(),
        available: false,
        determinism: Determinism::Unknown,
        missing_inputs: Vec::new(),
        fetch_cost: 0,
        decode_cost: if plan == "injected" { DECODE_NS } else { 0 },
        recompute_cost: 0,
    };
    let mut missing = Vec::new();
    for input in &record.inputs {
        let manifest = read_manifest(dir, input.content).map_err(|err| err.to_string())?;
        let available = store.read_content(input.content, 0, manifest.size).is_ok();
        if !available {
            missing.push(input.content);
        }
    }
    missing.sort_unstable();
    missing.dedup();
    Ok(vec![
        unavailable_fetch,
        unavailable_decode,
        RecoveryCandidate {
            strategy: RecoveryStrategy::Recompute,
            name: format!("recompute:{}", provenance.to_hex()),
            available: true,
            determinism: record.determinism,
            missing_inputs: missing,
            fetch_cost: 0,
            decode_cost: 0,
            recompute_cost: RECOMPUTE_NS,
        },
    ])
}

fn measure_reads(store: &Store, artifact: &Artifact, samples: usize) -> Result<Vec<u64>, String> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        store
            .read_content(artifact.content, 0, artifact.size)
            .map_err(|err| err.to_string())?;
        out.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    Ok(out)
}

fn median(mut samples: Vec<u64>) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn timing_summary(values: &[u64]) -> Json {
    if values.is_empty() {
        return json!({"artifacts": 0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mean = sorted.iter().copied().sum::<u64>() / sorted.len() as u64;
    json!({
        "artifacts": sorted.len(),
        "min_ns": sorted[0],
        "median_ns": sorted[sorted.len() / 2],
        "mean_ns": mean,
        "max_ns": sorted[sorted.len() - 1],
    })
}

fn artifact_bytes(index: usize) -> Vec<u8> {
    if index % COMPRESSIBLE_EVERY == 0 {
        let line = format!(
            "agent trace artefact {index}: repeated synthetic content for compressibility.\n"
        );
        let mut out = Vec::with_capacity(ARTIFACT_BYTES);
        while out.len() + line.len() <= ARTIFACT_BYTES {
            out.extend_from_slice(line.as_bytes());
        }
        out.resize(ARTIFACT_BYTES, b' ');
        return out;
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ (index as u64 + 1);
    let mut out = Vec::with_capacity(ARTIFACT_BYTES);
    while out.len() < ARTIFACT_BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(ARTIFACT_BYTES);
    out
}
