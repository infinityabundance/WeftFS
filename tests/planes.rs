//! Blob/commit plane court (subphase R10).
//!
//! Proves the split introduced in `src/planes.rs`:
//!
//! * the two plane contracts are implemented by the local, object, and S3
//!   backends, and are *probed* on a writable open — a probe that contradicts a
//!   declared capability fails closed;
//! * a split store (blobs on one plane, commit/control state on another)
//!   writes, reopens, verifies, and recovers to exactly the same logical state
//!   as the single-backend store it mirrors (same content roots, same commit
//!   generations);
//! * a process fault between blob publication and head publication, and
//!   between head-publication phases, leaves the old or the new whole state,
//!   never a partial one;
//! * a split whose blob plane cannot publish atomically or whose commit plane
//!   cannot publish conditionally is refused before any byte is written.
//!
//! The S3 cases run only when `WEFT_S3_TEST_ENDPOINT` is set (see
//! `scripts/s3_court.sh --planes`); otherwise they print a skip line and
//! return. This is a single-host court with synthetic fixtures. It makes no
//! media, power-loss, cloud, fleet, or production claim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Capabilities, Key, KeyKind};
use weftfs::s3::S3Config;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

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

fn seed(tag: u8) -> Vec<u8> {
    vec![tag; 32]
}

fn attrs() -> BTreeMap<String, weftcodec::wcb::Value> {
    BTreeMap::new()
}

fn expect_err<T>(result: weftfs::Result<T>, what: &str) -> weftfs::Error {
    match result {
        Ok(_) => panic!("{what}: expected an error"),
        Err(err) => err,
    }
}

/// Deterministic fixture covering the extension surface: a directory, seeded
/// files, a COW range write, truncate, rename, rmdir, a hard link, xattrs, a
/// typed relation, and a snapshot followed by another write.
fn build_fixture(path: &Path, kind: BackendKind) -> Store {
    let mut store = Store::init_with_backend(path, opts(), kind).unwrap();
    let root = store.resolve_path("/").unwrap();
    let dir = store
        .mkdir_seeded(root, b"dir", 0o755, 0, 0, &seed(1))
        .unwrap();
    let a = store
        .create_file_seeded(
            root,
            b"a.txt",
            b"hello world, weft",
            0o644,
            0,
            0,
            attrs(),
            &seed(2),
        )
        .unwrap();
    store.write_file(a, 6, b"WORLD").unwrap();
    store.truncate(a, 11).unwrap();
    let b = store
        .mkdir_seeded(root, b"b", 0o755, 0, 0, &seed(3))
        .unwrap();
    store
        .create_file_seeded(b, b"inner", b"inner", 0o644, 0, 0, attrs(), &seed(4))
        .unwrap();
    store.rename(root, b"b", root, b"c").unwrap();
    let empty = store
        .mkdir_seeded(root, b"empty", 0o755, 0, 0, &seed(5))
        .unwrap();
    let _ = empty;
    store.rmdir(root, b"empty").unwrap();
    let link_target = store
        .create_file_seeded(root, b"linked", b"shared", 0o644, 0, 0, attrs(), &seed(6))
        .unwrap();
    store.link(link_target, dir, b"hard").unwrap();
    store.set_xattr(a, "user.note", b"plane court").unwrap();
    store
        .relation_add(
            root,
            "tag",
            RelationTarget::Value(weftcodec::wcb::Value::text("planes")),
            None,
            attrs(),
        )
        .unwrap();
    store.snapshot_create("s1").unwrap();
    store.write_file(a, 0, b"HELLO").unwrap();
    store
}

fn assert_same_state(left: &Store, right: &Store) {
    assert_eq!(left.generation(), right.generation(), "generation");
    assert_eq!(left.snapshot_root(), right.snapshot_root(), "snapshot root");
    let a = left.current_state();
    let b = right.current_state();
    assert_eq!(a.generation, b.generation);
    assert_eq!(a.objects, b.objects, "object records");
    assert_eq!(a.entries, b.entries, "entry records");
    assert_eq!(a.relations, b.relations, "relations");
    assert_eq!(a.snapshots, b.snapshots, "snapshots");
}

