//! First-class provenance receipts and the deterministic recovery planner.
//!
//! These tests cover canonicalisation, store attachment/atomicity, the pure
//! planner, closure bounds/cycles, GC retention, and `verify --full`.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    choose_recovery, Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal,
    ProvenanceRecord, RecoveryCandidate, RecoveryRejection, RecoveryStrategy,
};
use weftfs::recovery::{manifest_path, object_path, read_manifest};
use weftfs::state::{RelationTarget, State};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    (dir, store)
}

fn make_input(role: &str, content: Hash32, object: Option<Hash32>) -> ProvenanceInput {
    ProvenanceInput::new(role, content, object)
}

fn make_record(
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

fn create_file(store: &mut Store, name: &[u8], bytes: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, name, bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap()
}

fn content_of(store: &Store, id: Hash32) -> Hash32 {
    store.object_meta(id).unwrap().content
}

fn candidate(
    strategy: RecoveryStrategy,
    name: &str,
    available: bool,
    determinism: Determinism,
) -> RecoveryCandidate {
    RecoveryCandidate {
        strategy,
        name: name.to_owned(),
        available,
        determinism,
        missing_inputs: Vec::new(),
        fetch_cost: 0,
        decode_cost: 0,
        recompute_cost: 0,
    }
}

// ---------------------------------------------------------------------------
// Record canonicalisation
// ---------------------------------------------------------------------------

#[test]
fn record_round_trips_byte_for_byte() {
    let mut params = BTreeMap::new();
    params.insert("temperature".to_owned(), Value::U64(0));
    params.insert("prompt".to_owned(), Value::Str("hello".to_owned()));
    let record = ProvenanceRecord::new(
        "tokenize",
        params,
        vec![make_input(
            "text",
            Hash32::from_bytes([9u8; 32]),
            Some(Hash32::from_bytes([8u8; 32])),
        )],
        vec![Hash32::from_bytes([7u8; 32])],
        ProvenanceEnvironment::new("weft-tool", "2.3.1")
            .with_env_hash(Hash32::from_bytes([6u8; 32]))
            .unwrap(),
        Determinism::Declared,
        ProvenancePrincipal::new(1000, 1000),
        1_700_000_000_000_000_001,
    )
    .unwrap();

    let bytes = record.encode().unwrap();
    let decoded = ProvenanceRecord::decode(&bytes).unwrap();
    assert_eq!(record, decoded);
    assert_eq!(record.id().unwrap(), decoded.id().unwrap());
    assert_eq!(decoded.encode().unwrap(), bytes);
}

#[test]
fn input_and_output_order_do_not_change_the_identity() {
    let a = make_input("text", Hash32::from_bytes([1u8; 32]), None);
    let b = make_input("ref", Hash32::from_bytes([2u8; 32]), None);
    let out1 = Hash32::from_bytes([3u8; 32]);
    let out2 = Hash32::from_bytes([4u8; 32]);

    let forward = make_record("eval", vec![a.clone(), b.clone()], vec![out1, out2]);
    let reverse = make_record("eval", vec![b, a], vec![out2, out1]);
    assert_eq!(forward.id().unwrap(), reverse.id().unwrap());
    assert_eq!(forward.encode().unwrap(), reverse.encode().unwrap());
    assert_eq!(
        forward
            .inputs
            .iter()
            .map(|i| i.role.clone())
            .collect::<Vec<_>>(),
        vec!["ref".to_owned(), "text".to_owned()]
    );
}

#[test]
fn unsorted_and_duplicate_inputs_are_rejected() {
    let record = make_record(
        "train-step",
        vec![
            make_input("a", Hash32::from_bytes([1u8; 32]), None),
            make_input("b", Hash32::from_bytes([2u8; 32]), None),
        ],
        vec![Hash32::from_bytes([3u8; 32])],
    );
    let value = record.to_value();
    let mut map = match value {
        Value::Map(map) => map,
        _ => unreachable!(),
    };

    // Unsorted input array: canonical WCB keeps array order, so decoding must
    // reject the record's non-canonical input order.
    let mut reversed = map.clone();
    if let Some(Value::Array(items)) = reversed.get_mut("inputs") {
        items.reverse();
    }
    let bytes = weftfs::wcb_util::encode(&Value::Map(reversed)).unwrap();
    let err = ProvenanceRecord::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), Class::NonCanonical);

    // Duplicate input.
    if let Some(Value::Array(items)) = map.get_mut("inputs") {
        let first = items[0].clone();
        items.push(first);
    }
    let bytes = weftfs::wcb_util::encode(&Value::Map(map.clone())).unwrap();
    let err = ProvenanceRecord::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), Class::NonCanonical);

    // Duplicate output.
    if let Some(Value::Array(items)) = map.get_mut("outputs") {
        let first = items[0].clone();
        items.push(first);
    }
    let bytes = weftfs::wcb_util::encode(&Value::Map(map)).unwrap();
    let err = ProvenanceRecord::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), Class::NonCanonical);

    // The constructor canonicalises, but still rejects a duplicate pair.
    let duplicate = ProvenanceRecord::new(
        "train-step",
        BTreeMap::new(),
        vec![
            make_input("a", Hash32::from_bytes([1u8; 32]), None),
            make_input("a", Hash32::from_bytes([1u8; 32]), None),
        ],
        vec![Hash32::from_bytes([3u8; 32])],
        ProvenanceEnvironment::new("unit-test", "1"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        1,
    );
    assert_eq!(duplicate.unwrap_err().class(), Class::NonCanonical);
}

