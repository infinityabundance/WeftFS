//! Optional authenticated encryption at rest (subphase T079).
//!
//! Normative description: `docs/ENCRYPTION.md`. The envelope is a **blob-plane**
//! wrapper: it encrypts the bytes the store hands to a blob plane and returns
//! them on read. Codec frames, manifests, object records, commit records and
//! every frozen wire byte are untouched; a reader with the key sees exactly the
//! bytes the writer produced.
//!
//! # Layout (per blob)
//!
//! ```text
//! header 96 bytes:
//!   0..6    magic "WEFTE1"
//!   6       version 1
//!   7       mode (1 convergent, 2 random)
//!   8..12   reserved zero
//!   12..16  block_size u32le (65536)
//!   16..24  plaintext_len u64le
//!   24..32  block_count u64le (>= 1)
//!   32..48  key_id (16 bytes)
//!   48..60  base_nonce (12 bytes)
//!   60..96  plaintext_hash (32 bytes; convergent only, zero in random mode)
//! blocks: ceil(len / block_size) blocks (at least one), each
//!   ChaCha20-Poly1305(per-object key, nonce_i, aad_i) over the plaintext block
//!   ciphertext length = plaintext block length + 16
//! ```
//!
//! `nonce_i = base_nonce XOR u64le(i)` (first 8 bytes) and
//! `aad_i = header || u64le(i) || u64le(plain_block_len)`. The header is
//! authenticated by every block, so a changed length, mode, key id or block
//! size fails closed. An empty plaintext still has one block, so an empty
//! object's header is authenticated too.
//!
//! # Two dedup policies
//!
//! * [`EncryptionMode::Convergent`] derives the per-object key and nonce from
//!   the plaintext hash, so identical plaintext under the same master key
//!   produces **identical ciphertext**. Physical and backend-level dedup is
//!   preserved; ciphertext equality reveals plaintext equality (including across
//!   tenants), and a party holding the key can confirm guesses offline.
//! * [`EncryptionMode::Random`] uses a fresh random nonce per publication and
//!   derives the per-object key from it, so identical plaintext produces
//!   **different ciphertext**. Any dedup performed on ciphertext (an object
//!   store's own dedup, a backup system, a snapshot-diff) is lost.
//!
//! **What neither mode does:** WeftFS addresses chunks and manifests by a
//! domain-separated hash of the *plaintext* (`WEFT-MODEL.md`), so the logical
//! address space still deduplicates identical plaintext before the blob plane
//! is consulted. Random mode removes ciphertext equality; it cannot remove that
//! logical dedup without changing frozen manifest bytes. This is stated again in
//! the document and is not claimed otherwise.
//!
//! # Key lifecycle
//!
//! A key file lists one active key and any number of retained keys. The key id
//! is derived from the secret, so a mislabelled entry is refused. A retained key
//! keeps old objects readable after rotation; a `revoked` key refuses with
//! `Unauthorized`. An unknown key id is `Corrupt`.

use std::fs;
use std::path::{Path, PathBuf};

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::rand::{SecureRandom, SystemRandom};

use crate::backend::{BackendStats, Key, RangeRead};
use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::packs::PackReclaim;
use crate::planes::{BlobCapabilities, BlobPlane};

/// Envelope magic.
pub const ENCRYPTION_MAGIC: &[u8; 6] = b"WEFTE1";
/// Envelope version.
pub const ENCRYPTION_VERSION: u8 = 1;
/// Fixed header size.
pub const HEADER_BYTES: usize = 96;
/// Fixed block size.
pub const BLOCK_BYTES: usize = 65_536;
/// AEAD tag size.
pub const TAG_BYTES: usize = 16;
/// Key-file schema (operator configuration, never frozen wire).
pub const KEY_FILE_SCHEMA: &str = "weft.encryption.keyfile.v1";

/// Exact substrings of an envelope refusal, as they appear in an error message
/// or a recovery note.
///
/// `Store::open` uses these to refuse an *older* head: when the newest
/// published state cannot be opened because the key set is wrong or a key is
/// revoked, that is a configuration failure, not a damaged store, and serving
/// a stale generation would be a silent downgrade.
const ENVELOPE_REFUSAL_MARKERS: [&str; 3] = [
    "encrypted object names unknown key id",
    "is revoked; the object cannot be read",
    "encrypted object authentication failed",
];

