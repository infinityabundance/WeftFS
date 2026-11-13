//! Physical object packs (subphase R11).
//!
//! A content-addressed namespace costs one physical request per small blob:
//! 10,000 one-kilobyte chunks are 10,000 `PUT`s and 10,000 `GET`s. The R10
//! commit plane makes it possible to publish a *catalog* atomically, so small
//! immutable blobs can be grouped into a few large pack objects and served by
//! a ranged read of the member's bytes.
//!
//! # Contract
//!
//! [`PackedBlobPlane`] implements [`BlobPlane`] over an inner blob plane and a
//! [`CommitPlane`]-backed catalog:
//!
//! * **Publication.** A blob whose length is at most
//!   [`PackOptions::member_threshold_bytes`] is appended to the open pack. When
//!   the open pack reaches [`PackOptions::pack_target_bytes`] or
//!   [`PackOptions::max_members_per_pack`] it is published with
//!   `put_if_absent` under a content-addressed [`Key::pack`], and its members
//!   are added to the catalog. Larger blobs bypass packing and are published
//!   flat, exactly as before.
//! * **Ordering.** The pack is published *before* any catalog record names it.
//!   A crash between the two leaves an unreferenced pack (collected by
//!   [`BlobPlane::reclaim_unreferenced_packs`]) and never a catalog entry
//!   pointing at missing bytes.
//! * **Catalog.** Membership is `key -> (pack id, offset, length, digest)`,
//!   split into `catalog_segments` immutable segment blobs plus one mutable
//!   root control record (`pack-catalog.root`) that names every segment. The
//!   root is replaced with a compare-and-swap, so a reader observes the
//!   complete old or the complete new catalog, never a partial one. The catalog
//!   is bounded by [`PackOptions::max_catalog_bytes`].
//! * **Reads.** One catalog lookup, then one `get_range` on the pack for
//!   exactly the member's bytes. The whole pack is never read or decoded for a
//!   small member, and a 4 KiB member of a 16 MiB pack fetches 4 KiB.
//! * **Integrity.** Every whole-member read recomputes the member digest stored
//!   in the catalog and fails closed with `Corrupt` on a mismatch, a short read,
//!   or a pack shorter than the recorded range. A truncated pack is therefore an
//!   error, not a short read.
//! * **Bounds and fallback.** When the catalog would exceed its bound, packing
//!   stops and new blobs are published flat. Nothing fails and no existing
//!   member becomes unreadable.
//! * **Refusal.** A plane that does not provide `true_range_reads` is refused at
//!   construction: packing it would turn a small read into a whole-pack read.
//!
//! No manifest, commit, or superblock byte changes. The logical keys of the
//! store (`objects/<ab>/<hash>`, `manifests/<ab>/<hash>`, `indexes/...`) are
//! unchanged; packs and the catalog occupy new namespaces (`packs/`,
//! `pack-catalog/`, `pack-catalog.root`).
//!
//! # Durability
//!
//! The open pack lives in memory. The store calls [`BlobPlane::flush`] before
//! it publishes a commit record, so the protocol is preserved: every blob named
//! by a published commit is durable and visible. A process crash with an open
//! pack loses only blobs that no commit names.
//!
//! # What is not claimed
//!
//! * Packs are never rewritten; a dead member's bytes stay in the pack until
//!   every member is deleted and the pack is reclaimed.
//! * A cold read of a member costs a catalog root read, one segment read, and
//!   the ranged pack read; a tiny member therefore has read amplification until
//!   the catalog is warm in memory (the catalog is bounded and cached for the
//!   process lifetime).
//! * One writer, one host. The catalog cache assumes the store's snapshot
//!   semantics: a reader sees the catalog as of its open.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::backend::{BackendStats, Key, KeyKind};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::planes::{control_token, BlobCapabilities, BlobPlane, CommitPlane};

/// Domain for a pack object identity: `pack id = H(PACK_DOMAIN, pack bytes)`.
pub const PACK_DOMAIN: &str = "weft/pack/v1";
/// Domain for a catalog segment object identity.
pub const CATALOG_SEGMENT_DOMAIN: &str = "weft/pack/catalog-segment/v1";
/// Domain for the catalog root control record digest.
pub const CATALOG_ROOT_DOMAIN: &str = "weft/pack/catalog-root/v1";
/// Domain for a member digest stored in the catalog.
pub const MEMBER_DOMAIN: &str = "weft/pack/member/v1";

/// Buffered-member kind bit for a chunk object (`objects/`).
///
/// A packed plane holds an open pack's members in memory until it seals. These
/// bits let the store ask which *kinds* a handle is holding, so it can keep
/// using pooled read handles for the kinds that are already on disk. Index
/// segments are packed too, but the store always reads them through the handle
/// that wrote them, so they need no bit.
pub const BUFFERED_OBJECT: u8 = 1 << 0;
/// Buffered-member kind bit for a manifest (`manifests/`).
pub const BUFFERED_MANIFEST: u8 = 1 << 1;

/// Buffered-kind bit for a catalogued member kind byte.
fn buffered_bit(kind: u8) -> u8 {
    match kind {
        0 => BUFFERED_OBJECT,
        1 => BUFFERED_MANIFEST,
        _ => 0,
    }
}

const SEGMENT_MAGIC: &[u8; 4] = b"WPS1";
const ROOT_MAGIC: &[u8; 4] = b"WPR1";
const FORMAT_VERSION: u32 = 1;

/// Encoded bytes of one catalog entry: kind(1) + id(32) + pack(32) +
/// offset(8) + length(8) + digest(32).
pub const CATALOG_ENTRY_BYTES: u64 = 113;
const SEGMENT_HEADER_BYTES: u64 = 4 + 4 + 4 + 4;
const ROOT_HEADER_BYTES: u64 = 4 + 4 + 4 + 8;
const DIGEST_BYTES: u64 = 32;

/// Physical packing policy.
///
/// Every field is a hard bound. `member_threshold_bytes` must not exceed
/// `pack_target_bytes`, and `pack_target_bytes` must not exceed the inner
/// plane's `max_object_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackOptions {
    /// Seal the open pack when appending the next member would exceed this.
    pub pack_target_bytes: u64,
    /// Only blobs at most this large are packed. Larger blobs stay flat.
    pub member_threshold_bytes: u64,
    /// Upper bound on members in one pack.
    pub max_members_per_pack: u32,
    /// Upper bound on the encoded catalog (all segments, excluding the root).
    pub max_catalog_bytes: u64,
    /// Number of catalog segments. Must be a power of two in `1..=4096`.
    pub catalog_segments: u32,
}

impl Default for PackOptions {
    fn default() -> PackOptions {
        PackOptions {
            pack_target_bytes: 16 * 1024 * 1024,
            member_threshold_bytes: 64 * 1024,
            max_members_per_pack: 65_536,
            max_catalog_bytes: 8 * 1024 * 1024,
            catalog_segments: 256,
        }
    }
}

