//! Scale phase: key-sufficiency certificate construction, EKBLOOM table
//! construction and lookup, and a conventional index, at 1k / 100k / 1M keys.
//!
//! The quadratic `first_conflict` is measured only up to the size that completes
//! inside the phase's time guard; the bounded construction is measured at every
//! declared size.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use ekbloom::{
    create_auto_within_budget, write_body, ManifestationBudget, ManifestationProfile, OpenObject,
    ReadRequest,
};
use serde_json::{json, Value as Json};

const HEADER: usize = 4096;
const SLOT: usize = 64;
const KEY: usize = 32;

fn xs(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

fn pairs(n: usize, seed: u64) -> Vec<(u64, u8)> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let key = xs(&mut state) & 0x00ff_ffff_ffff_ffff;
        out.push((key, (key % 3) as u8));
    }
    out
}

/// The bounded construction: one pass, a hash map, first-seen order preserved.
pub fn first_conflict_bounded(pairs: &[(u64, u8)]) -> Option<(u64, u8, u8, usize)> {
    let mut seen: HashMap<u64, (u8, usize)> = HashMap::with_capacity(pairs.len());
    for (key, out) in pairs {
        match seen.get_mut(key) {
            Some((first, count)) => {
                if *first != *out {
                    return Some((*key, *first, *out, *count + 1));
                }
                *count += 1;
            }
            None => {
                seen.insert(*key, (*out, 1));
            }
        }
    }
    None
}

