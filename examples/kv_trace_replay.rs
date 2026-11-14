//! Engine-shaped request-trace replay over real KV prefix records (T064/T069).
//!
//! **This is trace replay, not an inference engine.** No engine could be
//! installed in the court (see `bench/receipts/kv-engine-*.log`): vLLM and
//! SGLang both require a CUDA torch build that does not fit the 1 GiB `/tmp`
//! tmpfs and there is no `/dev/nvidia*`. What this harness does instead:
//!
//! * builds a real store of shared prefixes with real mode-12 page objects;
//! * replays a deterministic, engine-shaped request trace (system-prompt
//!   sharing, session extension, one-token drift, model drift);
//! * measures the **reuse** side for real: indexed lookup, page transfer and
//!   decode, and the local token-hash chain over each request (a measured
//!   lower bound on any prefill work needed to identify a prefix);
//! * computes the reuse decision under the declared prefill model and reports
//!   the **regret** of three policies against an oracle, as a function of a
//!   hypothetical per-token prefill cost. Every recompute number is
//!   hypothetical and the receipt says so; only the reuse and hashing numbers
//!   are measured.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftfs::identity::Hash32;
use weftfs::kv_prefix::{
    kv_page_bytes, kv_token_ids, KvLookup, KvPrefixBatchItem, KvPrefixRequest, KvPrefixSpec,
};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Principal;

const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const TENANT: &str = "tenant-a";
const MODEL: &str = "synthetic-0.5b";
const LAYERS: u32 = 2;
const HEADS: u32 = 2;
const HEAD_DIM: u32 = 32;
const PAGE_ROWS: u64 = 64;
const WORD_BYTES: usize = 2;
const GRANTOR_SEED: [u8; 32] = [7u8; 32];
const SHARED_PREFIXES: u64 = 64;
const REQUESTS: usize = 2_000;

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
    let sum: u128 = sorted.iter().map(|value| u128::from(*value)).sum();
    json!({
        "samples": sorted.len(),
        "p50_us": round(pick(50) as f64 / 1000.0),
        "p95_us": round(pick(95) as f64 / 1000.0),
        "p99_us": round(pick(99) as f64 / 1000.0),
        "mean_us": round(sum as f64 / sorted.len() as f64 / 1000.0),
    })
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
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

