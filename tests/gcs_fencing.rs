//! GCS fenced-writer court (subphase R18).
//!
//! Runs only when `WEFT_GCS_TEST_ENDPOINT` is set; without it every test prints
//! a skip line and returns. `bench/scripts/gcs_court.sh` starts a pinned
//! `fake-gcs-server` release on loopback and runs this suite with a short lease.
//!
//! The test bodies are imported verbatim from `tests/support/fencing.rs`, the
//! same module the S3 court uses (MinIO and Ceph RGW). Only the endpoint
//! configuration and the GCS conditional token (an object generation, not an
//! ETag) are backend-specific.
//!
//! Label: a single GCS-compatible emulator on loopback in the OOM-protected
//! Docker court with a memory backend. This proves protocol behaviour and
//! generation-precondition safety. Not a cloud, multi-node, fleet, device
//! cache, media, power-loss, or production claim, and not a multi-writer
//! protocol.

#[path = "support/fencing.rs"]
mod fencing;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use weftfs::backend::BackendKind;
use weftfs::gcs::{GcsBackend, GcsConfig};

use fencing::FenceCourt;

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Court {
    endpoint: String,
    bucket_base: String,
    project: String,
    token: Option<String>,
    anonymous: bool,
    lease: Duration,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_flag(name: &str) -> bool {
    matches!(
        env(name).as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

fn court() -> Option<Court> {
    let Some(endpoint) = env("WEFT_GCS_TEST_ENDPOINT") else {
        eprintln!(
            "SKIP gcs_fencing: WEFT_GCS_TEST_ENDPOINT is not set; run bench/scripts/gcs_court.sh"
        );
        return None;
    };
    let lease = env("WEFT_GCS_FENCE_LEASE_SECS")
        .or_else(|| env("WEFT_GCS_LOCK_LEASE_SECS"))
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(3)
        .max(1);
    let token = env("WEFT_GCS_ACCESS_TOKEN").or_else(|| env("GOOGLE_OAUTH_ACCESS_TOKEN"));
    let anonymous = env_flag("WEFT_GCS_ANONYMOUS") || token.is_none();
    Some(Court {
        endpoint,
        bucket_base: format!(
            "{}-fence-{}",
            env("WEFT_GCS_TEST_BUCKET").unwrap_or_else(|| "weft-gcs-court".to_owned()),
            std::process::id()
        ),
        project: env("WEFT_GCS_PROJECT").unwrap_or_else(|| "weft-court".to_owned()),
        token,
        anonymous,
        lease: Duration::from_secs(lease),
    })
}

impl Court {
    fn config(&self, tag: &str) -> GcsConfig {
        let n = BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
        GcsConfig {
            endpoint: self.endpoint.clone(),
            bucket: format!("{}-{tag}-{n}", self.bucket_base),
            token: self.token.clone(),
            project: Some(self.project.clone()),
            anonymous: self.anonymous,
            lock_lease: self.lease,
            request_timeout: Duration::from_secs(60),
        }
    }

    fn kind(&self, tag: &str) -> BackendKind {
        BackendKind::Gcs {
            config: self.config(tag),
        }
    }

    fn observer(&self, config: &GcsConfig) -> GcsBackend {
        GcsBackend::new("<gcs-fencing-observer>", config.clone())
    }
}

impl FenceCourt for Court {
    fn label(&self) -> &'static str {
        "gcs"
    }

    fn new_store(&self, tag: &str) -> BackendKind {
        self.kind(tag)
    }

    fn lease_duration(&self) -> Duration {
        self.lease
    }

    fn child_env(&self, kind: &BackendKind) -> Vec<(String, String)> {
        let BackendKind::Gcs { config } = kind else {
            panic!("the GCS court only holds GCS backends");
        };
        let mut env = vec![
            (
                "WEFT_FENCE_GCS_ENDPOINT".to_owned(),
                config.endpoint.clone(),
            ),
            ("WEFT_FENCE_GCS_BUCKET".to_owned(), config.bucket.clone()),
            (
                "WEFT_FENCE_GCS_PROJECT".to_owned(),
                config.project.clone().unwrap_or_default(),
            ),
            (
                "WEFT_FENCE_GCS_ANONYMOUS".to_owned(),
                if config.anonymous { "1" } else { "0" }.to_owned(),
            ),
        ];
        if let Some(token) = &config.token {
            env.push(("WEFT_FENCE_GCS_TOKEN".to_owned(), token.clone()));
        }
        env
    }
}

// ---------------------------------------------------------------------------
// Shared invariant bodies (identical in tests/s3_fencing.rs)
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
    let kind = BackendKind::Gcs {
        config: GcsConfig {
            endpoint: env("WEFT_FENCE_GCS_ENDPOINT").expect("endpoint"),
            bucket: env("WEFT_FENCE_GCS_BUCKET").expect("bucket"),
            token: env("WEFT_FENCE_GCS_TOKEN"),
            project: env("WEFT_FENCE_GCS_PROJECT"),
            anonymous: env("WEFT_FENCE_GCS_ANONYMOUS")
                .map(|value| value == "1")
                .unwrap_or(true),
            lock_lease: Duration::from_secs(lease),
            request_timeout: Duration::from_secs(60),
        },
    };
    fencing::run_child(kind);
}

// ---------------------------------------------------------------------------
// GCS-specific observation
// ---------------------------------------------------------------------------

#[test]
fn observer_reads_the_published_lock_without_a_lease() {
    let Some(court) = court() else { return };
    let label = tempfile::tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("observe");
    let BackendKind::Gcs { config } = &kind else {
        unreachable!()
    };
    let store = weftfs::store::Store::init_with_backend(&path, fencing::opts(), kind.clone())
        .expect("init");
    let observer = court.observer(config);
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
    // The conditional token really is the GCS generation of the live record:
    // replacing the object would change it and fence the writer out.
    let generation = observer
        .generation_for_test(&weftfs::backend::Key::lock())
        .expect("LOCK metadata")
        .expect("LOCK exists");
    assert!(generation > 0, "GCS generation must be observable");
    // An observer never acquires or mutates the lease.
    assert!(observer.read_lock_record().expect("reread").is_some());
    drop(store);
}
