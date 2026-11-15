//! T075 — taint tracing and quarantine.
//!
//! The fixture is the shortest graph that has the property that matters: one
//! dataset member feeding **two** checkpoints through separate provenance
//! receipts. Quarantining one branch must leave the other resolving exactly,
//! and the closure must be precise: nothing else may be reported tainted, and
//! nothing may be reported clear on an incomplete graph.
//!
//! These tests also pin the two refusals the mechanism is for: a quarantined
//! artifact cannot be read or materialized, and the refusal in
//! `non-disclosing` mode is byte-identical to an absent object.

use std::collections::BTreeMap;

use sha2::Digest;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::taint::{QuarantineMode, QuarantineRequest, TaintVerdict, QUARANTINE_SCHEMA};

const SIGNER: [u8; 32] = [0x31; 32];
const OTHER: [u8; 32] = [0x32; 32];

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
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

fn write_container(dir: &Path, name: &str, seed: u64) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let shape = [128u64, 8u64];
    let bytes = random_bytes(128 * 8 * 2, seed);
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "weight",
            dtype: "bf16",
            shape: &shape,
            data: &bytes,
        }],
        None,
    )
    .unwrap();
    let path = dir.join(format!("{name}.safetensors"));
    fs::write(&path, &file).unwrap();
    path
}

fn provenance(
    operation: &str,
    inputs: Vec<ProvenanceInput>,
    outputs: Vec<Hash32>,
) -> ProvenanceRecord {
    ProvenanceRecord::new(
        operation,
        BTreeMap::new(),
        inputs,
        outputs,
        ProvenanceEnvironment::new("unit-test", "1.0"),
        Determinism::Exact,
        ProvenancePrincipal::new(1000, 1000),
        1_700_000_000_000_000_000,
    )
    .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    /// Content root of the dataset member.
    dataset_root: Hash32,
    /// Object ids and content roots of the two checkpoint manifests.
    c1_object: Hash32,
    c1_content: Hash32,
    c2_object: Hash32,
    c2_content: Hash32,
    /// Exact bytes the two checkpoints restore to.
    c1_bytes: Vec<u8>,
    c2_bytes: Vec<u8>,
}

fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let uid = 0;
    let gid = 0;
    let root = store.resolve_path("/").unwrap();
    let data = store.mkdir(root, b"data", 0o755, uid, gid).unwrap();
    let member = store
        .create_file(
            data,
            b"member.bin",
            b"the dataset member that will be tainted",
            0o644,
            uid,
            gid,
            BTreeMap::new(),
        )
        .unwrap();
    let dataset_root = store.object_meta(member).unwrap().content;
    let model = store
        .create_file(
            data,
            b"model.bin",
            b"an untainted model declaration",
            0o644,
            uid,
            gid,
            BTreeMap::new(),
        )
        .unwrap();
    let model_root = store.object_meta(model).unwrap().content;

    let source_a = write_container(&dir.path().join("src"), "a", 0x1111_2222);
    let source_b = write_container(&dir.path().join("src"), "b", 0x3333_4444);
    for (name, source) in [("c1", &source_a), ("c2", &source_b)] {
        store
            .write_checkpoint(
                name,
                source,
                &CheckpointWriteOptions {
                    shards: 1,
                    axis: 0,
                    filters: Vec::new(),
                    strategy: ShardStrategy::Raw,
                    tile_shape: None,
                    ..CheckpointWriteOptions::default()
                },
            )
            .unwrap();
    }

    let c1_manifest = store.resolve_path("/checkpoints/c1/manifest").unwrap();
    let c2_manifest = store.resolve_path("/checkpoints/c2/manifest").unwrap();
    let c1_content = store.object_meta(c1_manifest).unwrap().content;
    let c2_content = store.object_meta(c2_manifest).unwrap().content;

    let record_c1 = provenance(
        "train",
        vec![
            ProvenanceInput::new("dataset", dataset_root, Some(member)),
            ProvenanceInput::new("model", model_root, Some(model)),
        ],
        vec![c1_content],
    );
    store.record_provenance(&[c1_manifest], &record_c1).unwrap();
    let record_c2 = provenance(
        "train",
        vec![
            ProvenanceInput::new("dataset", dataset_root, Some(member)),
            ProvenanceInput::new("model", model_root, Some(model)),
        ],
        vec![c2_content],
    );
    store.record_provenance(&[c2_manifest], &record_c2).unwrap();

    let (c1_bytes, _) = store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap();
    let (c2_bytes, _) = store
        .restore_checkpoint_bytes("c2", &CheckpointRestoreOptions::default())
        .unwrap();

    Fixture {
        _dir: dir,
        store,
        dataset_root,
        c1_object: c1_manifest,
        c1_content,
        c2_object: c2_manifest,
        c2_content,
        c1_bytes,
        c2_bytes,
    }
}

