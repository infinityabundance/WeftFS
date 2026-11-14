//! The prototype rule `weft.kv.exact-prefix.v1`.
//!
//! This module is deliberately explicit about what a decision depends on: every
//! input is either a content-addressed root, the snapshot commit id, or the
//! authority record root/epoch. The key is built from exactly those values, and
//! the evaluator never reads anything else.

use std::collections::BTreeMap;

use weftcodec::hash::hash_domain;
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;
use weftfs::identity::Hash32;

pub const RULE: &str = "weft.kv.exact-prefix.v1";
pub const RULE_VERSION: u64 = 1;

pub const SUBJECT_SCHEMA: &str = "weft.kv.exact-prefix.subject.v1";
pub const CONTRACT_SCHEMA: &str = "weft.kv.exact-prefix.contract.v1";
pub const DECISION_SCHEMA: &str = "weft.kv.exact-prefix.decision.v1";
pub const AUTHORITY_SCHEMA: &str = "weft.kv.exact-prefix.authority.v1";

pub const SUBJECT_DOMAIN: &str = "weft/kv/exact-prefix/subject/v1";
pub const CONTRACT_DOMAIN: &str = "weft/kv/exact-prefix/contract/v1";
pub const DECISION_DOMAIN: &str = "weft/kv/exact-prefix/decision/v1";
pub const AUTHORITY_DOMAIN: &str = "weft/kv/exact-prefix/authority/v1";
pub const KEY_DOMAIN: &str = "weft/kv/exact-prefix/key/v1";

/// The 144 bytes a manifest stores for exact comparison: the request-key
/// material. The manifest key area is 208 bytes: these 144 plus the page id and
/// page content root used by the post-lookup presence check.
pub const KEY_CORE_LEN: usize = 144;
pub const KEY_AREA_LEN: usize = 208;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Exact,
    Denied,
    Unproven,
}

impl Verdict {
    pub fn as_u8(self) -> u8 {
        match self {
            Verdict::Exact => 0,
            Verdict::Denied => 1,
            Verdict::Unproven => 2,
        }
    }

    pub fn from_u8(value: u8) -> Option<Verdict> {
        match value {
            0 => Some(Verdict::Exact),
            1 => Some(Verdict::Denied),
            2 => Some(Verdict::Unproven),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Exact => "exact",
            Verdict::Denied => "denied",
            Verdict::Unproven => "unproven",
        }
    }
}

// Reason codes are stable within this court. They are part of the decision
// record, so the court can compare not only the verdict but why it was reached.
pub const R_EXACT: u16 = 0;
pub const R_MODEL: u16 = 1;
pub const R_ADAPTER: u16 = 2;
pub const R_TOKENIZER: u16 = 3;
pub const R_ATTENTION: u16 = 4;
pub const R_POSITIONAL: u16 = 5;
pub const R_GEOMETRY: u16 = 6;
pub const R_TOKEN_HISTORY: u16 = 7;
pub const R_TENANT: u16 = 8;
pub const R_SALT: u16 = 9;
pub const R_MISSING_MODEL: u16 = 10;
pub const R_MISSING_ADAPTER: u16 = 11;
pub const R_MISSING_TOKENIZER: u16 = 12;
pub const R_MISSING_ATTENTION: u16 = 13;
pub const R_MISSING_POSITIONAL: u16 = 14;
pub const R_MISSING_GEOMETRY: u16 = 15;
pub const R_MISSING_TOKEN_HISTORY: u16 = 16;
pub const R_MISSING_TENANT: u16 = 17;
pub const R_MISSING_SALT: u16 = 18;
pub const R_SUBJECT_MISSING_MODEL: u16 = 20;
pub const R_SUBJECT_MISSING_ADAPTER: u16 = 21;
pub const R_SUBJECT_MISSING_TOKENIZER: u16 = 22;
pub const R_SUBJECT_MISSING_ATTENTION: u16 = 23;
pub const R_SUBJECT_MISSING_POSITIONAL: u16 = 24;
pub const R_SUBJECT_MISSING_GEOMETRY: u16 = 25;
pub const R_SUBJECT_MISSING_TOKEN_HISTORY: u16 = 26;
pub const R_SUBJECT_MISSING_TENANT: u16 = 27;
pub const R_SUBJECT_MISSING_SALT: u16 = 28;
pub const R_EVIDENCE: u16 = 30;
pub const R_AUTH_REFUSED: u16 = 31;
pub const R_AUTH_UNAVAILABLE: u16 = 32;

