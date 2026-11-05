# Write your own WeftCodec v1 decoder

This is a practical, normative-pointer guide to decoding WFT1 frames. It does
not replace the specification; it tells you which spec section is authoritative
for each step and the exact order that determines the error class.

Read alongside:

- [`../spec/WEFTCODEC-FORMAT.md`](../spec/WEFTCODEC-FORMAT.md) — frame layout,
  modes, WCB, limits, error taxonomy.
- [`../spec/WEFT-AI-CODEC.md`](../spec/WEFT-AI-CODEC.md) — modes 6–14.
- [`../spec/WEFT-SELECTORS.md`](../spec/WEFT-SELECTORS.md) — selector identity.
- [`../spec/WEFT-CONTEXT-ROOT.md`](../spec/WEFT-CONTEXT-ROOT.md) — Context Roots.
- [`../spec/WEFT-CAPABILITIES.md`](../spec/WEFT-CAPABILITIES.md) — envelopes.
- [`CONFORMANCE.md`](CONFORMANCE.md) — vectors and the runner contract.
- [`WRITE-YOUR-OWN-DECODER.md`](WRITE-YOUR-OWN-DECODER.md) — the practical decoder guide.

You do not need to read the Rust or Go sources. Every value a decoder needs is
in the frame, the frozen vector, or the normative text.

---

## 1. Frozen, implemented, experimental

| Area | Status | Pointer |
|---|---|---|
| Header/trailer, RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED | **Frozen v1.0** | FORMAT §2–§8 |
| WCB canonical bytes | **Frozen v1.0** | FORMAT §8 |
| Selector canonicalization and id | **Frozen v1.0** | SELECTORS §2–§4 |
| Context Root, causal closure, capability envelope records | **Frozen v1** (identity records) | CONTEXT-ROOT, CAPABILITIES |
| TENSOR (6), TOKEN_SEQUENCE (7), QUANTIZED_TENSOR (8), FLOAT_TENSOR (9) | **Implemented and frozen for the fields listed** | AI-CODEC §2–§5 |
| TENSOR_SHARDED (10), WEIGHT_DELTA (11), KV_CACHE (12), ACTIVATION_STREAM (13), SEMANTIC_DELTA (14) | **Implemented and frozen for the fields listed** | AI-CODEC §11–§15 |
| Mode values 15–127 | **Reserved**; reject `UnsupportedMode` | FORMAT §3, AI-CODEC §6 |
| Mode values 128–255 | **Experimental**; must be rejected for persistence | FORMAT §3 |
| Self-contained archive layout (`SELF_CONTAINED`) | **Experimental**; v1 readers reject it | FORMAT §7 |
| WeftFS v2 representation manifests (`weft.manifest.v2`) | **Outside WFT1**; not a frame mode | WEFT-MODEL |
| Any speculative AI mode with no wire value | **Conjectured/specified only** | AI-CODEC §6 |

A decoder that implements modes 0–5 and WCB is already useful and conformant for
that subset. It MUST reject modes it does not implement with `UnsupportedMode`
and MUST NOT treat an unknown mode's payload as opaque bytes.

---

## 2. Hashing

One construction in v1:

```
H(domain, data) = BLAKE3-256( u32le(len(domain)) || domain || data )
```

- Domain strings are ASCII, versioned, and length-prefixed (FORMAT §1.1).
- Plaintext content identity is `H("weft/chunk/v1", plaintext)`.
- Every dependency-table hash and every chunk-table hash uses the same
  `weft/chunk/v1` domain over the bytes it identifies.
- Canonical structured *value* identity is `H("weft/structured/v1", wcb_bytes)`.
  Note that a STRUCTURED frame's header `content_hash` is still
  `H("weft/chunk/v1", body)`, per FORMAT §2.1.
- Selector, Context Root, causal closure, capability, checkpoint, and payload
  bindings use their own domains listed in FORMAT §1.1 and the respective
  specs.

