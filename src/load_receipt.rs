//! `weft.load.receipt.v1`: end-to-end integrity from checkpoint to device.
//!
//! A load receipt is the record a loader emits **after** it has produced
//! tensor bytes: for every tensor it names the attested `LogicalTensorRoot`
//! from the checkpoint manifest, the root recomputed from the loaded bytes,
//! the object content root, how many independent units (tiles for modes
//! 15/17/18, shards for the checkpoint topology) were verified, the bytes
//! produced, and the distribution hash of every loaded tensor (the **set
//! root**).
//!
//! Two verification modes exist:
//!
//! * [`LoadVerification::Lazy`] — the normal restore path. Every unit the
//!   format makes independently checkable (a tile root for modes 15/17/18 and
//!   a shard root for every checkpoint shard) is verified while the bytes are
//!   decoded, and the loaded tensor's logical root is compared to the attested
//!   root before any byte is emitted for that tensor.
//! * [`LoadVerification::Full`] — lazy, plus a second whole-object pass that
//!   re-reads every stored shard through the store's verifying read path and
//!   re-checks its whole-content BLAKE3. This catches a store-side defect that
//!   a per-tile root cannot see (for example a manifest swap inside one shard
//!   whose tiles were individually valid).
//!
//! The record is canonical WCB. Its `receipt_root` is
//! `H("weft/load/receipt/v1", canonical_bytes)`; the set root is
//! `H("weft/load/set/v1", canonical WCB of the sorted roster)`. A receipt is
//! only emitted when `loaded_set_root == attested_set_root`; the loader returns
//! a typed `Corrupt` error naming the tensor otherwise.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::wcb_util as wu;

/// Canonical schema string.
pub const LOAD_RECEIPT_SCHEMA: &str = "weft.load.receipt.v1";
/// Domain for the receipt identity.
pub const LOAD_RECEIPT_DOMAIN: &str = "weft/load/receipt/v1";
/// Domain for the loaded/unloaded tensor-set root.
pub const LOAD_SET_DOMAIN: &str = "weft/load/set/v1";

/// How thoroughly a load was verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadVerification {
    /// Per-unit roots (tiles/shards) plus the loaded tensor root.
    Lazy,
    /// Lazy plus a whole-object content-hash re-read of every stored shard.
    Full,
}

impl LoadVerification {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "lazy" => Ok(LoadVerification::Lazy),
            "full" => Ok(LoadVerification::Full),
            other => Err(Error::invalid_argument(format!(
                "--verify-load must be `lazy` or `full` (got `{other}`)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LoadVerification::Lazy => "lazy",
            LoadVerification::Full => "full",
        }
    }
}

/// Options for emitting a load receipt during a checkpoint restore.
#[derive(Clone, Debug)]
pub struct LoadReceiptOptions {
    /// Replica label recorded in the receipt (host, rank, device set).
    pub replica: String,
    /// Lazy or full verification.
    pub verification: LoadVerification,
}

impl Default for LoadReceiptOptions {
    fn default() -> Self {
        LoadReceiptOptions {
            replica: "local".to_owned(),
            verification: LoadVerification::Lazy,
        }
    }
}

/// One tensor's verified load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadReceiptTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    /// Root recorded by the checkpoint manifest.
    pub attested_root: Hash32,
    /// Root recomputed from the produced bytes.
    pub loaded_root: Hash32,
    /// BLAKE3 of the whole tensor payload.
    pub content_root: Hash32,
    /// SHA-256 of the produced payload.
    pub sha256: [u8; 32],
    pub bytes: u64,
    /// Independently verified units (tiles for modes 15/17/18, shards for the
    /// checkpoint topology).
    pub units_verified: u64,
}

/// One replica's load receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadReceipt {
    pub checkpoint: String,
    pub manifest_root: Hash32,
    /// Caller-supplied replica label (host, rank, device set).
    pub replica: String,
    pub verification: LoadVerification,
    /// Sorted by tensor name.
    pub tensors: Vec<LoadReceiptTensor>,
    /// `H(LOAD_SET_DOMAIN, roster(loaded roots))`.
    pub loaded_set_root: Hash32,
    /// `H(LOAD_SET_DOMAIN, roster(attested roots))`.
    pub attested_set_root: Hash32,
    pub bytes_produced: u64,
    pub units_verified: u64,
    pub wall_ms: u64,
    /// True when the whole source container was reproduced.
    pub exact_source_file: bool,
    /// SHA-256 of the produced container (or of the concatenated payloads).
    pub file_sha256: [u8; 32],
}

