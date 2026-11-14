//! MW3 conflict-domain tests (subphase T020).
//!
//! Every assertion is about externally visible state, a typed error, or the
//! per-domain head image on disk. Nothing here inspects a private field to
//! declare success. The normative contract is `spec/WEFT-MULTIWRITER.md` §15.

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};

use tempfile::TempDir;
use weftfs::error::Class;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::mw3::{DomainKind, DomainRecord};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{BackendKind, OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options(mode: WriterMode) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn new_store() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = Store::init(&path, options(WriterMode::Single)).unwrap();
        drop(store);
    }
    (dir, path)
}

fn admin(path: &std::path::Path) -> Store {
    Store::open_with_options_and_backend(
        path,
        OpenMode::ReadWrite,
        options(WriterMode::Sharded),
        BackendKind::Local,
    )
    .unwrap()
}

fn open_domain(path: &std::path::Path, domain: weftfs::Hash32) -> Store {
    Store::open_domain_with_options_and_backend(
        path,
        OpenMode::ReadWrite,
        options(WriterMode::Sharded),
        BackendKind::Local,
        domain,
    )
    .unwrap()
}

fn open_domain_read(path: &std::path::Path, domain: weftfs::Hash32) -> Store {
    Store::open_domain_with_options_and_backend(
        path,
        OpenMode::ReadOnly,
        options(WriterMode::Sharded),
        BackendKind::Local,
        domain,
    )
    .unwrap()
}

fn register(path: &std::path::Path, name: &str, kind: DomainKind) -> weftfs::Hash32 {
    let mut admin = admin(path);
    let record = DomainRecord::new(name, kind, "").unwrap();
    let id = record.id().unwrap();
    assert_eq!(admin.register_domain(record).unwrap(), id);
    id
}

struct FilePlan {
    record: ObjectRecord,
    entry: EntryRecord,
}

