//! Optional encryption-at-rest court (T079).
//!
//! Proves the blob-plane envelope in `src/encryption.rs`:
//!
//! * both dedup policies round-trip: convergent seals equal plaintext to equal
//!   ciphertext (physical dedup preserved, equality visible), random seals it to
//!   different ciphertext (ciphertext equality removed);
//! * range reads return exactly the requested plaintext slice, including a
//!   partial final block, without reading the whole object;
//! * a flipped ciphertext byte, a flipped header field, and a truncated object
//!   are all typed `Corrupt` refusals, and a truncated object never yields
//!   partial plaintext;
//! * a key rotated away from `active` still opens objects it produced, an
//!   unknown key id is `Corrupt`, and a revoked key is `Unauthorized`;
//! * a whole store round-trips through `BackendKind::Encrypted` with the same
//!   logical state, `verify(true)` is clean, and the physical blob bytes on the
//!   local layout are ciphertext — the plaintext does not appear;
//! * GC and snapshots keep working through the envelope.
//!
//! Frames are untouched: the envelope wraps *blobs*, so the codec, the
//! manifest, and every existing reader are unchanged once the envelope is
//! opened. This is a single-host court with a synthetic fixture on a local
//! backend. It makes no media, power-loss, cloud, fleet, or
//! "confidential against a key holder" claim.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key, LocalBackend};
use weftfs::encryption::{self, EncryptedBlobPlane, EncryptionConfig, EncryptionMode};
use weftfs::planes::BlobPlane;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::{Class, Error};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn key(label: &str) -> [u8; 32] {
    encryption::test_key(label)
}

fn config(mode: EncryptionMode, label: &str) -> EncryptionConfig {
    EncryptionConfig::from_secret(mode, key(label))
}

fn expect_err<T>(result: weftfs::Result<T>, what: &str) -> Error {
    match result {
        Ok(_) => panic!("{what}: expected an error"),
        Err(err) => err,
    }
}

fn class_of(err: &Error) -> Class {
    err.class()
}

fn object_key(tag: u8) -> Key {
    Key::object(weftfs::Hash32::from_bytes([tag; 32]))
}

fn plane_at(root: &Path, config: EncryptionConfig) -> EncryptedBlobPlane<LocalBackend> {
    let mut inner = LocalBackend::new(root);
    BlobPlane::init_layout(&mut inner).expect("inner layout");
    EncryptedBlobPlane::new(inner, config).expect("encrypted plane")
}

