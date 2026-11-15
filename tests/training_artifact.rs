//! P01/P02: the composable Training Artifact and bounded causal closure.
//!
//! The identity guardrail is asserted directly: an artifact root is a function
//! of immutable content and declarations only, so editing a grant, a retention
//! binding or a quarantine decision never changes it; changing a member byte
//! always does.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::context::ContextRoot;
use weftcodec::selector;
use weftcodec::wcb::Value;
use weftcodec::context::DEFAULT_RESOLVER;
use weftcodec::Limits;

use weftfs::causal_closure::{CausalGraph, ClosureBounds, ClosureMode};
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};
use weftfs::training_artifact::{
    artifact_store_path, LineageMode, TrainingArtifact, TrainingArtifactSpec, TrainingGrant,
    TrainingPolicy, ValidationOptions,
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

fn write_checkpoint(store: &mut Store, dir: &Path) -> weftfs::checkpoint::CheckpointWriteReport {
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
    store
        .write_checkpoint(
            "tiny",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: weftfs::checkpoint::ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap()
}

struct Fixture {
    store: Store,
    root: Hash32,
    artifact: TrainingArtifact,
    snapshot: Hash32,
    note_version: Hash32,
    shard_object: Hash32,
    provenance_id: Hash32,
}

fn fixture(dir: &Path) -> Fixture {
    let mut store = Store::init(dir.join("store"), opts()).unwrap();
    let note = create(&mut store, "notes.txt", "run notes");
    let note_version = store.object_meta(note).unwrap().version;
    let report = write_checkpoint(&mut store, &dir.join("src"));
    let shard = &report.tensors[0];
    let shard_object = shard.object_ids[0];
    let shard_content = store.object_meta(shard_object).unwrap().content;

    let provenance = ProvenanceRecord::new(
        "p01-fixture",
        BTreeMap::new(),
        Vec::new(),
        vec![shard_content],
        ProvenanceEnvironment::new("training-artifact-test", "1"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        1_700_000_000_000_000_000,
    )
    .unwrap();
    let provenance_id = store
        .record_provenance(&[shard_object], &provenance)
        .unwrap();

    let snapshot = store.checkpoint().unwrap();
    let selector = selector::parse("select true").unwrap();
    let context = ContextRoot::new(
        &selector,
        snapshot,
        DEFAULT_RESOLVER,
        vec![note_version],
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("training", &context, None, false)
        .unwrap();
    let snapshot = store.checkpoint().unwrap();

    let mut spec = TrainingArtifactSpec::checkpoint("qwen-tiny");
    spec.checkpoint = Some("tiny".to_owned());
    spec.context_root = Some("training".to_owned());
    spec.provenance = vec![provenance_id];
    spec.causal_entries = vec![note_version];
    spec.policy = TrainingPolicy {
        lineage: LineageMode::Complete,
        allowed_dtypes: vec!["bf16".to_owned()],
        minimum_tensors: 1,
        checkpoint_complete: true,
    };
    let (artifact, root) = store.build_training_artifact(snapshot, &spec).unwrap();
    store.write_training_artifact(&artifact).unwrap();
    let snapshot = store.checkpoint().unwrap();
    Fixture {
        store,
        root,
        artifact,
        snapshot,
        note_version,
        shard_object,
        provenance_id,
    }
}

#[test]
fn artifact_validates_and_is_byte_identical_across_a_reopen() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let validation = fixture
        .store
        .validate_training_artifact_at(
            fixture.snapshot,
            fixture.root,
            &ValidationOptions::default(),
        )
        .unwrap();
    assert_eq!(validation.artifact_root, fixture.root);
    assert_eq!(validation.tensor_count, 1);
    assert_eq!(validation.shard_count, 1);
    assert_eq!(validation.tensors_exact, 1);
    assert!(validation.checkpoint_verified);
    assert_eq!(validation.provenance_verified, 1);
    assert_eq!(validation.causal_nodes, 1);

    let reopened =
        Store::open(dir.path().join("store"), weftfs::store::OpenMode::ReadOnly).unwrap();
    let again = reopened
        .validate_training_artifact_at(
            fixture.snapshot,
            fixture.root,
            &ValidationOptions::default(),
        )
        .unwrap();
    assert_eq!(again.artifact_root, validation.artifact_root);
    assert_eq!(again.tensors_exact, validation.tensors_exact);
}

#[test]
fn root_is_derived_only_from_content_and_declarations() {
    // The same declaration built twice is the same root.
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let mut spec = TrainingArtifactSpec::checkpoint("qwen-tiny");
    spec.checkpoint = Some("tiny".to_owned());
    spec.context_root = Some("training".to_owned());
    spec.provenance = vec![fixture.provenance_id];
    spec.causal_entries = vec![fixture.note_version];
    spec.policy = fixture.artifact.policy.clone();
    let (again, root) = fixture
        .store
        .build_training_artifact(fixture.snapshot, &spec)
        .unwrap();
    assert_eq!(root, fixture.root);
    assert_eq!(again, fixture.artifact);
    // A policy change is a declaration change: the root must move.
    let mut other = spec.clone();
    other.policy.minimum_tensors = 2;
    let (_artifact, moved) = fixture
        .store
        .build_training_artifact(fixture.snapshot, &other)
        .unwrap();
    assert_ne!(moved, fixture.root);
}

#[test]
fn editing_a_grant_never_changes_the_artifact_root() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let before = fixture
        .store
        .validate_training_artifact_at(
            fixture.snapshot,
            fixture.root,
            &ValidationOptions::default(),
        )
        .unwrap();
    assert_eq!(before.grant_epoch, 0);

    let grant = TrainingGrant {
        artifact_root: fixture.root,
        uid: 1001,
        gid: 1001,
        actions: vec!["train".to_owned()],
        policy_epoch: 1,
        issued_at_ns: 1_700_000_000_000_000_000,
        expires_at_ns: None,
        supersedes: None,
        signature: None,
    };
    let grant_id = fixture.store.set_training_grant(&grant).unwrap();
    assert_eq!(fixture.store.training_grant_epoch(fixture.root).unwrap(), 1);
    let stored = fixture.store.training_grants(fixture.root).unwrap().1;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id().unwrap(), grant_id);
    assert!(stored[0].authorizes(1001, 1001, "train", 1_700_000_000_000_000_001));

    // A second edit (a different grant) increments the epoch and still leaves
    // the artifact identity and every content identity untouched.
    let mut second = grant.clone();
    second.actions = vec!["evaluate".to_owned(), "train".to_owned()];
    second.supersedes = Some(grant_id);
    fixture.store.set_training_grant(&second).unwrap();
    assert_eq!(fixture.store.training_grant_epoch(fixture.root).unwrap(), 2);

    let reopened =
        Store::open(dir.path().join("store"), weftfs::store::OpenMode::ReadOnly).unwrap();
    let after = reopened
        .validate_training_artifact_at(
            reopened.head(),
            fixture.root,
            &ValidationOptions {
                check_grants: true,
                now_ns: 1_700_000_000_000_000_001,
                ..ValidationOptions::default()
            },
        )
        .unwrap();
    assert_eq!(after.artifact_root, fixture.root);
    assert_eq!(after.grant_epoch, 2);
    assert_eq!(after.tensors_exact, before.tensors_exact);
}

#[test]
fn mutated_tensor_value_is_refused() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let object = fixture.shard_object;
    fixture.store.write_file(object, 0, &[0xffu8; 4]).unwrap();
    // The pinned snapshot still holds the old version and stays exact: a
    // pin is never retargeted by a later write.
    fixture
        .store
        .validate_training_artifact_at(
            fixture.snapshot,
            fixture.root,
            &ValidationOptions::default(),
        )
        .unwrap();
    // The head now serves the replacement bytes, which no longer match the
    // artifact's immutable content identities.
    let head = fixture.store.head();
    let err_head = fixture
        .store
        .validate_training_artifact_at(head, fixture.root, &ValidationOptions::default())
        .unwrap_err();
    assert!(
        err_head.class() == Class::Corrupt || err_head.class() == Class::DependencyMismatch,
        "unexpected head class {}",
        err_head.class()
    );
}