The hash is an integrity identifier, never a MAC, signature, or authorization
decision.

---

## 3. Frame bytes

```
header (96) || dependency table (dep_count * 48) || chunk table (chunk_count * 48)
            || body (body_len) || trailer CRC32C (4)
```

Header offsets are normative in FORMAT §2.1. The fields you must read:

| Offset | Field |
|---:|---|
| 0 | magic `WFT1` |
| 4, 5 | major, minor |
| 6 | domain (`0` payload, `1` structured) |
| 7 | mode |
| 8 | flags (u16 LE) |
| 12 | decoded_len (u64 LE) |
| 20 | body_len (u64 LE) |
| 28 | dep_count (u32 LE) |
| 32 | chunk_count (u32 LE) |
| 36 | features (u16 LE) |
| 40 | content_hash (32 bytes) |
| 72 | header CRC32C (over bytes `[0,72)` + `[76,96)`) |
| 76 | table_len (u64 LE) |
| 84, 88 | reserved, must be zero |

Dependency entries: hash (32), decoded_len (8), domain (1), flags (1),
reserved (2), codec_hint (4). Chunk entries: encoded_off (8), encoded_len (4),
decoded_len (4), hash (32). See FORMAT §5–§6.

---

## 4. Exact order of checks (this determines the class)

This is the order a conformant decoder must use. Earlier checks win when a
frame is wrong in more than one way.

1. **Minimum length and frame bound.** `len < 100` is `Malformed`; `len >
   max_frame_bytes` is `LimitExceeded`. (FORMAT §2, §10)
2. **Magic.** Not `WFT1` → `Malformed`.
3. **Format version.** Major or minor not supported → `UnsupportedVersion`.
4. **Header CRC32C.** Verify before trusting any length field. Mismatch →
   `Corrupt`.
5. **Reserved header fields.** Any non-zero → `Malformed`.
6. **Domain.** Greater than `1` → `Malformed`.
7. **Critical flags and features.** Unknown critical bits, `ORIGINAL_HASH`,
   or `SELF_CONTAINED` → `UnsupportedFeature`.
8. **Limits before allocation.** `decoded_len`, `dep_count`, `chunk_count`
   against configured bounds → `LimitExceeded`.
9. **Table length arithmetic.** `table_len != dep_count*48 +
   chunk_count*48` → `Malformed`; overflow → `Malformed`.
10. **Total length arithmetic.** `len != 96 + table_len + body_len + 4` →
    `Malformed`.
11. **Flag/table consistency.** `CHUNKED` without chunks, chunks without
    `CHUNKED`, `CHUNK_HASHES` without `CHUNKED`, dependencies without the
    `DEPENDENCIES` feature → `Malformed`.
12. **Trailer CRC32C.** Verify over `[0, len-4)`. Mismatch → `Corrupt`.
    This is deliberately after the length checks: a frame with both a bad
    trailer and an inconsistent `table_len` is `Malformed`, not `Corrupt`.
13. **Dependency entries.** Flag bits, `REQUIRED`, reserved zero, domain range,
    dependency length bound → `Malformed` / `LimitExceeded`.
14. **Chunk entries.** Contiguous extents with no gap or overlap, non-zero
    `encoded_len`, extents ending exactly at `body_len`, decoded lengths summing
    to `decoded_len` → `Malformed`.
15. **Mode rules.** Domain, dependency/chunk counts, `DICT_RAW`, feature and
    flag combinations per FORMAT §3 → `Malformed` / `UnsupportedFeature`.
16. **Decode by mode.** Dependency resolution failures are
    `DependencyMissing` / `DependencyDenied` / `DependencyMismatch`; graph
    cycles are `DependencyCycle`; WCB violations are `NonCanonical`; zstd
    framing violations are `Malformed` or `Corrupt` per FORMAT §3.
17. **Decoded length.** Actual length must equal `decoded_len` → `Corrupt`.
18. **Content hash.** `H("weft/chunk/v1", decoded) == header.content_hash` →
    `Corrupt` otherwise.

