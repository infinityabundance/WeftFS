//! Mode-20 `ENGRAM_SHARD` store integration: exact full reads, row-region
//! reads that touch only the groups they intersect, and an incremental update
//! that writes only the changed group and leaves the old version intact.

use tempfile::tempdir;

use weftcodec::Region;
use weftfs::format::RepresentationKind;
use weftfs::identity;
use weftfs::recovery::{read_blob_verified, read_manifest, BlobKind};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    EngramGroupUpdate, EngramWriteSpec, TensorLayoutSpec, TensorPlanOptions, TensorStrategy,
    TensorWriteSpec,
};

const ROWS: u64 = 32;
const DIM: u64 = 4;
const ROW_BYTES: usize = (DIM as usize) * 2;

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

fn table(seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..ROWS as usize * ROW_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xff) as u8
        })
        .collect()
}

fn write_spec() -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::EngramShard),
        causal: Vec::new(),
        plan: TensorPlanOptions {
            engram: Some(EngramWriteSpec {
                row_group_edges: vec![0, 8, 16, 24, 32],
                hash_function: "ngram-blake3-v1".to_owned(),
                vocab_size: ROWS,
                precision: None,
                quantization: None,
            }),
            ..TensorPlanOptions::default()
        },
        tile_shape: None,
    }
}

#[test]
fn engram_store_round_trip_region_and_incremental_update() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let original = table(7);
    let layout = TensorLayoutSpec::row_major();
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"engram",
            "bf16",
            &[ROWS, DIM],
            &original,
            &layout,
            &write_spec(),
        )
        .unwrap();

    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).unwrap();
    let rep = manifest.representation.clone().unwrap();
    assert_eq!(rep.kind, RepresentationKind::Tensor);
    assert_eq!(rep.strategy, "engram-shard");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 20);
    assert!(rep.dependencies.is_empty());

    // Full read is exact.
    assert_eq!(store.read_tensor(id).unwrap(), original);

    // A row region inside group 1 touches only that group.
    let region = Region::new(vec![9, 0], vec![14, DIM]);
    let read = store
        .read_tensor_region_with_options(id, &region, &Default::default())
        .unwrap();
    let start = 9 * ROW_BYTES;
    let end = 14 * ROW_BYTES;
    assert_eq!(read.bytes, original[start..end]);
    assert_eq!(read.units_touched, 1);

    // A cross-group region touches exactly the two groups it straddles.
    let region = Region::new(vec![7, 0], vec![17, DIM]);
    let read = store
        .read_tensor_region_with_options(id, &region, &Default::default())
        .unwrap();
    assert_eq!(read.bytes, original[7 * ROW_BYTES..17 * ROW_BYTES]);
    assert_eq!(read.units_touched, 3);

    // Snapshot the old state, then update group 2 (rows 16..24).
    let snapshot = store.snapshot_create("before-engram-update").unwrap();
    let mut replacement = vec![0u8; (24 - 16) as usize * ROW_BYTES];
    for (index, byte) in replacement.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(11).wrapping_add(3);
    }
    let mut expected = original.clone();
    expected[16 * ROW_BYTES..24 * ROW_BYTES].copy_from_slice(&replacement);
    let new_version = store
        .update_engram_groups(
            id,
            &[EngramGroupUpdate {
                group: 2,
                bytes: &replacement,
            }],
        )
        .unwrap();
    assert_ne!(new_version, record.version);
    assert_eq!(store.read_tensor(id).unwrap(), expected);

    // The old version is unchanged through the snapshot.
    let old = store.read_object_at(Some(snapshot), id).unwrap();
    assert_eq!(old, original);

    // The new representation references the old artifact and emits the
    // unchanged groups as references, so its payload is only group 2.
    let new_record = store.object_meta(id).unwrap();
    let new_manifest = read_manifest(store.root(), new_record.content).unwrap();
    let new_rep = new_manifest.representation.unwrap();
    assert_eq!(new_rep.strategy, "engram-shard");
    assert_eq!(new_rep.dependencies.len(), 1);
    assert_eq!(new_rep.dependencies[0], record.content);
    let frame_id = new_rep.frame.unwrap();
    let frame_bytes = read_blob_verified(
        store.root(),
        BlobKind::Manifest,
        frame_id,
        identity::MANIFEST_DOMAIN,
    )
    .unwrap();
    let frame = weftcodec::Frame::parse(&frame_bytes, &weftcodec::Limits::default()).unwrap();
    let meta = weftcodec::engram::engram_metadata(&frame, &weftcodec::Limits::default()).unwrap();
    assert_eq!(
        meta.groups[2].storage,
        weftcodec::engram::EngramStorage::Inline {
            offset: 0,
            compressed_len: meta.payload_len,
        }
    );
    for index in [0usize, 1, 3] {
        assert!(matches!(
            meta.groups[index].storage,
            weftcodec::engram::EngramStorage::Reference { .. }
        ));
    }
    let old_frame_bytes = read_blob_verified(
        store.root(),
        BlobKind::Manifest,
        rep.frame.unwrap(),
        identity::MANIFEST_DOMAIN,
    )
    .unwrap();
    let old_frame =
        weftcodec::Frame::parse(&old_frame_bytes, &weftcodec::Limits::default()).unwrap();
    let old_meta =
        weftcodec::engram::engram_metadata(&old_frame, &weftcodec::Limits::default()).unwrap();
    assert!(
        meta.payload_len < old_meta.payload_len,
        "the update stores fewer groups than the original table"
    );

    // The new version still verifies end to end.
    assert_eq!(
        weftcodec::engram::decode_engram(
            &frame,
            &weftcodec::dependency::MapResolver::new(),
            &weftcodec::Limits::default(),
        )
        .unwrap_err()
        .class(),
        weftcodec::ErrorClass::DependencyMissing,
        "a reference frame cannot decode without its dependency"
    );
}
