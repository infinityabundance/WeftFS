//! `weft.materialization.receipt.v1`: a root-to-output receipt a loader can
//! hand to an independent verifier.
//!
//! The load receipt (`weft.load.receipt.v1`) attests the bytes a loader
//! produced. This receipt adds the two things an adapter needs to bind a
//! restored tensor to the store that produced it:
//!
//! * **source objects** — for every stored shard, the directory name, the
//!   resolved object id, the recomputed content root and the recomputed shard
//!   logical root. A verifier re-reads exactly those objects.
//! * **destination** — the tensor's offset inside the checkpoint container and
//!   its stored shard count, so a row says where the bytes belong as well as
//!   what they are.
//!
//! The receipt is derived from the **store**, not from a caller's buffer: the
//! canonical bytes are produced by re-reading every shard through the
//! verifying object path and recomputing every root. `weftfs receipt verify`
//! is a separate process that repeats that derivation and compares field by
//! field; a receipt that names a different manifest, object, root, length or
//! destination is refused.
//!
//! Adapters write a second, small `weft.adapter.receipt.v1` record binding
//! their restored entries (framework FQNs) to materialization rows. The
//! verifier checks that binding too, so neither the adapter's arithmetic nor
//! its hashes are trusted.

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::wcb_util as wu;

pub const MATERIALIZATION_SCHEMA: &str = "weft.materialization.receipt.v1";
pub const MATERIALIZATION_DOMAIN: &str = "weft/materialization/receipt/v1";
pub const MATERIALIZATION_SET_DOMAIN: &str = "weft/materialization/set/v1";
pub const ADAPTER_SCHEMA: &str = "weft.adapter.receipt.v1";
pub const ADAPTER_DOMAIN: &str = "weft/adapter/receipt/v1";
pub const ADAPTER_SET_DOMAIN: &str = "weft/adapter/set/v1";

/// One stored shard, as re-read and re-verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializationObject {
    pub index: u32,
    pub object_name: String,
    pub object_id: Hash32,
    /// Recomputed `H("weft/chunk/v1", shard bytes)`.
    pub content_root: Hash32,
    /// Recomputed `LogicalTensorRoot` of the shard.
    pub logical_root: Hash32,
    pub byte_len: u64,
}

/// One tensor's destination and the objects that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializationTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    /// Recomputed whole-tensor `LogicalTensorRoot`.
    pub logical_root: Hash32,
    /// Recomputed `H("weft/chunk/v1", whole payload)`.
    pub content_root: Hash32,
    /// SHA-256 of the whole payload.
    pub sha256: [u8; 32],
    pub bytes: u64,
    pub source_objects: Vec<MaterializationObject>,
    /// Offset of this tensor's payload in the source container.
    pub destination_offset: u64,
    pub shard_count: u32,
    /// `"exact"`: every recomputed root matched the manifest when the receipt
    /// was written. A receipt is never written otherwise.
    pub verification: String,
}

/// A derived, canonical materialization receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializationReceipt {
    pub checkpoint: String,
    pub manifest_root: Hash32,
    pub manifest_id: Hash32,
    pub replica: String,
    /// Sorted by tensor name.
    pub tensors: Vec<MaterializationTensor>,
    pub set_root: Hash32,
    pub bytes_produced: u64,
    pub objects_read: u64,
    pub backend_bytes_read: u64,
    pub store_reads: u64,
    pub wall_ms: u64,
    pub source_file_bytes: Option<u64>,
    pub source_sha256: Option<[u8; 32]>,
}

