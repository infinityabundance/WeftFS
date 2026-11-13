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
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use weftcodec::tensor::Region;
use weftfs::safetensors::{SafetensorsFile, SafetensorsTensor};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorWriteSpec};
use weftfs::{Hash32, TensorStrategy, TileDecodeOptions};

// ---------------------------------------------------------------------------
// Frozen contract constants (bench/CONTRACTS-ekbloom.md version 1; §9 is the
// version 2 E3 amendment)
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
// E3 frozen constants (bench/CONTRACTS-ekbloom.md §9, contract version 2)
// ---------------------------------------------------------------------------

/// Contract §9.3: region classes in bytes.
const READ_REGION_CLASSES: [u64; 3] = [1, 4_096, 65_536];
/// Contract §9.3: eight offsets per eligible (tensor, class).
const READ_OFFSETS_PER_CLASS: u64 = 8;
/// Contract §9.3: seven repetitions (rep 0 cold-first, reps 1-6 warm).
const READ_REPETITIONS: u32 = 7;
/// Contract §9.2: tile shape by rank.
const READ_TILE_1D: u64 = 128;
const READ_TILE_2D: u64 = 128;
/// Contract §9.5 G3: 2 * p95_E <= 5 * p95_best (2.5x, borrowed from
/// bench/CONTRACTS-context-advantage.md §3 C2).
const READ_P95_TOLERANCE_NUMERATOR: u64 = 5;
const READ_P95_TOLERANCE_DENOMINATOR: u64 = 2;
/// Contract §9.5 G3: the declared minimum sample counts for a p99.
const READ_MIN_POOLED_SAMPLES_FOR_P99: usize = 2_000;
const READ_MIN_CLASS_SAMPLES_FOR_P99: usize = 500;
/// Contract §9.1: the manifested EKBLOOM profile that provides a bounded path.
const READ_E_PROFILE: &str = "Sparse { interval: 1024 } (crate restart geometry)";
/// Contract §9.1: baseline alignment.
const READ_BASELINE_ALIGNMENT: u64 = 8;
/// Fixed store clock, matching the E1/E2 court.
const READ_FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;

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
        "usage: ekbloom-harness --phase boundary|space|read --out RECEIPT.json \\\n\
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
    if phase != "boundary" && phase != "space" && phase != "read" {
        eprintln!(
            "unsupported phase '{phase}' (this subphase implements boundary, space and read)"
        );
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
    repo: String,
    upstream_file: String,
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
        let repo = entry
            .get("repo")
            .and_then(Value::as_str)
            .ok_or_else(|| Skip {
                reason: format!("corpus manifest pair.{key}.repo is missing"),
            })?
            .to_owned();
        let upstream_file = entry
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| Skip {
                reason: format!("corpus manifest pair.{key}.file is missing"),
            })?
            .to_owned();
        // Local naming convention used by the neighbouring courts
        // (`scripts/s3_court.sh`, `scripts/demo-training-ready.sh`): the corpus
        // directory names a container by its Hugging Face repo basename. The
        // manifest's `file` field is the upstream filename and is recorded, but
        // it is not used to locate the local copy, so a stray upstream-named
        // file can never be silently picked up.
        let local_name = format!(
            "{}.safetensors",
            repo.rsplit('/').next().unwrap_or(repo.as_str())
        );
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
            repo,
            upstream_file,
            file: corpus_dir.join(local_name),
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

/// Bytes of one content-addressed blob.
///
/// Frames written by the tensor and codec representations are published as
/// `BlobKind::Manifest` (under `manifests/`), while raw chunks are
/// `BlobKind::Object` (under `objects/`). The locator therefore resolves either
/// namespace and fails closed when neither exists, rather than reporting a
/// silently smaller footprint.
fn referenced_blob_bytes(store: &Store, hash: Hash32) -> Result<u64, String> {
    let hex = hash.to_hex();
    for namespace in ["manifests", "objects"] {
        let path = store.root().join(namespace).join(&hex[0..2]).join(&hex);
        if let Ok(meta) = fs::metadata(&path) {
            return Ok(meta.len());
        }
    }
    Err(format!(
        "referenced blob {hex} is in neither manifests/ nor objects/"
    ))
}

