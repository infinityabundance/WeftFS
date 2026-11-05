//! Synthetic physical-representation benchmark (Rust store only).
//!
//! Two 8 MiB workloads:
//! * `text`    — highly compressible repeated text.
//! * `neardup` — deterministic pseudo-random 8 MiB with 1% of bytes changed.
//!
//! For each applicable representation we record the physical bytes of the
//! representation, the writer-call time (encode + store + commit/fsync), the
//! full-decode time, and a 4 KiB range-decode time:
//! * `raw`          — frozen v1 chunk manifest (the default path).
//! * `codec-zstd`   — `CodecAuto` choosing a ZSTD WFT1 frame.
//! * `codec-delta`  — `CodecAuto` choosing a DELTA frame against a causal base.
//!
//! This is a synthetic fixture measured on the host page cache inside the
//! Docker court. It is **not** a device-level, fleet, Go, or production claim.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};

use weftfs::format::RepresentationKind;
use weftfs::recovery::read_manifest;
use weftfs::store::{OpenMode, Options, RepresentationMode, Store};

const FILE_SIZE: usize = 8 * 1024 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024;
const RANGE_BYTES: u64 = 4096;
const REPS: usize = 5;

fn raw_options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn codec_options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        representation: RepresentationMode::CodecAuto,
        codec_min_bytes: 4096,
        codec_delta_margin_percent: 10,
        quotas: Vec::new(),
    }
}

fn compressible_text(len: usize) -> Vec<u8> {
    let line = b"weftfs representation benchmark: repeated sentence for zstd.\n";
    let mut out = Vec::with_capacity(len);
    while out.len() + line.len() <= len {
        out.extend_from_slice(line);
    }
    out.resize(len, b' ');
    out
}

fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state = 0x0bad_f00d_dead_beefu64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn near_duplicate(base: &[u8], stride: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut index = 0;
    while index < out.len() {
        out[index] ^= 0x5a;
        index += stride;
    }
    out
}

fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    total += directory_bytes(&entry.path());
                } else if let Ok(metadata) = entry.metadata() {
                    total += metadata.len();
                }
            }
        }
    }
    total
}

fn store_physical_bytes(store: &Store) -> u64 {
    directory_bytes(&store.root().join("objects"))
        + directory_bytes(&store.root().join("manifests"))
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn measure(store: &Store, id: weftfs::Hash32) -> (f64, f64) {
    let mut full = Vec::with_capacity(REPS);
    let mut range = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let started = Instant::now();
        let bytes = store.read_object(id).expect("full decode");
        full.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(bytes.len(), FILE_SIZE);
        let started = Instant::now();
        let slice = store
            .read_range(id, (FILE_SIZE as u64) / 2, RANGE_BYTES)
            .expect("range decode");
        range.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(slice.len() as u64, RANGE_BYTES);
    }
    (median(full), median(range))
}

fn describe_representation(store: &Store, id: weftfs::Hash32) -> Value {
    let record = store.object_meta(id).expect("metadata");
    let manifest = read_manifest(store.root(), record.content).expect("manifest");
    let manifest_bytes = fs::metadata(weftfs::recovery::manifest_path(
        store.root(),
        record.content,
    ))
    .map(|metadata| metadata.len())
    .unwrap_or(0);
    match &manifest.representation {
        Some(rep) if rep.kind == RepresentationKind::Codec => {
            let frame_bytes = rep
                .frame
                .and_then(|frame| {
                    fs::metadata(weftfs::recovery::manifest_path(store.root(), frame)).ok()
                })
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            json!({
                "kind": "codec",
                "strategy": rep.strategy,
                "physical_bytes": frame_bytes + manifest_bytes,
                "frame_bytes": frame_bytes,
                "manifest_bytes": manifest_bytes,
                "dependencies": rep.dependencies.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                "schema": "weft.manifest.v2",
            })
        }
        Some(_) => json!({
            "kind": "raw",
            "strategy": "raw",
            "physical_bytes": manifest.chunks.iter().map(|chunk| chunk.len).sum::<u64>() + manifest_bytes,
            "frame_bytes": 0,
            "manifest_bytes": manifest_bytes,
            "dependencies": Vec::<String>::new(),
            "schema": "weft.manifest.v2",
        }),
        None => json!({
            "kind": "raw",
            "strategy": "raw",
            "physical_bytes": manifest.chunks.iter().map(|chunk| chunk.len).sum::<u64>() + manifest_bytes,
            "frame_bytes": 0,
            "manifest_bytes": manifest_bytes,
            "dependencies": Vec::<String>::new(),
            "schema": "weft.manifest.v1",
        }),
    }
}