fn quarantine_request(content: Hash32, mode: QuarantineMode, epoch: u64) -> QuarantineRequest {
    QuarantineRequest {
        content,
        mode,
        reason: "dataset member withdrawn by the data owner".to_owned(),
        trace: None,
        trace_status: "closed".to_owned(),
        policy_epoch: epoch,
        issued_at_ns: Some(1_700_000_001_000_000_000),
        seed: SIGNER,
        authority: vec![signing_key(&SIGNER)],
        release_of: None,
    }
}

fn signing_key(seed: &[u8; 32]) -> [u8; 32] {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

#[test]
fn the_closure_is_precise_and_complete() {
    let fixture = fixture();
    let trace = fixture
        .store
        .taint_trace(
            &[fixture.dataset_root],
            &[fixture.c1_object, fixture.c2_object],
            16,
        )
        .unwrap();
    assert_eq!(trace.status(), "closed", "gaps: {:?}", trace.gaps);
    let tainted: Vec<Hash32> = trace.tainted.iter().map(|node| node.content).collect();
    assert_eq!(
        tainted.len(),
        3,
        "exactly the dataset member and two checkpoints: {tainted:?}"
    );
    assert_eq!(tainted[0], fixture.dataset_root);
    assert!(tainted.contains(&fixture.c1_content));
    assert!(tainted.contains(&fixture.c2_content));
    assert_eq!(trace.artifacts.len(), 2);
    for artifact in &trace.artifacts {
        assert_eq!(artifact.verdict, TaintVerdict::Tainted);
        assert!(
            artifact.witness.contains(&fixture.dataset_root),
            "witness must reach the declared root: {:?}",
            artifact.witness
        );
    }
    // The model declaration is an input, not an output: it is not tainted.
    assert!(!trace
        .tainted
        .iter()
        .any(|node| node.operation.as_deref() == Some("model")));

    // The trace record itself is canonical and content-addressed.
    let root = trace.root().unwrap();
    let decoded = weftfs::taint::TaintTrace::decode(&trace.encode().unwrap()).unwrap();
    assert_eq!(decoded.root().unwrap(), root);
}

#[test]
fn quarantining_one_branch_leaves_the_other_exact() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            1,
        ))
        .unwrap();
    assert!(id != Hash32::ZERO);

    // The quarantined branch is refused exactly like an absent object.
    let absent = hash_of_absent();
    let absent_err = fixture.store.read_object(absent).unwrap_err();
    let refused = fixture.store.read_object(fixture.c1_object).unwrap_err();
    assert_eq!(refused.class(), Class::NotFound);
    assert_eq!(refused.class(), absent_err.class());
    assert_eq!(
        refused.to_string(),
        absent_err.to_string(),
        "a non-disclosing refusal must be byte-identical to an absent object"
    );
    // Materialization through every checkpoint entry point is refused too.
    let restore = fixture
        .store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap_err();
    assert_eq!(restore.class(), Class::NotFound);
    let one = fixture
        .store
        .restore_named_tensor_at(None, "c1", "weight", None, None)
        .unwrap_err();
    assert_eq!(one.class(), Class::NotFound);
    // Loading the checkpoint manifest is refused as well, so no path through
    // the checkpoint API reaches a shard.
    assert_eq!(
        fixture
            .store
            .load_checkpoint_manifest("c1")
            .unwrap_err()
            .class(),
        Class::NotFound
    );

    // The other branch still resolves and restores exactly.
    let bytes = fixture.store.read_object(fixture.c2_object).unwrap();
    assert!(!bytes.is_empty());
    let (restored, report) = fixture
        .store
        .restore_checkpoint_bytes("c2", &CheckpointRestoreOptions::default())
        .unwrap();
    assert_eq!(restored, fixture.c2_bytes);
    let mut hasher = sha2::Sha256::new();
    hasher.update(&fixture.c2_bytes);
    let expected: [u8; 32] = hasher.finalize().into();
    assert_eq!(
        report.file_sha256, expected,
        "the surviving branch restores the exact same container"
    );
    assert_eq!(fixture.c1_bytes.len(), fixture.c2_bytes.len());
}

