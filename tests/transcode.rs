//! Stage 1: critical-path write policy and the bounded background transcode
//! pass.
//!
//! These tests use the shipped checkpoint API on a synthetic weight-like
//! container. They prove the properties the court then measures on real
//! weights:
//!
//! * a `Fast` save never produces a planned strategy;
//! * a `Fast` checkpoint restores bit-exactly;
//! * a transcode pass republishes the same object id with a smaller planned
//!   representation, keeps the old version reachable from a snapshot, records
//!   `weft.provenance.v1` with `operation: "transcode"`, and leaves every
//!   restore exact;
//! * budgets, cancellation and dry runs are honoured.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::provenance::ProvenanceRecord;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{FastCodec, ResolvedWritePolicy, TensorStrategy, WritePolicy};
use weftfs::transcode::TranscodeOptions;

const SHAPE: [u64; 3] = [96, 32, 16];
const TENSOR_A: &str = "model.layers.0.mlp.down_proj.weight";
const TENSOR_B: &str = "model.layers.0.self_attn.q_proj.weight";

fn opts() -> Options {
    Options {
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn temp_store(label: &str) -> (PathBuf, Store) {
    let path = std::env::temp_dir().join(format!(
        "weft-transcode-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    let store = Store::init(&path, opts()).unwrap();
    (path, store)
}

/// Weight-like BF16: bounded values with a repeated exponent, which is what
/// every planned mode exploits and raw/fast-ZSTD do not.
fn bf16_payload(elements: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements * 2);
    let mut state = seed | 1;
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

fn tensor_len() -> usize {
    SHAPE.iter().product::<u64>() as usize * 2
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let data_a = bf16_payload(tensor_len() / 2, 0x5eed_1234);
    let data_b = bf16_payload(tensor_len() / 2, 0x5eed_9876);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: TENSOR_A,
                dtype: "bf16",
                shape: &SHAPE,
                data: &data_a,
            },
            SafetensorsEntry {
                name: TENSOR_B,
                dtype: "bf16",
                shape: &SHAPE,
                data: &data_b,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("weights.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn write_fast(
    store: &mut Store,
    source: &Path,
    codec: FastCodec,
) -> weftfs::checkpoint::CheckpointWriteReport {
    store
        .write_checkpoint(
            "model",
            source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                write_policy: WritePolicy::Fast,
                fast_codec: codec,
                fast_zstd_level: 1,
                on_critical_path: true,
                deadline_ms: None,
                causal_checkpoint: None,
                semantic_search: true,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap()
}

#[test]
fn write_policy_auto_resolves_from_the_caller_declaration() {
    let auto = WritePolicy::Auto {
        on_critical_path: false,
        deadline_ms: None,
    };
    assert_eq!(auto.resolved(), ResolvedWritePolicy::Planned);
    let critical = WritePolicy::Auto {
        on_critical_path: true,
        deadline_ms: None,
    };
    assert_eq!(critical.resolved(), ResolvedWritePolicy::Fast);
    let deadline = WritePolicy::Auto {
        on_critical_path: false,
        deadline_ms: Some(50),
    };
    assert_eq!(deadline.resolved(), ResolvedWritePolicy::Fast);
    assert_eq!(WritePolicy::Fast.resolved(), ResolvedWritePolicy::Fast);
    assert_eq!(
        WritePolicy::Planned.resolved(),
        ResolvedWritePolicy::Planned
    );

    let options = CheckpointWriteOptions {
        write_policy: auto,
        on_critical_path: true,
        ..CheckpointWriteOptions::default()
    };
    assert!(options.is_fast());
    assert_eq!(options.effective_strategy(), ShardStrategy::Zstd);
}

#[test]
fn fast_save_never_produces_a_planned_strategy_and_restores_exactly() {
    for codec in [FastCodec::Raw, FastCodec::Zstd] {
        let (path, mut store) = temp_store("fast");
        let source = write_container(&path.join("src"));
        let report = write_fast(&mut store, &source, codec);
        assert_eq!(report.shard_objects, 2);
        for tensor in &report.tensors {
            for id in &tensor.object_ids {
                let meta = store.tensor_metadata(*id).unwrap();
                assert!(
                    matches!(meta.strategy.as_str(), "raw" | "zstd"),
                    "fast save wrote `{}` for {codec:?}",
                    meta.strategy
                );
                assert_eq!(meta.logical_root, tensor.logical_root);
            }
        }
        let mut bytes = Vec::new();
        let report = store
            .restore_checkpoint("model", &CheckpointRestoreOptions::default(), &mut bytes)
            .unwrap();
        assert!(report.exact_source_file, "fast restore must be exact");
        let source_bytes = fs::read(&source).unwrap();
        assert_eq!(
            weftfs::safetensors::sha256(&bytes),
            weftfs::safetensors::sha256(&source_bytes)
        );
        store.verify(true).unwrap();
        drop(store);
        let _ = fs::remove_dir_all(&path);
    }
}

#[test]
fn transcode_republishes_the_same_object_id_and_keeps_the_old_version() {
    let (path, mut store) = temp_store("transcode");
    let source = write_container(&path.join("src"));
    let report = write_fast(&mut store, &source, FastCodec::Raw);
    let object_ids: Vec<_> = report
        .tensors
        .iter()
        .flat_map(|tensor| tensor.object_ids.clone())
        .collect();
    let old_state = store.snapshot_create("pre-transcode").unwrap();

    let root = store.resolve_path("/checkpoints/model").unwrap();
    let transcode = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert!(
        !transcode.candidates.is_empty(),
        "weight-like BF16 must have a smaller planned form"
    );
    assert_eq!(transcode.applied.len(), transcode.candidates.len());
    for outcome in &transcode.applied {
        assert!(outcome.verified);
        assert_eq!(outcome.from_strategy, "raw");
        assert_ne!(outcome.to_strategy, TensorStrategy::Raw);
        assert!(outcome.new_physical_bytes < outcome.old_physical_bytes);
    }
    // Same object ids: a checkpoint manifest needs no rewrite.
    for id in &object_ids {
        let meta = store.tensor_metadata(*id).unwrap();
        assert!(
            !matches!(meta.strategy.as_str(), "raw" | "zstd"),
            "object {id} was not transcoded"
        );
    }
    // Live restore and the pinned pre-transcode restore are both exact.
    for snapshot in [None, Some(old_state)] {
        let mut bytes = Vec::new();
        let report = match snapshot {
            None => store
                .restore_checkpoint("model", &CheckpointRestoreOptions::default(), &mut bytes)
                .unwrap(),
            Some(snapshot) => store
                .restore_checkpoint_at(
                    snapshot,
                    "model",
                    &CheckpointRestoreOptions::default(),
                    &mut bytes,
                )
                .unwrap(),
        };
        assert!(report.exact_source_file);
    }
    // Provenance: one `transcode` record naming old and new content roots.
    for outcome in &transcode.applied {
        let record = store.read_provenance(outcome.provenance_id).unwrap();
        assert_eq!(record.operation, "transcode");
        assert!(record.outputs.contains(&outcome.content));
    }
    // A second pass has nothing to do.
    let again = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert!(again.applied.is_empty());
    store.verify(true).unwrap();
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn transcode_dry_run_and_budgets_write_nothing() {
    let (path, mut store) = temp_store("budget");
    let source = write_container(&path.join("src"));
    let report = write_fast(&mut store, &source, FastCodec::Raw);
    let root = store.resolve_path("/checkpoints/model").unwrap();
    let before = store.generation();

    let dry = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                dry_run: true,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert!(!dry.candidates.is_empty());
    assert!(dry.applied.is_empty());
    assert_eq!(store.generation(), before);

    // A one-object budget stops the pass with a recorded reason.
    let bounded = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 1,
                min_saved_percent: 0,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert_eq!(bounded.scanned, 1);
    assert!(bounded.stopped_reason.is_some());

    // Cooperative cancellation is checked before the next object.
    let cancel = Arc::new(AtomicBool::new(true));
    let cancelled = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                cancel: Some(cancel),
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert_eq!(cancelled.scanned, 0);
    assert!(cancelled
        .stopped_reason
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    for tensor in &report.tensors {
        for id in &tensor.object_ids {
            assert_eq!(store.tensor_metadata(*id).unwrap().strategy, "raw");
        }
    }
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn transcode_only_rewrites_fast_sources() {
    let (path, mut store) = temp_store("sources");
    let source = write_container(&path.join("src"));
    store
        .write_checkpoint(
            "model",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let root = store.resolve_path("/checkpoints/model").unwrap();
    let report = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    assert!(
        report.applied.is_empty(),
        "a planned save is not a transcode source"
    );
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn causal_checkpoint_links_matching_tensors() {
    let (path, mut store) = temp_store("causal");
    let source_a = write_container(&path.join("a"));
    store
        .write_checkpoint(
            "a",
            &source_a,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Planned,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();

    // A near-copy: one BF16 word per tensor differs.
    let data_a = bf16_payload(tensor_len() / 2, 0x5eed_1234);
    let data_b = bf16_payload(tensor_len() / 2, 0x5eed_9876);
    let mut changed_a = data_a.clone();
    let mut changed_b = data_b.clone();
    changed_a[100] ^= 0x01;
    changed_b[100] ^= 0x01;
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: TENSOR_A,
                dtype: "bf16",
                shape: &SHAPE,
                data: &changed_a,
            },
            SafetensorsEntry {
                name: TENSOR_B,
                dtype: "bf16",
                shape: &SHAPE,
                data: &changed_b,
            },
        ],
        None,
    )
    .unwrap();
    let source_b = path.join("b.safetensors");
    fs::write(&source_b, file).unwrap();
    let report = store
        .write_checkpoint(
            "b",
            &source_b,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Planned,
                tile_shape: None,
                causal_checkpoint: Some("a".to_owned()),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let mut delta_tensors = 0;
    for tensor in &report.tensors {
        for id in &tensor.object_ids {
            let strategy = store.tensor_metadata(*id).unwrap().strategy;
            if strategy == "weight-delta" || strategy == "sparse-delta" {
                delta_tensors += 1;
            }
        }
    }
    assert!(
        delta_tensors > 0,
        "a one-word change must be offered a delta"
    );
    let mut out = Vec::new();
    let restored = store
        .restore_checkpoint("b", &CheckpointRestoreOptions::default(), &mut out)
        .unwrap();
    assert!(restored.exact_source_file);
    store.verify(true).unwrap();
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn provenance_round_trips_as_a_canonical_record() {
    let (path, mut store) = temp_store("prov");
    let source = write_container(&path.join("src"));
    write_fast(&mut store, &source, FastCodec::Raw);
    let root = store.resolve_path("/checkpoints/model").unwrap();
    let report = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects: 64,
                min_saved_percent: 0,
                ..TranscodeOptions::default()
            },
        )
        .unwrap();
    let outcome = &report.applied[0];
    let bytes = outcome.provenance_id;
    let record = store.read_provenance(bytes).unwrap();
    let encoded = record.encode().unwrap();
    let decoded = ProvenanceRecord::decode(&encoded).unwrap();
    assert_eq!(record.id().unwrap(), decoded.id().unwrap());
    drop(store);
    let _ = fs::remove_dir_all(&path);
}
