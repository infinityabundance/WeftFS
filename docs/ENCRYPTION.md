# Optional encryption at rest (T079)

Status: **implemented** for the Rust store (`src/encryption.rs`,
`BackendKind::Encrypted`, `--encryption-key-file` / `--encryption-key-hex` /
`--encryption-mode`). Not implemented in WeftFS-Go, not implemented in the
FUSE mount beyond what the store already does, and not audited by anyone
outside this programme. Section 9 states exactly what it does not do.

The envelope wraps **blob bodies only**. Commit records, refs, the superblock,
the writer lock and the quota record stay plaintext: they are the store's
control plane and the frozen publication protocol. Codec frames, mode numbers,
roots, manifests and every existing reader are untouched — an opened envelope
yields the exact bytes that were published.

## 1. What is encrypted and when

`BackendKind::Encrypted { inner, config }` sits **outermost** over the blob
plane:

```
store  →  EncryptedBlobPlane  →  [PackedBlobPlane]  →  Local / Object / S3 / GCS / Azure / 3FS
```

Every `put_if_absent` seals the whole blob; every `get` opens it; `get_range`
reads only the header plus the blocks that cover the requested plaintext range;
`len` reports the **plaintext** length (an extra 96-byte header read); the
probe (`head_part`) publishes and reads back ciphertext. `list`, `exists`,
`delete`, `flush`, pack deferral and pack reclamation forward to the inner
plane unchanged, so packing groups ciphertext members and the catalog stays a
commit-plane record.

Because the wrapper is a `BlobPlane`, the store, the read pool, the decoded
object cache, GC, verify, snapshots, MW2 and the checkpoint writer need no
change. That is also why the envelope cannot hide object names: see §4.

## 2. Envelope format

```
offset  size  field
0       6     magic "WEFTE1"
6       1     version = 1
7       1     mode: 1 convergent, 2 random
8       4     reserved, must be zero
12      4     block_size (u32 LE) = 65536, bound [1, 16 MiB]
16      8     plaintext_len (u64 LE)
24      8     block_count (u64 LE), must equal max(1, ceil(plaintext_len / block_size))
32      16    key id = BLAKE3-16("weft/enc/key-id/v1", secret)
48      12    base nonce
60      32    plaintext hash BLAKE3("weft/chunk/v1", plaintext); zero in random mode
92      4     zero padding
96      …     block 0 ‖ block 1 ‖ …
```

Each block is `ChaCha20-Poly1305(plaintext_block) ‖ 16-byte tag`, so an object
costs `96 + plaintext_len + 16 × block_count` bytes. The block's AEAD input is:

* **key** — `BLAKE3-keyed(secret, "weft/enc/key-<mode>/v1" ‖ material)` where
  `material` is the plaintext hash for convergent mode and the base nonce for
  random mode. The per-object key is therefore independent of the blob name and
  of any other object.
* **nonce** — `base_nonce XOR u64le(block_index)`.
* **AAD** — the 96 encoded header bytes ‖ `u64le(block_index)` ‖
  `u64le(plaintext_block_len)`.

Decrypt refuses with `Corrupt` when: the magic/version is wrong, the reserved
bytes are non-zero, the block size or block count is inconsistent with the
declared length, the total length differs from the header's expectation, any
block fails authentication, or (convergent only) the recomputed plaintext hash
differs from the header. `UnsupportedVersion` is returned for a future envelope
version. A truncated object is a length mismatch, never a short plaintext.

The plaintext is never partially returned by `get`, and a `get_range` that
touches a damaged block fails for that range without decrypting the rest.

Bounds: one object's plaintext ≤ `MAX_PLAINTEXT_BYTES` (about 256 TiB, the
`u32` block-count bound × 64 KiB); a blob above the inner plane's
`max_object_bytes` is refused by the inner plane.

## 3. Keys, rotation and revocation

`weft.encryption.keyfile.v1`:

```json
{
  "schema": "weft.encryption.keyfile.v1",
  "mode": "convergent",
  "active": "<32 hex characters>",
  "keys": [
    {"id": "<32 hex>", "epoch": 1, "secret": "<64 hex>", "revoked": false},
    {"id": "<32 hex>", "epoch": 2, "secret": "<64 hex>", "revoked": false}
  ]
}
```

* `id` **must** equal `BLAKE3("weft/enc/key-id/v1", secret)[..16]`; a key file
  with a mismatched id is `NonCanonical`, a repeated id is `NonCanonical`, an
  empty key set or an `active` id absent from `keys` is `InvalidArgument`.
