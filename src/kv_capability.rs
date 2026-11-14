//! Zero-trust KV-cache pages (`WEFT-AI-CODEC.md` mode 12).
//!
//! A KV page is a canonical mode-12 frame plus a canonical
//! `weft.kv.capability.v1` descriptor that names the tenant, model version,
//! session, layer, page position and token range the page belongs to. The
//! descriptor is content-addressed and its id is what an authenticated
//! capability envelope grants (in `roots`), alongside a coarse `kv.<tenant>`
//! namespace (in `namespaces`).
//!
//! Read discipline, normative for this surface and implemented here:
//!
//! 1. Every failure **before a byte is returned** — unknown object, missing or
//!    malformed descriptor, missing/expired/foreign envelope, POSIX denial — is
//!    the *same* `Unauthorized` error with the *same* text. Nothing about the
//!    object's existence or its contents is disclosed.
//! 2. A content id is not a credential. Knowing the `ContentId` (or the
//!    `ObjectId`) authorises nothing.
//! 3. Only after authorization is the frame decoded, and then the descriptor is
//!    re-derived from the frame metadata and compared field by field. A
//!    mismatch is `Corrupt`, and it is reported to a reader that is already
//!    authorized.
//!
//! The mode-12 frame is frozen and unchanged: the tenant is carried by the
//! canonical descriptor and by object attributes, never by a new frame field.

use std::collections::BTreeMap;

use weftcodec::frame::Frame;
use weftcodec::kv_cache::{self, DecodedKvPage, KvCacheInput};
use weftcodec::kv_prov::{self, DecodedKvProvPage, KvProvenanceInput};
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::auth::Principal;
use crate::context::CapabilityContext;
use crate::error::{Error, Result};
use crate::identity::Hash32;
use crate::phase_profile::{self, Phase};
use crate::state::ObjectRecord;
use crate::store::Store;

/// Canonical descriptor schema.
pub const KV_DESCRIPTOR_SCHEMA: &str = "weft.kv.capability.v1";
/// Descriptor identity domain.
pub const DOMAIN_KV_DESCRIPTOR: &str = "weft/kv/capability/v1";
/// Attribute holding the canonical descriptor bytes (hex).
pub const ATTR_KV_DESCRIPTOR: &str = "weft.kv.descriptor";
/// Attribute holding the descriptor id (hex).
pub const ATTR_KV_DESCRIPTOR_ID: &str = "weft.kv.descriptor_id";
/// Attribute: tenant the page belongs to.
pub const ATTR_KV_TENANT: &str = "weft.kv.tenant";
/// Attribute: model version the page was produced by.
pub const ATTR_KV_MODEL: &str = "weft.kv.model";
/// Attribute: inference session id.
pub const ATTR_KV_SESSION: &str = "weft.kv.session";
/// Namespace prefix a capability envelope must grant: `kv.<tenant>`.
pub const KV_NAMESPACE_PREFIX: &str = "kv.";
/// Attribute recording the frame mode of a KV page (19 for provenance-bound).
/// Absent means mode 12, which keeps every pre-existing page readable.
pub const ATTR_KV_MODE: &str = "weft.kv.mode";
/// Attribute holding the mode-19 descriptor root (hex).
pub const ATTR_KV_PROV_ROOT: &str = "weft.kv.provenance_root";
/// Canonical provenance-bound capability descriptor schema.
pub const KV_PROV_DESCRIPTOR_SCHEMA: &str = "weft.kv.prov_capability.v1";
/// Provenance-bound capability descriptor identity domain.
pub const DOMAIN_KV_PROV_DESCRIPTOR: &str = "weft/kv/prov_capability/v1";

/// The single non-disclosing denial text. Every pre-byte failure uses it.
pub const KV_DENIED: &str = "kv page is not available to this principal";

fn denied() -> Error {
    Error::new(crate::error::Class::Unauthorized, KV_DENIED)
}

/// Maximum descriptors retained per store handle.
pub const MAX_KV_DESCRIPTOR_CACHE: usize = 4_096;
/// Maximum decoded capability envelopes retained per store handle.
pub const MAX_KV_CAPABILITY_CACHE: usize = 1_024;

/// One decoded capability envelope, kept beside the envelope it came from.
///
/// The envelope is retained so a cache hit can be *proved* to belong to the
/// principal's envelope; the ids below are a lookup hint, never the proof.
struct CachedCapability {
    envelope: weftcodec::capability::CapabilityEnvelope,
    context: std::sync::Arc<CapabilityContext>,
    /// Store generation the decode was produced under.
    generation: u64,
    /// Revocation epoch at that generation.
    revocation_epoch: u64,
}

