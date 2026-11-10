//! Attested load court harness (subphase R12, increment 3).
//!
//!   attested_load load   --store DIR --checkpoint NAME [--tensors a,b]
//!                        [--region-rows N] --replica NAME --principal NAME
//!                        [--seed FILE] [--snapshot HEX] --out RECORD.wcb
//!   attested_load verify --store DIR --receipt RECORD.wcb
//!                        [--trust HEX[,HEX]] [--require-signature]
//!
//! `load` reads the selected tensors through the shipped checkpoint restore
//! path (which verifies every shard/group root and the whole-tensor root as it
//! consumes them) and emits a canonical `weft.load-attestation.v1` record.
//!
//! `verify` is meant to run as an **independent process**: it decodes the
//! record, verifies the Ed25519 signature, re-reads every recorded byte range
//! from the store and re-hashes it, and reports the tensors the record marks
//! `unverified` without ever counting them as verified.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftcodec::tensor::Region;
use weftfs::backend::BackendKind;
use weftfs::identity::Hash32;
use weftfs::load_attestation::{
    attest_checkpoint_load, verify_against_store, AttestOptions, AttestStatus, LoadAttestation,
    VerifyOptions,
};
use weftfs::receipt_bundle::read_seed;
use weftfs::store::{OpenMode, Options, Store};

