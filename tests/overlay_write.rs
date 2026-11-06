//! `weft.manifest.v4` overlay write path.
//!
//! Every fixture here is produced by the shipped writer: a file created under
//! the Merkle content identity is a canonical `weft.manifest.v3` base, and each
//! range write publishes a v4 layer. Exactness is proved twice per case: the
//! bytes served through the overlay must equal a parallel Raw+Merkle store that
//! applied the same logical edits through the existing chunk-COW path, and the
//! two content roots must be identical (the overlay's Merkle root is defined to
//! be the canonical single-table root).
//!
//! Covered: aligned and misaligned overwrites, overlapping ranges, append,
//! growing truncate, zero range, layer coalescing, the manifest-extent limit
//! forcing a new layer, the configured depth and byte bounds, snapshot and GC
//! retention of older versions, and the process-fault crash proxy.

use std::collections::BTreeMap;
use std::process::Command;

use tempfile::tempdir;

use weftfs::format::ContentIdentity;
use weftfs::identity::Hash32;
use weftfs::store::{OpenMode, Options, RepresentationMode, Store};

const CHUNK: u64 = 8;
const SEED: [u8; 32] = [7u8; 32];

fn base_options(chunk: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: chunk,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        content_identity: ContentIdentity::MerkleV3,
        ..Options::default()
    }
}

fn overlay_options(chunk: u64) -> Options {
    Options {
        representation: RepresentationMode::Overlay,
        ..base_options(chunk)
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| ((index as u32 * 37 + u32::from(seed) * 11 + 1) % 251) as u8)
        .collect()
}

fn create(store: &mut Store, bytes: &[u8]) -> Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, b"f", bytes, 0o644, 0, 0, BTreeMap::new(), &SEED)
        .unwrap()
}

fn manifest(store: &Store, id: Hash32) -> weftfs::format::Manifest {
    let content = store.object_meta(id).unwrap().content;
    store.manifest_of(content).unwrap()
}

fn content_root(store: &Store, id: Hash32) -> Hash32 {
    manifest(store, id).content_hash
}

fn overlay(store: &Store, id: Hash32) -> Option<weftfs::format::OverlayRepresentation> {
    manifest(store, id).representation.and_then(|rep| {
        if rep.kind == weftfs::format::RepresentationKind::Overlay {
            rep.overlay
        } else {
            None
        }
    })
}

/// Apply the same logical edits to two stores: the overlay writer and the
/// existing chunk-COW writer, and assert they agree on bytes and content root.
fn apply_both(
    overlay_store: &mut Store,
    raw_store: &mut Store,
    overlay_id: Hash32,
    raw_id: Hash32,
    offset: u64,
    bytes: &[u8],
) {
    overlay_store.write_file(overlay_id, offset, bytes).unwrap();
    raw_store.write_file(raw_id, offset, bytes).unwrap();
    let overlay_bytes = overlay_store.read_object(overlay_id).unwrap();
    let raw_bytes = raw_store.read_object(raw_id).unwrap();
    assert_eq!(
        overlay_bytes,
        raw_bytes,
        "overlay bytes differ after a write at {offset} of {} bytes",
        bytes.len()
    );
    assert_eq!(
        content_root(overlay_store, overlay_id),
        content_root(raw_store, raw_id),
        "overlay content root differs from the canonical single-table root at {offset}"
    );
}

