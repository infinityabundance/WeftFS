//! Azure Blob backend court (subphase R21).
//!
//! Everything here runs against [`FakeAzure`], an in-process HTTP service that
//! speaks the subset of the Azure Blob REST API this backend uses: `Put Blob`
//! with `x-ms-blob-type: BlockBlob` and the two conditional forms, `Get Blob`
//! (whole and `Range`), `Get Blob Properties`, `Delete Blob`, `List Blobs`,
//! and container create/properties.
//!
//! The fake is a test double, not an Azurite stand-in. It exists so the
//! backend's request shapes, status mapping, credential signing, and — most
//! importantly — its handling of `If-None-Match`/`If-Match` are exercised
//! deterministically without a network. `bench/scripts/azure_court.sh` runs the
//! same backend against a real Azurite release and records
//! `emulator_unavailable` with the exact command when it cannot.
//!
//! The fake **independently recomputes the SharedKey signature** from the
//! headers and URL it received, using its own HMAC-SHA256 implementation. A
//! wrong canonicalisation, a header the client signed but did not send, or a
//! header it sent but did not sign all show up as `403 AuthenticationFailed`.
//! That is a genuine cross-check of the signing code, not a mirror of it.
//!
//! What the fake can prove: create-once conflict (`409`), matching-ETag success,
//! stale-ETag refusal (`412`), the quoted/dequoted `If-Match` negotiation,
//! ranged reads, missing blobs, transport errors preserving `Error::Io`, and
//! that a service which *accepts* a stale ETag is refused
//! `UnsupportedVersion` both by the probe and by `Store::init_with_backend`.
//!
//! What it cannot prove: anything about a real Azure deployment, media
//! durability, replication, power loss, or multi-node consistency.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use weftfs::azure::{AzureBackend, AzureConfig, IfMatchForm};
use weftfs::backend::{BackendKind, Key};
use weftfs::planes::{BlobPlane, CommitPlane};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

const ACCOUNT: &str = "devstoreaccount1";
const ACCOUNT_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

// ---------------------------------------------------------------------------
// A minimal in-process Azure Blob REST service
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Stored {
    bytes: Vec<u8>,
    etag: String,
}

#[derive(Default)]
struct State {
    /// container -> blob name -> bytes and ETag.
    containers: BTreeMap<String, BTreeMap<String, Stored>>,
    /// Every `Authorization` value and `x-ms-date` observed, so a test can
    /// prove the credential is actually sent and nothing else leaks it.
    authorizations: Vec<String>,
    next_etag: u64,
    /// When true, a non-zero `If-Match` is ignored: the fake accepts a stale
    /// ETag, which a correct backend must refuse.
    stale_accepting: bool,
    /// When true, `If-Match` is compared literally against the *unquoted*
    /// ETag value (Ceph RGW's behaviour in R16). The client must negotiate the
    /// dequoted form.
    literal_dequoted: bool,
    /// When true, every request must carry a valid SharedKey signature.
    require_signature: bool,
    /// Requests that failed signature validation.
    signature_failures: Vec<String>,
}

impl State {
    fn blob(&self, container: &str, name: &str) -> Option<&Stored> {
        self.containers.get(container).and_then(|map| map.get(name))
    }

    fn write(
        &mut self,
        container: &str,
        name: &str,
        bytes: Vec<u8>,
        if_none_match_star: bool,
        if_match: Option<&str>,
    ) -> Result<String, u16> {
        let current = self.blob(container, name).cloned();
        if if_none_match_star && current.is_some() {
            return Err(409);
        }
        if let Some(wanted) = if_match {
            let Some(existing) = &current else {
                return Err(412);
            };
            if !self.stale_accepting {
                // Literal mode compares the header bytes with the *unquoted*
                // tag, exactly as Ceph RGW does: the RFC-correct quoted form is
                // rejected even when it names the current version.
                let matches = if self.literal_dequoted {
                    wanted == strip_quotes(&existing.etag)
                } else {
                    wanted == existing.etag
                };
                if !matches {
                    return Err(412);
                }
            }
        }
        self.next_etag += 1;
        let etag = format!("\"0x8D{:016X}\"", self.next_etag);
        self.containers
            .entry(container.to_owned())
            .or_default()
            .insert(
                name.to_owned(),
                Stored {
                    bytes,
                    etag: etag.clone(),
                },
            );
        Ok(etag)
    }
}

fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

struct FakeAzure {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeAzure {
    fn start() -> FakeAzure {
        FakeAzure::start_with(FakeOptions::default())
    }

    fn start_with(options: FakeOptions) -> FakeAzure {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Azure");
        let addr = listener.local_addr().expect("fake Azure address");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let state = Arc::new(Mutex::new(State {
            stale_accepting: options.stale_accepting,
            literal_dequoted: options.literal_dequoted,
            require_signature: options.require_signature,
            next_etag: 1,
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                        handle_connection(stream, &thread_state);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        FakeAzure {
            addr,
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}/{ACCOUNT}", self.addr)
    }

    fn config(&self, container: &str) -> AzureConfig {
        AzureConfig {
            endpoint: self.endpoint(),
            container: container.to_owned(),
            account: ACCOUNT.to_owned(),
            key: Some(ACCOUNT_KEY.to_owned()),
            sas_token: None,
            anonymous: false,
            lock_lease: Duration::from_secs(2),
            request_timeout: Duration::from_secs(30),
        }
    }

    fn backend(&self, container: &str) -> AzureBackend {
        AzureBackend::new("<azure-unit>", self.config(container))
    }

    fn kind(&self, container: &str) -> BackendKind {
        BackendKind::Azure {
            config: self.config(container),
        }
    }

    fn authorizations(&self) -> Vec<String> {
        self.state.lock().unwrap().authorizations.clone()
    }

    fn signature_failures(&self) -> Vec<String> {
        self.state.lock().unwrap().signature_failures.clone()
    }

    fn etag(&self, container: &str, name: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .blob(container, name)
            .map(|stored| stored.etag.clone())
    }
}

#[derive(Default)]
struct FakeOptions {
    stale_accepting: bool,
    literal_dequoted: bool,
    require_signature: bool,
}

impl Drop for FakeAzure {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

const MAX_REQUEST_BYTES: u64 = 64 * 1024 * 1024;

fn handle_connection(mut stream: TcpStream, state: &Arc<Mutex<State>>) {
    let Ok(request) = read_request(&mut stream) else {
        return;
    };
    let response = route(&request, state);
    write_response(&mut stream, response);
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|err| err.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    let mut parts = line.trim_end().split(' ');
    let method = parts.next().ok_or("no method")?.to_owned();
    let target = parts.next().ok_or("no target")?.to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|err| err.to_string())?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if length > MAX_REQUEST_BYTES {
        return Err("request body too large".to_owned());
    }
    let mut body = vec![0u8; length as usize];
    reader
        .read_exact(&mut body)
        .map_err(|err| err.to_string())?;
    let (path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let mut query = BTreeMap::new();
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(name), percent_decode(value));
    }
    Ok(Request {
        method,
        path: percent_decode(path),
        query,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, response: Response) {
    // A HEAD reply announces the length the body *would* have. The fake records
    // it as an explicit header and keeps the body empty, so the declared length
    // must not be overwritten by the transported byte count.
    let declared = response
        .headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(response.body.len());
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        declared
    );
    for (name, value) in &response.headers {
        if name == "content-length" {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
    let _ = stream.flush();
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn route(request: &Request, state: &Arc<Mutex<State>>) -> Response {
    // The endpoint carries the account in the path, so the first segment is
    // the account and the canonicalised resource is exactly the URL path.
    let mut segments = request.path.trim_start_matches('/').splitn(3, '/');
    let account = segments.next().unwrap_or_default();
    let container = segments.next();
    let blob = segments.next();

    let mut state = state.lock().unwrap();
    state.authorizations.push(
        request
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_else(|| "<absent>".to_owned()),
    );

    if state.require_signature && !verify_shared_key(account, request) {
        let signature = request
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_default();
        state.signature_failures.push(format!(
            "{} {} -> {signature}",
            request.method, request.path
        ));
        return Response::new(403).with_header("x-ms-error-code", "AuthenticationFailed");
    }

    // Container-level operations.
    if blob.is_none() {
        let Some(container) = container else {
            return Response::new(400);
        };
        match request.method.as_str() {
            "HEAD" => {
                if state.containers.contains_key(container) {
                    Response::new(200)
                } else {
                    Response::new(404)
                }
            }
            "PUT" if request.query.get("restype").map(String::as_str) == Some("container") => {
                let existed = state
                    .containers
                    .insert(container.to_owned(), BTreeMap::new())
                    .is_some();
                if existed {
                    // A repeated create is the documented 409.
                    Response::new(409).with_header("x-ms-error-code", "ContainerAlreadyExists")
                } else {
                    Response::new(201)
                }
            }
            "GET" if request.query.get("comp").map(String::as_str) == Some("list") => {
                let Some(blobs) = state.containers.get(container) else {
                    return Response::new(404);
                };
                let prefix = request.query.get("prefix").cloned().unwrap_or_default();
                let mut xml = String::from(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Blobs>",
                );
                for (name, stored) in blobs.iter() {
                    if !name.starts_with(&prefix) {
                        continue;
                    }
                    xml.push_str(&format!(
                        "<Blob><Name>{}</Name><Properties><Content-Length>{}</Content-Length>\
                         <Etag>{}</Etag></Properties></Blob>",
                        xml_escape(name),
                        stored.bytes.len(),
                        xml_escape(&stored.etag)
                    ));
                }
                xml.push_str("</Blobs><NextMarker /></EnumerationResults>");
                Response {
                    status: 200,
                    headers: vec![("content-type".to_owned(), "application/xml".to_owned())],
                    body: xml.into_bytes(),
                }
            }
            _ => Response::new(400),
        }
    } else {
        let container = container.unwrap_or_default();
        let blob = blob.unwrap_or_default();
        match request.method.as_str() {
            "PUT" => {
                if request.headers.get("x-ms-blob-type").map(String::as_str) != Some("BlockBlob") {
                    return Response::new(400).with_header("x-ms-error-code", "InvalidHeaderValue");
                }
                let if_none_match_star = request
                    .headers
                    .get("if-none-match")
                    .map(|value| value == "*")
                    .unwrap_or(false);
                let if_match = request.headers.get("if-match").cloned();
                match state.write(
                    container,
                    blob,
                    request.body.clone(),
                    if_none_match_star,
                    if_match.as_deref(),
                ) {
                    Ok(etag) => Response::new(201).with_header("etag", &etag),
                    Err(409) => {
                        Response::new(409).with_header("x-ms-error-code", "BlobAlreadyExists")
                    }
                    Err(status) => Response::new(status),
                }
            }
            "GET" | "HEAD" => {
                let Some(stored) = state.blob(container, blob).cloned() else {
                    return Response::new(404).with_header("x-ms-error-code", "BlobNotFound");
                };
                let mut response = Response::new(200)
                    .with_header("etag", &stored.etag)
                    .with_header("last-modified", "Tue, 14 Nov 2023 22:13:20 GMT");
                let range = request
                    .headers
                    .get("range")
                    .and_then(|value| parse_range(value));
                let body = match range {
                    Some((start, end)) if start < stored.bytes.len() as u64 => {
                        let end = end.min(stored.bytes.len() as u64 - 1);
                        response.status = 206;
                        stored.bytes[start as usize..=end as usize].to_vec()
                    }
                    Some(_) => return Response::new(416),
                    None => stored.bytes.clone(),
                };
                response = response.with_header("content-length", &body.len().to_string());
                if request.method == "HEAD" {
                    response.body = Vec::new();
                } else {
                    response.body = body;
                }
                response
            }
            "DELETE" => {
                let Some(blobs) = state.containers.get_mut(container) else {
                    return Response::new(404);
                };
                match blobs.remove(blob) {
                    Some(_) => Response::new(202),
                    None => Response::new(404),
                }
            }
            _ => Response::new(400),
        }
    }
}

fn parse_range(value: &str) -> Option<(u64, u64)> {
    let rest = value.strip_prefix("bytes=")?;
    let (start, end) = rest.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// An independent SharedKey verifier (the cross-check)
// ---------------------------------------------------------------------------

fn verify_shared_key(account: &str, request: &Request) -> bool {
    let Some(header) = request.headers.get("authorization") else {
        return false;
    };
    let Some(rest) = header.strip_prefix("SharedKey ") else {
        return false;
    };
    let Some((header_account, signature)) = rest.split_once(':') else {
        return false;
    };
    if header_account != account {
        return false;
    }
    let key = match weftfs::b64::decode_any(ACCOUNT_KEY) {
        Ok(key) => key,
        Err(_) => return false,
    };
    let string_to_sign = independent_string_to_sign(account, request);
    weftfs::b64::encode_standard(&hmac_sha256(&key, string_to_sign.as_bytes())) == signature
}

fn independent_string_to_sign(account: &str, request: &Request) -> String {
    let header = |name: &str| request.headers.get(name).map(String::as_str).unwrap_or("");
    let sends_body = matches!(request.method.as_str(), "PUT" | "POST" | "PATCH");
    let content_length = if sends_body && !request.body.is_empty() {
        request.body.len().to_string()
    } else {
        String::new()
    };
    let mut text = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        request.method,
        header("content-encoding"),
        header("content-language"),
        content_length,
        header("content-md5"),
        header("content-type"),
        header("date"),
        header("if-modified-since"),
        header("if-match"),
        header("if-none-match"),
        header("if-unmodified-since"),
        header("range"),
    );
    for (name, value) in &request.headers {
        if name.starts_with("x-ms-") {
            text.push_str(name);
            text.push(':');
            text.push_str(value.trim());
            text.push('\n');
        }
    }
    // Canonicalised resource: the account followed by the path as received
    // (which itself starts with the account on a path-style endpoint), then the
    // query names sorted, each `name:value`.
    text.push_str(&format!("/{account}{}", request.path));
    let mut names: Vec<&String> = request.query.keys().collect();
    names.sort();
    for name in names {
        text.push('\n');
        text.push_str(&name.to_ascii_lowercase());
        text.push(':');
        text.push_str(&request.query[name]);
    }
    text
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for index in 0..64 {
        ipad[index] = block[index] ^ 0x36;
        opad[index] = block[index] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add(index as u8))
        .collect()
}

// ---------------------------------------------------------------------------
// End-to-end court
// ---------------------------------------------------------------------------

#[test]
fn store_round_trips_on_azure_with_independent_signature_checks() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-roundtrip";
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let kind = fake.kind(container);

    let mut store = Store::init_with_backend(&path, options(), kind.clone()).expect("init");
    let probes = store.plane_probes().cloned().expect("plane probes");
    assert!(
        probes.blobs.create_once_visible
            && probes.blobs.length_match
            && probes.blobs.range_read_match
            && probes.blobs.delete_observed,
        "blob plane probe must pass: {probes:?}"
    );
    assert!(
        probes.commits.create_ok
            && probes.commits.matching_compare_exchange_ok
            && probes.commits.stale_compare_exchange_rejected,
        "commit plane probe must prove stale-token refusal: {probes:?}"
    );

    let root = store.resolve_path("/").unwrap();
    let body = payload(4096, 3);
    let id = store
        .create_file_seeded(
            root,
            b"azure.txt",
            &body,
            0o644,
            0,
            0,
            Default::default(),
            b"az-1",
        )
        .expect("create");
    assert_eq!(store.read_object(id).unwrap(), body);

    store
        .write_file(id, 100, &payload(64, 9))
        .expect("range write");
    let mut expected = body.clone();
    expected[100..164].copy_from_slice(&payload(64, 9));
    assert_eq!(store.read_object(id).unwrap(), expected);

    let report = store.verify(true).expect("verify");
    assert!(report.ok, "verify problems: {:?}", report.problems);
    let generation = store.generation();
    let snapshot_root = store.snapshot_root();
    drop(store);

    let reopened = Store::open_with_backend(&path, OpenMode::ReadOnly, kind).expect("reopen");
    assert_eq!(reopened.generation(), generation);
    assert_eq!(reopened.snapshot_root(), snapshot_root);
    assert_eq!(reopened.read_object(id).unwrap(), expected);
    assert!(reopened.verify(true).expect("verify").ok);

    // The credential really was sent, and the fake's independent verifier
    // accepted every request.
    let authorizations = fake.authorizations();
    assert!(!authorizations.is_empty());
    assert!(
        authorizations
            .iter()
            .all(|value| value.starts_with("SharedKey devstoreaccount1:")),
        "every request must carry a SharedKey header: {authorizations:?}"
    );
    assert!(
        fake.signature_failures().is_empty(),
        "the independently recomputed signature rejected: {:?}",
        fake.signature_failures()
    );
}

#[test]
fn the_writer_lock_is_released_with_a_conditional_tombstone() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-lock";
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let kind = fake.kind(container);
    let store = Store::init_with_backend(&path, options(), kind.clone()).expect("init");
    assert_eq!(store.generation(), 1);
    drop(store);

    let observer = fake.backend(container);
    let record = observer
        .read_lock_record()
        .expect("read LOCK")
        .expect("published LOCK");
    assert!(record.released, "a clean drop must publish a tombstone");
    assert_eq!(record.fence, 1, "the first lease takes fence 1");
    assert!(
        record.released_from.is_some(),
        "a tombstone names the bytes it replaced"
    );
    // A released lock can be reclaimed by the next writable open.
    let reopened = Store::open_with_backend(&path, OpenMode::ReadWrite, kind).expect("reopen");
    drop(reopened);
}

#[test]
fn create_once_and_compare_and_swap_are_exact() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-cas";
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut backend = fake.backend(container);
    backend.ensure_layout().expect("layout");

    let key = Key::tmp("cas-blob");
    BlobPlane::put_if_absent(&mut backend, &key, b"one").expect("create");
    // Identical bytes are idempotent.
    BlobPlane::put_if_absent(&mut backend, &key, b"one").expect("idempotent create");
    // Different bytes are a real conflict.
    let err = BlobPlane::put_if_absent(&mut backend, &key, b"two").unwrap_err();
    assert_eq!(err.class_name(), "Exists", "{err}");

    let record = CommitPlane::read_control(&mut backend, &key)
        .expect("read")
        .expect("record");
    let etag = fake.etag(container, &key.display_path()).expect("etag");
    let updated =
        CommitPlane::compare_exchange(&mut backend, &key, record.token.as_deref(), b"three")
            .expect("matching CAS");
    assert_eq!(updated.bytes, b"three");
    // The old token is stale now: the CAS must be refused, and the winner's
    // bytes must survive.
    let err = CommitPlane::compare_exchange(&mut backend, &key, record.token.as_deref(), b"four")
        .unwrap_err();
    assert_eq!(err.class_name(), "Busy", "{err}");
    assert_eq!(
        CommitPlane::read_control(&mut backend, &key)
            .unwrap()
            .unwrap()
            .bytes,
        b"three"
    );
    assert_ne!(etag, fake.etag(container, &key.display_path()).unwrap());
    let _ = path;
}

#[test]
fn a_stale_etag_accepted_by_the_service_is_refused() {
    let fake = FakeAzure::start_with(FakeOptions {
        stale_accepting: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-stale";
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut backend = fake.backend(container);
    // The endpoint probe performs the stale conditional write itself, so this
    // is where a service that ignores `If-Match` is caught. (The generic plane
    // probe cannot: a backend that checks the content token before writing
    // never reaches the transport with a stale token.)
    let err = backend.ensure_layout().unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(
        err.message().contains("accepted a stale If-Match"),
        "the refusal must name the stale conditional write: {err}"
    );
    // A writable store refuses the same endpoint.
    let err = match Store::init_with_backend(&path, options(), fake.kind(container)) {
        Ok(_) => panic!("a stale-accepting endpoint must not open a writable store"),
        Err(err) => err,
    };
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
}

#[test]
fn a_literal_dequoted_endpoint_is_negotiated() {
    let fake = FakeAzure::start_with(FakeOptions {
        literal_dequoted: true,
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-dequoted";
    let mut mutable = fake.backend(container);
    mutable.ensure_layout().expect("layout");
    assert_eq!(
        mutable.if_match_form(),
        IfMatchForm::Dequoted,
        "the writable-open probe must record the measured form"
    );
    // A store still opens and commits, because every conditional write now uses
    // the measured form.
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut store = Store::init_with_backend(&path, options(), fake.kind(container)).expect("init");
    let root = store.resolve_path("/").unwrap();
    let id = store
        .create_file_seeded(
            root,
            b"f",
            b"dequoted",
            0o644,
            0,
            0,
            Default::default(),
            b"dq",
        )
        .expect("create");
    assert_eq!(store.read_object(id).unwrap(), b"dequoted");
    assert!(store.verify(true).expect("verify").ok);
    assert!(
        fake.signature_failures().is_empty(),
        "{:?}",
        fake.signature_failures()
    );
}

#[test]
fn a_missing_blob_is_not_found_and_transport_errors_are_io() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-missing";
    let mut backend = fake.backend(container);
    backend.ensure_layout().expect("layout");
    let missing = Key::tmp("never-written");
    let err = BlobPlane::get(&backend, &missing).unwrap_err();
    assert_eq!(err.class_name(), "NotFound", "{err}");
    assert_eq!(BlobPlane::exists(&backend, &missing).unwrap(), false);
    let err = BlobPlane::delete(&mut backend, &missing).unwrap_err();
    assert_eq!(err.class_name(), "NotFound", "{err}");

    // An unreachable endpoint is `Io`, never `Corrupt`: an endpoint that is
    // down says nothing about the bytes that were stored.
    let mut dead = AzureConfig {
        endpoint: "http://127.0.0.1:1/devstoreaccount1".to_owned(),
        container: "weft-azure-unit-dead".to_owned(),
        account: ACCOUNT.to_owned(),
        key: Some(ACCOUNT_KEY.to_owned()),
        sas_token: None,
        anonymous: false,
        lock_lease: Duration::from_secs(2),
        request_timeout: Duration::from_secs(2),
    };
    dead.request_timeout = Duration::from_secs(2);
    let mut backend = AzureBackend::new("<dead>", dead);
    let err = BlobPlane::get(&backend, &missing).unwrap_err();
    assert_eq!(err.class_name(), "Io", "{err}");
    let err = backend.acquire_lock().unwrap_err();
    assert_eq!(err.class_name(), "Io", "{err}");
}

#[test]
fn a_wrong_account_key_is_unauthorized() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-auth";
    let mut config = fake.config(container);
    config.key = Some(
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw=="
            .to_owned(),
    );
    let mut backend = AzureBackend::new("<wrong-key>", config);
    let err = backend.ensure_layout().unwrap_err();
    assert_eq!(err.class_name(), "Unauthorized", "{err}");
    assert!(!fake.signature_failures().is_empty());
}

#[test]
fn listing_returns_only_the_requested_prefix() {
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-list";
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut store = Store::init_with_backend(&path, options(), fake.kind(container)).expect("init");
    let root = store.resolve_path("/").unwrap();
    store
        .create_file_seeded(root, b"a", b"alpha", 0o644, 0, 0, Default::default(), b"a")
        .expect("create a");
    store
        .create_file_seeded(root, b"b", b"bravo", 0o644, 0, 0, Default::default(), b"b")
        .expect("create b");
    let entries = store.list_dir(root).expect("snapshot listing");
    assert_eq!(entries.len(), 2);
    let mut backend = fake.backend(container);
    let objects = BlobPlane::list(&backend, &Key::prefix(weftfs::backend::KeyKind::Object))
        .expect("object listing");
    assert!(!objects.is_empty());
    assert!(
        objects
            .iter()
            .all(|key| key.display_path().starts_with("objects/")),
        "{objects:?}"
    );
    let _ = &mut backend;
}

#[test]
fn sas_and_anonymous_credentials_are_exclusive_forms() {
    // Resolution refuses two credential forms rather than guessing.
    let err = AzureConfig::resolve(
        Some("http://127.0.0.1:10000/devstoreaccount1".to_owned()),
        Some("weft-azure-unit".to_owned()),
        Some(ACCOUNT.to_owned()),
        Some(ACCOUNT_KEY.to_owned()),
        Some("sv=2021&sig=abc".to_owned()),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument", "{err}");

    // A SAS credential alone is accepted, and no SharedKey header is produced.
    let fake = FakeAzure::start();
    let mut config = fake.config("weft-azure-unit-sas");
    config.key = None;
    config.sas_token = Some("sv=2021-12-02&sig=unit".to_owned());
    let mut backend = AzureBackend::new("<sas>", config);
    backend.ensure_layout().expect("layout");
    let key = Key::tmp("sas-blob");
    BlobPlane::put_if_absent(&mut backend, &key, b"sas-bytes").expect("put");
    assert_eq!(BlobPlane::get(&backend, &key).unwrap(), b"sas-bytes");
    let authorizations = fake.authorizations();
    assert!(!authorizations.is_empty());
    assert!(
        authorizations.iter().all(|value| value == "<absent>"),
        "a SAS credential must not send an Authorization header: {authorizations:?}"
    );
}

#[test]
fn a_hash32_key_path_is_addressed_verbatim() {
    // The logical layout must survive the wire: a content-addressed key is a
    // slash-separated path, not a single encoded segment.
    let fake = FakeAzure::start_with(FakeOptions {
        require_signature: true,
        ..FakeOptions::default()
    });
    let container = "weft-azure-unit-layout";
    let mut backend = fake.backend(container);
    backend.ensure_layout().expect("layout");
    let hash = Hash32::from_bytes([0x5a; 32]);
    let key = Key::object(hash);
    BlobPlane::put_if_absent(&mut backend, &key, b"chunk").expect("put");
    assert!(
        fake.etag(container, &format!("objects/5a/{}", "5a".repeat(32)))
            .is_some(),
        "the blob must be stored under the normative key path"
    );
    assert_eq!(BlobPlane::len(&backend, &key).unwrap(), 5);
    assert_eq!(BlobPlane::get_range(&backend, &key, 1, 3).unwrap(), b"hun");
}

// ---------------------------------------------------------------------------
// Live-emulator court
//
// These tests run only when `WEFT_AZURE_TEST_ENDPOINT` is set;
// `bench/scripts/azure_court.sh` starts a pinned Azurite release and supplies
// the credential. Without it each test prints one `SKIP` line and returns, so
// the suite is honest about what was and was not measured.
// ---------------------------------------------------------------------------

struct Live {
    endpoint: String,
    account: String,
    key: Option<String>,
    base: String,
}

static LIVE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn live() -> Option<Live> {
    let endpoint = std::env::var("WEFT_AZURE_TEST_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty());
    let Some(endpoint) = endpoint else {
        eprintln!("SKIP azure live court: WEFT_AZURE_TEST_ENDPOINT is not set");
        return None;
    };
    let base = std::env::var("WEFT_AZURE_TEST_CONTAINER")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "weft-az-court".to_owned());
    let account = std::env::var("WEFT_AZURE_ACCOUNT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "devstoreaccount1".to_owned());
    let key = std::env::var("WEFT_AZURE_KEY")
        .ok()
        .filter(|value| !value.is_empty());
    Some(Live {
        endpoint,
        account,
        key,
        base,
    })
}

impl Live {
    /// A fresh container name for one test. Every observer must be built from
    /// this exact name, because a store root *is* its container.
    fn container(&self, tag: &str) -> String {
        let n = LIVE_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{}-{tag}-{n}", self.base)
    }

    fn config_for(&self, container: &str) -> AzureConfig {
        AzureConfig {
            endpoint: self.endpoint.clone(),
            container: container.to_owned(),
            account: self.account.clone(),
            key: self.key.clone(),
            sas_token: None,
            anonymous: self.key.is_none(),
            lock_lease: Duration::from_secs(2),
            request_timeout: Duration::from_secs(60),
        }
    }

    fn kind_for(&self, container: &str) -> BackendKind {
        BackendKind::Azure {
            config: self.config_for(container),
        }
    }

    fn backend_for(&self, container: &str) -> AzureBackend {
        AzureBackend::new("<azure-live-court>", self.config_for(container))
    }
}

#[test]
fn live_store_round_trips_and_reopens() {
    let Some(live) = live() else { return };
    let container = live.container("live-rt");
    let backend = live.backend_for(&container);
    assert_eq!(
        backend.if_match_form(),
        IfMatchForm::AsReturned,
        "Azurite returns quoted ETags and accepts them quoted"
    );
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let kind = live.kind_for(&container);
    let mut store = Store::init_with_backend(&path, options(), kind.clone()).expect("init");
    let probes = store.plane_probes().cloned().expect("plane probes");
    assert!(
        probes.commits.stale_compare_exchange_rejected,
        "the plane probe must observe stale-token rejection: {probes:?}"
    );
    let root = store.resolve_path("/").unwrap();
    let body = payload(8192, 11);
    let id = store
        .create_file_seeded(root, b"live.txt", &body, 0o644, 0, 0, Default::default(), b"live")
        .expect("create");
    store.write_file(id, 32, &payload(64, 0x22)).expect("range write");
    let mut expected = body.clone();
    expected[32..96].copy_from_slice(&payload(64, 0x22));
    assert_eq!(store.read_object(id).unwrap(), expected);
    assert!(store.verify(true).expect("verify").ok);
    let generation = store.generation();
    let snapshot_root = store.snapshot_root();
    drop(store);

    let reopened = Store::open_with_backend(&path, OpenMode::ReadOnly, kind).expect("reopen");
    assert_eq!(reopened.generation(), generation);
    assert_eq!(reopened.snapshot_root(), snapshot_root);
    assert_eq!(reopened.read_object(id).unwrap(), expected);
}

#[test]
fn live_conditional_create_and_cas_are_exact() {
    let Some(live) = live() else { return };
    let container = live.container("live-cas");
    let mut backend = live.backend_for(&container);
    backend.ensure_layout().expect("layout");
    let key = Key::tmp("live-cas-blob");
    BlobPlane::put_if_absent(&mut backend, &key, b"one").expect("create");
    BlobPlane::put_if_absent(&mut backend, &key, b"one").expect("idempotent create");
    let err = BlobPlane::put_if_absent(&mut backend, &key, b"two").unwrap_err();
    assert_eq!(err.class_name(), "Exists", "{err}");
    let record = CommitPlane::read_control(&mut backend, &key)
        .expect("read")
        .expect("record");
    CommitPlane::compare_exchange(&mut backend, &key, record.token.as_deref(), b"three")
        .expect("matching CAS");
    let err = CommitPlane::compare_exchange(&mut backend, &key, record.token.as_deref(), b"four")
        .unwrap_err();
    assert_eq!(err.class_name(), "Busy", "{err}");
    assert_eq!(
        CommitPlane::read_control(&mut backend, &key)
            .unwrap()
            .unwrap()
            .bytes,
        b"three"
    );
}

#[test]
fn live_listing_and_ranges_are_exact() {
    let Some(live) = live() else { return };
    let container = live.container("live-list");
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let mut store =
        Store::init_with_backend(&path, options(), live.kind_for(&container)).expect("init");
    let root = store.resolve_path("/").unwrap();
    for (name, body) in [(b"a".as_slice(), b"alpha".as_slice()), (b"b", b"bravo")] {
        store
            .create_file_seeded(root, name, body, 0o644, 0, 0, Default::default(), name)
            .expect("create");
    }
    assert_eq!(store.list_dir(root).expect("listing").len(), 2);
    let backend = live.backend_for(&container);
    let objects = BlobPlane::list(&backend, &Key::prefix(weftfs::backend::KeyKind::Object))
        .expect("object listing");
    assert!(!objects.is_empty());
    assert!(objects
        .iter()
        .all(|key| key.display_path().starts_with("objects/")));
    let key = objects[0].clone();
    let whole = BlobPlane::get(&backend, &key).expect("get");
    let part = BlobPlane::get_range(&backend, &key, 1, 3).expect("range");
    assert_eq!(part, whole[1..4.min(whole.len())].to_vec());
}

#[test]
fn live_lock_is_released_with_a_conditional_tombstone() {
    let Some(live) = live() else { return };
    let container = live.container("live-lock");
    let dir = tempdir().unwrap();
    let path = dir.path().join("store");
    let kind = live.kind_for(&container);
    let store = Store::init_with_backend(&path, options(), kind.clone()).expect("init");
    drop(store);
    let observer = live.backend_for(&container);
    let record = observer
        .read_lock_record()
        .expect("read LOCK")
        .expect("published LOCK");
    assert!(record.released, "a clean drop publishes a tombstone");
    assert_eq!(record.fence, 1);
    assert!(record.released_from.is_some());
    let reopened = Store::open_with_backend(&path, OpenMode::ReadWrite, kind).expect("reopen");
    drop(reopened);
}
