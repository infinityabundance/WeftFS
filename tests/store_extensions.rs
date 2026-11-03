//! P5 store extensions: rmdir, POSIX rename replacement, xattrs, saved
//! contexts, open-handle pins, link counts, and statfs counters.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::selector;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::{RelationTarget, State};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
    }
}

fn names(store: &Store, dir: Hash32) -> Vec<Vec<u8>> {
    store
        .list_dir(dir)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn rmdir_removes_empty_directories_and_rejects_non_empty() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let empty = store.mkdir(root, b"empty", 0o755, 0, 0).unwrap();
    let full = store.mkdir(root, b"full", 0o755, 0, 0).unwrap();
    store
        .create_file(full, b"child", b"x", 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    let error = store.rmdir(root, b"full").unwrap_err();
    assert_eq!(error.class(), Class::NotEmpty);
    let error = store.rmdir(root, b"empty-missing").unwrap_err();
    assert_eq!(error.class(), Class::NotFound);

    store.rmdir(root, b"empty").unwrap();
    assert_eq!(
        store.resolve_path("/empty").unwrap_err().class(),
        Class::NotFound
    );
    assert!(!store.current_state().objects.contains_key(&empty));
    assert_eq!(names(&store, root), vec![b"full".to_vec()]);

    // Non-directory is refused with the POSIX ENOTDIR class.
    store
        .create_file(root, b"file", b"", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(
        store.rmdir(root, b"file").unwrap_err().class(),
        Class::NotDir
    );

    // A directory object pinned by an open handle survives `rmdir` until the
    // last pin is released (the store-level contract the FUSE layer uses).
    let pinned = store.mkdir(root, b"pinned", 0o755, 0, 0).unwrap();
    store.pin_object(pinned).unwrap();
    store.rmdir(root, b"pinned").unwrap();
    assert!(store.current_state().objects.contains_key(&pinned));
    store.unpin_object(pinned).unwrap();
    assert!(!store.current_state().objects.contains_key(&pinned));

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.resolve_path("/full").unwrap(), full);
    assert_eq!(
        store.resolve_path("/empty").unwrap_err().class(),
        Class::NotFound
    );
}

#[test]
fn rename_replaces_files_and_empty_directories_atomically() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let _sub = store.mkdir(root, b"sub", 0o755, 0, 0).unwrap();
    let src = store
        .create_file(root, b"src", b"source", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let dst = store
        .create_file(root, b"dst", b"destination", 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    // Replace a file: target ObjectId replaces the destination ObjectId.
    let before = store.generation();
    store.rename(root, b"src", root, b"dst").unwrap();
    assert_eq!(store.generation(), before + 1, "rename must be one commit");
    assert_eq!(store.resolve_path("/dst").unwrap(), src);
    assert_eq!(store.read_object(src).unwrap(), b"source");
    assert!(!store.current_state().objects.contains_key(&dst));
    assert_eq!(
        store.resolve_path("/src").unwrap_err().class(),
        Class::NotFound
    );

    // RENAME_NOREPLACE refuses an existing destination.
    let other = store
        .create_file(root, b"other", b"other", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let error = store
        .rename_no_replace(root, b"other", root, b"dst")
        .unwrap_err();
    assert_eq!(error.class(), Class::Exists);
    assert_eq!(store.read_object(other).unwrap(), b"other");

    // Directory replacement: empty destination directory is replaced.
    let keep = store.mkdir(root, b"keep", 0o755, 0, 0).unwrap();
    let doomed = store.mkdir(root, b"doomed", 0o755, 0, 0).unwrap();
    store.rename(root, b"keep", root, b"doomed").unwrap();
    assert_eq!(store.resolve_path("/doomed").unwrap(), keep);
    assert!(!store.current_state().objects.contains_key(&doomed));

    // Non-empty destination directory is ENOTEMPTY.
    let a = store.mkdir(root, b"a", 0o755, 0, 0).unwrap();
    let b = store.mkdir(root, b"b", 0o755, 0, 0).unwrap();
    store
        .create_file(b, b"child", b"x", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(
        store.rename(root, b"a", root, b"b").unwrap_err().class(),
        Class::NotEmpty
    );
    assert_eq!(store.resolve_path("/b").unwrap(), b);

    // A file cannot replace a directory and a directory cannot replace a file.
    assert_eq!(
        store.rename(root, b"dst", root, b"b").unwrap_err().class(),
        Class::IsDir
    );
    assert_eq!(
        store.rename(root, b"b", root, b"dst").unwrap_err().class(),
        Class::NotDir
    );

    // A directory cannot move inside itself.
    let inner = store.mkdir(a, b"inner", 0o755, 0, 0).unwrap();
    assert_eq!(
        store
            .rename(root, b"a", inner, b"moved")
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );

    // Renaming a directory to a new name keeps its ObjectId and subtree.
    store.rename(root, b"a", root, b"renamed").unwrap();
    assert_eq!(store.resolve_path("/renamed").unwrap(), a);
    assert_eq!(store.resolve_path("/renamed/inner").unwrap(), inner);

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.read_object(src).unwrap(), b"source");
    assert_eq!(store.resolve_path("/renamed/inner").unwrap(), inner);
}

#[test]
fn rename_hardlink_identity_and_exchange() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let a = store
        .create_file(root, b"a", b"a-bytes", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    store.link(a, root, b"b").unwrap();
    let before = store.generation();
    // Renaming between hard links to the same object is a POSIX no-op.
    store.rename(root, b"a", root, b"b").unwrap();
    assert_eq!(store.generation(), before);
    assert_eq!(store.resolve_path("/a").unwrap(), a);
    assert_eq!(store.resolve_path("/b").unwrap(), a);

    let other = store
        .create_file(root, b"other", b"other", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    store.rename_exchange(root, b"a", root, b"other").unwrap();
    assert_eq!(store.resolve_path("/a").unwrap(), other);
    assert_eq!(store.resolve_path("/b").unwrap(), a);
    assert_eq!(store.resolve_path("/other").unwrap(), a);
    assert_eq!(store.read_object(other).unwrap(), b"other");

    let sub = store.mkdir(root, b"dir", 0o755, 0, 0).unwrap();
    assert_eq!(
        store
            .rename_exchange(root, b"dir", root, b"a")
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );
    assert_eq!(store.resolve_path("/dir").unwrap(), sub);
}

#[test]
fn xattrs_are_stored_per_spec_and_survive_reopen() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let file = store
        .create_file(root, b"f", b"body", 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    assert_eq!(store.list_xattr(file).unwrap(), Vec::<String>::new());
    store.set_xattr(file, "user.color", b"blue").unwrap();
    store.set_xattr(file, "user.empty", b"").unwrap();
    store
        .set_xattr(file, "user.binary", &[0, 1, 2, 255])
        .unwrap();
    assert_eq!(
        store.get_xattr(file, "user.color").unwrap(),
        Some(b"blue".to_vec())
    );
    assert_eq!(store.get_xattr(file, "user.missing").unwrap(), None);
    assert_eq!(
        store.list_xattr(file).unwrap(),
        vec![
            "user.binary".to_owned(),
            "user.color".to_owned(),
            "user.empty".to_owned()
        ]
    );

    // Reserved namespace, malformed names, and oversized values are refused.
    assert_eq!(
        store
            .set_xattr(file, "weft.secret", b"x")
            .unwrap_err()
            .class(),
        Class::InvalidName
    );
    assert_eq!(
        store.set_xattr(file, "bad/name", b"x").unwrap_err().class(),
        Class::InvalidName
    );
    let big = vec![0u8; 65_537];
    assert_eq!(
        store.set_xattr(file, "user.big", &big).unwrap_err().class(),
        Class::LimitExceeded
    );

    // The stored key is exactly `xattr.<name>` and it is bytes.
    let record = store.object_meta(file).unwrap();
    assert_eq!(
        record.attrs.get("xattr.user.color"),
        Some(&weftcodec::wcb::Value::Bytes(b"blue".to_vec()))
    );

    assert!(store.remove_xattr(file, "user.color").unwrap());
    assert!(!store.remove_xattr(file, "user.color").unwrap());
    assert_eq!(store.get_xattr(file, "user.color").unwrap(), None);
    assert_eq!(
        store.list_xattr(file).unwrap(),
        vec!["user.binary".to_owned(), "user.empty".to_owned()]
    );

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.get_xattr(file, "user.binary").unwrap(),
        Some(vec![0, 1, 2, 255])
    );
}

#[test]
fn saved_contexts_are_relations_with_canonical_selector_hex() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let selector = selector::parse("select kind == \"file\"").unwrap();

    let relation = store.save_context("text-files", &selector).unwrap();
    let contexts = store.list_contexts().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].name, "text-files");
    assert_eq!(contexts[0].relation_id, relation);
    assert_eq!(
        contexts[0].selector_hex,
        hex::encode(selector.canonical_bytes().unwrap())
    );
    assert_eq!(contexts[0].selector_id, selector.selector_id().unwrap());
    assert_eq!(
        store
            .load_context("text-files")
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        selector.canonical_bytes().unwrap()
    );

    // Saving the identical context is idempotent (no extra commit).
    let generation = store.generation();
    assert_eq!(
        store.save_context("text-files", &selector).unwrap(),
        relation
    );
    assert_eq!(store.generation(), generation);

    // On-disk shape: root subject, value target, selector_hex attribute.
    let record = store
        .current_state()
        .relations
        .get(&relation)
        .cloned()
        .unwrap();
    assert_eq!(record.subject, State::ROOT_ID);
    assert_eq!(record.predicate, "saved_context");
    assert_eq!(
        record.target,
        RelationTarget::Value(weftcodec::wcb::Value::Str("text-files".to_owned()))
    );

    // Replacement is delete + add in exactly one commit.
    let replacement = selector::parse("select kind == \"dir\"").unwrap();
    let generation = store.generation();
    let new_id = store.save_context("text-files", &replacement).unwrap();
    assert_ne!(new_id, relation);
    assert_eq!(store.generation(), generation + 1);
    assert!(!store.current_state().relations.contains_key(&relation));
    assert_eq!(
        store
            .load_context("text-files")
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        replacement.canonical_bytes().unwrap()
    );

    // Two contexts, deterministic order.
    store
        .save_context("all", &selector::parse("select true").unwrap())
        .unwrap();
    assert_eq!(
        store
            .list_contexts()
            .unwrap()
            .into_iter()
            .map(|context| context.name)
            .collect::<Vec<_>>(),
        vec!["all".to_owned(), "text-files".to_owned()]
    );

    store.delete_context("all").unwrap();
    assert_eq!(
        store.delete_context("all").unwrap_err().class(),
        Class::NotFound
    );

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store
            .load_context("text-files")
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        replacement.canonical_bytes().unwrap()
    );
}

