//! Persistent, in-process checkpoint service (subphase S3).
//!
//! The Python DCP/Orbax adapters used to start the `weftfs` CLI once per call
//! and to stage every restore through a host safetensors file. This module
//! replaces that with one long-lived helper process:
//!
//! * the store is opened **read-only once** and that handle serves every read;
//! * requests and responses are length-prefixed binary frames over a unix
//!   socket (stdin/stdout framing was rejected because a caller-owned restore
//!   buffer must cross the process boundary — `SCM_RIGHTS` file descriptors are
//!   only expressible on a socket);
//! * a tensor is restored directly into a **caller-supplied memfd**; no file
//!   the store knows about (or any other file) is created;
//! * a checkpoint is written from a caller-supplied memfd holding the canonical
//!   safetensors container, so save no longer stages a host file either;
//! * every control frame is bounded, exactly one request is in flight, and a
//!   malformed frame terminates the service with a non-zero exit instead of
//!   trying to resynchronise a stream that cannot be resynchronised.
//!
//! The wire format, bounds, and failure semantics are normative in
//! `docs/CHECKPOINT-SERVICE.md`. Nothing here changes the frozen on-disk or
//! checkpoint wire formats: the service calls the same `Store` APIs the CLI
//! calls.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use weftcodec::Region;

use crate::backend::BackendKind;
use crate::checkpoint::{
    CheckpointManifest, CheckpointWriteOptions, RestoreTopology, ShardStrategy,
};
use crate::error::{Class, Error, Result};
use crate::identity::Hash32;
use crate::safetensors::SafetensorsFile;
use crate::store::{OpenMode, Options, Store};

mod ancillary;

/// Protocol version carried by the `INFO` response.
pub const PROTOCOL_VERSION: u32 = 3;
/// Largest accepted control frame (header + body) in either direction.
pub const MAX_FRAME_BYTES: usize = 1 << 20;
/// Default bound on one restored tensor / one staged container.
pub const DEFAULT_MAX_TENSOR_BYTES: u64 = 4 << 30;
/// Default idle window for a held writable handle.
///
/// `0` (the default) releases the writer lock immediately after every
/// publication, which is the historical behaviour and means two helpers on one
/// store never collide on the lock. `CheckpointSession` passes a positive
/// window explicitly; only then does the service hold the lock between saves.
pub const DEFAULT_SESSION_IDLE_SECS: u64 = 0;
/// Bound on one `WRITE_FILE` payload (metadata blobs, never tensors).
pub const MAX_FILE_BYTES: u64 = 16 << 20;

/// Request opcodes.
pub mod op {
    pub const INFO: u8 = 0x01;
    pub const LIST_CHECKPOINTS: u8 = 0x02;
    pub const CHECKPOINT_MANIFEST: u8 = 0x03;
    pub const RESTORE_TENSOR: u8 = 0x04;
    pub const WRITE_CHECKPOINT: u8 = 0x05;
    pub const VERIFY_CHECKPOINT: u8 = 0x06;
    /// Reopen the read handle so commits made by *another* writer become
    /// visible (the handle is otherwise pinned to the state at open).
    pub const REFRESH: u8 = 0x07;
    /// Publish one small file (for example DCP/Orbax metadata) from a memfd.
    pub const WRITE_FILE: u8 = 0x08;
    /// Report whether a store path exists and its size.
    pub const STAT_FILE: u8 = 0x09;
    /// Read one small store file into a caller-supplied memfd.
    pub const READ_FILE: u8 = 0x0a;
    /// Diagnostic phase-profile control (`u8` mode: 0 begin, 1 take, 2 end).
    /// Additive in protocol 3; it never changes published bytes.
    pub const PROFILE: u8 = 0x0b;
    /// Publish every tensor blob of a checkpoint and leave the group open.
    pub const PREPARE_CHECKPOINT: u8 = 0x0c;
    /// Publish the caller's metadata blob and the prepared state as one commit.
    pub const FINISH_CHECKPOINT: u8 = 0x0d;
    /// Discard an open prepared checkpoint without publishing.
    pub const ABORT_CHECKPOINT: u8 = 0x0e;
    pub const SHUTDOWN: u8 = 0x7f;
}

/// Response status bytes.
pub const STATUS_OK: u8 = 0;
pub const STATUS_ERROR: u8 = 1;

/// How the service is reached and what it will accept.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// An inherited socket file descriptor (preferred; no filesystem path).
    pub fd: Option<RawFd>,
    /// Alternatively, bind this unix socket path and accept one connection.
    pub socket: Option<PathBuf>,
    pub max_frame_bytes: usize,
    pub max_tensor_bytes: u64,
    /// How long a writable store handle may stay open **without a
    /// publication** before the service releases the exclusive writer lock.
    ///
    /// `0` releases it immediately after every publication, which is the
    /// pre-session behaviour: a caller cannot observe a held lock between
    /// saves. A positive value is a bounded ownership window, not an
    /// indefinite lock: the handle is dropped (and the lock released) on the
    /// first request that sees the window elapsed, and any publication that
    /// finds a stale fence fails closed and forces a reopen.
    pub session_idle_secs: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        ServiceConfig {
            fd: None,
            socket: None,
            max_frame_bytes: MAX_FRAME_BYTES,
            max_tensor_bytes: DEFAULT_MAX_TENSOR_BYTES,
            session_idle_secs: DEFAULT_SESSION_IDLE_SECS,
        }
    }
}

/// A decoded request.
struct Request {
    op: u8,
    id: u32,
    body: Vec<u8>,
    fds: Vec<OwnedFd>,
}

/// A frame that could not be decoded.
enum FrameError {
    /// The stream ended before a frame (clean client close).
    Eof,
    /// The frame is structurally invalid; the stream cannot be resynchronised.
    Malformed(String),
    Io(Error),
}

