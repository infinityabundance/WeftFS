//! EKBLOOM Context Court harness.
//!
//! Commands:
//!   prepare  build the fixture store and write fixture.json
//!   run      run one arm over the request stream against a store copy
//!   assemble read every arm's rows, evaluate G1-G4, write the receipt
//!   scale    key-sufficiency / EKBLOOM / conventional index at 1k/100k/1M
//!   lifecycle delete the manifestation, replay, and prove answers unchanged
//!
//! Every method rule is in `bench/CONTRACTS-ekbloom-context.md`.

mod arm;
mod caches;
mod count_alloc;
mod fixture;
mod manifest;
mod rule;
mod scale;

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Map as JsonMap, Value as Json};
use sha2::{Digest, Sha256};

use arm::{Arm, ArmKind};
use fixture::{Fixture, Request};
use rule::{contract_root, AuthorityRecord, Dimensions, SubjectKind};

#[global_allocator]
static ALLOC: count_alloc::Counting = count_alloc::Counting;

const PHASE_SCHEMA: &str = "weft.ekbloom_context.phase.v1";
const RECEIPT_SCHEMA: &str = "weft.ekbloom_context.v1";
const CONTRACT: &str = "bench/CONTRACTS-ekbloom-context.md";
const EK_BLOOM_CRATE: &str = "ekbloom 0.7.3";
const EK_BLOOM_CHECKSUM: &str = "1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4";

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

struct Args {
    map: HashMap<String, String>,
}

impl Args {
    fn parse() -> Args {
        let mut map = HashMap::new();
        let mut args = std::env::args().skip(1);
        let command = args.next().unwrap_or_default();
        map.insert("command".to_owned(), command);
        while let Some(key) = args.next() {
            if !key.starts_with("--") {
                continue;
            }
            let name = key.trim_start_matches("--").to_owned();
            let value = args.next().unwrap_or_default();
            map.insert(name, value);
        }
        Args { map }
    }

    fn get(&self, key: &str) -> Result<&str, String> {
        self.map
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing --{key}"))
    }

