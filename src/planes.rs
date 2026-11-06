//! Blob and commit planes: the two contracts a physical backend must honour.
//!
//! Historically [`crate::backend::StorageBackend`] carried both halves of the
//! job: immutable content-addressed bytes (`objects/`, `manifests/`,
//! `indexes/`) *and* POSIX-shaped control state (the two-slot superblock,
//! `write_at`, `sync_dir`, a writer lock). That works for one local filesystem,
//! but it forces every future backend to simulate POSIX superblock and lock
//! semantics.
//!
//! This module splits the contract in two:
//!
//! * [`BlobPlane`] — immutable bytes: read, ranged read, create-once
//!   publication, deletion, listing. This is everything a content-addressed
//!   chunk or manifest needs.
//! * [`CommitPlane`] — versioned control state: read/replace control records,
//!   compare-and-swap on a record, and a decoded `head` with
//!   [`CommitPlane::compare_exchange_head`]. This is everything the commit
//!   protocol needs.
//!
//! Both ship with real implementations for [`crate::backend::LocalBackend`],
//! [`crate::backend::ObjectBackend`], and [`crate::s3::S3Backend`]; a
//! [`crate::backend::CompositeBackend`] routes each [`Key`] to one plane so a
//! store can keep blobs and commits in different physical systems.
//!
//! # Capabilities are proved, not declared
//!
//! Every capability is either **probed** on a writable open
//! ([`BlobPlane::head_part`], [`CommitPlane::probe`]) or **attested** by a
//! primitive the backend actually exercises:
//!
//! * `atomic_publish` — the blob probe publishes a private object and observes
//!   that a second create cannot replace it; the commit probe performs a real
//!   compare-and-swap.
//! * `conditional_update` — the commit probe runs a *stale* compare-and-swap
//!   and requires it to be rejected. An endpoint that accepts a stale
//!   conditional write is refused, never downgraded.
//! * `durable_fsync` — attested by the publication primitive (`fsync`+`rename`
//!   / `link`, or the provider's acknowledged durable PUT). No probe can prove
//!   durability without physical power loss; nothing here claims otherwise.
//!
//! A probe may only **refute** a declared capability, never upgrade one that
//! was declared absent. A caller that declares `atomic_publish: false` (for
//! example [`crate::backend::BackendKind::Posix`] representing an unmeasured
//! network filesystem) is refused where the store needs that capability and is
//! never silently promoted by a probe.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::Serialize;

use crate::backend::{BackendStats, Capabilities, Key, StorageBackend};
use crate::error::{Class, Error, Result};
use crate::format::{decode_superblock, SuperblockSlot, SLOT_BYTES, SUPERBLOCK_BYTES};
use crate::identity::Hash32;

/// Domain for the control-record compare-and-swap token.
pub const CONTROL_TOKEN_DOMAIN: &str = "weft/control/token/v1";

/// Prefix of every probe scratch key. It starts with `put-` so the existing
/// stale-temp sweep removes a probe left behind by a crash.
pub const PROBE_KEY_PREFIX: &str = "put-plane-probe";

/// Canonical compare-and-swap token for a control record: the content token of
/// its bytes. Uniform across planes so a caller cannot confuse a transport
/// token (an S3 ETag) with the bytes it actually observed.
pub fn control_token(bytes: &[u8]) -> String {
    crate::identity::h(CONTROL_TOKEN_DOMAIN, bytes).to_hex()
}

