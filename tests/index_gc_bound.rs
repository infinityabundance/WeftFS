//! Index GC chain bound: the depth bound is **per retained root**.
//!
//! `Store::expand_retained_index_chains` keeps the WFX3 ancestors that a
//! retained commit still names. Before this court the walk collected every
//! retained root into one `visited` set and compared that union against
//! `HARD_MAX_CHAIN_DELTAS` (256), so a store with more than ~8 retained
//! snapshots/checkpoints refused index GC entirely and the index tree grew
//! with the commit count.
//!
//! These tests pin the corrected invariant: each retained root's own walk must
//! reach a full segment within the hard bound; the bound is never the size of
//! the union. A genuinely over-long (or cyclic) chain still fails closed, and a
//! store with no retention reclaims exactly what it always did.
//!
//! See `bench/RESULTS-incremental-index.md` ("GC bound fix").

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::selector;
use weftfs::auth::Principal;
use weftfs::identity::{h, Hash32};
use weftfs::store::{OpenMode, Options, Store};

/// Writer default: the deepest chain the shipped writer produces.
const WRITER_MAX_DELTAS: u32 = 32;
/// Hard reader/walk bound.
const HARD_MAX_DELTAS: u32 = 256;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        // No automatic state checkpoints unless a test asks for one, so the
        // retained set is exactly what the test creates.
        checkpoint_interval: 1_000_000,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn seed(store: &mut Store, live: usize) {
    let root = store.resolve_path("/").unwrap();
    store.begin_group().unwrap();
    for index in 0..live {
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            weftcodec::wcb::Value::U64((index % 4) as u64),
        );
        store
            .create_file_seeded(
                root,
                format!("f{index}").as_bytes(),
                format!("body-{index}").as_bytes(),
                0o644,
                0,
                0,
                attrs,
                format!("seed-{index}").as_bytes(),
            )
            .unwrap();
    }
    store.commit_group().unwrap();
}

fn commit_one(store: &mut Store, step: u64, live: usize) {
    let root = store.resolve_path("/").unwrap();
    let target = store
        .lookup(root, format!("f{}", step as usize % live).as_bytes())
        .unwrap();
    store.set_xattr(target, "rev", &step.to_le_bytes()).unwrap();
}

fn index_path(root: &Path, commit: Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}

fn segments(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root.join("indexes")) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        for file in fs::read_dir(entry.path()).unwrap().flatten() {
            if file.path().extension().is_some_and(|ext| ext == "idx") {
                out.push(file.path());
            }
        }
    }
    out.sort();
    out
}

/// Follow the real WFX3 chain of `head` on disk (kind at byte 8, base commit at
/// bytes 57..89). Returns the commit ids, head first.
fn chain(root: &Path, head: Hash32) -> Vec<Hash32> {
    let mut out = Vec::new();
    let mut current = head;
    loop {
        let bytes = fs::read(index_path(root, current)).unwrap();
        out.push(current);
        if bytes.len() < 90 || bytes[8] != 1 {
            return out;
        }
        current = Hash32::from_bytes(bytes[57..89].try_into().unwrap());
        assert!(
            out.len() <= HARD_MAX_DELTAS as usize + 2,
            "chain did not terminate"
        );
    }
}

fn answers(store: &Store, snapshot: Option<Hash32>) -> Vec<Hash32> {
    let query = selector::parse("select attrs.team == 2").unwrap();
    let limits = weftcodec::Limits::default();
    let principal = principal();
    let indexed = store
        .resolve(&query, snapshot, &principal, &limits)
        .unwrap();
    let scan = store
        .resolve_scan(&query, snapshot, &principal, &limits)
        .unwrap();
    let mut a: Vec<_> = indexed.results.iter().map(|item| item.object_id).collect();
    let mut b: Vec<_> = scan.results.iter().map(|item| item.object_id).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the loaded chain and the scan oracle disagree");
    a
}

