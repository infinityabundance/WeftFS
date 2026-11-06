//! Azure Blob Storage backend (subphase R21).
//!
//! This is a real Azure Blob REST backend, addressed exactly like every other
//! WeftFS backend: content, manifests, commit records, indexes, and refs keep
//! the normative logical key layout (`objects/<ab>/<hash>`,
//! `manifests/<ab>/<hash>`, `commits/<gen>.commit`, `indexes/<ab>/<commit>.idx`,
//! `refs/...`). Two stores built from the same operations publish byte-identical
//! records whichever backend carries them.
//!
//! # Protocol mapping
//!
//! * create-once publication: `Put Blob` with `x-ms-blob-type: BlockBlob` and
//!   `If-None-Match: *`. The service answers `201 Created`; a second create is
//!   `409 Conflict` (`BlobAlreadyExists`), which this backend maps to the same
//!   lost-precondition result S3 and GCS report.
//! * compare-and-swap: `Put Blob` with `If-Match: <ETag>`. A stale ETag is
//!   `412 Precondition Failed`. Entity tags are the service's own version token,
//!   so — exactly like the S3 backend — the public `ControlRecord.token` stays
//!   the content token of the bytes and the ETag is the transport token held
//!   privately.
//! * ranged reads: `Get Blob` with `Range: bytes=offset-end` (`206`).
//! * object length and existence: `Get Blob Properties` (`HEAD`).
//! * deletion: `Delete Blob` (`202`).
//! * listing: `List Blobs` (`GET /container?restype=container&comp=list`).
//! * the `superblock` object holds the same two 4096-byte slots as the local
//!   layout. A slot update is a whole-object read-modify-write conditioned on
//!   the ETag that was read, retried from a fresh read on a lost race.
//!
//! # Authentication
//!
//! Three credential forms are supported, and the backend refuses to guess when
//! a caller supplies two different ones:
//!
//! * **SharedKey** — the account name and one base64 account key. Every request
//!   is signed with HMAC-SHA256 over the documented canonicalised headers and
//!   resource; the signature is computed in this crate (`sha2` only, no new
//!   dependency).
//! * **SAS** — a pre-signed query string, appended verbatim to each request.
//! * **anonymous** — an emulator court only (Azurite can be started to accept
//!   it, but the default dev account requires SharedKey, which is what the
//!   court uses).
//!
//! Secrets are never written to diagnostics, receipts, or error messages;
//! [`AzureConfig`]'s `Debug` redacts them.
//!
//! # Fenced writer protocol (R18, ported)
//!
//! The single writer holds a lease in a `LOCK` blob using the same record shape
//! as the S3 and GCS backends ([`crate::writer_fence`]): owner, a monotonic
//! **fencing token**, a sequence number, an absolute `lease_deadline_ns`, and
//! the head being published.
//!
//! * takeover of an expired lease or a released tombstone is a `Put Blob` with
//!   `If-Match: <ETag read>`, and it bumps the fencing token by one;
//! * the **fence** before every publication is one conditional write of `LOCK`
//!   naming the ETag this handle last published. A successor's takeover rewrote
//!   the blob, so a stale writer gets `412` and its publication fails `Busy`
//!   with the head untouched;
//! * the same write renews the lease when at most one publication budget
//!   remains;
//! * a writer whose own deadline has passed refuses to publish before touching
//!   any head object;
//! * release is a content-addressed tombstone published with `If-Match`, never
//!   an unconditional delete after a read.
//!
//! # Endpoint behaviour is measured, not assumed
//!
//! HTTP entity tags are quoted. Azure compares `If-Match` against the quoted
//! ETag it returned; a proxy or emulator may not. Like the S3 backend, this one
//! *measures* the form at a writable open ([`IfMatchForm`]): it requires a
//! matching value to be accepted **and** a stale value in the same form to be
//! refused, with the winner's bytes surviving, and refuses the endpoint
//! otherwise.
//!
//! # Durability
//!
//! Azure has no `fsync` primitive. `durable_fsync` here means "the service
//! acknowledged the write". The Azurite court backs its data directory with a
//! tmpfs, so it proves protocol behaviour, conditional-write safety, and
//! fencing — nothing about media, power loss, cross-region replication, or
//! multi-node consistency. No Azure, cloud, fleet, or production claim is made.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::backend::{
    classify_listed_key as classify_relative, list_prefix_path as prefix_for_kind, BackendStats,
    Capabilities, Key, KeyKind, StorageBackend,
};
use crate::error::{Error, Result};
use crate::format::{MAX_CHUNK_SIZE, SUPERBLOCK_BYTES};
use crate::identity::Hash32;
use crate::planes::{
    control_token, BlobCapabilities, BlobPlane, CommitCapabilities, CommitPlane, ControlRecord,
};

/// Service API version sent on every request. The shared-key signature rules and
/// the conditional-header semantics used here are the documented ones for every
/// version from 2015-02-21 onward; a pinned value keeps the court reproducible.
const API_VERSION: &str = "2021-12-02";

/// Largest single blob the backend accepts. Matches the S3 and GCS backends'
/// bound so a store built on any of them has the same object-size limit.
const MAX_OBJECT_BYTES: u64 = MAX_CHUNK_SIZE * 64;

/// Bound on an XML metadata/listing/error body.
const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;

const LOCK_SCHEMA: &str = "weftfs-azure-lock v2";
const LOCK_SCHEMA_V1: &str = "weftfs-azure-lock v1";
const RELEASED_SCHEMA: &str = "weftfs-azure-lock-released v1";
/// A lease shorter than this cannot bound a publication, so it is refused.
const MIN_LOCK_LEASE_SECS: u64 = 1;

/// Connection and policy for an Azure Blob (or Azure-compatible) endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct AzureConfig {
    /// `https://<account>.blob.core.windows.net`, or an emulator endpoint such
    /// as `http://127.0.0.1:10000/devstoreaccount1`. An optional path prefix is
    /// preserved (Azurite puts the account in the path).
    pub endpoint: String,
    pub container: String,
    /// Storage account name used as the `SharedKey` credential scope.
    pub account: String,
    /// Base64 account key. Never logged.
    pub key: Option<String>,
    /// Pre-signed SAS query string (no leading `?`). Never logged.
    pub sas_token: Option<String>,
    /// Send no credential at all. An emulator court only.
    pub anonymous: bool,
    /// Writer-lock lease. A crashed writer leaves a stale lock that becomes
    /// reclaimable after this duration.
    pub lock_lease: Duration,
    /// Per-request timeout.
    pub request_timeout: Duration,
}

