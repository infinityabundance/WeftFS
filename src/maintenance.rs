//! T080 — background maintenance with a declared interference budget.
//!
//! The store's `gc` is a whole pass: it plans and sweeps in one call. This
//! module is the operator-facing driver around a *bounded* sweep: it plans
//! once, persists the plan and a cursor, sweeps in byte-bounded batches under a
//! declared rate and duty cycle, and records structured counters after every
//! batch. A restart resumes from the persisted cursor instead of restarting.
//!
//! Safety comes from the store, not from the plan: [`Store::maintenance_reclaim`]
//! re-derives reachability under a fresh GC epoch and refuses to delete a
//! candidate that is reachable *now*. A stale plan can therefore waste work but
//! can never reclaim live data, and a second concurrent pass is refused by the
//! epoch CAS (`Busy`).
//!
//! The budget is a **declaration that is enforced by throttling and reported
//! honestly**, not a scheduler guarantee:
//!
//! * `bytes_per_second` sets a minimum wall time per batch;
//! * `cpu_share_percent` caps the duty cycle: after a batch of `d` elapsed
//!   nanoseconds the driver sleeps until the pass has consumed at most
//!   `cpu_share_percent` of the wall time;
//! * `max_pass_bytes` is a refusal boundary: a single candidate larger than it
//!   cannot be split, so the driver refuses rather than exceed the budget;
//! * the measured rate and duty cycle are recorded in the state, so the
//!   operator sees what actually happened.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::ObjectKind;
use crate::store::{MaintenanceCandidate, MaintenancePlan, Store};
use crate::wcb_util as wu;

/// Durable progress record.
pub const MAINTENANCE_STATE_SCHEMA: &str = "weft.maintenance-state.v1";
/// Persisted plan record.
pub const MAINTENANCE_PLAN_SCHEMA: &str = "weft.maintenance-plan.v1";
/// Namespace directory.
pub const MAINTENANCE_DIR: &str = "maintenance";
/// Plan identity domain.
pub const MAINTENANCE_PLAN_DOMAIN: &str = "weft/maintenance/plan/v1";

/// Declared interference budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceBudget {
    /// Bytes of unreachable blob data to reclaim per second of wall time.
    pub bytes_per_second: u64,
    /// Maximum share of wall time maintenance may consume, in percent.
    pub cpu_share_percent: u8,
    /// Largest single batch, in bytes.
    pub max_batch_bytes: u64,
    /// Largest candidate the driver will ever delete. A larger candidate is a
    /// refusal, not a truncation.
    pub max_pass_bytes: u64,
    /// Batches smaller than this are not attempted (amortise a scan).
    pub min_batch_bytes: u64,
}

impl Default for MaintenanceBudget {
    fn default() -> Self {
        MaintenanceBudget {
            bytes_per_second: 8 * 1024 * 1024,
            cpu_share_percent: 25,
            max_batch_bytes: 64 * 1024 * 1024,
            max_pass_bytes: 1024 * 1024 * 1024,
            min_batch_bytes: 64 * 1024,
        }
    }
}

impl MaintenanceBudget {
    pub fn validate(&self) -> Result<()> {
        if self.bytes_per_second == 0 {
            return Err(Error::invalid_argument(
                "a maintenance budget needs a positive byte rate; a zero rate is not a plan",
            ));
        }
        if self.cpu_share_percent == 0 || self.cpu_share_percent > 100 {
            return Err(Error::invalid_argument("cpu_share_percent must be 1..=100"));
        }
        if self.max_batch_bytes == 0 || self.max_pass_bytes == 0 {
            return Err(Error::invalid_argument(
                "max_batch_bytes and max_pass_bytes must be positive",
            ));
        }
        if self.min_batch_bytes == 0 {
            return Err(Error::invalid_argument("min_batch_bytes must be positive"));
        }
        if self.min_batch_bytes > self.max_batch_bytes {
            return Err(Error::invalid_argument(
                "min_batch_bytes must not exceed max_batch_bytes",
            ));
        }
        if self.max_batch_bytes > self.max_pass_bytes {
            return Err(Error::invalid_argument(
                "max_batch_bytes must not exceed max_pass_bytes",
            ));
        }
        Ok(())
    }

