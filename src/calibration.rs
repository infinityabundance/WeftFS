//! Host-local decode-cost calibration (R12).
//!
//! The planner's `measured` table in [`crate::cost_model::MEASURED`] is a
//! constant baked into the crate. It was measured in the WeftFS bench court on
//! one specific image, kernel, CPU, and allocator; a different host can shift
//! it by tens of percent. This module lets an operator measure the same
//! quantities on the current host and hand the result to the planner with
//! `--calibration PATH`.
//!
//! A calibration document is strict. The loader rejects a wrong schema,
//! unknown or missing keys, a missing or duplicated class, out-of-range rows,
//! non-monotonic rows, a bad digest, and any file larger than
//! [`MAX_CALIBRATION_BYTES`]. There is no partial parse and no silent fallback
//! to the baked table: a planner given `--calibration` either uses every row
//! from that file or fails before planning.
//!
//! The measurement itself is the code that used to live in
//! `examples/calibrate_decode.rs`; the example is now a thin wrapper over
//! [`measure_decode_costs_scratch`], so the CLI verb and the example cannot
//! drift. Measurements are of the shipped `Store::read_tensor` path (manifest
//! read, frame/chunk verification, full logical decode) on synthetic
//! weight-like BF16 data at two sizes, with the affine fit
//!
//! ```text
//! estimated_decode_ns = fixed_ns + decoded_bytes * ns_per_mib / 1_048_576
//! ```
//!
//! JSON is not the store's canonical WCB, but it is written through
//! `serde_json`'s ordered map (BTreeMap), so the compact serialisation is
//! deterministic. The recorded digest is
//! `H("weft/decode-calibration/v1", compact_json_without_digest)`; the loader
//! recomputes it from the re-serialised document, so whitespace in the file is
//! irrelevant but any semantic change is detected.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Map, Value as Json};
use weftcodec::SemanticPolicy;

use crate::cost_model::{self, Coefficients, CostClass, CostModelKind, CostTable};
use crate::error::{Error, Result};
use crate::identity::{h, Hash32};
use crate::store::{Options, Store};
use crate::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

/// Calibration document schema.
pub const CALIBRATION_SCHEMA: &str = "weft.calibration.v1";

/// Domain separation for the calibration digest.
pub const CALIBRATION_DOMAIN: &str = "weft/decode-calibration/v1";

/// Hard size bound for a calibration document. The real document is a few
/// kilobytes; anything larger is not a calibration and is refused unread.
pub const MAX_CALIBRATION_BYTES: u64 = 1 << 20;

/// A measured row needs at least this many timed repetitions.
pub const MIN_SAMPLES: u64 = 5;

/// Per-call fixed cost ceiling (60 s). A row above this is a typo or a unit
/// error, not a measurement.
pub const MAX_FIXED_NS: u64 = 60_000_000_000;

/// Per-MiB cost ceiling (100 s/MiB). Same rationale as [`MAX_FIXED_NS`].
pub const MAX_NS_PER_MIB: u64 = 100_000_000_000;

/// Advisory store directory for installed calibrations. Like `indexes/`, it is
/// not part of the frozen on-disk format and is ignored by replay, verify, and
/// GC.
pub const CALIBRATION_DIR: &str = "calibrations";

const FIT_TEXT: &str = "fixed_ns + ns_per_mib * decoded_bytes / 1048576; fit from the small and \
large medians; delta rows subtract the measured base read";

const MIB: u64 = 1_048_576;

/// How a row was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOrigin {
    /// Timed on this host by [`measure_decode_costs`].
    Measured,
    /// Reuses another measured row because the store never produces the
    /// representation (SPARSE_DELTA reuses WEIGHT_DELTA).
    Reused,
    /// The historical declared table; used for classes the store never writes.
    Declared,
}

impl RowOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            RowOrigin::Measured => "measured",
            RowOrigin::Reused => "reused",
            RowOrigin::Declared => "declared",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "measured" => RowOrigin::Measured,
            "reused" => RowOrigin::Reused,
            "declared" => RowOrigin::Declared,
            _ => return None,
        })
    }
}

/// One calibration row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationRow {
    pub class: CostClass,
    pub fixed_ns: u64,
    pub ns_per_mib: u64,
    /// Number of timed samples behind the row; zero for reused/declared rows.
    pub samples: u64,
    pub origin: RowOrigin,
}

impl CalibrationRow {
    pub fn coefficients(&self) -> Coefficients {
        Coefficients {
            fixed_ns: self.fixed_ns,
            ns_per_mib: self.ns_per_mib,
        }
    }
}

/// Host and environment identity captured with a calibration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostIdentity {
    pub hostname: String,
    pub kernel: String,
    pub arch: String,
    pub cpus: u64,
    pub image: String,
    pub image_id: String,
    pub memory_limit: String,
    pub pids_limit: String,
    pub weftfs_revision: String,
    pub weftcodec_revision: String,
}

