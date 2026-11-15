//! Training-state artifacts: optimizer state, gradients, attention masks.
//!
//! These were previously *specified only*. They are now canonical WCB schemas
//! with fixtures and replay tests:
//!
//! * `weft.optimizer-state.v1` — AdamW `exp_avg` / `exp_avg_sq` / `step` per
//!   parameter, bound to an exact model checkpoint by content root.
//! * `weft.gradient.v1` — one gradient tensor per parameter for a declared
//!   training step, loss and batch, bound to the same model reference.
//! * `weft.attention-mask.v1` — one real mask tensor (dtype `u8`), with its
//!   kind, shape, sequence length and mask-encoding statement.
//!
//! Every member tensor is stored through the existing tensor planner and read
//! back through the verified tensor path, so restore is exact (byte-for-byte)
//! rather than approximate. Nothing here changes a frozen wire byte, mode
//! number or root.
//!
//! RL trajectories are **not** implemented: see `docs/STATUS.md` and
//! `spec/WEFT-TRAINING.md` §6. No descriptor exists without a consumer.

use std::collections::{BTreeMap, BTreeSet};

use weftcodec::wcb::Value;

use crate::adapter::{BaseRef, TensorRef};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use crate::state::{ObjectKind, State};
use crate::store::Store;
use crate::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use crate::wcb_util as wu;

/// Canonical optimizer-state schema.
pub const OPTIMIZER_SCHEMA: &str = "weft.optimizer-state.v1";
/// Domain separation for the optimizer-state identity.
pub const OPTIMIZER_DOMAIN: &str = "weft/optimizer-state/v1";
/// Canonical gradient schema.
pub const GRADIENT_SCHEMA: &str = "weft.gradient.v1";
/// Domain separation for the gradient identity.
pub const GRADIENT_DOMAIN: &str = "weft/gradient/v1";
/// Canonical attention-mask schema.
pub const ATTENTION_MASK_SCHEMA: &str = "weft.attention-mask.v1";
/// Domain separation for the attention-mask identity.
pub const ATTENTION_MASK_DOMAIN: &str = "weft/attention-mask/v1";
/// Store directory that holds training-state artifacts.
pub const TRAINING_ROOT: &str = "training";

/// Maximum number of parameters one training-state artifact may declare.
pub const MAX_TRAINING_PARAMS: usize = 65_536;
/// Maximum canonical bytes of one training-state manifest.
pub const MAX_TRAINING_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

fn u64_array(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u64>> {
    match wu::field(map, key)? {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::U64(value) => out.push(*value),
                    _ => return Err(Error::malformed(format!("key `{key}` must hold u64s"))),
                }
            }
            Ok(out)
        }
        _ => Err(Error::malformed(format!("key `{key}` must be an array"))),
    }
}

fn seed_from(label: &str) -> [u8; 32] {
    *identity::h("weft/training/seed/v1", label.as_bytes()).as_bytes()
}

fn entry_child(store: &Store, parent: Hash32, name: &str) -> Option<Hash32> {
    store
        .current_state()
        .entries
        .values()
        .find(|entry| entry.parent == parent && entry.name == name.as_bytes())
        .map(|entry| entry.target)
}

fn ensure_dir(
    store: &mut Store,
    parent: Hash32,
    name: &str,
    seed: Option<&[u8; 32]>,
) -> Result<Hash32> {
    if let Some(id) = entry_child(store, parent, name) {
        if store.object_meta(id)?.kind != ObjectKind::Dir {
            return Err(Error::not_dir(format!(
                "`{name}` exists and is not a directory"
            )));
        }
        return Ok(id);
    }
    let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
    match seed {
        Some(seed) => store
            .mkdir_seeded(parent, name.as_bytes(), 0o755, uid, gid, seed)
            .map_err(|err| {
                if err.class() == Class::Exists {
                    Error::exists(format!("directory `{name}` appeared concurrently"))
                } else {
                    err
                }
            }),
        None => store.mkdir(parent, name.as_bytes(), 0o755, uid, gid),
    }
}

/// AdamW hyperparameters as exact f64 bit patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdamwHyper {
    pub lr_bits: u64,
    pub beta1_bits: u64,
    pub beta2_bits: u64,
    pub eps_bits: u64,
    pub weight_decay_bits: u64,
}

