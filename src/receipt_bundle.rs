//! Signed receipt-bundle manifests (`weft.receipt-bundle.manifest.v1`).
//!
//! A receipt bundle is a tar archive produced by
//! `scripts/package-receipts.sh`. This module adds a canonical manifest of the
//! bundle's *payload* files (path, size, SHA-256, sorted by canonical encoded
//! bytes) plus the git revisions the harness ran at, and optionally an Ed25519
//! signature over
//!
//! ```text
//! sign_input = H("weft/receipt-bundle/v1", WCB(manifest))
//! ```
//!
//! The signature, public key, canonical manifest, and `CHECKSUMS.sha256` are
//! bundle metadata and are deliberately **not** listed in the manifest: the
//! signature cannot cover itself, and the manifest cannot list its own hash.
//!
//! Signing uses `weftcodec::capability::{sign_bytes, verify_bytes}`, the same
//! Ed25519 construction as capability envelopes. There is no second crypto
//! implementation.
//!
//! This proves bundle integrity relative to a key. It does **not** prove that a
//! measurement is true, who owns the key, or that the key was ever distributed
//! or rotated.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};

/// Canonical schema string of a receipt-bundle manifest.
pub const MANIFEST_SCHEMA: &str = "weft.receipt-bundle.manifest.v1";

/// Domain separation for the value that is signed.
pub const SIGN_DOMAIN: &str = "weft/receipt-bundle/v1";

/// Domain separation for a short public-key fingerprint.
pub const KEY_FINGERPRINT_DOMAIN: &str = "weft/receipt-bundle/key/v1";

/// Canonical manifest file name inside the bundle.
pub const MANIFEST_FILE: &str = "receipt-manifest.wcb";

/// Detached signature file name inside the bundle (lowercase hex + newline).
pub const SIGNATURE_FILE: &str = "signature";

/// Public key file name inside the bundle (lowercase hex + newline).
pub const PUBLIC_KEY_FILE: &str = "public_key";

/// Marker written when a bundle carries no signature.
pub const UNSIGNED_FILE: &str = "UNSIGNED.txt";

/// Bundle metadata files that are intentionally not listed in the manifest.
pub const RESERVED_ROOT_FILES: [&str; 4] = [
    MANIFEST_FILE,
    SIGNATURE_FILE,
    PUBLIC_KEY_FILE,
    "CHECKSUMS.sha256",
];

/// Ed25519 signature length.
pub const SIGNATURE_LEN: usize = 64;

/// Hard bound on payload files in one manifest.
pub const MAX_BUNDLE_FILES: usize = 200_000;

fn bundle_limits() -> Limits {
    Limits {
        max_structured_items: (MAX_BUNDLE_FILES as u64) * 8,
        max_structured_depth: 8,
        max_string_bytes: 64 * 1024,
        ..Limits::default()
    }
}

fn encode_value(value: &Value) -> Result<Vec<u8>> {
    wcb::encode(value, &bundle_limits()).map_err(Error::from)
}

fn decode_value(bytes: &[u8]) -> Result<Value> {
    wcb::decode(bytes, &bundle_limits()).map_err(Error::from)
}

/// One payload file listed in a bundle manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub size: u64,
    pub sha256: Hash32,
}

/// One repository revision the harness ran at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRevision {
    pub name: String,
    pub revision: String,
    pub branch: String,
    pub worktree: String,
    pub changed_files: u64,
}

/// A canonical receipt-bundle manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub generated_at: String,
    pub stamp: String,
    pub files: Vec<BundleFile>,
    pub revisions: Vec<BundleRevision>,
}

/// The result of signing a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedManifest {
    /// `H("weft/receipt-bundle/v1", canonical_manifest_bytes)`.
    pub digest: Hash32,
    pub public_key: Hash32,
    pub signature: Vec<u8>,
    pub fingerprint: String,
}

