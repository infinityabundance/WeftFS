# Revocation epochs and deny-before-decode (T078)

`src/revocation_epoch.rs` replaces a caller-supplied revocation set with one
monotonic **revocation epoch** published by the store, plus a per-read gate that
checks authorization and revocation before it fetches or decodes any object
bytes.

## 1. The problem this replaces

Before T078:

* `weftcodec::capability::verify_envelope(blob, revocations, …)` took the
  revocation list from the caller. A client that was not told about a revocation
  kept verifying;
* the KV prefix path kept a local revocation count;
* nothing tied a decision to a freshness contract, so a long-lived client could
  serve from an arbitrarily old view.

## 2. The record

Schema `weft.revocation-epoch.v1`, canonical WCB map with exactly:

| Key | Type | Meaning |
|---|---|---|
| `schema` | text | `weft.revocation-epoch.v1` |
| `epoch` | u64 | 1, 2, 3, … monotonic |
| `issued_at_ns` | u64 | store clock at publication |
| `valid_for_ns` | u64 | the record's own freshness window; non-zero |
| `prev` | bytes32 or null | record id of epoch-1 (`null` only for epoch 1) |
| `revocations` | array | `{tenant, kind, id}` sorted by `(tenant, kind, id)`, unique |

`kind` is `capability` (an envelope id or its revocation id) or `content` (a
whole-file content root or an object id). `RevocationEpochId =
H("weft/revocation/epoch/v1", WCB(record))`.

Layout, ordinary committed state:

```text
/revocation/epoch             current record (mutable pointer)
/revocation/history/<epoch>   immutable record for every epoch
```

Publication is one group commit: the history record is created, then the
current pointer is written. A crash leaves the old epoch current and an
unreferenced history blob, never a half-published epoch. Re-publishing an
existing entry is idempotent in content but still advances the epoch: a reader
that observed the older epoch must refresh.

`verify_revocation_chain` walks every history record and requires: contiguous
epochs, `history[i].prev == history[i-1].id`, `history[i].epoch == i`, the
current record equal to `history[current.epoch]`, and **no history slot above
the current epoch** (a rolled-back pointer is `Corrupt`). A forged or
non-canonical record is `Corrupt`/`NonCanonical`.

## 3. The freshness contract

A client calls `observe_revocation_epoch(max_staleness_ns, now_ns)` once and
keeps `(epoch, record id, observed_at_ns, max_staleness_ns)`. It must refresh
when either bound is exceeded, and it must not serve without a valid observation.
`observe` validates the chain and the record's own `valid_for_ns`; a record older
than its window is `StaleEpoch`.

`check_read_at_epoch` enforces, in this order:

1. the observation's age is within its `max_staleness_ns` (`StaleEpoch`);
2. the store's current epoch equals the observation's epoch and record id. A
   revocation published since the observation, or a rolled-back/forged current
   record, refuses here (`StaleEpoch` or `Corrupt`);
3. a `capability` entry matching the principal's verified envelope refuses
   (`CapabilityRevoked`, fixed text);
4. the store authorizes the object from **metadata** (POSIX/ACL). An absent or
   unreadable object gets the ordinary `NotFound`/`Unauthorized`, so the
   revocation answer is never an existence oracle for callers who may not read
   the object;
5. a `content` entry matching this tenant and the object's content root or id
   refuses (`CapabilityRevoked`, fixed text);
6. only then is an `AuthorizedRead` token returned. The caller reads bytes with
   `read_object_authorized`.

Steps 1–2 read one small control record. Steps 3–5 read metadata only. The
target object's bytes and frames are untouched on every refusal path, and the
denial text is the constant `revocation denies this read` — no id, size, count
or existence hint.

`tests/revocation_epoch.rs` measures exactly this: with a 1 MiB target, a stale
or revoked refusal adds zero `decode_calls`, and its decoded/backend byte deltas
stay below 8 KiB (the control record). It also proves the same denial text for a
revoked id and no revocation answer for absent ids.

## 4. Tenant scope

An entry is `(tenant, kind, id)`. Revoking tenant A's content root denies A and
leaves tenant B reading the same deduplicated bytes; the court checks both the
denial and an exact read by B. The tenant must be the authenticated one: when
the principal carries a verified envelope, a disagreeing `tenant` argument is
`InvalidArgument`, never a silent override.

## 5. Enforcement boundary — what remains local

* **Store-local, single-writer order.** The epoch is one mutable record plus an
  append-only history under one store's commit chain. There is no cross-store
  replication, no consensus, no gossip, no global ordering.
* **No clock defence.** `issued_at_ns` is the store's clock, `observed_at_ns`
  the client's. Skew is not detected. The `max_staleness_ns` bound is the
  client's own admission; an operator that sets it to hours has chosen that.
* **A client that cannot reach the store must refuse to serve.** There is no
  cached authority.
* **Already-open handles.** This gate governs a new authorization decision. Bytes
  a caller already read cannot be un-read; a consumer that holds decoded state
  must apply its own invalidation.
* **What is not claimed:** no distributed revocation, no transparency log, no
  revocation of an attestation already accepted, no timing side-channel
  resistance.

## 6. Cost

* One control-record read per authorized read (the current epoch).
* Chain validation is O(epochs) and runs in `observe` and `verify`, not on every
  read.
* History grows with the number of revocation publications, not with time.

## 7. Evidence

`scripts/revocation_epoch_court.sh` runs the 9-test Rust court (including the
counter-level deny-before-decode assertions) and a CLI scenario that publishes
two epochs, shows/verifies the chain, and observes it. See
`bench/RESULTS-revocation-epoch.md`.
