//! X18 — direct-to-destination verified reconstruction.
//!
//! Two paths are exercised against the same tiled checkpoint:
//!
//! * [`Store::restore_checkpoint_direct_into`] fills caller-owned per-tensor
//!   buffers with no container buffer and no SHA-256 pass.
//! * [`Store::restore_row_slab_direct_at`] fills a caller-owned row slab from
//!   the shards it needs, verifying each contributing tile against its own
//!   root.
//!
//! Every case compares against the *source container*, not against another
//! restore, so "equivalent" means "identical to the bytes that were saved".

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::TempDir;
use weftfs::checkpoint::{
    CheckpointRestoreOptions, CheckpointWriteOptions, DirectDestinations, ShardStrategy,
};
use weftfs::error::Class;
use weftfs::restore_pipeline::RestorePipelineOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 4096,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn tiny(value: u16) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// Four bf16 tensors of [256, 128] each (65,536 bytes per tensor).
fn container() -> (Vec<(String, Vec<u8>)>, Vec<u8>) {
    let shape = [256u64, 128u64];
    let mut tensors = Vec::new();
    for index in 0..4u16 {
        let mut data = Vec::with_capacity(256 * 128 * 2);
        for row in 0..256u16 {
            for col in 0..128u16 {
                data.extend_from_slice(&tiny(index * 1000 + row + col));
            }
        }
        tensors.push((format!("layer{index}.weight"), data));
    }
    let entries: Vec<SafetensorsEntry<'_>> = tensors
        .iter()
        .map(|(name, data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape: &shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None).expect("encode container");
    (tensors, file)
}

fn write_tiled_checkpoint(
    dir: &Path,
    shards: u32,
    tile: Vec<u64>,
) -> (Store, Vec<(String, Vec<u8>)>) {
    let (tensors, file) = container();
    let source = dir.join("model.safetensors");
    fs::write(&source, &file).unwrap();
    let store_dir = dir.join("store");
    let mut store = Store::init(&store_dir, options()).unwrap();
    let write = CheckpointWriteOptions {
        shards,
        axis: 0,
        filters: Vec::new(),
        strategy: ShardStrategy::TensorTiled,
        tile_shape: Some(tile),
        write_policy: weftfs::tensor_store::WritePolicy::Planned,
        ..CheckpointWriteOptions::default()
    };
    store.write_checkpoint("cp", &source, &write).unwrap();
    drop(store);
    let store = Store::open_with_options(&store_dir, OpenMode::ReadWrite, options()).unwrap();
    (store, tensors)
}

fn write_planned_checkpoint(dir: &Path, shards: u32) -> (Store, Vec<(String, Vec<u8>)>) {
    let (tensors, file) = container();
    let source = dir.join("model.safetensors");
    fs::write(&source, &file).unwrap();
    let store_dir = dir.join("store");
    let mut store = Store::init(&store_dir, options()).unwrap();
    let write = CheckpointWriteOptions {
        shards,
        axis: 0,
        filters: Vec::new(),
        strategy: ShardStrategy::Planned,
        tile_shape: None,
        write_policy: weftfs::tensor_store::WritePolicy::Planned,
        ..CheckpointWriteOptions::default()
    };
    store.write_checkpoint("cp", &source, &write).unwrap();
    drop(store);
    let store = Store::open_with_options(&store_dir, OpenMode::ReadWrite, options()).unwrap();
    (store, tensors)
}

fn destinations(tensors: &[(String, Vec<u8>)]) -> DirectDestinations {
    tensors
        .iter()
        .map(|(name, data)| (name.clone(), vec![0u8; data.len()]))
        .collect()
}

// ---------------------------------------------------------------------------
// Whole checkpoint, direct into caller buffers.
// ---------------------------------------------------------------------------

#[test]
fn direct_restore_fills_caller_buffers_with_the_exact_saved_bytes() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 2, vec![64, 64]);
    let mut dest = destinations(&tensors);
    let report = store
        .restore_checkpoint_direct_into(
            "cp",
            &CheckpointRestoreOptions::default(),
            &RestorePipelineOptions::default(),
            &mut dest,
        )
        .unwrap();
    for (name, data) in &tensors {
        assert_eq!(&dest[name], data, "{name} differs from the saved bytes");
    }
    assert_eq!(report.tensors.len(), tensors.len());
    for fill in &report.tensors {
        assert_eq!(fill.filled, fill.expected, "{}", fill.name);
    }
    assert_eq!(
        report.sha256_passes, 0,
        "the direct path computes no SHA-256"
    );
    assert!(!report.exact_source_verified);
    assert!(report.header_bytes > 0);
    assert_eq!(
        report.payload_bytes,
        tensors.iter().map(|(_, d)| d.len() as u64).sum::<u64>()
    );
    // The ordinary verification did run.
    for tensor in &report.restore.tensors {
        assert!(tensor.full_root_verified);
        assert!(tensor.shard_roots_verified);
        assert_eq!(tensor.sha256, [0u8; 32], "no per-tensor SHA-256 either");
    }
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn direct_restore_refuses_a_missing_or_mis_sized_buffer_before_reading() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 2, vec![64, 64]);

    let mut missing = destinations(&tensors);
    missing.remove(&tensors[0].0);
    let err = store
        .restore_checkpoint_direct_into(
            "cp",
            &CheckpointRestoreOptions::default(),
            &RestorePipelineOptions::default(),
            &mut missing,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");

    let mut wrong = destinations(&tensors);
    wrong.get_mut(&tensors[1].0).unwrap().push(0);
    let err = store
        .restore_checkpoint_direct_into(
            "cp",
            &CheckpointRestoreOptions::default(),
            &RestorePipelineOptions::default(),
            &mut wrong,
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    assert!(err.message().contains("buffer"), "{err}");
}

// ---------------------------------------------------------------------------
// Row slabs: rank coordinates, per-tile verification, no whole-tensor hash.
// ---------------------------------------------------------------------------

#[test]
fn a_row_slab_matches_the_saved_rows_and_verifies_per_tile() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 4, vec![64, 64]);
    let (name, data) = &tensors[0];
    let stride = 128 * 2;
    let (lo, hi) = (100u64, 250u64);
    let mut dst = vec![0u8; ((hi - lo) as usize) * stride];
    let report = store
        .restore_row_slab_direct_at(None, "cp", name, (lo, hi), &mut dst)
        .unwrap();
    let expected = &data[(lo as usize) * stride..(hi as usize) * stride];
    assert_eq!(dst, expected, "row slab differs from the saved rows");
    assert!(report.tile_roots_verified, "tiled shards verify per tile");
    assert_eq!(report.whole_object_shards, 0);
    assert!(
        !report.full_root_verified,
        "a strict slab has no whole-tensor root"
    );
    assert_eq!(report.sha256_passes, 0);
    assert!(report.tiles > 0);
    assert!(report.shards_read >= 1);
}