impl MaterializationReceipt {
    /// Roster root over `(name, logical_root)`, sorted by name.
    pub fn roster_root(entries: &[(String, Hash32)]) -> Result<Hash32> {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in sorted.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::corrupt(format!(
                    "materialization receipt roster has duplicate tensor `{}`",
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
        Ok(identity::h(
            MATERIALIZATION_SET_DOMAIN,
            &wu::encode(&value)?,
        ))
    }

    pub fn roster(&self) -> Result<Hash32> {
        Self::roster_root(
            &self
                .tensors
                .iter()
                .map(|tensor| (tensor.name.clone(), tensor.logical_root))
                .collect::<Vec<_>>(),
        )
    }

    fn shape_value(shape: &[u64]) -> Value {
        Value::Array(shape.iter().map(|dim| Value::U64(*dim)).collect())
    }

    fn object_value(object: &MaterializationObject) -> Value {
        wu::map([
            ("index", Value::U64(u64::from(object.index))),
            ("object_name", Value::Str(object.object_name.clone())),
            ("object_id", wu::hash_value(object.object_id)),
            ("content_root", wu::hash_value(object.content_root)),
            ("logical_root", wu::hash_value(object.logical_root)),
            ("byte_len", Value::U64(object.byte_len)),
        ])
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(MATERIALIZATION_SCHEMA.to_owned())),
            ("checkpoint", Value::Str(self.checkpoint.clone())),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            ("manifest_id", wu::hash_value(self.manifest_id)),
            ("replica", Value::Str(self.replica.clone())),
            ("verification", Value::Str("full".to_owned())),
            (
                "tensors",
                Value::Array(
                    self.tensors
                        .iter()
                        .map(|tensor| {
                            wu::map([
                                ("name", Value::Str(tensor.name.clone())),
                                ("dtype", Value::Str(tensor.dtype.clone())),
                                ("shape", Self::shape_value(&tensor.shape)),
                                ("logical_root", wu::hash_value(tensor.logical_root)),
                                ("content_root", wu::hash_value(tensor.content_root)),
                                ("sha256", Value::Bytes(tensor.sha256.to_vec())),
                                ("bytes", Value::U64(tensor.bytes)),
                                (
                                    "source_objects",
                                    Value::Array(
                                        tensor
                                            .source_objects
                                            .iter()
                                            .map(Self::object_value)
                                            .collect(),
                                    ),
                                ),
                                (
                                    "destination",
                                    wu::map([
                                        ("kind", Value::Str("checkpoint-tensor".to_owned())),
                                        ("offset", Value::U64(tensor.destination_offset)),
                                        ("shard_count", Value::U64(u64::from(tensor.shard_count))),
                                    ]),
                                ),
                                ("verification", Value::Str(tensor.verification.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("set_root", wu::hash_value(self.set_root)),
            ("bytes_produced", Value::U64(self.bytes_produced)),
            ("objects_read", Value::U64(self.objects_read)),
            ("backend_bytes_read", Value::U64(self.backend_bytes_read)),
            ("store_reads", Value::U64(self.store_reads)),
            ("wall_ms", Value::U64(self.wall_ms)),
            (
                "source_file_bytes",
                match self.source_file_bytes {
                    Some(value) => Value::U64(value),
                    None => Value::Null,
                },
            ),
            (
                "source_sha256",
                match self.source_sha256 {
                    Some(value) => Value::Bytes(value.to_vec()),
                    None => Value::Null,
                },
            ),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(MATERIALIZATION_DOMAIN, &self.encode()?))
    }

    pub fn decode(bytes: &[u8]) -> Result<MaterializationReceipt> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "checkpoint",
                "manifest_root",
                "manifest_id",
                "replica",
                "verification",
                "tensors",
                "set_root",
                "bytes_produced",
                "objects_read",
                "backend_bytes_read",
                "store_reads",
                "wall_ms",
                "source_file_bytes",
                "source_sha256",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != MATERIALIZATION_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "materialization receipt schema `{schema}` is not `{MATERIALIZATION_SCHEMA}`"
            )));
        }
        if wu::get_text(map, "verification")? != "full" {
            return Err(Error::unsupported_version(
                "materialization receipt verification is not `full`",
            ));
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
                    "logical_root",
                    "content_root",
                    "sha256",
                    "bytes",
                    "source_objects",
                    "destination",
                    "verification",
                ],
            )?;
            let mut shape = Vec::new();
            for dim in wu::get_array(entry, "shape")? {
                match dim {
                    Value::U64(value) => shape.push(*value),
                    other => {
                        return Err(Error::malformed(format!(
                            "materialization shape entry is not u64: {other:?}"
                        )))
                    }
                }
            }
            let mut objects = Vec::new();
            for object in wu::get_array(entry, "source_objects")? {
                let object = wu::as_map(object)?;
                wu::check_keys(
                    object,
                    &[
                        "index",
                        "object_name",
                        "object_id",
                        "content_root",
                        "logical_root",
                        "byte_len",
                    ],
                )?;
                objects.push(MaterializationObject {
                    index: u32::try_from(wu::get_u64(object, "index")?)
                        .map_err(|_| Error::corrupt("shard index does not fit u32"))?,
                    object_name: wu::get_text(object, "object_name")?,
                    object_id: wu::get_hash(object, "object_id")?,
                    content_root: wu::get_hash(object, "content_root")?,
                    logical_root: wu::get_hash(object, "logical_root")?,
                    byte_len: wu::get_u64(object, "byte_len")?,
                });
            }
            let map_destination = wu::get_map(entry, "destination")?;
            wu::check_keys(&map_destination, &["kind", "offset", "shard_count"])?;
            if wu::get_text(&map_destination, "kind")? != "checkpoint-tensor" {
                return Err(Error::unsupported_version(
                    "materialization destination kind is not `checkpoint-tensor`",
                ));
            }
            let sha256: [u8; 32] = wu::get_bytes(entry, "sha256")?
                .try_into()
                .map_err(|_| Error::corrupt("materialization sha256 is not 32 bytes"))?;
            tensors.push(MaterializationTensor {
                name: wu::get_text(entry, "name")?,
                dtype: wu::get_text(entry, "dtype")?,
                shape,
                logical_root: wu::get_hash(entry, "logical_root")?,
                content_root: wu::get_hash(entry, "content_root")?,
                sha256,
                bytes: wu::get_u64(entry, "bytes")?,
                source_objects: objects,
                destination_offset: wu::get_u64(&map_destination, "offset")?,
                shard_count: u32::try_from(wu::get_u64(&map_destination, "shard_count")?)
                    .map_err(|_| Error::corrupt("shard_count does not fit u32"))?,
                verification: wu::get_text(entry, "verification")?,
            });
        }
        let source_sha256 =
            match wu::field(map, "source_sha256")? {
                Value::Null => None,
                Value::Bytes(bytes) => Some(bytes.as_slice().try_into().map_err(|_| {
                    Error::corrupt("materialization source_sha256 is not 32 bytes")
                })?),
                other => {
                    return Err(Error::malformed(format!(
                        "materialization source_sha256 is not bytes: {other:?}"
                    )))
                }
            };
        let receipt = MaterializationReceipt {
            checkpoint: wu::get_text(map, "checkpoint")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            manifest_id: wu::get_hash(map, "manifest_id")?,
            replica: wu::get_text(map, "replica")?,
            tensors,
            set_root: wu::get_hash(map, "set_root")?,
            bytes_produced: wu::get_u64(map, "bytes_produced")?,
            objects_read: wu::get_u64(map, "objects_read")?,
            backend_bytes_read: wu::get_u64(map, "backend_bytes_read")?,
            store_reads: wu::get_u64(map, "store_reads")?,
            wall_ms: wu::get_u64(map, "wall_ms")?,
            source_file_bytes: match wu::field(map, "source_file_bytes")? {
                Value::Null => None,
                Value::U64(value) => Some(*value),
                other => {
                    return Err(Error::malformed(format!(
                        "materialization source_file_bytes is not u64: {other:?}"
                    )))
                }
            },
            source_sha256,
        };
        if receipt.set_root != receipt.roster()? {
            return Err(Error::corrupt(
                "materialization receipt set_root does not match its tensor roster",
            ));
        }
        if receipt.tensors.iter().any(|tensor| {
            tensor.verification != "exact"
                || tensor.source_objects.is_empty()
                || tensor
                    .source_objects
                    .iter()
                    .any(|object| object.byte_len == 0)
        }) {
            return Err(Error::corrupt(
                "materialization receipt has a non-exact row or an empty source-object list",
            ));
        }
        Ok(receipt)
    }

    pub fn summary(&self) -> String {
        format!(
            "checkpoint={} replica={} tensors={} bytes={} objects={} set_root={}",
            self.checkpoint,
            self.replica,
            self.tensors.len(),
            self.bytes_produced,
            self.objects_read,
            self.set_root
        )
    }
}

/// One adapter binding row: a framework entry to a materialization row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterEntry {
    /// Framework path (DCP FQN or an Orbax canonical leaf path).
    pub fqn: String,
    /// Materialization tensor name.
    pub tensor_name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub bytes: u64,
    pub sha256: [u8; 32],
    pub content_root: Hash32,
}

