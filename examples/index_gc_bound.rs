//! Index GC bound court: does one retained root's chain exhaust another root's
//! budget?
//!
//! `Store::expand_retained_index_chains` walks the WFX3 chain of every retained
//! root (the head, every snapshot, and every commit carrying a state
//! checkpoint) so that no ancestor segment a retained commit still names is
//! swept. The bound on that walk is meant to be **per retained root**. This
//! court builds a store that retains several roots, each anchored at the
//! deepest chain the writer permits, and records what GC does with it.
//!
//! Two cells:
//! * `retained`: `WEFT_GC_ROOTS` snapshots (default 12) plus one forced
//!   checkpoint commit, every snapshot taken at the writer's maximum chain
//!   depth so the chains cannot overlap. Each root alone is inside the bound;
//!   the union of all of them is what the pre-fix code measured.
//! * `head-only`: no snapshot and no checkpoint commit, i.e. the single-root
//!   case. It is the "no retention changed nothing" control: its reclaimed
//!   bytes are compared with the pre-fix receipt.
//!
//! Every reported field is derived from the store or from the segment files on
//! disk; nothing is assumed about the fix.
//!
//! Run in the Docker court (release):
//!   cargo run --release --locked --example index_gc_bound
//!
//! Receipt: `bench/receipts/index-gc-bound-<stamp>.json` (+ `.log`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use weftcodec::selector;
use weftfs::auth::Principal;
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, Store};

/// Writer default (`index_chain::DEFAULT_MAX_CHAIN_DELTAS`).
const WRITER_MAX_DELTAS: u32 = 32;
/// Hard reader bound (`index_chain::HARD_MAX_CHAIN_DELTAS`).
const HARD_MAX_DELTAS: u32 = 256;

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn env_usize(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(fallback)
}

fn env_string(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn timed<T, F: FnOnce() -> T>(action: F) -> (f64, T) {
    let start = Instant::now();
    let value = action();
    (start.elapsed().as_secs_f64() * 1000.0, value)
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn data_bytes(root: &Path) -> u64 {
    dir_bytes(&root.join("objects")) + dir_bytes(&root.join("manifests"))
}

fn index_path(root: &Path, commit: Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}

fn options(checkpoint_interval: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        // Zero automatic checkpoints in the retained cell would be wrong: the
        // point is to retain several roots. The caller picks the interval.
        checkpoint_interval,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn seed(store: &mut Store, live: usize) {
    let root = store.resolve_path("/").expect("root");
    store.begin_group().expect("group");
    for index in 0..live {
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            weftcodec::wcb::Value::U64((index % 4) as u64),
        );
        store
            .create_file_seeded(
                root,
                format!("f{index}").as_bytes(),
                format!("body-{index}").as_bytes(),
                0o644,
                0,
                0,
                attrs,
                format!("seed-{index}").as_bytes(),
            )
            .expect("create");
    }
    store.commit_group().expect("seed group");
}

fn commit_one(store: &mut Store, step: u64) {
    let root = store.resolve_path("/").expect("root");
    let live = 32usize;
    let target = store
        .lookup(root, format!("f{}", step as usize % live).as_bytes())
        .expect("lookup");
    store
        .set_xattr(target, "rev", &step.to_le_bytes())
        .expect("set_xattr");
}

/// Parse a WFX3 header far enough to follow the chain: `kind` at byte 8 and,
/// for a delta, `base_commit` at bytes 57..89.
fn chain_of(root: &Path, head: Hash32) -> (u64, Vec<Hash32>) {
    let mut chain = Vec::new();
    let mut current = head;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard <= HARD_MAX_DELTAS + 2, "chain walk did not terminate");
        let path = index_path(root, current);
        let bytes = fs::read(&path).unwrap_or_else(|err| {
            panic!("segment {} unreadable: {err}", path.display());
        });
        chain.push(current);
        if bytes.len() < 90 {
            break;
        }
        if bytes[8] != 1 {
            break;
        }
        current = Hash32::from_bytes(bytes[57..89].try_into().expect("base commit"));
    }
    ((chain.len() - 1) as u64, chain)
}

