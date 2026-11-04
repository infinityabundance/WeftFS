//! Extent-level copy-on-write range writes.
//!
//! A small overwrite in a large file must reuse every untouched chunk
//! reference verbatim: only the changed chunks may produce new object blobs.
//! The frozen v1 whole-file ContentId still costs one O(size) digest pass; these
//! tests assert the *physical* write amplification is O(changed chunks) and that
//! correctness (bytes, size, whole-content hash, crash recovery) is unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

use weftfs::recovery::read_manifest;
use weftfs::store::{OpenMode, Options, Store};

fn opts(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
        ..Options::default()
    }
}

fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else {
                out.insert(path);
            }
        }
    }
    out
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// One overwrite inside a single chunk of an 8-chunk object must add exactly
/// one new object blob and leave every other chunk reference byte-identical.
#[test]
fn overwrite_one_chunk_reuses_the_other_chunks_verbatim() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(16)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let original = pattern(128);
    let id = store
        .create_file(root, b"f", &original, 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    let old_manifest_id = store.object_meta(id).unwrap().content;
    let old = read_manifest(store.root(), old_manifest_id).unwrap();
    assert_eq!(old.chunk_size, 16);
    assert_eq!(old.chunks.len(), 8);

    let objects_before = files_under(&store.root().join("objects"));
    let manifests_before = files_under(&store.root().join("manifests"));

    let mut model = original.clone();
    model[20..23].copy_from_slice(b"XYZ");
    store.write_file(id, 20, b"XYZ").unwrap();

    assert_eq!(store.read_object(id).unwrap(), model);
    let meta = store.object_meta(id).unwrap();
    assert_eq!(meta.size, 128);
    let new = read_manifest(store.root(), meta.content).unwrap();
    assert_eq!(new.chunk_size, 16);
    assert_eq!(new.size, 128);
    assert_eq!(new.chunks.len(), 8);
    assert_eq!(
        new.content_hash,
        weftfs::identity::content_hash(&model),
        "content hash must match the model"
    );

    for index in 0..8 {
        if index == 1 {
            assert_ne!(
                new.chunks[index].hash, old.chunks[index].hash,
                "the written chunk must change"
            );
        } else {
            assert_eq!(
                new.chunks[index], old.chunks[index],
                "chunk {index} must be reused verbatim"
            );
        }
    }

    let objects_after = files_under(&store.root().join("objects"));
    let manifests_after = files_under(&store.root().join("manifests"));
    let new_objects: Vec<_> = objects_after.difference(&objects_before).collect();
    let new_manifests: Vec<_> = manifests_after.difference(&manifests_before).collect();
    assert_eq!(
        new_objects.len(),
        1,
        "exactly one changed chunk may add one object blob, got {new_objects:?}"
    );
    assert_eq!(
        new_manifests.len(),
        1,
        "one new manifest expected, got {new_manifests:?}"
    );
    // Untouched blobs are still present: nothing was rewritten in place.
    for index in [0usize, 2, 3, 4, 5, 6, 7] {
        let path = weftfs::recovery::object_path(store.root(), old.chunks[index].hash);
        assert!(
            objects_after.contains(&path),
            "old chunk {index} blob disappeared"
        );
    }
}

/// Rewriting a chunk with identical bytes must not publish a new blob at all:
/// the reconstructed chunk compares equal and the old reference is reused.
#[test]
fn identical_bytes_add_no_blob() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(16)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let original = pattern(128);
    let id = store
        .create_file(root, b"f", &original, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let objects_before = files_under(&store.root().join("objects"));
    let manifests_before = files_under(&store.root().join("manifests"));

    store.write_file(id, 20, &original[20..23]).unwrap();

    assert_eq!(store.read_object(id).unwrap(), original);
    let objects_after = files_under(&store.root().join("objects"));
    let manifests_after = files_under(&store.root().join("manifests"));
    assert_eq!(
        objects_after.difference(&objects_before).count(),
        0,
        "an unchanged chunk must not be re-stored"
    );
    assert_eq!(
        manifests_after.difference(&manifests_before).count(),
        0,
        "an identical manifest must deduplicate"
    );
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }
}

fn run_differential(chunk_size: u64, seed: u64) {
    // Bound the model so every full-content verification stays cheap; the
    // interesting behaviour is chunk-boundary crossing, not absolute size.
    const CAP: u64 = 192 * 1024;
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(chunk_size)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"f", b"", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let mut model: Vec<u8> = Vec::new();
    let mut rng = Rng(seed | 1);

    for op in 0..500usize {
        let span = (model.len() as u64 + 3 * chunk_size + 1).min(CAP);
        let offset = rng.below(span);
        let max_len = 3 * chunk_size + 1;
        let len = rng
            .below(max_len)
            .min(CAP.saturating_sub(offset))
            .min(max_len);
        let mut bytes = vec![0u8; usize::try_from(len).unwrap()];
        for byte in bytes.iter_mut() {
            *byte = (rng.next() & 0xff) as u8;
        }

        store.write_file(id, offset, &bytes).unwrap();

        // Model: a zero-length write is a no-op; otherwise the file grows to
        // cover the range (zero-filling the gap) and the bytes are spliced in.
        if !bytes.is_empty() {
            if offset + len > model.len() as u64 {
                model.resize(usize::try_from(offset + len).unwrap(), 0);
            }
            model[usize::try_from(offset).unwrap()..usize::try_from(offset + len).unwrap()]
                .copy_from_slice(&bytes);
        }

        assert_eq!(
            store.read_object(id).unwrap(),
            model,
            "bytes diverged at chunk_size={chunk_size} seed={seed} op={op}"
        );
        assert_eq!(
            store.object_meta(id).unwrap().size,
            model.len() as u64,
            "size diverged at chunk_size={chunk_size} seed={seed} op={op}"
        );

        if (op + 1) % 50 == 0 {
            drop(store);
            store = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
            let report = store.verify(true).unwrap();
            assert!(
                report.ok,
                "verify failed at chunk_size={chunk_size} seed={seed} op={op}: {:?}",
                report.problems
            );
            assert_eq!(store.read_object(id).unwrap(), model);
        }
    }
}