impl PackOptions {
    /// Validate the policy. There is no unvalidated path into the plane.
    pub fn validate(&self) -> Result<()> {
        if self.pack_target_bytes < 4096 {
            return Err(Error::invalid_argument(
                "pack_target_bytes must be at least 4096",
            ));
        }
        if self.pack_target_bytes > 1024 * 1024 * 1024 {
            return Err(Error::limit_exceeded(
                "pack_target_bytes is above the 1 GiB pack bound",
            ));
        }
        if self.member_threshold_bytes == 0 {
            return Err(Error::invalid_argument(
                "member_threshold_bytes must be positive",
            ));
        }
        if self.member_threshold_bytes > self.pack_target_bytes {
            return Err(Error::invalid_argument(
                "member_threshold_bytes must not exceed pack_target_bytes; a member that cannot \
                 fit in a pack is not packable",
            ));
        }
        if self.max_members_per_pack == 0 {
            return Err(Error::invalid_argument(
                "max_members_per_pack must be positive",
            ));
        }
        if self.catalog_segments == 0
            || self.catalog_segments > 4096
            || !self.catalog_segments.is_power_of_two()
        {
            return Err(Error::invalid_argument(
                "catalog_segments must be a power of two in 1..=4096",
            ));
        }
        let minimum = self.catalog_segments as u64
            * (SEGMENT_HEADER_BYTES + DIGEST_BYTES + CATALOG_ENTRY_BYTES);
        if self.max_catalog_bytes < minimum {
            return Err(Error::invalid_argument(format!(
                "max_catalog_bytes must be at least {minimum} for {} catalog segments (room for \
                 one entry per segment)",
                self.catalog_segments
            )));
        }
        Ok(())
    }

    /// Maximum number of catalogued members.
    pub fn max_catalog_entries(&self) -> u64 {
        self.max_catalog_bytes / CATALOG_ENTRY_BYTES
    }
}

/// Result of reclaiming packs after a deletion pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PackReclaim {
    pub deleted_packs: u64,
    pub deleted_pack_bytes: u64,
    pub deleted_catalog_segments: u64,
    pub live_packs: u64,
    pub live_members: u64,
}

/// Pack-plane counters for receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PackStats {
    pub sealed_packs: u64,
    pub packed_members: u64,
    pub packed_bytes: u64,
    pub flat_members: u64,
    pub flat_bytes: u64,
    pub catalog_entries: u64,
    pub catalog_segment_publishes: u64,
    pub catalog_root_publishes: u64,
    pub open_pack_bytes: u64,
    pub open_pack_members: u64,
    pub packing_disabled: bool,
}

/// One catalogued member, encoded canonically in a catalog segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    /// 1 byte on the wire: 0 object, 1 manifest, 2 index.
    kind: u8,
    id: Hash32,
    pack: Hash32,
    offset: u64,
    length: u64,
    digest: Hash32,
}

impl Entry {
    fn sort_key(&self) -> (u8, Hash32) {
        (self.kind, self.id)
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.extend_from_slice(self.id.as_bytes());
        out.extend_from_slice(self.pack.as_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(self.digest.as_bytes());
    }

    fn decode(bytes: &[u8]) -> Result<Entry> {
        if bytes.len() != CATALOG_ENTRY_BYTES as usize {
            return Err(Error::corrupt("catalog entry has the wrong length"));
        }
        let mut cursor = 0;
        let kind = bytes[cursor];
        cursor += 1;
        let id = take_hash(bytes, &mut cursor)?;
        let pack = take_hash(bytes, &mut cursor)?;
        let offset = take_u64(bytes, &mut cursor)?;
        let length = take_u64(bytes, &mut cursor)?;
        let digest = take_hash(bytes, &mut cursor)?;
        if !matches!(kind, 0..=2) {
            return Err(Error::corrupt(format!(
                "catalog entry carries unknown member kind {kind}"
            )));
        }
        Ok(Entry {
            kind,
            id,
            pack,
            offset,
            length,
            digest,
        })
    }

    fn key(&self) -> Key {
        match self.kind {
            0 => Key::object(self.id),
            1 => Key::manifest(self.id),
            _ => Key::index(self.id),
        }
    }
}

/// One immutable catalog segment.
#[derive(Debug)]
struct Segment {
    index: u32,
    entries: Vec<Entry>,
}

impl Segment {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            (SEGMENT_HEADER_BYTES + self.entries.len() as u64 * CATALOG_ENTRY_BYTES + DIGEST_BYTES)
                as usize,
        );
        out.extend_from_slice(SEGMENT_MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for entry in &self.entries {
            entry.encode(&mut out);
        }
        let digest = identity::h(CATALOG_SEGMENT_DOMAIN, &out);
        out.extend_from_slice(digest.as_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Segment> {
        let minimum = (SEGMENT_HEADER_BYTES + DIGEST_BYTES) as usize;
        if bytes.len() < minimum {
            return Err(Error::corrupt("catalog segment is too short"));
        }
        if &bytes[..4] != SEGMENT_MAGIC {
            return Err(Error::corrupt("catalog segment magic mismatch"));
        }
        let version = u32::from_le_bytes(le4(bytes, 4)?);
        if version != FORMAT_VERSION {
            return Err(Error::unsupported_version(format!(
                "catalog segment version {version} is not supported"
            )));
        }
        let index = u32::from_le_bytes(le4(bytes, 8)?);
        let count = u32::from_le_bytes(le4(bytes, 12)?) as u64;
        let body_len = SEGMENT_HEADER_BYTES + count * CATALOG_ENTRY_BYTES;
        if body_len + DIGEST_BYTES != bytes.len() as u64 {
            return Err(Error::corrupt(
                "catalog segment length does not match its entry count",
            ));
        }
        let digest_at = body_len as usize;
        let expected = identity::h(CATALOG_SEGMENT_DOMAIN, &bytes[..digest_at]);
        if bytes[digest_at..] != *expected.as_bytes() {
            return Err(Error::corrupt("catalog segment digest mismatch"));
        }
        let mut entries = Vec::with_capacity(count as usize);
        let mut cursor = SEGMENT_HEADER_BYTES as usize;
        let mut previous: Option<(u8, Hash32)> = None;
        for _ in 0..count {
            let entry = Entry::decode(&bytes[cursor..cursor + CATALOG_ENTRY_BYTES as usize])?;
            let key = entry.sort_key();
            if previous.is_some_and(|previous| previous >= key) {
                return Err(Error::corrupt(
                    "catalog segment entries are not strictly ordered",
                ));
            }
            previous = Some(key);
            if entry.pack == Hash32::ZERO || entry.digest == Hash32::ZERO || entry.length == 0 {
                return Err(Error::corrupt(
                    "catalog entry has a zero pack, digest, or length",
                ));
            }
            entries.push(entry);
            cursor += CATALOG_ENTRY_BYTES as usize;
        }
        Ok(Segment { index, entries })
    }
}

/// Cached catalog state, bounded by `PackOptions::max_catalog_bytes` plus the
/// open pack.
#[derive(Default)]
struct CatalogState {
    loaded: bool,
    root_token: Option<String>,
    segment_ids: Vec<Hash32>,
    segments: Vec<Option<Arc<Segment>>>,
    total_entries: u64,
    open_bytes: Vec<u8>,
    open_entries: Vec<Entry>,
    open_index: BTreeMap<(u8, Hash32), usize>,
    /// When set, sealing publishes the pack object and *records* its members
    /// here instead of touching the catalog. One owner adopts them later.
    defer: bool,
    /// Default behaviour (N15): every seal records its members and the catalog
    /// is published once, by `flush`, before the commit record.
    auto_defer: bool,
    pending: Vec<Entry>,
    /// Key -> position in `pending`, maintained with it, so a member sealed but
    /// not yet catalogued is still readable by this handle.
    pending_index: BTreeMap<(u8, Hash32), usize>,
    /// Digest per deferred key, so a second publication of the same key inside
    /// one handle is idempotent and a different payload is still refused.
    pending_digests: BTreeMap<(u8, Hash32), Hash32>,
    /// Member kinds this handle is holding that no other handle can resolve:
    /// open-pack members plus deferred members. Maintained with the two lists
    /// above, never recomputed by scanning them.
    buffered_kinds: u8,
    /// Keys this plane has *observed* absent in the flat namespace, bounded.
    /// The store reads a blob before publishing it; without this the plane
    /// would ask the inner namespace the same question twice per put.
    absent: BTreeSet<Key>,
    absent_order: std::collections::VecDeque<Key>,
    counters: PackStats,
    notes: Vec<String>,
}

/// Members of packs a plane published **without** publishing catalog metadata.
///
/// A blob worker of the prepare/commit checkpoint writer packs its small blobs
/// and publishes the immutable pack objects immediately (content-addressed, so
/// safe to publish concurrently), then hands this list to the single committer.
/// The committer merges every worker's members into one catalog publication, so
/// no worker ever writes a control record.
#[derive(Debug, Default)]
pub struct DeferredPackEntries {
    entries: Vec<Entry>,
}

impl DeferredPackEntries {
    /// Number of deferred members.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing was deferred.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Bound on the negative cache. A single-writer store holds an exclusive lock,
/// so a key observed absent cannot appear underneath it; the cache is an
/// optimisation, never a correctness assumption.
const MAX_ABSENT_KEYS: usize = 4096;

/// A [`BlobPlane`] that groups small blobs into physical pack objects.
pub struct PackedBlobPlane {
    inner: Box<dyn BlobPlane + Send>,
    catalog: Box<dyn CommitPlane + Send>,
    options: PackOptions,
    state: Mutex<CatalogState>,
}

impl PackedBlobPlane {
    /// Wrap `inner`. Refuses a plane without true range reads and an invalid
    /// policy before any byte is written.
    pub fn new(
        inner: Box<dyn BlobPlane + Send>,
        catalog: Box<dyn CommitPlane + Send>,
        options: PackOptions,
    ) -> Result<PackedBlobPlane> {
        options.validate()?;
        let capabilities = inner.blob_capabilities();
        if !capabilities.true_range_reads {
            return Err(Error::unsupported_version(format!(
                "packed blobs need true range reads from the inner blob plane `{}`; refusing to \
                 pack because a small member would otherwise read the whole pack",
                capabilities.backend
            )));
        }
        if options.pack_target_bytes > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(format!(
                "pack_target_bytes ({}) is above the inner plane's {}-byte object bound",
                options.pack_target_bytes, capabilities.max_object_bytes
            )));
        }
        if options.max_catalog_bytes > capabilities.max_object_bytes {
            return Err(Error::limit_exceeded(
                "max_catalog_bytes is above the inner plane's object bound",
            ));
        }
        Ok(PackedBlobPlane {
            inner,
            catalog,
            options,
            state: Mutex::new(CatalogState {
                auto_defer: true,
                ..CatalogState::default()
            }),
        })
    }

    /// The active policy.
    pub fn options(&self) -> PackOptions {
        self.options
    }

    /// Packing counters from this handle's own activity (sealed packs, packed
    /// and flat members, catalog size). Counters are per-process; use
    /// [`PackedBlobPlane::catalog_stats`] for the on-disk truth.
    pub fn pack_stats(&self) -> PackStats {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stats = state.counters.clone();
        stats.catalog_entries =
            state.total_entries + state.open_entries.len() as u64 + state.pending.len() as u64;
        stats.open_pack_bytes = state.open_bytes.len() as u64;
        stats.open_pack_members = state.open_entries.len() as u64;
        stats.packing_disabled = state
            .notes
            .iter()
            .any(|note| note.starts_with("catalog bound reached"));
        stats
    }

    /// Read the catalog and report what it actually names: sealed packs,
    /// members, and member bytes. Reads only; never writes. `flat_members` and
    /// `flat_bytes` cannot be derived from the catalog and are left at zero.
    pub fn catalog_stats(&mut self) -> Result<PackStats> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        for index in 0..state.segment_ids.len() {
            load_segment(&**inner, options, &mut state, index)?;
        }
        let mut packs: BTreeSet<Hash32> = BTreeSet::new();
        let mut entries = 0u64;
        let mut bytes = 0u64;
        for segment in state.segments.iter().filter_map(|segment| segment.as_ref()) {
            for entry in &segment.entries {
                packs.insert(entry.pack);
                entries += 1;
                bytes += entry.length;
            }
        }
        let mut stats = state.counters.clone();
        stats.sealed_packs = packs.len() as u64;
        stats.packed_members = entries;
        stats.packed_bytes = bytes;
        stats.catalog_entries = entries;
        stats.open_pack_bytes = state.open_bytes.len() as u64;
        stats.open_pack_members = state.open_entries.len() as u64;
        Ok(stats)
    }
}

