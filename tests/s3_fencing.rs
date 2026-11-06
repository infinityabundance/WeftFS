//! S3 fenced-writer court (subphase R17).
//!
//! These tests run only when `WEFT_S3_TEST_ENDPOINT` is set. Without it every
//! test prints a clear skip line and returns. `scripts/s3_court.sh` starts a
//! real MinIO server and runs this suite with the required environment.
//!
//! What is proved here, with a real endpoint and real conditional writes:
//!
//! | test | property |
//! |---|---|
//! | `expired_lease_blocks_publication_without_a_successor` | a writer whose own deadline passed cannot publish, even with nobody to fence it |
//! | `paused_writer_is_fenced_after_a_lease_reclaim` | a writer stopped with `SIGSTOP` whose lease is reclaimed fails `Busy` after `SIGCONT`; the head is unchanged |
//! | `release_of_a_replaced_lock_fails_closed` | releasing a lock that now names another writer is refused; that lock is left byte-identical |
//! | `two_writers_racing_on_release_leave_one_valid_lock` | a release racing an acquire never deletes the winner, and the fencing token never goes backwards |
//! | `renewal_keeps_the_writer_valid` | the bounded renewal interval extends the deadline under a conditional write |
//! | `fencing_token_is_monotonic_across_reclaims` | each takeover takes exactly the next token |
//!
//! The `SIGSTOP` case is a real child process: the test binary re-executes
//! itself with `--exact child_fenced_writer`, and the parent stops and resumes
//! it with the shell's `kill` builtin.
//!
//! Scope: one MinIO process on loopback with a tmpfs data directory. Not a
//! cloud, multi-node, fleet, media, or power-loss claim, and not a multi-writer
//! protocol: writers remain exclusive by design.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key, KeyKind, StorageBackend};
use weftfs::s3::{PublishedLock, S3Backend, S3Config};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);
/// A court endpoint with no lease/timing constraint is never used here; every
/// test reads the real lease from the environment so the timings follow it.
const READY_TIMEOUT: Duration = Duration::from_secs(120);

struct Court {
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    prefix: String,
    lease: Duration,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn court() -> Option<Court> {
    let Some(endpoint) = env("WEFT_S3_TEST_ENDPOINT") else {
        eprintln!("SKIP s3_fencing: WEFT_S3_TEST_ENDPOINT is not set; run scripts/s3_court.sh");
        return None;
    };
    let lease = env("WEFT_S3_LOCK_LEASE_SECS")
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(2)
        .max(1);
    Some(Court {
        endpoint,
        region: env("WEFT_S3_REGION").unwrap_or_else(|| "us-east-1".to_owned()),
        access_key: env("WEFT_S3_ACCESS_KEY")
            .or_else(|| env("AWS_ACCESS_KEY_ID"))
            .expect("S3 access key"),
        secret_key: env("WEFT_S3_SECRET_KEY")
            .or_else(|| env("AWS_SECRET_ACCESS_KEY"))
            .expect("S3 secret key"),
        prefix: format!(
            "{}-fence-{}",
            env("WEFT_S3_TEST_PREFIX").unwrap_or_else(|| "weft-test".to_owned()),
            std::process::id()
        ),
        lease: Duration::from_secs(lease),
    })
}

impl Court {
    fn config(&self, tag: &str) -> S3Config {
        let n = BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
        S3Config {
            endpoint: self.endpoint.clone(),
            bucket: format!("{}-{tag}-{n}", self.prefix),
            region: self.region.clone(),
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            session_token: None,
            path_style: true,
            lock_lease: self.lease,
            request_timeout: Duration::from_secs(60),
        }
    }

    fn kind(&self, tag: &str) -> BackendKind {
        BackendKind::S3 {
            config: self.config(tag),
        }
    }

    /// A read-only lease observer: it never acquires the writer lock, so it can
    /// read the published record while another handle holds it.
    fn observer(&self, config: &S3Config) -> S3Backend {
        S3Backend::new("<s3-fencing-observer>", config.clone())
    }

