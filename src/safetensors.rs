//! Exact safetensors container parsing and writing.
//!
//! A safetensors file is `u64le(header_len) || JSON header || data section`.
//! The data section is a contiguous packing of the tensors in the order their
//! `data_offsets` describe. This module keeps the *exact* header bytes so a
//! checkpoint can reproduce a whole original file byte for byte, and it writes
//! a well-formed header when a restore is partial (a subset of tensors or a
//! slice) and exact reproduction is impossible.
//!
//! Only the dtypes with an exact `WEFT-TENSOR.md` v1 mapping are accepted.
//! There is no approximation and no dtype coercion.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Bound on an accepted safetensors JSON header.
pub const MAX_HEADER_BYTES: u64 = 64 * 1024 * 1024;

/// Default window for [`SafetensorsFile::hash_tensor_digests`].
///
/// A streaming digest only needs a fixed buffer, so the window is independent
/// of the tensor and of the ingest bound.
pub const DEFAULT_INGEST_WINDOW_BYTES: usize = 8 * 1024 * 1024;

/// One tensor as described by the source header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetensorsTensor {
    /// Tensor name exactly as it appears in the header.
    pub name: String,
    /// Canonical `WEFT-TENSOR.md` dtype spelling (`bf16`, `f32`, ...).
    pub dtype: String,
    pub shape: Vec<u64>,
    /// `data_offsets[0]` (relative to the start of the data section).
    pub begin: u64,
    /// `data_offsets[1]`.
    pub end: u64,
}

impl SafetensorsTensor {
    /// Element bytes in the source container.
    pub fn byte_len(&self) -> u64 {
        self.end.saturating_sub(self.begin)
    }
}

/// A parsed safetensors container.
#[derive(Clone, Debug)]
pub struct SafetensorsFile {
    pub path: PathBuf,
    /// Total file length in bytes.
    pub file_bytes: u64,
    /// Exact `u64le(header_len) || JSON header` bytes.
    pub header_bytes: Vec<u8>,
    /// Offset of the first data byte (`8 + header_len`).
    pub data_start: u64,
    /// Tensors in ascending `data_offsets[0]` order.
    pub tensors: Vec<SafetensorsTensor>,
    /// SHA-256 of the complete file.
    pub sha256: [u8; 32],
    /// The `__metadata__` object, when present.
    pub metadata: Option<Json>,
}

/// Safetensors dtype name to canonical WeftTensor dtype name.
pub fn canonical_dtype(text: &str) -> Result<&'static str> {
    match text {
        "F16" => Ok("f16"),
        "BF16" => Ok("bf16"),
        "F32" => Ok("f32"),
        "F64" => Ok("f64"),
        "I8" => Ok("i8"),
        "U8" => Ok("u8"),
        "I16" => Ok("i16"),
        "I32" => Ok("i32"),
        "I64" => Ok("i64"),
        other => Err(Error::malformed(format!(
            "safetensors dtype `{other}` has no exact WEFT-TENSOR v1 mapping"
        ))),
    }
}

/// Canonical WeftTensor dtype name to safetensors dtype name.
pub fn container_dtype(canonical: &str) -> Result<&'static str> {
    match canonical {
        "f16" => Ok("F16"),
        "bf16" => Ok("BF16"),
        "f32" => Ok("F32"),
        "f64" => Ok("F64"),
        "i8" => Ok("I8"),
        "u8" => Ok("U8"),
        "i16" => Ok("I16"),
        "i32" => Ok("I32"),
        "i64" => Ok("I64"),
        other => Err(Error::unsupported_version(format!(
            "dtype `{other}` cannot be written to a safetensors container"
        ))),
    }
}

/// Element bytes for a safetensors dtype name.
fn element_bytes(dtype: &str) -> Result<u64> {
    match dtype {
        "F16" | "BF16" | "I16" => Ok(2),
        "F32" | "I32" => Ok(4),
        "F64" | "I64" => Ok(8),
        "I8" | "U8" => Ok(1),
        other => Err(Error::malformed(format!(
            "safetensors dtype `{other}` has no exact WEFT-TENSOR v1 mapping"
        ))),
    }
}

