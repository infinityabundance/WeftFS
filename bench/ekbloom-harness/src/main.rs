//! EKBLOOM boundary (`EKB-E1`) and space (`EKB-E2`) court harness.
//!
//! This crate is deliberately **outside** the WeftFS workspace and outside the
//! published `weftfs` dependency graph (see `exclude` in the root `Cargo.toml`
//! and `bench/CONTRACTS-ekbloom.md` §1). It exists to answer two questions with
//! the pinned public `ekbloom` crate:
//!
//! * `--phase boundary`: do real WeftFS tensor bytes round-trip byte-exactly
//!   through an EKBLOOM container, inside the court's unchanged hard limits?
//! * `--phase space`: is the EKBLOOM container smaller than the representation
//!   the shipped WeftFS planner chooses for the same bytes?
//!
//! Method, tensor selection rules, bounds and pass/fail shapes are fixed in
//! `bench/CONTRACTS-ekbloom.md`, committed before the first measurement. This
//! harness implements that contract; it does not decide it.
//!
//! The safetensors parser, the planner and the store are the **shipped** ones
//! (`weftfs::safetensors`, `weftfs::tensor_store`, `weftfs::store`). There is no
//! second parser and no re-derived planner: the point of E2 is to compare
//! against what WeftFS actually does.
//!
//! Everything is bounded: one tensor in memory at a time, a per-tensor logical
//! cap, a body cap, and a process-RSS abort that fires before the court's own
//! memory limit. Every refusal is recorded, never silently dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use weftfs::safetensors::{SafetensorsFile, SafetensorsTensor};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions};
use weftfs::Hash32;

// ---------------------------------------------------------------------------
// Frozen contract constants (bench/CONTRACTS-ekbloom.md, version 1)
// ---------------------------------------------------------------------------

const RECEIPT_SCHEMA: &str = "weft.ekbloom-court.v1";

const EKBLOOM_REPO: &str = "https://github.com/infinityabundance/EKBLOOM";
const EKBLOOM_REVISION: &str = "8275a41b72c17abdc55bf74b82f319876e587ba0";
const EKBLOOM_CRATE: &str = "ekbloom";
const EKBLOOM_VERSION: &str = "0.7.3";
/// crates.io checksum, verified against the version API before the contract was
/// committed and re-checked against the harness `Cargo.lock` at run time.
const EKBLOOM_CHECKSUM: &str = "1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4";

const CORPUS_MANIFEST: &str = "bench/corpora/real-models/qwen2.5-0.5b-pair.json";

/// Contract §3: per-tensor logical cap (384 MiB).
const PER_TENSOR_LOGICAL_CAP_BYTES: u64 = 384 * 1024 * 1024;
/// Contract §3: body cap, as a multiple of the tensor's logical byte length.
const BODY_CAP_MULTIPLE: u64 = 3;
/// Contract §3: process RSS abort, checked after every object.
const RSS_ABORT_BYTES: u64 = 2560 * 1024 * 1024;

/// Contract §3.2: per `(dtype, shape)` class, take every `k`-th tensor where
/// `k = ceil(class_size / 8)`.
const PER_CLASS_SAMPLES: usize = 8;

/// Contract §4.2: the margin is the shipped mode-9 gate's 1/64, in integer form.
const MARGIN_NUMERATOR: u128 = 63;
const MARGIN_DENOMINATOR: u128 = 64;

/// The crate CLI's own default for a fresh object: `--profile` omitted resolves
/// to `root` and `--machine` omitted resolves to `auto`
/// (`ekbloom-0.7.3/src/main.rs`, `resolve_profile(.., false)` and
/// `unwrap_or("auto")`). The harness passes exactly those two choices and
/// nothing else, so the E2 row is the crate's default and not a tuned row.
const EKBLOOM_DEFAULT_PROFILE: &str = "root (crate CLI default when --profile is omitted)";
const EKBLOOM_DEFAULT_POLICY: &str =
    "create_auto: machine 2 if it shrinks, else machine 3 (crate default)";

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

struct Args {
    phase: String,
    out: PathBuf,
    work: PathBuf,
    stamp: String,
    lock_checksum: String,
    rustc_version: String,
    cargo_version: String,
}

fn usage() -> ! {
    eprintln!(
        "usage: ekbloom-harness --phase boundary|space --out RECEIPT.json \\\n\
         \x20            --work DIR --stamp STAMP --lock-checksum HEX \\\n\
         \x20            --rustc-version STR --cargo-version STR"
    );
    std::process::exit(64);
}

fn parse_args() -> Args {
    let mut phase: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut work: Option<PathBuf> = None;
    let mut stamp = String::from("unknown");
    let mut lock_checksum = String::new();
    let mut rustc_version = String::from("unknown");
    let mut cargo_version = String::from("unknown");
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = || argv.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--phase" => phase = Some(value()),
            "--out" => out = Some(PathBuf::from(value())),
            "--work" => work = Some(PathBuf::from(value())),
            "--stamp" => stamp = value(),
            "--lock-checksum" => lock_checksum = value(),
            "--rustc-version" => rustc_version = value(),
            "--cargo-version" => cargo_version = value(),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument '{other}'");
                usage();
            }
        }
    }
    let phase = phase.unwrap_or_else(|| usage());
    if phase != "boundary" && phase != "space" {
        eprintln!("unsupported phase '{phase}' (this subphase implements boundary and space)");
        std::process::exit(64);
    }
    Args {
        phase,
        out: out.unwrap_or_else(|| usage()),
        work: work.unwrap_or_else(|| usage()),
        stamp,
        lock_checksum,
        rustc_version,
        cargo_version,
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Reset the kernel's peak-RSS high-water mark for this process.
///
/// `clear_refs = 5` resets `VmHWM` to the current RSS (Linux >= 4.0). When the
/// write is refused the harness records `peak_reset: false` and reports the
/// monotonically increasing `VmHWM`; it never estimates a peak.
fn reset_peak_rss() -> bool {
    fs::write("/proc/self/clear_refs", b"5\n").is_ok()
}

fn status_kib(field: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/self/status").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let rest = rest.trim_start_matches(':').trim();
            let number = rest.split_whitespace().next()?;
            return number.parse().ok();
        }
    }
    None
}

