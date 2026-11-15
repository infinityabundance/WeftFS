//! P01 fixture and tamper harness for the cross-language Training Artifact
//! court. It uses the shipped store API, never a test-only shortcut.
//!
//!   cargo run --release --example training_artifact_fixture -- init DIR
//!   ... tamper-value|tamper-policy|tamper-lineage|tamper-missing DIR ROOT
//!   ... grant DIR ROOT
//!   ... closure DIR ROOT complete|minimal OUTFILE
//!
//! `init` prints one JSON line naming every identity the court needs.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use weftcodec::context::ContextRoot;
use weftcodec::context::DEFAULT_RESOLVER;
use weftcodec::selector;
use weftfs::causal_closure::{ClosureBounds, ClosureMode};
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::error::Result;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};
use weftfs::training_artifact::{LineageMode, TrainingArtifactSpec, TrainingGrant, TrainingPolicy};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        ..Options::default()
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let verb = args.next().unwrap_or_default();
    match verb.as_str() {
        "init" => init(PathBuf::from(args.next().expect("store dir"))),
        "tamper-value" => tamper_value(store(&mut args), root(&mut args)),
        "tamper-policy" => tamper_policy(store(&mut args), root(&mut args)),
        "tamper-lineage" => tamper_lineage(store(&mut args), root(&mut args)),
        "tamper-missing" => tamper_missing(store(&mut args), root(&mut args)),
        "grant" => grant(store(&mut args), root(&mut args)),
        "closure" => {
            let dir = store(&mut args);
            let root = root(&mut args);
            let mode = ClosureMode::parse(&args.next().expect("mode"))?;
            let out = PathBuf::from(args.next().expect("out file"));
            closure(dir, root, mode, out)
        }
        other => Err(weftfs::error::Error::invalid_argument(format!(
            "unknown verb `{other}`"
        ))),
    }
}

fn store(args: &mut impl Iterator<Item = String>) -> PathBuf {
    PathBuf::from(args.next().expect("store dir"))
}

fn root(args: &mut impl Iterator<Item = String>) -> Hash32 {
    Hash32::from_hex(&args.next().expect("artifact root")).expect("artifact root hex")
}

fn print_json(value: serde_json::Value) {
    println!("{}", serde_json::to_string(&value).expect("json"));
}

fn init(dir: PathBuf) -> Result<()> {
    fs::create_dir_all(&dir)?;
    let mut store = Store::init(dir.join("store"), opts())?;

    // Two notes so the causal closure has a real edge (n2 -> n1).
    let note1 = store.create_file(
        store.resolve_path("/")?,
        b"note1.txt",
        b"lineage root",
        0o644,
        0,
        0,
        BTreeMap::new(),
    )?;
    let note1_version = store.object_meta(note1)?.version;
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "weft.role".to_owned(),
        weftcodec::wcb::Value::Str("run".to_owned()),
    );
    let seed = weftfs::identity::os_random_32()?;
    let note2 = store.create_file_seeded_causal(
        store.resolve_path("/")?,
        b"note2.txt",
        b"derived from note1",
        0o644,
        0,
        0,
        attrs,
        &seed,
        &[note1_version],
    )?;
    let note2_version = store.object_meta(note2)?.version;

    // One real checkpoint with one bf16 tensor.
    let shape = [8u64, 4];
    let data: Vec<u8> = (0..32u16).flat_map(|value| value.to_le_bytes()).collect();
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "bf16",
            shape: &shape,
            data: &data,
        }],
        None,
    )?;
    let source_dir = dir.join("src");
    fs::create_dir_all(&source_dir)?;
    let source = source_dir.join("tiny.safetensors");
    fs::write(&source, &file)?;
    let checkpoint = store.write_checkpoint(
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
    )?;
    let shard_object = checkpoint.tensors[0].object_ids[0];

    let provenance = ProvenanceRecord::new(
        "training-artifact-fixture",
        BTreeMap::new(),
        Vec::new(),
        vec![store.object_meta(shard_object)?.content],
        ProvenanceEnvironment::new("training-artifact-fixture", env!("CARGO_PKG_VERSION")),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        1_700_000_000_000_000_000,
    )?;
    let provenance_id = store.record_provenance(&[shard_object], &provenance)?;

    let snapshot = store.checkpoint()?;
    let selector = selector::parse("select true")?;
    let context = ContextRoot::new(
        &selector,
        snapshot,
        DEFAULT_RESOLVER,
        vec![note2_version],
        None,
        None,
    )?;
    store.save_context_root("training", &context, None, false)?;
    let snapshot = store.checkpoint()?;

    let mut spec = TrainingArtifactSpec::checkpoint("tiny-real");
    spec.checkpoint = Some("tiny".to_owned());
    spec.context_root = Some("training".to_owned());
    spec.provenance = vec![provenance_id];
    spec.causal_entries = vec![note2_version];
    spec.policy = TrainingPolicy {
        lineage: LineageMode::Complete,
        allowed_dtypes: vec!["bf16".to_owned()],
        minimum_tensors: 1,
        checkpoint_complete: true,
    };
    let (artifact, artifact_root) = store.build_training_artifact(snapshot, &spec)?;
    store.write_training_artifact(&artifact)?;
    let snapshot = store.checkpoint()?;

    print_json(serde_json::json!({
        "ok": true,
        "store": dir.join("store").display().to_string(),
        "artifact_root": artifact_root.to_hex(),
        "snapshot": snapshot.to_hex(),
        "checkpoint": "tiny",
        "shard_object": shard_object.to_hex(),
        "note1": note1.to_hex(),
        "note1_version": note1_version.to_hex(),
        "note2": note2.to_hex(),
        "note2_version": note2_version.to_hex(),
        "provenance_id": provenance_id.to_hex(),
        "tensor_count": artifact.tensors.len(),
        "shard_count": artifact.shard_count(),
        "element_bytes": artifact.total_element_bytes(),
    }));
    Ok(())
}

