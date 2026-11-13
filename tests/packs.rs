//! Packed blob plane court (subphase R11).
//!
//! Proves the plane contract in `src/packs.rs`:
//!
//! * packed reads are byte-identical for every member, whole and ranged, and a
//!   ranged read fetches exactly the member's bytes (asserted with the inner
//!   backend byte counter);
//! * a digest mismatch, a truncated pack, and a missing catalog all fail closed
//!   with a typed error;
//! * a process fault between pack publication and catalog publication, and
//!   between catalog segment and root publication, leaves the old or the new
//!   whole catalog, never a partial one;
//! * GC reclaims a pack only once every member is unreferenced; a pack with one
//!   live member survives;
//! * when the catalog bound is reached, new blobs are published flat and stay
//!   readable;
//! * the same input produces the same pack layout and the same catalog bytes.
//!
//! This is a single-host court with a local backend and a synthetic fixture. It
//! makes no media, power-loss, cloud, fleet, or production claim.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Capabilities, Key, KeyKind, LocalBackend};
use weftfs::packs::{PackOptions, PackedBlobPlane};
use weftfs::planes::{BlobPlane, CommitPlane};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::{Class, Hash32};

fn pack_options() -> PackOptions {
    PackOptions {
        pack_target_bytes: 8192,
        member_threshold_bytes: 2048,
        max_members_per_pack: 64,
        max_catalog_bytes: 1024 * 1024,
        catalog_segments: 8,
    }
}

fn plane_at(root: &Path, options: PackOptions) -> PackedBlobPlane {
    let mut inner = LocalBackend::new(root);
    BlobPlane::init_layout(&mut inner).expect("inner layout");
    let catalog = LocalBackend::new(root);
    PackedBlobPlane::new(Box::new(inner), Box::new(catalog), options).expect("packed plane")
}

fn member(i: u8) -> Key {
    Key::object(Hash32::from_bytes([i.wrapping_add(1); 32]))
}

/// Distinct object key from a 32-bit counter.
fn member32(i: u32) -> Key {
    let mut raw = [0u8; 32];
    raw[..4].copy_from_slice(&i.to_le_bytes());
    raw[31] = 0xa5;
    Key::object(Hash32::from_bytes(raw))
}

fn body(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn expect_class<T>(result: weftfs::Result<T>, class: Class, what: &str) {
    match result {
        Ok(_) => panic!("{what}: expected {class:?}, got success"),
        Err(err) => assert_eq!(err.class(), class, "{what}: {err}"),
    }
}

#[test]
fn packed_reads_are_byte_identical_and_ranges_are_narrow() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), pack_options());
    let mut expected: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    for i in 0..40u8 {
        let len = 1 + (i as usize * 97) % 1500;
        let key = member(i);
        let bytes = body(len, i);
        plane.put_if_absent(&key, &bytes).unwrap();
        expected.insert(key, bytes);
        if i % 7 == 6 {
            plane.flush().unwrap();
        }
    }
    plane.flush().unwrap();
    let stats = plane.pack_stats();
    assert!(
        stats.sealed_packs >= 2,
        "expected multiple sealed packs, got {stats:?}"
    );
    assert_eq!(stats.flat_members, 0, "{stats:?}");

    // Whole reads are byte-identical.
    for (key, bytes) in &expected {
        assert_eq!(&plane.get(key).unwrap(), bytes, "{}", key.display_path());
        assert_eq!(plane.len(key).unwrap(), bytes.len() as u64);
        assert!(plane.exists(key).unwrap());
    }

    // Ranged reads match the flat semantics.
    for (key, bytes) in &expected {
        let start = bytes.len() / 3;
        let want = 17.min(bytes.len() - start);
        assert_eq!(
            plane.get_range(key, start as u64, 17).unwrap(),
            bytes[start..start + want].to_vec(),
            "range {}",
            key.display_path()
        );
        expect_class(
            plane.get_range(key, bytes.len() as u64 + 1, 4),
            Class::InvalidArgument,
            "range past EOF",
        );
    }

    // A narrow range read fetches only that range: warm the catalog first, then
    // assert the inner plane's byte counter moved by exactly the range length.
    let key = member(30);
    let bytes = expected.get(&key).unwrap();
    let _ = plane.get(&key).unwrap();
    let before = plane.stats();
    let want = 4096u64.min(bytes.len() as u64 - 10);
    let got = plane.get_range(&key, 10, 4096).unwrap();
    let after = plane.stats();
    assert_eq!(got, bytes[10..10 + want as usize].to_vec());
    assert_eq!(
        after.range_reads - before.range_reads,
        1,
        "a member range must be one ranged request"
    );
    assert_eq!(
        after.bytes_read - before.bytes_read,
        want,
        "a member range must fetch exactly its bytes"
    );
}

