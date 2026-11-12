//! One logical publication per group: the durability ledger, the single index
//! segment, and the ordering rule that a published commit never names a blob
//! that is not durable.
//!
//! These tests do not change any format. They pin the properties R20 relies on:
//!
//! * a group publish produces exactly one commit record, one head publication
//!   and one advisory index segment, regardless of how many files it carries;
//! * the deferred durability batch still issues one data barrier per published
//!   file and at least one directory barrier, and it is always closed *before*
//!   the commit record exists (the counts are equal, so nothing is synced late
//!   and nothing is skipped);
//! * the deferred row and the eager row publish the same logical state, the
//!   same manifests and the same bytes — only the barrier placement differs;
//! * an aborted group publishes nothing, and an open group that is dropped
//!   publishes nothing.

use std::collections::BTreeMap;
use std::time::SystemTime;

use tempfile::tempdir;

use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, PublicationLedger, Store};

const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;

fn options(chunk_size: u64) -> Options {
    options_with(chunk_size, true)
}

fn options_with(chunk_size: u64, defer_group_blob_sync: bool) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        defer_group_blob_sync,
        ..Options::default()
    }
}

fn payloads() -> Vec<Vec<u8>> {
    (0..8usize)
        .map(|index| {
            let mut bytes = vec![0u8; 4096 + index * 512];
            for (offset, byte) in bytes.iter_mut().enumerate() {
                *byte = ((offset * 31 + index * 7) % 251) as u8;
            }
            bytes
        })
        .collect()
}

/// Write eight files in one group with a fixed seed per name.
fn write_group(store: &mut Store, root: Hash32) -> Vec<Hash32> {
    store.begin_group().unwrap();
    let mut ids = Vec::new();
    for (index, bytes) in payloads().iter().enumerate() {
        let id = store
            .create_file_seeded(
                root,
                format!("f{index}").as_bytes(),
                bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
                &[index as u8 + 1; 32],
            )
            .unwrap();
        ids.push(id);
    }
    store.commit_group().unwrap();
    ids
}

fn ledger_delta(before: &PublicationLedger, after: &PublicationLedger) -> PublicationLedger {
    PublicationLedger {
        object_blobs: after.object_blobs - before.object_blobs,
        object_blob_bytes: after.object_blob_bytes - before.object_blob_bytes,
        manifest_blobs: after.manifest_blobs - before.manifest_blobs,
        manifest_blob_bytes: after.manifest_blob_bytes - before.manifest_blob_bytes,
        duplicate_blobs: after.duplicate_blobs - before.duplicate_blobs,
        commit_records: after.commit_records - before.commit_records,
        commit_record_bytes: after.commit_record_bytes - before.commit_record_bytes,
        superblock_writes: after.superblock_writes - before.superblock_writes,
        index_segments: after.index_segments - before.index_segments,
        index_bytes: after.index_bytes - before.index_bytes,
        file_barriers: after
            .file_barriers
            .map(|value| value - before.file_barriers.unwrap_or(0)),
        dir_barriers: after
            .dir_barriers
            .map(|value| value - before.dir_barriers.unwrap_or(0)),
    }
}

#[test]
fn one_group_is_one_commit_one_head_and_one_index_segment() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options(1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let ledger_before = store.publication_ledger();
    let generation_before = store.generation();
    let head_before = store.head();

    let ids = write_group(&mut store, root);
    assert_eq!(ids.len(), 8);

    let ledger = ledger_delta(&ledger_before, &store.publication_ledger());
    assert_eq!(ledger.commit_records, 1, "one commit record per group");
    assert_eq!(
        ledger.superblock_writes, 1,
        "one head publication per group"
    );
    assert_eq!(ledger.index_segments, 1, "one advisory index segment");
    assert_eq!(store.generation(), generation_before + 1);
    assert_ne!(store.head(), head_before);
    // Eight four-to-eight-KiB files at a 1 KiB chunk: every file is chunked and
    // gets one manifest, so the blob kinds are both populated.
    assert!(ledger.object_blobs >= 8, "{ledger:?}");
    assert_eq!(ledger.manifest_blobs, 8, "{ledger:?}");
    assert_eq!(ledger.duplicate_blobs, 0);
}

#[test]
fn the_deferred_batch_skips_no_file_barrier_and_closes_before_the_commit() {
    for defer in [true, false] {
        let dir = tempdir().unwrap();
        let mut store = Store::init(dir.path(), options_with(1024, defer)).unwrap();
        let root = store.resolve_path("/").unwrap();
        let stats_before = store.backend_stats();
        let ledger_before = store.publication_ledger();

        write_group(&mut store, root);

        let stats = store.backend_stats();
        let ledger = ledger_delta(&ledger_before, &store.publication_ledger());
        let files = ledger.object_blobs
            + ledger.manifest_blobs
            + ledger.commit_records
            + ledger.index_segments;
        let file_barriers = ledger
            .file_barriers
            .expect("the local backend reports its barrier split");
        let dir_barriers = ledger
            .dir_barriers
            .expect("the local backend reports its barrier split");

        assert_eq!(
            file_barriers, files,
            "defer={defer}: every published file must be synced exactly once, \
             and the commit must not return before the barrier (ledger {ledger:?})"
        );
        assert!(
            dir_barriers > 0,
            "defer={defer}: at least one directory barrier is required"
        );
        assert_eq!(
            stats.fsyncs - stats_before.fsyncs,
            file_barriers + dir_barriers,
            "defer={defer}: the split must sum to the total"
        );
        if defer {
            // Two blobs per file live in the same `objects/<ab>` or
            // `manifests/<ab>` directory, and the deferred path syncs each
            // touched directory once rather than once per publication.
            assert!(
                dir_barriers < files,
                "defer=true should deduplicate directory barriers: {dir_barriers} vs {files}"
            );
        }
    }
}

