//! Checkpoint saves through the pack plane (subphase R10, increment 2).
//!
//! The pack plane is a **physical publication batching layer**: a checkpoint's
//! small blobs (per-shard chunks and manifests under the member threshold) are
//! sealed into a few large packs and named by a compare-and-swap catalog. Only
//! physical placement changes, so every identity check below compares a packed
//! save against the flat save it must equal.
//!
//! Covered here:
//!
//! * the same head, checkpoint manifest bytes, and manifest root for the flat
//!   and packed sequential paths **and** the packed prepare/commit path at
//!   ranks 1/2/4;
//! * every tensor restores byte-identically from the packed store, and
//!   `verify --full` is clean;
//! * a crash at either pack publication phase leaves the old whole generation
//!   or the new whole one, never a catalog entry pointing at missing bytes;
//! * a catalog bound is a fallback to flat publication, never a failed commit;
//! * the honest cost: a cold read of one packed member is more than one request.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key};
use weftfs::checkpoint::{CheckpointWriteOptions, ParallelCheckpointOptions, ShardStrategy};
use weftfs::packs::PackOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SEED: [u8; 32] = [0x5a; 32];
const STAMP: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(STAMP),
        ..Options::default()
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
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

fn write_container(dir: &Path) -> (Vec<(String, Vec<u8>)>, std::path::PathBuf) {
    fs::create_dir_all(dir).unwrap();
    let a = random_bytes(64 * 32 * 2, 0x5eed_1234);
    let b = random_bytes(32 * 32 * 2, 0x5eed_5678);
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: "alpha",
                dtype: "bf16",
                shape: &[64, 32],
                data: &a,
            },
            SafetensorsEntry {
                name: "beta",
                dtype: "bf16",
                shape: &[32, 32],
                data: &b,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("pair.safetensors");
    fs::write(&path, &file).unwrap();
    (vec![("alpha".to_owned(), a), ("beta".to_owned(), b)], path)
}

fn write_options() -> CheckpointWriteOptions {
    CheckpointWriteOptions {
        shards: 4,
        axis: 0,
        strategy: ShardStrategy::TensorSharded,
        deterministic_seed: Some(SEED),
        write_policy: weftfs::tensor_store::WritePolicy::Fast,
        // Fast raw is the pathological case: every shard is a chunk table, so
        // the save publishes hundreds of small chunk blobs.
        fast_codec: weftfs::tensor_store::FastCodec::Raw,
        ..CheckpointWriteOptions::default()
    }
}

fn pack_options(catalog_bytes: u64, segments: u32) -> PackOptions {
    PackOptions {
        pack_target_bytes: 1 << 20,
        member_threshold_bytes: 64 * 1024,
        max_members_per_pack: 65_536,
        max_catalog_bytes: catalog_bytes,
        catalog_segments: segments,
    }
}

fn packed() -> BackendKind {
    BackendKind::packed(BackendKind::Local, pack_options(4 << 20, 16)).unwrap()
}

fn flat() -> BackendKind {
    BackendKind::Local
}

struct Written {
    head: String,
    manifest: Vec<u8>,
    manifest_root: String,
    generation: u64,
    manifest_blob: weftfs::Hash32,
    pack: Option<weftfs::PackStats>,
    diagnostics: String,
}

fn write_one(kind: BackendKind, path: &Path, source: &Path, ranks: Option<usize>) -> Written {
    let mut store = Store::init_with_backend(path, opts(), kind).unwrap();
    let options = write_options();
    let report = match ranks {
        None => store.write_checkpoint("ckpt", source, &options).unwrap(),
        Some(ranks) => store
            .write_checkpoint_parallel(
                "ckpt",
                source,
                &options,
                &ParallelCheckpointOptions {
                    ranks,
                    // One tensor per in-flight task is enough for this fixture.
                    max_in_flight_bytes: 1 << 20,
                    per_rank_bytes: 64 << 20,
                    ..ParallelCheckpointOptions::default()
                },
            )
            .unwrap(),
    };
    let manifest_id = store.resolve_path("/checkpoints/ckpt/manifest").unwrap();
    let manifest = store.read_object(manifest_id).unwrap();
    let manifest_blob = store.object_meta(manifest_id).unwrap().content;
    Written {
        head: report.head.to_hex(),
        manifest,
        manifest_root: report.manifest_root.to_hex(),
        generation: store.generation(),
        manifest_blob,
        // Pack counters are per-handle activity: read them from the handle that
        // published, never from a later read-only one.
        pack: store.pack_stats(),
        diagnostics: store.backend_diagnostics().join("\n"),
    }
}

