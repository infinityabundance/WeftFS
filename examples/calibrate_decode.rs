//! Decode-cost calibration for the tensor planner (R1, R12).
//!
//! Thin wrapper over [`weftfs::calibration`]: the measurement, affine fit, and
//! strict calibration record live in the library, so `weftfs calibrate` and
//! this example cannot drift. This example keeps the historical bench-receipt
//! JSON shape (`small`/`large`/`fit` per row) that
//! `bench/scripts/assemble_planner_float.py` reads, and embeds the canonical
//! `weft.calibration.v1` document under `"canonical"`.
//!
//! Usage (inside the bench court):
//!   cargo run --release --example calibrate_decode -- \
//!       --out /work/.docker-cache/calibration/decode.json
//!
//! Synthetic payloads: BF16 values drawn from a bounded exponent/mantissa
//! distribution (not uniform random bytes), so zstd and the mode-9 plane coder
//! see realistic weight-like data. Every measurement is of the shipped store
//! read path, including frame/chunk verification.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};
use weftfs::calibration::{
    measure_decode_costs_scratch, record_from_measurement, HostIdentity, MeasurementOptions,
};
use weftfs::cost_model::{CostClass, MEASURED_CALIBRATION};

const MIB: u64 = 1_048_576;

fn main() {
    if let Err(err) = run() {
        eprintln!("calibrate_decode: {err}");
        std::process::exit(1);
    }
}

fn arg(args: &[String], flag: &str, default: &str) -> String {
    args.iter()
        .position(|value| value == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = arg(&args, "--out", "decode-calibration.json");
    let reps: usize = arg(&args, "--reps", "7")
        .parse()
        .map_err(|_| "--reps must be an integer".to_owned())?;
    let large_mib: u64 = arg(&args, "--large-mib", "16")
        .parse()
        .map_err(|_| "--large-mib must be an integer".to_owned())?;
    let small_kib: u64 = arg(&args, "--small-kib", "128")
        .parse()
        .map_err(|_| "--small-kib must be an integer".to_owned())?;
    let options = MeasurementOptions {
        reps,
        large_mib,
        small_kib,
    };
    options.validate().map_err(|err| err.to_string())?;

    let outcome = measure_decode_costs_scratch(&options).map_err(|err| err.to_string())?;
    let created_unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    // A complete, installable record is only produced when every class
    // measured successfully; the historical receipt still records errors.
    let canonical = if outcome.errors.is_empty() {
        Some(
            record_from_measurement(&outcome, &options, HostIdentity::current(), created_unix_ns)
                .map_err(|err| err.to_string())?,
        )
    } else {
        None
    };

    let rows: Vec<Json> = outcome
        .details
        .iter()
        .map(|detail| {
            let fit = outcome
                .rows
                .iter()
                .find(|row| row.class == detail.class)
                .copied();
            json!({
                "class": detail.class.as_str(),
                "strategy": strategy_name(detail.class),
                "small": {
                    "logical_bytes": detail.small_bytes,
                    "median_ns": detail.small_median_ns,
                    "median_ns_per_mib": if detail.small_bytes == 0 {
                        0.0
                    } else {
                        detail.small_median_ns as f64 / (detail.small_bytes as f64 / MIB as f64)
                    },
                    "dependency_median_ns": detail.small_dependency_median_ns,
                },
                "large": {
                    "logical_bytes": detail.large_bytes,
                    "median_ns": detail.large_median_ns,
                    "median_ns_per_mib": if detail.large_bytes == 0 {
                        0.0
                    } else {
                        detail.large_median_ns as f64 / (detail.large_bytes as f64 / MIB as f64)
                    },
                    "dependency_median_ns": detail.large_dependency_median_ns,
                },
                "fit": {
                    "fixed_ns": fit.map(|row| row.fixed_ns).unwrap_or(0),
                    "ns_per_mib": fit.map(|row| row.ns_per_mib).unwrap_or(0),
                },
            })
        })
        .collect();

    let receipt = json!({
        "schema": "weft.decode-calibration.v1",
        "label": "host-calibrated decode table; single-host court, synthetic weight-like BF16, warm cache",
        "calibration_id": MEASURED_CALIBRATION,
        "canonical_schema": weftfs::calibration::CALIBRATION_SCHEMA,
        "canonical": canonical.as_ref().map(|record| record.to_json()),
        "environment": {
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftfs_revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftcodec_revision": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "method": {
            "reps": reps,
            "warmup_reads": 1,
            "small_bytes": small_kib * 1024,
            "large_bytes": large_mib * MIB,
            "fit": "fixed_ns + ns_per_mib * decoded_bytes / 1048576; fit from the small and large medians; delta rows subtract the measured base read",
            "includes": "manifest read, frame/chunk verification, full logical decode",
        },
        "divergence_vs_baked": canonical.as_ref().map(|record| {
            record
                .divergence(weftfs::CostModelKind::Measured)
                .iter()
                .map(|row| {
                    json!({
                        "class": row.class.as_str(),
                        "host_ns_per_mib": row.host.ns_per_mib,
                        "baked_ns_per_mib": row.baked.ns_per_mib,
                        "host_1mib_ns": row.host_1mib_ns,
                        "baked_1mib_ns": row.baked_1mib_ns,
                        "estimate_1mib_delta_ppm": row.estimate_1mib_delta_ppm(),
                    })
                })
                .collect::<Vec<_>>()
        }),
        "rows": rows,
        "errors": outcome.errors,
    });
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{out}: {err}"))?;
    println!("{}", out);
    // Also print a copy-paste table for the two cost_model modules.
    println!("// measured table (fixed_ns, ns_per_mib)");
    for row in outcome.rows.iter() {
        println!(
            "({}, {}, {}),",
            row.class.as_str(),
            row.fixed_ns,
            row.ns_per_mib
        );
    }
    Ok(())
}

fn strategy_name(class: CostClass) -> &'static str {
    class.as_str()
}
