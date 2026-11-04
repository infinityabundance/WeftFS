//! Backend abstraction court.
//!
//! The store core is run twice over the same seeded operation script: once on
//! the normative `LocalBackend` and once on the rename-free `ObjectBackend`.
//! The logical state, commit records, commit hashes, manifests, and chunk
//! bytes must be identical. Capability refusals and a process-fault crash
//! proxy over the object backend are also exercised.
//!
//! This suite is a single-host, synthetic-fixture court. It makes no physical
//! power-loss, device-cache, fleet, or production claim.

use std::collections::BTreeMap;
use std::process::Command;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Capabilities, Key, KeyKind};
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

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

fn seed(tag: u8) -> Vec<u8> {
    vec![tag; 32]
}

fn expect_err<T>(result: weftfs::Result<T>, what: &str) -> weftfs::Error {
    match result {
        Ok(_) => panic!("{what}: expected an error"),
        Err(err) => err,
    }
}

fn attrs() -> BTreeMap<String, weftcodec::wcb::Value> {
    BTreeMap::new()
}

/// A compact replay of the extension patterns: directories, seeded file
/// creation, COW range write, truncate, rename, rmdir, hard link, xattrs,
/// relations, and a snapshot.
fn build_fixture(path: &std::path::Path, kind: BackendKind) -> Store {
    let mut store = Store::init_with_backend(path, opts(), kind).unwrap();
    let root = store.resolve_path("/").unwrap();

    let dir = store
        .mkdir_seeded(root, b"dir", 0o755, 0, 0, &seed(1))
        .unwrap();
    let a = store
        .create_file_seeded(
            root,
            b"a.txt",
            b"hello world, weft",
            0o644,
            0,
            0,
            attrs(),
            &seed(2),
        )
        .unwrap();
    // Range write: one chunk changes, the rest are reused verbatim.
    store.write_file(a, 6, b"WORLD").unwrap();
    // Truncate.
    store.truncate(a, 11).unwrap();
    // Create, rename, then remove an empty directory.
    let b = store
        .mkdir_seeded(root, b"b", 0o755, 0, 0, &seed(3))
        .unwrap();
    store
        .create_file_seeded(b, b"inner", b"inner", 0o644, 0, 0, attrs(), &seed(4))
        .unwrap();
    store.rename(root, b"b", root, b"c").unwrap();
    let empty = store
        .mkdir_seeded(root, b"empty", 0o755, 0, 0, &seed(5))
        .unwrap();
    let _ = empty;
    store.rmdir(root, b"empty").unwrap();
    // Hard link sharing one object.
    let link_target = store
        .create_file_seeded(root, b"linked", b"shared", 0o644, 0, 0, attrs(), &seed(6))
        .unwrap();
    store.link(link_target, dir, b"hard").unwrap();
    // Extended attributes.
    store.set_xattr(a, "user.note", b"backend court").unwrap();
    // Typed relation.
    store
        .relation_add(
            root,
            "tag",
            RelationTarget::Value(weftcodec::wcb::Value::text("fixture")),
            None,
            attrs(),
        )
        .unwrap();
    // Snapshot before the final write, then another write.
    store.snapshot_create("s1").unwrap();
    store.write_file(a, 0, b"HELLO").unwrap();
    store
}

fn assert_same_logical_state(local: &Store, object: &Store) {
    assert_eq!(local.generation(), object.generation());
    assert_eq!(local.snapshot_root(), object.snapshot_root());
    let a = local.current_state();
    let b = object.current_state();
    assert_eq!(a.generation, b.generation);
    assert_eq!(a.objects, b.objects);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.relations, b.relations);
    assert_eq!(a.snapshots, b.snapshots);
    assert_eq!(a.tips, b.tips);
}

fn backend_keys(kind: &BackendKind, root: &std::path::Path, key_kind: KeyKind) -> Vec<Key> {
    let backend = kind.open(root);
    let mut keys = backend
        .list_prefix(&Key::prefix(key_kind))
        .unwrap_or_else(|err| panic!("list {:?}: {err}", key_kind));
    keys.sort();
    keys
}

