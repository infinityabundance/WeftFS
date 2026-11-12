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

**Date:** 2026-11-12 · **Branch:** `staging` merged to `main` · **Tag:**
`v1.4.0` at the release commit (see below).

Every number in this note that is a current claim is emitted from a receipt
between `BEGIN GENERATED` sentinels by `scripts/perf_numbers.py`; the
`perf_numbers` court fails closed when a number drifts from its receipt. The
canonical table of every published number is
[`PERF-NUMBERS.md`](PERF-NUMBERS.md). Numbers that are requirements (the G1/G2/
G3/final targets) are prose, not measurements.

## The short version

Three numbers matter, and two of them are losses.

<!-- BEGIN GENERATED:release-short (scripts/perf_numbers.py) -->
| Question | Answer |
|---|---|
| Did the compressed write get faster? | **Yes, 40.9×**: 24,229.2 ms at R17 to **592.6 ms** least-contended / 600.9 ms median at R22, at the same 282,106,049 B |
| Did it reach native parity? | **No.** The native per-tensor control A is 332.0–339.7 ms; 592.6 ms is **1.78× A**. The ≤ 336 ms rung is **not met**, and the limiter is stated in §6 |
| Did the bytes change? | **No.** Every planned row publishes 282,106,049 B — **33.5 %** below A's 424,439,296 B — and every restores 101/101 exactly |
<!-- END GENERATED:release-short -->

The scoreboard: **G1 (≤ 3,360 ms) met at R19, G2 (≤ 1,344 ms) met at R20,
G3 (≤ 672 ms) met at R21 and held at R22; the final rung (≤ 336 ms) is not
met** — the measured wall against the 336 ms target is stated above.

## What changed

### R12c — checkpoint sessions, incremental indexes, metadata, and a bounded queue

<!-- BEGIN GENERATED:release-r12c (scripts/perf_numbers.py) -->
* **Persistent sessions (item 1).** One `CheckpointSession` per (store, run)
  instead of a helper process per checkpoint. Warm completion p50 falls
  **339.0 → 62.9 ms** at a 4-step interval; loop wall
  17,615.8 → **4,476.3 ms**; training-thread pause p50
  3.99 → **0.80 ms**. Every resumed state and next-step loss bit in
  the receipt matched. Receipt `training-goodput-session-20261111T220500Z.json`.
* **Operation-derived incremental index (item 2).** Publish no longer calls
  `StateIndex::build`. Commit p50 is **0.198 → 0.110 ms** at 1k
  generations and 0.112 ms at 100k — flat while history grows 100× — with
  byte-identical index segments; 10k live objects fall
  22.6 → **5.6 ms**. A same-revision control with maintenance disabled
  reproduces the baseline. Receipts
  `incremental-index-20261111T220000Z-baseline-delta-live64`,
  `…T230000Z-post-delta-live64`, `…T235000Z-post-delta-live10000`; analysis
  [`bench/RESULTS-incremental-index.md`](../bench/RESULTS-incremental-index.md).
* **Index GC bound per retained root (item 2.1).** Item 2 exposed a real defect:
  the retained-chain depth bound was applied to one `visited` set shared by every
  root, so index GC refused once several snapshots were retained and the index
  tree grew with commits. Fixed per root in both languages; all retained
  snapshots still load their full chain. Retained failures:
  `index-gc-bound-20261111T190100Z-prefix-8roots.json`, `…T194000Z-prefix-tail.json`.
* **DCP metadata encoding (item 3).** The pure-Python canonical encoder cost
  **3.957 ms p50**; a reusable plan plus a fast generic encoder take it to
  **0.555 ms** warm (0.119 ms signature + 0.405 ms assembly), with the
  reference encoder kept as the oracle and every plan proved equal to it before
  use; the first call pays a 71.8 ms plan build. Receipts
  `dcp-metadata-20261111T230500Z.json`, `…T235500Z.json`.
* **Bounded checkpoint queue (item 4).** `CheckpointSession` admits saves under
  `max_outstanding` (**default 2**), a staged-byte budget and a blocking wait with
  a 30 s ceiling; a refusal is `QueueFull` naming every bound and publishes
  nothing. At a 1-step interval the loop wall falls **7,415.3 → 5,761.7 ms**
  (42.5 → 48.9 ms warm p50, 85.9 ms p95), every save lands,
  `no_silent_skips` is true and every cell drains to `outstanding 0`. Receipt
  `checkpoint-queue-bound-after-20261112T043000Z.json`, baseline
  `checkpoint-queue-sweep-baseline-20261112T020000Z.json`.
<!-- END GENERATED:release-r12c -->

