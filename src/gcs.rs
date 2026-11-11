//! Google Cloud Storage backend (subphase R14).
//!
//! This is a real GCS JSON-API backend, addressed the same way as every other
//! WeftFS backend: content, manifests, commit records, indexes, and refs keep
//! the normative logical key layout (`objects/<ab>/<hash>`,
//! `manifests/<ab>/<hash>`, `commits/<gen>.commit`, `indexes/<ab>/<commit>.idx`,
//! `refs/...`). Two stores built from the same operations publish
//! byte-identical records, whichever backend carries them.
//!
//! # Why a separate backend for GCS
//!
//! GCS's concurrency model is *generation preconditions*, not ETags:
//!
//! * `ifGenerationMatch=0` publishes a key only if it does not exist
//!   (create-once).
//! * `ifGenerationMatch=<n>` replaces a key only if it is still at generation
//!   `n` (compare-and-swap).
//!
//! Neither form is `If-None-Match`/`If-Match`, so the S3 backend cannot be
//! reused by renaming headers: this module maps the same plane contract onto
//! GCS's own primitive. A failed precondition is HTTP `412`, which maps to the
//! same `Busy` class the S3 backend reports.
//!
//! # Fenced writer (subphase R18)
//!
//! The single writer holds a lease in a `LOCK` object. The record is the same
//! field set as the S3 backend's (`owner`, a monotonic **fencing token**,
//! `sequence`, an absolute `lease_deadline_ns`, `renewed_ns`, and the head
//! being published); only the conditional primitive differs. Generation
//! numbers are GCS's own token, so:
//!
//! * takeover of an expired lease or a released tombstone is an upload with
//!   `ifGenerationMatch=<generation observed at read>`;
//! * the **fence** before every publication is one upload of `LOCK` with
//!   `ifGenerationMatch=<the generation this handle last published>`, which
//!   proves the object still carries this writer's token and renews the lease
//!   when one publication budget remains;
//! * release is a content-addressed tombstone written with
//!   `ifGenerationMatch`, never a delete; a lock that now names another writer
//!   is never touched;
//! * a writer whose own deadline has passed refuses to publish before any
//!   head object is touched.
//!
//! `LOCK` is never deleted after a read. The exact atomicity statement (the
//! fence and the head compare-and-swap are two operations on two objects) is
//! the same one `bench/RESULTS-fencing.md` records for S3.
//!
//! # Authentication
//!
//! Requests carry `Authorization: Bearer <token>`. The token is supplied by
//! configuration or environment (`--gcs-token`, `WEFT_GCS_ACCESS_TOKEN`,
//! `GOOGLE_OAUTH_ACCESS_TOKEN`); an emulator court may run anonymous
//! (`--gcs-anonymous`, `WEFT_GCS_ANONYMOUS=1`). Service-account JSON key
//! signing is **not** implemented: it needs RS256, and this crate has no RSA
//! dependency. Setting `GOOGLE_APPLICATION_CREDENTIALS` therefore does not
//! silently authenticate anything — the backend fails closed with an explicit
//! message. Tokens are never written to diagnostics, receipts, or error
//! messages; [`GcsConfig`]'s `Debug` redacts them.
//!
//! # Durability
//!
//! GCS has no `fsync`. `durable_fsync` here means "the provider acknowledged
//! the write". A memory-backed emulator court proves protocol behaviour,
//! generation-precondition safety, and head compare-and-swap; it proves
//! nothing about media, power loss, or multi-node consistency. No cloud,
//! fleet, or production claim is made.
//!
//! # Upload paths
//!
//! Payloads up to [`SIMPLE_UPLOAD_MAX`] use the JSON API's simple media upload.
//! Larger payloads use the documented two-step resumable upload: a session is
//! initiated (carrying the generation precondition) and the bytes are sent to
//! the returned session URI in one bounded request. Both paths are exact; the
//! selection is a protocol rule, not a fallback.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::backend::{
    classify_listed_key as classify_relative, list_prefix_path as prefix_for_kind, BackendStats,
    Capabilities, Key, KeyKind, StorageBackend,
};
use crate::error::{Error, Result};
use crate::format::{MAX_CHUNK_SIZE, SUPERBLOCK_BYTES};
use crate::planes::{
    control_token, BlobCapabilities, BlobPlane, CommitCapabilities, CommitPlane, ControlRecord,
};
use crate::service_account::{ServiceAccountAuth, ServiceAccountKey, DEFAULT_SCOPE};

/// Largest single object the backend accepts. Matches the S3 backend's bound so
/// a store built on either has the same object-size limit.
const MAX_OBJECT_BYTES: u64 = MAX_CHUNK_SIZE * 64;

/// Largest payload sent with the JSON API's simple media upload. Above this the
/// provider requires a resumable upload; the backend switches protocol, it does
/// not truncate.
const SIMPLE_UPLOAD_MAX: u64 = 5 * 1024 * 1024;

/// Bound on a JSON metadata/list/error body. A listing page is a few hundred
/// kilobytes; this keeps a hostile endpoint from making us allocate an object.
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;

/// Default JSON API origin.
const DEFAULT_ENDPOINT: &str = "https://storage.googleapis.com";
const LOCK_SCHEMA: &str = "weftfs-gcs-lock v3";
const LOCK_SCHEMA_V2: &str = "weftfs-gcs-lock v2";
const LOCK_SCHEMA_V1: &str = "weftfs-gcs-lock v1";
const RELEASED_SCHEMA: &str = "weftfs-gcs-lock-released v2";
const RELEASED_SCHEMA_V1: &str = "weftfs-gcs-lock-released v1";
const LOCK_SCHEMAS: crate::writer_fence::Schemas = crate::writer_fence::Schemas {
    v3: LOCK_SCHEMA,
    v2: LOCK_SCHEMA_V2,
    v1: LOCK_SCHEMA_V1,
    released_v2: RELEASED_SCHEMA,
    released_v1: RELEASED_SCHEMA_V1,
};
/// A lease shorter than this cannot bound a publication, so it is refused.
const MIN_LOCK_LEASE_SECS: u64 = 1;

/// Connection and policy for a GCS (or GCS-compatible) endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct GcsConfig {
    /// `http://host:port` or `https://host`, optional path prefix.
    pub endpoint: String,
    pub bucket: String,
    /// OAuth2 bearer token. Never logged.
    pub token: Option<String>,
    /// Service-account key used to mint bearer tokens with the RS256
    /// JWT-bearer flow. Never logged, never rendered.
    pub service_account: Option<Arc<ServiceAccountKey>>,
    /// OAuth2 scope requested by the assertion.
    pub scope: String,
    /// Project used to create the bucket when it does not exist.
    pub project: Option<String>,
    /// Emulator mode: send no `Authorization` header. Never combined with a
    /// token, because a caller that supplied both has told us two different
    /// things about the endpoint.
    pub anonymous: bool,
    /// Writer-lock lease. A crashed writer leaves a stale lock that becomes
    /// reclaimable after this duration.
    pub lock_lease: Duration,
    /// Per-request timeout.
    pub request_timeout: Duration,
}

