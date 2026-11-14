//! Producer-authenticated provenance with key lifecycle (subphase T074).
//!
//! A **producer attestation** is an optional signature binding a declared
//! producer identity, run id, tool and version, input content roots, output
//! root and a determinism class. It is a canonical WCB record with the Ed25519
//! signature outside the signed body. The normative description is
//! `docs/PRODUCER-ATTESTATION.md`.
//!
//! A **key set** is a canonical record holding every key the operator trusts,
//! each with an epoch window, an optional rotation epoch and an optional
//! revocation epoch, plus the root of the key set it rotated from. Verification
//! reports which key signed and whether it was valid at the signing epoch.
//!
//! # The limit, stated once here and in the document
//!
//! A signature authenticates a **declaration**. It does not prove that the
//! declared computation ran, that the declared tool produced the bytes, or that
//! the declared inputs were actually read. [`verify_attestation_against_store`]
//! narrows that gap to "the declared roots exist in this store at this
//! snapshot"; it still cannot prove execution.
//!
//! # Fail-closed rules
//!
//! * A revoked key refuses in every mode.
//! * The default [`VerifyMode::Current`] refuses a key that was rotated or
//!   expired after signing: the operator has said it is no longer trusted for
//!   new verification.
//! * [`VerifyMode::Historical`] accepts an attestation whose key was valid at
//!   its declared `signing_epoch`, but reports `currently_trusted: false` and
//!   the retirement status. It is an audit/replay mode, never the default.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::State;
use crate::store::Store;

/// Key-set schema.
pub const PRODUCER_KEY_SET_SCHEMA: &str = "weft.producer-key-set.v1";
/// Key-set identity domain.
pub const PRODUCER_KEY_SET_DOMAIN: &str = "weft/producer-keys/v1";
/// Attestation schema.
pub const PRODUCER_ATTESTATION_SCHEMA: &str = "weft.producer-attestation.v1";
/// Attestation identity domain (over the body without `signature`).
pub const PRODUCER_ATTESTATION_DOMAIN: &str = "weft/producer-attestation/v1";
/// Longest rotation chain a key set may name.
pub const MAX_ROTATION_DEPTH: u32 = 64;
/// Bounded producer/tool/run strings.
pub const MAX_TEXT_BYTES: usize = 512;
/// Bound on declared inputs.
pub const MAX_INPUTS: usize = 4096;

/// Why an attestation was refused. The `Error` class is the coarse contract;
/// this code is the precise cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    NonCanonical,
    Malformed,
    UnknownSigner,
    BadSignature,
    RevokedKey,
    KeyRotated,
    KeyNotYetValid,
    KeyExpiredAtSigning,
    KeySetRootMismatch,
    InputMismatch,
    OutputMismatch,
    UnknownDeterminism,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Rejection::NonCanonical => "NonCanonical",
            Rejection::Malformed => "Malformed",
            Rejection::UnknownSigner => "UnknownSigner",
            Rejection::BadSignature => "BadSignature",
            Rejection::RevokedKey => "RevokedKey",
            Rejection::KeyRotated => "KeyRotated",
            Rejection::KeyNotYetValid => "KeyNotYetValid",
            Rejection::KeyExpiredAtSigning => "KeyExpiredAtSigning",
            Rejection::KeySetRootMismatch => "KeySetRootMismatch",
            Rejection::InputMismatch => "InputMismatch",
            Rejection::OutputMismatch => "OutputMismatch",
            Rejection::UnknownDeterminism => "UnknownDeterminism",
        }
    }

    fn class(self) -> Class {
        match self {
            Rejection::NonCanonical => Class::NonCanonical,
            Rejection::Malformed => Class::Malformed,
            Rejection::UnknownSigner
            | Rejection::RevokedKey
            | Rejection::KeyRotated
            | Rejection::KeyNotYetValid
            | Rejection::KeyExpiredAtSigning
            | Rejection::KeySetRootMismatch => Class::Unauthorized,
            Rejection::BadSignature
            | Rejection::InputMismatch
            | Rejection::OutputMismatch
            | Rejection::UnknownDeterminism => Class::Corrupt,
        }
    }
}

/// A typed refusal that also names the precise cause.
#[derive(Clone, Debug)]
pub struct AttestationError {
    pub rejection: Rejection,
    pub error: Error,
}

impl AttestationError {
    fn new(rejection: Rejection, message: impl Into<String>) -> Self {
        AttestationError {
            rejection,
            error: Error::new(rejection.class(), message),
        }
    }

    pub fn code(&self) -> &'static str {
        self.rejection.as_str()
    }
}

impl From<AttestationError> for Error {
    fn from(value: AttestationError) -> Self {
        value.error
    }
}

impl std::fmt::Display for AttestationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rejection.as_str(), self.error.message())
    }
}

/// Determinism class of the declared computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Determinism {
    Exact,
    Declared,
    Unknown,
}