/// Bounded caches of derived decode results for the KV read fast path.
///
/// Both caches hold **decode results only**, never an authorization decision:
///
/// * `descriptors` maps a stored object's `(id, content root, record version)`
///   to a descriptor whose identity was checked against its canonical bytes when
///   it was inserted. Those three values are fields of the object record, so a
///   hit cannot describe a different record, and the whole map is dropped
///   whenever the store's head generation changes.
/// * `capabilities` maps a *verified* envelope's `(envelope_id, blob_id)` to the
///   decoded `CapabilityContext`. A hit is only used when the cached envelope is
///   equal to the principal's envelope, so a forged id pair cannot serve another
///   grant's scope.
///
/// Every read still evaluates the validity window, the tenant, the namespace
/// grant, the root grant and the POSIX/ACL rules, in that order, against the
/// cached decode. Revoking a grant, expiring an envelope or changing an object
/// record therefore changes the answer on the very next read.
#[derive(Default)]
pub struct KvReadCache {
    generation: u64,
    descriptors: std::collections::HashMap<(Hash32, Hash32, Hash32), (StoredKvDescriptor, Hash32)>,
    capabilities: std::collections::HashMap<(Hash32, Hash32), CachedCapability>,
    /// Memoised revocation epoch for one generation. A revocation is a commit,
    /// so a generation change always re-derives it; the cache can never use an
    /// epoch older than the state it is answering for.
    revocation_epoch: Option<(u64, u64)>,
    /// Grant-cache counters (T065).
    grant_hits: u64,
    grant_misses: u64,
    grant_invalidations: u64,
}

impl KvReadCache {
    /// Drop every descriptor decoded under a different head generation.
    fn fence(&mut self, generation: u64) {
        if self.generation != generation {
            self.descriptors.clear();
            self.generation = generation;
        }
    }

    /// Number of retained descriptors (diagnostics and tests).
    pub fn descriptor_entries(&self) -> usize {
        self.descriptors.len()
    }

    /// Number of retained capability decodes (diagnostics and tests).
    pub fn capability_entries(&self) -> usize {
        self.capabilities.len()
    }

    /// Grant-cache counters: `(hits, misses, invalidations)`.
    pub fn grant_counters(&self) -> (u64, u64, u64) {
        (self.grant_hits, self.grant_misses, self.grant_invalidations)
    }
}

/// A KV page's capability descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvDescriptor {
    pub tenant: String,
    pub model: String,
    pub session: String,
    pub layer: u32,
    pub page_index: u64,
    pub page_count: u64,
    pub seq_start: u64,
    pub seq_end: u64,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub shape: Vec<u64>,
    pub page_bytes: u64,
}

impl KvDescriptor {
    /// Build a descriptor from a mode-12 input, its tenant, and the exact page
    /// bytes. The shape is the mode-12 canonical `[page_rows, head_count,
    /// head_dim]` (the frozen frame format, not an extension).
    pub fn from_input(input: &KvCacheInput, tenant: &str, page_bytes: u64) -> Result<Self> {
        let rows = input
            .seq_end
            .checked_sub(input.seq_start)
            .ok_or_else(|| Error::invalid_argument("kv page seq_start must be below seq_end"))?;
        let shape = vec![rows, u64::from(input.head_count), u64::from(input.head_dim)];
        Ok(KvDescriptor {
            tenant: tenant.to_owned(),
            model: input.model.clone(),
            session: input.session.clone(),
            layer: input.layer,
            page_index: input.page_index,
            page_count: input.page_count,
            seq_start: input.seq_start,
            seq_end: input.seq_end,
            dtype: input.dtype.as_str().to_owned(),
            head_count: input.head_count,
            head_dim: input.head_dim,
            shape,
            page_bytes,
        })
    }

    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(KV_DESCRIPTOR_SCHEMA.to_owned()),
        );
        map.insert("tenant".to_owned(), Value::Str(self.tenant.clone()));
        map.insert("model".to_owned(), Value::Str(self.model.clone()));
        map.insert("session".to_owned(), Value::Str(self.session.clone()));
        map.insert("layer".to_owned(), Value::U64(u64::from(self.layer)));
        map.insert("page_index".to_owned(), Value::U64(self.page_index));
        map.insert("page_count".to_owned(), Value::U64(self.page_count));
        map.insert("seq_start".to_owned(), Value::U64(self.seq_start));
        map.insert("seq_end".to_owned(), Value::U64(self.seq_end));
        map.insert("dtype".to_owned(), Value::Str(self.dtype.clone()));
        map.insert(
            "head_count".to_owned(),
            Value::U64(u64::from(self.head_count)),
        );
        map.insert("head_dim".to_owned(), Value::U64(u64::from(self.head_dim)));
        map.insert(
            "shape".to_owned(),
            Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
        );
        map.insert("page_bytes".to_owned(), Value::U64(self.page_bytes));
        Value::Map(map)
    }

    /// Canonical WCB bytes (sorted keys, minimal varints).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// `H("weft/kv/capability/v1", canonical bytes)`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(crate::identity::h(
            DOMAIN_KV_DESCRIPTOR,
            &self.canonical_bytes()?,
        ))
    }

    /// Strict decode: unknown keys, non-canonical forms, and missing fields are
    /// rejected.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let Value::Map(map) = value else {
            return Err(Error::corrupt("kv descriptor is not a map"));
        };
        let mut seen = 0usize;
        for key in map.keys() {
            match key.as_str() {
                "schema" | "tenant" | "model" | "session" | "layer" | "page_index"
                | "page_count" | "seq_start" | "seq_end" | "dtype" | "head_count" | "head_dim"
                | "shape" | "page_bytes" => seen += 1,
                other => {
                    return Err(Error::corrupt(format!(
                        "unknown kv descriptor key `{other}`"
                    )))
                }
            }
        }
        if seen != 14 {
            return Err(Error::corrupt("kv descriptor is missing a field"));
        }
        let text = |key: &str| -> Result<String> {
            match map.get(key) {
                Some(Value::Str(text)) => Ok(text.clone()),
                _ => Err(Error::corrupt(format!("kv descriptor `{key}` is not text"))),
            }
        };
        let number = |key: &str| -> Result<u64> {
            match map.get(key) {
                Some(Value::U64(value)) => Ok(*value),
                _ => Err(Error::corrupt(format!("kv descriptor `{key}` is not u64"))),
            }
        };
        if text("schema")? != KV_DESCRIPTOR_SCHEMA {
            return Err(Error::corrupt("unknown kv descriptor schema"));
        }
        let shape = match map.get("shape") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::U64(dim) => Ok(*dim),
                    _ => Err(Error::corrupt("kv descriptor shape entry is not u64")),
                })
                .collect::<Result<Vec<u64>>>()?,
            _ => return Err(Error::corrupt("kv descriptor `shape` is not an array")),
        };
        let descriptor = KvDescriptor {
            tenant: text("tenant")?,
            model: text("model")?,
            session: text("session")?,
            layer: u32::try_from(number("layer")?)
                .map_err(|_| Error::corrupt("kv descriptor layer does not fit u32"))?,
            page_index: number("page_index")?,
            page_count: number("page_count")?,
            seq_start: number("seq_start")?,
            seq_end: number("seq_end")?,
            dtype: text("dtype")?,
            head_count: u32::try_from(number("head_count")?)
                .map_err(|_| Error::corrupt("kv descriptor head_count does not fit u32"))?,
            head_dim: u32::try_from(number("head_dim")?)
                .map_err(|_| Error::corrupt("kv descriptor head_dim does not fit u32"))?,
            shape,
            page_bytes: number("page_bytes")?,
        };
        // Canonicality: the re-encoded bytes must be identical.
        if descriptor.canonical_bytes()? != bytes {
            return Err(Error::corrupt("kv descriptor is not canonical"));
        }
        Ok(descriptor)
    }

    /// The namespace a grant must include: `kv.<tenant>`.
    pub fn namespace(&self) -> String {
        format!("{KV_NAMESPACE_PREFIX}{}", self.tenant)
    }
}

