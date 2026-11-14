//! MW2 multi-writer court driver (subphase R4; extended by X13/N14).
//!
//! Modes:
//!   init        --store DIR [--backend local|object|s3|azure] [--fixed-timestamp-ns N]
//!   parity      --store DIR --name NAME --content TEXT --seed SEED
//!   write       --store DIR --writer ID --count N --prefix P [--hot F]
//!   outcome     --store DIR --tx-id ID      transaction_outcome lookup
//!   verify      --store DIR --writer ID --count N --prefix P   named-file check
//!   verify-full --store DIR                whole-store VerifyReport
//!   read        --store DIR --name NAME
//!   stat        --store DIR --name NAME    presence, length and content hash
//!   head        --store DIR                head, superblock digest, blob counts
//!   snapshot    --store DIR --name NAME    commit the current head as a snapshot
//!   gc          --store DIR [--apply]      mark/sweep (writer-mode optimistic)
//!   single      --store DIR --name NAME --content TEXT --seed SEED   one MW1 commit
//!   hold-read   --store DIR --name NAME --ready FILE --gate FILE    stale-reader probe
//!   open-check  --store DIR                single and optimistic writable opens
//!
//! `parity` prints one JSON object. `write` prints one JSON summary. No mode
//! inspects a private field to declare success: every success is a completed
//! transaction or a read of published state.
//!
//! The Azure ambiguous-success fault point is `WEFT_AZURE_AMBIGUOUS_READY`: when
//! that path appears, this process sends itself `SIGKILL`. The path is written by
//! the court's forwarding proxy only after the conditional head `PUT` has been
//! applied upstream and before its response is returned, so the writer dies
//! exactly between issuing the conditional write and consuming the answer. The
//! hook is inert unless the variable is set. It lives in the court driver, not
//! the library, because the library needs no change to produce this fault.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

use sha2::{Digest, Sha256};

use weftfs::backend::{BackendKind, HeadExpectation, Key as BackendKey, KeyKind, StorageBackend};
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

fn backend(args: &[String]) -> String {
    arg(args, "--backend").unwrap_or_else(|| "local".to_owned())
}

fn azure_container(args: &[String]) -> String {
    arg(args, "--container")
        .or_else(|| std::env::var("WEFT_AZURE_TEST_CONTAINER").ok())
        .unwrap_or_default()
}

fn azure_config(args: &[String]) -> weftfs::azure::AzureConfig {
    let key = std::env::var("WEFT_AZURE_KEY").unwrap_or_default();
    let lease = arg(args, "--lock-lease-secs")
        .or_else(|| std::env::var("WEFT_AZURE_LOCK_LEASE_SECS").ok())
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(60)
        .max(1);
    weftfs::azure::AzureConfig {
        endpoint: arg(args, "--endpoint")
            .or_else(|| std::env::var("WEFT_AZURE_TEST_ENDPOINT").ok())
            .or_else(|| std::env::var("WEFT_AZURE_ENDPOINT").ok())
            .unwrap_or_default(),
        container: azure_container(args),
        account: std::env::var("WEFT_AZURE_ACCOUNT").unwrap_or_default(),
        key: if key.is_empty() {
            None
        } else {
            Some(key.clone())
        },
        sas_token: None,
        anonymous: key.is_empty(),
        lock_lease: std::time::Duration::from_secs(lease),
        request_timeout: std::time::Duration::from_secs(60),
    }
}

fn backend_kind(args: &[String], name: &str) -> BackendKind {
    match name {
        "s3" => {
            let config = weftfs::s3::S3Config::from_env().expect("S3 config from environment");
            BackendKind::S3 { config }
        }
        "object" => BackendKind::Object,
        "azure" => {
            let container = azure_container(args);
            assert!(
                !container.is_empty(),
                "--container or WEFT_AZURE_TEST_CONTAINER is required for azure"
            );
            BackendKind::Azure {
                config: azure_config(args),
            }
        }
        _ => BackendKind::Local,
    }
}

fn open_store(
    args: &[String],
    path: &str,
    mode: OpenMode,
    writer_mode: WriterMode,
    now: i64,
) -> Store {
    let opts = options(writer_mode, now);
    Store::open_with_options_and_backend(path, mode, opts, backend_kind(args, &backend(args)))
        .expect("open store")
}

