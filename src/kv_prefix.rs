//! KV prefix cache: canonical prefix records, chain-aware storage, and a
//! reuse-vs-recompute decision (subphase T2).
//!
//! # What this is
//!
//! A **prefix record** is a canonical `weft.kv.prefix.v1` WCB record that names
//! one reusable KV cache prefix: the model identity, the tokenizer identity,
//! the attention configuration identity, the exact token range, the ordered
//! per-layer [`mode-12`](weftcodec::kv_cache) page objects, the token-hash
//! chain root, and a `pinned` flag. The record is stored as a file under
//! `/kv/prefixes/<name>`; the pages are ordinary mode-12 `KV_CACHE` objects
//! under `/kv/pages/p-<descriptor-id>` with the canonical
//! `weft.kv.capability.v1` descriptors from [`crate::kv_capability`].
//!
//! # Compatibility is exact, and mismatch fails closed
//!
//! A stored prefix may be reused for a request only when **every**
//! compatibility field matches: tenant, model, tokenizer hash, attention
//! configuration hash, dtype, head count, head dim, layer count, and page
//! rows. In addition the request's token-hash chain root at the stored length
//! must equal the stored root, and the stored prefix must start at token 0.
//! There is no "closest match" and no cross-model reuse. A lookup that finds
//! nothing compatible returns [`KvLookup::Recompute`] with a typed list of
//! reasons; the caller then recomputes. It never silently uses a prefix.
//!
//! # The decision
//!
//! `T_reuse = lookup + transfer + decode + placement`, with the lookup,
//! transfer and placement terms **declared** in `weft.kv.prefill.v1` and the
//! decode term taken from the existing measured/declared KV_CACHE cost row.
//! `T_recompute` is a **declared** prefill model (fixed plus per-token
//! coefficients) loaded from the same calibration document. Reuse is chosen
//! only when it is strictly cheaper. Both estimates, every component, and the
//! reason are recorded in [`KvReuseDecision`].
//!
//! **The prefill model is not measured against a real inference engine.** The
//! coefficients describe a declared tokens-per-second rate; see
//! `docs/KV-PREFIX.md`. No vLLM/SGLang integration and no time-to-first-token
//! claim is made anywhere.
//!
//! # Dedup
//!
//! A longer prefix names a parent. All parent page objects are reused
//! verbatim; only the new pages are written. Two prefixes that produce the
//! same page descriptor and bytes share the same page object through a
//! content-addressed page name. A parent may only be extended at a page
//! boundary; a partial last page is never silently re-used.
//!
//! # GC
//!
//! `kv gc` is a logical sweep over the KV namespace: pages referenced by a
//! live prefix record (or held by an open-handle pin) are kept; KV pages that
//! no prefix names are removed from current state. Physical blob reclamation
//! is still the store's own mark-and-sweep GC. A pinned prefix cannot be
//! deleted, so its pages are always referenced by a live prefix.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use weftcodec::frame::Frame;
use weftcodec::kv_cache::{self, DecodedKvPage, KvCacheInput, KvDtype};
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::auth::Principal;
use crate::context::CapabilityContext;
use crate::cost_model::{CostClass, CostModelKind, CostTable};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::kv_capability::{
    KvDescriptor, KvPageWrite, ATTR_KV_DESCRIPTOR, ATTR_KV_DESCRIPTOR_ID, KV_DENIED,
};
use crate::state::{ObjectKind, ObjectRecord};
use crate::store::Store;

/// Canonical prefix record schema.
pub const KV_PREFIX_SCHEMA: &str = "weft.kv.prefix.v1";
/// Prefix record identity domain.
pub const DOMAIN_KV_PREFIX: &str = "weft/kv/prefix/v1";
/// Token-hash chain domain.
pub const DOMAIN_KV_TOKEN_CHAIN: &str = "weft/kv/token-chain/v1";
/// Attribute holding the prefix id.
pub const ATTR_KV_PREFIX: &str = "weft.kv.prefix";
/// Attribute holding the pinned flag.
pub const ATTR_KV_PREFIX_PINNED: &str = "weft.kv.pinned";
/// Attribute holding the prefix tenant.
pub const ATTR_KV_PREFIX_TENANT: &str = "weft.kv.tenant";
/// Attribute holding the prefix model identity.
pub const ATTR_KV_PREFIX_MODEL: &str = "weft.kv.model";
/// Attribute holding the prefix token length.
pub const ATTR_KV_PREFIX_TOKENS: &str = "weft.kv.tokens";
/// Namespace directory names.
pub const KV_DIR: &str = "kv";
pub const KV_PREFIX_DIR: &str = "prefixes";
pub const KV_PAGE_DIR: &str = "pages";
/// Bound on a single prefix's token length (bounds chain work and page count).
pub const MAX_KV_PREFIX_TOKENS: u64 = 1_048_576;
/// Bound on the number of reasoning strings a lookup returns.
const MAX_LOOKUP_REASONS: usize = 16;

fn hex(hash: Hash32) -> String {
    hash.to_hex()
}

fn parse_hex(text: &str, what: &str) -> Result<Hash32> {
    Hash32::from_hex(text).map_err(|_| Error::corrupt(format!("{what} is not a 64-hex digest")))
}

// ---------------------------------------------------------------------------
// Token-hash chain
// ---------------------------------------------------------------------------

/// The prefix token-hash chain.
///
/// `root_0 = H(domain, "")`, `root_{i+1} = H(domain, root_i || u64le(token_i))`.
/// The chain binds a prefix to the exact token sequence, not just its length:
/// two different prompts of the same length have different roots.
pub fn kv_token_chain_root(tokens: &[u64]) -> Hash32 {
    let mut root = identity::h(DOMAIN_KV_TOKEN_CHAIN, b"");
    for token in tokens {
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(root.as_bytes());
        data.extend_from_slice(&token.to_le_bytes());
        root = identity::h(DOMAIN_KV_TOKEN_CHAIN, &data);
    }
    root
}

/// Tokenizer identity domain for the CLI's declared identity strings.
pub const DOMAIN_KV_TOKENIZER: &str = "weft/kv/tokenizer/v1";
/// Attention configuration identity domain.
pub const DOMAIN_KV_ATTENTION: &str = "weft/kv/attention/v1";

/// Hash a declared tokenizer identity string.
pub fn kv_tokenizer_hash(name: &str) -> Hash32 {
    identity::h(DOMAIN_KV_TOKENIZER, name.as_bytes())
}

/// Hash a declared attention configuration string.
pub fn kv_attention_hash(text: &str) -> Hash32 {
    identity::h(DOMAIN_KV_ATTENTION, text.as_bytes())
}

/// Logical byte length of one page.
pub fn kv_page_len(
    page_index: u64,
    page_rows: u64,
    tokens: u64,
    head_count: u32,
    head_dim: u32,
    dtype: &str,
) -> Result<u64> {
    let dtype = KvDtype::from_name(dtype).map_err(Error::from)?;
    let page_count = tokens.div_ceil(page_rows);
    page_element_bytes(
        page_index,
        page_count,
        page_rows,
        tokens,
        head_count,
        head_dim,
        dtype.word_bytes(),
    )
}

