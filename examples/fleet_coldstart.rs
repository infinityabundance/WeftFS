//! Fleet cold-start court (subphase R12, increment 2).
//!
//! One **shared** read-only `Store` in one server process, N FUSE views over it
//! (`SharedMount`, subphase R7), and N independent replica *processes* that each
//! load the same model through their own view. Because every view reads through
//! the same `Arc<Store>`, the server's single-flight registry and decoded-object
//! cache are shared by the whole fleet; that is what the two configurations
//! compare.
//!
//!   fleet_coldstart server  --store DIR [--backend s3]
//!                           --mounts FILE --expect-replicas N --done-dir DIR
//!                           --ready-file FILE --stats-out FILE
//!                           [--single-flight on|off] [--cache-bytes N]
//!                           [--cache-entries N] [--timeout-secs N]
//!
//!   fleet_coldstart replica --mount DIR --dir model --go-file FILE
//!                           --done-file FILE --result-out FILE
//!
//! This is one host with N processes, not a fleet claim.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftfs::backend::BackendKind;
use weftfs::fuse::{SharedMount, SharedViewSpec};
use weftfs::store::{OpenMode, Options, Store};

fn main() {
    if let Err(err) = run() {
        eprintln!("fleet_coldstart: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "server" => server(&args[1..]),
        "replica" => replica(&args[1..]),
        other => Err(format!(
            "unknown phase `{other}` (expected server, replica)"
        )),
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


fn parse_u64(args: &[String], flag: &str, default: u64) -> Result<u64, String> {
    match value_of(args, flag) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("{flag} must be an integer (got `{text}`)")),
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
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

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

fn server(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let mounts_file = PathBuf::from(required(args, "--mounts")?);
    let done_dir = PathBuf::from(required(args, "--done-dir")?);
    let ready_file = PathBuf::from(required(args, "--ready-file")?);
    let stats_out = PathBuf::from(required(args, "--stats-out")?);
    let expect = parse_u64(args, "--expect-replicas", 1)? as usize;
    let timeout = Duration::from_secs(parse_u64(args, "--timeout-secs", 900)?);
    let single_flight = value_of(args, "--single-flight").as_deref() != Some("off");
    let cache_bytes = parse_u64(args, "--cache-bytes", 0)?;
    let cache_entries = parse_u64(args, "--cache-entries", 65_536)? as usize;
    let label = value_of(args, "--label").unwrap_or_else(|| "unlabelled".to_owned());

    let backend = match value_of(args, "--backend").as_deref() {
        None | Some("local") => BackendKind::Local,
        Some("object") => BackendKind::Object,
        Some("s3") => BackendKind::S3 {
            config: weftfs::s3::S3Config::from_env().map_err(|err| err.to_string())?,
        },
        Some(other) => return Err(format!("unknown --backend `{other}`")),
    };
    let options = Options {
        single_flight,
        cache_bytes,
        cache_entries,
        ..Options::default()
    };
    let store_started = Instant::now();
    let store =
        Store::open_with_options_and_backend(&store_path, OpenMode::ReadOnly, options, backend)
            .map_err(|err| err.to_string())?;
    let open_ms = store_started.elapsed().as_secs_f64() * 1000.0;

    let text = fs::read_to_string(&mounts_file)
        .map_err(|err| format!("{}: {err}", mounts_file.display()))?;
    let mountpoints: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect();
    if mountpoints.is_empty() {
        return Err("the mount list is empty".to_owned());
    }
    let views = mountpoints
        .iter()
        .map(|mountpoint| SharedViewSpec {
            mountpoint: mountpoint.clone(),
            snapshot: None,
            snapshot_state: None,
            context: None,
            capability: None,
        })
        .collect::<Vec<_>>();

    let mount_started = Instant::now();
    let mount = SharedMount::start(store, views, false, false, None)
        .map_err(|err| format!("shared mount: {err}"))?;
    let mount_ms = mount_started.elapsed().as_secs_f64() * 1000.0;
    let ready_ns = now_ns();
    fs::write(&ready_file, format!("{ready_ns}\n"))
        .map_err(|err| format!("{}: {err}", ready_file.display()))?;

    eprintln!(
        "fleet_coldstart server: label={label} views={} open_ms={:.3} mount_ms={:.3}",
        mount.view_count(),
        open_ms,
        mount_ms
    );

    // Wait for every replica to publish its completion marker.
    let deadline = Instant::now() + timeout;
    loop {
        let done = fs::read_dir(&done_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().ends_with(".done"))
                    .count()
            })
            .unwrap_or(0);
        if done >= expect {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {} s waiting for {expect} replicas (saw {done})",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let all_done_ns = now_ns();

    let store = mount.store();
    let layers = store.read_layer_stats();
    let backend = store.backend_stats();
    let stats = json!({
        "schema": "weft-fleet-coldstart-server-v1",
        "label": label,
        "views": mount.view_count(),
        "open_ms": round(open_ms),
        "mount_ms": round(mount_ms),
        "ready_ns": ready_ns,
        "all_replicas_done_ns": all_done_ns,
        "single_flight": {
            "enabled": layers.single_flight.enabled,
            "leaders": layers.single_flight.leaders,
            "waiters": layers.single_flight.waiters,
            "degraded": layers.single_flight.degraded,
            "cancellations": layers.single_flight.cancellations,
            "panics": layers.single_flight.panics,
            "max_observed_entries": layers.single_flight.max_observed_entries,
        },
        "cache": {
            "enabled": layers.cache.enabled,
            "max_bytes": layers.cache.max_bytes,
            "bytes": layers.cache.bytes,
            "entries": layers.cache.entries,
            "hits": layers.cache.hits,
            "misses": layers.cache.misses,
            "admissions": layers.cache.admissions,
            "evictions": layers.cache.evictions,
            "rejections": layers.cache.rejections,
            "bytes_served": layers.cache.bytes_served,
        },
        "decode_calls": layers.decode_calls,
        "decode_bytes": layers.decode_bytes,
        "backend": {
            "reads": backend.reads,
            "range_reads": backend.range_reads,
            "bytes_read": backend.bytes_read,
        },
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&stats_out, &stats)?;
    println!(
        "{}",
        serde_json::to_string(&stats).map_err(|err| err.to_string())?
    );
    // Dropping the mount unmounts every view.
    drop(mount);
    Ok(())
}

// ---------------------------------------------------------------------------
// Replica
// ---------------------------------------------------------------------------

fn replica(args: &[String]) -> Result<(), String> {
    let mount = PathBuf::from(required(args, "--mount")?);
    let dir = value_of(args, "--dir").unwrap_or_else(|| "model".to_owned());
    let go_file = PathBuf::from(required(args, "--go-file")?);
    let done_file = PathBuf::from(required(args, "--done-file")?);
    let result_out = PathBuf::from(required(args, "--result-out")?);
    let label = value_of(args, "--label").unwrap_or_else(|| "replica".to_owned());
    let start_ns = now_ns();

    // Wait for the court's start signal, then for the mount to expose the model.
    let wait_deadline = Instant::now() + Duration::from_secs(300);
    while !go_file.exists() {
        if Instant::now() >= wait_deadline {
            return Err(format!("timed out waiting for {}", go_file.display()));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let go_ns = now_ns();
    let model_dir = mount.join(&dir);
    let list_deadline = Instant::now() + Duration::from_secs(120);
    let entries = loop {
        match list_model(&model_dir) {
            Ok(entries) if !entries.is_empty() => break entries,
            _ if Instant::now() >= list_deadline => {
                return Err(format!(
                    "timed out waiting for {} to become readable",
                    model_dir.display()
                ))
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let mut first_tensor_ns = 0u64;
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    let mut overall = Sha256::new();
    for (index, (name, size)) in entries.iter().enumerate() {
        let path = model_dir.join(name);
        let mut file =
            fs::File::open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let mut hasher = Sha256::new();
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|err| format!("{}: {err}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            bytes = bytes.saturating_add(read as u64);
        }
        if index == 0 {
            first_tensor_ns = now_ns();
        }
        overall.update(name.as_bytes());
        overall.update(&hasher.finalize());
        let _ = size;
    }
    let done_ns = now_ns();
    let result = json!({
        "schema": "weft-fleet-coldstart-replica-v1",
        "label": label,
        "mount": mount.display().to_string(),
        "start_ns": start_ns,
        "go_ns": go_ns,
        "first_tensor_ns": first_tensor_ns,
        "done_ns": done_ns,
        "tensors": entries.len(),
        "bytes": bytes,
        "bytes_digest": hex::encode(overall.finalize()),
        "wait_ms": round((go_ns.saturating_sub(start_ns)) as f64 / 1_000_000.0),
        "list_ms": round((first_tensor_ns.saturating_sub(go_ns)) as f64 / 1_000_000.0),
        "time_to_first_tensor_ms": round((first_tensor_ns.saturating_sub(go_ns)) as f64 / 1_000_000.0),
        "total_ms": round((done_ns.saturating_sub(go_ns)) as f64 / 1_000_000.0),
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&result_out, &result)?;
    fs::write(&done_file, format!("{done_ns}\n"))
        .map_err(|err| format!("{}: {err}", done_file.display()))?;
    println!(
        "{}",
        serde_json::to_string(&result).map_err(|err| err.to_string())?
    );
    Ok(())
}

/// `(name, size)` of every regular file in `dir`, smallest first so the fleet
/// converges on cheap tensors before any large one.
fn list_model(dir: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let metadata = entry
            .metadata()
            .map_err(|err| format!("{}: {err}", entry.path().display()))?;
        if metadata.is_file() {
            entries.push((
                entry.file_name().to_string_lossy().into_owned(),
                metadata.len(),
            ));
        }
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    Ok(entries)
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}