fn percentile(values: &mut [u64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let idx = ((values.len() as f64 - 1.0) * p).round() as usize;
    values[idx.min(values.len() - 1)] as f64
}

fn source_len(n: usize) -> usize {
    let slots = ((n * 2).max(8)).next_power_of_two();
    HEADER + slots * SLOT + n * KEY
}

fn build_source(n: usize, seed: u64) -> Vec<u8> {
    let slots = ((n * 2).max(8)).next_power_of_two();
    let key_area = HEADER + slots * SLOT;
    let mut source = vec![0u8; key_area + n * KEY];
    source[0..8].copy_from_slice(b"WKS1\0\0\0\0");
    source[8..12].copy_from_slice(&1u32.to_le_bytes());
    source[12..16].copy_from_slice(&(slots as u32).to_le_bytes());
    source[16..20].copy_from_slice(&(SLOT as u32).to_le_bytes());
    source[20..24].copy_from_slice(&(KEY as u32).to_le_bytes());
    let mut state = seed | 1;
    for ordinal in 0..n {
        let key = xs(&mut state);
        source[key_area + ordinal * KEY..key_area + (ordinal + 1) * KEY]
            .copy_from_slice(&key.to_le_bytes());
        let mut idx = (key % slots as u64) as usize;
        for _ in 0..64 {
            let base = HEADER + idx * SLOT;
            if source[base + 24] == 0 {
                source[base..base + 8].copy_from_slice(&key.to_le_bytes());
                source[base + 24] = 1;
                source[base + 25] = (key % 3) as u8;
                break;
            }
            idx = (idx + 1) % slots;
        }
    }
    source
}

fn measure_size(n: usize, work: &Path) -> Result<Json, String> {
    let mut row = serde_json::Map::new();
    row.insert("keys".into(), json!(n));
    // Bounded key-sufficiency certificate.
    let data = pairs(n, 0x5eed_0000_0000_0000 ^ n as u64);
    let start = Instant::now();
    let conflict = first_conflict_bounded(&data);
    row.insert(
        "bounded_certificate_ms".into(),
        json!(start.elapsed().as_secs_f64() * 1e3),
    );
    row.insert("bounded_conflict".into(), json!(conflict.is_some()));
    drop(data);

    // EKBLOOM table construction.
    let source = build_source(n, 0xeb10_0000_0000_0000 ^ n as u64);
    let source_bytes = source.len() as u64;
    let budget_bytes = source_bytes.saturating_mul(4).saturating_add(1 << 20);
    let start = Instant::now();
    let body = create_auto_within_budget(
        &source,
        ManifestationProfile::Sparse { interval: 1024 },
        &ManifestationBudget {
            maximum_physical_bytes: budget_bytes,
        },
    )
    .map_err(|e| format!("create_auto: {e}"))?;
    let build_ms = start.elapsed().as_secs_f64() * 1e3;
    let physical = body.physical_bytes();
    let path = work.join(format!("scale-{n}.ekb"));
    write_body(&path, &body).map_err(|e| format!("write_body: {e}"))?;
    drop(body);
    let file_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    row.insert("ekbloom_source_bytes".into(), json!(source_bytes));
    row.insert("ekbloom_build_ms".into(), json!(build_ms));
    row.insert("ekbloom_physical_bytes".into(), json!(physical));
    row.insert("ekbloom_file_bytes".into(), json!(file_bytes));

    let open = OpenObject::open(&path).map_err(|e| format!("open: {e}"))?;
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut buf = [0u8; SLOT];
    let mut samples = Vec::with_capacity(10_000);
    let mut reads = 0u64;
    for _ in 0..10_000 {
        let offset = (xs(&mut state) % u64::from(n as u32)) as u64 * SLOT as u64;
        let start = Instant::now();
        open.read_into(
            ReadRequest {
                offset: HEADER as u64 + offset,
                length: SLOT as u64,
            },
            &mut buf,
        )
        .map_err(|e| format!("read: {e}"))?;
        samples.push(start.elapsed().as_nanos() as u64);
        reads += 1;
    }
    let p50 = percentile(&mut samples.clone(), 0.50);
    let p95 = percentile(&mut samples, 0.95);
    row.insert("ekbloom_lookup_p50_ns".into(), json!(p50));
    row.insert("ekbloom_lookup_p95_ns".into(), json!(p95));
    row.insert("ekbloom_lookup_reads".into(), json!(reads));
    drop(open);
    let _ = fs::remove_file(&path);

    // Conventional index: the same 32-byte keys in a HashMap.
    let mut index: HashMap<u64, u32> = HashMap::with_capacity(n);
    let start = Instant::now();
    let mut state = 0x0bad_5eed_0000_0001u64;
    for ordinal in 0..n {
        index.insert(xs(&mut state), ordinal as u32);
    }
    row.insert(
        "index_build_ms".into(),
        json!(start.elapsed().as_secs_f64() * 1e3),
    );
    let mut samples = Vec::with_capacity(10_000);
    let mut state = 0xfeed_0000_0000_0001u64;
    for _ in 0..10_000 {
        let key = xs(&mut state);
        let start = Instant::now();
        let _ = index.get(&key);
        samples.push(start.elapsed().as_nanos() as u64);
    }
    let p50 = percentile(&mut samples.clone(), 0.50);
    let p95 = percentile(&mut samples, 0.95);
    row.insert("index_lookup_p50_ns".into(), json!(p50));
    row.insert("index_lookup_p95_ns".into(), json!(p95));
    Ok(Json::Object(row))
}

pub fn run(out: &Path, work: &Path, sizes: &[usize]) -> Result<Json, String> {
    fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for &n in sizes {
        match measure_size(n, work) {
            Ok(row) => rows.push(row),
            Err(err) => {
                errors.push(json!({"keys": n, "error": err}));
                // A failed size does not hide the smaller sizes that completed.
            }
        }
    }
    // The quadratic baseline: measured only while it completes quickly. The
    // projection is a statement about the measured growth, never a substitute
    // for a timed row.
    let mut quadratic = Vec::new();
    for &n in &[1_000usize, 4_000, 16_000, 64_000] {
        if n > 64_000 {
            break;
        }
        let data = pairs(n, 0x9e37_0000_0000_0000 ^ n as u64);
        let start = Instant::now();
        let conflict = ekbloom::bloom_key::first_conflict(data);
        let ms = start.elapsed().as_secs_f64() * 1e3;
        quadratic.push(json!({"keys": n, "ms": ms, "conflict": conflict.is_some()}));
        if ms > 30_000.0 {
            break;
        }
    }
    let note = if quadratic
        .last()
        .and_then(|r| r.get("keys"))
        .and_then(Json::as_u64)
        == Some(64_000)
    {
        "the quadratic baseline was measured through 64k keys; 100k and 1M were not \
         timed because the measured growth is quadratic and the projection exceeds the \
         phase cap"
    } else {
        "the quadratic baseline was stopped at the last size that completed under the \
         30-second guard; the bounded construction was measured at every declared size"
    };
    Ok(json!({
        "schema": "weft.ekbloom_context.scale.v1",
        "generated_utc": std::env::var("WEFT_EKBLOOM_CONTEXT_STAMP").ok(),
        "sizes": sizes,
        "rows": rows,
        "errors": errors,
        "quadratic_first_conflict": quadratic,
        "note": note,
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
            "memory": std::env::var("WEFT_COURT_MEMORY").ok(),
            "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
            "pids": std::env::var("WEFT_COURT_PIDS").ok(),
            "revisions": {
                "WeftFS": std::env::var("WEFT_REPO_WEFTFS").ok(),
                "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
                "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
                "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
            },
        },
    }))
}
