//! Exact versus approximate view identity (T052).
//!
//! A q8_0 view must carry its own identity and error contract, leave the exact
//! source root authoritative and verifiable, and be refused by the exact
//! resolution door.

use std::fs;

use tempfile::tempdir;
use weftfs::error::Class;
use weftfs::identity;
use weftfs::state::State;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorLayoutSpec;
use weftfs::view_identity::{
    decode_q8_0, encode_q8_0, half_ulp, ViewDescriptor, ViewWriteOptions, Q8_0_BLOCK,
};

const SHAPE: [u64; 2] = [64, 32];

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 64,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn bf16(value: f32) -> Vec<u8> {
    weftfs::adapter::f32_to_bf16_bits(value)
        .to_le_bytes()
        .to_vec()
}

fn tensor_bytes() -> Vec<u8> {
    let count = (SHAPE[0] * SHAPE[1]) as usize;
    let mut out = Vec::with_capacity(count * 2);
    for index in 0..count {
        let value = ((index as f32) * 0.017).sin() * (1.0 + (index % 7) as f32 * 0.13);
        out.extend_from_slice(&bf16(value));
    }
    out
}

fn setup(dir: &std::path::Path) -> (Store, weftfs::identity::Hash32) {
    let mut store = Store::init(dir.join("store"), opts()).unwrap();
    let object = store
        .write_tensor_file(
            State::ROOT_ID,
            b"exact",
            "bf16",
            &SHAPE,
            &tensor_bytes(),
            &TensorLayoutSpec::row_major(),
            None,
        )
        .unwrap();
    (store, object)
}

#[test]
fn a_view_has_its_own_identity_and_leaves_the_exact_root_authoritative() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    let exact_content = store.object_meta(exact).unwrap().content;
    let exact_logical = store.tensor_logical_root(exact).unwrap();

    let report = store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    assert!(report.error.holds, "{:?}", report.error);
    assert_eq!(report.error.clipped, 0);
    assert!(report.error.max_abs_error <= report.error.max_bound);

    let descriptor = store.load_view("q8").unwrap();
    assert_ne!(report.view_id, exact_content);
    assert_ne!(report.view_id, exact_logical);
    assert_ne!(descriptor.approximate_root, exact_content);
    assert_ne!(descriptor.approximate_root, exact_logical);
    assert_eq!(descriptor.source_content, exact_content);
    assert_eq!(descriptor.source_logical_root, exact_logical);

    // The exact object is untouched and still verifies.
    assert_eq!(store.read_tensor(exact).unwrap(), tensor_bytes());
    assert_eq!(store.tensor_logical_root(exact).unwrap(), exact_logical);
    assert_eq!(store.object_meta(exact).unwrap().content, exact_content);

    let resolved = store.resolve_view("q8").unwrap();
    assert_eq!(resolved.bytes.len(), tensor_bytes().len());
    assert_ne!(resolved.bytes, tensor_bytes());
    assert!(resolved.error.holds);

    // The reconstruction root equals the descriptor's declared root.
    assert_eq!(
        identity::h("weft/view/values/v1", &resolved.bytes),
        descriptor.approximate_root
    );

    let verification = store.verify_view("q8").unwrap();
    assert!(verification.exact_object_intact);
    assert_eq!(verification.exact_content, exact_content);
    assert_eq!(verification.exact_logical_root, exact_logical);
    assert_eq!(store.list_views().unwrap(), vec!["q8".to_owned()]);
}

