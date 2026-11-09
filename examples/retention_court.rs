//! SWA-style retention court (subphase R6).
//!
//! One invocation builds two synthetic populations of mode-19 KV pages — 1,000
//! `volatile` pages with a sliding-window reuse distance measured in minutes,
//! and 300 `ephemeral` pages reused across a simulated six-hour session — and
//! runs three policies over the same capacity budget:
//!
//! (a) fixed TTL + LRU, the production pattern, which ignores reachability;
//! (b) reachability-only GC, which ignores class and therefore discards the
//!     short-lived population as soon as it is unreferenced;
//! (c) class-aware GC (this subphase), which computes reachability first and
//!     then retains/orders the unreachable set by class and declared lifetime.
//!
//! The trace is **synthetic and deterministic**. It models reuse distance; it
//! is not a measurement of a serving workload. The receipt says so.
//!
//! `capacity` is measured in page-frame bytes (the cache's unit), not in
//! physical store bytes; the receipt reports the physical tree separately.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftcodec::frame::Frame;
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{
    encode_kv_prov_page, kv_prov_metadata, KvAttention, KvLayout, KvPositional, KvProvenanceInput,
};
use weftcodec::limits::Limits;

use weftfs::identity::Hash32;
use weftfs::retention::{RetentionClass, RetentionRequest};
use weftfs::store::{Options, Store};

const VOLATILE_PAGES: usize = 1_000;
const EPHEMERAL_PAGES: usize = 300;
const TICKS: usize = 360; // 360 minutes = six simulated hours
const TICK_NS: i64 = 60_000_000_000;
const BASE_NS: i64 = 1_800_000_000_000_000_000;
const VOLATILE_LIFETIME_MS: u64 = 300_000;
const EPHEMERAL_LIFETIME_MS: u64 = 6 * 3_600_000;
const FIXED_TTL_MS: u64 = 300_000;
const BUDGET_BYTES: u64 = 3_000_000;
const PROTECTED_PAGES: usize = 20;
const PAGE_ROWS: u64 = 4;
const HEAD_COUNT: u32 = 4;
const HEAD_DIM: u32 = 64;