static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A private, unique scratch key under the plane's `tmp/` namespace.
fn probe_key(tag: &str) -> Key {
    Key::tmp(format!(
        "{PROBE_KEY_PREFIX}-{tag}-{}-{}",
        std::process::id(),
        PROBE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Deterministic probe payload, bounded to a few hundred bytes.
pub(crate) fn probe_bytes() -> Vec<u8> {
    (0..192u16).map(|index| (index % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Declared and probed guarantees of a blob plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlobCapabilities {
    /// Backend label (`local`, `object`, `s3`, …).
    pub backend: String,
    pub atomic_publish: bool,
    pub durable_fsync: bool,
    pub true_range_reads: bool,
    pub max_object_bytes: u64,
    pub consistency: String,
    pub publication: String,
}

impl BlobCapabilities {
    /// Derive the blob view of a backend's [`Capabilities`].
    pub fn from_capabilities(backend: &str, capabilities: &Capabilities) -> BlobCapabilities {
        BlobCapabilities {
            backend: backend.to_owned(),
            atomic_publish: capabilities.atomic_publish,
            durable_fsync: capabilities.durable_fsync,
            true_range_reads: capabilities.true_range_reads,
            max_object_bytes: capabilities.max_object_bytes,
            consistency: capabilities.consistency.clone(),
            publication: capabilities.publication.clone(),
        }
    }
}

/// Declared and probed guarantees of a commit plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommitCapabilities {
    pub backend: String,
    /// Conditional (compare-and-swap) control updates. This is the one
    /// capability a writable store cannot proceed without.
    pub conditional_update: bool,
    pub atomic_publish: bool,
    pub durable_fsync: bool,
    pub max_control_bytes: u64,
    pub consistency: String,
    pub publication: String,
}

impl CommitCapabilities {
    /// Derive the commit view of a backend's [`Capabilities`].
    ///
    /// `conditional_update` requires both the declared conditional-head
    /// support and atomic publication: a compare-and-swap that is not atomic
    /// is not a compare-and-swap.
    pub fn from_capabilities(backend: &str, capabilities: &Capabilities) -> CommitCapabilities {
        CommitCapabilities {
            backend: backend.to_owned(),
            conditional_update: capabilities.conditional_head && capabilities.atomic_publish,
            atomic_publish: capabilities.atomic_publish,
            durable_fsync: capabilities.durable_fsync,
            max_control_bytes: capabilities.max_object_bytes,
            consistency: capabilities.consistency.clone(),
            publication: capabilities.publication.clone(),
        }
    }
}

/// Evidence produced by [`BlobPlane::head_part`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BlobProbe {
    pub backend: String,
    /// `false` when the required capability was declared absent, so the probe
    /// was skipped instead of exercising a path the store will refuse anyway.
    pub attempted: bool,
    pub create_once_visible: bool,
    pub length_match: bool,
    pub range_read_match: bool,
    pub delete_observed: bool,
    pub bytes_probed: u64,
    pub detail: String,
}

impl BlobProbe {
    fn passed(&self) -> bool {
        self.create_once_visible
            && self.length_match
            && self.range_read_match
            && self.delete_observed
    }
}

/// Evidence produced by [`CommitPlane::probe`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CommitProbe {
    pub backend: String,
    pub attempted: bool,
    pub create_ok: bool,
    pub matching_compare_exchange_ok: bool,
    /// The probe fails closed unless a *stale* compare-and-swap is rejected.
    pub stale_compare_exchange_rejected: bool,
    pub token_kind: String,
    pub detail: String,
}

/// Both plane probes from one writable open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlaneProbes {
    pub blobs: BlobProbe,
    pub commits: CommitProbe,
}

/// Per-plane capabilities of a composite backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlaneCapabilities {
    pub blobs: BlobCapabilities,
    pub commits: CommitCapabilities,
}

/// Per-plane request/byte counters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PlaneStats {
    pub blobs: BackendStats,
    pub commits: BackendStats,
}

// ---------------------------------------------------------------------------
// Control records
// ---------------------------------------------------------------------------

/// One control record: opaque bytes plus the compare-and-swap token a caller
/// must present to replace them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRecord {
    pub bytes: Vec<u8>,
    /// [`control_token`] of `bytes`. Never `None` for a record that exists.
    pub token: Option<String>,
}

/// The decoded published head of a store.
///
/// This is the superblock slot that recovery would select: the highest
/// generation with a valid CRC. `generation == 0` never occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitHead {
    pub generation: u64,
    pub commit: Hash32,
    pub commit_len: u64,
    pub timestamp_ns: i64,
}

// ---------------------------------------------------------------------------
// Blob plane
// ---------------------------------------------------------------------------

/// Immutable byte plane: content-addressed chunks, manifests, and indexes.
///
/// Every write is create-once: publishing the same key twice with identical
/// bytes is a no-op, publishing different bytes is an error. There is no
/// in-place mutation, no directory fsync, and no lock in this contract.
pub trait BlobPlane: Send {
    /// Backend label used in receipts.
    fn blob_label(&self) -> &'static str;

    /// Declared guarantees of this plane.
    fn blob_capabilities(&self) -> BlobCapabilities;

    /// Read the whole object.
    fn get(&self, key: &Key) -> Result<Vec<u8>>;

