//! X14 court harness — indexed semantic candidate discovery.
//!
//! One store, three discovery arms on the same planning options:
//!
//! | arm | semantic candidate source | cost |
//! |---|---|---|
//! | `scan` | the implicit whole-store scan (`semantic_scan`) | O(objects) reads per plan |
//! | `index` | the persisted fingerprint index | one lookup, no other tensor read |
//! | `none` | neither | no mode-14 candidate at all |
//!
//! The court also builds the index (wall, bytes read, record size), prices a
//! lookup, and then *deletes* the closest bases to measure what a stale index
//! costs: the entries are skipped and counted, and the missed saving is
//! reported as a number rather than hidden.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use weftfs::semantic_index::{LookupRequest, SemanticIndex};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy};
use weftfs::{Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const ROWS: u64 = 512;
const COLS: u64 = 1024;
const VARIANTS: usize = 12;
const DISTRACTORS: usize = 8;
const FILLER: usize = 200;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 4096,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

/// Deterministic BF16 bytes. `family` selects a value distribution; the
/// `perturb` set flips only low mantissa bits, which the default semantic
/// policy (mantissa truncate 3) discards — so a perturbation set that is a
/// *subset* of another tensor's set leaves the semantic root unchanged. That is
/// exactly what mode 14 needs of a base, and it is what makes a "a checkpoint
/// that differs in a few values" fixture honest.
fn tensor_with(family: u32, seed: u64, perturb_at: &dyn Fn(u64) -> bool) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ (u64::from(family) << 32) ^ seed;
    let mut out = Vec::with_capacity((ROWS * COLS * 2) as usize);
    for index in 0..(ROWS * COLS) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        // The exponent is pinned to a finite value: the default semantic
        // policy preserves NaN/inf words bit-exactly, so a random exponent
        // would give two tensors that differ only in low mantissa bits two
        // different semantic roots and the fixture would measure nothing.
        let mut value = ((state.wrapping_mul(0x2545_f491_4f6c_dd1d) as u16) & 0x807F) | 0x3C00;
        if perturb_at(index) {
            value ^= 0x0005;
        }
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// A family-A variant: every `step`-th element perturbed.
fn variant(step: u64) -> Vec<u8> {
    tensor_with(1, 0, &move |index| index % step == 0)
}

/// A query: every 8th element perturbed (a superset of every base's set) plus a
/// few extra values that no stored tensor has.
fn query(index: u64) -> Vec<u8> {
    tensor_with(1, 0, &move |position| {
        position % 8 == 0 || position % (4096 + index) == 0
    })
}

fn plan_options(
    scan: bool,
    index: Option<Arc<SemanticIndex>>,
    max_candidates: usize,
) -> TensorPlanOptions {
    TensorPlanOptions {
        semantic_search: true,
        semantic_scan: scan,
        semantic_scan_max_objects: 4096,
        max_candidates,
        candidates: Vec::new(),
        semantic_index: index,
        semantic_index_query: true,
        ..TensorPlanOptions::default()
    }
}

fn stats(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    json!({"n": sorted.len(), "min": sorted.first().copied().unwrap_or(0.0), "p50": at(0.5), "max": sorted.last().copied().unwrap_or(0.0)})
}

struct Arm {
    semantic_detail: Vec<String>,
    semantic_bytes: u64,
    chosen_physical: u64,
    chosen: Vec<String>,
    plan_ms: Vec<f64>,
}

fn run_arm(
    store: &Store,
    queries: &[(String, Vec<u8>)],
    options_for: impl Fn() -> TensorPlanOptions,
) -> Arm {
    let layout = TensorLayoutSpec::row_major();
    let mut semantic_bytes = 0u64;
    let mut chosen_physical = 0u64;
    let mut chosen = Vec::new();
    let mut semantic_detail = Vec::new();
    let mut plan_ms = Vec::new();
    for (name, bytes) in queries {
        let options = options_for();
        let started = Instant::now();
        let plan = store
            .plan_tensor("bf16", &[ROWS, COLS], bytes, &layout, &[], &options)
            .unwrap_or_else(|err| panic!("plan {name}: {err}"));
        plan_ms.push(started.elapsed().as_secs_f64() * 1e3);
        chosen_physical += plan.chosen_physical_bytes;
        chosen.push(plan.chosen.as_str().to_owned());
        match plan
            .candidates
            .iter()
            .find(|candidate| candidate.strategy == TensorStrategy::SemanticDelta)
        {
            Some(candidate) => {
                semantic_detail.push(candidate.detail.clone());
                if candidate.applicable {
                    semantic_bytes += candidate.physical_bytes.unwrap_or(0);
                }
            }
            None => semantic_detail.push("no semantic candidate row".to_owned()),
        }
    }
    Arm {
        semantic_detail,
        semantic_bytes,
        chosen_physical,
        chosen,
        plan_ms,
    }
}

fn query_report(
    store: &Store,
    index: &SemanticIndex,
    queries: &[(String, Vec<u8>)],
) -> serde_json::Value {
    let mut examined = 0usize;
    let mut compatible = 0usize;
    let mut returned = 0usize;
    let mut stale = 0usize;
    let mut lookup_ms = Vec::new();
    let mut best = Vec::new();
    for (_name, bytes) in queries {
        let request = LookupRequest {
            dtype: weftfs::representation::TensorElementDtype::Bf16,
            shape: &[ROWS, COLS],
            bytes,
            exclude: None,
            limit: 8,
        };
        let started = Instant::now();
        let (picked, stats) = store.query_semantic_index(index, &request).unwrap();
        lookup_ms.push(started.elapsed().as_secs_f64() * 1e3);
        examined += stats.examined;
        compatible += stats.compatible;
        returned += picked.len();
        stale += stats.stale;
        best.push(stats.best_distance);
    }
    json!({
        "queries": queries.len(),
        "examined_total": examined,
        "compatible_total": compatible,
        "returned_total": returned,
        "stale_total": stale,
        "best_distance": best,
        "lookup_ms": stats(&lookup_ms),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json_path: Option<std::path::PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json_path = Some(std::path::PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }

    let dir = tempfile::TempDir::new().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let root = weftfs::state::State::ROOT_ID;

    // Family A: a base and near-identical variants (same semantic root).
    for variant_index in 0..VARIANTS {
        let bytes = variant(8 * (variant_index as u64 + 1));
        store
            .write_tensor_file(
                root,
                format!("family-a-{variant_index:02}").as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &bytes,
                &layout,
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
    }
    // Distractors: same shape, a different distribution, and *different*
    // semantic roots — the index must rank them behind family A.
    for distractor in 0..DISTRACTORS {
        let bytes = tensor_with(9 + distractor as u32, 0, &|_| false);
        store
            .write_tensor_file(
                root,
                format!("distractor-{distractor:02}").as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &bytes,
                &layout,
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
    }
    // Filler tensors of a different shape: they inflate the scan's walk (which
    // is what the index replaces) without ever being compatible.
    let filler_layout = TensorLayoutSpec::row_major();
    for filler in 0..FILLER {
        let bytes = tensor_with(50, filler as u64, &|_| false)[..8 * 16 * 2].to_vec();
        store
            .write_tensor_file(
                root,
                format!("filler-{filler:03}").as_bytes(),
                "bf16",
                &[8, 16],
                &bytes,
                &filler_layout,
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
    }

    // Query tensors: five perturbations that are *not* stored.
    let queries: Vec<(String, Vec<u8>)> = (0..5)
        .map(|index| (format!("query-{index}"), query(index as u64)))
        .collect();

    // ---- build ----------------------------------------------------------
    let started = Instant::now();
    let build = store.build_semantic_index(4096, u64::MAX).unwrap();
    let build_ms = started.elapsed().as_secs_f64() * 1e3;
    store.store_semantic_index(&build.index).unwrap();
    let reload = store.load_semantic_index().unwrap().expect("stored index");
    assert_eq!(reload.root(), build.index.root());
    let index = Arc::new(reload);

    // ---- arms -----------------------------------------------------------
    // The historical scan with the default candidate bound: it accepts the
    // first `max_candidates` *compatible* objects in object-id order, with no
    // similarity ranking at all. On a store with more compatible tensors than
    // that bound it can miss the right base entirely — measured here, not
    // assumed.
    let scan_default = run_arm(&store, &queries, || plan_options(true, None, 8));
    // The scan given a bound large enough to examine every compatible object:
    // the honest "what the scan can do when it is allowed to read everything".
    let scan_all = run_arm(&store, &queries, || {
        plan_options(true, None, VARIANTS + DISTRACTORS)
    });
    // The index at the same small bound as the historical scan.
    let indexed = run_arm(&store, &queries, || {
        plan_options(false, Some(Arc::clone(&index)), 8)
    });
    let none = run_arm(&store, &queries, || plan_options(false, None, 8));
    let lookup = query_report(&store, &index, &queries);

    // ---- stale index: delete the whole family --------------------------
    let names: Vec<Vec<u8>> = (0..VARIANTS)
        .map(|variant| format!("family-a-{variant:02}").into_bytes())
        .collect();
    for name in &names {
        store.unlink(root, name).unwrap();
    }
    let stale_lookup = query_report(&store, &index, &queries);
    let stale_index = run_arm(&store, &queries, || {
        plan_options(false, Some(Arc::clone(&index)), 8)
    });

    let mut checks: Vec<serde_json::Value> = Vec::new();
    {
        checks.push(json!({
            "label": "the exhaustive scan finds a real cross-version saving",
            "ok": scan_all.semantic_bytes > 0,
            "detail": format!(
                "{} B of semantic-delta payload over {} queries",
                scan_all.semantic_bytes, queries.len()
            ),
        }));
        checks.push(json!({
            "label": "the index recovers the exhaustive scan's saving exactly",
            "ok": scan_all.semantic_bytes == indexed.semantic_bytes,
            "detail": format!(
                "scan(all) {} B vs index(8) {} B (delta {} B)",
                scan_all.semantic_bytes,
                indexed.semantic_bytes,
                indexed.semantic_bytes as i64 - scan_all.semantic_bytes as i64
            ),
        }));
        checks.push(json!({
            "label": "the index and the exhaustive scan choose the same representations",
            "ok": scan_all.chosen == indexed.chosen
                && scan_all.chosen_physical == indexed.chosen_physical,
            "detail": format!(
                "chosen {:?} vs {:?}; total physical {} vs {}",
                scan_all.chosen, indexed.chosen, scan_all.chosen_physical,
                indexed.chosen_physical
            ),
        }));
        checks.push(json!({
            "label": "the index finds a semantic candidate for every query",
            "ok": lookup["returned_total"].as_u64().unwrap_or(0) >= queries.len() as u64,
            "detail": format!("{lookup}"),
        }));
        checks.push(json!({
            "label": "without an index or a scan there is no semantic saving",
            "ok": none.semantic_bytes == 0,
            "detail": format!("{} B", none.semantic_bytes),
        }));
        checks.push(json!({
            "label": "a stale index skips entries instead of serving them",
            "ok": stale_lookup["stale_total"].as_u64().unwrap_or(0) >= VARIANTS as u64,
            "detail": format!("{stale_lookup}"),
        }));
    }

    let document = json!({
        "schema": "weft.semantic-index.court.v1",
        "fixture": {
            "family_variants": VARIANTS,
            "distractors": DISTRACTORS,
            "filler_other_shape": FILLER,
            "queries": queries.len(),
            "dtype": "bf16",
            "shape": [ROWS, COLS],
            "tensor_bytes": ROWS * COLS * 2,
        },
        "build": {
            "entries": build.index.len(),
            "objects_scanned": build.objects_scanned,
            "skipped": build.skipped,
            "bytes_read": build.bytes_read,
            "record_bytes": build.record_bytes,
            "wall_ms": build_ms,
            "us_per_entry": if build.index.is_empty() { 0.0 } else { build_ms * 1000.0 / build.index.len() as f64 },
        },
        "lookup": lookup,
        "stale_lookup": stale_lookup,
        "arms": {
            "scan_default_bound": {
                "semantic_detail": scan_default.semantic_detail,
                "semantic_bytes": scan_default.semantic_bytes,
                "chosen_physical_bytes": scan_default.chosen_physical,
                "chosen": scan_default.chosen,
                "plan_ms": stats(&scan_default.plan_ms),
                "missed_vs_scan_all_bytes": scan_all.semantic_bytes as i64
                    - scan_default.semantic_bytes as i64,
            },
            "scan_all": {
                "semantic_detail": scan_all.semantic_detail,
                "semantic_bytes": scan_all.semantic_bytes,
                "chosen_physical_bytes": scan_all.chosen_physical,
                "chosen": scan_all.chosen,
                "plan_ms": stats(&scan_all.plan_ms),
            },
            "index": {
                "semantic_detail": indexed.semantic_detail,
                "semantic_bytes": indexed.semantic_bytes,
                "chosen_physical_bytes": indexed.chosen_physical,
                "chosen": indexed.chosen,
                "plan_ms": stats(&indexed.plan_ms),
            },
            "none": {
                "semantic_detail": none.semantic_detail,
                "semantic_bytes": none.semantic_bytes,
                "chosen_physical_bytes": none.chosen_physical,
                "chosen": none.chosen,
                "plan_ms": stats(&none.plan_ms),
            },
            "stale_index": {
                "semantic_bytes": stale_index.semantic_bytes,
                "chosen_physical_bytes": stale_index.chosen_physical,
                "chosen": stale_index.chosen,
                "plan_ms": stats(&stale_index.plan_ms),
                "missed_semantic_bytes": scan_all.semantic_bytes as i64
                    - stale_index.semantic_bytes as i64,
                "missed_semantic_fraction": if scan_all.semantic_bytes == 0 { 0.0 } else {
                    1.0 - (stale_index.semantic_bytes as f64 / scan_all.semantic_bytes as f64)
                },
                "miss_rate": if queries.is_empty() { 0.0 } else {
                    (queries.len() as f64 - (stale_index.semantic_bytes > 0) as u8 as f64) / queries.len() as f64
                },
            },
        },
        "checks": checks,
        "checks_failed": checks.iter().filter(|check| check["ok"].as_bool() != Some(true)).count(),
        "environment": {
            "cpus": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "target": std::env::consts::ARCH,
            "clock": "Instant; the store is local and the page cache is warm",
        },
        "verification": {
            "store_verify_ok": store.verify(true).unwrap().ok,
            "open_mode": "read-write",
        },
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => std::fs::write(&path, text).unwrap(),
        None => println!("{text}"),
    }
    if document["checks_failed"].as_u64().unwrap_or(1) != 0 {
        eprintln!(
            "semantic_index_court: {} check(s) failed",
            document["checks_failed"]
        );
        std::process::exit(1);
    }
}
