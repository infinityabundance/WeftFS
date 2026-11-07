//! Backend-agnostic fenced-writer invariants (subphase R18).
//!
//! The same test bodies run against every remoted backend that implements the
//! fenced writer protocol: the S3 backend (MinIO in `scripts/s3_court.sh`, Ceph
//! RGW in `scripts/ceph_court.sh`) and the GCS backend (`fake-gcs-server` in
//! `bench/scripts/gcs_court.sh`). Only the conditional primitive differs, so
//! only the court configuration and the observer hooks are backend-specific;
//! the assertions are literally shared.
//!
//! Invariants:
//!
//! | body | property |
//! |---|---|
//! | `expired_lease_blocks_publication_without_a_successor` | a writer whose own deadline passed cannot publish, even with nobody to fence it |
//! | `paused_writer_is_fenced_after_a_lease_reclaim` | a writer stopped with `SIGSTOP` whose lease is reclaimed fails `Busy` after `SIGCONT`; its head and generation are unchanged |
//! | `release_of_a_replaced_lock_fails_closed` | releasing a lock that now names another writer is refused; the successor's bytes and conditional token are untouched |
//! | `renewal_keeps_the_writer_valid` | the bounded renewal extends the deadline under the same conditional write, fence constant |
//! | `fencing_token_is_monotonic_across_reclaims` | each takeover takes exactly the next token and names its head |
//!
//! This module is test support, not a court. It never decides whether a court
//! is available; the including test file skips with its own explicit line when
//! the endpoint environment is absent.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;

use weftfs::azure::AzureBackend;
use weftfs::backend::{BackendKind, Key, KeyKind, StorageBackend};
use weftfs::gcs::GcsBackend;
use weftfs::s3::S3Backend;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::writer_fence::PublishedLock;
use weftfs::Hash32;

/// A court that can hand out fresh stores on one remoted backend.
pub trait FenceCourt {
    /// Short backend label used in assertion messages.
    fn label(&self) -> &'static str;
    /// A fresh store namespace. Every call must use a distinct bucket, because
    /// a store root is the whole bucket.
    fn new_store(&self, tag: &str) -> BackendKind;
    /// The lease these tests run with. A court expireable in a test uses a
    /// short one; the general suites use a long one.
    fn lease_duration(&self) -> Duration;
    /// Environment a re-executed child needs to open the same backend.
    fn child_env(&self, kind: &BackendKind) -> Vec<(String, String)>;

    /// Sleep past the lease deadline plus a margin so a reclaim is allowed.
    fn sleep_past_deadline(&self) {
        std::thread::sleep(self.lease_duration() + Duration::from_millis(400));
    }
}

pub const READY_TIMEOUT: Duration = Duration::from_secs(180);