/// Whether `text` (an error message or recovery note) records an envelope
/// refusal rather than a damaged store.
pub fn is_envelope_refusal(text: &str) -> bool {
    ENVELOPE_REFUSAL_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

/// Dedup policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptionMode {
    /// Deterministic ciphertext; physical dedup preserved, equality visible.
    Convergent,
    /// Random nonce per publication; ciphertext equality (and ciphertext-level
    /// dedup) removed.
    Random,
}

impl EncryptionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EncryptionMode::Convergent => "convergent",
            EncryptionMode::Random => "random",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "convergent" => Ok(EncryptionMode::Convergent),
            "random" => Ok(EncryptionMode::Random),
            other => Err(Error::invalid_argument(format!(
                "encryption mode `{other}` is not convergent|random"
            ))),
        }
    }

    fn tag(self) -> u8 {
        match self {
            EncryptionMode::Convergent => 1,
            EncryptionMode::Random => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(EncryptionMode::Convergent),
            2 => Ok(EncryptionMode::Random),
            other => Err(Error::corrupt(format!(
                "unknown encryption envelope mode {other}"
            ))),
        }
    }
}

/// One key in the key file.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptionKey {
    /// 16-byte id derived from `secret`.
    pub id: [u8; 16],
    /// Monotonic operator epoch (receipts and diagnostics only).
    pub epoch: u32,
    pub secret: [u8; 32],
    /// A revoked key refuses to decrypt.
    pub revoked: bool,
}

impl EncryptionKey {
    /// Derive the id a secret must carry.
    pub fn id_for_secret(secret: &[u8; 32]) -> [u8; 16] {
        let digest = identity::h("weft/enc/key-id/v1", secret);
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest.as_bytes()[..16]);
        id
    }

    pub fn new(secret: [u8; 32], epoch: u32, revoked: bool) -> Self {
        EncryptionKey {
            id: Self::id_for_secret(&secret),
            epoch,
            secret,
            revoked,
        }
    }

    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }
}

/// Operator key file.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptionConfig {
    pub mode: EncryptionMode,
    /// Id of the key new publications use.
    pub active: [u8; 16],
    pub keys: Vec<EncryptionKey>,
    /// Where the configuration came from, for diagnostics.
    pub source: String,
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionKey")
            .field("id", &self.id_hex())
            .field("epoch", &self.epoch)
            .field("revoked", &self.revoked)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Debug for EncryptionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionConfig")
            .field("mode", &self.mode.as_str())
            .field("active", &hex::encode(self.active))
            .field("keys", &self.keys)
            .field("source", &self.source)
            .finish()
    }
}

impl EncryptionConfig {
    /// A one-key configuration from raw material (CLI `--encryption-key-hex`).
    pub fn from_secret(mode: EncryptionMode, secret: [u8; 32]) -> Self {
        let key = EncryptionKey::new(secret, 1, false);
        EncryptionConfig {
            mode,
            active: key.id,
            keys: vec![key],
            source: "inline".to_owned(),
        }
    }

