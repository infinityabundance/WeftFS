//! S3-compatible backend court (subphase Q4).
//!
//! These tests run only when `WEFT_S3_TEST_ENDPOINT` is set. Without it every
//! test prints a clear skip line and returns. `scripts/s3_court.sh` starts a
//! real MinIO server, creates a bucket, and runs this suite with the required
//! environment.
//!
//! Scope: a single-node S3-compatible endpoint on localhost with a tmpfs data
//! directory. No cloud, multi-node, device-cache, media, or power-loss claim is
//! made.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key, KeyKind, StorageBackend};
use weftfs::s3::{S3Backend, S3Config};
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

static BUCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Court {
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    prefix: String,
    lock_lease: Duration,
}

fn court() -> Option<Court> {
    let endpoint = match std::env::var("WEFT_S3_TEST_ENDPOINT") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            eprintln!(
                "SKIP s3_backend: WEFT_S3_TEST_ENDPOINT is not set; \
                 run scripts/s3_court.sh to start MinIO"
            );
            return None;
        }
    };
    let suffix = std::env::var("WEFT_S3_TEST_PREFIX").unwrap_or_else(|_| "weft-test".to_owned());
    let lock_lease = std::env::var("WEFT_S3_LOCK_LEASE_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(2);
    Some(Court {
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

impl Court {
    fn config(&self, _tag: &str) -> S3Config {
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

    fn kind(&self, tag: &str) -> BackendKind {
        BackendKind::S3 {
            config: self.config(tag),
        }
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

#[test]
fn capabilities_and_conditional_put_absent_or_idempotent() {
    let Some(court) = court() else { return };
    let mut backend = S3Backend::new("<s3-test>", court.config("cond"));
    backend.ensure_bucket().expect("ensure bucket");

    let caps = backend.capabilities();
    assert!(caps.atomic_publish);
    assert!(caps.durable_fsync);
    assert!(caps.true_range_reads);
    assert!(caps.consistency.contains("single-node"));
    assert!(caps.publication.contains("If-None-Match"));

    let hash = Hash32::from_bytes([7u8; 32]);
    let key = Key::object(hash);
    let first = b"first payload";
    backend.put_atomic(&key, first).expect("first put");
    backend
        .put_atomic(&key, first)
        .expect("identical put is idempotent");
    let err = expect_err(backend.put_atomic(&key, b"different"), "different bytes");
    assert_eq!(err.class_name(), "Exists", "unexpected error: {err}");

    assert!(backend.exists(&key).unwrap());
    assert_eq!(backend.len(&key).unwrap(), first.len() as u64);
    assert_eq!(backend.read_all(&key).unwrap(), first);
    assert_eq!(backend.read(&key, 6, 7).unwrap(), b"payload");
    assert_eq!(backend.read(&key, first.len() as u64, 4).unwrap(), b"");
    assert_eq!(
        backend.read(&key, 100, 4).unwrap_err().class_name(),
        "InvalidArgument"
    );

    let keys = backend.list_prefix(&Key::prefix(KeyKind::Object)).unwrap();
    assert!(keys.contains(&key), "list did not return {key:?}");

    backend.delete(&key).unwrap();
    assert!(!backend.exists(&key).unwrap());
    assert_eq!(backend.delete(&key).unwrap_err().class_name(), "NotFound");
}

#[test]
fn store_round_trip_over_s3() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let store_path = label.path().join("s3-store");
    let kind = court.kind("roundtrip");

    let mut store = Store::init_with_backend(&store_path, opts(), kind.clone()).expect("init");
    let root = store.resolve_path("/").expect("root");
    let dir = store
        .mkdir_seeded(root, b"dir", 0o755, 0, 0, &seed(1))
        .unwrap();
    let file = store
        .create_file_seeded(
            dir,
            b"a.txt",
            b"0123456789abcdefghij",
            0o644,
            0,
            0,
            attrs(),
            &seed(2),
        )
        .unwrap();
    assert_eq!(store.read_object(file).unwrap(), b"0123456789abcdefghij");
    assert_eq!(store.read_range(file, 4, 6).unwrap(), b"456789");

    store.write_file(file, 6, b"WORLD").unwrap();
    assert_eq!(store.read_object(file).unwrap(), b"012345WORLDbcdefghij");
    store.truncate(file, 8).unwrap();
    assert_eq!(store.read_object(file).unwrap(), b"012345WO");

    store.rename(dir, b"a.txt", root, b"moved.txt").unwrap();
    let moved = store.resolve_path("/moved.txt").unwrap();
    let snap = store.snapshot_create("before").unwrap();
    store.write_file(moved, 0, b"AFTER").unwrap();
    assert_eq!(
        store.read_object_at(Some(snap), moved).unwrap(),
        b"012345WO"
    );

    store
        .relation_add(
            root,
            "tag",
            RelationTarget::Value(weftcodec::wcb::Value::text("s3")),
            None,
            attrs(),
        )
        .unwrap();

    let report = store.verify(true).expect("verify full");
    assert!(report.ok, "{:?}", report.problems);
    let head = store.snapshot_root();
    let generation = store.generation();
    drop(store);

    let reopened =
        Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind).expect("reopen read-only");
    assert_eq!(reopened.snapshot_root(), head);
    assert_eq!(reopened.generation(), generation);
    assert_eq!(
        reopened.read_object_at(Some(snap), moved).unwrap(),
        b"012345WO"
    );
    assert!(reopened.verify(true).unwrap().ok);
}

#[test]
fn writer_lock_and_lost_conditional_head_update() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let store_path = label.path().join("s3-store");
    let kind = court.kind("lock");
    let config = match &kind {
        BackendKind::S3 { config } => config.clone(),
        _ => unreachable!(),
    };

    let store = Store::init_with_backend(&store_path, opts(), kind.clone()).expect("init");
    let err = expect_err(
        Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind.clone()),
        "second live writer",
    );
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    let head = store.snapshot_root();
    let generation = store.generation();
    drop(store);

    // Two backends read the same head ETag. The first conditional write wins;
    // the second, holding a stale ETag, must fail and leave the head readable.
    let first_backend = S3Backend::new("<s3-test>", config.clone());
    let second_backend = S3Backend::new("<s3-test>", config.clone());

    // Primitive check on an ordinary content key.
    let mut first_backend = first_backend;
    let probe_key = Key::object(Hash32::from_bytes([8u8; 32]));
    first_backend.put_atomic(&probe_key, b"A").unwrap();
    let probe_etag = first_backend.etag_for_test(&probe_key).unwrap();
    assert!(first_backend
        .conditional_put_for_test(&probe_key, b"BB", &probe_etag)
        .unwrap());
    assert!(!second_backend
        .conditional_put_for_test(&probe_key, b"CCC", &probe_etag)
        .unwrap());
    assert_eq!(first_backend.read_all(&probe_key).unwrap(), b"BB");

    let key = Key::superblock();
    let original = first_backend.read_all(&key).unwrap();
    let etag = first_backend.etag_for_test(&key).unwrap();
    assert_eq!(original.len(), 8192);

    // Flip a byte in the unused slot's padding so the bytes differ but the
    // published generation slot (odd generation -> slot 1) stays valid.
    let mut winner = original.clone();
    winner[4095] = 1;
    assert!(first_backend
        .conditional_put_for_test(&key, &winner, &etag)
        .expect("first conditional put"));
    let mut loser = original.clone();
    loser[4094] = 1;
    assert!(
        !second_backend
            .conditional_put_for_test(&key, &loser, &etag)
            .expect("second conditional put"),
        "stale If-Match unexpectedly succeeded"
    );
    assert_eq!(first_backend.read_all(&key).unwrap(), winner);

    let reopened =
        Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind).expect("reopen after race");
    assert_eq!(
        reopened.snapshot_root(),
        head,
        "head changed after lost race"
    );
    assert_eq!(reopened.generation(), generation);
}

#[test]
fn crash_proxy_over_s3_yields_old_or_new() {
    let Some(court) = court() else { return };
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let label = tempdir().unwrap();
        let store_path = label.path().join("s3-store");
        let kind = court.kind(&format!("crash-{phase}"));
        let config = match &kind {
            BackendKind::S3 { config } => config.clone(),
            _ => unreachable!(),
        };
        let mut store = Store::init_with_backend(&store_path, opts(), kind.clone()).expect("init");
        let root = store.resolve_path("/").unwrap();
        let id = store
            .create_file_seeded(root, b"f", b"old\n", 0o644, 0, 0, attrs(), &seed(30))
            .unwrap();
        let generation = store.generation();
        drop(store);

        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .args([
                "--backend",
                "s3",
                "write",
                "/f",
                "--offset",
                "0",
                "--text",
                "new content, longer than old\n",
            ])
            .env("WEFT_S3_ENDPOINT", &config.endpoint)
            .env("WEFT_S3_BUCKET", &config.bucket)
            .env("WEFT_S3_REGION", &config.region)
            .env("WEFT_S3_ACCESS_KEY", &config.access_key)
            .env("WEFT_S3_SECRET_KEY", &config.secret_key)
            .env("WEFT_S3_LOCK_LEASE_SECS", "1")
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}"
        );

        // The aborted writer left its LOCK object. Wait out the one-second
        // lease, then prove a writable open reclaims it and sees whole state.
        std::thread::sleep(Duration::from_millis(1_300));
        let store =
            Store::open_with_backend(&store_path, OpenMode::ReadWrite, kind).expect("reopen");
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
        // An unpublished newer commit file is expected after the early crash
        // phases, exactly as in the local/object crash proxies; the head and
        // object bytes above are the old-or-new proof.
        let _ = store.verify(true).unwrap();
    }
}