fn plan_file(store: &Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> FilePlan {
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
        id: derive_entry_id(State::ROOT_ID, name).unwrap(),
        parent: State::ROOT_ID,
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

fn commit(store: &mut Store, id: &str, plan: &FilePlan) -> weftfs::mw2::TransactionOutcome {
    let tx = stage(store, id, plan);
    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap()
}

#[test]
fn registration_is_idempotent_and_discovery_is_canonical() {
    let (_dir, path) = new_store();
    let id = register(&path, "qwen2.5", DomainKind::ModelBranch);
    // Registering the same record again is a no-op and does not bump the epoch.
    let mut admin = admin(&path);
    let before = admin.discover_domains().unwrap();
    assert_eq!(before.epoch, 1);
    admin
        .register_domain(DomainRecord::new("qwen2.5", DomainKind::ModelBranch, "").unwrap())
        .unwrap();
    let after = admin.discover_domains().unwrap();
    assert_eq!(after.epoch, 1);
    assert_eq!(after.domains.len(), 1);
    assert_eq!(after.domains[0].id, id);
    // A different record with the same display name is a different domain.
    let other = register(&path, "qwen2.5", DomainKind::CheckpointFamily);
    assert_ne!(other, id);
    // The discovery record decodes canonically and carries both.
    let index = admin.discover_domains().unwrap();
    assert_eq!(index.domains.len(), 2);
    assert!(index.find(&id).is_some() && index.find(&other).is_some());
}

#[test]
fn independent_heads_do_not_conflict() {
    let (_dir, path) = new_store();
    let train = register(&path, "train", DomainKind::CheckpointFamily);
    let eval = register(&path, "eval", DomainKind::CheckpointFamily);

    let mut a = open_domain(&path, train);
    let mut b = open_domain(&path, eval);
    let plan_a = plan_file(&a, b"same-name.bin", b"train-bytes", b"seed-train");
    let plan_b = plan_file(&b, b"same-name.bin", b"eval-bytes", b"seed-eval");
    let out_a = commit(&mut a, "0000000000000000000000000000000a", &plan_a);
    let out_b = commit(&mut b, "0000000000000000000000000000000b", &plan_b);
    assert_eq!(out_a.generation, 2, "genesis is generation 1");
    assert_eq!(out_b.generation, 2, "each domain has its own generation");
    // The same generation number in two domains is not a collision.
    assert_ne!(out_a.commit, out_b.commit);

    // Both domain heads are visible and the read view resolves both.
    let view = a.capture_read_view(&[train, eval]).unwrap();
    assert_eq!(view.pairs.len(), 2);
    for pair in &view.pairs {
        assert_eq!(pair.generation, 2);
    }
    // Each domain verifies exactly, with its own content.
    for (store, expected) in [(&a, &b"train-bytes"[..]), (&b, &b"eval-bytes"[..])] {
        let report = store.verify(true).unwrap();
        assert!(report.ok, "{:?}", report.problems);
        let id = store.lookup(State::ROOT_ID, b"same-name.bin").unwrap();
        assert_eq!(store.read_object(id).unwrap(), expected);
    }
}

#[test]
fn same_domain_contention_has_exactly_one_winner() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    let mut a = open_domain(&path, domain);
    let mut b = open_domain(&path, domain);
    let plan_a = plan_file(&a, b"hot.bin", b"a", b"seed-a");
    let plan_b = plan_file(&b, b"hot.bin", b"b", b"seed-b");
    commit(&mut a, "0000000000000000000000000000000a", &plan_a);
    // B's observed head is now stale; its create precondition sees the winner.
    let tx = stage(&b, "0000000000000000000000000000000b", &plan_b);
    let err = b
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    assert!(err.message().contains("NameExists"), "{err}");
    // Exactly one winner, and the winner's bytes are what a reader sees.
    let reader = open_domain_read(&path, domain);
    let id = reader.lookup(State::ROOT_ID, b"hot.bin").unwrap();
    assert_eq!(reader.read_object(id).unwrap(), b"a");
    assert_eq!(reader.generation(), 2);
}

#[test]
fn concurrent_same_domain_writers_never_both_win() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (index, seed) in [(0u32, "seed-0"), (1u32, "seed-1")] {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut store = open_domain(&path, domain);
            let plan = plan_file(&store, b"race.bin", seed.as_bytes(), seed.as_bytes());
            let tx = stage(&store, &format!("{index:032x}"), &plan);
            barrier.wait();
            store
                .execute_transaction(&tx, &TransactionOptions::default())
                .map(|out| out.generation)
                .map_err(|err| err.class())
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "{results:?}");
    let conflicts = results
        .iter()
        .filter(|r| matches!(r, Err(Class::Conflict)))
        .count();
    assert_eq!(conflicts, 1, "{results:?}");
}

#[test]
fn mw1_and_mw2_handles_refuse_a_sharded_store() {
    let (_dir, path) = new_store();
    register(&path, "train", DomainKind::CheckpointFamily);
    for mode in [WriterMode::Single, WriterMode::Optimistic] {
        let err = Store::open_with_options(&path, OpenMode::ReadWrite, options(mode))
            .err()
            .expect("a non-sharded writable open must refuse");
        assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
        // A read-only MW1 reader must refuse too, rather than report an empty
        // namespace.
        let err = Store::open_with_options(&path, OpenMode::ReadOnly, options(mode))
            .err()
            .expect("a non-sharded read-only open must refuse");
        assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    }
    // A sharded handle still opens and reads.
    let domain = register(&path, "eval", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, domain);
    let plan = plan_file(&store, b"x.bin", b"x", b"seed-x");
    commit(&mut store, "0000000000000000000000000000000c", &plan);
}

