# KV context plane: persisted index, grants, side channels, Go parity, engine

**Status: implemented (Rust store and Go prefix tier), experimental courts.**
This document is the analysis for subphases T063 (persisted prefix index), T065
(batched page publication and verified-session grant cache), T067 (side-channel
court), T068/X10 (Go parity and serving cost) and X17/T064/T069 (inference
engine attempt and trace replay). Every number below is emitted by
`scripts/perf_numbers.py` from the receipts named beside it; nothing here is
hand-written.

Receipts. The four non-index receipts are stamped `20261114T133000Z` at
WeftFS revision `0a0723f`; the index receipt was re-measured at `80d6bad` so its
ceiling field reports what the court actually achieved (stamp
`20261114T133053Z`). Every receipt records its own stamp and revision, and the
generated numbers below carry both.

| receipt | what it measures |
|---|---|
| `bench/receipts/kv-prefix-index-20261114T133053Z.json` | index ladder, crash, revocation, index-only scaling |
| `bench/receipts/kv-grants-20261114T133000Z.json` | publication batching, grant-cache reads |
| `bench/receipts/kv-sidechannel-20261114T133000Z.json` | outcome and timing side channels |
| `bench/receipts/kv-go-parity-20261114T133000Z.json` | Go prefix tier, Ed25519, handoff, per-page cost |
| `bench/receipts/kv-engine-20261114T133000Z.json` | engine attempt blocker + trace replay |

All courts ran in `scripts/docker-court.sh` under the unchanged hard limits
(3 GiB memory / 3 GiB swap, 1024 pids, 4 CPUs, 1 GiB `/tmp` tmpfs).

## 1. The persisted KV prefix index (T063)

### What changed

`kv_prefix_lookup` used to read and parse every prefix record. The index keys
each **page-aligned prefix boundary** by
`(identity_root, token_chain_root)` and names the exact record object. A lookup
walks the request's page blocks once, carrying the frozen token chain, and
probes the index at each boundary; the longest verified match wins. An entry is
used only after the record it names is read, re-hashed, and re-run through the
frozen compatibility rule.

The index is **generation-coupled**: it is written outside committed state and
the root record names the commit generation and head it describes, its segment
digests, the live entry count and an XOR accumulator over the entries. A load
requires all of them to match; anything else is refused and the oracle scan
answers. Mutations append one small delta segment; past the delta bound the
index is simply left stale and rebuilt on request. A crash between the segment
and the root leaves an unreferenced segment, never a dangling root.

Revocation (`weft.kv.revocation.v1`) is additive and never deletes the prefix
record, so snapshots that predate it still read. A revoked prefix is never
returned by either lookup path, and `kv get` is indistinguishable from an absent
name.

### The N ladder (end-to-end store, page-aligned synthetic prefixes)

<!-- BEGIN GENERATED:kv-index-ladder (scripts/perf_numbers.py) -->
| N | indexed p50 | p95 | p99 | oracle scan p50 | scan/index | index bytes | store bytes | reopen | first indexed lookup |
|---|---|---|---|---|---|---|---|---|---|
| 1,000 | 74.2 µs | 121.2 µs | 149.3 µs | 29,981.4 µs | 403.8× | 152,064 B | 23,824,421 B | 166.3 ms | 0.40 ms |
| 5,000 | 103.6 µs | 161.2 µs | 1,648.2 µs | 153,604.2 µs | 1,483.2× | 760,064 B | 118,980,291 B | 702.4 ms | 1.60 ms |
| 10,000 | 143.5 µs | 3,079.8 µs | 3,079.8 µs | 314,776.9 µs | 2,194.1× | 1,520,064 B | 267,557,404 B | 1,413.3 ms | 3.09 ms |
<!-- END GENERATED:kv-index-ladder -->

Comparisons are **differential**: each lookup row runs both the indexed path and
the oracle scan on the same fixture, in the same process, interleaved.

### Index-only scaling

<!-- BEGIN GENERATED:kv-index-only (scripts/perf_numbers.py) -->
The largest index-only rung is **N = 1,000,000**: segment **152,000,064 B**, encode **269.5 ms**, decode **47.8 ms**, 200 probes at p50 **1,362 ns**, peak RSS **837,080 KiB** (including the entry vector, the encoded copy and the decoded copy).
<!-- END GENERATED:kv-index-only -->

The index-only rows are synthetic and labelled as such: they show the wire
format and the probe cost. They are **not** end-to-end store results.

### Ceiling, crash, revocation