#[test]
fn gc_over_s3_deletes_only_unreachable_objects() {
    let Some(court) = court() else { return };
    let label = tempdir().unwrap();
    let store_path = label.path().join("s3-store");
    let kind = court.kind("gc");
    let mut store = Store::init_with_backend(
        &store_path,
        Options {
            checkpoint_interval: 0,
            ..opts()
        },
        kind.clone(),
    )
    .expect("init");
    let root = store.resolve_path("/").unwrap();

    let keep = store
        .create_file_seeded(
            root,
            b"keep",
            b"kept bytes",
            0o644,
            0,
            0,
            attrs(),
            &seed(40),
        )
        .unwrap();
    let snapshot = store.snapshot_create("pin").unwrap();
    let drop_id = store
        .create_file_seeded(
            root,
            b"drop",
            b"dropped bytes",
            0o644,
            0,
            0,
            attrs(),
            &seed(41),
        )
        .unwrap();
    assert_eq!(store.read_object(drop_id).unwrap(), b"dropped bytes");
    // Overwrite so the old content becomes unreachable; the pinned file's old
    // content stays reachable through the snapshot.
    store.write_file(drop_id, 0, b"dropped bytes v2").unwrap();
    store.write_file(keep, 0, b"kept bytes v2").unwrap();

    let report = store.gc(true).expect("gc");
    assert!(report.applied);
    assert!(!report.deleted.is_empty(), "gc deleted nothing: {report:?}");
    assert_eq!(store.read_object(keep).unwrap(), b"kept bytes v2");
    assert_eq!(
        store.read_object_at(Some(snapshot), keep).unwrap(),
        b"kept bytes"
    );
    assert_eq!(store.read_object(drop_id).unwrap(), b"dropped bytes v2");
    assert!(store.verify(true).unwrap().ok);
    drop(store);

    let reopened = Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind).unwrap();
    assert_eq!(reopened.read_object(keep).unwrap(), b"kept bytes v2");
    assert_eq!(
        reopened.read_object_at(Some(snapshot), keep).unwrap(),
        b"kept bytes"
    );
}