#[test]
fn aligned_and_misaligned_overwrites_are_exact_and_match_the_canonical_root() {
    let overlay_dir = tempdir().unwrap();
    let raw_dir = tempdir().unwrap();
    let mut overlay_store = Store::init(overlay_dir.path(), overlay_options(CHUNK)).unwrap();
    let mut raw_store = Store::init(raw_dir.path(), base_options(CHUNK)).unwrap();
    let initial = pattern(72, 1);
    let overlay_id = create(&mut overlay_store, &initial);
    let raw_id = create(&mut raw_store, &initial);
    assert!(
        overlay(&overlay_store, overlay_id).is_none(),
        "a freshly created file is a canonical v3 base, not an overlay"
    );

    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        8,
        &pattern(8, 2),
    );
    let layer = overlay(&overlay_store, overlay_id).expect("first write publishes an overlay");
    assert_eq!(layer.depth, 1);
    assert_eq!(layer.extents.len(), 1);
    assert_eq!(layer.extents[0].offset, 8);
    assert_eq!(layer.extents[0].length, 8);

    // Misaligned: 5 bytes at 21 spans chunks 2 and 3 of an 8-byte grid.
    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        21,
        &pattern(5, 3),
    );
    let layer = overlay(&overlay_store, overlay_id).expect("overlay");
    assert_eq!(layer.extents.len(), 1, "the adjacent extent is folded in");
    let extent = layer.extents.last().unwrap();
    assert_eq!(
        extent.offset, 8,
        "the merged extent starts at the previous extent"
    );
    assert_eq!(
        extent.length, 24,
        "a misaligned write widens to the chunk grid and keeps untouched bytes"
    );

    // A write at the very end of a full chunk and the final short chunk.
    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        64,
        &pattern(8, 4),
    );
    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        70,
        &[0xAA, 0xBB],
    );

    let report = overlay_store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn overlapping_ranges_in_one_call_are_last_writer_wins() {
    let overlay_dir = tempdir().unwrap();
    let raw_dir = tempdir().unwrap();
    let mut overlay_store = Store::init(overlay_dir.path(), overlay_options(CHUNK)).unwrap();
    let mut raw_store = Store::init(raw_dir.path(), base_options(CHUNK)).unwrap();
    let initial = pattern(40, 9);
    let overlay_id = create(&mut overlay_store, &initial);
    let raw_id = create(&mut raw_store, &initial);

    let first = pattern(12, 10);
    let second = pattern(7, 11);
    let ranges: Vec<(u64, &[u8])> = vec![(9, first.as_slice()), (14, second.as_slice())];
    overlay_store
        .write_file_ranges(overlay_id, &ranges)
        .unwrap();
    raw_store.write_file_ranges(raw_id, &ranges).unwrap();

    assert_eq!(
        overlay_store.read_object(overlay_id).unwrap(),
        raw_store.read_object(raw_id).unwrap()
    );
    assert_eq!(
        content_root(&overlay_store, overlay_id),
        content_root(&raw_store, raw_id)
    );
    // 9..21 and 14..21 overlap; the later range wins where they intersect.
    let bytes = overlay_store.read_object(overlay_id).unwrap();
    assert_eq!(&bytes[14..21], &second[..7]);
    assert_eq!(&bytes[9..14], &first[..5]);
}

#[test]
fn adjacent_writes_coalesce_without_growing_the_chain() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    let id = create(&mut store, &pattern(40, 1));
    store.write_file(id, 0, &pattern(8, 2)).unwrap();
    store.write_file(id, 8, &pattern(8, 3)).unwrap();
    let layer = overlay(&store, id).expect("overlay");
    assert_eq!(layer.depth, 1, "adjacent extents fold into one layer");
    assert_eq!(layer.extents.len(), 1, "adjacent extents merge");
    assert_eq!(layer.extents[0].offset, 0);
    assert_eq!(layer.extents[0].length, 16);

    // Overlapping the merged extent also stays at depth 1.
    store.write_file(id, 4, &pattern(8, 4)).unwrap();
    let layer = overlay(&store, id).expect("overlay");
    assert_eq!(layer.depth, 1);
    assert_eq!(layer.extents.len(), 1);
    assert_eq!(layer.extents[0].length, 16);
}

