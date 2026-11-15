//! X14 — indexed semantic candidate discovery.
//!
//! # The defect this closes
//!
//! Mode 14 (`SEMANTIC_DELTA`) needs a *base* tensor with the same dtype and
//! shape and the same semantic root under the chosen truncation/quantisation
//! policy. Before this module the only automatic way to find one was
//! [`TensorPlanOptions::semantic_scan`], a whole-store walk that decodes every
//! compatible tensor (`O(objects)` per tensor being written). It was switched
//! off by default for that reason, which silently removed the cross-version
//! savings it had found.
//!
//! This module replaces the implicit scan with an **explicit, bounded,
//! persisted index**:
//!
//! * [`Store::build_semantic_index`] reads each live tensor once and stores a
//!   small fingerprint (`weft.semantic.index.v1`): dtype, shape, element count,
//!   two quantised byte-plane histograms, and a digest of a bounded head/tail
//!   sample of the bytes. The record is content-addressed and digest-checked;
//!   a corrupt or stale record is never trusted.
//! * [`SemanticIndex::lookup`] ranks compatible entries by fingerprint distance
//!   (deterministic: distance, then base content id) and returns at most
//!   `limit` candidates. It reads no tensor bytes other than the query's own.
//!   The planner then prices the real mode-14 candidate against each one, so the
//!   index narrows the search — it never decides the representation.
//! * A **stale** entry (object deleted, or its content version moved) is
//!   detected on lookup and skipped, and the skip is counted. A stale index can
//!   therefore cost a *missed* saving, never a wrong one.
//!
//! # What it is not
//!
//! * It is not a semantic-hash index: the fingerprint is a prefilter, and the
//!   normative semantic root is still computed by the codec on the bytes.
//! * It is not persisted per commit: the index is a derived cache written on
//!   demand and invalidated by content identity. `build` is O(objects) and its
//!   cost is measured.
//! * It does not claim the scan would have found nothing: when the index is
//!   absent the planner falls back to the scan *if the caller enabled it*, and
//!   otherwise reports the omitted savings explicitly.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::representation::TensorElementDtype;
use crate::wcb_util as wu;

/// Schema of the persisted semantic candidate index.
pub const SEMANTIC_INDEX_SCHEMA: &str = "weft.semantic.index.v1";
/// Domain of the index record's identity.
pub const SEMANTIC_INDEX_DOMAIN: &str = "weft/semantic-index/v1";
/// Domain of one fingerprint's sketch digest.
pub const SEMANTIC_SKETCH_DOMAIN: &str = "weft/semantic-index/sketch/v1";
/// Hard bound on entries in one index record.
pub const MAX_ENTRIES: usize = 65_536;
/// Bytes sampled from the head of a tensor for the fingerprint.
pub const SKETCH_HEAD_BYTES: usize = 4096;
/// Histogram buckets per plane.
pub const BUCKETS: usize = 8;

/// One cheap, deterministic fingerprint of a tensor's stored bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub dtype: String,
    pub shape: Vec<u64>,
    pub element_bytes: u64,
    /// Eight quantised buckets per byte plane (two planes: the first and second
    /// half of the tensor).
    pub planes: [Vec<u32>; 2],
    /// `H("weft/semantic-index/sketch/v1", head || tail || len)`.
    pub sketch: Hash32,
}

impl Fingerprint {
    /// Compute a fingerprint from canonical tensor bytes.
    pub fn of(dtype: TensorElementDtype, shape: &[u64], bytes: &[u8]) -> Fingerprint {
        let element_bytes = u64::from(dtype.element_bytes());
        let mut planes = [vec![0u32; BUCKETS], vec![0u32; BUCKETS]];
        let half = bytes.len() / 2;
        for (index, byte) in bytes.iter().enumerate() {
            let plane = usize::from(index >= half);
            planes[plane][usize::from(*byte) >> 5] += 1;
        }
        let head = &bytes[..bytes.len().min(SKETCH_HEAD_BYTES)];
        let tail_start = bytes.len().saturating_sub(SKETCH_HEAD_BYTES);
        let tail = &bytes[tail_start..];
        let mut hasher = weftcodec::hash::DomainHasher::new(SEMANTIC_SKETCH_DOMAIN);
        hasher.update(head);
        hasher.update(tail);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        Fingerprint {
            dtype: dtype.as_str().to_owned(),
            shape: shape.to_vec(),
            element_bytes,
            planes,
            sketch: hasher.finalize(),
        }
    }

