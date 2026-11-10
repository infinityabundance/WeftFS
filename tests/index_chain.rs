//! WFX3 incremental index segments: chain load, forgery, compaction, GC, and
//! bounds. See `docs/INDEX-AMPLIFICATION.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::selector;
use weftfs::auth::Principal;
use weftfs::store::{OpenMode, Options, Store};

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn write_files(store: &mut Store, count: usize, prefix: &str) {
    let root = store.resolve_path("/").unwrap();
    for index in 0..count {
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            weftcodec::wcb::Value::Str(if index % 2 == 0 { "red" } else { "blue" }.to_owned()),
        );
        attrs.insert(
            "score".to_owned(),
            weftcodec::wcb::Value::U64(index as u64 % 7),
        );
        let id = store
            .create_file_seeded(
                root,
                format!("{prefix}{index}").as_bytes(),
                format!("body-{prefix}-{index}").as_bytes(),
                0o644,
                0,
                0,
                attrs,
                format!("seed-{prefix}-{index}").as_bytes(),
            )
            .unwrap();
        if index % 3 == 0 {
            store
                .relation_add(
                    id,
                    "tagged_as",
                    weftfs::state::RelationTarget::Value(weftcodec::wcb::Value::Str(
                        "alpha".to_owned(),
                    )),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
    }
}

fn index_path(dir: &Path, commit: weftfs::identity::Hash32) -> std::path::PathBuf {
    let hex = commit.to_hex();
    dir.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn queries() -> Vec<weftcodec::selector::Selector> {
    [
        "select true",
        "select attrs.team == \"red\"",
        "select attrs.score > 3",
        "select tag(\"alpha\")",
        "select has(attrs.team) and not attrs.team == \"blue\"",
    ]
    .iter()
    .map(|text| selector::parse(text).unwrap())
    .collect()
}

fn assert_answers_equal(store: &Store, snapshot: Option<weftfs::identity::Hash32>, label: &str) {
    let principal = principal();
    let limits = weftcodec::Limits::default();
    for query in queries() {
        let got = store
            .resolve(&query, snapshot, &principal, &limits)
            .unwrap();
        let scan = store
            .resolve_scan(&query, snapshot, &principal, &limits)
            .unwrap();
        assert_eq!(
            got.matched_count, scan.matched_count,
            "{label}: matched count differs"
        );
        let mut a: Vec<_> = got.results.iter().map(|item| item.object_id).collect();
        let mut b: Vec<_> = scan.results.iter().map(|item| item.object_id).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{label}: object ids differ");
    }
}

#[test]
fn a_chain_load_answers_exactly_like_a_full_segment_store() {
    let chain_dir = tempdir().unwrap();
    let full_dir = tempdir().unwrap();
    let mut chain = Store::init(chain_dir.path(), options()).unwrap();
    let mut full_options = options();
    full_options.index_chain_max_deltas = 0;
    let mut full = Store::init(full_dir.path(), full_options).unwrap();
    write_files(&mut chain, 24, "f");
    write_files(&mut full, 24, "f");

    let (kind, depth) = chain.index_head_chain().unwrap();
    assert_eq!(kind, "delta");
    assert!(depth > 0, "the default writer must produce a delta chain");
    assert_eq!(full.index_head_chain().unwrap(), ("full".to_owned(), 0));

    drop(chain);
    drop(full);
    let chain = Store::open(chain_dir.path(), OpenMode::ReadOnly).unwrap();
    let full = Store::open(full_dir.path(), OpenMode::ReadOnly).unwrap();
    assert_answers_equal(&chain, None, "chain store");
    assert_answers_equal(&full, None, "full store");
    let stats = chain.index_stats();
    assert!(stats.disk_loads >= 1, "the chain must load: {stats:?}");
    assert_eq!(stats.builds, 0, "{stats:?}");
    assert_eq!(stats.rejects, 0, "{stats:?}");
    let full_stats = full.index_stats();
    assert!(full_stats.disk_loads >= 1, "{full_stats:?}");
    assert_eq!(full_stats.builds, 0, "{full_stats:?}");
}

#[test]
fn a_partial_chain_is_rejected_and_rebuilt() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    write_files(&mut store, 12, "f");
    let head = store.head();
    let (_, depth) = store.index_head_chain().unwrap();
    assert!(depth > 0);
    drop(store);

    // Keep the head segment and delete every other segment: the chain is now
    // partial. The loader must not use it.
    for entry in fs::read_dir(dir.path().join("indexes")).unwrap() {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        for file in fs::read_dir(entry.path()).unwrap() {
            let path = file.unwrap().path();
            let keep = path == index_path(dir.path(), head);
            if !keep {
                fs::remove_file(path).unwrap();
            }
        }
    }

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    store.reset_index_stats();
    assert_answers_equal(&store, None, "partial chain");
    let stats = store.index_stats();
    assert_eq!(
        stats.disk_loads, 0,
        "a partial chain must not load: {stats:?}"
    );
    assert!(stats.rejects >= 1, "{stats:?}");
    assert!(stats.builds >= 1, "{stats:?}");
}

#[test]
fn a_forged_chain_root_is_rejected() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    write_files(&mut store, 8, "f");
    let head = store.head();
    drop(store);
    let path = index_path(dir.path(), head);

    // Rewrite the base id to a different nonzero id and recompute the segment
    // digest. The segment is self-consistent but no longer matches the chain
    // root the commit attests.
    let mut bytes = fs::read(&path).unwrap();
    assert_eq!(&bytes[0..4], b"WFX3");
    assert_eq!(bytes[8], 1, "the head must be a delta for this test");
    bytes[25..57].fill(0x5a);
    let body = bytes.len() - 32;
    let digest = weftfs::identity::h("weft/index/segment/v3", &bytes[..body]);
    bytes[body..].copy_from_slice(digest.as_bytes());
    fs::write(&path, bytes).unwrap();

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    store.reset_index_stats();
    assert_answers_equal(&store, None, "forged chain");
    let stats = store.index_stats();
    assert_eq!(stats.disk_loads, 0, "{stats:?}");
    assert!(stats.rejects >= 1, "{stats:?}");
    assert!(
        stats
            .notes
            .iter()
            .any(|note| note.contains("chain") || note.contains("attestation")),
        "the reject must name the chain: {stats:?}"
    );
}