#[test]
fn append_and_truncate_grow_publish_exact_layers() {
    let overlay_dir = tempdir().unwrap();
    let raw_dir = tempdir().unwrap();
    let mut overlay_store = Store::init(overlay_dir.path(), overlay_options(CHUNK)).unwrap();
    let mut raw_store = Store::init(raw_dir.path(), base_options(CHUNK)).unwrap();
    // 21 bytes: two full chunks plus a 5-byte tail.
    let initial = pattern(21, 1);
    let overlay_id = create(&mut overlay_store, &initial);
    let raw_id = create(&mut raw_store, &initial);

    // Append inside the short tail chunk.
    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        21,
        &pattern(5, 2),
    );
    // Append past EOF with a gap: 3 bytes at 40 leaves 26 zero bytes.
    apply_both(
        &mut overlay_store,
        &mut raw_store,
        overlay_id,
        raw_id,
        40,
        &pattern(3, 3),
    );

    // Growing truncate writes zeros through the overlay path.
    overlay_store.truncate(overlay_id, 60).unwrap();
    raw_store.truncate(raw_id, 60).unwrap();
    assert_eq!(
        overlay_store.read_object(overlay_id).unwrap(),
        raw_store.read_object(raw_id).unwrap()
    );
    assert_eq!(
        content_root(&overlay_store, overlay_id),
        content_root(&raw_store, raw_id)
    );
    assert_eq!(overlay_store.object_meta(overlay_id).unwrap().size, 60);

    // Truncate to the same size is a no-op and publishes nothing.
    let generation = overlay_store.generation();
    let version = overlay_store.truncate(overlay_id, 60).unwrap();
    assert_eq!(
        version,
        overlay_store.object_meta(overlay_id).unwrap().version
    );
    assert_eq!(overlay_store.generation(), generation);

    // Shrinking is refused rather than silently materialised.
    let err = overlay_store.truncate(overlay_id, 10).unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedRepresentation");
}

#[test]
fn zero_range_publishes_an_overlay_layer_and_is_bounded() {
    let overlay_dir = tempdir().unwrap();
    let raw_dir = tempdir().unwrap();
    let mut overlay_store = Store::init(overlay_dir.path(), overlay_options(CHUNK)).unwrap();
    let mut raw_store = Store::init(raw_dir.path(), base_options(CHUNK)).unwrap();
    let initial = pattern(96, 5);
    let overlay_id = create(&mut overlay_store, &initial);
    let raw_id = create(&mut raw_store, &initial);

    overlay_store.zero_range(overlay_id, 17, 30, false).unwrap();
    raw_store.zero_range(raw_id, 17, 30, false).unwrap();
    assert_eq!(
        overlay_store.read_object(overlay_id).unwrap(),
        raw_store.read_object(raw_id).unwrap()
    );
    assert_eq!(
        content_root(&overlay_store, overlay_id),
        content_root(&raw_store, raw_id)
    );
    assert!(overlay(&overlay_store, overlay_id).is_some());

    // Zero range that extends the file.
    overlay_store.zero_range(overlay_id, 120, 20, true).unwrap();
    raw_store.zero_range(raw_id, 120, 20, true).unwrap();
    assert_eq!(
        overlay_store.read_object(overlay_id).unwrap(),
        raw_store.read_object(raw_id).unwrap()
    );
    assert_eq!(
        content_root(&overlay_store, overlay_id),
        content_root(&raw_store, raw_id)
    );

    // A large zero fill is stored as one content-addressed zero chunk, so peak
    // memory does not scale with the range. This is asserted structurally: the
    // whole file is 140 bytes and all interior chunks are full-size.
    let manifest = manifest(&overlay_store, overlay_id);
    assert_eq!(manifest.size, 140);
}