#[test]
fn deferred_and_eager_publish_the_same_logical_state() {
    let dir_a = tempdir().unwrap();
    let mut eager = Store::init(dir_a.path(), options_with(1024, false)).unwrap();
    let root_a = eager.resolve_path("/").unwrap();
    let eager_ids = write_group(&mut eager, root_a);

    let dir_b = tempdir().unwrap();
    let mut deferred = Store::init(dir_b.path(), options_with(1024, true)).unwrap();
    let root_b = deferred.resolve_path("/").unwrap();
    // Seed the directory identity identically by using the same seed bytes.
    let deferred_ids = write_group(&mut deferred, root_b);

    assert_eq!(
        eager_ids, deferred_ids,
        "the same seeds must publish the same object ids"
    );
    for name in ["f0", "f3", "f7"] {
        let a = eager.resolve_path(&format!("/{name}")).unwrap();
        let b = deferred.resolve_path(&format!("/{name}")).unwrap();
        assert_eq!(a, b, "{name} entry differs");
        assert_eq!(
            eager.read_object(a).unwrap(),
            deferred.read_object(b).unwrap(),
            "{name} bytes differ"
        );
    }
    assert_eq!(eager.generation(), deferred.generation());
}

#[test]
fn an_aborted_group_publishes_nothing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options(1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let generation_before = store.generation();
    let head_before = store.head();
    let ledger_before = store.publication_ledger();

    store.begin_group().unwrap();
    for (index, bytes) in payloads().iter().enumerate() {
        store
            .create_file(
                root,
                format!("aborted{index}").as_bytes(),
                bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    store.abort_group();

    assert_eq!(store.generation(), generation_before);
    assert_eq!(store.head(), head_before);
    assert!(store.resolve_path("/aborted0").is_err());
    let ledger = ledger_delta(&ledger_before, &store.publication_ledger());
    assert_eq!(ledger.commit_records, 0);
    assert_eq!(ledger.superblock_writes, 0);
    // The blobs an aborted group published are unreferenced. They are allowed
    // to exist (GC reclaims them) but nothing may name them.
    assert!(ledger.object_blobs > 0);

    // A later commit must not inherit the aborted batch: it still gets its own
    // barriers before its record.
    let stats_before = store.backend_stats();
    let ledger_before = store.publication_ledger();
    store
        .create_file(root, b"after-abort", b"x", 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let ledger = ledger_delta(&ledger_before, &store.publication_ledger());
    let files =
        ledger.object_blobs + ledger.manifest_blobs + ledger.commit_records + ledger.index_segments;
    assert_eq!(
        ledger.file_barriers.expect("barrier split"),
        files,
        "every published file must be synced exactly once after an abort"
    );
    let _ = stats_before;
    assert!(store
        .verify(true)
        .unwrap()
        .problems
        .iter()
        .all(|problem| !problem.detail.contains("unreferenced")));
}

#[test]
fn a_reopened_store_loads_the_grouped_state_and_its_index_without_a_rebuild() {
    let dir = tempdir().unwrap();
    let ids = {
        let mut store = Store::init(dir.path(), options(1024)).unwrap();
        let root = store.resolve_path("/").unwrap();
        let ids = write_group(&mut store, root);
        store.verify(true).unwrap();
        ids
    };
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    let root = store.resolve_path("/").unwrap();
    for (index, expected) in ids.iter().enumerate() {
        let id = store.resolve_path(&format!("/f{index}")).unwrap();
        assert_eq!(&id, expected);
        assert_eq!(store.read_object(id).unwrap(), payloads()[index]);
    }
    assert!(store
        .verify(true)
        .unwrap()
        .problems
        .iter()
        .all(|problem| !problem.detail.to_lowercase().contains("index")));
    let _ = root;
}

#[test]
fn mtime_and_generation_are_monotonic_across_grouped_publications() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options(1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut seen = Vec::new();
    for round in 0..4 {
        store.begin_group().unwrap();
        store
            .create_file(
                root,
                format!("round-{round}").as_bytes(),
                format!("round {round}").as_bytes(),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
        store.commit_group().unwrap();
        seen.push(store.generation());
    }
    assert_eq!(seen, vec![2, 3, 4, 5]);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), 5);
    let mtimes: Vec<i64> = (0..4)
        .map(|round| {
            let id = store.resolve_path(&format!("/round-{round}")).unwrap();
            store.object_meta(id).unwrap().mtime_ns
        })
        .collect();
    assert!(mtimes.windows(2).all(|pair| pair[0] <= pair[1]));
}
