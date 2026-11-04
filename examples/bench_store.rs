//! Store/context benchmark against the real WeftFS store API.
//!
//! Builds deterministic stores at 100, 1,000, and 10,000 small objects with
//! 1–3 relations each and three snapshots, then measures ingest, replay,
//! selector resolution, one-hop relation queries, relation churn, snapshot
//! creation, full verification, GC, and on-disk bytes.
//!
//! No external SQLite/graph-store baseline is run in this pass: the store's
//! selector language has no drop-in external equivalent without reimplementing
//! the authorization and snapshot semantics, so a comparison would not be
//! controlled. The receipt says so explicitly; no competitive claim is made.
//!
//! Usage: bench_store [--sizes 100,1000,10000] [--out PATH] [--work-dir DIR]
//! Run through scripts/docker-court.sh bench.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::{json, Value};
use weftcodec::selector;
use weftcodec::wcb::Value as WcbValue;
use weftcodec::Limits;
use weftfs::auth::Principal;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

const FIXED_TIMESTAMP_NS: i64 = 1_767_225_600_000_000_000; // 2026-01-01T00:00:00Z
const CHECKPOINT_INTERVAL: u64 = 4_096;

static GARBAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Deterministic 32–256 byte payload, unique per object.
fn payload(index: usize) -> Vec<u8> {
    let len = 32 + (mix(index as u64) as usize % 225);
    let mut out = format!(
        "weft-bench-object {:06} {:016x}\n",
        index,
        mix(index as u64 ^ 0xA5A5_5A5A_1234_5678)
    )
    .into_bytes();
    let mut state = mix(index as u64 + 1) | 1;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xFF) as u8);
    }
    out.truncate(len);
    out
}

fn timed<T, F: FnOnce() -> T>(op: F) -> (u128, T) {
    let start = Instant::now();
    let value = op();
    (start.elapsed().as_nanos(), value)
}

fn vm_hwm_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().trim_end_matches(" kB").parse().ok();
        }
    }
    None
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            return rest
                .split_once(':')
                .map(|(_, value)| value.trim().to_owned());
        }
    }
    None
}

fn mem_total_kib() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest.trim().trim_end_matches(" kB").parse().ok();
        }
    }
    None
}

fn walk_dir(root: &Path, current: &Path, categories: &mut BTreeMap<String, (u64, u64)>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk_dir(root, &path, categories);
        } else if meta.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let category = relative
                .components()
                .next()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .unwrap_or_else(|| "other".to_owned());
            let slot = categories.entry(category).or_insert((0, 0));
            slot.0 += meta.len();
            slot.1 += 1;
        }
    }
}

fn disk_report(root: &Path) -> Value {
    let mut categories = BTreeMap::new();
    walk_dir(root, root, &mut categories);
    let total_bytes: u64 = categories.values().map(|(bytes, _)| *bytes).sum();
    let total_files: u64 = categories.values().map(|(_, files)| *files).sum();
    let categories_json: BTreeMap<String, Value> = categories
        .into_iter()
        .map(|(name, (bytes, files))| (name, json!({ "bytes": bytes, "files": files })))
        .collect();
    json!({
        "total_bytes": total_bytes,
        "total_files": total_files,
        "categories": categories_json,
    })
}

fn verify_json(report: &weftfs::VerifyReport) -> Value {
    json!({
        "ok": report.ok,
        "full": report.full,
        "generation": report.generation,
        "commit": report.commit.map(|hash| hash.to_hex()),
        "checked_commits": report.checked_commits,
        "checked_objects": report.checked_objects,
        "checked_manifests": report.checked_manifests,
        "checked_chunks": report.checked_chunks,
        "rejected_generations": report.rejected_generations,
        "problem_count": report.problems.len(),
        "problems": report.problems.iter().map(|problem| json!({
            "class": problem.class,
            "detail": problem.detail,
        })).collect::<Vec<_>>(),
    })
}

fn gc_json(report: &weftfs::GcReport) -> Value {
    json!({
        "applied": report.applied,
        "marked_manifests": report.marked_manifests,
        "marked_chunks": report.marked_chunks,
        "candidate_count": report.candidates.len(),
        "deleted_count": report.deleted.len(),
        "deleted_bytes": report.deleted_bytes,
        "removed_orphans": report.removed_orphans.len(),
        "retained": report.retained,
        "problem_count": report.problems.len(),
        "problems": report.problems,
    })
}