/// Run the service until the client shuts it down or closes the socket.
pub fn serve(
    store_path: &Path,
    options: Options,
    kind: BackendKind,
    config: ServiceConfig,
) -> Result<()> {
    if config.fd.is_none() == config.socket.is_none() {
        return Err(Error::invalid_argument(
            "checkpoint-service needs exactly one of --fd N or --socket PATH",
        ));
    }
    if config.max_frame_bytes == 0 || config.max_frame_bytes > MAX_FRAME_BYTES {
        return Err(Error::invalid_argument(format!(
            "--max-frame-bytes must be in 1..={MAX_FRAME_BYTES}"
        )));
    }
    if config.max_tensor_bytes == 0 {
        return Err(Error::invalid_argument(
            "--max-tensor-bytes must be a positive byte count",
        ));
    }
    let store = Store::open_with_options_and_backend(
        store_path,
        OpenMode::ReadOnly,
        options.clone(),
        kind.clone(),
    )?;
    let stream = match (config.fd, config.socket.clone()) {
        (Some(fd), _) => {
            if fd < 0 {
                return Err(Error::invalid_argument("--fd must be non-negative"));
            }
            // SAFETY: the caller declares this descriptor is a connected unix
            // stream it handed to us; ownership moves into `UnixStream`.
            ancillary::unix_stream_from_fd(fd)
                .map_err(|err| Error::invalid_argument(format!("--fd {fd}: {err}")))?
        }
        (None, Some(path)) => {
            let listener = UnixListener::bind(&path).map_err(|err| {
                Error::io(format!(
                    "checkpoint-service socket {}: {err}",
                    path.display()
                ))
            })?;
            // The protocol is binary, so stdout is free for readiness.
            println!("ready");
            let _ = std::io::stdout().flush();
            let (stream, _addr) = listener
                .accept()
                .map_err(|err| Error::io(format!("checkpoint-service accept: {err}")))?;
            let _ = std::fs::remove_file(&path);
            stream
        }
        (None, None) => unreachable!("validated above"),
    };
    let _ = stream.set_nonblocking(false);
    let mut service = Service {
        store,
        store_path: store_path.to_path_buf(),
        options,
        kind,
        config,
        manifest_cache: HashMap::new(),
        requests: 0,
        last_write: Instant::now(),
        deferred: None,
        write_handle_opens: 0,
        write_handle_releases: 0,
    };
    service.run(stream)
}

struct Service {
    /// The one handle this connection uses.
    ///
    /// It starts read-only. The first publication **upgrades** it to a writable
    /// handle and keeps it, so a session pays one open instead of four per
    /// save and every read through it is current (`group` updates the handle's
    /// state and head in place). [`Service::release_idle_write_handle`] drops
    /// it back to read-only after the configured idle window, which is the
    /// only thing that keeps the exclusive writer lock bounded.
    store: Store,
    store_path: PathBuf,
    options: Options,
    kind: BackendKind,
    config: ServiceConfig,
    /// Manifest cache keyed by checkpoint name. Entries survive a writable
    /// handle because manifests are immutable content-addressed blobs; a write
    /// that replaces a name clears the cache.
    manifest_cache: HashMap<String, CheckpointManifest>,
    requests: u64,
    /// When this connection last published through the writable handle.
    last_write: Instant,
    /// Name of the checkpoint whose group is open and unpublished.
    deferred: Option<String>,
    /// Number of times the writable handle was upgraded, for diagnostics.
    write_handle_opens: u64,
    /// Number of times an idle window released the writer lock.
    write_handle_releases: u64,
}

impl Service {
    /// Upgrade to (or reuse) the writable handle.
    ///
    /// A writable open re-proves the plane capabilities and takes the writer
    /// lock, so an upgraded handle is exactly as validated as a per-request
    /// one. There is no path that reuses a handle across a fence change: the
    /// writer lock (local/object) or the fenced lease record (S3/GCS/Azure) is
    /// held by this process for as long as the handle exists, and a
    /// publication whose lease expired fails closed.
    fn write_handle(&mut self) -> Result<&mut Store> {
        if self.store.mode() != OpenMode::ReadWrite {
            let store = Store::open_with_options_and_backend(
                &self.store_path,
                OpenMode::ReadWrite,
                self.options.clone(),
                self.kind.clone(),
            )?;
            self.store = store;
            self.manifest_cache.clear();
            self.write_handle_opens += 1;
            self.last_write = Instant::now();
        }
        Ok(&mut self.store)
    }

    /// Release the exclusive writer lock once the idle window has elapsed.
    ///
    /// With `session_idle_secs == 0` this runs after every publication, which
    /// is the historical behaviour. A failed downgrade keeps the writable
    /// handle (it is still a valid handle) and is retried on the next request.
    fn release_idle_write_handle(&mut self) {
        if self.store.mode() != OpenMode::ReadWrite {
            return;
        }
        // An open deferred group holds buffered ops for this handle; releasing
        // it would discard an unpublished checkpoint.
        if self.deferred.is_some() {
            return;
        }
        let window = Duration::from_secs(self.config.session_idle_secs);
        if window > Duration::ZERO && self.last_write.elapsed() < window {
            return;
        }
        match Store::open_with_options_and_backend(
            &self.store_path,
            OpenMode::ReadOnly,
            self.options.clone(),
            self.kind.clone(),
        ) {
            Ok(store) => {
                self.store = store;
                self.manifest_cache.clear();
                self.write_handle_releases += 1;
            }
            Err(_) => {
                // The writable handle is still valid; keep it and retry later.
            }
        }
    }

