//! KV prefix index court (subphase T063).
//!
//! # What it measures
//!
//! The **derived index**, not an inference engine. Two fixture classes:
//!
//! * an **end-to-end store** — real `weft.kv.prefix.v1` records and real
//!   mode-12 page objects, written in batch commits, indexed, reopened, and
//!   queried through both the index and the oracle scan;
//! * an **index-only** synthetic population — encoded segments and a binary
//!   search, used only to show how the wire format and the probe cost scale
//!   past the population the 3 GiB / 1 GiB court can hold as a real store.
//!   Rows from this class are labelled `"class": "index-only"` and are never
//!   presented as end-to-end results.
//!
//! Every lookup row also runs the oracle scan on the *same* fixture, so the
//! "index vs scan" comparison is differential, not a comparison against a
//! remembered number.
//!
//! # Modes
//!
//! * no flag — the N ladder plus index-only scaling plus revocation;
//! * `--crash-build <dir>` — internal child: build an index and abort at
//!   `WEFT_KV_INDEX_FAULT`; the parent runs it and then reopens the store.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftfs::identity::Hash32;
use weftfs::kv_index::{
    entries_accumulator, KvIndexEntry, KvIndexEntryKind, KvIndexSegment, SegmentKind,
};
use weftfs::kv_prefix::{
    kv_page_bytes, kv_token_ids, KvLookup, KvPrefixBatchItem, KvPrefixRequest, KvPrefixSpec,
};
use weftfs::store::{OpenMode, Options, Store};

const TENANT: &str = "tenant-a";
const MODEL: &str = "synthetic-tiny";
const LAYERS: u32 = 1;
const HEADS: u32 = 1;
const HEAD_DIM: u32 = 1;
const PAGE_ROWS: u64 = 8;
const WORD_BYTES: usize = 2;
const NOW_NS: i64 = 1_700_000_000_000_000_000;
const BATCH: usize = 256;

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
        "p95_ns": pick(95),
        "p99_ns": pick(99),
    })
}

fn peak_rss_kib() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches(" kB")
                .parse::<u64>()
                .unwrap_or(0);
        }
    }
    0
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn spec() -> KvPrefixSpec {
    KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: Hash32::from_bytes([0x11; 32]),
        attention_hash: Hash32::from_bytes([0x22; 32]),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: HEAD_DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        pinned: false,
    }
}

fn request(tokens: &[u64]) -> KvPrefixRequest {
    KvPrefixRequest {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: Hash32::from_bytes([0x11; 32]),
        attention_hash: Hash32::from_bytes([0x22; 32]),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: HEAD_DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        tokens: tokens.to_vec(),
    }
}

/// Tokens for prefix `index`: page-aligned, distinct, 4..=12 pages.
fn tokens_for(index: u64) -> Vec<u64> {
    let pages = 4 + (index % 9);
    kv_token_ids(0x51ED + index, pages * PAGE_ROWS)
}

fn payload(tokens: u64, seed: u64) -> Vec<Vec<Vec<u8>>> {
    let page_count = tokens.div_ceil(PAGE_ROWS);
    let mut layers = Vec::new();
    for layer in 0..LAYERS {
        let mut pages = Vec::new();
        for index in 0..page_count {
            let len = (PAGE_ROWS * u64::from(HEADS) * u64::from(HEAD_DIM)) as usize * WORD_BYTES;
            pages.push(kv_page_bytes(seed, layer, index, len));
        }
        layers.push(pages);
    }
    layers
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => total = total.saturating_add(dir_size(&entry.path())),
            Ok(_) => total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0)),
            Err(_) => {}
        }
    }
    total
}

fn count_files(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => total = total.saturating_add(count_files(&entry.path())),
            Ok(_) => total += 1,
            Err(_) => {}
        }
    }
    total
}

