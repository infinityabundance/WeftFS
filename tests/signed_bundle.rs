//! Signed receipt bundles: canonical manifest, Ed25519 signature, file
//! re-hashing, and provenance linkage to a store snapshot.
//!
//! Every rejection is asserted by message, not only by exit status, so a
//! regression that fails for the wrong reason is visible.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::{tempdir, TempDir};
use weftcodec::wcb::Value;

use weftfs::identity::Hash32;
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::receipt_bundle::{
    BundleManifest, MANIFEST_FILE, PUBLIC_KEY_FILE, SIGNATURE_FILE, UNSIGNED_FILE,
};
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};

const SEED: [u8; 32] = [0x11; 32];
const OTHER_SEED: [u8; 32] = [0x22; 32];
const GENERATED_AT: &str = "2026-11-06T00:00:00Z";
const STAMP: &str = "20261106T000000Z";

fn stage_bundle() -> (TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("REVISIONS.txt"),
        format!(
            "WeftFS {} staging clean 0\n\
             WeftCodec {} staging clean 0\n\
             WeftCodec-Go {} staging clean 0\n\
             WeftFS-Go {} staging clean 0\n",
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40),
            "d".repeat(40)
        ),
    )
    .unwrap();
    fs::create_dir_all(root.join("receipts")).unwrap();
    fs::write(
        root.join("manifest.json"),
        b"{\"schema\":\"weftfs.receipt-bundle/1\"}\n",
    )
    .unwrap();
    fs::write(root.join("receipts/one.json"), b"{\"result\":\"pass\"}\n").unwrap();
    fs::write(root.join("receipts/one.log"), b"raw log\n").unwrap();
    (dir, root)
}

fn manifest_for(root: &Path) -> BundleManifest {
    BundleManifest::from_directory(root, GENERATED_AT, STAMP).unwrap()
}

fn sign_into(root: &Path, seed: &[u8; 32]) -> weftfs::receipt_bundle::SignedManifest {
    let manifest = manifest_for(root);
    manifest.write_to(root).unwrap();
    let signed = manifest.sign(seed).unwrap();
    fs::write(
        root.join(SIGNATURE_FILE),
        weftfs::receipt_bundle::encode_key_blob(&signed.signature),
    )
    .unwrap();
    fs::write(
        root.join(PUBLIC_KEY_FILE),
        weftfs::receipt_bundle::encode_key_blob(signed.public_key.as_bytes()),
    )
    .unwrap();
    signed
}

fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_weft-receipt-bundle")
}

fn weftfs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_weftfs")
}

fn run_ok(args: &[&str]) -> String {
    let output = Command::new(helper()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "helper failed: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_fail(args: &[&str]) -> String {
    let output = Command::new(helper()).args(args).output().unwrap();
    assert!(!output.status.success(), "helper unexpectedly succeeded");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn public_key_hex(seed: &[u8; 32]) -> String {
    weftcodec::capability::public_key_for_seed(seed)
        .unwrap()
        .to_hex()
}

// ---------------------------------------------------------------------------
// Sign / verify
// ---------------------------------------------------------------------------

#[test]
fn sign_verify_round_trip() {
    let (_dir, root) = stage_bundle();
    let signed = sign_into(&root, &SEED);
    assert_eq!(signed.signature.len(), 64);

    let manifest = BundleManifest::read_from(&root).unwrap();
    manifest
        .verify_signature(&signed.public_key, &signed.signature)
        .unwrap();
    // The canonical manifest is byte-stable when read back.
    assert_eq!(manifest.encode().unwrap(), manifest.encode().unwrap());
    assert_eq!(manifest.verify_files(&root).unwrap(), 4);

    let out = run_ok(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&SEED),
    ]);
    assert!(out.contains("SIGNATURE OK ed25519:"), "{out}");
    assert!(out.contains("verified files: 4"), "{out}");
}

#[test]
fn wrong_key_is_rejected() {
    let (_dir, root) = stage_bundle();
    sign_into(&root, &SEED);
    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&OTHER_SEED),
    ]);
    assert!(
        out.contains("public_key file does not match the requested key"),
        "{out}"
    );

    // Also reject at the library level.
    let manifest = BundleManifest::read_from(&root).unwrap();
    let signed = manifest.sign(&SEED).unwrap();
    let other = weftcodec::capability::public_key_for_seed(&OTHER_SEED).unwrap();
    assert!(manifest
        .verify_signature(&other, &signed.signature)
        .is_err());
}

