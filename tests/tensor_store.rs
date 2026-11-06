//! First-class WeftTensor representations in the store and the cost planner.
//!
//! These cover the manifest extension (kind `"tensor"`, modes 10/11/14), the
//! planner (bytes-only and latency-weighted), semantic-delta selection,
//! fail-closed missing dependencies, GC retention, and layout-independent
//! logical identity.

use std::fs;

use tempfile::tempdir;
use weftcodec::wcb::Value;
use weftcodec::{Limits, SemanticPolicy};

use weftfs::format::{Manifest, RepresentationKind};
use weftfs::identity::{self, Hash32};
use weftfs::recovery::{manifest_path, read_manifest};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

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

/// Deterministic pseudo-random bytes (xorshift64*).
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

/// A smooth, compressible bf16-like tensor: structured 16-bit words.
fn smooth_tensor(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed | 1;
    for index in 0..len / 2 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let low = ((index as u16).wrapping_mul(37)) & 0x3ff;
        let word = (0x3f00u16 | low) ^ ((state as u16) & 0x3f);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// Change `count` elements by flipping only low mantissa bits (bits 0..=2),
/// which keeps the semantic root identical under `mantissa_truncate:3`.
fn nudge_low_mantissa(bytes: &[u8], count: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let stride = (out.len() / 2 / count.max(1)).max(1);
    let mut done = 0usize;
    let mut index = 0usize;
    while done < count && index * 2 + 1 < out.len() {
        out[index * 2] ^= 0x01;
        done += 1;
        index += stride;
    }
    out
}

/// Change `count` elements in the exponent/high-mantissa region, which changes
/// the semantic root.
fn nudge_high_bits(bytes: &[u8], count: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let stride = (out.len() / 2 / count.max(1)).max(1);
    let mut done = 0usize;
    let mut index = 0usize;
    while done < count && index * 2 + 1 < out.len() {
        out[index * 2 + 1] ^= 0x08;
        done += 1;
        index += stride;
    }
    out
}

fn blocked_physical(
    dtype: weftcodec::TensorDtype,
    shape: &[u64],
    block: &[u64],
    row_major: &[u8],
) -> Vec<u8> {
    let blocks =
        weftcodec::tensor::blocked_bytes(dtype, shape, block, row_major, &Limits::default())
            .unwrap();
    blocks.into_iter().flatten().collect()
}

fn manifest_of(store: &Store, id: Hash32) -> Manifest {
    let record = store.object_meta(id).unwrap();
    read_manifest(store.root(), record.content).unwrap()
}

fn frame_len(store: &Store, id: Hash32) -> u64 {
    let manifest = manifest_of(store, id);
    let frame = manifest
        .representation
        .as_ref()
        .and_then(|rep| rep.frame)
        .expect("frame id");
    fs::metadata(manifest_path(store.root(), frame))
        .unwrap()
        .len()
}

#[test]
fn mode10_round_trips_exact_bytes_across_layouts() {
    let shape = [256u64, 512];
    let bytes = random_bytes(256 * 512, 0x10);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    let row = TensorLayoutSpec::row_major();
    let a = store
        .write_tensor_file(
            root,
            b"q8-row",
            "q8_0",
            &shape,
            &bytes,
            &row,
            Some(TensorStrategy::TensorSharded),
        )
        .unwrap();
    let row_root = store.tensor_logical_root(a).unwrap();
    assert_eq!(store.read_tensor(a).unwrap(), bytes);
    let rep = manifest_of(&store, a).representation.unwrap();
    assert_eq!(rep.kind, RepresentationKind::Tensor);
    assert_eq!(rep.strategy, "tensor-sharded");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 10);
    assert!(rep.dependencies.is_empty());

    // Blocked physical order, canonicalised by the store.
    let block = [64u64, 64];
    let blocked_layout = TensorLayoutSpec::blocked(block.to_vec());
    let blocked_bytes = blocked_physical(weftcodec::TensorDtype::I8, &shape, &block, &bytes);
    let b = store
        .write_tensor_file(
            root,
            b"q8-blocked",
            "q8_0",
            &shape,
            &blocked_bytes,
            &blocked_layout,
            Some(TensorStrategy::TensorSharded),
        )
        .unwrap();
    assert_eq!(store.read_tensor(b).unwrap(), bytes);
    assert_eq!(store.tensor_logical_root(b).unwrap(), row_root);

    // Single-shard declaration (the store keeps one whole-tensor object).
    let sharded = TensorLayoutSpec::sharded(0, 1, 0);
    let c = store
        .write_tensor_file(
            root,
            b"q8-sharded",
            "q8_0",
            &shape,
            &bytes,
            &sharded,
            Some(TensorStrategy::TensorSharded),
        )
        .unwrap();
    assert_eq!(store.read_tensor(c).unwrap(), bytes);
    assert_eq!(store.tensor_logical_root(c).unwrap(), row_root);

    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
    assert_eq!(report.checked_representations, 3);
}