fn object_footprint(store: &Store, object: Hash32) -> Result<u64, String> {
    let record = store
        .object_meta(object)
        .map_err(|err| format!("object_meta: {err}"))?;
    let manifest_file = weftfs::recovery::manifest_path(store.root(), record.content);
    let mut total = fs::metadata(&manifest_file)
        .map_err(|err| format!("stat {}: {err}", manifest_file.display()))?
        .len();
    let manifest = weftfs::recovery::read_manifest(store.root(), record.content)
        .map_err(|err| format!("read_manifest: {err}"))?;
    if let Some(representation) = &manifest.representation {
        if let Some(frame) = representation.frame {
            total += referenced_blob_bytes(store, frame)?;
        }
    }
    let mut counted: BTreeSet<Hash32> = BTreeSet::new();
    for chunk in &manifest.chunks {
        if counted.insert(chunk.hash) {
            total += referenced_blob_bytes(store, chunk.hash)?;
        }
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
        "read" => run_read(&args, &arms, &corpus_dir),
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
    if verdict != "pass" && !(verdict == "loss" && (args.phase == "space" || args.phase == "read"))
    {
        std::process::exit(1);
    }
}

fn base_receipt(args: &Args, phase: &str, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let corpus: Vec<Value> = arms
        .iter()
        .map(|arm| {
            json!({
                "arm": arm.arm.name,
                "repo": arm.arm.repo,
                "upstream_file": arm.arm.upstream_file,
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

// ---------------------------------------------------------------------------
// E3 — bounded random access (contract version 2, §9)
// ---------------------------------------------------------------------------

struct ReadObject<'a> {
    key: String,
    container: &'a str,
    tensor: &'a SafetensorsTensor,
    file: &'a SafetensorsFile,
    elem: u64,
    requests: Vec<(u64, u64)>,
}

#[derive(Clone)]
struct ARecord {
    offset: u64,
    length: u64,
    sha256: String,
}

struct WArm {
    store: Store,
    ids: BTreeMap<String, Hash32>,
    per_object: BTreeMap<String, u64>,
    head: String,
    build_ms: f64,
}

struct AArm {
    file: File,
    records: BTreeMap<String, ARecord>,
    build_ms: f64,
}

struct EngineProbe {
    plan_error: Option<String>,
    requests: Vec<Value>,
}

struct EArm {
    bodies: BTreeMap<String, ekbloom::FileBody>,
    containers: BTreeMap<String, Value>,
    probes: BTreeMap<String, EngineProbe>,
    build_ms: f64,
}

struct ReadSample {
    arm: &'static str,
    rep: u32,
    tensor: String,
    offset: u64,
    length: u64,
    ns: u64,
    exact: bool,
    bytes_read: u64,
    requests: u64,
    units: u64,
    encoded_bytes: u64,
}

fn element_bytes(dtype: &str) -> Result<u64, Refusal> {
    match dtype {
        "bf16" | "f16" | "i16" | "u16" => Ok(2),
        "f32" | "i32" | "u32" => Ok(4),
        "f64" | "i64" | "u64" => Ok(8),
        "i8" | "u8" | "bool" => Ok(1),
        other => Err(Refusal {
            kind: "unsupported_dtype",
            detail: format!("dtype `{other}` has no declared element width in this contract"),
        }),
    }
}

fn tile_shape_for(shape: &[u64]) -> Option<Vec<u64>> {
    match shape.len() {
        1 if shape[0] % READ_TILE_1D == 0 => Some(vec![READ_TILE_1D]),
        2 if shape[0] % READ_TILE_2D == 0 && shape[1] % READ_TILE_2D == 0 => {
            Some(vec![READ_TILE_2D, READ_TILE_2D])
        }
        _ => None,
    }
}

/// Contract §9.2: per `(dtype, shape)` class, the lexicographically smallest
/// tensor name. Applied to each container independently.
fn read_selection(file: &SafetensorsFile) -> Vec<&SafetensorsTensor> {
    let mut best: BTreeMap<(String, Vec<u64>), usize> = BTreeMap::new();
    for (index, tensor) in file.tensors.iter().enumerate() {
        let key = (tensor.dtype.clone(), tensor.shape.clone());
        let slot = best.entry(key).or_insert(index);
        if tensor.name < file.tensors[*slot].name {
            *slot = index;
        }
    }
    let mut selected: Vec<&SafetensorsTensor> =
        best.values().map(|&index| &file.tensors[index]).collect();
    selected.sort_by(|a, b| a.name.cmp(&b.name));
    selected
}

/// Contract §9.3: three region classes, eight offsets, `floor(i*(L-len)/7)`.
fn requests_for_tensor(tensor: &SafetensorsTensor) -> Vec<(u64, u64)> {
    let logical = tensor.byte_len();
    let mut out = Vec::new();
    for &length in &READ_REGION_CLASSES {
        if logical < length {
            continue;
        }
        for i in 0..READ_OFFSETS_PER_CLASS {
            let span = (logical - length) as u128;
            let offset = (i as u128 * span / (READ_OFFSETS_PER_CLASS as u128 - 1)) as u64;
            out.push((offset, length));
        }
    }
    out
}

/// The minimal row-aligned covering region of a contiguous byte range, and the
/// offset of the requested range inside the returned region bytes.
fn covering_region(
    shape: &[u64],
    elem: u64,
    offset: u64,
    length: u64,
) -> Result<(Region, usize), Refusal> {
    let end = offset.checked_add(length).ok_or_else(|| Refusal {
        kind: "invalid_region",
        detail: "offset + length overflows u64".to_owned(),
    })?;
    match shape.len() {
        1 => {
            let n = shape[0];
            let start = offset / elem;
            let stop = (end + elem - 1) / elem;
            if stop > n {
                return Err(Refusal {
                    kind: "invalid_region",
                    detail: format!("1-D region end element {stop} exceeds shape {n}"),
                });
            }
            let slice_start = (offset - start * elem) as usize;
            Ok((Region::new(vec![start], vec![stop]), slice_start))
        }
        2 => {
            let rows = shape[0];
            let cols = shape[1];
            let row_bytes = cols * elem;
            let start = offset / row_bytes;
            let stop = (end + row_bytes - 1) / row_bytes;
            if stop > rows {
                return Err(Refusal {
                    kind: "invalid_region",
                    detail: format!("2-D region end row {stop} exceeds shape {rows}x{cols}"),
                });
            }
            let slice_start = (offset - start * row_bytes) as usize;
            Ok((Region::new(vec![start, 0], vec![stop, cols]), slice_start))
        }
        rank => Err(Refusal {
            kind: "unsupported_rank",
            detail: format!("rank {rank} is not measured by this contract"),
        }),
    }
}

fn seed_for(key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"weft/ekbloom-read/w/v1");
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&digest);
    seed
}

fn read_tensor_bytes(
    file: &SafetensorsFile,
    tensor: &SafetensorsTensor,
) -> Result<Vec<u8>, Refusal> {
    file.read_tensor(tensor).map_err(|err| Refusal {
        kind: "corpus_read_failed",
        detail: format!("{}: {err}", tensor.name),
    })
}

fn rss_guard() -> Result<(), Refusal> {
    let rss = status_kib("VmRSS").unwrap_or(0).saturating_mul(1024);
    if rss > RSS_ABORT_BYTES {
        Err(Refusal {
            kind: "memory_bound",
            detail: format!(
                "process RSS {rss} B exceeds the declared abort bound {RSS_ABORT_BYTES} B"
            ),
        })
    } else {
        Ok(())
    }
}

fn rchar_bytes() -> Option<u64> {
    let text = fs::read_to_string("/proc/self/io").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rchar:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn dir_breakdown(root: &Path) -> (u64, Value) {
    let mut total = 0u64;
    let mut map = serde_json::Map::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata().ok();
            let bytes = if meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
                dir_bytes(&entry.path())
            } else {
                meta.map(|m| m.len()).unwrap_or(0)
            };
            total = total.saturating_add(bytes);
            map.insert(name, json!(bytes));
        }
    }
    (total, Value::Object(map))
}

fn counter_is_zero(value: &Value) -> Option<bool> {
    value
        .as_str()
        .and_then(|text| text.parse::<u64>().ok())
        .map(|number| number == 0)
}

/// Nearest-rank percentile over an ascending slice.
fn percentile_sorted(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len() as u128;
    let rank = ((p as u128 * n) + 99) / 100;
    let index = rank.max(1).min(n) - 1;
    sorted[index as usize]
}

fn latency_stats(samples: &[&ReadSample], p99_min_n: usize) -> Value {
    let mut values: Vec<u64> = samples.iter().map(|sample| sample.ns).collect();
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        return json!({
            "samples": 0,
            "mean_ns": Value::Null,
            "min_ns": Value::Null,
            "p50_ns": Value::Null,
            "p95_ns": Value::Null,
            "p99_ns": Value::Null,
            "p99_eligible": false,
            "max_ns": Value::Null,
        });
    }
    let sum: u128 = values.iter().map(|&value| value as u128).sum();
    json!({
        "samples": n,
        "mean_ns": sum as f64 / n as f64,
        "min_ns": values[0],
        "p50_ns": percentile_sorted(&values, 50),
        "p95_ns": percentile_sorted(&values, 95),
        "p99_ns": if n >= p99_min_n { json!(percentile_sorted(&values, 99)) } else { Value::Null },
        "p99_eligible": n >= p99_min_n,
        "max_ns": values[n - 1],
    })
}

fn p95_ns(samples: &[&ReadSample]) -> u64 {
    let mut values: Vec<u64> = samples.iter().map(|sample| sample.ns).collect();
    values.sort_unstable();
    percentile_sorted(&values, 95)
}

