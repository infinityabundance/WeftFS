//! R12c item 2: operation-derived incremental index maintenance.
//!
//! The writer derives the next advisory index from the previous index and the
//! committed operation stream. `StateIndex::build` stays the oracle: these tests
//! compare the serialized candidate sections (not just query answers) after a
//! randomized operation sequence, and prove that a damaged segment is rejected
//! and rebuilt rather than trusted.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 128,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn queries() -> Vec<Selector> {
    [
        "select true",
        "select attrs.team == \"red\"",
        "select attrs.team == \"blue\"",
        "select has(attrs.rev)",
        "select tag(\"alpha\")",
        "select tag(\"beta\")",
        "select not has(attrs.team)",
    ]
    .iter()
    .map(|text| selector::parse(text).unwrap())
    .collect()
}

/// The oracle gate: the incrementally maintained index must encode exactly like
/// a full rebuild, and every selector view must agree with the scan oracle.
fn assert_oracle_and_queries(store: &Store, label: &str) {
    let incremental = store
        .head_index_sections_for_test()
        .unwrap_or_else(|err| panic!("{label}: no maintained index: {err}"));
    let rebuilt = store.rebuilt_index_sections_for_test().unwrap();
    assert_eq!(
        incremental, rebuilt,
        "{label}: the maintained index differs byte-for-byte from a full rebuild"
    );
    for query in queries() {
        let limits = weftcodec::Limits::default();
        let indexed = store
            .resolve(&query, None, &principal(), &limits)
            .unwrap_or_else(|err| panic!("{label}: indexed resolve: {err}"));
        let scan = store
            .resolve_scan(&query, None, &principal(), &limits)
            .unwrap_or_else(|err| panic!("{label}: scan resolve: {err}"));
        assert_eq!(
            indexed.matched_count, scan.matched_count,
            "{label}: indexed and scan counts differ"
        );
        let mut a: Vec<_> = indexed.results.iter().map(|item| item.object_id).collect();
        let mut b: Vec<_> = scan.results.iter().map(|item| item.object_id).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{label}: indexed and scan ids differ");
    }
}

/// xorshift64* — deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

struct Live {
    id: Hash32,
    name: Vec<u8>,
}

fn create(store: &mut Store, root: Hash32, index: u64, rng: &mut Rng) -> Live {
    let teams = ["red", "blue", "green"];
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "team".to_owned(),
        Value::Str(teams[rng.below(teams.len())].to_owned()),
    );
    attrs.insert("score".to_owned(), Value::U64(rng.below(16) as u64));
    let name = format!("f{index}").into_bytes();
    let id = store
        .create_file_seeded(
            root,
            &name,
            format!("body-{index}").as_bytes(),
            0o644,
            0,
            0,
            attrs,
            format!("incremental-seed-{index}").as_bytes(),
        )
        .expect("create");
    Live { id, name }
}

