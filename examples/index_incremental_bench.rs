//! Incremental (operation-derived) advisory index maintenance: commit cost,
//! per-commit index bytes, index footprint and cold query cost.
//!
//! One store per mode. A fixed live working set is seeded first, then a single
//! small operation (`set_xattr` on one rotating object) is committed repeatedly
//! up to the largest configured generation checkpoint. Metrics are sampled at
//! each checkpoint over the last `WEFT_INDEX_WINDOW` commits, so the *history*
//! dimension grows while the live state stays constant.
//!
//! Modes:
//! * `delta` (default): the default writer (`index_chain_max_deltas = 32`).
//! * `full`:  `incremental_index = false`, one full segment per commit.
//! * `delta-no-maint`: only exists after the incremental-maintenance change;
//!   it forces `StateIndex::build` on every publish so the same revision can
//!   reproduce the pre-change path. A revision without the switch refuses the
//!   mode rather than silently measuring something else.
//!
//! Environment:
//! * `WEFT_INDEX_SIZES`  generation checkpoints, ascending (default `1000,10000`)
//! * `WEFT_INDEX_MODE`   `delta` | `full` | `delta-no-maint` (default `delta`)
//! * `WEFT_INDEX_LIVE`   live objects held constant (default `512`)
//! * `WEFT_INDEX_WINDOW` commits in each published window (default `200`)
//! * `WEFT_INDEX_QUERY_OPENS` read-only reopens per checkpoint (default `5`)
//! * `WEFT_INDEX_GC`     `1` runs a GC at each checkpoint (default `0`)
//!
//! Run in the Docker court (release):
//!   cargo run --release --locked --example index_incremental_bench
//!
//! The receipt is written to `bench/receipts/incremental-index-<stamp>.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

const SEED_BATCH: usize = 512;
const CHECKPOINT_INTERVAL: u64 = 256;

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn timed<T, F: FnOnce() -> T>(action: F) -> (f64, T) {
    let start = Instant::now();
    let value = action();
    (start.elapsed().as_secs_f64() * 1000.0, value)
}