pub fn opts() -> Options {
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

pub fn write_file(store: &mut Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").expect("root");
    store
        .create_file_seeded(root, name, bytes, 0o644, 0, 0, Default::default(), seed)
        .expect("create file")
}

/// The same write, returning the error instead of panicking.
pub fn write_file_result(
    store: &mut Store,
    name: &[u8],
    bytes: &[u8],
    seed: &[u8],
) -> weftfs::error::Result<Hash32> {
    let root = store.resolve_path("/")?;
    store.create_file_seeded(root, name, bytes, 0o644, 0, 0, Default::default(), seed)
}

/// Count published commit records, so a refused publication can be shown to
/// have added none.
pub fn commit_count(kind: &BackendKind, path: &Path) -> usize {
    let backend = kind.open(path).expect("open backend");
    backend
        .list_prefix(&Key::prefix(KeyKind::Commit))
        .expect("list commits")
        .len()
}

/// A read-only lease observer: it never acquires the writer lock, so it can
/// read the published record while another handle holds it.
pub fn read_lock(kind: &BackendKind) -> Option<PublishedLock> {
    match kind {
        BackendKind::S3 { config } => S3Backend::new("<fence-observer>", config.clone())
            .read_lock_record()
            .expect("read LOCK"),
        BackendKind::Gcs { config } => GcsBackend::new("<fence-observer>", config.clone())
            .read_lock_record()
            .expect("read LOCK"),
        BackendKind::Azure { config } => AzureBackend::new("<fence-observer>", config.clone())
            .read_lock_record()
            .expect("read LOCK"),
        other => panic!("a fencing court needs an s3, gcs, or azure backend, got {other:?}"),
    }
}

/// The published lock bytes and the conditional token (`ETag` for S3, GCS
/// generation for GCS) that the next conditional write must name. Release of a
/// replaced lock must leave both exactly as the successor left them.
pub fn lock_bytes_and_token(kind: &BackendKind) -> (Vec<u8>, String) {
    match kind {
        BackendKind::S3 { config } => {
            let observer = S3Backend::new("<fence-observer>", config.clone());
            let bytes = observer.read_all(&Key::lock()).expect("LOCK bytes");
            let token = observer.etag_for_test(&Key::lock()).expect("LOCK ETag");
            (bytes, format!("etag:{token}"))
        }
        BackendKind::Gcs { config } => {
            let observer = GcsBackend::new("<fence-observer>", config.clone());
            let bytes = observer.read_all(&Key::lock()).expect("LOCK bytes");
            let generation = observer
                .generation_for_test(&Key::lock())
                .expect("LOCK metadata")
                .expect("LOCK exists");
            (bytes, format!("generation:{generation}"))
        }
        BackendKind::Azure { config } => {
            let observer = AzureBackend::new("<fence-observer>", config.clone());
            let bytes = observer.read_all(&Key::lock()).expect("LOCK bytes");
            let etag = observer.etag_for_test(&Key::lock()).expect("LOCK ETag");
            (bytes, format!("etag:{etag}"))
        }
        other => panic!("a fencing court needs an s3, gcs, or azure backend, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Shared test bodies
// ---------------------------------------------------------------------------

pub fn expired_lease_blocks_publication_without_a_successor(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.new_store("expired");
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    let id = write_file(&mut store, b"f", b"before-expiry", b"expired-f");
    let generation = store.generation();
    let head = store.snapshot_root();
    let commits = commit_count(&kind, &path);

    let before = read_lock(&kind).expect("published lock");
    assert_eq!(before.fence, 1, "first lease takes fence 1");
    assert!(!before.released);

    court.sleep_past_deadline();

    let err = store.write_file(id, 0, b"after-expiry").unwrap_err();
    assert_eq!(
        err.class_name(),
        "Busy",
        "{}: unexpected error: {err}",
        court.label()
    );
    assert!(
        err.message().contains("deadline passed"),
        "{}: the refusal must name the deadline: {err}",
        court.label()
    );

    // The head, the generation, the content, and the commit set are untouched.
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot_root(), head);
    assert_eq!(store.read_object(id).unwrap(), b"before-expiry");
    assert_eq!(commit_count(&kind, &path), commits);

    // No successor exists, so the LOCK still names this writer at fence 1.
    let after = read_lock(&kind).expect("published lock");
    assert_eq!(after.fence, 1);
    assert!(!after.released);
    assert_eq!(after.owner, before.owner);

    drop(store);
    let reopened = Store::open_with_backend(&path, OpenMode::ReadOnly, kind).expect("reopen");
    assert_eq!(reopened.generation(), generation);
    assert_eq!(reopened.snapshot_root(), head);
    assert!(reopened.verify(true).expect("verify").ok);
}

pub fn release_of_a_replaced_lock_fails_closed(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.new_store("release");
    let mut first = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    write_file(&mut first, b"f", b"first-writer", b"release-f");
    let first_lock = read_lock(&kind).expect("lock");
    let first_fence = first_lock.fence;
    let first_owner = first_lock.owner.clone();
    assert_eq!(first_fence, 1);

    court.sleep_past_deadline();

    // Second writer reclaims the expired lease: fence 2.
    let mut second =
        Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
    write_file(&mut second, b"g", b"second-writer", b"release-g");
    let replaced = read_lock(&kind).expect("lock");
    assert_eq!(
        replaced.fence,
        2,
        "{}: a takeover takes the next token",
        court.label()
    );
    assert_ne!(replaced.owner, first_owner);

    // Capture the successor's exact lock bytes and conditional token.
    let (bytes_before, token_before) = lock_bytes_and_token(&kind);
    let generation = second.generation();
    let head = second.snapshot_root();

    // Dropping the stale writer runs the release. It must not delete or replace
    // the successor's lock.
    drop(first);

    let (bytes_after, token_after) = lock_bytes_and_token(&kind);
    assert_eq!(
        bytes_before,
        bytes_after,
        "{}: the stale writer's release mutated the successor's LOCK",
        court.label()
    );
    assert_eq!(
        token_before,
        token_after,
        "{}: the successor's LOCK token changed",
        court.label()
    );
    let still = read_lock(&kind).expect("lock");
    assert_eq!(still.fence, 2);
    assert!(!still.released);

    // The successor is still the writer and can publish.
    write_file(&mut second, b"h", b"still-owner", b"release-h");
    assert_eq!(second.generation(), generation + 1);
    assert_ne!(second.snapshot_root(), head);
    assert!(second.verify(true).expect("verify").ok);

    // Its own release is a conditional tombstone that preserves the token.
    drop(second);
    let released = read_lock(&kind).expect("tombstone");
    assert!(
        released.released,
        "{}: a clean release publishes a tombstone",
        court.label()
    );
    assert_eq!(released.fence, 2);
    assert!(released.released_from.is_some());
}

pub fn renewal_keeps_the_writer_valid(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.new_store("renew");
    let lease = court.lease_duration();
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    let id = write_file(&mut store, b"f", b"renewal", b"renew-f");
    let start = read_lock(&kind).expect("lock");
    assert_eq!(start.fence, 1);

    // Sleep past the renewal point (half the lease) but inside the deadline.
    std::thread::sleep(lease.mul_f32(0.70));
    store.write_file(id, 0, b"RE").expect("write before expiry");
    let renewed = read_lock(&kind).expect("lock");
    assert_eq!(
        renewed.fence,
        1,
        "{}: a renewal must not change the fencing token",
        court.label()
    );
    assert!(
        renewed.deadline_ns > start.deadline_ns,
        "{}: renewal did not extend the deadline ({} -> {})",
        court.label(),
        start.deadline_ns,
        renewed.deadline_ns
    );
    assert!(
        renewed.sequence > start.sequence,
        "{}: renewal must advance the sequence",
        court.label()
    );

    // Stay alive for a second renewal window: without renewal this write would
    // fall outside the original deadline.
    std::thread::sleep(lease.mul_f32(0.70));
    store
        .write_file(id, 0, b"NE")
        .expect("write after a second renewal window");
    let again = read_lock(&kind).expect("lock");
    assert_eq!(again.fence, 1);
    assert!(again.deadline_ns > renewed.deadline_ns);
    assert!(store.verify(true).expect("verify").ok);
}

pub fn fencing_token_is_monotonic_across_reclaims(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.new_store("token");
    let mut seen = Vec::new();
    {
        let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
        write_file(&mut store, b"f", b"token", b"token-f");
        let record = read_lock(&kind).expect("lock");
        seen.push((record.fence, record.pending_generation));
    }
    for round in 0..4 {
        let record = read_lock(&kind).expect("tombstone");
        assert!(
            record.released,
            "{}: round {round}: expected a released tombstone, saw {record:?}",
            court.label()
        );
        let mut store =
            Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
        let name = format!("g{round}");
        let seed = format!("token-{round}");
        write_file(&mut store, name.as_bytes(), b"token", seed.as_bytes());
        let published = read_lock(&kind).expect("lock");
        seen.push((published.fence, published.pending_generation));
        drop(store);
    }
    let fences: Vec<u64> = seen.iter().map(|(fence, _)| *fence).collect();
    assert_eq!(
        fences,
        vec![1, 2, 3, 4, 5],
        "{}: fences {seen:?}",
        court.label()
    );
    // Every publication names its own head generation in the LOCK record: the
    // fencing token really is carried by the same conditional write that
    // immediately precedes the head compare-and-swap.
    for (index, (_, pending)) in seen.iter().enumerate() {
        assert!(
            *pending >= 1,
            "{}: entry {index} of {seen:?} has no pending head generation",
            court.label()
        );
    }
}

// ---------------------------------------------------------------------------
// SIGSTOP/SIGCONT child
// ---------------------------------------------------------------------------

pub fn paused_writer_is_fenced_after_a_lease_reclaim(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let work = label.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let path = work.join("store");
    let kind = court.new_store("paused");
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    write_file(&mut store, b"f", b"paused-child", b"paused-f");
    let initial = read_lock(&kind).expect("lock");
    assert_eq!(initial.fence, 1);
    drop(store);
    // Dropping the init handle published a released tombstone; the token is
    // kept, so the child takes exactly the next one.
    let released = read_lock(&kind).expect("tombstone");
    assert!(released.released);
    let child_fence = released.fence + 1;

    let ready = work.join("child.ready.json");
    let status_path = work.join("child.status.json");
    let gate = work.join("child.gate");
    let exe = std::env::current_exe().expect("current_exe");
    let mut command = Command::new(&exe);
    command
        .args([
            "--exact",
            "child_fenced_writer",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("WEFT_FENCE_CHILD", "paused-writer")
        .env("WEFT_FENCE_READY", &ready)
        .env("WEFT_FENCE_STATUS", &status_path)
        .env("WEFT_FENCE_GATE", &gate)
        .env("WEFT_FENCE_STORE", &path)
        .env(
            "WEFT_FENCE_LEASE_SECS",
            court.lease_duration().as_secs().to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in court.child_env(&kind) {
        command.env(name, value);
    }
    let child = command.spawn().expect("spawn the paused writer");

    let ready_value = wait_for_child(&ready, READY_TIMEOUT);
    assert_eq!(
        ready_value["fence"],
        serde_json::json!(child_fence),
        "{}: child fence",
        court.label()
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
    let reclaimed = read_lock(&kind).expect("lock");
    assert_eq!(
        reclaimed.fence,
        child_fence + 1,
        "{}: the successor must take the next token, not reuse the fenced writer's",
        court.label()
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
        "{}: the paused writer published after being fenced: {child_status}",
        court.label()
    );
    assert_eq!(
        child_status["class"],
        "Busy",
        "{}: status: {child_status}",
        court.label()
    );
    assert_eq!(
        child_status["generation"].as_u64(),
        Some(child_generation),
        "{}: the fenced writer's own generation moved",
        court.label()
    );
    assert_eq!(
        child_status["head"].as_str(),
        Some(child_head.as_str()),
        "{}: the fenced writer's own head moved",
        court.label()
    );

    let mut child = ChildGuard(child);
    child.wait();

    // The published head is the successor's, and the fenced writer added no
    // commit record or visible object.
    assert_eq!(second.snapshot_root(), successor_head);
    assert_eq!(second.generation(), successor_generation);
    let report = second.verify(true).expect("verify");
    assert!(
        report.ok,
        "{}: problems: {:?}",
        court.label(),
        report.problems
    );
    assert!(
        second.resolve_path("/fenced").is_err(),
        "{}: the fenced writer's file became visible",
        court.label()
    );
    assert_eq!(
        second
            .read_object(second.resolve_path("/g").unwrap())
            .unwrap(),
        b"successor"
    );

    let final_lock = read_lock(&kind).expect("lock");
    assert_eq!(final_lock.fence, child_fence + 1);
    assert_eq!(final_lock.owner, reclaimed.owner);
}

/// The child half of `paused_writer_is_fenced_after_a_lease_reclaim`.
///
/// Returns immediately unless `WEFT_FENCE_CHILD` is set, so the normal suite
/// run of this function is a no-op.
pub fn run_child(kind: BackendKind) {
    if std::env::var("WEFT_FENCE_CHILD").is_err() {
        return;
    }
    assert_eq!(std::env::var("WEFT_FENCE_CHILD").unwrap(), "paused-writer");
    let ready = PathBuf::from(std::env::var("WEFT_FENCE_READY").expect("ready path"));
    let status_path = PathBuf::from(std::env::var("WEFT_FENCE_STATUS").expect("status path"));
    let gate = PathBuf::from(std::env::var("WEFT_FENCE_GATE").expect("gate path"));
    let store_path = PathBuf::from(std::env::var("WEFT_FENCE_STORE").expect("store path"));

    let mut store = match Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind.clone()) {
        Ok(store) => store,
        Err(err) => {
            write_status(
                &status_path,
                &format!(
                    "{{\"ok\":false,\"phase\":\"open\",\"class\":\"{}\",\"message\":\"{}\"}}",
                    err.class_name(),
                    err.message().replace('"', "'")
                ),
            );
            std::process::exit(0);
        }
    };
    let lock = read_lock(&kind).expect("lock");
    write_status(
        &ready,
        &format!(
            "{{\"pid\":{},\"fence\":{},\"generation\":{},\"head\":\"{}\"}}",
            std::process::id(),
            lock.fence,
            store.generation(),
            store.snapshot_root().to_hex()
        ),
    );

    // The parent stops the process here and reclaims the lease.
    let _ = wait_for_file(&gate, Duration::from_secs(180));

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
    let lock_after = read_lock(&kind);
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
    write_status(&status_path, &status);
    drop(store);
}

/// The inverse sanity test: a writer suspended **after** the fence but
/// **before** the publication must not publish once a successor has reclaimed
/// the lease, even though the successor has not published a head yet.
///
/// This is the interleaving an external review found uncovered in the old
/// two-object protocol (fence on `LOCK`, then a separate head CAS on the
/// superblock). The v3 publication record makes the fence and the head one
/// conditional write, so the takeover invalidates the stale writer's write.
pub fn paused_after_the_fence_cannot_publish_after_a_takeover(court: &dyn FenceCourt) {
    let label = tempdir().unwrap();
    let work = label.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let path = work.join("store");
    let kind = court.new_store("after-fence");
    let mut store = Store::init_with_backend(&path, opts(), kind.clone()).expect("init");
    write_file(&mut store, b"f", b"seed", b"after-fence-seed");
    let initial_generation = store.generation();
    drop(store);
    let released = read_lock(&kind).expect("tombstone");
    assert!(released.released);
    let child_fence = released.fence + 1;

    let ready = work.join("after-fence.ready.json");
    let status_path = work.join("after-fence.status.json");
    let exe = std::env::current_exe().expect("current_exe");
    let mut command = Command::new(&exe);
    command
        .args([
            "--exact",
            "child_fenced_writer_after_fence",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("WEFT_FENCE_CHILD", "after-fence")
        .env("WEFT_FENCE_PAUSE_AFTER_FENCE", "1")
        .env("WEFT_FENCE_AFTER_FENCE_READY", &ready)
        .env("WEFT_FENCE_STATUS", &status_path)
        .env("WEFT_FENCE_STORE", &path)
        .env(
            "WEFT_FENCE_LEASE_SECS",
            court.lease_duration().as_secs().to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in court.child_env(&kind) {
        command.env(name, value);
    }
    let child = command.spawn().expect("spawn the after-fence writer");

    // The hook writes this file after the conditional fence write and then
    // stops itself with SIGSTOP, so the fence is durable and the publication
    // has not started.
    let ready_value = wait_for_child(&ready, READY_TIMEOUT);
    let child_pid = ready_value["pid"].as_u64().expect("pid") as u32;
    let child_generation = ready_value["generation"].as_u64().expect("generation");
    let child_head = ready_value["head"].as_str().expect("head").to_owned();
    assert_eq!(child_generation, initial_generation);
    let fenced = read_lock(&kind).expect("lock");
    assert_eq!(
        fenced.fence,
        child_fence,
        "{}: the child must have fenced before pausing",
        court.label()
    );
    assert_eq!(
        fenced.pending_generation,
        child_generation,
        "{}: the fence record must still attest the published head",
        court.label()
    );

    court.sleep_past_deadline();

    // A successor reclaims the expired lease and **does not publish a head**.
    let mut second =
        Store::open_with_backend(&path, OpenMode::ReadWrite, kind.clone()).expect("reclaim");
    let reclaimed = read_lock(&kind).expect("lock");
    assert_eq!(
        reclaimed.fence,
        child_fence + 1,
        "{}: the successor must take the next token",
        court.label()
    );
    assert_eq!(
        second.generation(),
        child_generation,
        "{}: the takeover itself must not publish a head",
        court.label()
    );
    assert_eq!(
        reclaimed.pending_generation,
        child_generation,
        "{}: the takeover must carry the published head forward",
        court.label()
    );

    // Resume the suspended writer. Its publication must fail Busy.
    kill_signal(child_pid, "CONT");
    let child_status = wait_for_child(&status_path, READY_TIMEOUT);
    assert_eq!(
        child_status["ok"],
        serde_json::json!(false),
        "{}: the after-fence writer published after being fenced: {child_status}",
        court.label()
    );
    assert_eq!(
        child_status["class"],
        "Busy",
        "{}: status: {child_status}",
        court.label()
    );
    assert_eq!(
        child_status["generation"].as_u64(),
        Some(child_generation),
        "{}: the fenced writer's generation moved",
        court.label()
    );
    assert_eq!(
        child_status["head"].as_str(),
        Some(child_head.as_str()),
        "{}: the fenced writer's head moved",
        court.label()
    );
    let message = child_status["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("LOCK now names") || message.contains("taken over"),
        "{}: the refusal must name the takeover, got {message:?}",
        court.label()
    );

    let mut child = ChildGuard(child);
    child.wait();

    // The head is unchanged, and the successor can publish.
    assert_eq!(second.generation(), child_generation);
    assert!(
        second.resolve_path("/fenced").is_err(),
        "{}: the fenced writer's file became visible",
        court.label()
    );
    write_file(&mut second, b"g", b"successor", b"after-fence-successor");
    let successor_head = second.snapshot_root();
    assert_eq!(second.generation(), child_generation + 1);
    assert_eq!(
        second
            .read_object(second.resolve_path("/g").unwrap())
            .unwrap(),
        b"successor",
        "{}: the successor's write is not visible",
        court.label()
    );
    let report = second.verify(true).expect("verify");
    assert!(
        report.ok,
        "{}: problems: {:?}",
        court.label(),
        report.problems
    );
    let final_lock = read_lock(&kind).expect("lock");
    assert_eq!(final_lock.pending_generation, second.generation());
    assert_eq!(final_lock.owner, reclaimed.owner);
    let _ = successor_head;
}

/// The child half of
/// [`paused_after_the_fence_cannot_publish_after_a_takeover`]. The publication
/// hook in `src/fence_pause.rs` writes `WEFT_FENCE_AFTER_FENCE_READY` and then
/// sends itself `SIGSTOP`.
pub fn run_child_after_fence(kind: BackendKind) {
    if std::env::var("WEFT_FENCE_CHILD").is_err() {
        return;
    }
    assert_eq!(std::env::var("WEFT_FENCE_CHILD").unwrap(), "after-fence");
    let status_path = PathBuf::from(std::env::var("WEFT_FENCE_STATUS").expect("status path"));
    let store_path = PathBuf::from(std::env::var("WEFT_FENCE_STORE").expect("store path"));

    let mut store = match Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind) {
        Ok(store) => store,
        Err(err) => {
            write_status(
                &status_path,
                &format!(
                    "{{\"ok\":false,\"phase\":\"open\",\"class\":\"{}\",\"message\":\"{}\"}}",
                    err.class_name(),
                    err.message().replace('"', "'")
                ),
            );
            std::process::exit(0);
        }
    };
    let generation = store.generation();
    let head = store.snapshot_root();
    // Resume order: the hook pauses inside this call, after the fence.
    let root = store.resolve_path("/").expect("root");
    let outcome = store.create_file_seeded(
        root,
        b"fenced",
        b"must-not-publish",
        0o644,
        0,
        0,
        Default::default(),
        b"after-fence-child",
    );
    let status = match outcome {
        Ok(_) => format!(
            "{{\"ok\":true,\"phase\":\"published\",\"generation\":{},\"head\":\"{}\"}}",
            store.generation(),
            store.snapshot_root().to_hex()
        ),
        Err(err) => format!(
            "{{\"ok\":false,\"phase\":\"publish\",\"class\":\"{}\",\"message\":\"{}\",\
              \"generation\":{},\"head\":\"{}\"}}",
            err.class_name(),
            err.message().replace('"', "'"),
            store.generation(),
            store.snapshot_root().to_hex()
        ),
    };
    let _ = (generation, head);
    write_status(&status_path, &status);
    drop(store);
}

fn write_status(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write status");
}

pub fn kill_signal(pid: u32, signal: &str) {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -{signal} {pid}"))
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} {pid} failed: {status}");
}

pub fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

pub struct ChildGuard(pub Child);

impl ChildGuard {
    pub fn wait(&mut self) {
        let status = self
            .0
            .wait_timeout(Duration::from_secs(90))
            .expect("wait for the child");
        assert!(status.is_some(), "the child did not exit");
        assert_eq!(status.unwrap().code(), Some(0), "the child failed");
    }
}

pub trait WaitTimeout {
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

pub fn wait_for_child(path: &Path, timeout: Duration) -> serde_json::Value {
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
