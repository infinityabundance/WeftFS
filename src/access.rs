//! Advisory read-access counters, the input to lineage-aware rebasing.
//!
//! The store keeps one counter per object: how many times a complete object
//! read or byte-range read observed it. The rebase planner multiplies that
//! count by the estimated decode cost of the object's *current*
//! representation; a hot object stored as an expensive delta is a candidate to
//! be re-published with an independent representation.
//!
//! Design constraints (R1):
//!
//! * **Advisory.** Counters never affect exactness, verification, commits, GC
//!   roots, or any frozen format. They live beside the store, not inside it.
//! * **Bounded.** At most [`MAX_TRACKED_OBJECTS`] counters and
//!   [`MAX_JOURNAL_RECORDS`] journal records are retained. On overflow the
//!   counter map stops accepting new objects and records a note.
//! * **Persisted.** Counters are a compact atomic snapshot
//!   (`access/counts.v1`) plus an append-shaped journal
//!   (`access/journal.v1`). A read triggers a best-effort flush every
//!   [`FLUSH_EVERY_READS`] observations, and explicit flush/close does the
//!   rest.
//! * **Rebuildable.** The snapshot is derived data: it can always be rebuilt
//!   from the journal, and deleting both only resets counters. A torn tail,
//!   wrong magic, bad digest, or older epoch is ignored with a note; it is
//!   never an error. A crash during compaction can over-count at most the
//!   journal window, which is why counters are advisory.
//!
//! Both files are addressed through [`crate::backend::Key`], so the same logic
//! runs on the local, object, and S3 backends.

use std::collections::BTreeMap;

use crate::backend::{Key, StorageBackend};
use crate::error::Result;
use crate::identity::{self, Hash32};

/// Snapshot magic (`WFA1` = WeftFS Access, format 1).
const SNAPSHOT_MAGIC: &[u8; 4] = b"WFA1";
/// Journal magic (`WFAJ`).
const JOURNAL_MAGIC: &[u8; 4] = b"WFAJ";
/// Current format version for both files.
const FORMAT_VERSION: u32 = 1;
/// Digest domain binding the snapshot bytes.
pub const ACCESS_DOMAIN: &str = "weft/access/v1";
/// Largest number of distinct object counters retained.
pub const MAX_TRACKED_OBJECTS: usize = 1_000_000;
/// Largest number of journal records replayed on open.
pub const MAX_JOURNAL_RECORDS: usize = 8192;
/// Compact after this many journal records.
pub const COMPACT_AFTER_RECORDS: usize = 2048;
/// Flush after this many observed reads (best effort, on the read path).
pub const FLUSH_EVERY_READS: u64 = 256;
/// One journal record: object id + count delta.
const RECORD_BYTES: usize = 32 + 8;

/// In-memory counters plus the persisted snapshot/journal state.
#[derive(Debug)]
pub struct AccessCounts {
    counts: BTreeMap<Hash32, u64>,
    /// Journal bytes currently published (header plus records).
    journal: Vec<u8>,
    /// Epoch shared by the current snapshot and journal.
    epoch: u64,
    /// Unflushed increments.
    pending: BTreeMap<Hash32, u64>,
    /// Reads observed since the last flush.
    pending_reads: u64,
    /// Diagnostics; never errors.
    notes: Vec<String>,
    /// True when at least one counter was dropped because of the bound.
    bounded_out: bool,
    /// Force a snapshot compaction on the next flush (a counter was consumed).
    force_compact: bool,
}

impl Default for AccessCounts {
    fn default() -> Self {
        AccessCounts {
            counts: BTreeMap::new(),
            journal: journal_header(0),
            epoch: 0,
            pending: BTreeMap::new(),
            pending_reads: 0,
            notes: Vec::new(),
            bounded_out: false,
            force_compact: false,
        }
    }
}

fn snapshot_header(epoch: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out
}

fn journal_header(epoch: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(JOURNAL_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out
}

fn snapshot_digest(bytes: &[u8]) -> Hash32 {
    identity::h(ACCESS_DOMAIN, bytes)
}

/// Encode the current counter map as a canonical snapshot.
pub fn encode_snapshot(counts: &BTreeMap<Hash32, u64>, epoch: u64) -> Vec<u8> {
    let mut out = snapshot_header(epoch);
    out.extend_from_slice(&(counts.len() as u64).to_le_bytes());
    for (id, reads) in counts {
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&reads.to_le_bytes());
    }
    let digest = snapshot_digest(&out);
    out.extend_from_slice(digest.as_bytes());
    out
}

