//! Distributed-semantics tests (subphase R13).
//!
//! These are the deterministic, always-on half of the distributed correctness
//! court. They pin the semantics the shell court
//! (`bench/scripts/distributed_court.sh`) exercises with real separate
//! processes against MinIO, so the contract never depends only on a shell
//! script:
//!
//! * the exclusive writer interlock (`Busy`) and a clean reopen after the
//!   writer exits;
//! * a reader keeps the generation it opened and does not observe later
//!   commits until it reopens;
//! * concurrent readers see the exact pinned bytes while a writer publishes;
//! * a process fault at every publication transition leaves the old or the new
//!   whole state, never a mixture, for the local and the local split layout;
//! * an at-least-once retry of the same logical change after a crash does not
//!   create a second visible state, and reaches the uninterrupted head when no
//!   commit file was published;
//! * an unreachable S3 endpoint fails closed with `Io`, never partial bytes;
//! * `gc --apply` running while a snapshot reader is live deletes only
//!   unreachable blobs, never a root, input, checkpoint, or provenance blob;
//! * knowing a content hash does not authorize retrieval under an
//!   authenticated capability envelope, and the denial is indistinguishable
//!   from an absent hash.
//!
//! Scope: single host, one process at a time for writers (by design), local
//! filesystems for the always-on cases. No physical power loss, no multi-node
//! S3, no fleet claim.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tempfile::tempdir;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};
use weftcodec::context::{ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::backend::BackendKind;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::s3::S3Config;
use weftfs::store::{OpenMode, Options, Store};

/// Fixed clock for every generated mtime and commit timestamp. Two runs of the
/// same deterministic operations then produce byte-identical commits.
const FIXED_TS: i64 = 1_700_000_000_000_000_000;
/// Child-process store path for the crash helper.
const CHILD_STORE_ENV: &str = "WEFT_DIST_TEST_STORE";
/// Child-process split blob root. Unset means a single local backend.
const CHILD_SPLIT_ENV: &str = "WEFT_DIST_TEST_SPLIT_BLOB";

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TS),
        ..Options::default()
    }
}

fn seed(tag: &[u8]) -> Vec<u8> {
    tag.to_vec()
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn create(store: &mut Store, name: &[u8], bytes: &[u8], seed_tag: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(
            root,
            name,
            bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
            &seed(seed_tag),
        )
        .unwrap()
}

/// Both files change in one commit: a crash that partially applied the group
/// would be visible as one new and one old file.
const ALPHA_OLD: &[u8] = b"alpha-old-payload-0000000000";
const BETA_OLD: &[u8] = b"beta-old-payload-000000000000";
const ALPHA_NEW: &[u8] = b"alpha-new-payload-1111111111";
const BETA_NEW: &[u8] = b"beta-new-payload-111111111111";

/// Child-process operation selector.
const CHILD_OP_ENV: &str = "WEFT_DIST_TEST_OP";

fn group_create_alpha_beta(store: &mut Store) {
    let root = store.resolve_path("/").unwrap();
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
                &seed(b"dist-alpha"),
            )?;
            store.create_file_seeded(
                root,
                b"beta",
                BETA_OLD,
                0o644,
                0,
                0,
                BTreeMap::new(),
                &seed(b"dist-beta"),
            )?;
            Ok(())
        })
        .expect("group create");
}

fn group_overwrite_alpha_beta(store: &mut Store) {
    let root = store.resolve_path("/").unwrap();
    let alpha = store.lookup(root, b"alpha").unwrap();
    let beta = store.lookup(root, b"beta").unwrap();
    store
        .group(|store| {
            store.write_file(alpha, 0, ALPHA_NEW)?;
            store.write_file(beta, 0, BETA_NEW)?;
            Ok(())
        })
        .expect("group overwrite");
}

