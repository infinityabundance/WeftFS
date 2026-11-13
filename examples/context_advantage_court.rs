//! Context Advantage Court (R26) — one backend, with and without the context
//! plane.
//!
//! The frozen methodology is `bench/CONTRACTS-context-advantage.md`, which was
//! committed before this harness produced any measurement. Three arms run on
//! the same local POSIX data plane with the same commit protocol, chunking,
//! durability barrier, cache capacity, concurrency and logical objects:
//!
//! * `N` — the backend alone: plain durable objects, no metadata at all;
//! * `M` — the backend plus a strong application-level metadata system: a
//!   zstd-3 payload per page, a canonical manifest, and a real key→value index
//!   keyed on the brief's five dimensions plus position, with full
//!   value-validation before reuse, TTL/eviction and its own subset selection;
//! * `W` — the backend plus the shipped WeftFS context plane: provenance-bound
//!   KV pages (mode 19), capability-scoped reads, `weft.checkpoint.v1`
//!   manifests and causal-closure resolution.
//!
//! Two subcommands:
//!
//! ```text
//! context_advantage_court run --arm N|M|W --work DIR --out FILE --rep N --order N,M,W
//! context_advantage_court assemble --work DIR --out RECEIPT --log LOG --reps N
//! ```
//!
//! `run` executes one arm for one repetition and writes a raw phase JSON.
//! `assemble` merges the phase files into the `weft.context_advantage.v1`
//! receipt: it never invents a number, and it retains every raw sample.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Map as JsonMap, Value as Json};

use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{KvAttention, KvLayout, KvPositional, KvProvenanceInput};
use weftcodec::Limits;
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::identity::Hash32;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::state::object_causal_closure;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{FastCodec, WritePolicy};
use weftfs::Principal;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SCHEMA: &str = "weft.context_advantage.v1";
const PHASE_SCHEMA: &str = "weft.context_advantage.phase.v1";
const CONTRACT: &str = "bench/CONTRACTS-context-advantage.md";

const TENANT: &str = "tenant-ctx";
const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const GRANTOR_SEED: [u8; 32] = [7u8; 32];
const GRANTEE: [u8; 32] = [9u8; 32];
const CHUNK_SIZE: u64 = 65_536;

/// KV page geometry: 64 rows x (8 heads x 64 dim) x 2 bytes = 65,536 B.
const PAGE_ROWS: u64 = 64;
const HEAD_ELEMS: u64 = 512;
const PAGE_ELEMENTS: usize = (PAGE_ROWS * HEAD_ELEMS) as usize;
const PAGE_BYTES: usize = PAGE_ELEMENTS * 2;
const PREFIXES: usize = 4;
const LAYERS: usize = 2;

/// The recompute kernel's reduction depth (a stand-in for a prefill's work).
const CONTEXT: usize = 64;

/// Recovery fixture: 96 tensors of 64x256 BF16 = 32,768 B each.
const TENSORS: usize = 96;
const TENSOR_ROWS: u64 = 64;
const TENSOR_COLS: u64 = 256;
const TENSOR_BYTES: usize = (TENSOR_ROWS * TENSOR_COLS) as usize * 2;
const NEEDED: [usize; 6] = [3, 17, 31, 45, 67, 91];
const ACTIVATION_BYTES: usize = 32_768;
const DERIVED_BYTES: usize = 8_192;

/// Declared fast-path tolerance (contract §3, fixed before measurement).
const TOL_P50: f64 = 2.0;
const TOL_P95: f64 = 2.5;
const TOL_BYTES: f64 = 1.10;

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(safetensors::sha256(bytes))
}

fn write_json_atomic(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let text = serde_json::to_vec_pretty(value).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &text).map_err(|err| format!("{}: {err}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(())
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
    // Fields after the command: state is index 0, so utime is index 11 and
    // stime is index 12 of the remaining list.
    if fields.len() < 13 {
        return 0.0;
    }
    let utime: f64 = fields[11].parse().unwrap_or(0.0);
    let stime: f64 = fields[12].parse().unwrap_or(0.0);
    let ticks = utime + stime;
    let hz = 100.0;
    ticks / hz * 1000.0
}

fn percentile_us(samples: &[Duration], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn latency_summary(samples: &[Duration]) -> Json {
    if samples.is_empty() {
        return json!({ "n": 0 });
    }
    let mut sorted: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "n": sorted.len(),
        "min_us": sorted[0],
        "p50_us": percentile_us(samples, 0.50),
        "p95_us": percentile_us(samples, 0.95),
        "max_us": sorted[sorted.len() - 1],
    })
}

fn walk_footprint(root: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

fn uname() -> String {
    match Command::new("uname").arg("-srm").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => "unknown".to_owned(),
    }
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
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").ok(),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "uname": uname(),
    })
}

// ---------------------------------------------------------------------------
// Scope: one full provenance tuple
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    label: String,
    model_root: [u8; 32],
    adapter: Option<[u8; 32]>,
    tokenizer_scheme: String,
    tokenizer_vocab: u64,
    head_count: u32,
    head_dim: u32,
    positional: String,
    dtype: String,
    page_rows: u64,
    layer_count: u64,
    salt: [u8; 32],
}

fn root32(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn base_scope() -> Scope {
    Scope {
        label: "S0".to_owned(),
        model_root: root32(0x11),
        adapter: None,
        tokenizer_scheme: "bpe".to_owned(),
        tokenizer_vocab: 151_936,
        head_count: 8,
        head_dim: 64,
        positional: "rope".to_owned(),
        dtype: "bf16".to_owned(),
        page_rows: PAGE_ROWS,
        layer_count: LAYERS as u64,
        salt: root32(0xA0),
    }
}

/// The seven single-dimension variants. Each changes exactly one field and
/// leaves the page geometry and the payload bytes identical.
fn variant_scopes() -> Vec<Scope> {
    let base = base_scope();
    let mut out = Vec::new();

    let mut model = base.clone();
    model.label = "model".to_owned();
    model.model_root = root32(0x22);
    out.push(model);

    let mut adapter = base.clone();
    adapter.label = "adapter".to_owned();
    adapter.adapter = Some(root32(0x33));
    out.push(adapter);

    let mut tokenizer = base.clone();
    tokenizer.label = "tokenizer".to_owned();
    tokenizer.tokenizer_scheme = "spm".to_owned();
    // Same vocab size on purpose: a key on vocab size alone must collide.
    out.push(tokenizer);

    let mut attention = base.clone();
    attention.label = "attention".to_owned();
    // 16 heads x 32 dim has exactly the same element count as 8 x 64.
    attention.head_count = 16;
    attention.head_dim = 32;
    out.push(attention);

    let mut positional = base.clone();
    positional.label = "positional".to_owned();
    positional.positional = "alibi".to_owned();
    out.push(positional);

    let mut dtype = base.clone();
    dtype.label = "dtype".to_owned();
    dtype.dtype = "fp16".to_owned();
    out.push(dtype);

    let mut salt = base.clone();
    salt.label = "salt".to_owned();
    salt.salt = root32(0xB0);
    out.push(salt);

    out
}

impl Scope {
    fn model_name(&self) -> &'static str {
        if self.model_root[0] == 0x11 {
            "model-a"
        } else {
            "model-b"
        }
    }

    fn dtype_enum(&self) -> KvDtype {
        match self.dtype.as_str() {
            "fp16" => KvDtype::Fp16,
            "fp32" => KvDtype::Fp32,
            _ => KvDtype::Bf16,
        }
    }

    fn attention(&self) -> KvAttention {
        KvAttention {
            head_count: self.head_count,
            head_dim: self.head_dim,
            sliding_window: 0,
            rope_theta_bits: 10_000.0f64.to_bits(),
            rope_scale_bits: 1.0f64.to_bits(),
        }
    }

    fn layout(&self) -> KvLayout {
        KvLayout {
            dtype: self.dtype_enum(),
            page_rows: self.page_rows,
            layer_count: self.layer_count,
        }
    }

    fn positional_enum(&self) -> KvPositional {
        match self.positional.as_str() {
            "alibi" => KvPositional::Alibi,
            "none" => KvPositional::None,
            _ => KvPositional::Rope,
        }
    }

    fn adapter_set_root(&self) -> [u8; 32] {
        match self.adapter {
            Some(root) => root,
            None => [0u8; 32],
        }
    }

    fn tokenizer_root(&self) -> [u8; 32] {
        let mut buf = Vec::new();
        buf.extend_from_slice(self.tokenizer_scheme.as_bytes());
        buf.extend_from_slice(&self.tokenizer_vocab.to_le_bytes());
        safetensors::sha256(&buf)
    }

    fn attention_bytes(&self) -> Vec<u8> {
        let a = self.attention();
        let mut buf = Vec::with_capacity(40);
        for value in [
            u64::from(a.head_count),
            u64::from(a.head_dim),
            a.sliding_window,
            a.rope_theta_bits,
            a.rope_scale_bits,
        ] {
            buf.extend_from_slice(&value.to_le_bytes());
        }
        buf
    }

    /// The index key a competent application would derive from the five
    /// dimensions the brief names, plus the page position.
    fn five_dim_key(&self, prefix: usize, layer: usize) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(&self.model_root);
        key.extend_from_slice(&self.adapter_set_root());
        key.extend_from_slice(&self.tokenizer_root());
        key.extend_from_slice(&self.attention_bytes());
        key.extend_from_slice(&(prefix as u64).to_le_bytes());
        key.extend_from_slice(&(layer as u64).to_le_bytes());
        key
    }

    /// A diagnostic variant: the same index keyed on all eight dimensions.
    fn full_key(&self, prefix: usize, layer: usize) -> Vec<u8> {
        let mut key = self.five_dim_key(prefix, layer);
        key.extend_from_slice(self.positional.as_bytes());
        key.extend_from_slice(self.dtype.as_bytes());
        key.extend_from_slice(&self.page_rows.to_le_bytes());
        key.extend_from_slice(&self.layer_count.to_le_bytes());
        key.extend_from_slice(&self.salt);
        key
    }

    fn to_json(&self) -> Json {
        json!({
            "label": self.label,
            "model_root": hex::encode(self.model_root),
            "adapter": self.adapter.map(hex::encode),
            "tokenizer_scheme": self.tokenizer_scheme,
            "tokenizer_vocab": self.tokenizer_vocab,
            "head_count": self.head_count,
            "head_dim": self.head_dim,
            "positional": self.positional,
            "dtype": self.dtype,
            "page_rows": self.page_rows,
            "layer_count": self.layer_count,
            "salt": hex::encode(self.salt),
        })
    }

    fn from_json(value: &Json) -> Result<Self, String> {
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Json::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("scope field `{key}` missing"))
        };
        let number = |key: &str| -> Result<u64, String> {
            value
                .get(key)
                .and_then(Json::as_u64)
                .ok_or_else(|| format!("scope field `{key}` missing"))
        };
        let hash = |key: &str| -> Result<[u8; 32], String> {
            let bytes = hex::decode(text(key)?).map_err(|err| err.to_string())?;
            if bytes.len() != 32 {
                return Err(format!("scope field `{key}` is not 32 bytes"));
            }
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            Ok(out)
        };
        let adapter = match value.get("adapter").and_then(Json::as_str) {
            Some(text) => {
                let bytes = hex::decode(text).map_err(|err| err.to_string())?;
                if bytes.len() != 32 {
                    return Err("scope adapter is not 32 bytes".to_owned());
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Some(out)
            }
            None => None,
        };
        Ok(Scope {
            label: text("label")?,
            model_root: hash("model_root")?,
            adapter,
            tokenizer_scheme: text("tokenizer_scheme")?,
            tokenizer_vocab: number("tokenizer_vocab")?,
            head_count: number("head_count")? as u32,
            head_dim: number("head_dim")? as u32,
            positional: text("positional")?,
            dtype: text("dtype")?,
            page_rows: number("page_rows")?,
            layer_count: number("layer_count")?,
            salt: hash("salt")?,
        })
    }
}

fn page_input(scope: &Scope, prefix: usize, layer: usize) -> KvCacheInput {
    KvCacheInput {
        model: scope.model_name().to_owned(),
        layer: layer as u32,
        session: format!("session-{prefix}"),
        page_index: layer as u64,
        page_count: LAYERS as u64,
        seq_start: 0,
        seq_end: scope.page_rows,
        dtype: scope.dtype_enum(),
        head_count: scope.head_count,
        head_dim: scope.head_dim,
    }
}