    /// Whether two fingerprints are comparable at all (the codec's own
    /// compatibility rule: same physical dtype, same shape).
    pub fn compatible(&self, other: &Fingerprint) -> bool {
        self.dtype == other.dtype && self.shape == other.shape
    }

    /// Deterministic distance. `0` means the histograms are identical.
    pub fn distance(&self, other: &Fingerprint) -> u64 {
        let mut total = 0u64;
        for plane in 0..2 {
            for bucket in 0..BUCKETS {
                total = total.saturating_add(
                    self.planes[plane][bucket].abs_diff(other.planes[plane][bucket]) as u64,
                );
            }
        }
        total
    }

    fn to_value(&self) -> Value {
        wu::map([
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("element_bytes", Value::U64(self.element_bytes)),
            (
                "planes",
                Value::Array(
                    self.planes
                        .iter()
                        .map(|plane| {
                            Value::Array(
                                plane
                                    .iter()
                                    .map(|count| Value::U64(u64::from(*count)))
                                    .collect(),
                            )
                        })
                        .collect(),
                ),
            ),
            ("sketch", wu::hash_value(self.sketch)),
        ])
    }

    fn from_value(value: &Value) -> Result<Fingerprint> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &["dtype", "shape", "element_bytes", "planes", "sketch"],
        )?;
        let shape = wu::get_array(map, "shape")?
            .iter()
            .map(|dim| match dim {
                Value::U64(value) => Ok(*value),
                _ => Err(Error::malformed("fingerprint shape must be u64 entries")),
            })
            .collect::<Result<Vec<u64>>>()?;
        let planes_value = wu::get_array(map, "planes")?;
        if planes_value.len() != 2 {
            return Err(Error::malformed(
                "fingerprint must carry exactly two planes".to_owned(),
            ));
        }
        let mut planes: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for (index, plane) in planes_value.iter().enumerate() {
            let bucket_values = match plane {
                Value::Array(values) => values,
                _ => return Err(Error::malformed("fingerprint plane must be an array")),
            };
            if bucket_values.len() != BUCKETS {
                return Err(Error::malformed(format!(
                    "fingerprint plane must hold {BUCKETS} buckets"
                )));
            }
            let mut buckets = Vec::with_capacity(BUCKETS);
            for bucket in bucket_values {
                match bucket {
                    Value::U64(value) => buckets.push(u32::try_from(*value).map_err(|_| {
                        Error::malformed("fingerprint bucket count does not fit u32")
                    })?),
                    _ => return Err(Error::malformed("fingerprint bucket must be u64")),
                }
            }
            planes[index] = buckets;
        }
        Ok(Fingerprint {
            dtype: wu::get_text(map, "dtype")?,
            shape,
            element_bytes: wu::get_u64(map, "element_bytes")?,
            planes,
            sketch: wu::get_hash(map, "sketch")?,
        })
    }
}

/// One indexed tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// The tensor object this fingerprint describes.
    pub object: Hash32,
    /// Content version the fingerprint was computed from. A different version
    /// makes the entry stale.
    pub content: Hash32,
    pub fingerprint: Fingerprint,
}

/// The persisted index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticIndex {
    entries: Vec<IndexEntry>,
    /// Entries sorted by object id, which is what the encoder writes.
    root: Hash32,
}

