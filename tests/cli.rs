//! CLI contract tests: verbs run end-to-end and `--json` keeps the required
//! identity fields separate.

use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

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

fn json(store: &std::path::Path, args: &[&str]) -> Value {
    let output = ok(store, args);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid JSON from {args:?}: {err}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn cli_verbs_and_query_json_contract() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["init"]);
    ok(&store, &["mkdir", "/dir"]);
    ok(&store, &["create", "/a.txt", "--text", "hello"]);
    ok(&store, &["create", "/dir/b.txt", "--text", "world"]);
    ok(&store, &["symlink", "a.txt", "/link"]);
    ok(&store, &["link", "/a.txt", "/hard"]);

    let listing = json(&store, &["--json", "ls", "/"]);
    let names: Vec<String> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, vec!["a.txt", "dir", "hard", "link"]);

    let stat = json(&store, &["stat", "/dir/b.txt", "--json"]);
    assert_eq!(stat["kind"], "file");
    assert_eq!(stat["size"], 5);

    let query = json(&store, &["--json", "query", "select kind == \"file\""]);
    assert_eq!(query["matched_count"], 2);
    assert_eq!(query["selector_id"].as_str().unwrap().len(), 64);
    assert_eq!(query["snapshot_id"].as_str().unwrap().len(), 64);
    for result in query["results"].as_array().unwrap() {
        assert_eq!(result["object_id"].as_str().unwrap().len(), 64);
        assert_eq!(result["version_id"].as_str().unwrap().len(), 64);
        assert_eq!(result["content_id"].as_str().unwrap().len(), 64);
        assert!(result["size"].is_u64());
        assert!(result["kind"].is_string());
        assert!(result["path"].is_string());
    }
    // The three identities are distinct fields, never conflated.
    let first = &query["results"][0];
    assert_ne!(first["object_id"], query["selector_id"]);
    assert_ne!(first["object_id"], first["content_id"]);

    let snapshot = json(&store, &["--json", "snapshot", "create", "v1"]);
    let snapshot_id = snapshot["snapshot_id"].as_str().unwrap().to_owned();
    ok(&store, &["create", "/later.txt", "--text", "later"]);

    let pinned = json(
        &store,
        &[
            "--json",
            "query",
            "select kind == \"file\"",
            "--snapshot",
            "v1",
        ],
    );
    assert_eq!(pinned["matched_count"], 2);
    assert_eq!(pinned["snapshot_id"], snapshot_id);

    let snapshots = json(&store, &["--json", "snapshot", "list"]);
    assert_eq!(snapshots["snapshots"]["v1"], snapshot_id);

    let relation = json(
        &store,
        &[
            "--json",
            "relation",
            "add",
            "--subject",
            "/a.txt",
            "--predicate",
            "tagged_as",
            "--value",
            "greeting",
        ],
    );
    let relation_id = relation["relation_id"].as_str().unwrap().to_owned();
    let relations = json(&store, &["--json", "relation", "list"]);
    assert_eq!(relations["relations"].as_array().unwrap().len(), 1);
    ok(&store, &["relation", "delete", &relation_id]);

    let verify = json(&store, &["--json", "verify", "--full"]);
    assert_eq!(verify["ok"], true, "verify: {verify}");
    let gc = json(&store, &["--json", "gc"]);
    assert_eq!(gc["applied"], false);
    let checkpoint = json(&store, &["--json", "checkpoint"]);
    assert!(checkpoint["generation"].as_u64().unwrap() > 0);

    let info = json(&store, &["--json", "info"]);
    assert!(info["objects"].as_u64().unwrap() >= 5);
}

#[test]
fn cli_errors_print_normative_class() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["init"]);
    let output = run(&store, &["cat", "/missing"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NotFound:"),
        "expected NotFound class, got: {stderr}"
    );

    let output = run(&store, &["create", "/x", "--text", "x", "--mode", "9999"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("InvalidArgument:"),
        "expected InvalidArgument class, got: {stderr}"
    );
}

#[test]
fn cli_conformance_store_is_deterministic() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let one = first.path().join("store");
    let two = second.path().join("store");
    ok(&one, &["conformance-store", "--out", one.to_str().unwrap()]);
    ok(&two, &["conformance-store", "--out", two.to_str().unwrap()]);
    let expected_one: Value =
        serde_json::from_slice(&std::fs::read(one.join("EXPECTED.json")).unwrap()).unwrap();
    let expected_two: Value =
        serde_json::from_slice(&std::fs::read(two.join("EXPECTED.json")).unwrap()).unwrap();
    assert_eq!(expected_one, expected_two);
    assert_eq!(
        expected_one["generation"],
        serde_json::json!(first_generation(&one))
    );
}

fn first_generation(store: &std::path::Path) -> u64 {
    let value: Value =
        serde_json::from_slice(&std::fs::read(store.join("EXPECTED.json")).unwrap()).unwrap();
    value["generation"].as_u64().unwrap()
}

#[test]
fn cli_conformance_store_needs_no_store_flag() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("v1");
    let output = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .args(["--json", "conformance-store", "--out"])
        .arg(&out)
        .output()
        .expect("run weftfs");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["generation"], 11);
    assert_eq!(
        summary["snapshot_root"],
        "8959c65b7b5d821063e8785ed950a67214b555210bec45f65fd576053b0e1d81"
    );
    assert!(out.join("EXPECTED.json").exists());
}