impl std::fmt::Debug for AzureConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The account key and the SAS token are secrets: show only presence.
        f.debug_struct("AzureConfig")
            .field("endpoint", &self.endpoint)
            .field("container", &self.container)
            .field("account", &self.account)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("sas_token", &self.sas_token.as_ref().map(|_| "<redacted>"))
            .field("anonymous", &self.anonymous)
            .field("lock_lease", &self.lock_lease)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl AzureConfig {
    /// Build from `WEFT_AZURE_*` first, then the standard Azure Storage names.
    pub fn from_env() -> Result<AzureConfig> {
        AzureConfig::resolve(None, None, None, None, None, None, None)
    }

    /// Resolve with explicit values taking precedence over the environment.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        endpoint: Option<String>,
        container: Option<String>,
        account: Option<String>,
        key: Option<String>,
        sas_token: Option<String>,
        anonymous: Option<bool>,
        lock_lease: Option<u64>,
    ) -> Result<AzureConfig> {
        let mut config = AzureConfig {
            endpoint: first_env(&["WEFT_AZURE_ENDPOINT", "AZURE_STORAGE_BLOB_ENDPOINT"])
                .unwrap_or_default(),
            container: first_env(&["WEFT_AZURE_CONTAINER", "AZURE_STORAGE_CONTAINER"])
                .unwrap_or_default(),
            account: first_env(&["WEFT_AZURE_ACCOUNT", "AZURE_STORAGE_ACCOUNT"])
                .unwrap_or_default(),
            key: first_env(&["WEFT_AZURE_KEY", "AZURE_STORAGE_KEY"]),
            sas_token: first_env(&["WEFT_AZURE_SAS", "AZURE_STORAGE_SAS_TOKEN"]),
            anonymous: env_flag("WEFT_AZURE_ANONYMOUS"),
            lock_lease: Duration::from_secs(
                first_env(&["WEFT_AZURE_LOCK_LEASE_SECS"])
                    .and_then(|text| text.parse::<u64>().ok())
                    .unwrap_or(900)
                    .max(MIN_LOCK_LEASE_SECS),
            ),
            request_timeout: Duration::from_secs(60),
        };
        if let Some(value) = endpoint {
            config.endpoint = value;
        }
        if let Some(value) = container {
            config.container = value;
        }
        if let Some(value) = account {
            config.account = value;
        }
        if let Some(value) = key {
            config.key = Some(value);
        }
        if let Some(value) = sas_token {
            config.sas_token = Some(value);
        }
        if let Some(value) = anonymous {
            config.anonymous = value;
        }
        if let Some(seconds) = lock_lease {
            config.lock_lease = Duration::from_secs(seconds.max(MIN_LOCK_LEASE_SECS));
        }
        let credentials = credential_forms(&config);
        if credentials > 1 {
            return Err(Error::invalid_argument(
                "Azure backend was given more than one credential form (account key, SAS token, \
                 anonymous); refusing to guess which one the endpoint should see",
            ));
        }
        if credentials == 0 {
            return Err(Error::invalid_argument(
                "Azure backend needs one credential form: an account key (--azure-key, \
                 WEFT_AZURE_KEY, AZURE_STORAGE_KEY), a SAS token (--azure-sas, WEFT_AZURE_SAS, \
                 AZURE_STORAGE_SAS_TOKEN), or --azure-anonymous for an emulator",
            ));
        }
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.endpoint.is_empty() {
            return Err(Error::invalid_argument(
                "Azure backend needs an endpoint (`--azure-endpoint`, WEFT_AZURE_ENDPOINT, or \
                 AZURE_STORAGE_BLOB_ENDPOINT)",
            ));
        }
        Endpoint::parse(&self.endpoint)?;
        if self.account.is_empty() {
            return Err(Error::invalid_argument(
                "Azure backend needs an account name (`--azure-account`, WEFT_AZURE_ACCOUNT, or \
                 AZURE_STORAGE_ACCOUNT) for the SharedKey credential scope",
            ));
        }
        if !self
            .account
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(Error::invalid_argument(format!(
                "Azure storage account `{}` must be lowercase letters and digits only",
                self.account
            )));
        }
        validate_container(&self.container)?;
        if let Some(key) = &self.key {
            let decoded = crate::b64::decode_any(key).map_err(|err| {
                Error::invalid_argument(format!(
                    "Azure account key is not valid base64 ({err}); the key itself is not printed"
                ))
            })?;
            if decoded.len() < 16 {
                return Err(Error::invalid_argument(
                    "Azure account key decodes to fewer than 16 bytes; refusing a credential that \
                     cannot be the account key",
                ));
            }
        }
        if let Some(sas) = &self.sas_token {
            if sas.starts_with('?') {
                return Err(Error::invalid_argument(
                    "Azure SAS token must not start with `?`; supply only the query string",
                ));
            }
            if sas.contains('#') || sas.contains(' ') {
                return Err(Error::invalid_argument(
                    "Azure SAS token contains a byte that cannot appear in a query string",
                ));
            }
        }
        Ok(())
    }
}

fn credential_forms(config: &AzureConfig) -> usize {
    [
        config.key.is_some(),
        config.sas_token.is_some(),
        config.anonymous,
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
}

fn validate_container(container: &str) -> Result<()> {
    if container.len() < 3 || container.len() > 63 {
        return Err(Error::invalid_argument(format!(
            "Azure container `{container}` is {} bytes; a container name is 3..=63 bytes",
            container.len()
        )));
    }
    if !container
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(Error::invalid_argument(format!(
            "Azure container `{container}` must be lowercase letters, digits, and hyphens"
        )));
    }
    if container.starts_with('-') || container.ends_with('-') {
        return Err(Error::invalid_argument(format!(
            "Azure container `{container}` must not start or end with a hyphen"
        )));
    }
    Ok(())
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
            Error::invalid_argument(format!("Azure endpoint `{text}` has no scheme"))
        })?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(Error::invalid_argument(format!(
                "Azure endpoint scheme `{scheme}` is not http or https"
            )));
        }
        let (authority, base_path) = match rest.split_once('/') {
            Some((authority, path)) => {
                let path = path.trim_matches('/');
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
            return Err(Error::invalid_argument("Azure endpoint has an empty host"));
        }
        Ok(Endpoint {
            scheme,
            authority,
            base_path,
        })
    }
}

/// How the endpoint compares an `If-Match` header with its own ETag.
///
/// Azure returns quoted entity tags and accepts the quoted value. The form is
/// still *measured* at a writable open for the same reason the S3 backend
/// measures it: a proxy or emulator may compare the header literally against an
/// unquoted tag. A backend that passes neither form is refused.
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

struct HttpResponse {
    status: u16,
    etag: Option<String>,
    content_length: Option<u64>,
    body: Vec<u8>,
}