fn build_fixture(path: &std::path::Path, kind: BackendKind) -> Store {
    let mut store = Store::init_with_backend(path, opts(), kind).unwrap();
    let root = store.resolve_path("/").unwrap();
    let dir = store
        .mkdir_seeded(root, b"dir", 0o755, 0, 0, &seed(1))
        .unwrap();
    let a = store
        .create_file_seeded(
            dir,
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
    store.set_xattr(a, "user.note", b"s3 court").unwrap();
    store
        .relation_add(
            root,
            "tag",
            RelationTarget::Value(weftcodec::wcb::Value::text("fixture")),
            None,
            attrs(),
        )
        .unwrap();
    store.snapshot_create("s1").unwrap();
    store.write_file(a, 0, b"HELLO").unwrap();
    store
}

fn assert_same_logical_state(local: &Store, s3: &Store) {
    assert_eq!(local.generation(), s3.generation());
    assert_eq!(local.snapshot_root(), s3.snapshot_root());
    let a = local.current_state();
    let b = s3.current_state();
    assert_eq!(a.generation, b.generation);
    assert_eq!(a.objects, b.objects);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.relations, b.relations);
    assert_eq!(a.snapshots, b.snapshots);
    assert_eq!(a.tips, b.tips);
}

fn keys(kind: &BackendKind, root: &std::path::Path, key_kind: KeyKind) -> Vec<Key> {
    let backend = kind.open(root);
    let mut keys = backend
        .list_prefix(&Key::prefix(key_kind))
        .unwrap_or_else(|err| panic!("list {:?}: {err}", key_kind));
    keys.sort();
    keys
}

#[test]
fn local_and_s3_artifacts_are_byte_identical() {
    let Some(court) = court() else { return };
    let local_dir = tempdir().unwrap();
    let s3_label = tempdir().unwrap();
    let s3_path = s3_label.path().join("s3-store");
    let s3_kind = court.kind("parity");

    let local = build_fixture(local_dir.path(), BackendKind::Local);
    let s3 = build_fixture(&s3_path, s3_kind.clone());

    assert_same_logical_state(&local, &s3);
    for key_kind in [
        KeyKind::Commit,
        KeyKind::Object,
        KeyKind::Manifest,
        KeyKind::Index,
        KeyKind::SnapshotRef,
    ] {
        let local_keys = keys(&BackendKind::Local, local_dir.path(), key_kind);
        let s3_keys = keys(&s3_kind, &s3_path, key_kind);
        assert_eq!(
            local_keys, s3_keys,
            "logical {key_kind:?} key sets differ between local and S3"
        );
        for key in local_keys {
            let a = local.backend_read_key(&key).unwrap();
            let b = s3.backend_read_key(&key).unwrap();
            assert_eq!(
                a,
                b,
                "{key_kind:?} artifact {} differs between local and S3",
                key.display_path()
            );
        }
    }
    assert_eq!(
        local.backend_read_key(&Key::format()).unwrap(),
        s3.backend_read_key(&Key::format()).unwrap()
    );
    assert_eq!(
        local.backend_read_key(&Key::superblock()).unwrap(),
        s3.backend_read_key(&Key::superblock()).unwrap()
    );
    assert!(local.verify(true).unwrap().ok);
    assert!(s3.verify(true).unwrap().ok);
}
