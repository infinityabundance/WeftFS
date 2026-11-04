//! Corruption-recovery and checkpoint tests. These corrupt bytes on purpose
//! and assert recovery falls back or fails closed without panicking.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use weftfs::identity::Hash32;
use weftfs::recovery::{commit_path, manifest_path, object_path};
use weftfs::store::{OpenMode, Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: None,
    }
}

fn build(path: &Path) -> (u64, Vec<Hash32>) {
    let mut store = Store::init(path, opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut ids = Vec::new();
    for index in 0..3 {
        let name = format!("file-{index}");
        let body = format!("content for file {index} with unique bytes {index}{index}{index}");
        ids.push(
            store
                .create_file(
                    root,
                    name.as_bytes(),
                    body.as_bytes(),
                    0o644,
                    0,
                    0,
                    Default::default(),
                )
                .unwrap(),
        );
    }
    let generation = store.generation();
    (generation, ids)
}

fn corrupt_flip(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(path, bytes).unwrap();
}

fn corrupt_slot(path: &Path, generation: u64) {
    let superblock = path.join("superblock");
    let mut bytes = fs::read(&superblock).unwrap();
    let offset = (generation % 2) as usize * 4096;
    for byte in &mut bytes[offset..offset + 4096] {
        *byte = 0;
    }
    fs::write(&superblock, bytes).unwrap();
}

fn assert_falls_back(path: &Path, head: u64) -> Store {
    let store = Store::open(path, OpenMode::ReadOnly).unwrap();
    assert!(
        store.generation() < head,
        "recovery should fall back below {head}, got {}",
        store.generation()
    );
    assert!(
        store.rejected_generations().contains(&head),
        "rejected set should name the torn head {head}: {:?}",
        store.rejected_generations()
    );
    store
}

#[test]
fn truncated_commit_file_falls_back() {
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    let path = commit_path(dir.path(), head);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let store = assert_falls_back(dir.path(), head);
    assert_eq!(store.resolve_path("/file-0").is_ok(), true);
    drop(store);
}

#[test]
fn commit_crc_flip_falls_back() {
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    corrupt_flip(&commit_path(dir.path(), head));
    let _store = assert_falls_back(dir.path(), head);
}

#[test]
fn corrupted_superblock_slot_falls_back_to_the_other_slot() {
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    corrupt_slot(dir.path(), head);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), head - 1);
}

#[test]
fn missing_chunk_falls_back_and_verify_reports_it() {
    let dir = tempdir().unwrap();
    let (head, ids) = build(dir.path());
    let writer = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    let record = writer.object_meta(*ids.last().unwrap()).unwrap();
    let manifest = weftfs::recovery::read_manifest(dir.path(), record.content).unwrap();
    let chunk = object_path(dir.path(), manifest.chunks[0].hash);
    drop(writer);
    fs::remove_file(&chunk).unwrap();

    let store = assert_falls_back(dir.path(), head);
    let report = store.verify(false).unwrap();
    assert!(!report.ok);
    assert!(!report.problems.is_empty());
}

#[test]
fn corrupt_manifest_falls_back_and_full_verify_reports() {
    let dir = tempdir().unwrap();
    let (head, ids) = build(dir.path());
    let writer = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    let record = writer.object_meta(*ids.last().unwrap()).unwrap();
    let manifest_file = manifest_path(dir.path(), record.content);
    drop(writer);
    corrupt_flip(&manifest_file);

    let store = assert_falls_back(dir.path(), head);
    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    assert!(!report.problems.is_empty());
}

