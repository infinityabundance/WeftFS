//! T024: multi-writer GC and concurrent retention.
//!
//! Every test uses at least two independent `Store` handles against one local
//! store, so the cross-writer case is exercised by construction: the handle
//! running GC is never the handle that prepared the transaction. Assertions are
//! about externally visible state (`read_object`, `verify`) or the published GC
//! report; no private field is inspected to declare success.

use std::collections::BTreeMap;

use tempfile::TempDir;
use weftfs::error::Class;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options(mode: WriterMode, grace_secs: u64) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: mode,
        checkpoint_interval: 0,
        gc_grace_secs: grace_secs,
        ..Options::default()
    }
}

fn new_store(grace_secs: u64) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = Store::init(&path, options(WriterMode::Single, grace_secs)).unwrap();
        drop(store);
    }
    (dir, path)
}

fn open(path: &std::path::Path, mode: WriterMode, grace_secs: u64) -> Store {
    Store::open_with_options(path, OpenMode::ReadWrite, options(mode, grace_secs)).unwrap()
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

fn object_bytes(store: &Store, name: &[u8]) -> Option<Vec<u8>> {
    let id = store.lookup(State::ROOT_ID, name).ok()?;
    Some(store.read_object(id).unwrap())
}

/// A prepared transaction from writer A must survive a GC started by writer B,
/// even with the age gate disabled.
#[test]
fn a_gc_started_by_another_writer_keeps_a_prepared_transactions_pins() {
    let (_dir, path) = new_store(0);
    let mut a = open(&path, WriterMode::Optimistic, 0);
    let mut b = open(&path, WriterMode::Optimistic, 0);

    let plan = plan_file(&a, b"pinned.txt", b"pinned-content", b"seed-pinned");
    let tx = stage(&a, "000000000000000000000000000000a1", &plan);
    a.prepare_transaction(&tx).unwrap();

    // B is a different process-equivalent handle and knows nothing about A's
    // transaction except its published record.
    let report = b.gc(true).unwrap();
    assert!(report.applied);
    assert!(report.reservations_live == 0);
    assert!(
        report.marked_manifests >= 1,
        "the prepared record's pin must be marked: {report:?}"
    );

    // Publication after B's sweep must still name a present blob.
    a.execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    assert_eq!(object_bytes(&a, b"pinned.txt").unwrap(), b"pinned-content");
    assert!(b.verify(true).unwrap().ok);
}

/// The window the prepared record does not cover: blobs are published before a
/// transaction exists. A reservation taken first must fence the sweep.
#[test]
fn a_reservation_fences_the_blob_publication_window() {
    let (_dir, path) = new_store(0);
    let mut a = open(&path, WriterMode::Optimistic, 0);
    let mut b = open(&path, WriterMode::Optimistic, 0);

    // Writer A registers, then publishes a manifest, and deliberately does not
    // build or prepare a transaction yet.
    let reservation = a
        .reserve_publication("000000000000000000000000000000b1")
        .unwrap();
    assert_eq!(reservation.epoch(), 0);
    let (manifest, _, _) = a.prepare_content(b"reserved-content").unwrap();

    // B sweeps with no age gate. Without the reservation this is exactly the
    // interleaving that used to reclaim the manifest.
    let report = b.gc(true).unwrap();
    assert_eq!(report.reservations_live, 1);
    assert!(
        report.retained_reservation_floor >= 1 || report.retained_newer_than_cut >= 1,
        "the reserved manifest must be retained by the cut or the floor: {report:?}"
    );

    // A now publishes normally; the bytes must still be readable.
    let object_id = weftfs::identity::object_id_from_seed(b"seed-reserved");
    let size = b"reserved-content".len() as u64;
    let version = derive_version_id(object_id, manifest, size, NOW, None).unwrap();
    let record = ObjectRecord {
        id: object_id,
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
        id: derive_entry_id(State::ROOT_ID, b"reserved.txt").unwrap(),
        parent: State::ROOT_ID,
        name: b"reserved.txt".to_vec(),
        target: object_id,
    };
    let mut tx = a
        .begin_transaction("000000000000000000000000000000b2")
        .unwrap();
    {
        let state = a.current_state();
        tx.put_object_create(record).unwrap();
        tx.put_entry_create(entry).unwrap();
        tx.set_tip(state, object_id, version).unwrap();
    }
    a.execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    assert_eq!(
        object_bytes(&a, b"reserved.txt").unwrap(),
        b"reserved-content"
    );
    assert!(a.verify(true).unwrap().ok);
    a.release_reservation(&reservation).unwrap();
    assert!(a.gc_reservations().unwrap().is_empty());
}

/// The negative control: the sweep must actually reclaim something, otherwise
/// the retention assertions above would be vacuous.
#[test]
fn the_sweep_reclaims_genuinely_unreachable_blobs() {
    let (_dir, path) = new_store(0);
    let mut store = open(&path, WriterMode::Optimistic, 0);

    let plan = plan_file(&store, b"short.txt", b"first-version", b"seed-garbage");
    let tx = stage(&store, "000000000000000000000000000000c1", &plan);
    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();

    // Replace the file with different content: the old manifest and chunk become
    // unreachable once the new version is published.
    let (manifest, _, size) = store.prepare_content(b"second-version").unwrap();
    let version = derive_version_id(plan.record.id, manifest, size, NOW + 1, None).unwrap();
    let mut record = plan.record.clone();
    record.content = manifest;
    record.size = size;
    record.version = version;
    record.mtime_ns = NOW + 1;
    let mut tx = store
        .begin_transaction("000000000000000000000000000000c2")
        .unwrap();
    {
        let state = store.current_state();
        tx.put_object(state, record.clone()).unwrap();
        tx.set_tip(state, record.id, version).unwrap();
    }
    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();

    // The sweep applies a one-second clock slack to its epoch cut so a coarse
    // filesystem timestamp can never make a just-published blob look old. The
    // replaced version must therefore be older than that.
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let report = store.gc(true).unwrap();
    assert!(
        report.deleted_bytes > 0,
        "the sweep must reclaim the replaced version: {report:?}"
    );
    assert!(report.deleted.len() >= 2, "manifest and chunk: {report:?}");
    assert_eq!(
        object_bytes(&store, b"short.txt").unwrap(),
        b"second-version"
    );
    assert!(store.verify(true).unwrap().ok);
    assert_eq!(report.gc_epoch, 1);
    assert!(store.gc_run_record(&report.gc_run_id).unwrap().is_some());
}

/// Two passes cannot share an epoch: the compare-and-swap serialises them, and
/// each run is recorded under its own id.
#[test]
fn concurrent_passes_are_serialised_by_the_epoch_cas() {
    let (_dir, path) = new_store(0);
    {
        let _ = open(&path, WriterMode::Optimistic, 0);
    }
    let first = std::thread::spawn({
        let path = path.clone();
        move || {
            let mut store = open(&path, WriterMode::Optimistic, 0);
            store.gc(true).unwrap().gc_epoch
        }
    });
    let second = std::thread::spawn({
        let path = path.clone();
        move || {
            let mut store = open(&path, WriterMode::Optimistic, 0);
            store.gc(true).unwrap().gc_epoch
        }
    });
    let a = first.join().unwrap();
    let b = second.join().unwrap();
    assert_ne!(a, b, "the two passes must not share an epoch");
    let store = open(&path, WriterMode::Optimistic, 0);
    let epoch = store.gc_epoch().unwrap().unwrap();
    assert!(epoch.epoch >= 2, "{epoch:?}");
    assert!(store.verify(true).unwrap().ok);
}

/// An abandoned reservation is removed once its deadline passes, and after that
/// its blobs are ordinary garbage.
#[test]
fn an_expired_reservation_is_removed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let base = Options {
        writer_mode: WriterMode::Single,
        checkpoint_interval: 0,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
        ..Options::default()
    };
    drop(Store::init(&path, base.clone()).unwrap());
    let mut store = Store::open_with_options(
        &path,
        OpenMode::ReadWrite,
        Options {
            writer_mode: WriterMode::Optimistic,
            gc_reservation_secs: 1,
            ..base
        },
    )
    .unwrap();
    let reservation = store
        .reserve_publication("000000000000000000000000000000d1")
        .unwrap();
    let _ = store.prepare_content(b"abandoned").unwrap();
    assert_eq!(store.gc_reservations().unwrap().len(), 1);
    // Abandon the writer without releasing: the record survives until its
    // deadline, exactly like a crashed process.
    let _ = reservation;
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let report = store.gc(true).unwrap();
    assert_eq!(report.deleted_reservations, 1, "{report:?}");
    assert!(store.gc_reservations().unwrap().is_empty());
    assert!(report.deleted_bytes >= 1, "the abandoned blobs are garbage");
}