fn cgroup_value(path: &str) -> Value {
    match fs::read_to_string(path) {
        Ok(text) => json!(text.trim()),
        Err(_) => json!("unavailable"),
    }
}

fn cgroup_pids_events_max() -> Value {
    match fs::read_to_string("/sys/fs/cgroup/pids.events") {
        Ok(text) => {
            for line in text.lines() {
                let mut parts = line.split_whitespace();
                if parts.next() == Some("max") {
                    if let Some(value) = parts.next() {
                        return json!(value);
                    }
                }
            }
            json!("0")
        }
        Err(_) => json!("unavailable"),
    }
}

fn cgroup_oom_kill() -> Value {
    match fs::read_to_string("/sys/fs/cgroup/memory.events") {
        Ok(text) => {
            for line in text.lines() {
                let mut parts = line.split_whitespace();
                if parts.next() == Some("oom_kill") {
                    if let Some(value) = parts.next() {
                        return json!(value);
                    }
                }
            }
            json!("0")
        }
        Err(_) => json!("unavailable"),
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_owned())
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// A typed, fail-closed harness outcome.
struct Skip {
    reason: String,
}

#[derive(Debug)]
struct Refusal {
    kind: &'static str,
    detail: String,
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

struct Arm {
    name: &'static str,
    file: PathBuf,
    expected_bytes: u64,
    expected_sha256: String,
}

struct VerifiedArm {
    arm: Arm,
    file: SafetensorsFile,
    header_sha256: String,
}

fn load_corpus(manifest_path: &Path, corpus_dir: &Path) -> Result<Vec<VerifiedArm>, Skip> {
    let text = fs::read_to_string(manifest_path).map_err(|err| Skip {
        reason: format!(
            "cannot read corpus manifest {}: {err}",
            manifest_path.display()
        ),
    })?;
    let manifest: Value = serde_json::from_str(&text).map_err(|err| Skip {
        reason: format!("corpus manifest is not JSON: {err}"),
    })?;
    let pair = manifest.get("pair").ok_or_else(|| Skip {
        reason: "corpus manifest has no `pair` object".to_owned(),
    })?;

    let mut arms = Vec::new();
    for (name, key) in [("base", "base"), ("instruct", "instruct")] {
        let entry = pair.get(key).ok_or_else(|| Skip {
            reason: format!("corpus manifest has no pair.{key}"),
        })?;
        let file_name = entry
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| Skip {
                reason: format!("corpus manifest pair.{key}.file is missing"),
            })?;
        let size = entry
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| Skip {
                reason: format!("corpus manifest pair.{key}.size is missing"),
            })?;
        let sha = entry
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| Skip {
                reason: format!("corpus manifest pair.{key}.sha256 is missing"),
            })?;
        arms.push(Arm {
            name,
            file: corpus_dir.join(file_name),
            expected_bytes: size,
            expected_sha256: sha.to_owned(),
        });
    }

    let mut verified = Vec::new();
    for arm in arms {
        if !arm.file.is_file() {
            return Err(Skip {
                reason: format!(
                    "corpus container {} is absent (no substitution is permitted)",
                    arm.file.display()
                ),
            });
        }
        // The shipped parser verifies the header layout and computes the
        // whole-file SHA-256; both are re-checked against the committed
        // manifest here, so a receipt may only quote digests measured in court.
        let file = SafetensorsFile::parse(&arm.file).map_err(|err| Skip {
            reason: format!("cannot parse {}: {err}", arm.file.display()),
        })?;
        let measured = hex(&file.sha256);
        if file.file_bytes != arm.expected_bytes || measured != arm.expected_sha256 {
            return Err(Skip {
                reason: format!(
                    "corpus digest mismatch for {}: measured {} bytes / {} (manifest {} bytes / {}); \
                     no substitution is permitted",
                    arm.file.display(),
                    file.file_bytes,
                    measured,
                    arm.expected_bytes,
                    arm.expected_sha256
                ),
            });
        }
        let header_sha256 = sha256_hex(&file.header_bytes);
        verified.push(VerifiedArm {
            arm,
            file,
            header_sha256,
        });
    }
    Ok(verified)
}

/// Tensors of one arm in the contract's ascending-name order.
fn sorted_tensors(file: &SafetensorsFile) -> Vec<&SafetensorsTensor> {
    let mut tensors: Vec<&SafetensorsTensor> = file.tensors.iter().collect();
    tensors.sort_by(|a, b| a.name.cmp(&b.name));
    tensors
}

/// Contract §3.2: per `(dtype, shape)` class, every `k`-th tensor in ascending
/// name order, `k = ceil(class_size / PER_CLASS_SAMPLES)`.
fn space_selection(file: &SafetensorsFile) -> Vec<&SafetensorsTensor> {
    let tensors = sorted_tensors(file);
    let mut classes: BTreeMap<(String, Vec<u64>), Vec<&SafetensorsTensor>> = BTreeMap::new();
    for tensor in tensors {
        classes
            .entry((tensor.dtype.clone(), tensor.shape.clone()))
            .or_default()
            .push(tensor);
    }
    let mut selected = Vec::new();
    for (_class, members) in classes {
        let stride = members.len().div_ceil(PER_CLASS_SAMPLES).max(1);
        selected.extend(members.iter().copied().step_by(stride));
    }
    selected.sort_by(|a, b| a.name.cmp(&b.name));
    selected
}

// ---------------------------------------------------------------------------
// EKBLOOM round trip (one object)
// ---------------------------------------------------------------------------

struct EkbloomRow {
    row: Value,
    exact: bool,
    verifier_ok: bool,
    body_bytes: u64,
}