    /// Release the writable handle immediately after a publication when the
    /// session window is zero. Called at the end of every mutating handler.
    fn after_publication(&mut self) {
        self.last_write = Instant::now();
        if self.config.session_idle_secs == 0 {
            self.release_idle_write_handle();
        }
    }
    fn run(&mut self, mut stream: UnixStream) -> Result<()> {
        let max_frame = self.config.max_frame_bytes;
        loop {
            let request = match recv_request(&stream, max_frame) {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(()),
                Err(FrameError::Eof) => return Ok(()),
                Err(FrameError::Malformed(message)) => {
                    // Best effort: a typed error frame so the client sees why,
                    // then a non-zero exit. The stream is not trusted again.
                    let body = encode_error_body(Class::Malformed, &message);
                    let frame = encode_frame(op::INFO, 0, STATUS_ERROR, &body);
                    let _ = send_frame(&mut stream, &frame, None);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Err(Error::malformed(format!(
                        "checkpoint-service framing: {message}"
                    )));
                }
                Err(FrameError::Io(err)) => return Err(err),
            };
            self.requests += 1;
            // The only automatic release path: an idle window, never a timer
            // thread, so the decision is made on a client request and a
            // service with no client makes no further writes.
            self.release_idle_write_handle();
            let started = Instant::now();
            let response = self.dispatch(&request, started);
            match response {
                Dispatch::Shutdown => {
                    let body = Vec::new();
                    let frame = encode_frame(request.op, request.id, STATUS_OK, &body);
                    let _ = send_frame(&mut stream, &frame, None);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Ok(());
                }
                Dispatch::Reply { op, body, fd } => {
                    let frame = encode_frame(op, request.id, STATUS_OK, &body);
                    if frame.len() > self.config.max_frame_bytes {
                        let body = encode_error_body(
                            Class::LimitExceeded,
                            "response frame exceeds --max-frame-bytes",
                        );
                        let frame = encode_frame(op, request.id, STATUS_ERROR, &body);
                        send_frame(&mut stream, &frame, None)?;
                    } else {
                        send_frame(&mut stream, &frame, fd.as_ref().map(|fd| fd.as_raw_fd()))?;
                    }
                }
                Dispatch::Failed(err) => {
                    let body = encode_error_body(err.class(), err.message());
                    let frame = encode_frame(request.op, request.id, STATUS_ERROR, &body);
                    if frame.len() > self.config.max_frame_bytes {
                        let body = encode_error_body(
                            Class::LimitExceeded,
                            "error frame exceeds --max-frame-bytes",
                        );
                        let frame = encode_frame(request.op, request.id, STATUS_ERROR, &body);
                        send_frame(&mut stream, &frame, None)?;
                    } else {
                        send_frame(&mut stream, &frame, None)?;
                    }
                }
                Dispatch::Fatal(err) => {
                    // A protocol violation: answer with the typed class, then
                    // terminate. The stream is never trusted again.
                    let body = encode_error_body(err.class(), err.message());
                    let frame = encode_frame(request.op, request.id, STATUS_ERROR, &body);
                    let _ = send_frame(&mut stream, &frame, None);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Err(err);
                }
            }
        }
    }

    fn dispatch(&mut self, request: &Request, started: Instant) -> Dispatch {
        let outcome = match request.op {
            op::INFO => self.handle_info(request, started),
            op::LIST_CHECKPOINTS => self.handle_list(request),
            op::CHECKPOINT_MANIFEST => self.handle_manifest(request),
            op::RESTORE_TENSOR => self.handle_restore(request, started),
            op::WRITE_CHECKPOINT => self.handle_write(request, started),
            op::VERIFY_CHECKPOINT => self.handle_verify(request),
            op::REFRESH => self.handle_refresh(request, started),
            op::WRITE_FILE => self.handle_write_file(request, started),
            op::STAT_FILE => self.handle_stat_file(request),
            op::READ_FILE => self.handle_read_file(request),
            op::PROFILE => self.handle_profile(request),
            op::PREPARE_CHECKPOINT => self.handle_prepare(request, started),
            op::FINISH_CHECKPOINT => self.handle_finish(request, started),
            op::ABORT_CHECKPOINT => self.handle_abort(request, started),
            op::SHUTDOWN => {
                if !request.body.is_empty() || !request.fds.is_empty() {
                    return Dispatch::Failed(Error::malformed(
                        "SHUTDOWN takes an empty body and no descriptors",
                    ));
                }
                // An open deferred checkpoint is discarded, never published by
                // a shutdown.
                self.store.abort_deferred_checkpoint();
                self.deferred = None;
                return Dispatch::Shutdown;
            }
            other => {
                // Unknown opcodes are a protocol violation: terminate rather
                // than keep serving a client whose code is not this protocol.
                Dispatch::Fatal(Error::malformed(format!("unknown opcode 0x{other:02x}")))
            }
        };
        outcome
    }

