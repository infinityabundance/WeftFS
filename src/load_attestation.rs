//! `weft.load-attestation.v1`: a **signed** record of one replica's load.
//!
//! Subphase R12, increment 3. The shipped [`crate::load_receipt`] already binds
//! a verified load to the checkpoint manifest and to the loaded tensor roots.
//! This module adds what an *attestation* needs on top:
//!
//! * a **verifier identity** (principal label + Ed25519 public key) and an
//!   **issue timestamp**;
//! * the **exact logical byte ranges** that were read, each with its own
//!   domain-separated digest, and the **groups** (checkpoint shard objects)
//!   whose roots were verified while consuming those ranges;
//! * an explicit list of the tensors that were **not loaded**, reported as
//!   `unverified` with a reason rather than silently omitted;
//! * an Ed25519 signature over the canonical record (without its signature).
//!
//! The signature uses the same primitive and encoding as the retention binding
//! and the receipt bundle ([`weftcodec::capability::sign_bytes`]): Ed25519 over
//! the canonical WCB bytes of the record. WeftFS-Go verifies that same
//! construction with `crypto/ed25519`, so the signature format is shared; the
//! *byte re-verification against a store* is Rust-only because WeftFS-Go has no
//! `weft.checkpoint.v1` reader.
//!
//! ## What a signature does and does not prove
//!
//! It proves the record was produced by the holder of the seed and has not been
//! altered. It does **not** prove that the measurement is true: a signer can
//! attest a lie. [`verify_against_store`] is the separate check that re-reads
//! every recorded range from a store and fails closed on any mismatch. A range
//! the loader never read cannot be verified by either layer.

use weftcodec::tensor::Region;
use weftcodec::wcb::Value;

use crate::checkpoint::CheckpointTensor;
use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::store::Store;
use crate::wcb_util as wu;

/// Canonical schema string.
pub const LOAD_ATTESTATION_SCHEMA: &str = "weft.load-attestation.v1";
/// Domain for the attestation identity over its full canonical bytes.
pub const LOAD_ATTESTATION_DOMAIN: &str = "weft/load/attestation/v1";
/// Domain for one loaded range's digest.
pub const LOAD_RANGE_DOMAIN: &str = "weft/load/range/v1";

/// Load status of one tensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestStatus {
    /// The loader read these bytes and verified the group roots covering them.
    Loaded,
    /// The loader did not read this tensor (or part of it). Never counted as
    /// verified.
    Unverified,
}

impl AttestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AttestStatus::Loaded => "loaded",
            AttestStatus::Unverified => "unverified",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "loaded" => Ok(AttestStatus::Loaded),
            "unverified" => Ok(AttestStatus::Unverified),
            other => Err(Error::malformed(format!(
                "load attestation status `{other}` is not `loaded` or `unverified`"
            ))),
        }
    }
}

/// One contiguous logical byte range the loader consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttestedRange {
    pub offset: u64,
    pub length: u64,
    /// `H("weft/load/range/v1", range_bytes)`.
    pub digest: Hash32,
}

/// One checkpoint shard group whose roots the loader verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttestedGroup {
    pub object: Hash32,
    pub content_root: Hash32,
    pub logical_root: Hash32,
    pub axis: u32,
    pub index: u32,
    pub byte_len: u64,
}

/// One tensor's attestation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestedTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    /// `LogicalTensorRoot` recorded by the checkpoint manifest.
    pub logical_root: Hash32,
    /// Element bytes of the whole tensor, from the manifest.
    pub element_bytes: u64,
    pub status: AttestStatus,
    pub bytes_loaded: u64,
    /// Sorted by offset, non-overlapping.
    pub ranges: Vec<AttestedRange>,
    /// Groups read while consuming the ranges (all shards for a whole-tensor
    /// load; the intersecting shards for a row region).
    pub groups: Vec<AttestedGroup>,
    /// Tiles touched inside those groups (mode 15/17) plus shards read.
    pub units_verified: u64,
    /// True only when the loader recomputed the whole-tensor root.
    pub full_root_verified: bool,
    /// Why this tensor is unverified (`not requested`, ...). Required for
    /// [`AttestStatus::Unverified`], absent for a loaded tensor.
    pub reason: Option<String>,
}

/// One verifier's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierIdentity {
    /// Caller-declared principal label (host/process/tenant).
    pub principal: String,
    /// The key that signed this record.
    pub public_key: Hash32,
}

