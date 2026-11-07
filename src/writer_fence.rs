//! Fenced writer-lock records shared by the remote backends (subphase R17/R18,
//! single-publication-point protocol R27).
//!
//! `S3Backend`, `GcsBackend`, and `AzureBackend` guard the single writer with
//! the same small text record and the same monotonic fencing token; only the
//! conditional write primitive differs (`If-Match` on an entity tag for S3,
//! `ifGenerationMatch` on an object generation for GCS, `If-Match` on an ETag
//! for Azure). Keeping the field set in one place is what makes the three
//! backends' diagnostics, test assertions, and receipts directly comparable:
//! the same parser reads all three, and a record written by one backend parses
//! identically when read by the other's schema names.
//!
//! The record is deliberately not a wire format of the store: it is control
//! state owned by a backend, and it never appears in a commit, manifest, or
//! content-addressed object.
//!
//! # The publication point (v3)
//!
//! A v3 record is the store's **publication point**. Publishing a head is one
//! conditional write of this record:
//!
//! * it carries the fence (`owner`, `fence`, `sequence`), and
//! * it names the head being published (`pending_generation`,
//!   `pending_commit`, `pending_commit_len`),
//! * and it is conditional on the exact bytes this handle last wrote
//!   (`If-Match` ETag / `ifGenerationMatch` / `If-Match` ETag).
//!
//! A successor that reclaims an expired lease rewrites the record, so a stale
//! writer's conditional write is refused (`412` / `409`/`412`) and no second
//! object write can slip through between a fence and a head compare-and-swap.
//! The two-slot superblock is written *after* this record as a cache; readers
//! validate it against the record and repair it on a writable open.
//!
//! # Record shapes
//!
//! ```text
//! weftfs-<backend>-lock v3
//! owner=<principal>
//! fence=<u64 monotonic token>
//! sequence=<u64 conditional writes by this handle>
//! lease_deadline_ns=<absolute deadline, unix nanoseconds>
//! renewed_ns=<when the deadline was last extended>
//! pending_generation=<head generation being published, 0 when idle>
//! pending_commit=<head commit id hex, "-" when idle>
//! pending_commit_len=<head WCB length in bytes, 0 when idle>
//! ```
//!
//! ```text
//! weftfs-<backend>-lock-released v2
//! owner=<principal that held the token>
//! fence=<u64 token being released>
//! sequence=<advanced sequence>
//! released_ns=<when>
//! released_from=<sha256 of the exact bytes this tombstone replaced>
//! pending_generation=<head the releasing writer had published, 0 when none>
//! pending_commit=<head commit id hex, "-" when none>
//! pending_commit_len=<head WCB length in bytes, 0 when none>
//! ```
//!
//! Legacy records stay readable:
//!
//! * `weftfs-<backend>-lock v2` — same fields minus `pending_commit_len`; the
//!   length is reported as `0` ("unknown") and is filled by the next
//!   publication.
//! * `weftfs-<backend>-lock v1` (`owner=` + `expires_ns=`) — read as fence 0 so
//!   the token chain stays monotonic across the upgrade.
//! * `weftfs-<backend>-lock-released v1` — a tombstone without the head fields.
//!
//! A writable open upgrades an expired v2/v1 record to v3 in the same
//! conditional write that takes the lease over, so a v2 writer and a v3 writer
//! never publish concurrently (the live v2 record refuses the open as `Busy`,
//! exactly like a live v3 record).

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::identity::Hash32;

/// The schema version a record was written under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockVersion {
    /// `owner=` + `expires_ns=`; read as fence 0.
    V1 = 1,
    /// Fenced record with the pending head but no length.
    V2 = 2,
    /// Fenced record that is the publication point; carries the head length.
    V3 = 3,
}

/// A parsed writer `LOCK` object, as published right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedLock {
    /// Schema version of the record that produced this value.
    pub schema_version: LockVersion,
    pub owner: String,
    /// Monotonic fencing token. `0` means a legacy v1 record, which predates
    /// fencing.
    pub fence: u64,
    pub sequence: u64,
    /// Absolute lease deadline (nanoseconds since the Unix epoch).
    pub deadline_ns: i64,
    pub renewed_ns: i64,
    /// The head this writer is publishing (0 / empty when none is in flight).
    pub pending_generation: u64,
    pub pending_commit: String,
    /// WCB length of `pending_commit`. `0` means the record does not know it
    /// (a v2/v1 record, or an idle record).
    pub pending_commit_len: u64,
    /// True for a released tombstone: no writer holds the lock.
    pub released: bool,
    /// Hex digest of the lock record this tombstone conditionally replaced.
    pub released_from: Option<String>,
}