* **Rotation is additive.** `active` names the key new publications use; every
  object header names the key that sealed it, so a key rotated away from
  `active` keeps every object it wrote readable for as long as it stays in
  `keys`. Rotating is: add the new key, set `active`, keep the old row.
* **Revocation.** `revoked: true` makes the key refuse to decrypt
  (`Unauthorized`, message names the key id). A key file whose *active* key is
  revoked is refused at load (`InvalidArgument`), because a writable store
  could otherwise publish unreadable objects. Revoking a key is therefore a
  destructive retention decision: objects sealed with it become unreadable
  even if the secret is still present.
* `Debug` and `describe()` never print secret bytes; only ids, epochs and
  counts.

`Store::open` treats an envelope refusal during state replay as a
configuration error: if the newest published state cannot be opened because the
key set is wrong or a key is revoked, the open fails with `Unauthorized`
(`the encryption key set cannot open this store's published state …`) rather
than falling back to an older generation whose manifests happen to decrypt. A
**fresh** store with the wrong key is not detected at open (there is nothing to
decrypt yet); the first read fails.

## 4. The dedup tradeoff, and the leak that is not hidden

Two policies, selectable per store open:

| policy | per-object key material | ciphertext of equal plaintext | physical dedup of equal chunks | equality visible to |
|---|---|---|---|---|
| `convergent` | plaintext content hash | **identical** | preserved (one object) | anyone who can read bodies **or** names |
| `random` | fresh 12-byte system nonce per publication | **different** | not at the ciphertext layer | anyone who can read **names** |

The store's blob name is the plaintext content hash
(`H("weft/chunk/v1", plaintext)`), a frozen v1 property. Consequences that the
measurement confirms and this document will not hide:

1. **Names leak equality in both modes.** Two tenants storing the same chunk
   produce the same object path. The court measured exactly the same object
   count and physical footprint for plain, convergent and random on the same
   fixture (see the table in §7). `random` removes ciphertext equality; it does
   **not** remove address equality.
2. **The store's own deduplication still works in both modes** because it
   deduplicates by that name. So "random loses dedup" is true of the ciphertext
   bytes, not of the store's object layer.
3. **Re-publishing the same name and plaintext is idempotent under convergent
   and a conflict under random** (`Exists` from the create-once plane), because
   the ciphertext differs. The store never gets there — it skips a blob whose
   name it already has — but a different caller that retries a publication must
   know this.
4. **Do not claim confidentiality and cross-tenant dedup at the same time.**
   Convergent mode gives physical dedup and reveals equality of the plaintext
   (and, via the header's plaintext hash, gives a verifier a direct
   confirm-or-deny oracle). Random mode gives up ciphertext dedup and still
   reveals equality through the name. A store that wanted neither would need
   name-keyed encryption addresses, which is a different (and unfrozen) store
   format, not a switch on this envelope.

The honest recommendation: use `convergent` when deduplication is the point and
the tenants are inside one trust domain; use `random` when ciphertext equality
itself is the threat and you accept the same storage cost. Neither choice hides
*which* content exists from an observer of the object namespace.

## 5. Threat model

Protects:

* bytes at rest in a blob store, a backup, a detached disk or a stolen volume,
  **when the key file is not on that volume**;
* against an attacker who can read objects but not guess the plaintext
  (convergent mode's plaintext-hash oracle applies only to an attacker who
  already has a candidate plaintext);
* against silent modification or truncation of a blob: AEAD plus the
  plaintext-hash check refuse the object.

Does **not** protect:

* an attacker who holds the key material — there is no per-tenant key
  separation and no claim of confidentiality against a key holder;
* the control plane: commit records carry paths, object records, relations,
  sizes and the full operation log in plaintext. Anyone who can read
  `commits/` learns the namespace, the file sizes and the graph;
* object sizes and counts (the envelope adds a fixed, predictable overhead);
* access patterns (no padding, no cover traffic, no nonce-based privacy);
* against rollback of the *control* plane (the envelope has no anti-rollback,
  see `docs/REVOCATION-EPOCH.md` for the revocation epoch, which is a different
  mechanism).

## 6. Operational procedure