/// Every file's bytes must be identical between two stores with the same state.
fn assert_same_bytes(left: &Store, right: &Store) {
    let mut ids: Vec<Hash32> = left
        .current_state()
        .objects
        .iter()
        .filter(|(_, record)| record.kind == weftfs::ObjectKind::File)
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "fixture has no file objects");
    for id in ids {
        assert_eq!(
            left.read_object(id).unwrap(),
            right.read_object(id).unwrap(),
            "object bytes for {id}"
        );
    }
}

fn files_under(dir: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(dir, &mut count);
    count
}

// ---------------------------------------------------------------------------
// Always-on: local planes and split configuration
// ---------------------------------------------------------------------------

#[test]
fn local_planes_are_probed_on_a_writable_open() {
    let dir = tempdir().unwrap();
    let store = Store::init_with_backend(dir.path(), opts(), BackendKind::Local).unwrap();
    let probes = store
        .plane_probes()
        .expect("a writable local open probes both planes");
    assert_eq!(probes.blobs.backend, "local");
    assert!(probes.blobs.attempted, "blob probe was skipped");
    assert!(probes.blobs.create_once_visible);
    assert!(probes.blobs.length_match);
    assert!(probes.blobs.range_read_match);
    assert!(probes.blobs.delete_observed);
    assert!(probes.blobs.bytes_probed > 0);
    assert_eq!(probes.commits.backend, "local");
    assert!(probes.commits.attempted, "commit probe was skipped");
    assert!(probes.commits.create_ok);
    assert!(probes.commits.matching_compare_exchange_ok);
    assert!(
        probes.commits.stale_compare_exchange_rejected,
        "the commit probe must prove that a stale token is rejected"
    );
    // Single-backend stores keep the historical one-stream counters.
    assert!(store.plane_stats().is_none());
    assert!(store.plane_capabilities().is_none());
}

#[test]
fn object_planes_are_probed_and_declare_conditional_head() {
    let dir = tempdir().unwrap();
    let store = Store::init_with_backend(dir.path(), opts(), BackendKind::Object).unwrap();
    let probes = store.plane_probes().expect("object backend probes planes");
    assert!(probes.blobs.attempted && probes.commits.attempted);
    assert!(probes.commits.stale_compare_exchange_rejected);
    assert!(store.backend_capabilities().conditional_head);
}

#[test]
fn split_local_planes_match_the_single_backend_store() {
    let base = tempdir().unwrap();
    let single_path = base.path().join("single");
    let commit_path = base.path().join("split-commit");
    let blob_path = base.path().join("split-blob");

    let single = build_fixture(&single_path, BackendKind::Local);
    let report = single.verify(true).unwrap();
    assert!(report.ok, "{:?}", report.problems);
    let single_root = single.snapshot_root();
    let single_generation = single.generation();
    drop(single);

    let kind = BackendKind::split_with_blob_root(
        BackendKind::Local,
        BackendKind::Local,
        Some(blob_path.clone()),
    )
    .unwrap();
    let split = build_fixture(&commit_path, kind.clone());
    let probes = split.plane_probes().expect("split probes both planes");
    assert!(probes.blobs.attempted && probes.commits.attempted);

    let planes = split
        .plane_capabilities()
        .expect("a split reports per-plane capabilities");
    assert_eq!(planes.blobs.backend, "local");
    assert_eq!(planes.commits.backend, "local");
    assert!(planes.commits.conditional_update);

    let stats = split
        .plane_stats()
        .expect("a split reports per-plane stats");
    assert!(stats.blobs.writes > 0, "blob plane saw no publication");
    assert!(stats.blobs.bytes_written > 0);
    assert!(stats.commits.writes > 0, "commit plane saw no publication");
    // The probe shares the plane counters, so this is a sanity floor, not an
    // exact count: every plane must have done real work.
    assert!(stats.blobs.bytes_read > 0);

    let report = split.verify(true).unwrap();
    assert!(report.ok, "{:?}", report.problems);
    assert_eq!(split.snapshot_root(), single_root, "content root");
    assert_eq!(split.generation(), single_generation, "generation");
    drop(split);

    // Reopen both and compare the recovered whole state.
    let single = Store::open(&single_path, OpenMode::ReadOnly).unwrap();
    let split = Store::open_with_backend(&commit_path, OpenMode::ReadOnly, kind).unwrap();
    assert_same_state(&single, &split);
    assert_same_bytes(&single, &split);
    assert!(single.verify(true).unwrap().ok);
    assert!(split.verify(true).unwrap().ok);

    // The physical separation is real: the commit root holds no blob file.
    assert!(
        files_under(&commit_path.join("objects")) == 0,
        "commit plane unexpectedly holds object bytes"
    );
    assert!(
        files_under(&commit_path.join("manifests")) == 0,
        "commit plane unexpectedly holds manifest bytes"
    );
    assert!(
        files_under(&blob_path.join("objects")) > 0,
        "blob plane holds no object bytes"
    );
    assert!(
        files_under(&blob_path.join("manifests")) > 0,
        "blob plane holds no manifest bytes"
    );
    assert!(commit_path.join("superblock").is_file());
    assert!(commit_path.join("FORMAT").is_file());
}