fn median(values: &[f64]) -> f64 {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
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

fn index_path(root: &Path, commit: weftfs::identity::Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
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

fn env_string(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn env_usize(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(fallback)
}

fn sizes() -> Vec<u64> {
    let mut values: Vec<u64> = env_string("WEFT_INDEX_SIZES", "1000,10000")
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .filter(|value: &u64| *value > 0)
        .collect();
    values.sort_unstable();
    values.dedup();
    assert!(
        !values.is_empty(),
        "WEFT_INDEX_SIZES must name one checkpoint"
    );
    values
}

fn options(mode: &str) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: CHECKPOINT_INTERVAL,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        incremental_index: mode != "full",
        incremental_index_maintenance: !maintenance_off(mode),
        ..Options::default()
    }
}

/// True when the mode forces the whole-state oracle on every publish. A
/// revision without the switch has no such mode.
fn maintenance_off(mode: &str) -> bool {
    mode == "delta-no-maint"
}

/// 512 small files in one group commit; `rev` is the attribute the measured
/// commits move.
fn seed(store: &mut Store, live: usize) {
    let root = store.resolve_path("/").expect("root");
    let teams = ["red", "green", "blue"];
    let mut done = 0;
    while done < live {
        let batch = (live - done).min(SEED_BATCH);
        let start = done;
        store
            .group(|group| {
                for index in start..start + batch {
                    let mut attrs = BTreeMap::new();
                    attrs.insert(
                        "team".to_owned(),
                        Value::Str(teams[index % teams.len()].to_owned()),
                    );
                    attrs.insert("score".to_owned(), Value::U64((index % 16) as u64));
                    let id = group.create_file_seeded(
                        root,
                        format!("f{index}").as_bytes(),
                        format!("seed-{index}").as_bytes(),
                        0o644,
                        0,
                        0,
                        attrs,
                        format!("index-incremental-seed-{index}").as_bytes(),
                    )?;
                    if index % 4 == 0 {
                        group.relation_add(
                            id,
                            "tagged_as",
                            RelationTarget::Value(Value::Str("alpha".to_owned())),
                            None,
                            BTreeMap::new(),
                        )?;
                    }
                    group.set_xattr(id, "rev", &0u64.to_le_bytes())?;
                }
                Ok(())
            })
            .expect("seed group");
        done += batch;
    }
}

fn selectors() -> Vec<(&'static str, Selector)> {
    [
        ("attr_eq", "select attrs.team == \"red\""),
        ("tag", "select tag(\"alpha\")"),
        ("has_attr", "select has(attrs.score)"),
        ("all_true", "select true"),
    ]
    .iter()
    .map(|(label, text)| (*label, selector::parse(text).expect("selector")))
    .collect()
}

/// Cold read cost: a fresh read-only handle per sample. `None` when the
/// concurrent read-only open is refused (the caller records why).
fn cold_query(
    root: &Path,
    selector: &Selector,
    principal: &Principal,
    opens: usize,
) -> serde_json::Value {
    let limits = weftcodec::Limits::default();
    let mut samples = Vec::with_capacity(opens);
    let mut loads = Vec::with_capacity(opens);
    let mut builds = Vec::with_capacity(opens);
    let mut rejects = Vec::with_capacity(opens);
    for _ in 0..opens {
        let store = match Store::open(root, OpenMode::ReadOnly) {
            Ok(store) => store,
            Err(err) => {
                return json!({"error": err.to_string()});
            }
        };
        store.reset_index_stats();
        let (ms, result) = timed(|| store.resolve(selector, None, principal, &limits));
        let result = result.expect("indexed resolution");
        samples.push(ms);
        let stats = store.index_stats();
        loads.push(stats.disk_loads);
        builds.push(stats.builds);
        rejects.push(stats.rejects);
        std::hint::black_box(result.matched_count);
    }
    json!({
        "p50_ms": median(&samples),
        "p95_ms": percentile(&samples, 0.95),
        "samples_ms": samples,
        "disk_loads": loads,
        "builds": builds,
        "rejects": rejects,
    })
}

fn phase_totals(snapshot: &weftfs::phase_profile::PhaseSnapshot) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for name in [
        "manifest_commit",
        "index_segment",
        "index_build",
        "index_load",
        "index_attest",
    ] {
        if let Some(entry) = snapshot.phases.get(name) {
            out.insert(
                name.to_owned(),
                json!({
                    "count": entry.count,
                    "total_ms": entry.total_ns as f64 / 1_000_000.0,
                    "max_ms": entry.max_ns as f64 / 1_000_000.0,
                }),
            );
        }
    }
    serde_json::Value::Object(out)
}