#[test]
fn explicit_mode_discloses_what_the_policy_allows() {
    let mut fixture = fixture();
    fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::Explicit,
            1,
        ))
        .unwrap();
    let err = fixture.store.read_object(fixture.c1_object).unwrap_err();
    assert_eq!(err.class(), Class::Quarantined, "{err}");
    assert!(err.message().contains("quarantined"), "{err}");
    // The other branch is untouched.
    fixture.store.read_object(fixture.c2_object).unwrap();
}

#[test]
fn a_quarantine_requires_an_authorized_signer() {
    let mut fixture = fixture();
    // An empty authority set refuses everything.
    let mut request = quarantine_request(fixture.c1_content, QuarantineMode::NonDisclosing, 1);
    request.authority = Vec::new();
    let err = fixture.store.quarantine(request).unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized, "{err}");
    // A signer outside the authority set is refused.
    let mut request = quarantine_request(fixture.c1_content, QuarantineMode::NonDisclosing, 1);
    request.authority = vec![signing_key(&OTHER)];
    let err = fixture.store.quarantine(request).unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized, "{err}");
    // Nothing was written: the artifact still reads.
    fixture.store.read_object(fixture.c1_object).unwrap();
    // The authorized signer works.
    fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            1,
        ))
        .unwrap();
    assert!(fixture.store.is_quarantined(fixture.c1_content).unwrap());
}

#[test]
fn release_is_epoch_fenced_and_restores_service() {
    let mut fixture = fixture();
    fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            5,
        ))
        .unwrap();
    let mut release = quarantine_request(fixture.c1_content, QuarantineMode::NonDisclosing, 5);
    release.reason = "withdrawal rescinded by the data owner".to_owned();
    let err = fixture.store.release_quarantine(release).unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    // A second quarantine at the same or lower epoch is refused.
    let err = fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::Explicit,
            5,
        ))
        .unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");

    let mut release = quarantine_request(fixture.c1_content, QuarantineMode::NonDisclosing, 6);
    release.reason = "withdrawal rescinded by the data owner".to_owned();
    fixture.store.release_quarantine(release).unwrap();
    let (_, record) = fixture
        .store
        .quarantine_of(fixture.c1_content)
        .unwrap()
        .unwrap();
    assert!(!record.is_quarantine(), "the current decision is a release");
    assert_eq!(record.supersedes.is_some(), true);
    let bytes = fixture.store.read_object(fixture.c1_object).unwrap();
    assert!(!bytes.is_empty());
    let (restored, _) = fixture
        .store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap();
    assert_eq!(restored, fixture.c1_bytes);
}

