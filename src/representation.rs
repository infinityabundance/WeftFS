//! Experimental codec-backed representation support.
//!
//! A `weftfs.manifest.v2` manifest can name a WFT1 frame blob as the physical
//! representation of a file's logical bytes. This module holds the policy
//! constants shared by the writer, reader, GC, and verify paths, plus a
//! test-visible counter of chunk decodes so a test can prove a range read does
//! not decode unrelated chunks.
//!
//! **Status: experimental and Rust-only.** The Go store is expected to reject
//! v2 manifests (unknown schema) until parity lands. See
//! `docs/store-status.md`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum transitive dependency depth for a codec representation.
pub const MAX_CODEC_DEPENDENCY_DEPTH: usize = 8;

/// Largest codec frame chunk size the writer will choose.
pub const CODEC_CHUNK_SIZE_MAX: u64 = 64 * 1024;

/// Smallest codec frame chunk size the writer will choose.
pub const CODEC_CHUNK_SIZE_MIN: u64 = 4 * 1024;

/// zstd level used by the experimental auto representation.
pub const CODEC_ZSTD_LEVEL: i32 = 3;

static CODEC_CHUNK_DECODES: AtomicU64 = AtomicU64::new(0);

/// Number of codec chunks decoded by the store since the last reset.
///
/// `#[doc(hidden)]` test/bench hook only; it is not a format feature.
#[doc(hidden)]
pub fn codec_chunk_decode_count() -> u64 {
    CODEC_CHUNK_DECODES.load(Ordering::Relaxed)
}

/// Reset [`codec_chunk_decode_count`].
#[doc(hidden)]
pub fn reset_codec_chunk_decode_count() {
    CODEC_CHUNK_DECODES.store(0, Ordering::Relaxed);
}

pub(crate) fn note_codec_chunk_decode() {
    CODEC_CHUNK_DECODES.fetch_add(1, Ordering::Relaxed);
}

/// The codec frame chunk size for a store chunk size.
pub fn codec_chunk_size(store_chunk_size: u64) -> u64 {
    store_chunk_size.clamp(CODEC_CHUNK_SIZE_MIN, CODEC_CHUNK_SIZE_MAX)
}