#[test]
fn large_blobs_bypass_packing_and_remain_flat() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), pack_options());
    for i in 0..6u8 {
        plane.put_if_absent(&member(i), &body(512, i)).unwrap();
    }
    let large_len = 4096; // above member_threshold_bytes (2048)
    plane
        .put_if_absent(&member(50), &body(large_len, 50))
        .unwrap();
    plane.flush().unwrap();
    let stats = plane.pack_stats();
    assert_eq!(stats.flat_members, 1, "{stats:?}");
    assert_eq!(stats.flat_bytes, large_len as u64, "{stats:?}");
    assert_eq!(
        plane.get(&member(50)).unwrap(),
        body(large_len, 50),
        "large blob reads flat"
    );
    // The large blob is a real flat object in the inner namespace.
    let inner = LocalBackend::new(dir.path());
    let keys = inner.list(&Key::prefix(KeyKind::Object)).unwrap();
    assert!(keys.contains(&member(50)), "large blob must stay flat");
    assert!(
        !keys.contains(&member(0)),
        "packed member must not also be flat"
    );
}

#[test]
fn digest_mismatch_fails_closed() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), pack_options());
    for i in 0..4u8 {
        plane.put_if_absent(&member(i), &body(300, i)).unwrap();
    }
    plane.flush().unwrap();
    // The first member sits at offset 0 of the pack.
    let pack_key = {
        let inner = LocalBackend::new(dir.path());
        let keys = inner.list(&Key::prefix(KeyKind::Pack)).unwrap();
        assert_eq!(keys.len(), 1);
        keys[0].clone()
    };
    let path = dir.path().join(pack_key.display_path());
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut first = [0u8; 1];
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut first).unwrap();
        first[0] ^= 0xFF;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&first).unwrap();
        file.sync_all().unwrap();
    }
    expect_class(plane.get(&member(0)), Class::Corrupt, "flipped pack byte");
    // A different member is unaffected.
    assert_eq!(plane.get(&member(1)).unwrap(), body(300, 1));
}

#[test]
fn truncated_pack_fails_closed() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), pack_options());
    for i in 0..3u8 {
        plane.put_if_absent(&member(i), &body(400, i)).unwrap();
    }
    plane.flush().unwrap();
    let pack_key = {
        let inner = LocalBackend::new(dir.path());
        inner.list(&Key::prefix(KeyKind::Pack)).unwrap()[0].clone()
    };
    let path = dir.path().join(pack_key.display_path());
    let len = std::fs::metadata(&path).unwrap().len();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(len - 50).unwrap();
    file.sync_all().unwrap();
    drop(file);
    // 3 x 400 bytes; the last member is truncated.
    expect_class(
        plane.get(&member(2)),
        Class::Corrupt,
        "truncated member must not read short",
    );
    assert_eq!(plane.get(&member(0)).unwrap(), body(400, 0));
}

