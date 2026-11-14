//! Transaction history emission for the independent checker (subphase T026).
//!
//! When `Options::history_path` (or `WEFT_HISTORY`) names a file, the store
//! appends one JSON line per observable event:
//!
//! * `tx-invoke`  — transaction id, client handle, domain, base generation, and
//!   the canonical key/value of every op;
//! * `tx-response` — committed (generation, commit, attempts) or a typed
//!   conflict/error class with the generation the handle observed;
//! * `read-invoke` / `read-response` — a lookup or object read, its key, the
//!   generation it observed, and the value it returned;
//! * `note` — free-form diagnostics such as the handle's identity.
//!
//! The format is JSON Lines, schema `weft.history.v1`. It is an **observation**
//! channel: a failed write is counted and otherwise ignored, and no commit,
//! verification or GC decision reads it. The checker (`verifier/history_check.py`)
//! is a separate program that consumes the file; the store has no checker code.
//!
//! Why a file rather than a new wire format: the history is a test and audit
//! artifact, not store state. It must be producible by both MW2 and MW3 runs and
//! by several independent processes appending to one path, which O_APPEND gives
//! without a coordinator.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::error::{Error, Result};

/// Schema string carried on every record.
pub const HISTORY_SCHEMA: &str = "weft.history.v1";

/// One append-only history file.
pub struct HistorySink {
    file: Mutex<std::fs::File>,
    pid: u32,
    /// Writes that failed. The sink never fails a store operation.
    failures: AtomicU64,
}

impl HistorySink {
    /// Open (or create) the history path for appending.
    ///
    /// `None` when neither the option nor `WEFT_HISTORY` is set. An explicitly
    /// named path that cannot be created is an error at open: a caller that
    /// asked for a history must not silently get none.
    pub fn from_options(options: &crate::store::Options) -> Result<Option<HistorySink>> {
        let path = options
            .history_path
            .clone()
            .or_else(|| std::env::var("WEFT_HISTORY").ok())
            .filter(|value| !value.is_empty());
        let Some(path) = path else {
            return Ok(None);
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(Path::new(&path))
            .map_err(|err| Error::io(format!("cannot open the history file {path}: {err}")))?;
        Ok(Some(HistorySink {
            file: Mutex::new(file),
            pid: std::process::id(),
            failures: AtomicU64::new(0),
        }))
    }

    /// Append one record. The caller supplies the `event` field.
    pub fn emit(&self, mut record: serde_json::Value) {
        if let serde_json::Value::Object(map) = &mut record {
            map.insert(
                "schema".to_owned(),
                serde_json::Value::String(HISTORY_SCHEMA.to_owned()),
            );
            map.insert("pid".to_owned(), serde_json::json!(self.pid));
            map.insert(
                "observed_ns".to_owned(),
                serde_json::json!(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos() as u64)
                    .unwrap_or(0)),
            );
        }
        let mut line = record.to_string();
        line.push('\n');
        let mut file = match self.file.lock() {
            Ok(file) => file,
            Err(poisoned) => poisoned.into_inner(),
        };
        // One `write_all` per record: O_APPEND makes a single write append at
        // the current end atomically with respect to other appenders, so
        // several independent processes can share one history file.
        if file.write_all(line.as_bytes()).is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Number of records this process failed to write.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}