fn provenance_input(scope: &Scope) -> KvProvenanceInput {
    let mut provenance = KvProvenanceInput::new(
        root32_hash(&scope.model_root),
        &scope.tokenizer_scheme,
        scope.tokenizer_vocab,
        scope.attention(),
        scope.positional_enum(),
        scope.layout(),
        root32_hash(&scope.salt),
    );
    if let Some(adapter) = scope.adapter {
        provenance = provenance.with_adapter(root32_hash(&adapter));
    }
    provenance
}

fn root32_hash(bytes: &[u8; 32]) -> Hash32 {
    Hash32::from_bytes(*bytes)
}

fn positions() -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for prefix in 0..PREFIXES {
        for layer in 0..LAYERS {
            out.push((prefix, layer));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The payload, and the kernel that recomputes it
// ---------------------------------------------------------------------------

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn token_values(prefix: usize, layer: usize) -> [f32; 8] {
    let mut state = 0x9E37_79B9_7F4A_7C15u64
        ^ (prefix as u64).wrapping_mul(0x0000_0100_0000_01B3)
        ^ ((layer as u64) << 32);
    let mut out = [0f32; 8];
    for slot in out.iter_mut() {
        let value = xorshift(&mut state);
        *slot = ((value >> 40) as f32 / (1u64 << 24) as f32) - 1.0;
    }
    out
}

fn weight_table() -> Vec<f32> {
    let mut state = 0x243F_6A88_85A3_08D3u64;
    let mut out = Vec::with_capacity(CONTEXT);
    for _ in 0..CONTEXT {
        let value = xorshift(&mut state);
        out.push(((value >> 40) as f32 / (1u64 << 24) as f32) - 1.0);
    }
    out
}

/// A stable 64-bit discriminator over every provenance dimension. It is used
/// only by the fixture generator: two scopes produce *different* element bytes,
/// exactly as two adapters or two tokenizers would produce different KV state.
fn scope_tag(scope: &Scope) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&scope.model_root);
    match scope.adapter {
        Some(adapter) => eat(&adapter),
        None => eat(b"no-adapter"),
    }
    eat(scope.tokenizer_scheme.as_bytes());
    eat(&scope.tokenizer_vocab.to_le_bytes());
    eat(&scope.head_count.to_le_bytes());
    eat(&scope.head_dim.to_le_bytes());
    eat(scope.positional.as_bytes());
    eat(scope.dtype.as_bytes());
    eat(&scope.page_rows.to_le_bytes());
    eat(&scope.layer_count.to_le_bytes());
    eat(&scope.salt);
    h
}

/// The reference kernel. It *computes* the page element bytes: the fixture was
/// produced by this function, so "recompute" regenerates real bytes rather than
/// sleeping. It is a scalar kernel, not an optimized prefill; see the contract.
///
/// Each element is a reduction over `CONTEXT` products of a token value and a
/// weight, with the weight index driven by a per-element hash so the result is
/// not periodic in the page geometry: the payload is high-entropy in the way a
/// real KV page is. The reduction is seeded by the scope, so two provenance
/// scopes produce different element bytes and "accepted but wrong" is
/// observable, as it would be in production.
fn recompute_page(scope: &Scope, prefix: usize, layer: usize) -> Vec<u8> {
    let tokens = token_values(prefix, layer);
    let weights = weight_table();
    let mask = CONTEXT - 1;
    let tag = scope_tag(scope);
    let mut out = Vec::with_capacity(PAGE_BYTES);
    for row in 0..PAGE_ROWS as usize {
        for element in 0..HEAD_ELEMS as usize {
            let mut h = 0x9E37_79B9_7F4A_7C15u64
                ^ tag
                ^ (element as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)
                ^ ((row as u64) << 17)
                ^ ((layer as u64) << 37)
                ^ ((prefix as u64) << 53);
            let mut acc = 0f32;
            for t in 0..CONTEXT {
                h ^= h << 13;
                h ^= h >> 7;
                h ^= h << 17;
                acc += tokens[t & 7] * weights[(h as usize) & mask];
            }
            let bits = (acc.to_bits() >> 16) as u16;
            out.extend_from_slice(&bits.to_le_bytes());
        }
    }
    out
}

fn tensor_bytes(index: usize) -> Vec<u8> {
    let mut state = 0xD1B5_4A32_D192_ED03u64 ^ (index as u64).wrapping_mul(0x9E37_79B9);
    let mut out = Vec::with_capacity(TENSOR_BYTES);
    while out.len() < TENSOR_BYTES {
        let value = xorshift(&mut state);
        out.extend_from_slice(&(value as u16).to_le_bytes());
    }
    out.truncate(TENSOR_BYTES);
    out
}

fn activation_bytes(index: usize) -> Vec<u8> {
    let mut state = 0x8542_7B7D_5B8A_5A3Fu64 ^ (index as u64).wrapping_mul(0x1000_0001);
    let mut out = Vec::with_capacity(ACTIVATION_BYTES);
    while out.len() < ACTIVATION_BYTES {
        out.extend_from_slice(&(xorshift(&mut state) as u16).to_le_bytes());
    }
    out.truncate(ACTIVATION_BYTES);
    out
}

fn derived_bytes() -> Vec<u8> {
    let mut state = 0xC2B2_AE3D_27D4_EB4Fu64;
    let mut out = Vec::with_capacity(DERIVED_BYTES);
    while out.len() < DERIVED_BYTES {
        out.extend_from_slice(&(xorshift(&mut state) as u16).to_le_bytes());
    }
    out.truncate(DERIVED_BYTES);
    out
}

// ---------------------------------------------------------------------------
// Options and store helpers
// ---------------------------------------------------------------------------

fn options() -> Options {
    Options {
        chunk_size: CHUNK_SIZE,
        fixed_timestamp_ns: Some(NOW_NS),
        gc_grace_secs: 0,
        ..Options::default()
    }
}

fn open_readonly(path: &Path) -> Result<Store, String> {
    Store::open(path, OpenMode::ReadOnly).map_err(|err| format!("open {}: {err}", path.display()))
}

fn open_readwrite(path: &Path) -> Result<Store, String> {
    Store::open(path, OpenMode::ReadWrite)
        .map_err(|err| format!("open rw {}: {err}", path.display()))
}

fn resolve(store: &Store, path: &str) -> Option<Hash32> {
    store.resolve_path(path).ok()
}

fn kv_name(prefix: usize, layer: usize) -> String {
    format!("p{prefix}-l{layer}.kv")
}

fn n_name(scope: &Scope, prefix: usize, layer: usize) -> String {
    format!("{}-p{prefix}-l{layer}.kv", scope.model_name())
}

fn blob_name(prefix: usize, layer: usize) -> String {
    format!("blob-p{prefix}-l{layer}.zst")
}

fn tenant() -> &'static str {
    TENANT
}

fn capability(roots: Vec<Hash32>) -> Result<Principal, String> {
    let mut roots = roots;
    roots.sort();
    roots.dedup();
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).map_err(|err| err.to_string())?,
        Hash32::from_bytes(GRANTEE),
        TENANT,
        roots,
        vec![format!("kv.{TENANT}")],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes(root32(0xA1)),
        NOW_NS,
        Hash32::from_bytes(root32(0xA2)),
    )
    .map_err(|err| err.to_string())?;
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).map_err(|err| err.to_string())?;
    let encoded = blob.encode().map_err(|err| err.to_string())?;
    let verified = verify_envelope(&encoded, &[], NOW_NS, None).map_err(|err| err.to_string())?;
    Ok(Principal::new(1001, 1001, Vec::new()).with_envelope(verified))
}

// ---------------------------------------------------------------------------
// Arm
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    N,
    M,
    W,
}

impl Arm {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "N" | "n" => Ok(Arm::N),
            "M" | "m" => Ok(Arm::M),
            "W" | "w" => Ok(Arm::W),
            other => Err(format!("unknown arm `{other}` (expected N|M|W)")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Arm::N => "N",
            Arm::M => "M",
            Arm::W => "W",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Arm::N => "backend alone: plain durable objects, no metadata",
            Arm::M => {
                "backend + strong application metadata (manifest, index, TTL, own subset selection)"
            }
            Arm::W => "backend + WeftFS context plane (mode 19, scoped reads, causal closure)",
        }
    }
}

// ---------------------------------------------------------------------------
// M's application metadata system
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MEntry {
    key_hex: String,
    scope: Scope,
    blob: String,
    content_root: String,
    bytes: u64,
    created_ns: i64,
    last_used_ns: i64,
}

struct MIndex {
    entries: BTreeMap<Vec<u8>, MEntry>,
    loads: u64,
    note: Option<String>,
}

impl MIndex {
    fn empty(note: &str) -> Self {
        MIndex {
            entries: BTreeMap::new(),
            loads: 0,
            note: Some(note.to_owned()),
        }
    }

    /// Load the index object. A missing or unreadable index is an empty index
    /// with a note: that is what a real service does, and it is the state the
    /// metadata-loss probe measures.
    fn load(store: &Store) -> Self {
        let Some(id) = resolve(store, "/kv/index.json") else {
            return MIndex::empty("index object absent");
        };
        let Ok(bytes) = store.read_object(id) else {
            return MIndex::empty("index object unreadable");
        };
        let Ok(value) = serde_json::from_slice::<Json>(&bytes) else {
            return MIndex::empty("index object is not JSON");
        };
        let Some(entries) = value.get("entries").and_then(Json::as_array) else {
            return MIndex::empty("index object has no entries");
        };
        let mut map = BTreeMap::new();
        for entry in entries {
            let Some(key_hex) = entry.get("key").and_then(Json::as_str) else {
                continue;
            };
            let Ok(key) = hex::decode(key_hex) else {
                continue;
            };
            let Some(scope_json) = entry.get("scope") else {
                continue;
            };
            let Ok(scope) = Scope::from_json(scope_json) else {
                continue;
            };
            let Some(blob) = entry.get("blob").and_then(Json::as_str) else {
                continue;
            };
            let Some(content_root) = entry.get("content_root").and_then(Json::as_str) else {
                continue;
            };
            let bytes = entry.get("bytes").and_then(Json::as_u64).unwrap_or(0);
            map.insert(
                key,
                MEntry {
                    key_hex: key_hex.to_owned(),
                    scope,
                    blob: blob.to_owned(),
                    content_root: content_root.to_owned(),
                    bytes,
                    created_ns: entry.get("created_ns").and_then(Json::as_i64).unwrap_or(0),
                    last_used_ns: entry
                        .get("last_used_ns")
                        .and_then(Json::as_i64)
                        .unwrap_or(0),
                },
            );
        }
        MIndex {
            loads: map.len() as u64,
            entries: map,
            note: None,
        }
    }

    fn to_json(&self, ttl_ns: i64, capacity: usize) -> Json {
        let entries: Vec<Json> = self
            .entries
            .values()
            .map(|entry| {
                json!({
                    "key": entry.key_hex,
                    "scope": entry.scope.to_json(),
                    "blob": entry.blob,
                    "content_root": entry.content_root,
                    "bytes": entry.bytes,
                    "created_ns": entry.created_ns,
                    "last_used_ns": entry.last_used_ns,
                })
            })
            .collect();
        json!({
            "schema": "app.kv.index.v1",
            "ttl_ns": ttl_ns,
            "capacity": capacity,
            "key_dimensions": [
                "model_root", "adapter_set_root", "tokenizer_root",
                "attention_config", "prefix", "layer"
            ],
            "value_dimensions": 8,
            "entries": entries,
        })
    }
}

const M_TTL_NS: i64 = 3_600_000_000_000; // one hour, far beyond the court run
const M_CAPACITY: usize = 4_096;

fn zstd_compress(bytes: &[u8]) -> Result<Vec<u8>, String> {
    zstd::encode_all(bytes, 3).map_err(|err| err.to_string())
}

