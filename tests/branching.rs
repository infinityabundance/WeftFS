//! Branching time-travel courts (P5a, part 2).
//!
//! Snapshot branches are refs to immutable commits, so writing after a branch
//! can never move the source. Merges are an explicit union: identical bytes at
//! the same path dedupe, different bytes fail closed with `Conflict` naming the
//! path. Context branches pin the fork commit and record `derived_from` lineage.

use std::collections::BTreeMap;
use std::process::{Command, Output};

use tempfile::tempdir;
use weftcodec::selector;
use weftcodec::Limits;

use weftfs::auth::Principal;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, Store};

const TS: i64 = 1_760_000_000_000_000_000;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 64,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(TS),
        ..Options::default()
    }
}

fn create(store: &mut Store, path: &str, body: &[u8]) -> Hash32 {
    let (parent, name) = split(path);
    let parent_id = store.resolve_path(&parent).unwrap();
    store
        .create_file(
            parent_id,
            name.as_bytes(),
            body,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap()
}

fn split(path: &str) -> (String, String) {
    let (parent, name) = path.rsplit_once('/').unwrap();
    let parent = if parent.is_empty() { "/" } else { parent };
    (parent.to_owned(), name.to_owned())
}

#[test]
fn snapshot_branch_pins_the_source_and_later_writes_do_not_move_it() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/history.txt", b"version-1");
    store.snapshot_create("s1").unwrap();
    for version in 2..=5u32 {
        let id = store.resolve_path("/history.txt").unwrap();
        store
            .write_file(id, 0, format!("version-{version}").as_bytes())
            .unwrap();
    }
    let source_commit = store.snapshot_create("s2").unwrap();
    let source_state = store.state_at(source_commit).unwrap();
    let source_id = source_state.lookup(Hash32::ZERO, b"history.txt").unwrap();
    let source_bytes = store
        .read_object_at(Some(source_commit), source_id)
        .unwrap();
    assert_eq!(source_bytes, b"version-5");

    // Branch s2 under a new name. It is the same immutable commit.
    let branch_commit = store.snapshot_branch("s2", "branch-a").unwrap();
    assert_eq!(branch_commit, source_commit);
    assert_eq!(
        store
            .snapshot_list()
            .iter()
            .find(|(name, _)| name == "branch-a"),
        Some(&("branch-a".to_owned(), source_commit))
    );

    // Write five more versions on the live head.
    for version in 6..=10u32 {
        let id = store.resolve_path("/history.txt").unwrap();
        store
            .write_file(id, 0, format!("version-{version}").as_bytes())
            .unwrap();
    }
    // Neither the source nor the branch moved.
    assert_eq!(
        store.snapshot_list().iter().find(|(name, _)| name == "s2"),
        Some(&("s2".to_owned(), source_commit))
    );
    assert_eq!(store.snapshot_root(), store.head());
    assert_eq!(
        store
            .read_object_at(Some(source_commit), source_id)
            .unwrap(),
        b"version-5"
    );
    assert_eq!(
        store
            .read_object_at(Some(branch_commit), source_id)
            .unwrap(),
        b"version-5"
    );
    let live = store.resolve_path("/history.txt").unwrap();
    assert_eq!(store.read_object(live).unwrap(), b"version-10");
    assert!(store.verify(true).unwrap().ok);

    // Reopening still sees both refs on the same commit.
    let reopened = Store::open_with_options(dir.path(), OpenMode::ReadOnly, options()).unwrap();
    let refs = reopened.snapshot_list();
    assert!(refs.contains(&("s2".to_owned(), source_commit)));
    assert!(refs.contains(&("branch-a".to_owned(), source_commit)));
    assert!(reopened.verify(true).unwrap().ok);
}

#[test]
fn merge_disjoint_branches_unions_paths() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/left.txt", b"left");
    let left = store.snapshot_create("left").unwrap();
    create(&mut store, "/right.txt", b"right");
    let right = store.snapshot_create("right").unwrap();
    assert_ne!(left, right);
    // Remove the right-only path from the head so the merge has real work.
    let root = store.resolve_path("/").unwrap();
    store.unlink(root, b"right.txt").unwrap();
    assert!(store.resolve_path("/right.txt").is_err());
    let before = store.generation();

    let report = store.merge_branches("left", "right").unwrap();
    assert_eq!(report.left, left);
    assert_eq!(report.right, right);
    assert_eq!(report.paths, 2);
    assert_eq!(report.added_objects, 1);
    assert_eq!(report.added_entries, 1);
    assert_eq!(report.generation, before + 1);
    assert_eq!(
        store
            .read_object(store.resolve_path("/right.txt").unwrap())
            .unwrap(),
        b"right"
    );
    assert_eq!(
        store
            .read_object(store.resolve_path("/left.txt").unwrap())
            .unwrap(),
        b"left"
    );
    assert!(store.verify(true).unwrap().ok);

    // Merging the same pair again is idempotent and publishes nothing.
    let generation = store.generation();
    let head = store.head();
    let repeat = store.merge_branches("left", "right").unwrap();
    assert_eq!(repeat.added_objects, 0);
    assert_eq!(repeat.added_entries, 0);
    assert_eq!(store.generation(), generation);
    assert_eq!(store.head(), head);
}