/// The capability descriptor of a provenance-bound (mode 19) KV page.
///
/// It carries the same page identity fields as [`KvDescriptor`] plus the
/// codec's `content_root`, so a capability envelope that names this descriptor
/// id names the *provenance* as well as the page. Two pages with identical page
/// metadata but different provenance have different ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvProvDescriptor {
    pub tenant: String,
    pub model: String,
    pub session: String,
    pub layer: u32,
    pub page_index: u64,
    pub page_count: u64,
    pub seq_start: u64,
    pub seq_end: u64,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub shape: Vec<u64>,
    pub page_bytes: u64,
    pub provenance_root: Hash32,
}

impl KvProvDescriptor {
    /// Build a descriptor from a mode-19 input, its tenant, exact page bytes and
    /// the codec `descriptor_root`.
    pub fn from_input(
        input: &KvCacheInput,
        tenant: &str,
        page_bytes: u64,
        provenance_root: Hash32,
    ) -> Result<Self> {
        let rows = input
            .seq_end
            .checked_sub(input.seq_start)
            .ok_or_else(|| Error::invalid_argument("kv page seq_start must be below seq_end"))?;
        Ok(KvProvDescriptor {
            tenant: tenant.to_owned(),
            model: input.model.clone(),
            session: input.session.clone(),
            layer: input.layer,
            page_index: input.page_index,
            page_count: input.page_count,
            seq_start: input.seq_start,
            seq_end: input.seq_end,
            dtype: input.dtype.as_str().to_owned(),
            head_count: input.head_count,
            head_dim: input.head_dim,
            shape: vec![rows, u64::from(input.head_count), u64::from(input.head_dim)],
            page_bytes,
            provenance_root,
        })
    }

    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(KV_PROV_DESCRIPTOR_SCHEMA.to_owned()),
        );
        map.insert("tenant".to_owned(), Value::Str(self.tenant.clone()));
        map.insert("model".to_owned(), Value::Str(self.model.clone()));
        map.insert("session".to_owned(), Value::Str(self.session.clone()));
        map.insert("layer".to_owned(), Value::U64(u64::from(self.layer)));
        map.insert("page_index".to_owned(), Value::U64(self.page_index));
        map.insert("page_count".to_owned(), Value::U64(self.page_count));
        map.insert("seq_start".to_owned(), Value::U64(self.seq_start));
        map.insert("seq_end".to_owned(), Value::U64(self.seq_end));
        map.insert("dtype".to_owned(), Value::Str(self.dtype.clone()));
        map.insert(
            "head_count".to_owned(),
            Value::U64(u64::from(self.head_count)),
        );
        map.insert("head_dim".to_owned(), Value::U64(u64::from(self.head_dim)));
        map.insert(
            "shape".to_owned(),
            Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
        );
        map.insert("page_bytes".to_owned(), Value::U64(self.page_bytes));
        map.insert(
            "provenance_root".to_owned(),
            Value::Str(self.provenance_root.to_hex()),
        );
        Value::Map(map)
    }

    /// Canonical WCB bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// `H("weft/kv/prov_capability/v1", canonical bytes)`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(crate::identity::h(
            DOMAIN_KV_PROV_DESCRIPTOR,
            &self.canonical_bytes()?,
        ))
    }

    /// Strict decode.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let Value::Map(map) = value else {
            return Err(Error::corrupt("kv provenance descriptor is not a map"));
        };
        let keys = [
            "schema",
            "tenant",
            "model",
            "session",
            "layer",
            "page_index",
            "page_count",
            "seq_start",
            "seq_end",
            "dtype",
            "head_count",
            "head_dim",
            "shape",
            "page_bytes",
            "provenance_root",
        ];
        for key in map.keys() {
            if !keys.contains(&key.as_str()) {
                return Err(Error::corrupt(format!(
                    "unknown kv provenance descriptor key `{key}`"
                )));
            }
        }
        if map.len() != keys.len() {
            return Err(Error::corrupt(
                "kv provenance descriptor is missing a field",
            ));
        }
        let text = |key: &str| -> Result<String> {
            match map.get(key) {
                Some(Value::Str(text)) => Ok(text.clone()),
                _ => Err(Error::corrupt(format!(
                    "kv provenance descriptor `{key}` is not text"
                ))),
            }
        };
        let number = |key: &str| -> Result<u64> {
            match map.get(key) {
                Some(Value::U64(value)) => Ok(*value),
                _ => Err(Error::corrupt(format!(
                    "kv provenance descriptor `{key}` is not u64"
                ))),
            }
        };
        if text("schema")? != KV_PROV_DESCRIPTOR_SCHEMA {
            return Err(Error::corrupt("unknown kv provenance descriptor schema"));
        }
        let shape = match map.get("shape") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::U64(dim) => Ok(*dim),
                    _ => Err(Error::corrupt(
                        "kv provenance descriptor shape entry is not u64",
                    )),
                })
                .collect::<Result<Vec<u64>>>()?,
            _ => {
                return Err(Error::corrupt(
                    "kv provenance descriptor `shape` is not an array",
                ))
            }
        };
        let descriptor = KvProvDescriptor {
            tenant: text("tenant")?,
            model: text("model")?,
            session: text("session")?,
            layer: u32::try_from(number("layer")?)
                .map_err(|_| Error::corrupt("kv provenance descriptor layer exceeds u32"))?,
            page_index: number("page_index")?,
            page_count: number("page_count")?,
            seq_start: number("seq_start")?,
            seq_end: number("seq_end")?,
            dtype: text("dtype")?,
            head_count: u32::try_from(number("head_count")?)
                .map_err(|_| Error::corrupt("kv provenance descriptor head_count exceeds u32"))?,
            head_dim: u32::try_from(number("head_dim")?)
                .map_err(|_| Error::corrupt("kv provenance descriptor head_dim exceeds u32"))?,
            shape,
            page_bytes: number("page_bytes")?,
            provenance_root: Hash32::from_hex(&text("provenance_root")?)
                .map_err(|_| Error::corrupt("kv provenance root is not a hash"))?,
        };
        if descriptor.canonical_bytes()? != bytes {
            return Err(Error::corrupt("kv provenance descriptor is not canonical"));
        }
        Ok(descriptor)
    }

    /// The namespace a grant must include: `kv.<tenant>`.
    pub fn namespace(&self) -> String {
        format!("{KV_NAMESPACE_PREFIX}{}", self.tenant)
    }
}

