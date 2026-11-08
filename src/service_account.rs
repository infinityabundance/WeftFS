//! Google service-account authentication: RS256 JWT-bearer flow (subphase R21).
//!
//! GCS accepts two credential forms for a JSON API call:
//!
//! * an OAuth2 **access token**, which expires and must be refreshed;
//! * a **service-account JSON key**, which this module turns into a signed
//!   assertion (a JWT) and exchanges at the key's `token_uri` for an access
//!   token.
//!
//! The second form has to sign with RSASSA-PKCS1-v1_5 SHA-256. This crate does
//! not take an RSA dependency: the operation is implemented here on top of the
//! primitive that *is* already in the dependency set, SHA-256 from `sha2`:
//!
//! 1. a strict DER reader for the PKCS#8 (`BEGIN PRIVATE KEY`) and PKCS#1
//!    (`BEGIN RSA PRIVATE KEY`) encodings a Google key file uses;
//! 2. fixed-width big-integer arithmetic (schoolbook multiplication,
//!    Montgomery reduction, square-and-multiply) with no `unsafe`;
//! 3. EMSA-PKCS1-v1_5 encoding with the published SHA-256 `DigestInfo` prefix;
//! 4. signing through the CRT parameters, checked against both a direct
//!    `m^d mod n` and a public-exponent verification at load time.
//!
//! # Correctness evidence
//!
//! `tests/service_account.rs` checks the implementation against the published
//! RS256 test vector in RFC 7515 Appendix A.2 — the exact 2048-bit key, the
//! exact signing input, and the exact signature octets. That vector is offline
//! and does not depend on Google.
//!
//! # What is refused, explicitly
//!
//! * a key file whose `type` is not `service_account` (for example a
//!   `authorized_user` ADC file, which needs a refresh-token flow this module
//!   does not implement);
//! * an EC or Ed25519 key, or an encrypted private key (PBES2/`ENCRYPTED
//!   PRIVATE KEY`): RS256 is what Google's JWT-bearer flow accepts, and
//!   guessing at another algorithm is never done;
//! * an RSA key whose components do not agree (p·q ≠ n, or CRT output
//!   differing from `m^d mod n`);
//! * a modulus outside 2048..=8192 bits.
//!
//! Each refusal is a typed [`Error`] whose message names what would be
//! required. No fallback, no "best effort".
//!
//! # Secrets
//!
//! The key material, the assertion, and the access token are never rendered by
//! `Debug`, never included in an error message, and never written to
//! diagnostics. [`ServiceAccountKey`] and [`TokenInfo`] redact them; the key is
//! identified by a SHA-256 fingerprint of its modulus so two handles can be
//! compared without exposing anything.
//!
//! # Scope of the evidence
//!
//! The implementation is exercised offline against the RFC vector, against an
//! in-process OAuth2 token endpoint, and against malformed inputs. It is **not**
//! proven against a live Google endpoint: no service-account credential exists
//! in this environment, so `bench/scripts/gcs_court.sh` records `no_credentials`
//! rather than claiming a live run.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The scope a WeftFS store needs on GCS. Read-write covers object and bucket
/// metadata operations; a read-only deployment can override it with
/// `WEFT_GCS_SCOPE`.
pub const DEFAULT_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";

/// Default Google token endpoint, used only when a key file omits `token_uri`.
pub const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Assertion lifetime. Google's JWT-bearer flow accepts at most one hour.
const ASSERTION_LIFETIME_SECS: i64 = 3600;

/// A cached access token is refreshed when less than this remains.
const MAX_REFRESH_WINDOW_NS: i64 = 300 * 1_000_000_000;

/// Larger key files are refused before parsing. A service-account JSON key with
/// a 4096-bit PEM is a few kilobytes.
const MAX_KEY_FILE_BYTES: u64 = 1 << 20;

/// Larger token-endpoint replies are refused.
const MAX_TOKEN_REPLY_BYTES: u64 = 1 << 20;

/// SHA-256 `DigestInfo` prefix from RFC 8017 §9.2 note 1.
const SHA256_DIGEST_INFO_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

// ---------------------------------------------------------------------------
// Public key / credential types
// ---------------------------------------------------------------------------

/// A parsed Google service-account key.
///
/// The private components live in [`RsaPrivateKey`], which has no `Debug`.
#[derive(Clone)]
pub struct ServiceAccountKey {
    client_email: String,
    private_key_id: Option<String>,
    token_uri: String,
    project_id: Option<String>,
    rsa: RsaPrivateKey,
    /// SHA-256 of the modulus, used for equality and identification.
    fingerprint: [u8; 32],
}

impl ServiceAccountKey {
    /// Parse a service-account JSON key file.
    ///
    /// `bytes` must be at most [`MAX_KEY_FILE_BYTES`].
    pub fn from_json_bytes(bytes: &[u8]) -> Result<ServiceAccountKey> {
        if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "service-account key file is {} bytes, above the {} byte bound",
                bytes.len(),
                MAX_KEY_FILE_BYTES
            )));
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|err| {
            // serde's message names the position, never the contents.
            Error::invalid_argument(format!("service-account key is not valid JSON: {err}"))
        })?;
        let object = value.as_object().ok_or_else(|| {
            Error::invalid_argument("service-account key is JSON but not a JSON object")
        })?;
        let text = |name: &str| object.get(name).and_then(Value::as_str);
        let kind = text("type").unwrap_or_default();
        if kind != "service_account" {
            return Err(Error::unsupported_version(format!(
                "credential type `{}` is not supported; the JWT-bearer flow needs a \
                 `service_account` key. An `authorized_user` application-default credential uses \
                 a refresh-token flow this crate does not implement, and a bare access token is \
                 supplied through --gcs-token instead",
                if kind.is_empty() { "<missing>" } else { kind }
            )));
        }
        let client_email = text("client_email")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::invalid_argument("service-account key has no `client_email`"))?
            .to_owned();
        let private_key = text("private_key")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::invalid_argument("service-account key has no `private_key`"))?;
        let token_uri = text("token_uri")
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_TOKEN_URI)
            .to_owned();
        let rsa = RsaPrivateKey::from_pem(private_key)?;
        let fingerprint: [u8; 32] = Sha256::digest(rsa.modulus_be()).into();
        Ok(ServiceAccountKey {
            client_email,
            private_key_id: text("private_key_id")
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            token_uri,
            project_id: text("project_id")
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            rsa,
            fingerprint,
        })
    }

    /// Read and parse a key file.
    pub fn from_path(path: &std::path::Path) -> Result<ServiceAccountKey> {
        let bytes = std::fs::read(path).map_err(|err| {
            Error::io(format!(
                "cannot read the service-account key file {}: {err}",
                path.display()
            ))
        })?;
        ServiceAccountKey::from_json_bytes(&bytes)
    }

    pub fn client_email(&self) -> &str {
        &self.client_email
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    pub fn token_uri(&self) -> &str {
        &self.token_uri
    }

    /// Hex SHA-256 of the modulus. Safe to log.
    pub fn fingerprint_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.fingerprint {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Modulus width in bits.
    pub fn modulus_bits(&self) -> usize {
        self.rsa.modulus_bits()
    }

    /// Build a signed JWT-bearer assertion for `scope` at `now_ns`.
    pub fn assertion(&self, scope: &str, now_ns: i64) -> Result<String> {
        if scope.is_empty() {
            return Err(Error::invalid_argument(
                "a service-account assertion needs a non-empty scope",
            ));
        }
        let issued_at = now_ns.div_euclid(1_000_000_000);
        let expires_at = issued_at.saturating_add(ASSERTION_LIFETIME_SECS);
        let header = match &self.private_key_id {
            Some(kid) => serde_json::json!({
                "alg": "RS256",
                "typ": "JWT",
                "kid": own(kid),
            }),
            None => serde_json::json!({"alg": "RS256", "typ": "JWT"}),
        };
        let claims = serde_json::json!({
            "iss": own(&self.client_email),
            "scope": own(scope),
            "aud": own(&self.token_uri),
            "exp": expires_at,
            "iat": issued_at,
        });
        let header = serde_json::to_vec(&header)
            .map_err(|err| Error::invalid_argument(format!("cannot encode JWT header: {err}")))?;
        let claims = serde_json::to_vec(&claims)
            .map_err(|err| Error::invalid_argument(format!("cannot encode JWT claims: {err}")))?;
        self.sign_compact(&header, &claims)
    }

    /// Sign an arbitrary JWS compact serialization. Used by the assertion
    /// builder and by the RFC 7515 vector test.
    pub fn sign_compact(&self, header: &[u8], payload: &[u8]) -> Result<String> {
        let signing_input = ServiceAccountKey::signing_input(header, payload);
        let signature = self.rsa.sign_sha256(signing_input.as_bytes())?;
        Ok(format!(
            "{signing_input}.{}",
            crate::b64::encode_url_nopad(&signature)
        ))
    }

    /// The exact JWS signing input for `header`/`payload`, without signing.
    pub fn signing_input(header: &[u8], payload: &[u8]) -> String {
        format!(
            "{}.{}",
            crate::b64::encode_url_nopad(header),
            crate::b64::encode_url_nopad(payload)
        )
    }
}

/// `serde_json::json!` needs an owned value; this keeps a borrowed `&str`
/// usable without changing the field order the macro produces.
fn own(value: &str) -> String {
    value.to_owned()
}

impl PartialEq for ServiceAccountKey {
    fn eq(&self, other: &Self) -> bool {
        // Identifier material only: never compare or expose private components.
        self.fingerprint == other.fingerprint
            && self.client_email == other.client_email
            && self.token_uri == other.token_uri
    }
}

impl Eq for ServiceAccountKey {}

impl fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountKey")
            .field("client_email", &self.client_email)
            .field("private_key_id", &self.private_key_id)
            .field("token_uri", &self.token_uri)
            .field("project_id", &self.project_id)
            .field("modulus_bits", &self.rsa.modulus_bits())
            .field("fingerprint", &self.fingerprint_hex())
            .field("private_key", &"<redacted>")
            .finish()
    }
}