fn present(root: &Path, chain: &[Hash32]) -> bool {
    chain
        .iter()
        .all(|commit| index_path(root, *commit).exists())
}

fn resolve_against_scan(store: &Store, snapshot: Option<Hash32>) -> (u64, u64) {
    let text = "select attrs.team == 2";
    let query = selector::parse(text).expect("selector");
    let limits = weftcodec::Limits::default();
    let principal = principal();
    let indexed = store
        .resolve(&query, snapshot, &principal, &limits)
        .expect("resolve");
    let scan = store
        .resolve_scan(&query, snapshot, &principal, &limits)
        .expect("scan");
    let mut a: Vec<_> = indexed.results.iter().map(|item| item.object_id).collect();
    let mut b: Vec<_> = scan.results.iter().map(|item| item.object_id).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "indexed and scanned answers differ");
    (indexed.matched_count as u64, scan.matched_count as u64)
}

fn retained_cell(roots: usize, tail: usize) -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    // No automatic checkpoints: the only retained roots are the ones this cell
    // deliberately creates, plus the single forced checkpoint commit.
    let mut store = Store::init(&root, options(1_000_000)).expect("init");

    let (seed_ms, ()) = timed(|| seed(&mut store, 32));
    let mut steps = 0u64;
    let mut pinned: Vec<(String, Hash32)> = Vec::new();
    let mut checkpoint_commit: Option<Hash32> = None;

    for index in 0..roots {
        // Advance until the writer's chain is at its maximum depth, so this
        // root's chain is as long as the format permits and cannot overlap the
        // next root's chain.
        let mut guard = 0;
        loop {
            let (kind, depth) = store.index_head_chain().expect("head chain");
            if kind == "delta" && depth >= u64::from(WRITER_MAX_DELTAS) {
                break;
            }
            commit_one(&mut store, steps);
            steps += 1;
            guard += 1;
            assert!(guard <= 64, "the writer never reached the maximum depth");
        }
        let name = format!("s{index}");
        let commit = store.snapshot_create(&name).expect("snapshot");
        pinned.push((name, commit));
        if index == 3 {
            checkpoint_commit = Some(store.checkpoint().expect("checkpoint"));
        }
    }
    // A few commits after the last snapshot so the head chain is separate too,
    // plus `tail` commits that no root retains: those are what GC may reclaim.
    for _ in 0..tail {
        commit_one(&mut store, steps);
        steps += 1;
    }
    for _ in 0..3 {
        commit_one(&mut store, steps);
        steps += 1;
    }

    let generation = store.generation();
    let index_bytes_before = dir_bytes(&root.join("indexes"));
    let data = data_bytes(&root);

    // Independent measurement: every retained root's chain, read from disk.
    let mut union: BTreeSet<Hash32> = BTreeSet::new();
    let mut per_root = Vec::new();
    let mut all_roots: Vec<Hash32> = store.snapshot_list().into_iter().map(|(_, c)| c).collect();
    all_roots.push(store.head());
    if let Some(commit) = checkpoint_commit {
        all_roots.push(commit);
    }
    all_roots.sort();
    all_roots.dedup();
    for commit in &all_roots {
        let (depth, chain) = chain_of(&root, *commit);
        union.extend(chain.iter().copied());
        per_root.push(json!({
            "commit": commit.to_hex(),
            "depth": depth,
            "segments": chain.len(),
        }));
    }
    let segments_on_disk = count_segments(&root);
    let union_segments = union.len();

    let (gc_ms, report) = timed(|| store.gc(true).expect("gc"));
    let index_bytes_after_gc = dir_bytes(&root.join("indexes"));
    let problems: Vec<String> = report.problems.clone();
    let (verify_ms, verify) = timed(|| store.verify(true).expect("verify"));
    drop(store);

    // Post-GC: every retained root must still resolve its own chain and answer
    // exactly like the scan oracle on a fresh handle.
    let mut post = Vec::new();
    for (name, commit) in &pinned {
        let (open_ms, cold) = timed(|| Store::open(&root, OpenMode::ReadOnly).expect("reopen"));
        cold.reset_index_stats();
        let (matched, scanned) = resolve_against_scan(&cold, Some(*commit));
        let stats = cold.index_stats();
        let (depth, chain) = chain_of(&root, *commit);
        post.push(json!({
            "snapshot": name,
            "commit": commit.to_hex(),
            "open_ms": open_ms,
            "depth": depth,
            "segments_present": present(&root, &chain),
            "disk_loads": stats.disk_loads,
            "builds": stats.builds,
            "rejects": stats.rejects,
            "matched": matched,
            "scan_matched": scanned,
        }));
    }

    let refused = problems
        .iter()
        .any(|note| note.contains("index GC skipped"));
    let reclaimed = index_bytes_before.saturating_sub(index_bytes_after_gc);
    let depths: Vec<u64> = per_root
        .iter()
        .filter_map(|entry| entry["depth"].as_u64())
        .collect();
    json!({
        "retained_roots": all_roots.len(),
        "unretained_tail_commits": tail,
        "commits": generation,
        "seed_ms": seed_ms,
        "index_bytes_before_gc": index_bytes_before,
        "data_bytes": data,
        "index_ppm_of_data_before_gc": if data == 0 { 0 } else { (index_bytes_before as f64 / data as f64 * 1_000_000.0).round() as u64 },
        "segments_on_disk_before_gc": segments_on_disk,
        "union_chain_segments": union_segments,
        "per_root_depth_min": depths.iter().copied().min().unwrap_or(0),
        "per_root_depth_max": depths.iter().copied().max().unwrap_or(0),
        "hard_max_chain_deltas": HARD_MAX_DELTAS,
        "per_root": per_root,
        "gc": {
            "ms": gc_ms,
            "refused": refused,
            "problems": problems,
            "deleted_indexes": report.deleted_indexes.len(),
            "deleted_index_bytes": report.deleted_index_bytes,
            "reclaimed_bytes": reclaimed,
            "index_bytes_after_gc": index_bytes_after_gc,
            "segments_on_disk_after_gc": count_segments(&root),
        },
        "verify": {
            "ms": verify_ms,
            "ok": verify.problems.is_empty(),
            "problems": verify
                .problems
                .iter()
                .map(|problem| format!("{}: {}", problem.class, problem.detail))
                .collect::<Vec<_>>(),
            "generation": verify.generation,
            "checked_commits": verify.checked_commits,
            "checked_manifests": verify.checked_manifests,
            "checked_objects": verify.checked_objects,
        },
        "post_gc": post,
    })
}

