//! `GcReport` byte accounting: the data counter must equal the real tree
//! delta, advisory index bytes must be reported separately, and a pinned
//! snapshot must reclaim nothing.
//!
//! Background: the trajectory court recorded `GcReport::deleted_bytes` at
//! ~3.47 GB while the data tree shrank by ~151 KB. The cause was that the
//! single counter summed data-blob bytes *and* advisory index-segment bytes
//! (3,271 stale segments of ~1 MB each). These tests pin the corrected
//! contract:
//!
//! * `deleted_bytes` = physical bytes of data blobs (objects + manifests)
//!   actually unlinked, which is exactly the shrink of the data tree;
//! * `deleted_index_bytes` = physical bytes of advisory index segments
//!   unlinked, reported separately and never added to the data counter;
//! * `deleted_total_bytes` = the sum of every component.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftfs::checkpoint::CheckpointWriteOptions;
use weftfs::identity::Hash32;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::state::State;
use weftfs::store::{Options, Store};

const SHAPE: [u64; 2] = [64, 64];
const TENSOR: &str = "weight";

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16 * 1024,
        // No automatic state checkpoints: the head and the explicit snapshot
        // are the only roots this fixture exercises.
        checkpoint_interval: 1_000_000,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn pattern(len: usize, seed: u64) -> Vec<u8> {
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

/// Bytes under one directory tree, following regular files only. This is the
/// `du`-equivalent the receipt uses; it is deliberately independent of the
/// backend implementation.
fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Every regular file under `indexes/` with its size.
fn index_files(root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let dir = root.join("indexes");
    let mut stack = vec![dir];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, meta.len());
            }
        }
    }
    out
}

fn data_bytes(root: &Path) -> u64 {
    dir_bytes(&root.join("objects")) + dir_bytes(&root.join("manifests"))
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let len = SHAPE.iter().product::<u64>() as usize * 2;
    let data = pattern(len, 0x9110_2233);
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let path = dir.join("weights.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

/// Unlink every file in a directory tree and then the tree itself, using only
/// the public store verbs (there is no recursive delete).
fn remove_tree(store: &mut Store, root: Hash32) -> weftfs::Result<()> {
    let entries = store.list_dir(root)?;
    for entry in entries {
        let is_dir = store
            .current_state()
            .objects
            .get(&entry.target)
            .map(|record| record.kind == weftfs::state::ObjectKind::Dir)
            .unwrap_or(false);
        if is_dir {
            remove_tree(store, entry.target)?;
            store.rmdir(root, &entry.name)?;
        } else {
            store.unlink(root, &entry.name)?;
        }
    }
    Ok(())
}

/// Write one checkpoint, force several commits (so index segments exist), then
/// delete the checkpoint's path. The snapshot (when requested) is the only
/// difference between the two stores.
fn build(keep_snapshot: bool) -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let source = write_container(dir.path());
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();

    store
        .write_checkpoint("c1", &source, &CheckpointWriteOptions::default())
        .unwrap();
    // Ordinary files force additional commits (and therefore additional
    // advisory index segments), like the trajectory court's 50-step series.
    // They stay reachable from the head in both variants.
    root_filler(&mut store);
    if keep_snapshot {
        store.snapshot_create("pin").unwrap();
    }
    let checkpoints = store.resolve_path("/checkpoints").unwrap();
    remove_tree(&mut store, checkpoints).unwrap();
    store.rmdir(State::ROOT_ID, b"checkpoints").unwrap();
    (dir, store)
}

fn root_filler(store: &mut Store) {
    for index in 0..12u32 {
        let name = format!("filler-{index:02}");
        let bytes = pattern(2048 + index as usize * 37, u64::from(index) + 7);
        store
            .create_file(
                State::ROOT_ID,
                name.as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
}

#[test]
fn pinned_snapshot_reclaims_no_data_and_reports_zero() {
    let (dir, mut store) = build(true);
    let root = dir.path().join("store");

    let before_data = data_bytes(&root);
    let before_indexes = index_files(&root);
    let report = store.gc(true).unwrap();
    let after_data = data_bytes(&root);
    let after_indexes = index_files(&root);

    // A snapshot pins the whole state: no data blob may be deleted.
    assert!(report.deleted.is_empty(), "deleted: {:?}", report.deleted);
    assert_eq!(report.deleted_bytes, 0, "data bytes must stay zero");
    assert_eq!(before_data, after_data, "the data tree must not shrink");

    // Advisory index segments are still swept; they are reported separately
    // and must never be folded into the data counter.
    let swept: BTreeSet<&String> = report.deleted_indexes.iter().collect();
    let expected_index_bytes: u64 = report
        .deleted_indexes
        .iter()
        .map(|name| *before_indexes.get(name).unwrap_or(&0))
        .sum();
    assert_eq!(report.deleted_index_bytes, expected_index_bytes);
    for name in &swept {
        assert!(
            !after_indexes.contains_key(*name),
            "index {name} still exists after gc"
        );
    }
    assert_eq!(
        report.deleted_total_bytes,
        report.deleted_bytes + report.deleted_index_bytes + report.reclaimed_pack_bytes
    );
}

#[test]
fn data_counter_equals_the_measured_tree_delta() {
    let (dir, mut store) = build(false);
    let root = dir.path().join("store");

    let before_data = data_bytes(&root);
    let before_indexes = index_files(&root);
    let report = store.gc(true).unwrap();
    let after_data = data_bytes(&root);
    let after_indexes = index_files(&root);

    // The data counter is exactly the physical shrink of objects+manifests.
    assert!(!report.deleted.is_empty(), "expected data blobs to collect");
    assert_eq!(
        report.deleted_bytes,
        before_data.saturating_sub(after_data),
        "deleted_bytes must equal the measured data-tree delta \
         (before {before_data}, after {after_data})"
    );

    // Every named index deletion is real, exactly sized, and excluded from the
    // data counter even though its bytes are real on disk.
    let expected_index_bytes: u64 = report
        .deleted_indexes
        .iter()
        .map(|name| *before_indexes.get(name).unwrap_or(&0))
        .sum();
    assert_eq!(report.deleted_index_bytes, expected_index_bytes);
    assert!(report.deleted_bytes > 0);
    assert!(
        report.deleted_index_bytes > 0,
        "the fixture must produce sweepable index segments"
    );
    for name in &report.deleted_indexes {
        assert!(!after_indexes.contains_key(name));
    }

    // The total reconciles exactly, and the data counter is strictly smaller
    // than the total whenever index bytes were swept (the original defect).
    assert_eq!(
        report.deleted_total_bytes,
        report.deleted_bytes + report.deleted_index_bytes + report.reclaimed_pack_bytes
    );
    assert!(report.deleted_total_bytes > report.deleted_bytes);

    // Applying GC again must reclaim nothing: the state is stable.
    let second = store.gc(true).unwrap();
    assert!(second.deleted.is_empty());
    assert_eq!(second.deleted_bytes, 0);
    assert_eq!(second.reclaimed_pack_bytes, 0);
}

#[test]
fn dry_run_reports_no_deleted_bytes() {
    let (dir, mut store) = build(false);
    let root = dir.path().join("store");
    let before = data_bytes(&root);
    let report = store.gc(false).unwrap();
    assert!(!report.applied);
    assert!(report.deleted.is_empty());
    assert_eq!(report.deleted_bytes, 0);
    assert_eq!(report.deleted_index_bytes, 0);
    assert_eq!(report.deleted_total_bytes, 0);
    assert_eq!(data_bytes(&root), before);
}
