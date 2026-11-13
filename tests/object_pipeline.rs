//! N04: the R22 bounded encode-and-publish pipeline on an object backend.
//!
//! The pipeline is *where* the work happens, never *what* is published. On a
//! remote object backend the early-barrier overlap of R21 has nothing to do —
//! there is no published file to `fsync` and the provider's acknowledgement of
//! the `PUT` is the durability point — so the pipeline must report
//! `provider-ack`, claim no barrier, and keep the R21 ordering rule intact:
//! every immutable blob is published before the caller's commit record, so a
//! crash leaves unreferenced blobs and never a dangling commit/catalog entry.
//!
//! `ObjectBackend` is the shipped rename-free immutable-object namespace on the
//! local filesystem; it exercises the same `BlobPlane` publication path a remote
//! object store uses without needing a server. The S3 arm runs only when the
//! S3 court's environment is present, and skips (returns) otherwise, exactly as
//! `tests/s3_backend.rs` does.
//!
//! This is a single-host, synthetic-fixture court. It makes no media,
//! power-loss, fleet, cloud or production claim.

use std::collections::BTreeMap;
use std::path::Path;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key};
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_pipeline::{TensorPipelineOptions, TensorPipelineTensor};
use weftfs::tensor_store::{TensorLayoutSpec, TensorWriteSpec};
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

fn seed_for(name: &str) -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in name.as_bytes().iter().enumerate() {
        seed[index % 32] = seed[index % 32].wrapping_add(*byte).wrapping_add(1);
    }
    seed[31] = name.len() as u8;
    seed
}

fn spec() -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan: weftfs::tensor_store::TensorPlanOptions::default(),
        tile_shape: None,
    }
}

struct Written {
    head: Hash32,
    generation: u64,
    manifests: BTreeMap<String, Vec<u8>>,
    content_roots: BTreeMap<String, Hash32>,
    model: &'static str,
    early_barrier_applicable: bool,
    provider_acknowledged_bytes: u64,
    writeback_files: u64,
}

