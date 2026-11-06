//! Google service-account authentication court (subphase R21).
//!
//! The cryptographic core is checked against the **published** RS256 test
//! vector in RFC 7515 Appendix A.2: the exact 2048-bit RSA key, the exact
//! signing input, and the exact signature octets. Nothing here needs Google or
//! a network.
//!
//! The OAuth2 exchange, its cache, and its error handling run against an
//! in-process HTTP endpoint, so token caching, refresh windows, refusals, and
//! redaction are deterministic.
//!
//! What this cannot prove: that a live Google deployment accepts the assertion.
//! No service-account credential exists in this environment, so
//! `bench/scripts/gcs_court.sh` records `no_credentials` rather than claiming a
//! live run.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use weftfs::service_account::{ServiceAccountAuth, ServiceAccountKey, DEFAULT_TOKEN_URI};

/// RFC 7515 Appendix A.2.1, the `"kty":"RSA"` JWK re-encoded as PKCS#1
/// (`BEGIN RSA PRIVATE KEY`). Test key only; it protects nothing.
const RFC7515_KEY_PKCS1: &str = r#"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd/wWJcyQoTbji9k0
l8W26mPddxHmfHQp+Vaw+4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL+yRT+SFd2lZS+pC
gNMsD1W/YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb/7OMg0LOL+bSf63kpaSHSX
ndS5z5rexMdbBYUsLA9e+KXBdQOS+UTo7WTBEMa2R2CapHg665xsmtdVMTBQY4uD
Zlxvb3qCo5ZwKh9kG4LT6/I5IhlJH7aGhyxXFvUK+DWNmoudF8NAco9/h9iaGNj8
q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQIDAQABAoIBABKucaRpzQorw35S
bEUAVx8dYXUdZOlJcHtiWQ+dC6V8ljxAHj/PLyzTveyI5QO/xkObCyjIL303l2cf
UhPu2MFaJdjVzqACXuOrLot/eSFvxjvqVidTtAZExqFRJ9mylUVAoLvhowVWmC1O
n95fZCXxTUtxNEG1Xcc7m0rtzJKs45J+N/V9DP1edYH6USyPSWGp6wuA+KgHRnKK
Vf9GRx80JQY7nVNkL17eHoTWEwga+lwi0FEoW9Y7lDtWXYmKBWhUE+U8PGxlJf8f
40493HDw1WRQ/aSLoS4QTp3rn7gYgeHEvfJdkkf0UMhlknlo53M09EFPdadQ4TlU
bjqKc50CgYEA4BzEEOtIpmVdVEZNCqS7baC4crd0pqnRH/5IB3jw3bcxGn6QLvnE
tfdUdiYrqBdss1l58BQ3KhooKeQTa9AB0Hw/Py5PJdTJNPY8cQn7ouZ2KKDcmnPG
BY5t7yLc1QlQ5xHdwW1VhvKn+nXqhJTBgIPgtldC+KDV5z+y2XDwGUcCgYEAuQPE
fgmVtjL0Uyyx88GZFF1fOunH3+7cepKmtH4pxhtCoHqpWmT8YAmZxaewHgHAjLYs
p1ZSe7zFYHj7C6ul7TjeLQeZD/YwD66t62wDmpe/HlB+TnBA+njbglfIsRLtXlnD
zQkv5dTltRJ11BKBBypeeF6689rjcJIDEz9RWdcCgYAHAp9XcCSrn8wVkMVkKdb7
DOX4IKjzdahm+ctDAJN4O/y7OW5FKebvUjdAIt2GuoTZ71iTG+7F0F+lP88jtjP4
U4qe7VHoewl4MKOfXZKTe+YCS1XbNvfgwJ3Ltyl1OH9hWvu2yza7q+d5PCsDzqtm
27kxuvULVeya+TEdAB1ijQKBgQCH/3r6YrVH/uCWGy6bzV1nGNOdjKc9tmkfOJmN
54dxdixdpozCQ6U4OxZrsj3FcOhHBsqAHvX2uuYjagqvo3cOj1TRqNocX40omfCC
Mx3bD1yPPf/6TI2XECva/ggqEY2mYzmIiA5LVVmc5nrybr+lssFKneeyxN2Wq93S
0iJMdQKBgCGHewxzoa1r8ZMD0LETNrToK423K377UCYqXfg5XMclbrjPbEC3YI1Z
NqMtuhdBJqUnBi6tjKMF+34Xf0CUN8ncuXGO2CAYvO8PdyCixHX52ybaDjy1FtCE
6yUXjoKNXKvUm7MWGsAYH6f4IegOetN5NvmUMFStCSkh7ixZLkN1
-----END RSA PRIVATE KEY-----"#;