#[test]
fn cross_domain_requests_are_refused() {
    let (_dir, path) = new_store();
    let a = register(&path, "train", DomainKind::CheckpointFamily);
    let b = register(&path, "eval", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, a);
    let plan = plan_file(&store, b"x.bin", b"x", b"seed-x");
    let tx = stage(&store, "0000000000000000000000000000000d", &plan);

    let err = store
        .execute_transaction_across(&[a, b], &tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::CrossDomain, "{err}");
    let err = store
        .execute_transaction_in(b, &tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::CrossDomain, "{err}");
    let err = store
        .execute_transaction_across(&[], &tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    // A single named domain that matches the handle is accepted.
    let outcome = store
        .execute_transaction_across(&[a], &tx, &TransactionOptions::default())
        .unwrap();
    assert_eq!(outcome.generation, 2);

    // A global MW1 handle cannot publish into a domain either.
    let mut global = match Store::open_with_options_and_backend(
        &path,
        OpenMode::ReadWrite,
        options(WriterMode::Sharded),
        BackendKind::Local,
    ) {
        Ok(store) => store,
        Err(err) => panic!("sharded admin open failed: {err}"),
    };
    let plan = plan_file(&global, b"y.bin", b"y", b"seed-y");
    let tx = {
        // Build against the (empty) global state; the call must refuse before
        // any precondition matters.
        let mut tx = global
            .begin_transaction("0000000000000000000000000000000e")
            .unwrap();
        tx.put_entry_create(plan.entry.clone()).unwrap();
        tx
    };
    let err = global
        .execute_transaction_in(a, &tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::CrossDomain, "{err}");
}

#[test]
fn gc_coordinates_domains_and_refuses_a_domain_bound_handle() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, domain);
    // A domain-bound handle has no global head to record an epoch or an
    // unlinked-record commit against, so it still refuses.
    let err = store.gc(false).unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");

    // An unbound sharded handle now runs the coordinated T024 pass: every
    // domain head is a root, and the unlinked-object-record removal it cannot
    // publish is reported in `problems` rather than silently skipped.
    let mut admin = admin(&path);
    let report = admin.gc(true).unwrap();
    assert_eq!(report.domain_heads_marked, 1, "{report:?}");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("unlinked-object-record removal is skipped")),
        "a sharded apply must report the skipped orphan removal: {:?}",
        report.problems
    );
    assert!(admin.verify(true).unwrap().ok);
}

#[test]
fn reopen_replays_the_domain_chain_exactly() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, domain);
    for index in 0..3u32 {
        let name = format!("f{index}.bin");
        let content = format!("content-{index}");
        let seed = format!("seed-{index}");
        let plan = plan_file(&store, name.as_bytes(), content.as_bytes(), seed.as_bytes());
        let outcome = commit(&mut store, &format!("{index:032x}"), &plan);
        assert_eq!(outcome.generation, u64::from(index) + 2);
    }
    let head = store.head();
    drop(store);

    let reopened = open_domain_read(&path, domain);
    assert_eq!(reopened.generation(), 4);
    assert_eq!(reopened.head(), head);
    for index in 0..3u32 {
        let name = format!("f{index}.bin");
        let content = format!("content-{index}");
        let id = reopened.lookup(State::ROOT_ID, name.as_bytes()).unwrap();
        assert_eq!(reopened.read_object(id).unwrap(), content.as_bytes());
    }
    let report = reopened.verify(true).unwrap();
    assert!(report.ok, "{:?}", report.problems);
    assert_eq!(report.checked_commits, 4);
}

#[test]
fn domain_verify_detects_a_corrupt_chunk() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, domain);
    let plan = plan_file(&store, b"data.bin", b"payload", b"seed-data");
    commit(&mut store, "0000000000000000000000000000000f", &plan);
    assert!(store.verify(true).unwrap().ok);
    drop(store);

    // Corrupt the only chunk blob behind the object. The head is untouched, so
    // the chain still replays and the failure must be attributed to the blob.
    let objects = path.join("objects");
    let mut corrupted = false;
    for shard in std::fs::read_dir(&objects).unwrap() {
        let shard = shard.unwrap();
        if !shard.file_type().unwrap().is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            std::fs::write(entry.path(), b"corrupted").unwrap();
            corrupted = true;
        }
    }
    assert!(corrupted, "the content chunk was written");
    let store = open_domain_read(&path, domain);
    let report = store.verify(true).unwrap();
    assert!(!report.ok, "corrupt chunk must fail verification");
    assert!(!report.problems.is_empty());
}

