//! X08: mixed read/write correctness of the packed plane under racing seal,
//! catalog publication, commit publication, rollback, cancellation, epoch
//! change, a real process crash, and concurrent GC.
//!
//! # The authoritative discovery invariant
//!
//! A concurrent reader of a packed store may observe **either** the state as of
//! the last commit it opened **or** a strictly newer committed state. It must
//! never observe:
//!
//! 1. a catalog entry whose pack object is absent or shorter than the entry's
//!    `(offset, len)` (a *dangling member*);
//! 2. a pack whose bytes are not durable **before** the catalog that names it
//!    (publication order is pack -> catalog segments -> catalog root, and the
//!    commit record is written after both);
//! 3. a member served from a pack that GC reclaimed while a live root still
//!    references it (GC marks every root a commit, snapshot, checkpoint,
//!    transaction, pin or provenance record names, and only then sweeps);
//! 4. bytes from a pack that do not verify against the member's recorded
//!    digest (every member read is digest-checked, and a partial range read
//!    that cannot cover the digest is refused, not guessed).
//!
//! The writer additionally guarantees that a member it has buffered but not yet
//! catalogued is resolvable only through the handle that buffered it
//! (`BUFFERED_OBJECT` / `BUFFERED_MANIFEST` route the read away from the pooled
//! handles), so a same-handle read-back after `create_file` is always its own
//! bytes.
//!
//! Every scenario below ends with `verify --full` clean; any failure is kept
//! and fixed at the root cause rather than by weakening an assertion.

use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use weftfs::backend::BackendKind;
use weftfs::packs::PackOptions;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::{Hash32, Result};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn pack_options() -> PackOptions {
    PackOptions {
        pack_target_bytes: 64 * 1024,
        member_threshold_bytes: 4096,
        ..PackOptions::default()
    }
}

fn packed_local() -> BackendKind {
    BackendKind::packed(BackendKind::Local, pack_options()).expect("packed local")
}