/// The same key as PKCS#8 (`BEGIN PRIVATE KEY`), the shape Google ships.
const RFC7515_KEY_PKCS8: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCh+BYK4uPJtGXO
jS1lYmM2K5J9vinh8CR3/BYlzJChNuOL2TSXxbbqY913EeZ8dCn5VrD7io8ImtxL
aYk8wTM/U+3QGbh3hCUv7JFP5IV3aVlL6kKA0ywPVb9ilE8TA5a8bpvfbr3SvaNn
juygxmj3AbONv/s4yDQs4v5tJ/reSlpIdJed1LnPmt7Ex1sFhSwsD174pcF1A5L5
ROjtZMEQxrZHYJqkeDrrnGya11UxMFBji4NmXG9veoKjlnAqH2QbgtPr8jkiGUkf
toaHLFcW9Qr4NY2ai50Xw0Byj3+H2JoY2PyrZ62EWQwuz3WTOTY8BwNNb2BvniHg
VFbK5emhAgMBAAECggEAEq5xpGnNCivDflJsRQBXHx1hdR1k6Ulwe2JZD50LpXyW
PEAeP88vLNO97IjlA7/GQ5sLKMgvfTeXZx9SE+7YwVol2NXOoAJe46sui395IW/G
O+pWJ1O0BkTGoVEn2bKVRUCgu+GjBVaYLU6f3l9kJfFNS3E0QbVdxzubSu3Mkqzj
kn439X0M/V51gfpRLI9JYanrC4D4qAdGcopV/0ZHHzQlBjudU2QvXt4ehNYTCBr6
XCLQUShb1juUO1ZdiYoFaFQT5Tw8bGUl/x/jTj3ccPDVZFD9pIuhLhBOneufuBiB
4cS98l2SR/RQyGWSeWjnczT0QU91p1DhOVRuOopznQKBgQDgHMQQ60imZV1URk0K
pLttoLhyt3SmqdEf/kgHePDdtzEafpAu+cS191R2JiuoF2yzWXnwFDcqGigp5BNr
0AHQfD8/Lk8l1Mk09jxxCfui5nYooNyac8YFjm3vItzVCVDnEd3BbVWG8qf6deqE
lMGAg+C2V0L4oNXnP7LZcPAZRwKBgQC5A8R+CZW2MvRTLLHzwZkUXV866cff7tx6
kqa0finGG0KgeqlaZPxgCZnFp7AeAcCMtiynVlJ7vMVgePsLq6XtON4tB5kP9jAP
rq3rbAOal78eUH5OcED6eNuCV8ixEu1eWcPNCS/l1OW1EnXUEoEHKl54Xrrz2uNw
kgMTP1FZ1wKBgAcCn1dwJKufzBWQxWQp1vsM5fggqPN1qGb5y0MAk3g7/Ls5bkUp
5u9SN0Ai3Ya6hNnvWJMb7sXQX6U/zyO2M/hTip7tUeh7CXgwo59dkpN75gJLVds2
9+DAncu3KXU4f2Fa+7bLNrur53k8KwPOq2bbuTG69QtV7Jr5MR0AHWKNAoGBAIf/
evpitUf+4JYbLpvNXWcY052Mpz22aR84mY3nh3F2LF2mjMJDpTg7FmuyPcVw6EcG
yoAe9fa65iNqCq+jdw6PVNGo2hxfjSiZ8IIzHdsPXI89//pMjZcQK9r+CCoRjaZj
OYiIDktVWZzmevJuv6WywUqd57LE3Zar3dLSIkx1AoGAIYd7DHOhrWvxkwPQsRM2
tOgrjbcrfvtQJipd+DlcxyVuuM9sQLdgjVk2oy26F0EmpScGLq2MowX7fhd/QJQ3
ydy5cY7YIBi87w93IKLEdfnbJtoOPLUW0ITrJReOgo1cq9SbsxYawBgfp/gh6A56
03k2+ZQwVK0JKSHuLFkuQ3U=
-----END PRIVATE KEY-----"#;

