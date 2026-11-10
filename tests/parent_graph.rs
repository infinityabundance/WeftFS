//! Parent-graph optimizer: pure-solver properties and a store round trip.
//!
//! The pure tests pin the stated state space and the deterministic tie-break.
//! The store test proves the decision surface is the existing
//! `causal_checkpoint` path and that every chosen version keeps the same
//! per-tensor `LogicalTensorRoot` and content root as an independent write.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::plan_graph::{self, PlanOptions, PriceOptions, Solver};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::TensorPlanOptions;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
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

/// Two bf16 tensors; `version == 1` changes a few words so a delta is cheaper.
fn write_container(dir: &Path, version: u64) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let shape = [64u64, 32];
    let len = (shape[0] * shape[1]) as usize * 2;
    let mut data = random_bytes(len, 0x5eed_1234);
    if version > 0 {
        for offset in [0usize, 16, 4000] {
            data[offset] = data[offset].wrapping_add(1);
        }
    }
    let bias_len = 32 * 2;
    let bias = random_bytes(bias_len, 0xb1a5);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: "weight",
                dtype: "bf16",
                shape: &shape,
                data: &data,
            },
            SafetensorsEntry {
                name: "bias",
                dtype: "bf16",
                shape: &[32],
                data: &bias,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join(format!("version-{version}.safetensors"));
    fs::write(&path, &file).unwrap();
    path
}

fn write_options() -> CheckpointWriteOptions {
    CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy: ShardStrategy::Planned,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    }
}

#[test]
fn dp_matches_the_oracle_when_chain_or_restart_is_optimal() {
    // Every non-adjacent edge is expensive, so the optimum is in the DP's
    // stated space and the two solvers must agree exactly, including the
    // tie-break.
    let mut versions = Vec::new();
    for i in 0..6usize {
        let independent = plan_graph::EdgeCost {
            bytes: 100,
            decode_ns: 100,
            coupled_ns: 0,
            coupled_tensors: 0,
            total_tensors: 1,
            ..plan_graph::EdgeCost::default()
        };
        let edges = (0..i)
            .map(|j| {
                Some(plan_graph::EdgeCost {
                    bytes: if j + 1 == i { 10 } else { 1_000 },
                    decode_ns: 40,
                    coupled_ns: 40,
                    coupled_tensors: 1,
                    total_tensors: 1,
                    ..plan_graph::EdgeCost::default()
                })
            })
            .collect();
        versions.push(plan_graph::VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent,
            edges,
        });
    }
    let graph = plan_graph::VersionGraph { versions };
    let options = PlanOptions {
        lambda_read_ns: 1,
        reads_per_version: 1,
        max_depth: 8,
        ..PlanOptions::default()
    };
    let mut options = options;
    options.solver = Solver::Dp;
    let dp = plan_graph::optimize(&graph, &options).unwrap();
    options.solver = Solver::BruteForce;
    let brute = plan_graph::optimize(&graph, &options).unwrap();
    assert_eq!(dp.parents, brute.parents);
    assert_eq!(dp.objective, brute.objective);
    assert_eq!(dp.depths, brute.depths);
}

#[test]
fn the_oracle_can_prefer_a_non_adjacent_parent() {
    // A crafted graph where version 3 is tiny against version 0. This records
    // the DP's stated limitation instead of hiding it.
    let mut versions = Vec::new();
    let edge = |bytes: u64| plan_graph::EdgeCost {
        bytes,
        decode_ns: 1,
        coupled_ns: 1,
        coupled_tensors: 1,
        total_tensors: 1,
        ..plan_graph::EdgeCost::default()
    };
    for i in 0..4usize {
        versions.push(plan_graph::VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent: edge(100),
            edges: (0..i)
                .map(|j| Some(edge(if i == 3 && j == 0 { 1 } else { 100 })))
                .collect(),
        });
    }
    let graph = plan_graph::VersionGraph { versions };
    let mut options = PlanOptions {
        lambda_read_ns: 0,
        max_depth: 8,
        ..PlanOptions::default()
    };
    options.solver = Solver::BruteForce;
    let brute = plan_graph::optimize(&graph, &options).unwrap();
    assert_eq!(brute.parents[3], Some(0));
    options.solver = Solver::Dp;
    let dp = plan_graph::optimize(&graph, &options).unwrap();
    assert_ne!(dp.parents, brute.parents);
    assert!(dp.objective > brute.objective);
}

#[test]
fn determinism_is_byte_stable() {
    let mut versions = Vec::new();
    for i in 0..5usize {
        let independent = plan_graph::EdgeCost {
            bytes: 100,
            decode_ns: 50,
            coupled_ns: 0,
            coupled_tensors: 0,
            total_tensors: 1,
            ..plan_graph::EdgeCost::default()
        };
        versions.push(plan_graph::VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent,
            edges: (0..i)
                .map(|_| {
                    Some(plan_graph::EdgeCost {
                        bytes: 30,
                        decode_ns: 20,
                        coupled_ns: 20,
                        coupled_tensors: 1,
                        total_tensors: 1,
                        ..plan_graph::EdgeCost::default()
                    })
                })
                .collect(),
        });
    }
    let graph = plan_graph::VersionGraph { versions };
    let options = PlanOptions {
        lambda_read_ns: 2,
        ..PlanOptions::default()
    };
    let first = plan_graph::optimize(&graph, &options).unwrap();
    let second = plan_graph::optimize(&graph, &options).unwrap();
    assert_eq!(first, second);
}

