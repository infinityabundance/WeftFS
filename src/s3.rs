//! S3-compatible storage backend (subphase Q4).
//!
//! This is a real object-store backend, not a local directory posing as one.
//! Every durable artifact is addressed by the same logical key as the local
//! layout (`objects/<ab>/<hash>`, `manifests/<ab>/<hash>`,
//! `commits/<gen>.commit`, `indexes/<ab>/<commit>.idx`, `refs/...`), so an S3
//! store and a local store built from the same operations publish byte-identical
//! records.
//!
//! # Protocol mapping
//!
//! * content, manifest, commit, index, ref, quota: `PUT` with
//!   `If-None-Match: *` (create-once). An existing key with identical bytes is
//!   idempotent; different bytes are `Exists`.
//! * head publication: the `superblock` object holds the same two 4096-byte
//!   slots as the local layout. Updating one slot is a whole-object
//!   read-modify-write with `If-Match: <current ETag>`. A lost race returns
//!   `412 Precondition Failed`; the writer retries from a fresh read and
//!   eventually fails closed, leaving the previous head readable.
//! * ranges: `GET` with `Range: bytes=offset-end`.
//! * writer lock: a `LOCK` object created with `If-None-Match: *`. It carries
//!   an owner, a monotonic **fencing token**, a bounded lease deadline and the
//!   head the lease holder is about to publish. An expired lease — or a
//!   released tombstone — can be replaced with `If-Match`, which bumps the
//!   fencing token. Two live writers fail closed with `Busy`.
//!
//! # Fenced single-writer protocol
//!
//! Every publication performs one conditional write on `LOCK` immediately
//! before it writes the superblock slot. That write is `If-Match: <ETag of the
//! bytes this handle last published>`, so it succeeds only while the object
//! still holds *this* writer's fencing token: a successor that reclaimed the
//! lease rewrote the object, which changes its ETag, so the stale writer gets
//! `412` and its publication fails `Busy` without touching the head.
//! The same conditional write names the head (`pending_generation`,
//! `pending_commit`) that is about to be published, and it renews the lease
//! when at most one publication budget remains. A writer stops publishing as
//! soon as its own deadline passes, even if nobody took over.
//!
//! Release is never an unconditional `DELETE` after a read. It is a
//! content-addressed tombstone published with `If-Match` on the ETag the writer
//! observed, so a successor's lock can never be deleted by the writer it
//! replaced. A tombstone is used rather than a conditional `DELETE` because
//! removing the object would reset the fencing token; the release record keeps
//! the token monotonic.
//!
//! # Durability
//!
//! S3 has no `fsync`. `durable_fsync` here means "the configured provider
//! acknowledged the write". The MinIO court backs its data directory with a
//! tmpfs, so it proves protocol behaviour and conditional-write safety, not
//! media or power-loss durability. No AWS, GCS, 3FS, cloud, or multi-node claim
//! is made.
//!
//! Signing is SigV4 over a minimal blocking HTTP client (`ureq`). No non-S3
//! protocol is invented.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::backend::{
    classify_listed_key as classify_relative, list_prefix_path as prefix_for_kind,
};
use crate::backend::{BackendStats, Capabilities, Key, KeyKind, StorageBackend};
use crate::error::{Error, Result};
use crate::format::{MAX_CHUNK_SIZE, SUPERBLOCK_BYTES};
use crate::identity::Hash32;

/// S3 max single-PUT object size. The backend advertises the smaller store
/// bound so a single allocation stays bounded.
const MAX_OBJECT_BYTES: u64 = MAX_CHUNK_SIZE * 64;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const LOCK_SCHEMA: &str = "weftfs-s3-lock v2";
const LOCK_SCHEMA_V1: &str = "weftfs-s3-lock v1";
const RELEASED_SCHEMA: &str = "weftfs-s3-lock-released v1";
/// A lease shorter than this cannot bound a publication, so it is refused.
const MIN_LOCK_LEASE_SECS: u64 = 1;

/// Connection and policy for an S3-compatible endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3Config {
    /// `http://host:port` or `https://host`, optional path prefix.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    /// Path-style addressing (`endpoint/bucket/key`). Default true; required by
    /// the MinIO court. Virtual-hosted style (`bucket.endpoint/key`) is also
    /// implemented.
    pub path_style: bool,
    /// Writer-lock lease. A crashed writer leaves a stale lock that becomes
    /// reclaimable after this duration.
    pub lock_lease: Duration,
    /// Per-request timeout.
    pub request_timeout: Duration,
}

impl S3Config {
    /// Build from `WEFT_S3_*` first, then the standard AWS environment names.
    pub fn from_env() -> Result<S3Config> {
        let endpoint = first_env(&[
            "WEFT_S3_ENDPOINT",
            "AWS_ENDPOINT_URL_S3",
            "AWS_ENDPOINT_URL",
        ])
        .ok_or_else(|| {
            Error::invalid_argument(
                "S3 backend needs an endpoint (`--s3-endpoint`, WEFT_S3_ENDPOINT, \
                 AWS_ENDPOINT_URL_S3, or AWS_ENDPOINT_URL)",
            )
        })?;
        let bucket = first_env(&["WEFT_S3_BUCKET", "AWS_S3_BUCKET"]).ok_or_else(|| {
            Error::invalid_argument("S3 backend needs a bucket (`--s3-bucket` or WEFT_S3_BUCKET)")
        })?;
        let region = first_env(&["WEFT_S3_REGION", "AWS_REGION", "AWS_DEFAULT_REGION"])
            .unwrap_or_else(|| "us-east-1".to_owned());
        let access_key =
            first_env(&["WEFT_S3_ACCESS_KEY", "AWS_ACCESS_KEY_ID"]).ok_or_else(|| {
                Error::invalid_argument(
                    "S3 backend needs an access key (`--s3-access-key` or AWS_ACCESS_KEY_ID)",
                )
            })?;
        let secret_key =
            first_env(&["WEFT_S3_SECRET_KEY", "AWS_SECRET_ACCESS_KEY"]).ok_or_else(|| {
                Error::invalid_argument(
                    "S3 backend needs a secret key (`--s3-secret-key` or AWS_SECRET_ACCESS_KEY)",
                )
            })?;
        let lease = first_env(&["WEFT_S3_LOCK_LEASE_SECS"])
            .and_then(|text| text.parse::<u64>().ok())
            .unwrap_or(900);
        Ok(S3Config {
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            session_token: first_env(&["WEFT_S3_SESSION_TOKEN", "AWS_SESSION_TOKEN"]),
            path_style: true,
            lock_lease: Duration::from_secs(lease.max(MIN_LOCK_LEASE_SECS)),
            request_timeout: Duration::from_secs(60),
        })
    }

