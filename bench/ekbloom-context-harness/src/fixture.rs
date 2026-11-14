//! Deterministic fixture: a WeftFS store, the subject scope records, the
//! consumer contracts, the authority records and the request stream.
//!
//! The store is built once by `prepare`; every arm runs against a byte-identical
//! copy. Object ids are recorded in `fixture.json` so the run does not depend on
//! reproducing the store's internal write order.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{json, Map as JsonMap, Value as Json};
use weftcodec::hash::hash_domain;
use weftcodec::kv_cache::{KvCacheInput, KvDtype};
use weftcodec::kv_prov::{
    adapter_set_root, tokenizer_root, KvAttention, KvLayout, KvPositional, KvProvenanceInput,
};
use weftcodec::wcb::Value;
use weftfs::backend::BackendKind;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};

use crate::rule::{
    Attention, Dimensions, Geometry, SubjectKind, SubjectRecord, CONTRACT_SCHEMA, SUBJECT_SCHEMA,
};

pub const BRANCHES: usize = 3;
pub const ROOTS_PER_BRANCH: usize = 64;
pub const KV_PER_BRANCH: usize = 2048;
pub const TENSOR_PER_BRANCH: usize = 128;
pub const MISSING_PER_BRANCH: usize = 8;
pub const COMPATIBLE_SLOTS: u8 = 4;
pub const VARIANT_SLOTS: u8 = 9;
pub const MISSING_SLOTS: u8 = 4;
pub const CONTRACTS_PER_BRANCH: usize = 17;
pub const HOT_PAIRS: usize = 2048;
pub const WARM_PAIRS: usize = 4096;
pub const REQUESTS: usize = 8_000;
pub const REPETITIONS: usize = 9;
pub const BUDGET_BYTES: u64 = 1_048_576;
pub const KV_PAGE_ROWS: u64 = 8;
pub const KV_HEAD_COUNT: u32 = 8;
pub const KV_HEAD_DIM: u32 = 8;
pub const KV_PAGE_BYTES: usize = 1_024;
pub const TENSOR_BYTES: usize = 512;
pub const FIXED_TS: i64 = 1_700_000_000_000_000_000;

pub const EVENT_MODEL_UPDATE: usize = 2_000;
pub const EVENT_REVOCATION: usize = 3_600;
pub const EVENT_MISSING_DEP: usize = 5_200;
pub const EVENT_SNAPSHOT: usize = 6_800;

#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub tenant: String,
    pub dir_id: Hash32,
    pub model_root: Hash32,
    pub adapter_set: Hash32,
    pub tokenizer: Hash32,
    pub attention: Attention,
    pub positional: String,
    pub geometry: Geometry,
    pub salt: Hash32,
}

#[derive(Clone, Debug)]
pub struct Subject {
    pub branch: usize,
    pub kind: SubjectKind,
    pub page_id: Hash32,
    pub page_name: String,
    pub scope_object_id: Hash32,
    pub scope_root: Hash32,
    pub content_root: Hash32,
    pub descriptor_id: Hash32,
    pub provenance_root: Hash32,
    /// 0..3: which compatible token-history contract this subject matches.
    pub history_variant: u8,
    /// True for the 24 subjects whose page object is absent from S0.
    pub missing: bool,
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub branches: Vec<Branch>,
    pub subjects: Vec<Subject>,
    /// `[branch][slot]`.
    pub contracts: Vec<Vec<Dimensions>>,
    pub authorities: Vec<crate::rule::AuthorityRecord>,
    /// New model root used by the model-update event on branch 1.
    pub model_v2: Hash32,
    pub hot_pairs: Vec<(u32, u8)>,
    pub warm_pairs: Vec<(u32, u8)>,
    pub base_head: Hash32,
}

fn hs(domain: &str, parts: &[&[u8]]) -> Hash32 {
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
    }
    hash_domain(domain, &data)
}

fn hex32(text: &str) -> Result<Hash32, String> {
    Hash32::from_hex(text).map_err(|e| e.to_string())
}

