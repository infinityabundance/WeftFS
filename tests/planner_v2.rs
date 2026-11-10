//! Planner v2: the four-term objective and the measured uncertainty bound.
//!
//! ```text
//! C = storage(bytes) + write(encode) + retention(retained bytes) + reads * restore
//! ```
//!
//! These tests are deliberately small and synthetic. They prove the arithmetic
//! and the refusals; `tests/parent_graph_sla.rs` proves the baked rows match the
//! committed receipts, and the S13 court measures real restores.

use weftfs::cost_model::RestoreOverhead;
use weftfs::plan_graph::{self, EdgeCost, PlanOptions, VersionCost, VersionGraph};

fn edge(bytes: u64, decode_ns: u64, encode_ns: u64) -> EdgeCost {
    EdgeCost {
        bytes,
        decode_ns,
        encode_ns,
        ..EdgeCost::default()
    }
}

/// Two versions: v1 either restarts (cheap encode, larger frame) or takes the
/// adjacent delta (smaller frame, expensive encode).
fn write_trade_graph() -> VersionGraph {
    VersionGraph {
        versions: vec![
            VersionCost {
                name: "v0".into(),
                logical_bytes: 1_000,
                independent: edge(100, 10, 0),
                edges: vec![],
            },
            VersionCost {
                name: "v1".into(),
                logical_bytes: 1_000,
                independent: edge(100, 10, 0),
                edges: vec![Some(edge(90, 10, 1_000))],
            },
        ],
    }
}

#[test]
fn the_default_solver_uses_the_exact_oracle_when_it_is_cheap() {
    // v1/v2/v3 can all name v0; the adjacent edge is much larger than the
    // non-adjacent one, so the chain/restart DP cannot reach the optimum.
    let independent = EdgeCost {
        bytes: 1_000,
        decode_ns: 10,
        ..EdgeCost::default()
    };
    let cheap = EdgeCost {
        bytes: 10,
        decode_ns: 5,
        ..EdgeCost::default()
    };
    let adjacent = EdgeCost {
        bytes: 500,
        decode_ns: 5,
        ..EdgeCost::default()
    };
    let mut versions = vec![VersionCost {
        name: "v0".into(),
        logical_bytes: 1_000,
        independent,
        edges: vec![],
    }];
    for i in 1..4usize {
        let edges = (0..i)
            .map(|j| Some(if j + 1 == i { adjacent } else { cheap }))
            .collect();
        versions.push(VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent,
            edges,
        });
    }
    let graph = VersionGraph { versions };
    let auto = PlanOptions {
        solver: plan_graph::Solver::Auto,
        ..PlanOptions::default()
    };
    let exact = plan_graph::optimize(&graph, &auto).unwrap();
    assert_eq!(exact.parents, vec![None, Some(0), Some(0), Some(0)]);
    assert_eq!(exact.solver, plan_graph::Solver::BruteForce);

    let mut dp = auto.clone();
    dp.solver = plan_graph::Solver::Dp;
    let restricted = plan_graph::optimize(&graph, &dp).unwrap();
    assert_eq!(restricted.parents, vec![None, Some(0), Some(1), Some(2)]);
    assert!(exact.objective < restricted.objective);
}

#[test]
fn a_bytes_cheap_but_expensive_to_build_representation_is_priced() {
    // Bytes only: the 90-byte delta wins.
    let bytes_only = PlanOptions {
        lambda_read_ns: 0,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&write_trade_graph(), &bytes_only).unwrap();
    assert_eq!(plan.parents[1], Some(0));
    assert_eq!(plan.total_encode_ns, 1_000);
    assert_eq!(plan.write_cost_units, 0);
    assert_eq!(plan.predicted_bytes, 190);

    // One byte charged per encode nanosecond: the delta's 1,000 ns encode makes
    // the 100-byte independent frame cheaper. This is the term S13b lacked.
    let priced = PlanOptions {
        lambda_read_ns: 0,
        lambda_write_ns: 1,
        lambda_write_ns_set: true,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&write_trade_graph(), &priced).unwrap();
    assert_eq!(plan.parents[1], None);
    assert_eq!(plan.total_encode_ns, 0);
    assert_eq!(plan.predicted_bytes, 200);
    assert_eq!(plan.write_cost_units, 0);

    // Explicit write lambda is independent of the read lambda.
    let split = PlanOptions {
        lambda_read_ns: 0,
        lambda_write_ns: 0,
        lambda_write_ns_set: true,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&write_trade_graph(), &split).unwrap();
    assert_eq!(plan.parents[1], Some(0));
}

#[test]
fn the_write_lambda_defaults_to_the_read_lambda() {
    let options = PlanOptions {
        lambda_read_ns: 7,
        ..PlanOptions::default()
    };
    assert_eq!(options.effective_write_lambda(), 7);
    let explicit = PlanOptions {
        lambda_read_ns: 7,
        lambda_write_ns: 3,
        lambda_write_ns_set: true,
        ..PlanOptions::default()
    };
    assert_eq!(explicit.effective_write_lambda(), 3);
}

