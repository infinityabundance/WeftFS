//! Encryption-at-rest cost court (T079).
//!
//! Measures the same fixture three ways — plain, convergent envelope, random
//! envelope — with interleaved repetitions, so a slow period of the host lands
//! on all three arms rather than on one.
//!
//! Reported per arm and repetition: write wall, read wall, write/read CPU
//! (user+sys), physical footprint, backend bytes written/read, object count,
//! and byte-exactness of the restore. Plus a plane-level seal/open throughput
//! and the two dedup-policy facts the documentation must not hide:
//!
//! * equal plaintext seals to equal ciphertext under `convergent` and to
//!   different ciphertext under `random`;
//! * the WeftFS blob *name* is the plaintext content hash in every mode, so
//!   neither mode hides equality from an observer of the object namespace.
//!
//! Run only inside the bench court:
//! `scripts/docker-court.sh bench "cd /work && bash bench/scripts/encryption_court.sh"`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key};
use weftfs::encryption::{self, EncryptionConfig, EncryptionMode};
use weftfs::planes::BlobPlane;
use weftfs::store::{OpenMode, Options, Store};

const MIB: usize = 1024 * 1024;
const BIG_BYTES: usize = 64 * MIB;
const SMALL_COUNT: usize = 128;
const SMALL_BYTES: usize = 8 * 1024;
const DUP_BYTES: usize = 4 * MIB;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

/// Deterministic non-repeating bytes.
fn fill(len: usize, salt: u64) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut x = salt.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    for byte in out.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *byte = (x & 0xff) as u8;
    }
    out
}

fn cpu_ms() -> f64 {
    // SAFETY: `getrusage` writes into a zeroed `rusage` for RUSAGE_SELF.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return f64::NAN;
        }
        let user = usage.ru_utime.tv_sec as f64 * 1000.0 + usage.ru_utime.tv_usec as f64 / 1000.0;
        let sys = usage.ru_stime.tv_sec as f64 * 1000.0 + usage.ru_stime.tv_usec as f64 / 1000.0;
        user + sys
    }
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn count_files(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += count_files(&entry.path());
        } else {
            total += 1;
        }
    }
    total
}

fn arm_kind(arm: &str) -> BackendKind {
    match arm {
        "plain" => BackendKind::Local,
        "convergent" => BackendKind::Encrypted {
            inner: Box::new(BackendKind::Local),
            config: EncryptionConfig::from_secret(
                EncryptionMode::Convergent,
                encryption::test_key("court-convergent"),
            ),
        },
        "random" => BackendKind::Encrypted {
            inner: Box::new(BackendKind::Local),
            config: EncryptionConfig::from_secret(
                EncryptionMode::Random,
                encryption::test_key("court-random"),
            ),
        },
        other => panic!("unknown arm {other}"),
    }
}

struct Fixture {
    big: Vec<u8>,
    small: Vec<Vec<u8>>,
    duplicate: Vec<u8>,
}

fn fixture() -> Fixture {
    Fixture {
        big: fill(BIG_BYTES, 0x1111),
        small: (0..SMALL_COUNT)
            .map(|i| fill(SMALL_BYTES, 0x2000 + i as u64))
            .collect(),
        duplicate: fill(DUP_BYTES, 0x9999),
    }
}

/// One measured write+read cycle for one arm.
struct Row {
    arm: String,
    rep: usize,
    write_ms: f64,
    read_ms: f64,
    write_cpu_ms: f64,
    read_cpu_ms: f64,
    physical_bytes: u64,
    backend_bytes_written: u64,
    backend_bytes_read: u64,
    object_files: u64,
    exact: bool,
}