impl SafetensorsFile {
    /// Parse a container from disk, verifying the header and data layout.
    pub fn parse(path: &Path) -> Result<SafetensorsFile> {
        let file_bytes = std::fs::metadata(path)
            .map_err(|err| Error::io(format!("{}: {err}", path.display())))?
            .len();
        let mut file =
            File::open(path).map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
        let mut length_bytes = [0u8; 8];
        file.read_exact(&mut length_bytes)
            .map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
        let header_len = u64::from_le_bytes(length_bytes);
        if header_len > MAX_HEADER_BYTES || 8 + header_len > file_bytes {
            return Err(Error::malformed(format!(
                "{}: implausible safetensors header length {header_len} (file {file_bytes})",
                path.display()
            )));
        }
        let mut header_json = vec![0u8; header_len as usize];
        file.read_exact(&mut header_json)
            .map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
        let mut header_bytes = Vec::with_capacity(8 + header_len as usize);
        header_bytes.extend_from_slice(&length_bytes);
        header_bytes.extend_from_slice(&header_json);

        let parsed: Json = serde_json::from_slice(&header_json)
            .map_err(|err| Error::malformed(format!("{}: header JSON: {err}", path.display())))?;
        let map = parsed.as_object().ok_or_else(|| {
            Error::malformed(format!("{}: header is not an object", path.display()))
        })?;

        let data_start = 8 + header_len;
        let mut tensors = Vec::new();
        let mut metadata = None;
        for (name, entry) in map {
            if name == "__metadata__" {
                metadata = Some(entry.clone());
                continue;
            }
            let entry = entry.as_object().ok_or_else(|| {
                Error::malformed(format!(
                    "{}: tensor `{name}` entry is not an object",
                    path.display()
                ))
            })?;
            let dtype_text = entry.get("dtype").and_then(Json::as_str).ok_or_else(|| {
                Error::malformed(format!(
                    "{}: tensor `{name}` is missing dtype",
                    path.display()
                ))
            })?;
            let dtype = canonical_dtype(dtype_text)?.to_owned();
            let shape: Vec<u64> = entry
                .get("shape")
                .and_then(Json::as_array)
                .ok_or_else(|| {
                    Error::malformed(format!(
                        "{}: tensor `{name}` is missing shape",
                        path.display()
                    ))
                })?
                .iter()
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        Error::malformed(format!(
                            "{}: tensor `{name}` has a non-integer shape",
                            path.display()
                        ))
                    })
                })
                .collect::<Result<Vec<u64>>>()?;
            let offsets = entry
                .get("data_offsets")
                .and_then(Json::as_array)
                .ok_or_else(|| {
                    Error::malformed(format!(
                        "{}: tensor `{name}` is missing data_offsets",
                        path.display()
                    ))
                })?;
            if offsets.len() != 2 {
                return Err(Error::malformed(format!(
                    "{}: tensor `{name}` data_offsets must have two entries",
                    path.display()
                )));
            }
            let begin = offsets[0].as_u64().ok_or_else(|| {
                Error::malformed(format!(
                    "{}: tensor `{name}` data_offsets[0] is not an integer",
                    path.display()
                ))
            })?;
            let end = offsets[1].as_u64().ok_or_else(|| {
                Error::malformed(format!(
                    "{}: tensor `{name}` data_offsets[1] is not an integer",
                    path.display()
                ))
            })?;
            let elements: u64 = shape.iter().try_fold(1u64, |acc, dim| {
                acc.checked_mul(*dim).ok_or_else(|| {
                    Error::malformed(format!(
                        "{}: tensor `{name}` shape overflows the element count",
                        path.display()
                    ))
                })
            })?;
            let expected = elements
                .checked_mul(element_bytes(dtype_text)?)
                .ok_or_else(|| {
                    Error::malformed(format!(
                        "{}: tensor `{name}` shape overflows the byte length",
                        path.display()
                    ))
                })?;
            if end < begin || end - begin != expected {
                return Err(Error::malformed(format!(
                    "{}: tensor `{name}` data_offsets [{begin},{end}] do not match \
                     {elements} x {dtype}",
                    path.display()
                )));
            }
            if data_start + end > file_bytes {
                return Err(Error::malformed(format!(
                    "{}: tensor `{name}` extends past the file",
                    path.display()
                )));
            }
            tensors.push(SafetensorsTensor {
                name: name.clone(),
                dtype,
                shape,
                begin,
                end,
            });
        }
        tensors.sort_by_key(|tensor| tensor.begin);

        // The data section must be a contiguous packing; otherwise a whole-file
        // byte-exact restore is impossible and must be reported, not guessed.
        let mut cursor = 0u64;
        for tensor in &tensors {
            if tensor.begin != cursor {
                return Err(Error::malformed(format!(
                    "{}: data section is not contiguous at tensor `{}` \
                     (begin {}, expected {cursor})",
                    path.display(),
                    tensor.name,
                    tensor.begin
                )));
            }
            cursor = tensor.end;
        }
        if data_start + cursor != file_bytes {
            return Err(Error::malformed(format!(
                "{}: data section ends at {} but the file is {file_bytes} bytes",
                path.display(),
                data_start + cursor
            )));
        }

        let sha256 = sha256_file(path)?;
        Ok(SafetensorsFile {
            path: path.to_path_buf(),
            file_bytes,
            header_bytes,
            data_start,
            tensors,
            sha256,
            metadata,
        })
    }

    /// Read one tensor's exact bytes from the source file.
    pub fn read_tensor(&self, tensor: &SafetensorsTensor) -> Result<Vec<u8>> {
        let mut file = File::open(&self.path)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        file.seek(SeekFrom::Start(self.data_start + tensor.begin))
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        let len = usize::try_from(tensor.byte_len()).map_err(|_| {
            Error::limit_exceeded(format!("tensor `{}` does not fit in memory", tensor.name))
        })?;
        let mut bytes = vec![0u8; len];
        file.read_exact(&mut bytes)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        Ok(bytes)
    }

    /// Find a tensor by exact name.
    pub fn tensor(&self, name: &str) -> Result<&SafetensorsTensor> {
        self.tensors
            .iter()
            .find(|tensor| tensor.name == name)
            .ok_or_else(|| Error::not_found(format!("no tensor named `{name}` in the container")))
    }

    /// Read `out.len()` bytes at `byte_offset` inside one tensor.
    ///
    /// `byte_offset` is relative to the tensor's first byte. The range must fit
    /// inside the tensor's declared `data_offsets`; a short read is `Io` and
    /// never a silently zero-filled buffer.
    pub fn read_tensor_range(
        &self,
        tensor: &SafetensorsTensor,
        byte_offset: u64,
        out: &mut [u8],
    ) -> Result<()> {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::SourceRead);
        let end = byte_offset
            .checked_add(out.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("tensor range offset overflow"))?;
        if end > tensor.byte_len() {
            return Err(Error::invalid_argument(format!(
                "range {byte_offset}..{end} is outside tensor `{}` ({} bytes)",
                tensor.name,
                tensor.byte_len()
            )));
        }
        let mut file = File::open(&self.path)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        file.seek(SeekFrom::Start(
            self.data_start + tensor.begin + byte_offset,
        ))
        .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        if !out.is_empty() {
            file.read_exact(out)
                .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        }
        Ok(())
    }

    /// Extract one shard's canonical row-major bytes without materialising the
    /// whole tensor.
    ///
    /// The output is byte-for-byte the same as
    /// [`weftcodec::tensor::shard_bytes`] over the whole tensor: `axis == 0` is
    /// one contiguous read, any other axis is `product(shape[..axis])`
    /// contiguous runs of `plane * inner_bytes`, where
    /// `inner_bytes = product(shape[axis + 1..]) * element_size` and
    /// `plane = shape[axis] / shards`.
    ///
    /// Fails closed with `LimitExceeded` (naming `--max-ingest-bytes` and
    /// `--shards`) when the shard is larger than `max_bytes`, and with
    /// `Malformed` when the container's tensor byte length disagrees with the
    /// declared dtype and shape (the strides would otherwise read a neighbour
    /// tensor's bytes).
    pub fn read_tensor_shard(
        &self,
        tensor: &SafetensorsTensor,
        dtype: weftcodec::TensorDtype,
        axis: u32,
        shards: u32,
        index: u32,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::SourceRead);
        let limits = weftcodec::Limits::default();
        let count = weftcodec::tensor::validate_shape(&tensor.shape, &limits)?;
        let expected = weftcodec::tensor::payload_len(count, dtype)?;
        if expected != tensor.byte_len() {
            return Err(Error::malformed(format!(
                "tensor payload is {} bytes but shape {:?} x {} requires {expected}",
                tensor.byte_len(),
                tensor.shape,
                dtype.as_str()
            )));
        }
        let shard_shape = weftcodec::tensor::shard_shape(&tensor.shape, axis, shards, index)?;
        let shard_elements = shard_shape.iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("shard shape overflow"))
        })?;
        let shard_len = weftcodec::tensor::payload_len(shard_elements, dtype)?;
        if shard_len > max_bytes {
            return Err(Error::limit_exceeded(format!(
                "shard {index}/{shards} of `{}` is {shard_len} bytes, above the {max_bytes}-byte ingest bound (raise --max-ingest-bytes or --shards)",
                tensor.name
            )));
        }
        let size = usize::try_from(shard_len)
            .map_err(|_| Error::limit_exceeded("shard payload does not fit in memory"))?;
        let esize = dtype.element_size();
        let axis = axis as usize;
        let plane = tensor.shape[axis] / u64::from(shards);
        let inner_elements = tensor.shape[axis + 1..].iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("shard shape overflow"))
        })?;
        let inner_bytes = inner_elements
            .checked_mul(esize)
            .ok_or_else(|| Error::limit_exceeded("shard stride overflow"))?;
        let run_bytes = plane
            .checked_mul(inner_bytes)
            .ok_or_else(|| Error::limit_exceeded("shard stride overflow"))?;
        let outer = tensor.shape[..axis].iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("shard shape overflow"))
        })?;
        if run_bytes
            .checked_mul(outer)
            .ok_or_else(|| Error::limit_exceeded("shard size overflow"))?
            != shard_len
        {
            return Err(Error::corrupt(format!(
                "shard {index}/{shards} of `{}` does not tile its declared shape",
                tensor.name
            )));
        }
        let stride_bytes = tensor.shape[axis]
            .checked_mul(inner_bytes)
            .ok_or_else(|| Error::limit_exceeded("shard stride overflow"))?;
        let start_bytes = u64::from(index)
            .checked_mul(run_bytes)
            .ok_or_else(|| Error::limit_exceeded("shard offset overflow"))?;
        let mut out = vec![0u8; size];
        if size == 0 {
            return Ok(out);
        }
        let mut file = File::open(&self.path)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        let run = usize::try_from(run_bytes)
            .map_err(|_| Error::limit_exceeded("shard run does not fit in memory"))?;
        let mut position = 0usize;
        for outer_index in 0..outer {
            let source = outer_index
                .checked_mul(stride_bytes)
                .and_then(|offset| offset.checked_add(start_bytes))
                .ok_or_else(|| Error::limit_exceeded("shard offset overflow"))?;
            if source
                .checked_add(run_bytes)
                .ok_or_else(|| Error::limit_exceeded("shard offset overflow"))?
                > tensor.byte_len()
            {
                return Err(Error::corrupt(format!(
                    "shard {index}/{shards} of `{}` reads past the tensor",
                    tensor.name
                )));
            }
            file.seek(SeekFrom::Start(self.data_start + tensor.begin + source))
                .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
            file.read_exact(&mut out[position..position + run])
                .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
            position += run;
        }
        Ok(out)
    }

    /// Stream the whole-tensor `ContentId` and canonical `values_hash` digests.
    ///
    /// The bytes are fed to both hashers in `window`-sized pieces (a window of
    /// zero means [`DEFAULT_INGEST_WINDOW_BYTES`]), so no whole-tensor buffer is
    /// allocated. The digests are the exact ones the one-shot functions produce:
    /// `identity::content_hash(bytes)` and `weftcodec::tensor::values_hash(bytes)`.
    ///
    /// Refuses with `LimitExceeded` when the tensor is larger than `max_bytes`
    /// (naming `--max-ingest-bytes`), and fails closed on a short read.
    pub fn hash_tensor_digests(
        &self,
        tensor: &SafetensorsTensor,
        window: usize,
        max_bytes: u64,
    ) -> Result<(crate::identity::Hash32, crate::identity::Hash32)> {
        if tensor.byte_len() > max_bytes {
            return Err(Error::limit_exceeded(format!(
                "tensor `{}` is {} bytes, above the {max_bytes}-byte ingest bound (raise --max-ingest-bytes)",
                tensor.name,
                tensor.byte_len()
            )));
        }
        let window = if window == 0 {
            DEFAULT_INGEST_WINDOW_BYTES
        } else {
            window
        };
        let window = window.min(tensor.byte_len().max(1) as usize).max(1);
        let mut content = crate::identity::content_hash_hasher();
        let mut values = weftcodec::tensor::values_hasher();
        let mut file = File::open(&self.path)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        file.seek(SeekFrom::Start(self.data_start + tensor.begin))
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        let mut remaining = tensor.byte_len();
        let mut buffer = vec![0u8; window];
        // Source read and hashing are timed separately and recorded once per
        // tensor, so the profiler does not pay a lock per 4 MiB window.
        let mut read_ns: u64 = 0;
        let mut hash_ns: u64 = 0;
        while remaining > 0 {
            let want = usize::try_from(remaining.min(window as u64))
                .map_err(|_| Error::limit_exceeded("tensor window does not fit in memory"))?;
            let read_started = std::time::Instant::now();
            file.read_exact(&mut buffer[..want])
                .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
            read_ns = read_ns.saturating_add(read_started.elapsed().as_nanos() as u64);
            let hash_started = std::time::Instant::now();
            content.update(&buffer[..want]);
            values.update(&buffer[..want]);
            hash_ns = hash_ns.saturating_add(hash_started.elapsed().as_nanos() as u64);
            remaining -= want as u64;
        }
        if crate::phase_profile::enabled() {
            crate::phase_profile::record(crate::phase_profile::Phase::SourceRead, read_ns);
            crate::phase_profile::record(crate::phase_profile::Phase::Digest, hash_ns);
        }
        Ok((
            crate::identity::finish_content_hash(content),
            values.finalize(),
        ))
    }
}