    /// Resolve with explicit values taking precedence over the environment.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        endpoint: Option<String>,
        bucket: Option<String>,
        region: Option<String>,
        access_key: Option<String>,
        secret_key: Option<String>,
        session_token: Option<String>,
    ) -> Result<S3Config> {
        let mut config = S3Config {
            endpoint: first_env(&[
                "WEFT_S3_ENDPOINT",
                "AWS_ENDPOINT_URL_S3",
                "AWS_ENDPOINT_URL",
            ])
            .unwrap_or_default(),
            bucket: first_env(&["WEFT_S3_BUCKET", "AWS_S3_BUCKET"]).unwrap_or_default(),
            region: first_env(&["WEFT_S3_REGION", "AWS_REGION", "AWS_DEFAULT_REGION"])
                .unwrap_or_else(|| "us-east-1".to_owned()),
            access_key: first_env(&["WEFT_S3_ACCESS_KEY", "AWS_ACCESS_KEY_ID"]).unwrap_or_default(),
            secret_key: first_env(&["WEFT_S3_SECRET_KEY", "AWS_SECRET_ACCESS_KEY"])
                .unwrap_or_default(),
            session_token: first_env(&["WEFT_S3_SESSION_TOKEN", "AWS_SESSION_TOKEN"]),
            path_style: true,
            lock_lease: Duration::from_secs(
                first_env(&["WEFT_S3_LOCK_LEASE_SECS"])
                    .and_then(|text| text.parse::<u64>().ok())
                    .unwrap_or(900)
                    .max(MIN_LOCK_LEASE_SECS),
            ),
            request_timeout: Duration::from_secs(60),
        };
        if let Some(value) = endpoint {
            config.endpoint = value;
        }
        if let Some(value) = bucket {
            config.bucket = value;
        }
        if let Some(value) = region {
            config.region = value;
        }
        if let Some(value) = access_key {
            config.access_key = value;
        }
        if let Some(value) = secret_key {
            config.secret_key = value;
        }
        if let Some(value) = session_token {
            config.session_token = Some(value);
        }
        if config.endpoint.is_empty() {
            return Err(Error::invalid_argument("S3 backend needs --s3-endpoint"));
        }
        if config.bucket.is_empty() {
            return Err(Error::invalid_argument("S3 backend needs --s3-bucket"));
        }
        if config.access_key.is_empty() {
            return Err(Error::invalid_argument("S3 backend needs --s3-access-key"));
        }
        if config.secret_key.is_empty() {
            return Err(Error::invalid_argument("S3 backend needs --s3-secret-key"));
        }
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.bucket.is_empty() {
            return Err(Error::invalid_argument("S3 bucket must not be empty"));
        }
        if self.endpoint.is_empty() {
            return Err(Error::invalid_argument("S3 endpoint must not be empty"));
        }
        if !self.bucket.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        }) {
            return Err(Error::invalid_argument(format!(
                "S3 bucket `{}` is not a valid DNS-style bucket name",
                self.bucket
            )));
        }
        Ok(())
    }
}

fn first_env(names: &[&str]) -> Option<String> {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct Endpoint {
    scheme: String,
    authority: String,
    base_path: String,
}

impl Endpoint {
    fn parse(text: &str) -> Result<Endpoint> {
        let (scheme, rest) = text.split_once("://").ok_or_else(|| {
            Error::invalid_argument(format!("S3 endpoint `{text}` has no scheme"))
        })?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(Error::invalid_argument(format!(
                "S3 endpoint scheme `{scheme}` is not http or https"
            )));
        }
        let (authority, base_path) = match rest.split_once('/') {
            Some((authority, path)) => {
                let path = path.trim_end_matches('/');
                let base = if path.is_empty() {
                    String::new()
                } else {
                    format!("/{path}")
                };
                (authority.to_owned(), base)
            }
            None => (rest.to_owned(), String::new()),
        };
        if authority.is_empty() {
            return Err(Error::invalid_argument("S3 endpoint has an empty host"));
        }
        Ok(Endpoint {
            scheme,
            authority,
            base_path,
        })
    }
}

struct HttpResponse {
    status: u16,
    etag: Option<String>,
    content_range: Option<String>,
    content_length: Option<u64>,
    body: Vec<u8>,
}

/// Raw result of one signed S3 request, for the independent-implementation
/// compatibility court (subphase R16). This is not part of the storage
/// contract: the store never inspects `content_range` and never branches on
/// anything but the status code and ETag above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub content_range: Option<String>,
    pub content_length: Option<u64>,
    pub body: Vec<u8>,
}

impl RawResponse {
    /// `<Code>` from an S3 XML error body, if the body is one.
    pub fn error_code(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.body);
        let start = text.find("<Code>")? + "<Code>".len();
        let rest = &text[start..];
        let end = rest.find("</Code>")?;
        Some(rest[..end].to_owned())
    }

    /// `<Message>` from an S3 XML error body, if the body is one.
    pub fn error_message(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.body);
        let start = text.find("<Message>")? + "<Message>".len();
        let rest = &text[start..];
        let end = rest.find("</Message>")?;
        Some(rest[..end].to_owned())
    }
}

struct S3Stats {
    reads: AtomicU64,
    range_reads: AtomicU64,
    bytes_read: AtomicU64,
    writes: AtomicU64,
    bytes_written: AtomicU64,
    conditional_conflicts: AtomicU64,
    lock_acquires: AtomicU64,
    /// Conditional lease writes that proved ownership before a publication.
    fence_checks: AtomicU64,
    /// Fence checks that also extended the lease deadline.
    lease_renewals: AtomicU64,
    /// Publications refused because the LOCK no longer named this writer.
    fence_refusals: AtomicU64,
    /// Publications refused because this writer's own deadline had passed.
    lease_expired_publications: AtomicU64,
}

impl Default for S3Stats {
    fn default() -> Self {
        S3Stats {
            reads: AtomicU64::new(0),
            range_reads: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            conditional_conflicts: AtomicU64::new(0),
            lock_acquires: AtomicU64::new(0),
            fence_checks: AtomicU64::new(0),
            lease_renewals: AtomicU64::new(0),
            fence_refusals: AtomicU64::new(0),
            lease_expired_publications: AtomicU64::new(0),
        }
    }
}

/// S3-backed durable store namespace.
pub struct S3Backend {
    root: PathBuf,
    config: S3Config,
    endpoint: Mutex<Option<Endpoint>>,
    agent: Mutex<Option<ureq::Agent>>,
    lock: Mutex<Option<LockLease>>,
    stats: S3Stats,
    diagnostics: Mutex<Vec<String>>,
    probe: Mutex<Option<()>>,
    /// How this endpoint compares `If-Match` against an ETag. Set by the
    /// writable-open probe; never guessed. See [`IfMatchForm`].
    if_match_form: Mutex<IfMatchForm>,
}

/// How the endpoint compares an `If-Match` header with its own ETag.
///
/// HTTP entity tags are quoted (`ETag: "abc"`). AWS S3 and MinIO compare the
/// opaque value and accept either form. Ceph RGW 19.2.0 compares the header
/// bytes literally against its unquoted ETag, so the RFC-correct quoted form
/// is rejected with `412` even when it matches. The writable-open probe
/// measures which form this endpoint honours, requires the matching form to be
/// accepted *and* a stale form to be refused, and records the answer in
/// [`StorageBackend::diagnostics`]. A backend that passes neither form is
/// refused, exactly as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfMatchForm {
    /// Send the ETag exactly as the endpoint returned it (quoted).
    AsReturned,
    /// Strip the surrounding double quotes before sending it.
    Dequoted,
}

impl IfMatchForm {
    /// Apply this form to a raw ETag header value.
    pub fn encode(self, etag: &str) -> String {
        match self {
            IfMatchForm::AsReturned => etag.to_owned(),
            IfMatchForm::Dequoted => strip_etag_quotes(etag),
        }
    }

    fn label(self) -> &'static str {
        match self {
            IfMatchForm::AsReturned => "quoted-as-returned",
            IfMatchForm::Dequoted => "dequoted",
        }
    }
}

fn strip_etag_quotes(etag: &str) -> String {
    let trimmed = etag.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

struct LockLease {
    owner: String,
    /// Monotonic fencing token. Bumped by every takeover, never reused.
    fence: u64,
    /// Number of conditional writes this handle has made to the LOCK object.
    sequence: u64,
    /// Absolute lease deadline recorded in the published object.
    deadline_ns: i64,
    renewed_ns: i64,
    /// ETag the endpoint returned for the bytes published at `sequence`.
    etag: String,
}

/// A parsed `LOCK` object, as published right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedLock {
    pub owner: String,
    /// Monotonic fencing token. `0` means a legacy `weftfs-s3-lock v1` record,
    /// which predates fencing.
    pub fence: u64,
    pub sequence: u64,
    /// Absolute lease deadline (nanoseconds since the Unix epoch).
    pub deadline_ns: i64,
    pub renewed_ns: i64,
    /// The head this writer is publishing (0 / empty when none is in flight).
    pub pending_generation: u64,
    pub pending_commit: String,
    /// True for a released tombstone: no writer holds the lock.
    pub released: bool,
    /// Hex digest of the lock record this tombstone conditionally replaced.
    pub released_from: Option<String>,
}

impl PublishedLock {
    /// Whether a writer still holds a live lease at `now_ns`.
    pub fn is_live(&self, now_ns: i64) -> bool {
        !self.released && now_ns <= self.deadline_ns
    }
}

