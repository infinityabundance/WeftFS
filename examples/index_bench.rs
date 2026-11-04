//! Synthetic in-memory before/after harness for the per-state selector index.
//!
//! This measures resolver candidate selection only. The state is built directly
//! in memory (no disk replay, no FUSE), and "before" is the full authorized
//! scan while "after" is index-narrowed candidates confirmed by `eval_expr`.
//! It is labelled synthetic in the receipt and makes no end-to-end claim.
//!
//! Run in the Docker court:
//!   cargo run --release --example index_bench

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::evaluate::Resolution;
use weftfs::identity::{content_hash, object_id_from_seed, Hash32};
use weftfs::state::{ObjectKind, ObjectRecord, RelationRecord, RelationTarget, State};
use weftfs::store::{Options, Store};

const OBJECTS: usize = 10_000;
const RELATIONS: usize = 10_000;
const ITERATIONS: usize = 100;

struct KindResult {
    name: &'static str,
    before_ms: Vec<f64>,
    after_ms: Vec<f64>,
    cold_ms: f64,
    matched_total: u64,
}

fn build_state() -> (State, Vec<Hash32>, Vec<Hash32>) {
    let mut state = State::default();
    state.generation = 1;

    let empty = content_hash(b"");
    state.objects.insert(
        State::ROOT_ID,
        ObjectRecord {
            id: State::ROOT_ID,
            kind: ObjectKind::Dir,
            mode: 0o755,
            uid: 0,
            gid: 0,
            mtime_ns: 0,
            version: empty,
            content: empty,
            size: 0,
            attrs: BTreeMap::new(),
        },
    );

    let teams = ["red", "green", "blue"];
    let mut ids = Vec::with_capacity(OBJECTS);
    for index in 0..OBJECTS {
        let id = object_id_from_seed(format!("index-bench-object-{index}").as_bytes());
        let mut nested = BTreeMap::new();
        nested.insert("a".to_owned(), Value::U64(index as u64 % 7));
        nested.insert("b".to_owned(), Value::Str("nested".to_owned()));
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            Value::Str(teams[index % teams.len()].to_owned()),
        );
        attrs.insert("score".to_owned(), Value::U64((index % 100) as u64));
        attrs.insert("nested".to_owned(), Value::Map(nested));
        attrs.insert("empty".to_owned(), Value::Array(Vec::new()));
        // One in seven objects is unreadable by the uid-0 principal, so the
        // authorized universe is a strict subset of the state.
        let (mode, uid, gid) = if index % 7 == 0 {
            (0o600, 2000, 2000)
        } else {
            (0o644, 1000, 1000)
        };
        state.objects.insert(
            id,
            ObjectRecord {
                id,
                kind: ObjectKind::File,
                mode,
                uid,
                gid,
                mtime_ns: index as i64,
                version: object_id_from_seed(format!("index-bench-version-{index}").as_bytes()),
                content: content_hash(format!("index-bench-content-{index}").as_bytes()),
                size: index as u64 % 1000,
                attrs,
            },
        );
        ids.push(id);
    }

    // 5,000 object-target edges and 5,000 `tagged_as` value edges = 10,000.
    let mut link_targets = Vec::new();
    for index in 0..RELATIONS / 2 {
        let subject = ids[index];
        let target_index = (index * 37 + 11) % OBJECTS;
        let target = ids[target_index];
        if target_index % 7 != 0 {
            link_targets.push(target);
        }
        let relation = RelationRecord::new(
            subject,
            "links",
            RelationTarget::Object(target),
            None,
            BTreeMap::new(),
        )
        .expect("links relation");
        state.relations.insert(relation.id, relation);

        let subject = ids[index];
        let relation = RelationRecord::new(
            subject,
            "tagged_as",
            RelationTarget::Value(Value::Str(format!("group-{}", index % 50))),
            None,
            BTreeMap::new(),
        )
        .expect("tag relation");
        state.relations.insert(relation.id, relation);
    }

    (state, ids, link_targets)
}

fn parse_all(texts: Vec<String>) -> Vec<Selector> {
    texts
        .into_iter()
        .map(|text| {
            selector::parse(&text)
                .unwrap_or_else(|error| panic!("selector `{text}` did not parse: {error}"))
        })
        .collect()
}

fn compact(resolution: &Resolution) -> (u64, Vec<(Hash32, Hash32, Hash32)>) {
    (
        resolution.matched_count,
        resolution
            .results
            .iter()
            .map(|item| (item.object_id, item.version_id, item.content_id))
            .collect(),
    )
}

fn timed<T, F: FnMut() -> T>(mut action: F) -> (f64, T) {
    let start = Instant::now();
    let value = action();
    (start.elapsed().as_secs_f64() * 1000.0, value)
}