struct AzureStats {
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

impl Default for AzureStats {
    fn default() -> Self {
        AzureStats {
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

struct LockLease {
    owner: String,
    /// Monotonic fencing token. Bumped by every takeover, never reused.
    fence: u64,
    /// Number of conditional writes this handle has made to the LOCK blob.
    sequence: u64,
    /// Absolute lease deadline recorded in the published blob.
    deadline_ns: i64,
    renewed_ns: i64,
    /// ETag the endpoint returned for the bytes published at `sequence`.
    etag: String,
}

/// Azure Blob-backed durable store namespace.
pub struct AzureBackend {
    root: PathBuf,
    config: AzureConfig,
    endpoint: Mutex<Option<Endpoint>>,
    agent: Mutex<Option<ureq::Agent>>,
    /// Decoded SharedKey credential, or `None` for SAS/anonymous.
    credential: Mutex<Option<SharedKey>>,
    lock: Mutex<Option<LockLease>>,
    stats: AzureStats,
    diagnostics: Mutex<Vec<String>>,
    probe: Mutex<Option<()>>,
    /// How this endpoint compares `If-Match`; measured, never guessed.
    if_match_form: Mutex<IfMatchForm>,
}

#[derive(Clone)]
struct SharedKey {
    account: String,
    key: Vec<u8>,
}

impl AzureBackend {
    pub fn new(root: impl Into<PathBuf>, config: AzureConfig) -> AzureBackend {
        AzureBackend {
            root: root.into(),
            config,
            endpoint: Mutex::new(None),
            agent: Mutex::new(None),
            credential: Mutex::new(None),
            lock: Mutex::new(None),
            stats: AzureStats::default(),
            diagnostics: Mutex::new(Vec::new()),
            probe: Mutex::new(None),
            if_match_form: Mutex::new(IfMatchForm::AsReturned),
        }
    }

    /// Declared guarantees for an Azure Blob endpoint.
    pub fn capabilities() -> Capabilities {
        Capabilities {
            atomic_publish: true,
            // Service acknowledgement, not a local fsync. Nothing here claims
            // media, replication, or power-loss durability.
            durable_fsync: true,
            true_range_reads: true,
            max_object_bytes: MAX_OBJECT_BYTES,
            conditional_head: true,
            consistency: "single-endpoint Azure Blob-compatible store; read-after-write and \
                          list-after-write for published blobs; conditional writes \
                          (If-None-Match: */If-Match: <ETag>) are required and probed at \
                          writable open; a monotonic fencing token is carried in the LOCK blob"
                .to_owned(),
            publication: "Azure Put Blob (x-ms-blob-type: BlockBlob) with If-None-Match: * \
                          (create-once) or If-Match: <observed ETag> (compare-and-swap); both \
                          forms and a stale-ETag refusal are measured at writable open; every \
                          head publication is preceded by one conditional write on LOCK that \
                          carries the fencing token and names the head, and release is a \
                          conditional content-addressed tombstone"
                .to_owned(),
        }
    }

    pub fn config(&self) -> &AzureConfig {
        &self.config
    }

    /// The `If-Match` comparison form this endpoint was measured to honour.
    pub fn if_match_form(&self) -> IfMatchForm {
        *self
            .if_match_form
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    fn shared_key(&self) -> Result<Option<SharedKey>> {
        if self.config.key.is_none() {
            return Ok(None);
        }
        let mut guard = self
            .credential
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(credential) = guard.as_ref() {
            return Ok(Some(credential.clone()));
        }
        let encoded = self.config.key.as_deref().unwrap_or_default();
        let key = crate::b64::decode_any(encoded).map_err(|err| {
            Error::invalid_argument(format!(
                "Azure account key is not valid base64 ({err}); the key itself is not printed"
            ))
        })?;
        let credential = SharedKey {
            account: self.config.account.clone(),
            key,
        };
        *guard = Some(credential.clone());
        Ok(Some(credential))
    }

    fn object_name(key: &Key) -> Result<String> {
        match key {
            Key::Root | Key::Prefix(_) => Err(Error::invalid_argument(
                "Azure backend cannot address a namespace prefix as a blob",
            )),
            Key::Other { name, .. } => Ok(name.clone()),
            other => Ok(other.display_path()),
        }
    }

    /// One signed request.
    ///
    /// `blob` is `None` for container-level operations. `query` is signed and
    /// sent in sorted order; a SAS token is appended verbatim after it.
    fn request(
        &self,
        method: &str,
        blob: Option<&str>,
        query: &[(String, String)],
        body: &[u8],
        extra: &[(&str, String)],
    ) -> Result<HttpResponse> {
        let endpoint = self.endpoint()?;
        let mut path = format!("{}/{}", endpoint.base_path, self.config.container);
        if let Some(blob) = blob {
            path.push('/');
            path.push_str(&encode_path(blob));
        }

        let (date, _) = rfc1123_now();
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("x-ms-date".to_owned(), date);
        headers.insert("x-ms-version".to_owned(), API_VERSION.to_owned());
        let sends_body = matches!(method, "PUT" | "POST" | "PATCH");
        if sends_body {
            headers.insert("content-length".to_owned(), body.len().to_string());
            headers.insert(
                "content-type".to_owned(),
                "application/octet-stream".to_owned(),
            );
        }
        for (name, value) in extra {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }

        let canonical_query = canonical_query(query);
        let mut url_query = canonical_query.clone();
        if let Some(sas) = &self.config.sas_token {
            if url_query.is_empty() {
                url_query = sas.clone();
            } else {
                url_query = format!("{url_query}&{sas}");
            }
        }

        let mut url = format!("{}://{}{}", endpoint.scheme, endpoint.authority, path);
        if !url_query.is_empty() {
            url.push('?');
            url.push_str(&url_query);
        }
        let mut request = self.agent().request(method, &url);
        for (name, value) in &headers {
            request = request.set(name, value);
        }
        if let Some(credential) = self.shared_key()? {
            let signature = shared_key_signature(
                &credential,
                method,
                &headers,
                sends_body,
                body.len() as u64,
                &path,
                &canonical_query,
            );
            request = request.set(
                "authorization",
                &format!(
                    "SharedKey {}:{}",
                    credential.account,
                    crate::b64::encode_standard(&signature)
                ),
            );
        }

        let outcome = if sends_body {
            request.send_bytes(body)
        } else {
            request.call()
        };
        let response = match outcome {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let etag = response.header("etag").map(str::to_owned);
                let content_length = response
                    .header("content-length")
                    .and_then(|value| value.parse::<u64>().ok());
                let body = read_bounded(response, content_length, MAX_XML_BYTES, method, blob)?;
                return Ok(HttpResponse {
                    status,
                    etag,
                    content_length,
                    body,
                });
            }
            Err(err) => {
                // A transport failure stays `Io`: an unreachable endpoint is not
                // a damaged store (the R13 rule, reused).
                return Err(Error::io(format!(
                    "Azure {method} {} failed: {err}",
                    self.describe(blob)
                )));
            }
        };
        let status = response.status();
        let etag = response.header("etag").map(str::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        let cap = if method == "GET" && blob.is_some() {
            MAX_OBJECT_BYTES
        } else {
            MAX_XML_BYTES
        };
        let body = read_bounded(response, content_length, cap, method, blob)?;
        Ok(HttpResponse {
            status,
            etag,
            content_length,
            body,
        })
    }

    fn describe(&self, blob: Option<&str>) -> String {
        match blob {
            Some(blob) => format!("azure://{}/{}", self.config.container, blob),
            None => format!("azure container {}", self.config.container),
        }
    }

    /// `Get Blob Properties`: length and ETag, or `None` when absent.
    fn head(&self, key: &Key) -> Result<Option<(u64, String)>> {
        let name = Self::object_name(key)?;
        let response = self.request("HEAD", Some(&name), &[], &[], &[])?;
        match response.status {
            200 => {
                let etag = response.etag.clone().ok_or_else(|| {
                    Error::unsupported_version(format!(
                        "Azure did not return an ETag for `{name}`; a fenced writer needs one"
                    ))
                })?;
                Ok(Some((response.content_length.unwrap_or(0), etag)))
            }
            404 => Ok(None),
            403 => Err(Error::unauthorized(format!(
                "Azure denied HEAD `{name}`: {}",
                response_body(&response.body)
            ))),
            status => Err(Error::io(format!(
                "Azure HEAD `{name}` returned HTTP {status}"
            ))),
        }
    }

    /// `Get Blob`, whole or ranged.
    fn get(&self, key: &Key, range: Option<(u64, u64)>) -> Result<HttpResponse> {
        let name = Self::object_name(key)?;
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some((offset, len)) = range {
            if len > 0 {
                let end = offset
                    .checked_add(len - 1)
                    .ok_or_else(|| Error::limit_exceeded("range end overflows u64"))?;
                extra.push(("range", format!("bytes={offset}-{end}")));
            }
        }
        self.request("GET", Some(&name), &[], &[], &extra)
    }

    /// Read a blob and the ETag those bytes belong to, in one request.
    fn get_with_etag(&self, key: &Key) -> Result<Option<(Vec<u8>, String)>> {
        let response = self.get(key, None)?;
        match response.status {
            200 => {
                let etag = response.etag.ok_or_else(|| {
                    Error::unsupported_version(format!(
                        "Azure did not return an ETag for {}; a compare-and-swap would have no \
                         token",
                        key.display_path()
                    ))
                })?;
                Ok(Some((response.body, etag)))
            }
            404 => Ok(None),
            403 => Err(Error::unauthorized(format!(
                "Azure denied reading {}: {}",
                key.display_path(),
                response_body(&response.body)
            ))),
            status => Err(Error::io(format!(
                "Azure GET {} returned HTTP {status}",
                key.display_path()
            ))),
        }
    }

    /// Publish `bytes` subject to `if_match`.
    ///
    /// Returns the ETag of the published blob, or `None` when the precondition
    /// was lost (`409 BlobAlreadyExists` for a create-once, `412` for a stale
    /// ETag).
    fn put_blob(&self, key: &Key, bytes: &[u8], if_match: Option<&str>) -> Result<Option<String>> {
        let header = if_match.map(|etag| self.if_match_form().encode(etag));
        self.put_blob_header(key, bytes, header, true)
    }

    /// `if_match_value` is the *literal* header value; `None` sends
    /// `If-None-Match: *`. Only the probe passes a value that was not encoded
    /// through [`IfMatchForm`].
    fn put_blob_header(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match_value: Option<String>,
        count_conflict: bool,
    ) -> Result<Option<String>> {
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "blob is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
                bytes.len()
            )));
        }
        let name = Self::object_name(key)?;
        let mut extra: Vec<(&str, String)> = vec![("x-ms-blob-type", "BlockBlob".to_owned())];
        match if_match_value {
            Some(etag) => extra.push(("if-match", etag)),
            None => extra.push(("if-none-match", "*".to_owned())),
        }
        let response = self.request("PUT", Some(&name), &[], bytes, &extra)?;
        match response.status {
            200 | 201 | 204 => {
                self.stats.writes.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                let etag = response.etag.ok_or_else(|| {
                    Error::unsupported_version(format!(
                        "Azure accepted a Put Blob for `{name}` without returning an ETag; \
                         refusing to continue because the next conditional write would have no \
                         token"
                    ))
                })?;
                Ok(Some(etag))
            }
            // Azure reports a failed create-once as 409 BlobAlreadyExists and a
            // failed If-Match as 412. Both mean "the precondition did not hold".
            409 | 412 => {
                if count_conflict {
                    self.stats
                        .conditional_conflicts
                        .fetch_add(1, Ordering::Relaxed);
                }
                Ok(None)
            }
            403 => Err(Error::unauthorized(format!("Azure denied PUT `{name}`"))),
            413 => Err(Error::limit_exceeded(format!(
                "Azure rejected the {}-byte blob `{name}` as too large",
                bytes.len()
            ))),
            status => Err(Error::io(format!(
                "Azure PUT `{name}` returned HTTP {status}: {}",
                response_body(&response.body)
            ))),
        }
    }