fn encode_lock(
    owner: &str,
    fence: u64,
    sequence: u64,
    deadline_ns: i64,
    renewed_ns: i64,
    pending: Option<(u64, &Hash32)>,
) -> String {
    let (generation, commit) = match pending {
        Some((generation, commit)) => (generation, commit.to_hex()),
        None => (0, "-".to_owned()),
    };
    format!(
        "{LOCK_SCHEMA}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         lease_deadline_ns={deadline_ns}\nrenewed_ns={renewed_ns}\n\
         pending_generation={generation}\npending_commit={commit}\n"
    )
}

fn encode_released(
    owner: &str,
    fence: u64,
    sequence: u64,
    released_ns: i64,
    released_from: &str,
) -> String {
    format!(
        "{RELEASED_SCHEMA}\nowner={owner}\nfence={fence}\nsequence={sequence}\n\
         released_ns={released_ns}\nreleased_from={released_from}\n"
    )
}

/// Parse a published `LOCK` object. Accepts the fenced v2 record, the legacy v1
/// record (fence 0), and the released tombstone.
fn parse_lock(bytes: &[u8]) -> Option<PublishedLock> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    let schema = lines.next()?;
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        fields.insert(key, value);
    }
    let number = |key: &str| fields.get(key).and_then(|text| text.parse::<i64>().ok());
    match schema {
        LOCK_SCHEMA => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: u64::try_from(number("fence")?).ok()?,
            sequence: u64::try_from(number("sequence")?).ok()?,
            deadline_ns: number("lease_deadline_ns")?,
            renewed_ns: number("renewed_ns")?,
            pending_generation: u64::try_from(number("pending_generation")?).ok()?,
            pending_commit: fields
                .get("pending_commit")
                .map(|text| (*text).to_owned())
                .unwrap_or_else(|| "-".to_owned()),
            released: false,
            released_from: None,
        }),
        LOCK_SCHEMA_V1 => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: 0,
            sequence: 0,
            deadline_ns: number("expires_ns")?,
            renewed_ns: 0,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            released: false,
            released_from: None,
        }),
        RELEASED_SCHEMA => Some(PublishedLock {
            owner: (*fields.get("owner")?).to_owned(),
            fence: u64::try_from(number("fence")?).ok()?,
            sequence: u64::try_from(number("sequence")?).ok()?,
            deadline_ns: number("released_ns")?,
            renewed_ns: number("released_ns")?,
            pending_generation: 0,
            pending_commit: "-".to_owned(),
            released: true,
            released_from: fields.get("released_from").map(|text| (*text).to_owned()),
        }),
        _ => None,
    }
}

impl S3Backend {
    pub fn new(root: impl Into<PathBuf>, config: S3Config) -> S3Backend {
        S3Backend {
            root: root.into(),
            config,
            endpoint: Mutex::new(None),
            agent: Mutex::new(None),
            lock: Mutex::new(None),
            stats: S3Stats::default(),
            diagnostics: Mutex::new(Vec::new()),
            probe: Mutex::new(None),
            if_match_form: Mutex::new(IfMatchForm::AsReturned),
        }
    }

    /// The `If-Match` comparison form this endpoint was measured to honour.
    pub fn if_match_form(&self) -> IfMatchForm {
        *self
            .if_match_form
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Declared guarantees for an S3-compatible endpoint.
    pub fn capabilities() -> Capabilities {
        Capabilities {
            atomic_publish: true,
            // Provider acknowledgement, not a local fsync. The court's MinIO
            // data directory is a tmpfs, so this is not a media-durability claim.
            durable_fsync: true,
            true_range_reads: true,
            max_object_bytes: MAX_OBJECT_BYTES,
            conditional_head: true,
            consistency: "single-node S3-compatible endpoint; read-after-write and \
                          list-after-write for published keys; conditional writes \
                          (If-None-Match/If-Match) are required and probed by the court; \
                          a monotonic fencing token is carried in the LOCK object"
                .to_owned(),
            publication: "S3 PUT with If-None-Match: * (create-once); superblock \
                          slot via GET+If-Match PUT; both conditional forms are \
                          probed at writable open and a backend that ignores them \
                          is refused; every head publication is preceded by one \
                          conditional write on LOCK that carries the fencing token \
                          and names the head, and release is a conditional tombstone"
                .to_owned(),
        }
    }

    pub fn config(&self) -> &S3Config {
        &self.config
    }

    fn note(&self, text: String) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if diagnostics.len() < 32 {
            diagnostics.push(text);
        }
    }

    fn endpoint(&self) -> Result<Endpoint> {
        let mut guard = self
            .endpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(endpoint) = guard.as_ref() {
            return Ok(endpoint.clone());
        }
        self.config.validate()?;
        let endpoint = Endpoint::parse(&self.config.endpoint)?;
        *guard = Some(endpoint.clone());
        Ok(endpoint)
    }