impl Determinism {
    pub fn as_str(self) -> &'static str {
        match self {
            Determinism::Exact => "exact",
            Determinism::Declared => "declared",
            Determinism::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "exact" => Ok(Determinism::Exact),
            "declared" => Ok(Determinism::Declared),
            "unknown" => Ok(Determinism::Unknown),
            other => Err(Error::invalid_argument(format!(
                "determinism `{other}` is not exact|declared|unknown"
            ))),
        }
    }
}

/// The Ed25519 public key for a 32-byte seed.
pub fn public_key_for_seed(seed: &[u8; 32]) -> Hash32 {
    Hash32::from_bytes(SigningKey::from_bytes(seed).verifying_key().to_bytes())
}

/// One trusted producer key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerKey {
    /// Raw 32-byte Ed25519 verifying key.
    pub key: Hash32,
    pub label: String,
    /// First key-set epoch at which this key is valid.
    pub valid_from_epoch: u64,
    /// Exclusive end; `None` means open-ended.
    pub valid_until_epoch: Option<u64>,
    /// Epoch at which the operator revoked the key (`None` = not revoked).
    pub revoked_at_epoch: Option<u64>,
    /// Epoch at which the key was rotated out (`None` = not rotated).
    pub rotated_at_epoch: Option<u64>,
}

impl ProducerKey {
    fn to_value(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert("key".to_owned(), Value::Bytes(self.key.as_bytes().to_vec()));
        map.insert("label".to_owned(), Value::Str(self.label.clone()));
        map.insert(
            "valid_from_epoch".to_owned(),
            Value::U64(self.valid_from_epoch),
        );
        map.insert(
            "valid_until_epoch".to_owned(),
            self.valid_until_epoch
                .map(Value::U64)
                .unwrap_or(Value::Null),
        );
        map.insert(
            "revoked_at_epoch".to_owned(),
            self.revoked_at_epoch.map(Value::U64).unwrap_or(Value::Null),
        );
        map.insert(
            "rotated_at_epoch".to_owned(),
            self.rotated_at_epoch.map(Value::U64).unwrap_or(Value::Null),
        );
        Value::Map(map)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let Value::Map(map) = value else {
            return Err(Error::malformed("producer key is not a map"));
        };
        const KEYS: &[&str] = &[
            "key",
            "label",
            "valid_from_epoch",
            "valid_until_epoch",
            "revoked_at_epoch",
            "rotated_at_epoch",
        ];
        if map.len() != KEYS.len() || !KEYS.iter().all(|key| map.contains_key(*key)) {
            return Err(Error::malformed("producer key has an unknown key set"));
        }
        let key = match map.get("key") {
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => Hash32::from_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::malformed("producer key is not 32 bytes"))?,
            ),
            _ => return Err(Error::malformed("producer key `key` is not bytes32")),
        };
        if key == Hash32::ZERO {
            return Err(Error::malformed("producer key must not be all zero"));
        }
        let label = match map.get("label") {
            Some(Value::Str(text)) if !text.is_empty() && text.len() <= MAX_TEXT_BYTES => {
                text.clone()
            }
            _ => return Err(Error::malformed("producer key `label` is not bounded text")),
        };
        let number = |name: &str| -> Result<Option<u64>> {
            match map.get(name) {
                Some(Value::U64(value)) => Ok(Some(*value)),
                Some(Value::Null) => Ok(None),
                _ => Err(Error::malformed(format!(
                    "producer key `{name}` is not u64|null"
                ))),
            }
        };
        let valid_from_epoch = number("valid_from_epoch")?
            .ok_or_else(|| Error::malformed("producer key needs `valid_from_epoch`"))?;
        let valid_until_epoch = number("valid_until_epoch")?;
        let revoked_at_epoch = number("revoked_at_epoch")?;
        let rotated_at_epoch = number("rotated_at_epoch")?;
        if let Some(until) = valid_until_epoch {
            if until <= valid_from_epoch {
                return Err(Error::malformed(
                    "producer key valid_until_epoch must be greater than valid_from_epoch",
                ));
            }
        }
        for (name, epoch) in [
            ("revoked_at_epoch", revoked_at_epoch),
            ("rotated_at_epoch", rotated_at_epoch),
        ] {
            if let Some(epoch) = epoch {
                if epoch < valid_from_epoch {
                    return Err(Error::malformed(format!(
                        "producer key {name} precedes valid_from_epoch"
                    )));
                }
            }
        }
        Ok(ProducerKey {
            key,
            label,
            valid_from_epoch,
            valid_until_epoch,
            revoked_at_epoch,
            rotated_at_epoch,
        })
    }

    /// Was the key valid at `epoch` ignoring the current-trust question?
    fn valid_at(&self, epoch: u64) -> bool {
        if epoch < self.valid_from_epoch {
            return false;
        }
        if let Some(until) = self.valid_until_epoch {
            if epoch >= until {
                return false;
            }
        }
        if let Some(rotated) = self.rotated_at_epoch {
            if epoch >= rotated {
                return false;
            }
        }
        if let Some(revoked) = self.revoked_at_epoch {
            if epoch >= revoked {
                return false;
            }
        }
        true
    }
}

