//! Quota and branching benchmark (P5a).
//!
//! Measures three things against the real store API:
//!
//! A. per-commit quota admission overhead: 1,000 single-object commits with
//!    and without a configured budget, on otherwise identical stores;
//! B. snapshot-branch creation cost (commit + pointer) over 50 branches;
//! C. union-merge cost for 1,000 disjoint paths.
//!
//! Run only inside the OOM-protected bench court:
//!   scripts/docker-court.sh bench "bash bench/scripts/run_quota_branch_bench.sh"
//!
//! No claim of general performance is made from one host and one court.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};
use weftfs::quota::{QuotaBudget, QuotaPrincipal};
use weftfs::store::{OpenMode, Options, Store};

const FIXED_TIMESTAMP_NS: i64 = 1_767_225_600_000_000_000;
const COMMITS: usize = 1_000;
const BRANCHES: usize = 50;
const MERGE_PATHS: usize = 1_000;

fn options(chunk_size: u64, quotas: Vec<QuotaBudget>) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        // No checkpoint work in the timed phase; a checkpoint is a separate,
        // much larger write and would dominate 1,000 small commits.
        checkpoint_interval: 1_000_000,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        quotas,
        ..Options::default()
    }
}

fn payload(index: usize) -> Vec<u8> {
    let mut state = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = format!("quota-branch-bench {index:06}\n").into_bytes();
    while out.len() < 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xFF) as u8);
    }
    out.truncate(1024);
    out
}

fn percentile(samples: &[u64], percent: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = ((sorted.len() - 1) * percent + 99) / 100;
    sorted[rank.min(sorted.len() - 1)]
}

fn vm_hwm_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            return Some(rest.trim_start_matches(':').trim().to_owned());
        }
    }
    None
}

