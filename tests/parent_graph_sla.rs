//! S13c: the parent-graph planner prices hops with the representation the edge
//! actually uses and refuses an SLA it cannot keep under the measured margin.
//!
//! Two tests are the measured-not-declared checks: they read the committed
//! receipts and recompute the baked constants from the raw samples. A drifted
//! constant fails here, not in prose. The rest pin the fail-closed behaviour an
//! accepted plan promises.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value as Json;
use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::cost_model::{
    self, hop_coefficients, hop_estimate_ns, CostClass, CostModelKind, RestoreOverhead,
    RESTORE_OVERHEAD_CALIBRATION, SLA_MARGIN_CALIBRATION,
};
use weftfs::plan_graph::{self, hop_decode_ns, EdgeCost, PlanOptions, VersionCost, VersionGraph};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorStrategy;

/// The S13b restore-floor calibration (fit residuals and per-MiB overhead).
const OVERHEAD_RECEIPT: &str = "bench/receipts/s13b-restore-floor-20261108T230417Z.json";
/// The S13b trajectory court (the measured warm-versus-cold floor spread).
const COURT_RECEIPT: &str = "bench/receipts/parent-graph-20261108T232016Z.json";
/// The two independent S13c controlled-chain runs whose union is the hop rows.
const HOP_RECEIPTS: [&str; 2] = [
    "bench/receipts/s13c-chain-hop-20261109T100500Z.json",
    "bench/receipts/s13c-chain-hop-20261109T101000Z.json",
];

const MIB: u64 = 1_048_576;

fn read_json(path: &str) -> Json {
    let text = fs::read_to_string(Path::new(path))
        .unwrap_or_else(|err| panic!("{path}: {err}; the measurement receipt must exist"));
    serde_json::from_str(&text).expect("receipt is JSON")
}

fn edge(bytes: u64, decode_ns: u64) -> EdgeCost {
    EdgeCost {
        bytes,
        decode_ns,
        coupled_ns: decode_ns,
        coupled_tensors: 1,
        total_tensors: 1,
    }
}

#[test]
fn the_baked_sla_margin_is_derived_from_the_measured_receipts() {
    let calibration = read_json(OVERHEAD_RECEIPT);
    let court = read_json(COURT_RECEIPT);
    assert_eq!(calibration["schema"], "weft.s13b.restore-floor.v1");
    assert_eq!(court["schema"], "weft.s13b.parent-graph.v2");

    // (1) The fit-residual allowance: the largest absolute fitted residual over
    //     the 22 points whose tensors are at least 1 MiB (the regime every
    //     parent-graph inventory is in; the sub-256-KiB per-object artifact is
    //     excluded by the S13b refutation of a per-object term there).
    let per_mib_ns = calibration["fit"]["per_mib_ns"]
        .as_u64()
        .expect("fit.per_mib_ns");
    assert_eq!(per_mib_ns, RestoreOverhead::MEASURED.per_mib_ns);
    let points: BTreeMap<String, u64> = calibration["points"]
        .as_array()
        .expect("points")
        .iter()
        .map(|point| {
            (
                point["id"].as_str().expect("point id").to_owned(),
                point["per_tensor_bytes"]
                    .as_u64()
                    .expect("per_tensor_bytes"),
            )
        })
        .collect();
    let mut worst_ppm = 0i64;
    let mut in_regime = 0u64;
    for row in calibration["fit"]["residuals"]
        .as_array()
        .expect("residuals")
    {
        let id = row["id"].as_str().expect("residual id");
        let bytes = points[id];
        if bytes < MIB {
            continue;
        }
        in_regime += 1;
        worst_ppm = worst_ppm.max(row["error_ppm"].as_i64().expect("error_ppm").abs());
    }
    assert!(in_regime >= 14, "only {in_regime} in-regime fit points");
    let fit_allowance =
        (u128::from(worst_ppm as u64) * u128::from(per_mib_ns) + 999_999) / 1_000_000;

    // (2) The measured cold-versus-warm under-prediction of the floor in the
    //     trajectory court, per logical MiB.
    let optimizer = court["realizations"]
        .as_array()
        .expect("realizations")
        .iter()
        .find(|row| row["label"] == "optimizer")
        .expect("the all-independent realization");
    let predicted = optimizer["predicted_restore_worst_ns"]
        .as_u64()
        .expect("predicted_restore_worst_ns");
    let measured = (optimizer["restore_worst_ms"]
        .as_f64()
        .expect("restore_worst_ms")
        * 1_000_000.0)
        .round() as u64;
    assert!(
        measured > predicted,
        "the S13b court must have measured the cold floor above the prediction"
    );
    let logical_bytes = court["graph"]["versions"][0]["logical_bytes"]
        .as_u64()
        .expect("logical_bytes");
    let deficit = measured - predicted;
    let cold_allowance = (u128::from(deficit) * u128::from(MIB) + u128::from(logical_bytes) - 1)
        / u128::from(logical_bytes);

    let derived = u64::try_from(fit_allowance + cold_allowance).unwrap();
    assert_eq!(
        derived,
        RestoreOverhead::MEASURED.margin_ns_per_mib,
        "the baked SLA margin drifted from its measured derivation \
         (fit {worst_ppm} ppm over {in_regime} points + {deficit} ns cold deficit over {logical_bytes} bytes)"
    );
    assert_eq!(SLA_MARGIN_CALIBRATION, "weftfs-s13c/sla-margin/v1");
    assert_eq!(
        RESTORE_OVERHEAD_CALIBRATION,
        "weftfs-s13b/restore-overhead/v1"
    );
}