<!-- BEGIN GENERATED:kv-index-crash (scripts/perf_numbers.py) -->
End-to-end ceiling: **20,000 prefixes** (the next rung hit ENOSPC in `/tmp`). Crash after the base segment leaves **no** root (the next reader rebuilds); crash after the root leaves a **valid** index; both cases produced **0** and **0** mismatches against the oracle. Revocation over one prefix in ten: **2,000** entries before, **1,968** after the bounded delta chain, **1,800** after one rebuild, **2.921 ms** per revocation, revoked prefixes returned by either path = **0**.
<!-- END GENERATED:kv-index-crash -->

The end-to-end ceiling is the point at which the court's 1 GiB `/tmp` runs out
while writing prefix records and page objects; it is a fixture-size bound, not a
scale claim for the index (the index-only rows go further). The requested rung
was 20,000 prefixes and the court held 11,776; the receipt records both, because
reporting the request as the ceiling would overstate the measured fixture. The
revocation run
revokes one prefix in ten; the delta chain exceeds its bound, the index becomes
stale, both lookup paths stay correct, and one rebuild restores it.

## 2. Batching and the verified-session grant cache (T065)

Two independent costs were addressed:

* **Publication.** `kv_prefix_put_batch_seeded` writes many prefixes in one
  group commit: one commit record, one superblock slot, one durability barrier,
  one generation. The record set is identical to the single path (the batch
  receipt asserts the sorted record-id digest matches).
* **Grant decode.** A verified grant decode is cached per handle, keyed by the
  envelope, the store generation and the revocation epoch. Any commit or
  revocation invalidates it. `Options::kv_grant_cache = false` restores the
  per-read decode as the control.

<!-- BEGIN GENERATED:kv-grants-publish (scripts/perf_numbers.py) -->
Publication of the same prefix set: single **670.1 ms** / **1,024** commits / **29,353,011 B** on disk versus batched **246.5 ms** / **4** commits / **7,409,001 B** — **2.72×** faster, with an identical record set.
<!-- END GENERATED:kv-grants-publish -->

<!-- BEGIN GENERATED:kv-grants-reads (scripts/perf_numbers.py) -->
Page stream (128 × 4 KiB pages, one grant): grant cache on **38.21 µs** p50 / **75.16 µs** p99 with **255** hits and **1** miss; the per-read decode control is **68.22 µs** p50 / **83.66 µs** p99 — **1.79×**. The canonical descriptor is **229 B**, **5.29 %** of the page.
A commit invalidates the cached grant: the next read costs **94,546 ns** against **42,019 ns** for a hit.
<!-- END GENERATED:kv-grants-reads -->

The descriptor is a fixed per-page governance cost; batching publication does
not remove it, and the stream measurement shows exactly what share it is.

## 3. Side-channel and tenant-security court (T067)

Every cross-tenant probe must be denied with the same class and text; a
non-denial is a hard failure. Timing is compared with a Mann–Whitney U z-score
against the **absent-object** arm, which any caller can produce for free, with
interleaved round-robin sampling and the arm order alternating every round. The
same-call control pair calibrates the noise floor.

<!-- BEGIN GENERATED:kv-sidechannel (scripts/perf_numbers.py) -->
Outcome: **pass** — every cross-tenant, absent and expired probe is denied with one class and one text; **0** stale reads after the revocation. **4,000** samples per arm; same-call control z = **2.88**.
Timing (p50 / p99, µs): authorized hit **34.04 / 46.99**, absent object **0.06**, foreign tenant **0.41**, wrong root **0.41**, expired grant **0.37**, no envelope **0.32**; with `kv_denial_equalise` the absent arm moves to **0.11**. Size-inference z = **4.40**.
<!-- END GENERATED:kv-sidechannel -->

`kv_denial_equalise` (default off) runs the capability half of the KV preamble
on a failure path. It narrows the absence gap and does not close it; both are
reported. No mitigation makes this constant-time.

## 4. Go parity (T068/X10)

WeftFS-Go now has the prefix tier: canonical `weft.kv.prefix.v1` records that
Rust parses (and vice versa), the frozen token-chain root, the exact
compatibility rule, parent-chain reuse, revocation, and a one-commit batch write.

**Ed25519 decision: implemented, not a trusted-verifier boundary.**
`Store.KvPrefixGetVerified` loads the stored signed envelope blob and verifies
it with WeftCodec-Go's `capability.Verify` (the Go standard library's
`crypto/ed25519`), then builds the scope and only then reads. The older
`Store.KvPrefixGet` entry point still accepts an already-verified
`*auth.Capability`; that is the historical trusted-verifier boundary and is
labelled as such in the source. The cross-language test is bidirectional: Go
verifies a Rust-written record under a Go-verified grant, and Rust verifies a
Go-signed grant and reads a Go-written prefix — identical page counts and prefix
ids.