/// Child process: open the named store (single local or local split) and
/// publish one two-file group (`create` or `overwrite`).
/// `WEFT_CRASH_PHASE` aborts the process at a publication transition; without
/// it the group publishes normally.
#[test]
fn distributed_group_crash_child() {
    let Some(path) = std::env::var_os(CHILD_STORE_ENV) else {
        return;
    };
    let kind = match std::env::var_os(CHILD_SPLIT_ENV) {
        Some(blob) => BackendKind::split_with_blob_root(
            BackendKind::Local,
            BackendKind::Local,
            Some(PathBuf::from(blob)),
        )
        .unwrap(),
        None => BackendKind::Local,
    };
    let mut store =
        Store::open_with_options_and_backend(Path::new(&path), OpenMode::ReadWrite, opts(), kind)
            .expect("child open");
    match std::env::var(CHILD_OP_ENV).as_deref() {
        Ok("overwrite") => group_overwrite_alpha_beta(&mut store),
        _ => group_create_alpha_beta(&mut store),
    }
}

fn spawn_group_child(
    store: &Path,
    split_blob: Option<&Path>,
    op: &str,
    phase: &str,
) -> std::process::ExitStatus {
    let exe = std::env::current_exe().expect("test binary path");
    let mut command = Command::new(exe);
    command
        .args(["--exact", "distributed_group_crash_child", "--nocapture"])
        .env(CHILD_STORE_ENV, store)
        .env(CHILD_OP_ENV, op)
        .env_remove(CHILD_SPLIT_ENV)
        .env("WEFT_CRASH_PHASE", phase)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(blob) = split_blob {
        command.env(CHILD_SPLIT_ENV, blob);
    }
    command.status().expect("spawn crash child")
}

fn kind_for(layout: Layout, blob: &Path) -> BackendKind {
    match layout {
        Layout::Local => BackendKind::Local,
        Layout::Split => BackendKind::split_with_blob_root(
            BackendKind::Local,
            BackendKind::Local,
            Some(blob.to_path_buf()),
        )
        .unwrap(),
    }
}

fn blob_dir_for(layout: Layout, base: &Path) -> PathBuf {
    match layout {
        Layout::Local => base.join("unused-blob"),
        Layout::Split => base.join("blob"),
    }
}

/// Resolve the `(alpha, beta)` object ids and their bytes from a store.
fn read_alpha_beta(store: &Store) -> (Vec<u8>, Vec<u8>) {
    let root = store.resolve_path("/").unwrap();
    let alpha = store.lookup(root, b"alpha").expect("alpha entry");
    let beta = store.lookup(root, b"beta").expect("beta entry");
    (
        store.read_object(alpha).expect("alpha bytes"),
        store.read_object(beta).expect("beta bytes"),
    )
}

fn assert_old_or_new(label: &str, alpha: &[u8], beta: &[u8]) {
    let both_old = alpha == ALPHA_OLD && beta == BETA_OLD;
    let both_new = alpha == ALPHA_NEW && beta == BETA_NEW;
    assert!(
        both_old || both_new,
        "{label}: mixed or partial state (alpha {} bytes, beta {} bytes)",
        alpha.len(),
        beta.len()
    );
}

// ---------------------------------------------------------------------------
// 1. Single-writer interlock
// ---------------------------------------------------------------------------