fn run_arm(dir: &Path, arm: &str, rep: usize, fixture: &Fixture) -> Row {
    let attrs = BTreeMap::new();
    let cpu0 = cpu_ms();
    let started = Instant::now();
    let mut store = Store::init_with_backend(dir, opts(), arm_kind(arm)).expect("init");
    let root = store.resolve_path("/").expect("root");
    let mut seeded = Vec::new();
    let big = store
        .create_file_seeded(
            root,
            b"big.bin",
            &fixture.big,
            0o644,
            0,
            0,
            attrs.clone(),
            b"big-seed",
        )
        .expect("big");
    seeded.push((big, "big.bin".to_string()));
    for (index, bytes) in fixture.small.iter().enumerate() {
        let name = format!("small-{index:03}.bin");
        let id = store
            .create_file_seeded(
                root,
                name.as_bytes(),
                bytes,
                0o644,
                0,
                0,
                attrs.clone(),
                format!("small-seed-{index}").as_bytes(),
            )
            .expect("small");
        seeded.push((id, name));
    }
    // Two identical files: the store's logical dedup gives them one content
    // root in every mode, which is the equality an observer of the namespace
    // can still see under `random`.
    let first = store
        .create_file_seeded(
            root,
            b"dup-a.bin",
            &fixture.duplicate,
            0o644,
            0,
            0,
            attrs.clone(),
            b"dup-a",
        )
        .expect("dup a");
    let second = store
        .create_file_seeded(
            root,
            b"dup-b.bin",
            &fixture.duplicate,
            0o644,
            0,
            0,
            attrs.clone(),
            b"dup-b",
        )
        .expect("dup b");
    let write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let write_cpu_ms = cpu_ms() - cpu0;
    let write_stats = store.backend_stats();
    let object_files = count_files(&dir.join("objects"));
    drop(store);

    let physical_bytes = dir_bytes(dir);
    let cpu0 = cpu_ms();
    let started = Instant::now();
    let store = Store::open_with_backend(dir, OpenMode::ReadOnly, arm_kind(arm)).expect("reopen");
    let mut exact = true;
    for (id, name) in &seeded {
        let restored = store.read_object_at(None, *id).expect("read");
        let expected = if name == "big.bin" {
            &fixture.big
        } else {
            let index: usize = name
                .trim_start_matches("small-")
                .trim_end_matches(".bin")
                .parse()
                .expect("index");
            &fixture.small[index]
        };
        exact &= restored == *expected;
    }
    // Snapshot-style pinned reads of the duplicate pair.
    exact &= store.read_object_at(None, first).expect("dup a") == fixture.duplicate;
    exact &= store.read_object_at(None, second).expect("dup b") == fixture.duplicate;
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let read_cpu_ms = cpu_ms() - cpu0;
    let read_stats = store.backend_stats();
    drop(store);

    Row {
        arm: arm.to_owned(),
        rep,
        write_ms,
        read_ms,
        write_cpu_ms,
        read_cpu_ms,
        physical_bytes,
        backend_bytes_written: write_stats.bytes_written,
        backend_bytes_read: read_stats.bytes_read,
        object_files,
        exact,
    }
}

