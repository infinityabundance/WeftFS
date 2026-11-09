//! MW2 multi-committer tests (subphase R4).
//!
//! These tests run two independent store handles against one local store. The
//! optimistic mode has no exclusive writer, so both handles open concurrently.
//! Every assertion is about externally visible state or a typed error; the
//! test never inspects a private in-memory field to declare success.

use std::collections::BTreeMap;
use std::fs;

use tempfile::TempDir;
use weftfs::auth::Principal;
use weftfs::error::Class;
use weftfs::mw2::{OutcomeLookup, TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options(mode: WriterMode) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

/// Create the store in single mode (the only mode that may initialize), then
/// drop it so the test can open it optimistically.
fn new_store() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = Store::init(&path, options(WriterMode::Single)).unwrap();
        drop(store);
    }
    (dir, path)
}

fn open(path: &std::path::Path) -> Store {
    Store::open_with_options(path, OpenMode::ReadWrite, options(WriterMode::Optimistic)).unwrap()
}

struct FilePlan {
    record: ObjectRecord,
    entry: EntryRecord,
}

fn plan_file(
    store: &Store,
    parent: weftfs::identity::Hash32,
    name: &[u8],
    bytes: &[u8],
    seed: &[u8],
) -> FilePlan {
    let (manifest, _content_hash, size) = store.prepare_content(bytes).unwrap();
    let id = weftfs::identity::object_id_from_seed(seed);
    let version = derive_version_id(id, manifest, size, NOW, None).unwrap();
    let record = ObjectRecord {
        id,
        kind: ObjectKind::File,
        mode: 0o644,
        uid: 0,
        gid: 0,
        mtime_ns: NOW,
        version,
        content: manifest,
        size,
        attrs: BTreeMap::new(),
    };
    let entry = EntryRecord {
        id: derive_entry_id(parent, name).unwrap(),
        parent,
        name: name.to_vec(),
        target: id,
    };
    FilePlan { record, entry }
}

fn stage(store: &Store, id: &str, plan: &FilePlan) -> weftfs::mw2::Transaction {
    let mut tx = store.begin_transaction(id).unwrap();
    let state = store.current_state();
    tx.put_object(state, plan.record.clone()).unwrap();
    tx.put_entry(state, plan.entry.clone()).unwrap();
    tx.set_tip(state, plan.record.id, plan.record.version)
        .unwrap();
    tx
}