#[test]
fn the_baked_hop_rows_are_the_measured_chain_union() {
    // Through-origin least squares over every (bytes, hop_ns) sample of the
    // class, unioned across the two independent runs.
    let mut samples: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for path in HOP_RECEIPTS {
        let receipt = read_json(path);
        assert_eq!(receipt["schema"], "weft.s13c.chain-hop.v1");
        assert_eq!(receipt["configuration"]["chains_only"], true);
        for row in receipt["hop_fit"].as_array().expect("hop_fit") {
            let strategy = row["strategy"].as_str().expect("strategy").to_owned();
            for hop in row["measured_hops"].as_array().expect("measured_hops") {
                samples.entry(strategy.clone()).or_default().push((
                    hop["element_bytes"].as_u64().expect("element_bytes"),
                    hop["hop_ns"].as_u64().expect("hop_ns"),
                ));
            }
        }
    }
    for (strategy, rows) in &samples {
        assert!(
            rows.len() >= 5,
            "{strategy} has only {} hop samples",
            rows.len()
        );
    }
    let derived = |strategy: &str| -> u64 {
        let rows = &samples[strategy];
        let numerator: u128 = rows
            .iter()
            .map(|(bytes, ns)| u128::from(*bytes) * u128::from(*ns))
            .sum();
        let denominator: u128 = rows
            .iter()
            .map(|(bytes, _)| u128::from(*bytes) * u128::from(*bytes))
            .sum();
        let scaled = numerator * u128::from(MIB);
        let (quotient, remainder) = (scaled / denominator, scaled % denominator);
        u64::try_from(quotient + u128::from(2 * remainder >= denominator)).unwrap()
    };
    assert_eq!(
        hop_coefficients(CostClass::SparseDelta).ns_per_mib,
        derived("sparse-delta"),
        "the baked sparse-delta hop row drifted from its measured samples"
    );
    assert_eq!(
        hop_coefficients(CostClass::WeightDelta).ns_per_mib,
        derived("weight-delta"),
        "the baked weight-delta hop row drifted from its measured samples"
    );
    // A hop increment carries no per-restore fixed work: both versions on the
    // edge pay that work, so it cancels.
    assert_eq!(hop_coefficients(CostClass::SparseDelta).fixed_ns, 0);
    assert_eq!(hop_coefficients(CostClass::WeightDelta).fixed_ns, 0);
}

