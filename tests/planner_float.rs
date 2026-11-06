//! R1: mode-9 FLOAT_TENSOR planner candidate, measured cost model, and
//! lineage-aware rebasing.

use std::fs;

use weftfs::cost_model::{estimate_ns, CostClass, CostModelKind};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
    let path = std::env::temp_dir().join(format!(
        "weft-planner-float-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    let store = Store::init(
        &path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .unwrap();
    (path, store)
}

/// Deterministic weight-like BF16 payload: bounded f32 values truncated to BF16.
fn bf16_payload(elements: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32);
        let value = (unit * 2.0 - 1.0) * 0.5;
        out.extend_from_slice(&((value.to_bits() >> 16) as u16).to_le_bytes());
    }
    out
}

fn random_payload(bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    let mut state = 0x0bad_f00d_dead_beefu64;
    for _ in 0..bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state >> 24) as u8);
    }
    out
}

#[test]
fn planner_selects_float_tensor_for_weight_like_bf16() {
    let (path, mut store) = temp_store("select");
    let root = store.resolve_path("/").unwrap();
    // 4 MiB of BF16 weights: mode 9 stores sign+mantissa raw and Huffman-codes
    // the exponent plane, so it should beat a single zstd frame on bytes.
    let bytes = bf16_payload(2 * 1024 * 1024);
    let shape = vec![2048, 1024];
    let layout = TensorLayoutSpec::row_major();
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let float = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::FloatTensor)
        .expect("float-tensor is a candidate");
    let zstd = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::Zstd)
        .expect("zstd is a candidate");
    assert!(float.applicable && zstd.applicable);
    assert!(
        float.physical_bytes.unwrap() < zstd.physical_bytes.unwrap(),
        "mode 9 should be smaller than zstd on weight-like BF16 ({} vs {})",
        float.physical_bytes.unwrap(),
        zstd.physical_bytes.unwrap()
    );
    assert_eq!(plan.chosen, TensorStrategy::FloatTensor);

    // And the chosen object round-trips exactly through mode 9.
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"weights",
            "bf16",
            &shape,
            &bytes,
            &layout,
            &TensorWriteSpec::default(),
        )
        .unwrap();
    assert_eq!(store.tensor_metadata(id).unwrap().strategy, "float-tensor");
    assert_eq!(store.read_tensor(id).unwrap(), bytes);
    store.verify(true).unwrap();
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn planner_does_not_select_float_tensor_for_random_data() {
    let (path, mut store) = temp_store("random");
    let root = store.resolve_path("/").unwrap();
    let bytes = random_payload(2 * 1024 * 1024);
    let shape = vec![1024, 1024];
    let layout = TensorLayoutSpec::row_major();
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let float = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::FloatTensor)
        .unwrap();
    let zstd = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::Zstd)
        .unwrap();
    // Either mode 9 is not priced at all (below the 1/64 saving margin) or it
    // is no smaller than zstd; either way it must not be chosen.
    if let (Some(float_bytes), Some(zstd_bytes)) = (float.physical_bytes, zstd.physical_bytes) {
        assert!(
            float_bytes >= zstd_bytes,
            "incompressible data must not make mode 9 smaller ({float_bytes} vs {zstd_bytes})"
        );
    }
    assert!(!float.applicable || plan.chosen != TensorStrategy::FloatTensor);
    assert_ne!(plan.chosen, TensorStrategy::FloatTensor);
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"random",
            "bf16",
            &shape,
            &bytes,
            &layout,
            &TensorWriteSpec::default(),
        )
        .unwrap();
    assert_ne!(store.tensor_metadata(id).unwrap().strategy, "float-tensor");
    assert_eq!(store.read_tensor(id).unwrap(), bytes);
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn measured_cost_model_is_used_and_lambda_can_flip_the_choice() {
    let (path, mut store) = temp_store("cost-model");
    // 48 MiB: large enough that the raw-vs-zstd byte/decode crossover falls on
    // an integer lambda for both cost tables (the threshold scales with size).
    let elements = 24usize * 1024 * 1024;
    let bytes = bf16_payload(elements);
    let shape = vec![(elements / 1024) as u64, 1024];
    let layout = TensorLayoutSpec::row_major();

    // Candidate physical sizes are model-independent.
    let plan0 = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let raw_bytes = plan0
        .candidates
        .iter()
        .find(|c| c.strategy == TensorStrategy::Raw)
        .unwrap()
        .physical_bytes
        .unwrap();
    let zstd_bytes = plan0
        .candidates
        .iter()
        .find(|c| c.strategy == TensorStrategy::Zstd)
        .unwrap()
        .physical_bytes
        .unwrap();
    assert!(zstd_bytes < raw_bytes, "test fixture must compress");

    // Candidate decode estimates must equal the shared integer table exactly.
    for model in [CostModelKind::Declared, CostModelKind::Measured] {
        let options = TensorPlanOptions {
            cost_model: model,
            ..TensorPlanOptions::default()
        };
        let plan = store
            .plan_tensor("bf16", &shape, &bytes, &layout, &[], &options)
            .unwrap();
        for candidate in &plan.candidates {
            if let Some(estimated) = candidate.estimated_decode_ns {
                let class = candidate.strategy.cost_class();
                assert_eq!(
                    estimated,
                    estimate_ns(class, bytes.len() as u64, model),
                    "{} estimate is not the shared table value",
                    candidate.strategy.as_str()
                );
            }
        }
    }

    // The two tables must produce different estimates for the same candidate,
    // proving `--planner-cost-model` reaches the planner.
    let declared_zstd = estimate_ns(CostClass::Zstd, bytes.len() as u64, CostModelKind::Declared);
    let measured_zstd = estimate_ns(CostClass::Zstd, bytes.len() as u64, CostModelKind::Measured);
    assert_ne!(declared_zstd, measured_zstd);

    // Lambda is the switch that must change the choice: at lambda = 0 the
    // smallest physical representation wins (mode 9 on weight-like BF16); at
    // lambda = 1 byte/ns the decode term dominates and raw (the cheapest
    // decode) wins, for both cost tables.
    for model in [CostModelKind::Declared, CostModelKind::Measured] {
        let zero = store
            .plan_tensor(
                "bf16",
                &shape,
                &bytes,
                &layout,
                &[],
                &TensorPlanOptions {
                    lambda_ns: 0,
                    cost_model: model,
                    ..TensorPlanOptions::default()
                },
            )
            .unwrap();
        let one = store
            .plan_tensor(
                "bf16",
                &shape,
                &bytes,
                &layout,
                &[],
                &TensorPlanOptions {
                    lambda_ns: 1,
                    cost_model: model,
                    ..TensorPlanOptions::default()
                },
            )
            .unwrap();
        assert_eq!(zero.chosen, TensorStrategy::FloatTensor);
        assert_eq!(one.chosen, TensorStrategy::Raw);
        assert_ne!(zero.chosen, one.chosen);
        assert_ne!(zero.chosen_cost, one.chosen_cost);
    }
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn rebase_rewrites_a_hot_delta_and_is_idempotent() {
    let (path, mut store) = temp_store("rebase");
    let root = store.resolve_path("/").unwrap();
    let layout = TensorLayoutSpec::row_major();
    let elements = 512 * 1024; // 1 MiB BF16
    let shape = vec![512, 1024];
    let base_bytes = bf16_payload(elements);
    let mut target_bytes = base_bytes.clone();
    // Sparse fine-tune update: change the low bit of every 4096th word.
    for index in (0..elements).step_by(4096) {
        target_bytes[index * 2] ^= 1;
    }

    let base_id = store
        .write_tensor_file_with_spec(
            root,
            b"base",
            "bf16",
            &shape,
            &base_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::FloatTensor),
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    let base_version = store.object_meta(base_id).unwrap().version;
    let delta_id = store
        .write_tensor_file_with_spec(
            root,
            b"instruct",
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::WeightDelta),
                causal: vec![base_version],
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    let before_meta = store.tensor_metadata(delta_id).unwrap();
    assert_eq!(before_meta.strategy, "weight-delta");
    let before_record = store.object_meta(delta_id).unwrap();
    let before_version = before_record.version;
    let before_content = before_record.content;

    // A snapshot pins the pre-rebase state.
    let snapshot = store.snapshot_create("before-rebase").unwrap();
    assert_eq!(
        store.read_object_at(Some(snapshot), delta_id).unwrap(),
        target_bytes
    );

    // Make it hot. Each read decodes the delta and its mode-9 base.
    for _ in 0..12 {
        assert_eq!(store.read_tensor(delta_id).unwrap(), target_bytes);
    }
    store.flush_access_counts().unwrap();
    // 12 explicit reads plus the snapshot read above.
    assert!(store.access_reads(delta_id) >= 12);

    let threshold = 1_000_000u64;
    let dry = store
        .rebase(
            root,
            &weftfs::RebaseOptions {
                threshold_ns: threshold,
                dry_run: true,
                ..weftfs::RebaseOptions::default()
            },
        )
        .unwrap();
    let candidate = dry
        .plan
        .candidates
        .iter()
        .find(|candidate| candidate.object == delta_id)
        .expect("the hot delta is a rebase candidate");
    assert_eq!(candidate.to_strategy, TensorStrategy::FloatTensor);
    assert!(candidate.new_estimated_decode_ns < candidate.estimated_decode_ns);
    assert!(dry.applied.is_empty(), "dry run must not write");

    let report = store
        .rebase(
            root,
            &weftfs::RebaseOptions {
                threshold_ns: threshold,
                dry_run: false,
                ..weftfs::RebaseOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.applied.len(), 1);
    let outcome = &report.applied[0];
    assert_eq!(outcome.object, delta_id);
    assert_ne!(outcome.new_version, before_version);
    assert_ne!(
        outcome.content, before_content,
        "the new representation has a new manifest (content root)"
    );
    assert_eq!(
        store.object_meta(delta_id).unwrap().content,
        outcome.content
    );
    // The new version's causal parent is the version it replaced.
    let closure = weftfs::state::object_causal_closure(&store.object_meta(delta_id).unwrap())
        .unwrap()
        .expect("rebase publishes causal attributes");
    assert_eq!(closure.parents, vec![before_version]);
    // The provenance receipt is attached to the object and says `rebase`.
    let records = store.provenance_records_of(delta_id).unwrap();
    let (provenance_id, record) = records
        .iter()
        .find(|(_, record)| record.operation == "rebase")
        .expect("rebase provenance exists");
    assert_eq!(*provenance_id, outcome.provenance_id);
    assert!(record.inputs.iter().any(|input| {
        input.role == "source" && input.content == before_content && input.object == Some(delta_id)
    }));
    assert_eq!(record.outputs, vec![outcome.content]);
    assert_eq!(record.determinism, weftfs::Determinism::Exact);

    // The live record is the new version; the previous version is still
    // resolvable through the pinned snapshot (history is not rewritten).
    let after_meta = store.tensor_metadata(delta_id).unwrap();
    assert_eq!(after_meta.strategy, "float-tensor");
    assert_eq!(after_meta.logical_root, before_meta.logical_root);
    assert_eq!(
        store.object_meta(delta_id).unwrap().version,
        outcome.new_version
    );
    assert_ne!(outcome.new_version, before_version);
    assert_eq!(
        store.access_reads(delta_id),
        0,
        "rebase consumes the counter"
    );

    // Idempotent: with no intervening reads a second run sees zero reads.
    let second = store
        .rebase(
            root,
            &weftfs::RebaseOptions {
                threshold_ns: threshold,
                dry_run: false,
                ..weftfs::RebaseOptions::default()
            },
        )
        .unwrap();
    assert!(second.applied.is_empty());
    assert!(second
        .plan
        .candidates
        .iter()
        .all(|candidate| candidate.object != delta_id));

    // Bytes and the pinned snapshot are unchanged by the rebase.
    assert_eq!(store.read_tensor(delta_id).unwrap(), target_bytes);
    assert_eq!(
        store.read_object_at(Some(snapshot), delta_id).unwrap(),
        target_bytes
    );
    store.verify(true).unwrap();
    drop(store);

    // Counters persist across reopen; the two reads after the second run are
    // retained and the consumed pre-rebase count is not resurrected.
    let store = Store::open(&path, OpenMode::ReadWrite).unwrap();
    assert_eq!(store.access_reads(delta_id), 2);
    assert!(store.current_state().objects.contains_key(&delta_id));
    drop(store);
    let _ = fs::remove_dir_all(&path);
}
