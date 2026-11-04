//! Persisted per-commit selector-index segments (`indexes/<ab>/<commit>.idx`).
//!
//! The index is advisory. Every test here proves the same safety rule: a
//! missing, stale, truncated, or tampered segment is ignored, the index is
//! rebuilt from replayed state, and query results are exactly the scan
//! oracle's results.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tempfile::tempdir;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::evaluate::Resolution;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

const INDEX_MAGIC: &[u8; 4] = b"WFX1";
const INDEX_DOMAIN: &str = "weft/index/v1";

struct Fixture {
    store: Store,
    visible: Vec<Hash32>,
    hidden: Vec<Hash32>,
    files: Vec<Hash32>,
    dir: tempfile::TempDir,
}

fn options() -> Options {
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

fn build_fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let d0 = store
        .mkdir_seeded(root, b"d0", 0o755, 0, 0, b"dir-0")
        .unwrap();
    let d1 = store
        .mkdir_seeded(root, b"d1", 0o755, 0, 0, b"dir-1")
        .unwrap();
    let parents = [root, d0, d1];

    let teams = ["red", "green", "blue"];
    let tags = ["alpha", "beta", "gamma"];
    let mut files = Vec::new();
    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    for index in 0..18u64 {
        let parent = parents[(index as usize) % parents.len()];
        // Every seventh object is owner-only for a different uid, so a uid-0
        // principal cannot read it or anything hung off it.
        let (mode, uid, gid, readable) = if index % 7 == 0 {
            (0o600, 2000, 2000, false)
        } else {
            (0o644, 1000, 1000, true)
        };
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            Value::Str(teams[(index % teams.len() as u64) as usize].to_owned()),
        );
        attrs.insert("score".to_owned(), Value::U64(index % 9));
        attrs.insert("signed".to_owned(), Value::I64((index % 5) as i64));
        let id = store
            .create_file_seeded(
                parent,
                format!("f{index}").as_bytes(),
                format!("body-{index}").as_bytes(),
                mode,
                uid,
                gid,
                attrs,
                format!("persist-object-{index}").as_bytes(),
            )
            .unwrap();
        files.push(id);
        if readable {
            visible.push(id);
        } else {
            hidden.push(id);
        }
    }

    for (index, object) in files.iter().enumerate() {
        if index % 2 == 0 {
            store
                .relation_add(
                    *object,
                    "tagged_as",
                    RelationTarget::Value(Value::Str(tags[index % tags.len()].to_owned())),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
        if index % 3 == 0 {
            let target = files[(index * 7 + 3) % files.len()];
            store
                .relation_add(
                    *object,
                    "links",
                    RelationTarget::Object(target),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
    }
    // An edge from a readable object into a hidden object: the candidate set
    // must still be filtered by authorization, exactly like the scan.
    store
        .relation_add(
            visible[0],
            "links",
            RelationTarget::Object(hidden[0]),
            None,
            BTreeMap::new(),
        )
        .unwrap();

    Fixture {
        store,
        visible,
        hidden,
        files,
        dir,
    }
}

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn index_path(root: &Path, commit: Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}

fn selectors(
    visible_objects: &[Hash32],
    hidden_objects: &[Hash32],
    files: &[Hash32],
) -> Vec<(String, Selector)> {
    let visible = visible_objects[0];
    let hidden = hidden_objects[0];
    let texts = vec![
        "select true".to_owned(),
        "select kind == \"file\"".to_owned(),
        "select has(attrs.team)".to_owned(),
        "select has(attrs.missing)".to_owned(),
        "select not has(attrs.team)".to_owned(),
        "select attrs.team == \"red\"".to_owned(),
        "select attrs.team != \"red\"".to_owned(),
        "select attrs.score == 3".to_owned(),
        "select attrs.score > 5".to_owned(),
        "select attrs.signed < 2".to_owned(),
        "select tag(\"alpha\")".to_owned(),
        "select tag(\"beta\")".to_owned(),
        "select tag(\"nope\")".to_owned(),
        "select tag(\"alpha\") and attrs.team == \"red\"".to_owned(),
        "select not tag(\"alpha\")".to_owned(),
        format!("select relation(links, obj:{})", visible.to_hex()),
        format!("select relation(links, obj:{})", hidden.to_hex()),
        format!("select not relation(links, obj:{})", hidden.to_hex()),
        format!("select object_id == \"{}\"", files[0].to_hex()),
        "select true order by size desc limit 5".to_owned(),
        "select kind == \"file\" order by attrs.score desc limit 6".to_owned(),
    ];
    texts
        .into_iter()
        .map(|text| {
            let selector = selector::parse(&text)
                .unwrap_or_else(|error| panic!("selector `{text}` did not parse: {error}"));
            (text, selector)
        })
        .collect()
}

fn compare(label: &str, indexed: &Resolution, scan: &Resolution) {
    assert_eq!(
        indexed.selector_id, scan.selector_id,
        "{label}: selector_id"
    );
    assert_eq!(
        indexed.snapshot_id, scan.snapshot_id,
        "{label}: snapshot_id"
    );
    assert_eq!(
        indexed.matched_count, scan.matched_count,
        "{label}: matched_count"
    );
    assert_eq!(indexed.results, scan.results, "{label}: ordered results");
    assert_eq!(indexed.evidence, scan.evidence, "{label}: evidence");
}

/// Compare the persisted-index path against the index-free scan oracle for
/// every selector and view. Returns the number of comparisons.
fn check_matrix(
    store: &Store,
    visible: &[Hash32],
    hidden: &[Hash32],
    files: &[Hash32],
    view: Option<Hash32>,
    label: &str,
) -> usize {
    let principal = principal();
    let limits = weftcodec::Limits::default();
    let mut checked = 0;
    for (text, selector) in selectors(visible, hidden, files) {
        let indexed = store
            .resolve(&selector, view, &principal, &limits)
            .unwrap_or_else(|err| panic!("{label} `{text}` indexed: {err}"));
        let scan = store
            .resolve_scan(&selector, view, &principal, &limits)
            .unwrap_or_else(|err| panic!("{label} `{text}` scan: {err}"));
        compare(&format!("{label} `{text}`"), &indexed, &scan);
        checked += 1;
    }
    checked
}

fn read_index_value(path: &Path) -> Value {
    let bytes = fs::read(path).expect("read index");
    assert_eq!(&bytes[0..4], INDEX_MAGIC, "index magic");
    let len = u64::from_le_bytes(bytes[4..12].try_into().unwrap()) as usize;
    weftcodec::wcb::decode_default(&bytes[12..12 + len]).expect("decode index WCB")
}

fn write_index_value(path: &Path, value: &Value) {
    let wcb = weftcodec::wcb::encode_default(value).expect("encode index WCB");
    let digest = weftfs::identity::h(INDEX_DOMAIN, &wcb);
    let mut out = Vec::with_capacity(4 + 8 + wcb.len() + 32);
    out.extend_from_slice(INDEX_MAGIC);
    out.extend_from_slice(&(wcb.len() as u64).to_le_bytes());
    out.extend_from_slice(&wcb);
    out.extend_from_slice(digest.as_bytes());
    fs::write(path, out).expect("write index");
}

fn mutate_index(path: &Path, mutate: impl FnOnce(&mut BTreeMap<String, Value>)) {
    let mut value = read_index_value(path);
    let map = match &mut value {
        Value::Map(map) => map,
        other => panic!("index root must be a map, got {other:?}"),
    };
    mutate(map);
    write_index_value(path, &value);
}

#[test]
fn persisted_index_loads_on_reopen_without_rebuild() {
    let fixture = build_fixture();
    let head = fixture.store.head();
    let path = index_path(fixture.store.root(), head);
    assert!(path.exists(), "a commit writes its advisory index");

    let principal = principal();
    let limits = weftcodec::Limits::default();
    let query = selector::parse("select tag(\"alpha\")").unwrap();
    let live = fixture
        .store
        .resolve(&query, None, &principal, &limits)
        .unwrap();
    let scan = fixture
        .store
        .resolve_scan(&query, None, &principal, &limits)
        .unwrap();
    compare("live", &live, &scan);
    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);

    for (label, mode) in [
        ("read-only", OpenMode::ReadOnly),
        ("read-write", OpenMode::ReadWrite),
    ] {
        let store = Store::open(&root, mode).unwrap();
        store.reset_index_stats();
        let result = store.resolve(&query, None, &principal, &limits).unwrap();
        let scan = store
            .resolve_scan(&query, None, &principal, &limits)
            .unwrap();
        compare(label, &result, &scan);
        let stats = store.index_stats();
        assert_eq!(
            stats.disk_loads, 1,
            "{label}: first query must load the persisted index: {stats:?}"
        );
        assert_eq!(
            stats.builds, 0,
            "{label}: first query must not rebuild: {stats:?}"
        );
        assert_eq!(stats.rejects, 0, "{label}: no rejects: {stats:?}");
    }
}

#[test]
fn persisted_index_matches_scan_matrix_across_views() {
    let mut fixture = build_fixture();
    let pinned = fixture.store.snapshot_create("pinned").unwrap();
    // Move the head past the snapshot so the two views have distinct segments.
    let root = fixture.store.resolve_path("/").unwrap();
    fixture
        .store
        .create_file_seeded(
            root,
            b"after-snapshot",
            b"later",
            0o644,
            1000,
            1000,
            BTreeMap::from([("team".to_owned(), Value::Str("red".to_owned()))]),
            b"persist-after-snapshot",
        )
        .unwrap();
    let head = fixture.store.head();
    assert_ne!(head, pinned);

    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);
    let store = Store::open(&root, OpenMode::ReadOnly).unwrap();
    store.reset_index_stats();

    let checked = check_matrix(
        &store,
        &fixture.visible,
        &fixture.hidden,
        &fixture.files,
        None,
        "head",
    ) + check_matrix(
        &store,
        &fixture.visible,
        &fixture.hidden,
        &fixture.files,
        Some(pinned),
        "pinned",
    );
    assert_eq!(
        checked,
        selectors(&fixture.visible, &fixture.hidden, &fixture.files).len() * 2
    );
    let stats = store.index_stats();
    assert_eq!(
        stats.builds, 0,
        "both views must load their persisted index: {stats:?}"
    );
    assert_eq!(stats.disk_loads, 2, "one segment per view: {stats:?}");
    assert_eq!(stats.rejects, 0, "{stats:?}");
}

#[test]
fn corrupt_index_rebuilds_and_results_are_unchanged() {
    enum Corruption {
        Truncate,
        FlipPayloadByte,
        Generation,
        Commit,
        ObjectCount,
        Schema,
    }

    let cases = [
        (Corruption::Truncate, "truncate"),
        (Corruption::FlipPayloadByte, "payload byte flip"),
        (Corruption::Generation, "wrong generation"),
        (Corruption::Commit, "wrong commit key"),
        (Corruption::ObjectCount, "wrong object_count"),
        (Corruption::Schema, "unknown schema"),
    ];

    for (case, label) in cases {
        let fixture = build_fixture();
        let head = fixture.store.head();
        let path = index_path(fixture.store.root(), head);
        assert!(path.exists(), "{label}: index must exist before corruption");

        match case {
            Corruption::Truncate => {
                let bytes = fs::read(&path).unwrap();
                fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
            }
            Corruption::FlipPayloadByte => {
                let mut bytes = fs::read(&path).unwrap();
                // Offset 12 starts the canonical WCB; flip a payload byte and
                // leave the wrapper intact so the integrity digest must catch it.
                let offset = 12 + (bytes.len() - 12 - 32) / 2;
                bytes[offset] ^= 0x01;
                fs::write(&path, &bytes).unwrap();
            }
            Corruption::Generation => mutate_index(&path, |map| {
                map.insert("generation".to_owned(), Value::U64(0));
            }),
            Corruption::Commit => mutate_index(&path, |map| {
                map.insert("commit".to_owned(), Value::Bytes(vec![0xAB; 32]));
            }),
            Corruption::ObjectCount => mutate_index(&path, |map| {
                let current = match map.get("object_count") {
                    Some(Value::U64(value)) => *value,
                    _ => panic!("{label}: missing object_count"),
                };
                map.insert("object_count".to_owned(), Value::U64(current + 1));
            }),
            Corruption::Schema => mutate_index(&path, |map| {
                map.insert(
                    "schema".to_owned(),
                    Value::Str("weftfs.index.v99".to_owned()),
                );
            }),
        }

        let root = fixture.store.root().to_path_buf();
        drop(fixture.store);
        let store = Store::open(&root, OpenMode::ReadOnly).unwrap();
        store.reset_index_stats();
        let checked = check_matrix(
            &store,
            &fixture.visible,
            &fixture.hidden,
            &fixture.files,
            None,
            label,
        );
        assert_eq!(
            checked,
            selectors(&fixture.visible, &fixture.hidden, &fixture.files).len()
        );
        let stats = store.index_stats();
        assert!(
            stats.builds >= 1,
            "{label}: the corrupt index must be replaced by a rebuild: {stats:?}"
        );
        assert_eq!(
            stats.disk_loads, 0,
            "{label}: the corrupt index must never be used: {stats:?}"
        );
        assert!(
            stats.rejects >= 1,
            "{label}: a reject note/counter is required: {stats:?}"
        );
        assert!(
            stats
                .notes
                .iter()
                .any(|note| note.contains(&head.to_hex()) || note.contains("index")),
            "{label}: reject note should name the index: {stats:?}"
        );
    }
}

#[test]
fn reopen_after_new_commit_uses_the_new_head_index() {
    let mut fixture = build_fixture();
    let old_head = fixture.store.head();
    let principal = principal();
    let limits = weftcodec::Limits::default();
    let query = selector::parse("select attrs.team == \"zeta\"").unwrap();
    assert_eq!(
        fixture
            .store
            .resolve(&query, None, &principal, &limits)
            .unwrap()
            .matched_count,
        0
    );

    let root = fixture.store.resolve_path("/").unwrap();
    fixture
        .store
        .create_file_seeded(
            root,
            b"zeta",
            b"new-state",
            0o644,
            1000,
            1000,
            BTreeMap::from([("team".to_owned(), Value::Str("zeta".to_owned()))]),
            b"persist-zeta",
        )
        .unwrap();
    let new_head = fixture.store.head();
    assert_ne!(old_head, new_head);
    let index_root = fixture.store.root().to_path_buf();
    assert!(index_path(&index_root, new_head).exists());
    assert!(index_path(&index_root, old_head).exists());
    drop(fixture.store);

    let store = Store::open(&index_root, OpenMode::ReadOnly).unwrap();
    store.reset_index_stats();
    let result = store.resolve(&query, None, &principal, &limits).unwrap();
    let scan = store
        .resolve_scan(&query, None, &principal, &limits)
        .unwrap();
    compare("new head", &result, &scan);
    assert_eq!(result.matched_count, 1, "new state must be visible");
    let stats = store.index_stats();
    assert_eq!(stats.disk_loads, 1, "{stats:?}");
    assert_eq!(stats.builds, 0, "{stats:?}");
    assert_eq!(stats.rejects, 0, "{stats:?}");
}

#[test]
fn gc_removes_only_unreachable_indexes() {
    let mut fixture = build_fixture();
    let pinned = fixture.store.snapshot_create("pinned").unwrap();
    let root = fixture.store.resolve_path("/").unwrap();
    fixture
        .store
        .create_file_seeded(
            root,
            b"after-pin",
            b"later",
            0o644,
            1000,
            1000,
            BTreeMap::new(),
            b"persist-after-pin",
        )
        .unwrap();
    let intermediate = fixture.store.head();
    fixture.store.unlink(root, b"after-pin").unwrap();
    let head_before_gc = fixture.store.head();
    assert_ne!(intermediate, head_before_gc);

    let index_root = fixture.store.root().to_path_buf();
    let pinned_index = index_path(&index_root, pinned);
    let intermediate_index = index_path(&index_root, intermediate);
    let head_index = index_path(&index_root, head_before_gc);
    assert!(pinned_index.exists());
    assert!(intermediate_index.exists());
    assert!(head_index.exists());

    let report = fixture.store.gc(true).unwrap();
    assert!(
        !report.deleted_indexes.is_empty(),
        "gc must report deleted index segments: {report:?}"
    );
    assert!(
        pinned_index.exists(),
        "the snapshot root's index must be retained"
    );
    let current_head = fixture.store.head();
    assert!(
        index_path(&index_root, current_head).exists(),
        "the current head's index must be retained"
    );
    assert!(
        !intermediate_index.exists(),
        "an index for an unretained commit must be deleted"
    );

    // Deleting indexes never touches data: the matrix still resolves
    // identically for the head and the pinned snapshot.
    let checked = check_matrix(
        &fixture.store,
        &fixture.visible,
        &fixture.hidden,
        &fixture.files,
        None,
        "gc head",
    ) + check_matrix(
        &fixture.store,
        &fixture.visible,
        &fixture.hidden,
        &fixture.files,
        Some(pinned),
        "gc pinned",
    );
    assert_eq!(
        checked,
        selectors(&fixture.visible, &fixture.hidden, &fixture.files).len() * 2
    );
    let verify = fixture.store.verify(false).unwrap();
    assert!(verify.ok, "verify after index gc: {:?}", verify.problems);
}

#[test]
fn read_only_open_never_writes_or_creates_indexes() {
    let fixture = build_fixture();
    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);

    fn index_files(root: &Path) -> BTreeMap<PathBuf, SystemTime> {
        let mut out = BTreeMap::new();
        let indexes = root.join("indexes");
        let shards = match fs::read_dir(&indexes) {
            Ok(shards) => shards,
            Err(_) => return out,
        };
        for shard in shards {
            let shard = shard.unwrap();
            if !shard.file_type().unwrap().is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path()).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    let modified = entry.metadata().unwrap().modified().unwrap();
                    out.insert(entry.path(), modified);
                }
            }
        }
        out
    }

    let before = index_files(&root);
    assert!(!before.is_empty(), "fixture commits write indexes");
    {
        let store = Store::open(&root, OpenMode::ReadOnly).unwrap();
        store.reset_index_stats();
        check_matrix(
            &store,
            &fixture.visible,
            &fixture.hidden,
            &fixture.files,
            None,
            "read-only",
        );
        let stats = store.index_stats();
        assert!(stats.disk_loads >= 1, "{stats:?}");
        assert_eq!(stats.writes, 0, "read-only must not write: {stats:?}");
    }
    let after = index_files(&root);
    assert_eq!(before, after, "read-only open changed index files/mtimes");

    // Even with no `indexes/` directory at all, a read-only open rebuilds in
    // memory and must not recreate the directory.
    let bare = tempdir().unwrap();
    let fixture = build_fixture();
    let bare_root = bare.path().join("store");
    fs::rename(fixture.dir.path(), &bare_root).unwrap();
    drop(fixture.store);
    fs::remove_dir_all(bare_root.join("indexes")).unwrap();
    {
        let store = Store::open(&bare_root, OpenMode::ReadOnly).unwrap();
        store.reset_index_stats();
        check_matrix(
            &store,
            &fixture.visible,
            &fixture.hidden,
            &fixture.files,
            None,
            "no-index-dir",
        );
        let stats = store.index_stats();
        assert!(stats.builds >= 1, "{stats:?}");
        assert_eq!(stats.writes, 0, "{stats:?}");
    }
    assert!(
        !bare_root.join("indexes").exists(),
        "read-only open must not create the index layout"
    );
}
