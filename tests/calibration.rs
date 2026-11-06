//! R12: host-local decode-cost calibration.
//!
//! The loader is strict by design: anything malformed, incomplete, duplicated,
//! out of range, non-monotonic, or digest-mismatched is rejected before the
//! planner sees a single row. These tests also prove the planner prices with
//! the file's rows and that behaviour is unchanged when no file is supplied.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value as Json};
use weftfs::calibration::{
    Calibration, CalibrationMethod, CalibrationRow, HostIdentity, RowOrigin, MAX_CALIBRATION_BYTES,
    MAX_FIXED_NS, MAX_NS_PER_MIB,
};
use weftfs::cost_model::{estimate_ns, CostClass, CostModelKind};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy};
use weftfs::{RebaseOptions, TensorWriteSpec};

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "weft-calibration-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    path
}

fn temp_store(label: &str) -> (PathBuf, Store) {
    let path = temp_dir(label);
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

fn host() -> HostIdentity {
    HostIdentity {
        hostname: "calibration-test".to_owned(),
        kernel: "test-kernel".to_owned(),
        arch: "test-arch".to_owned(),
        cpus: 4,
        image: "test-image".to_owned(),
        image_id: "sha256:test".to_owned(),
        memory_limit: "3g".to_owned(),
        pids_limit: "1024".to_owned(),
        weftfs_revision: "test".to_owned(),
        weftcodec_revision: "test".to_owned(),
    }
}

fn method() -> CalibrationMethod {
    CalibrationMethod {
        reps: 7,
        warmup_reads: 1,
        small_bytes: 128 * 1024,
        large_bytes: 16 * 1_048_576,
    }
}

fn row(
    class: CostClass,
    fixed_ns: u64,
    ns_per_mib: u64,
    samples: u64,
    origin: RowOrigin,
) -> CalibrationRow {
    CalibrationRow {
        class,
        fixed_ns,
        ns_per_mib,
        samples,
        origin,
    }
}

/// A complete, valid record. `raw_ns` and `zstd_ns` are exposed so a planner
/// test can make one representation obviously cheaper than the other.
fn record_with(raw_ns: u64, zstd_ns: u64) -> Calibration {
    let rows = [
        row(CostClass::Raw, 10_000, raw_ns, 14, RowOrigin::Measured),
        row(CostClass::Zstd, 20_000, zstd_ns, 14, RowOrigin::Measured),
        row(
            CostClass::FloatTensor,
            15_000,
            4_000_000,
            14,
            RowOrigin::Measured,
        ),
        row(
            CostClass::TensorSharded,
            15_000,
            9_000_000,
            14,
            RowOrigin::Measured,
        ),
        row(
            CostClass::TensorTiled,
            15_000,
            3_000_000,
            14,
            RowOrigin::Measured,
        ),
        row(
            CostClass::WeightDelta,
            30_000,
            5_000_000,
            14,
            RowOrigin::Measured,
        ),
        row(
            CostClass::SparseDelta,
            30_000,
            5_000_000,
            0,
            RowOrigin::Reused,
        ),
        row(
            CostClass::SemanticDelta,
            30_000,
            6_000_000,
            14,
            RowOrigin::Measured,
        ),
        row(
            CostClass::KvCache,
            100_000,
            1_048_576,
            0,
            RowOrigin::Declared,
        ),
        row(
            CostClass::Activation,
            100_000,
            1_048_576,
            0,
            RowOrigin::Declared,
        ),
    ];
    Calibration::from_rows(rows, host(), method(), 1_762_000_000_000_000_000).unwrap()
}

fn valid_record() -> Calibration {
    record_with(300_000, 800_000)
}

/// Recompute the digest after an intentional semantic edit, so row validation
/// is exercised independently of digest validation.
fn resign(mut document: Json) -> Json {
    document.as_object_mut().unwrap().remove("digest");
    let bytes = serde_json::to_vec(&document).unwrap();
    let id = weftfs::identity::h(weftfs::calibration::CALIBRATION_DOMAIN, &bytes);
    document["digest"] = json!(id.to_hex());
    document
}

fn expect_reject(document: &Json, needle: &str) {
    let err = Calibration::from_json(document).expect_err("document must be rejected");
    assert_eq!(
        err.class(),
        weftfs::Class::InvalidArgument,
        "wrong class for `{needle}`: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains(needle),
        "error for `{needle}` does not mention it: {text}"
    );
}

#[test]
fn record_round_trips_through_a_file() {
    let directory = temp_dir("round-trip");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("calibration.json");
    let record = valid_record();
    record.write(&path).unwrap();
    let loaded = Calibration::load(&path).unwrap();
    assert_eq!(loaded.id, record.id);
    assert_eq!(loaded.host, record.host);
    assert_eq!(loaded.method, record.method);
    assert_eq!(loaded.created_unix_ns, record.created_unix_ns);
    for class in CostClass::ALL {
        assert_eq!(loaded.row(class), record.row(class), "{class:?}");
        assert_eq!(
            loaded.estimate_ns(class, 1_048_576),
            record.estimate_ns(class, 1_048_576)
        );
    }
    // Re-serialising the loaded document reproduces the same digest.
    let reparsed = Calibration::from_json(&loaded.to_json()).unwrap();
    assert_eq!(reparsed.id, record.id);

    // `install` is content-addressed and idempotent.
    let store_dir = temp_dir("install-store");
    fs::create_dir_all(&store_dir).unwrap();
    let first = record.install(&store_dir).unwrap();
    let second = record.install(&store_dir).unwrap();
    assert_eq!(first, second);
    assert!(first.starts_with(store_dir.join("calibrations")));
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&record.id.to_hex()));
    let _ = fs::remove_dir_all(&directory);
    let _ = fs::remove_dir_all(&store_dir);
}

