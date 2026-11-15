//! Typed causal adapters (LoRA/DoRA) and merged-view identity.
//!
//! The fixtures are tiny and synthetic: a two-tensor base checkpoint, a LoRA
//! adapter with one target, and a DoRA adapter with one target. Every number
//! here is a test fixture, not a performance claim.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::adapter::{
    element_as_f32, f32_to_bf16_bits, map_peft_key, merge_one_tensor, AdapterKind, AdapterManifest,
    AdapterSource, AdapterWriteOptions, MatrixPart, MergeOptions, PeftKey,
    MERGE_EQUIVALENCE_NOT_CLAIMED,
};
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::safetensors::{self, SafetensorsEntry, SafetensorsFile};
use weftfs::store::{OpenMode, Options, Store};

const BASE_A: &str = "model.layers.0.self_attn.q_proj.weight";
const BASE_B: &str = "model.layers.0.self_attn.k_proj.weight";
const NORM: &str = "model.norm.weight";
const IN_DIM: u64 = 16;
const OUT_DIM: u64 = 8;
const RANK: u64 = 2;

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

fn bf16_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for value in values {
        out.extend_from_slice(&f32_to_bf16_bits(*value).to_le_bytes());
    }
    out
}

fn base_values(len: usize, seed: f32) -> Vec<f32> {
    (0..len)
        .map(|index| {
            let x = (index as f32 + 1.0) * 0.0125 * seed;
            x.sin() * 0.75
        })
        .collect()
}