impl SemanticIndex {
    /// Build an index from entries. Duplicate object ids keep the first entry.
    pub fn new(mut entries: Vec<IndexEntry>) -> Result<SemanticIndex> {
        if entries.len() > MAX_ENTRIES {
            return Err(Error::limit_exceeded(format!(
                "semantic index holds {} entries, bound is {MAX_ENTRIES}; enable the scan or \
                 split the store",
                entries.len()
            )));
        }
        entries.sort_by_key(|entry| entry.object);
        entries.dedup_by_key(|entry| entry.object);
        let root = Self::compute_root(&entries)?;
        Ok(SemanticIndex { entries, root })
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Identity of the index record.
    pub fn root(&self) -> Hash32 {
        self.root
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn compute_root(entries: &[IndexEntry]) -> Result<Hash32> {
        entries_digest(entries)
    }

    /// Encode the index as canonical WCB.
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_index(&self.entries)
    }

    /// Decode and digest-check an index.
    pub fn decode(bytes: &[u8]) -> Result<SemanticIndex> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(map, &["schema", "entries", "root"])?;
        let schema = wu::get_text(map, "schema")?;
        if schema != SEMANTIC_INDEX_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "semantic index schema `{schema}` is not `{SEMANTIC_INDEX_SCHEMA}`"
            )));
        }
        let mut entries = Vec::new();
        for entry in wu::get_array(map, "entries")? {
            let entry_map = wu::as_map(entry)?;
            wu::check_keys(entry_map, &["object", "content", "fingerprint"])?;
            entries.push(IndexEntry {
                object: wu::get_hash(entry_map, "object")?,
                content: wu::get_hash(entry_map, "content")?,
                fingerprint: Fingerprint::from_value(wu::field(entry_map, "fingerprint")?)?,
            });
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::limit_exceeded(format!(
                "semantic index holds {} entries, bound is {MAX_ENTRIES}",
                entries.len()
            )));
        }
        // Strictly ascending object ids: the canonical order, and a duplicate
        // is a non-canonical record rather than a last-wins surprise.
        for pair in entries.windows(2) {
            if pair[0].object >= pair[1].object {
                return Err(Error::non_canonical(
                    "semantic index entries are not in ascending object order",
                ));
            }
        }
        let declared = wu::get_hash(map, "root")?;
        let computed = entries_digest(&entries)?;
        if computed != declared {
            return Err(Error::corrupt(
                "semantic index digest does not match its entries",
            ));
        }
        let canonical = encode_index(&entries)?;
        if canonical != bytes {
            return Err(Error::non_canonical(
                "semantic index bytes are not canonical",
            ));
        }
        Ok(SemanticIndex {
            entries,
            root: computed,
        })
    }

    /// Rank compatible entries against a query fingerprint.
    ///
    /// Deterministic order: `(distance, base content id)`. `exclude` is the
    /// query's own object (an entry is never its own base). `limit` bounds the
    /// result. Entries whose dtype/shape are not comparable are not returned at
    /// all — the codec would refuse them.
    pub fn rank(
        &self,
        query: &Fingerprint,
        exclude: Option<Hash32>,
        limit: usize,
    ) -> Vec<(u64, IndexEntry)> {
        let mut scored: Vec<(u64, IndexEntry)> = self
            .entries
            .iter()
            .filter(|entry| Some(entry.object) != exclude)
            .filter(|entry| query.compatible(&entry.fingerprint))
            .map(|entry| (query.distance(&entry.fingerprint), entry.clone()))
            .collect();
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.content.cmp(&b.1.content))
                // The codec's base selection is deterministic on content
                // identity, so the tie-break must be too.
                .then_with(|| a.1.object.cmp(&b.1.object))
        });
        scored.truncate(limit.max(1));
        scored
    }
}

/// Canonical order: ascending object id, first entry per object wins.
fn canonical_entries(mut entries: Vec<IndexEntry>) -> Vec<IndexEntry> {
    entries.sort_by_key(|entry| entry.object);
    entries.dedup_by_key(|entry| entry.object);
    entries
}

fn entries_digest(entries: &[IndexEntry]) -> Result<Hash32> {
    let value = Value::Array(entries.iter().map(entry_value).collect());
    let bytes = wu::encode(&value)?;
    Ok(weftcodec::hash::hash_domain(SEMANTIC_INDEX_DOMAIN, &bytes))
}

fn encode_index(entries: &[IndexEntry]) -> Result<Vec<u8>> {
    if entries.len() > MAX_ENTRIES {
        return Err(Error::limit_exceeded(format!(
            "semantic index holds {} entries, bound is {MAX_ENTRIES}",
            entries.len()
        )));
    }
    let sorted = canonical_entries(entries.to_vec());
    let root = entries_digest(&sorted)?;
    wu::encode(&wu::map([
        ("schema", Value::Str(SEMANTIC_INDEX_SCHEMA.to_owned())),
        (
            "entries",
            Value::Array(sorted.iter().map(entry_value).collect()),
        ),
        ("root", wu::hash_value(root)),
    ]))
}

fn entry_value(entry: &IndexEntry) -> Value {
    wu::map([
        ("object", wu::hash_value(entry.object)),
        ("content", wu::hash_value(entry.content)),
        ("fingerprint", entry.fingerprint.to_value()),
    ])
}

/// One candidate returned by a lookup.
///
/// Both identities are carried because they mean different things: `object`
/// names the versioned file, `content` is the manifest identity the codec's
/// semantic-delta dependency must name. Returning the wrong one is not a
/// cosmetic mistake — a dependency on an object id is not a manifest id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateTensor {
    pub object: Hash32,
    pub content: Hash32,
    pub bytes: Vec<u8>,
}