/// Frame mode of a stored KV page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvPageMode {
    Page12,
    Provenance,
}

impl KvPageMode {
    pub fn value(self) -> u64 {
        match self {
            KvPageMode::Page12 => 12,
            KvPageMode::Provenance => 19,
        }
    }
}

/// A stored KV capability descriptor, either mode 12 or mode 19.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredKvDescriptor {
    Page(KvDescriptor),
    Provenance(KvProvDescriptor),
}

impl StoredKvDescriptor {
    pub fn mode(&self) -> KvPageMode {
        match self {
            StoredKvDescriptor::Page(_) => KvPageMode::Page12,
            StoredKvDescriptor::Provenance(_) => KvPageMode::Provenance,
        }
    }

    pub fn descriptor_id(&self) -> Result<Hash32> {
        match self {
            StoredKvDescriptor::Page(descriptor) => descriptor.id(),
            StoredKvDescriptor::Provenance(descriptor) => descriptor.id(),
        }
    }

    pub fn namespace(&self) -> String {
        match self {
            StoredKvDescriptor::Page(descriptor) => descriptor.namespace(),
            StoredKvDescriptor::Provenance(descriptor) => descriptor.namespace(),
        }
    }

    pub fn tenant(&self) -> &str {
        match self {
            StoredKvDescriptor::Page(descriptor) => &descriptor.tenant,
            StoredKvDescriptor::Provenance(descriptor) => &descriptor.tenant,
        }
    }
}

/// Result of writing one provenance-bound KV page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvProvPageWrite {
    pub object_id: Hash32,
    pub descriptor_id: Hash32,
    pub provenance_root: Hash32,
    pub frame_bytes: u64,
    pub page_bytes: u64,
}

/// Result of writing one KV page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvPageWrite {
    pub object_id: Hash32,
    pub descriptor_id: Hash32,
    pub frame_bytes: u64,
    pub page_bytes: u64,
}