fn write_base(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let a = bf16_bytes(&base_values((OUT_DIM * IN_DIM) as usize, 1.0));
    let b = bf16_bytes(&base_values((OUT_DIM * IN_DIM) as usize, 2.0));
    let norm = bf16_bytes(&base_values(IN_DIM as usize, 3.0));
    let file = safetensors::encode_file(
        &[
            SafetensorsEntry {
                name: BASE_A,
                dtype: "bf16",
                shape: &[OUT_DIM, IN_DIM],
                data: &a,
            },
            SafetensorsEntry {
                name: BASE_B,
                dtype: "bf16",
                shape: &[OUT_DIM, IN_DIM],
                data: &b,
            },
            SafetensorsEntry {
                name: NORM,
                dtype: "bf16",
                shape: &[IN_DIM],
                data: &norm,
            },
        ],
        None,
    )
    .unwrap();
    let path = dir.join("base.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn write_adapter(dir: &Path, dora: bool, targets: &[&str]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let mut entries: Vec<(String, Vec<u64>, Vec<u8>)> = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let module = target.strip_suffix(".weight").unwrap();
        let prefix = format!("base_model.model.{module}");
        let a = bf16_bytes(&base_values((RANK * IN_DIM) as usize, 0.5 + index as f32));
        let b = bf16_bytes(&base_values((OUT_DIM * RANK) as usize, 0.25 + index as f32));
        entries.push((format!("{prefix}.lora_A.weight"), vec![RANK, IN_DIM], a));
        entries.push((format!("{prefix}.lora_B.weight"), vec![OUT_DIM, RANK], b));
        if dora {
            let magnitude = bf16_bytes(
                &(0..OUT_DIM)
                    .map(|row| 0.5 + (row as f32) * 0.1)
                    .collect::<Vec<_>>(),
            );
            entries.push((
                format!("{prefix}.lora_magnitude_vector"),
                vec![OUT_DIM],
                magnitude,
            ));
        }
    }
    let refs: Vec<SafetensorsEntry<'_>> = entries
        .iter()
        .map(|(name, shape, data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&refs, None).unwrap();
    let path = dir.join("adapter_model.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

fn source(path: &Path) -> AdapterSource {
    let bytes = fs::read(path).unwrap();
    AdapterSource {
        repo: "synthetic/fixture".to_owned(),
        revision: "fixture".to_owned(),
        file: "adapter_model.safetensors".to_owned(),
        url: "fixture://none".to_owned(),
        file_sha256: safetensors::sha256(&bytes),
        config_sha256: [7u8; 32],
    }
}

fn modules(targets: &[&str]) -> Vec<String> {
    targets
        .iter()
        .map(|target| target.trim_end_matches(".weight").to_owned())
        .collect()
}

fn setup(dir: &Path) -> (Store, PathBuf) {
    let mut store = Store::init(dir.join("store"), opts()).unwrap();
    let base = write_base(&dir.join("base-src"));
    store
        .write_checkpoint(
            "base",
            &base,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Planned,
                tile_shape: None,
                deterministic_seed: Some([42u8; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    (store, base)
}

fn write_lora(store: &mut Store, dir: &Path, name: &str, targets: &[&str]) -> PathBuf {
    let path = write_adapter(&dir.join(format!("{name}-src")), false, targets);
    let model = SafetensorsFile::parse(&path).unwrap();
    store
        .write_adapter(
            name,
            "base",
            &source(&path),
            AdapterKind::Lora,
            RANK,
            4.0,
            false,
            "none",
            &modules(targets),
            &model,
            &AdapterWriteOptions {
                record_provenance: true,
                ..AdapterWriteOptions::default()
            },
        )
        .unwrap();
    path
}

#[test]
fn adapter_round_trips_every_tensor_and_exposes_the_base() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A]);

    let manifest = store.load_adapter("chat").unwrap();
    assert_eq!(manifest.kind, AdapterKind::Lora);
    assert_eq!(manifest.rank, RANK);
    assert_eq!(manifest.alpha(), 4.0);
    assert_eq!(manifest.scaling(), 2.0);
    assert_eq!(manifest.targets.len(), 1);
    assert_eq!(manifest.targets[0].base_tensor, BASE_A);
    assert_eq!(
        manifest.adapter_bytes().unwrap(),
        RANK * IN_DIM * 2 + OUT_DIM * RANK * 2
    );

    let verification = store.verify_adapter("chat").unwrap();
    assert_eq!(verification.targets, 1);
    assert_eq!(verification.tensor_objects, 2);

    // The adapter bytes read back exactly what the PEFT container holds.
    let reader =
        SafetensorsFile::parse(&dir.path().join("chat-src/adapter_model.safetensors")).unwrap();
    for (part, suffix) in [("a", ".lora_A.weight"), ("b", ".lora_B.weight")] {
        let tensor = reader
            .tensors
            .iter()
            .find(|tensor| tensor.name.ends_with(suffix))
            .unwrap();
        let expected = reader.read_tensor(tensor).unwrap();
        let actual = store.read_adapter_tensor("chat", BASE_A, part).unwrap();
        assert_eq!(actual, expected, "{part} differs");
    }

    // The restored adapter container is byte-identical to the source.
    let mut restored = Vec::new();
    let report = store.restore_adapter("chat", &mut restored).unwrap();
    assert_eq!(report.tensors, 2);
    let original = fs::read(dir.path().join("chat-src/adapter_model.safetensors")).unwrap();
    assert_eq!(restored, original);
    assert_eq!(report.container_sha256, safetensors::sha256(&original));

    // Provenance names the base manifest as an input.
    let manifest_object = store.adapter_manifest_object("chat").unwrap();
    let record = store.provenance_of(manifest_object).unwrap().unwrap();
    assert_eq!(record.operation, "adapter-ingest");
    assert!(record
        .inputs
        .iter()
        .any(|input| input.role == "base-checkpoint-manifest"));
}

#[test]
fn adapter_identity_is_the_manifest_not_the_source_label() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "a", &[BASE_A]);
    write_lora(&mut store, dir.path(), "b", &[BASE_A]);
    let a = store.load_adapter("a").unwrap();
    let b = store.load_adapter("b").unwrap();
    // Different directory names are part of the manifest, so the identities
    // differ even for identical arithmetic.
    assert_ne!(a.id().unwrap(), b.id().unwrap());
    // The encoded manifest states the merge equivalence explicitly.
    let bytes = a.encode().unwrap();
    let decoded = AdapterManifest::decode(&bytes).unwrap();
    assert_eq!(decoded, a);
    let value = decoded.to_value();
    let Value::Map(map) = value else {
        panic!("manifest is a map")
    };
    assert_eq!(
        map.get("merge_equivalence"),
        Some(&Value::Str(MERGE_EQUIVALENCE_NOT_CLAIMED.to_owned()))
    );
}

#[test]
fn a_base_content_change_is_a_dependency_mismatch() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A]);
    let manifest = store.load_adapter("chat").unwrap();

    // Rewrite one target's recorded base content out of band.
    let mut tampered = manifest.clone();
    tampered.targets[0].base_content = weftfs::identity::Hash32::from_bytes([9u8; 32]);
    let manifest_object = store.adapter_manifest_object("chat").unwrap();
    store
        .write_file(manifest_object, 0, &tampered.encode().unwrap())
        .unwrap();
    let err = store.verify_adapter("chat").unwrap_err();
    assert_eq!(err.class(), Class::DependencyMismatch, "{err}");
}

