# Retention bindings and class-aware GC (R6)

**Spec:** `spec/WEFT-RETENTION.md` (`weft.retention.v1`).
**Receipt:** `bench/receipts/retention-20261109T220237Z.json` (+ `.log`),
measured in the bench court at WeftFS `9de0e09`, WeftCodec `813171b`,
WeftCodec-Go `c872f26`, WeftFS-Go `9a87f56`.
**Court:** `scripts/retention_court.sh` (bench court, hard limits unchanged:
3 GiB == swap, 4 CPUs, 1024 pids, 2048 nproc, bounded `/tmp`).
**Cross-language parity:** `scripts/retention_cross.sh`.

## 1. The distinction the subphase is about

A storage layer sees bytes and references, not utility. A sliding-window KV
page is useless minutes after it is written; a long-context block cache written
in the same minute is useful for a day. One coarse TTL therefore either keeps
the short-lived class too long or discards the long-lived class too early.

The artifact already has an **identity** (`descriptor_root` for a mode-19 KV
page). Retention is a **second, mutable, versioned layer**:

* `weft.retention.v1` references `descriptor_root` and declares the class, the
  producing run, the declared lifetime, and the policy epoch;
* `RetentionId = H("weft/retention/v1", WCB(record))`;
* **no field of the binding is an input to any content root, descriptor,
  `ObjectId`, manifest, or dedup key.** Publishing, rebinding, or deleting a
  binding writes one metadata record and zero artifact bytes. Two artifacts
  with identical bytes and different policies have identical content roots.

Reachability is computed first, exactly as ordinary GC computes it. Class
affects **ordering**, never reachability safety: a class-aware pass reclaims a
subset of what a reachability-only pass would reclaim, and may retain
unreachable bytes for a live binding's declared window. The invariant is
`class_aware_gc_never_reclaims_a_reachable_blob` (Rust) and
`TestClassAwareGCNeverReclaimsReachable` (Go).

## 2. The court

Two synthetic populations of mode-19 KV pages, one deterministic trace:

| Population | Count | Reuse model | Declared class | Lifetime |
|---|---:|---|---|---:|
| volatile | 1,000 | sliding window; reuse distance 240–300 s | `volatile` | 300 s |
| ephemeral | 300 | reused across the six-hour session; 100 min (fast) / 5 h (slow) cycle | `ephemeral` | 6 h |

* 360 ticks of 60 s = six simulated hours; two request rates (10/tick "fast",
  2/tick "slow"); page frame 3,424 B; capacity 3,000,000 page-frame bytes.
* 20 pages are written first and captured by a snapshot, so they are reachable
  from a committed root for the whole run. This is how an unsafe policy is
  detected.
* The trace is **synthetic**. It models reuse distance; it is not a serving workload, and the receipt says so.

Policies:

* **(a) fixed TTL + LRU** — a 300 s global TTL, LRU under the capacity budget,
  never consults the store's reachability roots.
* **(b) reachability-only GC** — ignores class and lifetime; reclaims every
  unreachable blob every tick.
* **(c) class-aware GC** — reachability first, then expired bindings ordered
  `unbound < volatile < session < ephemeral < durable`, live bindings retained.

Every hit re-reads the exact frame and compares it byte-for-byte. Every run ends
with `verify --full` over the store.

## 3. Results

Page-frame bytes retained (`retained`), bytes reclaimed by the policy
(`reclaimed`), safe-hit rate, unsafe reclaims (artifacts evicted while reachable
from a committed root), and physical `objects/` + `manifests/` bytes:

| Rate | Policy | Safe-hit rate | Retained | Reclaimed | Unsafe | Physical | Wall |
|---|---|---:|---:|---:|---:|---:|---:|
| fast | fixed TTL + LRU | **35.31 %** | 123,264 | 12,302,432 | **45** | 6,457,356 | 158.0 s |
| fast | reachability-only | 1.50 % | 68,480 | 16,524,224 | 0 | 5,012,636 | 250.2 s |
| fast | class-aware | **74.94 %** | 3,002,848 | 4,536,800 | **0** | 4,340,836 | 151.3 s |
| slow | fixed TTL + LRU | **0.97 %** | 34,240 | 6,858,272 | **37** | 4,416,301 | 104.4 s |
| slow | reachability-only | 2.64 % | 68,480 | 6,782,944 | 0 | 2,766,411 | 143.4 s |
| slow | class-aware | **73.47 %** | 3,002,848 | 2,102,336 | **0** | 4,051,586 | 71.0 s |