    /// Load `weft.encryption.keyfile.v1` JSON.
    ///
    /// ```json
    /// {"schema":"weft.encryption.keyfile.v1","mode":"convergent",
    ///  "active":"<32 hex>","keys":[
    ///    {"id":"<32 hex>","epoch":1,"secret":"<64 hex>","revoked":false}]}
    /// ```
    pub fn from_key_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
        let doc: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| {
            Error::malformed(format!("key file {} is not JSON: {err}", path.display()))
        })?;
        if doc.get("schema").and_then(|value| value.as_str()) != Some(KEY_FILE_SCHEMA) {
            return Err(Error::malformed(format!(
                "key file schema must be {KEY_FILE_SCHEMA}"
            )));
        }
        let mode = EncryptionMode::parse(
            doc.get("mode")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::malformed("key file needs `mode`"))?,
        )?;
        let active = parse_id(
            doc.get("active")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::malformed("key file needs `active`"))?,
        )?;
        let rows = doc
            .get("keys")
            .and_then(|value| value.as_array())
            .ok_or_else(|| Error::malformed("key file needs a `keys` array"))?;
        let mut keys = Vec::with_capacity(rows.len());
        for row in rows {
            let id = parse_id(
                row.get("id")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| Error::malformed("key needs `id`"))?,
            )?;
            let secret = parse_secret(
                row.get("secret")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| Error::malformed("key needs `secret`"))?,
            )?;
            let epoch = row
                .get("epoch")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| Error::malformed("key needs u64 `epoch`"))?;
            let epoch =
                u32::try_from(epoch).map_err(|_| Error::malformed("key epoch does not fit u32"))?;
            let revoked = row
                .get("revoked")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let derived = EncryptionKey::id_for_secret(&secret);
            if derived != id {
                return Err(Error::non_canonical(format!(
                    "key id {} does not match its secret (derived {})",
                    hex::encode(id),
                    hex::encode(derived)
                )));
            }
            keys.push(EncryptionKey {
                id,
                epoch,
                secret,
                revoked,
            });
        }
        if keys.is_empty() {
            return Err(Error::invalid_argument("key file has no keys"));
        }
        let config = EncryptionConfig {
            mode,
            active,
            keys,
            source: path.display().to_string(),
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for key in &self.keys {
            if !seen.insert(key.id) {
                return Err(Error::non_canonical("key file repeats a key id"));
            }
        }
        let active = self
            .keys
            .iter()
            .find(|key| key.id == self.active)
            .ok_or_else(|| Error::invalid_argument("active key is absent from the key set"))?;
        if active.revoked {
            return Err(Error::invalid_argument("the active key is revoked"));
        }
        Ok(())
    }

    fn key(&self, id: &[u8]) -> Result<&EncryptionKey> {
        let key = self
            .keys
            .iter()
            .find(|key| key.id.as_slice() == id)
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "encrypted object names unknown key id {}",
                    hex::encode(id)
                ))
            })?;
        if key.revoked {
            return Err(Error::unauthorized(format!(
                "encryption key {} is revoked; the object cannot be read",
                key.id_hex()
            )));
        }
        Ok(key)
    }

    /// Diagnostic summary with no secret material.
    pub fn describe(&self) -> String {
        format!(
            "encryption:{}:active={}:keys={}",
            self.mode.as_str(),
            hex::encode(self.active),
            self.keys.len()
        )
    }
}

fn parse_id(text: &str) -> Result<[u8; 16]> {
    let raw = hex::decode(text.trim()).map_err(|_| Error::malformed("key id is not hex"))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| Error::malformed("key id must be 16 bytes"))
}

fn parse_secret(text: &str) -> Result<[u8; 32]> {
    let raw = hex::decode(text.trim()).map_err(|_| Error::malformed("key secret is not hex"))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| Error::malformed("key secret must be 32 bytes"))
}

/// A parsed envelope header.
#[derive(Clone, Debug)]
struct Header {
    mode: EncryptionMode,
    block_size: usize,
    plaintext_len: u64,
    block_count: u64,
    key_id: [u8; 16],
    base_nonce: [u8; 12],
    plaintext_hash: [u8; 32],
    bytes: [u8; HEADER_BYTES],
}