fn check_text(value: &str, what: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::malformed(format!("bundle manifest {what} is empty")));
    }
    if value.len() > 64 * 1024 {
        return Err(Error::limit_exceeded(format!(
            "bundle manifest {what} is {} bytes, above the 65536-byte bound",
            value.len()
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(Error::malformed(format!(
            "bundle manifest {what} contains a control character"
        )));
    }
    Ok(())
}

/// Reject absolute, parent-traversing, empty-component, or backslash paths.
pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::malformed("bundle file path is empty"));
    }
    if path.starts_with('/') {
        return Err(Error::malformed(format!(
            "bundle file path `{path}` is absolute"
        )));
    }
    if path.contains('\\') {
        return Err(Error::malformed(format!(
            "bundle file path `{path}` contains a backslash"
        )));
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(Error::malformed(format!(
                "bundle file path `{path}` has an empty or traversing component"
            )));
        }
    }
    if path.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Error::malformed(format!(
            "bundle file path `{path}` contains a control byte"
        )));
    }
    Ok(())
}

fn file_value(file: &BundleFile) -> Value {
    Value::map([
        ("path", Value::Str(file.path.clone())),
        ("size", Value::U64(file.size)),
        ("sha256", Value::Bytes(file.sha256.as_bytes().to_vec())),
    ])
}

fn revision_value(revision: &BundleRevision) -> Value {
    Value::map([
        ("name", Value::Str(revision.name.clone())),
        ("revision", Value::Str(revision.revision.clone())),
        ("branch", Value::Str(revision.branch.clone())),
        ("worktree", Value::Str(revision.worktree.clone())),
        ("changed_files", Value::U64(revision.changed_files)),
    ])
}

fn encoded_file(file: &BundleFile) -> Result<Vec<u8>> {
    encode_value(&file_value(file))
}

fn encoded_revision(revision: &BundleRevision) -> Result<Vec<u8>> {
    encode_value(&revision_value(revision))
}

fn expect_map<'a>(
    value: &'a Value,
    what: &str,
    keys: &[&str],
) -> Result<&'a std::collections::BTreeMap<String, Value>> {
    let map = match value {
        Value::Map(map) => map,
        _ => {
            return Err(Error::malformed(format!(
                "bundle manifest {what} must be a map"
            )))
        }
    };
    for key in map.keys() {
        if !keys.contains(&key.as_str()) {
            return Err(Error::non_canonical(format!(
                "unknown bundle manifest {what} key `{key}`"
            )));
        }
    }
    Ok(map)
}

fn expect_str<'a>(
    map: &'a std::collections::BTreeMap<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'a str> {
    match map.get(key) {
        Some(Value::Str(text)) => Ok(text),
        _ => Err(Error::malformed(format!(
            "bundle manifest {what} `{key}` must be text"
        ))),
    }
}

fn expect_u64(
    map: &std::collections::BTreeMap<String, Value>,
    key: &str,
    what: &str,
) -> Result<u64> {
    match map.get(key) {
        Some(Value::U64(value)) => Ok(*value),
        _ => Err(Error::malformed(format!(
            "bundle manifest {what} `{key}` must be a u64"
        ))),
    }
}

fn expect_hash32(
    map: &std::collections::BTreeMap<String, Value>,
    key: &str,
    what: &str,
) -> Result<Hash32> {
    match map.get(key) {
        Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
            let mut raw = [0u8; 32];
            raw.copy_from_slice(bytes);
            Ok(Hash32::from_bytes(raw))
        }
        _ => Err(Error::malformed(format!(
            "bundle manifest {what} `{key}` must be 32 bytes"
        ))),
    }
}

fn expect_array<'a>(
    map: &'a std::collections::BTreeMap<String, Value>,
    key: &str,
) -> Result<&'a [Value]> {
    match map.get(key) {
        Some(Value::Array(items)) => Ok(items),
        _ => Err(Error::malformed(format!(
            "bundle manifest `{key}` must be an array"
        ))),
    }
}