/// A. 1,000 single-object commits, with or without a budget.
fn quota_commit_overhead(dir: &Path, with_quota: bool) -> Value {
    let root = dir.join(if with_quota {
        "with-quota"
    } else {
        "without-quota"
    });
    let quotas = if with_quota {
        vec![QuotaBudget {
            principal: QuotaPrincipal::Posix { uid: 0, gid: 0 },
            max_new_bytes: u64::MAX,
            max_objects: u64::MAX,
        }]
    } else {
        Vec::new()
    };
    let mut store = Store::init(&root, options(4096, quotas)).expect("init");
    let root_id = store.resolve_path("/").expect("root");
    let mut samples = Vec::with_capacity(COMMITS);
    let started = Instant::now();
    for index in 0..COMMITS {
        let begin = Instant::now();
        store
            .create_file(
                root_id,
                format!("f{index:06}").as_bytes(),
                &payload(index),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("create file");
        samples.push(u64::try_from(begin.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let wall_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let usage = store.quota_usage(&QuotaPrincipal::Posix { uid: 0, gid: 0 });
    let generations = store.generation();
    let verify = store.verify(false).expect("verify");
    let bytes = directory_bytes(&root);
    drop(store);
    json!({
        "quota": with_quota,
        "commits": COMMITS,
        "generation": generations,
        "wall_ns": wall_ns,
        "per_commit_mean_ns": wall_ns / COMMITS as u64,
        "per_commit_p50_ns": percentile(&samples, 50),
        "per_commit_p95_ns": percentile(&samples, 95),
        "per_commit_max_ns": samples.iter().copied().max().unwrap_or(0),
        "store_bytes": bytes,
        "quota_used_bytes": usage.used_bytes,
        "quota_used_objects": usage.used_objects,
        "verify_ok": verify.ok,
    })
}

/// B. Snapshot-branch creation cost.
fn branch_creation(dir: &Path) -> Value {
    let root = dir.join("branches");
    let mut store = Store::init(&root, options(4096, Vec::new())).expect("init");
    let root_id = store.resolve_path("/").expect("root");
    for index in 0..32usize {
        store
            .create_file(
                root_id,
                format!("base{index:03}").as_bytes(),
                &payload(index),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("base file");
    }
    store.snapshot_create("source").expect("snapshot");
    let mut samples = Vec::with_capacity(BRANCHES);
    let started = Instant::now();
    for index in 0..BRANCHES {
        let begin = Instant::now();
        store
            .snapshot_branch("source", &format!("branch-{index:03}"))
            .expect("branch");
        samples.push(u64::try_from(begin.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let wall_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let refs = store.snapshot_list().len();
    drop(store);
    json!({
        "branches": BRANCHES,
        "snapshots_after": refs,
        "wall_ns": wall_ns,
        "mean_ns": wall_ns / BRANCHES as u64,
        "p50_ns": percentile(&samples, 50),
        "p95_ns": percentile(&samples, 95),
        "max_ns": samples.iter().copied().max().unwrap_or(0),
    })
}

/// C. Union merge of 1,000 disjoint paths.
fn merge_paths(dir: &Path) -> Value {
    let root = dir.join("merge");
    let mut store = Store::init(&root, options(4096, Vec::new())).expect("init");
    let root_id = store.resolve_path("/").expect("root");
    for index in 0..MERGE_PATHS {
        store
            .create_file(
                root_id,
                format!("left-{index:05}").as_bytes(),
                &payload(index),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("left file");
    }
    let left = store.snapshot_create("left").expect("left snapshot");
    for index in 0..MERGE_PATHS {
        store
            .create_file(
                root_id,
                format!("right-{index:05}").as_bytes(),
                &payload(index + MERGE_PATHS),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("right file");
    }
    let right = store.snapshot_create("right").expect("right snapshot");
    // Remove the right-only paths from the live head so the merge must add them.
    for index in 0..MERGE_PATHS {
        store
            .unlink(root_id, format!("right-{index:05}").as_bytes())
            .expect("unlink");
    }
    let generation_before = store.generation();
    let begin = Instant::now();
    let report = store.merge_branches("left", "right").expect("merge");
    let merge_ns = u64::try_from(begin.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let verify = store.verify(true).expect("verify");
    let merged_path = store.resolve_path("/right-00042").expect("merged path");
    let merged_len = store.read_object(merged_path).expect("read merged").len();
    drop(store);
    json!({
        "merge_paths": MERGE_PATHS,
        "merge_ns": merge_ns,
        "left": left.to_hex(),
        "right": right.to_hex(),
        "head": report.head.to_hex(),
        "added_objects": report.added_objects,
        "added_entries": report.added_entries,
        "generation_before": generation_before,
        "generation_after": report.generation,
        "verify_full_ok": verify.ok,
        "merged_sample_bytes": merged_len,
    })
}

fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Ok(metadata) = entry.metadata() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut work: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--work-dir" => work = args.next().map(PathBuf::from),
            other => panic!("unknown argument `{other}`"),
        }
    }
    let work_dir = work.unwrap_or_else(|| {
        PathBuf::from(env::var("WEFT_BENCH_DIR").unwrap_or_else(|_| "/tmp".to_owned()))
            .join("quota-branch-bench")
    });
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).expect("clear work dir");
    }
    fs::create_dir_all(&work_dir).expect("create work dir");
    let started = Instant::now();

    eprintln!("bench_quota_branch: A quota overhead");
    let without = quota_commit_overhead(&work_dir, false);
    let with = quota_commit_overhead(&work_dir, true);
    eprintln!("bench_quota_branch: B branch creation");
    let branches = branch_creation(&work_dir);
    eprintln!("bench_quota_branch: C merge");
    let merge = merge_paths(&work_dir);

    let overhead_ns = with["per_commit_p50_ns"].as_u64().unwrap_or(0)
        - without["per_commit_p50_ns"].as_u64().unwrap_or(0);

    let report = json!({
        "schema": "weft-quota-branch-bench-v1",
        "kind": "quota-branch-bench",
        "label": "single-host Docker bench court; synthetic deterministic fixtures; local store on the court bind mount; not a fleet claim",
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
            "nproc": std::thread::available_parallelism().ok().map(|n| n.get()),
            "revision_weftfs": env::var("WEFT_REPO_WEFTFS").ok(),
            "revision_weftcodec": env::var("WEFT_REPO_WEFTCODEC").ok(),
            "revision_weftfs_go": env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "work_dir": work_dir.display().to_string(),
        "wall_time_s": started.elapsed().as_secs_f64(),
        "vm_hwm_kib": vm_hwm_kib(),
        "quota_commit_overhead": {
            "without_quota": without,
            "with_quota": with,
            "p50_delta_ns": overhead_ns,
            "p50_ratio": if without["per_commit_p50_ns"].as_u64().unwrap_or(0) > 0 {
                Value::from(
                    with["per_commit_p50_ns"].as_f64().unwrap_or(0.0)
                        / without["per_commit_p50_ns"].as_f64().unwrap_or(1.0),
                )
            } else {
                Value::Null
            },
        },
        "branch_creation": branches,
        "merge": merge,
        "notes": [
            "A: 1,000 single-object commits, 1 KiB distinct payload each, chunk_size 4096, checkpoint_interval disabled. The quota run also rewrites the quota record after every commit (tmp+fsync+rename+dir fsync), which is the honest cost of durable, restart-safe usage.",
            "B: each branch is one commit plus one snapshot pointer write, over a 32-object store.",
            "C: 1,000 left-only paths, 1,000 right-only paths, one union merge that adds the right-only 1,000 objects and entries; full verify runs before the merge numbers are reported.",
            "Single host, warm caches, one repetition. These are court measurements, not a performance guarantee."
        ],
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize");
    match out {
        Some(path) => {
            fs::write(&path, text).expect("write report");
            eprintln!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }
}
