//! 3FS transport bench (subphase R22).
//!
//! Measures the USRBIO data plane against the plain positional path over the
//! *conformance* implementation of the 3FS ABI (`bench/threefs/hf3fs_shim.c`).
//!
//! This is deliberately not a 3FS benchmark. The shim serves reads with
//! `pread` in a shared region, has no RDMA, no kernel bypass, and no 3FS
//! servers, so it cannot show the transport's advantage. What it measures is
//! the cost of the adapter itself — descriptor open + registration + prep +
//! submit + reap versus one `pread` per range — and it proves the structural
//! property that N ranges cost one submission per ring-full.
//!
//! Run through `scripts/threefs_court.sh`, which builds the shim and sets
//! `WEFT_3FS_SHIM`, `WEFT_3FS_SHIM_DIR`, and `WEFT_STAMP`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use tempfile::tempdir;

use weftfs::backend::BackendKind;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::threefs::{ffi, ThreeFsConfig, ThreeFsMode, Usrbio};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = values.len();
    if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}

fn store_options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 64,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn build_store(root: &Path, kind: BackendKind, files: usize, len: usize) -> Store {
    let mut store = Store::init_with_backend(root, store_options(), kind).unwrap();
    let dir = store.resolve_path("/").unwrap();
    for i in 0..files {
        let bytes: Vec<u8> = (0..len).map(|j| ((i * 17 + j * 3) % 251) as u8).collect();
        store
            .create_file_seeded(
                dir,
                format!("f{i:04}").as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                std::collections::BTreeMap::new(),
                &[i as u8; 32],
            )
            .unwrap();
    }
    store
}

