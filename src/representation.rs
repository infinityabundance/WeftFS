//! Experimental codec-backed representation support.
//!
//! A `weft.manifest.v2` manifest can name a WFT1 frame blob as the physical
//! representation of a file's logical bytes. This module holds the policy
//! constants shared by the writer, reader, GC, and verify paths, plus a
//! test-visible counter of chunk decodes so a test can prove a range read does
//! not decode unrelated chunks.
//!
//! **Status: experimental and Rust-only.** The Go store is expected to reject
//! v2 manifests (unknown schema) until parity lands. See
//! `docs/store-status.md`.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

/// Maximum transitive dependency depth for a codec or tensor representation.
pub const MAX_CODEC_DEPENDENCY_DEPTH: usize = 8;

/// Hard bound on the number of chained immutable overlay manifests a reader
/// will resolve (`WEFT-MODEL.md` §2.3.3). A reader refuses a deeper chain with
/// `LimitExceeded`; a writer MUST stay at or below this bound and compact
/// instead. It is a reader bound so that a future writer option can never make
/// already-published data unreadable.
pub const MAX_OVERLAY_CHAIN_DEPTH: u64 = 256;

/// Largest number of extents a single `weft.manifest.v4` overlay manifest may
/// list. A writer may publish several non-overlapping extents in one manifest;
/// they MUST be sorted by offset and may touch but not overlap.
pub const MAX_OVERLAY_EXTENTS_PER_MANIFEST: usize = 64;

/// Largest codec frame chunk size the writer will choose.
pub const CODEC_CHUNK_SIZE_MAX: u64 = 64 * 1024;

/// Smallest codec frame chunk size the writer will choose.
pub const CODEC_CHUNK_SIZE_MIN: u64 = 4 * 1024;

/// zstd level used by the experimental auto representation.
pub const CODEC_ZSTD_LEVEL: i32 = 3;

thread_local! {
    /// Chunk decodes observed by **this thread**.
    ///
    /// The counter is deliberately per-thread: `cargo test` runs the tests of
    /// one binary in parallel on a thread pool, and a process-global counter
    /// let an unrelated test that decodes chunks inflate another test's
    /// observation (the historical
    /// `codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks`
    /// flake, which read 5 chunks instead of 1). Representation reads decode
    /// on the calling thread, so a per-thread observation is exactly the work
    /// the calling test performed and the assertion keeps its original
    /// meaning. The process-wide total is still available from
    /// [`codec_chunk_decode_total`].
    static CODEC_CHUNK_DECODES: Cell<u64> = const { Cell::new(0) };
}

static CODEC_CHUNK_DECODES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Number of codec chunks decoded by the calling thread since the last reset.
///
/// `#[doc(hidden)]` test/bench hook only; it is not a format feature.
#[doc(hidden)]
pub fn codec_chunk_decode_count() -> u64 {
    CODEC_CHUNK_DECODES.with(|count| count.get())
}

/// Reset this thread's [`codec_chunk_decode_count`].
#[doc(hidden)]
pub fn reset_codec_chunk_decode_count() {
    CODEC_CHUNK_DECODES.with(|count| count.set(0));
}

/// Number of codec chunks decoded by the whole process since start-up.
///
/// This is a diagnostic aggregate only; it can never be reset, so concurrent
/// tests cannot make each other's observation of it exact. `#[doc(hidden)]`.
#[doc(hidden)]
pub fn codec_chunk_decode_total() -> u64 {
    CODEC_CHUNK_DECODES_TOTAL.load(Ordering::Relaxed)
}

pub(crate) fn note_codec_chunk_decode() {
    CODEC_CHUNK_DECODES.with(|count| count.set(count.get().saturating_add(1)));
    CODEC_CHUNK_DECODES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

thread_local! {
    /// Overlay content-chunk reads observed by this thread (manifest reads are
    /// not counted). A range read must touch only the chunks it needs.
    static OVERLAY_CHUNK_READS: Cell<u64> = const { Cell::new(0) };
}

/// Number of content chunks an overlay read fetched on the calling thread.
///
/// `#[doc(hidden)]` test/bench hook only; it is not a format feature.
#[doc(hidden)]
pub fn overlay_chunk_read_count() -> u64 {
    OVERLAY_CHUNK_READS.with(|count| count.get())
}

/// Reset this thread's [`overlay_chunk_read_count`].
#[doc(hidden)]
pub fn reset_overlay_chunk_read_count() {
    OVERLAY_CHUNK_READS.with(|count| count.set(0));
}

pub(crate) fn note_overlay_chunk_read() {
    OVERLAY_CHUNK_READS.with(|count| count.set(count.get().saturating_add(1)));
}

/// The codec frame chunk size for a store chunk size.
pub fn codec_chunk_size(store_chunk_size: u64) -> u64 {
    store_chunk_size.clamp(CODEC_CHUNK_SIZE_MIN, CODEC_CHUNK_SIZE_MAX)
}

/// Element type of a stored logical tensor.
///
/// The canonical spelling is the `WEFT-TENSOR.md` name (`f16`, `f32`, ...),
/// plus the additive quantized names `q8_0` and `q4_0` that mode 10 defines.
/// The weight-delta vocabulary spells the floating types `fp16`/`fp32`; that
/// spelling is a codec-internal alias and is never written to a store manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TensorElementDtype {
    F16,
    Bf16,
    F32,
    F64,
    I8,
    U8,
    I16,
    I32,
    I64,
    Q8_0,
    Q4_0,
}

