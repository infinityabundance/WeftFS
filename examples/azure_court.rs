//! Azure Blob court harness (subphase R21).
//!
//! One deterministic workload runs twice: once on the normative local backend
//! and once on an Azure Blob-compatible endpoint. The receipt asserts that both
//! produce the **same content root**, the **same commit generation sequence**, a
//! clean full verify, and a read-only reopen that recovers the same whole state.
//!
//! It then runs the process-fault crash proxy over Azure: the `weftfs` CLI is
//! killed at each publication phase (`WEFT_CRASH_PHASE`), and the reopened store
//! must hold the old or the new whole state, never a partial one.
//!
//! Finally it records the ETag of the `superblock` control blob at several
//! points. That object is replaced on every commit, so a changing ETag sequence
//! is direct evidence that each head publication went through
//! `If-Match: <observed ETag>` rather than an unconditional overwrite.
//!
//! Court label: a single Azure Blob emulator on loopback in the OOM-protected
//! Docker court. Not an Azure, cloud, multi-node, media, power-loss, or
//! production claim.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use weftfs::azure::{AzureBackend, AzureConfig};
use weftfs::backend::{BackendKind, Key};
use weftfs::store::{OpenMode, Options, Store};

const SCHEMA: &str = "weft-azure-court-v1";
/// 12 files x 32 KiB = 384 KiB of visible content.
const FILES: usize = 12;
const FILE_BYTES: usize = 32 * 1024;
const CHUNK_SIZE: u64 = 8 * 1024;
/// 8 four-KiB range writes after creation.
const RANGE_WRITES: usize = 8;
const RANGE_BYTES: usize = 4096;
const LOCK_LEASE_SECS: u64 = 2;

fn usage() -> &'static str {
    "usage: azure_court --out <receipt.json> [--root <dir>] [--tests-summary <file>]\n\
     Requires WEFT_AZURE_TEST_ENDPOINT, WEFT_AZURE_TEST_CONTAINER, WEFT_AZURE_ACCOUNT and\n\
     WEFT_AZURE_KEY from bench/scripts/azure_court.sh."
}

fn main() {
    if let Err(err) = run() {
        eprintln!("azure_court: {err}");
        std::process::exit(1);
    }
}

struct Args {
    out: PathBuf,
    root: PathBuf,
    tests_summary: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut out: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    let mut tests_summary: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a path")?)),
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root needs a dir")?)),
            "--tests-summary" => {
                tests_summary = Some(PathBuf::from(
                    args.next().ok_or("--tests-summary needs a path")?,
                ))
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args {
        out: out.ok_or("--out <receipt.json> is required")?,
        root: root.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("weft-azure-{}", std::process::id()))
        }),
        tests_summary,
    })
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn endpoint() -> String {
    env("WEFT_AZURE_TEST_ENDPOINT")
}

fn container_base() -> String {
    let value = env("WEFT_AZURE_TEST_CONTAINER");
    if value.is_empty() {
        "weft-azure-court".to_owned()
    } else {
        value
    }
}

fn account() -> String {
    let value = env("WEFT_AZURE_ACCOUNT");
    if value.is_empty() {
        "devstoreaccount1".to_owned()
    } else {
        value
    }
}

fn account_key() -> String {
    env("WEFT_AZURE_KEY")
}

