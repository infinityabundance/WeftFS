//! Bounded parallel mode-15 tile decode and automatic tile-shape planning.
//!
//! The mode-15 wire layout is unchanged: these tests only exercise the read
//! scheduler and the additive planner option.

use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;
use weftcodec::{Frame, Limits, Region};
use weftfs::error::Class;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    derive_auto_tile_shape, TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec,
};
use weftfs::TileDecodeOptions;

const SHAPE: [u64; 2] = [64, 48];
const TILE: [u64; 2] = [16, 16];
const ELEMENT_BYTES: usize = 2;
// 4 x 3 full tiles.
const TILES: usize = 12;
const TILE_BYTES: usize = (TILE[0] * TILE[1]) as usize * ELEMENT_BYTES;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn tensor_bytes() -> Vec<u8> {
    let len = SHAPE.iter().product::<u64>() as usize * ELEMENT_BYTES;
    bytes(len, 0x71ed_c0de)
}

fn write_tiled(store: &mut Store, root: weftfs::Hash32) -> (weftfs::Hash32, Vec<u8>) {
    let data = tensor_bytes();
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::TensorTiled),
        tile_shape: Some(TILE.to_vec()),
        ..TensorWriteSpec::default()
    };
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"weight",
            "bf16",
            &SHAPE,
            &data,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .unwrap();
    (id, data)
}

fn open_fixture() -> (
    tempfile::TempDir,
    Store,
    weftfs::Hash32,
    weftfs::Hash32,
    Vec<u8>,
) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let (id, data) = write_tiled(&mut store, root);
    let meta = store.tensor_metadata(id).unwrap();
    assert_eq!(meta.strategy, "tensor-tiled");
    assert_eq!(meta.tile_shape.as_deref(), Some(TILE.as_slice()));
    (dir, store, root, id, data)
}

fn tile_options(threads: usize, max_inflight_bytes: usize) -> TileDecodeOptions {
    TileDecodeOptions {
        threads,
        max_inflight_bytes,
    }
}

#[test]
fn full_object_decode_is_identical_at_1_2_and_4_threads() {
    let (_dir, store, _root, id, expected) = open_fixture();
    let mut hashes = Vec::new();
    let mut tile_counts = Vec::new();
    for threads in [1usize, 2, 4] {
        let options = tile_options(threads, 64 * 1024 * 1024);
        let (got, stats) = store
            .read_tensor_with_options_and_stats(id, &options)
            .unwrap();
        assert_eq!(got, expected, "threads={threads}");
        assert_eq!(
            stats.effective_threads,
            weftfs::effective_threads(threads),
            "threads={threads}"
        );
        assert_eq!(stats.tiles, TILES, "threads={threads}");
        assert_eq!(stats.oversized_tiles, 0, "threads={threads}");
        hashes.push(blake3::hash(&got).to_hex().to_string());
        tile_counts.push(stats.tiles);
    }
    assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(tile_counts.windows(2).all(|pair| pair[0] == pair[1]));
    // The default API is the sequential path.
    assert_eq!(store.read_tensor(id).unwrap(), expected);
}

#[test]
fn region_reads_decode_only_intersecting_tiles_at_every_thread_count() {
    let (_dir, store, _root, id, expected) = open_fixture();
    // Region shapes chosen against the 4 x 3 tile grid: a one-row band touches
    // the three tiles of the first tile row; a single element touches one tile.
    let cases: [(&str, Region, u64); 3] = [
        (
            "whole",
            Region::new(vec![0, 0], SHAPE.to_vec()),
            TILES as u64,
        ),
        ("band", Region::new(vec![0, 0], vec![16, 48]), 3),
        ("element", Region::new(vec![0, 0], vec![1, 1]), 1),
    ];
    for (label, region, expected_tiles) in cases {
        let expected_bytes = expected_region(&expected, &region);
        let mut touched = Vec::new();
        for threads in [1usize, 2, 4] {
            let options = tile_options(threads, 64 * 1024 * 1024);
            let (read, stats) = store
                .read_tensor_region_with_options_and_stats(id, &region, &options)
                .unwrap();
            assert_eq!(read.bytes, expected_bytes, "{label} threads={threads}");
            assert_eq!(
                read.units_touched, expected_tiles,
                "{label} threads={threads}"
            );
            assert_eq!(
                stats.tiles as u64, expected_tiles,
                "{label} threads={threads}"
            );
            assert_eq!(
                stats.effective_threads,
                weftfs::effective_threads(threads),
                "{label} threads={threads}"
            );
            touched.push(read.units_touched);
        }
        assert!(touched.windows(2).all(|pair| pair[0] == pair[1]));
    }
}

