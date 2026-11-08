//! KV prefix-cache court (subphase T2).
//!
//! Measures the storage-side primitive, not an inference engine. The workload
//! is synthetic but every shape is declared in the receipt:
//!
//! * model identity `synthetic-0.5b`, BF16, 14 layers, 2 KV heads, head dim 64
//!   (Qwen2.5-0.5B's GQA geometry), 64 tokens per page;
//! * per token: 14 * 2 * 64 * 2 B = 3,584 B;
//! * per page (all layers): 64 * 3,584 = 229,376 B.
//!
//! Rows:
//!
//! * `one-prefix` — 512 tokens, 8 pages, logical and stored bytes;
//! * `parent-child` — a 1,024-token prefix whose parent is the 512-token one,
//!   showing shared page objects and bytes stored;
//! * `system-prompt-pool` — 100 prefixes that share one 512-token system
//!   prefix, dedup ratio, page objects on disk;
//! * `get` — authorized get latency (capability verification + decode) for a
//!   sample of prefixes;
//! * `lookup` — compatibility scan latency over the pool;
//! * `decision` — reuse-vs-recompute distribution over declared request shapes.
//!
//! Non-claims are in `docs/KV-PREFIX.md` and repeated in the receipt: no vLLM
//! or SGLang integration, no time-to-first-token claim, one host, synthetic
//! pages, warm cache.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};

use weftfs::identity::Hash32;
use weftfs::kv_prefix::{
    kv_attention_hash, kv_page_bytes, kv_page_len, kv_token_ids, kv_tokenizer_hash, KvLookup,
    KvPrefillModel, KvPrefixRequest, KvPrefixSpec,
};
use weftfs::store::{Options, Store};
use weftfs::Principal;

const MODEL: &str = "synthetic-0.5b";
const TENANT: &str = "tenant-a";
const LAYERS: u32 = 14;
const HEADS: u32 = 2;
const HEAD_DIM: u32 = 64;
const PAGE_ROWS: u64 = 64;
const WORD_BYTES: u64 = 2;
const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const GRANTOR_SEED: [u8; 32] = [7u8; 32];

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
        "max_us": round(sorted[sorted.len() - 1] as f64 / 1000.0),
        "mean_us": round(sum as f64 / sorted.len() as f64 / 1000.0),
        "p50_ns": pick(50),
        "p95_ns": pick(95),
    })
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().trim_end_matches(" kB").parse::<u64>().ok();
        }
    }
    None
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn spec(pinned: bool) -> KvPrefixSpec {
    KvPrefixSpec {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: kv_tokenizer_hash("declared-tokenizer-v1"),
        attention_hash: kv_attention_hash("mha;heads=2;head_dim=64;layers=14"),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: HEAD_DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        pinned,
    }
}

fn request(tokens: &[u64]) -> KvPrefixRequest {
    KvPrefixRequest {
        tenant: TENANT.to_owned(),
        model: MODEL.to_owned(),
        tokenizer_hash: kv_tokenizer_hash("declared-tokenizer-v1"),
        attention_hash: kv_attention_hash("mha;heads=2;head_dim=64;layers=14"),
        dtype: "bf16".to_owned(),
        head_count: HEADS,
        head_dim: HEAD_DIM,
        layer_count: LAYERS,
        page_rows: PAGE_ROWS,
        tokens: tokens.to_vec(),
    }
}

fn page_payloads(tokens: u64, from_page: u64, token_seed: u64) -> Vec<Vec<Vec<u8>>> {
    let page_count = tokens.div_ceil(PAGE_ROWS);
    let mut out = Vec::new();
    for layer in 0..LAYERS {
        let mut pages = Vec::new();
        for index in from_page..page_count {
            let len =
                kv_page_len(index, PAGE_ROWS, tokens, HEADS, HEAD_DIM, "bf16").unwrap() as usize;
            pages.push(kv_page_bytes(token_seed, layer, index, len));
        }
        out.push(pages);
    }
    out
}

fn put(
    store: &mut Store,
    name: &str,
    tokens: &[u64],
    parent: Option<&str>,
    token_seed: u64,   // token sequence identity
    payload_seed: u64, // page bytes
    pinned: bool,
) -> weftfs::kv_prefix::KvPrefixWrite {
    let from_page = match parent {
        Some(parent_name) => store.kv_prefix(parent_name).unwrap().page_count,
        None => 0,
    };
    store
        .kv_prefix_put_seeded(
            name,
            &spec(pinned),
            tokens,
            parent,
            &page_payloads(tokens.len() as u64, from_page, payload_seed),
            0,
            0,
            &{
                let mut seed = [0u8; 32];
                seed[..8].copy_from_slice(&token_seed.to_le_bytes());
                seed[8..16].copy_from_slice(&payload_seed.to_le_bytes());
                seed
            },
        )
        .unwrap()
}