fn request(tokens: &[u64], model: &str) -> KvPrefixRequest {
    KvPrefixRequest {
        tenant: TENANT.to_owned(),
        model: model.to_owned(),
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

fn payload(tokens: u64, from_page: u64, seed: u64) -> Vec<Vec<Vec<u8>>> {
    let page_count = tokens.div_ceil(PAGE_ROWS);
    let mut layers = Vec::new();
    for layer in 0..LAYERS {
        let mut pages = Vec::new();
        for index in from_page..page_count {
            let start = index * PAGE_ROWS;
            let end = tokens.min(start + PAGE_ROWS);
            let len =
                ((end - start) * u64::from(HEADS) * u64::from(HEAD_DIM)) as usize * WORD_BYTES;
            pages.push(kv_page_bytes(seed, layer, index, len));
        }
        layers.push(pages);
    }
    layers
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exact,
    Extend,
    Drift,
    ModelDrift,
}

fn lookup_indexed_sorted_p50(samples: &[u64]) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn ret_p50(samples: &[u64]) -> u64 { lookup_indexed_sorted_p50(samples) }

fn median_tokens(decisions: &[(Kind, bool, u64, u64)]) -> u64 {
    let mut tokens: Vec<u64> = decisions.iter().map(|entry| entry.2).collect();
    tokens.sort_unstable();
    tokens.get(tokens.len() / 2).copied().unwrap_or(1)
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
    let dir = value("--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp").join(format!("kv-trace-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work dir");
    let started = Instant::now();

    // --- fixture: 64 shared system prompts, one store ----------------------
    let mut store = Store::init(&dir, options()).expect("init store");
    let spec = spec();
    let mut shared: Vec<Vec<u64>> = Vec::new();
    let mut writes = Vec::new();
    let mut build_started = Instant::now();
    {
        let mut tokens = Vec::new();
        let mut payloads = Vec::new();
        let mut names = Vec::new();
        for index in 0..SHARED_PREFIXES {
            let ids = kv_token_ids(0x900 + 2 * index, 512);
            tokens.push(ids.clone());
            payloads.push(payload(512, 0, 0x400 + index));
            names.push(format!("sys-{index:03}"));
            shared.push(ids);
        }
        let items: Vec<KvPrefixBatchItem<'_>> = (0..SHARED_PREFIXES as usize)
            .map(|index| KvPrefixBatchItem {
                name: &names[index],
                spec: &spec,
                tokens: &tokens[index],
                parent_name: None,
                new_page_bytes: &payloads[index],
            })
            .collect();
        writes = store
            .kv_prefix_put_batch_seeded(&items, 0, 0, &[0x5A; 32])
            .unwrap();
    }
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
    // One grant per prefix keeps the envelope's root list small. Grants are
    // commits, so they are published *before* the index is built: a grant
    // publication bumps the generation and therefore (correctly) invalidates a
    // previously built index.
    let mut principals = Vec::new();
    for write in &writes {
        let verified = grant_for(&mut store, &write.record);
        principals.push(Principal::new(1001, 1001, Vec::new()).with_envelope(verified));
    }
    let index_build = store.kv_index_build().unwrap();

    // --- the trace ---------------------------------------------------------
    // 70% exact reuse, 15% extension, 10% one-token drift, 5% model drift.
    let mut trace: Vec<(Kind, usize, Vec<u64>)> = Vec::with_capacity(REQUESTS);
    for index in 0..REQUESTS {
        let slot = index % SHARED_PREFIXES as usize;
        let base = &shared[slot];
        let roll = index % 20;
        let kind = if roll < 14 {
            Kind::Exact
        } else if roll < 17 {
            Kind::Extend
        } else if roll < 19 {
            Kind::Drift
        } else {
            Kind::ModelDrift
        };
        let tokens = match kind {
            Kind::Exact => base.clone(),
            Kind::Extend => {
                let mut extended = base.clone();
                extended.extend(kv_token_ids(0x5000 + index as u64, 256));
                extended
            }
            Kind::Drift => {
                let mut drifted = base.clone();
                let last = drifted.len() - 1;
                drifted[last] ^= 1;
                drifted
            }
            Kind::ModelDrift => base.clone(),
        };
        trace.push((kind, slot, tokens));
    }

    // --- replay: measure the reuse path and the local hash work ----------
    let mut lookup_indexed = Vec::new();
    let mut lookup_scan = Vec::new();
    let mut get_us = Vec::new();
    let mut hash_us = Vec::new();
    let mut reuse_bytes = 0u64;
    let mut decisions: Vec<(Kind, bool, u64, u64)> = Vec::new(); // kind, reused, tokens, reuse_ns
    let mut counts = [0u64; 4];
    let mut indexed_mismatches = 0u64;
    for (kind, slot, tokens) in &trace {
        let model = if *kind == Kind::ModelDrift {
            "other-model"
        } else {
            MODEL
        };
        let req = request(tokens, model);
        let started = Instant::now();
        let indexed = store.kv_prefix_lookup_indexed(&req).unwrap();
        lookup_indexed.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        if *kind == Kind::Exact {
            let started = Instant::now();
            let scan = store.kv_prefix_lookup_scan(&req).unwrap();
            lookup_scan.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
            let same = match (&indexed, &scan) {
                (KvLookup::Reusable { record: a, .. }, KvLookup::Reusable { record: b, .. }) => {
                    a.id().unwrap() == b.id().unwrap()
                }
                (KvLookup::Recompute { .. }, KvLookup::Recompute { .. }) => true,
                _ => false,
            };
            if !same {
                indexed_mismatches += 1;
            }
        }
        // Measured local hashing of the request: the frozen chain, which any
        // implementation must compute to identify the prefix.
        let started = Instant::now();
        let _ = weftfs::kv_prefix::kv_token_chain_root(tokens);
        hash_us.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));

        let mut reused = false;
        let mut reuse_ns = 0u64;
        if let KvLookup::Reusable { .. } = indexed {
            let started = Instant::now();
            let read = store
                .kv_prefix_get(&format!("sys-{slot:03}"), &principals[*slot], NOW_NS)
                .unwrap();
            reuse_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            get_us.push(reuse_ns);
            reuse_bytes = reuse_bytes.saturating_add(read.record.page_bytes);
            reused = true;
        }
        counts[match kind {
            Kind::Exact => 0,
            Kind::Extend => 1,
            Kind::Drift => 2,
            Kind::ModelDrift => 3,
        }] += 1;
        decisions.push((*kind, reused, tokens.len() as u64, reuse_ns));
    }

    // --- decisions under declared and hypothetical per-token costs --------
    // The declared model (docs/KV-PREFIX.md): 2 ms fixed + 120 us/token.
    let declared_fixed_ns = 2_000_000u64;
    let declared_per_token_ns = 120_000u64;
    let mut grid = Vec::new();
    let mut always_reuse_regret = Vec::new();
    let mut always_recompute_regret = Vec::new();
    let mut declared_regret = Vec::new();
    let mut regret_rows = Vec::new();
    for per_token_us in [10u64, 50, 120, 500] {
        let per_token_ns = per_token_us * 1000;
        let mut reuse_regret = 0i64;
        let mut recompute_regret = 0i64;
        let mut model_regret = 0i64;
        let mut oracle_reuse = 0u64;
        let mut model_reuse = 0u64;
        for (_, reused, tokens, reuse_ns) in &decisions {
            let recompute_ns = declared_fixed_ns.saturating_add(per_token_ns * tokens);
            let oracle_reuse_here = *reused && *reuse_ns < recompute_ns;
            if oracle_reuse_here {
                oracle_reuse += 1;
            }
            // Cost under each policy.
            let always_reuse_cost = if *reused {
                *reuse_ns as i64
            } else {
                recompute_ns as i64
            };
            let always_recompute_cost = recompute_ns as i64;
            let model_decides_reuse = *reused
                && (*reuse_ns as i128)
                    < i128::from(declared_fixed_ns + declared_per_token_ns * tokens);
            if model_decides_reuse {
                model_reuse += 1;
            }
            let model_cost = if model_decides_reuse {
                *reuse_ns as i64
            } else {
                recompute_ns as i64
            };
            let oracle_cost = if oracle_reuse_here {
                *reuse_ns as i64
            } else {
                recompute_ns as i64
            };
            reuse_regret += always_reuse_cost - oracle_cost;
            recompute_regret += always_recompute_cost - oracle_cost;
            model_regret += model_cost - oracle_cost;
        }
        always_reuse_regret.push(reuse_regret);
        always_recompute_regret.push(recompute_regret);
        declared_regret.push(model_regret);
        grid.push(per_token_us);
        regret_rows.push(json!({
            "per_token_us": per_token_us,
            "oracle_reuse_requests": oracle_reuse,
            "declared_model_reuse_requests": model_reuse,
            "always_reuse_regret_ms": round((*always_reuse_regret.last().unwrap()).max(0) as f64 / 1e6),
            "always_recompute_regret_ms": round((*always_recompute_regret.last().unwrap()).max(0) as f64 / 1e6),
            "declared_model_regret_ms": round((*declared_regret.last().unwrap()).max(0) as f64 / 1e6),
        }));
    }

    let policies = [
        ("always_reuse", &always_reuse_regret),
        ("always_recompute", &always_recompute_regret),
        ("declared_model", &declared_regret),
    ];
    let mut best_policy = String::new();
    let mut best_total = i64::MAX;
    for (name, values) in policies {
        let total: i64 = values.iter().map(|value| (*value).max(0)).sum();
        if total < best_total {
            best_total = total;
            best_policy = name.to_owned();
        }
    }

    let receipt = json!({
        "schema": "weft.kv.trace_replay.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unknown".to_owned()),
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "label": "engine-shaped trace replay over real KV prefix records; NOT an inference engine",
        "fixture": {
            "shared_prefixes": SHARED_PREFIXES,
            "requests": REQUESTS,
            "shape": {"layers": LAYERS, "kv_heads": HEADS, "head_dim": HEAD_DIM,
                      "page_rows": PAGE_ROWS, "bytes_per_page_all_layers":
                      PAGE_ROWS as u64 * 2 * u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES as u64},
            "mix": {"exact": counts[0], "extension": counts[1], "token_drift": counts[2], "model_drift": counts[3]},
            "build_ms": round(build_ms),
            "index_build_ms": round(index_build.build_ms),
            "index_entries": index_build.entries,
        },
        "measured": {
            "lookup_indexed": distribution(&lookup_indexed),
            "lookup_scan_exact_only": distribution(&lookup_scan),
            "reuse_get": distribution(&get_us),
            "token_chain_hash_local": distribution(&hash_us),
            "reuse_logical_bytes": reuse_bytes,
            "reuse_requests": get_us.len(),
            "indexed_mismatches": indexed_mismatches,
            "index_stats": {
                "loads": store.kv_index_stats_snapshot().loads,
                "rejects": store.kv_index_stats_snapshot().rejects,
                "lookups": store.kv_index_stats_snapshot().lookups,
                "probes": store.kv_index_stats_snapshot().probes,
                "rejected_candidates": store.kv_index_stats_snapshot().rejected_candidates,
                "fallbacks": store.kv_index_stats_snapshot().fallbacks,
                "notes": store.kv_index_stats_snapshot().notes,
            },
        },
        "decision": {
            "declared_prefill_fixed_ns": declared_fixed_ns,
            "declared_prefill_ns_per_token": declared_per_token_ns,
            "grid_per_token_us": grid,
            "rows": regret_rows,
            "best_policy_by_total_regret": best_policy,
            "measured_break_even_per_token_us": {
                "value": round(((lookup_indexed_sorted_p50(&lookup_indexed) + ret_p50(&get_us)) as f64)
                    / median_tokens(&decisions) as f64 / 1000.0),
                "note": "reuse is cheaper than recompute whenever the per-token prefill cost exceeds (measured lookup + measured get) / tokens. Measured, not declared.",
            },
            "limitation": "the fixture's prefixes are 512 tokens, so every point on the declared grid is far above break-even; the grid shows the policy is insensitive in this regime, it does not locate a crossover.",
            "note": "regret is computed against an oracle that knows the measured reuse cost and the hypothetical recompute cost in the same row. Only the reuse cost and the local token hashing are measured; every recompute number is the declared/hypothetical prefill model.",
        },
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "non_claims": [
            "no inference engine ran; this is trace replay, and the recompute axis is hypothetical",
            "the local token-chain hash is a measured lower bound on prefix identification work, not a prefill",
            "no TTFT, tokens/s or hit-rate claim against any engine",
            "one host, synthetic trace, warm page cache",
        ],
        "wall_ms": round(started.elapsed().as_secs_f64() * 1000.0),
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = OpenMode::ReadOnly;
    let _ = Path::new("/");
}
