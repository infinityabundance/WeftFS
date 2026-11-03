//! Assemble the filesystem benchmark receipt from raw `fio --output-format=json`
//! files and the metadata captured by `bench/scripts/run_fs_bench.sh`.
//!
//! The helper deliberately emits no `winner` field and no cross-filesystem
//! claim. The tmpfs directory is recorded as a reference only.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    map
}

fn number(section: &Value, key: &str) -> Option<f64> {
    section.get(key).and_then(|value| value.as_f64())
}

fn percentile(section: &Value, wanted: f64) -> Option<f64> {
    for (key, multiplier) in [("clat_ns", 1.0), ("lat_ns", 1.0), ("clat", 1000.0)] {
        if let Some(container) = section.get(key) {
            if let Some(map) = container
                .get("percentile")
                .and_then(|value| value.as_object())
            {
                for (label, value) in map {
                    if let Ok(parsed) = label.parse::<f64>() {
                        if (parsed - wanted).abs() < 0.001 {
                            return value.as_f64().map(|number| number * multiplier);
                        }
                    }
                }
            }
        }
    }
    None
}

fn latency_mean(section: &Value) -> Option<(f64, f64)> {
    for (key, multiplier) in [("clat_ns", 1.0), ("lat_ns", 1.0), ("clat", 1000.0)] {
        if let Some(container) = section.get(key) {
            let mean = container.get("mean").and_then(|value| value.as_f64());
            let max = container.get("max").and_then(|value| value.as_f64());
            if mean.is_some() || max.is_some() {
                return Some((
                    mean.unwrap_or(0.0) * multiplier,
                    max.unwrap_or(0.0) * multiplier,
                ));
            }
        }
    }
    None
}

fn parse_fio(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let root: Value =
        serde_json::from_str(&text).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let job = root
        .get("jobs")
        .and_then(|value| value.as_array())
        .and_then(|jobs| jobs.first())
        .ok_or_else(|| format!("{} has no jobs array", path.display()))?;
    let read_section = job.get("read");
    let write_section = job.get("write");
    let write_ios = write_section
        .and_then(|section| number(section, "total_ios"))
        .unwrap_or(0.0);
    let read_ios = read_section
        .and_then(|section| number(section, "total_ios"))
        .unwrap_or(0.0);
    let (direction, section) = if write_ios > 0.0 && write_ios >= read_ios {
        ("write", write_section.expect("write section"))
    } else {
        ("read", read_section.expect("read section"))
    };
    let (mean_ns, max_ns) = latency_mean(section).unwrap_or((0.0, 0.0));
    Ok(json!({
        "direction": direction,
        "bw_kib_s": number(section, "bw"),
        "iops": number(section, "iops"),
        "runtime_ms": number(section, "runtime"),
        "io_bytes": number(section, "io_bytes"),
        "total_ios": number(section, "total_ios"),
        "error": number(section, "error"),
        "p50_ns": percentile(section, 50.0),
        "p95_ns": percentile(section, 95.0),
        "p99_ns": percentile(section, 99.0),
        "mean_ns": mean_ns,
        "max_ns": max_ns,
        "jobname": job.get("jobname").cloned().unwrap_or(Value::Null),
        "raw": root,
    }))
}