/// Deterministic synthetic token ids for a declared corpus.
///
/// xorshift64* seeded by `seed`, mapped into a 32,000-token vocabulary. This is
/// a fixture generator, not a tokenizer.
pub fn kv_token_ids(seed: u64, count: u64) -> Vec<u64> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(count.min(MAX_KV_PREFIX_TOKENS) as usize);
    for _ in 0..count.min(MAX_KV_PREFIX_TOKENS) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(state % 32_000);
    }
    out
}

/// Deterministic synthetic page bytes (all BF16/FP16/FP32 bit patterns legal).
pub fn kv_page_bytes(seed: u64, layer: u32, index: u64, len: usize) -> Vec<u8> {
    let mut state = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(layer).rotate_left(17) ^ index.rotate_left(41))
        | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// Everything needed to create a prefix. The token range is the token slice
/// passed to [`Store::kv_prefix_put`]; this struct carries identity only.
#[derive(Clone, Debug)]
pub struct KvPrefixSpec {
    pub tenant: String,
    pub model: String,
    pub tokenizer_hash: Hash32,
    pub attention_hash: Hash32,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub layer_count: u32,
    /// Tokens per page (the last page may be shorter).
    pub page_rows: u64,
    /// A pinned prefix cannot be deleted, so its pages are always live.
    pub pinned: bool,
}

/// A stored canonical prefix record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvPrefixRecord {
    pub tenant: String,
    pub model: String,
    pub tokenizer_hash: Hash32,
    pub attention_hash: Hash32,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub layer_count: u32,
    pub page_rows: u64,
    /// Pages per layer.
    pub page_count: u64,
    pub position_start: u64,
    pub position_end: u64,
    pub token_chain_root: Hash32,
    pub parent: Option<Hash32>,
    /// `pages[layer][page]` object ids, ordered.
    pub pages: Vec<Vec<Hash32>>,
    pub pinned: bool,
    /// Logical bytes of every page in this prefix (including reused pages).
    pub page_bytes: u64,
    /// Logical bytes of the page objects this prefix itself wrote.
    pub stored_page_bytes: u64,
    /// Page slots reused from the parent (already counted in `page_bytes`).
    pub reused_pages: u64,
}

