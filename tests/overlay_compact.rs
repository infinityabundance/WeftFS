//! `Store::compact` and `weftfs compact`: canonicalising an overlay file.
//!
//! Compaction must never change the logical bytes or the content root. Every
//! test here reads the object before and after (through the store and, where a
//! snapshot is involved, through the pinned state) and compares the Merkle
//! root. A tampered chunk proves the rebuild fails closed before publishing.

use std::collections::BTreeMap;
use std::process::Command;

use tempfile::tempdir;

use weftfs::format::{ContentIdentity, RepresentationKind};
use weftfs::identity::Hash32;
use weftfs::recovery::object_path;
use weftfs::store::{OpenMode, Options, RepresentationMode, Store};

const CHUNK: u64 = 8;
const SEED: [u8; 32] = [9u8; 32];

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        representation: RepresentationMode::Overlay,
        content_identity: ContentIdentity::MerkleV3,
        ..Options::default()
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| ((index as u32 * 41 + u32::from(seed) * 13 + 3) % 251) as u8)
        .collect()
}

fn create(store: &mut Store, bytes: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, b"f", bytes, 0o644, 0, 0, BTreeMap::new(), &SEED)
        .unwrap()
}

fn manifest_of(store: &Store, id: Hash32) -> weftfs::format::Manifest {
    store
        .manifest_of(store.object_meta(id).unwrap().content)
        .unwrap()
}

/// Build an overlay file with three layers and return (id, bytes, root).
fn overlay_file(store: &mut Store) -> (Hash32, Vec<u8>, Hash32) {
    let id = create(store, &pattern(96, 1));
    store.write_file(id, 0, &pattern(8, 2)).unwrap();
    store.write_file(id, 8, &pattern(8, 3)).unwrap();
    store.write_file(id, 41, &pattern(7, 4)).unwrap();
    let layer = manifest_of(store, id)
        .representation
        .expect("overlay representation");
    assert_eq!(layer.kind, RepresentationKind::Overlay);
    let bytes = store.read_object(id).unwrap();
    let root = manifest_of(store, id).content_hash;
    (id, bytes, root)
}

#[test]
fn compaction_preserves_bytes_and_the_content_root() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let (id, before_bytes, before_root) = overlay_file(&mut store);
    let before_content = store.object_meta(id).unwrap().content;
    let before_generation = store.generation();

    let report = store.compact(id).unwrap();
    assert_eq!(report.object, id);
    assert_eq!(report.previous_content, before_content);
    assert_ne!(report.content, before_content);
    assert_eq!(report.content_root, before_root);
    assert_eq!(report.size, 96);
    assert_eq!(report.chunks, 12);
    assert_eq!(report.verified_bytes, 96);
    assert_eq!(report.overlay_depth, 1);
    assert!(report.overlay_bytes > 0);
    assert_eq!(store.generation(), before_generation + 1);

    assert_eq!(store.read_object(id).unwrap(), before_bytes);
    let manifest = manifest_of(&store, id);
    assert!(
        manifest.representation.is_none(),
        "compaction must publish a plain v3 chunk table"
    );
    assert_eq!(manifest.content_hash, before_root);
    assert_eq!(
        manifest.chunks.iter().map(|chunk| chunk.len).sum::<u64>(),
        96
    );
    assert_eq!(store.manifest_of(report.content).unwrap().chunks.len(), 12);

    let verified = store.verify(true).unwrap();
    assert!(verified.ok, "problems: {:?}", verified.problems);

    // Compacting again is a documented refusal: the object is canonical now.
    let err = store.compact(id).unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedRepresentation");
    assert_eq!(store.generation(), before_generation + 1);
}

#[test]
fn compaction_of_a_non_overlay_object_is_refused_and_publishes_nothing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let id = create(&mut store, &pattern(48, 1));
    let generation = store.generation();
    let content = store.object_meta(id).unwrap().content;

    let err = store.compact(id).unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedRepresentation");
    assert_eq!(store.generation(), generation);
    assert_eq!(store.object_meta(id).unwrap().content, content);
    assert_eq!(store.read_object(id).unwrap(), pattern(48, 1));
}