fn head_only_cell(commits: u64) -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let mut store = Store::init(&root, options(1_000_000)).expect("init");
    seed(&mut store, 32);
    for step in 0..commits {
        commit_one(&mut store, step);
    }
    let index_bytes_before = dir_bytes(&root.join("indexes"));
    let data = data_bytes(&root);
    let segments_on_disk = count_segments(&root);
    let (depth, chain) = chain_of(&root, store.head());
    let (gc_ms, report) = timed(|| store.gc(true).expect("gc"));
    let after = dir_bytes(&root.join("indexes"));
    json!({
        "commits": store.generation(),
        "head_chain_depth": depth,
        "head_chain_segments": chain.len(),
        "segments_on_disk_before_gc": segments_on_disk,
        "index_bytes_before_gc": index_bytes_before,
        "data_bytes": data,
        "gc": {
            "ms": gc_ms,
            "refused": report.problems.iter().any(|note| note.contains("index GC skipped")),
            "problems": report.problems,
            "deleted_indexes": report.deleted_indexes.len(),
            "deleted_index_bytes": report.deleted_index_bytes,
            "index_bytes_after_gc": after,
            "segments_on_disk_after_gc": count_segments(&root),
        },
    })
}

fn count_segments(root: &Path) -> u64 {
    let mut count = 0;
    let Ok(entries) = fs::read_dir(root.join("indexes")) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(files) = fs::read_dir(&path) {
                count += files
                    .flatten()
                    .filter(|file| file.path().extension().is_some_and(|ext| ext == "idx"))
                    .count() as u64;
            }
        }
    }
    count
}