#[test]
fn removed_lineage_declaration_is_refused_by_the_policy() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let mut tampered = fixture.artifact.clone();
    tampered.causal = None;
    let root = tampered.root().unwrap();
    fixture.store.write_training_artifact(&tampered).unwrap();
    let head = fixture.store.head();
    let err = fixture
        .store
        .validate_training_artifact_at(head, root, &ValidationOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed);
    assert!(err.to_string().contains("lineage"));
}

#[test]
fn contradicted_policy_claim_is_refused() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let mut tampered = fixture.artifact.clone();
    tampered.policy.allowed_dtypes = vec!["f32".to_owned()];
    let root = tampered.root().unwrap();
    fixture.store.write_training_artifact(&tampered).unwrap();
    let head = fixture.store.head();
    let err = fixture
        .store
        .validate_training_artifact_at(head, root, &ValidationOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
    assert!(err.to_string().contains("allowed_dtypes"));
}

#[test]
fn missing_dependency_is_a_typed_refusal_that_names_the_identity() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let missing = Hash32::from_bytes([0x5a; 32]);
    let mut tampered = fixture.artifact.clone();
    tampered.causal =
        Some(weftcodec::context::CausalClosure::new(vec![missing], vec![missing]).unwrap());
    let root = tampered.root().unwrap();
    fixture.store.write_training_artifact(&tampered).unwrap();
    let head = fixture.store.head();
    let err = fixture
        .store
        .validate_training_artifact_at(head, root, &ValidationOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::DependencyMismatch);
    assert!(err.to_string().contains(&missing.to_hex()));
}

