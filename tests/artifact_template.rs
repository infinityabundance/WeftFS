//! T077 — evaluation and synthetic-data artifact templates.
//!
//! Each template records declared inputs and recorded outputs by content root,
//! in one canonical record, and publishes a provenance receipt with the same
//! edges so the taint tracer can walk through an evaluation or a generation
//! run. `replay` re-verifies both sides by content; it does not re-run anything.
//!
//! The negative case the gate asks for is here: a template whose declared input
//! root is not in the store is refused, before anything is written.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;
use weftfs::artifact_template::{
    ArtifactTemplate, EvalRunRequest, RecordedMetric, SynthRunRequest, EVAL_RUN_SCHEMA,
    SYNTH_RUN_SCHEMA,
};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{Determinism, ProvenanceEnvironment};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::taint::TaintVerdict;

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

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    dataset: Hash32,
    model: Hash32,
    config: Hash32,
    model_object: Hash32,
    dataset_object: Hash32,
    config_object: Hash32,
    output_object: Hash32,
    output_content: Hash32,
}

fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut make = |name: &str, bytes: &[u8]| {
        store
            .create_file(root, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
            .unwrap()
    };
    let dataset_object = make("dataset.bin", b"the evaluation dataset");
    let model_object = make("model.bin", b"the model declaration");
    let config_object = make("config.bin", b"the evaluation config");
    let output_object = make("metrics.json", b"{\"accuracy\": 0.9375}");
    let dataset = store.object_meta(dataset_object).unwrap().content;
    let model = store.object_meta(model_object).unwrap().content;
    let config = store.object_meta(config_object).unwrap().content;
    let output_content = store.object_meta(output_object).unwrap().content;
    Fixture {
        dir,
        store,
        dataset,
        model,
        config,
        model_object,
        dataset_object,
        config_object,
        output_object,
        output_content,
    }
}

fn environment() -> ProvenanceEnvironment {
    ProvenanceEnvironment::new("weft-eval", "0.1.0")
}

fn eval_request(fixture: &Fixture) -> EvalRunRequest {
    EvalRunRequest {
        run: "heldout-2026-11".to_owned(),
        dataset: fixture.dataset,
        model: fixture.model,
        config: fixture.config,
        metrics: vec![
            RecordedMetric::new("accuracy", 0.9375, "ratio").unwrap(),
            RecordedMetric::new("loss", 0.1, "scalar").unwrap(),
        ],
        outputs: vec![fixture.output_object],
        environment: environment(),
        issued_at_ns: Some(1_700_000_000_000_000_000),
        determinism: Determinism::Unknown,
    }
}

#[test]
fn an_evaluation_template_replays_its_declared_inputs_and_outputs() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    let template = fixture.store.read_template(id).unwrap();
    assert_eq!(template.schema, EVAL_RUN_SCHEMA);
    assert_eq!(template.inputs.len(), 3);
    assert_eq!(
        template
            .inputs
            .iter()
            .map(|input| input.role.as_str())
            .collect::<Vec<_>>(),
        vec!["config", "dataset", "model"],
        "inputs are canonicalised by role"
    );
    assert_eq!(template.outputs, vec![fixture.output_content]);
    // Metric values are stored as exact bits, and the decimal 0.1 is not
    // round-tripped through a string.
    assert_eq!(template.metrics[1].value_bits, 0.1f64.to_bits());
    assert_eq!(template.metrics[1].value(), 0.1f64);

    let replay = fixture.store.replay_template(id).unwrap();
    assert!(replay.exact, "replay: {replay:?}");
    assert_eq!(replay.inputs.len(), 3);
    assert_eq!(replay.outputs.len(), 1);
    assert_eq!(replay.outputs[0].content, fixture.output_content);
    assert!(replay.note.contains("does not re-run"));

    // The record is content-addressed and canonical.
    let encoded = template.encode().unwrap();
    let decoded = ArtifactTemplate::decode(&encoded).unwrap();
    assert_eq!(decoded.id().unwrap(), id);
    let mut corrupt = encoded.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0x01;
    assert!(ArtifactTemplate::decode(&corrupt).is_err());

    // The template is reachable from its output object.
    let found = fixture.store.templates_of(fixture.output_object).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, id);
}

