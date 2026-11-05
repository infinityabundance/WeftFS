# WeftFS v0.6.0 — authenticated capability envelopes and cross-tenant isolation

Research preview. v0.6.0 adds the D2 layer: an Ed25519-signed capability
envelope that a principal can present as an authenticated grant, offline
verification against an explicit revocation set, enforcement layered on POSIX
and explicit capabilities, a two-tenant isolation court, and byte-identical
signed blobs in Rust and Go.

This is an **additive** release. The frozen v1 on-disk formats, frame bytes,
`weft.capability.v1` records, selector grammar, manifests, and default `raw`
representation are unchanged. A bare capability hash is still only an
identifier, never a credential.

Everything below is measured in the Docker courts with the hard limits in
`AGENTS.md`. No claim is a fleet, production, timing-side-channel, or
hardware-independent claim. Every limit we know about is stated.

## Version and artifacts

```toml
weftcodec = "0.6.0"
weftfs = "0.6.0"
```

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.6.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.6.0
docker pull ghcr.io/infinityabundance/weftfs:v0.6.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.6.0
```

- Rust: `weftfs` 0.6.0 and `weftcodec` 0.6.0 (both `Cargo.toml` and
  `Cargo.lock`; the parent depends on `weftcodec = "0.6.0"`). The version
  bump changed only version literals; no dependency or format byte changed.
- Python remains `python/dist/weftcodec-0.5.0-py3-none-any.whl` (27,070 bytes,
  sha256 `cc0afe3b9b68490c5ad29a9fe54b830bf6266fd704e9ad962a674cdb4a2781f1`).
  The wheel is **not renamed** and still carries version 0.5.0, because the
  Python reader has no capability-envelope implementation and no Python code
  changed after v0.5.0. **Not published to PyPI** (no token).
- Images are published by each repository's `container.yml` on the release tag.
  FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and store-only
  commands do not. See `docs/containers.md`.

## Authenticated capability envelopes (`weft.capability.envelope.v1`)

`spec/WEFT-CAPABILITIES.md` §9 defines an additive signed grant. The canonical
WCB map always carries exactly these keys: `schema`, `grantor`, `grantee`,
`tenant`, `roots`, `namespaces`, `predicates`, `expires_at_ns`, `budget`,
`revocation_id`, `issued_at_ns`, and `nonce`. Unknown or missing keys are
rejected; grant lists are sorted, unique, and bounded; `roots` and `namespaces`
must not both be empty; `expires_at_ns` is inclusive and cannot precede
`issued_at_ns`; `grantor`, `grantee`, `revocation_id`, and `nonce` cannot be
all-zero.

Identities are domain-separated:

```text
EnvelopeId = H("weft/capability/envelope/v1", WCB(envelope))
sign_input = H("weft/capability/sign/v1",     WCB(envelope))
signature  = Ed25519.sign(grantor_private_key, sign_input)   # RFC 8032
Blob       = WCB({ "envelope": bytes(WCB(envelope)), "signature": bytes(64) })
BlobId     = H("weft/capability/blob/v1",     WCB(Blob))
```

Only the grantor's seed is secret. The envelope, signature, `EnvelopeId`, and
`BlobId` are public and are never derived from the secret. Ed25519 is
deterministic, so the same envelope bytes and key produce **byte-identical
signed blobs in Rust and Go**.

Offline verification (`verify_envelope`) performs, in order: canonical-form
re-encode comparison (`Malformed` / `NonCanonical` / `UnknownSchema`), strict
Ed25519 verification (`BadSignature`; small-order and non-canonical keys
rejected), optional caller-supplied grantee binding (`WrongGrantee`), expiry
(`Expired`), and revocation-set membership (`Revoked`). It performs **no I/O
and no network access**; it needs only the blob, the caller's revocation set,
a clock, and optionally the expected grantee.

Storage writes the blob to the content-addressed manifest namespace under
`BlobId` and publishes one relation:

```text
predicate:  capability_envelope
subject:    grantee (bytes32)
target:     value text = lowercase hex EnvelopeId
attrs:      capability.envelope_blob = lowercase hex BlobId
            capability.envelope_tenant = tenant label (discovery only)