/// A bounded, random-access reader over one tensor's bytes in a container.
///
/// Implements [`weftcodec::modes::ByteSource`], so an encoder reads windows of a
/// tensor without the store ever holding it. The file handle is the only state;
/// nothing is buffered beyond the caller's window.
pub struct TensorByteSource {
    file: File,
    base: u64,
    len: u64,
}

impl SafetensorsFile {
    /// Open a bounded source over `tensor`'s payload.
    pub fn open_tensor_source(&self, tensor: &SafetensorsTensor) -> Result<TensorByteSource> {
        let file = File::open(&self.path)
            .map_err(|err| Error::io(format!("{}: {err}", self.path.display())))?;
        Ok(TensorByteSource {
            file,
            base: self.data_start + tensor.begin,
            len: tensor.byte_len(),
        })
    }
}

impl TensorByteSource {
    /// Logical length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when the tensor has no payload bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read exactly `out.len()` bytes at logical `offset`.
    pub fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(out.len() as u64)
            .ok_or_else(|| Error::limit_exceeded("tensor source read range overflows u64"))?;
        if end > self.len {
            return Err(Error::malformed(format!(
                "tensor source is {} bytes but the read ends at {end}",
                self.len
            )));
        }
        self.file
            .seek(SeekFrom::Start(self.base + offset))
            .map_err(|err| Error::io(format!("tensor source seek: {err}")))?;
        self.file
            .read_exact(out)
            .map_err(|err| Error::io(format!("tensor source read: {err}")))
    }
}