#[test]
fn params_are_bounded() {
    let mut params = BTreeMap::new();
    params.insert("prompt".to_owned(), Value::Str("x".repeat(70 * 1024)));
    let err = ProvenanceRecord::new(
        "tokenize",
        params,
        Vec::new(),
        vec![Hash32::from_bytes([1u8; 32])],
        ProvenanceEnvironment::new("unit-test", "1"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        1,
    )
    .unwrap_err();
    assert_eq!(err.class(), Class::LimitExceeded);
    assert!(err.message().contains("params"));
}

// ---------------------------------------------------------------------------
// Store attachment
// ---------------------------------------------------------------------------

#[test]
fn output_content_mismatch_is_rejected_without_writing() {
    let (_dir, mut store) = new_store();
    let output = create_file(&mut store, b"out", b"actual output");
    let generation = store.generation();
    let record = make_record(
        "compile",
        Vec::new(),
        vec![Hash32::from_bytes([0xabu8; 32])],
    );

    let err = store.record_provenance(&[output], &record).unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
    assert!(err.message().contains("do not match"));
    assert_eq!(store.generation(), generation);
    assert!(store.provenance_ids_of(output).unwrap().is_empty());
    assert!(store.provenance_of(output).unwrap().is_none());
}

#[test]
fn provenance_relation_is_attached_atomically_to_every_output() {
    let (_dir, mut store) = new_store();
    let first = create_file(&mut store, b"first", b"first output");
    let second = create_file(&mut store, b"second", b"second output");
    let mut outputs = vec![content_of(&store, first), content_of(&store, second)];
    outputs.sort_unstable();
    let input = create_file(&mut store, b"input", b"input bytes");
    let record = make_record(
        "train-step",
        vec![make_input(
            "dataset",
            content_of(&store, input),
            Some(input),
        )],
        outputs.clone(),
    );

    let generation = store.generation();
    let id = store.record_provenance(&[second, first], &record).unwrap();
    assert_eq!(store.generation(), generation + 1);
    assert_eq!(id, record.id().unwrap());

    for output in [first, second] {
        let ids = store.provenance_ids_of(output).unwrap();
        assert_eq!(ids, vec![id]);
        let stored = store.provenance_of(output).unwrap().unwrap();
        assert_eq!(&stored, &record);
        let relation = store
            .current_state()
            .relations
            .values()
            .find(|relation| relation.subject == output && relation.predicate == "provenance")
            .unwrap();
        assert_eq!(
            relation.target,
            RelationTarget::Value(Value::Str(id.to_hex()))
        );
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

#[test]
fn cost_driven_choice_between_fetch_decode_and_recompute() {
    let target = Hash32::from_bytes([0x11u8; 32]);
    let mut fetch = candidate(RecoveryStrategy::Fetch, "fetch", true, Determinism::Unknown);
    let mut decode = candidate(
        RecoveryStrategy::Decode,
        "decode",
        true,
        Determinism::Unknown,
    );
    let mut recompute = candidate(
        RecoveryStrategy::Recompute,
        "recompute",
        true,
        Determinism::Exact,
    );

    fetch.fetch_cost = 10;
    decode.decode_cost = 20;
    recompute.recompute_cost = 15;
    assert_eq!(
        choose_recovery(target, &[fetch.clone(), decode.clone(), recompute.clone()])
            .unwrap()
            .strategy,
        RecoveryStrategy::Fetch
    );

    fetch.fetch_cost = 30;
    decode.decode_cost = 20;
    recompute.recompute_cost = 100;
    assert_eq!(
        choose_recovery(target, &[fetch.clone(), decode.clone(), recompute.clone()])
            .unwrap()
            .strategy,
        RecoveryStrategy::Decode
    );

    fetch.fetch_cost = 30;
    decode.decode_cost = 40;
    recompute.recompute_cost = 10;
    let choice = choose_recovery(target, &[fetch, decode, recompute]).unwrap();
    assert_eq!(choice.strategy, RecoveryStrategy::Recompute);
    assert_eq!(choice.cost, 10);
    assert_eq!(choice.recompute_cost, 10);
}

#[test]
fn cost_components_are_summed_for_work_actually_incurred() {
    let target = Hash32::from_bytes([0x22u8; 32]);
    let mut decode = candidate(
        RecoveryStrategy::Decode,
        "decode",
        true,
        Determinism::Unknown,
    );
    decode.fetch_cost = 4;
    decode.decode_cost = 6;
    let mut fetch = candidate(RecoveryStrategy::Fetch, "fetch", true, Determinism::Unknown);
    fetch.fetch_cost = 9;
    let choice = choose_recovery(target, &[fetch, decode]).unwrap();
    assert_eq!(choice.strategy, RecoveryStrategy::Fetch);
    assert_eq!(choice.cost, 9);

    let mut decode = candidate(
        RecoveryStrategy::Decode,
        "decode",
        true,
        Determinism::Unknown,
    );
    decode.fetch_cost = 4;
    decode.decode_cost = 2;
    let mut fetch = candidate(RecoveryStrategy::Fetch, "fetch", true, Determinism::Unknown);
    fetch.fetch_cost = 7;
    let choice = choose_recovery(target, &[fetch, decode]).unwrap();
    assert_eq!(choice.strategy, RecoveryStrategy::Decode);
    assert_eq!(choice.cost, 6);
}

#[test]
fn recompute_requires_exact_determinism() {
    let target = Hash32::from_bytes([0x33u8; 32]);
    for determinism in [Determinism::Declared, Determinism::Unknown] {
        let mut recompute = candidate(RecoveryStrategy::Recompute, "recompute", true, determinism);
        recompute.recompute_cost = 1;
        let unavailable_fetch = candidate(
            RecoveryStrategy::Fetch,
            "fetch",
            false,
            Determinism::Unknown,
        );
        let err = choose_recovery(target, &[unavailable_fetch, recompute]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("non-exact determinism"));
        assert!(text.contains(determinism.as_str()));
    }
}

#[test]
fn recompute_requires_every_input_to_be_available() {
    let target = Hash32::from_bytes([0x44u8; 32]);
    let mut recompute = candidate(
        RecoveryStrategy::Recompute,
        "recompute",
        true,
        Determinism::Exact,
    );
    recompute.missing_inputs = vec![Hash32::from_bytes([0x55u8; 32])];
    let unavailable_fetch = candidate(
        RecoveryStrategy::Fetch,
        "fetch",
        false,
        Determinism::Unknown,
    );
    let err = choose_recovery(target, &[unavailable_fetch, recompute]).unwrap_err();
    assert!(err.to_string().contains("missing input"));
}

#[test]
fn tie_break_is_cost_then_fetch_decode_recompute_then_name() {
    let target = Hash32::from_bytes([0x66u8; 32]);
    let fetch = candidate(
        RecoveryStrategy::Fetch,
        "z-fetch",
        true,
        Determinism::Unknown,
    );
    let decode = candidate(
        RecoveryStrategy::Decode,
        "a-decode",
        true,
        Determinism::Unknown,
    );
    let recompute = candidate(
        RecoveryStrategy::Recompute,
        "a-recompute",
        true,
        Determinism::Exact,
    );
    // Equal cost: fetch wins even though its name sorts last.
    let choice =
        choose_recovery(target, &[recompute.clone(), decode.clone(), fetch.clone()]).unwrap();
    assert_eq!(choice.strategy, RecoveryStrategy::Fetch);

    // Fetch unavailable: decode wins on strategy rank.
    let unavailable_fetch = candidate(
        RecoveryStrategy::Fetch,
        "z-fetch",
        false,
        Determinism::Unknown,
    );
    let choice = choose_recovery(target, &[recompute, decode.clone(), unavailable_fetch]).unwrap();
    assert_eq!(choice.strategy, RecoveryStrategy::Decode);

    // Equal cost and strategy: lexical name decides, independent of order.
    let alpha = candidate(
        RecoveryStrategy::Decode,
        "alpha",
        true,
        Determinism::Unknown,
    );
    let zeta = candidate(RecoveryStrategy::Decode, "zeta", true, Determinism::Unknown);
    assert_eq!(
        choose_recovery(target, &[zeta.clone(), alpha.clone()])
            .unwrap()
            .name,
        "alpha"
    );
    assert_eq!(
        choose_recovery(target, &[alpha, zeta]).unwrap().name,
        "alpha"
    );
}

#[test]
fn no_eligible_strategy_names_every_reason() {
    let target = Hash32::from_bytes([0x77u8; 32]);
    let fetch = candidate(
        RecoveryStrategy::Fetch,
        "fetch",
        false,
        Determinism::Unknown,
    );
    let decode = candidate(
        RecoveryStrategy::Decode,
        "decode",
        false,
        Determinism::Unknown,
    );
    let mut recompute = candidate(
        RecoveryStrategy::Recompute,
        "recompute",
        true,
        Determinism::Declared,
    );
    recompute.missing_inputs = vec![Hash32::from_bytes([0x88u8; 32])];
    let err = choose_recovery(target, &[fetch, decode, recompute]).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with("no eligible recovery strategy"));
    assert!(text.contains("fetch (unavailable)"));
    assert!(text.contains("decode (unavailable)"));
    assert!(text.contains("non-exact determinism (declared)"));
    // Every candidate is named with a typed reason.
    assert_eq!(err.rejections.len(), 3);
    assert!(matches!(
        err.rejections[0].1[0],
        RecoveryRejection::Unavailable
    ));
    assert!(text.contains("missing input"));
}

// ---------------------------------------------------------------------------
// Closure
// ---------------------------------------------------------------------------

fn chain_store() -> (tempfile::TempDir, Store, Hash32, Hash32, Hash32) {
    let (dir, mut store) = new_store();
    let leaf = create_file(&mut store, b"leaf", b"leaf bytes");
    let middle = create_file(&mut store, b"middle", b"middle bytes");
    let top = create_file(&mut store, b"top", b"top bytes");

    let middle_record = make_record(
        "transform",
        vec![make_input("input", content_of(&store, leaf), Some(leaf))],
        vec![content_of(&store, middle)],
    );
    store.record_provenance(&[middle], &middle_record).unwrap();

    let top_record = make_record(
        "eval",
        vec![make_input(
            "model",
            content_of(&store, middle),
            Some(middle),
        )],
        vec![content_of(&store, top)],
    );
    store.record_provenance(&[top], &top_record).unwrap();
    (dir, store, leaf, middle, top)
}

#[test]
fn closure_walks_transitively_and_respects_the_depth_bound() {
    let (_dir, store, leaf, middle, top) = chain_store();
    let closure = store.provenance_closure(top, 4).unwrap();
    assert_eq!(closure.target, top);
    assert_eq!(closure.depth_limit, 4);
    assert_eq!(closure.nodes.len(), 3);
    assert_eq!(closure.nodes[0].object, Some(top));
    assert_eq!(closure.nodes[0].depth, 0);
    assert_eq!(closure.nodes[1].object, Some(middle));
    assert_eq!(closure.nodes[1].depth, 1);
    assert_eq!(closure.nodes[2].object, Some(leaf));
    assert_eq!(closure.nodes[2].depth, 2);
    assert!(closure.nodes[2].provenance.is_none());

    // Depth 2 is just enough; depth 1 must fail closed, not truncate.
    assert!(store.provenance_closure(top, 2).is_ok());
    let err = store.provenance_closure(top, 1).unwrap_err();
    assert_eq!(err.class(), Class::LimitExceeded);

    assert_eq!(
        store.provenance_closure(top, 0).unwrap_err().class(),
        Class::InvalidArgument
    );
    let _ = leaf;
}

#[test]
fn closure_rejects_cycles_and_terminates() {
    let (_dir, mut store) = new_store();
    let a = create_file(&mut store, b"a", b"a bytes");
    let b = create_file(&mut store, b"b", b"b bytes");

    let record_a = make_record(
        "op-a",
        vec![make_input("from-b", content_of(&store, b), Some(b))],
        vec![content_of(&store, a)],
    );
    store.record_provenance(&[a], &record_a).unwrap();
    let record_b = make_record(
        "op-b",
        vec![make_input("from-a", content_of(&store, a), Some(a))],
        vec![content_of(&store, b)],
    );
    store.record_provenance(&[b], &record_b).unwrap();

    let err = store.provenance_closure(a, 8).unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
    assert!(err.message().contains("cycle"));
}

// ---------------------------------------------------------------------------
// GC retention
// ---------------------------------------------------------------------------

#[test]
fn gc_retains_provenance_and_inputs_until_the_output_is_deleted() {
    let (dir, mut store) = new_store();
    let input = create_file(&mut store, b"input", b"input bytes");
    let output = create_file(&mut store, b"output", b"output bytes");
    let input_content = content_of(&store, input);
    let output_content = content_of(&store, output);
    let input_manifest = read_manifest(dir.path(), input_content).unwrap();
    let record = make_record(
        "eval",
        vec![make_input("dataset", input_content, Some(input))],
        vec![output_content],
    );
    let provenance = store.record_provenance(&[output], &record).unwrap();
    let blob = manifest_path(dir.path(), provenance);
    assert!(blob.exists());

    let report = store.gc(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(
        blob.exists(),
        "provenance blob must survive while its output does"
    );
    assert!(store.current_state().objects.contains_key(&input));
    assert!(read_manifest(dir.path(), input_content).is_ok());
    for chunk in &input_manifest.chunks {
        assert!(
            object_path(dir.path(), chunk.hash).exists(),
            "an input chunk must survive while the output is retained"
        );
    }

    // Deleting the output (and its now-unreferenced input) makes both the
    // provenance receipt and the input bytes collectable.
    let root = store.resolve_path("/").unwrap();
    store.unlink(root, b"output").unwrap();
    store.unlink(root, b"input").unwrap();
    let report = store.gc(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(!store.current_state().objects.contains_key(&output));
    assert!(
        store.provenance_ids_of(output).unwrap().is_empty(),
        "report={report:?} relations={:?}",
        store.current_state().relations.values().collect::<Vec<_>>()
    );
    assert!(!blob.exists(), "provenance blob must be reclaimed");
    for chunk in &input_manifest.chunks {
        assert!(
            !object_path(dir.path(), chunk.hash).exists(),
            "input chunks must be reclaimed with the deleted output"
        );
    }
    assert!(!manifest_path(dir.path(), input_content).exists());
}

// ---------------------------------------------------------------------------
// verify --full
// ---------------------------------------------------------------------------

#[test]
fn verify_full_catches_a_tampered_record() {
    let (dir, mut store) = new_store();
    let output = create_file(&mut store, b"output", b"output bytes");
    let record = make_record("compile", Vec::new(), vec![content_of(&store, output)]);
    let provenance = store.record_provenance(&[output], &record).unwrap();
    let blob = manifest_path(dir.path(), provenance);
    fs::write(&blob, b"not a canonical provenance record").unwrap();

    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.class == "Corrupt"
                && problem.detail.contains(&provenance.to_hex())),
        "{:?}",
        report.problems
    );
}

#[test]
fn verify_full_catches_a_record_that_no_longer_matches_the_object() {
    let (_dir, mut store) = new_store();
    let output = create_file(&mut store, b"output", b"output bytes");
    let record = make_record("compile", Vec::new(), vec![content_of(&store, output)]);
    store.record_provenance(&[output], &record).unwrap();

    // Rewriting the object changes its content root; the old receipt is stale.
    store.write_file(output, 0, b"rewritten").unwrap();

    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    assert!(
        report.problems.iter().any(|problem| {
            problem.class == "Corrupt"
                && problem.detail.contains("does not declare the content root")
        }),
        "{:?}",
        report.problems
    );
}

#[test]
fn verify_full_rejects_a_malformed_relation_target() {
    let (_dir, mut store) = new_store();
    let output = create_file(&mut store, b"output", b"output bytes");
    store
        .relation_add(
            output,
            "provenance",
            RelationTarget::Value(Value::Str("not-a-provenance-id".to_owned())),
            None,
            BTreeMap::new(),
        )
        .unwrap();

    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.detail.contains("provenance relation")),
        "{:?}",
        report.problems
    );
}

#[test]
fn provenance_survives_reopen() {
    let (dir, mut store) = new_store();
    let output = create_file(&mut store, b"output", b"output bytes");
    let record = make_record("eval", Vec::new(), vec![content_of(&store, output)]);
    let provenance = store.record_provenance(&[output], &record).unwrap();
    drop(store);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.provenance_ids_of(output).unwrap(), vec![provenance]);
    assert_eq!(store.provenance_of(output).unwrap().unwrap(), record);
    assert_eq!(State::ROOT_ID, store.resolve_path("/").unwrap());
}