impl PublishedLock {
    /// Whether a writer still holds a live lease at `now_ns`.
    pub fn is_live(&self, now_ns: i64) -> bool {
        !self.released && now_ns <= self.deadline_ns
    }

    /// The attested head, when this record names a complete one.
    pub fn attested_head(&self) -> Option<(u64, Hash32)> {
        if self.pending_generation == 0 || self.pending_commit == "-" {
            return None;
        }
        let commit = Hash32::from_lower_hex(&self.pending_commit).ok()?;
        Some((self.pending_generation, commit))
    }
}

/// A head the writer lock record attests as published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttestedHead {
    pub generation: u64,
    pub commit: Hash32,
    /// WCB length of the commit record. `0` means the record does not know it.
    pub commit_len: u64,
}

/// Encode a v3 fenced lease record that is also the publication point.
/// `schema` selects the backend-specific name; the field set is identical for
/// every backend.
pub(crate) fn encode_lock(
    schema: &str,
    owner: &str,
    fence: u64,
    sequence: u64,
    deadline_ns: i64,
    renewed_ns: i64,
    pending: Option<(u64, &Hash32, u64)>,
) -> String {
    let (generation, commit, commit_len) = match pending {
        Some((generation, commit, commit_len)) => (generation, commit.to_hex(), commit_len),
        None => (0, "-".to_owned(), 0),
    };
    format!(
        "{schema}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         lease_deadline_ns={deadline_ns}\nrenewed_ns={renewed_ns}\n\
         pending_generation={generation}\npending_commit={commit}\n\
         pending_commit_len={commit_len}\n"
    )
}

/// Encode a v2 released tombstone, carrying the head the releasing writer had
/// published so a crash between the publication and the superblock cache can
/// still be recovered from the record.
pub(crate) fn encode_released(
    schema: &str,
    owner: &str,
    fence: u64,
    sequence: u64,
    released_ns: i64,
    released_from: &str,
    pending: Option<(u64, &Hash32, u64)>,
) -> String {
    let (generation, commit, commit_len) = match pending {
        Some((generation, commit, commit_len)) => (generation, commit.to_hex(), commit_len),
        None => (0, "-".to_owned(), 0),
    };
    format!(
        "{schema}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         released_ns={released_ns}\nreleased_from={released_from}\n\
         pending_generation={generation}\npending_commit={commit}\n\
         pending_commit_len={commit_len}\n"
    )
}

/// The five schema names a backend's control records use.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Schemas<'a> {
    pub v3: &'a str,
    pub v2: &'a str,
    pub v1: &'a str,
    pub released_v2: &'a str,
    pub released_v1: &'a str,
}