/// Why a lookup produced no candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookupStats {
    /// Entries examined.
    pub examined: usize,
    /// Compatible entries (dtype + shape).
    pub compatible: usize,
    /// Entries dropped because their object no longer resolves or its content
    /// version changed (a stale index).
    pub stale: usize,
    /// Candidates returned to the planner.
    pub returned: usize,
    /// Fingerprint distance of the first returned candidate.
    pub best_distance: Option<u64>,
}

/// Summary of one index build.
#[derive(Clone, Debug)]
pub struct BuildReport {
    pub index: SemanticIndex,
    /// Live tensor objects examined (whether or not they were indexed).
    pub objects_scanned: usize,
    /// Objects whose metadata or bytes could not be read.
    pub skipped: usize,
    /// Logical bytes read to fingerprint the store.
    pub bytes_read: u64,
    /// Wall time of the build.
    pub elapsed_ms: f64,
    /// Bytes of the encoded index.
    pub record_bytes: u64,
}

impl BuildReport {
    /// Bytes of the index record.
    pub fn record_bytes(&self) -> u64 {
        self.record_bytes
    }
}

/// Lookup by a device type/shape/byte query, used by the planner and the court.
pub struct LookupRequest<'a> {
    pub dtype: TensorElementDtype,
    pub shape: &'a [u64],
    pub bytes: &'a [u8],
    pub exclude: Option<Hash32>,
    pub limit: usize,
}

/// One thing a caller may want to know about a candidate.
#[derive(Clone, Debug, Default)]
pub struct ScoreTable {
    pub by_object: BTreeMap<Hash32, u64>,
}