#[test]
fn each_representation_is_priced_by_its_own_hop_row() {
    let bytes = 8 * MIB;
    let sparse = hop_decode_ns(TensorStrategy::SparseDelta, bytes);
    let weight = hop_decode_ns(TensorStrategy::WeightDelta, bytes);
    // The S13b defect was that both used the weight-delta decode row.
    assert_ne!(sparse, weight, "sparse and weight hops share a row");
    assert_eq!(sparse, hop_estimate_ns(CostClass::SparseDelta, bytes));
    assert_eq!(weight, hop_estimate_ns(CostClass::WeightDelta, bytes));

    // Non-delta classes keep the measured decode rows unchanged: this table is
    // a delta-hop correction, not a re-pricing of independent representations.
    for class in [
        CostClass::Raw,
        CostClass::Zstd,
        CostClass::FloatTensor,
        CostClass::TensorSharded,
        CostClass::TensorTiled,
        CostClass::SemanticDelta,
    ] {
        assert_eq!(
            hop_estimate_ns(class, bytes),
            cost_model::estimate_ns(class, bytes, CostModelKind::Measured),
            "{} must keep the measured decode row",
            class.as_str()
        );
    }
    // A zstd-patch edge is a delta and shares the measured weight-delta hop
    // row (no zstd-patch hop was measured; documented, not hidden).
    assert_eq!(hop_decode_ns(TensorStrategy::ZstdPatch, bytes), weight);
    // Mode 18 keeps its own measured row.
    assert_eq!(
        hop_decode_ns(TensorStrategy::FloatTiledStreams, bytes),
        cost_model::estimate_streams_ns(bytes)
    );
}

/// A three-version graph whose versions are 1 MiB each.
fn one_mib_graph() -> VersionGraph {
    let logical = 1_048_576u64;
    VersionGraph {
        versions: vec![
            VersionCost {
                name: "v0".into(),
                logical_bytes: logical,
                independent: edge(100, 10),
                edges: vec![],
            },
            VersionCost {
                name: "v1".into(),
                logical_bytes: logical,
                independent: edge(90, 9),
                edges: vec![Some(edge(20, 8))],
            },
        ],
    }
}

#[test]
fn an_sla_that_only_fits_without_the_margin_is_refused() {
    let graph = one_mib_graph();
    let options = PlanOptions {
        lambda_read_ns: 0,
        sla_ns: None,
        ..PlanOptions::default()
    };
    let point = plan_graph::optimize(&graph, &options).unwrap();
    let point_worst = point.worst_restore_ns;
    let bound = point.worst_sla_bound_ns;
    let margin = RestoreOverhead::MEASURED.estimate_margin_ns(1_048_576);
    assert!(margin > 0);
    assert_eq!(bound, point_worst + margin);
    assert_eq!(
        point.predicted_sla_bound_ns[0],
        point.predicted_chain_ns[0] + margin
    );

    // An SLA strictly between the point prediction and the bound fits only if
    // the margin is ignored. The planner must refuse it, name the margin, and
    // never create a store.
    let midpoint = point_worst + margin / 2;
    let dir = tempdir().unwrap();
    let refused = PlanOptions {
        sla_ns: Some(midpoint),
        ..PlanOptions::default()
    };
    let err = plan_graph::optimize(&graph, &refused).unwrap_err();
    assert_eq!(err.class().as_str(), "LimitExceeded");
    assert!(err.message().contains("SLA"));
    assert!(
        err.message().contains("margin"),
        "the refusal must name the margin: {}",
        err.message()
    );

    // The same SLA at the bound is accepted, and the bound (not the point
    // prediction) is what the plan promises.
    let accepted = PlanOptions {
        sla_ns: Some(bound),
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&graph, &accepted).unwrap();
    assert!(plan.sla_satisfied);
    assert_eq!(plan.worst_sla_bound_ns, bound);
    assert_eq!(plan.sla_margin_calibration, SLA_MARGIN_CALIBRATION);
    // Nothing was written by any of the refusals above.
    assert!(!dir.path().join("store").exists());
}

