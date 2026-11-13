//! N04 adverse court: the object-backend pipeline under provider faults.
//!
//! One scenario per invocation, driven by `scripts/object_pipeline_court.sh`,
//! which starts the byte-level fault proxy (`bench/scripts/s3_fault_proxy.py`)
//! in front of a real MinIO endpoint:
//!
//! * `abort` — the provider drops the connection at a chosen `PUT` before
//!   applying it. The write must fail closed; the reopened store must be at the
//!   previous head with `verify --full` clean; the identical retry through the
//!   now-healthy proxy must reach the same head as an unfaulted reference;
//! * `lost-ack` — the provider applies the `PUT` but the client never receives
//!   the acknowledgement. Same obligations, and the retry must *not* rewrite the
//!   blobs the first attempt already published (the create-once duplicate path
//!   accepts them), so the final tree equals the reference byte for byte;
//! * `stale-reader` — a read-only handle opened before the commits must never
//!   observe the new state, while a handle opened afterwards must.
//!
//! Every scenario writes one JSON observation record. Nothing here is a
//! product component; the fixture is synthetic and in memory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use weftfs::backend::BackendKind;
use weftfs::s3::S3Config;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_pipeline::{TensorPipelineOptions, TensorPipelineTensor};
use weftfs::tensor_store::{TensorLayoutSpec, TensorWriteSpec};

const FIXED_TS: i64 = 1_700_000_000_000_000_000;
const TENSORS: usize = 8;
const TENSOR_BYTES: usize = 256 * 1024;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 1024,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TS),
        ..Options::default()
    }
}

/// Deterministic bf16 tensor bytes: skewed enough that the planner has a real
/// choice, small enough that the whole fixture is 2 MiB.
fn payload(seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(TENSOR_BYTES);
    while out.len() < TENSOR_BYTES {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(TENSOR_BYTES);
    for (index, byte) in out.iter_mut().enumerate() {
        if index % 7 != 0 {
            *byte = (*byte & 0x0f) | 0x30;
        }
    }
    out
}

fn names() -> Vec<String> {
    (0..TENSORS)
        .map(|index| format!("layer{index}.weight"))
        .collect()
}

fn seed_for(name: &str) -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in name.as_bytes().iter().enumerate() {
        seed[index % 32] = seed[index % 32].wrapping_add(*byte).wrapping_add(1);
    }
    seed[31] = (name.len() % 251) as u8;
    seed
}

fn spec() -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan: weftfs::tensor_store::TensorPlanOptions::default(),
        tile_shape: None,
    }
}

fn bytes_for(name: &str) -> Vec<u8> {
    let mut seed = 0u64;
    for byte in name.as_bytes() {
        seed = seed.wrapping_mul(131).wrapping_add(u64::from(*byte));
    }
    payload(seed ^ 0x5ee)
}

struct Outcome {
    head: String,
    generation: u64,
    verify_ok: bool,
    manifest_ids: BTreeMap<String, String>,
}

fn write_group(store: &mut Store, root_label: &str) -> Result<Outcome, String> {
    let dir = match store.resolve_path("/tensors") {
        Ok(dir) => dir,
        Err(_) => {
            let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
            store
                .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
                .map_err(|err| format!("mkdir: {err}"))?
        }
    };
    let names = names();
    let payloads: Vec<Vec<u8>> = names.iter().map(|name| bytes_for(name)).collect();
    let shapes: Vec<[u64; 2]> = payloads.iter().map(|_| [128, 1024]).collect();
    let tensors: Vec<TensorPipelineTensor<'_>> = names
        .iter()
        .enumerate()
        .map(|(index, name)| TensorPipelineTensor {
            name,
            dtype: "bf16",
            shape: &shapes[index],
            seed: seed_for(name),
            byte_len: payloads[index].len() as u64,
        })
        .collect();
    store.begin_group().map_err(|err| err.to_string())?;
    let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
        buf.copy_from_slice(&payloads[index]);
        Ok(())
    };
    let mut options = TensorPipelineOptions::default();
    options.encode_threads = 4;
    options.cpu_tokens = 3;
    let outcome = store
        .prepare_tensor_group_pipelined(
            &tensors,
            &read,
            &spec(),
            &TensorLayoutSpec::row_major(),
            &options,
        )
        .map_err(|err| format!("{root_label}: pipeline: {err}"))?;
    let mut ids = Vec::new();
    for (index, result) in outcome.results.iter().enumerate() {
        let id = store
            .commit_prepared_tensor_object(dir, names[index].as_bytes(), &result.prepared, 0o644)
            .map_err(|err| format!("{root_label}: commit: {err}"))?;
        ids.push(id);
    }
    let head = store.commit_group().map_err(|err| err.to_string())?;
    let mut manifest_ids = BTreeMap::new();
    for (index, id) in ids.iter().enumerate() {
        let record = store.object_meta(*id).map_err(|err| err.to_string())?;
        manifest_ids.insert(names[index].clone(), record.content.to_hex());
    }
    let verify = store.verify(true).map_err(|err| err.to_string())?;
    Ok(Outcome {
        head: head.to_hex(),
        generation: store.generation(),
        verify_ok: verify.ok,
        manifest_ids,
    })
}