#[test]
fn the_exact_door_refuses_a_view() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let descriptor = store.load_view("q8").unwrap();
    let descriptor_object = store.resolve_path("/views/q8/descriptor").unwrap();

    // An exact artifact resolves.
    assert_eq!(store.resolve_exact_object(exact).unwrap(), tensor_bytes());

    // A view descriptor is refused with a typed error that names both the view
    // and the exact source.
    let err = store.resolve_exact_object(descriptor_object).unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedRepresentation, "{err}");
    let message = err.message().to_owned();
    assert!(message.contains("approximate view"), "{message}");
    assert!(
        message.contains(&descriptor.id().unwrap().to_hex()),
        "{message}"
    );
    assert!(message.contains(&exact.to_hex()), "{message}");

    // The codes and scales are themselves exact tensors (i8 / f32), which is
    // the whole point: the approximation lives in the descriptor, not in a
    // corrupted exact object.
    assert_eq!(
        store.resolve_exact_object(descriptor.codes).unwrap().len(),
        descriptor.codes_shape[0] as usize
    );
    assert_eq!(
        store.resolve_exact_object(descriptor.scales).unwrap().len(),
        descriptor.scales_shape[0] as usize * 4
    );
}

#[test]
fn the_error_contract_is_measured_on_every_element() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let descriptor = store.load_view("q8").unwrap();
    let codes = store.read_tensor(descriptor.codes).unwrap();
    let scales = store.read_tensor(descriptor.scales).unwrap();
    let reconstructed = decode_q8_0("bf16", &codes, &scales, Q8_0_BLOCK).unwrap();
    let exact_bytes = store.read_tensor(exact).unwrap();
    let mut worst = 0f64;
    for index in 0..descriptor.element_count as usize {
        let a = weftfs::adapter::element_as_f32("bf16", &exact_bytes, index).unwrap();
        let b = weftfs::adapter::element_as_f32("bf16", &reconstructed, index).unwrap();
        worst = worst.max(((a as f64) - (b as f64)).abs());
    }
    assert!(worst <= store.resolve_view("q8").unwrap().error.max_bound);
    // A q8_0 view is expected to be visibly lossy on this fixture; if it were
    // exact the test would be meaningless.
    assert!(worst > 0.0);
}

#[test]
fn tampering_with_view_members_is_typed() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let descriptor = store.load_view("q8").unwrap();

    // Corrupt one code byte: the reconstruction root no longer matches.
    let mut codes = store.read_object(descriptor.codes).unwrap();
    codes[0] ^= 0x01;
    store.write_file(descriptor.codes, 0, &codes).unwrap();
    let err = store.resolve_view("q8").unwrap_err();
    assert!(
        err.class() == Class::Corrupt || err.class() == Class::DependencyMismatch,
        "{err}"
    );

    // Restore the member and instead change the exact source: the view's
    // recorded source content root no longer matches.
    let dir2 = tempdir().unwrap();
    let (mut clean, clean_exact) = setup(dir2.path());
    clean
        .write_quantized_view("q8", clean_exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let mut replacement = tensor_bytes();
    replacement[0] ^= 0xff;
    clean.write_file(clean_exact, 0, &replacement).unwrap();
    let err = clean.verify_view("q8").unwrap_err();
    assert_eq!(err.class(), Class::DependencyMismatch, "{err}");
}

#[test]
fn a_non_dividing_block_size_is_refused_without_padding() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    let err = store
        .write_quantized_view("q8", exact, 48, &ViewWriteOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::InvalidArgument, "{err}");
}

#[test]
fn a_non_tensor_source_is_refused() {
    let dir = tempdir().unwrap();
    let (mut store, _exact) = setup(dir.path());
    let plain = store
        .create_file(
            State::ROOT_ID,
            b"plain",
            b"not a tensor",
            0o644,
            0,
            0,
            Default::default(),
        )
        .unwrap();
    let err = store
        .write_quantized_view("bad", plain, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap_err();
    assert_eq!(err.class(), Class::UnsupportedRepresentation, "{err}");
}

#[test]
fn descriptors_reject_a_claim_of_exactness() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let descriptor = store.load_view("q8").unwrap();
    let mut value = descriptor.to_value();
    let weftcodec::wcb::Value::Map(map) = &mut value else {
        panic!("descriptor is a map")
    };
    map.insert("exact".to_owned(), weftcodec::wcb::Value::Bool(true));
    let bytes = weftcodec::wcb::encode_default(&value).unwrap();
    let err = ViewDescriptor::decode(&bytes).unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");
}