    fn delete_blob(&self, key: &Key) -> Result<()> {
        let name = Self::object_name(key)?;
        let response = self.request(
            "DELETE",
            Some(&name),
            &[],
            &[],
            &[("x-ms-delete-snapshots", "include".to_owned())],
        )?;
        match response.status {
            200 | 202 | 204 => Ok(()),
            404 => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
            403 => Err(Error::unauthorized(format!(
                "Azure denied deleting `{name}`"
            ))),
            status => Err(Error::io(format!(
                "Azure DELETE `{name}` returned HTTP {status}: {}",
                response_body(&response.body)
            ))),
        }
    }

    fn list_blobs(&self, prefix: &Key) -> Result<Vec<Key>> {
        let Key::Prefix(kind) = prefix else {
            return Err(Error::invalid_argument(
                "list_prefix requires a Key::Prefix selector",
            ));
        };
        let Some(prefix_path) = prefix_for_kind(*kind) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut query: Vec<(String, String)> = vec![
                ("restype".to_owned(), "container".to_owned()),
                ("comp".to_owned(), "list".to_owned()),
                ("prefix".to_owned(), prefix_path.to_owned()),
                ("maxresults".to_owned(), "1000".to_owned()),
            ];
            if let Some(marker) = &marker {
                query.push(("marker".to_owned(), marker.clone()));
            }
            let response = self.request("GET", None, &query, &[], &[])?;
            match response.status {
                200 => {}
                404 => return Ok(out),
                403 => {
                    return Err(Error::unauthorized(format!(
                        "Azure denied listing `{prefix_path}`"
                    )))
                }
                status => {
                    return Err(Error::io(format!(
                        "Azure list returned HTTP {status}: {}",
                        response_body(&response.body)
                    )))
                }
            }
            let text = String::from_utf8_lossy(&response.body);
            for block in xml_blocks(&text, "Blob") {
                let Some(full) = xml_values(&block, "Name").into_iter().next() else {
                    continue;
                };
                let relative = full.strip_prefix(prefix_path).unwrap_or(&full);
                out.push(
                    classify_relative(*kind, relative).unwrap_or_else(|| Key::Other {
                        kind: *kind,
                        name: full.clone(),
                    }),
                );
            }
            let next = xml_values(&text, "NextMarker")
                .into_iter()
                .next()
                .filter(|value| !value.is_empty());
            match next {
                Some(next) => marker = Some(next),
                None => break,
            }
        }
        out.sort();
        Ok(out)
    }

    /// Ensure the configured container exists. Idempotent.
    pub fn ensure_container(&self) -> Result<()> {
        let response = self.request(
            "HEAD",
            None,
            &[("restype".to_owned(), "container".to_owned())],
            &[],
            &[],
        )?;
        match response.status {
            200 | 204 => return Ok(()),
            404 => {}
            403 => {
                return Err(Error::unauthorized(format!(
                    "Azure denied reading container `{}`",
                    self.config.container
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "Azure Get Container Properties returned HTTP {status}: {}",
                    response_body(&response.body)
                )))
            }
        }
        let response = self.request(
            "PUT",
            None,
            &[("restype".to_owned(), "container".to_owned())],
            &[],
            &[],
        )?;
        match response.status {
            200 | 201 | 204 => Ok(()),
            // A concurrent creator won; that is the desired outcome.
            409 => Ok(()),
            403 => Err(Error::unauthorized(format!(
                "Azure denied creating container `{}`",
                self.config.container
            ))),
            status => Err(Error::io(format!(
                "Azure Create Container `{}` returned HTTP {status}: {}",
                self.config.container,
                response_body(&response.body)
            ))),
        }
    }

    /// Prove the endpoint honours both conditional forms the store depends on.
    ///
    /// Create-once must be visible; a second create must be refused; a matching
    /// `If-Match` must be accepted; and — the load-bearing check — a *stale*
    /// `If-Match` must be refused with the winner's bytes surviving. An endpoint
    /// that accepts a stale value is refused `UnsupportedVersion`, never
    /// downgraded.
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
        let cleanup = || {
            let _ = self.delete_blob(&key);
        };