Never allocate based on an unverified length field. The limit checks in step 8
exist so that a 10 GiB declaration is rejected before allocation.

---

## 5. WCB (canonical structured bytes)

WCB is a small deterministic encoding, normative in FORMAT §8.

Tags: `00` null, `01` false, `02` true, `03` u64, `04` i64 (zigzag), `05` text,
`06` bytes, `07` array, `08` map, `09` timestamp (zigzag i64 nanoseconds).
`0A..FF` → `NonCanonical`.

Rules that trip implementers:

- Varints must be minimal. `0x80 0x00` is non-minimal, not just unusual.
- Map keys are text in v1 and sorted by the **full encoded key bytes** (tag +
  varint length + bytes) as unsigned bytes. Sorting by decoded string, or by
  the text bytes alone, is wrong.
- Duplicate encoded keys → `NonCanonical`.
- No trailing bytes after the value.
- Depth ≤ configured `max_structured_depth` (default 64), items ≤
  `max_structured_items`, strings ≤ `max_string_bytes`.
- Text must be valid UTF-8.
- Signed integers use zigzag on 64-bit two's-complement.
- Missing and null are distinct.

Validate canonically by decoding and re-encoding; the bytes must be identical.
This catches non-minimal varints, wrong map order, and non-canonical spellings
in one check.

---

## 6. Modes

| Mode | Name | What to do | Spec |
|---:|---|---|---|
| 0 | RAW | body is plaintext; chunked body uses literal chunks | FORMAT §3, §6 |
| 1 | ZSTD | one complete zstd frame; empty body is `Malformed`; declared content size must equal `decoded_len` | FORMAT §3 |
| 2 | REFERENCE | dependency 0 is the plaintext; body empty | FORMAT §3, §5 |
| 3 | DELTA | COPY/INSERT op stream over dependency 0 | FORMAT §4 |
| 4 | DICTIONARY | zstd body using dependency 0 as a raw dictionary | FORMAT §3, §5 |
| 5 | STRUCTURED | canonical WCB body; domain must be 1 | FORMAT §3, §8 |
| 6 | TENSOR | descriptor + element payload | AI-CODEC §2 |
| 7 | TOKEN_SEQUENCE | descriptor + delta-encoded tokens | AI-CODEC §3 |
| 8 | QUANTIZED_TENSOR | descriptor + packed affine words | AI-CODEC §4 |
| 9 | FLOAT_TENSOR | descriptor + exponent/sign-mantissa planes | AI-CODEC §5 |
| 10 | TENSOR_SHARDED | one shard frame; shard sets share a `layout_root` | AI-CODEC §11 |
| 11 | WEIGHT_DELTA | word delta against a base tensor frame | AI-CODEC §12 |
| 12 | KV_CACHE | one cache page per frame | AI-CODEC §13 |
| 13 | ACTIVATION_STREAM | one chunk per frame; per-chunk digest roots | AI-CODEC §14 |
| 14 | SEMANTIC_DELTA | exact delta plus a verified semantic policy | AI-CODEC §15 |

Modes 11 and 14 take a base frame. Resolve it through the dependency table; do
not accept an inferred substitute. Mode 14 recomputes the base tensor's
semantic root from the resolved bytes and fails closed on any mismatch.

---

## 7. Limits

Defaults are normative in FORMAT §10. Enforce each applicable bound **before**
allocating:

`max_frame_bytes` 1 GiB, `max_decoded_bytes` 4 GiB, `max_dependency_depth` 8,
`max_dependency_count` 64, `max_total_fetched_bytes` 8 GiB, `max_chunks`
1,048,576, `max_dictionary_bytes` 64 MiB, `max_structured_depth` 64,
`max_structured_items` 1,000,000, `max_string_bytes` 256 MiB, `max_delta_ops`
16,777,216, `max_tensor_elements` 2^32, `max_tensor_rank` 8, `max_tokens`
2^24.

