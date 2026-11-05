//! Create (or confirm) the bucket used by the S3 court.
//!
//! Run inside the S3 court after MinIO is ready:
//! `cargo run --quiet --example s3_court_bucket`.
//!
//! Reads `WEFT_S3_ENDPOINT`, `WEFT_S3_BUCKET`, `WEFT_S3_REGION`,
//! `WEFT_S3_ACCESS_KEY`, and `WEFT_S3_SECRET_KEY` (or the AWS_* fallbacks).

use weftfs::s3::{S3Backend, S3Config};

fn main() {
    let config = S3Config::from_env().expect("S3 configuration");
    let backend = S3Backend::new("<s3-court>", config.clone());
    backend.ensure_bucket().expect("ensure bucket");
    eprintln!(
        "s3_court_bucket: bucket {} ready at {}",
        config.bucket, config.endpoint
    );
}