fn peak_rss_kib() -> u64 {
    let text = fs::read_to_string("/proc/self/status").unwrap_or_default();
    text.lines()
        .find_map(|line| {
            let rest = line.strip_prefix("VmHWM:")?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(0)
}

fn utc_iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

fn main() {
    let roots = env_usize("WEFT_GC_ROOTS", 12);
    let tail = env_usize("WEFT_GC_TAIL", 400);
    let head_commits: u64 = env_string("WEFT_GC_HEAD_COMMITS", "200")
        .trim()
        .parse()
        .expect("WEFT_GC_HEAD_COMMITS");

    let retained = retained_cell(roots, tail);
    let head_only = head_only_cell(head_commits);

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let stamp = std::env::var("WEFT_STAMP")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| utc_iso(generated));
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());

    let receipt = json!({
        "schema": "weftfs.index-gc-bound.v1",
        "label": "synthetic fixture, tmpfs, single host, one process; index GC bound per retained root",
        "generated_unix_secs": generated,
        "generated_utc": stamp,
        "revision": env("WEFT_REPO_WEFTFS"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "profile": "release",
        "environment": {
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "kernel": env("WEFT_COURT_KERNEL"),
            "memory_limit": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "pids": env("WEFT_COURT_PIDS"),
        },
        "method": {
            "retained": "each snapshot is pinned when the writer's head chain is at the default maximum delta depth, so each root's chain is as long as the format permits and adjacent chains do not overlap",
            "head_only": "no snapshot and no checkpoint commit; the single-root control",
            "chain_walk": "the harness re-reads the WFX3 header of every segment it names (kind at byte 8, base commit at bytes 57..89) and never calls the store's resolver",
            "gcd": "store.gc(true) with gc_grace_secs = 0",
        },
        "control_variables": {
            "WEFT_GC_ROOTS": env("WEFT_GC_ROOTS"),
            "WEFT_GC_TAIL": env("WEFT_GC_TAIL"),
            "WEFT_GC_HEAD_COMMITS": env("WEFT_GC_HEAD_COMMITS"),
        },
        "retained_cell": retained,
        "head_only_cell": head_only,
        "peak_rss_kib": peak_rss_kib(),
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("index-gc-bound-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");

    let mut log = String::new();
    log.push_str(&format!("command: {}\n", env("WEFT_GC_COMMAND")));
    for (label, cell) in [("retained", &retained), ("head-only", &head_only)] {
        let gc = &cell["gc"];
        log.push_str(&format!(
            "{label}: commits={} index_before={} index_after={} data={} segments_before={} segments_after={} deleted={} deleted_bytes={} refused={}\n",
            cell["commits"],
            cell["index_bytes_before_gc"],
            gc["index_bytes_after_gc"],
            cell["data_bytes"],
            cell["segments_on_disk_before_gc"],
            gc["segments_on_disk_after_gc"],
            gc["deleted_indexes"],
            gc["deleted_index_bytes"],
            gc["refused"],
        ));
        if let Some(problems) = gc["problems"].as_array() {
            for problem in problems {
                log.push_str(&format!(
                    "{label}: problem: {}\n",
                    problem.as_str().unwrap_or("")
                ));
            }
        }
    }
    if let Some(roots) = retained["per_root"].as_array() {
        log.push_str(&format!(
            "retained: roots={} union_segments={} hard_max={}\n",
            roots.len(),
            retained["union_chain_segments"],
            retained["hard_max_chain_deltas"],
        ));
    }
    fs::write(receipts.join(format!("index-gc-bound-{stamp}.log")), log).expect("write log");
    println!(
        "{}",
        serde_json::to_string_pretty(&receipt).expect("serialize")
    );
}