impl Header {
    fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut out = [0u8; HEADER_BYTES];
        out[0..6].copy_from_slice(ENCRYPTION_MAGIC);
        out[6] = ENCRYPTION_VERSION;
        out[7] = self.mode.tag();
        out[12..16].copy_from_slice(&(self.block_size as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.plaintext_len.to_le_bytes());
        out[24..32].copy_from_slice(&self.block_count.to_le_bytes());
        out[32..48].copy_from_slice(&self.key_id);
        out[48..60].copy_from_slice(&self.base_nonce);
        out[60..92].copy_from_slice(&self.plaintext_hash);
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_BYTES {
            return Err(Error::corrupt("encrypted object header is truncated"));
        }
        if &bytes[0..6] != ENCRYPTION_MAGIC {
            return Err(Error::corrupt("missing encryption envelope magic"));
        }
        if bytes[6] != ENCRYPTION_VERSION {
            return Err(Error::unsupported_version(format!(
                "encryption envelope version {} is not supported",
                bytes[6]
            )));
        }
        let mode = EncryptionMode::from_tag(bytes[7])?;
        if bytes[8..12] != [0u8; 4] {
            return Err(Error::non_canonical(
                "encryption envelope reserved bytes are not zero",
            ));
        }
        let block_size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        if block_size == 0 || block_size > 16 * 1024 * 1024 {
            return Err(Error::corrupt("encryption envelope block size is invalid"));
        }
        let plaintext_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let block_count = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let expected_blocks = plaintext_len
            .saturating_add(block_size as u64 - 1)
            .saturating_div(block_size as u64)
            .max(1);
        if block_count != expected_blocks {
            return Err(Error::corrupt(
                "encryption envelope block count does not match its plaintext length",
            ));
        }
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&bytes[32..48]);
        let mut base_nonce = [0u8; 12];
        base_nonce.copy_from_slice(&bytes[48..60]);
        let mut plaintext_hash = [0u8; 32];
        plaintext_hash.copy_from_slice(&bytes[60..92]);
        let mut header = [0u8; HEADER_BYTES];
        header.copy_from_slice(&bytes[..HEADER_BYTES]);
        Ok(Header {
            mode,
            block_size,
            plaintext_len,
            block_count,
            key_id,
            base_nonce,
            plaintext_hash,
            bytes: header,
        })
    }

    /// Total encrypted length including the header.
    fn ciphertext_len(&self) -> u64 {
        (HEADER_BYTES as u64)
            .saturating_add(self.plaintext_len)
            .saturating_add(self.block_count.saturating_mul(TAG_BYTES as u64))
    }

    /// Plaintext length of block `index`.
    fn block_plain_len(&self, index: u64) -> usize {
        if index + 1 < self.block_count {
            return self.block_size;
        }
        let remainder = self.plaintext_len - (self.block_count - 1) * self.block_size as u64;
        if remainder == 0 {
            0
        } else {
            remainder as usize
        }
    }

    fn nonce(&self, index: u64) -> [u8; 12] {
        let mut nonce = self.base_nonce;
        let counter = index.to_le_bytes();
        for position in 0..8 {
            nonce[position] ^= counter[position];
        }
        nonce
    }

    fn aad(&self, index: u64, plain_len: usize) -> Vec<u8> {
        let mut aad = Vec::with_capacity(HEADER_BYTES + 16);
        aad.extend_from_slice(&self.bytes);
        aad.extend_from_slice(&index.to_le_bytes());
        aad.extend_from_slice(&(plain_len as u64).to_le_bytes());
        aad
    }
}

fn object_key(master: &[u8; 32], mode: EncryptionMode, material: &[u8]) -> [u8; 32] {
    let mut seeded = Vec::with_capacity(32 + material.len());
    match mode {
        EncryptionMode::Convergent => seeded.extend_from_slice(b"weft/enc/key-convergent/v1"),
        EncryptionMode::Random => seeded.extend_from_slice(b"weft/enc/key-random/v1"),
    }
    seeded.extend_from_slice(material);
    *blake3::keyed_hash(master, &seeded).as_bytes()
}

fn aead(key: &[u8; 32]) -> Result<LessSafeKey> {
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key)
        .map_err(|_| Error::corrupt("encryption key is not usable"))?;
    Ok(LessSafeKey::new(unbound))
}

fn subkey(config: &EncryptionConfig) -> Result<&EncryptionKey> {
    config
        .keys
        .iter()
        .find(|key| key.id == config.active)
        .ok_or_else(|| Error::invalid_argument("active encryption key is missing"))
}