#[test]
fn single_writer_interlock_is_exclusive_and_reopens_cleanly() {
    let dir = tempdir().unwrap();
    let mut first = Store::init(dir.path(), opts()).unwrap();
    let id = create(&mut first, b"f", b"interlock-bytes", b"interlock");

    let err = match Store::open(dir.path(), OpenMode::ReadWrite) {
        Ok(_) => panic!("a second writer must be refused while the first is live"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::Busy, "unexpected error: {err}");

    // A read-only open is not a writer and must still work.
    let reader = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reader.read_object(id).unwrap(), b"interlock-bytes");
    drop(reader);

    drop(first);
    let second = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
    assert_eq!(second.generation(), 2);
    assert_eq!(second.read_object(id).unwrap(), b"interlock-bytes");
    let report = second.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// 5. Stale reader
// ---------------------------------------------------------------------------

#[test]
fn stale_reader_does_not_observe_a_newer_generation_until_it_reopens() {
    const OLD: &[u8] = b"stale-reader-old-state";
    const NEW: &[u8] = b"stale-reader-new-state";
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let id = create(&mut writer, b"f", OLD, b"stale-f");
    let generation = writer.generation();
    let head = writer.snapshot_root();

    let reader = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reader.generation(), generation);
    assert_eq!(reader.snapshot_root(), head);
    assert_eq!(reader.read_object(id).unwrap(), OLD);

    writer.write_file(id, 0, NEW).unwrap();
    assert_eq!(writer.generation(), generation + 1);
    assert_ne!(writer.snapshot_root(), head);

    // The open reader keeps both its generation and its bytes.
    assert_eq!(reader.generation(), generation);
    assert_eq!(reader.snapshot_root(), head);
    assert_eq!(reader.read_object(id).unwrap(), OLD);

    // Reopening is the only way it sees the new state.
    let reopened = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reopened.generation(), generation + 1);
    assert_eq!(reopened.read_object(id).unwrap(), NEW);

    let report = writer.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn stale_reader_on_a_split_store_does_not_observe_a_newer_generation() {
    const OLD: &[u8] = b"split-stale-old-state--";
    const NEW: &[u8] = b"split-stale-new-state--";
    let base = tempdir().unwrap();
    let commit_dir = base.path().join("commit");
    let blob_dir = base.path().join("blob");
    let kind = kind_for(Layout::Split, &blob_dir);

    let mut writer = Store::init_with_backend(&commit_dir, opts(), kind.clone()).unwrap();
    let id = create(&mut writer, b"f", OLD, b"split-stale");
    let generation = writer.generation();
    let head = writer.snapshot_root();

    let reader =
        Store::open_with_options_and_backend(&commit_dir, OpenMode::ReadOnly, opts(), kind.clone())
            .unwrap();
    assert_eq!(reader.generation(), generation);
    assert_eq!(reader.read_object(id).unwrap(), OLD);

    writer.write_file(id, 0, NEW).unwrap();
    assert_eq!(reader.generation(), generation);
    assert_eq!(reader.snapshot_root(), head);
    assert_eq!(reader.read_object(id).unwrap(), OLD);

    let reopened =
        Store::open_with_options_and_backend(&commit_dir, OpenMode::ReadOnly, opts(), kind.clone())
            .unwrap();
    assert_eq!(reopened.generation(), generation + 1);
    assert_eq!(reopened.read_object(id).unwrap(), NEW);

    let report = writer.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// 4. Concurrent readers during commits
// ---------------------------------------------------------------------------

#[test]
fn concurrent_readers_see_exact_pinned_bytes_while_a_writer_publishes() {
    const READERS: usize = 8;
    const VERSIONS: usize = 10;
    const PAYLOAD: &[u8] = b"pinned-reader-payload-0000";
    let expected = sha256(PAYLOAD);

    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let id = create(&mut writer, b"hot", PAYLOAD, b"concurrent-hot");
    writer.snapshot_create("pin").unwrap();
    let pinned_generation = writer.generation();
    let pinned_commit = writer.snapshot_root();

    let path = dir.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(READERS + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for reader_index in 0..READERS {
        let path = path.clone();
        let barrier = barrier.clone();
        let stop = stop.clone();
        let reads = reads.clone();
        handles.push(thread::spawn(move || {
            let reader = Store::open(&path, OpenMode::ReadOnly).expect("reader open");
            assert_eq!(
                reader.generation(),
                pinned_generation,
                "reader {reader_index} opened at the wrong generation"
            );
            assert_eq!(reader.snapshot_root(), pinned_commit);
            barrier.wait();
            let mut local = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let bytes = reader.read_object(id).expect("pinned read");
                assert_eq!(
                    sha256(&bytes),
                    expected,
                    "reader {reader_index} observed bytes outside the pinned version"
                );
                local += 1;
            }
            reads.fetch_add(local, Ordering::Relaxed);
        }));
    }
    barrier.wait();
    // Publish new versions of the same object while every reader is live.
    for round in 0..VERSIONS {
        let mut next = b"pinned-reader-payload-".to_vec();
        next.extend_from_slice(format!("{round:04}").as_bytes());
        writer.write_file(id, 0, &next).unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for handle in handles {
        handle.join().expect("reader thread");
    }
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "no concurrent read completed"
    );
    assert_eq!(writer.generation(), pinned_generation + VERSIONS as u64);
    let report = writer.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// 2. Atomic publication under a process fault
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Layout {
    Local,
    Split,
}

impl Layout {
    fn label(self) -> &'static str {
        match self {
            Layout::Local => "local",
            Layout::Split => "local-split",
        }
    }
}

#[test]
fn crash_at_every_publication_transition_yields_old_or_new_whole_state() {
    for layout in [Layout::Local, Layout::Split] {
        for phase in [
            "after_objects",
            "after_commit_file",
            "after_superblock_write",
        ] {
            let base = tempdir().unwrap();
            let store_path = base.path().join("store");
            let blob = blob_dir_for(layout, base.path());
            let kind = kind_for(layout, &blob);

            let mut store = Store::init_with_backend(&store_path, opts(), kind.clone()).unwrap();
            group_create_alpha_beta(&mut store);
            let generation = store.generation();
            drop(store);

            let status = spawn_group_child(
                &store_path,
                if matches!(layout, Layout::Split) {
                    Some(&blob)
                } else {
                    None
                },
                "overwrite",
                phase,
            );
            assert!(
                !status.success(),
                "{}: child was expected to abort at {phase}",
                layout.label()
            );

            let reopened = Store::open_with_options_and_backend(
                &store_path,
                OpenMode::ReadOnly,
                opts(),
                kind.clone(),
            )
            .expect("reopen after crash");
            let (alpha, beta) = read_alpha_beta(&reopened);
            assert_old_or_new(&format!("{} {phase}", layout.label()), &alpha, &beta);
            match phase {
                "after_superblock_write" => {
                    assert_eq!(reopened.generation(), generation + 1);
                    assert_eq!(alpha, ALPHA_NEW);
                    assert_eq!(beta, BETA_NEW);
                }
                _ => {
                    assert_eq!(reopened.generation(), generation);
                    assert_eq!(alpha, ALPHA_OLD);
                    assert_eq!(beta, BETA_OLD);
                }
            }
            let report = reopened.verify(true).unwrap();
            if phase == "after_commit_file" {
                // The head did not move, so the commit file the fault left
                // behind is unreferenced. `verify` reports that tail (it never
                // silently ignores it) while every reachable byte is intact.
                assert_eq!(
                    report.problems.len(),
                    1,
                    "{} {}: unexpected problems {:?}",
                    layout.label(),
                    phase,
                    report.problems
                );
                assert!(
                    report.problems[0]
                        .detail
                        .contains("exists above the highest published superblock generation"),
                    "{} {}: unexpected problem {:?}",
                    layout.label(),
                    phase,
                    report.problems[0]
                );
                assert!(
                    report.problems[0]
                        .detail
                        .contains(&format!("generation {}", generation + 1)),
                    "{} {}: the tail must be the crashed generation",
                    layout.label(),
                    phase
                );
            } else {
                assert!(
                    report.ok,
                    "{} {phase}: verify problems {:?}",
                    layout.label(),
                    report.problems
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Idempotent retry after a crash
// ---------------------------------------------------------------------------

/// The uninterrupted reference: a fresh store, the same group, one commit.
fn uninterrupted_group() -> (tempfile::TempDir, Hash32, u64) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    group_create_alpha_beta(&mut store);
    let head = store.snapshot_root();
    let generation = store.generation();
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    (dir, head, generation)
}

#[test]
fn retry_after_objects_crash_reaches_the_uninterrupted_head() {
    let (_reference, reference_head, reference_generation) = uninterrupted_group();
    assert_eq!(reference_generation, 2);

    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    let generation = store.generation();
    assert_eq!(generation, 1);
    drop(store);

    let status = spawn_group_child(dir.path(), None, "create", "after_objects");
    assert!(!status.success(), "child was expected to abort");

    let mut store = Store::open_with_options_and_backend(
        dir.path(),
        OpenMode::ReadWrite,
        opts(),
        BackendKind::Local,
    )
    .unwrap();
    assert_eq!(
        store.generation(),
        generation,
        "a pre-commit crash must not publish a generation"
    );
    // The retry is the same logical change: same seeds, same bytes, same
    // fixed clock, same parent. The commit file was never written, so the
    // generation number is reused and the commit bytes are identical.
    group_create_alpha_beta(&mut store);
    assert_eq!(store.generation(), reference_generation);
    assert_eq!(
        store.snapshot_root(),
        reference_head,
        "the retried commit must be the uninterrupted commit"
    );
    let (alpha, beta) = read_alpha_beta(&store);
    assert_eq!(alpha, ALPHA_OLD);
    assert_eq!(beta, BETA_OLD);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn retry_after_commit_file_crash_has_one_visible_state() {
    let (_reference_dir, reference_head, reference_generation) = uninterrupted_group();

    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    drop(store);

    let status = spawn_group_child(dir.path(), None, "create", "after_commit_file");
    assert!(!status.success(), "child was expected to abort");

    let mut store = Store::open_with_options_and_backend(
        dir.path(),
        OpenMode::ReadWrite,
        opts(),
        BackendKind::Local,
    )
    .unwrap();
    assert_eq!(
        store.generation(),
        1,
        "an unpublished commit file must not be visible"
    );
    group_create_alpha_beta(&mut store);

    // The abandoned generation's commit file is never reused, so the retry is
    // a new generation. The *visible* state is still exactly the reference
    // state, and there is exactly one entry per path.
    assert_eq!(
        store.generation(),
        reference_generation + 1,
        "the retry must not reuse the abandoned generation"
    );
    assert_ne!(store.snapshot_root(), reference_head);
    let (alpha, beta) = read_alpha_beta(&store);
    assert_eq!(alpha, ALPHA_OLD);
    assert_eq!(beta, BETA_OLD);
    let root = store.resolve_path("/").unwrap();
    assert_eq!(store.list_dir(root).unwrap().len(), 2, "one entry per path");
    let lineage = store.commit_lineage(store.snapshot_root()).unwrap();
    let generations: Vec<u64> = lineage.iter().map(|entry| entry.generation).collect();
    assert_eq!(
        generations,
        vec![reference_generation + 1, 1],
        "the abandoned generation must not be on the visible parent chain"
    );
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn retry_after_published_crash_does_not_create_a_second_state() {
    let (_reference_dir, reference_head, reference_generation) = uninterrupted_group();

    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), opts()).unwrap();
    drop(store);

    let status = spawn_group_child(dir.path(), None, "create", "after_superblock_write");
    assert!(!status.success(), "child was expected to abort");

    let mut store = Store::open_with_options_and_backend(
        dir.path(),
        OpenMode::ReadWrite,
        opts(),
        BackendKind::Local,
    )
    .unwrap();
    assert_eq!(store.generation(), reference_generation);
    assert_eq!(
        store.snapshot_root(),
        reference_head,
        "the published crash must already be the uninterrupted commit"
    );

    // The at-least-once retry is refused as `Exists` (same seed, same name);
    // the client then proves the existing object already carries the intended
    // bytes. Nothing is published, so there is no second visible state.
    let root = store.resolve_path("/").unwrap();
    let before = store.generation();
    let err = store
        .create_file_seeded(
            root,
            b"alpha",
            ALPHA_OLD,
            0o644,
            0,
            0,
            BTreeMap::new(),
            &seed(b"dist-alpha"),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Exists, "unexpected error: {err}");
    assert_eq!(
        store.generation(),
        before,
        "a rejected retry writes nothing"
    );
    let alpha = store.lookup(root, b"alpha").unwrap();
    assert_eq!(store.read_object(alpha).unwrap(), ALPHA_OLD);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// 6. Unreachable store
// ---------------------------------------------------------------------------

#[test]
fn unreachable_endpoint_fails_closed_without_partial_bytes() {
    // Reserve a port and close it, so the connection is refused immediately.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = S3Config {
        endpoint: format!("http://127.0.0.1:{port}"),
        bucket: "distributed-none".to_owned(),
        region: "us-east-1".to_owned(),
        access_key: "unreachable".to_owned(),
        secret_key: "unreachable".to_owned(),
        session_token: None,
        path_style: true,
        lock_lease: Duration::from_secs(2),
        request_timeout: Duration::from_secs(2),
    };
    let kind = BackendKind::S3 { config };

    let err = match Store::open_with_options_and_backend(
        Path::new("<s3-unreachable>"),
        OpenMode::ReadOnly,
        opts(),
        kind.clone(),
    ) {
        Ok(_) => panic!("an unreachable endpoint must not open"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::Io, "unexpected error: {err}");
    assert!(
        !err.message().is_empty(),
        "a transport failure must carry a diagnostic"
    );

    let err = match Store::init_with_backend(Path::new("<s3-unreachable>"), opts(), kind) {
        Ok(_) => panic!("an unreachable endpoint must not initialise"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::Io, "unexpected error: {err}");
}

// ---------------------------------------------------------------------------
// 7. GC race
// ---------------------------------------------------------------------------

#[test]
fn gc_race_keeps_snapshot_reader_checkpoint_and_provenance_exact() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let root = writer.resolve_path("/").unwrap();

    let keep = create(&mut writer, b"keep", b"keep-payload-000000", b"gc-keep");
    let input = create(&mut writer, b"input", b"input-payload-00000", b"gc-input");
    let keep_meta = writer.object_meta(keep).unwrap();
    let input_meta = writer.object_meta(input).unwrap();

    // A provenance record keeps its output, its declared inputs, and the
    // receipt blob alive for as long as the output is retained.
    let record = ProvenanceRecord::new(
        "derive",
        BTreeMap::new(),
        vec![ProvenanceInput::new(
            "source",
            input_meta.content,
            Some(input),
        )],
        vec![keep_meta.content],
        ProvenanceEnvironment::new("distributed-test", "1.0"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        FIXED_TS,
    )
    .unwrap();
    let provenance = writer.record_provenance(&[keep], &record).unwrap();

    let pinned = writer.snapshot_create("pinned").unwrap();
    let checkpoint = writer.checkpoint().unwrap();

    // Unique garbage: created, then unlinked, so only GC can reclaim it.
    for index in 0..24 {
        let name = format!("garbage-{index:02}");
        let payload = format!("garbage-payload-{index:02}-xxxxxxxx");
        let tag = format!("gc-garbage-{index:02}");
        let _ = create(
            &mut writer,
            name.as_bytes(),
            payload.as_bytes(),
            tag.as_bytes(),
        );
    }
    for index in 0..24 {
        let name = format!("garbage-{index:02}");
        writer.unlink(root, name.as_bytes()).unwrap();
    }

    let expected = sha256(b"keep-payload-000000");
    let reader_path = dir.path().to_path_buf();
    let reader_id = keep;
    let reader_content = keep_meta.content;
    let reader_size = keep_meta.size;
    let opened_generation = writer.generation();
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let stop_reader = stop.clone();
    let reads_reader = reads.clone();
    let reader = thread::spawn(move || {
        let store = Store::open(&reader_path, OpenMode::ReadOnly).unwrap();
        assert_eq!(store.generation(), opened_generation);
        assert_eq!(
            store.read_object(reader_id).unwrap(),
            b"keep-payload-000000"
        );
        while !stop_reader.load(Ordering::Relaxed) {
            // Addressed by the pinned content root, exactly as a snapshot
            // materializer addresses it. The reader never reopens.
            let bytes = store
                .read_content(reader_content, 0, reader_size)
                .expect("pinned content read");
            assert_eq!(sha256(&bytes), expected, "reader observed changed bytes");
            reads_reader.fetch_add(1, Ordering::Relaxed);
        }
    });

    let report = writer.gc(true).unwrap();
    assert!(
        report.problems.is_empty(),
        "gc problems: {:?}",
        report.problems
    );
    assert!(
        !report.deleted.is_empty(),
        "the gc race must actually have garbage to reclaim"
    );
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader thread");
    assert!(reads.load(Ordering::Relaxed) > 0, "no read ran during gc");

    // Every pinned file still reads exactly, the snapshot state is intact, and
    // the provenance receipt and its input survived.
    let state = writer.state_at(pinned).unwrap();
    for (id, record) in &state.objects {
        if record.kind != weftfs::state::ObjectKind::File {
            continue;
        }
        let bytes = writer.read_object_at(Some(pinned), *id).unwrap();
        assert_eq!(bytes.len() as u64, record.size, "pinned size for {id}");
    }
    assert_eq!(
        writer.read_object_at(Some(pinned), keep).unwrap(),
        b"keep-payload-000000"
    );
    assert_eq!(
        writer.read_object_at(Some(pinned), input).unwrap(),
        b"input-payload-00000"
    );
    assert!(
        writer.current_state().objects.contains_key(&input),
        "the provenance input is a root while the output is retained"
    );
    assert!(writer
        .provenance_records_of(keep)
        .unwrap()
        .iter()
        .any(|(id, _)| *id == provenance));
    let verify = writer.verify(true).unwrap();
    assert!(verify.ok, "problems: {:?}", verify.problems);
    // The checkpoint manifest is still present and readable.
    assert!(writer.state_at(checkpoint).is_ok());
}

// ---------------------------------------------------------------------------
// 8. Authorization by content hash
// ---------------------------------------------------------------------------

fn envelope_principal(store: &mut Store) -> (Principal, Hash32, Hash32) {
    // Two files: `visible` is world-readable and selected by the granted root;
    // `secret` is also world-readable (POSIX allows it) but outside the root.
    let root = store.resolve_path("/").unwrap();
    let visible = store
        .create_file_seeded(
            root,
            b"visible",
            b"visible-payload-0000",
            0o644,
            0,
            0,
            attrs(&[("tenant", "a")]),
            &seed(b"cap-visible"),
        )
        .unwrap();
    let secret = store
        .create_file_seeded(
            root,
            b"secret",
            b"secret-payload-00000",
            0o644,
            0,
            0,
            BTreeMap::new(),
            &seed(b"cap-secret"),
        )
        .unwrap();
    let snapshot = store.snapshot_create("cap-snapshot").unwrap();
    let selector = selector::parse("select attrs.tenant == \"a\"").unwrap();
    let context = ContextRoot::new(
        &selector,
        snapshot,
        DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .unwrap();
    store
        .save_context_root("visible-view", &context, None, false)
        .unwrap();
    let context_id = context.id().unwrap();

    let signer_seed = [7u8; 32];
    let issuer = public_key_for_seed(&signer_seed).unwrap();
    let envelope = CapabilityEnvelope::new(
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
    .unwrap();
    let blob = sign_envelope(&envelope, &signer_seed).unwrap();
    let blob_bytes = blob.encode().unwrap();
    store.save_capability_envelope(&blob).unwrap();
    let verified = verify_envelope(&blob_bytes, &[], FIXED_TS, None).unwrap();
    (Principal::root().with_envelope(verified), visible, secret)
}

#[test]
fn content_hash_knowledge_does_not_authorize_retrieval() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let (principal, visible, secret) = envelope_principal(&mut store);

    let visible_meta = store.object_meta(visible).unwrap();
    let secret_meta = store.object_meta(secret).unwrap();

    // POSIX alone would allow the secret: the denial below is the capability,
    // not mode bits.
    assert!(
        weftfs::auth::can_read(&secret_meta, &Principal::root()),
        "the secret object must be POSIX-readable"
    );

    // Knowing the secret's ContentId is not authority. The answer is the
    // existing non-disclosing authorization error.
    let err = store
        .exact_id_object(secret_meta.content, None, &principal, FIXED_TS)
        .unwrap_err();
    assert_eq!(err.class(), Class::NotFound);
    assert_eq!(err.message(), "no such object");

    // An absent hash produces the identical class and text, so existence is
    // not revealed.
    let absent = Hash32::from_bytes([0xAB; 32]);
    let absent_err = store
        .exact_id_object(absent, None, &principal, FIXED_TS)
        .unwrap_err();
    assert_eq!(absent_err.class(), err.class());
    assert_eq!(absent_err.message(), err.message());

    // The granted object *is* addressable by its content hash.
    let record = store
        .exact_id_object(visible_meta.content, None, &principal, FIXED_TS)
        .unwrap();
    assert_eq!(record.id, visible);

    // A token minted for the visible object cannot read a different content
    // root that the caller happens to know the hash of.
    let token = store
        .authorize_read(visible, &Principal::root(), FIXED_TS)
        .unwrap();
    let err = store
        .read_content_authorized(&token, secret_meta.content, 0, secret_meta.size)
        .unwrap_err();
    assert_eq!(err.class(), Class::Unauthorized);
    assert_eq!(
        err.message(),
        "authorization token does not cover this content root"
    );

    // No denial text mentions the secret's name or bytes.
    assert!(!err.message().contains("secret"));
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}
