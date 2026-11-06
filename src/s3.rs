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
//!   an owner and an expiry; an expired lease can be replaced with `If-Match`.
//!   Two live writers fail closed with `Busy`.
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

use crate::backend::{BackendStats, Capabilities, Key, KeyKind, StorageBackend};
use crate::error::{Error, Result};
use crate::format::{MAX_CHUNK_SIZE, SUPERBLOCK_BYTES};
use crate::identity::Hash32;

/// S3 max single-PUT object size. The backend advertises the smaller store
/// bound so a single allocation stays bounded.
const MAX_OBJECT_BYTES: u64 = MAX_CHUNK_SIZE * 64;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const LOCK_SCHEMA: &str = "weftfs-s3-lock v1";

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
            lock_lease: Duration::from_secs(lease.max(1)),
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
                    .max(1),
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
    content_length: Option<u64>,
    body: Vec<u8>,
}

struct S3Stats {
    reads: AtomicU64,
    range_reads: AtomicU64,
    bytes_read: AtomicU64,
    writes: AtomicU64,
    bytes_written: AtomicU64,
    conditional_conflicts: AtomicU64,
    lock_acquires: AtomicU64,
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
}

struct LockLease {
    owner: String,
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
        }
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
            consistency: "single-node S3-compatible endpoint; read-after-write and \
                          list-after-write for published keys; conditional writes \
                          (If-None-Match/If-Match) are required and probed by the court"
                .to_owned(),
            publication: "S3 PUT with If-None-Match: * (create-once); superblock \
                          slot via GET+If-Match PUT; both conditional forms are \
                          probed at writable open and a backend that ignores them \
                          is refused"
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
                let bytes = read_bounded(response, content_length, key_path)?;
                return Ok(HttpResponse {
                    status,
                    etag,
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
        let content_length = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        let bytes = read_bounded(response, content_length, key_path)?;
        Ok(HttpResponse {
            status,
            etag,
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
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
                bytes.len()
            )));
        }
        let path = Self::key_path(key)
            .ok_or_else(|| Error::invalid_argument("S3 backend cannot address the store root"))?;
        let mut extra: Vec<(String, String)> = Vec::new();
        match if_match {
            Some(etag) => extra.push(("if-match".to_owned(), etag.to_owned())),
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

        // If-Match must reject a stale ETag, which is how the superblock head
        // is published.
        let etag = current.etag.clone().ok_or_else(|| {
            Error::unsupported_version(
                "S3-compatible endpoint did not return an ETag; refusing a writable store",
            )
        })?;
        if self
            .put_conditional(&key, b"probe-three", Some(&etag))?
            .is_none()
        {
            cleanup();
            return Err(Error::io(
                "S3 conditional probe: a matching If-Match was unexpectedly rejected",
            ));
        }
        if self
            .put_conditional_inner(&key, b"probe-four", Some(&etag), false)?
            .is_some()
        {
            cleanup();
            return Err(Error::unsupported_version(
                "S3-compatible endpoint ignored If-Match; refusing a writable store \
                 because head publication could lose a race silently",
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
        let now = now_ns();
        let owner = format!("{}-{}-{}", std::process::id(), unique_counter(), now);
        let expires = now
            .saturating_add(i64::try_from(self.config.lock_lease.as_nanos()).unwrap_or(i64::MAX));
        let body = format!("{LOCK_SCHEMA}\nowner={owner}\nexpires_ns={expires}\n");
        let key = Key::lock();
        for _ in 0..3 {
            match self.put_conditional(&key, body.as_bytes(), None)? {
                Some(_) => {
                    self.stats.lock_acquires.fetch_add(1, Ordering::Relaxed);
                    *self
                        .lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(LockLease {
                        owner: owner.clone(),
                    });
                    return Ok(());
                }
                None => {
                    let existing = self.get(&key, None)?;
                    if existing.status == 404 {
                        continue;
                    }
                    if existing.status != 200 {
                        return Err(Error::io(format!(
                            "S3 LOCK read returned HTTP {}",
                            existing.status
                        )));
                    }
                    let held_until = parse_lock_expiry(&existing.body);
                    if held_until.map(|value| value > now).unwrap_or(true) {
                        return Err(Error::busy(
                            "store is locked by another S3 writer (live LOCK object)",
                        ));
                    }
                    let etag = existing.etag.clone().unwrap_or_default();
                    match self.put_conditional(&key, body.as_bytes(), Some(&etag))? {
                        Some(_) => {
                            self.note("reclaimed an expired S3 writer lock".to_owned());
                            self.stats.lock_acquires.fetch_add(1, Ordering::Relaxed);
                            *self
                                .lock
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some(LockLease {
                                    owner: owner.clone(),
                                });
                            return Ok(());
                        }
                        None => continue,
                    }
                }
            }
        }
        Err(Error::busy(
            "could not acquire the S3 writer lock after three conditional attempts",
        ))
    }

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
        if let Ok(existing) = self.get(&key, None) {
            if existing.status == 200 {
                let still_ours = String::from_utf8_lossy(&existing.body)
                    .lines()
                    .any(|line| line == format!("owner={}", lease.owner));
                if still_ours {
                    let _ = self.request("DELETE", "LOCK", &[], &[], &[]);
                }
            }
        }
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
        let conflicts = self.stats.conditional_conflicts.load(Ordering::Relaxed);
        if conflicts > 0 {
            out.push(format!(
                "S3 conditional writes observed {conflicts} lost/retried race(s)"
            ));
        }
        out
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
        Ok(self.put_conditional(key, bytes, Some(if_match))?.is_some())
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

fn prefix_for_kind(kind: KeyKind) -> Option<&'static str> {
    match kind {
        KeyKind::Commit => Some("commits/"),
        KeyKind::Object => Some("objects/"),
        KeyKind::Manifest => Some("manifests/"),
        KeyKind::Index => Some("indexes/"),
        KeyKind::SnapshotRef => Some("refs/snapshots/"),
        KeyKind::Tag => Some("refs/tags/"),
        KeyKind::Tmp => Some("tmp/"),
        KeyKind::Access => Some("access/"),
        _ => None,
    }
}

fn classify_relative(kind: KeyKind, relative: &str) -> Option<Key> {
    match kind {
        KeyKind::Object | KeyKind::Manifest => {
            let (shard, name) = relative.split_once('/')?;
            let hash = Hash32::from_lower_hex(name).ok()?;
            if hash.to_hex()[0..2] != *shard {
                return None;
            }
            Some(if kind == KeyKind::Object {
                Key::object(hash)
            } else {
                Key::manifest(hash)
            })
        }
        KeyKind::Index => {
            let (shard, name) = relative.split_once('/')?;
            let stem = name.strip_suffix(".idx")?;
            let commit = Hash32::from_lower_hex(stem).ok()?;
            if commit.to_hex()[0..2] != *shard {
                return None;
            }
            Some(Key::index(commit))
        }
        KeyKind::Commit => {
            let stem = relative.strip_suffix(".commit")?;
            if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(Key::commit(stem.parse::<u64>().ok()?))
        }
        KeyKind::SnapshotRef => Some(Key::snapshot_ref(relative)),
        KeyKind::Tag => Some(Key::tag(relative)),
        KeyKind::Tmp => Some(Key::tmp(relative)),
        KeyKind::Access => match relative {
            "counts.v1" => Some(Key::access_counts()),
            "journal.v1" => Some(Key::access_journal()),
            _ => None,
        },
        _ => None,
    }
}

fn parse_lock_expiry(bytes: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.starts_with(LOCK_SCHEMA) {
        return None;
    }
    text.lines()
        .find_map(|line| line.strip_prefix("expires_ns="))
        .and_then(|value| value.parse::<i64>().ok())
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
    fn lock_expiry_round_trips() {
        let body = format!("{LOCK_SCHEMA}\nowner=x\nexpires_ns=42\n");
        assert_eq!(parse_lock_expiry(body.as_bytes()), Some(42));
        assert_eq!(parse_lock_expiry(b"garbage"), None);
    }
}
