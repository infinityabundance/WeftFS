//! Recovery-first checkpoint restore.
//!
//! A resumed training step needs a small *required set* of tensors (and their
//! exact bytes) before it can take a step. This module streams and verifies
//! that set first, in representation-dependency order, and only then restores
//! the rest of the checkpoint. The caller gets:
//!
//! * `time_to_first_usable_ms` — wall time until the first required tensor was
//!   verified and written;
//! * `bytes_fetched_before_first_usable` — backend bytes read before that
//!   moment (the manifest and the required shards only);
//! * `time_to_complete_ms` — wall time for the whole container.
//!
//! A host-side verified cache (`--first-cache DIR`) lets a restarted process
//! reuse the already-verified required set. Cached bytes are **not** trusted:
//! the loader re-checks the SHA-256, the BLAKE3 content root and the
//! `LogicalTensorRoot` against the checkpoint manifest before using them, and
//! falls back to the store on any mismatch.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};
use weftcodec::tensor as tc;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::checkpoint::{CheckpointRestoreOptions, CheckpointTensor, RestoredTensor, Sha256Digest};
use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::recovery;
use crate::representation::TensorElementDtype;
use crate::safetensors;
use crate::store::Store;
use crate::wcb_util as wu;

/// Canonical schema for one cached, verified tensor.
pub const RECOVERY_CACHE_SCHEMA: &str = "weft.recovery.cache.v1";

/// Options for a recovery-first restore.
#[derive(Clone, Debug, Default)]
pub struct RecoveryFirstOptions {
    /// Tensor names the caller needs before it can resume. Order in this list
    /// is not significant; the loader reorders by representation dependency
    /// depth and then by name.
    pub required: Vec<String>,
    /// Host directory for the verified required-set cache.
    pub cache_dir: Option<PathBuf>,
    /// Test hook: abort the process immediately after the required set has
    /// been verified, written and cached, before the rest is restored.
    pub abort_after_required: bool,
}

/// One cached tensor served without touching the object store.
#[derive(Clone, Debug)]
pub struct CachedTensor {
    pub name: String,
    pub bytes: u64,
    pub logical_root: Hash32,
}

/// Result of a recovery-first restore.
#[derive(Clone, Debug)]
pub struct RecoveryFirstReport {
    pub checkpoint: String,
    pub manifest_root: Hash32,
    /// Required tensors in the order they were restored.
    pub required_order: Vec<(String, u32)>,
    pub cached_required: Vec<CachedTensor>,
    pub tensors: Vec<RestoredTensor>,
    pub file_sha256: Sha256Digest,
    pub file_bytes: u64,
    pub exact_source_file: bool,
    pub time_to_first_usable_ms: f64,
    pub time_to_complete_ms: f64,
    pub bytes_fetched_before_first_usable: u64,
    pub bytes_fetched_total: u64,
    pub store_reads_before_first_usable: u64,
}