impl weftcodec::modes::ByteSource for TensorByteSource {
    fn len(&self) -> u64 {
        TensorByteSource::len(self)
    }

    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> weftcodec::Result<()> {
        TensorByteSource::read_at(self, offset, out)
            .map_err(|err| weftcodec::Error::io(err.message().to_owned()))
    }
}

/// One tensor to write into a synthesized safetensors container.
pub struct SafetensorsEntry<'a> {
    pub name: &'a str,
    /// Canonical `WEFT-TENSOR.md` dtype name.
    pub dtype: &'a str,
    pub shape: &'a [u64],
    pub data: &'a [u8],
}

/// Build a complete safetensors file from entries in the given order.
///
/// The header is padded with JSON whitespace to an 8-byte boundary, matching
/// the reference writer. Each entry's data must be exactly its shape payload.
pub fn encode_file(entries: &[SafetensorsEntry<'_>], metadata: Option<&Json>) -> Result<Vec<u8>> {
    let mut map = Map::new();
    if let Some(metadata) = metadata {
        map.insert("__metadata__".to_owned(), metadata.clone());
    }
    let mut data = Vec::new();
    for entry in entries {
        let dtype = container_dtype(entry.dtype)?;
        let elements: u64 = entry.shape.iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("tensor shape overflows the element count"))
        })?;
        let expected = elements
            .checked_mul(element_bytes(dtype)?)
            .ok_or_else(|| Error::limit_exceeded("tensor shape overflows the byte length"))?;
        if expected != entry.data.len() as u64 {
            return Err(Error::malformed(format!(
                "tensor `{}` has {} bytes but shape {:?} x {dtype} requires {expected}",
                entry.name,
                entry.data.len(),
                entry.shape
            )));
        }
        let begin = data.len() as u64;
        let end = begin + expected;
        map.insert(
            entry.name.to_owned(),
            json!({
                "dtype": dtype,
                "shape": entry.shape,
                "data_offsets": [begin, end],
            }),
        );
        data.extend_from_slice(entry.data);
    }
    let mut header = serde_json::to_vec(&Json::Object(map))
        .map_err(|err| Error::malformed(format!("safetensors header JSON: {err}")))?;
    while header.len() % 8 != 0 {
        header.push(b' ');
    }
    let mut out = Vec::with_capacity(8 + header.len() + data.len());
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&data);
    Ok(out)
}