#[test]
fn packed_and_flat_checkpoints_publish_the_same_identity() {
    let dir = tempdir().unwrap();
    let (payloads, source) = write_container(&dir.path().join("src"));
    let flat_run = write_one(flat(), &dir.path().join("flat"), &source, None);
    let packed_run = write_one(packed(), &dir.path().join("packed"), &source, None);
    assert_eq!(
        flat_run.manifest, packed_run.manifest,
        "manifest bytes differ"
    );
    assert_eq!(
        flat_run.manifest_root, packed_run.manifest_root,
        "manifest root differs"
    );
    assert_eq!(flat_run.head, packed_run.head, "commit head differs");
    assert_eq!(
        flat_run.generation, packed_run.generation,
        "generation differs"
    );

    // The prepare/commit path must publish the same state as the sequential
    // one, with and without packs.
    for ranks in [1usize, 2, 4] {
        let parallel = write_one(
            packed(),
            &dir.path().join(format!("packed-r{ranks}")),
            &source,
            Some(ranks),
        );
        assert_eq!(parallel.head, flat_run.head, "ranks {ranks}: head differs");
        assert_eq!(
            parallel.manifest, flat_run.manifest,
            "ranks {ranks}: manifest bytes differ"
        );
        assert_eq!(
            parallel.manifest_root, flat_run.manifest_root,
            "ranks {ranks}: manifest root differs"
        );
        assert_eq!(
            parallel.generation, flat_run.generation,
            "ranks {ranks}: generation differs"
        );
    }

    // The packed save must actually be packed (otherwise the test proves
    // nothing), and every tensor must restore byte-identically.
    let pack = packed_run.pack.expect("packed plane counters");
    assert!(pack.sealed_packs > 0, "no pack was sealed: {pack:?}");
    assert!(pack.packed_members > 0, "no member was packed: {pack:?}");
    assert_eq!(
        pack.flat_members, 0,
        "fast-raw shards should all be packed: {pack:?}"
    );
    let store =
        Store::open_with_backend(&dir.path().join("packed"), OpenMode::ReadOnly, packed()).unwrap();
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    for (name, expected) in &payloads {
        let bytes = store
            .restore_named_tensor_at(None, "ckpt", name, None, None)
            .unwrap();
        assert_eq!(
            &bytes.bytes, expected,
            "tensor `{name}` restored differently"
        );
    }
}