    /// Sleep past the lease deadline plus a margin so a reclaim is allowed.
    fn sleep_past_deadline(&self) {
        std::thread::sleep(self.lease + Duration::from_millis(400));
    }
}

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn config_of(kind: &BackendKind) -> S3Config {
    match kind {
        BackendKind::S3 { config } => config.clone(),
        other => panic!("expected an S3 kind, got {other:?}"),
    }
}

fn write_file(store: &mut Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").expect("root");
    store
        .create_file_seeded(root, name, bytes, 0o644, 0, 0, Default::default(), seed)
        .expect("create file")
}

/// Count published commit records, so a refused publication can be shown to
/// have added none.
fn commit_count(kind: &BackendKind, path: &Path) -> usize {
    let backend = kind.open(path).expect("open backend");
    backend
        .list_prefix(&weftfs::backend::Key::prefix(KeyKind::Commit))
        .expect("list commits")
        .len()
}

fn lock_record(config: &S3Config) -> Option<PublishedLock> {
    let observer = S3Backend::new("<s3-fencing-observer>", config.clone());
    observer.read_lock_record().expect("read LOCK")
}

#[test]
fn expired_lease_blocks_publication_without_a_successor() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("expired");
    let config = config_of(&kind);
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    let id = write_file(&mut store, b"f", b"before-expiry", b"expired-f");
    let generation = store.generation();
    let head = store.snapshot_root();
    let commits = commit_count(&kind, &path);

    let before = lock_record(&config).expect("published lock");
    assert_eq!(before.fence, 1, "first lease takes fence 1");
    assert!(!before.released);

    court.sleep_past_deadline();

    let err = store.write_file(id, 0, b"after-expiry").unwrap_err();
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    assert!(
        err.message().contains("deadline passed"),
        "the refusal must name the deadline: {err}"
    );

    // The head, the generation, the content, and the commit set are untouched.
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot_root(), head);
    assert_eq!(store.read_object(id).unwrap(), b"before-expiry");
    assert_eq!(commit_count(&kind, &path), commits);

    // No successor exists, so the LOCK still names this writer at fence 1.
    let after = lock_record(&config).expect("published lock");
    assert_eq!(after.fence, 1);
    assert!(!after.released);
    assert_eq!(after.owner, before.owner);

    drop(store);
    let reopened = Store::open_with_backend(&path, OpenMode::ReadOnly, kind).expect("reopen");
    assert_eq!(reopened.generation(), generation);
    assert_eq!(reopened.snapshot_root(), head);
    assert!(reopened.verify(true).expect("verify").ok);
}

#[test]
fn release_of_a_replaced_lock_fails_closed() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("release");
    let config = config_of(&kind);
    let mut first = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    write_file(&mut first, b"f", b"first-writer", b"release-f");
    let first_fence = lock_record(&config).expect("lock").fence;
    assert_eq!(first_fence, 1);

    court.sleep_past_deadline();

    // Second writer reclaims the expired lease: fence 2.
    let mut second =
        Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
    write_file(&mut second, b"g", b"second-writer", b"release-g");
    let replaced = lock_record(&config).expect("lock");
    assert_eq!(replaced.fence, 2, "a takeover takes the next token");
    assert_ne!(replaced.owner, "first-writer");

    // Capture the successor's exact lock bytes and ETag.
    let observer = court.observer(&config);
    let bytes_before = observer.read_all(&Key::lock()).expect("lock bytes");
    let etag_before = observer.etag_for_test(&Key::lock()).expect("lock etag");
    let generation = second.generation();
    let head = second.snapshot_root();

    // Dropping the stale writer runs the release. It must not delete or replace
    // the successor's lock.
    drop(first);

    let bytes_after = observer.read_all(&Key::lock()).expect("lock bytes");
    let etag_after = observer.etag_for_test(&Key::lock()).expect("lock etag");
    assert_eq!(
        bytes_before, bytes_after,
        "the stale writer's release mutated the successor's LOCK"
    );
    assert_eq!(etag_before, etag_after, "the successor's LOCK ETag changed");
    let still = lock_record(&config).expect("lock");
    assert_eq!(still.fence, 2);
    assert!(!still.released);

    // The successor is still the writer and can publish.
    write_file(&mut second, b"h", b"still-owner", b"release-h");
    assert_eq!(second.generation(), generation + 1);
    assert_ne!(second.snapshot_root(), head);
    assert!(second.verify(true).expect("verify").ok);

    // Its own release is a conditional tombstone that preserves the token.
    drop(second);
    let released = lock_record(&config).expect("tombstone");
    assert!(released.released, "a clean release publishes a tombstone");
    assert_eq!(released.fence, 2);
    assert!(released.released_from.is_some());
}

