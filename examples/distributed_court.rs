//! Distributed correctness court harness (subphase R13).
//!
//! `bench/scripts/distributed_court.sh` starts this harness inside the bench
//! court with a verified MinIO release binary. The harness starts MinIO itself
//! (so it can stop and restart the server process mid-run), creates one bucket
//! per isolated store, and drives **real separate processes** through the
//! contract the reviewer asked for:
//!
//! | id | scenario |
//! |---|---|
//! | S1 | single-writer interlock, 20 iterations alternating who wins |
//! | S2 | atomic publication under a process fault at every exposed transition |
//! | S3 | idempotent retry after a crash |
//! | S4 | concurrent readers during commits (exact pinned bytes) |
//! | S5 | stale reader: no newer generation until it reopens |
//! | S6 | unreachable/stopped server fails closed with a transport error |
//! | S7 | `gc --apply` racing a snapshot reader, checkpoint, and provenance |
//! | S8 | a known content hash is not authority under an envelope |
//! | S9 | S2/S4/S5 repeated for `--blob-backend s3 --commit-backend local` |
//!
//! Every scenario records its exact command lines, process PIDs and exit
//! codes, observed generations, verification result, and a raw log path in
//! `bench/receipts/distributed-<stamp>.json`.
//!
//! Scope: one host, one Docker court, a single-node MinIO process with a tmpfs
//! data directory. Not a cloud, multi-node, fleet, physical-media, or
//! power-loss claim. Writers are exclusive by design; there is no multi-writer
//! protocol to test.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use weftfs::auth::Principal;
use weftfs::backend::{BackendKind, Key, StorageBackend};
use weftfs::evaluate::lookup_snapshot;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::s3::{S3Backend, S3Config};
use weftfs::store::{OpenMode, Options, Store};

const SCHEMA: &str = "weft-distributed-court-v1";
/// Fixed clock for every commit in the court, so a retried deterministic
/// operation can be compared byte-for-byte with an uninterrupted run.
const FIXED_TS: i64 = 1_700_000_000_000_000_000;
const CHUNK_SIZE: u64 = 4096;
const CHECKPOINT_INTERVAL: u64 = 256;
const LOCK_LEASE_SECS: u64 = 30;
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// How long a child may take to open a store and report ready.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

const ALPHA_OLD: &[u8] = b"alpha-old-payload-0000000000";
const BETA_OLD: &[u8] = b"beta-old-payload-000000000000";
const ALPHA_NEW: &[u8] = b"alpha-new-payload-1111111111";
const BETA_NEW: &[u8] = b"beta-new-payload-111111111111";
const PINNED: &[u8] = b"pinned-reader-payload-0000";
const KEEP: &[u8] = b"keep-payload-000000";
const INPUT: &[u8] = b"input-payload-00000";

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        let mode = args.get(1).cloned().unwrap_or_default();
        std::process::exit(match child_main(&mode, &args[2..]) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("child {mode}: {err}");
                70
            }
        });
    }
    if let Err(err) = run_parent(&args) {
        eprintln!("distributed_court: {err}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Store specification shared by the parent and every child process
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Layout {
    /// One S3 backend: blobs and commit state in the same bucket.
    SingleS3,
    /// `--blob-backend s3 --commit-backend local`: blobs in one bucket,
    /// commit/control state in a local directory.
    SplitS3BlobsLocalCommits,
}

impl Layout {
    fn is_split(self) -> bool {
        matches!(self, Layout::SplitS3BlobsLocalCommits)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoreSpec {
    layout: Layout,
    label: String,
    bucket: String,
    local_root: Option<PathBuf>,
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    chunk_size: u64,
    checkpoint_interval: u64,
    fixed_timestamp_ns: i64,
}

impl StoreSpec {
    fn config(&self) -> S3Config {
        S3Config {
            endpoint: self.endpoint.clone(),
            bucket: self.bucket.clone(),
            region: self.region.clone(),
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            session_token: None,
            path_style: true,
            lock_lease: Duration::from_secs(LOCK_LEASE_SECS),
            request_timeout: Duration::from_secs(REQUEST_TIMEOUT_SECS),
        }
    }

    fn kind(&self) -> weftfs::Result<BackendKind> {
        let s3 = BackendKind::S3 {
            config: self.config(),
        };
        match self.layout {
            Layout::SingleS3 => Ok(s3),
            Layout::SplitS3BlobsLocalCommits => {
                BackendKind::split_with_blob_root(s3, BackendKind::Local, None)
            }
        }
    }

    fn store_path(&self) -> PathBuf {
        match self.layout {
            Layout::SingleS3 => PathBuf::from("<s3>"),
            Layout::SplitS3BlobsLocalCommits => self
                .local_root
                .clone()
                .expect("a split spec names a local commit root"),
        }
    }

    fn options(&self) -> Options {
        Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size: self.chunk_size,
            checkpoint_interval: self.checkpoint_interval,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(self.fixed_timestamp_ns),
            ..Options::default()
        }
    }

    /// Exact CLI flags a child process needs to select this store.
    fn cli_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.layout.is_split() {
            out.push("--store".to_owned());
            out.push(self.store_path().display().to_string());
            out.push("--backend".to_owned());
            out.push("local".to_owned());
            out.push("--blob-backend".to_owned());
            out.push("s3".to_owned());
            out.push("--commit-backend".to_owned());
            out.push("local".to_owned());
        } else {
            out.push("--backend".to_owned());
            out.push("s3".to_owned());
        }
        out.extend([
            "--s3-endpoint".to_owned(),
            self.endpoint.clone(),
            "--s3-bucket".to_owned(),
            self.bucket.clone(),
            "--s3-region".to_owned(),
            self.region.clone(),
            "--s3-access-key".to_owned(),
            self.access_key.clone(),
            "--s3-secret-key".to_owned(),
            self.secret_key.clone(),
        ]);
        out
    }

    fn load(path: &Path) -> Result<StoreSpec, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read spec {}: {err}", path.display()))?;
        serde_json::from_str(&text).map_err(|err| format!("bad spec {}: {err}", path.display()))
    }

    fn ensure_bucket(&self) -> Result<(), String> {
        let backend = S3Backend::new("<distributed-court>", self.config());
        backend
            .ensure_bucket()
            .map_err(|err| format!("cannot create bucket {}: {err}", self.bucket))
    }

    fn open_typed(&self, mode: OpenMode) -> weftfs::Result<Store> {
        Store::open_with_options_and_backend(self.store_path(), mode, self.options(), self.kind()?)
    }

    fn open(&self, mode: OpenMode) -> Result<Store, String> {
        self.open_typed(mode)
            .map_err(|err| format!("open {}: {err}", self.label))
    }

    fn init(&self) -> Result<Store, String> {
        if let Some(root) = &self.local_root {
            std::fs::create_dir_all(root)
                .map_err(|err| format!("cannot create {}: {err}", root.display()))?;
        }
        Store::init_with_backend(
            self.store_path(),
            self.options(),
            self.kind().map_err(|err| err.to_string())?,
        )
        .map_err(|err| format!("init {}: {err}", self.label))
    }
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm (no time crate needed).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn sh_quote(argv: &[String]) -> String {
    let mut out = String::new();
    for (index, arg) in argv.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        if arg.is_empty()
            || arg
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || b"-_.=/:,<>@".contains(&byte)))
        {
            out.push('\'');
            out.push_str(&arg.replace('\'', "'\\''"));
            out.push('\'');
        } else {
            out.push_str(arg);
        }
    }
    out
}

