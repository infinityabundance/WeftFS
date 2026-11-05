//! Training-ready materialization: every precondition is checked against the
//! pinned snapshot, and any failure leaves no output directory.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use weftcodec::context::{CapabilityScope, CausalClosure, ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::tensor::logical_root as tensor_logical_root;
use weftcodec::wcb::Value;
use weftcodec::{Limits, TensorDtype};

use weftfs::auth::Principal;
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::context::CapabilityContext;
use weftfs::error::Class;
use weftfs::identity::{self, Hash32};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, SetAttr, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy};
use weftfs::training_ready::{
    verify_training_ready_output, TrainingReadyOptions, MANIFEST_FILE, RECORD_FILE,
};

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

fn create(store: &mut Store, name: &str, text: &str) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(
            root,
            name.as_bytes(),
            text.as_bytes(),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap()
}

fn save_root(store: &mut Store, name: &str, snapshot: Hash32, causal: Vec<Hash32>) -> Hash32 {
    let selector = selector::parse("select true").unwrap();
    let root = ContextRoot::new(&selector, snapshot, DEFAULT_RESOLVER, causal, None, None).unwrap();
    store.save_context_root(name, &root, None, false).unwrap();
    root.id().unwrap()
}

fn write_checkpoint(store: &mut Store, dir: &Path) -> (String, usize) {
    let shape = [8u64, 4];
    let data: Vec<u8> = (0..64).map(|byte| byte as u8).collect();
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
    fs::create_dir_all(dir).unwrap();
    let source = dir.join("tiny.safetensors");
    fs::write(&source, &file).unwrap();
    let report = store
        .write_checkpoint(
            "tiny",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: weftfs::checkpoint::ShardStrategy::TensorSharded,
                tile_shape: None,
            },
        )
        .unwrap();
    ("tiny".to_owned(), report.tensors.len())
}

fn options(require_tensors: bool) -> TrainingReadyOptions {
    TrainingReadyOptions {
        require_tensors,
        ..TrainingReadyOptions::default()
    }
}

#[test]
fn happy_path_materializes_and_verifies_a_checkpoint() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let note = create(&mut store, "notes.txt", "training notes");
    store
        .relation_add(
            note,
            "derived_from",
            weftfs::state::RelationTarget::Value(Value::Str("source".to_owned())),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    let (checkpoint, tensors) = write_checkpoint(&mut store, &dir.path().join("src"));
    let snapshot = store.checkpoint().unwrap();
    let note_version = store.object_meta(note).unwrap().version;
    let root_id = save_root(&mut store, "training", snapshot, vec![note_version]);

    let out = dir.path().join("materialized");
    let report = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(true),
        )
        .unwrap();

    assert_eq!(report.context_root_id, root_id);
    assert_eq!(report.preconditions.resolution_exact, true);
    assert_eq!(report.preconditions.content_verified, true);
    assert_eq!(report.preconditions.causal_complete, true);
    assert_eq!(report.preconditions.tensors_verified, true);
    assert!(report.checkpoint_tensors_verified >= tensors as u64);
    assert!(report.tensor_objects_verified > 0);
    assert!(out.join(MANIFEST_FILE).is_file());
    assert!(out.join(RECORD_FILE).is_file());
    assert!(out.join("notes.txt").is_file());
    assert!(out
        .join(format!("checkpoints/{checkpoint}/manifest"))
        .is_file());

    let verified = verify_training_ready_output(&out).unwrap();
    assert_eq!(verified.files, report.files.len() as u64);
    assert_eq!(verified.bytes, report.cost.actual_bytes);
    assert_eq!(verified.checkpoint_tensors_verified, tensors as u64);
    assert_eq!(verified.checkpoints_verified, 1);
    assert_eq!(verified.tensor_roots_verified, tensors as u64);
}

#[test]
fn tampered_chunk_blob_is_refused_and_creates_no_output() {
    let dir = tempdir().unwrap();
    let store_path = dir.path().join("store");
    let mut store = Store::init(&store_path, opts()).unwrap();
    let note = create(&mut store, "data.bin", "exact bytes that must be hashed");
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let out = dir.path().join("materialized");

    let digest = identity::content_hash(b"exact bytes that must be hashed");
    let hex = digest.to_hex();
    let blob = store_path.join("objects").join(&hex[0..2]).join(&hex);
    assert!(blob.is_file(), "chunk blob {blob:?} is missing");
    {
        let mut handle = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&blob)
            .unwrap();
        let mut byte = [0u8; 1];
        handle.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xff;
        handle.seek(SeekFrom::Start(0)).unwrap();
        handle.write_all(&byte).unwrap();
        handle.sync_all().unwrap();
    }

    let err = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(!out.exists(), "a failed run must not create {out:?}");
    let _ = note;
}

#[test]
fn missing_causal_parent_is_named_and_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let parent = create(&mut store, "parent.txt", "parent bytes");
    let parent_version = store.object_meta(parent).unwrap().version;
    let root = store.resolve_path("/").unwrap();
    store.unlink(root, b"parent.txt").unwrap();
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, vec![parent_version]);
    let out = dir.path().join("materialized");

    let err = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::DependencyMismatch, "{err}");
    assert!(
        err.message().contains(&parent_version.to_hex()),
        "error must name the missing root: {err}"
    );
    assert!(!out.exists());
}

