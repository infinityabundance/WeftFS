//! T091 — bounded multi-parent reconstruction court.
//!
//! Hypothesis: an exact tensor region may be cheaper to reconstruct from
//! **several** causal sources than from one. This harness answers it with a
//! measured, exact container, not with an estimate.
//!
//! # The multi-parent container
//!
//! ```text
//! "WMP1" | u32 version = 1 | u16 dtype_len | dtype | u8 rank | varint dim...
//!        | u8 parent_count | 32-byte content root per parent
//!        | u64 target_len | u64 body_len | body | 32-byte BLAKE3 target root
//! body   = repeated op:
//!            tag 0x00 COPY  : varint parent_index, varint offset, varint len
//!            tag 0x01 INSERT: varint len, len bytes
//!          terminated by 0xFF
//! ```
//!
//! The matcher is the **shipped mode-3 matcher** (`modes::encode_delta_ops`):
//! 4-byte keys, a bounded index (1,000,000 sampled positions, four candidates
//! per key), greedy longest match of at least 8 bytes. The only difference is
//! that a COPY names one of several bases. Therefore the fan-out-1 multi-parent
//! control uses the same op stream as mode 3 and differs only by its header —
//! which is what makes the comparison calibrated rather than an encoding win.
//!
//! Every decode verifies the declared parent roots, the exact target length and
//! the trailing target hash. A missing parent, or a parent whose bytes do not
//! hash to the declared root, is a typed refusal that returns no bytes.
//!
//! # Baselines (all measured on the same bytes)
//!
//! * mode 3 `DELTA` against the best single parent (full frame bytes),
//! * mode 11 `WEIGHT_DELTA` against the best single parent's mode-9 frame,
//! * mode 16 `SPARSE_DELTA` likewise,
//! * mode 9 `FLOAT_TENSOR` on the target itself (a non-delta reference).
//!
//! # Corpus
//!
//! Real pythia-70m checkpoints (steps 0, 1, 2, 4, 8, 16, 64, 512, 1000). Each
//! target is reconstructed from a bounded parent list (nearest earlier
//! checkpoint first, then step 0, then the next nearest). All subsets of each
//! fan-out are tried, so the reported multi-parent number is the best a planner
//! could do at that fan-out, not a lucky ordering.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftcodec::dependency::MapResolver;
use weftcodec::limits::Limits;
use weftcodec::modes::{self, FloatDtype};
use weftcodec::sparse_delta::{decode_with_base_frame as decode_sparse, encode_sparse_delta};
use weftcodec::weight_delta::{
    decode_with_base_frame as decode_weight, encode_weight_delta, WeightDtype,
};
use weftfs::safetensors::SafetensorsFile;

const SCHEMA: &str = "weftfs.multi-parent/1";
const CONTAINER_MAGIC: &[u8; 4] = b"WMP1";
const MIN_MATCH: usize = 8;
const MAX_CHAIN: usize = 4;
const MAX_INDEXED_POSITIONS: usize = 1_000_000;
const HASH_DOMAIN: &str = "weft/multiparent/v1";

// ---------------------------------------------------------------------------
// small utilities
// ---------------------------------------------------------------------------

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn arg_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|value| value == name)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return f64::NAN;
    }
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn blake3_domain(data: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(HASH_DOMAIN.len() as u32).to_le_bytes());
    hasher.update(HASH_DOMAIN.as_bytes());
    hasher.update(data);
    *hasher.finalize().as_bytes()
}