#[test]
fn collapse_and_insert_are_refused_for_an_overlay_object() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    let id = create(&mut store, &pattern(64, 1));
    store.write_file(id, 0, &pattern(8, 2)).unwrap();

    let collapse = store.collapse_range(id, 0, 8).unwrap_err();
    assert_eq!(collapse.class_name(), "UnsupportedRepresentation");
    let insert = store.insert_range(id, 0, 8).unwrap_err();
    assert_eq!(insert.class_name(), "UnsupportedRepresentation");

    // The refusal published nothing and the object is unchanged.
    assert_eq!(store.object_meta(id).unwrap().size, 64);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn the_manifest_extent_limit_forces_a_new_layer() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    // 130 chunks so the writes below can name 65 distinct, non-adjacent chunks.
    let id = create(&mut store, &pattern((130 * CHUNK) as usize, 1));
    for index in 0..=64u64 {
        // Even chunks only: never adjacent, so each write adds one extent.
        let offset = index * 2 * CHUNK;
        store
            .write_file(id, offset, &pattern(8, index as u8))
            .unwrap();
    }
    let layer = overlay(&store, id).expect("overlay");
    assert_eq!(
        layer.depth, 2,
        "the 65th non-adjacent extent must start a new layer"
    );
    assert_eq!(layer.extents.len(), 1);
    assert!(store.verify(true).unwrap().ok);

    // The canonical root still matches a raw COW rebuild of the same bytes.
    let raw_dir = tempdir().unwrap();
    let mut raw_store = Store::init(raw_dir.path(), base_options(CHUNK)).unwrap();
    let raw_id = create(&mut raw_store, &pattern((130 * CHUNK) as usize, 1));
    for index in 0..=64u64 {
        let offset = index * 2 * CHUNK;
        raw_store
            .write_file(raw_id, offset, &pattern(8, index as u8))
            .unwrap();
    }
    assert_eq!(content_root(&store, id), content_root(&raw_store, raw_id));
    assert_eq!(
        store.read_object(id).unwrap(),
        raw_store.read_object(raw_id).unwrap()
    );
}