#[test]
fn the_quarantine_record_is_tamper_evident() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            1,
        ))
        .unwrap();
    let record = fixture.store.quarantine_record(id).unwrap();
    let encoded = record.encode().unwrap();
    // A flipped byte must not decode as a different decision.
    let mut corrupt = encoded.clone();
    let position = corrupt.len() - 1;
    corrupt[position] ^= 0x01;
    assert!(
        weftfs::taint::QuarantineRecord::decode(&corrupt).is_err(),
        "a tampered record must be refused"
    );
    // A record whose reason was edited and re-signed by the wrong key is
    // refused by the authority check.
    let mut forged = record.clone();
    forged.reason = "rewritten by an unauthorized principal".to_owned();
    forged.sign(&OTHER).unwrap();
    assert_eq!(
        forged
            .verify_authority(&[signing_key(&SIGNER)])
            .unwrap_err()
            .class(),
        Class::Unauthorized
    );

    // The pointer file is protected too: overwriting it is a Corrupt read, not
    // a silent release.
    let pointer = fixture.store.resolve_path("/quarantine/current").unwrap();
    let entry = fixture
        .store
        .list_dir(pointer)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    fixture
        .store
        .write_file(entry.target, 0, b"not a pointer")
        .unwrap();
    let err = fixture.store.quarantine_of(fixture.c1_content).unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn an_incomplete_graph_is_unproven_never_clear() {
    let mut fixture = fixture();
    // A third checkpoint with no provenance receipt at all.
    let dir = fixture._dir.path().join("src");
    let source = write_container(&dir, "c3", 0x5555_6666);
    fixture
        .store
        .write_checkpoint(
            "c3",
            &source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Raw,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    let c3 = fixture
        .store
        .resolve_path("/checkpoints/c3/manifest")
        .unwrap();
    let trace = fixture
        .store
        .taint_trace(&[fixture.dataset_root], &[fixture.c2_object, c3], 16)
        .unwrap();
    assert_eq!(trace.status(), "unproven");
    let verdict = |object: Hash32| {
        trace
            .artifacts
            .iter()
            .find(|artifact| artifact.object == object)
            .unwrap()
            .clone()
    };
    assert_eq!(verdict(fixture.c2_object).verdict, TaintVerdict::Tainted);
    let c3_verdict = verdict(c3);
    assert_eq!(
        c3_verdict.verdict,
        TaintVerdict::Unproven,
        "an artifact with no receipt is never reported clear"
    );
    assert!(!c3_verdict.gaps.is_empty());
}

#[test]
fn a_declared_root_with_no_object_is_reported() {
    let fixture = fixture();
    let trace = fixture
        .store
        .taint_trace(&[Hash32::from_bytes([0x77; 32])], &[fixture.c2_object], 8)
        .unwrap();
    assert_eq!(trace.status(), "unproven");
    assert!(trace
        .gaps
        .iter()
        .any(|gap| gap.contains("has no live object")));
    assert_eq!(
        trace.artifacts[0].verdict,
        TaintVerdict::Clear,
        "c2's own receipt and its inputs are intact; the missing declared root is a          global gap, not a reason to doubt c2"
    );
}

#[test]
fn a_two_hop_chain_beyond_the_depth_bound_is_refused() {
    let mut fixture = fixture();
    let uid = 0;
    let gid = 0;
    let root = fixture.store.resolve_path("/").unwrap();
    let work = fixture.store.mkdir(root, b"work", 0o755, uid, gid).unwrap();
    let a = fixture
        .store
        .create_file(
            work,
            b"a.bin",
            b"first transform",
            0o644,
            uid,
            gid,
            BTreeMap::new(),
        )
        .unwrap();
    let a_content = fixture.store.object_meta(a).unwrap().content;
    let b = fixture
        .store
        .create_file(
            work,
            b"b.bin",
            b"second transform",
            0o644,
            uid,
            gid,
            BTreeMap::new(),
        )
        .unwrap();
    let b_content = fixture.store.object_meta(b).unwrap().content;
    let record_a = provenance(
        "transform",
        vec![ProvenanceInput::new("source", fixture.dataset_root, None)],
        vec![a_content],
    );
    fixture.store.record_provenance(&[a], &record_a).unwrap();
    let record_b = provenance(
        "transform",
        vec![ProvenanceInput::new("source", a_content, Some(a))],
        vec![b_content],
    );
    fixture.store.record_provenance(&[b], &record_b).unwrap();
    // Depth 1 cannot reach b, so the trace refuses rather than truncating.
    let err = fixture
        .store
        .taint_trace(&[fixture.dataset_root], &[b], 1)
        .unwrap_err();
    assert_eq!(err.class(), Class::LimitExceeded, "{err}");
    // Depth 4 reaches it and is precise.
    let trace = fixture
        .store
        .taint_trace(&[fixture.dataset_root], &[b], 4)
        .unwrap();
    assert_eq!(trace.status(), "closed");
    assert_eq!(
        trace.artifacts[0].verdict,
        TaintVerdict::Tainted,
        "a two-hop derivation is tainted"
    );
    // The dataset root also feeds the two fixture checkpoints, so five content
    // roots are tainted: the root, a, b, c1 and c2.
    assert_eq!(trace.tainted.len(), 5);
}

#[test]
fn a_rewritten_object_does_not_escape_a_content_quarantine() {
    let mut fixture = fixture();
    // A second name for the same bytes: same content root, different object id.
    let root = fixture.store.resolve_path("/").unwrap();
    let copies = fixture.store.mkdir(root, b"copies", 0o755, 0, 0).unwrap();
    let bytes = fixture.store.read_object(fixture.c1_object).unwrap();
    let copy = fixture
        .store
        .create_file(copies, b"c1-copy", &bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(
        fixture.store.object_meta(copy).unwrap().content,
        fixture.c1_content
    );
    fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            1,
        ))
        .unwrap();
    assert_eq!(
        fixture.store.read_object(copy).unwrap_err().class(),
        Class::NotFound
    );
    assert_eq!(
        fixture
            .store
            .read_object(fixture.c1_object)
            .unwrap_err()
            .class(),
        Class::NotFound
    );
}