    /// Read `len` bytes at `offset`. Fewer bytes are returned at end of object;
    /// an offset strictly beyond the object is an error.
    fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// Publish `bytes` under `key` all-or-nothing, create-once.
    fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()>;

    /// Delete one object. A missing object is `NotFound`.
    fn delete(&mut self, key: &Key) -> Result<()>;

    /// Object length in bytes.
    fn len(&self, key: &Key) -> Result<u64>;

    /// Whether the object exists.
    fn exists(&self, key: &Key) -> Result<bool>;

    /// List every key under a prefix selector.
    fn list(&self, prefix: &Key) -> Result<Vec<Key>>;

    /// Remove stale staging files older than `grace_secs`.
    fn clean_stale(&mut self, grace_secs: u64) -> Result<()>;

    /// Create the plane's physical skeleton for a new store (bucket, shard
    /// directory, and so on).
    fn init_layout(&mut self) -> Result<()>;

    /// Recreate missing skeleton entries for an existing store.
    fn ensure_layout(&mut self) -> Result<()>;

    /// Plane counters.
    fn stats(&self) -> BackendStats;

    /// Plane diagnostics.
    fn diagnostics(&self) -> Vec<String>;

    /// Physics root of this plane (diagnostics only).
    fn root(&self) -> &Path;

    /// Probe the plane: publish a private object, `HEAD` it for its length,
    /// read back a *part* of it, and delete it.
    ///
    /// The name records the three facts it establishes: create-once
    /// visibility, exact length, and a true ranged read. It cannot establish
    /// durability; that stays an attested property of the publication
    /// primitive. A declared-absent `atomic_publish` skips the probe, because
    /// the store refuses that configuration where it matters.
    fn head_part(&mut self, bytes: &[u8]) -> Result<BlobProbe> {
        let label = self.blob_label().to_owned();
        let capabilities = self.blob_capabilities();
        let mut probe = BlobProbe {
            backend: label,
            attempted: false,
            ..BlobProbe::default()
        };
        if !capabilities.atomic_publish {
            probe.detail =
                "skipped: atomic_publish is declared absent, so the store refuses this plane \
                 wherever a create-once publication is required"
                    .to_owned();
            return Ok(probe);
        }
        probe.attempted = true;
        probe.bytes_probed = bytes.len() as u64;
        let key = probe_key("blob");
        // `put_if_absent` on a fresh key must make the bytes visible.
        self.put_if_absent(&key, bytes)?;
        probe.create_once_visible = match self.get(&key) {
            Ok(stored) => stored == bytes,
            Err(_) => false,
        };
        probe.length_match = self.len(&key)? == bytes.len() as u64;
        let start = bytes.len() / 3;
        let want = (bytes.len() - start).min(17);
        if want > 0 {
            let part = self.get_range(&key, start as u64, want as u64)?;
            probe.range_read_match = part == bytes[start..start + want];
        } else {
            probe.range_read_match = true;
        }
        self.delete(&key)?;
        probe.delete_observed = !self.exists(&key)?;
        if !probe.passed() {
            return Err(Error::io(format!(
                "blob plane `{}` failed its capability probe (create_once_visible={}, \
                 length_match={}, range_read_match={}, delete_observed={})",
                probe.backend,
                probe.create_once_visible,
                probe.length_match,
                probe.range_read_match,
                probe.delete_observed
            )));
        }
        probe.detail = format!(
            "published {} bytes under a private key, HEAD reported that length, a ranged read of \
             {} bytes matched, and delete was observed; durability is attested by `{}` and is not \
             proved here",
            probe.bytes_probed, want, capabilities.publication
        );
        Ok(probe)
    }
}

// ---------------------------------------------------------------------------
// Commit plane
// ---------------------------------------------------------------------------

/// Versioned control plane: commit records, refs, the superblock head, and the
/// single-writer lock.
///
/// The normative local layout, the commit-file encoding, and the two-slot
/// superblock are unchanged: an implementation of this trait is expected to
/// store the exact same bytes under the same logical keys. What the contract
/// adds is an explicit compare-and-swap over a control record, which a
/// non-POSIX metadata engine (a distributed KV store, a pack index, an
/// external consensus log) can implement without pretending to be a
/// filesystem.
pub trait CommitPlane: Send {
    /// Backend label used in receipts.
    fn commit_label(&self) -> &'static str;