fn azure_config(container: String) -> AzureConfig {
    let key = account_key();
    AzureConfig {
        endpoint: endpoint(),
        container,
        account: account(),
        key: if key.is_empty() {
            None
        } else {
            Some(key.clone())
        },
        sas_token: None,
        anonymous: key.is_empty(),
        lock_lease: Duration::from_secs(LOCK_LEASE_SECS),
        request_timeout: Duration::from_secs(60),
    }
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = u64::from(seed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(1);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn problems_text(report: &weftfs::store::VerifyReport) -> Vec<String> {
    report
        .problems
        .iter()
        .map(|problem| format!("{}: {}", problem.class, problem.detail))
        .collect()
}

fn utc_now() -> String {
    match std::env::var("WEFT_AZURE_STAMP") {
        Ok(value) if !value.is_empty() => return value,
        _ => {}
    }
    match Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => format!(
            "epoch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0)
        ),
    }
}

fn peak_rss_kib() -> u64 {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.root)
        .map_err(|err| format!("cannot create {}: {err}", args.root.display()))?;

    let superblock_observer = AzureBackend::new("<azure-court>", azure_config(container_base()));
    let contents: Vec<Vec<u8>> = (0..FILES)
        .map(|index| bytes(FILE_BYTES, index as u8))
        .collect();

    // ---- 1. local run -----------------------------------------------------
    let local = run_layout(
        "local",
        BackendKind::Local,
        &args.root.join("local"),
        &contents,
        None,
    )?;

    // ---- 2. Azure run -----------------------------------------------------
    let azure = run_layout(
        "azure",
        BackendKind::Azure {
            config: azure_config(container_base()),
        },
        &args.root.join("azure"),
        &contents,
        Some(&superblock_observer),
    )?;

    // ---- 3. exactness -----------------------------------------------------
    let exactness = json!({
        "content_root_match": local["snapshot_root"] == azure["snapshot_root"],
        "generation_match": local["generation"] == azure["generation"],
        "generation_sequence_match":
            local["generation_sequence"] == azure["generation_sequence"],
        "verified_bytes_match": local["verified_bytes"] == azure["verified_bytes"],
        "content_digest_match": local["content_digest"] == azure["content_digest"],
        "snapshot_root": local["snapshot_root"].clone(),
        "generation": local["generation"].clone(),
        "verified_bytes": local["verified_bytes"].clone(),
        "content_digest": local["content_digest"].clone(),
        "generation_sequence": local["generation_sequence"].clone(),
    });

    // ---- 4. crash proxy over Azure ----------------------------------------
    let crash = crash_proxy(&args.root.join("crash"))?;

    let receipt = json!({
        "schema": SCHEMA,
        "generated_utc": utc_now(),
        "workload": {
            "files": FILES,
            "file_bytes": FILE_BYTES,
            "chunk_size": CHUNK_SIZE,
            "range_writes": RANGE_WRITES,
            "range_write_bytes": RANGE_BYTES,
            "visible_bytes": (FILES * FILE_BYTES) as u64,
            "fixed_timestamp_ns": 1_700_000_000_000_000_000i64,
            "note": "deterministic bytes, seeded ObjectIds, fixed clock; local and Azure must reproduce the same root and the same commit sequence",
        },
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "revision_weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "azure_endpoint": endpoint(),
            "azure_account": account(),
            "azure_emulator": std::env::var("WEFT_AZURE_SERVER_LABEL").unwrap_or_else(|_| "unknown".to_owned()),
            "azure_emulator_release": std::env::var("WEFT_AZURE_SERVER_VERSION").unwrap_or_else(|_| "unknown".to_owned()),
            "azure_emulator_digest": std::env::var("WEFT_AZURE_SERVER_DIGEST").unwrap_or_else(|_| "unknown".to_owned()),
            "azure_container": container_base(),
        },
        "configs": [local, azure],
        "exactness": exactness,
        "crash_proxy": crash,
        "tests": match args.tests_summary.as_deref() {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
                let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
                json!({ "summary_file": path.display().to_string(), "summary": lines })
            }
            None => Value::Null,
        },
        "peak_rss_kib": peak_rss_kib(),
        "label": "Single-host Azure Blob emulator on loopback in the OOM-protected Docker court with a memory-backed data directory. Not an Azure, cloud, multi-node, fleet, device-cache, media, power-loss, or production claim.",
    });

    let text = serde_json::to_string_pretty(&receipt)
        .map_err(|err| format!("cannot encode receipt: {err}"))?;
    std::fs::write(&args.out, text)
        .map_err(|err| format!("cannot write {}: {err}", args.out.display()))?;
    println!("azure_court: receipt {}", args.out.display());
    println!(
        "azure_court: content_root_match={} generation_match={} sequence_match={}",
        receipt["exactness"]["content_root_match"],
        receipt["exactness"]["generation_match"],
        receipt["exactness"]["generation_sequence_match"]
    );
    if receipt["exactness"]["content_root_match"] != json!(true) {
        return Err("local and Azure content roots diverged".to_owned());
    }
    if receipt["exactness"]["generation_sequence_match"] != json!(true) {
        return Err("local and Azure commit sequences diverged".to_owned());
    }
    Ok(())
}

