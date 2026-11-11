//! R13 immutable-read pool: the read path, its accounting, and its safety.
//!
//! The pool exists so independent readers of content-addressed blobs do not
//! serialise on the store's mutable-state mutex. These tests pin the contract:
//!
//! * a blob is either its complete published bytes or a failed hash check, even
//!   while a writer publishes unrelated blobs and commits;
//! * pooled reads are counted in `Store::backend_stats()`;
//! * immutable reads take no state-mutex hold at all;
//! * every mutation invalidates pooled handles (a stale packed catalog could
//!   otherwise miss a just-published member);
//! * the bytes are identical before and after the change.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::tempdir;
use weftfs::backend::BackendKind;
use weftfs::packs::PackOptions;
use weftfs::recovery::{manifest_path, object_path};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
        ..Options::default()
    }
}

fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add((index % 97) as u8))
        .collect()
}

fn create(store: &mut Store, name: &[u8], bytes: &[u8]) -> weftfs::Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, name, bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap()
}

#[test]
fn immutable_reads_do_not_take_the_state_mutex() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let mut ids = Vec::new();
    for index in 0..4u8 {
        let bytes = payload(index, 32 * 1024);
        ids.push((
            create(&mut writer, format!("f{index}").as_bytes(), &bytes),
            bytes,
        ));
    }
    drop(writer);

    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    store.set_lock_probe(true);
    store.reset_lock_probe();
    std::thread::scope(|scope| {
        for (id, expected) in &ids {
            let store = &store;
            scope.spawn(move || {
                for _ in 0..20 {
                    let bytes = store.read_object(*id).unwrap();
                    assert_eq!(&bytes, expected);
                }
            });
        }
    });
    let report = store.lock_probe_report();
    assert!(
        report.hold.count == 0,
        "immutable reads took {} state-mutex holds ({} ns held)",
        report.hold.count,
        report.hold.total_ns
    );
    assert_eq!(report.wait.count, 0);
    assert!(
        report.io.count > 0,
        "the blob reads were not observed by the read path"
    );
    let pool = store.read_pool_stats();
    assert!(pool.poolable);
    assert!(pool.max_in_flight > 1, "reads did not overlap");
    assert!(pool.reads > 0 && pool.bytes > 0);
    assert!(store.verify(true).unwrap().problems.is_empty());
}

#[test]
fn pooled_reads_are_counted_in_backend_stats() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(7, 64 * 1024);
    let id = create(&mut writer, b"counted", &bytes);
    drop(writer);

    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    let before = store.backend_stats();
    for _ in 0..5 {
        assert_eq!(store.read_object(id).unwrap(), bytes);
    }
    let after = store.backend_stats();
    let delta = after.bytes_read.saturating_sub(before.bytes_read);
    assert!(
        delta >= 5 * bytes.len() as u64,
        "backend bytes_read grew by {delta}, expected at least {}",
        5 * bytes.len()
    );
    assert!(after.reads.saturating_sub(before.reads) >= 5);
    let pool = store.read_pool_stats();
    assert!(pool.handles_created >= 1);
    assert!(pool.reads >= 5);
}

#[test]
fn a_mutation_invalidates_pooled_handles_and_new_content_is_visible() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let first = payload(1, 16 * 1024);
    let id = create(&mut store, b"live", &first);
    assert_eq!(store.read_object(id).unwrap(), first);
    let epoch_after_read = store.read_pool_stats().epoch;

    let second = payload(2, 16 * 1024);
    let id2 = create(&mut store, b"second", &second);
    assert_eq!(store.read_object(id2).unwrap(), second);

    let pool = store.read_pool_stats();
    assert!(
        pool.epoch > epoch_after_read,
        "a mutation did not invalidate the read pool (epoch {} -> {})",
        epoch_after_read,
        pool.epoch
    );
    assert_eq!(store.read_object(id).unwrap(), first);
}

#[test]
fn concurrent_readers_and_a_writer_never_see_a_torn_blob() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let frozen = payload(3, 96 * 1024);
    let frozen_id = create(&mut writer, b"frozen", &frozen);

    // A read-only handle snapshots the state at this point. Its immutables are
    // exactly what the writer is about to rewrite and extend.
    let reader_store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    let stop = AtomicBool::new(false);
    let misreads = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..4 {
            let store = &reader_store;
            let stop = &stop;
            let expected = &frozen;
            handles.push(scope.spawn(move || -> Result<(), String> {
                while !stop.load(Ordering::Relaxed) {
                    let bytes = store
                        .read_object(frozen_id)
                        .map_err(|err| format!("reader failed: {err}"))?;
                    if &bytes != expected {
                        return Err("reader observed different bytes".to_owned());
                    }
                }
                Ok(())
            }));
        }

        // The writer rewrites the frozen object (a new immutable version) and
        // publishes unrelated files; the readers keep their old snapshot.
        for round in 0..10u8 {
            let new_bytes = payload(100 + round, 96 * 1024);
            writer.write_file(frozen_id, 0, &new_bytes).unwrap();
            let extra = payload(200 + round, 8 * 1024);
            create(&mut writer, format!("new{round}").as_bytes(), &extra);
        }
        stop.store(true, Ordering::Relaxed);
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reader panicked"))
            .collect::<Result<Vec<_>, String>>()
    });
    assert!(misreads.is_ok(), "{misreads:?}");

    // The writer's own view and a freshly opened reader both see the newest
    // version, and the store verifies clean.
    assert!(writer.verify(true).unwrap().problems.is_empty());
    drop(writer);
    let fresh = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    let latest = fresh.read_object(frozen_id).unwrap();
    assert_eq!(latest, payload(109, 96 * 1024));
    assert!(fresh.verify(true).unwrap().problems.is_empty());
}