#[test]
fn merge_conflicting_path_fails_closed_and_names_the_path() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/conflict.txt", b"one");
    store.snapshot_create("one").unwrap();
    let id = store.resolve_path("/conflict.txt").unwrap();
    store.write_file(id, 0, b"two").unwrap();
    store.snapshot_create("two").unwrap();
    let generation = store.generation();
    let head = store.head();

    let err = store
        .merge_branches("one", "two")
        .expect_err("different content at the same path must conflict");
    assert_eq!(err.class(), Class::Conflict);
    assert!(err.message().contains("/conflict.txt"), "{}", err.message());
    assert_eq!(store.generation(), generation);
    assert_eq!(store.head(), head);
    assert_eq!(store.read_object(id).unwrap(), b"two");
}

#[test]
fn merge_refuses_to_overwrite_the_live_head() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/head.txt", b"branch");
    let branch = store.snapshot_create("branch").unwrap();
    let id = store.resolve_path("/head.txt").unwrap();
    store.write_file(id, 0, b"liveX!").unwrap();
    let err = store
        .merge_branches("branch", &branch.to_hex())
        .expect_err("the live path already has different content");
    assert_eq!(err.class(), Class::Conflict);
    assert!(err.message().contains("/head.txt"));
    assert_eq!(store.read_object(id).unwrap(), b"liveX!");
}

#[test]
fn merge_identical_path_content_dedupes_cleanly() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/shared.txt", b"same bytes");
    let left = store.snapshot_create("left").unwrap();
    create(&mut store, "/extra.txt", b"extra");
    let right = store.snapshot_create("right").unwrap();
    let report = store.merge_branches("left", "right").unwrap();
    assert_eq!(report.paths, 2);
    assert_eq!(report.added_objects, 0);
    assert_eq!(report.added_entries, 0);
    assert_eq!(report.head, store.head());
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn context_branch_pins_the_fork_and_records_lineage() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    store.mkdir(root, b"logs", 0o755, 0, 0).unwrap();
    let logs = store.resolve_path("/logs").unwrap();
    let mut agent_attrs = BTreeMap::new();
    agent_attrs.insert(
        "task".to_owned(),
        weftcodec::wcb::Value::Str("agent-43".to_owned()),
    );
    for index in 0..3u32 {
        store
            .create_file(
                logs,
                format!("a{index}.txt").as_bytes(),
                format!("agent-43 line {index}").as_bytes(),
                0o644,
                0,
                0,
                agent_attrs.clone(),
            )
            .unwrap();
    }
    store
        .create_file(
            logs,
            b"other.txt",
            b"someone else",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let fork = store.snapshot_create("s-fork").unwrap();

    let parsed = selector::parse("select attrs.task == \"agent-43\"").unwrap();
    let source_root = weftcodec::context::ContextRoot::new(
        &parsed,
        fork,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("by:task=agent-43-base", &source_root, None, false)
        .unwrap();

    let branch = store
        .branch_context_root(
            "by:task=agent-43-base",
            "by:task=agent-43",
            Some("agent-43"),
            Some("qwen-test"),
            false,
        )
        .unwrap();
    assert_eq!(
        branch.root.snapshot, fork,
        "the branch pins the fork commit"
    );

    // Lineage relation is present and names both contexts.
    let lineage: Vec<_> = store
        .current_state()
        .relations
        .values()
        .filter(|relation| relation.predicate == "derived_from")
        .collect();
    assert_eq!(lineage.len(), 1);
    match &lineage[0].target {
        weftfs::state::RelationTarget::Value(weftcodec::wcb::Value::Str(name)) => {
            assert_eq!(name, "by:task=agent-43-base");
        }
        other => panic!("unexpected lineage target {other:?}"),
    }
    assert_eq!(
        lineage[0]
            .attrs
            .get("weft.context")
            .and_then(|value| match value {
                weftcodec::wcb::Value::Str(text) => Some(text.as_str()),
                _ => None,
            }),
        Some("by:task=agent-43")
    );

    // The fork view sees the three agent-43 files; the branch selector also
    // requires the model attribute, which these objects do not have.
    let resolved = store
        .resolve(&parsed, Some(fork), &Principal::root(), &Limits::default())
        .unwrap();
    assert_eq!(resolved.results.len(), 3);
    let branch_selector = branch.root.selector().unwrap();
    let narrowed = store
        .resolve(
            &branch_selector,
            Some(fork),
            &Principal::root(),
            &Limits::default(),
        )
        .unwrap();
    assert_eq!(narrowed.results.len(), 0, "model filter narrows the branch");

    // Writes after the branch never change the pinned view.
    let mut model_attrs = agent_attrs.clone();
    model_attrs.insert(
        "model".to_owned(),
        weftcodec::wcb::Value::Str("qwen-test".to_owned()),
    );
    store
        .create_file(logs, b"a3.txt", b"agent-43 later", 0o644, 0, 0, model_attrs)
        .unwrap();
    let after = store
        .resolve(
            &branch_selector,
            Some(fork),
            &Principal::root(),
            &Limits::default(),
        )
        .unwrap();
    assert_eq!(after.results.len(), 0, "the fork is immutable");
    let live = store
        .resolve(
            &branch_selector,
            None,
            &Principal::root(),
            &Limits::default(),
        )
        .unwrap();
    assert_eq!(live.results.len(), 1, "the live head sees the new write");
    assert!(store.verify(true).unwrap().ok);
}

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

fn run(store: &std::path::Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
    command.arg("--store").arg(store);
    command.args(args);
    command.output().expect("run weftfs")
}

fn ok(store: &std::path::Path, args: &[&str]) -> Output {
    let output = run(store, args);
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn cli_branch_and_merge_surface() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["init"]);
    ok(&store, &["create", "/one.txt", "--text", "one"]);
    ok(&store, &["snapshot", "create", "s1"]);
    let branch = ok(&store, &["--json", "snapshot", "branch", "s1", "b1"]);
    let branch_json: serde_json::Value = serde_json::from_slice(&branch.stdout).unwrap();
    assert_eq!(branch_json["name"], "b1");
    ok(&store, &["create", "/two.txt", "--text", "two"]);
    ok(&store, &["snapshot", "create", "s2"]);

    let merged = ok(&store, &["--json", "merge", "b1", "s2"]);
    let report: serde_json::Value = serde_json::from_slice(&merged.stdout).unwrap();
    assert_eq!(report["paths"], 2);
    assert_eq!(report["added_objects"], 0);

    // Overlapping different content is a Conflict naming the path.
    ok(&store, &["write", "/two.txt", "--text", "changed"]);
    ok(&store, &["snapshot", "create", "s3"]);
    let output = run(&store, &["merge", "s2", "s3"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Conflict"), "stderr: {stderr}");
    assert!(stderr.contains("/two.txt"), "stderr: {stderr}");
}

#[test]
fn cli_context_branch_mounts_and_lists_lineage() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["init"]);
    ok(
        &store,
        &["create", "/a.txt", "--text", "a", "--attr", "task=agent-9"],
    );
    ok(&store, &["snapshot", "create", "fork"]);
    ok(
        &store,
        &[
            "context",
            "create",
            "--name",
            "base",
            "--selector",
            "select attrs.task == \"agent-9\"",
            "--snapshot",
            "fork",
        ],
    );
    let output = ok(
        &store,
        &[
            "--json",
            "context",
            "branch",
            "base",
            "by:task=agent-9",
            "--task",
            "agent-9",
        ],
    );
    let branch: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(branch["derived_from"], "base");
    let fork_hex = branch["snapshot_id"].as_str().unwrap().to_owned();
    let listed = ok(&store, &["context", "list"]);
    let text = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(text.contains("by:task=agent-9"));
    assert!(!fork_hex.is_empty());
}