fn init_store(args: &[String], path: &str, now: i64) {
    let opts = options(WriterMode::Single, now);
    Store::init_with_backend(path, opts, backend_kind(args, &backend(args))).expect("init store");
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The published head and the raw superblock digest, read from the plane (not
/// from the store's cache), so a court can prove a refused publication changed
/// nothing.
fn read_superblock(args: &[String], path: &str) -> Option<(u64, Hash32, Vec<u8>)> {
    let mut plane = backend_kind(args, &backend(args))
        .open(std::path::Path::new(path))
        .ok()?;
    let image = plane.read_all(&BackendKey::superblock()).ok()?;
    let slot = weftfs::format::decode_superblock(&image)
        .ok()?
        .into_iter()
        .flatten()
        .max_by_key(|slot| slot.generation)?;
    Some((slot.generation, slot.commit, image))
}

fn domain_arg(args: &[String]) -> Option<weftfs::Hash32> {
    arg(args, "--domain").and_then(|text| weftfs::Hash32::from_lower_hex(&text).ok())
}

fn open_driver(
    path: &str,
    mode: OpenMode,
    writer_mode: WriterMode,
    now: i64,
    backend: &str,
    args: &[String],
) -> Store {
    // `--domain` selects an MW3 conflict domain; the writer mode is then
    // `sharded`. Nothing is upgraded silently: the caller passes the domain.
    match domain_arg(args) {
        Some(domain) => {
            let opts = options(WriterMode::Sharded, now);
            Store::open_domain_with_options_and_backend(
                path,
                mode,
                opts,
                backend_kind(args, backend),
                domain,
            )
            .expect("open domain store")
        }
        None => {
            let opts = options(writer_mode, now);
            Store::open_with_options_and_backend(path, mode, opts, backend_kind(args, backend))
                .expect("open store")
        }
    }
}

fn list_count(args: &[String], path: &str, kind: KeyKind) -> u64 {
    let Ok(plane) = backend_kind(args, &backend(args)).open(std::path::Path::new(path)) else {
        return 0;
    };
    plane
        .list_prefix(&BackendKey::Prefix(kind))
        .map(|keys| keys.len() as u64)
        .unwrap_or(0)
}

/// Arm the ambiguous-success fault point.
///
/// The court's forwarding proxy creates `WEFT_AZURE_AMBIGUOUS_READY` only after
/// the conditional head `PUT` has been applied upstream and before its response
/// is returned. As soon as the path appears this process sends itself
/// `SIGKILL`, so it dies exactly between issuing the conditional write and
/// consuming the answer. No destructor runs and no outcome is recorded.
#[allow(unsafe_code)]
fn arm_ambiguous_exit() {
    if std::env::var_os("WEFT_AZURE_AMBIGUOUS_HEAD").is_none() {
        return;
    }
    let Some(ready) = std::env::var_os("WEFT_AZURE_AMBIGUOUS_READY") else {
        return;
    };
    let ready = std::path::PathBuf::from(ready);
    std::thread::spawn(move || {
        for _ in 0..6000 {
            if ready.exists() {
                unsafe { libc::raise(libc::SIGKILL) };
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
}

fn error_report(err: &weftfs::Error) -> serde_json::Value {
    serde_json::json!({"ok": false, "class": err.class().as_str(), "error": err.to_string()})
}

/// Read one named file from a held handle without refreshing its head.
fn read_named(store: &Store, name: &str) -> serde_json::Value {
    match store.lookup(State::ROOT_ID, name.as_bytes()) {
        Ok(id) => match store.read_object(id) {
            Ok(bytes) => serde_json::json!({
                "present": true,
                "object_id": hex(id),
                "len": bytes.len(),
                "content_hash": hex(identity::content_hash(&bytes)),
            }),
            Err(err) => serde_json::json!({
                "present": true,
                "error": err.to_string(),
                "class": err.class().as_str(),
            }),
        },
        Err(err) if err.class() == weftfs::error::Class::NotFound => {
            serde_json::json!({"present": false})
        }
        Err(err) => serde_json::json!({
            "present": false,
            "error": err.to_string(),
            "class": err.class().as_str(),
        }),
    }
}

fn init_driver(path: &str, now: i64, backend: &str, args: &[String]) {
    let _ = backend;
    init_store(args, path, now);
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

/// Approximate process CPU from `/proc/self/stat` ticks (USER_HZ=100 on Linux).
fn cpu_ms() -> (u64, u64) {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|text| {
            let fields: Vec<&str> = text.split_whitespace().collect();
            let user: u64 = fields.get(13)?.parse().ok()?;
            let sys: u64 = fields.get(14)?.parse().ok()?;
            Some((user * 10, sys * 10))
        })
        .unwrap_or((0, 0))
}

fn options(mode: WriterMode, now: i64) -> Options {
    let mut options = Options {
        fixed_timestamp_ns: Some(now),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    };
    // Court hook: a shorter sweep grace lets the GC scenario reclaim data it
    // has just made unreachable. Inert unless the court sets it.
    if let Ok(text) = std::env::var("WEFT_MW2_GC_GRACE_SECS") {
        if let Ok(seconds) = text.parse::<u64>() {
            options.gc_grace_secs = seconds;
        }
    }
    options
}

fn hex(hash: Hash32) -> String {
    hash.to_hex()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("help");
    let store_path = arg(&args, "--store").unwrap_or_else(|| ".".to_owned());
    let now = fixed_ns(&args);
    let backend_name = backend(&args);
    match mode {
        "init" => {
            init_driver(&store_path, now, &backend_name, &args);
            println!("{{\"ok\":true}}");
        }
        "parity" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "parity.bin".to_owned());
            let content = arg(&args, "--content").unwrap_or_else(|| "parity".to_owned());
            let seed = arg(&args, "--seed").unwrap_or_else(|| "seed".to_owned());
            if backend_name == "local" && !std::path::Path::new(&store_path).join("FORMAT").exists()
            {
                let store =
                    Store::init(&store_path, options(WriterMode::Single, now)).expect("init");
                drop(store);
            }
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
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
            let (cpu_user0, cpu_sys0) = cpu_ms();
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
            arm_ambiguous_exit();
            let started = Instant::now();
            let mut latency_file =
                arg(&args, "--latencies-out").and_then(|path| std::fs::File::create(path).ok());
            let mut prepare_file = arg(&args, "--prepare-latencies-out")
                .and_then(|path| std::fs::File::create(path).ok());
            let mut committed = 0u64;
            let mut conflicts = 0u64;
            let mut aborted = 0u64;
            let mut attempts = 0u64;
            let mut logical_bytes = 0u64;
            let mut errors = Vec::new();
            for index in 0..count {
                let name = match &hot {
                    Some(name) => name.clone(),
                    None => format!("{prefix}-{index:06}"),
                };
                let content = format!("{writer}:{index}");
                let seed = format!("{writer}:{index}:seed");
                let prepare_started = Instant::now();
                let (manifest, _, size) =
                    store.prepare_content(content.as_bytes()).expect("content");
                if let Some(file) = prepare_file.as_mut() {
                    let _ = writeln!(file, "{}", prepare_started.elapsed().as_nanos());
                }
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
                let commit_started = Instant::now();
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
                if let Some(file) = latency_file.as_mut() {
                    let _ = writeln!(file, "{}", commit_started.elapsed().as_nanos());
                }
                match outcome {
                    Ok(outcome) => {
                        committed += 1;
                        logical_bytes = logical_bytes.saturating_add(size);
                        attempts += u64::from(outcome.attempts) + u64::from(outer);
                    }
                    Err(err) if err.class() == weftfs::error::Class::Conflict => {
                        conflicts += 1;
                    }
                    Err(err) if err.class() == weftfs::error::Class::Busy => {
                        // The outer retry budget was exhausted; the outcome is
                        // unknown and must be resolved by transaction id.
                        aborted += 1;
                        errors.push(err.to_string());
                    }
                    Err(err) => errors.push(err.to_string()),
                }
            }
            let stats = store.backend_stats();
            let rebase = store.rebase_stats();
            let (cpu_user1, cpu_sys1) = cpu_ms();
            println!(
                "{}",
                serde_json::json!({
                    "ok": errors.is_empty(),
                    "writer": writer,
                    "domain": domain_arg(&args).map(|id| hex(id)),
                    "rebase_gap_commits_read": rebase.gap_commits_read,
                    "rebase_gap_commits_cached": rebase.gap_commits_cached,
                    "rebase_touched_intersections": rebase.touched_intersections,
                    "rebase_max_consecutive_retries": rebase.max_consecutive_retries,
                    "rebase_total_attempts": rebase.total_attempts,
                    "committed": committed,
                    "conflicts": conflicts,
                    "aborted": aborted,
                    "attempts": attempts,
                    "logical_bytes": logical_bytes,
                    "errors": errors,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "backend_reads": stats.reads + stats.range_reads,
                    "backend_writes": stats.writes,
                    "bytes_read": stats.bytes_read,
                    "bytes_written": stats.bytes_written,
                    "peak_rss_kib": peak_rss_kib(),
                    "cpu_user_ms": cpu_user1.saturating_sub(cpu_user0),
                    "cpu_sys_ms": cpu_sys1.saturating_sub(cpu_sys0),
                    "head": hex(store.head()),
                    "generation": store.generation(),
                })
            );
        }
        "register" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "domain".to_owned());
            let kind = arg(&args, "--kind").unwrap_or_else(|| "checkpoint_family".to_owned());
            let scope = arg(&args, "--scope").unwrap_or_default();
            let mut store = Store::open_with_options_and_backend(
                &store_path,
                OpenMode::ReadWrite,
                options(WriterMode::Sharded, now),
                backend_kind(&args, &backend_name),
            )
            .expect("open sharded store");
            let record = weftfs::mw3::DomainRecord::new(
                name,
                weftfs::mw3::DomainKind::parse(&kind).expect("domain kind"),
                scope,
            )
            .expect("domain record");
            let id = store.register_domain(record).expect("register domain");
            if args.iter().any(|value| value == "--plain") {
                println!("{}", hex(id));
            } else {
                println!("{}", serde_json::json!({"ok": true, "domain": hex(id)}));
            }
        }
        "outcome" => {
            let tx_id = arg(&args, "--tx-id").expect("--tx-id");
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
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
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
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
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
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
        "head" => {
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            let (generation, head, image) =
                read_superblock(&args, &store_path).unwrap_or((0, Hash32::ZERO, Vec::new()));
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "generation": generation,
                    "head": hex(head),
                    "superblock_sha256": sha256_hex(&image),
                    "commits": list_count(&args, &store_path, KeyKind::Commit),
                    "objects": list_count(&args, &store_path, KeyKind::Object),
                    "manifests": list_count(&args, &store_path, KeyKind::Manifest),
                    "indexes": list_count(&args, &store_path, KeyKind::Index),
                    "store_generation": store.generation(),
                })
            );
        }
        "verify-full" => {
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            let report = store.verify(true).expect("verify");
            let problems: Vec<String> = report
                .problems
                .iter()
                .map(|problem| format!("{}: {}", problem.class, problem.detail))
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "ok": report.ok && problems.is_empty(),
                    "generation": report.generation,
                    "checked_commits": report.checked_commits,
                    "checked_objects": report.checked_objects,
                    "checked_manifests": report.checked_manifests,
                    "checked_chunks": report.checked_chunks,
                    "problems": problems,
                })
            );
        }
        "stat" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "stat.bin".to_owned());
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            let mut report = read_named(&store, &name);
            if let Some(object) = report.as_object_mut() {
                object.insert("ok".to_owned(), serde_json::Value::Bool(true));
            }
            println!("{report}");
        }
        "snapshot" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "snapshot".to_owned());
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
            let tx_id = arg(&args, "--tx-id").unwrap_or_else(|| {
                identity::h(
                    "weft/tx/id/v1",
                    format!("snapshot:{name}:{}", store.generation()).as_bytes(),
                )
                .to_hex()[..32]
                    .to_owned()
            });
            let head = store.head();
            let mut tx = store.begin_transaction(tx_id).expect("begin");
            {
                let state = store.current_state();
                tx.set_snapshot(state, &name, head).expect("snapshot op");
            }
            match store.execute_transaction(&tx, &TransactionOptions::default()) {
                Ok(outcome) => println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "snapshot": name,
                        "head": hex(outcome.commit),
                        "generation": outcome.generation,
                        "attempts": outcome.attempts,
                    })
                ),
                Err(err) => println!("{}", error_report(&err)),
            }
        }
        "gc" => {
            let apply = args.iter().any(|value| value == "--apply");
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
            match store.gc(apply) {
                Ok(report) => println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "applied": report.applied,
                        "marked_chunks": report.marked_chunks,
                        "marked_manifests": report.marked_manifests,
                        "candidates": report.candidates.len(),
                        "deleted": report.deleted.len(),
                        "deleted_bytes": report.deleted_bytes,
                        "deleted_index_bytes": report.deleted_index_bytes,
                        "removed_orphans": report.removed_orphans.len(),
                        "generation": store.generation(),
                    })
                ),
                Err(err) => println!("{}", error_report(&err)),
            }
        }
        "single" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "single.bin".to_owned());
            let content = arg(&args, "--content").unwrap_or_else(|| "single".to_owned());
            let seed = arg(&args, "--seed").unwrap_or_else(|| "single-seed".to_owned());
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            match store.create_file_seeded(
                State::ROOT_ID,
                name.as_bytes(),
                content.as_bytes(),
                0o644,
                0,
                0,
                BTreeMap::new(),
                seed.as_bytes(),
            ) {
                Ok(object_id) => println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "name": name,
                        "object_id": hex(object_id),
                        "head": hex(store.head()),
                        "generation": store.generation(),
                    })
                ),
                Err(err) => println!("{}", error_report(&err)),
            }
        }
        "hold-read" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "hold.bin".to_owned());
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            let before = read_named(&store, &name);
            if let Some(path) = arg(&args, "--ready") {
                let line = serde_json::json!({
                    "pid": std::process::id(),
                    "generation": store.generation(),
                    "head": hex(store.head()),
                    "before": before.clone(),
                });
                let _ = std::fs::write(path, format!("{line}\n"));
            }
            if let Some(path) = arg(&args, "--gate") {
                let gate = std::path::PathBuf::from(path);
                for _ in 0..12000 {
                    if gate.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            let after = read_named(&store, &name);
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "generation": store.generation(),
                    "head": hex(store.head()),
                    "before": before,
                    "after": after,
                })
            );
        }
        "open-check" => {
            let mode = arg(&args, "--mode").unwrap_or_else(|| "single".to_owned());
            let writer_mode = if mode == "optimistic" {
                WriterMode::Optimistic
            } else {
                WriterMode::Single
            };
            let opened = Store::open_with_options_and_backend(
                &store_path,
                OpenMode::ReadWrite,
                options(writer_mode, now),
                backend_kind(&args, &backend_name),
            );
            let report = match opened {
                Ok(store) => serde_json::json!({
                    "ok": true,
                    "generation": store.generation(),
                    "head": hex(store.head()),
                }),
                Err(err) => error_report(&err),
            };
            println!(
                "{}",
                serde_json::json!({"ok": true, "mode": mode, "open": report})
            );
        }
        "delete" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "delete.bin".to_owned());
            let mut store = open_driver(
                &store_path,
                OpenMode::ReadWrite,
                WriterMode::Optimistic,
                now,
                &backend_name,
                &args,
            );
            let tx_id = arg(&args, "--tx-id").unwrap_or_else(|| {
                identity::h(
                    "weft/tx/id/v1",
                    format!("delete:{name}:{}", store.generation()).as_bytes(),
                )
                .to_hex()[..32]
                    .to_owned()
            });
            let object_id = store
                .lookup(State::ROOT_ID, name.as_bytes())
                .expect("lookup");
            let entry_id = derive_entry_id(State::ROOT_ID, name.as_bytes()).expect("entry id");
            let mut tx = store.begin_transaction(tx_id).expect("begin");
            {
                let state = store.current_state();
                if state.entries.contains_key(&entry_id) {
                    tx.del_entry(state, entry_id).expect("del entry");
                }
                tx.delete_object(state, object_id).expect("delete object");
            }
            match store.execute_transaction(&tx, &TransactionOptions::default()) {
                Ok(outcome) => println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "deleted": name,
                        "head": hex(outcome.commit),
                        "generation": outcome.generation,
                    })
                ),
                Err(err) => println!("{}", error_report(&err)),
            }
        }
        "read-snapshot" => {
            let snapshot = arg(&args, "--snapshot").unwrap_or_default();
            let name = arg(&args, "--name").unwrap_or_else(|| "snap.bin".to_owned());
            let store = open_driver(
                &store_path,
                OpenMode::ReadOnly,
                WriterMode::Single,
                now,
                &backend_name,
                &args,
            );
            let report = (|| -> weftfs::Result<serde_json::Value> {
                let commit = store.resolve_snapshot_ref(&snapshot)?;
                let id = store.resolve_path_at(Some(commit), &format!("/{name}"))?;
                let bytes = store.read_object_at(Some(commit), id)?;
                Ok(serde_json::json!({
                    "present": true,
                    "snapshot_commit": hex(commit),
                    "object_id": hex(id),
                    "len": bytes.len(),
                    "content_hash": hex(identity::content_hash(&bytes)),
                }))
            })();
            match report {
                Ok(value) => println!("{}", serde_json::json!({"ok": true, "read": value})),
                Err(err) => println!("{}", error_report(&err)),
            }
        }
        "content-hash" => {
            let content = arg(&args, "--content").unwrap_or_default();
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "len": content.len(),
                    "content_hash": hex(identity::content_hash(content.as_bytes())),
                })
            );
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}