impl AdamwHyper {
    pub const KEYS: &'static [&'static str] = &[
        "beta1_bits",
        "beta2_bits",
        "eps_bits",
        "lr_bits",
        "weight_decay_bits",
    ];

    pub fn new(lr: f64, beta1: f64, beta2: f64, eps: f64, weight_decay: f64) -> Self {
        AdamwHyper {
            lr_bits: lr.to_bits(),
            beta1_bits: beta1.to_bits(),
            beta2_bits: beta2.to_bits(),
            eps_bits: eps.to_bits(),
            weight_decay_bits: weight_decay.to_bits(),
        }
    }

    fn to_value(&self) -> Value {
        wu::map([
            ("lr_bits", Value::U64(self.lr_bits)),
            ("beta1_bits", Value::U64(self.beta1_bits)),
            ("beta2_bits", Value::U64(self.beta2_bits)),
            ("eps_bits", Value::U64(self.eps_bits)),
            ("weight_decay_bits", Value::U64(self.weight_decay_bits)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let hyper = AdamwHyper {
            lr_bits: wu::get_u64(map, "lr_bits")?,
            beta1_bits: wu::get_u64(map, "beta1_bits")?,
            beta2_bits: wu::get_u64(map, "beta2_bits")?,
            eps_bits: wu::get_u64(map, "eps_bits")?,
            weight_decay_bits: wu::get_u64(map, "weight_decay_bits")?,
        };
        hyper.validate()?;
        Ok(hyper)
    }

    pub fn validate(&self) -> Result<()> {
        for (label, bits) in [
            ("lr", self.lr_bits),
            ("beta1", self.beta1_bits),
            ("beta2", self.beta2_bits),
            ("eps", self.eps_bits),
            ("weight_decay", self.weight_decay_bits),
        ] {
            if !f64::from_bits(bits).is_finite() {
                return Err(Error::malformed(format!(
                    "AdamW hyperparameter `{label}` must be finite"
                )));
            }
        }
        Ok(())
    }
}

/// One parameter's AdamW moments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizerParam {
    pub name: String,
    /// Content root the model checkpoint records for this tensor.
    pub base_content: Hash32,
    pub exp_avg: TensorRef,
    pub exp_avg_sq: TensorRef,
    pub step: u64,
}

impl OptimizerParam {
    const KEYS: &'static [&'static str] =
        &["base_content", "exp_avg", "exp_avg_sq", "name", "step"];

    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("base_content", wu::hash_value(self.base_content)),
            ("exp_avg", self.exp_avg.to_value()),
            ("exp_avg_sq", self.exp_avg_sq.to_value()),
            ("step", Value::U64(self.step)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(OptimizerParam {
            name: wu::get_text(map, "name")?,
            base_content: wu::get_hash(map, "base_content")?,
            exp_avg: TensorRef::from_value(wu::field(map, "exp_avg")?)?,
            exp_avg_sq: TensorRef::from_value(wu::field(map, "exp_avg_sq")?)?,
            step: wu::get_u64(map, "step")?,
        })
    }
}

/// A `weft.optimizer-state.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizerStateManifest {
    pub name: String,
    pub model: BaseRef,
    /// Only `adamw` is implemented.
    pub optimizer: String,
    pub hyper: AdamwHyper,
    /// Sorted by parameter name, unique.
    pub params: Vec<OptimizerParam>,
}