impl KvPrefixRecord {
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(DOMAIN_KV_PREFIX, &self.canonical_bytes()?))
    }

    pub fn tokens(&self) -> u64 {
        self.position_end
    }

    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("schema".to_owned(), Value::Str(KV_PREFIX_SCHEMA.to_owned()));
        map.insert("tenant".to_owned(), Value::Str(self.tenant.clone()));
        map.insert("model".to_owned(), Value::Str(self.model.clone()));
        map.insert(
            "tokenizer_hash".to_owned(),
            Value::Str(hex(self.tokenizer_hash)),
        );
        map.insert(
            "attention_hash".to_owned(),
            Value::Str(hex(self.attention_hash)),
        );
        map.insert("dtype".to_owned(), Value::Str(self.dtype.clone()));
        map.insert(
            "head_count".to_owned(),
            Value::U64(u64::from(self.head_count)),
        );
        map.insert("head_dim".to_owned(), Value::U64(u64::from(self.head_dim)));
        map.insert(
            "layer_count".to_owned(),
            Value::U64(u64::from(self.layer_count)),
        );
        map.insert("page_rows".to_owned(), Value::U64(self.page_rows));
        map.insert("page_count".to_owned(), Value::U64(self.page_count));
        map.insert("position_start".to_owned(), Value::U64(self.position_start));
        map.insert("position_end".to_owned(), Value::U64(self.position_end));
        map.insert(
            "token_chain_root".to_owned(),
            Value::Str(hex(self.token_chain_root)),
        );
        map.insert(
            "parent".to_owned(),
            match self.parent {
                Some(id) => Value::Str(hex(id)),
                None => Value::Null,
            },
        );
        map.insert(
            "pages".to_owned(),
            Value::Array(
                self.pages
                    .iter()
                    .map(|layer| {
                        Value::Array(layer.iter().map(|id| Value::Str(hex(*id))).collect())
                    })
                    .collect(),
            ),
        );
        map.insert("pinned".to_owned(), Value::Bool(self.pinned));
        map.insert("page_bytes".to_owned(), Value::U64(self.page_bytes));
        map.insert(
            "stored_page_bytes".to_owned(),
            Value::U64(self.stored_page_bytes),
        );
        map.insert("reused_pages".to_owned(), Value::U64(self.reused_pages));
        Value::Map(map)
    }

    /// Canonical WCB bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// Strict decode: exact key set, canonical re-encoding, and structural
    /// consistency (pages per layer, position/page arithmetic).
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        const KEYS: usize = 20;
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let Value::Map(map) = value else {
            return Err(Error::corrupt("kv prefix is not a map"));
        };
        let mut seen = 0usize;
        for key in map.keys() {
            match key.as_str() {
                "schema" | "tenant" | "model" | "tokenizer_hash" | "attention_hash" | "dtype"
                | "head_count" | "head_dim" | "layer_count" | "page_rows" | "page_count"
                | "position_start" | "position_end" | "token_chain_root" | "parent" | "pages"
                | "pinned" | "page_bytes" | "stored_page_bytes" | "reused_pages" => seen += 1,
                other => return Err(Error::corrupt(format!("unknown kv prefix key `{other}`"))),
            }
        }
        if seen != KEYS {
            return Err(Error::corrupt("kv prefix is missing a field"));
        }
        let text = |key: &str| -> Result<String> {
            match map.get(key) {
                Some(Value::Str(text)) => Ok(text.clone()),
                _ => Err(Error::corrupt(format!("kv prefix `{key}` is not text"))),
            }
        };
        let number = |key: &str| -> Result<u64> {
            match map.get(key) {
                Some(Value::U64(value)) => Ok(*value),
                _ => Err(Error::corrupt(format!("kv prefix `{key}` is not u64"))),
            }
        };
        let u32_of = |key: &str| -> Result<u32> {
            u32::try_from(number(key)?)
                .map_err(|_| Error::corrupt(format!("kv prefix `{key}` does not fit u32")))
        };
        if text("schema")? != KV_PREFIX_SCHEMA {
            return Err(Error::corrupt("unknown kv prefix schema"));
        }
        let parent = match map.get("parent") {
            Some(Value::Null) => None,
            Some(Value::Str(text)) => Some(parse_hex(text, "kv prefix parent")?),
            _ => return Err(Error::corrupt("kv prefix `parent` is not null or hex")),
        };
        let pinned = match map.get("pinned") {
            Some(Value::Bool(value)) => *value,
            _ => return Err(Error::corrupt("kv prefix `pinned` is not a bool")),
        };
        let pages = match map.get("pages") {
            Some(Value::Array(layers)) => layers
                .iter()
                .map(|layer| match layer {
                    Value::Array(ids) => ids
                        .iter()
                        .map(|id| match id {
                            Value::Str(text) => parse_hex(text, "kv prefix page id"),
                            _ => Err(Error::corrupt("kv prefix page id is not text")),
                        })
                        .collect::<Result<Vec<Hash32>>>(),
                    _ => Err(Error::corrupt("kv prefix layer is not an array")),
                })
                .collect::<Result<Vec<Vec<Hash32>>>>()?,
            _ => return Err(Error::corrupt("kv prefix `pages` is not an array")),
        };
        let record = KvPrefixRecord {
            tenant: text("tenant")?,
            model: text("model")?,
            tokenizer_hash: parse_hex(&text("tokenizer_hash")?, "kv prefix tokenizer_hash")?,
            attention_hash: parse_hex(&text("attention_hash")?, "kv prefix attention_hash")?,
            dtype: text("dtype")?,
            head_count: u32_of("head_count")?,
            head_dim: u32_of("head_dim")?,
            layer_count: u32_of("layer_count")?,
            page_rows: number("page_rows")?,
            page_count: number("page_count")?,
            position_start: number("position_start")?,
            position_end: number("position_end")?,
            token_chain_root: parse_hex(&text("token_chain_root")?, "kv prefix token_chain_root")?,
            parent,
            pages,
            pinned,
            page_bytes: number("page_bytes")?,
            stored_page_bytes: number("stored_page_bytes")?,
            reused_pages: number("reused_pages")?,
        };
        record.validate_shape()?;
        if record.canonical_bytes()? != bytes {
            return Err(Error::corrupt("kv prefix is not canonical"));
        }
        Ok(record)
    }

    /// Structural invariants that must hold for any record, stored or built.
    fn validate_shape(&self) -> Result<()> {
        if self.page_rows == 0 {
            return Err(Error::corrupt("kv prefix page_rows must be positive"));
        }
        if self.position_start != 0 {
            return Err(Error::corrupt(
                "kv prefix must start at token 0 (position_start != 0)",
            ));
        }
        if self.position_end == 0 || self.position_end > MAX_KV_PREFIX_TOKENS {
            return Err(Error::corrupt("kv prefix token length is out of bounds"));
        }
        if self.layer_count == 0 || self.layer_count > 1_024 {
            return Err(Error::corrupt("kv prefix layer_count is out of bounds"));
        }
        if self.pages.len() != self.layer_count as usize {
            return Err(Error::corrupt(
                "kv prefix page layers do not match layer_count",
            ));
        }
        let expected_pages = self.position_end.div_ceil(self.page_rows);
        if self.page_count != expected_pages {
            return Err(Error::corrupt(
                "kv prefix page_count does not match position_end/page_rows",
            ));
        }
        for layer in &self.pages {
            if layer.len() as u64 != self.page_count {
                return Err(Error::corrupt(
                    "kv prefix layer page count does not match page_count",
                ));
            }
            if layer.iter().any(|id| *id == Hash32::ZERO) {
                return Err(Error::corrupt("kv prefix has a zero page id"));
            }
        }
        if self.position_end <= self.page_rows && self.page_count != 1 {
            return Err(Error::corrupt("kv prefix page arithmetic is inconsistent"));
        }
        if self.reused_pages > self.page_count * u64::from(self.layer_count) {
            return Err(Error::corrupt(
                "kv prefix reused_pages exceeds its page slots",
            ));
        }
        if self.stored_page_bytes > self.page_bytes {
            return Err(Error::corrupt(
                "kv prefix stored_page_bytes exceeds page_bytes",
            ));
        }
        Ok(())
    }

    /// The exact compatibility comparison. `Err` names the first mismatched
    /// field; the caller never falls back to a partial match.
    pub fn compatibility(&self, request: &KvPrefixRequest) -> std::result::Result<(), String> {
        if self.tenant != request.tenant {
            return Err(format!(
                "tenant mismatch: stored `{}` vs requested `{}`",
                self.tenant, request.tenant
            ));
        }
        if self.model != request.model {
            return Err(format!(
                "model mismatch: stored `{}` vs requested `{}`",
                self.model, request.model
            ));
        }
        if self.tokenizer_hash != request.tokenizer_hash {
            return Err("tokenizer hash mismatch".to_owned());
        }
        if self.attention_hash != request.attention_hash {
            return Err("attention configuration hash mismatch".to_owned());
        }
        if self.dtype != request.dtype {
            return Err(format!(
                "dtype mismatch: stored `{}` vs requested `{}`",
                self.dtype, request.dtype
            ));
        }
        if self.head_count != request.head_count {
            return Err(format!(
                "head count mismatch: stored {} vs requested {}",
                self.head_count, request.head_count
            ));
        }
        if self.head_dim != request.head_dim {
            return Err(format!(
                "head dim mismatch: stored {} vs requested {}",
                self.head_dim, request.head_dim
            ));
        }
        if self.layer_count != request.layer_count {
            return Err(format!(
                "layer count mismatch: stored {} vs requested {}",
                self.layer_count, request.layer_count
            ));
        }
        if self.page_rows != request.page_rows {
            return Err(format!(
                "page rows mismatch: stored {} vs requested {}",
                self.page_rows, request.page_rows
            ));
        }
        if i64::try_from(self.position_end).is_err() {
            return Err("stored prefix length does not fit i64".to_owned());
        }
        let requested_tokens = request.tokens.len() as u64;
        if self.position_end > requested_tokens {
            return Err(format!(
                "stored prefix is longer than the request ({} > {})",
                self.position_end, requested_tokens
            ));
        }
        let prefix_root = kv_token_chain_root(&request.tokens[..self.position_end as usize]);
        if prefix_root != self.token_chain_root {
            return Err(format!(
                "token-chain mismatch at {} tokens",
                self.position_end
            ));
        }
        Ok(())
    }
}

/// A reuse request: the identity a caller would use to prefill, and the exact
/// token sequence it would prefill.
#[derive(Clone, Debug)]
pub struct KvPrefixRequest {
    pub tenant: String,
    pub model: String,
    pub tokenizer_hash: Hash32,
    pub attention_hash: Hash32,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub layer_count: u32,
    pub page_rows: u64,
    pub tokens: Vec<u64>,
}

/// The result of a compatibility lookup.
#[derive(Clone, Debug)]
pub enum KvLookup {
    /// A compatible prefix record (the longest one that matched).
    Reusable {
        record: Box<KvPrefixRecord>,
        scanned: u64,
    },
    /// No compatible prefix. Every reason is explicit; recompute.
    Recompute { reasons: Vec<String>, scanned: u64 },
}

/// Result of a prefix put.
#[derive(Clone, Debug)]
pub struct KvPrefixWrite {
    pub record: KvPrefixRecord,
    pub prefix_id: Hash32,
    pub commit: Hash32,
    pub new_pages: u64,
}

/// Result of an authorized prefix get.
#[derive(Clone, Debug)]
pub struct KvPrefixRead {
    pub record: KvPrefixRecord,
    pub pages: Vec<DecodedKvPage>,
}

/// KV logical GC report.
#[derive(Clone, Debug, Default)]
pub struct KvGcReport {
    pub applied: bool,
    pub prefixes: u64,
    pub marked_pages: u64,
    pub candidates: Vec<String>,
    pub deleted: Vec<String>,
    pub deleted_bytes: u64,
    /// Candidate pages kept because an open handle pinned the object.
    pub kept_pinned: u64,
    /// Problems: dangling page references, unparseable records, etc.
    pub problems: Vec<String>,
}

// ---------------------------------------------------------------------------
// Prefill / reuse calibration
// ---------------------------------------------------------------------------