impl HostIdentity {
    /// Best-effort identity of the current process environment.
    ///
    /// The `WEFT_COURT_*` and `WEFT_REPO_*` variables are set by
    /// `scripts/docker-court.sh`; outside a court they are `unknown`. Nothing
    /// here is trusted for pricing; the fields exist for the receipt.
    pub fn current() -> Self {
        let read = |path: &str| {
            fs::read_to_string(path)
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());
        HostIdentity {
            hostname: read("/proc/sys/kernel/hostname").unwrap_or_else(|| env("HOSTNAME")),
            kernel: match std::env::var("WEFT_COURT_KERNEL") {
                Ok(value) => value,
                Err(_) => {
                    read("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".to_owned())
                }
            },
            arch: std::env::consts::ARCH.to_owned(),
            cpus: std::thread::available_parallelism()
                .map(|value| value.get() as u64)
                .unwrap_or(0),
            image: env("WEFT_COURT_IMAGE"),
            image_id: env("WEFT_COURT_IMAGE_ID"),
            memory_limit: env("WEFT_COURT_MEMORY"),
            pids_limit: env("WEFT_COURT_PIDS"),
            weftfs_revision: env("WEFT_REPO_WEFTFS"),
            weftcodec_revision: env("WEFT_REPO_WEFTCODEC"),
        }
    }

    fn to_json(&self) -> Json {
        json!({
            "hostname": self.hostname,
            "kernel": self.kernel,
            "arch": self.arch,
            "cpus": self.cpus,
            "image": self.image,
            "image_id": self.image_id,
            "memory_limit": self.memory_limit,
            "pids_limit": self.pids_limit,
            "weftfs_revision": self.weftfs_revision,
            "weftcodec_revision": self.weftcodec_revision,
        })
    }

    fn from_json(value: &Json) -> Result<Self> {
        let map = exact_object(
            value,
            "host",
            &[
                "hostname",
                "kernel",
                "arch",
                "cpus",
                "image",
                "image_id",
                "memory_limit",
                "pids_limit",
                "weftfs_revision",
                "weftcodec_revision",
            ],
        )?;
        Ok(HostIdentity {
            hostname: string_field(map, "hostname", "host")?,
            kernel: string_field(map, "kernel", "host")?,
            arch: string_field(map, "arch", "host")?,
            cpus: u64_field(map, "cpus", "host")?,
            image: string_field(map, "image", "host")?,
            image_id: string_field(map, "image_id", "host")?,
            memory_limit: string_field(map, "memory_limit", "host")?,
            pids_limit: string_field(map, "pids_limit", "host")?,
            weftfs_revision: string_field(map, "weftfs_revision", "host")?,
            weftcodec_revision: string_field(map, "weftcodec_revision", "host")?,
        })
    }
}

/// Measurement method recorded with a calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationMethod {
    pub reps: u64,
    pub warmup_reads: u64,
    pub small_bytes: u64,
    pub large_bytes: u64,
}

impl CalibrationMethod {
    fn to_json(&self) -> Json {
        json!({
            "reps": self.reps,
            "warmup_reads": self.warmup_reads,
            "small_bytes": self.small_bytes,
            "large_bytes": self.large_bytes,
            "fit": FIT_TEXT,
        })
    }

    fn from_json(value: &Json) -> Result<Self> {
        let map = exact_object(
            value,
            "method",
            &["reps", "warmup_reads", "small_bytes", "large_bytes", "fit"],
        )?;
        if string_field(map, "fit", "method")? != FIT_TEXT {
            return Err(invalid("method.fit is not the documented affine fit"));
        }
        let method = CalibrationMethod {
            reps: u64_field(map, "reps", "method")?,
            warmup_reads: u64_field(map, "warmup_reads", "method")?,
            small_bytes: u64_field(map, "small_bytes", "method")?,
            large_bytes: u64_field(map, "large_bytes", "method")?,
        };
        method.validate()?;
        Ok(method)
    }

    fn validate(&self) -> Result<()> {
        if self.reps < MIN_SAMPLES {
            return Err(invalid(format!(
                "method.reps is {} but at least {MIN_SAMPLES} repetitions are required",
                self.reps
            )));
        }
        if self.warmup_reads < 1 {
            return Err(invalid("method.warmup_reads must be at least 1"));
        }
        if self.small_bytes < 64 * 1024 {
            return Err(invalid("method.small_bytes must be at least 64 KiB"));
        }
        if self.large_bytes < 8 * MIB {
            return Err(invalid("method.large_bytes must be at least 8 MiB"));
        }
        if self.small_bytes >= self.large_bytes {
            return Err(invalid(
                "method.small_bytes must be smaller than method.large_bytes",
            ));
        }
        Ok(())
    }
}

/// A validated, complete host calibration.
#[derive(Clone, Debug)]
pub struct Calibration {
    /// Digest of the document (`H(CALIBRATION_DOMAIN, compact_json)`).
    pub id: Hash32,
    pub created_unix_ns: u64,
    pub host: HostIdentity,
    pub method: CalibrationMethod,
    rows: [CalibrationRow; 10],
    table: Arc<CostTable>,
    document: Json,
}