fn dims_to_json(dims: &Dimensions) -> Json {
    let mut map = JsonMap::new();
    if let Some(v) = dims.model {
        map.insert("model".into(), json!(v.to_hex()));
    }
    if let Some(v) = dims.adapter_set {
        map.insert("adapter_set".into(), json!(v.to_hex()));
    }
    if let Some(v) = dims.tokenizer {
        map.insert("tokenizer".into(), json!(v.to_hex()));
    }
    if let Some(v) = dims.attention {
        map.insert(
            "attention".into(),
            json!({
                "head_count": v.head_count,
                "head_dim": v.head_dim,
                "sliding_window": v.sliding_window,
                "rope_theta_bits": v.rope_theta_bits,
                "rope_scale_bits": v.rope_scale_bits,
            }),
        );
    }
    if let Some(v) = &dims.positional {
        map.insert("positional".into(), json!(v));
    }
    if let Some(v) = &dims.geometry {
        map.insert(
            "geometry".into(),
            json!({
                "dtype": v.dtype,
                "page_rows": v.page_rows,
                "layer_count": v.layer_count,
            }),
        );
    }
    if let Some(v) = dims.token_history {
        map.insert("token_history".into(), json!(v.to_hex()));
    }
    if let Some(v) = &dims.tenant {
        map.insert("tenant".into(), json!(v));
    }
    if let Some(v) = dims.salt {
        map.insert("salt".into(), json!(v.to_hex()));
    }
    Json::Object(map)
}

fn dims_from_json(value: &Json) -> Result<Dimensions, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "dimensions are not an object".to_owned())?;
    let hash = |key: &str| -> Result<Option<Hash32>, String> {
        match obj.get(key).and_then(Json::as_str) {
            None => Ok(None),
            Some(text) => Ok(Some(hex32(text)?)),
        }
    };
    let attention = match obj.get("attention") {
        None => None,
        Some(value) => Some(Attention {
            head_count: value
                .get("head_count")
                .and_then(Json::as_u64)
                .ok_or("attention head_count")? as u32,
            head_dim: value
                .get("head_dim")
                .and_then(Json::as_u64)
                .ok_or("attention head_dim")? as u32,
            sliding_window: value
                .get("sliding_window")
                .and_then(Json::as_u64)
                .ok_or("attention sliding_window")?,
            rope_theta_bits: value
                .get("rope_theta_bits")
                .and_then(Json::as_u64)
                .ok_or("attention rope_theta_bits")?,
            rope_scale_bits: value
                .get("rope_scale_bits")
                .and_then(Json::as_u64)
                .ok_or("attention rope_scale_bits")?,
        }),
    };
    let geometry = match obj.get("geometry") {
        None => None,
        Some(value) => Some(Geometry {
            dtype: value
                .get("dtype")
                .and_then(Json::as_str)
                .ok_or("geometry dtype")?
                .to_owned(),
            page_rows: value
                .get("page_rows")
                .and_then(Json::as_u64)
                .ok_or("geometry page_rows")?,
            layer_count: value
                .get("layer_count")
                .and_then(Json::as_u64)
                .ok_or("geometry layer_count")?,
        }),
    };
    Ok(Dimensions {
        model: hash("model")?,
        adapter_set: hash("adapter_set")?,
        tokenizer: hash("tokenizer")?,
        attention,
        positional: obj
            .get("positional")
            .and_then(Json::as_str)
            .map(str::to_owned),
        geometry,
        token_history: hash("token_history")?,
        tenant: obj.get("tenant").and_then(Json::as_str).map(str::to_owned),
        salt: hash("salt")?,
    })
}

fn attention_of(branch: usize) -> Attention {
    match branch {
        0 => Attention {
            head_count: 8,
            head_dim: 8,
            sliding_window: 0,
            rope_theta_bits: 0x3f50624dd2f1a9fc,
            rope_scale_bits: 0x3ff0000000000000,
        },
        1 => Attention {
            head_count: 16,
            head_dim: 4,
            sliding_window: 4096,
            rope_theta_bits: 0x3f60624dd2f1a9fc,
            rope_scale_bits: 0x3ff0000000000000,
        },
        _ => Attention {
            head_count: 4,
            head_dim: 16,
            sliding_window: 0,
            rope_theta_bits: 0x3f70624dd2f1a9fc,
            rope_scale_bits: 0x3fe0000000000000,
        },
    }
}

fn positional_of(branch: usize) -> &'static str {
    match branch {
        0 => "rope",
        1 => "alibi",
        _ => "rope",
    }
}

fn branch_model_root(branch: usize) -> Hash32 {
    hs("weft/ekbctx/branch/v1", &[b"model", &[branch as u8]])
}

fn subject_token_history(branch: usize, variant: u8) -> Hash32 {
    let mut data = Vec::new();
    data.push(branch as u8);
    data.push(variant);
    hash_domain("weft/kv/exact-prefix/token-history/v1", &data)
}

fn subject_dims(branch: &Branch, branch_index: usize, history_variant: u8) -> Dimensions {
    Dimensions {
        model: Some(branch.model_root),
        adapter_set: Some(branch.adapter_set),
        tokenizer: Some(branch.tokenizer),
        attention: Some(branch.attention),
        positional: Some(branch.positional.clone()),
        geometry: Some(branch.geometry.clone()),
        token_history: Some(subject_token_history(branch_index, history_variant)),
        tenant: Some(branch.tenant.clone()),
        salt: Some(branch.salt),
    }
}

