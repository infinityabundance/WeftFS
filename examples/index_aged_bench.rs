//! Aged-store advisory-index court: what an old store costs.
//!
//! Two independent dimensions, one store per cell:
//!
//! * `gens` — the *history* grows: a fixed live working set is seeded, then one
//!   small `set_xattr` commit per step. Stresses per-commit index work, the
//!   segment chain, and reopen cost after a long history.
//! * `live` — the *state* grows: `WEFT_AGED_TARGET` live objects are seeded (in
//!   group commits) and a fixed number of measured commits follow. Stresses the
//!   per-object part of the index.
//!
//! For the target cell this measures:
//!   * publish wall p50/p95/p99 and index bytes per update,
//!   * index bytes vs data bytes,
//!   * cold reopen p50/p95 **with** the persisted segments, and **without**
//!     them (the directory is renamed away, so the reader must rebuild),
//!   * first-query cold p50/p95 for relation, tag and attr-eq selectors, and
//!   * peak RSS.
//!
//! Correctness at every cell:
//!   * the persisted index's ordered answers equal the scan oracle for every
//!     selector, and the writer's maintained WFX2 sections equal a full
//!     `StateIndex::build`,
//!   * GC with a snapshot pin keeps every segment a retained root names and
//!     reclaims the rest,
//!   * a forged head segment (one tag row removed, otherwise valid) is refused
//!     and rebuilt, and the rebuilt answers still equal the oracle.
//!
//! Environment:
//!   WEFT_AGED_MODE     `gens` | `live` (default `gens`)
//!   WEFT_AGED_TARGET   cell size (default `10000`)
//!   WEFT_AGED_LIVE     live objects for `gens` mode (default `64`)
//!   WEFT_AGED_GENS     measured commits for `live` mode (default `1000`)
//!   WEFT_AGED_ROOT     store directory (required; must be absent or empty)
//!   WEFT_AGED_SEED_BATCH  objects per seed group commit (default `4096`)
//!   WEFT_AGED_WINDOW   commits in the publish window (default `200`)
//!   WEFT_AGED_OPENS    cold reopen samples per mode (default `5`)
//!   WEFT_AGED_SKIP_FORGE=1  skip the forged-segment sub-check
//!   WEFT_AGED_STAMP    receipt stamp (default: current UTC)
//!   WEFT_AGED_DECLARED_BYTES  declared scratch budget, echoed into the receipt
//!
//! Run in the Docker court (release):
//!   cargo run --release --locked --example index_aged_bench
//!
//! The receipt is `bench/receipts/index-aged-<stamp>.json`, its log beside it.

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

fn allocated_bytes(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += allocated_bytes(&entry.path());
        } else {
            total += meta.blocks() * 512;
        }
    }
    total
}

fn file_count(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += file_count(&entry.path());
        } else {
            total += 1;
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

fn env_u64(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(fallback)
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: CHECKPOINT_INTERVAL,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        incremental_index: true,
        incremental_index_maintenance: true,
        index_chain_max_deltas: env_u64("WEFT_AGED_MAX_DELTAS", 32) as u32,
        ..Options::default()
    }
}