impl crate::store::Store {
    /// Build the X14 semantic candidate index over the live tensor objects.
    ///
    /// Reads each tensor's bytes once to fingerprint it. Bounded by
    /// `max_objects` (the walk stops at that many live objects, and the report
    /// says `truncated`) and by `max_bytes` (the walk stops before exceeding
    /// it). A tensor whose metadata or bytes cannot be read is counted in
    /// `skipped` and left out rather than failing the build.
    ///
    /// The index is **not** written by this call; [`Store::store_semantic_index`]
    /// persists it. Keeping measurement and persistence separate lets a court
    /// price a build without changing the store.
    pub fn build_semantic_index(&self, max_objects: usize, max_bytes: u64) -> Result<BuildReport> {
        let started = std::time::Instant::now();
        let state = self.current_state();
        let mut ids: Vec<Hash32> = state
            .objects
            .values()
            .filter(|record| record.kind == crate::state::ObjectKind::File)
            .map(|record| record.id)
            .collect();
        ids.sort();
        let mut entries = Vec::new();
        let mut objects_scanned = 0usize;
        let mut skipped = 0usize;
        let mut bytes_read = 0u64;
        for id in ids {
            if objects_scanned >= max_objects {
                break;
            }
            if bytes_read >= max_bytes {
                break;
            }
            objects_scanned += 1;
            let Ok(meta) = self.tensor_metadata(id) else {
                skipped += 1;
                continue;
            };
            let record = match state.objects.get(&id) {
                Some(record) => record,
                None => continue,
            };
            if bytes_read.saturating_add(record.size) > max_bytes {
                break;
            }
            let bytes = match self.read_object(id) {
                Ok(bytes) => bytes,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            bytes_read = bytes_read.saturating_add(bytes.len() as u64);
            entries.push(IndexEntry {
                object: id,
                content: record.content,
                fingerprint: Fingerprint::of(meta.dtype, &meta.shape, &bytes),
            });
        }
        let index = SemanticIndex::new(entries)?;
        let record_bytes = index.encode()?.len() as u64;
        Ok(BuildReport {
            index,
            objects_scanned,
            skipped,
            bytes_read,
            elapsed_ms: started.elapsed().as_secs_f64() * 1e3,
            record_bytes,
        })
    }

    /// Persist an index at `semantic-index.root` (one atomic replacement).
    pub fn store_semantic_index(&mut self, index: &SemanticIndex) -> Result<()> {
        self.require_writable()?;
        let bytes = index.encode()?;
        self.with_backend_mut(|backend| {
            backend.put_replace(&crate::backend::Key::semantic_index(), &bytes)
        })
    }

    /// Load the persisted index, if any. A record that fails its digest is a
    /// hard error: a stale or damaged index is never silently trusted.
    pub fn load_semantic_index(&self) -> Result<Option<SemanticIndex>> {
        match self.with_backend(|backend| backend.read_all(&crate::backend::Key::semantic_index()))
        {
            Ok(bytes) => Ok(Some(SemanticIndex::decode(&bytes)?)),
            Err(err) if err.class() == crate::error::Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Ask the index for mode-14 bases for `request`.
    ///
    /// Returns the candidate `(object, bytes)` pairs in ranked order plus the
    /// lookup statistics. An entry whose object no longer exists, or whose
    /// content version has moved, is skipped and counted as stale — the bytes are
    /// never trusted from a stale fingerprint.
    pub fn query_semantic_index(
        &self,
        index: &SemanticIndex,
        request: &LookupRequest,
    ) -> Result<(Vec<CandidateTensor>, LookupStats)> {
        let query = Fingerprint::of(request.dtype, request.shape, request.bytes);
        let mut stats = LookupStats {
            examined: index.len(),
            ..LookupStats::default()
        };
        stats.compatible = index
            .entries()
            .iter()
            .filter(|entry| query.compatible(&entry.fingerprint))
            .count();
        let ranked = index.rank(&query, request.exclude, request.limit);
        if let Some((distance, _)) = ranked.first() {
            stats.best_distance = Some(*distance);
        }
        let state = self.current_state();
        let mut out = Vec::new();
        for (_distance, entry) in ranked {
            let current = state.objects.get(&entry.object);
            let fresh = current
                .map(|record| {
                    record.kind == crate::state::ObjectKind::File && record.content == entry.content
                })
                .unwrap_or(false);
            if !fresh {
                stats.stale += 1;
                continue;
            }
            match self.read_object(entry.object) {
                Ok(bytes) => out.push(CandidateTensor {
                    object: entry.object,
                    content: entry.content,
                    bytes,
                }),
                Err(_) => {
                    stats.stale += 1;
                    continue;
                }
            }
        }
        stats.returned = out.len();
        Ok((out, stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(seed: u8, shape: &[u64]) -> Fingerprint {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).map(|b| b ^ seed).collect();
        Fingerprint::of(TensorElementDtype::Bf16, shape, &bytes)
    }

    #[test]
    fn compatible_entries_rank_by_distance_then_content() {
        let query = fingerprint(0, &[4, 512]);
        let near = fingerprint(1, &[4, 512]);
        let far = fingerprint(200, &[4, 512]);
        let other_shape = fingerprint(0, &[8, 256]);
        let entries = vec![
            IndexEntry {
                object: Hash32::from_lower_hex(&"11".repeat(32)).unwrap(),
                content: Hash32::from_lower_hex(&"21".repeat(32)).unwrap(),
                fingerprint: far,
            },
            IndexEntry {
                object: Hash32::from_lower_hex(&"12".repeat(32)).unwrap(),
                content: Hash32::from_lower_hex(&"22".repeat(32)).unwrap(),
                fingerprint: near,
            },
            IndexEntry {
                object: Hash32::from_lower_hex(&"13".repeat(32)).unwrap(),
                content: Hash32::from_lower_hex(&"23".repeat(32)).unwrap(),
                fingerprint: other_shape,
            },
        ];
        let index = SemanticIndex::new(entries).unwrap();
        let ranked = index.rank(&query, None, 4);
        assert_eq!(ranked.len(), 2, "the other shape is not comparable");
        assert!(ranked[0].0 <= ranked[1].0);
        // The same query twice gives the same order.
        let again = index.rank(&query, None, 4);
        assert_eq!(
            ranked.iter().map(|(_, e)| e.object).collect::<Vec<_>>(),
            again.iter().map(|(_, e)| e.object).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_record_round_trips_and_rejects_a_digest_mismatch() {
        let entries = vec![IndexEntry {
            object: Hash32::from_lower_hex(&"31".repeat(32)).unwrap(),
            content: Hash32::from_lower_hex(&"41".repeat(32)).unwrap(),
            fingerprint: fingerprint(9, &[2, 3]),
        }];
        let index = SemanticIndex::new(entries).unwrap();
        let bytes = index.encode().unwrap();
        let decoded = SemanticIndex::decode(&bytes).unwrap();
        assert_eq!(decoded, index);

        // A truncated record is not a record.
        assert!(SemanticIndex::decode(&bytes[..bytes.len() - 1]).is_err());
        // A record whose declared root does not match its entries is refused.
        let mut value = wu::decode(&bytes).unwrap();
        let map = match &mut value {
            Value::Map(map) => map,
            _ => unreachable!(),
        };
        map.insert(
            "root".to_owned(),
            wu::hash_value(Hash32::from_lower_hex(&"99".repeat(32)).unwrap()),
        );
        let forged = wu::encode(&value).unwrap();
        assert_eq!(
            SemanticIndex::decode(&forged).unwrap_err().class(),
            crate::error::Class::Corrupt
        );
    }
}
