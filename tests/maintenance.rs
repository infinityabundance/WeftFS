//! T080 — bounded background maintenance.
//!
//! What is pinned here:
//!
//! * the budget is enforced (a batch never exceeds its byte cap) and the
//!   achieved rate and duty cycle are recorded, not assumed;
//! * a persisted plan resumes from its cursor instead of restarting;
//! * a stale plan can never delete a blob that became reachable again;
//! * a candidate larger than `max_pass_bytes` is a typed refusal with nothing
//!   deleted;
//! * two concurrent passes reclaim each candidate at most once and lose no
//!   reachable object;
//! * the ops surface is read-only without `--apply`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tempfile::tempdir;
use weftfs::error::Class;
use weftfs::maintenance::{
    maintenance_run, MaintenanceBudget, MaintenancePlanRecord, MaintenanceState,
};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

/// Create `count` files of `bytes` bytes each and unlink them, so their blobs
/// are unreachable but still on disk. Also leaves three live files.
fn populate(store: &mut Store, count: usize, bytes: usize) -> Vec<u64> {
    let root = store.resolve_path("/").unwrap();
    let dir = store.mkdir(root, b"garbage", 0o755, 0, 0).unwrap();
    let mut lengths = Vec::new();
    for index in 0..count {
        let name = format!("gone-{index:04}");
        store
            .create_file(
                dir,
                name.as_bytes(),
                &vec![(index % 251) as u8; bytes],
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    for index in 0..count {
        let name = format!("gone-{index:04}");
        store.unlink(dir, name.as_bytes()).unwrap();
        lengths.push(bytes as u64);
    }
    let live = store.mkdir(root, b"live", 0o755, 0, 0).unwrap();
    for index in 0..3 {
        store
            .create_file(
                live,
                format!("live-{index}").as_bytes(),
                format!("live artifact {index}").as_bytes(),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    lengths
}

fn small_budget() -> MaintenanceBudget {
    MaintenanceBudget {
        bytes_per_second: 512 * 1024,
        cpu_share_percent: 100,
        max_batch_bytes: 32 * 1024,
        max_pass_bytes: 4 * 1024 * 1024,
        min_batch_bytes: 4 * 1024,
    }
}

/// The three named live artifacts, by content: nothing maintenance does may
/// change them.
fn live_artifacts(store: &Store) -> Vec<(String, Vec<u8>)> {
    let live = store.resolve_components(&[b"live"]).unwrap();
    let mut out = Vec::new();
    for index in 0..3 {
        let name = format!("live-{index}");
        let id = store.lookup(live, name.as_bytes()).unwrap();
        out.push((name, store.read_object(id).unwrap()));
    }
    out
}

#[test]
fn a_bounded_run_reclaims_only_unreachable_blobs_and_reports_its_rate() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 120, 4096);
    let before = live_artifacts(&store);
    let plan = store.maintenance_plan().unwrap();
    assert!(
        plan.candidates.len() >= 100,
        "expected the unlinked blobs to be unreachable, got {}",
        plan.candidates.len()
    );
    let planned_bytes: u64 = plan.candidates.iter().map(|c| c.bytes).sum();
    let budget = small_budget();
    let run = maintenance_run(&mut store, &budget, 64, true).unwrap();
    assert_eq!(run.status, "complete", "{run:?}");
    assert!(run.batches > 1, "a small budget must take several batches");
    assert_eq!(run.reclaimed_bytes, planned_bytes);
    assert_eq!(run.cursor, run.candidates);
    assert!(run.measured_bytes_per_second > 0);
    let state = store.maintenance_state().unwrap().unwrap();
    assert!(state.last_duty_percent <= 100);
    // Every live artifact is still readable and unchanged.
    assert_eq!(before, live_artifacts(&store));
    // The store still verifies: no reachable object lost, no orphan record
    // naming a deleted blob.
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "verify: {report:?}");
    // A second run completes, and what it finds is the driver's own churn: the
    // previous version of the state record is a new unreachable manifest. That
    // is bounded by the number of state writes and is published as a loss.
    let again = maintenance_run(&mut store, &budget, 8, true).unwrap();
    assert_eq!(again.status, "complete");
    assert!(
        again.reclaimed_bytes < 64 * 1024,
        "self-churn should be small, got {}",
        again.reclaimed_bytes
    );
}

#[test]
fn progress_is_durable_and_a_restart_resumes() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 60, 4096);
    let budget = small_budget();
    let first = maintenance_run(&mut store, &budget, 1, true).unwrap();
    assert_eq!(first.batches, 1);
    assert_eq!(first.status, "running", "{first:?}");
    assert!(first.cursor > 0 && first.cursor < first.candidates);
    let state = store.maintenance_state().unwrap().unwrap();
    assert_eq!(state.cursor, first.cursor);
    assert_eq!(state.passes, 1);
    drop(store);

    // Reopen and resume: the persisted plan must still match and the cursor
    // must not restart at zero.
    // Reopen with the same options: `gc_grace_secs` is an option, not persisted
    // state, and a default grace would retain every young blob.
    let mut store =
        Store::open_with_options(dir.path().join("store"), OpenMode::ReadWrite, opts()).unwrap();
    let second = maintenance_run(&mut store, &budget, 1, true).unwrap();
    assert!(second.resumed, "a restart must resume the persisted plan");
    assert!(second.cursor > first.cursor);
    let state = store.maintenance_state().unwrap().unwrap();
    assert_eq!(state.passes, 2);
    assert!(state.reclaimed_bytes > 0);
}

