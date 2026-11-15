//! T077 — canonical artifact templates for evaluation, RL and synthetic-data
//! generation runs.
//!
//! A template records **what was declared and what was produced**, by content
//! root, in one canonical WCB record:
//!
//! * `weft.eval-run.v1` — dataset root, model root, config root, metric values
//!   (as raw `f64` bits, never decimal), declared outputs;
//! * `weft.synth-data-run.v1` — seed bytes, config root, generator root, output
//!   dataset root, declared outputs.
//!
//! Recording a run also publishes a `weft.provenance.v1` receipt with the same
//! inputs and outputs, so the taint tracer (`spec/WEFT-TAINT.md`) walks through
//! an evaluation or a generation run exactly like any other operation.
//!
//! # What replay establishes, and what it does not
//!
//! `replay` re-reads every declared input **by content root** and checks that
//! the store still holds exactly those bytes, then checks that every recorded
//! output still resolves to a live object whose bytes hash to the recorded
//! root. That is all.
//!
//! Replay does **not** re-run the evaluation, the training step or the
//! generator. It establishes that the declared inputs are intact and the
//! declared outputs are intact and attributable. It does **not** establish
//! numerical determinism, bitwise reproducibility of a GPU kernel, floating
//! point associativity, or that the recorded metrics followed from the recorded
//! inputs. Those are environment properties, and nothing here measures or
//! assumes an environment. A record's `environment` block is a declaration, not
//! a proof.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use crate::store::Store;
use crate::wcb_util as wu;

/// Evaluation-run template schema.
pub const EVAL_RUN_SCHEMA: &str = "weft.eval-run.v1";
/// Synthetic-data-run template schema.
pub const SYNTH_RUN_SCHEMA: &str = "weft.synth-data-run.v1";
/// Identity domain for either template.
pub const TEMPLATE_DOMAIN: &str = "weft/artifact-template/v1";
/// Relation predicate from an output object to its template.
pub const TEMPLATE_PREDICATE: &str = "artifact_template";

/// Bounds.
pub const MAX_TEMPLATE_METRICS: usize = 4_096;
pub const MAX_TEMPLATE_OUTPUTS: usize = 4_096;
pub const MAX_TEMPLATE_TEXT: usize = 1_024;

/// One recorded metric. `value_bits` is the IEEE-754 binary64 bit pattern, so a
/// value is recorded exactly as the producer held it, never as a rounded
/// decimal string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedMetric {
    pub name: String,
    pub value_bits: u64,
    pub kind: String,
}

impl RecordedMetric {
    pub fn new(name: impl Into<String>, value: f64, kind: impl Into<String>) -> Result<Self> {
        let metric = RecordedMetric {
            name: name.into(),
            value_bits: value.to_bits(),
            kind: kind.into(),
        };
        metric.validate()?;
        Ok(metric)
    }

    pub fn value(&self) -> f64 {
        f64::from_bits(self.value_bits)
    }

    fn validate(&self) -> Result<()> {
        if self.name.is_empty() || self.kind.is_empty() {
            return Err(Error::malformed(
                "a recorded metric needs a non-empty name and kind",
            ));
        }
        if self.name.len() > MAX_TEMPLATE_TEXT || self.kind.len() > MAX_TEMPLATE_TEXT {
            return Err(Error::limit_exceeded(
                "a recorded metric name or kind exceeds the text bound",
            ));
        }
        Ok(())
    }

    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("value_bits", Value::U64(self.value_bits)),
            ("kind", Value::Str(self.kind.clone())),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["name", "value_bits", "kind"])?;
        let metric = RecordedMetric {
            name: wu::get_text(map, "name")?,
            value_bits: wu::get_u64(map, "value_bits")?,
            kind: wu::get_text(map, "kind")?,
        };
        metric.validate()?;
        Ok(metric)
    }
}

/// One declared input slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateInput {
    pub role: String,
    pub content: Hash32,
}

impl TemplateInput {
    pub fn new(role: impl Into<String>, content: Hash32) -> Result<Self> {
        let input = TemplateInput {
            role: role.into(),
            content,
        };
        if input.role.is_empty() || input.role.len() > MAX_TEMPLATE_TEXT {
            return Err(Error::malformed(
                "a template input role must be non-empty and within the text bound",
            ));
        }
        Ok(input)
    }
}