fn backend_summary(samples: &[&ReadSample]) -> Value {
    let n = samples.len();
    let bytes: u128 = samples.iter().map(|sample| sample.bytes_read as u128).sum();
    let requests: u128 = samples.iter().map(|sample| sample.requests as u128).sum();
    let units: u128 = samples.iter().map(|sample| sample.units as u128).sum();
    let encoded: u128 = samples
        .iter()
        .map(|sample| sample.encoded_bytes as u128)
        .sum();
    json!({
        "samples": n,
        "bytes_read": bytes as u64,
        "requests": requests as u64,
        "units": units as u64,
        "encoded_bytes": encoded as u64,
        "bytes_per_read": if n == 0 { Value::Null } else { json!(bytes as f64 / n as f64) },
        "requests_per_read": if n == 0 { Value::Null } else { json!(requests as f64 / n as f64) },
    })
}

fn build_w(root: &Path, objects: &[ReadObject<'_>]) -> Result<WArm, Refusal> {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("cannot create W root {}: {err}", root.display()),
    })?;
    let options = Options {
        fixed_timestamp_ns: Some(READ_FIXED_TIMESTAMP_NS),
        ..Options::default()
    };
    let mut store = Store::init(root, options).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("W Store::init: {err}"),
    })?;
    let root_id = store.resolve_path("/").map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("W resolve_path(/): {err}"),
    })?;
    let mut ids: BTreeMap<String, Hash32> = BTreeMap::new();
    let mut last = String::new();
    let started = Instant::now();
    let outcome = store.group(|inner| {
        for obj in objects {
            last = obj.key.clone();
            let tile = tile_shape_for(&obj.tensor.shape).ok_or_else(|| {
                weftfs::Error::invalid_argument(format!(
                    "shape {:?} does not divide the declared tile grid",
                    obj.tensor.shape
                ))
            })?;
            let bytes = obj.file.read_tensor(obj.tensor)?;
            let mut plan = TensorPlanOptions::default();
            // Only the forced mode-15 representation is emitted. Candidates that
            // cannot be chosen under the hint are disabled so the build stays
            // bounded; this changes no emitted byte of the mode-15 frame.
            plan.float_tensor = false;
            plan.zstd19 = false;
            plan.semantic_search = false;
            plan.zstd_encode_threads = 1;
            let spec = TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::TensorTiled),
                plan,
                tile_shape: Some(tile),
                ..TensorWriteSpec::default()
            };
            let id = inner.write_tensor_file_with_spec_seeded(
                root_id,
                obj.key.as_bytes(),
                &obj.tensor.dtype,
                &obj.tensor.shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                &spec,
                &seed_for(&obj.key),
            )?;
            ids.insert(obj.key.clone(), id);
            rss_guard().map_err(|refusal| weftfs::Error::io(refusal.detail))?;
        }
        Ok(())
    });
    let (_, head) = outcome.map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("W write failed (last object {last}): {err}"),
    })?;
    let build_ms = ms(started);
    drop(store);
    let read_store = Store::open(root, OpenMode::ReadOnly).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("W read-only open: {err}"),
    })?;
    let mut per_object = BTreeMap::new();
    for obj in objects {
        let id = ids.get(&obj.key).ok_or_else(|| Refusal {
            kind: "arm_build_failed",
            detail: format!("W object {} has no id", obj.key),
        })?;
        let footprint = object_footprint(&read_store, *id).map_err(|detail| Refusal {
            kind: "arm_build_failed",
            detail,
        })?;
        per_object.insert(obj.key.clone(), footprint);
    }
    Ok(WArm {
        store: read_store,
        ids,
        per_object,
        head: head.to_hex(),
        build_ms,
    })
}

fn build_a(root: &Path, objects: &[ReadObject<'_>]) -> Result<AArm, Refusal> {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("cannot create A root {}: {err}", root.display()),
    })?;
    let mut ordered: Vec<(&ReadObject<'_>, u64, u64)> = Vec::new();
    let mut offset = 0u64;
    for obj in objects {
        let length = obj.tensor.byte_len();
        let pad = (READ_BASELINE_ALIGNMENT - (offset % READ_BASELINE_ALIGNMENT))
            % READ_BASELINE_ALIGNMENT;
        offset = offset.saturating_add(pad);
        ordered.push((obj, offset, length));
        offset = offset.saturating_add(length);
    }
    let total = offset;
    let started = Instant::now();
    let payload_path = root.join("payload.raw");
    let mut payload = File::create(&payload_path).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("A payload create: {err}"),
    })?;
    let mut records: BTreeMap<String, ARecord> = BTreeMap::new();
    let mut manifest_records: Vec<Value> = Vec::new();
    let mut index_records: Vec<Value> = Vec::new();
    for (obj, record_offset, length) in &ordered {
        let bytes = read_tensor_bytes(obj.file, obj.tensor)?;
        if bytes.len() as u64 != *length {
            return Err(Refusal {
                kind: "arm_build_failed",
                detail: format!(
                    "A tensor {} read {} bytes, expected {length}",
                    obj.key,
                    bytes.len()
                ),
            });
        }
        let digest = sha256_hex(&bytes);
        payload
            .seek(SeekFrom::Start(*record_offset))
            .map_err(|err| Refusal {
                kind: "arm_build_failed",
                detail: format!("A payload seek {}: {err}", obj.key),
            })?;
        payload.write_all(&bytes).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A payload write {}: {err}", obj.key),
        })?;
        records.insert(
            obj.key.clone(),
            ARecord {
                offset: *record_offset,
                length: *length,
                sha256: digest.clone(),
            },
        );
        manifest_records.push(json!({
            "name": obj.key,
            "container": obj.container,
            "tensor": obj.tensor.name,
            "dtype": obj.tensor.dtype,
            "shape": obj.tensor.shape,
            "offset": record_offset,
            "length": length,
            "sha256": digest,
        }));
        index_records.push(json!({
            "name": obj.key,
            "offset": record_offset,
            "length": length,
        }));
        rss_guard()?;
    }
    let _ = payload.sync_all();
    drop(payload);
    let manifest = json!({
        "schema": "weft.ekbloom-read-baseline.v1",
        "payload_file": "payload.raw",
        "alignment_bytes": READ_BASELINE_ALIGNMENT,
        "payload_bytes": total,
        "tensors": manifest_records,
    });
    let index = json!({
        "schema": "weft.ekbloom-read-index.v1",
        "payload_file": "payload.raw",
        "tensors": index_records,
    });
    let manifest_path = root.join("manifest.json");
    let index_path = root.join("index.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A manifest encode: {err}"),
        })?,
    )
    .map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("A manifest write: {err}"),
    })?;
    fs::write(
        &index_path,
        serde_json::to_vec(&index).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A index encode: {err}"),
        })?,
    )
    .map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("A index write: {err}"),
    })?;
    // Open-time validation: the manifest and index must describe the same
    // records the builder wrote, and the payload must exist at the declared
    // length. The payload is not re-read here (it was just written; a re-read
    // would only warm the page cache).
    let read_manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A manifest re-read: {err}"),
        })?)
        .map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A manifest parse: {err}"),
        })?;
    let read_index: Value =
        serde_json::from_slice(&fs::read(&index_path).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A index re-read: {err}"),
        })?)
        .map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("A index parse: {err}"),
        })?;
    if read_manifest["schema"] != "weft.ekbloom-read-baseline.v1"
        || read_index["schema"] != "weft.ekbloom-read-index.v1"
        || read_manifest["payload_bytes"].as_u64() != Some(total)
        || fs::metadata(&payload_path).map(|meta| meta.len()).ok() != Some(total)
        || read_manifest["tensors"].as_array().map(|rows| rows.len()) != Some(records.len())
        || read_index["tensors"].as_array().map(|rows| rows.len()) != Some(records.len())
    {
        return Err(Refusal {
            kind: "arm_build_failed",
            detail: "A manifest/index failed open-time validation".to_owned(),
        });
    }
    let file = File::open(&payload_path).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("A payload open: {err}"),
    })?;
    Ok(AArm {
        file,
        records,
        build_ms: ms(started),
    })
}