/// How a key stands at one epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStatus {
    Current,
    Retired,
    Revoked,
    Unknown,
    NotYetValid,
    Expired,
}

impl KeyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyStatus::Current => "current",
            KeyStatus::Retired => "retired",
            KeyStatus::Revoked => "revoked",
            KeyStatus::Unknown => "unknown",
            KeyStatus::NotYetValid => "not_yet_valid",
            KeyStatus::Expired => "expired",
        }
    }
}

/// A canonical producer key set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerKeySet {
    pub epoch: u64,
    /// Root of the key set this one rotated from, if any.
    pub rotated_from: Option<Hash32>,
    /// Sorted by key bytes, unique.
    pub keys: Vec<ProducerKey>,
}

impl ProducerKeySet {
    pub fn new(
        epoch: u64,
        rotated_from: Option<Hash32>,
        mut keys: Vec<ProducerKey>,
    ) -> Result<Self> {
        if keys.is_empty() {
            return Err(Error::invalid_argument("a key set needs at least one key"));
        }
        if keys.len() > 4096 {
            return Err(Error::limit_exceeded("a key set is bounded to 4096 keys"));
        }
        keys.sort_by(|a, b| a.key.cmp(&b.key));
        let mut seen = BTreeSet::new();
        for key in &keys {
            if !seen.insert(key.key) {
                return Err(Error::non_canonical("a key set has a duplicate key"));
            }
        }
        if let Some(root) = rotated_from {
            if root == Hash32::ZERO {
                return Err(Error::non_canonical("rotated_from must not be all zero"));
            }
        }
        Ok(ProducerKeySet {
            epoch,
            rotated_from,
            keys,
        })
    }

    pub fn key(&self, key: Hash32) -> Option<&ProducerKey> {
        self.keys.iter().find(|candidate| candidate.key == key)
    }

    fn to_value(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(PRODUCER_KEY_SET_SCHEMA.to_owned()),
        );
        map.insert("epoch".to_owned(), Value::U64(self.epoch));
        map.insert(
            "rotated_from".to_owned(),
            self.rotated_from
                .map(|root| Value::Bytes(root.as_bytes().to_vec()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "keys".to_owned(),
            Value::Array(self.keys.iter().map(ProducerKey::to_value).collect()),
        );
        Value::Map(map)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// `H("weft/producer-keys/v1", WCB(record))`.
    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(
            PRODUCER_KEY_SET_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let encoded = wcb::encode(&value, &Limits::default()).map_err(Error::from)?;
        if encoded != bytes {
            return Err(Error::non_canonical(
                "producer key set is not canonical WCB",
            ));
        }
        let Value::Map(map) = value else {
            return Err(Error::malformed("producer key set is not a map"));
        };
        const KEYS: &[&str] = &["schema", "epoch", "rotated_from", "keys"];
        if map.len() != KEYS.len() || !KEYS.iter().all(|key| map.contains_key(*key)) {
            return Err(Error::malformed("producer key set has an unknown key set"));
        }
        match map.get("schema") {
            Some(Value::Str(text)) if text == PRODUCER_KEY_SET_SCHEMA => {}
            _ => return Err(Error::malformed("unknown producer key-set schema")),
        }
        let epoch = match map.get("epoch") {
            Some(Value::U64(value)) => *value,
            _ => return Err(Error::malformed("producer key set `epoch` is not u64")),
        };
        let rotated_from = match map.get("rotated_from") {
            Some(Value::Null) => None,
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
                Some(Hash32::from_bytes(bytes.as_slice().try_into().map_err(
                    |_| Error::malformed("producer key set rotated_from is not 32 bytes"),
                )?))
            }
            _ => {
                return Err(Error::malformed(
                    "producer key set `rotated_from` is not bytes32|null",
                ))
            }
        };
        let keys = match map.get("keys") {
            Some(Value::Array(items)) => items
                .iter()
                .map(ProducerKey::from_value)
                .collect::<Result<Vec<_>>>()?,
            _ => return Err(Error::malformed("producer key set `keys` is not an array")),
        };
        let set = ProducerKeySet::new(epoch, rotated_from, keys)?;
        if set.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "producer key set is not in canonical order",
            ));
        }
        Ok(set)
    }

    /// Status of `key` when evaluated at the set's own `epoch`.
    pub fn status(&self, key: Hash32) -> KeyStatus {
        let Some(entry) = self.key(key) else {
            return KeyStatus::Unknown;
        };
        if let Some(revoked) = entry.revoked_at_epoch {
            if self.epoch >= revoked {
                return KeyStatus::Revoked;
            }
        }
        if let Some(rotated) = entry.rotated_at_epoch {
            if self.epoch >= rotated {
                return KeyStatus::Retired;
            }
        }
        if self.epoch < entry.valid_from_epoch {
            return KeyStatus::NotYetValid;
        }
        if let Some(until) = entry.valid_until_epoch {
            if self.epoch >= until {
                return KeyStatus::Expired;
            }
        }
        KeyStatus::Current
    }
}