fn run_size(size: usize, work_dir: &Path, out: &mut Vec<Value>) -> Result<(), String> {
    let path = work_dir.join(format!("store-{size}"));
    if path.exists() {
        fs::remove_dir_all(&path).map_err(|err| format!("remove {}: {err}", path.display()))?;
    }
    fs::create_dir_all(&path).map_err(|err| format!("create {}: {err}", path.display()))?;

    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: CHECKPOINT_INTERVAL,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
    };
    let ingest_start = Instant::now();
    let (init_ns, mut store) = timed(|| Store::init(&path, options.clone())).pipe_result()?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;

    let mut create_ns: Vec<u128> = Vec::with_capacity(size);
    let mut relation_ns: Vec<u128> = Vec::new();
    let mut snapshot_ns: Vec<u128> = Vec::new();
    let mut objects: Vec<weftfs::Hash32> = Vec::with_capacity(size);

    for index in 0..size {
        let name = format!("obj-{index:06}.dat");
        let seed = format!("bench-{size}-{index}");
        let bytes = payload(index);
        let (ns, id) = timed(|| {
            store.create_file_seeded(
                root,
                name.as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
                seed.as_bytes(),
            )
        });
        create_ns.push(ns);
        objects.push(id.map_err(|err| err.to_string())?);
    }

    let objects_only_generation = store.generation();
    let (ns, snap) = timed(|| store.snapshot_create("objects-only"));
    snapshot_ns.push(ns);
    snap.map_err(|err| err.to_string())?;
    let objects_only_snapshot = store
        .snapshot_list()
        .iter()
        .find(|(name, _)| name == "objects-only")
        .map(|(_, hash)| hash.to_hex());

    let mut relation_count = 0u64;
    let mut tagged_even = 0u64;
    let mut half_snapshot: Option<String> = None;
    for index in 0..size {
        let subject = objects[index];
        let target = objects[(index + 1) % size];

        let (ns, result) = timed(|| {
            store.relation_add(
                subject,
                "refers_to",
                RelationTarget::Object(target),
                None,
                BTreeMap::new(),
            )
        });
        relation_ns.push(ns);
        result.map_err(|err| err.to_string())?;
        relation_count += 1;

        if index % 5 != 0 {
            let label = if index % 2 == 0 { "even" } else { "odd" };
            let (ns, result) = timed(|| {
                store.relation_add(
                    subject,
                    "tagged_as",
                    RelationTarget::Value(WcbValue::Str(label.to_owned())),
                    None,
                    BTreeMap::new(),
                )
            });
            relation_ns.push(ns);
            result.map_err(|err| err.to_string())?;
            relation_count += 1;
            if label == "even" {
                tagged_even += 1;
            }
        }

        if index % 3 == 0 && index % 5 != 0 {
            let (ns, result) = timed(|| {
                store.relation_add(
                    subject,
                    "owner",
                    RelationTarget::Value(WcbValue::Str("team-3".to_owned())),
                    None,
                    BTreeMap::new(),
                )
            });
            relation_ns.push(ns);
            result.map_err(|err| err.to_string())?;
            relation_count += 1;
        }

        if index == size / 2 {
            let (ns, result) = timed(|| store.snapshot_create("half"));
            snapshot_ns.push(ns);
            result.map_err(|err| err.to_string())?;
            half_snapshot = Some(
                store
                    .snapshot_list()
                    .iter()
                    .find(|(name, _)| name == "half")
                    .map(|(_, hash)| hash.to_hex())
                    .unwrap_or_default(),
            );
        }
    }

    let (ns, result) = timed(|| store.snapshot_create("final"));
    snapshot_ns.push(ns);
    result.map_err(|err| err.to_string())?;
    let final_snapshot = store
        .snapshot_list()
        .iter()
        .find(|(name, _)| name == "final")
        .map(|(_, hash)| hash.to_hex())
        .unwrap_or_default();
    let objects_only_snapshot = objects_only_snapshot.unwrap_or_default();

    let ingest_wall_ns = ingest_start.elapsed().as_nanos();
    let generation_after_ingest = store.generation();

    // Relation add/delete commits against an existing object.
    let churn_rounds = if size >= 10_000 { 10 } else { 25 };
    let marker = objects[0];
    let mut churn_add_ns = Vec::with_capacity(churn_rounds);
    let mut churn_delete_ns = Vec::with_capacity(churn_rounds);
    for round in 0..churn_rounds {
        let predicate = format!("bench_churn_{round}");
        let (ns, result) = timed(|| {
            store.relation_add(
                marker,
                predicate.clone(),
                RelationTarget::Value(WcbValue::Str(format!("churn-{round}"))),
                None,
                BTreeMap::new(),
            )
        });
        churn_add_ns.push(ns);
        let relation_id = result.map_err(|err| err.to_string())?;
        let (ns, result) = timed(|| store.relation_delete(relation_id));
        churn_delete_ns.push(ns);
        result.map_err(|err| err.to_string())?;
    }

    let disk_before_verify = disk_report(&path);
    drop(store);

    // Replay from the on-disk format, read-only. At 10k objects a replay is
    // ~33 s because recovery validates every object's manifest; fewer samples
    // keep the whole benchmark inside the <10 min budget. The reduction is
    // recorded in the receipt.
    let open_samples = if size >= 10_000 { 2 } else { 5 };
    let mut open_ns = Vec::with_capacity(open_samples);
    let mut read_only: Option<Store> = None;
    for _ in 0..open_samples {
        let (ns, opened) = timed(|| Store::open(&path, OpenMode::ReadOnly));
        open_ns.push(ns);
        let opened = opened.map_err(|err| err.to_string())?;
        read_only = Some(opened);
    }
    let read_only = read_only.expect("opened read-only at least once");

    let limits = Limits::default();
    let principal = Principal::root();
    let q0 = selector::parse("select tag(\"absent_tag\")").map_err(|err| err.to_string())?;
    let q1 = selector::parse(&format!(
        "select relation(refers_to, obj:{})",
        objects[0].to_hex()
    ))
    .map_err(|err| err.to_string())?;
    let qn = selector::parse("select kind == \"file\"").map_err(|err| err.to_string())?;
    let qeven = selector::parse("select tag(\"even\")").map_err(|err| err.to_string())?;
    let qhop = selector::parse(&format!(
        "select relation(refers_to, obj:{})",
        objects[size / 2].to_hex()
    ))
    .map_err(|err| err.to_string())?;

    let query_iterations = if size >= 10_000 { 10 } else { 50 };
    let measure_query = |sel: &weftcodec::selector::Selector| -> Result<(Vec<u128>, u64), String> {
        let mut samples = Vec::with_capacity(query_iterations);
        let mut matched = 0u64;
        for _ in 0..query_iterations {
            let (ns, resolution) = timed(|| read_only.resolve(sel, None, &principal, &limits));
            samples.push(ns);
            let resolution = resolution.map_err(|err| err.to_string())?;
            matched = resolution.matched_count;
        }
        Ok((samples, matched))
    };
    let (query_0_ns, query_0_matched) = measure_query(&q0)?;
    let (query_1_ns, query_1_matched) = measure_query(&q1)?;
    let (query_n_ns, query_n_matched) = measure_query(&qn)?;
    let (query_even_ns, query_even_matched) = measure_query(&qeven)?;
    let (query_hop_ns, query_hop_matched) = measure_query(&qhop)?;

    if query_0_matched != 0 {
        return Err(format!("0-result query matched {query_0_matched}"));
    }
    if query_1_matched != 1 {
        return Err(format!("1-result query matched {query_1_matched}"));
    }
    if query_n_matched != size as u64 {
        return Err(format!(
            "n-result query matched {query_n_matched}, expected {size}"
        ));
    }
    if query_hop_matched != 1 {
        return Err(format!("one-hop query matched {query_hop_matched}"));
    }
    if query_even_matched != tagged_even {
        return Err(format!(
            "even-tag query matched {query_even_matched}, expected {tagged_even}"
        ));
    }

    // Full verification while the read-only handle is open. The 10k store
    // takes ~72 s per pass, so one pass is recorded there.
    let verify_samples = if size >= 10_000 { 1 } else { 3 };
    let mut verify_ns = Vec::with_capacity(verify_samples);
    let mut verify_reports = Vec::with_capacity(verify_samples);
    for _ in 0..verify_samples {
        let (ns, report) = timed(|| read_only.verify(true));
        verify_ns.push(ns);
        let report = report.map_err(|err| err.to_string())?;
        verify_reports.push(verify_json(&report));
    }
    drop(read_only);

    // Reopen read-write for checkpoint, garbage creation, and GC.
    let (reopen_rw_ns, store) =
        timed(|| Store::open_with_options(&path, OpenMode::ReadWrite, options.clone()));
    let mut store = store.map_err(|err| err.to_string())?;

    let (checkpoint_ns, checkpoint_result) = timed(|| store.checkpoint());
    let checkpoint_result = checkpoint_result.map_err(|err| err.to_string())?;

    // Create garbage after the checkpoint so no checkpoint state roots it.
    let garbage_count = 20usize;
    let mut garbage_create_ns = Vec::with_capacity(garbage_count);
    let mut garbage_unlink_ns = Vec::with_capacity(garbage_count);
    for index in 0..garbage_count {
        let counter = GARBAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("garbage-{index:03}-{counter}");
        let bytes = payload(1_000_000 + index);
        let seed = format!("garbage-{size}-{index}");
        let (ns, result) = timed(|| {
            store.create_file_seeded(
                root,
                name.as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
                seed.as_bytes(),
            )
        });
        garbage_create_ns.push(ns);
        result.map_err(|err| err.to_string())?;
        let (ns, result) = timed(|| store.unlink(root, name.as_bytes()));
        garbage_unlink_ns.push(ns);
        result.map_err(|err| err.to_string())?;
    }

    let disk_before_gc = disk_report(&path);
    let (gc_dry_ns, dry_result) = timed(|| store.gc(false));
    let dry_result = dry_result.map_err(|err| err.to_string())?;
    let dry_json = gc_json(&dry_result);
    let (gc_apply_ns, apply_result) = timed(|| store.gc(true));
    let apply_result = apply_result.map_err(|err| err.to_string())?;
    let apply_json = gc_json(&apply_result);
    let disk_after_gc = disk_report(&path);
    let generation_final = store.generation();

    out.push(json!({
        "objects": size,
        "relations": relation_count,
        "objects_only_generation": objects_only_generation,
        "generation_after_ingest": generation_after_ingest,
        "generation_final": generation_final,
        "snapshots": {
            "names": ["objects-only", "half", "final"],
            "objects_only": objects_only_snapshot,
            "half": half_snapshot,
            "final": final_snapshot,
        },
        "options": {
            "owner_uid": options.owner_uid,
            "owner_gid": options.owner_gid,
            "chunk_size": options.chunk_size,
            "checkpoint_interval": options.checkpoint_interval,
            "gc_grace_secs": options.gc_grace_secs,
            "fixed_timestamp_ns": options.fixed_timestamp_ns,
        },
        "counts": {
            "tagged_even": tagged_even,
            "churn_rounds": churn_rounds,
            "garbage_files": garbage_count,
            "open_readonly_samples": open_samples,
            "verify_full_samples": verify_samples,
        },
        "throughput": {
            "ingest_wall_ns": ingest_wall_ns,
            "objects_per_sec": if ingest_wall_ns > 0 { (size as f64) / (ingest_wall_ns as f64 / 1e9) } else { 0.0 },
        },
        "timings": {
            "init_ns": [init_ns],
            "create_ns": create_ns,
            "relation_add_ns": relation_ns,
            "snapshot_ns": snapshot_ns,
            "churn_add_ns": churn_add_ns,
            "churn_delete_ns": churn_delete_ns,
            "open_readonly_ns": open_ns,
            "query_0_ns": query_0_ns,
            "query_1_ns": query_1_ns,
            "query_n_ns": query_n_ns,
            "query_even_ns": query_even_ns,
            "query_hop_ns": query_hop_ns,
            "verify_full_ns": verify_ns,
            "reopen_rw_ns": [reopen_rw_ns],
            "checkpoint_ns": [checkpoint_ns],
            "garbage_create_ns": garbage_create_ns,
            "garbage_unlink_ns": garbage_unlink_ns,
            "gc_dry_run_ns": [gc_dry_ns],
            "gc_apply_ns": [gc_apply_ns],
        },
        "query_matched": {
            "zero": query_0_matched,
            "one": query_1_matched,
            "n": query_n_matched,
            "even": query_even_matched,
            "one_hop": query_hop_matched,
        },
        "verify": {
            "samples": verify_reports,
            "checkpoint_commit": checkpoint_result.to_hex(),
        },
        "gc": {
            "dry_run": dry_json,
            "apply": apply_json,
        },
        "disk": {
            "before_verify": disk_before_verify,
            "before_gc": disk_before_gc,
            "after_gc": disk_after_gc,
        },
        "vm_hwm_kib_after_size": vm_hwm_kib(),
    }));
    Ok(())
}

