//! Mode 16 `SPARSE_DELTA` through the store planner: exact read-back, an
//! independently recomputed `delta_root`, and the sparse-vs-dense choice
//! between modes 16 and 11.

use std::fs;

use weftcodec::{sparse_delta_metadata, weight_delta_metadata, Frame, Limits};
use weftfs::identity::MANIFEST_DOMAIN;
use weftfs::recovery::{read_blob_verified, BlobKind};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
    let path = std::env::temp_dir().join(format!(
        "weft-planner-sparse-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    let store = Store::init(
        &path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .unwrap();
    (path, store)
}

/// Deterministic weight-like BF16 payload (bounded values, not random words).
fn bf16_payload(elements: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32);
        let value = (unit * 2.0 - 1.0) * 0.5;
        out.extend_from_slice(&((value.to_bits() >> 16) as u16).to_le_bytes());
    }
    out
}

fn word(bytes: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]])
}

fn set_word(bytes: &mut [u8], index: usize, value: u16) {
    bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
}

/// `base` with `count` deterministic, guaranteed non-zero word deltas.
fn sparse_target(base: &[u8], elements: usize, count: usize) -> Vec<u8> {
    let mut target = base.to_vec();
    let mut state = 0x0123_4567_89ab_cdefu64;
    for step in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // A spread that always lands inside the tensor and never repeats.
        let index = (step * (elements / count.max(1)) + (state as usize % 97)) % elements;
        let delta = ((state >> 32) as u16) | 1;
        set_word(&mut target, index, word(base, index).wrapping_add(delta));
    }
    target
}

fn sparse_delta_root(base: &[u8], target: &[u8], elements: usize) -> weftcodec::Hash32 {
    let mut delta_words = Vec::with_capacity(elements * 2);
    for index in 0..elements {
        let delta = word(target, index).wrapping_sub(word(base, index));
        delta_words.extend_from_slice(&delta.to_le_bytes());
    }
    weftcodec::hash::hash_domain(weftcodec::tensor::DOMAIN_TENSOR_VALUES, &delta_words)
}

