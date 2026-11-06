//! Bounded parallel tensor reads.
//!
//! Covers the four contract points of `Store::read_tensors_bounded`: exact
//! input order for every thread count, a deterministic fail-closed error, a
//! real memory bound (observed through the public stats), and absence of
//! deadlock across repeated runs.

use std::fs;

use tempfile::tempdir;
use weftfs::identity::Hash32;
use weftfs::parallel::{effective_threads, ParallelReadOptions, ParallelReadStats};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorLayoutSpec;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

/// Deterministic pseudo-random bytes (xorshift64*).
fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    ids: Vec<Hash32>,
    expected: Vec<Vec<u8>>,
}

/// Write `sizes.len()` tensor objects of the given logical sizes.
fn fixture(sizes: &[usize]) -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut ids = Vec::new();
    let mut expected = Vec::new();
    for (index, size) in sizes.iter().enumerate() {
        let shape = [*size as u64];
        let payload = bytes(*size, 0x51 + index as u64);
        let id = store
            .write_tensor_file(
                root,
                format!("t{index}").as_bytes(),
                "q8_0",
                &shape,
                &payload,
                &TensorLayoutSpec::row_major(),
                None,
            )
            .unwrap();
        ids.push(id);
        expected.push(payload);
    }
    drop(store);
    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    Fixture {
        _dir: dir,
        store,
        ids,
        expected,
    }
}

fn options(threads: usize, max_inflight_bytes: usize) -> ParallelReadOptions {
    ParallelReadOptions {
        threads,
        max_inflight_bytes,
    }
}

fn read(
    store: &Store,
    ids: &[Hash32],
    threads: usize,
    max_inflight_bytes: usize,
) -> (Vec<Vec<u8>>, ParallelReadStats) {
    store
        .read_tensors_bounded_with_stats(ids, &options(threads, max_inflight_bytes))
        .unwrap()
}

#[test]
fn thread_count_never_changes_bytes_or_order() {
    let sizes = [64, 192, 4_096, 65_536, 128, 65_536, 1_024];
    let fx = fixture(&sizes);
    let baseline: Vec<Vec<u8>> = fx
        .ids
        .iter()
        .map(|id| fx.store.read_tensor(*id).unwrap())
        .collect();
    assert_eq!(baseline, fx.expected, "sequential read must be exact");

    for threads in [1, 2, 4, 0, usize::MAX] {
        let (got, stats) = read(&fx.store, &fx.ids, threads, 1 << 20);
        assert_eq!(
            got, fx.expected,
            "threads={threads} must be exact and ordered"
        );
        assert_eq!(stats.tasks, fx.ids.len());
        assert_eq!(stats.requested_threads, threads);
        assert_eq!(stats.effective_threads, effective_threads(threads));
        assert!(stats.effective_threads >= 1);
    }
    // `threads = 0` means one worker, and the cap is available_parallelism.
    assert_eq!(effective_threads(0), 1);
    assert_eq!(effective_threads(1), 1);
}

#[test]
fn determinism_across_twenty_repeats_at_four_threads() {
    let sizes = [2_048, 64, 70_016, 128, 4_096, 33_344, 512, 9_984, 65_536];
    let fx = fixture(&sizes);
    let baseline: Vec<Vec<u8>> = fx
        .ids
        .iter()
        .map(|id| fx.store.read_tensor(*id).unwrap())
        .collect();
    assert_eq!(baseline, fx.expected);

    for repeat in 0..20 {
        let (got, stats) = read(&fx.store, &fx.ids, 4, 128 * 1024);
        assert_eq!(got, baseline, "repeat {repeat} changed bytes or order");
        assert_eq!(stats.tasks, sizes.len());
        // The bound holds on every repeat, not just on average.
        assert!(
            stats.observed_max_inflight_bytes <= 128 * 1024,
            "repeat {repeat}: inflight {} > bound",
            stats.observed_max_inflight_bytes
        );
    }
}

#[test]
fn inflight_bound_holds_and_is_observable() {
    // Eight 256 KiB tensors, 512 KiB staging: at most two can be reserved.
    let sizes = [262_144usize; 8];
    let fx = fixture(&sizes);
    let (got, stats) = read(&fx.store, &fx.ids, 4, 512 * 1024);
    assert_eq!(got, fx.expected);
    assert_eq!(stats.oversized_tasks, 0, "no task exceeds the bound alone");
    assert!(
        stats.observed_max_inflight_bytes <= 512 * 1024,
        "observed {} > bound",
        stats.observed_max_inflight_bytes
    );
    assert!(
        stats.observed_max_inflight_tasks <= 2,
        "observed {} reserved tasks, bound allows 2",
        stats.observed_max_inflight_tasks
    );
    // The instrumentation is not vacuous: work really was staged in parallel.
    if stats.effective_threads > 1 {
        assert!(
            stats.observed_max_inflight_tasks >= 2,
            "expected concurrent staging with {} workers",
            stats.effective_threads
        );
    }
}

