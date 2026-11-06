//! Ordered producer/consumer restore.
//!
//! Covers the documented contract of `run_ordered_pipeline` and
//! `Store::restore_tensors_pipelined*`: exact source order for every thread
//! count, ordering under skewed decode times, a real staging bound, the
//! lowest-index error policy with a deterministic written prefix, a read-only
//! store, and a real process kill during the write leaving only a truncated
//! destination.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use weftfs::identity::Hash32;
use weftfs::parallel::effective_threads;
use weftfs::restore_pipeline::{run_ordered_pipeline, RestorePipelineOptions};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorLayoutSpec;
use weftfs::{AuthorizedRead, Error, Principal, Result};

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
    dir: tempfile::TempDir,
    store: Store,
    ids: Vec<Hash32>,
    expected: Vec<Vec<u8>>,
}

impl Fixture {
    fn store_path(&self) -> PathBuf {
        self.dir.path().join("store")
    }

    fn concatenated(&self) -> Vec<u8> {
        self.expected.concat()
    }

    fn authorized(&self) -> Vec<AuthorizedRead> {
        self.ids
            .iter()
            .map(|id| {
                self.store
                    .authorize_read(*id, &Principal::root(), self.store.clock_now_ns())
                    .unwrap()
            })
            .collect()
    }
}