/// Attributes a KV page carries, from its descriptor.
fn descriptor_attrs(descriptor: &KvDescriptor) -> Result<BTreeMap<String, Value>> {
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
        ATTR_KV_TENANT.to_owned(),
        Value::Str(descriptor.tenant.clone()),
    );
    attrs.insert(
        ATTR_KV_MODEL.to_owned(),
        Value::Str(descriptor.model.clone()),
    );
    attrs.insert(
        ATTR_KV_SESSION.to_owned(),
        Value::Str(descriptor.session.clone()),
    );
    attrs.insert(
        ATTR_KV_PAGE_BYTES.to_owned(),
        Value::U64(descriptor.page_bytes),
    );
    Ok(attrs)
}

/// Attributes a provenance-bound KV page carries, from its descriptor.
fn prov_descriptor_attrs(descriptor: &KvProvDescriptor) -> Result<BTreeMap<String, Value>> {
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
        ATTR_KV_TENANT.to_owned(),
        Value::Str(descriptor.tenant.clone()),
    );
    attrs.insert(
        ATTR_KV_MODEL.to_owned(),
        Value::Str(descriptor.model.clone()),
    );
    attrs.insert(
        ATTR_KV_SESSION.to_owned(),
        Value::Str(descriptor.session.clone()),
    );
    attrs.insert(
        ATTR_KV_PAGE_BYTES.to_owned(),
        Value::U64(descriptor.page_bytes),
    );
    attrs.insert(
        ATTR_KV_MODE.to_owned(),
        Value::U64(KvPageMode::Provenance.value()),
    );
    attrs.insert(
        ATTR_KV_PROV_ROOT.to_owned(),
        Value::Str(descriptor.provenance_root.to_hex()),
    );
    Ok(attrs)
}

impl Store {
    /// Write one mode-12 KV page guarded by a canonical capability descriptor.
    ///
    /// The frame is the frozen mode-12 encoding; the descriptor (tenant, model
    /// version, session, layer, page position, token range, dtype, shape, exact
    /// page length) is stored as canonical WCB in reserved attributes, and its
    /// id is `H("weft/kv/capability/v1", descriptor)`.
    #[allow(clippy::too_many_arguments)]
    pub fn write_kv_page(
        &mut self,
        parent: Hash32,
        name: &[u8],
        tenant: &str,
        input: &KvCacheInput,
        elements: &[u8],
        uid: u64,
        gid: u64,
    ) -> Result<KvPageWrite> {
        let limits = Limits::default();
        let frame = kv_cache::encode_kv_page(input, elements, &limits).map_err(Error::from)?;
        let descriptor = KvDescriptor::from_input(input, tenant, elements.len() as u64)?;
        let descriptor_id = descriptor.id()?;
        // Round-trip the descriptor before publishing: a page whose descriptor
        // does not decode back to itself is never written.
        let attrs = descriptor_attrs(&descriptor)?;
        let object_id = self.create_file(parent, name, &frame, 0o644, uid, gid, attrs)?;
        Ok(KvPageWrite {
            object_id,
            descriptor_id,
            frame_bytes: frame.len() as u64,
            page_bytes: elements.len() as u64,
        })
    }

    /// Write one provenance-bound (mode 19) KV page.
    ///
    /// The frame is the additive mode-19 encoding (the mode-12 payload under a
    /// bound descriptor). The capability descriptor is the mode-12 descriptor
    /// plus the codec `content_root`, so a capability envelope that names its
    /// id names the provenance as well as the page.
    #[allow(clippy::too_many_arguments)]
    pub fn write_kv_prov_page(
        &mut self,
        parent: Hash32,
        name: &[u8],
        tenant: &str,
        input: &KvCacheInput,
        elements: &[u8],
        provenance: &KvProvenanceInput,
        uid: u64,
        gid: u64,
    ) -> Result<KvProvPageWrite> {
        let limits = Limits::default();
        let frame = kv_prov::encode_kv_prov_page(input, elements, provenance, &limits)
            .map_err(Error::from)?;
        // Round-trip through the codec so the stored descriptor root is the one
        // a decoder will recompute, never a caller-supplied value.
        let parsed = Frame::parse(&frame, &limits).map_err(Error::from)?;
        let codec = kv_prov::kv_prov_metadata(&parsed, &limits).map_err(Error::from)?;
        let provenance_root = codec.root(&limits).map_err(Error::from)?;
        let descriptor =
            KvProvDescriptor::from_input(input, tenant, elements.len() as u64, provenance_root)?;
        let descriptor_id = descriptor.id()?;
        let attrs = prov_descriptor_attrs(&descriptor)?;
        let object_id = self.create_file(parent, name, &frame, 0o644, uid, gid, attrs)?;
        Ok(KvProvPageWrite {
            object_id,
            descriptor_id,
            provenance_root,
            frame_bytes: frame.len() as u64,
            page_bytes: elements.len() as u64,
        })
    }

    /// The descriptor of a stored page, if it has a well-formed one.
    ///
    /// This is *not* an authorization path: it exists so verification and
    /// administration can inspect identity. It never returns page bytes.
    pub fn kv_descriptor(&self, id: Hash32) -> Result<KvDescriptor> {
        let record = self.object_meta(id)?;
        descriptor_of(&record)
    }

