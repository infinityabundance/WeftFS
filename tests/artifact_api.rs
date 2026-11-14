//! T072: one explicit artifact API used by DCP, Orbax and an agent consumer.
//!
//! These tests exercise the Rust surface directly. The three Python consumers
//! and the byte-equivalence proof against the legacy path are exercised by
//! `scripts/artifact_api_court.sh`.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;
use weftcodec::context::{Budget, ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use weftfs::artifact_api::{ArtifactRef, ContextPlane, Materialized, SnapshotRef};
use weftfs::auth::Principal;
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

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

fn selector_from(text: &str) -> weftcodec::selector::Selector {
    selector::parse(text).unwrap()
}

fn root_for(_store: &Store, selector_text: &str, snapshot: weftfs::Hash32) -> ContextRoot {
    ContextRoot::new(
        &selector_from(selector_text),
        snapshot,
        DEFAULT_RESOLVER,
        Vec::new(),
        None,
        Some(Budget {
            max_results: Some(1000),
            max_bytes: None,
            max_depth: None,
            max_work: None,
        }),
    )
    .unwrap()
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
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

/// A small store with two files, an attribute selector, a causal edge, and a
/// snapshot. The causal entry is a real object id, so the plan is complete.
fn context_fixture() -> (tempfile::TempDir, Store, weftfs::Hash32) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let a = store
        .create_file_seeded(
            root,
            b"a.bin",
            b"alpha bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-a",
        )
        .unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert("task".to_owned(), Value::Str("summarize".to_owned()));
    store
        .create_file_seeded(
            root,
            b"b.bin",
            b"beta bytes, longer",
            0o644,
            0,
            0,
            attrs,
            b"seed-b",
        )
        .unwrap();
    // A causal edge from b to a (a's version id is accepted as an entry).
    let a_record = store.object_meta(a).unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert("task".to_owned(), Value::Str("summarize".to_owned()));
    store
        .create_file_seeded_causal(
            root,
            b"c.bin",
            b"gamma derived",
            0o644,
            0,
            0,
            attrs,
            b"seed-c",
            &[a_record.version],
        )
        .unwrap();
    store
        .relation_add(
            a,
            "derived_from",
            RelationTarget::Object(a),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    let snapshot = store.snapshot_create("jan").unwrap();
    (dir, store, snapshot)
}

#[test]
fn resolve_plan_materialize_verify_is_exact_and_snapshot_scoped() {
    let (_dir, mut store, snapshot) = context_fixture();
    let root = root_for(&store, "select attrs.task == \"summarize\"", snapshot);
    store
        .save_context_root("task-root", &root, None, false)
        .unwrap();

    let plane = ContextPlane::from_store(store, Principal::root());
    let view = plane
        .observe_snapshot(&SnapshotRef::Name("jan".into()))
        .unwrap();
    assert_eq!(view.snapshot, snapshot);
    assert!(!view.provisional);

    let resolution = plane
        .resolve(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "task-root".into(),
            },
        )
        .unwrap();
    assert_eq!(resolution.kind, "context_root");
    assert_eq!(resolution.matched_count, 2);
    assert_eq!(resolution.entries.len(), 2);

    let plan = plane
        .plan(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "task-root".into(),
            },
        )
        .unwrap();
    assert!(plan.complete, "missing: {:?}", plan.missing);
    assert_eq!(plan.snapshot, snapshot);
    assert!(!plan.provisional);

    let out = _dir.path().join("materialized");
    let materialized = plane
        .materialize(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "task-root".into(),
            },
            &out,
        )
        .unwrap();
    let Materialized::Directory(outcome) = materialized else {
        panic!("context root materializes to a directory");
    };
    assert_eq!(outcome.files, 2);
    assert_eq!(fs::read(out.join("b.bin")).unwrap(), b"beta bytes, longer");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join(".weft-artifact.json")).unwrap()).unwrap();
    assert_eq!(manifest["plan_root"], outcome.plan_root.to_hex());

    let verification = plane
        .verify(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "task-root".into(),
            },
            Some(&out),
        )
        .unwrap();
    assert!(verification.ok, "problems: {:?}", verification.problems);
    assert_eq!(verification.store_checked, 2);
    assert_eq!(verification.output_checked, 2);

    // A different pinned snapshot is refused rather than retargeted.
    let mut writable = plane;
    let feb = writable.store_mut().snapshot_create("feb").unwrap();
    let other = writable.observe_snapshot(&SnapshotRef::Id(feb)).unwrap();
    let err = writable
        .resolve(
            other,
            &ArtifactRef::ContextRoot {
                name_or_uri: "task-root".into(),
            },
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);

    // A missing snapshot is a typed SnapshotMissing, never the head.
    let err = writable
        .observe_snapshot(&SnapshotRef::Id(weftfs::Hash32::from_bytes([0x55; 32])))
        .unwrap_err();
    assert_eq!(err.class(), Class::SnapshotMissing);
    let err = writable
        .observe_snapshot(&SnapshotRef::Name("no-such-snapshot".into()))
        .unwrap_err();
    assert_eq!(err.class(), Class::SnapshotMissing);
}