#[test]
fn a_stale_plan_never_deletes_a_blob_that_became_reachable() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 20, 4096);
    let plan = store.maintenance_plan().unwrap();
    let candidate = plan
        .candidates
        .iter()
        .find(|candidate| candidate.kind == "object")
        .expect("an object candidate")
        .clone();
    // Read the candidate's bytes straight off the store's blob plane and make
    // them live again by creating a file with the same content: the chunk hash
    // is content-addressed, so the blob becomes reachable.
    let path = dir
        .path()
        .join("store/objects")
        .join(&candidate.hash.to_hex()[0..2])
        .join(candidate.hash.to_hex());
    let bytes = std::fs::read(&path).unwrap();
    let root = store.resolve_path("/").unwrap();
    let revived = store.mkdir(root, b"revived", 0o755, 0, 0).unwrap();
    store
        .create_file(
            revived,
            b"revived.bin",
            &bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    // The old plan still names the blob; the sweep must re-check reachability.
    let sweep = store.maintenance_reclaim(&[candidate.clone()]).unwrap();
    assert_eq!(sweep.deleted, 0, "a reachable blob must not be deleted");
    assert_eq!(sweep.skipped.len(), 1);
    assert!(sweep.skipped[0].contains("reachable again"));
    // And the revived file still reads.
    let revived_id = store.lookup(revived, b"revived.bin").unwrap();
    assert_eq!(store.read_object(revived_id).unwrap(), bytes);
}

#[test]
fn a_candidate_larger_than_the_budget_is_a_typed_refusal() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 8, 4096);
    let mut budget = small_budget();
    budget.max_pass_bytes = 512;
    budget.max_batch_bytes = 512;
    budget.min_batch_bytes = 64;
    let err = maintenance_run(&mut store, &budget, 4, true).unwrap_err();
    assert_eq!(err.class(), Class::BudgetExceeded, "{err}");
    assert!(err.message().contains("cannot be split"), "{err}");
    // Nothing was written or deleted.
    assert!(store.maintenance_state().unwrap().is_none());
    assert!(!store.maintenance_plan().unwrap().candidates.is_empty());
}

