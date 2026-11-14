//! KV page batching and verified-session grant cache court (subphase T065).
//!
//! Measures four things on one synthetic page stream:
//!
//! 1. **Publication batching.** The same 1,024 prefixes written one commit each
//!    versus 256 per commit: wall time, commit count and bytes written.
//! 2. **Page reads.** One 128-page prefix read in order under a warm grant,
//!    with the grant cache on (default) and off (the control), reporting
//!    p50/p95/p99 and the cache counters.
//! 3. **Descriptor accounting.** The frozen per-page mode-12 frame size and the
//!    canonical `weft.kv.capability.v1` descriptor size, so the per-page
//!    governance overhead is visible rather than asserted.
//! 4. **Invalidation cost.** A revocation drops the cached grant: the first read
//!    afterwards is a miss, and the miss cost is reported against a hit.
//!
//! Non-claims: no engine, no network, one host, synthetic pages, warm page
//! cache. A denial is still non-disclosing; the timing side channel is the
//! separate T067 court.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{public_key_for_seed, sign_envelope, CapabilityEnvelope};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};

use weftfs::identity::Hash32;
use weftfs::kv_prefix::{kv_page_bytes, kv_token_ids, KvPrefixBatchItem, KvPrefixSpec};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const SEED: [u8; 32] = [7u8; 32];
const PREFIXES: u64 = 1_024;
const BATCH: usize = 256;
const PAGES: u64 = 128;
const PAGE_ROWS: u64 = 8;

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
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

fn options(cache: bool) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        kv_grant_cache: cache,
        ..Options::default()
    }
}

fn spec() -> KvPrefixSpec {
    KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: "m".to_owned(),
        tokenizer_hash: Hash32::from_bytes([1; 32]),
        attention_hash: Hash32::from_bytes([2; 32]),
        dtype: "bf16".to_owned(),
        head_count: 2,
        head_dim: 4,
        layer_count: 1,
        page_rows: PAGE_ROWS,
        pinned: false,
    }
}

fn prefix_pages(index: u64) -> (Vec<u64>, Vec<Vec<Vec<u8>>>) {
    let tokens = kv_token_ids(0x1000 + index, PAGE_ROWS * 2);
    let pages = vec![vec![
        kv_page_bytes(0x2000 + index, 0, 0, (PAGE_ROWS * 2 * 4 * 2) as usize),
        kv_page_bytes(0x2000 + index, 0, 1, (PAGE_ROWS * 2 * 4 * 2) as usize),
    ]];
    (tokens, pages)
}

/// Sorted record-id digest: the identity every prefix in the store has.
fn record_set_digest(store: &Store) -> Hash32 {
    let mut ids: Vec<Hash32> = store
        .kv_prefix_records_with_ids()
        .unwrap()
        .into_iter()
        .map(|(_, _, record)| record.id().unwrap())
        .collect();
    ids.sort();
    let mut bytes = Vec::with_capacity(ids.len() * 32);
    for id in ids {
        bytes.extend_from_slice(id.as_bytes());
    }
    weftfs::identity::h("weft/kv/bench-record-set/v1", &bytes)
}