    fn agent(&self) -> ureq::Agent {
        let mut guard = self
            .agent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(agent) = guard.as_ref() {
            return agent.clone();
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(self.config.request_timeout)
            .timeout_connect(Duration::from_secs(10))
            .build();
        *guard = Some(agent.clone());
        agent
    }

    fn host(&self, endpoint: &Endpoint) -> String {
        if self.config.path_style {
            endpoint.authority.clone()
        } else {
            format!("{}.{}", self.config.bucket, endpoint.authority)
        }
    }

    /// Build the encoded URL path for `key_path` (empty means the bucket).
    fn url_path(&self, endpoint: &Endpoint, key_path: &str) -> String {
        let encoded = encode_path(key_path);
        if self.config.path_style {
            if encoded.is_empty() {
                format!("{}/{}", endpoint.base_path, self.config.bucket)
            } else {
                format!("{}/{}/{}", endpoint.base_path, self.config.bucket, encoded)
            }
        } else if encoded.is_empty() {
            if endpoint.base_path.is_empty() {
                "/".to_owned()
            } else {
                endpoint.base_path.clone()
            }
        } else {
            format!("{}/{}", endpoint.base_path, encoded)
        }
    }

    fn request(
        &self,
        method: &str,
        key_path: &str,
        query: &[(String, String)],
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let endpoint = self.endpoint()?;
        let host = self.host(&endpoint);
        let path = self.url_path(&endpoint, key_path);
        let payload_hash = if body.is_empty() {
            EMPTY_SHA256.to_owned()
        } else {
            hex(&sha256(body))
        };
        let (amz_date, date) = amz_now();
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("host".to_owned(), host.clone());
        headers.insert("x-amz-content-sha256".to_owned(), payload_hash.clone());
        headers.insert("x-amz-date".to_owned(), amz_date.clone());
        if let Some(token) = &self.config.session_token {
            headers.insert("x-amz-security-token".to_owned(), token.clone());
        }
        for (name, value) in extra {
            headers.insert(name.to_ascii_lowercase(), (*value).to_owned());
        }
        let canonical_query = canonical_query(query);
        let signed_names: Vec<&str> = headers.keys().map(String::as_str).collect();
        let signed_headers = signed_names.join(";");
        let mut canonical_headers = String::new();
        for name in &signed_names {
            canonical_headers.push_str(name);
            canonical_headers.push(':');
            canonical_headers.push_str(headers[*name].trim());
            canonical_headers.push('\n');
        }
        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.config.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&sha256(canonical_request.as_bytes()))
        );
        let signing_key = signing_key(&self.config.secret_key, &date, &self.config.region);
        let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key
        );
        let url = if canonical_query.is_empty() {
            format!("{}://{}{}", endpoint.scheme, host, path)
        } else {
            format!("{}://{}{}?{}", endpoint.scheme, host, path, canonical_query)
        };

        let agent = self.agent();
        let mut request = agent.request(method, &url);
        for (name, value) in &headers {
            request = request.set(name, value);
        }
        request = request.set("authorization", &authorization);
        let outcome = if body.is_empty() && matches!(method, "GET" | "HEAD" | "DELETE") {
            request.call()
        } else {
            request.send_bytes(body)
        };
        let response = match outcome {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let etag = response.header("etag").map(str::to_owned);
                let content_length = response
                    .header("content-length")
                    .and_then(|value| value.parse::<u64>().ok());
                let content_range = response.header("content-range").map(str::to_owned);
                let bytes = read_bounded(response, content_length, key_path)?;
                return Ok(HttpResponse {
                    status,
                    etag,
                    content_range,
                    content_length,
                    body: bytes,
                });
            }
            Err(err) => {
                return Err(Error::io(format!(
                    "S3 {method} {} failed: {err}",
                    self.describe(key_path)
                )))
            }
        };
        let status = response.status();
        let etag = response.header("etag").map(str::to_owned);
        let content_range = response.header("content-range").map(str::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        let bytes = read_bounded(response, content_length, key_path)?;
        Ok(HttpResponse {
            status,
            etag,
            content_range,
            content_length,
            body: bytes,
        })
    }

    fn describe(&self, key_path: &str) -> String {
        if key_path.is_empty() {
            format!("bucket {}", self.config.bucket)
        } else {
            format!("s3://{}/{}", self.config.bucket, key_path)
        }
    }

    fn key_path(key: &Key) -> Option<String> {
        match key {
            Key::Root | Key::Prefix(_) => None,
            Key::Other { name, .. } => Some(name.clone()),
            other => Some(other.display_path()),
        }
    }

    fn head(&self, key: &Key) -> Result<HttpResponse> {
        let path = Self::key_path(key)
            .ok_or_else(|| Error::invalid_argument("S3 backend cannot address the store root"))?;
        self.request("HEAD", &path, &[], &[], &[])
    }

    fn get(&self, key: &Key, range: Option<(u64, u64)>) -> Result<HttpResponse> {
        let path = Self::key_path(key)
            .ok_or_else(|| Error::invalid_argument("S3 backend cannot address the store root"))?;
        let mut extra: Vec<(String, String)> = Vec::new();
        if let Some((offset, len)) = range {
            if len > 0 {
                let end = offset
                    .checked_add(len - 1)
                    .ok_or_else(|| Error::limit_exceeded("range end overflows u64"))?;
                extra.push(("range".to_owned(), format!("bytes={offset}-{end}")));
            }
        }
        let extra_refs: Vec<(&str, &str)> = extra
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.request("GET", &path, &[], &[], &extra_refs)
    }

    fn put_conditional(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match: Option<&str>,
    ) -> Result<Option<String>> {
        self.put_conditional_inner(key, bytes, if_match, true)
    }

    fn put_conditional_inner(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match: Option<&str>,
        count_conflict: bool,
    ) -> Result<Option<String>> {
        let header = if_match.map(|etag| self.if_match_form().encode(etag));
        self.put_conditional_header(key, bytes, header, count_conflict)
    }

    /// `if_match_value` is the *literal* value sent in `If-Match`; `None` sends
    /// `If-None-Match: *`. Only the probe passes a value that was not encoded
    /// through [`IfMatchForm`].
    fn put_conditional_header(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match_value: Option<String>,
        count_conflict: bool,
    ) -> Result<Option<String>> {
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
                bytes.len()
            )));
        }
        let path = Self::key_path(key)
            .ok_or_else(|| Error::invalid_argument("S3 backend cannot address the store root"))?;
        let mut extra: Vec<(String, String)> = Vec::new();
        match if_match_value {
            Some(etag) => extra.push(("if-match".to_owned(), etag)),
            None => extra.push(("if-none-match".to_owned(), "*".to_owned())),
        }
        let extra_refs: Vec<(&str, &str)> = extra
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let response = self.request("PUT", &path, &[], bytes, &extra_refs)?;
        match response.status {
            200 | 201 | 204 => {
                self.stats.writes.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                Ok(Some(response.etag.unwrap_or_default()))
            }
            412 => {
                if count_conflict {
                    self.stats
                        .conditional_conflicts
                        .fetch_add(1, Ordering::Relaxed);
                }
                Ok(None)
            }
            403 => Err(Error::unauthorized(format!(
                "S3 denied PUT {}",
                self.describe(&path)
            ))),
            status => Err(Error::io(format!(
                "S3 PUT {} returned HTTP {status}: {}",
                self.describe(&path),
                response_body(&response.body)
            ))),
        }
    }

    /// Ensure the configured bucket exists. Idempotent.
    pub fn ensure_bucket(&self) -> Result<()> {
        let response = self.request("HEAD", "", &[], &[], &[])?;
        match response.status {
            200 | 204 => Ok(()),
            404 => {
                let body = if self.config.region == "us-east-1" {
                    Vec::new()
                } else {
                    format!(
                        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                         <LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                        xml_escape(&self.config.region)
                    )
                    .into_bytes()
                };
                let response = self.request("PUT", "", &[], &body, &[])?;
                match response.status {
                    200 | 204 => Ok(()),
                    // A concurrent creator won; that is the desired outcome.
                    409 => Ok(()),
                    status => Err(Error::io(format!(
                        "S3 CreateBucket {} returned HTTP {status}: {}",
                        self.config.bucket,
                        response_body(&response.body)
                    ))),
                }
            }
            status => Err(Error::io(format!(
                "S3 HEAD bucket {} returned HTTP {status}: {}",
                self.config.bucket,
                response_body(&response.body)
            ))),
        }
    }

    /// Prove that the endpoint honours both conditional-write forms the store
    /// depends on. A backend that ignores them is refused, never downgraded.
    fn probe_conditional_writes(&self) -> Result<()> {
        {
            let guard = self
                .probe
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.is_some() {
                return Ok(());
            }
        }
        let key = Key::tmp(format!(
            "conditional-probe-{}-{}",
            std::process::id(),
            unique_counter()
        ));
        let path = Self::key_path(&key).expect("tmp key is addressable");
        let cleanup = || {
            let _ = self.request("DELETE", &path, &[], &[], &[]);
        };

        // If-None-Match: * must create once and reject the second create.
        if self.put_conditional(&key, b"probe-one", None)?.is_none() {
            cleanup();
            return Err(Error::io("S3 conditional probe: unexpected existing key"));
        }
        if self
            .put_conditional_inner(&key, b"probe-two", None, false)?
            .is_some()
        {
            cleanup();
            return Err(Error::unsupported_version(
                "S3-compatible endpoint ignored If-None-Match: *; refusing a writable store \
                 because create-once publication cannot be guaranteed",
            ));
        }
        let current = self.get(&key, None)?;
        if current.status != 200 || current.body != b"probe-one" {
            cleanup();
            return Err(Error::io(
                "S3 conditional probe: first writer's bytes are not readable",
            ));
        }

        // If-Match must accept a matching ETag and reject a stale one, which is
        // how the superblock head is published. Entity tags are quoted, so the
        // RFC-correct value is the string the endpoint returned. Ceph RGW
        // compares the header bytes literally against its unquoted ETag, so a
        // rejected matching write is retried with the de-quoted value. The
        // accepted form is only kept when a stale value in the *same* form is
        // refused and the winner's bytes survive; otherwise the endpoint is
        // refused exactly as before.
        let etag = current.etag.clone().ok_or_else(|| {
            Error::unsupported_version(
                "S3-compatible endpoint did not return an ETag; refusing a writable store",
            )
        })?;
        let dequoted = strip_etag_quotes(&etag);
        let as_returned =
            self.put_conditional_header(&key, b"probe-three", Some(etag.clone()), false)?;
        let (form, matching_value) = if as_returned.is_some() {
            (IfMatchForm::AsReturned, etag.clone())
        } else if dequoted != etag {
            match self.put_conditional_header(
                &key,
                b"probe-three",
                Some(dequoted.clone()),
                false,
            )? {
                Some(_) => (IfMatchForm::Dequoted, dequoted),
                None => {
                    cleanup();
                    return Err(Error::unsupported_version(format!(
                        "S3-compatible endpoint rejected a matching If-Match in both forms \
                         (as returned {etag:?} and dequoted {dequoted:?}); refusing a writable \
                         store because head publication cannot be proven"
                    )));
                }
            }
        } else {
            cleanup();
            return Err(Error::io(
                "S3 conditional probe: a matching If-Match was unexpectedly rejected",
            ));
        };
        let before = self.get(&key, None)?;
        if before.status != 200 || before.body != b"probe-three" {
            cleanup();
            return Err(Error::io(
                "S3 conditional probe: the matching conditional write is not readable",
            ));
        }
        if self
            .put_conditional_header(&key, b"probe-four", Some(matching_value), false)?
            .is_some()
        {
            cleanup();
            return Err(Error::unsupported_version(
                "S3-compatible endpoint ignored If-Match; refusing a writable store \
                 because head publication could lose a race silently",
            ));
        }
        let after = self.get(&key, None)?;
        if after.status != 200 || after.body != b"probe-three" {
            cleanup();
            return Err(Error::io(
                "S3 conditional probe: a refused stale If-Match changed the object",
            ));
        }
        *self
            .if_match_form
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = form;
        if form == IfMatchForm::Dequoted {
            self.note(format!(
                "endpoint compares If-Match literally: ETag {etag} is accepted only when the \
                 surrounding quotes are stripped; using the dequoted form for every \
                 conditional write (matching accepted, stale refused, winner bytes kept)"
            ));
        }
        cleanup();
        let mut guard = self
            .probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(());
        Ok(())
    }

    fn acquire_lock(&self) -> Result<()> {
        // The writer lock is the first conditional write of a writable open
        // (the store locks before it runs the plane probe), so negotiate the
        // If-Match form here. Skipping this would make an endpoint that
        // compares entity tags literally reject every expired-lease reclaim.
        self.probe_conditional_writes()?;
        let now = now_ns();
        let owner = format!("{}-{}-{}", std::process::id(), unique_counter(), now);
        let lease_ns = self.lease_ns();
        let deadline = now.saturating_add(lease_ns);
        let key = Key::lock();
        for _ in 0..4 {
            let existing = self.get(&key, None)?;
            match existing.status {
                404 => {
                    // No record at all: create it once. The first lease holder
                    // takes fencing token 1.
                    let body = encode_lock(&owner, 1, 1, deadline, now, None);
                    if let Some(etag) = self.put_conditional(&key, body.as_bytes(), None)? {
                        self.install_lease(owner, 1, 1, deadline, now, etag);
                        return Ok(());
                    }
                    // Another writer created it between the GET and the PUT.
                    continue;
                }
                200 => {
                    let Some(observed) = parse_lock(&existing.body) else {
                        return Err(Error::busy(
                            "store LOCK object is unreadable; refusing a writable open rather \
                             than guessing whether a writer holds it",
                        ));
                    };
                    if observed.is_live(now) {
                        // The refusal text is a fixed sentence on purpose: the
                        // R13 court asserts that every interlocked open returns
                        // the same non-disclosing message. The operational
                        // detail (owner, fencing token, remaining lease) goes to
                        // diagnostics, never to the caller.
                        self.note(format!(
                            "writable open refused: the LOCK is live (owner {} fence {}, lease ends \
                             in {} ms)",
                            observed.owner,
                            observed.fence,
                            (observed.deadline_ns - now) / 1_000_000
                        ));
                        return Err(Error::busy(
                            "store is locked by another S3 writer (live LOCK object)",
                        ));
                    }
                    // Expired lease or a released tombstone: take over with the
                    // next fencing token, conditional on the exact record read.
                    // A lost race is retried from a fresh read, never blind.
                    let fence = observed.fence.checked_add(1).ok_or_else(|| {
                        Error::limit_exceeded("S3 fencing token would overflow u64")
                    })?;
                    let body = encode_lock(&owner, fence, 1, deadline, now, None);
                    let Some(etag) = existing.etag else {
                        return Err(Error::unsupported_version(
                            "S3-compatible endpoint did not return an ETag for the writer lock; \
                             refusing a writable store because a fenced reclaim cannot be proven",
                        ));
                    };
                    match self.put_conditional(&key, body.as_bytes(), Some(&etag))? {
                        Some(etag) => {
                            self.note(format!(
                                "{} S3 writer lock: fence {} -> {}",
                                if observed.released {
                                    "reclaimed a released"
                                } else {
                                    "reclaimed an expired"
                                },
                                observed.fence,
                                fence
                            ));
                            self.install_lease(owner, fence, 1, deadline, now, etag);
                            return Ok(());
                        }
                        None => continue,
                    }
                }
                status => return Err(Error::io(format!("S3 LOCK read returned HTTP {status}"))),
            }
        }
        Err(Error::busy(
            "could not acquire the S3 writer lock after four conditional attempts",
        ))
    }

    fn install_lease(
        &self,
        owner: String,
        fence: u64,
        sequence: u64,
        deadline_ns: i64,
        renewed_ns: i64,
        etag: String,
    ) {
        self.stats.lock_acquires.fetch_add(1, Ordering::Relaxed);
        *self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(LockLease {
            owner,
            fence,
            sequence,
            deadline_ns,
            renewed_ns,
            etag,
        });
    }

    /// Release the lease with a content-addressed tombstone published under
    /// `If-Match` on the ETag this writer observed.
    ///
    /// Never an unconditional `DELETE` after a read: if the lock was replaced
    /// between the read and this conditional write, the write fails and the
    /// successor's lock is left byte-identical. Every failure is a note, never
    /// a silent unconditional delete.
    fn release_lock(&self) {
        let lease = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(lease) = lease else {
            return;
        };
        let key = Key::lock();
        let observed = match self.get(&key, None) {
            Ok(observed) => observed,
            Err(err) => {
                self.note(format!("could not read LOCK to release it: {err}"));
                return;
            }
        };
        if observed.status == 404 {
            return;
        }
        if observed.status != 200 {
            self.note(format!(
                "could not read LOCK to release it: HTTP {}",
                observed.status
            ));
            return;
        }
        let Some(record) = parse_lock(&observed.body) else {
            self.note("refusing to release an unreadable LOCK record".to_owned());
            return;
        };
        if record.owner != lease.owner || record.fence != lease.fence {
            self.note(format!(
                "refusing to release the S3 writer lock: it now names owner {} fence {} and this \
                 handle holds fence {}; the conditional release was not attempted",
                record.owner, record.fence, lease.fence
            ));
            return;
        }
        let Some(etag) = observed.etag else {
            self.note("refusing to release the S3 writer lock: no ETag to condition on".to_owned());
            return;
        };
        let released_from = sha256_hex(&observed.body);
        let tombstone = encode_released(
            &lease.owner,
            lease.fence,
            lease.sequence.saturating_add(1),
            now_ns(),
            &released_from,
        );
        match self.put_conditional(&key, tombstone.as_bytes(), Some(&etag)) {
            Ok(Some(_)) => self.note(format!(
                "released the S3 writer lock with a conditional tombstone (fence {})",
                lease.fence
            )),
            Ok(None) => self.note(format!(
                "refusing to release the S3 writer lock: it was replaced between the read and the \
                 conditional tombstone (this writer held fence {}); the successor's lock is intact",
                lease.fence
            )),
            Err(err) => self.note(format!(
                "conditional release of the S3 writer lock failed, nothing was deleted: {err}"
            )),
        }
    }

    /// Prove this handle may publish right now, renewing the lease when one
    /// publication budget remains.
    pub(crate) fn fence_lease(&self) -> Result<()> {
        self.fence_lease_for(None)
    }

    /// The same conditional write, additionally naming the head that is about
    /// to be published.
    pub(crate) fn fence_head(&self, generation: u64, commit: &Hash32) -> Result<()> {
        self.fence_lease_for(Some((generation, commit)))
    }

    fn fence_lease_for(&self, pending: Option<(u64, &Hash32)>) -> Result<()> {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = guard.as_mut() else {
            return Err(Error::busy(
                "S3 writer holds no lease; refusing to publish without a fenced lease",
            ));
        };
        let now = now_ns();
        if now >= lease.deadline_ns {
            let owner = lease.owner.clone();
            let fence = lease.fence;
            let expired_ms = (now - lease.deadline_ns) / 1_000_000;
            // Observation only. The decision below never acts on it.
            let observed = self.read_lock_record().ok().flatten();
            self.stats
                .lease_expired_publications
                .fetch_add(1, Ordering::Relaxed);
            return Err(Error::busy(format!(
                "S3 writer lease deadline passed {expired_ms} ms ago (owner {owner} fence {fence}); \
                 publication refused before the head was touched{}",
                describe_observed(&observed, &owner, fence)
            )));
        }
        let budget = self.publication_budget_ns();
        let renew = now.saturating_add(budget) >= lease.deadline_ns;
        let deadline = if renew {
            now.saturating_add(self.lease_ns())
        } else {
            lease.deadline_ns
        };
        let sequence = lease.sequence.saturating_add(1);
        let body = encode_lock(&lease.owner, lease.fence, sequence, deadline, now, pending);
        // This is the fence. `If-Match` names the bytes this handle published
        // last, so it succeeds only while the object still carries this
        // writer's fencing token. A successor's reclaim rewrote the object, so
        // this fails with 412 and nothing is published.
        match self.put_conditional(&Key::lock(), body.as_bytes(), Some(&lease.etag))? {
            Some(etag) => {
                lease.sequence = sequence;
                lease.deadline_ns = deadline;
                lease.renewed_ns = now;
                lease.etag = etag;
                self.stats.fence_checks.fetch_add(1, Ordering::Relaxed);
                if renew {
                    self.stats.lease_renewals.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            }
            None => {
                let observed = self.read_lock_record().ok().flatten();
                self.stats.fence_refusals.fetch_add(1, Ordering::Relaxed);
                Err(Error::busy(format!(
                    "S3 writer lease was taken over: the conditional fence CAS for owner {} \
                     fence {} was refused, so the publication was abandoned without writing the \
                     head{}",
                    lease.owner,
                    lease.fence,
                    describe_observed(&observed, &lease.owner, lease.fence)
                )))
            }
        }
    }

    /// The published `LOCK` record, read fresh. Read-only: the store never acts
    /// on this value, it only reports it.
    pub fn read_lock_record(&self) -> Result<Option<PublishedLock>> {
        let response = self.get(&Key::lock(), None)?;
        match response.status {
            200 => Ok(parse_lock(&response.body)),
            404 => Ok(None),
            status => Err(Error::io(format!("S3 LOCK read returned HTTP {status}"))),
        }
    }

    /// Court/operator reset: replace the published `LOCK` with a released
    /// tombstone under a conditional write. Never called by the store, which
    /// only ever releases a lease it still holds.
    pub fn release_stale_lease(&self) -> Result<Option<PublishedLock>> {
        let response = self.get(&Key::lock(), None)?;
        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            return Err(Error::io(format!(
                "S3 LOCK read returned HTTP {}",
                response.status
            )));
        }
        let Some(record) = parse_lock(&response.body) else {
            return Err(Error::corrupt("published LOCK record is unreadable"));
        };
        let Some(etag) = response.etag else {
            return Err(Error::unsupported_version(
                "S3-compatible endpoint did not return an ETag for the writer lock",
            ));
        };
        let fence = record
            .fence
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("S3 fencing token would overflow u64"))?;
        let tombstone = encode_released(
            &record.owner,
            fence,
            record.sequence.saturating_add(1),
            now_ns(),
            &sha256_hex(&response.body),
        );
        match self.put_conditional(&Key::lock(), tombstone.as_bytes(), Some(&etag))? {
            Some(_) => Ok(self.read_lock_record()?),
            None => Err(Error::busy(
                "the LOCK was replaced while the court reset it; nothing was deleted",
            )),
        }
    }

    fn lease_ns(&self) -> i64 {
        i64::try_from(self.config.lock_lease.as_nanos()).unwrap_or(i64::MAX)
    }

    /// The window a single publication may still occupy. A writer renews when
    /// one budget remains, and a successor may not take over before the
    /// recorded deadline, so a head publication always finishes inside the
    /// lease it was started under.
    fn publication_budget_ns(&self) -> i64 {
        (self.lease_ns() / 2).max(1)
    }

    fn list_objects(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list_prefix requires a Key::Prefix selector",
            ));
        };
        let Some(prefix_path) = prefix_for_kind(*kind) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<(String, String)> = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("prefix".to_owned(), prefix_path.to_owned()),
                ("max-keys".to_owned(), "1000".to_owned()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let response = self.request("GET", "", &query, &[], &[])?;
            match response.status {
                200 => {}
                404 => return Ok(out),
                status => {
                    return Err(Error::io(format!(
                        "S3 ListObjectsV2 returned HTTP {status}: {}",
                        response_body(&response.body)
                    )))
                }
            }
            let text = String::from_utf8_lossy(&response.body);
            for full in xml_values(&text, "Key") {
                let relative = full.strip_prefix(prefix_path).unwrap_or(&full);
                match classify_relative(*kind, relative) {
                    Some(key) => out.push(key),
                    None => out.push(Key::Other {
                        kind: *kind,
                        name: full,
                    }),
                }
            }
            let truncated = xml_values(&text, "IsTruncated")
                .first()
                .map(|value| value == "true")
                .unwrap_or(false);
            if !truncated {
                break;
            }
            token = xml_values(&text, "NextContinuationToken")
                .into_iter()
                .next();
            if token.is_none() {
                break;
            }
        }
        out.sort();
        Ok(out)
    }
}