/// RFC 7515 A.2.1: BASE64URL(header) || '.' || BASE64URL(payload).
const RFC7515_SIGNING_INPUT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ";

/// RFC 7515 A.2.1: BASE64URL(RSASSA-PKCS1-v1_5 SHA-256 signature).
const RFC7515_SIGNATURE: &str = "cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw";

const RFC7515_HEADER: &[u8] = b"{\"alg\":\"RS256\"}";
const RFC7515_PAYLOAD: &[u8] =
    b"{\"iss\":\"joe\",\r\n \"exp\":1300819380,\r\n \"http://example.com/is_root\":true}";

fn key_json(pem: &str, token_uri: &str) -> String {
    serde_json::json!({
        "type": "service_account",
        "project_id": "weft-test",
        "private_key_id": "kid-1234",
        "private_key": pem,
        "client_email": "weft-store@weft-test.iam.gserviceaccount.com",
        "token_uri": token_uri,
    })
    .to_string()
}

fn key_from(pem: &str) -> ServiceAccountKey {
    ServiceAccountKey::from_json_bytes(key_json(pem, DEFAULT_TOKEN_URI).as_bytes())
        .expect("fixture key parses")
}

// ---------------------------------------------------------------------------
// 1. The published RS256 vector
// ---------------------------------------------------------------------------

#[test]
fn rfc7515_rs256_vector_signs_to_the_published_bytes() {
    let key = key_from(RFC7515_KEY_PKCS1);
    assert_eq!(
        ServiceAccountKey::signing_input(RFC7515_HEADER, RFC7515_PAYLOAD),
        RFC7515_SIGNING_INPUT
    );
    let jws = key
        .sign_compact(RFC7515_HEADER, RFC7515_PAYLOAD)
        .expect("sign");
    assert_eq!(
        jws,
        format!("{RFC7515_SIGNING_INPUT}.{RFC7515_SIGNATURE}"),
        "the RS256 signature must equal RFC 7515 A.2.1 octet for octet"
    );
}

#[test]
fn pkcs8_and_pkcs1_encodings_are_the_same_key() {
    let a = key_from(RFC7515_KEY_PKCS1);
    let b = key_from(RFC7515_KEY_PKCS8);
    assert_eq!(a, b);
    assert_eq!(a.fingerprint_hex(), b.fingerprint_hex());
    assert_eq!(a.modulus_bits(), 2048);
    assert_eq!(a.client_email(), "weft-store@weft-test.iam.gserviceaccount.com");
    // Both produce the same signature, so the PKCS#8 unwrapping is exact.
    assert_eq!(
        a.sign_compact(RFC7515_HEADER, RFC7515_PAYLOAD).unwrap(),
        b.sign_compact(RFC7515_HEADER, RFC7515_PAYLOAD).unwrap()
    );
}

// ---------------------------------------------------------------------------
// 2. Assertion shape (the standard header/claim form)
// ---------------------------------------------------------------------------

fn decode(segment: &str) -> serde_json::Value {
    let bytes = weftfs::b64::decode_any(segment).expect("base64url segment");
    serde_json::from_slice(&bytes).expect("segment is JSON")
}