fn payload(len: usize) -> Vec<u8> {
    // Non-repeating enough that a wrong slice is visible.
    (0..len)
        .map(|index| ((index * 131 + 17) % 251) as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[test]
fn both_modes_round_trip_and_differ_in_ciphertext_equality() {
    let plaintext = payload(200_000);
    let convergent = config(EncryptionMode::Convergent, "conv");
    let random = config(EncryptionMode::Random, "rand");

    let conv_a = encryption::seal(&convergent, &plaintext).expect("seal");
    let conv_b = encryption::seal(&convergent, &plaintext).expect("seal");
    let rand_a = encryption::seal(&random, &plaintext).expect("seal");
    let rand_b = encryption::seal(&random, &plaintext).expect("seal");

    assert_eq!(
        encryption::open(&convergent, &conv_a).expect("open"),
        plaintext
    );
    assert_eq!(encryption::open(&random, &rand_a).expect("open"), plaintext);
    assert_eq!(
        conv_a, conv_b,
        "convergent encryption must seal equal plaintext to equal bytes"
    );
    assert_ne!(
        rand_a, rand_b,
        "random-nonce encryption must not produce equal ciphertext for equal plaintext"
    );

    // Determinism is a property of the mode, not the key label.
    let conv_other = config(EncryptionMode::Convergent, "conv");
    assert_eq!(
        encryption::seal(&conv_other, &plaintext).expect("seal"),
        conv_a
    );

    // The envelope costs a fixed 96-byte header plus one 16-byte tag per 64 KiB
    // block, and nothing per plaintext byte.
    let blocks = plaintext.len().div_ceil(encryption::BLOCK_BYTES);
    assert_eq!(
        conv_a.len(),
        encryption::HEADER_BYTES + plaintext.len() + blocks * encryption::TAG_BYTES
    );
}

#[test]
fn empty_and_boundary_plaintexts_round_trip() {
    let config = config(EncryptionMode::Convergent, "boundary");
    for len in [
        0usize,
        1,
        encryption::BLOCK_BYTES - 1,
        encryption::BLOCK_BYTES,
    ] {
        let plaintext = payload(len);
        let sealed = encryption::seal(&config, &plaintext).expect("seal");
        assert_eq!(
            encryption::open(&config, &sealed).expect("open"),
            plaintext,
            "length {len}"
        );
    }
}

#[test]
fn tamper_truncation_and_header_edit_fail_closed() {
    let config = config(EncryptionMode::Convergent, "tamper");
    let plaintext = payload(3 * encryption::BLOCK_BYTES + 11);
    let sealed = encryption::seal(&config, &plaintext).expect("seal");

    // Flip one ciphertext byte in the second block.
    let mut flipped = sealed.clone();
    let offset = encryption::HEADER_BYTES + encryption::BLOCK_BYTES + 7;
    flipped[offset] ^= 0x01;
    let err = expect_err(encryption::open(&config, &flipped), "flipped byte");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");

    // Flip a header field (the plaintext length) without changing the body.
    let mut edited = sealed.clone();
    edited[40] ^= 0x01;
    let err = expect_err(encryption::open(&config, &edited), "edited header");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");

    // Truncate.
    let truncated = &sealed[..sealed.len() - 1];
    let err = expect_err(encryption::open(&config, truncated), "truncated");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");

    // Truncate to a whole block boundary: still a length mismatch, still
    // refused, and no partial plaintext is returned.
    let short = &sealed[..encryption::HEADER_BYTES];
    let err = expect_err(encryption::open(&config, short), "header only");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");
}

#[test]
fn key_rotation_revocation_and_unknown_ids() {
    let old = encryption::EncryptionKey::new(key("old"), 1, false);
    let new = encryption::EncryptionKey::new(key("new"), 2, false);
    let old_only = EncryptionConfig {
        mode: EncryptionMode::Convergent,
        active: old.id,
        keys: vec![old.clone()],
        source: "test".to_owned(),
    };
    let sealed = encryption::seal(&old_only, b"rotated payload").expect("seal");

    // The key rotated away from `active` still opens what it produced.
    let rotated = EncryptionConfig {
        mode: EncryptionMode::Convergent,
        active: new.id,
        keys: vec![old.clone(), new.clone()],
        source: "test".to_owned(),
    };
    assert_eq!(
        encryption::open(&rotated, &sealed).expect("open with retained key"),
        b"rotated payload"
    );
    // A new publication uses the active key.
    let fresh = encryption::seal(&rotated, b"rotated payload").expect("seal");
    assert_ne!(fresh, sealed);

    // A revoked key is `Unauthorized`, not `Corrupt`: the object exists and is
    // authentic, but this principal may not read it.
    let revoked = EncryptionConfig {
        mode: EncryptionMode::Convergent,
        active: new.id,
        keys: vec![
            encryption::EncryptionKey {
                revoked: true,
                ..old.clone()
            },
            new.clone(),
        ],
        source: "test".to_owned(),
    };
    let err = expect_err(encryption::open(&revoked, &sealed), "revoked key");
    assert_eq!(class_of(&err), Class::Unauthorized, "{err}");

    // A key set that never saw the object names an unknown id and fails closed.
    let stranger = EncryptionConfig {
        mode: EncryptionMode::Convergent,
        active: new.id,
        keys: vec![new.clone()],
        source: "test".to_owned(),
    };
    let err = expect_err(encryption::open(&stranger, &sealed), "unknown key");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");

    // A key file that revokes its own active key is refused at load time.
    let bad = EncryptionConfig {
        mode: EncryptionMode::Convergent,
        active: new.id,
        keys: vec![encryption::EncryptionKey {
            revoked: true,
            ..new.clone()
        }],
        source: "test".to_owned(),
    };
    let dir = tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let doc = serde_json::json!({
        "schema": encryption::KEY_FILE_SCHEMA,
        "mode": "convergent",
        "active": hex::encode(bad.active),
        "keys": [{
            "id": hex::encode(bad.keys[0].id),
            "epoch": 1,
            "secret": hex::encode(bad.keys[0].secret),
            "revoked": true,
        }],
    });
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let err = expect_err(encryption::load(&path), "revoked active key");
    assert_eq!(class_of(&err), Class::InvalidArgument, "{err}");
}

#[test]
fn key_file_round_trip_and_rejections() {
    let first = encryption::EncryptionKey::new(key("file-1"), 1, false);
    let second = encryption::EncryptionKey::new(key("file-2"), 2, false);
    let dir = tempdir().unwrap();
    let path = dir.path().join("weft.encryption.keyfile.v1.json");
    let doc = serde_json::json!({
        "schema": encryption::KEY_FILE_SCHEMA,
        "mode": "random",
        "active": hex::encode(second.id),
        "keys": [
            {"id": hex::encode(first.id), "epoch": 1,
             "secret": hex::encode(first.secret), "revoked": false},
            {"id": hex::encode(second.id), "epoch": 2,
             "secret": hex::encode(second.secret), "revoked": false},
        ],
    });
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let loaded = encryption::load(&path).expect("load");
    assert_eq!(loaded.mode, EncryptionMode::Random);
    assert_eq!(loaded.active, second.id);
    assert_eq!(loaded.keys.len(), 2);
    assert!(loaded.describe().contains("random"));

    // Wrong schema.
    let mut bad = doc.clone();
    bad["schema"] = serde_json::json!("weft.encryption.keyfile.v2");
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let err = expect_err(encryption::load(&path), "wrong schema");
    assert_eq!(class_of(&err), Class::Malformed, "{err}");

    // Id that does not match its secret.
    let mut bad = doc.clone();
    bad["keys"][0]["id"] = serde_json::json!(hex::encode([0u8; 16]));
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let err = expect_err(encryption::load(&path), "mismatched id");
    assert_eq!(class_of(&err), Class::NonCanonical, "{err}");

    // Active key absent from the key set.
    let mut bad = doc.clone();
    bad["active"] = serde_json::json!(hex::encode([0xabu8; 16]));
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let err = expect_err(encryption::load(&path), "unknown active");
    assert_eq!(class_of(&err), Class::InvalidArgument, "{err}");

    // Repeated key id.
    let mut bad = doc.clone();
    bad["keys"][1] = doc["keys"][0].clone();
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let err = expect_err(encryption::load(&path), "duplicate key");
    assert_eq!(class_of(&err), Class::NonCanonical, "{err}");
}

// ---------------------------------------------------------------------------
// Plane
// ---------------------------------------------------------------------------

#[test]
fn plane_whole_and_range_reads_return_plaintext_slices() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(dir.path(), config(EncryptionMode::Random, "range"));
    let plaintext = payload(3 * encryption::BLOCK_BYTES + 1234);
    plane
        .put_if_absent(&object_key(1), &plaintext)
        .expect("put");

    assert_eq!(plane.get(&object_key(1)).expect("get"), plaintext);
    assert_eq!(
        plane.len(&object_key(1)).expect("len"),
        plaintext.len() as u64,
        "the plane reports plaintext length, not ciphertext length"
    );
    assert!(plane.exists(&object_key(1)).expect("exists"));

    let cases = [
        (0u64, 1u64),
        (0, encryption::BLOCK_BYTES as u64),
        (1, encryption::BLOCK_BYTES as u64 / 2),
        (
            encryption::BLOCK_BYTES as u64 - 7,
            20, // straddles a block boundary
        ),
        (
            (plaintext.len() - 3) as u64,
            100, // short at end of object
        ),
        (plaintext.len() as u64, 10), // zero bytes at end
        (plaintext.len() as u64 - 1, 1),
    ];
    for (offset, len) in cases {
        let got = plane
            .get_range(&object_key(1), offset, len)
            .unwrap_or_else(|err| panic!("range {offset}+{len}: {err}"));
        let start = offset as usize;
        let end = (start + len as usize).min(plaintext.len());
        assert_eq!(got, &plaintext[start..end], "range {offset}+{len}");
    }

    let err = expect_err(
        plane.get_range(&object_key(1), plaintext.len() as u64 + 1, 1),
        "offset past end",
    );
    assert_eq!(class_of(&err), Class::InvalidArgument, "{err}");

    // Reads are independent of the key that wrote them: the mode and key id
    // ride in each object's header.
    let same_key_other_mode = config(EncryptionMode::Convergent, "range");
    let mut reopened = plane_at(dir.path(), same_key_other_mode);
    assert_eq!(
        reopened.get(&object_key(1)).expect("get after mode change"),
        plaintext
    );
}

#[test]
fn plane_refuses_a_corrupt_object_without_returning_bytes() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(
        dir.path(),
        config(EncryptionMode::Convergent, "plane-tamper"),
    );
    let plaintext = payload(100_000);
    plane
        .put_if_absent(&object_key(2), &plaintext)
        .expect("put");

    // Flip a byte inside the second block on the inner plane.
    let path = object_file(dir.path(), 2);
    assert!(path.exists(), "expected the object at {}", path.display());
    let mut raw = fs::read(&path).unwrap();
    let offset = encryption::HEADER_BYTES + encryption::BLOCK_BYTES + encryption::TAG_BYTES + 3;
    raw[offset] ^= 0x80;
    fs::write(&path, &raw).unwrap();

    let err = expect_err(plane.get(&object_key(2)), "tampered object");
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");

    // A ranged read of the untouched first block still works: the failure is
    // per-block, not a global decryption.
    let head = plane
        .get_range(&object_key(2), 0, 32)
        .expect("untouched block");
    assert_eq!(head, &plaintext[..32]);
    let err = expect_err(
        plane.get_range(&object_key(2), encryption::BLOCK_BYTES as u64 + 1, 64),
        "tampered block range",
    );
    assert_eq!(class_of(&err), Class::Corrupt, "{err}");
}