    /// Declared guarantees of this plane.
    fn commit_capabilities(&self) -> CommitCapabilities;

    /// Read a control record. `Ok(None)` means it does not exist.
    fn read_control(&self, key: &Key) -> Result<Option<ControlRecord>>;

    /// Atomically replace `key` with `bytes` iff it still holds the record
    /// whose token is `expected`. `None` means the record must be absent.
    /// A lost race is `Busy`, never a silent overwrite.
    fn compare_exchange(
        &mut self,
        key: &Key,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> Result<ControlRecord>;

    /// Atomically replace a control record unconditionally.
    fn write_control(&mut self, key: &Key, bytes: &[u8]) -> Result<()>;

    /// Create a zero-filled control record of `len` bytes. Fails if it exists.
    fn create_control(&mut self, key: &Key, len: u64) -> Result<()>;

    /// Publish a control record create-once.
    fn put_control_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()>;

    /// Delete a control record.
    fn delete_control(&mut self, key: &Key) -> Result<()>;

    /// Control record length.
    fn control_len(&self, key: &Key) -> Result<u64>;

    /// Whether a control record exists.
    fn control_exists(&self, key: &Key) -> Result<bool>;

    /// List control records under a prefix selector.
    fn list_control(&self, prefix: &Key) -> Result<Vec<Key>>;

    /// Last modification time, when the plane can report one.
    fn modified(&self, key: &Key) -> Result<Option<SystemTime>>;

    /// Make the directory holding `key` durable.
    fn sync_dir(&mut self, key: &Key) -> Result<()>;

    /// Create the plane's physical skeleton for a new store.
    fn init_layout(&mut self) -> Result<()>;

    /// Recreate missing skeleton entries for an existing store.
    fn ensure_layout(&mut self) -> Result<()>;

    /// Acquire the exclusive single-writer lock.
    fn acquire_lock(&mut self) -> Result<()>;

    /// Remove stale staging files older than `grace_secs`.
    fn clean_stale(&mut self, grace_secs: u64) -> Result<()>;

    /// Plane counters.
    fn stats(&self) -> BackendStats;

    /// Plane diagnostics.
    fn diagnostics(&self) -> Vec<String>;

    /// Physics root of this plane (diagnostics only).
    fn root(&self) -> &Path;

    /// The published head, or `None` for a store whose superblock is still
    /// all zeros.
    fn load_head(&self) -> Result<Option<CommitHead>> {
        let Some(record) = self.read_control(&Key::superblock())? else {
            return Ok(None);
        };
        highest_slot(&record.bytes).map(|slot| {
            slot.map(|slot| CommitHead {
                generation: slot.generation,
                commit: slot.commit,
                commit_len: slot.commit_len,
                timestamp_ns: slot.timestamp_ns,
            })
        })
    }

    /// Publish `next` iff the plane still reports `expected` as the head.
    ///
    /// Comparison uses `generation` and `commit` only: those are the two
    /// fields that make a head unique. `commit_len` and `timestamp_ns` belong
    /// to the slot being written, not to the identity of the old head.
    ///
    /// `expected == None` means "no head yet". The slot chosen by the protocol
    /// is `next.generation % 2`; the rest of the superblock image is preserved
    /// byte for byte.
    fn compare_exchange_head(
        &mut self,
        expected: Option<&CommitHead>,
        next: &CommitHead,
    ) -> Result<()> {
        let key = Key::superblock();
        let record = self.read_control(&key)?.ok_or_else(|| {
            Error::corrupt("superblock control record is missing; the store layout is incomplete")
        })?;
        let current = highest_slot(&record.bytes)?.map(|slot| CommitHead {
            generation: slot.generation,
            commit: slot.commit,
            commit_len: slot.commit_len,
            timestamp_ns: slot.timestamp_ns,
        });
        let observed = current.as_ref().map(|head| (head.generation, head.commit));
        let wanted = expected.map(|head| (head.generation, head.commit));
        if observed != wanted {
            return Err(Error::busy(format!(
                "head publication refused: expected {}, observed {}",
                describe_head(expected),
                describe_head(current.as_ref())
            )));
        }
        let mut image = record.bytes;
        let offset = ((next.generation % 2) as usize) * SLOT_BYTES;
        let slot = SuperblockSlot {
            generation: next.generation,
            commit: next.commit,
            commit_len: next.commit_len,
            timestamp_ns: next.timestamp_ns,
        };
        image[offset..offset + SLOT_BYTES].copy_from_slice(&slot.encode());
        self.compare_exchange(&key, record.token.as_deref(), &image)?;
        Ok(())
    }

    /// Probe conditional control updates.
    ///
    /// Establishes: create-once CAS, a matching CAS, and — the load-bearing
    /// part — that a *stale* CAS token is rejected. A plane that accepts a
    /// stale token could lose a head publication silently, so it is refused
    /// with `UnsupportedVersion` rather than downgraded.
    fn probe(&mut self) -> Result<CommitProbe> {
        let label = self.commit_label().to_owned();
        let capabilities = self.commit_capabilities();
        let mut probe = CommitProbe {
            backend: label,
            attempted: false,
            token_kind: "content-token".to_owned(),
            ..CommitProbe::default()
        };
        if !capabilities.conditional_update {
            probe.detail =
                "skipped: conditional_update is declared absent, so the store refuses this plane \
                 for a writable open"
                    .to_owned();
            return Ok(probe);
        }
        probe.attempted = true;
        let key = probe_key("commit");
        let created = self.compare_exchange(&key, None, b"plane-probe-a")?;
        probe.create_ok = created.bytes == b"plane-probe-a";
        let matching = self.compare_exchange(&key, created.token.as_deref(), b"plane-probe-b")?;
        probe.matching_compare_exchange_ok = matching.bytes == b"plane-probe-b";
        let stale = self.compare_exchange(&key, created.token.as_deref(), b"plane-probe-c");
        probe.stale_compare_exchange_rejected = match &stale {
            Err(err) => err.class() == Class::Busy,
            Ok(_) => false,
        };
        // Best-effort cleanup; the stale attempt may have succeeded on a
        // broken endpoint, and the probe must not leave the scratch record.
        let _ = self.delete_control(&key);
        if !probe.create_ok || !probe.matching_compare_exchange_ok {
            return Err(Error::io(format!(
                "commit plane `{}` failed its capability probe (create_ok={}, \
                 matching_compare_exchange_ok={})",
                probe.backend, probe.create_ok, probe.matching_compare_exchange_ok
            )));
        }
        if !probe.stale_compare_exchange_rejected {
            return Err(Error::unsupported_version(format!(
                "commit plane `{}` accepted a stale compare-and-swap token; refusing a writable \
                 store because head publication could lose a race silently",
                probe.backend
            )));
        }
        probe.detail = format!(
            "create-once CAS, matching CAS, and stale-token rejection all observed on a private \
             control record; durability is attested by `{}` and is not proved here",
            capabilities.publication
        );
        Ok(probe)
    }
}

fn describe_head(head: Option<&CommitHead>) -> String {
    match head {
        None => "no head".to_owned(),
        Some(head) => format!(
            "generation {} commit {}",
            head.generation,
            head.commit.to_hex()
        ),
    }
}

/// Highest valid slot of a superblock image, or `None` when neither slot is
/// valid (the initial all-zero image).
fn highest_slot(bytes: &[u8]) -> Result<Option<SuperblockSlot>> {
    if bytes.len() != SUPERBLOCK_BYTES {
        return Err(Error::corrupt(format!(
            "superblock control record is {} bytes, expected {SUPERBLOCK_BYTES}",
            bytes.len()
        )));
    }
    let mut highest: Option<SuperblockSlot> = None;
    for outcome in decode_superblock(bytes)?.into_iter().flatten() {
        if highest
            .map(|current| outcome.generation > current.generation)
            .unwrap_or(true)
        {
            highest = Some(outcome);
        }
    }
    Ok(highest)
}

// ---------------------------------------------------------------------------
// Probe helpers that operate on a whole `StorageBackend`
// ---------------------------------------------------------------------------

/// Read a control record from any backend, with a content token.
pub(crate) fn read_control_generic(
    backend: &dyn StorageBackend,
    key: &Key,
) -> Result<Option<ControlRecord>> {
    match backend.read_all(key) {
        Ok(bytes) => Ok(Some(ControlRecord {
            token: Some(control_token(&bytes)),
            bytes,
        })),
        Err(err) if err.class() == Class::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Probe the blob half of a whole backend.
pub(crate) fn probe_blob_plane(backend: &mut dyn StorageBackend) -> Result<BlobProbe> {
    let capabilities = backend.capabilities();
    let label = backend.label().to_owned();
    let mut probe = BlobProbe {
        backend: label.clone(),
        attempted: false,
        ..BlobProbe::default()
    };
    if !capabilities.atomic_publish {
        probe.detail =
            "skipped: atomic_publish is declared absent, so the store refuses this plane wherever \
             a create-once publication is required"
                .to_owned();
        return Ok(probe);
    }
    let bytes = probe_bytes();
    probe.attempted = true;
    probe.bytes_probed = bytes.len() as u64;
    let key = probe_key("blob");
    backend.put_atomic(&key, &bytes)?;
    probe.create_once_visible = backend.read_all(&key).map(|stored| stored == bytes)?;
    probe.length_match = backend.len(&key)? == bytes.len() as u64;
    let start = bytes.len() / 3;
    let want = (bytes.len() - start).min(17);
    let part = backend.read(&key, start as u64, want as u64)?;
    probe.range_read_match = part == bytes[start..start + want];
    backend.delete(&key)?;
    probe.delete_observed = !backend.exists(&key)?;
    if !probe.passed() {
        return Err(Error::io(format!(
            "blob plane `{label}` failed its capability probe (create_once_visible={}, \
             length_match={}, range_read_match={}, delete_observed={})",
            probe.create_once_visible,
            probe.length_match,
            probe.range_read_match,
            probe.delete_observed
        )));
    }
    probe.detail = format!(
        "published {n} bytes under a private key, HEAD reported that length, a ranged read of \
         {want} bytes matched, and delete was observed; durability is attested by `{publication}` \
         and is not proved here",
        n = probe.bytes_probed,
        publication = capabilities.publication
    );
    Ok(probe)
}

/// Probe the commit half of a whole backend.
pub(crate) fn probe_commit_plane(backend: &mut dyn StorageBackend) -> Result<CommitProbe> {
    let capabilities = backend.capabilities();
    let label = backend.label().to_owned();
    let mut probe = CommitProbe {
        backend: label,
        attempted: false,
        token_kind: "content-token".to_owned(),
        ..CommitProbe::default()
    };
    if !(capabilities.atomic_publish && capabilities.conditional_head) {
        probe.detail =
            "skipped: conditional_update is declared absent, so the store refuses this plane for a \
             writable open"
                .to_owned();
        return Ok(probe);
    }
    probe.attempted = true;
    let key = probe_key("commit");
    backend.compare_and_swap(&key, None, b"plane-probe-a")?;
    probe.create_ok = backend.read_all(&key)? == b"plane-probe-a";
    let token = control_token(b"plane-probe-a");
    backend.compare_and_swap(&key, Some(&token), b"plane-probe-b")?;
    probe.matching_compare_exchange_ok = backend.read_all(&key)? == b"plane-probe-b";
    let stale = backend.compare_and_swap(&key, Some(&token), b"plane-probe-c");
    probe.stale_compare_exchange_rejected = match &stale {
        Err(err) => err.class() == Class::Busy,
        Ok(()) => false,
    };
    let _ = backend.delete(&key);
    if !probe.create_ok || !probe.matching_compare_exchange_ok {
        return Err(Error::io(format!(
            "commit plane `{}` failed its capability probe",
            probe.backend
        )));
    }
    if !probe.stale_compare_exchange_rejected {
        return Err(Error::unsupported_version(format!(
            "commit plane `{}` accepted a stale compare-and-swap token; refusing a writable store \
             because head publication could lose a race silently",
            probe.backend
        )));
    }
    probe.detail = format!(
        "create-once CAS, matching CAS, and stale-token rejection all observed on a private \
         control record; durability is attested by `{}` and is not proved here",
        capabilities.publication
    );
    Ok(probe)
}

// ---------------------------------------------------------------------------
// Concrete implementations
// ---------------------------------------------------------------------------

/// Implement both planes for a type that already implements
/// [`StorageBackend`]. Every method delegates to the storage primitive, so the
/// plane contract and the legacy contract can never drift apart: there is one
/// implementation of each physical operation.
macro_rules! impl_planes_for_storage_backend {
    ($type:ty, $label:expr) => {
        impl BlobPlane for $type {
            fn blob_label(&self) -> &'static str {
                $label
            }

            fn blob_capabilities(&self) -> BlobCapabilities {
                BlobCapabilities::from_capabilities($label, &StorageBackend::capabilities(self))
            }

            fn get(&self, key: &Key) -> Result<Vec<u8>> {
                StorageBackend::read_all(self, key)
            }

            fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
                StorageBackend::read(self, key, offset, len)
            }

            fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
                StorageBackend::put_atomic(self, key, bytes)
            }

            fn delete(&mut self, key: &Key) -> Result<()> {
                StorageBackend::delete(self, key)
            }

            fn len(&self, key: &Key) -> Result<u64> {
                StorageBackend::len(self, key)
            }

            fn exists(&self, key: &Key) -> Result<bool> {
                StorageBackend::exists(self, key)
            }

            fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
                StorageBackend::list_prefix(self, prefix)
            }

            fn clean_stale(&mut self, grace_secs: u64) -> Result<()> {
                StorageBackend::clean_stale_tmp(self, grace_secs)
            }

            fn init_layout(&mut self) -> Result<()> {
                StorageBackend::init_layout(self)
            }

            fn ensure_layout(&mut self) -> Result<()> {
                StorageBackend::ensure_layout(self)
            }

            fn stats(&self) -> BackendStats {
                StorageBackend::stats(self)
            }

            fn diagnostics(&self) -> Vec<String> {
                StorageBackend::diagnostics(self)
            }

            fn root(&self) -> &Path {
                StorageBackend::root(self)
            }
        }