Retained failures for R12c: `index-gc-bound-20261111T190100Z-prefix-8roots.json`
and `…T194000Z-prefix-tail.json` (the shared retention bound before the fix).

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

**R14d was re-verified at the release pin** rather than deferred:
`bench/receipts/r14d-go-suite-20261112T221649Z.json` records the whole Go suite
under `-race` in the fuse court at WeftFS-Go `1fa3d32` (`cmd/weftfs`,
`fusefs`, `index`, `model`, `store` all `ok`, 0 failures), the CGO-disabled
build, `go vet`, and `TestAzureOptimisticTwoDisjointCommitters` passing. The
minimal `go` court cannot run the `fusefs` half because it has no
`/bin/fusermount`; that court's limitation is recorded in the receipt, and the
supported whole-suite court is the fuse court.

### R17–R22 — the compressed-write ladder

The comparator is **the same backend with and without the WeftFS
protocol+codec**, never the reference FUSE filesystem: **A** is a native
per-tensor durable write of the same 101 tensors, **C** is the WeftFS planned
write, both measured in the same session. Method frozen in
[`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md);
fixture Qwen2.5-0.5B-Instruct, 101 tensors, 424,439,296 logical bytes, 4 CPUs,
3 GiB court.

<!-- BEGIN GENERATED:release-ladder (scripts/perf_numbers.py) -->
| Rung | Durable write wall | Footprint | What bought it | Receipt |
|---|---:|---:|---|---|
| R17 (start) | 24,229.2 ms | 282,283,011 B | nothing — this is the attribution baseline | `write-cost-ablation-20261112T052203Z` |
| R18 | 4,117.8 ms | 282,283,011 B | exact two-stage planner: analyse → select → materialise; level 19 conditional, not deleted | `planner-two-stage-20261112T081151Z` |
| R19 | 2,328.4 ms | 282,283,011 B | byte-identical mode-9 encoder, 2.85× sequential / 5.81× at four emission threads | `mode9-encoder-20261112T160000Z`, `r19-wall-after-20261112T140000Z` |
| R20 | 1,236.8 ms | **282,106,049 B** | one group commit and **one** durability barrier per checkpoint | `r20-publication-20261112T150000Z` |
| R21 | 623.3 ms | 282,106,049 B | bounded reader → N encoders → one publisher pipeline plus the early-barrier writeback thread | `r21-pipeline-20261112T201500Z` |
| **R22 (shipped)** | **592.6 ms** (median 600.9) | **282,106,049 B** | chunk-parallel ZSTD (byte-identical), exact parallel analyse, measured defaults | `r22-pareto-20261112T230000Z` |
| **target** | **≤ 336 ms** | ≥ first-success band | **not met** | — |
<!-- END GENERATED:release-ladder -->

<!-- BEGIN GENERATED:release-ladder-intro (scripts/perf_numbers.py) -->
The R17 wall was dominated by one term: level-19 ZSTD accounted for
84.0 % of it (20,353.0 ms) and was selected for **zero** tensors.
Every later rung attacks one attribution line and re-reports both boards.
<!-- END GENERATED:release-ladder-intro -->

At R18 the gate was **missed on wall, met on storage**: analysis cost was real
and the skipped mode-9 encodes were worth little. R19 then made G1, R20 made
G2, and R21 made G3.

## The scoreboard

<!-- BEGIN GENERATED:release-scoreboard (scripts/perf_numbers.py) -->
| requirement | target | measured |
|---|---|---|
| durable write wall | G1 ≤ 3,360 / G2 ≤ 1,344 / G3 ≤ 672 / final ≤ 336 ms | **592.6 min / 600.9 median → G3 met; final not met (1.78× A)** |
| physical footprint | ≤ 340,000,000 first / ≤ 284,140,000 final | **282,106,049 B** (final band, 33.5 % below A) |
| exact restores | 101/101 | **101/101** in every row, `mismatches: 0` |
| crash consistency | old-or-new whole | `crash_proxy_old_or_new: true` on every Weft row (three phases each), 0 partial |
| court limits | unchanged | 4 CPUs / 3 GiB; peak RSS 939,256 KiB (939,256 KiB of 3,145,728 KiB available), 0 OOM, 0 pids refusals |
| Rust/Go format compatibility | no regression | Rust 772/0/0, FUSE 815/0/2 ignored, Rust↔Go cross gate passed, `verify --full` ok on a chunk-parallel-written store |
<!-- END GENERATED:release-scoreboard -->

The wall ends when the commit record and superblock are published and every
blob is durable; the footprint is the reachable physical state that commit
names. No deferred or background transcode is counted, no row publishes raw
bytes and compresses later, and no FUSE writeback or page-cache trick is
involved.

## The Pareto frontier, stated precisely

25 configurations × 7 interleaved repetitions (`r22-pareto-20261112T230000Z`,
11/11 checks). Two different sets are called a frontier in this document, and
they are not the same set.

**1. The fixture numerical frontier** is the non-dominated set on the two
measured axes — least-contended wall and physical footprint — over every row in
this exact fixture, with no policy filter. It is:

<!-- BEGIN GENERATED:release-frontier (scripts/perf_numbers.py) -->
**A** (332.0 ms, 424,439,296 B), **C-pipe4-t19off** (575.9 ms, 282,106,049 B), **C-pipe4-pack1m** (962.7 ms, 282,102,473 B).
<!-- END GENERATED:release-frontier -->

**The shipped default is not in it.** `C-pipe4` has the same footprint as
several rows with a lower measured wall, so it is dominated on the measured
point estimates; the level-19-off row is on the frontier and the 1 MiB-pack row
is the smallest-footprint point.

**2. The portable/eligible policy frontier** removes configurations that
survive only by eliminating a candidate that another model family needs. On the
same axes it is:

<!-- BEGIN GENERATED:release-eligible-frontier (scripts/perf_numbers.py) -->
**A** (332.0 ms, 424,439,296 B), **C-pipe4-sync16** (582.0 ms, 282,106,049 B), **C-pipe4-pack1m** (962.7 ms, 282,102,473 B). `C-pipe4-t19off` is excluded by policy: disabling the level-19 gate wins on this fixture and loses pythia-70m (see `RESULTS-pareto.md` §6.1).
<!-- END GENERATED:release-eligible-frontier -->

But the point estimates inside the middle band are not separable by this
measurement: the same row's **median moved 13 % between sessions** while its
least-contended value moved 2.5 %, and R22 does not reproduce R21's clean-host
ordering of the sync granularities. So the default is chosen from a
**statistically indistinguishable band**, not because it is the minimum:

| config | min ms | median | footprint B | verdict |
|---|---:|---:|---:|---|
<!-- BEGIN GENERATED:release-pareto (scripts/perf_numbers.py) -->
| config | min ms | median ms | footprint B | verdict |
|---|---:|---:|---:|---|
| `A` | 332.0 | 339.7 | 424,439,296 | native anchor: fastest, largest |
| `C-pipe4` | 592.6 | 600.9 | 282,106,049 | shipped default: inside the measurement band, dominated on the point estimates |
| `C-pipe4-zpar4` | 584.8 | 618.5 | 282,106,049 | inside the band; not adopted (pool policy) |
| `C-pipe4-t19off` | 575.9 | 593.7 | 282,106,049 | numerical frontier; not policy-eligible (disables the level-19 gate) |
| `C-pipe4-tm9off` | 593.6 | 649.2 | 330,355,860 | dominated on (wall, footprint); its restore is faster, a third axis this frontier does not price |
<!-- END GENERATED:release-pareto -->

`C-pipe4` is the default because R21 measured the 8 MiB barrier as the best
granularity on a clean host, and R22 did not separate the alternatives at this
operating point. It is **not** claimed to be Pareto-optimal, and the earlier
wording that called it the "knee" of a Pareto frontier is withdrawn.

The full 25-row table, with every loss, is in
[`bench/RESULTS-pareto.md`](../bench/RESULTS-pareto.md) §4.

## The honest miss: the measured wall against 336 ms

The final rung is **not reached**. The measured limiter is the
**non-overlappable durable tail**:

<!-- BEGIN GENERATED:release-limiter (scripts/perf_numbers.py) -->
* The early-barrier thread `fsync`s **202 files / 282,118,555 B in
  461.6 ms** (611 MB/s) while native writes A's 424,439,296 B
  in 332.0 ms (1,278 MB/s). The publisher's own wall is
  452.5 ms. The last tensors publish after the last encode, so the tail
  is charged in full.
* The encode term is 614.8 ms summed over threads (level 3
  319.9, mode 9 229.4), digest 74.7, analyse
  156.9. The wall is not encode-bound at this operating point; it is
  publication- and writeback-bound.
<!-- END GENERATED:release-limiter -->

R22's own projection was refuted by its own court: chunk-parallel ZSTD cuts the
272 MB embedding's level-3 frame, and is byte-identical, but the fixture wall
does not move because the publication and writeback terms are larger than the
encode term at this operating point. The single change most likely to close the
gap is to seal the pack plane and publish its catalog **on the early-barrier
thread**, so fewer, larger durable objects do not pay a critical-path sealing
cost (packs already halve publisher busy time and remove every writeback file).
That is work for a later release, not a claim here.

## Every loss

<!-- BEGIN GENERATED:release-losses (scripts/perf_numbers.py) -->
* **Native parity is not reached** (1.78× A) and the ≤ 336 ms rung is not met.
* **R18 missed its wall gate** while meeting storage: analysis cost was real and
  the skipped mode-9 encodes were worth little (receipt
  `planner-two-stage-20261112T081151Z`).
* **A deeper pipeline buys nothing.** 8 workers are no better than 4
  (648.7 vs 592.6 ms); 2 workers are publisher-bound
  (926.5 ms).
* **RSS and CPU rise.** Peak RSS 270,532 → 939,256 KiB
  (29.9 % of the 3 GiB court, 3.5× native) and CPU
  100 → 1,150 ms.
* **The pack plane loses for the third time** (962.7 ms against
  592.6 ms, 1.6×). It halves publisher busy time
  (391.2 → 219.5 ms) and removes every writeback file, but
  sealing and catalog publication cost more at this operating point.
* **No chunking is slower** (3,518.1 ms) and saves only
  2,400 B.
* **A level-19 gate with 4 MiB of leeway is 4.3× slower for zero
  bytes.**
* **16 KiB codec chunks are 13.4 % worse than 64 KiB** and identical
  in bytes.
* **The rows inside the host's measured spread are a band, not an ordering.**
  The same row's median moved 13 % between sessions while its least-contended
  value moved 2.5 %; only differences larger than ~10 % are treated as real.
  Superseded sweeps (`r22-pareto-20261112T220000Z`, `…T193000Z`, `…T190000Z`,
  `…T165731Z`) are retained and not cited.
* **Host degradation is disclosed.** Between clean runs the host's durability
  path fell from 1.28 GB/s to 125–180 MB/s and the native control wrote the
  fixture in 11,482.6 ms; those attempts produced no cited row
  (`r22-probe-20261112T163717Z` and the superseded pareto receipts are
  retained).
* **R12c items 1–4 each publish losses**: a 4-step clean-step p50 rise and a
  32-step loop wall that is worse (sessions); index bytes still grow in the
  number of commits and 1M generations does not fit the court; the first DCP
  save of a shape pays a plan build; a bounded queue worsens completion p95 and
  raises RSS. Each is quantified in its own receipt.
* **The multi-writer throughput table was not re-measured.** It remains the R4
  numbers at `b0bcbc5e`/`f46b351f`, which predate this release's write work.
<!-- END GENERATED:release-losses -->

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
* **No claim that the shipped write wall generalises.** It is one fixture, four
  families measured for direction, and a stated band.
* **No registry publication in this release run.** See Artifacts.

## Verification

<!-- BEGIN GENERATED:release-verification (scripts/perf_numbers.py) -->
`bench/receipts/verify-20261111T182601Z.json`: **45/45** courts pass,
0 failed, 0 skipped, result `pass`, at WeftFS
`3ab54c75b7205e9897e185aa1fac670b6dff16ff` (WeftCodec `58db5aa`, WeftCodec-Go `9812d15`,
WeftFS-Go `1fa3d32`). Rust all-targets **772** passed / 0 failed; FUSE
all-targets **815** passed / 0 failed / 2 ignored (the two
pre-existing kernel/`CAP_SETUID` tests); WeftCodec **267** passed; the Go
whole suite passes under `-race` in the fuse court (R14d receipt
`bench/receipts/r14d-go-suite-20261112T221649Z.json`). The conformance manifest
holds **420** vectors, aggregate `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`; the claims manifest
holds **1,046** entries and `scripts/check-claims.sh` passes.
<!-- END GENERATED:release-verification -->

## Artifacts

<!-- BEGIN GENERATED:release-artifacts (scripts/perf_numbers.py) -->
ARTIFACTS_PENDING
<!-- END GENERATED:release-artifacts -->

## Where to read next

* [`bench/RESULTS-index.md`](../bench/RESULTS-index.md) — the map, the current
  pair (write-cost ledger, Pareto frontier) and every retained loss.
* [`docs/STATUS.md`](STATUS.md) — current state, per-subphase evidence.
* [`docs/PERF-NUMBERS.md`](PERF-NUMBERS.md) — every published number with its
  receipt, stamp and revision.
* [`docs/PLAN-R17-R22.md`](PLAN-R17-R22.md) and
  [`docs/PLAN-R12C.md`](PLAN-R12C.md) — what is done and what remains.
* [`docs/RELEASE-v1.3.0.md`](RELEASE-v1.3.0.md) — the previous release.
