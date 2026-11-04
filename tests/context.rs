//! F2 integration tests: Context Root persistence/resolution/materialization,
//! capability enforcement, budgets, and causal identity.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::context::{Budget, CapabilityScope, CausalClosure, ContextRoot, IdentityMode};
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::context::{compute_causal_closure, effective_budget, CapabilityContext};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::{
    derive_version_id, derive_version_id_with_identity, object_causal_closure, ObjectKind,
    ObjectRecord, State, ATTR_CAUSAL_CLOSURE, ATTR_CAUSAL_HASH, ATTR_CAUSAL_PARENTS,
    ATTR_IDENTITY_MODE,
};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn init(path: &Path) -> Store {
    Store::init(path, opts()).unwrap()
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

fn create(store: &mut Store, name: &str, text: &str, extra: &[(&str, &str)]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(
            root,
            name.as_bytes(),
            text.as_bytes(),
            0o644,
            0,
            0,
            attrs(extra),
        )
        .unwrap()
}

fn context_root(
    selector_text: &str,
    snapshot: Hash32,
    causal: Vec<Hash32>,
    budget: Option<Budget>,
) -> ContextRoot {
    let selector = selector::parse(selector_text).unwrap();
    ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        causal,
        None,
        budget,
    )
    .unwrap()
}

#[test]
fn context_create_resolve_materialize_zero_one_n() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    create(&mut store, "b.txt", "beta", &[("project", "demo")]);
    create(&mut store, "other.txt", "other", &[("project", "other")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let root = context_root(
        "select attrs.project == \"demo\" order by size asc",
        snapshot,
        Vec::new(),
        None,
    );
    let relation = store.save_context_root("demo", &root, None, false).unwrap();
    assert_eq!(
        store.load_context_root("demo").unwrap().relation_id,
        relation
    );

    // n = 2
    let resolved = store
        .resolve_context(
            "demo",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 2);
    assert_eq!(resolved.resolution.results.len(), 2);
    assert_eq!(resolved.resolution.snapshot_id, snapshot);
    assert_eq!(resolved.resolution.selector_id, root.selector_id);
    assert_eq!(resolved.stored.context_root_id, root.id().unwrap());
    assert!(!resolved.truncated);

    // 0 and 1 cardinality are explicit.
    let zero = context_root(
        "select attrs.project == \"missing\"",
        snapshot,
        Vec::new(),
        None,
    );
    store.save_context_root("zero", &zero, None, false).unwrap();
    let resolved = store
        .resolve_context(
            "zero",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 0);
    assert!(resolved.resolution.results.is_empty());

    let one = context_root(
        "select attrs.project == \"other\"",
        snapshot,
        Vec::new(),
        None,
    );
    store.save_context_root("one", &one, None, false).unwrap();
    let resolved = store
        .resolve_context(
            "one",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);

    // Materialize n=2 atomically and verify exact bytes plus manifest.
    let out = dir.path().join("out");
    let report = store
        .materialize_context(
            "demo",
            &out,
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"alpha");
    assert_eq!(fs::read(out.join("b.txt")).unwrap(), b"beta");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(report.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["context_root_id"], root.id().unwrap().to_hex());
    assert_eq!(manifest["snapshot_id"], snapshot.to_hex());
    assert_eq!(manifest["selector_id"], root.selector_id.to_hex());
    assert_eq!(manifest["files"].as_array().unwrap().len(), 2);
    for file in manifest["files"].as_array().unwrap() {
        assert_eq!(file["content_hash"].as_str().unwrap().len(), 64);
    }
}

#[test]
fn snapshot_pinning_never_follows_the_live_head() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let root = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    store.save_context_root("demo", &root, None, false).unwrap();
    // Advance the live store.
    create(&mut store, "b.txt", "beta", &[("project", "demo")]);
    let resolved = store
        .resolve_context(
            "demo",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);
    assert_eq!(resolved.resolution.snapshot_id, snapshot);
}

#[test]
fn duplicate_names_require_explicit_replacement() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let first = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    store
        .save_context_root("demo", &first, None, false)
        .unwrap();
    let err = store
        .save_context_root("demo", &first, None, false)
        .unwrap_err();
    assert_eq!(err.class(), Class::Exists);
    // Replacement is a single delete+add commit.
    let second = context_root("select true", snapshot, Vec::new(), None);
    let before = store.generation();
    store
        .save_context_root("demo", &second, None, true)
        .unwrap();
    assert_eq!(store.generation(), before + 1);
    assert_eq!(
        store.load_context_root("demo").unwrap().root.selector_id,
        second.selector_id
    );
}

