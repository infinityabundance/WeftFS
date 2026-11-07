//! Zero-trust KV page hot-path cost.
//!
//! Measures the four paths a reader can take on a mode-12 `KV_CACHE` page
//! guarded by a canonical `weft.kv.capability.v1` descriptor and an
//! authenticated capability envelope:
//!
//! * `full`        — load the stored envelope, verify its Ed25519 signature,
//!                   build the principal, authorize, decode. What a process that
//!                   has just been handed an envelope by id pays per read.
//! * `cached`      — the same read with an already verified envelope in hand.
//!                   This is the steady-state hot path.
//! * `decode_only` — read + parse + decode with no capability work. The control
//!                   that isolates the governance cost.
//! * `denied`      — an unauthorized read, to show the fail-closed path cost.
//!
//! Two page sizes (1 KiB and 64 KiB of BF16 payload) show the split between the
//! fixed authorization cost and the per-byte decode cost.
//!
//! Run only in the OOM-protected court. The workload is synthetic and the
//! receipt says so.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::frame::Frame;
use weftcodec::kv_cache::{self, KvCacheInput, KvDtype};
use weftcodec::Limits;

use weftfs::identity::Hash32;
use weftfs::kv_capability::{KV_DENIED, KV_DESCRIPTOR_SCHEMA};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const SEED: [u8; 32] = [7u8; 32];
const DEFAULT_REPS: usize = 2_000;

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn distribution(samples_ns: &[u64]) -> Json {
    if samples_ns.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted = samples_ns.to_vec();
    sorted.sort_unstable();
    let pick = |pct: usize| -> u64 { sorted[((sorted.len() * pct) / 100).min(sorted.len() - 1)] };
    let sum: u128 = sorted.iter().map(|value| u128::from(*value)).sum();
    json!({
        "samples": sorted.len(),
        "min_us": round(sorted[0] as f64 / 1000.0),
        "p50_us": round(pick(50) as f64 / 1000.0),
        "p95_us": round(pick(95) as f64 / 1000.0),
        "p99_us": round(pick(99) as f64 / 1000.0),
        "max_us": round(sorted[sorted.len() - 1] as f64 / 1000.0),
        "mean_us": round(sum as f64 / sorted.len() as f64 / 1000.0),
        "p50_ns": pick(50),
        "p99_ns": pick(99),
    })
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn page_input(rows: u64) -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: "session-42".to_owned(),
        page_index: 0,
        page_count: 1,
        seq_start: 0,
        seq_end: rows,
        dtype: KvDtype::Bf16,
        head_count: 4,
        head_dim: 64,
    }
}