#[test]
fn malformed_documents_fail_closed() {
    let good = valid_record().to_json();

    let mut wrong_schema = good.clone();
    wrong_schema["schema"] = json!("weft.calibration.v0");
    expect_reject(&wrong_schema, "schema");

    let mut missing_row = good.clone();
    missing_row["rows"].as_array_mut().unwrap().pop();
    expect_reject(&missing_row, "rows has 9 entries");

    let mut duplicate_row = good.clone();
    let first = good["rows"][0].clone();
    duplicate_row["rows"][1] = first;
    expect_reject(&duplicate_row, "duplicate row");

    let mut unknown_class = good.clone();
    unknown_class["rows"][0]["class"] = json!("not-a-class");
    expect_reject(&unknown_class, "unknown cost class");

    let mut unknown_origin = good.clone();
    unknown_origin["rows"][0]["origin"] = json!("guessed");
    expect_reject(&unknown_origin, "unknown row origin");

    let mut unknown_key = good.clone();
    unknown_key["surprise"] = json!(1);
    expect_reject(&unknown_key, "unknown key");

    let mut missing_key = good.clone();
    missing_key.as_object_mut().unwrap().remove("method");
    expect_reject(&missing_key, "missing key `method`");

    let mut zero_row = good.clone();
    zero_row["rows"][0]["ns_per_mib"] = json!(0u64);
    expect_reject(&resign(zero_row), "must be greater than zero");

    let mut huge_row = good.clone();
    huge_row["rows"][0]["ns_per_mib"] = json!(MAX_NS_PER_MIB + 1);
    expect_reject(&resign(huge_row), "ns/MiB bound");

    let mut huge_fixed = good.clone();
    huge_fixed["rows"][0]["fixed_ns"] = json!(MAX_FIXED_NS + 1);
    expect_reject(&resign(huge_fixed), "ns bound");

    let mut too_few_samples = good.clone();
    too_few_samples["rows"][0]["samples"] = json!(4u64);
    expect_reject(&resign(too_few_samples), "below 5");

    let mut declared_samples = good.clone();
    declared_samples["rows"][8]["samples"] = json!(7u64);
    expect_reject(
        &resign(declared_samples),
        "only measured rows carry samples",
    );

    let mut bad_method = good.clone();
    bad_method["method"]["reps"] = json!(2u64);
    expect_reject(&bad_method, "at least 5 repetitions");

    let mut bad_fit = good.clone();
    bad_fit["method"]["fit"] = json!("something else");
    expect_reject(&bad_fit, "documented affine fit");

    let mut negative = good.clone();
    negative["rows"][0]["fixed_ns"] = json!(-1);
    expect_reject(&negative, "unsigned integer");

    let mut wrong_digest = good.clone();
    wrong_digest["rows"][0]["ns_per_mib"] = json!(123_456u64);
    expect_reject(&wrong_digest, "digest mismatch");

    let mut no_digest = good.clone();
    no_digest.as_object_mut().unwrap().remove("digest");
    expect_reject(&no_digest, "missing key `digest`");

    expect_reject(&json!([]), "must be a JSON object");
}

