//! Fast-open courts: generation-addressed bounded replay.
//!
//! These tests compare `Store::open` (bounded, directory-addressed replay)
//! against the original full-scan `load_state_at` oracle and against
//! `Store::open_full_scan_for_bench`, which forces the pre-optimisation path.
//! They also prove the fast path is actually taken via the process-wide
//! `recovery::commit_file_reads()` counter, and that every structural
//! corruption either falls back to the authoritative full recovery (with a
//! note) or fails closed. No case may return a silently shorter state.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use tempfile::tempdir;

use weftcodec::wcb::Value as WcbValue;
use weftfs::identity::Hash32;
use weftfs::recovery;
use weftfs::state::{RelationTarget, State};
use weftfs::store::{OpenMode, Options, Store};

/// The read counter is process-global; counter assertions must not race.
static COUNTER_LOCK: Mutex<()> = Mutex::new(());

const INTERVAL: u64 = 128;

fn opts(interval: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: interval,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
    }
}

/// Build a deterministic store with exactly `generations` committed
/// generations over a small fixed file set, populating objects, entries,
/// relations, snapshots and tips.
fn build_store(dir: &Path, generations: u64, interval: u64) -> Vec<Hash32> {
    let mut store = Store::init(dir, opts(interval)).expect("init");
    let root = store.resolve_path("/").expect("root");
    let mut ids = Vec::new();
    for index in 0..8u64 {
        let id = store
            .create_file(
                root,
                format!("f{index}").as_bytes(),
                format!("seed body {index}").as_bytes(),
                0o644,
                0,
                0,
                Default::default(),
            )
            .expect("create file");
        ids.push(id);
    }
    store
        .relation_add(
            ids[0],
            "links",
            RelationTarget::Value(WcbValue::Str("target".to_owned())),
            None,
            Default::default(),
        )
        .expect("relation");
    store.snapshot_create("initial").expect("snapshot");

    while store.generation() < generations {
        let generation = store.generation();
        let index = (generation as usize) % ids.len();
        let body = format!("generation {generation} body {}", generation % 17);
        store
            .write_file(ids[index], 0, body.as_bytes())
            .expect("write file");
        if store.generation() == 400 {
            store.snapshot_create("mid").expect("snapshot 400");
        }
        if store.generation() == 1200 {
            store.snapshot_create("late").expect("snapshot 1200");
        }
    }
    assert_eq!(store.generation(), generations);
    ids
}

fn oracle(dir: &Path) -> (State, u64, Hash32, Vec<String>) {
    let view = recovery::read_superblock(dir).expect("superblock");
    let max_visible = view
        .slots
        .iter()
        .flatten()
        .map(|slot| slot.generation)
        .max()
        .expect("a valid slot");
    let loaded =
        recovery::load_commit_file(&recovery::commit_path(dir, max_visible), Some(max_visible))
            .expect("head commit");
    let (state, notes) =
        recovery::load_state_at(dir, loaded.hash, max_visible, true).expect("full replay oracle");
    (state, loaded.generation, loaded.hash, notes)
}

fn assert_states_equal(left: &State, right: &State, context: &str) {
    assert_eq!(left.generation, right.generation, "{context}: generation");
    assert_eq!(left.objects, right.objects, "{context}: objects");
    assert_eq!(left.entries, right.entries, "{context}: entries");
    assert_eq!(left.relations, right.relations, "{context}: relations");
    assert_eq!(left.snapshots, right.snapshots, "{context}: snapshots");
    assert_eq!(left.tips, right.tips, "{context}: tips");
}