fn page_bytes(rows: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity((rows * 4 * 64 * 2) as usize);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..(rows * 4 * 64) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

fn grant(store: &mut Store, descriptor_id: Hash32) -> (Hash32, VerifiedEnvelope) {
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        TENANT,
        vec![descriptor_id],
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes([0x22; 32]),
        NOW_NS,
        Hash32::from_bytes([0x33; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &SEED).unwrap();
    let envelope_id = store.save_capability_envelope(&blob).unwrap();
    let verified = store
        .load_capability_envelope(envelope_id, &[], NOW_NS)
        .unwrap();
    (envelope_id, verified)
}

fn measured(reps: usize, mut run: impl FnMut()) -> Json {
    // One warm-up pass so the first sample is not a cold page-in.
    run();
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let started = Instant::now();
        run();
        samples.push(started.elapsed().as_nanos() as u64);
    }
    distribution(&samples)
}

fn main() {
    if let Err(err) = run() {
        eprintln!("kv_capability_bench: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out =
        PathBuf::from(value_of(&args, "--out").unwrap_or_else(|| "kv-capability.json".to_owned()));
    let work = PathBuf::from(
        value_of(&args, "--work").unwrap_or_else(|| ".docker-cache/kv-capability".to_owned()),
    );
    let reps = value_of(&args, "--reps")
        .map(|text| text.parse::<usize>())
        .transpose()
        .map_err(|_| "--reps must be an integer".to_owned())?
        .unwrap_or(DEFAULT_REPS);
    if reps < 1000 {
        return Err(format!("--reps must be at least 1000 (got {reps})"));
    }
    if work.exists() {
        fs::remove_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    }
    let limits = Limits::default();

    let mut sizes = Vec::new();
    for (label, rows) in [("1KiB", 2u64), ("64KiB", 128u64)] {
        let dir = work.join(label);
        fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let mut store = Store::init(&dir, options()).map_err(|err| err.to_string())?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let input = page_input(rows);
        let page = page_bytes(rows);
        let written = store
            .write_kv_page(root, b"page.kv", TENANT, &input, &page, 0, 0)
            .map_err(|err| err.to_string())?;
        let (envelope_id, verified) = grant(&mut store, written.descriptor_id);
        let principal = Principal::new(0, 0, Vec::new()).with_envelope(verified.clone());
        let anonymous = Principal::new(0, 0, Vec::new());

        // Exactness is a precondition for timing anything.
        let first = store
            .read_kv_page(written.object_id, &principal, NOW_NS)
            .map_err(|err| err.to_string())?;
        if first.bytes != page {
            return Err(format!("{label}: authorized read is not byte-exact"));
        }
        let denied = store
            .read_kv_page(written.object_id, &anonymous, NOW_NS)
            .expect_err("an anonymous read must fail");
        if denied.message() != KV_DENIED {
            return Err(format!("{label}: denial text `{}`", denied.message()));
        }

        // `full`: envelope load + signature verification + read, every time.
        let mut verification_samples = Vec::with_capacity(reps);
        let full = measured(reps, || {
            let started = Instant::now();
            let verified = store
                .load_capability_envelope(envelope_id, &[], NOW_NS)
                .expect("stored envelope");
            verification_samples.push(started.elapsed().as_nanos() as u64);
            let principal = Principal::new(0, 0, Vec::new()).with_envelope(verified);
            let decoded = store
                .read_kv_page(written.object_id, &principal, NOW_NS)
                .expect("authorized read");
            assert_eq!(decoded.bytes.len(), page.len());
        });

        // `cached`: the hot path, verified envelope already in hand.
        let cached = measured(reps, || {
            let decoded = store
                .read_kv_page(written.object_id, &principal, NOW_NS)
                .expect("authorized read");
            assert_eq!(decoded.bytes.len(), page.len());
        });

        // `decode_only`: no capability work at all (the control).
        let decode_only = measured(reps, || {
            let authorized = store
                .authorize_read(written.object_id, &Principal::root(), NOW_NS)
                .expect("authorize");
            let bytes = store.read_object_authorized(&authorized).expect("read");
            let frame = Frame::parse(&bytes, &limits).expect("frame");
            let decoded = kv_cache::decode_kv_page(&frame, &limits).expect("decode");
            assert_eq!(decoded.bytes.len(), page.len());
        });

        // `denied`: the fail-closed path.
        let denied_timing = measured(reps, || {
            let err = store
                .read_kv_page(written.object_id, &anonymous, NOW_NS)
                .expect_err("anonymous read must fail");
            assert_eq!(err.message(), KV_DENIED);
        });

        // A control that shows the ordinary whole-object read without a decode.
        // The object's logical content *is* the frozen mode-12 frame; the
        // decoded page is what `read_kv_page` returns.
        let object_read = measured(reps, || {
            let bytes = store.read_object(written.object_id).expect("read");
            assert_eq!(bytes.len() as u64, written.frame_bytes);
        });

        sizes.push(json!({
            "label": label,
            "page_rows": rows,
            "page_bytes": page.len(),
            "head_count": input.head_count,
            "head_dim": input.head_dim,
            "dtype": "bf16",
            "frame_bytes": written.frame_bytes,
            "descriptor_id": written.descriptor_id.to_hex(),
            "descriptor_schema": KV_DESCRIPTOR_SCHEMA,
            "envelope_id": envelope_id.to_hex(),
            "authorized_exact_bytes": true,
            "anonymous_denial_text": KV_DENIED,
            "cost": {
                "full_load_verify_authorize_decode": full,
                "envelope_load_and_verify_only": distribution(&verification_samples),
                "cached_verify_once_authorize_decode": cached,
                "authorize_decode_control": decode_only,
                "object_read_control": object_read,
                "denied_path": denied_timing,
            },
        }));
        let _ = store;
    }

    let receipt = json!({
        "schema": "weft.receipt.kv-capability.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "command": "scripts/docker-court.sh rust \"cd /work && bash bench/scripts/kv_capability_court.sh\"",
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "court_cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "court_memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "court_pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "reps_per_path": reps,
        },
        "fixture": {
            "kind": "synthetic deterministic BF16 KV page",
            "tenant": TENANT,
            "model": page_input(2).model,
            "session": "session-42",
            "grant": "Ed25519 weft.capability.envelope.v1, namespaces=[kv.tenant-a], roots=[descriptor id]",
            "store": "normative local backend, fixed clock",
            "cache_state": "warm (one warm-up read before sampling)",
        },
        "sizes": sizes,
        "label": "Single host, warm cache, synthetic page bytes, in-process store. Not a network or production claim.",
    });
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}
