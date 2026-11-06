//! Helper for signed receipt bundles.
//!
//! The packaging and verification shell scripts call this binary through
//! `scripts/receipt-bundle-court.sh`, which runs it inside the hardened Rust
//! Docker court. It never reads a secret from the environment; a signing key is
//! a file path supplied explicitly.
//!
//! Subcommands:
//!   create  — build `receipt-manifest.wcb` from a staged bundle and sign it
//!   verify  — re-hash the bundle and verify the detached signature
//!   digest  — print the manifest digest and content root (provenance linkage)
//!   fingerprint — print the short public-key fingerprint

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::json;

use weftfs::identity::{self, Hash32};
use weftfs::receipt_bundle::{
    BundleManifest, MANIFEST_FILE, PUBLIC_KEY_FILE, SIGNATURE_FILE, UNSIGNED_FILE,
};

#[derive(Parser)]
#[command(
    name = "weft-receipt-bundle",
    version,
    about = "Signed receipt-bundle helper"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the canonical manifest from a staged bundle, optionally signing it.
    Create {
        /// Bundle staging directory.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Manifest output path (default: <root>/receipt-manifest.wcb).
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Ed25519 secret key: 32 raw bytes or 64 hex characters.
        #[arg(long = "sign-key")]
        sign_key: Option<PathBuf>,
        /// Signature output path (default: <root>/signature).
        #[arg(long)]
        signature: Option<PathBuf>,
        /// Public-key output path (default: <root>/public_key).
        #[arg(long)]
        public_key: Option<PathBuf>,
        /// Manifest `generated_at` field.
        #[arg(long = "generated-at")]
        generated_at: Option<String>,
        /// Manifest `stamp` field (default derived from generated_at).
        #[arg(long)]
        stamp: Option<String>,
    },
    /// Verify canonical manifest, every listed file, and the signature.
    Verify {
        /// Bundle root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Manifest path (default: <root>/receipt-manifest.wcb).
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Required signer public key, 64 hex characters.
        #[arg(long = "public-key")]
        public_key: Option<String>,
        /// Trust the bundled `public_key` file (self-contained verification).
        #[arg(long = "signature-from-bundle")]
        signature_from_bundle: bool,
        /// Skip file re-hashing (manifest/signature only).
        #[arg(long = "no-check-files")]
        no_check_files: bool,
    },
    /// Print the signed manifest digest and the store content root of its bytes.
    Digest {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Print the short fingerprint of a public key.
    Fingerprint {
        #[arg(long = "public-key")]
        public_key: String,
    },
    /// Derive the public key and fingerprint from a seed file.
    Derive {
        #[arg(long = "sign-key")]
        sign_key: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("FAIL: {}", err.message());
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> weftfs::Result<()> {
    match cli.command {
        Command::Create {
            root,
            manifest,
            sign_key,
            signature,
            public_key,
            generated_at,
            stamp,
        } => create(
            root,
            manifest,
            sign_key,
            signature,
            public_key,
            generated_at,
            stamp,
        ),
        Command::Verify {
            root,
            manifest,
            public_key,
            signature_from_bundle,
            no_check_files,
        } => verify(
            root,
            manifest,
            public_key,
            signature_from_bundle,
            no_check_files,
        ),
        Command::Digest { root, manifest } => digest(root, manifest),
        Command::Fingerprint { public_key } => {
            let key = weftfs::receipt_bundle::parse_public_key(&public_key)?;
            println!("{}", weftfs::receipt_bundle::key_fingerprint(&key));
            Ok(())
        }
        Command::Derive { sign_key } => {
            let seed = weftfs::receipt_bundle::read_seed(&sign_key)?;
            let public = weftcodec::capability::public_key_for_seed(&seed)?;
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "public_key": public.to_hex(),
                    "fingerprint": weftfs::receipt_bundle::key_fingerprint(&public),
                }))
                .unwrap_or_default()
            );
            Ok(())
        }
    }
}