/// Optional Ed25519 signature over the record without its `signature` key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadAttestationSignature {
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

impl LoadAttestationSignature {
    fn to_value(&self) -> Value {
        wu::map([
            ("algorithm", Value::Str("ed25519".to_owned())),
            ("public_key", Value::Bytes(self.public_key.to_vec())),
            ("signature", Value::Bytes(self.signature.to_vec())),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["algorithm", "public_key", "signature"])?;
        let algorithm = wu::get_text(map, "algorithm")?;
        if algorithm != "ed25519" {
            return Err(Error::malformed(format!(
                "load attestation signature algorithm `{algorithm}` is not `ed25519`"
            )));
        }
        let public_key: [u8; 32] = wu::get_bytes(map, "public_key")?
            .try_into()
            .map_err(|_| Error::malformed("load attestation public_key must be 32 bytes"))?;
        let signature: [u8; 64] = wu::get_bytes(map, "signature")?
            .try_into()
            .map_err(|_| Error::malformed("load attestation signature must be 64 bytes"))?;
        Ok(LoadAttestationSignature {
            public_key,
            signature,
        })
    }
}

/// Totals, recomputed and re-checked on decode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttestationTotals {
    pub loaded_tensors: u64,
    pub unverified_tensors: u64,
    pub loaded_bytes: u64,
    pub unverified_bytes: u64,
}

/// One signed load attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadAttestation {
    pub checkpoint: String,
    /// The checkpoint manifest root (the checkpoint's own identity).
    pub checkpoint_root: Hash32,
    /// Equal to `checkpoint_root`; kept explicit because the task binds both.
    pub manifest_root: Hash32,
    pub snapshot: Option<Hash32>,
    pub replica: String,
    pub verifier: VerifierIdentity,
    /// Absolute timestamp in nanoseconds (WCB timestamp).
    pub issued_at_ns: i64,
    /// Sorted by tensor name.
    pub tensors: Vec<AttestedTensor>,
    pub totals: AttestationTotals,
    /// Root of the shipped `weft.load.receipt.v1`, when one was produced.
    pub load_receipt_root: Option<Hash32>,
    pub signature: Option<LoadAttestationSignature>,
}

/// Options for [`attest_checkpoint_load`].
#[derive(Clone, Debug)]
pub struct AttestOptions {
    pub replica: String,
    pub principal: String,
    /// `None` selects every tensor; `Some(list)` selects by exact name.
    pub tensors: Option<Vec<String>>,
    /// Load only this region of each selected tensor. Must be a **row range**
    /// on a rank-2 tensor (`start[1..] == 0`, `end[1..] == shape[1..]`), so the
    /// range is one contiguous logical interval.
    pub region: Option<Region>,
    pub snapshot: Option<Hash32>,
    pub issued_at_ns: Option<i64>,
    pub seed: Option<[u8; 32]>,
}

impl Default for AttestOptions {
    fn default() -> Self {
        AttestOptions {
            replica: "local".to_owned(),
            principal: "weftfs".to_owned(),
            tensors: None,
            region: None,
            snapshot: None,
            issued_at_ns: None,
            seed: None,
        }
    }
}

/// Result of [`verify_against_store`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttestationVerification {
    pub signature_verified: bool,
    pub verified_tensors: u64,
    pub unverified_tensors: u64,
    /// Bytes re-read and re-hashed during this verification.
    pub replayed_bytes: u64,
    pub ranges_replayed: u64,
    /// Tensors the attestation itself reports as not loaded. Listed, never
    /// counted as verified.
    pub unverified: Vec<(String, String)>,
    pub wall_ms: u64,
}

impl LoadAttestation {
    /// Per-tensor entry order is canonical: sorted by name.
    fn canonicalize(&mut self) {
        self.tensors.sort_by(|a, b| a.name.cmp(&b.name));
        for tensor in &mut self.tensors {
            tensor.ranges.sort_by_key(|range| range.offset);
            tensor.groups.sort_by_key(|group| (group.axis, group.index));
        }
        let mut totals = AttestationTotals::default();
        for tensor in &self.tensors {
            match tensor.status {
                AttestStatus::Loaded => {
                    totals.loaded_tensors += 1;
                    totals.loaded_bytes = totals.loaded_bytes.saturating_add(tensor.bytes_loaded);
                }
                AttestStatus::Unverified => {
                    totals.unverified_tensors += 1;
                    totals.unverified_bytes = totals
                        .unverified_bytes
                        .saturating_add(tensor.element_bytes.saturating_sub(tensor.bytes_loaded));
                }
            }
        }
        self.totals = totals;
    }