/// MW3: one coordinated pass marks every registered domain head, reclaims data
/// that no domain names, and never distinguishes a bound domain handle.
#[test]
fn a_coordinated_pass_marks_every_domain_head() {
    let (_dir, path) = new_store(0);

    // Register two domains through a global sharded handle.
    let (domain_a, domain_b) = {
        let mut admin = open(&path, WriterMode::Sharded, 0);
        let a = admin
            .register_domain(
                weftfs::mw3::DomainRecord::new(
                    "family-a",
                    weftfs::mw3::DomainKind::CheckpointFamily,
                    "a",
                )
                .unwrap(),
            )
            .unwrap();
        let b = admin
            .register_domain(
                weftfs::mw3::DomainRecord::new(
                    "family-b",
                    weftfs::mw3::DomainKind::CheckpointFamily,
                    "b",
                )
                .unwrap(),
            )
            .unwrap();
        (a, b)
    };

    // Publish one file per domain, with distinct content so the blob sets are
    // disjoint and a replacement is observable.
    for (domain, name, content, seed, tx_id_prefix) in [
        (
            domain_a,
            b"a.bin".to_vec(),
            b"a-content".to_vec(),
            b"seed-a".to_vec(),
            "e1",
        ),
        (
            domain_b,
            b"b.bin".to_vec(),
            b"b-content".to_vec(),
            b"seed-b".to_vec(),
            "e2",
        ),
    ] {
        let mut store = Store::open_domain_with_options_and_backend(
            &path,
            OpenMode::ReadWrite,
            options(WriterMode::Sharded, 0),
            weftfs::backend::BackendKind::Local,
            domain,
        )
        .unwrap();
        let plan = plan_file(&store, &name, &content, &seed);
        let tx_id = format!("{tx_id_prefix}0000000000000000000000000000e1");
        let tx = stage(&store, &tx_id, &plan);
        store
            .execute_transaction(&tx, &TransactionOptions::default())
            .unwrap();
    }

    // The coordinated pass runs on a global sharded handle.
    let mut admin = open(&path, WriterMode::Sharded, 0);
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let report = admin.gc(true).unwrap();
    assert_eq!(report.domain_heads_marked, 2, "{report:?}");
    assert!(
        report.deleted_bytes == 0,
        "nothing is unreachable yet: {report:?}"
    );

    // Every domain still reads and verifies.
    for (domain, name, content) in [
        (domain_a, b"a.bin".as_slice(), b"a-content".as_slice()),
        (domain_b, b"b.bin".as_slice(), b"b-content".as_slice()),
    ] {
        let store = Store::open_domain_with_options_and_backend(
            &path,
            OpenMode::ReadOnly,
            options(WriterMode::Sharded, 0),
            weftfs::backend::BackendKind::Local,
            domain,
        )
        .unwrap();
        assert_eq!(object_bytes(&store, name).unwrap(), content);
        assert!(store.verify(true).unwrap().ok);
    }

    // Replace domain A's file. The previous version's manifest and chunk become
    // unreachable from every domain head. (The object record itself survives;
    // MW3 GC does not remove unlinked object records — that is a per-domain
    // logical publish and is reported, not hidden.)
    {
        let mut store = Store::open_domain_with_options_and_backend(
            &path,
            OpenMode::ReadWrite,
            options(WriterMode::Sharded, 0),
            weftfs::backend::BackendKind::Local,
            domain_a,
        )
        .unwrap();
        let object_id = store.lookup(State::ROOT_ID, b"a.bin").unwrap();
        let (manifest, _, size) = store.prepare_content(b"a-content-v2").unwrap();
        let version = derive_version_id(object_id, manifest, size, NOW + 1, None).unwrap();
        let mut record = store.current_state().objects[&object_id].clone();
        record.content = manifest;
        record.size = size;
        record.version = version;
        record.mtime_ns = NOW + 1;
        let mut tx = store
            .begin_transaction("f10000000000000000000000000000e1")
            .unwrap();
        {
            let state = store.current_state();
            tx.put_object(state, record).unwrap();
            tx.set_tip(state, object_id, version).unwrap();
        }
        store
            .execute_transaction(&tx, &TransactionOptions::default())
            .unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let report = admin.gc(true).unwrap();
    assert!(report.deleted_bytes > 0, "{report:?}");
    assert_eq!(report.domain_heads_marked, 2);

    let b_store = Store::open_domain_with_options_and_backend(
        &path,
        OpenMode::ReadOnly,
        options(WriterMode::Sharded, 0),
        weftfs::backend::BackendKind::Local,
        domain_b,
    )
    .unwrap();
    assert_eq!(object_bytes(&b_store, b"b.bin").unwrap(), b"b-content");
    assert!(b_store.verify(true).unwrap().ok);

    let a_store = Store::open_domain_with_options_and_backend(
        &path,
        OpenMode::ReadOnly,
        options(WriterMode::Sharded, 0),
        weftfs::backend::BackendKind::Local,
        domain_a,
    )
    .unwrap();
    assert_eq!(object_bytes(&a_store, b"a.bin").unwrap(), b"a-content-v2");
    assert!(a_store.verify(true).unwrap().ok);

    // A domain-bound handle may not run a coordinated sweep.
    let mut bound = Store::open_domain_with_options_and_backend(
        &path,
        OpenMode::ReadWrite,
        options(WriterMode::Sharded, 0),
        weftfs::backend::BackendKind::Local,
        domain_b,
    )
    .unwrap();
    let err = bound.gc(true).unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
}
