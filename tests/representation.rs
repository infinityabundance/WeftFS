//! Experimental codec-backed representation manifests (`weft.manifest.v2`).
//!
//! These tests exercise the opt-in `RepresentationMode::CodecAuto` path and
//! prove that the default `Raw` path still emits the frozen v1 manifest bytes.
//! They are Rust-store tests only; Go parity is explicitly out of scope.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use weftfs::format::{Manifest, Representation, RepresentationKind, MANIFEST_SCHEMA};
use weftfs::identity::{self, Hash32};
use weftfs::recovery::{manifest_path, read_manifest};
use weftfs::representation::{codec_chunk_decode_count, reset_codec_chunk_decode_count};
use weftfs::store::{OpenMode, Options, RepresentationMode, Store};

fn raw_opts(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn codec_opts(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        representation: RepresentationMode::CodecAuto,
        codec_min_bytes: 4_096,
        codec_delta_margin_percent: 10,
    }
}

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn compressible_text(len: usize) -> Vec<u8> {
    let line = b"weftfs representation manifest: the same sentence repeated.\n";
    let mut out = Vec::with_capacity(len);
    while out.len() + line.len() <= len {
        out.extend_from_slice(line);
    }
    out.resize(len, b' ');
    out
}

fn near_duplicate(base: &[u8], stride: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut index = 0;
    while index < out.len() {
        out[index] ^= 0x5a;
        index += stride;
    }
    out
}

