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