/// `weft.adapter.receipt.v1`, written by a framework adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterReceipt {
    pub adapter: String,
    pub checkpoint: String,
    /// `MaterializationReceipt::root()` the adapter bound against.
    pub materialization_root: Hash32,
    pub manifest_root: Hash32,
    pub producer: String,
    /// Sorted by `fqn`.
    pub entries: Vec<AdapterEntry>,
    pub set_root: Hash32,
    pub wall_ms: u64,
}

impl AdapterReceipt {
    pub fn set_root(entries: &[(String, String, Hash32)]) -> Result<Hash32> {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in sorted.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::corrupt(format!(
                    "adapter receipt has duplicate entry `{}`",
                    pair[0].0
                )));
            }
        }
        let value = Value::Array(
            sorted
                .iter()
                .map(|(fqn, tensor, root)| {
                    wu::map([
                        ("fqn", Value::Str(fqn.clone())),
                        ("tensor_name", Value::Str(tensor.clone())),
                        ("content_root", wu::hash_value(*root)),
                    ])
                })
                .collect(),
        );
        Ok(identity::h(ADAPTER_SET_DOMAIN, &wu::encode(&value)?))
    }

    pub fn roster(&self) -> Result<Hash32> {
        Self::set_root(
            &self
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.fqn.clone(),
                        entry.tensor_name.clone(),
                        entry.content_root,
                    )
                })
                .collect::<Vec<_>>(),
        )
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(ADAPTER_SCHEMA.to_owned())),
            ("adapter", Value::Str(self.adapter.clone())),
            ("checkpoint", Value::Str(self.checkpoint.clone())),
            (
                "materialization_root",
                wu::hash_value(self.materialization_root),
            ),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            ("producer", Value::Str(self.producer.clone())),
            (
                "entries",
                Value::Array(
                    self.entries
                        .iter()
                        .map(|entry| {
                            wu::map([
                                ("fqn", Value::Str(entry.fqn.clone())),
                                ("tensor_name", Value::Str(entry.tensor_name.clone())),
                                ("dtype", Value::Str(entry.dtype.clone())),
                                (
                                    "shape",
                                    Value::Array(
                                        entry.shape.iter().map(|dim| Value::U64(*dim)).collect(),
                                    ),
                                ),
                                ("bytes", Value::U64(entry.bytes)),
                                ("sha256", Value::Bytes(entry.sha256.to_vec())),
                                ("content_root", wu::hash_value(entry.content_root)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("set_root", wu::hash_value(self.set_root)),
            ("wall_ms", Value::U64(self.wall_ms)),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(ADAPTER_DOMAIN, &self.encode()?))
    }

    pub fn decode(bytes: &[u8]) -> Result<AdapterReceipt> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "adapter",
                "checkpoint",
                "materialization_root",
                "manifest_root",
                "producer",
                "entries",
                "set_root",
                "wall_ms",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != ADAPTER_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "adapter receipt schema `{schema}` is not `{ADAPTER_SCHEMA}`"
            )));
        }
        let mut entries = Vec::new();
        for entry in wu::get_array(map, "entries")? {
            let entry = wu::as_map(entry)?;
            wu::check_keys(
                entry,
                &[
                    "fqn",
                    "tensor_name",
                    "dtype",
                    "shape",
                    "bytes",
                    "sha256",
                    "content_root",
                ],
            )?;
            let mut shape = Vec::new();
            for dim in wu::get_array(entry, "shape")? {
                match dim {
                    Value::U64(value) => shape.push(*value),
                    other => {
                        return Err(Error::malformed(format!(
                            "adapter entry shape is not u64: {other:?}"
                        )))
                    }
                }
            }
            let sha256: [u8; 32] = wu::get_bytes(entry, "sha256")?
                .try_into()
                .map_err(|_| Error::corrupt("adapter entry sha256 is not 32 bytes"))?;
            entries.push(AdapterEntry {
                fqn: wu::get_text(entry, "fqn")?,
                tensor_name: wu::get_text(entry, "tensor_name")?,
                dtype: wu::get_text(entry, "dtype")?,
                shape,
                bytes: wu::get_u64(entry, "bytes")?,
                sha256,
                content_root: wu::get_hash(entry, "content_root")?,
            });
        }
        let receipt = AdapterReceipt {
            adapter: wu::get_text(map, "adapter")?,
            checkpoint: wu::get_text(map, "checkpoint")?,
            materialization_root: wu::get_hash(map, "materialization_root")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            producer: wu::get_text(map, "producer")?,
            entries,
            set_root: wu::get_hash(map, "set_root")?,
            wall_ms: wu::get_u64(map, "wall_ms")?,
        };
        if receipt.set_root != receipt.roster()? {
            return Err(Error::corrupt(
                "adapter receipt set_root does not match its entry roster",
            ));
        }
        Ok(receipt)
    }
}