/// Write `sizes.len()` tensor files named `t0..` at the store root, then reopen
/// the store read-only.
fn fixture(sizes: &[usize]) -> Fixture {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut ids = Vec::new();
    let mut expected = Vec::new();
    for (index, size) in sizes.iter().enumerate() {
        let payload = bytes(*size, 0x73 + (index as u64) * 2);
        let id = store
            .write_tensor_file(
                root,
                format!("t{index}").as_bytes(),
                "q8_0",
                &[*size as u64],
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
        dir,
        store,
        ids,
        expected,
    }
}

fn pipeline(threads: usize, bound: usize) -> RestorePipelineOptions {
    RestorePipelineOptions {
        threads,
        max_inflight_bytes: bound,
    }
}

fn tree_digest(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, u64, Vec<u8>)>) -> io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else {
                let relative = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((relative, entry.metadata()?.len(), fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    walk(dir, dir, &mut entries).unwrap();
    let mut hasher = blake3::Hasher::new();
    for (path, len, content) in entries {
        hasher.update(path.as_bytes());
        hasher.update(&len.to_le_bytes());
        hasher.update(&content);
    }
    hasher.finalize().to_hex().to_string()
}

// ---------------------------------------------------------------------------
// byte identity and order
// ---------------------------------------------------------------------------

#[test]
fn byte_identical_output_for_1_2_4_threads() {
    let sizes = [64usize, 192, 4_096, 65_536, 128, 65_536, 1_024, 262_144];
    let fx = fixture(&sizes);
    let expected = fx.concatenated();
    let total: u64 = sizes.iter().map(|size| *size as u64).sum();

    for threads in [1, 2, 4] {
        for repeat in 0..3 {
            let mut out = Vec::new();
            let stats = fx
                .store
                .restore_tensors_pipelined(&fx.ids, &mut out, &pipeline(threads, 128 * 1024))
                .unwrap();
            assert_eq!(
                out, expected,
                "threads={threads} repeat={repeat} changed bytes or order"
            );
            assert_eq!(stats.tasks, sizes.len());
            assert_eq!(stats.tensors_written, sizes.len());
            assert_eq!(stats.bytes_written, total);
            assert_eq!(stats.requested_threads, threads);
            assert_eq!(stats.effective_threads, effective_threads(threads));
            assert_eq!(stats.max_inflight_bytes, 128 * 1024);
            assert_eq!(stats.bytes_dropped, 0);
            assert_eq!(stats.tensors_dropped, 0);
            if threads == 1 {
                // The one-worker path stages exactly one tensor at a time.
                assert_eq!(stats.observed_max_inflight_tasks, 1);
            }
        }
    }
}

#[test]
fn authorized_restore_matches_the_id_restore() {
    let sizes = [64usize, 1_024, 4_096, 128];
    let fx = fixture(&sizes);
    let tokens = fx.authorized();

    let mut by_id = Vec::new();
    let a = fx
        .store
        .restore_tensors_pipelined(&fx.ids, &mut by_id, &pipeline(4, 1 << 20))
        .unwrap();
    let mut by_token = Vec::new();
    let b = fx
        .store
        .restore_tensors_pipelined_authorized(&tokens, &mut by_token, &pipeline(4, 1 << 20))
        .unwrap();

    assert_eq!(by_id, fx.concatenated());
    assert_eq!(by_id, by_token);
    assert_eq!(a.bytes_written, b.bytes_written);
    assert_eq!(a.tensors_written, b.tensors_written);
}

#[test]
fn order_is_preserved_under_skewed_decode_times() {
    // Later inputs decode faster than earlier ones, so completion order is the
    // reverse of input order; the writer must still emit 0..n.
    let inputs: Vec<usize> = (0..24).collect();
    let sizes: Vec<Result<usize>> = inputs.iter().map(|_| Ok(8)).collect();
    let mut seen: Vec<(usize, Vec<u8>)> = Vec::new();

    let stats = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, 32),
        |index, _| {
            let delay = ((24 - index) % 5) as u64;
            std::thread::sleep(Duration::from_millis(delay));
            Ok(vec![index as u8; 8])
        },
        |value| value.len(),
        |index, value| {
            seen.push((index, value.clone()));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(stats.tensors_written, inputs.len());
    assert_eq!(seen.len(), inputs.len());
    for (position, (index, value)) in seen.iter().enumerate() {
        assert_eq!(*index, position, "writer emitted an input out of order");
        assert_eq!(value, &vec![position as u8; 8]);
    }
}

// ---------------------------------------------------------------------------
// staging bound
// ---------------------------------------------------------------------------

#[test]
fn staging_bound_is_observed_and_enforced() {
    let chunk = 4 * 1024 * 1024usize;
    let bound = 2 * chunk;
    let inputs: Vec<usize> = (0..8).collect();
    let sizes: Vec<Result<usize>> = inputs.iter().map(|_| Ok(chunk)).collect();
    let mut written = 0usize;

    let stats = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, bound),
        |index, _| {
            std::thread::sleep(Duration::from_millis(10));
            Ok(vec![index as u8; chunk])
        },
        |value| value.len(),
        |_index, value| {
            // Slow sink: hold the value long enough that workers publish ahead.
            std::thread::sleep(Duration::from_millis(15));
            written += value.len();
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(written, inputs.len() * chunk);
    assert_eq!(stats.bytes_written, (inputs.len() * chunk) as u64);
    assert_eq!(stats.oversized_tasks, 0);
    assert!(
        stats.observed_max_inflight_bytes <= bound,
        "observed {} reserved bytes > bound {bound}",
        stats.observed_max_inflight_bytes
    );
    assert!(
        stats.observed_max_inflight_tasks <= 2,
        "observed {} reserved tasks, bound allows 2",
        stats.observed_max_inflight_tasks
    );
    if stats.effective_threads > 1 {
        assert!(
            stats.observed_max_inflight_tasks >= 2,
            "expected concurrent staging with {} workers",
            stats.effective_threads
        );
    }
}

#[test]
fn oversized_tasks_run_one_at_a_time_without_deadlock() {
    // Every tensor alone exceeds the 64 KiB bound. The ordered writer must
    // still make progress: exactly one oversized reservation at a time.
    let chunk = 1024 * 1024usize;
    let inputs: Vec<usize> = (0..4).collect();
    let sizes: Vec<Result<usize>> = inputs.iter().map(|_| Ok(chunk)).collect();
    let mut written = 0usize;

    let stats = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, 64 * 1024),
        |index, _| Ok(vec![index as u8; chunk]),
        |value| value.len(),
        |_index, value| {
            written += value.len();
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(written, inputs.len() * chunk);
    assert_eq!(stats.oversized_tasks, inputs.len());
    assert_eq!(stats.observed_max_inflight_tasks, 1);
    assert!(
        stats.observed_max_inflight_bytes <= chunk,
        "observed {} exceeds one oversized tensor",
        stats.observed_max_inflight_bytes
    );
}

#[test]
fn oversized_between_normal_tasks_terminates_and_orders() {
    // A mixed corpus with a tiny bound used to expose the reservation-order
    // deadlock: an oversized tensor ahead of the writer must not prevent the
    // earlier (normal) reservations from being granted.
    let inputs: Vec<usize> = (0..12).collect();
    let sizes: Vec<Result<usize>> = inputs
        .iter()
        .map(|index| Ok(if *index == 7 { 2 * 1024 * 1024 } else { 4_096 }))
        .collect();
    let mut written = 0u64;

    let stats = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, 16 * 1024),
        |index, _| {
            std::thread::sleep(Duration::from_millis((index % 3) as u64));
            Ok(vec![index as u8; sizes_len_for_test(index)])
        },
        |value| value.len(),
        |_index, value| {
            written += value.len() as u64;
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(written, stats.bytes_written);
    assert_eq!(stats.tensors_written, inputs.len());
    assert_eq!(stats.oversized_tasks, 1);
}

fn sizes_len_for_test(index: usize) -> usize {
    if index == 7 {
        2 * 1024 * 1024
    } else {
        4_096
    }
}

// ---------------------------------------------------------------------------
// error policy
// ---------------------------------------------------------------------------

#[test]
fn first_error_is_lowest_index_and_writing_stops() {
    let inputs: Vec<usize> = (0..12).collect();
    let sizes: Vec<Result<usize>> = inputs.iter().map(|_| Ok(64)).collect();

    for _ in 0..5 {
        let mut seen: Vec<(usize, u8)> = Vec::new();
        let err = run_ordered_pipeline(
            &inputs,
            &sizes,
            &pipeline(4, 1 << 20),
            |index, _| {
                if index == 2 || index == 5 {
                    Err(Error::corrupt(format!("synthetic failure at {index}")))
                } else {
                    Ok(vec![index as u8; 64])
                }
            },
            |value| value.len(),
            |index, value| {
                seen.push((index, value[0]));
                Ok(())
            },
        )
        .expect_err("two failing inputs must fail the pipeline");

        assert!(
            err.message().contains("at 2"),
            "the lowest failing index must win: {}",
            err.message()
        );
        assert_eq!(
            seen,
            vec![(0, 0), (1, 1)],
            "only the prefix before the lowest failure may be written"
        );
    }
}

#[test]
fn corrupted_tensor_fails_closed_with_a_deterministic_prefix() {
    let sizes = [8_192usize; 8];
    let fx = fixture(&sizes);
    // Corrupt the manifests of tensors 2 and 5. The store stays open, so the
    // corruption is seen on read.
    for index in [2usize, 5] {
        let record = fx.store.object_meta(fx.ids[index]).unwrap();
        let path = weftfs::recovery::manifest_path(fx.store.root(), record.content);
        let mut manifest = fs::read(&path).unwrap();
        manifest[0] ^= 0xff;
        fs::write(&path, &manifest).unwrap();
    }
    let prefix = fx.expected[..2].concat();

    let mut first: Option<(String, Vec<u8>)> = None;
    for threads in [1, 2, 4] {
        for _ in 0..3 {
            let mut out = Vec::new();
            let err = fx
                .store
                .restore_tensors_pipelined(&fx.ids, &mut out, &pipeline(threads, 1 << 20))
                .expect_err("a corrupted tensor must fail the whole restore");
            assert_eq!(err.class_name(), "Corrupt", "{}", err.message());
            assert_eq!(
                out,
                prefix,
                "threads={threads}: wrong written prefix ({} wrote {} of {} bytes): {}",
                err.class_name(),
                out.len(),
                prefix.len(),
                err.message()
            );
            match &first {
                None => first = Some((err.to_string(), out)),
                Some((message, bytes)) => {
                    assert_eq!(message, &err.to_string(), "error is not deterministic");
                    assert_eq!(bytes, &out);
                }
            }
        }
    }
}

#[test]
fn writer_failure_keeps_only_the_successful_prefix() {
    let inputs: Vec<usize> = (0..10).collect();
    let sizes: Vec<Result<usize>> = inputs.iter().map(|_| Ok(32)).collect();
    let mut seen = 0usize;

    let err = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, 1 << 20),
        |index, _| Ok(vec![index as u8; 32]),
        |value| value.len(),
        |index, _value| {
            if index == 4 {
                return Err(Error::io("destination failed"));
            }
            seen += 1;
            Ok(())
        },
    )
    .expect_err("a sink failure must fail the pipeline");

    assert_eq!(err.class_name(), "Io", "{}", err.message());
    assert_eq!(seen, 4, "indices 0..4 must have been written exactly once");
}