/// One access token and the instants that bound its usable lifetime.
#[derive(Clone)]
pub struct TokenInfo {
    access_token: String,
    /// Unix nanoseconds at which the provider says the token expires.
    pub expires_at_ns: i64,
    /// Unix nanoseconds at which the exchange started.
    pub issued_at_ns: i64,
}

impl TokenInfo {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }
}

impl fmt::Debug for TokenInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenInfo")
            .field("access_token", &"<redacted>")
            .field("expires_at_ns", &self.expires_at_ns)
            .field("issued_at_ns", &self.issued_at_ns)
            .finish()
    }
}

/// Token-exchange accounting for diagnostics and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthStats {
    /// Assertions signed and exchanged at the token endpoint.
    pub exchanges: u64,
    /// Calls served from the cache without a network request.
    pub cache_hits: u64,
    /// Exchanges refused by the provider.
    pub refusals: u64,
}

/// The OAuth2 JWT-bearer client for one service account.
///
/// The cache is a single slot behind a mutex. The mutex is held across the
/// exchange call on purpose: two threads that both need a fresh token must not
/// perform two exchanges, and a token refresh is rare (once per hour).
pub struct ServiceAccountAuth {
    key: std::sync::Arc<ServiceAccountKey>,
    scope: String,
    agent: ureq::Agent,
    cached: Mutex<Option<TokenInfo>>,
    stats: Mutex<AuthStats>,
}

impl ServiceAccountAuth {
    pub fn new(
        key: std::sync::Arc<ServiceAccountKey>,
        scope: impl Into<String>,
    ) -> ServiceAccountAuth {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .timeout_connect(Duration::from_secs(10))
            .build();
        ServiceAccountAuth {
            key,
            scope: scope.into(),
            agent,
            cached: Mutex::new(None),
            stats: Mutex::new(AuthStats::default()),
        }
    }

    pub fn key(&self) -> &ServiceAccountKey {
        &self.key
    }

    pub fn stats(&self) -> AuthStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A valid access token, refreshing when the remaining lifetime falls
    /// inside the bounded refresh window.
    pub fn token(&self) -> Result<TokenInfo> {
        self.token_at(now_ns())
    }

    /// The same call with an injected clock, for the cache tests.
    pub fn token_at(&self, now_ns: i64) -> Result<TokenInfo> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(current) = cached.as_ref() {
            let window = refresh_window_ns(current);
            if now_ns.saturating_add(window) < current.expires_at_ns {
                let mut stats = self
                    .stats
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                stats.cache_hits += 1;
                return Ok(current.clone());
            }
        }

        let assertion = self.key.assertion(&self.scope, now_ns)?;
        let grant = "urn:ietf:params:oauth:grant-type:jwt-bearer";
        let outcome = self
            .agent
            .post(&self.key.token_uri)
            .send_form(&[("grant_type", grant), ("assertion", assertion.as_str())]);
        let response = match outcome {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let mut stats = self
                    .stats
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                stats.refusals += 1;
                drop(stats);
                let code = read_error_code(response);
                return Err(match status {
                    400 | 401 | 403 => Error::unauthorized(format!(
                        "the token endpoint refused the service-account assertion (HTTP {status}{})",
                        code.map(|code| format!(", error {code}")).unwrap_or_default()
                    )),
                    other => Error::io(format!(
                        "the token endpoint returned HTTP {other}{}",
                        code.map(|code| format!(", error {code}")).unwrap_or_default()
                    )),
                });
            }
            Err(err) => {
                return Err(Error::io(format!(
                    "cannot reach the token endpoint {}: {err}",
                    self.key.token_uri
                )))
            }
        };
        if response.status() != 200 {
            let mut stats = self
                .stats
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            stats.refusals += 1;
            drop(stats);
            return Err(Error::io(format!(
                "the token endpoint returned HTTP {}",
                response.status()
            )));
        }
        let body = read_bounded(response, MAX_TOKEN_REPLY_BYTES)?;
        let value: Value = serde_json::from_slice(&body).map_err(|_| {
            Error::corrupt("the token endpoint returned a reply that is not valid JSON")
        })?;
        let access_token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| Error::corrupt("the token endpoint reply has no `access_token` string"))?
            .to_owned();
        if let Some(token_type) = value.get("token_type").and_then(Value::as_str) {
            if !token_type.eq_ignore_ascii_case("bearer") {
                return Err(Error::unsupported_version(format!(
                    "the token endpoint issued `{token_type}` instead of a Bearer token"
                )));
            }
        }
        let lifetime_secs = value
            .get("expires_in")
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
            })
            .filter(|seconds| *seconds > 0)
            .ok_or_else(|| {
                Error::corrupt(
                    "the token endpoint reply has no positive `expires_in`; refusing a token whose \
                     lifetime is unknown",
                )
            })?;
        let lifetime_ns = lifetime_secs
            .saturating_mul(1_000_000_000)
            .min(i64::MAX / 2);
        let token = TokenInfo {
            access_token,
            expires_at_ns: now_ns.saturating_add(lifetime_ns),
            issued_at_ns: now_ns,
        };
        *cached = Some(token.clone());
        let mut stats = self
            .stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.exchanges += 1;
        drop(stats);
        Ok(token)
    }
}

