//! Local/object backend writer fencing (subphase R17).
//!
//! A POSIX store's writer lease is its `flock` on the `LOCK` file. That has no
//! expiry, so the S3 lease/deadline machinery does not apply; what *does*
//! apply is the fencing question the reviewer asked for: can a writer that no
//! longer owns the namespace still publish?
//!
//! It must not. Before every publication the store proves that the `LOCK` name
//! still resolves to the exact inode this handle locked. Replacing or removing
//! the file hands the namespace to someone else, and the old handle then fails
//! `Busy` before the commit record or the head is written. These tests prove
//! that for `LocalBackend` and `ObjectBackend`, and prove that the published
//! state is untouched afterwards.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;

use weftfs::backend::BackendKind;
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn attrs() -> BTreeMap<String, weftcodec::wcb::Value> {
    BTreeMap::new()
}

fn lock_path(kind: &BackendKind) -> std::path::PathBuf {
    match kind {
        BackendKind::Local => std::path::PathBuf::from("LOCK"),
        BackendKind::Object | BackendKind::Posix { .. } => {
            std::path::PathBuf::from("objects/00/LOCK")
        }
        other => panic!("unexpected single-backend kind {other:?}"),
    }
}

/// Replace the lock file with a brand new inode, simulating a namespace that
/// was handed to another writer (an operator removing a stale lock, or a
/// different process recreating it).
fn replace_lock(root: &std::path::Path, relative: &std::path::Path) {
    let path = root.join(relative);
    fs::remove_file(&path).expect("remove LOCK");
    fs::write(&path, b"replacement\n").expect("create a new LOCK inode");
}

fn exercise(kind: BackendKind) {
    let dir = tempdir().unwrap();
    let mut store = Store::init_with_backend(dir.path(), opts(), kind.clone()).expect("init");
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"f",
            b"before-fencing",
            0o644,
            0,
            0,
            attrs(),
            b"fence-1",
        )
        .unwrap();
    let generation = store.generation();
    let head = store.snapshot_root();

    // A second live writer is still refused with Busy.
    let err = match Store::open_with_backend(dir.path(), OpenMode::ReadWrite, kind.clone()) {
        Ok(_) => panic!("a second live writer must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");

    // Hand the LOCK name to a new inode while this handle still holds the old
    // one. The handle no longer owns the namespace.
    replace_lock(dir.path(), &lock_path(&kind));

    let err = store.write_file(id, 0, b"after-fencing").unwrap_err();
    assert_eq!(
        err.class_name(),
        "Busy",
        "a publication under a lock this handle does not hold must fail closed: {err}"
    );
    assert!(
        err.message().contains("LOCK"),
        "the refusal must name the lock: {err}"
    );
    // Nothing was published: generation, head, and in-memory state are intact.
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot_root(), head);
    assert_eq!(store.read_object(id).unwrap(), b"before-fencing");

    // A fresh writer takes the *new* lock and observes the old whole state.
    let mut reopened =
        Store::open_with_backend(dir.path(), OpenMode::ReadWrite, kind.clone()).expect("reopen");
    assert_eq!(reopened.generation(), generation);
    assert_eq!(reopened.snapshot_root(), head);
    assert_eq!(reopened.read_object(id).unwrap(), b"before-fencing");
    // And it can publish normally.
    let version = reopened.write_file(id, 0, b"after-reopen").unwrap();
    assert_eq!(reopened.generation(), generation + 1);
    assert_ne!(reopened.snapshot_root(), head);
    assert_eq!(reopened.read_object(id).unwrap(), b"after-reopenng");
    assert_eq!(
        reopened.current_state().objects.get(&id).map(|o| o.version),
        Some(version)
    );
    assert!(reopened.verify(true).unwrap().ok);
    drop(store);
}

#[test]
fn local_writer_refuses_to_publish_under_a_replaced_lock() {
    exercise(BackendKind::Local);
}

#[test]
fn object_writer_refuses_to_publish_under_a_replaced_lock() {
    exercise(BackendKind::Object);
}

#[test]
fn local_writer_refuses_to_publish_after_its_lock_file_is_removed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(root, b"f", b"kept", 0o644, 0, 0, attrs(), b"fence-2")
        .unwrap();
    let generation = store.generation();
    let head = store.snapshot_root();

    fs::remove_file(dir.path().join("LOCK")).unwrap();
    let err = store.write_file(id, 0, b"lost").unwrap_err();
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot_root(), head);
    assert_eq!(store.read_object(id).unwrap(), b"kept");
    drop(store);
}

#[test]
fn local_writer_lock_still_excludes_a_second_handle() {
    let dir = tempdir().unwrap();
    let first = Store::init(dir.path(), opts()).unwrap();
    let err = match Store::open(dir.path(), OpenMode::ReadWrite) {
        Ok(_) => panic!("second writer must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    drop(first);
    let second = Store::open(dir.path(), OpenMode::ReadWrite).expect("reopen after release");
    assert!(second.verify(true).unwrap().ok);
}