/// One declared input root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerInput {
    pub role: String,
    pub content_root: Hash32,
}

/// A producer attestation. `signature` is outside the signed body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerAttestation {
    pub producer: String,
    pub run_id: String,
    pub tool: String,
    pub tool_version: String,
    pub determinism: Determinism,
    pub signing_epoch: u64,
    pub key_set_root: Hash32,
    /// Raw Ed25519 verifying key that produced `signature`. It is inside the
    /// signed body, so a record cannot claim one signer and be signed by another.
    pub signer: Option<Hash32>,
    /// Sorted by `(role, content_root)`, unique.
    pub inputs: Vec<ProducerInput>,
    pub output: Hash32,
    pub snapshot: Option<Hash32>,
    pub issued_at_ns: i64,
    pub signature: Option<[u8; 64]>,
}

impl ProducerAttestation {
    /// Build an unsigned attestation. Inputs are sorted canonically.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        producer: impl Into<String>,
        run_id: impl Into<String>,
        tool: impl Into<String>,
        tool_version: impl Into<String>,
        determinism: Determinism,
        signing_epoch: u64,
        key_set_root: Hash32,
        mut inputs: Vec<ProducerInput>,
        output: Hash32,
        snapshot: Option<Hash32>,
        issued_at_ns: i64,
    ) -> Result<Self> {
        let text = |value: String, what: &str| -> Result<String> {
            if value.is_empty() || value.len() > MAX_TEXT_BYTES {
                return Err(Error::invalid_argument(format!(
                    "producer attestation {what} must be 1..={MAX_TEXT_BYTES} bytes"
                )));
            }
            Ok(value)
        };
        let producer = text(producer.into(), "producer")?;
        let run_id = text(run_id.into(), "run_id")?;
        let tool = text(tool.into(), "tool")?;
        let tool_version = text(tool_version.into(), "tool_version")?;
        if key_set_root == Hash32::ZERO {
            return Err(Error::invalid_argument("key_set_root must not be all zero"));
        }
        if output == Hash32::ZERO {
            return Err(Error::invalid_argument("output must not be all zero"));
        }
        if let Some(snapshot) = snapshot {
            if snapshot == Hash32::ZERO {
                return Err(Error::invalid_argument("snapshot must not be all zero"));
            }
        }
        if inputs.len() > MAX_INPUTS {
            return Err(Error::limit_exceeded(format!(
                "producer attestation is bounded to {MAX_INPUTS} inputs"
            )));
        }
        for input in &inputs {
            if input.role.is_empty() || input.role.len() > MAX_TEXT_BYTES {
                return Err(Error::invalid_argument(
                    "producer input role must be bounded non-empty text",
                ));
            }
            if input.content_root == Hash32::ZERO {
                return Err(Error::invalid_argument(
                    "producer input root must not be all zero",
                ));
            }
        }
        inputs.sort_by(|a, b| {
            a.role
                .as_bytes()
                .cmp(b.role.as_bytes())
                .then(a.content_root.cmp(&b.content_root))
        });
        inputs.dedup();
        Ok(ProducerAttestation {
            producer,
            run_id,
            tool,
            tool_version,
            determinism,
            signing_epoch,
            key_set_root,
            signer: None,
            inputs,
            output,
            snapshot,
            issued_at_ns,
            signature: None,
        })
    }

    /// The signed body (no `signature` key), canonical WCB.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(PRODUCER_ATTESTATION_SCHEMA.to_owned()),
        );
        map.insert("producer".to_owned(), Value::Str(self.producer.clone()));
        map.insert("run_id".to_owned(), Value::Str(self.run_id.clone()));
        map.insert("tool".to_owned(), Value::Str(self.tool.clone()));
        map.insert(
            "tool_version".to_owned(),
            Value::Str(self.tool_version.clone()),
        );
        map.insert(
            "determinism".to_owned(),
            Value::Str(self.determinism.as_str().to_owned()),
        );
        map.insert("signing_epoch".to_owned(), Value::U64(self.signing_epoch));
        map.insert(
            "key_set_root".to_owned(),
            Value::Bytes(self.key_set_root.as_bytes().to_vec()),
        );
        map.insert(
            "signer".to_owned(),
            self.signer
                .map(|signer| Value::Bytes(signer.as_bytes().to_vec()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "inputs".to_owned(),
            Value::Array(
                self.inputs
                    .iter()
                    .map(|input| {
                        let mut row = std::collections::BTreeMap::new();
                        row.insert("role".to_owned(), Value::Str(input.role.clone()));
                        row.insert(
                            "content_root".to_owned(),
                            Value::Bytes(input.content_root.as_bytes().to_vec()),
                        );
                        Value::Map(row)
                    })
                    .collect(),
            ),
        );
        map.insert(
            "output".to_owned(),
            Value::Bytes(self.output.as_bytes().to_vec()),
        );
        map.insert(
            "snapshot".to_owned(),
            self.snapshot
                .map(|snapshot| Value::Bytes(snapshot.as_bytes().to_vec()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "issued_at_ns".to_owned(),
            Value::U64(u64::try_from(self.issued_at_ns).unwrap_or(0)),
        );
        wcb::encode(&Value::Map(map), &Limits::default()).map_err(Error::from)
    }

    /// Full record bytes (body + signature).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let signature = self
            .signature
            .ok_or_else(|| Error::invalid_argument("attestation is not signed"))?;
        let body = self.signing_bytes()?;
        let value = wcb::decode(&body, &Limits::default()).map_err(Error::from)?;
        let Value::Map(mut map) = value else {
            return Err(Error::malformed("attestation body is not a map"));
        };
        map.insert("signature".to_owned(), Value::Bytes(signature.to_vec()));
        wcb::encode(&Value::Map(map), &Limits::default()).map_err(Error::from)
    }

    /// `H("weft/producer-attestation/v1", signing bytes)`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(
            PRODUCER_ATTESTATION_DOMAIN,
            &self.signing_bytes()?,
        ))
    }

    /// Sign with a 32-byte Ed25519 seed. The public key is written into the
    /// signed body before signing, so verification knows exactly one candidate.
    pub fn sign(&mut self, seed: &[u8; 32]) -> Result<Hash32> {
        let key = SigningKey::from_bytes(seed);
        let signer = Hash32::from_bytes(key.verifying_key().to_bytes());
        self.signer = Some(signer);
        let signature = key.sign(&self.signing_bytes()?);
        self.signature = Some(signature.to_bytes());
        Ok(signer)
    }

    /// Strict decode of the full record.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let encoded = wcb::encode(&value, &Limits::default()).map_err(Error::from)?;
        if encoded != bytes {
            return Err(Error::non_canonical(
                "producer attestation is not canonical WCB",
            ));
        }
        let Value::Map(map) = value else {
            return Err(Error::malformed("producer attestation is not a map"));
        };
        const KEYS: &[&str] = &[
            "schema",
            "producer",
            "run_id",
            "tool",
            "tool_version",
            "determinism",
            "signing_epoch",
            "key_set_root",
            "signer",
            "inputs",
            "output",
            "snapshot",
            "issued_at_ns",
            "signature",
        ];
        if map.len() != KEYS.len() || !KEYS.iter().all(|key| map.contains_key(*key)) {
            return Err(Error::malformed(
                "producer attestation has an unknown key set",
            ));
        }
        match map.get("schema") {
            Some(Value::Str(text)) if text == PRODUCER_ATTESTATION_SCHEMA => {}
            _ => return Err(Error::malformed("unknown producer attestation schema")),
        }
        let text = |name: &str| -> Result<String> {
            match map.get(name) {
                Some(Value::Str(value)) => Ok(value.clone()),
                _ => Err(Error::malformed(format!(
                    "producer attestation `{name}` is not text"
                ))),
            }
        };
        let root = |name: &str| -> Result<Hash32> {
            match map.get(name) {
                Some(Value::Bytes(bytes)) if bytes.len() == 32 => Ok(Hash32::from_bytes(
                    bytes.as_slice().try_into().map_err(|_| {
                        Error::malformed(format!("producer attestation `{name}` is not 32 bytes"))
                    })?,
                )),
                _ => Err(Error::malformed(format!(
                    "producer attestation `{name}` is not bytes32"
                ))),
            }
        };
        let signing_epoch = match map.get("signing_epoch") {
            Some(Value::U64(value)) => *value,
            _ => {
                return Err(Error::malformed(
                    "producer attestation `signing_epoch` is not u64",
                ))
            }
        };
        let issued_at_ns = match map.get("issued_at_ns") {
            Some(Value::U64(value)) => i64::try_from(*value).map_err(|_| {
                Error::malformed("producer attestation issued_at_ns is out of range")
            })?,
            _ => {
                return Err(Error::malformed(
                    "producer attestation `issued_at_ns` is not u64",
                ))
            }
        };
        let snapshot = match map.get("snapshot") {
            Some(Value::Null) => None,
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
                Some(Hash32::from_bytes(bytes.as_slice().try_into().map_err(
                    |_| Error::malformed("producer attestation snapshot is not 32 bytes"),
                )?))
            }
            _ => {
                return Err(Error::malformed(
                    "producer attestation `snapshot` is not bytes32|null",
                ))
            }
        };
        let signer = match map.get("signer") {
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
                Some(Hash32::from_bytes(bytes.as_slice().try_into().map_err(
                    |_| Error::malformed("producer attestation signer is not 32 bytes"),
                )?))
            }
            _ => {
                return Err(Error::malformed(
                    "producer attestation `signer` is not bytes32",
                ))
            }
        };
        let inputs = match map.get("inputs") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    let Value::Map(row) = item else {
                        return Err(Error::malformed("producer input is not a map"));
                    };
                    if row.len() != 2 {
                        return Err(Error::malformed("producer input has an unknown key set"));
                    }
                    let role = match row.get("role") {
                        Some(Value::Str(text)) => text.clone(),
                        _ => return Err(Error::malformed("producer input role is not text")),
                    };
                    let content_root = match row.get("content_root") {
                        Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
                            Hash32::from_bytes(bytes.as_slice().try_into().map_err(|_| {
                                Error::malformed("producer input root is not 32 bytes")
                            })?)
                        }
                        _ => {
                            return Err(Error::malformed(
                                "producer input content_root is not bytes32",
                            ))
                        }
                    };
                    Ok(ProducerInput { role, content_root })
                })
                .collect::<Result<Vec<_>>>()?,
            _ => {
                return Err(Error::malformed(
                    "producer attestation `inputs` is not an array",
                ))
            }
        };
        let signature = match map.get("signature") {
            Some(Value::Bytes(bytes)) if bytes.len() == 64 => {
                Some(<[u8; 64]>::try_from(bytes.as_slice()).map_err(|_| {
                    Error::malformed("producer attestation signature is not 64 bytes")
                })?)
            }
            _ => {
                return Err(Error::malformed(
                    "producer attestation `signature` is not bytes64",
                ))
            }
        };
        let attestation = ProducerAttestation::new(
            text("producer")?,
            text("run_id")?,
            text("tool")?,
            text("tool_version")?,
            Determinism::parse(&text("determinism")?)?,
            signing_epoch,
            root("key_set_root")?,
            inputs,
            root("output")?,
            snapshot,
            issued_at_ns,
        )?;
        let mut attestation = attestation;
        attestation.signer = signer;
        attestation.signature = signature;
        if attestation.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "producer attestation is not in canonical order",
            ));
        }
        Ok(attestation)
    }
}