fn copy_dir_all(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copy dir");
    for entry in fs::read_dir(source).expect("read source dir") {
        let entry = entry.expect("dir entry");
        let file_type = entry.file_type().expect("file type");
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

fn flip_last_byte(path: &Path) {
    let mut bytes = fs::read(path).expect("read corrupt target");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    fs::write(path, bytes).expect("write corrupt target");
}

#[test]
fn fast_open_matches_full_replay_and_reads_a_bounded_window() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempdir().expect("tempdir");
    build_store(dir.path(), 2_000, INTERVAL);

    let (oracle_state, oracle_generation, oracle_head, oracle_notes) = oracle(dir.path());
    assert!(oracle_notes.is_empty(), "oracle notes: {oracle_notes:?}");
    assert!(
        !oracle_state.snapshots.is_empty(),
        "snapshots must be populated"
    );
    assert!(!oracle_state.tips.is_empty(), "tips must be populated");
    assert!(
        !oracle_state.relations.is_empty(),
        "relations must be populated"
    );

    // ReadOnly: directory-free fast path.
    recovery::reset_commit_file_reads();
    let readonly = Store::open(dir.path(), OpenMode::ReadOnly).expect("read-only open");
    let readonly_reads = recovery::commit_file_reads();
    assert_eq!(readonly.generation(), oracle_generation);
    assert_eq!(readonly.snapshot_root(), oracle_head);
    assert_states_equal(
        readonly.current_state(),
        &oracle_state,
        "read-only fast open",
    );
    assert!(readonly.rejected_generations().is_empty());
    assert!(readonly.recovery_notes().is_empty());
    assert!(
        readonly_reads <= INTERVAL + 2,
        "read-only open decoded {readonly_reads} commit files; expected the head-to-checkpoint window"
    );
    drop(readonly);

    // ReadWrite: one directory enumeration is still needed to preserve
    // next_generation semantics, but commit decodes stay bounded.
    recovery::reset_commit_file_reads();
    let readwrite = Store::open(dir.path(), OpenMode::ReadWrite).expect("read-write open");
    let readwrite_reads = recovery::commit_file_reads();
    assert_eq!(readwrite.generation(), oracle_generation);
    assert_eq!(readwrite.snapshot_root(), oracle_head);
    assert_states_equal(
        readwrite.current_state(),
        &oracle_state,
        "read-write fast open",
    );
    assert!(readwrite.rejected_generations().is_empty());
    assert!(
        readwrite_reads <= INTERVAL + 2,
        "read-write open decoded {readwrite_reads} commit files; expected the head-to-checkpoint window"
    );
    drop(readwrite);

    // The forced old path is the full scan and produces the same state.
    recovery::reset_commit_file_reads();
    let full = Store::open_full_scan_for_bench(dir.path(), OpenMode::ReadOnly).expect("full open");
    let full_reads = recovery::commit_file_reads();
    assert!(
        full_reads >= 2_000,
        "the forced full path must decode every commit file, decoded {full_reads}"
    );
    assert_eq!(full.generation(), oracle_generation);
    assert_states_equal(full.current_state(), &oracle_state, "forced full open");
}

/// Open the same (possibly corrupted) store through the fast path and through
/// the forced full path, then require identical recovered semantics. The fast
/// path may carry extra "fell back" notes; after filtering those, the notes
/// must be identical too.
fn assert_fast_matches_full(dir: &Path) -> Store {
    let fast = Store::open(dir, OpenMode::ReadOnly).expect("fast open");
    let fast_state = fast.current_state().clone();
    let fast_head = fast.snapshot_root();
    let fast_generation = fast.generation();
    let fast_rejected = fast.rejected_generations().to_vec();
    let fast_notes = fast.recovery_notes().to_vec();
    drop(fast);

    let full = Store::open_full_scan_for_bench(dir, OpenMode::ReadOnly).expect("full open");
    assert_eq!(fast_generation, full.generation(), "generation diverged");
    assert_eq!(fast_head, full.snapshot_root(), "head diverged");
    assert_states_equal(&fast_state, full.current_state(), "recovered state");
    assert_eq!(
        fast_rejected,
        full.rejected_generations(),
        "rejected set diverged"
    );
    let filtered: Vec<String> = fast_notes
        .iter()
        .filter(|note| !note.contains("fell back"))
        .cloned()
        .collect();
    assert_eq!(
        filtered,
        full.recovery_notes(),
        "notes diverged beyond fallback notes"
    );
    assert!(
        fast_notes.iter().any(|note| note.contains("fell back")),
        "a fallback must be recorded: {fast_notes:?}"
    );
    full
}

#[test]
fn corrupted_chains_fall_back_to_the_authoritative_full_recovery() {
    // The read counter is process-global; keep every test in this binary from
    // opening a store concurrently with a counter assertion.
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempdir().expect("tempdir");
    build_store(dir.path(), 600, INTERVAL);
    let pristine = dir.path().to_path_buf();

    // (a) delete a mid-chain commit.
    let deleted = tempdir().expect("tempdir");
    copy_dir_all(&pristine, deleted.path());
    fs::remove_file(recovery::commit_path(deleted.path(), 550)).expect("delete mid-chain commit");
    let store = assert_fast_matches_full(deleted.path());
    assert_eq!(store.generation(), 549, "recovered head below the hole");
    assert!(store.rejected_generations().contains(&600));
    drop(store);

    // (b) corrupt a mid-chain commit's bytes (hash/CRC can no longer match).
    let corrupt = tempdir().expect("tempdir");
    copy_dir_all(&pristine, corrupt.path());
    flip_last_byte(&recovery::commit_path(corrupt.path(), 550));
    let store = assert_fast_matches_full(corrupt.path());
    assert_eq!(
        store.generation(),
        549,
        "recovered head below the corrupt commit"
    );
    assert!(
        store.rejected_generations().contains(&550),
        "the corrupt commit must be rejected: {:?}",
        store.rejected_generations()
    );
    drop(store);

    // (c) truncate the head commit file.
    let truncated = tempdir().expect("tempdir");
    copy_dir_all(&pristine, truncated.path());
    let head_path = recovery::commit_path(truncated.path(), 600);
    let bytes = fs::read(&head_path).expect("read head commit");
    fs::write(&head_path, &bytes[..bytes.len() / 2]).expect("truncate head commit");
    let store = assert_fast_matches_full(truncated.path());
    assert_eq!(store.generation(), 599);
    assert!(store.rejected_generations().contains(&600));
    drop(store);

    // (d) plant a commit whose declared generation disagrees with its name.
    let mislabelled = tempdir().expect("tempdir");
    copy_dir_all(&pristine, mislabelled.path());
    let source = recovery::commit_path(mislabelled.path(), 551);
    let target = recovery::commit_path(mislabelled.path(), 550);
    fs::copy(&source, &target).expect("plant mislabelled commit");
    let store = assert_fast_matches_full(mislabelled.path());
    assert_eq!(store.generation(), 549);
    assert!(
        store.rejected_generations().contains(&550),
        "the mislabelled commit must be rejected: {:?}",
        store.rejected_generations()
    );
}

#[test]
fn open_stops_at_the_nearest_checkpoint_and_falls_back_when_it_is_corrupt() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempdir().expect("tempdir");
    // With interval 1280 there is exactly one checkpoint below the 2,000-commit
    // head: generation 1280. The bounded window is therefore 721 commits,
    // still within `checkpoint_interval`.
    build_store(dir.path(), 2_000, 1_280);

    let checkpoint_commit =
        recovery::load_commit_file(&recovery::commit_path(dir.path(), 1_280), Some(1_280))
            .expect("checkpoint commit");
    let checkpoint = checkpoint_commit
        .commit
        .checkpoint
        .expect("generation 1280 carries a checkpoint");

    let (oracle_state, oracle_generation, oracle_head, oracle_notes) = oracle(dir.path());
    assert!(oracle_notes.is_empty());

    recovery::reset_commit_file_reads();
    let store = Store::open(dir.path(), OpenMode::ReadOnly).expect("fast open");
    let reads = recovery::commit_file_reads();
    assert_eq!(store.generation(), oracle_generation);
    assert_eq!(store.snapshot_root(), oracle_head);
    assert_states_equal(
        store.current_state(),
        &oracle_state,
        "checkpoint-bounded open",
    );
    assert!(
        reads <= 1_280 + 2,
        "open decoded {reads} commits; expected at most the checkpoint interval plus the head"
    );
    drop(store);

    // Corrupt the checkpoint manifest: the bounded path must not silently
    // accept the window. It falls back to full replay, which ignores the
    // invalid checkpoint and still produces the exact same state.
    flip_last_byte(&recovery::manifest_path(dir.path(), checkpoint));
    let store = Store::open(dir.path(), OpenMode::ReadOnly).expect("fallback open");
    assert_eq!(store.generation(), oracle_generation);
    assert_eq!(store.snapshot_root(), oracle_head);
    assert_states_equal(
        store.current_state(),
        &oracle_state,
        "corrupt-checkpoint fallback",
    );
    assert!(
        store
            .recovery_notes()
            .iter()
            .any(|note| note.contains("checkpoint")),
        "the ignored checkpoint must be reported: {:?}",
        store.recovery_notes()
    );
    assert!(
        store
            .recovery_notes()
            .iter()
            .any(|note| note.contains("fell back")),
        "the fallback must be reported: {:?}",
        store.recovery_notes()
    );
}
