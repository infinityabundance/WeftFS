//! R21: the bounded parallel encode-and-publish pipeline.
//!
//! The pipeline changes *when* work happens, never *what* is published. These
//! tests are therefore mostly identity tests: the same inputs through 1/2/4/8
//! workers (and with candidate parallelism on and off) must produce the same
//! head, the same per-object manifest ids and content roots, and the same
//! committed bytes. The remaining tests cover the parts a byte comparison
//! cannot see: the byte reservations are actually enforced, a single oversize
//! tensor is admitted alone, and a failing reader publishes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_pipeline::{TensorPipelineOptions, TensorPipelineTensor};
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use weftfs::Error;

const FIXED_TS: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 1024,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TS),
        ..Options::default()
    }
}

/// Deterministic bytes: a skewed pattern so the planner has a real choice.
fn payload(len: usize, seed: u64) -> Vec<u8> {
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
    // Make large tensors compressible enough that the planner really decides.
    for (index, byte) in out.iter_mut().enumerate() {
        if index % 7 != 0 {
            *byte = (*byte & 0x0f) | 0x30;
        }
    }
    out
}

struct Fixture {
    name: &'static str,
    dtype: &'static str,
    shape: [u64; 2],
    bytes: Vec<u8>,
}

fn fixture() -> Vec<Fixture> {
    // 1 MiB, 256 KiB and two tiny tensors: big enough to exercise candidate
    // selection, small enough that eight workers fit the test court.
    vec![
        Fixture {
            name: "big.weight",
            dtype: "bf16",
            shape: [512, 1024],
            bytes: payload(1024 * 1024, 11),
        },
        Fixture {
            name: "mid.weight",
            dtype: "bf16",
            shape: [256, 512],
            bytes: payload(256 * 1024, 22),
        },
        Fixture {
            name: "tiny.norm",
            dtype: "bf16",
            shape: [64, 1],
            bytes: payload(128, 33),
        },
        Fixture {
            name: "tiny2.norm",
            dtype: "bf16",
            shape: [64, 1],
            bytes: payload(128, 44),
        },
    ]
}

/// A unique deterministic ObjectId seed per tensor name (the pipeline, like
/// the store, derives the ObjectId from the seed alone).
fn seed_for(name: &str) -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in name.as_bytes().iter().enumerate() {
        seed[index % 32] = seed[index % 32].wrapping_add(*byte).wrapping_add(1);
    }
    seed[31] = name.len() as u8;
    seed
}

fn spec(parallel_candidates: bool) -> TensorWriteSpec {
    let mut plan = weftfs::tensor_store::TensorPlanOptions::default();
    plan.parallel_candidates = parallel_candidates;
    TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan,
        tile_shape: None,
    }
}

struct Written {
    head: Hash32,
    objects: BTreeMap<String, (Hash32, Hash32)>, // name -> (manifest id, content root)
    manifests: BTreeMap<String, Vec<u8>>,
    stats: weftfs::tensor_pipeline::TensorPipelineStats,
}

fn write_group(
    root: &Path,
    options: &TensorPipelineOptions,
    parallel_candidates: bool,
) -> Result<Written, Error> {
    let fixtures = fixture();
    let mut store = Store::init(root, opts())?;
    let root_id = store.resolve_path("/")?;
    let tensors_dir = store.mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")?;
    let tensors: Vec<TensorPipelineTensor<'_>> = fixtures
        .iter()
        .map(|item| TensorPipelineTensor {
            name: item.name,
            dtype: item.dtype,
            shape: &item.shape,
            seed: seed_for(item.name),
            byte_len: item.bytes.len() as u64,
        })
        .collect();
    store.begin_group()?;
    let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
        buf.copy_from_slice(&fixtures[index].bytes);
        Ok(())
    };
    let outcome = store.prepare_tensor_group_pipelined(
        &tensors,
        &read,
        &spec(parallel_candidates),
        &TensorLayoutSpec::row_major(),
        options,
    )?;
    let mut ids = Vec::new();
    for (index, result) in outcome.results.iter().enumerate() {
        let id = store.commit_prepared_tensor_object(
            tensors_dir,
            fixtures[index].name.as_bytes(),
            &result.prepared,
            0o644,
        )?;
        ids.push(id);
    }
    let head = store.commit_group()?;
    let mut objects = BTreeMap::new();
    let mut manifests = BTreeMap::new();
    for (index, id) in ids.iter().enumerate() {
        let record = store.object_meta(*id)?;
        objects.insert(
            fixtures[index].name.to_owned(),
            (record.content, outcome.results[index].prepared.content_root),
        );
        let bytes = fs::read(store.root().join(blob_path(&record.content))).unwrap_or_default();
        manifests.insert(fixtures[index].name.to_owned(), bytes);
    }
    Ok(Written {
        head,
        objects,
        manifests,
        stats: outcome.stats,
    })
}