fn put_manifest(root: &Path, manifest: &Manifest) -> Hash32 {
    let bytes = manifest.encode().unwrap();
    let id = identity::manifest_id(&bytes);
    let path = manifest_path(root, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();
    id
}

fn put_frame(root: &Path, frame: &[u8]) -> Hash32 {
    let id = identity::h(identity::MANIFEST_DOMAIN, frame);
    let path = manifest_path(root, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, frame).unwrap();
    id
}

#[test]
fn raw_mode_writes_the_frozen_v1_manifest_layout() {
    let bytes = deterministic_bytes(4 * 1024);
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();

    let mut a = Store::init(first.path(), raw_opts(1024)).unwrap();
    let root = a.resolve_path("/").unwrap();
    let a_id = a
        .create_file_seeded(
            root,
            b"f",
            &bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"raw-seed",
        )
        .unwrap();
    let a_record = a.object_meta(a_id).unwrap();

    let mut b = Store::init(second.path(), raw_opts(1024)).unwrap();
    let root = b.resolve_path("/").unwrap();
    let b_id = b
        .create_file_seeded(
            root,
            b"f",
            &bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"raw-seed",
        )
        .unwrap();
    let b_record = b.object_meta(b_id).unwrap();

    // Deterministic identity and byte-identical manifests across two stores.
    assert_eq!(a.head(), b.head());
    assert_eq!(a_record.content, b_record.content);
    let a_manifest_bytes = fs::read(manifest_path(a.root(), a_record.content)).unwrap();
    let b_manifest_bytes = fs::read(manifest_path(b.root(), b_record.content)).unwrap();
    assert_eq!(a_manifest_bytes, b_manifest_bytes);

    let manifest = read_manifest(a.root(), a_record.content).unwrap();
    assert!(manifest.representation.is_none());

    let value = weftfs::wcb_util::decode(&a_manifest_bytes).unwrap();
    let Value::Map(map) = value else {
        panic!("manifest is not a WCB map");
    };
    assert_eq!(
        map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA.to_owned()))
    );
    assert!(
        !map.contains_key("representation"),
        "Raw mode must not write a representation key"
    );

    // Recompute the raw chunk layout independently.
    let expected: Vec<(Hash32, u64)> = bytes
        .chunks(1024)
        .map(|chunk| {
            (
                identity::h(identity::CHUNK_DOMAIN, chunk),
                chunk.len() as u64,
            )
        })
        .collect();
    let actual: Vec<(Hash32, u64)> = manifest
        .chunks
        .iter()
        .map(|chunk| (chunk.hash, chunk.len))
        .collect();
    assert_eq!(actual, expected);

    let report = a.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn codec_auto_falls_back_to_a_legal_v2_raw_manifest() {
    // Deterministic pseudo-random bytes do not compress, so the writer must
    // choose the v2 `kind:"raw"` fallback.
    let bytes = deterministic_bytes(8 * 1024);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(root, b"r", &bytes, 0o644, 0, 0, BTreeMap::new(), b"raw-v2")
        .unwrap();
    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.expect("v2 manifest");
    assert_eq!(rep.kind, RepresentationKind::Raw);
    assert_eq!(rep.strategy, "raw");
    assert!(rep.frame.is_none());
    assert!(rep.dependencies.is_empty());
    assert_eq!(rep.decoded_size, bytes.len() as u64);
    assert_eq!(store.read_object(id).unwrap(), bytes);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks() {
    let bytes = compressible_text(256 * 1024);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(64 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"c",
            &bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"codec-seed",
        )
        .unwrap();
    let record = store.object_meta(id).unwrap();
    assert_eq!(record.size, bytes.len() as u64);

    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.clone().expect("v2 manifest");
    assert_eq!(rep.kind, RepresentationKind::Codec);
    assert_eq!(rep.strategy, "zstd");
    assert!(rep.dependencies.is_empty());
    let frame = rep.frame.expect("frame id");
    let frame_len = fs::metadata(manifest_path(store.root(), frame))
        .unwrap()
        .len();
    assert!(
        frame_len < bytes.len() as u64,
        "codec frame {frame_len} should be smaller than raw {} bytes",
        bytes.len()
    );

    assert_eq!(store.read_object(id).unwrap(), bytes);
    for (offset, length) in [
        (0u64, 4096u64),
        (123_456, 4096),
        (bytes.len() as u64 - 4096, 4096),
    ] {
        let got = store.read_range(id, offset, length).unwrap();
        let start = offset as usize;
        let stop = start + length as usize;
        assert_eq!(got, &bytes[start..stop], "range at {offset}");
    }
    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    assert_eq!(report.checked_representations, 1);

    // A 1 KiB read must not decode all four 64 KiB chunks.
    reset_codec_chunk_decode_count();
    let got = store.read_range(id, 100_000, 1024).unwrap();
    assert_eq!(got, &bytes[100_000..101_024]);
    let decoded = codec_chunk_decode_count();
    assert_eq!(
        decoded, 1,
        "expected exactly one decoded chunk, got {decoded}"
    );

    // A full read decodes every chunk.
    reset_codec_chunk_decode_count();
    store.read_object(id).unwrap();
    assert_eq!(codec_chunk_decode_count(), 4);
}

#[test]
fn delta_uses_a_causal_base_and_beats_an_independent_zstd_frame() {
    let v1 = deterministic_bytes(256 * 1024);
    let v2 = near_duplicate(&v1, 100);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(64 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(root, b"d", &v1, 0o644, 0, 0, BTreeMap::new(), b"delta-seed")
        .unwrap();
    let v1_record = store.object_meta(id).unwrap();

    let v2_version = store
        .write_file_causal(id, 0, &v2, &[v1_record.version])
        .unwrap();
    assert_ne!(v2_version, v1_record.version);
    assert_eq!(store.read_object(id).unwrap(), v2);

    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.clone().expect("v2 manifest");
    assert_eq!(rep.kind, RepresentationKind::Codec);
    assert_eq!(rep.strategy, "delta");
    assert_eq!(rep.dependencies, vec![v1_record.content]);

    let frame_len = fs::metadata(manifest_path(store.root(), rep.frame.unwrap()))
        .unwrap()
        .len();
    let zstd = weftcodec::modes::encode_zstd(&v2, 3, Some(64 * 1024), &Limits::default()).unwrap();
    assert!(
        frame_len < zstd.len() as u64,
        "delta frame {frame_len} should beat zstd {}",
        zstd.len()
    );

    for (offset, length) in [
        (0u64, 4096u64),
        (77_777, 4096),
        (v2.len() as u64 - 4096, 4096),
    ] {
        let got = store.read_range(id, offset, length).unwrap();
        let start = offset as usize;
        let stop = start + length as usize;
        assert_eq!(got, &v2[start..stop], "range at {offset}");
    }
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn missing_dependency_fails_closed_and_verify_reports_it() {
    let v1 = deterministic_bytes(128 * 1024);
    let v2 = near_duplicate(&v1, 100);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(64 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"m",
            &v1,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"missing-seed",
        )
        .unwrap();
    let v1_record = store.object_meta(id).unwrap();
    store
        .write_file_causal(id, 0, &v2, &[v1_record.version])
        .unwrap();
    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.clone().unwrap();
    let base_root = rep.dependencies[0];

    fs::remove_file(manifest_path(store.root(), base_root)).unwrap();

    let full = store.read_object(id).unwrap_err();
    assert_eq!(full.class_name(), "Corrupt", "unexpected error: {full}");
    let ranged = store.read_range(id, 0, 32).unwrap_err();
    assert_eq!(ranged.class_name(), "Corrupt", "unexpected error: {ranged}");

    let report = store.verify(true).unwrap();
    assert!(!report.ok, "verify must report the missing dependency");
    let base_hex = base_root.to_hex();
    assert!(
        report.problems.iter().any(|problem| {
            problem.detail.contains(&base_hex)
                || problem.detail.contains("codec")
                || problem.detail.contains("missing")
        }),
        "no problem names the missing dependency: {:?}",
        report.problems
    );
}

#[test]
fn gc_retains_codec_dependencies_of_a_live_dependent() {
    let v1 = deterministic_bytes(64 * 1024);
    let v2 = near_duplicate(&v1, 100);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(64 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let base = store
        .create_file_seeded(root, b"base", &v1, 0o644, 0, 0, BTreeMap::new(), b"gc-base")
        .unwrap();
    let base_record = store.object_meta(base).unwrap();
    let derived = store
        .create_file_seeded_causal(
            root,
            b"derived",
            &v2,
            0o644,
            0,
            0,
            BTreeMap::new(),
            b"gc-derived",
            &[base_record.version],
        )
        .unwrap();
    let derived_manifest =
        read_manifest(store.root(), store.object_meta(derived).unwrap().content).unwrap();
    let dependency = derived_manifest
        .representation
        .clone()
        .unwrap()
        .dependencies[0];
    assert_eq!(dependency, base_record.content);
    assert_eq!(
        derived_manifest.representation.clone().unwrap().strategy,
        "delta"
    );

    store.unlink(root, b"base").unwrap();
    let report = store.gc(true).unwrap();
    assert!(
        manifest_path(store.root(), dependency).exists(),
        "GC deleted a live dependent's base manifest; report {report:?}"
    );
    assert_eq!(store.read_object(derived).unwrap(), v2);

    store.unlink(root, b"derived").unwrap();
    store.gc(true).unwrap();
    assert!(
        !manifest_path(store.root(), dependency).exists(),
        "GC should reclaim the dependency once no root references it"
    );
}

#[test]
fn dependency_depth_bound_is_enforced() {
    let dir = tempdir().unwrap();
    let store = Store::init(dir.path(), raw_opts(1024)).unwrap();

    // Build a chain bottom-up. Each level is a DELTA frame against the level
    // below, so materialising the top requires walking every link.
    let mut contents: Vec<Vec<u8>> = (0..12)
        .map(|index| vec![b'a' + (index as u8 % 26); 64])
        .collect();
    contents.reverse();

    let limits = Limits::default();
    let bottom = contents.last().unwrap().clone();
    let bottom_frame = weftcodec::modes::encode_zstd(&bottom, 3, Some(64), &limits).unwrap();
    let bottom_id = put_frame(store.root(), &bottom_frame);
    let mut below = put_manifest(
        store.root(),
        &Manifest {
            size: bottom.len() as u64,
            chunk_size: 64,
            chunks: Vec::new(),
            content_hash: identity::content_hash(&bottom),
            representation: Some(Representation {
                kind: RepresentationKind::Codec,
                frame: Some(bottom_id),
                dependencies: Vec::new(),
                strategy: "zstd".to_owned(),
                decoded_size: bottom.len() as u64,
                tensor: None,
            }),
        },
    );

    for window in contents.windows(2).rev() {
        let (base, target) = (&window[1], &window[0]);
        let frame = weftcodec::modes::encode_delta(target, base, &limits).unwrap();
        let frame_id = put_frame(store.root(), &frame);
        below = put_manifest(
            store.root(),
            &Manifest {
                size: target.len() as u64,
                chunk_size: 64,
                chunks: Vec::new(),
                content_hash: identity::content_hash(target),
                representation: Some(Representation {
                    kind: RepresentationKind::Codec,
                    frame: Some(frame_id),
                    dependencies: vec![below],
                    strategy: "delta".to_owned(),
                    decoded_size: target.len() as u64,
                    tensor: None,
                }),
            },
        );
    }

    // The chain is 11 dependencies deep, beyond the bound of 8.
    let top = below;
    let err = store.read_content(top, 0, 64).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt", "unexpected error: {err}");
    assert!(
        err.message().contains("depth"),
        "unexpected depth error: {err}"
    );

    // A content-addressed cycle is a hash fixed point and is not constructible.
    // The reader still keeps a visited set and rejects one if a future writer
    // ever produces it; the depth bound above is the reachable proof.
}

#[test]
fn codec_store_reopens_and_raw_mode_preserves_v1_reads() {
    let bytes = compressible_text(64 * 1024);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), codec_opts(64 * 1024)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file(root, b"x", &bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let record = store.object_meta(id).unwrap();
    assert_eq!(
        read_manifest(store.root(), record.content)
            .unwrap()
            .representation
            .unwrap()
            .kind,
        RepresentationKind::Codec
    );
    drop(store);

    // A default (`Raw`) reader opens a CodecAuto store and reads its v2
    // codec-backed content without knowing the writer's option.
    let reopened = Store::open(dir.path(), OpenMode::ReadOnly).unwrap();
    assert_eq!(reopened.read_object(id).unwrap(), bytes);
    assert_eq!(reopened.read_range(id, 0, 1024).unwrap(), &bytes[..1024]);
    assert!(reopened.verify(true).unwrap().ok);
    drop(reopened);

    // Writing through a Raw-mode opener materialises the codec version and
    // publishes a frozen v1 manifest again: a Raw store never writes v2.
    let mut raw =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, raw_opts(64 * 1024)).unwrap();
    let mut model = bytes.clone();
    model[10..13].copy_from_slice(b"XYZ");
    raw.write_file(id, 10, b"XYZ").unwrap();
    assert_eq!(raw.read_object(id).unwrap(), model);
    let record = raw.object_meta(id).unwrap();
    let manifest = read_manifest(raw.root(), record.content).unwrap();
    assert!(
        manifest.representation.is_none(),
        "Raw mode must not write a v2 manifest"
    );
    let manifest_bytes = fs::read(manifest_path(raw.root(), record.content)).unwrap();
    let Value::Map(map) = weftfs::wcb_util::decode(&manifest_bytes).unwrap() else {
        panic!("manifest is not a WCB map");
    };
    assert_eq!(
        map.get("schema"),
        Some(&Value::Str(MANIFEST_SCHEMA.to_owned()))
    );
    assert!(raw.verify(true).unwrap().ok);
}
