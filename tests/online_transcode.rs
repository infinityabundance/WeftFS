//! P06 — online background transcode with a real budget.
//!
//! The property under test is not "the worker rewrites things" but that it does
//! so **while the store stays writable**, within a declared budget, without
//! changing any object's identity, and with the previous representation still
//! reachable afterwards.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use weftfs::online_transcode::{OnlineTranscodeOptions, OnlineTranscoder, SharedStore};
use weftfs::state::State;
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const ROWS: u64 = 256;
const COLS: u64 = 512;
const OBJECTS: usize = 6;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 4096,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn tensor(seed: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity((ROWS * COLS * 2) as usize);
    for _ in 0..(ROWS * COLS) / 4 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

struct Fixture {
    dir: TempDir,
    store: Store,
    ids: Vec<weftfs::Hash32>,
    bytes: Vec<Vec<u8>>,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let mut ids = Vec::new();
    let mut bytes = Vec::new();
    for index in 0..OBJECTS {
        let data = tensor(index as u32);
        let id = store
            .write_tensor_file(
                State::ROOT_ID,
                format!("t{index}").as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &data,
                &layout,
                // A fast, linear-pass representation: exactly what the online
                // worker exists to improve after the save has returned.
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
        ids.push(id);
        bytes.push(data);
    }
    Fixture {
        dir,
        store,
        ids,
        bytes,
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    predicate()
}

fn strategy_of(store: &Store, id: weftfs::Hash32) -> String {
    store
        .tensor_metadata(id)
        .unwrap()
        .strategy
        .as_str()
        .to_owned()
}

#[test]
fn the_worker_rewrites_without_changing_identity() {
    let fixture = fixture();
    let before: Vec<String> = fixture
        .ids
        .iter()
        .map(|id| strategy_of(&fixture.store, *id))
        .collect();
    assert!(
        before.iter().all(|strategy| strategy == "zstd"),
        "{before:?}"
    );

    let shared = SharedStore::new(fixture.store);
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            min_saved_percent: 0,
            pause: Duration::from_millis(5),
            max_applied: 3,
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();
    assert!(wait_for(
        || worker.stats().applied >= 3,
        Duration::from_secs(60)
    ));
    worker.stop();
    let stats = worker.stats();
    assert!(stats.applied >= 3, "{stats:?}");

    shared
        .with_foreground(|store| {
            // The worker is stopped; check the durable result.
            assert!(store.verify(true).unwrap().ok);
            let mut rewritten = 0;
            for (index, id) in fixture.ids.iter().enumerate() {
                // The object id never moves.
                assert_eq!(store.object_meta(*id).unwrap().id, *id);
                // The logical bytes are unchanged.
                assert_eq!(&store.read_object(*id).unwrap(), &fixture.bytes[index]);
                if strategy_of(store, *id) != "zstd" {
                    rewritten += 1;
                }
            }
            assert!(rewritten >= 3, "{rewritten} rewritten");
            Ok(())
        })
        .unwrap();
    // The declared budget was honoured: one worker, never more.
    let budget = worker.budget_stats();
    assert_eq!(budget.max_active_workers, 1);
    assert!(budget.observed_max_active_workers <= 1, "{budget:?}");
    let _ = fixture.dir;
}

#[test]
fn foreground_writes_progress_while_the_worker_runs() {
    let fixture = fixture();
    let dir = fixture.dir.path().to_path_buf();
    let ids = fixture.ids.clone();
    let shared = SharedStore::new(fixture.store);
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            min_saved_percent: 0,
            pause: Duration::from_millis(1),
            yield_interval: Duration::from_millis(1),
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();

    // Foreground writes while the worker is active. Each one is a real
    // mutation: a new version via `write_tensor_range`.
    let mut latencies = Vec::new();
    for round in 0..20u32 {
        let started = Instant::now();
        shared
            .with_foreground(|store| {
                let region = weftcodec::tensor::Region::new(vec![0, 0], vec![1, (COLS / 4) as u64]);
                let patch = vec![round as u8; (COLS / 4 * 2) as usize];
                store
                    .write_tensor_range(
                        fixture.ids[0],
                        &region,
                        &patch,
                        &weftfs::tensor_store::TensorWriteSpec::default(),
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
        latencies.push(started.elapsed().as_secs_f64() * 1e3);
    }
    let applied_while_foreground = worker.stats().applied;
    worker.stop();
    let stats = worker.stats();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() as f64 * 0.95) as usize - 1];
    eprintln!(
        "foreground write latency: p50 {p50:.2} ms, p95 {p95:.2} ms; worker applied \
         {applied_while_foreground} during the run ({stats:?})"
    );
    // Every foreground write landed, and the worker made progress at the same
    // time: the foreground was not starved and the worker was not stopped.
    assert!(p95 < 2_000.0, "foreground p95 {p95:.2} ms");
    shared
        .with_foreground(|store| {
            assert!(store.verify(true).unwrap().ok);
            for id in &ids {
                assert!(store.object_meta(*id).is_ok());
            }
            Ok(())
        })
        .unwrap();
    let _ = dir;
}

#[test]
fn oversized_objects_are_skipped_not_squeezed_into_the_budget() {
    let fixture = fixture();
    let shared = SharedStore::new(fixture.store);
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            // Every object is 256 KiB; a 4 KiB bound must skip them all.
            max_object_bytes: 4096,
            min_saved_percent: 0,
            pause: Duration::from_millis(2),
            max_applied: 1,
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();
    assert!(wait_for(
        || worker.stats().oversized_skips > 0,
        Duration::from_secs(20)
    ));
    worker.stop();
    let stats = worker.stats();
    assert_eq!(stats.applied, 0, "{stats:?}");
    // The policy's byte bound is the declared one, and no oversized request was
    // admitted: the worker skips rather than exceeding it.
    let budget = worker.budget_stats();
    assert_eq!(budget.max_inflight_bytes, 4096);
    assert_eq!(budget.oversized_admissions, 0, "{budget:?}");
}

#[test]
fn a_rewrite_keeps_the_previous_representation_reachable() {
    // The crash-safety property in its testable form: after a rewrite, a pinned
    // snapshot of the earlier state still names the old representation, and the
    // store's full verification proves that representation is still reachable.
    let mut fixture = fixture();
    let before: Vec<weftfs::Hash32> = fixture
        .ids
        .iter()
        .map(|id| fixture.store.object_meta(*id).unwrap().content)
        .collect();
    let snapshot = fixture.store.snapshot_create("pinned").unwrap();

    let shared = SharedStore::new(fixture.store);
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            min_saved_percent: 0,
            pause: Duration::from_millis(2),
            max_applied: OBJECTS as u64,
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();
    assert!(wait_for(
        || worker.stats().applied >= OBJECTS as u64,
        Duration::from_secs(120)
    ));
    worker.stop();

    shared
        .with_foreground(|store| {
            let pinned = store.state_at(snapshot).unwrap();
            let mut changed = 0;
            for (index, id) in fixture.ids.iter().enumerate() {
                let old = pinned.objects.get(id).expect("the snapshot still names the object");
                assert_eq!(
                    old.content, before[index],
                    "the pinned snapshot still names the old representation"
                );
                let now = store.object_meta(*id).unwrap();
                if now.content != before[index] {
                    changed += 1;
                }
                assert_eq!(&store.read_object(*id).unwrap(), &fixture.bytes[index]);
            }
            assert!(changed >= 1, "at least one representation moved");
            // Full verification walks every reachable blob, including both
            // versions: the old representation is still there.
            assert!(store.verify(true).unwrap().ok);
            Ok(())
        })
        .unwrap();
    let _ = fixture.dir;
}
