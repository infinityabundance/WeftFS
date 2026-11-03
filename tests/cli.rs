//! CLI contract tests: verbs run end-to-end and `--json` keeps the required
//! identity fields separate.

use std::fs;
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
fn cli_context_causal_capability_end_to_end() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["init"]);
    ok(
        &store,
        &[
            "create",
            "/parent.txt",
            "--text",
            "parent",
            "--attr",
            "project=demo",
        ],
    );
    let parent = json(
        &store,
        &[
            "--json",
            "create",
            "/child.txt",
            "--text",
            "child",
            "--attr",
            "project=demo",
        ],
    );
    let _ = parent;
    let parent_id = json(&store, &["--json", "stat", "/parent.txt"])["object_id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &store,
        &[
            "write",
            "/child.txt",
            "--text",
            "CHILD",
            "--causal",
            &parent_id,
        ],
    );
    let causal = json(&store, &["--json", "causal", "show", "/child.txt"]);
    assert_eq!(causal["identity_mode"], "causal_exact");
    assert_eq!(causal["parents"].as_array().unwrap().len(), 1);
    assert_eq!(causal["parents"][0].as_str().unwrap().len(), 64);

    let snapshot = json(&store, &["--json", "snapshot", "create", "s1"]);
    let snapshot_id = snapshot["snapshot_id"].as_str().unwrap().to_owned();
    let created = json(
        &store,
        &[
            "--json",
            "context",
            "create",
            "--name",
            "demo",
            "--selector",
            "select attrs.project == \"demo\"",
            "--snapshot",
            "s1",
        ],
    );
    assert_eq!(created["snapshot_id"], snapshot_id);
    assert_eq!(created["context_root_id"].as_str().unwrap().len(), 64);
    let uri = created["uri"].as_str().unwrap().to_owned();

    let list = json(&store, &["--json", "context", "list"]);
    assert_eq!(list["contexts"].as_array().unwrap().len(), 1);
    let show = json(&store, &["--json", "context", "show", &uri]);
    assert_eq!(show["context_root_id"], created["context_root_id"]);

    let resolved = json(&store, &["--json", "context", "resolve", "demo"]);
    assert_eq!(resolved["matched_count"], 2);
    assert_eq!(resolved["returned"], 2);
    assert_eq!(resolved["context_root_id"], created["context_root_id"]);
    assert_eq!(resolved["snapshot_id"], snapshot_id);
    assert_eq!(resolved["selector_id"], created["selector_id"]);

    let out = dir.path().join("materialized");
    let report = json(
        &store,
        &[
            "--json",
            "context",
            "materialize",
            "demo",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(report["files"], 2);
    assert!(out.join("parent.txt").exists());
    assert!(out.join(".weft-context.json").exists());

    // Capability file (JSON) binds a narrowed grant.
    let capability_path = dir.path().join("cap.json");
    fs::write(
        &capability_path,
        r#"{
          "schema": "weft.capability.v1",
          "principal": "agent",
          "namespaces": ["default"],
          "predicates": ["tagged_as"],
          "budget": {"max_results": 10}
        }"#,
    )
    .unwrap();
    let bound = json(
        &store,
        &[
            "--json",
            "context",
            "create",
            "--name",
            "bound",
            "--selector",
            "select attrs.project == \"demo\"",
            "--snapshot",
            "s1",
            "--capability",
            capability_path.to_str().unwrap(),
        ],
    );
    assert!(bound["capability_hash"].as_str().unwrap().len() == 64);
    let resolved = json(
        &store,
        &[
            "--json",
            "context",
            "resolve",
            "bound",
            "--capability",
            capability_path.to_str().unwrap(),
        ],
    );
    assert_eq!(resolved["matched_count"], 2);
    assert_eq!(resolved["capability_hash"], bound["capability_hash"]);

    // A budget overrun is an error, never silent truncation.
    ok(
        &store,
        &[
            "context",
            "create",
            "--name",
            "limited",
            "--selector",
            "select attrs.project == \"demo\"",
            "--snapshot",
            "s1",
            "--max-results",
            "1",
        ],
    );
    let output = run(&store, &["--json", "context", "resolve", "limited"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("LimitExceeded"));

    // URI canonicalization over the store name.
    let canonical = json(&store, &["--json", "uri", "canonicalize", &uri]);
    assert_eq!(canonical["uri"], uri);
    let shown = json(&store, &["--json", "uri", "show", "demo"]);
    assert_eq!(shown["uri"], uri);
}

#[test]
fn cli_conformance_store_fixture_is_deterministic() {
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