#[test]
fn assertion_has_the_standard_header_and_claim_shape() {
    let key = key_from(RFC7515_KEY_PKCS1);
    let now_ns = 1_700_000_000_123_456_789i64;
    let scope = "https://www.googleapis.com/auth/devstorage.read_write";
    let jwt = key.assertion(scope, now_ns).expect("assertion");
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3, "JWS compact serialization has three parts");

    let header = decode(parts[0]);
    let object = header.as_object().expect("header object");
    assert_eq!(object.get("alg").and_then(|v| v.as_str()), Some("RS256"));
    assert_eq!(object.get("typ").and_then(|v| v.as_str()), Some("JWT"));
    assert_eq!(
        object.get("kid").and_then(|v| v.as_str()),
        Some("kid-1234"),
        "a private_key_id is carried as kid"
    );

    let claims = decode(parts[1]);
    assert_eq!(
        claims.get("iss").and_then(|v| v.as_str()),
        Some("weft-store@weft-test.iam.gserviceaccount.com")
    );
    assert_eq!(claims.get("scope").and_then(|v| v.as_str()), Some(scope));
    assert_eq!(
        claims.get("aud").and_then(|v| v.as_str()),
        Some(DEFAULT_TOKEN_URI)
    );
    let iat = claims.get("iat").and_then(|v| v.as_i64()).expect("iat");
    let exp = claims.get("exp").and_then(|v| v.as_i64()).expect("exp");
    assert_eq!(iat, now_ns.div_euclid(1_000_000_000));
    assert_eq!(exp - iat, 3600, "Google accepts at most one hour");
    assert!(!parts[2].is_empty());
}