fn median(values: &mut [f64]) -> f64 {
    assert!(!values.is_empty());
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn run_kind(
    name: &'static str,
    store: &Store,
    state: &State,
    snapshot: Hash32,
    principal: &Principal,
    limits: &weftcodec::Limits,
    selectors: &[Selector],
) -> KindResult {
    let mut before_ms = Vec::with_capacity(selectors.len());
    let mut expected = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let (ms, resolution) = timed(|| {
            store
                .resolve_state_scan(selector, snapshot, state, principal, limits)
                .expect("scan resolution")
        });
        before_ms.push(ms);
        expected.push(compact(&resolution));
    }

    // Each kind uses its own snapshot key, so this first indexed call is a
    // genuine cache miss and includes one `StateIndex::build`.
    let (cold_ms, cold) = timed(|| {
        store
            .resolve_state(&selectors[0], snapshot, state, principal, limits)
            .expect("indexed resolution")
    });
    assert_eq!(compact(&cold), expected[0], "{name}: cold indexed != scan");

    let mut after_ms = Vec::with_capacity(selectors.len());
    for (selector, scan) in selectors.iter().zip(expected.iter()) {
        let (ms, resolution) = timed(|| {
            store
                .resolve_state(selector, snapshot, state, principal, limits)
                .expect("indexed resolution")
        });
        assert_eq!(
            &compact(&resolution),
            scan,
            "{name}: indexed resolution != scan oracle"
        );
        after_ms.push(ms);
    }

    KindResult {
        name,
        before_ms,
        after_ms,
        cold_ms,
        matched_total: expected.iter().map(|(count, _)| count).sum(),
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_owned())
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir for cache-holding store");
    let store = Store::init(dir.path(), Options::default()).expect("init store");
    let (state, ids, link_targets) = build_state();
    let principal = Principal::new(0, 0, Vec::new());
    let limits = weftcodec::Limits::default();

    let visible: Vec<Hash32> = ids
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 7 != 0)
        .map(|(_, id)| *id)
        .collect();

    let relation_texts = (0..ITERATIONS)
        .map(|index| {
            format!(
                "select relation(links, obj:{})",
                link_targets[index % link_targets.len()].to_hex()
            )
        })
        .collect::<Vec<_>>();
    let tag_texts = (0..ITERATIONS)
        .map(|index| format!("select tag(\"group-{}\")", index % 50))
        .collect::<Vec<_>>();
    let attr_texts = (0..ITERATIONS)
        .map(|index| {
            if index % 2 == 0 {
                format!(
                    "select attrs.team == \"{}\"",
                    ["red", "green", "blue"][(index / 2) % 3]
                )
            } else {
                format!("select attrs.score == {}", (index / 2) % 100)
            }
        })
        .collect::<Vec<_>>();

    let relation_selectors = parse_all(relation_texts);
    let tag_selectors = parse_all(tag_texts);
    let attr_selectors = parse_all(attr_texts);

    let kinds = [
        (
            "relation",
            run_kind(
                "relation",
                &store,
                &state,
                Hash32::from_bytes([0x11; 32]),
                &principal,
                &limits,
                &relation_selectors,
            ),
        ),
        (
            "tag",
            run_kind(
                "tag",
                &store,
                &state,
                Hash32::from_bytes([0x22; 32]),
                &principal,
                &limits,
                &tag_selectors,
            ),
        ),
        (
            "attr_eq",
            run_kind(
                "attr_eq",
                &store,
                &state,
                Hash32::from_bytes([0x33; 32]),
                &principal,
                &limits,
                &attr_selectors,
            ),
        ),
    ];

    println!(
        "index-bench (synthetic in-memory state, same process): objects={OBJECTS} relations={RELATIONS} iterations={ITERATIONS} per kind"
    );
    println!(
        "{:<10} {:>16} {:>18} {:>16} {:>10}",
        "kind", "before_med_ms", "after_warm_med_ms", "after_cold_ms", "speedup"
    );
    let mut kind_json = serde_json::Map::new();
    for (_, result) in &kinds {
        let mut before = result.before_ms.clone();
        let mut after = result.after_ms.clone();
        let before_median = median(&mut before);
        let after_median = median(&mut after);
        let speedup = if after_median > 0.0 {
            before_median / after_median
        } else {
            f64::INFINITY
        };
        println!(
            "{:<10} {:>16.3} {:>18.3} {:>16.3} {:>9.2}x",
            result.name, before_median, after_median, result.cold_ms, speedup
        );
        kind_json.insert(
            result.name.to_owned(),
            json!({
                "iterations": result.after_ms.len(),
                "before_median_ms": before_median,
                "before_max_ms": before.last().copied().unwrap_or(0.0),
                "after_warm_median_ms": after_median,
                "after_warm_max_ms": after.last().copied().unwrap_or(0.0),
                "after_cold_ms": result.cold_ms,
                "matched_total": result.matched_total,
                "before_over_after": speedup,
            }),
        );
    }

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_secs();
    let receipt = json!({
        "schema": "weftfs.index-bench.v1",
        "label": "synthetic in-memory state built in the same process; resolver candidate selection only; not a disk, FUSE, or end-to-end measurement",
        "generated_unix_secs": generated,
        "revision": env("WEFT_REPO_WEFTFS"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "profile": "release",
        "environment": {
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "kernel": env("WEFT_COURT_KERNEL"),
            "memory_limit": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
        },
        "state": {
            "objects": OBJECTS,
            "relations": RELATIONS,
            "object_target_edges": RELATIONS / 2,
            "tagged_as_edges": RELATIONS / 2,
            "visible_objects": visible.len(),
            "snapshot_keys": "synthetic per-kind hashes (0x11/0x22/0x33 repeated)",
        },
        "method": {
            "before": "Store::resolve_state_scan: full authorized universe, no index",
            "after_warm": "Store::resolve_state: cached StateIndex narrows candidates, eval_expr confirms every candidate",
            "after_cold": "first indexed call for a fresh snapshot key, includes one StateIndex::build",
            "cache_capacity": 8,
            "statistic": "median of per-query wall-clock milliseconds",
            "equivalence": "indexed matched_count and ordered (object_id, version_id, content_id) verified against the scan for every query",
        },
        "kinds": kind_json,
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("index-bench-{generated}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!("receipt: {}", path.display());
}
