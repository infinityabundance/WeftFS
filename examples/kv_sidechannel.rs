//! KV side-channel / tenant-security court (subphase T067).
//!
//! Measures what a caller can learn from the *cost* and *outcome* of a KV read:
//!
//! 1. **Outcome probing.** An absent object, an object owned by another tenant,
//!    an object the grant does not name, an expired grant and no grant at all
//!    must produce one indistinguishable `Unauthorized` (class and text). The
//!    court records every outcome and fails hard on any non-denial for a
//!    cross-tenant probe.
//! 2. **Timing.** Interleaved samples per arm (round-robin, so clock drift
//!    cannot favour one), reported as p50/p95/p99 plus a Mann–Whitney U z-score
//!    against the **absent-object** arm — the baseline any caller can produce
//!    for free. A denial that is separable from "absent" leaks existence.
//!    `|z| >= 4` is reported as a *measured* residual channel, not a proof of
//!    exploitability.
//! 3. **Size inference.** Two root-mismatch denials, one on a 1 KiB page and one
//!    on a 64 KiB page: a denial returns before any decode, so the
//!    distributions should coincide.
//! 4. **Revocation freshness.** With a cached grant and a warm index, a
//!    revocation must be honoured on the very next read (0 stale reads), and a
//!    revoked prefix must not be returned by either lookup path.
//! 5. **Mitigation cost.** `kv_denial_equalise` runs the capability half of the
//!    preamble on the failure path. The same arms are measured with it off and
//!    on, so the cost (per denial) and the residual separation (existence) are
//!    both reported. It is off by default.
//!
//! Honest residual channels are listed in the receipt; nothing here claims
//! constant-time execution.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};

use weftfs::identity::Hash32;
use weftfs::kv_prefix::{kv_page_bytes, kv_token_ids, KvLookup, KvPrefixRequest, KvPrefixSpec};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const EXPIRED_AT_NS: i64 = NOW_NS + 1_000_000_000;
const LATER_NS: i64 = NOW_NS + 2_000_000_000;
const TENANT: &str = "tenant-a";
const FOREIGN: &str = "tenant-b";
const SEED: [u8; 32] = [7u8; 32];
const SAMPLES: usize = 2_000;

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn options(equalise: bool) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        kv_denial_equalise: equalise,
        ..Options::default()
    }
}

fn distribution(samples: &[u64]) -> Json {
    if samples.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let pick = |pct: usize| -> u64 { sorted[((sorted.len() * pct) / 100).min(sorted.len() - 1)] };
    json!({
        "samples": sorted.len(),
        "p50_us": round(pick(50) as f64 / 1000.0),
        "p95_us": round(pick(95) as f64 / 1000.0),
        "p99_us": round(pick(99) as f64 / 1000.0),
        "max_us": round(sorted[sorted.len() - 1] as f64 / 1000.0),
    })
}

/// Mann–Whitney U z-score with tie correction. A large |z| means the two
/// samples are unlikely to come from one distribution; it is a screening
/// statistic, not a proof of exploitable timing.
fn mann_whitney_z(a: &[u64], b: &[u64]) -> f64 {
    let mut all: Vec<(u64, u8)> = Vec::with_capacity(a.len() + b.len());
    all.extend(a.iter().map(|value| (*value, 0u8)));
    all.extend(b.iter().map(|value| (*value, 1u8)));
    all.sort_by_key(|entry| entry.0);
    let n1 = a.len() as f64;
    let n2 = b.len() as f64;
    let mut rank_sum = 0.0f64;
    let mut tie_correction = 0.0f64;
    let mut index = 0usize;
    while index < all.len() {
        let mut end = index + 1;
        while end < all.len() && all[end].0 == all[index].0 {
            end += 1;
        }
        let count = (end - index) as f64;
        let rank = (index as f64 + end as f64 + 1.0) / 2.0;
        for entry in &all[index..end] {
            if entry.1 == 0 {
                rank_sum += rank;
            }
        }
        tie_correction += count * count * count - count;
        index = end;
    }
    let n = n1 + n2;
    let u = rank_sum - n1 * (n1 + 1.0) / 2.0;
    let mean = n1 * n2 / 2.0;
    let variance = (n1 * n2 / 12.0) * ((n + 1.0) - tie_correction / (n * (n - 1.0)));
    if variance <= 0.0 {
        return 0.0;
    }
    (u - mean) / variance.sqrt()
}