/// Verification mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyMode {
    /// Default: only a currently trusted key may authenticate a declaration.
    Current,
    /// Audit/replay: the key only has to have been valid at `signing_epoch`.
    /// A revoked key still refuses.
    Historical,
}

/// A verified attestation.
#[derive(Clone, Debug)]
pub struct VerifiedAttestation {
    pub attestation_id: Hash32,
    pub signer: Hash32,
    pub key_set_root: Hash32,
    pub key_set_epoch: u64,
    pub signing_epoch: u64,
    pub key_status: KeyStatus,
    /// The key was inside its epoch window at `signing_epoch`.
    pub valid_at_signing: bool,
    /// The key is trusted *now* under the supplied key set.
    pub currently_trusted: bool,
    pub producer: String,
    pub run_id: String,
    pub tool: String,
    pub tool_version: String,
    pub determinism: Determinism,
    pub output: Hash32,
    pub snapshot: Option<Hash32>,
}

/// Verify one attestation against `key_set`.
///
/// Order: decode canonicality, key-set binding, key membership, signature,
/// revocation, then the mode-specific trust decision. Nothing is accepted
/// before the signature verifies.
pub fn verify_attestation(
    bytes: &[u8],
    key_set: &ProducerKeySet,
    mode: VerifyMode,
    expected_signer: Option<Hash32>,
) -> std::result::Result<VerifiedAttestation, AttestationError> {
    let attestation = ProducerAttestation::from_canonical_bytes(bytes).map_err(|err| {
        let rejection = if err.class() == Class::NonCanonical {
            Rejection::NonCanonical
        } else {
            Rejection::Malformed
        };
        AttestationError::new(rejection, err.message().to_owned())
    })?;
    let key_set_root = key_set.root().map_err(|err| {
        AttestationError::new(Rejection::KeySetRootMismatch, err.message().to_owned())
    })?;
    if attestation.key_set_root != key_set_root {
        return Err(AttestationError::new(
            Rejection::KeySetRootMismatch,
            format!(
                "attestation names key set {}, the supplied set roots at {}",
                attestation.key_set_root, key_set_root
            ),
        ));
    }
    let signature = attestation.signature.ok_or_else(|| {
        AttestationError::new(Rejection::BadSignature, "attestation has no signature")
    })?;
    let signer = attestation.signer.ok_or_else(|| {
        AttestationError::new(Rejection::UnknownSigner, "attestation names no signer")
    })?;
    if let Some(expected) = expected_signer {
        if signer != expected {
            return Err(AttestationError::new(
                Rejection::UnknownSigner,
                "attestation was signed by a different key than expected",
            ));
        }
    }
    let entry = key_set.key(signer).ok_or_else(|| {
        AttestationError::new(
            Rejection::UnknownSigner,
            format!("signer {signer} is not present in the supplied key set"),
        )
    })?;
    let verifying = VerifyingKey::from_bytes(signer.as_bytes()).map_err(|err| {
        AttestationError::new(
            Rejection::BadSignature,
            format!("signer is not a valid Ed25519 key: {err}"),
        )
    })?;
    let signature = Signature::from_slice(&signature).map_err(|err| {
        AttestationError::new(
            Rejection::BadSignature,
            format!("signature encoding is invalid: {err}"),
        )
    })?;
    let body = attestation
        .signing_bytes()
        .map_err(|err| AttestationError::new(Rejection::Malformed, err.message().to_owned()))?;
    verifying.verify_strict(&body, &signature).map_err(|_| {
        AttestationError::new(
            Rejection::BadSignature,
            format!("signature does not verify under signer {signer}"),
        )
    })?;
    if let Some(revoked) = entry.revoked_at_epoch {
        if key_set.epoch >= revoked {
            return Err(AttestationError::new(
                Rejection::RevokedKey,
                format!("signing key {} was revoked at epoch {revoked}", signer),
            ));
        }
    }
    let valid_at_signing = entry.valid_at(attestation.signing_epoch);
    let status = key_set.status(signer);
    let currently_trusted = status == KeyStatus::Current;
    match mode {
        VerifyMode::Current => {
            if let Some(rotated) = entry.rotated_at_epoch {
                if key_set.epoch >= rotated {
                    return Err(AttestationError::new(
                        Rejection::KeyRotated,
                        format!(
                            "signing key {} was rotated at epoch {rotated} and is no longer \
                             trusted for verification; use historical mode to audit the record",
                            signer
                        ),
                    ));
                }
            }
            match status {
                KeyStatus::Current => {}
                KeyStatus::Revoked => {
                    return Err(AttestationError::new(
                        Rejection::RevokedKey,
                        format!("signing key {signer} is revoked"),
                    ))
                }
                KeyStatus::NotYetValid => {
                    return Err(AttestationError::new(
                        Rejection::KeyNotYetValid,
                        format!("signing key {signer} is not valid yet"),
                    ))
                }
                KeyStatus::Expired | KeyStatus::Retired => {
                    return Err(AttestationError::new(
                        Rejection::KeyRotated,
                        format!(
                            "signing key {signer} is {} in the supplied key set",
                            status.as_str()
                        ),
                    ))
                }
                KeyStatus::Unknown => {
                    return Err(AttestationError::new(
                        Rejection::UnknownSigner,
                        format!("signing key {signer} is absent"),
                    ))
                }
            }
        }
        VerifyMode::Historical => {
            if !valid_at_signing {
                let rejection = if attestation.signing_epoch < entry.valid_from_epoch {
                    Rejection::KeyNotYetValid
                } else {
                    Rejection::KeyExpiredAtSigning
                };
                return Err(AttestationError::new(
                    rejection,
                    format!(
                        "signing key {signer} was not valid at signing epoch {}",
                        attestation.signing_epoch
                    ),
                ));
            }
        }
    }
    let attestation_id = attestation
        .id()
        .map_err(|err| AttestationError::new(Rejection::Malformed, err.message().to_owned()))?;
    Ok(VerifiedAttestation {
        attestation_id,
        signer,
        key_set_root,
        key_set_epoch: key_set.epoch,
        signing_epoch: attestation.signing_epoch,
        key_status: status,
        valid_at_signing,
        currently_trusted,
        producer: attestation.producer,
        run_id: attestation.run_id,
        tool: attestation.tool,
        tool_version: attestation.tool_version,
        determinism: attestation.determinism,
        output: attestation.output,
        snapshot: attestation.snapshot,
    })
}