/// `weft.eval-run.v1` or `weft.synth-data-run.v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactTemplate {
    pub schema: String,
    /// Caller-declared run name. Not identity.
    pub run: String,
    /// Sorted by `(role, content)`.
    pub inputs: Vec<TemplateInput>,
    /// Sorted, unique content roots of the recorded outputs.
    pub outputs: Vec<Hash32>,
    /// Evaluation metrics; empty for a synthetic-data run.
    pub metrics: Vec<RecordedMetric>,
    /// Seed bytes for a synthetic-data run; empty for an evaluation run.
    pub seed: Vec<u8>,
    /// Declared generator content root (synthetic-data runs only).
    pub generator: Option<Hash32>,
    pub environment: ProvenanceEnvironment,
    pub issued_at_ns: i64,
}

fn environment_value(environment: &ProvenanceEnvironment) -> Value {
    let mut map = environment.fields.clone();
    map.insert("tool".to_owned(), Value::Str(environment.tool.clone()));
    map.insert("version".to_owned(), Value::Str(environment.version.clone()));
    map.insert("env_hash".to_owned(), wu::opt_hash_value(environment.env_hash));
    Value::Map(map)
}

fn environment_from_value(value: &Value) -> Result<ProvenanceEnvironment> {
    let map = wu::as_map(value)?;
    let tool = wu::get_text(map, "tool")?;
    let version = wu::get_text(map, "version")?;
    let env_hash = wu::get_opt_hash(map, "env_hash")?;
    let mut fields = map.clone();
    for key in ProvenanceEnvironment::RESERVED {
        fields.remove(*key);
    }
    let environment = ProvenanceEnvironment {
        tool,
        version,
        env_hash,
        fields,
    };
    Ok(environment)
}

impl ArtifactTemplate {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "run",
        "inputs",
        "outputs",
        "metrics",
        "seed",
        "generator",
        "environment",
        "issued_at",
    ];

    pub fn validate(&self) -> Result<()> {
        if self.schema != EVAL_RUN_SCHEMA && self.schema != SYNTH_RUN_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "artifact template schema `{}` is not {} or {}",
                self.schema, EVAL_RUN_SCHEMA, SYNTH_RUN_SCHEMA
            )));
        }
        if self.run.is_empty() || self.run.len() > MAX_TEMPLATE_TEXT {
            return Err(Error::malformed(
                "a template run name must be non-empty and within the text bound",
            ));
        }
        if self.inputs.is_empty() {
            return Err(Error::malformed("a template declares no inputs"));
        }
        if self.outputs.is_empty() {
            return Err(Error::malformed("a template declares no outputs"));
        }
        if self.metrics.len() > MAX_TEMPLATE_METRICS || self.outputs.len() > MAX_TEMPLATE_OUTPUTS {
            return Err(Error::limit_exceeded(
                "a template exceeds the metric or output bound",
            ));
        }
        for pair in self.inputs.windows(2) {
            let left = (&pair[0].role, pair[0].content);
            let right = (&pair[1].role, pair[1].content);
            if left >= right {
                return Err(Error::non_canonical(
                    "template inputs are not strictly sorted by (role, content)",
                ));
            }
        }
        for pair in self.outputs.windows(2) {
            if pair[0] >= pair[1] {
                return Err(Error::non_canonical(
                    "template outputs are not strictly ascending",
                ));
            }
        }
        match self.schema.as_str() {
            EVAL_RUN_SCHEMA => {
                if !self.seed.is_empty() || self.generator.is_some() {
                    return Err(Error::malformed(
                        "an evaluation template carries no seed and no generator",
                    ));
                }
            }
            SYNTH_RUN_SCHEMA => {
                if self.seed.is_empty() {
                    return Err(Error::malformed(
                        "a synthetic-data template must record its seed",
                    ));
                }
                if self.generator.is_none() {
                    return Err(Error::malformed(
                        "a synthetic-data template must record its generator root",
                    ));
                }
                if !self.metrics.is_empty() {
                    return Err(Error::malformed(
                        "a synthetic-data template carries no metrics",
                    ));
                }
            }
            _ => unreachable!(),
        }
        if self.environment.tool.is_empty() || self.environment.version.is_empty() {
            return Err(Error::malformed(
                "a template environment needs a non-empty tool and version",
            ));
        }
        if self.seed.len() > 4_096 {
            return Err(Error::limit_exceeded("a template seed exceeds 4096 bytes"));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(self.schema.clone())),
            ("run", Value::Str(self.run.clone())),
            (
                "inputs",
                Value::Array(
                    self.inputs
                        .iter()
                        .map(|input| {
                            wu::map([
                                ("role", Value::Str(input.role.clone())),
                                ("content", wu::hash_value(input.content)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "outputs",
                Value::Array(
                    self.outputs
                        .iter()
                        .map(|output| wu::hash_value(*output))
                        .collect(),
                ),
            ),
            (
                "metrics",
                Value::Array(
                    self.metrics
                        .iter()
                        .map(|metric| metric.to_value())
                        .collect(),
                ),
            ),
            ("seed", Value::Bytes(self.seed.clone())),
            ("generator", wu::opt_hash_value(self.generator)),
            ("environment", environment_value(&self.environment)),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let mut inputs = Vec::new();
        for item in wu::get_array(map, "inputs")? {
            let entry = wu::as_map(item)?;
            wu::check_keys(entry, &["role", "content"])?;
            inputs.push(TemplateInput::new(
                wu::get_text(entry, "role")?,
                wu::get_hash(entry, "content")?,
            )?);
        }
        let mut outputs = Vec::new();
        for item in wu::get_array(map, "outputs")? {
            match item {
                Value::Bytes(bytes) => {
                    outputs.push(Hash32::from_slice(bytes).map_err(Error::from)?)
                }
                _ => return Err(Error::malformed("template output must be bytes32")),
            }
        }
        let mut metrics = Vec::new();
        for item in wu::get_array(map, "metrics")? {
            metrics.push(RecordedMetric::from_value(item)?);
        }
        let template = ArtifactTemplate {
            schema: wu::get_text(map, "schema")?,
            run: wu::get_text(map, "run")?,
            inputs,
            outputs,
            metrics,
            seed: wu::get_bytes(map, "seed")?,
            generator: wu::get_opt_hash(map, "generator")?,
            environment: environment_from_value(wu::field(map, "environment")?)?,
            issued_at_ns: wu::get_timestamp(map, "issued_at")?,
        };
        template.validate()?;
        Ok(template)
    }

    /// `TemplateId = H("weft/artifact-template/v1", WCB(record))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(TEMPLATE_DOMAIN, &self.encode()?))
    }
}

/// One replayed input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedInput {
    pub role: String,
    pub content: Hash32,
    /// Live object ids carrying the declared content, ascending.
    pub objects: Vec<Hash32>,
    pub bytes: u64,
    /// True when the bytes hash to the declared content root.
    pub exact: bool,
}

/// One replayed output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedOutput {
    pub content: Hash32,
    pub objects: Vec<Hash32>,
    pub bytes: u64,
    pub exact: bool,
}

/// Result of [`Store::replay_template`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateReplay {
    pub template: Hash32,
    pub schema: String,
    pub inputs: Vec<ReplayedInput>,
    pub outputs: Vec<ReplayedOutput>,
    /// True only when every declared input and output re-verified exactly.
    pub exact: bool,
    /// What this replay does and does not establish, stated on the record.
    pub note: String,
}