fn main() {
    let mut raw_dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--raw-dir" => raw_dir = Some(PathBuf::from(args.next().expect("dir"))),
            "--out" => out = Some(PathBuf::from(args.next().expect("path"))),
            other => panic!("unknown argument {other}"),
        }
    }
    let raw_dir = raw_dir.expect("--raw-dir is required");
    let out = out.expect("--out is required");

    let environment = read_optional(&raw_dir.join("environment.txt"))
        .map(|text| parse_env(&text))
        .unwrap_or_default();
    let environment_json: Map<String, Value> = environment
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let status = read_optional(&raw_dir.join("status.txt")).unwrap_or_default();
    let go_status = status
        .lines()
        .find_map(|line| line.strip_prefix("go_status="))
        .unwrap_or("unknown")
        .trim()
        .to_owned();
    let go_error = read_optional(&raw_dir.join("go-error.txt"));

    let mut samples: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    let mut raw_files: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&raw_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            let parts: Vec<&str> = stem.split("--").collect();
            if parts.len() != 3 || !parts[2].starts_with('r') {
                continue;
            }
            let target = parts[0].to_owned();
            let workload = parts[1].to_owned();
            let rep: u64 = parts[2][1..].parse().unwrap_or(0);
            match parse_fio(&path) {
                Ok(mut parsed) => {
                    if let Some(object) = parsed.as_object_mut() {
                        object.insert("rep".to_owned(), json!(rep));
                        object.insert(
                            "raw_file".to_owned(),
                            json!(path
                                .strip_prefix(&raw_dir)
                                .unwrap_or(&path)
                                .display()
                                .to_string()),
                        );
                    }
                    samples.entry((target, workload)).or_default().push(parsed);
                    raw_files.push(path.display().to_string());
                }
                Err(err) => {
                    eprintln!("warning: {err}");
                }
            }
        }
    }

    let mut results: Vec<Value> = Vec::new();
    for ((target, workload), mut points) in samples {
        points.sort_by_key(|point| point.get("rep").and_then(|rep| rep.as_u64()).unwrap_or(0));
        let field_values = |field: &str| -> Vec<f64> {
            points
                .iter()
                .filter_map(|point| point.get(field).and_then(|value| value.as_f64()))
                .collect()
        };
        let mut bw = field_values("bw_kib_s");
        let mut iops = field_values("iops");
        let mut p50 = field_values("p50_ns");
        let mut p95 = field_values("p95_ns");
        let mut p99 = field_values("p99_ns");
        results.push(json!({
            "target": target,
            "workload": workload,
            "samples": points,
            "median": {
                "bw_kib_s": median(&mut bw),
                "iops": median(&mut iops),
                "p50_ns": median(&mut p50),
                "p95_ns": median(&mut p95),
                "p99_ns": median(&mut p99),
            },
        }));
    }

    let mount_info = |name: &str| read_optional(&raw_dir.join(format!("mountinfo-{name}.txt")));
    let commands = read_optional(&raw_dir.join("commands.txt")).unwrap_or_default();
    let fio_version = read_optional(&raw_dir.join("fio-version.txt")).unwrap_or_default();

    let report = json!({
        "schema": "weft-fs-bench-v1",
        "kind": "filesystem-fio",
        "environment": environment_json,
        "fio_version": fio_version.trim(),
        "go_status": go_status,
        "go_error": go_error,
        "mounts": {
            "rust": mount_info("rust"),
            "go": mount_info("go"),
            "tmpfs_reference": mount_info("tmpfs"),
        },
        "commands": commands,
        "raw_files": raw_files,
        "workloads": {
            "seq-write-1m": "sequential write, 1 MiB blocks, 8 MiB, psync, buffered (direct=0)",
            "seq-read-1m": "sequential read of the pre-created 8 MiB file, 1 MiB blocks",
            "rand-read-4k": "random read, 4 KiB blocks, 8 MiB file, psync",
            "rand-write-4k": "random write, 4 KiB blocks, 8 MiB, psync",
            "fsync-write-4k": "sequential write, 4 KiB blocks, 1 MiB, fsync after every write",
        },
        "results": results,
        "limitations": [
            "The tmpfs directory is a REFERENCE ONLY. It has no durability and no FUSE layer, so it is explicitly non-equivalent to WeftFS. No superiority claim follows from it.",
            "No ext4/XFS/Btrfs/ZFS baseline is run in this pass; those filesystems are not available on the court's bind mount.",
            "fio runs buffered (direct=0). Direct I/O is not benchmarked because same-inode concurrent direct I/O on kernel 7.3 can return EIO with this fuser version (see docs/fuse-status.md).",
            "`fsync` through FUSE exercises the daemon's fsync path, not physical media power-loss durability.",
            "Every run is in one OOM-limited container (memory==swap, pids, cpus). Throughput and latency are single-host observations, not hardware-independent claims.",
            "The receipt embeds raw fio JSON but computes only medians of three repetitions; no confidence interval is claimed."
        ],
        "notes": [
            "Both WeftFS mounts and the tmpfs directory are in the same container and run identical fio arguments.",
            "The Go mount is recorded as `go_status=blocked` with the exact error when it cannot be built or mounted.",
            "No `winner` field is emitted. See bench/RESULTS-v1.md for scoped analysis and losses."
        ]
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize receipt");
    fs::write(&out, text).expect("write receipt");
    eprintln!("wrote {}", out.display());
}