#[test]
fn depth_bound_holds_for_every_solver() {
    let mut versions = Vec::new();
    for i in 0..8usize {
        let independent = plan_graph::EdgeCost {
            bytes: 1_000,
            decode_ns: 10,
            coupled_ns: 0,
            coupled_tensors: 0,
            total_tensors: 1,
            ..plan_graph::EdgeCost::default()
        };
        versions.push(plan_graph::VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent,
            edges: (0..i)
                .map(|_| {
                    Some(plan_graph::EdgeCost {
                        bytes: 10,
                        decode_ns: 10,
                        coupled_ns: 10,
                        coupled_tensors: 1,
                        total_tensors: 1,
                        ..plan_graph::EdgeCost::default()
                    })
                })
                .collect(),
        });
    }
    let graph = plan_graph::VersionGraph { versions };
    for solver in [Solver::Dp, Solver::Greedy, Solver::BruteForce] {
        for max_depth in 0..=8u32 {
            let options = PlanOptions {
                lambda_read_ns: 0,
                max_depth,
                solver,
                ..PlanOptions::default()
            };
            let plan = plan_graph::optimize(&graph, &options).unwrap();
            assert!(
                plan.max_depth <= max_depth,
                "{solver:?} max_depth {max_depth} produced {}",
                plan.max_depth
            );
        }
    }
}

#[test]
fn sla_refusal_is_typed_and_does_not_degrade() {
    let mut versions = Vec::new();
    for i in 0..3usize {
        let independent = plan_graph::EdgeCost {
            bytes: 100,
            decode_ns: 100,
            coupled_ns: 0,
            coupled_tensors: 0,
            total_tensors: 1,
            ..plan_graph::EdgeCost::default()
        };
        versions.push(plan_graph::VersionCost {
            name: format!("v{i}"),
            logical_bytes: 1_000,
            independent,
            edges: (0..i)
                .map(|_| {
                    Some(plan_graph::EdgeCost {
                        bytes: 10,
                        decode_ns: 100,
                        coupled_ns: 100,
                        coupled_tensors: 1,
                        total_tensors: 1,
                        ..plan_graph::EdgeCost::default()
                    })
                })
                .collect(),
        });
    }
    let graph = plan_graph::VersionGraph { versions };
    let options = PlanOptions {
        sla_ns: Some(50),
        ..PlanOptions::default()
    };
    let error = plan_graph::optimize(&graph, &options).unwrap_err();
    assert_eq!(error.class().as_str(), "LimitExceeded");
    assert!(error.message().contains("SLA"));
}

#[test]
fn store_round_trip_keeps_every_chosen_root() {
    let dir = tempdir().unwrap();
    let v0 = write_container(dir.path(), 0);
    let v1 = write_container(dir.path(), 1);
    let v2 = write_container(dir.path(), 0);

    // Independent probe store.
    let probe_dir = dir.path().join("probe");
    let mut probe = Store::init(&probe_dir, opts()).unwrap();
    let write = write_options();
    for (name, path) in [("v0", &v0), ("v1", &v1), ("v2", &v2)] {
        probe.write_checkpoint(name, path, &write).unwrap();
    }
    let names = vec!["v0".to_owned(), "v1".to_owned(), "v2".to_owned()];
    let graph = probe
        .price_parent_graph(
            &names,
            &PriceOptions {
                plan: TensorPlanOptions {
                    semantic_search: false,
                    ..TensorPlanOptions::default()
                },
                max_tensors: 10_000,
                shards: 1,
            },
        )
        .unwrap();
    drop(probe);

    // A delta against v0 must be compatible and cheaper than independent bytes.
    let edge = graph.versions[1].edges[0].expect("v1 must be delta-compatible with v0");
    assert!(edge.coupled_tensors >= 1);
    assert!(edge.bytes <= graph.versions[1].independent.bytes);

    let plan = plan_graph::optimize(
        &graph,
        &PlanOptions {
            lambda_read_ns: 0,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        plan.parents[1],
        Some(0),
        "bytes-only plan must take the delta"
    );

    // Realise the plan in a fresh store through `causal_checkpoint`.
    let out_dir = dir.path().join("out");
    let mut out = Store::init(&out_dir, opts()).unwrap();
    let sources = vec![
        ("v0".to_owned(), v0.clone()),
        ("v1".to_owned(), v1.clone()),
        ("v2".to_owned(), v2.clone()),
    ];
    let reports = out.write_parent_graph(&sources, &plan, &write).unwrap();
    assert_eq!(reports.len(), 3);

    // Every chosen version must keep the same per-tensor roots as the
    // independent write, and restore exactly.
    let independent = Store::open(&probe_dir, weftfs::store::OpenMode::ReadOnly).unwrap();
    for (index, name) in names.iter().enumerate() {
        let a = independent.load_checkpoint(name).unwrap();
        let b = out.load_checkpoint(name).unwrap();
        assert_eq!(a.tensors.len(), b.tensors.len());
        for (ta, tb) in a.tensors.iter().zip(b.tensors.iter()) {
            assert_eq!(ta.name, tb.name);
            assert_eq!(ta.logical_root, tb.logical_root, "{name}/{}", ta.name);
            assert_eq!(ta.content_root, tb.content_root, "{name}/{}", ta.name);
            assert_eq!(ta.element_bytes, tb.element_bytes);
        }
        let source = [&v0, &v1, &v2][index];
        let expected = weftfs::safetensors::sha256(&fs::read(source).unwrap());
        let mut bytes = Vec::new();
        let restored = out
            .restore_checkpoint(name, &CheckpointRestoreOptions::default(), &mut bytes)
            .unwrap();
        assert!(restored.exact_source_file);
        assert_eq!(restored.file_sha256, expected);
    }
    let verify = out.verify(true).unwrap();
    assert!(verify.ok, "{:?}", verify.problems);
}