/// A request to record an evaluation run.
#[derive(Clone, Debug)]
pub struct EvalRunRequest {
    pub run: String,
    /// `dataset` content root.
    pub dataset: Hash32,
    /// `model` content root.
    pub model: Hash32,
    /// `config` content root.
    pub config: Hash32,
    pub metrics: Vec<RecordedMetric>,
    /// Output object ids. Their content roots become the declared outputs.
    pub outputs: Vec<Hash32>,
    pub environment: ProvenanceEnvironment,
    pub issued_at_ns: Option<i64>,
    pub determinism: Determinism,
}

/// A request to record a synthetic-data generation run.
#[derive(Clone, Debug)]
pub struct SynthRunRequest {
    pub run: String,
    /// `config` content root (generation parameters).
    pub config: Hash32,
    /// `generator` content root (model or program that produced the data).
    pub generator: Hash32,
    pub seed: Vec<u8>,
    /// Output object ids; their content roots form the produced dataset root.
    pub outputs: Vec<Hash32>,
    pub environment: ProvenanceEnvironment,
    pub issued_at_ns: Option<i64>,
    pub determinism: Determinism,
}

impl Store {
    /// Require a declared content root to be present in the store.
    ///
    /// Several live objects may share one content root (a copy is the same
    /// artifact); existence is what matters, not uniqueness. A root the store
    /// cannot produce is refused: a template never records an input that does
    /// not exist.
    fn require_declared_input(&self, role: &str, content: Hash32) -> Result<()> {
        if content == Hash32::ZERO {
            return Err(Error::malformed(format!(
                "declared `{role}` content root is the zero hash"
            )));
        }
        let mut matches: Vec<Hash32> = self
            .current_state()
            .objects
            .values()
            .filter(|record| {
                record.kind == crate::state::ObjectKind::File && record.content == content
            })
            .map(|record| record.id)
            .collect();
        matches.sort_unstable();
        if matches.is_empty() {
            return Err(Error::not_found(format!(
                "declared `{role}` content root {content} does not exist in this store; a \
                 template never records an input the store cannot produce"
            )));
        }
        Ok(())
    }