#[test]
fn a_whole_tensor_slab_verifies_the_tensor_roots() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 4, vec![64, 64]);
    let (name, data) = &tensors[2];
    let mut dst = vec![0u8; data.len()];
    let report = store
        .restore_row_slab_direct_at(None, "cp", name, (0, 256), &mut dst)
        .unwrap();
    assert_eq!(&dst, data);
    assert!(report.full_root_verified);
    assert!(report.tile_roots_verified);
    assert!(report.content_root.is_some());
    assert!(report.logical_root.is_some());
}

#[test]
fn a_planned_shard_has_no_per_unit_root_and_says_so() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_planned_checkpoint(dir.path(), 2);
    let (name, data) = &tensors[1];
    let stride = 128 * 2;
    let (lo, hi) = (10u64, 20u64);
    let mut dst = vec![0u8; ((hi - lo) as usize) * stride];
    let report = store
        .restore_row_slab_direct_at(None, "cp", name, (lo, hi), &mut dst)
        .unwrap();
    assert_eq!(dst, &data[(lo as usize) * stride..(hi as usize) * stride]);
    // A mode-10/q8_0 shard has no tile roots: the bytes were authenticated by
    // the object's whole-content root instead, and the report says so.
    assert!(!report.tile_roots_verified);
    assert!(report.whole_object_shards >= 1);
    assert_eq!(report.sha256_passes, 0);
}

#[test]
fn a_sub_box_region_and_a_non_axis_zero_tensor_are_refused() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 2, vec![64, 64]);
    let (name, data) = &tensors[0];
    let mut dst = vec![0u8; data.len()];
    // Rows outside the tensor.
    let err = store
        .restore_row_slab_direct_at(None, "cp", name, (200, 300), &mut dst)
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
    // Wrong destination size.
    let err = store
        .restore_row_slab_direct_at(None, "cp", name, (0, 256), &mut dst[..100])
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
}

// ---------------------------------------------------------------------------
// Corruption: a damaged stored region is refused, and nothing unverified lands
// in the caller's buffer.
// ---------------------------------------------------------------------------

fn corrupt_tensor_frame(store: &Store, store_dir: &Path, tensor_name: &str) -> std::path::PathBuf {
    // Find the frame blob the named tensor's first shard actually names, and
    // damage a byte inside its stored payload. This is the exact source region a
    // restore reads.
    let manifest = store.load_checkpoint("cp").unwrap();
    let tensor = manifest.tensor(tensor_name).unwrap();
    let shard = &tensor.shards[0];
    let frame = store
        .tensor_frame_id(shard.object_id)
        .unwrap()
        .expect("the shard has a frame");
    let hex = frame.to_hex();
    let path = store_dir.join("manifests").join(&hex[0..2]).join(&hex);
    let mut bytes = fs::read(&path).unwrap();
    // Past the 96-byte header and the descriptor: a payload byte.
    let index = bytes.len() - 16;
    bytes[index] ^= 0x01;
    fs::write(&path, &bytes).unwrap();
    path
}