#[test]
fn inflight_bound_is_respected() {
    let (_dir, store, _root, id, _expected) = open_fixture();
    // Two tile buffers may be reserved at once; never more than the bound.
    let bound = TILE_BYTES * 2;
    let options = tile_options(4, bound);
    let (_bytes, stats) = store
        .read_tensor_with_options_and_stats(id, &options)
        .unwrap();
    assert_eq!(stats.tiles, TILES);
    assert_eq!(stats.max_inflight_bytes, bound);
    assert_eq!(stats.oversized_tiles, 0);
    assert!(
        stats.observed_max_inflight_bytes <= bound,
        "observed {} > bound {bound}",
        stats.observed_max_inflight_bytes
    );
    assert!(stats.observed_max_inflight_tasks <= 2);

    // A bound below one tile makes every tile oversized, and each runs alone.
    let options = tile_options(4, TILE_BYTES - 1);
    let (_bytes, stats) = store
        .read_tensor_with_options_and_stats(id, &options)
        .unwrap();
    assert!(stats.oversized_tiles > 0);
    assert!(stats.observed_max_inflight_tasks <= 1);
}

#[test]
fn repeated_parallel_decode_is_deterministic() {
    let (_dir, store, _root, id, expected) = open_fixture();
    let options = tile_options(4, 64 * 1024 * 1024);
    let first = store
        .read_tensor_with_options_and_stats(id, &options)
        .unwrap();
    for _ in 0..20 {
        let (got, stats) = store
            .read_tensor_with_options_and_stats(id, &options)
            .unwrap();
        assert_eq!(got, first.0);
        assert_eq!(got, expected);
        assert_eq!(stats.tiles, first.1.tiles);
    }
}

fn corrupt_first_tile(store: &Store, id: weftfs::Hash32) -> PathBuf {
    let record = store.object_meta(id).unwrap();
    let manifest = weftfs::recovery::read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.expect("tensor representation");
    let frame_id = rep.frame.expect("frame id");
    let hex = frame_id.to_hex();
    let path = store.root().join("manifests").join(&hex[0..2]).join(&hex);
    let mut frame = fs::read(&path).unwrap();
    let limits = Limits::default();
    let parsed = Frame::parse(&frame, &limits).unwrap();
    let descriptor = weftcodec::parse_tiled_tensor(&parsed, &limits).unwrap();
    let payload_base = frame.len() - descriptor.payload.len();
    let entry = &descriptor.meta.tiles[0];
    let at = payload_base + entry.offset as usize + (entry.compressed_len as usize / 2);
    frame[at] ^= 0xff;
    fs::write(&path, &frame).unwrap();
    path
}