#[test]
fn clear_and_unproven_are_separated_by_the_closure() {
    let mut fixture = fixture();
    let root = fixture.store.resolve_path("/").unwrap();
    // (a) An artifact with an intact receipt that never consumed the tainted
    //     root: clear.
    let independent = fixture
        .store
        .create_file(
            root,
            b"independent.bin",
            b"unrelated artifact",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let independent_content = fixture.store.object_meta(independent).unwrap().content;
    let record = provenance(
        "evaluate",
        vec![ProvenanceInput::new(
            "config",
            Hash32::from_bytes([0x44; 32]),
            None,
        )],
        vec![independent_content],
    );
    fixture
        .store
        .record_provenance(&[independent], &record)
        .unwrap();
    // (b) An artifact with no receipt at all: its derivation is unknown.
    let orphan = fixture
        .store
        .create_file(
            root,
            b"orphan.bin",
            b"artifact with no receipt",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let trace = fixture
        .store
        .taint_trace(&[fixture.dataset_root], &[independent, orphan], 16)
        .unwrap();
    assert_eq!(trace.status(), "unproven");
    let verdict = |object: Hash32| {
        trace
            .artifacts
            .iter()
            .find(|artifact| artifact.object == object)
            .unwrap()
            .verdict
    };
    assert_eq!(
        verdict(independent),
        TaintVerdict::Clear,
        "an intact receipt whose inputs never touched the tainted root is clear"
    );
    assert_eq!(
        verdict(orphan),
        TaintVerdict::Unproven,
        "an artifact with no receipt is never reported clear"
    );
    // (c) A record whose input hint no longer resolves breaks the chain.
    let broken = fixture
        .store
        .create_file(
            root,
            b"broken.bin",
            b"derived through a stale hint",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let broken_content = fixture.store.object_meta(broken).unwrap().content;
    let record = provenance(
        "evaluate",
        vec![ProvenanceInput::new(
            "parent",
            Hash32::from_bytes([0x45; 32]),
            Some(Hash32::from_bytes([0x46; 32])),
        )],
        vec![broken_content],
    );
    fixture.store.record_provenance(&[broken], &record).unwrap();
    let trace = fixture
        .store
        .taint_trace(&[fixture.dataset_root], &[broken], 16)
        .unwrap();
    let verdict = trace
        .artifacts
        .iter()
        .find(|artifact| artifact.object == broken)
        .unwrap();
    assert_eq!(verdict.verdict, TaintVerdict::Unproven);
    assert!(
        verdict
            .gaps
            .iter()
            .any(|gap| gap.contains("is gone") || gap.contains("no longer carries")),
        "gaps: {:?}",
        verdict.gaps
    );
}

fn hash_of_absent() -> Hash32 {
    // A digest that resolves to nothing: the refusal comparison must be
    // against the store's own absent-object error, not a hand-written string.
    Hash32::from_bytes([0xfe; 32])
}

#[test]
fn the_record_schema_is_pinned() {
    // A record with an unexpected key is refused, so the schema cannot drift.
    let mut fixture = fixture();
    let id = fixture
        .store
        .quarantine(quarantine_request(
            fixture.c1_content,
            QuarantineMode::NonDisclosing,
            1,
        ))
        .unwrap();
    let record = fixture.store.quarantine_record(id).unwrap();
    assert_eq!(QUARANTINE_SCHEMA, "weft.quarantine.v1");
    let value = record.to_value();
    let weftcodec::wcb::Value::Map(mut map) = value else {
        panic!("record is a map");
    };
    map.insert("extra".to_owned(), weftcodec::wcb::Value::U64(1));
    let err = weftfs::taint::QuarantineRecord::decode(
        &weftfs::wcb_util::encode(&weftcodec::wcb::Value::Map(map)).unwrap(),
    )
    .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
    // Reopen read-only: the decision survives and still enforces.
    drop(fixture.store);
    let store = Store::open(fixture._dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.read_object(fixture.c1_object).unwrap_err().class(),
        Class::NotFound
    );
    assert_eq!(store.quarantine_entries().unwrap().len(), 1);
}