A configured bound may only be lowered, never raised to bypass a vector.

---

## 8. Failure classes

`Malformed`, `Corrupt`, `UnsupportedVersion`, `UnsupportedMode`,
`UnsupportedFeature`, `LimitExceeded`, `DependencyMissing`,
`DependencyDenied`, `DependencyMismatch`, `DependencyCycle`, `NonCanonical`,
`Io`.

Only the class is interoperable. Message text is human-readable and may change.
See FORMAT §11.

---

## 9. Decoder skeleton

Fifty lines, deliberately close to the order in §4. Error constructors are
omitted.

```python
def decode_frame(buf, limits, resolve):
    if len(buf) < 100:                    return malformed("short frame")
    if len(buf) > limits.max_frame:       return limit("frame too large")
    if buf[0:4] != b"WFT1":               return malformed("magic")
    if buf[4] != 1 or buf[5] != 0:        return unsupported_version()

    hdr_crc = crc32c(buf[0:72] + buf[76:96])
    if hdr_crc != u32le(buf, 72):         return corrupt("header crc")

    domain   = buf[6]
    mode     = buf[7]
    flags    = u16le(buf, 8)
    decoded  = u64le(buf, 12)
    body_len = u64le(buf, 20)
    dep_n    = u32le(buf, 28)
    chunk_n  = u32le(buf, 32)
    features = u16le(buf, 36)
    content  = buf[40:72]
    table_len = u64le(buf, 76)

    if u16le(buf, 38) or u32le(buf, 84) or u64le(buf, 88):
        return malformed("reserved header")
    if domain > 1:                        return malformed("domain")
    if flags & ~0x000F:                   return unsupported_feature()
    if flags & 0x0008:                    return unsupported_feature()
    if flags & 0x0002:                    return unsupported_feature()
    if features & ~0x0003:                return unsupported_feature()
    if decoded > limits.max_decoded:      return limit()
    if dep_n > limits.max_dep:            return limit()
    if chunk_n > limits.max_chunk:        return limit()

    expect_table = dep_n * 48 + chunk_n * 48
    if table_len != expect_table:         return malformed("table_len")
    if len(buf) != 96 + table_len + body_len + 4:
        return malformed("total length")
    if flags & 0x0001:
        if chunk_n == 0 or not (features & 0x0002):
            return malformed("chunk flags")
    elif chunk_n != 0 or (features & 0x0002):
        return malformed("chunk flags")
    if dep_n and not (features & 0x0001):
        return malformed("dependency feature")

    if crc32c(buf[:-4]) != u32le(buf, len(buf) - 4):
        return corrupt("trailer crc")

    deps, chunks, body = parse_tables(buf, table_len, body_len)
    err = validate_mode_rules(mode, domain, flags, features, deps, chunks)
    if err: return err

    try:
        plain = decode_by_mode(mode, body, deps, resolve, limits)
    except WeftError as e:
        return e
    if len(plain) != decoded:             return corrupt("decoded length")
    if hash_domain("weft/chunk/v1", plain) != content:
        return corrupt("content hash")
    return plain
```

`decode_by_mode` dispatches on `mode` exactly as §6 lists. `parse_tables`
performs the dependency and chunk field checks from steps 13–14.

---

## 10. Testing your decoder

1. Read every vector in `WeftCodec/conformance/v1/` (or the manifest).
2. For each `codec` vector, assert the decoded bytes and the declared
   `plaintext_hash`/`content_hash`.
3. For each reject, assert the exact class, not the message.
4. Replay `structured`, `selectors`, `context`, `causal`, `tensor`,
   `capability`, and `checkpoint` vectors according to their kinds.
5. Run the third-party runner against your adapter:

```sh
scripts/run-conformance.sh ./your-adapter
```

6. Commit your adapter and the receipt; do not edit the vectors.

Skips are not passes. A decoder that implements only modes 0–5 should report
explicit skips for modes 6–14, not fabricated successes.