fn write_single(dir: &Path, count: u64) -> (f64, Vec<u64>, u64, u64, Hash32, Hash32) {
    let mut store = Store::init(dir, options(true)).unwrap();
    let spec = spec();
    let before = store.generation();
    let mut samples = Vec::with_capacity(count as usize);
    let started = Instant::now();
    for index in 0..count {
        let (tokens, pages) = prefix_pages(index);
        let name = format!("p-{index:05}");
        let call = Instant::now();
        store
            .kv_prefix_put_seeded(&name, &spec, &tokens, None, &pages, 0, 0, &[0x5A; 32])
            .unwrap();
        samples.push(u64::try_from(call.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    let commits = store.generation() - before;
    let head = store.head();
    (wall, samples, commits, dir_size(dir), head, record_set_digest(&store))
}

fn write_batched(dir: &Path, count: u64) -> (f64, Vec<u64>, u64, u64, Hash32, Hash32) {
    let mut store = Store::init(dir, options(true)).unwrap();
    let spec = spec();
    let before = store.generation();
    let mut samples = Vec::with_capacity((count / BATCH as u64) as usize);
    let started = Instant::now();
    let mut next = 0u64;
    while next < count {
        let end = (next + BATCH as u64).min(count);
        let tokens: Vec<Vec<u64>> = (next..end).map(|index| prefix_pages(index).0).collect();
        let payloads: Vec<Vec<Vec<Vec<u8>>>> =
            (next..end).map(|index| prefix_pages(index).1).collect();
        let names: Vec<String> = (next..end).map(|index| format!("p-{index:05}")).collect();
        let items: Vec<KvPrefixBatchItem<'_>> = (0..(end - next) as usize)
            .map(|offset| KvPrefixBatchItem {
                name: &names[offset],
                spec: &spec,
                tokens: &tokens[offset],
                parent_name: None,
                new_page_bytes: &payloads[offset],
            })
            .collect();
        let call = Instant::now();
        store
            .kv_prefix_put_batch_seeded(&items, 0, 0, &[0x5A; 32])
            .unwrap();
        samples.push(u64::try_from(call.elapsed().as_nanos()).unwrap_or(u64::MAX));
        next = end;
    }
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    let commits = store.generation() - before;
    let head = store.head();
    (wall, samples, commits, dir_size(dir), head, record_set_digest(&store))
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

struct PageStream {
    store: Store,
    objects: Vec<Hash32>,
    principal: Principal,
    page_bytes: usize,
    descriptor_bytes: usize,
}

fn page_stream(dir: &Path, cache: bool) -> PageStream {
    let mut store = Store::init(dir, options(cache)).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut objects = Vec::new();
    let mut descriptor_ids = Vec::new();
    for index in 0..PAGES {
        let input = KvCacheInput {
            model: "qwen2.5-0.5b@060db649".to_owned(),
            layer: 0,
            session: "stream".to_owned(),
            page_index: index,
            page_count: PAGES,
            seq_start: index * PAGE_ROWS,
            seq_end: (index + 1) * PAGE_ROWS,
            dtype: KvDtype::Bf16,
            head_count: 4,
            head_dim: 64,
        };
        let bytes = kv_page_bytes(0x3000 + index, 0, index, (PAGE_ROWS * 4 * 64 * 2) as usize);
        let written = store
            .write_kv_page(
                root,
                format!("page-{index:04}.kv").as_bytes(),
                TENANT,
                &input,
                &bytes,
                0,
                0,
            )
            .unwrap();
        objects.push(written.object_id);
        descriptor_ids.push(written.descriptor_id);
        let _ = bytes;
    }
    descriptor_ids.sort();
    descriptor_ids.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        TENANT,
        descriptor_ids,
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes([0xA1; 32]),
        NOW_NS,
        Hash32::from_bytes([0x31; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    let verified = store.load_capability_envelope(id, &[], NOW_NS).unwrap();
    let descriptor = store.kv_descriptor(objects[0]).unwrap();
    let descriptor_bytes = descriptor.canonical_bytes().unwrap().len();
    PageStream {
        store,
        objects,
        principal: Principal::new(1001, 1001, Vec::new()).with_envelope(verified),
        page_bytes: (PAGE_ROWS * 4 * 64 * 2) as usize,
        descriptor_bytes,
    }
}

fn read_stream(stream: &PageStream) -> (Vec<u64>, u64) {
    let mut samples = Vec::with_capacity(stream.objects.len());
    let mut bytes = 0u64;
    for object in &stream.objects {
        let started = Instant::now();
        let decoded = stream
            .store
            .read_kv_page(*object, &stream.principal, NOW_NS)
            .unwrap();
        samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        bytes = bytes.saturating_add(decoded.bytes.len() as u64);
    }
    (samples, bytes)
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
    let root = value("--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("kv-grants-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("work dir");
    let started = Instant::now();

    let single = write_single(&root.join("single"), PREFIXES);
    let batched = write_batched(&root.join("batched"), PREFIXES);

    let mut stream_cached = page_stream(&root.join("stream-cached"), true);
    let stream_control = page_stream(&root.join("stream-control"), false);
    // Warm one pass so the descriptor cache is populated; then measure.
    let _ = read_stream(&stream_cached);
    let (cached_samples, cached_bytes) = read_stream(&stream_cached);
    let (control_samples, control_bytes) = read_stream(&stream_control);
    let (hits, misses, invalidations_before) = stream_cached.store.kv_grant_cache_counters();

    // A hit is one measured read with no state change in between.
    let hit_started = Instant::now();
    let _ = stream_cached
        .store
        .read_kv_page(stream_cached.objects[0], &stream_cached.principal, NOW_NS);
    let hit_ns = u64::try_from(hit_started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    // Invalidation cost: any commit bumps the generation, so the very next read
    // on this handle is a guaranteed grant-cache miss. That is the boundary the
    // cache obeys: it is keyed by the head, so it can never outlive a commit.
    let spec = spec();
    let (tokens, pages) = prefix_pages(0);
    stream_cached
        .store
        .kv_prefix_put_seeded("invalidate", &spec, &tokens, None, &pages, 0, 0, &[0x5A; 32])
        .unwrap();
    let miss_started = Instant::now();
    let _ = stream_cached
        .store
        .read_kv_page(stream_cached.objects[0], &stream_cached.principal, NOW_NS);
    let miss_ns = u64::try_from(miss_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let (hits_after, misses_after, invalidations_after) =
        stream_cached.store.kv_grant_cache_counters();

    let receipt = json!({
        "schema": "weft.kv.grants.bench.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unknown".to_owned()),
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "label": "KV page batching and verified-session grant cache; no engine, one host, synthetic pages",
        "fixture": {
            "prefixes": PREFIXES,
            "batch": BATCH,
            "pages_per_prefix": 2,
            "page_rows": PAGE_ROWS,
            "stream_pages": PAGES,
            "page_bytes": stream_cached.page_bytes,
            "descriptor_bytes": stream_cached.descriptor_bytes,
            "descriptor_per_page_percent": round(
                stream_cached.descriptor_bytes as f64 * 100.0
                    / (stream_cached.page_bytes + stream_cached.descriptor_bytes) as f64
            ),
        },
        "publication": {
            "single": {
                "wall_ms": round(single.0),
                "commits": single.2,
                "per_prefix_ms": distribution(&single.1),
                "store_bytes": single.3,
                "head": single.4.to_hex(),
                "record_set": single.5.to_hex(),
            },
            "batched": {
                "wall_ms": round(batched.0),
                "commits": batched.2,
                "per_batch_ms": distribution(&batched.1),
                "store_bytes": batched.3,
                "head": batched.4.to_hex(),
                "record_set": batched.5.to_hex(),
            },
            "same_records": single.5 == batched.5,
            "heads_differ_by_construction": single.4 != batched.4,
            "speedup": round(single.0 / batched.0),
        },
        "page_reads": {
            "grant_cache_on": {
                "per_page": distribution(&cached_samples),
                "bytes": cached_bytes,
                "hits": hits,
                "misses": misses,
            },
            "grant_cache_off_control": {
                "per_page": distribution(&control_samples),
                "bytes": control_bytes,
            },
            "bytes_identical": cached_bytes == control_bytes,
            "hit_ns": hit_ns,
        },
        "invalidation": {
            "hit_ns": hit_ns,
            "miss_after_commit_ns": miss_ns,
            "hit_over_miss": round(hit_ns as f64 / miss_ns.max(1) as f64),
            "misses_before": misses,
            "misses_after": misses_after,
            "invalidations_after": invalidations_after,
            "hits_before": hits,
            "hits_after": hits_after,
            "initial_invalidations": invalidations_before,
        },
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "non_claims": [
            "no inference engine ran",
            "one host, warm page cache, synthetic pages; no fleet or production claim",
            "the descriptor is a fixed per-page governance cost; batching publication does not remove it",
        ],
        "wall_ms": round(started.elapsed().as_secs_f64() * 1000.0),
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
    let _ = std::fs::remove_dir_all(&root);
}