#[test]
fn sparse_change_selects_mode16_and_reads_back_exactly() {
    let (path, mut store) = temp_store("sparse");
    let root = store.resolve_path("/").unwrap();
    let layout = TensorLayoutSpec::row_major();
    let elements = 512 * 1024; // 1 MiB of BF16
    let shape = vec![512u64, 1024u64];
    let base_bytes = bf16_payload(elements);
    let target_bytes = sparse_target(&base_bytes, elements, 19);

    // The base is committed independently so its version can be the causal
    // parent of the next write.
    let base_id = store
        .write_tensor_file_with_spec(
            root,
            b"base",
            "bf16",
            &shape,
            &base_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::FloatTensor),
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    let base_version = store.object_meta(base_id).unwrap().version;

    // This test targets mode 16 specifically. The ZSTD level-19 and patch
    // candidates were added later and can win on this input; they have their
    // own tests in `planner_write_cost.rs`.
    let legacy = TensorPlanOptions {
        zstd19: false,
        zstd_patch: false,
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &[base_version],
            &legacy,
        )
        .unwrap();
    let sparse = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::SparseDelta)
        .expect("sparse-delta is a candidate");
    let weight = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::WeightDelta)
        .expect("weight-delta is a candidate");
    assert!(sparse.applicable && weight.applicable);
    assert!(
        sparse.physical_bytes.unwrap() * 100 < weight.physical_bytes.unwrap(),
        "a 19-word change must be far smaller under mode 16 ({} vs {} bytes)",
        sparse.physical_bytes.unwrap(),
        weight.physical_bytes.unwrap()
    );
    assert_eq!(plan.chosen, TensorStrategy::SparseDelta);

    let delta_id = store
        .write_tensor_file_with_spec(
            root,
            b"step1",
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &TensorWriteSpec {
                causal: vec![base_version],
                plan: legacy,
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    let meta = store.tensor_metadata(delta_id).unwrap();
    assert_eq!(meta.strategy, "sparse-delta");
    assert_eq!(store.read_tensor(delta_id).unwrap(), target_bytes);

    // The stored frame is mode 16, its descriptor names the base content root,
    // and its delta_root equals an independently recomputed values root.
    let manifest = store
        .manifest_of(store.object_meta(delta_id).unwrap().content)
        .unwrap();
    let rep = manifest.representation.expect("representation");
    assert_eq!(rep.strategy, "sparse-delta");
    assert_eq!(rep.dependencies.len(), 1);
    let tensor = rep.tensor.expect("tensor fields");
    assert_eq!(tensor.tensor_mode, 16);
    let frame_id = rep.frame.expect("frame id");
    let frame_bytes =
        read_blob_verified(store.root(), BlobKind::Manifest, frame_id, MANIFEST_DOMAIN).unwrap();
    let limits = Limits::default();
    let parsed = Frame::parse(&frame_bytes, &limits).unwrap();
    assert_eq!(parsed.header.mode.as_str(), "sparse_delta");
    assert_eq!(parsed.header.mode.as_u8(), 16);
    let descriptor = sparse_delta_metadata(&parsed, &limits).unwrap();
    let base_manifest = store.manifest_of(rep.dependencies[0]).unwrap();
    assert_eq!(base_manifest.size, elements as u64 * 2);
    assert_eq!(
        descriptor.delta_root,
        sparse_delta_root(&base_bytes, &target_bytes, elements)
    );
    assert_eq!(descriptor.base, parsed.dependencies[0].hash);
    assert_eq!(descriptor.dtype.as_str(), "bf16");
    assert_eq!(descriptor.shape, shape);
    assert_ne!(
        descriptor.delta_root,
        weftcodec::hash::hash_domain(
            weftcodec::tensor::DOMAIN_TENSOR_VALUES,
            &vec![0u8; elements * 2]
        )
    );

    store.verify(true).unwrap();
    drop(store);

    // Reopen and read through the manifest again: the persisted bytes decode.
    let store = Store::open(&path, OpenMode::ReadWrite).unwrap();
    let id = store.resolve_path("/step1").unwrap();
    assert_eq!(store.read_tensor(id).unwrap(), target_bytes);
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn dense_change_keeps_mode11_and_is_readable() {
    let (path, mut store) = temp_store("dense");
    let root = store.resolve_path("/").unwrap();
    let layout = TensorLayoutSpec::row_major();
    let elements = 256 * 1024; // 512 KiB of BF16
    let shape = vec![256u64, 1024u64];
    let base_bytes = bf16_payload(elements);
    // Every word changes.
    let mut target_bytes = base_bytes.clone();
    let mut state = 0xfeed_face_cafe_beefu64;
    for index in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let delta = ((state >> 32) as u16) | 1;
        set_word(
            &mut target_bytes,
            index,
            word(&base_bytes, index).wrapping_add(delta),
        );
    }

    let base_id = store
        .write_tensor_file_with_spec(
            root,
            b"base",
            "bf16",
            &shape,
            &base_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::FloatTensor),
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    let base_version = store.object_meta(base_id).unwrap().version;

    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &[base_version],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let sparse = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::SparseDelta)
        .expect("sparse-delta is a candidate");
    let weight = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::WeightDelta)
        .expect("weight-delta is a candidate");
    assert!(sparse.applicable && weight.applicable);
    assert!(
        sparse.physical_bytes.unwrap() > weight.physical_bytes.unwrap(),
        "a fully dense delta must be larger under mode 16 ({} vs {} bytes)",
        sparse.physical_bytes.unwrap(),
        weight.physical_bytes.unwrap()
    );
    assert_eq!(plan.chosen, TensorStrategy::WeightDelta);

    let delta_id = store
        .write_tensor_file_with_spec(
            root,
            b"dense",
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &TensorWriteSpec {
                causal: vec![base_version],
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.tensor_metadata(delta_id).unwrap().strategy,
        "weight-delta"
    );
    assert_eq!(store.read_tensor(delta_id).unwrap(), target_bytes);

    // Mode 16 is still readable when a caller asks for it explicitly, and it
    // reconstructs the same bytes.
    let forced_id = store
        .write_tensor_file_with_spec(
            root,
            b"dense-forced",
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::SparseDelta),
                causal: vec![base_version],
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.tensor_metadata(forced_id).unwrap().strategy,
        "sparse-delta"
    );
    assert_eq!(store.read_tensor(forced_id).unwrap(), target_bytes);
    let manifest = store
        .manifest_of(store.object_meta(forced_id).unwrap().content)
        .unwrap();
    let rep = manifest.representation.expect("representation");
    let frame_bytes = read_blob_verified(
        store.root(),
        BlobKind::Manifest,
        rep.frame.expect("frame"),
        MANIFEST_DOMAIN,
    )
    .unwrap();
    let limits = Limits::default();
    let parsed = Frame::parse(&frame_bytes, &limits).unwrap();
    let sparse_meta = sparse_delta_metadata(&parsed, &limits).unwrap();
    // A dense delta has one run per element: the mode-16 payload is at least
    // three bytes per word, so the mode-11 frame must be smaller.
    assert_eq!(
        sparse_meta.delta_root,
        sparse_delta_root(&base_bytes, &target_bytes, elements)
    );
    let weight_manifest = store
        .manifest_of(store.object_meta(delta_id).unwrap().content)
        .unwrap();
    let weight_rep = weight_manifest.representation.expect("representation");
    let weight_frame = read_blob_verified(
        store.root(),
        BlobKind::Manifest,
        weight_rep.frame.expect("frame"),
        MANIFEST_DOMAIN,
    )
    .unwrap();
    let weight_parsed = Frame::parse(&weight_frame, &limits).unwrap();
    assert_eq!(weight_parsed.header.mode.as_str(), "weight_delta");
    assert!(weight_delta_metadata(&weight_parsed, &limits).is_ok());

    store.verify(true).unwrap();
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn cost_model_has_a_sparse_delta_row() {
    use weftfs::cost_model::{coefficients, estimate_ns, CostClass, CostModelKind};
    for model in [CostModelKind::Declared, CostModelKind::Measured] {
        let row = coefficients(CostClass::SparseDelta, model);
        assert!(row.ns_per_mib > 0);
        assert!(estimate_ns(CostClass::SparseDelta, 4 << 20, model) > 0);
    }
    // The row reuses the weight-delta coefficients rather than inventing an
    // unmeasured one.
    assert_eq!(
        coefficients(CostClass::SparseDelta, CostModelKind::Measured),
        coefficients(CostClass::WeightDelta, CostModelKind::Measured)
    );
}