fn run_case(
    tag: &str,
    label: &str,
    options: Options,
    v1: &[u8],
    v2: Option<&[u8]>,
    use_causal: bool,
) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .join(".docker-cache")
        .join("representation-bench")
        .join(tag);
    if path.exists() {
        fs::remove_dir_all(&path).expect("clear bench store");
    }
    fs::create_dir_all(&path).expect("create bench dir");
    let mut store = Store::init(&path, options).expect("init bench store");
    let root_id = store.resolve_path("/").expect("root");

    let started = Instant::now();
    let id = store
        .create_file(root_id, b"f", v1, 0o644, 0, 0, Default::default())
        .expect("create v1");
    let create_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut update_ms = Value::Null;
    let mut base_dependency = Value::Null;
    if let Some(v2) = v2 {
        let v1_version = store.object_meta(id).expect("v1 meta").version;
        let started = Instant::now();
        if use_causal {
            store
                .write_file_causal(id, 0, v2, &[v1_version])
                .expect("write v2 delta");
        } else {
            store.write_file_causal(id, 0, v2, &[]).expect("write v2");
        }
        update_ms = json!(started.elapsed().as_secs_f64() * 1000.0);
        let record = store.object_meta(id).expect("v2 meta");
        let manifest = read_manifest(store.root(), record.content).expect("v2 manifest");
        base_dependency = json!(manifest
            .representation
            .as_ref()
            .map(|rep| rep
                .dependencies
                .iter()
                .map(|hash| hash.to_hex())
                .collect::<Vec<_>>())
            .unwrap_or_default());
    }

    let expected = v2.unwrap_or(v1);
    let (decode_ms, range_ms) = measure(&store, id);
    let representation = describe_representation(&store, id);
    let total_bytes = store_physical_bytes(&store);
    let verified = store.verify(true).map(|report| report.ok).unwrap_or(false);
    let actual = store.read_object(id).expect("decode after measurement");
    assert_eq!(actual, expected, "round trip mismatch for {tag}");

    json!({
        "case": tag,
        "label": label,
        "encode_ms": if v2.is_some() { update_ms } else { json!(create_ms) },
        "create_v1_ms": create_ms,
        "full_decode_ms_median": decode_ms,
        "range_4k_decode_ms_median": range_ms,
        "store_total_physical_bytes": total_bytes,
        "representation": representation,
        "delta_dependency": base_dependency,
        "store_verified_after_run": verified,
    })
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn main() {
    let text = compressible_text(FILE_SIZE);
    let near_base = pseudo_random(FILE_SIZE);
    let near_v2 = near_duplicate(&near_base, 100);

    let mut cases = Vec::new();

    cases.push(run_case(
        "text-raw",
        "raw v1 chunks",
        raw_options(),
        &text,
        None,
        false,
    ));
    cases.push(run_case(
        "text-codec-zstd",
        "CodecAuto ZSTD frame",
        codec_options(),
        &text,
        None,
        false,
    ));
    cases.push(run_case(
        "neardup-raw",
        "raw v1 chunks (v2 write)",
        raw_options(),
        &near_base,
        Some(&near_v2),
        false,
    ));
    cases.push(run_case(
        "neardup-codec-zstd",
        "CodecAuto without a causal base (ZSTD attempted; falls back to v2 raw when not smaller)",
        codec_options(),
        &near_base,
        Some(&near_v2),
        false,
    ));
    cases.push(run_case(
        "neardup-codec-delta",
        "CodecAuto DELTA frame (v2 write against causal v1)",
        codec_options(),
        &near_base,
        Some(&near_v2),
        true,
    ));

    let revision = env_or(
        "WEFT_REPO_WEFTFS",
        &command_stdout("git", &["rev-parse", "HEAD"]),
    );
    let dirty = !command_stdout("git", &["--no-optional-locks", "status", "--porcelain"])
        .trim()
        .is_empty();
    let timestamp = command_stdout("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]);
    let receipt = json!({
        "label": "synthetic Rust-store representation benchmark; host page cache inside a Docker court; Rust only; not a device, fleet, Go-parity, or production claim",
        "revision": revision,
        "working_tree_dirty": dirty,
        "revision_note": "revision is the git HEAD of the tree used for the run; working_tree_dirty records whether the representation change itself was uncommitted",
        "court": {
            "image": env_or("WEFT_COURT_IMAGE", "unknown"),
            "image_id": env_or("WEFT_COURT_IMAGE_ID", "unknown"),
            "kernel": env_or("WEFT_COURT_KERNEL", &command_stdout("uname", &["-r"])),
            "memory_limit": env_or("WEFT_COURT_MEMORY", "unknown"),
            "cpus": env_or("WEFT_COURT_CPUS", "unknown"),
            "pids_limit": env_or("WEFT_COURT_PIDS", "unknown"),
        },
        "workload": {
            "file_size_bytes": FILE_SIZE,
            "chunk_size_bytes": CHUNK_SIZE,
            "range_bytes": RANGE_BYTES,
            "repetitions": REPS,
            "text": "deterministic repeated sentence",
            "neardup": "deterministic pseudo-random bytes with 1% changed at stride 100",
            "physical_bytes_note": "representation.physical_bytes counts the manifest plus chunk lengths (or frame bytes); store_total_physical_bytes counts unique blobs on disk after deduplication",
        },
        "cases": cases,
        "receipt_timestamp_utc": timestamp,
    });

    let receipts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/receipts");
    fs::create_dir_all(&receipts).expect("create receipts dir");
    let path = receipts.join(format!("representation-{timestamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!("{}", path.display());
    println!("{}", serde_json::to_string_pretty(&receipt).unwrap());

    // A cheap post-run reopen check: a default (Raw) reader must open these
    // stores and decode the codec-backed content.
    for case in ["text-codec-zstd", "neardup-codec-delta"] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".docker-cache")
            .join("representation-bench")
            .join(case);
        let store = Store::open(&path, OpenMode::ReadOnly).expect("reopen");
        assert!(store.verify(true).map(|report| report.ok).unwrap_or(false));
    }
}