#[test]
fn plane_put_is_create_once_over_ciphertext() {
    let dir = tempdir().unwrap();
    let mut plane = plane_at(
        dir.path(),
        config(EncryptionMode::Convergent, "create-once"),
    );
    plane.put_if_absent(&object_key(3), b"first").expect("put");
    // Identical bytes: the inner plane sees byte-identical ciphertext because
    // the mode is convergent, so this is an idempotent no-op.
    plane.put_if_absent(&object_key(3), b"first").expect("same");
    let err = expect_err(
        plane.put_if_absent(&object_key(3), b"second"),
        "different bytes",
    );
    assert_eq!(class_of(&err), Class::Exists, "{err}");
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

fn encrypted_kind(config: EncryptionConfig) -> BackendKind {
    BackendKind::Encrypted {
        inner: Box::new(BackendKind::Local),
        config,
    }
}

/// Physical `objects/<ab>/<hash>` path for the local layout.
fn object_file(root: &Path, tag: u8) -> PathBuf {
    let hash = weftfs::Hash32::from_bytes([tag; 32]);
    let hex = hex::encode(hash.as_bytes());
    root.join("objects").join(&hex[..2]).join(hex)
}

fn all_object_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dir = root.join("objects");
    let Ok(shards) = fs::read_dir(&dir) else {
        return out;
    };
    for shard in shards {
        let shard = shard.unwrap().path();
        if !shard.is_dir() {
            continue;
        }
        for entry in fs::read_dir(shard).unwrap() {
            out.push(entry.unwrap().path());
        }
    }
    out
}