#[test]
fn an_adapter_tensor_change_is_corrupt() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A]);
    let manifest = store.load_adapter("chat").unwrap();
    let object = manifest.targets[0].a.object;
    let mut bytes = store.read_object(object).unwrap();
    bytes[0] ^= 0xff;
    store.write_file(object, 0, &bytes).unwrap();
    let err = store.verify_adapter("chat").unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
}

#[test]
fn merging_reproduces_the_pinned_arithmetic_and_keeps_non_targets_by_reference() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A]);

    let report = store
        .merge_adapter(
            "chat",
            &MergeOptions {
                record_provenance: true,
                verify_root: true,
                ..MergeOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.merged_tensors, 1);
    assert_eq!(report.base_tensors, 3);

    let view = store.load_merged_view("chat").unwrap();
    assert_eq!(view.contract.order, "delta_then_add");
    assert_eq!(view.contract.dtype_accum, "f32");
    assert_eq!(view.contract.rounding, "round-half-to-even");
    assert_eq!(view.contract.scaling_bits, 2.0f64.to_bits());
    assert_eq!(view.contract.dtype_output, "bf16");
    assert_eq!(view.tensors.len(), 3);
    let merged_entry = view.tensors.iter().find(|t| t.name == BASE_A).unwrap();
    assert!(merged_entry.merged);
    let norm_entry = view.tensors.iter().find(|t| t.name == NORM).unwrap();
    assert!(!norm_entry.merged);
    assert!(norm_entry.object.is_none());

    // Independent recomputation of the same contract in the test.
    let base_manifest = store.checkpoint_manifest("base").unwrap();
    let base_tensor = base_manifest
        .tensors
        .iter()
        .find(|tensor| tensor.name == BASE_A)
        .unwrap();
    let base_bytes = store
        .restore_named_tensor_at(None, "base", BASE_A, None, None)
        .unwrap()
        .bytes;
    let a_bytes = store.read_adapter_tensor("chat", BASE_A, "a").unwrap();
    let b_bytes = store.read_adapter_tensor("chat", BASE_A, "b").unwrap();
    let expected = merge_one_tensor(
        AdapterKind::Lora,
        2.0,
        "bf16",
        &base_tensor.shape,
        &base_bytes,
        "bf16",
        &[RANK, IN_DIM],
        &a_bytes,
        "bf16",
        &[OUT_DIM, RANK],
        &b_bytes,
        None,
    )
    .unwrap();
    let stored = store.read_object(merged_entry.object.unwrap()).unwrap();
    assert_eq!(stored, expected);
    assert_ne!(stored, base_bytes);

    let verification = store.verify_merged_view("chat").unwrap();
    assert_eq!(verification.tensors, 3);
    assert_eq!(verification.merged_tensors, 1);
    assert_eq!(verification.base_tensors, 2);

    // The merged view is a distinct identity from both inputs.
    assert_ne!(report.merged_view, view.adapter);
    assert_ne!(report.merged_view, view.base.manifest_root);

    // Provenance names both the adapter and the base as inputs.
    let manifest_object = store.merged_view_manifest_object("chat").unwrap();
    let record = store.provenance_of(manifest_object).unwrap().unwrap();
    assert_eq!(record.operation, "adapter-merge");
    let roles: Vec<&str> = record
        .inputs
        .iter()
        .map(|input| input.role.as_str())
        .collect();
    assert!(roles.contains(&"adapter-manifest"));
    assert!(roles.contains(&"base-checkpoint-manifest"));

    // Restoring the view yields the base's non-target tensor verbatim.
    let mut restored = Vec::new();
    store.restore_merged_view("chat", &mut restored).unwrap();
    let path = dir.path().join("merged.safetensors");
    fs::write(&path, &restored).unwrap();
    let parsed = SafetensorsFile::parse(&path).unwrap();
    let norm = parsed.tensor(NORM).unwrap();
    let norm_bytes = parsed.read_tensor(norm).unwrap();
    assert_eq!(
        norm_bytes,
        store
            .restore_named_tensor_at(None, "base", NORM, None, None)
            .unwrap()
            .bytes
    );
    let merged = parsed.tensor(BASE_A).unwrap();
    assert_eq!(parsed.read_tensor(merged).unwrap(), expected);
}