#[test]
fn a_flipped_manifest_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(9, 32 * 1024);
    let id = create(&mut store, b"torn", &bytes);
    assert_eq!(store.read_object(id).unwrap(), bytes);
    let record = store.object_meta(id).unwrap();

    let path = manifest_path(dir.path(), record.content);
    flip_byte(&path, 17);
    let err = store.read_object(id).unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt, "{err}");
    assert!(!store.verify(true).unwrap().problems.is_empty());

    // A fresh open must not silently accept the corrupt manifest: either the
    // commit is rejected (the object is absent) or the read fails closed.
    drop(store);
    let reopened = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    if reopened.object_meta(id).is_ok() {
        let err = reopened.read_object(id).unwrap_err();
        assert_eq!(err.class(), weftfs::Class::Corrupt, "{err}");
    }
    assert!(!reopened.verify(true).unwrap().problems.is_empty());
}

#[test]
fn a_flipped_chunk_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let bytes = payload(11, 32 * 1024);
    let id = create(&mut store, b"chunk", &bytes);
    assert_eq!(store.read_object(id).unwrap(), bytes);
    let record = store.object_meta(id).unwrap();
    let manifest = store.manifest_of(record.content).unwrap();
    let chunk = manifest.chunks[0].hash;

    let path = object_path(dir.path(), chunk);
    flip_byte(&path, 17);
    let err = store.read_object(id).unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Corrupt, "{err}");
}

fn flip_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut byte = [0u8; 1];
    std::io::Read::read_exact(&mut file, &mut byte).unwrap();
    byte[0] ^= 0x5a;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn a_pooled_handle_survives_concurrent_reads_without_mixing_bytes() {
    let dir = tempdir().unwrap();
    let mut writer = Store::init(dir.path(), opts()).unwrap();
    let mut subjects = Vec::new();
    for index in 0..8u8 {
        let bytes = payload(index, 64 * 1024);
        let id = create(&mut writer, format!("s{index}").as_bytes(), &bytes);
        subjects.push((id, bytes));
    }
    drop(writer);

    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, opts()).unwrap();
    std::thread::scope(|scope| {
        for (id, expected) in &subjects {
            let store = &store;
            scope.spawn(move || {
                for _ in 0..10 {
                    assert_eq!(&store.read_object(*id).unwrap(), expected);
                }
            });
        }
    });
    assert!(store.read_pool_stats().max_in_flight > 1);
    assert!(store.verify(true).unwrap().problems.is_empty());
}

/// A packed store with the frozen whole-file identity and 64 KiB chunks: every
/// chunk is at or below the default member threshold, so a create puts all of
/// them into the writer's open pack.
fn packed_store(dir: &Path) -> (Store, Options) {
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
        ..Options::default()
    };
    let kind = BackendKind::packed(BackendKind::Local, PackOptions::default()).unwrap();
    let store = Store::init_with_backend(dir.join("store"), options.clone(), kind).unwrap();
    (store, options)
}

#[test]
fn a_packed_write_reads_back_the_members_it_has_not_yet_catalogued() {
    // Regression, found by the aggregate court at R13 (`TestRustPacksGoReadsAnd
    // Verifies`): `store_file_ranges` stores the rewritten chunk and then
    // re-reads *every* chunk to feed the frozen whole-file ContentId. A packed
    // plane keeps that new chunk in its open pack until the catalog is sealed,
    // and a pooled read handle is a different plane instance, so it reported
    // `missing blob objects/..: does not exist`. Reads of a blob this handle
    // buffered must therefore use the writer's own handle.
    let dir = tempdir().unwrap();
    let (mut store, options) = packed_store(dir.path());
    let payload = payload(23, 280_000);
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"packed.bin", &payload, 0o644, 0, 0, BTreeMap::new())
        .unwrap();

    // The write this failed on: 17 bytes at 120000, inside the second chunk.
    let edit = b"packed-write-byte";
    let mut want = payload.clone();
    want[120_000..120_000 + edit.len()].copy_from_slice(edit);
    let content = store.write_file(id, 120_000, edit).unwrap();
    assert_eq!(store.read_object(id).unwrap(), want);

    // The read-back used this handle, not the pool, and said so.
    let stats = store.read_pool_stats();
    assert!(
        stats.buffered_blob_reads > 0,
        "the whole-file digest must read the buffered chunk through the writer: {stats:?}"
    );

    // Nothing is buffered once the commit flushed the pack catalog, so pooling
    // resumes: a later read is served by the pool.
    let reads_before = store.read_pool_stats().reads;
    assert_eq!(store.read_object(id).unwrap(), want);
    assert!(
        store.read_pool_stats().reads > reads_before,
        "reads after the flush must use the pool again: {:?}",
        store.read_pool_stats()
    );

    assert!(store.verify(true).unwrap().problems.is_empty());
    let head = store.head();
    drop(store);

    // A fresh read-only handle sees the same bytes and the same head.
    let reader = Store::open_with_options_and_backend(
        dir.path().join("store"),
        OpenMode::ReadOnly,
        options,
        BackendKind::packed(BackendKind::Local, PackOptions::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(reader.read_object(id).unwrap(), want);
    assert_eq!(reader.head(), head);
    assert!(reader.verify(true).unwrap().problems.is_empty());
    // `write_file` returns the new version id; the reopened handle sees it.
    assert_eq!(content, reader.object_meta(id).unwrap().version);
}