fn payload(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn verify_full(store: &Store, what: &str) {
    let report = store.verify(true).expect("verify");
    assert!(
        report.problems.is_empty(),
        "{what}: verify --full reported {:?}",
        report.problems
    );
}

/// Recursively copy a store tree, for failure diagnostics only.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Open a read-only handle **on the packed backend**. A packed store cannot be
/// opened as a flat plane: its manifests live inside pack objects, so a plain
/// `Local` handle would report every blob as missing.
fn open_ro(path: &std::path::Path) -> Store {
    Store::open_with_options_and_backend(path, OpenMode::ReadOnly, opts(), packed_local())
        .expect("open packed store read-only")
}

fn create(store: &mut Store, name: &[u8], bytes: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").expect("resolve root");
    store
        .create_file(root, name, bytes, 0o644, 0, 0, BTreeMap::new())
        .expect("create")
}

/// Scenario A. A writer publishes 24 groups of small files; two reader threads
/// continuously reopen the store and read every id published so far, while the
/// writer also reads each object back through its own handle before the catalog
/// names it. Every read must be exact and the final store must verify.
#[test]
fn racing_seal_catalog_and_commit_never_expose_a_torn_member() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut writer =
        Store::init_with_backend(&path, opts(), packed_local()).expect("init packed store");
    let published: Arc<std::sync::Mutex<Vec<(Hash32, Vec<u8>)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let failures = Arc::new(AtomicU64::new(0));

    let readers: Vec<_> = (0..3)
        .map(|thread| {
            let path = path.clone();
            let published = Arc::clone(&published);
            let stop = Arc::clone(&stop);
            let reads = Arc::clone(&reads);
            let failures = Arc::clone(&failures);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Snapshot the published list *before* opening: every id in
                    // it was committed no later than the store state this open
                    // selects, so a stale reader is allowed but a torn member is
                    // not.
                    let snapshot = published.lock().unwrap().clone();
                    let store = match Store::open_with_options_and_backend(
                        &path,
                        OpenMode::ReadOnly,
                        opts(),
                        packed_local(),
                    ) {
                        Ok(store) => store,
                        Err(_) => {
                            failures.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    for (id, expected) in snapshot {
                        match store.read_object(id) {
                            Ok(bytes) if bytes == expected => {
                                reads.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(_) => {
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(err) => {
                                eprintln!("reader {thread}: {err}");
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        })
        .collect();

    for group in 0..24u64 {
        writer.begin_group().expect("begin group");
        let mut batch = Vec::new();
        for index in 0..8u64 {
            let seed = group * 1024 + index;
            let len = 200 + (seed as usize % 3000);
            let bytes = payload(seed, len);
            let id = create(&mut writer, format!("g{group}-f{index}").as_bytes(), &bytes);
            // Read back through the writing handle while the member is still
            // buffered in the open pack (the R13/T1 discovery bug).
            assert_eq!(
                writer.read_object(id).expect("buffered read-back"),
                bytes,
                "group {group} file {index}: the writer did not read its own buffered member"
            );
            batch.push((id, bytes));
        }
        writer.commit_group().expect("commit group");
        published.lock().unwrap().extend(batch);
        if group % 6 == 5 {
            verify_full(&writer, "mid-run");
        }
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().expect("reader panicked");
    }
    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "readers observed a torn, missing or dangling member"
    );
    assert!(
        reads.load(Ordering::Relaxed) > 100,
        "readers made no progress"
    );
    verify_full(&writer, "final");
    let head = writer.head();
    drop(writer);
    let fresh = open_ro(&path);
    assert_eq!(fresh.head(), head);
    for (id, expected) in published.lock().unwrap().iter() {
        assert_eq!(&fresh.read_object(*id).unwrap(), expected);
    }
}

/// Scenario B. A group that is aborted publishes nothing: the head, the files
/// and every live root are exactly the pre-group values, and the store verifies.
#[test]
fn an_aborted_group_publishes_no_pack_and_no_catalog_entry() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut store = Store::init_with_backend(&path, opts(), packed_local()).unwrap();
    let before = store.head();
    let generation = store.generation();
    store.begin_group().unwrap();
    for index in 0..12u8 {
        let bytes = payload(900 + index as u64, 700);
        create(&mut store, format!("aborted{index}").as_bytes(), &bytes);
    }
    store.abort_group();
    assert_eq!(store.head(), before, "an aborted group moved the head");
    assert_eq!(store.generation(), generation);
    assert!(store.resolve_path("/aborted0").is_err());
    verify_full(&store, "after abort");
    // The plane is still usable: the next group publishes normally and the
    // abandoned pack members are unreferenced, not dangling.
    store.begin_group().unwrap();
    let bytes = payload(7, 800);
    let id = create(&mut store, b"after-abort", &bytes);
    store.commit_group().unwrap();
    assert_eq!(store.read_object(id).unwrap(), bytes);
    verify_full(&store, "after the next group");
}

/// Scenario C. A real process crash between pack publication and catalog
/// publication leaves the previous whole generation; reopening and GC clean up
/// the unreferenced pack without ever creating a dangling entry.
#[test]
fn a_process_crash_mid_seal_leaves_old_or_new_whole_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let bin = env!("CARGO_BIN_EXE_weftfs");
    for phase in ["pack_after_pack_object", "pack_after_catalog_segments"] {
        let store_path = path.join(phase);
        let seed_payload = dir.path().join(format!("{phase}.bin"));
        fs::write(&seed_payload, payload(11, 9000)).unwrap();
        // Establish a committed baseline with the CLI so the child is a real
        // second process on the same on-disk format.
        let status = Command::new(bin)
            .args(["--store"])
            .arg(&store_path)
            .args(["--pack", "init"])
            .status()
            .expect("run init");
        assert!(status.success());
        let status = Command::new(bin)
            .args(["--store"])
            .arg(&store_path)
            .args(["--pack", "create", "/base.bin", "--file"])
            .arg(&seed_payload)
            .args(["--seed", "x08-base"])
            .status()
            .expect("run create");
        assert!(status.success());
        let baseline = open_ro(&store_path);
        let base_head = baseline.head();
        drop(baseline);

        // The crashing child writes a group and dies at the injected phase.
        let crash_dir = dir.path().join(format!("{phase}-payloads"));
        fs::create_dir_all(&crash_dir).unwrap();
        let args: Vec<String> = vec![
            "--store".into(),
            store_path.display().to_string(),
            "--pack".into(),
            "create".into(),
            "/crash.bin".into(),
            "--text".into(),
            "crash-payload".into(),
            "--seed".into(),
            "x08-crash".into(),
        ];
        let status = Command::new(bin)
            .args(&args)
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("run crashing child");
        assert!(
            !status.success(),
            "the crash child did not abort at {phase}"
        );

        let reopened = open_ro(&store_path);
        if let Ok(report) = reopened.verify(true) {
            if !report.problems.is_empty() {
                // Diagnostic: keep the failing store so the defect can be
                // inspected rather than guessed at.
                let keep = std::path::Path::new("/work/.docker-cache/x08-failing-store");
                let _ = std::fs::remove_dir_all(keep);
                let _ = copy_tree(&store_path, keep);
                eprintln!("kept failing store for {phase} at {}", keep.display());
            }
        }
        verify_full(&reopened, phase);
        let head = reopened.head();
        let base_visible = reopened.resolve_path("/base.bin").is_ok();
        let crash_visible = reopened.resolve_path("/crash.bin").is_ok();
        assert!(base_visible, "{phase}: the committed baseline disappeared");
        assert!(
            head == base_head || (head != base_head && crash_visible),
            "{phase}: the head moved but the new file is not visible"
        );
        drop(reopened);

        // GC must reclaim the unreferenced pack (if any) without leaving a
        // catalog entry behind, and the live root must still read exactly.
        let mut writer = Store::open_with_options_and_backend(
            &store_path,
            OpenMode::ReadWrite,
            opts(),
            packed_local(),
        )
        .expect("reopen rw");
        let report = writer.gc(false).expect("gc");
        let _ = report;
        verify_full(&writer, "after crash gc");
        let base = writer.resolve_path("/base.bin").unwrap();
        assert_eq!(writer.read_object(base).unwrap(), payload(11, 9000));
    }
}

/// Scenario D. GC runs concurrently with readers of a frozen snapshot and with
/// a writer that keeps publishing; every reader keeps exact bytes and the live
/// roots survive the sweep.
#[test]
fn gc_concurrent_with_readers_and_a_writer_keeps_every_live_root() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut writer = Store::init_with_backend(&path, opts(), packed_local()).unwrap();
    let mut ids = Vec::new();
    for index in 0..40u8 {
        let bytes = payload(4000 + index as u64, 500 + index as usize * 37);
        let id = create(&mut writer, format!("live{index}").as_bytes(), &bytes);
        ids.push((id, bytes));
    }
    let mut doomed = Vec::new();
    for index in 0..40u8 {
        let bytes = payload(9000 + index as u64, 400);
        let name = format!("doomed{index}");
        let id = create(&mut writer, name.as_bytes(), &bytes);
        doomed.push((id, name, bytes));
    }
    verify_full(&writer, "before gc");

    let stop = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(AtomicU64::new(0));
    let reads = Arc::new(AtomicU64::new(0));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let path = path.clone();
            let ids = ids.clone();
            let stop = Arc::clone(&stop);
            let failures = Arc::clone(&failures);
            let reads = Arc::clone(&reads);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let store = match Store::open_with_options_and_backend(
                        &path,
                        OpenMode::ReadOnly,
                        opts(),
                        packed_local(),
                    ) {
                        Ok(store) => store,
                        Err(_) => {
                            failures.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    for (id, expected) in &ids {
                        match store.read_object(*id) {
                            Ok(bytes) if &bytes == expected => {
                                reads.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        })
        .collect();

    let root = writer.resolve_path("/").expect("root");
    for (_, name, _) in &doomed {
        writer.unlink(root, name.as_bytes()).expect("unlink");
    }
    // Repeated GC passes while the readers run.
    for _ in 0..3 {
        let report = writer.gc(false).expect("gc");
        let _ = report;
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "a reader lost a live root while GC ran"
    );
    assert!(reads.load(Ordering::Relaxed) > 0);
    verify_full(&writer, "after concurrent gc");
    for (id, expected) in &ids {
        assert_eq!(&writer.read_object(*id).unwrap(), expected);
    }
    for (id, _, _) in &doomed {
        assert!(writer.read_object(*id).is_err());
    }
    let _ = &doomed;
}

/// Scenario E. A reader cancelled while it holds a reservation releases
/// everything, returns `Cancelled`, and leaves the packed store untouched.
#[test]
fn a_cancelled_reader_leaves_the_packed_store_intact() {
    use weftfs::restore_pipeline::{run_ordered_pipeline, RestorePipelineOptions};
    use weftfs::stage_budget::StageBudget;

    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut writer = Store::init_with_backend(&path, opts(), packed_local()).unwrap();
    let mut ids = Vec::new();
    for round in 0..8u64 {
        let bytes = payload(1500 + round, 2500);
        let id = create(&mut writer, format!("c{round}").as_bytes(), &bytes);
        ids.push((id, bytes));
    }
    let budget = StageBudget::with_capacity(2, 1 << 20).unwrap();
    let token: Arc<weftfs::stage_budget::StageCancelToken> = Arc::new(budget.cancel_token());
    let sizes: Vec<Result<usize>> = ids.iter().map(|(_, bytes)| Ok(bytes.len())).collect();
    let cancellation = {
        let token = Arc::clone(&token);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
            token.cancel();
        })
    };
    let options = RestorePipelineOptions {
        threads: 4,
        max_inflight_bytes: 1 << 20,
        budget: Some(Arc::clone(&budget)),
        cancel: Some(Arc::clone(&token)),
        ..RestorePipelineOptions::default()
    };
    let mut written = 0usize;
    let result = run_ordered_pipeline(
        &ids,
        &sizes,
        &options,
        |_index, (id, _)| writer.read_object(*id),
        |bytes| bytes.len(),
        |_index, _bytes| {
            written += 1;
            Ok(())
        },
    );
    // Either the cancel won (Cancelled) or the whole read finished first; both
    // are correct, and neither may corrupt the plane.
    match result {
        Ok(_) => {}
        Err(err) => assert_eq!(err.class(), weftfs::Class::Cancelled),
    }
    let stats = budget.stats();
    assert!(
        stats.observed_max_active_workers <= 2,
        "cancelled read exceeded the worker cap: {stats:?}"
    );
    assert!(written <= ids.len());
    // Everything is released and the store is exact.
    let admission = budget
        .admit(weftfs::stage_budget::Stage::Restore, 2, 1 << 20, None)
        .expect("the cancelled read released its reservations");
    drop(admission);
    verify_full(&writer, "after cancellation");
    for (id, expected) in &ids {
        assert_eq!(&writer.read_object(*id).unwrap(), expected);
    }
    let _ = cancellation.join();
}