#[test]
fn a_model_without_a_margin_cannot_claim_an_sla() {
    let graph = one_mib_graph();
    let options = PlanOptions {
        sla_ns: Some(60_000_000),
        restore_overhead: Some(RestoreOverhead {
            margin_ns_per_mib: 0,
            ..RestoreOverhead::MEASURED
        }),
        ..PlanOptions::default()
    };
    let err = plan_graph::optimize(&graph, &options).unwrap_err();
    assert_eq!(err.class().as_str(), "InvalidArgument");
    assert!(err.message().contains("margin"));

    // A diagnostic model is still usable when no SLA is claimed.
    let diagnostic = PlanOptions {
        sla_ns: None,
        restore_overhead: Some(RestoreOverhead::ZERO),
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&graph, &diagnostic).unwrap();
    assert_eq!(plan.worst_sla_bound_ns, plan.worst_restore_ns);
}

#[test]
fn the_same_graph_and_options_always_give_the_same_plan() {
    let graph = one_mib_graph();
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
// Store round trip: edges priced by the stored representation, roots unchanged
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

/// One 512 KiB bf16 tensor; later versions differ in four bytes, so a delta is
/// the natural representation.
fn container(dir: &Path, version: u64) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let elements = 262_144u64;
    let mut data = random_bytes(elements as usize * 2, 0x5eed_1234);
    if version > 0 {
        for offset in [0usize, 16, 4000, 500_000] {
            data[offset] = data[offset].wrapping_add(1);
        }
    }
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "bf16",
            shape: &[elements],
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
fn graph_edges_use_the_hop_rows_and_a_plan_round_trips_unchanged() {
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
    let graph = probe
        .price_parent_graph(
            &["v0".to_owned(), "v1".to_owned()],
            &plan_graph::PriceOptions {
                plan: weftfs::tensor_store::TensorPlanOptions {
                    semantic_search: false,
                    ..weftfs::tensor_store::TensorPlanOptions::default()
                },
                ..plan_graph::PriceOptions::default()
            },
        )
        .unwrap();

    // The independent cost is priced from the stored representation through the
    // hop table. For a zstd/raw/float-tensor representation that is the measured
    // decode row, so this asserts the routing, not a value change.
    let manifest = probe.load_checkpoint("v1").unwrap();
    let tensor = &manifest.tensors[0];
    let meta = probe.tensor_metadata(tensor.shards[0].object_id).unwrap();
    let strategy = TensorStrategy::parse(&meta.strategy).unwrap();
    assert_eq!(
        graph.versions[1].independent.decode_ns,
        hop_decode_ns(strategy, tensor.element_bytes),
        "the graph did not price the stored representation {} with the hop table",
        meta.strategy
    );

    // The delta edge is priced by the delta class the edge actually uses. Both
    // candidate delta classes have their own measured hop row, and neither
    // equals the write-time decode row the S13b model over-priced.
    let delta_edge = graph.versions[1].edges[0].expect("v1 must be able to name v0");
    let bytes = tensor.element_bytes;
    let sparse = hop_decode_ns(TensorStrategy::SparseDelta, bytes);
    let weight = hop_decode_ns(TensorStrategy::WeightDelta, bytes);
    assert_ne!(sparse, weight);
    assert!(
        delta_edge.decode_ns == sparse || delta_edge.decode_ns == weight,
        "edge decode {} is neither the sparse ({sparse}) nor the weight ({weight}) hop price",
        delta_edge.decode_ns
    );
    let write_row = cost_model::estimate_ns(CostClass::WeightDelta, bytes, CostModelKind::Measured);
    assert_ne!(delta_edge.decode_ns, write_row);

    // Optimise with a generous SLA and round-trip the plan into a new store.
    let plan = plan_graph::optimize(
        &graph,
        &PlanOptions {
            lambda_read_ns: 0,
            sla_ns: Some(60_000_000),
            ..PlanOptions::default()
        },
    )
    .unwrap();

    let out_dir = dir.path().join("out");
    let mut out = Store::init(&out_dir, opts()).unwrap();
    let sources = vec![("v0".to_owned(), v0), ("v1".to_owned(), v1)];
    out.write_parent_graph(&sources, &plan, &write).unwrap();

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