fn run_sequence(seed: u64, operations: usize) -> (Store, tempfile::TempDir, u64) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut rng = Rng::new(seed);
    let mut live: Vec<Live> = Vec::new();
    let mut relations: Vec<Hash32> = Vec::new();
    let mut oracle_builds = 0u64;
    for index in 0..8u64 {
        live.push(create(&mut store, root, index, &mut rng));
    }
    let tags = ["alpha", "beta"];
    let mut next_name = 8u64;

    for step in 0..operations {
        match rng.below(100) {
            0..=27 if !live.is_empty() => {
                let target = live[rng.below(live.len())].id;
                store
                    .set_xattr(target, "rev", &(step as u64 + 1).to_le_bytes())
                    .expect("rev");
            }
            28..=37 if !live.is_empty() => {
                let target = live[rng.below(live.len())].id;
                let tag = tags[rng.below(tags.len())];
                let id = store
                    .relation_add(
                        target,
                        "tagged_as",
                        RelationTarget::Value(Value::Str(tag.to_owned())),
                        None,
                        BTreeMap::new(),
                    )
                    .expect("tag");
                relations.push(id);
            }
            38..=47 if !relations.is_empty() => {
                let index = rng.below(relations.len());
                let id = relations.remove(index);
                // The subject may have been unlinked (which cascades its
                // relations); a stale id is simply skipped.
                let _ = store.relation_delete(id);
            }
            48..=57 if live.len() >= 2 => {
                let a = live[rng.below(live.len())].id;
                let b = live[rng.below(live.len())].id;
                if a != b {
                    if let Ok(id) = store.relation_add(
                        a,
                        "links",
                        RelationTarget::Object(b),
                        None,
                        BTreeMap::new(),
                    ) {
                        relations.push(id);
                    }
                }
            }
            58..=67 if live.len() < 48 => {
                let created = create(&mut store, root, next_name, &mut rng);
                next_name += 1;
                live.push(created);
            }
            68..=77 if live.len() > 4 => {
                let index = rng.below(live.len());
                let entry = live.remove(index);
                store.unlink(root, &entry.name).expect("unlink");
            }
            78..=87 if !live.is_empty() => {
                let index = rng.below(live.len());
                let old = live[index].name.clone();
                let new = format!("r{next_name}").into_bytes();
                next_name += 1;
                if store.rename(root, &old, root, &new).is_ok() {
                    live[index].name = new;
                }
            }
            _ if !live.is_empty() => {
                // An unrelated metadata edit: the object record changes, the
                // indexed attributes do not.
                let target = live[rng.below(live.len())].id;
                store
                    .setattr(
                        target,
                        weftfs::store::SetAttr {
                            mode: Some(0o600),
                            ..weftfs::store::SetAttr::default()
                        },
                    )
                    .expect("setattr");
            }
            _ => {}
        }
        if step % 500 == 499 {
            assert_oracle_and_queries(&store, &format!("seed {seed} step {step}"));
            oracle_builds += 1;
        }
    }
    assert_oracle_and_queries(&store, &format!("seed {seed} final"));
    oracle_builds += 1;
    (store, dir, oracle_builds)
}

#[test]
fn randomized_sequences_match_the_full_rebuild_oracle() {
    let mut totals = (0u64, 0u64);
    for seed in [1u64, 7, 2026] {
        let (store, _dir, oracle_builds) = run_sequence(seed, 10_000);
        let stats = store.index_stats();
        assert!(
            stats.maintenance_updates + 2 >= stats.writes,
            "seed {seed}: every non-genesis attested commit must be derived: {stats:?}"
        );
        assert_eq!(
            stats.maintenance_fallbacks, 0,
            "seed {seed}: no maintenance step may fall back: {stats:?}"
        );
        // The only `StateIndex::build` calls are the genesis commit and this
        // test's own oracle comparisons of the replayed state.
        assert_eq!(
            stats.builds,
            oracle_builds + 1,
            "seed {seed}: the writer itself must not rebuild: {stats:?}"
        );
        totals.0 += stats.maintenance_updates;
        totals.1 += stats.maintenance_fallbacks;
    }
    assert_eq!(totals.1, 0);
}

#[test]
fn group_commits_are_maintained_incrementally() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut rng = Rng::new(99);
    let mut live = Vec::new();
    for index in 0..6u64 {
        live.push(create(&mut store, root, index, &mut rng));
    }
    // One group commit with several index-relevant ops on the same object.
    store
        .group(|group| {
            for _ in 0..3 {
                group.set_xattr(live[0].id, "rev", &7u64.to_le_bytes())?;
                group.set_xattr(live[1].id, "team", b"red")?;
            }
            group.relation_add(
                live[2].id,
                "tagged_as",
                RelationTarget::Value(Value::Str("alpha".to_owned())),
                None,
                BTreeMap::new(),
            )?;
            group.relation_add(
                live[3].id,
                "links",
                RelationTarget::Object(live[4].id),
                None,
                BTreeMap::new(),
            )?;
            Ok(())
        })
        .expect("group");
    assert_oracle_and_queries(&store, "after group");
    let stats = store.index_stats();
    assert!(stats.maintenance_updates >= 1, "{stats:?}");
    assert_eq!(stats.maintenance_fallbacks, 0, "{stats:?}");
}