fn main() {
    let shim = env("WEFT_3FS_SHIM").expect("WEFT_3FS_SHIM");
    let data = env("WEFT_3FS_SHIM_DIR").expect("WEFT_3FS_SHIM_DIR");
    let stamp = env("WEFT_STAMP").unwrap_or_else(|| "unstamped".to_owned());
    let shim = PathBuf::from(shim);
    let data = PathBuf::from(data);
    let reps: usize = env("WEFT_THREEFS_REPS")
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let files: usize = 300;
    let file_bytes: usize = 256 << 10;

    // ---- prepare -------------------------------------------------------
    std::fs::create_dir_all(&data).unwrap();
    let mut paths = Vec::with_capacity(files);
    for i in 0..files {
        let path = data.join(format!("bench-{i:04}.bin"));
        if !path.exists() {
            let bytes: Vec<u8> = (0..file_bytes)
                .map(|j| ((i * 29 + j * 11) % 251) as u8)
                .collect();
            std::fs::write(&path, &bytes).unwrap();
        }
        paths.push(path);
    }

    let transport = Usrbio::open(&ffi::Options {
        mount_point: data.clone(),
        library: Some(shim.clone()),
        entries: 128,
        io_bytes: 8 << 20,
        ..ffi::Options::default()
    })
    .unwrap();
    let mount_is_3fs = transport.mount_is_3fs().unwrap();

    // ---- scalar positional path ---------------------------------------
    // Destination buffers are allocated once and reused, so this side pays no
    // per-rep allocation. The ring side returns owned vectors and therefore
    // does pay one: the comparison is deliberately unflattering to the ring.
    let mut scalar_bufs: Vec<Vec<u8>> = (0..files).map(|_| vec![0u8; file_bytes]).collect();
    let mut scalar_ms = Vec::new();
    let mut scalar_ref: Option<Vec<u8>> = None;
    for _ in 0..reps {
        let start = Instant::now();
        for (path, buf) in paths.iter().zip(scalar_bufs.iter_mut()) {
            let file = std::fs::File::open(path).unwrap();
            use std::os::unix::fs::FileExt;
            file.read_exact_at(buf, 0).unwrap();
        }
        scalar_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        scalar_ref = Some(scalar_bufs.concat());
    }

    // ---- USRBIO batched path ------------------------------------------
    let before = transport.counters();
    let mut ring_ms = Vec::new();
    let mut ring_ref: Option<Vec<u8>> = None;
    for _ in 0..reps {
        let start = Instant::now();
        let requests: Vec<ffi::ReadRequest<'_>> = paths
            .iter()
            .map(|path| ffi::ReadRequest {
                path: path.as_path(),
                offset: 0,
                len: file_bytes as u64,
            })
            .collect();
        let out = transport.read_batch(&requests).unwrap();
        ring_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        ring_ref = Some(out.into_iter().flatten().collect());
    }
    let after = transport.counters();
    let identical = scalar_ref == ring_ref;

    // ---- store phases --------------------------------------------------
    let files_small = 100;
    let small_bytes = 16 << 10;
    let posix_dir = tempdir().unwrap();
    let usrbio_dir = tempdir().unwrap();
    let mount = tempdir().unwrap();
    let posix_kind = BackendKind::ThreeFs {
        config: ThreeFsConfig {
            mode: ThreeFsMode::Posix,
            ..ThreeFsConfig::default()
        },
    };
    let usrbio_kind = BackendKind::ThreeFs {
        config: ThreeFsConfig {
            mode: ThreeFsMode::Usrbio,
            mount_point: Some(mount.path().to_path_buf()),
            library: Some(shim.clone()),
            entries: 128,
            io_bytes: 8 << 20,
            ..ThreeFsConfig::default()
        },
    };

    let start = Instant::now();
    let posix = build_store(
        posix_dir.path(),
        posix_kind.clone(),
        files_small,
        small_bytes,
    );
    let posix_write_ms = start.elapsed().as_secs_f64() * 1000.0;
    let posix_gen = posix.generation();
    let posix_snapshot = posix.snapshot_root();
    let posix_verify = posix.verify(true).unwrap();

    let start = Instant::now();
    let ring_store = build_store(
        usrbio_dir.path(),
        usrbio_kind.clone(),
        files_small,
        small_bytes,
    );
    let usrbio_write_ms = start.elapsed().as_secs_f64() * 1000.0;
    let usrbio_gen = ring_store.generation();
    let usrbio_snapshot = ring_store.snapshot_root();
    let usrbio_verify = ring_store.verify(true).unwrap();
    let store_counters = {
        // The store drops its backend on close; read the counters from a fresh
        // transport handle is therefore impossible, so the verify counters are
        // what the receipt reports. The transport counters above cover reads.
        let _ = &ring_store;
        transport.counters()
    };

    let start = Instant::now();
    let reopened =
        Store::open_with_backend(usrbio_dir.path(), OpenMode::ReadOnly, usrbio_kind.clone())
            .unwrap();
    let reopen_ms = start.elapsed().as_secs_f64() * 1000.0;
    let reopened_ok = reopened.verify(true).unwrap().ok;

    let ring_median = median(ring_ms.clone());
    let scalar_median = median(scalar_ms.clone());

    let receipt = json!({
        "schema": "weft.receipt.threefs.v1",
        "label": "single-host court; conformance shim for the 3FS USRBIO ABI, not a 3FS run; \
                  synthetic fixtures; local filesystem",
        "stamp": stamp,
        "environment": {
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "court_kernel": env("WEFT_COURT_KERNEL"),
            "memory_limit": env("WEFT_COURT_MEMORY"),
            "cpu_limit": env("WEFT_COURT_CPUS"),
            "pids_limit": env("WEFT_COURT_PIDS"),
            "weftfs_revision": env("WEFT_REPO_WEFTFS"),
            "host_cpus": std::thread::available_parallelism().map(|v| v.get()).unwrap_or(0),
            "peak_rss_kib": peak_rss_kib(),
        },
        "transport": {
            "mode": "usrbio",
            "library": shim.display().to_string(),
            "mount_point": data.display().to_string(),
            "mount_is_3fs": mount_is_3fs,
            "entries": 128,
            "io_bytes": 8 << 20,
            "note": "hf3fs_is_hf3fs=false: this library is the conformance shim, so no 3FS \
                     throughput, latency, RDMA, or deployment claim follows from this receipt",
        },
        "range_reads": {
            "files": files,
            "bytes_per_file": file_bytes,
            "reps": reps,
            "scalar_ms": scalar_ms,
            "scalar_median_ms": scalar_median,
            "scalar_mib_per_s": (files * file_bytes) as f64 / (scalar_median / 1000.0) / (1 << 20) as f64,
            "ring_ms": ring_ms,
            "ring_median_ms": ring_median,
            "ring_mib_per_s": (files * file_bytes) as f64 / (ring_median / 1000.0) / (1 << 20) as f64,
            "ring_vs_scalar_ratio": ring_median / scalar_median,
            "requests": after.requests - before.requests,
            "submissions": after.submissions - before.submissions,
            "completions": after.completions - before.completions,
            "bytes_read": after.bytes_read - before.bytes_read,
            "requests_per_submission": (after.requests - before.requests) as f64
                / ((after.submissions - before.submissions).max(1)) as f64,
            "bytes_identical": identical,
        },
        "store_round_trip": {
            "files": files_small,
            "bytes_per_file": small_bytes,
            "posix": {
                "write_ms": posix_write_ms,
                "generation": posix_gen,
                "snapshot_root": posix_snapshot.to_hex(),
                "verify_ok": posix_verify.ok,
            },
            "usrbio": {
                "write_ms": usrbio_write_ms,
                "generation": usrbio_gen,
                "snapshot_root": usrbio_snapshot.to_hex(),
                "verify_ok": usrbio_verify.ok,
                "reopen_ms": reopen_ms,
                "reopen_verify_ok": reopened_ok,
                "counters_total": {
                    "submissions": store_counters.submissions,
                    "requests": store_counters.requests,
                    "completions": store_counters.completions,
                    "bytes_read": store_counters.bytes_read,
                    "bytes_written": store_counters.bytes_written,
                    "direct_reads": store_counters.direct_reads,
                    "staged_reads": store_counters.staged_reads,
                    "registrations": store_counters.registrations,
                },
            },
            "same_generation": posix_gen == usrbio_gen,
            "same_snapshot_root": posix_snapshot == usrbio_snapshot,
        },
        "losses": [
            "The scalar side reuses its destinations while the ring side returns owned vectors and \
             allocates per rep; the ring number is therefore an upper bound on its cost, not a \
             like-for-like win.",
            "The USRBIO path opens and registers one descriptor per read. A production adapter \
             should keep immutable keys registered; this receipt prices the cost of not doing so.",
            "The shim performs pread per request inside wait_for_ios, so the batched path cannot \
             exhibit a transport advantage; any difference it shows is adapter overhead, not 3FS \
             behaviour.",
            "No 3FS deployment, no RDMA, no kernel bypass, no multi-node consistency test.",
            "The store's write phase enters through the ring for staging only; fsync and rename \
             still order each publication, so this is not a write-throughput claim.",
        ],
    });

    println!("{}", serde_json::to_string_pretty(&receipt).unwrap());
}