#[test]
fn mode11_round_trips_exact_bytes_and_stable_root_across_layouts() {
    let shape = [256u64, 512];
    let base_bytes = smooth_tensor(256 * 512 * 2, 0x22);
    let target = nudge_low_mantissa(&base_bytes, 400);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    let base = store
        .write_tensor_file(
            root,
            b"base",
            "bf16",
            &shape,
            &base_bytes,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    let base_record = store.object_meta(base).unwrap();
    let base_root = store.tensor_logical_root(base).unwrap();

    let mut targets = Vec::new();
    for (name, layout, physical) in [
        (
            b"w-row".as_slice(),
            TensorLayoutSpec::row_major(),
            target.clone(),
        ),
        (
            b"w-blocked".as_slice(),
            TensorLayoutSpec::blocked(vec![64, 64]),
            blocked_physical(weftcodec::TensorDtype::Bf16, &shape, &[64, 64], &target),
        ),
        (
            b"w-sharded".as_slice(),
            TensorLayoutSpec::sharded(0, 1, 0),
            target.clone(),
        ),
    ] {
        let spec = TensorWriteSpec {
            strategy_hint: Some(TensorStrategy::WeightDelta),
            causal: vec![base_record.version],
            ..TensorWriteSpec::default()
        };
        let id = store
            .write_tensor_file_with_spec(root, name, "bf16", &shape, &physical, &layout, &spec)
            .unwrap();
        assert_eq!(
            store.read_tensor(id).unwrap(),
            target,
            "layout {}",
            layout.name()
        );
        let rep = manifest_of(&store, id).representation.unwrap();
        assert_eq!(rep.kind, RepresentationKind::Tensor);
        assert_eq!(rep.strategy, "weight-delta");
        assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 11);
        assert_eq!(rep.dependencies.len(), 1);
        targets.push(id);
    }
    // The logical root is a property of the logical tensor, not of the layout.
    let target_root = store.tensor_logical_root(targets[0]).unwrap();
    assert_ne!(target_root, base_root);
    for id in &targets {
        assert_eq!(store.tensor_logical_root(*id).unwrap(), target_root);
    }

    // A row-major read is region-addressable and exact.
    let region = weftcodec::Region::new(vec![10, 0], vec![12, 512]);
    let read = store.read_tensor_region(targets[0], &region).unwrap();
    let element_bytes = 2usize;
    let expect_start = 10 * 512 * element_bytes;
    let expect = &target[expect_start..expect_start + 2 * 512 * element_bytes];
    assert_eq!(read.bytes, expect);

    let report = store.verify(true).unwrap();
    assert!(report.ok, "problems: {:?}", report.problems);
}

