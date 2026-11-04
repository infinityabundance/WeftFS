//! Differential court for the per-state selector index (`src/index.rs`).
//!
//! For each deterministic random state and selector, the indexed resolver
//! (`Store::resolve`) must be byte-for-byte identical to the index-free scan
//! oracle (`Store::resolve_scan`): `selector_id`, `snapshot_id`,
//! `matched_count`, the ordered result list, and evidence. The oracle shares
//! the resolver body with the indexed path (`evaluate::resolve_state_inner`),
//! so only candidate selection differs.

use std::collections::BTreeMap;

use tempfile::tempdir;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::evaluate::Resolution;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};

/// Small deterministic PRNG (splitmix-style); no external dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }
}

struct Fixture {
    store: Store,
    visible: Vec<Hash32>,
    hidden: Vec<Hash32>,
    files: Vec<Hash32>,
    _dir: tempfile::TempDir,
}

fn options(seed: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000 + seed as i64),
        ..Options::default()
    }
}

fn build_fixture(seed: u64) -> Fixture {
    let mut rng = Rng::new(seed);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options(seed)).unwrap();
    let root = store.resolve_path("/").unwrap();

    let teams = ["red", "green", "blue"];
    let tags = ["alpha", "beta", "gamma", "delta"];

    let mut parents = vec![root];
    for index in 0..3u64 {
        let id = store
            .mkdir_seeded(
                root,
                format!("dir{index}").as_bytes(),
                0o755,
                0,
                0,
                format!("dir-seed-{seed}-{index}").as_bytes(),
            )
            .unwrap();
        parents.push(id);
    }

    let mut files = Vec::new();
    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    for index in 0..36u64 {
        let parent = parents[(index as usize) % parents.len()];
        // Owner-only (root), world-readable, and hidden (uid 2000, 0600).
        let (mode, uid, gid, is_visible) = match index % 9 {
            0 | 4 => (0o600, 0, 0, true),
            7 => (0o600, 2000, 2000, false),
            _ => (0o644, 1000, 1000, true),
        };

        let mut attrs = BTreeMap::new();
        if rng.below(4) != 0 {
            attrs.insert(
                "team".to_owned(),
                Value::Str(teams[rng.below(3) as usize].to_owned()),
            );
        }
        if rng.below(3) != 0 {
            attrs.insert("score".to_owned(), Value::U64(rng.below(100)));
        }
        if rng.below(4) != 0 {
            attrs.insert("signed".to_owned(), Value::I64(rng.below(20) as i64));
        }
        if rng.below(5) != 0 {
            attrs.insert("epoch".to_owned(), Value::Timestamp(rng.below(20) as i64));
        }
        match rng.below(6) {
            0 => {
                attrs.insert("empty".to_owned(), Value::Array(Vec::new()));
            }
            1 => {
                attrs.insert("empty".to_owned(), Value::Map(BTreeMap::new()));
            }
            2 => {
                attrs.insert("empty".to_owned(), Value::Str(String::new()));
            }
            3 => {
                attrs.insert("empty".to_owned(), Value::Null);
            }
            _ => {}
        }
        if rng.below(2) == 0 {
            let mut nested = BTreeMap::new();
            nested.insert(
                "a".to_owned(),
                Value::Array(vec![Value::U64(1), Value::Str("x".to_owned())]),
            );
            attrs.insert("nested".to_owned(), Value::Map(nested));
        }
        if rng.below(3) == 0 {
            attrs.insert("media_type".to_owned(), Value::Str("text/plain".to_owned()));
        }

        let id = store
            .create_file_seeded(
                parent,
                format!("f{index}").as_bytes(),
                format!("payload-{index}").as_bytes(),
                mode,
                uid,
                gid,
                attrs,
                format!("file-seed-{seed}-{index}").as_bytes(),
            )
            .unwrap();
        files.push(id);
        if is_visible {
            visible.push(id);
        } else {
            hidden.push(id);
        }
    }

    // Tags: zero, one, or many per object. Hidden subjects are tagged too, to
    // prove candidate narrowing never exposes them.
    for (index, object) in files.iter().enumerate() {
        let count = (index as u64 + rng.below(3)) % 4;
        for _ in 0..count {
            let tag = tags[rng.below(tags.len() as u64) as usize];
            store
                .relation_add(
                    *object,
                    "tagged_as",
                    RelationTarget::Value(Value::Str(tag.to_owned())),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
    }

    // Object-target relations, including edges into hidden objects.
    for (index, object) in files.iter().enumerate() {
        if index % 2 == 0 {
            let target = files[rng.below(files.len() as u64) as usize];
            store
                .relation_add(
                    *object,
                    "links",
                    RelationTarget::Object(target),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
        if index % 3 == 0 {
            store
                .relation_add(
                    *object,
                    "points",
                    RelationTarget::Value(Value::U64(rng.below(10))),
                    None,
                    BTreeMap::new(),
                )
                .unwrap();
        }
    }
    if let Some(hidden_target) = hidden.first() {
        store
            .relation_add(
                visible[0],
                "links",
                RelationTarget::Object(*hidden_target),
                None,
                BTreeMap::new(),
            )
            .unwrap();
    }

    Fixture {
        store,
        visible,
        hidden,
        files,
        _dir: dir,
    }
}

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn queries(fixture: &Fixture) -> Vec<(String, Selector)> {
    let visible = fixture.visible[0];
    let hidden = fixture.hidden[0];
    let content = fixture.store.object_meta(visible).unwrap().content;
    let texts = vec![
        "select true".to_owned(),
        "select false".to_owned(),
        "select kind == \"file\"".to_owned(),
        "select kind == \"dir\"".to_owned(),
        "select has(attrs.team)".to_owned(),
        "select has(attrs.empty)".to_owned(),
        "select has(attrs.nested)".to_owned(),
        "select has(attrs.missing)".to_owned(),
        "select not has(attrs.team)".to_owned(),
        "select has(attrs.missing) or has(attrs.nested)".to_owned(),
        "select attrs.team == \"red\"".to_owned(),
        "select attrs.team != \"red\"".to_owned(),
        "select attrs.team == \"nope\"".to_owned(),
        "select attrs.team == null".to_owned(),
        "select attrs.missing == null".to_owned(),
        "select attrs.empty == \"\"".to_owned(),
        "select attrs.empty == null".to_owned(),
        "select attrs.nested == null".to_owned(),
        "select attrs.score > 50".to_owned(),
        "select attrs.score <= 25".to_owned(),
        "select attrs.score == 7".to_owned(),
        "select attrs.signed == 3".to_owned(),
        "select attrs.signed < 5".to_owned(),
        "select attrs.epoch == 5".to_owned(),
        "select attrs.epoch > 10".to_owned(),
        "select size > 5".to_owned(),
        "select size == 7".to_owned(),
        "select namespace == \"weft\"".to_owned(),
        "select has(media_type)".to_owned(),
        "select media_type == \"text/plain\"".to_owned(),
        format!("select object_id == \"{}\"", fixture.files[0].to_hex()),
        format!("select content_id == \"{}\"", content.to_hex()),
        format!("select relation(links, obj:{})", hidden.to_hex()),
        format!("select relation(links, obj:{})", visible.to_hex()),
        format!("select relation(points, obj:{})", visible.to_hex()),
        format!("select relation(none, obj:{})", visible.to_hex()),
        "select tag(\"alpha\")".to_owned(),
        "select tag(\"beta\")".to_owned(),
        "select tag(\"nope\")".to_owned(),
        "select tag(\"alpha\") and attrs.team == \"red\"".to_owned(),
        "select tag(\"alpha\") or tag(\"beta\")".to_owned(),
        "select not tag(\"alpha\")".to_owned(),
        "select not attrs.score > 50".to_owned(),
        "select attrs.team == \"red\" and not tag(\"alpha\")".to_owned(),
        format!("select not relation(links, obj:{})", hidden.to_hex()),
        "select (attrs.team == \"red\" and tag(\"alpha\")) or not has(attrs.score)".to_owned(),
        "select not (tag(\"alpha\") or attrs.team == \"red\")".to_owned(),
        "select true order by size desc limit 5".to_owned(),
        "select true order by object_id asc limit 7 offset 3".to_owned(),
        "select kind == \"file\" order by attrs.score desc, attrs.team asc limit 6".to_owned(),
        "select true order by mtime asc".to_owned(),
        "select attrs.team == \"red\" order by object_id desc offset 2".to_owned(),
    ];
    texts
        .into_iter()
        .map(|text| {
            let selector = selector::parse(&text)
                .unwrap_or_else(|error| panic!("selector `{text}` did not parse: {error}"));
            (text, selector)
        })
        .collect()
}

fn compare_success(label: &str, indexed: &Resolution, scan: &Resolution) {
    assert_eq!(
        indexed.selector_id, scan.selector_id,
        "{label}: selector_id"
    );
    assert_eq!(
        indexed.snapshot_id, scan.snapshot_id,
        "{label}: snapshot_id"
    );
    assert_eq!(
        indexed.matched_count, scan.matched_count,
        "{label}: matched_count"
    );
    assert_eq!(indexed.results, scan.results, "{label}: ordered results");
    assert_eq!(indexed.evidence, scan.evidence, "{label}: evidence");
}

fn compare_errors(label: &str, indexed: &weftfs::Error, scan: &weftfs::Error) {
    assert_eq!(
        indexed.class_name(),
        scan.class_name(),
        "{label}: error class"
    );
    assert_eq!(
        indexed.to_string(),
        scan.to_string(),
        "{label}: error message"
    );
}

#[test]
fn indexed_matches_scan_across_random_states() {
    let seeds = [11u64, 29, 47];
    let principal = principal();
    let limits = weftcodec::Limits::default();
    let mut states = 0usize;
    let mut selectors_checked = 0usize;
    let mut assertions = 0usize;

    for seed in seeds {
        let mut fixture = build_fixture(seed);
        let pinned = fixture.store.snapshot_create("pinned").unwrap();

        // Mutate after the snapshot so head and pinned views differ.
        let root = fixture.store.resolve_path("/").unwrap();
        fixture
            .store
            .create_file_seeded(
                root,
                b"post-snapshot",
                b"later",
                0o644,
                1000,
                1000,
                BTreeMap::from([("team".to_owned(), Value::Str("red".to_owned()))]),
                format!("post-seed-{seed}").as_bytes(),
            )
            .unwrap();
        fixture
            .store
            .relation_add(
                fixture.visible[0],
                "links",
                RelationTarget::Object(fixture.files[1]),
                None,
                BTreeMap::new(),
            )
            .unwrap();

        for (label, selector) in queries(&fixture) {
            for (view_name, view) in [("head", None), ("pinned", Some(pinned))] {
                let indexed = fixture.store.resolve(&selector, view, &principal, &limits);
                let scan = fixture
                    .store
                    .resolve_scan(&selector, view, &principal, &limits);
                match (indexed, scan) {
                    (Ok(indexed), Ok(scan)) => {
                        compare_success(&format!("seed={seed} {view_name} {label}"), &indexed, &scan);
                        assertions += 5;
                    }
                    (Err(indexed), Err(scan)) => {
                        compare_errors(&format!("seed={seed} {view_name} {label}"), &indexed, &scan);
                        assertions += 2;
                    }
                    (indexed, scan) => panic!(
                        "seed={seed} {view_name} `{label}`: indexed/scan disagreed on success: {:?} / {:?}",
                        indexed.map(|r| r.matched_count),
                        scan.map(|r| r.matched_count)
                    ),
                }
                selectors_checked += 1;
            }
        }

        // Error parity: the same pre-resolution checks must produce the same
        // class and message on both paths.
        let mut bad_namespace = selector::parse("select true").unwrap();
        bad_namespace.namespace = "other".to_owned();
        let indexed = fixture
            .store
            .resolve(&bad_namespace, None, &principal, &limits);
        let scan = fixture
            .store
            .resolve_scan(&bad_namespace, None, &principal, &limits);
        match (indexed, scan) {
            (Err(indexed), Err(scan)) => {
                compare_errors("namespace mismatch", &indexed, &scan);
                assertions += 2;
            }
            _ => panic!("namespace mismatch should fail identically on both paths"),
        }
        selectors_checked += 1;
        states += 1;
    }

    println!(
        "index-equivalence: states={states} seeds={seeds:?} selectors={selectors_checked} assertions={assertions}"
    );
    assert_eq!(states, seeds.len());
}