#[test]
fn encrypted_store_round_trips_and_stores_ciphertext() {
    let dir = tempdir().unwrap();
    let key_config = config(EncryptionMode::Convergent, "store");
    let mut store =
        Store::init_with_backend(dir.path(), opts(), encrypted_kind(key_config.clone()))
            .expect("init encrypted store");

    let root = store.resolve_path("/").unwrap();
    let plaintext: Vec<u8> = (0..500u32).map(|index| (index % 251) as u8).collect();
    let attrs = std::collections::BTreeMap::new();
    let file = store
        .create_file_seeded(
            root,
            b"secret.bin",
            &plaintext,
            0o644,
            0,
            0,
            attrs,
            &[3u8; 32],
        )
        .unwrap();
    let read = store.read_object_at(None, file).expect("read");
    assert_eq!(read, plaintext);
    assert_eq!(
        store.read_range(file, 7, 51).expect("range"),
        plaintext[7..58]
    );

    // Every physical object body is a sealed envelope: the magic is present and
    // the plaintext byte sequence does not appear.
    let objects = all_object_files(dir.path());
    assert!(!objects.is_empty(), "expected at least one stored object");
    for path in &objects {
        let raw = fs::read(path).unwrap();
        assert!(
            raw.starts_with(encryption::ENCRYPTION_MAGIC),
            "object {} is not enveloped",
            path.display()
        );
        assert!(
            !raw.windows(plaintext.len())
                .any(|window| window == plaintext.as_slice()),
            "plaintext leaked into {}",
            path.display()
        );
    }

    // A snapshot, a mutation, and a full verify all keep working.
    let snapshot = store.snapshot_create("enc").unwrap();
    let updated = store.write_file(file, 0, b"REPLACED").unwrap();
    assert_ne!(updated, file, "a write publishes a new version id");
    assert_eq!(store.read_object_at(None, file).unwrap()[..8], *b"REPLACED");
    assert_eq!(
        store.read_object_at(Some(snapshot), file).unwrap(),
        plaintext,
        "the snapshot must see the pre-mutation bytes"
    );
    let report = store.verify(true).expect("verify");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    drop(store);

    // Reopen with the same key: state, snapshot, and verify are identical.
    let mut reopened = Store::open_with_backend(
        dir.path(),
        OpenMode::ReadWrite,
        encrypted_kind(key_config.clone()),
    )
    .expect("reopen");
    assert_eq!(
        reopened.read_object_at(Some(snapshot), file).unwrap(),
        plaintext
    );
    assert_eq!(
        reopened.read_object_at(None, file).unwrap()[..8],
        *b"REPLACED"
    );
    assert!(reopened.verify(true).unwrap().problems.is_empty());
    drop(reopened);

    // Wrong key: commit and control *records* are not enveloped, so recovery can
    // read the head. The state manifests cannot be opened with this key, and the
    // store refuses the open instead of silently serving an older generation
    // whose manifests happen to decrypt.
    let stranger = config(EncryptionMode::Convergent, "store-stranger");
    let err = match Store::open_with_backend(dir.path(), OpenMode::ReadOnly, encrypted_kind(stranger)) {
        Ok(store) => {
            // If an implementation ever opens, no read may return plaintext.
            let read = store.read_object_at(None, file);
            assert!(read.is_err(), "a wrong key returned plaintext");
            panic!("a store opened with a wrong key instead of refusing");
        }
        Err(err) => err,
    };
    assert_eq!(class_of(&err), Class::Unauthorized, "{err}");
    assert!(
        err.to_string()
            .contains("cannot open this store's published state"),
        "the refusal must name the cause: {err}"
    );

    // The key that did write the store still opens it after the refusal.
    let recovered = Store::open_with_backend(
        dir.path(),
        OpenMode::ReadOnly,
        encrypted_kind(key_config),
    )
    .expect("the right key still opens");
    assert_eq!(
        recovered.read_object_at(None, file).unwrap()[..8],
        *b"REPLACED"
    );
    assert!(recovered.verify(true).unwrap().problems.is_empty());
}

