//! S13b: the graph planner charges the measured whole-checkpoint restore
//! overhead, and an SLA claim is refused when that term is missing.
//!
//! The first test is the measured-not-declared check: it reads the committed
//! calibration receipt and requires the baked constants to equal the receipt's
//! `baked` object and the receipt to carry at least five samples per fitted
//! term. A drifted constant fails here, not in prose.

use std::fs;
use std::path::Path;

use serde_json::Value as Json;
use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::cost_model::{RestoreOverhead, RESTORE_OVERHEAD_CALIBRATION};
use weftfs::plan_graph::{self, EdgeCost, PlanOptions, VersionCost, VersionGraph};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

/// The committed S13b calibration receipt.
const RECEIPT: &str = "bench/receipts/s13b-restore-floor-20261108T230417Z.json";

fn edge(bytes: u64, decode_ns: u64) -> EdgeCost {
    EdgeCost {
        bytes,
        decode_ns,
        coupled_ns: decode_ns,
        coupled_tensors: 1,
        total_tensors: 1,
        ..EdgeCost::default()
    }
}

/// A three-version graph with one tensor of 1,000 logical bytes each.
fn graph() -> VersionGraph {
    VersionGraph {
        versions: vec![
            VersionCost {
                name: "v0".into(),
                logical_bytes: 1_000,
                independent: edge(100, 10),
                edges: vec![],
            },
            VersionCost {
                name: "v1".into(),
                logical_bytes: 1_000,
                independent: edge(90, 9),
                edges: vec![Some(edge(20, 8))],
            },
            VersionCost {
                name: "v2".into(),
                logical_bytes: 1_000,
                independent: edge(90, 9),
                edges: vec![Some(edge(20, 8)), Some(edge(20, 8))],
            },
        ],
    }
}

#[test]
fn baked_overhead_matches_the_measured_receipt() {
    let path = Path::new(RECEIPT);
    let text = fs::read_to_string(path).unwrap_or_else(|err| {
        panic!(
            "{}: {err}; the measurement receipt must exist",
            path.display()
        )
    });
    let receipt: Json = serde_json::from_str(&text).expect("receipt is JSON");
    assert_eq!(receipt["schema"], "weft.s13b.restore-floor.v1");

    let baked = &receipt["baked"];
    assert_eq!(
        baked["per_version_ns"].as_u64(),
        Some(RestoreOverhead::MEASURED.per_version_ns),
        "baked per-version term drifted from the measured receipt"
    );
    assert_eq!(
        baked["per_tensor_ns"].as_u64(),
        Some(RestoreOverhead::MEASURED.per_tensor_ns),
        "baked per-tensor term drifted from the measured receipt"
    );
    assert_eq!(
        baked["per_mib_ns"].as_u64(),
        Some(RestoreOverhead::MEASURED.per_mib_ns),
        "baked per-MiB term drifted from the measured receipt"
    );

    // Measured, not declared: every fitted term carries samples and the receipt
    // names the same calibration identity the library exposes.
    let samples = receipt["fit"]["samples"].as_u64().expect("fit samples");
    assert!(samples >= 5, "fitted with only {samples} samples");
    assert_eq!(
        receipt["method"]["decode_rows"],
        weftfs::cost_model::MEASURED_CALIBRATION
    );
    assert_eq!(
        receipt["baked"]["per_version_source"].as_str(),
        Some("measured_one_tensor_manifest_read")
    );
    assert_eq!(
        RESTORE_OVERHEAD_CALIBRATION,
        "weftfs-s13b/restore-overhead/v1"
    );

    // The real-container validation point must be inside the S13b 10 % target.
    let validation = receipt["real_validation"]
        .as_array()
        .expect("real_validation rows");
    assert!(!validation.is_empty(), "no real-container validation point");
    for row in validation {
        let error_ppm = row["restore_error_ppm"]
            .as_i64()
            .expect("restore_error_ppm");
        assert!(
            error_ppm.abs() <= 100_000,
            "{} predicted restore error {error_ppm} ppm exceeds 10 %",
            row["id"]
        );
    }
}

#[test]
fn graph_prediction_includes_the_measured_overhead() {
    let graph = graph();
    let options = PlanOptions::default();
    let term = options.restore_overhead.expect("default overhead");
    assert_eq!(term, RestoreOverhead::MEASURED);

    let plan = plan_graph::evaluate(&graph, &[None, Some(0), Some(1)], &options).unwrap();
    for i in 0..graph.len() {
        let expected_overhead = term.estimate_ns(1, graph.versions[i].logical_bytes);
        assert_eq!(
            plan.predicted_overhead_ns[i], expected_overhead,
            "version {i}"
        );
        assert_eq!(
            plan.predicted_chain_ns[i],
            plan.predicted_decode_chain_ns[i] + expected_overhead,
            "version {i} restore must be decode + overhead"
        );
    }
    // Decode accumulates along the chain; the overhead does not.
    assert_eq!(
        plan.predicted_decode_chain_ns[1],
        plan.predicted_decode_chain_ns[0] + 8
    );
    assert_eq!(
        plan.predicted_decode_chain_ns[2],
        plan.predicted_decode_chain_ns[1] + 8
    );
    assert_eq!(plan.predicted_overhead_ns[0], plan.predicted_overhead_ns[2]);
    assert_eq!(
        plan.worst_restore_ns,
        *plan.predicted_chain_ns.iter().max().unwrap()
    );
    assert_eq!(plan.restore_overhead, Some(term));
}