impl fmt::Debug for ServiceAccountAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountAuth")
            .field("key", &self.key)
            .field("scope", &self.scope)
            .field("stats", &self.stats())
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The bounded refresh window: 300 s, or half the token's lifetime when that
/// is smaller. A short-lived token is therefore refreshed before it expires
/// instead of being hammered on every call, and a long-lived one is refreshed
/// with 300 s to spare.
fn refresh_window_ns(token: &TokenInfo) -> i64 {
    let lifetime = (token.expires_at_ns - token.issued_at_ns).max(0);
    (lifetime / 2).clamp(1, MAX_REFRESH_WINDOW_NS)
}

fn now_ns() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    i64::try_from(nanos).unwrap_or(i64::MAX)
}

fn read_bounded(response: ureq::Response, cap: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let content_length = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(length) = content_length {
        if length > cap {
            return Err(Error::limit_exceeded(format!(
                "token endpoint reply is {length} bytes, above the {cap} byte bound"
            )));
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format!("cannot read the token endpoint reply: {err}")))?;
    if bytes.len() as u64 > cap {
        return Err(Error::limit_exceeded(format!(
            "token endpoint reply exceeded the {cap} byte bound"
        )));
    }
    Ok(bytes)
}

/// The provider's `error` code, if the reply carries one. Only the short code
/// is returned; the `error_description` is not echoed because it is provider
/// prose that may quote account details.
fn read_error_code(response: ureq::Response) -> Option<String> {
    let bytes = read_bounded(response, MAX_TOKEN_REPLY_BYTES).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("error")
        .and_then(Value::as_str)
        .map(|code| code.chars().take(64).collect())
}

// ---------------------------------------------------------------------------
// RSA (RSASSA-PKCS1-v1_5 with SHA-256)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct RsaPrivateKey {
    modulus: BigUint,
    public_exponent: BigUint,
    private_exponent: BigUint,
    p: BigUint,
    q: BigUint,
    dp: BigUint,
    dq: BigUint,
    qinv: BigUint,
    modulus_bytes: usize,
    /// Montgomery contexts, computed once at load time (each requires ~2*(64k)
    /// doublings, which is far more than one exponentiation needs).
    n_mod: Modulus,
    p_mod: Modulus,
    q_mod: Modulus,
}

impl RsaPrivateKey {
    fn from_pem(pem: &str) -> Result<RsaPrivateKey> {
        let (label, body) = pem_block(pem)?;
        let der = crate::b64::decode_any(&body).map_err(|err| {
            Error::invalid_argument(format!(
                "the private key's base64 body is malformed ({err}); key material is not printed"
            ))
        })?;
        match label.as_str() {
            "PRIVATE KEY" => RsaPrivateKey::from_pkcs8(&der),
            "RSA PRIVATE KEY" => RsaPrivateKey::from_pkcs1(&der),
            "ENCRYPTED PRIVATE KEY" => Err(Error::unsupported_version(
                "the private key is an encrypted PKCS#8 (`ENCRYPTED PRIVATE KEY`). Decrypting it \
                 needs PBES2/PBKDF2, which this crate does not implement; supply an unencrypted \
                 service-account key",
            )),
            other => Err(Error::unsupported_version(format!(
                "PEM block `{other}` is not an RSA private key; only `PRIVATE KEY` (PKCS#8) and \
                 `RSA PRIVATE KEY` (PKCS#1) are accepted"
            ))),
        }
    }

    fn from_pkcs8(der: &[u8]) -> Result<RsaPrivateKey> {
        let mut outer = Der::new(der)?;
        let body = outer.sequence()?;
        outer.finish()?;
        let mut inner = Der::new(body)?;
        let version = inner.integer()?;
        if version != BigUint::from_u64(0) {
            return Err(Error::invalid_argument(
                "PKCS#8 private key has a non-zero version",
            ));
        }
        let mut algorithm = Der::new(inner.sequence()?)?;
        let oid = algorithm.oid()?;
        if oid != OID_RSA_ENCRYPTION {
            return Err(Error::unsupported_version(
                "the private key is not an RSA key; RS256 needs `1.2.840.113549.1.1.1`, and this \
                 crate refuses to sign with any other algorithm",
            ));
        }
        // The parameters are NULL for rsaEncryption. Accept an absent or NULL
        // value, reject anything else.
        if !algorithm.is_empty() {
            algorithm.null()?;
        }
        algorithm.finish()?;
        let key_der = inner.octet_string()?;
        inner.finish()?;
        RsaPrivateKey::from_pkcs1(key_der)
    }

    fn from_pkcs1(der: &[u8]) -> Result<RsaPrivateKey> {
        let mut outer = Der::new(der)?;
        let body = outer.sequence()?;
        outer.finish()?;
        let mut reader = Der::new(body)?;
        let version = reader.integer()?;
        if version != BigUint::from_u64(0) {
            return Err(Error::invalid_argument(
                "PKCS#1 RSAPrivateKey has a non-zero version",
            ));
        }
        let modulus = reader.integer()?;
        let public_exponent = reader.integer()?;
        let private_exponent = reader.integer()?;
        let p = reader.integer()?;
        let q = reader.integer()?;
        let dp = reader.integer()?;
        let dq = reader.integer()?;
        let qinv = reader.integer()?;
        reader.finish()?;

        let bits = modulus.bit_len();
        if !(2048..=8192).contains(&bits) {
            return Err(Error::unsupported_version(format!(
                "the RSA modulus is {bits} bits; a service-account key must be 2048..=8192 bits"
            )));
        }
        if !public_exponent.is_odd() || public_exponent < BigUint::from_u64(3) {
            return Err(Error::invalid_argument(
                "the RSA public exponent is not an odd integer >= 3",
            ));
        }
        if p.is_zero() || q.is_zero() || dp.is_zero() || dq.is_zero() || qinv.is_zero() {
            return Err(Error::invalid_argument(
                "the RSA key has a zero CRT component",
            ));
        }
        // p*q == n catches a truncated or reordered key file.
        if p.mul(&q) != modulus {
            return Err(Error::invalid_argument(
                "the RSA key's p*q does not equal its modulus; the key file is inconsistent",
            ));
        }
        let key = RsaPrivateKey {
            modulus_bytes: (bits + 7) / 8,
            n_mod: Modulus::new(modulus.clone()),
            p_mod: Modulus::new(p.clone()),
            q_mod: Modulus::new(q.clone()),
            modulus,
            public_exponent,
            private_exponent,
            p,
            q,
            dp,
            dq,
            qinv,
        };
        key.self_check()?;
        Ok(key)
    }