fn page_elements(subject: usize) -> Vec<u8> {
    let mut out = vec![0u8; KV_PAGE_BYTES];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = ((subject as u32 * 31 + i as u32 * 17 + 11) & 0xff) as u8;
    }
    out
}

fn tensor_bytes(subject: usize) -> Vec<u8> {
    let mut out = vec![0u8; TENSOR_BYTES];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = ((subject as u32 * 13 + i as u32 * 7 + 5) & 0xff) as u8;
    }
    out
}

pub fn prepare(path: &Path) -> Result<Fixture, String> {
    let mut opts = Options {
        owner_uid: 0,
        owner_gid: 0,
        fixed_timestamp_ns: Some(FIXED_TS),
        cache_bytes: 0,
        single_flight: false,
        attest_indexes: false,
        incremental_index: false,
        ..Options::default()
    };
    opts.chunk_size = 65_536;
    let mut store = Store::init_with_backend(path, opts, BackendKind::Local)
        .map_err(|e| format!("init: {e}"))?;
    let root = store.resolve_path("/").map_err(|e| e.to_string())?;
    let kv_root = store
        .mkdir_seeded(root, b"kv", 0o755, 0, 0, b"kv-root")
        .map_err(|e| e.to_string())?;
    let events_root = store
        .mkdir_seeded(root, b"events", 0o755, 0, 0, b"events-root")
        .map_err(|e| e.to_string())?;
    // One group commit for the whole fixture: thousands of single-mutation
    // commits would spend most of the setup in fsync, not in the court.
    store.begin_group().map_err(|e| e.to_string())?;

    let mut branches = Vec::new();
    for b in 0..BRANCHES {
        let name = format!("b{b}");
        let dir = store
            .mkdir_seeded(
                kv_root,
                name.as_bytes(),
                0o755,
                0,
                0,
                format!("kv-{name}").as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        branches.push(Branch {
            name,
            tenant: format!("tenant-b{b}"),
            dir_id: dir,
            model_root: branch_model_root(b),
            adapter_set: adapter_set_root(&[]).map_err(|e| e.to_string())?,
            tokenizer: tokenizer_root("spm", 32_000).map_err(|e| e.to_string())?,
            attention: attention_of(b),
            positional: positional_of(b).to_owned(),
            geometry: Geometry {
                dtype: "bf16".to_owned(),
                page_rows: KV_PAGE_ROWS,
                layer_count: 32,
            },
            salt: hs("weft/ekbctx/branch/v1", &[b"salt", &[b as u8]]),
        });
    }

    let mut subjects: Vec<Subject> = Vec::new();
    for (b, branch) in branches.iter().enumerate() {
        let first = b * (KV_PER_BRANCH + TENSOR_PER_BRANCH);
        // 512 KV pages.
        for i in 0..KV_PER_BRANCH {
            let index = first + i;
            let history_variant = (i % 4) as u8;
            let dims = subject_dims(branch, b, history_variant);
            let elements = page_elements(index);
            let input = KvCacheInput {
                model: format!("model-{}-v1", branch.name),
                layer: (i % 32) as u32,
                session: format!("session-{index}"),
                page_index: 0,
                page_count: 1,
                seq_start: 0,
                seq_end: KV_PAGE_ROWS,
                dtype: KvDtype::Bf16,
                head_count: branch.attention.head_count,
                head_dim: branch.attention.head_dim,
            };
            let provenance = KvProvenanceInput {
                model_root: branch.model_root,
                adapter_roots: Vec::new(),
                tokenizer_scheme: "spm".to_owned(),
                tokenizer_vocab_size: 32_000,
                attention: KvAttention {
                    head_count: branch.attention.head_count,
                    head_dim: branch.attention.head_dim,
                    sliding_window: branch.attention.sliding_window,
                    rope_theta_bits: branch.attention.rope_theta_bits,
                    rope_scale_bits: branch.attention.rope_scale_bits,
                },
                positional: if branch.positional == "rope" {
                    KvPositional::Rope
                } else {
                    KvPositional::Alibi
                },
                layout: KvLayout {
                    dtype: KvDtype::Bf16,
                    page_rows: KV_PAGE_ROWS,
                    layer_count: 32,
                },
                salt: branch.salt,
                extensions: Vec::new(),
            };
            let page_name = format!("p{i:04}");
            let written = store
                .write_kv_prov_page(
                    branch.dir_id,
                    page_name.as_bytes(),
                    &branch.tenant,
                    &input,
                    &elements,
                    &provenance,
                    0,
                    0,
                )
                .map_err(|e| format!("kv page {b}/{i}: {e}"))?;
            let record = SubjectRecord {
                kind: SubjectKind::KvProv,
                page_id: written.object_id,
                descriptor_id: written.descriptor_id,
                provenance_root: written.provenance_root,
                dims,
            };
            let bytes = record.canonical_bytes()?;
            let scope_name = format!("s{i:04}");
            let scope_object_id = store
                .create_file_seeded(
                    branch.dir_id,
                    scope_name.as_bytes(),
                    &bytes,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("scope-{b}-{i}").as_bytes(),
                )
                .map_err(|e| e.to_string())?;
            let content_root = store
                .object_meta(written.object_id)
                .map_err(|e| e.to_string())?
                .content;
            let scope_root = store
                .object_meta(scope_object_id)
                .map_err(|e| e.to_string())?
                .content;
            // Tag relation for the subject's Context Root grouping.
            let tag = format!("r{i:02}");
            store
                .relation_add(
                    branch.dir_id,
                    "tag",
                    RelationTarget::Value(Value::text(tag)),
                    None,
                    BTreeMap::new(),
                )
                .map_err(|e| e.to_string())?;
            subjects.push(Subject {
                branch: b,
                kind: SubjectKind::KvProv,
                page_id: written.object_id,
                page_name,
                scope_object_id,
                scope_root,
                content_root,
                descriptor_id: written.descriptor_id,
                provenance_root: written.provenance_root,
                history_variant,
                missing: false,
            });
        }
        // 32 tensor-region keys.
        for i in 0..TENSOR_PER_BRANCH {
            let index = first + KV_PER_BRANCH + i;
            let history_variant = (i % 4) as u8;
            let dims = subject_dims(branch, b, history_variant);
            let bytes = tensor_bytes(index);
            let page_name = format!("t{i:04}");
            let page_id = store
                .create_file_seeded(
                    branch.dir_id,
                    page_name.as_bytes(),
                    &bytes,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("tensor-{b}-{i}").as_bytes(),
                )
                .map_err(|e| e.to_string())?;
            let content_root = store
                .object_meta(page_id)
                .map_err(|e| e.to_string())?
                .content;
            let record = SubjectRecord {
                kind: SubjectKind::TensorRegion,
                page_id,
                descriptor_id: Hash32::ZERO,
                provenance_root: Hash32::ZERO,
                dims,
            };
            let record_bytes = record.canonical_bytes()?;
            let scope_name = format!("s{:04}", KV_PER_BRANCH + i);
            let scope_object_id = store
                .create_file_seeded(
                    branch.dir_id,
                    scope_name.as_bytes(),
                    &record_bytes,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("scope-{b}-{}", KV_PER_BRANCH + i).as_bytes(),
                )
                .map_err(|e| e.to_string())?;
            let scope_root = store
                .object_meta(scope_object_id)
                .map_err(|e| e.to_string())?
                .content;
            let tag = format!("r{index:02}");
            store
                .relation_add(
                    branch.dir_id,
                    "tag",
                    RelationTarget::Value(Value::text(tag)),
                    None,
                    BTreeMap::new(),
                )
                .map_err(|e| e.to_string())?;
            subjects.push(Subject {
                branch: b,
                kind: SubjectKind::TensorRegion,
                page_id,
                page_name,
                scope_object_id,
                scope_root,
                content_root,
                descriptor_id: Hash32::ZERO,
                provenance_root: Hash32::ZERO,
                history_variant,
                missing: false,
            });
        }
        // 8 subjects with no page object at all (persistent missing dependency).
        for i in 0..MISSING_PER_BRANCH {
            let index = first + KV_PER_BRANCH + TENSOR_PER_BRANCH + i;
            let history_variant = (i % 4) as u8;
            let dims = subject_dims(branch, b, history_variant);
            let page_id = hs(
                "weft/object/v1",
                &[b"absent-page", &[b as u8], &(i as u64).to_le_bytes()],
            );
            let record = SubjectRecord {
                kind: SubjectKind::KvProv,
                page_id,
                descriptor_id: Hash32::ZERO,
                provenance_root: Hash32::ZERO,
                dims,
            };
            let record_bytes = record.canonical_bytes()?;
            let scope_name = format!("s{:04}", KV_PER_BRANCH + TENSOR_PER_BRANCH + i);
            let scope_object_id = store
                .create_file_seeded(
                    branch.dir_id,
                    scope_name.as_bytes(),
                    &record_bytes,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("scope-{b}-{}", KV_PER_BRANCH + TENSOR_PER_BRANCH + i).as_bytes(),
                )
                .map_err(|e| e.to_string())?;
            let scope_root = store
                .object_meta(scope_object_id)
                .map_err(|e| e.to_string())?
                .content;
            subjects.push(Subject {
                branch: b,
                kind: SubjectKind::KvProv,
                page_id,
                page_name: String::new(),
                scope_object_id,
                scope_root,
                content_root: Hash32::ZERO,
                descriptor_id: Hash32::ZERO,
                provenance_root: Hash32::ZERO,
                history_variant,
                missing: true,
            });
        }
    }

    // 192 Context Roots (64 per branch).
    let selector = weftcodec::selector::parse_with(
        "select tag(\"r00\")",
        "weft",
        &weftcodec::Limits::default(),
    )
    .map_err(|e| e.to_string())?;
    for b in 0..BRANCHES {
        for i in 0..ROOTS_PER_BRANCH {
            let root_record = weftcodec::context::ContextRoot::new(
                &selector,
                store.snapshot_root(),
                "weftfs-1",
                Vec::new(),
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
            store
                .save_context_root(&format!("root-b{b}-{i:02}"), &root_record, None, false)
                .map_err(|e| e.to_string())?;
        }
    }
    let _ = events_root;

    store.commit_group().map_err(|e| e.to_string())?;
    let contracts = build_contracts(&branches);
    let authorities = build_authorities(&branches, &subjects)?;
    let (hot_pairs, warm_pairs) = build_pairs(&subjects);
    let base_head = store.snapshot_root();
    drop(store);
    Ok(Fixture {
        branches,
        subjects,
        contracts,
        authorities,
        model_v2: hs("weft/ekbctx/branch/v1", &[b"model-v2", &1u8.to_le_bytes()]),
        hot_pairs,
        warm_pairs,
        base_head,
    })
}

fn build_contracts(branches: &[Branch]) -> Vec<Vec<Dimensions>> {
    let mut out = Vec::new();
    for (b, branch) in branches.iter().enumerate() {
        let base = Dimensions {
            model: Some(branch.model_root),
            adapter_set: Some(branch.adapter_set),
            tokenizer: Some(branch.tokenizer),
            attention: Some(branch.attention),
            positional: Some(branch.positional.clone()),
            geometry: Some(branch.geometry.clone()),
            token_history: Some(subject_token_history(b, 0)),
            tenant: Some(branch.tenant.clone()),
            salt: Some(branch.salt),
        };
        let mut slots = Vec::new();
        for v in 0..COMPATIBLE_SLOTS {
            let mut dims = base.clone();
            dims.token_history = Some(subject_token_history(b, v));
            slots.push(dims);
        }
        // 9 single-dimension variants.
        let next_model = branches[(b + 1) % branches.len()].model_root;
        let mut m = base.clone();
        m.model = Some(next_model);
        slots.push(m);
        let mut m = base.clone();
        m.adapter_set = Some(hs("weft/kv/adapters/v1", &[b"single-adapter", &[b as u8]]));
        slots.push(m);
        let mut m = base.clone();
        m.tokenizer = Some(tokenizer_root("bpe", 32_000).unwrap_or(Hash32::ZERO));
        slots.push(m);
        let mut m = base.clone();
        m.attention = Some(Attention {
            head_count: 4,
            head_dim: 16,
            sliding_window: 0,
            rope_theta_bits: branch.attention.rope_theta_bits,
            rope_scale_bits: branch.attention.rope_scale_bits,
        });
        slots.push(m);
        let mut m = base.clone();
        m.positional = Some(if branch.positional == "rope" {
            "alibi".to_owned()
        } else {
            "rope".to_owned()
        });
        slots.push(m);
        let mut m = base.clone();
        m.geometry = Some(Geometry {
            dtype: "fp16".to_owned(),
            page_rows: branch.geometry.page_rows,
            layer_count: branch.geometry.layer_count,
        });
        slots.push(m);
        let mut m = base.clone();
        m.tenant = Some("tenant-other".to_owned());
        slots.push(m);
        let mut m = base.clone();
        m.token_history = Some(hs(
            "weft/kv/exact-prefix/token-history/v1",
            &[b"other-history", &[b as u8]],
        ));
        slots.push(m);
        let mut m = base.clone();
        m.salt = Some(hs("weft/ekbctx/branch/v1", &[b"other-salt", &[b as u8]]));
        slots.push(m);
        // 4 contracts with one required dimension absent.
        let mut m = base.clone();
        m.attention = None;
        slots.push(m);
        let mut m = base.clone();
        m.tokenizer = None;
        slots.push(m);
        let mut m = base.clone();
        m.token_history = None;
        slots.push(m);
        let mut m = base.clone();
        m.geometry = None;
        slots.push(m);
        assert_eq!(slots.len(), CONTRACTS_PER_BRANCH);
        out.push(slots);
    }
    out
}

fn build_authorities(
    branches: &[Branch],
    subjects: &[Subject],
) -> Result<Vec<crate::rule::AuthorityRecord>, String> {
    let mut out = Vec::new();
    for (b, branch) in branches.iter().enumerate() {
        let mut allowed: Vec<Hash32> = subjects
            .iter()
            .filter(|s| s.branch == b)
            .map(|s| s.scope_root)
            .collect();
        allowed.sort();
        allowed.dedup();
        out.push(crate::rule::AuthorityRecord {
            scope: format!("authority-{}", branch.name),
            tenant: branch.tenant.clone(),
            epoch: 1,
            revoked: false,
            allowed_subjects: allowed,
        });
    }
    Ok(out)
}

fn build_pairs(subjects: &[Subject]) -> (Vec<(u32, u8)>, Vec<(u32, u8)>) {
    let live: Vec<usize> = subjects
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.missing)
        .map(|(i, _)| i)
        .collect();
    let mut hot = Vec::new();
    for i in 0..HOT_PAIRS {
        let idx = live[(i * 17 + 5) % live.len()];
        hot.push((idx as u32, subjects[idx].history_variant));
    }
    let mut warm = Vec::new();
    for i in 0..WARM_PAIRS {
        let idx = live[(i * 37 + 11) % live.len()];
        warm.push((idx as u32, subjects[idx].history_variant));
    }
    (hot, warm)
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

fn subject_to_json(subject: &Subject) -> Json {
    json!({
        "branch": subject.branch,
        "kind": subject.kind.as_str(),
        "page_id": subject.page_id.to_hex(),
        "page_name": subject.page_name,
        "scope_object_id": subject.scope_object_id.to_hex(),
        "scope_root": subject.scope_root.to_hex(),
        "content_root": subject.content_root.to_hex(),
        "descriptor_id": subject.descriptor_id.to_hex(),
        "provenance_root": subject.provenance_root.to_hex(),
        "history_variant": subject.history_variant,
        "missing": subject.missing,
    })
}

fn subject_from_json(value: &Json) -> Result<Subject, String> {
    let text = |key: &str| -> Result<String, String> {
        value
            .get(key)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("subject `{key}` is missing"))
    };
    let branch = value
        .get("branch")
        .and_then(Json::as_u64)
        .ok_or("subject branch")? as usize;
    let kind = match text("kind")?.as_str() {
        "kv_prov" => SubjectKind::KvProv,
        "tensor_region" => SubjectKind::TensorRegion,
        other => return Err(format!("unknown subject kind `{other}`")),
    };
    Ok(Subject {
        branch,
        kind,
        page_id: hex32(&text("page_id")?)?,
        page_name: text("page_name")?,
        scope_object_id: hex32(&text("scope_object_id")?)?,
        scope_root: hex32(&text("scope_root")?)?,
        content_root: hex32(&text("content_root")?)?,
        descriptor_id: hex32(&text("descriptor_id")?)?,
        provenance_root: hex32(&text("provenance_root")?)?,
        history_variant: value
            .get("history_variant")
            .and_then(Json::as_u64)
            .ok_or("subject history_variant")? as u8,
        missing: value
            .get("missing")
            .and_then(Json::as_bool)
            .ok_or("subject missing")?,
    })
}

pub fn write_fixture_json(path: &Path, fixture: &Fixture) -> Result<(), String> {
    let branches: Vec<Json> = fixture
        .branches
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "tenant": b.tenant,
                "dir_id": b.dir_id.to_hex(),
                "model_root": b.model_root.to_hex(),
                "adapter_set": b.adapter_set.to_hex(),
                "tokenizer": b.tokenizer.to_hex(),
                "attention": {
                    "head_count": b.attention.head_count,
                    "head_dim": b.attention.head_dim,
                    "sliding_window": b.attention.sliding_window,
                    "rope_theta_bits": b.attention.rope_theta_bits,
                    "rope_scale_bits": b.attention.rope_scale_bits,
                },
                "positional": b.positional,
                "geometry": {
                    "dtype": b.geometry.dtype,
                    "page_rows": b.geometry.page_rows,
                    "layer_count": b.geometry.layer_count,
                },
                "salt": b.salt.to_hex(),
            })
        })
        .collect();
    let contracts: Vec<Json> = fixture
        .contracts
        .iter()
        .map(|slots| Json::Array(slots.iter().map(dims_to_json).collect()))
        .collect();
    let authorities: Vec<Json> = fixture
        .authorities
        .iter()
        .map(|a| {
            json!({
                "scope": a.scope,
                "tenant": a.tenant,
                "epoch": a.epoch,
                "revoked": a.revoked,
                "allowed_subjects": a.allowed_subjects.iter().map(|h| h.to_hex()).collect::<Vec<String>>(),
            })
        })
        .collect();
    let value = json!({
        "schema": "weft.ekbloom_context.fixture.v1",
        "base_head": fixture.base_head.to_hex(),
        "model_v2": fixture.model_v2.to_hex(),
        "branches": branches,
        "subjects": fixture.subjects.iter().map(subject_to_json).collect::<Vec<Json>>(),
        "contracts": contracts,
        "authorities": authorities,
        "hot_pairs": fixture.hot_pairs,
        "warm_pairs": fixture.warm_pairs,
    });
    let text = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_fixture_json(path: &Path) -> Result<Fixture, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: Json = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let branches_value = value
        .get("branches")
        .and_then(Json::as_array)
        .ok_or("fixture branches")?;
    let mut branches = Vec::new();
    for item in branches_value {
        let text = |key: &str| -> Result<String, String> {
            item.get(key)
                .and_then(Json::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("branch `{key}` is missing"))
        };
        let attention = item.get("attention").ok_or("branch attention")?;
        let geometry = item.get("geometry").ok_or("branch geometry")?;
        branches.push(Branch {
            name: text("name")?,
            tenant: text("tenant")?,
            dir_id: hex32(&text("dir_id")?)?,
            model_root: hex32(&text("model_root")?)?,
            adapter_set: hex32(&text("adapter_set")?)?,
            tokenizer: hex32(&text("tokenizer")?)?,
            attention: Attention {
                head_count: attention
                    .get("head_count")
                    .and_then(Json::as_u64)
                    .ok_or("head_count")? as u32,
                head_dim: attention
                    .get("head_dim")
                    .and_then(Json::as_u64)
                    .ok_or("head_dim")? as u32,
                sliding_window: attention
                    .get("sliding_window")
                    .and_then(Json::as_u64)
                    .ok_or("sliding_window")?,
                rope_theta_bits: attention
                    .get("rope_theta_bits")
                    .and_then(Json::as_u64)
                    .ok_or("rope_theta_bits")?,
                rope_scale_bits: attention
                    .get("rope_scale_bits")
                    .and_then(Json::as_u64)
                    .ok_or("rope_scale_bits")?,
            },
            positional: text("positional")?,
            geometry: Geometry {
                dtype: geometry
                    .get("dtype")
                    .and_then(Json::as_str)
                    .ok_or("geometry dtype")?
                    .to_owned(),
                page_rows: geometry
                    .get("page_rows")
                    .and_then(Json::as_u64)
                    .ok_or("geometry page_rows")?,
                layer_count: geometry
                    .get("layer_count")
                    .and_then(Json::as_u64)
                    .ok_or("geometry layer_count")?,
            },
            salt: hex32(&text("salt")?)?,
        });
    }
    let subjects: Vec<Subject> = value
        .get("subjects")
        .and_then(Json::as_array)
        .ok_or("fixture subjects")?
        .iter()
        .map(subject_from_json)
        .collect::<Result<_, _>>()?;
    let contracts: Vec<Vec<Dimensions>> = value
        .get("contracts")
        .and_then(Json::as_array)
        .ok_or("fixture contracts")?
        .iter()
        .map(|slots| {
            slots
                .as_array()
                .ok_or_else(|| "contract branch is not an array".to_owned())
                .and_then(|items| items.iter().map(dims_from_json).collect())
        })
        .collect::<Result<_, String>>()?;
    let authorities = value
        .get("authorities")
        .and_then(Json::as_array)
        .ok_or("fixture authorities")?
        .iter()
        .map(|item| {
            Ok(crate::rule::AuthorityRecord {
                scope: item
                    .get("scope")
                    .and_then(Json::as_str)
                    .ok_or("authority scope")?
                    .to_owned(),
                tenant: item
                    .get("tenant")
                    .and_then(Json::as_str)
                    .ok_or("authority tenant")?
                    .to_owned(),
                epoch: item
                    .get("epoch")
                    .and_then(Json::as_u64)
                    .ok_or("authority epoch")?,
                revoked: item
                    .get("revoked")
                    .and_then(Json::as_bool)
                    .ok_or("authority revoked")?,
                allowed_subjects: item
                    .get("allowed_subjects")
                    .and_then(Json::as_array)
                    .ok_or("authority allowed_subjects")?
                    .iter()
                    .map(|h| hex32(h.as_str().ok_or("authority hash")?))
                    .collect::<Result<_, String>>()?,
            })
        })
        .collect::<Result<_, String>>()?;
    let pairs = |key: &str| -> Result<Vec<(u32, u8)>, String> {
        value
            .get(key)
            .and_then(Json::as_array)
            .ok_or_else(|| format!("fixture `{key}`"))?
            .iter()
            .map(|pair| {
                let pair = pair.as_array().ok_or("pair is not an array")?;
                Ok((
                    pair.first().and_then(Json::as_u64).ok_or("pair subject")? as u32,
                    pair.get(1).and_then(Json::as_u64).ok_or("pair slot")? as u8,
                ))
            })
            .collect()
    };
    Ok(Fixture {
        branches,
        subjects,
        contracts,
        authorities,
        model_v2: hex32(
            value
                .get("model_v2")
                .and_then(Json::as_str)
                .ok_or("fixture model_v2")?,
        )?,
        hot_pairs: pairs("hot_pairs")?,
        warm_pairs: pairs("warm_pairs")?,
        base_head: hex32(
            value
                .get("base_head")
                .and_then(Json::as_str)
                .ok_or("fixture base_head")?,
        )?,
    })
}