<!-- BEGIN GENERATED:kv-go-parity (scripts/perf_numbers.py) -->
Tests: **5 passed / 0 failed / 0 skipped**, including the bidirectional handoff. Batched write of 4 MiB in **1** commit: **360.0 µs** per page; a single-prefix commit is **402.3 µs** per prefix. A verified read of a one-page prefix is **0.131 ms**, allocating **209,528 B** per read and **438,576 B** per write.
<!-- END GENERATED:kv-go-parity -->

## 5. The engine attempt (X17/T064/T069)

**No inference engine could run in the court.** Both attempts are recorded with
their exact commands and error text in the receipt:

* vLLM 0.33.0 resolves to CUDA torch 2.14.0 plus its CUDA wheel set; the install
  dies with `[Errno 28] No space left on device` in the 1 GiB `/tmp` tmpfs.
  `--no-deps` still fails: the vLLM wheel alone does not fit, and torch and the
  CUDA runtime would be missing anyway.
* SGLang 0.5.23 installs with `--no-deps`, but importing it fails on absent
  runtime dependencies; with dependencies it dies in the same place downloading
  the same torch.
* The court has no `/dev/nvidia*`.

What ran instead is an **engine-shaped request-trace replay over the same
records**, labelled as trace replay. The reuse side is measured for real
(indexed lookup, page transfer and decode); the recompute side is a declared
hypothetical prefill model, swept over a grid. The receipt states which numbers
are which, and `engine_measured` is empty.

<!-- BEGIN GENERATED:kv-engine (scripts/perf_numbers.py) -->
Engine attempt: **no inference engine could run in the court**. Trace replay over **1,700** reuse requests: indexed lookup **199.7 µs** p50 against the oracle scan **7,135.4 µs**, reuse get **885.3 µs** p50 / **1,091.5 µs** p99, local token-chain hash **81.1 µs**; mismatches **0**.
Measured break-even is **2.119 µs/token**: reuse is cheaper than recompute whenever the per-token prefill cost exceeds it. Against the hypothetical grid, always-recompute regret is **11,344.9 ms** at 10 µs/token, **49,232.9 ms** at 50, **115,536.9 ms** at 120 and **475,472.9 ms** at 500; always-reuse and the declared model have zero regret in every cell because this fixture's prefixes are long.
<!-- END GENERATED:kv-engine -->

The regret table is a decision analysis: for each hypothetical per-token
prefill cost, it compares three policies (always reuse, always recompute, the
declared model) with an oracle that knows the measured reuse cost and that same
hypothetical recompute cost. It does not measure an engine, a TTFT, a hit rate
or a token rate.

## 6. Every loss

* **Index ceiling.** The end-to-end ladder stops where the court's `/tmp` runs
  out (11,776 prefixes of a requested 20,000 rung); the index-only rows are
  synthetic. No million-prefix end-to-end claim.
* **Partial-tail prefixes are not indexed.** A prefix whose last page is not
  page-aligned is skipped at build; when any exist, an index miss falls back to
  the oracle scan (correct, slower).
* **Index memory and build cost are real.** The root and segments must be read
  and validated; reopen cost grows with entry count. The index is per-handle and
  not shared between processes.
* **Every commit invalidates the grant cache.** Readers that interleave writes
  pay a miss per write generation; the cache is not persisted.
* **Batching changes the commit head.** Records are identical; heads are not.
* **The existence side channel persists at nanosecond resolution.** Denials for
  an absent object and for a present-but-unauthorized object differ in cost;
  equalisation narrows but does not close the gap. It is not constant-time.
* **Size inference is statistically detectable at very large sample counts**
  even though the absolute difference is tiny; see the receipt's z-score.
* **Go batching is a small win on this fixture**, far smaller than Rust's,
  because the per-prefix work dominates the commit.
* **Go allocation per read and per write is large** (hundreds of KiB); no
  buffer-reuse optimisation was shipped.
* **Go has no persisted index**; its lookup is the oracle scan.
* **No Go mode-19 or mode-15 writer.**
* **The engine did not run.** Everything about prefill, TTFT and token rate is
  hypothetical.

## 7. Non-claims

No production-serving claim, no GPU claim, no fleet or cloud claim, no
multi-tenant deployment claim. One host, synthetic pages and traces, warm page
cache. Timing results are in-process; a remote attacker also sees network noise,
which is not modelled. The index is not an authorization mechanism: page bytes
are still gated per page by the capability envelope.