/// Build only the `u64le(header_len) || JSON header` prefix for entries whose
/// data lengths are known.
pub fn encode_header(
    entries: &[(&str, &str, &[u64], u64)],
    metadata: Option<&Json>,
) -> Result<Vec<u8>> {
    let mut map = Map::new();
    if let Some(metadata) = metadata {
        map.insert("__metadata__".to_owned(), metadata.clone());
    }
    let mut offset = 0u64;
    for (name, dtype, shape, len) in entries {
        let dtype = container_dtype(dtype)?;
        let begin = offset;
        let end = begin
            .checked_add(*len)
            .ok_or_else(|| Error::limit_exceeded("safetensors data offset overflows"))?;
        map.insert(
            (*name).to_owned(),
            json!({
                "dtype": dtype,
                "shape": shape,
                "data_offsets": [begin, end],
            }),
        );
        offset = end;
    }
    let mut header = serde_json::to_vec(&Json::Object(map))
        .map_err(|err| Error::malformed(format!("safetensors header JSON: {err}")))?;
    while header.len() % 8 != 0 {
        header.push(b' ');
    }
    let mut out = Vec::with_capacity(8 + header.len());
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(&header);
    Ok(out)
}

/// SHA-256 of a byte slice.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha256(bytes))
}

/// Streaming SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file =
        File::open(path).map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// SHA-256 of a complete file, streamed in bounded pieces.
