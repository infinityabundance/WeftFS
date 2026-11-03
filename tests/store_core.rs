//! P4 store integration tests: objects, snapshots, relations, authorization,
//! locking, GC, and fixture determinism.

use std::collections::BTreeMap;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::conformance::build_fixture_store;
use weftfs::evaluate::Resolution;
use weftfs::identity::Hash32;
use weftfs::recovery::{manifest_path, object_path};
use weftfs::state::{ObjectKind, RelationTarget, State};
use weftfs::store::{OpenMode, Options, SetAttr, Store};

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

fn resolve(store: &Store, query: &str, snapshot: Option<Hash32>) -> Resolution {
    let selector = selector::parse(query).unwrap();
    store
        .resolve(
            &selector,
            snapshot,
            &Principal::root(),
            &weftcodec::Limits::default(),
        )
        .unwrap()
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
fn init_create_reopen_roundtrip() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    assert_eq!(store.generation(), 1);
    let root = store.resolve_path("/").unwrap();
    assert_eq!(root, State::ROOT_ID);

    let sub = store.mkdir(root, b"dir1", 0o755, 0, 0).unwrap();
    let a = store
        .create_file(root, b"a.txt", b"hello", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let b = store
        .create_file(sub, b"b.txt", b"world", 0o600, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(store.resolve_path("/a.txt").unwrap(), a);
    assert_eq!(store.resolve_path("/dir1/b.txt").unwrap(), b);
    drop(store);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.resolve_path("/dir1").unwrap(), sub);
    assert_eq!(store.read_object(b).unwrap(), b"world");
    let meta = store.object_meta(b).unwrap();
    assert_eq!(meta.kind, ObjectKind::File);
    assert_eq!(meta.mode, 0o600);
    assert_eq!(meta.uid, 0);
    assert_eq!(meta.gid, 0);
    assert_eq!(meta.size, 5);
    assert_eq!(names(&store, sub), vec![b"b.txt".to_vec()]);
    assert_eq!(
        names(&store, root),
        vec![b"a.txt".to_vec(), b"dir1".to_vec()]
    );
    let dir_meta = store.object_meta(sub).unwrap();
    assert_eq!(dir_meta.kind, ObjectKind::Dir);
    assert_eq!(dir_meta.size, 0);
}

#[test]
fn overwrite_range_truncate_and_snapshot_reads() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"f", b"AAAA", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let before = store.object_meta(id).unwrap();
    let snapshot = store.snapshot_create("s0").unwrap();

    let version2 = store.write_file(id, 1, b"BBBB").unwrap();
    assert_eq!(store.read_object(id).unwrap(), b"ABBBB");
    let after = store.object_meta(id).unwrap();
    assert_eq!(after.id, before.id, "ObjectId is stable across writes");
    assert_ne!(after.version, before.version, "VersionId changes");
    assert_eq!(after.version, version2);
    assert_eq!(after.size, 5);

    assert_eq!(store.read_object_at(Some(snapshot), id).unwrap(), b"AAAA");
    assert_eq!(
        store.read_content(before.content, 0, u64::MAX).unwrap(),
        b"AAAA"
    );
    assert_eq!(
        store.read_content(after.content, 0, u64::MAX).unwrap(),
        b"ABBBB"
    );

    store.truncate(id, 2).unwrap();
    assert_eq!(store.read_object(id).unwrap(), b"AB");
    drop(store);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.read_object(id).unwrap(), b"AB");
    assert_eq!(store.read_object_at(Some(snapshot), id).unwrap(), b"AAAA");
}

#[test]
fn rename_hardlink_unlink_snapshot_and_gc_retention() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let content = b"shared content that must survive through the snapshot";
    let id = store
        .create_file(root, b"one", content, 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    store.rename(root, b"one", root, b"two").unwrap();
    assert_eq!(store.resolve_path("/two").unwrap(), id);
    assert_eq!(
        store.lookup(root, b"one").unwrap_err().class_name(),
        "NotFound"
    );

    store.link(id, root, b"three").unwrap();
    assert_eq!(store.resolve_path("/two").unwrap(), id);
    assert_eq!(store.resolve_path("/three").unwrap(), id);

    store.unlink(root, b"two").unwrap();
    assert_eq!(store.read_object(id).unwrap(), content);

    let snapshot = store.snapshot_create("keep").unwrap();
    let content_root = store.object_meta(id).unwrap().content;
    store.unlink(root, b"three").unwrap();
    assert_eq!(
        store.lookup(root, b"three").unwrap_err().class_name(),
        "NotFound"
    );
    assert_eq!(store.object_meta(id).unwrap_err().class_name(), "NotFound");
    assert_eq!(
        store.read_object_at(Some(snapshot), id).unwrap(),
        content,
        "snapshot keeps the object readable"
    );

    let report = store.gc(true).unwrap();
    assert!(report.applied);
    assert_eq!(
        store.read_content(content_root, 0, u64::MAX).unwrap(),
        content,
        "GC must retain content reachable from a snapshot"
    );
    let state = store.state_at(snapshot).unwrap();
    assert!(state.objects.contains_key(&id));
}