/// The verified attestation plus its store cross-check.
#[derive(Clone, Debug)]
pub struct StoreCheckedAttestation {
    pub verified: VerifiedAttestation,
    /// Every declared input root exists in the store at the snapshot.
    pub inputs_present: bool,
    /// The declared output root exists in the store at the snapshot.
    pub output_present: bool,
}

/// Verify the signature, then check every declared root against the store.
///
/// The store check answers "these roots are real content in this store at this
/// snapshot". It still cannot prove that the computation ran or that the tool
/// actually read those inputs; see the module note.
///
/// Cost: one enumeration of objects plus every checkpoint manifest at the
/// snapshot. It is a verification path, not a hot path.
pub fn verify_attestation_against_store(
    store: &Store,
    snapshot: Option<Hash32>,
    bytes: &[u8],
    key_set: &ProducerKeySet,
    mode: VerifyMode,
) -> std::result::Result<StoreCheckedAttestation, AttestationError> {
    let verified = verify_attestation(bytes, key_set, mode, None)?;
    let attestation = ProducerAttestation::from_canonical_bytes(bytes)
        .map_err(|err| AttestationError::new(Rejection::Malformed, err.message().to_owned()))?;
    let state = match snapshot {
        Some(snapshot) => store.state_at(snapshot).map_err(|err| {
            AttestationError::new(
                Rejection::InputMismatch,
                format!(
                    "snapshot {} is not replayable: {}",
                    snapshot.to_hex(),
                    err.message()
                ),
            )
        })?,
        None => store.current_state().clone(),
    };
    let roots = store_content_roots(store, snapshot, &state)?;
    for input in &attestation.inputs {
        if !roots.contains(&input.content_root) {
            return Err(AttestationError::new(
                Rejection::InputMismatch,
                format!(
                    "declared input `{}` root {} is not content in this store at the pinned \
                     snapshot",
                    input.role,
                    input.content_root.to_hex()
                ),
            ));
        }
    }
    if !roots.contains(&attestation.output) {
        return Err(AttestationError::new(
            Rejection::OutputMismatch,
            format!(
                "declared output root {} is not content in this store at the pinned snapshot",
                attestation.output.to_hex()
            ),
        ));
    }
    Ok(StoreCheckedAttestation {
        verified,
        inputs_present: true,
        output_present: true,
    })
}