#[test]
fn q8_0_encodes_and_decodes_within_the_bound_on_any_shape() {
    for (dtype, element_bytes) in [("bf16", 2usize), ("f32", 4), ("f16", 2)] {
        let count = 128usize;
        let mut bytes = Vec::with_capacity(count * element_bytes);
        for index in 0..count {
            let value = ((index as f32) - 64.0) / 37.0;
            match dtype {
                "f32" => bytes.extend_from_slice(&value.to_le_bytes()),
                "bf16" => {
                    bytes.extend_from_slice(&weftfs::adapter::f32_to_bf16_bits(value).to_le_bytes())
                }
                _ => {
                    bytes.extend_from_slice(&weftfs::adapter::f32_to_element("f16", value).unwrap())
                }
            }
        }
        let view = encode_q8_0(dtype, &bytes, 32).unwrap();
        assert!(view.max_abs_error <= view.max_bound, "{dtype}");
        assert_eq!(view.clipped, 0);
        let back = decode_q8_0(dtype, &view.codes, &view.scales, 32).unwrap();
        assert_eq!(back, view.reconstructed);
    }
}

#[test]
fn the_bound_function_covers_the_measured_narrowing_error() {
    // The error contract is `scale/2 + half_ulp(dtype, q*scale)`. If `half_ulp`
    // understated the narrowing error, the contract would be false. Check it
    // against a dense sweep of values (including the worst cases near the
    // midpoint between two representable neighbours) for every dtype we encode.
    for dtype in ["bf16", "f16"] {
        for exponent in -30i32..=14 {
            let base = 2f32.powi(exponent);
            for step in 0..=2048usize {
                let value = base * (1.0 + step as f32 / 2048.0);
                let narrowed = weftfs::adapter::f32_to_element(dtype, value).unwrap();
                let back = weftfs::adapter::element_as_f32(dtype, &narrowed, 0).unwrap();
                let error = ((value as f64) - (back as f64)).abs();
                let bound = half_ulp(dtype, value);
                assert!(
                    error <= bound,
                    "{dtype} at {value:e} (exp {exponent}, step {step}): error {error:e} exceeds bound {bound:e}"
                );
            }
        }
    }
}

#[test]
fn identity_only_resolution_matches_the_strict_door() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let strict = store.resolve_view("q8").unwrap();
    let quick = store.resolve_view_bytes("q8").unwrap();
    assert_eq!(quick.bytes, strict.bytes);
    assert_eq!(quick.view_id, strict.view_id);
    assert_eq!(quick.descriptor, strict.descriptor);
    // Identity is still enforced: a changed code byte changes the
    // reconstruction root and both doors refuse it.
    let descriptor = store.load_view("q8").unwrap();
    let mut codes = store.read_object(descriptor.codes).unwrap();
    codes[0] ^= 0x01;
    store.write_file(descriptor.codes, 0, &codes).unwrap();
    assert_eq!(
        store.resolve_view_bytes("q8").unwrap_err().class(),
        Class::Corrupt
    );
    assert!(store.resolve_view("q8").is_err());
}

#[test]
fn views_survive_reopen() {
    let dir = tempdir().unwrap();
    let (mut store, exact) = setup(dir.path());
    let report = store
        .write_quantized_view("q8", exact, Q8_0_BLOCK, &ViewWriteOptions::default())
        .unwrap();
    let descriptor = store.load_view("q8").unwrap();
    let bytes = store.resolve_view("q8").unwrap().bytes;
    drop(store);
    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.load_view("q8").unwrap(), descriptor);
    assert_eq!(store.resolve_view("q8").unwrap().bytes, bytes);
    assert_eq!(store.verify_view("q8").unwrap().view_id, report.view_id);
    assert_eq!(store.resolve_exact_object(exact).unwrap(), tensor_bytes());
    let _ = fs::metadata(dir.path().join("store"));
}