#[test]
fn missing_catalog_fails_closed() {
    let dir = tempdir().unwrap();
    {
        let mut plane = plane_at(dir.path(), pack_options());
        for i in 0..4u8 {
            plane.put_if_absent(&member(i), &body(200, i)).unwrap();
        }
        plane.flush().unwrap();
    }
    // Remove the catalog root: the pack is now unreferenced.
    let mut catalog = LocalBackend::new(dir.path());
    catalog
        .delete_control(&Key::pack_catalog_root())
        .expect("delete root");
    let plane = plane_at(dir.path(), pack_options());
    expect_class(
        plane.get(&member(0)),
        Class::NotFound,
        "member of an unreferenced pack",
    );
    assert!(!plane.exists(&member(0)).unwrap());
    // The pack object itself still exists and is reclaimed by GC.
    let inner = LocalBackend::new(dir.path());
    assert_eq!(inner.list(&Key::prefix(KeyKind::Pack)).unwrap().len(), 1);
}

#[test]
fn missing_segment_fails_closed() {
    let dir = tempdir().unwrap();
    {
        let mut plane = plane_at(dir.path(), pack_options());
        for i in 0..4u8 {
            plane.put_if_absent(&member(i), &body(200, i)).unwrap();
        }
        plane.flush().unwrap();
    }
    // Remove every catalog segment but keep the root.
    let mut inner = LocalBackend::new(dir.path());
    for key in inner.list(&Key::prefix(KeyKind::PackCatalog)).unwrap() {
        inner.delete(&key).unwrap();
    }
    let plane = plane_at(dir.path(), pack_options());
    expect_class(
        plane.get(&member(0)),
        Class::NotFound,
        "member whose catalog segment is missing",
    );
}

#[test]
fn gc_reclaims_a_pack_only_when_every_member_is_gone() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), pack_options());
    for i in 0..4u8 {
        plane.put_if_absent(&member(i), &body(256, i)).unwrap();
    }
    plane.flush().unwrap();
    assert_eq!(plane.pack_stats().sealed_packs, 1);

    plane.delete(&member(0)).unwrap();
    let reclaim = plane.reclaim_unreferenced_packs().unwrap();
    assert_eq!(reclaim.deleted_packs, 0, "a live member keeps its pack");
    assert_eq!(reclaim.live_packs, 1);
    assert_eq!(reclaim.live_members, 3);
    let inner = LocalBackend::new(dir.path());
    assert_eq!(inner.list(&Key::prefix(KeyKind::Pack)).unwrap().len(), 1);
    for i in 1..4u8 {
        assert_eq!(plane.get(&member(i)).unwrap(), body(256, i));
    }

    plane
        .delete_many(&[member(1), member(2), member(3)])
        .unwrap();
    let reclaim = plane.reclaim_unreferenced_packs().unwrap();
    assert_eq!(reclaim.deleted_packs, 1);
    assert!(reclaim.deleted_pack_bytes > 0);
    assert_eq!(reclaim.live_packs, 0);
    let inner = LocalBackend::new(dir.path());
    assert!(inner.list(&Key::prefix(KeyKind::Pack)).unwrap().is_empty());
    expect_class(plane.get(&member(1)), Class::NotFound, "collected member");
}

#[test]
fn catalog_bound_falls_back_to_flat() {
    let options = PackOptions {
        max_catalog_bytes: 400, // room for three 113-byte entries
        catalog_segments: 1,
        ..pack_options()
    };
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), options);
    for i in 0..10u8 {
        plane.put_if_absent(&member(i), &body(128, i)).unwrap();
    }
    plane.flush().unwrap();
    let stats = plane.pack_stats();
    assert!(stats.packing_disabled, "{stats:?}");
    assert_eq!(stats.catalog_entries, 3, "{stats:?}");
    assert_eq!(stats.flat_members, 7, "{stats:?}");
    for i in 0..10u8 {
        assert_eq!(plane.get(&member(i)).unwrap(), body(128, i));
    }
    let inner = LocalBackend::new(dir.path());
    assert_eq!(
        inner.list(&Key::prefix(KeyKind::Object)).unwrap().len(),
        7,
        "blobs past the catalog bound are flat"
    );
}