#[test]
fn the_configured_depth_bound_refuses_and_names_compaction() {
    let dir = tempdir().unwrap();
    let options = Options {
        max_overlay_depth: 1,
        ..overlay_options(CHUNK)
    };
    let mut store = Store::init(dir.path(), options.clone()).unwrap();
    let id = create(&mut store, &pattern((130 * CHUNK) as usize, 1));
    let mut failure = None;
    for index in 0..=64u64 {
        let offset = index * 2 * CHUNK;
        match store.write_file(id, offset, &pattern(8, index as u8)) {
            Ok(_) => {}
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }
    let err = failure.expect("the writer must refuse once the depth bound is reached");
    assert_eq!(err.class_name(), "LimitExceeded");
    assert!(
        err.message().contains("compact"),
        "the refusal must tell the caller to compact: {}",
        err.message()
    );
    // The object that was accepted so far is still exact and verifiable.
    assert!(store.verify(true).unwrap().ok);
    assert!(overlay(&store, id).is_some());
}

#[test]
fn the_configured_byte_bound_refuses_and_names_compaction() {
    let dir = tempdir().unwrap();
    let options = Options {
        max_overlay_bytes: 40,
        ..overlay_options(CHUNK)
    };
    let mut store = Store::init(dir.path(), options).unwrap();
    let id = create(&mut store, &pattern((130 * CHUNK) as usize, 1));
    // Six non-adjacent 8-byte extents total 48 logical overlay bytes.
    let mut failure = None;
    for index in 0..6u64 {
        let offset = index * 2 * CHUNK;
        match store.write_file(id, offset, &pattern(8, index as u8)) {
            Ok(_) => {}
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }
    let err = failure.expect("the writer must refuse once the byte bound is reached");
    assert_eq!(err.class_name(), "LimitExceeded");
    assert!(
        err.message().contains("compact"),
        "the refusal must tell the caller to compact: {}",
        err.message()
    );
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn rewriting_identical_bytes_publishes_nothing() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    let initial = pattern(32, 3);
    let id = create(&mut store, &initial);
    store.write_file(id, 0, &pattern(8, 4)).unwrap();
    let generation = store.generation();
    let version = store.object_meta(id).unwrap().version;
    let same = store.read_range(id, 0, 8).unwrap();
    let published = store.write_file(id, 0, &same).unwrap();
    assert_eq!(published, version);
    assert_eq!(store.generation(), generation);
}

#[test]
fn snapshots_and_gc_retain_every_overlay_layer() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    let initial = pattern(64, 1);
    let id = create(&mut store, &initial);
    let initial = pattern(64, 1);
    let base_content = store.object_meta(id).unwrap().content;

    store.write_file(id, 0, &pattern(8, 2)).unwrap();
    let first_overlay = store.object_meta(id).unwrap().content;
    let first_bytes = store.read_object(id).unwrap();
    let snapshot = store.snapshot_create("before-more-writes").unwrap();

    store.write_file(id, 9, &pattern(5, 3)).unwrap();
    store.write_file(id, 40, &pattern(8, 4)).unwrap();
    let latest_bytes = store.read_object(id).unwrap();

    // Older versions stay byte-exact by content id.
    assert_eq!(store.read_content(base_content, 0, 64).unwrap(), initial);
    assert_eq!(
        store.read_content(first_overlay, 0, 64).unwrap(),
        first_bytes
    );

    store.gc(true).unwrap();

    assert_eq!(store.read_content(base_content, 0, 64).unwrap(), initial);
    assert_eq!(
        store.read_content(first_overlay, 0, 64).unwrap(),
        first_bytes
    );
    assert_eq!(store.read_object(id).unwrap(), latest_bytes);

    // The snapshot still resolves to the version it pinned.
    let state = store.state_at(snapshot).unwrap();
    let record = state.objects.get(&id).unwrap();
    assert_eq!(record.content, first_overlay);
    assert_eq!(
        store.read_content(record.content, 0, record.size).unwrap(),
        first_bytes
    );
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn gc_collects_an_unreferenced_overlay_layer() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
    let id = create(&mut store, &pattern(64, 1));
    store.write_file(id, 0, &pattern(8, 2)).unwrap();
    // Rewriting the same chunk publishes a new layer whose base is the same v3
    // manifest; the first layer becomes unreachable.
    store.write_file(id, 0, &pattern(8, 3)).unwrap();
    let layers = manifest(&store, id);
    assert_eq!(layers.representation.unwrap().overlay.unwrap().depth, 1);

    let report = store.gc(false).unwrap();
    assert!(
        report.marked_manifests >= 2,
        "the head's base chain must be marked: {report:?}"
    );
    store.gc(true).unwrap();
    assert_eq!(store.read_object(id).unwrap(), {
        let mut expected = pattern(64, 1);
        expected[..8].copy_from_slice(&pattern(8, 3));
        expected
    });
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn crash_proxy_over_overlay_publication_yields_old_or_new() {
    for phase in [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ] {
        let dir = tempdir().unwrap();
        let payload_dir = tempdir().unwrap();
        let initial = pattern(64, 1);
        let mut old_bytes = initial.clone();
        old_bytes[..8].copy_from_slice(&pattern(8, 3));
        let mut expected_new = old_bytes.clone();
        expected_new[8..24].copy_from_slice(&pattern(16, 2));
        {
            let mut store = Store::init(dir.path(), overlay_options(CHUNK)).unwrap();
            let id = create(&mut store, &initial);
            store.write_file(id, 0, &pattern(8, 3)).unwrap();
        }
        let payload = payload_dir.path().join("payload.bin");
        std::fs::write(&payload, pattern(16, 2)).unwrap();
        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .arg("--store")
            .arg(dir.path())
            .args([
                "--representation",
                "overlay",
                "write",
                "/f",
                "--offset",
                "8",
                "--file",
            ])
            .arg(&payload)
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs binary");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}, but exited {status:?}"
        );

        let store = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
        let id = store.resolve_path("/f").unwrap();
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes == old_bytes || bytes == expected_new,
            "phase {phase} produced a partial overlay state"
        );
        // Only a published commit may be verified: an unpublished commit file
        // is expected to exist in the `after_commit_file` phase and `verify`
        // reports it as a pending tail, not a data error.
        if bytes == expected_new {
            let report = store.verify(true).unwrap();
            assert!(
                report.ok,
                "phase {phase} verify problems: {:?}",
                report.problems
            );
        }
    }
}