fn build_variant(
    dir: &PathBuf,
    artifact_root: Hash32,
    change: impl FnOnce(&mut weftfs::training_artifact::TrainingArtifact),
) -> Result<(Hash32, Hash32)> {
    let mut store = Store::open(dir.join("store"), weftfs::store::OpenMode::ReadWrite)?;
    let head = store.head();
    let state = store.state_at(head)?;
    let (mut artifact, _) = store.load_training_artifact_at(&state, Some(head), artifact_root)?;
    change(&mut artifact);
    let new_root = artifact.root()?;
    store.write_training_artifact(&artifact)?;
    let snapshot = store.checkpoint()?;
    Ok((new_root, snapshot))
}

fn tamper_value(dir: PathBuf, artifact_root: Hash32) -> Result<()> {
    let mut store = Store::open(dir.join("store"), weftfs::store::OpenMode::ReadWrite)?;
    let head = store.head();
    let state = store.state_at(head)?;
    let (artifact, _) = store.load_training_artifact_at(&state, Some(head), artifact_root)?;
    let object = artifact.tensors[0].shards[0].object;
    store.write_file(object, 0, &[0xff, 0xff, 0xff, 0xff])?;
    let snapshot = store.checkpoint()?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": artifact_root.to_hex(),
        "snapshot": snapshot.to_hex(),
        "tampered_object": object.to_hex(),
    }));
    Ok(())
}

fn tamper_policy(dir: PathBuf, artifact_root: Hash32) -> Result<()> {
    let (new_root, snapshot) = build_variant(&dir, artifact_root, |artifact| {
        artifact.policy.allowed_dtypes = vec!["f64".to_owned()];
    })?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": new_root.to_hex(),
        "original_root": artifact_root.to_hex(),
        "snapshot": snapshot.to_hex(),
    }));
    Ok(())
}

fn tamper_lineage(dir: PathBuf, artifact_root: Hash32) -> Result<()> {
    let (new_root, snapshot) = build_variant(&dir, artifact_root, |artifact| {
        artifact.causal = None;
    })?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": new_root.to_hex(),
        "original_root": artifact_root.to_hex(),
        "snapshot": snapshot.to_hex(),
    }));
    Ok(())
}

fn tamper_missing(dir: PathBuf, artifact_root: Hash32) -> Result<()> {
    let missing = Hash32::from_bytes([0x5a; 32]);
    let (new_root, snapshot) = build_variant(&dir, artifact_root, |artifact| {
        artifact.causal = Some(
            weftcodec::context::CausalClosure::new(vec![missing], vec![missing])
                .expect("causal closure"),
        );
    })?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": new_root.to_hex(),
        "original_root": artifact_root.to_hex(),
        "snapshot": snapshot.to_hex(),
        "missing": missing.to_hex(),
    }));
    Ok(())
}

fn grant(dir: PathBuf, artifact_root: Hash32) -> Result<()> {
    let mut store = Store::open(dir.join("store"), weftfs::store::OpenMode::ReadWrite)?;
    let grant = TrainingGrant {
        artifact_root,
        uid: 4242,
        gid: 4242,
        actions: vec!["train".to_owned()],
        policy_epoch: 1,
        issued_at_ns: 1_700_000_000_000_000_000,
        expires_at_ns: None,
        supersedes: None,
        signature: None,
    };
    let id = store.set_training_grant(&grant)?;
    let epoch = store.training_grant_epoch(artifact_root)?;
    let snapshot = store.checkpoint()?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": artifact_root.to_hex(),
        "grant_id": id.to_hex(),
        "grant_epoch": epoch,
        "snapshot": snapshot.to_hex(),
    }));
    Ok(())
}

fn closure(dir: PathBuf, artifact_root: Hash32, mode: ClosureMode, out: PathBuf) -> Result<()> {
    let store = Store::open(dir.join("store"), weftfs::store::OpenMode::ReadOnly)?;
    let snapshot = store.head();
    let evidence =
        store.training_artifact_closure(snapshot, artifact_root, mode, ClosureBounds::default())?;
    let bundle = evidence.to_json();
    fs::write(&out, serde_json::to_vec_pretty(&bundle).expect("json"))?;
    print_json(serde_json::json!({
        "ok": true,
        "artifact_root": artifact_root.to_hex(),
        "mode": mode.as_str(),
        "bundle": out.display().to_string(),
        "refusal": bundle["refusal"].clone(),
    }));
    Ok(())
}