fn probe_engine(container: &Path, expected: &[u8], requests: &[(u64, u64)]) -> EngineProbe {
    let body = match ekbloom::open(container) {
        Ok(body) => body,
        Err(err) => {
            return EngineProbe {
                plan_error: Some(format!("open: {err}")),
                requests: Vec::new(),
            }
        }
    };
    let workload = ekbloom::Workload {
        kind: ekbloom::WorkloadKind::RangeMixed,
        hot_prefix: None,
        sample: requests.to_vec(),
    };
    let budget = ekbloom::ManifestationBudget {
        maximum_physical_bytes: u64::MAX,
    };
    let engine = match ekbloom::Engine::plan(&body, &budget, &workload) {
        Ok(engine) => engine,
        Err(err) => {
            return EngineProbe {
                plan_error: Some(format!("plan: {err}")),
                requests: Vec::new(),
            }
        }
    };
    let mut rows = Vec::new();
    for &(offset, length) in requests {
        let request = ekbloom::ReadRequest { offset, length };
        let claimed = engine.worst_case_steps_for(request);
        match engine.read_counted_steps(&body, request) {
            Ok((bytes, steps)) => {
                let exact = bytes.len() == length as usize
                    && bytes.as_slice() == &expected[offset as usize..(offset + length) as usize];
                rows.push(json!({
                    "offset": offset,
                    "length": length,
                    "claimed_steps": claimed,
                    "actual_positioning": steps.positioning,
                    "exact": exact,
                }));
            }
            Err(err) => rows.push(json!({
                "offset": offset,
                "length": length,
                "claimed_steps": claimed,
                "error": format!("{err}"),
            })),
        }
    }
    EngineProbe {
        plan_error: None,
        requests: rows,
    }
}

fn build_e(root: &Path, objects: &[ReadObject<'_>]) -> Result<EArm, Refusal> {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).map_err(|err| Refusal {
        kind: "arm_build_failed",
        detail: format!("cannot create E root {}: {err}", root.display()),
    })?;
    let started = Instant::now();
    let mut bodies = BTreeMap::new();
    let mut containers = BTreeMap::new();
    let mut probes = BTreeMap::new();
    for (index, obj) in objects.iter().enumerate() {
        let bytes = read_tensor_bytes(obj.file, obj.tensor)?;
        let logical = bytes.len() as u64;
        let container = root.join(format!("{index:02}.ekb"));
        let body = ekbloom::create_auto(
            &bytes,
            ekbloom::ManifestationProfile::Sparse { interval: 1024 },
        )
        .map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("E create_auto {}: {err}", obj.key),
        })?;
        let physical = body.physical_bytes();
        let machine = body.machine_id;
        let profiles = body.profiles.clone();
        let logical_len = body.logical_len;
        if physical > logical.saturating_mul(BODY_CAP_MULTIPLE) {
            return Err(Refusal {
                kind: "memory_bound",
                detail: format!(
                    "E body {physical} B for {} exceeds the contract's {BODY_CAP_MULTIPLE}x logical cap ({} B)",
                    obj.key,
                    logical.saturating_mul(BODY_CAP_MULTIPLE)
                ),
            });
        }
        ekbloom::write_body(&container, &body).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("E write_body {}: {err}", obj.key),
        })?;
        let body_bytes = fs::metadata(&container)
            .map_err(|err| Refusal {
                kind: "arm_build_failed",
                detail: format!("E stat {}: {err}", obj.key),
            })?
            .len();
        drop(body);
        let rchar_before = rchar_bytes();
        let open_started = Instant::now();
        let file_body = ekbloom::FileBody::open(&container).map_err(|err| Refusal {
            kind: "arm_build_failed",
            detail: format!("E FileBody::open {}: {err}", obj.key),
        })?;
        let open_ms = ms(open_started);
        let open_rchar = match (rchar_before, rchar_bytes()) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        let probe = probe_engine(&container, &bytes, &obj.requests);
        bodies.insert(obj.key.clone(), file_body);
        containers.insert(
            obj.key.clone(),
            json!({
                "container": container.file_name().map(|name| name.to_string_lossy().to_string()),
                "logical_bytes": logical,
                "body_bytes": body_bytes,
                "in_memory_physical_bytes": physical,
                "container_overhead_bytes": body_bytes.saturating_sub(physical),
                "machine_id": machine,
                "profiles": profiles,
                "logical_len": logical_len,
                "file_body_open_ms": open_ms,
                "file_body_open_rchar_bytes": open_rchar,
                "engine_probe": Value::Null,
            }),
        );
        probes.insert(obj.key.clone(), probe);
        rss_guard()?;
    }
    Ok(EArm {
        bodies,
        containers,
        probes,
        build_ms: ms(started),
    })
}

