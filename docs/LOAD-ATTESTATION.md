# Signed load attestations (`weft.load-attestation.v1`)

Subphase R12, increment 3. A load attestation is what a replica emits after it
has decoded part of a checkpoint. It is a **canonical WCB record with an Ed25519
signature**, and it exists so a third party can answer two separate questions:

1. *Did this replica really load these bytes, and were the group roots covering
   them verified at load time?* — answered by re-reading the recorded ranges
   from a store and re-hashing them.
2. *Did the holder of this key produce this record, unaltered?* — answered by
   the signature alone.

Neither question implies the other. A signature over a false measurement still
verifies; a true measurement with a valid re-read is not attributable without
the signature.

## Shape

```
weft.load-attestation.v1 {
  schema: "weft.load-attestation.v1",
  checkpoint: <name>,
  checkpoint_root: <hash32>,      // = manifest_root
  manifest_root: <hash32>,
  snapshot: <hash32|null>,
  replica: <label>,
  verifier: { principal: <label>, public_key: <hash32> },
  issued_at: <timestamp ns>,
  tensors: [                      // strictly sorted by name
    { name, dtype, shape, logical_root, element_bytes,
      status: "loaded" | "unverified",
      bytes_loaded,
      ranges: [ { offset, length, digest } ],   // sorted, non-overlapping
      groups: [ { object, content_root, logical_root, axis, index, byte_len } ],
      units_verified,
      full_root_verified,
      reason: <string|null> }
  ],
  totals: { loaded_tensors, unverified_tensors, loaded_bytes, unverified_bytes },
  load_receipt_root: <hash32|null>,
  signature: { algorithm: "ed25519", public_key: <32 bytes>, signature: <64 bytes> } | null
}
```

* `range.digest = H("weft/load/range/v1", range_bytes)`.
* `load_receipt_root` is the root of the shipped `weft.load.receipt.v1` when the
  loader produced one; it is not required.
* The signature covers the canonical bytes of the record **without** the
  `signature` key, using the same Ed25519 construction as the retention binding
  and the receipt bundle (`weftcodec::capability::sign_bytes`).
* `AttestationId = H("weft/load/attestation/v1", canonical bytes including the
  signature)`.

## What "verified" means per entry

* `status: loaded` with `full_root_verified: true` — the loader read the whole
  tensor through the shipped restore path, which verifies every shard/group root
  and recomputes the whole-tensor `LogicalTensorRoot`.
* `status: loaded` with `full_root_verified: false` — the loader read a row
  region. It verified the group roots covering that region; it did **not**
  prove the rest of the tensor. The record says so instead of implying a whole
  root.
* `status: unverified` with a `reason` — the loader did not read this tensor.
  It is listed, counted in `totals.unverified_tensors`, and never counted as
  verified.

`totals.unverified_bytes` is every byte the record does **not** attest: the
whole tensor when it was not read, and the unread remainder when only a region
was read.

A record is rejected on decode if an unverified entry has no reason, if a loaded
entry has no ranges or groups, if ranges overlap or are unsorted, if entries are
not strictly sorted by name, if the totals disagree with the entries, or if
`checkpoint_root != manifest_root`.

## Verification

`verify_against_store` (Rust) performs, in order:

1. the signature (and, when configured, trust-set membership);
2. a fresh manifest read, required to produce the recorded `manifest_root`;
3. for every loaded entry, a re-read of exactly the recorded range(s) and a
   comparison of `H("weft/load/range/v1", bytes)` with the recorded digest;
4. a check that a claimed whole-tensor root is actually re-proved by the re-read.

Any mismatch is a typed `Corrupt` error. A modified receipt fails step 1; a
modified store blob fails step 3 (the store's own group hashing catches it
first). `attest-verify` in WeftFS-Go (`cmd/attest-verify`) independently checks
steps 1 and the record shape — canonical encoding, key sets, sorted entries,
loaded/unverified consistency — using `crypto/ed25519`. It does **not** re-read
tensor bytes: WeftFS-Go has no `weft.checkpoint.v1` reader, and the command's
output says `byte_replay=not_performed`.

## Losses

1. **Lazy verification cannot prove bytes that were never read.** A region load
   proves the groups covering the region, not the whole tensor; the record says
   `full_root_verified: false`. Unloaded tensors are listed as unverified.
2. **The signature proves origin, not truth.** A signer can attest a wrong
   measurement; only the store re-read tests the measurement.
3. **Cost.** Verification re-reads and re-hashes every recorded byte, so it
   costs a second pass over the loaded set. The court reports µs/MiB and µs per
   recorded range.
4. **Row-aligned regions only.** A partial range must cover whole rows of a
   rank-2 tensor so the recorded logical interval is contiguous; other shapes
   are refused rather than approximated.
5. The Go verifier is a signature/record verifier only; byte replay is Rust-only.