pub fn reason_name(code: u16) -> &'static str {
    match code {
        R_EXACT => "exact",
        R_MODEL => "model_mismatch",
        R_ADAPTER => "adapter_mismatch",
        R_TOKENIZER => "tokenizer_mismatch",
        R_ATTENTION => "attention_mismatch",
        R_POSITIONAL => "positional_mismatch",
        R_GEOMETRY => "geometry_mismatch",
        R_TOKEN_HISTORY => "token_history_mismatch",
        R_TENANT => "tenant_mismatch",
        R_SALT => "salt_mismatch",
        R_MISSING_MODEL => "missing_model",
        R_MISSING_ADAPTER => "missing_adapter",
        R_MISSING_TOKENIZER => "missing_tokenizer",
        R_MISSING_ATTENTION => "missing_attention",
        R_MISSING_POSITIONAL => "missing_positional",
        R_MISSING_GEOMETRY => "missing_geometry",
        R_MISSING_TOKEN_HISTORY => "missing_token_history",
        R_MISSING_TENANT => "missing_tenant",
        R_MISSING_SALT => "missing_salt",
        R_SUBJECT_MISSING_MODEL => "subject_missing_model",
        R_SUBJECT_MISSING_ADAPTER => "subject_missing_adapter",
        R_SUBJECT_MISSING_TOKENIZER => "subject_missing_tokenizer",
        R_SUBJECT_MISSING_ATTENTION => "subject_missing_attention",
        R_SUBJECT_MISSING_POSITIONAL => "subject_missing_positional",
        R_SUBJECT_MISSING_GEOMETRY => "subject_missing_geometry",
        R_SUBJECT_MISSING_TOKEN_HISTORY => "subject_missing_token_history",
        R_SUBJECT_MISSING_TENANT => "subject_missing_tenant",
        R_SUBJECT_MISSING_SALT => "subject_missing_salt",
        R_EVIDENCE => "evidence_unavailable",
        R_AUTH_REFUSED => "authority_refused",
        R_AUTH_UNAVAILABLE => "authority_unavailable",
        _ => "unknown_reason",
    }
}