fn encrypt(config: &EncryptionConfig, plaintext: &[u8]) -> Result<Vec<u8>> {
    let key = subkey(config)?;
    let plaintext_hash = identity::content_hash(plaintext);
    let mut base_nonce = [0u8; 12];
    let per_key = match config.mode {
        EncryptionMode::Convergent => {
            let digest = identity::h("weft/enc/nonce/v1", plaintext_hash.as_bytes());
            base_nonce.copy_from_slice(&digest.as_bytes()[..12]);
            object_key(&key.secret, config.mode, plaintext_hash.as_bytes())
        }
        EncryptionMode::Random => {
            SystemRandom::new()
                .fill(&mut base_nonce)
                .map_err(|_| Error::io("system random source failed"))?;
            object_key(&key.secret, config.mode, &base_nonce)
        }
    };
    let block_size = BLOCK_BYTES;
    let block_count = ((plaintext.len() + block_size - 1) / block_size).max(1) as u64;
    let mut header = Header {
        mode: config.mode,
        block_size,
        plaintext_len: plaintext.len() as u64,
        block_count,
        key_id: key.id,
        base_nonce,
        plaintext_hash: match config.mode {
            EncryptionMode::Convergent => *plaintext_hash.as_bytes(),
            EncryptionMode::Random => [0u8; 32],
        },
        bytes: [0u8; HEADER_BYTES],
    };
    let header_bytes = header.encode();
    // `aad` binds the *encoded* header, so it must be the exact bytes that are
    // written ahead of the body. Preparing them here is what makes the seal and
    // open AAD identical.
    header.bytes = header_bytes;
    let mut out = Vec::with_capacity(header.ciphertext_len() as usize);
    out.extend_from_slice(&header_bytes);
    let cipher = aead(&per_key)?;
    for index in 0..block_count {
        let start = index as usize * block_size;
        let end = (start + block_size).min(plaintext.len());
        let mut block = plaintext[start..end].to_vec();
        let nonce = header.nonce(index);
        cipher
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(header.aad(index, block.len())),
                &mut block,
            )
            .map_err(|_| Error::corrupt("encryption failed"))?;
        out.extend_from_slice(&block);
    }
    Ok(out)
}

fn object_key_for(header: &Header, key: &EncryptionKey) -> [u8; 32] {
    match header.mode {
        EncryptionMode::Convergent => object_key(&key.secret, header.mode, &header.plaintext_hash),
        EncryptionMode::Random => object_key(&key.secret, header.mode, &header.base_nonce),
    }
}

fn decrypt_all(config: &EncryptionConfig, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let header = Header::decode(ciphertext)?;
    if ciphertext.len() as u64 != header.ciphertext_len() {
        return Err(Error::corrupt(format!(
            "encrypted object length {} does not match its header's {}",
            ciphertext.len(),
            header.ciphertext_len()
        )));
    }
    let key = config.key(&header.key_id)?;
    let per_key = object_key_for(&header, key);
    let cipher = aead(&per_key)?;
    let mut out = Vec::with_capacity(header.plaintext_len as usize);
    let mut cursor = HEADER_BYTES;
    for index in 0..header.block_count {
        let plain_len = header.block_plain_len(index);
        let mut block = ciphertext[cursor..cursor + plain_len + TAG_BYTES].to_vec();
        cursor += plain_len + TAG_BYTES;
        let nonce = header.nonce(index);
        let plaintext = cipher
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(header.aad(index, plain_len)),
                &mut block,
            )
            .map_err(|_| Error::corrupt("encrypted object authentication failed"))?;
        out.extend_from_slice(plaintext);
    }
    if out.len() as u64 != header.plaintext_len {
        return Err(Error::corrupt(
            "encrypted object decoded to an unexpected length",
        ));
    }
    if header.mode == EncryptionMode::Convergent {
        let digest = identity::content_hash(&out);
        if digest.as_bytes() != &header.plaintext_hash {
            return Err(Error::corrupt(
                "encrypted object plaintext hash does not match its header",
            ));
        }
    }
    Ok(out)
}

/// A [`BlobPlane`] that encrypts every blob and decrypts every read.
pub struct EncryptedBlobPlane<P: BlobPlane> {
    inner: P,
    config: EncryptionConfig,
}

impl<P: BlobPlane> EncryptedBlobPlane<P> {
    pub fn new(inner: P, config: EncryptionConfig) -> Result<Self> {
        config.validate()?;
        // A probe write/read proves the configuration can round-trip before any
        // store operation depends on it.
        let probe = b"weft-encryption-roundtrip-probe";
        let sealed = encrypt(&config, probe)?;
        if decrypt_all(&config, &sealed)? != probe {
            return Err(Error::corrupt(
                "encryption configuration failed its round-trip probe",
            ));
        }
        Ok(EncryptedBlobPlane { inner, config })
    }