impl Calibration {
    /// Build and validate a record from exactly ten rows in
    /// [`CostClass::ALL`] order.
    pub fn from_rows(
        rows: [CalibrationRow; 10],
        host: HostIdentity,
        method: CalibrationMethod,
        created_unix_ns: u64,
    ) -> Result<Self> {
        method.validate()?;
        if created_unix_ns == 0 {
            return Err(invalid("created_unix_ns must be greater than zero"));
        }
        let mut seen = [false; 10];
        for row in rows.iter() {
            let index = row.class.index();
            if seen[index] {
                return Err(invalid(format!(
                    "duplicate row for class `{}`",
                    row.class.as_str()
                )));
            }
            seen[index] = true;
            validate_row(row)?;
        }
        for (index, present) in seen.iter().enumerate() {
            if !present {
                return Err(invalid(format!(
                    "missing row for class `{}`",
                    CostClass::ALL[index].as_str()
                )));
            }
        }
        let mut coefficients = [Coefficients {
            fixed_ns: 0,
            ns_per_mib: 1,
        }; 10];
        for row in rows.iter() {
            coefficients[row.class.index()] = row.coefficients();
        }
        let table = Arc::new(CostTable::from_rows(coefficients));
        validate_monotonic(&table)?;
        let mut document = json!({
            "schema": CALIBRATION_SCHEMA,
            "created_unix_ns": created_unix_ns,
            "host": host.to_json(),
            "method": method.to_json(),
            "rows": rows.iter().map(row_to_json).collect::<Vec<_>>(),
        });
        let id = digest_of(&document)?;
        document["digest"] = Json::String(id.to_hex());
        Ok(Calibration {
            id,
            created_unix_ns,
            host,
            method,
            rows,
            table,
            document,
        })
    }

    /// The validated affine table used for pricing.
    pub fn table(&self) -> Arc<CostTable> {
        Arc::clone(&self.table)
    }

    pub fn rows(&self) -> &[CalibrationRow; 10] {
        &self.rows
    }

    pub fn row(&self, class: CostClass) -> CalibrationRow {
        self.rows[class.index()]
    }

    /// Estimated nanoseconds under this calibration.
    pub fn estimate_ns(&self, class: CostClass, decoded_bytes: u64) -> u64 {
        self.table.estimate_ns(class, decoded_bytes)
    }

    /// Parse and validate a calibration document.
    pub fn from_json(value: &Json) -> Result<Self> {
        let map = exact_object(
            value,
            "calibration",
            &[
                "schema",
                "created_unix_ns",
                "host",
                "method",
                "rows",
                "digest",
            ],
        )?;
        let schema = string_field(map, "schema", "calibration")?;
        if schema != CALIBRATION_SCHEMA {
            return Err(invalid(format!(
                "schema is `{schema}`, expected `{CALIBRATION_SCHEMA}`"
            )));
        }
        let created_unix_ns = u64_field(map, "created_unix_ns", "calibration")?;
        if created_unix_ns == 0 {
            return Err(invalid("created_unix_ns must be greater than zero"));
        }
        let host =
            HostIdentity::from_json(map.get("host").ok_or_else(|| invalid("host is missing"))?)?;
        let method = CalibrationMethod::from_json(
            map.get("method")
                .ok_or_else(|| invalid("method is missing"))?,
        )?;
        let rows_value = map.get("rows").ok_or_else(|| invalid("rows is missing"))?;
        let rows_array = rows_value
            .as_array()
            .ok_or_else(|| invalid("rows must be an array"))?;
        if rows_array.len() != CostClass::ALL.len() {
            return Err(invalid(format!(
                "rows has {} entries, expected exactly {} (one per class)",
                rows_array.len(),
                CostClass::ALL.len()
            )));
        }
        let mut rows = [CalibrationRow {
            class: CostClass::Raw,
            fixed_ns: 0,
            ns_per_mib: 1,
            samples: 0,
            origin: RowOrigin::Declared,
        }; 10];
        let mut seen = [false; 10];
        for entry in rows_array {
            let row = row_from_json(entry)?;
            let index = row.class.index();
            if seen[index] {
                return Err(invalid(format!(
                    "duplicate row for class `{}`",
                    row.class.as_str()
                )));
            }
            seen[index] = true;
            rows[index] = row;
        }
        for (index, present) in seen.iter().enumerate() {
            if !present {
                return Err(invalid(format!(
                    "missing row for class `{}`",
                    CostClass::ALL[index].as_str()
                )));
            }
        }
        let digest_text = string_field(map, "digest", "calibration")?;
        let digest = Hash32::from_hex(&digest_text)
            .map_err(|err| invalid(format!("digest is not 64 hex characters: {err}")))?;
        let mut without_digest = value.clone();
        if let Some(object) = without_digest.as_object_mut() {
            object.remove("digest");
        }
        let computed = digest_of(&without_digest)?;
        if computed != digest {
            return Err(invalid(format!(
                "digest mismatch: file says {digest}, document hashes to {computed}"
            )));
        }
        let mut calibration = Calibration::from_rows(rows, host, method, created_unix_ns)?;
        calibration.id = digest;
        calibration.document = value.clone();
        Ok(calibration)
    }