#[test]
fn materialize_refuses_a_missing_declared_dependency_without_output() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    // A causal entry that names no object in the store: a declared dependency
    // that the planner cannot satisfy.
    let phantom = weftfs::Hash32::from_bytes([0x42; 32]);
    store
        .create_file_seeded_causal(
            root,
            b"orphan.bin",
            b"bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-orphan",
            &[phantom],
        )
        .unwrap();
    let snapshot = store.snapshot_create("jan").unwrap();
    let context = root_for(&store, "select true", snapshot);
    store
        .save_context_root("all", &context, None, false)
        .unwrap();

    let plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Id(snapshot)).unwrap();
    let plan = plane
        .plan(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "all".into(),
            },
        )
        .unwrap();
    assert!(!plan.complete, "plan={plan:?}");
    assert_eq!(plan.missing.len(), 1);
    assert_eq!(plan.missing[0].root, phantom);

    let out = dir.path().join("out");
    let err = plane
        .materialize(
            view,
            &ArtifactRef::ContextRoot {
                name_or_uri: "all".into(),
            },
            &out,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::DependencyMismatch);
    assert!(!out.exists(), "a refused materialization publishes nothing");
}

#[test]
fn head_view_is_provisional_and_never_pinned() {
    let (_dir, store, _snapshot) = context_fixture();
    let mut plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Head).unwrap();
    assert!(view.provisional);
    let err = plane
        .pin(
            view,
            &ArtifactRef::Object {
                id: plane.store().resolve_path("/a.bin").unwrap(),
            },
            0,
            60_000,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
}

#[test]
fn pin_release_are_durable_gc_roots_and_one_commit_each() {
    let (_dir, mut store, snapshot) = context_fixture();
    let root = root_for(&store, "select true", snapshot);
    store.save_context_root("all", &root, None, false).unwrap();
    let object = store.resolve_path("/a.bin").unwrap();
    let before = store.generation();

    let mut plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Id(snapshot)).unwrap();
    let artifact = ArtifactRef::Object { id: object };
    let pinned = plane.pin(view, &artifact, 3, 120_000).unwrap();
    assert!(pinned.marker_created);
    assert_eq!(pinned.commit, plane.store().head());
    let after_pin = plane.store().generation();
    assert_eq!(after_pin, before + 1);

    // Idempotent: the same pin publishes nothing new.
    let again = plane.pin(view, &artifact, 3, 120_000).unwrap();
    assert!(!again.marker_created);
    assert_eq!(plane.store().generation(), after_pin);

    // A GC sweep retains both the pinned marker and the object's bytes.
    let gc = plane.store_mut().gc(true).unwrap();
    assert!(gc.problems.is_empty(), "gc problems: {:?}", gc.problems);
    assert!(plane.store().resolve_path("/pins").is_ok());
    assert!(plane
        .store()
        .read_object(object)
        .is_ok_and(|bytes| bytes == b"alpha bytes"));

    let released = plane.release(view, &artifact).unwrap();
    assert!(released.marker_removed);
    assert!(released.reclaimable_after_gc);
    assert_eq!(plane.store().generation(), after_pin + 1);
    assert!(plane
        .store()
        .lookup(
            plane.store().resolve_path("/pins").unwrap(),
            pinned.root.to_hex().as_bytes()
        )
        .is_err());
}

