//! X09 — writer session lease: expiry, renewal, fencing and fairness.
//!
//! Every case runs two or more independent store handles against one local
//! store in `optimistic` (MW2) mode, which is the only mode where a lease can
//! expire: MW1's authority is the kernel lock a live process holds, and a config
//! that asked for a lease there is refused (`single_writer_mode_refuses_a_lease`).
//!
//! A lease TTL below one second is rejected by the contract, so these tests use
//! a one-second lease and wait it out. The wall time is stated rather than
//! hidden: the expiry cases take ~1.2 s each.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::writer_lease::{LeaseState, WriterLeaseOptions};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const TTL: Duration = Duration::from_secs(1);

fn base_options(mode: WriterMode) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn lease_options(ttl: Duration, keeper: bool, fairness: bool, wait: Duration) -> Options {
    Options {
        writer_lease: Some(WriterLeaseOptions {
            ttl,
            keeper,
            fairness,
            // A queued ticket must outlive the wait it is protecting, or a
            // slow waiter would silently lose its place.
            ticket_ttl: wait + Duration::from_secs(1),
            wait,
        }),
        ..base_options(WriterMode::Optimistic)
    }
}

fn new_store() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = Store::init(&path, base_options(WriterMode::Single)).unwrap();
        drop(store);
    }
    (dir, path)
}

fn open(path: &std::path::Path, options: Options) -> Store {
    Store::open_with_options(path, OpenMode::ReadWrite, options).unwrap()
}

fn try_open(path: &std::path::Path, options: Options) -> weftfs::error::Result<Store> {
    Store::open_with_options(path, OpenMode::ReadWrite, options)
}

/// A one-file transaction plan. Prepared while the lease is live so the test can
/// separate "immutable blob publication" from "control-state publication".
struct Plan {
    record: ObjectRecord,
    entry: EntryRecord,
}