/// Encode one tensor into a fresh `.ekb` container, read it back, and compare.
///
/// The recorded `body_bytes` is the complete on-disk container (header, core,
/// manifest, every section, footer), never the in-memory core.
fn ekbloom_round_trip(
    tensor: &SafetensorsTensor,
    bytes: &[u8],
    container: &Path,
) -> Result<EkbloomRow, Refusal> {
    let logical = bytes.len() as u64;
    if logical > PER_TENSOR_LOGICAL_CAP_BYTES {
        return Err(Refusal {
            kind: "memory_bound",
            detail: format!(
                "logical bytes {logical} exceed the contract's per-tensor cap \
                 {PER_TENSOR_LOGICAL_CAP_BYTES}"
            ),
        });
    }

    let _ = fs::remove_file(container);
    let peak_reset = reset_peak_rss();

    let started = Instant::now();
    let body = ekbloom::create_auto(bytes, ekbloom::ManifestationProfile::Root).map_err(|err| {
        Refusal {
            kind: "encode_error",
            detail: format!("create_auto(Root) failed: {err}"),
        }
    })?;
    let encode_ms = ms(started);

    let physical = body.physical_bytes();
    let core_bytes = body.visible_bytes();
    let machine_id = body.machine_id;
    let profiles = body.profiles.clone();
    if physical > logical.saturating_mul(BODY_CAP_MULTIPLE) {
        return Err(Refusal {
            kind: "memory_bound",
            detail: format!(
                "body {physical} bytes exceeds the contract's {BODY_CAP_MULTIPLE}x logical cap \
                 ({} bytes)",
                logical.saturating_mul(BODY_CAP_MULTIPLE)
            ),
        });
    }

    ekbloom::write_body(container, &body).map_err(|err| Refusal {
        kind: "write_error",
        detail: format!("write_body failed: {err}"),
    })?;
    let body_bytes = fs::metadata(container)
        .map_err(|err| Refusal {
            kind: "write_error",
            detail: format!("cannot stat the container: {err}"),
        })?
        .len();
    drop(body);

    // Open the container EKBLOOM just wrote and run EKBLOOM's own deepest
    // verifier on it (contract §4.1 C3).
    let reopened = ekbloom::open(container).map_err(|err| Refusal {
        kind: "unexpected_refusal",
        detail: format!("open() refused a container EKBLOOM had just written: {err}"),
    })?;
    let report = ekbloom::verify_deep(&reopened).map_err(|err| Refusal {
        kind: "unexpected_refusal",
        detail: format!("verify_deep() refused a container EKBLOOM had just written: {err}"),
    })?;
    let verifier_ok = report.ok;
    let verifier_failed: Vec<String> = report
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect();
    let verifier_checks = report.checks.len();
    let verified_logical_len = report.logical_len;
    drop(reopened);

    let started = Instant::now();
    let restored = ekbloom::read_file(
        container,
        ekbloom::ReadRequest {
            offset: 0,
            length: logical,
        },
    )
    .map_err(|err| Refusal {
        kind: "unexpected_refusal",
        detail: format!("read_file() failed on a container EKBLOOM had just written: {err}"),
    })?;
    let decode_ms = ms(started);

    let exact = restored.len() == bytes.len() && restored == bytes;
    let sha_in = sha256_hex(bytes);
    let sha_out = sha256_hex(&restored);
    let sha_match = sha_in == sha_out;
    let exact = exact && sha_match;

    let peak_kib = status_kib("VmHWM").unwrap_or(0);
    let rss_kib = status_kib("VmRSS").unwrap_or(0);

    Ok(EkbloomRow {
        row: json!({
            "name": tensor.name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "begin": tensor.begin,
            "end": tensor.end,
            "logical_bytes": logical,
            "body_bytes": body_bytes,
            "core_bytes": core_bytes,
            "in_memory_physical_bytes": physical,
            "container_overhead_bytes": body_bytes.saturating_sub(physical),
            "machine_id": machine_id,
            "profiles": profiles,
            "exact": exact,
            "sha256_in": sha_in,
            "sha256_out": sha_out,
            "ekbloom_verifier_ok": verifier_ok,
            "ekbloom_verifier_checks": verifier_checks,
            "ekbloom_verifier_logical_len": verified_logical_len,
            "ekbloom_verifier_failures": verifier_failed,
            "encode_ms": encode_ms,
            "decode_ms": decode_ms,
            "process_peak_rss_kib": peak_kib,
            "process_rss_kib": rss_kib,
            "peak_reset": peak_reset,
        }),
        exact,
        verifier_ok,
        body_bytes,
    })
}

// ---------------------------------------------------------------------------
// Shipped-planner footprint (E2 arm)
// ---------------------------------------------------------------------------

fn object_footprint(store: &Store, object: Hash32) -> Result<u64, String> {
    let record = store
        .object_meta(object)
        .map_err(|err| format!("object_meta: {err}"))?;
    let manifest_file = weftfs::recovery::manifest_path(store.root(), record.content);
    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    paths.insert(manifest_file);
    let manifest = weftfs::recovery::read_manifest(store.root(), record.content)
        .map_err(|err| format!("read_manifest: {err}"))?;
    if let Some(representation) = &manifest.representation {
        if let Some(frame) = representation.frame {
            paths.insert(weftfs::recovery::object_path(store.root(), frame));
        }
    }
    for chunk in &manifest.chunks {
        paths.insert(weftfs::recovery::object_path(store.root(), chunk.hash));
    }
    let mut total = 0u64;
    for path in paths {
        total += fs::metadata(&path)
            .map_err(|err| format!("stat {}: {err}", path.display()))?
            .len();
    }
    Ok(total)
}

fn dir_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

struct PlannedRow {
    row: Value,
    exact: bool,
    strategy_matches: bool,
    planned_bytes: u64,
}