// ---------------------------------------------------------------------------
// Request stream
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub subject: u32,
    pub slot: u8,
    pub bucket: u8,
}

pub const BUCKET_HOT: u8 = 0;
pub const BUCKET_WARM: u8 = 1;
pub const BUCKET_COLD: u8 = 2;
pub const BUCKET_VARIANT: u8 = 3;
pub const BUCKET_MISSING_DIM: u8 = 4;
pub const BUCKET_MISSING_EVIDENCE: u8 = 5;

pub fn bucket_name(bucket: u8) -> &'static str {
    match bucket {
        BUCKET_HOT => "hot_exact",
        BUCKET_WARM => "warm_exact",
        BUCKET_COLD => "cold_exact",
        BUCKET_VARIANT => "incompatible_variant",
        BUCKET_MISSING_DIM => "missing_dimension",
        BUCKET_MISSING_EVIDENCE => "missing_evidence",
        _ => "unknown",
    }
}

fn xs(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

pub fn request_stream(fixture: &Fixture, seed: u64) -> Vec<Request> {
    let mut buckets = Vec::with_capacity(REQUESTS);
    for _ in 0..4_400 {
        buckets.push(BUCKET_HOT);
    }
    for _ in 0..1_600 {
        buckets.push(BUCKET_WARM);
    }
    for _ in 0..800 {
        buckets.push(BUCKET_COLD);
    }
    for _ in 0..720 {
        buckets.push(BUCKET_VARIANT);
    }
    for _ in 0..240 {
        buckets.push(BUCKET_MISSING_DIM);
    }
    for _ in 0..240 {
        buckets.push(BUCKET_MISSING_EVIDENCE);
    }
    let mut state = seed | 1;
    for i in (1..buckets.len()).rev() {
        let j = (xs(&mut state) % (i as u64 + 1)) as usize;
        buckets.swap(i, j);
    }
    let live: Vec<u32> = fixture
        .subjects
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.missing)
        .map(|(i, _)| i as u32)
        .collect();
    let absent: Vec<u32> = fixture
        .subjects
        .iter()
        .enumerate()
        .filter(|(_, s)| s.missing)
        .map(|(i, _)| i as u32)
        .collect();
    let mut out = Vec::with_capacity(REQUESTS);
    for bucket in buckets {
        let req = match bucket {
            BUCKET_HOT => {
                let r = xs(&mut state) % 100;
                let pool = if r < 80 { 24 } else { HOT_PAIRS };
                let (subject, slot) = fixture.hot_pairs[(xs(&mut state) as usize) % pool];
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
            BUCKET_WARM => {
                let (subject, slot) =
                    fixture.warm_pairs[(xs(&mut state) as usize) % fixture.warm_pairs.len()];
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
            BUCKET_COLD => {
                let subject = live[(xs(&mut state) as usize) % live.len()];
                let slot = fixture.subjects[subject as usize].history_variant;
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
            BUCKET_VARIANT => {
                let subject = live[(xs(&mut state) as usize) % live.len()];
                let slot = (COMPATIBLE_SLOTS + (xs(&mut state) % u64::from(VARIANT_SLOTS)) as u8)
                    % CONTRACTS_PER_BRANCH as u8;
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
            BUCKET_MISSING_DIM => {
                let subject = live[(xs(&mut state) as usize) % live.len()];
                let slot = COMPATIBLE_SLOTS
                    + VARIANT_SLOTS
                    + (xs(&mut state) % u64::from(MISSING_SLOTS)) as u8;
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
            _ => {
                let subject = absent[(xs(&mut state) as usize) % absent.len()];
                let slot = fixture.subjects[subject as usize].history_variant;
                Request {
                    subject,
                    slot,
                    bucket,
                }
            }
        };
        out.push(req);
    }
    out
}