* Class-aware beats fixed TTL by **39.6 points** (fast) and **72.5 points**
  (slow) on this trace.
* Fixed TTL evicts **45 / 37** artifacts that were reachable from the snapshot.
  The snapshot keeps the bytes, so the store does not lose data — which is
  exactly why a cache that ignores reachability is both unsafe at the reference
  level and unable to free that capacity. Reachability-only and class-aware
  report **zero** unsafe reclaims.
* Reachability-only keeps the smallest physical tree (5.0 / 2.8 MB) and the
  worst hit rate (1.5 / 2.6 %): it discards the short-lived class entirely.
* Class-aware retains 3.00 MB against a 3.00 MB capacity budget: it is **2,848 B
  over** at the end (0.09 %), and its peak over-budget is 1,451,200 B, entirely
  from the initial 1,300-page seeding burst (the first five ticks), which a real
  cache would amortise. This is published, not hidden.
* `reclaimed` is counted in page-frame bytes (cache capacity), not physical
  store bytes; `physical` is the data tree at the end.

## 4. The cost of a misclassification

Both runs use class-aware policy (c) at the fast rate and the same trace.

| Declaration | Safe-hit rate | Retained | Extra retained | Early reclaims | Unsafe |
|---|---:|---:|---:|---:|---:|
| correct | 74.94 % | 3,002,848 | — | — | 0 |
| volatile declared `ephemeral` | 100 % | 4,451,200 | **+1,448,352 B** | 0 | 0 |
| ephemeral declared `volatile` | 67.25 % (−7.69 pts) | 3,002,848 | 0 | **453** | **0** |

* Declaring a volatile page `ephemeral` buys retention it does not need. On
  this trace it raises the hit rate (everything is retained) but costs
  **1,448,352 extra retained bytes**, +48.24 % over the correct retained
  footprint, and reclaims nothing. It is a capacity loss, not a correctness
  loss.
* Declaring an ephemeral page `volatile` reclaims it early: **453 early
  reclaims**, **277 extra misses**, **−7.69 points** of safe-hit rate. It is
  **safe**: reachability still governs, so zero artifacts were reclaimed while
  reachable. The cost is utility, not corruption.

## 5. Losses and non-claims

* **Capacity is not free.** A 6-hour retention window for 300 ephemeral pages
  costs 3.00 MB of the 3.00 MB budget; the correct policy is at the budget edge
  and 2,848 B over at the end.
* **Warm-up is over budget.** The synthetic trace seeds 1,300 pages at t=0,
  producing a 1,451,200 B peak over-budget before volatile bindings expire. A
  serving cache fills gradually; this court does not.
* **Fixed TTL is not safe.** 45 / 37 artifacts were evicted while still
  reachable from the snapshot. The snapshot's bytes survive; the cache's
  reference does not.
* **No wall-clock guarantee.** `declared_lifetime_ms` is a hint. Nothing
  verifies a producer's declaration, and no clock-skew defence is claimed.
* **No fleet, media, or serving claim.** One host, one local store, a synthetic
  trace, and a simulated clock. No inference engine was integrated.
* **Trace sensitivity.** The win is a property of this trace's reuse distance.
  If the trace's reuse distance were shorter than every policy's window, the
  policies would tie; the court would publish that number instead. It did not.
* **Go surface.** The Go store implements the record, pointer, and ordering and
  is read/write-compatible with Rust (`scripts/retention_cross.sh`: 12/12
  checks, `YES byte-identical` for both the record and the committed store
  binding). Go has no mode-19 KV writer, so its `RetentionRequest` supplies the
  artifact anchor explicitly.
* **Runtime.** The full court is 1,120.6 s in the bench court, dominated by
  360 GC passes per run over a store with ~4,000 blobs. It is not part of `verify-all`; it is a standalone bench court.

## 6. Reproduce

```sh
scripts/docker-court.sh rust "cd /work && cargo test --test retention --locked"
scripts/docker-court.sh go "cd /work/WeftFS-Go && go test -race -count=1 ./store/..."
scripts/retention_cross.sh
scripts/retention_court.sh
```