#[test]
fn declaration_errors_do_not_stall_later_reservations() {
    // Index 1 cannot be decoded (the declared size is unknown) and index 5 is
    // an error too. Later inputs must still reserve and complete, and the
    // writer must stop at index 1.
    let inputs: Vec<usize> = (0..12).collect();
    let sizes: Vec<Result<usize>> = inputs
        .iter()
        .map(|index| match index {
            1 => Err(Error::corrupt("unknown size at 1")),
            _ => Ok(16),
        })
        .collect();
    let mut seen = Vec::new();

    let err = run_ordered_pipeline(
        &inputs,
        &sizes,
        &pipeline(4, 1 << 20),
        |index, _| {
            std::thread::sleep(Duration::from_millis(1));
            Ok(vec![index as u8; 16])
        },
        |value| value.len(),
        |index, _value| {
            seen.push(index);
            Ok(())
        },
    )
    .expect_err("the declared-size error must fail the pipeline");

    assert!(err.message().contains("unknown size at 1"));
    assert_eq!(seen, vec![0]);
}

// ---------------------------------------------------------------------------
// read-only store
// ---------------------------------------------------------------------------

#[test]
fn restore_does_not_mutate_the_store() {
    let fx = fixture(&[4_096usize, 65_536, 1_024, 131_072]);
    let before = tree_digest(&fx.store_path());
    let generation = fx.store.generation();
    let head = fx.store.snapshot_root();

    for threads in [1, 2, 4] {
        let mut out = Vec::new();
        fx.store
            .restore_tensors_pipelined_authorized(
                &fx.authorized(),
                &mut out,
                &pipeline(threads, 128 * 1024),
            )
            .unwrap();
        assert_eq!(out, fx.concatenated());
    }

    assert_eq!(fx.store.generation(), generation, "generation moved");
    assert_eq!(fx.store.snapshot_root(), head, "head moved");
    assert_eq!(
        tree_digest(&fx.store_path()),
        before,
        "a read-only restore changed store files"
    );
    let report = fx.store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// process kill during the write
// ---------------------------------------------------------------------------

const CRASH_STORE_ENV: &str = "WEFT_RESTORE_PIPELINE_STORE";
const CRASH_OUT_ENV: &str = "WEFT_RESTORE_PIPELINE_OUT";
const CRASH_AFTER_ENV: &str = "WEFT_RESTORE_PIPELINE_AFTER_BYTES";
const CRASH_COUNT_ENV: &str = "WEFT_RESTORE_PIPELINE_TENSORS";

/// Writes exactly `limit` bytes, then aborts the process mid-write.
struct CrashSink {
    file: fs::File,
    written: u64,
    limit: u64,
}

impl Write for CrashSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.written);
        if (buf.len() as u64) >= remaining {
            if remaining > 0 {
                self.file.write_all(&buf[..remaining as usize])?;
            }
            self.file.flush()?;
            std::process::abort();
        }
        self.file.write_all(buf)?;
        self.file.flush()?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Child side of the kill test. Without the environment it runs a small