fn page_objects(store: &Store) -> (u64, u64) {
    let Ok(pages) = store.resolve_path("/kv/pages") else {
        return (0, 0);
    };
    let mut count = 0u64;
    let mut bytes = 0u64;
    for entry in store.list_dir(pages).unwrap() {
        count += 1;
        if let Ok(meta) = store.object_meta(entry.target) {
            bytes = bytes.saturating_add(meta.size);
        }
    }
    (count, bytes)
}

fn grant_for(store: &mut Store, record: &weftfs::kv_prefix::KvPrefixRecord) -> VerifiedEnvelope {
    let mut roots: Vec<Hash32> = record
        .pages
        .iter()
        .flatten()
        .map(|id| store.kv_descriptor(*id).unwrap().id().unwrap())
        .collect();
    roots.sort();
    roots.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        Hash32::from_bytes([9u8; 32]),
        TENANT,
        roots,
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes([0x31; 32]),
        NOW_NS,
        Hash32::from_bytes([0xA1; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    store.load_capability_envelope(id, &[], NOW_NS).unwrap()
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
        let base = if Path::new("/mnt/weft-bench").is_dir() {
            PathBuf::from("/mnt/weft-bench")
        } else {
            std::env::temp_dir()
        };
        base.join(format!("kv-prefix-bench-{}", std::process::id()))
    });
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create bench dir");

    let per_token_bytes = u64::from(LAYERS) * u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES;
    let per_page_bytes = PAGE_ROWS * per_token_bytes;
    let mut row_sizes = Vec::new();

    let started = Instant::now();
    let mut store = Store::init(&dir, options()).expect("init store");

    // --- one prefix -------------------------------------------------------
    let system_tokens = kv_token_ids(1, 512);
    let one_started = Instant::now();
    let one = put(&mut store, "system", &system_tokens, None, 1, 1, false);
    let one_put_ms = round(one_started.elapsed().as_secs_f64() * 1000.0);
    let (objects_after_one, bytes_after_one) = page_objects(&store);
    row_sizes.push(json!({
        "row": "one-prefix",
        "tokens": one.record.position_end,
        "pages_per_layer": one.record.page_count,
        "layers": one.record.layer_count,
        "logical_page_bytes": one.record.page_bytes,
        "logical_bytes_per_token": per_token_bytes,
        "logical_bytes_per_page": per_page_bytes,
        "stored_page_bytes": one.record.stored_page_bytes,
        "page_objects": objects_after_one,
        "page_object_bytes": bytes_after_one,
        "put_wall_ms": one_put_ms,
        "prefix": one.prefix_id.to_hex(),
    }));

    // --- parent + child ---------------------------------------------------
    let child_tokens = kv_token_ids(1, 1024);
    let child_started = Instant::now();
    let child = put(
        &mut store,
        "system+user",
        &child_tokens,
        Some("system"),
        1,
        2,
        false,
    );
    let child_put_ms = round(child_started.elapsed().as_secs_f64() * 1000.0);
    let (objects_after_child, bytes_after_child) = page_objects(&store);
    row_sizes.push(json!({
        "row": "parent-child",
        "tokens": child.record.position_end,
        "pages_per_layer": child.record.page_count,
        "logical_page_bytes": child.record.page_bytes,
        "stored_page_bytes": child.record.stored_page_bytes,
        "reused_pages": child.record.reused_pages,
        "new_pages": child.new_pages,
        "page_objects": objects_after_child,
        "page_object_bytes": bytes_after_child,
        "delta_page_objects": objects_after_child - objects_after_one,
        "put_wall_ms": child_put_ms,
        "prefix": child.prefix_id.to_hex(),
        "parent": child.record.parent.map(|id| id.to_hex()),
    }));

    // --- 100 prefixes over one system prompt ------------------------------
    // Each prefix shares the 512-token system prefix and adds 1..=8 pages
    // (64..512 tokens) of its own. The token sequence after the system prefix
    // is distinct per prefix.
    let pool_size = 100u64;
    let pool_started = Instant::now();
    let mut pool = Vec::new();
    for index in 0..pool_size {
        let extra_pages = (index % 8) + 1;
        let extra_tokens = extra_pages * 64;
        let total = 512 + extra_tokens;
        let name = format!("pool-{index:03}");
        // Same first 512 token ids as `system` (same seed), then a distinct
        // continuation stream.
        let mut tokens = kv_token_ids(1, 512);
        tokens.extend(kv_token_ids(1000 + index, extra_tokens));
        put(
            &mut store,
            &name,
            &tokens,
            Some("system"),
            index + 100,
            index + 100,
            false,
        );
        pool.push((name, total));
    }
    let pool_wall_ms = round(pool_started.elapsed().as_secs_f64() * 1000.0);
    let (pool_objects, pool_object_bytes) = page_objects(&store);
    let mut logical_total = 0u64;
    let mut stored_total = 0u64;
    for (name, _) in &pool {
        let record = store.kv_prefix(name).unwrap();
        logical_total = logical_total.saturating_add(record.page_bytes);
        stored_total = stored_total.saturating_add(record.stored_page_bytes);
    }
    let system_bytes = one.record.page_bytes;
    let pool_logical = logical_total + system_bytes;
    let pool_stored = stored_total + one.record.stored_page_bytes;
    row_sizes.push(json!({
        "row": "system-prompt-pool",
        "prefixes": pool_size,
        "system_prompt_tokens": 512,
        "system_prompt_page_bytes": system_bytes,
        "continuation_tokens_min": 64,
        "continuation_tokens_max": 512,
        "logical_bytes_all_prefixes": pool_logical,
        "stored_page_bytes_all_prefixes": pool_stored,
        "dedup_ratio": round(1.0 - pool_stored as f64 / pool_logical as f64),
        "page_objects_on_disk": pool_objects,
        "page_object_bytes_on_disk": pool_object_bytes,
        "pool_wall_ms": pool_wall_ms,
    }));

    // --- lookup latency ---------------------------------------------------
    let lookup_request = request(&system_tokens[..512]);
    let mut lookup_samples = Vec::new();
    for _ in 0..50 {
        let started = Instant::now();
        let found = store.kv_prefix_lookup(&lookup_request).unwrap();
        lookup_samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        assert!(matches!(found, KvLookup::Reusable { .. }));
    }

    // --- get latency ------------------------------------------------------
    // One grant per prefix keeps every envelope well inside its list bound.
    let mut get_samples = Vec::new();
    let mut page_bytes_per_get = 0u64;
    let mut probe = Vec::new();
    for (index, (name, _)) in pool.iter().enumerate() {
        if index % 10 != 0 {
            continue;
        }
        let record = store.kv_prefix(name).unwrap();
        let verified = grant_for(&mut store, &record);
        let principal = Principal::new(1001, 1001, Vec::new()).with_envelope(verified);
        let mut pages = 0usize;
        for _ in 0..10 {
            let started = Instant::now();
            let read = store.kv_prefix_get(name, &principal, NOW_NS).unwrap();
            get_samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
            page_bytes_per_get = read.record.page_bytes;
            pages = read.pages.len();
        }
        probe.push(json!({
            "prefix": name,
            "tokens": record.position_end,
            "pages": pages,
            "page_bytes": record.page_bytes,
        }));
    }

    // --- decision distribution -------------------------------------------
    let declared = KvPrefillModel::declared();
    let mut decisions = Vec::new();
    let mut reuse = 0u64;
    let mut recompute = 0u64;
    let mut reasons: std::collections::BTreeMap<String, u64> = Default::default();

    // Compatible shapes: the stored system prompt at several request lengths.
    for tokens in [512u64, 1024, 4096, 8192, 32_768, 131_072] {
        let mut ids = kv_token_ids(1, 512);
        if tokens > 512 {
            ids.extend(kv_token_ids(7, tokens - 512));
        }
        push_decision(
            &store,
            &declared,
            &format!("compatible-{tokens}"),
            request(&ids),
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
    }
    // Different model.
    for tokens in [512u64, 4096, 32_768] {
        let mut ids = kv_token_ids(1, 512);
        if tokens > 512 {
            ids.extend(kv_token_ids(7, tokens - 512));
        }
        let mut req = request(&ids);
        req.model = "other-model".to_owned();
        push_decision(
            &store,
            &declared,
            &format!("model-mismatch-{tokens}"),
            req,
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
    }
    // Different tokenizer and attention configuration.
    {
        let mut req = request(&system_tokens);
        req.tokenizer_hash = kv_tokenizer_hash("other-tokenizer");
        push_decision(
            &store,
            &declared,
            "tokenizer-mismatch",
            req,
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
        let mut req = request(&system_tokens);
        req.attention_hash = kv_attention_hash("gqa;heads=14;head_dim=64;layers=24");
        push_decision(
            &store,
            &declared,
            "attention-mismatch",
            req,
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
    }
    // Different token sequence, long enough to cover every stored prefix.
    {
        let other = kv_token_ids(4242, 2048);
        push_decision(
            &store,
            &declared,
            "token-chain-mismatch",
            request(&other),
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
    }
    // Shorter request than every stored prefix.
    {
        push_decision(
            &store,
            &declared,
            "shorter-than-prefix",
            request(&system_tokens[..64]),
            &mut decisions,
            &mut reasons,
            &mut reuse,
            &mut recompute,
        );
    }

    let receipt = json!({
        "schema": "weft.kv.prefix.bench.v1",
        "label": "storage-side KV prefix primitive; no inference engine in the court",
        "shapes": {
            "model": MODEL,
            "dtype": "bf16",
            "layers": LAYERS,
            "kv_heads": HEADS,
            "head_dim": HEAD_DIM,
            "page_rows": PAGE_ROWS,
            "bytes_per_token": per_token_bytes,
            "bytes_per_page_all_layers": per_page_bytes,
            "note": "Qwen2.5-0.5B GQA geometry, synthetic page bytes",
        },
        "store_dir": dir.display().to_string(),
        "generation": store.generation(),
        "sizes": row_sizes,
        "lookup": distribution(&lookup_samples),
        "get": {
            "distribution": distribution(&get_samples),
            "page_bytes_per_get": page_bytes_per_get,
            "probes": probe,
        },
        "decision": {
            "reuse": reuse,
            "recompute": recompute,
            "categories": reasons,
            "rows": decisions,
            "model_label": declared.label,
            "coefficients": {
                "defaults": {
                    "prefill_fixed_ns": declared.defaults.prefill_fixed_ns,
                    "prefill_ns_per_token": declared.defaults.prefill_ns_per_token,
                },
                "transfer_ns_per_mib": declared.transfer_ns_per_mib,
                "placement_fixed_ns": declared.placement_fixed_ns,
                "lookup_fixed_ns": declared.lookup_fixed_ns,
                "decode_cost_model": declared.decode_cost_model.as_str(),
            },
        },
        "wall_ms": round(started.elapsed().as_secs_f64() * 1000.0),
        "peak_rss_kib": peak_rss_kib(),
        "verify_full": {
            "run": false,
            "note": "the separate verification gate runs `weftfs verify --full` on this store",
        },
        "non_claims": [
            "no vLLM or SGLang integration; no inference engine ran in this court",
            "no time-to-first-token or wall-clock serving claim",
            "synthetic page bytes and declared prefill coefficients",
            "one host, one court, warm cache",
            "get latency is storage + capability verification + decode, not engine placement",
        ],
        "validation_experiment": "Load the same prefix objects from a real engine's KV cache (vLLM or SGLang) with an identical tokenizer and attention configuration, place them, and compare TTFT against a recompute of the same prompt; this court does not do that.",
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[allow(clippy::too_many_arguments)]
fn push_decision(
    store: &Store,
    model: &KvPrefillModel,
    label: &str,
    request: KvPrefixRequest,
    decisions: &mut Vec<Json>,
    reasons: &mut std::collections::BTreeMap<String, u64>,
    reuse: &mut u64,
    recompute: &mut u64,
) {
    let decision = store.kv_decide_reuse(&request, model).unwrap();
    if decision.reuse {
        *reuse += 1;
    } else {
        *recompute += 1;
    }
    let category = decision_category(&decision);
    *reasons.entry(category).or_insert(0) += 1;
    decisions.push(json!({
        "label": label,
        "reuse": decision.reuse,
        "tokens": decision.tokens,
        "page_bytes": decision.page_bytes,
        "t_reuse_ns": decision.reuse_total_ns,
        "t_recompute_ns": decision.recompute_ns,
        "lookup_ns": decision.lookup_ns,
        "observed_lookup_ns": decision.observed_lookup_ns,
        "transfer_ns": decision.transfer_ns,
        "decode_ns": decision.decode_ns,
        "placement_ns": decision.placement_ns,
        "reason": decision.reason,
    }));
}

fn decision_category(decision: &weftfs::kv_prefix::KvReuseDecision) -> String {
    if decision.reuse {
        return "reuse".to_owned();
    }
    if decision.reason.contains("no compatible prefix") {
        for key in [
            "tenant mismatch",
            "model mismatch",
            "tokenizer hash mismatch",
            "attention configuration hash mismatch",
            "dtype mismatch",
            "head count mismatch",
            "head dim mismatch",
            "layer count mismatch",
            "page rows mismatch",
            "token-chain mismatch",
            "longer than the request",
        ] {
            if decision.reason.contains(key) {
                return key.replace(' ', "-");
            }
        }
        return "no-compatible-prefix".to_owned();
    }
    "recompute-cheaper".to_owned()
}