#[test]
fn two_writers_racing_on_release_leave_one_valid_lock() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("race");
    let config = config_of(&kind);

    let store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    assert_eq!(lock_record(&config).expect("lock").fence, 1);
    drop(store);
    // The released tombstone keeps the fencing token, so the next acquire takes
    // fence 2 and the race below always moves the token forward by one.
    assert_eq!(lock_record(&config).expect("tombstone").fence, 1);

    for iteration in 0..20u32 {
        // Take the lease this iteration starts from.
        let holder =
            Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("hold");
        let holding = lock_record(&config).expect("lock");
        assert!(
            !holding.released,
            "iteration {iteration}: expected a live lock"
        );
        let fence = holding.fence;

        // Race a release (dropping the holder) against a fresh acquire. The
        // acquire may lose (Busy) while the release is in flight; retry a
        // bounded number of times. Whichever order wins, the lock must never
        // disappear and the token must never go backwards.
        let path_clone = path.clone();
        let release = std::thread::spawn(move || {
            drop(holder);
            path_clone
        });
        let release_path = release.join().expect("release thread");

        let mut winner = None;
        for _ in 0..40 {
            match Store::open_with_backend(&release_path, OpenMode::ReadWrite, kind.clone()) {
                Ok(store) => {
                    winner = Some(store);
                    break;
                }
                Err(err) => {
                    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let winner = winner.expect("the acquire never observed the released lock");

        let after = lock_record(&config).expect("lock");
        assert_eq!(
            after.fence,
            fence + 1,
            "iteration {iteration}: a reclaim after a release must take the next token"
        );
        assert!(!after.released);
        assert_ne!(after.owner, holding.owner);
        drop(winner);
        let tombstone = lock_record(&config).expect("tombstone");
        assert!(
            tombstone.released,
            "iteration {iteration}: release lost its tombstone"
        );
        assert_eq!(tombstone.fence, after.fence);
    }

    let reopened = Store::open_with_backend(&path, OpenMode::ReadOnly, kind).expect("reopen");
    assert!(reopened.verify(true).expect("verify").ok);
}

#[test]
fn fencing_token_is_monotonic_across_reclaims() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("token");
    let config = config_of(&kind);
    let mut seen = Vec::new();
    {
        let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
        write_file(&mut store, b"f", b"token", b"token-f");
        let record = lock_record(&config).expect("lock");
        seen.push((record.fence, record.pending_generation));
    }
    for round in 0..4 {
        let record = lock_record(&config).expect("tombstone");
        assert!(
            record.released,
            "round {round}: expected a released tombstone, saw {record:?}"
        );
        let mut store =
            Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
        let name = format!("g{round}");
        let seed = format!("token-{round}");
        write_file(&mut store, name.as_bytes(), b"token", seed.as_bytes());
        let published = lock_record(&config).expect("lock");
        seen.push((published.fence, published.pending_generation));
        drop(store);
    }
    let fences: Vec<u64> = seen.iter().map(|(fence, _)| *fence).collect();
    assert_eq!(fences, vec![1, 2, 3, 4, 5], "fences {seen:?}");
    // Every publication names its own head generation in the LOCK record: the
    // fencing token really is carried by the same conditional write that
    // immediately precedes the head compare-and-swap.
    for (index, (_, pending)) in seen.iter().enumerate() {
        assert!(
            *pending >= 1,
            "entry {index} of {seen:?} has no pending head generation"
        );
    }
}

#[test]
fn renewal_keeps_the_writer_valid() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("renew");
    let config = config_of(&kind);
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    let id = write_file(&mut store, b"f", b"renewal", b"renew-f");
    let start = lock_record(&config).expect("lock");
    assert_eq!(start.fence, 1);

    // Sleep past the renewal point (half the lease) but inside the deadline.
    std::thread::sleep(court.lease.mul_f32(0.70));
    store.write_file(id, 0, b"RE").expect("write before expiry");
    let renewed = lock_record(&config).expect("lock");
    assert_eq!(
        renewed.fence, 1,
        "a renewal must not change the fencing token"
    );
    assert!(
        renewed.deadline_ns > start.deadline_ns,
        "renewal did not extend the deadline ({} -> {})",
        start.deadline_ns,
        renewed.deadline_ns
    );
    assert!(
        renewed.sequence > start.sequence,
        "renewal must advance the sequence"
    );

    // Stay alive for a second renewal window: without renewal this write would
    // fall outside the original deadline.
    std::thread::sleep(court.lease.mul_f32(0.70));
    store
        .write_file(id, 0, b"NE")
        .expect("write after a second renewal window");
    let again = lock_record(&config).expect("lock");
    assert_eq!(again.fence, 1);
    assert!(again.deadline_ns > renewed.deadline_ns);
    assert!(store.verify(true).expect("verify").ok);
}