fn create(
    root: PathBuf,
    manifest: Option<PathBuf>,
    sign_key: Option<PathBuf>,
    signature: Option<PathBuf>,
    public_key: Option<PathBuf>,
    generated_at: Option<String>,
    stamp: Option<String>,
) -> weftfs::Result<()> {
    let generated_at = generated_at.unwrap_or_else(now_rfc3339);
    let stamp = stamp.unwrap_or_else(|| generated_at.replace(['-', ':'], ""));
    let manifest_path = manifest.unwrap_or_else(|| root.join(MANIFEST_FILE));
    let signature_path = signature.unwrap_or_else(|| root.join(SIGNATURE_FILE));
    let public_key_path = public_key.unwrap_or_else(|| root.join(PUBLIC_KEY_FILE));

    let manifest = BundleManifest::from_directory(&root, generated_at, stamp)?;
    let bytes = manifest.encode()?;
    fs::write(&manifest_path, &bytes).map_err(|err| {
        weftfs::error::Error::io(format!("cannot write {}: {err}", manifest_path.display()))
    })?;
    let digest = identity::h(weftfs::receipt_bundle::SIGN_DOMAIN, &bytes);

    let mut signed = false;
    let mut fingerprint = String::new();
    let mut public_key_hex = String::new();
    let mut signature_hex = String::new();
    if let Some(key_path) = sign_key {
        let seed = weftfs::receipt_bundle::read_seed(&key_path)?;
        let signed_manifest = manifest.sign(&seed)?;
        write_blob(&signature_path, &signed_manifest.signature)?;
        write_blob(&public_key_path, signed_manifest.public_key.as_bytes())?;
        match fs::remove_file(root.join(UNSIGNED_FILE)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(weftfs::error::Error::io(format!(
                    "cannot remove {}: {err}",
                    root.join(UNSIGNED_FILE).display()
                )))
            }
        }
        signed = true;
        fingerprint = signed_manifest.fingerprint;
        public_key_hex = signed_manifest.public_key.to_hex();
        signature_hex = hex::encode(&signed_manifest.signature);
    }

    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema": weftfs::receipt_bundle::MANIFEST_SCHEMA,
            "manifest": manifest_path.display().to_string(),
            "manifest_sha256": digest.to_hex(),
            "manifest_content_root": identity::content_hash(&bytes).to_hex(),
            "files": manifest.files.len(),
            "revisions": manifest.revisions.len(),
            "signed": signed,
            "public_key": public_key_hex,
            "signature": signature_hex,
            "fingerprint": fingerprint,
        }))
        .unwrap_or_default()
    );
    Ok(())
}