impl Store {
    /// Restore `required` first, then the rest of the checkpoint.
    ///
    /// `writer` must be readable because the exact-source check re-hashes the
    /// assembled container. Output is the same complete safetensors file a
    /// plain restore would produce; only the production order differs.
    pub fn restore_checkpoint_recovery_first<W: Read + Write + Seek>(
        &self,
        name: &str,
        options: &CheckpointRestoreOptions,
        recovery: &RecoveryFirstOptions,
        writer: &mut W,
    ) -> Result<RecoveryFirstReport> {
        if options.tensor.is_some() || options.region.is_some() {
            return Err(Error::invalid_argument(
                "recovery-first restore takes a complete checkpoint (use --first-needed for the required set)",
            ));
        }
        if recovery.required.is_empty() {
            return Err(Error::invalid_argument(
                "--first-needed lists no tensor names",
            ));
        }
        let started = Instant::now();
        let manifest = self.load_checkpoint(name)?;
        let manifest_root = manifest.root()?;
        let limits = Limits::default();

        // Validate the required set before writing anything.
        let mut required: Vec<&CheckpointTensor> = Vec::with_capacity(recovery.required.len());
        for name in &recovery.required {
            required.push(manifest.tensor(name)?);
        }
        // Dependency order: tensors whose stored representation carries no base
        // frame first. Two tensors at the same depth keep manifest order.
        let mut depths = Vec::with_capacity(required.len());
        for tensor in &required {
            let mut depth = 0u32;
            for shard in &tensor.shards {
                depth = depth.max(self.shard_dependency_depth(shard.object_id)?);
            }
            depths.push(depth);
        }
        let mut order: Vec<usize> = (0..required.len()).collect();
        order.sort_by_key(|&index| (depths[index], required[index].name.clone()));
        let required_order: Vec<(String, u32)> = order
            .iter()
            .map(|&index| (required[index].name.clone(), depths[index]))
            .collect();

        let header = manifest.source_header.clone().ok_or_else(|| {
            Error::unsupported_version(
                "recovery-first restore needs a checkpoint with a recorded source header",
            )
        })?;
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
        writer
            .write_all(&header)
            .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;

        let mut cached_required = Vec::new();
        let mut first_usable_ms: Option<f64> = None;
        let mut bytes_before_first_usable = 0u64;
        let mut reads_before_first_usable = 0u64;
        let mut restored: BTreeMap<String, RestoredTensor> = BTreeMap::new();
        let before = self.backend_stats();

        for &index in &order {
            let tensor = required[index];
            let (bytes, row, cached) =
                self.restore_one_recovery(name, tensor, recovery.cache_dir.as_deref(), &limits)?;
            writer
                .seek(SeekFrom::Start(header.len() as u64 + tensor.source_offset))
                .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
            writer
                .write_all(&bytes)
                .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
            if let Some(cached) = cached {
                cached_required.push(cached);
            }
            if first_usable_ms.is_none() {
                first_usable_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                let stats = self.backend_stats();
                bytes_before_first_usable = stats.bytes_read.saturating_sub(before.bytes_read);
                reads_before_first_usable = stats
                    .reads
                    .saturating_sub(before.reads)
                    .saturating_add(stats.range_reads.saturating_sub(before.range_reads));
            }
            restored.insert(tensor.name.clone(), row);
        }

        if recovery.abort_after_required {
            // Test hook: die after the required set is durable and cached.
            std::process::abort();
        }

        // Everything else, in source order, skipping the required set.
        let mut remaining: Vec<&CheckpointTensor> = manifest
            .tensors
            .iter()
            .filter(|tensor| !recovery.required.iter().any(|name| name == &tensor.name))
            .collect();
        remaining.sort_by_key(|tensor| tensor.source_offset);
        for tensor in remaining {
            let (bytes, row) = self.restore_one_plain(name, tensor, &limits)?;
            writer
                .seek(SeekFrom::Start(header.len() as u64 + tensor.source_offset))
                .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
            writer
                .write_all(&bytes)
                .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
            restored.insert(tensor.name.clone(), row);
        }
        writer
            .flush()
            .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;

        // Exact-source check: re-hash the assembled container.
        let end = writer
            .seek(SeekFrom::End(0))
            .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let count = writer
                .read(&mut buffer)
                .map_err(|err| Error::io(format!("recovery-first spool: {err}")))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let file_sha256: Sha256Digest = hasher.finalize().into();
        let exact_source_file = manifest
            .source_sha256
            .map(|expected| expected == file_sha256)
            .unwrap_or(false);
        if manifest.complete && !exact_source_file {
            return Err(Error::corrupt(format!(
                "recovery-first restore produced SHA-256 {} but the checkpoint records {}",
                hex::encode(file_sha256),
                manifest.source_sha256.map(hex::encode).unwrap_or_default()
            )));
        }
        let total = self.backend_stats();
        let tensors = manifest
            .tensors
            .iter()
            .filter_map(|tensor| restored.remove(&tensor.name))
            .collect::<Vec<_>>();
        Ok(RecoveryFirstReport {
            checkpoint: name.to_owned(),
            manifest_root,
            required_order,
            cached_required,
            tensors,
            file_sha256,
            file_bytes: end,
            exact_source_file,
            time_to_first_usable_ms: first_usable_ms.unwrap_or(0.0),
            time_to_complete_ms: started.elapsed().as_secs_f64() * 1000.0,
            bytes_fetched_before_first_usable: bytes_before_first_usable,
            bytes_fetched_total: total.bytes_read.saturating_sub(before.bytes_read),
            store_reads_before_first_usable: reads_before_first_usable,
        })
    }

    /// Representation dependency depth of one shard object (0 = independent).
    pub fn shard_dependency_depth(&self, id: Hash32) -> Result<u32> {
        let record = self.object_meta(id)?;
        self.dependency_depth_of_content(record.content, 0)
    }

    fn dependency_depth_of_content(&self, content: Hash32, depth: u32) -> Result<u32> {
        if depth as usize > crate::representation::MAX_CODEC_DEPENDENCY_DEPTH {
            return Err(Error::corrupt(
                "recovery-first dependency depth exceeds the codec bound",
            ));
        }
        let manifest =
            self.read_manifest_keyed(content)?;
        let mut deepest = depth;
        if let Some(representation) = &manifest.representation {
            for dependency in &representation.dependencies {
                deepest =
                    deepest.max(self.dependency_depth_of_content(*dependency, depth + 1)?);
            }
        }
        Ok(deepest)
    }

    fn restore_one_recovery(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        cache_dir: Option<&Path>,
        limits: &Limits,
    ) -> Result<(Vec<u8>, RestoredTensor, Option<CachedTensor>)> {
        if let Some(dir) = cache_dir {
            if let Some(cached) = self.read_recovery_cache(dir, tensor, limits)? {
                let row = restored_row(tensor, &cached.bytes, 1, true)?;
                return Ok((
                    cached.bytes,
                    row,
                    Some(CachedTensor {
                        name: tensor.name.clone(),
                        bytes: tensor.element_bytes,
                        logical_root: tensor.logical_root,
                    }),
                ));
            }
        }
        let (bytes, row) = self.restore_one_plain(checkpoint, tensor, limits)?;
        if let Some(dir) = cache_dir {
            self.write_recovery_cache(dir, tensor, &bytes, limits)?;
        }
        Ok((bytes, row, None))
    }