/// Every content root reachable as content at `state`: object content roots and
/// checkpoint manifest/tensor/shard roots.
fn store_content_roots(
    store: &Store,
    snapshot: Option<Hash32>,
    state: &State,
) -> std::result::Result<BTreeSet<Hash32>, AttestationError> {
    let mut roots = BTreeSet::new();
    for record in state.objects.values() {
        match record.kind {
            crate::state::ObjectKind::File => {
                // `record.content` is the manifest id; the whole-bytes identity
                // is the manifest's `content_hash`.
                match store.manifest_of(record.content) {
                    Ok(manifest) => {
                        roots.insert(manifest.content_hash);
                    }
                    Err(err) => {
                        return Err(AttestationError::new(
                            Rejection::InputMismatch,
                            format!(
                                "object {} has no readable manifest: {}",
                                record.id,
                                err.message()
                            ),
                        ))
                    }
                }
            }
            crate::state::ObjectKind::Dir => {
                roots.insert(identity::content_hash(b""));
            }
            crate::state::ObjectKind::Symlink => {
                if let Some(target) = record.symlink_target() {
                    roots.insert(identity::content_hash(target));
                }
            }
        }
    }
    // Checkpoint identities: the manifest root, every tensor content root and
    // every shard content root the pinned snapshot can load. Names come from
    // the live metadata view; every manifest is decoded at the pinned snapshot
    // and a name absent there is skipped.
    let names = store
        .list_checkpoints()
        .map_err(|err| AttestationError::new(Rejection::Malformed, err.message().to_owned()))?;
    for name in names {
        let loaded = match snapshot {
            Some(snapshot) => store.load_checkpoint_manifest_at(snapshot, &name),
            None => store.load_checkpoint_manifest(&name),
        };
        let Ok(loaded) = loaded else { continue };
        roots.insert(loaded.manifest_root);
        for tensor in &loaded.manifest.tensors {
            roots.insert(tensor.content_root);
            roots.insert(tensor.logical_root);
            for shard in &tensor.shards {
                roots.insert(shard.content_root);
                roots.insert(shard.logical_root);
            }
        }
    }
    Ok(roots)
}