    /// Load and validate a calibration file.
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .map_err(|err| Error::io(format!("calibration `{}`: {err}", path.display())))?;
        if metadata.len() > MAX_CALIBRATION_BYTES {
            return Err(invalid(format!(
                "`{}` is {} bytes, above the {MAX_CALIBRATION_BYTES}-byte bound",
                path.display(),
                metadata.len()
            )));
        }
        let bytes = fs::read(path)
            .map_err(|err| Error::io(format!("calibration `{}`: {err}", path.display())))?;
        if bytes.len() as u64 > MAX_CALIBRATION_BYTES {
            return Err(invalid(format!(
                "`{}` grew past the {MAX_CALIBRATION_BYTES}-byte bound while reading",
                path.display()
            )));
        }
        let value: Json = serde_json::from_slice(&bytes)
            .map_err(|err| invalid(format!("`{}` is not valid JSON: {err}", path.display())))?;
        Calibration::from_json(&value)
    }

    /// The canonical in-memory document (including `digest`).
    pub fn to_json(&self) -> Json {
        self.document.clone()
    }

    /// Write the document (pretty JSON; the digest is over the compact form).
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::io(format!("calibration `{}`: {err}", parent.display()))
                })?;
            }
        }
        let bytes = serde_json::to_vec_pretty(&self.document)
            .map_err(|err| invalid(format!("cannot serialise calibration: {err}")))?;
        fs::write(path, bytes)
            .map_err(|err| Error::io(format!("calibration `{}`: {err}", path.display())))
    }

    /// Atomically install the document into `<store>/calibrations/<id>.json`.
    ///
    /// The directory is advisory: it is not part of the frozen format and is
    /// not replayed, verified, or collected. Re-installing the same document is
    /// idempotent (the name is the content digest).
    pub fn install(&self, store_dir: &Path) -> Result<PathBuf> {
        let directory = store_dir.join(CALIBRATION_DIR);
        fs::create_dir_all(&directory).map_err(|err| {
            Error::io(format!(
                "cannot create calibration directory `{}`: {err}",
                directory.display()
            ))
        })?;
        let path = directory.join(format!("{}.json", self.id.to_hex()));
        let tmp = directory.join(format!(".{}.{}.tmp", self.id.to_hex(), std::process::id()));
        let bytes = serde_json::to_vec_pretty(&self.document)
            .map_err(|err| invalid(format!("cannot serialise calibration: {err}")))?;
        {
            let mut file = fs::File::create(&tmp)
                .map_err(|err| Error::io(format!("calibration `{}`: {err}", tmp.display())))?;
            file.write_all(&bytes)
                .map_err(|err| Error::io(format!("calibration `{}`: {err}", tmp.display())))?;
            file.sync_all()
                .map_err(|err| Error::io(format!("calibration `{}`: {err}", tmp.display())))?;
        }
        fs::rename(&tmp, &path).map_err(|err| {
            let _ = fs::remove_file(&tmp);
            Error::io(format!("calibration `{}`: {err}", path.display()))
        })?;
        if let Ok(dir) = fs::File::open(&directory) {
            let _ = dir.sync_all();
        }
        Ok(path)
    }

    /// Per-class divergence between this calibration and a baked table.
    pub fn divergence(&self, baked: CostModelKind) -> Vec<Divergence> {
        let baked_table = CostTable::baked(baked);
        CostClass::ALL
            .iter()
            .map(|class| {
                let host = self.row(*class).coefficients();
                let reference = baked_table.row(*class);
                Divergence {
                    class: *class,
                    host,
                    baked: reference,
                    host_1mib_ns: self.table.estimate_ns(*class, MIB),
                    baked_1mib_ns: baked_table.estimate_ns(*class, MIB),
                }
            })
            .collect()
    }
}

/// One row of the host-vs-baked divergence report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub class: CostClass,
    pub host: Coefficients,
    pub baked: Coefficients,
    /// Host estimate for a 1 MiB payload.
    pub host_1mib_ns: u64,
    /// Baked estimate for a 1 MiB payload.
    pub baked_1mib_ns: u64,
}

impl Divergence {
    /// Signed parts-per-million change in the per-MiB coefficient.
    pub fn ns_per_mib_delta_ppm(&self) -> i64 {
        signed_ppm(self.host.ns_per_mib, self.baked.ns_per_mib)
    }

    /// Signed parts-per-million change in the 1 MiB estimate.
    pub fn estimate_1mib_delta_ppm(&self) -> i64 {
        signed_ppm(self.host_1mib_ns, self.baked_1mib_ns)
    }

    /// Human-readable percent of [`Self::estimate_1mib_delta_ppm`].
    pub fn estimate_1mib_delta_percent(&self) -> f64 {
        self.estimate_1mib_delta_ppm() as f64 / 10_000.0
    }
}

fn signed_ppm(value: u64, reference: u64) -> i64 {
    if reference == 0 {
        return if value == 0 { 0 } else { i64::MAX };
    }
    let numerator = i128::from(value) - i128::from(reference);
    let scaled = numerator * 1_000_000i128 / i128::from(reference);
    i64::try_from(scaled).unwrap_or(if scaled < 0 { i64::MIN } else { i64::MAX })
}

/// Measurement parameters for [`measure_decode_costs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasurementOptions {
    pub reps: usize,
    pub large_mib: u64,
    pub small_kib: u64,
}

impl Default for MeasurementOptions {
    fn default() -> Self {
        MeasurementOptions {
            reps: 7,
            large_mib: 16,
            small_kib: 128,
        }
    }
}

