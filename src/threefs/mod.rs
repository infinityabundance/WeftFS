//! 3FS transports.
//!
//! DeepSeek 3FS exposes two interfaces a storage engine can use without
//! replacing it: a POSIX namespace (via its FUSE client) and the native
//! user-space ring API, USRBIO. This module adds both as explicit transports of
//! the *same* on-disk WeftFS layout, so the bytes and the commit protocol do not
//! depend on which one is chosen.
//!
//! * [`ThreeFsMode::Posix`] — the normative layout under a directory served by a
//!   3FS mount. Every operation is an ordinary positional `pread`/`pwrite`,
//!   `fsync`, and `rename`. This is the correctness baseline and the mode that
//!   carries metadata/control I/O in both modes.
//! * [`ThreeFsMode::Usrbio`] — the same namespace, but immutable data-plane
//!   reads and staging writes go through [`ffi::Usrbio`]: one shared memory
//!   region, a read ring and a write ring, batched submission and completion
//!   reaping. Control state (commit records, superblock, lock, indexes) still
//!   uses POSIX, because USRBIO carries data I/O only.
//!
//! Fail-closed rules:
//!
//! * `usrbio` mode refuses to open when the client library is missing or lacks
//!   a required symbol. It never silently degrades to POSIX.
//! * `usrbio` mode refuses when the library loads but the path is not a 3FS
//!   mount *only if* the caller asked for [`ThreeFsConfig::require_3fs_mount`].
//!   The default records the answer instead, because an ABI conformance harness
//!   must be able to drive the transport without a 3FS deployment, and the
//!   answer travels in the diagnostics so a receipt can never imply otherwise.
//!
//! # Honest status
//!
//! The POSIX transport is byte-identical to the local backend and is exercised
//! by the ordinary store courts. The USRBIO transport is exercised against a
//! conformance library that implements the documented ABI over real POSIX I/O
//! (see `bench/threefs/`); **no 3FS deployment is run by this repository**, so
//! no 3FS throughput, latency, RDMA, or multi-node claim is made.

pub mod ffi;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::Capabilities;
use crate::error::{Error, Result};
use crate::format::MAX_CHUNK_SIZE;

pub use ffi::{Counters as UsrbioCounters, ReadRequest, SharedUsrbio, Usrbio};

/// How a 3FS-backed store performs data I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreeFsMode {
    /// Ordinary POSIX I/O through the 3FS FUSE mount.
    Posix,
    /// Native USRBIO rings for the immutable data plane.
    Usrbio,
}

impl ThreeFsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreeFsMode::Posix => "posix",
            ThreeFsMode::Usrbio => "usrbio",
        }
    }

    pub fn parse(text: &str) -> Result<ThreeFsMode> {
        match text {
            "posix" => Ok(ThreeFsMode::Posix),
            "usrbio" => Ok(ThreeFsMode::Usrbio),
            other => Err(Error::invalid_argument(format!(
                "3FS transport must be `posix` or `usrbio` (got `{other}`)"
            ))),
        }
    }
}

/// Resolved 3FS transport configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeFsConfig {
    pub mode: ThreeFsMode,
    /// 3FS mount point; also the `hf3fs_mount_point` argument for the rings.
    /// Defaults to the store root, which is what a store created *inside* a 3FS
    /// mount wants.
    pub mount_point: Option<PathBuf>,
    /// Explicit `libhf3fs_api_shared.so`; `None` uses `WEFT_3FS_LIB`.
    pub library: Option<PathBuf>,
    pub entries: usize,
    pub io_bytes: usize,
    pub io_depth: i32,
    pub timeout_ms: i32,
    /// Refuse when `hf3fs_is_hf3fs` reports the mount is not 3FS.
    pub require_3fs_mount: bool,
}

impl Default for ThreeFsConfig {
    fn default() -> ThreeFsConfig {
        ThreeFsConfig {
            mode: ThreeFsMode::Posix,
            mount_point: None,
            library: None,
            entries: 256,
            io_bytes: 8 << 20,
            io_depth: 0,
            timeout_ms: 0,
            require_3fs_mount: false,
        }
    }
}

impl ThreeFsConfig {
    /// Resolve CLI values, keeping the defaults for anything omitted.
    pub fn resolve(
        mode: &str,
        mount_point: Option<PathBuf>,
        library: Option<PathBuf>,
        entries: Option<usize>,
        io_bytes: Option<usize>,
        require_3fs_mount: bool,
    ) -> Result<ThreeFsConfig> {
        let mut config = ThreeFsConfig {
            mode: ThreeFsMode::parse(mode)?,
            mount_point,
            library,
            require_3fs_mount,
            ..ThreeFsConfig::default()
        };
        if let Some(entries) = entries {
            config.entries = entries;
        }
        if let Some(bytes) = io_bytes {
            config.io_bytes = bytes;
        }
        let options = config.usrbio_options(Path::new("."));
        options.validate()?;
        Ok(config)
    }