impl Drop for S3Backend {
    fn drop(&mut self) {
        self.release_lock();
    }
}

impl StorageBackend for S3Backend {
    fn label(&self) -> &'static str {
        "s3"
    }

    fn capabilities(&self) -> Capabilities {
        S3Backend::capabilities()
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let response = self.get(key, Some((offset, len)))?;
        let bytes = match response.status {
            200 => {
                // The server ignored Range. Slice the whole object.
                let start = usize::try_from(offset)
                    .map_err(|_| Error::limit_exceeded("range offset does not fit memory"))?;
                if start > response.body.len() {
                    return Err(Error::invalid_argument(format!(
                        "read offset {offset} is beyond {}",
                        key.display_path()
                    )));
                }
                let end = response
                    .body
                    .len()
                    .min(start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX)));
                response.body[start..end].to_vec()
            }
            206 => response.body,
            404 => {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )))
            }
            416 => {
                // End-of-object is not an error, matching the local backend;
                // only an offset strictly beyond the object is rejected.
                let total = self.len(key)?;
                if offset == total {
                    return Ok(Vec::new());
                }
                return Err(Error::invalid_argument(format!(
                    "read offset {offset} is beyond {} ({total} bytes)",
                    key.display_path()
                )));
            }
            403 => {
                return Err(Error::unauthorized(format!(
                    "S3 denied {}",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "S3 range GET {} returned HTTP {status}",
                    key.display_path()
                )))
            }
        };
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats.range_reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    fn read_all(&self, key: &Key) -> Result<Vec<u8>> {
        let response = self.get(key, None)?;
        let bytes = match response.status {
            200 => response.body,
            404 => {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )))
            }
            403 => {
                return Err(Error::unauthorized(format!(
                    "S3 denied {}",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "S3 GET {} returned HTTP {status}",
                    key.display_path()
                )))
            }
        };
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "object {} is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
                key.display_path(),
                bytes.len()
            )));
        }
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_read
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    fn write_at(&mut self, key: &Key, offset: u64, bytes: &[u8]) -> Result<()> {
        // Only the two-slot superblock uses write_at. S3 has no in-place range
        // write: read the whole object, patch the slot, and publish the result
        // with If-Match so a lost race fails instead of overwriting.
        for _ in 0..8 {
            let current = self.get(key, None)?;
            if current.status == 404 {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )));
            }
            if current.status != 200 {
                return Err(Error::io(format!(
                    "S3 read-modify-write {} returned HTTP {}",
                    key.display_path(),
                    current.status
                )));
            }
            let etag = current.etag.clone().ok_or_else(|| {
                Error::io("S3 endpoint did not return an ETag for a conditional update")
            })?;
            let mut updated = current.body;
            if updated.len() as u64 != SUPERBLOCK_BYTES as u64 && key.kind() == KeyKind::Superblock
            {
                return Err(Error::corrupt(format!(
                    "superblock is {} bytes, expected {SUPERBLOCK_BYTES}",
                    updated.len()
                )));
            }
            let start = usize::try_from(offset)
                .map_err(|_| Error::limit_exceeded("write offset does not fit memory"))?;
            let end = start
                .checked_add(bytes.len())
                .ok_or_else(|| Error::limit_exceeded("write end overflows memory"))?;
            if end > updated.len() {
                return Err(Error::invalid_argument(format!(
                    "write range {offset}..{end} exceeds {} bytes",
                    updated.len()
                )));
            }
            updated[start..end].copy_from_slice(bytes);
            if let Some(_) = self.put_conditional(key, &updated, Some(&etag))? {
                return Ok(());
            }
            self.note(format!(
                "conditional superblock update for {} lost a race; retrying from a fresh read",
                key.display_path()
            ));
        }
        Err(Error::busy(
            "superblock conditional update kept losing to another writer",
        ))
    }

    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        match self.put_conditional(key, bytes, None)? {
            Some(_) => Ok(()),
            None => {
                // The key exists. Identical bytes are idempotent; different
                // bytes under a content-addressed name are a real conflict.
                let existing = self.read_all(key)?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(Error::exists(format!(
                        "key {} already exists with different bytes",
                        key.display_path()
                    )))
                }
            }
        }
    }

    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        // Mutable store record (quota policy). Replace atomically; retry a
        // conditional replace rather than clobbering a concurrent update.
        for _ in 0..8 {
            let current = self.get(key, None)?;
            match current.status {
                200 => {
                    let etag = current.etag.clone().ok_or_else(|| {
                        Error::io("S3 endpoint did not return an ETag for a replacement")
                    })?;
                    if self.put_conditional(key, bytes, Some(&etag))?.is_some() {
                        return Ok(());
                    }
                }
                404 => {
                    if self.put_conditional(key, bytes, None)?.is_some() {
                        return Ok(());
                    }
                }
                status => {
                    return Err(Error::io(format!(
                        "S3 replace {} returned HTTP {status}",
                        key.display_path()
                    )))
                }
            }
        }
        Err(Error::busy(format!(
            "conditional replacement of {} kept losing",
            key.display_path()
        )))
    }

    fn compare_and_swap(&mut self, key: &Key, expected: Option<&str>, bytes: &[u8]) -> Result<()> {
        // Native conditional write: read the record and its ETag, require the
        // caller's content token to match the bytes just read, then publish
        // with If-Match (or If-None-Match: * when the caller expects absence).
        // A 412 is a lost race and stays `Busy`.
        let current = self.get(key, None)?;
        let (present, etag) = match current.status {
            200 => (true, current.etag.clone()),
            404 => (false, None),
            403 => {
                return Err(Error::unauthorized(format!(
                    "S3 denied {} during a compare-and-swap",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "S3 compare-and-swap read {} returned HTTP {status}",
                    key.display_path()
                )))
            }
        };
        let token = if present {
            Some(crate::planes::control_token(&current.body))
        } else {
            None
        };
        if token.as_deref() != expected {
            return Err(Error::busy(format!(
                "control record {} changed since it was read; compare-and-swap refused",
                key.display_path()
            )));
        }
        match self.put_conditional(key, bytes, etag.as_deref())? {
            Some(_) => Ok(()),
            None => Err(Error::busy(format!(
                "S3 conditional replacement lost a race for {}",
                key.display_path()
            ))),
        }
    }

    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()> {
        let len = usize::try_from(len)
            .map_err(|_| Error::limit_exceeded("zeroed object does not fit the address space"))?;
        if len as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "zeroed object is {len} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound"
            )));
        }
        self.put_atomic(key, &vec![0u8; len])
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        let path = Self::key_path(key)
            .ok_or_else(|| Error::invalid_argument("S3 backend cannot address the store root"))?;
        // S3 DELETE is idempotent and returns 204 for a missing key. The
        // trait requires NotFound, so probe first; GC then still observes an
        // honest report while safe retries remain safe.
        let head = self.request("HEAD", &path, &[], &[], &[])?;
        match head.status {
            200 | 204 => {}
            404 => {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )))
            }
            403 => {
                return Err(Error::unauthorized(format!(
                    "S3 denied {}",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "S3 HEAD {} returned HTTP {status}",
                    self.describe(&path)
                )))
            }
        }
        let response = self.request("DELETE", &path, &[], &[], &[])?;
        match response.status {
            200 | 204 => Ok(()),
            404 => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
            status => Err(Error::io(format!(
                "S3 DELETE {} returned HTTP {status}",
                self.describe(&path)
            ))),
        }
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        S3Backend::list_objects(self, prefix)
    }

    fn sync(&mut self, _key: &Key) -> Result<()> {
        // A successful S3 PUT is acknowledged by the provider; there is no
        // separate fsync primitive.
        Ok(())
    }

    fn sync_dir(&mut self, _key: &Key) -> Result<()> {
        Ok(())
    }

    fn len(&self, key: &Key) -> Result<u64> {
        let response = self.head(key)?;
        match response.status {
            200 | 204 => response
                .content_length
                .ok_or_else(|| Error::io("S3 HEAD response is missing Content-Length")),
            404 => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
            status => Err(Error::io(format!(
                "S3 HEAD {} returned HTTP {status}",
                key.display_path()
            ))),
        }
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        let response = self.head(key)?;
        match response.status {
            200 | 204 => Ok(true),
            404 => Ok(false),
            403 => Err(Error::unauthorized(format!(
                "S3 denied {}",
                key.display_path()
            ))),
            status => Err(Error::io(format!(
                "S3 HEAD {} returned HTTP {status}",
                key.display_path()
            ))),
        }
    }

    fn modified(&self, _key: &Key) -> Result<Option<SystemTime>> {
        // The HEAD response carries HTTP-date Last-Modified. The store does not
        // depend on it; returning None keeps the primitive honest.
        Ok(None)
    }

    fn init_layout(&mut self) -> Result<()> {
        self.ensure_bucket()?;
        self.probe_conditional_writes()
    }

    fn ensure_layout(&mut self) -> Result<()> {
        self.ensure_bucket()?;
        self.probe_conditional_writes()
    }

    fn acquire_writer_lock(&mut self) -> Result<()> {
        self.acquire_lock()
    }

    fn verify_write_lease(&mut self) -> Result<()> {
        self.fence_lease()
    }

    fn fence_head_publication(&mut self, generation: u64, commit: &Hash32) -> Result<()> {
        self.fence_head(generation, commit)
    }

    fn clean_stale_tmp(&mut self, _grace_secs: u64) -> Result<()> {
        // There is no staging namespace: every PUT is atomic under its final
        // key. The only `tmp/` objects are conditional-write probe keys, which
        // a crashed process can leave behind; the writer lock is held here, so
        // deleting all of them is safe.
        let keys = self.list_objects(&Key::prefix(KeyKind::Tmp))?;
        for key in keys {
            let _ = self.delete(&key);
        }
        Ok(())
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn stats(&self) -> BackendStats {
        BackendStats {
            reads: self.stats.reads.load(Ordering::Relaxed),
            range_reads: self.stats.range_reads.load(Ordering::Relaxed),
            bytes_read: self.stats.bytes_read.load(Ordering::Relaxed),
            writes: self.stats.writes.load(Ordering::Relaxed),
            // Includes lock and superblock publication, like the local backend
            // counts every published file.
            bytes_written: self.stats.bytes_written.load(Ordering::Relaxed),
            link_publications: 0,
            rename_publications: 0,
            rename_fallbacks: 0,
        }
    }

    fn diagnostics(&self) -> Vec<String> {
        let mut out = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let probed = self
            .probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some();
        if probed {
            out.push(format!(
                "S3 If-Match form measured at open: {}",
                self.if_match_form().label()
            ));
        }
        let conflicts = self.stats.conditional_conflicts.load(Ordering::Relaxed);
        if conflicts > 0 {
            out.push(format!(
                "S3 conditional writes observed {conflicts} lost/retried race(s)"
            ));
        }
        out
    }

    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        let blobs = crate::planes::probe_blob_plane(self)?;
        let commits = crate::planes::probe_commit_plane(self)?;
        Ok(Some(crate::planes::PlaneProbes { blobs, commits }))
    }
}