impl MeasurementOptions {
    pub fn validate(&self) -> Result<()> {
        if self.reps < MIN_SAMPLES as usize {
            return Err(invalid(format!(
                "the calibration requires at least {MIN_SAMPLES} repetitions"
            )));
        }
        if self.large_mib < 8
            || self.small_kib < 64
            || self.small_kib * 1024 >= self.large_mib * MIB
        {
            return Err(invalid(
                "sizes must satisfy 8 <= large MiB and 64 KiB <= small < large",
            ));
        }
        Ok(())
    }
}

/// Measurement output: one row per representable class plus any errors.
#[derive(Clone, Debug, Default)]
pub struct MeasurementOutcome {
    pub rows: Vec<CalibrationRow>,
    pub errors: Vec<String>,
    /// Raw medians per class, kept for the historical bench receipt shape.
    pub details: Vec<MeasuredClass>,
}

/// Per-class medians behind one measured row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasuredClass {
    pub class: CostClass,
    pub small_bytes: u64,
    pub large_bytes: u64,
    pub small_median_ns: u64,
    pub large_median_ns: u64,
    pub small_dependency_median_ns: Option<u64>,
    pub large_dependency_median_ns: Option<u64>,
}

/// Measure every planner representation on `store` rooted at `root`.
///
/// The caller keeps the store; this never deletes anything.
pub fn measure_decode_costs(
    store: &mut Store,
    root: Hash32,
    options: &MeasurementOptions,
) -> Result<MeasurementOutcome> {
    options.validate()?;
    let layout = TensorLayoutSpec::row_major();
    let mut outcome = MeasurementOutcome::default();
    let classes = [
        (CostClass::Raw, TensorStrategy::Raw),
        (CostClass::Zstd, TensorStrategy::Zstd),
        (CostClass::FloatTensor, TensorStrategy::FloatTensor),
        (CostClass::TensorSharded, TensorStrategy::TensorSharded),
        (CostClass::TensorTiled, TensorStrategy::TensorTiled),
        (CostClass::WeightDelta, TensorStrategy::WeightDelta),
        (CostClass::SemanticDelta, TensorStrategy::SemanticDelta),
    ];
    for (class, strategy) in classes {
        let mut measured = Vec::new();
        for target_bytes in [options.small_kib * 1024, options.large_mib * MIB] {
            match measure(store, root, &layout, strategy, target_bytes, options.reps) {
                Ok(row) => measured.push(row),
                Err(err) => {
                    outcome
                        .errors
                        .push(format!("{} @ {target_bytes} bytes: {err}", class.as_str()));
                    break;
                }
            }
        }
        if measured.len() != 2 {
            continue;
        }
        let small = &measured[0];
        let large = &measured[1];
        // Delta rows are fitted on the *incremental* frame cost: the base read
        // is measured with the same repetitions and subtracted, so a delta's
        // base can be priced recursively by the planner.
        let small_ns = (small
            .median_ns
            .saturating_sub(small.dependency_median_ns.unwrap_or(0))) as f64;
        let large_ns = (large
            .median_ns
            .saturating_sub(large.dependency_median_ns.unwrap_or(0))) as f64;
        let delta_bytes = (large.logical_bytes - small.logical_bytes) as f64;
        let per_byte = if large_ns > small_ns && delta_bytes > 0.0 {
            (large_ns - small_ns) / delta_bytes
        } else {
            0.0
        };
        let fixed = (small_ns - per_byte * small.logical_bytes as f64).max(0.0);
        let ns_per_mib = (per_byte * MIB as f64).round() as u64;
        let fixed_ns = fixed.round() as u64;
        outcome.rows.push(CalibrationRow {
            class,
            fixed_ns,
            ns_per_mib: ns_per_mib.max(1),
            samples: (options.reps * 2) as u64,
            origin: RowOrigin::Measured,
        });
        outcome.details.push(MeasuredClass {
            class,
            small_bytes: small.logical_bytes,
            large_bytes: large.logical_bytes,
            small_median_ns: u64::try_from(small.median_ns).unwrap_or(u64::MAX),
            large_median_ns: u64::try_from(large.median_ns).unwrap_or(u64::MAX),
            small_dependency_median_ns: small
                .dependency_median_ns
                .and_then(|value| u64::try_from(value).ok()),
            large_dependency_median_ns: large
                .dependency_median_ns
                .and_then(|value| u64::try_from(value).ok()),
        });
    }
    Ok(outcome)
}