#[test]
fn two_concurrent_passes_reclaim_each_blob_once() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 80, 4096);
    let plan = store.maintenance_plan().unwrap();
    let planned_bytes: u64 = plan.candidates.iter().map(|c| c.bytes).sum();
    assert!(planned_bytes > 0);
    drop(store);

    // Both passes are handed the *same* candidate list, so any overlap in what
    // they reclaim is a genuine double-free and would show as a total above the
    // plan.
    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = dir.path().join("store");
        let candidates = plan.candidates.clone();
        handles.push(std::thread::spawn(move || {
            let mut options = opts();
            options.writer_mode = weftfs::mw2::WriterMode::Optimistic;
            let mut store =
                Store::open_with_options(&path, OpenMode::ReadWrite, options).unwrap();
            let mut total = 0u64;
            let mut busy = 0u64;
            let mut deleted = 0u64;
            for _ in 0..64 {
                match store.maintenance_reclaim(&candidates) {
                    Ok(sweep) => {
                        total += sweep.deleted_bytes;
                        deleted += sweep.deleted;
                        if deleted == candidates.len() as u64 {
                            break;
                        }
                    }
                    Err(err) if err.class() == Class::Busy => {
                        busy += 1;
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(err) => panic!("{err}"),
                }
            }
            (total, busy, deleted)
        }));
    }
    let mut total = 0u64;
    let mut busy = 0u64;
    let mut deleted = 0u64;
    for handle in handles {
        let (bytes, refusals, count) = handle.join().unwrap();
        total += bytes;
        busy += refusals;
        deleted += count;
    }
    // No double-free: the two passes together reclaimed each candidate once.
    assert_eq!(total, planned_bytes, "double-free or under-reclaim");
    assert_eq!(deleted, plan.candidates.len() as u64);
    // Reachable objects are all intact and the store verifies.
    let store =
        Store::open_with_options(dir.path().join("store"), OpenMode::ReadOnly, opts()).unwrap();
    assert_eq!(live_artifacts(&store).len(), 3);
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "verify: {report:?}");
    eprintln!(
        "COURT concurrent passes total_bytes={total} planned={planned_bytes} busy={busy} deleted={deleted}"
    );
}

#[test]
fn the_ops_surface_is_read_only_without_apply() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 10, 4096);
    let generation = store.generation();
    drop(store);
    let store =
        Store::open_with_options(dir.path().join("store"), OpenMode::ReadOnly, opts()).unwrap();
    let report = store.ops_report().unwrap();
    assert_eq!(report.generation, generation);
    assert!(report.state.is_none());
    let plan = report.plan.unwrap();
    assert!(!plan.candidates.is_empty());
    assert!(plan.reachable_chunks > 0);
    // A read-only report never creates the maintenance directory.
    assert!(!dir.path().join("store/maintenance").exists());
    // And nothing was deleted: a fresh read-only handle with the same options
    // sees the same plan.
    let plan_len = plan.candidates.len();
    drop(store);
    let store2 =
        Store::open_with_options(dir.path().join("store"), OpenMode::ReadOnly, opts()).unwrap();
    assert_eq!(store2.maintenance_plan().unwrap().candidates.len(), plan_len);
}

#[test]
fn the_state_and_plan_records_are_canonical() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    populate(&mut store, 10, 4096);
    maintenance_run(&mut store, &small_budget(), 1, true).unwrap();
    let state = store.maintenance_state().unwrap().unwrap();
    let encoded = state.encode().unwrap();
    let decoded = MaintenanceState::decode(&encoded).unwrap();
    assert_eq!(decoded, state);
    assert!(MaintenanceState::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut extended = encoded.clone();
    extended.push(0);
    assert!(MaintenanceState::decode(&extended).is_err());
    let plan = store.maintenance_plan_record().unwrap().unwrap();
    let decoded = MaintenancePlanRecord::decode(&plan.encode().unwrap()).unwrap();
    assert_eq!(decoded, plan);
    assert_eq!(decoded.root().unwrap(), plan.root().unwrap());
}

#[test]
fn an_invalid_budget_is_refused() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let mut budget = MaintenanceBudget::default();
    budget.bytes_per_second = 0;
    assert_eq!(
        maintenance_run(&mut store, &budget, 1, false)
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );
    let mut budget = MaintenanceBudget::default();
    budget.cpu_share_percent = 0;
    assert_eq!(
        maintenance_run(&mut store, &budget, 1, false)
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );
    let mut budget = MaintenanceBudget::default();
    budget.min_batch_bytes = budget.max_batch_bytes + 1;
    assert_eq!(
        maintenance_run(&mut store, &budget, 1, false)
            .unwrap_err()
            .class(),
        Class::InvalidArgument
    );
}