#[test]
fn two_stores_with_the_same_fixture_publish_the_same_view_identity() {
    let dir = tempdir().unwrap();
    let (mut first, _) = setup(&dir.path().join("one"));
    write_lora(&mut first, &dir.path().join("one"), "chat", &[BASE_A]);
    let a = first
        .merge_adapter("chat", &MergeOptions::default())
        .unwrap();

    let (mut second, _) = setup(&dir.path().join("two"));
    write_lora(&mut second, &dir.path().join("two"), "chat", &[BASE_A]);
    let b = second
        .merge_adapter("chat", &MergeOptions::default())
        .unwrap();

    assert_eq!(a.merged_view, b.merged_view);
    let view_a = first.load_merged_view("chat").unwrap();
    let view_b = second.load_merged_view("chat").unwrap();
    assert_eq!(view_a.encode().unwrap(), view_b.encode().unwrap());
}

#[test]
fn dora_applies_the_magnitude_and_row_norm() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    let path = write_adapter(&dir.path().join("dora-src"), true, &[BASE_A]);
    let model = SafetensorsFile::parse(&path).unwrap();
    store
        .write_adapter(
            "dora",
            "base",
            &source(&path),
            AdapterKind::Dora,
            RANK,
            4.0,
            false,
            "none",
            &modules(&[BASE_A]),
            &model,
            &AdapterWriteOptions::default(),
        )
        .unwrap();
    let manifest = store.load_adapter("dora").unwrap();
    assert_eq!(manifest.kind, AdapterKind::Dora);
    assert!(manifest.targets[0].magnitude.is_some());

    let report = store
        .merge_adapter("dora", &MergeOptions::default())
        .unwrap();
    assert_eq!(report.merged_tensors, 1);
    let view = store.load_merged_view("dora").unwrap();
    assert_eq!(view.contract.order, "delta_then_add_then_row_normalize");
    assert_eq!(view.contract.dora_norm, "l2_over_input_dim");

    // Each merged row must have L2 norm equal to the magnitude value.
    let entry = view.tensors.iter().find(|t| t.name == BASE_A).unwrap();
    let merged = store.read_object(entry.object.unwrap()).unwrap();
    let magnitude = store
        .read_adapter_tensor("dora", BASE_A, "magnitude")
        .unwrap();
    for row in 0..OUT_DIM as usize {
        let mut sum_sq = 0f64;
        for column in 0..IN_DIM as usize {
            let value = element_as_f32("bf16", &merged, row * IN_DIM as usize + column).unwrap();
            sum_sq += (value as f64) * (value as f64);
        }
        let expected = element_as_f32("bf16", &magnitude, row).unwrap() as f64;
        let got = sum_sq.sqrt();
        assert!(
            (got - expected).abs() <= 2e-3 * expected.max(1.0),
            "row {row}: norm {got} vs magnitude {expected}"
        );
    }
}