/// Measure in a throw-away store under the system temporary directory.
pub fn measure_decode_costs_scratch(options: &MeasurementOptions) -> Result<MeasurementOutcome> {
    options.validate()?;
    let store_path = std::env::temp_dir().join(format!(
        "weft-decode-calibration-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    if store_path.exists() {
        fs::remove_dir_all(&store_path)
            .map_err(|err| Error::io(format!("calibration scratch store: {err}")))?;
    }
    let result = (|| {
        let mut store = Store::init(
            &store_path,
            Options {
                chunk_size: 64 * 1024,
                checkpoint_interval: 512,
                gc_grace_secs: 0,
                ..Options::default()
            },
        )?;
        let root = store.resolve_path("/")?;
        let outcome = measure_decode_costs(&mut store, root, options)?;
        drop(store);
        Ok(outcome)
    })();
    let _ = fs::remove_dir_all(&store_path);
    result
}

/// Build a complete calibration from a measurement.
///
/// The seven measured classes must all be present; any measurement error is a
/// hard failure, because a calibration with a guessed row is worse than no
/// calibration. SPARSE_DELTA reuses WEIGHT_DELTA (the store never produces a
/// separately calibrated sparse row), and KV_CACHE/ACTIVATION keep the
/// declared rows (the store never produces either representation).
pub fn record_from_measurement(
    outcome: &MeasurementOutcome,
    options: &MeasurementOptions,
    host: HostIdentity,
    created_unix_ns: u64,
) -> Result<Calibration> {
    if !outcome.errors.is_empty() {
        return Err(invalid(format!(
            "calibration measurement failed: {}",
            outcome.errors.join("; ")
        )));
    }
    let method = CalibrationMethod {
        reps: options.reps as u64,
        warmup_reads: 1,
        small_bytes: options.small_kib * 1024,
        large_bytes: options.large_mib * MIB,
    };
    let measured = |class: CostClass| -> Result<CalibrationRow> {
        outcome
            .rows
            .iter()
            .find(|row| row.class == class)
            .copied()
            .ok_or_else(|| {
                invalid(format!(
                    "calibration measurement did not produce a `{}` row",
                    class.as_str()
                ))
            })
    };
    let weight_delta = measured(CostClass::WeightDelta)?;
    let mut rows = [CalibrationRow {
        class: CostClass::Raw,
        fixed_ns: 0,
        ns_per_mib: 1,
        samples: 0,
        origin: RowOrigin::Declared,
    }; 10];
    for class in [
        CostClass::Raw,
        CostClass::Zstd,
        CostClass::FloatTensor,
        CostClass::TensorSharded,
        CostClass::TensorTiled,
        CostClass::WeightDelta,
        CostClass::SemanticDelta,
    ] {
        rows[class.index()] = measured(class)?;
    }
    rows[CostClass::SparseDelta.index()] = CalibrationRow {
        class: CostClass::SparseDelta,
        origin: RowOrigin::Reused,
        samples: 0,
        ..weight_delta
    };
    for class in [CostClass::KvCache, CostClass::Activation] {
        let baked = cost_model::coefficients(class, CostModelKind::Declared);
        rows[class.index()] = CalibrationRow {
            class,
            fixed_ns: baked.fixed_ns,
            ns_per_mib: baked.ns_per_mib,
            samples: 0,
            origin: RowOrigin::Declared,
        };
    }
    Calibration::from_rows(rows, host, method, created_unix_ns)
}

fn row_to_json(row: &CalibrationRow) -> Json {
    json!({
        "class": row.class.as_str(),
        "fixed_ns": row.fixed_ns,
        "ns_per_mib": row.ns_per_mib,
        "samples": row.samples,
        "origin": row.origin.as_str(),
    })
}

fn row_from_json(value: &Json) -> Result<CalibrationRow> {
    let map = exact_object(
        value,
        "row",
        &["class", "fixed_ns", "ns_per_mib", "samples", "origin"],
    )?;
    let name = string_field(map, "class", "row")?;
    let class =
        CostClass::parse(&name).ok_or_else(|| invalid(format!("unknown cost class `{name}`")))?;
    let origin_text = string_field(map, "origin", "row")?;
    let origin = RowOrigin::parse(&origin_text)
        .ok_or_else(|| invalid(format!("unknown row origin `{origin_text}`")))?;
    Ok(CalibrationRow {
        class,
        fixed_ns: u64_field(map, "fixed_ns", "row")?,
        ns_per_mib: u64_field(map, "ns_per_mib", "row")?,
        samples: u64_field(map, "samples", "row")?,
        origin,
    })
}

fn validate_row(row: &CalibrationRow) -> Result<()> {
    let name = row.class.as_str();
    if row.fixed_ns > MAX_FIXED_NS {
        return Err(invalid(format!(
            "`{name}` fixed_ns {} exceeds the {MAX_FIXED_NS} ns bound",
            row.fixed_ns
        )));
    }
    if row.ns_per_mib == 0 {
        return Err(invalid(format!(
            "`{name}` ns_per_mib must be greater than zero"
        )));
    }
    if row.ns_per_mib > MAX_NS_PER_MIB {
        return Err(invalid(format!(
            "`{name}` ns_per_mib {} exceeds the {MAX_NS_PER_MIB} ns/MiB bound",
            row.ns_per_mib
        )));
    }
    match row.origin {
        RowOrigin::Measured => {
            if row.samples < MIN_SAMPLES {
                return Err(invalid(format!(
                    "`{name}` is measured but has {} samples, below {MIN_SAMPLES}",
                    row.samples
                )));
            }
        }
        RowOrigin::Reused | RowOrigin::Declared => {
            if row.samples != 0 {
                return Err(invalid(format!(
                    "`{name}` is {} but records {} samples; only measured rows carry samples",
                    row.origin.as_str(),
                    row.samples
                )));
            }
        }
    }
    Ok(())
}

/// Every row must price a larger payload as strictly more expensive.
fn validate_monotonic(table: &CostTable) -> Result<()> {
    for class in CostClass::ALL {
        let small = table.estimate_ns(class, MIB);
        let large = table.estimate_ns(class, 64 * MIB);
        if large <= small {
            return Err(invalid(format!(
                "`{}` is not monotonic: {small} ns at 1 MiB, {large} ns at 64 MiB",
                class.as_str()
            )));
        }
    }
    Ok(())
}

fn digest_of(document: &Json) -> Result<Hash32> {
    let bytes = serde_json::to_vec(document)
        .map_err(|err| invalid(format!("cannot serialise calibration: {err}")))?;
    Ok(h(CALIBRATION_DOMAIN, &bytes))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::invalid_argument(format!("calibration: {}", message.into()))
}

fn exact_object<'a>(
    value: &'a Json,
    context: &str,
    keys: &[&str],
) -> Result<&'a Map<String, Json>> {
    let map = value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be a JSON object")))?;
    for key in map.keys() {
        if !keys.contains(&key.as_str()) {
            return Err(invalid(format!(
                "{context} has unknown key `{key}` (expected {keys:?})"
            )));
        }
    }
    for key in keys {
        if !map.contains_key(*key) {
            return Err(invalid(format!("{context} is missing key `{key}`")));
        }
    }
    Ok(map)
}