trait PipeResult<T, E> {
    fn pipe_result(self) -> Result<(u128, T), String>;
}

impl<T, E: std::fmt::Display> PipeResult<T, E> for (u128, Result<T, E>) {
    fn pipe_result(self) -> Result<(u128, T), String> {
        let (ns, result) = self;
        let value = result.map_err(|err| err.to_string())?;
        Ok((ns, value))
    }
}

fn main() {
    let mut sizes = vec![100usize, 1_000, 10_000];
    let mut out_path: Option<String> = None;
    let mut work_dir: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sizes" => {
                let value = args.next().expect("sizes");
                sizes = value
                    .split(',')
                    .map(|part| part.trim().parse::<usize>().expect("integer size"))
                    .filter(|size| *size > 1)
                    .collect();
            }
            "--out" => out_path = Some(args.next().expect("path")),
            "--work-dir" => work_dir = Some(PathBuf::from(args.next().expect("path"))),
            other => panic!("unknown argument {other}"),
        }
    }
    let work_dir = work_dir.unwrap_or_else(|| {
        let candidate = PathBuf::from("/work/.docker-cache/bench-store");
        if Path::new("/work").exists() {
            candidate
        } else {
            env::temp_dir().join("bench-store")
        }
    });
    fs::create_dir_all(&work_dir).expect("create work dir");

    let started_at = Instant::now();
    let mut results = Vec::new();
    for size in sizes {
        eprintln!("bench_store: size {size}");
        run_size(size, &work_dir, &mut results).unwrap_or_else(|err| {
            panic!("store benchmark failed at size {size}: {err}");
        });
        eprintln!(
            "bench_store: size {size} done in {:.1}s",
            started_at.elapsed().as_secs_f64()
        );
    }

    let report = json!({
        "schema": "weft-store-bench-v1",
        "kind": "store-context-bench",
        "environment": {
            "kernel": env::var("WEFT_COURT_KERNEL").ok(),
            "court_image": env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": env::var("WEFT_COURT_IMAGE_ID").ok(),
            "court_memory": env::var("WEFT_COURT_MEMORY").ok(),
            "court_cpus": env::var("WEFT_COURT_CPUS").ok(),
            "court_pids": env::var("WEFT_COURT_PIDS").ok(),
            "docker_client": env::var("WEFT_DOCKER_CLIENT").ok(),
            "docker_server": env::var("WEFT_DOCKER_SERVER").ok(),
            "cpu_model": cpu_model(),
            "host_mem_total_kib": mem_total_kib(),
            "nproc": std::thread::available_parallelism().ok().map(|n| n.get()),
            "revision_weftfs": env::var("WEFT_REPO_WEFTFS").ok(),
            "revision_weftcodec": env::var("WEFT_REPO_WEFTCODEC").ok(),
            "revision_weftfs_go": env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "work_dir": work_dir.display().to_string(),
        "wall_time_s": started_at.elapsed().as_secs_f64(),
        "vm_hwm_kib": vm_hwm_kib(),
        "results": results,
        "notes": [
            "No external SQLite or graph-store baseline was run in this pass. The selector semantics (snapshot pinning, authorization filtering, evidence, and ambiguity errors) have no drop-in external equivalent without reimplementing the implementation under test, so any SQLite/graph comparison here would not be controlled. No competitive claim is made.",
            "Ingest throughput includes fsync-heavy commit publication. The store lives under /work/.docker-cache (host bind mount), not tmpfs.",
            "Query samples are repeated resolutions against an already-replayed read-only handle; they do not include open/replay time.",
            "GC grace is 0 so the garbage created after a checkpoint is reclaimable in the same run. This is a benchmark choice, not the production default.",
            "At 10,000 objects, open(ReadOnly) is sampled twice and full verify once (instead of five and three) because each pass costs ~33 s and ~72 s. The counts are in each result's `counts` object; the reduction is deliberate and is stated in bench/RESULTS-v1.md.",
            "Every result count is asserted (0, 1, n, even-tag, and one-hop) before the sample is recorded."
        ],
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize");
    match out_path {
        Some(path) => {
            fs::write(&path, text).expect("write report");
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
}
