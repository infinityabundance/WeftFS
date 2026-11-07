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

    /// The descriptor of a stored page, if it has a well-formed one.
    ///
    /// This is *not* an authorization path: it exists so verification and
    /// administration can inspect identity. It never returns page bytes.
    pub fn kv_descriptor(&self, id: Hash32) -> Result<KvDescriptor> {
        let record = self.object_meta(id)?;
        descriptor_of(&record)
    }

    /// Read one KV page under an authenticated capability.
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
        // 1. Object record and descriptor. Any failure is the same denial.
        let Ok(record) = self.object_meta(id) else {
            return Err(denied());
        };
        let Ok(descriptor) = descriptor_of(&record) else {
            return Err(denied());
        };
        let Ok(descriptor_id) = descriptor.id() else {
            return Err(denied());
        };
        // 2. The grant. The envelope must be present, valid, for the same
        //    tenant, grant the tenant namespace, and name this descriptor.
        let Some(verified) = principal.envelope() else {
            return Err(denied());
        };
        let Ok(capability) = CapabilityContext::from_envelope(&verified.envelope) else {
            return Err(denied());
        };
        if capability.check_valid_at(now_ns).is_err() {
            return Err(denied());
        }
        if capability.scope.principal != descriptor.tenant
            || !capability.allows_namespace(&descriptor.namespace())
            || !capability.allows_root(descriptor_id)
        {
            return Err(denied());
        }
        // 3. POSIX/ACL authorization, folded into the same denial.
        let Ok(authorized) = self.authorize_read(id, principal, now_ns) else {
            return Err(denied());
        };
        let Ok(bytes) = self.read_object_authorized(&authorized) else {
            return Err(denied());
        };
        // 4. Authorized. Decode and re-derive the descriptor from the frame.
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

/// Environment attribute names a KV page uses, for tests and documentation.
pub fn reserved_attribute_names() -> [&'static str; 6] {
    [
        ATTR_KV_DESCRIPTOR,
        ATTR_KV_DESCRIPTOR_ID,
        ATTR_KV_TENANT,
        ATTR_KV_MODEL,
        ATTR_KV_SESSION,
        ATTR_KV_PAGE_BYTES,
    ]
}

/// Attribute: exact logical page length.
pub const ATTR_KV_PAGE_BYTES: &str = "weft.kv.page_bytes";