#[test]
fn deterministic_layout_for_a_fixed_pack_size() {
    let options = PackOptions {
        pack_target_bytes: 4096,
        member_threshold_bytes: 1024,
        max_members_per_pack: 32,
        max_catalog_bytes: 1024 * 1024,
        catalog_segments: 4,
    };
    let mut layouts = Vec::new();
    for round in 0..2u8 {
        let dir = tempdir().unwrap();
        let mut plane = plane_at(dir.path(), options);
        for i in 0..20u8 {
            plane
                .put_if_absent(&member(i), &body(300, i.wrapping_add(round * 0)))
                .unwrap();
        }
        plane.flush().unwrap();
        let inner = LocalBackend::new(dir.path());
        let packs: Vec<(String, u64)> = inner
            .list(&Key::prefix(KeyKind::Pack))
            .unwrap()
            .into_iter()
            .map(|key| {
                let len = inner.len(&key).unwrap();
                (key.display_path(), len)
            })
            .collect();
        let catalog = LocalBackend::new(dir.path());
        let root = catalog
            .read_control(&Key::pack_catalog_root())
            .unwrap()
            .expect("catalog root");
        layouts.push((packs, root.bytes));
        std::mem::drop(plane);
    }
    assert_eq!(layouts[0].0, layouts[1].0, "pack layout must be stable");
    assert_eq!(
        layouts[0].1, layouts[1].1,
        "catalog bytes must be identical"
    );
}

fn store_options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn packed_local(options: PackOptions) -> BackendKind {
    BackendKind::packed(BackendKind::Local, options).expect("packed local")
}

#[test]
fn store_round_trip_verify_and_gc_on_a_packed_plane() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut store =
        Store::init_with_backend(&path, store_options(), packed_local(pack_options())).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut ids = Vec::new();
    for i in 0..8u8 {
        let payload = body(700 + i as usize * 13, i);
        let id = store
            .create_file_seeded(
                root,
                format!("f{i}").as_bytes(),
                &payload,
                0o644,
                0,
                0,
                BTreeMap::new(),
                format!("pack-seed-{i}").as_bytes(),
            )
            .unwrap();
        ids.push((id, payload));
    }
    for (id, payload) in &ids {
        assert_eq!(&store.read_object(*id).unwrap(), payload);
    }
    let report = store.verify(true).unwrap();
    assert!(
        report.problems.is_empty(),
        "verify problems: {:?}",
        report.problems
    );
    let diagnostics = store.backend_diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("packed blobs:")),
        "diagnostics: {diagnostics:?}"
    );
    let gc = store.gc(true).unwrap();
    assert!(gc.problems.is_empty(), "gc problems: {:?}", gc.problems);
    for (id, payload) in &ids {
        assert_eq!(&store.read_object(*id).unwrap(), payload);
    }
    drop(store);

    // Reopen read-only on the same packed backend and verify again.
    let reopened =
        Store::open_with_backend(&path, OpenMode::ReadOnly, packed_local(pack_options())).unwrap();
    let report = reopened.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    for (id, payload) in &ids {
        assert_eq!(&reopened.read_object(*id).unwrap(), payload);
    }
}