#[test]
fn structural_refusals_are_typed() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());

    // A key that is not an adapter matrix is refused, never dropped.
    let mut entries = vec![
        (
            "base_model.model.model.layers.0.self_attn.q_proj.lora_A.weight".to_owned(),
            vec![RANK, IN_DIM],
            bf16_bytes(&base_values((RANK * IN_DIM) as usize, 1.0)),
        ),
        (
            "base_model.model.model.layers.0.self_attn.q_proj.lora_B.weight".to_owned(),
            vec![OUT_DIM, RANK],
            bf16_bytes(&base_values((OUT_DIM * RANK) as usize, 2.0)),
        ),
        (
            "base_model.model.model.layers.0.mlp.down_proj.modules_to_save.default.weight"
                .to_owned(),
            vec![OUT_DIM, RANK],
            bf16_bytes(&base_values((OUT_DIM * RANK) as usize, 3.0)),
        ),
    ];
    let refs: Vec<SafetensorsEntry<'_>> = entries
        .iter()
        .map(|(name, shape, data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&refs, None).unwrap();
    let path = dir.path().join("extra.safetensors");
    fs::write(&path, &file).unwrap();
    let model = SafetensorsFile::parse(&path).unwrap();
    let err = store
        .write_adapter(
            "bad",
            "base",
            &source(&path),
            AdapterKind::Lora,
            RANK,
            4.0,
            false,
            "none",
            &modules(&[BASE_A]),
            &model,
            &AdapterWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // A DoRA adapter without a magnitude is refused.
    let path = write_adapter(&dir.path().join("lora-only"), false, &[BASE_A]);
    let model = SafetensorsFile::parse(&path).unwrap();
    let err = store
        .write_adapter(
            "bad-dora",
            "base",
            &source(&path),
            AdapterKind::Dora,
            RANK,
            4.0,
            false,
            "none",
            &modules(&[BASE_A]),
            &model,
            &AdapterWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Malformed, "{err}");

    // A target the base does not have is refused.
    let unknown = "model.layers.9.mlp.up_proj.weight";
    let path = write_adapter(&dir.path().join("unknown"), false, &[unknown]);
    let model = SafetensorsFile::parse(&path).unwrap();
    let err = store
        .write_adapter(
            "bad-target",
            "base",
            &source(&path),
            AdapterKind::Lora,
            RANK,
            4.0,
            false,
            "none",
            &modules(&[unknown]),
            &model,
            &AdapterWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::NotFound, "{err}");
}

#[test]
fn peft_keys_map_to_base_tensor_names() {
    assert_eq!(
        map_peft_key("base_model.model.model.layers.0.self_attn.q_proj.lora_A.weight"),
        PeftKey::Matrix {
            base_tensor: BASE_A.to_owned(),
            which: MatrixPart::A,
        }
    );
    assert_eq!(
        map_peft_key("model.layers.0.self_attn.q_proj.lora_B.weight"),
        PeftKey::Matrix {
            base_tensor: BASE_A.to_owned(),
            which: MatrixPart::B,
        }
    );
    assert_eq!(
        map_peft_key("base_model.model.model.layers.0.self_attn.q_proj.lora_B.default.weight"),
        PeftKey::Matrix {
            base_tensor: BASE_A.to_owned(),
            which: MatrixPart::B,
        }
    );
    assert_eq!(
        map_peft_key("base_model.model.model.layers.0.self_attn.q_proj.lora_magnitude_vector"),
        PeftKey::Matrix {
            base_tensor: BASE_A.to_owned(),
            which: MatrixPart::Magnitude,
        }
    );
    assert_eq!(
        map_peft_key(
            "base_model.model.model.layers.0.mlp.down_proj.modules_to_save.default.weight"
        ),
        PeftKey::Other
    );
}

#[test]
fn f16_subnormals_decode_to_their_exact_values() {
    // 0x0001 is 2^-24, 0x03ff is (1023/1024) * 2^-14, 0x0400 is the smallest
    // normal. A missing implicit-bit step would halve every one of these.
    for (bits, expected) in [
        (0x0001u16, 2f32.powi(-24)),
        (0x0002, 2f32.powi(-23)),
        (0x03ff, (1023.0 / 1024.0) * 2f32.powi(-14)),
        (0x0400, 2f32.powi(-14)),
        (0x3c00, 1.0),
        (0xc000, -2.0),
    ] {
        let bytes = bits.to_le_bytes();
        let got = element_as_f32("f16", &bytes, 0).unwrap();
        assert_eq!(got.to_bits(), expected.to_bits(), "f16 {bits:#06x}");
    }
    // Narrowing the same values must round-trip exactly.
    for (bits, value) in [
        (0x0001u16, 2f32.powi(-24)),
        (0x03ff, (1023.0 / 1024.0) * 2f32.powi(-14)),
        (0x3c00, 1.0),
    ] {
        let narrowed = weftfs::adapter::f32_to_element("f16", value).unwrap();
        assert_eq!(narrowed, bits.to_le_bytes().to_vec(), "f16 {bits:#06x}");
    }
    // Rounding around the smallest subnormal: the exact midpoint 2^-25 ties to
    // the even value (zero), and anything above it becomes 0x0001.
    let zero = weftfs::adapter::f32_to_element("f16", 2f32.powi(-25)).unwrap();
    assert_eq!(zero, 0x0000u16.to_le_bytes().to_vec());
    let above = weftfs::adapter::f32_to_element("f16", 2f32.powi(-25) * 1.01).unwrap();
    assert_eq!(above, 0x0001u16.to_le_bytes().to_vec());
    let below = weftfs::adapter::f32_to_element("f16", 2f32.powi(-25) * 0.99).unwrap();
    assert_eq!(below, 0x0000u16.to_le_bytes().to_vec());
}

#[test]
fn bf16_rounding_is_nearest_even() {
    // Exact values round-trip.
    for value in [0.0f32, 1.0, -2.5, 0.5, 2.0] {
        let bits = f32_to_bf16_bits(value);
        let back = f32::from_bits((bits as u32) << 16);
        assert_eq!(back, value, "lossless bf16 values must round-trip");
    }
    // A value exactly between two bf16 values rounds to the even one.
    let lower = f32::from_bits(0x3f80_0000);
    let upper = f32::from_bits(0x3f81_0000);
    let middle = (lower.to_bits() + upper.to_bits()) / 2;
    let tied = f32::from_bits(middle);
    let bits = f32_to_bf16_bits(tied);
    assert_eq!(bits & 1, 0, "a tie must round to even");
}

#[test]
fn writing_the_same_adapter_twice_is_refused() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    let path = write_lora(&mut store, dir.path(), "chat", &[BASE_A]);
    let model = SafetensorsFile::parse(&path).unwrap();
    let err = store
        .write_adapter(
            "chat",
            "base",
            &source(&path),
            AdapterKind::Lora,
            RANK,
            4.0,
            false,
            "none",
            &modules(&[BASE_A]),
            &model,
            &AdapterWriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Exists, "{err}");
}