/// Plane-level seal/open throughput and the two dedup facts.
fn plane_bench() -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (mode, label) in [
        (EncryptionMode::Convergent, "convergent"),
        (EncryptionMode::Random, "random"),
    ] {
        let config = EncryptionConfig::from_secret(mode, encryption::test_key("court-plane"));
        let block = fill(MIB, 0x4242);
        let copies = 64usize;
        let cpu0 = cpu_ms();
        let started = Instant::now();
        let mut sealed = Vec::with_capacity(copies);
        for index in 0..copies {
            let mut plain = block.clone();
            // Distinct trailing 8 bytes so every object is a distinct key.
            plain[..8].copy_from_slice(&(index as u64).to_le_bytes());
            sealed.push(encryption::seal(&config, &plain).expect("seal"));
        }
        let seal_ms = started.elapsed().as_secs_f64() * 1000.0;
        let seal_cpu_ms = cpu_ms() - cpu0;

        let cpu0 = cpu_ms();
        let started = Instant::now();
        for index in 0..copies {
            let mut plain = block.clone();
            plain[..8].copy_from_slice(&(index as u64).to_le_bytes());
            let opened = encryption::open(&config, &sealed[index]).expect("open");
            assert_eq!(opened, plain);
        }
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        let open_cpu_ms = cpu_ms() - cpu0;

        let mib = (copies * MIB) as f64 / (1024.0 * 1024.0);
        out.insert(
            label.to_owned(),
            json!({
                "seal_mib": copies as f64,
                "seal_wall_ms": seal_ms,
                "seal_cpu_ms": seal_cpu_ms,
                "seal_mib_per_s": mib / (seal_ms / 1000.0),
                "open_wall_ms": open_ms,
                "open_cpu_ms": open_cpu_ms,
                "open_mib_per_s": mib / (open_ms / 1000.0),
                "ciphertext_bytes": sealed.len() as u64 * (MIB as u64
                    + encryption::HEADER_BYTES as u64
                    + encryption::TAG_BYTES as u64),
            }),
        );
    }

    // Equal plaintext: equal ciphertext under convergent, different under
    // random.
    let plain = fill(300_000, 0x7777);
    let conv = EncryptionConfig::from_secret(
        EncryptionMode::Convergent,
        encryption::test_key("court-plane"),
    );
    let rand =
        EncryptionConfig::from_secret(EncryptionMode::Random, encryption::test_key("court-plane"));
    let conv_a = encryption::seal(&conv, &plain).expect("seal");
    let conv_b = encryption::seal(&conv, &plain).expect("seal");
    let rand_a = encryption::seal(&rand, &plain).expect("seal");
    let rand_b = encryption::seal(&rand, &plain).expect("seal");

    // Re-publishing the same key with the same plaintext: convergent is an
    // idempotent no-op at the create-once plane, random conflicts because the
    // ciphertext differs. A store must therefore skip a second publication by
    // name, which is exactly what the measured store does.
    let dir = tempdir().expect("tempdir");
    let mut conv_plane = encryption::EncryptedBlobPlane::new(
        weftfs::backend::LocalBackend::new(dir.path()),
        conv.clone(),
    )
    .expect("convergent plane");
    conv_plane.init_layout().expect("layout");
    let key = Key::object(weftfs::Hash32::from_bytes([0x5a; 32]));
    conv_plane.put_if_absent(&key, &plain).expect("first");
    let convergent_republish = match conv_plane.put_if_absent(&key, &plain) {
        Ok(()) => "idempotent",
        Err(err) => {
            return json!({
                "error": format!("convergent republish failed: {err}"),
            })
        }
    };
    let dir = tempdir().expect("tempdir");
    let mut rand_plane = encryption::EncryptedBlobPlane::new(
        weftfs::backend::LocalBackend::new(dir.path()),
        rand.clone(),
    )
    .expect("random plane");
    rand_plane.init_layout().expect("layout");
    rand_plane.put_if_absent(&key, &plain).expect("first");
    let random_republish = match rand_plane.put_if_absent(&key, &plain) {
        Ok(()) => "idempotent",
        Err(err) => match err.class() {
            weftfs::Class::Exists => "conflict",
            _ => "other-error",
        },
    };

    json!({
        "modes": out,
        "equal_plaintext_equal_ciphertext": {
            "convergent": conv_a == conv_b,
            "random": rand_a == rand_b,
        },
        "convergent_republish": convergent_republish,
        "random_republish": random_republish,
        "envelope_overhead_bytes_per_object": encryption::HEADER_BYTES + encryption::TAG_BYTES,
    })
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn main() {
    let mut out_path: Option<PathBuf> = None;
    let mut reps = 7usize;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--reps" => {
                reps = args[index + 1].parse().expect("reps");
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }

    let fixture = fixture();
    let arms = ["plain", "convergent", "random"];
    let mut rows: Vec<Row> = Vec::new();
    for rep in 0..reps {
        // Interleave the arms inside every repetition.
        for arm in arms {
            let dir = tempdir().expect("tempdir");
            let row = run_arm(dir.path(), arm, rep, &fixture);
            eprintln!(
                "rep {rep} {arm}: write {:.1} ms, read {:.1} ms, physical {} B, objects {}",
                row.write_ms, row.read_ms, row.physical_bytes, row.object_files
            );
            rows.push(row);
        }
    }

    let plane = plane_bench();

    let mut summary = serde_json::Map::new();
    for arm in arms {
        let mut write: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.write_ms)
            .collect();
        let mut read: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.read_ms)
            .collect();
        let mut wcpu: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.write_cpu_ms)
            .collect();
        let mut rcpu: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.read_cpu_ms)
            .collect();
        let footprints: Vec<u64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.physical_bytes)
            .collect();
        let objects: Vec<u64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.object_files)
            .collect();
        let exact = rows.iter().filter(|r| r.arm == arm).all(|r| r.exact);
        let footprint_uniform = footprints.windows(2).all(|pair| pair[0] == pair[1]);
        let objects_uniform = objects.windows(2).all(|pair| pair[0] == pair[1]);
        let mut written: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.backend_bytes_written as f64)
            .collect();
        let mut read_bytes: Vec<f64> = rows
            .iter()
            .filter(|r| r.arm == arm)
            .map(|r| r.backend_bytes_read as f64)
            .collect();
        summary.insert(
            arm.to_owned(),
            json!({
                "reps": write.len(),
                "write_ms_median": median(&mut write),
                "read_ms_median": median(&mut read),
                "write_cpu_ms_median": median(&mut wcpu),
                "read_cpu_ms_median": median(&mut rcpu),
                "physical_bytes": footprints.first().copied().unwrap_or(0),
                "physical_bytes_uniform_across_reps": footprint_uniform,
                "object_files": objects.first().copied().unwrap_or(0),
                "object_files_uniform_across_reps": objects_uniform,
                "exact": exact,
                "backend_bytes_written_median": median(&mut written),
                "backend_bytes_read_median": median(&mut read_bytes),
            }),
        );
    }

    let plain = &summary["plain"];
    let mut checks = Vec::new();
    let mut check = |name: &str, pass: bool, detail: String| {
        checks.push(json!({"name": name, "pass": pass, "detail": detail}));
    };
    for arm in arms {
        check(
            &format!("{arm}_exact"),
            summary[arm]["exact"].as_bool().unwrap_or(false),
            "every restored object equals the fixture".to_owned(),
        );
        check(
            &format!("{arm}_footprint_uniform"),
            summary[arm]["physical_bytes_uniform_across_reps"]
                .as_bool()
                .unwrap_or(false),
            "the footprint does not depend on the repetition".to_owned(),
        );
    }
    let plain_bytes = plain["physical_bytes"].as_u64().unwrap_or(0) as f64;
    for arm in ["convergent", "random"] {
        let bytes = summary[arm]["physical_bytes"].as_u64().unwrap_or(0) as f64;
        let overhead = if plain_bytes > 0.0 {
            (bytes - plain_bytes) / plain_bytes * 100.0
        } else {
            0.0
        };
        // One 96-byte header plus one 16-byte tag per object (64 KiB chunk),
        // so the expected footprint overhead is about 0.17%.
        check(
            &format!("{arm}_footprint_within_envelope_bound"),
            bytes <= plain_bytes * 1.01,
            format!("{overhead:.4}% over plain for the {arm} envelope"),
        );
    }
    check(
        "convergent_keeps_equal_object_names",
        summary["convergent"]["object_files"] == summary["plain"]["object_files"],
        format!(
            "plain {} objects, convergent {} objects",
            summary["plain"]["object_files"], summary["convergent"]["object_files"]
        ),
    );
    check(
        "random_keeps_equal_object_names",
        summary["random"]["object_files"] == summary["plain"]["object_files"],
        format!(
            "plain {} objects, random {} objects: the blob name is the plaintext hash in every mode",
            summary["plain"]["object_files"], summary["random"]["object_files"]
        ),
    );
    let equal = &plane["equal_plaintext_equal_ciphertext"];
    check(
        "convergent_equality",
        equal["convergent"].as_bool().unwrap_or(false),
        "equal plaintext seals to equal ciphertext under convergent".to_owned(),
    );
    check(
        "random_inequality",
        !equal["random"].as_bool().unwrap_or(true),
        "equal plaintext does not seal to equal ciphertext under random".to_owned(),
    );
    check(
        "convergent_republish_idempotent",
        plane["convergent_republish"].as_str() == Some("idempotent"),
        format!("republish: {}", plane["convergent_republish"]),
    );
    check(
        "random_republish_conflicts",
        plane["random_republish"].as_str() == Some("conflict"),
        format!("republish: {}", plane["random_republish"]),
    );

    let passed = checks
        .iter()
        .filter(|check| check["pass"].as_bool().unwrap_or(false))
        .count();
    let failed = checks.len() - passed;

    let receipt = json!({
        "schema": "weft.encryption.bench.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "generated_utc": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "environment": {
            "revisions": {
                "WeftFS": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            },
            "label": "single host, one bench container, tmpfs fixture, warm page cache; no media, power-loss, fleet or cloud claim; the envelope covers blob bodies only",
        },
        "command": "scripts/docker-court.sh bench \"cd /work && bash bench/scripts/encryption_court.sh\"",
        "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
        "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
        "court_memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
        "court_cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
        "court_pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
        "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
        "host_revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
        "fixture": {
            "big_bytes": BIG_BYTES,
            "small_count": SMALL_COUNT,
            "small_bytes": SMALL_BYTES,
            "duplicate_bytes": DUP_BYTES,
            "logical_bytes": BIG_BYTES + SMALL_COUNT * SMALL_BYTES + 2 * DUP_BYTES,
            "chunk_size": 65_536,
            "generator": "xorshift64 deterministic fill; synthetic, not model weights",
        },
        "method": {
            "reps": reps,
            "interleaved": "arms run inside every rep in the order plain, convergent, random",
            "write": "init + one create per file (each its own commit) + backend stats",
            "read": "drop, reopen read-only, read and compare every object",
            "cpu": "getrusage(RUSAGE_SELF) user+sys delta around each phase",
            "physical": "sum of every file under the store root",
        },
        "rows": rows.iter().map(|row| json!({
            "arm": row.arm,
            "rep": row.rep,
            "write_ms": row.write_ms,
            "read_ms": row.read_ms,
            "write_cpu_ms": row.write_cpu_ms,
            "read_cpu_ms": row.read_cpu_ms,
            "physical_bytes": row.physical_bytes,
            "backend_bytes_written": row.backend_bytes_written,
            "backend_bytes_read": row.backend_bytes_read,
            "object_files": row.object_files,
            "exact": row.exact,
        })).collect::<Vec<_>>(),
        "summary": summary,
        "plane": plane,
        "checks": checks,
        "passed": passed,
        "failed": failed,
        "result": if failed == 0 { "pass" } else { "fail" },
        "limits": [
            "single host, one container, warm page cache for the read phase",
            "synthetic fixture; not model weights",
            "no physical power-loss, device-cache, fleet or cloud claim",
            "the envelope covers blob bodies only: commit and control records are plaintext",
            "no claim that encryption hides equality: the blob name is the plaintext hash",
        ],
    });

    let text = serde_json::to_string_pretty(&receipt).expect("json");
    match out_path {
        Some(path) => {
            fs::write(&path, text).expect("write receipt");
            eprintln!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