pub fn sha256_path_hex(path: &Path) -> Result<String> {
    Ok(hex::encode(sha256_file(path)?))
}

/// Metadata references as a map for callers that need to preserve it.
pub fn metadata_pairs(metadata: Option<&Json>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(Json::Object(map)) = metadata {
        for (key, value) in map {
            if let Some(text) = value.as_str() {
                out.insert(key.clone(), text.to_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg_bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            out.push((state >> 33) as u8);
        }
        out
    }

    fn write_container(
        dtype: &'static str,
        shape: &[u64],
        data: &[u8],
    ) -> (tempfile::TempDir, SafetensorsFile) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.safetensors");
        let bytes = encode_file(
            &[SafetensorsEntry {
                name: "t",
                dtype,
                shape,
                data,
            }],
            None,
        )
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let file = SafetensorsFile::parse(&path).unwrap();
        (dir, file)
    }

    fn element_len(shape: &[u64], dtype: weftcodec::TensorDtype) -> usize {
        let count = shape.iter().fold(1u64, |acc, dim| acc * dim);
        usize::try_from(count * dtype.element_size()).unwrap()
    }

    #[test]
    fn streamed_shards_match_shard_bytes() {
        let limits = weftcodec::Limits::default();
        let cases: [(&'static str, weftcodec::TensorDtype, &[u64]); 4] = [
            ("bf16", weftcodec::TensorDtype::Bf16, &[8]),
            ("f32", weftcodec::TensorDtype::F32, &[4, 6]),
            ("i8", weftcodec::TensorDtype::I8, &[4, 6, 5]),
            ("f64", weftcodec::TensorDtype::F64, &[2, 3, 2, 2]),
        ];
        for (name, dtype, shape) in cases {
            let data = lcg_bytes(element_len(shape, dtype), 0x51ed);
            let (_dir, file) = write_container(name, shape, &data);
            let tensor = file.tensor("t").unwrap();
            for axis in 0..shape.len() as u32 {
                for shards in [1u32, 2, 3, 4] {
                    if shape[axis as usize] % u64::from(shards) != 0 {
                        continue;
                    }
                    for index in 0..shards {
                        let direct = weftcodec::tensor::shard_bytes(
                            dtype, shape, axis, shards, index, &data, &limits,
                        )
                        .unwrap();
                        let streamed = file
                            .read_tensor_shard(tensor, dtype, axis, shards, index, u64::MAX)
                            .unwrap();
                        assert_eq!(
                            streamed, direct,
                            "{name} {shape:?} axis {axis} shards {shards} index {index}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn streamed_digests_match_one_shot() {
        let shape: &[u64] = &[5, 7, 3];
        let dtype = weftcodec::TensorDtype::Bf16;
        let data = lcg_bytes(element_len(shape, dtype), 0xfeed);
        let (_dir, file) = write_container("bf16", shape, &data);
        let tensor = file.tensor("t").unwrap();
        let expected_content = crate::identity::content_hash(&data);
        let expected_values = weftcodec::tensor::values_hash(&data);
        for window in [1usize, 3, 64, 4096, DEFAULT_INGEST_WINDOW_BYTES] {
            let (content, values) = file.hash_tensor_digests(tensor, window, u64::MAX).unwrap();
            assert_eq!(content, expected_content, "window {window}");
            assert_eq!(values, expected_values, "window {window}");
        }
        // Window zero selects the default window.
        let (content, values) = file.hash_tensor_digests(tensor, 0, u64::MAX).unwrap();
        assert_eq!(content, expected_content);
        assert_eq!(values, expected_values);
    }

    #[test]
    fn range_reads_are_exact_and_bounded() {
        let shape: &[u64] = &[6, 5];
        let dtype = weftcodec::TensorDtype::F32;
        let data = lcg_bytes(element_len(shape, dtype), 0x1234);
        let (_dir, file) = write_container("f32", shape, &data);
        let tensor = file.tensor("t").unwrap();
        let mut out = vec![0u8; 17];
        file.read_tensor_range(tensor, 3, &mut out).unwrap();
        assert_eq!(out, data[3..20]);
        let mut empty = Vec::new();
        file.read_tensor_range(tensor, data.len() as u64, &mut empty)
            .unwrap();
        let err = file
            .read_tensor_range(tensor, data.len() as u64 - 1, &mut out)
            .unwrap_err();
        assert_eq!(err.class(), crate::error::Class::InvalidArgument);
    }

    #[test]
    fn ingest_bounds_fail_closed() {
        let shape: &[u64] = &[4, 4];
        let dtype = weftcodec::TensorDtype::Bf16;
        let data = lcg_bytes(element_len(shape, dtype), 0xabcd);
        let (_dir, file) = write_container("bf16", shape, &data);
        let tensor = file.tensor("t").unwrap();
        let whole = data.len() as u64;
        let err = file.hash_tensor_digests(tensor, 64, whole - 1).unwrap_err();
        assert_eq!(err.class(), crate::error::Class::LimitExceeded);
        let shard = file
            .read_tensor_shard(tensor, dtype, 0, 2, 0, whole / 4)
            .unwrap_err();
        assert_eq!(shard.class(), crate::error::Class::LimitExceeded);
        // The same shard fits at the exact bound.
        file.read_tensor_shard(tensor, dtype, 0, 2, 0, whole / 2)
            .unwrap();
    }

    #[test]
    fn sha256_of_streamed_digests_still_matches_the_file() {
        // Guards against a window loop that silently skips or repeats bytes:
        // the content digest over a huge window must equal the digest over a
        // 1-byte window for a multi-window tensor.
        let shape: &[u64] = &[9];
        let dtype = weftcodec::TensorDtype::I8;
        let data = lcg_bytes(element_len(shape, dtype), 0x7777);
        let (_dir, file) = write_container("i8", shape, &data);
        let tensor = file.tensor("t").unwrap();
        let (a, va) = file.hash_tensor_digests(tensor, 2, u64::MAX).unwrap();
        let (b, vb) = file.hash_tensor_digests(tensor, 4096, u64::MAX).unwrap();
        assert_eq!(a, b);
        assert_eq!(va, vb);
    }
}