#[test]
fn snapshots_are_stable_and_selectors_follow_the_pin() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, b"first", b"one", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let snapshot = store.snapshot_create("v1").unwrap();
    store
        .create_file(root, b"second", b"two", 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    let pinned = resolve(&store, "select kind == \"file\"", Some(snapshot));
    assert_eq!(pinned.matched_count, 1);
    assert_eq!(pinned.snapshot_id, snapshot);
    let live = resolve(&store, "select kind == \"file\"", None);
    assert_eq!(live.matched_count, 2);
    assert_ne!(pinned.selector_id, Hash32::ZERO);
    assert_eq!(
        pinned.selector_id, live.selector_id,
        "SelectorId is the query hash"
    );
    drop(store);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.snapshot_list(),
        vec![("v1".to_owned(), snapshot)],
        "snapshot id is stable across reopen"
    );
    let reopened = resolve(&store, "select kind == \"file\"", Some(snapshot));
    assert_eq!(reopened.matched_count, 1);
    assert_eq!(reopened.snapshot_id, snapshot);
}

#[test]
fn relations_cycles_tags_one_hop_and_cardinality() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let a = store
        .create_file(root, b"a", b"aaa", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let b = store
        .create_file(root, b"b", b"bbb", 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    let tag = store
        .relation_add(
            a,
            "tagged_as",
            RelationTarget::Value(Value::Str("demo".to_owned())),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .relation_add(
            a,
            "derived_from",
            RelationTarget::Object(b),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    // Cycles are legal in the context graph.
    store
        .relation_add(
            b,
            "derived_from",
            RelationTarget::Object(a),
            None,
            BTreeMap::new(),
        )
        .unwrap();

    let tagged = resolve(&store, "select tag(\"demo\")", None);
    assert_eq!(tagged.matched_count, 1);
    assert_eq!(tagged.results[0].object_id, a);
    assert_eq!(tagged.evidence[0].entries[0].kind, "tag");

    let derived = resolve(
        &store,
        &format!("select relation(derived_from, obj:{})", b.to_hex()),
        None,
    );
    assert_eq!(derived.matched_count, 1);
    assert_eq!(derived.results[0].object_id, a);

    let selector = selector::parse("select kind == \"file\"").unwrap();
    let err = store
        .require_one(
            &selector,
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
        )
        .unwrap_err();
    assert_eq!(err.class_name(), "Ambiguous");

    let none = selector::parse("select size == 999999").unwrap();
    let err = store
        .require_one(
            &none,
            None,
            &Principal::root(),
            &weftcodec::Limits::default(),
        )
        .unwrap_err();
    assert_eq!(err.class_name(), "NotFound");

    store.relation_delete(tag).unwrap();
    assert_eq!(
        resolve(&store, "select tag(\"demo\")", None).matched_count,
        0
    );
    // The derived_from cycle is still present.
    assert_eq!(store.current_state().relations.len(), 2);
}

#[test]
fn authorization_filters_results_counts_and_evidence() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let public = store
        .create_file(root, b"public", b"public", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let private = store
        .create_file(root, b"private", b"secret", 0o600, 0, 0, BTreeMap::new())
        .unwrap();
    let acl_yes = store
        .create_file(
            root,
            b"acl-yes",
            b"acl",
            0o644,
            0,
            0,
            BTreeMap::from([(
                "weft.acl.read".to_owned(),
                Value::Array(vec![Value::U64(1000)]),
            )]),
        )
        .unwrap();
    store
        .create_file(
            root,
            b"acl-no",
            b"nope",
            0o644,
            0,
            0,
            BTreeMap::from([(
                "weft.acl.read".to_owned(),
                Value::Array(vec![Value::U64(0)]),
            )]),
        )
        .unwrap();

    store
        .relation_add(
            public,
            "tagged_as",
            RelationTarget::Value(Value::Str("visible".to_owned())),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .relation_add(
            private,
            "tagged_as",
            RelationTarget::Value(Value::Str("secret".to_owned())),
            None,
            BTreeMap::new(),
        )
        .unwrap();

    let principal = Principal::new(1000, 1000, Vec::new());
    let limits = weftcodec::Limits::default();
    let query = selector::parse("select kind == \"file\"").unwrap();
    let resolution = store.resolve(&query, None, &principal, &limits).unwrap();
    let ids: Vec<Hash32> = resolution.results.iter().map(|r| r.object_id).collect();
    assert_eq!(resolution.matched_count, 2);
    assert!(ids.contains(&public));
    assert!(ids.contains(&acl_yes));
    assert!(!ids.contains(&private));
    assert_eq!(resolution.evidence.len(), 2);

    let tag = selector::parse("select tag(\"secret\")").unwrap();
    let hidden = store.resolve(&tag, None, &principal, &limits).unwrap();
    assert_eq!(hidden.matched_count, 0);
    assert!(hidden.evidence.is_empty());

    let visible = resolve(&store, "select tag(\"visible\")", None);
    assert_eq!(visible.matched_count, 1);
    assert_eq!(visible.results[0].object_id, public);

    assert!(weftfs::auth::can_read(
        &store.object_meta(public).unwrap(),
        &principal
    ));
    assert!(!weftfs::auth::can_read(
        &store.object_meta(private).unwrap(),
        &principal
    ));
    assert!(weftfs::auth::can_read(
        &store.object_meta(acl_yes).unwrap(),
        &principal
    ));
}

#[test]
fn single_writer_lock_and_read_only_open() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let root = writer.resolve_path("/").unwrap();
    writer
        .create_file(root, b"f", b"x", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let generation = writer.generation();

    let reader = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reader.generation(), generation);
    assert_eq!(
        reader
            .read_object(reader.resolve_path("/f").unwrap())
            .unwrap(),
        b"x"
    );

    let second = match Store::open(dir.path(), OpenMode::ReadWrite) {
        Ok(_) => panic!("second writer unexpectedly acquired the lock"),
        Err(error) => error,
    };
    assert_eq!(second.class_name(), "Busy");

    drop(writer);
    let reopened = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
    assert_eq!(reopened.generation(), generation);
}

#[test]
fn gc_mark_sweep_removes_only_unreachable_blobs() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let old_content = b"old content that becomes unreachable after overwrite";
    let id = store
        .create_file(root, b"f", old_content, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let old_meta = store.object_meta(id).unwrap();
    let old_manifest_path = manifest_path(dir.path(), old_meta.content);
    let old_manifest = weftfs::recovery::read_manifest(dir.path(), old_meta.content).unwrap();
    let old_chunk_path = object_path(dir.path(), old_manifest.chunks[0].hash);
    assert!(old_manifest_path.exists());
    assert!(old_chunk_path.exists());

    store
        .write_file(id, 0, b"new content with a different chunk")
        .unwrap();
    store
        .truncate(id, b"new content with a different chunk".len() as u64)
        .unwrap();
    let dry = store.gc(false).unwrap();
    assert!(!dry.applied);
    assert!(!dry.candidates.is_empty());
    assert!(old_manifest_path.exists());
    assert!(old_chunk_path.exists());

    let applied = store.gc(true).unwrap();
    assert!(applied.applied);
    assert!(!applied.deleted.is_empty());
    assert!(!old_chunk_path.exists(), "unreachable chunk is swept");
    assert_eq!(
        store.read_object(id).unwrap(),
        b"new content with a different chunk"
    );

    // Snapshot retention: content reachable from a snapshot survives.
    let retained = b"retained by snapshot";
    let rid = store
        .create_file(root, b"r", retained, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let rmeta = store.object_meta(rid).unwrap();
    let rmanifest = weftfs::recovery::read_manifest(dir.path(), rmeta.content).unwrap();
    let rchunk = object_path(dir.path(), rmanifest.chunks[0].hash);
    let snapshot = store.snapshot_create("keep").unwrap();
    store.write_file(rid, 0, b"replacement").unwrap();
    store.gc(true).unwrap();
    assert!(rchunk.exists());
    assert_eq!(store.read_object_at(Some(snapshot), rid).unwrap(), retained);
}

#[test]
fn conformance_fixture_is_deterministic_and_small() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let a = build_fixture_store(&first.path().join("store")).unwrap();
    let b = build_fixture_store(&second.path().join("store")).unwrap();

    assert_eq!(a.format_version, "weftfs-store 1");
    assert_eq!(a.generation, b.generation);
    assert_eq!(a.head, b.head);
    assert_eq!(a.objects.len(), b.objects.len());
    for (left, right) in a.objects.iter().zip(b.objects.iter()) {
        assert_eq!(left.path, right.path);
        assert_eq!(left.object_id, right.object_id);
        assert_eq!(left.version_id, right.version_id);
        assert_eq!(left.content_id, right.content_id);
        assert_eq!(left.content_hash, right.content_hash);
        assert_eq!(left.size, right.size);
    }
    assert_eq!(a.snapshots, b.snapshots);
    assert_eq!(a.relation_ids, b.relation_ids);
    assert_eq!(a.selectors.len(), 3);
    for (left, right) in a.selectors.iter().zip(b.selectors.iter()) {
        assert_eq!(left.source, right.source);
        assert_eq!(left.selector_id, right.selector_id);
        assert_eq!(left.snapshot_id, right.snapshot_id);
        assert_eq!(left.matched_count, right.matched_count);
        assert_eq!(left.object_ids, right.object_ids);
    }
    assert_eq!(a.selectors[0].matched_count, 3, "three regular files");
    assert_eq!(a.selectors[1].matched_count, 1, "one tagged object");
    assert_eq!(a.selectors[2].matched_count, 1, "one derived_from edge");

    // The committed handoff fixture must match the current implementation.
    let committed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/store/v1/EXPECTED.json");
    if committed.exists() {
        let text = std::fs::read_to_string(&committed).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            expected["generation"].as_u64().unwrap(),
            a.generation,
            "committed fixture generation drifted"
        );
        assert_eq!(
            expected["snapshot_root"].as_str().unwrap(),
            a.head.to_hex(),
            "committed fixture head drifted"
        );
    }

    fn dir_size(path: &Path) -> u64 {
        let mut total = 0;
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                total += dir_size(&entry.path());
            } else {
                total += metadata.len();
            }
        }
        total
    }
    let size = dir_size(&first.path().join("store"));
    assert!(
        size < 100 * 1024,
        "fixture must stay under 100 KiB, got {size}"
    );
}