#[test]
fn contexts_are_snapshot_scoped_and_round_trip_through_replay() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    store
        .save_context(
            "files",
            &selector::parse("select kind == \"file\"").unwrap(),
        )
        .unwrap();
    let snapshot = store.snapshot_create("v1").unwrap();
    store
        .save_context("dirs", &selector::parse("select kind == \"dir\"").unwrap())
        .unwrap();

    let current = store.list_contexts().unwrap();
    assert_eq!(
        current.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["dirs", "files"]
    );
    let pinned = store.state_at(snapshot).unwrap();
    // The snapshot commit itself names the pinned parent state; the
    // `set_snapshot` op is published in the next commit (P4 §3.3), so the
    // pinned state holds the older context set only.
    assert!(pinned.snapshots.is_empty());
    let pinned_relation = pinned
        .relations
        .values()
        .find(|relation| {
            relation.predicate == "saved_context"
                && matches!(
                    &relation.target,
                    RelationTarget::Value(weftcodec::wcb::Value::Str(name)) if name == "files"
                )
        })
        .expect("snapshot state keeps the older context");
    assert_eq!(pinned_relation.subject, root);

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store
            .list_contexts()
            .unwrap()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["dirs", "files"]
    );
}

#[test]
fn open_handle_pins_keep_unlinked_content_readable() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let file = store
        .create_file(
            root,
            b"open.txt",
            b"pinned bytes",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let record = store.object_meta(file).unwrap();
    let content = record.content;
    let size = record.size;

    assert_eq!(store.pin_object(file).unwrap(), 1);
    store.unlink(root, b"open.txt").unwrap();
    // The object record survives because it is pinned.
    assert!(store.current_state().objects.contains_key(&file));
    assert_eq!(store.object_pin_count(file), 1);
    assert_eq!(store.read_object(file).unwrap(), b"pinned bytes");
    assert_eq!(
        store.read_content(content, 0, size).unwrap(),
        b"pinned bytes"
    );
    assert_eq!(
        store.read_content(content, 0, size).unwrap(),
        b"pinned bytes"
    );

    // A write through the pinned object still works and stays addressable.
    store.write_file(file, 0, b"PINNED").unwrap();
    let record = store.object_meta(file).unwrap();
    assert_eq!(
        store.read_content(record.content, 0, record.size).unwrap(),
        b"PINNED bytes"
    );

    // The last unpin deletes the unreferenced object record durably.
    let generation = store.generation();
    assert_eq!(store.unpin_object(file).unwrap(), 0);
    assert_eq!(store.generation(), generation + 1);
    assert!(!store.current_state().objects.contains_key(&file));

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.resolve_path("/open.txt").unwrap_err().class(),
        Class::NotFound
    );
}

