//! GCS backend court (subphase R14).
//!
//! Everything here runs against [`FakeGcs`], an in-process HTTP service that
//! speaks the subset of the GCS JSON API this backend uses: object metadata,
//! media download (including `Range`), simple and resumable uploads with
//! `ifGenerationMatch`, listing, bucket metadata, and delete.
//!
//! The fake is a test double, not a GCS emulator: it exists so the backend's
//! request shapes, status mapping, and — most importantly — its handling of
//! generation preconditions are exercised deterministically without a network
//! or an external dependency. `bench/scripts/gcs_court.sh` runs the same
//! backend against an external emulator when one is available and records
//! `emulator_unavailable` with the exact command when it is not.
//!
//! What the fake can prove: create-once conflict, matching-generation success,
//! stale-generation refusal, ranged reads, missing objects, transport errors
//! preserving `Error::Io`, and that a service which *accepts* a stale
//! generation is refused `UnsupportedVersion` both by the probe and by
//! `Store::init_with_backend`.
//!
//! What it cannot prove: anything about a real GCS deployment, media
//! durability, power loss, or multi-node consistency.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key};
use weftfs::gcs::{GcsBackend, GcsConfig};
use weftfs::planes::{BlobPlane, CommitPlane};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Hash32;

// ---------------------------------------------------------------------------
// A minimal in-process GCS JSON API
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Stored {
    bytes: Vec<u8>,
    generation: u64,
}

struct Session {
    bucket: String,
    name: String,
}

#[derive(Default)]
struct State {
    /// bucket -> object name -> bytes and generation.
    objects: BTreeMap<String, BTreeMap<String, Stored>>,
    sessions: BTreeMap<String, Session>,
    /// Every Authorization header value observed, so a test can prove the
    /// bearer token is actually sent and that nothing else leaks it.
    authorizations: Vec<String>,
    next_generation: u64,
    session_counter: u64,
    /// When true, a non-zero `ifGenerationMatch` is ignored: the fake accepts
    /// a stale generation, which a correct backend must refuse.
    stale_accepting: bool,
}

impl State {
    fn object(&self, bucket: &str, name: &str) -> Option<&Stored> {
        self.objects.get(bucket).and_then(|map| map.get(name))
    }

