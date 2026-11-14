//! T028: explicit, safe writer-mode migration.
//!
//! The property under test is that **no dual active authority** exists: the mode
//! is one record, changed by one compare-and-swap, and every writable open is
//! checked against it. Downgrades are refused; a live prepared transaction
//! blocks the transition; a crash leaves exactly the old or the new mode.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use tempfile::TempDir;
use weftfs::error::Class;
use weftfs::mw2::{TransactionOptions, WriterMode};
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

fn new_store() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    drop(Store::init(&path, options(WriterMode::Single)).unwrap());
    (dir, path)
}

fn open(path: &std::path::Path, mode: WriterMode) -> weftfs::Result<Store> {
    Store::open_with_options(path, OpenMode::ReadWrite, options(mode))
}

fn plan(store: &Store, name: &[u8], seed: &[u8]) -> (ObjectRecord, EntryRecord) {
    let (manifest, _, size) = store.prepare_content(b"mode-content").unwrap();
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
    (record, entry)
}

#[test]
fn single_to_optimistic_is_one_cas_and_idempotent() {
    let (_dir, path) = new_store();
    {
        let mut store = open(&path, WriterMode::Single).unwrap();
        assert_eq!(store.recorded_writer_mode().unwrap(), None);
        let applied = store
            .migrate_writer_mode(WriterMode::Single, WriterMode::Optimistic, Duration::ZERO)
            .unwrap();
        assert_eq!(applied, WriterMode::Optimistic);
        assert_eq!(
            store.recorded_writer_mode().unwrap(),
            Some(WriterMode::Optimistic)
        );
        // Repeating the same transition is a no-op, not an error.
        store
            .migrate_writer_mode(
                WriterMode::Optimistic,
                WriterMode::Optimistic,
                Duration::ZERO,
            )
            .unwrap();
        // A single-writer open is refused once the record says optimistic.
        let err = match open(&path, WriterMode::Single) {
            Ok(_) => panic!("a single-writer open must be refused"),
            Err(err) => err,
        };
        assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    }
    // A read-only open is never refused, in any mode.
    Store::open_with_options(&path, OpenMode::ReadOnly, options(WriterMode::Single)).unwrap();
}

#[test]
fn every_downgrade_is_refused() {
    let (_dir, path) = new_store();
    let mut store = open(&path, WriterMode::Single).unwrap();
    let err = store
        .migrate_writer_mode(WriterMode::Single, WriterMode::Sharded, Duration::ZERO)
        .unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    assert!(err
        .to_string()
        .contains("promoted into an MW3 conflict domain"));

    store
        .migrate_writer_mode(WriterMode::Single, WriterMode::Optimistic, Duration::ZERO)
        .unwrap();
    // optimistic -> single is a downgrade and is refused.
    let err = store
        .migrate_writer_mode(WriterMode::Optimistic, WriterMode::Single, Duration::ZERO)
        .unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    assert!(err.to_string().contains("downgrading"));
    // optimistic -> sharded is the one supported upward transition.
    store
        .migrate_writer_mode(WriterMode::Optimistic, WriterMode::Sharded, Duration::ZERO)
        .unwrap();
    drop(store);
    // From sharded, both reverse transitions are refused.
    let mut admin = open(&path, WriterMode::Sharded).unwrap();
    for to in [WriterMode::Optimistic, WriterMode::Single] {
        let err = admin
            .migrate_writer_mode(WriterMode::Sharded, to, Duration::ZERO)
            .unwrap_err();
        assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    }
}