/// Build a store that retains `roots` snapshots plus one forced checkpoint
/// commit. Each snapshot is pinned when the writer's chain is at the deepest
/// depth it permits, so the chains are long and do not overlap.
fn retained_store(roots: usize) -> (tempfile::TempDir, Store, Vec<(String, Hash32)>, Hash32) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    seed(&mut store, 8);
    let mut pinned = Vec::new();
    let mut steps = 0u64;
    let mut checkpoint = Hash32::ZERO;
    for index in 0..roots {
        let mut guard = 0;
        loop {
            let (kind, depth) = store.index_head_chain().unwrap();
            if kind == "delta" && depth >= u64::from(WRITER_MAX_DELTAS) {
                break;
            }
            commit_one(&mut store, steps, 8);
            steps += 1;
            guard += 1;
            assert!(guard <= 64, "the writer never reached the maximum depth");
        }
        let name = format!("s{index}");
        let commit = store.snapshot_create(&name).unwrap();
        pinned.push((name, commit));
        if index == 3 {
            checkpoint = store.checkpoint().unwrap();
        }
    }
    (dir, store, pinned, checkpoint)
}

#[test]
fn many_retained_roots_each_with_a_long_chain_are_reclaimed() {
    let (dir, mut store, pinned, checkpoint) = retained_store(10);
    let root = dir.path().to_path_buf();

    let head = store.head();
    let mut roots: Vec<Hash32> = pinned.iter().map(|(_, commit)| *commit).collect();
    roots.push(head);
    roots.push(checkpoint);
    roots.sort();
    roots.dedup();
    assert!(
        roots.len() >= 8,
        "the reproduction needs several retained roots"
    );

    // The union of the retained chains must be well past the hard bound; that
    // is exactly what the pre-fix code measured against.
    let mut union: BTreeSet<Hash32> = BTreeSet::new();
    for committed in &roots {
        let walked = chain(&root, *committed);
        assert!(
            walked.len() as u64 <= u64::from(HARD_MAX_DELTAS) + 1,
            "one root must stay inside the bound: {} segments",
            walked.len()
        );
        union.extend(walked);
    }
    assert!(
        union.len() > HARD_MAX_DELTAS as usize,
        "the fixture must exceed the union bound: {} segments",
        union.len()
    );

    let before = segments(&root).len();
    let bytes_before = store.index_bytes_total().unwrap();
    let report = store.gc(true).unwrap();
    assert!(
        report.problems.is_empty(),
        "GC must resolve every per-root chain: {:?}",
        report.problems
    );
    assert!(
        report.deleted_index_bytes > 0,
        "GC must reclaim index segments it no longer needs: {report:?}"
    );
    let after = segments(&root).len();
    assert!(
        after < before,
        "the index tree must shrink: {before} -> {after}"
    );
    assert!(
        store.index_bytes_total().unwrap() < bytes_before,
        "reclaimed bytes must leave the tree smaller"
    );

    // Reachability safety: every retained root keeps its whole chain, loads it
    // from disk (never rebuilds), and answers exactly like the scan oracle.
    for committed in &roots {
        let walked = chain(&root, *committed);
        for segment in &walked {
            assert!(
                index_path(&root, *segment).exists(),
                "a retained chain lost segment {segment:?}"
            );
        }
    }
    drop(store);

    for (name, commit) in &pinned {
        let cold = Store::open(&root, OpenMode::ReadOnly).unwrap();
        cold.reset_index_stats();
        answers(&cold, Some(*commit));
        let stats = cold.index_stats();
        assert!(stats.disk_loads >= 1, "{name}: chain must load: {stats:?}");
        assert_eq!(stats.builds, 0, "{name}: nothing to rebuild: {stats:?}");
        assert_eq!(stats.rejects, 0, "{name}: {stats:?}");
    }
    let cold = Store::open(&root, OpenMode::ReadOnly).unwrap();
    cold.reset_index_stats();
    answers(&cold, Some(checkpoint));
    let stats = cold.index_stats();
    assert!(stats.disk_loads >= 1, "checkpoint chain: {stats:?}");
    assert_eq!(stats.builds, 0, "checkpoint chain: {stats:?}");

    // And the whole store still verifies with every representation decoded.
    let verify = cold.verify(true).unwrap();
    assert!(
        verify.problems.is_empty(),
        "verify --full: {:?}",
        verify.problems
    );
    assert!(verify.full);
}