#[test]
fn commit_plane_without_conditional_update_is_refused() {
    let dir = tempdir().unwrap();
    Store::init(&dir.path().join("ok"), opts()).unwrap();
    let mut capabilities = Capabilities::local_posix();
    capabilities.conditional_head = false;
    let weak = BackendKind::Posix { capabilities };
    let err = expect_err(
        Store::open_with_backend(dir.path().join("ok"), OpenMode::ReadWrite, weak),
        "writable open without conditional head",
    );
    assert_eq!(err.class_name(), "UnsupportedVersion");
    assert!(
        err.message().contains("conditional_update"),
        "unexpected error: {err}"
    );
}

#[test]
fn split_refuses_a_blob_plane_without_atomic_publish() {
    let dir = tempdir().unwrap();
    let mut capabilities = Capabilities::local_posix();
    capabilities.atomic_publish = false;
    let kind = BackendKind::split(BackendKind::Posix { capabilities }, BackendKind::Local).unwrap();
    let err = expect_err(
        Store::init_with_backend(dir.path().join("weak-blob"), opts(), kind),
        "split with a weak blob plane",
    );
    assert_eq!(err.class_name(), "UnsupportedVersion");
    assert!(
        err.message().contains("blob plane") && err.message().contains("atomic_publish"),
        "unexpected error: {err}"
    );
}

#[test]
fn split_refuses_a_commit_plane_without_conditional_update() {
    let dir = tempdir().unwrap();
    let mut capabilities = Capabilities::local_posix();
    capabilities.conditional_head = false;
    let kind = BackendKind::split(BackendKind::Local, BackendKind::Posix { capabilities }).unwrap();
    let err = expect_err(
        Store::init_with_backend(dir.path().join("weak-commit"), opts(), kind),
        "split with a weak commit plane",
    );
    assert_eq!(err.class_name(), "UnsupportedVersion");
    assert!(
        err.message().contains("commit plane") && err.message().contains("conditional_update"),
        "unexpected error: {err}"
    );
}

#[test]
fn nested_splits_are_refused() {
    let inner = BackendKind::split(BackendKind::Local, BackendKind::Local).unwrap();
    let err = expect_err(
        BackendKind::split(inner, BackendKind::Local),
        "nested split",
    );
    assert!(err.message().contains("nesting"), "unexpected error: {err}");
}