/// Outcome of one offline verification.
#[derive(Clone, Debug, Default)]
pub struct MaterializationVerifyReport {
    pub tensors_checked: usize,
    pub objects_checked: usize,
    pub adapter_entries_checked: usize,
    pub mismatches: Vec<String>,
}

impl MaterializationVerifyReport {
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Decode and fully re-derive a materialization receipt from the store,
/// optionally checking an adapter binding against it.
///
/// This is the independent verifier: it reads the receipt, then re-reads every
/// named source object through the store's verifying path and recomputes every
/// root and hash. It never uses a value from the receipt as a root of trust.
pub fn verify_receipt(
    store: &crate::store::Store,
    receipt_bytes: &[u8],
    adapter_bytes: Option<&[u8]>,
) -> Result<MaterializationVerifyReport> {
    let receipt = MaterializationReceipt::decode(receipt_bytes)?;
    let (manifest, manifest_root, manifest_id, rows) =
        store.derive_materialization_rows(&receipt.checkpoint)?;
    let mut report = MaterializationVerifyReport::default();
    if manifest_root != receipt.manifest_root {
        report.mismatches.push(format!(
            "manifest root {} does not match the store's {}",
            receipt.manifest_root, manifest_root
        ));
    }
    if manifest_id != receipt.manifest_id {
        report.mismatches.push(format!(
            "manifest object id {} does not match the store's {}",
            receipt.manifest_id, manifest_id
        ));
    }
    if rows.len() != receipt.tensors.len() {
        report.mismatches.push(format!(
            "receipt has {} tensors, the store derives {}",
            receipt.tensors.len(),
            rows.len()
        ));
    }
    let by_name: std::collections::BTreeMap<&str, &MaterializationTensor> =
        rows.iter().map(|row| (row.name.as_str(), row)).collect();
    for row in &receipt.tensors {
        report.tensors_checked += 1;
        let Some(derived) = by_name.get(row.name.as_str()) else {
            report
                .mismatches
                .push(format!("receipt tensor `{}` is not in the store", row.name));
            continue;
        };
        if **derived != *row {
            if derived.logical_root != row.logical_root {
                report.mismatches.push(format!(
                    "tensor `{}` logical root {} != derived {}",
                    row.name, row.logical_root, derived.logical_root
                ));
            }
            if derived.content_root != row.content_root {
                report.mismatches.push(format!(
                    "tensor `{}` content root {} != derived {}",
                    row.name, row.content_root, derived.content_root
                ));
            }
            if derived.sha256 != row.sha256 {
                report.mismatches.push(format!(
                    "tensor `{}` sha256 differs from the store-derived payload",
                    row.name
                ));
            }
            if derived.dtype != row.dtype || derived.shape != row.shape {
                report
                    .mismatches
                    .push(format!("tensor `{}` dtype/shape differs", row.name));
            }
            if derived.bytes != row.bytes {
                report
                    .mismatches
                    .push(format!("tensor `{}` byte length differs", row.name));
            }
            if derived.source_objects != row.source_objects {
                report.mismatches.push(format!(
                    "tensor `{}` source-object list differs from the store",
                    row.name
                ));
            }
            if derived.destination_offset != row.destination_offset
                || derived.shard_count != row.shard_count
            {
                report
                    .mismatches
                    .push(format!("tensor `{}` destination differs", row.name));
            }
        }
        report.objects_checked += row.source_objects.len();
    }
    let derived_set = MaterializationReceipt::roster_root(
        &rows
            .iter()
            .map(|row| (row.name.clone(), row.logical_root))
            .collect::<Vec<_>>(),
    )?;
    if derived_set != receipt.set_root {
        report
            .mismatches
            .push("receipt set_root differs from the store-derived roster".to_owned());
    }
    let _ = manifest;

    if let Some(bytes) = adapter_bytes {
        let adapter = AdapterReceipt::decode(bytes)?;
        if adapter.materialization_root != receipt.root()? {
            report.mismatches.push(format!(
                "adapter receipt binds materialization root {} but the receipt is {}",
                adapter.materialization_root,
                receipt.root()?
            ));
        }
        if adapter.manifest_root != receipt.manifest_root {
            report
                .mismatches
                .push("adapter receipt manifest root differs".to_owned());
        }
        if adapter.checkpoint != receipt.checkpoint {
            report
                .mismatches
                .push("adapter receipt names a different checkpoint".to_owned());
        }
        for entry in &adapter.entries {
            report.adapter_entries_checked += 1;
            let Some(row) = by_name.get(entry.tensor_name.as_str()) else {
                report.mismatches.push(format!(
                    "adapter entry `{}` names unknown tensor `{}`",
                    entry.fqn, entry.tensor_name
                ));
                continue;
            };
            if entry.dtype != row.dtype
                || entry.shape != row.shape
                || entry.bytes != row.bytes
                || entry.sha256 != row.sha256
                || entry.content_root != row.content_root
            {
                report.mismatches.push(format!(
                    "adapter entry `{}` does not match materialization row `{}`",
                    entry.fqn, entry.tensor_name
                ));
            }
        }
    }
    Ok(report)
}