/// in-process restore, so a normal suite still exercises the helper.
#[test]
fn restore_pipeline_crash_helper() {
    let Ok(store_path) = std::env::var(CRASH_STORE_ENV) else {
        let fx = fixture(&[4_096usize, 8_192, 1_024]);
        let mut out = Vec::new();
        fx.store
            .restore_tensors_pipelined(&fx.ids, &mut out, &pipeline(2, 1 << 20))
            .unwrap();
        assert_eq!(out, fx.concatenated());
        return;
    };
    let out_path = PathBuf::from(std::env::var(CRASH_OUT_ENV).unwrap());
    let after: u64 = std::env::var(CRASH_AFTER_ENV).unwrap().parse().unwrap();
    let count: usize = std::env::var(CRASH_COUNT_ENV).unwrap().parse().unwrap();

    let store = Store::open(&store_path, OpenMode::ReadOnly).unwrap();
    let ids: Vec<Hash32> = (0..count)
        .map(|index| store.resolve_path(&format!("/t{index}")).unwrap())
        .collect();
    let tokens: Vec<AuthorizedRead> = ids
        .iter()
        .map(|id| {
            store
                .authorize_read(*id, &Principal::root(), store.clock_now_ns())
                .unwrap()
        })
        .collect();
    let file = fs::File::create(&out_path).unwrap();
    let mut sink = CrashSink {
        file,
        written: 0,
        limit: after,
    };
    // Unwinds never: `CrashSink` aborts the process at the limit.
    store
        .restore_tensors_pipelined_authorized(&tokens, &mut sink, &pipeline(4, 1 << 20))
        .unwrap();
    panic!("the crash sink did not abort");
}