#[test]
fn pack_catalog_bound_falls_back_to_flat_and_still_commits() {
    let dir = tempdir().unwrap();
    let (_payloads, source) = write_container(&dir.path().join("src"));
    // One segment needs a 16-byte header, a 32-byte digest, and 113 bytes per
    // entry: 161 bytes. This bound fits one entry and nothing more.
    let tiny = BackendKind::packed(BackendKind::Local, pack_options(400, 1)).unwrap();
    let bounded = write_one(tiny, &dir.path().join("tiny"), &source, None);
    let flat_run = write_one(flat(), &dir.path().join("flat"), &source, None);
    // Identity does not depend on where a blob physically lives.
    assert_eq!(bounded.head, flat_run.head);
    assert_eq!(bounded.manifest, flat_run.manifest);
    assert_eq!(bounded.manifest_root, flat_run.manifest_root);
    assert!(
        bounded.diagnostics.contains("catalog bound reached"),
        "expected a flat fallback note: {}",
        bounded.diagnostics
    );

    // A plane configured for a different segment count refuses to read the
    // catalog rather than guessing: the root names exactly one segment.
    let mismatched =
        Store::open_with_backend(&dir.path().join("tiny"), OpenMode::ReadOnly, packed()).unwrap();
    let err = mismatched
        .read_blob_for_bench(&Key::manifest(bounded.manifest_blob))
        .expect_err("a mismatched catalog shape must be refused");
    let text = format!("{err}");
    assert!(text.contains("segment"), "unexpected refusal text: {text}");
    drop(mismatched);

    // With its own policy the same store opens, verifies, and restores.
    let store = Store::open_with_backend(
        &dir.path().join("tiny"),
        OpenMode::ReadOnly,
        BackendKind::packed(BackendKind::Local, pack_options(400, 1)).unwrap(),
    )
    .unwrap();
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[test]
fn packed_checkpoint_crash_proxy_yields_old_or_new() {
    let dir = tempdir().unwrap();
    let (payloads, source) = write_container(&dir.path().join("src"));
    for phase in ["pack_after_pack_object", "pack_after_catalog_segments"] {
        let path = dir.path().join(format!("crash-{phase}"));
        // A first checkpoint establishes an "old" whole generation.
        let mut store = Store::init_with_backend(&path, opts(), packed()).unwrap();
        store
            .write_checkpoint("first", &source, &write_options())
            .unwrap();
        let before_generation = store.generation();
        drop(store);

        let child = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .args([
                "--pack",
                "--pack-target-bytes",
                "1048576",
                "--pack-member-threshold",
                "65536",
                "--pack-catalog-bytes",
                "4194304",
                "--pack-segments",
                "16",
                "--store",
            ])
            .arg(&path)
            .args(["checkpoint", "write", "--file"])
            .arg(&source)
            .args([
                "--name",
                "second",
                "--shards",
                "4",
                "--axis",
                "0",
                "--shard-strategy",
                "tensor-sharded",
                "--write-policy",
                "fast",
                "--fast-codec",
                "raw",
                "--checkpoint-seed",
                "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
            ])
            .env("WEFT_CRASH_PHASE", phase)
            .output()
            .expect("spawn weftfs");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            child.status.signal(),
            Some(6),
            "phase {phase}: the child must abort at the pack publication phase, not fail earlier; \
             stdout={} stderr={}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );

        let store = Store::open_with_backend(&path, OpenMode::ReadWrite, packed()).unwrap();
        let report = store.verify(true).unwrap();
        assert!(
            report.problems.is_empty(),
            "phase {phase}: verify problems {:?}",
            report.problems
        );
        if phase == "pack_after_catalog_segments" {
            // Segments published but no root: the catalog is the old whole one.
            assert_eq!(store.generation(), before_generation);
        }
        // The old checkpoint is always whole and readable.
        for (name, expected) in &payloads {
            let bytes = store
                .restore_named_tensor_at(None, "first", name, None, None)
                .unwrap();
            assert_eq!(&bytes.bytes, expected, "phase {phase}: `{name}` changed");
        }
        // The new one is either absent or complete, never partial.
        if store.load_checkpoint("second").is_ok() {
            for (name, expected) in &payloads {
                let bytes = store
                    .restore_named_tensor_at(None, "second", name, None, None)
                    .unwrap();
                assert_eq!(&bytes.bytes, expected, "phase {phase}: `{name}` partial");
            }
        }
    }
}

#[test]
fn cold_read_of_a_packed_member_costs_more_requests() {
    let dir = tempdir().unwrap();
    let (_payloads, source) = write_container(&dir.path().join("src"));
    let flat_run = write_one(flat(), &dir.path().join("flat"), &source, None);
    write_one(packed(), &dir.path().join("packed"), &source, None);

    // Read exactly one small manifest member from a fresh handle: the flat
    // plane needs one GET, the packed plane needs the catalog root, one
    // segment, and one ranged pack read.
    let flat_store =
        Store::open_with_backend(&dir.path().join("flat"), OpenMode::ReadOnly, flat()).unwrap();
    let packed_store =
        Store::open_with_backend(&dir.path().join("packed"), OpenMode::ReadOnly, packed()).unwrap();
    let key = Key::manifest(flat_run.manifest_blob);
    let before = flat_store.backend_stats();
    let flat_bytes = flat_store.read_blob_for_bench(&key).unwrap();
    let flat_reads = flat_store.backend_stats().reads - before.reads;
    let before = packed_store.backend_stats();
    let packed_bytes = packed_store.read_blob_for_bench(&key).unwrap();
    let packed_requests = (packed_store.backend_stats().reads - before.reads)
        + (packed_store.backend_stats().range_reads - before.range_reads);
    assert_eq!(flat_bytes, packed_bytes);
    assert!(
        flat_reads >= 1,
        "a flat member is one request (got {flat_reads})"
    );
    assert!(
        packed_requests > flat_reads,
        "a cold packed member must cost more requests (flat {flat_reads}, packed {packed_requests})"
    );
}