#[test]
fn oversized_task_runs_alone() {
    // Every tensor alone exceeds the 64 KiB bound, so each must run alone.
    let sizes = [1_048_576usize; 4];
    let fx = fixture(&sizes);
    let (got, stats) = read(&fx.store, &fx.ids, 4, 64 * 1024);
    assert_eq!(got, fx.expected);
    assert_eq!(stats.oversized_tasks, 4);
    assert_eq!(stats.observed_max_inflight_tasks, 1);
    assert!(
        stats.observed_max_inflight_bytes <= 1_048_576,
        "observed {} exceeds one oversized task",
        stats.observed_max_inflight_bytes
    );
}

#[test]
fn tight_bound_and_oversized_mix_terminates() {
    // A one-byte bound and a mixed corpus: every task is oversized, and the
    // oversized path must not deadlock against the consumer.
    let sizes = [64usize, 4_096, 64, 65_536, 128];
    let fx = fixture(&sizes);
    for threads in [2, 4] {
        let (got, stats) = read(&fx.store, &fx.ids, threads, 1);
        assert_eq!(got, fx.expected, "threads={threads}");
        assert_eq!(stats.oversized_tasks, sizes.len());
    }
}

#[test]
fn corrupted_object_fails_closed_and_deterministically() {
    let sizes = [8_192usize; 6];
    let fx0 = fixture(&sizes);
    // Corrupt the third tensor's manifest blob on disk; the store stays open
    // so nothing is re-read from the commit chain.
    let record = fx0.store.object_meta(fx0.ids[2]).unwrap();
    let path = weftfs::recovery::manifest_path(fx0.store.root(), record.content);
    let mut manifest = fs::read(&path).unwrap();
    manifest[0] ^= 0xff;
    fs::write(&path, &manifest).unwrap();

    let mut seen: Option<(String, String)> = None;
    for threads in [1, 2, 4] {
        for repeat in 0..3 {
            let err = fx0
                .store
                .read_tensors_bounded(&fx0.ids, &options(threads, 1 << 20))
                .expect_err("a corrupted tensor must fail the whole call");
            let fate = (err.class_name().to_owned(), err.message().to_owned());
            match &seen {
                None => seen = Some(fate),
                Some(previous) => assert_eq!(
                    previous, &fate,
                    "threads={threads} repeat={repeat}: error is not deterministic"
                ),
            }
        }
    }
    let (class, message) = seen.unwrap();
    assert_eq!(class, "Corrupt", "{message}");
}

#[test]
fn error_policy_is_lowest_failing_index() {
    // Corrupt two tensors out of order in the input list: the reported error
    // must be the one for the lower index, whatever order the workers finish.
    let sizes = [4_096usize; 8];
    let fx0 = fixture(&sizes);
    for index in [5usize, 2] {
        let record = fx0.store.object_meta(fx0.ids[index]).unwrap();
        let path = weftfs::recovery::manifest_path(fx0.store.root(), record.content);
        let mut manifest = fs::read(&path).unwrap();
        manifest[0] ^= 0xff;
        fs::write(&path, &manifest).unwrap();
    }
    let mut first: Option<String> = None;
    for threads in [2, 4] {
        for _ in 0..5 {
            let err = fx0
                .store
                .read_tensors_bounded(&fx0.ids, &options(threads, 1 << 20))
                .expect_err("corrupted tensors must fail");
            match &first {
                None => first = Some(err.to_string()),
                Some(previous) => assert_eq!(previous, &err.to_string()),
            }
        }
    }
}

#[test]
fn empty_input_is_a_no_op() {
    let fx = fixture(&[16]);
    let (got, stats) = read(&fx.store, &[], 4, 1 << 20);
    assert!(got.is_empty());
    assert_eq!(stats.tasks, 0);
    assert_eq!(stats.effective_threads, effective_threads(4));
}

#[test]
fn non_tensor_object_reports_the_same_error_on_every_thread_count() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let tensor = store
        .write_tensor_file(
            root,
            b"t",
            "q8_0",
            &[64],
            &bytes(64, 0x77),
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    store
        .create_file(
            root,
            b"plain",
            &bytes(32, 0x78),
            0o644,
            0,
            0,
            std::collections::BTreeMap::new(),
        )
        .unwrap();
    let plain = store.resolve_path("/plain").unwrap();
    drop(store);
    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    let ids = [tensor, plain];
    let mut seen: Option<String> = None;
    for threads in [1, 2, 4] {
        let err = store
            .read_tensors_bounded(&ids, &options(threads, 1 << 20))
            .expect_err("a non-tensor object is not readable as a tensor");
        match &seen {
            None => seen = Some(err.to_string()),
            Some(previous) => assert_eq!(previous, &err.to_string()),
        }
    }
}
