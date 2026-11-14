//! T026: the store's history channel emits the record shapes the independent
//! checker consumes, and emits nothing when it is disabled.

use std::collections::BTreeMap;
use std::fs;

use tempfile::TempDir;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options(history: Option<&std::path::Path>) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: WriterMode::Optimistic,
        checkpoint_interval: 0,
        history_path: history.map(|path| path.display().to_string()),
        ..Options::default()
    }
}

fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_transaction_and_a_read_are_recorded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store");
    let history = dir.path().join("history.jsonl");
    drop(
        Store::init(
            &path,
            Options {
                fixed_timestamp_ns: Some(NOW),
                ..Options::default()
            },
        )
        .unwrap(),
    );

    let mut store =
        Store::open_with_options(&path, OpenMode::ReadWrite, options(Some(&history))).unwrap();
    let (manifest, _, size) = store.prepare_content(b"history").unwrap();
    let object_id = weftfs::identity::object_id_from_seed(b"history-seed");
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
        id: derive_entry_id(State::ROOT_ID, b"history.bin").unwrap(),
        parent: State::ROOT_ID,
        name: b"history.bin".to_vec(),
        target: object_id,
    };
    let mut tx = store
        .begin_transaction("00000000000000000000000000000a01")
        .unwrap();
    {
        let state = store.current_state();
        tx.put_object_create(record).unwrap();
        tx.put_entry_create(entry).unwrap();
        tx.set_tip(state, object_id, version).unwrap();
    }
    store
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap();
    let id = store.lookup(State::ROOT_ID, b"history.bin").unwrap();
    let bytes = store.read_object(id).unwrap();
    assert_eq!(bytes, b"history");

    let all = records(&history);
    let events: Vec<&str> = all
        .iter()
        .map(|record| record["event"].as_str().unwrap())
        .collect();
    assert!(events.contains(&"tx-invoke"), "{events:?}");
    assert!(events.contains(&"tx-response"), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "read-response")
            .count(),
        2,
        "one lookup and one object read: {events:?}"
    );
    let response = all
        .iter()
        .find(|record| record["event"] == "tx-response")
        .unwrap();
    assert_eq!(response["outcome"], "committed");
    assert_eq!(response["generation"], 2);
    let invoke = all
        .iter()
        .find(|record| record["event"] == "tx-invoke")
        .unwrap();
    let keys: Vec<&str> = invoke["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["key"].as_str().unwrap())
        .collect();
    assert!(keys[0].starts_with("object:"), "{keys:?}");
    assert!(keys[1].starts_with("entry:"), "{keys:?}");
    assert!(keys[2].starts_with("tip:"), "{keys:?}");
}

#[test]
fn no_history_file_appears_when_the_option_is_absent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store");
    let history = dir.path().join("history.jsonl");
    let single = Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: WriterMode::Single,
        ..Options::default()
    };
    drop(Store::init(&path, single).unwrap());
    let mut store = Store::open_with_options(
        &path,
        OpenMode::ReadWrite,
        Options {
            fixed_timestamp_ns: Some(NOW),
            writer_mode: WriterMode::Optimistic,
            ..Options::default()
        },
    )
    .unwrap();
    let _ = store.prepare_content(b"unrecorded").unwrap();
    drop(store);
    assert!(!history.exists());
}
