//! T077 court harness — evaluation and synthetic-data templates.
//!
//! Records one evaluation run and one synthetic-data run, replays both, and
//! measures what a template costs: record bytes and replay wall time on a
//! store with a 4 MiB metrics artifact. It also pins the negative case the gate
//! asks for: a declared input root that is not in the store is refused and
//! writes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::json;
use weftfs::artifact_template::{EvalRunRequest, RecordedMetric, SynthRunRequest};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{Determinism, ProvenanceEnvironment};
use weftfs::store::{OpenMode, Options, Store};

const METRICS_BYTES: usize = 4 * 1024 * 1024;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 100_000,
        gc_grace_secs: 0,
        ..Options::default()
    }
}

fn main() {
    let mut out = PathBuf::from(".docker-cache/artifact-template-court");
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

    let mut store = Store::init(out.join("store"), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut make = |store: &mut Store, name: &str, bytes: &[u8]| {
        store
            .create_file(root, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
            .unwrap()
    };
    let dataset_object = make(
        &mut store,
        "dataset.bin",
        b"the held-out evaluation dataset",
    );
    let model_object = make(&mut store, "model.safetensors", b"bf16 weights placeholder");
    let config_object = make(&mut store, "config.json", b"{\"temperature\": 0.0}");
    let mut metrics = vec![b'0'; METRICS_BYTES];
    for (index, byte) in metrics.iter_mut().enumerate() {
        *byte = b"0123456789abcdef"[(index / 4096) % 16];
    }
    let output_object = make(&mut store, "metrics.json", &metrics);
    let synth_object = make(
        &mut store,
        "synth.jsonl",
        b"{\"prompt\": \"...\", \"completion\": \"...\"}\n",
    );
    let dataset = store.object_meta(dataset_object).unwrap().content;
    let model = store.object_meta(model_object).unwrap().content;
    let config = store.object_meta(config_object).unwrap().content;
    let output_content = store.object_meta(output_object).unwrap().content;
    let synth_content = store.object_meta(synth_object).unwrap().content;

    let environment = ProvenanceEnvironment::new("weft-eval", "0.1.0");
    let eval_started = Instant::now();
    let eval_id = store
        .record_eval_run(EvalRunRequest {
            run: "heldout-2026-11".to_owned(),
            dataset,
            model,
            config,
            metrics: vec![
                RecordedMetric::new("accuracy", 0.9375, "ratio").unwrap(),
                RecordedMetric::new("loss", 0.1, "scalar").unwrap(),
            ],
            outputs: vec![output_object],
            environment: environment.clone(),
            issued_at_ns: Some(1_700_000_000_000_000_000),
            determinism: Determinism::Unknown,
        })
        .unwrap();
    let eval_record_ns = eval_started.elapsed().as_nanos() as u64;
    let eval_bytes = store
        .read_template(eval_id)
        .unwrap()
        .encode()
        .unwrap()
        .len() as u64;

    let synth_started = Instant::now();
    let synth_id = store
        .record_synth_run(SynthRunRequest {
            run: "synth-0007".to_owned(),
            config,
            generator: model,
            seed: vec![0xde, 0xad, 0xbe, 0xef],
            outputs: vec![synth_object],
            environment: environment.clone(),
            issued_at_ns: Some(1_700_000_000_000_000_000),
            determinism: Determinism::Unknown,
        })
        .unwrap();
    let synth_record_ns = synth_started.elapsed().as_nanos() as u64;
    let synth_bytes = store
        .read_template(synth_id)
        .unwrap()
        .encode()
        .unwrap()
        .len() as u64;

    // Replay, 7 reps each.
    let mut eval_replay = Vec::new();
    let mut synth_replay = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let replay = store.replay_template(eval_id).unwrap();
        assert!(replay.exact);
        eval_replay.push(started.elapsed().as_nanos() as u64);
    }
    for _ in 0..7 {
        let started = Instant::now();
        let replay = store.replay_template(synth_id).unwrap();
        assert!(replay.exact);
        synth_replay.push(started.elapsed().as_nanos() as u64);
    }
    eval_replay.sort_unstable();
    synth_replay.sort_unstable();
    let eval_replay_ns = eval_replay[3];
    let synth_replay_ns = synth_replay[3];

    let eval_template = store.read_template(eval_id).unwrap();
    check(
        "an evaluation template records its three declared inputs and metric bits",
        eval_template.inputs.len() == 3
            && eval_template.outputs == vec![output_content]
            && eval_template.metrics[1].value_bits == 0.1f64.to_bits(),
        format!(
            "inputs={} metric_bits={:x} outputs={}",
            eval_template.inputs.len(),
            eval_template.metrics[1].value_bits,
            eval_template.outputs.len()
        ),
    );
    check(
        "both templates replay exactly",
        store.replay_template(eval_id).unwrap().exact
            && store.replay_template(synth_id).unwrap().exact,
        format!("eval={eval_id} synth={synth_id}"),
    );
    check(
        "a synthetic-data template records its seed and generator",
        {
            let template = store.read_template(synth_id).unwrap();
            template.seed == vec![0xde, 0xad, 0xbe, 0xef] && template.generator == Some(model)
        },
        format!("seed_len=4"),
    );
    check(
        "the template feeds the taint graph",
        {
            let trace = store.taint_trace(&[dataset], &[output_object], 8).unwrap();
            trace.status() == "closed"
                && trace.artifacts[0].verdict == weftfs::taint::TaintVerdict::Tainted
        },
        "dataset -> eval output is tainted and closed".to_owned(),
    );
    let refused = store
        .record_eval_run(EvalRunRequest {
            run: "refused".to_owned(),
            dataset: Hash32::from_bytes([0x5a; 32]),
            model,
            config,
            metrics: Vec::new(),
            outputs: vec![output_object],
            environment: environment.clone(),
            issued_at_ns: None,
            determinism: Determinism::Unknown,
        })
        .unwrap_err();
    check(
        "a declared input root that is not in the store is refused",
        refused.class() == Class::NotFound && store.template_ids().unwrap().len() == 2,
        format!(
            "class={} templates={}",
            refused.class().as_str(),
            store.template_ids().unwrap().len()
        ),
    );
    drop(store);
    let store = Store::open_with_options(out.join("store"), OpenMode::ReadOnly, options()).unwrap();
    check(
        "templates survive a read-only reopen",
        store.replay_template(eval_id).unwrap().exact
            && store.replay_template(synth_id).unwrap().exact,
        "both replay exact after reopen".to_owned(),
    );

    let passed = checks.iter().filter(|c| c["pass"] == json!(true)).count();
    let failed = checks.len() - passed;
    let document = json!({
        "schema": "weft.artifact-template-court/v1",
        "checks": checks,
        "passed": passed,
        "failed": failed,
        "eval_template_bytes": eval_bytes,
        "synth_template_bytes": synth_bytes,
        "eval_record_ns": eval_record_ns,
        "synth_record_ns": synth_record_ns,
        "eval_replay_ns": eval_replay_ns,
        "synth_replay_ns": synth_replay_ns,
        "output_bytes": METRICS_BYTES as u64,
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(path, text).unwrap(),
        None => println!("{text}"),
    }
}