#[test]
fn both_slots_zero_fails_closed() {
    let dir = tempdir().unwrap();
    let (_head, _) = build(dir.path());
    let superblock = dir.path().join("superblock");
    fs::write(&superblock, vec![0u8; 8192]).unwrap();
    let error = match Store::open(dir.path(), OpenMode::ReadOnly) {
        Ok(_) => panic!("open unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.class_name(), "Corrupt");
}

#[test]
fn checkpoint_survives_reopen_and_corrupt_checkpoint_is_ignored() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    for index in 0..3 {
        store
            .create_file(
                root,
                format!("f{index}").as_bytes(),
                format!("body {index}").as_bytes(),
                0o644,
                0,
                0,
                Default::default(),
            )
            .unwrap();
    }
    let checkpoint_head = store.checkpoint().unwrap();
    let generation = store.generation();
    drop(store);

    // Reopen with the checkpoint valid.
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), generation);
    assert_eq!(
        store
            .read_object(store.resolve_path("/f1").unwrap())
            .unwrap(),
        b"body 1"
    );
    drop(store);

    // Find and corrupt the checkpoint manifest.
    let commits = weftfs::recovery::all_commits(dir.path(), generation).unwrap();
    let checkpointed = commits
        .iter()
        .find(|commit| commit.commit.checkpoint.is_some())
        .expect("checkpoint commit exists");
    assert_eq!(checkpointed.hash, checkpoint_head);
    let checkpoint = checkpointed.commit.checkpoint.unwrap();
    let path = manifest_path(dir.path(), checkpoint);
    corrupt_flip(&path);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.generation(),
        generation,
        "checkpoint commit is still published"
    );
    assert!(
        store
            .recovery_notes()
            .iter()
            .any(|note| note.contains("checkpoint")),
        "recovery must report the ignored checkpoint: {:?}",
        store.recovery_notes()
    );
    assert_eq!(
        store
            .read_object(store.resolve_path("/f2").unwrap())
            .unwrap(),
        b"body 2"
    );
    let report = store.verify(false).unwrap();
    assert!(!report.ok, "verify must report the corrupt checkpoint");
}

#[test]
fn verify_reports_healthy_store_as_ok() {
    let dir = tempdir().unwrap();
    let (_head, _) = build(dir.path());
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    assert!(report.checked_objects >= 3);
    assert!(report.checked_chunks >= 3);
}

#[test]
fn rejected_generations_are_never_deleted() {
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    corrupt_flip(&commit_path(dir.path(), head));
    let _store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert!(
        commit_path(dir.path(), head).exists(),
        "recovery must not delete rejected data"
    );
}

#[test]
fn publication_crash_tail_above_superblock_is_not_visible() {
    // Simulate a crash after writing a commit file but before publishing the
    // superblock by copying a valid commit file to an unpublished generation.
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    let source = commit_path(dir.path(), head);
    let target = commit_path(dir.path(), head + 1);
    fs::copy(source, &target).unwrap();
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(
        store.generation(),
        head,
        "unpublished commit must not be selected"
    );
    let report = store.verify(false).unwrap();
    assert!(
        !report.ok,
        "verify should flag a commit file above the published superblock"
    );
}

#[test]
fn writer_after_rejected_generation_does_not_overwrite_it() {
    let dir = tempdir().unwrap();
    let (head, _) = build(dir.path());
    let torn = commit_path(dir.path(), head);
    let bytes = fs::read(&torn).unwrap();
    fs::write(&torn, &bytes[..bytes.len() / 2]).unwrap();
    let truncated_len = fs::metadata(&torn).unwrap().len();

    let mut store = Store::open(dir.path(), OpenMode::ReadWrite).unwrap();
    assert_eq!(store.generation(), head - 1);
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(
            root,
            b"after-crash",
            b"recovered writer",
            0o644,
            0,
            0,
            Default::default(),
        )
        .unwrap();
    assert_eq!(
        store.generation(),
        head + 1,
        "generation skips the rejected slot"
    );
    assert_eq!(
        fs::metadata(&torn).unwrap().len(),
        truncated_len,
        "the rejected commit file is never overwritten or deleted"
    );
    drop(store);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.generation(), head + 1);
    assert_eq!(
        store
            .read_object(store.resolve_path("/after-crash").unwrap())
            .unwrap(),
        b"recovered writer"
    );
}