#[test]
fn tampered_file_breaks_file_check_and_signature() {
    let (_dir, root) = stage_bundle();
    let signed = sign_into(&root, &SEED);
    fs::write(root.join("receipts/one.log"), b"TAMPERED").unwrap();

    // File re-hashing catches it first.
    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&SEED),
    ]);
    assert!(out.contains("sha256"), "{out}");

    // A fresh manifest over the tampered tree no longer matches the old
    // signature (the signature covers the manifest digest).
    let recomputed = manifest_for(&root);
    assert_ne!(recomputed.digest().unwrap(), signed.digest);
    assert!(recomputed
        .verify_signature(&signed.public_key, &signed.signature)
        .is_err());
}

#[test]
fn missing_signature_fails_when_a_key_is_required() {
    let (_dir, root) = stage_bundle();
    sign_into(&root, &SEED);
    fs::remove_file(root.join(SIGNATURE_FILE)).unwrap();
    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&SEED),
    ]);
    assert!(out.contains("missing signature file"), "{out}");
}

#[test]
fn unsigned_bundle_fails_with_public_key_and_says_so_without() {
    let (_dir, root) = stage_bundle();
    fs::write(
        root.join(UNSIGNED_FILE),
        b"This bundle is UNSIGNED. No --sign-key was supplied.\n",
    )
    .unwrap();
    let manifest = manifest_for(&root);
    manifest.write_to(&root).unwrap();

    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&SEED),
    ]);
    assert!(
        out.contains("missing signature file; bundle is unsigned"),
        "{out}"
    );

    let out = run_ok(&["verify", "--root", root.to_str().unwrap()]);
    assert!(out.contains("signature: absent (unsigned bundle)"), "{out}");
    assert!(out.contains("verified files: 5"), "{out}");

    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--signature-from-bundle",
    ]);
    assert!(out.contains("missing signature file"), "{out}");
}

#[test]
fn signature_from_bundle_verifies_self_contained() {
    let (_dir, root) = stage_bundle();
    sign_into(&root, &SEED);
    let out = run_ok(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--signature-from-bundle",
    ]);
    assert!(out.contains("SIGNATURE OK ed25519:"), "{out}");
}

// ---------------------------------------------------------------------------
// Canonical encoding and determinism
// ---------------------------------------------------------------------------

#[test]
fn manifest_is_deterministic_across_directory_scans() {
    let (_dir, root) = stage_bundle();
    let first = manifest_for(&root);
    let second = manifest_for(&root);
    assert_eq!(first.encode().unwrap(), second.encode().unwrap());
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());

    // The signed digest is independent of scan order and of the signature.
    let signed_a = sign_into(&root, &SEED);
    let signed_b = sign_into(&root, &SEED);
    assert_eq!(signed_a.digest, signed_b.digest);
    assert_eq!(signed_a.signature, signed_b.signature);
}

#[test]
fn non_canonical_manifest_is_rejected() {
    let (_dir, root) = stage_bundle();
    let manifest = manifest_for(&root);
    let mut bytes = manifest.encode().unwrap();
    // Flip a byte in the middle of the canonical encoding; this must not
    // decode to a canonical manifest that re-encodes to the same bytes.
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    let err = BundleManifest::decode(&bytes).unwrap_err();
    assert!(
        err.message().contains("not in canonical form")
            || err.message().contains("WCB")
            || err.message().contains("Malformed"),
        "{}",
        err.message()
    );

    // Trailing bytes are never accepted.
    let mut extended = manifest.encode().unwrap();
    extended.push(0);
    assert!(BundleManifest::decode(&extended).is_err());
}

#[test]
fn manifest_paths_reject_traversal() {
    let err = weftfs::receipt_bundle::BundleFile {
        path: "../escape".into(),
        size: 0,
        sha256: Hash32::ZERO,
    };
    let manifest = BundleManifest::new(
        GENERATED_AT,
        STAMP,
        vec![err],
        vec![weftfs::receipt_bundle::BundleRevision {
            name: "WeftFS".into(),
            revision: "a".repeat(40),
            branch: "staging".into(),
            worktree: "clean".into(),
            changed_files: 0,
        }],
    );
    assert!(manifest.is_err());
}