#[test]
fn optimistic_open_creates_the_policy_and_single_open_refuses_it() {
    let (_dir, path) = new_store();
    {
        let store = open(&path);
        assert_eq!(store.options().writer_mode, WriterMode::Optimistic);
        drop(store);
    }
    assert!(path.join("writers").is_file(), "policy record was written");
    let err = match Store::open_with_options(
        &path,
        OpenMode::ReadWrite,
        options(WriterMode::Single),
    ) {
        Ok(_) => panic!("a single-mode writable open must refuse an optimistic store"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
}

#[test]
fn disjoint_transactions_both_commit_and_rebase() {
    let (_dir, path) = new_store();
    let mut a = open(&path);
    let mut b = open(&path);

    let plan_a = plan_file(&a, State::ROOT_ID, b"a.txt", b"alpha", b"seed-a");
    let plan_b = plan_file(&a, State::ROOT_ID, b"b.txt", b"beta", b"seed-b");

    // Both transactions are built against the same head. A publishes first.
    let tx_a = stage(&a, "0000000000000000000000000000000a", &plan_a);
    let tx_b = stage(&b, "0000000000000000000000000000000b", &plan_b);
    assert_eq!(tx_a.base_head(), tx_b.base_head());

    let out_a = a
        .execute_transaction(&tx_a, &TransactionOptions::default())
        .unwrap();
    let out_b = b
        .execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap();
    assert_eq!(out_a.idempotent, false);
    assert_eq!(out_b.idempotent, false);
    assert!(out_b.attempts >= 1);
    assert_ne!(out_a.commit, out_b.commit);

    // Each handle is a snapshot. A fresh open selects the published head that
    // contains both commits.
    let check = open(&path);
    assert!(check.verify(true).unwrap().ok, "full verify after rebase");
    let root = State::ROOT_ID;
    let entries = check.list_dir(root).unwrap();
    assert!(entries.iter().any(|entry| entry.name == b"a.txt"));
    assert!(entries.iter().any(|entry| entry.name == b"b.txt"));
    assert_eq!(check.read_object(plan_a.record.id).unwrap(), b"alpha");
    assert_eq!(check.read_object(plan_b.record.id).unwrap(), b"beta");
}

#[test]
fn same_name_create_is_a_typed_conflict() {
    let (_dir, path) = new_store();
    let mut a = open(&path);
    let mut b = open(&path);

    let plan_a = plan_file(&a, State::ROOT_ID, b"same", b"one", b"seed-1");
    let plan_b = plan_file(&a, State::ROOT_ID, b"same", b"two", b"seed-2");
    let tx_a = stage(&a, "0000000000000000000000000000001a", &plan_a);
    let tx_b = stage(&b, "0000000000000000000000000000001b", &plan_b);

    let out = a
        .execute_transaction(&tx_a, &TransactionOptions::default())
        .unwrap();
    let head_after_a = a.head();
    let err = b
        .execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    assert!(err.message().contains("NameExists"), "{err}");
    assert_eq!(a.head(), out.commit);
    assert_eq!(a.head(), head_after_a, "a conflict publishes nothing");

    let root = State::ROOT_ID;
    let entry = a.lookup(root, b"same").unwrap();
    assert_eq!(entry, plan_a.record.id);
}

#[test]
fn same_object_update_without_observing_the_winner_is_rejected() {
    let (_dir, path) = new_store();
    let mut a = open(&path);

    let plan = plan_file(&a, State::ROOT_ID, b"f", b"v1", b"seed-f");
    let tx = stage(&a, "0000000000000000000000000000002a", &plan);
    a.execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();

    // Two handles observe the same v1 object and both plan a v2 update.
    let mut b = open(&path);
    let (manifest_a, _, size_a) = a.prepare_content(b"v2-a").unwrap();
    let (manifest_b, _, size_b) = b.prepare_content(b"v2-b").unwrap();
    let version_a = derive_version_id(plan.record.id, manifest_a, size_a, NOW + 1, None).unwrap();
    let version_b = derive_version_id(plan.record.id, manifest_b, size_b, NOW + 2, None).unwrap();
    let mut rec_a = plan.record.clone();
    rec_a.content = manifest_a;
    rec_a.size = size_a;
    rec_a.version = version_a;
    rec_a.mtime_ns = NOW + 1;
    let mut rec_b = plan.record.clone();
    rec_b.content = manifest_b;
    rec_b.size = size_b;
    rec_b.version = version_b;
    rec_b.mtime_ns = NOW + 2;

    let mut tx_a = a
        .begin_transaction("0000000000000000000000000000002b")
        .unwrap();
    let state_a = a.current_state();
    tx_a.put_object(state_a, rec_a.clone()).unwrap();
    let mut tx_b = b
        .begin_transaction("0000000000000000000000000000002c")
        .unwrap();
    let state_b = b.current_state();
    tx_b.put_object(state_b, rec_b.clone()).unwrap();

    a.execute_transaction(&tx_a, &TransactionOptions::default())
        .unwrap();
    let err = b
        .execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    assert!(err.message().contains("ObjectChanged"), "{err}");
    assert_eq!(a.read_object(plan.record.id).unwrap(), b"v2-a");
}

#[test]
fn replaying_the_same_transaction_id_is_idempotent() {
    let (_dir, path) = new_store();
    let mut store = open(&path);
    let plan = plan_file(&store, State::ROOT_ID, b"once", b"bytes", b"seed-once");
    let tx = stage(&store, "0000000000000000000000000000003a", &plan);

    let first = store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    let head = store.head();
    let second = store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    assert_eq!(first.commit, second.commit);
    assert!(second.idempotent);
    assert_eq!(store.head(), head, "a replay publishes no new commit");
    assert_eq!(store.generation(), first.generation);
}

#[test]
fn an_ambiguous_outcome_is_resolved_by_the_op_digest() {
    let (_dir, path) = new_store();
    let mut store = open(&path);
    let plan = plan_file(&store, State::ROOT_ID, b"amb", b"payload", b"seed-amb");
    let id = "0000000000000000000000000000004a";
    let tx = stage(&store, id, &plan);
    let first = store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();

    // Simulate a lost acknowledgement: the commit is published, but the
    // discovery record never reached `committed`.
    let record = weftfs::mw2::TxRecord {
        id: id.to_owned(),
        state: weftfs::mw2::TxRecordState::Prepared,
        digest: tx.digest().unwrap(),
        parent_generation: tx.base_head_generation(),
        commit: None,
        updated_ns: NOW,
        pins: Vec::new(),
    };
    let bytes = weftfs::mw2::encode_tx_record(&record).unwrap();
    fs::write(path.join("txs").join(id), bytes).unwrap();

    let recovered = store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    assert_eq!(recovered.commit, first.commit);
    assert!(recovered.idempotent);
    assert_eq!(
        recovered.attempts, 0,
        "resolution must not publish a second commit"
    );
}

#[test]
fn gc_racing_a_prepared_transaction_retains_its_manifest() {
    let (_dir, path) = new_store();
    let mut store = open(&path);
    let plan = plan_file(&store, State::ROOT_ID, b"gc", b"pinned-content", b"seed-gc");
    let tx = stage(&store, "0000000000000000000000000000005a", &plan);

    // The manifests are durable, the transaction record is prepared, and no
    // commit names either yet.
    store.prepare_transaction(&tx).unwrap();
    let report = store.gc(true).unwrap();
    assert!(report.applied);

    // If GC had reclaimed the manifest, publication would name a missing blob.
    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    let verify = store.verify(true).unwrap();
    assert!(verify.ok, "{:?}", verify.problems);
    assert_eq!(
        store.read_object(plan.record.id).unwrap(),
        b"pinned-content"
    );
}

#[test]
fn an_authorization_change_that_wins_the_race_denies_publication() {
    let (_dir, path) = new_store();
    let mut a = open(&path);

    // The protected file is world-readable.
    let protected = plan_file(&a, State::ROOT_ID, b"protected", b"secret", b"seed-prot");
    let tx = stage(&a, "0000000000000000000000000000006a", &protected);
    a.execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();

    // B plans an unrelated create but requires that it may read `protected`.
    let mut b = open(&path);
    let unrelated = plan_file(&b, State::ROOT_ID, b"other", b"x", b"seed-other");
    let mut tx_b = stage(&b, "0000000000000000000000000000006b", &unrelated);
    tx_b.require_readable(protected.record.id, Principal::new(2, 2, Vec::new()));

    // A revokes world-read before B publishes.
    let mut rec = protected.record.clone();
    rec.mode = 0o600;
    let version = derive_version_id(rec.id, rec.content, rec.size, NOW + 9, None).unwrap();
    rec.version = version;
    rec.mtime_ns = NOW + 9;
    let mut tx_a = a
        .begin_transaction("0000000000000000000000000000006c")
        .unwrap();
    let state = a.current_state();
    tx_a.put_object(state, rec.clone()).unwrap();
    a.execute_transaction(&tx_a, &TransactionOptions::default())
        .unwrap();

    let err = b
        .execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized, "{err}");
    assert!(matches!(
        a.transaction_outcome("0000000000000000000000000000006b")
            .unwrap(),
        OutcomeLookup::Unknown | OutcomeLookup::Absent
    ));
}

#[test]
fn transaction_ids_are_not_reused_for_a_different_op_set() {
    let (_dir, path) = new_store();
    let mut store = open(&path);
    let id = "0000000000000000000000000000007a";
    let one = plan_file(&store, State::ROOT_ID, b"one", b"1", b"seed-7a");
    let two = plan_file(&store, State::ROOT_ID, b"two", b"2", b"seed-7b");
    let tx_one = stage(&store, id, &one);
    store
        .execute_transaction(&tx_one, &TransactionOptions::default())
        .unwrap();

    let tx_two = stage(&store, id, &two);
    let err = store
        .execute_transaction(&tx_two, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    assert!(err.message().contains("different op set"), "{err}");
}

#[test]
fn the_seeded_transaction_id_and_policy_are_inspectable() {
    let (_dir, path) = new_store();
    let store = open(&path);
    let bytes = fs::read(path.join("writers")).unwrap();
    assert_eq!(
        weftfs::mw2::decode_writers_mode(&bytes).unwrap(),
        WriterMode::Optimistic
    );
    assert!(matches!(
        store
            .transaction_outcome("0000000000000000000000000000008a")
            .unwrap(),
        OutcomeLookup::Absent
    ));
}
