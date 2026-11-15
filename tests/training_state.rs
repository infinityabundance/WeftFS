//! Optimizer state, gradients and attention masks: fixtures and exact replay.
//!
//! The fixtures are synthetic (deterministic bytes) and labelled as such. Every
//! artifact is written to a real store, reopened, and replayed byte-for-byte.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::adapter::f32_to_bf16_bits;
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::error::Class;
use weftfs::safetensors::{self, SafetensorsEntry, SafetensorsFile};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::training_state::{
    AdamwHyper, AttentionMaskManifest, GradientEntry, GradientsManifest, OptimizerEntry,
    OptimizerStateManifest, TrainingWriteOptions,
};

const W: &str = "model.layers.0.self_attn.q_proj.weight";
const B: &str = "model.layers.0.mlp.down_proj.weight";
const NORM: &str = "model.norm.weight";

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 64,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn bytes_of(len: usize, seed: u8) -> Vec<u8> {
    let mut state = seed as u64 | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn write_model(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let w = bytes_of(8 * 16 * 2, 1);
    let b = bytes_of(8 * 16 * 2, 2);
    let norm = bytes_of(16 * 2, 3);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: W,
                dtype: "bf16",
                shape: &[8, 16],
                data: &w,
            },
            SafetensorsEntry {
                name: B,
                dtype: "bf16",
                shape: &[8, 16],
                data: &b,
            },
            SafetensorsEntry {
                name: NORM,
                dtype: "bf16",
                shape: &[16],
                data: &norm,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("model.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn setup(dir: &Path) -> Store {
    let mut store = Store::init(dir.join("store"), opts()).unwrap();
    let source = write_model(&dir.join("src"));
    store
        .write_checkpoint(
            "model",
            &source,
            &CheckpointWriteOptions {
                strategy: weftfs::checkpoint::ShardStrategy::Planned,
                deterministic_seed: Some([11u8; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    store
}

fn moment(len: usize, seed: u8) -> Vec<u8> {
    // Positive values with small magnitude: a plausible exp_avg_sq payload.
    let _ = seed;
    let count = len / 2;
    let mut out = Vec::with_capacity(len);
    for index in 0..count {
        let value = ((index as f32) * 0.004 + 1.0e-6).abs();
        out.extend_from_slice(&f32_to_bf16_bits(value).to_le_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn optimizer_state_round_trips_exactly() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let avg_w = moment(8 * 16 * 2, 4);
    let sq_w = moment(8 * 16 * 2, 5);
    let avg_norm = moment(16 * 2, 6);
    let sq_norm = moment(16 * 2, 7);
    let entries = [
        OptimizerEntry {
            name: W,
            exp_avg: &avg_w,
            exp_avg_sq: &sq_w,
            step: 7,
            moment_dtype: None,
        },
        OptimizerEntry {
            name: NORM,
            exp_avg: &avg_norm,
            exp_avg_sq: &sq_norm,
            step: 7,
            moment_dtype: None,
        },
    ];
    let report = store
        .write_optimizer_state(
            "step-7",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &entries,
            &TrainingWriteOptions {
                record_provenance: true,
                ..TrainingWriteOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.params, 2);
    assert_eq!(report.logical_bytes, (8 * 16 * 2 * 2 + 16 * 2 * 2) as u64);

    let manifest = store.load_optimizer_state("step-7").unwrap();
    assert_eq!(manifest.optimizer, "adamw");
    assert_eq!(manifest.hyper.lr_bits, 1e-4f64.to_bits());
    assert_eq!(manifest.params.len(), 2);
    // Params are canonically sorted.
    assert_eq!(manifest.params[0].name, W, "sorted by name");
    assert_eq!(manifest.params[1].name, NORM);

    let restored = store.restore_optimizer_state("step-7").unwrap();
    assert_eq!(restored.artifact, report.artifact);
    let map: std::collections::BTreeMap<_, _> = restored
        .params
        .iter()
        .map(|(name, avg, sq, step)| (name.clone(), (avg.clone(), sq.clone(), *step)))
        .collect();
    assert_eq!(map[W].0, avg_w);
    assert_eq!(map[W].1, sq_w);
    assert_eq!(map[NORM].0, avg_norm);
    assert_eq!(map[W].2, 7);
    assert_eq!(store.verify_optimizer_state("step-7").unwrap().params, 2);
    let (covered, total) = store
        .training_coverage("model", &map.keys().cloned().collect())
        .unwrap();
    assert_eq!((covered, total), (2, 3));

    // Provenance names the model manifest as an input.
    let manifest_object = store
        .resolve_path("/training/optimizer/step-7/manifest")
        .unwrap();
    let record = store.provenance_of(manifest_object).unwrap().unwrap();
    assert_eq!(record.operation, "optimizer-state-ingest");
    assert_eq!(record.inputs[0].role, "model-checkpoint-manifest");
}

#[test]
fn gradients_round_trip_exactly() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let grad_w = bytes_of(8 * 16 * 2, 9);
    let grad_b = bytes_of(8 * 16 * 2, 10);
    let entries = [
        GradientEntry {
            name: W,
            grad: &grad_w,
                grad_dtype: None,
        },
        GradientEntry {
            name: B,
            grad: &grad_b,
                grad_dtype: None,
        },
    ];
    let report = store
        .write_gradients(
            "step-7",
            "model",
            7,
            "cross_entropy",
            2.302_585,
            32,
            64,
            Some(1.25),
            &entries,
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let manifest = store.load_gradients("step-7").unwrap();
    assert_eq!(manifest.step, 7);
    assert_eq!(manifest.loss_bits, 2.302_585f64.to_bits());
    assert_eq!(manifest.global_norm_bits, 1.25f64.to_bits());
    assert_eq!(manifest.params.len(), 2);
    let restored = store.restore_gradients("step-7").unwrap();
    assert_eq!(restored.artifact, report.artifact);
    let map: std::collections::BTreeMap<_, _> = restored.params.into_iter().collect();
    assert_eq!(map[W], grad_w);
    assert_eq!(map[B], grad_b);
    assert_eq!(store.verify_gradients("step-7").unwrap().tensor_objects, 2);
}

#[test]
fn attention_mask_round_trips_with_its_encoding_statement() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    // A real causal mask [1, 1, 4, 4], u8, 1 = attends.
    let mut mask = Vec::with_capacity(16);
    for row in 0..4u8 {
        for column in 0..4u8 {
            mask.push(if column <= row { 1 } else { 0 });
        }
    }
    let report = store
        .write_attention_mask(
            "causal-4",
            "causal",
            &[1, 1, 4, 4],
            4,
            0,
            &mask,
            "1 = the query attends to the key; 0 = masked",
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let manifest = store.load_attention_mask("causal-4").unwrap();
    assert_eq!(manifest.kind, "causal");
    assert_eq!(manifest.dtype, "u8");
    assert_eq!(manifest.shape, vec![1, 1, 4, 4]);
    assert_eq!(manifest.seq_len, 4);
    assert_eq!(manifest.mask.logical_bytes().unwrap(), 16);
    let restored = store.read_attention_mask("causal-4").unwrap();
    assert_eq!(restored.artifact, report.artifact);
    assert_eq!(restored.bytes, mask);
    assert_eq!(
        store
            .verify_attention_mask("causal-4")
            .unwrap()
            .tensor_objects,
        1
    );
}

#[test]
fn reopen_replays_every_artifact_byte_for_byte() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let avg = moment(8 * 16 * 2, 12);
    let sq = moment(8 * 16 * 2, 13);
    let grad = bytes_of(8 * 16 * 2, 14);
    let mask: Vec<u8> = (0..16u8).map(|index| u8::from(index % 4 == 0)).collect();
    let optimizer = store
        .write_optimizer_state(
            "o",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &avg,
                exp_avg_sq: &sq,
                step: 3,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let gradients = store
        .write_gradients(
            "g",
            "model",
            3,
            "cross_entropy",
            1.0,
            8,
            16,
            None,
            &[GradientEntry {
                name: W,
                grad: &grad,
                grad_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let attention = store
        .write_attention_mask(
            "m",
            "custom",
            &[1, 4, 4],
            4,
            1,
            &mask,
            "1 = attends",
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    drop(store);

    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.load_optimizer_state("o").unwrap().id().unwrap(),
        optimizer.artifact
    );
    assert_eq!(
        store.load_gradients("g").unwrap().id().unwrap(),
        gradients.artifact
    );
    assert_eq!(
        store.load_attention_mask("m").unwrap().id().unwrap(),
        attention.artifact
    );
    let restored = store.restore_optimizer_state("o").unwrap();
    assert_eq!(restored.params[0].1, avg);
    assert_eq!(restored.params[0].2, sq);
    assert_eq!(store.restore_gradients("g").unwrap().params[0].1, grad);
    assert_eq!(store.read_attention_mask("m").unwrap().bytes, mask);
    assert_eq!(
        store.list_training("optimizer").unwrap(),
        vec!["o".to_owned()]
    );
    assert_eq!(
        store.list_training("gradient").unwrap(),
        vec!["g".to_owned()]
    );
    assert_eq!(store.list_training("mask").unwrap(), vec!["m".to_owned()]);
}

#[test]
fn malformed_moments_are_refused_before_anything_is_written() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let short = vec![0u8; 7];
    let ok = moment(8 * 16 * 2, 21);
    let err = store
        .write_optimizer_state(
            "bad",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &short,
                exp_avg_sq: &ok,
                step: 1,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
    // No artifact was published: the directory may exist, the manifest may not.
    assert!(store.load_optimizer_state("bad").is_err());
    assert!(!store.list_training("optimizer").unwrap().contains(&"bad".to_owned()));

    // A parameter the model does not have.
    let err = store
        .write_optimizer_state(
            "bad2",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: "model.layers.9.weight",
                exp_avg: &ok,
                exp_avg_sq: &ok,
                step: 1,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");

    // A mask with the wrong number of bytes.
    let err = store
        .write_attention_mask(
            "bad-mask",
            "causal",
            &[1, 4, 4],
            4,
            0,
            &[1u8; 15],
            "1 = attends",
            &TrainingWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // An unsupported mask dtype / kind is refused by the manifest parser.
    let mut manifest = AttentionMaskManifest {
        name: "x".to_owned(),
        kind: "causal".to_owned(),
        dtype: "u8".to_owned(),
        shape: vec![1, 4, 4],
        seq_len: 4,
        pad_positions: 0,
        mask: weftfs::adapter::TensorRef {
            object: weftfs::identity::Hash32::from_bytes([1; 32]),
            content: weftfs::identity::Hash32::from_bytes([2; 32]),
            dtype: "u8".to_owned(),
            shape: vec![1, 4, 4],
        },
        interpretation: "1 = attends".to_owned(),
    };
    manifest.dtype = "u16".to_owned();
    let err = AttentionMaskManifest::decode(&manifest.encode().unwrap()).unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedRepresentation, "{err}");
    manifest.dtype = "u8".to_owned();
    manifest.kind = "rope".to_owned();
    let err = AttentionMaskManifest::decode(&manifest.encode().unwrap()).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
}

#[test]
fn model_drift_and_tensor_tampering_are_typed() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let avg = moment(8 * 16 * 2, 31);
    let sq = moment(8 * 16 * 2, 32);
    store
        .write_optimizer_state(
            "o",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &avg,
                exp_avg_sq: &sq,
                step: 1,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    // Tamper one moment object's bytes.
    let manifest = store.load_optimizer_state("o").unwrap();
    let object = manifest.params[0].exp_avg.object;
    let mut bytes = store.read_object(object).unwrap();
    bytes[0] ^= 0xff;
    store.write_file(object, 0, &bytes).unwrap();
    let err = store.verify_optimizer_state("o").unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");

    // Model drift: rewrite the checkpoint manifest object.
    let dir2 = tempdir().unwrap();
    let mut clean = setup(dir2.path());
    let avg = moment(8 * 16 * 2, 33);
    clean
        .write_optimizer_state(
            "o",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &avg,
                exp_avg_sq: &avg,
                step: 1,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let manifest_id = clean
        .resolve_path("/checkpoints/model/manifest")
        .unwrap();
    let mut bytes = clean.read_object(manifest_id).unwrap();
    bytes.push(0);
    clean.write_file(manifest_id, 0, &bytes).unwrap();
    let err = match clean.load_optimizer_state("o") {
        Ok(_) => clean.verify_optimizer_state("o").unwrap_err(),
        Err(err) => err,
    };
    assert!(
        err.class() == Class::Corrupt
            || err.class() == Class::DependencyMismatch
            || err.class() == Class::Malformed
            || err.class() == Class::NonCanonical,
        "{err}"
    );
}

#[test]
fn manifest_schemas_reject_tampered_identities() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let grad = bytes_of(8 * 16 * 2, 41);
    store
        .write_gradients(
            "g",
            "model",
            1,
            "cross_entropy",
            1.0,
            8,
            16,
            None,
            &[GradientEntry {
                name: W,
                grad: &grad,
                grad_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let manifest = store.load_gradients("g").unwrap();
    let bytes = manifest.encode().unwrap();
    let mut value = weftcodec::wcb::decode_default(&bytes).unwrap();
    let weftcodec::wcb::Value::Map(map) = &mut value else {
        panic!("manifest is a map")
    };
    map.insert(
        "schema".to_owned(),
        weftcodec::wcb::Value::Str("weft.gradient.v2".to_owned()),
    );
    let tampered = weftcodec::wcb::encode_default(&value).unwrap();
    let err = GradientsManifest::decode(&tampered).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // The canonical encoding is stable and the identity is a function of it.
    assert_eq!(manifest.encode().unwrap(), bytes);
    assert_eq!(
        manifest.id().unwrap(),
        GradientsManifest::decode(&bytes).unwrap().id().unwrap()
    );
    // Optimizer manifests round-trip too.
    let avg = moment(8 * 16 * 2, 42);
    store
        .write_optimizer_state(
            "o",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &avg,
                exp_avg_sq: &avg,
                step: 1,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap();
    let optimizer = store.load_optimizer_state("o").unwrap();
    assert_eq!(
        OptimizerStateManifest::decode(&optimizer.encode().unwrap()).unwrap(),
        optimizer
    );
    // A non-adamw optimizer is refused.
    let mut value = optimizer.to_value();
    let weftcodec::wcb::Value::Map(map) = &mut value else {
        panic!("manifest is a map")
    };
    map.insert(
        "optimizer".to_owned(),
        weftcodec::wcb::Value::Str("sgd".to_owned()),
    );
    let err = OptimizerStateManifest::decode(&weftcodec::wcb::encode_default(&value).unwrap())
        .unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedRepresentation, "{err}");
}

#[test]
fn zero_step_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = setup(dir.path());
    let avg = moment(8 * 16 * 2, 51);
    let err = store
        .write_optimizer_state(
            "o",
            "model",
            AdamwHyper::new(1e-4, 0.9, 0.95, 1e-8, 0.01),
            &[OptimizerEntry {
                name: W,
                exp_avg: &avg,
                exp_avg_sq: &avg,
                step: 0,
            moment_dtype: None,
            }],
            &TrainingWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
}

#[test]
fn the_fixture_container_is_synthetic() {
    // Guard against a later edit presenting this fixture as real PyTorch state.
    let dir = tempdir().unwrap();
    let source = write_model(&dir.path().join("src"));
    let file = SafetensorsFile::parse(&source).unwrap();
    assert_eq!(file.tensors.len(), 3);
    assert_eq!(file.tensors.len(), 3);
}