fn config(endpoint: &str, bucket: &str) -> S3Config {
    S3Config {
        endpoint: endpoint.to_owned(),
        bucket: bucket.to_owned(),
        region: std::env::var("WEFT_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
        access_key: std::env::var("WEFT_S3_ACCESS_KEY")
            .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
            .expect("S3 access key"),
        secret_key: std::env::var("WEFT_S3_SECRET_KEY")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .expect("S3 secret key"),
        session_token: None,
        path_style: true,
        lock_lease: Duration::from_secs(60),
        request_timeout: Duration::from_secs(60),
    }
}

fn main() {
    let mut scenario = String::new();
    let mut endpoint = String::new();
    let mut reference_endpoint = String::new();
    let mut bucket = String::new();
    let mut out = PathBuf::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        let take = |index: usize| -> String {
            args.get(index + 1)
                .cloned()
                .unwrap_or_else(|| panic!("{} needs a value", args[index]))
        };
        match args[index].as_str() {
            "--scenario" => {
                scenario = take(index);
                index += 2;
            }
            "--endpoint" => {
                endpoint = take(index);
                index += 2;
            }
            "--reference-endpoint" => {
                reference_endpoint = take(index);
                index += 2;
            }
            "--bucket" => {
                bucket = take(index);
                index += 2;
            }
            "--out" => {
                out = PathBuf::from(take(index));
                index += 2;
            }
            other => panic!("unknown argument `{other}`"),
        }
    }
    assert!(!scenario.is_empty() && !endpoint.is_empty() && !bucket.is_empty());
    if reference_endpoint.is_empty() {
        reference_endpoint = endpoint.clone();
    }
    let result = match scenario.as_str() {
        "abort" | "lost-ack" => fault_scenario(&scenario, &endpoint, &reference_endpoint, &bucket),
        "stale-reader" => stale_reader(&endpoint, &bucket),
        "baseline" => baseline(&endpoint, &bucket),
        other => panic!("unknown scenario `{other}`"),
    };
    let record = match result {
        Ok(record) => record,
        Err(err) => {
            eprintln!("object_pipeline_adverse: {scenario} failed: {err}");
            std::process::exit(1);
        }
    };
    let text = serde_json::to_string_pretty(&record).expect("serialize record");
    std::fs::write(&out, format!("{text}\n")).expect("write record");
    println!("{text}");
}