fn run(mode: &str, live: usize, checkpoints: &[u64]) -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let principal = principal();
    let selectors = selectors();
    let window = env_usize("WEFT_INDEX_WINDOW", 200);
    let opens = env_usize("WEFT_INDEX_QUERY_OPENS", 5);
    let gc = env_string("WEFT_INDEX_GC", "0") == "1";

    let (seed_ms, mut store) = timed(|| {
        let mut store = Store::init(&root, options(mode)).expect("init store");
        seed(&mut store, live);
        store
    });
    let ids: Vec<weftfs::identity::Hash32> = (0..live)
        .map(|index| {
            let dir_id = store.resolve_path("/").expect("root");
            store
                .lookup(dir_id, format!("f{index}").as_bytes())
                .expect("lookup")
        })
        .collect();
    assert_eq!(ids.len(), live);

    let mut commit_ms: Vec<f64> = Vec::with_capacity(window);
    let mut commit_bytes: Vec<f64> = Vec::with_capacity(window);
    let mut reports = Vec::new();
    let max = *checkpoints.iter().max().expect("checkpoint");
    let mut previous_generation = 0u64;
    let mut failure = serde_json::Value::Null;

    weftfs::phase_profile::begin();
    for step in 0..max {
        let target = ids[(step as usize) % live];
        let value = (step as u64 + 1).to_le_bytes();
        let start = Instant::now();
        let outcome = store.set_xattr(target, "rev", &value);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        if let Err(err) = outcome {
            // A failure is a published result, not a reason to lose the run.
            failure = json!({
                "generation": step as u64 + 1,
                "class": format!("{:?}", err.class()),
                "message": err.to_string(),
                "index_bytes": dir_bytes(&root.join("indexes")),
                "data_bytes": data_bytes(&root),
            });
            break;
        }
        let head = store.head();
        let index_size = fs::metadata(index_path(&root, head))
            .map(|meta| meta.len())
            .unwrap_or(0);
        let commit_size = fs::metadata(
            root.join("commits")
                .join(format!("{}.commit", store.generation())),
        )
        .map(|meta| meta.len())
        .unwrap_or(0);
        commit_ms.push(ms);
        commit_bytes.push((index_size + commit_size) as f64);
        if commit_ms.len() > window {
            commit_ms.remove(0);
            commit_bytes.remove(0);
        }
        let generation = step as u64 + 1;
        if !checkpoints.contains(&generation) {
            continue;
        }
        let snapshot = weftfs::phase_profile::snapshot();
        let index_notes = store.index_stats();
        let (kind, depth) = store.index_head_chain().expect("head chain");
        let index_bytes = dir_bytes(&root.join("indexes"));
        let data = data_bytes(&root);
        let mut cold = serde_json::Map::new();
        for (label, selector) in &selectors {
            cold.insert(
                (*label).to_owned(),
                cold_query(&root, selector, &principal, opens),
            );
        }
        let mut gc_report = serde_json::Value::Null;
        if gc {
            let (gc_ms, report) = timed(|| store.gc(true).expect("gc"));
            gc_report = json!({
                "ms": gc_ms,
                "deleted_indexes": report.deleted_indexes.len(),
                "deleted_index_bytes": report.deleted_index_bytes,
                "index_bytes_after": dir_bytes(&root.join("indexes")),
                "problems": report.problems,
            });
        }
        reports.push(json!({
            "generation": generation,
            "generations_since_previous_checkpoint": generation - previous_generation,
            "live_objects": live,
            "commit_ms": {
                "p50": median(&commit_ms),
                "p95": percentile(&commit_ms, 0.95),
                "p99": percentile(&commit_ms, 0.99),
                "mean": commit_ms.iter().sum::<f64>() / commit_ms.len() as f64,
                "window": commit_ms.len(),
            },
            "bytes_written_per_commit": {
                "p50": median(&commit_bytes),
                "p95": percentile(&commit_bytes, 0.95),
                "mean": commit_bytes.iter().sum::<f64>() / commit_bytes.len() as f64,
            },
            "index_bytes": index_bytes,
            "data_bytes": data,
            "index_ppm_of_data": if data == 0 {
                0
            } else {
                (index_bytes as f64 / data as f64 * 1_000_000.0).round() as u64
            },
            "head_segment": kind,
            "head_chain_depth": depth,
            "index_stats": {
                "loads": index_notes.disk_loads,
                "builds": index_notes.builds,
                "maintenance_updates": index_notes.maintenance_updates,
                "maintenance_fallbacks": index_notes.maintenance_fallbacks,
                "rejects": index_notes.rejects,
                "writes": index_notes.writes,
                "write_errors": index_notes.write_errors,
                "notes": index_notes.notes,
            },
            "phase_totals": phase_totals(&snapshot),
            "cold_query": cold,
            "gc": gc_report,
        }));
        previous_generation = generation;
    }
    let phases = weftfs::phase_profile::finish();
    let index_stats = store.index_stats();
    let (kind, depth) = store.index_head_chain().expect("head chain");
    let index_bytes = dir_bytes(&root.join("indexes"));
    let data = data_bytes(&root);
    drop(store);

    json!({
        "mode": mode,
        "live_objects": live,
        "seed_ms": seed_ms,
        "checkpoints": checkpoints,
        "window": window,
        "failure": failure,
        "final": {
            "generation": store_generation(&root),
            "index_bytes": index_bytes,
            "data_bytes": data,
            "head_segment": kind,
            "head_chain_depth": depth,
            "index_stats": {
                "loads": index_stats.disk_loads,
                "builds": index_stats.builds,
                "maintenance_updates": index_stats.maintenance_updates,
                "maintenance_fallbacks": index_stats.maintenance_fallbacks,
                "rejects": index_stats.rejects,
                "writes": index_stats.writes,
                "write_errors": index_stats.write_errors,
            },
            "phase_totals": phase_totals(&phases),
        },
        "samples": reports,
    })
}

