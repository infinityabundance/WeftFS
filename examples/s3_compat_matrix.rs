//! S3-compatible implementation compatibility matrix (subphase R16).
//!
//! "S3-compatible" is a claim, not a specification. This harness exercises the
//! exact behaviours the WeftFS store depends on, against whatever endpoint the
//! environment names, and records the observed result per behaviour instead of
//! assuming the AWS S3 reference behaviour:
//!
//! 1. `if-none-match-create-once` — `PUT` with `If-None-Match: *` creates once
//!    and rejects the second create.
//! 2. `if-match-conditional-overwrite` — a matching `If-Match` ETag is
//!    accepted and a stale one is rejected (`412`).
//! 3. `range-open-ended` — `GET` with `Range: bytes=N-` returns `206` with the
//!    exact tail and a correct `Content-Range`.
//! 4. `range-out-of-range` — a range past the end returns `416`.
//! 5. `head-missing-key` — `HEAD` on an absent key returns `404`.
//! 6. `delete-idempotent` — deleting an absent key still succeeds (S3
//!    semantics).
//! 7. `list-pagination` — `ListObjectsV2` with `max-keys=2` paginates with
//!    `NextContinuationToken` and returns every key exactly once.
//! 8. `stale-cas-probe` — two writers holding the same head ETag: the first
//!    conditional write wins and the second is refused, leaving the winner's
//!    bytes readable. This is the store's head-publication primitive.
//!
//! Usage (inside a Docker court, never on the host):
//!
//! ```text
//! WEFT_S3_ENDPOINT=... WEFT_S3_BUCKET=... WEFT_S3_ACCESS_KEY=... \
//! WEFT_S3_SECRET_KEY=... WEFT_S3_TEST_PREFIX=... \
//! WEFT_S3_SERVER_LABEL="Ceph RGW 19.2.0" WEFT_COMPAT_STAMP=2026-11-06T120000Z \
//!   cargo run --quiet --example s3_compat_matrix -- --out bench/receipts/s3-compat.json
//! ```
//!
//! The harness only issues signed HTTP requests; it never writes store state.
//! A single-host endpoint proves protocol behaviour, not durability.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Map, Value};

use weftfs::s3::{RawResponse, S3Backend, S3Config};

const SCHEMA: &str = "weft-s3-compat-matrix-v1";

fn usage() -> &'static str {
    "usage: s3_compat_matrix [--out <receipt.json>] [--label <name>]\n\
     s3_compat_matrix --compare <a.json> <b.json> [--out <merged.json>]\n\
     Reads WEFT_S3_ENDPOINT, WEFT_S3_BUCKET, WEFT_S3_REGION, WEFT_S3_ACCESS_KEY,\n\
     WEFT_S3_SECRET_KEY, WEFT_S3_TEST_PREFIX, WEFT_S3_SERVER_LABEL, and\n\
     WEFT_COMPAT_STAMP."
}