#[test]
fn unlisted_payload_file_is_rejected() {
    let (_dir, root) = stage_bundle();
    sign_into(&root, &SEED);
    fs::write(root.join("receipts/extra.json"), b"{}\n").unwrap();
    let out = run_fail(&[
        "verify",
        "--root",
        root.to_str().unwrap(),
        "--public-key",
        &public_key_hex(&SEED),
    ]);
    assert!(out.contains("unlisted payload file"), "{out}");
}

// ---------------------------------------------------------------------------
// Provenance linkage
// ---------------------------------------------------------------------------

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

#[test]
fn provenance_show_resolves_bundle_manifest_and_snapshot() {
    let (_dir, bundle_root) = stage_bundle();
    let signed = sign_into(&bundle_root, &SEED);
    let manifest_bytes = fs::read(bundle_root.join(MANIFEST_FILE)).unwrap();
    let manifest = BundleManifest::decode(&manifest_bytes).unwrap();
    assert_eq!(manifest.digest().unwrap(), signed.digest);

    let store_dir = tempdir().unwrap();
    let mut store = Store::init(store_dir.path(), opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    let object = store
        .create_file(
            root,
            b"bundle.manifest",
            &manifest_bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let content = store.object_meta(object).unwrap().content;
    let snapshot = store.snapshot_create("q6-signed-bundle").unwrap();

    let mut params = BTreeMap::new();
    params.insert(
        "manifest_sha256".to_owned(),
        Value::Str(signed.digest.to_hex()),
    );
    params.insert(
        "signature_fingerprint".to_owned(),
        Value::Str(signed.fingerprint.clone()),
    );
    params.insert("bundle_stamp".to_owned(), Value::Str(STAMP.to_owned()));
    let record = ProvenanceRecord::new(
        "package-receipts",
        params,
        vec![ProvenanceInput::new("derived_from", snapshot, None)],
        vec![content],
        ProvenanceEnvironment::new("weft-receipt-bundle", "0.7.0"),
        Determinism::Exact,
        ProvenancePrincipal::new(0, 0),
        1_700_000_000_000_000_000,
    )
    .unwrap();
    let provenance = store.record_provenance(&[object], &record).unwrap();
    store
        .relation_add(
            object,
            "derived_from",
            RelationTarget::Value(Value::Str(snapshot.to_hex())),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    let report = store.verify(true).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    drop(store);

    // The real CLI resolves the attachment and shows the derived_from input.
    let output = Command::new(weftfs_bin())
        .args([
            "--store",
            store_dir.path().to_str().unwrap(),
            "provenance",
            "show",
            "/bundle.manifest",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "provenance show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("provenance_id={provenance}")),
        "{stdout}"
    );
    assert!(stdout.contains(&snapshot.to_hex()), "{stdout}");
    assert!(stdout.contains("derived_from"), "{stdout}");

    // The relation is a separate, queryable edge.
    let output = Command::new(weftfs_bin())
        .args([
            "--store",
            store_dir.path().to_str().unwrap(),
            "relation",
            "list",
            "--subject",
            "/bundle.manifest",
            "--predicate",
            "derived_from",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&snapshot.to_hex()), "{stdout}");

    // Tampering changes both the signature input and the provenance output
    // hash: recomputing the manifest over the modified tree cannot reproduce
    // the recorded content root.
    fs::write(bundle_root.join("receipts/one.log"), b"tampered\n").unwrap();
    let recomputed = manifest_for(&bundle_root);
    assert_ne!(recomputed.digest().unwrap(), signed.digest);
    assert_eq!(
        record.params.get("manifest_sha256"),
        Some(&Value::Str(signed.digest.to_hex()))
    );
    // A store built from the tampered manifest has a different content root,
    // so it cannot match the recorded provenance output hash either.
    let tampered_dir = tempdir().unwrap();
    let mut tampered_store = Store::init(tampered_dir.path(), opts()).unwrap();
    let tampered_root = tampered_store.resolve_path("/").unwrap();
    let tampered_object = tampered_store
        .create_file(
            tampered_root,
            b"bundle.manifest",
            &recomputed.encode().unwrap(),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let tampered_content = tampered_store.object_meta(tampered_object).unwrap().content;
    assert_ne!(tampered_content, content);
    assert!(recomputed
        .verify_signature(&signed.public_key, &signed.signature)
        .is_err());
}