    /// The exact bytes a signature covers: the record without `signature`.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value(false))
    }

    /// Canonical WCB bytes, signature included when present.
    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value(true))
    }

    /// `H(LOAD_ATTESTATION_DOMAIN, canonical bytes)`.
    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(LOAD_ATTESTATION_DOMAIN, &self.encode()?))
    }

    /// Sign in place. The verifier identity's public key is replaced by the
    /// signing key's public key, because that is the identity the record proves.
    pub fn sign(&mut self, seed: &[u8; 32]) -> Result<()> {
        let public_key = weftcodec::capability::public_key_for_seed(seed)?;
        self.verifier.public_key = public_key;
        let signature = weftcodec::capability::sign_bytes(seed, &self.signing_bytes()?)?;
        let signature: [u8; 64] = signature
            .try_into()
            .map_err(|_| Error::malformed("Ed25519 signature is not 64 bytes"))?;
        self.signature = Some(LoadAttestationSignature {
            public_key: *public_key.as_bytes(),
            signature,
        });
        Ok(())
    }

    /// Verify the signature against its embedded key.
    pub fn verify_signature(&self) -> Result<()> {
        let Some(signature) = &self.signature else {
            return Ok(());
        };
        if signature.public_key != *self.verifier.public_key.as_bytes() {
            return Err(Error::corrupt(
                "load attestation signature key does not match the verifier identity",
            ));
        }
        weftcodec::capability::verify_bytes(
            &self.verifier.public_key,
            &self.signing_bytes()?,
            &signature.signature,
        )
        .map_err(|_| Error::corrupt("load attestation signature does not verify"))
    }

    /// Verify the signature and require the key to be in `trust`.
    pub fn verify_signature_with_trust(&self, trust: &[[u8; 32]]) -> Result<()> {
        if trust.is_empty() {
            return self.verify_signature();
        }
        let Some(signature) = &self.signature else {
            return Err(Error::unauthorized(
                "load attestation is unsigned and this verifier requires a trusted signer",
            ));
        };
        self.verify_signature()?;
        if trust.iter().any(|key| key == &signature.public_key) {
            Ok(())
        } else {
            Err(Error::unauthorized(
                "load attestation signer is not in the configured trust set",
            ))
        }
    }

    pub fn to_value(&self, include_signature: bool) -> Value {
        let tensors = self
            .tensors
            .iter()
            .map(|tensor| {
                wu::map([
                    ("name", Value::Str(tensor.name.clone())),
                    ("dtype", Value::Str(tensor.dtype.clone())),
                    (
                        "shape",
                        Value::Array(tensor.shape.iter().map(|dim| Value::U64(*dim)).collect()),
                    ),
                    ("logical_root", wu::hash_value(tensor.logical_root)),
                    ("element_bytes", Value::U64(tensor.element_bytes)),
                    ("status", Value::Str(tensor.status.as_str().to_owned())),
                    ("bytes_loaded", Value::U64(tensor.bytes_loaded)),
                    (
                        "ranges",
                        Value::Array(
                            tensor
                                .ranges
                                .iter()
                                .map(|range| {
                                    wu::map([
                                        ("offset", Value::U64(range.offset)),
                                        ("length", Value::U64(range.length)),
                                        ("digest", wu::hash_value(range.digest)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "groups",
                        Value::Array(
                            tensor
                                .groups
                                .iter()
                                .map(|group| {
                                    wu::map([
                                        ("object", wu::hash_value(group.object)),
                                        ("content_root", wu::hash_value(group.content_root)),
                                        ("logical_root", wu::hash_value(group.logical_root)),
                                        ("axis", Value::U64(u64::from(group.axis))),
                                        ("index", Value::U64(u64::from(group.index))),
                                        ("byte_len", Value::U64(group.byte_len)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    ("units_verified", Value::U64(tensor.units_verified)),
                    ("full_root_verified", Value::Bool(tensor.full_root_verified)),
                    (
                        "reason",
                        match &tensor.reason {
                            Some(reason) => Value::Str(reason.clone()),
                            None => Value::Null,
                        },
                    ),
                ])
            })
            .collect();
        let mut pairs = vec![
            ("schema", Value::Str(LOAD_ATTESTATION_SCHEMA.to_owned())),
            ("checkpoint", Value::Str(self.checkpoint.clone())),
            ("checkpoint_root", wu::hash_value(self.checkpoint_root)),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            ("snapshot", wu::opt_hash_value(self.snapshot)),
            ("replica", Value::Str(self.replica.clone())),
            (
                "verifier",
                wu::map([
                    ("principal", Value::Str(self.verifier.principal.clone())),
                    ("public_key", wu::hash_value(self.verifier.public_key)),
                ]),
            ),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
            ("tensors", Value::Array(tensors)),
            (
                "totals",
                wu::map([
                    ("loaded_tensors", Value::U64(self.totals.loaded_tensors)),
                    (
                        "unverified_tensors",
                        Value::U64(self.totals.unverified_tensors),
                    ),
                    ("loaded_bytes", Value::U64(self.totals.loaded_bytes)),
                    ("unverified_bytes", Value::U64(self.totals.unverified_bytes)),
                ]),
            ),
            (
                "load_receipt_root",
                wu::opt_hash_value(self.load_receipt_root),
            ),
        ];
        if include_signature {
            pairs.push((
                "signature",
                match &self.signature {
                    Some(signature) => signature.to_value(),
                    None => Value::Null,
                },
            ));
        }
        wu::map(pairs)
    }

    /// Decode and validate canonical bytes. Recomputes every total and rejects
    /// a record whose rosters, ranges or groups are inconsistent.
    pub fn decode(bytes: &[u8]) -> Result<LoadAttestation> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        // The signing form omits `signature`; the stored form includes it.
        let has_signature = map.contains_key("signature");
        wu::check_keys(
            map,
            &[
                "schema",
                "checkpoint",
                "checkpoint_root",
                "manifest_root",
                "snapshot",
                "replica",
                "verifier",
                "issued_at",
                "tensors",
                "totals",
                "load_receipt_root",
                "signature",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != LOAD_ATTESTATION_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "load attestation schema `{schema}` is not `{LOAD_ATTESTATION_SCHEMA}`"
            )));
        }
        let verifier_map = wu::get_map(map, "verifier")?;
        wu::check_keys(&verifier_map, &["principal", "public_key"])?;
        let public_key = wu::get_hash(&verifier_map, "public_key")?;
        let mut tensors = Vec::new();
        for entry in wu::get_array(map, "tensors")? {
            let entry = wu::as_map(entry)?;
            wu::check_keys(
                entry,
                &[
                    "name",
                    "dtype",
                    "shape",
                    "logical_root",
                    "element_bytes",
                    "status",
                    "bytes_loaded",
                    "ranges",
                    "groups",
                    "units_verified",
                    "full_root_verified",
                    "reason",
                ],
            )?;
            let mut shape = Vec::new();
            for dim in wu::get_array(entry, "shape")? {
                match dim {
                    Value::U64(value) => shape.push(*value),
                    other => {
                        return Err(Error::malformed(format!(
                            "load attestation shape entry is not u64: {other:?}"
                        )))
                    }
                }
            }
            let mut ranges = Vec::new();
            for range in wu::get_array(entry, "ranges")? {
                let range = wu::as_map(range)?;
                wu::check_keys(range, &["offset", "length", "digest"])?;
                ranges.push(AttestedRange {
                    offset: wu::get_u64(range, "offset")?,
                    length: wu::get_u64(range, "length")?,
                    digest: wu::get_hash(range, "digest")?,
                });
            }
            let mut groups = Vec::new();
            for group in wu::get_array(entry, "groups")? {
                let group = wu::as_map(group)?;
                wu::check_keys(
                    group,
                    &[
                        "object",
                        "content_root",
                        "logical_root",
                        "axis",
                        "index",
                        "byte_len",
                    ],
                )?;
                groups.push(AttestedGroup {
                    object: wu::get_hash(group, "object")?,
                    content_root: wu::get_hash(group, "content_root")?,
                    logical_root: wu::get_hash(group, "logical_root")?,
                    axis: wu::get_u64(group, "axis")? as u32,
                    index: wu::get_u64(group, "index")? as u32,
                    byte_len: wu::get_u64(group, "byte_len")?,
                });
            }
            let full_root_verified = match wu::field(entry, "full_root_verified")? {
                Value::Bool(value) => *value,
                other => {
                    return Err(Error::malformed(format!(
                        "load attestation full_root_verified is not a bool: {other:?}"
                    )))
                }
            };
            let status = AttestStatus::parse(&wu::get_text(entry, "status")?)?;
            let reason = match wu::field(entry, "reason")? {
                Value::Null => None,
                Value::Str(text) => Some(text.clone()),
                other => {
                    return Err(Error::malformed(format!(
                        "load attestation reason is not a string: {other:?}"
                    )))
                }
            };
            if status == AttestStatus::Unverified && reason.is_none() {
                return Err(Error::corrupt(
                    "load attestation lists an unverified tensor without a reason",
                ));
            }
            if status == AttestStatus::Loaded && (ranges.is_empty() || groups.is_empty()) {
                return Err(Error::corrupt(
                    "load attestation lists a loaded tensor with no ranges or groups",
                ));
            }
            let mut previous_end: Option<u64> = None;
            for range in &ranges {
                if range.length == 0 {
                    return Err(Error::corrupt("load attestation has a zero-length range"));
                }
                if let Some(end) = previous_end {
                    if range.offset < end {
                        return Err(Error::corrupt(
                            "load attestation ranges overlap or are not sorted",
                        ));
                    }
                }
                previous_end = Some(range.offset.saturating_add(range.length));
            }
            tensors.push(AttestedTensor {
                name: wu::get_text(entry, "name")?,
                dtype: wu::get_text(entry, "dtype")?,
                shape,
                logical_root: wu::get_hash(entry, "logical_root")?,
                element_bytes: wu::get_u64(entry, "element_bytes")?,
                status,
                bytes_loaded: wu::get_u64(entry, "bytes_loaded")?,
                ranges,
                groups,
                units_verified: wu::get_u64(entry, "units_verified")?,
                full_root_verified,
                reason,
            });
        }
        let totals_map = wu::get_map(map, "totals")?;
        wu::check_keys(
            &totals_map,
            &[
                "loaded_tensors",
                "unverified_tensors",
                "loaded_bytes",
                "unverified_bytes",
            ],
        )?;
        let totals = AttestationTotals {
            loaded_tensors: wu::get_u64(&totals_map, "loaded_tensors")?,
            unverified_tensors: wu::get_u64(&totals_map, "unverified_tensors")?,
            loaded_bytes: wu::get_u64(&totals_map, "loaded_bytes")?,
            unverified_bytes: wu::get_u64(&totals_map, "unverified_bytes")?,
        };
        let signature = match wu::field(map, "signature")? {
            Value::Null => None,
            value => Some(LoadAttestationSignature::from_value(value)?),
        };
        let attestation = LoadAttestation {
            checkpoint: wu::get_text(map, "checkpoint")?,
            checkpoint_root: wu::get_hash(map, "checkpoint_root")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            snapshot: wu::get_opt_hash(map, "snapshot")?,
            replica: wu::get_text(map, "replica")?,
            verifier: VerifierIdentity {
                principal: wu::get_text(&verifier_map, "principal")?,
                public_key,
            },
            issued_at_ns: wu::get_timestamp(map, "issued_at")?,
            tensors,
            totals,
            load_receipt_root: wu::get_opt_hash(map, "load_receipt_root")?,
            signature,
        };
        if attestation.checkpoint_root != attestation.manifest_root {
            return Err(Error::corrupt(
                "load attestation checkpoint_root and manifest_root disagree",
            ));
        }
        // Recompute the totals from the entries. The stored totals are only a
        // convenience; a mismatch is a corrupt record.
        let mut recomputed = attestation.clone();
        recomputed.canonicalize();
        if recomputed.totals != totals {
            return Err(Error::corrupt(
                "load attestation totals do not match its tensor entries",
            ));
        }
        for pair in attestation.tensors.windows(2) {
            if pair[0].name >= pair[1].name {
                return Err(Error::non_canonical(
                    "load attestation tensors are not strictly sorted by name",
                ));
            }
        }
        if attestation.signature.is_some() != has_signature {
            return Err(Error::corrupt(
                "load attestation signature presence mismatch",
            ));
        }
        Ok(attestation)
    }
}

/// Load the selected tensors of a checkpoint, verifying group roots as they are
/// consumed, and build a (possibly signed) attestation.
pub fn attest_checkpoint_load(
    store: &Store,
    name: &str,
    options: &AttestOptions,
) -> Result<LoadAttestation> {
    let manifest = match options.snapshot {
        Some(snapshot) => crate::checkpoint::checkpoint_manifest_at(store, snapshot, name)?,
        None => store.load_checkpoint(name)?,
    };
    let manifest_root = manifest.root()?;
    let mut entries = Vec::with_capacity(manifest.tensors.len());
    for tensor in &manifest.tensors {
        let selected = match &options.tensors {
            None => true,
            Some(list) => list.iter().any(|name| name == &tensor.name),
        };
        if !selected {
            entries.push(unverified_entry(tensor, "not requested"));
            continue;
        }
        let region = match &options.region {
            None => None,
            Some(region) => {
                if tensor.shape.len() != 2 {
                    return Err(Error::invalid_argument(format!(
                        "an attested region requires a rank-2 tensor; `{}` has rank {}",
                        tensor.name,
                        tensor.shape.len()
                    )));
                }
                region.validate(&tensor.shape).map_err(|err| {
                    Error::invalid_argument(format!(
                        "attested region for `{}` is invalid: {err}",
                        tensor.name
                    ))
                })?;
                if region.start[1] != 0 || region.end[1] != tensor.shape[1] {
                    return Err(Error::invalid_argument(
                        "an attested region must cover whole rows (start[1]=0, end[1]=shape[1])",
                    ));
                }
                Some(region.clone())
            }
        };
        let restore = crate::checkpoint::CheckpointRestoreOptions {
            tensor: Some(tensor.name.clone()),
            region: region.clone(),
            topology: None,
        };
        let (bytes, report) = match options.snapshot {
            Some(snapshot) => store.restore_checkpoint_bytes_at(snapshot, name, &restore)?,
            None => store.restore_checkpoint_bytes(name, &restore)?,
        };
        let restored = report
            .tensors
            .first()
            .ok_or_else(|| Error::corrupt("restore report has no tensor"))?;
        // `restore_checkpoint` produces a complete safetensors container; the
        // attested bytes are the tensor payload after the synthesized header.
        let payload = container_payload(&bytes)?;
        if payload.len() as u64 != restored.bytes {
            return Err(Error::corrupt(format!(
                "restored payload for `{}` is {} bytes but the report says {}",
                tensor.name,
                payload.len(),
                restored.bytes
            )));
        }
        let digest = identity::h(LOAD_RANGE_DOMAIN, payload);
        let offset = match &restore.region {
            None => 0,
            Some(region) => row_range_offset(tensor, region)?,
        };
        let groups = match region.as_ref() {
            None => tensor
                .shards
                .iter()
                .map(|shard| AttestedGroup {
                    object: shard.object_id,
                    content_root: shard.content_root,
                    logical_root: shard.logical_root,
                    axis: shard.axis,
                    index: shard.index,
                    byte_len: shard.byte_len,
                })
                .collect::<Vec<_>>(),
            Some(region) => intersecting_groups(tensor, region),
        };
        entries.push(AttestedTensor {
            name: tensor.name.clone(),
            dtype: tensor.dtype.clone(),
            shape: tensor.shape.clone(),
            logical_root: tensor.logical_root,
            element_bytes: tensor.element_bytes,
            status: AttestStatus::Loaded,
            bytes_loaded: payload.len() as u64,
            ranges: vec![AttestedRange {
                offset,
                length: payload.len() as u64,
                digest,
            }],
            groups,
            units_verified: restored
                .units_touched
                .saturating_add(restored.shards_read as u64),
            full_root_verified: restored.full_root_verified,
            reason: None,
        });
    }
    let issued_at_ns = match options.issued_at_ns {
        Some(value) => value,
        None => store.clock_now_ns(),
    };
    let mut attestation = LoadAttestation {
        checkpoint: manifest.name.clone(),
        checkpoint_root: manifest_root,
        manifest_root,
        snapshot: options.snapshot,
        replica: options.replica.clone(),
        verifier: VerifierIdentity {
            principal: options.principal.clone(),
            public_key: Hash32::ZERO,
        },
        issued_at_ns,
        tensors: entries,
        totals: AttestationTotals::default(),
        load_receipt_root: None,
        signature: None,
    };
    attestation.canonicalize();
    if let Some(seed) = &options.seed {
        attestation.sign(seed)?;
    }
    Ok(attestation)
}

fn unverified_entry(tensor: &CheckpointTensor, reason: &str) -> AttestedTensor {
    AttestedTensor {
        name: tensor.name.clone(),
        dtype: tensor.dtype.clone(),
        shape: tensor.shape.clone(),
        logical_root: tensor.logical_root,
        element_bytes: tensor.element_bytes,
        status: AttestStatus::Unverified,
        bytes_loaded: 0,
        ranges: Vec::new(),
        groups: Vec::new(),
        units_verified: 0,
        full_root_verified: false,
        reason: Some(reason.to_owned()),
    }
}

/// Byte offset of a whole-row region in the canonical row-major tensor.
fn row_range_offset(tensor: &CheckpointTensor, region: &Region) -> Result<u64> {
    let row_bytes = tensor
        .element_bytes
        .checked_div(tensor.shape.first().copied().unwrap_or(1).max(1))
        .ok_or_else(|| Error::corrupt("checkpoint tensor has no element bytes"))?;
    Ok(region.start[0].saturating_mul(row_bytes))
}

/// The shard groups a whole-row region intersects along the split axis.
fn intersecting_groups(tensor: &CheckpointTensor, region: &Region) -> Vec<AttestedGroup> {
    let axis = tensor.shards.first().map(|shard| shard.axis).unwrap_or(0) as usize;
    let shards = tensor.shards.first().map(|shard| shard.shards).unwrap_or(1) as u64;
    let extent = tensor.shape.get(axis).copied().unwrap_or(1);
    let per = if shards == 0 { extent } else { extent / shards };
    tensor
        .shards
        .iter()
        .filter(|shard| {
            let start = u64::from(shard.index).saturating_mul(per);
            let end = start.saturating_add(per);
            region.start[axis] < end && region.end[axis] > start
        })
        .map(|shard| AttestedGroup {
            object: shard.object_id,
            content_root: shard.content_root,
            logical_root: shard.logical_root,
            axis: shard.axis,
            index: shard.index,
            byte_len: shard.byte_len,
        })
        .collect()
}

/// Options for [`verify_against_store`].
#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    /// When non-empty, the signer must be in this set.
    pub trust: Vec<[u8; 32]>,
    /// Require a signature at all.
    pub require_signature: bool,
}

/// Re-verify an attestation against a store: signature, then every recorded
/// range re-read and re-hashed through the shipped restore path.
///
/// A tampered receipt fails the signature; a tampered store fails the re-read
/// (the store itself verifies group roots, so a modified blob is `Corrupt`).
/// Tensors the attestation marks `unverified` are returned in
/// [`AttestationVerification::unverified`] and are never counted as verified.
pub fn verify_against_store(
    store: &Store,
    attestation: &LoadAttestation,
    options: &VerifyOptions,
) -> Result<AttestationVerification> {
    let started = std::time::Instant::now();
    if attestation.signature.is_none() && options.require_signature {
        return Err(Error::unauthorized(
            "load attestation is unsigned and verification requires a signature",
        ));
    }
    attestation.verify_signature_with_trust(&options.trust)?;
    // The record's own roots must agree with a fresh manifest read.
    let manifest = match attestation.snapshot {
        Some(snapshot) => {
            crate::checkpoint::checkpoint_manifest_at(store, snapshot, &attestation.checkpoint)?
        }
        None => store.load_checkpoint(&attestation.checkpoint)?,
    };
    let manifest_root = manifest.root()?;
    if manifest_root != attestation.manifest_root {
        return Err(Error::corrupt(format!(
            "load attestation manifest_root {} does not match the store's {}",
            attestation.manifest_root, manifest_root
        )));
    }
    let mut result = AttestationVerification {
        signature_verified: attestation.signature.is_some()
            || options.trust.is_empty() && !options.require_signature,
        ..AttestationVerification::default()
    };
    for entry in &attestation.tensors {
        let manifest_tensor = manifest
            .tensors
            .iter()
            .find(|tensor| tensor.name == entry.name)
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "load attestation names `{}`, which the checkpoint does not contain",
                    entry.name
                ))
            })?;
        if manifest_tensor.logical_root != entry.logical_root {
            return Err(Error::corrupt(format!(
                "load attestation logical root for `{}` does not match the manifest",
                entry.name
            )));
        }
        if entry.status == AttestStatus::Unverified {
            result.unverified_tensors += 1;
            result
                .unverified
                .push((entry.name.clone(), entry.reason.clone().unwrap_or_default()));
            continue;
        }
        // Re-read exactly the recorded range(s). Each range must re-hash to the
        // digest the signer recorded.
        for range in &entry.ranges {
            let region = region_for_range(manifest_tensor, range)?;
            let restore = crate::checkpoint::CheckpointRestoreOptions {
                tensor: Some(entry.name.clone()),
                region,
                topology: None,
            };
            let (bytes, report) = match attestation.snapshot {
                Some(snapshot) => store.restore_checkpoint_bytes_at(
                    snapshot,
                    &attestation.checkpoint,
                    &restore,
                )?,
                None => store.restore_checkpoint_bytes(&attestation.checkpoint, &restore)?,
            };
            let payload = container_payload(&bytes)?;
            let digest = identity::h(LOAD_RANGE_DOMAIN, payload);
            if digest != range.digest {
                return Err(Error::corrupt(format!(
                    "load attestation range {}..{} of `{}` re-hashes to {} but the receipt records {}",
                    range.offset,
                    range.offset.saturating_add(range.length),
                    entry.name,
                    digest,
                    range.digest
                )));
            }
            if payload.len() as u64 != range.length {
                return Err(Error::corrupt(format!(
                    "load attestation range length for `{}` is {} but the store produced {} bytes",
                    entry.name,
                    range.length,
                    payload.len()
                )));
            }
            let restored = report
                .tensors
                .first()
                .ok_or_else(|| Error::corrupt("restore report has no tensor"))?;
            if entry.full_root_verified && !restored.full_root_verified {
                return Err(Error::corrupt(format!(
                    "load attestation claims a verified whole-tensor root for `{}` but the re-read did not prove it",
                    entry.name
                )));
            }
            result.ranges_replayed += 1;
            result.replayed_bytes = result.replayed_bytes.saturating_add(payload.len() as u64);
        }
        result.verified_tensors += 1;
    }
    result.wall_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

/// The tensor payload of a restored safetensors container.
fn container_payload(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < 8 {
        return Err(Error::corrupt(
            "restored container is shorter than its header length prefix",
        ));
    }
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    let header_len = u64::from_le_bytes(prefix);
    let start = 8usize
        .checked_add(header_len as usize)
        .ok_or_else(|| Error::corrupt("restored container header length overflows"))?;
    if start > bytes.len() {
        return Err(Error::corrupt(
            "restored container header runs past the produced bytes",
        ));
    }
    Ok(&bytes[start..])
}

/// The restore region that produces exactly one recorded range.
fn region_for_range(tensor: &CheckpointTensor, range: &AttestedRange) -> Result<Option<Region>> {
    if range.offset == 0 && range.length == tensor.element_bytes {
        return Ok(None);
    }
    if tensor.shape.len() != 2 {
        return Err(Error::corrupt(format!(
            "load attestation range for `{}` is partial but the tensor is not rank 2",
            tensor.name
        )));
    }
    let row_bytes = tensor.element_bytes / tensor.shape[0].max(1);
    if row_bytes == 0 || range.offset % row_bytes != 0 || range.length % row_bytes != 0 {
        return Err(Error::corrupt(format!(
            "load attestation range for `{}` is not row aligned",
            tensor.name
        )));
    }
    let start_row = range.offset / row_bytes;
    let rows = range.length / row_bytes;
    let end_row = start_row.saturating_add(rows);
    if end_row > tensor.shape[0] {
        return Err(Error::corrupt(format!(
            "load attestation range for `{}` runs past the tensor",
            tensor.name
        )));
    }
    Ok(Some(Region::new(
        vec![start_row, 0],
        vec![end_row, tensor.shape[1]],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing_is_exact() {
        assert_eq!(AttestStatus::parse("loaded").unwrap(), AttestStatus::Loaded);
        assert_eq!(
            AttestStatus::parse("unverified").unwrap(),
            AttestStatus::Unverified
        );
        assert!(AttestStatus::parse("Loaded").is_err());
        assert!(AttestStatus::parse("").is_err());
    }
}