fn exit_label(status: Option<ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status {
        None => "timeout".to_owned(),
        Some(status) => match (status.code(), status.signal()) {
            (Some(code), _) => format!("exit {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "unknown".to_owned(),
        },
    }
}

fn tail(text: &str, lines: usize, limit: usize) -> String {
    let collected: Vec<&str> = text.lines().rev().take(lines).collect();
    let mut out = collected
        .into_iter()
        .rev()
        .collect::<Vec<&str>>()
        .join("\n");
    if out.len() > limit {
        out.truncate(limit);
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------------------
// Scenario log + process records
// ---------------------------------------------------------------------------

struct ScenarioLog {
    path: PathBuf,
    file: BufWriter<File>,
    lines: usize,
}

impl ScenarioLog {
    fn create(path: PathBuf) -> Result<ScenarioLog, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let file = File::create(&path)
            .map_err(|err| format!("cannot create {}: {err}", path.display()))?;
        Ok(ScenarioLog {
            path,
            file: BufWriter::new(file),
            lines: 0,
        })
    }

    fn line(&mut self, text: &str) {
        let _ = writeln!(self.file, "{}", text);
        self.lines += 1;
    }

    fn record(&mut self, record: &ChildRecord) {
        self.line(&format!(
            "  process {} pid={} exit={} argv={}",
            record.label,
            record.pid,
            record.exit,
            sh_quote(&record.argv)
        ));
        if !record.stdout_tail.is_empty() {
            for line in record.stdout_tail.lines() {
                self.line(&format!("    out: {line}"));
            }
        }
        if !record.stderr_tail.is_empty() {
            for line in record.stderr_tail.lines() {
                self.line(&format!("    err: {line}"));
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ChildRecord {
    label: String,
    argv: Vec<String>,
    pid: u32,
    exit: String,
    stdout_tail: String,
    stderr_tail: String,
    timed_out: bool,
}

impl ChildRecord {
    fn json(&self) -> Value {
        json!({
            "label": self.label,
            "pid": self.pid,
            "argv": self.argv,
            "command": sh_quote(&self.argv),
            "exit": self.exit,
            "timed_out": self.timed_out,
            "stdout_tail": self.stdout_tail,
            "stderr_tail": self.stderr_tail,
        })
    }

    fn succeeded(&self) -> bool {
        self.exit == "exit 0"
    }
}

struct ChildHandle {
    child: Child,
    argv: Vec<String>,
    pid: u32,
    out_path: PathBuf,
    err_path: PathBuf,
    label: String,
}

fn spawn_child(
    exe: &Path,
    args: &[String],
    envs: &[(&str, &str)],
    dir: &Path,
    label: &str,
) -> Result<ChildHandle, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let out_path = dir.join(format!("{label}.out"));
    let err_path = dir.join(format!("{label}.err"));
    let stdout = File::create(&out_path)
        .map_err(|err| format!("cannot create {}: {err}", out_path.display()))?;
    let stderr = File::create(&err_path)
        .map_err(|err| format!("cannot create {}: {err}", err_path.display()))?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(exe.display().to_string());
    argv.extend(args.iter().cloned());
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    command.env_remove("WEFT_CRASH_PHASE");
    for (name, value) in envs {
        command.env(name, value);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("cannot spawn {}: {err}", label))?;
    Ok(ChildHandle {
        pid: child.id(),
        child,
        argv,
        out_path,
        err_path,
        label: label.to_owned(),
    })
}

impl ChildHandle {
    fn status_file(&self) -> PathBuf {
        self.out_path.with_extension("status.json")
    }

    fn ready_file(&self) -> PathBuf {
        self.out_path.with_extension("ready.json")
    }

    fn gate_file(&self) -> PathBuf {
        self.out_path.with_extension("gate")
    }

    /// Wait for the child to finish, hard-killing it on timeout. Reads the
    /// captured output and records the result.
    fn finish(mut self, timeout: Duration) -> Result<(ChildRecord, Option<Value>), String> {
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            match self
                .child
                .try_wait()
                .map_err(|err| format!("wait {}: {err}", self.label))?
            {
                Some(status) => break Some(status),
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    timed_out = true;
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let stdout = std::fs::read_to_string(&self.out_path).unwrap_or_default();
        let stderr = std::fs::read_to_string(&self.err_path).unwrap_or_default();
        let status_value = read_json_file(&self.status_file());
        let record = ChildRecord {
            label: self.label,
            argv: self.argv,
            pid: self.pid,
            exit: exit_label(status),
            stdout_tail: tail(&stdout, 6, 800),
            stderr_tail: tail(&stderr, 6, 800),
            timed_out,
        };
        Ok((record, status_value))
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{err}"))?;
    }
    std::fs::write(
        path,
        serde_json::to_string(value).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

fn wait_for_json(path: &Path, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = read_json_file(path) {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// MinIO process control
// ---------------------------------------------------------------------------

struct Minio {
    binary: PathBuf,
    data_dir: PathBuf,
    port: u16,
    console_port: u16,
    access_key: String,
    secret_key: String,
    log_dir: PathBuf,
    child: Option<Child>,
    pid: u32,
    starts: u32,
    stops: u32,
}

impl Minio {
    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn start(&mut self) -> Result<(), String> {
        if self.child.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|err| format!("cannot create {}: {err}", self.data_dir.display()))?;
        std::fs::create_dir_all(&self.log_dir)
            .map_err(|err| format!("cannot create {}: {err}", self.log_dir.display()))?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join("minio.log"))
            .map_err(|err| format!("cannot open minio log: {err}"))?;
        let log_err = log
            .try_clone()
            .map_err(|err| format!("cannot clone minio log: {err}"))?;
        let child = Command::new(&self.binary)
            .args([
                "server",
                self.data_dir.to_str().unwrap_or_default(),
                "--address",
                &format!("127.0.0.1:{}", self.port),
                "--console-address",
                &format!("127.0.0.1:{}", self.console_port),
            ])
            .env("MINIO_ROOT_USER", &self.access_key)
            .env("MINIO_ROOT_PASSWORD", &self.secret_key)
            .env("MINIO_BROWSER", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|err| format!("cannot start MinIO: {err}"))?;
        self.pid = child.id();
        self.child = Some(child);
        self.starts += 1;
        self.wait_ready()
    }

    fn wait_ready(&self) -> Result<(), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .timeout_connect(Duration::from_secs(2))
            .build();
        let url = format!("http://127.0.0.1:{}/minio/health/ready", self.port);
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Ok(response) = agent.get(&url).call() {
                if response.status() == 200 {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "MinIO did not become ready on port {}; see {}",
                    self.port,
                    self.log_dir.join("minio.log").display()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop the server process. The data directory stays on the court's tmpfs,
    /// so a later start serves the same bucket state.
    fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.stops += 1;
        }
        // Wait until the port is actually refusing connections.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match std::net::TcpStream::connect(("127.0.0.1", self.port)) {
                Err(_) => return Ok(()),
                Ok(_) if Instant::now() >= deadline => {
                    return Err("MinIO port still accepts connections after stop".to_owned())
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Parent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Args {
    out: PathBuf,
    root: PathBuf,
    stamp: String,
    weftfs_bin: PathBuf,
    minio_bin: PathBuf,
    tests_summary: Option<PathBuf>,
}

fn parse_parent_args(args: &[String]) -> Result<Args, String> {
    let mut out = None;
    let mut root = None;
    let mut stamp = None;
    let mut weftfs_bin = None;
    let mut minio_bin = None;
    let mut tests_summary = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--root" => root = Some(PathBuf::from(value()?)),
            "--stamp" => stamp = Some(value()?),
            "--weftfs-bin" => weftfs_bin = Some(PathBuf::from(value()?)),
            "--minio-bin" => minio_bin = Some(PathBuf::from(value()?)),
            "--tests-summary" => tests_summary = Some(PathBuf::from(value()?)),
            "-h" | "--help" => {
                println!(
                    "usage: distributed_court --out RECEIPT --weftfs-bin PATH --minio-bin PATH \
                     [--root DIR] [--stamp STAMP] [--tests-summary FILE]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args {
        out: out.ok_or("--out is required")?,
        root: root.unwrap_or_else(|| PathBuf::from("/tmp/weft-distributed-court")),
        stamp: stamp.unwrap_or_else(utc_stamp),
        weftfs_bin: weftfs_bin.ok_or("--weftfs-bin is required")?,
        minio_bin: minio_bin.ok_or("--minio-bin is required")?,
        tests_summary,
    })
}

fn run_parent(args: &[String]) -> Result<(), String> {
    let args = parse_parent_args(args)?;
    let started = Instant::now();
    std::fs::create_dir_all(&args.root)
        .map_err(|err| format!("cannot create {}: {err}", args.root.display()))?;
    let run_dir = args.root.join(format!("run-{}", std::process::id()));
    std::fs::create_dir_all(&run_dir)
        .map_err(|err| format!("cannot create {}: {err}", run_dir.display()))?;
    let log_dir = args
        .out
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("bench/receipts"));
    std::fs::create_dir_all(&log_dir).map_err(|err| format!("cannot create logs: {err}"))?;

    let minio_bin = args.minio_bin.clone();
    if !minio_bin.exists() {
        return Err(format!(
            "MinIO binary {} does not exist; scripts/s3_court.sh downloads and verifies it",
            minio_bin.display()
        ));
    }
    let minio_sha256 =
        std::env::var("WEFT_S3_SERVER_SHA256").unwrap_or_else(|_| "unknown".to_owned());
    let minio_release =
        std::env::var("WEFT_S3_SERVER_RELEASE").unwrap_or_else(|_| "unknown".to_owned());
    let minio_version =
        std::env::var("WEFT_S3_SERVER_LABEL").unwrap_or_else(|_| "unknown".to_owned());

    let access_key = "weftdist".to_owned();
    let secret_key = "weftdist-secret".to_owned();
    let port = free_port()?;
    let console_port = free_port()?;
    let mut minio = Minio {
        binary: minio_bin.clone(),
        data_dir: run_dir.join("minio-data"),
        port,
        console_port,
        access_key: access_key.clone(),
        secret_key: secret_key.clone(),
        log_dir: run_dir.join("logs"),
        child: None,
        pid: 0,
        starts: 0,
        stops: 0,
    };
    minio.start()?;

    let mut ctx = Court {
        args: args.clone(),
        run_dir: run_dir.clone(),
        log_dir: log_dir.clone(),
        minio,
        access_key,
        secret_key,
        buckets: 0,
    };

    let mut scenarios = Vec::new();
    for scenario in [
        run_s1_interlock,
        run_s2_crash_publication,
        run_s3_retry,
        run_s4_concurrent_readers,
        run_s5_stale_reader,
        run_s6_unreachable,
        run_s7_gc_race,
        run_s8_content_hash,
        run_s9_split,
    ] {
        let result = scenario(&mut ctx);
        match result {
            Ok(value) => scenarios.push(value),
            Err(err) => {
                // A scenario failure is recorded, not silently dropped.
                let scenario_id = scenarios.len();
                eprintln!("scenario {} failed: {err}", scenario_id + 1);
                return Err(format!("scenario {} failed: {err}", scenario_id + 1));
            }
        }
    }

    let minio_info = json!({
        "release": minio_release,
        "version_line": minio_version,
        "binary": minio_bin.display().to_string(),
        "binary_sha256": minio_sha256,
        "endpoint": ctx.minio.endpoint(),
        "pid": ctx.minio.pid,
        "starts": ctx.minio.starts,
        "stops": ctx.minio.stops,
        "data_dir": ctx.minio.data_dir.display().to_string(),
        "data_fs": "container tmpfs (/tmp)",
    });
    let _ = ctx.minio.stop();

    let checks_passed = scenarios
        .iter()
        .map(|scenario| scenario["checks_passed"].as_u64().unwrap_or(0))
        .sum::<u64>();
    let checks_failed = scenarios
        .iter()
        .map(|scenario| scenario["checks_failed"].as_u64().unwrap_or(0))
        .sum::<u64>();
    let scenarios_passed = scenarios
        .iter()
        .filter(|scenario| scenario["passed"].as_bool().unwrap_or(false))
        .count();
    let processes_recorded = scenarios
        .iter()
        .map(|scenario| scenario["processes"].as_array().map(Vec::len).unwrap_or(0))
        .sum::<usize>();
    let tests = args.tests_summary.as_ref().map(|path| {
        json!({
            "summary_file": path.display().to_string(),
            "summary": std::fs::read_to_string(path).unwrap_or_default(),
        })
    });
    let receipt = json!({
        "schema": SCHEMA,
        "generated_utc": args.stamp,
        "label": "Single-host distributed correctness court: a real MinIO process on loopback in the OOM-protected bench court, separate writer/reader/GC processes, local tmpfs data. Not a cloud, multi-node, fleet, physical-media, power-loss, or production claim. Writers are exclusive by design.",
        "environment": {
            "court_image": env_or("WEFT_COURT_IMAGE"),
            "court_image_id": env_or("WEFT_COURT_IMAGE_ID"),
            "kernel": env_or("WEFT_COURT_KERNEL"),
            "docker_server": env_or("WEFT_DOCKER_SERVER"),
            "memory": env_or("WEFT_COURT_MEMORY"),
            "cpus": env_or("WEFT_COURT_CPUS"),
            "pids": env_or("WEFT_COURT_PIDS"),
            "nproc": env_or("WEFT_COURT_NPROC"),
            "revisions": {
                "WeftFS": env_or("WEFT_REPO_WEFTFS"),
                "WeftCodec": env_or("WEFT_REPO_WEFTCODEC"),
                "WeftCodec-Go": env_or("WEFT_REPO_WEFTCODEC_GO"),
                "WeftFS-Go": env_or("WEFT_REPO_WEFTFS_GO"),
            },
            "weftfs_binary": args.weftfs_bin.display().to_string(),
            "harness": std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            "harness_peak_rss_kib": peak_rss_kib(),
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "work_root": run_dir.display().to_string(),
        },
        "minio": minio_info,
        "workload": {
            "chunk_size": CHUNK_SIZE,
            "checkpoint_interval": CHECKPOINT_INTERVAL,
            "fixed_timestamp_ns": FIXED_TS,
            "writers": "one at a time by design; the interlock is the contract",
            "buckets_created": ctx.buckets,
        },
        "scenarios": scenarios,
        "summary": {
            "scenarios_total": 9,
            "scenarios_passed": scenarios_passed,
            "scenarios_failed": 9 - scenarios_passed,
            "checks_passed": checks_passed,
            "checks_failed": checks_failed,
            "processes_recorded": processes_recorded,
            "result": if checks_failed == 0 && scenarios_passed == 9 { "pass" } else { "fail" },
        },
        "tests": tests,
        "residual": [
            "Single-node MinIO on loopback with a tmpfs data directory; not multi-node and not a cloud provider.",
            "No TCP-level network partition beyond stopping the server process and an endpoint with no listener; there is no packet filter, delay, or split-brain test.",
            "A SIGKILL of a writer is a process fault, not physical media or host-cache loss.",
            "Writers are exclusive by design (one writer at a time); no multi-writer protocol is claimed or tested.",
            "The `--blob-backend s3 --commit-backend local` layout is Rust-only; there is no Go parity in this court.",
            "S3 durability means the MinIO process acknowledged the request; there is no fsync.",
        ],
    });
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    std::fs::write(
        &args.out,
        serde_json::to_string_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("cannot write {}: {err}", args.out.display()))?;
    println!(
        "distributed_court: {}/9 scenarios, {} checks passed, {} failed",
        scenarios_passed, checks_passed, checks_failed
    );
    println!("distributed_court: receipt {}", args.out.display());
    if checks_failed != 0 || scenarios_passed != 9 {
        return Err("the distributed court did not pass".to_owned());
    }
    Ok(())
}

fn env_or(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_owned())
}

fn free_port() -> Result<u16, String> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| format!("bind: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("local_addr: {err}"))?
        .port();
    drop(listener);
    Ok(port)
}

struct Court {
    args: Args,
    run_dir: PathBuf,
    log_dir: PathBuf,
    minio: Minio,
    access_key: String,
    secret_key: String,
    buckets: u64,
}

impl Court {
    /// Write a store spec for a child and return its path.
    fn write_spec(&mut self, layout: Layout, label: &str) -> Result<(PathBuf, StoreSpec), String> {
        self.buckets += 1;
        let bucket = format!(
            "dist-{}-{}-{}",
            std::process::id(),
            self.buckets,
            BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let local_root = layout
            .is_split()
            .then(|| self.run_dir.join("stores").join(label).join("commit"));
        let spec = StoreSpec {
            layout,
            label: label.to_owned(),
            bucket,
            local_root,
            endpoint: self.minio.endpoint(),
            region: "us-east-1".to_owned(),
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            chunk_size: CHUNK_SIZE,
            checkpoint_interval: CHECKPOINT_INTERVAL,
            fixed_timestamp_ns: FIXED_TS,
        };
        spec.ensure_bucket()?;
        let path = self.run_dir.join("specs").join(format!("{label}.json"));
        write_json_file(
            &path,
            &serde_json::to_value(&spec).map_err(|err| err.to_string())?,
        )?;
        Ok((path, spec))
    }

    /// Write a spec without creating its bucket (used for endpoints that are
    /// deliberately unreachable).
    fn write_spec_unroutable(
        &mut self,
        layout: Layout,
        label: &str,
        endpoint: String,
    ) -> Result<(PathBuf, StoreSpec), String> {
        self.buckets += 1;
        let bucket = format!(
            "dist-unroutable-{}-{}",
            std::process::id(),
            BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let local_root = layout
            .is_split()
            .then(|| self.run_dir.join("stores").join(label).join("commit"));
        let spec = StoreSpec {
            layout,
            label: label.to_owned(),
            bucket,
            local_root,
            endpoint,
            region: "us-east-1".to_owned(),
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            chunk_size: CHUNK_SIZE,
            checkpoint_interval: CHECKPOINT_INTERVAL,
            fixed_timestamp_ns: FIXED_TS,
        };
        let path = self.run_dir.join("specs").join(format!("{label}.json"));
        write_json_file(
            &path,
            &serde_json::to_value(&spec).map_err(|err| err.to_string())?,
        )?;
        Ok((path, spec))
    }

    fn log(&self, scenario: u32) -> Result<ScenarioLog, String> {
        ScenarioLog::create(
            self.log_dir
                .join(format!("distributed-{}.s{scenario}.log", self.args.stamp)),
        )
    }

    /// Spawn a child mode with the usual spec/ready/status/gate environment.
    fn spawn_mode(
        &self,
        dir: &Path,
        label: &str,
        mode: &str,
        spec_path: &Path,
        extra: &[(&str, &str)],
    ) -> Result<ChildHandle, String> {
        let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
        let ready = dir.join(format!("{label}.ready.json"));
        let status = dir.join(format!("{label}.status.json"));
        let gate = dir.join(format!("{label}.gate"));
        // Never read a result left by an earlier child with the same label.
        let _ = std::fs::remove_file(&ready);
        let _ = std::fs::remove_file(&status);
        let _ = std::fs::remove_file(&gate);
        let mut envs = vec![
            ("WEFT_DIST_SPEC".to_owned(), spec_path.display().to_string()),
            (
                "WEFT_DIST_READY_FILE".to_owned(),
                ready.display().to_string(),
            ),
            (
                "WEFT_DIST_STATUS_FILE".to_owned(),
                status.display().to_string(),
            ),
            ("WEFT_DIST_GATE".to_owned(), gate.display().to_string()),
        ];
        for (name, value) in extra {
            envs.push(((*name).to_owned(), (*value).to_owned()));
        }
        let refs: Vec<(&str, &str)> = envs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        spawn_child(
            &exe,
            &["--child".to_owned(), mode.to_owned()],
            &refs,
            dir,
            label,
        )
    }

    fn run_cli(
        &self,
        dir: &Path,
        label: &str,
        spec: &StoreSpec,
        args: &[String],
        timeout: Duration,
    ) -> Result<ChildRecord, String> {
        let exe = self.args.weftfs_bin.clone();
        let mut argv = spec.cli_args();
        argv.extend(args.iter().cloned());
        let handle = spawn_child(&exe, &argv, &[], dir, label)?;
        let (record, _) = handle.finish(timeout)?;
        Ok(record)
    }
}

/// Accumulates one scenario's checks, processes, generations, and verification.
struct CheckSet {
    log: ScenarioLog,
    id: &'static str,
    name: &'static str,
    checks: Vec<Value>,
    passed: u64,
    failed: u64,
    processes: Vec<Value>,
    generations: Vec<u64>,
    verification: Option<Value>,
    commands: Vec<String>,
    details: BTreeMap<String, Value>,
}

impl CheckSet {
    fn new(
        root: &Court,
        scenario: u32,
        id: &'static str,
        name: &'static str,
    ) -> Result<CheckSet, String> {
        Ok(CheckSet {
            log: root.log(scenario)?,
            id,
            name,
            checks: Vec::new(),
            passed: 0,
            failed: 0,
            processes: Vec::new(),
            generations: Vec::new(),
            verification: None,
            commands: Vec::new(),
            details: BTreeMap::new(),
        })
    }

    fn line(&mut self, text: &str) {
        self.log.line(text);
    }

    /// Fold another scenario's checks/processes/generations into this one, so
    /// the top-level counts include a sub-court's evidence.
    fn absorb(&mut self, value: &Value) {
        for check in value["checks"].as_array().cloned().unwrap_or_default() {
            if check["passed"] == json!(true) {
                self.passed += 1;
            } else {
                self.failed += 1;
            }
            self.checks.push(check);
        }
        for process in value["processes"].as_array().cloned().unwrap_or_default() {
            self.processes.push(process);
        }
        for generation in value["generation_sequence"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(generation) = generation.as_u64() {
                self.generations.push(generation);
            }
        }
    }

    fn check(&mut self, name: &str, condition: bool, detail: impl Into<String>) {
        let detail = detail.into();
        let ok = condition;
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        self.log.line(&format!(
            "[{}] {} {}: {}",
            self.id,
            if ok { "PASS" } else { "FAIL" },
            name,
            detail
        ));
        self.checks.push(json!({
            "name": name,
            "passed": ok,
            "detail": detail,
        }));
    }

    fn process(&mut self, record: &ChildRecord) {
        self.log.record(record);
        self.processes.push(record.json());
    }

    fn generation(&mut self, generation: u64) {
        self.generations.push(generation);
    }

    fn command(&mut self, command: &str) {
        self.commands.push(command.to_owned());
    }

    fn set(&mut self, key: &str, value: Value) {
        self.details.insert(key.to_owned(), value);
    }

    fn set_verification(&mut self, value: Value) {
        self.verification = Some(value);
    }

    fn finish(self) -> Value {
        let log_path = self
            .log
            .path
            .strip_prefix(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| self.log.path.clone());
        json!({
            "id": self.id,
            "name": self.name,
            "passed": self.failed == 0,
            "checks_passed": self.passed,
            "checks_failed": self.failed,
            "checks": self.checks,
            "processes": self.processes,
            "generation_sequence": self.generations,
            "verification": self.verification,
            "commands": self.commands,
            "details": self.details,
            "log": log_path.display().to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Shared parent operations
// ---------------------------------------------------------------------------

fn open_store(spec: &StoreSpec, mode: OpenMode) -> Result<Store, String> {
    spec.open(mode)
}

/// Replace a possibly stale writer lease with a released tombstone under a
/// conditional write (the court reset).
///
/// In production a crashed writer's `LOCK` object becomes reclaimable when its
/// lease expires, and the court could simply wait. It clears the lease
/// explicitly so a retry does not spend the lease window waiting; the lease
/// itself is still the production mechanism and is never disabled. The reset
/// keeps the fencing token monotonic, so a stale writer from a previous
/// iteration can never reuse the token of the iteration that follows.
fn clear_stale_lock(spec: &StoreSpec) -> Result<(), String> {
    let backend = S3Backend::new("<distributed-court>", spec.config());
    match backend.read_lock_record() {
        Ok(None) => Ok(()),
        Ok(Some(_)) => backend
            .release_stale_lease()
            .map(|_| ())
            .map_err(|err| format!("cannot release the stale S3 lease: {err}")),
        Err(err) => Err(format!("cannot read the S3 lease: {err}")),
    }
}

/// Store identity for the receipt: layout, bucket, local commit root, and the
/// loopback endpoint. Credentials are never written to a receipt.
fn spec_detail(spec: &StoreSpec) -> Value {
    json!({
        "layout": match spec.layout {
            Layout::SingleS3 => "s3",
            Layout::SplitS3BlobsLocalCommits => "s3-blobs-local-commits",
        },
        "bucket": spec.bucket.clone(),
        "local_commit_root": spec.local_root.as_ref().map(|path| path.display().to_string()),
        "endpoint": spec.endpoint.clone(),
    })
}

fn verify_spec(spec: &StoreSpec, label: &str) -> Result<Value, String> {
    let store = open_store(spec, OpenMode::ReadOnly)?;
    let report = store
        .verify(true)
        .map_err(|err| format!("verify {label}: {err}"))?;
    Ok(json!({
        "ok": report.ok,
        "generation": report.generation,
        "commit": report.commit.map(|hash| hash.to_hex()),
        "checked_commits": report.checked_commits,
        "checked_objects": report.checked_objects,
        "checked_manifests": report.checked_manifests,
        "checked_chunks": report.checked_chunks,
        "problems": report.problems.iter().map(|problem| json!({
            "class": problem.class,
            "detail": problem.detail,
        })).collect::<Vec<Value>>(),
    }))
}

fn read_path(store: &Store, path: &str) -> Result<(Hash32, Vec<u8>), String> {
    let id = store
        .resolve_path(path)
        .map_err(|err| format!("resolve {path}: {err}"))?;
    let bytes = store
        .read_object(id)
        .map_err(|err| format!("read {path}: {err}"))?;
    Ok((id, bytes))
}

fn create_file(
    spec: &StoreSpec,
    store: &mut Store,
    name: &[u8],
    bytes: &[u8],
    seed: &[u8],
) -> Result<Hash32, String> {
    let _ = spec;
    let root = store
        .resolve_path("/")
        .map_err(|err| format!("resolve root: {err}"))?;
    store
        .create_file_seeded(root, name, bytes, 0o644, 0, 0, BTreeMap::new(), seed)
        .map_err(|err| format!("create {}: {err}", String::from_utf8_lossy(name)))
}

fn write_path(store: &mut Store, path: &str, bytes: &[u8]) -> Result<Hash32, String> {
    let id = store
        .resolve_path(path)
        .map_err(|err| format!("resolve {path}: {err}"))?;
    store
        .write_file(id, 0, bytes)
        .map_err(|err| format!("write {path}: {err}"))
}

// ---------------------------------------------------------------------------
// S1 — single-writer interlock
// ---------------------------------------------------------------------------

fn run_s1_interlock(court: &mut Court) -> Result<Value, String> {
    let mut set = CheckSet::new(court, 1, "S1", "single-writer interlock (20 iterations)")?;
    let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s1")?;
    set.set("store", spec_detail(&spec));
    {
        let mut store = spec.init()?;
        create_file(&spec, &mut store, b"f", b"interlock-bytes", b"s1-f")?;
    }
    let dir = court.run_dir.join("s1");
    let mut ready_count = 0u64;
    let mut busy_count = 0u64;
    let mut reopen_generations = Vec::new();
    let mut busy_texts = Vec::new();

    for iteration in 0..20u32 {
        let (first_label, second_label) = if iteration % 2 == 0 {
            ("first-a", "first-b")
        } else {
            ("first-b", "first-a")
        };
        let first = court.spawn_mode(&dir, first_label, "hold-writer", &spec_path, &[])?;
        let ready = wait_for_json(&first.ready_file(), READY_TIMEOUT)
            .map_err(|err| format!("iteration {iteration} first writer: {err}"))?;
        ready_count += 1;
        set.line(&format!(
            "[S1] iteration={iteration} first={first_label} pid={} generation={}",
            first.pid, ready["generation"]
        ));
        let first_gen = ready["generation"].as_u64().unwrap_or(0);
        set.generation(first_gen);

        let second = court.spawn_mode(&dir, second_label, "hold-writer", &spec_path, &[])?;
        let second_pid = second.pid;
        let (second_record, second_status) = second.finish(Duration::from_secs(30))?;
        set.process(&second_record);
        let exit = second_status.clone().unwrap_or_else(|| json!({}));
        let class = exit["class"].as_str().unwrap_or("").to_owned();
        let message = exit["message"].as_str().unwrap_or("").to_owned();
        if class == "Busy" {
            busy_count += 1;
            busy_texts.push(message);
        }
        set.check(
            &format!("iteration {iteration} second writer refused Busy"),
            exit["ok"] == json!(false) && class == "Busy" && !second_record.succeeded(),
            format!(
                "label={second_label} pid={second_pid} exit={} class={class}",
                second_record.exit
            ),
        );

        // Release the first writer, then prove the store still opens.
        std::fs::write(first.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
        let (first_record, first_status) = first.finish(Duration::from_secs(30))?;
        set.process(&first_record);
        let first_ok = first_status
            .as_ref()
            .map(|value| value["ok"] == json!(true))
            .unwrap_or(false);
        set.check(
            &format!("iteration {iteration} first writer exited cleanly"),
            first_record.succeeded() && first_ok,
            format!("pid={} exit={}", first_record.pid, first_record.exit),
        );

        let third = court.spawn_mode(
            &dir,
            "after-exit",
            "hold-writer",
            &spec_path,
            &[("WEFT_DIST_HOLD_MS", "0")],
        )?;
        let (third_record, third_status) = third.finish(Duration::from_secs(30))?;
        set.process(&third_record);
        let third_ok = third_status
            .as_ref()
            .map(|value| value["ok"] == json!(true))
            .unwrap_or(false);
        let third_gen = third_status
            .as_ref()
            .and_then(|value| value["generation"].as_u64())
            .unwrap_or(0);
        reopen_generations.push(third_gen);
        set.check(
            &format!("iteration {iteration} store reopens after the writer exits"),
            third_record.succeeded() && third_ok && third_gen == first_gen,
            format!(
                "pid={} exit={} generation={third_gen}",
                third_record.pid, third_record.exit
            ),
        );
    }

    let verification = verify_spec(&spec, "s1")?;
    set.set_verification(verification.clone());
    let (id, bytes) = {
        let store = open_store(&spec, OpenMode::ReadOnly)?;
        read_path(&store, "/f")?
    };
    set.check(
        "store intact after 20 alternating interlocked opens",
        verification["ok"] == json!(true) && bytes == b"interlock-bytes",
        format!(
            "object={} bytes={} verify_ok={}",
            id.to_hex(),
            bytes.len(),
            verification["ok"]
        ),
    );
    set.check(
        "every iteration observed exactly one Busy",
        busy_count == 20 && ready_count == 20,
        format!("busy={busy_count} ready={ready_count}"),
    );
    set.check(
        "Busy text is the same non-disclosing sentence",
        busy_texts.iter().all(|text| text == &busy_texts[0]),
        format!("distinct_messages={}", {
            let mut unique = busy_texts.clone();
            unique.sort();
            unique.dedup();
            unique.len()
        }),
    );
    set.check(
        "alternation produced no divergence in the reopened generation",
        reopen_generations
            .iter()
            .all(|generation| *generation == reopen_generations[0]),
        format!("generations={reopen_generations:?}"),
    );
    set.set("busy_count", json!(busy_count));
    set.set("ready_count", json!(ready_count));
    set.command(&sh_quote(&[
        court.args.weftfs_bin.display().to_string(),
        "--child".to_owned(),
        "hold-writer".to_owned(),
    ]));
    let value = set.finish();
    Ok(value)
}

// ---------------------------------------------------------------------------
// S2 / S9a — atomic publication under a process fault
// ---------------------------------------------------------------------------

fn crash_publication(
    court: &mut Court,
    log_scenario: u32,
    id: &'static str,
    name: &'static str,
    layout: Layout,
    iterations: usize,
) -> Result<Value, String> {
    let mut set = CheckSet::new(court, log_scenario, id, name)?;
    let phases = [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ];
    let mut runs = 0usize;
    let mut heads = Vec::new();
    let mut buckets = Vec::new();
    let mut old_count = 0usize;
    let mut new_count = 0usize;
    let mut partial_count = 0usize;
    let mut tail_reported = 0usize;
    let mut aborted = 0usize;
    let mut wrong_class = Vec::new();

    for (phase_index, phase) in phases.iter().enumerate() {
        for iteration in 0..iterations {
            runs += 1;
            let label = format!("{id}-{phase_index}-{iteration}");
            let (spec_path, spec) = court.write_spec(layout, &label)?;
            buckets.push(spec.bucket.clone());
            let (generation, head) = {
                let mut store = spec.init()?;
                create_file(&spec, &mut store, b"alpha", ALPHA_OLD, b"dist-alpha")?;
                create_file(&spec, &mut store, b"beta", BETA_OLD, b"dist-beta")?;
                (store.generation(), store.snapshot_root().to_hex())
            };
            heads.push(head.clone());
            set.generation(generation);
            let dir = court.run_dir.join(&label);
            let child = court.spawn_mode(
                &dir,
                "writer",
                "group-overwrite",
                &spec_path,
                &[("WEFT_CRASH_PHASE", phase)],
            )?;
            let pid = child.pid;
            let (record, _status) = child.finish(Duration::from_secs(60))?;
            set.line(&format!(
                "[{id}] phase={phase} iteration={iteration} bucket={} generation={generation} writer_pid={pid} exit={}",
                spec.bucket, record.exit
            ));
            set.command(&format!(
                "{}  (WEFT_CRASH_PHASE={phase})",
                sh_quote(&record.argv)
            ));
            if record.exit.starts_with("signal") {
                aborted += 1;
            } else {
                wrong_class.push(format!(
                    "phase={phase} iteration={iteration} exit={}",
                    record.exit
                ));
            }
            set.process(&record);

            let reopened = open_store(&spec, OpenMode::ReadOnly)?;
            let observed_generation = reopened.generation();
            set.generation(observed_generation);
            let alpha = read_path(&reopened, "/alpha")?.1;
            let beta = read_path(&reopened, "/beta")?.1;
            let report = reopened
                .verify(true)
                .map_err(|err| format!("verify after crash: {err}"))?;
            let both_old = alpha == ALPHA_OLD && beta == BETA_OLD;
            let both_new = alpha == ALPHA_NEW && beta == BETA_NEW;
            if both_old {
                old_count += 1;
            } else if both_new {
                new_count += 1;
            } else {
                partial_count += 1;
            }
            let expected_generation = if *phase == "after_superblock_write" {
                generation + 1
            } else {
                generation
            };
            let mut tail_problem = false;
            if !report.ok {
                for problem in &report.problems {
                    if problem
                        .detail
                        .contains("exists above the highest published superblock generation")
                    {
                        tail_problem = true;
                        tail_reported += 1;
                    }
                }
            }
            let tail_ok = if *phase == "after_commit_file" {
                tail_problem && report.problems.len() == 1
            } else {
                report.ok
            };
            set.check(
                &format!("{phase} iteration {iteration} old/new whole state"),
                (both_old || both_new) && observed_generation == expected_generation,
                format!(
                    "old={both_old} new={both_new} generation={observed_generation} expected={expected_generation}"
                ),
            );
            set.check(
                &format!("{phase} iteration {iteration} verification"),
                tail_ok,
                format!(
                    "ok={} tail={tail_problem} problems={:?}",
                    report.ok,
                    report
                        .problems
                        .iter()
                        .map(|p| p.detail.clone())
                        .collect::<Vec<String>>()
                ),
            );
        }
    }

    set.check(
        "every fault produced a real process death",
        aborted == runs && wrong_class.is_empty(),
        format!("aborted={aborted} runs={runs} unexpected={wrong_class:?}"),
    );
    set.check(
        "no crash produced a mixed generation",
        partial_count == 0,
        format!("partial={partial_count} of {runs}"),
    );
    set.check(
        "every reopen yielded old or new whole state",
        old_count + new_count == runs,
        format!("old={old_count} new={new_count} runs={runs}"),
    );
    set.check(
        "the only reported defect is the expected unpublished tail",
        tail_reported == iterations,
        format!("tail_reported={tail_reported} expected={iterations} (after_commit_file only)"),
    );
    set.set("runs", json!(runs));
    set.set("old_count", json!(old_count));
    set.set("new_count", json!(new_count));
    set.set("partial_count", json!(partial_count));
    set.set("unpublished_tail_reported", json!(tail_reported));
    set.set("head_before_sequence", json!(heads));
    set.set("buckets", json!(buckets));
    set.set(
        "stale_lease_note",
        json!("a crashed S3 writer's LOCK object was cleared explicitly before each retry; production waits out its 30 s lease, which is never disabled"),
    );
    set.command(&format!(
        "{} (writer crashed at each WEFT_CRASH_PHASE)",
        court.args.weftfs_bin.display()
    ));
    Ok(set.finish())
}

fn run_s2_crash_publication(court: &mut Court) -> Result<Value, String> {
    crash_publication(
        court,
        2,
        "S2",
        "atomic publication under a process fault (single S3)",
        Layout::SingleS3,
        5,
    )
}

// ---------------------------------------------------------------------------
// S3 — idempotent retry
// ---------------------------------------------------------------------------

fn reference_state() -> Result<(u64, String, Hash32, Hash32), String> {
    // A reference store is local and deterministic; it never touches S3, so it
    // cannot interfere with any bucket under test.
    let dir = std::env::temp_dir().join(format!("weft-dist-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let mut store = Store::init(
        &dir,
        Options {
            chunk_size: CHUNK_SIZE,
            checkpoint_interval: CHECKPOINT_INTERVAL,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(FIXED_TS),
            ..Options::default()
        },
    )
    .map_err(|err| format!("reference init: {err}"))?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    store
        .group(|store| {
            store.create_file_seeded(
                root,
                b"alpha",
                ALPHA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"dist-alpha",
            )?;
            store.create_file_seeded(
                root,
                b"beta",
                BETA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"dist-beta",
            )?;
            Ok(())
        })
        .map_err(|err| format!("reference group: {err}"))?;
    let generation = store.generation();
    let head = store.snapshot_root().to_hex();
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let alpha = store
        .lookup(root, b"alpha")
        .map_err(|err| err.to_string())?;
    let beta = store.lookup(root, b"beta").map_err(|err| err.to_string())?;
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok((generation, head, alpha, beta))
}

fn run_s3_retry(court: &mut Court) -> Result<Value, String> {
    let mut set = CheckSet::new(court, 3, "S3", "idempotent retry after a crash (single S3)")?;
    let (reference_generation, reference_head, reference_alpha, reference_beta) =
        reference_state()?;
    set.check(
        "reference run published one generation",
        reference_generation == 2,
        format!("generation={reference_generation} head={reference_head}"),
    );
    set.set(
        "reference",
        json!({
            "generation": reference_generation,
            "head": reference_head,
            "alpha": reference_alpha.to_hex(),
            "beta": reference_beta.to_hex(),
        }),
    );

    // 3a: crash after objects, before any commit file exists. The retry must
    // reuse the generation number and produce the identical commit.
    {
        let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s3a")?;
        set.set("store_after_objects", spec_detail(&spec));
        let dir = court.run_dir.join("s3a");
        let generation_before = spec.init()?.generation();
        set.generation(generation_before);
        let child = court.spawn_mode(
            &dir,
            "writer",
            "group-create",
            &spec_path,
            &[("WEFT_CRASH_PHASE", "after_objects")],
        )?;
        let (record, _) = child.finish(Duration::from_secs(60))?;
        set.process(&record);
        set.command(&format!(
            "{}  (WEFT_CRASH_PHASE=after_objects)",
            sh_quote(&record.argv)
        ));
        set.check(
            "3a writer died at after_objects",
            !record.succeeded(),
            format!("exit={}", record.exit),
        );
        clear_stale_lock(&spec)?;
        let mut store = open_store(&spec, OpenMode::ReadWrite)?;
        let after_crash_generation = store.generation();
        set.generation(after_crash_generation);
        set.check(
            "3a pre-commit crash published nothing",
            after_crash_generation == generation_before,
            format!("generation={after_crash_generation}"),
        );
        group_create_alpha_beta(&mut store)?;
        let generation = store.generation();
        let head = store.snapshot_root().to_hex();
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let alpha = store
            .lookup(root, b"alpha")
            .map_err(|err| err.to_string())?;
        let beta = store.lookup(root, b"beta").map_err(|err| err.to_string())?;
        set.generation(generation);
        set.check(
            "3a retry reused the generation number",
            generation == reference_generation,
            format!("generation={generation} reference={reference_generation}"),
        );
        set.check(
            "3a retried head equals the uninterrupted head",
            head == reference_head,
            format!("head={head} reference={reference_head}"),
        );
        set.check(
            "3a retried object identities equal the uninterrupted run",
            alpha == reference_alpha && beta == reference_beta,
            format!("alpha={} beta={}", alpha.to_hex(), beta.to_hex()),
        );
        set.set(
            "after_objects",
            json!({
                "generation": generation,
                "head": head,
                "head_equals_uninterrupted": head == reference_head,
            }),
        );
        drop(store);
        let verification = verify_spec(&spec, "3a")?;
        set.check(
            "3a reopen verifies",
            verification["ok"] == json!(true),
            format!("generation={}", verification["generation"]),
        );
    }

    // 3b: crash after the commit file, before the head. The abandoned
    // generation cannot be reused, but the retry must not create a second
    // visible state.
    {
        let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s3b")?;
        set.set("store_after_commit_file", spec_detail(&spec));
        let dir = court.run_dir.join("s3b");
        let generation_before = spec.init()?.generation();
        set.generation(generation_before);
        let child = court.spawn_mode(
            &dir,
            "writer",
            "group-create",
            &spec_path,
            &[("WEFT_CRASH_PHASE", "after_commit_file")],
        )?;
        let (record, _) = child.finish(Duration::from_secs(60))?;
        set.process(&record);
        set.command(&format!(
            "{}  (WEFT_CRASH_PHASE=after_commit_file)",
            sh_quote(&record.argv)
        ));
        set.check(
            "3b writer died at after_commit_file",
            !record.succeeded(),
            format!("exit={}", record.exit),
        );
        clear_stale_lock(&spec)?;
        let mut store = open_store(&spec, OpenMode::ReadWrite)?;
        let after_crash_generation = store.generation();
        set.generation(after_crash_generation);
        set.check(
            "3b the unpublished commit file is invisible",
            after_crash_generation == generation_before,
            format!("generation={after_crash_generation}"),
        );
        let missing = read_path(&store, "/alpha");
        set.check(
            "3b the crashed change did not become visible",
            missing.is_err(),
            format!("resolve_ok={}", missing.is_ok()),
        );
        group_create_alpha_beta(&mut store)?;
        let generation = store.generation();
        let head = store.snapshot_root().to_hex();
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let entries = store.list_dir(root).map_err(|err| err.to_string())?;
        let lineage = store
            .commit_lineage(store.snapshot_root())
            .map_err(|err| err.to_string())?
            .iter()
            .map(|entry| entry.generation)
            .collect::<Vec<u64>>();
        let alpha = store
            .lookup(root, b"alpha")
            .map_err(|err| err.to_string())?;
        let beta = store.lookup(root, b"beta").map_err(|err| err.to_string())?;
        let alpha_bytes = store.read_object(alpha).map_err(|err| err.to_string())?;
        set.generation(generation);
        set.check(
            "3b the retry never reuses the abandoned generation",
            generation == reference_generation + 1,
            format!("generation={generation} reference={reference_generation}"),
        );
        set.check(
            "3b exactly one entry exists per path",
            entries.len() == 2,
            format!("entries={}", entries.len()),
        );
        set.check(
            "3b the abandoned generation is not on the visible parent chain",
            lineage == vec![reference_generation + 1, 1],
            format!("lineage={lineage:?}"),
        );
        set.check(
            "3b the visible state is the reference state",
            alpha == reference_alpha && beta == reference_beta && alpha_bytes == ALPHA_OLD,
            format!("head={head} (differs from the reference only by generation)"),
        );
        set.set(
            "after_commit_file",
            json!({
                "generation": generation,
                "head": head,
                "reused_generation": generation == reference_generation,
                "visible_entry_per_path": entries.len(),
                "lineage_generations": lineage,
            }),
        );
        drop(store);
        let verification = verify_spec(&spec, "3b")?;
        set.check(
            "3b reopen verifies",
            verification["ok"] == json!(true),
            format!("problems={:?}", verification["problems"]),
        );
    }

    // 3c: crash after the head is published. The change is already the
    // uninterrupted commit; an at-least-once retry is refused and writes
    // nothing.
    {
        let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s3c")?;
        set.set("store_after_superblock_write", spec_detail(&spec));
        let dir = court.run_dir.join("s3c");
        let generation_before = spec.init()?.generation();
        set.generation(generation_before);
        let child = court.spawn_mode(
            &dir,
            "writer",
            "group-create",
            &spec_path,
            &[("WEFT_CRASH_PHASE", "after_superblock_write")],
        )?;
        let (record, _) = child.finish(Duration::from_secs(60))?;
        set.process(&record);
        set.command(&format!(
            "{}  (WEFT_CRASH_PHASE=after_superblock_write)",
            sh_quote(&record.argv)
        ));
        set.check(
            "3c writer died after publishing the head",
            !record.succeeded(),
            format!("exit={}", record.exit),
        );
        clear_stale_lock(&spec)?;
        let mut store = open_store(&spec, OpenMode::ReadWrite)?;
        let generation = store.generation();
        let head = store.snapshot_root().to_hex();
        set.generation(generation);
        set.check(
            "3c the published crash is already the uninterrupted commit",
            generation == reference_generation && head == reference_head,
            format!("generation={generation} head={head}"),
        );
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let err = store
            .create_file_seeded(
                root,
                b"alpha",
                ALPHA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"dist-alpha",
            )
            .err()
            .map(|err| (err.class_name().to_owned(), err.message().to_owned()));
        let after = store.generation();
        set.check(
            "3c the retry is refused as Exists",
            matches!(&err, Some((class, _)) if class == "Exists"),
            format!("error={err:?}"),
        );
        set.check(
            "3c the refused retry wrote nothing",
            after == reference_generation,
            format!("generation={after}"),
        );
        let alpha = store
            .lookup(root, b"alpha")
            .map_err(|err| err.to_string())?;
        set.check(
            "3c the existing object already carries the intended bytes",
            store.read_object(alpha).map_err(|err| err.to_string())? == ALPHA_OLD,
            format!("object={}", alpha.to_hex()),
        );
        set.set(
            "after_superblock_write",
            json!({
                "generation": generation,
                "head": head,
                "head_equals_uninterrupted": head == reference_head,
                "retry": "Exists (no write)",
            }),
        );
        drop(store);
        let verification = verify_spec(&spec, "3c")?;
        set.check(
            "3c reopen verifies",
            verification["ok"] == json!(true),
            format!("generation={}", verification["generation"]),
        );
    }

    Ok(set.finish())
}

fn group_create_alpha_beta(store: &mut Store) -> Result<(), String> {
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    store
        .group(|store| {
            store.create_file_seeded(
                root,
                b"alpha",
                ALPHA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"dist-alpha",
            )?;
            store.create_file_seeded(
                root,
                b"beta",
                BETA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"dist-beta",
            )?;
            Ok(())
        })
        .map(|_| ())
        .map_err(|err| format!("group create: {err}"))
}

// ---------------------------------------------------------------------------
// Child implementations
// ---------------------------------------------------------------------------

fn child_main(mode: &str, _args: &[String]) -> Result<(), String> {
    let spec_path = std::env::var("WEFT_DIST_SPEC").map_err(|_| "WEFT_DIST_SPEC is not set")?;
    let spec = StoreSpec::load(Path::new(&spec_path))?;
    let ready = env_path("WEFT_DIST_READY_FILE");
    let status = env_path("WEFT_DIST_STATUS_FILE");
    let gate = env_path("WEFT_DIST_GATE");
    match mode {
        "hold-writer" => child_hold_writer(&spec, ready, status, gate),
        "group-create" => child_group(&spec, status, "create"),
        "group-overwrite" => child_group(&spec, status, "overwrite"),
        "reader" => child_reader(&spec, ready, status, gate, false),
        "reader-fail" => child_reader(&spec, ready, status, gate, true),
        "gc" => child_gc(&spec, status),
        "verify" => child_verify(&spec, status),
        "capability" => child_capability(&spec, status),
        other => Err(format!("unknown child mode `{other}`")),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().map(PathBuf::from)
}

fn child_hold_writer(
    spec: &StoreSpec,
    ready: Option<PathBuf>,
    status: Option<PathBuf>,
    gate: Option<PathBuf>,
) -> Result<(), String> {
    let store = match spec.open_typed(OpenMode::ReadWrite) {
        Ok(store) => store,
        Err(err) => {
            if let Some(path) = &status {
                write_json_file(
                    path,
                    &json!({
                        "ok": false,
                        "phase": "open",
                        "class": err.class_name(),
                        "message": err.message(),
                    }),
                )?;
            }
            // A refused open is a real nonzero exit, so the parent's exit-code
            // evidence is not confused with a clean hold-and-close.
            return Err(format!(
                "open failed: {}: {}",
                err.class_name(),
                err.message()
            ));
        }
    };
    if let Some(path) = &ready {
        write_json_file(
            path,
            &json!({
                "pid": std::process::id(),
                "generation": store.generation(),
                "head": store.snapshot_root().to_hex(),
            }),
        )?;
    }
    let hold_ms: u64 = std::env::var("WEFT_DIST_HOLD_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);
    if hold_ms > 0 {
        if let Some(gate) = &gate {
            let _ = wait_for_file(gate, Duration::from_millis(hold_ms + 30_000));
        } else {
            std::thread::sleep(Duration::from_millis(hold_ms));
        }
    }
    if let Some(path) = &status {
        write_json_file(
            path,
            &json!({
                "ok": true,
                "phase": "held",
                "generation": store.generation(),
                "head": store.snapshot_root().to_hex(),
            }),
        )?;
    }
    drop(store);
    Ok(())
}

fn child_group(spec: &StoreSpec, status: Option<PathBuf>, op: &str) -> Result<(), String> {
    let mut store = spec.open(OpenMode::ReadWrite)?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    match op {
        "create" => {
            store
                .group(|store| {
                    store.create_file_seeded(
                        root,
                        b"alpha",
                        ALPHA_OLD,
                        0o644,
                        0,
                        0,
                        BTreeMap::new(),
                        b"dist-alpha",
                    )?;
                    store.create_file_seeded(
                        root,
                        b"beta",
                        BETA_OLD,
                        0o644,
                        0,
                        0,
                        BTreeMap::new(),
                        b"dist-beta",
                    )?;
                    Ok(())
                })
                .map_err(|err| format!("group create: {err}"))?;
        }
        "overwrite" => {
            let alpha = store
                .lookup(root, b"alpha")
                .map_err(|err| err.to_string())?;
            let beta = store.lookup(root, b"beta").map_err(|err| err.to_string())?;
            store
                .group(|store| {
                    store.write_file(alpha, 0, ALPHA_NEW)?;
                    store.write_file(beta, 0, BETA_NEW)?;
                    Ok(())
                })
                .map_err(|err| format!("group overwrite: {err}"))?;
        }
        other => return Err(format!("unknown group op `{other}`")),
    }
    if let Some(path) = &status {
        write_json_file(
            path,
            &json!({
                "ok": true,
                "phase": op,
                "generation": store.generation(),
                "head": store.snapshot_root().to_hex(),
            }),
        )?;
    }
    Ok(())
}

fn read_pinned(store: &Store, pin: Option<Hash32>, id: Hash32) -> weftfs::Result<Vec<u8>> {
    match pin {
        Some(commit) => store.read_object_at(Some(commit), id),
        None => store.read_object(id),
    }
}

fn child_reader(
    spec: &StoreSpec,
    ready: Option<PathBuf>,
    status: Option<PathBuf>,
    gate: Option<PathBuf>,
    fail_closed: bool,
) -> Result<(), String> {
    let path = std::env::var("WEFT_DIST_PATH").unwrap_or_else(|_| "/f".to_owned());
    let before = std::env::var("WEFT_DIST_EXPECT_BEFORE").unwrap_or_default();
    let after = std::env::var("WEFT_DIST_EXPECT_AFTER").unwrap_or_default();
    let reads: usize = std::env::var("WEFT_DIST_READS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let reopen = std::env::var("WEFT_DIST_REOPEN")
        .map(|value| value == "1")
        .unwrap_or(false);
    let interval = Duration::from_millis(
        std::env::var("WEFT_DIST_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(5),
    );

    let store = match spec.open_typed(OpenMode::ReadOnly) {
        Ok(store) => store,
        Err(err) => {
            if let Some(path) = &status {
                write_json_file(
                    path,
                    &json!({
                        "ok": false,
                        "phase": "open",
                        "class": err.class_name(),
                        "message": err.message(),
                    }),
                )?;
            }
            return Err(format!(
                "open failed: {}: {}",
                err.class_name(),
                err.message()
            ));
        }
    };
    let generation_before = store.generation();
    let head_before = store.snapshot_root().to_hex();
    // A reader may pin a snapshot (the GC-race case) and/or name an object
    // directly, so an object that is no longer in the live namespace can still
    // be addressed through the snapshot.
    let pin = match std::env::var("WEFT_DIST_SNAPSHOT") {
        Ok(name) if !name.is_empty() => {
            Some(lookup_snapshot(&store, &name).map_err(|err| format!("snapshot {name}: {err}"))?)
        }
        _ => None,
    };
    let id = match std::env::var("WEFT_DIST_OBJECT") {
        Ok(text) if !text.is_empty() => {
            Hash32::from_hex(&text).map_err(|err| format!("object id: {err}"))?
        }
        _ => match pin {
            Some(commit) => store
                .resolve_path_at(Some(commit), &path)
                .map_err(|err| err.to_string())?,
            None => store.resolve_path(&path).map_err(|err| err.to_string())?,
        },
    };
    let pinned_state = match pin {
        Some(commit) => Some(store.state_at(commit).map_err(|err| err.to_string())?),
        None => None,
    };
    let meta = match &pinned_state {
        Some(state) => state
            .objects
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("object {} is not in the pinned snapshot", id.to_hex()))?,
        None => store.object_meta(id).map_err(|err| err.to_string())?,
    };

    let mut stale_reads = 0usize;
    for index in 0..reads {
        let bytes = match read_pinned(&store, pin, id) {
            Ok(bytes) => bytes,
            Err(err) => {
                if let Some(path) = &status {
                    write_json_file(
                        path,
                        &json!({
                            "ok": false,
                            "phase": "before",
                            "class": err.class_name(),
                            "message": err.message(),
                            "reads": index,
                        }),
                    )?;
                }
                return Ok(());
            }
        };
        if !before.is_empty() && sha256_hex(&bytes) != before {
            if let Some(path) = &status {
                write_json_file(
                    path,
                    &json!({
                        "ok": false,
                        "phase": "before",
                        "class": "Mismatch",
                        "message": "bytes do not match the construction oracle",
                        "reads": index,
                        "sha256": sha256_hex(&bytes),
                    }),
                )?;
            }
            return Ok(());
        }
        std::thread::sleep(interval);
    }

    // Report ready only after the pre-gate reads, so a parent that stops the
    // server or publishes new generations knows the reader has already
    // observed the pinned state. This is what makes 6b's "stop mid-run"
    // sequencing deterministic.
    if let Some(path) = &ready {
        write_json_file(
            path,
            &json!({
                "pid": std::process::id(),
                "generation": generation_before,
                "head": head_before,
                "object": id.to_hex(),
                "content": meta.content.to_hex(),
                "size": meta.size,
                "snapshot": pin.map(|commit| commit.to_hex()),
                "before_reads": reads,
            }),
        )?;
    }

    if let Some(gate) = &gate {
        let _ = wait_for_file(gate, Duration::from_secs(180));
    }

    if fail_closed {
        // After the gate the server may be gone. A read must fail with a
        // transport error and must not return any bytes.
        match read_pinned(&store, pin, id) {
            Ok(bytes) => {
                if let Some(path) = &status {
                    write_json_file(
                        path,
                        &json!({
                            "ok": false,
                            "phase": "fail-closed",
                            "class": "NoError",
                            "message": "read unexpectedly succeeded",
                            "bytes": bytes.len(),
                        }),
                    )?;
                }
            }
            Err(err) => {
                if let Some(path) = &status {
                    write_json_file(
                        path,
                        &json!({
                            "ok": true,
                            "phase": "fail-closed",
                            "class": err.class_name(),
                            "message": err.message(),
                            "bytes_emitted": 0,
                        }),
                    )?;
                }
            }
        }
        return Ok(());
    }

    // Stale phase: the same open handle must still see the pinned bytes even
    // after the writer published new generations.
    for index in 0..reads {
        let bytes = read_pinned(&store, pin, id).map_err(|err| err.to_string())?;
        if !before.is_empty() && sha256_hex(&bytes) != before {
            if let Some(path) = &status {
                write_json_file(
                    path,
                    &json!({
                        "ok": false,
                        "phase": "stale",
                        "class": "Mismatch",
                        "message": "an open reader observed a later generation",
                        "reads": index,
                        "sha256": sha256_hex(&bytes),
                    }),
                )?;
            }
            return Ok(());
        }
        stale_reads += 1;
        std::thread::sleep(interval);
    }

    let mut generation_after = generation_before;
    let mut fresh_reads = 0usize;
    if reopen {
        drop(store);
        let fresh = spec.open(OpenMode::ReadOnly)?;
        generation_after = fresh.generation();
        for _ in 0..reads {
            let bytes = read_pinned(&fresh, pin, id).map_err(|err| err.to_string())?;
            if !after.is_empty() && sha256_hex(&bytes) != after {
                if let Some(path) = &status {
                    write_json_file(
                        path,
                        &json!({
                            "ok": false,
                            "phase": "reopen",
                            "class": "Mismatch",
                            "message": "the reopened handle did not observe the new bytes",
                            "sha256": sha256_hex(&bytes),
                        }),
                    )?;
                }
                return Ok(());
            }
            fresh_reads += 1;
            std::thread::sleep(interval);
        }
    }

    if let Some(path) = &status {
        write_json_file(
            path,
            &json!({
                "ok": true,
                "phase": "done",
                "generation_before": generation_before,
                "generation_after": generation_after,
                "head_before": head_before,
                "stale_reads": stale_reads,
                "fresh_reads": fresh_reads,
                "total_reads": reads * 2 + fresh_reads,
            }),
        )?;
    }
    Ok(())
}

fn child_gc(spec: &StoreSpec, status: Option<PathBuf>) -> Result<(), String> {
    let mut store = spec.open(OpenMode::ReadWrite)?;
    let report = store.gc(true).map_err(|err| format!("gc: {err}"))?;
    if let Some(path) = &status {
        write_json_file(
            path,
            &json!({
                "ok": report.problems.is_empty(),
                "applied": report.applied,
                "marked_manifests": report.marked_manifests,
                "marked_chunks": report.marked_chunks,
                "candidates": report.candidates.len(),
                "deleted": report.deleted.len(),
                "deleted_bytes": report.deleted_bytes,
                "retained": report.retained,
                "removed_orphans": report.removed_orphans.len(),
                "problems": report.problems,
                "generation": store.generation(),
            }),
        )?;
    }
    Ok(())
}

fn child_verify(spec: &StoreSpec, status: Option<PathBuf>) -> Result<(), String> {
    let store = spec.open(OpenMode::ReadOnly)?;
    let report = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    if let Some(path) = &status {
        write_json_file(
            path,
            &json!({
                "ok": report.ok,
                "generation": report.generation,
                "checked_commits": report.checked_commits,
                "checked_objects": report.checked_objects,
                "checked_manifests": report.checked_manifests,
                "checked_chunks": report.checked_chunks,
                "problems": report.problems.iter().map(|problem| json!({
                    "class": problem.class,
                    "detail": problem.detail,
                })).collect::<Vec<Value>>(),
            }),
        )?;
    }
    Ok(())
}

fn child_capability(spec: &StoreSpec, status: Option<PathBuf>) -> Result<(), String> {
    let envelope_path = std::env::var("WEFT_DIST_ENVELOPE").map_err(|_| "WEFT_DIST_ENVELOPE")?;
    let now: i64 = std::env::var("WEFT_DIST_NOW_NS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(FIXED_TS);
    let bytes = std::fs::read(&envelope_path).map_err(|err| format!("envelope: {err}"))?;
    let verified = weftcodec::capability::verify_envelope(&bytes, &[], now, None)
        .map_err(|err| format!("envelope verify: {err}"))?;
    let principal = Principal::root().with_envelope(verified);
    let store = spec.open(OpenMode::ReadOnly)?;
    let visible = store
        .resolve_path("/visible")
        .map_err(|err| err.to_string())?;
    let secret = store
        .resolve_path("/secret")
        .map_err(|err| err.to_string())?;
    let visible_meta = store.object_meta(visible).map_err(|err| err.to_string())?;
    let secret_meta = store.object_meta(secret).map_err(|err| err.to_string())?;
    let posix_readable = weftfs::auth::can_read(&secret_meta, &Principal::root());

    let secret_by_content = store.exact_id_object(secret_meta.content, None, &principal, now);
    let absent = Hash32::from_bytes([0xAB; 32]);
    let absent_by_content = store.exact_id_object(absent, None, &principal, now);
    let visible_by_content = store.exact_id_object(visible_meta.content, None, &principal, now);
    let token = store.authorize_read(visible, &Principal::root(), now);
    let cross_content = token.as_ref().ok().map(|token| {
        store.read_content_authorized(token, secret_meta.content, 0, secret_meta.size)
    });

    let describe = |result: &weftfs::Result<weftfs::state::ObjectRecord>| match result {
        Ok(record) => json!({"ok": true, "object": record.id.to_hex()}),
        Err(err) => json!({"ok": false, "class": err.class_name(), "message": err.message()}),
    };
    let cross = match &cross_content {
        None => json!({"ok": false, "class": "NoToken", "message": "no token"}),
        Some(Ok(bytes)) => json!({"ok": true, "bytes": bytes.len()}),
        Some(Err(err)) => json!({"ok": false, "class": err.class_name(), "message": err.message()}),
    };
    let value = json!({
        "ok": true,
        "posix_readable": posix_readable,
        "secret_object": secret.to_hex(),
        "secret_content": secret_meta.content.to_hex(),
        "visible_object": visible.to_hex(),
        "secret_by_content": describe(&secret_by_content),
        "absent_by_content": describe(&absent_by_content),
        "visible_by_content": describe(&visible_by_content),
        "cross_content": cross,
        "generation": store.generation(),
    });
    if let Some(path) = &status {
        write_json_file(path, &value)?;
    }
    Ok(())
}

fn concurrent_readers(
    court: &mut Court,
    scenario: u32,
    id: &'static str,
    name: &'static str,
    layout: Layout,
) -> Result<Value, String> {
    const READERS: usize = 8;
    const READS: usize = 4;
    const VERSIONS: usize = 10;
    let mut set = CheckSet::new(court, scenario, id, name)?;
    let (spec_path, spec) = court.write_spec(layout, &format!("s{scenario}-readers"))?;
    set.set("store", spec_detail(&spec));

    let pinned_generation;
    let pinned_head;
    let pinned_object;
    {
        let mut store = spec.init()?;
        let object = create_file(&spec, &mut store, b"hot", PINNED, b"concurrent-hot")?;
        store
            .snapshot_create("pin")
            .map_err(|err| err.to_string())?;
        pinned_generation = store.generation();
        pinned_head = store.snapshot_root().to_hex();
        pinned_object = object.to_hex();
    }
    set.generation(pinned_generation);
    set.set(
        "pinned_commit",
        json!({"generation": pinned_generation, "head": pinned_head, "object": pinned_object}),
    );
    let expected = sha256_hex(PINNED);
    let dir = court.run_dir.join(format!("s{scenario}-readers"));

    let mut readers = Vec::new();
    for index in 0..READERS {
        let label = format!("reader-{index:02}");
        let child = court.spawn_mode(
            &dir,
            &label,
            "reader",
            &spec_path,
            &[
                ("WEFT_DIST_PATH", "/hot"),
                ("WEFT_DIST_EXPECT_BEFORE", expected.as_str()),
                ("WEFT_DIST_READS", "4"),
                ("WEFT_DIST_REOPEN", "0"),
                ("WEFT_DIST_INTERVAL_MS", "5"),
            ],
        )?;
        let ready = wait_for_json(&child.ready_file(), READY_TIMEOUT)
            .map_err(|err| format!("reader {index}: {err}"))?;
        set.check(
            &format!("reader {index} opened at the pinned generation"),
            ready["generation"].as_u64() == Some(pinned_generation)
                && ready["object"].as_str() == Some(pinned_object.as_str()),
            format!(
                "generation={} object={}",
                ready["generation"], ready["object"]
            ),
        );
        readers.push((label, child));
    }

    let published;
    {
        let mut store = open_store(&spec, OpenMode::ReadWrite)?;
        let mut sequence = Vec::new();
        for round in 0..VERSIONS {
            let mut next = b"pinned-reader-payload-".to_vec();
            next.extend_from_slice(format!("{round:04}").as_bytes());
            write_path(&mut store, "/hot", &next)?;
            sequence.push(store.generation());
        }
        published = store.generation();
        set.generation(published);
        set.set("published_generations", json!(sequence));
        drop(store);
    }

    let mut total_reads = 0usize;
    for (label, child) in readers {
        std::fs::write(child.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
        let (record, status) = child.finish(Duration::from_secs(90))?;
        set.process(&record);
        let status = status.unwrap_or_else(|| json!({}));
        let stale_reads = status["stale_reads"].as_u64().unwrap_or(0);
        total_reads += status["total_reads"].as_u64().unwrap_or(0) as usize;
        set.check(
            &format!("{label} kept the exact pinned bytes across {VERSIONS} commits"),
            record.succeeded() && status["ok"] == json!(true) && stale_reads == READS as u64,
            format!(
                "exit={} stale_reads={stale_reads} reads={}",
                record.exit, status["total_reads"]
            ),
        );
    }
    set.check(
        "every reader completed its reads",
        total_reads == READERS * READS * 2,
        format!("reads={total_reads} expected={}", READERS * READS * 2),
    );
    set.check(
        "the writer published exactly the intended generations",
        published == pinned_generation + VERSIONS as u64,
        format!("generation={published} pinned={pinned_generation}"),
    );
    let verification = verify_spec(&spec, "concurrent readers")?;
    set.set_verification(verification.clone());
    set.check(
        "verification is clean",
        verification["ok"] == json!(true),
        format!("generation={}", verification["generation"]),
    );
    set.set("readers", json!(READERS));
    set.set("versions", json!(VERSIONS));
    set.set("total_reads", json!(total_reads));
    Ok(set.finish())
}

fn run_s4_concurrent_readers(court: &mut Court) -> Result<Value, String> {
    concurrent_readers(
        court,
        4,
        "S4",
        "concurrent readers during commits (single S3)",
        Layout::SingleS3,
    )
}

fn stale_reader(
    court: &mut Court,
    scenario: u32,
    id: &'static str,
    name: &'static str,
    layout: Layout,
) -> Result<Value, String> {
    const OLD: &[u8] = b"stale-reader-old-state-";
    const NEW: &[u8] = b"stale-reader-new-state-";
    const READS: usize = 4;
    let mut set = CheckSet::new(court, scenario, id, name)?;
    let (spec_path, spec) = court.write_spec(layout, &format!("s{scenario}-stale"))?;
    set.set("store", spec_detail(&spec));
    let opened_generation;
    let opened_head;
    {
        let mut store = spec.init()?;
        create_file(&spec, &mut store, b"f", OLD, b"stale-f")?;
        opened_generation = store.generation();
        opened_head = store.snapshot_root().to_hex();
    }
    set.generation(opened_generation);
    let dir = court.run_dir.join(format!("s{scenario}-stale"));
    let child = court.spawn_mode(
        &dir,
        "reader",
        "reader",
        &spec_path,
        &[
            ("WEFT_DIST_PATH", "/f"),
            ("WEFT_DIST_EXPECT_BEFORE", sha256_hex(OLD).as_str()),
            ("WEFT_DIST_EXPECT_AFTER", sha256_hex(NEW).as_str()),
            ("WEFT_DIST_READS", "4"),
            ("WEFT_DIST_REOPEN", "1"),
            ("WEFT_DIST_INTERVAL_MS", "5"),
        ],
    )?;
    let ready = wait_for_json(&child.ready_file(), READY_TIMEOUT)
        .map_err(|err| format!("stale reader: {err}"))?;
    set.check(
        "reader opened at the pre-write generation",
        ready["generation"].as_u64() == Some(opened_generation)
            && ready["head"].as_str() == Some(opened_head.as_str()),
        format!("generation={} head={}", ready["generation"], ready["head"]),
    );
    let published;
    {
        let mut store = open_store(&spec, OpenMode::ReadWrite)?;
        write_path(&mut store, "/f", NEW)?;
        published = store.generation();
    }
    set.generation(published);
    std::fs::write(child.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
    let (record, status) = child.finish(Duration::from_secs(90))?;
    set.process(&record);
    let status = status.unwrap_or_else(|| json!({}));
    set.check(
        "the open reader never observed the newer generation",
        record.succeeded()
            && status["ok"] == json!(true)
            && status["generation_before"].as_u64() == Some(opened_generation)
            && status["stale_reads"].as_u64() == Some(READS as u64),
        format!(
            "exit={} stale_reads={} generation_before={}",
            record.exit, status["stale_reads"], status["generation_before"]
        ),
    );
    set.check(
        "reopening is what observes the newer generation",
        status["generation_after"].as_u64() == Some(published)
            && status["fresh_reads"].as_u64() == Some(READS as u64),
        format!(
            "generation_after={} fresh_reads={}",
            status["generation_after"], status["fresh_reads"]
        ),
    );
    let verification = verify_spec(&spec, "stale reader")?;
    set.set_verification(verification.clone());
    set.check(
        "verification is clean",
        verification["ok"] == json!(true),
        format!("generation={}", verification["generation"]),
    );
    set.set("stale_reads", status["stale_reads"].clone());
    set.set("fresh_reads", status["fresh_reads"].clone());
    Ok(set.finish())
}

fn run_s5_stale_reader(court: &mut Court) -> Result<Value, String> {
    stale_reader(
        court,
        5,
        "S5",
        "stale reader until reopen (single S3)",
        Layout::SingleS3,
    )
}

fn run_s6_unreachable(court: &mut Court) -> Result<Value, String> {
    let mut set = CheckSet::new(court, 6, "S6", "unreachable store fails closed (single S3)")?;
    let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s6")?;
    set.set("store", spec_detail(&spec));
    let alive_generation;
    {
        let mut store = spec.init()?;
        create_file(&spec, &mut store, b"keep", KEEP, b"s6-keep")?;
        alive_generation = store.generation();
    }
    set.generation(alive_generation);
    let dir = court.run_dir.join("s6");
    let expected = sha256_hex(KEEP);

    // 6a: an endpoint with no listener. A reader process must fail at open
    // with an I/O class and emit no bytes.
    {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| format!("bind: {err}"))?;
        let closed_port = listener
            .local_addr()
            .map_err(|err| format!("local_addr: {err}"))?
            .port();
        drop(listener);
        let (unroutable_path, _unroutable) = court.write_spec_unroutable(
            Layout::SingleS3,
            "s6-unroutable",
            format!("http://127.0.0.1:{closed_port}"),
        )?;
        let child = court.spawn_mode(
            &dir,
            "unroutable",
            "reader",
            &unroutable_path,
            &[
                ("WEFT_DIST_PATH", "/keep"),
                ("WEFT_DIST_EXPECT_BEFORE", expected.as_str()),
                ("WEFT_DIST_READS", "1"),
            ],
        )?;
        set.command(&format!(
            "{}  (endpoint http://127.0.0.1:{closed_port}, no listener)",
            sh_quote(&child.argv)
        ));
        let (_record, status) = child.finish(Duration::from_secs(60))?;
        set.process(&_record);
        let status = status.unwrap_or_else(|| json!({}));
        set.check(
            "6a an endpoint with no listener fails closed with Io",
            status["ok"] == json!(false)
                && status["class"].as_str() == Some("Io")
                && !_record.succeeded(),
            format!(
                "exit={} class={} message={}",
                _record.exit, status["class"], status["message"]
            ),
        );
        set.check(
            "6a no bytes were emitted",
            _record.stdout_tail.is_empty(),
            format!("stdout_tail={:?}", _record.stdout_tail),
        );
    }

    // 6b: stop the real MinIO process while a reader is live, then restart it
    // on the same data directory and prove a fresh open verifies.
    {
        let child = court.spawn_mode(
            &dir,
            "stopped",
            "reader-fail",
            &spec_path,
            &[
                ("WEFT_DIST_PATH", "/keep"),
                ("WEFT_DIST_EXPECT_BEFORE", expected.as_str()),
                ("WEFT_DIST_READS", "2"),
            ],
        )?;
        let ready = wait_for_json(&child.ready_file(), READY_TIMEOUT)
            .map_err(|err| format!("6b reader: {err}"))?;
        set.check(
            "6b the reader opened and read before the stop",
            ready["generation"].as_u64() == Some(alive_generation),
            format!("generation={}", ready["generation"]),
        );
        let stopped = court.minio.stop();
        set.check(
            "6b the MinIO process is stopped",
            stopped.is_ok() && !court.minio.alive(),
            format!("stop={stopped:?}"),
        );
        std::fs::write(child.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
        let (record, status) = child.finish(Duration::from_secs(60))?;
        set.process(&record);
        let status = status.unwrap_or_else(|| json!({}));
        set.check(
            "6b a read against the stopped server fails with a transport error",
            status["ok"] == json!(true) && status["class"].as_str() == Some("Io"),
            format!(
                "exit={} class={} message={}",
                record.exit, status["class"], status["message"]
            ),
        );
        set.check(
            "6b no partial or zero-filled bytes were returned",
            status["bytes_emitted"].as_u64() == Some(0) && record.stdout_tail.is_empty(),
            format!(
                "bytes_emitted={} stdout_tail={:?}",
                status["bytes_emitted"], record.stdout_tail
            ),
        );

        let restarted = court.minio.start();
        set.check(
            "6b the MinIO process restarts on the same data",
            restarted.is_ok() && court.minio.alive(),
            format!("start={restarted:?}"),
        );
        let verify = court.spawn_mode(&dir, "after-restart-verify", "verify", &spec_path, &[])?;
        let (verify_record, verify_status) = verify.finish(Duration::from_secs(120))?;
        set.process(&verify_record);
        let verify_status = verify_status.unwrap_or_else(|| json!({}));
        set.check(
            "6b a subsequent successful reopen verifies",
            verify_record.succeeded()
                && verify_status["ok"] == json!(true)
                && verify_status["generation"].as_u64() == Some(alive_generation),
            format!(
                "exit={} generation={} problems={:?}",
                verify_record.exit, verify_status["generation"], verify_status["problems"]
            ),
        );
        let reader = court.spawn_mode(
            &dir,
            "after-restart-reader",
            "reader",
            &spec_path,
            &[
                ("WEFT_DIST_PATH", "/keep"),
                ("WEFT_DIST_EXPECT_BEFORE", expected.as_str()),
                ("WEFT_DIST_READS", "2"),
                ("WEFT_DIST_REOPEN", "0"),
            ],
        )?;
        let _ready = wait_for_json(&reader.ready_file(), READY_TIMEOUT)
            .map_err(|err| format!("6b recovery reader: {err}"))?;
        std::fs::write(reader.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
        let (reader_record, reader_status) = reader.finish(Duration::from_secs(60))?;
        set.process(&reader_record);
        let reader_status = reader_status.unwrap_or_else(|| json!({}));
        set.check(
            "6b the recovery reader completes without a mismatch",
            reader_record.succeeded() && reader_status["ok"] == json!(true),
            format!(
                "exit={} stale_reads={}",
                reader_record.exit, reader_status["stale_reads"]
            ),
        );
        set.set(
            "minio_restarts",
            json!({"starts": court.minio.starts, "stops": court.minio.stops}),
        );
    }

    Ok(set.finish())
}

fn run_s7_gc_race(court: &mut Court) -> Result<Value, String> {
    let mut set = CheckSet::new(
        court,
        7,
        "S7",
        "gc --apply racing a snapshot reader (single S3)",
    )?;
    let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s7")?;
    set.set("store", spec_detail(&spec));
    let dir = court.run_dir.join("s7");
    let keep_object;
    let gone_object;
    let opened_generation;
    let pinned_commit;
    {
        let mut store = spec.init()?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let keep = create_file(&spec, &mut store, b"keep", KEEP, b"gc-keep")?;
        let input = create_file(&spec, &mut store, b"input", INPUT, b"gc-input")?;
        let gone = create_file(
            &spec,
            &mut store,
            b"gone",
            b"gone-payload-000000",
            b"gc-gone",
        )?;
        keep_object = keep.to_hex();
        gone_object = gone.to_hex();
        let keep_meta = store.object_meta(keep).map_err(|err| err.to_string())?;
        let input_meta = store.object_meta(input).map_err(|err| err.to_string())?;
        let record = ProvenanceRecord::new(
            "derive",
            BTreeMap::new(),
            vec![ProvenanceInput::new(
                "source",
                input_meta.content,
                Some(input),
            )],
            vec![keep_meta.content],
            ProvenanceEnvironment::new("distributed-court", "1.0"),
            Determinism::Exact,
            ProvenancePrincipal::new(0, 0),
            FIXED_TS,
        )
        .map_err(|err| format!("provenance: {err}"))?;
        store
            .record_provenance(&[keep], &record)
            .map_err(|err| format!("provenance: {err}"))?;
        pinned_commit = store
            .snapshot_create("pinned")
            .map_err(|err| err.to_string())?
            .to_hex();
        store.checkpoint().map_err(|err| err.to_string())?;
        store.unlink(root, b"gone").map_err(|err| err.to_string())?;
        for index in 0..24 {
            let name = format!("garbage-{index:02}");
            let payload = format!("garbage-payload-{index:02}-xxxxxxxx");
            let tag = format!("gc-garbage-{index:02}");
            let _ = create_file(
                &spec,
                &mut store,
                name.as_bytes(),
                payload.as_bytes(),
                tag.as_bytes(),
            );
        }
        for index in 0..24 {
            let name = format!("garbage-{index:02}");
            store
                .unlink(root, name.as_bytes())
                .map_err(|err| err.to_string())?;
        }
        opened_generation = store.generation();
    }
    set.generation(opened_generation);
    set.set(
        "roots",
        json!({
            "snapshot": pinned_commit,
            "keep_object": keep_object,
            "gone_object": gone_object,
        }),
    );

    let reader = court.spawn_mode(
        &dir,
        "reader",
        "reader",
        &spec_path,
        &[
            ("WEFT_DIST_PATH", "/gone"),
            ("WEFT_DIST_SNAPSHOT", "pinned"),
            (
                "WEFT_DIST_EXPECT_BEFORE",
                sha256_hex(b"gone-payload-000000").as_str(),
            ),
            ("WEFT_DIST_READS", "6"),
            ("WEFT_DIST_REOPEN", "0"),
            ("WEFT_DIST_INTERVAL_MS", "5"),
        ],
    )?;
    let ready = wait_for_json(&reader.ready_file(), READY_TIMEOUT)
        .map_err(|err| format!("gc reader: {err}"))?;
    set.check(
        "the reader pinned the snapshot-only object",
        ready["object"].as_str() == Some(gone_object.as_str()),
        format!("object={}", ready["object"]),
    );

    let gc = court.spawn_mode(&dir, "gc", "gc", &spec_path, &[])?;
    let (gc_record, gc_status) = gc.finish(Duration::from_secs(180))?;
    set.process(&gc_record);
    let gc_status = gc_status.unwrap_or_else(|| json!({}));
    set.check(
        "gc --apply ran and reclaimed garbage",
        gc_record.succeeded()
            && gc_status["ok"] == json!(true)
            && gc_status["deleted"].as_u64().unwrap_or(0) > 0,
        format!(
            "exit={} deleted={} bytes={} problems={:?}",
            gc_record.exit, gc_status["deleted"], gc_status["deleted_bytes"], gc_status["problems"]
        ),
    );

    std::fs::write(reader.gate_file(), b"go").map_err(|err| format!("gate: {err}"))?;
    let (reader_record, reader_status) = reader.finish(Duration::from_secs(90))?;
    set.process(&reader_record);
    let reader_status = reader_status.unwrap_or_else(|| json!({}));
    set.check(
        "the live snapshot reader saw the exact pinned bytes through gc",
        reader_record.succeeded() && reader_status["ok"] == json!(true),
        format!(
            "exit={} stale_reads={}",
            reader_record.exit, reader_status["stale_reads"]
        ),
    );

    let verification = verify_spec(&spec, "gc race")?;
    set.set_verification(verification.clone());
    set.check(
        "verification is clean after gc",
        verification["ok"] == json!(true),
        format!("problems={:?}", verification["problems"]),
    );

    // Every root still resolves exactly after the sweep.
    {
        let store = open_store(&spec, OpenMode::ReadOnly)?;
        let pin = lookup_snapshot(&store, "pinned").map_err(|err| err.to_string())?;
        let pinned_state = store.state_at(pin).map_err(|err| err.to_string())?;
        let gone_id = Hash32::from_hex(&gone_object).map_err(|err| err.to_string())?;
        let gone_bytes = store
            .read_object_at(Some(pin), gone_id)
            .map_err(|err| format!("snapshot-only object after gc: {err}"))?;
        set.check(
            "the snapshot-only object is still readable",
            gone_bytes == b"gone-payload-000000",
            format!("bytes={}", gone_bytes.len()),
        );
        let keep_id = Hash32::from_hex(&keep_object).map_err(|err| err.to_string())?;
        let keep_bytes = store
            .read_object_at(Some(pin), keep_id)
            .map_err(|err| err.to_string())?;
        set.check(
            "the snapshot output is still exact",
            keep_bytes == KEEP,
            format!("bytes={}", keep_bytes.len()),
        );
        set.check(
            "the checkpoint state is still readable and names the snapshot object",
            pinned_state.objects.contains_key(&gone_id),
            format!("objects={}", pinned_state.objects.len()),
        );
        let records = store
            .provenance_records_of(keep_id)
            .map_err(|err| err.to_string())?;
        let keep_content = store
            .object_meta(keep_id)
            .map(|record| record.content)
            .unwrap_or(Hash32::ZERO);
        let declared = records
            .first()
            .map(|(id, _)| {
                store
                    .read_provenance(*id)
                    .map(|record| record.outputs.contains(&keep_content))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        set.check(
            "the provenance receipt and its input survived",
            !records.is_empty() && declared,
            format!("records={}", records.len()),
        );
    }
    set.set("deleted", gc_status["deleted"].clone());
    set.set("deleted_bytes", gc_status["deleted_bytes"].clone());
    Ok(set.finish())
}

fn run_s8_content_hash(court: &mut Court) -> Result<Value, String> {
    let mut set = CheckSet::new(
        court,
        8,
        "S8",
        "a known content hash is not authority (single S3)",
    )?;
    let (spec_path, spec) = court.write_spec(Layout::SingleS3, "s8")?;
    set.set("store", spec_detail(&spec));
    let envelope_path = court.run_dir.join("s8").join("envelope.bin");
    let secret_object;
    let visible_object;
    let secret_content;
    let generation;
    {
        let mut store = spec.init()?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let mut tenant_attrs = BTreeMap::new();
        tenant_attrs.insert("tenant".to_owned(), weftcodec::wcb::Value::text("a"));
        let visible = store
            .create_file_seeded(
                root,
                b"visible",
                b"visible-payload-0000",
                0o644,
                0,
                0,
                tenant_attrs,
                b"cap-visible",
            )
            .map_err(|err| err.to_string())?;
        let secret = store
            .create_file_seeded(
                root,
                b"secret",
                b"secret-payload-00000",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"cap-secret",
            )
            .map_err(|err| err.to_string())?;
        secret_object = secret.to_hex();
        visible_object = visible.to_hex();
        secret_content = store
            .object_meta(secret)
            .map_err(|err| err.to_string())?
            .content
            .to_hex();
        let snapshot = store
            .snapshot_create("cap-snapshot")
            .map_err(|err| err.to_string())?;
        let selector = weftcodec::selector::parse("select attrs.tenant == \"a\"")
            .map_err(|err| err.to_string())?;
        let context = weftcodec::context::ContextRoot::new(
            &selector,
            snapshot,
            weftcodec::context::DEFAULT_RESOLVER,
            Vec::new(),
            None,
            None,
        )
        .map_err(|err| err.to_string())?;
        store
            .save_context_root("visible-view", &context, None, false)
            .map_err(|err| err.to_string())?;
        let context_id = context.id().map_err(|err| err.to_string())?;
        let signer_seed = [7u8; 32];
        let issuer = weftcodec::capability::public_key_for_seed(&signer_seed)
            .map_err(|err| err.to_string())?;
        let envelope = weftcodec::capability::CapabilityEnvelope::new(
            issuer,
            Hash32::from_bytes([0x42; 32]),
            "tenant-a",
            vec![context_id],
            Vec::new(),
            Vec::new(),
            None,
            None,
            Hash32::from_bytes([0x22; 32]),
            FIXED_TS,
            Hash32::from_bytes([0x33; 32]),
        )
        .map_err(|err| err.to_string())?;
        let blob = weftcodec::capability::sign_envelope(&envelope, &signer_seed)
            .map_err(|err| err.to_string())?;
        let bytes = blob.encode().map_err(|err| err.to_string())?;
        if let Some(parent) = envelope_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::write(&envelope_path, &bytes)
            .map_err(|err| format!("cannot write envelope: {err}"))?;
        store
            .save_capability_envelope(&blob)
            .map_err(|err| err.to_string())?;
        generation = store.generation();
    }
    set.generation(generation);
    let dir = court.run_dir.join("s8");

    let child = court.spawn_mode(
        &dir,
        "capability",
        "capability",
        &spec_path,
        &[
            (
                "WEFT_DIST_ENVELOPE",
                envelope_path.to_str().unwrap_or_default(),
            ),
            ("WEFT_DIST_NOW_NS", "1700000000000000000"),
        ],
    )?;
    let (record, status) = child.finish(Duration::from_secs(120))?;
    set.process(&record);
    let status = status.unwrap_or_else(|| json!({}));
    set.check(
        "the envelope principal is a valid authenticated grant",
        status["ok"] == json!(true) && record.succeeded(),
        format!("exit={}", record.exit),
    );
    set.check(
        "the secret is POSIX-readable, so the denial is the capability",
        status["posix_readable"] == json!(true),
        format!("posix_readable={}", status["posix_readable"]),
    );
    let secret_lookup = &status["secret_by_content"];
    set.check(
        "knowing the secret ContentId is denied as NotFound",
        secret_lookup["class"].as_str() == Some("NotFound")
            && secret_lookup["message"].as_str() == Some("no such object"),
        format!(
            "class={} message={}",
            secret_lookup["class"], secret_lookup["message"]
        ),
    );
    set.check(
        "an absent ContentId produces the identical answer",
        status["absent_by_content"]["class"] == secret_lookup["class"]
            && status["absent_by_content"]["message"] == secret_lookup["message"],
        format!(
            "absent_class={} absent_message={}",
            status["absent_by_content"]["class"], status["absent_by_content"]["message"]
        ),
    );
    set.check(
        "the granted object is addressable by its ContentId",
        status["visible_by_content"]["ok"] == json!(true)
            && status["visible_by_content"]["object"].as_str() == Some(visible_object.as_str()),
        format!("result={}", status["visible_by_content"]),
    );
    set.check(
        "an authorized token cannot cover another content root",
        status["cross_content"]["class"].as_str() == Some("Unauthorized")
            && status["cross_content"]["message"].as_str()
                == Some("authorization token does not cover this content root"),
        format!(
            "class={} message={}",
            status["cross_content"]["class"], status["cross_content"]["message"]
        ),
    );
    let texts = [
        secret_lookup["message"].as_str().unwrap_or(""),
        status["cross_content"]["message"].as_str().unwrap_or(""),
    ];
    set.check(
        "no denial text reveals the secret's name",
        texts.iter().all(|text| !text.contains("secret")),
        format!("texts={texts:?}"),
    );
    set.set(
        "content_hashes",
        json!({"secret": secret_content, "secret_object": secret_object, "visible_object": visible_object}),
    );

    // The shipped CLI path applies the same gate before reading bytes.
    let envelope_text = envelope_path.display().to_string();
    let visible_cat = court.run_cli(
        &dir,
        "cat-visible",
        &spec,
        &[
            "cat".to_owned(),
            "--envelope".to_owned(),
            envelope_text.clone(),
            "/visible".to_owned(),
        ],
        Duration::from_secs(60),
    )?;
    set.process(&visible_cat);
    set.check(
        "the CLI reads the granted object with the envelope",
        visible_cat.succeeded() && visible_cat.stdout_tail.contains("visible-payload"),
        format!("exit={}", visible_cat.exit),
    );
    let secret_cat = court.run_cli(
        &dir,
        "cat-secret",
        &spec,
        &[
            "cat".to_owned(),
            "--envelope".to_owned(),
            envelope_text,
            "/secret".to_owned(),
        ],
        Duration::from_secs(60),
    )?;
    set.process(&secret_cat);
    set.check(
        "the CLI denies the ungranted object with the non-disclosing error",
        !secret_cat.succeeded()
            && secret_cat.stderr_tail.contains("NotFound: no such object")
            && secret_cat.stdout_tail.is_empty(),
        format!(
            "exit={} stderr={:?}",
            secret_cat.exit, secret_cat.stderr_tail
        ),
    );
    let verification = verify_spec(&spec, "content hash")?;
    set.set_verification(verification.clone());
    set.check(
        "verification is clean",
        verification["ok"] == json!(true),
        format!("problems={:?}", verification["problems"]),
    );
    Ok(set.finish())
}

fn run_s9_split(court: &mut Court) -> Result<Value, String> {
    let crash = crash_publication(
        court,
        9,
        "S9a",
        "atomic publication under a process fault (s3 blobs / local commits)",
        Layout::SplitS3BlobsLocalCommits,
        5,
    )?;
    let readers = concurrent_readers(
        court,
        10,
        "S9b",
        "concurrent readers during commits (s3 blobs / local commits)",
        Layout::SplitS3BlobsLocalCommits,
    )?;
    let stale = stale_reader(
        court,
        11,
        "S9c",
        "stale reader until reopen (s3 blobs / local commits)",
        Layout::SplitS3BlobsLocalCommits,
    )?;

    let mut set = CheckSet::new(
        court,
        9,
        "S9",
        "split layout: publication, concurrent readers, stale reader",
    )?;
    set.check(
        "S9a publication under a fault matches the single-backend contract",
        crash["passed"] == json!(true),
        format!(
            "old={} new={} partial={} runs={}",
            crash["details"]["old_count"],
            crash["details"]["new_count"],
            crash["details"]["partial_count"],
            crash["details"]["runs"]
        ),
    );
    set.absorb(&crash);
    set.check(
        "S9b concurrent readers keep the pinned bytes on the split layout",
        readers["passed"] == json!(true),
        format!("total_reads={}", readers["details"]["total_reads"]),
    );
    set.absorb(&readers);
    set.check(
        "S9c a stale reader on the split layout waits for reopen",
        stale["passed"] == json!(true),
        format!(
            "stale_reads={} fresh_reads={}",
            stale["details"]["stale_reads"], stale["details"]["fresh_reads"]
        ),
    );
    set.absorb(&stale);
    set.set(
        "subcourts",
        json!({
            "S9a": {"passed": crash["passed"], "checks_passed": crash["checks_passed"], "checks_failed": crash["checks_failed"], "details": crash["details"], "log": crash["log"]},
            "S9b": {"passed": readers["passed"], "checks_passed": readers["checks_passed"], "checks_failed": readers["checks_failed"], "details": readers["details"], "log": readers["log"]},
            "S9c": {"passed": stale["passed"], "checks_passed": stale["checks_passed"], "checks_failed": stale["checks_failed"], "details": stale["details"], "log": stale["log"]},
        }),
    );
    set.set_verification(readers["verification"].clone());
    Ok(set.finish())
}