impl BundleManifest {
    /// Build a manifest, sorting entries by their canonical encoded bytes.
    pub fn new(
        generated_at: impl Into<String>,
        stamp: impl Into<String>,
        mut files: Vec<BundleFile>,
        mut revisions: Vec<BundleRevision>,
    ) -> Result<Self> {
        let generated_at = generated_at.into();
        let stamp = stamp.into();
        check_text(&generated_at, "generated_at")?;
        check_text(&stamp, "stamp")?;
        if files.len() > MAX_BUNDLE_FILES {
            return Err(Error::limit_exceeded(format!(
                "bundle manifest lists {} files, above the {MAX_BUNDLE_FILES}-file bound",
                files.len()
            )));
        }
        for file in &files {
            validate_path(&file.path)?;
        }
        for revision in &revisions {
            check_text(&revision.name, "revision name")?;
            check_text(&revision.revision, "revision id")?;
            check_text(&revision.branch, "revision branch")?;
            check_text(&revision.worktree, "revision worktree")?;
        }
        Self::sort_files(&mut files)?;
        Self::sort_revisions(&mut revisions)?;
        Ok(BundleManifest {
            generated_at,
            stamp,
            files,
            revisions,
        })
    }

    fn sort_files(files: &mut [BundleFile]) -> Result<()> {
        let mut keyed = Vec::with_capacity(files.len());
        for file in files.iter() {
            keyed.push((encoded_file(file)?, file.clone()));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in keyed.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::malformed(format!(
                    "bundle manifest lists `{}` more than once",
                    pair[0].1.path
                )));
            }
        }
        for (slot, (_, file)) in files.iter_mut().zip(keyed.into_iter()) {
            *slot = file;
        }
        Ok(())
    }

    fn sort_revisions(revisions: &mut [BundleRevision]) -> Result<()> {
        let mut keyed = Vec::with_capacity(revisions.len());
        for revision in revisions.iter() {
            keyed.push((encoded_revision(revision)?, revision.clone()));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in keyed.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::malformed(format!(
                    "bundle manifest lists repository `{}` more than once",
                    pair[0].1.name
                )));
            }
        }
        for (slot, (_, revision)) in revisions.iter_mut().zip(keyed.into_iter()) {
            *slot = revision;
        }
        Ok(())
    }

    /// Canonical WCB bytes of this manifest.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let files: Result<Vec<Value>> = self
            .files
            .iter()
            .map(|file| {
                validate_path(&file.path)?;
                Ok(file_value(file))
            })
            .collect();
        let revisions: Vec<Value> = self.revisions.iter().map(revision_value).collect();
        let value = Value::map([
            ("schema", Value::Str(MANIFEST_SCHEMA.to_owned())),
            ("generated_at", Value::Str(self.generated_at.clone())),
            ("stamp", Value::Str(self.stamp.clone())),
            ("files", Value::Array(files?)),
            ("revisions", Value::Array(revisions)),
        ]);
        encode_value(&value)
    }

    /// Decode and validate canonical bytes.
    ///
    /// Rejects non-canonical undecodable bytes, unknown/missing keys, wrong
    /// types, duplicate or unsorted arrays, and unsafe paths.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = decode_value(bytes)?;
        let map = expect_map(
            &value,
            "record",
            &["schema", "generated_at", "stamp", "files", "revisions"],
        )?;
        let schema = expect_str(map, "schema", "record")?;
        if schema != MANIFEST_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "bundle manifest schema `{schema}` is not `{MANIFEST_SCHEMA}`"
            )));
        }
        if map.len() != 5 {
            return Err(Error::malformed(
                "bundle manifest is missing a required key",
            ));
        }
        let generated_at = expect_str(map, "generated_at", "record")?.to_owned();
        let stamp = expect_str(map, "stamp", "record")?.to_owned();
        check_text(&generated_at, "generated_at")?;
        check_text(&stamp, "stamp")?;

        let mut files = Vec::new();
        let mut previous: Option<Vec<u8>> = None;
        for item in expect_array(map, "files")? {
            let item = expect_map(item, "file", &["path", "size", "sha256"])?;
            if item.len() != 3 {
                return Err(Error::malformed("bundle manifest file is missing a key"));
            }
            let path = expect_str(item, "path", "file")?.to_owned();
            validate_path(&path)?;
            let size = expect_u64(item, "size", "file")?;
            let sha256 = expect_hash32(item, "sha256", "file")?;
            let file = BundleFile { path, size, sha256 };
            let encoded = encoded_file(&file)?;
            if let Some(prev) = &previous {
                if *prev >= encoded {
                    return Err(Error::non_canonical(
                        "bundle manifest files are not strictly sorted by encoded bytes",
                    ));
                }
            }
            previous = Some(encoded);
            files.push(file);
        }

        let mut revisions = Vec::new();
        let mut previous: Option<Vec<u8>> = None;
        for item in expect_array(map, "revisions")? {
            let item = expect_map(
                item,
                "revision",
                &["name", "revision", "branch", "worktree", "changed_files"],
            )?;
            if item.len() != 5 {
                return Err(Error::malformed(
                    "bundle manifest revision is missing a key",
                ));
            }
            let revision = BundleRevision {
                name: expect_str(item, "name", "revision")?.to_owned(),
                revision: expect_str(item, "revision", "revision")?.to_owned(),
                branch: expect_str(item, "branch", "revision")?.to_owned(),
                worktree: expect_str(item, "worktree", "revision")?.to_owned(),
                changed_files: expect_u64(item, "changed_files", "revision")?,
            };
            check_text(&revision.name, "revision name")?;
            check_text(&revision.revision, "revision id")?;
            check_text(&revision.branch, "revision branch")?;
            check_text(&revision.worktree, "revision worktree")?;
            let encoded = encoded_revision(&revision)?;
            if let Some(prev) = &previous {
                if *prev >= encoded {
                    return Err(Error::non_canonical(
                        "bundle manifest revisions are not strictly sorted by encoded bytes",
                    ));
                }
            }
            previous = Some(encoded);
            revisions.push(revision);
        }

        let manifest = BundleManifest {
            generated_at,
            stamp,
            files,
            revisions,
        };
        if manifest.encode()? != bytes {
            return Err(Error::non_canonical(
                "bundle manifest bytes are not in canonical form",
            ));
        }
        Ok(manifest)
    }

    /// `H("weft/receipt-bundle/v1", canonical_manifest_bytes)`.
    pub fn digest(&self) -> Result<Hash32> {
        Ok(identity::h(SIGN_DOMAIN, &self.encode()?))
    }

    /// Scan `root` for payload files and `REVISIONS.txt`.
    pub fn from_directory(
        root: &Path,
        generated_at: impl Into<String>,
        stamp: impl Into<String>,
    ) -> Result<Self> {
        let mut files = Vec::new();
        walk_directory(root, root, &mut files)?;
        let revisions = read_revisions(&root.join("REVISIONS.txt"))?;
        BundleManifest::new(generated_at, stamp, files, revisions)
    }

    /// Read `receipt-manifest.wcb` from a bundle root.
    pub fn read_from(root: &Path) -> Result<Self> {
        let path = root.join(MANIFEST_FILE);
        let bytes = fs::read(&path)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
        BundleManifest::decode(&bytes)
    }

    /// Write the canonical manifest to `root/receipt-manifest.wcb`.
    pub fn write_to(&self, root: &Path) -> Result<PathBuf> {
        let path = root.join(MANIFEST_FILE);
        fs::write(&path, self.encode()?)
            .map_err(|err| Error::io(format!("cannot write {}: {err}", path.display())))?;
        Ok(path)
    }

    /// Re-hash every listed file and reject missing, wrong-size, wrong-hash, or
    /// unlisted payload files.
    pub fn verify_files(&self, root: &Path) -> Result<u64> {
        let listed: BTreeSet<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
        for file in &self.files {
            let path = root.join(&file.path);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Error::corrupt(format!(
                        "listed file `{}` is missing from the bundle",
                        file.path
                    )))
                }
                Err(err) => {
                    return Err(Error::io(format!("cannot stat {}: {err}", path.display())))
                }
            };
            if !metadata.is_file() {
                return Err(Error::corrupt(format!(
                    "listed path `{}` is not a regular file",
                    file.path
                )));
            }
            if metadata.len() != file.size {
                return Err(Error::corrupt(format!(
                    "listed file `{}` size is {} bytes, manifest says {}",
                    file.path,
                    metadata.len(),
                    file.size
                )));
            }
            let digest = sha256_path(&path)?;
            if digest != file.sha256 {
                return Err(Error::corrupt(format!(
                    "listed file `{}` sha256 {} does not match manifest {}",
                    file.path,
                    digest.to_hex(),
                    file.sha256.to_hex()
                )));
            }
        }

        let mut unlisted = Vec::new();
        collect_unlisted(root, root, &listed, &mut unlisted)?;
        if !unlisted.is_empty() {
            unlisted.sort();
            return Err(Error::corrupt(format!(
                "bundle contains unlisted payload file `{}`",
                unlisted[0]
            )));
        }
        Ok(self.files.len() as u64)
    }

    /// Sign this manifest with a 32-byte Ed25519 seed.
    pub fn sign(&self, seed: &[u8]) -> Result<SignedManifest> {
        let digest = self.digest()?;
        let public_key = weftcodec::capability::public_key_for_seed(seed)?;
        let signature = weftcodec::capability::sign_bytes(seed, digest.as_bytes())?;
        Ok(SignedManifest {
            digest,
            public_key,
            signature,
            fingerprint: key_fingerprint(&public_key),
        })
    }

    /// Verify a detached signature over this manifest.
    pub fn verify_signature(&self, public_key: &Hash32, signature: &[u8]) -> Result<()> {
        let digest = self.digest()?;
        weftcodec::capability::verify_bytes(public_key, digest.as_bytes(), signature)
            .map_err(Error::from)
    }
}