fn string_field(map: &Map<String, Json>, key: &str, context: &str) -> Result<String> {
    map.get(key)
        .and_then(Json::as_str)
        .map(|value| value.to_owned())
        .ok_or_else(|| invalid(format!("{context}.{key} must be a string")))
}

fn u64_field(map: &Map<String, Json>, key: &str, context: &str) -> Result<u64> {
    map.get(key)
        .and_then(Json::as_u64)
        .ok_or_else(|| invalid(format!("{context}.{key} must be an unsigned integer")))
}

// ---- measurement ---------------------------------------------------------

struct Measurement {
    logical_bytes: u64,
    median_ns: u128,
    dependency_median_ns: Option<u128>,
}

fn measure(
    store: &mut Store,
    root: Hash32,
    layout: &TensorLayoutSpec,
    strategy: TensorStrategy,
    target_bytes: u64,
    reps: usize,
) -> Result<Measurement> {
    let elements = target_bytes / 2;
    let shape = vec![elements / 1024, 1024];
    let (dtype, shape, bytes, spec, base_id) = match strategy {
        TensorStrategy::TensorSharded => {
            let elements = target_bytes;
            (
                "q8_0".to_owned(),
                vec![elements],
                q8_payload(elements),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    ..TensorWriteSpec::default()
                },
                None,
            )
        }
        TensorStrategy::TensorTiled => (
            "bf16".to_owned(),
            shape,
            bf16_payload(elements),
            TensorWriteSpec {
                strategy_hint: Some(strategy),
                tile_shape: Some(vec![32, 256]),
                ..TensorWriteSpec::default()
            },
            None,
        ),
        TensorStrategy::WeightDelta => {
            let (base_id, base_bytes) = base_case(store, root, layout, target_bytes, false)?;
            let base = store.object_meta(base_id)?;
            (
                "bf16".to_owned(),
                shape,
                nudge(&base_bytes),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    causal: vec![base.version],
                    ..TensorWriteSpec::default()
                },
                Some(base_id),
            )
        }
        TensorStrategy::SemanticDelta => {
            let (base_id, base_bytes) = base_case(store, root, layout, target_bytes, true)?;
            (
                "bf16".to_owned(),
                shape,
                clear_low_mantissa_bits(&base_bytes, 3),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    plan: TensorPlanOptions {
                        semantic_policy: Some(SemanticPolicy::MantissaTruncate { bits: 3 }),
                        candidates: vec![base_id],
                        ..TensorPlanOptions::default()
                    },
                    ..TensorWriteSpec::default()
                },
                Some(base_id),
            )
        }
        _ => (
            "bf16".to_owned(),
            shape,
            bf16_payload(elements),
            TensorWriteSpec {
                strategy_hint: Some(strategy),
                ..TensorWriteSpec::default()
            },
            None,
        ),
    };
    let dir = store
        .mkdir(root, strategy.as_str().as_bytes(), 0o755, 0, 0)
        .or_else(|err| {
            if err.class() == crate::error::Class::Exists {
                store.resolve_components(&[strategy.as_str().as_bytes()])
            } else {
                Err(err)
            }
        })?;
    let id = store.write_tensor_file_with_spec(
        dir,
        format!("calibration-{target_bytes}").as_bytes(),
        &dtype,
        &shape,
        &bytes,
        layout,
        &spec,
    )?;
    let meta = store.tensor_metadata(id)?;
    if meta.strategy != strategy.as_str() {
        return Err(invalid(format!(
            "requested `{}` but the store recorded `{}`",
            strategy.as_str(),
            meta.strategy
        )));
    }
    // Warm-up read (also verifies the object is decodable).
    store.read_tensor(id)?;
    let dependency_median_ns = match base_id {
        Some(base_id) => {
            store.read_tensor(base_id)?;
            let mut base_ns = Vec::with_capacity(reps);
            for _ in 0..reps {
                let started = Instant::now();
                store.read_tensor(base_id)?;
                base_ns.push(started.elapsed().as_nanos());
            }
            base_ns.sort_unstable();
            Some(base_ns[base_ns.len() / 2])
        }
        None => None,
    };
    let mut reps_ns = Vec::with_capacity(reps);
    for _ in 0..reps {
        let started = Instant::now();
        let data = store.read_tensor(id)?;
        let elapsed = started.elapsed().as_nanos();
        if data.len() as u64 != target_bytes {
            return Err(invalid(format!(
                "decoded {} bytes, expected {target_bytes}",
                data.len()
            )));
        }
        reps_ns.push(elapsed);
    }
    let mut sorted = reps_ns;
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    Ok(Measurement {
        logical_bytes: target_bytes,
        median_ns: median,
        dependency_median_ns,
    })
}