    fn usrbio_options(&self, store_root: &Path) -> ffi::Options {
        ffi::Options {
            mount_point: self
                .mount_point
                .clone()
                .unwrap_or_else(|| store_root.to_path_buf()),
            library: self.library.clone(),
            entries: self.entries,
            io_bytes: self.io_bytes,
            io_depth: self.io_depth,
            timeout_ms: self.timeout_ms,
            numa: -1,
        }
    }

    /// Open the transport, if this mode has one. `Posix` returns `None`.
    pub fn open(&self, store_root: &Path) -> Result<Option<SharedUsrbio>> {
        match self.mode {
            ThreeFsMode::Posix => Ok(None),
            ThreeFsMode::Usrbio => {
                let options = self.usrbio_options(store_root);
                options.validate()?;
                let transport = Arc::new(Usrbio::open(&options)?);
                // Probe once at open so diagnostics and receipts can state the
                // answer without touching the filesystem later.
                let is_3fs = transport.mount_is_3fs()?;
                if self.require_3fs_mount && !is_3fs {
                    return Err(Error::unsupported_version(format!(
                        "3FS transport `usrbio` was configured with require_3fs_mount, but \
                         hf3fs_is_hf3fs reports that `{}` is not served by 3FS",
                        transport.mount_point().display()
                    )));
                }
                Ok(Some(transport))
            }
        }
    }
}

/// Declared capabilities of the 3FS POSIX transport.
///
/// These are the same guarantees as [`Capabilities::local_posix`], re-stated so
/// a receipt names the transport. They remain **declared**, not measured: no 3FS
/// deployment is available in the courts, so the store's own writable-open
/// probes are the only thing standing behind them.
pub fn posix_capabilities() -> Capabilities {
    Capabilities {
        atomic_publish: true,
        durable_fsync: true,
        true_range_reads: true,
        max_object_bytes: MAX_CHUNK_SIZE * 64,
        conditional_head: true,
        consistency: "3FS POSIX namespace through its FUSE client; single writer held by the \
                      store's exclusive lock; rename(2) is atomic and the parent directory fsync \
                      orders the new name. Declared, not measured: no 3FS deployment was run."
            .to_owned(),
        publication: "tmp file + fsync + rename(2) + directory fsync (3FS POSIX transport)"
            .to_owned(),
    }
}

/// Declared capabilities of the 3FS USRBIO transport: POSIX publication with the
/// data plane moved onto the ring API.
pub fn usrbio_capabilities() -> Capabilities {
    Capabilities {
        consistency: "3FS POSIX namespace for control state plus hf3fs USRBIO rings for the \
                      immutable data plane; single writer held by the store's exclusive lock. \
                      Declared, not measured: no 3FS deployment was run."
            .to_owned(),
        publication: "control: tmp file + fsync + rename(2) + directory fsync; data: shared \
                      region + hf3fs_prep_io/submit_ios/wait_for_ios"
            .to_owned(),
        ..posix_capabilities()
    }
}

/// Capabilities for a configured mode.
pub fn capabilities_for(mode: ThreeFsMode) -> Capabilities {
    match mode {
        ThreeFsMode::Posix => posix_capabilities(),
        ThreeFsMode::Usrbio => usrbio_capabilities(),
    }
}

/// A resolved transport the backend can report on.
#[derive(Clone, Debug, Default)]
pub struct ThreeFsStatus {
    pub mode: Option<ThreeFsMode>,
    pub mount_point: Option<PathBuf>,
    pub library: Option<PathBuf>,
    /// `hf3fs_is_hf3fs(mount)`; `None` for POSIX mode or before the first probe.
    pub mount_is_3fs: Option<bool>,
    pub counters: UsrbioCounters,
    pub detail: String,
}

impl ThreeFsStatus {
    pub fn describe(&self) -> String {
        let mode = self.mode.map(|mode| mode.as_str()).unwrap_or("none");
        let mount = self
            .mount_point
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_owned());
        let mut text = format!("3fs transport={mode} mount={mount}");
        if let Some(library) = &self.library {
            text.push_str(&format!(" library={}", library.display()));
        }
        if let Some(is_3fs) = self.mount_is_3fs {
            text.push_str(&format!(" hf3fs_is_hf3fs={is_3fs}"));
        }
        text.push_str(&format!(
            " submissions={} requests={} completions={} bytes_read={} bytes_written={} \
             direct_reads={} staged_reads={} registrations={}",
            self.counters.submissions,
            self.counters.requests,
            self.counters.completions,
            self.counters.bytes_read,
            self.counters.bytes_written,
            self.counters.direct_reads,
            self.counters.staged_reads,
            self.counters.registrations
        ));
        if !self.detail.is_empty() {
            text.push_str(": ");
            text.push_str(&self.detail);
        }
        text
    }
}