pub fn guarantee_for(verdict: Verdict, reason: u16) -> String {
    match verdict {
        Verdict::Exact => "exact:all_dimensions_equal_and_evidence_verified".to_owned(),
        Verdict::Denied => format!("denied:{}", reason_name(reason)),
        Verdict::Unproven => format!("unproven:{}", reason_name(reason)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attention {
    pub head_count: u32,
    pub head_dim: u32,
    pub sliding_window: u64,
    pub rope_theta_bits: u64,
    pub rope_scale_bits: u64,
}

impl Attention {
    fn to_value(self) -> Value {
        Value::map([
            ("head_count", Value::U64(u64::from(self.head_count))),
            ("head_dim", Value::U64(u64::from(self.head_dim))),
            ("sliding_window", Value::U64(self.sliding_window)),
            ("rope_theta_bits", Value::U64(self.rope_theta_bits)),
            ("rope_scale_bits", Value::U64(self.rope_scale_bits)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Map(map) = value else {
            return Err("attention is not a map".to_owned());
        };
        let u64_of = |key: &str| -> Result<u64, String> {
            match map.get(key) {
                Some(Value::U64(v)) => Ok(*v),
                _ => Err(format!("attention `{key}` is missing or not u64")),
            }
        };
        Ok(Attention {
            head_count: u32::try_from(u64_of("head_count")?)
                .map_err(|_| "attention head_count exceeds u32".to_owned())?,
            head_dim: u32::try_from(u64_of("head_dim")?)
                .map_err(|_| "attention head_dim exceeds u32".to_owned())?,
            sliding_window: u64_of("sliding_window")?,
            rope_theta_bits: u64_of("rope_theta_bits")?,
            rope_scale_bits: u64_of("rope_scale_bits")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub dtype: String,
    pub page_rows: u64,
    pub layer_count: u64,
}

impl Geometry {
    fn to_value(&self) -> Value {
        Value::map([
            ("dtype", Value::Str(self.dtype.clone())),
            ("page_rows", Value::U64(self.page_rows)),
            ("layer_count", Value::U64(self.layer_count)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Map(map) = value else {
            return Err("geometry is not a map".to_owned());
        };
        let text = |key: &str| -> Result<String, String> {
            match map.get(key) {
                Some(Value::Str(v)) => Ok(v.clone()),
                _ => Err(format!("geometry `{key}` is missing or not text")),
            }
        };
        let u64_of = |key: &str| -> Result<u64, String> {
            match map.get(key) {
                Some(Value::U64(v)) => Ok(*v),
                _ => Err(format!("geometry `{key}` is missing or not u64")),
            }
        };
        Ok(Geometry {
            dtype: text("dtype")?,
            page_rows: u64_of("page_rows")?,
            layer_count: u64_of("layer_count")?,
        })
    }
}

/// One complete or partial set of rule dimensions. `None` is absence, not a
/// wildcard: a required dimension that is absent yields `unproven`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dimensions {
    pub model: Option<Hash32>,
    pub adapter_set: Option<Hash32>,
    pub tokenizer: Option<Hash32>,
    pub attention: Option<Attention>,
    pub positional: Option<String>,
    pub geometry: Option<Geometry>,
    pub token_history: Option<Hash32>,
    pub tenant: Option<String>,
    pub salt: Option<Hash32>,
}

impl Dimensions {
    pub fn to_map(&self) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        if let Some(v) = self.model {
            map.insert("model_root".to_owned(), Value::Bytes(v.as_bytes().to_vec()));
        }
        if let Some(v) = self.adapter_set {
            map.insert(
                "adapter_set_root".to_owned(),
                Value::Bytes(v.as_bytes().to_vec()),
            );
        }
        if let Some(v) = self.tokenizer {
            map.insert(
                "tokenizer_root".to_owned(),
                Value::Bytes(v.as_bytes().to_vec()),
            );
        }
        if let Some(v) = self.attention {
            map.insert("attention".to_owned(), v.to_value());
        }
        if let Some(v) = &self.positional {
            map.insert("positional".to_owned(), Value::Str(v.clone()));
        }
        if let Some(v) = &self.geometry {
            map.insert("geometry".to_owned(), v.to_value());
        }
        if let Some(v) = self.token_history {
            map.insert(
                "token_history_root".to_owned(),
                Value::Bytes(v.as_bytes().to_vec()),
            );
        }
        if let Some(v) = &self.tenant {
            map.insert("tenant".to_owned(), Value::Str(v.clone()));
        }
        if let Some(v) = self.salt {
            map.insert("salt".to_owned(), Value::Bytes(v.as_bytes().to_vec()));
        }
        map
    }

    pub fn from_map(map: &BTreeMap<String, Value>) -> Result<Self, String> {
        let hash = |key: &str| -> Result<Option<Hash32>, String> {
            match map.get(key) {
                None => Ok(None),
                Some(Value::Bytes(bytes)) => Hash32::from_slice(bytes)
                    .map(Some)
                    .map_err(|_| format!("`{key}` is not a 32-byte hash")),
                Some(_) => Err(format!("`{key}` is not bytes")),
            }
        };
        let text = |key: &str| -> Result<Option<String>, String> {
            match map.get(key) {
                None => Ok(None),
                Some(Value::Str(text)) => Ok(Some(text.clone())),
                Some(_) => Err(format!("`{key}` is not text")),
            }
        };
        let attention = match map.get("attention") {
            None => None,
            Some(value) => Some(Attention::from_value(value)?),
        };
        let geometry = match map.get("geometry") {
            None => None,
            Some(value) => Some(Geometry::from_value(value)?),
        };
        Ok(Dimensions {
            model: hash("model_root")?,
            adapter_set: hash("adapter_set_root")?,
            tokenizer: hash("tokenizer_root")?,
            attention,
            positional: text("positional")?,
            geometry,
            token_history: hash("token_history_root")?,
            tenant: text("tenant")?,
            salt: hash("salt")?,
        })
    }

    /// The first required dimension that is absent or unequal. Absence in the
    /// *contract* and absence in the *subject* are distinct reasons, so a court
    /// reader can tell a bad request from bad evidence.
    pub fn first_difference(contract: &Dimensions, subject: &Dimensions) -> Option<(Verdict, u16)> {
        macro_rules! check {
            ($c:expr, $s:expr, $miss_c:expr, $miss_s:expr, $neq:expr) => {
                match (&$c, &$s) {
                    (None, _) => return Some((Verdict::Unproven, $miss_c)),
                    (_, None) => return Some((Verdict::Unproven, $miss_s)),
                    (Some(a), Some(b)) if a != b => return Some((Verdict::Denied, $neq)),
                    _ => {}
                }
            };
        }
        check!(
            contract.model,
            subject.model,
            R_MISSING_MODEL,
            R_SUBJECT_MISSING_MODEL,
            R_MODEL
        );
        check!(
            contract.adapter_set,
            subject.adapter_set,
            R_MISSING_ADAPTER,
            R_SUBJECT_MISSING_ADAPTER,
            R_ADAPTER
        );
        check!(
            contract.tokenizer,
            subject.tokenizer,
            R_MISSING_TOKENIZER,
            R_SUBJECT_MISSING_TOKENIZER,
            R_TOKENIZER
        );
        check!(
            contract.attention,
            subject.attention,
            R_MISSING_ATTENTION,
            R_SUBJECT_MISSING_ATTENTION,
            R_ATTENTION
        );
        check!(
            contract.positional,
            subject.positional,
            R_MISSING_POSITIONAL,
            R_SUBJECT_MISSING_POSITIONAL,
            R_POSITIONAL
        );
        check!(
            contract.geometry,
            subject.geometry,
            R_MISSING_GEOMETRY,
            R_SUBJECT_MISSING_GEOMETRY,
            R_GEOMETRY
        );
        check!(
            contract.token_history,
            subject.token_history,
            R_MISSING_TOKEN_HISTORY,
            R_SUBJECT_MISSING_TOKEN_HISTORY,
            R_TOKEN_HISTORY
        );
        check!(
            contract.tenant,
            subject.tenant,
            R_MISSING_TENANT,
            R_SUBJECT_MISSING_TENANT,
            R_TENANT
        );
        check!(
            contract.salt,
            subject.salt,
            R_MISSING_SALT,
            R_SUBJECT_MISSING_SALT,
            R_SALT
        );
        None
    }
}

pub fn contract_bytes(dims: &Dimensions) -> Result<Vec<u8>, String> {
    let mut map = dims.to_map();
    map.insert("schema".to_owned(), Value::Str(CONTRACT_SCHEMA.to_owned()));
    wcb::encode(&Value::Map(map), &Limits::default()).map_err(|e| e.to_string())
}

pub fn contract_root(dims: &Dimensions) -> Result<Hash32, String> {
    Ok(hash_domain(CONTRACT_DOMAIN, &contract_bytes(dims)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubjectKind {
    KvProv,
    TensorRegion,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectKind::KvProv => "kv_prov",
            SubjectKind::TensorRegion => "tensor_region",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubjectRecord {
    pub kind: SubjectKind,
    pub page_id: Hash32,
    pub descriptor_id: Hash32,
    pub provenance_root: Hash32,
    pub dims: Dimensions,
}

impl SubjectRecord {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut map = self.dims.to_map();
        map.insert("schema".to_owned(), Value::Str(SUBJECT_SCHEMA.to_owned()));
        map.insert("kind".to_owned(), Value::Str(self.kind.as_str().to_owned()));
        map.insert(
            "subject".to_owned(),
            Value::Bytes(self.page_id.as_bytes().to_vec()),
        );
        map.insert(
            "descriptor".to_owned(),
            Value::Bytes(self.descriptor_id.as_bytes().to_vec()),
        );
        map.insert(
            "provenance_root".to_owned(),
            Value::Bytes(self.provenance_root.as_bytes().to_vec()),
        );
        wcb::encode(&Value::Map(map), &Limits::default()).map_err(|e| e.to_string())
    }

    pub fn root(&self) -> Result<Hash32, String> {
        Ok(hash_domain(SUBJECT_DOMAIN, &self.canonical_bytes()?))
    }

    /// Parse a canonical subject record. Used only by the builder and the
    /// lifecycle replay; the hot path never parses it in W, because W reads it
    /// from the store and parses the dimensions it needs.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Map(map) = value else {
            return Err("subject record is not a map".to_owned());
        };
        let hash = |key: &str| -> Result<Hash32, String> {
            match map.get(key) {
                Some(Value::Bytes(bytes)) => {
                    Hash32::from_slice(bytes).map_err(|_| format!("`{key}` is not a hash"))
                }
                _ => Err(format!("subject `{key}` is missing or not bytes")),
            }
        };
        let kind = match map.get("kind") {
            Some(Value::Str(text)) if text == "kv_prov" => SubjectKind::KvProv,
            Some(Value::Str(text)) if text == "tensor_region" => SubjectKind::TensorRegion,
            _ => return Err("subject kind is missing or unknown".to_owned()),
        };
        Ok(SubjectRecord {
            kind,
            page_id: hash("subject")?,
            descriptor_id: hash("descriptor")?,
            provenance_root: hash("provenance_root")?,
            dims: Dimensions::from_map(map)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AuthorityRecord {
    pub scope: String,
    pub tenant: String,
    pub epoch: u64,
    pub revoked: bool,
    pub allowed_subjects: Vec<Hash32>,
}

impl AuthorityRecord {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut allowed = self.allowed_subjects.clone();
        allowed.sort();
        let value = Value::map([
            ("schema", Value::Str(AUTHORITY_SCHEMA.to_owned())),
            ("scope", Value::Str(self.scope.clone())),
            ("tenant", Value::Str(self.tenant.clone())),
            ("epoch", Value::U64(self.epoch)),
            ("revoked", Value::Bool(self.revoked)),
            (
                "allowed_subjects",
                Value::Array(
                    allowed
                        .iter()
                        .map(|root| Value::Bytes(root.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
        ]);
        wcb::encode(&value, &Limits::default()).map_err(|e| e.to_string())
    }

    pub fn root(&self) -> Result<Hash32, String> {
        Ok(hash_domain(AUTHORITY_DOMAIN, &self.canonical_bytes()?))
    }

    pub fn allows(&self, subject_root: Hash32, tenant: &str) -> bool {
        !self.revoked
            && self.tenant == tenant
            && self.allowed_subjects.binary_search(&subject_root).is_ok()
    }
}

/// Build the 144-byte request-key core and the 208-byte manifest key area.
pub fn key_material(
    subject_root: Hash32,
    contract_root: Hash32,
    snapshot: Hash32,
    authority_root: Hash32,
    authority_epoch: u64,
    page_id: Hash32,
    page_content_root: Hash32,
) -> [u8; KEY_AREA_LEN] {
    let mut out = [0u8; KEY_AREA_LEN];
    out[0..8].copy_from_slice(&RULE_VERSION.to_le_bytes());
    out[8..40].copy_from_slice(subject_root.as_bytes());
    out[40..72].copy_from_slice(contract_root.as_bytes());
    out[72..104].copy_from_slice(snapshot.as_bytes());
    out[104..136].copy_from_slice(authority_root.as_bytes());
    out[136..144].copy_from_slice(&authority_epoch.to_le_bytes());
    out[144..176].copy_from_slice(page_id.as_bytes());
    out[176..208].copy_from_slice(page_content_root.as_bytes());
    out
}

/// The request key: `H(KEY_DOMAIN, WCB({rule, rule_version, subject, contract,
/// snapshot, authority, epoch}))`. The page id/content are not inputs: they are
/// implied by the subject scope record (which is content-addressed), and the
/// manifest carries them only for the post-lookup presence check.
pub fn request_key_from_core(core: &[u8]) -> Result<Hash32, String> {
    if core.len() < KEY_CORE_LEN {
        return Err("key material is shorter than its 144-byte core".to_owned());
    }
    let subject = Hash32::from_slice(&core[8..40]).map_err(|e| e.to_string())?;
    let contract = Hash32::from_slice(&core[40..72]).map_err(|e| e.to_string())?;
    let snapshot = Hash32::from_slice(&core[72..104]).map_err(|e| e.to_string())?;
    let authority = Hash32::from_slice(&core[104..136]).map_err(|e| e.to_string())?;
    let mut epoch = [0u8; 8];
    epoch.copy_from_slice(&core[136..144]);
    let epoch = u64::from_le_bytes(epoch);
    let value = Value::map([
        ("rule", Value::Str(RULE.to_owned())),
        ("rule_version", Value::U64(RULE_VERSION)),
        ("subject_root", Value::Bytes(subject.as_bytes().to_vec())),
        ("contract_root", Value::Bytes(contract.as_bytes().to_vec())),
        ("snapshot", Value::Bytes(snapshot.as_bytes().to_vec())),
        (
            "authority_root",
            Value::Bytes(authority.as_bytes().to_vec()),
        ),
        ("authority_epoch", Value::U64(epoch)),
    ]);
    let bytes = wcb::encode(&value, &Limits::default()).map_err(|e| e.to_string())?;
    Ok(hash_domain(KEY_DOMAIN, &bytes))
}

pub fn request_key(
    subject_root: Hash32,
    contract_root: Hash32,
    snapshot: Hash32,
    authority_root: Hash32,
    authority_epoch: u64,
) -> Result<Hash32, String> {
    let material = key_material(
        subject_root,
        contract_root,
        snapshot,
        authority_root,
        authority_epoch,
        Hash32::ZERO,
        Hash32::ZERO,
    );
    request_key_from_core(&material)
}

#[derive(Clone, Debug)]
pub struct Evidence {
    pub kind: &'static str,
    pub root: Hash32,
}

#[derive(Clone, Debug)]
pub struct DecisionRecord {
    pub request_key: Hash32,
    pub verdict: Verdict,
    pub reason: u16,
    pub evidence: Vec<Evidence>,
    pub snapshot: Hash32,
    pub authority_epoch: u64,
}

impl DecisionRecord {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let evidence = Value::Array(
            self.evidence
                .iter()
                .map(|e| {
                    Value::map([
                        ("kind", Value::Str(e.kind.to_owned())),
                        ("root", Value::Bytes(e.root.as_bytes().to_vec())),
                    ])
                })
                .collect(),
        );
        let value = Value::map([
            ("schema", Value::Str(DECISION_SCHEMA.to_owned())),
            ("rule", Value::Str(RULE.to_owned())),
            ("rule_version", Value::U64(RULE_VERSION)),
            (
                "request_key",
                Value::Bytes(self.request_key.as_bytes().to_vec()),
            ),
            ("verdict", Value::Str(self.verdict.as_str().to_owned())),
            (
                "established_guarantee",
                Value::Str(guarantee_for(self.verdict, self.reason)),
            ),
            ("evidence", evidence),
            (
                "evaluated_snapshot",
                Value::Bytes(self.snapshot.as_bytes().to_vec()),
            ),
            ("authority_epoch", Value::U64(self.authority_epoch)),
            (
                "reasons",
                Value::Array(vec![Value::Str(reason_name(self.reason).to_owned())]),
            ),
        ]);
        wcb::encode(&value, &Limits::default()).map_err(|e| e.to_string())
    }

    pub fn root(&self) -> Result<Hash32, String> {
        Ok(hash_domain(DECISION_DOMAIN, &self.canonical_bytes()?))
    }
}