/// Build a base object used by the delta cases; returns its id and bytes.
fn base_case(
    store: &mut Store,
    root: Hash32,
    layout: &TensorLayoutSpec,
    target_bytes: u64,
    semantic: bool,
) -> Result<(Hash32, Vec<u8>)> {
    let elements = target_bytes / 2;
    let shape = vec![elements / 1024, 1024];
    let base_bytes = bf16_payload(elements);
    let dir = store
        .mkdir(root, b"calibration-bases", 0o755, 0, 0)
        .or_else(|err| {
            if err.class() == crate::error::Class::Exists {
                store.resolve_components(&[b"calibration-bases"])
            } else {
                Err(err)
            }
        })?;
    let name = format!(
        "base-{}-{target_bytes}",
        if semantic { "semantic" } else { "weight" }
    );
    let base_id = match store.resolve_components(&[b"calibration-bases", name.as_bytes()]) {
        Ok(id) => id,
        Err(_) => store.write_tensor_file_with_spec(
            dir,
            name.as_bytes(),
            "bf16",
            &shape,
            &base_bytes,
            layout,
            &TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::Raw),
                ..TensorWriteSpec::default()
            },
        )?,
    };
    Ok((base_id, base_bytes))
}

/// Deterministic weight-like BF16 payload.
fn bf16_payload(elements: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements as usize * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32);
        let value = (unit * 2.0 - 1.0) * 0.5;
        let bits = value.to_bits();
        out.extend_from_slice(&((bits >> 16) as u16).to_le_bytes());
    }
    out
}

/// q8_0 logical bytes (one two's-complement byte per element).
fn q8_payload(elements: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements as usize);
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state >> 32) as u8);
    }
    out
}

/// Flip the lowest bit of every 4096th 16-bit word (a sparse weight update).
fn nudge(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let words = out.len() / 2;
    for index in (0..words).step_by(4096) {
        out[index * 2] ^= 1;
    }
    out
}

/// Clear the low `bits` mantissa bits while preserving sign/exponent.
fn clear_low_mantissa_bits(bytes: &[u8], bits: u32) -> Vec<u8> {
    let mask = !(((1u16 << bits) - 1) as u16);
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.chunks_exact(2) {
        let word = u16::from_le_bytes([chunk[0], chunk[1]]) & mask;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_host() -> HostIdentity {
        HostIdentity {
            hostname: "test".to_owned(),
            kernel: "test".to_owned(),
            arch: "test".to_owned(),
            cpus: 1,
            image: "test".to_owned(),
            image_id: "test".to_owned(),
            memory_limit: "3g".to_owned(),
            pids_limit: "1024".to_owned(),
            weftfs_revision: "test".to_owned(),
            weftcodec_revision: "test".to_owned(),
        }
    }

    fn dummy_method() -> CalibrationMethod {
        CalibrationMethod {
            reps: 7,
            warmup_reads: 1,
            small_bytes: 128 * 1024,
            large_bytes: 16 * MIB,
        }
    }

    fn dummy_rows() -> [CalibrationRow; 10] {
        let mut rows = [CalibrationRow {
            class: CostClass::Raw,
            fixed_ns: 10_000,
            ns_per_mib: 300_000,
            samples: 14,
            origin: RowOrigin::Measured,
        }; 10];
        for (index, class) in CostClass::ALL.iter().enumerate() {
            rows[index].class = *class;
            rows[index].fixed_ns = 10_000 + index as u64;
            rows[index].ns_per_mib = 100_000 * (index as u64 + 1);
        }
        rows
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = Calibration::from_rows(
            dummy_rows(),
            dummy_host(),
            dummy_method(),
            1_762_000_000_000_000_000,
        )
        .unwrap();
        let json = record.to_json();
        let parsed = Calibration::from_json(&json).unwrap();
        assert_eq!(parsed.id, record.id);
        assert_eq!(parsed.rows(), record.rows());
        for class in CostClass::ALL {
            assert_eq!(parsed.row(class), record.row(class));
        }
        assert_eq!(parsed.host, record.host);
        assert_eq!(parsed.method, record.method);
    }

    #[test]
    fn digest_detects_a_modified_row() {
        let record = Calibration::from_rows(
            dummy_rows(),
            dummy_host(),
            dummy_method(),
            1_762_000_000_000_000_000,
        )
        .unwrap();
        let mut json = record.to_json();
        json["rows"][0]["ns_per_mib"] = json!(999_999u64);
        let err = Calibration::from_json(&json).unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "{err}");
    }
}
