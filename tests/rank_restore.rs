//! Rank-directed cross-topology restore (T053).
//!
//! A stored checkpoint is written 8-way on axis 0. For every target topology
//! `S in {1,2,4,8}` and every rank `r in 0..S`, the rank restore must fetch
//! exactly the stored shards whose slab intersects `r`'s destination extents,
//! and the bytes it materialises must equal the corresponding slice of a full
//! restore. The arithmetic is asserted against the writer's own `shard_shape`
//! and against `restore_checkpoint`'s topology proof (the same
//! `LogicalTensorRoot`).

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::checkpoint::{
    CheckpointRestoreOptions, CheckpointRestoreReport, CheckpointWriteOptions, RestoreTopology,
    ShardStrategy,
};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SHAPE: [u64; 3] = [64, 32, 16];
const TENSOR: &str = "weight";
const ELEMENT_BYTES: usize = 2;

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

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
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

fn tensor_len() -> usize {
    SHAPE.iter().product::<u64>() as usize * ELEMENT_BYTES
}

fn write_container(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let data = random_bytes(tensor_len(), 0x5eed_1234);
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let path = dir.join("rank3.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn write_eight_way(store: &mut Store, source: &Path) {
    store
        .write_checkpoint(
            "rank3",
            source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
}

fn setup() -> (tempfile::TempDir, Store, Vec<u8>, CheckpointRestoreReport) {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let source = write_container(&dir.path().join("src"));
    write_eight_way(&mut store, &source);
    let (container, report) = store
        .restore_checkpoint_bytes("rank3", &CheckpointRestoreOptions::default())
        .unwrap();
    assert_eq!(container.len(), report.file_bytes as usize);
    // `restore_checkpoint` writes the source header first; the tensor payload
    // is the trailing `tensor_len()` bytes of the container.
    let payload_offset = container.len() - tensor_len();
    (dir, store, container[payload_offset..].to_vec(), report)
}

/// Copy the region `[start, end)` out of a rank-3 row-major buffer.
fn slice_region(bytes: &[u8], start: &[u64], end: &[u64]) -> Vec<u8> {
    let dims = [SHAPE[0] as usize, SHAPE[1] as usize, SHAPE[2] as usize];
    let s = [start[0] as usize, start[1] as usize, start[2] as usize];
    let e = [end[0] as usize, end[1] as usize, end[2] as usize];
    let out_dims = [e[0] - s[0], e[1] - s[1], e[2] - s[2]];
    let mut out = Vec::with_capacity(out_dims.iter().product::<usize>() * ELEMENT_BYTES);
    for i in 0..out_dims[0] {
        for j in 0..out_dims[1] {
            let offset = (((s[0] + i) * dims[1] + (s[1] + j)) * dims[2] + s[2]) * ELEMENT_BYTES;
            let len = out_dims[2] * ELEMENT_BYTES;
            out.extend_from_slice(&bytes[offset..offset + len]);
        }
    }
    out
}

#[test]
fn every_rank_fetches_the_intersection_it_owns() {
    let (_dir, store, full, full_report) = setup();
    assert!(full_report.exact_source_file);
    let manifest = store.checkpoint_manifest("rank3").unwrap();
    let tensor = manifest.tensor(TENSOR).unwrap();
    let stored: Vec<u32> = tensor.shards.iter().map(|shard| shard.index).collect();

    for shards in [1u32, 2, 4, 8] {
        let topology = RestoreTopology::Sharded { axis: 0, shards };
        // The full-tensor topology proof the writer already ships.
        let (proved, _) = store
            .restore_checkpoint_bytes(
                "rank3",
                &CheckpointRestoreOptions {
                    topology: Some(topology.clone()),
                    ..CheckpointRestoreOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            proved[proved.len() - tensor_len()..],
            full[..],
            "topology {shards} full restore changed bytes"
        );

        let plane = SHAPE[0] / u64::from(shards);
        let mut seen_bytes = 0u64;
        for rank in 0..shards {
            let plan = store.plan_rank_restore("rank3", &topology, rank).unwrap();
            assert_eq!(plan.axis, 0);
            assert_eq!(plan.shards, shards);
            assert_eq!(plan.rank, rank);
            assert_eq!(plan.tensors.len(), 1);
            let planned = &plan.tensors[0];
            assert_eq!(planned.start, vec![rank as u64 * plane, 0, 0]);
            assert_eq!(
                planned.end,
                vec![(rank as u64 + 1) * plane, SHAPE[1], SHAPE[2]]
            );
            assert_eq!(planned.shape, vec![plane, SHAPE[1], SHAPE[2]]);

            let mut chunks = Vec::new();
            let report = store
                .restore_checkpoint_rank("rank3", &topology, rank, None, |row, bytes| {
                    chunks.push((row.name.clone(), bytes.to_vec()));
                    Ok(())
                })
                .unwrap();
            assert!(report.fetched_set_matches_plan, "rank {rank}/{shards}");
            let row = &report.tensors[0];
            assert_eq!(row.shard_indices, planned.stored_shards);
            // With the target on the stored split axis, a rank touches exactly
            // the stored shard(s) its slab overlaps. Both splits divide 64, so
            // a target plane is either inside one stored slab or an exact
            // multiple of it.
            let expected: Vec<u32> = stored
                .iter()
                .copied()
                .filter(|index| {
                    let lo = u64::from(*index) * (SHAPE[0] / 8);
                    let hi = lo + SHAPE[0] / 8;
                    lo < planned.end[0] && hi > planned.start[0]
                })
                .collect();
            assert_eq!(row.shard_indices, expected);
            assert_eq!(row.representation_bytes, planned.stored_shard_bytes);
            // Logical bytes owned, exactly.
            assert_eq!(
                row.needed_bytes,
                plane * SHAPE[1] * SHAPE[2] * ELEMENT_BYTES as u64
            );
            // The materialised bytes are the exact slice of a full restore.
            let expected_bytes = slice_region(&full, &planned.start, &planned.end);
            assert_eq!(chunks[0].1, expected_bytes, "rank {rank}/{shards} bytes");
            assert_eq!(
                row.content_root,
                weftfs::identity::content_hash(&expected_bytes)
            );
            seen_bytes += row.needed_bytes;
        }
        assert_eq!(
            seen_bytes,
            SHAPE[0] * SHAPE[1] * SHAPE[2] * ELEMENT_BYTES as u64,
            "the ranks partition the logical tensor"
        );
    }
}

#[test]
fn a_target_axis_off_the_stored_axis_touches_every_shard() {
    let (_dir, store, full, _report) = setup();
    let topology = RestoreTopology::Sharded { axis: 1, shards: 4 };
    let plane = SHAPE[1] / 4;
    for rank in 0..4u32 {
        let plan = store.plan_rank_restore("rank3", &topology, rank).unwrap();
        let planned = &plan.tensors[0];
        assert_eq!(planned.stored_shards, (0..8).collect::<Vec<u32>>());
        let mut payload = Vec::new();
        let report = store
            .restore_checkpoint_rank("rank3", &topology, rank, None, |_row, bytes| {
                payload = bytes.to_vec();
                Ok(())
            })
            .unwrap();
        assert!(report.fetched_set_matches_plan);
        assert_eq!(
            report.tensors[0].shard_indices,
            (0..8).collect::<Vec<u32>>()
        );
        let expected = slice_region(
            &full,
            &[0, rank as u64 * plane, 0],
            &[SHAPE[0], (rank as u64 + 1) * plane, SHAPE[2]],
        );
        assert_eq!(payload.len(), expected.len(), "axis-1 rank {rank} length");
        assert!(
            payload == expected,
            "axis-1 rank {rank}: first diff {:?}",
            payload
                .iter()
                .zip(expected.iter())
                .position(|(a, b)| a != b)
        );
    }
}

#[test]
fn a_non_divisible_target_is_the_writer_refusal() {
    let (_dir, store, _full, _report) = setup();
    let topology = RestoreTopology::Sharded { axis: 0, shards: 3 };
    let err = store.plan_rank_restore("rank3", &topology, 0).unwrap_err();
    assert_eq!(err.class(), weftfs::Class::NonCanonical);
    let text = err.to_string();
    assert!(text.contains("no silent padding"), "{text}");
    assert!(text.contains(TENSOR), "{text}");
    // The read path refuses identically; nothing is materialised.
    let mut calls = 0usize;
    let err = store
        .restore_checkpoint_rank("rank3", &topology, 0, None, |_row, _bytes| {
            calls += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::NonCanonical);
    assert_eq!(calls, 0);
}

#[test]
fn one_named_tensor_is_the_same_arithmetic() {
    let (_dir, store, full, _report) = setup();
    let topology = RestoreTopology::Sharded { axis: 0, shards: 2 };
    let plan = store
        .plan_rank_restore_tensor("rank3", &topology, 1, Some(TENSOR))
        .unwrap();
    assert_eq!(plan.tensors.len(), 1);
    let mut payload = Vec::new();
    store
        .restore_checkpoint_rank("rank3", &topology, 1, Some(TENSOR), |_row, bytes| {
            payload = bytes.to_vec();
            Ok(())
        })
        .unwrap();
    assert_eq!(payload, slice_region(&full, &[32, 0, 0], &[64, 32, 16]));
    // An unknown name is NotFound, not an empty successful restore.
    let err = store
        .plan_rank_restore_tensor("rank3", &topology, 1, Some("absent"))
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::NotFound);
}

#[test]
fn a_rank_restore_needs_a_sharded_target() {
    let (_dir, store, _full, _report) = setup();
    let err = store
        .plan_rank_restore("rank3", &RestoreTopology::RowMajor, 0)
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::InvalidArgument);
    let err = store
        .plan_rank_restore("rank3", &RestoreTopology::Sharded { axis: 0, shards: 2 }, 2)
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::InvalidArgument);
}

#[test]
fn a_rank_restore_counts_only_the_shards_it_reads() {
    let (_dir, store, _full, _report) = setup();
    // Target 8-way on the stored axis: rank 3 reads exactly stored shard 3.
    let topology = RestoreTopology::Sharded { axis: 0, shards: 8 };
    let mut payload = Vec::new();
    let report = store
        .restore_checkpoint_rank("rank3", &topology, 3, None, |_row, bytes| {
            payload = bytes.to_vec();
            Ok(())
        })
        .unwrap();
    let row = &report.tensors[0];
    assert_eq!(row.shard_indices, vec![3]);
    // The backend may serve the object from the read path; the representation
    // counter is the arithmetic identity, not a cache-dependent number.
    assert_eq!(
        row.representation_bytes,
        row.predicted_shards.len() as u64 * {
            let tensor = store.checkpoint_manifest("rank3").unwrap().tensors[0].clone();
            tensor.shards[3].byte_len
        }
    );
    assert_eq!(payload.len() as u64, row.needed_bytes);
    // A warm store can still report zero new reads from a cached object, so the
    // amplification is measured from `representation_bytes`, not invented.
    assert!(report.store_reads > 0);
}

#[test]
fn a_rank_one_tensor_is_replicated_when_the_target_axis_does_not_exist() {
    // A 1-D tensor has no axis 1 to split. Every destination rank needs the
    // whole tensor; the plan says so instead of inventing an axis.
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path().join("store"), opts()).unwrap();
    let data = random_bytes(48 * ELEMENT_BYTES, 0xabcd);
    let shape = [48u64];
    let file = safetensors::encode_file(
        &[SafetensorsEntry {
            name: "bias",
            dtype: "bf16",
            shape: &shape,
            data: &data,
        }],
        None,
    )
    .unwrap();
    let source = dir.path().join("bias.safetensors");
    fs::write(&source, &file).unwrap();
    store
        .write_checkpoint(
            "bias",
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();

    let topology = RestoreTopology::Sharded { axis: 1, shards: 2 };
    for rank in 0..2u32 {
        let plan = store.plan_rank_restore("bias", &topology, rank).unwrap();
        let planned = &plan.tensors[0];
        assert_eq!(planned.start, vec![0]);
        assert_eq!(planned.end, vec![48]);
        assert_eq!(planned.needed_bytes, 48 * ELEMENT_BYTES as u64);
        assert_eq!(planned.stored_shards, vec![0, 1]);
        let mut payload = Vec::new();
        let report = store
            .restore_checkpoint_rank("bias", &topology, rank, None, |_row, bytes| {
                payload = bytes.to_vec();
                Ok(())
            })
            .unwrap();
        assert!(report.fetched_set_matches_plan);
        assert_eq!(payload, data, "rank {rank} replicated payload");
    }
}