        impl CommitPlane for $type {
            fn commit_label(&self) -> &'static str {
                $label
            }

            fn commit_capabilities(&self) -> CommitCapabilities {
                CommitCapabilities::from_capabilities($label, &StorageBackend::capabilities(self))
            }

            fn read_control(&self, key: &Key) -> Result<Option<ControlRecord>> {
                read_control_generic(self, key)
            }

            fn compare_exchange(
                &mut self,
                key: &Key,
                expected: Option<&str>,
                bytes: &[u8],
            ) -> Result<ControlRecord> {
                StorageBackend::compare_and_swap(self, key, expected, bytes)?;
                Ok(ControlRecord {
                    token: Some(control_token(bytes)),
                    bytes: bytes.to_vec(),
                })
            }

            fn write_control(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
                StorageBackend::put_replace(self, key, bytes)
            }

            fn create_control(&mut self, key: &Key, len: u64) -> Result<()> {
                StorageBackend::create_zeroed(self, key, len)
            }

            fn put_control_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
                StorageBackend::put_atomic(self, key, bytes)
            }

            fn delete_control(&mut self, key: &Key) -> Result<()> {
                StorageBackend::delete(self, key)
            }

            fn control_len(&self, key: &Key) -> Result<u64> {
                StorageBackend::len(self, key)
            }

            fn control_exists(&self, key: &Key) -> Result<bool> {
                StorageBackend::exists(self, key)
            }

            fn list_control(&self, prefix: &Key) -> Result<Vec<Key>> {
                StorageBackend::list_prefix(self, prefix)
            }

            fn modified(&self, key: &Key) -> Result<Option<SystemTime>> {
                StorageBackend::modified(self, key)
            }

            fn sync_dir(&mut self, key: &Key) -> Result<()> {
                StorageBackend::sync_dir(self, key)
            }

            fn init_layout(&mut self) -> Result<()> {
                StorageBackend::init_layout(self)
            }

            fn ensure_layout(&mut self) -> Result<()> {
                StorageBackend::ensure_layout(self)
            }

            fn acquire_lock(&mut self) -> Result<()> {
                StorageBackend::acquire_writer_lock(self)
            }

            fn clean_stale(&mut self, grace_secs: u64) -> Result<()> {
                StorageBackend::clean_stale_tmp(self, grace_secs)
            }

            fn stats(&self) -> BackendStats {
                StorageBackend::stats(self)
            }

            fn diagnostics(&self) -> Vec<String> {
                StorageBackend::diagnostics(self)
            }

            fn root(&self) -> &Path {
                StorageBackend::root(self)
            }
        }
    };
}

impl_planes_for_storage_backend!(crate::backend::LocalBackend, "local");
impl_planes_for_storage_backend!(crate::backend::ObjectBackend, "object");
impl_planes_for_storage_backend!(crate::s3::S3Backend, "s3");