    /// The validated descriptor of `record`, and its id.
    ///
    /// The decode is a pure function of the object record, so the result is
    /// retained in the head-fenced [`KvReadCache`]. A hit is only ever a
    /// descriptor that was checked against its own canonical bytes and recorded id
    /// when it was inserted; a record created by any later write has a different
    /// `(id, content, version)` triple, and every entry is dropped when the head
    /// generation changes.
    fn kv_descriptor_of(&self, record: &ObjectRecord) -> Result<(StoredKvDescriptor, Hash32)> {
        let key = (record.id, record.content, record.version);
        {
            let mut cache = self
                .kv_read_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.fence(self.generation());
            if let Some(hit) = cache.descriptors.get(&key) {
                return Ok(hit.clone());
            }
        }
        let descriptor = descriptor_of_any(record)?;
        let descriptor_id = descriptor.descriptor_id()?;
        let mut cache = self
            .kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.fence(self.generation());
        if cache.descriptors.len() >= MAX_KV_DESCRIPTOR_CACHE {
            cache.descriptors.clear();
        }
        cache
            .descriptors
            .insert(key, (descriptor.clone(), descriptor_id));
        Ok((descriptor, descriptor_id))
    }

    /// The decoded capability carried by `principal`'s verified envelope.
    ///
    /// `None` means the principal carries no envelope. A cached decode is only
    /// reused when it was produced from an envelope equal to this one, so the
    /// entry cannot answer for a different grant. Nothing about *authorization*
    /// is cached: every caller still evaluates the validity window and the
    /// grants on the returned context.
    fn kv_capability_of(
        &self,
        principal: &Principal,
    ) -> Result<Option<std::sync::Arc<CapabilityContext>>> {
        let Some(verified) = principal.envelope() else {
            return Ok(None);
        };
        let key = (verified.envelope_id, verified.blob_id);
        if !self.options().kv_grant_cache {
            // The control path: decode the grant on every read.
            self.kv_read_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .grant_misses += 1;
            return Ok(Some(std::sync::Arc::new(CapabilityContext::from_envelope(
                &verified.envelope,
            )?)));
        }
        let generation = self.generation();
        let epoch = self.kv_revocation_epoch_cached()?;
        {
            let mut cache = self
                .kv_read_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // A hit must match the envelope, the generation and the revocation
            // epoch. `generation` changes on every commit, so a grant cached
            // before *any* state change is never reused.
            let hit = cache.capabilities.get(&key).and_then(|cached| {
                (cached.envelope == verified.envelope
                    && cached.generation == generation
                    && cached.revocation_epoch == epoch)
                    .then(|| std::sync::Arc::clone(&cached.context))
            });
            if let Some(context) = hit {
                cache.grant_hits += 1;
                return Ok(Some(context));
            }
        }
        let context = std::sync::Arc::new(CapabilityContext::from_envelope(&verified.envelope)?);
        let mut cache = self
            .kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = cache.capabilities.get(&key) {
            if previous.generation != generation || previous.revocation_epoch != epoch {
                cache.grant_invalidations += 1;
            }
        }
        if cache.capabilities.len() >= MAX_KV_CAPABILITY_CACHE {
            cache.capabilities.clear();
        }
        cache.grant_misses += 1;
        cache.capabilities.insert(
            key,
            CachedCapability {
                envelope: verified.envelope.clone(),
                context: std::sync::Arc::clone(&context),
                generation,
                revocation_epoch: epoch,
            },
        );
        Ok(Some(context))
    }