#[test]
fn planner_bytes_only_chooses_weight_delta_for_a_near_duplicate_pair() {
    let shape = [256u64, 512];
    let v1 = smooth_tensor(256 * 512 * 2, 0x33);
    let v2 = nudge_low_mantissa(&v1, 1_000);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    let base = store
        .write_tensor_file(
            root,
            b"c1",
            "bf16",
            &shape,
            &v1,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    let base_record = store.object_meta(base).unwrap();

    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &[base_record.version],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    assert_eq!(plan.chosen, TensorStrategy::WeightDelta, "plan {plan:?}");
    let zstd = weftcodec::modes::encode_zstd(&v2, 3, Some(64 * 1024), &Limits::default()).unwrap();
    assert!(
        plan.chosen_physical_bytes * 2 < zstd.len() as u64,
        "delta {} bytes is not < 50% of independent zstd {} bytes",
        plan.chosen_physical_bytes,
        zstd.len()
    );

    // Determinism: the same inputs produce the same choice and prices.
    let again = store
        .plan_tensor(
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &[base_record.version],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    assert_eq!(plan, again);

    // The write agrees with the plan.
    let spec = TensorWriteSpec {
        causal: vec![base_record.version],
        ..TensorWriteSpec::default()
    };
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"c2",
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .unwrap();
    assert_eq!(frame_len(&store, id), plan.chosen_physical_bytes);
    assert_eq!(store.read_tensor(id).unwrap(), v2);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn planner_bytes_only_chooses_raw_for_incompressible_tensors() {
    let shape = [256u64, 512];
    let bytes = random_bytes(256 * 512 * 2, 0x44);
    let plan = {
        let dir = tempdir().unwrap();
        let store = Store::init(dir.path(), opts()).unwrap();
        store
            .plan_tensor(
                "bf16",
                &shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                &[],
                &TensorPlanOptions::default(),
            )
            .unwrap()
    };
    assert_eq!(plan.chosen, TensorStrategy::Raw, "plan {plan:?}");
    let zstd = plan
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::Zstd)
        .and_then(|item| item.physical_bytes)
        .expect("zstd candidate");
    assert!(
        plan.chosen_physical_bytes <= zstd,
        "raw {} should not exceed zstd {}",
        plan.chosen_physical_bytes,
        zstd
    );
}

#[test]
fn planner_latency_weight_switches_to_the_faster_representation() {
    let shape = [256u64, 512];
    let v1 = smooth_tensor(256 * 512 * 2, 0x55);
    let v2 = nudge_low_mantissa(&v1, 1_000);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let base = store
        .write_tensor_file(
            root,
            b"l1",
            "bf16",
            &shape,
            &v1,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    let base_record = store.object_meta(base).unwrap();

    let bytes_only = store
        .plan_tensor(
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &[base_record.version],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    assert_eq!(bytes_only.chosen, TensorStrategy::WeightDelta);

    let latency = store
        .plan_tensor(
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &[base_record.version],
            &TensorPlanOptions {
                lambda_ns: 10_000_000,
                ..TensorPlanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(latency.chosen, TensorStrategy::Raw, "plan {latency:?}");
    assert!(
        latency.chosen_physical_bytes > bytes_only.chosen_physical_bytes,
        "the faster choice should be the larger one: {} vs {}",
        latency.chosen_physical_bytes,
        bytes_only.chosen_physical_bytes
    );
    let weighted_delta = latency
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::WeightDelta)
        .and_then(|item| item.cost)
        .expect("weighted delta candidate");
    assert!(
        latency.chosen_cost < weighted_delta,
        "weighted raw cost {} should beat weighted delta cost {weighted_delta}",
        latency.chosen_cost
    );

    // Determinism under a latency weight.
    let repeat = store
        .plan_tensor(
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &[base_record.version],
            &TensorPlanOptions {
                lambda_ns: 10_000_000,
                ..TensorPlanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(latency, repeat);
}

#[test]
fn semantic_planner_chooses_semantic_delta_only_for_a_matching_root() {
    let shape = [256u64, 512];
    let v1 = smooth_tensor(256 * 512 * 2, 0x66);
    let same_root = nudge_low_mantissa(&v1, 131);
    let different_root = nudge_high_bits(&v1, 131);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let base = store
        .write_tensor_file(
            root,
            b"s1",
            "bf16",
            &shape,
            &v1,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();

    let policy = Some(SemanticPolicy::MantissaTruncate { bits: 3 });
    let options = TensorPlanOptions {
        semantic_policy: policy.clone(),
        candidates: vec![base],
        ..TensorPlanOptions::default()
    };
    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &same_root,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .unwrap();
    assert_eq!(plan.chosen, TensorStrategy::SemanticDelta, "plan {plan:?}");

    let other = store
        .plan_tensor(
            "bf16",
            &shape,
            &different_root,
            &TensorLayoutSpec::row_major(),
            &[],
            &options,
        )
        .unwrap();
    assert_ne!(
        other.chosen,
        TensorStrategy::SemanticDelta,
        "plan {other:?}"
    );

    // The chosen representation round-trips through the store.
    let spec = TensorWriteSpec {
        plan: options,
        ..TensorWriteSpec::default()
    };
    let id = store
        .write_tensor_file_with_spec(
            root,
            b"s2",
            "bf16",
            &shape,
            &same_root,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .unwrap();
    let rep = manifest_of(&store, id).representation.unwrap();
    assert_eq!(rep.strategy, "semantic-delta");
    assert_eq!(rep.tensor.as_ref().unwrap().tensor_mode, 14);
    assert_eq!(store.read_tensor(id).unwrap(), same_root);
    assert_ne!(
        store.tensor_logical_root(id).unwrap(),
        store.tensor_logical_root(base).unwrap(),
        "a semantic match is not a content match"
    );
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn missing_dependency_fails_closed_gc_retains_it_and_verify_names_it() {
    let shape = [128u64, 512];
    let v1 = smooth_tensor(128 * 512 * 2, 0x77);
    let v2 = nudge_low_mantissa(&v1, 500);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let base = store
        .write_tensor_file(
            root,
            b"base",
            "bf16",
            &shape,
            &v1,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    let base_record = store.object_meta(base).unwrap();
    let spec = TensorWriteSpec {
        strategy_hint: Some(TensorStrategy::WeightDelta),
        causal: vec![base_record.version],
        ..TensorWriteSpec::default()
    };
    let derived = store
        .write_tensor_file_with_spec(
            root,
            b"derived",
            "bf16",
            &shape,
            &v2,
            &TensorLayoutSpec::row_major(),
            &spec,
        )
        .unwrap();
    let derived_manifest = manifest_of(&store, derived);
    let dependency = derived_manifest.representation.unwrap().dependencies[0];
    assert_eq!(dependency, base_record.content);

    // GC retains the base manifest while the dependent is live.
    store.unlink(root, b"base").unwrap();
    store.gc(true).unwrap();
    assert!(
        manifest_path(store.root(), dependency).exists(),
        "GC deleted a live tensor dependent's base"
    );
    assert_eq!(store.read_tensor(derived).unwrap(), v2);

    // Remove the base manifest: reads and verify fail closed and name it.
    fs::remove_file(manifest_path(store.root(), dependency)).unwrap();
    let full = store.read_tensor(derived).unwrap_err();
    assert_eq!(full.class_name(), "Corrupt", "unexpected error: {full}");
    let ranged = store.read_range(derived, 0, 32).unwrap_err();
    assert_eq!(ranged.class_name(), "Corrupt", "unexpected error: {ranged}");
    let report = store.verify(true).unwrap();
    assert!(!report.ok);
    let base_hex = dependency.to_hex();
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.detail.contains(&base_hex)),
        "no problem names the missing dependency: {:?}",
        report.problems
    );
}

#[test]
fn byte_rewrite_of_a_tensor_object_drops_tensor_identity() {
    let shape = [8u64, 8];
    let bytes = smooth_tensor(8 * 8 * 2, 0x99);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .write_tensor_file(
            root,
            b"t",
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    assert!(store.tensor_metadata(id).is_ok());
    let mut modified = bytes.clone();
    modified[0] ^= 0xff;
    store.write_file(id, 0, &modified[..1]).unwrap();
    assert_eq!(store.read_object(id).unwrap(), modified);
    // The object is now an ordinary byte representation; claiming the old
    // tensor identity would be false, so the attributes are gone.
    let error = store.tensor_metadata(id).unwrap_err();
    assert_eq!(error.class_name(), "NotFound");
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn tensor_layout_encoding_is_canonical_and_attributes_are_recorded() {
    let shape = [8u64, 8];
    let bytes = smooth_tensor(8 * 8 * 2, 0x88);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let id = store
        .write_tensor_file(
            root,
            b"t",
            "bf16",
            &shape,
            &bytes,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    let record = store.object_meta(id).unwrap();
    assert_eq!(
        record.attrs.get("weft.tensor.dtype"),
        Some(&Value::Str("bf16".to_owned()))
    );
    assert_eq!(
        record.attrs.get("weft.tensor.logical_root"),
        Some(&Value::Bytes(
            store.tensor_logical_root(id).unwrap().as_bytes().to_vec()
        ))
    );
    let meta = store.tensor_metadata(id).unwrap();
    assert_eq!(meta.dtype.as_str(), "bf16");
    assert_eq!(meta.shape, shape.to_vec());
    // The layout attribute is the canonical WCB map.
    let layout_bytes = match record.attrs.get("weft.tensor.layout") {
        Some(Value::Bytes(bytes)) => bytes.clone(),
        other => panic!("layout attribute missing: {other:?}"),
    };
    let decoded = weftfs::wcb_util::decode(&layout_bytes).unwrap();
    assert_eq!(
        TensorLayoutSpec::from_value(&decoded).unwrap().2,
        TensorLayoutSpec::row_major()
    );
    // Recomputing the stored manifest id is stable.
    let manifest = manifest_of(&store, id);
    let encoded = manifest.encode().unwrap();
    assert_eq!(identity::manifest_id(&encoded), record.content);
}

#[test]
fn planner_offers_mode15_only_when_a_tile_shape_is_supplied() {
    let shape = [64u64, 32];
    let bytes = smooth_tensor(64 * 32 * 2, 0x33);
    let base = smooth_tensor(64 * 32 * 2, 0x44);
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();
    store
        .write_tensor_file(
            root,
            b"base",
            "bf16",
            &shape,
            &base,
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();

    let plan_with = |options: TensorPlanOptions| {
        store
            .plan_tensor(
                "bf16",
                &shape,
                &bytes,
                &TensorLayoutSpec::row_major(),
                &[],
                &options,
            )
            .unwrap()
    };

    let without = plan_with(TensorPlanOptions::default());
    let tiled = without
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::TensorTiled)
        .expect("tiled candidate is always listed");
    assert!(!tiled.applicable);
    assert!(tiled.detail.contains("no tile shape"), "{}", tiled.detail);

    let options = TensorPlanOptions {
        tile_shape: Some(vec![16, 16]),
        ..TensorPlanOptions::default()
    };
    let with = plan_with(options.clone());
    let tiled = with
        .candidates
        .iter()
        .find(|item| item.strategy == TensorStrategy::TensorTiled)
        .expect("tiled candidate is always listed");
    assert!(tiled.applicable, "{tiled:?}");
    assert!(tiled.physical_bytes.unwrap_or(0) > 0);
    assert!(tiled.estimated_decode_ns.unwrap_or(0) > 0);

    // The option is deterministic and does not change without it.
    assert_eq!(with, plan_with(options));
    assert_eq!(without, plan_with(TensorPlanOptions::default()));
}
