//! T075 court harness — taint tracing, quarantine, and the cost of the gate.
//!
//! Builds the minimal graph that matters (one dataset member feeding two
//! checkpoints through separate provenance receipts), records every verdict and
//! refusal the mechanism claims, and measures what the enforcement gate costs
//! on the read path: an ordinary read, a read while an *unrelated* quarantine
//! exists, and a refused read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::taint::{QuarantineMode, QuarantineRequest, TaintVerdict};

const SIGNER: [u8; 32] = [0x31; 32];
const READS: usize = 2_000;

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
    let shape = [256u64, 16u64];
    let bytes = random_bytes(256 * 16 * 2, seed);
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
        ProvenanceEnvironment::new("taint-court", "1.0"),
        Determinism::Exact,
        ProvenancePrincipal::new(1000, 1000),
        1_700_000_000_000_000_000,
    )
    .unwrap()
}

fn median(mut samples: Vec<u64>) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    let mut out = PathBuf::from(".docker-cache/taint-court");
    let mut json_path: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();
    let mut checks = Vec::new();
    let mut check = |name: &str, pass: bool, detail: String| {
        checks.push(json!({"name": name, "pass": pass, "detail": detail}));
    };

    let mut options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 8192,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };
    options.chunk_size = 8192;
    let mut store = Store::init(out.join("store"), options).unwrap();
    let root = store.resolve_path("/").unwrap();
    let data = store.mkdir(root, b"data", 0o755, 0, 0).unwrap();
    let member = store
        .create_file(
            data,
            b"member.bin",
            b"the dataset member that will be tainted",
            0o644,
            0,
            0,
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
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let model_root = store.object_meta(model).unwrap().content;

    for (name, seed) in [("c1", 0x1111_2222u64), ("c2", 0x3333_4444)] {
        let source = write_container(&out.join("src"), name, seed);
        store
            .write_checkpoint(
                name,
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
    }
    let c1 = store.resolve_path("/checkpoints/c1/manifest").unwrap();
    let c2 = store.resolve_path("/checkpoints/c2/manifest").unwrap();
    let c1_content = store.object_meta(c1).unwrap().content;
    let c2_content = store.object_meta(c2).unwrap().content;
    for (object, content) in [(c1, c1_content), (c2, c2_content)] {
        let record = provenance(
            "train",
            vec![
                ProvenanceInput::new("dataset", dataset_root, Some(member)),
                ProvenanceInput::new("model", model_root, Some(model)),
            ],
            vec![content],
        );
        store.record_provenance(&[object], &record).unwrap();
    }
    let (c1_bytes, _) = store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap();
    let (c2_bytes, _) = store
        .restore_checkpoint_bytes("c2", &CheckpointRestoreOptions::default())
        .unwrap();

    // ------------------------------------------------------------------ trace
    let started = Instant::now();
    let trace = store.taint_trace(&[dataset_root], &[c1, c2], 32).unwrap();
    let trace_us = started.elapsed().as_micros() as u64;
    check(
        "closure is closed and precise",
        trace.status() == "closed" && trace.tainted.len() == 3,
        format!(
            "status={} tainted={} artifacts={}",
            trace.status(),
            trace.tainted.len(),
            trace.artifacts.len()
        ),
    );
    let all_tainted = trace
        .artifacts
        .iter()
        .all(|artifact| artifact.verdict == TaintVerdict::Tainted);
    check(
        "both branches are tainted with a witness to the root",
        all_tainted
            && trace
                .artifacts
                .iter()
                .all(|artifact| artifact.witness.contains(&dataset_root)),
        format!("{} artifacts", trace.artifacts.len()),
    );

    // ------------------------------------------------------- gate cost (warm)
    let mut plain = Vec::new();
    for _ in 0..READS {
        let started = Instant::now();
        let bytes = store.read_object(c2).unwrap();
        std::hint::black_box(&bytes);
        plain.push(started.elapsed().as_nanos() as u64);
    }
    let plain_median = median(plain);

    // Quarantine an unrelated content root: every read now performs a lookup
    // that misses. This is the steady-state cost of the gate.
    let unrelated = Hash32::from_bytes([0x9b; 32]);
    let request = QuarantineRequest {
        content: unrelated,
        mode: QuarantineMode::NonDisclosing,
        reason: "unrelated control quarantine".to_owned(),
        trace: None,
        trace_status: "closed".to_owned(),
        policy_epoch: 1,
        issued_at_ns: Some(1_700_000_001_000_000_000),
        seed: SIGNER,
        authority: vec![signing_key(&SIGNER)],
        release_of: None,
    };
    let unrelated_id = store.quarantine(request).unwrap();
    let mut gated = Vec::new();
    for _ in 0..READS {
        let started = Instant::now();
        let bytes = store.read_object(c2).unwrap();
        std::hint::black_box(&bytes);
        gated.push(started.elapsed().as_nanos() as u64);
    }
    let gated_median = median(gated);

    // Quarantine the branch and measure the refusal path.
    let request = QuarantineRequest {
        content: c1_content,
        mode: QuarantineMode::NonDisclosing,
        reason: "dataset member withdrawn by the data owner".to_owned(),
        trace: Some(trace.root().unwrap()),
        trace_status: trace.status().to_owned(),
        policy_epoch: 2,
        issued_at_ns: Some(1_700_000_002_000_000_000),
        seed: SIGNER,
        authority: vec![signing_key(&SIGNER)],
        release_of: None,
    };
    let quarantine_id = store.quarantine(request).unwrap();
    let mut refused = Vec::new();
    let mut refusal_class = String::new();
    for _ in 0..READS {
        let started = Instant::now();
        let result = store.read_object(c1);
        refused.push(started.elapsed().as_nanos() as u64);
        if let Err(err) = result {
            refusal_class = err.class().as_str().to_owned();
        }
    }
    let refused_median = median(refused);

    // Non-disclosing refusal must equal the absent-object refusal exactly.
    let absent = Hash32::from_bytes([0xfe; 32]);
    let absent_message = store.read_object(absent).unwrap_err().to_string();
    let refused_message = store.read_object(c1).unwrap_err().to_string();
    check(
        "non-disclosing refusal is byte-identical to an absent object",
        refusal_class == Class::NotFound.as_str() && absent_message == refused_message,
        format!(
            "class={refusal_class} message_equal={}",
            absent_message == refused_message
        ),
    );

    // The surviving branch is untouched.
    let (restored, report) = store
        .restore_checkpoint_bytes("c2", &CheckpointRestoreOptions::default())
        .unwrap();
    check(
        "the unquarantined branch restores the same bytes",
        restored == c2_bytes && restored != c1_bytes,
        format!(
            "{} bytes, sha256 unchanged={}",
            restored.len(),
            report.exact_source_file
        ),
    );
    let blocked = store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap_err();
    check(
        "the quarantined branch cannot be materialized",
        blocked.class() == Class::NotFound,
        format!("class={}", blocked.class().as_str()),
    );

    // An artifact with no receipt is unproven.
    let orphan = store
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
    let trace = store.taint_trace(&[dataset_root], &[orphan], 16).unwrap();
    check(
        "an artifact with no receipt is unproven, never clear",
        trace.status() == "unproven" && trace.artifacts[0].verdict == TaintVerdict::Unproven,
        format!(
            "status={} verdict={}",
            trace.status(),
            trace.artifacts[0].verdict.as_str()
        ),
    );

    // Release restores service.
    let release = QuarantineRequest {
        content: c1_content,
        mode: QuarantineMode::NonDisclosing,
        reason: "withdrawal rescinded".to_owned(),
        trace: None,
        trace_status: "closed".to_owned(),
        policy_epoch: 3,
        issued_at_ns: Some(1_700_000_003_000_000_000),
        seed: SIGNER,
        authority: vec![signing_key(&SIGNER)],
        release_of: Some(quarantine_id),
    };
    store.release_quarantine(release).unwrap();
    let (again, _) = store
        .restore_checkpoint_bytes("c1", &CheckpointRestoreOptions::default())
        .unwrap();
    check(
        "a signed release restores exact service",
        again == c1_bytes,
        format!("{} bytes", again.len()),
    );

    // Reopen read-only: the decision must still be enforced on disk.
    let quarantine_again = QuarantineRequest {
        content: c1_content,
        mode: QuarantineMode::Explicit,
        reason: "final state for the reopen check".to_owned(),
        trace: None,
        trace_status: "closed".to_owned(),
        policy_epoch: 4,
        issued_at_ns: Some(1_700_000_004_000_000_000),
        seed: SIGNER,
        authority: vec![signing_key(&SIGNER)],
        release_of: None,
    };
    store.quarantine(quarantine_again).unwrap();
    drop(store);
    let store = Store::open(out.join("store"), OpenMode::ReadOnly).unwrap();
    let err = store.read_object(c1).unwrap_err();
    check(
        "the decision is durable and enforced after reopen",
        err.class() == Class::Quarantined,
        format!("class={}", err.class().as_str()),
    );

    let document = json!({
        "schema": "weft.taint-court/v1",
        "checks": checks,
        "passed": checks.iter().filter(|c| c.as_object().unwrap()["pass"] == json!(true)).count(),
        "failed": checks.iter().filter(|c| c.as_object().unwrap()["pass"] == json!(false)).count(),
        "trace_us": trace_us,
        "reads": READS,
        "read_ns": {
            "no_quarantine_p50": plain_median,
            "gate_miss_p50": gated_median,
            "refused_p50": refused_median,
        },
        "gate_overhead_ns": gated_median as i64 - plain_median as i64,
        "refusal_overhead_ns": refused_median as i64 - plain_median as i64,
        "unrelated_quarantine": unrelated_id.to_hex(),
        "quarantine_record": quarantine_id.to_hex(),
        "signer_public_key": hex(&signing_key(&SIGNER)),
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(path, text).unwrap(),
        None => println!("{text}"),
    }
}

fn signing_key(seed: &[u8; 32]) -> [u8; 32] {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