    pub fn config(&self) -> &EncryptionConfig {
        &self.config
    }

    fn read_header(&self, key: &Key) -> Result<Header> {
        let bytes = self.inner.get_range(key, 0, HEADER_BYTES as u64)?;
        Header::decode(&bytes)
    }

    fn read_block_range(
        &self,
        key: &Key,
        header: &Header,
        first: u64,
        last: u64,
    ) -> Result<Vec<u8>> {
        let offset = HEADER_BYTES as u64 + first * (header.block_size as u64 + TAG_BYTES as u64);
        // Exactly the blocks requested, not the rest of the object.
        let mut length = 0u64;
        for index in first..=last {
            length = length.saturating_add(header.block_plain_len(index) as u64 + TAG_BYTES as u64);
        }
        let bytes = self.inner.get_range(key, offset, length)?;
        let key = self.config.key(&header.key_id)?;
        let per_key = object_key_for(header, key);
        let cipher = aead(&per_key)?;
        let mut out = Vec::new();
        let mut cursor = 0usize;
        for index in first..=last {
            let plain_len = header.block_plain_len(index);
            if cursor + plain_len + TAG_BYTES > bytes.len() {
                return Err(Error::corrupt("encrypted object block range is truncated"));
            }
            let mut block = bytes[cursor..cursor + plain_len + TAG_BYTES].to_vec();
            cursor += plain_len + TAG_BYTES;
            let nonce = header.nonce(index);
            let plaintext = cipher
                .open_in_place(
                    Nonce::assume_unique_for_key(nonce),
                    Aad::from(header.aad(index, plain_len)),
                    &mut block,
                )
                .map_err(|_| Error::corrupt("encrypted object authentication failed"))?;
            out.extend_from_slice(plaintext);
        }
        Ok(out)
    }
}

impl<P: BlobPlane> BlobPlane for EncryptedBlobPlane<P> {
    fn blob_label(&self) -> &'static str {
        "encrypted"
    }

    fn blob_capabilities(&self) -> BlobCapabilities {
        // Encryption adds CPU work and bytes; it does not add or remove a
        // durability, atomicity or range-read guarantee.
        self.inner.blob_capabilities()
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>> {
        let ciphertext = self.inner.get(key)?;
        decrypt_all(&self.config, &ciphertext)
    }

    fn get_range(&self, key: &Key, offset: u64, len: u64) -> Result<Vec<u8>> {
        let header = self.read_header(key)?;
        if offset > header.plaintext_len {
            return Err(Error::invalid_argument(format!(
                "range offset {offset} is beyond the {} byte plaintext",
                header.plaintext_len
            )));
        }
        if len == 0 || offset == header.plaintext_len {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len).min(header.plaintext_len);
        let block_size = header.block_size as u64;
        let first = offset / block_size;
        let last = (end - 1) / block_size;
        let block_bytes = self.read_block_range(key, &header, first, last)?;
        let start = (offset - first * block_size) as usize;
        let stop = start + (end - offset) as usize;
        if stop > block_bytes.len() {
            return Err(Error::corrupt(
                "encrypted object range decoded to an unexpected length",
            ));
        }
        Ok(block_bytes[start..stop].to_vec())
    }

    fn read_ranges(&self, requests: &[RangeRead]) -> Result<Vec<Vec<u8>>> {
        requests
            .iter()
            .map(|request| self.get_range(&request.key, request.offset, request.len))
            .collect()
    }

    fn put_if_absent(&mut self, key: &Key, bytes: &[u8]) -> Result<()> {
        let sealed = encrypt(&self.config, bytes)?;
        self.inner.put_if_absent(key, &sealed)
    }

    fn delete(&mut self, key: &Key) -> Result<()> {
        self.inner.delete(key)
    }

    fn delete_many(&mut self, keys: &[Key]) -> Result<()> {
        self.inner.delete_many(keys)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }

    fn reclaim_unreferenced_packs(&mut self) -> Result<PackReclaim> {
        self.inner.reclaim_unreferenced_packs()
    }

    fn verify_packs(&self) -> Result<Vec<String>> {
        self.inner.verify_packs()
    }

    fn pack_counters(&self) -> Option<crate::packs::PackStats> {
        self.inner.pack_counters()
    }

    fn buffered_blob_kinds(&self) -> u8 {
        self.inner.buffered_blob_kinds()
    }

    fn refresh_metadata(&mut self) -> Result<()> {
        self.inner.refresh_metadata()
    }

    fn defer_pack_catalog(&mut self) -> Result<bool> {
        self.inner.defer_pack_catalog()
    }

    fn take_pending_pack_members(&mut self) -> Result<Option<crate::packs::DeferredPackEntries>> {
        self.inner.take_pending_pack_members()
    }

    fn adopt_pending_pack_members(
        &mut self,
        entries: crate::packs::DeferredPackEntries,
    ) -> Result<()> {
        self.inner.adopt_pending_pack_members(entries)
    }

    fn len(&self, key: &Key) -> Result<u64> {
        // The store asks for the plaintext length; the physical object is
        // larger by the header and one tag per block.
        let header = self.read_header(key)?;
        Ok(header.plaintext_len)
    }

    fn exists(&self, key: &Key) -> Result<bool> {
        self.inner.exists(key)
    }

    fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.inner.list(prefix)
    }

    fn clean_stale(&mut self, grace_secs: u64) -> Result<()> {
        self.inner.clean_stale(grace_secs)
    }

    fn init_layout(&mut self) -> Result<()> {
        self.inner.init_layout()
    }

    fn ensure_layout(&mut self) -> Result<()> {
        self.inner.ensure_layout()
    }

    fn stats(&self) -> BackendStats {
        self.inner.stats()
    }

    fn diagnostics(&self) -> Vec<String> {
        let mut lines = self.inner.diagnostics();
        lines.push(self.config.describe());
        lines
    }

    fn root(&self) -> &Path {
        self.inner.root()
    }

    // `head_part` deliberately keeps the trait default: the probe must publish
    // and read back through *this* plane, so the bytes it leaves behind (and
    // deletes) are ciphertext, not plaintext on the inner plane.
}