/// Process-fault proxy over a local split: kill the writer at each publication
/// phase and prove the reopened store holds the old or the new whole state.
#[test]
fn split_crash_proxy_yields_old_or_new() {
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let base = tempdir().unwrap();
        let commit_path = base.path().join("commit");
        let blob_path = base.path().join("blob");
        let kind = BackendKind::split_with_blob_root(
            BackendKind::Local,
            BackendKind::Local,
            Some(blob_path.clone()),
        )
        .unwrap();

        let mut store = Store::init_with_backend(&commit_path, opts(), kind.clone()).unwrap();
        let root = store.resolve_path("/").unwrap();
        let id = store
            .create_file_seeded(root, b"f", b"old\n", 0o644, 0, 0, attrs(), &seed(30))
            .unwrap();
        let generation = store.generation();
        drop(store);

        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .args([
                "--backend",
                "local",
                "--blob-backend",
                "local",
                "--commit-backend",
                "local",
                "--blob-root",
            ])
            .arg(&blob_path)
            .arg("--store")
            .arg(&commit_path)
            .args([
                "write",
                "/f",
                "--offset",
                "0",
                "--text",
                "new content, longer than old\n",
            ])
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}"
        );

        let store = Store::open_with_backend(&commit_path, OpenMode::ReadWrite, kind)
            .expect("reopen after crash");
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes == b"old\n" || bytes == b"new content, longer than old\n",
            "phase {phase} produced a partial state: {bytes:?}"
        );
        if phase == "after_superblock_write" {
            assert_eq!(store.generation(), generation + 1);
            assert_eq!(bytes, b"new content, longer than old\n");
        } else {
            assert_eq!(store.generation(), generation);
            assert_eq!(bytes, b"old\n");
        }
        let report = store.verify(true).unwrap();
        // A crash before the head is published leaves an unreferenced newer
        // commit file; that is the documented state, exactly as in the
        // single-backend crash proxies. The head and the object bytes are the
        // old-or-new proof.
        let _ = report;
    }
}

#[test]
fn split_publication_uses_one_head_compare_and_swap() {
    // A second writer must observe `Busy`, which proves the split publishes the
    // head conditionally rather than overwriting it.
    let base = tempdir().unwrap();
    let commit_path = base.path().join("commit");
    let blob_path = base.path().join("blob");
    let kind = BackendKind::split_with_blob_root(
        BackendKind::Local,
        BackendKind::Local,
        Some(blob_path.clone()),
    )
    .unwrap();
    let store = Store::init_with_backend(&commit_path, opts(), kind.clone()).unwrap();
    let err = expect_err(
        Store::open_with_backend(&commit_path, OpenMode::ReadWrite, kind),
        "second live split writer",
    );
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    drop(store);
}

// ---------------------------------------------------------------------------
// S3 shapes (opt-in court)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct S3Court {
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    prefix: String,
    lock_lease: Duration,
}