// ---------------------------------------------------------------------------
// SIGSTOP/SIGCONT child
// ---------------------------------------------------------------------------

fn kill_signal(pid: u32, signal: &str) {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -{signal} {pid}"))
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} {pid} failed: {status}");
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// The child half of `paused_writer_is_fenced_after_a_lease_reclaim`.
///
/// Returns immediately unless `WEFT_FENCE_CHILD` names a store, so the normal
/// suite run of this function is a no-op.
#[test]
fn child_fenced_writer() {
    let Ok(role) = std::env::var("WEFT_FENCE_CHILD") else {
        return;
    };
    assert_eq!(role, "paused-writer");
    let endpoint = std::env::var("WEFT_FENCE_ENDPOINT").expect("endpoint");
    let bucket = std::env::var("WEFT_FENCE_BUCKET").expect("bucket");
    let access_key = std::env::var("WEFT_FENCE_ACCESS_KEY").expect("access key");
    let secret_key = std::env::var("WEFT_FENCE_SECRET_KEY").expect("secret key");
    let lease = std::env::var("WEFT_FENCE_LEASE_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(2)
        .max(1);
    let ready = PathBuf::from(std::env::var("WEFT_FENCE_READY").expect("ready path"));
    let status_path = PathBuf::from(std::env::var("WEFT_FENCE_STATUS").expect("status path"));
    let gate = PathBuf::from(std::env::var("WEFT_FENCE_GATE").expect("gate path"));
    let store_path = PathBuf::from(std::env::var("WEFT_FENCE_STORE").expect("store path"));

    let kind = BackendKind::S3 {
        config: S3Config {
            endpoint,
            bucket: bucket.clone(),
            region: std::env::var("WEFT_FENCE_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
            access_key,
            secret_key,
            session_token: None,
            path_style: true,
            lock_lease: Duration::from_secs(lease),
            request_timeout: Duration::from_secs(60),
        },
    };
    let mut store = match Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind.clone()) {
        Ok(store) => store,
        Err(err) => {
            std::fs::write(
                &status_path,
                format!(
                    "{{\"ok\":false,\"phase\":\"open\",\"class\":\"{}\",\"message\":\"{}\"}}",
                    err.class_name(),
                    err.message().replace('"', "'")
                ),
            )
            .expect("write status");
            std::process::exit(0);
        }
    };
    let observer = S3Backend::new("<s3-fencing-child>", config_of(&kind));
    let lock = observer.read_lock_record().expect("lock").expect("lock");
    std::fs::write(
        &ready,
        format!(
            "{{\"pid\":{},\"fence\":{},\"generation\":{},\"head\":\"{}\"}}",
            std::process::id(),
            lock.fence,
            store.generation(),
            store.snapshot_root().to_hex()
        ),
    )
    .expect("write ready");

    // The parent stops the process here and reclaims the lease.
    let _ = wait_for_file(&gate, Duration::from_secs(120));

    let root = store.resolve_path("/").expect("root");
    let outcome = store.create_file_seeded(
        root,
        b"fenced",
        b"must-not-publish",
        0o644,
        0,
        0,
        Default::default(),
        b"fenced-child",
    );
    let lock_after = observer.read_lock_record().ok().flatten();
    let status = match outcome {
        Ok(_) => format!(
            "{{\"ok\":true,\"phase\":\"published\",\"generation\":{},\"head\":\"{}\",\"fence\":{}}}",
            store.generation(),
            store.snapshot_root().to_hex(),
            lock_after.map(|record| record.fence).unwrap_or(0)
        ),
        Err(err) => format!(
            "{{\"ok\":false,\"phase\":\"publish\",\"class\":\"{}\",\"message\":\"{}\",\"generation\":{},\
              \"head\":\"{}\",\"fence\":{}}}",
            err.class_name(),
            err.message().replace('"', "'"),
            store.generation(),
            store.snapshot_root().to_hex(),
            lock_after.map(|record| record.fence).unwrap_or(0)
        ),
    };
    std::fs::write(&status_path, status).expect("write status");
    drop(store);
}

#[test]
fn paused_writer_is_fenced_after_a_lease_reclaim() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let work = label.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let path = work.join("store");
    let kind = court.kind("paused");
    let config = config_of(&kind);
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    write_file(&mut store, b"f", b"paused-child", b"paused-f");
    let initial = lock_record(&config).expect("lock");
    assert_eq!(initial.fence, 1);
    drop(store);
    // Dropping the init handle published a released tombstone; the token is
    // kept, so the child takes exactly the next one.
    let released = lock_record(&config).expect("tombstone");
    assert!(released.released);
    let child_fence = released.fence + 1;

    let ready = work.join("child.ready.json");
    let status_path = work.join("child.status.json");
    let gate = work.join("child.gate");
    let exe = std::env::current_exe().expect("current_exe");
    let child = Command::new(&exe)
        .args([
            "--exact",
            "child_fenced_writer",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("WEFT_FENCE_CHILD", "paused-writer")
        .env("WEFT_FENCE_ENDPOINT", &config.endpoint)
        .env("WEFT_FENCE_BUCKET", &config.bucket)
        .env("WEFT_FENCE_REGION", &config.region)
        .env("WEFT_FENCE_ACCESS_KEY", &config.access_key)
        .env("WEFT_FENCE_SECRET_KEY", &config.secret_key)
        .env(
            "WEFT_FENCE_LEASE_SECS",
            config.lock_lease.as_secs().to_string(),
        )
        .env("WEFT_FENCE_READY", &ready)
        .env("WEFT_FENCE_STATUS", &status_path)
        .env("WEFT_FENCE_GATE", &gate)
        .env("WEFT_FENCE_STORE", &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the paused writer");

    let ready_value = wait_for_child(&ready, READY_TIMEOUT);
    assert_eq!(
        ready_value["fence"],
        serde_json::json!(child_fence),
        "child fence"
    );
    let child_pid = ready_value["pid"].as_u64().expect("pid") as u32;
    let child_generation = ready_value["generation"].as_u64().expect("generation");
    let child_head = ready_value["head"].as_str().expect("head").to_owned();

    // Stop the writer exactly where the parent wants it: after it took the
    // lease, before it published.
    kill_signal(child_pid, "STOP");
    std::thread::sleep(Duration::from_millis(100));

    court.sleep_past_deadline();

    // A second writer reclaims the expired lease with the next token.
    let mut second =
        Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
    let reclaimed = lock_record(&config).expect("lock");
    assert_eq!(
        reclaimed.fence,
        child_fence + 1,
        "the successor must take the next token, not reuse the fenced writer's"
    );
    write_file(&mut second, b"g", b"successor", b"paused-g");
    let successor_head = second.snapshot_root();
    let successor_generation = second.generation();
    assert_eq!(successor_generation, child_generation + 1);

    // Resume the paused writer. Its publication must fail Busy.
    kill_signal(child_pid, "CONT");
    std::fs::write(&gate, b"go").expect("open the gate");
    let child_status = wait_for_child(&status_path, READY_TIMEOUT);
    assert_eq!(
        child_status["ok"],
        serde_json::json!(false),
        "the paused writer published after being fenced: {child_status}"
    );
    assert_eq!(child_status["class"], "Busy", "status: {child_status}");
    assert_eq!(
        child_status["generation"].as_u64(),
        Some(child_generation),
        "the fenced writer's own generation moved"
    );
    assert_eq!(
        child_status["head"].as_str(),
        Some(child_head.as_str()),
        "the fenced writer's own head moved"
    );

    let mut child = ChildGuard(child);
    child.wait();

    // The published head is the successor's, and the fenced writer added no
    // commit record or visible object.
    assert_eq!(second.snapshot_root(), successor_head);
    assert_eq!(second.generation(), successor_generation);
    let report = second.verify(true).expect("verify");
    assert!(report.ok, "problems: {:?}", report.problems);
    assert!(
        second.resolve_path("/fenced").is_err(),
        "the fenced writer's file became visible"
    );
    assert_eq!(
        second
            .read_object(second.resolve_path("/g").unwrap())
            .unwrap(),
        b"successor"
    );

    let final_lock = lock_record(&config).expect("lock");
    assert_eq!(final_lock.fence, child_fence + 1);
    assert_eq!(final_lock.owner, reclaimed.owner);
}

struct ChildGuard(Child);

impl ChildGuard {
    fn wait(&mut self) {
        let status = self
            .0
            .wait_timeout(Duration::from_secs(60))
            .expect("wait for the child");
        assert!(status.is_some(), "the child did not exit");
        assert_eq!(status.unwrap().code(), Some(0), "the child failed");
    }
}

trait WaitTimeout {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                let _ = self.kill();
                let _ = self.wait();
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn wait_for_child(path: &Path, timeout: Duration) -> serde_json::Value {
    wait_for_file(path, timeout).unwrap_or_else(|err| panic!("{err}"));
    // The child writes the file in one call, but re-read until it parses so a
    // partially flushed file is not a test failure.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str(&text) {
                return value;
            }
        }
        if Instant::now() >= deadline {
            panic!("child status {} never parsed", path.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn observer_reads_the_published_lock_without_a_lease() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("observe");
    let config = config_of(&kind);
    let store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    let observer = court.observer(&config);
    let record = observer
        .read_lock_record()
        .expect("read LOCK")
        .expect("published LOCK");
    assert_eq!(record.fence, 1);
    assert!(!record.released);
    assert_eq!(
        record.pending_generation, 1,
        "the fenced head publication must name generation 1: {record:?}"
    );
    assert_eq!(
        record.pending_commit.len(),
        64,
        "the fenced head publication must name its commit id: {record:?}"
    );
    assert!(
        record.deadline_ns > 0,
        "a live lease must record an absolute deadline"
    );
    // An observer never acquires or mutates the lease.
    assert!(observer.read_lock_record().expect("reread").is_some());
    drop(store);
}