impl fmt::Debug for GcsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The bearer token is a secret: show only whether one is present.
        f.debug_struct("GcsConfig")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field(
                "service_account",
                &self.service_account.as_ref().map(|key| key.client_email()),
            )
            .field(
                "service_account_key",
                &self.service_account.as_ref().map(|_| "<redacted>"),
            )
            .field("scope", &self.scope)
            .field("project", &self.project)
            .field("anonymous", &self.anonymous)
            .field("lock_lease", &self.lock_lease)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl GcsConfig {
    /// Build from the environment only.
    pub fn from_env() -> Result<GcsConfig> {
        GcsConfig::resolve(None, None, None, None, None, None, None)
    }

    /// Resolve with explicit values taking precedence over the environment.
    ///
    /// Exactly one of three credential forms must resolve:
    ///
    /// * a bearer token (`--gcs-token`, `WEFT_GCS_ACCESS_TOKEN`,
    ///   `GOOGLE_OAUTH_ACCESS_TOKEN`);
    /// * a service-account key (`--gcs-credentials`, `WEFT_GCS_CREDENTIALS`,
    ///   or `GOOGLE_APPLICATION_CREDENTIALS`), signed with RS256;
    /// * `--gcs-anonymous` for an emulator.
    ///
    /// Supplying two forms is refused rather than guessed at.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        endpoint: Option<String>,
        bucket: Option<String>,
        token: Option<String>,
        project: Option<String>,
        anonymous: Option<bool>,
        credentials: Option<String>,
        scope: Option<String>,
    ) -> Result<GcsConfig> {
        let mut config = GcsConfig {
            endpoint: first_env(&["WEFT_GCS_ENDPOINT"])
                .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned()),
            bucket: first_env(&["WEFT_GCS_BUCKET"]).unwrap_or_default(),
            token: first_env(&["WEFT_GCS_ACCESS_TOKEN", "GOOGLE_OAUTH_ACCESS_TOKEN"]),
            service_account: None,
            scope: first_env(&["WEFT_GCS_SCOPE"]).unwrap_or_else(|| DEFAULT_SCOPE.to_owned()),
            project: first_env(&["WEFT_GCS_PROJECT", "GOOGLE_CLOUD_PROJECT"]),
            anonymous: env_flag("WEFT_GCS_ANONYMOUS"),
            lock_lease: Duration::from_secs(
                first_env(&["WEFT_GCS_LOCK_LEASE_SECS"])
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
        if let Some(value) = token {
            config.token = Some(value);
        }
        if let Some(value) = project {
            config.project = Some(value);
        }
        if let Some(value) = anonymous {
            config.anonymous = value;
        }
        if let Some(value) = scope {
            if !value.is_empty() {
                config.scope = value;
            }
        }
        let credentials_path = credentials
            .or_else(|| first_env(&["WEFT_GCS_CREDENTIALS", "GOOGLE_APPLICATION_CREDENTIALS"]));
        if let Some(path) = credentials_path {
            let key = ServiceAccountKey::from_path(std::path::Path::new(&path)).map_err(|err| {
                Error::new(
                    err.class(),
                    format!("service-account key from `{path}`: {}", err.message()),
                )
            })?;
            if config.project.is_none() {
                config.project = key.project_id().map(str::to_owned);
            }
            config.service_account = Some(Arc::new(key));
        }
        if config.bucket.is_empty() {
            return Err(Error::invalid_argument(
                "GCS backend needs a bucket (`--gcs-bucket` or WEFT_GCS_BUCKET)",
            ));
        }
        let forms = [
            config.token.is_some(),
            config.service_account.is_some(),
            config.anonymous,
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if forms > 1 {
            return Err(Error::invalid_argument(
                "GCS backend was given more than one credential form (bearer token, \
                 service-account key, anonymous); refusing to guess which one the endpoint should \
                 see",
            ));
        }
        if forms == 0 {
            return Err(Error::invalid_argument(
                "GCS backend needs one credential form: a bearer token (`--gcs-token`, \
                 WEFT_GCS_ACCESS_TOKEN, GOOGLE_OAUTH_ACCESS_TOKEN), a service-account key \
                 (`--gcs-credentials`, WEFT_GCS_CREDENTIALS, GOOGLE_APPLICATION_CREDENTIALS), or \
                 `--gcs-anonymous` for an emulator",
            ));
        }
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.bucket.is_empty() {
            return Err(Error::invalid_argument("GCS bucket must not be empty"));
        }
        if self.bucket.len() > 222 {
            return Err(Error::invalid_argument(
                "GCS bucket name is longer than the 222-byte service limit",
            ));
        }
        if !self.bucket.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'-'
                || byte == b'_'
                || byte == b'.'
        }) {
            return Err(Error::invalid_argument(format!(
                "GCS bucket `{}` is not a valid bucket name",
                self.bucket
            )));
        }
        if self.endpoint.is_empty() {
            return Err(Error::invalid_argument("GCS endpoint must not be empty"));
        }
        Endpoint::parse(&self.endpoint)?;
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

fn env_flag(name: &str) -> bool {
    first_env(&[name])
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
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
            Error::invalid_argument(format!("GCS endpoint `{text}` has no scheme"))
        })?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(Error::invalid_argument(format!(
                "GCS endpoint scheme `{scheme}` is not http or https"
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
            return Err(Error::invalid_argument("GCS endpoint has an empty host"));
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
    location: Option<String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct GcsStats {
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
    /// Publication writes that carried a new head (fence + head in one CAS).
    head_publications: AtomicU64,
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
    /// GCS generation the endpoint assigned to the bytes published at
    /// `sequence`. This is the token the fence conditions on.
    generation: u64,
    /// The head this handle last published, carried forward by every LOCK
    /// rewrite so a crash never loses the attestation.
    head: Option<(u64, crate::identity::Hash32, u64)>,
}

/// Metadata the store depends on: the object's length and its GCS generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectMeta {
    size: u64,
    generation: u64,
}

/// The generation precondition on one upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Precondition {
    /// Publish only if the object does not exist (`ifGenerationMatch=0`).
    Missing,
    /// Replace only if the object is still at this generation.
    Matches(u64),
}

impl Precondition {
    fn parameter(self) -> String {
        match self {
            Precondition::Missing => "0".to_owned(),
            Precondition::Matches(generation) => generation.to_string(),
        }
    }
}

/// GCS-backed durable store namespace.
pub struct GcsBackend {
    root: PathBuf,
    config: GcsConfig,
    endpoint: Mutex<Option<Endpoint>>,
    agent: Mutex<Option<ureq::Agent>>,
    /// Lazily built service-account token source. `None` for a bearer token or
    /// an anonymous emulator court.
    auth: Mutex<Option<ServiceAccountAuth>>,
    lock: Mutex<Option<LockLease>>,
    stats: GcsStats,
    diagnostics: Mutex<Vec<String>>,
    /// Set once the generation-precondition probe has been observed to pass.
    probed: Mutex<Option<()>>,
}

impl GcsBackend {
    pub fn new(root: impl Into<PathBuf>, config: GcsConfig) -> GcsBackend {
        GcsBackend {
            root: root.into(),
            config,
            endpoint: Mutex::new(None),
            agent: Mutex::new(None),
            auth: Mutex::new(None),
            lock: Mutex::new(None),
            stats: GcsStats::default(),
            diagnostics: Mutex::new(Vec::new()),
            probed: Mutex::new(None),
        }
    }

    /// Declared guarantees for a GCS-compatible endpoint.
    pub fn capabilities() -> Capabilities {
        Capabilities {
            atomic_publish: true,
            // Provider acknowledgement, not a local fsync. Nothing here claims
            // media or power-loss durability.
            durable_fsync: true,
            true_range_reads: true,
            max_object_bytes: MAX_OBJECT_BYTES,
            conditional_head: true,
            consistency: "single-endpoint GCS-compatible store; read-after-write and \
                          list-after-write for published keys; generation preconditions \
                          (ifGenerationMatch) are required and probed at writable open"
                .to_owned(),
            publication: "GCS JSON API upload with ifGenerationMatch=0 (create-once) or \
                          ifGenerationMatch=<observed generation> (compare-and-swap); both \
                          forms and a stale-generation refusal are probed before any commit \
                          is published. The writer lease is a fenced record whose takeover, \
                          renewal, and release are all generation-conditioned; the fence is \
                          the last upload before the head compare-and-swap. Credentials are a \
                          bearer token or a service-account RS256 JWT-bearer assertion \
                          exchanged at the key's token_uri"
                .to_owned(),
        }
    }

    pub fn config(&self) -> &GcsConfig {
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

    fn origin(&self) -> Result<String> {
        let endpoint = self.endpoint()?;
        Ok(format!(
            "{}://{}{}",
            endpoint.scheme, endpoint.authority, endpoint.base_path
        ))
    }

    fn object_url(&self, name: &str, query: &[(&str, String)]) -> Result<String> {
        let mut url = format!(
            "{}/storage/v1/b/{}/o/{}",
            self.origin()?,
            pct(&self.config.bucket),
            pct(name)
        );
        let query = query_string(query);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        Ok(url)
    }

    fn upload_url(&self, query: &[(&str, String)]) -> Result<String> {
        Ok(format!(
            "{}/upload/storage/v1/b/{}/o?{}",
            self.origin()?,
            pct(&self.config.bucket),
            query_string(query)
        ))
    }

    fn bucket_url(&self, query: &[(&str, String)]) -> Result<String> {
        let mut url = format!(
            "{}/storage/v1/b/{}",
            self.origin()?,
            pct(&self.config.bucket)
        );
        let query = query_string(query);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        Ok(url)
    }

    /// The bucket *collection*: bucket creation is a `POST` here, with the new
    /// bucket's name in the query string, not in the path.
    fn bucket_collection_url(&self, query: &[(&str, String)]) -> Result<String> {
        Ok(format!(
            "{}/storage/v1/b?{}",
            self.origin()?,
            query_string(query)
        ))
    }

    /// The object collection of one bucket: listing lives at `.../b/<bucket>/o`.
    fn object_collection_url(&self, query: &[(&str, String)]) -> Result<String> {
        Ok(format!(
            "{}/storage/v1/b/{}/o?{}",
            self.origin()?,
            pct(&self.config.bucket),
            query_string(query)
        ))
    }

    /// The `Authorization` value for this request, minting a service-account
    /// token when that is the configured credential form.
    ///
    /// A token exchange is a network call, so this can fail with `Io` or
    /// `Unauthorized`; the caller propagates it rather than sending an
    /// unauthenticated request that would be refused anyway.
    fn authorization(&self) -> Result<Option<String>> {
        if let Some(token) = &self.config.token {
            return Ok(Some(format!("Bearer {token}")));
        }
        let Some(key) = &self.config.service_account else {
            return Ok(None);
        };
        let mut guard = self
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(ServiceAccountAuth::new(
                key.clone(),
                self.config.scope.clone(),
            ));
        }
        let token = guard.as_ref().expect("just installed").token()?;
        Ok(Some(format!("Bearer {}", token.access_token())))
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&[u8]>,
        cap: u64,
    ) -> Result<HttpResponse> {
        let agent = self.agent();
        let mut request = agent.request(method, url);
        if let Some(authorization) = self.authorization()? {
            request = request.set("authorization", &authorization);
        }
        request = request.set("user-agent", "weftfs-gcs/1");
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let outcome = match body {
            Some(bytes) => request.send_bytes(bytes),
            None => request.call(),
        };
        let response = match outcome {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let location = response.header("location").map(str::to_owned);
                let content_length = response
                    .header("content-length")
                    .and_then(|value| value.parse::<u64>().ok());
                let body = read_bounded(response, content_length, cap, url)?;
                return Ok(HttpResponse {
                    status,
                    location,
                    body,
                });
            }
            Err(err) => {
                // A transport failure stays `Io`: an unreachable or refusing
                // endpoint is not a damaged store.
                return Err(Error::io(format!("GCS {method} {url} failed: {err}")));
            }
        };
        let status = response.status();
        let location = response.header("location").map(str::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        let body = read_bounded(response, content_length, cap, url)?;
        Ok(HttpResponse {
            status,
            location,
            body,
        })
    }

    fn object_name(key: &Key) -> Result<String> {
        match key {
            Key::Root | Key::Prefix(_) => Err(Error::invalid_argument(
                "GCS backend cannot address a namespace prefix as an object",
            )),
            Key::Other { name, .. } => Ok(name.clone()),
            other => Ok(other.display_path()),
        }
    }

    /// Object metadata, or `Ok(None)` when the key does not exist.
    fn metadata(&self, key: &Key) -> Result<Option<ObjectMeta>> {
        let name = Self::object_name(key)?;
        let url = self.object_url(&name, &[])?;
        let response = self.request("GET", &url, &[], None, MAX_JSON_BYTES)?;
        match response.status {
            200 => {
                let value = json::parse(&response.body)
                    .map_err(|err| Error::corrupt(format!("GCS metadata for `{name}`: {err}")))?;
                let generation = value
                    .get("generation")
                    .and_then(JsonValue::as_u64)
                    .ok_or_else(|| {
                        Error::unsupported_version(format!(
                            "GCS metadata for `{name}` has no integer `generation`; refusing a \
                             writable store whose compare-and-swap would have no token"
                        ))
                    })?;
                let size = value.get("size").and_then(JsonValue::as_u64).unwrap_or(0);
                Ok(Some(ObjectMeta { size, generation }))
            }
            404 => Ok(None),
            401 | 403 => Err(Error::unauthorized(format!(
                "GCS denied metadata for `{name}`"
            ))),
            status => Err(Error::io(format!(
                "GCS metadata for `{name}` returned HTTP {status}: {}",
                response_body(&response.body)
            ))),
        }
    }

    /// Read the whole object's bytes.
    fn media(&self, key: &Key, range: Option<(u64, u64)>) -> Result<HttpResponse> {
        let name = Self::object_name(key)?;
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some((offset, len)) = range {
            if len > 0 {
                let end = offset
                    .checked_add(len - 1)
                    .ok_or_else(|| Error::limit_exceeded("range end overflows u64"))?;
                headers.push(("range", format!("bytes={offset}-{end}")));
            }
        }
        let url = self.object_url(&name, &[("alt", "media".to_owned())])?;
        self.request("GET", &url, &headers, None, MAX_OBJECT_BYTES)
    }

    /// Read an object's bytes together with the generation those bytes came
    /// from. The generation is confirmed after the read so a concurrent
    /// replacement can never be mistaken for the observed state.
    fn read_with_generation(&self, key: &Key) -> Result<Option<(Vec<u8>, u64)>> {
        for _ in 0..4 {
            let Some(before) = self.metadata(key)? else {
                return Ok(None);
            };
            let response = self.media(key, None)?;
            let bytes = match response.status {
                200 => response.body,
                404 => return Ok(None),
                401 | 403 => {
                    return Err(Error::unauthorized(format!(
                        "GCS denied reading {}",
                        key.display_path()
                    )))
                }
                status => {
                    return Err(Error::io(format!(
                        "GCS read {} returned HTTP {status}",
                        key.display_path()
                    )))
                }
            };
            if bytes.len() as u64 != before.size {
                continue;
            }
            let Some(after) = self.metadata(key)? else {
                continue;
            };
            if after.generation == before.generation {
                return Ok(Some((bytes, after.generation)));
            }
        }
        Err(Error::busy(format!(
            "GCS object {} kept changing while it was being read",
            key.display_path()
        )))
    }

    /// Publish `bytes` under `key` subject to `precondition`.
    ///
    /// Returns `true` when the bytes were published and `false` when a
    /// generation precondition failed (HTTP 412). Everything else is an error.
    fn upload(&self, key: &Key, bytes: &[u8], precondition: Precondition) -> Result<bool> {
        Ok(self.upload_tracked(key, bytes, precondition)?.is_some())
    }

    /// Publish `bytes` and report the generation the endpoint assigned them.
    ///
    /// `Ok(None)` means the precondition failed (HTTP 412). The generation is
    /// the token a fenced writer conditions its next write on, so it must name
    /// *these* bytes. The JSON API returns the object resource in the upload
    /// reply; if an endpoint omits it, the generation is confirmed by a
    /// read-after-write whose bytes must be exactly the bytes just published.
    /// An endpoint that can do neither is refused rather than guessed at.
    fn upload_tracked(
        &self,
        key: &Key,
        bytes: &[u8],
        precondition: Precondition,
    ) -> Result<Option<u64>> {
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "object is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
                bytes.len()
            )));
        }
        let name = Self::object_name(key)?;
        let mut params: Vec<(&str, String)> =
            vec![("name", name.clone()), ("uploadType", "media".to_owned())];
        params.push(("ifGenerationMatch", precondition.parameter()));
        let response = if bytes.len() as u64 <= SIMPLE_UPLOAD_MAX {
            let url = self.upload_url(&params)?;
            self.request(
                "POST",
                &url,
                &[("content-type", "application/octet-stream".to_owned())],
                Some(bytes),
                MAX_JSON_BYTES,
            )?
        } else {
            self.resumable_upload(&name, bytes, precondition)?
        };
        match response.status {
            200 | 201 | 204 => {
                self.stats.writes.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if let Some(generation) = response_generation(&response.body) {
                    return Ok(Some(generation));
                }
                // The reply carried no object resource. Confirm the generation
                // by re-reading, and require the bytes to be exactly ours: a
                // successor's replacement would differ, so its generation can
                // never be mistaken for this writer's token.
                match self.read_with_generation(key)? {
                    Some((observed, generation)) if observed == bytes => Ok(Some(generation)),
                    Some(_) => Err(Error::busy(format!(
                        "GCS object {} was replaced while its generation was being confirmed",
                        key.display_path()
                    ))),
                    None => Err(Error::busy(format!(
                        "GCS object {} disappeared while its generation was being confirmed",
                        key.display_path()
                    ))),
                }
            }
            412 => {
                self.stats
                    .conditional_conflicts
                    .fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            401 | 403 => Err(Error::unauthorized(format!("GCS denied writing `{name}`"))),
            413 => Err(Error::limit_exceeded(format!(
                "GCS rejected the {}-byte object `{name}` as too large for one request",
                bytes.len()
            ))),
            status => Err(Error::io(format!(
                "GCS upload of `{name}` returned HTTP {status}: {}",
                response_body(&response.body)
            ))),
        }
    }

    /// Two-step resumable upload for payloads above the simple-upload bound.
    /// The generation precondition travels on the session-initiation request,
    /// which is where the provider evaluates it.
    fn resumable_upload(
        &self,
        name: &str,
        bytes: &[u8],
        precondition: Precondition,
    ) -> Result<HttpResponse> {
        let mut params: Vec<(&str, String)> = vec![
            ("name", name.to_owned()),
            ("uploadType", "resumable".to_owned()),
        ];
        params.push(("ifGenerationMatch", precondition.parameter()));
        let url = self.upload_url(&params)?;
        let init = self.request(
            "POST",
            &url,
            &[
                ("content-type", "application/octet-stream".to_owned()),
                ("x-upload-content-length", bytes.len().to_string()),
            ],
            Some(&[]),
            MAX_JSON_BYTES,
        )?;
        match init.status {
            200 | 201 => {}
            // The precondition is evaluated at session initiation, so a 412 is
            // reported exactly like a failed simple upload.
            412 => {
                self.stats
                    .conditional_conflicts
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(init);
            }
            401 | 403 => {
                return Err(Error::unauthorized(format!(
                    "GCS denied initiating an upload for `{name}`"
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "GCS resumable initiation for `{name}` returned HTTP {status}: {}",
                    response_body(&init.body)
                )))
            }
        }
        let location = init.location.ok_or_else(|| {
            Error::unsupported_version(format!(
                "GCS resumable initiation for `{name}` returned no session URI; refusing to \
                 continue rather than guessing an upload endpoint"
            ))
        })?;
        let response = self.request(
            "PUT",
            &location,
            &[("content-type", "application/octet-stream".to_owned())],
            Some(bytes),
            MAX_JSON_BYTES,
        )?;
        if response.status == 308 {
            return Err(Error::io(format!(
                "GCS resumable upload for `{name}` ended incomplete (HTTP 308); no partial object \
                 is treated as published"
            )));
        }
        Ok(response)
    }

    fn delete_object(&self, key: &Key) -> Result<()> {
        let name = Self::object_name(key)?;
        let url = self.object_url(&name, &[])?;
        let response = self.request("DELETE", &url, &[], None, MAX_JSON_BYTES)?;
        match response.status {
            200 | 204 => Ok(()),
            404 => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
            401 | 403 => Err(Error::unauthorized(format!("GCS denied deleting `{name}`"))),
            status => Err(Error::io(format!(
                "GCS delete of `{name}` returned HTTP {status}: {}",
                response_body(&response.body)
            ))),
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
            let mut params: Vec<(&str, String)> = vec![
                ("prefix", prefix_path.to_owned()),
                ("maxResults", "1000".to_owned()),
            ];
            if let Some(token) = &token {
                params.push(("pageToken", token.clone()));
            }
            let url = self.object_collection_url(&params)?;
            let response = self.request("GET", &url, &[], None, MAX_JSON_BYTES)?;
            match response.status {
                200 => {}
                404 => return Ok(out),
                401 | 403 => {
                    return Err(Error::unauthorized(format!(
                        "GCS denied listing `{prefix_path}`"
                    )))
                }
                status => {
                    return Err(Error::io(format!(
                        "GCS list returned HTTP {status}: {}",
                        response_body(&response.body)
                    )))
                }
            }
            let value = json::parse(&response.body)
                .map_err(|err| Error::corrupt(format!("GCS listing `{prefix_path}`: {err}")))?;
            if let Some(items) = value.get("items").and_then(JsonValue::as_array) {
                for item in items {
                    let Some(full) = item.get("name").and_then(JsonValue::as_text) else {
                        continue;
                    };
                    let relative = full.strip_prefix(prefix_path).unwrap_or(full);
                    out.push(
                        classify_relative(*kind, relative).unwrap_or_else(|| Key::Other {
                            kind: *kind,
                            name: full.to_owned(),
                        }),
                    );
                }
            }
            token = value
                .get("nextPageToken")
                .and_then(JsonValue::as_text)
                .map(str::to_owned);
            if token.is_none() {
                break;
            }
        }
        out.sort();
        Ok(out)
    }

    /// Ensure the configured bucket exists. Idempotent.
    ///
    /// A GCS bucket is created inside a project, so creation is only attempted
    /// when a project was configured; otherwise a missing bucket is reported,
    /// never invented.
    pub fn ensure_bucket(&self) -> Result<()> {
        let url = self.bucket_url(&[])?;
        let response = self.request("GET", &url, &[], None, MAX_JSON_BYTES)?;
        match response.status {
            200 => return Ok(()),
            404 => {}
            401 | 403 => {
                return Err(Error::unauthorized(format!(
                    "GCS denied reading bucket `{}`",
                    self.config.bucket
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "GCS bucket metadata returned HTTP {status}: {}",
                    response_body(&response.body)
                )))
            }
        }
        let Some(project) = &self.config.project else {
            return Err(Error::not_found(format!(
                "GCS bucket `{}` does not exist and no project was configured; set \
                 --gcs-project or create the bucket out of band",
                self.config.bucket
            )));
        };
        // `buckets.insert` takes the bucket resource in the request body; the
        // project is the only query parameter.
        let body = format!("{{\"name\":\"{}\"}}", self.config.bucket);
        let url = self.bucket_collection_url(&[("project", project.clone())])?;
        let response = self.request(
            "POST",
            &url,
            &[("content-type", "application/json".to_owned())],
            Some(body.as_bytes()),
            MAX_JSON_BYTES,
        )?;
        match response.status {
            200 | 201 => Ok(()),
            // A concurrent creator won; that is the desired outcome.
            409 => Ok(()),
            401 | 403 => Err(Error::unauthorized(format!(
                "GCS denied creating bucket `{}`",
                self.config.bucket
            ))),
            status => Err(Error::io(format!(
                "GCS CreateBucket `{}` returned HTTP {status}: {}",
                self.config.bucket,
                response_body(&response.body)
            ))),
        }
    }

    /// Prove that the endpoint honours the two generation preconditions the
    /// store depends on, including that a **stale** generation is rejected.
    ///
    /// A service that accepts a stale `ifGenerationMatch` could silently lose a
    /// head publication, so it is refused `UnsupportedVersion` rather than
    /// downgraded.
    fn probe_generation_preconditions(&self) -> Result<()> {
        {
            let guard = self
                .probed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.is_some() {
                return Ok(());
            }
        }
        let key = Key::tmp(format!(
            "generation-probe-{}-{}",
            std::process::id(),
            unique_counter()
        ));
        let cleanup = || {
            let _ = self.delete_object(&key);
        };

        // `ifGenerationMatch=0` must create once.
        if !self.upload(&key, b"probe-one", Precondition::Missing)? {
            cleanup();
            return Err(Error::io(
                "GCS generation probe: a fresh key reported a failed precondition",
            ));
        }
        // ...and a second create must be refused.
        if self.upload(&key, b"probe-two", Precondition::Missing)? {
            cleanup();
            return Err(Error::unsupported_version(
                "GCS-compatible endpoint ignored `ifGenerationMatch=0`; refusing a writable store \
                 because create-once publication cannot be guaranteed",
            ));
        }
        let Some((bytes, generation)) = self.read_with_generation(&key)? else {
            cleanup();
            return Err(Error::io(
                "GCS generation probe: the first writer's object is not readable",
            ));
        };
        if bytes != b"probe-one" {
            cleanup();
            return Err(Error::io(
                "GCS generation probe: the first writer's bytes were replaced by a rejected \
                 create",
            ));
        }
        // A matching generation must succeed.
        if !self.upload(&key, b"probe-three", Precondition::Matches(generation))? {
            cleanup();
            return Err(Error::io(
                "GCS generation probe: a matching `ifGenerationMatch` was unexpectedly rejected",
            ));
        }
        // A stale generation must fail. This is the load-bearing check.
        if self.upload(&key, b"probe-four", Precondition::Matches(generation))? {
            cleanup();
            return Err(Error::unsupported_version(
                "GCS-compatible endpoint accepted a stale `ifGenerationMatch`; refusing a writable \
                 store because head publication could lose a race silently",
            ));
        }
        cleanup();
        let mut guard = self
            .probed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(());
        Ok(())
    }

    fn acquire_lock(&self) -> Result<()> {
        // The writer lock is the first conditional write of a writable open
        // (the store locks before it runs the plane probe). Proving the
        // generation preconditions here means every later lease write, and the
        // conditional release in `Drop`, already knows the endpoint honours
        // them.
        self.probe_generation_preconditions()?;
        let now = now_ns();
        let owner = format!("{}-{}-{}", std::process::id(), unique_counter(), now);
        let deadline = now.saturating_add(self.lease_ns());
        let key = Key::lock();
        for _ in 0..4 {
            match self.read_with_generation(&key)? {
                None => {
                    // No record at all: create it once. The first lease holder
                    // takes fencing token 1 and carries forward the head the
                    // superblock cache attests.
                    let head = self.superblock_head();
                    let body = encode_lock(
                        &owner,
                        1,
                        1,
                        deadline,
                        now,
                        head.as_ref()
                            .map(|(generation, commit, len)| (*generation, commit, *len)),
                    );
                    if let Some(generation) =
                        self.upload_tracked(&key, body.as_bytes(), Precondition::Missing)?
                    {
                        self.install_lease(owner, 1, 1, deadline, now, generation, head);
                        return Ok(());
                    }
                    // Another writer created it between the read and the PUT.
                    continue;
                }
                Some((observed_bytes, generation)) => {
                    let Some(observed) = parse_lock(&observed_bytes) else {
                        return Err(Error::busy(
                            "store LOCK object is unreadable; refusing a writable open rather \
                             than guessing whether a writer holds it",
                        ));
                    };
                    if observed.is_live(now) {
                        // The caller receives one fixed, non-disclosing
                        // sentence; owner, fence, and remaining lease go to
                        // diagnostics only.
                        self.note(format!(
                            "writable open refused: the LOCK is live (owner {} fence {}, lease ends \
                             in {} ms)",
                            observed.owner,
                            observed.fence,
                            (observed.deadline_ns - now) / 1_000_000
                        ));
                        return Err(Error::busy(
                            "store is locked by another GCS writer (live LOCK object)",
                        ));
                    }
                    // Expired lease or a released tombstone: take over with the
                    // next fencing token, conditional on the exact generation
                    // read. A lost race retries from a fresh read, never blind.
                    // The same write upgrades an expired v2/v1 record to v3.
                    let fence = observed.fence.checked_add(1).ok_or_else(|| {
                        Error::limit_exceeded("GCS fencing token would overflow u64")
                    })?;
                    let head = match observed.attested_head() {
                        Some((generation, commit)) => {
                            Some((generation, commit, observed.pending_commit_len))
                        }
                        None => self.superblock_head(),
                    };
                    let body = encode_lock(
                        &owner,
                        fence,
                        1,
                        deadline,
                        now,
                        head.as_ref()
                            .map(|(generation, commit, len)| (*generation, commit, *len)),
                    );
                    match self.upload_tracked(
                        &key,
                        body.as_bytes(),
                        Precondition::Matches(generation),
                    )? {
                        Some(new_generation) => {
                            self.note(format!(
                                "{} GCS writer lock: fence {} -> {}",
                                if observed.released {
                                    "reclaimed a released"
                                } else {
                                    "reclaimed an expired"
                                },
                                observed.fence,
                                fence
                            ));
                            if observed.schema_version < crate::writer_fence::LockVersion::V3 {
                                self.note(format!(
                                    "upgraded the GCS writer lock record from {:?} to v3 (single \
                                     conditional write; the head it attests is carried forward)",
                                    observed.schema_version
                                ));
                            }
                            self.install_lease(
                                owner,
                                fence,
                                1,
                                deadline,
                                now,
                                new_generation,
                                head,
                            );
                            return Ok(());
                        }
                        None => continue,
                    }
                }
            }
        }
        Err(Error::busy(
            "could not acquire the GCS writer lock after four conditional attempts",
        ))
    }

    /// The head the two-slot superblock cache attests, if any. Used only to
    /// carry a head forward when the LOCK record does not name one.
    fn superblock_head(&self) -> Option<(u64, crate::identity::Hash32, u64)> {
        let (bytes, _) = self.read_with_generation(&Key::superblock()).ok()??;
        let mut highest: Option<(u64, crate::identity::Hash32, u64)> = None;
        for slot in crate::format::decode_superblock(&bytes)
            .ok()?
            .into_iter()
            .flatten()
        {
            if slot.generation == 0 || slot.commit == crate::identity::Hash32::ZERO {
                continue;
            }
            if highest
                .map(|(generation, _, _)| slot.generation > generation)
                .unwrap_or(true)
            {
                highest = Some((slot.generation, slot.commit, slot.commit_len));
            }
        }
        highest
    }

    fn install_lease(
        &self,
        owner: String,
        fence: u64,
        sequence: u64,
        deadline_ns: i64,
        renewed_ns: i64,
        generation: u64,
        head: Option<(u64, crate::identity::Hash32, u64)>,
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
            generation,
            head,
        });
    }

    /// Prove this handle may publish right now, renewing the lease when one
    /// publication budget remains. The conditional write carries forward the
    /// head this handle last published.
    fn fence_lease(&self) -> Result<()> {
        self.fence_lease_for(None)
    }

    /// The publication point: one `ifGenerationMatch` write of the LOCK record
    /// that carries both the fence and the new head.
    fn publish_fenced_head(
        &self,
        generation: u64,
        commit: &crate::identity::Hash32,
        commit_len: u64,
    ) -> Result<()> {
        self.fence_lease_for(Some((generation, commit, commit_len)))
    }

    fn fence_lease_for(&self, pending: Option<(u64, &crate::identity::Hash32, u64)>) -> Result<()> {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = guard.as_mut() else {
            return Err(Error::busy(
                "GCS writer holds no lease; refusing to publish without a fenced lease",
            ));
        };
        let now = now_ns();
        if now >= lease.deadline_ns {
            let owner = lease.owner.clone();
            let fence = lease.fence;
            let expired_ms = (now - lease.deadline_ns) / 1_000_000;
            // Observation only. The decision never acts on it.
            let observed = self.read_lock_record().ok().flatten();
            self.stats
                .lease_expired_publications
                .fetch_add(1, Ordering::Relaxed);
            return Err(Error::busy(format!(
                "GCS writer lease deadline passed {expired_ms} ms ago (owner {owner} fence {fence}); \
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
        let head = pending
            .map(|(generation, commit, len)| (generation, *commit, len))
            .or(lease.head);
        let body = encode_lock(
            &lease.owner,
            lease.fence,
            sequence,
            deadline,
            now,
            head.as_ref()
                .map(|(generation, commit, len)| (*generation, commit, *len)),
        );
        // This is the publication point. `ifGenerationMatch` names the
        // generation this handle published last, so it succeeds only while the
        // object still carries this writer's fencing token. A successor's
        // reclaim rewrote the same object, so this fails with 412 and nothing
        // is published.
        match self.upload_tracked(
            &Key::lock(),
            body.as_bytes(),
            Precondition::Matches(lease.generation),
        )? {
            Some(generation) => {
                lease.sequence = sequence;
                lease.deadline_ns = deadline;
                lease.renewed_ns = now;
                lease.generation = generation;
                lease.head = head;
                self.stats.fence_checks.fetch_add(1, Ordering::Relaxed);
                if pending.is_some() {
                    self.stats.head_publications.fetch_add(1, Ordering::Relaxed);
                }
                if renew {
                    self.stats.lease_renewals.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            }
            None => {
                let observed = self.read_lock_record().ok().flatten();
                self.stats.fence_refusals.fetch_add(1, Ordering::Relaxed);
                Err(Error::busy(format!(
                    "GCS writer lease was taken over: the conditional fence CAS for owner {} \
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
        let response = self.media(&Key::lock(), None)?;
        match response.status {
            200 => Ok(parse_lock(&response.body)),
            404 => Ok(None),
            status => Err(Error::io(format!("GCS LOCK read returned HTTP {status}"))),
        }
    }

    /// Release the lease with a content-addressed tombstone published under
    /// `ifGenerationMatch` on the generation this writer observed.
    ///
    /// Never an unconditional delete after a read: if the lock was replaced
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
        let observed = match self.read_with_generation(&key) {
            Ok(observed) => observed,
            Err(err) => {
                self.note(format!("could not read LOCK to release it: {err}"));
                return;
            }
        };
        let Some((body, generation)) = observed else {
            return;
        };
        let Some(record) = parse_lock(&body) else {
            self.note("refusing to release an unreadable LOCK record".to_owned());
            return;
        };
        if record.owner != lease.owner || record.fence != lease.fence {
            self.note(format!(
                "refusing to release the GCS writer lock: it now names owner {} fence {} and this \
                 handle holds fence {}; the conditional release was not attempted",
                record.owner, record.fence, lease.fence
            ));
            return;
        }
        let released_from = sha256_hex(&body);
        let head = record
            .attested_head()
            .map(|(generation, commit)| (generation, commit, record.pending_commit_len))
            .or(lease.head);
        let tombstone = encode_released(
            &lease.owner,
            lease.fence,
            lease.sequence.saturating_add(1),
            now_ns(),
            &released_from,
            head.as_ref()
                .map(|(generation, commit, len)| (*generation, commit, *len)),
        );
        match self.upload_tracked(
            &key,
            tombstone.as_bytes(),
            Precondition::Matches(generation),
        ) {
            Ok(Some(_)) => self.note(format!(
                "released the GCS writer lock with a conditional tombstone (fence {})",
                lease.fence
            )),
            Ok(None) => self.note(format!(
                "refusing to release the GCS writer lock: it was replaced between the read and the \
                 conditional tombstone (this writer held fence {}); the successor's lock is intact",
                lease.fence
            )),
            Err(err) => self.note(format!(
                "conditional release of the GCS writer lock failed, nothing was deleted: {err}"
            )),
        }
    }

    /// Court/operator reset: replace the published `LOCK` with a released
    /// tombstone under a generation precondition. Never called by the store,
    /// which only ever releases a lease it still holds.
    pub fn release_stale_lease(&self) -> Result<Option<PublishedLock>> {
        self.probe_generation_preconditions()?;
        let Some((body, generation)) = self.read_with_generation(&Key::lock())? else {
            return Ok(None);
        };
        let Some(record) = parse_lock(&body) else {
            return Err(Error::corrupt("published LOCK record is unreadable"));
        };
        let fence = record
            .fence
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("GCS fencing token would overflow u64"))?;
        let tombstone = encode_released(
            &record.owner,
            fence,
            record.sequence.saturating_add(1),
            now_ns(),
            &sha256_hex(&body),
            record
                .attested_head()
                .map(|(generation, commit)| (generation, commit, record.pending_commit_len))
                .as_ref()
                .map(|(generation, commit, len)| (*generation, commit, *len)),
        );
        match self.upload_tracked(
            &Key::lock(),
            tombstone.as_bytes(),
            Precondition::Matches(generation),
        )? {
            Some(_) => self.read_lock_record(),
            None => Err(Error::busy(
                "the LOCK was replaced while the court reset it; nothing was deleted",
            )),
        }
    }

    fn lease_ns(&self) -> i64 {
        i64::try_from(self.config.lock_lease.as_nanos())
            .unwrap_or(i64::MAX)
            .max(MIN_LOCK_LEASE_SECS as i64 * 1_000_000_000)
    }

    /// Local own-deadline check: no network write, no renewal.
    fn check_own_deadline(&self) -> Result<()> {
        let guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = guard.as_ref() else {
            return Err(Error::busy(
                "GCS writer holds no lease; refusing to publish without a fenced lease",
            ));
        };
        let now = now_ns();
        if now >= lease.deadline_ns {
            let owner = lease.owner.clone();
            let fence = lease.fence;
            let expired_ms = (now - lease.deadline_ns) / 1_000_000;
            drop(guard);
            let observed = self.read_lock_record().ok().flatten();
            self.stats
                .lease_expired_publications
                .fetch_add(1, Ordering::Relaxed);
            return Err(Error::busy(format!(
                "GCS writer lease deadline passed {expired_ms} ms ago (owner {owner} fence {fence}); \
                 publication refused before the head was touched{}",
                describe_observed(&observed, &owner, fence)
            )));
        }
        Ok(())
    }

    /// The window a single publication may still occupy. A writer renews when
    /// one budget remains, and a successor may not take over before the
    /// recorded deadline, so a head publication always finishes inside the
    /// lease it was started under.
    fn publication_budget_ns(&self) -> i64 {
        (self.lease_ns() / 2).max(1)
    }

    /// The head the writer lock record attests, read fresh with a plain GET and
    /// no probe.
    fn lock_attested_head(&self) -> Result<Option<AttestedHead>> {
        let Some(record) = self.read_lock_record()? else {
            return Ok(None);
        };
        let Some((generation, commit)) = record.attested_head() else {
            return Ok(None);
        };
        Ok(Some(AttestedHead {
            generation,
            commit,
            commit_len: record.pending_commit_len,
        }))
    }

    /// Refresh the two-slot superblock cache after a lock-record publication.
    /// It never moves the cache backwards, and a failure is a diagnostic, not a
    /// publication failure.
    fn refresh_head_cache(
        &self,
        generation: u64,
        commit: &crate::identity::Hash32,
        commit_len: u64,
        timestamp_ns: i64,
    ) -> Result<()> {
        if commit_len == 0 {
            self.note(
                "superblock cache not refreshed: the lock record does not attest the commit \
                 length (a pre-v3 record); the next publication fills it"
                    .to_owned(),
            );
            return Ok(());
        }
        let key = Key::superblock();
        for _ in 0..8 {
            let Some((bytes, generation_token)) = self.read_with_generation(&key)? else {
                return Err(Error::corrupt(
                    "superblock cache is missing; the store layout is incomplete",
                ));
            };
            if bytes.len() != crate::format::SUPERBLOCK_BYTES {
                return Err(Error::corrupt(format!(
                    "superblock is {} bytes, expected {}",
                    bytes.len(),
                    crate::format::SUPERBLOCK_BYTES
                )));
            }
            let mut highest = 0u64;
            for slot in crate::format::decode_superblock(&bytes)?
                .into_iter()
                .flatten()
            {
                highest = highest.max(slot.generation);
            }
            if highest >= generation {
                return Ok(());
            }
            let mut image = bytes;
            let offset = ((generation % 2) as usize) * crate::format::SLOT_BYTES;
            let slot = crate::format::SuperblockSlot {
                generation,
                commit: *commit,
                commit_len,
                timestamp_ns,
            };
            image[offset..offset + crate::format::SLOT_BYTES].copy_from_slice(&slot.encode());
            if self
                .upload_tracked(&key, &image, Precondition::Matches(generation_token))?
                .is_some()
            {
                return Ok(());
            }
            self.note(
                "conditional superblock cache update lost a race; retrying from a fresh read"
                    .to_owned(),
            );
        }
        Err(Error::busy(
            "superblock cache update kept losing to another writer",
        ))
    }
}

impl Drop for GcsBackend {
    fn drop(&mut self) {
        self.release_lock();
    }
}

// ---------------------------------------------------------------------------
// Plane implementations
// ---------------------------------------------------------------------------

impl BlobPlane for GcsBackend {
    fn blob_label(&self) -> &'static str {
        "gcs"
    }

    fn blob_capabilities(&self) -> BlobCapabilities {
        BlobCapabilities::from_capabilities("gcs", &GcsBackend::capabilities())
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>> {
        StorageBackend::read_all(self, key)
    }

    fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        StorageBackend::read(self, key, offset, len)
    }

    fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        if self.upload(key, bytes, Precondition::Missing)? {
            return Ok(());
        }
        // The key exists. Identical bytes are idempotent; different bytes under
        // a content-addressed name are a real conflict.
        let existing = self.media(key, None)?;
        match existing.status {
            200 if existing.body == bytes => Ok(()),
            200 => Err(Error::exists(format!(
                "key {} already exists with different bytes",
                key.display_path()
            ))),
            404 => Err(Error::busy(format!(
                "key {} was reported present but disappeared during create-once publication",
                key.display_path()
            ))),
            401 | 403 => Err(Error::unauthorized(format!(
                "GCS denied reading {}",
                key.display_path()
            ))),
            status => Err(Error::io(format!(
                "GCS read after a create-once conflict on {} returned HTTP {status}",
                key.display_path()
            ))),
        }
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        self.delete_object(key)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        match self.metadata(key)? {
            Some(meta) => Ok(meta.size),
            None => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
        }
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        Ok(self.metadata(key)?.is_some())
    }

    fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_objects(prefix)
    }

    fn clean_stale(&mut self, _grace_secs: u64) -> Result<()> {
        // There is no staging namespace: every upload is atomic under its final
        // key. The only `tmp/` objects are capability-probe keys, which a
        // crashed process can leave behind; the writer lock is held here, so
        // deleting all of them is safe.
        let keys = self.list_objects(&Key::prefix(KeyKind::Tmp))?;
        for key in keys {
            let _ = self.delete_object(&key);
        }
        Ok(())
    }

    fn init_layout(&mut self) -> Result<()> {
        GcsBackend::init_layout(self)
    }

    fn ensure_layout(&mut self) -> Result<()> {
        GcsBackend::ensure_layout(self)
    }

    fn stats(&self) -> BackendStats {
        self.backend_stats()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.backend_diagnostics()
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl CommitPlane for GcsBackend {
    fn commit_label(&self) -> &'static str {
        "gcs"
    }

    fn commit_capabilities(&self) -> CommitCapabilities {
        CommitCapabilities::from_capabilities("gcs", &GcsBackend::capabilities())
    }

    fn read_control(&self, key: &Key) -> Result<Option<ControlRecord>> {
        match self.media(key, None)? {
            response if response.status == 200 => Ok(Some(ControlRecord {
                token: Some(control_token(&response.body)),
                bytes: response.body,
            })),
            response if response.status == 404 => Ok(None),
            response if response.status == 401 || response.status == 403 => Err(
                Error::unauthorized(format!("GCS denied reading {}", key.display_path())),
            ),
            response => Err(Error::io(format!(
                "GCS read {} returned HTTP {}",
                key.display_path(),
                response.status
            ))),
        }
    }

    fn compare_exchange(
        &mut self,
        key: &Key,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> Result<ControlRecord> {
        // Read the record and the generation those bytes belong to, require the
        // caller's content token to match, then condition the upload on that
        // exact generation. A lost race is HTTP 412 and stays `Busy`.
        let observed = self.read_with_generation(key)?;
        let token = observed.as_ref().map(|(current, _)| control_token(current));
        if token.as_deref() != expected {
            return Err(Error::busy(format!(
                "control record {} changed since it was read; compare-and-swap refused",
                key.display_path()
            )));
        }
        let precondition = match observed {
            Some((_, generation)) => Precondition::Matches(generation),
            None => Precondition::Missing,
        };
        match self.upload(key, bytes, precondition)? {
            true => Ok(ControlRecord {
                token: Some(control_token(bytes)),
                bytes: bytes.to_vec(),
            }),
            false => Err(Error::busy(format!(
                "GCS generation precondition lost a race for {}",
                key.display_path()
            ))),
        }
    }

    fn write_control(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        // Unconditional replacement, but conditioned on the observed generation
        // so a concurrent update is retried rather than clobbered.
        for _ in 0..8 {
            let precondition = match self.read_with_generation(key)? {
                Some((_, generation)) => Precondition::Matches(generation),
                None => Precondition::Missing,
            };
            if self.upload(key, bytes, precondition)? {
                return Ok(());
            }
        }
        Err(Error::busy(format!(
            "conditional replacement of {} kept losing",
            key.display_path()
        )))
    }

    fn create_control(&mut self, key: &Key, len: u64) -> Result<()> {
        let len = usize::try_from(len)
            .map_err(|_| Error::limit_exceeded("zeroed object does not fit the address space"))?;
        if len as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "zeroed object is {len} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound"
            )));
        }
        BlobPlane::put_if_absent(self, key, &vec![0u8; len])
    }

    fn put_control_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        BlobPlane::put_if_absent(self, key, bytes)
    }

    fn delete_control(&mut self, key: &Key) -> Result<()> {
        self.delete_object(key)
    }

    fn control_len(&self, key: &Key) -> Result<u64> {
        BlobPlane::len(self, key)
    }

    fn control_exists(&self, key: &Key) -> Result<bool> {
        BlobPlane::exists(self, key)
    }

    fn list_control(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_objects(prefix)
    }

    fn modified(&self, _key: &Key) -> Result<Option<SystemTime>> {
        // The JSON API reports an `updated` timestamp string. The store does not
        // depend on it, and this crate has no date parser; returning `None` is
        // honest rather than approximating.
        Ok(None)
    }

    fn sync_dir(&mut self, _key: &Key) -> Result<()> {
        // A successful upload is acknowledged by the provider; there is no
        // separate fsync primitive.
        Ok(())
    }

    fn init_layout(&mut self) -> Result<()> {
        GcsBackend::init_layout(self)
    }

    fn ensure_layout(&mut self) -> Result<()> {
        GcsBackend::ensure_layout(self)
    }

    fn acquire_lock(&mut self) -> Result<()> {
        GcsBackend::acquire_lock(self)
    }

    fn verify_write_lease(&mut self) -> Result<()> {
        GcsBackend::fence_lease(self)
    }

    fn check_lease_deadline(&self) -> Result<()> {
        GcsBackend::check_own_deadline(self)
    }

    fn fence_head_publication(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
    ) -> Result<()> {
        let _ = (generation, commit);
        GcsBackend::fence_lease(self)
    }

    fn publish_fenced_head(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
        commit_len: u64,
    ) -> Result<bool> {
        GcsBackend::publish_fenced_head(self, generation, commit, commit_len)?;
        Ok(true)
    }

    fn attested_head(&self) -> Result<Option<AttestedHead>> {
        GcsBackend::lock_attested_head(self)
    }

    fn refresh_head_cache(
        &mut self,
        generation: u64,
        commit: crate::identity::Hash32,
        commit_len: u64,
        timestamp_ns: i64,
    ) -> Result<()> {
        GcsBackend::refresh_head_cache(self, generation, &commit, commit_len, timestamp_ns)
    }

    fn clean_stale(&mut self, grace_secs: u64) -> Result<()> {
        BlobPlane::clean_stale(self, grace_secs)
    }

    fn stats(&self) -> BackendStats {
        self.backend_stats()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.backend_diagnostics()
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl GcsBackend {
    /// Ensure the bucket exists and prove the generation preconditions.
    ///
    /// Inherent so a caller holding a concrete [`GcsBackend`] does not have to
    /// disambiguate between the two plane traits.
    pub fn init_layout(&mut self) -> Result<()> {
        self.ensure_bucket()?;
        self.probe_generation_preconditions()
    }

    /// Ensure the bucket exists and prove the generation preconditions.
    pub fn ensure_layout(&mut self) -> Result<()> {
        self.ensure_bucket()?;
        self.probe_generation_preconditions()
    }

    fn backend_stats(&self) -> BackendStats {
        BackendStats {
            reads: self.stats.reads.load(Ordering::Relaxed),
            range_reads: self.stats.range_reads.load(Ordering::Relaxed),
            bytes_read: self.stats.bytes_read.load(Ordering::Relaxed),
            writes: self.stats.writes.load(Ordering::Relaxed),
            bytes_written: self.stats.bytes_written.load(Ordering::Relaxed),
            link_publications: 0,
            rename_publications: 0,
            rename_fallbacks: 0,
            fsyncs: 0,
        }
    }

    fn backend_diagnostics(&self) -> Vec<String> {
        let mut out = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let conflicts = self.stats.conditional_conflicts.load(Ordering::Relaxed);
        if conflicts > 0 {
            out.push(format!(
                "GCS generation preconditions observed {conflicts} lost/retried race(s)"
            ));
        }
        let fences = self.stats.fence_checks.load(Ordering::Relaxed);
        let renewals = self.stats.lease_renewals.load(Ordering::Relaxed);
        if fences > 0 {
            out.push(format!(
                "GCS fence CAS proved the lease {fences} time(s), {renewals} with a renewal"
            ));
        }
        if let Some(auth) = self
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            let stats = auth.stats();
            out.push(format!(
                "GCS service-account auth: {} token exchange(s), {} cache hit(s), {} refusal(s); \
                 key {} ({} bits)",
                stats.exchanges,
                stats.cache_hits,
                stats.refusals,
                auth.key().fingerprint_hex(),
                auth.key().modulus_bits()
            ));
        }
        let refusals = self.stats.fence_refusals.load(Ordering::Relaxed);
        let expired = self
            .stats
            .lease_expired_publications
            .load(Ordering::Relaxed);
        if refusals > 0 || expired > 0 {
            out.push(format!(
                "GCS fence refused {refusals} publication(s) after a takeover and {expired} after \
                 this writer's own deadline"
            ));
        }
        out
    }

    /// Test and court hook: the GCS generation of `key`, or `None` when it does
    /// not exist. Recorded in receipts as evidence that a control object was
    /// replaced under a generation precondition.
    #[doc(hidden)]
    pub fn generation_for_test(&self, key: &Key) -> Result<Option<u64>> {
        Ok(self.metadata(key)?.map(|meta| meta.generation))
    }

    /// Test hook: upload `bytes` with `ifGenerationMatch=generation`. Returns
    /// `false` when the precondition failed.
    #[doc(hidden)]
    pub fn conditional_upload_for_test(
        &self,
        key: &Key,
        bytes: &[u8],
        generation: u64,
    ) -> Result<bool> {
        self.upload(key, bytes, Precondition::Matches(generation))
    }
}

// ---------------------------------------------------------------------------
// StorageBackend: one implementation of each physical operation
// ---------------------------------------------------------------------------

impl StorageBackend for GcsBackend {
    fn label(&self) -> &'static str {
        "gcs"
    }

    fn capabilities(&self) -> Capabilities {
        GcsBackend::capabilities()
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let response = self.media(key, Some((offset, len)))?;
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
                // End-of-object is not an error, matching other backends; only
                // an offset strictly beyond the object is rejected.
                let total = BlobPlane::len(self, key)?;
                if offset == total {
                    return Ok(Vec::new());
                }
                return Err(Error::invalid_argument(format!(
                    "read offset {offset} is beyond {} ({total} bytes)",
                    key.display_path()
                )));
            }
            401 | 403 => {
                return Err(Error::unauthorized(format!(
                    "GCS denied {}",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "GCS range GET {} returned HTTP {status}",
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
        let response = self.media(key, None)?;
        let bytes = match response.status {
            200 => response.body,
            404 => {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )))
            }
            401 | 403 => {
                return Err(Error::unauthorized(format!(
                    "GCS denied {}",
                    key.display_path()
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "GCS GET {} returned HTTP {status}",
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
        // Only the two-slot superblock takes this path. GCS has no in-place
        // range write: read the whole object, patch the bytes, and publish the
        // result under the generation that was read.
        for _ in 0..8 {
            let Some((mut image, generation)) = self.read_with_generation(key)? else {
                return Err(Error::not_found(format!(
                    "key {} does not exist",
                    key.display_path()
                )));
            };
            if image.len() != SUPERBLOCK_BYTES && key.kind() == KeyKind::Superblock {
                return Err(Error::corrupt(format!(
                    "superblock is {} bytes, expected {SUPERBLOCK_BYTES}",
                    image.len()
                )));
            }
            let start = usize::try_from(offset)
                .map_err(|_| Error::limit_exceeded("write offset does not fit memory"))?;
            let end = start
                .checked_add(bytes.len())
                .ok_or_else(|| Error::limit_exceeded("write end overflows memory"))?;
            if end > image.len() {
                return Err(Error::invalid_argument(format!(
                    "write range {offset}..{end} exceeds {} bytes",
                    image.len()
                )));
            }
            image[start..end].copy_from_slice(bytes);
            if self.upload(key, &image, Precondition::Matches(generation))? {
                return Ok(());
            }
            self.note(format!(
                "conditional update for {} lost a race; retrying from a fresh read",
                key.display_path()
            ));
        }
        Err(Error::busy(
            "superblock conditional update kept losing to another writer",
        ))
    }

    fn put_atomic(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        BlobPlane::put_if_absent(self, key, bytes)
    }

    fn put_replace(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        CommitPlane::write_control(self, key, bytes)
    }

    fn compare_and_swap(&mut self, key: &Key, expected: Option<&str>, bytes: &[u8]) -> Result<()> {
        CommitPlane::compare_exchange(self, key, expected, bytes).map(|_| ())
    }

    fn create_zeroed(&mut self, key: &Key, len: u64) -> Result<()> {
        CommitPlane::create_control(self, key, len)
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        self.delete_object(key)
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_objects(prefix)
    }

    fn sync(&mut self, _key: &Key) -> Result<()> {
        Ok(())
    }

    fn sync_dir(&mut self, _key: &Key) -> Result<()> {
        Ok(())
    }

    fn len(&self, key: &Key) -> Result<u64> {
        BlobPlane::len(self, key)
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        BlobPlane::exists(self, key)
    }

    fn modified(&self, _key: &Key) -> Result<Option<SystemTime>> {
        Ok(None)
    }

    fn init_layout(&mut self) -> Result<()> {
        BlobPlane::init_layout(self)
    }

    fn ensure_layout(&mut self) -> Result<()> {
        BlobPlane::ensure_layout(self)
    }

    fn acquire_writer_lock(&mut self) -> Result<()> {
        CommitPlane::acquire_lock(self)
    }

    fn verify_write_lease(&mut self) -> Result<()> {
        GcsBackend::fence_lease(self)
    }

    fn check_lease_deadline(&self) -> Result<()> {
        GcsBackend::check_own_deadline(self)
    }

    fn fence_head_publication(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
    ) -> Result<()> {
        let _ = (generation, commit);
        GcsBackend::fence_lease(self)
    }

    fn publish_fenced_head(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
        commit_len: u64,
    ) -> Result<bool> {
        GcsBackend::publish_fenced_head(self, generation, commit, commit_len)?;
        Ok(true)
    }

    fn attested_head(&self) -> Result<Option<AttestedHead>> {
        GcsBackend::lock_attested_head(self)
    }

    fn refresh_head_cache(
        &mut self,
        generation: u64,
        commit: crate::identity::Hash32,
        commit_len: u64,
        timestamp_ns: i64,
    ) -> Result<()> {
        GcsBackend::refresh_head_cache(self, generation, &commit, commit_len, timestamp_ns)
    }

    fn clean_stale_tmp(&mut self, grace_secs: u64) -> Result<()> {
        BlobPlane::clean_stale(self, grace_secs)
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn stats(&self) -> BackendStats {
        self.backend_stats()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.backend_diagnostics()
    }

    fn probe_planes(&mut self) -> Result<Option<crate::planes::PlaneProbes>> {
        let bytes = crate::planes::probe_bytes();
        let blobs = BlobPlane::head_part(self, &bytes)?;
        let commits = CommitPlane::probe(self)?;
        Ok(Some(crate::planes::PlaneProbes { blobs, commits }))
    }
}

pub use crate::writer_fence::{AttestedHead, LockVersion, PublishedLock};

fn encode_lock(
    owner: &str,
    fence: u64,
    sequence: u64,
    deadline_ns: i64,
    renewed_ns: i64,
    pending: Option<(u64, &crate::identity::Hash32, u64)>,
) -> String {
    crate::writer_fence::encode_lock(
        LOCK_SCHEMA,
        owner,
        fence,
        sequence,
        deadline_ns,
        renewed_ns,
        pending,
    )
}

fn encode_released(
    owner: &str,
    fence: u64,
    sequence: u64,
    released_ns: i64,
    released_from: &str,
    pending: Option<(u64, &crate::identity::Hash32, u64)>,
) -> String {
    crate::writer_fence::encode_released(
        RELEASED_SCHEMA,
        owner,
        fence,
        sequence,
        released_ns,
        released_from,
        pending,
    )
}

fn parse_lock(bytes: &[u8]) -> Option<PublishedLock> {
    crate::writer_fence::parse_lock(bytes, &LOCK_SCHEMAS)
}

fn describe_observed(observed: &Option<PublishedLock>, owner: &str, fence: u64) -> String {
    crate::writer_fence::describe_observed(observed, owner, fence)
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::writer_fence::sha256_hex(bytes)
}

/// The `generation` field of an upload reply's object resource, if it carried
/// one. Both string and numeric JSON spellings are accepted, without rounding.
fn response_generation(body: &[u8]) -> Option<u64> {
    let value = json::parse(body).ok()?;
    value.get("generation").and_then(JsonValue::as_u64)
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
    cap: u64,
    what: &str,
) -> Result<Vec<u8>> {
    if let Some(len) = content_length {
        if len > cap {
            return Err(Error::limit_exceeded(format!(
                "GCS response `{what}` is {len} bytes, above the backend's {cap}-byte bound"
            )));
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format!("GCS response read: {err}")))?;
    if bytes.len() as u64 > cap {
        return Err(Error::limit_exceeded(format!(
            "GCS response `{what}` exceeded the backend's {cap}-byte bound"
        )));
    }
    Ok(bytes)
}

fn response_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(240).collect()
}

/// RFC 3986 percent-encoding of one path or query component. `/` is encoded so
/// an object name stays a single path segment, which is what the GCS JSON API
/// requires.
fn pct(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
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

fn query_string(query: &[(&str, String)]) -> String {
    query
        .iter()
        .map(|(key, value)| format!("{}={}", pct(key), pct(value)))
        .collect::<Vec<_>>()
        .join("&")
}

// ---------------------------------------------------------------------------
// A bounded JSON reader
// ---------------------------------------------------------------------------

/// Minimal, bounded JSON reader for the three response shapes this backend
/// parses (object metadata, a bucket listing page, an upload response).
///
/// It is deliberately strict: depth, node count, and string length are bounded,
/// trailing bytes are rejected, and a parse failure is an error rather than an
/// empty result. No new dependency is introduced.
mod json {
    use std::collections::BTreeMap;

    const MAX_DEPTH: usize = 32;
    const MAX_NODES: usize = 1_000_000;
    const MAX_STRING_BYTES: usize = 1 << 20;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        /// Numbers keep their exact source text so a 64-bit generation is never
        /// rounded through a float.
        Number(String),
        String(String),
        Array(Vec<Value>),
        Object(BTreeMap<String, Value>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Object(map) => map.get(key),
                _ => None,
            }
        }

        pub fn as_array(&self) -> Option<&[Value]> {
            match self {
                Value::Array(items) => Some(items),
                _ => None,
            }
        }

        /// String content, or the exact text of a number.
        pub fn as_text(&self) -> Option<&str> {
            match self {
                Value::String(text) | Value::Number(text) => Some(text),
                _ => None,
            }
        }

        pub fn as_u64(&self) -> Option<u64> {
            self.as_text()?.parse::<u64>().ok()
        }

        #[allow(dead_code)]
        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Value::Bool(value) => Some(*value),
                _ => None,
            }
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Value, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "body is not UTF-8".to_owned())?;
        let mut parser = Parser {
            bytes: text.as_bytes(),
            index: 0,
            nodes: 0,
        };
        parser.skip_ws();
        let value = parser.value(0)?;
        parser.skip_ws();
        if parser.index != parser.bytes.len() {
            return Err(format!(
                "trailing bytes at offset {} after the JSON value",
                parser.index
            ));
        }
        Ok(value)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        index: usize,
        nodes: usize,
    }

    impl<'a> Parser<'a> {
        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.index).copied()
        }

        fn bump(&mut self) -> Option<u8> {
            let byte = self.peek()?;
            self.index += 1;
            Some(byte)
        }

        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.index += 1;
            }
        }

        fn expect(&mut self, expected: u8) -> Result<(), String> {
            match self.bump() {
                Some(byte) if byte == expected => Ok(()),
                Some(byte) => Err(format!(
                    "expected `{}` at offset {}, found `{}`",
                    expected as char,
                    self.index - 1,
                    byte as char
                )),
                None => Err(format!("expected `{}` at end of input", expected as char)),
            }
        }

        fn count_node(&mut self) -> Result<(), String> {
            self.nodes += 1;
            if self.nodes > MAX_NODES {
                return Err(format!("more than {MAX_NODES} JSON values"));
            }
            Ok(())
        }

        fn value(&mut self, depth: usize) -> Result<Value, String> {
            if depth > MAX_DEPTH {
                return Err(format!("JSON nesting deeper than {MAX_DEPTH}"));
            }
            self.count_node()?;
            self.skip_ws();
            match self.peek() {
                Some(b'n') => {
                    self.literal(b"null")?;
                    Ok(Value::Null)
                }
                Some(b't') => {
                    self.literal(b"true")?;
                    Ok(Value::Bool(true))
                }
                Some(b'f') => {
                    self.literal(b"false")?;
                    Ok(Value::Bool(false))
                }
                Some(b'"') => Ok(Value::String(self.string()?)),
                Some(b'[') => self.array(depth),
                Some(b'{') => self.object(depth),
                Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.number(),
                Some(byte) => Err(format!(
                    "unexpected `{}` at offset {}",
                    byte as char, self.index
                )),
                None => Err("unexpected end of input".to_owned()),
            }
        }

        fn literal(&mut self, text: &[u8]) -> Result<(), String> {
            if self.bytes[self.index..].starts_with(text) {
                self.index += text.len();
                Ok(())
            } else {
                Err(format!("invalid literal at offset {}", self.index))
            }
        }

        fn array(&mut self, depth: usize) -> Result<Value, String> {
            self.expect(b'[')?;
            let mut items = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.index += 1;
                return Ok(Value::Array(items));
            }
            loop {
                items.push(self.value(depth + 1)?);
                self.skip_ws();
                match self.bump() {
                    Some(b',') => continue,
                    Some(b']') => return Ok(Value::Array(items)),
                    _ => return Err(format!("unterminated array at offset {}", self.index)),
                }
            }
        }

        fn object(&mut self, depth: usize) -> Result<Value, String> {
            self.expect(b'{')?;
            let mut map = BTreeMap::new();
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.index += 1;
                return Ok(Value::Object(map));
            }
            loop {
                self.skip_ws();
                let key = self.string()?;
                self.skip_ws();
                self.expect(b':')?;
                let value = self.value(depth + 1)?;
                if map.insert(key.clone(), value).is_some() {
                    return Err(format!("duplicate JSON key `{key}`"));
                }
                self.skip_ws();
                match self.bump() {
                    Some(b',') => continue,
                    Some(b'}') => return Ok(Value::Object(map)),
                    _ => return Err(format!("unterminated object at offset {}", self.index)),
                }
            }
        }

        fn string(&mut self) -> Result<String, String> {
            self.expect(b'"')?;
            let mut out = String::new();
            loop {
                let Some(byte) = self.bump() else {
                    return Err("unterminated string".to_owned());
                };
                match byte {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let Some(escape) = self.bump() else {
                            return Err("unterminated escape".to_owned());
                        };
                        match escape {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{0008}'),
                            b'f' => out.push('\u{000c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let first = self.hex4()?;
                                let ch = if (0xD800..0xDC00).contains(&first) {
                                    // High surrogate: a low surrogate must
                                    // follow, otherwise the text is invalid.
                                    if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                                        return Err("unpaired UTF-16 surrogate".to_owned());
                                    }
                                    let second = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&second) {
                                        return Err("unpaired UTF-16 surrogate".to_owned());
                                    }
                                    let code = 0x10000
                                        + u32::from(first - 0xD800) * 0x400
                                        + u32::from(second - 0xDC00);
                                    char::from_u32(code)
                                        .ok_or_else(|| "invalid surrogate pair".to_owned())?
                                } else if (0xDC00..0xE000).contains(&first) {
                                    return Err("unpaired UTF-16 surrogate".to_owned());
                                } else {
                                    char::from_u32(u32::from(first))
                                        .ok_or_else(|| "invalid \\u escape".to_owned())?
                                };
                                out.push(ch);
                            }
                            other => return Err(format!("invalid escape `\\{}`", other as char)),
                        }
                    }
                    0x00..=0x1f => {
                        return Err(format!("raw control byte 0x{byte:02x} in a JSON string"))
                    }
                    _ => {
                        // Non-ASCII UTF-8 continuation bytes are copied through;
                        // validity was established by the `from_utf8` check.
                        let start = self.index - 1;
                        let width = utf8_width(byte);
                        self.index = start + width;
                        if self.index > self.bytes.len() {
                            return Err("truncated UTF-8 sequence".to_owned());
                        }
                        out.push_str(
                            std::str::from_utf8(&self.bytes[start..self.index])
                                .map_err(|_| "invalid UTF-8 in string".to_owned())?,
                        );
                    }
                }
                if out.len() > MAX_STRING_BYTES {
                    return Err(format!("JSON string longer than {MAX_STRING_BYTES} bytes"));
                }
            }
        }

        fn hex4(&mut self) -> Result<u16, String> {
            let mut value: u16 = 0;
            for _ in 0..4 {
                let Some(byte) = self.bump() else {
                    return Err("truncated \\u escape".to_owned());
                };
                let digit = match byte {
                    b'0'..=b'9' => u16::from(byte - b'0'),
                    b'a'..=b'f' => u16::from(byte - b'a') + 10,
                    b'A'..=b'F' => u16::from(byte - b'A') + 10,
                    _ => return Err("invalid hex digit in \\u escape".to_owned()),
                };
                value = value * 16 + digit;
            }
            Ok(value)
        }

        fn number(&mut self) -> Result<Value, String> {
            let start = self.index;
            if self.peek() == Some(b'-') {
                self.index += 1;
            }
            let mut digits = 0usize;
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.index += 1;
                digits += 1;
            }
            if digits == 0 {
                return Err(format!("number has no digits at offset {start}"));
            }
            if self.peek() == Some(b'.') {
                self.index += 1;
                let mut fraction = 0usize;
                while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                    self.index += 1;
                    fraction += 1;
                }
                if fraction == 0 {
                    return Err(format!("number has no fraction digits at offset {start}"));
                }
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                self.index += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.index += 1;
                }
                let mut exponent = 0usize;
                while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                    self.index += 1;
                    exponent += 1;
                }
                if exponent == 0 {
                    return Err(format!("number has no exponent digits at offset {start}"));
                }
            }
            let text = std::str::from_utf8(&self.bytes[start..self.index])
                .map_err(|_| "invalid number".to_owned())?;
            Ok(Value::Number(text.to_owned()))
        }
    }

    fn utf8_width(first: u8) -> usize {
        match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_object_metadata() {
            let value =
                parse(br#"{"name":"objects/ab/x","size":"123","generation":"1699000000000001"}"#)
                    .unwrap();
            assert_eq!(value.get("size").and_then(Value::as_u64), Some(123));
            assert_eq!(
                value.get("generation").and_then(Value::as_u64),
                Some(1_699_000_000_000_001)
            );
            assert_eq!(
                value.get("name").and_then(Value::as_text),
                Some("objects/ab/x")
            );
        }

        #[test]
        fn parses_numeric_generation_without_rounding() {
            let value = parse(br#"{"generation":1699000000000001}"#).unwrap();
            assert_eq!(
                value.get("generation").and_then(Value::as_u64),
                Some(1_699_000_000_000_001)
            );
        }

        #[test]
        fn parses_a_listing_page_with_escapes() {
            let value = parse(
                br#"{"items":[{"name":"objects/ab/a"},{"name":"refs/tags/a\"b"}],"nextPageToken":"t"}"#,
            )
            .unwrap();
            let items = value.get("items").and_then(Value::as_array).unwrap();
            assert_eq!(items.len(), 2);
            assert_eq!(
                items[1].get("name").and_then(Value::as_text),
                Some("refs/tags/a\"b")
            );
            assert_eq!(
                value.get("nextPageToken").and_then(Value::as_text),
                Some("t")
            );
        }

        #[test]
        fn rejects_malformed_and_deep_input() {
            assert!(parse(b"{\"a\":}").is_err());
            assert!(parse(b"{} trailing").is_err());
            assert!(parse(b"{\"a\":1,\"a\":2}").is_err());
            let mut deep = String::new();
            for _ in 0..64 {
                deep.push('[');
            }
            for _ in 0..64 {
                deep.push(']');
            }
            assert!(parse(deep.as_bytes()).is_err());
        }
    }
}

use json::Value as JsonValue;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_keeps_one_path_segment() {
        assert_eq!(pct("objects/ab/ff"), "objects%2Fab%2Fff");
        assert_eq!(pct("a b"), "a%20b");
        assert_eq!(pct("plain-name_1.2~"), "plain-name_1.2~");
    }

    #[test]
    fn config_debug_never_reveals_the_token() {
        let config = GcsConfig {
            endpoint: "http://127.0.0.1:4443".to_owned(),
            bucket: "weft-court".to_owned(),
            token: Some("super-secret-token".to_owned()),
            service_account: None,
            scope: crate::service_account::DEFAULT_SCOPE.to_owned(),
            project: Some("weft".to_owned()),
            anonymous: false,
            lock_lease: Duration::from_secs(2),
            request_timeout: Duration::from_secs(60),
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn bucket_validation_rejects_bad_names() {
        let mut config = GcsConfig {
            endpoint: "http://127.0.0.1:4443".to_owned(),
            bucket: "weft-court".to_owned(),
            token: None,
            service_account: None,
            scope: crate::service_account::DEFAULT_SCOPE.to_owned(),
            project: None,
            anonymous: true,
            lock_lease: Duration::from_secs(2),
            request_timeout: Duration::from_secs(60),
        };
        assert!(config.validate().is_ok());
        config.bucket = "Weft_Court".to_owned();
        assert!(config.validate().is_err());
        config.bucket = "ok".to_owned();
        config.endpoint = "ftp://host".to_owned();
        assert!(config.validate().is_err());
    }

    #[test]
    fn precondition_parameters_are_the_gcs_spelling() {
        assert_eq!(Precondition::Missing.parameter(), "0");
        assert_eq!(Precondition::Matches(42).parameter(), "42");
    }

    #[test]
    fn lock_record_round_trips_and_legacy_reads_as_fence_zero() {
        let body = encode_lock("x", 3, 7, 1_000, 900, None);
        let record = parse_lock(body.as_bytes()).expect("v3 lock parses");
        assert_eq!(record.schema_version, LockVersion::V3);
        assert_eq!(record.owner, "x");
        assert_eq!(record.fence, 3);
        assert_eq!(record.sequence, 7);
        assert_eq!(record.deadline_ns, 1_000);
        assert!(!record.released);

        let legacy = format!("{LOCK_SCHEMA_V1}\nowner=y\nexpires_ns=42\n");
        let record = parse_lock(legacy.as_bytes()).expect("v1 lock parses");
        assert_eq!(record.schema_version, LockVersion::V1);
        assert_eq!(record.fence, 0);
        assert_eq!(record.deadline_ns, 42);

        let tombstone = encode_released("owner=x", 3, 8, 2_000, "deadbeef", None);
        let record = parse_lock(tombstone.as_bytes()).expect("tombstone parses");
        assert!(record.released);
        assert_eq!(record.fence, 3);
        assert_eq!(record.released_from.as_deref(), Some("deadbeef"));

        assert_eq!(parse_lock(b"garbage"), None);
    }

    #[test]
    fn upload_reply_generation_is_parsed_without_rounding() {
        assert_eq!(
            response_generation(br#"{"name":"LOCK","generation":"1699000000000001"}"#),
            Some(1_699_000_000_000_001)
        );
        assert_eq!(
            response_generation(br#"{"generation":1699000000000001}"#),
            Some(1_699_000_000_000_001)
        );
        assert_eq!(response_generation(b""), None);
    }

    #[test]
    fn listing_helper_is_shared_with_the_logical_layout() {
        assert_eq!(prefix_for_kind(KeyKind::Object), Some("objects/"));
        let name = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = classify_relative(KeyKind::Object, &format!("01/{name}")).unwrap();
        assert_eq!(
            key,
            Key::object(crate::Hash32::from_lower_hex(name).unwrap())
        );
    }
}