#[test]
fn gc_retains_pinned_content_and_sweeps_it_after_unpin() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let file = store
        .create_file(root, b"gc.txt", b"keep me", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let content = store.object_meta(file).unwrap().content;
    store.unlink(root, b"gc.txt").unwrap();

    store.pin_content(content);
    let report = store.gc(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(
        report
            .candidates
            .iter()
            .all(|path| !path.contains(&content.to_hex())),
        "pinned manifest must not be a candidate: {:?}",
        report.candidates
    );
    assert_eq!(store.read_content(content, 0, 7).unwrap(), b"keep me");

    store.unpin_content(content);
    let report = store.gc(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(store.read_content(content, 0, 7).is_err());
}

#[test]
fn nlink_and_statfs_counters_derive_from_state() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let sub = store.mkdir(root, b"sub", 0o755, 0, 0).unwrap();
    let file = store
        .create_file(root, b"f", &[0u8; 4096], 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(store.nlink(file).unwrap(), 1);
    store.link(file, root, b"f2").unwrap();
    assert_eq!(store.nlink(file).unwrap(), 2);
    store.link(file, sub, b"f3").unwrap();
    assert_eq!(store.nlink(file).unwrap(), 3);
    store.unlink(root, b"f").unwrap();
    assert_eq!(store.nlink(file).unwrap(), 2);

    // Directories: 2 + number of subdirectories, independent of files.
    assert_eq!(store.nlink(root).unwrap(), 3); // 2 + "sub"
    assert_eq!(store.nlink(sub).unwrap(), 2);

    let stats = store.statfs();
    assert_eq!(stats.block_size, 4096);
    assert_eq!(stats.files, store.current_state().objects.len() as u64);
    assert!(stats.blocks >= 2, "content and metadata blocks are counted");
    assert_eq!(stats.files_free, 0);
    assert_eq!(stats.max_name_len, 255);

    let error = store
        .nlink(Hash32::from_slice(&[7u8; 32]).unwrap())
        .unwrap_err();
    assert_eq!(error.class(), Class::NotFound);
}

#[test]
fn orphan_object_records_left_by_a_crash_are_removed_by_gc_apply() {
    // Simulate a crash during an open-unlinked handle: the object record stays
    // referenced by no entry. A writer reopen still sees it; `gc --apply`
    // removes it in a commit.
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = State::ROOT_ID;
    let file = store
        .create_file(root, b"tmp", b"orphan", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    store.pin_object(file).unwrap();
    store.unlink(root, b"tmp").unwrap();
    // Drop without unpinning: the pin is process-local and never persisted.
    drop(store);

    let mut store = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
    assert!(store.current_state().objects.contains_key(&file));
    let report = store.gc(true).unwrap();
    assert_eq!(report.removed_orphans, vec![file.to_hex()]);
    assert!(!store.current_state().objects.contains_key(&file));
    assert_eq!(
        store.object_meta(file).unwrap_err().class(),
        Class::NotFound
    );
    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert!(!store.current_state().objects.contains_key(&file));
}
