//! P03: the ready-state cache is invalidated by every input that can change
//! the answer, and a stale or tampered entry re-verifies instead of passing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::context::{ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::{Limits, TensorDtype};

use weftfs::auth::Principal;
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::ready_cache::ReadyCacheOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};
use weftfs::taint::{QuarantineMode, QuarantineRequest};
use weftfs::training_artifact::TrainingGrant;
use weftfs::training_ready::TrainingReadyOptions;

const SIGNER: [u8; 32] = [0x31; 32];

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

fn signing_key(seed: &[u8; 32]) -> [u8; 32] {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

fn create(store: &mut Store, name: &str, text: &str) -> Hash32 {
    store
        .create_file(
            store.resolve_path("/").unwrap(),
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
    let data: Vec<u8> = (0..32u16).flat_map(|value| value.to_le_bytes()).collect();
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
    root_name: String,
    note_object: Hash32,
    cache_dir: PathBuf,
}

fn fixture(dir: &Path) -> Fixture {
    let mut store = Store::init(dir.join("store"), opts()).unwrap();
    let note = create(&mut store, "notes.txt", "training notes");
    let note_version = store.object_meta(note).unwrap().version;
    let report = write_checkpoint(&mut store, &dir.join("src"));
    let shard_object = report.tensors[0].object_ids[0];
    let snapshot = store.checkpoint().unwrap();
    save_root(&mut store, "training", snapshot, vec![note_version]);
    Fixture {
        store,
        root_name: "training".to_owned(),
        note_object: note,
        cache_dir: dir.join("cache"),
    }
}

fn save_root(store: &mut Store, name: &str, snapshot: Hash32, causal: Vec<Hash32>) {
    let selector = selector::parse("select true").unwrap();
    let root = ContextRoot::new(&selector, snapshot, DEFAULT_RESOLVER, causal, None, None).unwrap();
    store.save_context_root(name, &root, None, true).unwrap();
}

fn options(max_bytes: Option<u64>) -> TrainingReadyOptions {
    TrainingReadyOptions {
        max_bytes,
        ..TrainingReadyOptions::default()
    }
}

fn check(
    fixture: &Fixture,
    options: &TrainingReadyOptions,
    refresh: bool,
) -> weftfs::error::Result<(
    weftfs::training_ready::TrainingReadyReport,
    weftfs::ready_cache::ReadyCacheOutcome,
)> {
    let (report, outcome) = fixture.store.check_training_ready_cached(
        &fixture.root_name,
        None,
        &Principal::root(),
        &Limits::default(),
        fixture.store.clock_now_ns(),
        options,
        Some(&ReadyCacheOptions {
            dir: fixture.cache_dir.clone(),
            refresh,
        }),
    )?;
    Ok((report, outcome.unwrap()))
}

#[test]
fn cold_then_warm_then_refresh() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let options = options(None);
    let (cold, cold_outcome) = check(&fixture, &options, false).unwrap();
    assert!(!cold_outcome.hit);
    assert!(cold_outcome.entry_written);
    assert!(cold_outcome.verify_ms > 0.0);
    let (warm, warm_outcome) = check(&fixture, &options, false).unwrap();
    assert!(warm_outcome.hit);
    assert_eq!(warm_outcome.verify_ms, 0.0);
    assert_eq!(warm.context_root_id, cold.context_root_id);
    assert_eq!(warm.snapshot_id, cold.snapshot_id);
    assert_eq!(warm.files.len(), cold.files.len());
    let (refreshed, refreshed_outcome) = check(&fixture, &options, true).unwrap();
    assert!(!refreshed_outcome.hit);
    assert!(refreshed_outcome.verify_ms > 0.0);
    assert_eq!(refreshed.context_root_id, cold.context_root_id);
}

#[test]
fn every_input_invalidates() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let options = options(None);
    let (_baseline, baseline_outcome) = check(&fixture, &options, false).unwrap();
    assert!(!baseline_outcome.hit);
    let (_warm, warm) = check(&fixture, &options, false).unwrap();
    assert!(warm.hit);

    // 1. An unrelated commit (generation/head/state digest).
    create(&mut fixture.store, "unrelated.txt", "noise");
    let (_report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit, "a new generation must invalidate");
    assert!(outcome.verify_ms > 0.0);

    // 2. A member manifest/content change, observed through a retargeted root.
    fixture
        .store
        .write_file(fixture.note_object, 0, b"note")
        .unwrap();
    let snapshot = fixture.store.checkpoint().unwrap();
    let current_note = fixture.store.object_meta(fixture.note_object).unwrap().version;
    save_root(&mut fixture.store, "training", snapshot, vec![current_note]);
    let (report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit, "a changed member manifest must invalidate");
    assert_eq!(report.snapshot_id, snapshot);

    // 3. A dependency change: a new causal parent on a member object.
    let other = create(&mut fixture.store, "other.txt", "extra parent");
    let other_version = fixture.store.object_meta(other).unwrap().version;
    fixture
        .store
        .write_file_causal(fixture.note_object, 0, b"note", &[other_version])
        .unwrap();
    let snapshot = fixture.store.checkpoint().unwrap();
    let current_note = fixture.store.object_meta(fixture.note_object).unwrap().version;
    save_root(&mut fixture.store, "training", snapshot, vec![current_note]);
    let (_report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit, "a changed causal dependency must invalidate");

    // 4. The declared policy (cost envelope).
    let (report, _) = check(&fixture, &options, false).unwrap();
    let policy = self::options(Some(report.cost.actual_bytes + 1));
    let (_report, outcome) = check(&fixture, &policy, false).unwrap();
    assert!(!outcome.hit, "a changed policy must invalidate");

    // 5. The grant epoch (a mutable authority record).
    let grant = TrainingGrant {
        artifact_root: Hash32::from_bytes([9u8; 32]),
        uid: 1001,
        gid: 1001,
        actions: vec!["train".to_owned()],
        policy_epoch: 1,
        issued_at_ns: 1_700_000_000_000_000_000,
        expires_at_ns: None,
        supersedes: None,
        signature: None,
    };
    fixture.store.set_training_grant(&grant).unwrap();
    let (_report, outcome) = check(&fixture, &policy, false).unwrap();
    assert!(!outcome.hit, "a grant edit must invalidate");

    // 6. A quarantine decision (a mutable authority record) must invalidate and
    //    the re-verification must refuse, never return the cached pass.
    let (baseline_report, _) = check(&fixture, &policy, false).unwrap();
    let member_content = baseline_report
        .files
        .iter()
        .find(|file| file.object_id == fixture.note_object)
        .map(|file| file.content_root)
        .expect("the shard is a member");
    fixture
        .store
        .quarantine(QuarantineRequest {
            content: member_content,
            mode: QuarantineMode::Explicit,
            reason: "withdrawn member".to_owned(),
            trace: None,
            trace_status: "closed".to_owned(),
            policy_epoch: 1,
            issued_at_ns: Some(1_700_000_001_000_000_000),
            seed: SIGNER,
            authority: vec![signing_key(&SIGNER)],
            release_of: None,
        })
        .unwrap();
    let err = check(&fixture, &policy, false).unwrap_err();
    assert!(
        err.class() == Class::Quarantined || err.class() == Class::NotFound,
        "unexpected class {}",
        err.class()
    );
}