/// `objects/<2 hex>/<hash>` relative to the store root, from the manifest id.
fn blob_path(hash: &Hash32) -> PathBuf {
    let hex = hash.to_hex();
    PathBuf::from("manifests").join(&hex[..2]).join(&hex)
}

#[test]
fn every_worker_count_publishes_the_same_bytes() {
    let dir = tempdir().unwrap();
    let mut reference: Option<Written> = None;
    for workers in [1usize, 2, 4, 8] {
        let root = dir.path().join(format!("w{workers}"));
        let mut options = TensorPipelineOptions::default();
        options.encode_threads = workers;
        options.cpu_tokens = workers;
        let written = write_group(&root, &options, false).unwrap();
        assert_eq!(written.objects.len(), 4);
        // A full verify over the store the pipeline just wrote: every frame,
        // every manifest and the whole commit chain decode and match.
        let reopened = Store::open_with_options(&root, OpenMode::ReadWrite, opts()).unwrap();
        let report = reopened.verify(true).unwrap();
        assert!(report.ok, "verify --full failed at {workers} workers: {report:?}");
        assert_eq!(report.checked_objects, 4, "every tensor object was checked");
        assert!(report.checked_representations >= 1);
        drop(reopened);
        match &reference {
            None => reference = Some(written),
            Some(expected) => {
                assert_eq!(
                    written.head, expected.head,
                    "head differs at {workers} workers"
                );
                assert_eq!(
                    written.objects, expected.objects,
                    "objects differ at {workers} workers"
                );
                assert_eq!(
                    written.manifests, expected.manifests,
                    "manifest bytes differ at {workers} workers"
                );
            }
        }
    }
}

#[test]
fn candidate_parallelism_does_not_change_a_single_byte() {
    let dir = tempdir().unwrap();
    let mut sequential = TensorPipelineOptions::default();
    sequential.encode_threads = 1;
    sequential.cpu_tokens = 1;
    sequential.candidate_parallel_min_bytes = 0; // off
    let a = write_group(&dir.path().join("off"), &sequential, false).unwrap();

    let mut concurrent = TensorPipelineOptions::default();
    concurrent.encode_threads = 2;
    concurrent.cpu_tokens = 2;
    // Every tensor takes the concurrent candidate path.
    concurrent.candidate_parallel_min_bytes = 1;
    let b = write_group(&dir.path().join("on"), &concurrent, true).unwrap();
    assert_eq!(b.stats.candidate_parallel_tensors as usize, 4);
    assert_eq!(a.head, b.head);
    assert_eq!(a.objects, b.objects);
    assert_eq!(a.manifests, b.manifests);
}

#[test]
fn the_byte_reservations_are_never_exceeded() {
    let dir = tempdir().unwrap();
    let mut options = TensorPipelineOptions::default();
    options.encode_threads = 4;
    options.cpu_tokens = 4;
    // Deliberately tight: the 1 MiB tensor's worker reservation is 3 MiB + 1
    // MiB slack, so only one large tensor can be in flight.
    options.worker_bytes = 6 * 1024 * 1024;
    options.staging_bytes = 2 * 1024 * 1024;
    options.encode_bytes = 2 * 1024 * 1024;
    options.publish_bytes = 2 * 1024 * 1024;
    let written = write_group(&dir.path().join("tight"), &options, false).unwrap();
    let stats = &written.stats;
    assert!(stats.tensors == 4);
    assert!(
        stats.observed_max_staging_bytes <= options.staging_bytes,
        "staging {} > {}",
        stats.observed_max_staging_bytes,
        options.staging_bytes
    );
    assert!(
        stats.observed_max_encode_bytes <= options.encode_bytes,
        "encode queue {} > {}",
        stats.observed_max_encode_bytes,
        options.encode_bytes
    );
    assert!(
        stats.observed_max_publish_bytes <= options.publish_bytes,
        "publish {} > {}",
        stats.observed_max_publish_bytes,
        options.publish_bytes
    );
    assert!(
        stats.observed_max_worker_bytes <= options.worker_bytes,
        "workers {} > {}",
        stats.observed_max_worker_bytes,
        options.worker_bytes
    );
    assert!(stats.reservation_bound_bytes >= options.worker_bytes);
}