#[test]
fn corrupted_tile_fails_closed_identically_at_every_thread_count() {
    let (_dir, store, _root, id, _expected) = open_fixture();
    // Sanity: the object is readable before corruption.
    store.read_tensor(id).unwrap();
    corrupt_first_tile(&store, id);

    let mut region_errors = Vec::new();
    let mut object_errors = Vec::new();
    for threads in [1usize, 2, 4] {
        let options = tile_options(threads, 64 * 1024 * 1024);
        let region = Region::new(vec![0, 0], vec![16, 16]);
        let err = store
            .read_tensor_region_with_options_and_stats(id, &region, &options)
            .unwrap_err();
        // A flipped Huffman byte is caught as a structural decode error or a
        // tile-root mismatch; both are fail-closed.
        assert!(
            matches!(
                err.class(),
                Class::Corrupt | Class::Malformed | Class::NonCanonical
            ),
            "region threads={threads} class={:?}: {err}",
            err.class()
        );
        region_errors.push(format!("{}: {}", err.class_name(), err.message()));
        let err = store
            .read_tensor_with_options_and_stats(id, &options)
            .unwrap_err();
        // The whole-object path verifies the frame blob first, so the mutated
        // bytes are rejected as a hash mismatch before any tile is trusted.
        assert_eq!(err.class(), Class::Corrupt, "object threads={threads}");
        object_errors.push(format!("{}: {}", err.class_name(), err.message()));
    }
    // One error string per path, independent of the worker count.
    assert!(region_errors.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(object_errors.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn auto_tile_derivation_divides_both_extents() {
    let shape = [151_936u64, 896];
    for budget in [32 * 1024u64, 64 * 1024] {
        let tile = derive_auto_tile_shape(
            &shape,
            weftfs::representation::TensorElementDtype::Bf16,
            budget,
        )
        .unwrap();
        assert_eq!(tile.len(), 2);
        assert_eq!(shape[0] % tile[0], 0, "row tile {tile:?}");
        assert_eq!(shape[1] % tile[1], 0, "column tile {tile:?}");
        let bytes = tile[0] * tile[1] * 2;
        assert!(bytes <= budget, "tile {tile:?} uses {bytes} B > {budget}");
        // The largest dividing pair at these budgets is exact for this shape.
        if budget == 32 * 1024 {
            assert_eq!(tile, vec![128, 128]);
        } else {
            assert_eq!(tile, vec![128, 224]);
        }
    }
}

#[test]
fn auto_tile_derivation_errors_when_nothing_fits() {
    // One bf16 element is two bytes, so a one-byte budget admits no tile.
    let err = derive_auto_tile_shape(
        &[10, 6],
        weftfs::representation::TensorElementDtype::Bf16,
        1,
    )
    .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
    assert!(err.message().contains("10") && err.message().contains("6"));

    // Rank is not 2: no silent padding rule is guessed.
    let err = derive_auto_tile_shape(
        &[4, 4, 4],
        weftfs::representation::TensorElementDtype::Bf16,
        1024,
    )
    .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument);
}

#[test]
fn auto_tile_planner_is_deterministic_and_records_the_shape() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let data = tensor_bytes();
    let shape = SHAPE;
    let options = TensorPlanOptions {
        auto_tile_bytes: Some(1024),
        ..TensorPlanOptions::default()
    };
    let first = store
        .plan_tensor(
            "bf16",
            &shape,
            &data,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .unwrap();
    let second = store
        .plan_tensor(
            "bf16",
            &shape,
            &data,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .unwrap();
    assert_eq!(first, second);
    let tiled = first
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::TensorTiled)
        .expect("tiled candidate");
    assert!(tiled.applicable, "{tiled:?}");
    assert!(
        tiled.detail.contains("auto from a 1024-byte budget"),
        "{tiled:?}"
    );
    let expected_tile = derive_auto_tile_shape(
        &shape,
        weftfs::representation::TensorElementDtype::Bf16,
        1024,
    )
    .unwrap();
    assert_eq!(expected_tile, vec![32, 16]);
    assert!(tiled.detail.contains(&format!("{expected_tile:?}")));

    // Writing with the auto planner and forcing the tiled candidate records the
    // derived shape in the object's tensor metadata.
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::TensorTiled),
        plan: options,
        ..TensorWriteSpec::default()
    };
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"auto",
            "bf16",
            &shape,
            &data,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .unwrap();
    let meta = store.tensor_metadata(id).unwrap();
    assert_eq!(meta.tile_shape.as_deref(), Some(expected_tile.as_slice()));
    assert_eq!(store.read_tensor(id).unwrap(), data);
    let four = tile_options(4, 64 * 1024 * 1024);
    assert_eq!(store.read_tensor_with_options(id, &four).unwrap(), data);
}

fn expected_region(full: &[u8], region: &Region) -> Vec<u8> {
    let element = ELEMENT_BYTES;
    let mut out = Vec::new();
    for row in region.start[0]..region.end[0] {
        for col in region.start[1]..region.end[1] {
            let at = ((row * SHAPE[1] + col) as usize) * element;
            out.extend_from_slice(&full[at..at + element]);
        }
    }
    out
}
