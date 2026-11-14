//! Producer attestation harness (subphase T074).
//!
//! A small, auditable driver over `weftfs::producer_auth`. It writes canonical
//! WCB key sets and attestations so the Go and Python verifiers can consume
//! exactly the bytes Rust produced.
//!
//! ```text
//! producer_attest keygen  --key KEYFILE [--seed-hex HEX] [--json]
//! producer_attest keyset  --spec SPEC.json --out KEYSET.wcb [--json]
//! producer_attest sign    --key KEYFILE --keyset KEYSET.wcb
//!                         --producer P --run-id R --tool T --tool-version V
//!                         --determinism exact|declared|unknown
//!                         --signing-epoch N --input role=HEX [--input ...]
//!                         --output HEX [--snapshot HEX] [--issued-at-ns N]
//!                         --out ATTESTATION.wcb [--json]
//! producer_attest verify  --attestation FILE --keyset FILE
//!                         [--mode current|historical] [--store DIR]
//!                         [--snapshot HEX] [--json]
//! ```

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value as Json;
use weftfs::identity::Hash32;
use weftfs::producer_auth::{
    public_key_for_seed, verify_attestation, verify_attestation_against_store, Determinism,
    ProducerAttestation, ProducerInput, ProducerKey, ProducerKeySet, VerifyMode,
};
use weftfs::store::OpenMode;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an Ed25519 seed file and print its public key.
    Keygen {
        #[arg(long)]
        key: PathBuf,
        #[arg(long = "seed-hex")]
        seed_hex: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Build a canonical key set from a JSON spec.
    Keyset {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Sign a producer attestation.
    Sign {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        keyset: PathBuf,
        #[arg(long)]
        producer: String,
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long)]
        tool: String,
        #[arg(long = "tool-version")]
        tool_version: String,
        #[arg(long, default_value = "unknown")]
        determinism: String,
        #[arg(long = "signing-epoch")]
        signing_epoch: u64,
        #[arg(long = "input", value_name = "ROLE=HEX")]
        inputs: Vec<String>,
        #[arg(long)]
        output: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long = "issued-at-ns")]
        issued_at_ns: Option<i64>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Verify an attestation (optionally against the store's actual content).
    Verify {
        #[arg(long)]
        attestation: PathBuf,
        #[arg(long)]
        keyset: PathBuf,
        #[arg(long, default_value = "current")]
        mode: String,
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn hex32(text: &str, what: &str) -> Result<Hash32, String> {
    Hash32::from_hex(text).map_err(|_| format!("{what} is not 64 hex digits"))
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Keygen {
            key,
            seed_hex,
            json,
        } => {
            let seed: [u8; 32] = match seed_hex {
                Some(text) => {
                    let bytes =
                        hex::decode(text.trim()).map_err(|_| "seed is not hex".to_owned())?;
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| "seed must be 32 bytes".to_owned())?
                }
                None => {
                    let mut seed = [0u8; 32];
                    for (index, byte) in seed.iter_mut().enumerate() {
                        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
                    }
                    // Deterministic only when the caller supplied no seed is not
                    // acceptable for production, so refuse instead.
                    if std::env::var("WEFT_ALLOW_FIXED_TEST_SEED").as_deref() != Ok("1") {
                        return Err(
                            "no --seed-hex supplied; refusing to invent key material".to_owned()
                        );
                    }
                    seed
                }
            };
            if let Some(parent) = key.parent() {
                fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            fs::write(&key, seed).map_err(|err| err.to_string())?;
            let signing = public_key_for_seed(&seed);
            let public = signing;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "public_key": public.to_hex(),
                                       "key_file": key.display().to_string()})
                );
            } else {
                println!("{}", public);
            }
            Ok(())
        }
        Command::Keyset { spec, out, json } => {
            let spec: Json = serde_json::from_slice(&fs::read(&spec).map_err(|e| e.to_string())?)
                .map_err(|err| format!("spec is not JSON: {err}"))?;
            let epoch = spec
                .get("epoch")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| "spec needs u64 `epoch`".to_owned())?;
            let rotated_from = match spec.get("rotated_from").and_then(|v| v.as_str()) {
                Some(text) => Some(hex32(text, "rotated_from")?),
                None => None,
            };
            let rows = spec
                .get("keys")
                .and_then(|value| value.as_array())
                .ok_or_else(|| "spec needs a `keys` array".to_owned())?;
            let mut keys = Vec::new();
            for row in rows {
                let label = row
                    .get("label")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "key needs `label`".to_owned())?;
                let public = hex32(
                    row.get("public_key")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "key needs `public_key`".to_owned())?,
                    "public_key",
                )?;
                let number = |name: &str| -> Result<Option<u64>, String> {
                    match row.get(name) {
                        None | Some(Json::Null) => Ok(None),
                        Some(value) => value
                            .as_u64()
                            .map(Some)
                            .ok_or_else(|| format!("`{name}` is not u64")),
                    }
                };
                keys.push(ProducerKey {
                    key: public,
                    label: label.to_owned(),
                    valid_from_epoch: number("valid_from_epoch")?.unwrap_or(0),
                    valid_until_epoch: number("valid_until_epoch")?,
                    revoked_at_epoch: number("revoked_at_epoch")?,
                    rotated_at_epoch: number("rotated_at_epoch")?,
                });
            }
            let set =
                ProducerKeySet::new(epoch, rotated_from, keys).map_err(|err| err.to_string())?;
            let bytes = set.canonical_bytes().map_err(|err| err.to_string())?;
            fs::write(&out, &bytes).map_err(|err| err.to_string())?;
            let root = set.root().map_err(|err| err.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "root": root.to_hex(), "epoch": set.epoch,
                                       "keys": set.keys.len(), "out": out.display().to_string()})
                );
            } else {
                println!("{}", root);
            }
            Ok(())
        }
        Command::Sign {
            key,
            keyset,
            producer,
            run_id,
            tool,
            tool_version,
            determinism,
            signing_epoch,
            inputs,
            output,
            snapshot,
            issued_at_ns,
            out,
            json,
        } => {
            let seed: [u8; 32] = fs::read(&key)
                .map_err(|err| format!("cannot read key: {err}"))?
                .as_slice()
                .try_into()
                .map_err(|_| "key file must be exactly 32 bytes".to_owned())?;
            let set_bytes = fs::read(&keyset).map_err(|err| err.to_string())?;
            let set =
                ProducerKeySet::from_canonical_bytes(&set_bytes).map_err(|err| err.to_string())?;
            let root = set.root().map_err(|err| err.to_string())?;
            let mut parsed_inputs = Vec::new();
            for entry in &inputs {
                let (role, hex) = entry
                    .split_once('=')
                    .ok_or_else(|| format!("--input `{entry}` needs ROLE=HEX"))?;
                parsed_inputs.push(ProducerInput {
                    role: role.to_owned(),
                    content_root: hex32(hex, "input root")?,
                });
            }
            let mut attestation = ProducerAttestation::new(
                producer,
                run_id,
                tool,
                tool_version,
                Determinism::parse(&determinism).map_err(|err| err.to_string())?,
                signing_epoch,
                root,
                parsed_inputs,
                hex32(&output, "output")?,
                match snapshot {
                    Some(text) => Some(hex32(&text, "snapshot")?),
                    None => None,
                },
                issued_at_ns.unwrap_or(0),
            )
            .map_err(|err| err.to_string())?;
            let signer = attestation.sign(&seed).map_err(|err| err.to_string())?;
            let bytes = attestation
                .canonical_bytes()
                .map_err(|err| err.to_string())?;
            fs::write(&out, &bytes).map_err(|err| err.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "signer": signer.to_hex(),
                        "attestation_id": attestation.id().map_err(|e| e.to_string())?.to_hex(),
                        "key_set_root": root.to_hex(),
                        "bytes": bytes.len(),
                        "out": out.display().to_string(),
                    })
                );
            } else {
                println!("{}", signer);
            }
            Ok(())
        }
        Command::Verify {
            attestation,
            keyset,
            mode,
            store,
            snapshot,
            json,
        } => {
            let bytes = fs::read(&attestation).map_err(|err| err.to_string())?;
            let set = ProducerKeySet::from_canonical_bytes(
                &fs::read(&keyset).map_err(|err| err.to_string())?,
            )
            .map_err(|err| err.to_string())?;
            let mode = match mode.as_str() {
                "current" => VerifyMode::Current,
                "historical" => VerifyMode::Historical,
                other => return Err(format!("mode `{other}` is not current|historical")),
            };
            let result = match store {
                Some(path) => {
                    let store = weftfs::store::Store::open_with_options(
                        &path,
                        OpenMode::ReadOnly,
                        weftfs::store::Options::default(),
                    )
                    .map_err(|err| err.to_string())?;
                    let snapshot = match snapshot {
                        Some(text) => Some(hex32(&text, "snapshot")?),
                        None => None,
                    };
                    verify_attestation_against_store(&store, snapshot, &bytes, &set, mode)
                        .map(|checked| checked.verified)
                        .map_err(|err| format!("{}: {}", err.code(), err))
                }
                None => verify_attestation(&bytes, &set, mode, None)
                    .map_err(|err| format!("{}: {}", err.code(), err)),
            };
            let verified = match result {
                Ok(verified) => verified,
                Err(message) => return Err(message),
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "attestation_id": verified.attestation_id.to_hex(),
                        "signer": verified.signer.to_hex(),
                        "key_set_root": verified.key_set_root.to_hex(),
                        "key_set_epoch": verified.key_set_epoch,
                        "signing_epoch": verified.signing_epoch,
                        "key_status": verified.key_status.as_str(),
                        "valid_at_signing": verified.valid_at_signing,
                        "currently_trusted": verified.currently_trusted,
                        "producer": verified.producer,
                        "run_id": verified.run_id,
                        "tool": verified.tool,
                        "tool_version": verified.tool_version,
                        "determinism": verified.determinism.as_str(),
                        "output": verified.output.to_hex(),
                    })
                );
            } else {
                println!("signer={}", verified.signer);
                println!("key_status={}", verified.key_status.as_str());
            }
            Ok(())
        }
    }
}