fn store_generation(root: &Path) -> u64 {
    // The generation is the highest `commits/<gen>.commit` name.
    fs::read_dir(root.join("commits"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .strip_suffix(".commit")
                        .and_then(|name| name.parse::<u64>().ok())
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn main() {
    let checkpoints = sizes();
    let live = env_usize("WEFT_INDEX_LIVE", 512);
    let mode = env_string("WEFT_INDEX_MODE", "delta");
    assert!(
        ["delta", "full", "delta-no-maint"].contains(&mode.as_str()),
        "WEFT_INDEX_MODE must be delta, full, or delta-no-maint"
    );
    if maintenance_off(&mode) {
        // Recorded in the receipt as the same-revision control cell.
    }

    let report = run(&mode, live, &checkpoints);

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
        "schema": "weftfs.incremental-index-bench.v1",
        "label": "synthetic fixture, tmpfs, single host, one process, warm page cache; \
                  history grows while the live working set is constant",
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
            "seed": "live working set created in group commits; one small `set_xattr` commit per measured step",
            "window": "statistics are the last WEFT_INDEX_WINDOW commits ending at the checkpoint",
            "cold_query": "one fresh read-only handle per sample; the first indexed resolve per handle",
            "phase_totals": "cumulative phase-profile totals since the first measured commit",
            "bytes_written_per_commit": "index segment file + commit file, read from fs metadata",
        },
        "control_variables": {
            "WEFT_INDEX_MODE": env("WEFT_INDEX_MODE"),
            "WEFT_INDEX_SIZES": env("WEFT_INDEX_SIZES"),
            "WEFT_INDEX_LIVE": env("WEFT_INDEX_LIVE"),
            "WEFT_INDEX_WINDOW": env("WEFT_INDEX_WINDOW"),
            "WEFT_INDEX_GC": env("WEFT_INDEX_GC"),
        },
        "run": report,
        "peak_rss_kib": peak_rss_kib(),
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("incremental-index-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    let mut log = String::new();
    log.push_str(&format!("command: {}\n", env("WEFT_INDEX_COMMAND")));
    log.push_str(&format!(
        "mode: {mode} live: {live} checkpoints: {checkpoints:?}\n"
    ));
    for sample in report["samples"].as_array().into_iter().flatten() {
        log.push_str(&format!(
            "mode={} generation={} commit_p50={:.3} ms p95={:.3} ms p99={:.3} ms \
             bytes_per_commit_p50={:.0} index_bytes={} data_bytes={} depth={}\n",
            mode,
            sample["generation"],
            sample["commit_ms"]["p50"].as_f64().unwrap_or(0.0),
            sample["commit_ms"]["p95"].as_f64().unwrap_or(0.0),
            sample["commit_ms"]["p99"].as_f64().unwrap_or(0.0),
            sample["bytes_written_per_commit"]["p50"]
                .as_f64()
                .unwrap_or(0.0),
            sample["index_bytes"],
            sample["data_bytes"],
            sample["head_chain_depth"],
        ));
    }
    fs::write(
        path.with_extension("log"),
        format!(
            "{log}\n{}\n",
            serde_json::to_string_pretty(&receipt).expect("json")
        ),
    )
    .expect("write log");
    println!("incremental-index bench receipt: {}", path.display());
    print!("{log}");
}