/// Key-set rotation helper: a new epoch naming the previous root.
pub fn rotate_key_set(
    previous: &ProducerKeySet,
    next_epoch: u64,
    keys: Vec<ProducerKey>,
) -> Result<ProducerKeySet> {
    if next_epoch <= previous.epoch {
        return Err(Error::invalid_argument(
            "a rotated key set needs a strictly greater epoch",
        ));
    }
    let previous_root = previous.root()?;
    ProducerKeySet::new(next_epoch, Some(previous_root), keys)
}

/// Validate a rotation chain of at most [`MAX_ROTATION_DEPTH`] ancestors.
pub fn validate_rotation_chain(roots: &[Hash32], oldest_first: bool) -> Result<()> {
    if roots.len() > MAX_ROTATION_DEPTH as usize {
        return Err(Error::limit_exceeded(format!(
            "rotation chain exceeds {MAX_ROTATION_DEPTH} entries"
        )));
    }
    let mut seen = BTreeSet::new();
    for root in roots {
        if *root == Hash32::ZERO {
            return Err(Error::non_canonical("rotation chain contains a zero root"));
        }
        if !seen.insert(*root) {
            return Err(Error::non_canonical("rotation chain repeats a root"));
        }
    }
    let _ = oldest_first;
    Ok(())
}
