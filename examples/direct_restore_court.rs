//! X18 court harness — direct-to-destination verified reconstruction.
//!
//! One tiled checkpoint, five arms on the same bytes:
//!
//! | arm | path | SHA-256 |
//! |---|---|---|
//! | `container_then_copy` | `restore_checkpoint_pipelined`, then copy into per-tensor buffers | per-tensor + container |
//! | `container_unhashed` | `restore_checkpoint_pipelined_unhashed` into a container buffer | none |
//! | `direct_into_destinations` | [`Store::restore_checkpoint_direct_into`] | none |
//! | `rank_ordinary` | [`Store::restore_checkpoint_rank`] (one quarter of the rows) | per-tensor |
//! | `rank_direct_slab` | [`Store::restore_row_slab_direct_at`] (the same rows) | none |
//!
//! Every arm is checked byte-for-byte against the source tensors. The receipt
//! records wall, CPU and the process RSS delta so the cost of the container
//! buffer and of the SHA-256 pass are both visible.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use weftfs::checkpoint::{
    CheckpointRestoreOptions, CheckpointWriteOptions, DirectDestinations, RestoreTopology,
    ShardStrategy,
};
use weftfs::restore_pipeline::RestorePipelineOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const TENSORS: usize = 8;
const ROWS: u64 = 2048;
const COLS: u64 = 2048;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 64 * 1024,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn tensor_bytes(index: u16) -> Vec<u8> {
    let mut data = Vec::with_capacity((ROWS * COLS * 2) as usize);
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(index);
    for _ in 0..(ROWS * COLS) / 4 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        data.extend_from_slice(&value.to_le_bytes());
    }
    data
}

struct Fixture {
    dir: tempfile::TempDir,
    store_dir: PathBuf,
    tensors: Vec<(String, Vec<u8>)>,
}

impl Fixture {
    fn build() -> Fixture {
        let dir = tempfile::TempDir::new().unwrap();
        let mut tensors = Vec::new();
        for index in 0..TENSORS {
            tensors.push((format!("layer{index}.weight"), tensor_bytes(index as u16)));
        }
        let shape = [ROWS, COLS];
        let entries: Vec<SafetensorsEntry<'_>> = tensors
            .iter()
            .map(|(name, data)| SafetensorsEntry {
                name,
                dtype: "bf16",
                shape: &shape,
                data,
            })
            .collect();
        let file = safetensors::encode_file(&entries, None).unwrap();
        let source = dir.path().join("model.safetensors");
        fs::write(&source, &file).unwrap();
        let store_dir = dir.path().join("store");
        {
            let mut store = Store::init(&store_dir, options()).unwrap();
            let write = CheckpointWriteOptions {
                shards: 4,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorTiled,
                tile_shape: Some(vec![128, 512]),
                write_policy: weftfs::tensor_store::WritePolicy::Planned,
                ..CheckpointWriteOptions::default()
            };
            store.write_checkpoint("cp", &source, &write).unwrap();
        }
        Fixture {
            dir,
            store_dir,
            tensors,
        }
    }

    fn open(&self) -> Store {
        Store::open_with_options(&self.store_dir, OpenMode::ReadOnly, options()).unwrap()
    }
}

#[derive(Clone, Copy)]
struct Rusage {
    user_us: f64,
    sys_us: f64,
}

fn rusage() -> Rusage {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        Rusage {
            user_us: usage.ru_utime.tv_sec as f64 * 1e6 + usage.ru_utime.tv_usec as f64,
            sys_us: usage.ru_stime.tv_sec as f64 * 1e6 + usage.ru_stime.tv_usec as f64,
        }
    }
}

fn rss_kib() -> u64 {
    let text = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let value = rest.trim().split_whitespace().next().unwrap_or("0");
            return value.parse().unwrap_or(0);
        }
    }
    0
}

fn destinations(tensors: &[(String, Vec<u8>)]) -> DirectDestinations {
    tensors
        .iter()
        .map(|(name, data)| (name.clone(), vec![0u8; data.len()]))
        .collect()
}

struct Arm {
    label: &'static str,
    runs: Vec<f64>,
    cpu: Vec<f64>,
    rss_start: u64,
    rss_end: u64,
}