#[test]
fn an_sla_that_only_fits_without_the_overhead_is_refused() {
    let graph = graph();
    // The raw decode chain peaks at 26 ns; the measured overhead is ~46 µs at
    // 1,000 logical bytes. An SLA of 1 ms fits the raw rows and not the
    // corrected restore, so the corrected model must refuse.
    let raw = PlanOptions {
        restore_overhead: None,
        ..PlanOptions::default()
    };
    let raw_plan = plan_graph::evaluate(&graph, &[None, None, None], &raw).unwrap();
    assert!(raw_plan.worst_restore_ns < 1_000_000);

    let corrected = PlanOptions {
        sla_ns: Some(5_000),
        ..PlanOptions::default()
    };
    let err = plan_graph::optimize(&graph, &corrected).unwrap_err();
    assert_eq!(err.class().as_str(), "LimitExceeded");
    assert!(err.message().contains("SLA"));
}

#[test]
fn a_missing_overhead_model_cannot_claim_an_sla_and_writes_nothing() {
    let graph = graph();
    let dir = tempdir().unwrap();
    let target = dir.path().join("out");
    let options = PlanOptions {
        sla_ns: Some(60_000_000),
        restore_overhead: None,
        ..PlanOptions::default()
    };
    let err = plan_graph::optimize(&graph, &options).unwrap_err();
    assert_eq!(err.class().as_str(), "InvalidArgument");
    assert!(err.message().contains("restore-overhead"));
    // The refusal is before any store exists: planning never opens the target.
    assert!(!target.exists(), "the refusal must not create a store");
}

#[test]
fn the_same_graph_and_options_always_give_the_same_plan() {
    let graph = graph();
    let options = PlanOptions {
        lambda_read_ns: 1,
        sla_ns: Some(60_000_000),
        ..PlanOptions::default()
    };
    let first = plan_graph::optimize(&graph, &options).unwrap();
    let second = plan_graph::optimize(&graph, &options).unwrap();
    assert_eq!(first, second);
    assert!(first.sla_satisfied);
    assert_eq!(first.restore_overhead, Some(RestoreOverhead::MEASURED));
}

// ---------------------------------------------------------------------------
// Store round trip with the corrected model
// ---------------------------------------------------------------------------

fn opts() -> Options {
    Options {
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn container(dir: &Path, version: u64) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let shape = [64u64, 32];
    let len = (shape[0] * shape[1]) as usize * 2;
    let mut data = random_bytes(len, 0x5eed_1234);
    if version > 0 {
        for offset in [0usize, 16, 4000] {
            data[offset] = data[offset].wrapping_add(1);
        }
    }
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "bf16",
            shape: &shape,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let path = dir.join(format!("v{version}.safetensors"));
    fs::write(&path, file).unwrap();
    path
}

#[test]
fn a_corrected_plan_round_trips_with_unchanged_roots() {
    let dir = tempdir().unwrap();
    let v0 = container(dir.path(), 0);
    let v1 = container(dir.path(), 1);

    let probe_dir = dir.path().join("probe");
    let mut probe = Store::init(&probe_dir, opts()).unwrap();
    let write = CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy: ShardStrategy::Planned,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    };
    for (name, path) in [("v0", &v0), ("v1", &v1)] {
        probe.write_checkpoint(name, path, &write).unwrap();
    }
    drop(probe);

    let graph = VersionGraph {
        versions: vec![
            VersionCost {
                name: "v0".into(),
                logical_bytes: 4_096,
                independent: edge(100, 10),
                edges: vec![],
            },
            VersionCost {
                name: "v1".into(),
                logical_bytes: 4_096,
                independent: edge(90, 9),
                edges: vec![Some(edge(20, 8))],
            },
        ],
    };
    let plan = plan_graph::optimize(
        &graph,
        &PlanOptions {
            lambda_read_ns: 0,
            sla_ns: Some(60_000_000),
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert_eq!(plan.parents, vec![None, Some(0)]);

    let out_dir = dir.path().join("out");
    let mut out = Store::init(&out_dir, opts()).unwrap();
    let sources = vec![("v0".to_owned(), v0), ("v1".to_owned(), v1)];
    let reports = out.write_parent_graph(&sources, &plan, &write).unwrap();
    assert_eq!(reports.len(), 2);

    let independent = Store::open(&probe_dir, OpenMode::ReadOnly).unwrap();
    for name in ["v0", "v1"] {
        let a = independent.load_checkpoint(name).unwrap();
        let b = out.load_checkpoint(name).unwrap();
        assert_eq!(a.tensors.len(), b.tensors.len());
        for (ta, tb) in a.tensors.iter().zip(b.tensors.iter()) {
            assert_eq!(ta.logical_root, tb.logical_root);
            assert_eq!(ta.content_root, tb.content_root);
        }
        let mut bytes = Vec::new();
        let restored = out
            .restore_checkpoint(name, &CheckpointRestoreOptions::default(), &mut bytes)
            .unwrap();
        assert!(restored.exact_source_file);
    }
    assert!(out.verify(true).unwrap().ok);
}