/// Compare every committed artifact by logical key and bytes.
fn assert_same_artifacts(
    local: &Store,
    object: &Store,
    local_kind: &BackendKind,
    object_kind: &BackendKind,
    local_root: &std::path::Path,
    object_root: &std::path::Path,
) {
    for kind in [
        KeyKind::Commit,
        KeyKind::Object,
        KeyKind::Manifest,
        KeyKind::Index,
    ] {
        let local_keys = backend_keys(local_kind, local_root, kind);
        let object_keys = backend_keys(object_kind, object_root, kind);
        assert_eq!(local_keys, object_keys, "logical {kind:?} key sets differ");
        for key in local_keys {
            let a = local
                .backend_read_key(&key)
                .unwrap_or_else(|err| panic!("local read {}: {err}", key.display_path()));
            let b = object
                .backend_read_key(&key)
                .unwrap_or_else(|err| panic!("object read {}: {err}", key.display_path()));
            assert_eq!(
                a,
                b,
                "{kind:?} artifact {} differs between backends",
                key.display_path()
            );
        }
    }
}

#[test]
fn same_fixture_over_local_and_object_is_byte_identical() {
    let local_dir = tempdir().unwrap();
    let object_dir = tempdir().unwrap();
    let local = build_fixture(local_dir.path(), BackendKind::Local);
    let object = build_fixture(object_dir.path(), BackendKind::Object);

    assert_same_logical_state(&local, &object);
    assert_same_artifacts(
        &local,
        &object,
        &BackendKind::Local,
        &BackendKind::Object,
        local_dir.path(),
        object_dir.path(),
    );

    // Both backends verify fully, including representation decoding.
    assert!(local.verify(true).unwrap().ok);
    assert!(object.verify(true).unwrap().ok);

    // Reopen both and require the same recovered head.
    drop(local);
    drop(object);
    let local = Store::open(local_dir.path(), OpenMode::ReadOnly).unwrap();
    let object =
        Store::open_with_backend(object_dir.path(), OpenMode::ReadOnly, BackendKind::Object)
            .unwrap();
    assert_same_logical_state(&local, &object);
    assert_eq!(local.snapshot_root(), object.snapshot_root());
}

#[test]
fn object_backend_exercises_store_extensions() {
    let dir = tempdir().unwrap();
    let mut store = Store::init_with_backend(dir.path(), opts(), BackendKind::Object).unwrap();
    let root = store.resolve_path("/").unwrap();

    let id = store
        .create_file_seeded(
            root,
            b"f",
            b"0123456789abcdefghij",
            0o644,
            0,
            0,
            attrs(),
            &seed(10),
        )
        .unwrap();
    assert_eq!(store.read_object(id).unwrap(), b"0123456789abcdefghij");

    // COW overwrite.
    store.write_file(id, 16, b"XYZ").unwrap();
    assert_eq!(store.read_object(id).unwrap(), b"0123456789abcdefXYZj");
    // Truncate extends with zeros.
    store.truncate(id, 24).unwrap();
    assert_eq!(store.read_object(id).unwrap().len(), 24);
    // Rename and rmdir.
    let sub = store
        .mkdir_seeded(root, b"sub", 0o755, 0, 0, &seed(11))
        .unwrap();
    store.rename(root, b"f", sub, b"g").unwrap();
    assert!(store.resolve_path("/sub/g").is_ok());
    store.unlink(sub, b"g").unwrap();
    store.rmdir(root, b"sub").unwrap();
    // xattr.
    store.set_xattr(root, "user.owner", b"court").unwrap();
    assert_eq!(
        store.get_xattr(root, "user.owner").unwrap().as_deref(),
        Some(b"court".as_slice())
    );
    // Relation.
    store
        .relation_add(
            root,
            "depends_on",
            RelationTarget::Value(weftcodec::wcb::Value::text("object")),
            None,
            attrs(),
        )
        .unwrap();
    // Snapshot + immutable read after a later write.
    let keep = store
        .create_file_seeded(root, b"keep", b"before", 0o644, 0, 0, attrs(), &seed(12))
        .unwrap();
    let snap = store.snapshot_create("before").unwrap();
    let before = store.read_object_at(Some(snap), keep).unwrap();
    assert_eq!(before, b"before");
    store.write_file(keep, 0, b"after!").unwrap();
    assert_eq!(store.read_object_at(Some(snap), keep).unwrap(), before);
    assert_eq!(store.read_object(keep).unwrap()[..5], *b"after");

    let report = store.verify(true).unwrap();
    assert!(report.ok, "{:?}", report.problems);
}