/// Parse a published `LOCK` object. Accepts the fenced v3 record, the fenced
/// v2 record, the legacy v1 record (fence 0), and both released tombstones.
pub(crate) fn parse_lock(bytes: &[u8], schemas: &Schemas<'_>) -> Option<PublishedLock> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    let header = lines.next()?;
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        fields.insert(key, value);
    }
    let number = |key: &str| fields.get(key).and_then(|text| text.parse::<i64>().ok());
    let unsigned = |key: &str| fields.get(key).and_then(|text| text.parse::<u64>().ok());
    let commit = |fields: &BTreeMap<&str, &str>| {
        fields
            .get("pending_commit")
            .map(|text| (*text).to_owned())
            .unwrap_or_else(|| "-".to_owned())
    };
    if header == schemas.v3 {
        return Some(PublishedLock {
            schema_version: LockVersion::V3,
            owner: (*fields.get("owner")?).to_owned(),
            fence: unsigned("fence")?,
            sequence: unsigned("sequence")?,
            deadline_ns: number("lease_deadline_ns")?,
            renewed_ns: number("renewed_ns")?,
            pending_generation: unsigned("pending_generation")?,
            pending_commit: commit(&fields),
            pending_commit_len: unsigned("pending_commit_len")?,
            released: false,
            released_from: None,
        });
    }
    if header == schemas.v2 {
        return Some(PublishedLock {
            schema_version: LockVersion::V2,
            owner: (*fields.get("owner")?).to_owned(),
            fence: unsigned("fence")?,
            sequence: unsigned("sequence")?,
            deadline_ns: number("lease_deadline_ns")?,
            renewed_ns: number("renewed_ns")?,
            pending_generation: unsigned("pending_generation")?,
            pending_commit: commit(&fields),
            pending_commit_len: 0,
            released: false,
            released_from: None,
        });
    }
    if header == schemas.v1 {
        return Some(PublishedLock {
            schema_version: LockVersion::V1,
            owner: (*fields.get("owner")?).to_owned(),
            fence: 0,
            sequence: 0,
            deadline_ns: number("expires_ns")?,
            renewed_ns: 0,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            pending_commit_len: 0,
            released: false,
            released_from: None,
        });
    }
    if header == schemas.released_v2 {
        return Some(PublishedLock {
            schema_version: LockVersion::V3,
            owner: (*fields.get("owner")?).to_owned(),
            fence: unsigned("fence")?,
            sequence: unsigned("sequence")?,
            deadline_ns: number("released_ns")?,
            renewed_ns: number("released_ns")?,
            pending_generation: unsigned("pending_generation")?,
            pending_commit: commit(&fields),
            pending_commit_len: unsigned("pending_commit_len")?,
            released: true,
            released_from: fields.get("released_from").map(|text| (*text).to_owned()),
        });
    }
    if header == schemas.released_v1 {
        return Some(PublishedLock {
            schema_version: LockVersion::V2,
            owner: (*fields.get("owner")?).to_owned(),
            fence: unsigned("fence")?,
            sequence: unsigned("sequence")?,
            deadline_ns: number("released_ns")?,
            renewed_ns: number("released_ns")?,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            pending_commit_len: 0,
            released: true,
            released_from: fields.get("released_from").map(|text| (*text).to_owned()),
        });
    }
    None
}

/// A read-only sentence describing who holds the lock now, for diagnostics on
/// a refused publication. Never used to make a storage decision.
pub(crate) fn describe_observed(
    observed: &Option<PublishedLock>,
    owner: &str,
    fence: u64,
) -> String {
    match observed {
        Some(record) if record.owner != owner || record.fence != fence => format!(
            "; the LOCK now names owner {} fence {}",
            record.owner, record.fence
        ),
        Some(record) if record.released => "; the LOCK is a released tombstone".to_owned(),
        Some(_) => "; the LOCK still names this writer, but its own lease expired".to_owned(),
        None => "; the LOCK is absent".to_owned(),
    }
}