#[test]
fn assertion_for_an_empty_scope_is_refused() {
    let key = key_from(RFC7515_KEY_PKCS1);
    let err = key.assertion("", 1_700_000_000_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument");
}

// ---------------------------------------------------------------------------
// 3. Refusals
// ---------------------------------------------------------------------------

#[test]
fn non_service_account_credentials_are_refused_by_name() {
    let json = serde_json::json!({
        "type": "authorized_user",
        "client_id": "x.apps.googleusercontent.com",
        "client_secret": "secret",
        "refresh_token": "token",
    })
    .to_string();
    let err = ServiceAccountKey::from_json_bytes(json.as_bytes()).unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(err.message().contains("service_account"), "{err}");
    assert!(err.message().contains("refresh-token"), "{err}");
}

#[test]
fn encrypted_and_non_rsa_keys_are_refused_by_name() {
    let encrypted = "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n";
    let err = ServiceAccountKey::from_json_bytes(key_json(encrypted, DEFAULT_TOKEN_URI).as_bytes())
        .unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(err.message().contains("encrypted"), "{err}");

    // A PKCS#8 key whose algorithm OID is not rsaEncryption (Ed25519 here).
    let ed25519_pkcs8 = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n-----END PRIVATE KEY-----\n";
    let err =
        ServiceAccountKey::from_json_bytes(key_json(ed25519_pkcs8, DEFAULT_TOKEN_URI).as_bytes())
            .unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(err.message().contains("RSA"), "{err}");
}

#[test]
fn an_inconsistent_key_is_refused() {
    // Corrupt the last integer (`qinv`) so p*q still equals n but the CRT
    // parameters no longer agree with the private exponent.
    let label = "RSA PRIVATE KEY";
    let body: String = RFC7515_KEY_PKCS1
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let mut der = weftfs::b64::decode_any(&body).expect("der");
    let last = der.len() - 1;
    der[last] ^= 0x01;
    let encoded = weftfs::b64::encode_standard(&der);
    let mut pem = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {label}-----\n"));
    let err =
        ServiceAccountKey::from_json_bytes(key_json(&pem, DEFAULT_TOKEN_URI).as_bytes())
            .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument", "{err}");
    assert!(
        err.message().contains("CRT") || err.message().contains("inconsistent"),
        "{err}"
    );
}

#[test]
fn malformed_input_never_echoes_key_material() {
    let json = key_json("-----BEGIN RSA PRIVATE KEY-----\nnot base64!!\n-----END RSA PRIVATE KEY-----\n", DEFAULT_TOKEN_URI);
    let err = ServiceAccountKey::from_json_bytes(json.as_bytes()).unwrap_err();
    assert!(!err.message().contains("not base64!!"), "{err}");
    assert!(err.message().contains("not printed"), "{err}");
    let err = ServiceAccountKey::from_json_bytes(b"{\"type\":\"service_account\"}").unwrap_err();
    assert!(!err.message().contains("PRIVATE KEY"), "{err}");
}

// ---------------------------------------------------------------------------
// 4. Token endpoint, cache, and redaction
// ---------------------------------------------------------------------------

struct TokenServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    assertions: Arc<Mutex<Vec<String>>>,
    authorizations: Arc<Mutex<Vec<String>>>,
    response: Arc<Mutex<(u16, String)>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TokenServer {
    fn start(status: u16, body: String) -> TokenServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind token server");
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let assertions = Arc::new(Mutex::new(Vec::new()));
        let authorizations = Arc::new(Mutex::new(Vec::new()));
        let response = Arc::new(Mutex::new((status, body)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_assertions = Arc::clone(&assertions);
        let thread_authorizations = Arc::clone(&authorizations);
        let thread_response = Arc::clone(&response);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        handle(
                            &mut stream,
                            &thread_requests,
                            &thread_assertions,
                            &thread_authorizations,
                            &thread_response,
                        );
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        TokenServer {
            addr,
            requests,
            assertions,
            authorizations,
            response,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/token", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn assertions(&self) -> Vec<String> {
        self.assertions.lock().unwrap().clone()
    }

    fn authorizations(&self) -> Vec<String> {
        self.authorizations.lock().unwrap().clone()
    }

    fn set_response(&self, status: u16, body: String) {
        *self.response.lock().unwrap() = (status, body);
    }
}

impl Drop for TokenServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(
    stream: &mut TcpStream,
    requests: &AtomicUsize,
    assertions: &Mutex<Vec<String>>,
    authorizations: &Mutex<Vec<String>>,
    response: &Mutex<(u16, String)>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() {
            return;
        }
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
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length.min(4096)];
    let _ = reader.read_exact(&mut body);
    authorizations.lock().unwrap().push(
        headers
            .get("authorization")
            .cloned()
            .unwrap_or_else(|| "<absent>".to_owned()),
    );
    let form = String::from_utf8_lossy(&body).into_owned();
    if let Some(assertion) = form.split('&').find_map(|pair| {
        pair.strip_prefix("assertion=")
            .map(|value| percent_decode(value))
    }) {
        assertions.lock().unwrap().push(assertion);
    }
    requests.fetch_add(1, Ordering::Relaxed);
    let (status, reply) = response.lock().unwrap().clone();
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'+' {
            out.push(b' ');
            index += 1;
            continue;
        }
        if byte == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(byte);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn auth_for(server: &TokenServer) -> ServiceAccountAuth {
    let key = ServiceAccountKey::from_json_bytes(
        key_json(RFC7515_KEY_PKCS1, &server.url()).as_bytes(),
    )
    .expect("key");
    ServiceAccountAuth::new(Arc::new(key), "https://www.googleapis.com/auth/devstorage.read_write")
}

#[test]
fn token_is_cached_and_refreshed_inside_a_bounded_window() {
    let server = TokenServer::start(
        200,
        serde_json::json!({
            "access_token": "ya29.test-token",
            "expires_in": 3600,
            "token_type": "Bearer",
        })
        .to_string(),
    );
    let auth = auth_for(&server);
    let t0 = 1_700_000_000_000_000_000i64;

    let first = auth.token_at(t0).expect("first token");
    assert_eq!(first.access_token(), "ya29.test-token");
    assert_eq!(first.expires_at_ns, t0 + 3_600_000_000_000);
    assert_eq!(server.requests(), 1);
    assert_eq!(auth.stats().exchanges, 1);
    // The assertion carried the issuer and the requested scope.
    let assertions = server.assertions();
    assert_eq!(assertions.len(), 1);
    let claims = decode(assertions[0].split('.').nth(1).unwrap());
    assert_eq!(
        claims.get("iss").and_then(|v| v.as_str()),
        Some("weft-store@weft-test.iam.gserviceaccount.com")
    );

    // Well inside the lifetime: no network request.
    let second = auth.token_at(t0 + 3_000_000_000_000).expect("cached token");
    assert_eq!(second.access_token(), "ya29.test-token");
    assert_eq!(server.requests(), 1);
    assert_eq!(auth.stats().cache_hits, 1);

    // Inside the 300 s refresh window: exactly one more exchange.
    let third = auth.token_at(t0 + 3_500_000_000_000).expect("refreshed token");
    assert_eq!(third.access_token(), "ya29.test-token");
    assert_eq!(server.requests(), 2);
    assert_eq!(auth.stats().exchanges, 2);
}

#[test]
fn a_short_lived_token_uses_half_its_lifetime_as_the_window() {
    let server = TokenServer::start(
        200,
        serde_json::json!({"access_token": "short", "expires_in": 100}).to_string(),
    );
    let auth = auth_for(&server);
    let t0 = 1_700_000_000_000_000_000i64;
    auth.token_at(t0).expect("first token");
    // 60 s left of 100: outside the 50 s window, so a cache hit.
    auth.token_at(t0 + 40_000_000_000).expect("cached");
    assert_eq!(server.requests(), 1);
    // 45 s left: inside the window, so one exchange.
    auth.token_at(t0 + 55_000_000_000).expect("refresh");
    assert_eq!(server.requests(), 2);
    // The refreshed token has a fresh 100 s lifetime; 1 s later is a hit.
    auth.token_at(t0 + 56_000_000_000).expect("cached again");
    assert_eq!(server.requests(), 2);
}

#[test]
fn provider_refusals_are_typed_and_do_not_echo_the_assertion() {
    let server = TokenServer::start(
        400,
        serde_json::json!({"error": "invalid_grant", "error_description": "Invalid JWT"}).to_string(),
    );
    let auth = auth_for(&server);
    let err = auth.token_at(1_700_000_000_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "Unauthorized", "{err}");
    assert!(err.message().contains("invalid_grant"), "{err}");
    assert!(!err.message().contains("eyJ"), "the assertion must not leak: {err}");
    assert_eq!(auth.stats().refusals, 1);

    // A malformed success reply is refused rather than accepted.
    server.set_response(200, "{\"not\":\"a token\"}".to_owned());
    let err = auth.token_at(1_700_000_001_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt", "{err}");

    server.set_response(
        200,
        serde_json::json!({"access_token": "x", "expires_in": 3600, "token_type": "mac"}).to_string(),
    );
    let err = auth.token_at(1_700_000_002_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");

    server.set_response(
        200,
        serde_json::json!({"access_token": "x", "expires_in": 0}).to_string(),
    );
    let err = auth.token_at(1_700_000_003_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "Corrupt", "{err}");
}

#[test]
fn debug_output_redacts_the_key_assertion_and_token() {
    let key = key_from(RFC7515_KEY_PKCS1);
    let key_debug = format!("{key:?}");
    assert!(key_debug.contains("<redacted>"));
    assert!(!key_debug.contains("MIIEow"), "{key_debug}");
    assert!(!key_debug.contains("PRIVATE KEY"), "{key_debug}");
    assert!(!key_debug.contains("private_exponent"), "{key_debug}");

    let server = TokenServer::start(
        200,
        serde_json::json!({"access_token": "ya29.secret-value", "expires_in": 3600}).to_string(),
    );
    let auth = auth_for(&server);
    let token = auth.token_at(1_700_000_000_000_000_000).expect("token");
    let token_debug = format!("{token:?}");
    assert!(!token_debug.contains("ya29.secret-value"), "{token_debug}");
    assert!(token_debug.contains("<redacted>"));
    let auth_debug = format!("{auth:?}");
    assert!(!auth_debug.contains("ya29.secret-value"), "{auth_debug}");
    // A token endpoint that cannot be reached yields a typed Io error.
    let dead = ServiceAccountAuth::new(
        Arc::new(
            ServiceAccountKey::from_json_bytes(
                key_json(RFC7515_KEY_PKCS1, "http://127.0.0.1:1/token").as_bytes(),
            )
            .unwrap(),
        ),
        "scope",
    );
    let err = dead.token_at(1_700_000_000_000_000_000).unwrap_err();
    assert_eq!(err.class_name(), "Io", "{err}");
}

// ---------------------------------------------------------------------------
// 5. The GCS backend uses the token source
// ---------------------------------------------------------------------------

#[test]
fn gcs_backend_sends_the_service_account_bearer_token() {
    use std::time::Duration;

    let token_server = TokenServer::start(
        200,
        serde_json::json!({"access_token": "ya29.gcs-token", "expires_in": 3600}).to_string(),
    );
    let gcs = TokenServer::start(200, "{\"kind\":\"storage#bucket\"}".to_owned());
    let key = Arc::new(
        ServiceAccountKey::from_json_bytes(
            key_json(RFC7515_KEY_PKCS1, &token_server.url()).as_bytes(),
        )
        .expect("key"),
    );
    let config = weftfs::gcs::GcsConfig {
        endpoint: format!("http://{}", gcs.addr),
        bucket: "weft-account-auth".to_owned(),
        token: None,
        service_account: Some(Arc::clone(&key)),
        scope: weftfs::service_account::DEFAULT_SCOPE.to_owned(),
        project: Some("weft-test".to_owned()),
        anonymous: false,
        lock_lease: Duration::from_secs(30),
        request_timeout: Duration::from_secs(10),
    };
    let backend = weftfs::gcs::GcsBackend::new("<gcs-auth-unit>", config);
    backend.ensure_bucket().expect("bucket metadata");
    assert_eq!(
        gcs.authorizations(),
        vec!["Bearer ya29.gcs-token".to_owned()],
        "the service-account token must be the Authorization value"
    );
    assert_eq!(token_server.requests(), 1);
    // A second call reuses the cached token.
    backend.ensure_bucket().expect("bucket metadata");
    assert_eq!(token_server.requests(), 1);
    assert_eq!(gcs.authorizations().len(), 2);
    assert!(weftfs::planes::BlobPlane::diagnostics(&backend)
        .iter()
        .any(|line| line.contains("service-account auth")));
}

#[test]
fn gcs_config_refuses_two_credential_forms_and_loads_one_key_file() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sa.json");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(key_json(RFC7515_KEY_PKCS1, DEFAULT_TOKEN_URI).as_bytes())
        .unwrap();
    drop(file);

    let err = weftfs::gcs::GcsConfig::resolve(
        Some("http://127.0.0.1:1".to_owned()),
        Some("weft-unit".to_owned()),
        Some("token".to_owned()),
        None,
        Some(false),
        Some(path.display().to_string()),
        None,
    )
    .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument", "{err}");
    assert!(err.message().contains("more than one credential form"), "{err}");

    let config = weftfs::gcs::GcsConfig::resolve(
        Some("http://127.0.0.1:1".to_owned()),
        Some("weft-unit".to_owned()),
        None,
        Some("weft-explicit".to_owned()),
        Some(false),
        Some(path.display().to_string()),
        Some("https://example.invalid/scope".to_owned()),
    )
    .expect("key file");
    assert!(config.token.is_none());
    assert_eq!(config.scope, "https://example.invalid/scope");
    let key = config.service_account.expect("key loaded");
    assert_eq!(key.modulus_bits(), 2048);
    // An explicit project wins over the one in the key file.
    assert_eq!(config.project.as_deref(), Some("weft-explicit"));

    // No credential at all is refused with the three accepted forms named.
    let err = weftfs::gcs::GcsConfig::resolve(
        Some("http://127.0.0.1:1".to_owned()),
        Some("weft-unit".to_owned()),
        None,
        None,
        Some(false),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err.class_name(), "InvalidArgument", "{err}");
    assert!(err.message().contains("GOOGLE_APPLICATION_CREDENTIALS"), "{err}");
}
