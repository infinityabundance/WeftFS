# WeftFS v1.4.0 — the compressed-write frontier, incremental indexes, and a bounded checkpoint queue

Released on `staging`. v1.4.0 is the first release after v1.3.0. It changes
**no frozen wire or on-disk byte**: the WFT1 frame, payload modes 0–20, the
canonical WCB, `weft.manifest.v1`–`v4`, the selector grammar, the store layout,
the superblock and recovery protocol, the transport container, `weftfs.commit.v1`
/ `weftfs.commit.v2` and every frozen store and conformance fixture are
unchanged. No mode value was added, removed, or renumbered.

The release is **R12c** (checkpoint-session work), **R14d** (Azure MW2
verified in Go), and **R17–R22** (the compressed-write ladder). The version
bump is package metadata plus the tool-version field in newly written
provenance records (Rust `env!("CARGO_PKG_VERSION")`, Go `storeVersion`).
`1.4.0` means “the release after 1.3.0”, not production readiness.

Everything was built and measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

**Date:** 2026-11-12 · **Branch:** `staging` · **Tag:** `v1.4.0` at the release
commit (see below).

## The short version

Three numbers matter, and two of them are losses.

| Question | Answer |
|---|---|
| Did the compressed write get faster? | **Yes, 40.9×**: 24,229.2 ms at R17 to **592.6 ms** least-contended / 600.9 ms median at R22, at the same 282,106,049 B |
| Did it reach native parity? | **No.** The native per-tensor control A is 332.0–333.5 ms; 592.6 ms is **1.78× A**. The ≤ 336 ms rung is **not met**, and §6 names the measured limiter |
| Did the bytes change? | **No.** Every planned row publishes 282,106,049 B — **33.5 %** below A's 424,439,296 B — and every restores 101/101 exactly |

The scoreboard: **G1 (≤ 3,360 ms) met at R19, G2 (≤ 1,344 ms) met at R20,
G3 (≤ 672 ms) met at R21 and held at R22; the final rung (≤ 336 ms) is not
met** — 592.6 ms against 336 ms.

## What changed

### R12c — checkpoint sessions, incremental indexes, metadata, and a bounded queue

* **Persistent sessions (item 1).** One `CheckpointSession` per (store, run)
  instead of a helper process per checkpoint. Warm completion p50 falls
  **339.0 → 62.9 ms** at a 4-step interval (and 173.6 → 50.6, 119.9 → 47.7,
  94.7 → 46.3 at 8/16/32); loop wall 17,615.8 → **4,476.3 ms**; training-thread
  pause p50 3.99 → **0.80 ms**. Exactness: 196/196 resumed states, 196/196
  next-step loss bits, 172/172 post-step states, identical loss trajectory.
  Receipt `training-goodput-session-20261111T220500Z.json`.
* **Operation-derived incremental index (item 2).** Publish no longer calls
  `StateIndex::build`. Commit p50 is **0.198/0.201/0.199 → 0.110/0.112/0.112 ms**
  at 1k/10k/100k generations — flat within 1.8 % while history grows 100× — with
  byte-identical index segments; live-state cost fell 3.2–4.1×
  (0.641 ms at 1,000 live, **5.569 ms** at 10,000). A same-revision control with
  maintenance disabled reproduces the baseline within 0.5 %, so the comparison
  is the change, not the host. Receipts `incremental-index-20261111T22*` and
  `…T23*`; analysis [`bench/RESULTS-incremental-index.md`](../bench/RESULTS-incremental-index.md).
* **Index GC bound per retained root (item 2.1).** Item 2 exposed a real defect:
  the retained-chain depth bound was applied to one `visited` set shared by every
  root, so index GC refused outright once ~8 snapshots were retained
  (`index GC skipped: … LimitExceeded`) and the index tree grew with commits.
  Fixed per root in both languages; the reproducing fixture reclaims
  **182,321 B of 369,010 B** and all 12 snapshots still load their full chain.
  64 live / 100,000 generations publishes 8,754,798 B instead of 81,816,646 B.
  Retained failures: `index-gc-bound-20261111T190100Z-prefix-8roots.json`,
  `…T194000Z-prefix-tail.json`.