#[test]
fn a_single_over_long_chain_still_refuses_and_deletes_nothing() {
    // The shipped writer never builds a chain deeper than 32 deltas and the
    // commit attestation refuses anything deeper than the hard bound, so this
    // chain is forged from real segment bytes: it is the only way to exercise
    // the fail-closed path with a genuine over-long chain.
    let source_dir = tempdir().unwrap();
    let mut source_options = options();
    source_options.index_chain_max_deltas = 1;
    let mut source = Store::init(source_dir.path(), source_options).unwrap();
    seed(&mut source, 4);
    commit_one(&mut source, 0, 4);
    commit_one(&mut source, 1, 4);
    commit_one(&mut source, 2, 4);
    let mut delta_bytes = None;
    let mut full_bytes = None;
    for path in segments(source_dir.path()) {
        let bytes = fs::read(&path).unwrap();
        match bytes[8] {
            1 if delta_bytes.is_none() => delta_bytes = Some(bytes),
            0 if full_bytes.is_none() => full_bytes = Some(bytes),
            _ => {}
        }
    }
    let delta_bytes = delta_bytes.expect("a delta segment");
    let full_bytes = full_bytes.expect("a full segment");
    drop(source);

    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    seed(&mut store, 4);
    commit_one(&mut store, 7, 4);
    let root = dir.path().to_path_buf();
    let head = store.head();

    // A linear chain of 300 deltas (the bound is 256) ending in a full segment.
    let ids: Vec<Hash32> = (0..=HARD_MAX_DELTAS + 43)
        .map(|index| h("weft/test/gc-chain", format!("{index}").as_bytes()))
        .collect();
    let mut rewritten = 0u64;
    for (position, id) in ids.iter().enumerate() {
        let payload = if position + 1 == ids.len() {
            full_bytes.clone()
        } else {
            let mut bytes = delta_bytes.clone();
            bytes[57..89].copy_from_slice(ids[position + 1].as_bytes());
            let body = bytes.len() - 32;
            let digest = h("weft/index/segment/v3", &bytes[..body]);
            bytes[body..].copy_from_slice(digest.as_bytes());
            rewritten += 1;
            bytes
        };
        let path = index_path(&root, *id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, payload).unwrap();
    }
    assert_eq!(rewritten, u64::from(HARD_MAX_DELTAS) + 43);
    // The retained root now names that chain.
    fs::write(
        index_path(&root, head),
        fs::read(index_path(&root, ids[0])).unwrap(),
    )
    .unwrap();

    let indexes_before = segments(&root).len();
    let report = store.gc(true).unwrap();
    assert!(report.deleted_indexes.is_empty(), "nothing may be deleted");
    assert_eq!(segments(&root).len(), indexes_before);
    let refusal = report
        .problems
        .iter()
        .find(|note| note.contains("index GC skipped"))
        .unwrap_or_else(|| panic!("GC must refuse: {report:?}"));
    assert!(
        refusal.contains("LimitExceeded: index segment chain exceeds the hard depth bound"),
        "the refusal must keep the typed class and text: {refusal}"
    );
}

#[test]
fn a_chain_without_retention_reclaims_exactly_the_unnamed_segments() {
    // No snapshot and no checkpoint commit: one retained root, so the answer
    // must be identical to the pre-fix behaviour (the shared set was that one
    // root's chain).
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    seed(&mut store, 8);
    for step in 0..120 {
        commit_one(&mut store, step, 8);
    }
    let root = dir.path().to_path_buf();
    let head = store.head();
    let retained_chain: BTreeSet<Hash32> = chain(&root, head).into_iter().collect();

    let before: Vec<PathBuf> = segments(&root);
    let before_bytes: u64 = before
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    let expected_deleted: u64 = before
        .iter()
        .filter(|path| {
            let hex = path.file_stem().unwrap().to_string_lossy().to_string();
            !retained_chain.contains(&Hash32::from_hex(&hex).unwrap())
        })
        .count() as u64;
    let expected_bytes: u64 = before
        .iter()
        .filter(|path| {
            let hex = path.file_stem().unwrap().to_string_lossy().to_string();
            !retained_chain.contains(&Hash32::from_hex(&hex).unwrap())
        })
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    assert!(expected_deleted > 0, "the fixture must have dead segments");

    let report = store.gc(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.deleted_indexes.len() as u64, expected_deleted);
    assert_eq!(report.deleted_index_bytes, expected_bytes);
    let after = segments(&root);
    assert_eq!(after.len() as u64, retained_chain.len() as u64);
    let after_bytes: u64 = after
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    assert_eq!(after_bytes, before_bytes - expected_bytes);

    drop(store);
    let cold = Store::open(&root, OpenMode::ReadOnly).unwrap();
    cold.reset_index_stats();
    answers(&cold, None);
    let stats = cold.index_stats();
    assert!(stats.disk_loads >= 1, "{stats:?}");
    assert_eq!(stats.builds, 0, "{stats:?}");
    assert!(cold.verify(true).unwrap().problems.is_empty());
}