/// The store's canonical dtype spelling (`f32`) to the codec's (`fp32`).
fn codec_dtype(canonical: &str) -> Result<&'static str, String> {
    match canonical {
        "f16" => Ok("fp16"),
        "bf16" => Ok("bf16"),
        "f32" => Ok("fp32"),
        other => Err(format!("dtype `{other}` has no exact float codec mapping")),
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn put_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn get_varint(input: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let mut value: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *input
            .get(*cursor)
            .ok_or_else(|| "multiparent: truncated varint".to_owned())?;
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 63 {
            return Err("multiparent: varint overflow".to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// matcher: the shipped mode-3 anchor index, fanned out over several bases
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Index {
    table: HashMap<u32, Vec<u32>>,
}

impl Index {
    fn build(base: &[u8]) -> Index {
        let mut table: HashMap<u32, Vec<u32>> = HashMap::new();
        if base.len() < 4 {
            return Index { table };
        }
        let stride = if base.len() > MAX_INDEXED_POSITIONS {
            (base.len() / MAX_INDEXED_POSITIONS).max(1)
        } else {
            1
        };
        let mut pos = 0usize;
        while base.len() - pos >= 4 {
            if pos % stride == 0 {
                let key =
                    u32::from_le_bytes([base[pos], base[pos + 1], base[pos + 2], base[pos + 3]]);
                let chain = table.entry(key).or_default();
                if chain.len() < MAX_CHAIN {
                    chain.push(pos as u32);
                }
            }
            pos += 1;
        }
        Index { table }
    }
}

/// One COPY or INSERT in the multi-parent op stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Copy { parent: u8, offset: u64, len: u64 },
    Insert { offset: u64, len: u64 },
}

struct MultiEncoding {
    ops: Vec<Op>,
    copies: u64,
    copy_bytes: u64,
    insert_bytes: u64,
    per_parent_copies: Vec<u64>,
    per_parent_bytes: Vec<u64>,
    elapsed_ms: f64,
}

fn encode_multi(target: &[u8], parents: &[&[u8]], indexes: &[&Index]) -> MultiEncoding {
    let started = Instant::now();
    let mut ops = Vec::new();
    let mut copies = 0u64;
    let mut copy_bytes = 0u64;
    let mut insert_bytes = 0u64;
    let mut per_parent_copies = vec![0u64; parents.len()];
    let mut per_parent_bytes = vec![0u64; parents.len()];
    let mut literal_start = 0usize;
    let mut i = 0usize;
    while i < target.len() {
        let mut best_len = 0usize;
        let mut best_parent = 0usize;
        let mut best_pos = 0usize;
        if target.len() - i >= 4 {
            let key = u32::from_le_bytes([target[i], target[i + 1], target[i + 2], target[i + 3]]);
            for (parent_index, index) in indexes.iter().enumerate() {
                if let Some(chain) = index.table.get(&key) {
                    for position in chain {
                        let base_pos = *position as usize;
                        let base = parents[parent_index];
                        let max_len = (base.len() - base_pos).min(target.len() - i);
                        let mut len = 0usize;
                        while len < max_len && base[base_pos + len] == target[i + len] {
                            len += 1;
                        }
                        if len > best_len {
                            best_len = len;
                            best_parent = parent_index;
                            best_pos = base_pos;
                        }
                    }
                }
            }
        }
        if best_len >= MIN_MATCH {
            if literal_start < i {
                ops.push(Op::Insert {
                    offset: literal_start as u64,
                    len: (i - literal_start) as u64,
                });
                insert_bytes += (i - literal_start) as u64;
            }
            ops.push(Op::Copy {
                parent: best_parent as u8,
                offset: best_pos as u64,
                len: best_len as u64,
            });
            copies += 1;
            copy_bytes += best_len as u64;
            per_parent_copies[best_parent] += 1;
            per_parent_bytes[best_parent] += best_len as u64;
            i += best_len;
            literal_start = i;
        } else {
            i += 1;
        }
    }
    if literal_start < target.len() {
        ops.push(Op::Insert {
            offset: literal_start as u64,
            len: (target.len() - literal_start) as u64,
        });
        insert_bytes += (target.len() - literal_start) as u64;
    }
    MultiEncoding {
        ops,
        copies,
        copy_bytes,
        insert_bytes,
        per_parent_copies,
        per_parent_bytes,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    }
}

// ---------------------------------------------------------------------------
// container: encode / decode / verify
// ---------------------------------------------------------------------------

fn serialize(
    dtype: &str,
    shape: &[u64],
    parent_roots: &[[u8; 32]],
    target: &[u8],
    encoding: &MultiEncoding,
    target_bytes: &[u8],
) -> (Vec<u8>, u64) {
    let mut body = Vec::new();
    for op in &encoding.ops {
        match op {
            Op::Copy {
                parent,
                offset,
                len,
            } => {
                body.push(0x00);
                body.push(*parent);
                put_varint(*offset, &mut body);
                put_varint(*len, &mut body);
            }
            Op::Insert { offset, len } => {
                body.push(0x01);
                put_varint(*len, &mut body);
                let start = *offset as usize;
                body.extend_from_slice(&target[start..start + *len as usize]);
            }
        }
    }
    body.push(0xff);

    let mut out = Vec::new();
    out.extend_from_slice(CONTAINER_MAGIC);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(dtype.len() as u16).to_le_bytes());
    out.extend_from_slice(dtype.as_bytes());
    out.push(shape.len() as u8);
    for dim in shape {
        put_varint(*dim, &mut out);
    }
    out.push(parent_roots.len() as u8);
    for root in parent_roots {
        out.extend_from_slice(root);
    }
    out.extend_from_slice(&(target.len() as u64).to_le_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&blake3_domain(target_bytes));
    (out, body.len() as u64)
}

struct Container {
    dtype: String,
    shape: Vec<u64>,
    parent_roots: Vec<[u8; 32]>,
    target_len: u64,
    target_root: [u8; 32],
    ops: Vec<Op>,
    inserts: Vec<u8>,
}

fn parse_container(bytes: &[u8]) -> Result<Container, String> {
    let mut cursor = 0usize;
    if bytes.len() < 4 || &bytes[0..4] != CONTAINER_MAGIC {
        return Err("multiparent: bad magic".to_owned());
    }
    cursor += 4;
    let version = u32::from_le_bytes(
        bytes
            .get(cursor..cursor + 4)
            .ok_or_else(|| "multiparent: truncated version".to_owned())?
            .try_into()
            .unwrap(),
    );
    cursor += 4;
    if version != 1 {
        return Err(format!("multiparent: unsupported version {version}"));
    }
    let dtype_len = u16::from_le_bytes(
        bytes
            .get(cursor..cursor + 2)
            .ok_or_else(|| "multiparent: truncated dtype length".to_owned())?
            .try_into()
            .unwrap(),
    ) as usize;
    cursor += 2;
    let dtype = std::str::from_utf8(
        bytes
            .get(cursor..cursor + dtype_len)
            .ok_or_else(|| "multiparent: truncated dtype".to_owned())?,
    )
    .map_err(|_| "multiparent: dtype is not UTF-8".to_owned())?
    .to_owned();
    cursor += dtype_len;
    let rank = *bytes
        .get(cursor)
        .ok_or_else(|| "multiparent: truncated rank".to_owned())? as usize;
    cursor += 1;
    let mut shape = Vec::with_capacity(rank);
    for _ in 0..rank {
        shape.push(get_varint(bytes, &mut cursor)?);
    }
    let parent_count = *bytes
        .get(cursor)
        .ok_or_else(|| "multiparent: truncated parent count".to_owned())?
        as usize;
    cursor += 1;
    let mut parent_roots = Vec::with_capacity(parent_count);
    for _ in 0..parent_count {
        let root: [u8; 32] = bytes
            .get(cursor..cursor + 32)
            .ok_or_else(|| "multiparent: truncated parent root".to_owned())?
            .try_into()
            .unwrap();
        parent_roots.push(root);
        cursor += 32;
    }
    let target_len = u64::from_le_bytes(
        bytes
            .get(cursor..cursor + 8)
            .ok_or_else(|| "multiparent: truncated target length".to_owned())?
            .try_into()
            .unwrap(),
    );
    cursor += 8;
    let body_len = u64::from_le_bytes(
        bytes
            .get(cursor..cursor + 8)
            .ok_or_else(|| "multiparent: truncated body length".to_owned())?
            .try_into()
            .unwrap(),
    ) as usize;
    cursor += 8;
    let body = bytes
        .get(cursor..cursor + body_len)
        .ok_or_else(|| "multiparent: truncated body".to_owned())?;
    cursor += body_len;
    let target_root: [u8; 32] = bytes
        .get(cursor..cursor + 32)
        .ok_or_else(|| "multiparent: truncated target root".to_owned())?
        .try_into()
        .unwrap();
    if cursor + 32 != bytes.len() {
        return Err("multiparent: trailing bytes".to_owned());
    }
    // Parse the op stream before producing any output byte.
    let mut ops = Vec::new();
    let mut inserts = Vec::new();
    let mut position = 0usize;
    loop {
        let tag = *body
            .get(position)
            .ok_or_else(|| "multiparent: unterminated op stream".to_owned())?;
        position += 1;
        match tag {
            0xff => break,
            0x00 => {
                let parent = *body
                    .get(position)
                    .ok_or_else(|| "multiparent: truncated copy".to_owned())?;
                position += 1;
                let offset = get_varint(body, &mut position)?;
                let len = get_varint(body, &mut position)?;
                if parent as usize >= parent_roots.len() {
                    return Err("multiparent: copy names a parent outside the table".to_owned());
                }
                ops.push(Op::Copy {
                    parent,
                    offset,
                    len,
                });
            }
            0x01 => {
                let len = get_varint(body, &mut position)?;
                let start = position;
                let end = position
                    .checked_add(len as usize)
                    .ok_or_else(|| "multiparent: insert length overflow".to_owned())?;
                if end > body.len() {
                    return Err("multiparent: truncated insert".to_owned());
                }
                let insert_offset = inserts.len() as u64;
                inserts.extend_from_slice(&body[start..end]);
                position = end;
                ops.push(Op::Insert {
                    offset: insert_offset,
                    len,
                });
            }
            other => return Err(format!("multiparent: unknown op tag {other:#04x}")),
        }
    }
    if position != body.len() {
        return Err("multiparent: trailing op bytes".to_owned());
    }
    Ok(Container {
        dtype,
        shape,
        parent_roots,
        target_len,
        target_root,
        ops,
        inserts,
    })
}

/// Decode with the declared parents. A missing parent, a parent whose bytes do
/// not hash to the declared root, a copy outside its parent, a length mismatch
/// or a final hash mismatch all return `Err` **before any output is returned**.
fn decode_multi(
    container_bytes: &[u8],
    provided: &HashMap<[u8; 32], Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let parsed = parse_container(container_bytes)?;
    let mut parents: Vec<&[u8]> = Vec::with_capacity(parsed.parent_roots.len());
    for root in &parsed.parent_roots {
        let bytes = provided.get(root).ok_or_else(|| {
            format!(
                "multiparent: missing parent {} (typed refusal, no bytes returned)",
                hex32(root)
            )
        })?;
        if blake3_domain(bytes) != *root {
            return Err(format!(
                "multiparent: parent {} does not hash to its declared root",
                hex32(root)
            ));
        }
        parents.push(bytes.as_slice());
    }
    let capacity = usize::try_from(parsed.target_len)
        .map_err(|_| "multiparent: target length does not fit usize".to_owned())?;
    let mut out = Vec::with_capacity(capacity.min(1 << 26));
    for op in &parsed.ops {
        match op {
            Op::Copy {
                parent,
                offset,
                len,
            } => {
                let base = parents[*parent as usize];
                let start = *offset as usize;
                let end = start
                    .checked_add(*len as usize)
                    .ok_or_else(|| "multiparent: copy overflow".to_owned())?;
                if end > base.len() {
                    return Err("multiparent: copy past the end of its parent".to_owned());
                }
                out.extend_from_slice(&base[start..end]);
            }
            Op::Insert { offset, len } => {
                let start = *offset as usize;
                out.extend_from_slice(&parsed.inserts[start..start + *len as usize]);
            }
        }
    }
    if out.len() as u64 != parsed.target_len {
        return Err(format!(
            "multiparent: decoded {} bytes, declared {}",
            out.len(),
            parsed.target_len
        ));
    }
    if blake3_domain(&out) != parsed.target_root {
        return Err("multiparent: decoded bytes do not match the declared target root".to_owned());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// corpus and selection
// ---------------------------------------------------------------------------

struct Checkpoint {
    label: String,
    file: SafetensorsFile,
    tensors: BTreeMap<String, usize>,
}

impl Checkpoint {
    fn open(path: &Path, label: &str) -> Result<Checkpoint, String> {
        let file = SafetensorsFile::parse(path).map_err(|err| err.to_string())?;
        let tensors = file
            .tensors
            .iter()
            .enumerate()
            .map(|(index, tensor)| (tensor.name.clone(), index))
            .collect();
        Ok(Checkpoint {
            label: label.to_owned(),
            file,
            tensors,
        })
    }

    fn bytes(&self, name: &str) -> Result<Vec<u8>, String> {
        let index = *self
            .tensors
            .get(name)
            .ok_or_else(|| format!("{}: missing tensor {name}", self.label))?;
        self.file
            .read_tensor(&self.file.tensors[index])
            .map_err(|err| format!("{}: {err}", self.label))
    }
}

fn step_label(step: u32) -> String {
    format!("pythia-step{step}")
}

fn step_path(dir: &Path, step: u32) -> PathBuf {
    dir.join(format!("pythia-70m-step{step}.safetensors"))
}

/// Parent preference order for one target: nearest earlier checkpoint first,
/// then step 0, then the next nearest earlier checkpoints.
fn parent_order(target: u32, available: &[u32], limit: usize) -> Vec<u32> {
    let mut earlier: Vec<u32> = available
        .iter()
        .copied()
        .filter(|step| *step < target)
        .collect();
    earlier.sort_unstable_by(|a, b| b.cmp(a));
    let mut order = Vec::new();
    if let Some(nearest) = earlier.first().copied() {
        order.push(nearest);
    }
    if available.contains(&0) && !order.contains(&0) {
        order.push(0);
    }
    for step in earlier {
        if order.len() >= limit {
            break;
        }
        if !order.contains(&step) {
            order.push(step);
        }
    }
    order.truncate(limit);
    order
}

fn subsets(items: &[u32], size: usize) -> Vec<Vec<u32>> {
    fn walk(items: &[u32], size: usize, start: usize, current: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
        if current.len() == size {
            out.push(current.clone());
            return;
        }
        for index in start..items.len() {
            current.push(items[index]);
            walk(items, size, index + 1, current, out);
            current.pop();
        }
    }
    let mut out = Vec::new();
    if size == 0 || size > items.len() {
        return out;
    }
    walk(items, size, 0, &mut Vec::new(), &mut out);
    out
}

fn changed_words(a: &[u8], b: &[u8]) -> (u64, u64) {
    let words = a.len().min(b.len()) / 2;
    let mut changed = 0u64;
    for index in 0..words {
        let left = u16::from_le_bytes([a[index * 2], a[index * 2 + 1]]);
        let right = u16::from_le_bytes([b[index * 2], b[index * 2 + 1]]);
        if left != right {
            changed += 1;
        }
    }
    (changed, words as u64)
}

// ---------------------------------------------------------------------------
// one target
// ---------------------------------------------------------------------------

struct TensorRow {
    name: String,
    baseline_decode_ms: Json,
    logical_bytes: u64,
    nearest_parent: String,
    changed_words: u64,
    words: u64,
    best_single_delta: u64,
    best_single_delta_body: u64,
    best_single_delta_parent: String,
    best_single_weight_delta: u64,
    best_single_sparse_delta: u64,
    float_tensor_bytes: u64,
    multiparent: Vec<FanoutRow>,
    decode_ms: BTreeMap<String, f64>,
    availability: Json,
}

struct FanoutRow {
    fanout: usize,
    parents: Vec<String>,
    bytes: u64,
    body_bytes: u64,
    same_parent_mode3_bytes: Option<u64>,
    same_parent_mode3_body_bytes: Option<u64>,
    copies: u64,
    copy_bytes: u64,
    insert_bytes: u64,
    encode_ms: f64,
    decode_ms: f64,
    exact: bool,
}

fn measure_target(
    _target_label: &str,
    checkpoint: &Checkpoint,
    name: &str,
    parents: &[&Checkpoint],
    parent_steps: &[u32],
    fanouts: &[usize],
    reps: usize,
    limits: &Limits,
) -> Result<TensorRow, String> {
    let target = checkpoint.bytes(name)?;
    let dtype_name = checkpoint.file.tensors[*checkpoint.tensors.get(name).unwrap()]
        .dtype
        .clone();
    let shape = checkpoint.file.tensors[*checkpoint.tensors.get(name).unwrap()]
        .shape
        .clone();
    let logical_bytes = target.len() as u64;

    let codec_name = codec_dtype(&dtype_name)?;
    let float_dtype =
        FloatDtype::from_name(codec_name).map_err(|err| format!("{name}: {err}"))?;
    let weight_dtype =
        WeightDtype::from_name(codec_name).map_err(|err| format!("{name}: {err}"))?;

    let mut parent_bytes: Vec<Vec<u8>> = Vec::with_capacity(parents.len());
    for parent in parents {
        parent_bytes.push(parent.bytes(name)?);
    }
    let parent_refs: Vec<&[u8]> = parent_bytes.iter().map(|bytes| bytes.as_slice()).collect();

    let (changed, words) = changed_words(&parent_bytes[0], &target);

    // Baselines: the best single parent for each family.
    let mut best_delta = u64::MAX;
    let mut best_delta_parent = String::new();
    let mut mode3_by_parent: Vec<u64> = Vec::with_capacity(parent_refs.len());
    let mut mode3_body_by_parent: Vec<u64> = Vec::with_capacity(parent_refs.len());
    let mut best_delta_frame: Vec<u8> = Vec::new();
    let mut best_weight = u64::MAX;
    let mut best_weight_frame: Vec<u8> = Vec::new();
    let mut best_weight_base: Vec<u8> = Vec::new();
    let mut best_sparse = u64::MAX;
    for (index, base) in parent_refs.iter().enumerate() {
        let delta = modes::encode_delta(&target, base, limits)
            .map_err(|err| format!("{name}: mode 3 encode: {err}"))?;
        mode3_by_parent.push(delta.len() as u64);
        let parsed = weftcodec::frame::Frame::parse(&delta, limits)
            .map_err(|err| format!("{name}: mode 3 parse: {err}"))?;
        mode3_body_by_parent.push(parsed.body.len() as u64);
        if (delta.len() as u64) < best_delta {
            best_delta = delta.len() as u64;
            best_delta_parent = parents[index].label.clone();
            best_delta_frame = delta.clone();
        }
        let base_frame = modes::encode_float_tensor(base, float_dtype, &shape, limits)
            .map_err(|err| format!("{name}: base mode 9: {err}"))?;
        let weight = encode_weight_delta(&target, weight_dtype, &shape, &base_frame, limits)
            .map_err(|err| format!("{name}: mode 11 encode: {err}"))?;
        if (weight.len() as u64) < best_weight {
            best_weight = weight.len() as u64;
            best_weight_frame = weight.clone();
            best_weight_base = base_frame.clone();
        }
        if let Ok(sparse) = encode_sparse_delta(&target, weight_dtype, &shape, &base_frame, limits)
        {
            if (sparse.len() as u64) < best_sparse {
                best_sparse = sparse.len() as u64;
            }
        }
    }
    let float_tensor = modes::encode_float_tensor(&target, float_dtype, &shape, limits)
        .map_err(|err| format!("{name}: mode 9 target: {err}"))?;

    // Baseline decode cost on the same bytes: mode 3 and mode 11 (best parent).
    let mut mode3_samples = Vec::with_capacity(reps);
    let mut mode11_samples = Vec::with_capacity(reps);
    let mut delta_decoded: Option<Vec<u8>> = None;
    let mut weight_decoded: Option<Vec<u8>> = None;
    for _ in 0..reps {
        let mut resolver = MapResolver::new();
        for bytes in &parent_bytes {
            resolver.insert_bytes(bytes.clone());
        }
        let started = Instant::now();
        let decoded = modes::decode_bytes(&best_delta_frame, &resolver, limits)
            .map_err(|err| format!("{name}: mode 3 decode: {err}"))?;
        mode3_samples.push(started.elapsed().as_secs_f64() * 1000.0);
        delta_decoded = Some(decoded);

        let resolver = MapResolver::new();
        let started = Instant::now();
        let parsed = weftcodec::frame::Frame::parse(&best_weight_frame, limits)
            .map_err(|err| format!("{name}: mode 11 parse: {err}"))?;
        let decoded = decode_weight(&parsed, &best_weight_base, &resolver, limits)
            .map_err(|err| format!("{name}: mode 11 decode: {err}"))?;
        mode11_samples.push(started.elapsed().as_secs_f64() * 1000.0);
        weight_decoded = Some(decoded);
    }
    if delta_decoded.as_deref() != Some(target.as_slice()) {
        return Err(format!("{name}: mode 3 decode is not byte-exact"));
    }
    if weight_decoded.as_deref() != Some(target.as_slice()) {
        return Err(format!("{name}: mode 11 decode is not byte-exact"));
    }
    let baseline_decode_ms = json!({
        "mode3": median(mode3_samples),
        "mode11": median(mode11_samples),
    });

    // Multi-parent over every subset of each fan-out.
    let indexes: Vec<Index> = parent_refs.iter().map(|base| Index::build(base)).collect();
    let mut rows = Vec::new();
    let mut decode_ms = BTreeMap::new();
    for fanout in fanouts {
        let mut best: Option<(Vec<u32>, MultiEncoding, Vec<u8>, u64)> = None;
        for subset in subsets(parent_steps, *fanout) {
            let chosen: Vec<usize> = subset
                .iter()
                .map(|step| {
                    parent_steps
                        .iter()
                        .position(|candidate| candidate == step)
                        .unwrap()
                })
                .collect();
            let subset_bytes: Vec<&[u8]> = chosen.iter().map(|index| parent_refs[*index]).collect();
            let subset_indexes: Vec<&Index> =
                chosen.iter().map(|index| &indexes[*index]).collect();
            let encoding = encode_multi(&target, &subset_bytes, &subset_indexes);
            let roots: Vec<[u8; 32]> = subset_bytes
                .iter()
                .map(|bytes| blake3_domain(bytes))
                .collect();
            let (container, body_bytes) =
                serialize(&dtype_name, &shape, &roots, &target, &encoding, &target);
            let better = match &best {
                None => true,
                Some((_, _, bytes, _)) => container.len() < bytes.len(),
            };
            if better {
                best = Some((subset.clone(), encoding, container, body_bytes));
            }
        }
        let Some((subset, encoding, container, body_bytes)) = best else {
            continue;
        };
        // Decode exactly and time it.
        let mut provided: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
        for (index, step) in parent_steps.iter().enumerate() {
            if subset.contains(step) {
                provided.insert(
                    blake3_domain(&parent_bytes[index]),
                    parent_bytes[index].clone(),
                );
            }
        }
        let sample_ms = {
            let mut samples = Vec::with_capacity(reps);
            let mut decoded: Option<Vec<u8>> = None;
            for _ in 0..reps {
                let started = Instant::now();
                decoded = Some(decode_multi(&container, &provided).map_err(|err| err.to_string())?);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            let decoded = decoded.unwrap();
            if decoded != target {
                return Err(format!("{name}: multi-parent decode is not byte-exact"));
            }
            median(samples)
        };
        let key = format!("fanout{}", subset.len());
        decode_ms.insert(key, sample_ms);
        let same_parent = if subset.len() == 1 {
            parent_steps.iter().position(|step| *step == subset[0])
        } else {
            None
        };
        let same_parent_mode3 = same_parent.map(|index| mode3_by_parent[index]);
        let same_parent_mode3_body = same_parent.map(|index| mode3_body_by_parent[index]);
        rows.push(FanoutRow {
            fanout: subset.len(),
            parents: subset.iter().map(|step| step_label(*step)).collect(),
            bytes: container.len() as u64,
            body_bytes,
            same_parent_mode3_bytes: same_parent_mode3,
            same_parent_mode3_body_bytes: same_parent_mode3_body,
            copies: encoding.copies,
            copy_bytes: encoding.copy_bytes,
            insert_bytes: encoding.insert_bytes,
            encode_ms: encoding.elapsed_ms,
            decode_ms: sample_ms,
            exact: true,
        });
    }

    // Availability: the missing-parent and wrong-parent refusals.
    let availability = {
        let mut provided: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
        let all_indexes: Vec<&Index> = indexes.iter().collect();
        let encoding = encode_multi(&target, &parent_refs, &all_indexes);
        let roots: Vec<[u8; 32]> = parent_refs
            .iter()
            .map(|bytes| blake3_domain(bytes))
            .collect();
        let (container, _body) =
            serialize(&dtype_name, &shape, &roots, &target, &encoding, &target);
        for base in &parent_refs {
            provided.insert(blake3_domain(base), base.to_vec());
        }
        let full_ok = decode_multi(&container, &provided).is_ok();
        // Drop the last declared parent.
        let missing_root = roots[roots.len() - 1];
        let mut missing = provided.clone();
        missing.remove(&missing_root);
        let missing_result = decode_multi(&container, &missing);
        // Corrupt a parent's bytes.
        let mut corrupt = provided.clone();
        let mut bytes = corrupt.remove(&missing_root).unwrap();
        if let Some(byte) = bytes.first_mut() {
            *byte ^= 0xff;
        }
        corrupt.insert(missing_root, bytes);
        let corrupt_result = decode_multi(&container, &corrupt);
        json!({
            "full_decode_ok": full_ok,
            "missing_parent": {
                "refused": missing_result.is_err(),
                "error": missing_result.err(),
                "bytes_returned": 0,
            },
            "corrupt_parent": {
                "refused": corrupt_result.is_err(),
                "error": corrupt_result.err(),
                "bytes_returned": 0,
            },
        })
    };

    Ok(TensorRow {
        name: name.to_owned(),
        baseline_decode_ms,
        logical_bytes,
        nearest_parent: parents[0].label.clone(),
        changed_words: changed,
        words,
        best_single_delta: best_delta,
        best_single_delta_body: mode3_body_by_parent
            .iter()
            .copied()
            .min()
            .unwrap_or(0),
        best_single_delta_parent: best_delta_parent,
        best_single_weight_delta: best_weight,
        best_single_sparse_delta: best_sparse,
        float_tensor_bytes: float_tensor.len() as u64,
        multiparent: rows,
        decode_ms,
        availability,
    })
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = PathBuf::from(
        arg(&args, "--corpus").unwrap_or_else(|| ".docker-cache/real-corpus".to_owned()),
    );
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "multi-parent.json".to_owned()));
    let max_tensors: usize = arg(&args, "--max-tensors")
        .and_then(|value| value.parse().ok())
        .unwrap_or(16);
    let max_tensor_bytes: u64 = arg(&args, "--max-tensor-bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(32 * 1024 * 1024);
    let reps: usize = arg(&args, "--reps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let max_parents: usize = arg(&args, "--max-parents")
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let targets: Vec<u32> = arg(&args, "--targets")
        .map(|text| {
            text.split(',')
                .filter_map(|part| part.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 8, 16, 64, 512, 1000]);
    let fanouts: Vec<usize> = arg(&args, "--fanouts")
        .map(|text| {
            text.split(',')
                .filter_map(|part| part.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 2, 3]);
    let limits = Limits::default();
    if arg_flag(&args, "--skip-availability") {
        // Kept as an explicit switch so a reduced run records that it skipped
        // the refusal matrix rather than silently omitting it.
    }

    let mut rows: Vec<Json> = Vec::new();
    let mut checkpoint_rows: Vec<Json> = Vec::new();
    let mut totals = Totals::default();
    let mut refusals = 0u32;
    let mut checked_refusals = 0u32;

    for target_step in &targets {
        let target_path = step_path(&corpus, *target_step);
        let target = Checkpoint::open(&target_path, &step_label(*target_step))?;
        checkpoint_rows.push(json!({
            "label": target.label,
            "path": target_path.display().to_string(),
            "file_bytes": target.file.file_bytes,
            "sha256": target.file.sha256.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "tensors": target.file.tensors.len(),
        }));
        let available: Vec<u32> = [0u32, 1, 2, 4, 8, 16, 64, 512, 1000]
            .into_iter()
            .filter(|step| step_path(&corpus, *step).exists())
            .collect();
        let order = parent_order(*target_step, &available, max_parents);
        if order.is_empty() {
            continue;
        }
        let mut parents: Vec<Checkpoint> = Vec::new();
        for step in &order {
            parents.push(Checkpoint::open(
                &step_path(&corpus, *step),
                &step_label(*step),
            )?);
        }
        // Candidate tensors: present in the target and every chosen parent,
        // same dtype and shape, within the byte cap.
        let mut names: Vec<String> = Vec::new();
        for tensor in &target.file.tensors {
            if tensor.byte_len() > max_tensor_bytes {
                continue;
            }
            let mut compatible = true;
            for parent in &parents {
                match parent.tensors.get(&tensor.name) {
                    Some(index) => {
                        let other = &parent.file.tensors[*index];
                        if other.dtype != tensor.dtype || other.shape != tensor.shape {
                            compatible = false;
                            break;
                        }
                    }
                    None => {
                        compatible = false;
                        break;
                    }
                }
            }
            if compatible {
                names.push(tensor.name.clone());
            }
        }
        // Size-stratified sample: alphabetical order is biased (every pythia
        // `*.bias` sorts first and is 2 KiB), so order by byte length first and
        // then take an evenly spaced stride across the size distribution.
        let size_of: BTreeMap<String, u64> = target
            .file
            .tensors
            .iter()
            .map(|tensor| (tensor.name.clone(), tensor.byte_len()))
            .collect();
        names.sort_by_key(|name| (size_of.get(name).copied().unwrap_or(0), name.clone()));
        if names.len() > max_tensors {
            let stride = names.len() / max_tensors;
            names = names
                .into_iter()
                .enumerate()
                .filter(|(index, _)| index % stride.max(1) == 0)
                .map(|(_, name)| name)
                .take(max_tensors)
                .collect();
        }

        let parent_refs: Vec<&Checkpoint> = parents.iter().collect();
        for name in &names {
            let row = measure_target(
                &target.label,
                &target,
                name,
                &parent_refs,
                &order,
                &fanouts,
                reps,
                &limits,
            )?;
            totals.tensors += 1;
            totals.logical_bytes += row.logical_bytes;
            totals.changed_words += row.changed_words;
            totals.words += row.words;
            totals.best_single_delta += row.best_single_delta;
            totals.best_single_delta_body += row.best_single_delta_body;
            totals.best_single_weight += row.best_single_weight_delta;
            totals.best_single_sparse += row.best_single_sparse_delta;
            totals.float_tensor += row.float_tensor_bytes;
            let best_multi = row
                .multiparent
                .iter()
                .min_by_key(|fanout| fanout.bytes)
                .map(|fanout| fanout.bytes)
                .unwrap_or(u64::MAX);
            totals.best_multi += best_multi;
            // Payload-normalised body: the op payload without the container's
            // per-COPY parent index and without the op-stream terminator.
            totals.best_multi_body += row
                .multiparent
                .iter()
                .map(|fanout| fanout.body_bytes - fanout.copies - 1)
                .min()
                .unwrap_or(u64::MAX);
            if best_multi < row.best_single_delta {
                totals.multi_wins_vs_delta += 1;
            }
            if best_multi < row.best_single_weight_delta {
                totals.multi_wins_vs_weight += 1;
            }
            if row.availability["missing_parent"]["refused"] == json!(true) {
                refusals += 1;
            }
            if row.availability["corrupt_parent"]["refused"] == json!(true) {
                refusals += 1;
            }
            checked_refusals += 2;
            rows.push(json!({
                "target": target.label,
                "name": row.name,
                "logical_bytes": row.logical_bytes,
                "nearest_parent": row.nearest_parent,
                "changed_words": row.changed_words,
                "words": row.words,
                "changed_fraction": ratio(row.changed_words, row.words),
                "best_single_delta_bytes": row.best_single_delta,
                "best_single_delta_body_bytes": row.best_single_delta_body,
                "best_single_delta_parent": row.best_single_delta_parent,
                "best_single_weight_delta_bytes": row.best_single_weight_delta,
                "best_single_sparse_delta_bytes": row.best_single_sparse_delta,
                "float_tensor_bytes": row.float_tensor_bytes,
                "baseline_decode_ms": row.baseline_decode_ms,
                "multiparent": row.multiparent.iter().map(|fanout| json!({
                    "fanout": fanout.fanout,
                    "parents": fanout.parents,
                    "bytes": fanout.bytes,
                    "body_bytes": fanout.body_bytes,
                    "same_parent_mode3_bytes": fanout.same_parent_mode3_bytes,
                    "same_parent_mode3_body_bytes": fanout.same_parent_mode3_body_bytes,
                    "header_delta_vs_same_parent_mode3": fanout.same_parent_mode3_bytes
                        .map(|mode3| fanout.bytes as i64 - mode3 as i64),
                    // The container names a parent in every COPY (one byte);
                    // mode 3 does not need that byte. Subtracting it isolates
                    // the pure header difference and proves the op streams are
                    // otherwise identical at fan-out 1.
                    "container_body_delta_vs_same_parent_mode3":
                        fanout.same_parent_mode3_bytes.map(|mode3| {
                            fanout.bytes as i64 - mode3 as i64 - fanout.copies as i64
                        }),
                    "copies": fanout.copies,
                    "copy_bytes": fanout.copy_bytes,
                    "insert_bytes": fanout.insert_bytes,
                    "encode_ms": fanout.encode_ms,
                    "decode_ms": fanout.decode_ms,
                    "exact": fanout.exact,
                    "ratio_vs_best_single_delta": ratio(fanout.bytes, row.best_single_delta),
                    "ratio_vs_best_single_weight_delta": ratio(fanout.bytes, row.best_single_weight_delta),
                })).collect::<Vec<_>>(),
                "decode_ms": row.decode_ms,
                "availability": row.availability,
            }));
        }
        eprintln!(
            "T091: {} tensors from target {} (parents {})",
            names.len(),
            target.label,
            order
                .iter()
                .map(|step| step.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    // Aggregate per fanout: sum of the best subset per tensor at that fanout.
    let mut baseline_decode_totals = (0.0f64, 0.0f64);
    for row in &rows {
        baseline_decode_totals.0 += row["baseline_decode_ms"]["mode3"].as_f64().unwrap_or(0.0);
        baseline_decode_totals.1 += row["baseline_decode_ms"]["mode11"].as_f64().unwrap_or(0.0);
    }
    let mut fanout_totals: BTreeMap<usize, u64> = BTreeMap::new();
    for row in &rows {
        let mut best: BTreeMap<usize, u64> = BTreeMap::new();
        for fanout in row["multiparent"].as_array().unwrap() {
            let size = fanout["fanout"].as_u64().unwrap() as usize;
            let bytes = fanout["bytes"].as_u64().unwrap();
            let entry = best.entry(size).or_insert(u64::MAX);
            if bytes < *entry {
                *entry = bytes;
            }
        }
        for (size, bytes) in best {
            *fanout_totals.entry(size).or_insert(0) += bytes;
        }
    }

    let document = json!({
        "schema": SCHEMA,
        "generated_at": utc_stamp(),
        "revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
        "environment": {
            "court": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "limits": {
                "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "?".to_owned()),
                "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "?".to_owned()),
                "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "?".to_owned()),
            },
        },
        "container": {
            "magic": "WMP1",
            "version": 1,
            "matcher": "shipped mode-3 matcher (4-byte keys, 1,000,000 sampled positions, four candidates per key, greedy longest match >= 8 bytes)",
            "parent_identity": "BLAKE3 over the parent's exact tensor bytes with the weft/multiparent/v1 domain",
            "exactness": "declared parent roots verified, decoded length verified, trailing BLAKE3 target root verified",
        },
        "configuration": {
            "corpus": corpus.display().to_string(),
            "targets": targets,
            "fanouts": fanouts,
            "max_parents": max_parents,
            "max_tensors_per_target": max_tensors,
            "max_tensor_bytes": max_tensor_bytes,
            "decode_reps": reps,
        },
        "checkpoints": checkpoint_rows,
        "totals": {
            "tensors": totals.tensors,
            "logical_bytes": totals.logical_bytes,
            "changed_words": totals.changed_words,
            "words": totals.words,
            "changed_fraction": ratio(totals.changed_words, totals.words),
            "best_single_delta_bytes": totals.best_single_delta,
            "best_single_weight_delta_bytes": totals.best_single_weight,
            "best_single_sparse_delta_bytes": totals.best_single_sparse,
            "float_tensor_bytes": totals.float_tensor,
            "best_multiparent_bytes": totals.best_multi,
            "best_multiparent_body_bytes": totals.best_multi_body,
            "best_single_delta_body_bytes": totals.best_single_delta_body,
            "multiparent_over_single_delta": ratio(totals.best_multi, totals.best_single_delta),
            "multiparent_over_single_weight_delta": ratio(totals.best_multi, totals.best_single_weight),
            "tensors_where_multiparent_beats_single_delta": totals.multi_wins_vs_delta,
            "tensors_where_multiparent_beats_single_weight_delta": totals.multi_wins_vs_weight,
            "fanout_totals": fanout_totals.iter().map(|(size, bytes)| json!({
                "fanout": size,
                "bytes": bytes,
                "over_best_single_delta": ratio(*bytes, totals.best_single_delta),
                "over_best_single_weight_delta": ratio(*bytes, totals.best_single_weight),
            })).collect::<Vec<_>>(),
            "baseline_decode_ms_sum": {
                "mode3": baseline_decode_totals.0,
                "mode11": baseline_decode_totals.1,
            },
            "availability_refusals": refusals,
            "availability_checks": checked_refusals,
        },
        "tensors": rows,
    });
    let bytes = serde_json::to_vec_pretty(&document).map_err(|err| err.to_string())?;
    fs::write(&out, bytes).map_err(|err| format!("write {}: {err}", out.display()))?;
    println!(
        "{}",
        json!({
            "out": out.display().to_string(),
            "tensors": totals.tensors,
            "best_multiparent_bytes": totals.best_multi,
            "best_multiparent_body_bytes": totals.best_multi_body,
            "best_single_delta_body_bytes": totals.best_single_delta_body,
            "best_single_delta_bytes": totals.best_single_delta,
            "best_single_weight_delta_bytes": totals.best_single_weight,
        })
    );
    Ok(())
}

#[derive(Default)]
struct Totals {
    tensors: u64,
    logical_bytes: u64,
    changed_words: u64,
    words: u64,
    best_single_delta: u64,
    best_single_delta_body: u64,
    best_multi_body: u64,
    best_single_weight: u64,
    best_single_sparse: u64,
    float_tensor: u64,
    best_multi: u64,
    multi_wins_vs_delta: u64,
    multi_wins_vs_weight: u64,
}

fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{year:04}{m:02}{d:02}T{:02}{:02}{:02}{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        now.subsec_millis()
    )
}

fn main() {
    if let Err(err) = run() {
        eprintln!("multi_parent_court: {err}");
        std::process::exit(1);
    }
}