#[test]
fn an_adapter_restores_the_source_container_byte_for_byte() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    let path = write_lora(&mut store, dir.path(), "chat", &[BASE_A, BASE_B]);
    let source_bytes = fs::read(&path).unwrap();
    let mut restored = Vec::new();
    let report = store.restore_adapter("chat", &mut restored).unwrap();
    assert!(report.byte_exact);
    assert_eq!(restored, source_bytes);
    assert_eq!(report.container_bytes, source_bytes.len() as u64);
    assert_eq!(
        report.container_sha256.to_vec(),
        safetensors::sha256(&source_bytes).to_vec()
    );
    // The manifest records the header verbatim, so the restore does not depend
    // on re-encoding a header and hoping it matches.
    let manifest = store.load_adapter("chat").unwrap();
    let container = manifest.source_container.as_ref().unwrap();
    assert_eq!(container.file_bytes, source_bytes.len() as u64);
    assert_eq!(
        &source_bytes[..container.header.len()],
        &container.header[..]
    );
}

#[test]
fn a_recorded_source_header_that_no_longer_matches_fails_closed() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A]);
    // Keep every tensor byte and every offset, but change one header character.
    // The tensors would still restore exactly; the *container* would not, so a
    // byte-exact restore must refuse instead of claiming success.
    let manifest_object = store.adapter_manifest_object("chat").unwrap();
    let mut value = weftcodec::wcb::decode(
        &store.read_object(manifest_object).unwrap(),
        &weftcodec::Limits::default(),
    )
    .unwrap();
    let weftcodec::wcb::Value::Map(map) = &mut value else {
        panic!("manifest is a map")
    };
    let weftcodec::wcb::Value::Map(container) = map.get_mut("source_container").unwrap() else {
        panic!("source_container is a map")
    };
    let weftcodec::wcb::Value::Bytes(header) = container.get_mut("header").unwrap() else {
        panic!("header is bytes")
    };
    header[8] = b'[';
    let bytes = weftcodec::wcb::encode_default(&value).unwrap();
    store.write_file(manifest_object, 0, &bytes).unwrap();
    let err = store.restore_adapter("chat", &mut Vec::new()).unwrap_err();
    assert_eq!(err.class(), Class::Corrupt, "{err}");
    assert!(err.message().contains("cannot be reproduced"), "{err}");
}

#[test]
fn reopened_store_verifies_adapter_and_view() {
    let dir = tempdir().unwrap();
    let (mut store, _base) = setup(dir.path());
    write_lora(&mut store, dir.path(), "chat", &[BASE_A, BASE_B]);
    store
        .merge_adapter("chat", &MergeOptions::default())
        .unwrap();
    let adapter = store.load_adapter("chat").unwrap().id().unwrap();
    let view = store.load_merged_view("chat").unwrap().id().unwrap();
    drop(store);

    let store = Store::open(dir.path().join("store"), OpenMode::ReadOnly).unwrap();
    assert_eq!(store.load_adapter("chat").unwrap().id().unwrap(), adapter);
    assert_eq!(store.load_merged_view("chat").unwrap().id().unwrap(), view);
    assert_eq!(store.verify_adapter("chat").unwrap().targets, 2);
    assert_eq!(store.verify_merged_view("chat").unwrap().merged_tensors, 2);
    assert_eq!(store.list_adapters().unwrap(), vec!["chat".to_owned()]);
    let mut restored = Vec::new();
    store.restore_adapter("chat", &mut restored).unwrap();
    assert!(!restored.is_empty());
    let mut merged = Vec::new();
    store.restore_merged_view("chat", &mut merged).unwrap();
    assert!(!merged.is_empty());
    let _ = BTreeMap::<String, Value>::new();
}