#[test]
fn retention_is_reported_and_weighted_explicitly() {
    let options = PlanOptions {
        lambda_read_ns: 0,
        lambda_retention_ns: 0,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&write_trade_graph(), &options).unwrap();
    assert_eq!(plan.retained_bytes, plan.predicted_bytes);
    assert_eq!(plan.retention_cost_units, 0);

    let held = PlanOptions {
        lambda_read_ns: 0,
        lambda_retention_ns: 2,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&write_trade_graph(), &held).unwrap();
    // Retention is proportional to retained bytes, so it reinforces storage and
    // does not change this ordering; it is reported and charged exactly.
    assert_eq!(plan.parents[1], Some(0));
    assert_eq!(
        plan.retention_cost_units,
        u128::from(plan.retained_bytes) * 2
    );
    assert_eq!(plan.objective, u128::from(plan.predicted_bytes) * 3);
}

fn uncertain_graph() -> VersionGraph {
    VersionGraph {
        versions: vec![VersionCost {
            name: "v0".into(),
            logical_bytes: 1_048_576,
            independent: EdgeCost {
                bytes: 100,
                decode_ns: 1_000,
                decode_uncertainty_ns: 500,
                ..EdgeCost::default()
            },
            edges: vec![],
        }],
    }
}

#[test]
fn an_sla_is_accepted_only_when_the_uncertainty_bound_fits() {
    let model = RestoreOverhead {
        per_version_ns: 0,
        per_tensor_ns: 0,
        per_mib_ns: 0,
        margin_ns_per_mib: 1_000,
    };
    let base = PlanOptions {
        lambda_read_ns: 0,
        restore_overhead: Some(model),
        ..PlanOptions::default()
    };
    let point = plan_graph::optimize(&uncertain_graph(), &base).unwrap();
    let margin = model.estimate_margin_ns(1_048_576);
    assert_eq!(margin, 1_000);
    assert_eq!(point.worst_restore_ns, 1_000);
    assert_eq!(point.worst_uncertainty_ns, 500);
    assert_eq!(point.worst_sla_bound_ns, 1_000 + 1_000 + 500);

    // Strictly inside the point + margin + uncertainty bound: refused.
    let refused = PlanOptions {
        sla_ns: Some(point.worst_sla_bound_ns - 1),
        ..base.clone()
    };
    let err = plan_graph::optimize(&uncertain_graph(), &refused).unwrap_err();
    assert_eq!(err.class().as_str(), "LimitExceeded");
    assert!(err.message().contains("uncertainty"), "{}", err.message());

    // At the bound: accepted, and the reported bound is what the SLA covers.
    let accepted = PlanOptions {
        sla_ns: Some(point.worst_sla_bound_ns),
        ..base
    };
    let plan = plan_graph::optimize(&uncertain_graph(), &accepted).unwrap();
    assert!(plan.sla_satisfied);
    assert_eq!(plan.worst_sla_bound_ns, point.worst_sla_bound_ns);
    assert_eq!(
        plan.predicted_sla_bound_ns[0],
        plan.predicted_chain_ns[0] + plan.predicted_margin_ns[0] + plan.predicted_uncertainty_ns[0]
    );
    assert_eq!(
        plan.hop_uncertainty_calibration,
        weftfs::cost_model::HOP_UNCERTAINTY_CALIBRATION
    );
}

#[test]
fn uncertainty_accumulates_along_a_chain_and_the_plan_is_deterministic() {
    let mut versions = Vec::new();
    for i in 0..4usize {
        let independent = EdgeCost {
            bytes: 100,
            decode_ns: 100,
            decode_uncertainty_ns: 10,
            ..EdgeCost::default()
        };
        let edges = (0..i)
            .map(|_| {
                Some(EdgeCost {
                    bytes: 20,
                    decode_ns: 100,
                    decode_uncertainty_ns: 10,
                    ..EdgeCost::default()
                })
            })
            .collect();
        versions.push(VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_048_576,
            independent,
            edges,
        });
    }
    let graph = VersionGraph { versions };
    let options = PlanOptions {
        lambda_read_ns: 0,
        max_depth: 8,
        ..PlanOptions::default()
    };
    let plan = plan_graph::optimize(&graph, &options).unwrap();
    assert_eq!(plan.parents, vec![None, Some(0), Some(1), Some(2)]);
    assert_eq!(plan.predicted_uncertainty_ns, vec![10, 20, 30, 40]);
    assert_eq!(plan.worst_uncertainty_ns, 40);
    assert_eq!(
        plan.predicted_sla_bound_ns[3],
        plan.predicted_chain_ns[3] + plan.predicted_margin_ns[3] + plan.predicted_uncertainty_ns[3]
    );
    assert_eq!(plan, plan_graph::optimize(&graph, &options).unwrap());
}