fn main() {
    if let Err(err) = run() {
        eprintln!("attested_load: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "load" => load(&args[1..]),
        "verify" => verify(&args[1..]),
        other => Err(format!("unknown phase `{other}` (expected load, verify)")),
    }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    value_of(args, flag).ok_or_else(|| format!("missing {flag}"))
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn open_store(path: &PathBuf, args: &[String]) -> Result<Store, String> {
    let backend = match value_of(args, "--backend").as_deref() {
        None | Some("local") => BackendKind::Local,
        Some("object") => BackendKind::Object,
        Some("s3") => BackendKind::S3 {
            config: weftfs::s3::S3Config::from_env().map_err(|err| err.to_string())?,
        },
        Some(other) => return Err(format!("unknown --backend `{other}`")),
    };
    Store::open_with_options_and_backend(path, OpenMode::ReadOnly, Options::default(), backend)
        .map_err(|err| err.to_string())
}

fn load(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let checkpoint = required(args, "--checkpoint")?;
    let replica = value_of(args, "--replica").unwrap_or_else(|| "local".to_owned());
    let principal = value_of(args, "--principal").unwrap_or_else(|| "weftfs".to_owned());
    let out = PathBuf::from(required(args, "--out")?);
    let tensors = value_of(args, "--tensors").map(|text| {
        text.split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| part.trim().to_owned())
            .collect::<Vec<_>>()
    });
    let seed = match value_of(args, "--seed") {
        Some(path) => Some(read_seed(&PathBuf::from(path)).map_err(|err| err.to_string())?),
        None => None,
    };
    let snapshot = value_of(args, "--snapshot")
        .map(|text| Hash32::from_hex(&text).map_err(|err| err.to_string()))
        .transpose()?;
    let region = value_of(args, "--region-rows")
        .map(|text| {
            text.parse::<u64>()
                .map_err(|_| "--region-rows must be an integer".to_owned())
        })
        .transpose()?;

    let store = open_store(&store_path, args)?;
    let manifest = match snapshot {
        Some(snapshot) => weftfs::checkpoint::checkpoint_manifest_at(&store, snapshot, &checkpoint)
            .map_err(|err| err.to_string())?,
        None => store
            .checkpoint_manifest(&checkpoint)
            .map_err(|err| err.to_string())?,
    };
    // A row region needs each selected tensor's shape before the load starts.
    let region = match region {
        None => None,
        Some(rows) => {
            let target = manifest
                .tensors
                .iter()
                .find(|tensor| match &tensors {
                    None => true,
                    Some(list) => list.iter().any(|name| name == &tensor.name),
                })
                .ok_or_else(|| "no selected tensor for --region-rows".to_owned())?;
            if target.shape.len() != 2 {
                return Err(format!(
                    "a row region needs a rank-2 tensor; `{}` has rank {}",
                    target.name,
                    target.shape.len()
                ));
            }
            let end_row = rows.min(target.shape[0]);
            Some(Region::new(vec![0, 0], vec![end_row, target.shape[1]]))
        }
    };

    let options = AttestOptions {
        replica,
        principal,
        tensors,
        region,
        snapshot,
        issued_at_ns: None,
        seed,
    };
    let started = Instant::now();
    let attestation =
        attest_checkpoint_load(&store, &checkpoint, &options).map_err(|err| err.to_string())?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let bytes = attestation.encode().map_err(|err| err.to_string())?;
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    fs::write(&out, &bytes).map_err(|err| format!("{}: {err}", out.display()))?;

    let loaded: u64 = attestation
        .tensors
        .iter()
        .filter(|tensor| tensor.status == AttestStatus::Loaded)
        .map(|tensor| tensor.bytes_loaded)
        .sum();
    let groups: u64 = attestation
        .tensors
        .iter()
        .map(|tensor| tensor.groups.len() as u64)
        .sum();
    let ranges: u64 = attestation
        .tensors
        .iter()
        .map(|tensor| tensor.ranges.len() as u64)
        .sum();
    let unverified = attestation
        .tensors
        .iter()
        .filter(|tensor| tensor.status == AttestStatus::Unverified)
        .map(|tensor| {
            json!({
                "name": tensor.name,
                "reason": tensor.reason,
                "element_bytes": tensor.element_bytes,
            })
        })
        .collect::<Vec<Json>>();
    let summary = json!({
        "phase": "load",
        "status": "ok",
        "checkpoint": attestation.checkpoint,
        "checkpoint_root": attestation.checkpoint_root.to_hex(),
        "manifest_root": attestation.manifest_root.to_hex(),
        "snapshot": attestation.snapshot.map(|snapshot| snapshot.to_hex()),
        "replica": attestation.replica,
        "principal": attestation.verifier.principal,
        "public_key": attestation.verifier.public_key.to_hex(),
        "signed": attestation.signature.is_some(),
        "issued_at_ns": attestation.issued_at_ns,
        "load_ms": (load_ms * 1000.0).round() / 1000.0,
        "loaded_bytes": loaded,
        "loaded_mb_per_s": if load_ms > 0.0 { loaded as f64 / 1_000_000.0 / (load_ms / 1000.0) } else { 0.0 },
        "loaded_tensors": attestation.totals.loaded_tensors,
        "unverified_tensors": attestation.totals.unverified_tensors,
        "unverified": unverified,
        "ranges": ranges,
        "groups": groups,
        "units_verified": attestation.tensors.iter().map(|t| t.units_verified).sum::<u64>(),
        "receipt_bytes": bytes.len(),
        "receipt_root": attestation.root().map_err(|err| err.to_string())?.to_hex(),
        "peak_rss_kib": peak_rss_kib(),
    });
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn verify(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let receipt_path = PathBuf::from(required(args, "--receipt")?);
    let trust = value_of(args, "--trust")
        .map(|text| {
            text.split(',')
                .filter(|part| !part.trim().is_empty())
                .map(|part| {
                    Hash32::from_hex(part.trim())
                        .map(|hash| *hash.as_bytes())
                        .map_err(|err| err.to_string())
                })
                .collect::<Result<Vec<[u8; 32]>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    let bytes =
        fs::read(&receipt_path).map_err(|err| format!("{}: {err}", receipt_path.display()))?;
    let started = Instant::now();
    let attestation = LoadAttestation::decode(&bytes).map_err(|err| err.to_string())?;
    let store = open_store(&store_path, args)?;
    let options = VerifyOptions {
        trust,
        require_signature: flag(args, "--require-signature"),
    };
    let result = verify_against_store(&store, &attestation, &options)
        .map_err(|err| format!("attestation verification failed: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let loads = attestation
        .tensors
        .iter()
        .filter(|tensor| tensor.status == AttestStatus::Loaded)
        .collect::<Vec<_>>();
    let summary = json!({
        "phase": "verify",
        "status": "ok",
        "receipt_bytes": bytes.len(),
        "checkpoint": attestation.checkpoint,
        "manifest_root": attestation.manifest_root.to_hex(),
        "signature_verified": result.signature_verified,
        "verifier_public_key": attestation.verifier.public_key.to_hex(),
        "verified_tensors": result.verified_tensors,
        "unverified_tensors": result.unverified_tensors,
        "unverified": result.unverified.iter().map(|(name, reason)| json!({"name": name, "reason": reason})).collect::<Vec<Json>>(),
        "ranges_replayed": result.ranges_replayed,
        "replayed_bytes": result.replayed_bytes,
        "wall_ms": (wall_ms * 1000.0).round() / 1000.0,
        "us_per_mib": if result.replayed_bytes > 0 { wall_ms * 1000.0 / (result.replayed_bytes as f64 / (1024.0 * 1024.0)) } else { 0.0 },
        "us_per_range": if result.ranges_replayed > 0 { wall_ms * 1000.0 / result.ranges_replayed as f64 } else { 0.0 },
        "load_ranges": loads.iter().map(|tensor| tensor.ranges.len() as u64).sum::<u64>(),
        "peak_rss_kib": peak_rss_kib(),
    });
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn peak_rss_kib() -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kib) = rest.split_whitespace().next() {
                return kib.parse().unwrap_or(0);
            }
        }
    }
    0
}