/// Parse a snapshot. Returns the counters, the epoch, and any note.
fn parse_snapshot(bytes: &[u8]) -> Option<(BTreeMap<Hash32, u64>, u64)> {
    if bytes.len() < 16 + 8 + 32 {
        return None;
    }
    if &bytes[0..4] != SNAPSHOT_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if version != FORMAT_VERSION {
        return None;
    }
    let epoch = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let body_end = bytes.len() - 32;
    let digest = Hash32::from_slice(&bytes[body_end..]).ok()?;
    if snapshot_digest(&bytes[..body_end]) != digest {
        return None;
    }
    let count = u64::from_le_bytes(bytes[16..24].try_into().ok()?) as usize;
    if count > MAX_TRACKED_OBJECTS {
        return None;
    }
    let expected = 24usize.checked_add(count.checked_mul(40)?)?;
    if expected != body_end {
        return None;
    }
    let mut counts = BTreeMap::new();
    let mut offset = 24;
    for _ in 0..count {
        let id = Hash32::from_slice(&bytes[offset..offset + 32]).ok()?;
        let reads = u64::from_le_bytes(bytes[offset + 32..offset + 40].try_into().ok()?);
        counts.insert(id, reads);
        offset += 40;
    }
    Some((counts, epoch))
}

/// Parse a journal header and record area. Returns `(epoch, records)`.
fn parse_journal(bytes: &[u8]) -> Option<(u64, Vec<(Hash32, u64)>)> {
    if bytes.len() < 16 {
        return None;
    }
    if &bytes[0..4] != JOURNAL_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if version != FORMAT_VERSION {
        return None;
    }
    let epoch = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let body = &bytes[16..];
    let usable = body.len() - (body.len() % RECORD_BYTES);
    let mut records = Vec::with_capacity((usable / RECORD_BYTES).min(MAX_JOURNAL_RECORDS));
    for chunk in body[..usable].chunks_exact(RECORD_BYTES) {
        if records.len() >= MAX_JOURNAL_RECORDS {
            break;
        }
        let id = Hash32::from_slice(&chunk[..32]).ok()?;
        let delta = u64::from_le_bytes(chunk[32..40].try_into().ok()?);
        records.push((id, delta));
    }
    Some((epoch, records))
}

impl AccessCounts {
    /// Load counters from a backend. Every failure degrades to an empty table
    /// plus a note; it is never an error.
    pub fn load(backend: &dyn StorageBackend) -> AccessCounts {
        let mut out = AccessCounts::default();
        let mut snapshot_epoch = 0u64;
        match backend.read_all(&Key::access_counts()) {
            Ok(bytes) => match parse_snapshot(&bytes) {
                Some((counts, epoch)) => {
                    snapshot_epoch = epoch;
                    out.epoch = epoch;
                    out.counts = counts;
                }
                None => out
                    .notes
                    .push("access counters: counts snapshot rejected; starting empty".to_owned()),
            },
            Err(err) => {
                if err.class() != crate::error::Class::NotFound {
                    out.notes.push(format!(
                        "access counters: counts snapshot unreadable ({err}); starting empty"
                    ));
                }
            }
        }
        match backend.read_all(&Key::access_journal()) {
            Ok(bytes) => match parse_journal(&bytes) {
                Some((epoch, records)) if epoch == snapshot_epoch => {
                    for (id, delta) in records {
                        let entry = out.counts.entry(id).or_insert(0);
                        *entry = entry.saturating_add(delta);
                    }
                    // Keep the journal bytes verbatim (a torn tail is trimmed)
                    // so the next flush appends rather than rewrites history.
                    let usable = bytes.len() - 16 - ((bytes.len() - 16) % RECORD_BYTES);
                    out.journal = bytes[..16 + usable].to_vec();
                    if usable / RECORD_BYTES >= COMPACT_AFTER_RECORDS {
                        out.force_compact = true;
                    }
                }
                Some((epoch, _)) => out.notes.push(format!(
                    "access counters: journal epoch {epoch} is not the snapshot epoch \
                     {snapshot_epoch}; journal ignored"
                )),
                None => out
                    .notes
                    .push("access counters: journal rejected; counts snapshot only".to_owned()),
            },
            Err(err) => {
                if err.class() != crate::error::Class::NotFound {
                    out.notes.push(format!(
                        "access counters: journal unreadable ({err}); counts snapshot only"
                    ));
                }
            }
        }
        if out.counts.len() > MAX_TRACKED_OBJECTS {
            out.counts = out.counts.into_iter().take(MAX_TRACKED_OBJECTS).collect();
            out.bounded_out = true;
            out.notes.push(format!(
                "access counters: retained the first {MAX_TRACKED_OBJECTS} counters"
            ));
        }
        out
    }