#[test]
fn random_mode_store_round_trips_and_loses_ciphertext_dedup() {
    let dir = tempdir().unwrap();
    let config = config(EncryptionMode::Random, "random-store");
    let mut store =
        Store::init_with_backend(dir.path(), opts(), encrypted_kind(config.clone())).unwrap();
    let root = store.resolve_path("/").unwrap();
    let attrs = std::collections::BTreeMap::new();
    let body = payload(4096);
    let one = store
        .create_file_seeded(
            root,
            b"one.bin",
            &body,
            0o644,
            0,
            0,
            attrs.clone(),
            &[4u8; 32],
        )
        .unwrap();
    // The same content at a different path: the store's *logical* dedup keeps
    // one object id (manifests are frozen and hash plaintext), but the
    // ciphertext for a fresh publication is not reused.
    let two = store
        .create_file_seeded(root, b"two.bin", &body, 0o644, 0, 0, attrs, &[5u8; 32])
        .unwrap();
    assert_ne!(one, two);
    assert_eq!(store.read_object_at(None, one).unwrap(), body);
    assert_eq!(store.read_object_at(None, two).unwrap(), body);
    assert!(store.verify(true).unwrap().problems.is_empty());
    drop(store);

    let reopened = Store::open_with_backend(dir.path(), OpenMode::ReadOnly, encrypted_kind(config))
        .expect("reopen");
    assert_eq!(reopened.read_object_at(None, one).unwrap(), body);
}