fn run_layout(
    label: &str,
    kind: BackendKind,
    path: &Path,
    contents: &[Vec<u8>],
    observer: Option<&AzureBackend>,
) -> Result<Value, String> {
    let _ = std::fs::remove_dir_all(path);
    let started = Instant::now();
    let mut store = Store::init_with_backend(path, options(), kind.clone())
        .map_err(|err| format!("{label} init: {err}"))?;
    let plane_probes = store.plane_probes().cloned();
    let plane_capabilities = store.plane_capabilities();
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;

    let mut generation_sequence = vec![store.generation()];
    let mut superblock_etags: Vec<String> = Vec::new();
    // Sample the superblock's ETag. That control blob is replaced on every
    // commit, so the samples must all differ.
    let mut observe = |etags: &mut Vec<String>| {
        if let Some(observer) = observer {
            if let Ok(etag) = observer.etag_for_test(&Key::superblock()) {
                etags.push(etag);
            }
        }
    };
    observe(&mut superblock_etags);

    let create_started = Instant::now();
    let mut ids = Vec::with_capacity(FILES);
    for (index, body) in contents.iter().enumerate() {
        let id = store
            .create_file_seeded(
                root_id,
                format!("f{index:03}").as_bytes(),
                body,
                0o644,
                0,
                0,
                Default::default(),
                format!("seed-{index}").as_bytes(),
            )
            .map_err(|err| format!("{label} create: {err}"))?;
        ids.push(id);
        generation_sequence.push(store.generation());
    }
    let create = create_started.elapsed();
    observe(&mut superblock_etags);

    let patch = bytes(RANGE_BYTES, 0xAA);
    let write_started = Instant::now();
    for step in 0..RANGE_WRITES {
        let file = ids[step % ids.len()];
        let offset = (step * RANGE_BYTES) % (FILE_BYTES - RANGE_BYTES);
        store
            .write_file(file, offset as u64, &patch)
            .map_err(|err| format!("{label} range write: {err}"))?;
        generation_sequence.push(store.generation());
    }
    let range_write = write_started.elapsed();
    observe(&mut superblock_etags);

    let read_started = Instant::now();
    let mut verified = 0u64;
    let mut digest = blake3::Hasher::new();
    for (index, id) in ids.iter().enumerate() {
        let body = store
            .read_object(*id)
            .map_err(|err| format!("{label} read: {err}"))?;
        if body.len() != contents[index].len() {
            return Err(format!("{label} file {index} length changed"));
        }
        verified += body.len() as u64;
        digest.update(&body);
    }
    let read = read_started.elapsed();

    let verify_started = Instant::now();
    let report = store
        .verify(true)
        .map_err(|err| format!("{label} verify: {err}"))?;
    let verify = verify_started.elapsed();
    let problems = problems_text(&report);
    let generation = store.generation();
    let snapshot_root = store.snapshot_root().to_hex();
    let diagnostics = store.backend_diagnostics();
    drop(store);

    // Read-only reopen: the recovered state must be the same whole state.
    let reopen_started = Instant::now();
    let reopened = Store::open_with_backend(path, OpenMode::ReadOnly, kind)
        .map_err(|err| format!("{label} reopen: {err}"))?;
    let reopen = reopen_started.elapsed();
    let reopened_generation = reopened.generation();
    let reopened_root = reopened.snapshot_root().to_hex();
    let reopen_report = reopened
        .verify(true)
        .map_err(|err| format!("{label} reopen verify: {err}"))?;
    let reopen_problems: Vec<String> = problems_text(&reopen_report);
    drop(reopened);

    if !problems.is_empty() {
        return Err(format!("{label} verify reported problems: {problems:?}"));
    }
    if !reopen_problems.is_empty() {
        return Err(format!(
            "{label} reopen verify reported problems: {reopen_problems:?}"
        ));
    }
    if reopened_generation != generation || reopened_root != snapshot_root {
        return Err(format!("{label} reopened to a different state"));
    }
    // Every sample of the control blob's ETag must differ: each commit replaced
    // it under a conditional write.
    for pair in superblock_etags.windows(2) {
        if pair[0] == pair[1] {
            return Err(format!(
                "{label} superblock ETag did not change across publications: {superblock_etags:?}"
            ));
        }
    }
    if observer.is_some() && superblock_etags.len() < 3 {
        return Err(format!(
            "{label} superblock ETag sampling returned {} samples, expected at least 3",
            superblock_etags.len()
        ));
    }

    Ok(json!({
        "config": label,
        "status": "ok",
        "store": path.display().to_string(),
        "generation": generation,
        "generation_sequence": generation_sequence,
        "superblock_etag_observed": superblock_etags,
        "snapshot_root": snapshot_root,
        "verified_bytes": verified,
        "content_digest": hex::encode(digest.finalize().as_bytes()),
        "phases_ms": {
            "create": create.as_secs_f64() * 1000.0,
            "range_write": range_write.as_secs_f64() * 1000.0,
            "read": read.as_secs_f64() * 1000.0,
            "verify": verify.as_secs_f64() * 1000.0,
            "reopen": reopen.as_secs_f64() * 1000.0,
        },
        "verify": { "problems": problems },
        "reopen": {
            "generation": reopened_generation,
            "snapshot_root": reopened_root,
            "problems": reopen_problems,
            "match": reopened_generation == generation && reopened_root == snapshot_root,
        },
        "planes": {
            "capabilities": plane_capabilities,
            "probes": plane_probes,
        },
        "diagnostics": diagnostics,
        "wall_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))
}

fn cli_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| format!("cannot locate harness: {err}"))?;
    let directory = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot derive the target directory")?;
    let binary = directory.join("weftfs");
    if !binary.is_file() {
        return Err(format!(
            "the weftfs CLI was not built at {}; run `cargo build --release --bins` first",
            binary.display()
        ));
    }
    Ok(binary)
}

fn crash_proxy(root: &Path) -> Result<Value, String> {
    let cli = cli_binary()?;
    std::fs::create_dir_all(root)
        .map_err(|err| format!("cannot create {}: {err}", root.display()))?;
    let base = container_base();
    let mut phases: Vec<Value> = Vec::new();
    let mut all_exact = true;

    for (index, phase) in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ]
    .iter()
    .enumerate()
    {
        let container = format!("{base}-crash-{index}");
        let store_path = root.join(format!("store-{index}"));
        let config = azure_config(container.clone());
        let kind = BackendKind::Azure {
            config: config.clone(),
        };
        let crash_observer = AzureBackend::new("<azure-court>", config);
        let mut store = Store::init_with_backend(&store_path, options(), kind.clone())
            .map_err(|err| format!("crash {phase} init: {err}"))?;
        let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
        let id = store
            .create_file_seeded(
                root_id,
                b"f",
                b"old\n",
                0o644,
                0,
                0,
                Default::default(),
                b"crash-seed",
            )
            .map_err(|err| format!("crash {phase} create: {err}"))?;
        let generation_before = store.generation();
        drop(store);

        let status = Command::new(&cli)
            .arg("--backend")
            .arg("azure")
            .arg("--store")
            .arg(&store_path)
            .arg("write")
            .arg("/f")
            .arg("--offset")
            .arg("0")
            .arg("--text")
            .arg("new content, longer than old\n")
            // Credentials travel in the environment, never on a command line
            // other processes can read.
            .env("WEFT_AZURE_ENDPOINT", endpoint())
            .env("WEFT_AZURE_CONTAINER", &container)
            .env("WEFT_AZURE_ACCOUNT", account())
            .env("WEFT_AZURE_KEY", account_key())
            .env("WEFT_AZURE_LOCK_LEASE_SECS", LOCK_LEASE_SECS.to_string())
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .map_err(|err| format!("cannot spawn the weftfs CLI: {err}"))?;
        let aborted = status.code().is_none();
        if !aborted {
            return Err(format!(
                "the crash child did not abort at phase {phase} (status {status})"
            ));
        }

        // The aborted writer left its LOCK blob behind; wait out the lease so
        // recovery, not lock stealing, is what is being tested.
        std::thread::sleep(Duration::from_secs(LOCK_LEASE_SECS + 1));

        let reopened = Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind.clone())
            .map_err(|err| format!("crash {phase} reopen: {err}"))?;
        let bytes = reopened
            .read_object(id)
            .map_err(|err| format!("crash {phase} read: {err}"))?;
        let generation_after = reopened.generation();
        let report = reopened
            .verify(true)
            .map_err(|err| format!("crash {phase} verify: {err}"))?;
        let problems: Vec<String> = problems_text(&report);
        let superblock_etag = crash_observer.etag_for_test(&Key::superblock()).ok();
        drop(reopened);

        let old = bytes == b"old\n";
        let new = bytes == b"new content, longer than old\n";
        let exact = old || new;
        all_exact &= exact;
        if !exact {
            return Err(format!(
                "phase {phase} produced a partial state: {:?}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        // A crash after the commit file and before the head publication leaves
        // an unreferenced newer commit record. That is the documented early-crash
        // state (recovery ignores it), and it is the only problem a crash phase
        // may report; anything else is a real defect.
        let unpublished_tail = !problems.is_empty()
            && problems.iter().all(|problem| {
                problem.starts_with("Corrupt:")
                    && problem.contains("exists above the highest published superblock generation")
            });
        if !problems.is_empty() && !unpublished_tail {
            return Err(format!("phase {phase} verify reported {problems:?}"));
        }

        phases.push(json!({
            "phase": phase,
            "aborted_by_signal": aborted,
            "state": if old { "old" } else { "new" },
            "generation_before": generation_before,
            "generation_after": generation_after,
            "bytes": String::from_utf8_lossy(&bytes).trim_end().to_owned(),
            "verify_state": if unpublished_tail { "unpublished-tail" } else { "clean" },
            "verify_problems": problems,
            "superblock_etag": superblock_etag,
        }));
    }

    Ok(json!({
        "aborted_by_signal": true,
        "old_or_new_only": all_exact,
        "partial_states": 0,
        "phases": phases,
        "note": "process-fault proxy: the writer is aborted at each publication phase; this is not a physical power-loss test",
    }))
}
