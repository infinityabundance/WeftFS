//! Azure Blob fenced-writer court (subphase R21).
//!
//! Runs only when `WEFT_AZURE_TEST_ENDPOINT` is set; without it every test
//! prints a skip line and returns. `bench/scripts/azure_court.sh` starts a
//! pinned Azurite release on loopback and runs this suite with a short lease.
//!
//! The test bodies are imported verbatim from `tests/support/fencing.rs`, the
//! same module the S3 court (MinIO, Ceph RGW) and the GCS court use. Only the
//! endpoint configuration and the Azure conditional token (an entity tag, not a
//! generation) are backend-specific.
//!
//! Label: a single Azure Blob emulator on loopback in the OOM-protected Docker
//! court with a memory-backed data directory. This proves protocol behaviour
//! and ETag-precondition safety. Not an Azure, cloud, multi-node, fleet, device
//! cache, media, power-loss, or production claim, and not a multi-writer
//! protocol.

#[path = "support/fencing.rs"]
mod fencing;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use weftfs::azure::{AzureBackend, AzureConfig};
use weftfs::backend::BackendKind;

use fencing::FenceCourt;

static CONTAINER_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Court {
    endpoint: String,
    container_base: String,
    account: String,
    key: Option<String>,
    lease: Duration,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn court() -> Option<Court> {
    let Some(endpoint) = env("WEFT_AZURE_TEST_ENDPOINT") else {
        eprintln!(
            "SKIP azure_fencing: WEFT_AZURE_TEST_ENDPOINT is not set; run \
             bench/scripts/azure_court.sh"
        );
        return None;
    };
    let lease = env("WEFT_AZURE_FENCE_LEASE_SECS")
        .or_else(|| env("WEFT_AZURE_LOCK_LEASE_SECS"))
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(3)
        .max(1);
    Some(Court {
        endpoint,
        container_base: format!(
            "{}-fence-{}",
            env("WEFT_AZURE_TEST_CONTAINER").unwrap_or_else(|| "weft-azure-court".to_owned()),
            std::process::id()
        ),
        account: env("WEFT_AZURE_ACCOUNT").unwrap_or_else(|| "devstoreaccount1".to_owned()),
        key: env("WEFT_AZURE_KEY"),
        lease: Duration::from_secs(lease),
    })
}

impl Court {
    fn config(&self, tag: &str) -> AzureConfig {
        let n = CONTAINER_COUNTER.fetch_add(1, Ordering::Relaxed);
        AzureConfig {
            endpoint: self.endpoint.clone(),
            container: format!("{}-{tag}-{n}", self.container_base),
            account: self.account.clone(),
            key: self.key.clone(),
            sas_token: None,
            anonymous: self.key.is_none(),
            lock_lease: self.lease,
            request_timeout: Duration::from_secs(60),
        }
    }

    fn kind(&self, tag: &str) -> BackendKind {
        BackendKind::Azure {
            config: self.config(tag),
        }
    }

    fn observer(&self, config: &AzureConfig) -> AzureBackend {
        AzureBackend::new("<azure-fencing-observer>", config.clone())
    }
}

impl FenceCourt for Court {
    fn label(&self) -> &'static str {
        "azure"
    }

    fn new_store(&self, tag: &str) -> BackendKind {
        self.kind(tag)
    }

    fn lease_duration(&self) -> Duration {
        self.lease
    }

    fn child_env(&self, kind: &BackendKind) -> Vec<(String, String)> {
        let BackendKind::Azure { config } = kind else {
            panic!("the Azure court only holds Azure backends");
        };
        let mut env = vec![
            (
                "WEFT_FENCE_AZURE_ENDPOINT".to_owned(),
                config.endpoint.clone(),
            ),
            (
                "WEFT_FENCE_AZURE_CONTAINER".to_owned(),
                config.container.clone(),
            ),
            (
                "WEFT_FENCE_AZURE_ACCOUNT".to_owned(),
                config.account.clone(),
            ),
        ];
        if let Some(key) = &config.key {
            env.push(("WEFT_FENCE_AZURE_KEY".to_owned(), key.clone()));
        }
        env
    }
}

// ---------------------------------------------------------------------------
// Shared invariant bodies (identical in tests/s3_fencing.rs and
// tests/gcs_fencing.rs)
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
    let kind = BackendKind::Azure {
        config: AzureConfig {
            endpoint: env("WEFT_FENCE_AZURE_ENDPOINT").expect("endpoint"),
            container: env("WEFT_FENCE_AZURE_CONTAINER").expect("container"),
            account: env("WEFT_FENCE_AZURE_ACCOUNT")
                .unwrap_or_else(|| "devstoreaccount1".to_owned()),
            key: env("WEFT_FENCE_AZURE_KEY"),
            sas_token: None,
            anonymous: env("WEFT_FENCE_AZURE_KEY").is_none(),
            lock_lease: Duration::from_secs(lease),
            request_timeout: Duration::from_secs(60),
        },
    };
    fencing::run_child(kind);
}

// ---------------------------------------------------------------------------
// Azure-specific observation
// ---------------------------------------------------------------------------

#[test]
fn observer_reads_the_published_lock_without_a_lease() {
    let Some(court) = court() else { return };
    let label = tempfile::tempdir().unwrap();
    let path = label.path().join("store");
    let kind = court.kind("observe");
    let BackendKind::Azure { config } = &kind else {
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
    // The conditional token really is the ETag of the live record: replacing
    // the blob changes it and fences the writer out.
    let etag = observer
        .etag_for_test(&weftfs::backend::Key::lock())
        .expect("LOCK ETag");
    assert!(etag.contains("0x"), "Azure ETags are version tags: {etag}");
    assert!(observer.read_lock_record().expect("reread").is_some());
    drop(store);
}
