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
        let Ok(record) = self.object_meta(id) else {
            return Err(denied());
        };
        let Ok(descriptor) = descriptor_of_any(&record) else {
            return Err(denied());
        };
        let Ok(descriptor_id) = descriptor.descriptor_id() else {
            return Err(denied());
        };
        let Some(verified) = principal.envelope() else {
            return Err(denied());
        };
        let Ok(capability) = CapabilityContext::from_envelope(&verified.envelope) else {
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
        let Ok(authorized) = self.authorize_read(id, principal, now_ns) else {
            return Err(denied());
        };
        let Ok(bytes) = self.read_object_authorized(&authorized) else {
            return Err(denied());
        };
        Ok((descriptor, bytes))
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
        let frame = Frame::parse(&bytes, &limits).map_err(Error::from)?;
        let decoded = kv_prov::decode_kv_prov_page(&frame, &limits).map_err(Error::from)?;
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
