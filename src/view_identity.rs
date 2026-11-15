//! Exact versus approximate view identity.
//!
//! An **approximate view** is a derived, lossy representation of an exact
//! artifact. It carries:
//!
//! 1. its own identity (`ViewId = H("weft/view/v1", WCB(descriptor))`),
//! 2. an explicit error contract,
//! 3. a reference to the exact source root it was derived from, and
//! 4. an `approximate_root` over the values it reconstructs.
//!
//! The exact artifact's root is unchanged by view creation and remains the
//! authoritative identity. A view is never returned by an exact-resolution
//! entry point: [`Store::resolve_exact_object`] refuses a `weft.view.v1`
//! descriptor with a typed error naming both the view and the exact source, so
//! a caller that asked for exact bytes cannot silently receive quantized ones.
//!
//! The only method implemented here is `q8_0-block` (32-element blocks, `f32`
//! scale, `i8` codes, round-half-to-even). Its contract is an absolute
//! per-element bound: for every element, `|x - x_hat| <= scale_block / 2`. No
//! clamp can trigger, because `scale = amax/127` and `|x| <= amax`, so
//! `|round(x/scale)| <= 127`. The measured bound is reported and asserted.

use std::collections::BTreeMap;
use std::io::Write;

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{ObjectKind, State};
use crate::store::Store;
use crate::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use crate::wcb_util as wu;

/// Canonical view descriptor schema.
pub const VIEW_SCHEMA: &str = "weft.view.v1";
/// Domain separation for the view identity.
pub const VIEW_DOMAIN: &str = "weft/view/v1";
/// Domain separation for the reconstructed-value root.
pub const VIEW_VALUES_DOMAIN: &str = "weft/view/values/v1";
/// Store directory that holds views.
pub const VIEW_ROOT: &str = "views";

/// Block size of the only implemented quantized view method.
pub const Q8_0_BLOCK: u64 = 32;

/// A `weft.view.v1` descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDescriptor {
    pub name: String,
    /// Canonical method name (`q8_0-block`).
    pub method: String,
    pub block_size: u64,
    /// Exact source object id and logical content root.
    pub source_object: Hash32,
    pub source_content: Hash32,
    pub source_logical_root: Hash32,
    pub source_dtype: String,
    pub source_shape: Vec<u64>,
    /// `H("weft/view/values/v1", reconstructed-bytes)`.
    pub approximate_root: Hash32,
    pub codes: Hash32,
    pub codes_content: Hash32,
    pub codes_shape: Vec<u64>,
    pub scales: Hash32,
    pub scales_content: Hash32,
    pub scales_shape: Vec<u64>,
    /// Element count and reconstructed dtype.
    pub element_count: u64,
    pub decoded_dtype: String,
    /// Human-readable error contract, reproduced verbatim in the descriptor.
    pub error_bound: String,
}