    fn handle_info(&mut self, request: &Request, started: Instant) -> Dispatch {
        if let Err(err) = require_empty(request) {
            return Dispatch::Failed(err);
        }
        let mut body = Vec::new();
        put_u32(&mut body, PROTOCOL_VERSION);
        put_u32(&mut body, self.config.max_frame_bytes as u32);
        put_u64(&mut body, self.config.max_tensor_bytes);
        put_u64(&mut body, self.store.generation());
        put_hash(&mut body, self.store.head());
        put_str(&mut body, &self.store_path.display().to_string());
        put_u64(&mut body, elapsed_us(started));
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn handle_list(&mut self, request: &Request) -> Dispatch {
        if let Err(err) = require_empty(request) {
            return Dispatch::Failed(err);
        }
        match self.store.list_checkpoints() {
            Ok(names) => {
                let mut body = Vec::new();
                put_u32(&mut body, names.len() as u32);
                for name in names {
                    put_str(&mut body, &name);
                }
                Dispatch::Reply {
                    op: request.op,
                    body,
                    fd: None,
                }
            }
            Err(err) => Dispatch::Failed(err),
        }
    }

    fn manifest(&mut self, name: &str) -> Result<CheckpointManifest> {
        if let Some(cached) = self.manifest_cache.get(name) {
            return Ok(cached.clone());
        }
        let manifest = self.store.checkpoint_manifest(name)?;
        if self.manifest_cache.len() >= 8 {
            let victim = self
                .manifest_cache
                .keys()
                .next()
                .cloned()
                .expect("cache is non-empty");
            self.manifest_cache.remove(&victim);
        }
        self.manifest_cache
            .insert(name.to_owned(), manifest.clone());
        Ok(manifest)
    }

    fn handle_manifest(&mut self, request: &Request) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let name = match cursor.string() {
            Ok(name) => name,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        match self
            .manifest(&name)
            .and_then(|manifest| encode_manifest(&manifest))
        {
            Ok(body) => Dispatch::Reply {
                op: request.op,
                body,
                fd: None,
            },
            Err(err) => Dispatch::Failed(err),
        }
    }

    fn handle_restore(&mut self, request: &Request, started: Instant) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let parsed = (|| -> Result<(String, String, Option<Region>, Option<RestoreTopology>)> {
            let name = cursor.string()?;
            let tensor = cursor.string()?;
            let region = if cursor.u8()? == 1 {
                let mut start = Vec::new();
                let mut end = Vec::new();
                let rank = cursor.u8()? as usize;
                for _ in 0..rank {
                    start.push(cursor.u64()?);
                }
                for _ in 0..rank {
                    end.push(cursor.u64()?);
                }
                Some(Region::new(start, end))
            } else {
                None
            };
            let topology = if cursor.u8()? == 1 {
                let shards = cursor.u32()?;
                let axis = cursor.u32()?;
                Some(RestoreTopology::Sharded { axis, shards })
            } else {
                None
            };
            cursor.finish()?;
            Ok((name, tensor, region, topology))
        })();
        let (name, tensor, region, topology) = match parsed {
            Ok(parsed) => parsed,
            Err(err) => return Dispatch::Failed(err),
        };
        if request.fds.len() != 1 {
            return Dispatch::Failed(Error::invalid_argument(
                "RESTORE_TENSOR needs exactly one caller-supplied memfd",
            ));
        }
        let fd = &request.fds[0];
        let target_bytes =
            match self.expected_payload(&name, &tensor, region.as_ref(), topology.as_ref()) {
                Ok(bytes) => bytes,
                Err(err) => return Dispatch::Failed(err),
            };
        let before = self.store.backend_stats();
        if target_bytes > self.config.max_tensor_bytes {
            return Dispatch::Failed(Error::limit_exceeded(format!(
                "restore of {} bytes exceeds --max-tensor-bytes ({})",
                target_bytes, self.config.max_tensor_bytes
            )));
        }
        match fd_size(fd) {
            Ok(size) if size >= target_bytes => {}
            Ok(size) => {
                return Dispatch::Failed(Error::invalid_argument(format!(
                    "caller buffer is {size} bytes; the restore needs {target_bytes}"
                )))
            }
            Err(err) => return Dispatch::Failed(err),
        }
        let outcome = match self.store.restore_named_tensor_at(
            None,
            &name,
            &tensor,
            region.as_ref(),
            topology.as_ref(),
        ) {
            Ok(outcome) => outcome,
            Err(err) => return Dispatch::Failed(err),
        };
        let after = self.store.backend_stats();
        if outcome.bytes.len() as u64 != target_bytes {
            return Dispatch::Failed(Error::corrupt(format!(
                "restore produced {} bytes, expected {target_bytes}",
                outcome.bytes.len()
            )));
        }
        if let Err(err) = write_all_at(fd, &outcome.bytes) {
            return Dispatch::Failed(err);
        }
        let mut body = Vec::new();
        let report = &outcome.report;
        put_str(&mut body, &report.dtype);
        put_u32(&mut body, report.shape.len() as u32);
        for dim in &report.shape {
            put_u64(&mut body, *dim);
        }
        put_u64(&mut body, report.bytes);
        put_hash(&mut body, report.restored_content_root);
        put_hash(&mut body, report.full_logical_root);
        put_u8(&mut body, u8::from(report.full_root_verified));
        put_u8(&mut body, u8::from(report.shard_roots_verified));
        match report.topology_root {
            Some(root) => {
                put_u8(&mut body, 1);
                put_hash(&mut body, root);
            }
            None => put_u8(&mut body, 0),
        }
        put_u32(&mut body, report.shards_read as u32);
        put_u64(&mut body, report.units_touched);
        put_u64(
            &mut body,
            after.bytes_read.saturating_sub(before.bytes_read),
        );
        put_u64(
            &mut body,
            after.reads.saturating_sub(before.reads)
                + after.range_reads.saturating_sub(before.range_reads),
        );
        put_u64(&mut body, elapsed_us(started));
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn expected_payload(
        &mut self,
        name: &str,
        tensor: &str,
        region: Option<&Region>,
        topology: Option<&RestoreTopology>,
    ) -> Result<u64> {
        if region.is_some() && topology.is_some() {
            return Err(Error::invalid_argument(
                "a region restore cannot be verified against a full-tensor topology",
            ));
        }
        let manifest = self.manifest(name)?;
        let entry = manifest.tensor(tensor)?;
        let dtype = crate::representation::TensorElementDtype::parse(&entry.dtype)?;
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!(
                "tensor `{tensor}` dtype `{}` has no physical layout",
                entry.dtype
            ))
        })?;
        let shape = match region {
            Some(region) => {
                region.validate(&entry.shape)?;
                region.extent()
            }
            None => entry.shape.clone(),
        };
        let elements = shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .ok_or_else(|| Error::limit_exceeded("tensor shape overflow"))?;
        weftcodec::tensor::payload_len(elements, physical).map_err(Error::from)
    }

    fn handle_write(&mut self, request: &Request, started: Instant) -> Dispatch {
        if self.deferred.is_some() {
            return Dispatch::Failed(Error::invalid_argument(
                "a deferred checkpoint is open on this connection; finish or abort it first",
            ));
        }
        self.write_checkpoint_request(request, started, false)
    }

    /// `PREPARE_CHECKPOINT`: publish every blob and leave the group open.
    fn handle_prepare(&mut self, request: &Request, started: Instant) -> Dispatch {
        if self.deferred.is_some() {
            return Dispatch::Failed(Error::invalid_argument(
                "a deferred checkpoint is already open on this connection",
            ));
        }
        self.write_checkpoint_request(request, started, true)
    }

    /// `FINISH_CHECKPOINT`: add one metadata file and publish exactly one commit.
    fn handle_finish(&mut self, request: &Request, started: Instant) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let parsed = (|| -> Result<(String, String)> {
            let name = cursor.string()?;
            let path = cursor.string()?;
            cursor.finish()?;
            Ok((name, path))
        })();
        let (name, path) = match parsed {
            Ok(parsed) => parsed,
            Err(err) => return Dispatch::Failed(err),
        };
        let open = match self.deferred.as_deref() {
            Some(open) => open.to_owned(),
            None => {
                return Dispatch::Failed(Error::invalid_argument(
                    "FINISH_CHECKPOINT with no prepared checkpoint",
                ))
            }
        };
        if open != name {
            return Dispatch::Failed(Error::invalid_argument(format!(
                "prepared checkpoint is `{open}`, not `{name}`"
            )));
        }
        let prefix = format!("/checkpoints/{name}/");
        if !path.starts_with(&prefix) {
            return Dispatch::Failed(Error::invalid_argument(format!(
                "metadata path `{path}` is not under `{prefix}`"
            )));
        }
        if request.fds.len() != 1 {
            return Dispatch::Failed(Error::invalid_argument(
                "FINISH_CHECKPOINT needs exactly one caller-supplied memfd",
            ));
        }
        let fd = &request.fds[0];
        let size = match fd_size(fd) {
            Ok(size) => size,
            Err(err) => return Dispatch::Failed(err),
        };
        if size > MAX_FILE_BYTES {
            return Dispatch::Failed(Error::limit_exceeded(format!(
                "metadata payload is {size} bytes, above the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let mut bytes = vec![0u8; size as usize];
        let mut read = 0usize;
        while read < bytes.len() {
            match ancillary::read_at(fd.as_fd(), &mut bytes[read..], read as u64) {
                Ok(0) => break,
                Ok(count) => read += count,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    return Dispatch::Failed(Error::io(format!(
                        "checkpoint-service read of caller buffer: {err}"
                    )))
                }
            }
        }
        if read != bytes.len() {
            return Dispatch::Failed(Error::malformed(format!(
                "caller buffer declared {size} bytes but only {read} were readable"
            )));
        }
        self.deferred = None;
        let head = {
            let _commit = crate::phase_profile::scope(crate::phase_profile::Phase::MetadataCommit);
            match self
                .store
                .commit_deferred_checkpoint_metadata(&path, &bytes)
            {
                Ok(head) => head,
                Err(err) => return Dispatch::Failed(err),
            }
        };
        self.manifest_cache.clear();
        self.after_publication();
        let mut body = Vec::new();
        put_hash(&mut body, head);
        put_u64(&mut body, self.store.generation());
        put_u64(&mut body, size);
        put_u64(&mut body, elapsed_us(started));
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    /// `ABORT_CHECKPOINT`: discard an open group; nothing is published.
    fn handle_abort(&mut self, request: &Request, started: Instant) -> Dispatch {
        if let Err(err) = require_empty(request) {
            return Dispatch::Failed(err);
        }
        self.store.abort_deferred_checkpoint();
        self.deferred = None;
        let mut body = Vec::new();
        put_u64(&mut body, self.store.generation());
        put_u64(&mut body, elapsed_us(started));
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn write_checkpoint_request(
        &mut self,
        request: &Request,
        started: Instant,
        defer: bool,
    ) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let parsed = (|| -> Result<(String, CheckpointWriteOptions)> {
            let name = cursor.string()?;
            let shards = cursor.u32()?;
            let axis = cursor.u32()?;
            let strategy = ShardStrategy::parse(&cursor.string()?)?;
            let rank = cursor.u32()? as usize;
            let mut tile_shape = Vec::new();
            for _ in 0..rank {
                tile_shape.push(cursor.u64()?);
            }
            // Protocol v2 appends the critical-path write policy. A v1 body
            // (no trailing bytes) keeps the historical planned behaviour.
            let mut write_policy = crate::tensor_store::WritePolicy::Planned;
            let mut fast_codec = crate::tensor_store::FastCodec::Zstd;
            let mut fast_zstd_level = 1i32;
            let mut on_critical_path = false;
            let mut deadline_ms = None;
            if cursor.remaining() >= 8 {
                let policy = cursor.u8()?;
                let codec = cursor.u8()?;
                fast_zstd_level = cursor.u32()? as i32;
                let flags = cursor.u8()?;
                on_critical_path = flags & 1 != 0;
                let has_deadline = cursor.u8()?;
                if has_deadline == 1 {
                    deadline_ms = Some(cursor.u64()?);
                }
                fast_codec = match codec {
                    0 => crate::tensor_store::FastCodec::Raw,
                    1 => crate::tensor_store::FastCodec::Zstd,
                    other => return Err(Error::malformed(format!("unknown fast codec {other}"))),
                };
                write_policy = match policy {
                    0 => crate::tensor_store::WritePolicy::Planned,
                    1 => crate::tensor_store::WritePolicy::Fast,
                    2 => crate::tensor_store::WritePolicy::Auto {
                        on_critical_path,
                        deadline_ms,
                    },
                    other => return Err(Error::malformed(format!("unknown write policy {other}"))),
                };
            }
            cursor.finish()?;
            Ok((
                name,
                CheckpointWriteOptions {
                    shards,
                    axis,
                    filters: Vec::new(),
                    strategy,
                    tile_shape: if tile_shape.is_empty() {
                        None
                    } else {
                        Some(tile_shape)
                    },
                    write_policy,
                    fast_codec,
                    fast_zstd_level,
                    on_critical_path,
                    deadline_ms,
                    defer_commit: defer,
                    ..CheckpointWriteOptions::default()
                },
            ))
        })();
        let (name, options) = match parsed {
            Ok(parsed) => parsed,
            Err(err) => return Dispatch::Failed(err),
        };
        if request.fds.len() != 1 {
            return Dispatch::Failed(Error::invalid_argument(
                "WRITE_CHECKPOINT needs exactly one caller-supplied memfd holding the container",
            ));
        }
        let fd = &request.fds[0];
        // Reuse the exact shipped safetensors parser as a preflight bound so a
        // single huge tensor is refused before anything is published.
        let proc_path = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));
        let file = match SafetensorsFile::parse(&proc_path) {
            Ok(file) => file,
            Err(err) => return Dispatch::Failed(err),
        };
        for tensor in &file.tensors {
            let bytes = tensor.byte_len();
            if bytes > self.config.max_tensor_bytes {
                return Dispatch::Failed(Error::limit_exceeded(format!(
                    "tensor `{}` is {bytes} bytes, above --max-tensor-bytes ({})",
                    tensor.name, self.config.max_tensor_bytes
                )));
            }
        }
        drop(file);
        // The session's writable handle is reused when it is already open. A
        // `Busy`/fence refusal is not retried here: the caller sees the typed
        // error and must reopen (a stale fence is never silently replaced).
        let outcome =
            (|| -> Result<(crate::checkpoint::CheckpointWriteReport, CheckpointManifest)> {
                let write_store = self.write_handle()?;
                let report = write_store.write_checkpoint(&name, &proc_path, &options)?;
                let manifest = write_store.checkpoint_manifest(&report.name)?;
                Ok((report, manifest))
            })();
        let (report, manifest) = match outcome {
            Ok(outcome) => outcome,
            Err(err) => return Dispatch::Failed(err),
        };
        if defer {
            self.deferred = Some(report.name.clone());
        }
        // The checkpoint this handle just published is visible to every later
        // read through the same handle; only a replaced name needs its cached
        // manifest dropped.
        self.manifest_cache.clear();
        if !defer {
            self.after_publication();
        }
        let mut body = Vec::new();
        put_hash(&mut body, report.checkpoint_id);
        put_hash(&mut body, report.head);
        put_hash(&mut body, report.manifest_root);
        put_bytes32(&mut body, &report.source_sha256);
        put_u64(&mut body, report.source_file_bytes);
        put_u64(&mut body, report.total_element_bytes);
        put_u32(&mut body, report.tensors.len() as u32);
        put_u32(&mut body, report.shard_objects as u32);
        put_u8(&mut body, u8::from(report.complete));
        put_u64(&mut body, elapsed_us(started));
        match encode_manifest(&manifest) {
            Ok(encoded) => {
                put_u32(&mut body, encoded.len() as u32);
                body.extend_from_slice(&encoded);
            }
            Err(err) => return Dispatch::Failed(err),
        }
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    /// Publish one small file (DCP/Orbax metadata) at an existing store path.
    fn handle_write_file(&mut self, request: &Request, started: Instant) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let path = match cursor.string() {
            Ok(path) => path,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        if request.fds.len() != 1 {
            return Dispatch::Failed(Error::invalid_argument(
                "WRITE_FILE needs exactly one caller-supplied memfd",
            ));
        }
        let fd = &request.fds[0];
        let size = match fd_size(fd) {
            Ok(size) => size,
            Err(err) => return Dispatch::Failed(err),
        };
        if size > MAX_FILE_BYTES {
            return Dispatch::Failed(Error::limit_exceeded(format!(
                "WRITE_FILE payload is {size} bytes, above the {MAX_FILE_BYTES}-byte bound"
            )));
        }
        let mut bytes = vec![0u8; size as usize];
        let mut read = 0usize;
        while read < bytes.len() {
            match ancillary::read_at(fd.as_fd(), &mut bytes[read..], read as u64) {
                Ok(0) => break,
                Ok(count) => read += count,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    return Dispatch::Failed(Error::io(format!(
                        "checkpoint-service read of caller buffer: {err}"
                    )))
                }
            }
        }
        if read != bytes.len() {
            return Dispatch::Failed(Error::malformed(format!(
                "caller buffer declared {size} bytes but only {read} were readable"
            )));
        }
        let outcome = (|| -> Result<()> {
            let (parent, name) = split_store_path(&path)?;
            let write_store = self.write_handle()?;
            let parent_id = write_store.resolve_path(&parent)?;
            let (uid, gid) = (
                write_store.options().owner_uid,
                write_store.options().owner_gid,
            );
            {
                let _commit =
                    crate::phase_profile::scope(crate::phase_profile::Phase::MetadataCommit);
                write_store.create_file(
                    parent_id,
                    name.as_bytes(),
                    &bytes,
                    0o644,
                    uid,
                    gid,
                    std::collections::BTreeMap::new(),
                )?;
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {}
            Err(err) => return Dispatch::Failed(err),
        }
        self.manifest_cache.clear();
        self.after_publication();
        let mut body = Vec::new();
        put_u64(&mut body, size);
        put_u64(&mut body, self.store.generation());
        put_u64(&mut body, elapsed_us(started));
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn handle_read_file(&mut self, request: &Request) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let path = match cursor.string() {
            Ok(path) => path,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        if request.fds.len() != 1 {
            return Dispatch::Failed(Error::invalid_argument(
                "READ_FILE needs exactly one caller-supplied memfd",
            ));
        }
        let outcome = (|| -> Result<Vec<u8>> {
            let id = self.store.resolve_path(&path)?;
            let meta = self.store.object_meta(id)?;
            if meta.size > MAX_FILE_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "`{path}` is {} bytes, above the {MAX_FILE_BYTES}-byte read bound",
                    meta.size
                )));
            }
            self.store.read_object(id)
        })();
        let bytes = match outcome {
            Ok(bytes) => bytes,
            Err(err) => return Dispatch::Failed(err),
        };
        let fd = &request.fds[0];
        match fd_size(fd) {
            Ok(size) if size >= bytes.len() as u64 => {}
            Ok(size) => {
                return Dispatch::Failed(Error::invalid_argument(format!(
                    "caller buffer is {size} bytes; `{path}` needs {}",
                    bytes.len()
                )))
            }
            Err(err) => return Dispatch::Failed(err),
        }
        if let Err(err) = write_all_at(fd, &bytes) {
            return Dispatch::Failed(err);
        }
        let mut body = Vec::new();
        put_u64(&mut body, bytes.len() as u64);
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    /// Diagnostic phase-profile control.
    ///
    /// ``mode`` 0 clears the collector and enables profiling, 1 returns the
    /// totals collected so far without clearing them, and 2 stops profiling.
    /// The response is a bounded list of `(name, count, total_ns, max_ns)`; it
    /// is a measurement aid and never affects published bytes.
    fn handle_profile(&mut self, request: &Request) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let mode = match cursor.u8() {
            Ok(mode) => mode,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        let snapshot = match mode {
            0 => {
                crate::phase_profile::begin();
                crate::phase_profile::PhaseSnapshot::default()
            }
            1 => crate::phase_profile::snapshot(),
            2 => crate::phase_profile::finish(),
            other => {
                return Dispatch::Failed(Error::invalid_argument(format!(
                    "PROFILE mode {other} is not 0, 1, or 2"
                )))
            }
        };
        let mut body = Vec::new();
        let present: Vec<crate::phase_profile::Phase> = crate::phase_profile::Phase::ALL
            .iter()
            .copied()
            .filter(|phase| snapshot.phases.contains_key(phase.as_str()))
            .collect();
        put_u32(&mut body, present.len() as u32);
        for phase in present {
            let totals = &snapshot.phases[phase.as_str()];
            put_str(&mut body, phase.as_str());
            put_u64(&mut body, totals.count);
            put_u64(&mut body, totals.total_ns.min(u128::from(u64::MAX)) as u64);
            put_u64(&mut body, totals.max_ns);
        }
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn handle_stat_file(&mut self, request: &Request) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let path = match cursor.string() {
            Ok(path) => path,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        let mut body = Vec::new();
        match self.store.resolve_path(&path) {
            Ok(id) => match self.store.object_meta(id) {
                Ok(meta) => {
                    put_u8(&mut body, 1);
                    put_u64(&mut body, meta.size);
                }
                Err(err) => return Dispatch::Failed(err),
            },
            Err(err) if err.class() == Class::NotFound => {
                put_u8(&mut body, 0);
                put_u64(&mut body, 0);
            }
            Err(err) => return Dispatch::Failed(err),
        }
        Dispatch::Reply {
            op: request.op,
            body,
            fd: None,
        }
    }

    fn handle_refresh(&mut self, request: &Request, started: Instant) -> Dispatch {
        if let Err(err) = require_empty(request) {
            return Dispatch::Failed(err);
        }
        // A session that holds the writer lock cannot observe another writer's
        // commit (none can publish while the lock is held), and reopening here
        // would silently drop that lock. Report the current head instead.
        if self.store.mode() == OpenMode::ReadWrite {
            let mut body = Vec::new();
            put_u64(&mut body, self.store.generation());
            put_hash(&mut body, self.store.head());
            put_u64(&mut body, elapsed_us(started));
            return Dispatch::Reply {
                op: request.op,
                body,
                fd: None,
            };
        }
        match Store::open_with_options_and_backend(
            &self.store_path,
            OpenMode::ReadOnly,
            self.options.clone(),
            self.kind.clone(),
        ) {
            Ok(store) => {
                self.store = store;
                self.manifest_cache.clear();
                let mut body = Vec::new();
                put_u64(&mut body, self.store.generation());
                put_hash(&mut body, self.store.head());
                put_u64(&mut body, elapsed_us(started));
                Dispatch::Reply {
                    op: request.op,
                    body,
                    fd: None,
                }
            }
            Err(err) => Dispatch::Failed(err),
        }
    }

    fn handle_verify(&mut self, request: &Request) -> Dispatch {
        let mut cursor = Cursor::new(&request.body);
        let name = match cursor.string() {
            Ok(name) => name,
            Err(err) => return Dispatch::Failed(err),
        };
        if let Err(err) = cursor.finish() {
            return Dispatch::Failed(err);
        }
        match crate::checkpoint::verify_checkpoint(&self.store, &name) {
            Ok(count) => {
                let mut body = Vec::new();
                put_u32(&mut body, count as u32);
                Dispatch::Reply {
                    op: request.op,
                    body,
                    fd: None,
                }
            }
            Err(err) => Dispatch::Failed(err),
        }
    }
}

enum Dispatch {
    Reply {
        op: u8,
        body: Vec<u8>,
        fd: Option<OwnedFd>,
    },
    Failed(Error),
    /// A protocol violation: reply once, then exit non-zero.
    Fatal(Error),
    Shutdown,
}

// ---------------------------------------------------------------------------
// Frame encoding

fn encode_frame(op: u8, id: u32, status: u8, body: &[u8]) -> Vec<u8> {
    let len = 1 + 4 + 1 + body.len();
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(op);
    out.extend_from_slice(&id.to_le_bytes());
    out.push(status);
    out.extend_from_slice(body);
    out
}

fn encode_error_body(class: Class, message: &str) -> Vec<u8> {
    let mut body = Vec::new();
    put_str(&mut body, class.as_str());
    put_str(&mut body, message);
    body
}

fn encode_manifest(manifest: &CheckpointManifest) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_hash(&mut out, manifest.root()?);
    put_str(&mut out, &manifest.name);
    put_str(&mut out, &manifest.container);
    put_u8(&mut out, u8::from(manifest.complete));
    put_u64(&mut out, manifest.element_bytes);
    match manifest.source_file_bytes {
        Some(bytes) => {
            put_u8(&mut out, 1);
            put_u64(&mut out, bytes);
        }
        None => put_u8(&mut out, 0),
    }
    match manifest.source_sha256 {
        Some(digest) => {
            put_u8(&mut out, 1);
            put_bytes32(&mut out, &digest);
        }
        None => put_u8(&mut out, 0),
    }
    put_u32(&mut out, manifest.tensors.len() as u32);
    for tensor in &manifest.tensors {
        put_str(&mut out, &tensor.name);
        put_str(&mut out, &tensor.dtype);
        put_u32(&mut out, tensor.shape.len() as u32);
        for dim in &tensor.shape {
            put_u64(&mut out, *dim);
        }
        put_hash(&mut out, tensor.logical_root);
        put_hash(&mut out, tensor.content_root);
        put_u64(&mut out, tensor.element_bytes);
        put_u64(&mut out, tensor.source_offset);
        put_u32(&mut out, tensor.shards.len() as u32);
        for shard in &tensor.shards {
            put_u32(&mut out, shard.axis);
            put_u32(&mut out, shard.shards);
            put_u32(&mut out, shard.index);
            put_str(&mut out, &shard.object_name);
            put_hash(&mut out, shard.object_id);
            put_hash(&mut out, shard.content_root);
            put_hash(&mut out, shard.logical_root);
            put_u64(&mut out, shard.byte_len);
        }
    }
    if out.len() > MAX_FRAME_BYTES {
        return Err(Error::limit_exceeded(format!(
            "manifest encoding is {} bytes, above the {MAX_FRAME_BYTES}-byte frame bound",
            out.len()
        )));
    }
    Ok(out)
}

fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes32(out: &mut Vec<u8>, value: &[u8; 32]) {
    out.extend_from_slice(value);
}

fn put_hash(out: &mut Vec<u8>, value: Hash32) {
    out.extend_from_slice(value.as_bytes());
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros() as u64
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, at: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.at + len > self.bytes.len() {
            return Err(Error::malformed("truncated request body"));
        }
        let slice = &self.bytes[self.at..self.at + len];
        self.at += len;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(buf))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::malformed("request string is not UTF-8"))
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn finish(&self) -> Result<()> {
        if self.at != self.bytes.len() {
            return Err(Error::malformed(format!(
                "request has {} trailing bytes",
                self.bytes.len() - self.at
            )));
        }
        Ok(())
    }
}

/// Split an absolute store path into `(parent, name)`; the parent root is `/`.
fn split_store_path(path: &str) -> Result<(String, String)> {
    if !path.starts_with('/') {
        return Err(Error::invalid_argument(format!(
            "WRITE_FILE path must be absolute (got `{path}`)"
        )));
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(Error::invalid_argument("WRITE_FILE needs a file name"));
    }
    match trimmed.rfind('/') {
        Some(0) => Ok(("/".to_owned(), trimmed[1..].to_owned())),
        Some(at) => Ok((trimmed[..at].to_owned(), trimmed[at + 1..].to_owned())),
        None => Ok(("/".to_owned(), trimmed.to_owned())),
    }
}