fn plan(store: &Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> Plan {
    let (manifest, _content, size) = store.prepare_content(bytes).unwrap();
    let id = weftfs::identity::object_id_from_seed(seed);
    let version = derive_version_id(id, manifest, size, NOW, None).unwrap();
    Plan {
        record: ObjectRecord {
            id,
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: NOW,
            version,
            content: manifest,
            size,
            attrs: BTreeMap::new(),
        },
        entry: EntryRecord {
            id: derive_entry_id(State::ROOT_ID, name).unwrap(),
            parent: State::ROOT_ID,
            name: name.to_vec(),
            target: id,
        },
    }
}

fn stage(store: &Store, id: &str, plan: &Plan) -> weftfs::mw2::Transaction {
    let mut tx = store.begin_transaction(id).unwrap();
    let state = store.current_state();
    tx.put_object(state, plan.record.clone()).unwrap();
    tx.put_entry(state, plan.entry.clone()).unwrap();
    tx.set_tip(state, plan.record.id, plan.record.version)
        .unwrap();
    tx
}

fn commit_files(path: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path.join("commits"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// 1. An idle holder expires and the successor acquires.
// ---------------------------------------------------------------------------

#[test]
fn idle_holder_expires_and_successor_acquires() {
    let (_dir, path) = new_store();
    let mut a = open(
        &path,
        lease_options(TTL, false, true, Duration::from_millis(200)),
    );
    let fence_a = a.writer_lease_view().expect("a holds a lease").fence;
    let record_a = a.writer_lease_record().unwrap().expect("record");
    assert_eq!(record_a.state, LeaseState::Held);
    assert_eq!(record_a.fence, fence_a);
    assert!(record_a.is_live(record_a.acquired_ns));

    // A stays *alive* and does nothing at all. This is exactly the defect: a
    // live, idle session used to block a successor indefinitely.
    std::thread::sleep(TTL + Duration::from_millis(250));

    let mut b = open(
        &path,
        lease_options(TTL, false, true, Duration::from_secs(3)),
    );
    let fence_b = b.writer_lease_view().expect("b holds a lease").fence;
    assert_eq!(
        fence_b,
        fence_a + 1,
        "a takeover always increments the fencing token"
    );
    assert!(b.writer_lease_stats().unwrap().takeovers >= 1);

    // The store is usable by the successor.
    let plan_b = plan(&b, b"from-b", b"beta", b"seed-b");
    let tx_b = stage(&b, "0000000000000000000000000000000b", &plan_b);
    b.execute_transaction(&tx_b, &TransactionOptions::default())
        .expect("the successor publishes");
    let _ = &mut a;
}

// ---------------------------------------------------------------------------
// 2. A live, renewing holder is never stolen.
// ---------------------------------------------------------------------------

#[test]
fn live_renewing_holder_is_never_stolen() {
    let (_dir, path) = new_store();
    let a = open(
        &path,
        lease_options(TTL, true, true, Duration::from_millis(200)),
    );
    let fence_a = a.writer_lease_view().unwrap().fence;
    let ticks_before = a.writer_lease_stats().unwrap().renewals;

    // Five successor attempts across three lease lifetimes. Each one may wait,
    // but must never acquire: the keeper renews the live session on a timer.
    let deadline = Instant::now() + TTL * 3;
    let mut refusals = 0;
    while Instant::now() < deadline {
        let err = match try_open(
            &path,
            lease_options(TTL, false, true, Duration::from_millis(400)),
        ) {
            Ok(successor) => {
                let fence = successor.writer_lease_view().map(|lease| lease.fence);
                panic!("a live renewing holder was stolen: successor fence {fence:?}, holder {fence_a}");
            }
            Err(err) => err,
        };
        assert_eq!(err.class(), Class::Busy, "{err}");
        assert!(err.message().contains("live writer lease"), "{err}");
        refusals += 1;
    }
    assert!(refusals >= 3, "only {refusals} successor attempts ran");
    let renewals = a.writer_lease_stats().unwrap().renewals;
    assert!(
        renewals > ticks_before,
        "the keeper never renewed: {renewals} <= {ticks_before}"
    );
    let record = a.writer_lease_record().unwrap().unwrap();
    assert_eq!(record.owner, a.writer_lease_view().unwrap().owner);
    assert_eq!(record.fence, fence_a, "the fence did not move");
    // The published deadline is still in the future.
    assert!(record.deadline_ns > weftfs::writer_lease::wall_now_ns());
}

// ---------------------------------------------------------------------------
// 3. The expired holder's next publish is refused, typed, and mutates nothing.
// ---------------------------------------------------------------------------

#[test]
fn expired_holder_next_publish_is_refused_and_mutates_nothing() {
    let (_dir, path) = new_store();
    let mut a = open(
        &path,
        lease_options(TTL, false, true, Duration::from_millis(200)),
    );
    // Prepare the *blobs* while the lease is live. Blobs are immutable and
    // content-addressed; preparing one is not a state mutation.
    let plan_a = plan(&a, b"late", b"delta", b"seed-a");
    let head_before = a.head();
    let generation_before = a.generation();
    let commits_before = commit_files(&path);

    std::thread::sleep(TTL + Duration::from_millis(250));

    // (a) Even the prepare half is refused once the lease has expired.
    let err = a.prepare_content(b"late-blob").unwrap_err();
    assert_eq!(err.class(), Class::LeaseExpired, "{err}");

    // (b) The publication itself is refused, with a typed class.
    let tx = stage_after_expiry(&mut a, &plan_a);
    let err = a
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::LeaseExpired, "{err}");
    assert!(err.message().contains("expired"), "{err}");

    // (c) Nothing observable changed: same head, same generation, no commit
    // record for the attempted generation.
    assert_eq!(
        a.head(),
        head_before,
        "a refused publication moved the head"
    );
    assert_eq!(a.generation(), generation_before);
    assert_eq!(
        commit_files(&path),
        commits_before,
        "a commit file appeared"
    );
    let record = a.writer_lease_record().unwrap().unwrap();
    assert_eq!(record.fence, a.writer_lease_view().unwrap().fence);

    // (d) A successor publishes at the next fence and the store verifies.
    std::mem::forget(a); // no release tombstone: the record expires on its own
    let mut b = open(
        &path,
        lease_options(TTL, false, true, Duration::from_secs(3)),
    );
    let plan_b = plan(&b, b"after", b"epsilon", b"seed-b");
    let tx_b = stage(&b, "0000000000000000000000000000000e", &plan_b);
    b.execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap();
    assert!(b.verify(true).unwrap().ok);
}

/// Build a transaction without calling `prepare_content` (which is refused once
/// the lease expired). The plan's blobs were prepared earlier.
fn stage_after_expiry(store: &mut Store, plan: &Plan) -> weftfs::mw2::Transaction {
    let mut tx = store
        .begin_transaction("0000000000000000000000000000000d")
        .unwrap();
    let state = store.current_state();
    tx.put_object(state, plan.record.clone()).unwrap();
    tx.put_entry(state, plan.entry.clone()).unwrap();
    tx.set_tip(state, plan.record.id, plan.record.version)
        .unwrap();
    tx
}

// ---------------------------------------------------------------------------
// 4. An abrupt death releases nothing; a graceful close releases immediately.
// ---------------------------------------------------------------------------

#[test]
fn graceful_close_releases_and_abrupt_death_expires() {
    let (_dir, path) = new_store();

    // Graceful close: the drop writes a release tombstone, so the successor
    // acquires at once, without waiting out the deadline.
    {
        let a = open(
            &path,
            lease_options(TTL, false, true, Duration::from_millis(200)),
        );
        let fence_a = a.writer_lease_view().unwrap().fence;
        assert!(a
            .writer_lease_record()
            .unwrap()
            .unwrap()
            .is_live(weftfs::writer_lease::wall_now_ns()));
        drop(a);
        let record = {
            let observer = Store::open_with_options(
                &path,
                OpenMode::ReadOnly,
                base_options(WriterMode::Optimistic),
            )
            .unwrap();
            observer.writer_lease_record().unwrap().unwrap()
        };
        assert_eq!(record.state, LeaseState::Released);
        assert_eq!(
            record.released_from.as_deref(),
            Some(record.released_from.as_deref().unwrap())
        );
        let started = Instant::now();
        let b = open(
            &path,
            lease_options(TTL, false, true, Duration::from_secs(1)),
        );
        assert!(started.elapsed() < TTL / 2, "release was not immediate");
        assert_eq!(b.writer_lease_view().unwrap().fence, fence_a + 1);
        std::mem::forget(b);
    }

    // Abrupt death proxy: `mem::forget` skips `Drop`, so no tombstone is
    // written — exactly what a SIGKILL leaves behind. The record stays live
    // until its deadline, then a successor takes over. (The court runs the real
    // SIGKILL case; see `scripts/writer_lease_court.sh`.)
    let record = {
        let observer = Store::open_with_options(
            &path,
            OpenMode::ReadOnly,
            base_options(WriterMode::Optimistic),
        )
        .unwrap();
        observer.writer_lease_record().unwrap().unwrap()
    };
    assert_eq!(record.state, LeaseState::Held, "forget wrote no tombstone");
    let err = try_open(
        &path,
        lease_options(TTL, false, true, Duration::from_millis(100)),
    )
    .err()
    .expect("a live record refuses");
    assert_eq!(err.class(), Class::Busy, "{err}");
    std::thread::sleep(TTL + Duration::from_millis(250));
    let c = open(
        &path,
        lease_options(TTL, false, true, Duration::from_secs(1)),
    );
    assert_eq!(c.writer_lease_view().unwrap().fence, record.fence + 1);
    std::mem::forget(c);
}

// ---------------------------------------------------------------------------
// 5. The fence refuses a resurrected stale writer.
// ---------------------------------------------------------------------------

#[test]
fn fence_prevents_a_resurrected_stale_writer() {
    let (_dir, path) = new_store();
    let mut a = open(
        &path,
        lease_options(TTL, false, true, Duration::from_millis(200)),
    );
    let plan_a = plan(&a, b"stale", b"zulu", b"seed-a");
    let fence_a = a.writer_lease_view().unwrap().fence;

    // The old holder goes quiet ("SIGSTOP" in the court) and its lease expires.
    std::thread::sleep(TTL + Duration::from_millis(250));
    let mut b = open(
        &path,
        lease_options(TTL, false, true, Duration::from_secs(3)),
    );
    let fence_b = b.writer_lease_view().unwrap().fence;
    assert_eq!(fence_b, fence_a + 1);
    let plan_b = plan(&b, b"fresh", b"yankee", b"seed-b");
    let tx_b = stage(&b, "0000000000000000000000000000001b", &plan_b);
    let out_b = b
        .execute_transaction(&tx_b, &TransactionOptions::default())
        .unwrap();

    // The old holder resumes and tries to publish *after* the successor.
    let head_before = b.head();
    let generation_before = b.generation();
    let tx_a = stage(&a, "0000000000000000000000000000001a", &plan_a);
    let err = a
        .execute_transaction(&tx_a, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::LeaseExpired, "{err}");
    // The refusal names *something* specific. On this schedule the cheap local
    // deadline check fires first (see
    // `the_durable_record_is_the_fence_even_when_the_local_deadline_is_live`
    // for the durable-record check in isolation).
    assert!(err.message().contains("lease"), "{err}");

    // The successor's commit is the head, the stale writer's is not, and the
    // store verifies. Only the successor's name is in the committed namespace.
    assert_eq!(b.head(), out_b.commit);
    assert_eq!(b.head(), head_before);
    assert_eq!(b.generation(), generation_before);
    let names: Vec<Vec<u8>> = b
        .list_dir(State::ROOT_ID)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(names.contains(&b"fresh".to_vec()), "{names:?}");
    assert!(!names.contains(&b"stale".to_vec()), "{names:?}");
    // A fresh reader (a separate handle opening at the head) agrees.
    let observer = Store::open_with_options(
        &path,
        OpenMode::ReadOnly,
        base_options(WriterMode::Optimistic),
    )
    .unwrap();
    assert!(observer.verify(true).unwrap().ok);
    let names: Vec<Vec<u8>> = observer
        .list_dir(State::ROOT_ID)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec![b"fresh".to_vec()]);
    drop(a);
}

// ---------------------------------------------------------------------------
// 6. Fairness: a stream of short-lived writers cannot starve a waiter.
// ---------------------------------------------------------------------------

#[test]
fn queue_is_fifo_and_short_writers_cannot_starve_a_waiter() {
    let (_dir, path) = new_store();

    // A holder keeps the lease for 2 s with its keeper running, so nobody can
    // take it. A waiter then joins the queue. A stream of short-lived writers
    // arrives behind the waiter and must not overtake it.
    let holder_path = path.clone();
    let holder = std::thread::spawn(move || {
        let store = open(
            &holder_path,
            lease_options(TTL, true, true, Duration::from_millis(200)),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        std::thread::sleep(Duration::from_secs(2));
        fence
    });
    std::thread::sleep(Duration::from_millis(300));

    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let store = open(
            &waiter_path,
            lease_options(TTL, false, true, Duration::from_secs(8)),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        let waited = started.elapsed();
        // Hold briefly so the stream below sees a live holder again.
        std::thread::sleep(Duration::from_millis(300));
        drop(store);
        (fence, waited)
    });
    std::thread::sleep(Duration::from_millis(300));

    // Every short writer is refused while the holder and then the queued waiter
    // own the store.
    let mut acquired_while_queued = 0;
    let mut refused = 0;
    for _ in 0..40u32 {
        let short = lease_options(TTL, false, true, Duration::from_millis(50));
        match try_open(&path, short) {
            Ok(store) => {
                acquired_while_queued += 1;
                drop(store);
            }
            Err(err) => {
                assert_eq!(err.class(), Class::Busy, "{err}");
                refused += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let fence_holder = holder.join().unwrap();
    let (fence_waiter, waited) = waiter.join().unwrap();
    assert_eq!(
        refused, 40,
        "{refused} refused, {acquired_while_queued} acquired"
    );
    assert_eq!(
        acquired_while_queued, 0,
        "the waiter was overtaken {acquired_while_queued} time(s); the FIFO guarantee failed"
    );
    assert!(
        waited < Duration::from_secs(8),
        "the waiter never acquired: {waited:?}"
    );
    assert_eq!(
        fence_waiter,
        fence_holder + 1,
        "the waiter takes the next fence; no newcomer took it first"
    );
}

/// The control arm for the fairness measurement: with `fairness: false` the same
/// stream of short-lived writers *does* overtake a queued waiter, because each
/// one only races for the compare-and-swap.
#[test]
fn without_the_queue_short_writers_overtake_a_waiter() {
    let (_dir, path) = new_store();
    let holder_path = path.clone();
    let holder = std::thread::spawn(move || {
        let store = open(
            &holder_path,
            lease_options(TTL, true, false, Duration::from_millis(200)),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        std::thread::sleep(Duration::from_secs(2));
        fence
    });
    std::thread::sleep(Duration::from_millis(300));

    // The waiter retries for a long time but holds no ticket.
    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let store = open(
            &waiter_path,
            lease_options(TTL, false, false, Duration::from_secs(8)),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        let waited = started.elapsed();
        std::thread::sleep(Duration::from_millis(300));
        drop(store);
        (fence, waited)
    });
    std::thread::sleep(Duration::from_millis(300));

    let mut acquired_while_queued = 0;
    for _ in 0..40u32 {
        let short = lease_options(TTL, false, false, Duration::from_millis(50));
        if let Ok(store) = try_open(&path, short) {
            acquired_while_queued += 1;
            drop(store);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let fence_holder = holder.join().unwrap();
    let (fence_waiter, waited) = waiter.join().unwrap();
    // The control arm is *allowed* to be zero on a fast host; it is reported,
    // not asserted. The fair arm above asserts the guarantee.
    eprintln!(
        "fairness=off: {acquired_while_queued} newcomer(s) acquired ahead of the waiter; \
         waiter fence {fence_waiter} after {waited:?} (holder fence {fence_holder})"
    );
    assert!(fence_waiter >= fence_holder + 1);
}

// ---------------------------------------------------------------------------
// 7. Contract edges.
// ---------------------------------------------------------------------------

#[test]
fn single_writer_mode_refuses_a_lease() {
    let (_dir, path) = new_store();
    let options = Options {
        writer_lease: Some(WriterLeaseOptions::default()),
        ..base_options(WriterMode::Single)
    };
    let err = match Store::open_with_options(&path, OpenMode::ReadWrite, options) {
        Ok(_) => panic!("MW1 + lease must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("optimistic"), "{err}");
}

#[test]
fn a_lease_shorter_than_one_second_is_refused() {
    let (_dir, path) = new_store();
    let options = lease_options(
        Duration::from_millis(200),
        true,
        true,
        Duration::from_secs(1),
    );
    let err = match Store::open_with_options(&path, OpenMode::ReadWrite, options) {
        Ok(_) => panic!("a sub-second lease must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
}

/// The durable record — not the local clock — is the fence.
///
/// This case rewrites the lease record out of band to a higher fence whose
/// deadline is in the future, while the holder's own local deadline is still
/// live. The local deadline check cannot fire, so the refusal proves the
/// publication consults the durable record: a writer that lost the store can
/// never publish, even if its own clock says its lease is still valid.
#[test]
fn the_durable_record_is_the_fence_even_when_the_local_deadline_is_live() {
    use weftfs::writer_lease::{wall_now_ns, LeaseIo, LeaseState};

    let (_dir, path) = new_store();
    let mut a = open(
        &path,
        lease_options(TTL * 4, false, true, Duration::from_millis(200)),
    );
    let lease_a = a.writer_lease_view().unwrap();
    assert!(
        !lease_a.expired(wall_now_ns()),
        "the local deadline is live"
    );
    let plan_a = plan(&a, b"intruded", b"xray", b"seed-a");
    let head_before = a.head();
    let generation_before = a.generation();

    // Out-of-band takeover: exactly what a successor's acquisition does, with a
    // higher fence and a future deadline.
    let io = LeaseIo::open(&weftfs::backend::BackendKind::Local, &path).unwrap();
    let (record, bytes) = io.read().unwrap().unwrap();
    let now = wall_now_ns();
    let intruder = weftfs::writer_lease::LeaseRecord {
        owner: "intruder".to_owned(),
        fence: record.fence + 1,
        sequence: 1,
        acquired_ns: now,
        renewed_ns: now,
        deadline_ns: now + TTL.as_nanos() as i64,
        ttl_ns: TTL.as_nanos() as i64,
        state: LeaseState::Held,
        released_from: None,
        head_generation: record.head_generation,
        head_commit: record.head_commit,
        head_commit_len: record.head_commit_len,
    };
    io.compare_and_swap(Some(&weftfs::writer_lease::lease_token(&bytes)), &intruder)
        .unwrap();

    // A is still locally in time, so `require_writable` passes; the publication
    // fence must refuse.
    let tx = stage(&a, "0000000000000000000000000000000f", &plan_a);
    let err = a
        .execute_transaction(&tx, &TransactionOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::LeaseExpired, "{err}");
    assert!(err.message().contains("superseded"), "{err}");
    assert_eq!(a.head(), head_before);
    assert_eq!(a.generation(), generation_before);
    // The intruder's record is untouched by the failed publication.
    let after = io.read().unwrap().unwrap().0;
    assert_eq!(after.owner, "intruder");
    assert_eq!(after.fence, intruder.fence);
}

#[test]
fn a_tampered_lease_record_is_rejected() {
    let (_dir, path) = new_store();
    let a = open(
        &path,
        lease_options(TTL, false, true, Duration::from_millis(200)),
    );
    let record = a.writer_lease_record().unwrap().unwrap();
    let bytes = weftfs::writer_lease::encode_lease(&record).unwrap();
    // Canonical round trip.
    assert_eq!(weftfs::writer_lease::decode_lease(&bytes).unwrap(), record);
    // A truncated record is not a record.
    assert!(weftfs::writer_lease::decode_lease(&bytes[..bytes.len() - 1]).is_err());
    // An empty record is not a lease.
    assert!(weftfs::writer_lease::decode_lease(b"").is_err());
    // A record that names no fence is refused by the encoder.
    let mut no_fence = record.clone();
    no_fence.fence = 0;
    assert_eq!(
        weftfs::writer_lease::encode_lease(&no_fence)
            .unwrap_err()
            .class(),
        Class::Malformed
    );
    // A held record carrying a release token is refused.
    let mut contradictory = record.clone();
    contradictory.released_from = Some("abcd".to_owned());
    assert_eq!(
        weftfs::writer_lease::encode_lease(&contradictory)
            .unwrap_err()
            .class(),
        Class::Malformed
    );
    drop(a);
}

#[test]
fn the_lease_record_is_not_part_of_the_logical_state() {
    // The lease is control state: holding, renewing or releasing it must not
    // change the committed head or the checkpointed state.
    let (_dir, path) = new_store();
    let a = open(
        &path,
        lease_options(TTL, true, true, Duration::from_millis(200)),
    );
    let head = a.head();
    let generation = a.generation();
    std::thread::sleep(Duration::from_millis(700));
    assert!(a.writer_lease_stats().unwrap().renewals > 0);
    assert_eq!(a.head(), head);
    assert_eq!(a.generation(), generation);
    let _ = Hash32::ZERO;
}