    /// Prove the CRT parameters and the private exponent agree before the key
    /// is ever used: sign a fixed value three ways and compare.
    fn self_check(&self) -> Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(b"weftfs/service-account/self-check/v1");
        let digest: [u8; 32] = hasher.finalize().into();
        let message = BigUint::from_be_bytes(&digest);
        let message = message.rem(&self.modulus);
        let direct = self.mod_pow(&message, &self.private_exponent);
        let crt = self.crt(&message)?;
        if direct != crt {
            return Err(Error::invalid_argument(
                "the RSA key's CRT parameters do not agree with its private exponent",
            ));
        }
        // The public operation must invert the private one.
        let recovered = self.public_pow(&crt);
        if recovered != message {
            return Err(Error::invalid_argument(
                "the RSA key's public and private exponents are not inverses",
            ));
        }
        Ok(())
    }

    fn modulus_bits(&self) -> usize {
        self.modulus.bit_len()
    }

    fn modulus_be(&self) -> Vec<u8> {
        self.modulus.to_be_bytes()
    }

    /// Sign `message` with RSASSA-PKCS1-v1_5 and SHA-256.
    fn sign_sha256(&self, message: &[u8]) -> Result<Vec<u8>> {
        let digest: [u8; 32] = Sha256::digest(message).into();
        let mut digest_info = Vec::with_capacity(SHA256_DIGEST_INFO_PREFIX.len() + 32);
        digest_info.extend_from_slice(&SHA256_DIGEST_INFO_PREFIX);
        digest_info.extend_from_slice(&digest);
        let k = self.modulus_bytes;
        // EMSA-PKCS1-v1_5: 0x00 || 0x01 || PS || 0x00 || DigestInfo.
        let padding = k
            .checked_sub(digest_info.len() + 3)
            .expect("modulus width was checked against DigestInfo");
        let mut encoded = Vec::with_capacity(k);
        encoded.push(0x00);
        encoded.push(0x01);
        encoded.resize(2 + padding, 0xff);
        encoded.push(0x00);
        encoded.extend_from_slice(&digest_info);
        debug_assert_eq!(encoded.len(), k);
        let representative = BigUint::from_be_bytes(&encoded);
        let signature = self.crt(&representative)?;
        // Mandatory fault check: RSA is multiplicative, so a single fault in a
        // CRT half-exponentiation can turn a valid signature into a factor of
        // `n`. Recompute with the *public* exponent (cheap, public data) and
        // refuse anything that does not reproduce the exact EMSA encoding.
        // This also covers a CRT recombination error.
        if self.public_pow(&signature) != representative {
            return Err(Error::corrupt(
                "RSA signature failed its public-exponent fault check; refusing to return it",
            ));
        }
        let bytes = signature.to_be_bytes();
        if bytes.len() > k {
            return Err(Error::invalid_argument(
                "the RSA signature is wider than its modulus",
            ));
        }
        let mut out = vec![0u8; k - bytes.len()];
        out.extend_from_slice(&bytes);
        Ok(out)
    }

    /// CRT exponentiation with a Montgomery ladder in both halves.
    ///
    /// Every operation on key material is fixed-width and branch-free:
    ///
    /// * both half-exponentiations run exactly `modulus.bit_len()` ladder steps
    ///   (the modulus width is public), each with two fixed-width Montgomery
    ///   multiplications;
    /// * the exponent bit is read at a fixed index and selects operands with an
    ///   arithmetic mask (`cswap`), never a branch and never an index computed
    ///   from a secret;
    /// * `m1 - m2 mod p` uses a branchless borrow mask instead of the previous
    ///   `if m1 >= m2`;
    /// * recombination uses fixed-width multiply/add low-limb loops.
    ///
    /// Deliverable scope: the *arithmetic* is constant-time in the sequence of
    /// executed instructions and memory addresses. `BigUint::rem` still runs on
    /// public message bytes, and the final `to_be_bytes` trims the (published)
    /// signature. A formal audit of the compiler's code generation, and of
    /// cache/branch-predictor effects across microarchitectures, is still
    /// required; see `docs/STATUS.md`.
    fn crt(&self, message: &BigUint) -> Result<BigUint> {
        let m1 = self.p_mod.ladder_pow(&message.rem(&self.p), &self.dp);
        let m2 = self.q_mod.ladder_pow(&message.rem(&self.q), &self.dq);

        // h = qinv * ((m1 - m2) mod p) mod p, with a branchless borrow mask.
        let kp = self.p_mod.limbs;
        let a = fixed_width(&m1, kp);
        let b = fixed_width(&m2, kp);
        let (difference, borrow) = sub_fixed(&a, &b);
        let mask = 0u64.wrapping_sub(borrow);
        let p_limbs = fixed_width(&self.p, kp);
        let addend: Vec<u64> = p_limbs.iter().map(|limb| limb & mask).collect();
        let difference = add_fixed(&difference, &addend);

        let diff_mont = self.p_mod.to_mont_ct(&difference);
        let qinv_mont = self.p_mod.to_mont_ct(&fixed_width(&self.qinv, kp));
        let h = self
            .p_mod
            .from_mont_ct(&self.p_mod.mont_mul_ct(&diff_mont, &qinv_mont));

        // m2 + h*q, fixed-width low-limb arithmetic (`h*q + m2 < n`).
        let k = self.n_mod.limbs;
        let product = mul_low(&padded(&h, k), &fixed_width(&self.q, k));
        let result = add_fixed(&fixed_width(&m2, k), &product);
        Ok(BigUint::from_fixed(&result))
    }

    fn mod_pow(&self, base: &BigUint, exponent: &BigUint) -> BigUint {
        self.n_mod.pow(base, exponent)
    }

    /// `base^e mod n` for the **public** exponent, with a value-independent
    /// instruction stream: a fixed number of squarings and a mask-selected
    /// multiply per bit of `e` (whose width is public). Used by the mandatory
    /// fault check after signing, so the whole signing operation stays
    /// mask-only in its data-dependent paths (the signature is public, but a
    /// value-dependent verification would pollute the timing profile).
    fn public_pow(&self, base: &BigUint) -> BigUint {
        let k = self.n_mod.limbs;
        let base_mont = self.n_mod.to_mont_ct(&fixed_width(&base.rem(&self.n_mod.n), k));
        let exponent = fixed_width(&self.public_exponent, k);
        let mut result = fixed_width(&self.n_mod.r_mod, k);
        let bits = self.public_exponent.bit_len();
        for index in (0..bits).rev() {
            result = self.n_mod.mont_mul_ct(&result, &result);
            let bit = (exponent[index / 64] >> (index % 64)) & 1;
            let product = self.n_mod.mont_mul_ct(&result, &base_mont);
            result = select_masked(bit, &product, &result);
        }
        BigUint::from_fixed(&self.n_mod.from_mont_ct(&result))
    }
}

