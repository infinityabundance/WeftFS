//! S3 fenced-writer court (subphases R17/R18).
//!
//! These tests run only when `WEFT_S3_TEST_ENDPOINT` is set. Without it every
//! test prints a clear skip line and returns. `scripts/s3_court.sh` (MinIO) and
//! `scripts/ceph_court.sh` (Ceph RGW) both start a real endpoint and run this
//! suite with the required environment.
//!
//! The protocol invariants themselves live in `tests/support/fencing.rs` and
//! are shared verbatim with the GCS court (`tests/gcs_fencing.rs`). What is
//! S3-specific here is only the endpoint/ETag configuration, the
//! release/acquire race, and the observer test.
//!
//! The lease here is deliberately short (`WEFT_S3_FENCE_LEASE_SECS`, 3 s in the
//! court) so a court can let it expire. The general S3 suites use a long lease
//! because a writer that is idle past its deadline must reopen rather than
//! publish.
//!
//! The `SIGSTOP` case is a real child process: the test binary re-executes
//! itself with `--exact child_fenced_writer`, and the parent stops and resumes
//! it with the shell's `kill` builtin.
//!
//! Scope: one endpoint process on loopback with a tmpfs data directory. Not a
//! cloud, multi-node, fleet, media, or power-loss claim, and not a multi-writer
//! protocol: writers remain exclusive by design.

#[path = "support/fencing.rs"]
mod fencing;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tempfile::tempdir;

use weftfs::backend::BackendKind;
use weftfs::s3::{S3Backend, S3Config};
use weftfs::store::{OpenMode, Options, Store};

use fencing::FenceCourt;

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let lease = env("WEFT_S3_FENCE_LEASE_SECS")
        .or_else(|| env("WEFT_S3_LOCK_LEASE_SECS"))
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(3)
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
}

impl FenceCourt for Court {
    fn label(&self) -> &'static str {
        "s3"
    }

    fn new_store(&self, tag: &str) -> BackendKind {
        self.kind(tag)
    }

    fn lease_duration(&self) -> Duration {
        self.lease
    }

    fn child_env(&self, kind: &BackendKind) -> Vec<(String, String)> {
        let BackendKind::S3 { config } = kind else {
            panic!("the S3 court only holds S3 backends");
        };
        vec![
            ("WEFT_FENCE_S3_ENDPOINT".to_owned(), config.endpoint.clone()),
            ("WEFT_FENCE_S3_BUCKET".to_owned(), config.bucket.clone()),
            ("WEFT_FENCE_S3_REGION".to_owned(), config.region.clone()),
            (
                "WEFT_FENCE_S3_ACCESS_KEY".to_owned(),
                config.access_key.clone(),
            ),
            (
                "WEFT_FENCE_S3_SECRET_KEY".to_owned(),
                config.secret_key.clone(),
            ),
        ]
    }
}

fn opts() -> Options {
    fencing::opts()
}

fn lock_record(config: &S3Config) -> Option<weftfs::s3::PublishedLock> {
    let observer = S3Backend::new("<s3-fencing-observer>", config.clone());
    observer.read_lock_record().expect("read LOCK")
}

fn config_of(kind: &BackendKind) -> S3Config {
    match kind {
        BackendKind::S3 { config } => config.clone(),
        other => panic!("expected an S3 kind, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Shared invariant bodies (identical in tests/gcs_fencing.rs)
// ---------------------------------------------------------------------------

#[test]
fn expired_lease_blocks_publication_without_a_successor() {
    let Some(court) = court() else { return };
    fencing::expired_lease_blocks_publication_without_a_successor(&court);
}

#[test]
fn release_of_a_replaced_lock_fails_closed() {
    let Some(court) = court() else { return };
    fencing::release_of_a_replaced_lock_fails_closed(&court);
}

#[test]
fn renewal_keeps_the_writer_valid() {
    let Some(court) = court() else { return };
    fencing::renewal_keeps_the_writer_valid(&court);
}

#[test]
fn fencing_token_is_monotonic_across_reclaims() {
    let Some(court) = court() else { return };
    fencing::fencing_token_is_monotonic_across_reclaims(&court);
}

#[test]
fn paused_writer_is_fenced_after_a_lease_reclaim() {
    let Some(court) = court() else { return };
    fencing::paused_writer_is_fenced_after_a_lease_reclaim(&court);
}

/// The child half of `paused_writer_is_fenced_after_a_lease_reclaim`. Returns
/// immediately unless `WEFT_FENCE_CHILD` is set.
#[test]
fn child_fenced_writer() {
    if std::env::var("WEFT_FENCE_CHILD").is_err() {
        return;
    }
    let lease = env("WEFT_FENCE_LEASE_SECS")
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(2)
        .max(1);
    let kind = BackendKind::S3 {
        config: S3Config {
            endpoint: env("WEFT_FENCE_S3_ENDPOINT").expect("endpoint"),
            bucket: env("WEFT_FENCE_S3_BUCKET").expect("bucket"),
            region: env("WEFT_FENCE_S3_REGION").unwrap_or_else(|| "us-east-1".to_owned()),
            access_key: env("WEFT_FENCE_S3_ACCESS_KEY").expect("access key"),
            secret_key: env("WEFT_FENCE_S3_SECRET_KEY").expect("secret key"),
            session_token: None,
            path_style: true,
            lock_lease: Duration::from_secs(lease),
            request_timeout: Duration::from_secs(60),
        },
    };
    fencing::run_child(kind);
}

// ---------------------------------------------------------------------------
// S3-specific tests
// ---------------------------------------------------------------------------

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

        // Race a release against an acquire, deterministically: the acquirer
        // retries the open until it has observed the live lock at least once,
        // then this thread releases, and the acquirer must win with the next
        // token. The two conditional writes therefore genuinely overlap.
        let observed_live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquirer_flag = observed_live.clone();
        let acquirer_path = path.clone();
        let acquirer_kind = kind.clone();
        let acquire = std::thread::spawn(move || {
            let mut attempts = 0u32;
            let mut last_class = None;
            for _ in 0..400 {
                attempts += 1;
                match Store::open_with_backend(
                    &acquirer_path,
                    OpenMode::ReadWrite,
                    acquirer_kind.clone(),
                ) {
                    Ok(store) => return (Some(store), attempts, last_class),
                    Err(err) => {
                        last_class = Some(err.class_name().to_owned());
                        acquirer_flag.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            }
            (None, attempts, last_class)
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while !observed_live.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                panic!("iteration {iteration}: the acquirer never observed the live lock");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(holder);
        let (winner, attempts, last_class) = acquire.join().expect("acquirer thread");
        let winner = winner.expect("the acquirer never observed the released lock");
        assert_eq!(
            last_class.as_deref(),
            Some("Busy"),
            "iteration {iteration}: the acquirer only ever saw {last_class:?}"
        );
        assert!(
            attempts >= 2,
            "iteration {iteration}: the acquirer succeeded before observing the live lock"
        );
        eprintln!("release/acquire race {iteration}: acquired after {attempts} attempt(s)");

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