/// One declared prefill row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvPrefillRow {
    pub prefill_fixed_ns: u64,
    pub prefill_ns_per_token: u64,
}

/// The declared reuse/prefill model (`weft.kv.prefill.v1`).
///
/// This is a **declared** model, not a measurement. The shipped document is
/// `bench/calibration/kv-prefill-v1.json`; `KvPrefillModel::declared()` is the
/// same document in code and a test keeps them equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvPrefillModel {
    pub label: String,
    pub defaults: KvPrefillRow,
    pub models: BTreeMap<String, KvPrefillRow>,
    pub transfer_ns_per_mib: u64,
    pub placement_fixed_ns: u64,
    pub lookup_fixed_ns: u64,
    pub decode_cost_model: CostModelKind,
}

impl KvPrefillModel {
    /// The baked document. Must stay equal to the shipped JSON.
    pub fn declared() -> Self {
        let defaults = KvPrefillRow {
            prefill_fixed_ns: 2_000_000,
            prefill_ns_per_token: 120_000,
        };
        let mut models = BTreeMap::new();
        models.insert(
            "qwen2.5-0.5b@060db649".to_owned(),
            KvPrefillRow {
                prefill_fixed_ns: 1_500_000,
                prefill_ns_per_token: 90_000,
            },
        );
        KvPrefillModel {
            label: "declared; not measured against a real inference engine".to_owned(),
            defaults,
            models,
            transfer_ns_per_mib: 250_000,
            placement_fixed_ns: 50_000,
            lookup_fixed_ns: 20_000,
            decode_cost_model: CostModelKind::Measured,
        }
    }

    pub fn row(&self, model: &str) -> KvPrefillRow {
        self.models.get(model).copied().unwrap_or(self.defaults)
    }

    /// Strict parse of `weft.kv.prefill.v1`. Unknown keys, a missing default,
    /// a bad cost-model name, and out-of-bound coefficients are rejected.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::parse("kv prefill model is not UTF-8"))?;
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|err| Error::parse(format!("kv prefill model: {err}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| Error::parse("kv prefill model is not a JSON object"))?;
        for key in object.keys() {
            match key.as_str() {
                "schema"
                | "label"
                | "defaults"
                | "models"
                | "transfer_ns_per_mib"
                | "placement_fixed_ns"
                | "lookup_fixed_ns"
                | "decode_cost_model" => {}
                other => {
                    return Err(Error::parse(format!(
                        "unknown kv prefill model key `{other}`"
                    )))
                }
            }
        }
        if object.get("schema").and_then(|value| value.as_str()) != Some("weft.kv.prefill.v1") {
            return Err(Error::parse(
                "kv prefill model schema must be weft.kv.prefill.v1",
            ));
        }
        let defaults = prefill_row(
            object
                .get("defaults")
                .ok_or_else(|| Error::parse("kv prefill model needs `defaults`"))?,
            "defaults",
        )?;
        if defaults.prefill_ns_per_token == 0 {
            return Err(Error::parse(
                "kv prefill defaults.prefill_ns_per_token must be positive",
            ));
        }
        let mut models = BTreeMap::new();
        if let Some(map) = object.get("models") {
            let map = map
                .as_object()
                .ok_or_else(|| Error::parse("kv prefill model `models` is not an object"))?;
            for (name, value) in map {
                models.insert(name.clone(), prefill_row(value, &format!("models.{name}"))?);
            }
        }
        let decode = object
            .get("decode_cost_model")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::parse("kv prefill model needs `decode_cost_model`"))?;
        Ok(KvPrefillModel {
            label: object
                .get("label")
                .and_then(|value| value.as_str())
                .unwrap_or("declared")
                .to_owned(),
            defaults,
            models,
            transfer_ns_per_mib: prefill_u64(object, "transfer_ns_per_mib", "")?,
            placement_fixed_ns: prefill_u64(object, "placement_fixed_ns", "")?,
            lookup_fixed_ns: prefill_u64(object, "lookup_fixed_ns", "")?,
            decode_cost_model: CostModelKind::parse(decode)?,
        })
    }
}

fn prefill_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    prefix: &str,
) -> Result<u64> {
    let raw = object
        .get(name)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            Error::parse(format!(
                "kv prefill model `{prefix}{name}` must be a non-negative integer"
            ))
        })?;
    if raw > 1_000_000_000_000_000 {
        return Err(Error::parse(format!(
            "kv prefill model `{prefix}{name}` is out of bounds"
        )));
    }
    Ok(raw)
}

fn prefill_row(value: &serde_json::Value, prefix: &str) -> Result<KvPrefillRow> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::parse(format!("kv prefill model {prefix} is not an object")))?;
    for key in object.keys() {
        if key != "prefill_fixed_ns" && key != "prefill_ns_per_token" {
            return Err(Error::parse(format!(
                "unknown kv prefill row key `{key}` in {prefix}"
            )));
        }
    }
    Ok(KvPrefillRow {
        prefill_fixed_ns: prefill_u64(object, "prefill_fixed_ns", &format!("{prefix}."))?,
        prefill_ns_per_token: prefill_u64(object, "prefill_ns_per_token", &format!("{prefix}."))?,
    })
}

/// The decision and every component that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvReuseDecision {
    pub reuse: bool,
    pub reason: String,
    pub prefix_id: Option<Hash32>,
    pub tokens: u64,
    pub page_bytes: u64,
    pub lookup_ns: u64,
    /// Observed wall time of the compatibility scan. Not part of the decision.
    pub observed_lookup_ns: u64,
    pub transfer_ns: u64,
    pub decode_ns: u64,
    pub placement_ns: u64,
    pub reuse_total_ns: u64,
    pub recompute_fixed_ns: u64,
    pub recompute_ns: u64,
    pub model_row: String,
    pub decode_cost_model: &'static str,
    pub scanned: u64,
}

// ---------------------------------------------------------------------------
// Store integration
// ---------------------------------------------------------------------------

impl Store {
    /// Write one mode-12 KV page with an explicit ObjectId seed.
    #[allow(clippy::too_many_arguments)]
    pub fn write_kv_page_seeded(
        &mut self,
        parent: Hash32,
        name: &[u8],
        tenant: &str,
        input: &KvCacheInput,
        elements: &[u8],
        uid: u64,
        gid: u64,
        seed: &[u8],
    ) -> Result<KvPageWrite> {
        let limits = Limits::default();
        let frame = kv_cache::encode_kv_page(input, elements, &limits).map_err(Error::from)?;
        let descriptor = KvDescriptor::from_input(input, tenant, elements.len() as u64)?;
        let descriptor_id = descriptor.id()?;
        let attrs = kv_page_attrs(&descriptor)?;
        let object_id =
            self.create_file_seeded(parent, name, &frame, 0o644, uid, gid, attrs, seed)?;
        Ok(KvPageWrite {
            object_id,
            descriptor_id,
            frame_bytes: frame.len() as u64,
            page_bytes: elements.len() as u64,
        })
    }