fn seed(store: &mut Store, live: u64, batch_size: u64) {
    let root = store.resolve_path("/").expect("root");
    let teams = ["red", "green", "blue"];
    let mut done = 0u64;
    while done < live {
        let batch = (live - done).min(batch_size);
        let start = done;
        store
            .group(|group| {
                for index in start..start + batch {
                    let mut attrs = BTreeMap::new();
                    attrs.insert(
                        "team".to_owned(),
                        Value::Str(teams[(index % 3) as usize].to_owned()),
                    );
                    attrs.insert("score".to_owned(), Value::U64(index % 16));
                    let id = group.create_file_seeded(
                        root,
                        format!("f{index}").as_bytes(),
                        format!("seed-{index}").as_bytes(),
                        0o644,
                        0,
                        0,
                        attrs,
                        format!("index-aged-seed-{index}").as_bytes(),
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

fn selectors(relation_target: &str) -> Vec<(&'static str, Selector)> {
    let texts: [(&'static str, String); 4] = [
        (
            "relation",
            format!("select relation(links, obj:{relation_target})"),
        ),
        ("tag", "select tag(\"alpha\")".to_owned()),
        ("attr_eq", "select attrs.team == \"red\"".to_owned()),
        ("has_attr", "select has(attrs.score)".to_owned()),
    ];
    texts
        .into_iter()
        .map(|(label, text)| (label, selector::parse(&text).expect("selector")))
        .collect()
}

/// Ordered identity of a resolution: object ids, then matched count. Two
/// resolutions are equal only when both the set *and* the order agree.
fn resolution_signature(result: &weftfs::evaluate::Resolution) -> (Vec<String>, u64) {
    (
        result
            .results
            .iter()
            .map(|entry| entry.object_id.to_hex())
            .collect(),
        result.matched_count,
    )
}

/// One cold read-only open and the first indexed resolve per selector.
fn cold_open(
    root: &Path,
    selectors: &[(&'static str, Selector)],
    principal: &Principal,
) -> serde_json::Value {
    let limits = weftcodec::Limits::default();
    let (open_ms, opened) = timed(|| Store::open(root, OpenMode::ReadOnly));
    let store = match opened {
        Ok(store) => store,
        Err(err) => return json!({"error": err.to_string()}),
    };
    let mut queries = serde_json::Map::new();
    let mut oracle_match = true;
    for (label, selector) in selectors {
        let (ms, indexed) = timed(|| store.resolve(selector, None, principal, &limits));
        let indexed = match indexed {
            Ok(value) => value,
            Err(err) => {
                queries.insert((*label).to_owned(), json!({"error": err.to_string()}));
                continue;
            }
        };
        let scanned = store
            .resolve_scan(selector, None, principal, &limits)
            .expect("scan oracle");
        let same = resolution_signature(&indexed) == resolution_signature(&scanned);
        oracle_match &= same;
        queries.insert(
            (*label).to_owned(),
            json!({
                "ms": ms,
                "matched": indexed.matched_count,
                "oracle_match": same,
            }),
        );
    }
    let stats = store.index_stats();
    json!({
        "open_ms": open_ms,
        "queries": queries,
        "oracle_match": oracle_match,
        "index_stats": {
            "loads": stats.disk_loads,
            "builds": stats.builds,
            "rejects": stats.rejects,
            "maintenance_fallbacks": stats.maintenance_fallbacks,
            "notes": stats.notes,
        },
    })
}

fn percentile_block(values: &[f64]) -> serde_json::Value {
    json!({
        "p50": median(values),
        "p95": percentile(values, 0.95),
        "samples_ms": values,
    })
}

fn cold_series(
    root: &Path,
    selectors: &[(&'static str, Selector)],
    principal: &Principal,
    opens: u64,
) -> serde_json::Value {
    let mut open_ms = Vec::new();
    let mut per_selector: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut all_match = true;
    let mut loads = Vec::new();
    let mut builds = Vec::new();
    let mut rejects = Vec::new();
    for _ in 0..opens {
        let sample = cold_open(root, selectors, principal);
        if let Some(ms) = sample["open_ms"].as_f64() {
            open_ms.push(ms);
        }
        all_match &= sample["oracle_match"].as_bool().unwrap_or(false);
        if let Some(stats) = sample["index_stats"].as_object() {
            loads.push(stats["loads"].as_u64().unwrap_or(0));
            builds.push(stats["builds"].as_u64().unwrap_or(0));
            rejects.push(stats["rejects"].as_u64().unwrap_or(0));
        }
        if let Some(queries) = sample["queries"].as_object() {
            for (label, _) in selectors {
                if let Some(ms) = queries.get(*label).and_then(|v| v["ms"].as_f64()) {
                    per_selector.entry(*label).or_default().push(ms);
                }
            }
        }
    }
    let queries = per_selector
        .into_iter()
        .map(|(label, values)| (label.to_owned(), percentile_block(&values)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "open": if open_ms.is_empty() { serde_json::Value::Null } else { percentile_block(&open_ms) },
        "first_query": queries,
        "oracle_match": all_match,
        "index_stats": { "loads": loads, "builds": builds, "rejects": rejects },
    })
}

fn run() -> serde_json::Value {
    let mode = env_string("WEFT_AGED_MODE", "gens");
    assert!(
        mode == "gens" || mode == "live",
        "WEFT_AGED_MODE must be gens or live"
    );
    let target = env_u64("WEFT_AGED_TARGET", 10_000);
    assert!(target > 0, "WEFT_AGED_TARGET must be positive");
    let live = if mode == "gens" {
        env_u64("WEFT_AGED_LIVE", 64)
    } else {
        target
    };
    let generations = if mode == "gens" {
        target
    } else {
        env_u64("WEFT_AGED_GENS", 1_000)
    };
    let root = PathBuf::from(std::env::var("WEFT_AGED_ROOT").expect("WEFT_AGED_ROOT"));
    if root.exists() {
        assert!(
            fs::read_dir(&root)
                .map(|mut e| e.next().is_none())
                .unwrap_or(false),
            "WEFT_AGED_ROOT must be absent or empty"
        );
    }
    let batch_size = env_u64("WEFT_AGED_SEED_BATCH", 4096).max(1);
    let window = env_u64("WEFT_AGED_WINDOW", 200) as usize;
    let opens = env_u64("WEFT_AGED_OPENS", 5);
    let skip_forge = env_string("WEFT_AGED_SKIP_FORGE", "0") == "1";
    let principal = principal();

    let (seed_ms, mut store) = timed(|| {
        let mut store = Store::init(&root, options()).expect("init store");
        seed(&mut store, live, batch_size);
        store
    });
    let seeded_generation = store.generation();

    let mut ids: Vec<weftfs::identity::Hash32> = Vec::with_capacity(live as usize);
    let dir_id = store.resolve_path("/").expect("root");
    for index in 0..live {
        ids.push(
            store
                .lookup(dir_id, format!("f{index}").as_bytes())
                .expect("lookup"),
        );
    }
    // One object-targeted relation per eight objects, in group commits, so the
    // relation selector has a real target with a non-trivial match set.
    let relation_target = ids[0];
    let mut linked = 0u64;
    while linked < live {
        let batch = (live - linked).min(batch_size as u64);
        let start = linked;
        store
            .group(|group| {
                for index in start..start + batch {
                    if index % 8 == 0 {
                        group.relation_add(
                            ids[index as usize],
                            "links",
                            RelationTarget::Object(relation_target),
                            None,
                            BTreeMap::new(),
                        )?;
                    }
                }
                Ok(())
            })
            .expect("link group");
        linked += batch;
    }
    let selectors = selectors(&relation_target.to_hex());

    let mut commit_ms: Vec<f64> = Vec::with_capacity(window);
    let mut commit_bytes: Vec<f64> = Vec::with_capacity(window);
    let (commit_wall_ms, (failure, failed)) = timed(|| {
        for step in 0..generations {
            let target_id = if mode == "gens" {
                ids[(step as usize) % ids.len()]
            } else {
                // Live cells rotate the attribute over the whole state.
                ids[(step as usize) % ids.len()]
            };
            let value = (step as u64 + 1).to_le_bytes();
            let start = Instant::now();
            let outcome = store.set_xattr(target_id, "rev", &value);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if let Err(err) = outcome {
                return (
                    json!({
                        "generation": step as u64 + 1,
                        "class": format!("{:?}", err.class()),
                        "message": err.to_string(),
                    }),
                    true,
                );
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
        }
        (serde_json::Value::Null, false)
    });

    let measured_generation = store.generation();
    let index_stats = store.index_stats();
    let (head_kind, head_depth) = store.index_head_chain().expect("head chain");
    let index_bytes = dir_bytes(&root.join("indexes"));
    let data = data_bytes(&root);
    // A checkpoint state manifest is encoded with the codec's default WCB
    // limits, so a large live state may not be able to write one at all. The
    // court records what actually exists instead of assuming it.
    let checkpoint_names = store.list_checkpoints().unwrap_or_default();

    // A cell whose measured commit was refused (for example a checkpoint state
    // manifest above the codec's WCB item bound) is published as a failure, not
    // as a panic: the receipt carries the typed error and the post-measure
    // steps are recorded as skipped.
    let mut sections_match = None;
    let mut snapshot_ms = 0.0f64;
    let mut snapshot = weftfs::identity::Hash32::ZERO;
    let mut retained_before: Vec<weftfs::identity::Hash32> = Vec::new();
    let mut retained_exists_before = 0usize;
    let mut gc_ms = 0.0f64;
    let mut deleted_indexes = 0usize;
    let mut deleted_index_bytes = 0u64;
    let mut retained_after_gc: Vec<weftfs::identity::Hash32> = Vec::new();
    let mut retained_missing = 0usize;
    let mut gc_problems: Vec<String> = Vec::new();
    let mut gc_checkpoints = 0u64;
    let mut index_files_after_gc = 0u64;
    let mut cold_persisted = json!({"skipped": "the measured commit failed"});
    let mut cold_rebuilt = json!({"skipped": "the measured commit failed"});
    let mut forgery = json!({"skipped": "the measured commit failed"});

    if !failed {
        sections_match = match (
            store.head_index_sections_for_test(),
            store.rebuilt_index_sections_for_test(),
        ) {
            (Ok(maintained), Ok(rebuilt)) => Some(maintained == rebuilt),
            _ => None,
        };

        // GC with a snapshot pin: every segment a retained root names must survive.
        let (ms, pinned) = timed(|| store.snapshot_create("aged-pin").expect("snapshot"));
        snapshot_ms = ms;
        snapshot = pinned;
        retained_before = store.retained_index_segment_ids().expect("retained ids");
        retained_exists_before = retained_before
            .iter()
            .filter(|id| index_path(&root, **id).exists())
            .count();
        let (ms, report) = timed(|| store.gc(true).expect("gc"));
        gc_ms = ms;
        deleted_indexes = report.deleted_indexes.len();
        deleted_index_bytes = report.deleted_index_bytes;
        retained_after_gc = store
            .retained_index_segment_ids()
            .expect("retained ids after");
        retained_missing = retained_after_gc
            .iter()
            .filter(|id| !index_path(&root, **id).exists())
            .count();
        gc_problems = report.problems.clone();
        gc_checkpoints = report.checkpoints_retained;
        index_files_after_gc = file_count(&root.join("indexes"));
        drop(report);
        drop(store);

        // Cold reopen: with the persisted segments, then with them renamed away.
        cold_persisted = cold_series(&root, &selectors, &principal, opens);
        let indexes = root.join("indexes");
        let hidden = root.join("indexes.hidden");
        fs::rename(&indexes, &hidden).expect("hide index segments");
        cold_rebuilt = cold_series(&root, &selectors, &principal, opens);
        fs::rename(&hidden, &indexes).expect("restore index segments");

        // Forged head segment: one tag row removed, everything else valid. The
        // commit attestation is the only refutation; a reader that trusted the
        // file would lose a quarter of the tag matches.
        if skip_forge {
            forgery = json!({"skipped": true});
        } else {
            let mut store = Store::open(&root, OpenMode::ReadWrite).expect("writable reopen");
            let (compact_ms, compacted) = timed(|| store.compact_index());
            let compact_ok = compacted.is_ok();
            let pre_path = index_path(&root, store.head());
            let pre_forged = fs::read(&pre_path).unwrap_or_default();
            // Does the freshly published full segment decode at all? This is the
            // regression check for the relation-edge bound: before the fix every
            // compacted segment carrying a value-target relation was rebuild-only.
            let compact_probe = {
                let limits = weftcodec::Limits::default();
                let probe = Store::open(&root, OpenMode::ReadOnly).expect("probe open");
                probe.reset_index_stats();
                let query = selector::parse("select tag(\"alpha\")").expect("probe selector");
                let _ = probe.resolve(&query, None, &principal, &limits);
                let stats = probe.index_stats();
                json!({
                    "loads": stats.disk_loads,
                    "builds": stats.builds,
                    "rejects": stats.rejects,
                    "notes": stats.notes,
                })
            };
            let forged = store.forge_head_index_remove_tag_for_test("alpha");
            let forged_head = store.head().to_hex();
            let forged_path = index_path(&root, store.head());
            let forged_bytes = fs::read(&forged_path).unwrap_or_default();
            let forged_magic = forged_bytes.get(0..4).map(|b| b.to_vec());
            let count_at = |bytes: &[u8]| -> Option<u32> {
                bytes
                    .get(113..117)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_le_bytes)
            };
            let pre_count = count_at(&pre_forged);
            let post_count = count_at(&forged_bytes);
            drop(store);
            forgery = if !compact_ok {
                json!({"compact_ms": compact_ms, "error": "compact_index failed"})
            } else if let Err(err) = forged {
                json!({"compact_ms": compact_ms, "error": err.to_string()})
            } else {
                let sample = cold_open(&root, &selectors, &principal);
                json!({
                    "compact_ms": compact_ms,
                    "head": forged_head,
                    "forged_len": forged_bytes.len(),
                    "forged_magic": forged_magic,
                    "pre_len": pre_forged.len(),
                    "pre_first_section_count": pre_count,
                    "post_first_section_count": post_count,
                    "compact_probe": compact_probe,
                    "open_ms": sample["open_ms"],
                    "oracle_match": sample["oracle_match"],
                    "tag_matched": sample["queries"]["tag"]["matched"],
                    "index_stats": sample["index_stats"],
                })
            };
        }
    } else {
        drop(store);
    }

    json!({
        "mode": mode,
        "cell": {
            "target": target,
            "live_objects": live,
            "measured_generations": generations,
        },
        "root": root.display().to_string(),
        "declared_scratch_bytes": env_u64("WEFT_AGED_DECLARED_BYTES", 0),
        "seed_ms": seed_ms,
        "seeded_generation": seeded_generation,
        "failure": failure,
        "failed": failed,
        "commit_wall_ms": commit_wall_ms,
        "publish": {
            "p50": if commit_ms.is_empty() { 0.0 } else { median(&commit_ms) },
            "p95": if commit_ms.is_empty() { 0.0 } else { percentile(&commit_ms, 0.95) },
            "p99": if commit_ms.is_empty() { 0.0 } else { percentile(&commit_ms, 0.99) },
            "mean": if commit_ms.is_empty() { 0.0 } else { commit_ms.iter().sum::<f64>() / commit_ms.len() as f64 },
            "window": commit_ms.len(),
        },
        "bytes_per_update": {
            "p50": if commit_bytes.is_empty() { 0.0 } else { median(&commit_bytes) },
            "p95": if commit_bytes.is_empty() { 0.0 } else { percentile(&commit_bytes, 0.95) },
            "mean": if commit_bytes.is_empty() { 0.0 } else { commit_bytes.iter().sum::<f64>() / commit_bytes.len() as f64 },
        },
        "footprint": {
            "index_bytes": index_bytes,
            "index_allocated_bytes": allocated_bytes(&root.join("indexes")),
            "index_files": file_count(&root.join("indexes")),
            "data_bytes": data,
            "commit_bytes": dir_bytes(&root.join("commits")),
            "commit_files": file_count(&root.join("commits")),
            "total_allocated_bytes": allocated_bytes(&root),
            "index_ppm_of_data": if data == 0 { 0 } else { (index_bytes as f64 / data as f64 * 1_000_000.0).round() as u64 },
            "index_bytes_per_live_object": if live == 0 { 0 } else { (index_bytes / live) as u64 },
        },
        "generation_after_measure": measured_generation,
        "named_checkpoints": checkpoint_names.len(),
        "head_segment": head_kind,
        "head_chain_depth": head_depth,
        "index_stats": {
            "loads": index_stats.disk_loads,
            "builds": index_stats.builds,
            "maintenance_updates": index_stats.maintenance_updates,
            "maintenance_fallbacks": index_stats.maintenance_fallbacks,
            "rejects": index_stats.rejects,
            "writes": index_stats.writes,
            "write_errors": index_stats.write_errors,
            "notes": index_stats.notes,
        },
        "oracle": {
            "maintained_sections_equal_rebuild": sections_match,
        },
        "gc": {
            "snapshot_ms": snapshot_ms,
            "snapshot": snapshot.to_hex(),
            "retained_before": retained_before.len(),
            "retained_present_before": retained_exists_before,
            "retained_after": retained_after_gc.len(),
            "retained_missing_after": retained_missing,
            "deleted_indexes": deleted_indexes,
            "deleted_index_bytes": deleted_index_bytes,
            "index_files_after": index_files_after_gc,
            "checkpoint_manifests": gc_checkpoints,
            "ms": gc_ms,
            "problems": gc_problems,
        },
        "cold_persisted": cold_persisted,
        "cold_rebuilt": cold_rebuilt,
        "forged_segment": forgery,
        "peak_rss_kib": peak_rss_kib(),
    })
}

fn main() {
    let report = run();

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let stamp = std::env::var("WEFT_AGED_STAMP")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| utc_iso(generated));
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());
    let receipt = json!({
        "schema": "weftfs.aged-index-court.v1",
        "label": "synthetic fixture, one store per cell, one process, single host; \
                  `gens` grows history with a constant live set, `live` grows state. \
                  A scratch root is declared in the cell, never assumed to be tmpfs.",
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
            "tmpfs": env("WEFT_AGED_TMPFS"),
            "storage": env("WEFT_AGED_STORAGE"),
        },
        "control_variables": {
            "WEFT_AGED_MODE": env("WEFT_AGED_MODE"),
            "WEFT_AGED_TARGET": env("WEFT_AGED_TARGET"),
            "WEFT_AGED_LIVE": env("WEFT_AGED_LIVE"),
            "WEFT_AGED_GENS": env("WEFT_AGED_GENS"),
            "WEFT_AGED_SEED_BATCH": env("WEFT_AGED_SEED_BATCH"),
            "WEFT_AGED_OPENS": env("WEFT_AGED_OPENS"),
            "WEFT_AGED_SKIP_FORGE": env("WEFT_AGED_SKIP_FORGE"),
        },
        "cell_label": env("WEFT_AGED_LABEL"),
        "reduced_from": std::env::var("WEFT_AGED_REDUCED_FROM").ok(),
        "reduced_reason": std::env::var("WEFT_AGED_REDUCED_REASON").ok(),
        "method": {
            "publish": "each measured step is one `set_xattr` commit; statistics are the last WEFT_AGED_WINDOW commits",
            "bytes_per_update": "head index segment file + commit file, from fs metadata after each commit",
            "cold_persisted": "a fresh read-only handle per sample; first indexed resolve per selector, compared with resolve_scan",
            "cold_rebuilt": "the same, with `indexes/` renamed away so every answer must be rebuilt from replayed state",
            "forged_segment": "one valid full segment with the tag row removed replaces the head; the commit attestation must refute it",
            "gc": "snapshot pin taken at the target, then gc(apply=true); retained roots come from the store's own traversal",
        },
        "run": report,
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("index-aged-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    let run = &receipt["run"];
    let mut log = String::new();
    log.push_str(&format!("command: {}\n", env("WEFT_AGED_COMMAND")));
    log.push_str(&format!(
        "mode={} target={} live={} generations={}\n",
        run["mode"],
        run["cell"]["target"],
        run["cell"]["live_objects"],
        run["cell"]["measured_generations"]
    ));
    log.push_str(&format!(
        "publish p50={:.3} p95={:.3} p99={:.3} ms | bytes/update p50={:.0} mean={:.0}\n",
        run["publish"]["p50"].as_f64().unwrap_or(0.0),
        run["publish"]["p95"].as_f64().unwrap_or(0.0),
        run["publish"]["p99"].as_f64().unwrap_or(0.0),
        run["bytes_per_update"]["p50"].as_f64().unwrap_or(0.0),
        run["bytes_per_update"]["mean"].as_f64().unwrap_or(0.0),
    ));
    log.push_str(&format!(
        "index={} data={} commit={} allocated={} index_files={}\n",
        run["footprint"]["index_bytes"],
        run["footprint"]["data_bytes"],
        run["footprint"]["commit_bytes"],
        run["footprint"]["total_allocated_bytes"],
        run["footprint"]["index_files"],
    ));
    for key in ["cold_persisted", "cold_rebuilt"] {
        let cold = &run[key];
        log.push_str(&format!(
            "{key}: open p50={:?} p95={:?} | oracle_match={} loads={:?} builds={:?} rejects={:?}\n",
            cold["open"]["p50"],
            cold["open"]["p95"],
            cold["oracle_match"],
            cold["index_stats"]["loads"],
            cold["index_stats"]["builds"],
            cold["index_stats"]["rejects"]
        ));
    }
    log.push_str(&format!(
        "gc: retained={} missing_after={} deleted={} deleted_bytes={} ms={:.3} problems={}\n",
        run["gc"]["retained_after"],
        run["gc"]["retained_missing_after"],
        run["gc"]["deleted_indexes"],
        run["gc"]["deleted_index_bytes"],
        run["gc"]["ms"].as_f64().unwrap_or(0.0),
        run["gc"]["problems"]
    ));
    log.push_str(&format!(
        "oracle sections_equal_rebuild={:?} forged={}\n",
        run["oracle"]["maintained_sections_equal_rebuild"], run["forged_segment"]
    ));
    log.push_str(&format!("peak_rss_kib={}\n", run["peak_rss_kib"]));
    fs::write(
        path.with_extension("log"),
        format!(
            "{log}\n{}\n",
            serde_json::to_string_pretty(&receipt).expect("json")
        ),
    )
    .expect("write log");
    println!("index-aged court receipt: {}", path.display());
    print!("{log}");
}