#[test]
fn a_synthetic_data_template_records_its_seed_and_generator() {
    let mut fixture = fixture();
    let output = fixture
        .store
        .create_file(
            fixture.store.resolve_path("/").unwrap(),
            b"synth.jsonl",
            b"{\"prompt\": \"...\", \"completion\": \"...\"}",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let output_content = fixture.store.object_meta(output).unwrap().content;
    let request = SynthRunRequest {
        run: "synth-0007".to_owned(),
        config: fixture.config,
        generator: fixture.model,
        seed: vec![0xde, 0xad, 0xbe, 0xef],
        outputs: vec![output],
        environment: environment(),
        issued_at_ns: Some(1_700_000_000_000_000_000),
        determinism: Determinism::Unknown,
    };
    let id = fixture.store.record_synth_run(request).unwrap();
    let template = fixture.store.read_template(id).unwrap();
    assert_eq!(template.schema, SYNTH_RUN_SCHEMA);
    assert_eq!(template.seed, vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(template.generator, Some(fixture.model));
    assert_eq!(template.outputs, vec![output_content]);
    assert!(template.metrics.is_empty());
    let replay = fixture.store.replay_template(id).unwrap();
    assert!(replay.exact);
    assert_eq!(replay.inputs.len(), 2);
}

#[test]
fn a_declared_input_root_that_is_not_in_the_store_is_refused() {
    let mut fixture = fixture();
    let mut request = eval_request(&fixture);
    request.dataset = Hash32::from_bytes([0x5a; 32]);
    let err = fixture.store.record_eval_run(request).unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
    assert!(
        err.message().contains("does not exist in this store"),
        "{err}"
    );
    assert!(
        fixture.store.template_ids().unwrap().is_empty(),
        "a refused template writes nothing"
    );

    // The same for a synthetic-data generator root.
    let request = SynthRunRequest {
        run: "synth".to_owned(),
        config: fixture.config,
        generator: Hash32::from_bytes([0x5b; 32]),
        seed: vec![1],
        outputs: vec![fixture.output_object],
        environment: environment(),
        issued_at_ns: None,
        determinism: Determinism::Unknown,
    };
    let err = fixture.store.record_synth_run(request).unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
    assert!(fixture.store.template_ids().unwrap().is_empty());
}

#[test]
fn a_zero_input_root_is_refused_and_a_copy_is_accepted() {
    let mut fixture = fixture();
    let mut request = eval_request(&fixture);
    request.model = Hash32::ZERO;
    let err = fixture.store.record_eval_run(request).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // Two live objects with the same content do not make an input ambiguous:
    // the declared root still names exactly one content. The record is
    // accepted and replay reports both objects.
    fixture
        .store
        .create_file(
            fixture.store.resolve_path("/").unwrap(),
            b"dataset-copy.bin",
            b"the evaluation dataset",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    let replay = fixture.store.replay_template(id).unwrap();
    let dataset = replay
        .inputs
        .iter()
        .find(|input| input.role == "dataset")
        .unwrap();
    assert_eq!(dataset.objects.len(), 2);
    assert!(dataset.exact);
}

#[test]
fn a_deleted_input_or_output_makes_replay_inexact() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    assert!(fixture.store.replay_template(id).unwrap().exact);
    // Unlink the recorded output object (the content is only reachable through
    // it now).
    let parent = fixture.store.resolve_path("/").unwrap();
    fixture.store.unlink(parent, b"metrics.json").unwrap();
    let replay = fixture.store.replay_template(id).unwrap();
    assert!(!replay.exact, "a missing output must make replay inexact");
    assert_eq!(replay.outputs[0].objects.len(), 0);
    assert!(!replay.outputs[0].exact);
}

#[test]
fn a_template_feeds_the_taint_graph() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    let trace = fixture
        .store
        .taint_trace(&[fixture.dataset], &[fixture.output_object], 8)
        .unwrap();
    assert_eq!(trace.status(), "closed", "gaps: {:?}", trace.gaps);
    assert_eq!(trace.artifacts[0].verdict, TaintVerdict::Tainted);
    assert!(
        trace
            .tainted
            .iter()
            .any(|node| node.content == fixture.output_content),
        "the recorded output must be in the forward closure"
    );
    // The provenance record names the template, so a reader can get back to the
    // declaration from the graph.
    let records = fixture
        .store
        .provenance_records_of(fixture.output_object)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].1.operation, EVAL_RUN_SCHEMA);
    let template_param = records[0].1.params.get("template").unwrap();
    match template_param {
        weftcodec::wcb::Value::Bytes(bytes) => {
            assert_eq!(Hash32::from_slice(bytes).unwrap(), id)
        }
        other => panic!("unexpected template param {other:?}"),
    }
}