    /// Bytes allowed in one batch: one tick of the rate, clamped.
    pub fn batch_bytes(&self) -> u64 {
        // One tick is 100 ms of the declared rate.
        let tick = self.bytes_per_second.saturating_mul(100) / 1_000;
        tick.clamp(self.min_batch_bytes, self.max_batch_bytes)
    }

    /// The refusal policy: candidates that cannot be processed within the
    /// declared bounds stop the driver instead of being skipped silently.
    pub fn check_candidate(&self, candidate: &MaintenanceCandidate) -> Result<()> {
        if candidate.bytes > self.max_pass_bytes {
            return Err(Error::budget_exceeded(format!(
                "candidate {} ({} bytes) exceeds max_pass_bytes {}; a blob cannot be split, \
                 so the budget as declared cannot be met",
                candidate.hash, candidate.bytes, self.max_pass_bytes
            )));
        }
        Ok(())
    }

    /// Wall-time target for a batch, and the duty-cycle sleep after it.
    pub fn io_target(&self, bytes: u64) -> Duration {
        let ns = (bytes as u128)
            .saturating_mul(1_000_000_000)
            .checked_div(self.bytes_per_second as u128)
            .unwrap_or(0);
        Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    pub fn duty_sleep(&self, elapsed: Duration) -> Duration {
        if self.cpu_share_percent >= 100 {
            return Duration::ZERO;
        }
        let target = elapsed
            .as_nanos()
            .saturating_mul(100)
            .saturating_div(u128::from(self.cpu_share_percent));
        Duration::from_nanos(u64::try_from(target).unwrap_or(u64::MAX)).saturating_sub(elapsed)
    }
}

/// Persistent driver state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceState {
    pub plan_root: Hash32,
    pub generation: u64,
    pub head: Hash32,
    pub candidates: u64,
    pub cursor: u64,
    pub reclaimed_blobs: u64,
    pub reclaimed_bytes: u64,
    /// Work deferred: candidates skipped by a sweep (became reachable, or
    /// already gone) plus candidates left by a pass cap.
    pub deferred: u64,
    pub passes: u64,
    /// `planned` | `running` | `complete`
    pub status: String,
    pub updated_at_ns: i64,
    /// Measured, not declared: the last batch's achieved byte rate.
    pub last_rate_bytes_per_second: u64,
    /// Measured, not declared: the last batch's duty cycle in percent.
    pub last_duty_percent: u64,
    pub budget: BTreeMap<String, u64>,
}

impl MaintenanceState {
    fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(MAINTENANCE_STATE_SCHEMA.to_owned())),
            ("plan_root", wu::hash_value(self.plan_root)),
            ("generation", Value::U64(self.generation)),
            ("head", wu::hash_value(self.head)),
            ("candidates", Value::U64(self.candidates)),
            ("cursor", Value::U64(self.cursor)),
            ("reclaimed_blobs", Value::U64(self.reclaimed_blobs)),
            ("reclaimed_bytes", Value::U64(self.reclaimed_bytes)),
            ("deferred", Value::U64(self.deferred)),
            ("passes", Value::U64(self.passes)),
            ("status", Value::Str(self.status.clone())),
            ("updated_at", Value::Timestamp(self.updated_at_ns)),
            (
                "last_rate_bytes_per_second",
                Value::U64(self.last_rate_bytes_per_second),
            ),
            ("last_duty_percent", Value::U64(self.last_duty_percent)),
            (
                "budget",
                Value::Map(
                    self.budget
                        .iter()
                        .map(|(key, value)| (key.clone(), Value::U64(*value)))
                        .collect(),
                ),
            ),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "plan_root",
                "generation",
                "head",
                "candidates",
                "cursor",
                "reclaimed_blobs",
                "reclaimed_bytes",
                "deferred",
                "passes",
                "status",
                "updated_at",
                "last_rate_bytes_per_second",
                "last_duty_percent",
                "budget",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != MAINTENANCE_STATE_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "maintenance state schema `{schema}` is not `{MAINTENANCE_STATE_SCHEMA}`"
            )));
        }
        let status = wu::get_text(map, "status")?;
        if status != "planned" && status != "running" && status != "complete" {
            return Err(Error::malformed(format!(
                "maintenance status `{status}` is not planned|running|complete"
            )));
        }
        let budget_map = wu::get_map(map, "budget")?;
        let mut budget = BTreeMap::new();
        for (key, value) in budget_map {
            match value {
                Value::U64(value) => {
                    budget.insert(key, value);
                }
                _ => return Err(Error::malformed("maintenance budget values must be u64")),
            }
        }
        Ok(MaintenanceState {
            plan_root: wu::get_hash(map, "plan_root")?,
            generation: wu::get_u64(map, "generation")?,
            head: wu::get_hash(map, "head")?,
            candidates: wu::get_u64(map, "candidates")?,
            cursor: wu::get_u64(map, "cursor")?,
            reclaimed_blobs: wu::get_u64(map, "reclaimed_blobs")?,
            reclaimed_bytes: wu::get_u64(map, "reclaimed_bytes")?,
            deferred: wu::get_u64(map, "deferred")?,
            passes: wu::get_u64(map, "passes")?,
            status,
            updated_at_ns: wu::get_timestamp(map, "updated_at")?,
            last_rate_bytes_per_second: wu::get_u64(map, "last_rate_bytes_per_second")?,
            last_duty_percent: wu::get_u64(map, "last_duty_percent")?,
            budget,
        })
    }
}

