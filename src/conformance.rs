//! Deterministic conformance fixture for the WeftFS-Go handoff.
//!
//! The fixture uses a fixed timestamp (`1700000000000000000` ns) and explicit
//! ObjectId seeds, so two builds produce byte-identical commits, manifests,
//! chunks, and selector results. It is intentionally small (< 100 KiB).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use weftcodec::selector;

use crate::auth::Principal;
use crate::error::Result;
use crate::identity::Hash32;
use crate::state::{ObjectKind, RelationTarget};
use crate::store::{Options, Store};

/// Fixed timestamp used by every fixture commit and object mtime.
pub const FIXTURE_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;

/// One object in the fixture summary.
#[derive(Clone, Debug)]
pub struct FixtureObject {
    pub path: String,
    pub object_id: Hash32,
    pub version_id: Hash32,
    pub content_id: Hash32,
    /// Whole-file content digest for files; the object's content digest for
    /// directories/symlinks.
    pub content_hash: Hash32,
    pub size: u64,
    pub kind: String,
}

/// One directory entry in the fixture summary (hard links appear once per
/// name, which is the point).
#[derive(Clone, Debug)]
pub struct FixtureEntry {
    pub path: String,
    pub entry_id: Hash32,
    pub target: Hash32,
}

/// One selector expectation.
#[derive(Clone, Debug)]
pub struct FixtureSelector {
    pub source: String,
    pub selector_id: Hash32,
    pub snapshot_id: Hash32,
    pub matched_count: u64,
    pub object_ids: Vec<Hash32>,
}

/// Summary returned by [`build_fixture_store`].
#[derive(Clone, Debug)]
pub struct FixtureSummary {
    pub format_version: String,
    pub generation: u64,
    pub head: Hash32,
    pub objects: Vec<FixtureObject>,
    pub entries: Vec<FixtureEntry>,
    pub snapshots: Vec<(String, Hash32)>,
    pub relation_ids: Vec<Hash32>,
    pub selectors: Vec<FixtureSelector>,
}

/// Build the fixture store at `path`. The directory must be absent or empty.
pub fn build_fixture_store(path: &Path) -> Result<FixtureSummary> {
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 3_600,
        fixed_timestamp_ns: Some(FIXTURE_TIMESTAMP_NS),
        ..Options::default()
    };
    let mut store = Store::init(path, options)?;

    let root = store.resolve_path("/")?;
    let docs = store.mkdir_seeded(root, b"docs", 0o755, 0, 0, b"fixture-dir-docs")?;
    let data = store.mkdir_seeded(root, b"data", 0o755, 0, 0, b"fixture-dir-data")?;

    let hello = store.create_file_seeded(
        root,
        b"hello.txt",
        b"hello weft\n",
        0o644,
        0,
        0,
        BTreeMap::new(),
        b"fixture-object-1",
    )?;
    let readme = store.create_file_seeded(
        docs,
        b"readme.md",
        b"# WeftFS fixture\n\nDeterministic store for cross-language handoff.\n",
        0o644,
        0,
        0,
        BTreeMap::new(),
        b"fixture-object-2",
    )?;
    let mut binary = Vec::with_capacity(4096);
    for index in 0..4096u32 {
        binary.push(((index.wrapping_mul(37).wrapping_add(11)) % 256) as u8);
    }
    let binary_id = store.create_file_seeded(
        data,
        b"binary.bin",
        &binary,
        0o644,
        0,
        0,
        BTreeMap::new(),
        b"fixture-object-3",
    )?;
    store.symlink_seeded(root, b"link", b"hello.txt", 0, 0, b"fixture-symlink")?;
    store.link(hello, root, b"hello.txt-hard")?;

    store.relation_add(
        hello,
        "tagged_as",
        RelationTarget::Value(weftcodec::wcb::Value::Str("greeting".to_owned())),
        Some(BTreeMap::from([(
            "source".to_owned(),
            weftcodec::wcb::Value::Str("fixture".to_owned()),
        )])),
        BTreeMap::new(),
    )?;
    store.relation_add(
        hello,
        "derived_from",
        RelationTarget::Object(readme),
        None,
        BTreeMap::new(),
    )?;

    let snapshot_id = store.snapshot_create("v1")?;
    let state = store.current_state().clone();
    let generation = store.generation();
    let head = store.head();

    let mut objects = Vec::new();
    for record in state.objects.values() {
        let path = state
            .path_for(record.id)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|| format!("<unlinked:{}>", record.id));
        let content_hash = if record.kind == ObjectKind::File {
            store
                .with_backend(|backend| {
                    crate::recovery::read_manifest_via(backend, record.content)
                })?
                .content_hash
        } else {
            record.content
        };
        objects.push(FixtureObject {
            path,
            object_id: record.id,
            version_id: record.version,
            content_id: record.content,
            content_hash,
            size: record.size,
            kind: record.kind.as_str().to_owned(),
        });
    }
    objects.sort_by(|a, b| a.path.cmp(&b.path));

    let mut relation_ids: Vec<Hash32> = state.relations.keys().copied().collect();
    relation_ids.sort();

    let mut entries: Vec<FixtureEntry> = state
        .entries
        .values()
        .filter_map(|entry| {
            entry_path(&state, entry).map(|path| FixtureEntry {
                path,
                entry_id: entry.id,
                target: entry.target,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let principal = Principal::root();
    let limits = weftcodec::Limits::default();
    let mut selectors = Vec::new();
    let sources = [
        "select kind == \"file\"".to_owned(),
        "select tag(\"greeting\")".to_owned(),
        format!("select relation(derived_from, obj:{})", readme.to_hex()),
    ];
    for source in sources {
        let parsed = selector::parse(&source)?;
        let resolution = store.resolve(&parsed, Some(snapshot_id), &principal, &limits)?;
        selectors.push(FixtureSelector {
            source,
            selector_id: resolution.selector_id,
            snapshot_id: resolution.snapshot_id,
            matched_count: resolution.matched_count,
            object_ids: resolution.results.iter().map(|r| r.object_id).collect(),
        });
    }

    // Keep `binary_id` observable in the summary even though the object loop
    // already visits it; this also documents the expected identity.
    debug_assert!(state.objects.contains_key(&binary_id));

    Ok(FixtureSummary {
        format_version: crate::format::FORMAT_CONTENTS.trim_end().to_owned(),
        generation,
        head,
        objects,
        entries,
        snapshots: state
            .snapshots
            .iter()
            .map(|(name, hash)| (name.clone(), *hash))
            .collect(),
        relation_ids,
        selectors,
    })
}

fn entry_path(state: &crate::state::State, entry: &crate::state::EntryRecord) -> Option<String> {
    let mut components = vec![entry.name.clone()];
    let mut current = entry.parent;
    let mut seen = BTreeSet::new();
    while current != crate::state::State::ROOT_ID {
        if !seen.insert(current) {
            return None;
        }
        let parent_entry = state.entries_for(current).into_iter().next()?;
        components.push(parent_entry.name.clone());
        current = parent_entry.parent;
    }
    components.reverse();
    let mut out = String::new();
    for component in components {
        out.push('/');
        out.push_str(&String::from_utf8_lossy(&component));
    }
    Some(out)
}