    /// Revocation epoch memoised for one generation.
    fn kv_revocation_epoch_cached(&self) -> Result<u64> {
        let generation = self.generation();
        {
            let cache = self
                .kv_read_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached_generation, epoch)) = cache.revocation_epoch {
                if cached_generation == generation {
                    return Ok(epoch);
                }
            }
        }
        let epoch = self.kv_revocation_epoch()?;
        let mut cache = self
            .kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.revocation_epoch = Some((generation, epoch));
        Ok(epoch)
    }

    /// Grant-cache counters: `(hits, misses, invalidations)`.
    pub fn kv_grant_cache_counters(&self) -> (u64, u64, u64) {
        self.kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .grant_counters()
    }

    /// Retained descriptor decodes in this handle's read cache.
    ///
    /// The cache is fenced before it is reported, so the count is always the
    /// number of entries valid at the current head generation: a stale entry is
    /// dropped rather than counted.
    pub fn kv_read_cache_descriptors(&self) -> usize {
        let mut cache = self
            .kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.fence(self.generation());
        cache.descriptor_entries()
    }

    /// Retained capability decodes in this handle's read cache (diagnostics).
    pub fn kv_read_cache_capabilities(&self) -> usize {
        self.kv_read_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capability_entries()
    }

    /// The provenance-bound descriptor of a stored mode-19 page.
    pub fn kv_prov_descriptor(&self, id: Hash32) -> Result<KvProvDescriptor> {
        let record = self.object_meta(id)?;
        descriptor_of_prov(&record)
    }

    /// The frame mode of a stored KV page (metadata only, no bytes).
    pub fn kv_page_mode(&self, id: Hash32) -> Result<KvPageMode> {
        let record = self.object_meta(id)?;
        Ok(descriptor_of_any(&record)?.mode())
    }

    /// The shared authorization preamble.
    ///
    /// Ordering is normative and unchanged from the original mode-12 path:
    /// object record → descriptor → derived descriptor id → envelope validity →
    /// tenant/namespace/root grant → POSIX authorization → read. Every failure
    /// before a byte is returned is the same [`KV_DENIED`].
    fn read_kv_authorized(
        &self,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<(StoredKvDescriptor, Vec<u8>)> {
        let record = match ({
            let _state = phase_profile::scope(Phase::AuthState);
            self.object_meta(id)
        }) {
            Ok(record) => record,
            Err(_) => {
                if self.options().kv_denial_equalise {
                    self.kv_denial_preamble(principal, now_ns);
                }
                return Err(denied());
            }
        };
        let (descriptor, descriptor_id) = match ({
            let _descriptor = phase_profile::scope(Phase::AuthDescriptor);
            self.kv_descriptor_of(&record)
        }) {
            Ok(value) => value,
            Err(_) => {
                if self.options().kv_denial_equalise {
                    self.kv_denial_preamble(principal, now_ns);
                }
                return Err(denied());
            }
        };
        let capability_phase = phase_profile::scope(Phase::AuthCapability);
        let Ok(Some(capability)) = self.kv_capability_of(principal) else {
            return Err(denied());
        };
        if capability.check_valid_at(now_ns).is_err() {
            return Err(denied());
        }
        if capability.scope.principal != descriptor.tenant()
            || !capability.allows_namespace(&descriptor.namespace())
            || !capability.allows_root(descriptor_id)
        {
            return Err(denied());
        }
        drop(capability_phase);
        let Ok(authorized) = ({
            let _posix = phase_profile::scope(Phase::AuthPosix);
            self.authorize_read_with_capability(id, principal, now_ns, Some(capability.as_ref()))
        }) else {
            return Err(denied());
        };
        let Ok(bytes) = self.read_object_authorized(&authorized) else {
            return Err(denied());
        };
        Ok((descriptor, bytes))
    }

    /// The capability half of the KV preamble, on fixed inputs.
    ///
    /// Runs only on a path that has already failed, and only when
    /// `kv_denial_equalise` is set. It does the same grant decode and the same
    /// three grant tests the present path does, on a fixed namespace and root,
    /// so an absent object costs approximately what a present-but-unauthorized
    /// object costs. It can never succeed.
    fn kv_denial_preamble(&self, principal: &Principal, now_ns: i64) {
        let Ok(Some(capability)) = self.kv_capability_of(principal) else {
            return;
        };
        let _ = capability.check_valid_at(now_ns);
        let _ = capability.scope.principal.is_empty();
        let _ = capability.allows_namespace("kv.");
        let _ = capability.allows_root(Hash32::ZERO);
    }

    /// Read one mode-12 KV page under an authenticated capability.
    ///
    /// Fails closed **before any byte is returned** with [`KV_DENIED`] for: an
    /// unknown object, a missing or malformed descriptor, a descriptor id that
    /// does not match its bytes, an absent envelope, an expired envelope, a
    /// foreign tenant, a missing `kv.<tenant>` namespace grant, a missing
    /// descriptor id in the grant's roots, or a POSIX/ACL denial.
    pub fn read_kv_page(
        &self,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<DecodedKvPage> {
        let (stored, bytes) = self.read_kv_authorized(id, principal, now_ns)?;
        let StoredKvDescriptor::Page(descriptor) = stored else {
            return Err(Error::corrupt(
                "kv page is provenance-bound (mode 19); use read_kv_prov_page",
            ));
        };
        let limits = Limits::default();
        let frame = Frame::parse(&bytes, &limits).map_err(Error::from)?;
        let decoded = kv_cache::decode_kv_page(&frame, &limits).map_err(Error::from)?;
        let from_frame = KvDescriptor {
            tenant: descriptor.tenant.clone(),
            model: decoded.meta.model.clone(),
            session: decoded.meta.session.clone(),
            layer: decoded.meta.layer,
            page_index: decoded.meta.page_index,
            page_count: decoded.meta.page_count,
            seq_start: decoded.meta.seq_start,
            seq_end: decoded.meta.seq_end,
            dtype: decoded.meta.dtype.as_str().to_owned(),
            head_count: decoded.meta.head_count,
            head_dim: decoded.meta.head_dim,
            shape: decoded.meta.shape.clone(),
            page_bytes: decoded.meta.page_bytes,
        };
        if from_frame != descriptor {
            return Err(Error::corrupt(
                "kv page descriptor does not match its frame metadata",
            ));
        }
        Ok(decoded)
    }

    /// Read one provenance-bound (mode 19) KV page under an authenticated
    /// capability.
    ///
    /// The authorization preamble is identical to [`Store::read_kv_page`]. After
    /// authorization the mode-19 descriptor is decoded and compared field by
    /// field with the stored capability descriptor, including the codec
    /// `content_root`; any mismatch is `Corrupt`. A mode-12 object is never
    /// decoded by this path, and a mode-19 object is never decoded by the
    /// mode-12 path: a request whose scope does not name the object emits zero
    /// bytes.
    pub fn read_kv_prov_page(
        &self,
        id: Hash32,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<DecodedKvProvPage> {
        let (stored, bytes) = self.read_kv_authorized(id, principal, now_ns)?;
        let StoredKvDescriptor::Provenance(descriptor) = stored else {
            return Err(Error::corrupt(
                "kv page is a plain mode-12 page; use read_kv_page",
            ));
        };
        let limits = Limits::default();
        let frame = phase_profile::timed(Phase::FrameParse, || Frame::parse(&bytes, &limits))
            .map_err(Error::from)?;
        let decoded = phase_profile::timed(Phase::PageDecode, || {
            kv_prov::decode_kv_prov_page(&frame, &limits)
        })
        .map_err(Error::from)?;
        let from_frame = KvProvDescriptor {
            tenant: descriptor.tenant.clone(),
            model: decoded.descriptor.page.model.clone(),
            session: decoded.descriptor.page.session.clone(),
            layer: decoded.descriptor.page.layer,
            page_index: decoded.descriptor.page.page_index,
            page_count: decoded.descriptor.page.page_count,
            seq_start: decoded.descriptor.page.seq_start,
            seq_end: decoded.descriptor.page.seq_end,
            dtype: decoded.descriptor.page.dtype.as_str().to_owned(),
            head_count: decoded.descriptor.page.head_count,
            head_dim: decoded.descriptor.page.head_dim,
            shape: decoded.descriptor.page.shape.clone(),
            page_bytes: decoded.descriptor.page.page_bytes,
            provenance_root: decoded.root,
        };
        if from_frame != descriptor {
            return Err(Error::corrupt(
                "kv provenance descriptor does not match its frame metadata",
            ));
        }
        Ok(decoded)
    }
}

/// Extract and validate the descriptor carried by an object record.
fn descriptor_of(record: &ObjectRecord) -> Result<KvDescriptor> {
    let Some(Value::Str(hex_text)) = record.attrs.get(ATTR_KV_DESCRIPTOR) else {
        return Err(Error::corrupt("kv page has no descriptor"));
    };
    let bytes = hex::decode(hex_text).map_err(|_| Error::corrupt("kv descriptor is not hex"))?;
    let descriptor = KvDescriptor::from_canonical_bytes(&bytes)?;
    let Some(Value::Str(id_text)) = record.attrs.get(ATTR_KV_DESCRIPTOR_ID) else {
        return Err(Error::corrupt("kv page has no descriptor id"));
    };
    let recorded =
        Hash32::from_hex(id_text).map_err(|_| Error::corrupt("kv descriptor id is not a hash"))?;
    if recorded != descriptor.id()? {
        return Err(Error::corrupt("kv descriptor id does not match its bytes"));
    }
    Ok(descriptor)
}

/// Extract and validate a provenance-bound descriptor from an object record.
fn descriptor_of_prov(record: &ObjectRecord) -> Result<KvProvDescriptor> {
    let Some(Value::Str(hex_text)) = record.attrs.get(ATTR_KV_DESCRIPTOR) else {
        return Err(Error::corrupt("kv page has no descriptor"));
    };
    let bytes = hex::decode(hex_text).map_err(|_| Error::corrupt("kv descriptor is not hex"))?;
    let descriptor = KvProvDescriptor::from_canonical_bytes(&bytes)?;
    let Some(Value::Str(id_text)) = record.attrs.get(ATTR_KV_DESCRIPTOR_ID) else {
        return Err(Error::corrupt("kv page has no descriptor id"));
    };
    let recorded =
        Hash32::from_hex(id_text).map_err(|_| Error::corrupt("kv descriptor id is not a hash"))?;
    if recorded != descriptor.id()? {
        return Err(Error::corrupt("kv descriptor id does not match its bytes"));
    }
    // The recorded provenance root must be the one the descriptor names.
    if let Some(Value::Str(root_text)) = record.attrs.get(ATTR_KV_PROV_ROOT) {
        let recorded_root = Hash32::from_hex(root_text)
            .map_err(|_| Error::corrupt("kv provenance root is not a hash"))?;
        if recorded_root != descriptor.provenance_root {
            return Err(Error::corrupt(
                "kv provenance root attribute does not match its descriptor",
            ));
        }
    }
    Ok(descriptor)
}

/// Read whichever descriptor a record carries. An absent mode attribute means
/// mode 12, which keeps every pre-R5 page readable.
fn descriptor_of_any(record: &ObjectRecord) -> Result<StoredKvDescriptor> {
    match record.attrs.get(ATTR_KV_MODE) {
        None => Ok(StoredKvDescriptor::Page(descriptor_of(record)?)),
        Some(Value::U64(12)) => Ok(StoredKvDescriptor::Page(descriptor_of(record)?)),
        Some(Value::U64(19)) => Ok(StoredKvDescriptor::Provenance(descriptor_of_prov(record)?)),
        Some(_) => Err(Error::corrupt("kv page mode is not recognised")),
    }
}

/// Environment attribute names a KV page uses, for tests and documentation.
pub fn reserved_attribute_names() -> [&'static str; 8] {
    [
        ATTR_KV_DESCRIPTOR,
        ATTR_KV_DESCRIPTOR_ID,
        ATTR_KV_TENANT,
        ATTR_KV_MODEL,
        ATTR_KV_SESSION,
        ATTR_KV_PAGE_BYTES,
        ATTR_KV_MODE,
        ATTR_KV_PROV_ROOT,
    ]
}

/// Attribute: exact logical page length.
pub const ATTR_KV_PAGE_BYTES: &str = "weft.kv.page_bytes";