#[test]
fn pack_crash_proxy_yields_old_or_new() {
    for phase in ["pack_after_pack_object", "pack_after_catalog_segments"] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store");
        // The CLI child below runs with default pack options, so the store must
        // use the same policy for the catalog shape to match.
        let kind = packed_local(PackOptions::default());
        let mut store = Store::init_with_backend(&path, store_options(), kind.clone()).unwrap();
        let root = store.resolve_path("/").unwrap();
        let id = store
            .create_file_seeded(
                root,
                b"f",
                b"old\n",
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"pack-crash-seed",
            )
            .unwrap();
        let generation = store.generation();
        drop(store);

        let status = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .args(["--pack", "--store"])
            .arg(&path)
            .args([
                "write",
                "/f",
                "--offset",
                "0",
                "--text",
                "new content, longer than old\n",
            ])
            .env("WEFT_CRASH_PHASE", phase)
            .status()
            .expect("spawn weftfs");
        assert!(
            !status.success(),
            "child was expected to abort at phase {phase}"
        );

        let store =
            Store::open_with_backend(&path, OpenMode::ReadWrite, kind).expect("reopen after crash");
        let bytes = store.read_object(id).unwrap();
        assert!(
            bytes == b"old\n" || bytes == b"new content, longer than old\n",
            "phase {phase} produced a partial state: {bytes:?}"
        );
        if phase == "pack_after_catalog_segments" {
            // The pack and its segments are published but the root is not, so
            // the catalog is still the old whole catalog.
            assert_eq!(store.generation(), generation);
            assert_eq!(bytes, b"old\n");
        }
        let report = store.verify(true).unwrap();
        assert!(
            report.problems.is_empty(),
            "phase {phase} verify problems: {:?}",
            report.problems
        );
    }
}

#[test]
fn packed_plane_refuses_a_plane_without_range_reads() {
    let dir = tempdir().unwrap();
    let capabilities = Capabilities {
        true_range_reads: false,
        ..Capabilities::local_posix()
    };
    let mut inner = LocalBackend::with_capabilities(dir.path(), capabilities);
    BlobPlane::init_layout(&mut inner).unwrap();
    let catalog = LocalBackend::new(dir.path());
    match PackedBlobPlane::new(Box::new(inner), Box::new(catalog), PackOptions::default()) {
        Ok(_) => panic!("a plane without true range reads must be refused"),
        Err(err) => assert_eq!(err.class(), Class::UnsupportedVersion, "{err}"),
    }
}

#[test]
fn backend_kind_refuses_packing_a_weak_plane() {
    let capabilities = Capabilities {
        true_range_reads: false,
        ..Capabilities::local_posix()
    };
    let weak = BackendKind::Posix { capabilities };
    match BackendKind::packed(weak, PackOptions::default()) {
        Ok(_) => panic!("packing a weak plane must be refused"),
        Err(err) => assert_eq!(err.class(), Class::UnsupportedVersion, "{err}"),
    }
}

// ---------------------------------------------------------------------------
// N15: catalog amplification
// ---------------------------------------------------------------------------