/// The persisted plan record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenancePlanRecord {
    pub candidates: Vec<MaintenanceCandidate>,
    pub reachable_manifests: u64,
    pub reachable_chunks: u64,
    pub orphan_records: u64,
    pub issued_at_ns: i64,
}

impl MaintenancePlanRecord {
    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(MAINTENANCE_PLAN_SCHEMA.to_owned())),
            (
                "candidates",
                Value::Array(
                    self.candidates
                        .iter()
                        .map(|candidate| {
                            wu::map([
                                ("kind", Value::Str(candidate.kind.clone())),
                                ("hash", wu::hash_value(candidate.hash)),
                                ("bytes", Value::U64(candidate.bytes)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("reachable_manifests", Value::U64(self.reachable_manifests)),
            ("reachable_chunks", Value::U64(self.reachable_chunks)),
            ("orphan_records", Value::U64(self.orphan_records)),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "candidates",
                "reachable_manifests",
                "reachable_chunks",
                "orphan_records",
                "issued_at",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != MAINTENANCE_PLAN_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "maintenance plan schema `{schema}` is not `{MAINTENANCE_PLAN_SCHEMA}`"
            )));
        }
        let mut candidates = Vec::new();
        for item in wu::get_array(map, "candidates")? {
            let entry = wu::as_map(item)?;
            wu::check_keys(entry, &["kind", "hash", "bytes"])?;
            let kind = wu::get_text(entry, "kind")?;
            if kind != "object" && kind != "manifest" {
                return Err(Error::malformed(format!(
                    "maintenance candidate kind `{kind}` is not object|manifest"
                )));
            }
            candidates.push(MaintenanceCandidate {
                kind,
                hash: wu::get_hash(entry, "hash")?,
                bytes: wu::get_u64(entry, "bytes")?,
            });
        }
        Ok(MaintenancePlanRecord {
            candidates,
            reachable_manifests: wu::get_u64(map, "reachable_manifests")?,
            reachable_chunks: wu::get_u64(map, "reachable_chunks")?,
            orphan_records: wu::get_u64(map, "orphan_records")?,
            issued_at_ns: wu::get_timestamp(map, "issued_at")?,
        })
    }

    /// The **plan key**: `H("weft/maintenance/plan/v1", WCB({schema, candidates}))`.
    ///
    /// Only the candidate set is hashed, so the key is stable across runs whose
    /// reachable counts or timestamps changed. That is what makes a persisted
    /// cursor resumable: a re-plan over the same garbage yields the same key.
    pub fn root(&self) -> Result<Hash32> {
        let key = wu::map([
            ("schema", Value::Str(MAINTENANCE_PLAN_SCHEMA.to_owned())),
            (
                "candidates",
                Value::Array(
                    self.candidates
                        .iter()
                        .map(|candidate| {
                            wu::map([
                                ("kind", Value::Str(candidate.kind.clone())),
                                ("hash", wu::hash_value(candidate.hash)),
                                ("bytes", Value::U64(candidate.bytes)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        Ok(identity::h(MAINTENANCE_PLAN_DOMAIN, &wu::encode(&key)?))
    }
}

/// Outcome of one driver invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceRun {
    pub passes: u64,
    pub batches: u64,
    pub reclaimed_blobs: u64,
    pub reclaimed_bytes: u64,
    pub deferred: u64,
    pub status: String,
    pub plan_root: Hash32,
    pub candidates: u64,
    pub cursor: u64,
    pub wall_ms: u64,
    pub measured_bytes_per_second: u64,
    pub measured_duty_percent: u64,
    pub resumed: bool,
    pub problems: Vec<String>,
}

impl Default for MaintenanceRun {
    fn default() -> Self {
        MaintenanceRun {
            passes: 0,
            batches: 0,
            reclaimed_blobs: 0,
            reclaimed_bytes: 0,
            deferred: 0,
            status: String::new(),
            plan_root: Hash32::ZERO,
            candidates: 0,
            cursor: 0,
            wall_ms: 0,
            measured_bytes_per_second: 0,
            measured_duty_percent: 0,
            resumed: false,
            problems: Vec::new(),
        }
    }
}


/// What the read-only ops surface reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpsReport {
    pub state: Option<MaintenanceState>,
    pub plan: Option<MaintenancePlan>,
    pub quarantine_entries: u64,
    pub generation: u64,
    pub head: Hash32,
    pub index_bytes: u64,
}

impl Default for OpsReport {
    fn default() -> Self {
        OpsReport {
            state: None,
            plan: None,
            quarantine_entries: 0,
            generation: 0,
            head: Hash32::ZERO,
            index_bytes: 0,
        }
    }
}


impl Store {
    fn maintenance_dir(&mut self, create: bool) -> Result<Option<Hash32>> {
        let root = self.resolve_path("/")?;
        match self.lookup(root, MAINTENANCE_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => {
                if !create {
                    return Ok(None);
                }
                let uid = self.options().owner_uid;
                let gid = self.options().owner_gid;
                Ok(Some(self.mkdir(
                    root,
                    MAINTENANCE_DIR.as_bytes(),
                    0o755,
                    uid,
                    gid,
                )?))
            }
            Err(err) => Err(err),
        }
    }

    fn maintenance_dir_read(&self) -> Result<Option<Hash32>> {
        let root = self.resolve_path("/")?;
        match self.lookup(root, MAINTENANCE_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The persisted driver state, if any.
    pub fn maintenance_state(&self) -> Result<Option<MaintenanceState>> {
        let Some(dir) = self.maintenance_dir_read()? else {
            return Ok(None);
        };
        let id = match self.lookup(dir, b"state") {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object_exempt(id)?;
        Ok(Some(MaintenanceState::decode(&bytes)?))
    }

    /// The persisted plan, if any.
    pub fn maintenance_plan_record(&self) -> Result<Option<MaintenancePlanRecord>> {
        let Some(dir) = self.maintenance_dir_read()? else {
            return Ok(None);
        };
        let id = match self.lookup(dir, b"plan") {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object_exempt(id)?;
        Ok(Some(MaintenancePlanRecord::decode(&bytes)?))
    }

    fn write_maintenance_file(&mut self, dir: Hash32, name: &str, bytes: &[u8]) -> Result<()> {
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        match self.lookup(dir, name.as_bytes()) {
            Ok(object) => {
                // A range write does not shrink a file: writing a shorter
                // record would leave the previous record's tail in place and
                // produce a record that does not decode. Truncate to the new
                // length so the file is exactly the canonical record.
                self.write_file(object, 0, bytes)?;
                let current = self.object_meta(object)?.size;
                if current != bytes.len() as u64 {
                    self.truncate(object, bytes.len() as u64)?;
                }
                Ok(())
            }
            Err(err) if err.class() == Class::NotFound => {
                self.create_file(
                    dir,
                    name.as_bytes(),
                    bytes,
                    0o644,
                    uid,
                    gid,
                    BTreeMap::new(),
                )?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn persist_maintenance(
        &mut self,
        plan: &MaintenancePlanRecord,
        state: &MaintenanceState,
        write_plan: bool,
    ) -> Result<()> {
        let dir = self
            .maintenance_dir(true)?
            .ok_or_else(|| Error::corrupt("maintenance directory was not created"))?;
        // The plan is written once per plan (its bytes are large); the state is
        // written after every batch. Rewriting an unchanged plan would publish
        // a new manifest version for no reason and add self-churn to the next
        // pass's candidate set.
        if write_plan {
            let plan_bytes = plan.encode()?;
            self.write_maintenance_file(dir, "plan", &plan_bytes)?;
        }
        let state_bytes = state.encode()?;
        self.write_maintenance_file(dir, "state", &state_bytes)?;
        Ok(())
    }

    /// Read-only ops report: counters, reachability plan, quarantine count.
    pub fn ops_report(&self) -> Result<OpsReport> {
        let state = self.maintenance_state()?;
        let plan = self.maintenance_plan()?;
        Ok(OpsReport {
            state,
            quarantine_entries: self.quarantine_entries()?.len() as u64,
            generation: self.generation(),
            head: self.head(),
            index_bytes: self.index_bytes_total().unwrap_or(0),
            plan: Some(plan),
        })
    }
}

/// Run bounded maintenance passes.
///
/// `max_passes` bounds the number of batches in this invocation, so a caller
/// can run one batch and exit. `apply = false` plans and reports without
/// deleting anything.
pub fn maintenance_run(
    store: &mut Store,
    budget: &MaintenanceBudget,
    max_passes: u64,
    apply: bool,
) -> Result<MaintenanceRun> {
    budget.validate()?;
    let started = Instant::now();
    let existing = store.maintenance_state()?;
    // Reuse the persisted plan while it is still in progress. A stale plan is
    // safe (every candidate is re-verified before deletion) and reusing it is
    // what makes a restart resume instead of restarting. A new plan is made
    // only when the previous one completed or none exists, so garbage created
    // during a run is picked up by the next run rather than reshaping the
    // cursor mid-flight.
    let resumable = match (&existing, store.maintenance_plan_record()?) {
        (Some(state), Some(plan)) if state.status != "complete" && state.cursor < state.candidates => {
            Some(plan)
        }
        _ => None,
    };
    let plan_now = store.maintenance_plan()?;
    let (record, plan_changed) = match resumable {
        Some(plan) => (plan, false),
        None => (
            MaintenancePlanRecord {
                candidates: plan_now.candidates.clone(),
                reachable_manifests: plan_now.reachable_manifests,
                reachable_chunks: plan_now.reachable_chunks,
                orphan_records: plan_now.orphan_records,
                issued_at_ns: store.now_ns(),
            },
            true,
        ),
    };
    let plan_root = record.root()?;
    // Resume when the persisted plan is exactly this plan and its cursor has
    // not reached the end. Anything else is a new plan; cumulative counters are
    // kept, the cursor is not.
    let mut resumed = false;
    let mut state = match existing {
        Some(state)
            if state.plan_root == plan_root
                && state.cursor < state.candidates
                && state.status != "complete" =>
        {
            resumed = true;
            state
        }
        Some(previous) => MaintenanceState {
            plan_root,
            generation: store.generation(),
            head: store.head(),
            candidates: record.candidates.len() as u64,
            cursor: 0,
            reclaimed_blobs: previous.reclaimed_blobs,
            reclaimed_bytes: previous.reclaimed_bytes,
            deferred: previous.deferred,
            passes: previous.passes,
            status: "planned".to_owned(),
            updated_at_ns: store.now_ns(),
            last_rate_bytes_per_second: previous.last_rate_bytes_per_second,
            last_duty_percent: previous.last_duty_percent,
            budget: BTreeMap::new(),
        },
        None => MaintenanceState {
            plan_root,
            generation: store.generation(),
            head: store.head(),
            candidates: record.candidates.len() as u64,
            cursor: 0,
            reclaimed_blobs: 0,
            reclaimed_bytes: 0,
            deferred: 0,
            passes: 0,
            status: "planned".to_owned(),
            updated_at_ns: store.now_ns(),
            last_rate_bytes_per_second: 0,
            last_duty_percent: 0,
            budget: BTreeMap::new(),
        },
    };
    state.budget = BTreeMap::from([
        ("bytes_per_second".to_owned(), budget.bytes_per_second),
        (
            "cpu_share_percent".to_owned(),
            u64::from(budget.cpu_share_percent),
        ),
        ("max_batch_bytes".to_owned(), budget.max_batch_bytes),
        ("max_pass_bytes".to_owned(), budget.max_pass_bytes),
    ]);
    // Refusal policy: a candidate that cannot fit the declared bound stops the
    // driver before anything is written.
    for candidate in &record.candidates {
        budget.check_candidate(candidate)?;
    }

    let mut run = MaintenanceRun {
        plan_root,
        candidates: record.candidates.len() as u64,
        cursor: state.cursor,
        resumed,
        ..MaintenanceRun::default()
    };
    if !apply {
        run.status = state.status.clone();
        run.deferred = state.deferred;
        run.reclaimed_blobs = state.reclaimed_blobs;
        run.reclaimed_bytes = state.reclaimed_bytes;
        run.wall_ms = started.elapsed().as_millis() as u64;
        return Ok(run);
    }
    if state.cursor >= state.candidates {
        state.status = "complete".to_owned();
        state.updated_at_ns = store.now_ns();
        store.persist_maintenance(&record, &state, plan_changed)?;
        run.status = state.status.clone();
        run.wall_ms = started.elapsed().as_millis() as u64;
        return Ok(run);
    }
    let batch_limit = budget.batch_bytes();
    let mut passes = 0u64;
    while passes < max_passes && state.cursor < state.candidates {
        // Build one batch under the byte cap.
        let mut batch: Vec<MaintenanceCandidate> = Vec::new();
        let mut batch_bytes = 0u64;
        while state.cursor < state.candidates {
            let candidate = record.candidates[state.cursor as usize].clone();
            if !batch.is_empty() && batch_bytes + candidate.bytes > batch_limit {
                break;
            }
            batch_bytes = batch_bytes.saturating_add(candidate.bytes);
            batch.push(candidate);
            state.cursor += 1;
            if batch_bytes >= batch_limit {
                break;
            }
        }
        let batch_started = Instant::now();
        let sweep = store.maintenance_reclaim(&batch)?;
        let elapsed = batch_started.elapsed();
        passes += 1;
        run.batches += 1;
        run.reclaimed_blobs += sweep.deleted;
        run.reclaimed_bytes += sweep.deleted_bytes;
        state.reclaimed_blobs += sweep.deleted;
        state.reclaimed_bytes += sweep.deleted_bytes;
        state.deferred = state.deferred.saturating_add(sweep.skipped.len() as u64);
        run.deferred = state.deferred;
        run.problems.extend(sweep.problems.clone());
        state.passes += 1;
        state.status = if state.cursor >= state.candidates {
            "complete".to_owned()
        } else {
            "running".to_owned()
        };
        state.updated_at_ns = store.now_ns();
        state.last_rate_bytes_per_second = if elapsed.as_nanos() == 0 {
            0
        } else {
            u64::try_from(
                (sweep.deleted_bytes as u128)
                    .saturating_mul(1_000_000_000)
                    .checked_div(elapsed.as_nanos())
                    .unwrap_or(0),
            )
            .unwrap_or(u64::MAX)
        };
        // Interference budget: wait out the I/O target and the duty cycle.
        let io_left = budget
            .io_target(sweep.deleted_bytes.max(1))
            .saturating_sub(elapsed);
        let duty_left = budget.duty_sleep(elapsed);
        let sleep = io_left.max(duty_left);
        let duty_window = elapsed + sleep;
        state.last_duty_percent = if duty_window.as_nanos() == 0 {
            0
        } else {
            u64::try_from(
                elapsed
                    .as_nanos()
                    .saturating_mul(100)
                    .checked_div(duty_window.as_nanos())
                    .unwrap_or(0),
            )
            .unwrap_or(100)
        };
        store.persist_maintenance(&record, &state, plan_changed && passes == 1)?;
        run.cursor = state.cursor;
        if !sleep.is_zero() {
            std::thread::sleep(sleep);
        }
    }
    let wall = started.elapsed();
    run.status = state.status.clone();
    run.passes = state.passes;
    run.cursor = state.cursor;
    run.wall_ms = wall.as_millis() as u64;
    run.measured_bytes_per_second = if wall.as_nanos() == 0 {
        0
    } else {
        u64::try_from(
            (run.reclaimed_bytes as u128)
                .saturating_mul(1_000_000_000)
                .checked_div(wall.as_nanos())
                .unwrap_or(0),
        )
        .unwrap_or(u64::MAX)
    };
    Ok(run)
}

/// The reachable object records a maintenance pass must never touch, for a
/// court that asserts no live artifact was lost.
pub fn reachable_objects(store: &Store) -> Result<Vec<Hash32>> {
    let mut out: Vec<Hash32> = store
        .current_state()
        .objects
        .values()
        .filter(|record| record.kind == ObjectKind::File)
        .map(|record| record.id)
        .collect();
    out.sort_unstable();
    Ok(out)
}