impl OptimizerStateManifest {
    const KEYS: &'static [&'static str] =
        &["hyper", "model", "name", "optimizer", "params", "schema"];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(OPTIMIZER_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("model", self.model.to_value()),
            ("optimizer", Value::Str(self.optimizer.clone())),
            ("hyper", self.hyper.to_value()),
            (
                "params",
                Value::Array(self.params.iter().map(OptimizerParam::to_value).collect()),
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != OPTIMIZER_SCHEMA {
            return Err(Error::malformed(format!(
                "optimizer-state schema is `{schema}`, expected `{OPTIMIZER_SCHEMA}`"
            )));
        }
        let optimizer = wu::get_text(map, "optimizer")?;
        if optimizer != "adamw" {
            return Err(Error::unsupported_representation(format!(
                "optimizer `{optimizer}` is not implemented (only `adamw`)"
            )));
        }
        let params = match wu::field(map, "params")? {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(OptimizerParam::from_value(item)?);
                }
                out
            }
            _ => return Err(Error::malformed("optimizer params must be an array")),
        };
        let manifest = OptimizerStateManifest {
            name: wu::get_text(map, "name")?,
            model: BaseRef::from_value(wu::field(map, "model")?)?,
            optimizer,
            hyper: AdamwHyper::from_value(wu::field(map, "hyper")?)?,
            params,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("optimizer-state name must not be empty"));
        }
        if self.params.is_empty() {
            return Err(Error::malformed(
                "optimizer-state artifact has no parameters",
            ));
        }
        if self.params.len() > MAX_TRAINING_PARAMS {
            return Err(Error::limit_exceeded(format!(
                "optimizer-state declares {} parameters, above the {MAX_TRAINING_PARAMS} bound",
                self.params.len()
            )));
        }
        for pair in self.params.windows(2) {
            if pair[0].name >= pair[1].name {
                return Err(Error::non_canonical(
                    "optimizer-state params must be sorted by name and unique",
                ));
            }
        }
        for param in &self.params {
            if param.step == 0 {
                return Err(Error::malformed(format!(
                    "optimizer-state param `{}` has step 0",
                    param.name
                )));
            }
            if param.exp_avg.dtype != param.exp_avg_sq.dtype
                || param.exp_avg.shape != param.exp_avg_sq.shape
            {
                return Err(Error::malformed(format!(
                    "optimizer-state param `{}` moments disagree on dtype or shape",
                    param.name
                )));
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = wu::encode(&self.to_value())?;
        if bytes.len() as u64 > MAX_TRAINING_MANIFEST_BYTES {
            return Err(Error::limit_exceeded(
                "optimizer-state manifest exceeds its byte bound",
            ));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `H("weft/optimizer-state/v1", WCB(manifest))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(OPTIMIZER_DOMAIN, &self.encode()?))
    }

    /// Total logical bytes of both moment tensors plus the base tensors they
    /// shadow (used by courts for retention accounting).
    pub fn moment_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        for param in &self.params {
            total += param.exp_avg.logical_bytes()? + param.exp_avg_sq.logical_bytes()?;
        }
        Ok(total)
    }
}

/// One parameter's gradient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradientParam {
    pub name: String,
    pub base_content: Hash32,
    pub grad: TensorRef,
}

impl GradientParam {
    const KEYS: &'static [&'static str] = &["base_content", "grad", "name"];

    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("base_content", wu::hash_value(self.base_content)),
            ("grad", self.grad.to_value()),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(GradientParam {
            name: wu::get_text(map, "name")?,
            base_content: wu::get_hash(map, "base_content")?,
            grad: TensorRef::from_value(wu::field(map, "grad")?)?,
        })
    }
}

/// A `weft.gradient.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradientsManifest {
    pub name: String,
    pub model: BaseRef,
    pub step: u64,
    /// `cross_entropy` in the only implementation shipped today.
    pub loss_kind: String,
    /// Loss value as exact f64 bits.
    pub loss_bits: u64,
    pub tokens: u64,
    pub seq_len: u64,
    /// Global gradient norm as exact f64 bits (`0` when not computed).
    pub global_norm_bits: u64,
    /// Sorted by parameter name, unique.
    pub params: Vec<GradientParam>,
}

impl GradientsManifest {
    const KEYS: &'static [&'static str] = &[
        "global_norm_bits",
        "loss_bits",
        "loss_kind",
        "model",
        "name",
        "params",
        "schema",
        "seq_len",
        "step",
        "tokens",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(GRADIENT_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("model", self.model.to_value()),
            ("step", Value::U64(self.step)),
            ("loss_kind", Value::Str(self.loss_kind.clone())),
            ("loss_bits", Value::U64(self.loss_bits)),
            ("tokens", Value::U64(self.tokens)),
            ("seq_len", Value::U64(self.seq_len)),
            ("global_norm_bits", Value::U64(self.global_norm_bits)),
            (
                "params",
                Value::Array(self.params.iter().map(GradientParam::to_value).collect()),
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != GRADIENT_SCHEMA {
            return Err(Error::malformed(format!(
                "gradient schema is `{schema}`, expected `{GRADIENT_SCHEMA}`"
            )));
        }
        let params = match wu::field(map, "params")? {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(GradientParam::from_value(item)?);
                }
                out
            }
            _ => return Err(Error::malformed("gradient params must be an array")),
        };
        let manifest = GradientsManifest {
            name: wu::get_text(map, "name")?,
            model: BaseRef::from_value(wu::field(map, "model")?)?,
            step: wu::get_u64(map, "step")?,
            loss_kind: wu::get_text(map, "loss_kind")?,
            loss_bits: wu::get_u64(map, "loss_bits")?,
            tokens: wu::get_u64(map, "tokens")?,
            seq_len: wu::get_u64(map, "seq_len")?,
            global_norm_bits: wu::get_u64(map, "global_norm_bits")?,
            params,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("gradient name must not be empty"));
        }
        if self.params.is_empty() {
            return Err(Error::malformed("gradient artifact has no parameters"));
        }
        if self.params.len() > MAX_TRAINING_PARAMS {
            return Err(Error::limit_exceeded(format!(
                "gradient declares {} parameters, above the {MAX_TRAINING_PARAMS} bound",
                self.params.len()
            )));
        }
        for pair in self.params.windows(2) {
            if pair[0].name >= pair[1].name {
                return Err(Error::non_canonical(
                    "gradient params must be sorted by name and unique",
                ));
            }
        }
        if !f64::from_bits(self.loss_bits).is_finite() {
            return Err(Error::malformed("gradient loss must be finite"));
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = wu::encode(&self.to_value())?;
        if bytes.len() as u64 > MAX_TRAINING_MANIFEST_BYTES {
            return Err(Error::limit_exceeded(
                "gradient manifest exceeds its byte bound",
            ));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `H("weft/gradient/v1", WCB(manifest))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(GRADIENT_DOMAIN, &self.encode()?))
    }

    pub fn logical_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        for param in &self.params {
            total += param.grad.logical_bytes()?;
        }
        Ok(total)
    }
}

/// A `weft.attention-mask.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionMaskManifest {
    pub name: String,
    /// `causal`, `padding`, `hybrid` or `custom`.
    pub kind: String,
    /// Only `u8` is implemented today.
    pub dtype: String,
    pub shape: Vec<u64>,
    pub seq_len: u64,
    pub pad_positions: u64,
    pub mask: TensorRef,
    /// The exact encoding statement, reproduced verbatim.
    pub interpretation: String,
}

impl AttentionMaskManifest {
    const KEYS: &'static [&'static str] = &[
        "dtype",
        "interpretation",
        "kind",
        "mask",
        "name",
        "pad_positions",
        "schema",
        "seq_len",
        "shape",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(ATTENTION_MASK_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("kind", Value::Str(self.kind.clone())),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("seq_len", Value::U64(self.seq_len)),
            ("pad_positions", Value::U64(self.pad_positions)),
            ("mask", self.mask.to_value()),
            ("interpretation", Value::Str(self.interpretation.clone())),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != ATTENTION_MASK_SCHEMA {
            return Err(Error::malformed(format!(
                "attention-mask schema is `{schema}`, expected `{ATTENTION_MASK_SCHEMA}`"
            )));
        }
        let manifest = AttentionMaskManifest {
            name: wu::get_text(map, "name")?,
            kind: wu::get_text(map, "kind")?,
            dtype: wu::get_text(map, "dtype")?,
            shape: u64_array(map, "shape")?,
            seq_len: wu::get_u64(map, "seq_len")?,
            pad_positions: wu::get_u64(map, "pad_positions")?,
            mask: TensorRef::from_value(wu::field(map, "mask")?)?,
            interpretation: wu::get_text(map, "interpretation")?,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("attention-mask name must not be empty"));
        }
        if self.dtype != "u8" {
            return Err(Error::unsupported_representation(format!(
                "attention-mask dtype `{}` is not implemented (only `u8`)",
                self.dtype
            )));
        }
        if !matches!(
            self.kind.as_str(),
            "causal" | "padding" | "hybrid" | "custom"
        ) {
            return Err(Error::malformed(format!(
                "attention-mask kind `{}` is not one of causal|padding|hybrid|custom",
                self.kind
            )));
        }
        if self.seq_len == 0 {
            return Err(Error::malformed(
                "attention-mask seq_len must be at least 1",
            ));
        }
        if self.shape != self.mask.shape {
            return Err(Error::malformed(
                "attention-mask shape must equal the stored tensor's shape",
            ));
        }
        let elements = self
            .shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .ok_or_else(|| Error::limit_exceeded("attention-mask shape overflows"))?;
        if elements == 0 {
            return Err(Error::malformed("attention-mask has no elements"));
        }
        if self.pad_positions > self.seq_len {
            return Err(Error::malformed(
                "attention-mask pad_positions exceeds seq_len",
            ));
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = wu::encode(&self.to_value())?;
        if bytes.len() as u64 > MAX_TRAINING_MANIFEST_BYTES {
            return Err(Error::limit_exceeded(
                "attention-mask manifest exceeds its byte bound",
            ));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `H("weft/attention-mask/v1", WCB(manifest))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(ATTENTION_MASK_DOMAIN, &self.encode()?))
    }
}

/// Options shared by every training-state write.
#[derive(Clone, Debug, Default)]
pub struct TrainingWriteOptions {
    pub strategy: Option<TensorStrategy>,
    pub record_provenance: bool,
    pub deterministic_seed: Option<[u8; 32]>,
}

/// One optimizer-state entry supplied by the caller.
pub struct OptimizerEntry<'a> {
    pub name: &'a str,
    pub exp_avg: &'a [u8],
    pub exp_avg_sq: &'a [u8],
    pub step: u64,
    /// Dtype of both moments. `None` means "the parameter's own dtype". Real
    /// AdamW state for a bf16 model is normally `f32`, so this is explicit
    /// rather than inferred: the bytes are stored exactly as supplied.
    pub moment_dtype: Option<&'a str>,
}

/// One gradient entry supplied by the caller.
pub struct GradientEntry<'a> {
    pub name: &'a str,
    pub grad: &'a [u8],
    /// Dtype of the gradient. `None` means "the parameter's own dtype".
    pub grad_dtype: Option<&'a str>,
}

/// Result of one training-state write.
#[derive(Clone, Debug)]
pub struct TrainingWriteReport {
    pub name: String,
    pub artifact: Hash32,
    pub manifest_object: Hash32,
    pub manifest_bytes: u64,
    pub params: usize,
    pub logical_bytes: u64,
    pub provenance: Option<Hash32>,
}

/// Restored optimizer state.
#[derive(Clone, Debug)]
pub struct RestoredOptimizer {
    pub artifact: Hash32,
    pub params: Vec<(String, Vec<u8>, Vec<u8>, u64)>,
}

/// Restored gradients.
#[derive(Clone, Debug)]
pub struct RestoredGradients {
    pub artifact: Hash32,
    pub params: Vec<(String, Vec<u8>)>,
}

/// Restored attention mask.
#[derive(Clone, Debug)]
pub struct RestoredMask {
    pub artifact: Hash32,
    pub shape: Vec<u64>,
    pub bytes: Vec<u8>,
}

/// Verification result for any training-state artifact.
#[derive(Clone, Debug)]
pub struct TrainingVerification {
    pub artifact: Hash32,
    pub params: usize,
    pub tensor_objects: usize,
    pub model_manifest_root: Option<Hash32>,
}

fn training_path(kind: &str, name: &str) -> String {
    format!("/{TRAINING_ROOT}/{kind}/{name}/manifest")
}

fn training_dir_name(kind: &str) -> Result<&'static str> {
    match kind {
        "optimizer" => Ok("optimizer"),
        "gradient" => Ok("gradient"),
        "mask" => Ok("mask"),
        other => Err(Error::invalid_argument(format!(
            "training artifact kind must be `optimizer`, `gradient` or `mask` (got `{other}`)"
        ))),
    }
}

impl Store {
    /// Write one AdamW optimizer-state artifact bound to an exact checkpoint.
    pub fn write_optimizer_state(
        &mut self,
        name: &str,
        model_checkpoint: &str,
        hyper: AdamwHyper,
        entries: &[OptimizerEntry<'_>],
        options: &TrainingWriteOptions,
    ) -> Result<TrainingWriteReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        hyper.validate()?;
        let model = self.checkpoint_base_ref(model_checkpoint)?;
        let loaded = self.load_checkpoint_manifest(model_checkpoint)?;
        let by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| (tensor.name.as_str(), tensor))
            .collect();
        let (_root, dir) = self.training_dir("optimizer", name, options)?;
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let mut params = Vec::with_capacity(entries.len());
        let mut logical_bytes = 0u64;
        for entry in entries {
            let base = by_name.get(entry.name).ok_or_else(|| {
                Error::not_found(format!(
                    "optimizer-state parameter `{}` is not a tensor of `{model_checkpoint}`",
                    entry.name
                ))
            })?;
            // `CheckpointTensor::element_bytes` is the whole tensor's element
            // payload, already derived from shape and dtype.
            let moment_dtype = entry.moment_dtype.unwrap_or(base.dtype.as_str());
            let element = crate::representation::TensorElementDtype::parse(moment_dtype)?;
            let elements = base
                .shape
                .iter()
                .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
                .ok_or_else(|| Error::limit_exceeded("optimizer moment shape overflows"))?;
            let expected = elements
                .checked_mul(element.element_bytes())
                .ok_or_else(|| Error::limit_exceeded("optimizer moment bytes overflow"))?;
            for (label, bytes) in [("exp_avg", entry.exp_avg), ("exp_avg_sq", entry.exp_avg_sq)] {
                if bytes.len() as u64 != expected {
                    return Err(Error::malformed(format!(
                        "optimizer-state `{}` {label} is {} bytes, expected {expected} for {:?} x {moment_dtype}",
                        entry.name,
                        bytes.len(),
                        base.shape
                    )));
                }
            }
            let exp_avg = self.write_state_tensor(
                dir,
                name,
                &format!("{}/avg", entry.name),
                moment_dtype,
                &base.shape,
                entry.exp_avg,
                options,
            )?;
            let exp_avg_sq = self.write_state_tensor(
                dir,
                name,
                &format!("{}/avg_sq", entry.name),
                moment_dtype,
                &base.shape,
                entry.exp_avg_sq,
                options,
            )?;
            logical_bytes += expected * 2;
            params.push(OptimizerParam {
                name: entry.name.to_owned(),
                base_content: base.content_root,
                exp_avg,
                exp_avg_sq,
                step: entry.step,
            });
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let manifest = OptimizerStateManifest {
            name: name.to_owned(),
            model,
            optimizer: "adamw".to_owned(),
            hyper,
            params,
        };
        manifest.validate()?;
        let bytes = manifest.encode()?;
        let manifest_object = self.create_file_seeded(
            dir,
            b"manifest",
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("optimizer/{name}/manifest")),
        )?;
        let provenance = if options.record_provenance {
            Some(self.record_training_provenance(
                "optimizer-state-ingest",
                name,
                &manifest.model,
                loaded.manifest_id,
                manifest_object,
            )?)
        } else {
            None
        };
        Ok(TrainingWriteReport {
            name: name.to_owned(),
            artifact: manifest.id()?,
            manifest_object,
            manifest_bytes: bytes.len() as u64,
            params: manifest.params.len(),
            logical_bytes,
            provenance,
        })
    }

    /// Write one gradient artifact bound to an exact checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn write_gradients(
        &mut self,
        name: &str,
        model_checkpoint: &str,
        step: u64,
        loss_kind: &str,
        loss: f64,
        tokens: u64,
        seq_len: u64,
        global_norm: Option<f64>,
        entries: &[GradientEntry<'_>],
        options: &TrainingWriteOptions,
    ) -> Result<TrainingWriteReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        if !loss.is_finite() {
            return Err(Error::invalid_argument("gradient loss must be finite"));
        }
        let model = self.checkpoint_base_ref(model_checkpoint)?;
        let loaded = self.load_checkpoint_manifest(model_checkpoint)?;
        let by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| (tensor.name.as_str(), tensor))
            .collect();
        let (_root, dir) = self.training_dir("gradient", name, options)?;
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let mut params = Vec::with_capacity(entries.len());
        let mut logical_bytes = 0u64;
        for entry in entries {
            let base = by_name.get(entry.name).ok_or_else(|| {
                Error::not_found(format!(
                    "gradient parameter `{}` is not a tensor of `{model_checkpoint}`",
                    entry.name
                ))
            })?;
            // `CheckpointTensor::element_bytes` is the whole tensor's element
            // payload, already derived from shape and dtype.
            let grad_dtype = entry.grad_dtype.unwrap_or(base.dtype.as_str());
            let element = crate::representation::TensorElementDtype::parse(grad_dtype)?;
            let elements = base
                .shape
                .iter()
                .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
                .ok_or_else(|| Error::limit_exceeded("gradient shape overflows"))?;
            let expected = elements
                .checked_mul(element.element_bytes())
                .ok_or_else(|| Error::limit_exceeded("gradient bytes overflow"))?;
            if entry.grad.len() as u64 != expected {
                return Err(Error::malformed(format!(
                    "gradient `{}` is {} bytes, expected {expected} for {:?} x {grad_dtype}",
                    entry.name,
                    entry.grad.len(),
                    base.shape
                )));
            }
            let grad = self.write_state_tensor(
                dir,
                name,
                &format!("{}/grad", entry.name),
                grad_dtype,
                &base.shape,
                entry.grad,
                options,
            )?;
            logical_bytes += expected;
            params.push(GradientParam {
                name: entry.name.to_owned(),
                base_content: base.content_root,
                grad,
            });
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let manifest = GradientsManifest {
            name: name.to_owned(),
            model,
            step,
            loss_kind: loss_kind.to_owned(),
            loss_bits: loss.to_bits(),
            tokens,
            seq_len,
            global_norm_bits: global_norm.unwrap_or(0.0).to_bits(),
            params,
        };
        manifest.validate()?;
        let bytes = manifest.encode()?;
        let manifest_object = self.create_file_seeded(
            dir,
            b"manifest",
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("gradient/{name}/manifest")),
        )?;
        let provenance = if options.record_provenance {
            Some(self.record_training_provenance(
                "gradient-ingest",
                name,
                &manifest.model,
                loaded.manifest_id,
                manifest_object,
            )?)
        } else {
            None
        };
        Ok(TrainingWriteReport {
            name: name.to_owned(),
            artifact: manifest.id()?,
            manifest_object,
            manifest_bytes: bytes.len() as u64,
            params: manifest.params.len(),
            logical_bytes,
            provenance,
        })
    }

    /// Write one attention-mask artifact.
    #[allow(clippy::too_many_arguments)]
    pub fn write_attention_mask(
        &mut self,
        name: &str,
        kind: &str,
        shape: &[u64],
        seq_len: u64,
        pad_positions: u64,
        bytes: &[u8],
        interpretation: &str,
        options: &TrainingWriteOptions,
    ) -> Result<TrainingWriteReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        let expected: u64 = shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .ok_or_else(|| Error::limit_exceeded("attention-mask shape overflows"))?;
        if bytes.len() as u64 != expected {
            return Err(Error::malformed(format!(
                "attention mask is {} bytes but shape {shape:?} requires {expected}",
                bytes.len()
            )));
        }
        let (_root, dir) = self.training_dir("mask", name, options)?;
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let mask = self.write_state_tensor(dir, name, "mask", "u8", shape, bytes, options)?;
        let manifest = AttentionMaskManifest {
            name: name.to_owned(),
            kind: kind.to_owned(),
            dtype: "u8".to_owned(),
            shape: shape.to_vec(),
            seq_len,
            pad_positions,
            mask,
            interpretation: interpretation.to_owned(),
        };
        manifest.validate()?;
        let encoded = manifest.encode()?;
        let manifest_object = self.create_file_seeded(
            dir,
            b"manifest",
            &encoded,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("mask/{name}/manifest")),
        )?;
        Ok(TrainingWriteReport {
            name: name.to_owned(),
            artifact: manifest.id()?,
            manifest_object,
            manifest_bytes: encoded.len() as u64,
            params: 1,
            logical_bytes: bytes.len() as u64,
            provenance: None,
        })
    }

    fn training_dir(
        &mut self,
        kind: &str,
        name: &str,
        options: &TrainingWriteOptions,
    ) -> Result<(Hash32, Hash32)> {
        let kind_dir = training_dir_name(kind)?;
        let root = ensure_dir(self, State::ROOT_ID, TRAINING_ROOT, None)?;
        let kind_root = ensure_dir(
            self,
            root,
            kind_dir,
            Some(&seed_from(&format!("training/{kind_dir}"))),
        )?;
        let seed = options
            .deterministic_seed
            .unwrap_or_else(|| seed_from(&format!("{kind_dir}/{name}")));
        let dir = ensure_dir(self, kind_root, name, Some(&seed))?;
        Ok((root, dir))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_state_tensor(
        &mut self,
        dir: Hash32,
        artifact: &str,
        label: &str,
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        options: &TrainingWriteOptions,
    ) -> Result<TensorRef> {
        let spec = TensorWriteSpec {
            strategy_hint: options.strategy,
            ..TensorWriteSpec::default()
        };
        let slug = label.replace(['.', '/'], "_");
        let name = format!("{slug}.t");
        let object = self.write_tensor_file_with_spec_seeded(
            dir,
            name.as_bytes(),
            dtype,
            shape,
            bytes,
            &TensorLayoutSpec::row_major(),
            &spec,
            &seed_from(&format!("{artifact}/{slug}")),
        )?;
        let meta = self.object_meta(object)?;
        Ok(TensorRef {
            object,
            content: meta.content,
            dtype: dtype.to_owned(),
            shape: shape.to_vec(),
        })
    }

    fn record_training_provenance(
        &mut self,
        operation: &str,
        name: &str,
        model: &BaseRef,
        model_manifest_object: Hash32,
        artifact_manifest_object: Hash32,
    ) -> Result<Hash32> {
        let mut params = BTreeMap::new();
        params.insert("name".to_owned(), Value::Str(name.to_owned()));
        params.insert(
            "model_checkpoint".to_owned(),
            Value::Str(model.checkpoint.clone()),
        );
        let model_meta = self.object_meta(model_manifest_object)?;
        let artifact_meta = self.object_meta(artifact_manifest_object)?;
        let record = ProvenanceRecord::new(
            operation,
            params,
            vec![ProvenanceInput::new(
                "model-checkpoint-manifest",
                model_meta.content,
                Some(model_manifest_object),
            )],
            vec![artifact_meta.content],
            ProvenanceEnvironment::new("weftfs", env!("CARGO_PKG_VERSION")),
            Determinism::Exact,
            ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid),
            self.clock_now_ns(),
        )?;
        self.record_provenance(&[artifact_manifest_object], &record)
    }

    /// Load one optimizer-state manifest.
    pub fn load_optimizer_state(&self, name: &str) -> Result<OptimizerStateManifest> {
        let bytes = self.read_object(self.resolve_path(&training_path("optimizer", name))?)?;
        let manifest = OptimizerStateManifest::decode(&bytes)?;
        if manifest.name != name {
            return Err(Error::corrupt(format!(
                "optimizer-state manifest in `{name}` declares `{}`",
                manifest.name
            )));
        }
        Ok(manifest)
    }

    /// Load one gradient manifest.
    pub fn load_gradients(&self, name: &str) -> Result<GradientsManifest> {
        let bytes = self.read_object(self.resolve_path(&training_path("gradient", name))?)?;
        let manifest = GradientsManifest::decode(&bytes)?;
        if manifest.name != name {
            return Err(Error::corrupt(format!(
                "gradient manifest in `{name}` declares `{}`",
                manifest.name
            )));
        }
        Ok(manifest)
    }

    /// Load one attention-mask manifest.
    pub fn load_attention_mask(&self, name: &str) -> Result<AttentionMaskManifest> {
        let bytes = self.read_object(self.resolve_path(&training_path("mask", name))?)?;
        let manifest = AttentionMaskManifest::decode(&bytes)?;
        if manifest.name != name {
            return Err(Error::corrupt(format!(
                "attention-mask manifest in `{name}` declares `{}`",
                manifest.name
            )));
        }
        Ok(manifest)
    }

    /// Restore every optimizer-state moment and verify its root.
    pub fn restore_optimizer_state(&self, name: &str) -> Result<RestoredOptimizer> {
        let manifest = self.load_optimizer_state(name)?;
        self.verify_model_ref(&manifest.model, name)?;
        let mut params = Vec::with_capacity(manifest.params.len());
        for param in &manifest.params {
            let avg =
                self.read_verified_state_tensor(&param.exp_avg, name, &param.name, "exp_avg")?;
            let avg_sq = self.read_verified_state_tensor(
                &param.exp_avg_sq,
                name,
                &param.name,
                "exp_avg_sq",
            )?;
            params.push((param.name.clone(), avg, avg_sq, param.step));
        }
        Ok(RestoredOptimizer {
            artifact: manifest.id()?,
            params,
        })
    }

    /// Restore every gradient and verify its root.
    pub fn restore_gradients(&self, name: &str) -> Result<RestoredGradients> {
        let manifest = self.load_gradients(name)?;
        self.verify_model_ref(&manifest.model, name)?;
        let mut params = Vec::with_capacity(manifest.params.len());
        for param in &manifest.params {
            let grad = self.read_verified_state_tensor(&param.grad, name, &param.name, "grad")?;
            params.push((param.name.clone(), grad));
        }
        Ok(RestoredGradients {
            artifact: manifest.id()?,
            params,
        })
    }

    /// Read one attention mask and verify its root.
    pub fn read_attention_mask(&self, name: &str) -> Result<RestoredMask> {
        let manifest = self.load_attention_mask(name)?;
        let bytes = self.read_verified_state_tensor(&manifest.mask, name, "mask", "mask")?;
        Ok(RestoredMask {
            artifact: manifest.id()?,
            shape: manifest.shape.clone(),
            bytes,
        })
    }

    /// Verify one optimizer-state artifact.
    pub fn verify_optimizer_state(&self, name: &str) -> Result<TrainingVerification> {
        let manifest = self.load_optimizer_state(name)?;
        self.verify_model_ref(&manifest.model, name)?;
        let mut objects = 0usize;
        for param in &manifest.params {
            self.check_state_tensor(&param.exp_avg, name, &param.name)?;
            self.check_state_tensor(&param.exp_avg_sq, name, &param.name)?;
            objects += 2;
        }
        Ok(TrainingVerification {
            artifact: manifest.id()?,
            params: manifest.params.len(),
            tensor_objects: objects,
            model_manifest_root: Some(manifest.model.manifest_root),
        })
    }

    /// Verify one gradient artifact.
    pub fn verify_gradients(&self, name: &str) -> Result<TrainingVerification> {
        let manifest = self.load_gradients(name)?;
        self.verify_model_ref(&manifest.model, name)?;
        for param in &manifest.params {
            self.check_state_tensor(&param.grad, name, &param.name)?;
        }
        Ok(TrainingVerification {
            artifact: manifest.id()?,
            params: manifest.params.len(),
            tensor_objects: manifest.params.len(),
            model_manifest_root: Some(manifest.model.manifest_root),
        })
    }

    /// Verify one attention-mask artifact.
    pub fn verify_attention_mask(&self, name: &str) -> Result<TrainingVerification> {
        let manifest = self.load_attention_mask(name)?;
        self.check_state_tensor(&manifest.mask, name, "mask")?;
        Ok(TrainingVerification {
            artifact: manifest.id()?,
            params: 1,
            tensor_objects: 1,
            model_manifest_root: None,
        })
    }

    /// Training artifacts of one kind, sorted by name.
    pub fn list_training(&self, kind: &str) -> Result<Vec<String>> {
        let kind_dir = training_dir_name(kind)?;
        let path = format!("/{TRAINING_ROOT}/{kind_dir}");
        let root = match self.resolve_path(&path) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut names = Vec::new();
        for entry in self.list_dir(root)? {
            if self.object_meta(entry.target)?.kind != ObjectKind::Dir {
                continue;
            }
            let name = match String::from_utf8(entry.name.clone()) {
                Ok(name) => name,
                Err(_) => continue,
            };
            // A directory only counts as an artifact when it carries a
            // published manifest.
            if entry_child(self, entry.target, "manifest").is_some() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn verify_model_ref(&self, model: &BaseRef, artifact: &str) -> Result<()> {
        let loaded = self.load_checkpoint_manifest(&model.checkpoint)?;
        if loaded.manifest_root != model.manifest_root {
            return Err(Error::dependency_mismatch(format!(
                "training artifact `{artifact}` names model manifest root {} but `{}` now resolves to {}",
                model.manifest_root, model.checkpoint, loaded.manifest_root
            )));
        }
        let meta = self.object_meta(loaded.manifest_id)?;
        if meta.content != model.manifest_content {
            return Err(Error::dependency_mismatch(format!(
                "training artifact `{artifact}` names model manifest content {} but the live manifest content is {}",
                model.manifest_content, meta.content
            )));
        }
        Ok(())
    }

    fn check_state_tensor(&self, reference: &TensorRef, artifact: &str, param: &str) -> Result<()> {
        let meta = self.object_meta(reference.object)?;
        if meta.content != reference.content {
            return Err(Error::corrupt(format!(
                "training artifact `{artifact}` parameter `{param}` object changed content"
            )));
        }
        if meta.size != reference.logical_bytes()? {
            return Err(Error::corrupt(format!(
                "training artifact `{artifact}` parameter `{param}` is {} bytes, expected {}",
                meta.size,
                reference.logical_bytes()?
            )));
        }
        Ok(())
    }

    fn read_verified_state_tensor(
        &self,
        reference: &TensorRef,
        artifact: &str,
        param: &str,
        label: &str,
    ) -> Result<Vec<u8>> {
        self.check_state_tensor(reference, artifact, param)?;
        let bytes = self.read_tensor(reference.object)?;
        if bytes.len() as u64 != reference.logical_bytes()? {
            return Err(Error::corrupt(format!(
                "training artifact `{artifact}` {label} for `{param}` read {} bytes, expected {}",
                bytes.len(),
                reference.logical_bytes()?
            )));
        }
        Ok(bytes)
    }

    /// Every parameter name common to a checkpoint and an optimizer-state or
    /// gradient artifact (used to prove a partial artifact is intentional).
    pub fn training_coverage(
        &self,
        model_checkpoint: &str,
        artifact_params: &BTreeSet<String>,
    ) -> Result<(usize, usize)> {
        let loaded = self.load_checkpoint_manifest(model_checkpoint)?;
        let total = loaded.manifest.tensors.len();
        let covered = loaded
            .manifest
            .tensors
            .iter()
            .filter(|tensor| artifact_params.contains(&tensor.name))
            .count();
        Ok((covered, total))
    }
}