#[test]
fn a_snapshot_pinned_before_compaction_still_reads_its_version() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let (id, _latest, _root) = overlay_file(&mut store);
    let pinned_bytes = store.read_object(id).unwrap();
    let pinned_content = store.object_meta(id).unwrap().content;
    let snapshot = store.snapshot_create("before-compaction").unwrap();

    let report = store.compact(id).unwrap();
    assert_ne!(report.content, pinned_content);
    assert_eq!(store.read_object(id).unwrap(), pinned_bytes);

    // Before GC the old chain is still readable through the snapshot.
    let state = store.state_at(snapshot).unwrap();
    let record = state.objects.get(&id).unwrap().clone();
    assert_eq!(record.content, pinned_content);
    assert_eq!(
        store.read_content(record.content, 0, record.size).unwrap(),
        pinned_bytes
    );

    // GC keeps every blob the snapshot reaches, and collects nothing the head
    // still needs.
    store.gc(true).unwrap();
    let state = store.state_at(snapshot).unwrap();
    let record = state.objects.get(&id).unwrap();
    assert_eq!(
        store.read_content(record.content, 0, record.size).unwrap(),
        pinned_bytes
    );
    assert_eq!(store.read_object(id).unwrap(), pinned_bytes);
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn gc_after_compaction_without_a_snapshot_collects_the_old_layers() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let (id, bytes, _root) = overlay_file(&mut store);
    let old_content = store.object_meta(id).unwrap().content;
    let compacted = store.compact(id).unwrap();

    store.gc(true).unwrap();
    assert_eq!(store.read_object(id).unwrap(), bytes);
    assert_eq!(store.object_meta(id).unwrap().content, compacted.content);
    assert!(
        store.read_content(old_content, 0, 96).is_err(),
        "the unreferenced overlay chain is collectable"
    );
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn corruption_is_detected_before_any_publication() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let (id, _bytes, _root) = overlay_file(&mut store);
    let content = store.object_meta(id).unwrap().content;
    let manifest = store.manifest_of(content).unwrap();
    let overlay = manifest.representation.unwrap().overlay.unwrap();
    let chunk = overlay
        .extents
        .first()
        .unwrap()
        .chunks
        .iter()
        .find(|chunk| chunk.len == CHUNK)
        .cloned()
        .unwrap();

    // Replace one chunk blob with different bytes of the same length.
    let path = object_path(dir.path(), chunk.hash);
    std::fs::write(&path, vec![0xEEu8; CHUNK as usize]).unwrap();
    let generation = store.generation();

    let err = store.compact(id).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt");
    assert_eq!(
        store.generation(),
        generation,
        "a failed compaction must publish nothing"
    );
    assert_eq!(store.object_meta(id).unwrap().content, content);
}

#[test]
fn compaction_is_old_or_new_under_the_crash_proxy() {
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let dir = tempdir().unwrap();
        let (expected_bytes, before_generation, before_content) = {
            let mut store = Store::init(dir.path(), options()).unwrap();
            let (id, bytes, _root) = overlay_file(&mut store);
            (
                bytes,
                store.generation(),
                store.object_meta(id).unwrap().content,
            )
        };
        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .arg("--store")
            .arg(dir.path())
            .args(["compact", "/f"])
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs binary");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}, but exited {status:?}"
        );

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        let id = store.resolve_path("/f").unwrap();
        assert_eq!(
            store.read_object(id).unwrap(),
            expected_bytes,
            "phase {phase} changed the logical bytes"
        );
        let generation = store.generation();
        assert!(
            generation == before_generation || generation == before_generation + 1,
            "phase {phase}: unexpected generation {generation}"
        );
        let content = store.object_meta(id).unwrap().content;
        if generation == before_generation {
            assert_eq!(content, before_content, "phase {phase}: old manifest moved");
        } else {
            assert_ne!(content, before_content, "phase {phase}: nothing published");
            assert!(
                store.manifest_of(content).unwrap().representation.is_none(),
                "phase {phase}: published manifest is not canonical"
            );
            let report = store.verify(true).unwrap();
            assert!(
                report.ok,
                "phase {phase} verify problems: {:?}",
                report.problems
            );
        }
    }
}

#[test]
fn compaction_clears_the_configured_writer_bounds() {
    let dir = tempdir().unwrap();
    let bounded = Options {
        max_overlay_depth: 1,
        ..options()
    };
    let mut store = Store::init(dir.path(), bounded).unwrap();
    let id = create(&mut store, &pattern((130 * CHUNK) as usize, 1));
    let mut failure = None;
    for index in 0..=64u64 {
        if let Err(err) = store.write_file(id, index * 2 * CHUNK, &pattern(8, index as u8)) {
            failure = Some(err);
            break;
        }
    }
    let err = failure.expect("the depth bound must refuse the 65th extent");
    assert_eq!(err.class_name(), "LimitExceeded");
    assert!(err.message().contains("compact"));

    let bytes = store.read_object(id).unwrap();
    let report = store.compact(id).unwrap();
    assert_eq!(report.overlay_depth, 1);
    assert!(report.overlay_bytes > 0);
    assert_eq!(store.read_object(id).unwrap(), bytes);

    // The canonical base is depth 0, so a fresh layer fits again.
    store.write_file(id, 2 * CHUNK, &pattern(8, 200)).unwrap();
    let layer = manifest_of(&store, id).representation.unwrap();
    assert_eq!(layer.kind, RepresentationKind::Overlay);
    assert_eq!(layer.overlay.unwrap().depth, 1);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn the_cli_compact_verb_reports_the_unchanged_root() {
    let dir = tempdir().unwrap();
    let (id, bytes, root) = {
        let mut store = Store::init(dir.path(), options()).unwrap();
        overlay_file(&mut store)
    };
    let output = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .arg("--store")
        .arg(dir.path())
        .args(["--json", "compact", "/f"])
        .output()
        .expect("run weftfs compact");
    assert!(
        output.status.success(),
        "compact failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["content_root"], root.to_hex());
    assert_eq!(value["size"], 96);
    assert_eq!(value["chunks"], 12);
    assert_eq!(value["overlay_depth"], 1);
    assert!(value["verified_bytes"].as_u64().unwrap() >= 96);

    let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.read_object(id).unwrap(), bytes);
    let published = Hash32::from_hex(value["content"].as_str().unwrap()).unwrap();
    assert_eq!(store.manifest_of(published).unwrap().content_hash, root);
}
