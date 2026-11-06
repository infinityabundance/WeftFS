//! Fenced writer-lock records shared by the remote backends (subphase R18).
//!
//! `S3Backend` and `GcsBackend` guard the single writer with the same small
//! text record and the same monotonic fencing token; only the conditional
//! write primitive differs (`If-Match` on an entity tag for S3,
//! `ifGenerationMatch` on an object generation for GCS). Keeping the field set
//! in one place is what makes the two backends' diagnostics, test assertions,
//! and receipts directly comparable: the same parser reads both, and a record
//! written by one backend parses identically when read by the other's schema
//! names.
//!
//! The record is deliberately not a wire format of the store: it is control
//! state owned by a backend, and it never appears in a commit, manifest, or
//! content-addressed object.
//!
//! # Record shapes
//!
//! ```text
//! weftfs-<backend>-lock v2
//! owner=<principal>
//! fence=<u64 monotonic token>
//! sequence=<u64 conditional writes by this handle>
//! lease_deadline_ns=<absolute deadline, unix nanoseconds>
//! renewed_ns=<when the deadline was last extended>
//! pending_generation=<head generation being published, 0 when idle>
//! pending_commit=<head commit id hex, "-" when idle>
//! ```
//!
//! ```text
//! weftfs-<backend>-lock-released v1
//! owner=<principal that held the token>
//! fence=<u64 token being released>
//! sequence=<advanced sequence>
//! released_ns=<when>
//! released_from=<sha256 of the exact bytes this tombstone replaced>
//! ```
//!
//! A legacy v1 record (`owner=` + `expires_ns=`) is read as fence 0 so the
//! token chain stays monotonic across the upgrade.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::identity::Hash32;

/// A parsed writer `LOCK` object, as published right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedLock {
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
}

/// Encode a fenced lease record. `schema` selects the backend-specific name;
/// the field set is identical for every backend.
pub(crate) fn encode_lock(
    schema: &str,
    owner: &str,
    fence: u64,
    sequence: u64,
    deadline_ns: i64,
    renewed_ns: i64,
    pending: Option<(u64, &Hash32)>,
) -> String {
    let (generation, commit) = match pending {
        Some((generation, commit)) => (generation, commit.to_hex()),
        None => (0, "-".to_owned()),
    };
    format!(
        "{schema}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         lease_deadline_ns={deadline_ns}\nrenewed_ns={renewed_ns}\n\
         pending_generation={generation}\npending_commit={commit}\n"
    )
}

/// Encode a released tombstone. A tombstone is used instead of deleting the
/// record because deleting it would reset the fencing token.
pub(crate) fn encode_released(
    schema: &str,
    owner: &str,
    fence: u64,
    sequence: u64,
    released_ns: i64,
    released_from: &str,
) -> String {
    format!(
        "{schema}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         released_ns={released_ns}\nreleased_from={released_from}\n"
    )
}

/// Parse a published `LOCK` object. Accepts the fenced v2 record, the legacy v1
/// record (fence 0), and the released tombstone.
pub(crate) fn parse_lock(
    bytes: &[u8],
    schema: &str,
    schema_v1: &str,
    released_schema: &str,
) -> Option<PublishedLock> {
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
    match header {
        header if header == schema => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: u64::try_from(number("fence")?).ok()?,
            sequence: u64::try_from(number("sequence")?).ok()?,
            deadline_ns: number("lease_deadline_ns")?,
            renewed_ns: number("renewed_ns")?,
            pending_generation: u64::try_from(number("pending_generation")?).ok()?,
            pending_commit: fields
                .get("pending_commit")
                .map(|text| (*text).to_owned())
                .unwrap_or_else(|| "-".to_owned()),
            released: false,
            released_from: None,
        }),
        header if header == schema_v1 => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: 0,
            sequence: 0,
            deadline_ns: number("expires_ns")?,
            renewed_ns: 0,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            released: false,
            released_from: None,
        }),
        header if header == released_schema => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: u64::try_from(number("fence")?).ok()?,
            sequence: u64::try_from(number("sequence")?).ok()?,
            deadline_ns: number("released_ns")?,
            renewed_ns: number("released_ns")?,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            released: true,
            released_from: fields.get("released_from").map(|text| (*text).to_owned()),
        }),
        _ => None,
    }
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

    fn schema() -> &'static str {
        "weftfs-test-lock v2"
    }

    #[test]
    fn fenced_record_round_trips() {
        let commit = Hash32::from_bytes([0xab; 32]);
        let body = encode_lock(schema(), "owner-a", 3, 9, 1_000, 900, Some((12, &commit)));
        let record = parse_lock(
            body.as_bytes(),
            schema(),
            "weftfs-test-lock v1",
            "weftfs-test-lock-released v1",
        )
        .expect("v2 record parses");
        assert_eq!(record.owner, "owner-a");
        assert_eq!(record.fence, 3);
        assert_eq!(record.sequence, 9);
        assert_eq!(record.deadline_ns, 1_000);
        assert_eq!(record.renewed_ns, 900);
        assert_eq!(record.pending_generation, 12);
        assert_eq!(record.pending_commit, "ab".repeat(32));
        assert!(!record.released);

        let idle = encode_lock(schema(), "owner-a", 3, 10, 1_000, 950, None);
        let record = parse_lock(
            idle.as_bytes(),
            schema(),
            "weftfs-test-lock v1",
            "weftfs-test-lock-released v1",
        )
        .expect("idle record parses");
        assert_eq!(record.pending_generation, 0);
        assert_eq!(record.pending_commit, "-");
    }

    #[test]
    fn legacy_record_reads_as_fence_zero() {
        let legacy = format!("weftfs-test-lock v1\nowner=x\nexpires_ns=42\n");
        let record = parse_lock(
            legacy.as_bytes(),
            schema(),
            "weftfs-test-lock v1",
            "weftfs-test-lock-released v1",
        )
        .expect("v1 record parses");
        assert_eq!(record.fence, 0);
        assert_eq!(record.deadline_ns, 42);
        assert_eq!(record.sequence, 0);
    }

    #[test]
    fn tombstone_round_trips_and_names_its_predecessor() {
        let body = encode_released(
            "weftfs-test-lock-released v1",
            "owner-a",
            4,
            11,
            5_000,
            "deadbeef",
        );
        let record = parse_lock(
            body.as_bytes(),
            schema(),
            "weftfs-test-lock v1",
            "weftfs-test-lock-released v1",
        )
        .expect("tombstone parses");
        assert!(record.released);
        assert_eq!(record.fence, 4);
        assert_eq!(record.sequence, 11);
        assert_eq!(record.released_from.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn malformed_records_are_rejected() {
        let parse = |bytes: &[u8]| {
            parse_lock(
                bytes,
                schema(),
                "weftfs-test-lock v1",
                "weftfs-test-lock-released v1",
            )
        };
        assert_eq!(parse(b"garbage"), None);
        assert_eq!(parse(b"weftfs-test-lock v2\nowner=x\n"), None);
        assert_eq!(
            parse(b"weftfs-test-lock v2\nowner=x\nfence=-1\nsequence=1\nlease_deadline_ns=1\nrenewed_ns=1\npending_generation=0\npending_commit=-\n"),
            None
        );
    }

    #[test]
    fn schemas_do_not_parse_across_backends() {
        let body = encode_lock("weftfs-gcs-lock v2", "o", 1, 1, 1, 1, None);
        assert!(parse_lock(
            body.as_bytes(),
            "weftfs-s3-lock v2",
            "weftfs-s3-lock v1",
            "weftfs-s3-lock-released v1"
        )
        .is_none());
    }
}