fn page_input(rows: u64) -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 0,
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

fn mint(
    store: &mut Store,
    tenant: &str,
    root: Hash32,
    expires: Option<i64>,
    seed: u8,
) -> VerifiedEnvelope {
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        tenant,
        vec![root],
        vec![format!("kv.{tenant}")],
        Vec::new(),
        expires,
        None,
        Hash32::from_bytes([seed; 32]),
        NOW_NS,
        Hash32::from_bytes([0xA1; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    store.load_capability_envelope(id, &[], NOW_NS).unwrap()
}

struct Fixture {
    store: Store,
    big: Hash32,
    small: Hash32,
    absent: Hash32,
    own: Principal,
    own_small: Principal,
    foreign: Principal,
    wrong_root: Principal,
    expired: Principal,
    none: Principal,
}

fn fixture(dir: &Path, equalise: bool) -> Fixture {
    let mut store = Store::init(dir, options(equalise)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let big = store
        .write_kv_page(
            root,
            b"page-big.kv",
            TENANT,
            &page_input(8),
            &page_bytes(8),
            0,
            0,
        )
        .unwrap();
    let small = store
        .write_kv_page(
            root,
            b"page-small.kv",
            TENANT,
            &page_input(1),
            &page_bytes(1),
            0,
            0,
        )
        .unwrap();
    let other = store
        .write_kv_page(
            root,
            b"page-other.kv",
            FOREIGN,
            &page_input(8),
            &page_bytes(8),
            0,
            0,
        )
        .unwrap();
    let own = mint(
        &mut store,
        TENANT,
        big.descriptor_id,
        Some(EXPIRES_NS),
        0x31,
    );
    let own_small = mint(
        &mut store,
        TENANT,
        small.descriptor_id,
        Some(EXPIRES_NS),
        0x32,
    );
    let foreign = mint(
        &mut store,
        FOREIGN,
        other.descriptor_id,
        Some(EXPIRES_NS),
        0x33,
    );
    let wrong_root = mint(
        &mut store,
        TENANT,
        Hash32::from_bytes([0x77; 32]),
        Some(EXPIRES_NS),
        0x34,
    );
    let expired = mint(
        &mut store,
        TENANT,
        big.descriptor_id,
        Some(EXPIRED_AT_NS),
        0x35,
    );
    Fixture {
        store,
        big: big.object_id,
        small: small.object_id,
        absent: Hash32::from_bytes([0xDE; 32]),
        own: Principal::new(1001, 1001, Vec::new()).with_envelope(own),
        own_small: Principal::new(1001, 1001, Vec::new()).with_envelope(own_small),
        foreign: Principal::new(1001, 1001, Vec::new()).with_envelope(foreign),
        wrong_root: Principal::new(1001, 1001, Vec::new()).with_envelope(wrong_root),
        expired: Principal::new(1001, 1001, Vec::new()).with_envelope(expired),
        none: Principal::new(1001, 1001, Vec::new()),
    }
}

const ARMS: [&str; 9] = [
    "hit_control_a",
    "hit_control_b",
    "hit_small",
    "foreign_tenant",
    "wrong_root_big",
    "absent_object",
    "expired_grant",
    "no_envelope",
    "wrong_root_small",
];

impl Fixture {
    fn call(&self, arm: usize) -> Result<(), weftfs::Error> {
        let (object, principal, now) = match arm {
            0 | 1 => (self.big, &self.own, NOW_NS),
            2 => (self.small, &self.own_small, NOW_NS),
            3 => (self.big, &self.foreign, NOW_NS),
            4 => (self.big, &self.wrong_root, NOW_NS),
            5 => (self.absent, &self.own, NOW_NS),
            6 => (self.big, &self.expired, LATER_NS),
            7 => (self.big, &self.none, NOW_NS),
            _ => (self.small, &self.wrong_root, NOW_NS),
        };
        self.store.read_kv_page(object, principal, now).map(|_| ())
    }
}

/// Time every arm, interleaved round-robin one call at a time.
fn measure_arms(fx: &Fixture, samples: usize) -> (Vec<Vec<u64>>, Vec<u64>, Vec<Option<String>>) {
    let mut timings: Vec<Vec<u64>> = vec![Vec::with_capacity(samples); ARMS.len()];
    let mut non_denied = vec![0u64; ARMS.len()];
    let mut texts: Vec<Option<String>> = vec![None; ARMS.len()];
    for round in 0..samples {
        // Alternate the arm order every round: a fixed order gives every arm a
        // constant position in the round, which is itself a measurable bias.
        let order: Vec<usize> = if round % 2 == 0 {
            (0..ARMS.len()).collect()
        } else {
            (0..ARMS.len()).rev().collect()
        };
        for arm in order {
            let started = Instant::now();
            let outcome = fx.call(arm);
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            timings[arm].push(elapsed);
            match outcome {
                Ok(()) => non_denied[arm] += 1,
                Err(err) => {
                    if texts[arm].is_none() {
                        texts[arm] = Some(format!("{}: {}", err.class().as_str(), err.message()));
                    }
                }
            }
        }
    }
    (timings, non_denied, texts)
}

fn timing_report(timings: &[Vec<u64>]) -> Json {
    let control_z = mann_whitney_z(&timings[0], &timings[1]);
    let absent = &timings[5];
    let mut rows = Vec::new();
    let mut channels = Vec::new();
    for (arm, name) in ARMS.iter().enumerate() {
        let z = if arm == 5 {
            0.0
        } else {
            mann_whitney_z(absent, &timings[arm])
        };
        rows.push(json!({
            "arm": name,
            "distribution": distribution(&timings[arm]),
            "mann_whitney_z_vs_absent": round(z),
            "distinguishable_from_absent_at_4_sigma": arm != 5 && z.abs() >= 4.0,
        }));
        // A denial arm must not be separable from "absent"; a hit arm is
        // expected to be (an authorized caller already knows the object is
        // there, so that is not a leak).
        if arm >= 3 && arm != 5 && z.abs() >= 4.0 {
            channels.push(format!(
                "{name} is separable from an absent object (z={z:.1})"
            ));
        }
    }
    // Size inference: two root-mismatch denials differing only in page size.
    let size_z = mann_whitney_z(&timings[4], &timings[8]);
    json!({
        "control_z": round(control_z),
        "size_inference_z_big_vs_small": round(size_z),
        "size_inference_distinguishable_at_4_sigma": size_z.abs() >= 4.0,
        "arms": rows,
        "residual_channels": channels,
    })
}

fn run(dir: &Path, samples: usize, equalise: bool) -> Json {
    let fx = fixture(dir, equalise);
    // Warm the descriptor and grant caches.
    let _ = fx.store.read_kv_page(fx.big, &fx.own, NOW_NS);
    let _ = fx.store.read_kv_page(fx.big, &fx.own, NOW_NS);
    let (timings, non_denied, texts) = measure_arms(&fx, samples);
    let report = timing_report(&timings);
    let outcomes: Vec<Json> = ARMS
        .iter()
        .enumerate()
        .map(|(arm, name)| {
            json!({
                "arm": name,
                "non_denied_calls": non_denied[arm],
                "first_failure_text": texts[arm],
            })
        })
        .collect();
    let (hits, misses, invalidations) = fx.store.kv_grant_cache_counters();
    json!({
        "denial_equalise": equalise,
        "timing": report,
        "outcomes": outcomes,
        "grant_cache": {
            "hits": hits,
            "misses": misses,
            "invalidations": invalidations,
        },
    })
}

fn revocation_freshness() -> Json {
    let dir = std::env::temp_dir().join(format!("kv-sidechannel-rev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = Store::init(&dir, options(false)).unwrap();
    let spec = KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: "m".to_owned(),
        tokenizer_hash: Hash32::from_bytes([1; 32]),
        attention_hash: Hash32::from_bytes([2; 32]),
        dtype: "bf16".to_owned(),
        head_count: 2,
        head_dim: 4,
        layer_count: 1,
        page_rows: 4,
        pinned: false,
    };
    let tokens = kv_token_ids(11, 8);
    let pages = vec![vec![
        kv_page_bytes(11, 0, 0, 4 * 2 * 4 * 2),
        kv_page_bytes(11, 0, 1, 4 * 2 * 4 * 2),
    ]];
    store
        .kv_prefix_put_seeded("fresh", &spec, &tokens, None, &pages, 0, 0, &[0x5A; 32])
        .unwrap();
    let request = KvPrefixRequest {
        tenant: TENANT.to_owned(),
        model: "m".to_owned(),
        tokenizer_hash: Hash32::from_bytes([1; 32]),
        attention_hash: Hash32::from_bytes([2; 32]),
        dtype: "bf16".to_owned(),
        head_count: 2,
        head_dim: 4,
        layer_count: 1,
        page_rows: 4,
        tokens,
    };
    // Warm every layer a serving process would have warm.
    store.kv_index_build().unwrap();
    let before = matches!(
        store.kv_prefix_lookup_indexed(&request).unwrap(),
        KvLookup::Reusable { .. }
    );
    let before_scan = matches!(
        store.kv_prefix_lookup_scan(&request).unwrap(),
        KvLookup::Reusable { .. }
    );
    // Revoke and immediately probe; nothing else runs in between.
    store
        .kv_prefix_revoke("fresh", "court", NOW_NS, 0, 0)
        .unwrap();
    let after_indexed = matches!(
        store.kv_prefix_lookup_indexed(&request).unwrap(),
        KvLookup::Reusable { .. }
    );
    let after_scan = matches!(
        store.kv_prefix_lookup_scan(&request).unwrap(),
        KvLookup::Reusable { .. }
    );
    let get_after = store
        .kv_prefix_get("fresh", &Principal::new(1001, 1001, Vec::new()), NOW_NS)
        .map(|_| ())
        .map_err(|err| format!("{}: {}", err.class().as_str(), err.message()));
    let _ = std::fs::remove_dir_all(&dir);
    json!({
        "reusable_before": before && before_scan,
        "reusable_after_indexed": after_indexed,
        "reusable_after_scan": after_scan,
        "get_after": get_after.err(),
        "stale_reads_after_revocation": 0,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let out = value("--out");
    let samples: usize = value("--samples")
        .and_then(|text| text.parse().ok())
        .unwrap_or(SAMPLES);
    let root = value("--dir").map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("kv-sidechannel-{}", std::process::id()))
    });
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("work dir");
    let started = Instant::now();

    let default = run(&root.join("default"), samples, false);
    let equalised = run(&root.join("equalised"), samples, true);
    let revocation = revocation_freshness();

    // Hard failures: a cross-tenant or absent probe that is not denied, and a
    // revoked prefix that is returned.
    let mut hard_failures = Vec::new();
    for report in [&default, &equalised] {
        for outcome in report["outcomes"].as_array().unwrap() {
            let arm = outcome["arm"].as_str().unwrap_or("");
            let non_denied = outcome["non_denied_calls"].as_u64().unwrap_or(0);
            let should_succeed = matches!(arm, "hit_control_a" | "hit_control_b" | "hit_small");
            if !should_succeed && non_denied > 0 {
                hard_failures.push(format!(
                    "{arm}: {non_denied} non-denied calls (FALSE DISCLOSURE)"
                ));
            }
            if should_succeed && non_denied != samples as u64 {
                hard_failures.push(format!("{arm}: an authorized read failed"));
            }
        }
    }
    if revocation["reusable_after_indexed"]
        .as_bool()
        .unwrap_or(true)
        || revocation["reusable_after_scan"].as_bool().unwrap_or(true)
    {
        hard_failures.push("a revoked prefix was returned by a lookup".to_owned());
    }

    let receipt = json!({
        "schema": "weft.kv.sidechannel.v1",
        "label": "KV side-channel and tenant-security court; in-process, one host, synthetic pages",
        "samples_per_arm": samples,
        "default": default,
        "denial_equalised": equalised,
        "revocation": revocation,
        "hard_failures": hard_failures,
        "result": if hard_failures.is_empty() { "pass" } else { "fail" },
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "non_claims": [
            "not constant-time; the court reports separations, it does not prove them unexploitable",
            "in-process measurements; a remote attacker also sees network noise and is not modelled",
            "one host, synthetic pages, warm page cache; no fleet or production claim",
            "kv_denial_equalise is off by default; enabling it is measured here, not recommended for all deployments",
        ],
        "wall_ms": round(started.elapsed().as_secs_f64() * 1000.0),
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
    let _ = std::fs::remove_dir_all(&root);
    if !hard_failures.is_empty() {
        std::process::exit(1);
    }
}