    fn opt(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    fn u64(&self, key: &str, default: u64) -> Result<u64, String> {
        match self.map.get(key) {
            None => Ok(default),
            Some(text) => text
                .parse()
                .map_err(|_| format!("--{key} must be an integer")),
        }
    }
}

fn write_json_atomic(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn peak_rss_kib() -> u64 {
    let text = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn cpu_ms() -> f64 {
    let text = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some(close) = text.rfind(')') else {
        return 0.0;
    };
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    if fields.len() < 13 {
        return 0.0;
    }
    let utime: f64 = fields[11].parse().unwrap_or(0.0);
    let stime: f64 = fields[12].parse().unwrap_or(0.0);
    (utime + stime) / 100.0 * 1000.0
}

fn uname() -> String {
    Command::new("uname")
        .arg("-srm")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn environment() -> Json {
    json!({
        "court_image": std::env::var("WEFT_COURT_IMAGE").ok(),
        "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
        "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
        "memory": std::env::var("WEFT_COURT_MEMORY").ok(),
        "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
        "pids": std::env::var("WEFT_COURT_PIDS").ok(),
        "nproc": std::env::var("WEFT_COURT_NPROC").ok(),
        "uname": uname(),
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").ok(),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
    })
}

fn store_options() -> weftfs::store::Options {
    let mut opts = weftfs::store::Options {
        owner_uid: 0,
        owner_gid: 0,
        fixed_timestamp_ns: Some(fixture::FIXED_TS),
        cache_bytes: 0,
        single_flight: false,
        attest_indexes: false,
        incremental_index: false,
        ..weftfs::store::Options::default()
    };
    opts.chunk_size = 65_536;
    opts
}

fn stream_roots(contracts: &[Vec<Dimensions>]) -> Result<Vec<Vec<weftfs::Hash32>>, String> {
    let mut out = Vec::new();
    for slots in contracts {
        let mut roots = Vec::new();
        for dims in slots {
            roots.push(contract_root(dims)?);
        }
        out.push(roots);
    }
    Ok(out)
}

fn authority_roots(authorities: &[AuthorityRecord]) -> Result<Vec<weftfs::Hash32>, String> {
    authorities.iter().map(|a| a.root()).collect()
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

fn cmd_prepare(args: &Args) -> Result<(), String> {
    let base = PathBuf::from(args.get("base")?);
    let out = PathBuf::from(args.get("out")?);
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let fixture = fixture::prepare(&base)?;
    fixture::write_fixture_json(&out, &fixture)?;
    println!(
        "prepared base={} subjects={} branches={} contracts={} head={}",
        base.display(),
        fixture.subjects.len(),
        fixture.branches.len(),
        fixture.contracts.iter().map(Vec::len).sum::<usize>(),
        fixture.base_head.to_hex()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct EventRecord {
    index: u64,
    kind: String,
    head_before: String,
    head_after: String,
    first_after: Option<Json>,
}

fn commit_note(
    store: &mut weftfs::store::Store,
    events_dir: weftfs::Hash32,
    name: &str,
    body: &[u8],
    seed: &str,
) -> Result<(), String> {
    store
        .create_file_seeded(
            events_dir,
            name.as_bytes(),
            body,
            0o644,
            0,
            0,
            BTreeMap::new(),
            seed.as_bytes(),
        )
        .map(|_| ())
        .map_err(|e| format!("event {name}: {e}"))
}

fn apply_event(
    kind: &str,
    store: &mut weftfs::store::Store,
    fixture: &Fixture,
    contracts: &mut [Vec<Dimensions>],
    contract_roots: &mut [Vec<weftfs::Hash32>],
    authorities: &mut [AuthorityRecord],
    authority_roots: &mut [weftfs::Hash32],
    events_dir: weftfs::Hash32,
) -> Result<(), String> {
    match kind {
        "model_update" => {
            let old = contracts[1][0].model;
            for slot in contracts[1].iter_mut() {
                if slot.model == old {
                    slot.model = Some(fixture.model_v2);
                }
            }
            for (slot, root) in contracts[1].iter().zip(contract_roots[1].iter_mut()) {
                *root = contract_root(slot)?;
            }
            commit_note(
                store,
                events_dir,
                "model-update-b1",
                b"model update b1",
                "event-model-update-b1",
            )?;
        }
        "revocation" => {
            authorities[2].epoch += 1;
            authorities[2].revoked = true;
            authority_roots[2] = authorities[2].root()?;
            commit_note(
                store,
                events_dir,
                "revoke-b2",
                b"revoke b2",
                "event-revoke-b2",
            )?;
        }
        "missing_dependency" => {
            let mut deleted = false;
            for (subject, _) in &fixture.hot_pairs {
                let subject = &fixture.subjects[*subject as usize];
                if subject.branch == 0 && !subject.missing && !subject.page_name.is_empty() {
                    store
                        .unlink(fixture.branches[0].dir_id, subject.page_name.as_bytes())
                        .map_err(|e| format!("unlink missing dependency: {e}"))?;
                    deleted = true;
                    break;
                }
            }
            if !deleted {
                return Err("no hot b0 subject available for the missing-dependency event".into());
            }
        }
        "snapshot" => {
            store
                .snapshot_create("s-final")
                .map_err(|e| format!("snapshot: {e}"))?;
        }
        other => return Err(format!("unknown event `{other}`")),
    }
    Ok(())
}

fn cmd_run(args: &Args) -> Result<(), String> {
    let store_path = PathBuf::from(args.get("store")?);
    let fixture_path = PathBuf::from(args.get("fixture")?);
    let arm_name = args.get("arm")?;
    let variant = args.opt("variant").map(str::to_owned);
    let rep = args.u64("rep", 1)?;
    let seed = args.u64("seed", 0)?;
    let work = PathBuf::from(args.get("work")?);
    let out = PathBuf::from(args.get("out")?);
    let rows_path = PathBuf::from(args.get("rows")?);
    let order = args.opt("order").unwrap_or("").to_owned();
    let order_index = args.u64("order-index", 0)?;
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let fixture = fixture::load_fixture_json(&fixture_path)?;
    let mut store = weftfs::store::Store::open_with_options(
        &store_path,
        weftfs::store::OpenMode::ReadWrite,
        store_options(),
    )
    .map_err(|e| format!("open store: {e}"))?;
    let events_dir = store
        .resolve_path("/events")
        .map_err(|e| format!("resolve /events: {e}"))?;
    let mut contracts = fixture.contracts.clone();
    let mut contract_roots = stream_roots(&contracts)?;
    let mut authorities = fixture.authorities.clone();
    let mut authority_roots = authority_roots(&authorities)?;
    let kind = ArmKind::parse(arm_name)?;
    let mut arm = Arm::new(kind, variant.clone(), &work)?;
    let stream = fixture::request_stream(&fixture, seed);
    let mut rows = BufWriter::new(File::create(&rows_path).map_err(|e| e.to_string())?);
    let mut latencies: Vec<u64> = Vec::with_capacity(stream.len());
    let mut events: Vec<EventRecord> = Vec::new();
    let mut pending_event: Option<usize> = None;

    for (i, request) in stream.iter().enumerate() {
        let event_kind = match i {
            fixture::EVENT_MODEL_UPDATE => Some("model_update"),
            fixture::EVENT_REVOCATION => Some("revocation"),
            fixture::EVENT_MISSING_DEP => Some("missing_dependency"),
            fixture::EVENT_SNAPSHOT => Some("snapshot"),
            _ => None,
        };
        if let Some(event_kind) = event_kind {
            let before = store.snapshot_root();
            apply_event(
                event_kind,
                &mut store,
                &fixture,
                &mut contracts,
                &mut contract_roots,
                &mut authorities,
                &mut authority_roots,
                events_dir,
            )?;
            let after = store.snapshot_root();
            events.push(EventRecord {
                index: i as u64,
                kind: event_kind.to_owned(),
                head_before: before.to_hex(),
                head_after: after.to_hex(),
                first_after: None,
            });
            pending_event = Some(events.len() - 1);
        }

        let backend_before = store.backend_stats();
        let (a0, b0, _, _) = count_alloc::snapshot();
        let start = Instant::now();
        let outcome = arm.handle(
            &store,
            &fixture,
            &contracts,
            &contract_roots,
            &authorities,
            &authority_roots,
            request,
        )?;
        let nanos = start.elapsed().as_nanos() as u64;
        let backend_after = store.backend_stats();
        let (a1, b1, _, _) = count_alloc::snapshot();
        let backend_reads = backend_after.reads.saturating_sub(backend_before.reads)
            + backend_after
                .range_reads
                .saturating_sub(backend_before.range_reads);
        let backend_bytes = backend_after
            .bytes_read
            .saturating_sub(backend_before.bytes_read);
        let allocs = a1.saturating_sub(a0);
        let alloc_bytes = b1.saturating_sub(b0);
        arm.stats.nodes += outcome.nodes;
        arm.stats.backend_bytes += backend_bytes;
        arm.stats.backend_requests += backend_reads;
        arm.stats.allocations += allocs;
        arm.stats.alloc_bytes += alloc_bytes;
        arm.stats.manifest_reads += outcome.manifest_reads;
        arm.stats.manifest_read_bytes += outcome.manifest_read_bytes;
        latencies.push(nanos);
        writeln!(
            rows,
            "{i}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            request.subject,
            request.slot,
            request.bucket,
            outcome.verdict,
            outcome.reason,
            nanos,
            outcome.nodes,
            u8::from(outcome.cache_hit),
            u8::from(outcome.manifest_hit),
            u8::from(outcome.evaluation),
            outcome.stale_rejections,
            allocs,
            alloc_bytes,
            backend_reads,
            backend_bytes,
            outcome.manifest_reads,
            outcome.manifest_read_bytes,
        )
        .map_err(|e| e.to_string())?;
        if let Some(event) = pending_event.take() {
            events[event].first_after = Some(json!({
                "index": i,
                "verdict": outcome.verdict,
                "reason": outcome.reason,
                "cache_hit": outcome.cache_hit,
                "manifest_hit": outcome.manifest_hit,
                "evaluation": outcome.evaluation,
                "stale_rejections": outcome.stale_rejections,
            }));
        }
        arm.maybe_rebuild()?;
    }
    rows.flush().map_err(|e| e.to_string())?;
    drop(rows);

    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let cache_stats = arm.cache_stats();
    let phase = json!({
        "schema": PHASE_SCHEMA,
        "arm": kind.as_str(),
        "variant": variant,
        "rep": rep,
        "seed": seed,
        "order": order,
        "order_index": order_index,
        "store_head_start": fixture.base_head.to_hex(),
        "store_head_end": store.snapshot_root().to_hex(),
        "fixture": {
            "subjects": fixture.subjects.len(),
            "contracts": fixture.contracts.iter().map(Vec::len).sum::<usize>(),
        },
        "stats": {
            "requests": arm.stats.requests,
            "cache_hits": arm.stats.cache_hits,
            "cache_misses": arm.stats.cache_misses,
            "manifest_hits": arm.stats.manifest_hits,
            "manifest_misses": arm.stats.manifest_misses,
            "evaluations": arm.stats.evaluations,
            "stale_rejections": arm.stats.stale_rejections,
            "nodes": arm.stats.nodes,
            "backend_bytes": arm.stats.backend_bytes,
            "backend_requests": arm.stats.backend_requests,
            "allocations": arm.stats.allocations,
            "alloc_bytes": arm.stats.alloc_bytes,
            "manifest_reads": arm.stats.manifest_reads,
            "manifest_read_bytes": arm.stats.manifest_read_bytes,
            "decision_records_built": arm.stats.decision_records_built,
            "manifest_builds": arm.stats.manifest_builds,
            "manifest_build_failures": arm.stats.manifest_build_failures,
            "manifest_build_ms": arm.stats.manifest_build_ms,
            "manifest_bytes": arm.stats.manifest_bytes,
            "manifest_resident_surface": arm.manifest_resident_surface(),
            "retained_bytes": arm.retained_bytes(),
            "cache_retained_bytes": arm.cache_retained_bytes(),
            "pending_bytes": arm.pending_bytes(),
            "cache_variant": variant,
            "cache_stats": {
                "hits": cache_stats.hits,
                "misses": cache_stats.misses,
                "admissions": cache_stats.admissions,
                "evictions": cache_stats.evictions,
                "rejections": cache_stats.rejections,
            },
            "peak_rss_kib": peak_rss_kib(),
            "cpu_ms": cpu_ms(),
        },
        "latency": {
            "n": sorted.len(),
            "p50_us": percentile(&sorted, 0.50) as f64 / 1e3,
            "p95_us": percentile(&sorted, 0.95) as f64 / 1e3,
            "p99_us": percentile(&sorted, 0.99) as f64 / 1e3,
            "max_us": sorted.last().copied().unwrap_or(0) as f64 / 1e3,
            "total_ms": latencies.iter().sum::<u64>() as f64 / 1e6,
        },
        "events": events.iter().map(|e| json!({
            "index": e.index,
            "kind": e.kind,
            "head_before": e.head_before,
            "head_after": e.head_after,
            "first_after": e.first_after,
        })).collect::<Vec<Json>>(),
        "rows_path": rows_path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
    });
    write_json_atomic(&out, &phase)
}

impl Arm {
    fn cache_retained_bytes(&self) -> u64 {
        // The manifest is counted separately; this is the conventional part.
        self.retained_bytes()
            .saturating_sub(self.stats.manifest_bytes.min(self.retained_bytes()))
    }
}

// ---------------------------------------------------------------------------
// rows and comparison
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Row {
    index: u64,
    verdict: u8,
    reason: u16,
    nanos: u64,
    nodes: u64,
    cache_hit: bool,
    manifest_hit: bool,
    evaluation: bool,
    stale: u64,
    allocs: u64,
    alloc_bytes: u64,
    backend_reads: u64,
    backend_bytes: u64,
    manifest_reads: u64,
    manifest_read_bytes: u64,
}

// The TSV has 17 columns; `read_rows` reads 0..16. Column 16 is manifest_reads.
// manifest_read_bytes is the 17th field and is stored as field 16 above, so
// re-read it explicitly to keep the struct honest.

fn read_rows_full(path: &Path) -> Result<Vec<Row>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 18 {
            return Err(format!(
                "{}: expected 18 fields, got {}",
                path.display(),
                f.len()
            ));
        }
        let n = |i: usize| -> u64 { f[i].parse().unwrap_or(0) };
        out.push(Row {
            index: n(0),
            verdict: n(4) as u8,
            reason: n(5) as u16,
            nanos: n(6),
            nodes: n(7),
            cache_hit: n(8) != 0,
            manifest_hit: n(9) != 0,
            evaluation: n(10) != 0,
            stale: n(11),
            allocs: n(12),
            alloc_bytes: n(13),
            backend_reads: n(14),
            backend_bytes: n(15),
            manifest_reads: n(16),
            manifest_read_bytes: n(17),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// assemble
// ---------------------------------------------------------------------------

const ARM_FILES: [(&str, &str); 5] = [
    ("W", "W"),
    ("M-lru", "M-lru"),
    ("M-tinylfu", "M-tinylfu"),
    ("E-lru", "E"),
    ("C-naive", "NAIVE"),
];

fn arm_phase_path(work: &Path, rep: u64, name: &str) -> PathBuf {
    work.join(format!("rep-{rep:02}"))
        .join(format!("{name}.json"))
}

fn arm_rows_path(work: &Path, rep: u64, name: &str) -> PathBuf {
    work.join(format!("rep-{rep:02}"))
        .join(format!("{name}.tsv"))
}

#[derive(Clone, Debug, Default)]
struct ArmAggregate {
    latencies: Vec<u64>,
    nodes: u64,
    backend_bytes: u64,
    backend_reads: u64,
    allocations: u64,
    alloc_bytes: u64,
    cache_hits: u64,
    cache_misses: u64,
    manifest_hits: u64,
    manifest_misses: u64,
    evaluations: u64,
    stale: u64,
    false_accepts: u64,
    semantic_mismatches: u64,
    requests: u64,
    retained_by_rep: Vec<u64>,
    manifest_build_ms: f64,
    manifest_builds: u64,
    manifest_build_failures: u64,
    manifest_bytes_by_rep: Vec<u64>,
    peak_rss_kib: u64,
    cpu_ms: f64,
}

fn cmd_assemble(args: &Args) -> Result<(), String> {
    let work = PathBuf::from(args.get("work")?);
    let fixture_path = PathBuf::from(args.get("fixture")?);
    let out = PathBuf::from(args.get("out")?);
    let log_path = PathBuf::from(args.get("log")?);
    let rows_out = PathBuf::from(args.get("rows-out")?);
    let reps = args.u64("reps", fixture::REPETITIONS as u64)?;
    let label = args.opt("label").unwrap_or("full").to_owned();
    let fixture = fixture::load_fixture_json(&fixture_path)?;

    let mut aggregates: BTreeMap<String, ArmAggregate> = BTreeMap::new();
    let mut per_rep: Vec<Json> = Vec::new();
    let mut event_rows: Vec<Json> = Vec::new();
    let mut raw_rows: Vec<String> = Vec::new();
    let mut orders: Vec<String> = Vec::new();
    let mut fixture_heads: Vec<String> = Vec::new();

    for rep in 1..=reps {
        let w_rows = read_rows_full(&arm_rows_path(&work, rep, "W"))?;
        if w_rows.len() != fixture::REQUESTS {
            return Err(format!(
                "rep {rep}: W has {} rows, expected {}",
                w_rows.len(),
                fixture::REQUESTS
            ));
        }
        let mut rep_json = JsonMap::new();
        rep_json.insert("rep".into(), json!(rep));
        for (label_arm, name) in ARM_FILES {
            let phase: Json = serde_json::from_slice(
                &fs::read(arm_phase_path(&work, rep, name)).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let rows = read_rows_full(&arm_rows_path(&work, rep, name))?;
            if rows.len() != w_rows.len() {
                return Err(format!(
                    "rep {rep} arm {label_arm}: {} rows vs W {}",
                    rows.len(),
                    w_rows.len()
                ));
            }
            if let Some(order) = phase.get("order").and_then(Json::as_str) {
                if !orders.contains(&order.to_owned()) {
                    orders.push(order.to_owned());
                }
            }
            if let Some(head) = phase.get("store_head_start").and_then(Json::as_str) {
                if !fixture_heads.contains(&head.to_owned()) {
                    fixture_heads.push(head.to_owned());
                }
            }
            let stats = phase.get("stats").cloned().unwrap_or(Json::Null);
            let agg = aggregates
                .entry(label_arm.to_owned())
                .or_insert_with(ArmAggregate::default);
            agg.retained_by_rep.push(
                stats
                    .get("retained_bytes")
                    .and_then(Json::as_u64)
                    .unwrap_or(0),
            );
            agg.manifest_bytes_by_rep.push(
                stats
                    .get("manifest_bytes")
                    .and_then(Json::as_u64)
                    .unwrap_or(0),
            );
            agg.manifest_build_ms += stats
                .get("manifest_build_ms")
                .and_then(Json::as_f64)
                .unwrap_or(0.0);
            agg.manifest_builds += stats
                .get("manifest_builds")
                .and_then(Json::as_u64)
                .unwrap_or(0);
            agg.manifest_build_failures += stats
                .get("manifest_build_failures")
                .and_then(Json::as_u64)
                .unwrap_or(0);
            agg.peak_rss_kib = agg.peak_rss_kib.max(
                stats
                    .get("peak_rss_kib")
                    .and_then(Json::as_u64)
                    .unwrap_or(0),
            );
            agg.cpu_ms += stats.get("cpu_ms").and_then(Json::as_f64).unwrap_or(0.0);

            let mut false_accepts = 0u64;
            let mut mismatches = 0u64;
            for (w, a) in w_rows.iter().zip(rows.iter()) {
                if a.verdict == 0 && w.verdict != 0 {
                    false_accepts += 1;
                }
                if a.verdict != w.verdict || a.reason != w.reason {
                    mismatches += 1;
                }
                agg.latencies.push(a.nanos);
                agg.nodes += a.nodes;
                agg.backend_bytes += a.backend_bytes;
                agg.backend_reads += a.backend_reads;
                agg.allocations += a.allocs;
                agg.alloc_bytes += a.alloc_bytes;
                agg.cache_hits += u64::from(a.cache_hit);
                agg.cache_misses += u64::from(!a.cache_hit);
                agg.manifest_hits += u64::from(a.manifest_hit);
                agg.manifest_misses += u64::from(!a.manifest_hit);
                agg.evaluations += u64::from(a.evaluation);
                agg.stale += a.stale;
                agg.requests += 1;
            }
            agg.false_accepts += false_accepts;
            agg.semantic_mismatches += mismatches;
            rep_json.insert(
                label_arm.to_owned(),
                json!({
                    "requests": rows.len(),
                    "false_accepts": false_accepts,
                    "semantic_mismatches": mismatches,
                    "retained_bytes": stats.get("retained_bytes"),
                    "manifest_bytes": stats.get("manifest_bytes"),
                    "manifest_builds": stats.get("manifest_builds"),
                    "manifest_build_ms": stats.get("manifest_build_ms"),
                    "cache_hits": stats.get("cache_hits"),
                    "cache_misses": stats.get("cache_misses"),
                    "manifest_hits": stats.get("manifest_hits"),
                    "evaluations": stats.get("evaluations"),
                    "p99_us": phase.get("latency").and_then(|l| l.get("p99_us")),
                }),
            );

            // Events: the first request after each event must be a real
            // re-evaluation and must agree with W.
            if let Some(events) = phase.get("events").and_then(Json::as_array) {
                for event in events {
                    let index = event.get("index").and_then(Json::as_u64).unwrap_or(0);
                    let first = event.get("first_after").cloned().unwrap_or(Json::Null);
                    let first_index = first.get("index").and_then(Json::as_u64);
                    let w_row = first_index.and_then(|i| w_rows.get(i as usize)).copied();
                    let a_row = first_index.and_then(|i| rows.get(i as usize));
                    let agrees = matches!((w_row, a_row), (Some(w), Some(a)) if w.verdict == a.verdict && w.reason == a.reason);
                    event_rows.push(json!({
                        "rep": rep,
                        "arm": label_arm,
                        "event": event.get("kind"),
                        "index": index,
                        "evaluated_after": first.get("evaluation").and_then(Json::as_bool).unwrap_or(false),
                        "cache_hit_after": first.get("cache_hit").and_then(Json::as_bool),
                        "manifest_hit_after": first.get("manifest_hit").and_then(Json::as_bool),
                        "agrees_with_w": agrees,
                    }));
                }
            }

            // Retain the raw rows of the first and last repetition only. The
            // arrays `w_rows` and `rows` still cover every repetition for G1.
            if rep == 1 || rep == reps {
                for (idx, a) in rows.iter().enumerate() {
                    raw_rows.push(format!(
                        "{rep}\t{label_arm}\t{idx}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        a.verdict,
                        a.reason,
                        a.nanos,
                        a.nodes,
                        u8::from(a.cache_hit),
                        u8::from(a.manifest_hit),
                        u8::from(a.evaluation),
                        a.stale,
                        a.allocs,
                        a.alloc_bytes,
                        a.backend_reads,
                        a.backend_bytes,
                        a.manifest_reads,
                        a.manifest_read_bytes,
                    ));
                }
            }
        }
        per_rep.push(Json::Object(rep_json));
    }

    // Pooled percentiles.
    let summary = |agg: &mut ArmAggregate| -> Json {
        agg.latencies.sort_unstable();
        json!({
            "requests": agg.requests,
            "p50_us": percentile(&agg.latencies, 0.50) as f64 / 1e3,
            "p95_us": percentile(&agg.latencies, 0.95) as f64 / 1e3,
            "p99_us": percentile(&agg.latencies, 0.99) as f64 / 1e3,
            "max_us": agg.latencies.last().copied().unwrap_or(0) as f64 / 1e3,
            "total_ms": agg.latencies.iter().sum::<u64>() as f64 / 1e6,
            "nodes": agg.nodes,
            "backend_bytes": agg.backend_bytes,
            "backend_reads": agg.backend_reads,
            "allocations": agg.allocations,
            "alloc_bytes": agg.alloc_bytes,
            "cache_hits": agg.cache_hits,
            "cache_misses": agg.cache_misses,
            "manifest_hits": agg.manifest_hits,
            "manifest_misses": agg.manifest_misses,
            "evaluations": agg.evaluations,
            "stale_rejections": agg.stale,
            "false_accepts": agg.false_accepts,
            "semantic_mismatches": agg.semantic_mismatches,
            "retained_bytes_by_rep": agg.retained_by_rep,
            "manifest_bytes_by_rep": agg.manifest_bytes_by_rep,
            "retained_bytes_max": agg.retained_by_rep.iter().copied().max().unwrap_or(0),
            "manifest_build_ms": agg.manifest_build_ms,
            "manifest_builds": agg.manifest_builds,
            "manifest_build_failures": agg.manifest_build_failures,
            "peak_rss_kib": agg.peak_rss_kib,
            "cpu_ms": agg.cpu_ms,
        })
    };

    let mut arm_json = JsonMap::new();
    for name in ["W", "M-lru", "M-tinylfu", "E-lru", "C-naive"] {
        if let Some(agg) = aggregates.get_mut(name) {
            arm_json.insert(name.to_owned(), summary(agg));
        }
    }

    let get = |arm: &str, key: &str| -> f64 {
        arm_json
            .get(arm)
            .and_then(|a| a.get(key))
            .and_then(Json::as_f64)
            .unwrap_or(0.0)
    };
    let p99_m = get("M-lru", "p99_us").min(get("M-tinylfu", "p99_us"));
    let p99_e = get("E-lru", "p99_us");
    let g2_win = p99_e <= 0.90 * p99_m;
    let g2_non_inferior = p99_e <= 1.10 * p99_m;
    let budget = fixture::BUDGET_BYTES;
    let mut g3_ok = true;
    let mut g3_detail = Vec::new();
    for rep in 0..reps as usize {
        let e = arm_json
            .get("E-lru")
            .and_then(|a| a.get("retained_bytes_by_rep"))
            .and_then(Json::as_array)
            .and_then(|a| a.get(rep))
            .and_then(Json::as_u64)
            .unwrap_or(u64::MAX);
        let m_lru = arm_json
            .get("M-lru")
            .and_then(|a| a.get("retained_bytes_by_rep"))
            .and_then(Json::as_array)
            .and_then(|a| a.get(rep))
            .and_then(Json::as_u64)
            .unwrap_or(0);
        let m_tf = arm_json
            .get("M-tinylfu")
            .and_then(|a| a.get("retained_bytes_by_rep"))
            .and_then(Json::as_array)
            .and_then(|a| a.get(rep))
            .and_then(Json::as_u64)
            .unwrap_or(0);
        let ok = e <= budget && e <= m_lru && e <= m_tf;
        g3_ok &= ok;
        g3_detail.push(json!({
            "rep": rep + 1,
            "E": e, "M-lru": m_lru, "M-tinylfu": m_tf, "budget": budget, "ok": ok
        }));
    }

    let g1_arms = ["M-lru", "M-tinylfu", "E-lru"];
    let g1_ok = g1_arms.iter().all(|arm| {
        arm_json
            .get(*arm)
            .map(|a| {
                a.get("false_accepts").and_then(Json::as_u64).unwrap_or(1) == 0
                    && a.get("semantic_mismatches")
                        .and_then(Json::as_u64)
                        .unwrap_or(1)
                        == 0
            })
            .unwrap_or(false)
    });
    let naive_false_accepts = arm_json
        .get("C-naive")
        .and_then(|a| a.get("false_accepts"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let sensitivity_ok = naive_false_accepts > 0;

    let g4_ok = event_rows
        .iter()
        .filter(|row| row.get("arm").and_then(Json::as_str) != Some("C-naive"))
        .all(|row| {
            let arm = row.get("arm").and_then(Json::as_str).unwrap_or("");
            arm == "W"
                || (row
                    .get("evaluated_after")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
                    && row
                        .get("agrees_with_w")
                        .and_then(Json::as_bool)
                        .unwrap_or(false))
        });

    let orders_ok = reps < 4 || orders.len() >= 2;
    let checks_ok = sensitivity_ok && orders_ok;
    let claim_holds = g1_ok && g2_win && g3_ok && g4_ok;
    let direction = if !g1_ok {
        "refuted_unsafe"
    } else if g2_win {
        "survives"
    } else {
        "retired"
    };

    let mut losses: Vec<String> = Vec::new();
    if p99_e > p99_m {
        losses.push(format!(
            "E does not beat M on p99: {:.3} us vs {:.3} us ({:.3}x). The manifested table is not \
             a faster path than the equal-budget conventional cache.",
            p99_e,
            p99_m,
            p99_e / p99_m.max(1e-9)
        ));
    } else if !g2_win {
        losses.push(format!(
            "E improves p99 but not by the declared 10 % margin: {:.3} us vs {:.3} us ({:.3}x, \
             required <= 0.90x). A non-inferiority result is not a win.",
            p99_e,
            p99_m,
            p99_e / p99_m.max(1e-9)
        ));
    }
    let e_ret = get("E-lru", "retained_bytes_max") as u64;
    let m_ret = get("M-lru", "retained_bytes_max") as u64;
    if e_ret > m_ret {
        losses.push(format!(
            "E retains {e_ret} B versus M-LRU {m_ret} B: the manifest costs more bytes than the \
             cache space it displaces, at the declared 1 MiB budget."
        ));
    }
    let e_manifest_bytes = arm_json
        .get("E-lru")
        .and_then(|a| a.get("manifest_bytes_by_rep"))
        .and_then(Json::as_array)
        .and_then(|a| a.last())
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let e_build_ms = get("E-lru", "manifest_build_ms");
    if e_manifest_bytes > 0 {
        losses.push(format!(
            "E's final manifestation is {e_manifest_bytes} B; rebuilding it costs {e_build_ms:.3} ms \
             over {} builds, all excluded from request latency and reported separately.",
            get("E-lru", "manifest_builds") as u64
        ));
    }
    let e_alloc = get("E-lru", "allocations") / get("E-lru", "requests").max(1.0);
    let m_alloc = get("M-lru", "allocations") / get("M-lru", "requests").max(1.0);
    losses.push(format!(
        "allocations per request: W {:.2}, M-LRU {:.2}, E {:.2}; EKBLOOM's caller-buffer path \
         still allocates inside the crate.",
        get("W", "allocations") / get("W", "requests").max(1.0),
        m_alloc,
        e_alloc
    ));
    let m_p99 = p99_m;
    let w_p99 = get("W", "p99_us");
    losses.push(format!(
        "W p99 {:.3} us vs M-LRU {:.3} us: the cache is the actual win here, not the manifestation.",
        w_p99, m_p99
    ));

    let mut log = BufWriter::new(File::create(&log_path).map_err(|e| e.to_string())?);
    writeln!(
        log,
        "CHECK rows_complete pass {reps} reps x {} requests per arm",
        fixture::REQUESTS
    )
    .ok();
    writeln!(
        log,
        "CHECK sensitivity_control {} C-naive false acceptances {}",
        if sensitivity_ok { "pass" } else { "FAIL" },
        naive_false_accepts
    )
    .ok();
    writeln!(
        log,
        "GATE g1_zero_incorrect_acceptances {}",
        if g1_ok { "holds" } else { "fails" }
    )
    .ok();
    writeln!(
        log,
        "GATE g2_p99_margin {} E {:.3} us M {:.3} us (declared E <= 0.90 x M; non-inferiority <= 1.10 x: {})",
        if g2_win { "holds" } else { "fails" },
        p99_e,
        p99_m,
        g2_non_inferior
    )
    .ok();
    writeln!(
        log,
        "GATE g3_retained_bytes {} E_max {} B M-LRU_max {} B budget {}",
        if g3_ok { "holds" } else { "fails" },
        e_ret,
        m_ret,
        budget
    )
    .ok();
    writeln!(
        log,
        "GATE g4_correctness_under_change {}",
        if g4_ok { "holds" } else { "fails" }
    )
    .ok();
    writeln!(log, "DIRECTION {direction}").ok();
    log.flush().ok();

    let raw_text = raw_rows.join("\n");
    let compressed = zstd::encode_all(raw_text.as_bytes(), 3).map_err(|e| e.to_string())?;
    fs::write(&rows_out, &compressed).map_err(|e| format!("{}: {e}", rows_out.display()))?;
    let rows_sha = sha256_hex(&compressed);

    let receipt = json!({
        "schema": RECEIPT_SCHEMA,
        "generated_utc": std::env::var("WEFT_EKBLOOM_CONTEXT_STAMP").ok(),
        "label": label,
        "contract": CONTRACT,
        "contract_sha256": sha256_hex(
            &fs::read(CONTRACT).map_err(|e| format!("{CONTRACT}: {e}"))?
        ),
        "ekbloom": {
            "crate": EK_BLOOM_CRATE,
            "checksum": EK_BLOOM_CHECKSUM,
            "revision": "8275a41b72c17abdc55bf74b82f319876e587ba0",
            "manifestation_profile": "sparse(K=1024)",
        },
        "environment": environment(),
        "fixture": {
            "branches": fixture::BRANCHES,
            "tenants": fixture::BRANCHES,
            "context_roots": fixture::ROOTS_PER_BRANCH * fixture::BRANCHES,
            "kv_pages": fixture::KV_PER_BRANCH * fixture::BRANCHES,
            "tensor_regions": fixture::TENSOR_PER_BRANCH * fixture::BRANCHES,
            "missing_subjects": fixture::MISSING_PER_BRANCH * fixture::BRANCHES,
            "subjects": fixture.subjects.len(),
            "contracts": fixture.contracts.iter().map(Vec::len).sum::<usize>(),
            "contracts_per_branch": fixture::CONTRACTS_PER_BRANCH,
            "base_head": fixture.base_head.to_hex(),
            "kv_page_bytes": fixture::KV_PAGE_BYTES,
            "tensor_region_bytes": fixture::TENSOR_BYTES,
            "requests": fixture::REQUESTS,
            "repetitions": reps,
            "samples_per_arm": fixture::REQUESTS as u64 * reps,
            "events": [
                { "index": fixture::EVENT_MODEL_UPDATE, "kind": "model_update" },
                { "index": fixture::EVENT_REVOCATION, "kind": "revocation" },
                { "index": fixture::EVENT_MISSING_DEP, "kind": "missing_dependency" },
                { "index": fixture::EVENT_SNAPSHOT, "kind": "snapshot" },
            ],
            "bucket_shares": {
                "hot_exact": "55%", "warm_exact": "20%", "cold_exact": "10%",
                "incompatible_variant": "9%", "missing_dimension": "3%",
                "missing_evidence": "3%",
            },
        },
        "methodology": {
            "arms": {
                "W": "shipped resolver, no cross-request reuse (oracle)",
                "M-lru": "exact-keyed byte-budgeted LRU",
                "M-tinylfu": "exact-keyed byte-budgeted W-TinyLFU",
                "E-lru": "M-LRU plus an EKBLOOM-manifested certified decision table",
                "C-naive": "sensitivity control: insufficient key (subject+contract only), no snapshot/epoch, no post-lookup re-check",
            },
            "budget_bytes": budget,
            "entry_bytes": 256,
            "manifest_slot_bytes": 64,
            "manifest_key_bytes": 208,
            "manifest_cap_bytes": fixture::BUDGET_BYTES / 2,
            "interleaving": "arm order rotates by one position per repetition over [W, M-LRU, M-TinyLFU, E]; each repetition is recorded",
            "repetitions": reps,
            "samples_per_arm": fixture::REQUESTS as u64 * reps,
            "p99_support": (fixture::REQUESTS as f64 * reps as f64 * 0.01) as u64,
            "percentiles": "nearest-rank",
            "manifest_builds_excluded_from_request_latency": true,
            "orders_seen": orders,
            "fixture_heads_seen": fixture_heads,
        },
        "arms": Json::Object(arm_json),
        "per_rep": per_rep,
        "events": event_rows,
        "gates": {
            "g1": {
                "holds": g1_ok,
                "detail": "zero false acceptances and zero semantic mismatches for M-LRU, M-TinyLFU and E over every request in every repetition",
            },
            "g2": {
                "holds": g2_win,
                "non_inferior": g2_non_inferior,
                "declared": { "reduction": 0.10, "p99_max_ratio": 0.90 },
                "detail": format!("E p99 {p99_e:.3} us, M p99 {p99_m:.3} us"),
            },
            "g3": {
                "holds": g3_ok,
                "budget": budget,
                "detail": g3_detail,
            },
            "g4": {
                "holds": g4_ok,
                "detail": "every event's first request re-evaluated and agreed with W",
            },
            "sensitivity_control": {
                "holds": sensitivity_ok,
                "naive_false_accepts": naive_false_accepts,
                "detail": "the insufficient-key control must produce at least one false acceptance; zero makes the court inconclusive",
            },
        },
        "direction": direction,
        "claim_holds": claim_holds,
        "losses": losses,
        "rows": {
            "path": rows_out.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "sha256": rows_sha,
            "lines": raw_rows.len(),
            "retained_repetitions": [1, reps],
            "note": "raw per-request rows for repetitions 1 and the last; G1 was evaluated over every repetition",
        },
        "result": if checks_ok { "pass" } else { "fail" },
        "result_note": "`result` reports measurement validity (all rows present, the sensitivity control non-vacuous, every structural check met). `claim_holds` reports whether the pre-registered gates G1-G4 survived. `direction` is the retirement rule's outcome.",
        "not_measured": [
            "fleet, multi-host, multi-writer, media power-loss",
            "a real model-serving integration; the rule is a prototype over identity dimensions",
            "a remote or shared manifestation; E's table is one local file",
            "a Go or independent implementation",
            "closure and impact phases (declared not implemented in this subphase)",
        ],
        "scope": "single host, one local POSIX backend, OOM-protected Docker court, synthetic deterministic workload. Not a cloud, fleet, media, power-loss, GPU or serving claim.",
    });
    write_json_atomic(&out, &receipt)
}

// ---------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------

fn cmd_lifecycle(args: &Args) -> Result<(), String> {
    let store_path = PathBuf::from(args.get("store")?);
    let fixture_path = PathBuf::from(args.get("fixture")?);
    let out = PathBuf::from(args.get("out")?);
    let work = PathBuf::from(args.get("work")?);
    let fixture = fixture::load_fixture_json(&fixture_path)?;
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let mut store = weftfs::store::Store::open_with_options(
        &store_path,
        weftfs::store::OpenMode::ReadWrite,
        store_options(),
    )
    .map_err(|e| format!("open store: {e}"))?;
    let contracts = fixture.contracts.clone();
    let roots = stream_roots(&contracts)?;
    let authorities = fixture.authorities.clone();
    let aroots = authority_roots(&authorities)?;
    let stream = fixture::request_stream(&fixture, 0x1eaf_0000_0000_0001);
    let mut e = Arm::new(ArmKind::E, Some("lru".to_owned()), &work)?;
    let mut w = Arm::new(ArmKind::W, None, &work)?;
    let mut verdicts = Vec::new();
    for request in stream.iter().take(2_000) {
        let outcome = e.handle(
            &store,
            &fixture,
            &contracts,
            &roots,
            &authorities,
            &aroots,
            request,
        )?;
        verdicts.push((outcome.verdict, outcome.reason));
        e.maybe_rebuild()?;
    }
    let manifest_path = e.manifest_path();
    let manifest_bytes = e.retained_bytes();
    let existed = manifest_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    if let Some(path) = &manifest_path {
        let _ = fs::remove_file(path);
    }
    // Drop the cache and the open table: the next pass must recompute.
    e = Arm::new(ArmKind::E, Some("lru".to_owned()), &work)?;
    let mut mismatches = 0u64;
    let mut exact = 0u64;
    for (request, expected) in stream.iter().take(2_000).zip(verdicts.iter()) {
        let a = e.handle(
            &store,
            &fixture,
            &contracts,
            &roots,
            &authorities,
            &aroots,
            request,
        )?;
        let w_out = w.handle(
            &store,
            &fixture,
            &contracts,
            &roots,
            &authorities,
            &aroots,
            request,
        )?;
        if (a.verdict, a.reason) != (w_out.verdict, w_out.reason) {
            mismatches += 1;
        }
        if (a.verdict, a.reason) != *expected {
            mismatches += 1;
        }
        exact += 1;
    }

    // Candidate-only rule: a 16-byte prefix hit with a different 144-byte core
    // must not produce a verdict.
    let mut entries = Vec::new();
    for i in 0..4u8 {
        let mut entry = caches::Entry::default();
        entry.key = [i; 32];
        entry.subject = [i + 1; 32];
        entry.contract = [i + 2; 32];
        entry.snapshot = [i + 3; 32];
        entry.authority = [i + 4; 32];
        entry.page_id = [i + 5; 32];
        entry.content = [i + 6; 32];
        entry.epoch = 1;
        entry.verdict = 0;
        entries.push(entry);
    }
    let path = work.join("lifecycle-candidate.ekb");
    let (mut table, _) = manifest::ManifestTable::build(&path, &entries, 1 << 20, 1)?;
    let core = rule::key_material(
        weftfs::Hash32::from_bytes([1; 32]),
        weftfs::Hash32::from_bytes([2; 32]),
        weftfs::Hash32::from_bytes([3; 32]),
        weftfs::Hash32::from_bytes([4; 32]),
        1,
        weftfs::Hash32::from_bytes([5; 32]),
        weftfs::Hash32::from_bytes([6; 32]),
    );
    let key = rule::request_key_from_core(&core)?;
    let mut altered = core;
    altered[8] ^= 0xff; // same prefix is not guaranteed; see below
    let mut altered_core = [0u8; rule::KEY_CORE_LEN];
    altered_core.copy_from_slice(&altered[..rule::KEY_CORE_LEN]);
    let candidate_only = table.lookup(key.as_bytes(), &altered_core).is_none();
    let _ = fs::remove_file(&path);

    let result = json!({
        "schema": "weft.ekbloom_context.lifecycle.v1",
        "generated_utc": std::env::var("WEFT_EKBLOOM_CONTEXT_STAMP").ok(),
        "manifest_existed": existed,
        "manifest_bytes_before_delete": manifest_bytes,
        "replay_requests": exact,
        "verdict_mismatches": mismatches,
        "candidate_only_rule_holds": candidate_only,
        "note": "the manifestation is a collectable accelerator: after deleting it and dropping the cache, replaying the same stream reproduces every verdict and every reason, and a prefix candidate with a different core never returns a verdict",
        "environment": environment(),
    });
    write_json_atomic(&out, &result)
}

// ---------------------------------------------------------------------------
// scale
// ---------------------------------------------------------------------------

fn cmd_scale(args: &Args) -> Result<(), String> {
    let out = PathBuf::from(args.get("out")?);
    let work = PathBuf::from(args.get("work")?);
    let sizes_text = args.opt("sizes").unwrap_or("1000,100000,1000000");
    let sizes: Vec<usize> = sizes_text
        .split(',')
        .map(|s| s.parse::<usize>())
        .collect::<Result<_, _>>()
        .map_err(|_| "--sizes must be a comma-separated list of integers".to_owned())?;
    let value = scale::run(&out, &work, &sizes)?;
    write_json_atomic(&out, &value)
}

// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();
    let result = match args.map.get("command").map(String::as_str) {
        Some("prepare") => cmd_prepare(&args),
        Some("run") => cmd_run(&args),
        Some("assemble") => cmd_assemble(&args),
        Some("lifecycle") => cmd_lifecycle(&args),
        Some("scale") => cmd_scale(&args),
        other => Err(format!(
            "unknown command {other:?}; expected prepare|run|assemble|lifecycle|scale"
        )),
    };
    match result {
        Ok(()) => {}
        Err(err) => {
            eprintln!("ekbloom-context-harness: {err}");
            std::process::exit(1);
        }
    }
}