    fn publish_template(
        &mut self,
        mut template: ArtifactTemplate,
        outputs: &[Hash32],
        determinism: Determinism,
    ) -> Result<Hash32> {
        self.require_writable()?;
        template
            .inputs
            .sort_by(|a, b| (&a.role, a.content).cmp(&(&b.role, b.content)));
        template.outputs.sort_unstable();
        template.outputs.dedup();
        template.validate()?;
        let bytes = template.encode()?;
        let id = template.id()?;
        // The blob is durable before the relation is visible; a crash in
        // between leaves an orphan blob GC collects.
        self.template_blob(&bytes, id)?;
        let record = ProvenanceRecord::new(
            template.schema.clone(),
            {
                let mut params = BTreeMap::new();
                params.insert("template".to_owned(), wu::hash_value(id));
                params.insert("run".to_owned(), Value::Str(template.run.clone()));
                if !template.metrics.is_empty() {
                    params.insert(
                        "metric_count".to_owned(),
                        Value::U64(template.metrics.len() as u64),
                    );
                }
                params
            },
            template
                .inputs
                .iter()
                .map(|input| ProvenanceInput::new(input.role.clone(), input.content, None))
                .collect(),
            template.outputs.clone(),
            template.environment.clone(),
            determinism,
            ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid),
            template.issued_at_ns,
        )?;
        let mut output_objects: Vec<Hash32> = outputs.to_vec();
        output_objects.sort_unstable();
        output_objects.dedup();
        self.record_provenance(&output_objects, &record)?;
        // Attach the template to every output object so `template show` can be
        // reached from an artifact without knowing the run.
        let mut ops = Vec::with_capacity(output_objects.len());
        for object in &output_objects {
            ops.push(crate::state::Op::PutRelation(
                crate::state::RelationRecord::new(
                    *object,
                    TEMPLATE_PREDICATE,
                    crate::state::RelationTarget::Value(Value::Str(id.to_hex())),
                    None,
                    BTreeMap::new(),
                )?,
            ));
        }
        self.commit_ops(ops, false)?;
        Ok(id)
    }

    /// Record an evaluation run.
    pub fn record_eval_run(&mut self, request: EvalRunRequest) -> Result<Hash32> {
        let dataset_object = self.require_declared_input("dataset", request.dataset)?;
        let model_object = self.require_declared_input("model", request.model)?;
        let config_object = self.require_declared_input("config", request.config)?;
        if request.outputs.is_empty() {
            return Err(Error::invalid_argument(
                "an evaluation run declares no outputs",
            ));
        }
        let mut outputs = Vec::new();
        for object in &request.outputs {
            let record = self.current_state().objects.get(object).ok_or_else(|| {
                Error::not_found(format!("output object {object} does not exist"))
            })?;
            outputs.push(record.content);
        }
        let template = ArtifactTemplate {
            schema: EVAL_RUN_SCHEMA.to_owned(),
            run: request.run,
            inputs: vec![
                TemplateInput::new("config", request.config)?,
                TemplateInput::new("dataset", request.dataset)?,
                TemplateInput::new("model", request.model)?,
            ],
            outputs: outputs.clone(),
            metrics: request.metrics,
            seed: Vec::new(),
            generator: None,
            environment: request.environment,
            issued_at_ns: request.issued_at_ns.unwrap_or_else(|| self.now_ns()),
        };
        let _ = (dataset_object, model_object, config_object);
        self.publish_template(template, &request.outputs, request.determinism)
    }

    /// Record a synthetic-data generation run.
    pub fn record_synth_run(&mut self, request: SynthRunRequest) -> Result<Hash32> {
        self.require_declared_input("config", request.config)?;
        self.require_declared_input("generator", request.generator)?;
        if request.outputs.is_empty() {
            return Err(Error::invalid_argument(
                "a synthetic-data run declares no outputs",
            ));
        }
        if request.seed.is_empty() {
            return Err(Error::invalid_argument(
                "a synthetic-data run must declare its seed bytes",
            ));
        }
        let mut outputs = Vec::new();
        for object in &request.outputs {
            let record = self.current_state().objects.get(object).ok_or_else(|| {
                Error::not_found(format!("output object {object} does not exist"))
            })?;
            outputs.push(record.content);
        }
        let template = ArtifactTemplate {
            schema: SYNTH_RUN_SCHEMA.to_owned(),
            run: request.run,
            inputs: vec![
                TemplateInput::new("config", request.config)?,
                TemplateInput::new("generator", request.generator)?,
            ],
            outputs,
            metrics: Vec::new(),
            seed: request.seed,
            generator: Some(request.generator),
            environment: request.environment,
            issued_at_ns: request.issued_at_ns.unwrap_or_else(|| self.now_ns()),
        };
        self.publish_template(template, &request.outputs, request.determinism)
    }

    fn template_blob(&self, bytes: &[u8], id: Hash32) -> Result<()> {
        // Content-addressed under its own domain; stored as a manifest-namespace
        // blob so the store's normal read path verifies it.
        let stored = self.store_blob(crate::recovery::BlobKind::Manifest, TEMPLATE_DOMAIN, bytes)?;
        if stored != id {
            return Err(Error::corrupt(
                "template blob does not hash to its own identity",
            ));
        }
        Ok(())
    }

    /// Read one template by identity.
    pub fn read_template(&self, id: Hash32) -> Result<ArtifactTemplate> {
        let bytes = self.read_blob_verified_keyed(
            crate::recovery::BlobKind::Manifest,
            id,
            TEMPLATE_DOMAIN,
        )?;
        ArtifactTemplate::decode(&bytes)
    }

    /// Templates attached to an output object, ascending by id.
    pub fn templates_of(&self, object: Hash32) -> Result<Vec<(Hash32, ArtifactTemplate)>> {
        let mut out = Vec::new();
        for relation in self.current_state().relations.values() {
            if relation.subject != object || relation.predicate != TEMPLATE_PREDICATE {
                continue;
            }
            let crate::state::RelationTarget::Value(Value::Str(text)) = &relation.target else {
                continue;
            };
            let id = Hash32::from_hex(text)
                .map_err(|_| Error::corrupt("template relation carries a non-hex template id"))?;
            out.push((id, self.read_template(id)?));
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Replay a template: re-verify declared inputs and recorded outputs by
    /// content. Does **not** re-run the computation.
    pub fn replay_template(&self, id: Hash32) -> Result<TemplateReplay> {
        let template = self.read_template(id)?;
        let state = self.current_state();
        let mut exact = true;
        let mut inputs = Vec::new();
        for input in &template.inputs {
            let mut objects: Vec<Hash32> = state
                .objects
                .values()
                .filter(|record| {
                    record.kind == crate::state::ObjectKind::File && record.content == input.content
                })
                .map(|record| record.id)
                .collect();
            objects.sort_unstable();
            if objects.is_empty() {
                exact = false;
                inputs.push(ReplayedInput {
                    role: input.role.clone(),
                    content: input.content,
                    objects,
                    bytes: 0,
                    exact: false,
                });
                continue;
            }
            // Re-read through the full verified path: the store refuses the
            // read if the bytes do not hash to the content its manifest
            // declares, and the filter above already bound that manifest to the
            // recorded content root.
            let bytes = self.read_object(objects[0])?;
            inputs.push(ReplayedInput {
                role: input.role.clone(),
                content: input.content,
                objects,
                bytes: bytes.len() as u64,
                exact: true,
            });
        }
        let mut outputs = Vec::new();
        for content in &template.outputs {
            let mut objects: Vec<Hash32> = state
                .objects
                .values()
                .filter(|record| {
                    record.kind == crate::state::ObjectKind::File && record.content == *content
                })
                .map(|record| record.id)
                .collect();
            objects.sort_unstable();
            if objects.is_empty() {
                exact = false;
                outputs.push(ReplayedOutput {
                    content: *content,
                    objects,
                    bytes: 0,
                    exact: false,
                });
                continue;
            }
            let bytes = self.read_object(objects[0])?;
            outputs.push(ReplayedOutput {
                content: *content,
                objects,
                bytes: bytes.len() as u64,
                exact: true,
            });
        }
        Ok(TemplateReplay {
            template: id,
            schema: template.schema,
            inputs,
            outputs,
            exact,
            note: "replay verifies declared inputs and recorded outputs by content root. It \
                   does not re-run the evaluation, training step or generator and establishes \
                   no numerical or behavioural determinism."
                .to_owned(),
        })
    }

    /// All template ids reachable from the live state, ascending.
    pub fn template_ids(&self) -> Result<Vec<Hash32>> {
        let mut ids: BTreeSet<Hash32> = BTreeSet::new();
        for relation in self.current_state().relations.values() {
            if relation.predicate != TEMPLATE_PREDICATE {
                continue;
            }
            if let crate::state::RelationTarget::Value(Value::Str(text)) = &relation.target {
                if let Ok(id) = Hash32::from_hex(text) {
                    ids.insert(id);
                }
            }
        }
        Ok(ids.into_iter().collect())
    }
}