#[test]
fn empty_files_and_range_reads() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let empty = store
        .create_file(root, b"empty", b"", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(store.read_object(empty).unwrap(), b"");
    assert_eq!(store.object_meta(empty).unwrap().size, 0);
    assert_eq!(store.read_range(empty, 0, 10).unwrap(), b"");

    let mut body = Vec::new();
    for index in 0..3000u32 {
        body.push((index % 251) as u8);
    }
    let file = store
        .create_file(root, b"range", &body, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    assert_eq!(store.read_object(file).unwrap(), body);
    assert_eq!(
        store.read_range(file, 1000, 1500).unwrap(),
        &body[1000..2500]
    );
    assert_eq!(
        store.read_range(file, 2990, 100).unwrap(),
        &body[2990..3000]
    );
    assert_eq!(store.read_range(file, 4000, 10).unwrap(), b"");
    let content_root = store.object_meta(file).unwrap().content;
    assert_eq!(
        store.read_content(content_root, 1023, 3).unwrap(),
        &body[1023..1026]
    );
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn setattr_and_symlink_metadata() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let file = store
        .create_file(root, b"f", b"data", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let old_version = store.object_meta(file).unwrap().version;
    let new_version = store
        .setattr(
            file,
            SetAttr {
                mode: Some(0o600),
                uid: Some(501),
                gid: Some(20),
                mtime_ns: Some(123),
                attrs: None,
            },
        )
        .unwrap();
    assert_ne!(new_version, old_version);
    let meta = store.object_meta(file).unwrap();
    assert_eq!(meta.mode, 0o600);
    assert_eq!(meta.uid, 501);
    assert_eq!(meta.gid, 20);
    assert_eq!(meta.mtime_ns, 123);

    let link = store.symlink(root, b"l", b"f", 501, 20).unwrap();
    let link_meta = store.object_meta(link).unwrap();
    assert_eq!(link_meta.kind, ObjectKind::Symlink);
    assert_eq!(link_meta.symlink_target(), Some(&b"f"[..]));
    assert_eq!(store.read_object(link).unwrap(), b"f");
    assert_eq!(store.read_object(file).unwrap(), b"data");
}