fn measure(label: &'static str, reps: usize, mut body: impl FnMut() -> u64) -> Arm {
    let mut runs = Vec::with_capacity(reps);
    let mut cpu = Vec::with_capacity(reps);
    let rss_start = rss_kib();
    for _ in 0..reps {
        let before = rusage();
        let started = Instant::now();
        let produced = body();
        let wall_ms = started.elapsed().as_secs_f64() * 1e3;
        let after = rusage();
        runs.push(wall_ms);
        cpu.push((after.user_us - before.user_us + after.sys_us - before.sys_us) / 1000.0);
        assert!(produced > 0);
    }
    Arm {
        label,
        runs,
        cpu,
        rss_start,
        rss_end: rss_kib(),
    }
}

fn stats(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    json!({
        "n": sorted.len(),
        "min": sorted.first().copied().unwrap_or(0.0),
        "p50": at(0.5),
        "p95": at(0.95),
        "max": sorted.last().copied().unwrap_or(0.0),
    })
}

fn arm_row(arm: &Arm) -> serde_json::Value {
    json!({
        "arm": arm.label,
        "wall_ms": stats(&arm.runs),
        "cpu_ms": stats(&arm.cpu),
        "rss_start_kib": arm.rss_start,
        "rss_end_kib": arm.rss_end,
        "rss_delta_kib": arm.rss_end.saturating_sub(arm.rss_start),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = PathBuf::from("/tmp/direct-restore-court");
    let mut json_path: Option<PathBuf> = None;
    let mut reps = 5usize;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--reps" => {
                reps = args[index + 1].parse().unwrap();
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    fs::create_dir_all(&out).unwrap();

    let fixture = Fixture::build();
    let store = fixture.open();
    let options = CheckpointRestoreOptions::default();
    let pipeline = RestorePipelineOptions::default();
    let total_bytes: u64 = fixture.tensors.iter().map(|(_, d)| d.len() as u64).sum();

    // ---- arm 1: ordinary restore into a container buffer, then copy --------
    let mut container_out: Option<Vec<u8>> = None;
    let arm_container = measure("container_then_copy", reps, || {
        let mut container = Vec::new();
        let report = store
            .restore_checkpoint_pipelined("cp", &options, &pipeline, &mut container)
            .unwrap();
        // A real caller needs the tensors separately: copy them out, which is
        // the second buffer the direct path does not need.
        let mut dest = destinations(&fixture.tensors);
        let header = report.file_bytes - total_bytes;
        let mut cursor = header as usize;
        for (name, data) in &fixture.tensors {
            dest.get_mut(name)
                .unwrap()
                .copy_from_slice(&container[cursor..cursor + data.len()]);
            cursor += data.len();
        }
        let produced = dest.values().map(|buffer| buffer.len() as u64).sum();
        container_out = Some(container);
        produced
    });

    // ---- arm 2: the same without the SHA-256 pass --------------------------
    let arm_unhashed = measure("container_unhashed", reps, || {
        let mut container = Vec::new();
        let report = store
            .restore_checkpoint_pipelined_unhashed("cp", &options, &pipeline, &mut container)
            .unwrap();
        assert!(
            report.file_bytes >= total_bytes,
            "the container carries the payloads plus its header"
        );
        container.len() as u64
    });

    // ---- arm 3: direct into caller-owned buffers --------------------------
    let mut direct_buffers: Option<DirectDestinations> = None;
    let arm_direct = measure("direct_into_destinations", reps, || {
        let mut dest = destinations(&fixture.tensors);
        let report = store
            .restore_checkpoint_direct_into("cp", &options, &pipeline, &mut dest)
            .unwrap();
        assert_eq!(report.sha256_passes, 0);
        assert!(!report.exact_source_verified);
        let produced = report.payload_bytes;
        direct_buffers = Some(dest);
        produced
    });

    // ---- arm 4: ordinary rank restore (with per-tensor SHA-256) -----------
    let topology = RestoreTopology::Sharded { axis: 0, shards: 4 };
    let mut rank_bytes: Option<BTreeMap<String, Vec<u8>>> = None;
    let arm_rank = measure("rank_ordinary", reps, || {
        let mut bytes = BTreeMap::new();
        let report = store
            .restore_checkpoint_rank("cp", &topology, 0, None, |row, data| {
                bytes.insert(row.name.clone(), data.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(report.tensors.len(), TENSORS);
        let produced = bytes.values().map(|value| value.len() as u64).sum();
        rank_bytes = Some(bytes);
        produced
    });

    // ---- arm 5: direct row slab (rank coordinates) ------------------------
    let mut slab_bytes: Option<BTreeMap<String, Vec<u8>>> = None;
    let arm_slab = measure("rank_direct_slab", reps, || {
        let mut bytes = BTreeMap::new();
        let stride = (COLS * 2) as usize;
        let mut produced = 0u64;
        for (name, _) in &fixture.tensors {
            let mut dst = vec![0u8; 512 * stride];
            let report = store
                .restore_row_slab_direct_at(None, "cp", name, (0, 512), &mut dst)
                .unwrap();
            assert!(report.tile_roots_verified);
            assert!(!report.full_root_verified);
            assert_eq!(report.sha256_passes, 0);
            produced += dst.len() as u64;
            bytes.insert(name.clone(), dst);
        }
        slab_bytes = Some(bytes);
        produced
    });

    // ---- exactness -------------------------------------------------------
    let mut checks: Vec<serde_json::Value> = Vec::new();
    {
        let direct = direct_buffers.as_ref().expect("direct arm ran");
        let mut exact = true;
        for (name, data) in &fixture.tensors {
            if &direct[name] != data {
                exact = false;
            }
        }
        checks.push(json!({
            "label": "direct buffers equal the saved tensors byte for byte",
            "ok": exact,
            "detail": format!("{TENSORS} tensors, {total_bytes} bytes"),
        }));

        let rank = rank_bytes.as_ref().expect("rank arm ran");
        let slab = slab_bytes.as_ref().expect("slab arm ran");
        let mut slab_exact = true;
        let stride = (COLS * 2) as usize;
        for (name, source) in &fixture.tensors {
            if rank[name] != slab[name] {
                slab_exact = false;
            }
            if slab[name][..] != source[..512 * stride] {
                slab_exact = false;
            }
        }
        checks.push(json!({
            "label": "the direct row slab equals the ordinary rank restore and the saved rows",
            "ok": slab_exact,
            "detail": "rows [0, 512) of every tensor".to_owned(),
        }));

        let container = container_out.as_ref().expect("container arm ran");
        let header = container.len() as u64 - total_bytes;
        let mut container_exact = true;
        let mut cursor = header as usize;
        for (name, data) in &fixture.tensors {
            if &container[cursor..cursor + data.len()] != &direct[name][..] {
                container_exact = false;
            }
            cursor += data.len();
        }
        checks.push(json!({
            "label": "the container restore and the direct restore agree on every byte",
            "ok": container_exact,
            "detail": format!("header {header} B"),
        }));
    }
    let container_len = container_out.as_ref().map(|c| c.len() as u64).unwrap_or(0);

    let rows = vec![
        arm_row(&arm_container),
        arm_row(&arm_unhashed),
        arm_row(&arm_direct),
        arm_row(&arm_rank),
        arm_row(&arm_slab),
    ];
    let document = json!({
        "schema": "weft.direct-restore.court.v1",
        "fixture": {
            "tensors": TENSORS,
            "rows": ROWS,
            "cols": COLS,
            "dtype": "bf16",
            "logical_bytes": total_bytes,
            "container_bytes": container_len,
            "shards": 4,
            "tile_shape": [128, 512],
            "strategy": "tensor-tiled",
        },
        "reps": reps,
        "arms": rows,
        "checks": checks,
        "checks_failed": checks.iter().filter(|check| check["ok"].as_bool() != Some(true)).count(),
        "environment": {
            "cpus": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "target": std::env::consts::ARCH,
            "clock": "Instant plus getrusage; RSS is VmHWM, monotone within one process",
        },
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(&path, text).unwrap(),
        None => println!("{text}"),
    }
    let _ = fixture.dir.path();
    if document["checks_failed"].as_u64().unwrap_or(1) != 0 {
        eprintln!(
            "direct_restore_court: {} check(s) failed",
            document["checks_failed"]
        );
        std::process::exit(1);
    }
}