fn walk_directory(root: &Path, dir: &Path, out: &mut Vec<BundleFile>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", dir.display())))?
    {
        let entry =
            entry.map_err(|err| Error::io(format!("cannot read directory entry: {err}")))?;
        entries.push(entry);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?;
        if file_type.is_symlink() {
            return Err(Error::malformed(format!(
                "bundle contains a symlink at {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            walk_directory(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(Error::malformed(format!(
                "bundle contains a non-regular entry at {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::io("bundle path escaped its root"))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| Error::malformed("bundle path is not valid UTF-8"))?
            .replace('\\', "/");
        if is_reserved_root(&relative) {
            continue;
        }
        validate_path(&relative)?;
        let metadata = entry
            .metadata()
            .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?;
        out.push(BundleFile {
            path: relative,
            size: metadata.len(),
            sha256: sha256_path(&path)?,
        });
    }
    Ok(())
}

fn collect_unlisted(
    root: &Path,
    dir: &Path,
    listed: &BTreeSet<&str>,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(dir)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", dir.display())))?
    {
        let entry =
            entry.map_err(|err| Error::io(format!("cannot read directory entry: {err}")))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?;
        if file_type.is_symlink() {
            return Err(Error::corrupt(format!(
                "bundle contains a symlink at {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_unlisted(root, &path, listed, out)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::io("bundle path escaped its root"))?
            .to_str()
            .ok_or_else(|| Error::malformed("bundle path is not valid UTF-8"))?
            .replace('\\', "/");
        if is_reserved_root(&relative) {
            continue;
        }
        if !listed.contains(relative.as_str()) {
            out.push(relative);
        }
    }
    Ok(())
}

/// True for bundle metadata at the bundle root only.
pub fn is_reserved_root(relative: &str) -> bool {
    RESERVED_ROOT_FILES.contains(&relative)
}

fn sha256_path(path: &Path) -> Result<Hash32> {
    let mut file = fs::File::open(path)
        .map_err(|err| Error::io(format!("cannot open {}: {err}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut raw = [0u8; 32];
    raw.copy_from_slice(&digest);
    Ok(Hash32::from_bytes(raw))
}

fn read_revisions(path: &Path) -> Result<Vec<BundleRevision>> {
    let text = fs::read_to_string(path)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
    let mut revisions = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(Error::malformed(format!(
                "REVISIONS.txt line is not `name revision branch worktree changed`: `{line}`"
            )));
        }
        let changed_files = fields[4]
            .parse::<u64>()
            .map_err(|_| Error::malformed(format!("REVISIONS.txt count is not u64: `{line}`")))?;
        revisions.push(BundleRevision {
            name: fields[0].to_owned(),
            revision: fields[1].to_owned(),
            branch: fields[2].to_owned(),
            worktree: fields[3].to_owned(),
            changed_files,
        });
    }
    if revisions.is_empty() {
        return Err(Error::malformed("REVISIONS.txt lists no repositories"));
    }
    Ok(revisions)
}

/// Short, human-verifiable public-key fingerprint.
///
/// `ed25519:` followed by the first 16 bytes of
/// `H("weft/receipt-bundle/key/v1", public_key)`, lowercase hex.
pub fn key_fingerprint(public_key: &Hash32) -> String {
    let digest = identity::h(KEY_FINGERPRINT_DOMAIN, public_key.as_bytes());
    let hex = digest.to_hex();
    format!("ed25519:{}", &hex[..32])
}

/// Parse a public key given as 64 lowercase/uppercase hex characters.
pub fn parse_public_key(text: &str) -> Result<Hash32> {
    let trimmed = text.trim();
    let bytes = hex::decode(trimmed)
        .map_err(|_| Error::malformed("Ed25519 public key is not hexadecimal"))?;
    if bytes.len() != 32 {
        return Err(Error::malformed(format!(
            "Ed25519 public key must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut raw = [0u8; 32];
    raw.copy_from_slice(&bytes);
    Ok(Hash32::from_bytes(raw))
}

/// Read a 32-byte Ed25519 seed from a raw or hex file.
pub fn read_seed(path: &Path) -> Result<[u8; 32]> {
    let bytes = fs::read(path)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
    let decoded = decode_key_blob(&bytes, 32)?;
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&decoded);
    Ok(seed)
}

/// Decode a key/signature blob that may be raw or hex text.
pub fn decode_key_blob(bytes: &[u8], expected: usize) -> Result<Vec<u8>> {
    if bytes.len() == expected {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::malformed("blob is neither raw nor hexadecimal text"))?;
    let trimmed = text.trim();
    if trimmed.len() == expected * 2 {
        let decoded =
            hex::decode(trimmed).map_err(|_| Error::malformed("blob is not hexadecimal"))?;
        if decoded.len() == expected {
            return Ok(decoded);
        }
    }
    Err(Error::malformed(format!(
        "blob must be {expected} raw bytes or {} hex characters, got {} bytes",
        expected * 2,
        bytes.len()
    )))
}

/// Encode a key/signature blob as lowercase hex plus a trailing newline.
pub fn encode_key_blob(bytes: &[u8]) -> String {
    format!("{}\n", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> Hash32 {
        Hash32::from_bytes([byte; 32])
    }

    #[test]
    fn manifest_round_trips_and_sorts() {
        let manifest = BundleManifest::new(
            "2026-11-06T00:00:00Z",
            "20261106T000000Z",
            vec![
                BundleFile {
                    path: "b/b".into(),
                    size: 2,
                    sha256: hash(2),
                },
                BundleFile {
                    path: "a".into(),
                    size: 1,
                    sha256: hash(1),
                },
            ],
            vec![BundleRevision {
                name: "WeftFS".into(),
                revision: "0".repeat(40),
                branch: "staging".into(),
                worktree: "clean".into(),
                changed_files: 0,
            }],
        )
        .unwrap();
        let bytes = manifest.encode().unwrap();
        let decoded = BundleManifest::decode(&bytes).unwrap();
        assert_eq!(manifest, decoded);
        assert_eq!(decoded.files[0].path, "a");
    }

    #[test]
    fn rejects_unsorted_manifest() {
        let manifest = BundleManifest::new(
            "t",
            "s",
            vec![
                BundleFile {
                    path: "a".into(),
                    size: 1,
                    sha256: hash(1),
                },
                BundleFile {
                    path: "b".into(),
                    size: 2,
                    sha256: hash(2),
                },
            ],
            vec![BundleRevision {
                name: "WeftFS".into(),
                revision: "0".repeat(40),
                branch: "staging".into(),
                worktree: "clean".into(),
                changed_files: 0,
            }],
        )
        .unwrap();
        let mut value = decode_value(&manifest.encode().unwrap()).unwrap();
        if let Value::Map(map) = &mut value {
            if let Some(Value::Array(files)) = map.get_mut("files") {
                files.reverse();
            }
        }
        let raw = encode_value(&value).unwrap();
        let err = BundleManifest::decode(&raw).unwrap_err();
        assert!(err.message().contains("strictly sorted"));
    }
}