    /// Record one observed read of `id`.
    pub fn note_read(&mut self, id: Hash32) {
        if id == Hash32::ZERO {
            return;
        }
        let known = self.counts.contains_key(&id);
        if !known && self.counts.len() >= MAX_TRACKED_OBJECTS {
            if !self.bounded_out {
                self.bounded_out = true;
                self.notes.push(format!(
                    "access counters: object bound {MAX_TRACKED_OBJECTS} reached; \
                     new objects are not counted"
                ));
            }
            return;
        }
        let entry = self.counts.entry(id).or_insert(0);
        *entry = entry.saturating_add(1);
        let pending = self.pending.entry(id).or_insert(0);
        *pending = pending.saturating_add(1);
        self.pending_reads = self.pending_reads.saturating_add(1);
    }

    /// Reads observed for one object.
    pub fn reads(&self, id: Hash32) -> u64 {
        self.counts.get(&id).copied().unwrap_or(0)
    }

    /// Every counter, ascending by object id.
    pub fn entries(&self) -> &BTreeMap<Hash32, u64> {
        &self.counts
    }

    /// Number of distinct objects with a counter.
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Total observations retained.
    pub fn total_reads(&self) -> u64 {
        self.counts
            .values()
            .fold(0u64, |sum, value| sum.saturating_add(*value))
    }

    /// True when the read path should flush.
    pub fn should_flush(&self) -> bool {
        self.pending_reads >= FLUSH_EVERY_READS
    }

    /// True when anything is unflushed.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Consume the counter for one object (rebase idempotence). Returns the
    /// number of reads consumed, or 0 when there was no counter.
    pub fn consume(&mut self, id: Hash32) -> u64 {
        let previous = self.counts.remove(&id).unwrap_or(0);
        // A consumed counter must not be replayed from the journal, so a flush
        // writes the authoritative map as a new snapshot epoch.
        if previous > 0 {
            self.pending.remove(&id);
            self.pending_reads = self.pending_reads.saturating_add(1);
            // The published journal still contains this object's old deltas;
            // only a fresh snapshot can express a consumed counter.
            self.force_compact = true;
        }
        previous
    }

    /// Diagnostics recorded during load or accounting.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Persist counters through the backend. Returns whether a write happened.
    ///
    /// Compaction (a new snapshot epoch plus an empty journal) runs once the
    /// journal passes [`COMPACT_AFTER_RECORDS`] or after [`consume`]. The
    /// snapshot is written before the journal, so an interrupted compaction is
    /// resolved by epoch: the stale journal is ignored on the next open.
    pub fn flush(&mut self, backend: &mut dyn StorageBackend) -> Result<bool> {
        if self.pending.is_empty() {
            return Ok(false);
        }
        let compact = self.force_compact
            || self.journal.len().saturating_sub(16) / RECORD_BYTES >= COMPACT_AFTER_RECORDS;
        if compact {
            self.epoch = self.epoch.saturating_add(1);
            let snapshot = encode_snapshot(&self.counts, self.epoch);
            backend.put_replace(&Key::access_counts(), &snapshot)?;
            self.journal = journal_header(self.epoch);
        } else {
            for (id, delta) in &self.pending {
                let mut record = [0u8; RECORD_BYTES];
                record[..32].copy_from_slice(id.as_bytes());
                record[32..].copy_from_slice(&delta.to_le_bytes());
                self.journal.extend_from_slice(&record);
            }
            backend.put_replace(&Key::access_journal(), &self.journal)?;
        }
        self.pending.clear();
        self.pending_reads = 0;
        self.force_compact = false;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_and_rejects_corruption() {
        let mut counts = BTreeMap::new();
        counts.insert(Hash32::from_bytes([1u8; 32]), 7);
        counts.insert(Hash32::from_bytes([2u8; 32]), 9);
        let bytes = encode_snapshot(&counts, 3);
        let (parsed, epoch) = parse_snapshot(&bytes).expect("snapshot parses");
        assert_eq!(epoch, 3);
        assert_eq!(parsed, counts);
        let mut corrupt = bytes.clone();
        corrupt[30] ^= 0xff;
        assert!(parse_snapshot(&corrupt).is_none());
        assert!(parse_snapshot(&bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn journal_round_trips_and_ignores_torn_tails() {
        let mut journal = journal_header(4);
        journal.extend_from_slice(&[1u8; 32]);
        journal.extend_from_slice(&5u64.to_le_bytes());
        journal.extend_from_slice(&[2u8; 3]); // torn tail
        let (epoch, records) = parse_journal(&journal).expect("journal parses");
        assert_eq!(epoch, 4);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1, 5);
        let mut wrong = journal_header(4);
        wrong[0] = b'X';
        assert!(parse_journal(&wrong).is_none());
    }

    #[test]
    fn consume_removes_the_counter() {
        let mut counts = AccessCounts::default();
        let id = Hash32::from_bytes([3u8; 32]);
        counts.note_read(id);
        counts.note_read(id);
        assert_eq!(counts.reads(id), 2);
        assert_eq!(counts.consume(id), 2);
        assert_eq!(counts.reads(id), 0);
        assert!(counts.entries().is_empty());
    }
}