#[test]
fn a_tampered_cache_entry_reverifies() {
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let options = options(None);
    let (_cold, cold) = check(&fixture, &options, false).unwrap();
    let path = fixture
        .cache_dir
        .join(format!("{}.json", cold.key.to_hex()));
    let mut entry: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    entry["evidence"] = serde_json::json!("00".repeat(32));
    fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
    let (_report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit);
    assert!(outcome.rejected.is_some(), "the rejection must be recorded");
    assert!(outcome.entry_written, "the entry must be rebuilt");
    let (_report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(outcome.hit, "the rebuilt entry must be usable");
}

#[test]
fn a_cache_entry_from_another_state_is_rejected() {
    let dir = tempdir().unwrap();
    let mut fixture = fixture(dir.path());
    let options = options(None);
    let (_cold, cold) = check(&fixture, &options, false).unwrap();
    let old_bytes = fs::read(
        fixture
            .cache_dir
            .join(format!("{}.json", cold.key.to_hex())),
    )
    .unwrap();
    // Change state without changing the root: this changes the new key.
    create(&mut fixture.store, "noise.txt", "generation");
    let (_report, miss) = check(&fixture, &options, false).unwrap();
    assert!(!miss.hit);
    // Move the old entry to the new key path. The entry is self-consistent but
    // names the old head/state, so it must be rejected.
    fs::copy(
        fixture
            .cache_dir
            .join(format!("{}.json", cold.key.to_hex())),
        fixture
            .cache_dir
            .join(format!("{}.json", miss.key.to_hex())),
    )
    .unwrap();
    fs::write(
        fixture
            .cache_dir
            .join(format!("{}.json", miss.key.to_hex())),
        old_bytes,
    )
    .unwrap();
    let (_report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit);
    assert!(outcome.rejected.is_some());
}

#[test]
fn tensor_dtype_is_unused_but_the_fixture_is_a_tensor() {
    // Guard against a silent fixture regression: the checkpoint member really
    // is a bf16 tensor, so `require_tensors` exercises the tensor path.
    let dir = tempdir().unwrap();
    let fixture = fixture(dir.path());
    let mut options = options(None);
    options.require_tensors = true;
    let (report, outcome) = check(&fixture, &options, false).unwrap();
    assert!(!outcome.hit);
    assert_eq!(report.preconditions.tensors_verified, true);
    let element = report
        .files
        .iter()
        .find_map(|file| file.tensor.as_ref())
        .expect("a tensor member");
    assert_eq!(element.dtype, "bf16");
    assert_eq!(element.shape, vec![8, 4]);
    let _ = TensorDtype::Bf16;
}