impl BlobPlane for PackedBlobPlane {
    fn blob_label(&self) -> &'static str {
        "packed"
    }

    fn blob_capabilities(&self) -> BlobCapabilities {
        let mut capabilities = self.inner.blob_capabilities();
        capabilities.backend = format!("packed-{}", capabilities.backend);
        capabilities.publication = format!(
            "small blobs grouped into content-addressed packs published with `{}`; the catalog \
             root is replaced with compare-and-swap",
            capabilities.publication
        );
        capabilities
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        match read_packed(&**inner, options, &mut state, key)? {
            Some(bytes) => Ok(bytes),
            None => match inner.get(key) {
                Ok(bytes) => Ok(bytes),
                Err(err) if err.class() == Class::NotFound => {
                    remember_absent(&mut state, key);
                    Err(err)
                }
                Err(err) => Err(err),
            },
        }
    }

    fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        let Some((entry, slot)) = find(&**inner, options, &mut state, key)? else {
            return match inner.get_range(key, offset, len) {
                Ok(bytes) => Ok(bytes),
                Err(err) if err.class() == Class::NotFound => {
                    remember_absent(&mut state, key);
                    Err(err)
                }
                Err(err) => Err(err),
            };
        };
        if offset > entry.length {
            return Err(Error::invalid_argument(format!(
                "read offset {offset} is beyond {} ({} bytes)",
                key.display_path(),
                entry.length
            )));
        }
        let want = (entry.length - offset).min(len);
        if want == 0 {
            return Ok(Vec::new());
        }
        if matches!(slot, Slot::Open) {
            let start = usize::try_from(entry.offset + offset)
                .map_err(|_| Error::limit_exceeded("open pack offset does not fit memory"))?;
            let end = start
                .checked_add(want as usize)
                .ok_or_else(|| Error::limit_exceeded("open pack range overflows memory"))?;
            return state
                .open_bytes
                .get(start..end)
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| Error::corrupt("open pack range is out of bounds"));
        }
        // One ranged read for exactly the member's bytes. The member digest is
        // a whole-member check, exactly as the flat plane only verifies on a
        // whole-object read.
        let fetched = inner.get_range(&Key::pack(entry.pack), entry.offset + offset, want)?;
        if fetched.len() as u64 != want {
            return Err(Error::corrupt(format!(
                "pack {} returned {} bytes for a {}-byte member range",
                entry.pack.to_hex(),
                fetched.len(),
                want
            )));
        }
        Ok(fetched)
    }

    fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        // The blob may already exist in a pack, in the open pack, or flat.
        if let Some((entry, slot)) = find(&**inner, options, &mut state, key)? {
            let existing = if matches!(slot, Slot::Open) {
                let start = entry.offset as usize;
                let end = start + entry.length as usize;
                state
                    .open_bytes
                    .get(start..end)
                    .ok_or_else(|| Error::corrupt("open pack range is out of bounds"))?
                    .to_vec()
            } else {
                let fetched =
                    inner.get_range(&Key::pack(entry.pack), entry.offset, entry.length)?;
                if fetched.len() as u64 != entry.length {
                    return Err(Error::corrupt("existing pack member is truncated"));
                }
                verify_member_digest(&entry, &fetched)?;
                fetched
            };
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::exists(format!(
                    "key {} is already packed with different bytes",
                    key.display_path()
                )))
            };
        }
        // A read immediately before this publication already observed the key
        // absent (the store verifies a blob before publishing it). Skip the
        // duplicate request for the same key in this process.
        if !state.absent.contains(key) {
            match inner.get(key) {
                Ok(existing) => {
                    return if existing == bytes {
                        Ok(())
                    } else {
                        Err(Error::exists(format!(
                            "key {} already exists with different bytes",
                            key.display_path()
                        )))
                    };
                }
                Err(err) if err.class() == Class::NotFound => {
                    remember_absent(&mut state, key);
                }
                Err(err) => return Err(err),
            }
        }
        let Some((kind, id)) = member_identity(key) else {
            return inner.put_if_absent(key, bytes);
        };
        if state.defer {
            // In deferred mode a member this handle already packed is not
            // readable from the catalog yet, so idempotency is decided here.
            if let Some(recorded) = state.pending_digests.get(&(kind, id)).copied() {
                return if recorded == identity::h(MEMBER_DOMAIN, bytes) {
                    Ok(())
                } else {
                    Err(Error::exists(format!(
                        "key {} is already packed with different bytes",
                        key.display_path()
                    )))
                };
            }
        }
        let length = bytes.len() as u64;
        let max_entries = options.max_catalog_entries();
        let pending = state.total_entries + state.open_entries.len() as u64;
        if length == 0 || length > options.member_threshold_bytes || pending >= max_entries {
            if pending >= max_entries && length > 0 && length <= options.member_threshold_bytes {
                note(
                    &mut state,
                    "catalog bound reached; new blobs are published flat".to_owned(),
                );
            }
            state.counters.flat_members += 1;
            state.counters.flat_bytes += length;
            forget_absent(&mut state, key);
            return inner.put_if_absent(key, bytes);
        }
        append(inner, catalog, options, &mut state, kind, id, bytes)?;
        forget_absent(&mut state, key);
        Ok(())
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        self.delete_many(std::slice::from_ref(key))
    }

    fn len(&self, key: &Key) -> Result<u64> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        match find(&**inner, options, &mut state, key)? {
            Some((entry, _)) => Ok(entry.length),
            None => match inner.len(key) {
                Ok(len) => Ok(len),
                Err(err) if err.class() == Class::NotFound => {
                    remember_absent(&mut state, key);
                    Err(err)
                }
                Err(err) => Err(err),
            },
        }
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        match find(&**inner, options, &mut state, key)? {
            Some(_) => Ok(true),
            None => match inner.exists(key) {
                Ok(true) => Ok(true),
                Ok(false) => {
                    remember_absent(&mut state, key);
                    Ok(false)
                }
                Err(err) => Err(err),
            },
        }
    }

    fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list requires a Key::Prefix selector",
            ));
        };
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut out = inner.list(prefix)?;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        for index in 0..state.segment_ids.len() {
            load_segment(&**inner, options, &mut state, index)?;
        }
        for entry in state
            .segments
            .iter()
            .filter_map(|segment| segment.as_ref())
            .flat_map(|segment| segment.entries.iter())
            .chain(state.open_entries.iter())
        {
            let key = entry.key();
            if key.kind() == *kind {
                out.push(key);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn clean_stale(&mut self, grace_secs: u64) -> Result<()> {
        self.inner.clean_stale(grace_secs)
    }

    fn init_layout(&mut self) -> Result<()> {
        self.inner.init_layout()
    }

    fn ensure_layout(&mut self) -> Result<()> {
        self.inner.ensure_layout()
    }

    fn flush(&mut self) -> Result<()> {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::BlobPublish);
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        seal(inner, catalog, options, &mut state)?;
        if state.auto_defer && !state.defer && !state.pending.is_empty() {
            adopt_pending(inner, catalog, options, &mut state)?;
        }
        Ok(())
    }

    fn buffered_blob_kinds(&self) -> u8 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Open-pack members are readable only through this handle, and deferred
        // members are readable by nobody until the catalog owner adopts them.
        state.buffered_kinds
    }

    fn pack_counters(&self) -> Option<PackStats> {
        Some(self.pack_stats())
    }

    fn defer_pack_catalog(&mut self) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.defer = true;
        Ok(true)
    }

    fn take_pending_pack_members(&mut self) -> Result<Option<DeferredPackEntries>> {
        let Self {
            inner,
            options,
            state,
            ..
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.defer {
            return Ok(None);
        }
        // Seal the open pack so every member this handle published is in a
        // published pack object before the catalog can name it.
        seal_deferred(inner, options, &mut state)?;
        let entries = std::mem::take(&mut state.pending);
        state.pending_digests.clear();
        state.pending_index.clear();
        state.buffered_kinds = 0;
        Ok(Some(DeferredPackEntries { entries }))
    }

    fn adopt_pending_pack_members(&mut self, members: DeferredPackEntries) -> Result<()> {
        if members.entries.is_empty() {
            return Ok(());
        }
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        commit_entries(inner, catalog, options, &mut state, members.entries)
    }

    fn refresh_metadata(&mut self) -> Result<()> {
        // Only drop the cached *catalog root and segments*. Blobs buffered in
        // the open pack are uncompressed local state and are kept: a refresh
        // must never discard publications that have not been sealed yet.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.loaded = false;
        state.root_token = None;
        state.segment_ids.clear();
        state.segments.clear();
        state.absent.clear();
        state.absent_order.clear();
        Ok(())
    }

    fn stats(&self) -> BackendStats {
        self.inner.stats().merged(&self.catalog.stats())
    }

    fn diagnostics(&self) -> Vec<String> {
        let mut out = self.inner.diagnostics();
        let stats = self.pack_stats();
        out.push(format!(
            "packed blobs: {} packs, {} packed members ({} bytes), {} flat members ({} bytes), \
             catalog {} entries over {} segments, open pack {} bytes / {} members",
            stats.sealed_packs,
            stats.packed_members,
            stats.packed_bytes,
            stats.flat_members,
            stats.flat_bytes,
            stats.catalog_entries,
            self.options.catalog_segments,
            stats.open_pack_bytes,
            stats.open_pack_members
        ));
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for note in &state.notes {
            out.push(format!("packed blobs: {note}"));
        }
        drop(state);
        out.extend(
            self.catalog
                .diagnostics()
                .into_iter()
                .map(|line| format!("pack catalog: {line}")),
        );
        out
    }

    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn reclaim_unreferenced_packs(&mut self) -> Result<PackReclaim> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        for index in 0..state.segment_ids.len() {
            load_segment(&**inner, options, &mut state, index)?;
        }
        let mut referenced: BTreeSet<Hash32> = BTreeSet::new();
        let mut live_members = 0u64;
        for segment in state.segments.iter().filter_map(|segment| segment.as_ref()) {
            for entry in &segment.entries {
                referenced.insert(entry.pack);
                live_members += 1;
            }
        }
        let mut report = PackReclaim {
            live_packs: referenced.len() as u64,
            live_members,
            ..PackReclaim::default()
        };
        for key in inner.list(&Key::prefix(KeyKind::Pack))? {
            let Key::Pack { hash } = key else {
                continue;
            };
            if referenced.contains(&hash) {
                continue;
            }
            let bytes = inner.len(&key).unwrap_or(0);
            inner.delete(&key)?;
            report.deleted_packs += 1;
            report.deleted_pack_bytes += bytes;
        }
        // Catalog segments no longer named by the root (a crash between
        // segment and root publication leaves these).
        let live_segments: BTreeSet<Hash32> = state
            .segment_ids
            .iter()
            .copied()
            .filter(|id| *id != Hash32::ZERO)
            .collect();
        for key in inner.list(&Key::prefix(KeyKind::PackCatalog))? {
            let Key::PackCatalog { hash } = key else {
                continue;
            };
            if !live_segments.contains(&hash) {
                inner.delete(&key)?;
                report.deleted_catalog_segments += 1;
            }
        }
        Ok(report)
    }

    fn verify_packs(&self) -> Result<Vec<String>> {
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut problems = Vec::new();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(err) = ensure_loaded(&**catalog, options, &mut state) {
            return Ok(vec![format!("pack catalog root: {err}")]);
        }
        let mut entries = 0u64;
        let mut ranges: BTreeMap<Hash32, Vec<(u64, u64)>> = BTreeMap::new();
        for index in 0..state.segment_ids.len() {
            let segment = match load_segment(&**inner, options, &mut state, index) {
                Ok(segment) => segment,
                Err(err) => {
                    problems.push(format!("pack catalog segment {index}: {err}"));
                    continue;
                }
            };
            entries += segment.entries.len() as u64;
            for entry in &segment.entries {
                ranges
                    .entry(entry.pack)
                    .or_default()
                    .push((entry.offset, entry.length));
            }
        }
        if entries != state.total_entries {
            problems.push(format!(
                "pack catalog root declares {} entries but its segments hold {entries}",
                state.total_entries
            ));
        }
        for (pack, mut spans) in ranges {
            let key = Key::pack(pack);
            match inner.len(&key) {
                Ok(len) => {
                    spans.sort_unstable();
                    let mut cursor = 0u64;
                    for (offset, length) in spans {
                        if offset < cursor {
                            problems.push(format!(
                                "pack {} has overlapping catalog members at offset {offset}",
                                pack.to_hex()
                            ));
                        }
                        let end = offset.saturating_add(length);
                        if end > len {
                            problems.push(format!(
                                "pack {} is {len} bytes but a member ends at {end}",
                                pack.to_hex()
                            ));
                        }
                        cursor = end;
                    }
                }
                Err(err) if err.class() == Class::NotFound => {
                    problems.push(format!(
                        "pack {} named by the catalog is missing",
                        pack.to_hex()
                    ));
                }
                Err(err) => problems.push(format!("pack {}: {err}", pack.to_hex())),
            }
        }
        Ok(problems)
    }

    fn delete_many(&mut self, keys: &[Key]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let Self {
            inner,
            catalog,
            options,
            state,
        } = self;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_loaded(&**catalog, options, &mut state)?;
        let mut removed_open: BTreeSet<(u8, Hash32)> = BTreeSet::new();
        let mut removed_pending: BTreeSet<(u8, Hash32)> = BTreeSet::new();
        let mut touched: BTreeMap<usize, BTreeSet<(u8, Hash32)>> = BTreeMap::new();
        let mut flat: Vec<&Key> = Vec::new();
        for key in keys {
            match find(&**inner, options, &mut state, key)? {
                Some((entry, Slot::Open)) => {
                    removed_open.insert(entry.sort_key());
                }
                Some((entry, Slot::Pending)) => {
                    // Never catalogued, so nothing names it: dropping the record
                    // is enough and the pack's bytes become garbage.
                    removed_pending.insert(entry.sort_key());
                }
                Some((entry, Slot::Catalog)) => {
                    let index = segment_index(entry.id, options.catalog_segments);
                    touched.entry(index).or_default().insert(entry.sort_key());
                }
                None => flat.push(key),
            }
        }
        for key in flat {
            inner.delete(key)?;
        }
        if !removed_open.is_empty() {
            rebuild_open(&mut state, &removed_open);
        }
        if !removed_pending.is_empty() {
            rebuild_pending(&mut state, &removed_pending);
        }
        if touched.is_empty() {
            return Ok(());
        }
        // One republish per touched segment, then one root compare-and-swap.
        let mut new_ids = state.segment_ids.clone();
        let mut new_total = state.total_entries;
        let mut pending: Vec<(usize, Arc<Segment>, Vec<u8>)> = Vec::new();
        for (index, drop_keys) in &touched {
            let current = load_segment(&**inner, options, &mut state, *index)?;
            let mut kept: Vec<Entry> = Vec::with_capacity(current.entries.len());
            for entry in &current.entries {
                if !drop_keys.contains(&entry.sort_key()) {
                    kept.push(*entry);
                }
            }
            new_total = new_total.saturating_sub(current.entries.len() as u64) + kept.len() as u64;
            let segment = Arc::new(Segment {
                index: *index as u32,
                entries: kept,
            });
            let bytes = segment.encode();
            new_ids[*index] = fragment_id(&bytes);
            pending.push((*index, segment, bytes));
        }
        for (_, _, bytes) in &pending {
            let id = fragment_id(bytes);
            inner.put_if_absent(&Key::pack_catalog(id), bytes)?;
            state.counters.catalog_segment_publishes += 1;
        }
        crash_point("pack_after_catalog_segments");
        let root = encode_root(&new_ids, new_total)?;
        let token = state.root_token.clone();
        let written = commit_root(&mut **catalog, &root, token.as_deref())?;
        state.root_token = Some(written);
        state.segment_ids = new_ids;
        state.total_entries = new_total;
        for (index, segment, _) in pending {
            state.segments[index] = Some(segment);
        }
        state.counters.catalog_root_publishes += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Catalog helpers (free functions so one method can hold the state lock and
// still borrow the planes mutably)
// ---------------------------------------------------------------------------

fn ensure_loaded(
    catalog: &dyn CommitPlane,
    options: &PackOptions,
    state: &mut CatalogState,
) -> Result<()> {
    if state.loaded {
        return Ok(());
    }
    let record = catalog.read_control(&Key::pack_catalog_root())?;
    match record {
        None => {
            state.segment_ids = vec![Hash32::ZERO; options.catalog_segments as usize];
            state.segments = vec![None; options.catalog_segments as usize];
            state.total_entries = 0;
            state.root_token = None;
        }
        Some(record) => {
            let (ids, total) = decode_root(&record.bytes, options.catalog_segments)?;
            state.segment_ids = ids;
            state.segments = vec![None; options.catalog_segments as usize];
            state.total_entries = total;
            state.root_token = record.token;
        }
    }
    state.loaded = true;
    Ok(())
}

fn load_segment(
    inner: &dyn BlobPlane,
    _options: &PackOptions,
    state: &mut CatalogState,
    index: usize,
) -> Result<Arc<Segment>> {
    if let Some(segment) = &state.segments[index] {
        return Ok(Arc::clone(segment));
    }
    let id = *state
        .segment_ids
        .get(index)
        .ok_or_else(|| Error::corrupt("catalog segment index is out of bounds"))?;
    if id == Hash32::ZERO {
        let segment = Arc::new(Segment {
            index: index as u32,
            entries: Vec::new(),
        });
        state.segments[index] = Some(Arc::clone(&segment));
        return Ok(segment);
    }
    let bytes = inner.get(&Key::pack_catalog(id))?;
    let segment = Segment::decode(&bytes)?;
    if segment.index != index as u32 {
        return Err(Error::corrupt(format!(
            "catalog segment {index} decodes as segment {}",
            segment.index
        )));
    }
    let segment = Arc::new(segment);
    state.segments[index] = Some(Arc::clone(&segment));
    Ok(segment)
}

/// Where a catalogued member's bytes live right now.
enum Slot {
    /// Buffered in this handle's open, unpublished pack.
    Open,
    /// In a pack object this handle already published, whose catalog record has
    /// not been published yet (N15 auto-deferral between seal and flush).
    Pending,
    /// In a pack named by a published catalog segment. The segment itself is
    /// cached in `state.segments`, so the slot carries no reference.
    Catalog,
}

fn find(
    inner: &dyn BlobPlane,
    options: &PackOptions,
    state: &mut CatalogState,
    key: &Key,
) -> Result<Option<(Entry, Slot)>> {
    let Some((kind, id)) = member_identity(key) else {
        return Ok(None);
    };
    if let Some(index) = state.open_index.get(&(kind, id)) {
        let entry = *state
            .open_entries
            .get(*index)
            .ok_or_else(|| Error::corrupt("open pack index is inconsistent"))?;
        return Ok(Some((entry, Slot::Open)));
    }
    if let Some(index) = state.pending_index.get(&(kind, id)) {
        let entry = *state
            .pending
            .get(*index)
            .ok_or_else(|| Error::corrupt("pending pack index is inconsistent"))?;
        return Ok(Some((entry, Slot::Pending)));
    }
    let index = segment_index(id, options.catalog_segments);
    let segment = load_segment(inner, options, state, index)?;
    let wanted = (kind, id);
    match segment
        .entries
        .binary_search_by(|entry| entry.sort_key().cmp(&wanted))
    {
        Ok(position) => Ok(Some((segment.entries[position], Slot::Catalog))),
        Err(_) => Ok(None),
    }
}

fn read_packed(
    inner: &dyn BlobPlane,
    options: &PackOptions,
    state: &mut CatalogState,
    key: &Key,
) -> Result<Option<Vec<u8>>> {
    let Some((entry, slot)) = find(inner, options, state, key)? else {
        return Ok(None);
    };
    let bytes = if matches!(slot, Slot::Open) {
        let start = usize::try_from(entry.offset)
            .map_err(|_| Error::limit_exceeded("open pack offset does not fit memory"))?;
        let end = start
            .checked_add(entry.length as usize)
            .ok_or_else(|| Error::limit_exceeded("open pack range overflows memory"))?;
        state
            .open_bytes
            .get(start..end)
            .ok_or_else(|| Error::corrupt("open pack range is out of bounds"))?
            .to_vec()
    } else {
        let fetched = inner.get_range(&Key::pack(entry.pack), entry.offset, entry.length)?;
        if fetched.len() as u64 != entry.length {
            return Err(Error::corrupt(format!(
                "pack {} returned {} bytes for member {} at offset {} (expected {})",
                entry.pack.to_hex(),
                fetched.len(),
                key.display_path(),
                entry.offset,
                entry.length
            )));
        }
        fetched
    };
    verify_member_digest(&entry, &bytes)?;
    Ok(Some(bytes))
}

#[allow(clippy::too_many_arguments)]
fn append(
    inner: &mut Box<dyn BlobPlane + Send>,
    catalog: &mut Box<dyn CommitPlane + Send>,
    options: &PackOptions,
    state: &mut CatalogState,
    kind: u8,
    id: Hash32,
    bytes: &[u8],
) -> Result<()> {
    let length = bytes.len() as u64;
    let would_overflow = |state: &CatalogState| {
        state.open_bytes.len() as u64 + length > options.pack_target_bytes
            || state.open_entries.len() >= options.max_members_per_pack as usize
    };
    if !state.open_entries.is_empty() && would_overflow(state) {
        seal(inner, catalog, options, state)?;
    }
    if !state.open_entries.is_empty() && would_overflow(state) {
        // The threshold is at most the pack target, so a single member always
        // fits an empty pack. Fail closed rather than publish an oversized pack.
        return Err(Error::limit_exceeded(format!(
            "member of {length} bytes does not fit the open pack target {}",
            options.pack_target_bytes
        )));
    }
    let offset = state.open_bytes.len() as u64;
    state.open_bytes.extend_from_slice(bytes);
    let entry = Entry {
        kind,
        id,
        pack: Hash32::ZERO,
        offset,
        length,
        digest: identity::h(MEMBER_DOMAIN, bytes),
    };
    let position = state.open_entries.len();
    state.open_entries.push(entry);
    state.open_index.insert((kind, id), position);
    state.buffered_kinds |= buffered_bit(kind);
    state.counters.packed_members += 1;
    state.counters.packed_bytes += length;
    Ok(())
}

/// Publish the open pack, the touched segments, and the root.
fn seal(
    inner: &mut Box<dyn BlobPlane + Send>,
    catalog: &mut Box<dyn CommitPlane + Send>,
    options: &PackOptions,
    state: &mut CatalogState,
) -> Result<()> {
    if state.open_entries.is_empty() {
        return Ok(());
    }
    if state.defer || state.auto_defer {
        return seal_deferred(inner, options, state);
    }
    let pack_bytes = std::mem::take(&mut state.open_bytes);
    let mut entries = std::mem::take(&mut state.open_entries);
    let index = std::mem::take(&mut state.open_index);
    let outcome = seal_inner(inner, catalog, options, state, &pack_bytes, &mut entries);
    match outcome {
        Ok(()) => {
            // Nothing is buffered any more: every member is catalogued and any
            // pooled read handle can resolve it.
            state.buffered_kinds = 0;
            Ok(())
        }
        Err(err) => {
            // Restore the buffered pack so a retry re-publishes the same pack
            // object idempotently instead of losing members.
            state.open_bytes = pack_bytes;
            state.open_entries = entries;
            state.open_index = index;
            Err(err)
        }
    }
}

/// Seal the open pack in deferred mode: publish the immutable pack object and
/// record its members for the one catalog owner. No control record is touched.
fn seal_deferred(
    inner: &mut Box<dyn BlobPlane + Send>,
    _options: &PackOptions,
    state: &mut CatalogState,
) -> Result<()> {
    if state.open_entries.is_empty() {
        return Ok(());
    }
    let pack_bytes = std::mem::take(&mut state.open_bytes);
    let mut entries = std::mem::take(&mut state.open_entries);
    state.open_index.clear();
    let pack_id = identity::h(PACK_DOMAIN, &pack_bytes);
    for entry in entries.iter_mut() {
        entry.pack = pack_id;
    }
    // Same ordering as the catalogued path: the pack is durable before any
    // catalog record that names it can exist, and here it is published only
    // after every member's bytes are already in it.
    inner.put_if_absent(&Key::pack(pack_id), &pack_bytes)?;
    crash_point("pack_after_pack_object");
    for entry in entries {
        state.pending_digests.insert(entry.sort_key(), entry.digest);
        state
            .pending_index
            .insert(entry.sort_key(), state.pending.len());
        state.pending.push(entry);
    }
    state.counters.sealed_packs += 1;
    Ok(())
}

/// Seals one pack. Segment publication and the root compare-and-swap are
/// retried when another handle advanced the catalog first: two blob workers of
/// the prepare/commit checkpoint writer can seal concurrently, and the loser of
/// a CAS must re-merge against the winner's segments rather than fail the
/// checkpoint. Every published object is content-addressed, so a retry
/// republishes the same bytes idempotently.
const MAX_SEAL_ATTEMPTS: usize = 16;

fn seal_inner(
    inner: &mut Box<dyn BlobPlane + Send>,
    catalog: &mut Box<dyn CommitPlane + Send>,
    options: &PackOptions,
    state: &mut CatalogState,
    pack_bytes: &[u8],
    entries: &mut [Entry],
) -> Result<()> {
    let pack_id = identity::h(PACK_DOMAIN, pack_bytes);
    for entry in entries.iter_mut() {
        entry.pack = pack_id;
    }
    // 1. Publish the pack first. A crash after this leaves an unreferenced
    //    pack, never a catalog entry pointing at missing bytes.
    inner.put_if_absent(&Key::pack(pack_id), pack_bytes)?;
    crash_point("pack_after_pack_object");
    commit_entries(inner, catalog, options, state, entries.to_vec())?;
    state.counters.sealed_packs += 1;
    Ok(())
}

/// Merge `entries` into the catalog: publish the touched segments, then
/// advance the root with one compare-and-swap.
///
/// This is the only place that writes pack catalog metadata, so the deferred
/// path (`take_pending_pack_members` → `adopt_pending_pack_members`) and the
/// immediate path cannot drift.
fn commit_entries(
    inner: &mut Box<dyn BlobPlane + Send>,
    catalog: &mut Box<dyn CommitPlane + Send>,
    options: &PackOptions,
    state: &mut CatalogState,
    entries: Vec<Entry>,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        // 2. Publish each touched catalog segment as an immutable object.
        let mut per_segment: BTreeMap<usize, Vec<Entry>> = BTreeMap::new();
        for entry in entries.iter() {
            per_segment
                .entry(segment_index(entry.id, options.catalog_segments))
                .or_default()
                .push(*entry);
        }
        let mut new_ids = state.segment_ids.clone();
        let mut pending: Vec<(usize, Arc<Segment>, Vec<u8>)> = Vec::new();
        let mut fresh_entries = 0u64;
        for (segment_index_value, mut added) in per_segment {
            let current = load_segment(&**inner, options, state, segment_index_value)?;
            // Merge two sorted lists. A key this pack holds that the catalog
            // already names is **not** an error: content addressing means two
            // workers can pack the same chunk concurrently, and identical
            // bytes are exactly what the key promises. Reject only a key whose
            // recorded length or digest disagrees (a collision or a bug).
            added.sort_by_key(Entry::sort_key);
            let existing = &current.entries;
            let mut merged = Vec::with_capacity(existing.len() + added.len());
            let mut fresh = 0u64;
            let mut cursor = 0usize;
            for entry in added {
                while cursor < existing.len() && existing[cursor].sort_key() < entry.sort_key() {
                    merged.push(existing[cursor]);
                    cursor += 1;
                }
                if cursor < existing.len() && existing[cursor].sort_key() == entry.sort_key() {
                    let recorded = existing[cursor];
                    if recorded.length != entry.length || recorded.digest != entry.digest {
                        return Err(Error::corrupt(format!(
                            "catalog already names {} with different bytes than this pack holds",
                            entry.key().display_path()
                        )));
                    }
                    // Keep the catalogued copy: an identical member is still a
                    // member, and dropping it here would silently un-name it.
                    merged.push(recorded);
                    cursor += 1;
                    continue;
                }
                merged.push(entry);
                fresh += 1;
            }
            merged.extend_from_slice(&existing[cursor..]);
            if fresh == 0 {
                // Every member of this segment was already catalogued.
                continue;
            }
            fresh_entries += fresh;
            let segment = Arc::new(Segment {
                index: segment_index_value as u32,
                entries: merged,
            });
            let bytes = segment.encode();
            new_ids[segment_index_value] = fragment_id(&bytes);
            pending.push((segment_index_value, segment, bytes));
        }
        if fresh_entries == 0 {
            // Another handle catalogued every member of this pack first. The
            // pack object is unreferenced garbage and no root change is needed.
            note(
                state,
                "pack was already named by the catalog; nothing to publish".to_owned(),
            );
            return Ok(());
        }
        for (_, _, bytes) in &pending {
            let id = fragment_id(bytes);
            inner.put_if_absent(&Key::pack_catalog(id), bytes)?;
            state.counters.catalog_segment_publishes += 1;
        }
        crash_point("pack_after_catalog_segments");

        // 3. Publish the root with one compare-and-swap.
        let new_total = state.total_entries + fresh_entries;
        let root = encode_root(&new_ids, new_total)?;
        let token = state.root_token.clone();
        match commit_root(&mut **catalog, &root, token.as_deref()) {
            Ok(written) => {
                state.root_token = Some(written);
                state.segment_ids = new_ids;
                state.total_entries = new_total;
                for (index, segment, _) in pending {
                    state.segments[index] = Some(segment);
                }
                state.counters.catalog_root_publishes += 1;
                return Ok(());
            }
            Err(err) if err.class() == Class::Busy && attempt < MAX_SEAL_ATTEMPTS => {
                attempt += 1;
                // Re-read the catalog root and every segment this seal touches;
                // the winner's entries are now part of the merge base.
                state.loaded = false;
                state.root_token = None;
                state.segment_ids.clear();
                state.segments.clear();
                ensure_loaded(&**catalog, options, state)?;
                note(
                    state,
                    format!("pack catalog compare-and-swap retry {attempt} after concurrent seal"),
                );
            }
            Err(err) => return Err(err),
        }
    }
}

/// Publish every sealed-but-uncatalogued member in **one** catalog write.
///
/// This is the N15 change: the sequential write path seals packs as they fill
/// and used to republish every touched segment per seal, so a large save wrote
/// the whole catalog hundreds of times (the 7B save measured 148,694 segment
/// publications and +51.16 % bytes). The invariants are unchanged — the pack
/// object is durable before any catalog record names it, and the root is still
/// one compare-and-swap — but the catalog is written once per flush, and the
/// store flushes before every commit record.
fn adopt_pending(
    inner: &mut Box<dyn BlobPlane + Send>,
    catalog: &mut Box<dyn CommitPlane + Send>,
    options: &PackOptions,
    state: &mut CatalogState,
) -> Result<()> {
    let entries = std::mem::take(&mut state.pending);
    state.pending_digests.clear();
    state.pending_index.clear();
    state.buffered_kinds = 0;
    commit_entries(inner, catalog, options, state, entries)
}

fn rebuild_pending(state: &mut CatalogState, removed: &BTreeSet<(u8, Hash32)>) {
    let entries = std::mem::take(&mut state.pending);
    state.pending_index.clear();
    for entry in entries {
        if removed.contains(&entry.sort_key()) {
            state.pending_digests.remove(&entry.sort_key());
            continue;
        }
        state
            .pending_index
            .insert(entry.sort_key(), state.pending.len());
        state.pending.push(entry);
    }
}

/// Rebuild the open pack after members were removed from it, so buffered
/// offsets stay exact. Nothing has been published yet.
fn rebuild_open(state: &mut CatalogState, removed: &BTreeSet<(u8, Hash32)>) {
    let entries = std::mem::take(&mut state.open_entries);
    let mut bytes = Vec::with_capacity(state.open_bytes.len());
    let mut kept = Vec::with_capacity(entries.len());
    let mut index = BTreeMap::new();
    for mut entry in entries {
        if removed.contains(&entry.sort_key()) {
            continue;
        }
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        entry.offset = bytes.len() as u64;
        bytes.extend_from_slice(&state.open_bytes[start..end]);
        index.insert(entry.sort_key(), kept.len());
        kept.push(entry);
    }
    state.open_bytes = bytes;
    state.open_entries = kept;
    state.open_index = index;
}

fn note(state: &mut CatalogState, text: String) {
    if state.notes.len() < 16 && !state.notes.contains(&text) {
        state.notes.push(text);
    }
}

/// Record that a read observed `key` absent in the flat namespace.
fn remember_absent(state: &mut CatalogState, key: &Key) {
    if state.absent.insert(key.clone()) {
        state.absent_order.push_back(key.clone());
        while state.absent_order.len() > MAX_ABSENT_KEYS {
            if let Some(old) = state.absent_order.pop_front() {
                state.absent.remove(&old);
            }
        }
    }
}

fn forget_absent(state: &mut CatalogState, key: &Key) {
    if state.absent.remove(key) {
        state.absent_order.retain(|known| known != key);
    }
}

/// The object name of a catalog segment is the digest of its payload (the same
/// digest the segment carries as its trailing bytes).
fn fragment_id(bytes: &[u8]) -> Hash32 {
    let payload_len = bytes.len() - DIGEST_BYTES as usize;
    identity::h(CATALOG_SEGMENT_DOMAIN, &bytes[..payload_len])
}

fn commit_root(
    catalog: &mut dyn CommitPlane,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<String> {
    let record = catalog.compare_exchange(&Key::pack_catalog_root(), expected, bytes)?;
    Ok(record.token.unwrap_or_else(|| control_token(bytes)))
}

fn encode_root(ids: &[Hash32], total: u64) -> Result<Vec<u8>> {
    let mut out =
        Vec::with_capacity((ROOT_HEADER_BYTES + ids.len() as u64 * 32 + DIGEST_BYTES) as usize);
    out.extend_from_slice(ROOT_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    for id in ids {
        out.extend_from_slice(id.as_bytes());
    }
    let digest = identity::h(CATALOG_ROOT_DOMAIN, &out);
    out.extend_from_slice(digest.as_bytes());
    Ok(out)
}

fn decode_root(bytes: &[u8], expected_segments: u32) -> Result<(Vec<Hash32>, u64)> {
    let minimum = (ROOT_HEADER_BYTES + DIGEST_BYTES) as usize;
    if bytes.len() < minimum {
        return Err(Error::corrupt("pack catalog root is too short"));
    }
    if &bytes[..4] != ROOT_MAGIC {
        return Err(Error::corrupt("pack catalog root magic mismatch"));
    }
    let version = u32::from_le_bytes(le4(bytes, 4)?);
    if version != FORMAT_VERSION {
        return Err(Error::unsupported_version(format!(
            "pack catalog root version {version} is not supported"
        )));
    }
    let segments = u32::from_le_bytes(le4(bytes, 8)?);
    if segments != expected_segments {
        return Err(Error::unsupported_version(format!(
            "pack catalog root names {segments} segments but this plane is configured for \
             {expected_segments}"
        )));
    }
    let total = u64::from_le_bytes(le8(bytes, 12)?);
    let body_len = ROOT_HEADER_BYTES + segments as u64 * 32;
    if body_len + DIGEST_BYTES != bytes.len() as u64 {
        return Err(Error::corrupt(
            "pack catalog root length does not match its segment count",
        ));
    }
    let digest_at = body_len as usize;
    let expected = identity::h(CATALOG_ROOT_DOMAIN, &bytes[..digest_at]);
    if bytes[digest_at..] != *expected.as_bytes() {
        return Err(Error::corrupt("pack catalog root digest mismatch"));
    }
    let mut ids = Vec::with_capacity(segments as usize);
    let mut cursor = ROOT_HEADER_BYTES as usize;
    for _ in 0..segments {
        ids.push(Hash32::from_bytes(
            bytes[cursor..cursor + 32]
                .try_into()
                .map_err(|_| Error::corrupt("catalog root segment id is truncated"))?,
        ));
        cursor += 32;
    }
    Ok((ids, total))
}

fn member_identity(key: &Key) -> Option<(u8, Hash32)> {
    match key {
        Key::Object { hash } => Some((0, *hash)),
        Key::Manifest { hash } => Some((1, *hash)),
        Key::Index { commit } => Some((2, *commit)),
        _ => None,
    }
}

fn segment_index(id: Hash32, segments: u32) -> usize {
    let raw = u32::from_le_bytes([
        id.as_bytes()[0],
        id.as_bytes()[1],
        id.as_bytes()[2],
        id.as_bytes()[3],
    ]);
    (raw & (segments - 1)) as usize
}

fn verify_member_digest(entry: &Entry, bytes: &[u8]) -> Result<()> {
    let digest = identity::h(MEMBER_DOMAIN, bytes);
    if digest != entry.digest {
        return Err(Error::corrupt(format!(
            "packed member {} does not match its catalog digest",
            entry.key().display_path()
        )));
    }
    Ok(())
}

fn take_hash(bytes: &[u8], cursor: &mut usize) -> Result<Hash32> {
    let end = cursor.saturating_add(32);
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| Error::corrupt("catalog entry is truncated"))?;
    let value: [u8; 32] = slice
        .try_into()
        .map_err(|_| Error::corrupt("catalog entry hash is truncated"))?;
    *cursor = end;
    Ok(Hash32::from_bytes(value))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let end = cursor.saturating_add(8);
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| Error::corrupt("catalog entry is truncated"))?;
    let value: [u8; 8] = slice
        .try_into()
        .map_err(|_| Error::corrupt("catalog entry integer is truncated"))?;
    *cursor = end;
    Ok(u64::from_le_bytes(value))
}

fn le4(bytes: &[u8], offset: usize) -> Result<[u8; 4]> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| Error::corrupt("catalog record is truncated"))?;
    slice
        .try_into()
        .map_err(|_| Error::corrupt("catalog record integer is truncated"))
}