    /// Store a KV prefix (pages + canonical record) in one commit.
    ///
    /// `tokens` is the exact token sequence; the record's chain root is derived
    /// from it, so a caller cannot claim a chain it does not have.
    /// `new_page_bytes[layer][index - parent_pages]` supplies only the pages
    /// this prefix adds; a parent's pages are reused verbatim.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_prefix_put(
        &mut self,
        name: &str,
        spec: &KvPrefixSpec,
        tokens: &[u64],
        parent_name: Option<&str>,
        new_page_bytes: &[Vec<Vec<u8>>],
        uid: u64,
        gid: u64,
    ) -> Result<KvPrefixWrite> {
        let seed = identity::os_random_32()?;
        self.kv_prefix_put_seeded(
            name,
            spec,
            tokens,
            parent_name,
            new_page_bytes,
            uid,
            gid,
            &seed,
        )
    }

    /// Deterministic form of [`Store::kv_prefix_put`] for fixtures and courts.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_prefix_put_seeded(
        &mut self,
        name: &str,
        spec: &KvPrefixSpec,
        tokens: &[u64],
        parent_name: Option<&str>,
        new_page_bytes: &[Vec<Vec<u8>>],
        uid: u64,
        gid: u64,
        seed: &[u8],
    ) -> Result<KvPrefixWrite> {
        self.require_writable()?;
        if name.is_empty() || name.contains('/') || name.contains('\0') {
            return Err(Error::invalid_argument(
                "kv prefix name must be non-empty and contain no `/`",
            ));
        }
        let position_end = tokens.len() as u64;
        if position_end == 0 || position_end > MAX_KV_PREFIX_TOKENS {
            return Err(Error::limit_exceeded(format!(
                "kv prefix token length must be 1..={MAX_KV_PREFIX_TOKENS}"
            )));
        }
        if spec.page_rows == 0 {
            return Err(Error::invalid_argument(
                "kv prefix page_rows must be positive",
            ));
        }
        let dtype = KvDtype::from_name(&spec.dtype).map_err(Error::from)?;
        if spec.layer_count == 0 || spec.head_count == 0 || spec.head_dim == 0 {
            return Err(Error::invalid_argument(
                "kv prefix layer_count, head_count and head_dim must be positive",
            ));
        }
        if new_page_bytes.len() != spec.layer_count as usize {
            return Err(Error::invalid_argument(
                "kv prefix page payloads must have one row per layer",
            ));
        }
        let page_count = position_end.div_ceil(spec.page_rows);
        let chain_root = kv_token_chain_root(tokens);
        let word_bytes = dtype.word_bytes();

        // Parent: load, prove compatibility, and prove the chain prefix.
        let parent = match parent_name {
            Some(parent_name) => {
                let parent_record = self.kv_prefix(parent_name)?;
                let parent_request = KvPrefixRequest {
                    tenant: spec.tenant.clone(),
                    model: spec.model.clone(),
                    tokenizer_hash: spec.tokenizer_hash,
                    attention_hash: spec.attention_hash,
                    dtype: spec.dtype.clone(),
                    head_count: spec.head_count,
                    head_dim: spec.head_dim,
                    layer_count: spec.layer_count,
                    page_rows: spec.page_rows,
                    tokens: tokens.to_vec(),
                };
                if let Err(reason) = parent_record.compatibility(&parent_request) {
                    return Err(Error::conflict(format!(
                        "kv prefix `{parent_name}` is not a compatible parent: {reason}"
                    )));
                }
                if parent_record.position_end % spec.page_rows != 0 {
                    return Err(Error::invalid_argument(format!(
                        "kv prefix `{parent_name}` ends mid-page ({} tokens, {}-token pages); \
                         extend only at a page boundary",
                        parent_record.position_end, spec.page_rows
                    )));
                }
                if parent_record.position_end >= position_end {
                    return Err(Error::invalid_argument(
                        "a parent prefix must be strictly shorter than the new prefix",
                    ));
                }
                if parent_record.page_count != parent_record.position_end / spec.page_rows {
                    return Err(Error::corrupt(format!(
                        "kv prefix `{parent_name}` page count is inconsistent"
                    )));
                }
                Some(parent_record)
            }
            None => None,
        };
        let parent_pages = parent.as_ref().map(|record| record.page_count).unwrap_or(0);
        let new_pages_per_layer = page_count - parent_pages;
        for (layer, pages) in new_page_bytes.iter().enumerate() {
            if pages.len() as u64 != new_pages_per_layer {
                return Err(Error::invalid_argument(format!(
                    "kv prefix layer {layer} supplies {} new pages, expected {new_pages_per_layer}",
                    pages.len()
                )));
            }
            for (offset, payload) in pages.iter().enumerate() {
                let index = parent_pages + offset as u64;
                let expected = page_element_bytes(
                    index,
                    page_count,
                    spec.page_rows,
                    position_end,
                    spec.head_count,
                    spec.head_dim,
                    word_bytes,
                )?;
                if payload.len() as u64 != expected {
                    return Err(Error::invalid_argument(format!(
                        "kv prefix page {index} has {} bytes, expected {expected}",
                        payload.len()
                    )));
                }
            }
        }

        let record_for_id =
            |pages: Vec<Vec<Hash32>>, stored: u64, reused: u64| -> Result<KvPrefixRecord> {
                Ok(KvPrefixRecord {
                    tenant: spec.tenant.clone(),
                    model: spec.model.clone(),
                    tokenizer_hash: spec.tokenizer_hash,
                    attention_hash: spec.attention_hash,
                    dtype: spec.dtype.clone(),
                    head_count: spec.head_count,
                    head_dim: spec.head_dim,
                    layer_count: spec.layer_count,
                    page_rows: spec.page_rows,
                    page_count,
                    position_start: 0,
                    position_end,
                    token_chain_root: chain_root,
                    parent: parent.as_ref().map(|record| record.id()).transpose()?,
                    pages,
                    pinned: spec.pinned,
                    page_bytes: total_page_bytes(
                        page_count,
                        spec.page_rows,
                        position_end,
                        spec.layer_count,
                        spec.head_count,
                        spec.head_dim,
                        word_bytes,
                    )?,
                    stored_page_bytes: stored,
                    reused_pages: reused,
                })
            };

        let existing = match self.kv_prefix(name) {
            Ok(record) => Some(record),
            Err(err) if err.class() == Class::NotFound => None,
            Err(err) => return Err(err),
        };
        if let Some(existing) = &existing {
            if existing.pinned {
                return Err(Error::busy(format!(
                    "kv prefix `{name}` is pinned and cannot be replaced"
                )));
            }
        }

        // Everything below is one commit: no partially visible prefix.
        let ((record, prefix_id), commit) = self.group(|store| {
            let root = store.resolve_path("/")?;
            let kv = ensure_dir(store, root, KV_DIR, uid, gid)?;
            let prefix_dir = ensure_dir(store, kv, KV_PREFIX_DIR, uid, gid)?;
            let page_dir = ensure_dir(store, kv, KV_PAGE_DIR, uid, gid)?;

            let mut layers: Vec<Vec<Hash32>> = Vec::with_capacity(spec.layer_count as usize);
            let mut stored_bytes = 0u64;
            for layer in 0..spec.layer_count {
                let mut ids: Vec<Hash32> = Vec::with_capacity(page_count as usize);
                for index in 0..parent_pages {
                    ids.push(parent.as_ref().unwrap().pages[layer as usize][index as usize]);
                }
                for offset in 0..new_pages_per_layer as usize {
                    let index = parent_pages + offset as u64;
                    let payload = &new_page_bytes[layer as usize][offset];
                    let input = KvCacheInput {
                        model: spec.model.clone(),
                        layer,
                        session: String::new(),
                        page_index: index,
                        page_count,
                        seq_start: index * spec.page_rows,
                        seq_end: position_end.min((index + 1) * spec.page_rows),
                        dtype,
                        head_count: spec.head_count,
                        head_dim: spec.head_dim,
                    };
                    let descriptor =
                        KvDescriptor::from_input(&input, &spec.tenant, payload.len() as u64)?;
                    let descriptor_id = descriptor.id()?;
                    // `stored_page_bytes` is the logical size of the pages this
                    // prefix adds. It is a property of the prefix, not of whether
                    // the page object already existed, so a repeated put produces
                    // the same record id.
                    stored_bytes = stored_bytes.saturating_add(payload.len() as u64);
                    // The page name binds both the descriptor and the exact
                    // payload: two prefixes whose pages share metadata but not
                    // bytes must never alias one object.
                    let page_name = format!(
                        "p-{}-{}",
                        descriptor_id.to_hex(),
                        identity::h(crate::identity::CHUNK_DOMAIN, payload).to_hex()
                    );
                    // Content-addressed reuse: the same descriptor *and* the same
                    // decoded bytes is the same page object. Different bytes under
                    // the same descriptor is a conflict, never a silent share.
                    let object_id = match store.lookup(page_dir, page_name.as_bytes()) {
                        Ok(existing_id) => {
                            let existing_bytes = store.read_object(existing_id)?;
                            let frame = Frame::parse(&existing_bytes, &Limits::default())
                                .map_err(Error::from)?;
                            let decoded = kv_cache::decode_kv_page(&frame, &Limits::default())
                                .map_err(Error::from)?;
                            if decoded.bytes != *payload {
                                return Err(Error::conflict(format!(
                                    "kv page `{page_name}` already exists with different bytes"
                                )));
                            }
                            existing_id
                        }
                        Err(err) if err.class() == Class::NotFound => {
                            // The page ObjectId seed is derived from the page
                            // descriptor id, so two different descriptors can never
                            // collide on one ObjectId even at the same layer/index.
                            let page_seed = identity::h("weft/kv/page-seed/v1", &{
                                let mut data =
                                    Vec::with_capacity(seed.len() + descriptor_id.as_bytes().len());
                                data.extend_from_slice(seed);
                                data.extend_from_slice(descriptor_id.as_bytes());
                                data.extend_from_slice(
                                    identity::h(crate::identity::CHUNK_DOMAIN, payload).as_bytes(),
                                );
                                data
                            });
                            let written = store.write_kv_page_seeded(
                                page_dir,
                                page_name.as_bytes(),
                                &spec.tenant,
                                &input,
                                payload,
                                uid,
                                gid,
                                page_seed.as_bytes(),
                            )?;
                            written.object_id
                        }
                        Err(err) => return Err(err),
                    };
                    ids.push(object_id);
                }
                layers.push(ids);
            }

            let reused = parent
                .as_ref()
                .map(|record| record.page_count * u64::from(record.layer_count))
                .unwrap_or(0);
            let record = record_for_id(layers, stored_bytes, reused)?;
            record.validate_shape()?;
            let prefix_id = record.id()?;
            let bytes = record.canonical_bytes()?;
            let mut attrs = BTreeMap::new();
            attrs.insert(ATTR_KV_PREFIX.to_owned(), Value::Str(prefix_id.to_hex()));
            attrs.insert(ATTR_KV_PREFIX_PINNED.to_owned(), Value::Bool(spec.pinned));
            attrs.insert(
                ATTR_KV_PREFIX_TENANT.to_owned(),
                Value::Str(spec.tenant.clone()),
            );
            attrs.insert(
                ATTR_KV_PREFIX_MODEL.to_owned(),
                Value::Str(spec.model.clone()),
            );
            attrs.insert(ATTR_KV_PREFIX_TOKENS.to_owned(), Value::U64(position_end));
            if existing.is_some() {
                store.unlink(prefix_dir, name.as_bytes())?;
            }
            let record_seed = identity::h("weft/kv/prefix-seed/v1", &{
                let mut data = Vec::with_capacity(seed.len() + prefix_id.as_bytes().len());
                data.extend_from_slice(seed);
                data.extend_from_slice(prefix_id.as_bytes());
                data
            });
            store.create_file_seeded(
                prefix_dir,
                name.as_bytes(),
                &bytes,
                0o644,
                uid,
                gid,
                attrs,
                record_seed.as_bytes(),
            )?;
            Ok((record, prefix_id))
        })?;
        Ok(KvPrefixWrite {
            prefix_id,
            record,
            commit,
            new_pages: new_pages_per_layer * u64::from(spec.layer_count),
        })
    }

    /// Load a prefix record by name.
    pub fn kv_prefix(&self, name: &str) -> Result<KvPrefixRecord> {
        let kv = self.resolve_path(&format!("/{KV_DIR}"))?;
        let prefix_dir = self.lookup(kv, KV_PREFIX_DIR.as_bytes()).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::not_found(format!("kv prefix `{name}` does not exist"))
            } else {
                err
            }
        })?;
        let id = self.lookup(prefix_dir, name.as_bytes()).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::not_found(format!("kv prefix `{name}` does not exist"))
            } else {
                err
            }
        })?;
        let bytes = self.read_object(id)?;
        KvPrefixRecord::from_canonical_bytes(&bytes)
    }

    /// Every live prefix record, sorted by name.
    pub fn kv_prefix_list(&self) -> Result<Vec<(String, KvPrefixRecord)>> {
        let kv = match self.resolve_path(&format!("/{KV_DIR}")) {
            Ok(kv) => kv,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let prefix_dir = match self.lookup(kv, KV_PREFIX_DIR.as_bytes()) {
            Ok(dir) => dir,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut out = Vec::new();
        for entry in self.list_dir(prefix_dir)? {
            let name = String::from_utf8_lossy(&entry.name).into_owned();
            let target = self.lookup(prefix_dir, &entry.name)?;
            if self.object_meta(target)?.kind != ObjectKind::File {
                continue;
            }
            let bytes = self.read_object(target)?;
            match KvPrefixRecord::from_canonical_bytes(&bytes) {
                Ok(record) => out.push((name, record)),
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Exact compatibility lookup. Chooses the longest compatible prefix;
    /// never a partial match.
    pub fn kv_prefix_lookup(&self, request: &KvPrefixRequest) -> Result<KvLookup> {
        if request.tokens.is_empty() {
            return Err(Error::invalid_argument(
                "kv lookup needs at least one token",
            ));
        }
        if request.tokens.len() as u64 > MAX_KV_PREFIX_TOKENS {
            return Err(Error::limit_exceeded(format!(
                "kv lookup token length exceeds {MAX_KV_PREFIX_TOKENS}"
            )));
        }
        let prefixes = self.kv_prefix_list()?;
        let scanned = prefixes.len() as u64;
        let mut best: Option<KvPrefixRecord> = None;
        let mut reasons = Vec::new();
        for (_, record) in &prefixes {
            match record.compatibility(request) {
                Ok(()) => {
                    if best
                        .as_ref()
                        .map(|candidate| record.position_end > candidate.position_end)
                        .unwrap_or(true)
                    {
                        best = Some(record.clone());
                    }
                }
                Err(reason) => {
                    if reasons.len() < MAX_LOOKUP_REASONS {
                        reasons.push(reason);
                    }
                }
            }
        }
        match best {
            Some(record) => Ok(KvLookup::Reusable {
                record: Box::new(record),
                scanned,
            }),
            None => {
                if prefixes.is_empty() {
                    reasons.push("no kv prefixes are stored".to_owned());
                }
                Ok(KvLookup::Recompute { reasons, scanned })
            }
        }
    }

    /// Read a prefix under an authenticated capability.
    ///
    /// Every pre-byte failure is the same non-disclosing [`KV_DENIED`]. Each
    /// page is read through [`Store::read_kv_page`], so page access has the same
    /// per-object capability rule as the KV governance surface. After
    /// authorization the decoded page metadata is checked against the record.
    pub fn kv_prefix_get(
        &self,
        name: &str,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<KvPrefixRead> {
        let Some(verified) = principal.envelope() else {
            return Err(Error::new(Class::Unauthorized, KV_DENIED));
        };
        let capability = CapabilityContext::from_envelope(&verified.envelope)
            .map_err(|_| Error::new(Class::Unauthorized, KV_DENIED))?;
        if capability.check_valid_at(now_ns).is_err() {
            return Err(Error::new(Class::Unauthorized, KV_DENIED));
        }
        let record = match self.kv_prefix(name) {
            Ok(record) => record,
            Err(err) if err.class() == Class::NotFound => return Err(err),
            Err(err) => return Err(err),
        };
        if capability.scope.principal != record.tenant
            || !capability.allows_namespace(&format!("kv.{}", record.tenant))
        {
            return Err(Error::new(Class::Unauthorized, KV_DENIED));
        }
        let mut pages = Vec::with_capacity(
            (record.page_count * u64::from(record.layer_count)).min(65_536) as usize,
        );
        for (layer, ids) in record.pages.iter().enumerate() {
            for (index, id) in ids.iter().enumerate() {
                let decoded = self
                    .read_kv_page(*id, principal, now_ns)
                    .map_err(|err| match err.class() {
                        Class::Unauthorized => Error::new(Class::Unauthorized, KV_DENIED),
                        // An authorized reader may see a corrupt page.
                        other => Error::new(other, err.message()),
                    })?;
                let expected_start = (index as u64) * record.page_rows;
                let expected_end = record
                    .position_end
                    .min((index as u64 + 1) * record.page_rows);
                if decoded.meta.layer != layer as u32
                    || decoded.meta.page_index != index as u64
                    || decoded.meta.seq_start != expected_start
                    || decoded.meta.seq_end != expected_end
                    || decoded.meta.dtype.as_str() != record.dtype
                    || decoded.meta.model != record.model
                    || decoded.meta.head_count != record.head_count
                    || decoded.meta.head_dim != record.head_dim
                {
                    return Err(Error::corrupt(format!(
                        "kv page {}:{index} metadata does not match its prefix record",
                        layer
                    )));
                }
                pages.push(decoded);
            }
        }
        Ok(KvPrefixRead { record, pages })
    }

    /// Delete a prefix record. A pinned prefix is refused. Pages stay until
    /// `kv_gc`; the record's history is immutable.
    pub fn kv_prefix_delete(&mut self, name: &str) -> Result<()> {
        self.require_writable()?;
        let record = self.kv_prefix(name)?;
        if record.pinned {
            return Err(Error::busy(format!(
                "kv prefix `{name}` is pinned; unpin it before deleting"
            )));
        }
        let kv = self.resolve_path(&format!("/{KV_DIR}"))?;
        let prefix_dir = self.lookup(kv, KV_PREFIX_DIR.as_bytes())?;
        self.unlink(prefix_dir, name.as_bytes())
    }

    /// Logical KV GC: keep every page named by a live prefix or pinned by an
    /// open handle; remove every other KV page from current state.
    pub fn kv_gc(&mut self, apply: bool) -> Result<KvGcReport> {
        self.require_writable()?;
        let mut report = KvGcReport {
            applied: apply,
            ..KvGcReport::default()
        };
        let prefixes = self.kv_prefix_list()?;
        report.prefixes = prefixes.len() as u64;
        let mut marked: BTreeSet<Hash32> = BTreeSet::new();
        for (name, record) in &prefixes {
            for layer in &record.pages {
                for id in layer {
                    if self.object_meta(*id).is_err() {
                        report
                            .problems
                            .push(format!("kv prefix `{name}` names missing page {id}"));
                        continue;
                    }
                    marked.insert(*id);
                }
            }
        }
        report.marked_pages = marked.len() as u64;
        let kv = match self.resolve_path(&format!("/{KV_DIR}")) {
            Ok(kv) => kv,
            Err(err) if err.class() == Class::NotFound => return Ok(report),
            Err(err) => return Err(err),
        };
        let page_dir = match self.lookup(kv, KV_PAGE_DIR.as_bytes()) {
            Ok(dir) => dir,
            Err(err) if err.class() == Class::NotFound => return Ok(report),
            Err(err) => return Err(err),
        };
        let mut candidates: Vec<(String, Hash32, u64)> = Vec::new();
        for entry in self.list_dir(page_dir)? {
            let name = String::from_utf8_lossy(&entry.name).into_owned();
            let target = self.lookup(page_dir, &entry.name)?;
            let object = self.object_meta(target)?;
            if object.kind != ObjectKind::File {
                continue;
            }
            if !object.attrs.contains_key(ATTR_KV_DESCRIPTOR)
                || !object.attrs.contains_key(ATTR_KV_DESCRIPTOR_ID)
            {
                report
                    .problems
                    .push(format!("kv page `{name}` has no capability descriptor"));
                continue;
            }
            if marked.contains(&target) {
                continue;
            }
            if self.object_pin_count(target) > 0 {
                report.kept_pinned += 1;
                continue;
            }
            candidates.push((name, target, object.size));
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, target, size) in candidates {
            report.candidates.push(name.clone());
            if apply {
                let current = self.list_dir(page_dir)?;
                if let Some(entry) = current.iter().find(|entry| entry.target == target) {
                    self.unlink(page_dir, &entry.name)?;
                }
                report.deleted.push(name);
                report.deleted_bytes = report.deleted_bytes.saturating_add(size);
            }
        }
        Ok(report)
    }

    /// The reuse-vs-recompute decision with every component recorded.
    pub fn kv_decide_reuse(
        &self,
        request: &KvPrefixRequest,
        model: &KvPrefillModel,
    ) -> Result<KvReuseDecision> {
        let started = Instant::now();
        let lookup = self.kv_prefix_lookup(request)?;
        let observed_lookup_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let tokens = request.tokens.len() as u64;
        let row = model.row(&request.model);
        let recompute_fixed_ns = row.prefill_fixed_ns;
        let recompute_ns = saturating_add(
            recompute_fixed_ns,
            saturating_mul(tokens, row.prefill_ns_per_token),
        );
        let table = CostTable::baked(model.decode_cost_model);
        match lookup {
            KvLookup::Reusable { record, scanned } => {
                let page_bytes = record.page_bytes;
                let transfer_ns = per_mib(page_bytes, model.transfer_ns_per_mib);
                let decode_ns = table.estimate_ns(CostClass::KvCache, page_bytes);
                let placement_ns = model.placement_fixed_ns;
                // The declared lookup term is a floor: a pooled scan that took
                // longer than the declared fixed cost is priced at what it
                // actually cost. The decision is therefore conservative, and
                // `observed_lookup_ns` is reported separately so a reader can
                // see how much of the estimate was analytic.
                let lookup_ns = model.lookup_fixed_ns.max(observed_lookup_ns);
                let reuse_total_ns = saturating_add(
                    saturating_add(lookup_ns, transfer_ns),
                    saturating_add(decode_ns, placement_ns),
                );
                let reuse = reuse_total_ns < recompute_ns;
                let reason = if reuse {
                    format!(
                        "reuse `{}`: T_reuse {reuse_total_ns} ns < T_recompute {recompute_ns} ns",
                        record.id()?.to_hex()
                    )
                } else {
                    format!(
                        "recompute: T_reuse {reuse_total_ns} ns >= T_recompute {recompute_ns} ns"
                    )
                };
                Ok(KvReuseDecision {
                    reuse,
                    reason,
                    prefix_id: Some(record.id()?),
                    tokens,
                    page_bytes,
                    lookup_ns,
                    observed_lookup_ns,
                    transfer_ns,
                    decode_ns,
                    placement_ns,
                    reuse_total_ns,
                    recompute_fixed_ns,
                    recompute_ns,
                    model_row: request.model.clone(),
                    decode_cost_model: model.decode_cost_model.as_str(),
                    scanned,
                })
            }
            KvLookup::Recompute { reasons, scanned } => {
                let lookup_ns = model.lookup_fixed_ns.max(observed_lookup_ns);
                let reuse_total_ns = lookup_ns;
                Ok(KvReuseDecision {
                    reuse: false,
                    reason: format!(
                        "recompute: no compatible prefix ({})",
                        if reasons.is_empty() {
                            "no candidate".to_owned()
                        } else {
                            reasons.join("; ")
                        }
                    ),
                    prefix_id: None,
                    tokens,
                    page_bytes: 0,
                    lookup_ns,
                    observed_lookup_ns,
                    transfer_ns: 0,
                    decode_ns: 0,
                    placement_ns: 0,
                    reuse_total_ns,
                    recompute_fixed_ns,
                    recompute_ns,
                    model_row: request.model.clone(),
                    decode_cost_model: model.decode_cost_model.as_str(),
                    scanned,
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn kv_page_attrs(descriptor: &KvDescriptor) -> Result<BTreeMap<String, Value>> {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        ATTR_KV_DESCRIPTOR.to_owned(),
        Value::Str(hex::encode(descriptor.canonical_bytes()?)),
    );
    attrs.insert(
        ATTR_KV_DESCRIPTOR_ID.to_owned(),
        Value::Str(descriptor.id()?.to_hex()),
    );
    attrs.insert(
        crate::kv_capability::ATTR_KV_TENANT.to_owned(),
        Value::Str(descriptor.tenant.clone()),
    );
    attrs.insert(
        crate::kv_capability::ATTR_KV_MODEL.to_owned(),
        Value::Str(descriptor.model.clone()),
    );
    attrs.insert(
        crate::kv_capability::ATTR_KV_SESSION.to_owned(),
        Value::Str(descriptor.session.clone()),
    );
    attrs.insert(
        crate::kv_capability::ATTR_KV_PAGE_BYTES.to_owned(),
        Value::U64(descriptor.page_bytes),
    );
    Ok(attrs)
}

fn page_element_bytes(
    index: u64,
    page_count: u64,
    page_rows: u64,
    position_end: u64,
    head_count: u32,
    head_dim: u32,
    word_bytes: u64,
) -> Result<u64> {
    if index >= page_count {
        return Err(Error::invalid_argument("kv page index is past the prefix"));
    }
    let start = index
        .checked_mul(page_rows)
        .ok_or_else(|| Error::limit_exceeded("kv page offset overflows u64"))?;
    let end = position_end.min(start.saturating_add(page_rows));
    let rows = end
        .checked_sub(start)
        .ok_or_else(|| Error::corrupt("kv page range is inverted"))?;
    rows.checked_mul(u64::from(head_count))
        .and_then(|value| value.checked_mul(u64::from(head_dim)))
        .and_then(|value| value.checked_mul(word_bytes))
        .ok_or_else(|| Error::limit_exceeded("kv page byte length overflows u64"))
}

fn total_page_bytes(
    page_count: u64,
    page_rows: u64,
    position_end: u64,
    layer_count: u32,
    head_count: u32,
    head_dim: u32,
    word_bytes: u64,
) -> Result<u64> {
    let mut total = 0u64;
    for index in 0..page_count {
        total = total
            .checked_add(page_element_bytes(
                index,
                page_count,
                page_rows,
                position_end,
                head_count,
                head_dim,
                word_bytes,
            )?)
            .ok_or_else(|| Error::limit_exceeded("kv prefix page bytes overflow u64"))?;
    }
    total
        .checked_mul(u64::from(layer_count))
        .ok_or_else(|| Error::limit_exceeded("kv prefix total bytes overflow u64"))
}

fn ensure_dir(store: &mut Store, parent: Hash32, name: &str, uid: u64, gid: u64) -> Result<Hash32> {
    match store.lookup(parent, name.as_bytes()) {
        Ok(id) => {
            if store.object_meta(id)?.kind != ObjectKind::Dir {
                return Err(Error::not_dir(format!(
                    "`{name}` exists and is not a directory"
                )));
            }
            Ok(id)
        }
        Err(err) if err.class() == Class::NotFound => {
            store.mkdir(parent, name.as_bytes(), 0o755, uid, gid)
        }
        Err(err) => Err(err),
    }
}

fn per_mib(bytes: u64, ns_per_mib: u64) -> u64 {
    let scaled = (u128::from(bytes) * u128::from(ns_per_mib)) / 1_048_576u128;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

fn saturating_mul(a: u64, b: u64) -> u64 {
    u64::try_from(u128::from(a) * u128::from(b)).unwrap_or(u64::MAX)
}

fn saturating_add(a: u64, b: u64) -> u64 {
    a.saturating_add(b)
}

/// Reserved prefix attribute names, for tests and documentation.
pub fn reserved_prefix_attribute_names() -> [&'static str; 5] {
    [
        ATTR_KV_PREFIX,
        ATTR_KV_PREFIX_PINNED,
        ATTR_KV_PREFIX_TENANT,
        ATTR_KV_PREFIX_MODEL,
        ATTR_KV_PREFIX_TOKENS,
    ]
}

/// The record object's full attribute map is private to the module; this is
/// only used by verification tools that need the id.
pub fn prefix_id_of(record: &ObjectRecord) -> Option<Hash32> {
    match record.attrs.get(ATTR_KV_PREFIX) {
        Some(Value::Str(text)) => Hash32::from_hex(text).ok(),
        _ => None,
    }
}
