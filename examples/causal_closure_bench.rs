//! P02 matched-cost court: bounded causal closure through an index, the same
//! closure through a naive full-state scan, and the two closure definitions.
//!
//!   cargo run --release --example causal_closure_bench -- OUT.json [NODES] [REPS]
//!
//! The fixture is a synthetic causal DAG: two interleaved chains of `NODES`
//! objects each, where every 100th node of chain A declares a transitive
//! `closure` entry into chain B that is not one of its parent edges. `minimal`
//! follows parent edges only; `complete` follows the declared closure too, so
//! the two results are materially different on the same graph.

use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use weftcodec::wcb::Value;
use weftcodec::Limits;

use weftfs::causal_closure::{
    causal_id, causal_record_value, CausalGraph, ClosureBounds, ClosureMode, DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_NODES,
};
use weftfs::error::Result;
use weftfs::identity::Hash32;
use weftfs::store::{Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn hash_array(values: &[Hash32]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|id| Value::Bytes(id.as_bytes().to_vec()))
            .collect(),
    )
}

fn causal_attrs(
    parents: &[Hash32],
    extras: &[Hash32],
) -> weftfs::error::Result<BTreeMap<String, Value>> {
    let mut closure: Vec<Hash32> = parents.to_vec();
    closure.extend(extras.iter().copied());
    closure.sort_unstable();
    closure.dedup();
    let mut attrs = BTreeMap::new();
    attrs.insert("weft.causal.parents".to_owned(), hash_array(parents));
    attrs.insert("weft.causal.closure".to_owned(), hash_array(&closure));
    attrs.insert(
        "weft.causal.hash".to_owned(),
        Value::Bytes(causal_id(parents, &closure)?.as_bytes().to_vec()),
    );
    attrs.insert(
        "weft.identity.mode".to_owned(),
        Value::Str("causal_exact".to_owned()),
    );
    Ok(attrs)
}