fn compare_receipts(paths: &[String], out: Option<&str>) -> Result<(), String> {
    let mut implementations: Vec<Value> = Vec::new();
    let mut per_impl: Vec<Vec<(String, bool, String)>> = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|err| format!("read {path}: {err}"))?;
        let value: Value = serde_json::from_str(&text).map_err(|err| format!("{path}: {err}"))?;
        let name = value
            .get("implementation")
            .and_then(Value::as_str)
            .unwrap_or("unlabelled")
            .to_owned();
        implementations.push(json!({
            "name": name,
            "endpoint": value.get("endpoint").cloned().unwrap_or(Value::Null),
            "bucket": value.get("bucket").cloned().unwrap_or(Value::Null),
            "summary": value.get("summary").cloned().unwrap_or(Value::Null),
        }));
        let mut rows = Vec::new();
        for check in value
            .get("checks")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{path}: no checks array"))?
        {
            rows.push((
                check
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unnamed")
                    .to_owned(),
                check
                    .get("supported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                check
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            ));
        }
        per_impl.push(rows);
    }

    let mut names: Vec<String> = Vec::new();
    for rows in &per_impl {
        for (name, _, _) in rows {
            if !names.iter().any(|existing| existing == name) {
                names.push(name.clone());
            }
        }
    }

    let header: Vec<String> = implementations
        .iter()
        .map(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unlabelled")
                .to_owned()
        })
        .collect();
    println!(
        "{:<34} {:<12} {:<12} notes",
        "behaviour", header[0], header[1]
    );
    let mut merged_rows = Vec::new();
    for name in &names {
        let mut results = Vec::new();
        let mut column = vec![String::new(); 2];
        for (index, rows) in per_impl.iter().enumerate() {
            match rows.iter().find(|(row_name, _, _)| row_name == name) {
                Some((_, supported, summary)) => {
                    column[index] = if *supported {
                        "supported".to_owned()
                    } else {
                        "NOT OK".to_owned()
                    };
                    results.push(json!({
                        "implementation": header[index],
                        "supported": supported,
                        "summary": summary,
                    }));
                }
                None => {
                    column[index] = "-".to_owned();
                    results.push(Value::Null);
                }
            }
        }
        let note = per_impl
            .iter()
            .zip(&header)
            .filter_map(|(rows, implementation)| {
                rows.iter()
                    .find(|(row_name, _, _)| row_name == name)
                    .filter(|(_, supported, _)| !*supported)
                    .map(|(_, _, summary)| format!("{implementation}: {summary}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        println!("{:<34} {:<12} {:<12} {}", name, column[0], column[1], note);
        merged_rows.push(json!({
            "name": name,
            "results": results,
        }));
    }

    let receipt = json!({
        "schema": "weft-s3-compat-matrix-v1",
        "generated_utc": std::env::var("WEFT_COMPAT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "mode": "comparison",
        "implementations": implementations,
        "rows": merged_rows,
        "label": "Two independent S3 implementations measured with the same harness, \
                  same checks, same client. Single-host synthetic objects; protocol \
                  behaviour only, not a durability or production claim.",
    });
    if let Some(path) = out {
        let text = serde_json::to_string_pretty(&receipt).map_err(|err| err.to_string())?;
        std::fs::write(path, format!("{text}\n")).map_err(|err| format!("write {path}: {err}"))?;
        eprintln!("s3_compat_matrix: receipt {path}");
    }
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("s3_compat_matrix: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut out: Option<String> = None;
    let mut label: Option<String> = None;
    let mut compare: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(args.next().ok_or("--out needs a path")?),
            "--label" => label = Some(args.next().ok_or("--label needs a value")?),
            "--compare" => {
                compare.push(args.next().ok_or("--compare needs a receipt")?);
                compare.push(args.next().ok_or("--compare needs two receipts")?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if !compare.is_empty() {
        return compare_receipts(&compare, out.as_deref());
    }
    let label = label
        .or_else(|| std::env::var("WEFT_S3_SERVER_LABEL").ok())
        .unwrap_or_else(|| "unlabelled endpoint".to_owned());
    let stamp = std::env::var("WEFT_COMPAT_STAMP").unwrap_or_else(|_| "unstamped".to_owned());

    let mut config = S3Config::from_env().map_err(|err| err.message().to_owned())?;
    config.path_style = true;
    config.request_timeout = Duration::from_secs(60);
    let endpoint = config.endpoint.clone();
    let bucket = config.bucket.clone();
    let backend = S3Backend::new("<s3-compat>", config);
    backend
        .ensure_bucket()
        .map_err(|err| format!("ensure bucket: {}", err.message()))?;

    let base = std::env::var("WEFT_S3_TEST_PREFIX").unwrap_or_else(|_| "weft-compat".to_owned());
    let base = format!("{base}/run-{}-{}", std::process::id(), unique());
    let court = Court { backend, base };

    let mut checks: Vec<Check> = Vec::new();
    checks.push(court.if_none_match_create_once());
    checks.push(court.if_match_conditional_overwrite());
    checks.push(court.range_open_ended());
    checks.push(court.range_out_of_range());
    checks.push(court.head_missing_key());
    checks.push(court.delete_idempotent());
    checks.push(court.list_pagination());
    checks.push(court.stale_cas_probe());

    let supported = checks.iter().filter(|check| check.supported).count();
    let mut receipt = Map::new();
    receipt.insert("schema".to_owned(), json!(SCHEMA));
    receipt.insert("generated_utc".to_owned(), json!(stamp));
    receipt.insert("implementation".to_owned(), json!(label));
    receipt.insert("endpoint".to_owned(), json!(endpoint));
    receipt.insert("bucket".to_owned(), json!(bucket));
    receipt.insert("run_prefix".to_owned(), json!(court.base));
    receipt.insert(
        "summary".to_owned(),
        json!({
            "total": checks.len(),
            "supported": supported,
            "unsupported": checks.len() - supported,
        }),
    );
    receipt.insert(
        "checks".to_owned(),
        Value::Array(checks.iter().map(Check::to_json).collect()),
    );
    receipt.insert(
        "label".to_owned(),
        json!(
            "Single-host S3-compatible endpoint on localhost, synthetic objects. \
             Protocol behaviour only; not a durability, media, power-loss, cloud, \
             fleet, or production claim."
        ),
    );

    println!(
        "{:<34} {:<10} {}",
        "behaviour",
        label_for_column(&label),
        "observed"
    );
    for check in &checks {
        println!(
            "{:<34} {:<10} {}",
            check.name,
            if check.supported {
                "supported"
            } else {
                "NOT OK"
            },
            check.summary
        );
    }
    println!(
        "\n{}/{} behaviours as the store requires them ({})",
        supported,
        checks.len(),
        label
    );

    if let Some(path) = out {
        let text =
            serde_json::to_string_pretty(&Value::Object(receipt)).map_err(|err| err.to_string())?;
        std::fs::write(&path, format!("{text}\n")).map_err(|err| format!("write {path}: {err}"))?;
        eprintln!("s3_compat_matrix: receipt {path}");
    }
    Ok(())
}

fn label_for_column(label: &str) -> &str {
    label
}

/// One observed behaviour.
struct Check {
    name: &'static str,
    supported: bool,
    summary: String,
    detail: Value,
}

impl Check {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "supported": self.supported,
            "summary": self.summary,
            "detail": self.detail,
        })
    }
}

struct Court {
    backend: S3Backend,
    base: String,
}

impl Court {
    fn key(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.base)
    }

    fn put(&self, path: &str, body: &[u8], extra: &[(&str, &str)]) -> Result<RawResponse, String> {
        self.backend
            .raw_request_for_court("PUT", path, &[], body, extra)
            .map_err(|err| err.message().to_owned())
    }

    fn get(&self, path: &str, extra: &[(&str, &str)]) -> Result<RawResponse, String> {
        self.backend
            .raw_request_for_court("GET", path, &[], &[], extra)
            .map_err(|err| err.message().to_owned())
    }

    fn head(&self, path: &str) -> Result<RawResponse, String> {
        self.backend
            .raw_request_for_court("HEAD", path, &[], &[], &[])
            .map_err(|err| err.message().to_owned())
    }

    fn delete(&self, path: &str) -> Result<RawResponse, String> {
        self.backend
            .raw_request_for_court("DELETE", path, &[], &[], &[])
            .map_err(|err| err.message().to_owned())
    }

    fn list(&self, query: &[(String, String)]) -> Result<RawResponse, String> {
        self.backend
            .raw_request_for_court("GET", "", query, &[], &[])
            .map_err(|err| err.message().to_owned())
    }

    fn create_once(&self, path: &str, body: &[u8]) -> Result<RawResponse, String> {
        self.put(path, body, &[("if-none-match", "*")])
    }

    fn response_detail(&self, response: &RawResponse) -> Value {
        json!({
            "status": response.status,
            "error_code": response.error_code(),
            "error_message": response.error_message(),
            "etag": response.etag,
            "content_range": response.content_range,
        })
    }

    /// 1. `If-None-Match: *` must create once and reject the second create.
    fn if_none_match_create_once(&self) -> Check {
        let name = "if-none-match-create-once";
        let path = self.key("create-once");
        let first = self.create_once(&path, b"first");
        let second = self.create_once(&path, b"second");
        let read = self.get(&path, &[]);
        match (first, second, read) {
            (Ok(first), Ok(second), Ok(read)) => {
                let first_ok = (200..300).contains(&first.status);
                let second_rejected = second.status == 412;
                let bytes_kept = read.status == 200 && read.body == b"first";
                let supported = first_ok && second_rejected && bytes_kept;
                Check {
                    name,
                    supported,
                    summary: format!(
                        "create={} repeat={} bytes_kept={bytes_kept}",
                        first.status, second.status
                    ),
                    detail: json!({
                        "first": self.response_detail(&first),
                        "second": self.response_detail(&second),
                        "first_writer_bytes_readable": bytes_kept,
                    }),
                }
            }
            _ => Check {
                name,
                supported: false,
                summary: "request error".to_owned(),
                detail: json!({"error": "request failed"}),
            },
        }
    }

    /// 2. A matching `If-Match` is accepted; a stale one is rejected.
    ///
    /// The detail records whether the server accepts the ETag exactly as it
    /// returned it, and (only when that fails) the de-quoted form. That
    /// distinction is the difference between "the store can publish" and "the
    /// store must refuse this endpoint".
    fn if_match_conditional_overwrite(&self) -> Check {
        let name = "if-match-conditional-overwrite";
        let path = self.key("if-match");
        let Some(created) = self.create_once(&path, b"v0").ok() else {
            return Check {
                name,
                supported: false,
                summary: "create failed".to_owned(),
                detail: json!({"error": "create failed"}),
            };
        };
        if !(200..300).contains(&created.status) {
            return Check {
                name,
                supported: false,
                summary: format!("create status {}", created.status),
                detail: json!({"create": self.response_detail(&created)}),
            };
        }
        let head = match self.head(&path) {
            Ok(response) if response.status == 200 => response,
            Ok(response) => {
                return Check {
                    name,
                    supported: false,
                    summary: format!("HEAD status {}", response.status),
                    detail: json!({"head": self.response_detail(&response)}),
                }
            }
            Err(err) => {
                return Check {
                    name,
                    supported: false,
                    summary: err,
                    detail: json!({"error": "HEAD failed"}),
                }
            }
        };
        let Some(etag) = head.etag.clone() else {
            return Check {
                name,
                supported: false,
                summary: "no ETag on HEAD".to_owned(),
                detail: json!({"head": self.response_detail(&head)}),
            };
        };
        let bare = etag.trim_matches('"').to_owned();
        let quoted_form = self.put(&path, b"v1", &[("if-match", etag.as_str())]);
        let quoted_form = match quoted_form {
            Ok(response) => response,
            Err(err) => {
                return Check {
                    name,
                    supported: false,
                    summary: err,
                    detail: json!({"error": "conditional PUT failed"}),
                }
            }
        };
        let mut detail = Map::new();
        detail.insert("etag_as_returned".to_owned(), json!(etag));
        detail.insert(
            "matching_if_match_as_returned".to_owned(),
            self.response_detail(&quoted_form),
        );
        let mut used_etag = etag.clone();
        if (200..300).contains(&quoted_form.status) {
            detail.insert("matching_accepted".to_owned(), json!(true));
        } else if bare != etag {
            // Diagnostic only: does the de-quoted form work?
            match self.put(&path, b"v1", &[("if-match", bare.as_str())]) {
                Ok(response) => {
                    detail.insert(
                        "matching_if_match_dequoted".to_owned(),
                        self.response_detail(&response),
                    );
                    detail.insert(
                        "dequoted_accepted".to_owned(),
                        json!((200..300).contains(&response.status)),
                    );
                    if (200..300).contains(&response.status) {
                        used_etag = bare.clone();
                    }
                }
                Err(err) => {
                    detail.insert("dequoted_error".to_owned(), json!(err));
                }
            }
        } else {
            detail.insert("etag_has_no_quotes".to_owned(), json!(true));
        }

        let matching_ok = (200..300).contains(&quoted_form.status)
            || detail
                .get("dequoted_accepted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if !matching_ok {
            detail.insert("matching_accepted".to_owned(), json!(false));
            let observed = detail
                .get("matching_if_match_as_returned")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            return Check {
                name,
                supported: false,
                summary: format!("matching If-Match rejected (status {observed})"),
                detail: Value::Object(detail),
            };
        }

        // `used_etag` is now stale (the successful conditional write replaced
        // it). A second writer holding it must be refused and the winner's
        // bytes must stay readable.
        let stale = self.put(&path, b"v2", &[("if-match", used_etag.as_str())]);
        let read = self.get(&path, &[]);
        let (stale_status, stale_code) = match &stale {
            Ok(response) => (response.status, response.error_code()),
            Err(_) => (0, None),
        };
        detail.insert(
            "stale_if_match".to_owned(),
            match &stale {
                Ok(response) => self.response_detail(response),
                Err(err) => json!({"error": err}),
            },
        );
        let bytes_kept = match read {
            Ok(response) => {
                detail.insert(
                    "read_after_stale".to_owned(),
                    self.response_detail(&response),
                );
                response.status == 200 && response.body == b"v1"
            }
            Err(err) => {
                detail.insert("read_after_stale_error".to_owned(), json!(err));
                false
            }
        };
        detail.insert("winner_bytes_kept".to_owned(), json!(bytes_kept));
        let supported = stale_status == 412 && bytes_kept;
        let summary = if matching_ok && !matches!(quoted_form.status, 200..=299) {
            format!(
                "accepted only after de-quoting; stale={stale_status}{}",
                stale_code
                    .as_deref()
                    .map(|code| format!(" {code}"))
                    .unwrap_or_default()
            )
        } else {
            format!(
                "matching accepted; stale={stale_status}{}",
                stale_code
                    .as_deref()
                    .map(|code| format!(" {code}"))
                    .unwrap_or_default()
            )
        };
        Check {
            name,
            supported,
            summary,
            detail: Value::Object(detail),
        }
    }

    /// 3. Open-ended range read.
    fn range_open_ended(&self) -> Check {
        let name = "range-open-ended";
        let path = self.key("range-open");
        let body = b"0123456789abcdef";
        let created = self.create_once(&path, body);
        let response = self.get(&path, &[("range", "bytes=4-")]);
        match (created, response) {
            (Ok(created), Ok(response))
                if (200..300).contains(&created.status) && response.status == 206 =>
            {
                let expected = "456789abcdef";
                let exact = response.body == expected.as_bytes();
                let content_range_ok = response
                    .content_range
                    .as_deref()
                    .map(|value| value == "bytes 4-15/16")
                    .unwrap_or(false);
                Check {
                    name,
                    supported: exact && content_range_ok,
                    summary: format!(
                        "status={} bytes={} content-range={}",
                        response.status,
                        response.body.len(),
                        response.content_range.as_deref().unwrap_or("<none>")
                    ),
                    detail: json!({
                        "body_matches": exact,
                        "content_range_ok": content_range_ok,
                        "response": self.response_detail(&response),
                    }),
                }
            }
            (Ok(created), Ok(response)) => Check {
                name,
                supported: false,
                summary: format!("create={} range={}", created.status, response.status),
                detail: json!({
                    "create": self.response_detail(&created),
                    "range": self.response_detail(&response),
                }),
            },
            _ => Check {
                name,
                supported: false,
                summary: "request error".to_owned(),
                detail: json!({"error": "request failed"}),
            },
        }
    }

    /// 4. Out-of-range read must be `416` (and at-EOF too).
    fn range_out_of_range(&self) -> Check {
        let name = "range-out-of-range";
        let path = self.key("range-past-end");
        let body = b"0123456789abcdef";
        let _ = self.create_once(&path, body);
        let past_end = self.get(&path, &[("range", "bytes=999-")]);
        let at_eof = self.get(&path, &[("range", "bytes=16-")]);
        match (past_end, at_eof) {
            (Ok(past_end), Ok(at_eof)) => {
                let past_ok = past_end.status == 416;
                let eof_ok = at_eof.status == 416;
                Check {
                    name,
                    supported: past_ok && eof_ok,
                    summary: format!(
                        "past_end={}{} at_eof={}{}",
                        past_end.status,
                        past_end
                            .error_code()
                            .map(|code| format!("/{code}"))
                            .unwrap_or_default(),
                        at_eof.status,
                        at_eof
                            .error_code()
                            .map(|code| format!("/{code}"))
                            .unwrap_or_default()
                    ),
                    detail: json!({
                        "past_end": self.response_detail(&past_end),
                        "at_eof": self.response_detail(&at_eof),
                    }),
                }
            }
            _ => Check {
                name,
                supported: false,
                summary: "request error".to_owned(),
                detail: json!({"error": "request failed"}),
            },
        }
    }

    /// 5. `HEAD` on a missing key.
    fn head_missing_key(&self) -> Check {
        let name = "head-missing-key";
        let path = self.key("definitely-absent");
        match self.head(&path) {
            Ok(response) => Check {
                name,
                supported: response.status == 404,
                summary: format!("status={}", response.status),
                detail: json!({"response": self.response_detail(&response)}),
            },
            Err(err) => Check {
                name,
                supported: false,
                summary: err,
                detail: json!({"error": "HEAD failed"}),
            },
        }
    }

    /// 6. Delete must be idempotent.
    fn delete_idempotent(&self) -> Check {
        let name = "delete-idempotent";
        let path = self.key("delete-me");
        let created = self.create_once(&path, b"payload");
        let first = self.delete(&path);
        let second = self.delete(&path);
        match (created, first, second) {
            (Ok(created), Ok(first), Ok(second)) => {
                let created_ok = (200..300).contains(&created.status);
                let first_ok = (200..300).contains(&first.status);
                let second_ok = (200..300).contains(&second.status);
                Check {
                    name,
                    supported: created_ok && first_ok && second_ok,
                    summary: format!(
                        "delete={} delete_again={}{}",
                        first.status,
                        second.status,
                        second
                            .error_code()
                            .map(|code| format!("/{code}"))
                            .unwrap_or_default()
                    ),
                    detail: json!({
                        "create": self.response_detail(&created),
                        "first_delete": self.response_detail(&first),
                        "second_delete": self.response_detail(&second),
                    }),
                }
            }
            _ => Check {
                name,
                supported: false,
                summary: "request error".to_owned(),
                detail: json!({"error": "request failed"}),
            },
        }
    }

    /// 7. `ListObjectsV2` pagination.
    fn list_pagination(&self) -> Check {
        let name = "list-pagination";
        let keys: Vec<String> = (0..5)
            .map(|index| self.key(&format!("list/{index:02}")))
            .collect();
        for key in &keys {
            match self.create_once(key, b"x") {
                Ok(response) if (200..300).contains(&response.status) => {}
                Ok(response) => {
                    return Check {
                        name,
                        supported: false,
                        summary: format!("create {key} status {}", response.status),
                        detail: json!({"create": self.response_detail(&response)}),
                    }
                }
                Err(err) => {
                    return Check {
                        name,
                        supported: false,
                        summary: err,
                        detail: json!({"error": "create failed"}),
                    }
                }
            }
        }
        let prefix = self.key("list/");
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut pages = Vec::new();
        let mut token: Option<String> = None;
        let mut duplicates = 0usize;
        for _ in 0..10 {
            let mut query: Vec<(String, String)> = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("prefix".to_owned(), prefix.clone()),
                ("max-keys".to_owned(), "2".to_owned()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let response = match self.list(&query) {
                Ok(response) => response,
                Err(err) => {
                    return Check {
                        name,
                        supported: false,
                        summary: err,
                        detail: json!({"error": "list failed"}),
                    }
                }
            };
            let text = String::from_utf8_lossy(&response.body).into_owned();
            let page_keys = xml_values(&text, "Key");
            for key in &page_keys {
                if !seen.insert(key.clone()) {
                    duplicates += 1;
                }
            }
            let truncated = xml_values(&text, "IsTruncated")
                .first()
                .map(|value| value == "true")
                .unwrap_or(false);
            let next = xml_values(&text, "NextContinuationToken")
                .into_iter()
                .next();
            pages.push(json!({
                "status": response.status,
                "keys": page_keys,
                "is_truncated": truncated,
                "next_token_present": next.is_some(),
            }));
            if !truncated {
                break;
            }
            match next {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        let complete = keys.iter().all(|key| seen.contains(key)) && seen.len() == keys.len();
        let supported = complete && duplicates == 0 && pages.len() >= 2;
        Check {
            name,
            supported,
            summary: format!(
                "pages={} keys={} duplicates={} complete={complete}",
                pages.len(),
                seen.len(),
                duplicates
            ),
            detail: json!({
                "pages": pages,
                "expected_keys": keys,
                "duplicates": duplicates,
                "complete": complete,
            }),
        }
    }

    /// 8. Stale compare-and-swap probe on an 8192-byte superblock-shaped
    /// object: the first conditional write wins, the second is refused, and
    /// the winner's bytes survive.
    ///
    /// This is the store's head-publication primitive, so it measures what the
    /// store can actually do: the as-returned ETag form is tried first, and if
    /// the server rejects a *matching* value the de-quoted form is tried (the
    /// same negotiation `S3Backend` performs at writable open). The form that
    /// worked is recorded, and the stale test uses that same form.
    fn stale_cas_probe(&self) -> Check {
        let name = "stale-cas-probe";
        let path = self.key("superblock-shaped");
        let original = vec![0x11u8; 8192];
        let created = self.create_once(&path, &original);
        match created {
            Ok(response) if (200..300).contains(&response.status) => {}
            Ok(response) => {
                return Check {
                    name,
                    supported: false,
                    summary: format!("create status {}", response.status),
                    detail: json!({"create": self.response_detail(&response)}),
                }
            }
            Err(err) => {
                return Check {
                    name,
                    supported: false,
                    summary: err,
                    detail: json!({"error": "create failed"}),
                }
            }
        }
        let head = match self.head(&path) {
            Ok(response) if response.status == 200 => response,
            Ok(response) => {
                return Check {
                    name,
                    supported: false,
                    summary: format!("HEAD status {}", response.status),
                    detail: json!({"head": self.response_detail(&response)}),
                }
            }
            Err(err) => {
                return Check {
                    name,
                    supported: false,
                    summary: err,
                    detail: json!({"error": "HEAD failed"}),
                }
            }
        };
        let Some(head_etag) = head.etag.clone() else {
            return Check {
                name,
                supported: false,
                summary: "no ETag on HEAD".to_owned(),
                detail: json!({"head": self.response_detail(&head)}),
            };
        };
        let bare = head_etag.trim_matches('"').to_owned();
        let mut winner = original.clone();
        winner[4095] = 0x22;
        let mut loser = original.clone();
        loser[4094] = 0x33;

        let mut detail = Map::new();
        detail.insert("head_etag".to_owned(), json!(head_etag));
        let as_returned = self.put(&path, &winner, &[("if-match", head_etag.as_str())]);
        let (form, winner_response) = match as_returned {
            Ok(response) if (200..300).contains(&response.status) => {
                ("as-returned".to_owned(), Ok(response))
            }
            other => {
                detail.insert(
                    "winner_as_returned".to_owned(),
                    match &other {
                        Ok(response) => self.response_detail(response),
                        Err(err) => json!({"error": err}),
                    },
                );
                if bare == head_etag {
                    ("as-returned".to_owned(), other)
                } else {
                    detail.insert("winner_dequoted_form".to_owned(), json!(true));
                    (
                        "dequoted".to_owned(),
                        self.put(&path, &winner, &[("if-match", bare.as_str())]),
                    )
                }
            }
        };
        let winner_ok =
            matches!(&winner_response, Ok(response) if (200..300).contains(&response.status));
        detail.insert(
            "winner".to_owned(),
            match &winner_response {
                Ok(response) => self.response_detail(response),
                Err(err) => json!({"error": err}),
            },
        );
        if !winner_ok {
            return Check {
                name,
                supported: false,
                summary: "no If-Match form was accepted for a matching ETag".to_owned(),
                detail: Value::Object(detail),
            };
        }

        let stale_value = if form == "dequoted" {
            &bare
        } else {
            &head_etag
        };
        let loser_response = self.put(&path, &loser, &[("if-match", stale_value.as_str())]);
        let loser_status = match &loser_response {
            Ok(response) => response.status,
            Err(_) => 0,
        };
        detail.insert(
            "loser".to_owned(),
            match &loser_response {
                Ok(response) => self.response_detail(response),
                Err(err) => json!({"error": err}),
            },
        );
        let read = self.get(&path, &[]);
        let bytes_kept = match &read {
            Ok(response) => response.status == 200 && response.body == winner,
            Err(_) => false,
        };
        detail.insert("winner_bytes_kept".to_owned(), json!(bytes_kept));
        detail.insert("if_match_form".to_owned(), json!(form));
        let supported = loser_status == 412 && bytes_kept;
        Check {
            name,
            supported,
            summary: format!(
                "winner=200 (form={form}) loser={loser_status} winner_bytes_kept={bytes_kept}"
            ),
            detail: Value::Object(detail),
        }
    }
}

/// Collect text between `<tag>` and `</tag>` for every occurrence.
fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(xml_unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    out
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn unique() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.subsec_nanos() as u64 ^ value.as_secs())
        .unwrap_or(0)
}