/// Price and materialise one object through the shipped planner and store.
///
/// `planned_bytes` is `TensorPlan::chosen_physical_bytes` from
/// `Store::plan_tensor` with `TensorPlanOptions::default()` (contract §4.2).
/// The object is then really written and read back, and its observed on-disk
/// footprint is recorded beside the planner's number.
fn planned_round_trip(
    store: &mut Store,
    root: Hash32,
    tensor: &SafetensorsTensor,
    bytes: &[u8],
) -> Result<PlannedRow, String> {
    let layout = TensorLayoutSpec::row_major();
    let options = TensorPlanOptions::default();
    let plan = store
        .plan_tensor(&tensor.dtype, &tensor.shape, bytes, &layout, &[], &options)
        .map_err(|err| format!("plan_tensor: {err}"))?;

    let candidates: Vec<Value> = plan
        .candidates
        .iter()
        .map(|candidate| {
            json!({
                "strategy": candidate.strategy.as_str(),
                "applicable": candidate.applicable,
                "physical_bytes": candidate.physical_bytes,
                "estimated_decode_ns": candidate.estimated_decode_ns,
                "detail": candidate.detail,
            })
        })
        .collect();

    let object = store
        .write_tensor_file(
            root,
            tensor.name.as_bytes(),
            &tensor.dtype,
            &tensor.shape,
            bytes,
            &layout,
            None,
        )
        .map_err(|err| format!("write_tensor_file: {err}"))?;
    let footprint = object_footprint(store, object)?;
    let restored = store
        .read_tensor(object)
        .map_err(|err| format!("read_tensor: {err}"))?;
    let exact = restored.len() == bytes.len() && restored == bytes;

    let record = store
        .object_meta(object)
        .map_err(|err| format!("object_meta: {err}"))?;
    let manifest = weftfs::recovery::read_manifest(store.root(), record.content)
        .map_err(|err| format!("read_manifest: {err}"))?;
    let strategy = manifest
        .representation
        .as_ref()
        .map(|rep| rep.strategy.clone())
        .unwrap_or_else(|| "raw".to_owned());
    let strategy_matches = strategy == plan.chosen.as_str();

    Ok(PlannedRow {
        row: json!({
            "planned_bytes": plan.chosen_physical_bytes,
            "planned_strategy": plan.chosen.as_str(),
            "runner_up": plan.runner_up.map(|s| s.as_str()),
            "runner_up_physical_bytes": plan.runner_up_physical_bytes,
            "object_strategy": strategy,
            "object_footprint_bytes": footprint,
            "candidates": candidates,
            "planned_exact": exact,
            "planned_sha256": sha256_hex(&restored),
        }),
        exact,
        strategy_matches,
        planned_bytes: plan.chosen_physical_bytes,
    })
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();

    if let Err(err) = fs::create_dir_all(&args.work) {
        eprintln!(
            "cannot create work directory {}: {err}",
            args.work.display()
        );
        std::process::exit(1);
    }

    let corpus_dir = PathBuf::from(
        std::env::var("WEFT_EKBLOOM_CORPUS_DIR")
            .unwrap_or_else(|_| ".docker-cache/real-corpus".to_owned()),
    );
    let arms = match load_corpus(Path::new(CORPUS_MANIFEST), &corpus_dir) {
        Ok(arms) => arms,
        Err(skip) => {
            eprintln!("ekbloom_harness: skip {}", skip.reason);
            let _ = fs::write(args.work.join("skip_reason"), &skip.reason);
            std::process::exit(78);
        }
    };

    let locksum = args.lock_checksum.trim().to_owned();
    if locksum != EKBLOOM_CHECKSUM {
        eprintln!(
            "ekbloom_harness: fail lock_checksum: Cargo.lock resolved {locksum:?} but the contract \
             pins {EKBLOOM_CHECKSUM}"
        );
        std::process::exit(1);
    }

    for arm in &arms {
        println!(
            "corpus: arm={} file={} bytes={} sha256={} header_sha256={} tensors={}",
            arm.arm.name,
            arm.arm.file.display(),
            arm.file.file_bytes,
            hex(&arm.file.sha256),
            arm.header_sha256,
            arm.file.tensors.len()
        );
    }

    let receipt = match args.phase.as_str() {
        "boundary" => run_boundary(&args, &arms, &corpus_dir),
        "space" => run_space(&args, &arms, &corpus_dir),
        _ => unreachable!(),
    };

    let text = serde_json::to_string_pretty(&receipt).expect("receipt serialises");
    if let Err(err) = fs::write(&args.out, text) {
        eprintln!("cannot write receipt {}: {err}", args.out.display());
        std::process::exit(1);
    }
    println!("receipt: {}", args.out.display());

    let verdict = receipt
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("fail");
    println!("verdict: {verdict}");
    if verdict != "pass" && !(verdict == "loss" && args.phase == "space") {
        std::process::exit(1);
    }
}