#[test]
fn randomised_range_writes_match_an_in_memory_model() {
    for chunk_size in [16u64, 4096] {
        for seed in [5u64, 17, 41] {
            run_differential(chunk_size, seed);
        }
    }
}

fn crash_write_child(store_path: &Path, phase: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .arg("--store")
        .arg(store_path)
        .args(["write", "/f", "--offset", "20", "--text", "ZZZ"])
        .env("WEFT_CRASH_PHASE", phase)
        .status()
        .expect("spawn weftfs binary");
    assert!(
        !status.success(),
        "child was expected to abort at phase {phase}, but exited {status:?}"
    );
}

/// Process-fault crash proxy over the COW path: a fatal stop after the new
/// chunk blob is durable but before the commit, and after the commit file but
/// before the superblock slot, yields the old or the new whole file, never a
/// partial extent.
#[test]
fn crash_proxy_on_range_write_is_old_or_new_whole() {
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let dir = tempdir().unwrap();
        let mut store = Store::init(dir.path(), opts(16)).unwrap();
        let root = store.resolve_path("/").unwrap();
        let original = pattern(128);
        let id = store
            .create_file(root, b"f", &original, 0o644, 0, 0, BTreeMap::new())
            .unwrap();
        let generation = store.generation();
        drop(store);

        crash_write_child(dir.path(), phase);

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        let mut expected = original.clone();
        expected[20..23].copy_from_slice(b"ZZZ");
        let bytes = store.read_object(id).unwrap();
        if phase == "after_superblock_write" {
            assert_eq!(
                store.generation(),
                generation + 1,
                "phase {phase} must publish the new generation"
            );
            assert_eq!(bytes, expected, "phase {phase} must expose the new bytes");
        } else {
            assert_eq!(
                store.generation(),
                generation,
                "phase {phase} must not publish the new generation"
            );
            assert_eq!(bytes, original, "phase {phase} must keep the old bytes");
        }
        let report = store.verify(true).unwrap();
        match phase {
            "after_superblock_write" => {
                assert!(report.ok, "phase {phase}: {:?}", report.problems);
            }
            "after_commit_file" => {
                // The commit file is durable but unpublished. Recovery treats it
                // as an ignored tail (the legal old-state outcome) and `verify`
                // reports exactly that one explicit tail diagnostic. Anything
                // else would be a real problem.
                assert_eq!(
                    report.problems.len(),
                    1,
                    "phase {phase}: {:?}",
                    report.problems
                );
                assert!(
                    report.problems[0]
                        .detail
                        .contains("exists above the highest published superblock generation"),
                    "phase {phase}: {:?}",
                    report.problems
                );
            }
            _ => assert!(report.ok, "phase {phase}: {:?}", report.problems),
        }
    }
}

/// The object id is stable across the write, so a caller that re-reads the
/// manifest sees the same chunk_size the file was created with.
#[test]
fn chunk_size_comes_from_the_manifest() {
    for chunk_size in [16u64, 4096] {
        let dir = tempdir().unwrap();
        let mut store = Store::init(dir.path(), opts(chunk_size)).unwrap();
        let root = store.resolve_path("/").unwrap();
        store
            .create_file(root, b"f", &pattern(3 * 16), 0o644, 0, 0, BTreeMap::new())
            .unwrap();

        // Re-open with unrelated runtime options: chunk_size is a manifest
        // property, not a mutable store option.
        drop(store);
        let mut store = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
        let id = store.resolve_path("/f").unwrap();
        let mut model = pattern(3 * 16);
        let write_at = 5usize;
        store.write_file(id, write_at as u64, b"abc").unwrap();
        model[write_at..write_at + 3].copy_from_slice(b"abc");
        assert_eq!(store.read_object(id).unwrap(), model);

        let manifest = read_manifest(store.root(), store.object_meta(id).unwrap().content).unwrap();
        assert_eq!(manifest.chunk_size, chunk_size);
        assert_eq!(manifest.size, model.len() as u64);
    }
}

/// A zero-length write publishes nothing and returns the current version.
#[test]
fn zero_length_write_is_a_no_op() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts(16)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"f", b"hello", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let before = store.object_meta(id).unwrap();
    let generation = store.generation();
    let version = store.write_file(id, 2, b"").unwrap();
    assert_eq!(version, before.version);
    assert_eq!(store.generation(), generation);
    let after = store.object_meta(id).unwrap();
    assert_eq!(after.content, before.content);
    assert_eq!(after.size, before.size);
    assert_eq!(store.read_object(id).unwrap(), b"hello");
}