#[test]
fn a_live_prepared_transaction_blocks_the_transition() {
    let (_dir, path) = new_store();
    let mut a = open(&path, WriterMode::Single).unwrap();
    a.migrate_writer_mode(WriterMode::Single, WriterMode::Optimistic, Duration::ZERO)
        .unwrap();
    let mut store = open(&path, WriterMode::Optimistic).unwrap();
    let (record, entry) = plan(&store, b"blocking.bin", b"seed-blocking");
    let mut tx = store
        .begin_transaction("000000000000000000000000000000a9")
        .unwrap();
    {
        let state = store.current_state();
        tx.put_object_create(record.clone()).unwrap();
        tx.put_entry_create(entry).unwrap();
        tx.set_tip(state, record.id, record.version).unwrap();
    }
    store.prepare_transaction(&tx).unwrap();

    let err = store
        .migrate_writer_mode(WriterMode::Optimistic, WriterMode::Sharded, Duration::ZERO)
        .unwrap_err();
    assert_eq!(err.class(), Class::Busy, "{err}");
    assert!(err.to_string().contains("000000000000000000000000000000a9"));
    assert_eq!(
        store.recorded_writer_mode().unwrap(),
        Some(WriterMode::Optimistic),
        "a refused migration must not change the record"
    );

    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    let applied = store
        .migrate_writer_mode(WriterMode::Optimistic, WriterMode::Sharded, Duration::ZERO)
        .unwrap();
    assert_eq!(applied, WriterMode::Sharded);
    assert_eq!(
        store.recorded_writer_mode().unwrap(),
        Some(WriterMode::Sharded)
    );

    // A single or optimistic writable open is now refused; a sharded domain
    // handle works after registration.
    for mode in [WriterMode::Single, WriterMode::Optimistic] {
        let err = match open(&path, mode) {
            Ok(_) => panic!("a {mode:?} writable open must be refused"),
            Err(err) => err,
        };
        assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    }
    drop(store);
    let mut admin = open(&path, WriterMode::Sharded).unwrap();
    let domain = admin
        .register_domain(
            weftfs::mw3::DomainRecord::new(
                "after-migration",
                weftfs::mw3::DomainKind::Namespace,
                "",
            )
            .unwrap(),
        )
        .unwrap();
    drop(admin);
    let mut domain_store = Store::open_domain_with_options_and_backend(
        &path,
        OpenMode::ReadWrite,
        options(WriterMode::Sharded),
        weftfs::backend::BackendKind::Local,
        domain,
    )
    .unwrap();
    let (record, entry) = plan(&domain_store, b"after.bin", b"seed-after");
    let mut tx = domain_store
        .begin_transaction("000000000000000000000000000000aa")
        .unwrap();
    {
        tx.put_object_create(record.clone()).unwrap();
        tx.put_entry_create(entry).unwrap();
        tx.set_tip(domain_store.current_state(), record.id, record.version)
            .unwrap();
    }
    domain_store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    assert!(domain_store.verify(true).unwrap().ok);
}

#[test]
fn a_migration_naming_the_wrong_source_mode_is_refused() {
    let (_dir, path) = new_store();
    let mut store = open(&path, WriterMode::Single).unwrap();
    let err = store
        .migrate_writer_mode(WriterMode::Optimistic, WriterMode::Sharded, Duration::ZERO)
        .unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedVersion, "{err}");
    assert!(err.to_string().contains("not `optimistic`"));
    assert!(!path.join("writers").exists(), "no record may be created");
}

#[test]
fn the_record_is_not_a_cache_of_the_head() {
    // A migration changes only the writers record: the head, the commit chain,
    // and every blob are untouched, so a reader opened before and after the
    // migration sees the same namespace.
    let (_dir, path) = new_store();
    let mut store = open(&path, WriterMode::Single).unwrap();
    store
        .create_file_seeded(
            State::ROOT_ID,
            b"stable.bin",
            b"mode-content",
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"seed-stable",
        )
        .unwrap();
    let head_before = fs::read(path.join("superblock")).unwrap();
    store
        .migrate_writer_mode(WriterMode::Single, WriterMode::Optimistic, Duration::ZERO)
        .unwrap();
    assert_eq!(
        head_before,
        fs::read(path.join("superblock")).unwrap(),
        "the migration must not publish or rewrite the head"
    );
    let reader =
        Store::open_with_options(&path, OpenMode::ReadOnly, options(WriterMode::Single)).unwrap();
    let id = reader.lookup(State::ROOT_ID, b"stable.bin").unwrap();
    assert_eq!(reader.read_object(id).unwrap(), b"mode-content");
    assert!(reader.verify(true).unwrap().ok);
}