* **DCP metadata encoding (item 3).** The pure-Python canonical encoder cost
  **3.957 ms p50** for 169 entries; a reusable plan plus a fast generic encoder
  take it to **0.555 ms** warm (0.121 signature + 0.400 assembly), with the
  reference encoder kept as the oracle and every plan proved equal to it before
  use. End to end: `adapter.metadata_serialize` **4.679 → 0.631 ms**, completion
  p50 **40.03 → 34.58 ms**. A native round-trip option was measured at
  0.443 ms *before any encoding*, so it was refuted without being built.
  Receipts `dcp-metadata-20261111T230500Z.json`, `…T235500Z.json`,
  `checkpoint-latency-20261112T000500Z.json` vs `…T001500Z.json`.
* **Bounded checkpoint queue (item 4).** The pre-change behaviour was measured:
  with non-waiting callers, which save survived was decided by a race (`2` of 4,
  `2` of 8), and `save_checkpoint_async` grew the process from 2 to 19 threads
  because it created a pool per call. `CheckpointSession` now admits saves under
  `max_outstanding` (**default 2**), a staged-byte budget and a blocking wait
  with a 30 s ceiling; a refusal is `QueueFull` naming every bound and publishes
  nothing. At a 1-step interval the loop wall falls **7,415.3 → 5,761.7 ms
  (−22.3 %)**, 96/96 saves land, `no_silent_skips` is true and every cell drains
  to `outstanding 0`. Receipt
  `checkpoint-queue-bound-after-20261112T043000Z.json`.

### R14d — Azure MW2, verified in both languages

Rust gained an Azure `publish_head_conditional` (one `Put Blob` whose `If-Match`
is the ETag read with the head check) and the MW2 gate; Go gained the same
conditional head plus the policy gate. The Go half was committed as an
unverified WIP and **failed its own suite**; the reproduced failure was

```
TestAzureOptimisticTwoDisjointCommitters
  verify: ok=false err=<nil> problems=[{UnsupportedVersion recovery failed:
  UnsupportedVersion: the superblock cache names generation 3 but the writer
  lock record attests generation 1; ...}]
```

The cause was Go recovery, not the Azure client: recovery treated the writer
`LOCK` record as the head authority on every path except the two that wrapped
the commit source in `headAttestHider`, so `Verify` and a read-only open refused
an optimistic store. Rust already gates the same check on `WriterMode::policy`.
`recoverWithStrategy` now reads the `writers` policy record itself, skips the
attested-head reconciliation for an optimistic store, and **fails closed** on a
record that exists but cannot be decoded; the wrapper type is gone, so there is
one authority decision. The Azure two-committer test now also reopens read-only
and verifies, and `TestMalformedWritersPolicyFailsClosed` pins the fail-closed
default. WeftFS-Go `19615ea`. The Rust Azure suite was re-run unchanged:
22 passed / 0 failed.

### R17–R22 — the compressed-write ladder