impl LoadReceipt {
    /// Roster root over `(name, root)` pairs, canonically sorted by name.
    pub fn roster_root(entries: &[(String, Hash32)]) -> Result<Hash32> {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in sorted.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::corrupt(format!(
                    "load receipt roster has duplicate tensor `{}`",
                    pair[0].0
                )));
            }
        }
        let value = Value::Array(
            sorted
                .iter()
                .map(|(name, root)| {
                    wu::map([
                        ("name", Value::Str(name.clone())),
                        ("logical_root", wu::hash_value(*root)),
                    ])
                })
                .collect(),
        );
        Ok(identity::h(LOAD_SET_DOMAIN, &wu::encode(&value)?))
    }

    pub fn loaded_roster(&self) -> Result<Hash32> {
        Self::roster_root(
            &self
                .tensors
                .iter()
                .map(|tensor| (tensor.name.clone(), tensor.loaded_root))
                .collect::<Vec<_>>(),
        )
    }

    pub fn attested_roster(&self) -> Result<Hash32> {
        Self::roster_root(
            &self
                .tensors
                .iter()
                .map(|tensor| (tensor.name.clone(), tensor.attested_root))
                .collect::<Vec<_>>(),
        )
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(LOAD_RECEIPT_SCHEMA.to_owned())),
            ("checkpoint", Value::Str(self.checkpoint.clone())),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            ("replica", Value::Str(self.replica.clone())),
            (
                "verification",
                Value::Str(self.verification.as_str().to_owned()),
            ),
            (
                "tensors",
                Value::Array(
                    self.tensors
                        .iter()
                        .map(|tensor| {
                            wu::map([
                                ("name", Value::Str(tensor.name.clone())),
                                ("dtype", Value::Str(tensor.dtype.clone())),
                                (
                                    "shape",
                                    Value::Array(
                                        tensor.shape.iter().map(|dim| Value::U64(*dim)).collect(),
                                    ),
                                ),
                                ("attested_root", wu::hash_value(tensor.attested_root)),
                                ("loaded_root", wu::hash_value(tensor.loaded_root)),
                                ("content_root", wu::hash_value(tensor.content_root)),
                                ("sha256", Value::Bytes(tensor.sha256.to_vec())),
                                ("bytes", Value::U64(tensor.bytes)),
                                ("units_verified", Value::U64(tensor.units_verified)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("loaded_set_root", wu::hash_value(self.loaded_set_root)),
            ("attested_set_root", wu::hash_value(self.attested_set_root)),
            ("bytes_produced", Value::U64(self.bytes_produced)),
            ("units_verified", Value::U64(self.units_verified)),
            ("wall_ms", Value::U64(self.wall_ms)),
            ("exact_source_file", Value::Bool(self.exact_source_file)),
            ("file_sha256", Value::Bytes(self.file_sha256.to_vec())),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// Receipt identity over the canonical bytes.
    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(LOAD_RECEIPT_DOMAIN, &self.encode()?))
    }

    pub fn decode(bytes: &[u8]) -> Result<LoadReceipt> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "checkpoint",
                "manifest_root",
                "replica",
                "verification",
                "tensors",
                "loaded_set_root",
                "attested_set_root",
                "bytes_produced",
                "units_verified",
                "wall_ms",
                "exact_source_file",
                "file_sha256",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != LOAD_RECEIPT_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "load receipt schema `{schema}` is not `{LOAD_RECEIPT_SCHEMA}`"
            )));
        }
        let mut tensors = Vec::new();
        for entry in wu::get_array(map, "tensors")? {
            let entry = wu::as_map(entry)?;
            wu::check_keys(
                entry,
                &[
                    "name",
                    "dtype",
                    "shape",
                    "attested_root",
                    "loaded_root",
                    "content_root",
                    "sha256",
                    "bytes",
                    "units_verified",
                ],
            )?;
            let mut shape = Vec::new();
            for dim in wu::get_array(entry, "shape")? {
                match dim {
                    Value::U64(value) => shape.push(*value),
                    other => {
                        return Err(Error::malformed(format!(
                            "load receipt shape entry is not u64: {other:?}"
                        )))
                    }
                }
            }
            let sha256: [u8; 32] = wu::get_bytes(entry, "sha256")?
                .try_into()
                .map_err(|_| Error::corrupt("load receipt tensor sha256 is not 32 bytes"))?;
            tensors.push(LoadReceiptTensor {
                name: wu::get_text(entry, "name")?,
                dtype: wu::get_text(entry, "dtype")?,
                shape,
                attested_root: wu::get_hash(entry, "attested_root")?,
                loaded_root: wu::get_hash(entry, "loaded_root")?,
                content_root: wu::get_hash(entry, "content_root")?,
                sha256,
                bytes: wu::get_u64(entry, "bytes")?,
                units_verified: wu::get_u64(entry, "units_verified")?,
            });
        }
        let file_sha256: [u8; 32] = wu::get_bytes(map, "file_sha256")?
            .try_into()
            .map_err(|_| Error::corrupt("load receipt file_sha256 is not 32 bytes"))?;
        let receipt = LoadReceipt {
            checkpoint: wu::get_text(map, "checkpoint")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            replica: wu::get_text(map, "replica")?,
            verification: LoadVerification::parse(&wu::get_text(map, "verification")?)?,
            tensors,
            loaded_set_root: wu::get_hash(map, "loaded_set_root")?,
            attested_set_root: wu::get_hash(map, "attested_set_root")?,
            bytes_produced: wu::get_u64(map, "bytes_produced")?,
            units_verified: wu::get_u64(map, "units_verified")?,
            wall_ms: wu::get_u64(map, "wall_ms")?,
            exact_source_file: match wu::field(map, "exact_source_file")? {
                Value::Bool(value) => *value,
                other => {
                    return Err(Error::malformed(format!(
                        "load receipt exact_source_file is not a bool: {other:?}"
                    )))
                }
            },
            file_sha256,
        };
        // A receipt is only valid when its own roster roots agree.
        if receipt.loaded_set_root != receipt.loaded_roster()? {
            return Err(Error::corrupt(
                "load receipt loaded_set_root does not match its tensor roster",
            ));
        }
        if receipt.attested_set_root != receipt.attested_roster()? {
            return Err(Error::corrupt(
                "load receipt attested_set_root does not match its tensor roster",
            ));
        }
        if receipt.loaded_set_root != receipt.attested_set_root {
            return Err(Error::corrupt(
                "load receipt proves a different root than the checkpoint attests",
            ));
        }
        Ok(receipt)
    }

    /// One-line human summary.
    pub fn summary(&self) -> String {
        format!(
            "checkpoint={} replica={} verification={} tensors={} bytes={} units={} ms={} set_root={}",
            self.checkpoint,
            self.replica,
            self.verification.as_str(),
            self.tensors.len(),
            self.bytes_produced,
            self.units_verified,
            self.wall_ms,
            self.loaded_set_root,
        )
    }

    /// Extra fields for a JSON report.
    pub fn to_json_fields(&self) -> BTreeMap<String, serde_json::Value> {
        let mut out = BTreeMap::new();
        out.insert("checkpoint".to_owned(), self.checkpoint.clone().into());
        out.insert("replica".to_owned(), self.replica.clone().into());
        out.insert(
            "verification".to_owned(),
            self.verification.as_str().to_owned().into(),
        );
        out.insert("tensors".to_owned(), self.tensors.len().into());
        out.insert("bytes_produced".to_owned(), self.bytes_produced.into());
        out.insert("units_verified".to_owned(), self.units_verified.into());
        out.insert("wall_ms".to_owned(), self.wall_ms.into());
        out.insert(
            "loaded_set_root".to_owned(),
            self.loaded_set_root.to_hex().into(),
        );
        out.insert(
            "attested_set_root".to_owned(),
            self.attested_set_root.to_hex().into(),
        );
        out
    }
}
