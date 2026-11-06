//! Live service-account token probe (subphase R21).
//!
//! Optional court step. It is run only when a real service-account JSON key is
//! available, and it performs exactly one real token exchange so the court can
//! state whether a live Google endpoint accepted the assertion. It prints the
//! key fingerprint, the modulus width, and the token's remaining lifetime —
//! never the assertion and never the token.
//!
//! Exit codes:
//!   0  exchange succeeded
//!   1  exchange failed (the typed class is printed)
//!   3  no credential was configured (`no_credentials`)
//!
//! Usage:
//!   WEFT_GCS_CREDENTIALS=/work/.docker-cache/gcs-sa/key.json \
//!     cargo run --release --example gcs_auth_probe

use std::time::{SystemTime, UNIX_EPOCH};

use weftfs::service_account::{ServiceAccountAuth, ServiceAccountKey, DEFAULT_SCOPE};

fn main() {
    let path = std::env::var("WEFT_GCS_CREDENTIALS")
        .or_else(|_| std::env::var("GOOGLE_APPLICATION_CREDENTIALS"))
        .ok()
        .filter(|value| !value.is_empty());
    let Some(path) = path else {
        println!("no_credentials");
        std::process::exit(3);
    };
    let scope = std::env::var("WEFT_GCS_SCOPE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SCOPE.to_owned());
    let key = match ServiceAccountKey::from_path(std::path::Path::new(&path)) {
        Ok(key) => key,
        Err(err) => {
            println!(
                "key_load_failed class={} message={}",
                err.class_name(),
                err.message()
            );
            std::process::exit(1);
        }
    };
    let fingerprint = key.fingerprint_hex();
    let bits = key.modulus_bits();
    let auth = ServiceAccountAuth::new(std::sync::Arc::new(key), scope);
    match auth.token() {
        Ok(token) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos() as i64)
                .unwrap_or(0);
            let remaining = (token.expires_at_ns - now).max(0) / 1_000_000_000;
            println!(
                "token_exchange_ok fingerprint={fingerprint} modulus_bits={bits} \
                 expires_in_secs={remaining} exchanges={} cache_hits={}",
                auth.stats().exchanges,
                auth.stats().cache_hits
            );
        }
        Err(err) => {
            println!(
                "token_exchange_failed class={} message={} fingerprint={fingerprint} \
                 modulus_bits={bits}",
                err.class_name(),
                err.message()
            );
            std::process::exit(1);
        }
    }
}