#[test]
fn one_transaction_publishes_every_pin_in_one_commit_and_rolls_back() {
    let (_dir, store, snapshot) = context_fixture();
    let a = store.resolve_path("/a.bin").unwrap();
    let bb = store.resolve_path("/b.bin").unwrap();
    let mut plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Id(snapshot)).unwrap();
    let before = plane.store().generation();

    let (summaries, commit) = plane
        .transaction(|txn| {
            let first = txn.pin(view, &ArtifactRef::Object { id: a }, 1, 60_000)?;
            let second = txn.pin(view, &ArtifactRef::Object { id: bb }, 1, 60_000)?;
            Ok((first.root, second.root))
        })
        .unwrap();
    assert_eq!(commit, plane.store().head());
    assert_eq!(plane.store().generation(), before + 1);
    assert_ne!(summaries.0, summaries.1);

    // A failing transaction publishes nothing at all: the third pin is rolled
    // back and no marker for it exists afterwards.
    let third = plane.store().resolve_path("/c.bin").unwrap();
    let before_failure = plane.store().generation();
    let err = plane
        .transaction(|txn| {
            txn.pin(view, &ArtifactRef::Object { id: third }, 5, 60_000)?;
            Err::<(), _>(weftfs::Error::invalid_argument("deliberate rollback"))
        })
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
    assert_eq!(plane.store().generation(), before_failure);
    let pins = plane.store().resolve_path("/pins").unwrap();
    let third_root = plane
        .resolve(view, &ArtifactRef::Object { id: third })
        .unwrap()
        .entries[0]
        .content_root;
    assert!(plane
        .store()
        .lookup(pins, third_root.to_hex().as_bytes())
        .is_err());
}