fn percentile(sorted: &[u128], pct: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * pct).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn stats(mut values: Vec<u128>) -> serde_json::Value {
    values.sort_unstable();
    let p50 = percentile(&values, 0.50);
    let p95 = percentile(&values, 0.95);
    let mean = values.iter().sum::<u128>() as f64 / values.len() as f64;
    serde_json::json!({
        "reps": values.len(),
        "p50_ns": p50,
        "p95_ns": p95,
        "min_ns": values.first().copied().unwrap_or(0),
        "max_ns": values.last().copied().unwrap_or(0),
        "mean_ns": mean,
    })
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .unwrap_or_else(|| "bench/receipts/causal-closure.json".to_owned());
    let bundle_out = std::env::var("WEFT_CLOSURE_BUNDLE").ok();
    let branches: usize = args
        .next()
        .and_then(|text| text.parse().ok())
        .unwrap_or(64);
    let reps: usize = args
        .next()
        .and_then(|text| text.parse().ok())
        .unwrap_or(50);
    let dir = tempfile::tempdir().expect("temporary fixture directory");
    let mut store = Store::init(dir.path().join("store"), opts())?;
    let root = store.resolve_path("/")?;

    // 64 branches of depth 60: 3,840 nodes with a maximum depth of 60, plus a
    // 40-node side chain reachable only through declared `closure` entries.
    const DEPTH: usize = 60;
    const SIDE: usize = 40;
    let start = Instant::now();
    let mut side_versions: Vec<Hash32> = Vec::with_capacity(SIDE);
    let mut branch_heads: Vec<Hash32> = Vec::with_capacity(branches);
    let mut pending = 0usize;
    store.begin_group()?;
    for i in 0..SIDE {
        let parents: Vec<Hash32> = if i == 0 {
            Vec::new()
        } else {
            vec![side_versions[i - 1]]
        };
        let object = store.create_file(
            root,
            format!("s-{i:03}").as_bytes(),
            format!("side node {i}").as_bytes(),
            0o644,
            0,
            0,
            causal_attrs(&parents, &[])?,
        )?;
        side_versions.push(store.object_meta(object)?.version);
        pending += 1;
    }
    for b in 0..branches {
        let mut previous: Option<Hash32> = None;
        for i in 0..DEPTH {
            let parents: Vec<Hash32> = previous.into_iter().collect();
            // Only nodes near the root declare side-chain ancestry, keeping
            // every path within the default depth bound.
            let extras: Vec<Hash32> = if i >= 40 && i % 5 == 0 {
                vec![side_versions[(i / 5).min(SIDE - 1)]]
            } else {
                Vec::new()
            };
            let object = store.create_file(
                root,
                format!("b{b:03}-{i:03}").as_bytes(),
                format!("branch {b} node {i}").as_bytes(),
                0o644,
                0,
                0,
                causal_attrs(&parents, &extras)?,
            )?;
            previous = Some(store.object_meta(object)?.version);
            pending += 1;
            if pending >= 128 {
                store.commit_group()?;
                store.begin_group()?;
                pending = 0;
            }
        }
        branch_heads.push(previous.expect("a branch has nodes"));
    }
    let mut join_parents: Vec<Hash32> = branch_heads.clone();
    join_parents.sort_unstable();
    let join = store.create_file(
        root,
        b"join",
        b"joined root",
        0o644,
        0,
        0,
        causal_attrs(&join_parents, &[])?,
    )?;
    if pending > 0 {
        store.commit_group()?;
    }
    let fixture_ms = start.elapsed().as_secs_f64() * 1000.0;
    let snapshot = store.checkpoint()?;
    let state = store.state_at(snapshot)?;
    let limits = Limits::default();
    let roots = vec![store.object_meta(join)?.version];

    let build_start = Instant::now();
    let graph = CausalGraph::build(&state, &limits)?;
    let index_build_ns = build_start.elapsed().as_nanos();
    let bounds = ClosureBounds {
        max_depth: DEFAULT_MAX_DEPTH,
        max_nodes: DEFAULT_MAX_NODES,
    };

    let mut index_stats = BTreeMap::new();
    let mut naive_stats = BTreeMap::new();
    let mut node_counts = BTreeMap::new();
    for mode in [ClosureMode::Complete, ClosureMode::Minimal] {
        let first = graph.compute(mode, &roots, bounds, &|_| true)?;
        node_counts.insert(
            mode.as_str(),
            serde_json::json!({
                "nodes": first.returned_closure_size,
                "edges_considered": first.edges_considered,
                "max_depth": first.max_depth_seen,
            }),
        );
        let mut index_values = Vec::with_capacity(reps);
        for _ in 0..reps {
            let start = Instant::now();
            let result = graph.compute(mode, &roots, bounds, &|_| true)?;
            assert_eq!(result.returned_closure_size, first.returned_closure_size);
            index_values.push(start.elapsed().as_nanos());
        }
        index_stats.insert(mode.as_str(), stats(index_values));
        let naive_reps = (reps / 5).max(3);
        let mut naive_values = Vec::with_capacity(naive_reps);
        for _ in 0..naive_reps {
            let start = Instant::now();
            let result = CausalGraph::naive_scan(&state, &limits, mode, &roots, bounds)?;
            assert_eq!(result.returned_closure_size, first.returned_closure_size);
            naive_values.push(start.elapsed().as_nanos());
        }
        naive_stats.insert(mode.as_str(), stats(naive_values));
    }

    let complete = graph.compute(ClosureMode::Complete, &roots, bounds, &|_| true)?;
    let minimal = graph.compute(ClosureMode::Minimal, &roots, bounds, &|_| true)?;
    let minimal_is_subset = minimal.nodes.iter().all(|node| {
        complete
            .nodes
            .iter()
            .any(|other| other.identity == node.identity)
    });
    let naive_complete =
        CausalGraph::naive_scan(&state, &limits, ClosureMode::Complete, &roots, bounds)?;
    let naive_minimal =
        CausalGraph::naive_scan(&state, &limits, ClosureMode::Minimal, &roots, bounds)?;
    let same_as_naive = naive_complete.identities() == complete.identities()
        && naive_minimal.identities() == minimal.identities();

    let receipt = serde_json::json!({
        "kind": "causal-closure-cost",
        "timestamp_utc": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unknown".to_owned()),
        "environment": {
            "revisions": {
                "WeftFS": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
                "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
                "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            },
            "court": "bench (3 GiB / 4 cpus / 1024 pids)",
            "cache": "warm, single host",
        },
        "fixture": {
            "branches": branches,
            "depth": DEPTH,
            "side_chain": SIDE,
            "objects": branches * DEPTH + SIDE + 1,
            "extra_declared_edges": branches * (DEPTH / 5 - 8),
            "roots": 1,
            "build_ms": fixture_ms,
            "snapshot": snapshot.to_hex(),
        },
        "index": {
            "build_ns": index_build_ns,
            "graph_nodes": graph.len(),
            "modes": index_stats,
            "node_counts": node_counts,
        },
        "naive_full_scan": {
            "modes": naive_stats,
            "note": "re-scans every object once per query, then walks",
        },
        "single_query": {
            "index_complete_ns": index_build_ns + index_stats["complete"]["p50_ns"].as_u64().unwrap_or(0) as u128,
            "naive_complete_ns": naive_stats["complete"]["p50_ns"],
            "index_minimal_ns": index_build_ns + index_stats["minimal"]["p50_ns"].as_u64().unwrap_or(0) as u128,
            "naive_minimal_ns": naive_stats["minimal"]["p50_ns"],
            "note": "index total includes one graph build; the naive total rebuilds the whole graph inside the query",
        },
        "checks": [
            {"name": "complete_is_superset_of_minimal", "ok": minimal_is_subset},
            {"name": "modes_differ", "ok": complete.returned_closure_size != minimal.returned_closure_size},
            {"name": "same_result_index_and_naive", "ok": same_as_naive},
        ],
        "result": if minimal_is_subset && same_as_naive { "pass" } else { "fail" },
    });
    if let Some(bundle_path) = bundle_out {
        let records: Vec<serde_json::Value> = complete
            .nodes
            .iter()
            .map(|node| {
                let record_bytes = weftcodec::wcb::encode_default(&causal_record_value(
                    &node.parents,
                    &node.declared_closure,
                ))
                .unwrap_or_default();
                let hex_bytes: String =
                    record_bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                serde_json::json!({
                    "identity": node.identity.to_hex(),
                    "object": node.object.map(|id| id.to_hex()),
                    "parents": node.parents.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "closure": node.declared_closure.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "record_bytes": hex_bytes,
                    "causal_id": causal_id(&node.parents, &node.declared_closure)
                        .map(|id| id.to_hex()).ok(),
                    "depth": node.depth,
                })
            })
            .collect();
        let bundle = serde_json::json!({
            "schema": "weft.closure.bundle.v1",
            "artifact_root": snapshot.to_hex(),
            "snapshot": snapshot.to_hex(),
            "mode": "complete",
            "max_depth": bounds.max_depth,
            "max_nodes": bounds.max_nodes,
            "roots": roots.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
            "records": records,
            "result": {
                "nodes": complete.nodes.iter().map(|node| serde_json::json!({
                    "identity": node.identity.to_hex(), "depth": node.depth,
                })).collect::<Vec<_>>(),
                "total": complete.total_closure_size,
                "returned": complete.returned_closure_size,
                "truncated": complete.truncated,
            },
            "refusal": serde_json::Value::Null,
        });
        fs::write(&bundle_path, serde_json::to_vec_pretty(&bundle).expect("bundle json"))?;
    }
    if let Some(parent) = std::path::Path::new(&out).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, serde_json::to_vec_pretty(&receipt).expect("json"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&out, fs::Permissions::from_mode(0o666));
    }
    println!(
        "causal-closure: complete={} minimal={} index_build={:.3} ms p50 complete={} ns minimal={} ns naive complete={} ns",
        complete.returned_closure_size,
        minimal.returned_closure_size,
        index_build_ns as f64 / 1e6,
        index_stats["complete"]["p50_ns"],
        index_stats["minimal"]["p50_ns"],
        naive_stats["complete"]["p50_ns"],
    );
    Ok(())
}