#[test]
fn capability_denials_do_not_leak_and_budgets_fail_closed() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    let a = create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let object = store.object_meta(a).unwrap();
    let root = store.resolve_path("/").unwrap();
    store
        .relation_add(
            root,
            "derived_from",
            weftfs::state::RelationTarget::Object(a),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    let snapshot = store.snapshot_create("s1").unwrap();

    // No predicates granted: a relation selector is a denial.
    let relation_selector = format!("select relation(derived_from, obj:{})", object.id);
    let relation_root = context_root(&relation_selector, snapshot, Vec::new(), None);
    store
        .save_context_root("relation", &relation_root, None, false)
        .unwrap();
    let cap = CapabilityContext::new(
        CapabilityScope::new(
            "agent",
            vec!["default".to_owned()],
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let err = store
        .resolve_context(
            "relation",
            Some(&cap),
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    // The denial must not expose the matched object or count.
    assert!(!err.message().contains(&object.id.to_hex()));

    // Namespace mismatch is the same non-disclosing class.
    let cap_other_namespace = CapabilityContext::new(
        CapabilityScope::new(
            "agent",
            vec!["other".to_owned()],
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let err = store
        .resolve_context(
            "relation",
            Some(&cap_other_namespace),
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // Denied root id: capability grants only a different root.
    let demo_root = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    store
        .save_context_root("demo", &demo_root, None, false)
        .unwrap();
    let foreign = CapabilityContext::new(
        CapabilityScope::new(
            "agent",
            Vec::new(),
            vec![Hash32::from_hex(&"11".repeat(32)).unwrap()],
            vec!["derived_from".to_owned()],
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let err = store
        .resolve_context(
            "demo",
            Some(&foreign),
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);

    // Budget: max_results=1 over a two-result selection is LimitExceeded, not
    // silent truncation.
    create(&mut store, "b.txt", "beta", &[("project", "demo")]);
    let snapshot2 = store.snapshot_create("s2").unwrap();
    let budget = Budget {
        max_results: Some(1),
        max_bytes: None,
        max_depth: None,
        max_work: None,
    };
    let limited = context_root(
        "select attrs.project == \"demo\"",
        snapshot2,
        Vec::new(),
        Some(budget),
    );
    store
        .save_context_root("limited", &limited, None, false)
        .unwrap();
    let err = store
        .resolve_context(
            "limited",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::LimitExceeded);
}

#[test]
fn capability_hash_participates_in_context_root_identity() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let plain = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    let scope = CapabilityScope::new(
        "agent",
        vec!["default".to_owned()],
        Vec::new(),
        vec!["tagged_as".to_owned()],
        None,
        None,
    )
    .unwrap();
    let capability = CapabilityContext::new(scope.clone()).unwrap();
    let selector = selector::parse("select attrs.project == \"demo\"").unwrap();
    let bound = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        Some(capability.hash),
        None,
    )
    .unwrap();
    assert_ne!(plain.id().unwrap(), bound.id().unwrap());
    store
        .save_context_root("bound", &bound, Some(&scope), false)
        .unwrap();
    // Stored capability record is hash-verifiable and reused.
    assert_eq!(store.load_capability(capability.hash).unwrap(), scope);
    let resolved = store
        .resolve_context(
            "bound",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);
    assert_eq!(resolved.capability_hash, Some(capability.hash));
}

#[test]
fn effective_budget_is_component_wise_minimum() {
    let root = Budget {
        max_results: Some(10),
        max_bytes: Some(100),
        max_depth: None,
        max_work: Some(50),
    };
    let cap_scope = CapabilityScope::new(
        "agent",
        vec!["default".to_owned()],
        Vec::new(),
        Vec::new(),
        Some(Budget {
            max_results: Some(3),
            max_bytes: Some(500),
            max_depth: Some(1),
            max_work: None,
        }),
        None,
    )
    .unwrap();
    let cap = CapabilityContext::new(cap_scope).unwrap();
    let effective = effective_budget(Some(&root), Some(&cap)).unwrap();
    assert_eq!(effective.max_results, Some(3));
    assert_eq!(effective.max_bytes, Some(100));
    assert_eq!(effective.max_depth, Some(1));
    assert_eq!(effective.max_work, Some(50));
}

#[test]
fn bytes_only_version_is_byte_identical_and_causal_exact_changes_it() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    let parent = create(&mut store, "parent.txt", "parent", &[]);
    let parent_record = store.object_meta(parent).unwrap();
    let child = create(&mut store, "child.txt", "child", &[]);
    let child_record = store.object_meta(child).unwrap();
    // The bytes-only identity must equal the frozen v1 derivation.
    let expected = derive_version_id(
        child_record.id,
        child_record.content,
        child_record.size,
        child_record.mtime_ns,
        None,
    )
    .unwrap();
    assert_eq!(child_record.version, expected);

    // Release the init writer lock before reopening for mutation.
    drop(store);

    // A causal_exact write includes the closure hash in VersionId.
    let mut store = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
    let causal_child = store
        .write_file_causal(child, 0, b"child", &[parent])
        .unwrap();
    let record = store.object_meta(child).unwrap();
    assert_eq!(record.version, causal_child);
    assert_ne!(record.version, child_record.version);
    assert_eq!(
        record.attrs.get(ATTR_IDENTITY_MODE),
        Some(&Value::Str("causal_exact".to_owned()))
    );
    let closure = object_causal_closure(&record).unwrap().unwrap();
    assert_eq!(closure.parents, vec![parent_record.version]);
    assert_eq!(closure.closure, vec![parent_record.version]);
    assert_eq!(
        record.attrs.get(ATTR_CAUSAL_HASH),
        Some(&Value::Bytes(closure.id().unwrap().as_bytes().to_vec()))
    );
    // The closure record is content-addressed and readable.
    let blob = weftfs::recovery::read_blob_verified(
        store.root(),
        weftfs::recovery::BlobKind::Manifest,
        closure.id().unwrap(),
        weftcodec::hash::DOMAIN_CAUSAL,
    )
    .unwrap();
    assert_eq!(
        CausalClosure::from_canonical_bytes(&blob, &weftcodec::Limits::default()).unwrap(),
        closure
    );
}

#[test]
fn transitive_closure_and_cycle_termination() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    let a = create(&mut store, "a.txt", "a", &[]);
    let a_record = store.object_meta(a).unwrap();
    let b = create(&mut store, "b.txt", "b", &[]);
    let b_version = store.write_file_causal(b, 0, b"b", &[a]).unwrap();
    let c = create(&mut store, "c.txt", "c", &[]);
    let c_version = store.write_file_causal(c, 0, b"c", &[b]).unwrap();
    let c_record = store.object_meta(c).unwrap();
    let closure = object_causal_closure(&c_record).unwrap().unwrap();
    assert_eq!(closure.parents, vec![b_version]);
    let mut expected = vec![a_record.version, b_version];
    expected.sort();
    assert_eq!(closure.closure, expected);
    assert_eq!(c_record.version, c_version);

    // A synthetic cycle A -> B -> A must terminate with a set.
    let mut state = State::default();
    let mut a_attrs = BTreeMap::new();
    let mut b_attrs = BTreeMap::new();
    let a_version = Hash32::from_hex(&"aa".repeat(32)).unwrap();
    let b_version = Hash32::from_hex(&"bb".repeat(32)).unwrap();
    let a_closure = CausalClosure::new(vec![b_version], vec![a_version, b_version]).unwrap();
    let b_closure = CausalClosure::new(vec![a_version], vec![a_version, b_version]).unwrap();
    a_attrs.insert(
        ATTR_CAUSAL_PARENTS.to_owned(),
        Value::Array(vec![Value::Bytes(b_version.as_bytes().to_vec())]),
    );
    a_attrs.insert(
        ATTR_CAUSAL_CLOSURE.to_owned(),
        Value::Array(vec![
            Value::Bytes(a_version.as_bytes().to_vec()),
            Value::Bytes(b_version.as_bytes().to_vec()),
        ]),
    );
    a_attrs.insert(
        ATTR_CAUSAL_HASH.to_owned(),
        Value::Bytes(a_closure.id().unwrap().as_bytes().to_vec()),
    );
    a_attrs.insert(
        ATTR_IDENTITY_MODE.to_owned(),
        Value::Str("causal_exact".to_owned()),
    );
    b_attrs.insert(
        ATTR_CAUSAL_PARENTS.to_owned(),
        Value::Array(vec![Value::Bytes(a_version.as_bytes().to_vec())]),
    );
    b_attrs.insert(
        ATTR_CAUSAL_CLOSURE.to_owned(),
        Value::Array(vec![
            Value::Bytes(a_version.as_bytes().to_vec()),
            Value::Bytes(b_version.as_bytes().to_vec()),
        ]),
    );
    b_attrs.insert(
        ATTR_CAUSAL_HASH.to_owned(),
        Value::Bytes(b_closure.id().unwrap().as_bytes().to_vec()),
    );
    b_attrs.insert(
        ATTR_IDENTITY_MODE.to_owned(),
        Value::Str("causal_exact".to_owned()),
    );
    let a_id = Hash32::from_hex(&"01".repeat(32)).unwrap();
    let b_id = Hash32::from_hex(&"02".repeat(32)).unwrap();
    state.objects.insert(
        a_id,
        ObjectRecord {
            id: a_id,
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: 1,
            version: a_version,
            content: weftfs::identity::content_hash(b"a"),
            size: 1,
            attrs: a_attrs,
        },
    );
    state.objects.insert(
        b_id,
        ObjectRecord {
            id: b_id,
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: 1,
            version: b_version,
            content: weftfs::identity::content_hash(b"b"),
            size: 1,
            attrs: b_attrs,
        },
    );
    let computed = compute_causal_closure(&state, &[a_id], &weftcodec::Limits::default()).unwrap();
    assert_eq!(computed.parents, vec![a_version]);
    assert_eq!(computed.closure, vec![a_version, b_version]);
}

#[test]
fn corrupted_context_record_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let root = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    store.save_context_root("demo", &root, None, false).unwrap();
    // A second relation with the reserved predicate but a corrupted record.
    let bad = weftfs::state::RelationRecord::new(
        State::ROOT_ID,
        "context_root",
        weftfs::state::RelationTarget::Value(Value::Str("bad".to_owned())),
        None,
        BTreeMap::from([(
            "context.record".to_owned(),
            Value::Str("deadbeef".to_owned()),
        )]),
    )
    .unwrap();
    store
        .relation_add(
            State::ROOT_ID,
            "context_root",
            bad.target.clone(),
            None,
            bad.attrs.clone(),
        )
        .unwrap();
    let err = store.load_context_root("bad").unwrap_err();
    assert!(matches!(
        err.class(),
        Class::Corrupt | Class::Malformed | Class::NonCanonical
    ));
    // The intact root still resolves; the corrupt one cannot poison listings.
    assert!(store.list_context_roots().is_err());
    let resolved = store
        .resolve_context(
            "demo",
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap();
    assert_eq!(resolved.resolution.matched_count, 1);
}

#[test]
fn materialization_mismatch_leaves_no_output() {
    let dir = tempdir().unwrap();
    let mut store = init(dir.path());
    let a = create(&mut store, "a.txt", "alpha", &[("project", "demo")]);
    let snapshot = store.snapshot_create("s1").unwrap();
    let root = context_root(
        "select attrs.project == \"demo\"",
        snapshot,
        Vec::new(),
        None,
    );
    store.save_context_root("demo", &root, None, false).unwrap();
    let record = store.object_meta(a).unwrap();
    let manifest = weftfs::recovery::read_manifest(store.root(), record.content).unwrap();
    let chunk = manifest.chunks[0].hash;
    let chunk_path = weftfs::recovery::object_path(store.root(), chunk);
    let mut bytes = fs::read(&chunk_path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&chunk_path, bytes).unwrap();

    let out = dir.path().join("out");
    let err = store
        .materialize_context(
            "demo",
            &out,
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
    assert!(!out.exists(), "failed materialization left output behind");
}

#[test]
fn golden_causal_vectors_match_both_languages() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/context/v1/causal_versions.json");
    let bytes = std::fs::read(&path).unwrap();
    let vector: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let closure = &vector["closure"];
    let parents: Vec<Hash32> = closure["parents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| Hash32::from_hex(value.as_str().unwrap()).unwrap())
        .collect();
    let entries: Vec<Hash32> = closure["closure"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| Hash32::from_hex(value.as_str().unwrap()).unwrap())
        .collect();
    let record = CausalClosure::new(parents, entries).unwrap();
    assert_eq!(
        hex::encode(record.canonical_bytes().unwrap()),
        closure["canonical_hex"].as_str().unwrap()
    );
    assert_eq!(
        record.id().unwrap().to_hex(),
        closure["causal_hash"].as_str().unwrap()
    );
    let causal_hash = record.id().unwrap();
    for case in vector["vectors"].as_array().unwrap() {
        let object = Hash32::from_hex(case["object"].as_str().unwrap()).unwrap();
        let content = Hash32::from_hex(case["content"].as_str().unwrap()).unwrap();
        let size = case["size"].as_u64().unwrap();
        let mtime = case["mtime_ns"].as_i64().unwrap();
        let parent = case["parent"]
            .as_str()
            .map(|text| Hash32::from_hex(text).unwrap());
        let bytes_only = derive_version_id_with_identity(
            object,
            content,
            size,
            mtime,
            parent,
            IdentityMode::BytesOnly,
            None,
        )
        .unwrap();
        let causal_exact = derive_version_id_with_identity(
            object,
            content,
            size,
            mtime,
            parent,
            IdentityMode::CausalExact,
            Some(causal_hash),
        )
        .unwrap();
        assert_eq!(
            bytes_only.to_hex(),
            case["bytes_only_version_id"].as_str().unwrap(),
            "bytes_only vector {}",
            case["name"]
        );
        assert_eq!(
            causal_exact.to_hex(),
            case["causal_exact_version_id"].as_str().unwrap(),
            "causal_exact vector {}",
            case["name"]
        );
    }
}

#[test]
fn identity_mode_rejects_bad_mode_text() {
    assert!(IdentityMode::parse("bytes_only").is_ok());
    assert!(IdentityMode::parse("causal_exact").is_ok());
    assert!(IdentityMode::parse("sometimes").is_err());
}