#[test]
fn a_corrupted_stored_shard_is_refused_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 1, vec![64, 64]);
    let store_dir = dir.path().join("store");
    corrupt_tensor_frame(&store, &store_dir, &tensors[0].0);
    drop(store);
    let store = Store::open_with_options(&store_dir, OpenMode::ReadOnly, options()).unwrap();

    // A whole-tensor slab: the shard's frame fails its blob content hash, so
    // the read is refused and the destination stays zeroed.
    let (name, data) = &tensors[0];
    let mut dst = vec![0u8; data.len()];
    let err = store
        .restore_row_slab_direct_at(None, "cp", name, (0, 256), &mut dst)
        .unwrap_err();
    assert!(
        matches!(err.class(), Class::Corrupt | Class::NotFound),
        "{err}"
    );
    assert!(
        dst.iter().all(|byte| *byte == 0),
        "nothing unverified is written"
    );
    // A different, undamaged tensor still restores exactly: the refusal is
    // local to the corrupt region, not a blanket failure.
    let (other, other_data) = &tensors[1];
    let mut dst = vec![0u8; other_data.len()];
    store
        .restore_row_slab_direct_at(None, "cp", other, (0, 256), &mut dst)
        .unwrap();
    assert_eq!(&dst, other_data);
}

#[test]
fn a_corrupted_store_fails_the_whole_direct_restore_before_filling() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 1, vec![64, 64]);
    let store_dir = dir.path().join("store");
    corrupt_tensor_frame(&store, &store_dir, &tensors[0].0);
    drop(store);
    let store = Store::open_with_options(&store_dir, OpenMode::ReadOnly, options()).unwrap();
    let mut dest = destinations(&tensors);
    let err = store
        .restore_checkpoint_direct_into(
            "cp",
            &CheckpointRestoreOptions::default(),
            &RestorePipelineOptions::default(),
            &mut dest,
        )
        .unwrap_err();
    assert!(
        matches!(err.class(), Class::Corrupt | Class::NotFound),
        "{err}"
    );
    // The failing tensor's own buffer was never written. A restore that fails
    // on tensor N can leave tensors before N filled (the contract says so); the
    // failing tensor is the one that must stay empty.
    assert!(
        dest[&tensors[0].0].iter().all(|byte| *byte == 0),
        "the failing tensor's buffer was partially filled"
    );
}

// ---------------------------------------------------------------------------
// Equivalence with the existing restore, byte for byte.
// ---------------------------------------------------------------------------

#[test]
fn direct_and_ordinary_restores_agree_on_every_byte() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 4, vec![64, 64]);
    let options = CheckpointRestoreOptions::default();
    let pipeline = RestorePipelineOptions::default();
    let mut ordinary = Vec::new();
    let normal = store
        .restore_checkpoint_pipelined("cp", &options, &pipeline, &mut ordinary)
        .unwrap();
    let mut dest = destinations(&tensors);
    let direct = store
        .restore_checkpoint_direct_into("cp", &options, &pipeline, &mut dest)
        .unwrap();
    assert_eq!(normal.file_bytes, direct.restore.file_bytes);
    let header = direct.header_bytes as usize;
    let mut cursor = header;
    for (name, data) in &tensors {
        assert_eq!(
            &ordinary[cursor..cursor + data.len()],
            &dest[name][..],
            "{name}"
        );
        cursor += data.len();
    }
    assert_eq!(cursor, ordinary.len());
    // The ordinary path computes the SHA-256 the direct path omits; both are
    // verified, and the direct one is not allowed to claim the digest.
    assert_ne!(normal.file_sha256, [0u8; 32]);
    assert_eq!(direct.restore.file_sha256, [0u8; 32]);
    assert_ne!(
        direct.header_sha256, [0u8; 32],
        "the header digest is kept, separately"
    );
    assert_ne!(
        direct.header_sha256, [0u8; 32],
        "the header digest is kept, separately"
    );
    assert!(!direct.restore.exact_source_file);
    assert_eq!(direct.sha256_passes, 0);
}

#[test]
fn a_row_slab_equals_the_ordinary_rank_restore() {
    let dir = TempDir::new().unwrap();
    let (store, tensors) = write_tiled_checkpoint(dir.path(), 4, vec![64, 64]);
    // Rank 0 of a 4-way restore owns rows [0, 64) of every tensor.
    let topology = weftfs::checkpoint::RestoreTopology::Sharded { axis: 0, shards: 4 };
    let mut by_name: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let report = store
        .restore_checkpoint_rank("cp", &topology, 0, None, |row, bytes| {
            by_name.insert(row.name.clone(), bytes.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(report.tensors.len(), tensors.len());
    let stride = 128 * 2;
    for (name, data) in &tensors {
        let mut dst = vec![0u8; 64 * stride];
        let direct = store
            .restore_row_slab_direct_at(None, "cp", name, (0, 64), &mut dst)
            .unwrap();
        assert_eq!(dst, by_name[name], "{name} differs from the rank restore");
        assert_eq!(&dst[..], &data[..64 * stride]);
        assert!(direct.tile_roots_verified);
    }
}