#[test]
fn digest_is_over_the_canonical_document_not_the_file_whitespace() {
    let record = valid_record();
    let directory = temp_dir("whitespace");
    fs::create_dir_all(&directory).unwrap();
    let compact = directory.join("compact.json");
    let pretty = directory.join("pretty.json");
    fs::write(&compact, serde_json::to_vec(&record.to_json()).unwrap()).unwrap();
    fs::write(
        &pretty,
        serde_json::to_vec_pretty(&record.to_json()).unwrap(),
    )
    .unwrap();
    let a = Calibration::load(&compact).unwrap();
    let b = Calibration::load(&pretty).unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(a.id, record.id);
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn oversized_documents_are_refused_before_parsing() {
    let directory = temp_dir("oversized");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("huge.json");
    let bytes = vec![b' '; MAX_CALIBRATION_BYTES as usize + 1];
    fs::write(&path, bytes).unwrap();
    let err = Calibration::load(&path).unwrap_err();
    assert_eq!(err.class(), weftfs::Class::InvalidArgument);
    assert!(err.to_string().contains("byte bound"), "{err}");
    let _ = fs::remove_dir_all(&directory);
}

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

#[test]
fn planner_uses_the_calibration_rows() {
    let (path, store) = temp_store("planner");
    let elements = 512 * 1024; // 1 MiB BF16
    let bytes = bf16_payload(elements);
    let shape = vec![512u64, 1024u64];
    let layout = TensorLayoutSpec::row_major();
    let size = bytes.len() as u64;

    // A deliberately lopsided host table: zstd costs 1 ns/MiB, raw costs the
    // maximum legal amount. With a large lambda the decode term dominates and
    // only this table can select zstd.
    let record = record_with(MAX_NS_PER_MIB, 1);
    let table = record.table();

    let plan = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions {
                lambda_ns: 1_000_000_000,
                calibration: Some(table.clone()),
                ..TensorPlanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(plan.chosen, TensorStrategy::Zstd);
    assert_eq!(
        plan.chosen_estimated_decode_ns,
        record.estimate_ns(CostClass::Zstd, size)
    );
    assert_eq!(
        plan.chosen_estimated_decode_ns,
        20_000 + (size * 1) / 1_048_576,
        "the zstd row's fixed_ns plus 1 ns/MiB at 1 MiB"
    );
    // Every candidate estimate comes from the file, not the baked table.
    for candidate in &plan.candidates {
        if let Some(estimated) = candidate.estimated_decode_ns {
            assert_eq!(
                estimated,
                record.estimate_ns(candidate.strategy.cost_class(), size),
                "{} did not use the calibration row",
                candidate.strategy.as_str()
            );
        }
    }
    // The baked table would not produce this estimate.
    assert_ne!(
        record.estimate_ns(CostClass::Zstd, size),
        estimate_ns(CostClass::Zstd, size, CostModelKind::Measured)
    );

    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn default_behaviour_is_unchanged_without_a_calibration() {
    let (path, store) = temp_store("default");
    let elements = 512 * 1024;
    let bytes = bf16_payload(elements);
    let shape = vec![512u64, 1024u64];
    let layout = TensorLayoutSpec::row_major();
    let size = bytes.len() as u64;

    let implicit = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions::default(),
        )
        .unwrap();
    let explicit = store
        .plan_tensor(
            "bf16",
            &shape,
            &bytes,
            &layout,
            &[],
            &TensorPlanOptions {
                cost_model: CostModelKind::Measured,
                calibration: None,
                ..TensorPlanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(implicit.chosen, explicit.chosen);
    assert_eq!(
        implicit.chosen_estimated_decode_ns,
        estimate_ns(implicit.chosen.cost_class(), size, CostModelKind::Measured)
    );
    assert_eq!(
        implicit.chosen_estimated_decode_ns,
        explicit.chosen_estimated_decode_ns
    );

    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn rebase_prices_a_delta_chain_with_the_calibration_rows() {
    let (path, mut store) = temp_store("rebase");
    let root = store.resolve_path("/").unwrap();
    let layout = TensorLayoutSpec::row_major();
    let elements = 512 * 1024; // 1 MiB BF16
    let shape = vec![512u64, 1024u64];
    let base_bytes = bf16_payload(elements);
    let mut target_bytes = base_bytes.clone();
    for index in (0..elements).step_by(4096) {
        target_bytes[index * 2] ^= 1;
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
    let delta_id = store
        .write_tensor_file_with_spec(
            root,
            b"instruct",
            "bf16",
            &shape,
            &target_bytes,
            &layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::WeightDelta),
                causal: vec![base_version],
                ..TensorWriteSpec::default()
            },
        )
        .unwrap();
    for _ in 0..12 {
        store.read_tensor(delta_id).unwrap();
    }
    store.flush_access_counts().unwrap();

    // A table with a very expensive weight-delta row makes the hot delta
    // unambiguously above the threshold.
    let record = record_with(300_000, 800_000);
    let plan = store
        .plan_rebase(
            root,
            &RebaseOptions {
                threshold_ns: 1_000_000,
                calibration: Some(record.table()),
                dry_run: true,
                ..RebaseOptions::default()
            },
        )
        .unwrap();
    let candidate = plan
        .candidates
        .iter()
        .find(|candidate| candidate.object == delta_id)
        .expect("the hot delta is a candidate");
    let size = target_bytes.len() as u64;
    assert_eq!(
        candidate.estimated_decode_ns,
        record.estimate_ns(CostClass::WeightDelta, size)
            + record.estimate_ns(CostClass::FloatTensor, base_bytes.len() as u64),
        "the current chain must be priced with the calibration's incremental delta row plus its base row"
    );
    // Same decision with no writes.
    assert!(plan
        .candidates
        .iter()
        .all(|candidate| candidate.object == delta_id));

    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn install_is_ignored_by_store_replay_and_verify() {
    // Installing a calibration must not change the store's logical state or
    // make verify fail: `calibrations/` is advisory, like `indexes/`.
    let (path, mut store) = temp_store("install-advisory");
    let root = store.resolve_path("/").unwrap();
    store.mkdir(root, b"dir", 0o755, 0, 0).unwrap();
    let record = valid_record();
    record.install(&path).unwrap();
    let head = store.head();
    let generation = store.generation();
    store.verify(true).unwrap();
    drop(store);
    let reopened = Store::open(&path, OpenMode::ReadOnly).unwrap();
    assert_eq!(reopened.head(), head);
    assert_eq!(reopened.generation(), generation);
    reopened.verify(true).unwrap();
    let _ = fs::remove_dir_all(&path);
}