/// A packed plane now publishes the catalog **once per flush**, not once per
/// sealed pack, and the final catalog is identical either way.
///
/// The pre-N15 sequential writer republished every touched segment on every
/// seal. With `catalog_segments` segments and `packs` seals that is up to
/// `packs * segments` segment writes, each carrying the whole merged segment,
/// and every superseded segment stays on disk as an orphan. This test writes the
/// same members two ways and requires (a) the same final root, (b) at most one
/// publication per segment for the batched run, (c) no orphan segment blobs, and
/// (d) that the per-put-flush run really did publish many more segments.
#[test]
fn the_catalog_is_published_once_per_flush_and_ends_identical() {
    let options = PackOptions {
        pack_target_bytes: 8192,
        member_threshold_bytes: 2048,
        max_members_per_pack: 64,
        max_catalog_bytes: 4 * 1024 * 1024,
        catalog_segments: 8,
    };
    let count = 400u32;
    // Members are 1000 bytes and a pack holds 8192, so a pack fills after
    // exactly 8 members. Flushing on those boundaries is the *old* schedule: it
    // seals and catalogues each pack as it fills. Both runs therefore produce
    // the same packs, and only the catalog publication count may differ.
    let run = |flush_each_pack: bool| {
        let dir = tempdir().unwrap();
        let mut plane = plane_at(dir.path(), options);
        for i in 0..count {
            plane
                .put_if_absent(&member32(i), &body(1000, i as u8))
                .unwrap();
            if flush_each_pack && (i + 1) % 8 == 0 {
                plane.flush().unwrap();
            }
        }
        plane.flush().unwrap();
        let stats = plane.pack_stats();
        let catalog = LocalBackend::new(dir.path());
        let root = catalog
            .read_control(&Key::pack_catalog_root())
            .unwrap()
            .expect("catalog root");
        let segments_on_disk = catalog.list(&Key::prefix(KeyKind::PackCatalog)).unwrap().len();
        // Every member is still readable after the batched publication.
        for i in 0..count {
            let bytes = plane.get(&member32(i)).unwrap();
            assert_eq!(bytes, body(1000, i as u8), "member {i} differs");
        }
        std::mem::drop(plane);
        (
            root.bytes,
            stats.catalog_segment_publishes,
            stats.sealed_packs,
            segments_on_disk,
        )
    };
    let batched = run(false);
    let per_pack = run(true);
    eprintln!(
        "catalog amplification: batched publishes={} packs={} segment_blobs={}; \
         per-seal publishes={} packs={} segment_blobs={}",
        batched.1, batched.2, batched.3, per_pack.1, per_pack.2, per_pack.3
    );
    assert_eq!(
        batched.0, per_pack.0,
        "the final catalog root must not depend on publication batching"
    );
    assert_eq!(
        batched.2, per_pack.2,
        "pack layout (sealed packs) must not depend on publication batching"
    );
    assert!(
        batched.1 <= options.catalog_segments as u64,
        "batched run published {} segments for {} segments of catalog",
        batched.1,
        options.catalog_segments
    );
    assert!(
        batched.3 <= options.catalog_segments as usize,
        "batched run left {} segment blobs on disk for {} segments",
        batched.3,
        options.catalog_segments
    );
    assert!(
        per_pack.1 >= batched.1 * 4,
        "the per-pack-seal control should publish far more segments: {} vs {}",
        per_pack.1,
        batched.1
    );
    assert!(
        per_pack.3 > batched.3,
        "the per-pack-seal control should leave orphan segments: {} vs {}",
        per_pack.3,
        batched.3
    );
}

/// A packed store and a flat store that receive the same writes publish the same
/// head, the same manifests and the same content roots. Packing changes physical
/// layout only.
#[test]
fn packing_does_not_change_state_or_identity() {
    let run = |packed: bool| {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store");
        let backend = if packed {
            packed_local(pack_options())
        } else {
            BackendKind::Local
        };
        let mut store = Store::init_with_backend(&path, store_options(), backend).unwrap();
        let root = store.resolve_path("/").unwrap();
        let sub = store
            .mkdir_seeded(root, b"d", 0o755, 0, 0, &[9u8; 32])
            .unwrap();
        let mut ids = Vec::new();
        for i in 0..12u8 {
            let payload = body(300 + i as usize * 11, i);
            let id = store
                .create_file_seeded(
                    sub,
                    format!("f{i}").as_bytes(),
                    &payload,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("seed-{i}").as_bytes(),
                )
                .unwrap();
            ids.push((id, payload));
        }
        let head = store.head();
        let generation = store.generation();
        let verify = store.verify(true).unwrap();
        assert!(verify.problems.is_empty(), "{:?}", verify.problems);
        for (id, payload) in &ids {
            assert_eq!(&store.read_object(*id).unwrap(), payload);
        }
        let strategies: Vec<u64> = ids.iter().map(|(id, _)| *id.as_bytes().last().unwrap() as u64).collect();
        (head, generation, strategies)
    };
    let flat = run(false);
    let packed = run(true);
    assert_eq!(flat.0, packed.0, "head differs between flat and packed");
    assert_eq!(flat.1, packed.1, "generation differs");
    assert_eq!(flat.2, packed.2, "object identity stream differs");
}
