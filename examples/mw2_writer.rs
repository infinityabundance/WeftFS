//! MW2 multi-writer court driver (subphase R4).
//!
//! Modes:
//!   init   --store DIR [--fixed-timestamp-ns N]
//!   parity --store DIR --name NAME --content TEXT --seed SEED [--fixed-timestamp-ns N]
//!   write  --store DIR --writer ID --count N --prefix P [--hot F] [--fixed-timestamp-ns N]
//!   read   --store DIR --name NAME
//!
//! `parity` prints one JSON object. `write` prints one JSON summary. No mode
//! inspects a private field to declare success: every success is a completed
//! transaction or a read of published state.

use std::collections::BTreeMap;
use std::time::Instant;

use weftfs::identity::{self, Hash32};
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{OpenMode, Options, Store};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn fixed_ns(args: &[String]) -> i64 {
    arg(args, "--fixed-timestamp-ns")
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(1_700_000_000_000_000_000)
}

fn options(mode: WriterMode, now: i64) -> Options {
    Options {
        fixed_timestamp_ns: Some(now),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn hex(hash: Hash32) -> String {
    hash.to_hex()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("help");
    let store_path = arg(&args, "--store").unwrap_or_else(|| ".".to_owned());
    let now = fixed_ns(&args);
    match mode {
        "init" => {
            let store = Store::init(&store_path, options(WriterMode::Single, now)).expect("init");
            drop(store);
            println!("{{\"ok\":true}}");
        }
        "parity" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "parity.bin".to_owned());
            let content = arg(&args, "--content").unwrap_or_else(|| "parity".to_owned());
            let seed = arg(&args, "--seed").unwrap_or_else(|| "seed".to_owned());
            if !std::path::Path::new(&store_path).join("FORMAT").exists() {
                let store =
                    Store::init(&store_path, options(WriterMode::Single, now)).expect("init");
                drop(store);
            }
            let mut store = Store::open_with_options(
                &store_path,
                OpenMode::ReadWrite,
                options(WriterMode::Optimistic, now),
            )
            .expect("open optimistic");
            let (manifest, content_hash, size) =
                store.prepare_content(content.as_bytes()).expect("content");
            let object_id = identity::object_id_from_seed(seed.as_bytes());
            let version = derive_version_id(object_id, manifest, size, now, None).expect("version");
            let record = ObjectRecord {
                id: object_id,
                kind: ObjectKind::File,
                mode: 0o644,
                uid: 0,
                gid: 0,
                mtime_ns: now,
                version,
                content: manifest,
                size,
                attrs: BTreeMap::new(),
            };
            let entry = EntryRecord {
                id: derive_entry_id(State::ROOT_ID, name.as_bytes()).expect("entry id"),
                parent: State::ROOT_ID,
                name: name.as_bytes().to_vec(),
                target: object_id,
            };
            let tx_id = "00000000000000000000000000000001";
            let mut tx = store.begin_transaction(tx_id).expect("begin");
            {
                let state = store.current_state();
                tx.put_object_create(record.clone()).expect("put object");
                tx.put_entry_create(entry.clone()).expect("put entry");
                tx.set_tip(state, object_id, version).expect("tip");
            }
            let outcome = store
                .execute_transaction(&tx, &TransactionOptions::default())
                .expect("execute");
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "head": hex(outcome.commit),
                    "generation": outcome.generation,
                    "manifest": hex(manifest),
                    "content_root": hex(content_hash),
                    "object_id": hex(object_id),
                    "entry_id": hex(entry.id),
                    "version": hex(version),
                    "size": size,
                    "tx_id": tx_id,
                })
            );
        }
        "write" => {
            let writer = arg(&args, "--writer").unwrap_or_else(|| "w0".to_owned());
            let count: u64 = arg(&args, "--count")
                .and_then(|text| text.parse().ok())
                .unwrap_or(10);
            let prefix = arg(&args, "--prefix").unwrap_or_else(|| writer.clone());
            let hot = arg(&args, "--hot");
            let explicit_tx = arg(&args, "--tx-id");
            let pause_ms: u64 = arg(&args, "--pause-before-execute-ms")
                .and_then(|text| text.parse().ok())
                .unwrap_or(0);
            let mut store = Store::open_with_options(
                &store_path,
                OpenMode::ReadWrite,
                options(WriterMode::Optimistic, now),
            )
            .expect("open optimistic");
            let started = Instant::now();
            let mut committed = 0u64;
            let mut conflicts = 0u64;
            let mut attempts = 0u64;
            let mut errors = Vec::new();
            for index in 0..count {
                let name = match &hot {
                    Some(name) => name.clone(),
                    None => format!("{prefix}-{index:06}"),
                };
                let content = format!("{writer}:{index}");
                let seed = format!("{writer}:{index}:seed");
                let (manifest, _, size) =
                    store.prepare_content(content.as_bytes()).expect("content");
                let object_id = identity::object_id_from_seed(seed.as_bytes());
                let version =
                    derive_version_id(object_id, manifest, size, now, None).expect("version");
                let record = ObjectRecord {
                    id: object_id,
                    kind: ObjectKind::File,
                    mode: 0o644,
                    uid: 0,
                    gid: 0,
                    mtime_ns: now,
                    version,
                    content: manifest,
                    size,
                    attrs: BTreeMap::new(),
                };
                let entry = EntryRecord {
                    id: derive_entry_id(State::ROOT_ID, name.as_bytes()).expect("entry id"),
                    parent: State::ROOT_ID,
                    name: name.as_bytes().to_vec(),
                    target: object_id,
                };
                let tx_id = match (&explicit_tx, count) {
                    (Some(id), 1) => id.clone(),
                    _ => identity::h("weft/tx/id/v1", format!("{writer}:{index}").as_bytes())
                        .to_hex()[..32]
                        .to_owned(),
                };
                let mut tx = store.begin_transaction(tx_id).expect("begin");
                {
                    let state = store.current_state();
                    tx.put_object_create(record.clone()).expect("put object");
                    tx.put_entry_create(entry.clone()).expect("put entry");
                    tx.set_tip(state, object_id, version).expect("tip");
                }
                if pause_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(pause_ms));
                }
                let options = TransactionOptions {
                    max_attempts: 64,
                    ..TransactionOptions::default()
                };
                let mut outcome = store.execute_transaction(&tx, &options);
                let mut outer = 0u32;
                while let Err(err) = &outcome {
                    if err.class() != weftfs::error::Class::Busy || outer >= 3 {
                        break;
                    }
                    // An `outcome_unknown` is resolved by retrying the same
                    // transaction id; the outcome lookup runs first.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    outer += 1;
                    outcome = store.execute_transaction(&tx, &options);
                }
                match outcome {
                    Ok(outcome) => {
                        committed += 1;
                        attempts += u64::from(outcome.attempts) + u64::from(outer);
                    }
                    Err(err) if err.class() == weftfs::error::Class::Conflict => {
                        conflicts += 1;
                    }
                    Err(err) => errors.push(err.to_string()),
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "ok": errors.is_empty(),
                    "writer": writer,
                    "committed": committed,
                    "conflicts": conflicts,
                    "attempts": attempts,
                    "errors": errors,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                })
            );
        }
        "outcome" => {
            let tx_id = arg(&args, "--tx-id").expect("--tx-id");
            let mut store =
                Store::open_with_options(&store_path, OpenMode::ReadWrite, options(WriterMode::Optimistic, now))
                    .expect("open optimistic");
            let lookup = store.transaction_outcome(&tx_id).expect("lookup");
            let (state, commit) = match lookup {
                weftfs::mw2::OutcomeLookup::Committed(outcome) => {
                    ("committed", Some(hex(outcome.commit)))
                }
                weftfs::mw2::OutcomeLookup::Unknown => ("unknown", None),
                weftfs::mw2::OutcomeLookup::Absent => ("absent", None),
            };
            let _ = &mut store;
            println!(
                "{}",
                serde_json::json!({"ok": true, "state": state, "commit": commit})
            );
        }
        "verify" => {
            let writer = arg(&args, "--writer").unwrap_or_else(|| "w0".to_owned());
            let count: u64 = arg(&args, "--count")
                .and_then(|text| text.parse().ok())
                .unwrap_or(10);
            let prefix = arg(&args, "--prefix").unwrap_or_else(|| writer.clone());
            let store = Store::open(&store_path, OpenMode::ReadOnly).expect("open");
            let mut mismatches = Vec::new();
            let mut checked = 0u64;
            for index in 0..count {
                let name = format!("{prefix}-{index:06}");
                let expected = format!("{writer}:{index}");
                match store.lookup(State::ROOT_ID, name.as_bytes()) {
                    Ok(id) => match store.read_object(id) {
                        Ok(bytes) => {
                            checked += 1;
                            if bytes != expected.as_bytes() {
                                mismatches.push(format!("{name}: content mismatch"));
                            }
                        }
                        Err(err) => mismatches.push(format!("{name}: {err}")),
                    },
                    Err(err) => mismatches.push(format!("{name}: {err}")),
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "ok": mismatches.is_empty(),
                    "checked": checked,
                    "mismatches": mismatches,
                })
            );
        }
        "read" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "parity.bin".to_owned());
            let store = Store::open(&store_path, OpenMode::ReadOnly).expect("open");
            let id = store
                .lookup(State::ROOT_ID, name.as_bytes())
                .expect("lookup");
            let bytes = store.read_object(id).expect("read");
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "object_id": hex(id),
                    "len": bytes.len(),
                    "content_hash": hex(identity::content_hash(&bytes)),
                })
            );
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}