#[test]
fn capabilities_fail_closed() {
    let dir = tempdir().unwrap();
    let mut caps = Capabilities::local_posix();
    caps.durable_fsync = false;
    let weak = BackendKind::Posix { capabilities: caps };

    // Initialization publishes a genesis commit, so it must refuse.
    let init_err = expect_err(
        Store::init_with_backend(dir.path().join("weak-init"), opts(), weak.clone()),
        "weak init",
    );
    assert!(
        init_err.message().contains("durable_fsync"),
        "unexpected error: {init_err}"
    );

    // Build a normal local store, then attempt a durability-requiring open.
    drop(build_fixture(
        dir.path().join("ok").as_path(),
        BackendKind::Local,
    ));
    let open_err = expect_err(
        Store::open_with_backend(dir.path().join("ok"), OpenMode::ReadWrite, weak),
        "weak open",
    );
    assert!(
        open_err.message().contains("durable_fsync"),
        "unexpected error: {open_err}"
    );

    // atomic_publish=false may open for reading, but publication refuses.
    let mut caps = Capabilities::local_posix();
    caps.atomic_publish = false;
    let no_publish = BackendKind::Posix { capabilities: caps };
    // Init itself publishes, so it must refuse.
    let err = expect_err(
        Store::init_with_backend(dir.path().join("no-publish"), opts(), no_publish.clone()),
        "no-publish init",
    );
    assert!(
        err.message().contains("atomic_publish"),
        "unexpected error: {err}"
    );

    let mut store =
        Store::open_with_backend(dir.path().join("ok"), OpenMode::ReadWrite, no_publish).unwrap();
    let root = store.resolve_path("/").unwrap();
    let err = expect_err(
        store.create_file_seeded(root, b"x", b"x", 0o644, 0, 0, attrs(), &seed(20)),
        "no-publish create",
    );
    assert!(
        err.message().contains("atomic_publish"),
        "unexpected error: {err}"
    );
}

fn read_state(store: &Store, id: Hash32) -> Vec<u8> {
    store.read_object(id).unwrap()
}

#[test]
fn crash_proxy_over_object_backend_yields_old_or_new() {
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let dir = tempdir().unwrap();
        let mut store = Store::init_with_backend(dir.path(), opts(), BackendKind::Object).unwrap();
        let root = store.resolve_path("/").unwrap();
        let id = store
            .create_file_seeded(root, b"f", b"old\n", 0o644, 0, 0, attrs(), &seed(30))
            .unwrap();
        let generation = store.generation();
        drop(store);

        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .arg("--store")
            .arg(dir.path())
            .args(["--backend", "object"])
            .args([
                "write",
                "/f",
                "--offset",
                "0",
                "--text",
                "new content, longer than old\n",
            ])
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}"
        );

        let store =
            Store::open_with_backend(dir.path(), OpenMode::ReadOnly, BackendKind::Object).unwrap();
        let bytes = read_state(&store, id);
        assert!(
            bytes == b"old\n" || bytes == b"new content, longer than old\n",
            "phase {phase} produced a partial state: {bytes:?}"
        );
        if phase == "after_superblock_write" {
            assert_eq!(store.generation(), generation + 1);
            assert_eq!(bytes, b"new content, longer than old\n");
        } else {
            assert_eq!(store.generation(), generation);
            assert_eq!(bytes, b"old\n");
        }
        let _ = store.verify(true).unwrap();
    }
}