fn zstd_decompress(bytes: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let out = zstd::decode_all(bytes).map_err(|err| err.to_string())?;
    if out.len() != expected {
        return Err(format!(
            "zstd payload is {} bytes, expected {expected}",
            out.len()
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Build (experiment A)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct BuildReport {
    writes: u64,
    backend_bytes_written: u64,
    ms: f64,
    commits: u64,
    index_entries: u64,
    mode19_bytes: u64,
    mode12_bytes: u64,
    page_bytes: u64,
    metadata_objects: Vec<String>,
    note: Option<String>,
}

fn build_a(
    arm: Arm,
    store: &mut Store,
    kv: Hash32,
    pages: &[(usize, usize, Vec<u8>)],
    scope: &Scope,
) -> Result<BuildReport, String> {
    let started = Instant::now();
    let before = store.backend_stats();
    let mut report = BuildReport::default();
    let mut index_entries: Vec<MEntry> = Vec::new();
    let mut manifest_entries: Vec<Json> = Vec::new();

    for (prefix, layer, bytes) in pages {
        match arm {
            Arm::N => {
                let name = n_name(scope, *prefix, *layer);
                store
                    .create_file(kv, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("N write `{name}`: {err}"))?;
                report.writes += 1;
                report.commits += 1;
            }
            Arm::M => {
                let blob = zstd_compress(bytes)?;
                let name = blob_name(*prefix, *layer);
                store
                    .create_file(kv, name.as_bytes(), &blob, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("M write `{name}`: {err}"))?;
                report.writes += 1;
                report.commits += 1;
                let key = scope.five_dim_key(*prefix, *layer);
                index_entries.push(MEntry {
                    key_hex: hex::encode(&key),
                    scope: scope.clone(),
                    blob: name.clone(),
                    content_root: sha256_hex(bytes),
                    bytes: bytes.len() as u64,
                    created_ns: NOW_NS,
                    last_used_ns: NOW_NS,
                });
                manifest_entries.push(json!({
                    "position": {"prefix": prefix, "layer": layer},
                    "blob": name,
                    "content_root": sha256_hex(bytes),
                    "bytes": bytes.len() as u64,
                    "scope": scope.to_json(),
                }));
            }
            Arm::W => {
                let name = kv_name(*prefix, *layer);
                let written = store
                    .write_kv_prov_page(
                        kv,
                        name.as_bytes(),
                        tenant(),
                        &page_input(scope, *prefix, *layer),
                        bytes,
                        &provenance_input(scope),
                        0,
                        0,
                    )
                    .map_err(|err| format!("W write `{name}`: {err}"))?;
                report.writes += 1;
                report.commits += 1;
                report.mode19_bytes += written.frame_bytes;
                report.page_bytes += written.page_bytes;
                // The same page as a mode-12 frame: the isolated cost of the
                // provenance descriptor, separated from the codec difference.
                let limits = Limits::default();
                let mode12 = weftcodec::kv_cache::encode_kv_page(
                    &page_input(scope, *prefix, *layer),
                    bytes,
                    &limits,
                )
                .map_err(|err| err.to_string())?;
                report.mode12_bytes += mode12.len() as u64;
            }
        }
    }

    if arm == Arm::M {
        let index = MIndex {
            entries: index_entries
                .iter()
                .map(|entry| {
                    (
                        hex::decode(&entry.key_hex).unwrap_or_default(),
                        entry.clone(),
                    )
                })
                .collect(),
            loads: index_entries.len() as u64,
            note: None,
        };
        let index_value = index.to_json(M_TTL_NS, M_CAPACITY);
        let index_bytes = serde_json::to_vec(&index_value).map_err(|err| err.to_string())?;
        store
            .create_file(
                kv,
                b"index.json",
                &index_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("M write index: {err}"))?;
        report.commits += 1;
        let manifest = json!({
            "schema": "app.kv.manifest.v1",
            "entries": manifest_entries,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|err| err.to_string())?;
        store
            .create_file(
                kv,
                b"manifest.json",
                &manifest_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("M write manifest: {err}"))?;
        report.commits += 1;
        report.index_entries = index_entries.len() as u64;
        report.metadata_objects = vec!["/kv/index.json".to_owned(), "/kv/manifest.json".to_owned()];
    }

    report.ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = store.backend_stats();
    report.backend_bytes_written = after.bytes_written.saturating_sub(before.bytes_written);
    if report.mode19_bytes == 0 && arm == Arm::W {
        report.note = Some("mode-19 frame sizes were not recorded".to_owned());
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Measurement A
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ProbeTally {
    accepted: u64,
    accepted_wrong: u64,
    denied: u64,
    logical_returned: u64,
    backend_bytes: u64,
    requests: u64,
    samples: Vec<Duration>,
}

fn add_tally(into: &mut JsonMap<String, Json>, id: &str, tally: &ProbeTally) {
    into.insert(
        id.to_owned(),
        json!({
            "accepted": tally.accepted,
            "accepted_wrong_bytes": tally.accepted_wrong,
            "denied": tally.denied,
            "logical_bytes_returned": tally.logical_returned,
            "backend_bytes": tally.backend_bytes,
            "requests": tally.requests,
            "latency": latency_summary(&tally.samples),
        }),
    );
}

/// One fast-path / reuse read. Returns `(logical_bytes, exact)`.
fn read_page(
    arm: Arm,
    store: &Store,
    scope: &Scope,
    prefix: usize,
    layer: usize,
    expected: &[u8],
    principal: Option<&Principal>,
) -> Result<(usize, bool), String> {
    match arm {
        Arm::N => {
            let path = format!("/kv/{}", n_name(scope, prefix, layer));
            let id = resolve(store, &path).ok_or_else(|| format!("N: `{path}` missing"))?;
            let bytes = store.read_object(id).map_err(|err| err.to_string())?;
            let exact = bytes == expected;
            Ok((bytes.len(), exact))
        }
        Arm::M => Err("M reads go through the index, not read_page".to_owned()),
        Arm::W => {
            let path = format!("/kv/{}", kv_name(prefix, layer));
            let id = resolve(store, &path).ok_or_else(|| format!("W: `{path}` missing"))?;
            let principal = principal.ok_or_else(|| "W: no principal".to_owned())?;
            let page = store
                .read_kv_prov_page(id, principal, NOW_NS)
                .map_err(|err| err.to_string())?;
            let exact = page.bytes == expected;
            Ok((page.bytes.len(), exact))
        }
    }
}

/// M's read: index lookup, then read + digest + full-scope validation.
fn m_read(
    store: &Store,
    index: &MIndex,
    scope: &Scope,
    prefix: usize,
    layer: usize,
    expected: &[u8],
    use_full_key: bool,
) -> Result<(usize, bool, bool), String> {
    // Returns (bytes_returned, exact, validated)
    let key = if use_full_key {
        scope.full_key(prefix, layer)
    } else {
        scope.five_dim_key(prefix, layer)
    };
    let now = NOW_NS;
    let Some(entry) = index.entries.get(&key) else {
        return Ok((0, false, false));
    };
    if now.saturating_sub(entry.created_ns) > M_TTL_NS {
        return Ok((0, false, false));
    }
    let Some(id) = resolve(store, &format!("/kv/{}", entry.blob)) else {
        return Ok((0, false, false));
    };
    let blob = store.read_object(id).map_err(|err| err.to_string())?;
    let decoded = match zstd_decompress(&blob, entry.bytes as usize) {
        Ok(bytes) => bytes,
        Err(_) => return Ok((0, false, false)),
    };
    if sha256_hex(&decoded) != entry.content_root {
        return Ok((0, false, false));
    }
    if &entry.scope != scope {
        // Full-value validation catches every dimension, including the three
        // the five-dimension key does not carry. A validated rejection returns
        // no bytes: the blob was read, the caller learns nothing from it.
        return Ok((0, false, true));
    }
    let exact = decoded == expected;
    Ok((decoded.len(), exact, true))
}

struct MeasurementA {
    compatible: Vec<Duration>,
    compatible_exact: u64,
    compatible_total: u64,
    compatible_backend_bytes: u64,
    compatible_requests: u64,
    compatible_logical: u64,
    probes: Vec<(String, ProbeTally)>,
    diagnostics: JsonMap<String, Json>,
    recompute: Vec<Duration>,
    recompute_exact: bool,
    open_ms: f64,
    index_load: Json,
}

fn measure_a(
    arm: Arm,
    store: &Store,
    pages: &[(usize, usize, Vec<u8>)],
    variant_pages: &[Vec<(usize, usize, Vec<u8>)>],
    scope: &Scope,
    variants: &[Scope],
    index: &MIndex,
    rounds: usize,
) -> Result<MeasurementA, String> {
    let mut out = MeasurementA {
        compatible: Vec::new(),
        compatible_exact: 0,
        compatible_total: 0,
        compatible_backend_bytes: 0,
        compatible_requests: 0,
        compatible_logical: 0,
        probes: variants
            .iter()
            .map(|variant| (variant.label.clone(), ProbeTally::default()))
            .collect(),
        diagnostics: JsonMap::new(),
        recompute: Vec::new(),
        recompute_exact: true,
        open_ms: 0.0,
        index_load: Json::Null,
    };

    // ---- W setup: the descriptor id the caller expects per scope/position.
    let mut principals: BTreeMap<String, Principal> = BTreeMap::new();
    if arm == Arm::W {
        let setup_started = Instant::now();
        let limits = Limits::default();
        let mut roots = Vec::new();
        for (prefix, layer, bytes) in pages {
            let frame = weftcodec::kv_prov::encode_kv_prov_page(
                &page_input(scope, *prefix, *layer),
                bytes,
                &provenance_input(scope),
                &limits,
            )
            .map_err(|err| err.to_string())?;
            let parsed =
                weftcodec::frame::Frame::parse(&frame, &limits).map_err(|err| err.to_string())?;
            let codec = weftcodec::kv_prov::kv_prov_metadata(&parsed, &limits)
                .map_err(|err| err.to_string())?;
            let root = codec.root(&limits).map_err(|err| err.to_string())?;
            let descriptor = weftfs::kv_capability::KvProvDescriptor::from_input(
                &page_input(scope, *prefix, *layer),
                tenant(),
                bytes.len() as u64,
                root,
            )
            .map_err(|err| err.to_string())?;
            roots.push(descriptor.id().map_err(|err| err.to_string())?);
        }
        principals.insert(scope.label.clone(), capability(roots)?);
        for variant in variants {
            let mut variant_roots = Vec::new();
            let slot = variants
                .iter()
                .position(|candidate| candidate.label == variant.label)
                .ok_or_else(|| "variant slot missing".to_owned())?;
            for (prefix, layer, bytes) in &variant_pages[slot] {
                let frame = weftcodec::kv_prov::encode_kv_prov_page(
                    &page_input(variant, *prefix, *layer),
                    bytes,
                    &provenance_input(variant),
                    &limits,
                )
                .map_err(|err| err.to_string())?;
                let parsed = weftcodec::frame::Frame::parse(&frame, &limits)
                    .map_err(|err| err.to_string())?;
                let codec = weftcodec::kv_prov::kv_prov_metadata(&parsed, &limits)
                    .map_err(|err| err.to_string())?;
                let root = codec.root(&limits).map_err(|err| err.to_string())?;
                let descriptor = weftfs::kv_capability::KvProvDescriptor::from_input(
                    &page_input(variant, *prefix, *layer),
                    tenant(),
                    bytes.len() as u64,
                    root,
                )
                .map_err(|err| err.to_string())?;
                variant_roots.push(descriptor.id().map_err(|err| err.to_string())?);
            }
            principals.insert(variant.label.clone(), capability(variant_roots)?);
        }
        out.diagnostics.insert(
            "probe_setup_ms".to_owned(),
            json!(setup_started.elapsed().as_secs_f64() * 1000.0),
        );
    }

    // ---- Compatible reuse (the happy path).
    let principal = principals.get(&scope.label).cloned();
    for _ in 0..rounds {
        for (prefix, layer, expected) in pages {
            let before = store.backend_stats();
            let started = Instant::now();
            let (bytes, exact) = match arm {
                Arm::M => {
                    let (returned, exact, _validated) =
                        m_read(store, index, scope, *prefix, *layer, expected, false)?;
                    (returned, exact)
                }
                _ => read_page(
                    arm,
                    store,
                    scope,
                    *prefix,
                    *layer,
                    expected,
                    principal.as_ref(),
                )?,
            };
            out.compatible.push(started.elapsed());
            let after = store.backend_stats();
            out.compatible_backend_bytes += after.bytes_read.saturating_sub(before.bytes_read);
            out.compatible_requests +=
                (after.reads + after.range_reads).saturating_sub(before.reads + before.range_reads);
            out.compatible_logical += bytes as u64;
            out.compatible_total += 1;
            if exact && bytes == expected.len() {
                out.compatible_exact += 1;
            }
        }
    }

    // ---- Cross-scope probes.
    for (slot, variant) in variants.iter().enumerate() {
        let variant_principal = principals.get(&variant.label).cloned();
        for (prefix, layer, expected) in &variant_pages[slot] {
            let prefix = *prefix;
            let layer = *layer;
            let before = store.backend_stats();
            let started = Instant::now();
            let mut tally = std::mem::take(&mut out.probes[slot].1);
            match arm {
                Arm::N => {
                    let path = format!("/kv/{}", n_name(variant, prefix, layer));
                    match resolve(store, &path) {
                        None => {
                            tally.denied += 1;
                        }
                        Some(id) => match store.read_object(id) {
                            Ok(bytes) => {
                                tally.accepted += 1;
                                tally.logical_returned += bytes.len() as u64;
                                if bytes != *expected {
                                    tally.accepted_wrong += 1;
                                }
                            }
                            Err(_) => tally.denied += 1,
                        },
                    }
                }
                Arm::M => {
                    let (returned, exact, _validated) =
                        m_read(store, index, variant, prefix, layer, expected, false)?;
                    if returned == 0 {
                        tally.denied += 1;
                    } else {
                        tally.accepted += 1;
                        tally.logical_returned += returned as u64;
                        if !exact {
                            tally.accepted_wrong += 1;
                        }
                    }
                }
                Arm::W => {
                    let path = format!("/kv/{}", kv_name(prefix, layer));
                    match resolve(store, &path) {
                        None => tally.denied += 1,
                        Some(id) => {
                            let principal = variant_principal
                                .as_ref()
                                .ok_or_else(|| "W: no variant principal".to_owned())?;
                            match store.read_kv_prov_page(id, principal, NOW_NS) {
                                Ok(page) => {
                                    tally.accepted += 1;
                                    tally.logical_returned += page.bytes.len() as u64;
                                    if page.bytes != *expected {
                                        tally.accepted_wrong += 1;
                                    }
                                }
                                Err(_) => tally.denied += 1,
                            }
                        }
                    }
                }
            }
            let elapsed = started.elapsed();
            let after = store.backend_stats();
            tally.samples.push(elapsed);
            tally.backend_bytes += after.bytes_read.saturating_sub(before.bytes_read);
            tally.requests +=
                (after.reads + after.range_reads).saturating_sub(before.reads + before.range_reads);
            out.probes[slot].1 = tally;
        }
    }

    // ---- M's two labelled diagnostic variants over the same index.
    if arm == Arm::M {
        let mut index_only_accepts = 0u64;
        let mut full_key_denied = 0u64;
        let mut full_key_bytes = 0u64;
        for variant in variants {
            for (prefix, layer, expected) in pages {
                let key = variant.five_dim_key(*prefix, *layer);
                if index.entries.contains_key(&key) {
                    index_only_accepts += 1;
                }
                let before = store.backend_stats();
                let (returned, _exact, _validated) =
                    m_read(store, index, variant, *prefix, *layer, expected, true)?;
                let after = store.backend_stats();
                if returned == 0 {
                    full_key_denied += 1;
                }
                full_key_bytes += after.bytes_read.saturating_sub(before.bytes_read);
            }
        }
        out.diagnostics
            .insert("index_only_accepts".to_owned(), json!(index_only_accepts));
        out.diagnostics
            .insert("full_key_denied".to_owned(), json!(full_key_denied));
        out.diagnostics.insert(
            "full_key_denial_backend_bytes".to_owned(),
            json!(full_key_bytes),
        );
        out.diagnostics
            .insert("index_loaded_entries".to_owned(), json!(index.loads));
        out.diagnostics
            .insert("index_note".to_owned(), json!(index.note));
    }

    // ---- Recompute baseline: re-run the kernel that produced the page.
    for _ in 0..3 {
        let started = Instant::now();
        let bytes = recompute_page(scope, pages[0].0, pages[0].1);
        out.recompute.push(started.elapsed());
        if bytes != pages[0].2 {
            out.recompute_exact = false;
        }
    }

    // ---- Decode-only diagnostic (W): the same page as a mode-12 frame and as
    // a mode-19 frame, decoded in memory with no I/O. This isolates the cost of
    // the provenance descriptor from the payload codec and from the backend.
    if arm == Arm::W {
        let limits = Limits::default();
        let (prefix, layer, bytes) = &pages[0];
        let frame19 = weftcodec::kv_prov::encode_kv_prov_page(
            &page_input(scope, *prefix, *layer),
            bytes,
            &provenance_input(scope),
            &limits,
        )
        .map_err(|err| err.to_string())?;
        let frame12 = weftcodec::kv_cache::encode_kv_page(
            &page_input(scope, *prefix, *layer),
            bytes,
            &limits,
        )
        .map_err(|err| err.to_string())?;
        let mut decode19 = Vec::new();
        let mut decode12 = Vec::new();
        for _ in 0..8 {
            let parsed =
                weftcodec::frame::Frame::parse(&frame19, &limits).map_err(|err| err.to_string())?;
            let started = Instant::now();
            weftcodec::kv_prov::decode_kv_prov_page(&parsed, &limits)
                .map_err(|err| err.to_string())?;
            decode19.push(started.elapsed());
            let parsed =
                weftcodec::frame::Frame::parse(&frame12, &limits).map_err(|err| err.to_string())?;
            let started = Instant::now();
            weftcodec::kv_cache::decode_kv_page(&parsed, &limits).map_err(|err| err.to_string())?;
            decode12.push(started.elapsed());
        }
        out.diagnostics.insert(
            "decode_only".to_owned(),
            json!({
                "mode19": latency_summary(&decode19),
                "mode12": latency_summary(&decode12),
                "mode19_frame_bytes": frame19.len(),
                "mode12_frame_bytes": frame12.len(),
                "note": "in-memory decode of the same page; isolates the provenance descriptor from I/O",
            }),
        );
    }

    Ok(out)
}

/// The metadata-loss probe: the arm's own metadata database is gone.
fn measure_loss(
    arm: Arm,
    store: &Store,
    pages: &[(usize, usize, Vec<u8>)],
    scope: &Scope,
    rounds: usize,
) -> Result<Json, String> {
    let mut samples = Vec::new();
    let mut reused = 0u64;
    let mut exact = 0u64;
    let mut total = 0u64;
    let mut backend_bytes = 0u64;
    match arm {
        Arm::N => {
            // N never had metadata, so "loss" is a no-op: measured so the row
            // exists in every arm.
            for _ in 0..rounds {
                for (prefix, layer, expected) in pages {
                    let started = Instant::now();
                    let (_, is_exact) =
                        read_page(arm, store, scope, *prefix, *layer, expected, None)?;
                    samples.push(started.elapsed());
                    reused += 1;
                    total += 1;
                    if is_exact {
                        exact += 1;
                    }
                }
            }
        }
        Arm::M => {
            // The index and manifest objects were removed before this call.
            let index = MIndex::load(store);
            for _ in 0..rounds {
                for (prefix, layer, expected) in pages {
                    let before = store.backend_stats();
                    let started = Instant::now();
                    let (returned, is_exact, _validated) =
                        m_read(store, &index, scope, *prefix, *layer, expected, false)?;
                    samples.push(started.elapsed());
                    let after = store.backend_stats();
                    backend_bytes += after.bytes_read.saturating_sub(before.bytes_read);
                    total += 1;
                    if returned > 0 {
                        reused += 1;
                        if is_exact {
                            exact += 1;
                        }
                    }
                }
            }
        }
        Arm::W => {
            let limits = Limits::default();
            let mut roots = Vec::new();
            for (prefix, layer, bytes) in pages {
                let frame = weftcodec::kv_prov::encode_kv_prov_page(
                    &page_input(scope, *prefix, *layer),
                    bytes,
                    &provenance_input(scope),
                    &limits,
                )
                .map_err(|err| err.to_string())?;
                let parsed = weftcodec::frame::Frame::parse(&frame, &limits)
                    .map_err(|err| err.to_string())?;
                let codec = weftcodec::kv_prov::kv_prov_metadata(&parsed, &limits)
                    .map_err(|err| err.to_string())?;
                let root = codec.root(&limits).map_err(|err| err.to_string())?;
                let descriptor = weftfs::kv_capability::KvProvDescriptor::from_input(
                    &page_input(scope, *prefix, *layer),
                    tenant(),
                    bytes.len() as u64,
                    root,
                )
                .map_err(|err| err.to_string())?;
                roots.push(descriptor.id().map_err(|err| err.to_string())?);
            }
            let principal = capability(roots)?;
            for _ in 0..rounds {
                for (prefix, layer, expected) in pages {
                    let before = store.backend_stats();
                    let started = Instant::now();
                    let (returned, is_exact) = read_page(
                        arm,
                        store,
                        scope,
                        *prefix,
                        *layer,
                        expected,
                        Some(&principal),
                    )?;
                    samples.push(started.elapsed());
                    let after = store.backend_stats();
                    backend_bytes += after.bytes_read.saturating_sub(before.bytes_read);
                    total += 1;
                    if returned > 0 {
                        reused += 1;
                        if is_exact {
                            exact += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(json!({
        "total": total,
        "reused": reused,
        "reused_exact": exact,
        "backend_bytes": backend_bytes,
        "latency": latency_summary(&samples),
        "metadata_removed": arm == Arm::M,
    }))
}

// ---------------------------------------------------------------------------
// Experiment B: dependency-aware recovery
// ---------------------------------------------------------------------------

fn build_b(arm: Arm, work: &Path) -> Result<(Json, String), String> {
    let root = work.join("store-b");
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|err| format!("{}: {err}", root.display()))?;
    }
    fs::create_dir_all(&root).map_err(|err| format!("{}: {err}", root.display()))?;
    let mut store = Store::init(&root, options()).map_err(|err| err.to_string())?;
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
    let ckpt = store
        .mkdir(root_id, b"ckpt", 0o755, 0, 0)
        .map_err(|err| err.to_string())?;

    let started = Instant::now();
    let before = store.backend_stats();
    let mut tensors = Vec::new();
    for index in 0..TENSORS {
        tensors.push((index, tensor_bytes(index)));
    }
    let activations = [activation_bytes(0), activation_bytes(1)];
    let derived = derived_bytes();
    let mut writes = 0u64;

    match arm {
        Arm::N => {
            for (index, bytes) in &tensors {
                let name = format!("t{index:03}.bin");
                store
                    .create_file(ckpt, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("N tensor write: {err}"))?;
                writes += 1;
            }
            for (slot, bytes) in activations.iter().enumerate() {
                let name = format!("act{slot:03}.bin");
                store
                    .create_file(ckpt, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("N activation write: {err}"))?;
                writes += 1;
            }
            store
                .create_file(ckpt, b"derived.bin", &derived, 0o644, 0, 0, BTreeMap::new())
                .map_err(|err| format!("N derived write: {err}"))?;
            writes += 1;
        }
        Arm::M => {
            let mut manifest = Vec::new();
            for (index, bytes) in &tensors {
                let blob = zstd_compress(bytes)?;
                let name = format!("t{index:03}.zst");
                store
                    .create_file(ckpt, name.as_bytes(), &blob, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("M tensor write: {err}"))?;
                writes += 1;
                manifest.push(json!({
                    "name": format!("t{index:03}"),
                    "blob": name,
                    "content_root": sha256_hex(bytes),
                    "bytes": bytes.len() as u64,
                    "dependencies": [],
                }));
            }
            for (slot, bytes) in activations.iter().enumerate() {
                let name = format!("act{slot:03}.bin");
                store
                    .create_file(ckpt, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("M activation write: {err}"))?;
                writes += 1;
            }
            store
                .create_file(ckpt, b"derived.bin", &derived, 0o644, 0, 0, BTreeMap::new())
                .map_err(|err| format!("M derived write: {err}"))?;
            writes += 1;
            let value = json!({
                "schema": "app.checkpoint.manifest.v1",
                "tensors": manifest,
                "needs": NEEDED.iter().map(|index| format!("t{index:03}")).collect::<Vec<_>>(),
                "activations": ["act000.bin", "act001.bin"],
                "derived": {
                    "blob": "derived.bin",
                    "dependencies": ["act000.bin", "act001.bin"],
                    "content_root": sha256_hex(&derived),
                    "bytes": derived.len() as u64,
                },
            });
            let bytes = serde_json::to_vec(&value).map_err(|err| err.to_string())?;
            store
                .create_file(ckpt, b"manifest.json", &bytes, 0o644, 0, 0, BTreeMap::new())
                .map_err(|err| format!("M checkpoint manifest: {err}"))?;
            writes += 1;
            store
                .create_file(ckpt, b"index.json", &bytes, 0o644, 0, 0, BTreeMap::new())
                .map_err(|err| format!("M checkpoint index: {err}"))?;
            writes += 1;
        }
        Arm::W => {
            let entries: Vec<(String, Vec<u8>)> = tensors
                .iter()
                .map(|(index, bytes)| (format!("t{index:03}"), bytes.clone()))
                .collect();
            let refs: Vec<SafetensorsEntry<'_>> = entries
                .iter()
                .map(|(name, bytes)| SafetensorsEntry {
                    name,
                    dtype: "bf16",
                    shape: &[TENSOR_ROWS, TENSOR_COLS],
                    data: bytes,
                })
                .collect();
            let container = safetensors::encode_file(&refs, None).map_err(|err| err.to_string())?;
            let container_path = work.join("ckpt.safetensors");
            fs::write(&container_path, &container)
                .map_err(|err| format!("{}: {err}", container_path.display()))?;
            let checkpoint_options = CheckpointWriteOptions {
                shards: 1,
                strategy: ShardStrategy::Zstd,
                write_policy: WritePolicy::Fast,
                fast_codec: FastCodec::Zstd,
                fast_zstd_level: 3,
                deterministic_seed: Some([0x5Au8; 32]),
                ..CheckpointWriteOptions::default()
            };
            store
                .write_checkpoint("ckpt", &container_path, &checkpoint_options)
                .map_err(|err| format!("W checkpoint write: {err}"))?;
            writes += 1;
            let mut activation_ids = Vec::new();
            for (slot, bytes) in activations.iter().enumerate() {
                let name = format!("act{slot:03}.bin");
                let id = store
                    .create_file(ckpt, name.as_bytes(), bytes, 0o644, 0, 0, BTreeMap::new())
                    .map_err(|err| format!("W activation write: {err}"))?;
                activation_ids.push(id);
                writes += 1;
            }
            store
                .create_file_seeded_causal(
                    ckpt,
                    b"derived.bin",
                    &derived,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    &[0xC7u8; 32],
                    &activation_ids,
                )
                .map_err(|err| format!("W derived write: {err}"))?;
            writes += 1;
        }
    }

    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = store.backend_stats();
    let build = json!({
        "writes": writes,
        "ms": ms,
        "backend_bytes_written": after.bytes_written.saturating_sub(before.bytes_written),
        "logical_bytes": (TENSORS * TENSOR_BYTES + 2 * ACTIVATION_BYTES + DERIVED_BYTES) as u64,
    });
    drop(store);
    let expected_outputs = expected_output_digest(&tensors, &activations, &derived);
    Ok((build, expected_outputs))
}

fn expected_output_digest(
    tensors: &[(usize, Vec<u8>)],
    activations: &[Vec<u8>],
    derived: &[u8],
) -> String {
    let mut buffer = Vec::new();
    for index in NEEDED {
        if let Some((_, bytes)) = tensors.iter().find(|(i, _)| *i == index) {
            buffer.extend_from_slice(bytes);
        }
    }
    for bytes in activations {
        buffer.extend_from_slice(bytes);
    }
    buffer.extend_from_slice(derived);
    sha256_hex(&buffer)
}

fn measure_b(arm: Arm, work: &Path) -> Result<(Json, Json), String> {
    let root = work.join("store-b");
    let store = open_readonly(&root)?;

    let mut samples = Vec::new();
    let mut outputs = 0u64;
    let mut logical = 0u64;
    let mut buffer = Vec::new();
    // The bytes of the *demanded* output set, in contract order, for every arm.
    // N materializes more than the contract demands; its demanded subset is
    // still comparable byte for byte.
    let mut demanded: Vec<u8> = Vec::new();
    let before = store.backend_stats();
    let started = Instant::now();

    match arm {
        Arm::N => {
            for index in 0..TENSORS {
                let path = format!("/ckpt/t{index:03}.bin");
                let id = resolve(&store, &path).ok_or_else(|| format!("{path} missing"))?;
                let t = Instant::now();
                let bytes = store.read_object(id).map_err(|err| err.to_string())?;
                samples.push(t.elapsed());
                if NEEDED.contains(&index) {
                    demanded.extend_from_slice(&bytes);
                }
                buffer.extend_from_slice(&bytes);
                logical += bytes.len() as u64;
                outputs += 1;
            }
            for slot in 0..2 {
                let path = format!("/ckpt/act{slot:03}.bin");
                let id = resolve(&store, &path).ok_or_else(|| format!("{path} missing"))?;
                let t = Instant::now();
                let bytes = store.read_object(id).map_err(|err| err.to_string())?;
                samples.push(t.elapsed());
                demanded.extend_from_slice(&bytes);
                buffer.extend_from_slice(&bytes);
                logical += bytes.len() as u64;
                outputs += 1;
            }
            let id =
                resolve(&store, "/ckpt/derived.bin").ok_or_else(|| "derived missing".to_owned())?;
            let t = Instant::now();
            let bytes = store.read_object(id).map_err(|err| err.to_string())?;
            samples.push(t.elapsed());
            demanded.extend_from_slice(&bytes);
            buffer.extend_from_slice(&bytes);
            logical += bytes.len() as u64;
            outputs += 1;
        }
        Arm::M => {
            let id = resolve(&store, "/ckpt/manifest.json")
                .ok_or_else(|| "M manifest missing".to_owned())?;
            let manifest_bytes = store.read_object(id).map_err(|err| err.to_string())?;
            let manifest: Json = serde_json::from_slice(&manifest_bytes)
                .map_err(|err| format!("M manifest is not JSON: {err}"))?;
            let tensors = manifest
                .get("tensors")
                .and_then(Json::as_array)
                .ok_or_else(|| "M manifest has no tensors".to_owned())?;
            for index in NEEDED {
                let name = format!("t{index:03}");
                let entry = tensors
                    .iter()
                    .find(|entry| entry.get("name").and_then(Json::as_str) == Some(&name))
                    .ok_or_else(|| format!("M manifest has no `{name}`"))?;
                let blob = entry
                    .get("blob")
                    .and_then(Json::as_str)
                    .ok_or_else(|| "M entry has no blob".to_owned())?;
                let bytes_expected =
                    entry.get("bytes").and_then(Json::as_u64).unwrap_or(0) as usize;
                let root_expected = entry
                    .get("content_root")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let id = resolve(&store, &format!("/ckpt/{blob}"))
                    .ok_or_else(|| format!("M blob `{blob}` missing"))?;
                let t = Instant::now();
                let raw = store.read_object(id).map_err(|err| err.to_string())?;
                let bytes = zstd_decompress(&raw, bytes_expected)?;
                samples.push(t.elapsed());
                if sha256_hex(&bytes) != root_expected {
                    return Err(format!("M blob `{blob}` digest mismatch"));
                }
                buffer.extend_from_slice(&bytes);
                logical += bytes.len() as u64;
                outputs += 1;
            }
            for slot in 0..2 {
                let path = format!("/ckpt/act{slot:03}.bin");
                let id = resolve(&store, &path).ok_or_else(|| format!("{path} missing"))?;
                let t = Instant::now();
                let bytes = store.read_object(id).map_err(|err| err.to_string())?;
                samples.push(t.elapsed());
                buffer.extend_from_slice(&bytes);
                logical += bytes.len() as u64;
                outputs += 1;
            }
            let id = resolve(&store, "/ckpt/derived.bin")
                .ok_or_else(|| "M derived missing".to_owned())?;
            let t = Instant::now();
            let bytes = store.read_object(id).map_err(|err| err.to_string())?;
            samples.push(t.elapsed());
            buffer.extend_from_slice(&bytes);
            logical += bytes.len() as u64;
            outputs += 1;
        }
        Arm::W => {
            let manifest = store
                .load_checkpoint("ckpt")
                .map_err(|err| format!("W checkpoint manifest: {err}"))?;
            for index in NEEDED {
                let name = format!("t{index:03}");
                manifest
                    .tensor(&name)
                    .map_err(|err| format!("W checkpoint has no `{name}`: {err}"))?;
                let t = Instant::now();
                let restored = store
                    .restore_named_tensor_at(None, "ckpt", &name, None, None)
                    .map_err(|err| format!("W restore `{name}`: {err}"))?;
                samples.push(t.elapsed());
                buffer.extend_from_slice(&restored.bytes);
                logical += restored.bytes.len() as u64;
                outputs += 1;
            }
            for slot in 0..2 {
                let path = format!("/ckpt/act{slot:03}.bin");
                let id = resolve(&store, &path).ok_or_else(|| format!("{path} missing"))?;
                let t = Instant::now();
                let bytes = store.read_object(id).map_err(|err| err.to_string())?;
                samples.push(t.elapsed());
                buffer.extend_from_slice(&bytes);
                logical += bytes.len() as u64;
                outputs += 1;
            }
            let id = resolve(&store, "/ckpt/derived.bin")
                .ok_or_else(|| "W derived missing".to_owned())?;
            let t = Instant::now();
            let bytes = store.read_object(id).map_err(|err| err.to_string())?;
            samples.push(t.elapsed());
            buffer.extend_from_slice(&bytes);
            logical += bytes.len() as u64;
            outputs += 1;
        }
    }

    let wall = started.elapsed();
    let after = store.backend_stats();
    let digest = match arm {
        Arm::N => sha256_hex(&demanded),
        _ => sha256_hex(&buffer),
    };

    // Causal closure: only W declares parents, so only W can prove them.
    let closure = if arm == Arm::W {
        let id =
            resolve(&store, "/ckpt/derived.bin").ok_or_else(|| "derived missing".to_owned())?;
        let record = store.object_meta(id).map_err(|err| err.to_string())?;
        let stored = object_causal_closure(&record).map_err(|err| err.to_string())?;
        let live: BTreeSet<Hash32> = store
            .current_state()
            .objects
            .values()
            .map(|record| record.version)
            .collect();
        let mut parents = 0u64;
        let mut missing = 0u64;
        if let Some(closure) = &stored {
            parents = closure.parents.len() as u64;
            for identity in &closure.closure {
                if !live.contains(identity) {
                    missing += 1;
                }
            }
        }
        json!({
            "declared": stored.is_some(),
            "parents": parents,
            "closure_members": stored.as_ref().map(|c| c.closure.len()).unwrap_or(0),
            "missing": missing,
            "verified": stored.is_some() && missing == 0,
        })
    } else {
        json!({ "declared": false, "verified": false })
    };

    let recovery = json!({
        "outputs": outputs,
        "logical_bytes": logical,
        "digest": digest,
        "wall_ms": wall.as_secs_f64() * 1000.0,
        "backend_bytes": after.bytes_read.saturating_sub(before.bytes_read),
        "requests": (after.reads + after.range_reads)
            .saturating_sub(before.reads + before.range_reads),
        "latency": latency_summary(&samples),
        "causal_closure": closure,
    });

    // Missing dependency: remove one activation the derived artifact needs.
    drop(store);
    let mut store = open_readwrite(&root)?;
    store
        .unlink(
            resolve(&store, "/ckpt").ok_or_else(|| "no /ckpt".to_owned())?,
            b"act000.bin",
        )
        .map_err(|err| format!("unlink activation: {err}"))?;
    drop(store);
    let store = open_readonly(&root)?;
    let missing = match arm {
        Arm::N => {
            let path = "/ckpt/act000.bin";
            match resolve(&store, path) {
                None => json!({"failed_closed": true, "class": "NotFound", "emitted": 0}),
                Some(_) => {
                    json!({"failed_closed": false, "class": "ok", "emitted": ACTIVATION_BYTES})
                }
            }
        }
        Arm::M => {
            let before = store.backend_stats();
            let mut emitted = 0u64;
            let mut class = "ok".to_owned();
            let mut failed = false;
            for slot in 0..2 {
                let path = format!("/ckpt/act{slot:03}.bin");
                match resolve(&store, &path) {
                    None => {
                        failed = true;
                        class = "NotFound".to_owned();
                    }
                    Some(id) => {
                        if let Ok(bytes) = store.read_object(id) {
                            emitted += bytes.len() as u64;
                        }
                    }
                }
            }
            let after = store.backend_stats();
            json!({
                "failed_closed": failed,
                "class": class,
                "emitted": emitted,
                "backend_bytes": after.bytes_read.saturating_sub(before.bytes_read),
            })
        }
        Arm::W => {
            let id =
                resolve(&store, "/ckpt/derived.bin").ok_or_else(|| "derived missing".to_owned())?;
            let record = store.object_meta(id).map_err(|err| err.to_string())?;
            let stored = object_causal_closure(&record).map_err(|err| err.to_string())?;
            let live: BTreeSet<Hash32> = store
                .current_state()
                .objects
                .values()
                .map(|record| record.version)
                .collect();
            let mut missing = 0u64;
            if let Some(closure) = &stored {
                for identity in &closure.closure {
                    if !live.contains(identity) {
                        missing += 1;
                    }
                }
            }
            json!({
                "failed_closed": missing > 0,
                "class": if missing > 0 { "DependencyMismatch" } else { "ok" },
                "emitted": 0,
                "missing_members": missing,
            })
        }
    };

    Ok((recovery, missing))
}

// ---------------------------------------------------------------------------
// One arm, one repetition
// ---------------------------------------------------------------------------

fn run_arm(args: &Args) -> Result<(), String> {
    let arm = Arm::parse(&args.arm)?;
    let work = args.work.clone();
    let store_root = work.join("store-a");
    if store_root.exists() {
        fs::remove_dir_all(&store_root)
            .map_err(|err| format!("{}: {err}", store_root.display()))?;
    }
    fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;

    let scope = base_scope();
    let variants = variant_scopes();
    let pages: Vec<(usize, usize, Vec<u8>)> = positions()
        .into_iter()
        .map(|(prefix, layer)| (prefix, layer, recompute_page(&scope, prefix, layer)))
        .collect();
    // What each variant request *would* expect. Only S0's pages are stored, so a
    // cross-scope acceptance is a page whose bytes differ from the request's.
    let variant_pages: Vec<Vec<(usize, usize, Vec<u8>)>> = variants
        .iter()
        .map(|variant| {
            positions()
                .into_iter()
                .map(|(prefix, layer)| (prefix, layer, recompute_page(variant, prefix, layer)))
                .collect()
        })
        .collect();

    let cpu_before = cpu_ms();
    let mut store = Store::init(&store_root, options()).map_err(|err| err.to_string())?;
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;
    let kv = store
        .mkdir(root_id, b"kv", 0o755, 0, 0)
        .map_err(|err| err.to_string())?;
    let build_a_report = build_a(arm, &mut store, kv, &pages, &scope)?;
    let footprint = walk_footprint(&store_root);
    drop(store);

    let open_started = Instant::now();
    let store = open_readonly(&store_root)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let index_started = Instant::now();
    let index = if arm == Arm::M {
        MIndex::load(&store)
    } else {
        MIndex::empty("not applicable")
    };
    let index_load_ms = index_started.elapsed().as_secs_f64() * 1000.0;
    let index_load = json!({
        "entries": index.loads,
        "ms": index_load_ms,
        "note": index.note,
    });
    let rounds = if args.reduced { 4 } else { 8 };
    let mut measurement = measure_a(
        arm,
        &store,
        &pages,
        &variant_pages,
        &scope,
        &variants,
        &index,
        rounds,
    )?;
    measurement.open_ms = open_ms;
    measurement.index_load = index_load;
    drop(store);

    // Metadata loss: physically remove the application metadata, then re-read
    // with an empty index. Only M has metadata to lose.
    if arm == Arm::M {
        let mut store = open_readwrite(&store_root)?;
        let kv_dir = resolve(&store, "/kv").ok_or_else(|| "no /kv".to_owned())?;
        for name in ["index.json", "manifest.json"] {
            if resolve(&store, &format!("/kv/{name}")).is_some() {
                store
                    .unlink(kv_dir, name.as_bytes())
                    .map_err(|err| format!("unlink {name}: {err}"))?;
            }
        }
        drop(store);
    }
    let store = open_readonly(&store_root)?;
    let loss = measure_loss(arm, &store, &pages, &scope, 2)?;
    drop(store);

    let (build_b_report, expected_digest) = build_b(arm, &work)?;
    let (recovery_report, missing_dependency) = measure_b(arm, &work)?;

    let mut probe_json = JsonMap::new();
    let mut tally_totals = ProbeTally::default();
    for (label, tally) in &measurement.probes {
        add_tally(&mut probe_json, label, tally);
        tally_totals.accepted += tally.accepted;
        tally_totals.accepted_wrong += tally.accepted_wrong;
        tally_totals.denied += tally.denied;
        tally_totals.logical_returned += tally.logical_returned;
        tally_totals.backend_bytes += tally.backend_bytes;
        tally_totals.requests += tally.requests;
    }

    let mut probe_summary = JsonMap::new();
    probe_summary.insert("accepted".to_owned(), json!(tally_totals.accepted));
    probe_summary.insert(
        "accepted_wrong_bytes".to_owned(),
        json!(tally_totals.accepted_wrong),
    );
    probe_summary.insert("denied".to_owned(), json!(tally_totals.denied));
    probe_summary.insert(
        "logical_bytes_returned".to_owned(),
        json!(tally_totals.logical_returned),
    );
    probe_summary.insert(
        "backend_bytes".to_owned(),
        json!(tally_totals.backend_bytes),
    );
    probe_summary.insert("requests".to_owned(), json!(tally_totals.requests));

    let phase = json!({
        "schema": PHASE_SCHEMA,
        "arm": arm.as_str(),
        "arm_description": arm.description(),
        "rep": args.rep,
        "order_index": args.order_index,
        "order": args.order,
        "reduced": args.reduced,
        "build_a": {
            "ms": build_a_report.ms,
            "writes": build_a_report.writes,
            "commits": build_a_report.commits,
            "backend_bytes_written": build_a_report.backend_bytes_written,
            "index_entries": build_a_report.index_entries,
            "mode19_bytes": build_a_report.mode19_bytes,
            "mode12_bytes": build_a_report.mode12_bytes,
            "page_bytes": build_a_report.page_bytes,
            "note": build_a_report.note,
            "footprint_files": footprint.0,
            "footprint_bytes": footprint.1,
        },
        "open_ms": measurement.open_ms,
        "index_load": measurement.index_load,
        "a": {
            "compatible": {
                "total": measurement.compatible_total,
                "exact": measurement.compatible_exact,
                "backend_bytes": measurement.compatible_backend_bytes,
                "requests": measurement.compatible_requests,
                "logical_bytes": measurement.compatible_logical,
                "latency": latency_summary(&measurement.compatible),
                "raw_samples_ns": measurement.compatible.iter()
                    .map(|d| d.as_nanos() as u64).collect::<Vec<u64>>(),
            },
            "probes": probe_json,
            "probe_summary": probe_summary,
            "diagnostics": measurement.diagnostics,
            "metadata_loss": loss,
            "recompute": {
                "latency": latency_summary(&measurement.recompute),
                "regenerated_page_exactly": measurement.recompute_exact,
                "bytes": PAGE_BYTES,
                "raw_samples_ns": measurement.recompute.iter()
                    .map(|d| d.as_nanos() as u64).collect::<Vec<u64>>(),
            },
        },
        "b": {
            "build": build_b_report,
            "recovery": recovery_report,
            "missing_dependency": missing_dependency,
        },
        "rss_kib": peak_rss_kib(),
        "cpu_ms": cpu_ms() - cpu_before,
        "expected_digest": expected_digest,
    });

    write_json_atomic(&args.out, &phase)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Assemble
// ---------------------------------------------------------------------------

struct Args {
    command: String,
    arm: String,
    work: PathBuf,
    out: PathBuf,
    log: PathBuf,
    rep: u64,
    order_index: u64,
    order: String,
    reps: u64,
    reduced: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: String::new(),
        arm: "N".to_owned(),
        work: PathBuf::from(".docker-cache/context-advantage"),
        out: PathBuf::from("bench/receipts/context-advantage.json"),
        log: PathBuf::from("bench/receipts/context-advantage.log"),
        rep: 1,
        order_index: 0,
        order: "N,M,W".to_owned(),
        reps: 7,
        reduced: false,
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    let mut positional = Vec::new();
    while index < raw.len() {
        let item = &raw[index];
        if item == "--reduced" {
            args.reduced = true;
            index += 1;
            continue;
        }
        if item == "--force" {
            index += 1;
            continue;
        }
        if let Some(flag) = item.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (flag, None),
            };
            let mut value = |what: &str| -> Result<String, String> {
                if let Some(value) = &inline {
                    return Ok(value.clone());
                }
                let next = raw
                    .get(index + 1)
                    .ok_or_else(|| format!("--{what} needs a value"))?;
                index += 1;
                Ok(next.clone())
            };
            match name {
                "arm" => args.arm = value("arm")?,
                "work" => args.work = PathBuf::from(value("work")?),
                "out" => args.out = PathBuf::from(value("out")?),
                "log" => args.log = PathBuf::from(value("log")?),
                "rep" => {
                    args.rep = value("rep")?
                        .parse()
                        .map_err(|_| "--rep must be an integer".to_owned())?
                }
                "order-index" => {
                    args.order_index = value("order-index")?
                        .parse()
                        .map_err(|_| "--order-index must be an integer".to_owned())?
                }
                "order" => args.order = value("order")?,
                "reps" => {
                    args.reps = value("reps")?
                        .parse()
                        .map_err(|_| "--reps must be an integer".to_owned())?
                }
                other => return Err(format!("unknown flag `--{other}`")),
            }
        } else {
            positional.push(item.clone());
        }
        index += 1;
    }
    args.command = positional.first().cloned().unwrap_or_default();
    if args.command.is_empty() {
        return Err(
            "usage: context_advantage_court run|assemble [--arm N|M|W] [--work DIR] [--out FILE]"
                .to_owned(),
        );
    }
    Ok(args)
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

fn spread(values: &[f64]) -> Json {
    if values.is_empty() {
        return json!({ "n": 0 });
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "n": sorted.len(),
        "min": sorted[0],
        "median": median(&sorted),
        "max": sorted[sorted.len() - 1],
    })
}

fn pick(value: &Json, path: &[&str]) -> Option<f64> {
    let mut cursor = value;
    for part in path {
        cursor = cursor.get(*part)?;
    }
    cursor.as_f64()
}

fn pick_u64(value: &Json, path: &[&str]) -> u64 {
    pick(value, path).unwrap_or(0.0) as u64
}

fn assemble(args: &Args) -> Result<(), String> {
    let mut phases: Vec<Json> = Vec::new();
    let rep_count = args.reps.max(1);
    for rep in 1..=rep_count {
        for arm in ["N", "M", "W"] {
            let path = args
                .work
                .join(format!("rep-{rep:02}"))
                .join(format!("{arm}.json"));
            let text =
                fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            phases.push(serde_json::from_str(&text).map_err(|err| err.to_string())?);
        }
    }

    let mut arms = JsonMap::new();
    for arm in ["N", "M", "W"] {
        let rows: Vec<&Json> = phases
            .iter()
            .filter(|row| row.get("arm").and_then(Json::as_str) == Some(arm))
            .collect();
        let mut metrics = JsonMap::new();

        for (id, path) in [
            ("build_ms", vec!["build_a", "ms"]),
            ("build_commits", vec!["build_a", "commits"]),
            ("build_bytes", vec!["build_a", "backend_bytes_written"]),
            ("footprint_bytes", vec!["build_a", "footprint_bytes"]),
            ("open_ms", vec!["open_ms"]),
            ("index_load_ms", vec!["index_load", "ms"]),
            ("frame_mode19_bytes", vec!["build_a", "mode19_bytes"]),
            ("frame_mode12_bytes", vec!["build_a", "mode12_bytes"]),
            ("rss_kib", vec!["rss_kib"]),
        ] {
            let values: Vec<f64> = rows.iter().filter_map(|row| pick(row, &path)).collect();
            metrics.insert(id.to_owned(), spread(&values));
        }

        let compatible_exact: Vec<f64> = rows
            .iter()
            .map(|row| pick_u64(row, &["a", "compatible", "exact"]) as f64)
            .collect();
        let compatible_total: Vec<f64> = rows
            .iter()
            .map(|row| pick_u64(row, &["a", "compatible", "total"]) as f64)
            .collect();
        metrics.insert("compatible_exact".to_owned(), spread(&compatible_exact));
        metrics.insert("compatible_total".to_owned(), spread(&compatible_total));
        metrics.insert(
            "compatible_backend_bytes".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "compatible", "backend_bytes"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "compatible_requests".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "compatible", "requests"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "hit_p50_us".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "compatible", "latency", "p50_us"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "hit_p95_us".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "compatible", "latency", "p95_us"]))
                    .collect::<Vec<f64>>(),
            ),
        );

        for field in [
            "accepted",
            "accepted_wrong_bytes",
            "denied",
            "logical_bytes_returned",
            "backend_bytes",
            "requests",
        ] {
            metrics.insert(
                format!("probe_{field}"),
                spread(
                    &rows
                        .iter()
                        .filter_map(|row| pick(row, &["a", "probe_summary", field]))
                        .collect::<Vec<f64>>(),
                ),
            );
        }

        metrics.insert(
            "loss_total".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "metadata_loss", "total"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "loss_reused".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "metadata_loss", "reused"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "loss_reused_exact".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "metadata_loss", "reused_exact"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "recompute_p50_us".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "recompute", "latency", "p50_us"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "recovery_logical_bytes".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["b", "recovery", "logical_bytes"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "recovery_backend_bytes".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["b", "recovery", "backend_bytes"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "recovery_wall_ms".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["b", "recovery", "wall_ms"]))
                    .collect::<Vec<f64>>(),
            ),
        );
        metrics.insert(
            "recovery_outputs".to_owned(),
            spread(
                &rows
                    .iter()
                    .filter_map(|row| pick(row, &["b", "recovery", "outputs"]))
                    .collect::<Vec<f64>>(),
            ),
        );

        // Per-variant probe detail, aggregated across repetitions.
        let mut variants = JsonMap::new();
        let labels: Vec<String> = rows
            .first()
            .and_then(|row| {
                row.get("a")
                    .and_then(|a| a.get("probes"))
                    .and_then(Json::as_object)
            })
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        for label in labels {
            let mut detail = JsonMap::new();
            for field in [
                "accepted",
                "accepted_wrong_bytes",
                "denied",
                "logical_bytes_returned",
                "backend_bytes",
            ] {
                let values: Vec<f64> = rows
                    .iter()
                    .filter_map(|row| pick(row, &["a", "probes", &label, field]))
                    .collect();
                detail.insert(field.to_owned(), spread(&values));
            }
            let p50: Vec<f64> = rows
                .iter()
                .filter_map(|row| pick(row, &["a", "probes", &label, "latency", "p50_us"]))
                .collect();
            detail.insert("p50_us".to_owned(), spread(&p50));
            variants.insert(label, Json::Object(detail));
        }

        let recovery_digests: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                row.get("b")
                    .and_then(|b| b.get("recovery"))
                    .and_then(|r| r.get("digest"))
                    .and_then(Json::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let missing: Vec<&Json> = rows
            .iter()
            .filter_map(|row| row.get("b").and_then(|b| b.get("missing_dependency")))
            .collect();
        let closures: Vec<&Json> = rows
            .iter()
            .filter_map(|row| {
                row.get("b")
                    .and_then(|b| b.get("recovery"))
                    .and_then(|r| r.get("causal_closure"))
            })
            .collect();

        let row = json!({
            "description": rows.first().and_then(|row| row.get("arm_description").cloned()).unwrap_or(Json::Null),
            "metrics": Json::Object(metrics),
            "probe_variants": Json::Object(variants),
            "recovery_digests": recovery_digests,
            "missing_dependency": missing,
            "causal_closure": closures,
        });
        arms.insert(arm.to_owned(), row);
    }

    // ---- Checks and claim gates.
    let mut checks: Vec<Json> = Vec::new();
    let mut push = |id: &str, ok: bool, detail: String| {
        checks.push(json!({"id": id, "ok": ok, "detail": detail}));
    };

    let rows_per_arm: Vec<(String, usize)> = ["N", "M", "W"]
        .iter()
        .map(|arm| {
            (
                (*arm).to_owned(),
                phases
                    .iter()
                    .filter(|row| row.get("arm").and_then(Json::as_str) == Some(*arm))
                    .count(),
            )
        })
        .collect();
    push(
        "rows_complete",
        rows_per_arm
            .iter()
            .all(|(_, count)| *count == rep_count as usize),
        format!("rows per arm {rows_per_arm:?}, expected {rep_count}"),
    );

    let orders: BTreeSet<String> = phases
        .iter()
        .filter_map(|row| row.get("order").and_then(Json::as_str).map(str::to_owned))
        .collect();
    // Every arm must occupy every slot at least once across the repetitions,
    // so no arm always runs first or last.
    let mut slot_arms: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for row in &phases {
        let slot = row.get("order_index").and_then(Json::as_u64).unwrap_or(0);
        if let Some(arm) = row.get("arm").and_then(Json::as_str) {
            slot_arms.entry(slot).or_default().insert(arm.to_owned());
        }
    }
    let rotation_ok = if rep_count >= 3 {
        slot_arms.len() == 3 && slot_arms.values().all(|arms| arms.len() == 3)
    } else {
        orders.len() == rep_count as usize
    };
    push(
        "interleaved_orders",
        rotation_ok,
        format!(
            "distinct orders {}; slot->arms {:?}",
            orders.len(),
            slot_arms
                .iter()
                .map(|(slot, arms)| (*slot, arms.iter().cloned().collect::<Vec<_>>()))
                .collect::<BTreeMap<_, _>>()
        ),
    );

    let arm_metric = |arm: &str, key: &str, stat: &str| -> f64 {
        arms.get(arm)
            .and_then(|row| row.get("metrics"))
            .and_then(|metrics| metrics.get(key))
            .and_then(|value| value.get(stat))
            .and_then(Json::as_f64)
            .unwrap_or(0.0)
    };

    let exact_all = ["N", "M", "W"].iter().all(|arm| {
        arm_metric(arm, "compatible_exact", "min") == arm_metric(arm, "compatible_total", "min")
            && arm_metric(arm, "compatible_total", "min") > 0.0
    });
    push(
        "compatible_reuse_exact_every_arm",
        exact_all,
        format!(
            "N {}/{} M {}/{} W {}/{} (min across reps)",
            arm_metric("N", "compatible_exact", "min"),
            arm_metric("N", "compatible_total", "min"),
            arm_metric("M", "compatible_exact", "min"),
            arm_metric("M", "compatible_total", "min"),
            arm_metric("W", "compatible_exact", "min"),
            arm_metric("W", "compatible_total", "min"),
        ),
    );

    let n_accepts = arm_metric("N", "probe_accepted", "min") > 0.0;
    push(
        "n_probe_is_live",
        n_accepts,
        format!(
            "N accepted {} probes (min across reps); a zero would make the probe vacuous",
            arm_metric("N", "probe_accepted", "min")
        ),
    );

    let w_accepts = arm_metric("W", "probe_accepted", "max");
    push(
        "w_zero_incompatible_accepted",
        w_accepts == 0.0,
        format!("W accepted {w_accepts} across every rep (max)"),
    );
    let w_denied_bytes = arm_metric("W", "probe_logical_bytes_returned", "max");
    push(
        "w_zero_bytes_on_denial",
        w_denied_bytes == 0.0,
        format!("W returned {w_denied_bytes} logical bytes on probes (max)"),
    );
    let m_accepts = arm_metric("M", "probe_accepted", "max");
    push(
        "m_incompatible_accepted",
        m_accepts == 0.0,
        format!("M (governing arm, full value validation) accepted {m_accepts} (max)"),
    );

    for arm in ["N", "M", "W"] {
        let bytes = arm_metric(arm, "probe_logical_bytes_returned", "max");
        // N has no scope to validate against, so it is expected to return
        // bytes; the check that matters is that a *refusal* emits nothing.
        push(
            &format!("refusal_emits_nothing_{arm}"),
            bytes == 0.0 || arm == "N",
            format!("{arm} logical bytes returned on probes: {bytes}"),
        );
    }

    let expected_digest = phases
        .first()
        .and_then(|row| row.get("expected_digest"))
        .and_then(Json::as_str)
        .map(str::to_owned);
    let recovery_exact = ["N", "M", "W"].iter().all(|arm| {
        arms.get(*arm)
            .and_then(|row| row.get("recovery_digests"))
            .and_then(Json::as_array)
            .map(|digests| {
                digests.len() == rep_count as usize
                    && digests
                        .iter()
                        .all(|digest| digest.as_str().map(str::to_owned) == expected_digest)
            })
            .unwrap_or(false)
    });
    push(
        "recovery_outputs_exact_every_arm",
        recovery_exact,
        format!(
            "expected digest {:?}; all arms matched in every rep: {recovery_exact}",
            expected_digest
        ),
    );

    let w_closure_verified = arms
        .get("W")
        .and_then(|row| row.get("causal_closure"))
        .and_then(Json::as_array)
        .map(|rows| {
            rows.iter().all(|row| {
                row.get("verified").and_then(Json::as_bool) == Some(true)
                    && row.get("parents").and_then(Json::as_u64) == Some(2)
            })
        })
        .unwrap_or(false);
    push(
        "w_causal_closure_verified",
        w_closure_verified,
        "W resolved the derived artifact's two declared parents to live objects".to_owned(),
    );

    let missing_closed = ["N", "M", "W"].iter().all(|arm| {
        arms.get(*arm)
            .and_then(|row| row.get("missing_dependency"))
            .and_then(Json::as_array)
            .map(|rows| {
                rows.iter()
                    .all(|row| row.get("failed_closed").and_then(Json::as_bool) == Some(true))
            })
            .unwrap_or(false)
    });
    push(
        "missing_dependency_fails_closed",
        missing_closed,
        "every arm refused after one activation was removed".to_owned(),
    );

    let recompute_exact = ["N", "M", "W"].iter().all(|arm| {
        phases
            .iter()
            .filter(|row| row.get("arm").and_then(Json::as_str) == Some(*arm))
            .all(|row| {
                row.get("a")
                    .and_then(|a| a.get("recompute"))
                    .and_then(|r| r.get("regenerated_page_exactly"))
                    .and_then(Json::as_bool)
                    == Some(true)
            })
    });
    push(
        "recompute_regenerates_the_page",
        recompute_exact,
        "the measured recompute kernel reproduced the stored page bytes".to_owned(),
    );

    // ---- Declared tolerance (contract §3) — evaluated, never widened.
    let n_p50 = arm_metric("N", "hit_p50_us", "median");
    let n_p95 = arm_metric("N", "hit_p95_us", "median");
    let n_bytes = arm_metric("N", "compatible_backend_bytes", "median")
        / arm_metric("N", "compatible_total", "median").max(1.0);
    let w_p50 = arm_metric("W", "hit_p50_us", "median");
    let w_p95 = arm_metric("W", "hit_p95_us", "median");
    let w_bytes = arm_metric("W", "compatible_backend_bytes", "median")
        / arm_metric("W", "compatible_total", "median").max(1.0);
    let m_p50 = arm_metric("M", "hit_p50_us", "median");
    let m_p95 = arm_metric("M", "hit_p95_us", "median");
    let m_bytes = arm_metric("M", "compatible_backend_bytes", "median")
        / arm_metric("M", "compatible_total", "median").max(1.0);
    let c1 = w_p50 <= n_p50 * TOL_P50;
    let c2 = w_p95 <= n_p95 * TOL_P95;
    let c3 = w_bytes <= n_bytes * TOL_BYTES;
    let fast_path = json!({
        "declared": {
            "p50_ratio_max": TOL_P50,
            "p95_ratio_max": TOL_P95,
            "bytes_ratio_max": TOL_BYTES,
        },
        "observed": {
            "N": {"p50_us": n_p50, "p95_us": n_p95, "backend_bytes_per_request": n_bytes},
            "M": {"p50_us": m_p50, "p95_us": m_p95, "backend_bytes_per_request": m_bytes},
            "W": {"p50_us": w_p50, "p95_us": w_p95, "backend_bytes_per_request": w_bytes},
        },
        "ratios": {
            "W_over_N_p50": w_p50 / n_p50.max(1e-9),
            "W_over_N_p95": w_p95 / n_p95.max(1e-9),
            "W_over_N_bytes": w_bytes / n_bytes.max(1e-9),
            "M_over_N_p50": m_p50 / n_p50.max(1e-9),
            "M_over_N_p95": m_p95 / n_p95.max(1e-9),
            "M_over_N_bytes": m_bytes / n_bytes.max(1e-9),
        },
    });

    let mut claim_gates: Vec<Json> = Vec::new();
    let gate =
        |id: &str, holds: bool, detail: String| json!({"id": id, "holds": holds, "detail": detail});
    claim_gates.push(gate(
        "f1_w_rejects_cross_scope",
        w_accepts == 0.0 && w_denied_bytes == 0.0,
        format!("W accepted {w_accepts} and returned {w_denied_bytes} bytes"),
    ));
    claim_gates.push(gate(
        "f2_probe_is_live",
        n_accepts,
        format!(
            "N accepted {} (min)",
            arm_metric("N", "probe_accepted", "min")
        ),
    ));
    let m_loss_reused = arm_metric("M", "loss_reused", "max");
    claim_gates.push(gate(
        "f3_baseline_does_not_tie",
        !(m_accepts == 0.0 && m_loss_reused > 0.0),
        format!(
            "M accepted {m_accepts} incompatible and reused {m_loss_reused} after metadata loss; \
             a tie here refutes the claim"
        ),
    ));
    let w_recovery = arm_metric("W", "recovery_logical_bytes", "median");
    let m_recovery = arm_metric("M", "recovery_logical_bytes", "median");
    claim_gates.push(gate(
        "f4_recovery_does_not_materialize_more",
        w_recovery <= m_recovery,
        format!("W materialized {w_recovery} B, M {m_recovery} B for the identical output set"),
    ));
    claim_gates.push(gate(
        "c1_fast_path_p50",
        c1,
        format!(
            "W p50 {w_p50:.1} us vs N {n_p50:.1} us = {:.2}x (declared <= {TOL_P50}x)",
            w_p50 / n_p50.max(1e-9)
        ),
    ));
    claim_gates.push(gate(
        "c2_fast_path_p95",
        c2,
        format!(
            "W p95 {w_p95:.1} us vs N {n_p95:.1} us = {:.2}x (declared <= {TOL_P95}x)",
            w_p95 / n_p95.max(1e-9)
        ),
    ));
    claim_gates.push(gate(
        "c3_fast_path_bytes",
        c3,
        format!(
            "W {w_bytes:.0} B/request vs N {n_bytes:.0} B/request = {:.2}x (declared <= {TOL_BYTES}x)",
            w_bytes / n_bytes.max(1e-9)
        ),
    ));

    let claim_holds = claim_gates
        .iter()
        .all(|gate| gate.get("holds").and_then(Json::as_bool) == Some(true));

    // ---- Measured losses. Published, not buried.
    let mut losses: Vec<String> = Vec::new();
    if w_p50 > n_p50 {
        losses.push(format!(
            "W fast-path p50 is {:.2}x N ({:.1} vs {:.1} us): the context plane does not \
             preserve a raw-read critical path on this workload",
            w_p50 / n_p50.max(1e-9),
            w_p50,
            n_p50
        ));
    }
    if m_p50 > n_p50 {
        losses.push(format!(
            "M fast-path p50 is {:.2}x N ({:.1} vs {:.1} us): the penalty is not unique to W -- \
             any compressed representation pays it",
            m_p50 / n_p50.max(1e-9),
            m_p50,
            n_p50
        ));
    }
    if w_p50 > m_p50 {
        losses.push(format!(
            "M is {:.2}x faster than W on the fast path ({:.1} vs {:.1} us)",
            w_p50 / m_p50.max(1e-9),
            m_p50,
            w_p50
        ));
    }
    {
        let w_backend = arm_metric("W", "recovery_backend_bytes", "median");
        let m_backend = arm_metric("M", "recovery_backend_bytes", "median");
        if w_backend > m_backend {
            losses.push(format!(
                "W reads {:.2}x M's backend bytes for the identical recovery output set \
                 ({:.0} vs {:.0} B): the shipped per-tensor restore re-loads the checkpoint \
                 manifest on every call",
                w_backend / m_backend.max(1e-9),
                w_backend,
                m_backend
            ));
        }
        let w_wall = arm_metric("W", "recovery_wall_ms", "median");
        let m_wall = arm_metric("M", "recovery_wall_ms", "median");
        if w_wall > m_wall {
            losses.push(format!(
                "W recovery wall is {:.2}x M ({:.3} vs {:.3} ms)",
                w_wall / m_wall.max(1e-9),
                w_wall,
                m_wall
            ));
        }
    }
    {
        let n_accepted = arm_metric("N", "probe_accepted", "max");
        if n_accepted > 0.0 {
            losses.push(format!(
                "N accepted {n_accepted} of 56 cross-scope requests and returned the wrong \
                 bytes for every one: the floor is unsafe by construction"
            ));
        }
        let m_loss = arm_metric("M", "loss_reused", "max");
        if m_loss == 0.0 {
            losses.push(
                "M reused nothing after its metadata database was lost (0 of 16): its safe-reuse \
                 capability is tied to that database"
                    .to_owned(),
            );
        }
        let m_probe_bytes = arm_metric("M", "probe_backend_bytes", "median");
        if m_probe_bytes > 0.0 {
            losses.push(format!(
                "M read {m_probe_bytes:.0} backend bytes to refuse cross-scope requests whose key \
                 it matched: the three dimensions its index key omits must be validated from the \
                 payload"
            ));
        }
    }

    // ---- The isolated context-plane overhead: mode 19 vs mode 12 decode of the
    // same page in memory (W only). Reported, not a gate.
    let mut decode12: Vec<f64> = Vec::new();
    let mut decode19: Vec<f64> = Vec::new();
    for row in &phases {
        if row.get("arm").and_then(Json::as_str) != Some("W") {
            continue;
        }
        let diag = row
            .get("a")
            .and_then(|a| a.get("diagnostics"))
            .and_then(|d| d.get("decode_only"));
        if let Some(value) = diag
            .and_then(|d| d.get("mode12"))
            .and_then(|m| m.get("p50_us"))
            .and_then(Json::as_f64)
        {
            decode12.push(value);
        }
        if let Some(value) = diag
            .and_then(|d| d.get("mode19"))
            .and_then(|m| m.get("p50_us"))
            .and_then(Json::as_f64)
        {
            decode19.push(value);
        }
    }
    let d12 = median(&decode12);
    let d19 = median(&decode19);
    let provenance_overhead = json!({
        "mode12_decode_p50_us": d12,
        "mode19_decode_p50_us": d19,
        "ratio": if d12 > 0.0 { d19 / d12 } else { 0.0 },
        "note": "in-memory decode of the same page, same payload codec; isolates the \
                 provenance descriptor from I/O and from the payload codec",
    });

    let checks_ok = checks
        .iter()
        .all(|check| check.get("ok").and_then(Json::as_bool) == Some(true));

    let receipt = json!({
        "schema": SCHEMA,
        "generated_utc": std::env::var("WEFT_CONTEXT_ADVANTAGE_STAMP").ok(),
        "label": std::env::var("WEFT_CONTEXT_ADVANTAGE_LABEL").ok(),
        "contract": CONTRACT,
        "methodology": {
            "arms": {
                "N": "backend alone: plain durable objects, no metadata",
                "M": "backend + strong application metadata: zstd-3 blob, canonical manifest, \
                      key->value index on the brief's five dimensions plus position, full \
                      value validation before reuse, TTL/eviction, own subset selection",
                "W": "backend + WeftFS context plane: provenance-bound KV pages (mode 19), \
                      capability-scoped reads, weft.checkpoint.v1 manifests, causal closure",
            },
            "held_constant": [
                "BackendKind::Local data plane",
                "commit protocol and durability barrier",
                "chunk size 65,536 B",
                "no decoded-object cache, no single-flight",
                "single process, single thread, one request at a time",
                "identical logical objects, names, counts and demanded output set",
                "fixed store clock",
                "Docker court limits 3g/3g/1024 pids/4 cpus, never raised",
            ],
            "declared_tolerance": {
                "p50_ratio_max": TOL_P50,
                "p95_ratio_max": TOL_P95,
                "bytes_ratio_max": TOL_BYTES,
                "declared_before_measurement": true,
            },
            "repetitions": rep_count,
            "reduced": args.reduced,
            "raw_samples": "every repetition's phase JSON is committed beside the receipt",
        },
        "environment": environment(),
        "fixture": {
            "page_rows": PAGE_ROWS,
            "head_elements": HEAD_ELEMS,
            "page_bytes": PAGE_BYTES,
            "prefixes": PREFIXES,
            "layers": LAYERS,
            "pages_written": positions().len(),
            "probe_count": positions().len() * 7,
            "base_scope": base_scope().to_json(),
            "variants": variant_scopes().iter().map(Scope::to_json).collect::<Vec<Json>>(),
            "tensors": TENSORS,
            "tensor_bytes": TENSOR_BYTES,
            "needed": NEEDED.to_vec(),
            "activation_bytes": ACTIVATION_BYTES,
            "derived_bytes": DERIVED_BYTES,
            "recompute": "scalar reference kernel that regenerates the page; not an optimized prefill",
        },
        "arms": Json::Object(arms),
        "fast_path": fast_path,
        "provenance_overhead": provenance_overhead,
        "losses": losses,
        "raw": phases,
        "checks": checks,
        "claim_gates": claim_gates,
        "claim_holds": claim_holds,
        "result": if checks_ok { "pass" } else { "fail" },
        "result_note": "`result` reports measurement validity (every row present, every arm exact, \
                        every declared check met). `claim_holds` reports whether the pre-registered \
                        capability claim survived; the two are deliberately separate.",
        "not_measured": [
            "fleet, multi-host, multi-writer and media power-loss durability",
            "GPU or production prefill: recompute_ms is a scalar reference kernel",
            "an application baseline that embeds a canonical scope record in every blob and \
             rebuilds its index by scanning (available, untested)",
            "live cloud backends; this court is the local POSIX data plane",
            "timing, size or access-pattern privacy of the salt",
        ],
        "scope": "single host, one backend, OOM-protected Docker court. Not a cloud, fleet, media \
                  or power-loss claim.",
    });

    write_json_atomic(&args.out, &receipt)?;
    if !args.log.as_os_str().is_empty() {
        if let Some(parent) = args.log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut log = File::create(&args.log).map_err(|err| err.to_string())?;
        for check in &checks {
            writeln!(
                log,
                "CHECK {} {} {}",
                check.get("id").and_then(Json::as_str).unwrap_or("?"),
                if check.get("ok").and_then(Json::as_bool) == Some(true) {
                    "pass"
                } else {
                    "FAIL"
                },
                check.get("detail").and_then(Json::as_str).unwrap_or("")
            )
            .map_err(|err| err.to_string())?;
        }
        for gate in &claim_gates {
            writeln!(
                log,
                "GATE {} {} {}",
                gate.get("id").and_then(Json::as_str).unwrap_or("?"),
                if gate.get("holds").and_then(Json::as_bool) == Some(true) {
                    "holds"
                } else {
                    "fails"
                },
                gate.get("detail").and_then(Json::as_str).unwrap_or("")
            )
            .map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = (|| -> Result<(), String> {
        let args = parse_args()?;
        match args.command.as_str() {
            "run" => run_arm(&args),
            "assemble" => assemble(&args),
            other => Err(format!("unknown command `{other}` (expected run|assemble)")),
        }
    })() {
        eprintln!("context_advantage_court: {error}");
        std::process::exit(1);
    }
}