#[test]
fn discovery_maps_each_domain_to_its_live_head() {
    let (_dir, path) = new_store();
    let train = register(&path, "train", DomainKind::CheckpointFamily);
    let eval = register(&path, "eval", DomainKind::CheckpointFamily);
    {
        let mut writer = open_domain(&path, train);
        let plan = plan_file(&writer, b"a.bin", b"a", b"seed-a");
        commit(&mut writer, "00000000000000000000000000000011", &plan);
    }
    let store = open_domain_read(&path, train);
    let heads = store.discover_heads().unwrap();
    assert_eq!(heads.len(), 2);
    for (entry, head) in &heads {
        // The record names the head object; the value is read from that object.
        assert_eq!(
            entry.head(),
            format!("domains/{}/superblock", entry.id.to_hex())
        );
        if entry.id == train {
            let (generation, commit) = head.expect("train has a head");
            assert_eq!(generation, 2);
            let train_view = open_domain_read(&path, train);
            assert_eq!(commit, train_view.head());
            assert_eq!(generation, train_view.generation());
        } else {
            assert_eq!(entry.id, eval);
            // Registration publishes a genesis commit, so both have a head.
            assert_eq!(head.expect("eval has a genesis head").0, 1);
        }
    }
}

#[test]
fn domain_links_record_lineage_and_never_merge_data() {
    let (_dir, path) = new_store();
    let predecessor = register(&path, "train", DomainKind::CheckpointFamily);
    let successor = register(&path, "train-next", DomainKind::CheckpointFamily);
    let mut store = open_domain(&path, successor);
    let link = weftfs::mw3::DomainLink::new(
        successor,
        vec![predecessor],
        vec![2],
        weftfs::mw3::DomainLinkKind::Split,
        None,
    )
    .unwrap();
    let id = store.write_domain_link(&link).unwrap();
    // Idempotent for the identical record.
    assert_eq!(store.write_domain_link(&link).unwrap(), id);
    // A successor has exactly one link: a different record is a typed conflict.
    let other = weftfs::mw3::DomainLink::new(
        successor,
        vec![predecessor],
        vec![3],
        weftfs::mw3::DomainLinkKind::Split,
        None,
    )
    .unwrap();
    let err = store.write_domain_link(&other).unwrap_err();
    assert_eq!(err.class(), Class::Conflict, "{err}");
    // The record reads back canonically and the successor's data is untouched.
    let stored = store.domain_link(successor).unwrap().expect("link");
    assert_eq!(stored, link);
    assert!(store.lookup(State::ROOT_ID, b"a.bin").is_err());
    // A split names one predecessor; a merge names two or more.
    assert!(weftfs::mw3::DomainLink::new(
        successor,
        vec![predecessor],
        vec![1],
        weftfs::mw3::DomainLinkKind::Merge,
        None,
    )
    .is_err());
    let third = register(&path, "train-third", DomainKind::CheckpointFamily);
    let merged = weftfs::mw3::DomainLink::new(
        successor,
        vec![predecessor.min(third), predecessor.max(third)],
        vec![1, 1],
        weftfs::mw3::DomainLinkKind::Merge,
        None,
    )
    .unwrap();
    assert!(merged.predecessors.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_link_requires_a_registered_successor() {
    let (_dir, path) = new_store();
    let mut admin = admin(&path);
    let missing = weftfs::Hash32::from_bytes([9u8; 32]);
    let link = weftfs::mw3::DomainLink::new(
        missing,
        vec![weftfs::Hash32::from_bytes([8u8; 32])],
        vec![1],
        weftfs::mw3::DomainLinkKind::Supersede,
        None,
    )
    .unwrap();
    let err = admin.write_domain_link(&link).unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
}

#[test]
fn read_view_names_a_missing_domain() {
    let (_dir, path) = new_store();
    let domain = register(&path, "train", DomainKind::CheckpointFamily);
    {
        let mut writer = open_domain(&path, domain);
        let plan = plan_file(&writer, b"x.bin", b"x", b"seed-x");
        commit(&mut writer, "00000000000000000000000000000010", &plan);
    }
    let store = open_domain_read(&path, domain);
    let missing = weftfs::Hash32::from_bytes([7u8; 32]);
    let err = store.capture_read_view(&[domain, missing]).unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
    assert!(err.message().contains(&missing.to_hex()), "{err}");
    // A view pin is a durable blob with the same root.
    let writer = open_domain(&path, domain);
    let view = writer.capture_read_view(&[domain]).unwrap();
    let pin = writer.write_view_pin(&view, "tester", NOW).unwrap();
    assert_ne!(pin, weftfs::Hash32::ZERO);
}