/// Convenience: wrap a boxed plane.
pub fn wrap(
    inner: Box<dyn BlobPlane + Send>,
    config: EncryptionConfig,
) -> Result<Box<dyn BlobPlane + Send>> {
    Ok(Box::new(EncryptedBlobPlane::new(inner, config)?))
}

/// Convenience: the on-disk name of the envelope for a key file path.
pub fn describe_source(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Public round-trip used by the court and by configuration validation.
pub fn round_trip(config: &EncryptionConfig, plaintext: &[u8]) -> Result<Vec<u8>> {
    let sealed = encrypt(config, plaintext)?;
    decrypt_all(config, &sealed)
}

/// Public sealed form used by the court to measure envelope overhead.
pub fn seal(config: &EncryptionConfig, plaintext: &[u8]) -> Result<Vec<u8>> {
    encrypt(config, plaintext)
}

/// Public open used by the court to prove tamper refusal.
pub fn open(config: &EncryptionConfig, ciphertext: &[u8]) -> Result<Vec<u8>> {
    decrypt_all(config, ciphertext)
}

/// A deterministic test key from a label. Never used for real data; the court
/// uses it so encrypted fixtures are reproducible.
pub fn test_key(label: &str) -> [u8; 32] {
    *identity::h("weft/enc/test-key/v1", label.as_bytes()).as_bytes()
}

/// The maximum plaintext a single envelope can carry under the block bound.
pub const MAX_PLAINTEXT_BYTES: u64 = (u32::MAX as u64 + 1) * BLOCK_BYTES as u64 - 1;

/// A configuration from a key file, with a clear error naming the file.
pub fn load(path: impl AsRef<Path>) -> Result<EncryptionConfig> {
    let path: PathBuf = path.as_ref().to_path_buf();
    EncryptionConfig::from_key_file(&path)
}

/// The plaintext hash a convergent envelope carries, for diagnostics.
pub fn convergent_plaintext_hash(sealed: &[u8]) -> Result<Hash32> {
    let header = Header::decode(sealed)?;
    if header.mode != EncryptionMode::Convergent {
        return Err(Error::invalid_argument(
            "a random-mode envelope carries no plaintext hash",
        ));
    }
    Ok(Hash32::from_bytes(header.plaintext_hash))
}