fn main() {
    if let Err(err) = run() {
        eprintln!("retention_court: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(arg(&args, "--out").ok_or("--out FILE is required")?);
    let work = PathBuf::from(
        arg(&args, "--work").unwrap_or_else(|| ".docker-cache/retention-court".to_owned()),
    );
    let keep = flag(&args, "--keep");
    fs::create_dir_all(&work).map_err(|err| format!("cannot create {}: {err}", work.display()))?;

    let started = Instant::now();
    let mut runs: Vec<Json> = Vec::new();
    let mut all_ok = true;

    for rate in [Rate::Fast, Rate::Slow] {
        for policy in [
            Policy::FixedTtl,
            Policy::ReachabilityOnly,
            Policy::ClassAware,
        ] {
            let result = run_policy(&work, rate, policy, ClassMode::Correct)?;
            let ok = result["checks"]["exactness"].as_bool().unwrap_or(false)
                && result["checks"]["verify_clean"].as_bool().unwrap_or(false)
                && result["checks"]["protected_readable"]
                    .as_bool()
                    .unwrap_or(false)
                && (policy == Policy::FixedTtl
                    || result["checks"]["unsafe_zero"].as_bool().unwrap_or(false));
            if !ok {
                all_ok = false;
            }
            runs.push(result);
        }
    }

    let baseline = runs
        .iter()
        .find(|row| row["policy"] == "fixed_ttl_lru" && row["rate"] == "fast")
        .cloned();
    let correct_fast = runs
        .iter()
        .find(|row| row["policy"] == "class_aware" && row["rate"] == "fast")
        .cloned();
    let class_aware_wins = match (&baseline, &correct_fast) {
        (Some(base), Some(correct)) => {
            let base_rate = base["safe_hit_rate"].as_f64().unwrap_or(0.0);
            let class_rate = correct["safe_hit_rate"].as_f64().unwrap_or(0.0);
            class_rate > base_rate
        }
        _ => false,
    };
    if !class_aware_wins {
        all_ok = false;
    }

    let mut misclassification: Vec<Json> = Vec::new();
    for mode in [
        ClassMode::VolatileAsEphemeral,
        ClassMode::EphemeralAsVolatile,
    ] {
        let result = run_policy(&work, Rate::Fast, Policy::ClassAware, mode)?;
        if !result["checks"]["exactness"].as_bool().unwrap_or(false)
            || !result["checks"]["unsafe_zero"].as_bool().unwrap_or(false)
        {
            all_ok = false;
        }
        misclassification.push(result);
    }

    let correct_row = correct_fast.as_ref();
    let volatile_mis = misclassification
        .iter()
        .find(|row| row["class_mode"] == "volatile_declared_ephemeral");
    let ephemeral_mis = misclassification
        .iter()
        .find(|row| row["class_mode"] == "ephemeral_declared_volatile");
    let number = |row: Option<&Json>, key: &str| -> f64 {
        row.and_then(|row| row[key].as_f64()).unwrap_or(0.0)
    };
    let u64_of = |row: Option<&Json>, key: &str| -> u64 {
        row.and_then(|row| row[key].as_u64()).unwrap_or(0)
    };
    let misclassification_cost = json!({
        "correct": {
            "retained_bytes": u64_of(correct_row, "retained_bytes"),
            "peak_retained_bytes": u64_of(correct_row, "peak_retained_bytes"),
            "reclaimed_bytes": u64_of(correct_row, "reclaimed_bytes"),
            "safe_hit_rate": number(correct_row, "safe_hit_rate"),
        },
        "volatile_declared_ephemeral": {
            "retained_bytes": u64_of(volatile_mis, "retained_bytes"),
            "extra_retained_bytes": u64_of(volatile_mis, "retained_bytes")
                .saturating_sub(u64_of(correct_row, "retained_bytes")),
            "safe_hit_rate": number(volatile_mis, "safe_hit_rate"),
            "safe_hit_rate_delta": number(volatile_mis, "safe_hit_rate")
                - number(correct_row, "safe_hit_rate"),
            "reclaimed_bytes": u64_of(volatile_mis, "reclaimed_bytes"),
            "unsafe_reclaims": u64_of(volatile_mis, "unsafe_reclaims"),
        },
        "ephemeral_declared_volatile": {
            "retained_bytes": u64_of(ephemeral_mis, "retained_bytes"),
            "safe_hit_rate": number(ephemeral_mis, "safe_hit_rate"),
            "safe_hit_rate_delta": number(ephemeral_mis, "safe_hit_rate")
                - number(correct_row, "safe_hit_rate"),
            "extra_misses": u64_of(ephemeral_mis, "misses")
                .saturating_sub(u64_of(correct_row, "misses")),
            "early_reclaims": u64_of(ephemeral_mis, "early_reclaims"),
            "unsafe_reclaims": u64_of(ephemeral_mis, "unsafe_reclaims"),
            "unsafe_is_zero": u64_of(ephemeral_mis, "unsafe_reclaims") == 0,
        },
        "note": "declaring a volatile page ephemeral buys retention it does not need (extra retained bytes); declaring an ephemeral page volatile reclaims it early but stays safe because reachability still governs",
    });

    let receipt = json!({
        "schema": "weft.retention-court.v1",
        "generated_at_unix_ns": now_ns(),
        "trace": {
            "synthetic": true,
            "description": "deterministic sliding-window (volatile) + multi-hour session (ephemeral) reuse model; not a serving-workload measurement",
            "volatile_pages": VOLATILE_PAGES,
            "ephemeral_pages": EPHEMERAL_PAGES,
            "ticks": TICKS,
            "tick_seconds": TICK_NS / 1_000_000_000,
            "simulated_seconds": (TICKS as i64) * (TICK_NS / 1_000_000_000),
            "rates": {
                "fast_requests_per_tick": Rate::Fast.requests_per_tick(),
                "slow_requests_per_tick": Rate::Slow.requests_per_tick(),
            },
            "page_frame_bytes": page_frame_len(),
            "volatile_lifetime_ms": VOLATILE_LIFETIME_MS,
            "ephemeral_lifetime_ms": EPHEMERAL_LIFETIME_MS,
            "fixed_ttl_ms": FIXED_TTL_MS,
            "capacity_bytes": BUDGET_BYTES,
            "capacity_unit": "page-frame bytes of index entries",
            "protected_pages": PROTECTED_PAGES,
        },
        "policies": runs,
        "misclassification": misclassification,
        "misclassification_cost": misclassification_cost,
        "conclusion": {
            "class_aware_wins_on_this_trace": class_aware_wins,
            "class_aware_safe_hit_rate": correct_fast.as_ref().map(|row| row["safe_hit_rate"].clone()),
            "fixed_ttl_safe_hit_rate": baseline.as_ref().map(|row| row["safe_hit_rate"].clone()),
            "loss": if class_aware_wins {
                Json::Null
            } else {
                json!("the synthetic trace's reuse distance is too short for any policy to matter; the measured numbers are published, not hidden")
            },
        },
        "environment": environment(),
        "wall_ms": started.elapsed().as_millis() as u64,
        "checks": {
            "all_exactness": all_ok,
            "note": "a policy that keeps reachability as a precondition must report unsafe_reclaims == 0; every hit re-hashes the exact frame",
        },
    });

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    println!("retention_court: wrote {}", out.display());
    if !keep {
        let _ = fs::remove_dir_all(&work);
    }
    if all_ok {
        Ok(())
    } else {
        Err("one or more court checks failed".to_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rate {
    Fast,
    Slow,
}

impl Rate {
    fn label(self) -> &'static str {
        match self {
            Rate::Fast => "fast",
            Rate::Slow => "slow",
        }
    }

    fn requests_per_tick(self) -> usize {
        match self {
            Rate::Fast => 10,
            Rate::Slow => 2,
        }
    }

    fn volatile_per_tick(self) -> usize {
        match self {
            Rate::Fast => 7,
            Rate::Slow => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    FixedTtl,
    ReachabilityOnly,
    ClassAware,
}

impl Policy {
    fn label(self) -> &'static str {
        match self {
            Policy::FixedTtl => "fixed_ttl_lru",
            Policy::ReachabilityOnly => "reachability_only",
            Policy::ClassAware => "class_aware",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassMode {
    Correct,
    VolatileAsEphemeral,
    EphemeralAsVolatile,
}

impl ClassMode {
    fn label(self) -> &'static str {
        match self {
            ClassMode::Correct => "correct",
            ClassMode::VolatileAsEphemeral => "volatile_declared_ephemeral",
            ClassMode::EphemeralAsVolatile => "ephemeral_declared_volatile",
        }
    }
}

struct PageSpec {
    index: usize,
    ephemeral: bool,
    descriptor_root: Hash32,
    bytes: Vec<u8>,
    frame: Vec<u8>,
    input: KvCacheInput,
    provenance: KvProvenanceInput,
}

struct CacheEntry {
    descriptor_root: Hash32,
    artifact: Hash32,
    frame: Vec<u8>,
    size: u64,
    last_tick: usize,
    ephemeral: bool,
    lifetime_ms: u64,
    issued_ns: i64,
    object_name: Option<String>,
}

struct RunOutcome {
    requests: u64,
    hits: u64,
    misses: u64,
    retained_final: u64,
    peak_retained: u64,
    reclaimed_bytes: u64,
    unsafe_reclaims: u64,
    unsafe_bytes: u64,
    early_reclaims: u64,
    over_budget_peak: u64,
    mismatches: u64,
    protected_unreadable: u64,
    verify_problems: usize,
    writes: u64,
    physical_bytes: u64,
    wall_ms: u64,
    peak_rss_kib: u64,
}

fn run_policy(work: &Path, rate: Rate, policy: Policy, mode: ClassMode) -> Result<Json, String> {
    let started = Instant::now();
    let dir = work.join(format!(
        "{}-{}-{}",
        policy.label(),
        rate.label(),
        mode.label()
    ));
    let root = dir.join("store");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let mut store = Store::init(
        &root,
        Options {
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(BASE_NS),
            ..Options::default()
        },
    )
    .map_err(|err| format!("cannot init store: {err}"))?;
    let pages_dir = store
        .mkdir(
            store.resolve_path("/").map_err(|err| err.to_string())?,
            b"pages",
            0o755,
            0,
            0,
        )
        .map_err(|err| err.to_string())?;

    let specs: Vec<PageSpec> = (0..(VOLATILE_PAGES + EPHEMERAL_PAGES))
        .map(make_page)
        .collect();
    let page_size = page_frame_len();

    let mut cache: BTreeMap<usize, CacheEntry> = BTreeMap::new();
    let mut serial: u64 = 0;
    let mut retained: u64 = 0;
    let mut peak_retained: u64;
    let mut reclaimed: u64 = 0;
    let mut unsafe_reclaims = 0u64;
    let mut unsafe_bytes = 0u64;
    let mut early_reclaims = 0u64;
    let mut over_budget_peak = 0u64;
    let mut mismatches = 0u64;
    let mut writes = 0u64;

    // The protected population is written first and captured by a snapshot.
    // Its artifacts stay reachable from that committed root for the whole run.
    // Writes are batched into groups so the run's commit count stays bounded.
    let mut protected = BTreeSet::new();
    store
        .group(|s| {
            for spec in &specs[..PROTECTED_PAGES] {
                seed_one(
                    s,
                    pages_dir,
                    spec,
                    mode,
                    false,
                    &mut serial,
                    &mut cache,
                    &mut retained,
                    page_size,
                )
                .map_err(weftfs::Error::io)?;
                protected.insert(spec.descriptor_root);
                writes += 1;
            }
            Ok(())
        })
        .map_err(|err| format!("seed protected: {err}"))?;
    store
        .snapshot_create("protected")
        .map_err(|err| format!("snapshot: {err}"))?;
    if policy != Policy::FixedTtl {
        store
            .group(|s| {
                for spec in &specs[..PROTECTED_PAGES] {
                    if let Some(entry) = cache.get_mut(&spec.index) {
                        if let Some(name) = entry.object_name.take() {
                            release_object(s, pages_dir, &name).map_err(weftfs::Error::io)?;
                        }
                    }
                }
                Ok(())
            })
            .map_err(|err| format!("release protected: {err}"))?;
    }

    for chunk in specs[PROTECTED_PAGES..].chunks(200) {
        store
            .group(|s| {
                for spec in chunk {
                    seed_one(
                        s,
                        pages_dir,
                        spec,
                        mode,
                        policy != Policy::FixedTtl,
                        &mut serial,
                        &mut cache,
                        &mut retained,
                        page_size,
                    )
                    .map_err(weftfs::Error::io)?;
                    writes += 1;
                }
                Ok(())
            })
            .map_err(|err| format!("seed pages: {err}"))?;
    }
    peak_retained = retained;

    let mut ephemeral_cursor = 0usize;
    let mut volatile_cursor = 0usize;
    let mut requests = 0u64;
    let mut hits = 0u64;
    let mut misses = 0u64;

    for tick in 0..TICKS {
        let now = BASE_NS + (tick as i64) * TICK_NS;
        let volatile_hot = hot_set(tick, rate);
        store
            .group(|s| {
                for request in 0..rate.requests_per_tick() {
                    let ephemeral = request >= rate.volatile_per_tick();
                    let page_index = if ephemeral {
                        let index = VOLATILE_PAGES + (ephemeral_cursor % EPHEMERAL_PAGES);
                        ephemeral_cursor += 1;
                        index
                    } else {
                        let index = volatile_hot[volatile_cursor % volatile_hot.len()];
                        volatile_cursor += 1;
                        index
                    };
                    let spec = &specs[page_index];
                    requests += 1;
                    let hit = match cache.get(&page_index) {
                        Some(entry) => {
                            match s.read_content(entry.artifact, 0, entry.frame.len() as u64) {
                                Ok(bytes) if bytes == entry.frame => true,
                                Ok(_) => {
                                    mismatches += 1;
                                    false
                                }
                                Err(_) => false,
                            }
                        }
                        None => false,
                    };
                    if hit {
                        hits += 1;
                        if let Some(entry) = cache.get_mut(&page_index) {
                            entry.last_tick = tick;
                        }
                        continue;
                    }

                    misses += 1;
                    let (descriptor_root, artifact, object_name) =
                        write_page(s, pages_dir, spec, &mut serial).map_err(weftfs::Error::io)?;
                    writes += 1;
                    if let Some(stale) = cache.remove(&page_index) {
                        retained = retained.saturating_sub(stale.size);
                        if policy != Policy::FixedTtl {
                            if let Some(name) = stale.object_name {
                                release_object(s, pages_dir, &name).map_err(weftfs::Error::io)?;
                            }
                        }
                    }
                    let class = class_for(mode, spec.ephemeral);
                    let lifetime = lifetime_for(mode, spec.ephemeral);
                    bind_page(s, spec, descriptor_root, class, lifetime, now, tick)
                        .map_err(weftfs::Error::io)?;
                    retained += page_size;
                    cache.insert(
                        page_index,
                        CacheEntry {
                            descriptor_root,
                            artifact,
                            frame: spec.frame.clone(),
                            size: page_size,
                            last_tick: tick,
                            ephemeral: spec.ephemeral,
                            lifetime_ms: lifetime,
                            issued_ns: now,
                            object_name: Some(object_name),
                        },
                    );
                    if policy != Policy::FixedTtl {
                        if let Some(entry) = cache.get_mut(&page_index) {
                            if let Some(name) = entry.object_name.take() {
                                release_object(s, pages_dir, &name).map_err(weftfs::Error::io)?;
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(|err| format!("tick {tick}: {err}"))?;
        peak_retained = peak_retained.max(retained);

        match policy {
            Policy::FixedTtl => {
                // (a) A 300 s TTL plus LRU. It never consults the store's
                // reachability roots, so it can evict a still-rooted artifact.
                let ttl_ns = FIXED_TTL_MS as i64 * 1_000_000;
                let mut selected: Vec<usize> = cache
                    .iter()
                    .filter(|(_, entry)| now - entry.issued_ns >= ttl_ns)
                    .map(|(index, _)| *index)
                    .collect();
                selected.sort_by_key(|index| cache[index].last_tick);
                if retained > BUDGET_BYTES {
                    let mut all: Vec<usize> = cache.keys().copied().collect();
                    all.sort_by_key(|index| cache[index].last_tick);
                    for index in all {
                        if retained <= BUDGET_BYTES {
                            break;
                        }
                        if !selected.contains(&index) {
                            selected.push(index);
                        }
                    }
                }
                for index in selected {
                    let expired = now - cache[&index].issued_ns >= ttl_ns;
                    if retained <= BUDGET_BYTES && !expired {
                        continue;
                    }
                    if let Some(entry) = cache.remove(&index) {
                        if protected.contains(&entry.descriptor_root) {
                            unsafe_reclaims += 1;
                            unsafe_bytes += entry.size;
                        }
                        retained = retained.saturating_sub(entry.size);
                        reclaimed += entry.size;
                        if let Some(name) = entry.object_name {
                            release_object(&mut store, pages_dir, &name)?;
                        }
                    }
                }
                store.gc(true).map_err(|err| format!("gc: {err}"))?;
            }
            Policy::ReachabilityOnly => {
                // (b) Ignore class and lifetime; reclaim everything unreachable.
                store.gc(true).map_err(|err| format!("gc: {err}"))?;
                let stale: Vec<usize> = cache
                    .iter()
                    .filter(|(_, entry)| store.read_content(entry.artifact, 0, 1).is_err())
                    .map(|(index, _)| *index)
                    .collect();
                for index in stale {
                    if let Some(entry) = cache.remove(&index) {
                        retained = retained.saturating_sub(entry.size);
                        reclaimed += entry.size;
                    }
                }
            }
            Policy::ClassAware => {
                // (c) Reachability first, then class ordering; a live binding
                // retains its unreachable bytes for the declared window.
                if retained > BUDGET_BYTES {
                    let excess = retained - BUDGET_BYTES;
                    store
                        .retention_gc(true, now, Some(excess))
                        .map_err(|err| format!("retention gc: {err}"))?;
                    let stale: Vec<usize> = cache
                        .iter()
                        .filter(|(_, entry)| {
                            entry.issued_ns + entry.lifetime_ms as i64 * 1_000_000 <= now
                                && store.read_content(entry.artifact, 0, 1).is_err()
                        })
                        .map(|(index, _)| *index)
                        .collect();
                    for index in stale {
                        if let Some(entry) = cache.remove(&index) {
                            retained = retained.saturating_sub(entry.size);
                            reclaimed += entry.size;
                            if entry.ephemeral && entry.lifetime_ms < EPHEMERAL_LIFETIME_MS {
                                early_reclaims += 1;
                            }
                        }
                    }
                }
                over_budget_peak = over_budget_peak.max(retained.saturating_sub(BUDGET_BYTES));
            }
        }
    }

    let mut protected_unreadable = 0u64;
    for descriptor_root in &protected {
        let readable = store
            .retention_pointer(*descriptor_root)
            .ok()
            .flatten()
            .map(|pointer| store.read_content(pointer.artifact, 0, 1).is_ok())
            .unwrap_or(false);
        if !readable {
            protected_unreadable += 1;
        }
    }
    let verify = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let verify_problems = verify.problems.len();
    let physical_bytes = tree_bytes(&root);
    drop(store);

    let outcome = RunOutcome {
        requests,
        hits,
        misses,
        retained_final: retained,
        peak_retained,
        reclaimed_bytes: reclaimed,
        unsafe_reclaims,
        unsafe_bytes,
        early_reclaims,
        over_budget_peak,
        mismatches,
        protected_unreadable,
        verify_problems,
        writes,
        physical_bytes,
        wall_ms: started.elapsed().as_millis() as u64,
        peak_rss_kib: peak_rss_kib(),
    };
    let safe_hit_rate = if requests == 0 {
        0.0
    } else {
        outcome.hits as f64 / outcome.requests as f64
    };
    let _ = fs::remove_dir_all(&dir);
    Ok(json!({
        "policy": policy.label(),
        "rate": rate.label(),
        "class_mode": mode.label(),
        "requests": outcome.requests,
        "hits": outcome.hits,
        "misses": outcome.misses,
        "safe_hit_rate": safe_hit_rate,
        "retained_bytes": outcome.retained_final,
        "peak_retained_bytes": outcome.peak_retained,
        "reclaimed_bytes": outcome.reclaimed_bytes,
        "unsafe_reclaims": outcome.unsafe_reclaims,
        "unsafe_bytes": outcome.unsafe_bytes,
        "early_reclaims": outcome.early_reclaims,
        "over_budget_peak_bytes": outcome.over_budget_peak,
        "writes": outcome.writes,
        "physical_bytes": outcome.physical_bytes,
        "wall_ms": outcome.wall_ms,
        "peak_rss_kib": outcome.peak_rss_kib,
        "checks": {
            "exactness": outcome.mismatches == 0,
            "verify_clean": outcome.verify_problems == 0,
            "protected_readable": outcome.protected_unreadable == 0,
            "unsafe_zero": outcome.unsafe_reclaims == 0,
            "note": "unsafe_zero is required only for policies that keep reachability as a precondition",
        },
        "verify_problems": outcome.verify_problems,
    }))
}

fn class_for(mode: ClassMode, ephemeral: bool) -> RetentionClass {
    match (mode, ephemeral) {
        (ClassMode::VolatileAsEphemeral, false) => RetentionClass::Ephemeral,
        (ClassMode::EphemeralAsVolatile, true) => RetentionClass::Volatile,
        (_, true) => RetentionClass::Ephemeral,
        (_, false) => RetentionClass::Volatile,
    }
}

fn lifetime_for(mode: ClassMode, ephemeral: bool) -> u64 {
    match class_for(mode, ephemeral) {
        RetentionClass::Ephemeral => EPHEMERAL_LIFETIME_MS,
        _ => VOLATILE_LIFETIME_MS,
    }
}

fn hot_set(tick: usize, rate: Rate) -> Vec<usize> {
    let per_tick = rate.volatile_per_tick().max(1);
    let hot_size = (per_tick * 4).max(5);
    let base = (tick * per_tick) % VOLATILE_PAGES;
    (0..hot_size)
        .map(|offset| (base + offset) % VOLATILE_PAGES)
        .collect()
}

fn page_input(index: usize) -> KvCacheInput {
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: "retention-court".to_owned(),
        page_index: index as u64,
        page_count: (VOLATILE_PAGES + EPHEMERAL_PAGES) as u64,
        seq_start: index as u64 * PAGE_ROWS,
        seq_end: index as u64 * PAGE_ROWS + PAGE_ROWS,
        dtype: KvDtype::Bf16,
        head_count: HEAD_COUNT,
        head_dim: HEAD_DIM,
    }
}

fn page_attention() -> KvAttention {
    KvAttention {
        head_count: HEAD_COUNT,
        head_dim: HEAD_DIM,
        sliding_window: 0,
        rope_theta_bits: 10_000.0f64.to_bits(),
        rope_scale_bits: 1.0f64.to_bits(),
    }
}

fn page_layout() -> KvLayout {
    KvLayout {
        dtype: KvDtype::Bf16,
        page_rows: PAGE_ROWS,
        layer_count: 24,
    }
}

fn page_bytes() -> Vec<u8> {
    let mut out =
        Vec::with_capacity((PAGE_ROWS * u64::from(HEAD_COUNT) * u64::from(HEAD_DIM) * 2) as usize);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15 | 1;
    for _ in 0..(PAGE_ROWS * u64::from(HEAD_COUNT) * u64::from(HEAD_DIM)) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

fn provenance(index: usize) -> KvProvenanceInput {
    KvProvenanceInput::new(
        Hash32::from_bytes([0x11; 32]),
        "qwen-bpe",
        151_936,
        page_attention(),
        KvPositional::Rope,
        page_layout(),
        Hash32::from_bytes([(index & 0xff) as u8; 32]),
    )
}

fn make_page(index: usize) -> PageSpec {
    let input = page_input(index);
    let bytes = page_bytes();
    let provenance = provenance(index);
    let frame = encode_kv_prov_page(&input, &bytes, &provenance, &Limits::default())
        .expect("deterministic frame");
    let parsed = Frame::parse(&frame, &Limits::default()).expect("parse frame");
    let descriptor = kv_prov_metadata(&parsed, &Limits::default()).expect("metadata");
    let descriptor_root = descriptor
        .root(&Limits::default())
        .expect("descriptor root");
    PageSpec {
        index,
        ephemeral: index >= VOLATILE_PAGES,
        descriptor_root,
        bytes,
        frame,
        input,
        provenance,
    }
}

fn page_frame_len() -> u64 {
    page_frame_len_for(0)
}

fn page_frame_len_for(index: usize) -> u64 {
    let spec = make_page(index);
    spec.frame.len() as u64
}

fn seed_one(
    store: &mut Store,
    pages_dir: Hash32,
    spec: &PageSpec,
    mode: ClassMode,
    release: bool,
    serial: &mut u64,
    cache: &mut BTreeMap<usize, CacheEntry>,
    retained: &mut u64,
    page_size: u64,
) -> Result<(), String> {
    let (descriptor_root, artifact, object_name) = write_page(store, pages_dir, spec, serial)?;
    let class = class_for(mode, spec.ephemeral);
    let lifetime = lifetime_for(mode, spec.ephemeral);
    bind_page(store, spec, descriptor_root, class, lifetime, BASE_NS, 0)?;
    *retained += page_size;
    let object_name = if release {
        release_object(store, pages_dir, &object_name)?;
        None
    } else {
        Some(object_name)
    };
    cache.insert(
        spec.index,
        CacheEntry {
            descriptor_root,
            artifact,
            frame: spec.frame.clone(),
            size: page_size,
            last_tick: 0,
            ephemeral: spec.ephemeral,
            lifetime_ms: lifetime,
            issued_ns: BASE_NS,
            object_name,
        },
    );
    Ok(())
}

fn write_page(
    store: &mut Store,
    pages_dir: Hash32,
    spec: &PageSpec,
    serial: &mut u64,
) -> Result<(Hash32, Hash32, String), String> {
    let name = format!("{}-{:08}", spec.index, serial);
    *serial += 1;
    let written = store
        .write_kv_prov_page(
            pages_dir,
            name.as_bytes(),
            "tenant-court",
            &spec.input,
            &spec.bytes,
            &spec.provenance,
            0,
            0,
        )
        .map_err(|err| format!("write page {}: {err}", spec.index))?;
    if written.provenance_root != spec.descriptor_root {
        return Err(format!(
            "page {} descriptor root changed between encode and store",
            spec.index
        ));
    }
    let record = store
        .object_meta(written.object_id)
        .map_err(|err| format!("page {} metadata: {err}", spec.index))?;
    Ok((written.provenance_root, record.content, name))
}

fn bind_page(
    store: &mut Store,
    spec: &PageSpec,
    descriptor_root: Hash32,
    class: RetentionClass,
    lifetime_ms: u64,
    issued_ns: i64,
    tick: usize,
) -> Result<(), String> {
    let mut request = RetentionRequest::new(descriptor_root, class, lifetime_ms, "retention-court");
    request.policy_epoch = tick as u64;
    request.issued_at_ns = Some(issued_ns);
    store
        .set_retention(request)
        .map_err(|err| format!("bind page {}: {err}", spec.index))?;
    Ok(())
}

fn release_object(store: &mut Store, pages_dir: Hash32, name: &str) -> Result<(), String> {
    match store.unlink(pages_dir, name.as_bytes()) {
        Ok(()) => Ok(()),
        Err(err) if err.class() == weftfs::Class::NotFound => Ok(()),
        Err(err) => Err(format!("release `{name}`: {err}")),
    }
}

fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    for sub in ["objects", "manifests"] {
        let path = root.join(sub);
        if let Ok(entries) = fs::read_dir(&path) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        total = total.saturating_add(meta.len());
                    } else if meta.is_dir() {
                        if let Ok(inner) = fs::read_dir(entry.path()) {
                            for item in inner.flatten() {
                                if let Ok(meta) = item.metadata() {
                                    total = total.saturating_add(meta.len());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    total
}

fn peak_rss_kib() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let value = rest.trim().trim_end_matches(" kB").trim();
            return value.parse().unwrap_or(0);
        }
    }
    0
}

fn environment() -> Json {
    let mut map = serde_json::Map::new();
    for (key, value) in std::env::vars() {
        if key.starts_with("WEFT_COURT_") || key.starts_with("WEFT_REPO_") {
            map.insert(key, Json::String(value));
        }
    }
    Json::Object(map)
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}