// ---------------------------------------------------------------------------
// Cross-language: a Go binary, when present, must read and verify the branch.
// ---------------------------------------------------------------------------

fn go_binary() -> Option<String> {
    let candidates = [
        std::env::var("WEFTFS_GO_BIN").unwrap_or_default(),
        "../../.docker-cache/target-go/weftfs".to_owned(),
        "../.docker-cache/target-go/weftfs".to_owned(),
        "/work/.docker-cache/target-go/weftfs".to_owned(),
    ];
    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        if let Ok(info) = std::fs::metadata(&candidate) {
            if info.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[test]
fn go_reads_and_verifies_a_rust_branch() {
    let Some(go) = go_binary() else {
        eprintln!("Go binary not found; cross-language branch court skipped");
        return;
    };
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    create(&mut store, "/history.txt", b"v1");
    store.snapshot_create("s1").unwrap();
    let id = store.resolve_path("/history.txt").unwrap();
    store.write_file(id, 0, b"v2").unwrap();
    store.snapshot_create("s2").unwrap();
    store.snapshot_branch("s2", "branch-go").unwrap();
    drop(store);

    let verify = Command::new(&go)
        .arg("--store")
        .arg(dir.path())
        .arg("verify")
        .arg("--full")
        .output()
        .expect("run go verify");
    assert!(
        verify.status.success(),
        "go verify failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("ok=true"));

    let snapshots = Command::new(&go)
        .arg("--store")
        .arg(dir.path())
        .arg("--json")
        .arg("snapshot")
        .arg("list")
        .output()
        .expect("run go snapshot list");
    assert!(snapshots.status.success());
    let text = String::from_utf8_lossy(&snapshots.stdout).into_owned();
    assert!(text.contains("branch-go"), "go snapshot list: {text}");

    let listed = Command::new(&go)
        .arg("--store")
        .arg(dir.path())
        .arg("--json")
        .arg("query")
        .arg("select kind == \"file\"")
        .arg("--snapshot")
        .arg("branch-go")
        .output()
        .expect("run go query");
    assert!(
        listed.status.success(),
        "go query at branch failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let text = String::from_utf8_lossy(&listed.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("go query JSON");
    assert_eq!(parsed["matched_count"], 1, "go query: {text}");
}