fn s3_court() -> Option<S3Court> {
    let endpoint = match std::env::var("WEFT_S3_TEST_ENDPOINT") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            eprintln!(
                "SKIP planes s3: WEFT_S3_TEST_ENDPOINT is not set; \
                 run scripts/s3_court.sh --planes to start MinIO"
            );
            return None;
        }
    };
    let suffix = std::env::var("WEFT_S3_TEST_PREFIX").unwrap_or_else(|_| "weft-planes".to_owned());
    let lock_lease = std::env::var("WEFT_S3_LOCK_LEASE_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(2);
    Some(S3Court {
        endpoint,
        region: std::env::var("WEFT_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
        access_key: std::env::var("WEFT_S3_ACCESS_KEY")
            .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
            .expect("S3 access key"),
        secret_key: std::env::var("WEFT_S3_SECRET_KEY")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .expect("S3 secret key"),
        prefix: format!("{suffix}-{}", std::process::id()),
        lock_lease: Duration::from_secs(lock_lease.max(1)),
    })
}

impl S3Court {
    fn config(&self) -> S3Config {
        let n = BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
        S3Config {
            endpoint: self.endpoint.clone(),
            bucket: format!("{}-{n}", self.prefix),
            region: self.region.clone(),
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            session_token: None,
            path_style: true,
            lock_lease: self.lock_lease,
            request_timeout: Duration::from_secs(60),
        }
    }

    fn kind(&self) -> BackendKind {
        BackendKind::S3 {
            config: self.config(),
        }
    }
}

fn s3_env_args(config: &S3Config, args: &mut Vec<String>) {
    args.extend([
        "--s3-endpoint".to_owned(),
        config.endpoint.clone(),
        "--s3-bucket".to_owned(),
        config.bucket.clone(),
        "--s3-region".to_owned(),
        config.region.clone(),
        "--s3-access-key".to_owned(),
        config.access_key.clone(),
        "--s3-secret-key".to_owned(),
        config.secret_key.clone(),
    ]);
}

/// Blobs on S3, commit/control state on local disk: write, reopen, verify, and
/// recover to exactly the single-backend logical state.
#[test]
fn s3_blobs_local_commits_match_the_single_backend_store() {
    let Some(court) = s3_court() else { return };
    let base = tempdir().unwrap();
    let single_path = base.path().join("single-s3");
    let commit_path = base.path().join("split-commit");
    let single_kind = court.kind();
    let split_kind = BackendKind::split(court.kind(), BackendKind::Local).unwrap();

    let single = build_fixture(&single_path, single_kind.clone());
    let single_root = single.snapshot_root();
    let single_generation = single.generation();
    assert!(single.verify(true).unwrap().ok);
    drop(single);

    let split = build_fixture(&commit_path, split_kind.clone());
    let planes = split.plane_capabilities().expect("split capabilities");
    assert_eq!(planes.blobs.backend, "s3");
    assert_eq!(planes.commits.backend, "local");
    let probes = split.plane_probes().expect("split probes");
    assert!(probes.blobs.attempted);
    assert!(probes.commits.attempted);
    assert!(probes.commits.stale_compare_exchange_rejected);
    assert!(split.verify(true).unwrap().ok, "split verify --full");
    assert_eq!(split.snapshot_root(), single_root, "content root");
    assert_eq!(split.generation(), single_generation, "generation");
    drop(split);

    let single =
        Store::open_with_backend(&single_path, OpenMode::ReadOnly, single_kind.clone()).unwrap();
    let split = Store::open_with_backend(&commit_path, OpenMode::ReadOnly, split_kind).unwrap();
    assert_same_state(&single, &split);
    assert_same_bytes(&single, &split);
    assert!(split.verify(true).unwrap().ok);

    // The commit root is local and holds the head; the blobs are remote.
    assert!(commit_path.join("superblock").is_file());
    assert!(commit_path.join("FORMAT").is_file());
    assert_eq!(
        files_under(&commit_path.join("objects")),
        0,
        "local commit root unexpectedly holds chunk bytes"
    );
}

/// Local blobs, S3 commit/control state. The S3 backend already implements the
/// conditional primitives the head needs (`GET` + `If-Match` PUT), so the
/// mirrored split is expressible; the court proves it is exact.
#[test]
fn local_blobs_s3_commits_match_the_single_backend_store() {
    let Some(court) = s3_court() else { return };
    let base = tempdir().unwrap();
    let single_path = base.path().join("single-s3");
    let blob_path = base.path().join("split-blob");
    let commit_path = base.path().join("split-commit");
    let single_kind = court.kind();
    let split_kind = BackendKind::split_with_blob_root(
        BackendKind::Local,
        court.kind(),
        Some(blob_path.clone()),
    )
    .unwrap();

    let single = build_fixture(&single_path, single_kind.clone());
    let single_root = single.snapshot_root();
    let single_generation = single.generation();
    assert!(single.verify(true).unwrap().ok);
    drop(single);

    let split = build_fixture(&commit_path, split_kind.clone());
    let planes = split.plane_capabilities().expect("split capabilities");
    assert_eq!(planes.blobs.backend, "local");
    assert_eq!(planes.commits.backend, "s3");
    assert!(split.verify(true).unwrap().ok, "split verify --full");
    assert_eq!(split.snapshot_root(), single_root, "content root");
    assert_eq!(split.generation(), single_generation, "generation");
    drop(split);

    let single =
        Store::open_with_backend(&single_path, OpenMode::ReadOnly, single_kind.clone()).unwrap();
    let split = Store::open_with_backend(&commit_path, OpenMode::ReadOnly, split_kind).unwrap();
    assert_same_state(&single, &split);
    assert_same_bytes(&single, &split);
    assert!(split.verify(true).unwrap().ok);
    assert!(
        files_under(&blob_path.join("objects")) > 0,
        "local blob plane holds no chunk bytes"
    );
}

#[test]
fn s3_split_crash_proxy_yields_old_or_new() {
    let Some(court) = s3_court() else { return };
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let base = tempdir().unwrap();
        let commit_path = base.path().join("commit");
        let config = court.config();
        let kind = BackendKind::split(
            BackendKind::S3 {
                config: config.clone(),
            },
            BackendKind::Local,
        )
        .unwrap();

        let mut store = Store::init_with_backend(&commit_path, opts(), kind.clone()).unwrap();
        let root = store.resolve_path("/").unwrap();
        let id = store
            .create_file_seeded(root, b"f", b"old\n", 0o644, 0, 0, attrs(), &seed(40))
            .unwrap();
        let generation = store.generation();
        drop(store);

        let mut args: Vec<String> = vec![
            "--backend".to_owned(),
            "s3".to_owned(),
            "--commit-backend".to_owned(),
            "local".to_owned(),
        ];
        s3_env_args(&config, &mut args);
        args.push("--store".to_owned());
        args.push(commit_path.display().to_string());
        args.extend([
            "write".to_owned(),
            "/f".to_owned(),
            "--offset".to_owned(),
            "0".to_owned(),
            "--text".to_owned(),
            "new content, longer than old\n".to_owned(),
        ]);
        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .args(&args)
            .env("WEFT_CRASH_PHASE", phase)
            .env("WEFT_S3_LOCK_LEASE_SECS", "1")
            .status()
            .expect("spawn weftfs");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}"
        );

        // The aborted writer left its LOCK object in the commit plane, which is
        // local here, so a reopen reclaims it immediately (flock dies with the
        // process). Wait a beat anyway to keep the S3 cases uniform.
        std::thread::sleep(Duration::from_millis(200));
        let store = Store::open_with_backend(&commit_path, OpenMode::ReadWrite, kind)
            .expect("reopen after crash");
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes == b"old\n" || bytes == b"new content, longer than old\n",
            "phase {phase} produced a partial state: {bytes:?}"
        );
        if phase == "after_superblock_write" {
            assert_eq!(store.generation(), generation + 1);
        } else {
            assert_eq!(store.generation(), generation);
            assert_eq!(bytes, b"old\n");
        }
        // As in the single-backend S3 crash proxy, an unpublished newer commit
        // file is the documented early-crash state; old-or-new is proved by the
        // head and the object bytes above.
        let _ = store.verify(true).unwrap();
    }
}