The comparator is **the same backend with and without the WeftFS
protocol+codec**, never the reference FUSE filesystem: **A** is a native
per-tensor durable write of the same 101 tensors, **C** is the WeftFS planned
write, both measured in the same session. Method frozen in
[`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md);
fixture Qwen2.5-0.5B-Instruct, 101 tensors, 424,439,296 logical bytes, 4 CPUs,
3 GiB court.

| Rung | Durable write wall | Footprint | What bought it | Receipt |
|---|---:|---:|---|---|
| R17 (start) | 24,229.2 ms | 282,283,011 B | nothing — this is the attribution baseline | `write-cost-ablation-20261112T050102Z` |
| R18 | 4,117.8 ms | 282,283,011 B | exact two-stage planner: analyse → select → materialise; level 19 conditional, not deleted | `planner-two-stage-20261112T081151Z` |
| R19 | 2,328.4 ms | 282,283,011 B | byte-identical mode-9 encoder, 2.85× sequential / 5.81× at four emission threads | `mode9-encoder-20261112T160000Z`, `r19-wall-after-20261112T140000Z` |
| R20 | 1,236.8 ms | **282,106,049 B** | one group commit and **one** durability barrier per checkpoint | `r20-publication-20261112T150000Z` |
| R21 | 623.3 ms | 282,106,049 B | bounded reader → N encoders → one publisher pipeline plus the early-barrier writeback thread | `r21-pipeline-20261112T201500Z` |
| **R22 (shipped)** | **592.6 ms** (median 600.9) | **282,106,049 B** | chunk-parallel ZSTD (byte-identical), exact parallel analyse, measured defaults | `r22-pareto-20261112T230000Z` |
| **target** | **≤ 336 ms** | ≥ first-success band | **not met** | — |

The 24,229.2 ms at R17 was **84.0 %** ZSTD-19 (20,353 ms) selected for **zero**
tensors, **7.2 %** mode 9 (1,743 ms, buying 48.1 MB), **6.1 %**
publication/commit (1,489 ms), and a **0.065 %** residual. Every later rung
attacks one of those attribution lines and re-reports both boards.

At R18 the gate was **missed on wall, met on storage**: 4,117.8 ms against a
3,850 ms expectation is a 7.0 % miss, 1.23× G1, because analysis costs 348.8 ms
and the 66 skipped mode-9 encodes were worth only 93,696 bytes. R19 then made
G1, R20 made G2, and R21 made G3.

## The scoreboard

| requirement | target | measured |
|---|---|---|
| durable write wall | G1 ≤ 3,360 / G2 ≤ 1,344 / G3 ≤ 672 / final ≤ 336 ms | **592.6 min / 600.9 median → G3 met; final not met (1.78× A)** |
| physical footprint | ≤ 340,000,000 first / ≤ 284,140,000 final | **282,106,049 B** (final band, 33.5 % below A) |
| exact restores | 101/101 | **101/101** in every row, `mismatches: 0` |
| crash consistency | old-or-new whole | `crash_proxy_old_or_new: true` on every Weft row (three phases each), 0 partial |
| court limits | unchanged | 4 CPUs / 3 GiB; peak RSS 939,256 KiB (34 % of the court), 0 OOM, 0 pids refusals |
| Rust/Go format compatibility | no regression | Rust 807/0/0, FUSE 850/0/2 ignored, Rust↔Go cross gate passed, `verify --full` ok on a chunk-parallel-written store |

The wall ends when the commit record and superblock are published and every
blob is durable; the footprint is the reachable physical state that commit
names. No deferred or background transcode is counted, no row publishes raw
bytes and compresses later, and no FUSE writeback or page-cache trick is
involved.

## The Pareto frontier

25 configurations × 7 interleaved repetitions (`r22-pareto-20261112T230000Z`,
11/11 checks). Pareto-optimal wall against footprint:

| config | min ms | median | footprint B | verdict |
|---|---:|---:|---:|---|
| A (native control) | 332.0 | 339.7 | 424,439,296 | fastest, largest |
| **C-pipe4 (shipped)** | **592.6** | **600.9** | **282,106,049** | the knee: full 33.5 % reduction, portable |
| C-pipe4-zpar4 | 584.8 | 618.5 | 282,106,049 | 1.3 % faster, inside the host spread; not adopted |
| C-pipe4-t19off | 575.9 | 593.7 | 282,106,049 | Pareto here only; not shippable (loses pythia) |
| C-pipe4-tm9off | 593.6 | 649.2 | 330,355,860 | frontier only if 17.1 % more bytes are acceptable |

Everything else is dominated on both axes. The default is the knee, not the
minimum.

## The honest miss: 592.6 ms against 336 ms

The final rung is **not reached**. The measured limiter is the
**non-overlappable durable tail**: 282 MB written and `fsync`ed as 202 files in
**461.6 ms** (publication wall 452.5 ms) against native's 424 MB in 332 ms,
over a 288 ms CPU floor. R22's own projection was refuted by its own court:
chunk-parallel ZSTD cuts the 272 MB embedding's level-3 frame from 280.3 ms to
135.7 ms, and is byte-identical, but the fixture wall does not move because the
publication and writeback terms are larger than the encode term at this
operating point. The single change most likely to close the gap is to seal the
pack plane and publish its catalog **on the early-barrier thread**, so fewer,
larger durable objects do not pay a critical-path sealing cost (packs already
halve publisher busy time, 391 → 220 ms, and remove all 202 writeback files).
That is work for a later release, not a claim here.

## Every loss

* **Native parity is not reached** (1.78× A) and the ≤ 336 ms rung is not met.
* **R18 missed its wall gate** (7.0 %, 1.23× G1) while meeting storage.
* **A deeper pipeline buys nothing.** 8 workers are no better than 4
  (648.7 vs 592.6 ms); 2 workers are publisher-bound (926.5 ms).
* **RSS and CPU rise.** Peak RSS 270,532 → 939,256 KiB (34 % of the court) and
  CPU 100 → 1,150 ms for the wall it buys.
* **The pack plane loses for the third time** (962.7 ms against 592.6 ms; it
  halves publisher busy time and removes all 202 writeback files, but sealing
  and catalog publication cost more at this operating point).
* **No chunking is 5.9× slower** and saves 2.4 KB: without chunks the two-stage
  predictor falls back on 50 of 101 tensors and level 19 actually runs.
* **A level-19 gate with 4 MiB of leeway is 4.4× slower for zero bytes.**
* **16 KiB codec chunks are 13 % worse than 64 KiB** and identical in bytes.
* **The 576–612 ms cluster is a band, not an ordering.** The same row's median
  moved 13 % between sessions; only differences above ~10 % are treated as real.
* **Host degradation is disclosed.** Between clean runs the host's durability
  path fell from 1.28 GB/s to 125–180 MB/s and the native control wrote the
  fixture in 11,482.6 ms; those attempts produced no cited row
  (`r22-probe-20261112T163717Z` and superseded pareto receipts are retained).
* **R12c items 1–4 each publish losses**: a 4-step clean-step p50 rise of 13 %
  and a 32-step loop wall 4.3 % worse (sessions); index bytes still grow in the
  number of commits and 1M generations does not fit the court; the first DCP
  save of a shape pays a plan build (3.5–20.7 ms cold p50 rise); a bounded queue
  worsens completion p95 (47.7 → 85.9 ms) and raises RSS 12–19 %.
* **The multi-writer throughput table was not re-measured.** It remains the R4
  numbers at `b0bcbc5e`/`f46b351f`, which predate this release's write work.

## Non-claims

* **Single host, warm cache, local filesystem.** No fleet, cluster, or
  multi-host claim. The MinIO arm is **one loopback process with tmpfs data**,
  not AWS and not a network.
* **No physical power loss, host page-cache loss, or media-durability claim.**
  Crash evidence is a process-fault proxy; the block-layer courts are models.
* **No cloud claim.** No live AWS, Azure, or GCS credential was used.
* **No GPU claim.** Every measurement is CPU-only in the 4-vCPU court.
* **No MW3 claim**, no multi-node ordering, and no linearizability proof: the
  head sequence is a bounded TLA+ model plus finite-history tests.
* **No general-purpose-filesystem performance claim.** The Rust and Go
  filesystems are reference implementations; the comparator for every number
  above is the same backend with and without WeftFS.
* **No claim that the 592.6 ms figure generalises.** It is one fixture, four
  families measured for direction, and a stated band.
* **No registry publication in this release run.** See Artifacts.

## Verification

VERIFICATION_PLACEHOLDER

## Artifacts

ARTIFACTS_PLACEHOLDER

## Where to read next

* [`bench/RESULTS-index.md`](../bench/RESULTS-index.md) — the map, the current
  pair (write-cost ledger, Pareto frontier) and every retained loss.
* [`docs/STATUS.md`](STATUS.md) — current state, per-subphase evidence.
* [`docs/PLAN-R17-R22.md`](PLAN-R17-R22.md) and
  [`docs/PLAN-R12C.md`](PLAN-R12C.md) — what is done and what remains.
* [`docs/RELEASE-v1.3.0.md`](RELEASE-v1.3.0.md) — the previous release.