/// Lowercase SHA-256 of `bytes`, used for a tombstone's `released_from`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Hash32;

    fn schemas() -> Schemas<'static> {
        Schemas {
            v3: "weftfs-test-lock v3",
            v2: "weftfs-test-lock v2",
            v1: "weftfs-test-lock v1",
            released_v2: "weftfs-test-lock-released v2",
            released_v1: "weftfs-test-lock-released v1",
        }
    }

    #[test]
    fn fenced_record_round_trips() {
        let commit = Hash32::from_bytes([0xab; 32]);
        let body = encode_lock(
            schemas().v3,
            "owner-a",
            3,
            9,
            1_000,
            900,
            Some((12, &commit, 4_096)),
        );
        let record = parse_lock(body.as_bytes(), &schemas()).expect("v3 record parses");
        assert_eq!(record.schema_version, LockVersion::V3);
        assert_eq!(record.owner, "owner-a");
        assert_eq!(record.fence, 3);
        assert_eq!(record.sequence, 9);
        assert_eq!(record.deadline_ns, 1_000);
        assert_eq!(record.renewed_ns, 900);
        assert_eq!(record.pending_generation, 12);
        assert_eq!(record.pending_commit, "ab".repeat(32));
        assert_eq!(record.pending_commit_len, 4_096);
        assert!(!record.released);
        assert_eq!(
            record.attested_head(),
            Some((12, Hash32::from_bytes([0xab; 32])))
        );

        let idle = encode_lock(schemas().v3, "owner-a", 3, 10, 1_000, 950, None);
        let record = parse_lock(idle.as_bytes(), &schemas()).expect("idle record parses");
        assert_eq!(record.pending_generation, 0);
        assert_eq!(record.pending_commit, "-");
        assert_eq!(record.pending_commit_len, 0);
        assert_eq!(record.attested_head(), None);
    }

    #[test]
    fn legacy_v2_record_reads_with_an_unknown_length() {
        let v2 = format!(
            "{}\nowner=x\nfence=2\nsequence=5\nlease_deadline_ns=42\nrenewed_ns=41\n\
             pending_generation=7\npending_commit={}\n",
            schemas().v2,
            "cd".repeat(32)
        );
        let record = parse_lock(v2.as_bytes(), &schemas()).expect("v2 record parses");
        assert_eq!(record.schema_version, LockVersion::V2);
        assert_eq!(record.fence, 2);
        assert_eq!(record.pending_generation, 7);
        assert_eq!(record.pending_commit_len, 0);
        assert_eq!(
            record.attested_head(),
            Some((7, Hash32::from_lower_hex(&"cd".repeat(32)).unwrap()))
        );
    }

    #[test]
    fn legacy_v1_record_reads_as_fence_zero() {
        let legacy = format!("{}\nowner=x\nexpires_ns=42\n", schemas().v1);
        let record = parse_lock(legacy.as_bytes(), &schemas()).expect("v1 record parses");
        assert_eq!(record.schema_version, LockVersion::V1);
        assert_eq!(record.fence, 0);
        assert_eq!(record.deadline_ns, 42);
        assert_eq!(record.sequence, 0);
        assert_eq!(record.attested_head(), None);
    }

    #[test]
    fn tombstone_round_trips_and_names_its_predecessor() {
        let commit = Hash32::from_bytes([0x11; 32]);
        let body = encode_released(
            schemas().released_v2,
            "owner-a",
            4,
            11,
            5_000,
            "deadbeef",
            Some((6, &commit, 512)),
        );
        let record = parse_lock(body.as_bytes(), &schemas()).expect("tombstone parses");
        assert!(record.released);
        assert_eq!(record.fence, 4);
        assert_eq!(record.sequence, 11);
        assert_eq!(record.released_from.as_deref(), Some("deadbeef"));
        assert_eq!(record.pending_generation, 6);
        assert_eq!(record.pending_commit_len, 512);
        assert_eq!(
            record.attested_head(),
            Some((6, Hash32::from_bytes([0x11; 32])))
        );
    }

    #[test]
    fn legacy_tombstone_still_parses() {
        let body = format!(
            "{}\nowner=a\nfence=4\nsequence=11\nreleased_ns=5000\nreleased_from=deadbeef\n",
            schemas().released_v1
        );
        let record = parse_lock(body.as_bytes(), &schemas()).expect("v1 tombstone parses");
        assert!(record.released);
        assert_eq!(record.pending_generation, 0);
        assert_eq!(record.attested_head(), None);
    }

    #[test]
    fn malformed_records_are_rejected() {
        let parse = |bytes: &[u8]| parse_lock(bytes, &schemas());
        assert_eq!(parse(b"garbage"), None);
        assert_eq!(parse(b"weftfs-test-lock v3\nowner=x\n"), None);
        assert_eq!(
            parse(b"weftfs-test-lock v2\nowner=x\nfence=-1\nsequence=1\nlease_deadline_ns=1\nrenewed_ns=1\npending_generation=0\npending_commit=-\n"),
            None
        );
        // v3 without the length field is not a v3 record.
        assert_eq!(
            parse(b"weftfs-test-lock v3\nowner=x\nfence=1\nsequence=1\nlease_deadline_ns=1\nrenewed_ns=1\npending_generation=0\npending_commit=-\n"),
            None
        );
    }

    #[test]
    fn schemas_do_not_parse_across_backends() {
        let body = encode_lock("weftfs-gcs-lock v3", "o", 1, 1, 1, 1, None);
        assert!(parse_lock(body.as_bytes(), &schemas()).is_none());
    }
}