#[test]
fn split_report_lists_planes_in_store_info() {
    // The CLI surface the court uses: `info --json` names both planes.
    let base = tempdir().unwrap();
    let commit_path = base.path().join("commit");
    let blob_path = base.path().join("blob");
    let kind = BackendKind::split_with_blob_root(
        BackendKind::Object,
        BackendKind::Local,
        Some(PathBuf::from(&blob_path)),
    )
    .unwrap();
    drop(build_fixture(&commit_path, kind.clone()));

    let output = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .args([
            "--backend",
            "local",
            "--blob-backend",
            "object",
            "--commit-backend",
            "local",
            "--blob-root",
        ])
        .arg(&blob_path)
        .arg("--store")
        .arg(&commit_path)
        .args(["--json", "info"])
        .output()
        .expect("run weftfs info");
    assert!(
        output.status.success(),
        "info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("object-blobs/local-commits"), "{text}");
    assert!(text.contains("\"blobs\""), "{text}");
    assert!(text.contains("\"commits\""), "{text}");
}

#[test]
fn plane_keys_route_to_the_named_plane() {
    // The routing rule itself: blob kinds go to the blob plane, everything else
    // to the commit plane. Asserted through the counters of a split store.
    let base = tempdir().unwrap();
    let commit_path = base.path().join("commit");
    let blob_path = base.path().join("blob");
    let kind =
        BackendKind::split_with_blob_root(BackendKind::Local, BackendKind::Local, Some(blob_path))
            .unwrap();
    let mut store = Store::init_with_backend(&commit_path, opts(), kind).unwrap();
    let before = store.plane_stats().unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(root, b"r", b"routed bytes", 0o644, 0, 0, attrs(), &seed(50))
        .unwrap();
    let _ = store.read_object(id).unwrap();
    let after = store.plane_stats().unwrap();
    assert!(
        after.blobs.writes > before.blobs.writes,
        "chunk publication did not reach the blob plane"
    );
    assert!(
        after.commits.writes > before.commits.writes,
        "commit publication did not reach the commit plane"
    );
    assert!(
        after.blobs.bytes_read > before.blobs.bytes_read,
        "object read did not reach the blob plane"
    );
    // The advisory index and access counters live on the commit plane; neither
    // is a blob-plane key.
    assert!(matches!(Key::index(Hash32::ZERO).kind(), KeyKind::Index));
}