#[test]
fn budget_below_actual_is_refused_before_writing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    create(&mut store, "a.txt", "0123456789");
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let out = dir.path().join("materialized");

    let mut envelope = options(false);
    envelope.max_bytes = Some(1);
    let err = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &envelope,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::BudgetExceeded, "{err}");
    assert!(!out.exists());
}

#[test]
fn corrupted_logical_tensor_root_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let shape = [4u64, 4];
    let data: Vec<u8> = (0..32).map(|byte| byte as u8).collect();
    let tensor = store
        .write_tensor_file(
            root,
            b"t.bin",
            "bf16",
            &shape,
            &data,
            &TensorLayoutSpec::row_major(),
            Some(TensorStrategy::Raw),
        )
        .unwrap();
    let expected =
        tensor_logical_root(TensorDtype::Bf16, &shape, &data, &Limits::default()).unwrap();
    assert_eq!(store.tensor_logical_root(tensor).unwrap(), expected);
    let mut attrs = store.object_meta(tensor).unwrap().attrs;
    attrs.insert(
        "weft.tensor.logical_root".to_owned(),
        Value::Bytes(vec![0x5a; 32]),
    );
    store
        .setattr(
            tensor,
            SetAttr {
                attrs: Some(attrs),
                ..SetAttr::default()
            },
        )
        .unwrap();
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let out = dir.path().join("materialized");

    let err = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(true),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(!out.exists());
}

#[test]
fn missing_capability_grant_is_unauthorized() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    create(&mut store, "a.txt", "authorized bytes");
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let out = dir.path().join("materialized");
    let scope = CapabilityScope::new(
        "caller",
        vec!["other".to_owned()],
        Vec::new(),
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    let capability = CapabilityContext::new(scope).unwrap();

    let err = store
        .materialize_training_ready(
            "training",
            &out,
            Some(&capability),
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized, "{err}");
    assert!(!out.exists());
}

#[test]
fn second_run_into_existing_directory_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    create(&mut store, "a.txt", "once");
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let out = dir.path().join("materialized");
    store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap();
    let err = store
        .materialize_training_ready(
            "training",
            &out,
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Exists, "{err}");
}

#[test]
fn check_mode_writes_nothing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    create(&mut store, "a.txt", "checked only");
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, Vec::new());
    let report = store
        .check_training_ready(
            "training",
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap();
    assert!(report.out.is_none());
    assert!(report.cost.actual_objects >= 1);
    assert!(dir.path().join("store").is_dir());
    assert!(!dir.path().join("materialized").exists());
}

#[test]
fn cli_check_materialize_and_verify_out() {
    let dir = tempdir().unwrap();
    let store_path = dir.path().join("store");
    {
        let mut store = Store::init(&store_path, opts()).unwrap();
        create(&mut store, "dataset.bin", "cli surface bytes");
        let snapshot = store.checkpoint().unwrap();
        save_root(&mut store, "training", snapshot, Vec::new());
    }
    let out = dir.path().join("materialized");
    let check = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "--json",
            "training-ready",
            "--root",
            "training",
            "--check",
        ])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(report["preconditions"]["content_verified"], true);
    assert!(report["cost"]["actual_objects"].as_u64().unwrap() >= 1);

    let materialize = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .args([
            "--store",
            store_path.to_str().unwrap(),
            "--json",
            "materialize",
            "--root",
            "training",
            "--training-ready",
            "--out",
            out.to_str().unwrap(),
            "--require-tensors",
        ])
        .output()
        .unwrap();
    assert!(
        materialize.status.success(),
        "{}",
        String::from_utf8_lossy(&materialize.stderr)
    );
    let verify = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .args([
            "--json",
            "training-ready",
            "--verify-out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert!(report["files"].as_u64().unwrap() >= 1);
    assert_eq!(report["checkpoints_verified"], 0);
}

#[test]
fn causal_cycle_terminates() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let a = create(&mut store, "a.txt", "a");
    let b = create(&mut store, "b.txt", "b");
    // Content identities are stable across a metadata change, so they make a
    // real cycle: A's closure names B and B's closure names A.
    let a_content = store.object_meta(a).unwrap().content;
    let b_content = store.object_meta(b).unwrap().content;
    for (id, parent) in [(a, b_content), (b, a_content)] {
        let mut attrs = store.object_meta(id).unwrap().attrs;
        let closure = CausalClosure::new(vec![parent], vec![parent]).unwrap();
        let hash = closure.id().unwrap();
        weftfs::state::apply_causal_attrs(
            &mut attrs,
            &closure,
            weftcodec::context::IdentityMode::CausalExact,
            hash,
        )
        .unwrap();
        store
            .setattr(
                id,
                SetAttr {
                    attrs: Some(attrs),
                    ..SetAttr::default()
                },
            )
            .unwrap();
    }
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, vec![a_content, b_content]);
    let report = store
        .check_training_ready(
            "training",
            None,
            &Principal::root(),
            &Limits::default(),
            store.clock_now_ns(),
            &options(false),
        )
        .unwrap();
    assert!(report.preconditions.causal_complete);
}
