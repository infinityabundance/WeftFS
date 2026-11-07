# KV-cache governance

**Status:** implemented (Rust store; no Go parity, no wire change). Court:
`bench/scripts/kv_capability_court.sh`. Tests: `tests/kv_capability.rs`
(13/13). Receipt: `bench/receipts/kv-capability-20261107T152958Z.json`.

A `KV_CACHE` page (codec mode 12) is a *page of inference state*, not a
checkpoint: it belongs to one tenant, one model version, one session, and one
token range. Treating it as an ordinary file means any principal that can read
the file can read another tenant's attention state. This document defines the
governance surface that prevents that, using only shipped machinery: the frozen
mode-12 frame, canonical WCB, and the authenticated capability envelope.

## 1. The artifact

```
object (file)                    bytes = the frozen mode-12 frame (WFT1)
├── frame descriptor             model, layer, session, page_index/page_count,
│                                seq_start/seq_end, dtype, head geometry, shape,
│                                page_bytes   (normative, unchanged)
└── reserved attributes
    ├── weft.kv.descriptor       canonical WCB, schema weft.kv.capability.v1
    ├── weft.kv.descriptor_id    H("weft/kv/capability/v1", descriptor bytes)
    ├── weft.kv.tenant           tenant label
    ├── weft.kv.model            model version the page was produced by
    ├── weft.kv.session          inference session id
    └── weft.kv.page_bytes       exact logical page length
```

`weft.kv.capability.v1` is the canonical descriptor:

| Field | Type | Meaning |
|---|---|---|
| `schema` | text | `weft.kv.capability.v1` |
| `tenant` | text | tenant that owns the page |
| `model` | text | model version (checkpoint identity) |
| `session` | text | inference session id |
| `layer` | u64 | transformer layer |
| `page_index`, `page_count` | u64 | page position |
| `seq_start`, `seq_end` | u64 | token range |
| `dtype` | text | `bf16`, `fp16`, or `fp32` |
| `head_count`, `head_dim` | u64 | head geometry |
| `shape` | [u64; 3] | `[page_rows, head_count, head_dim]` (mode-12 canonical) |
| `page_bytes` | u64 | exact logical page length |

Unknown keys, missing keys, non-minimal varints, and any non-canonical encoding
are rejected. `DescriptorId = H("weft/kv/capability/v1", WCB(descriptor))`, the
same domain-separated hash construction as every other Weft identity.

The mode-12 **frame is untouched**. Tenant is the only field the frozen frame
does not carry, so it lives in the canonical descriptor and in reserved
attributes. There is no "descriptor v2" of the frame and no new mode.

## 2. The grant

A grant is the shipped `weft.capability.envelope.v1`, signed with Ed25519:

* `tenant` = the KV tenant;
* `namespaces` contains `kv.<tenant>` (coarse tenant scope);
* `roots` contains the **descriptor id** (precise page scope);
* `expires_at_ns` is the validity window;
* `revocation_id` is the revocation handle;
* `grantee` is bound at verification.

`roots` is a hash list and is used here as an *artifact allowlist*, not as a
Context Root list. That is the only new interpretation this document adds, and
it is why a grant is page-scoped without changing the envelope format.

The tenant check reads `CapabilityScope::principal`, which
`CapabilityEnvelope::capability_scope()` sets from the envelope's `tenant` field,
so the two are equal by construction; the read path tests the scope value
because that is the frozen enforcement type.

## 3. Enforcement order (normative)

`Store::read_kv_page(id, principal, now_ns)`:

1. object record and descriptor present and internally consistent;
2. envelope present, signature valid (verified before the principal exists), not
   expired, same tenant, `kv.<tenant>` granted, descriptor id in `roots`;
3. POSIX mode and `weft.acl.read` authorization (a capability can only narrow,
   never widen, permissions);
4. read the bytes through the authorized read token (cache/single-flight are
   never consulted before authorization);
5. decode the frame, re-derive the descriptor from the frame metadata, compare
   every field;
6. return the decoded page.

**Every failure in steps 1–4 returns the same `Unauthorized` error with the same
text: `kv page is not available to this principal`.** That includes a
non-existent object, a foreign tenant, a missing namespace, a missing root, an
expired grant, a torn descriptor, and a POSIX denial. Steps 5–6 can only fail
for a reader that is already authorized, and a descriptor/frame disagreement is
`Corrupt` — an integrity report, not a disclosure.

A content hash is not a credential: the tests prove that knowing both the
`ContentId` and the whole-file digest still yields the identical denial.

## 4. Measured cost

2,000 samples per path, warm in-process store, one warm-up read, fixed clock,
`weft/rust-court:1` under the standard limits (`bench/receipts/kv-capability-20261107T152958Z.json`).

| Path | 1 KiB page p50 / p99 | 64 KiB page p50 / p99 |
|---|---|---|
| `authorize_decode_control` (no capability work) | 18.0 / 21.2 µs | 188.7 / 204.1 µs |
| `cached` (verified grant in hand) | **33.0 / 42.1 µs** | **203.9 / 215.7 µs** |
| `full` (load envelope + verify each read) | 81.2 / 96.6 µs | 257.2 / 274.9 µs |
| `denied` (unauthorized) | 6.4 / 8.7 µs | 6.5 / 6.9 µs |
| `object_read_control` (frame bytes, no decode) | 7.5 / 8.9 µs | 28.7 / 33.9 µs |
| envelope load + Ed25519 verify alone | 47.5 / 52.6 µs | 49.9 / 54.7 µs |

**The capability check is a real, measurable, and fixed cost.** With a verified
grant in hand it adds ≈ 15 µs per read regardless of page size: **+83 % on a
1 KiB page and +8 % on a 64 KiB page**. Re-verifying the Ed25519 signature on
every read adds another ≈ 48 µs, which dominates the 1 KiB case (2.5× the
decode-only control). The denied path is the cheapest of all (6.4 µs): a denial
returns before any decode.

Recommendation implied by the numbers: verify a grant once per session (or per
lease) and carry the `VerifiedEnvelope` in the principal; do not re-verify per
page. A page smaller than ~4 KiB is dominated by governance, not payload.

## 5. What this does **not** claim

* Not constant-time. The denial text is identical and the outcomes are
  indistinguishable, but a reader who can measure nanoseconds may distinguish
  "object absent" from "object present, grant wrong" by work performed. The
  tests assert text, class, and `Debug` equality — not timing equality.
* No key management. Grant signing seeds are the caller's responsibility; the
  court uses fixed test seeds.
* No revocation service. Revocation is the envelope's `revocation_id` checked
  against a caller-supplied set; the store does not gossip revocations.
* No cross-store or cross-tenant index. `kv.<tenant>` is a namespace string in
  the grant; nothing prevents two tenants using the same label in different
  stores.
* No Go parity, no FUSE read path for KV pages, no online re-encryption of a
  page, and no per-page data key. The page bytes are protected by authorization,
  not encryption at rest.
* 13 unit tests and one benchmark process; single host, warm cache, synthetic
  page bytes. No production, fleet, or multi-tenant deployment claim.

## 6. Reproduce

```sh
scripts/docker-court.sh rust "cd /work && bash bench/scripts/kv_capability_court.sh"
```

The court runs `cargo test --release --test kv_capability` and then the
benchmark; both must pass before the receipt is written.
