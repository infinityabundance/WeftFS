//! T052 exact-vs-approximate view court.
//!
//! Measures, on real public tensors, whether a q8_0-block view wins on any axis
//! (stored bytes, read time, or both) and records its measured error against the
//! declared per-element bound. The exact object is never modified: it is read
//! back and its logical root re-verified after the view is written, and the
//! exact resolution door is asked for the view's descriptor and must refuse.
//!
//! Usage inside a court:
//!   view_court --model FILE --tensors a,b --scratch DIR --out receipt.json

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::safetensors::SafetensorsFile;
use weftfs::state::State;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::TensorLayoutSpec;
use weftfs::view_identity::{ViewWriteOptions, Q8_0_BLOCK};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn run_zstd(zstd: &str, input: &Path, out: &Path) -> Result<u64, String> {
    let status = Command::new(zstd)
        .args(["-19", "-T1", "-q", "-f"])
        .arg(input)
        .arg("-o")
        .arg(out)
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd exited {status}"));
    }
    fs::metadata(out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat: {err}"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fail = |message: &str| -> ! {
        eprintln!("view_court: {message}");
        std::process::exit(2);
    };
    let model = PathBuf::from(arg(&args, "--model").unwrap_or_else(|| fail("--model")));
    let tensors: Vec<String> = arg(&args, "--tensors")
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let scratch = PathBuf::from(arg(&args, "--scratch").unwrap_or_else(|| fail("--scratch")));
    let out_path = arg(&args, "--out").unwrap_or_else(|| fail("--out"));
    let zstd = arg(&args, "--zstd").unwrap_or_else(|| "/usr/bin/zstd".to_owned());
    let blocks: Vec<u64> = arg(&args, "--blocks")
        .unwrap_or_else(|| "32,64,128".to_owned())
        .split(',')
        .filter_map(|text| text.parse().ok())
        .collect();
    fs::create_dir_all(&scratch).unwrap_or_else(|err| fail(&format!("scratch: {err}")));
    let store_dir = scratch.join("store");
    let _ = fs::remove_dir_all(&store_dir);
    let mut store =
        Store::init(&store_dir, opts()).unwrap_or_else(|err| fail(&format!("store: {err}")));
    let file = SafetensorsFile::parse(&model).unwrap_or_else(|err| fail(&format!("model: {err}")));

    let mut cells = Vec::new();
    for (index, name) in tensors.iter().enumerate() {
        let Some(tensor) = file.tensors.iter().find(|tensor| &tensor.name == name) else {
            fail(&format!("model has no tensor `{name}`"));
        };
        if tensor.dtype != "bf16" && tensor.dtype != "f32" && tensor.dtype != "f16" {
            fail(&format!(
                "tensor `{name}` has dtype {} which q8_0 cannot target",
                tensor.dtype
            ));
        }
        let bytes = file
            .read_tensor(tensor)
            .unwrap_or_else(|err| fail(&format!("read: {err}")));
        let before = store.retained_blob_bytes().unwrap();
        let object = store
            .write_tensor_file(
                State::ROOT_ID,
                format!("exact-{index}").as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                None,
            )
            .unwrap_or_else(|err| fail(&format!("exact write: {err}")));
        let exact_physical = store.retained_blob_bytes().unwrap() - before;
        let exact_content = store.object_meta(object).unwrap().content;
        let exact_logical = store.tensor_logical_root(object).unwrap();

        let started = Instant::now();
        let restored = store.read_tensor(object).unwrap();
        let exact_read_ms = started.elapsed().as_secs_f64() * 1000.0;
        if restored != bytes {
            fail(&format!("exact read of `{name}` is not byte-identical"));
        }

        let raw_path = scratch.join(format!("{index}.exact.raw"));
        fs::write(&raw_path, &bytes).unwrap();
        let exact_zstd = run_zstd(
            &zstd,
            &raw_path,
            &scratch.join(format!("{index}.exact.zst")),
        )
        .unwrap_or_else(|err| fail(&err));

        let mut block_cells = Vec::new();
        for block in &blocks {
            let label = format!("q8_{block}-{index}");
            let before = store.retained_blob_bytes().unwrap();
            let report = match store.write_quantized_view(
                &label,
                object,
                *block,
                &ViewWriteOptions::default(),
            ) {
                Ok(report) => report,
                Err(err) => {
                    block_cells.push(json!({
                        "block": block,
                        "refused": true,
                        "class": err.class().as_str(),
                        "message": err.message(),
                    }));
                    continue;
                }
            };
            let view_physical = store.retained_blob_bytes().unwrap() - before;
            let started = Instant::now();
            let resolved = store
                .resolve_view(&label)
                .unwrap_or_else(|err| fail(&format!("resolve: {err}")));
            let view_read_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let identity_only = store
                .resolve_view_bytes(&label)
                .unwrap_or_else(|err| fail(&format!("resolve bytes: {err}")));
            let view_read_identity_only_ms = started.elapsed().as_secs_f64() * 1000.0;
            if identity_only.bytes != resolved.bytes {
                fail("identity-only resolution disagrees with the strict door");
            }
            let descriptor = store.load_view(&label).unwrap();
            let verify = store
                .verify_view(&label)
                .unwrap_or_else(|err| fail(&format!("verify view: {err}")));

            // The exact door must refuse the descriptor object.
            let descriptor_object = store
                .resolve_path(&format!("/views/{label}/descriptor"))
                .unwrap();
            let refusal = store.resolve_exact_object(descriptor_object).err();
            let refusal_class = refusal.as_ref().map(|err| err.class().as_str().to_owned());
            let refusal_message = refusal.as_ref().map(|err| err.message().to_owned());

            // The exact object is unchanged and still verifies.
            let exact_after_content = store.object_meta(object).unwrap().content;
            let exact_after_logical = store.tensor_logical_root(object).unwrap();
            let exact_intact = exact_after_content == exact_content
                && exact_after_logical == exact_logical
                && store.read_tensor(object).unwrap() == bytes;

            let codes = store.read_tensor(descriptor.codes).unwrap();
            let scales = store.read_tensor(descriptor.scales).unwrap();
            let mut codes_raw = codes.clone();
            codes_raw.extend_from_slice(&scales);
            let view_raw = scratch.join(format!("{index}.{block}.view.raw"));
            fs::write(&view_raw, &codes_raw).unwrap();
            let view_zstd = run_zstd(
                &zstd,
                &view_raw,
                &scratch.join(format!("{index}.{block}.view.zst")),
            )
            .unwrap_or_else(|err| fail(&err));

            block_cells.push(json!({
                "block": block,
                "view_id": report.view_id.to_hex(),
                "descriptor_bytes": store.object_meta(store.resolve_path(&format!("/views/{label}/descriptor")).unwrap()).unwrap().size,
                "stored_physical_bytes": view_physical,
                "codes_bytes": report.codes_bytes,
                "scales_bytes": report.scales_bytes,
                "codes_and_scales_raw_bytes": codes_raw.len(),
                "codes_and_scales_zstd19_bytes": view_zstd,
                "physical_vs_exact": view_physical as f64 / exact_physical as f64,
                "physical_vs_exact_zstd19": view_physical as f64 / exact_zstd as f64,
                "view_read_ms": view_read_ms,
                "view_read_identity_only_ms": view_read_identity_only_ms,
                "read_ms_vs_exact": view_read_ms / exact_read_ms,
                "read_identity_only_ms_vs_exact": view_read_identity_only_ms / exact_read_ms,
                "max_abs_error": report.error.max_abs_error,
                "max_bound": report.error.max_bound,
                "clipped": report.error.clipped,
                "overflowed": report.error.overflowed,
                "error_holds": report.error.holds,
                "elements": report.error.elements,
                "approximate_root": report.approximate_root.to_hex(),
                "verify_ok": verify.exact_object_intact && verify.error.holds,
                "exact_object_intact": exact_intact,
                "exact_door_refused": refusal_class.as_deref() == Some("UnsupportedRepresentation"),
                "exact_door_class": refusal_class,
                "exact_door_message": refusal_message,
                "resolved_bytes_equal_exact": resolved.bytes == bytes,
            }));
        }

        cells.push(json!({
            "tensor": name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "elements": bytes.len() as u64 / if tensor.dtype == "f32" { 4 } else { 2 },
            "exact_bytes": bytes.len(),
            "exact_stored_physical_bytes": exact_physical,
            "exact_zstd19_bytes": exact_zstd,
            "exact_read_ms": exact_read_ms,
            "logical_root": exact_logical.to_hex(),
            "content": exact_content.to_hex(),
            "views": block_cells,
        }));
    }
    let verification = store
        .verify(true)
        .unwrap_or_else(|err| fail(&format!("verify: {err}")));

    let receipt = json!({
        "schema": "weft.view-court.v1",
        "generated_utc": chrono_like(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
            "single_host": true,
            "warm_cache": true,
            "note": "one host, warm page cache, CPU only; a single read timing per cell is a point measurement, not a percentile",
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "model": model.display().to_string(),
        "default_block": Q8_0_BLOCK,
        "tensors": cells,
        "verify": {
            "full": true,
            "checked_objects": verification.checked_objects,
            "checked_manifests": verification.checked_manifests,
            "checked_representations": verification.checked_representations,
            "problems": verification.problems.len(),
        },
    });
    fs::write(&out_path, serde_json::to_vec_pretty(&receipt).unwrap())
        .unwrap_or_else(|err| fail(&format!("write receipt: {err}")));
    println!("view_court: wrote {out_path}");
}

fn chrono_like() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, seconds) = (now / 86_400, now % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