/// A populate failure that still reports how many prefixes were durably
/// written before it. The distinction matters: a rung *requested* at 20,000
/// prefixes that dies at 11,776 has a ceiling of 11,776, not 20,000, and
/// reporting the request as the ceiling would be a false scale claim.
#[derive(Debug)]
struct PopulateFailure {
    written: u64,
    note: String,
}

/// Populate `count` prefixes in batches of [`BATCH`] (one commit per batch).
fn populate(
    store: &mut Store,
    count: u64,
    page_seed_base: u64,
) -> std::result::Result<f64, PopulateFailure> {
    let started = Instant::now();
    let spec = spec();
    let mut next = 0u64;
    while next < count {
        let end = (next + BATCH as u64).min(count);
        let tokens: Vec<Vec<u64>> = (next..end).map(tokens_for).collect();
        let payloads: Vec<Vec<Vec<Vec<u8>>>> = (next..end)
            .map(|index| payload(tokens_for(index).len() as u64, page_seed_base + index))
            .collect();
        let names: Vec<String> = (next..end).map(|index| format!("p-{index:010}")).collect();
        let items: Vec<KvPrefixBatchItem<'_>> = (0..(end - next) as usize)
            .map(|offset| KvPrefixBatchItem {
                name: &names[offset],
                spec: &spec,
                tokens: &tokens[offset],
                parent_name: None,
                new_page_bytes: &payloads[offset],
            })
            .collect();
        store
            .kv_prefix_put_batch_seeded(&items, 0, 0, &[0x5A; 32])
            .map_err(|err| PopulateFailure {
                written: next,
                note: format!("batch put at {next}: {err}"),
            })?;
        next = end;
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

/// Probe `samples` exact requests along each path (interleaved so a clock shift
/// cannot favour one).
fn probe(store: &Store, indices: &[u64], samples: usize) -> (Vec<u64>, Vec<u64>, u64) {
    let mut indexed = Vec::with_capacity(samples);
    let mut scan = Vec::with_capacity(samples);
    let mut mismatches = 0u64;
    for round in 0..samples {
        let index = indices[round % indices.len()];
        let tokens = tokens_for(index);
        let req = request(&tokens);
        let started = Instant::now();
        let a = store.kv_prefix_lookup_indexed(&req).expect("indexed");
        indexed.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        let started = Instant::now();
        let b = store.kv_prefix_lookup_scan(&req).expect("scan");
        scan.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        let same = match (&a, &b) {
            (KvLookup::Reusable { record: x, .. }, KvLookup::Reusable { record: y, .. }) => {
                x.id().unwrap() == y.id().unwrap()
            }
            (KvLookup::Recompute { .. }, KvLookup::Recompute { .. }) => true,
            _ => false,
        };
        if !same {
            mismatches += 1;
        }
        assert!(matches!(a, KvLookup::Reusable { .. }), "fixture miss");
    }
    (indexed, scan, mismatches)
}

struct LadderRow {
    row: Json,
    ok: bool,
    note: String,
    /// Prefixes durably written when a populate failure stopped the rung.
    achieved: Option<u64>,
}

/// One rung: build a fresh store of `count` prefixes and measure everything.
fn ladder_rung(root: &Path, count: u64) -> LadderRow {
    let dir = root.join(format!("n-{count}"));
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return LadderRow {
            row: json!({"n": count}),
            ok: false,
            note: "cannot create fixture directory".to_owned(),
            achieved: None,
        };
    }
    let mut store = match Store::init(&dir, options()) {
        Ok(store) => store,
        Err(err) => {
            return LadderRow {
                row: json!({"n": count}),
                ok: false,
                note: format!("store init failed: {err}"),
                achieved: None,
            }
        }
    };
    let populate_ms = match populate(&mut store, count, 10_000) {
        Ok(ms) => ms,
        Err(failure) => {
            let _ = std::fs::remove_dir_all(&dir);
            return LadderRow {
                row: json!({
                    "n": count,
                    "class": "end-to-end",
                    "achieved": failure.written,
                    "note": failure.note,
                }),
                ok: false,
                note: failure.note,
                achieved: Some(failure.written),
            };
        }
    };
    let generation_after_populate = store.generation();
    let logical = store.kv_prefix_list().map(|list| list.len()).unwrap_or(0);
    assert_eq!(logical as u64, count);

    let build_started = Instant::now();
    let report = match store.kv_index_build() {
        Ok(report) => report,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dir);
            return LadderRow {
                row: json!({"n": count, "populate_ms": round(populate_ms)}),
                ok: false,
                note: format!("index build failed: {err}"),
                achieved: None,
            };
        }
    };
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
    let status = store.kv_index_status().unwrap();
    assert!(status.valid, "{status:?}");
    let index_bytes = status.bytes;
    let store_bytes = dir_size(&dir);
    let files = count_files(&dir);

    // Lookup probes on the live store (warm).
    let indices: Vec<u64> = (0..count.min(64)).map(|i| (i * 97) % count).collect();
    let samples = ((200_000 / count.max(1)).clamp(20, 200)) as usize;
    let (indexed_ns, scan_ns, mismatches) = probe(&store, &indices, samples);

    // Reopen cost: drop, reopen read-only, time the first indexed lookup
    // (which loads and validates the index) and then a warm batch.
    drop(store);
    let reopen_started = Instant::now();
    let reopened = Store::open_with_options(&dir, OpenMode::ReadOnly, options()).unwrap();
    let reopen_ms = reopen_started.elapsed().as_secs_f64() * 1000.0;
    let first_started = Instant::now();
    let first = reopened
        .kv_prefix_lookup_indexed(&request(&tokens_for(0)))
        .unwrap();
    let first_lookup_ms = first_started.elapsed().as_secs_f64() * 1000.0;
    assert!(matches!(first, KvLookup::Reusable { .. }));
    let (warm_indexed_ns, _, _) = probe(&reopened, &indices, 50);

    let json_row = json!({
        "n": count,
        "class": "end-to-end",
        "populate_ms": round(populate_ms),
        "prefixes": logical,
        "commits": generation_after_populate,
        "index_build_ms": round(build_ms),
        "index_build_reported_ms": round(report.build_ms),
        "index_bytes": index_bytes,
        "index_bytes_per_prefix": round(index_bytes as f64 / count as f64),
        "index_segments": status.segments,
        "store_bytes": store_bytes,
        "store_files": files,
        "reopen_ms": round(reopen_ms),
        "first_indexed_lookup_ms": round(first_lookup_ms),
        "lookup_indexed": distribution(&indexed_ns),
        "lookup_scan": distribution(&scan_ns),
        "lookup_indexed_warm": distribution(&warm_indexed_ns),
        "mismatches": mismatches,
        "peak_rss_kib": peak_rss_kib(),
        "stats": stats_json(&reopened.kv_index_stats_snapshot()),
    });
    let _ = std::fs::remove_dir_all(&dir);
    LadderRow {
        row: json_row,
        ok: true,
        note: String::new(),
        achieved: None,
    }
}