impl S3Backend {
    /// Test hook: perform a conditional whole-object PUT with a caller-supplied
    /// `If-Match` ETag. Returns `false` on `412 Precondition Failed`.
    #[doc(hidden)]
    pub fn conditional_put_for_test(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match: &str,
    ) -> Result<bool> {
        // A caller-supplied ETag is the value the endpoint returned. Negotiate
        // the comparison form exactly as a writable open does, so the hook
        // exercises the same wire form the store uses.
        self.probe_conditional_writes()?;
        Ok(self.put_conditional(key, bytes, Some(if_match))?.is_some())
    }

    /// Court hook: send one signed request and return the raw response.
    ///
    /// Used only by `examples/s3_compat_matrix.rs` to characterise the
    /// conditional-write, range, and listing behaviour of an independent
    /// S3-compatible server. Storage decisions never read this surface.
    #[doc(hidden)]
    pub fn raw_request_for_court(
        &self,
        method: &str,
        key_path: &str,
        query: &[(String, String)],
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<RawResponse> {
        let response = self.request(method, key_path, query, body, extra)?;
        Ok(RawResponse {
            status: response.status,
            etag: response.etag,
            content_range: response.content_range,
            content_length: response.content_length,
            body: response.body,
        })
    }

    /// Test hook: current ETag for a key.
    #[doc(hidden)]
    pub fn etag_for_test(&self, key: &Key) -> Result<String> {
        let response = self.head(key)?;
        match response.status {
            200 | 204 => response
                .etag
                .ok_or_else(|| Error::io("S3 endpoint did not return an ETag")),
            404 => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
            status => Err(Error::io(format!(
                "S3 HEAD {} returned HTTP {status}",
                key.display_path()
            ))),
        }
    }
}

fn describe_observed(observed: &Option<PublishedLock>, owner: &str, fence: u64) -> String {
    match observed {
        Some(record) if record.owner != owner || record.fence != fence => format!(
            "; the LOCK now names owner {} fence {}",
            record.owner, record.fence
        ),
        Some(record) if record.released => "; the LOCK is a released tombstone".to_owned(),
        Some(_) => "; the LOCK still names this writer, but its own lease expired".to_owned(),
        None => "; the LOCK is absent".to_owned(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn now_ns() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    i64::try_from(nanos).unwrap_or(i64::MAX)
}

fn unique_counter() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn read_bounded(
    response: ureq::Response,
    content_length: Option<u64>,
    what: &str,
) -> Result<Vec<u8>> {
    if let Some(len) = content_length {
        if len > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "S3 object `{what}` is {len} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound"
            )));
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_OBJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format!("S3 response read: {err}")))?;
    if bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Err(Error::limit_exceeded(format!(
            "S3 object `{what}` exceeded the backend's {MAX_OBJECT_BYTES}-byte bound"
        )));
    }
    Ok(bytes)
}