#[test]
fn templates_survive_a_read_only_reopen() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    let before = fixture.store.replay_template(id).unwrap();
    drop(fixture.store);
    let store = Store::open(fixture.dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    let after = store.replay_template(id).unwrap();
    assert_eq!(before, after);
    assert_eq!(store.template_ids().unwrap(), vec![id]);
}

#[test]
fn a_malformed_metric_is_refused() {
    assert!(RecordedMetric::new("", 1.0, "scalar").is_err());
    assert!(RecordedMetric::new("accuracy", 1.0, "").is_err());
    // NaN and infinities keep their exact bits.
    let nan = RecordedMetric::new("nan", f64::NAN, "scalar").unwrap();
    assert!(nan.value().is_nan());
    let inf = RecordedMetric::new("inf", f64::INFINITY, "scalar").unwrap();
    assert_eq!(inf.value_bits, f64::INFINITY.to_bits());
}

#[test]
fn the_schema_refuses_an_unexpected_key_and_cross_kind_fields() {
    let mut fixture = fixture();
    let id = fixture
        .store
        .record_eval_run(eval_request(&fixture))
        .unwrap();
    let template = fixture.store.read_template(id).unwrap();
    let weftcodec::wcb::Value::Map(mut map) = template.to_value() else {
        panic!("template is a map");
    };
    map.insert("extra".to_owned(), weftcodec::wcb::Value::U64(1));
    let err = ArtifactTemplate::decode(
        &weftfs::wcb_util::encode(&weftcodec::wcb::Value::Map(map)).unwrap(),
    )
    .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // An evaluation template carrying a seed is refused.
    let mut cross = template.clone();
    cross.seed = vec![0u8; 4];
    let err = ArtifactTemplate::decode(&cross.encode().unwrap()).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // A synthetic-data template without a seed is refused.
    let mut cross = template.clone();
    cross.schema = SYNTH_RUN_SCHEMA.to_owned();
    cross.generator = Some(fixture.model);
    cross.metrics.clear();
    let err = ArtifactTemplate::decode(&cross.encode().unwrap()).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
}

#[test]
fn an_evaluation_output_must_be_a_live_object() {
    let mut fixture = fixture();
    let mut request = eval_request(&fixture);
    request.outputs = vec![Hash32::from_bytes([0x6c; 32])];
    let err = fixture.store.record_eval_run(request).unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
    let _ = fixture.dataset_object;
    let _ = fixture.model_object;
    let _ = fixture.config_object;
}

#[test]
fn the_suite_files_are_untouched_by_a_refused_record() {
    let mut fixture = fixture();
    let before = fs::read_dir(fixture.dir.path().join("store").join("manifests"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    let mut request = eval_request(&fixture);
    request.dataset = Hash32::from_bytes([0x7d; 32]);
    assert!(fixture.store.record_eval_run(request).is_err());
    let after = fs::read_dir(fixture.dir.path().join("store").join("manifests"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(before, after, "a refused template must publish no blob");
}