```

Publishing the same envelope twice is idempotent. GC retains the blob exactly
while the attachment relation is retained. `verify --full` checks the blob's
content address, canonical form, signature, and recomputed `EnvelopeId`; it
deliberately does **not** reject expired or revoked envelopes, because those
are caller policy at use time.

### Enforcement

A verified envelope is attached to the `Principal`, not to a bare hash. At every
enforcement point it is applied **in addition to** POSIX permissions and in
addition to any explicit or Context-Root-bound capability:

- every grant present must independently allow the namespace, root, and relation
  predicates of the operation;
- budgets take the component-wise minimum across all grants;
- expiry takes the earliest bound;
- an envelope cannot widen `weft.capability.v1`, POSIX bits, or an ACL;
- the authorized universe is still computed from POSIX/ACL readability first.

Denials use the existing non-disclosing classes: `Unauthorized` at the resolver
and `NotFound` for exact-id access. The precise rejection is available only to
the holder through offline `capability verify`.

### CLI

`weftfs capability sign --envelope FILE --key FILE --out FILE [--attach]`,
`capability key --key FILE`, `capability verify --envelope FILE
[--revocations FILE] [--grantee HEX] [--now-ns N]`, `capability id`,
`capability list`, and `capability show ID`. Resolving and reading commands
(`context resolve`, `context materialize`, `cat`) accept `--envelope` plus
`--revocations` and enforce the verified grant. The Rust codec CLI exposes the
same sign/verify/id/key verbs so blobs can be produced or checked without a
store.

### Tests and cross-language evidence

- `tests/capability_envelope.rs` — 6/6: round-trip and store attachment; typed
  reject matrix; envelope narrows POSIX and intersects an explicit capability;
  revoked or expired envelopes cannot be attached or used; a workspace grant
  alone does not authorize an unrelated root; a relation selector requires the
  envelope predicate grant.
- `WeftCodec` unit tests cover canonical decode, rejection names, identities,
  signing, and offline verification; `WeftCodec-Go/capability` carries the
  independent implementation and its own tests.
- `WeftCodec-Go/hack/capability-cross.sh` is wired into `scripts/verify-all.sh`
  as five courts: Rust signs, Go signs, Rust verifies the Go blob, Go verifies
  the Rust blob, then `compare`. It requires byte-identical blobs, identical
  public key, `EnvelopeId`, and `sign_input`, both cross-verifications, and
  identical `BadSignature`, `WrongGrantee`, `Expired`, `Revoked`, and
  `NonCanonical` rejections. `compare` prints `capability-cross: OK` only when
  every check passes.

### Measured cost

Receipt `bench/receipts/capability-envelope-20261105T034858Z.json`, analysis in
`bench/RESULTS-capability-envelope.md`. Benchmark court, synthetic 1,000-object
store, single host, warm caches.

| Measurement | Samples | Median | p95 |
|---|---:|---:|---:|
| Offline verify, cold (first use of each blob) | 1,000 | 43,110 ns | 43,702 ns |
| Offline verify, warm (three further passes) | 3,000 | 43,180 ns | 43,621 ns |

Cold and warm medians differ by 70 ns (0.16%), so there is no material
first-use penalty once the code is resident. The measurement is the full
offline check: WCB decode, canonical re-encode comparison, Ed25519
verification, expiry, and revocation-set membership.

| Signed resolve (1,000 objects) | Samples | Median | p95 |
|---|---:|---:|---:|
| No envelope | 50 | 89,469,087 ns | 92,246,417 ns |
| Verified envelope attached | 50 | 89,941,598 ns | 93,162,746 ns |

Median overhead is **472,511 ns (0.53%)**, and both cases matched 1,000 objects
with 1,001 work units (root + 1,000 objects). The extra cost is the additional
scope checks in the resolver, not re-verification: verification happens once at
the trust boundary and the typed result is attached to the principal.

## Cross-tenant isolation court

Two tenants share one store. `tests/capability_isolation.rs` — 4/4 — proves the
logical non-disclosure contract:

- **Resolve/stat/enumerate.** Tenant A cannot resolve, stat, or enumerate
  tenant B's objects. Denial classes and messages are identical whether those
  objects exist or not.
- **Deduplication.** Identical chunk content may deduplicate across tenants
  **only because each tenant independently holds an authorized object with that
  content root**. Addressing the shared `ContentId` returns the caller's own
  authorized object. A content root held only by tenant B remains `NotFound`
  and is not made reachable by deduplication.
- **Work accounting.** Work units and budget outcomes for tenant A are
  independent of tenant B's population. The benchmark counterpart measured
  **work units 7 vs 7** when tenant B's unreadable population was 0 versus 40
  objects (`work_units_equal = true`).
- **Fail closed.** Revoked or expired envelopes are rejected at attach and at
  use; reusing the same hash or `BlobId` cannot revive them. A new grant
  requires a new signature and a new `revocation_id`.

The chunk store still deduplicates physical bytes across tenants; the
guarantee is that deduplication cannot be used to infer names, counts, or
existence across tenants. There is **no timing claim** (see limits).

## Verification

`bench/receipts/verify-20261105T035513Z.json` — **25/25 courts `pass`**,
including:

- the pre-existing 20 courts (Rust codec/store/FUSE, Go codec/store/FUSE,
  codec cross-decode in both directions, spec drift, generated-type drift,
  claims, Buck2 CAS and demo, agent memory, context cross-language gates,
  fuzz differential);
- five new authenticated-capability courts: `capability_cross_rust_gen`,
  `capability_cross_go_gen`, `capability_cross_rust_verify`,
  `capability_cross_go_verify`, and `capability_cross_compare`.

Revisions pinned by that receipt:

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `091d0bb` | program home, Rust store + FUSE, specs, courts |
| `WeftCodec` | `272c2c0` | Rust codec and envelope implementation |
| `WeftCodec-Go` | `b7400ec` | independent Go codec, envelope parity |
| `WeftFS-Go` | `963dc62` | independent Go store + FUSE, spec mirror |

The tag is cut after the version bump and documentation commits, so the tagged
commits are later than the verified code revisions; the only changes between
them are version literals and documentation. `verify-20261105T034941Z.json` is
retained because one run of the pre-existing `tests/representation.rs` shared
atomic decode counter raced under parallel execution (`expected exactly one
decoded chunk, got 2`); it passed standalone and in the 035513Z rerun, which is
the current receipt.

The host-only `claims` court (`scripts/check-claims.sh`) checks **42
documentation claims** against the files that carry them. The other long-run
receipts from v0.5.0 (100,000/100,000 crash iterations, 1,202,560 completed
POSIX operations, power-loss/block-layer proxies, comparative and real-model
benches) are unchanged and still apply to the same code paths.

## State of the rest of the program

Nothing else changed after the v0.5.0 tag. All v0.5.0 results still stand:
WeftTensor v1 logical roots across layouts, AI codec modes 8–14 (byte-identical
Rust↔Go where claimed), tensor representations and the cost-aware planner,
Local/Object backends, Go v2 manifest and tensor parity plus bounded open,
provenance receipts and the recovery planner, the Python 0.5.0 reader, the
Qwen2.5-0.5B/Instruct measurement, and the FUSE and comparative baselines.

The previously reported gaps are unchanged and remain the top follow-ups:

- FUSE write throughput: create 157–301/s versus ext4 291,722/s; 4 KiB sync
  overwrite 108 ms (Rust) / 392 ms (Go), dominated by the frozen v1 whole-file
  `ContentId` digest pass.
- No Go `weft.provenance.v1` parity; no persisted Go selector index or bounded
  commit index; Go `PUNCH_HOLE`/`ZERO_RANGE` are content-only.
- Region reads of delta/Huffman tensor frames decode the whole frame; planner
  costs are uncalibrated estimates.
- Physical media power loss, host page-cache loss, and RAPL energy are not
  measured or claimed.
- No 3FS run; the POSIX backend remains a capability configuration.

## Still not claimed

- **No federated revocation.** The revocation set is explicit and
  caller-supplied; there is no network lookup, global revocation service, or
  implicit trust of the store. Coordinating that set is the caller's job.
- **No key rotation, delegation, or HSM.** A grantor signs grants directly.
  There is no delegation chain, certificate graph, transitive authority, key
  rotation protocol, or hardware-backed key claim. Seed files are raw bytes or
  hex on disk.
- **No timing-side-channel claim.** v1 does not claim constant-time
  authorization, cache behaviour, or the absence of cross-tenant timing
  channels. The isolation court proves the logical/non-disclosure contract,
  not a timing bound.
- **Grantee binding is caller-supplied.** `verify_envelope` checks
  `WrongGrantee` only when the caller passes an expected grantee; the store
  supplies it at enforcement points, but offline verification of an arbitrary
  blob does not discover the intended grantee.
- Revocation is keyed by `revocation_id`, not by `EnvelopeId`; replacing an
  attachment is a new envelope, never an un-revoke. The store has no un-revoke
  operation.
- `verify --full` does not reject expired or revoked envelopes; those are
  use-time policy.
- Grants are namespace/root/predicate scoped, not per-object or per-inode.
  Capabilities add restrictions; they do not replace kernel permissions and are
  not an OS sandbox.
- No multi-writer or distributed store. The store is single-host,
  single-writer with `flock`.
- No physical media power loss, host cache loss, RAPL energy, or fleet claim.
- Python does not implement capability envelopes; the release wheel remains
  0.5.0 and is not on PyPI.