fn response_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(240).collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    let mut outer = Sha256::new();
    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for index in 0..64 {
        ipad[index] = block[index] ^ 0x36;
        opad[index] = block[index] ^ 0x5c;
    }
    inner.update(ipad);
    inner.update(data);
    outer.update(opad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn signing_key(secret: &str, date: &str, region: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    hmac_sha256(&k_service, b"aws4_request")
}

/// RFC 3986 encoding with `/` preserved. AWS requires this exact spelling.
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn canonical_query(query: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| (encode_component(key), encode_component(value)))
        .collect();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn amz_now() -> (String, String) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let date = format!("{year:04}{month:02}{day:02}");
    let amz = format!("{date}T{hour:02}{minute:02}{second:02}Z");
    (amz, date)
}

/// Howard Hinnant's `civil_from_days` (proleptic Gregorian, days since epoch).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d as u32)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") => u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32),
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn xml_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(&close) else {
            break;
        };
        out.push(xml_unescape(&rest[..end]));
        rest = &rest[end + close.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigv4_encoding_and_dates_are_canonical() {
        assert_eq!(encode_path("objects/ab/ff"), "objects/ab/ff");
        assert_eq!(encode_component("a b"), "a%20b");
        assert_eq!(encode_component("a/b"), "a%2Fb");
        // 1970-01-01 is day 0; 2024-01-01 is day 19723.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn xml_values_decode_minio_escapes() {
        let xml = "<Key>a&amp;b</Key><Key>c&#34;d</Key>";
        assert_eq!(xml_values(xml, "Key"), vec!["a&b", "c\"d"]);
    }

    #[test]
    fn lock_record_round_trips_and_parses_legacy_v1() {
        let commit = Hash32::from_bytes([7u8; 32]);
        let body = encode_lock("owner-a", 3, 9, 1_000, 900, Some((12, &commit)));
        let record = parse_lock(body.as_bytes()).expect("v2 record parses");
        assert_eq!(record.owner, "owner-a");
        assert_eq!(record.fence, 3);
        assert_eq!(record.sequence, 9);
        assert_eq!(record.deadline_ns, 1_000);
        assert_eq!(record.renewed_ns, 900);
        assert_eq!(record.pending_generation, 12);
        assert_eq!(record.pending_commit, commit.to_hex());
        assert!(!record.released);
        assert!(record.is_live(999));
        assert!(record.is_live(1_000));
        assert!(!record.is_live(1_001));

        // A lease with no head in flight still round-trips.
        let idle = encode_lock("owner-a", 3, 10, 1_000, 950, None);
        let record = parse_lock(idle.as_bytes()).expect("idle record parses");
        assert_eq!(record.pending_generation, 0);
        assert_eq!(record.pending_commit, "-");

        // Legacy v1 records predate fencing and are read as fence 0.
        let legacy = format!("{LOCK_SCHEMA_V1}\nowner=x\nexpires_ns=42\n");
        let record = parse_lock(legacy.as_bytes()).expect("v1 record parses");
        assert_eq!(record.owner, "x");
        assert_eq!(record.fence, 0);
        assert_eq!(record.deadline_ns, 42);
        assert!(record.is_live(41));
        assert!(!record.is_live(43));

        assert_eq!(parse_lock(b"garbage"), None);
        assert_eq!(parse_lock(b"weftfs-s3-lock v2\nowner=x\n"), None);
    }

    #[test]
    fn released_tombstone_records_are_content_addressed() {
        let body = encode_released("owner-a", 4, 11, 5_000, "deadbeef");
        let record = parse_lock(body.as_bytes()).expect("tombstone parses");
        assert!(record.released);
        assert_eq!(record.fence, 4);
        assert_eq!(record.sequence, 11);
        assert_eq!(record.deadline_ns, 5_000);
        assert_eq!(record.released_from.as_deref(), Some("deadbeef"));
        // A released record is never live, whatever the clock says.
        assert!(!record.is_live(0));
        assert!(!record.is_live(i64::MAX));
    }

    #[test]
    fn if_match_forms_encode_etags() {
        // The wire value an ETag header carries, quoted, is what a
        // spec-conforming server compares. Ceph RGW compares the raw header
        // bytes against its unquoted ETag, which the dequoted form matches.
        assert_eq!(strip_etag_quotes("\"abc123\""), "abc123");
        assert_eq!(strip_etag_quotes("abc123"), "abc123");
        assert_eq!(strip_etag_quotes("\"\""), "");
        assert_eq!(strip_etag_quotes("W/\"abc\""), "W/\"abc\"");
        assert_eq!(IfMatchForm::AsReturned.encode("\"abc123\""), "\"abc123\"");
        assert_eq!(IfMatchForm::Dequoted.encode("\"abc123\""), "abc123");
        assert_eq!(IfMatchForm::Dequoted.encode("abc123"), "abc123");
        assert_eq!(IfMatchForm::AsReturned.label(), "quoted-as-returned");
        assert_eq!(IfMatchForm::Dequoted.label(), "dequoted");
    }

    #[test]
    fn a_fresh_backend_defaults_to_the_as_returned_form() {
        let config = S3Config {
            endpoint: "http://127.0.0.1:9000".to_owned(),
            bucket: "bucket".to_owned(),
            region: "us-east-1".to_owned(),
            access_key: "key".to_owned(),
            secret_key: "secret".to_owned(),
            session_token: None,
            path_style: true,
            lock_lease: Duration::from_secs(900),
            request_timeout: Duration::from_secs(30),
        };
        let backend = S3Backend::new("<test>", config);
        assert_eq!(backend.if_match_form(), IfMatchForm::AsReturned);
        // Nothing claims a measured form before the probe runs.
        assert!(backend.diagnostics().is_empty());
    }
}