fn fault_scenario(
    scenario: &str,
    endpoint: &str,
    reference_endpoint: &str,
    bucket: &str,
) -> Result<serde_json::Value, String> {
    // 1. The store exists and has one committed directory before the fault, so
    //    "previous head" is a real generation rather than "no store at all".
    let reference_bucket = format!("{bucket}-reference");
    let faulted_bucket = format!("{bucket}-faulted");
    let dir = std::env::temp_dir().join(format!("weft-n04-{}-{scenario}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;

    let reference = {
        let mut store = Store::init_with_backend(
            dir.join("reference"),
            opts(),
            BackendKind::S3 {
                config: config(reference_endpoint, &reference_bucket),
            },
        )
        .map_err(|err| format!("reference init: {err}"))?;
        write_group(&mut store, "reference")?
    };

    // 2. Faulted attempt through the proxy.
    let mut store = Store::init_with_backend(
        dir.join("faulted"),
        opts(),
        BackendKind::S3 {
            config: config(endpoint, &faulted_bucket),
        },
    )
    .map_err(|err| format!("faulted init: {err}"))?;
    // The directory commit is what the fault must leave intact.
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
    store
        .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
        .map_err(|err| format!("mkdir: {err}"))?;
    let pre_fault_head = store.head().to_hex();
    let pre_fault_generation = store.generation();
    let failed = {
        let names = names();
        let payloads: Vec<Vec<u8>> = names.iter().map(|name| bytes_for(name)).collect();
        let shapes: Vec<[u64; 2]> = payloads.iter().map(|_| [128, 1024]).collect();
        let tensors: Vec<TensorPipelineTensor<'_>> = names
            .iter()
            .enumerate()
            .map(|(index, name)| TensorPipelineTensor {
                name,
                dtype: "bf16",
                shape: &shapes[index],
                seed: seed_for(name),
                byte_len: payloads[index].len() as u64,
            })
            .collect();
        let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
            buf.copy_from_slice(&payloads[index]);
            Ok(())
        };
        let mut options = TensorPipelineOptions::default();
        options.encode_threads = 4;
        options.cpu_tokens = 3;
        store.prepare_tensor_group_pipelined(
            &tensors,
            &read,
            &spec(),
            &TensorLayoutSpec::row_major(),
            &options,
        )
    };
    let failure = match failed {
        Ok(_) => {
            return Err(format!(
                "{scenario}: the faulted write unexpectedly succeeded; the proxy did not fire"
            ))
        }
        Err(err) => format!("{err}"),
    };
    drop(store);

    // 3. Reopen: previous head, clean verify, nothing partial.
    let reopened = Store::open_with_options_and_backend(
        dir.join("faulted"),
        OpenMode::ReadOnly,
        opts(),
        BackendKind::S3 {
            config: config(endpoint, &faulted_bucket),
        },
    )
    .map_err(|err| format!("reopen after fault: {err}"))?;
    let after_head = reopened.head().to_hex();
    let after_generation = reopened.generation();
    let after_verify = reopened
        .verify(true)
        .map_err(|err| format!("verify after fault: {err}"))?;
    let partial_visible = after_head != pre_fault_head || after_generation != pre_fault_generation;
    {
        let names = names();
        let leaked: Vec<String> = names
            .iter()
            .filter(|name| reopened.resolve_path(&format!("/tensors/{name}")).is_ok())
            .cloned()
            .collect();
        if !leaked.is_empty() {
            return Err(format!(
                "{scenario}: the failed write left visible entries: {leaked:?}"
            ));
        }
    }
    if partial_visible || !after_verify.ok {
        return Err(format!(
            "{scenario}: reopened store is not the previous head (head {after_head}, expected \
             {pre_fault_head}; verify_ok {})",
            after_verify.ok
        ));
    }
    drop(reopened);

    // 4. Retry the identical write through the now-healthy proxy.
    let retried = {
        let mut store = Store::open_with_options_and_backend(
            dir.join("faulted"),
            OpenMode::ReadWrite,
            opts(),
            BackendKind::S3 {
                config: config(endpoint, &faulted_bucket),
            },
        )
        .map_err(|err| format!("reopen for retry: {err}"))?;
        write_group(&mut store, "retry")?
    };
    if retried.head != reference.head {
        return Err(format!(
            "{scenario}: retry head {} != reference head {}",
            retried.head, reference.head
        ));
    }
    if retried.manifest_ids != reference.manifest_ids {
        return Err(format!(
            "{scenario}: retry manifest ids differ from the reference"
        ));
    }
    if !retried.verify_ok {
        return Err(format!("{scenario}: retry verify failed"));
    }

    Ok(serde_json::json!({
        "scenario": scenario,
        "fault": "the provider aborted the write" ,
        "failure_message": failure,
        "pre_fault_head": pre_fault_head,
        "pre_fault_generation": pre_fault_generation,
        "head_after_fault": after_head,
        "generation_after_fault": after_generation,
        "verify_after_fault_ok": after_verify.ok,
        "no_partial_entry_visible": true,
        "retry_head": retried.head,
        "retry_generation": retried.generation,
        "reference_head": reference.head,
        "reference_generation": reference.generation,
        "retry_matches_reference": retried.head == reference.head
            && retried.manifest_ids == reference.manifest_ids,
        "retry_verify_ok": retried.verify_ok,
        "tensors": TENSORS,
        "logical_bytes": (TENSORS * TENSOR_BYTES) as u64,
    }))
}

/// Plain successful pipeline write; used with the proxy in `count` mode so the
/// court learns how many `PUT`s the store's initialisation plus one write
/// performs before it chooses a fault target.
fn baseline(endpoint: &str, bucket: &str) -> Result<serde_json::Value, String> {
    let dir = std::env::temp_dir().join(format!("weft-n04-{}-baseline", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let mut store = Store::init_with_backend(
        dir.join("store"),
        opts(),
        BackendKind::S3 {
            config: config(endpoint, bucket),
        },
    )
    .map_err(|err| format!("init: {err}"))?;
    let written = write_group(&mut store, "baseline")?;
    Ok(serde_json::json!({
        "scenario": "baseline",
        "head": written.head,
        "generation": written.generation,
        "tensors": written.manifest_ids.len(),
        "verify_ok": written.verify_ok,
    }))
}

fn stale_reader(endpoint: &str, bucket: &str) -> Result<serde_json::Value, String> {
    let dir = std::env::temp_dir().join(format!("weft-n04-{}-reader", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let bucket = format!("{bucket}-reader");
    let store_path = dir.join("store");
    {
        let mut store = Store::init_with_backend(
            &store_path,
            opts(),
            BackendKind::S3 {
                config: config(endpoint, &bucket),
            },
        )
        .map_err(|err| format!("init: {err}"))?;
        let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
        store
            .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
            .map_err(|err| format!("mkdir: {err}"))?;
    }
    let reader = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        opts(),
        BackendKind::S3 {
            config: config(endpoint, &bucket),
        },
    )
    .map_err(|err| format!("reader open: {err}"))?;
    let head_before = reader.head().to_hex();
    let generation_before = reader.generation();

    let written = {
        let mut store = Store::open_with_options_and_backend(
            &store_path,
            OpenMode::ReadWrite,
            opts(),
            BackendKind::S3 {
                config: config(endpoint, &bucket),
            },
        )
        .map_err(|err| format!("writer open: {err}"))?;
        write_group(&mut store, "writer")?
    };

    let mut leaked = Vec::new();
    for name in names() {
        if reader.resolve_path(&format!("/tensors/{name}")).is_ok() {
            leaked.push(name);
        }
    }
    let pinned = reader.head().to_hex() == head_before
        && reader.generation() == generation_before
        && leaked.is_empty();
    drop(reader);
    let fresh = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        opts(),
        BackendKind::S3 {
            config: config(endpoint, &bucket),
        },
    )
    .map_err(|err| format!("fresh open: {err}"))?;
    let fresh_sees = fresh
        .resolve_path(&format!("/tensors/{}", names()[0]))
        .is_ok();
    if !pinned || !fresh_sees || fresh.head().to_hex() != written.head {
        return Err(format!(
            "stale-reader: pinned={pinned} fresh_sees={fresh_sees} fresh_head={} written_head={}",
            fresh.head().to_hex(),
            written.head
        ));
    }
    Ok(serde_json::json!({
        "scenario": "stale-reader",
        "head_before": head_before,
        "generation_before": generation_before,
        "written_head": written.head,
        "written_generation": written.generation,
        "reader_stayed_pinned": pinned,
        "reader_saw_no_new_entry": leaked.is_empty(),
        "fresh_reader_sees_new_entry": fresh_sees,
        "fresh_reader_head_matches": fresh.head().to_hex() == written.head,
    }))
}