impl fmt::Debug for RsaPrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RsaPrivateKey")
            .field("modulus_bits", &self.modulus_bits())
            .field("private_components", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// PEM
// ---------------------------------------------------------------------------

/// Extract `(label, base64 body)` from a single PEM block, accepting only
/// line wrapping and no other content.
fn pem_block(pem: &str) -> Result<(String, String)> {
    if pem.contains("Proc-Type:") {
        return Err(Error::unsupported_version(
            "the private key is a legacy encrypted PEM (`Proc-Type: 4,ENCRYPTED`); supply an \
             unencrypted service-account key",
        ));
    }
    let begin = pem
        .find("-----BEGIN ")
        .ok_or_else(|| Error::invalid_argument("the private key has no PEM BEGIN marker"))?;
    let rest = &pem[begin + 11..];
    let label_end = rest
        .find("-----")
        .ok_or_else(|| Error::invalid_argument("the PEM BEGIN marker is unterminated"))?;
    let label = rest[..label_end].to_owned();
    let body_start = begin + 11 + label_end + 5;
    let body_rest = &pem[body_start..];
    let footer = format!("-----END {label}-----");
    let body_end = body_rest.find(&footer).ok_or_else(|| {
        Error::invalid_argument(format!(
            "the PEM block `{label}` has no matching END marker"
        ))
    })?;
    let mut body = String::with_capacity(body_end);
    for ch in body_rest[..body_end].chars() {
        match ch {
            '\r' | '\n' | ' ' | '\t' => {}
            other => body.push(other),
        }
    }
    if body.is_empty() {
        return Err(Error::invalid_argument(format!(
            "the PEM block `{label}` has an empty body"
        )));
    }
    Ok((label, body))
}

// ---------------------------------------------------------------------------
// A strict DER reader
// ---------------------------------------------------------------------------

struct Der<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Result<Der<'a>> {
        Ok(Der { bytes, index: 0 })
    }

    fn is_empty(&self) -> bool {
        self.index == self.bytes.len()
    }

    fn next_tag(&mut self) -> Result<u8> {
        let tag = *self
            .bytes
            .get(self.index)
            .ok_or_else(|| Error::invalid_argument("DER value is truncated"))?;
        self.index += 1;
        Ok(tag)
    }

    fn finish(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::invalid_argument(
                "DER value has trailing bytes; refusing to guess the structure",
            ))
        }
    }

    fn null(&mut self) -> Result<()> {
        let body = self.read(0x05)?;
        if body.is_empty() {
            Ok(())
        } else {
            Err(Error::invalid_argument("DER NULL value is not empty"))
        }
    }

    fn read(&mut self, expected_tag: u8) -> Result<&'a [u8]> {
        let tag = self.next_tag()?;
        if tag != expected_tag {
            return Err(Error::invalid_argument(format!(
                "expected DER tag 0x{expected_tag:02x}, found 0x{tag:02x}"
            )));
        }
        let length = self.length()?;
        let start = self.index;
        let end = start
            .checked_add(length)
            .ok_or_else(|| Error::invalid_argument("DER length overflows the address space"))?;
        if end > self.bytes.len() {
            return Err(Error::invalid_argument(
                "DER value runs past the end of its container",
            ));
        }
        self.index = end;
        Ok(&self.bytes[start..end])
    }

    fn length(&mut self) -> Result<usize> {
        let first = *self
            .bytes
            .get(self.index)
            .ok_or_else(|| Error::invalid_argument("DER length is truncated"))?;
        self.index += 1;
        if first < 0x80 {
            return Ok(first as usize);
        }
        let count = (first & 0x7f) as usize;
        if count == 0 {
            return Err(Error::invalid_argument(
                "DER indefinite length is not allowed",
            ));
        }
        if count > 4 {
            return Err(Error::invalid_argument(
                "DER length uses more than four octets",
            ));
        }
        let mut length: usize = 0;
        for _ in 0..count {
            let byte = *self
                .bytes
                .get(self.index)
                .ok_or_else(|| Error::invalid_argument("DER length is truncated"))?;
            self.index += 1;
            length = (length << 8) | byte as usize;
        }
        // Long form must not encode a length that fits in the short form.
        if length < 0x80 {
            return Err(Error::invalid_argument(
                "DER length is not minimally encoded",
            ));
        }
        Ok(length)
    }

    fn sequence(&mut self) -> Result<&'a [u8]> {
        self.read(0x30)
    }

    fn octet_string(&mut self) -> Result<&'a [u8]> {
        self.read(0x04)
    }

    fn oid(&mut self) -> Result<&'a [u8]> {
        self.read(0x06)
    }

    fn integer(&mut self) -> Result<BigUint> {
        let bytes = self.read(0x02)?;
        if bytes.is_empty() {
            return Err(Error::invalid_argument("DER INTEGER is empty"));
        }
        if bytes[0] & 0x80 != 0 {
            return Err(Error::invalid_argument(
                "DER INTEGER is negative; RSA components are positive",
            ));
        }
        // Minimal encoding: a leading zero is only allowed when the next byte
        // would otherwise set the sign bit.
        if bytes.len() > 1 && bytes[0] == 0 && bytes[1] & 0x80 == 0 {
            return Err(Error::invalid_argument(
                "DER INTEGER is not minimally encoded",
            ));
        }
        Ok(BigUint::from_be_bytes(bytes))
    }
}

// ---------------------------------------------------------------------------
// Fixed-width unsigned integers and Montgomery modular arithmetic
// ---------------------------------------------------------------------------

/// Unsigned big integer, little-endian 64-bit limbs, no trailing zero limbs.
#[derive(Clone, PartialEq, Eq)]
struct BigUint {
    limbs: Vec<u64>,
}

impl BigUint {
    fn zero() -> BigUint {
        BigUint { limbs: Vec::new() }
    }

    fn from_u64(value: u64) -> BigUint {
        if value == 0 {
            BigUint::zero()
        } else {
            BigUint { limbs: vec![value] }
        }
    }

    /// Build a value from fixed-width little-endian limbs.
    fn from_fixed(limbs: &[u64]) -> BigUint {
        let mut value = BigUint {
            limbs: limbs.to_vec(),
        };
        value.trim();
        value
    }

    fn from_be_bytes(bytes: &[u8]) -> BigUint {
        let mut limbs = Vec::with_capacity((bytes.len() + 7) / 8);
        let mut end = bytes.len();
        while end > 0 {
            let start = end.saturating_sub(8);
            let mut limb: u64 = 0;
            for &byte in &bytes[start..end] {
                limb = (limb << 8) | u64::from(byte);
            }
            limbs.push(limb);
            end = start;
        }
        let mut value = BigUint { limbs };
        value.trim();
        value
    }

    fn to_be_bytes(&self) -> Vec<u8> {
        if self.limbs.is_empty() {
            return vec![0];
        }
        let mut out = Vec::with_capacity(self.limbs.len() * 8);
        for (index, limb) in self.limbs.iter().enumerate().rev() {
            let bytes = limb.to_be_bytes();
            if index == self.limbs.len() - 1 {
                let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
                out.extend_from_slice(&bytes[first..]);
            } else {
                out.extend_from_slice(&bytes);
            }
        }
        out
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    fn is_odd(&self) -> bool {
        self.limbs
            .first()
            .map(|limb| limb & 1 == 1)
            .unwrap_or(false)
    }

    fn bit_len(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(top) => (self.limbs.len() - 1) * 64 + (64 - top.leading_zeros() as usize),
        }
    }

    fn bit(&self, index: usize) -> bool {
        let limb = index / 64;
        let offset = index % 64;
        self.limbs
            .get(limb)
            .map(|value| (value >> offset) & 1 == 1)
            .unwrap_or(false)
    }

    fn add(&self, other: &BigUint) -> BigUint {
        let len = self.limbs.len().max(other.limbs.len());
        let mut out = Vec::with_capacity(len + 1);
        let mut carry: u64 = 0;
        for index in 0..len {
            let a = self.limbs.get(index).copied().unwrap_or(0);
            let b = other.limbs.get(index).copied().unwrap_or(0);
            let (sum, overflow) = a.overflowing_add(b);
            let (sum, overflow2) = sum.overflowing_add(carry);
            out.push(sum);
            carry = u64::from(overflow || overflow2);
        }
        if carry != 0 {
            out.push(carry);
        }
        let mut value = BigUint { limbs: out };
        value.trim();
        value
    }

    /// `self - other`; the caller proves `self >= other`.
    fn sub(&self, other: &BigUint) -> BigUint {
        let mut out = Vec::with_capacity(self.limbs.len());
        let mut borrow: u64 = 0;
        for index in 0..self.limbs.len() {
            let a = self.limbs[index];
            let b = other.limbs.get(index).copied().unwrap_or(0);
            let (diff, underflow1) = a.overflowing_sub(b);
            let (diff, underflow2) = diff.overflowing_sub(borrow);
            out.push(diff);
            borrow = u64::from(underflow1 || underflow2);
        }
        debug_assert_eq!(borrow, 0, "BigUint::sub called with self < other");
        let mut value = BigUint { limbs: out };
        value.trim();
        value
    }