#[test]
fn compaction_writes_a_full_segment_and_gc_drops_the_chain() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    write_files(&mut store, 16, "f");
    let (_, depth) = store.index_head_chain().unwrap();
    assert!(depth > 0, "a chain must exist before compaction");
    let before = store.index_bytes_total().unwrap();
    let old_head = store.head();

    let new_head = store.compact_index().unwrap();
    assert_ne!(new_head, old_head);
    assert_eq!(store.index_head_chain().unwrap(), ("full".to_owned(), 0));
    assert_answers_equal(&store, None, "after compaction");

    let report = store.gc(true).unwrap();
    assert!(
        report.deleted_index_bytes > 0,
        "compaction must let GC drop the old chain: {report:?}"
    );
    assert!(!index_path(dir.path(), old_head).exists());
    assert!(index_path(dir.path(), new_head).exists());
    let after = store.index_bytes_total().unwrap();
    assert!(after < before, "compaction must not grow the index tree");
    assert_answers_equal(&store, None, "after compaction and GC");
}

#[test]
fn a_snapshot_pins_its_old_chain_across_gc() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    write_files(&mut store, 10, "f");
    let pinned = store.snapshot_create("pinned").unwrap();
    write_files(&mut store, 10, "g");
    let (_, depth) = store.index_head_chain().unwrap();
    assert!(depth > 0);

    let report = store.gc(true).unwrap();
    assert!(
        report.problems.is_empty(),
        "GC must resolve every retained chain: {report:?}"
    );
    assert!(
        index_path(dir.path(), pinned).exists(),
        "the snapshot's head segment must survive"
    );

    drop(store);
    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    store.reset_index_stats();
    assert_answers_equal(&store, Some(pinned), "pinned snapshot");
    let stats = store.index_stats();
    assert!(
        stats.disk_loads >= 1,
        "the pinned chain must still load: {stats:?}"
    );
    assert_eq!(stats.builds, 0, "{stats:?}");
}

#[test]
fn the_chain_depth_bound_and_byte_budget_are_enforced() {
    let dir = tempdir().unwrap();
    let mut bounded = options();
    bounded.index_chain_max_deltas = 2;
    let mut store = Store::init(dir.path(), bounded).unwrap();
    let mut seen_full = false;
    for index in 0..12 {
        write_files(&mut store, 1, &format!("f{index}-"));
        let (kind, depth) = store.index_head_chain().unwrap();
        assert!(depth <= 2, "depth {depth} exceeded the writer bound");
        seen_full |= kind == "full";
    }
    assert!(seen_full, "the bound must force at least one full segment");

    // A byte budget: once the tree reaches it, further segments are skipped and
    // the commit carries no attestation; queries rebuild.
    let budget_dir = tempdir().unwrap();
    let mut budget_options = options();
    budget_options.index_bytes_budget = Some(1);
    let mut budget = Store::init(budget_dir.path(), budget_options).unwrap();
    write_files(&mut budget, 4, "b");
    assert_eq!(budget.index_head_chain().unwrap(), ("none".to_owned(), 0));
    assert!(budget.index_stats().write_errors >= 1);
    assert_answers_equal(&budget, None, "budgeted store");
    assert!(budget.index_bytes_total().unwrap() <= 1);
}