#[test]
fn a_stale_or_damaged_segment_is_rejected_and_rebuilt() {
    #[derive(Debug)]
    enum Damage {
        DeleteHead,
        TruncateHead,
        FlipBodyByte,
        OrphanUnderNoCommit,
    }
    for damage in [
        Damage::DeleteHead,
        Damage::TruncateHead,
        Damage::FlipBodyByte,
        Damage::OrphanUnderNoCommit,
    ] {
        let dir = tempdir().unwrap();
        let mut store = Store::init(dir.path(), options()).unwrap();
        let root = store.resolve_path("/").unwrap();
        let mut rng = Rng::new(11);
        let mut live = Vec::new();
        for index in 0..20u64 {
            live.push(create(&mut store, root, index, &mut rng));
        }
        for (index, entry) in live.iter().enumerate() {
            store
                .set_xattr(entry.id, "rev", &(index as u64).to_le_bytes())
                .unwrap();
        }
        let head = store.head();
        store.snapshot_create("pinned").unwrap();
        drop(store);
        let path = index_path(dir.path(), head);
        assert!(path.exists(), "a commit writes its advisory segment");
        match damage {
            Damage::DeleteHead => fs::remove_file(&path).unwrap(),
            Damage::TruncateHead => {
                let bytes = fs::read(&path).unwrap();
                fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
            }
            Damage::FlipBodyByte => {
                let mut bytes = fs::read(&path).unwrap();
                let position = bytes.len() / 2;
                bytes[position] ^= 0xff;
                fs::write(&path, bytes).unwrap();
            }
            Damage::OrphanUnderNoCommit => {
                // A segment keyed by a commit id that the chain does not name is
                // unreferenced: it must never be read.
                let mut orphan = path.clone();
                let name = format!("{}.idx", "ff".repeat(32));
                orphan.set_file_name(name);
                fs::create_dir_all(orphan.parent().unwrap()).unwrap();
                fs::copy(&path, &orphan).unwrap();
            }
        }

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        store.reset_index_stats();
        assert_answers_equal_to_scan(&store, &format!("{damage:?}"));
        let stats = store.index_stats();
        match damage {
            Damage::OrphanUnderNoCommit => {
                // The head is intact; an extra unreferenced file changes nothing.
                assert!(stats.disk_loads >= 1, "{stats:?}");
                assert_eq!(stats.rejects, 0, "{stats:?}");
                assert_eq!(stats.builds, 0, "{stats:?}");
            }
            Damage::DeleteHead | Damage::TruncateHead | Damage::FlipBodyByte => {
                assert_eq!(
                    stats.disk_loads, 0,
                    "{damage:?}: a damaged head must not be used: {stats:?}"
                );
                assert!(
                    stats.builds >= 1,
                    "{damage:?}: the index must be rebuilt: {stats:?}"
                );
                if matches!(damage, Damage::TruncateHead | Damage::FlipBodyByte) {
                    assert!(
                        stats.rejects >= 1,
                        "{damage:?}: a damaged segment needs a reject note: {stats:?}"
                    );
                }
            }
        }
    }
}

fn assert_answers_equal_to_scan(store: &Store, label: &str) {
    let limits = weftcodec::Limits::default();
    for query in queries() {
        let indexed = store.resolve(&query, None, &principal(), &limits).unwrap();
        let scan = store
            .resolve_scan(&query, None, &principal(), &limits)
            .unwrap();
        assert_eq!(indexed.matched_count, scan.matched_count, "{label}");
    }
}

fn index_path(root: &Path, commit: Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}