```bash
# 1. create a key file (id must be derived; the CLI computes it for inline keys)
weftfs info --encryption-key-hex <64 hex> --encryption-mode convergent --store ./s
# 2. write through the envelope
weftfs write /a.bin --data ... --encryption-key-file keys.json --store ./s
# 3. rotate: add key 2, set active to key 2, keep key 1
# 4. verify still reads every object (with the whole key set present)
weftfs verify --full --encryption-key-file keys.json --store ./s
```

`weftfs info --json` reports the plane as `encrypted-convergent-(local)` and
the backend diagnostics include `encryption:<mode>:active=<id>:keys=<n>`. The
`--encryption-*` flags are global, so every store command that touches blobs
must be given the same key file; a command that omits it reads ciphertext and
fails closed.

## 7. Measured cost

Bench court (`weft/bench-court:1`, 3 GiB / 4 vCPU / 1024 pids), seven
interleaved repetitions of the same fixture (one large file, a small-file set
and an identical duplicate pair; the receipt records the exact sizes and the
logical-byte total). Wall and CPU are medians; CPU is `getrusage` user+sys for
the phase. Reading is warm page cache, so the read row isolates CPU, not media.

<!-- BEGIN GENERATED:encryption-table (scripts/perf_numbers.py) -->
| row | write median ms | read median ms | write CPU ms | read CPU ms | footprint B | objects |
|---|---:|---:|---:|---:|---:|---:|
| `plain` | 52.7 | 39.7 | 51.7 | 39.6 | 72,574,425 | 1,216 |
| `convergent` | 95.3 | 78.9 | 94.4 | 78.2 | 72,739,961 | 1,216 |
| `random` | 94.0 | 65.4 | 92.2 | 65.0 | 72,739,961 | 1,216 |
<!-- END GENERATED:encryption-table -->

<!-- BEGIN GENERATED:encryption-headline (scripts/perf_numbers.py) -->
On this fixture the convergent envelope costs **1.81x** the plain
write wall and **1.99x** the plain read wall; the random envelope
costs **1.78x** and **1.65x**. Physical footprint
rises by **0.228 %** in both modes and the object count is the
same in all three arms (**1,216**), which is the leak: the blob name
is the plaintext hash whichever mode is chosen. The plane itself seals at
**1,568 MiB/s** and opens at **2,127 MiB/s**
(convergent) on one core of the bench court; seven interleaved repetitions,
medians, warm page cache. Source: `bench/receipts/encryption-20261114T235500Z.json`
(stamp 20261114T235500Z, revision 7f85d5cfb5ca).
<!-- END GENERATED:encryption-headline -->

Interpretation:

* the envelope is **CPU**, not I/O: sealing runs at about the rate in the plane
  rows above and opening is faster than sealing, so the extra wall time is the
  extra CPU the store spends, not extra requests;
* `convergent` costs more to read than `random` because it re-hashes the
  decrypted plaintext and compares it with the header before returning;
* the footprint difference is the fixed header plus one tag per block, applied
  to every object *and* to every manifest.

Raw rows per repetition, the plane microbench, the equality/republish checks
and the limits are in `bench/receipts/encryption-20261114T235500Z.json`
(+ `.log`) and `bench/RESULTS-encryption.md`.

## 8. Failure classes

| condition | class |
|---|---|
| missing/unknown key id in a header | `Corrupt` |
| revoked key | `Unauthorized` |
| key file schema/id/duplicate/active problems | `Malformed`, `NonCanonical`, `InvalidArgument` |
| flipped ciphertext byte, flipped header field, truncation, wrong total length | `Corrupt` |
| unknown envelope version | `UnsupportedVersion` |
| reserved header bytes non-zero | `NonCanonical` |
| newest state unopenable at open time | `Unauthorized` (refuses a stale head) |
| object larger than the inner plane's bound | `LimitExceeded` (inner plane) |

## 9. What this does not do

* No Go implementation, no Python implementation, no FUSE-specific handling.
* No per-tenant keys, no key escrow, no HSM/KMS integration, no envelope-key
  wrapping; the key file is a plaintext JSON file the operator protects.
* No authenticated control plane, no encrypted commit records, no encrypted
  paths or relation names.
* No anti-rollback, no key-use audit trail, no key rotation receipts.
* No claim of confidentiality against a co-tenant who holds the key, and no
  claim that convergent and random differ in what the *namespace* reveals.
* No media-level, power-loss, fleet or cloud measurement; the numbers are one
  host, one container, tmpfs, warm cache.
* ChaCha20-Poly1305 and BLAKE3 are used through `ring` and `blake3`; no
  independent cryptographic review of this composition exists.