impl ViewDescriptor {
    pub const KEYS: &'static [&'static str] = &[
        "approximate_root",
        "block_size",
        "codes",
        "codes_content",
        "codes_shape",
        "decoded_dtype",
        "element_count",
        "error_bound",
        "exact",
        "method",
        "name",
        "scales",
        "scales_content",
        "scales_shape",
        "schema",
        "source_content",
        "source_dtype",
        "source_logical_root",
        "source_object",
        "source_shape",
        "view_kind",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(VIEW_SCHEMA.to_owned())),
            ("view_kind", Value::Str("quantized_view".to_owned())),
            ("exact", Value::Bool(false)),
            ("name", Value::Str(self.name.clone())),
            ("method", Value::Str(self.method.clone())),
            ("block_size", Value::U64(self.block_size)),
            ("source_object", wu::hash_value(self.source_object)),
            ("source_content", wu::hash_value(self.source_content)),
            (
                "source_logical_root",
                wu::hash_value(self.source_logical_root),
            ),
            ("source_dtype", Value::Str(self.source_dtype.clone())),
            (
                "source_shape",
                Value::Array(self.source_shape.iter().map(|d| Value::U64(*d)).collect()),
            ),
            ("approximate_root", wu::hash_value(self.approximate_root)),
            ("codes", wu::hash_value(self.codes)),
            ("codes_content", wu::hash_value(self.codes_content)),
            (
                "codes_shape",
                Value::Array(self.codes_shape.iter().map(|d| Value::U64(*d)).collect()),
            ),
            ("scales", wu::hash_value(self.scales)),
            ("scales_content", wu::hash_value(self.scales_content)),
            (
                "scales_shape",
                Value::Array(self.scales_shape.iter().map(|d| Value::U64(*d)).collect()),
            ),
            ("element_count", Value::U64(self.element_count)),
            ("decoded_dtype", Value::Str(self.decoded_dtype.clone())),
            ("error_bound", Value::Str(self.error_bound.clone())),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != VIEW_SCHEMA {
            return Err(Error::malformed(format!(
                "view descriptor schema is `{schema}`, expected `{VIEW_SCHEMA}`"
            )));
        }
        if wu::get_text(map, "view_kind")? != "quantized_view" {
            return Err(Error::malformed("view_kind must be `quantized_view` in v1"));
        }
        if get_bool(map, "exact")? {
            return Err(Error::malformed(
                "a view descriptor must declare `exact: false`; an exact artifact is not a view",
            ));
        }
        let method = wu::get_text(map, "method")?;
        if method != "q8_0-block" {
            return Err(Error::unsupported_representation(format!(
                "view method `{method}` is not implemented (only `q8_0-block`)"
            )));
        }
        Ok(ViewDescriptor {
            name: wu::get_text(map, "name")?,
            method,
            block_size: wu::get_u64(map, "block_size")?,
            source_object: wu::get_hash(map, "source_object")?,
            source_content: wu::get_hash(map, "source_content")?,
            source_logical_root: wu::get_hash(map, "source_logical_root")?,
            source_dtype: wu::get_text(map, "source_dtype")?,
            source_shape: u64_array(map, "source_shape")?,
            approximate_root: wu::get_hash(map, "approximate_root")?,
            codes: wu::get_hash(map, "codes")?,
            codes_content: wu::get_hash(map, "codes_content")?,
            codes_shape: u64_array(map, "codes_shape")?,
            scales: wu::get_hash(map, "scales")?,
            scales_content: wu::get_hash(map, "scales_content")?,
            scales_shape: u64_array(map, "scales_shape")?,
            element_count: wu::get_u64(map, "element_count")?,
            decoded_dtype: wu::get_text(map, "decoded_dtype")?,
            error_bound: wu::get_text(map, "error_bound")?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `ViewId = H("weft/view/v1", WCB(descriptor))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(VIEW_DOMAIN, &self.encode()?))
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("view name must not be empty"));
        }
        if self.block_size == 0 {
            return Err(Error::malformed("view block_size must be at least 1"));
        }
        if self.element_count == 0 {
            return Err(Error::malformed("view has no elements"));
        }
        if self.approximate_root == self.source_content
            || self.approximate_root == self.source_logical_root
        {
            return Err(Error::corrupt(
                "a view's approximate root must differ from the exact source roots",
            ));
        }
        if self.source_content == Hash32::ZERO || self.source_logical_root == Hash32::ZERO {
            return Err(Error::malformed("view source roots must not be zero"));
        }
        Ok(())
    }
}

fn get_bool(map: &BTreeMap<String, Value>, key: &str) -> Result<bool> {
    match wu::field(map, key)? {
        Value::Bool(value) => Ok(*value),
        _ => Err(Error::malformed(format!("key `{key}` must be a bool"))),
    }
}

fn u64_array(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u64>> {
    match wu::field(map, key)? {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::U64(value) => out.push(*value),
                    _ => return Err(Error::malformed(format!("key `{key}` must hold u64s"))),
                }
            }
            Ok(out)
        }
        _ => Err(Error::malformed(format!("key `{key}` must be an array"))),
    }
}

/// Half the spacing of `dtype` at `value` — the exact maximum narrowing error
/// for round-to-nearest-even. Zero for `f32`. Used to state the error contract:
/// the reconstruction is computed in `f32` and then narrowed back to the source
/// dtype, so the per-element bound has two terms.
///
/// `bf16` has 7 explicit mantissa bits, so the spacing at a value with unbiased
/// exponent `E` is `2^(E-7)` and the half-spacing is `2^(E-8)`. `f16` has 10, so
/// the half-spacing is `2^(E-11)` (fixed at `2^-25` below the smallest normal).
/// Getting either constant wrong would understate the guarantee, which is why
/// `tests/view_identity.rs` checks the bound against a measured worst case.
pub fn half_ulp(dtype: &str, value: f32) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return 0.0;
    }
    let exponent = ((value.abs().to_bits() >> 23) & 0xff) as i32 - 127;
    match dtype {
        "bf16" => {
            if exponent < -126 {
                // Subnormal bf16: the spacing is fixed at 2^-133.
                2f64.powi(-134)
            } else {
                2f64.powi(exponent - 8)
            }
        }
        // f16 has 10 explicit mantissa bits; below the smallest normal the
        // spacing is fixed, so the bound is the subnormal half-spacing.
        "f16" => {
            if exponent < -14 {
                2f64.powi(-25)
            } else if exponent > 15 {
                f64::INFINITY
            } else {
                2f64.powi(exponent - 11)
            }
        }
        "f32" => 0.0,
        _ => f64::INFINITY,
    }
}

/// Encoded q8_0 view of one tensor.
pub struct Q8View {
    pub codes: Vec<u8>,
    pub scales: Vec<u8>,
    pub reconstructed: Vec<u8>,
    pub max_abs_error: f64,
    pub max_bound: f64,
    pub clipped: u64,
    /// Reconstructed values that are not finite although the source was.
    pub overflowed: u64,
}

/// Encode `bytes` (canonical element bytes of `dtype`) as a q8_0-block view and
/// reconstruct it. `block_size` must divide the element count.
pub fn encode_q8_0(dtype: &str, bytes: &[u8], block_size: u64) -> Result<Q8View> {
    if block_size == 0 {
        return Err(Error::invalid_argument("block size must be at least 1"));
    }
    let element_bytes =
        crate::representation::TensorElementDtype::parse(dtype)?.element_bytes() as usize;
    if bytes.len() % element_bytes != 0 {
        return Err(Error::malformed(
            "tensor bytes are not a whole number of elements",
        ));
    }
    let elements = bytes.len() / element_bytes;
    if elements as u64 % block_size != 0 {
        return Err(Error::invalid_argument(format!(
            "block size {block_size} does not divide {elements} elements (no silent padding)"
        )));
    }
    let blocks = elements as u64 / block_size;
    let mut codes = Vec::with_capacity(elements);
    let mut scales = Vec::with_capacity(blocks as usize * 4);
    let mut reconstructed = Vec::with_capacity(bytes.len());
    let mut max_abs_error = 0f64;
    let mut max_bound = 0f64;
    let mut clipped = 0u64;
    let mut overflowed = 0u64;
    let mut values = vec![0f32; block_size as usize];
    for block in 0..blocks as usize {
        let base = block * block_size as usize;
        let mut amax = 0f32;
        for index in 0..block_size as usize {
            let value = crate::adapter::element_as_f32(dtype, bytes, base + index)?;
            values[index] = value;
            amax = amax.max(value.abs());
        }
        let scale = if amax == 0.0 { 0f32 } else { amax / 127.0 };
        scales.extend_from_slice(&scale.to_le_bytes());
        for index in 0..block_size as usize {
            let value = values[index];
            let quantized = if scale == 0.0 {
                0i8
            } else {
                let q = (value / scale).round_ties_even();
                if q > 127.0 {
                    clipped += 1;
                    127i8
                } else if q < -127.0 {
                    clipped += 1;
                    -127i8
                } else {
                    q as i8
                }
            };
            codes.push(quantized as u8);
            let back = quantized as f32 * scale;
            let narrowed_bytes = crate::adapter::f32_to_element(dtype, back)?;
            let narrowed = crate::adapter::element_as_f32(dtype, &narrowed_bytes, 0)?;
            if value.is_finite() && !narrowed.is_finite() {
                overflowed += 1;
            }
            reconstructed.extend_from_slice(&narrowed_bytes);
            let error = ((value as f64) - (narrowed as f64)).abs();
            if error > max_abs_error {
                max_abs_error = error;
            }
            let bound = (scale as f64).abs() / 2.0 + half_ulp(dtype, back);
            if bound > max_bound {
                max_bound = bound;
            }
        }
    }
    Ok(Q8View {
        codes,
        scales,
        reconstructed,
        max_abs_error,
        max_bound,
        clipped,
        overflowed,
    })
}

/// Reconstruct element bytes from stored codes and scales.
pub fn decode_q8_0(dtype: &str, codes: &[u8], scales: &[u8], block_size: u64) -> Result<Vec<u8>> {
    if block_size == 0 {
        return Err(Error::invalid_argument("block size must be at least 1"));
    }
    let blocks = scales.len() / 4;
    if scales.len() % 4 != 0 {
        return Err(Error::malformed("scales must be a whole number of f32"));
    }
    if codes.len() != blocks * block_size as usize {
        return Err(Error::malformed(format!(
            "codes are {} bytes but {blocks} blocks x {block_size} elements require {}",
            codes.len(),
            blocks * block_size as usize
        )));
    }
    let mut out = Vec::with_capacity(codes.len() * 2);
    for block in 0..blocks {
        let mut word = [0u8; 4];
        word.copy_from_slice(&scales[block * 4..block * 4 + 4]);
        let scale = f32::from_le_bytes(word);
        for index in 0..block_size as usize {
            let code = codes[block * block_size as usize + index] as i8;
            let value = code as f32 * scale;
            out.extend_from_slice(&crate::adapter::f32_to_element(dtype, value)?);
        }
    }
    Ok(out)
}

/// Measured error of one reconstructed view.
#[derive(Clone, Debug)]
pub struct ViewErrorReport {
    pub max_abs_error: f64,
    pub max_bound: f64,
    pub clipped: u64,
    pub overflowed: u64,
    pub elements: u64,
    pub holds: bool,
}

/// Result of [`Store::write_quantized_view`].
#[derive(Clone, Debug)]
pub struct ViewWriteReport {
    pub name: String,
    pub view_id: Hash32,
    pub descriptor_object: Hash32,
    pub descriptor_content: Hash32,
    pub codes_bytes: u64,
    pub scales_bytes: u64,
    pub exact_bytes: u64,
    pub approximate_root: Hash32,
    pub error: ViewErrorReport,
}

/// Options for a view write.
#[derive(Clone, Debug, Default)]
pub struct ViewWriteOptions {
    pub strategy: Option<TensorStrategy>,
    pub deterministic_seed: Option<[u8; 32]>,
}

/// A view resolved through the approximate door.
#[derive(Clone, Debug)]
pub struct ResolvedView {
    pub view_id: Hash32,
    pub descriptor: ViewDescriptor,
    pub bytes: Vec<u8>,
    pub error: ViewErrorReport,
}

impl ViewDescriptor {
    /// The exact artifact a caller must ask for instead of the view.
    pub fn exact_reference(&self) -> String {
        format!(
            "object:{} content:{} logical:{}",
            self.source_object.to_hex(),
            self.source_content.to_hex(),
            self.source_logical_root.to_hex()
        )
    }
}

fn seed_from(label: &str) -> [u8; 32] {
    *identity::h("weft/view/seed/v1", label.as_bytes()).as_bytes()
}

fn ensure_dir(
    store: &mut Store,
    parent: Hash32,
    name: &str,
    seed: Option<&[u8; 32]>,
) -> Result<Hash32> {
    if let Some(id) = entry_child(store, parent, name) {
        if store.object_meta(id)?.kind != ObjectKind::Dir {
            return Err(Error::not_dir(format!(
                "`{name}` exists and is not a directory"
            )));
        }
        return Ok(id);
    }
    let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
    match seed {
        Some(seed) => store
            .mkdir_seeded(parent, name.as_bytes(), 0o755, uid, gid, seed)
            .map_err(|err| {
                if err.class() == Class::Exists {
                    Error::exists(format!("directory `{name}` appeared concurrently"))
                } else {
                    err
                }
            }),
        None => store.mkdir(parent, name.as_bytes(), 0o755, uid, gid),
    }
}

fn entry_child(store: &Store, parent: Hash32, name: &str) -> Option<Hash32> {
    store
        .current_state()
        .entries
        .values()
        .find(|entry| entry.parent == parent && entry.name == name.as_bytes())
        .map(|entry| entry.target)
}

impl Store {
    /// Write one q8_0-block approximate view of an exact tensor object.
    ///
    /// The exact object is read (and its own roots verified by the read path)
    /// but never modified. Codes and scales are stored as ordinary exact tensors;
    /// the descriptor is a new file whose content address is the view manifest.
    pub fn write_quantized_view(
        &mut self,
        name: &str,
        source_object: Hash32,
        block_size: u64,
        options: &ViewWriteOptions,
    ) -> Result<ViewWriteReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        let meta = self.object_meta(source_object)?;
        if meta.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "a view source must be a tensor file object",
            ));
        }
        let stored = self.tensor_metadata(source_object).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::unsupported_representation(format!(
                    "a view source must be a tensor object (object {source_object} is not a tensor: {err})"
                ))
            } else {
                err
            }
        })?;
        let exact_bytes = stored
            .shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .and_then(|elements| elements.checked_mul(stored.dtype.element_bytes()))
            .ok_or_else(|| Error::limit_exceeded("view source tensor shape overflows"))?;
        let dtype = stored.dtype.as_str().to_owned();
        let shape = stored.shape.clone();
        let bytes = self.read_tensor(source_object)?;
        let logical_root = self.tensor_logical_root(source_object)?;

        let q8 = encode_q8_0(&dtype, &bytes, block_size)?;
        let elements = bytes.len() as u64
            / crate::representation::TensorElementDtype::parse(&dtype)?.element_bytes();
        let blocks = elements / block_size;

        let views_root = ensure_dir(self, State::ROOT_ID, VIEW_ROOT, None)?;
        let seed = options
            .deterministic_seed
            .unwrap_or_else(|| seed_from(name));
        let dir = ensure_dir(self, views_root, name, Some(&seed))?;
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let spec = TensorWriteSpec {
            strategy_hint: options.strategy,
            ..TensorWriteSpec::default()
        };
        let codes_object = self.write_tensor_file_with_spec_seeded(
            dir,
            b"codes",
            "i8",
            &[elements],
            &q8.codes,
            &TensorLayoutSpec::row_major(),
            &spec,
            &seed_from(&format!("{name}/codes")),
        )?;
        let scales_object = self.write_tensor_file_with_spec_seeded(
            dir,
            b"scales",
            "f32",
            &[blocks],
            &q8.scales,
            &TensorLayoutSpec::row_major(),
            &spec,
            &seed_from(&format!("{name}/scales")),
        )?;
        let codes_meta = self.object_meta(codes_object)?;
        let scales_meta = self.object_meta(scales_object)?;
        let approximate_root = identity::h(VIEW_VALUES_DOMAIN, &q8.reconstructed);
        let descriptor = ViewDescriptor {
            name: name.to_owned(),
            method: "q8_0-block".to_owned(),
            block_size,
            source_object,
            source_content: meta.content,
            source_logical_root: logical_root,
            source_dtype: dtype.clone(),
            source_shape: shape,
            approximate_root,
            codes: codes_object,
            codes_content: codes_meta.content,
            codes_shape: vec![elements],
            scales: scales_object,
            scales_content: scales_meta.content,
            scales_shape: vec![blocks],
            element_count: elements,
            decoded_dtype: dtype.clone(),
            error_bound: format!(
                "absolute per element: |x - x_hat| <= scale_block/2 + half_ulp({dtype}, q*scale_block), where scale_block = max|x| in the {block_size}-element block / 127 and q is the i8 code; round-half-to-even; the first term is the quantization error and the second is the final narrowing of the reconstructed value back to {dtype}; no clamp is reachable because |x| <= 127*scale_block"
            ),
        };
        descriptor.validate()?;
        let bytes = descriptor.encode()?;
        let descriptor_object = self.create_file_seeded(
            dir,
            b"descriptor",
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("{name}/descriptor")),
        )?;
        let descriptor_content = self.object_meta(descriptor_object)?.content;
        Ok(ViewWriteReport {
            name: name.to_owned(),
            view_id: descriptor.id()?,
            descriptor_object,
            descriptor_content,
            codes_bytes: codes_meta.size,
            scales_bytes: scales_meta.size,
            exact_bytes,
            approximate_root,
            error: ViewErrorReport {
                max_abs_error: q8.max_abs_error,
                max_bound: q8.max_bound,
                clipped: q8.clipped,
                overflowed: q8.overflowed,
                elements,
                holds: q8.clipped == 0 && q8.overflowed == 0 && q8.max_abs_error <= q8.max_bound,
            },
        })
    }

    /// Read one view descriptor by name.
    pub fn load_view(&self, name: &str) -> Result<ViewDescriptor> {
        let object = self.resolve_path(&format!("/{VIEW_ROOT}/{name}/descriptor"))?;
        let bytes = self.read_object(object)?;
        let descriptor = ViewDescriptor::decode(&bytes)?;
        descriptor.validate()?;
        if descriptor.name != name {
            return Err(Error::corrupt(format!(
                "view descriptor in `{name}` declares name `{}`",
                descriptor.name
            )));
        }
        Ok(descriptor)
    }

    /// View names, sorted.
    pub fn list_views(&self) -> Result<Vec<String>> {
        let root = match self.lookup(State::ROOT_ID, VIEW_ROOT.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut names = Vec::new();
        for entry in self.list_dir(root)? {
            if self.object_meta(entry.target)?.kind == ObjectKind::Dir {
                if let Ok(name) = String::from_utf8(entry.name.clone()) {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Reconstruct a view's approximate bytes and re-measure its error contract.
    ///
    /// The exact source is read only to measure the error; it is not returned.
    pub fn resolve_view(&self, name: &str) -> Result<ResolvedView> {
        let descriptor = self.load_view(name)?;
        if self.object_meta(descriptor.codes)?.content != descriptor.codes_content {
            return Err(Error::corrupt(format!(
                "view `{name}` codes object failed its content check"
            )));
        }
        if self.object_meta(descriptor.scales)?.content != descriptor.scales_content {
            return Err(Error::corrupt(format!(
                "view `{name}` scales object failed its content check"
            )));
        }
        let codes = self.read_tensor(descriptor.codes)?;
        let scales = self.read_tensor(descriptor.scales)?;
        if codes.len() as u64 != descriptor.codes_shape[0] {
            return Err(Error::corrupt(format!(
                "view `{name}` codes are {} bytes, expected {}",
                codes.len(),
                descriptor.codes_shape[0]
            )));
        }
        let bytes = decode_q8_0(
            &descriptor.decoded_dtype,
            &codes,
            &scales,
            descriptor.block_size,
        )?;
        if identity::h(VIEW_VALUES_DOMAIN, &bytes) != descriptor.approximate_root {
            return Err(Error::corrupt(format!(
                "view `{name}` reconstruction does not match its recorded approximate_root"
            )));
        }
        let error = measure_error(self, &descriptor, &bytes)?;
        if !error.holds {
            return Err(Error::corrupt(format!(
                "view `{name}` violates its error contract: max |error| {} > bound {}",
                error.max_abs_error, error.max_bound
            )));
        }
        Ok(ResolvedView {
            view_id: descriptor.id()?,
            descriptor,
            bytes,
            error,
        })
    }

    /// Verify a view's identity, stored member roots and error contract without
    /// returning its bytes.
    pub fn verify_view(&self, name: &str) -> Result<ViewVerification> {
        let descriptor = self.load_view(name)?;
        let source_meta = self.object_meta(descriptor.source_object)?;
        if source_meta.content != descriptor.source_content {
            return Err(Error::dependency_mismatch(format!(
                "view `{name}` names exact source content {} but the object now has {}",
                descriptor.source_content, source_meta.content
            )));
        }
        let logical = self.tensor_logical_root(descriptor.source_object)?;
        if logical != descriptor.source_logical_root {
            return Err(Error::dependency_mismatch(format!(
                "view `{name}` names exact logical root {} but the source now has {}",
                descriptor.source_logical_root, logical
            )));
        }
        if self.object_meta(descriptor.codes)?.content != descriptor.codes_content {
            return Err(Error::corrupt(format!(
                "view `{name}` codes object failed its content check"
            )));
        }
        if self.object_meta(descriptor.scales)?.content != descriptor.scales_content {
            return Err(Error::corrupt(format!(
                "view `{name}` scales object failed its content check"
            )));
        }
        let resolved = self.resolve_view(name)?;
        Ok(ViewVerification {
            view_id: resolved.view_id,
            exact_content: descriptor.source_content,
            exact_logical_root: descriptor.source_logical_root,
            approximate_root: descriptor.approximate_root,
            exact_object_intact: true,
            error: resolved.error,
        })
    }

    /// Resolve bytes through the **exact** door.
    ///
    /// Returns the object's bytes when the object is an exact artifact. A
    /// `weft.view.v1` descriptor is refused with `UnsupportedRepresentation`,
    /// naming the view and the exact source a caller must ask for instead. This
    /// is the guardrail: a request for exact bytes can never receive a
    /// quantized reconstruction.
    pub fn resolve_exact_object(&self, object: Hash32) -> Result<Vec<u8>> {
        let meta = self.object_meta(object)?;
        if meta.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "resolve_exact expects a regular file object",
            ));
        }
        let bytes = self.read_object(object)?;
        if let Ok(descriptor) = ViewDescriptor::decode(&bytes) {
            return Err(Error::unsupported_representation(format!(
                "refusing exact resolution: object {object} is approximate view `{}` (ViewId {}), not the exact artifact; ask for exact source {}",
                descriptor.name,
                descriptor.id()?.to_hex(),
                descriptor.exact_reference()
            )));
        }
        Ok(bytes)
    }
}

fn measure_error(
    store: &Store,
    descriptor: &ViewDescriptor,
    reconstructed: &[u8],
) -> Result<ViewErrorReport> {
    let exact = store.read_tensor(descriptor.source_object)?;
    if exact.len() != reconstructed.len() {
        return Err(Error::corrupt(
            "view reconstruction length differs from the exact source",
        ));
    }
    let scales = store.read_tensor(descriptor.scales)?;
    let codes = store.read_tensor(descriptor.codes)?;
    let mut max_abs_error = 0f64;
    let mut max_bound = 0f64;
    let mut overflowed = 0u64;
    let elements = descriptor.element_count as usize;
    for index in 0..elements {
        let exact_value = crate::adapter::element_as_f32(&descriptor.source_dtype, &exact, index)?;
        let view_value =
            crate::adapter::element_as_f32(&descriptor.decoded_dtype, reconstructed, index)?;
        let error = ((exact_value as f64) - (view_value as f64)).abs();
        if error > max_abs_error {
            max_abs_error = error;
        }
        if exact_value.is_finite() && !view_value.is_finite() {
            overflowed += 1;
        }
        let block = index / descriptor.block_size as usize;
        let mut word = [0u8; 4];
        word.copy_from_slice(&scales[block * 4..block * 4 + 4]);
        let scale = f32::from_le_bytes(word);
        let code = codes[index] as i8;
        let t = code as f32 * scale;
        let bound = (scale as f64).abs() / 2.0 + half_ulp(&descriptor.decoded_dtype, t);
        if bound > max_bound {
            max_bound = bound;
        }
    }
    Ok(ViewErrorReport {
        max_abs_error,
        max_bound,
        clipped: 0,
        overflowed,
        elements: descriptor.element_count,
        holds: overflowed == 0 && max_abs_error <= max_bound,
    })
}

/// Result of [`Store::verify_view`].
#[derive(Clone, Debug)]
pub struct ViewVerification {
    pub view_id: Hash32,
    pub exact_content: Hash32,
    pub exact_logical_root: Hash32,
    pub approximate_root: Hash32,
    pub exact_object_intact: bool,
    pub error: ViewErrorReport,
}

/// Write a descriptor's exact source bytes to a writer (used by courts that
/// need the authoritative artifact after proving a view cannot supply it).
pub fn write_exact_bytes<W: Write>(
    store: &Store,
    descriptor: &ViewDescriptor,
    mut writer: W,
) -> Result<u64> {
    let bytes = store.read_tensor(descriptor.source_object)?;
    writer
        .write_all(&bytes)
        .map_err(|err| Error::io(format!("exact write: {err}")))?;
    Ok(bytes.len() as u64)
}