fn measure_pass(
    rep: u32,
    record: bool,
    objects: &[ReadObject<'_>],
    w: &WArm,
    a: &AArm,
    e: &EArm,
    samples: &mut Vec<ReadSample>,
    exact_failures: &mut Vec<Value>,
) -> Result<(), Refusal> {
    let shift = if rep == u32::MAX {
        0
    } else {
        (rep % 3) as usize
    };
    let base = ["W", "A", "E"];
    let order = [base[shift], base[(shift + 1) % 3], base[(shift + 2) % 3]];
    let mut scratch: Vec<u8> = Vec::new();
    for obj in objects {
        let source = read_tensor_bytes(obj.file, obj.tensor)?;
        for arm in order {
            for &(offset, length) in &obj.requests {
                let expected = &source[offset as usize..(offset + length) as usize];
                let mut sample = ReadSample {
                    arm,
                    rep: if rep == u32::MAX { 0 } else { rep },
                    tensor: obj.key.clone(),
                    offset,
                    length,
                    ns: 0,
                    exact: false,
                    bytes_read: 0,
                    requests: 0,
                    units: 0,
                    encoded_bytes: 0,
                };
                match arm {
                    "W" => {
                        let (region, slice_start) =
                            covering_region(&obj.tensor.shape, obj.elem, offset, length)?;
                        let id = *w.ids.get(&obj.key).ok_or_else(|| Refusal {
                            kind: "arm_read_failed",
                            detail: format!("W has no id for {}", obj.key),
                        })?;
                        let before = w.store.backend_stats();
                        let started = Instant::now();
                        let (read, stats) = w
                            .store
                            .read_tensor_region_with_options_and_stats(
                                id,
                                &region,
                                &TileDecodeOptions::default(),
                            )
                            .map_err(|err| Refusal {
                                kind: "arm_read_failed",
                                detail: format!("W read {}: {err}", obj.key),
                            })?;
                        sample.ns = started.elapsed().as_nanos() as u64;
                        let after = w.store.backend_stats();
                        sample.bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
                        sample.requests = after.reads.saturating_sub(before.reads);
                        sample.units = stats.tiles as u64;
                        let stop = slice_start.saturating_add(length as usize);
                        sample.exact = read.bytes.len() >= stop
                            && &read.bytes[slice_start..stop] == expected
                            && sha256_hex(&read.bytes[slice_start..stop]) == sha256_hex(expected);
                    }
                    "A" => {
                        let record = a.records.get(&obj.key).ok_or_else(|| Refusal {
                            kind: "arm_read_failed",
                            detail: format!("A has no record for {}", obj.key),
                        })?;
                        if scratch.len() != length as usize {
                            scratch.resize(length as usize, 0);
                        }
                        let started = Instant::now();
                        a.file
                            .read_exact_at(&mut scratch, record.offset + offset)
                            .map_err(|err| Refusal {
                                kind: "arm_read_failed",
                                detail: format!("A read {}: {err}", obj.key),
                            })?;
                        sample.ns = started.elapsed().as_nanos() as u64;
                        sample.bytes_read = length;
                        sample.requests = 1;
                        sample.units = 1;
                        sample.exact = scratch.as_slice() == expected
                            && sha256_hex(&scratch) == sha256_hex(expected);
                    }
                    "E" => {
                        let body = e.bodies.get(&obj.key).ok_or_else(|| Refusal {
                            kind: "arm_read_failed",
                            detail: format!("E has no body for {}", obj.key),
                        })?;
                        let before = (
                            body.fetched_bytes(),
                            body.fetched_reads(),
                            body.fetched_encoded_bytes(),
                        );
                        let started = Instant::now();
                        let bytes =
                            body.read(ekbloom::ReadRequest { offset, length })
                                .map_err(|err| Refusal {
                                    kind: "arm_read_failed",
                                    detail: format!("E read {}: {err}", obj.key),
                                })?;
                        sample.ns = started.elapsed().as_nanos() as u64;
                        let after = (
                            body.fetched_bytes(),
                            body.fetched_reads(),
                            body.fetched_encoded_bytes(),
                        );
                        sample.bytes_read = after.0.saturating_sub(before.0);
                        sample.requests = after.1.saturating_sub(before.1);
                        sample.units = sample.requests;
                        sample.encoded_bytes = after.2.saturating_sub(before.2);
                        sample.exact = bytes.as_slice() == expected
                            && sha256_hex(&bytes) == sha256_hex(expected);
                    }
                    _ => unreachable!(),
                }
                if record {
                    if !sample.exact && exact_failures.len() < 16 {
                        exact_failures.push(json!({
                            "arm": sample.arm,
                            "rep": sample.rep,
                            "tensor": sample.tensor,
                            "offset": sample.offset,
                            "length": sample.length,
                        }));
                    }
                    samples.push(sample);
                }
            }
        }
    }
    Ok(())
}

fn read_base_receipt(args: &Args, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let mut receipt = base_receipt(args, "read", arms, corpus_dir);
    receipt["contract"] = json!("bench/CONTRACTS-ekbloom.md (version 2, §9)");
    receipt["feature_string"] = json!(
        "weftfs: default (no features); weftcodec: path dependency; ekbloom: default (no features)"
    );
    receipt["upstream"]["api_used"] = json!([
        "ekbloom::create_auto(&[u8], ManifestationProfile::Sparse { interval: 1024 }) -> EkbloomBody",
        "ekbloom::write_body(&Path, &EkbloomBody)",
        "ekbloom::FileBody::open(&Path) -> FileBody",
        "FileBody::read(ReadRequest { offset, length }) -> Vec<u8>",
        "FileBody::fetched_bytes() / fetched_reads() / fetched_encoded_bytes() -> u64",
        "ekbloom::open(&Path) -> EkbloomBody; ekbloom::Engine::plan(..); Engine::worst_case_steps_for(..); Engine::read_counted_steps(..) [supporting probe, not the gating path]",
        "weftfs: Store::group, Store::write_tensor_file_with_spec_seeded(TensorStrategy::TensorTiled), Store::open(ReadOnly), Store::read_tensor_region_with_options_and_stats, Store::backend_stats",
    ]);
    receipt["e3_contract"] = json!({
        "region_classes_bytes": READ_REGION_CLASSES,
        "offsets_per_class": READ_OFFSETS_PER_CLASS,
        "repetitions": READ_REPETITIONS,
        "repetition_0": "cold-first (first timed pass, no warm-up)",
        "repetitions_1_6": "warm (one untimed full pass precedes them)",
        "tile_shape_rank_1": [READ_TILE_1D],
        "tile_shape_rank_2": [READ_TILE_2D, READ_TILE_2D],
        "ebloom_profile": READ_E_PROFILE,
        "baseline_alignment_bytes": READ_BASELINE_ALIGNMENT,
        "p99_min_pooled_samples": READ_MIN_POOLED_SAMPLES_FOR_P99,
        "p99_min_class_samples": READ_MIN_CLASS_SAMPLES_FOR_P99,
        "p95_tolerance": {
            "numerator": READ_P95_TOLERANCE_NUMERATOR,
            "denominator": READ_P95_TOLERANCE_DENOMINATOR,
            "integer_form": "2 * p95_E_ns <= 5 * p95_best_ns",
            "citation": "bench/CONTRACTS-context-advantage.md §3 (C — fast-path neutrality), C2: p95 <= 2.5x",
            "declared_before_measurement": true,
            "operator_veto": "docs/EKBLOOM-PLAN.md §9 item 1 (E3 gate shape)",
        },
    });
    receipt
}

fn read_loss_receipt(mut receipt: Value, kind: &str, detail: &str) -> Value {
    receipt["refusal"] = json!({"kind": kind, "detail": detail});
    receipt["verdict"] = json!("loss");
    receipt["checks"] = json!([
        {"id": "C_arms_built", "pass": false, "detail": detail},
    ]);
    receipt["cgroup"] = json!({
        "memory_peak_bytes": cgroup_value("/sys/fs/cgroup/memory.peak"),
        "memory_max_bytes": cgroup_value("/sys/fs/cgroup/memory.max"),
        "pids_events_max": cgroup_pids_events_max(),
        "pids_current": cgroup_value("/sys/fs/cgroup/pids.current"),
        "pids_max": cgroup_value("/sys/fs/cgroup/pids.max"),
        "oom_kill": cgroup_oom_kill(),
        "process_peak_rss_kib": status_kib("VmHWM").unwrap_or(0),
        "process_rss_kib": status_kib("VmRSS").unwrap_or(0),
    });
    receipt["limitations"] = json!([
        "typed arm build refusal; the gate was not evaluated and no number is reported for it",
        "one host, one process, local POSIX; no fleet, cloud, media, power-loss or GPU claim",
    ]);
    receipt
}

fn run_read(args: &Args, arms: &[VerifiedArm], corpus_dir: &Path) -> Value {
    let mut receipt = read_base_receipt(args, arms, corpus_dir);
    let mut objects: Vec<ReadObject<'_>> = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();
    for arm in arms {
        let selected = read_selection(&arm.file);
        for tensor in selected {
            let elem = match element_bytes(&tensor.dtype) {
                Ok(elem) => elem,
                Err(refusal) => {
                    excluded.push(json!({
                        "container": arm.arm.name,
                        "name": tensor.name,
                        "reason": refusal.detail,
                    }));
                    continue;
                }
            };
            if tile_shape_for(&tensor.shape).is_none() {
                excluded.push(json!({
                    "container": arm.arm.name,
                    "name": tensor.name,
                    "shape": tensor.shape,
                    "reason": "shape does not divide the declared [128] / [128,128] tile grid",
                }));
                continue;
            }
            let key = format!("{}__{}", arm.arm.name, tensor.name);
            objects.push(ReadObject {
                key,
                container: arm.arm.name,
                tensor,
                file: &arm.file,
                elem,
                requests: requests_for_tensor(tensor),
            });
        }
    }
    objects.sort_by(|left, right| left.key.cmp(&right.key));
    let logical_bytes: u64 = objects.iter().map(|obj| obj.tensor.byte_len()).sum();
    let mut per_container: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for obj in &objects {
        let entry = per_container.entry(obj.container).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(obj.tensor.byte_len());
    }
    receipt["selection"] = json!({
        "rule": "contract §9.2: per (dtype, shape) class, the lexicographically smallest tensor name; applied to base and instruct independently",
        "objects": objects.len(),
        "expected_objects": 14,
        "logical_bytes": logical_bytes,
        "per_container": per_container.iter().map(|(container, (count, bytes))| json!({
            "container": container,
            "objects": count,
            "logical_bytes": bytes,
        })).collect::<Vec<_>>(),
        "excluded": excluded,
    });
    if objects.is_empty() {
        return read_loss_receipt(
            receipt,
            "no_usable_tensors",
            "no tensor survived the declared selection rule",
        );
    }

    let w_root = args.work.join("arm-w");
    let a_root = args.work.join("arm-a");
    let e_root = args.work.join("arm-e");
    receipt["roots"] = json!({
        "W": w_root.display().to_string(),
        "A": a_root.display().to_string(),
        "E": e_root.display().to_string(),
    });

    let w = match build_w(&w_root, &objects) {
        Ok(w) => w,
        Err(refusal) => {
            return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
        }
    };
    let a = match build_a(&a_root, &objects) {
        Ok(a) => a,
        Err(refusal) => {
            return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
        }
    };
    let e = match build_e(&e_root, &objects) {
        Ok(e) => e,
        Err(refusal) => {
            return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
        }
    };

    let w_head = w.head.clone();
    let w_build_ms = w.build_ms;
    let w_objects = w.ids.len();
    let a_build_ms = a.build_ms;
    let a_records = a.records.len();
    let e_build_ms = e.build_ms;
    let mut e_containers = serde_json::Map::new();
    for (key, value) in &e.containers {
        e_containers.insert(key.clone(), value.clone());
    }
    let mut engine_probes = serde_json::Map::new();
    for (key, probe) in &e.probes {
        engine_probes.insert(
            key.clone(),
            json!({
                "plan_error": probe.plan_error,
                "requests": probe.requests,
            }),
        );
    }
    let mut rows: Vec<Value> = Vec::new();
    for obj in &objects {
        let id = w.ids.get(&obj.key);
        let record = a.records.get(&obj.key);
        rows.push(json!({
            "container": obj.container,
            "name": obj.tensor.name,
            "key": obj.key,
            "dtype": obj.tensor.dtype,
            "shape": obj.tensor.shape,
            "logical_bytes": obj.tensor.byte_len(),
            "element_bytes": obj.elem,
            "requests": obj.requests.len(),
            "W": {
                "object_id": id.map(|id| id.to_hex()),
                "object_footprint_bytes": w.per_object.get(&obj.key),
            },
            "A": {
                "offset": record.map(|record| record.offset),
                "length": record.map(|record| record.length),
                "sha256": record.map(|record| record.sha256.clone()),
            },
            "E": e.containers.get(&obj.key),
        }));
    }
    receipt["rows"] = json!(rows);
    // Contract §9.6: a digest over the per-object retained sizes and content
    // hashes, so a repeated run can be compared structurally.
    let mut determinism = Sha256::new();
    determinism.update(b"weft/ekbloom-read/determinism/v1");
    for obj in &objects {
        determinism.update(obj.key.as_bytes());
        determinism.update(
            w.per_object
                .get(&obj.key)
                .copied()
                .unwrap_or(0)
                .to_le_bytes(),
        );
        if let Some(record) = a.records.get(&obj.key) {
            determinism.update(record.length.to_le_bytes());
            determinism.update(record.sha256.as_bytes());
        }
        if let Some(Value::Object(map)) = e.containers.get(&obj.key) {
            let body_bytes = map.get("body_bytes").and_then(Value::as_u64).unwrap_or(0);
            determinism.update(body_bytes.to_le_bytes());
        }
    }
    receipt["determinism_digest"] = json!(hex(&determinism.finalize()));
    receipt["determinism_inputs"] =
        json!("SHA-256 over (key, W whole-object footprint, A length, A source SHA-256, E body_bytes) in ascending key order");
    let base_order = ["W", "A", "E"];
    let repetition_order: Vec<Value> = (0..READ_REPETITIONS)
        .map(|rep| {
            let shift = (rep % 3) as usize;
            json!([
                base_order[shift],
                base_order[(shift + 1) % 3],
                base_order[(shift + 2) % 3]
            ])
        })
        .collect();
    receipt["repetition_order"] = json!(repetition_order);
    receipt["repetition_semantics"] = json!({
        "0": "cold-first: first timed pass after construction, no warm-up",
        "1_6": "warm: one untimed full pass precedes them",
        "cold_definition": "process-cold only; the court cannot drop the host page cache",
    });
    receipt["construction"] = json!({
        "W": {
            "build_ms": w_build_ms,
            "head": w_head,
            "objects": w_objects,
            "write_options": {
                "strategy_hint": "tensor-tiled",
                "tile_shape_rank_1": [READ_TILE_1D],
                "tile_shape_rank_2": [READ_TILE_2D, READ_TILE_2D],
                "planner_options": {
                    "float_tensor": false,
                    "zstd19": false,
                    "semantic_search": false,
                    "zstd_encode_threads": 1,
                    "note": "candidates that cannot be chosen under the forced mode-15 hint are disabled; no emitted mode-15 byte changes",
                },
                "commit": "one Store::group",
            },
        },
        "A": {
            "build_ms": a_build_ms,
            "records": a_records,
            "payload_file": "payload.raw",
            "manifest_file": "manifest.json",
            "index_file": "index.json",
            "alignment_bytes": READ_BASELINE_ALIGNMENT,
            "open_validation": "manifest and index schema, record count, offsets, lengths and payload length are validated once at open; the payload is not re-read (it was just written, and a re-read would only warm the page cache)",
        },
        "E": {
            "build_ms": e_build_ms,
            "profile": READ_E_PROFILE,
            "containers": e_containers,
            "engine_probe": engine_probes,
            "note": "the engine probe is the in-memory Engine at an unbounded budget; it is supporting evidence, not the gating FileBody path",
            "open_io_note": "FileBody::open validates the container by streaming it; per-container file_body_open_ms and file_body_open_rchar_bytes are reported and are never added to a read's latency",
        },
    });

    let mut samples: Vec<ReadSample> = Vec::new();
    let mut exact_failures: Vec<Value> = Vec::new();
    if let Err(refusal) = measure_pass(
        0,
        true,
        &objects,
        &w,
        &a,
        &e,
        &mut samples,
        &mut exact_failures,
    ) {
        return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
    }
    // One untimed full pass precedes the six warm repetitions (contract §9.3).
    if let Err(refusal) = measure_pass(
        u32::MAX,
        false,
        &objects,
        &w,
        &a,
        &e,
        &mut samples,
        &mut exact_failures,
    ) {
        return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
    }
    for rep in 1..READ_REPETITIONS {
        if let Err(refusal) = measure_pass(
            rep,
            true,
            &objects,
            &w,
            &a,
            &e,
            &mut samples,
            &mut exact_failures,
        ) {
            return read_loss_receipt(receipt, refusal.kind, &refusal.detail);
        }
    }

    // Footprint is measured after every arm handle is dropped (contract §9.4).
    drop(w.store);
    drop(a.file);
    drop(e.bodies);
    let (w_bytes, w_breakdown) = dir_breakdown(&w_root);
    let (a_bytes, a_breakdown) = dir_breakdown(&a_root);
    let (e_bytes, e_breakdown) = dir_breakdown(&e_root);

    let pooled = |arm: &str| -> Vec<&ReadSample> {
        samples.iter().filter(|sample| sample.arm == arm).collect()
    };
    let by_class = |arm: &str, length: u64| -> Vec<&ReadSample> {
        samples
            .iter()
            .filter(|sample| sample.arm == arm && sample.length == length)
            .collect()
    };
    let class_stats = |arm: &str| -> Value {
        let mut map = serde_json::Map::new();
        for length in READ_REGION_CLASSES {
            map.insert(
                length.to_string(),
                latency_stats(&by_class(arm, length), READ_MIN_CLASS_SAMPLES_FOR_P99),
            );
        }
        Value::Object(map)
    };
    let latency = json!({
        "W": {"pooled": latency_stats(&pooled("W"), READ_MIN_POOLED_SAMPLES_FOR_P99), "by_class": class_stats("W")},
        "A": {"pooled": latency_stats(&pooled("A"), READ_MIN_POOLED_SAMPLES_FOR_P99), "by_class": class_stats("A")},
        "E": {"pooled": latency_stats(&pooled("E"), READ_MIN_POOLED_SAMPLES_FOR_P99), "by_class": class_stats("E")},
        "percentile_method": "nearest-rank over the pooled samples; per-class statistics are observations",
    });
    let backend = json!({
        "W": backend_summary(&pooled("W")),
        "A": backend_summary(&pooled("A")),
        "E": backend_summary(&pooled("E")),
        "method": {
            "W": "Store::backend_stats() deltas around the read (reads, range_reads, bytes_read) plus TileDecodeStats.tiles",
            "A": "one pread per read, exactly the requested length; counted by the harness",
            "E": "FileBody::fetched_bytes/fetched_reads/fetched_encoded_bytes deltas; open-time I/O is reported separately and excluded",
        },
    });
    let exactness = json!({
        "W": {"samples": pooled("W").len(), "exact": pooled("W").iter().filter(|sample| sample.exact).count()},
        "A": {"samples": pooled("A").len(), "exact": pooled("A").iter().filter(|sample| sample.exact).count()},
        "E": {"samples": pooled("E").len(), "exact": pooled("E").iter().filter(|sample| sample.exact).count()},
        "all_exact": samples.iter().all(|sample| sample.exact),
        "first_failures": exact_failures,
    });
    let footprint = json!({
        "logical_bytes": logical_bytes,
        "W": {"retained_bytes": w_bytes, "breakdown": w_breakdown},
        "A": {"retained_bytes": a_bytes, "breakdown": a_breakdown},
        "E": {"retained_bytes": e_bytes, "breakdown": e_breakdown},
        "E_over_W_ppm": if w_bytes > 0 { (e_bytes as u128 * 1_000_000 / w_bytes as u128) as u64 } else { 0 },
        "E_over_A_ppm": if a_bytes > 0 { (e_bytes as u128 * 1_000_000 / a_bytes as u128) as u64 } else { 0 },
        "includes": "regular files only: W is the whole store tree (FORMAT, LOCK, superblock, commits, objects, manifests, indexes, refs, catalog); A is payload.raw + manifest.json + index.json; E is every .ekb container. Directory metadata and block-size rounding are excluded for all arms.",
    });

    let exact_ok = !samples.is_empty() && samples.iter().all(|sample| sample.exact);
    let footprint_ok = e_bytes < w_bytes && e_bytes < a_bytes;
    let p95_e = p95_ns(&pooled("E"));
    let p95_w = p95_ns(&pooled("W"));
    let p95_a = p95_ns(&pooled("A"));
    let p95_best = p95_w.min(p95_a);
    let p95_holds = (p95_e as u128) * READ_P95_TOLERANCE_DENOMINATOR as u128
        <= (p95_best as u128) * READ_P95_TOLERANCE_NUMERATOR as u128;
    let oom_zero = counter_is_zero(&cgroup_oom_kill());
    let pids_zero = counter_is_zero(&cgroup_pids_events_max());
    let resource_ok = oom_zero == Some(true) && pids_zero == Some(true);
    let pass = exact_ok && footprint_ok && p95_holds && resource_ok;
    let verdict = if !exact_ok {
        "fail"
    } else if pass {
        "pass"
    } else {
        "loss"
    };
    receipt["latency"] = latency;
    receipt["backend"] = backend;
    receipt["exactness"] = exactness;
    let mut expected_by_class = BTreeMap::new();
    for obj in &objects {
        for &(_, length) in &obj.requests {
            *expected_by_class.entry(length).or_insert(0usize) += READ_REPETITIONS as usize;
        }
    }
    let mut actual_by_class = BTreeMap::new();
    for &length in &READ_REGION_CLASSES {
        actual_by_class.insert(
            length,
            samples
                .iter()
                .filter(|sample| sample.arm == "W" && sample.length == length)
                .count(),
        );
    }
    let actual_pooled = samples.iter().filter(|sample| sample.arm == "W").count();
    let expected_pooled: usize = expected_by_class.values().sum();
    receipt["sample_counts"] = json!({
        "per_arm": true,
        "arms": ["W", "A", "E"],
        "expected_by_class": expected_by_class,
        "actual_by_class": actual_by_class,
        "expected_pooled": expected_pooled,
        "actual_pooled": actual_pooled,
        "p99_pooled_eligible": actual_pooled >= READ_MIN_POOLED_SAMPLES_FOR_P99,
        "p99_class_eligible": actual_by_class.iter().map(|(length, count)| (length.to_string(), *count >= READ_MIN_CLASS_SAMPLES_FOR_P99)).collect::<BTreeMap<_, _>>(),
        "note": "counts are per arm; all three arms measured the same 1,904 reads. The contract's stated expected counts (784 / 672 / 672 = 2,128 pooled) assumed every selected tensor was eligible for every class; the contract's own L >= len rule excludes the four rank-1 tensors (two per container) from the 4,096 and 65,536 classes, so the actual counts are 784 / 560 / 560 = 1,904 pooled. The gate uses the pooled p95 and is unaffected; the pooled p99 is reported as ineligible, and every per-class p99 is eligible (>= 500).",
    });
    receipt["footprint"] = footprint;
    receipt["gate"] = json!({
        "G1_exactness": exact_ok,
        "G2_footprint": footprint_ok,
        "G3_p95": p95_holds,
        "G4_resource": resource_ok,
        "E_footprint_bytes": e_bytes,
        "W_footprint_bytes": w_bytes,
        "A_footprint_bytes": a_bytes,
        "E_strictly_smaller_than_W": e_bytes < w_bytes,
        "E_strictly_smaller_than_A": e_bytes < a_bytes,
        "p95_E_ns": p95_e,
        "p95_W_ns": p95_w,
        "p95_A_ns": p95_a,
        "p95_best_ns": p95_best,
        "tolerance": {
            "numerator": READ_P95_TOLERANCE_NUMERATOR,
            "denominator": READ_P95_TOLERANCE_DENOMINATOR,
            "integer_check": "2 * p95_E_ns <= 5 * p95_best_ns",
            "citation": "bench/CONTRACTS-context-advantage.md §3 (C — fast-path neutrality), C2: p95 <= 2.5x",
        },
        "p95_lhs": ((p95_e as u128) * READ_P95_TOLERANCE_DENOMINATOR as u128) as u64,
        "p95_rhs": ((p95_best as u128) * READ_P95_TOLERANCE_NUMERATOR as u128) as u64,
        "pass": pass,
    });
    receipt["checks"] = json!([
        {"id": "C_arms_built", "pass": true, "detail": "W, A and E constructed and opened"},
        {"id": "G1_exactness", "pass": exact_ok, "detail": format!("{} samples; {} exact", samples.len(), samples.iter().filter(|sample| sample.exact).count())},
        {"id": "G2_footprint", "pass": footprint_ok, "detail": format!("E {e_bytes} B; W {w_bytes} B; A {a_bytes} B; strict against both: {footprint_ok}")},
        {"id": "G3_p95", "pass": p95_holds, "detail": format!("2 * {p95_e} <= 5 * {p95_best} (E p95 {} ns; W {} ns; A {} ns)", p95_e, p95_w, p95_a)},
        {"id": "G4_resource", "pass": resource_ok, "detail": format!("oom_kill zero: {oom_zero:?}; pids.events max zero: {pids_zero:?}")},
        {"id": "C_engine_bound", "pass": engine_probes.values().all(|probe| probe
            .get("requests").and_then(Value::as_array).map(|rows| rows.iter().all(|row| row.get("error").is_none() && row.get("exact").and_then(Value::as_bool) == Some(true))).unwrap_or(false)),
         "gating": false,
         "detail": "supporting in-memory Engine probe: every request decoded exactly and no read errored (not the gating FileBody path)"},
    ]);
    receipt["samples"] = json!(samples
        .iter()
        .map(|sample| json!({
            "arm": sample.arm,
            "rep": sample.rep,
            "tensor": sample.tensor,
            "offset": sample.offset,
            "length": sample.length,
            "ns": sample.ns,
            "exact": sample.exact,
            "bytes_read": sample.bytes_read,
            "requests": sample.requests,
            "units": sample.units,
            "encoded_bytes": sample.encoded_bytes,
        }))
        .collect::<Vec<_>>());
    receipt["cgroup"] = json!({
        "memory_peak_bytes": cgroup_value("/sys/fs/cgroup/memory.peak"),
        "memory_max_bytes": cgroup_value("/sys/fs/cgroup/memory.max"),
        "pids_events_max": cgroup_pids_events_max(),
        "pids_current": cgroup_value("/sys/fs/cgroup/pids.current"),
        "pids_max": cgroup_value("/sys/fs/cgroup/pids.max"),
        "oom_kill": cgroup_oom_kill(),
        "process_peak_rss_kib": status_kib("VmHWM").unwrap_or(0),
        "process_rss_kib": status_kib("VmRSS").unwrap_or(0),
    });
    receipt["limitations"] = json!([
        "process-cold first pass only: the court cannot drop the host page cache and the data was just written; no media or power-loss claim",
        "A is a raw local payload + canonical manifest + index; no compression and no decode",
        "E is the file-backed FileBody path on a Sparse{1024} container; the in-memory Engine bound is supporting evidence, not the gating path",
        "W uses mode 15 with tile [128] / [128,128] and a read-only store with no decoded-object cache and no single-flight",
        "E open-time I/O is reported separately (container bytes and /proc/self/io rchar) and excluded from read latency",
        "per-class and strict latency wins are observations; the gate is the pooled p95 and the strict footprint",
        "footprint excludes directory metadata and block-size rounding for all three arms equally",
        "one host, one process, local POSIX, warm page cache; no fleet, cloud, media, power-loss, GPU or serving claim",
    ]);
    receipt["verdict"] = json!(verdict);
    receipt
}