    fn write(
        &mut self,
        bucket: &str,
        name: &str,
        bytes: Vec<u8>,
        if_generation_match: Option<u64>,
    ) -> Result<u64, u16> {
        let current = self.object(bucket, name).map(|stored| stored.generation);
        match (if_generation_match, current) {
            // Create-once.
            (Some(0), Some(_)) => return Err(412),
            (Some(0), None) => {}
            // Compare-and-swap.
            (Some(wanted), Some(observed)) => {
                if !self.stale_accepting && wanted != observed {
                    return Err(412);
                }
            }
            (Some(_), None) => return Err(412),
            (None, _) => {}
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        self.objects
            .entry(bucket.to_owned())
            .or_default()
            .insert(name.to_owned(), Stored { bytes, generation });
        Ok(generation)
    }
}

/// A running fake service. Dropping it stops the accept loop.
struct FakeGcs {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeGcs {
    fn start(stale_accepting: bool) -> FakeGcs {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake GCS");
        let addr = listener.local_addr().expect("fake GCS address");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let state = Arc::new(Mutex::new(State {
            stale_accepting,
            next_generation: 1,
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
                        handle_connection(stream, &thread_state, addr);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        FakeGcs {
            addr,
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn config(&self, bucket: &str) -> GcsConfig {
        GcsConfig {
            endpoint: self.endpoint(),
            bucket: bucket.to_owned(),
            token: Some("unit-test-token".to_owned()),
            project: Some("weft-unit".to_owned()),
            anonymous: false,
            lock_lease: Duration::from_secs(2),
            request_timeout: Duration::from_secs(30),
        }
    }

    fn backend(&self, bucket: &str) -> GcsBackend {
        GcsBackend::new("<gcs-unit>", self.config(bucket))
    }

    fn authorizations(&self) -> Vec<String> {
        self.state.lock().unwrap().authorizations.clone()
    }
}

impl Drop for FakeGcs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop so the thread can observe the stop flag.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Request {
    method: String,
    target: String,
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

    fn json(status: u16, body: String) -> Response {
        Response {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Response {
        Response::json(
            status,
            format!(
                "{{\"error\":{{\"code\":{status},\"message\":{}}}}}",
                json_quote(message)
            ),
        )
    }

    fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

const MAX_REQUEST_BYTES: u64 = 64 * 1024 * 1024;

fn handle_connection(mut stream: TcpStream, state: &Arc<Mutex<State>>, addr: SocketAddr) {
    let Ok(request) = read_request(&mut stream) else {
        return;
    };
    let response = route(&request, state, addr);
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
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, response: Response) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
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
        204 => "No Content",
        206 => "Partial Content",
        308 => "Resume Incomplete",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn route(request: &Request, state: &Arc<Mutex<State>>, addr: SocketAddr) -> Response {
    let (raw_path, raw_query) = request
        .target
        .split_once('?')
        .unwrap_or((request.target.as_str(), ""));
    let path = decode(raw_path);
    let query = parse_query(raw_query);

    let mut state = state.lock().unwrap();
    if let Some(value) = request.headers.get("authorization") {
        state.authorizations.push(value.clone());
    }

    if path.starts_with("/upload-session/") {
        if request.method != "PUT" {
            return Response::error(400, "upload session expects PUT");
        }
        let id = path.trim_start_matches("/upload-session/").to_owned();
        let Some(session) = state.sessions.remove(&id) else {
            return Response::error(404, "unknown upload session");
        };
        // The precondition was evaluated when the session was initiated, as
        // GCS does; the PUT itself stores the bytes.
        return match state.write(&session.bucket, &session.name, request.body.clone(), None) {
            Ok(generation) => Response::json(
                200,
                format!(
                    "{{\"name\":{},\"generation\":\"{generation}\"}}",
                    json_quote(&session.name)
                ),
            ),
            Err(status) => Response::error(status, "write failed"),
        };
    }

    if path == "/upload/storage/v1/b" || path.starts_with("/upload/storage/v1/b/") {
        return upload(request, &path, &query, &mut state, addr);
    }

    if path == "/storage/v1/b" {
        if request.method != "POST" {
            return Response::error(400, "bucket collection expects POST");
        }
        // `buckets.insert` sends the bucket resource in the request body; a
        // query `name` is also accepted for convenience.
        let bucket = query
            .get("name")
            .cloned()
            .or_else(|| json_string_field(&request.body, "name"))
            .unwrap_or_default();
        if bucket.is_empty() {
            return Response::error(400, "bucket name is required");
        }
        state.objects.entry(bucket.clone()).or_default();
        return Response::json(200, format!("{{\"name\":{}}}", json_quote(&bucket)));
    }

    let Some(rest) = path.strip_prefix("/storage/v1/b/") else {
        return Response::error(404, "no such API path");
    };
    let (bucket, tail) = match rest.split_once('/') {
        Some((bucket, tail)) => (bucket.to_owned(), tail.to_owned()),
        None => (rest.to_owned(), String::new()),
    };
    if tail.is_empty() {
        // Bucket metadata.
        if !state.objects.contains_key(&bucket) {
            return Response::error(404, "no such bucket");
        }
        return Response::json(200, format!("{{\"name\":{}}}", json_quote(&bucket)));
    }
    let name = match tail.strip_prefix("o") {
        Some(rest) => rest.strip_prefix('/').unwrap_or(rest).to_owned(),
        None => return Response::error(404, "no such API path"),
    };

    // `.../b/<bucket>/o` with no object name is the object collection.
    if name.is_empty() {
        if request.method != "GET" {
            return Response::error(400, "object collection expects GET");
        }
        let Some(map) = state.objects.get(&bucket) else {
            return Response::error(404, "no such bucket");
        };
        let prefix = query.get("prefix").cloned().unwrap_or_default();
        // One page: the tests never exceed the client's request page size.
        let items = map
            .keys()
            .filter(|object| object.starts_with(&prefix))
            .map(|object| format!("{{\"name\":{}}}", json_quote(object)))
            .collect::<Vec<_>>()
            .join(",");
        return Response::json(200, format!("{{\"items\":[{items}]}}"));
    }

    match request.method.as_str() {
        "GET" => {
            if query.get("alt").map(String::as_str) == Some("media") {
                let Some(stored) = state.object(&bucket, &name).cloned() else {
                    return Response::error(404, "no such object");
                };
                return media(request, stored);
            }
            let Some(stored) = state.object(&bucket, &name) else {
                return Response::error(404, "no such object");
            };
            Response::json(
                200,
                format!(
                    "{{\"name\":{},\"bucket\":{},\"size\":\"{}\",\"generation\":\"{}\"}}",
                    json_quote(&name),
                    json_quote(&bucket),
                    stored.bytes.len(),
                    stored.generation
                ),
            )
        }
        "DELETE" => {
            let removed = state
                .objects
                .get_mut(&bucket)
                .and_then(|map| map.remove(&name));
            match removed {
                Some(_) => Response::new(204),
                None => Response::error(404, "no such object"),
            }
        }
        _ => Response::error(400, "unsupported method"),
    }
}

fn upload(
    request: &Request,
    path: &str,
    query: &BTreeMap<String, String>,
    state: &mut State,
    addr: SocketAddr,
) -> Response {
    let Some(rest) = path.strip_prefix("/upload/storage/v1/b/") else {
        return Response::error(400, "bad upload path");
    };
    let bucket = rest.strip_suffix("/o").unwrap_or(rest).to_owned();
    let Some(name) = query.get("name").cloned() else {
        return Response::error(400, "upload name is required");
    };
    let precondition = query
        .get("ifGenerationMatch")
        .and_then(|value| value.parse::<u64>().ok());
    match query.get("uploadType").map(String::as_str) {
        Some("media") => match state.write(&bucket, &name, request.body.clone(), precondition) {
            Ok(generation) => Response::json(
                200,
                format!(
                    "{{\"name\":{},\"generation\":\"{generation}\",\"size\":\"{}\"}}",
                    json_quote(&name),
                    request.body.len()
                ),
            ),
            Err(status) => Response::error(status, "precondition failed"),
        },
        Some("resumable") => {
            // Evaluate the precondition at initiation, as GCS does.
            let current = state.object(&bucket, &name).map(|stored| stored.generation);
            let accepted = match (precondition, current) {
                (Some(0), Some(_)) => false,
                (Some(0), None) => true,
                (Some(wanted), Some(observed)) => state.stale_accepting || wanted == observed,
                (Some(_), None) => false,
                (None, _) => true,
            };
            if !accepted {
                return Response::error(412, "precondition failed");
            }
            state.session_counter += 1;
            let id = format!("session-{}", state.session_counter);
            state.sessions.insert(
                id.clone(),
                Session {
                    bucket: bucket.clone(),
                    name: name.clone(),
                },
            );
            Response::new(200)
                .with_header("location", &format!("http://{addr}/upload-session/{id}"))
        }
        _ => Response::error(400, "unsupported upload type"),
    }
}

fn media(request: &Request, stored: Stored) -> Response {
    let Some(range) = request.headers.get("range") else {
        return Response {
            status: 200,
            headers: vec![(
                "x-goog-generation".to_owned(),
                stored.generation.to_string(),
            )],
            body: stored.bytes,
        };
    };
    let Some(spec) = range.strip_prefix("bytes=") else {
        return Response::error(416, "unsupported range");
    };
    let (start, end) = match spec.split_once('-') {
        Some((start, end)) => (start, end),
        None => return Response::error(416, "unsupported range"),
    };
    let Ok(start) = start.parse::<usize>() else {
        return Response::error(416, "unsupported range");
    };
    if start >= stored.bytes.len() {
        return Response::error(416, "range starts past the object");
    }
    let end = if end.is_empty() {
        stored.bytes.len() - 1
    } else {
        end.parse::<usize>()
            .unwrap_or(stored.bytes.len() - 1)
            .min(stored.bytes.len() - 1)
    };
    let body = stored.bytes[start..=end].to_vec();
    Response {
        status: 206,
        headers: vec![
            (
                "content-range".to_owned(),
                format!("bytes {start}-{end}/{}", stored.bytes.len()),
            ),
            (
                "x-goog-generation".to_owned(),
                stored.generation.to_string(),
            ),
        ],
        body,
    }
}

fn parse_query(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(decode(key), decode(value));
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_digit(bytes[index + 1]);
            let low = hex_digit(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Extract one string field from a small flat JSON body. Enough for the
/// fake's bucket-insert handler; `None` rather than a guess when unreadable.
fn json_string_field(body: &[u8], field: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let needle = format!("\"{field}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let mut chars = rest.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            ch => out.push(ch),
        }
    }
    None
}

fn json_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn expect_err<T>(result: weftfs::Result<T>, what: &str) -> weftfs::Error {
    match result {
        Ok(_) => panic!("{what}: expected an error"),
        Err(err) => err,
    }
}

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = u64::from(seed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(1);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

// ---------------------------------------------------------------------------
// Blob plane
// ---------------------------------------------------------------------------

#[test]
fn create_once_succeeds_is_idempotent_and_rejects_different_bytes() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("create-once");

    backend
        .put_if_absent(&key, b"alpha")
        .expect("first publish");
    // The same bytes under the same key are idempotent.
    backend
        .put_if_absent(&key, b"alpha")
        .expect("idempotent republish");

    let err = expect_err(
        backend.put_if_absent(&key, b"beta"),
        "same key, different bytes",
    );
    assert_eq!(err.class_name(), "Exists", "unexpected error: {err}");

    // The rejected write must not have replaced the first writer's bytes.
    assert_eq!(backend.get(&key).unwrap(), b"alpha");
    // A generation precondition is what made that safe.
    assert!(backend.generation_for_test(&key).unwrap().unwrap() >= 1);

    // The bearer token reaches the endpoint as an Authorization header.
    let seen = fake.authorizations();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|value| value == "Bearer unit-test-token"),
        "unexpected authorization headers: {seen:?}"
    );
}

#[test]
fn range_reads_are_exact_and_end_of_object_is_not_an_error() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("ranges");
    let body = b"0123456789abcdef".to_vec();
    backend.put_if_absent(&key, &body).unwrap();

    assert_eq!(backend.len(&key).unwrap(), 16);
    assert_eq!(backend.get_range(&key, 4, 5).unwrap(), b"45678");
    assert_eq!(backend.get_range(&key, 15, 1).unwrap(), b"f");
    // An offset exactly at the end returns no bytes, matching the local
    // backend's semantics.
    assert!(backend.get_range(&key, 16, 4).unwrap().is_empty());
    // Strictly beyond the object is an error.
    let err = expect_err(backend.get_range(&key, 17, 1), "offset past end");
    assert_eq!(
        err.class_name(),
        "InvalidArgument",
        "unexpected error: {err}"
    );
    // A range that runs past the end is clipped, not padded.
    assert_eq!(backend.get_range(&key, 14, 8).unwrap(), b"ef");
}

#[test]
fn missing_object_is_not_found_everywhere() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("absent");

    assert_eq!(
        expect_err(backend.get(&key), "get absent").class_name(),
        "NotFound"
    );
    assert_eq!(
        expect_err(backend.len(&key), "len absent").class_name(),
        "NotFound"
    );
    assert_eq!(
        expect_err(backend.delete(&key), "delete absent").class_name(),
        "NotFound"
    );
    assert!(!backend.exists(&key).unwrap());
    assert!(backend
        .read_control(&key)
        .expect("read_control absent")
        .is_none());
}

#[test]
fn listing_round_trips_the_normative_layout() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let hash =
        Hash32::from_lower_hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap();
    let object = Key::object(hash);
    backend.put_if_absent(&object, b"chunk").unwrap();
    backend
        .put_if_absent(&Key::snapshot_ref("s1"), b"ref")
        .unwrap();

    let objects = backend
        .list(&Key::prefix(weftfs::backend::KeyKind::Object))
        .unwrap();
    assert_eq!(objects, vec![object]);

    let refs = backend
        .list(&Key::prefix(weftfs::backend::KeyKind::SnapshotRef))
        .unwrap();
    assert_eq!(refs, vec![Key::snapshot_ref("s1")]);
}

// ---------------------------------------------------------------------------
// Commit plane: generation preconditions
// ---------------------------------------------------------------------------

#[test]
fn matched_generation_succeeds_and_a_stale_generation_is_busy() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("cas");

    // Absent -> create-once.
    let created = backend
        .compare_exchange(&key, None, b"one")
        .expect("create via compare-exchange");
    assert_eq!(created.bytes, b"one");
    let first_token = created.token.expect("token");
    let first_generation = backend.generation_for_test(&key).unwrap().unwrap();

    // The token is the content token, and a matching generation replaces.
    let matched = backend
        .compare_exchange(&key, Some(&first_token), b"two")
        .expect("matching compare-exchange");
    assert_eq!(matched.bytes, b"two");

    // Reusing the old token must fail as Busy without touching the bytes.
    let stale = expect_err(
        backend.compare_exchange(&key, Some(&first_token), b"three"),
        "stale compare-exchange",
    );
    assert_eq!(stale.class_name(), "Busy", "unexpected error: {stale}");

    // The same fact at the wire level: a stale ifGenerationMatch is refused.
    assert!(!backend
        .conditional_upload_for_test(&key, b"four", first_generation)
        .unwrap());
    assert_eq!(backend.get(&key).unwrap(), b"two");
    assert!(backend.generation_for_test(&key).unwrap().unwrap() > first_generation);
}

#[test]
fn control_create_is_create_once_and_delete_is_reported() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("control");

    backend.create_control(&key, 32).unwrap();
    assert_eq!(backend.control_len(&key).unwrap(), 32);
    assert!(backend.control_exists(&key).unwrap());
    assert_eq!(backend.get(&key).unwrap(), vec![0u8; 32]);

    let err = expect_err(backend.create_control(&key, 8), "second create");
    assert_eq!(err.class_name(), "Exists", "unexpected error: {err}");

    backend.delete_control(&key).unwrap();
    assert!(!backend.control_exists(&key).unwrap());
    assert_eq!(
        expect_err(backend.delete_control(&key), "second delete").class_name(),
        "NotFound"
    );
}

#[test]
fn transport_failure_stays_io_and_is_not_reported_as_corruption() {
    // Port 1 on loopback has no listener and is never allocated to a test fake,
    // so the connection is refused deterministically.
    let config = GcsConfig {
        endpoint: "http://127.0.0.1:1".to_owned(),
        bucket: "weft-unit".to_owned(),
        token: Some("unit-test-token".to_owned()),
        project: None,
        anonymous: false,
        lock_lease: Duration::from_secs(2),
        request_timeout: Duration::from_secs(5),
    };
    let backend = GcsBackend::new("<gcs-unit>", config);
    let err = expect_err(backend.get(&Key::tmp("unreachable")), "unreachable");
    assert_eq!(err.class_name(), "Io", "unexpected error: {err}");
    assert!(
        !err.message().contains("unit-test-token"),
        "the transport error leaked the bearer token: {err}"
    );
}

// ---------------------------------------------------------------------------
// Capability probing
// ---------------------------------------------------------------------------

#[test]
fn probe_refuses_an_endpoint_that_accepts_a_stale_generation() {
    let fake = FakeGcs::start(true);
    let mut backend = fake.backend("weft-unit");

    let err = expect_err(backend.init_layout(), "stale-accepting fake");
    assert_eq!(
        err.class_name(),
        "UnsupportedVersion",
        "unexpected error: {err}"
    );
    assert!(
        err.message().contains("stale") && err.message().contains("ifGenerationMatch"),
        "unexpected error text: {err}"
    );
}

#[test]
fn store_refuses_a_stale_accepting_endpoint_before_publishing() {
    let fake = FakeGcs::start(true);
    let dir = tempdir().unwrap();
    let kind = BackendKind::Gcs {
        config: fake.config("weft-unit"),
    };
    let err = expect_err(
        Store::init_with_backend(dir.path().join("store"), opts(), kind),
        "store over a stale-accepting fake",
    );
    assert_eq!(
        err.class_name(),
        "UnsupportedVersion",
        "unexpected error: {err}"
    );
}

#[test]
fn probe_records_create_once_matching_and_stale_rejection() {
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    backend.init_layout().expect("honest fake passes the probe");

    let blobs = backend.head_part(&[7u8; 192]).expect("blob probe");
    assert!(blobs.attempted);
    assert!(blobs.create_once_visible);
    assert!(blobs.length_match);
    assert!(blobs.range_read_match);
    assert!(blobs.delete_observed);

    let commits = backend.probe().expect("commit probe");
    assert!(commits.attempted);
    assert!(commits.create_ok);
    assert!(commits.matching_compare_exchange_ok);
    assert!(commits.stale_compare_exchange_rejected);
    assert_eq!(commits.token_kind, "content-token");

    // Probe scratch objects are cleaned up.
    let leftovers = backend
        .list(&Key::prefix(weftfs::backend::KeyKind::Tmp))
        .unwrap();
    assert!(leftovers.is_empty(), "probe left {leftovers:?}");
}

// ---------------------------------------------------------------------------
// End to end: a real store over the fake
// ---------------------------------------------------------------------------

#[test]
fn store_round_trip_is_byte_identical_to_local_and_reopens() {
    let fake = FakeGcs::start(false);
    let dir = tempdir().unwrap();
    let local_path = dir.path().join("local");
    let gcs_path = dir.path().join("gcs");

    let gcs_kind = BackendKind::Gcs {
        config: fake.config("weft-round-trip"),
    };
    // A second handle observes the control object's GCS generations without
    // taking the writer lock.
    let observer = fake.backend("weft-round-trip");

    let contents: Vec<Vec<u8>> = (0..4).map(|index| bytes(4096, index as u8)).collect();

    let run = |kind: BackendKind, path: &std::path::Path| -> (String, u64, Vec<u64>) {
        let mut store = Store::init_with_backend(path, opts(), kind).expect("init store");
        let root = store.resolve_path("/").unwrap();
        let mut ids = Vec::new();
        let mut generations = vec![store.generation()];
        for (index, body) in contents.iter().enumerate() {
            let id = store
                .create_file_seeded(
                    root,
                    format!("f{index}").as_bytes(),
                    body,
                    0o644,
                    0,
                    0,
                    Default::default(),
                    format!("seed-{index}").as_bytes(),
                )
                .expect("create file");
            ids.push(id);
            generations.push(store.generation());
        }
        // One range write, then a full verify.
        store
            .write_file(ids[0], 0, &[0xAA; 64])
            .expect("range write");
        generations.push(store.generation());
        let report = store.verify(true).expect("verify full");
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        let snapshot = store.snapshot_root().to_hex();
        let generation = store.generation();
        drop(store);
        (snapshot, generation, generations)
    };

    let (local_root, local_generation, local_generations) = run(BackendKind::Local, &local_path);
    let (gcs_root, gcs_generation, gcs_generations) = run(gcs_kind.clone(), &gcs_path);

    assert_eq!(gcs_root, local_root, "content roots diverged");
    assert_eq!(gcs_generation, local_generation, "generations diverged");
    assert_eq!(
        gcs_generations, local_generations,
        "commit sequences diverged"
    );

    // The superblock is replaced on every commit, so its GCS generation is a
    // direct observation that each head publication used ifGenerationMatch.
    let superblock_generation = observer
        .generation_for_test(&Key::superblock())
        .unwrap()
        .expect("superblock object exists");
    assert!(
        superblock_generation >= gcs_generation,
        "superblock generation {superblock_generation} should be at least the commit count \
         {gcs_generation}"
    );

    // Reopen read-only over GCS and prove the recovered state is the same whole
    // state, with a clean full verify.
    let reopened = Store::open_with_backend(&gcs_path, OpenMode::ReadOnly, gcs_kind.clone())
        .expect("reopen read-only");
    assert_eq!(reopened.snapshot_root().to_hex(), gcs_root);
    assert_eq!(reopened.generation(), gcs_generation);
    let report = reopened.verify(true).expect("reopen verify");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    drop(reopened);

    // A second live writer is refused Busy, which is the LOCK object's
    // generation-conditioned create proving itself.
    let live = Store::init_with_backend(
        dir.path().join("lock-holder"),
        opts(),
        BackendKind::Gcs {
            config: fake.config("weft-lock"),
        },
    )
    .expect("init lock holder");
    let err = expect_err(
        Store::open_with_backend(
            dir.path().join("lock-holder"),
            OpenMode::ReadWrite,
            BackendKind::Gcs {
                config: fake.config("weft-lock"),
            },
        ),
        "second live GCS writer",
    );
    assert_eq!(err.class_name(), "Busy", "unexpected error: {err}");
    drop(live);
}

#[test]
fn resumable_upload_carries_the_payload_above_the_simple_bound() {
    // Above SIMPLE_UPLOAD_MAX the backend must switch to the resumable
    // protocol; the fake implements both so this path is exercised rather than
    // assumed.
    let fake = FakeGcs::start(false);
    let mut backend = fake.backend("weft-unit");
    let key = Key::tmp("large");
    let body = bytes(5 * 1024 * 1024 + 4096, 9);
    backend
        .put_if_absent(&key, &body)
        .expect("resumable upload");
    assert_eq!(backend.len(&key).unwrap(), body.len() as u64);
    assert_eq!(backend.get(&key).unwrap(), body);
}

#[test]
fn anonymous_emulator_mode_sends_no_authorization_header() {
    let fake = FakeGcs::start(false);
    let mut config = fake.config("weft-unit");
    config.token = None;
    config.anonymous = true;
    let mut backend = GcsBackend::new("<gcs-unit>", config);
    backend
        .put_if_absent(&Key::tmp("anonymous"), b"bytes")
        .unwrap();
    assert!(
        fake.authorizations().is_empty(),
        "anonymous mode sent a credential: {:?}",
        fake.authorizations()
    );
}

#[test]
fn configuration_refuses_an_ambiguous_credential() {
    let err = expect_err(
        GcsConfig::resolve(
            Some("http://127.0.0.1:1".to_owned()),
            Some("weft-unit".to_owned()),
            Some("token".to_owned()),
            None,
            Some(true),
        ),
        "token plus anonymous",
    );
    assert_eq!(err.class_name(), "InvalidArgument");

    let err = expect_err(
        GcsConfig::resolve(
            Some("http://127.0.0.1:1".to_owned()),
            Some("weft-unit".to_owned()),
            None,
            None,
            Some(false),
        ),
        "no credential",
    );
    assert_eq!(err.class_name(), "InvalidArgument");
    assert!(
        err.message().contains("GOOGLE_APPLICATION_CREDENTIALS"),
        "the refusal should name the unsupported path: {err}"
    );
}