fn verify(
    root: PathBuf,
    manifest: Option<PathBuf>,
    public_key: Option<String>,
    signature_from_bundle: bool,
    no_check_files: bool,
) -> weftfs::Result<()> {
    if public_key.is_some() && signature_from_bundle {
        return Err(weftfs::error::Error::invalid_argument(
            "--public-key and --signature-from-bundle are mutually exclusive",
        ));
    }
    let manifest_path = manifest.unwrap_or_else(|| root.join(MANIFEST_FILE));
    let bytes = fs::read(&manifest_path).map_err(|err| {
        weftfs::error::Error::io(format!("cannot read {}: {err}", manifest_path.display()))
    })?;
    let manifest = BundleManifest::decode(&bytes)?;
    let checked = if no_check_files {
        0
    } else {
        manifest.verify_files(&root)?
    };

    let signature_path = root.join(SIGNATURE_FILE);
    let public_key_path = root.join(PUBLIC_KEY_FILE);
    let require_key = public_key.is_some() || signature_from_bundle;
    if require_key {
        let signature_bytes = match fs::read(&signature_path) {
            Ok(bytes) => weftfs::receipt_bundle::decode_key_blob(
                &bytes,
                weftfs::receipt_bundle::SIGNATURE_LEN,
            )?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(weftfs::error::Error::corrupt(
                    "missing signature file; bundle is unsigned",
                ))
            }
            Err(err) => {
                return Err(weftfs::error::Error::io(format!(
                    "cannot read {}: {err}",
                    signature_path.display()
                )))
            }
        };
        let expected = match &public_key {
            Some(text) => weftfs::receipt_bundle::parse_public_key(text)?,
            None => read_public_key(&public_key_path)?,
        };
        if let Ok(bundled) = fs::read(&public_key_path) {
            if let Ok(raw) = weftfs::receipt_bundle::decode_key_blob(&bundled, 32) {
                let bundled_key = Hash32::from_bytes({
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&raw);
                    key
                });
                if bundled_key != expected {
                    return Err(weftfs::error::Error::corrupt(format!(
                        "public_key file does not match the requested key (bundle {}, requested {})",
                        weftfs::receipt_bundle::key_fingerprint(&bundled_key),
                        weftfs::receipt_bundle::key_fingerprint(&expected)
                    )));
                }
            }
        }
        manifest
            .verify_signature(&expected, &signature_bytes)
            .map_err(|err| {
                weftfs::error::Error::corrupt(format!(
                    "signature does not verify under {}: {}",
                    weftfs::receipt_bundle::key_fingerprint(&expected),
                    err.message()
                ))
            })?;
        println!(
            "SIGNATURE OK {}",
            weftfs::receipt_bundle::key_fingerprint(&expected)
        );
    } else if signature_path.is_file() {
        println!("signature: present (not verified; pass --public-key or --signature-from-bundle)");
    } else {
        println!("signature: absent (unsigned bundle)");
    }

    println!("manifest files: {}", manifest.files.len());
    println!("manifest revisions: {}", manifest.revisions.len());
    println!(
        "manifest sha256: {}",
        identity::h(weftfs::receipt_bundle::SIGN_DOMAIN, &bytes).to_hex()
    );
    if !no_check_files {
        println!("verified files: {checked}");
    }
    Ok(())
}

fn digest(root: PathBuf, manifest: Option<PathBuf>) -> weftfs::Result<()> {
    let manifest_path = manifest.unwrap_or_else(|| root.join(MANIFEST_FILE));
    let bytes = fs::read(&manifest_path).map_err(|err| {
        weftfs::error::Error::io(format!("cannot read {}: {err}", manifest_path.display()))
    })?;
    let manifest = BundleManifest::decode(&bytes)?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "manifest": manifest_path.display().to_string(),
            "manifest_sha256": identity::h(weftfs::receipt_bundle::SIGN_DOMAIN, &bytes).to_hex(),
            "manifest_content_root": identity::content_hash(&bytes).to_hex(),
            "manifest_bytes": bytes.len(),
            "files": manifest.files.len(),
            "revisions": manifest.revisions.len(),
            "generated_at": manifest.generated_at,
            "stamp": manifest.stamp,
        }))
        .unwrap_or_default()
    );
    Ok(())
}

fn write_blob(path: &std::path::Path, bytes: &[u8]) -> weftfs::Result<()> {
    fs::write(path, weftfs::receipt_bundle::encode_key_blob(bytes))
        .map_err(|err| weftfs::error::Error::io(format!("cannot write {}: {err}", path.display())))
}

fn read_public_key(path: &std::path::Path) -> weftfs::Result<Hash32> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(weftfs::error::Error::corrupt(
                "missing public_key file; cannot verify a self-contained bundle",
            ))
        }
        Err(err) => {
            return Err(weftfs::error::Error::io(format!(
                "cannot read {}: {err}",
                path.display()
            )))
        }
    };
    let raw = weftfs::receipt_bundle::decode_key_blob(&bytes, 32)?;
    Ok(Hash32::from_bytes({
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw);
        key
    }))
}

fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil_from_days.
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