        if self
            .put_blob_header(&key, b"probe-one", None, false)?
            .is_none()
        {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: a fresh blob reported a failed precondition",
            ));
        }
        if self
            .put_blob_header(&key, b"probe-two", None, false)?
            .is_some()
        {
            cleanup();
            return Err(Error::unsupported_version(
                "Azure-compatible endpoint ignored If-None-Match: *; refusing a writable store \
                 because create-once publication cannot be guaranteed",
            ));
        }
        let Some((current, etag)) = self.get_with_etag(&key)? else {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: the first writer's blob is not readable",
            ));
        };
        if current != b"probe-one" {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: a rejected create replaced the first writer's bytes",
            ));
        }

        let dequoted = strip_etag_quotes(&etag);
        let as_returned = self.put_blob_header(&key, b"probe-three", Some(etag.clone()), false)?;
        let (form, matching_value) = if as_returned.is_some() {
            (IfMatchForm::AsReturned, etag.clone())
        } else if dequoted != etag {
            match self.put_blob_header(&key, b"probe-three", Some(dequoted.clone()), false)? {
                Some(_) => (IfMatchForm::Dequoted, dequoted),
                None => {
                    cleanup();
                    return Err(Error::unsupported_version(format!(
                        "Azure-compatible endpoint rejected a matching If-Match in both forms \
                         (as returned {etag:?} and dequoted {dequoted:?}); refusing a writable \
                         store because head publication cannot be proven"
                    )));
                }
            }
        } else {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: a matching If-Match was unexpectedly rejected",
            ));
        };
        let Some((after_match, _)) = self.get_with_etag(&key)? else {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: the matching conditional write is not readable",
            ));
        };
        if after_match != b"probe-three" {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: the matching conditional write is not readable",
            ));
        }
        if self
            .put_blob_header(&key, b"probe-four", Some(matching_value), false)?
            .is_some()
        {
            cleanup();
            return Err(Error::unsupported_version(
                "Azure-compatible endpoint accepted a stale If-Match; refusing a writable store \
                 because head publication could lose a race silently",
            ));
        }
        let Some((after_stale, _)) = self.get_with_etag(&key)? else {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: the blob disappeared after a refused stale write",
            ));
        };
        if after_stale != b"probe-three" {
            cleanup();
            return Err(Error::io(
                "Azure conditional probe: a refused stale If-Match changed the blob",
            ));
        }
        *self
            .if_match_form
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = form;
        if form == IfMatchForm::Dequoted {
            self.note(format!(
                "endpoint compares If-Match literally: ETag {etag} is accepted only when the \
                 surrounding quotes are stripped; using the dequoted form for every conditional \
                 write (matching accepted, stale refused, winner bytes kept)"
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
        // The writer lock is the first conditional write of a writable open (the
        // store locks before it runs the plane probe), so negotiate the If-Match
        // form here. Skipping this would make an endpoint that compares entity
        // tags literally reject every expired-lease reclaim.
        self.probe_conditional_writes()?;
        let now = now_ns();
        let owner = format!("{}-{}-{}", std::process::id(), unique_counter(), now);
        let deadline = now.saturating_add(self.lease_ns());
        let key = Key::lock();
        for _ in 0..4 {
            match self.get_with_etag(&key)? {
                None => {
                    // No record at all: create it once. The first lease holder
                    // takes fencing token 1.
                    let body = encode_lock(&owner, 1, 1, deadline, now, None);
                    if let Some(etag) = self.put_blob_header(&key, body.as_bytes(), None, false)? {
                        self.install_lease(owner, 1, 1, deadline, now, etag);
                        return Ok(());
                    }
                    // Another writer created it between the GET and the PUT.
                    continue;
                }
                Some((observed_bytes, etag)) => {
                    let Some(observed) = parse_lock(&observed_bytes) else {
                        return Err(Error::busy(
                            "store LOCK blob is unreadable; refusing a writable open rather than \
                             guessing whether a writer holds it",
                        ));
                    };
                    if observed.is_live(now) {
                        // The caller receives one fixed, non-disclosing sentence;
                        // owner, fence, and remaining lease go to diagnostics.
                        self.note(format!(
                            "writable open refused: the LOCK is live (owner {} fence {}, lease \
                             ends in {} ms)",
                            observed.owner,
                            observed.fence,
                            (observed.deadline_ns - now) / 1_000_000
                        ));
                        return Err(Error::busy(
                            "store is locked by another Azure writer (live LOCK blob)",
                        ));
                    }
                    let fence = observed.fence.checked_add(1).ok_or_else(|| {
                        Error::limit_exceeded("Azure fencing token would overflow u64")
                    })?;
                    let body = encode_lock(&owner, fence, 1, deadline, now, None);
                    match self.put_blob(&key, body.as_bytes(), Some(&etag))? {
                        Some(etag) => {
                            self.note(format!(
                                "{} Azure writer lock: fence {} -> {}",
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
            }
        }
        Err(Error::busy(
            "could not acquire the Azure writer lock after four conditional attempts",
        ))
    }

    #[allow(clippy::too_many_arguments)]
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

    /// Prove this handle may publish right now, renewing the lease when one
    /// publication budget remains.
    fn fence_lease(&self) -> Result<()> {
        self.fence_lease_for(None)
    }

    /// The same conditional write, additionally naming the head about to be
    /// published.
    fn fence_head(&self, generation: u64, commit: &Hash32) -> Result<()> {
        self.fence_lease_for(Some((generation, commit)))
    }

    fn fence_lease_for(&self, pending: Option<(u64, &Hash32)>) -> Result<()> {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = guard.as_mut() else {
            return Err(Error::busy(
                "Azure writer holds no lease; refusing to publish without a fenced lease",
            ));
        };
        let now = now_ns();
        if now >= lease.deadline_ns {
            let owner = lease.owner.clone();
            let fence = lease.fence;
            let expired_ms = (now - lease.deadline_ns) / 1_000_000;
            let observed = self.read_lock_record().ok().flatten();
            self.stats
                .lease_expired_publications
                .fetch_add(1, Ordering::Relaxed);
            return Err(Error::busy(format!(
                "Azure writer lease deadline passed {expired_ms} ms ago (owner {owner} fence \
                 {fence}); publication refused before the head was touched{}",
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
        // This is the fence. `If-Match` names the ETag this handle published
        // last, so it succeeds only while the blob still carries this writer's
        // fencing token. A successor's reclaim rewrote the blob, so this fails
        // with 412 and nothing is published.
        match self.put_blob(&Key::lock(), body.as_bytes(), Some(&lease.etag))? {
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
                    "Azure writer lease was taken over: the conditional fence CAS for owner {} \
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
            status => Err(Error::io(format!("Azure LOCK read returned HTTP {status}"))),
        }
    }

    /// Release the lease with a content-addressed tombstone published under
    /// `If-Match` on the ETag this writer observed.
    ///
    /// Never an unconditional delete after a read: if the lock was replaced
    /// between the read and this conditional write, the write fails and the
    /// successor's lock is left byte-identical.
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
        let observed = match self.get_with_etag(&key) {
            Ok(observed) => observed,
            Err(err) => {
                self.note(format!("could not read LOCK to release it: {err}"));
                return;
            }
        };
        let Some((body, etag)) = observed else {
            return;
        };
        let Some(record) = parse_lock(&body) else {
            self.note("refusing to release an unreadable LOCK record".to_owned());
            return;
        };
        if record.owner != lease.owner || record.fence != lease.fence {
            self.note(format!(
                "refusing to release the Azure writer lock: it now names owner {} fence {} and \
                 this handle holds fence {}; the conditional release was not attempted",
                record.owner, record.fence, lease.fence
            ));
            return;
        }
        if etag != lease.etag {
            // The bytes are this writer's but the version moved: either this
            // handle published again without updating its lease (impossible) or
            // the blob was replaced with identical bytes. Condition on what we
            // read, which is the version the tombstone will name.
            self.note(
                "the LOCK ETag differs from this handle's last published ETag; conditioning the \
                 conditional tombstone on the version just read"
                    .to_owned(),
            );
        }
        let released_from = sha256_hex(&body);
        let tombstone = encode_released(
            &lease.owner,
            lease.fence,
            lease.sequence.saturating_add(1),
            now_ns(),
            &released_from,
        );
        match self.put_blob(&key, tombstone.as_bytes(), Some(&etag)) {
            Ok(Some(_)) => self.note(format!(
                "released the Azure writer lock with a conditional tombstone (fence {})",
                lease.fence
            )),
            Ok(None) => self.note(format!(
                "refusing to release the Azure writer lock: it was replaced between the read and \
                 the conditional tombstone (this writer held fence {}); the successor's lock is \
                 intact",
                lease.fence
            )),
            Err(err) => self.note(format!(
                "conditional release of the Azure writer lock failed, nothing was deleted: {err}"
            )),
        }
    }

    /// Court/operator reset: replace the published `LOCK` with a released
    /// tombstone under a conditional write. Never called by the store, which
    /// only ever releases a lease it still holds.
    pub fn release_stale_lease(&self) -> Result<Option<PublishedLock>> {
        self.probe_conditional_writes()?;
        let Some((body, etag)) = self.get_with_etag(&Key::lock())? else {
            return Ok(None);
        };
        let Some(record) = parse_lock(&body) else {
            return Err(Error::corrupt("published LOCK record is unreadable"));
        };
        let fence = record
            .fence
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("Azure fencing token would overflow u64"))?;
        let tombstone = encode_released(
            &record.owner,
            fence,
            record.sequence.saturating_add(1),
            now_ns(),
            &sha256_hex(&body),
        );
        match self.put_blob(&Key::lock(), tombstone.as_bytes(), Some(&etag))? {
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

    /// The window a single publication may still occupy. A writer renews when
    /// one budget remains, and a successor may not take over before the
    /// recorded deadline, so a head publication always finishes inside the
    /// lease it was started under.
    fn publication_budget_ns(&self) -> i64 {
        (self.lease_ns() / 2).max(1)
    }

    /// Ensure the container exists and prove the conditional forms.
    ///
    /// Inherent so a caller holding a concrete `AzureBackend` does not have to
    /// disambiguate between the two plane traits.
    pub fn init_layout(&mut self) -> Result<()> {
        self.ensure_container()?;
        self.probe_conditional_writes()
    }

    /// Ensure the container exists and prove the conditional forms.
    pub fn ensure_layout(&mut self) -> Result<()> {
        self.ensure_container()?;
        self.probe_conditional_writes()
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
        }
    }

    fn backend_diagnostics(&self) -> Vec<String> {
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
                "Azure If-Match form measured at open: {}",
                self.if_match_form().label()
            ));
        }
        let conflicts = self.stats.conditional_conflicts.load(Ordering::Relaxed);
        if conflicts > 0 {
            out.push(format!(
                "Azure conditional writes observed {conflicts} lost/retried race(s)"
            ));
        }
        let fences = self.stats.fence_checks.load(Ordering::Relaxed);
        let renewals = self.stats.lease_renewals.load(Ordering::Relaxed);
        if fences > 0 {
            out.push(format!(
                "Azure fence CAS proved the lease {fences} time(s), {renewals} with a renewal"
            ));
        }
        let refusals = self.stats.fence_refusals.load(Ordering::Relaxed);
        let expired = self
            .stats
            .lease_expired_publications
            .load(Ordering::Relaxed);
        if refusals > 0 || expired > 0 {
            out.push(format!(
                "Azure fence refused {refusals} publication(s) after a takeover and {expired} \
                 after this writer's own deadline"
            ));
        }
        out
    }
}

impl Drop for AzureBackend {
    fn drop(&mut self) {
        self.release_lock();
    }
}

// ---------------------------------------------------------------------------
// Plane implementations
// ---------------------------------------------------------------------------

impl BlobPlane for AzureBackend {
    fn blob_label(&self) -> &'static str {
        "azure"
    }

    fn blob_capabilities(&self) -> BlobCapabilities {
        BlobCapabilities::from_capabilities("azure", &AzureBackend::capabilities())
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>> {
        StorageBackend::read_all(self, key)
    }

    fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        StorageBackend::read(self, key, offset, len)
    }

    fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        if self.put_blob_header(key, bytes, None, false)?.is_some() {
            return Ok(());
        }
        // The key exists. Identical bytes are idempotent; different bytes under
        // a content-addressed name are a real conflict. A blob that disappears
        // here means the create-once was lost to a concurrent delete, which is
        // `Busy` and never silently accepted.
        let Some((existing, _)) = self.get_with_etag(key)? else {
            return Err(Error::busy(format!(
                "key {} was reported present but disappeared during create-once publication",
                key.display_path()
            )));
        };
        if existing == bytes {
            Ok(())
        } else {
            Err(Error::exists(format!(
                "key {} already exists with different bytes",
                key.display_path()
            )))
        }
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        self.delete_blob(key)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        match self.head(key)? {
            Some((length, _)) => Ok(length),
            None => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
        }
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        Ok(self.head(key)?.is_some())
    }

    fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_blobs(prefix)
    }

    fn clean_stale(&mut self, _grace_secs: u64) -> Result<()> {
        // There is no staging namespace: every upload is atomic under its final
        // key. The only `tmp/` blobs are capability-probe keys, which a crashed
        // process can leave behind; the writer lock is held here, so deleting
        // all of them is safe.
        let keys = self.list_blobs(&Key::prefix(KeyKind::Tmp))?;
        for key in keys {
            let _ = self.delete_blob(&key);
        }
        Ok(())
    }

    fn init_layout(&mut self) -> Result<()> {
        AzureBackend::init_layout(self)
    }

    fn ensure_layout(&mut self) -> Result<()> {
        AzureBackend::ensure_layout(self)
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

impl CommitPlane for AzureBackend {
    fn commit_label(&self) -> &'static str {
        "azure"
    }

    fn commit_capabilities(&self) -> CommitCapabilities {
        CommitCapabilities::from_capabilities("azure", &AzureBackend::capabilities())
    }

    fn read_control(&self, key: &Key) -> Result<Option<ControlRecord>> {
        match self.get(key, None)? {
            response if response.status == 200 => Ok(Some(ControlRecord {
                token: Some(control_token(&response.body)),
                bytes: response.body,
            })),
            response if response.status == 404 => Ok(None),
            response if response.status == 403 => Err(Error::unauthorized(format!(
                "Azure denied reading {}",
                key.display_path()
            ))),
            response => Err(Error::io(format!(
                "Azure read {} returned HTTP {}",
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
        // Read the record and the ETag those bytes belong to, require the
        // caller's content token to match, then condition the upload on that
        // exact entity tag. A lost race is 412/409 and stays `Busy`.
        let observed = self.get_with_etag(key)?;
        let token = observed.as_ref().map(|(current, _)| control_token(current));
        if token.as_deref() != expected {
            return Err(Error::busy(format!(
                "control record {} changed since it was read; compare-and-swap refused",
                key.display_path()
            )));
        }
        match observed {
            Some((_, etag)) => match self.put_blob(key, bytes, Some(etag.as_str()))? {
                Some(_) => Ok(ControlRecord {
                    token: Some(control_token(bytes)),
                    bytes: bytes.to_vec(),
                }),
                None => Err(Error::busy(format!(
                    "Azure ETag precondition lost a race for {}",
                    key.display_path()
                ))),
            },
            None => match self.put_blob_header(key, bytes, None, true)? {
                Some(_) => Ok(ControlRecord {
                    token: Some(control_token(bytes)),
                    bytes: bytes.to_vec(),
                }),
                None => Err(Error::busy(format!(
                    "Azure create-once precondition lost a race for {}",
                    key.display_path()
                ))),
            },
        }
    }

    fn write_control(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        // Unconditional replacement, but conditioned on the observed ETag so a
        // concurrent update is retried rather than clobbered.
        for _ in 0..8 {
            match self.get_with_etag(key)? {
                Some((_, etag)) => {
                    if self.put_blob(key, bytes, Some(&etag))?.is_some() {
                        return Ok(());
                    }
                }
                None => {
                    if self.put_blob_header(key, bytes, None, false)?.is_some() {
                        return Ok(());
                    }
                }
            }
        }
        Err(Error::busy(format!(
            "conditional replacement of {} kept losing",
            key.display_path()
        )))
    }

    fn create_control(&mut self, key: &Key, len: u64) -> Result<()> {
        let len = usize::try_from(len)
            .map_err(|_| Error::limit_exceeded("zeroed blob does not fit the address space"))?;
        if len as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "zeroed blob is {len} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound"
            )));
        }
        BlobPlane::put_if_absent(self, key, &vec![0u8; len])
    }

    fn put_control_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        BlobPlane::put_if_absent(self, key, bytes)
    }

    fn delete_control(&mut self, key: &Key) -> Result<()> {
        self.delete_blob(key)
    }

    fn control_len(&self, key: &Key) -> Result<u64> {
        BlobPlane::len(self, key)
    }

    fn control_exists(&self, key: &Key) -> Result<bool> {
        BlobPlane::exists(self, key)
    }

    fn list_control(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_blobs(prefix)
    }

    fn modified(&self, _key: &Key) -> Result<Option<SystemTime>> {
        // Azure returns `Last-Modified` as an RFC 1123 string, and this crate has
        // no date parser. The store does not depend on it; returning `None` is
        // honest rather than approximating.
        Ok(None)
    }

    fn sync_dir(&mut self, _key: &Key) -> Result<()> {
        // A successful Put Blob is acknowledged by the service; there is no
        // separate fsync primitive.
        Ok(())
    }

    fn init_layout(&mut self) -> Result<()> {
        AzureBackend::init_layout(self)
    }

    fn ensure_layout(&mut self) -> Result<()> {
        AzureBackend::ensure_layout(self)
    }

    fn acquire_lock(&mut self) -> Result<()> {
        AzureBackend::acquire_lock(self)
    }

    fn verify_write_lease(&mut self) -> Result<()> {
        AzureBackend::fence_lease(self)
    }

    fn fence_head_publication(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
    ) -> Result<()> {
        AzureBackend::fence_head(self, generation, commit)
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

// ---------------------------------------------------------------------------
// StorageBackend: one implementation of each physical operation
// ---------------------------------------------------------------------------

impl StorageBackend for AzureBackend {
    fn label(&self) -> &'static str {
        "azure"
    }

    fn capabilities(&self) -> Capabilities {
        AzureBackend::capabilities()
    }

    fn read(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let response = self.get(key, Some((offset, len)))?;
        let bytes = match response.status {
            200 => {
                // The server ignored Range. Slice the whole blob.
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
                // End of blob is not an error; only an offset strictly beyond it
                // is rejected.
                let total = BlobPlane::len(self, key)?;
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
                    "Azure denied {}: {}",
                    key.display_path(),
                    response_body(&response.body)
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "Azure range GET {} returned HTTP {status}",
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
                    "Azure denied {}: {}",
                    key.display_path(),
                    response_body(&response.body)
                )))
            }
            status => {
                return Err(Error::io(format!(
                    "Azure GET {} returned HTTP {status}",
                    key.display_path()
                )))
            }
        };
        if bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "blob {} is {} bytes, above the backend's {MAX_OBJECT_BYTES}-byte bound",
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
        // Only the two-slot superblock takes this path. Azure has no in-place
        // range write for a block blob: read the whole blob, patch the bytes,
        // and publish the result under the ETag that was read.
        for _ in 0..8 {
            let Some((mut image, etag)) = self.get_with_etag(key)? else {
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
            if self.put_blob(key, &image, Some(&etag))?.is_some() {
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
        self.delete_blob(key)
    }

    fn list_prefix(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.list_blobs(prefix)
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
        AzureBackend::fence_lease(self)
    }

    fn fence_head_publication(
        &mut self,
        generation: u64,
        commit: &crate::identity::Hash32,
    ) -> Result<()> {
        AzureBackend::fence_head(self, generation, commit)
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

// ---------------------------------------------------------------------------
// Test and court hooks
// ---------------------------------------------------------------------------

impl AzureBackend {
    /// Test hook: the current ETag for a key.
    #[doc(hidden)]
    pub fn etag_for_test(&self, key: &Key) -> Result<String> {
        match self.head(key)? {
            Some((_, etag)) => Ok(etag),
            None => Err(Error::not_found(format!(
                "key {} does not exist",
                key.display_path()
            ))),
        }
    }

    /// Test hook: perform a conditional whole-blob PUT with a caller-supplied
    /// `If-Match` ETag. Returns `false` on a failed precondition.
    #[doc(hidden)]
    pub fn conditional_put_for_test(
        &self,
        key: &Key,
        bytes: &[u8],
        if_match: &str,
    ) -> Result<bool> {
        self.probe_conditional_writes()?;
        Ok(self.put_blob(key, bytes, Some(if_match))?.is_some())
    }

    /// Test hook: put bytes with `If-None-Match: *`. Returns `false` when the
    /// blob already exists (whatever its bytes).
    #[doc(hidden)]
    pub fn create_once_for_test(&self, key: &Key, bytes: &[u8]) -> Result<bool> {
        Ok(self.put_blob_header(key, bytes, None, false)?.is_some())
    }
}

pub use crate::writer_fence::PublishedLock;

fn encode_lock(
    owner: &str,
    fence: u64,
    sequence: u64,
    deadline_ns: i64,
    renewed_ns: i64,
    pending: Option<(u64, &Hash32)>,
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
) -> String {
    crate::writer_fence::encode_released(
        RELEASED_SCHEMA,
        owner,
        fence,
        sequence,
        released_ns,
        released_from,
    )
}

fn parse_lock(bytes: &[u8]) -> Option<PublishedLock> {
    crate::writer_fence::parse_lock(bytes, LOCK_SCHEMA, LOCK_SCHEMA_V1, RELEASED_SCHEMA)
}

fn describe_observed(observed: &Option<PublishedLock>, owner: &str, fence: u64) -> String {
    crate::writer_fence::describe_observed(observed, owner, fence)
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::writer_fence::sha256_hex(bytes)
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// The documented `SharedKey` string-to-sign and its HMAC-SHA256 signature.
///
/// Field order is fixed by the Blob service specification; `Content-Length` is
/// empty when the body is empty, and `Date` is empty because `x-ms-date` is
/// always sent. Header values are the ones actually transmitted, so the
/// signature cannot describe a request other than the one made.
#[allow(clippy::too_many_arguments)]
fn shared_key_signature(
    credential: &SharedKey,
    method: &str,
    headers: &BTreeMap<String, String>,
    sends_body: bool,
    body_len: u64,
    request_path: &str,
    canonical_query: &str,
) -> Vec<u8> {
    let string_to_sign = shared_key_string_to_sign(
        credential.account.as_str(),
        method,
        headers,
        sends_body,
        body_len,
        request_path,
        canonical_query,
    );
    hmac_sha256(&credential.key, string_to_sign.as_bytes())
}

/// The exact string-to-sign, split out so a test can assert the field order
/// and the canonicalised resource without re-deriving the HMAC.
#[allow(clippy::too_many_arguments)]
fn shared_key_string_to_sign(
    account: &str,
    method: &str,
    headers: &BTreeMap<String, String>,
    sends_body: bool,
    body_len: u64,
    request_path: &str,
    canonical_query: &str,
) -> String {
    let header = |name: &str| headers.get(name).map(String::as_str).unwrap_or("");
    let content_length = if sends_body && body_len > 0 {
        body_len.to_string()
    } else {
        String::new()
    };
    // Fixed field order from the Blob service specification. `Date` is empty
    // because `x-ms-date` is always sent; `Content-Length` is empty when the
    // body is empty even though the header itself is transmitted as `0`.
    let mut string_to_sign = format!(
        "{method}\n{ce}\n{clang}\n{clen}\n{md5}\n{ct}\n{date}\n{ims}\n{im}\n{inm}\n{ius}\n{range}\n",
        ce = header("content-encoding"),
        clang = header("content-language"),
        clen = content_length,
        md5 = header("content-md5"),
        ct = header("content-type"),
        date = header("date"),
        ims = header("if-modified-since"),
        im = header("if-match"),
        inm = header("if-none-match"),
        ius = header("if-unmodified-since"),
        range = header("range"),
    );
    let mut canonical_headers = String::new();
    for (name, value) in headers {
        if name.starts_with("x-ms-") {
            canonical_headers.push_str(name);
            canonical_headers.push(':');
            canonical_headers.push_str(value.trim());
            canonical_headers.push('\n');
        }
    }
    // The canonicalised resource is the account followed by the request path.
    // A path-style endpoint (Azurite) puts the account in the URL path, so the
    // account legitimately appears twice there — that is exactly the string the
    // service recomputes, and using only `container/blob` would be rejected.
    let mut resource = format!("/{account}{}", percent_decode(request_path));
    // The canonicalised resource names decoded values, so nothing is
    // re-encoded here; only the query spelling is normalised.
    for pair in canonical_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        resource.push('\n');
        resource.push_str(&percent_decode(name).to_ascii_lowercase());
        resource.push(':');
        resource.push_str(&percent_decode(value));
    }
    string_to_sign.push_str(&canonical_headers);
    string_to_sign.push_str(&resource);
    string_to_sign
}

/// RFC 1123 timestamp in GMT, as Azure requires for `x-ms-date`.
fn rfc1123_now() -> (String, i64) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    (rfc1123(seconds as i64), seconds as i64)
}

fn rfc1123(seconds: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday.
    let weekday = (days + 4).rem_euclid(7) as usize;
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[weekday],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
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
    method: &str,
    blob: Option<&str>,
) -> Result<Vec<u8>> {
    if let Some(len) = content_length {
        if len > cap {
            return Err(Error::limit_exceeded(format!(
                "Azure {method} {} response is {len} bytes, above the backend's {cap}-byte bound",
                blob.unwrap_or("container")
            )));
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format!("Azure response read: {err}")))?;
    if bytes.len() as u64 > cap {
        return Err(Error::limit_exceeded(format!(
            "Azure {method} {} response exceeded the backend's {cap}-byte bound",
            blob.unwrap_or("container")
        )));
    }
    Ok(bytes)
}

fn response_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(240).collect()
}

/// RFC 3986 encoding of a blob name with `/` preserved, so a logical key such
/// as `objects/ab/<hash>` keeps its path segments.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
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

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
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
    outer.finalize().to_vec()
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

/// The *contents* of every `<tag>…</tag>` element, so a listing can find each
/// `<Blob>` record before reading its `<Name>`. `<Blobs>` does not match
/// `<Blob>` because the opening marker includes the closing `>`.
fn xml_blocks(xml: &str, tag: &str) -> Vec<String> {
    xml_values(xml, tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1123_dates_are_formatted_for_the_documented_epoch() {
        // 2015-06-26T23:39:12Z, the date used in Azure's signing example.
        assert_eq!(rfc1123(1_435_361_952), "Fri, 26 Jun 2015 23:39:12 GMT");
        assert_eq!(rfc1123(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(rfc1123(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        let query = vec![
            ("restype".to_owned(), "container".to_owned()),
            ("prefix".to_owned(), "object set/".to_owned()),
            ("comp".to_owned(), "list".to_owned()),
        ];
        assert_eq!(
            canonical_query(&query),
            "comp=list&prefix=object%20set%2F&restype=container"
        );
        assert_eq!(percent_decode("object%20set%2F"), "object set/");
    }

    #[test]
    fn shared_key_string_to_sign_has_the_documented_field_order() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "x-ms-date".to_owned(),
            "Fri, 26 Jun 2015 23:39:12 GMT".to_owned(),
        );
        headers.insert("x-ms-version".to_owned(), API_VERSION.to_owned());
        headers.insert("x-ms-blob-type".to_owned(), "BlockBlob".to_owned());
        headers.insert(
            "content-type".to_owned(),
            "application/octet-stream".to_owned(),
        );
        headers.insert("content-length".to_owned(), "5".to_owned());
        let string_to_sign = shared_key_string_to_sign(
            "myaccount",
            "PUT",
            &headers,
            true,
            5,
            "/mycontainer/objects/ab/blob",
            "comp=list&restype=container",
        );
        let expected = "PUT\n\n\n5\n\napplication/octet-stream\n\n\n\n\n\n\n\
x-ms-blob-type:BlockBlob\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\n\
x-ms-version:2021-12-02\n/myaccount/mycontainer/objects/ab/blob\n\
comp:list\nrestype:container";
        assert_eq!(string_to_sign, expected);

        let credential = SharedKey {
            account: "myaccount".to_owned(),
            key: b"unit-test-key".to_vec(),
        };
        assert_eq!(
            shared_key_signature(
                &credential,
                "PUT",
                &headers,
                true,
                5,
                "/mycontainer/objects/ab/blob",
                "comp=list&restype=container",
            ),
            hmac_sha256(b"unit-test-key", expected.as_bytes())
        );
        // Content-Length is empty in the signature when the body is empty,
        // even though the header itself is sent as `0`.
        let mut empty_headers = headers.clone();
        empty_headers.insert("content-length".to_owned(), "0".to_owned());
        let empty = shared_key_string_to_sign(
            "myaccount",
            "PUT",
            &empty_headers,
            true,
            0,
            "/mycontainer/objects/ab/blob",
            "comp=list&restype=container",
        );
        assert!(empty.starts_with("PUT\n\n\n\n\napplication/octet-stream\n"));
    }

    #[test]
    fn xml_listing_blocks_are_extracted_before_their_names() {
        let xml = "<EnumerationResults><Blobs>\
<Blob><Name>objects/ab/aa</Name><Properties><Content-Length>3</Content-Length></Properties></Blob>\
<Blob><Name>objects/cd/bb</Name></Blob></Blobs><NextMarker>m2</NextMarker></EnumerationResults>";
        let blocks = xml_blocks(xml, "Blob");
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            xml_values(&blocks[0], "Name"),
            vec!["objects/ab/aa".to_owned()]
        );
        assert_eq!(
            xml_values(&blocks[1], "Name"),
            vec!["objects/cd/bb".to_owned()]
        );
        assert_eq!(xml_values(xml, "NextMarker"), vec!["m2".to_owned()]);
    }

    #[test]
    fn config_debug_never_reveals_credentials() {
        let config = AzureConfig {
            endpoint: "http://127.0.0.1:10000/devstoreaccount1".to_owned(),
            container: "weft-azure-court".to_owned(),
            account: "devstoreaccount1".to_owned(),
            key: Some("c2VjcmV0LWFjY291bnQta2V5LXZhbHVl".to_owned()),
            sas_token: Some("sv=2021&sig=SECRETSAS".to_owned()),
            anonymous: false,
            lock_lease: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
        };
        let text = format!("{config:?}");
        assert!(!text.contains("c2VjcmV0"), "{text}");
        assert!(!text.contains("SECRETSAS"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
    }

    #[test]
    fn container_names_follow_the_service_rules() {
        assert!(validate_container("weft-azure-court").is_ok());
        assert!(validate_container("ab").is_err());
        assert!(validate_container("Weft-Azure").is_err());
        assert!(validate_container("-leading").is_err());
        assert!(validate_container("trailing-").is_err());
        assert!(validate_container("has_underscore").is_err());
    }

    #[test]
    fn credentials_must_be_exclusive() {
        // Only the account-key and SAS forms are credential-bearing settings;
        // `validate` accepts either alone and `resolve` is what refuses both.
        let base = AzureConfig {
            endpoint: "http://127.0.0.1:10000/devstoreaccount1".to_owned(),
            container: "weft-azure-court".to_owned(),
            account: "devstoreaccount1".to_owned(),
            key: None,
            sas_token: None,
            anonymous: false,
            lock_lease: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
        };
        assert_eq!(credential_forms(&base), 0);
        let key = AzureConfig {
            key: Some("Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==".to_owned()),
            ..base.clone()
        };
        assert_eq!(credential_forms(&key), 1);
        if let Err(err) = key.validate() {
            panic!("account-key config must validate: {err}");
        }
        let sas = AzureConfig {
            sas_token: Some("sv=2021-12-02&sig=x".to_owned()),
            ..base.clone()
        };
        assert_eq!(credential_forms(&sas), 1);
        assert!(sas.validate().is_ok());
        let both = AzureConfig {
            key: Some("AAAA".to_owned()),
            sas_token: Some("sv=1".to_owned()),
            ..base
        };
        assert_eq!(credential_forms(&both), 2);
    }

    #[test]
    fn strip_etag_quotes_is_exact() {
        assert_eq!(strip_etag_quotes("\"0x8DABC\""), "0x8DABC");
        assert_eq!(strip_etag_quotes("0x8DABC"), "0x8DABC");
        assert_eq!(strip_etag_quotes("\""), "\"");
    }
}