#[test]
fn checkpoint_tensor_and_directory_materialization_are_byte_exact() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let shape = [8u64, 4];
    let first = random_bytes(64, 0x11);
    let second = random_bytes(32, 0x22);
    let container = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: "layer.weight",
                dtype: "bf16",
                shape: &shape,
                data: &first,
            },
            SafetensorsEntry {
                name: "layer.bias",
                dtype: "bf16",
                shape: &[16],
                data: &second,
            },
        ],
        None,
    )
    .unwrap();
    let source = dir.path().join("model.safetensors");
    fs::write(&source, &container).unwrap();
    store
        .write_checkpoint(
            "model",
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let _snapshot = store.snapshot_create("ckpt").unwrap();

    let plane = ContextPlane::from_store(store, Principal::root());
    let view = plane
        .observe_snapshot(&SnapshotRef::Name("ckpt".into()))
        .unwrap();
    let artifact = ArtifactRef::Checkpoint {
        name: "model".into(),
        tensor: None,
    };
    let resolution = plane.resolve(view, &artifact).unwrap();
    assert_eq!(resolution.tensor_count, 2);
    assert_eq!(resolution.entries.len(), 2);

    // One tensor through the streaming consumer path is exactly the stored
    // tensor bytes.
    let tensor = plane
        .materialize_tensor(view, "model", "layer.weight")
        .unwrap();
    assert_eq!(tensor.bytes, first);
    assert!(tensor.report.full_root_verified);

    // A whole checkpoint materializes every tensor as a file and verifies.
    let out = dir.path().join("ckpt-out");
    let materialized = plane.materialize(view, &artifact, &out).unwrap();
    let Materialized::Directory(outcome) = materialized else {
        panic!("checkpoint materializes to a directory");
    };
    assert_eq!(outcome.files, 2);
    assert_eq!(fs::read(out.join("layer.weight")).unwrap(), first);
    assert_eq!(fs::read(out.join("layer.bias")).unwrap(), second);
    let verification = plane.verify(view, &artifact, Some(&out)).unwrap();
    assert!(verification.ok, "problems: {:?}", verification.problems);

    // Tampering with one produced file is detected.
    fs::write(out.join("layer.bias"), b"tampered").unwrap();
    let verification = plane.verify(view, &artifact, Some(&out)).unwrap();
    assert!(!verification.ok);
    assert!(verification
        .problems
        .iter()
        .any(|problem| problem.entry == "layer.bias"));

    // A missing shard object is a plan-level missing dependency, and
    // materialization refuses before writing anything.
    let shard_object = plane
        .resolve(view, &artifact)
        .unwrap()
        .entries
        .iter()
        .flat_map(|entry| entry.dependency_objects.clone())
        .next()
        .unwrap();
    // Remove one shard object through its directory entry; the plan must then
    // report the now-absent shard and materialization must refuse.
    let mut plane = plane;
    let (parent, name) = plane
        .store()
        .current_state()
        .entries
        .values()
        .find(|entry| entry.target == shard_object)
        .map(|entry| (entry.parent, entry.name.clone()))
        .expect("the shard object has a directory entry");
    plane.store_mut().unlink(parent, &name).unwrap();
    let snapshot2 = plane.store_mut().snapshot_create("broken").unwrap();
    let broken_view = plane.observe_snapshot(&SnapshotRef::Id(snapshot2)).unwrap();
    let plan = plane.plan(broken_view, &artifact).unwrap();
    assert!(!plan.complete);
    let fresh = dir.path().join("broken-out");
    let err = plane
        .materialize(broken_view, &artifact, &fresh)
        .unwrap_err();
    assert!(
        matches!(err.class(), Class::DependencyMismatch | Class::NotFound),
        "unexpected class: {err}"
    );
    assert!(!fresh.exists());
}

#[test]
fn pin_field_is_written_once_and_holds_a_canonical_record() {
    let (_dir, mut store, snapshot) = context_fixture();
    let root = root_for(&store, "select true", snapshot);
    store.save_context_root("all", &root, None, false).unwrap();
    let mut plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Id(snapshot)).unwrap();
    let artifact = ArtifactRef::ContextRoot {
        name_or_uri: "all".into(),
    };
    let pinned = plane.pin(view, &artifact, 7, 90_000).unwrap();
    let pins = plane.store().resolve_path("/pins").unwrap();
    let id = plane
        .store()
        .lookup(pins, pinned.root.to_hex().as_bytes())
        .unwrap();
    let bytes = plane.store().read_object(id).unwrap();
    let value = weftcodec::wcb::decode(&bytes, &Limits::default()).unwrap();
    let weftcodec::wcb::Value::Map(map) = value else {
        panic!("pin marker is a map");
    };
    assert_eq!(
        map.get("schema"),
        Some(&Value::Str("weft.artifact.pin.v1".to_owned()))
    );
    assert_eq!(map.get("policy_epoch"), Some(&Value::U64(7)));
    assert_eq!(
        weftcodec::wcb::encode(&Value::Map(map), &Limits::default()).unwrap(),
        bytes,
        "the pin marker is canonical WCB"
    );
}

#[test]
fn a_directory_object_materializes_as_a_directory() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let sub = store
        .mkdir_seeded(root, b"sub", 0o755, 0, 0, b"seed-sub")
        .unwrap();
    store
        .create_file_seeded(
            sub,
            b"x.bin",
            b"inside",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-x",
        )
        .unwrap();
    let snapshot = store.snapshot_create("s").unwrap();
    let plane = ContextPlane::from_store(store, Principal::root());
    let view = plane.observe_snapshot(&SnapshotRef::Id(snapshot)).unwrap();
    let out = dir.path().join("dir-out");
    plane
        .materialize(view, &ArtifactRef::Object { id: sub }, &out)
        .unwrap();
    assert!(out.join("sub").is_dir());
}