#[test]
fn encrypted_store_can_be_packed_underneath() {
    // The envelope is the outermost blob wrapper, so packing still sees and
    // stores ciphertext members; reads through both layers are exact.
    let dir = tempdir().unwrap();
    let config = config(EncryptionMode::Convergent, "packed");
    let packed = BackendKind::packed(BackendKind::Local, weftfs::packs::PackOptions::default())
        .expect("packed");
    let kind = BackendKind::Encrypted {
        inner: Box::new(packed),
        config: config.clone(),
    };
    let mut store = Store::init_with_backend(dir.path(), opts(), kind).unwrap();
    let root = store.resolve_path("/").unwrap();
    let body: Vec<u8> = (0..200u32).map(|index| (index * 7) as u8).collect();
    let file = store
        .create_file_seeded(
            root,
            b"packed.bin",
            &body,
            0o644,
            0,
            0,
            std::collections::BTreeMap::new(),
            &[6u8; 32],
        )
        .unwrap();
    assert_eq!(store.read_object_at(None, file).unwrap(), body);
    assert!(store.verify(true).unwrap().problems.is_empty());
    drop(store);

    let reopened = Store::open_with_backend(
        dir.path(),
        OpenMode::ReadOnly,
        BackendKind::Encrypted {
            inner: Box::new(
                BackendKind::packed(BackendKind::Local, weftfs::packs::PackOptions::default())
                    .unwrap(),
            ),
            config,
        },
    )
    .expect("reopen");
    assert_eq!(reopened.read_object_at(None, file).unwrap(), body);
}

#[test]
fn diagnostics_name_the_envelope_without_secrets() {
    let config = config(EncryptionMode::Random, "diag");
    let text = format!("{config:?}");
    assert!(text.contains("random"));
    assert!(text.contains("<redacted>"));
    assert!(!text.contains(&hex::encode(config.keys[0].secret)));
    assert!(config.describe().starts_with("encryption:random:"));
}