#[test]
fn an_oversize_tensor_is_admitted_alone() {
    let dir = tempdir().unwrap();
    let mut options = TensorPipelineOptions::default();
    options.encode_threads = 2;
    options.cpu_tokens = 2;
    // Smaller than the largest tensor: staging, worker and publish budgets all
    // have to fall back to the oversize rule.
    options.staging_bytes = 64 * 1024;
    options.encode_bytes = 64 * 1024;
    options.publish_bytes = 64 * 1024;
    options.worker_bytes = 64 * 1024;
    let written = write_group(&dir.path().join("oversize"), &options, false).unwrap();
    assert_eq!(written.objects.len(), 4);
    assert!(written.stats.oversize_staging >= 1);
    // A fragment larger than the publish budget is admitted alone, so the
    // publisher's in-flight bytes are bounded by the largest fragment (the 1 MiB
    // tensor's chosen frame is ~675 KB), never by the budget.
    assert!(written.stats.oversize_publish >= 1);
    assert!(
        written.stats.observed_max_publish_bytes <= 1024 * 1024,
        "the single publisher never holds more than one fragment: {}",
        written.stats.observed_max_publish_bytes
    );
}

#[test]
fn a_failing_reader_publishes_nothing_and_names_the_tensor() {
    let dir = tempdir().unwrap();
    let fixtures = fixture();
    let root = dir.path().join("fail");
    let mut store = Store::init(&root, opts()).unwrap();
    let root_id = store.resolve_path("/").unwrap();
    let tensors_dir = store
        .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
        .unwrap();
    let tensors: Vec<TensorPipelineTensor<'_>> = fixtures
        .iter()
        .map(|item| TensorPipelineTensor {
            name: item.name,
            dtype: item.dtype,
            shape: &item.shape,
            seed: seed_for(item.name),
            byte_len: item.bytes.len() as u64,
        })
        .collect();
    let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
        if index == 2 {
            return Err(Error::io("synthetic read failure"));
        }
        buf.copy_from_slice(&fixtures[index].bytes);
        Ok(())
    };
    let options = TensorPipelineOptions::default();
    store.begin_group().unwrap();
    let error = store
        .prepare_tensor_group_pipelined(
            &tensors,
            &read,
            &spec(false),
            &TensorLayoutSpec::row_major(),
            &options,
        )
        .err()
        .expect("the pipeline must fail when a read fails");
    assert!(
        error.message().contains("tiny.norm"),
        "the error must name the tensor: {}",
        error.message()
    );
    store.abort_group();
    // Nothing was committed, so the store still has exactly one generation and
    // /tensors is empty.
    let reopened = Store::open_with_options(&root, OpenMode::ReadOnly, opts()).unwrap();
    let entries: Vec<_> = reopened
        .list_dir(tensors_dir)
        .unwrap_or_default()
        .into_iter()
        .collect();
    assert!(entries.is_empty(), "an aborted pipeline published an entry");
}

#[test]
fn a_strategy_hint_still_forces_that_representation() {
    let dir = tempdir().unwrap();
    let fixtures = fixture();
    let root = dir.path().join("hint");
    let mut store = Store::init(&root, opts()).unwrap();
    let root_id = store.resolve_path("/").unwrap();
    let tensors_dir = store
        .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
        .unwrap();
    let tensors: Vec<TensorPipelineTensor<'_>> = fixtures
        .iter()
        .map(|item| TensorPipelineTensor {
            name: item.name,
            dtype: item.dtype,
            shape: &item.shape,
            seed: seed_for(item.name),
            byte_len: item.bytes.len() as u64,
        })
        .collect();
    let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
        buf.copy_from_slice(&fixtures[index].bytes);
        Ok(())
    };
    let mut hinted = spec(false);
    hinted.strategy_hint = Some(TensorStrategy::Raw);
    store.begin_group().unwrap();
    let outcome = store
        .prepare_tensor_group_pipelined(
            &tensors,
            &read,
            &hinted,
            &TensorLayoutSpec::row_major(),
            &TensorPipelineOptions::default(),
        )
        .unwrap();
    for result in &outcome.results {
        assert_eq!(result.prepared.strategy, "raw");
    }
}
