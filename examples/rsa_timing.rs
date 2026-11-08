//! RS256 signing timing smoke test.
//!
//! Measures `ServiceAccountKey::sign_compact` over two message classes chosen
//! to exercise different operand patterns in the secret-key exponentiations,
//! and reports the medians and a Welch t-statistic.
//!
//! **This cannot prove constant-time execution.** It is a smoke test in the
//! `dudect` tradition: a large |t| tells you something is visibly data-
//! dependent; a small |t| does not prove the absence of a leak, does not cover
//! every microarchitecture, and does not audit the compiler's code generation.
//! A formal side-channel audit is still required.
//!
//! Usage (inside the court, release build):
//!   cargo run --release --example rsa_timing -- [--samples 20000] [--out FILE]

use std::fs;
use std::time::Instant;

use serde_json::json;
use sha2::{Digest, Sha256};

use weftfs::service_account::ServiceAccountKey;

/// RFC 7515 Appendix A.2 RSA key (PKCS#1). Test key only; it protects nothing.
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

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn key() -> ServiceAccountKey {
    let json = json!({
        "type": "service_account",
        "project_id": "weft-timing",
        "private_key_id": "kid-timing",
        "private_key": RFC7515_KEY_PKCS1,
        "client_email": "timing@weft-timing.iam.gserviceaccount.com",
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string();
    ServiceAccountKey::from_json_bytes(json.as_bytes()).expect("fixture key parses")
}

/// Find a nonce whose SHA-256 starts with the requested two bytes. The digest
/// is the low end of the EMSA-PKCS1-v1_5 representative, so the two classes
/// differ in a known part of the operand while every high limb is identical.
fn message_with_digest_prefix(prefix: [u8; 2], class: &str) -> Vec<u8> {
    for nonce in 0u64.. {
        let mut message = format!("weft/rsa-timing/{class}/").into_bytes();
        message.extend_from_slice(&nonce.to_le_bytes());
        let digest = Sha256::digest(&message);
        if digest[0] == prefix[0] && digest[1] == prefix[1] {
            return message;
        }
    }
    unreachable!()
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn variance(values: &[f64]) -> f64 {
    let mean = mean(values);
    let sum: f64 = values
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum();
    sum / (values.len() as f64 - 1.0)
}

fn welch_t(a: &[f64], b: &[f64]) -> f64 {
    let denominator = (variance(a) / a.len() as f64 + variance(b) / b.len() as f64).sqrt();
    if denominator == 0.0 {
        return 0.0;
    }
    (mean(a) - mean(b)) / denominator
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let samples: usize = arg(&args, "--samples")
        .unwrap_or("20000")
        .parse()
        .expect("--samples");
    let key = key();
    let header = b"eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9";
    // Class A: the digest's first two bytes are 0x0000 (sparse low-end limbs).
    // Class B: the digest's first two bytes are 0xffff (dense low-end limbs).
    let message_a = message_with_digest_prefix([0x00, 0x00], "a");
    let message_b = message_with_digest_prefix([0xff, 0xff], "b");

    // Warm up (page faults, code paths) before any sample is recorded.
    for _ in 0..50 {
        key.sign_compact(header, &message_a).expect("warmup");
        key.sign_compact(header, &message_b).expect("warmup");
    }

    let repeats: usize = arg(&args, "--repeats")
        .unwrap_or("3")
        .parse()
        .expect("--repeats");
    // Randomized interleaving: a fixed A-then-B order would let any slow drift
    // (frequency, cache state) masquerade as a class difference.
    let mut order = 0x9e37_79b9_7f4a_7c15u64;
    let mut rounds = Vec::new();
    for round in 0..repeats {
        let mut a = Vec::with_capacity(samples);
        let mut b = Vec::with_capacity(samples);
        let started = Instant::now();
        for _ in 0..samples {
            order ^= order >> 12;
            order ^= order << 25;
            order ^= order >> 27;
            let a_first = order.wrapping_mul(0x2545_f491_4f6c_dd1d) & 1 == 0;
            if a_first {
                let start = Instant::now();
                key.sign_compact(header, &message_a).expect("sign a");
                a.push(start.elapsed().as_nanos() as f64);
                let start = Instant::now();
                key.sign_compact(header, &message_b).expect("sign b");
                b.push(start.elapsed().as_nanos() as f64);
            } else {
                let start = Instant::now();
                key.sign_compact(header, &message_b).expect("sign b");
                b.push(start.elapsed().as_nanos() as f64);
                let start = Instant::now();
                key.sign_compact(header, &message_a).expect("sign a");
                a.push(start.elapsed().as_nanos() as f64);
            }
        }
        rounds.push(json!({
            "round": round,
            "median_a_ns": median(&a),
            "median_b_ns": median(&b),
            "mean_a_ns": mean(&a),
            "mean_b_ns": mean(&b),
            "variance_a_ns2": variance(&a),
            "variance_b_ns2": variance(&b),
            "welch_t": welch_t(&a, &b),
            "elapsed_s": started.elapsed().as_secs_f64(),
        }));
    }
    let t_values: Vec<f64> = rounds
        .iter()
        .map(|round| round["welch_t"].as_f64().unwrap_or(0.0))
        .collect();
    let worst_t = t_values
        .iter()
        .fold(0.0f64, |worst, value| worst.max(value.abs()));

    let result = json!({
        "schema": "weft-rsa-timing-v1",
        "purpose": "dudect-style timing smoke test; NOT a constant-time proof",
        "samples_per_class": samples,
        "interleaving": "randomized per pair (xorshift64*), so slow drift cannot mimic a class difference",
        "sign_operations": samples * 2,
        "class_a": {
            "description": "SHA-256 digest prefix 0000",
            "message_len": message_a.len(),
        },
        "class_b": {
            "description": "SHA-256 digest prefix ffff",
            "message_len": message_b.len(),
        },
        "rounds": rounds,
        "worst_abs_welch_t": worst_t,
        "threshold": {
            "value": 4.5,
            "meaning": "the conventional dudect cutoff for this sample size; |t| above it is a visible data dependence, below it is not a proof",
        },
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
            "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
        },
        "non_claims": [
            "A small |t| is a smoke-test result, not constant-time proof.",
            "One microarchitecture, one compiler version, one host.",
            "The test does not model cache/branch-predictor interference, frequency scaling, or a co-located attacker.",
            "A formal side-channel audit of the emitted instructions and memory access pattern is still required.",
        ],
    });
    let text = serde_json::to_string_pretty(&result).unwrap();
    println!("{text}");
    if let Some(path) = arg(&args, "--out") {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, &text).expect("write receipt");
    }
}