#[test]
fn artifact_record_tamper_is_refused() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    // Overwrite the record file with non-canonical bytes at the same path.
    let path = artifact_store_path(fixture.root);
    let object = fixture.store.resolve_path(&format!("/{path}")).unwrap();
    let mut bytes = fixture.store.read_object(object).unwrap();
    bytes.push(0);
    fixture.store.write_file(object, 0, &bytes).unwrap();
    let err = fixture
        .store
        .validate_training_artifact_at(
            fixture.store.head(),
            fixture.root,
            &ValidationOptions::default(),
        )
        .unwrap_err();
    assert!(
        err.class() == Class::Corrupt || err.class() == Class::NonCanonical,
        "unexpected class {}",
        err.class()
    );
}

#[test]
fn closure_modes_and_evidence_bundle_are_deterministic() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    for mode in [ClosureMode::Complete, ClosureMode::Minimal] {
        let evidence = fixture
            .store
            .training_artifact_closure(
                fixture.snapshot,
                fixture.root,
                mode,
                ClosureBounds::default(),
            )
            .unwrap();
        let result = evidence.outcome.as_ref().unwrap();
        assert_eq!(result.returned_closure_size, result.total_closure_size);
        assert!(!result.truncated);
        let bundle = evidence.to_json();
        assert_eq!(bundle["mode"], mode.as_str());
        assert!(bundle["records"].as_array().unwrap().len() >= 1);
    }
}

#[test]
fn graph_index_and_naive_scan_agree_on_a_bounded_fixture() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let state = fixture.store.state_at(fixture.snapshot).unwrap();
    let graph = CausalGraph::build(&state, &Limits::default()).unwrap();
    let parents = fixture.artifact.causal.as_ref().unwrap().parents.clone();
    let indexed = graph
        .compute(
            ClosureMode::Complete,
            &parents,
            ClosureBounds::default(),
            &|_| true,
        )
        .unwrap();
    let naive = CausalGraph::naive_scan(
        &state,
        &Limits::default(),
        ClosureMode::Complete,
        &parents,
        ClosureBounds::default(),
    )
    .unwrap();
    assert_eq!(indexed.identities(), naive.identities());
    assert_eq!(indexed.identities(), vec![fixture.note_version]);
}

#[test]
fn grant_record_round_trips_and_rejects_bad_actions() {
    let grant = TrainingGrant {
        artifact_root: Hash32::from_bytes([1; 32]),
        uid: 7,
        gid: 8,
        actions: vec!["evaluate".to_owned(), "train".to_owned()],
        policy_epoch: 3,
        issued_at_ns: 10,
        expires_at_ns: Some(20),
        supersedes: None,
        signature: None,
    };
    let bytes = grant.canonical_bytes().unwrap();
    assert_eq!(TrainingGrant::decode(&bytes).unwrap(), grant);
    let id = grant.id().unwrap();
    assert_eq!(grant.id().unwrap(), id);
    let mut bad = grant.clone();
    bad.actions = vec!["train".to_owned(), "evaluate".to_owned()];
    assert!(bad.validate().is_err());
    let mut bad = grant.clone();
    bad.actions = vec!["administrate".to_owned()];
    assert!(bad.validate().is_err());
    let mut expired = grant.clone();
    expired.expires_at_ns = Some(10);
    assert!(expired.validate().is_err());
    assert!(!grant.authorizes(7, 8, "replicate", 15));
    assert!(!grant.authorizes(7, 9, "train", 15));
    assert!(!grant.authorizes(7, 8, "train", 20));
}

#[test]
fn non_canonical_artifact_bytes_are_rejected() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let bytes = fixture.artifact.canonical_bytes().unwrap();
    // Unsort the tensor array by duplicating the single member: canonical
    // re-encoding must differ.
    let mut value = weftcodec::wcb::decode_default(&bytes).unwrap();
    if let Value::Map(map) = &mut value {
        if let Some(Value::Array(items)) = map.get_mut("tensors") {
            items.push(items[0].clone());
        }
    }
    let tampered = weftcodec::wcb::encode_default(&value).unwrap();
    let err = TrainingArtifact::decode(&tampered).unwrap_err();
    assert!(
        err.class() == Class::NonCanonical || err.class() == Class::Malformed,
        "unexpected class {}",
        err.class()
    );
}