fn stats_json(stats: &weftfs::kv_index::KvIndexStats) -> Json {
    json!({
        "loads": stats.loads,
        "rejects": stats.rejects,
        "rebuilds": stats.rebuilds,
        "lookups": stats.lookups,
        "probes": stats.probes,
        "rejected_candidates": stats.rejected_candidates,
        "fallbacks": stats.fallbacks,
        "deltas_appended": stats.deltas_appended,
        "notes": stats.notes,
    })
}

/// Index-only scaling: encode, write, decode and probe a synthetic population.
fn index_only_row(root: &Path, count: u64) -> Json {
    let started = Instant::now();
    let mut entries = Vec::with_capacity(count as usize);
    for index in 0..count {
        entries.push(KvIndexEntry {
            identity_root: Hash32::from_bytes([0x10; 32]),
            chain_root: Hash32::from_bytes({
                let mut raw = [0u8; 32];
                raw[..8].copy_from_slice(&index.to_le_bytes());
                raw[24..].copy_from_slice(&(index.rotate_left(17)).to_le_bytes());
                raw
            }),
            record_prefix_id: Hash32::from_bytes({
                let mut raw = [0x21u8; 32];
                raw[..8].copy_from_slice(&index.to_le_bytes());
                raw
            }),
            record_object_id: Hash32::from_bytes({
                let mut raw = [0x31u8; 32];
                raw[..8].copy_from_slice(&index.to_le_bytes());
                raw
            }),
            position_end: 8 * (index % 65 + 1),
            page_count: index % 65 + 1,
            kind: KvIndexEntryKind::Live,
        });
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let encode_started = Instant::now();
    let segment = KvIndexSegment::encode(SegmentKind::Base, 1, &entries).unwrap();
    let encode_ms = encode_started.elapsed().as_secs_f64() * 1000.0;
    let path = root.join(format!("index-only-{count}.kvi"));
    std::fs::write(&path, &segment).expect("write segment");
    let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let decode_started = Instant::now();
    let decoded = KvIndexSegment::decode(&segment).unwrap();
    let decode_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(decoded.entries.len() as u64, count);
    // Probe 200 keys by binary search over the decoded entry vector.
    let mut probes = Vec::with_capacity(200);
    for sample in 0..200u64 {
        let key = &decoded.entries[((sample * 7919) % count) as usize];
        let started = Instant::now();
        let found = decoded
            .entries
            .binary_search_by_key(&(key.identity_root, key.chain_root), |entry| {
                (entry.identity_root, entry.chain_root)
            })
            .is_ok();
        probes.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        assert!(found);
    }
    let accumulator = entries_accumulator(&entries);
    let _ = std::fs::remove_file(&path);
    json!({
        "n": count,
        "class": "index-only",
        "note": "synthetic entries; no store, no records, no page objects",
        "build_ms": round(build_ms),
        "encode_ms": round(encode_ms),
        "decode_ms": round(decode_ms),
        "segment_bytes": written,
        "bytes_per_entry": round(written as f64 / count as f64),
        "probe": distribution(&probes),
        "accumulator": accumulator.to_hex(),
        "peak_rss_kib": peak_rss_kib(),
    })
}

/// Build an index in a child process and abort at `point`.
fn crash_build_child(dir: &Path) -> ! {
    let mut store = Store::open_with_options(dir, OpenMode::ReadWrite, options())
        .expect("open for crash build");
    let _ = store.kv_index_build();
    eprintln!("crash build did not abort at the requested point");
    std::process::exit(70);
}

/// Parent side of the crash court.
fn crash_case(root: &Path, point: &str, count: u64) -> Json {
    let dir = root.join(format!("crash-{point}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut store = Store::init(&dir, options()).unwrap();
        populate(&mut store, count, 50_000).expect("crash fixture");
    }
    let exe = std::env::current_exe().expect("current exe");
    let started = Instant::now();
    let status = std::process::Command::new(exe)
        .arg("--crash-build")
        .arg(&dir)
        .env("WEFT_KV_INDEX_FAULT", point)
        .status()
        .expect("spawn crash child");
    let child_ms = started.elapsed().as_secs_f64() * 1000.0;
    let aborted = !status.success();
    let mut store = Store::open_with_options(&dir, OpenMode::ReadWrite, options()).unwrap();
    let status_after = store.kv_index_status().unwrap();
    // Whatever survived, the answer must be exact on both paths.
    let mut mismatches = 0u64;
    let mut probes = 0u64;
    for index in [0u64, count / 2, count - 1] {
        let req = request(&tokens_for(index));
        let a = store.kv_prefix_lookup_indexed(&req).unwrap();
        let b = store.kv_prefix_lookup_scan(&req).unwrap();
        probes += 1;
        let same = match (&a, &b) {
            (KvLookup::Reusable { record: x, .. }, KvLookup::Reusable { record: y, .. }) => {
                x.id().unwrap() == y.id().unwrap()
            }
            _ => false,
        };
        if !same {
            mismatches += 1;
        }
    }
    // A rebuild must produce a valid index with the same entry count.
    let rebuilt = store.kv_index_build().unwrap();
    let rebuilt_status = store.kv_index_status().unwrap();
    assert!(rebuilt_status.valid);
    let fresh = store
        .kv_prefix_lookup_indexed(&request(&tokens_for(0)))
        .unwrap();
    assert!(matches!(fresh, KvLookup::Reusable { .. }));
    json!({
        "fault_point": point,
        "child_exit_success": status.success(),
        "child_aborted": aborted,
        "child_ms": round(child_ms),
        "index_present_after_crash": status_after.present,
        "index_valid_after_crash": status_after.valid,
        "entries_after_crash": status_after.entries,
        "probes": probes,
        "mismatches": mismatches,
        "rebuild_entries": rebuilt.entries,
        "rebuild_valid": rebuilt_status.valid,
        "rebuild_bytes": rebuilt.bytes,
    })
}

fn revocation_case(root: &Path, count: u64) -> Json {
    let dir = root.join("revocation");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = Store::init(&dir, options()).unwrap();
    populate(&mut store, count, 70_000).expect("revocation fixture");
    store.kv_index_build().unwrap();
    let before = store.kv_index_status().unwrap();
    let revoked_names: Vec<String> = (0..count)
        .step_by(10)
        .map(|index| format!("p-{index:010}"))
        .collect();
    let revoke_started = Instant::now();
    for name in &revoked_names {
        store.kv_prefix_revoke(name, "court", NOW_NS, 0, 0).unwrap();
    }
    let revoke_ms = revoke_started.elapsed().as_secs_f64() * 1000.0;
    let after = store.kv_index_status().unwrap();
    // A revoked prefix is returned by neither path; its neighbours still are.
    let first_revoked: u64 = 0;
    let req = request(&tokens_for(first_revoked));
    let indexed = store.kv_prefix_lookup_indexed(&req).unwrap();
    let scan = store.kv_prefix_lookup_scan(&req).unwrap();
    let revoked_returns = matches!(indexed, KvLookup::Reusable { .. }) as u64
        + matches!(scan, KvLookup::Reusable { .. }) as u64;
    let neighbour = request(&tokens_for(1));
    let neighbour_ok = matches!(
        store.kv_prefix_lookup_indexed(&neighbour).unwrap(),
        KvLookup::Reusable { .. }
    );
    // Invalidation is bounded: one delta per revocation, so a revocation batch
    // larger than the delta bound leaves the index stale (the reader falls back
    // to the oracle, never to an unverified entry). One rebuild restores it.
    let rebuild_started = Instant::now();
    let rebuilt = store.kv_index_build().unwrap();
    let rebuild_ms = rebuild_started.elapsed().as_secs_f64() * 1000.0;
    let rebuilt_status = store.kv_index_status().unwrap();
    let still_valid = matches!(
        store.kv_prefix_lookup_indexed(&neighbour).unwrap(),
        KvLookup::Reusable { .. }
    );
    let still_revoked = matches!(
        store.kv_prefix_lookup_indexed(&req).unwrap(),
        KvLookup::Reusable { .. }
    );
    json!({
        "prefixes": count,
        "revoked": revoked_names.len(),
        "revocation_epoch": store.kv_revocation_epoch().unwrap(),
        "revoke_all_ms": round(revoke_ms),
        "revoke_ms_each": round(revoke_ms / revoked_names.len() as f64),
        "entries_before": before.entries,
        "entries_after": after.entries,
        "entry_delta": before.entries as i64 - after.entries as i64,
        "index_valid_after": after.valid,
        "revoked_returns_across_both_paths": revoked_returns,
        "unrevoked_neighbour_reusable": neighbour_ok,
        "index_bytes_before": before.bytes,
        "index_bytes_after": after.bytes,
        "rebuild_ms": round(rebuild_ms),
        "rebuild_entries": rebuilt.entries,
        "rebuild_valid": rebuilt_status.valid,
        "unrevoked_still_reusable_after_rebuild": still_valid,
        "revoked_still_absent_after_rebuild": !still_revoked,
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
    let dir = value("--dir").map(PathBuf::from).unwrap_or_else(|| {
        let base = if Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        base.join(format!("kv-index-bench-{}", std::process::id()))
    });
    if args.iter().any(|arg| arg == "--crash-build") {
        let dir = args.last().expect("crash-build dir").clone();
        crash_build_child(Path::new(&dir));
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create bench dir");

    let started = Instant::now();
    let ladder_sizes: Vec<u64> = value("--ladder")
        .map(|text| text.split(',').filter_map(|v| v.parse().ok()).collect())
        .unwrap_or_else(|| vec![1_000, 5_000, 20_000, 50_000, 100_000]);
    let max_index_only: u64 = value("--index-only-max")
        .and_then(|text| text.parse().ok())
        .unwrap_or(1_000_000);

    let mut ladder = Vec::new();
    let mut ceiling = None;
    for count in &ladder_sizes {
        let rung = ladder_rung(&dir, *count);
        if !rung.ok {
            ceiling = Some(json!({
                "requested": count,
                "achieved": rung.achieved,
                "note": rung.note,
            }));
            ladder.push(rung.row);
            break;
        }
        ladder.push(rung.row);
    }

    let index_only: Vec<Json> = Vec::new();
    let index_only_ceiling: Option<Json> = None;
    let mut index_only_rows = index_only;
    let mut size = 1_000u64;
    while size <= max_index_only {
        if std::fs::metadata("/tmp").is_ok() {
            index_only_rows.push(index_only_row(&dir, size));
        }
        let next = size * 10;
        if next > max_index_only {
            break;
        }
        size = next;
    }
    // Include a non-power rung near the bound when the budget allows.
    if max_index_only >= 1_000_000 && size > max_index_only {
        // already covered by the loop
    }

    let crash = vec![
        crash_case(&dir, "after-base-segment", 2_000),
        crash_case(&dir, "after-root", 2_000),
    ];
    let revocation = revocation_case(&dir, 2_000);

    let receipt = json!({
        "schema": "weft.kv.index.bench.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unknown".to_owned()),
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "label": "KV prefix index court: end-to-end store rows and index-only scaling rows; no inference engine",
        "fixture": {
            "model": MODEL,
            "dtype": "bf16",
            "layers": LAYERS,
            "kv_heads": HEADS,
            "head_dim": HEAD_DIM,
            "page_rows": PAGE_ROWS,
            "bytes_per_page_all_layers": PAGE_ROWS as u64
                * u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES as u64,
            "tokens_per_prefix": "32..=96 (4..=12 page-aligned pages)",
            "population": "batch of 256 prefixes per commit",
        },
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "entry_bytes": weftfs::kv_index::index_entry_bytes(),
        "ladder": ladder,
        "end_to_end_ceiling": ceiling,
        "index_only": index_only_rows,
        "index_only_ceiling": index_only_ceiling,
        "crash": crash,
        "revocation": revocation,
        "wall_ms": round(started.elapsed().as_secs_f64() * 1000.0),
        "peak_rss_kib": peak_rss_kib(),
        "non_claims": [
            "no inference engine ran; this is a storage index court",
            "index-only rows are synthetic and are not end-to-end store results",
            "one host, one court, warm page cache; no fleet claim",
            "scan timings are the oracle on the same fixture, not a baseline product",
        ],
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