#[test]
fn kill_mid_write_leaves_only_a_truncated_destination() {
    let sizes = [1_048_576usize; 6];
    let fx = fixture(&sizes);
    let total: usize = sizes.iter().sum();
    let after = (total / 3) as u64;
    let expected = fx.concatenated();

    let before = tree_digest(&fx.store_path());
    let generation = fx.store.generation();
    let head = fx.store.snapshot_root();
    let out = fx.dir.path().join("destination.bin");

    let exe = std::env::current_exe().expect("test binary path");
    let status = Command::new(exe)
        .args(["restore_pipeline_crash_helper", "--exact", "--nocapture"])
        .env(CRASH_STORE_ENV, fx.store_path())
        .env(CRASH_OUT_ENV, &out)
        .env(CRASH_AFTER_ENV, after.to_string())
        .env(CRASH_COUNT_ENV, sizes.len().to_string())
        .status()
        .expect("spawn crash helper");
    assert!(!status.success(), "child was expected to abort");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "expected SIGABRT");
    }

    let written = fs::read(&out).expect("the truncated destination must exist");
    assert_eq!(
        written.len() as u64,
        after,
        "the destination must be truncated exactly at the kill point"
    );
    assert_eq!(
        written,
        expected[..written.len()],
        "the truncated destination must be an exact source prefix"
    );

    // The store is untouched, and nothing from the destination was published.
    let store = Store::open(fx.store_path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot_root(), head);
    assert_eq!(tree_digest(&fx.store_path()), before);
    assert!(store.verify(true).unwrap().ok);
    assert!(store.resolve_path("/destination.bin").is_err());
}

// ---------------------------------------------------------------------------
// empty input
// ---------------------------------------------------------------------------

#[test]
fn empty_input_writes_nothing() {
    let fx = fixture(&[64]);
    let mut out = Vec::new();
    let stats = fx
        .store
        .restore_tensors_pipelined(&[], &mut out, &pipeline(4, 1 << 20))
        .unwrap();
    assert!(out.is_empty());
    assert_eq!(stats.tasks, 0);
    assert_eq!(stats.tensors_written, 0);
    assert_eq!(stats.bytes_written, 0);
}