    fn mul(&self, other: &BigUint) -> BigUint {
        if self.is_zero() || other.is_zero() {
            return BigUint::zero();
        }
        let mut out = vec![0u64; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry: u128 = 0;
            for (j, &b) in other.limbs.iter().enumerate() {
                let index = i + j;
                let sum = out[index] as u128 + (a as u128) * (b as u128) + carry;
                out[index] = sum as u64;
                carry = sum >> 64;
            }
            let mut index = i + other.limbs.len();
            while carry > 0 {
                let sum = out[index] as u128 + carry;
                out[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        let mut value = BigUint { limbs: out };
        value.trim();
        value
    }

    /// `self mod modulus` by binary shift-and-subtract.
    ///
    /// One bit at a time keeps the running value below the modulus, so the
    /// reduction needs only comparison and subtraction. It is O(bits · limbs)
    /// — a few thousand limb operations for a 2048-bit value — and is used only
    /// to bring a key component or an encoded representative into range, never
    /// on a hot path where a division algorithm would be worth the risk.
    fn rem(&self, modulus: &BigUint) -> BigUint {
        debug_assert!(!modulus.is_zero());
        if self < modulus {
            return self.clone();
        }
        let mut remainder = BigUint::zero();
        for index in (0..self.bit_len()).rev() {
            remainder = remainder.add(&remainder);
            if self.bit(index) {
                remainder = remainder.add(&BigUint::from_u64(1));
            }
            if &remainder >= modulus {
                remainder = remainder.sub(modulus);
            }
        }
        remainder
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.limbs.len() != other.limbs.len() {
            return self.limbs.len().cmp(&other.limbs.len());
        }
        for index in (0..self.limbs.len()).rev() {
            match self.limbs[index].cmp(&other.limbs[index]) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            }
        }
        std::cmp::Ordering::Equal
    }
}

impl fmt::Debug for BigUint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BigUint({} bits)", self.bit_len())
    }
}

/// One modulus with its Montgomery constants, computed by repeated doubling so
/// no general division is needed anywhere in this file.
#[derive(Clone)]
struct Modulus {
    n: BigUint,
    limbs: usize,
    /// `n` zero-padded to exactly `limbs` 64-bit limbs. Fixed width is what
    /// makes every loop below independent of the operand values.
    n_limbs: Vec<u64>,
    /// Public modulus width in bits. The ladder runs exactly this many steps.
    bits: usize,
    /// `-n^{-1} mod 2^64`; valid because `n` is odd.
    n0inv: u64,
    /// `R mod n`, R = 2^(64*limbs).
    r_mod: BigUint,
    /// `R^2 mod n`.
    r2_mod: BigUint,
}

impl Modulus {
    fn new(n: BigUint) -> Modulus {
        assert!(n.is_odd(), "RSA moduli are odd");
        let limbs = n.limbs.len().max(1);
        let r_mod = pow2_mod(limbs * 64, &n);
        let r2_mod = pow2_mod(limbs * 128, &n);
        let n0inv = montgomery_n0inv(n.limbs[0]);
        Modulus {
            n_limbs: fixed_width(&n, limbs),
            bits: n.bit_len(),
            n,
            limbs,
            n0inv,
            r_mod,
            r2_mod,
        }
    }