fn base_receipt(args: &Args, phase: &str, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let corpus: Vec<Value> = arms
        .iter()
        .map(|arm| {
            json!({
                "arm": arm.arm.name,
                "file": arm.arm.file.display().to_string(),
                "expected_bytes": arm.arm.expected_bytes,
                "measured_bytes": arm.file.file_bytes,
                "expected_sha256": arm.arm.expected_sha256,
                "measured_sha256": hex(&arm.file.sha256),
                "header_sha256": arm.header_sha256,
                "tensors": arm.file.tensors.len(),
                "data_start": arm.file.data_start,
                "digest_match": true,
            })
        })
        .collect();

    json!({
        "schema": RECEIPT_SCHEMA,
        "court": "ekbloom",
        "phase": phase,
        "stamp": args.stamp,
        "contract": "bench/CONTRACTS-ekbloom.md (version 1)",
        "revisions": {
            "weftfs": env("WEFT_REPO_WEFTFS"),
            "weftfs_go": env("WEFT_REPO_WEFTFS_GO"),
            "weftcodec": env("WEFT_REPO_WEFTCODEC"),
            "weftcodec_go": env("WEFT_REPO_WEFTCODEC_GO"),
        },
        "court_limits": {
            "image": env("WEFT_COURT_IMAGE"),
            "image_id": env("WEFT_COURT_IMAGE_ID"),
            "kernel": env("WEFT_COURT_KERNEL"),
            "memory": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "pids": env("WEFT_COURT_PIDS"),
            "nproc": env("WEFT_COURT_NPROC"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
            "docker_client": env("WEFT_DOCKER_CLIENT"),
        },
        "toolchain": {
            "rustc": args.rustc_version,
            "cargo": args.cargo_version,
        },
        "upstream": {
            "repository": EKBLOOM_REPO,
            "revision": EKBLOOM_REVISION,
            "crate": EKBLOOM_CRATE,
            "version": EKBLOOM_VERSION,
            "checksum_contract": EKBLOOM_CHECKSUM,
            "checksum_cargo_lock": args.lock_checksum.trim(),
            "rust_version_declared": "1.89",
            "license": "MIT OR Apache-2.0",
            "default_representation": EKBLOOM_DEFAULT_POLICY,
            "default_profile": EKBLOOM_DEFAULT_PROFILE,
            "api_used": [
                "ekbloom::create_auto(&[u8], ManifestationProfile::Root) -> EkbloomBody",
                "ekbloom::write_body(&Path, &EkbloomBody)",
                "ekbloom::open(&Path) -> EkbloomBody",
                "ekbloom::verify_deep(&EkbloomBody) -> VerificationReport",
                "ekbloom::read_file(&Path, ReadRequest { offset: 0, length: logical_len }) -> Vec<u8>",
            ],
        },
        "corpus_manifest": CORPUS_MANIFEST,
        "corpus_dir": corpus_dir.display().to_string(),
        "corpus": corpus,
        "bounds": {
            "per_tensor_logical_cap_bytes": PER_TENSOR_LOGICAL_CAP_BYTES,
            "body_cap_multiple_of_logical": BODY_CAP_MULTIPLE,
            "process_rss_abort_bytes": RSS_ABORT_BYTES,
            "court_memory_bound": "3 GiB (never raised)",
        },
        "label": "single host, OOM-protected Docker court (3 GiB / 3 GiB swap / 1024 pids / \
                  4 cpus / bounded /tmp), public model weights on a warm local page cache, \
                  one process, no fleet, cloud, media, power-loss, GPU or serving claim",
    })
}

fn run_boundary(args: &Args, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let mut receipt = base_receipt(args, "boundary", arms, corpus_dir);
    let mut rows: Vec<Value> = Vec::new();
    let mut object_count = 0usize;
    let mut logical_bytes = 0u64;
    let mut body_bytes = 0u64;
    let mut exact_count = 0usize;
    let mut verifier_ok_count = 0usize;
    let mut verifier_failures: Vec<String> = Vec::new();
    let mut refusals: Vec<Value> = Vec::new();
    let mut peak_rss_kib = 0u64;
    let mut size_digest = Sha256::new();
    let mut aborted: Option<String> = None;

    for arm in arms {
        let tensors = sorted_tensors(&arm.file);
        println!(
            "boundary: arm={} objects={} (every tensor, contract §3.1)",
            arm.arm.name,
            tensors.len()
        );
        for (index, tensor) in tensors.iter().enumerate() {
            let container = args.work.join(format!("{}-{index:04}.ekb", arm.arm.name));
            let bytes = match arm.file.read_tensor(tensor) {
                Ok(bytes) => bytes,
                Err(err) => {
                    aborted = Some(format!(
                        "cannot read tensor `{}` from {}: {err}",
                        tensor.name,
                        arm.arm.file.display()
                    ));
                    break;
                }
            };
            match ekbloom_round_trip(tensor, &bytes, &container) {
                Ok(row) => {
                    object_count += 1;
                    logical_bytes += bytes.len() as u64;
                    body_bytes += row.body_bytes;
                    if row.exact {
                        exact_count += 1;
                    }
                    if row.verifier_ok {
                        verifier_ok_count += 1;
                    } else {
                        verifier_failures.push(tensor.name.clone());
                    }
                    let rss = row
                        .row
                        .get("process_rss_kib")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if rss > RSS_ABORT_BYTES / 1024 {
                        aborted = Some(format!(
                            "process RSS {rss} KiB exceeded the contract abort bound {} KiB \
                             after tensor `{}`",
                            RSS_ABORT_BYTES / 1024,
                            tensor.name
                        ));
                    }
                    peak_rss_kib = peak_rss_kib.max(
                        row.row
                            .get("process_peak_rss_kib")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    size_digest.update(row.body_bytes.to_le_bytes());
                    size_digest.update((bytes.len() as u64).to_le_bytes());
                    size_digest.update(tensor.name.as_bytes());
                    println!(
                        "  [{}/{}] {} logical={} body={} exact={} verifier={} encode_ms={:.3} \
                         decode_ms={:.3} peak_kib={}",
                        index + 1,
                        tensors.len(),
                        tensor.name,
                        bytes.len(),
                        row.body_bytes,
                        row.exact,
                        row.verifier_ok,
                        row.row
                            .get("encode_ms")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        row.row
                            .get("decode_ms")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        row.row
                            .get("process_peak_rss_kib")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                    rows.push(row.row);
                }
                Err(refusal) => {
                    println!(
                        "  [{}/{}] {} REFUSED {}: {}",
                        index + 1,
                        tensors.len(),
                        tensor.name,
                        refusal.kind,
                        refusal.detail
                    );
                    refusals.push(json!({
                        "arm": arm.arm.name,
                        "name": tensor.name,
                        "kind": refusal.kind,
                        "detail": refusal.detail,
                        "logical_bytes": bytes.len(),
                    }));
                }
            }
            let _ = fs::remove_file(&container);
            if aborted.is_some() {
                break;
            }
        }
        if aborted.is_some() {
            break;
        }
    }

    let c2_pass = exact_count == object_count && object_count > 0 && refusals.is_empty();
    let c3_pass = verifier_ok_count == object_count && object_count > 0;
    let oom_kill = cgroup_oom_kill();
    let pids_events = cgroup_pids_events_max();
    let c4_pass = aborted.is_none()
        && refusals.is_empty()
        && oom_kill.as_str() == Some("0")
        && pids_events.as_str() == Some("0");

    let verdict = if aborted.is_some() {
        "fail"
    } else if c2_pass && c3_pass && c4_pass {
        "pass"
    } else {
        "fail"
    };

    let mut limitations: Vec<Value> = Vec::new();
    if aborted.is_none() && !refusals.is_empty() {
        limitations.push(json!(
            "objects were refused by the declared bounds; see `refusals` (contract §4.1 \
             `memory_bound` ends the gate)"
        ));
    }
    if let Some(reason) = &aborted {
        limitations.push(json!(format!("run aborted: {reason}")));
    }
    if oom_kill.as_str() != Some("0") {
        limitations.push(json!(format!("cgroup oom_kill = {oom_kill}")));
    }
    if pids_events.as_str() != Some("0") {
        limitations.push(json!(format!("cgroup pids.events max = {pids_events}")));
    }
    limitations.push(json!(
        "encode_ms/decode_ms are observational fields only; contract §4.1 makes no \
         throughput or latency claim and none of these numbers may be quoted as one"
    ));
    limitations.push(json!(
        "one host, warm page cache, one process; not a fleet, media, power-loss or \
         cloud result"
    ));

    receipt["selection"] = json!({
        "rule": "contract §3.1: every tensor of both arms, ascending name order, no sampling",
        "objects": object_count,
        "logical_bytes": logical_bytes,
        "body_bytes": body_bytes,
        "arm_objects": arms.iter().map(|arm| json!({
            "arm": arm.arm.name,
            "tensors": arm.file.tensors.len(),
        })).collect::<Vec<_>>(),
    });
    receipt["rows"] = json!(rows);
    receipt["refusals"] = json!(refusals);
    receipt["summary"] = json!({
        "objects": object_count,
        "exact_objects": exact_count,
        "verifier_ok_objects": verifier_ok_count,
        "verifier_failed_objects": verifier_failures,
        "refused_objects": refusals.len(),
        "logical_bytes": logical_bytes,
        "body_bytes": body_bytes,
        "size_row_digest": hex(&size_digest.finalize()),
        "process_peak_rss_kib": peak_rss_kib,
        "process_peak_rss_bytes": (peak_rss_kib.saturating_mul(1024)) as u64,
        "process_peak_fraction_of_court_bound_ppm": if peak_rss_kib > 0 {
            (((peak_rss_kib as u128) * 1024 * 1_000_000) / (3u128 * 1024 * 1024 * 1024)) as u64
        } else {
            0
        },
        "aborted": aborted,
    });
    receipt["checks"] = json!([
        {"id": "C1_build", "pass": true, "detail": "pinned crate built and the harness linked"},
        {"id": "C2_round_trip", "pass": c2_pass,
         "detail": format!("{exact_count}/{object_count} objects byte-exact; {} refused", refusals.len())},
        {"id": "C3_upstream_verifier", "pass": c3_pass, "available": true,
         "detail": format!("ekbloom::verify_deep accepted {verifier_ok_count}/{object_count} containers")},
        {"id": "C4_resource", "pass": c4_pass,
         "detail": format!("oom_kill={oom_kill} pids_events_max={pids_events} aborted={}", aborted.is_some())},
    ]);
    receipt["cgroup"] = json!({
        "memory_peak_bytes": cgroup_value("/sys/fs/cgroup/memory.peak"),
        "memory_max_bytes": cgroup_value("/sys/fs/cgroup/memory.max"),
        "pids_events_max": pids_events,
        "pids_current": cgroup_value("/sys/fs/cgroup/pids.current"),
        "pids_max": cgroup_value("/sys/fs/cgroup/pids.max"),
        "oom_kill": oom_kill,
    });
    receipt["limitations"] = json!(limitations);
    receipt["verdict"] = json!(verdict);
    receipt
}

fn run_space(args: &Args, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let mut receipt = base_receipt(args, "space", arms, corpus_dir);
    let mut rows: Vec<Value> = Vec::new();
    let mut ladder: Vec<Value> = Vec::new();

    let mut raw_sum: u128 = 0;
    let mut planned_sum: u128 = 0;
    let mut body_sum: u128 = 0;
    let mut observed_sum: u128 = 0;
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut ties = 0usize;
    let mut exact_both = 0usize;
    let mut strategy_mismatches: Vec<String> = Vec::new();
    let mut ekbloom_refusals: Vec<Value> = Vec::new();
    let mut largest_loss: Option<(i128, String)> = None;
    let mut largest_win: Option<(i128, String)> = None;

    for arm in arms {
        let selected = space_selection(&arm.file);
        let store_dir = args.work.join(format!("store-{}", arm.arm.name));
        let _ = fs::remove_dir_all(&store_dir);
        fs::create_dir_all(&store_dir).expect("store work dir");
        let options = Options {
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        };
        let mut store = Store::init(&store_dir, options).expect("store init");
        let root = store.resolve_path("/").expect("store root");

        println!(
            "space: arm={} objects={} (contract §3.2 shape-class rule)",
            arm.arm.name,
            selected.len()
        );

        for (index, tensor) in selected.iter().enumerate() {
            let bytes = arm.file.read_tensor(tensor).expect("read tensor");
            let logical = bytes.len() as u64;
            let container = args
                .work
                .join(format!("space-{}-{index:04}.ekb", arm.arm.name));

            let ekbloom = ekbloom_round_trip(tensor, &bytes, &container);
            let planned = planned_round_trip(&mut store, root, tensor, &bytes);
            let _ = fs::remove_file(&container);

            let mut row = json!({
                "arm": arm.arm.name,
                "index": index,
                "name": tensor.name,
                "dtype": tensor.dtype,
                "shape": tensor.shape,
                "logical_bytes": logical,
                "raw_bytes": logical,
            });

            match &ekbloom {
                Ok(ek) => {
                    row["body_bytes"] = ek.row["body_bytes"].clone();
                    row["ekbloom_exact"] = json!(ek.exact);
                    row["ekbloom_verifier_ok"] = json!(ek.verifier_ok);
                    row["ekbloom_core_bytes"] = ek.row["core_bytes"].clone();
                    row["ekbloom_machine_id"] = ek.row["machine_id"].clone();
                    row["ekbloom_encode_ms"] = ek.row["encode_ms"].clone();
                    row["ekbloom_decode_ms"] = ek.row["decode_ms"].clone();
                    row["ekbloom_process_peak_rss_kib"] = ek.row["process_peak_rss_kib"].clone();
                }
                Err(refusal) => {
                    row["body_bytes"] = Value::Null;
                    row["ekbloom_refusal"] = json!({
                        "kind": refusal.kind,
                        "detail": refusal.detail,
                    });
                    ekbloom_refusals.push(json!({
                        "arm": arm.arm.name,
                        "name": tensor.name,
                        "kind": refusal.kind,
                        "detail": refusal.detail,
                    }));
                }
            }

            match &planned {
                Ok(pl) => {
                    row["planned_bytes"] = json!(pl.planned_bytes);
                    row["planned_strategy"] = pl.row["planned_strategy"].clone();
                    row["object_strategy"] = pl.row["object_strategy"].clone();
                    row["object_footprint_bytes"] = pl.row["object_footprint_bytes"].clone();
                    row["planned_exact"] = json!(pl.exact);
                    if !pl.strategy_matches {
                        strategy_mismatches.push(format!(
                            "{}:{} plan={} object={}",
                            arm.arm.name,
                            tensor.name,
                            pl.row["planned_strategy"],
                            pl.row["object_strategy"]
                        ));
                    }
                }
                Err(detail) => {
                    row["planned_bytes"] = Value::Null;
                    row["planned_error"] = json!(detail);
                }
            }

            // Per-object verdict, only when both arms produced a number.
            if let (Ok(ek), Ok(pl)) = (&ekbloom, &planned) {
                raw_sum += logical as u128;
                planned_sum += pl.planned_bytes as u128;
                body_sum += ek.body_bytes as u128;
                observed_sum += pl.row["object_footprint_bytes"].as_u64().unwrap_or(0) as u128;
                let body = ek.body_bytes as u128;
                let plannedb = pl.planned_bytes as u128;
                let verdict = if body * MARGIN_DENOMINATOR <= plannedb * MARGIN_NUMERATOR {
                    wins += 1;
                    "win"
                } else if body == plannedb {
                    ties += 1;
                    "tie"
                } else {
                    losses += 1;
                    "loss"
                };
                row["object_verdict"] = json!(verdict);
                let delta = body as i128 - plannedb as i128;
                if delta > 0
                    && largest_loss
                        .as_ref()
                        .map(|(d, _)| delta > *d)
                        .unwrap_or(true)
                {
                    largest_loss = Some((delta, format!("{}:{}", arm.arm.name, tensor.name)));
                }
                if delta < 0
                    && largest_win
                        .as_ref()
                        .map(|(d, _)| delta < *d)
                        .unwrap_or(true)
                {
                    largest_win = Some((delta, format!("{}:{}", arm.arm.name, tensor.name)));
                }
                if ek.exact && pl.exact {
                    exact_both += 1;
                }
            }

            println!(
                "  [{}/{}] {} logical={} body={} planned={} verdict={}",
                index + 1,
                selected.len(),
                tensor.name,
                logical,
                row.get("body_bytes")
                    .map(|v| v.to_string())
                    .unwrap_or("refused".into()),
                row.get("planned_bytes")
                    .map(|v| v.to_string())
                    .unwrap_or("error".into()),
                row.get("object_verdict")
                    .and_then(Value::as_str)
                    .unwrap_or("n/a"),
            );
            rows.push(row);
        }

        // Secondary ladder: every manifestation profile on the smallest tensor
        // of this arm (contract §4.2 — published for completeness only; a
        // verdict may not be taken from a non-default row).
        let smallest = sorted_tensors(&arm.file)
            .into_iter()
            .min_by(|a, b| a.byte_len().cmp(&b.byte_len()).then(a.name.cmp(&b.name)))
            .cloned();
        if let Some(tensor) = smallest {
            let bytes = arm.file.read_tensor(&tensor).expect("read smallest");
            let profiles: Vec<(&str, ekbloom::ManifestationProfile)> = vec![
                ("root", ekbloom::ManifestationProfile::Root),
                (
                    "sparse(1024)",
                    ekbloom::ManifestationProfile::Sparse { interval: 1024 },
                ),
                (
                    "dense(1024)",
                    ekbloom::ManifestationProfile::Dense { interval: 1024 },
                ),
                ("full", ekbloom::ManifestationProfile::Full),
                ("bloom4", ekbloom::ManifestationProfile::Bloom { depth: 4 }),
                ("bloom8", ekbloom::ManifestationProfile::Bloom { depth: 8 }),
                (
                    "bloom16",
                    ekbloom::ManifestationProfile::Bloom { depth: 16 },
                ),
                (
                    "regional(4,4096)",
                    ekbloom::ManifestationProfile::Regional {
                        depth: 4,
                        region_bytes: 4096,
                    },
                ),
                ("path4", ekbloom::ManifestationProfile::Path { depth: 4 }),
                ("max-speed", ekbloom::ManifestationProfile::MaxSpeed),
            ];
            for (label, profile) in profiles {
                let container = args.work.join(format!(
                    "ladder-{}-{}.ekb",
                    arm.arm.name,
                    label.replace(['(', ')', ','], "_")
                ));
                let _ = fs::remove_file(&container);
                let mut entry = json!({
                    "arm": arm.arm.name,
                    "tensor": tensor.name,
                    "logical_bytes": bytes.len(),
                    "profile": label,
                });
                match ekbloom::create_auto(&bytes, profile) {
                    Ok(body) => {
                        let physical = body.physical_bytes();
                        let machine = body.machine_id;
                        match ekbloom::write_body(&container, &body) {
                            Ok(()) => {
                                entry["body_bytes"] =
                                    json!(fs::metadata(&container).map(|m| m.len()).unwrap_or(0));
                                entry["in_memory_physical_bytes"] = json!(physical);
                                entry["machine_id"] = json!(machine);
                                entry["ok"] = json!(true);
                            }
                            Err(err) => {
                                entry["ok"] = json!(false);
                                entry["error"] = json!(format!("write_body: {err}"));
                            }
                        }
                    }
                    Err(err) => {
                        entry["ok"] = json!(false);
                        entry["error"] = json!(format!("create_auto: {err}"));
                    }
                }
                let _ = fs::remove_file(&container);
                println!(
                    "  ladder {} {} body={} ok={}",
                    arm.arm.name,
                    label,
                    entry
                        .get("body_bytes")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    entry
                        .get("ok")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into())
                );
                ladder.push(entry);
            }
        }

        receipt.as_object_mut().expect("receipt object").insert(
            format!("store_{}", arm.arm.name),
            json!({
                "store_bytes": dir_bytes(&store_dir),
                "objects": selected.len(),
            }),
        );
    }

    let ratio_ppm = if planned_sum > 0 {
        (body_sum * 1_000_000) / planned_sum
    } else {
        0
    };
    let aggregate_win = planned_sum > 0
        && body_sum * MARGIN_DENOMINATOR <= planned_sum * MARGIN_NUMERATOR
        && exact_both == rows.len()
        && ekbloom_refusals.is_empty()
        && strategy_mismatches.is_empty();
    let verdict =
        if rows.is_empty() || !ekbloom_refusals.is_empty() || strategy_mismatches.len() > 0 {
            "fail"
        } else if aggregate_win {
            "win"
        } else {
            "loss"
        };

    receipt["selection"] = json!({
        "rule": "contract §3.2: per (dtype, shape) class, every k-th tensor in ascending name \
                 order where k = ceil(class_size / 8); a class of size 1 is taken in full",
        "per_class_samples": PER_CLASS_SAMPLES,
        "objects": rows.len(),
        "arms": arms.iter().map(|arm| json!({
            "arm": arm.arm.name,
            "tensors_in_container": arm.file.tensors.len(),
            "selected": space_selection(&arm.file).len(),
            "selected_logical_bytes": space_selection(&arm.file).iter().map(|t| t.byte_len()).sum::<u64>(),
        })).collect::<Vec<_>>(),
    });
    receipt["arms"] = json!({
        "raw": "exact element bytes of the tensor",
        "planned": "Store::plan_tensor with TensorPlanOptions::default(): raw, zstd (level 3 and \
                    gated level 19), float-tensor (mode 9), tensor-sharded (mode 10); delta, \
                    patch, tiled and engram candidates are not applicable without a base, a tile \
                    shape or engram metadata",
        "ekbloom": format!("{EKBLOOM_DEFAULT_POLICY}; profile {EKBLOOM_DEFAULT_PROFILE}; \
                            body_bytes is the complete on-disk .ekb container"),
        "margin": "1/64 (the shipped mode-9 saving fraction), integer form \
                   body*64 <= planned*63",
    });
    receipt["aggregate"] = json!({
        "raw_bytes": raw_sum as u64,
        "planned_bytes": planned_sum as u64,
        "body_bytes": body_sum as u64,
        "observed_weftfs_object_bytes": observed_sum as u64,
        "body_over_planned_ppm": ratio_ppm as u64,
        "body_minus_planned_bytes": (body_sum as i128 - planned_sum as i128) as i64,
        "per_object_wins": wins,
        "per_object_losses": losses,
        "per_object_ties": ties,
        "objects_exact_both_arms": exact_both,
        "largest_loss": largest_loss.map(|(delta, who)| json!({"bytes": delta as i64, "object": who})),
        "largest_win": largest_win.map(|(delta, who)| json!({"bytes": delta as i64, "object": who})),
        "aggregate_margin_holds": aggregate_win,
    });
    receipt["rows"] = json!(rows);
    receipt["profile_ladder"] = json!(ladder);
    receipt["refusals"] = json!(ekbloom_refusals);
    receipt["checks"] = json!([
        {"id": "C2_restore_both_arms", "pass": exact_both == rows.len(),
         "detail": format!("{exact_both}/{} objects restored byte-exactly on both arms", rows.len())},
        {"id": "C_plan_matches_object", "pass": strategy_mismatches.is_empty(),
         "detail": if strategy_mismatches.is_empty() {
             "every written object used the strategy the plan priced".to_owned()
         } else {
             format!("{strategy_mismatches:?}")
         }},
        {"id": "C_margin", "pass": planned_sum > 0,
         "detail": format!("aggregate body {body_sum} vs planned {planned_sum} (ppm {ratio_ppm}); margin 1/64")},
    ]);
    receipt["cgroup"] = json!({
        "memory_peak_bytes": cgroup_value("/sys/fs/cgroup/memory.peak"),
        "memory_max_bytes": cgroup_value("/sys/fs/cgroup/memory.max"),
        "pids_events_max": cgroup_pids_events_max(),
        "pids_current": cgroup_value("/sys/fs/cgroup/pids.current"),
        "pids_max": cgroup_value("/sys/fs/cgroup/pids.max"),
        "oom_kill": cgroup_oom_kill(),
    });
    receipt["limitations"] = json!([
        "body_bytes charges EKBLOOM its whole container while planned_bytes is the shipped \
         planner's own frame accounting; the asymmetry runs against EKBLOOM and is deliberate \
         (contract §4.2)",
        "the profile ladder is secondary evidence on the smallest tensor of each arm; a verdict \
         may not be taken from a non-default row",
        "the corpus is one public checkpoint pair; this is a per-object size result on these \
         objects, not a general codec claim",
        "one host, warm page cache, one process; not a fleet, media, power-loss or cloud result",
    ]);
    receipt["verdict"] = json!(verdict);
    receipt
}
