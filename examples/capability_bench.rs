//! D2 authenticated capability envelope benchmark.
//!
//! Measures:
//!
//! * offline envelope verification: 1,000 signed envelopes, first pass
//!   ("cold") and repeated pass ("warm");
//! * signed resolve overhead versus unsigned resolve on one store;
//! * cross-tenant isolation work-unit equality for the same tenant A with a
//!   small and a large tenant-B population.
//!
//! Run only inside the OOM-protected bench court. The workload is synthetic
//! and single-host. The receipt is labelled accordingly.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};
use weftcodec::context::ContextRoot;
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::identity::{self, Hash32};
use weftfs::store::{Options, SetAttr, Store};

const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const ENVELOPES: usize = 1_000;
const STORE_OBJECTS: usize = 1_000;
const RESOLVE_SAMPLES: usize = 50;
const A_UID: u64 = 1001;
const B_UID: u64 = 1002;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        ..Options::default()
    }
}

fn seed_byte(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn hash(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

fn timing_summary(values: &[u64]) -> Json {
    if values.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let sum: u64 = sorted.iter().sum();
    let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
    json!({
        "samples": sorted.len(),
        "min_ns": sorted[0],
        "median_ns": sorted[sorted.len() / 2],
        "mean_ns": sum / sorted.len() as u64,
        "p95_ns": p95,
        "max_ns": sorted[sorted.len() - 1],
    })
}

fn make_envelope(index: usize, root: Option<Hash32>) -> CapabilityEnvelope {
    let grantor = public_key_for_seed(&seed_byte(7)).unwrap();
    let nonce = identity::h("weft/bench/capability-nonce", &(index as u64).to_le_bytes());
    let revocation = identity::h(
        "weft/bench/capability-revocation",
        &(index as u64).to_le_bytes(),
    );
    CapabilityEnvelope::new(
        grantor,
        hash(0x42),
        format!("tenant-{}", index % 16),
        root.into_iter().collect(),
        vec!["default".to_owned()],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        revocation,
        FIXED_TIMESTAMP_NS,
        nonce,
    )
    .unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = value_of(&args, "--out").unwrap_or_else(|| "capability-envelope.json".to_owned());
    if let Err(err) = run(Path::new(&out)) {
        eprintln!("capability_bench: {err}");
        std::process::exit(1);
    }
}

fn run(out: &Path) -> Result<(), String> {
    let dir = PathBuf::from(format!("/tmp/weft-capability-bench-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }

    // ---- envelope verification: 1,000 envelopes --------------------------
    let mut blobs = Vec::with_capacity(ENVELOPES);
    for index in 0..ENVELOPES {
        let envelope = make_envelope(index, None);
        let blob = sign_envelope(&envelope, &seed_byte(7)).map_err(|err| err.to_string())?;
        blobs.push(blob.encode().map_err(|err| err.to_string())?);
    }
    let mut cold = Vec::with_capacity(ENVELOPES);
    for blob in &blobs {
        let start = Instant::now();
        verify_envelope(blob, &[], FIXED_TIMESTAMP_NS, None).map_err(|err| err.to_string())?;
        cold.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let mut warm = Vec::with_capacity(ENVELOPES * 3);
    for _ in 0..3 {
        for blob in &blobs {
            let start = Instant::now();
            verify_envelope(blob, &[], FIXED_TIMESTAMP_NS, None).map_err(|err| err.to_string())?;
            warm.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    // ---- signed versus unsigned resolve ----------------------------------
    let store_dir = dir.join("store");
    let mut store = Store::init(&store_dir, options()).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    for index in 0..STORE_OBJECTS {
        let name = format!("object-{index:04}.txt");
        let payload = format!("capability bench payload {index}");
        store
            .create_file_seeded(
                root,
                name.as_bytes(),
                payload.as_bytes(),
                0o644,
                0,
                0,
                attrs(&[("project", "capability-bench")]),
                format!("capability-bench-object-{index}").as_bytes(),
            )
            .map_err(|err| err.to_string())?;
    }
    let snapshot = store
        .snapshot_create("bench")
        .map_err(|err| err.to_string())?;
    let selector = selector::parse("select attrs.project == \"capability-bench\"")
        .map_err(|err| err.to_string())?;
    let context = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .map_err(|err| err.to_string())?;
    store
        .save_context_root("bench", &context, None, false)
        .map_err(|err| err.to_string())?;
    let context_id = context.id().map_err(|err| err.to_string())?;

    let signed_envelope = make_envelope(usize::MAX, Some(context_id));
    let signed_blob = sign_envelope(&signed_envelope, &seed_byte(7))
        .map_err(|err| err.to_string())?
        .encode()
        .map_err(|err| err.to_string())?;
    let verified = verify_envelope(&signed_blob, &[], FIXED_TIMESTAMP_NS, None)
        .map_err(|err| err.to_string())?;

    let unsigned_principal = Principal::root();
    let signed_principal = Principal::root().with_envelope(verified);
    let limits = weftcodec::Limits::default();
    let now = store.clock_now_ns();

    let mut unsigned_ns = Vec::with_capacity(RESOLVE_SAMPLES);
    let mut signed_ns = Vec::with_capacity(RESOLVE_SAMPLES);
    let mut unsigned_work = 0;
    let mut signed_work = 0;
    let mut matched = 0;
    for _ in 0..RESOLVE_SAMPLES {
        let start = Instant::now();
        let resolved = store
            .resolve_context("bench", None, &unsigned_principal, &limits, now)
            .map_err(|err| err.to_string())?;
        unsigned_ns.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        unsigned_work = resolved.work_units;
        matched = resolved.resolution.matched_count;

        let start = Instant::now();
        let resolved = store
            .resolve_context("bench", None, &signed_principal, &limits, now)
            .map_err(|err| err.to_string())?;
        signed_ns.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        signed_work = resolved.work_units;
    }
    let unsigned_median = {
        let mut sorted = unsigned_ns.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    };
    let signed_median = {
        let mut sorted = signed_ns.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    };
    let overhead_ns = signed_median.saturating_sub(unsigned_median);
    let overhead_percent = if unsigned_median == 0 {
        0.0
    } else {
        (overhead_ns as f64 / unsigned_median as f64) * 100.0
    };

    // Keep the unsigned/signed principals alive to the end of the measurement.
    drop(unsigned_principal);
    drop(signed_principal);

    // ---- isolation work-unit equality -----------------------------------
    let small = isolation_work_units(dir.join("isolation-small"), 0)?;
    let large = isolation_work_units(dir.join("isolation-large"), 40)?;

    let receipt = json!({
        "schema": "weft.capability-envelope.bench.v1",
        "label": "Synthetic, single host, Docker bench court with hard memory/CPU/pids limits and bounded /tmp. Envelope and resolve measurements are warm-cache; isolation compares authorized work units only. Not a fleet, power-loss, or timing-side-channel claim.",
        "environment": {
            "kernel": env("WEFT_COURT_KERNEL"),
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "court_memory": env("WEFT_COURT_MEMORY"),
            "court_cpus": env("WEFT_COURT_CPUS"),
            "court_pids": env("WEFT_COURT_PIDS"),
            "docker_client": env("WEFT_DOCKER_CLIENT"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
            "revision_weftfs": env("WEFT_REPO_WEFTFS"),
            "revision_weftcodec": env("WEFT_REPO_WEFTCODEC"),
        },
        "workload": {
            "envelopes": ENVELOPES,
            "store_objects": STORE_OBJECTS,
            "resolve_samples": RESOLVE_SAMPLES,
            "selector": "select attrs.project == \"capability-bench\"",
        },
        "envelope_verify": {
            "cold": timing_summary(&cold),
            "warm": timing_summary(&warm),
            "note": "cold = first verification of each distinct blob; warm = three subsequent passes over the same 1,000 blobs",
        },
        "signed_resolve": {
            "unsigned": timing_summary(&unsigned_ns),
            "signed": timing_summary(&signed_ns),
            "overhead_median_ns": overhead_ns,
            "overhead_percent": overhead_percent,
            "matched_count": matched,
            "work_units_unsigned": unsigned_work,
            "work_units_signed": signed_work,
            "work_units_equal": unsigned_work == signed_work,
        },
        "isolation": {
            "tenant_b_population_small": small.1,
            "tenant_b_population_large": large.1,
            "work_units_small": small.0,
            "work_units_large": large.0,
            "work_units_equal": small.0 == large.0,
            "note": "same tenant A content and envelope; tenant B adds unreadable objects",
        },
    });
    let text = serde_json::to_string_pretty(&receipt).map_err(|err| err.to_string())?;
    fs::write(out, format!("{text}\n")).map_err(|err| format!("{}: {err}", out.display()))?;
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Build a store with a fixed tenant A and `b_population` unreadable tenant B
/// objects; return `(A work units, B population)` for A's view.
fn isolation_work_units(dir: PathBuf, b_population: usize) -> Result<(u64, usize), String> {
    let mut store = Store::init(&dir, options()).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    store
        .setattr(
            root,
            SetAttr {
                mode: Some(0o755),
                ..SetAttr::default()
            },
        )
        .map_err(|err| err.to_string())?;
    let a_dir = store
        .mkdir_seeded(root, b"a", 0o700, A_UID, A_UID, &seed_byte(0xA0))
        .map_err(|err| err.to_string())?;
    let b_dir = store
        .mkdir_seeded(root, b"b", 0o700, B_UID, B_UID, &seed_byte(0xB0))
        .map_err(|err| err.to_string())?;
    for index in 0..5 {
        store
            .create_file_seeded(
                a_dir,
                format!("a-{index}.txt").as_bytes(),
                format!("tenant-a-payload-{index}").as_bytes(),
                0o600,
                A_UID,
                A_UID,
                attrs(&[("tenant", "a")]),
                format!("capability-bench-a-{index}").as_bytes(),
            )
            .map_err(|err| err.to_string())?;
    }
    for index in 0..b_population {
        store
            .create_file_seeded(
                b_dir,
                format!("b-{index}.txt").as_bytes(),
                format!("tenant-b-payload-{index}").as_bytes(),
                0o600,
                B_UID,
                B_UID,
                attrs(&[("tenant", "b")]),
                format!("capability-bench-b-{index}").as_bytes(),
            )
            .map_err(|err| err.to_string())?;
    }
    let snapshot = store.snapshot_create("s1").map_err(|err| err.to_string())?;
    let selector =
        selector::parse("select attrs.tenant == \"a\"").map_err(|err| err.to_string())?;
    let context = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        None,
    )
    .map_err(|err| err.to_string())?;
    store
        .save_context_root("a_view", &context, None, false)
        .map_err(|err| err.to_string())?;
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&seed_byte(7)).unwrap(),
        hash(0xA5),
        "tenant-a",
        vec![context.id().map_err(|err| err.to_string())?],
        Vec::new(),
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        hash(0xE1),
        FIXED_TIMESTAMP_NS,
        hash(0x11),
    )
    .map_err(|err| err.to_string())?;
    let blob = sign_envelope(&envelope, &seed_byte(7))
        .map_err(|err| err.to_string())?
        .encode()
        .map_err(|err| err.to_string())?;
    let verified =
        verify_envelope(&blob, &[], FIXED_TIMESTAMP_NS, None).map_err(|err| err.to_string())?;
    let principal = Principal::new(A_UID, A_UID, Vec::new()).with_envelope(verified);
    let resolved = store
        .resolve_context(
            "a_view",
            None,
            &principal,
            &weftcodec::Limits::default(),
            FIXED_TIMESTAMP_NS,
        )
        .map_err(|err| err.to_string())?;
    let _ = fs::remove_dir_all(&dir);
    Ok((resolved.work_units, b_population))
}