    /// `a * b * R^{-1} mod n`, for `a, b < n`.
    fn mont_mul(&self, a: &BigUint, b: &BigUint) -> BigUint {
        let k = self.limbs;
        let mut t = vec![0u64; 2 * k + 2];
        for (i, &ai) in a.limbs.iter().enumerate().take(k) {
            let mut carry: u128 = 0;
            for j in 0..k {
                let bj = b.limbs.get(j).copied().unwrap_or(0);
                let index = i + j;
                let sum = t[index] as u128 + (ai as u128) * (bj as u128) + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
            }
            let mut index = i + k;
            while carry > 0 {
                let sum = t[index] as u128 + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        for i in 0..k {
            let m = t[i].wrapping_mul(self.n0inv);
            let mut carry: u128 = 0;
            for j in 0..k {
                let nj = self.n.limbs.get(j).copied().unwrap_or(0);
                let index = i + j;
                let sum = t[index] as u128 + (m as u128) * (nj as u128) + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
            }
            let mut index = i + k;
            while carry > 0 {
                let sum = t[index] as u128 + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        let mut result = BigUint {
            limbs: t[k..].to_vec(),
        };
        result.trim();
        if result >= self.n {
            result = result.sub(&self.n);
        }
        result
    }

    fn to_mont(&self, value: &BigUint) -> BigUint {
        self.mont_mul(value, &self.r2_mod)
    }

    fn from_mont(&self, value: &BigUint) -> BigUint {
        self.mont_mul(value, &BigUint::from_u64(1))
    }

    // ---- constant-time fixed-width path ---------------------------------

    /// `a * b * R^{-1} mod n`, constant-time in the operand values.
    ///
    /// CIOS over exactly `limbs` limbs: every inner loop runs `limbs` times,
    /// carry propagation is unrolled to a fixed length, and the final
    /// conditional subtraction uses a borrow mask instead of a branch. `a` and
    /// `b` must be zero-padded to `self.limbs` and less than `n`.
    fn mont_mul_ct(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let k = self.limbs;
        debug_assert_eq!(a.len(), k);
        debug_assert_eq!(b.len(), k);
        let mut t = vec![0u64; k + 2];
        for i in 0..k {
            let mut carry: u128 = 0;
            for j in 0..k {
                let sum = t[j] as u128 + (a[i] as u128) * (b[j] as u128) + carry;
                t[j] = sum as u64;
                carry = sum >> 64;
            }
            // Fixed-length carry propagation (no `while carry != 0`).
            let sum = t[k] as u128 + carry;
            t[k] = sum as u64;
            t[k + 1] = (sum >> 64) as u64;

            let m = t[0].wrapping_mul(self.n0inv);
            let mut carry: u128 = 0;
            for j in 0..k {
                let sum = t[j] as u128 + (m as u128) * (self.n_limbs[j] as u128) + carry;
                t[j] = sum as u64;
                carry = sum >> 64;
            }
            let sum = t[k] as u128 + carry;
            t[k] = sum as u64;
            t[k + 1] = t[k + 1].wrapping_add((sum >> 64) as u64);

            // Divide by 2^64 (shift one limb left). Fixed length.
            for j in 0..=k {
                t[j] = t[j + 1];
            }
            t[k + 1] = 0;
        }
        // The post-shift accumulator occupies k+1 limbs: `t[k]` is the carry
        // out of the low k limbs and must participate in the reduction.
        let mut result = t[..=k].to_vec();
        let modulus = padded(&self.n_limbs, k + 1);
        // `t < 2n` after CIOS, so one conditional subtraction is enough; a
        // second mask-only subtraction is harmless and keeps the fixed loop
        // shape if an unusual input ever lands above n.
        conditional_subtract(&mut result, &modulus);
        conditional_subtract(&mut result, &modulus);
        result.truncate(k);
        result
    }

    fn to_mont_ct(&self, value: &[u64]) -> Vec<u64> {
        let r2 = fixed_width(&self.r2_mod, self.limbs);
        self.mont_mul_ct(value, &r2)
    }

    fn from_mont_ct(&self, value: &[u64]) -> Vec<u64> {
        let mut one = vec![0u64; self.limbs];
        one[0] = 1;
        self.mont_mul_ct(value, &one)
    }

    /// `base^exponent mod n` with a Montgomery ladder.
    ///
    /// Both the base and the exponent are zero-padded to `self.limbs`; the loop
    /// runs `self.bits` times regardless of the exponent, and each step performs
    /// exactly one squaring and one multiplication. The exponent bit selects
    /// which product goes where through a mask-only swap. The exponent must be
    /// below the modulus (the RSA CRT exponents `dp < p` and `dq < q` are), so
    /// iterating the public modulus width covers every exponent bit without
    /// measuring the secret's bit length.
    fn ladder_pow(&self, base: &BigUint, exponent: &BigUint) -> BigUint {
        let k = self.limbs;
        let base_mont = self.to_mont_ct(&fixed_width(base, k));
        let exponent = fixed_width(exponent, k);
        let mut r0 = fixed_width(&self.r_mod, k); // R mod n == 1 in Montgomery form
        let mut r1 = base_mont;
        for index in (0..self.bits).rev() {
            let bit = (exponent[index / 64] >> (index % 64)) & 1;
            cswap(bit, &mut r0, &mut r1);
            let product = self.mont_mul_ct(&r0, &r1);
            r1 = product;
            r0 = self.mont_mul_ct(&r0, &r0);
            cswap(bit, &mut r0, &mut r1);
        }
        BigUint::from_fixed(&self.from_mont_ct(&r0))
    }

    fn pow(&self, base: &BigUint, exponent: &BigUint) -> BigUint {
        if exponent.is_zero() {
            return BigUint::from_u64(1);
        }
        let base_mont = self.to_mont(&base.rem(&self.n));
        let mut result = self.r_mod.clone();
        for index in (0..exponent.bit_len()).rev() {
            result = self.mont_mul(&result, &result);
            if exponent.bit(index) {
                result = self.mont_mul(&result, &base_mont);
            }
        }
        self.from_mont(&result)
    }
}

/// Zero-pad a value to exactly `limbs` 64-bit limbs, so every constant-time
/// loop below has a value-independent length. A wider value is a programming
/// error (all callers pass values below their modulus).
fn fixed_width(value: &BigUint, limbs: usize) -> Vec<u64> {
    debug_assert!(value.limbs.len() <= limbs, "value wider than its modulus");
    let mut out = vec![0u64; limbs];
    for (index, limb) in value.limbs.iter().enumerate().take(limbs) {
        out[index] = *limb;
    }
    out
}

/// Zero-extend an already-fixed-width limb slice to `width` limbs.
fn padded(limbs: &[u64], width: usize) -> Vec<u64> {
    let mut out = vec![0u64; width];
    for (index, limb) in limbs.iter().enumerate().take(width) {
        out[index] = *limb;
    }
    out
}

/// Fixed-width subtraction; the returned borrow is `0` or `1`.
fn sub_fixed(a: &[u64], b: &[u64]) -> (Vec<u64>, u64) {
    debug_assert_eq!(a.len(), b.len());
    let mut out = vec![0u64; a.len()];
    let mut borrow = 0u64;
    for index in 0..a.len() {
        let (difference, borrow1) = a[index].overflowing_sub(b[index]);
        let (difference, borrow2) = difference.overflowing_sub(borrow);
        out[index] = difference;
        borrow = u64::from(borrow1) | u64::from(borrow2);
    }
    (out, borrow)
}

/// Fixed-width addition modulo 2^(64*len); the carry out is discarded.
fn add_fixed(a: &[u64], b: &[u64]) -> Vec<u64> {
    debug_assert_eq!(a.len(), b.len());
    let mut out = vec![0u64; a.len()];
    let mut carry = 0u64;
    for index in 0..a.len() {
        let (sum, carry1) = a[index].overflowing_add(b[index]);
        let (sum, carry2) = sum.overflowing_add(carry);
        out[index] = sum;
        carry = u64::from(carry1) | u64::from(carry2);
    }
    out
}

/// Low `len` limbs of `a * b`, fixed loop counts.
fn mul_low(a: &[u64], b: &[u64]) -> Vec<u64> {
    debug_assert_eq!(a.len(), b.len());
    let k = a.len();
    let mut out = vec![0u64; k];
    for i in 0..k {
        let mut carry: u128 = 0;
        for j in 0..k - i {
            let index = i + j;
            let sum = out[index] as u128 + (a[i] as u128) * (b[j] as u128) + carry;
            out[index] = sum as u64;
            carry = sum >> 64;
        }
    }
    out
}

/// If `value >= modulus`, replace it with `value - modulus`; branch-free, with
/// a mask derived from the subtraction borrow.
fn conditional_subtract(value: &mut [u64], modulus: &[u64]) {
    let (difference, borrow) = sub_fixed(value, modulus);
    let take = (borrow as u64).wrapping_sub(1);
    for index in 0..value.len() {
        value[index] = (difference[index] & take) | (value[index] & !take);
    }
}

/// Select `if_one` when `bit` is 1 and `if_zero` otherwise, mask-only.
fn select_masked(bit: u64, if_one: &[u64], if_zero: &[u64]) -> Vec<u64> {
    debug_assert_eq!(if_one.len(), if_zero.len());
    let mask = 0u64.wrapping_sub(bit & 1);
    (0..if_one.len())
        .map(|index| (if_one[index] & mask) | (if_zero[index] & !mask))
        .collect()
}

/// Swap `a` and `b` when `bit` is 1, using only mask arithmetic.
fn cswap(bit: u64, a: &mut [u64], b: &mut [u64]) {
    debug_assert_eq!(a.len(), b.len());
    let mask = 0u64.wrapping_sub(bit & 1);
    for index in 0..a.len() {
        let difference = (a[index] ^ b[index]) & mask;
        a[index] ^= difference;
        b[index] ^= difference;
    }
}

/// `2^bits mod n`, by repeated doubling. `n` must be odd and > 1.
fn pow2_mod(bits: usize, n: &BigUint) -> BigUint {
    let mut value = BigUint::from_u64(1);
    for _ in 0..bits {
        value = value.add(&value);
        if &value >= n {
            value = value.sub(n);
        }
    }
    value
}

/// `-n^{-1} mod 2^64` for odd `n`, by Newton iteration on wrapping arithmetic.
fn montgomery_n0inv(n: u64) -> u64 {
    let mut x = 1u64;
    for _ in 0..6 {
        x = x.wrapping_mul(2u64.wrapping_sub(n.wrapping_mul(x)));
    }
    x.wrapping_neg()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn biguint_bytes_and_ordering_round_trip() {
        let cases: &[&[u8]] = &[
            &[],
            &[1],
            &[0xff],
            &[1, 0],
            &[0xff; 8],
            &[0xff; 9],
            &[0; 40],
        ];
        for bytes in cases {
            let value = BigUint::from_be_bytes(bytes);
            let round = value.to_be_bytes();
            let expected: Vec<u8> = {
                let mut trimmed = bytes.to_vec();
                while trimmed.first() == Some(&0) {
                    trimmed.remove(0);
                }
                if trimmed.is_empty() {
                    vec![0]
                } else {
                    trimmed
                }
            };
            assert_eq!(round, expected, "bytes {bytes:?}");
        }
        assert!(BigUint::from_u64(2) < BigUint::from_u64(3));
        assert!(BigUint::from_be_bytes(&[1, 0]) > BigUint::from_be_bytes(&[0xff]));
    }

    #[test]
    fn montgomery_multiplication_matches_schoolbook() {
        // R mod n and R^2 mod n are only useful if mont_mul really divides by R.
        let n = BigUint::from_be_bytes(&[
            0x9f, 0x2c, 0x44, 0x18, 0x1d, 0xe5, 0x1c, 0x0f, 0x5b, 0x7f, 0x63, 0x1a, 0x9b, 0x2d,
            0xf3, 0xd1,
        ]);
        let modulus = Modulus::new(n.clone());
        for a in 1u64..8 {
            for b in 1u64..8 {
                let a = BigUint::from_u64(a);
                let b = BigUint::from_u64(b);
                let got = modulus
                    .from_mont(&modulus.mont_mul(&modulus.to_mont(&a), &modulus.to_mont(&b)));
                let expected = a.mul(&b).rem(&n);
                assert_eq!(got, expected, "a={a:?} b={b:?}");
            }
        }
        // A modpow against a plain repeated-multiplication oracle.
        let base = BigUint::from_u64(7);
        let exp = BigUint::from_u64(13);
        let got = modulus.pow(&base, &exp);
        let mut expected = BigUint::from_u64(1);
        for _ in 0..13 {
            expected = expected.mul(&base).rem(&n);
        }
        assert_eq!(got, expected);
    }

    /// Deterministic pseudo-random bytes (xorshift64*).
    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            out.extend_from_slice(&value.to_be_bytes());
        }
        out.truncate(len);
        out
    }

    #[test]
    fn montgomery_ct_matches_the_reference_multiplication() {
        for seed in 1..=8u64 {
            let mut n_bytes = pseudo_random(24, seed);
            n_bytes[23] |= 1; // odd (from_be_bytes: last byte is the low byte)
            let n = BigUint::from_be_bytes(&n_bytes);
            let modulus = Modulus::new(n.clone());
            let a = BigUint::from_be_bytes(&pseudo_random(24, seed + 100)).rem(&n);
            let b = BigUint::from_be_bytes(&pseudo_random(24, seed + 200)).rem(&n);
            let reference = modulus.mont_mul(
                &modulus.to_mont(&a),
                &modulus.to_mont(&b),
            );
            let reference = modulus.from_mont(&reference);
            let ct = modulus.mont_mul_ct(
                &modulus.to_mont_ct(&fixed_width(&a, modulus.limbs)),
                &modulus.to_mont_ct(&fixed_width(&b, modulus.limbs)),
            );
            let ct = modulus.from_mont_ct(&ct);
            assert_eq!(BigUint::from_fixed(&ct), reference, "seed {seed}");
        }
    }

    #[test]
    fn ladder_matches_square_and_multiply() {
        for seed in 1..=6u64 {
            let mut n_bytes = pseudo_random(24, seed * 7);
            n_bytes[23] |= 1;
            let n = BigUint::from_be_bytes(&n_bytes);
            let modulus = Modulus::new(n.clone());
            for small in 0..12u64 {
                let base = BigUint::from_be_bytes(&pseudo_random(24, seed * 31 + small)).rem(&n);
                let exponent = BigUint::from_u64(small);
                assert_eq!(
                    modulus.ladder_pow(&base, &exponent),
                    modulus.pow(&base, &exponent),
                    "seed {seed} small exponent {small}"
                );
            }
            // A full-width exponent too.
            let base = BigUint::from_be_bytes(&pseudo_random(24, seed * 97)).rem(&n);
            let exponent =
                BigUint::from_be_bytes(&pseudo_random(24, seed * 193)).rem(&n);
            assert_eq!(
                modulus.ladder_pow(&base, &exponent),
                modulus.pow(&base, &exponent),
                "seed {seed} wide exponent"
            );
        }
    }



    #[test]
    fn fixed_width_helpers_are_masked_not_branched() {
        let a = BigUint::from_u64(5);
        let b = BigUint::from_u64(9);
        let (difference, borrow) = sub_fixed(&fixed_width(&a, 2), &fixed_width(&b, 2));
        assert_eq!(borrow, 1, "5 - 9 borrows");
        assert_eq!(
            add_fixed(&difference, &fixed_width(&b, 2)),
            fixed_width(&a, 2),
            "the wrapped difference plus the subtrahend restores the minuend"
        );
        let (difference, borrow) = sub_fixed(&fixed_width(&b, 2), &fixed_width(&a, 2));
        assert_eq!(borrow, 0);
        assert_eq!(BigUint::from_fixed(&difference), BigUint::from_u64(4));

        let mut value = fixed_width(&BigUint::from_u64(12), 2);
        conditional_subtract(&mut value, &fixed_width(&BigUint::from_u64(10), 2));
        assert_eq!(BigUint::from_fixed(&value), BigUint::from_u64(2));
        let mut value = fixed_width(&BigUint::from_u64(7), 2);
        conditional_subtract(&mut value, &fixed_width(&BigUint::from_u64(10), 2));
        assert_eq!(BigUint::from_fixed(&value), BigUint::from_u64(7));

        let mut left = fixed_width(&BigUint::from_u64(0x1111), 1);
        let mut right = fixed_width(&BigUint::from_u64(0x2222), 1);
        cswap(0, &mut left, &mut right);
        assert_eq!(BigUint::from_fixed(&left), BigUint::from_u64(0x1111));
        cswap(1, &mut left, &mut right);
        assert_eq!(BigUint::from_fixed(&left), BigUint::from_u64(0x2222));
        assert_eq!(BigUint::from_fixed(&right), BigUint::from_u64(0x1111));

        assert_eq!(
            BigUint::from_fixed(&mul_low(
                &fixed_width(&BigUint::from_u64(3), 2),
                &fixed_width(&BigUint::from_u64(5), 2)
            )),
            BigUint::from_u64(15)
        );
    }

    #[test]
    fn pow2_mod_matches_known_values() {
        let n = BigUint::from_u64(97);
        assert_eq!(pow2_mod(0, &n), BigUint::from_u64(1));
        assert_eq!(pow2_mod(10, &n), BigUint::from_u64(1024 % 97));
        // 2^64 mod 97
        let mut expected = 1u64;
        for _ in 0..64 {
            expected = (expected * 2) % 97;
        }
        assert_eq!(pow2_mod(64, &n), BigUint::from_u64(expected));
    }

    #[test]
    fn der_reader_is_strict() {
        // Not a SEQUENCE.
        assert!(Der::new(&[0x04, 0x00]).unwrap().sequence().is_err());
        // Indefinite length.
        assert!(Der::new(&[0x30, 0x80, 0x00, 0x00])
            .unwrap()
            .sequence()
            .is_err());
        // Length past the end.
        assert!(Der::new(&[0x30, 0x05, 0x00]).unwrap().sequence().is_err());
        // Non-minimal long form.
        assert!(Der::new(&[0x30, 0x81, 0x01, 0x00])
            .unwrap()
            .sequence()
            .is_err());
        // Negative INTEGER.
        assert!(Der::new(&[0x02, 0x01, 0x80]).unwrap().integer().is_err());
        // Non-minimal INTEGER.
        assert!(Der::new(&[0x02, 0x02, 0x00, 0x01])
            .unwrap()
            .integer()
            .is_err());
        // Trailing bytes after the value.
        let mut reader = Der::new(&[0x02, 0x01, 0x01, 0x00]).unwrap();
        assert_eq!(reader.integer().unwrap(), BigUint::from_u64(1));
        assert!(reader.finish().is_err());
    }

    #[test]
    fn pem_extraction_accepts_wrapping_and_rejects_garbage() {
        let (label, body) =
            pem_block("-----BEGIN TEST-----\nAAEC\nAwQF\n-----END TEST-----\n").unwrap();
        assert_eq!(label, "TEST");
        assert_eq!(body, "AAECAwQF");
        assert!(pem_block("no pem here").is_err());
        assert!(pem_block("-----BEGIN X-----\nabc\n").is_err());
        let encrypted = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n\nAAAA\n-----END RSA PRIVATE KEY-----\n";
        let err = pem_block(encrypted).unwrap_err();
        assert_eq!(err.class_name(), "UnsupportedVersion");
        assert!(err.message().contains("encrypted"));
    }

    #[test]
    fn n0inv_is_the_negated_inverse() {
        for n in [3u64, 5, 97, 0x9e37_79b9_7f4a_7c15 | 1, u64::MAX - 58] {
            let inv = montgomery_n0inv(n);
            assert_eq!(n.wrapping_mul(inv.wrapping_neg()), 1);
        }
    }
}