impl TensorElementDtype {
    /// Parse a canonical tensor dtype name.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "f16" => Ok(TensorElementDtype::F16),
            "bf16" => Ok(TensorElementDtype::Bf16),
            "f32" => Ok(TensorElementDtype::F32),
            "f64" => Ok(TensorElementDtype::F64),
            "i8" => Ok(TensorElementDtype::I8),
            "u8" => Ok(TensorElementDtype::U8),
            "i16" => Ok(TensorElementDtype::I16),
            "i32" => Ok(TensorElementDtype::I32),
            "i64" => Ok(TensorElementDtype::I64),
            "q8_0" => Ok(TensorElementDtype::Q8_0),
            "q4_0" => Ok(TensorElementDtype::Q4_0),
            other => Err(Error::malformed(format!(
                "unknown tensor dtype `{other}` (v1 allows f16, bf16, f32, f64, i8, u8, \
                 i16, i32, i64, q8_0, q4_0)"
            ))),
        }
    }

    /// Canonical `WEFT-TENSOR.md` name.
    pub fn as_str(self) -> &'static str {
        match self {
            TensorElementDtype::F16 => "f16",
            TensorElementDtype::Bf16 => "bf16",
            TensorElementDtype::F32 => "f32",
            TensorElementDtype::F64 => "f64",
            TensorElementDtype::I8 => "i8",
            TensorElementDtype::U8 => "u8",
            TensorElementDtype::I16 => "i16",
            TensorElementDtype::I32 => "i32",
            TensorElementDtype::I64 => "i64",
            TensorElementDtype::Q8_0 => "q8_0",
            TensorElementDtype::Q4_0 => "q4_0",
        }
    }

    /// Logical element bytes. Quantized dtypes use one byte per logical
    /// element: `q8_0` is its two's-complement byte and `q4_0` is the unpacked
    /// nibble (`0..=15`), matching `WEFT-AI-CODEC.md` §12.3.
    pub fn element_bytes(self) -> u64 {
        match self {
            TensorElementDtype::F16 | TensorElementDtype::Bf16 | TensorElementDtype::I16 => 2,
            TensorElementDtype::F32 | TensorElementDtype::I32 => 4,
            TensorElementDtype::F64 | TensorElementDtype::I64 => 8,
            TensorElementDtype::I8
            | TensorElementDtype::U8
            | TensorElementDtype::Q8_0
            | TensorElementDtype::Q4_0 => 1,
        }
    }

    /// True for the two additive quantized element types.
    pub fn is_quantized(self) -> bool {
        matches!(self, TensorElementDtype::Q8_0 | TensorElementDtype::Q4_0)
    }

    /// The core `weftcodec` tensor dtype used for physical layout arithmetic.
    ///
    /// `q8_0` behaves byte-for-byte like `i8` and `q4_0` like `u8` (unpacked
    /// nibbles), which is exactly how WEFT-TENSOR §4 defines the physical
    /// buffers.
    pub fn physical_tensor_dtype(self) -> Option<weftcodec::TensorDtype> {
        use weftcodec::TensorDtype;
        match self {
            TensorElementDtype::F16 => Some(TensorDtype::F16),
            TensorElementDtype::Bf16 => Some(TensorDtype::Bf16),
            TensorElementDtype::F32 => Some(TensorDtype::F32),
            TensorElementDtype::F64 => Some(TensorDtype::F64),
            TensorElementDtype::I8 => Some(TensorDtype::I8),
            TensorElementDtype::U8 => Some(TensorDtype::U8),
            TensorElementDtype::I16 => Some(TensorDtype::I16),
            TensorElementDtype::I32 => Some(TensorDtype::I32),
            TensorElementDtype::I64 => Some(TensorDtype::I64),
            TensorElementDtype::Q8_0 => Some(TensorDtype::I8),
            TensorElementDtype::Q4_0 => Some(TensorDtype::U8),
        }
    }

    /// The weight-delta dtype for this element type, when mode 11/14 supports
    /// it. The store restricts delta strategies to the full-precision dtypes
    /// (`bf16`, `f16`, `f32`); `q8_0`/`q4_0` use mode 10 or a byte
    /// representation because their `LogicalTensorRoot` includes quantization
    /// planes that mode 11 does not carry.
    pub fn weight_dtype(self) -> Option<weftcodec::WeightDtype> {
        use weftcodec::WeightDtype;
        match self {
            TensorElementDtype::Bf16 => Some(WeightDtype::Bf16),
            TensorElementDtype::F16 => Some(WeightDtype::Fp16),
            TensorElementDtype::F32 => Some(WeightDtype::Fp32),
            _ => None,
        }
    }

    /// The mode-10 sharded dtype, when this element type can use mode 10.
    pub fn sharded_dtype(self) -> Option<weftcodec::ShardedDtype> {
        use weftcodec::ShardedDtype;
        match self {
            TensorElementDtype::Q8_0 => Some(ShardedDtype::Q8_0),
            TensorElementDtype::Q4_0 => Some(ShardedDtype::Q4_0),
            _ => None,
        }
    }

    /// The manifest name of a weight-delta descriptor dtype.
    pub fn from_weight_name(name: &str) -> Result<Self> {
        match name {
            "bf16" => Ok(TensorElementDtype::Bf16),
            "fp16" => Ok(TensorElementDtype::F16),
            "fp32" => Ok(TensorElementDtype::F32),
            "q8_0" => Ok(TensorElementDtype::Q8_0),
            "q4_0" => Ok(TensorElementDtype::Q4_0),
            other => Err(Error::malformed(format!("unknown weight-dtype `{other}`"))),
        }
    }
}