/// Prepare the group through the pipeline, publish, commit, and read back the
/// canonical manifest bytes for every tensor. `init` selects a fresh store or an
/// existing one (the reader test writes into a store that already exists).
fn write_group_in(
    root: &Path,
    kind: BackendKind,
    workers: usize,
    init: bool,
) -> Result<Written, Error> {
    let fixtures = fixture();
    let mut store = if init {
        Store::init_with_backend(root, opts(), kind)?
    } else {
        Store::open_with_backend(root, OpenMode::ReadWrite, kind)?
    };
    let root_id = store.resolve_path("/")?;
    let tensors_dir = if init {
        store.mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")?
    } else {
        store.resolve_path("/tensors")?
    };
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
    let mut options = TensorPipelineOptions::default();
    options.encode_threads = workers;
    options.cpu_tokens = workers;
    let outcome = store.prepare_tensor_group_pipelined(
        &tensors,
        &read,
        &spec(),
        &TensorLayoutSpec::row_major(),
        &options,
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
    let mut manifests = BTreeMap::new();
    let mut content_roots = BTreeMap::new();
    for (index, id) in ids.iter().enumerate() {
        let record = store.object_meta(*id)?;
        content_roots.insert(fixtures[index].name.to_owned(), record.content);
        manifests.insert(
            fixtures[index].name.to_owned(),
            store
                .read_blob_for_bench(&Key::manifest(record.content))
                .unwrap_or_default(),
        );
    }
    Ok(Written {
        head,
        generation: store.generation(),
        manifests,
        content_roots,
        model: outcome.stats.durability_model,
        early_barrier_applicable: outcome.stats.early_barrier_applicable,
        provider_acknowledged_bytes: outcome.stats.provider_acknowledged_bytes,
        writeback_files: outcome.stats.writeback_files,
    })
}

fn write_group(root: &Path, kind: BackendKind, workers: usize) -> Result<Written, Error> {
    write_group_in(root, kind, workers, true)
}

// ---------------------------------------------------------------------------
// Identity: the object backend changes nothing the pipeline publishes
// ---------------------------------------------------------------------------

#[test]
fn object_backend_pipeline_is_byte_identical_at_every_worker_count() {
    let dir = tempdir().unwrap();
    let mut reference: Option<Written> = None;
    for workers in [1usize, 2, 4, 8] {
        let root = dir.path().join(format!("w{workers}"));
        let written = write_group(&root, BackendKind::Object, workers).unwrap();
        // The object namespace publishes with fsync(2) + link(2), so this is a
        // file-barrier backend and the early barrier is applicable.
        assert_eq!(written.model, "file-barrier");
        assert!(written.early_barrier_applicable);
        assert_eq!(written.provider_acknowledged_bytes, 0);
        let store =
            Store::open_with_backend(&root, OpenMode::ReadOnly, BackendKind::Object).unwrap();
        assert!(store.verify(true).unwrap().ok);
        match &reference {
            None => reference = Some(written),
            Some(expected) => {
                assert_eq!(written.head, expected.head, "workers={workers} head");
                assert_eq!(
                    written.generation, expected.generation,
                    "workers={workers} generation"
                );
                assert_eq!(
                    written.manifests, expected.manifests,
                    "workers={workers} manifest bytes"
                );
                assert_eq!(
                    written.content_roots, expected.content_roots,
                    "workers={workers} content roots"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Durability model: never claim a file barrier a backend does not have
// ---------------------------------------------------------------------------

#[test]
fn provider_ack_backend_reports_provider_durability_and_claims_no_barrier() {
    // Skip when the S3 court's environment is absent: a local directory is not
    // an object-store provider and is never substituted for one.
    let Ok(endpoint) = std::env::var("WEFT_S3_TEST_ENDPOINT") else {
        eprintln!(
            "SKIP object_pipeline: WEFT_S3_TEST_ENDPOINT is not set; run scripts/s3_court.sh"
        );
        return;
    };
    let prefix = std::env::var("WEFT_S3_TEST_PREFIX").unwrap_or_else(|_| "weft-n04".to_owned());
    let config = weftfs::s3::S3Config {
        endpoint,
        bucket: format!("{prefix}-{}", std::process::id()),
        region: std::env::var("WEFT_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
        access_key: std::env::var("WEFT_S3_ACCESS_KEY")
            .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
            .expect("S3 access key"),
        secret_key: std::env::var("WEFT_S3_SECRET_KEY")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .expect("S3 secret key"),
        session_token: None,
        path_style: true,
        lock_lease: std::time::Duration::from_secs(60),
        request_timeout: std::time::Duration::from_secs(60),
    };
    let kind = BackendKind::S3 {
        config: config.clone(),
    };
    let dir = tempdir().unwrap();
    let store_path = dir.path().join("s3-pipeline-store");
    let written = match write_group(&store_path, kind.clone(), 2) {
        Ok(written) => written,
        Err(err) => panic!("S3 pipeline write failed: {err}"),
    };
    assert_eq!(written.model, "provider-ack");
    assert!(!written.early_barrier_applicable);
    assert_eq!(written.writeback_files, 0, "no file barrier was claimed");
    assert!(
        written.provider_acknowledged_bytes > 0,
        "the provider acknowledged published bytes"
    );
    let store = Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind).unwrap();
    assert!(store.verify(true).unwrap().ok);
    assert_eq!(store.backend_stats().fsyncs, 0);
}

// ---------------------------------------------------------------------------
// Ordering: a crash before the commit names nothing
// ---------------------------------------------------------------------------

#[test]
fn blobs_published_without_a_commit_are_unreferenced_and_reclaimable() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("store");
    // Two generations: genesis + /tensors. Then the pipeline publishes blobs
    // and deliberately does not commit them.
    let before = {
        let mut store = Store::init_with_backend(&root, opts(), BackendKind::Object).unwrap();
        let root_id = store.resolve_path("/").unwrap();
        store
            .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
            .unwrap();
        let head = store.head();
        let generation = store.generation();
        (head, generation)
    };
    let fixtures = fixture();
    {
        let mut store =
            Store::open_with_backend(&root, OpenMode::ReadWrite, BackendKind::Object).unwrap();
        let tensors_dir = store.resolve_path("/tensors").unwrap();
        // Open a group so the prepared objects and their published blobs are
        // never committed: the exact state a crash between the last published
        // blob and the commit record leaves behind.
        store.begin_group().unwrap();
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
        let mut options = TensorPipelineOptions::default();
        options.encode_threads = 2;
        options.cpu_tokens = 2;
        let outcome = store
            .prepare_tensor_group_pipelined(
                &tensors,
                &read,
                &spec(),
                &TensorLayoutSpec::row_major(),
                &options,
            )
            .unwrap();
        // Publish only, never commit: exactly the state a crash after the last
        // published blob and before the commit record leaves behind.
        for (index, result) in outcome.results.iter().enumerate() {
            store
                .commit_prepared_tensor_object(
                    tensors_dir,
                    fixtures[index].name.as_bytes(),
                    &result.prepared,
                    0o644,
                )
                .unwrap();
        }
    }
    // The dropped handle never committed the group, so the reopened store is at
    // the previous head and verifies clean: no dangling commit entry.
    let mut store = Store::open_with_options_and_backend(
        &root,
        OpenMode::ReadWrite,
        opts(),
        BackendKind::Object,
    )
    .unwrap();
    assert_eq!(store.head(), before.0);
    assert_eq!(store.generation(), before.1);
    assert!(store.verify(true).unwrap().ok);
    let report = store.gc(true).unwrap();
    assert!(
        !report.deleted.is_empty(),
        "the orphan blobs are reclaimable"
    );
    assert!(store.verify(true).unwrap().ok);
}

// ---------------------------------------------------------------------------
// A stale reader sees a prefix, never a newer publication
// ---------------------------------------------------------------------------

#[test]
fn a_reader_opened_before_the_commit_never_sees_the_new_state() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("store");
    {
        let mut store = Store::init_with_backend(&root, opts(), BackendKind::Object).unwrap();
        let root_id = store.resolve_path("/").unwrap();
        store
            .mkdir_seeded(root_id, b"tensors", 0o755, 0, 0, b"tensors-dir")
            .unwrap();
    }
    let reader = Store::open_with_backend(&root, OpenMode::ReadOnly, BackendKind::Object).unwrap();
    let head_before = reader.head();
    let generation_before = reader.generation();
    assert!(reader.resolve_path("/tensors").is_ok());
    assert!(reader.resolve_path("/late").is_err());

    let written = write_group_in(&root, BackendKind::Object, 2, false).unwrap();
    assert_ne!(written.head, head_before);

    // The open reader is pinned to the commit it opened; the newly written
    // tensor names do not suddenly appear in it.
    assert_eq!(reader.head(), head_before);
    assert_eq!(reader.generation(), generation_before);
    assert!(reader.resolve_path("/late").is_err());
    for tensor in fixture() {
        assert!(
            reader
                .resolve_path(&format!("/tensors/{}", tensor.name))
                .is_err(),
            "stale reader must not see {}",
            tensor.name
        );
    }
    drop(reader);
    let fresh = Store::open_with_backend(&root, OpenMode::ReadOnly, BackendKind::Object).unwrap();
    assert_eq!(fresh.head(), written.head);
    assert!(fresh.resolve_path("/tensors/big.weight").is_ok());
}

// ---------------------------------------------------------------------------
// A lost acknowledgement, retried, is idempotent
// ---------------------------------------------------------------------------

#[test]
fn republishing_an_acknowledged_fragment_is_idempotent() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("store");
    let fixtures = fixture();
    let first = write_group(&root, BackendKind::Object, 2).unwrap();

    // A writer whose provider acknowledgement was lost retries the *identical
    // publication*, not the whole object install. `store_blob` re-reads an
    // existing blob, re-hashes it and accepts it only when it matches its name,
    // so a retry cannot rewrite or corrupt anything, publishes no new bytes and
    // leaves the committed state unchanged.
    let store = Store::open_with_backend(&root, OpenMode::ReadWrite, BackendKind::Object).unwrap();
    let writes_before = store.backend_stats().bytes_written;
    for tensor in &fixtures {
        let encoded = store
            .encode_tensor_object(
                tensor.dtype,
                &tensor.shape,
                &tensor.bytes,
                &TensorLayoutSpec::row_major(),
                &spec(),
                &seed_for(tensor.name),
            )
            .unwrap();
        let first_publish = store.publish_encoded_tensor(encoded.clone()).unwrap();
        // The retry: same frame, provider acknowledgement assumed lost.
        let retry = store.publish_encoded_tensor(encoded).unwrap();
        assert_eq!(retry.manifest_id, first_publish.manifest_id);
        assert_eq!(retry.content_root, first_publish.content_root);
        assert_eq!(retry.logical_root, first_publish.logical_root);
    }
    assert_eq!(
        store.backend_stats().bytes_written,
        writes_before,
        "a retried publication writes no new bytes"
    );
    assert_eq!(store.head(), first.head);
    assert_eq!(store.generation(), first.generation);
    assert!(store.verify(true).unwrap().ok);
    let ledger = store.publication_ledger();
    assert!(
        ledger.duplicate_blobs > 0,
        "the retry hit the create-once duplicate path"
    );
}

// ---------------------------------------------------------------------------
// Object and local namespaces publish the same logical state
// ---------------------------------------------------------------------------

#[test]
fn object_backend_publishes_the_same_tree_as_local() {
    let dir = tempdir().unwrap();
    let local = write_group(&dir.path().join("local"), BackendKind::Local, 2).unwrap();
    let object = write_group(&dir.path().join("object"), BackendKind::Object, 2).unwrap();
    assert_eq!(local.head, object.head);
    assert_eq!(local.generation, object.generation);
    assert_eq!(local.manifests, object.manifests);
    assert_eq!(local.content_roots, object.content_roots);
}