    fn restore_one_plain(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        limits: &Limits,
    ) -> Result<(Vec<u8>, RestoredTensor)> {
        let (bytes, row) = self.restore_named_tensor_row(checkpoint, tensor, limits)?;
        Ok((bytes, row))
    }

    fn restore_named_tensor_row(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        _limits: &Limits,
    ) -> Result<(Vec<u8>, RestoredTensor)> {
        let restored = self.restore_named_tensor_at(None, checkpoint, &tensor.name, None, None)?;
        Ok((restored.bytes, restored.report))
    }

    fn cache_path(dir: &Path, name: &str) -> PathBuf {
        let digest = safetensors::sha256(name.as_bytes());
        dir.join(format!("{}.wcb", hex::encode(digest)))
    }

    fn read_recovery_cache(
        &self,
        dir: &Path,
        tensor: &CheckpointTensor,
        limits: &Limits,
    ) -> Result<Option<CachedBytes>> {
        let path = Self::cache_path(dir, &tensor.name);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(Error::io(format!("{}: {err}", path.display())));
            }
        };
        let value = match wu::decode(&bytes) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let map = match wu::as_map(&value) {
            Ok(map) => map,
            Err(_) => return Ok(None),
        };
        if wu::get_text(map, "schema").ok().as_deref() != Some(RECOVERY_CACHE_SCHEMA) {
            return Ok(None);
        }
        if wu::get_text(map, "name").ok().as_deref() != Some(tensor.name.as_str()) {
            return Ok(None);
        }
        let data = wu::get_bytes(map, "data")?;
        let recorded_sha: Sha256Digest = match wu::get_bytes(map, "sha256") {
            Ok(value) => match value.try_into() {
                Ok(value) => value,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };
        if safetensors::sha256(&data) != recorded_sha {
            return Ok(None);
        }
        if identity::content_hash(&data) != tensor.content_root {
            return Ok(None);
        }
        let dtype = TensorElementDtype::parse(&tensor.dtype)?;
        let physical = dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("cached tensor has no physical dtype"))?;
        let root = tc::logical_root(physical, &tensor.shape, &data, limits)?;
        if root != tensor.logical_root {
            return Ok(None);
        }
        Ok(Some(CachedBytes { bytes: data }))
    }

    fn write_recovery_cache(
        &self,
        dir: &Path,
        tensor: &CheckpointTensor,
        bytes: &[u8],
        _limits: &Limits,
    ) -> Result<()> {
        std::fs::create_dir_all(dir)
            .map_err(|err| Error::io(format!("{}: {err}", dir.display())))?;
        let value = wu::map([
            ("schema", Value::Str(RECOVERY_CACHE_SCHEMA.to_owned())),
            ("name", Value::Str(tensor.name.clone())),
            ("dtype", Value::Str(tensor.dtype.clone())),
            (
                "shape",
                Value::Array(tensor.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("logical_root", wu::hash_value(tensor.logical_root)),
            ("content_root", wu::hash_value(tensor.content_root)),
            ("sha256", Value::Bytes(safetensors::sha256(bytes).to_vec())),
            ("data", Value::Bytes(bytes.to_vec())),
        ]);
        let encoded = wu::encode(&value)?;
        let path = Self::cache_path(dir, &tensor.name);
        let tmp = path.with_extension("tmp");
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)
                .map_err(|err| Error::io(format!("{}: {err}", tmp.display())))?;
            file.write_all(&encoded)
                .map_err(|err| Error::io(format!("{}: {err}", tmp.display())))?;
            file.sync_all()
                .map_err(|err| Error::io(format!("{}: {err}", tmp.display())))?;
        }
        std::fs::rename(&tmp, &path)
            .map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
        Ok(())
    }
}

struct CachedBytes {
    bytes: Vec<u8>,
}

fn restored_row(
    tensor: &CheckpointTensor,
    bytes: &[u8],
    units: u64,
    verified: bool,
) -> Result<RestoredTensor> {
    let dtype = TensorElementDtype::parse(&tensor.dtype)?;
    let physical = dtype
        .physical_tensor_dtype()
        .ok_or_else(|| Error::unsupported_version("tensor has no physical dtype"))?;
    let root = tc::logical_root(physical, &tensor.shape, bytes, &Limits::default())?;
    if root != tensor.logical_root {
        return Err(Error::corrupt(format!(
            "cached tensor `{}` root {root} does not match the manifest's {}",
            tensor.name, tensor.logical_root
        )));
    }
    Ok(RestoredTensor {
        name: tensor.name.clone(),
        dtype: tensor.dtype.clone(),
        shape: tensor.shape.clone(),
        full_shape: tensor.shape.clone(),
        full_logical_root: tensor.logical_root,
        restored_content_root: identity::content_hash(bytes),
        bytes: bytes.len() as u64,
        shards_read: 1,
        units_touched: units,
        full_root_verified: verified,
        shard_roots_verified: verified,
        topology_root: None,
        sha256: safetensors::sha256(bytes),
    })
}