fn require_empty(request: &Request) -> Result<()> {
    if !request.body.is_empty() {
        return Err(Error::malformed("this request takes an empty body"));
    }
    if !request.fds.is_empty() {
        return Err(Error::malformed("this request takes no descriptors"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Socket I/O

fn recv_request(
    stream: &UnixStream,
    max_frame: usize,
) -> std::result::Result<Option<Request>, FrameError> {
    let mut fds = Vec::new();
    let mut header = [0u8; 4];
    match recv_exact(stream, &mut header, &mut fds) {
        Ok(true) => {}
        Ok(false) => {
            for fd in fds {
                drop(fd);
            }
            return Err(FrameError::Eof);
        }
        Err(err) => return Err(FrameError::Io(err)),
    }
    let len = u32::from_le_bytes(header) as usize;
    if len < 6 {
        return Err(FrameError::Malformed(format!(
            "frame length {len} is below the 6-byte minimum"
        )));
    }
    if len > max_frame {
        return Err(FrameError::Malformed(format!(
            "frame length {len} exceeds the {max_frame}-byte bound"
        )));
    }
    let mut payload = vec![0u8; len];
    match recv_exact(stream, &mut payload, &mut fds) {
        Ok(true) => {}
        Ok(false) => {
            return Err(FrameError::Malformed(
                "stream ended inside a frame".to_owned(),
            ))
        }
        Err(err) => return Err(FrameError::Io(err)),
    }
    if fds.len() > 1 {
        return Err(FrameError::Malformed(format!(
            "frame carried {} descriptors; at most 1 is accepted",
            fds.len()
        )));
    }
    let op = payload[0];
    let id = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
    let status = payload[5];
    if status != 0 {
        return Err(FrameError::Malformed(format!(
            "request frame has status {status}; only 0 is valid"
        )));
    }
    Ok(Some(Request {
        op,
        id,
        body: payload[6..].to_vec(),
        fds,
    }))
}

/// Read exactly `buf.len()` bytes. Returns `false` on a clean EOF before any
/// byte of this call. Ancillary descriptors are collected on the first read.
fn recv_exact(
    stream: &UnixStream,
    buf: &mut [u8],
    fds: &mut Vec<OwnedFd>,
) -> std::result::Result<bool, Error> {
    let mut filled = 0usize;
    let mut first = true;
    while filled < buf.len() {
        let got = match ancillary::recv_with_fds(stream.as_raw_fd(), &mut buf[filled..], first) {
            Ok(received) => {
                fds.extend(received.fds);
                received.bytes
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(Error::io(format!("checkpoint-service recv: {err}")));
            }
        };
        first = false;
        if got == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(Error::io("checkpoint-service: stream ended mid-frame"));
        }
        filled += got;
    }
    Ok(true)
}

fn send_frame(stream: &mut UnixStream, frame: &[u8], fd: Option<RawFd>) -> Result<()> {
    if frame.len() > MAX_FRAME_BYTES + 64 {
        return Err(Error::limit_exceeded("response frame is too large to send"));
    }
    let mut sent = 0usize;
    while sent < frame.len() {
        // A descriptor may only travel with the first byte window; a retry after
        // a partial write must not attach it twice.
        let pass = if sent == 0 { fd } else { None };
        match ancillary::send_with_fd(stream.as_raw_fd(), &frame[sent..], pass) {
            Ok(0) => {
                return Err(Error::io("checkpoint-service: socket write returned 0"));
            }
            Ok(count) => {
                sent += count;
                if sent < frame.len() && pass.is_some() {
                    return Err(Error::io(
                        "checkpoint-service: short sendmsg after passing a descriptor",
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(Error::io(format!("checkpoint-service send: {err}")));
            }
        }
    }
    Ok(())
}

fn fd_size(fd: &OwnedFd) -> Result<u64> {
    ancillary::size_of_fd(fd.as_fd())
        .map_err(|err| Error::io(format!("checkpoint-service fstat: {err}")))
}

/// Write the whole buffer at offset 0 of the descriptor, ignoring its file
/// offset so a shared mapping sees the bytes immediately.
fn write_all_at(fd: &OwnedFd, bytes: &[u8]) -> Result<()> {
    ancillary::write_all_at(fd.as_fd(), bytes)
        .map_err(|err| Error::io(format!("checkpoint-service write to caller buffer: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_layout() {
        let frame = encode_frame(op::INFO, 7, STATUS_OK, b"abc");
        assert_eq!(frame.len(), 4 + 6 + 3);
        assert_eq!(u32::from_le_bytes(frame[0..4].try_into().unwrap()), 9);
        assert_eq!(frame[4], op::INFO);
        assert_eq!(u32::from_le_bytes(frame[5..9].try_into().unwrap()), 7);
        assert_eq!(frame[9], STATUS_OK);
        assert_eq!(&frame[10..], b"abc");
    }

    #[test]
    fn cursor_rejects_trailing_and_truncated() {
        let mut cursor = Cursor::new(b"\x02\x00\x00\x00ab\xff");
        assert_eq!(cursor.string().unwrap(), "ab");
        assert!(cursor.finish().is_err());
        let mut cursor = Cursor::new(b"\x05\x00\x00\x00ab");
        assert!(cursor.string().is_err());
    }

    #[test]
    fn error_body_has_class_then_message() {
        let body = encode_error_body(Class::Corrupt, "bad");
        let mut cursor = Cursor::new(&body);
        assert_eq!(cursor.string().unwrap(), "Corrupt");
        assert_eq!(cursor.string().unwrap(), "bad");
        cursor.finish().unwrap();
    }
}