fn le8(bytes: &[u8], offset: usize) -> Result<[u8; 8]> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| Error::corrupt("catalog record is truncated"))?;
    slice
        .try_into()
        .map_err(|_| Error::corrupt("catalog record integer is truncated"))
}

/// Process-fault injection for the pack court. The same `WEFT_CRASH_PHASE`
/// variable drives the store's crash proxies; `abort` is a real process death,
/// not an error return.
fn crash_point(phase: &str) {
    if let Ok(configured) = std::env::var("WEFT_CRASH_PHASE") {
        if configured == phase {
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: u8) -> Entry {
        Entry {
            kind: 0,
            id: Hash32::from_bytes([tag; 32]),
            pack: Hash32::from_bytes([tag.wrapping_add(1); 32]),
            offset: tag as u64 * 100,
            length: 100,
            digest: Hash32::from_bytes([tag.wrapping_add(2); 32]),
        }
    }

    #[test]
    fn entry_round_trip_is_canonical() {
        let entry = entry(7);
        let mut bytes = Vec::new();
        entry.encode(&mut bytes);
        assert_eq!(bytes.len() as u64, CATALOG_ENTRY_BYTES);
        assert_eq!(Entry::decode(&bytes).unwrap(), entry);
    }

    #[test]
    fn segment_rejects_a_flipped_digest() {
        let segment = Segment {
            index: 3,
            entries: vec![entry(1)],
        };
        let mut bytes = segment.encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(Segment::decode(&bytes).is_err());
    }

    #[test]
    fn segment_rejects_unordered_entries() {
        let segment = Segment {
            index: 0,
            entries: vec![entry(2), entry(1)],
        };
        assert!(Segment::decode(&segment.encode()).is_err());
    }

    #[test]
    fn root_rejects_a_mismatched_segment_count() {
        let ids = vec![Hash32::ZERO; 4];
        let bytes = encode_root(&ids, 0).unwrap();
        assert!(decode_root(&bytes, 4).is_ok());
        assert!(decode_root(&bytes, 8).is_err());
    }

    #[test]
    fn options_reject_a_member_larger_than_a_pack() {
        let options = PackOptions {
            pack_target_bytes: 4096,
            member_threshold_bytes: 8192,
            ..PackOptions::default()
        };
        assert!(options.validate().is_err());
    }

    #[test]
    fn segment_index_stays_in_range() {
        let id = Hash32::from_bytes([0xFF; 32]);
        assert_eq!(segment_index(id, 256), 255);
        assert_eq!(segment_index(id, 1), 0);
    }
}
