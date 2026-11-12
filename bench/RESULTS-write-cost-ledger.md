# Write-cost ledger — R17 attribution and ablation

**Status:** executed. This document accounts for the entire measured write wall
of the C (codec) configuration on the frozen 424.44 MB Qwen2.5-0.5B-Instruct
fixture, and reports the independent ablations that attribute it. Every number
below comes from a committed receipt; nothing is extrapolated.

**The framing.** This is not the WeftFS reference FUSE filesystem compared with
a mature filesystem. It is **one backend with and without the WeftFS
augmentation**. A is that backend's own durable per-tensor write path; B adds
the WeftFS store protocol with a pass-through representation; C adds the codec;
D adds the context/provenance protocol; E adds both. The reference filesystem
is not a comparator.

The six-phase recovery plan this ledger drives is docs/PLAN-R17-R22.md.

## 1. Receipts

| Receipt | What it is |
|---|---|
| [`write-cost-ablation-20261112T052203Z.json`](receipts/write-cost-ablation-20261112T052203Z.json) | **cited.** 7 interleaved repetitions × 10 configurations, phase profile on every repetition, per-tensor detail on repetition 1, crash proxy per Weft configuration, all checks pass. |
| [`augmentation-local-20261112T041225Z.json`](receipts/augmentation-local-20261112T041225Z.json) | **cited.** Unmodified contract court at this revision (A/B/C/D/E/N, one run each). Its C row is the same-revision identity proof for the ablation runner's C. |
| [`bf16-plane-profile-20261112T054210Z.json`](receipts/bf16-plane-profile-20261112T054210Z.json) | **cited.** BF16 split / Huffman stage profile, embedding + 8 × 8.7 MB MLP tensors, 5 repetitions. |
| `write-cost-ablation-20261112T050102Z.json` | Retained repeat. Same code; six of ten checks passed, the two failures were harness-classification bugs for the native rows (A/N have no store to crash; A/N footprints were read from the wrong JSON path). Its clean minima agree with the cited run. |
| `write-cost-ablation-20261112T043910Z.json` | Retained repeat (before the crash-baseline fix; D/E read the pre-protocol generation). |
| `write-cost-ablation-20261112T041701Z.json` | Retained first matrix (before the identity/crash fixes). |
| `augmentation-local-20261112T041011Z.json` | Retained contract court: D was host-contended (46.9 s). |
| `augmentation-local-20261112T041345Z.json` | Retained contract court: A/B/C were host-contended (7.1/42.2/54.0 s). |
| `bf16-plane-profile-20261112T054141Z.json` | Retained first stage profile (its non-embedding sample was eight 1,792-byte layernorms, so it exercises only the embedding; superseded by the largest-tensor selection). |

## 2. Methodology

- **Fixture:** `.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors`,
  988,097,824 bytes, SHA-256 `fdf756fa…88fb7fe`, re-hashed by the harness before
  any write. Subset: the 272,269,312-byte BF16 embedding plus 100 evenly spaced
  tensors = **101 tensors, 424,439,296 logical bytes**.
- **Court:** `weft/bench-court:1`
  `sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
  kernel `7.3.3-1-cachyos`, 3 GiB memory with swap == memory, 4 CPUs, 1024 pids,
  bounded `/tmp`, `no-new-privileges`, `cap-drop=ALL`. Limits were not raised.
- **Process isolation:** every write and restore is a fresh child process
  (`VmHWM`, cold page-cache control). Nothing is reused between configurations.
- **Repetitions:** 7 per configuration. The configuration order is **rotated by
  one position per repetition**, so no configuration always occupies the same
  slot. Raw samples are in the receipt; median and p95 are computed from them.
- **Phase profile:** enabled on every timed repetition (`phase_profile::begin`
  around the write child). The per-tensor breakdown is emitted on repetition 1.
  The profiled C minimum (24,229.2 ms) is **0.9 % below** the unprofiled
  official C (24,459.1 ms), so the instrumentation is not measurable above host
  noise.
- **Attribution repetition:** the ledger describes the **least-contended**
  repetition (minimum wall). The median and p95 are published unchanged. The
  reason is in §3.
- **Ledger closure:** the ledger's categories plus an explicit residual sum to
  the measured wall of the run it describes. The residual is 15.86 ms
  (**0.065 %**) for C and 9.34 ms (0.44 %) for the one-group-commit B.
- **Exactness:** every configuration restored all 101 tensors with SHA-256
  equality in every repetition (`mismatches: 0`).
- **Crash proxy:** every Weft configuration ran three process-fault phases
  (`after_objects`, `after_commit_file`, `after_superblock_write`) and reopened
  the store with a full verify and a 101-tensor byte comparison.

### 3. Host contention is disclosed, not smoothed away

The host was shared with an external, unrelated CPU/I/O-heavy process during
these runs (observed `python3` at ~99 % CPU and multi-second I/O freezes; the
court itself stayed inside its limits). The distributions are bimodal: a clean
observation of a 2.8 s configuration and a 26–43 s observation that stalls
inside publication. Examples (ms, raw):

| Config | clean samples | stalled samples |
|---|---|---|
| B | 2,840 / 6,768 / 7,291 / 9,111 / 10,796 | 34,761 / 38,771 |
| C | 24,229 / 24,341 | 45,622 / 49,940 / 50,127 / 51,025 / 51,907 |
| C-noz19 | 3,850 / 3,898 / 3,913 / 3,927 / 3,931 / 3,980 | 12,464 |
| C-both | 2,133 / 2,143 / 2,145 / 2,151 / 2,154 / 2,157 | 28,269 |
| B-group | 2,139 / 2,142 / 2,159 / 2,161 / 2,163 / 2,168 | 36,154 |

The least-contended minima agree with the independent one-run official court
within 0–2 % (C 24,229 vs 24,459; B 2,840 vs 2,892; D 2,886 vs 2,910; E 24,171
vs 24,175), which is why the minima — not the contaminated medians — are used
for attribution. The medians and p95 in the tables are the observed
distribution, not an estimate of the intrinsic cost. This is a single-host,
shared-host disclosure; no fleet or cloud claim is made.

## 4. Baseline at this revision (official court, unprofiled)

| Metric | A Native | B Pass-through | C Codec | D Protocol | E Combined | N (supplementary) |
|---|---:|---:|---:|---:|---:|---:|
| Durable write wall (ms) | 364.5 | 2,892.1 | 24,459.1 | 2,910.4 | 24,174.7 | 5,504.4 † |
| Physical footprint (B) | 424,439,296 | 424,784,917 | 282,283,011 | 424,856,058 | 282,354,152 | 424,477,171 |
| Bytes written (B) | 424,439,296 | 425,207,023 | 282,705,117 | 425,235,823 | 282,733,917 | 424,477,171 |
| Write CPU user+sys (ms) | 110 | 280 | 22,830 | 270 | 22,610 | 100 |
| Write requests | 101 | 911 | 516 | 911 | 516 | 104 |
| Peak write RSS (KiB) | 271,324 | 272,624 | 1,186,988 | 272,364 | 1,189,024 | 271,252 |
| Chosen representations | 101 files | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 files + 3 metadata |
| Cold restore (ms) | 350.6 | 545.1 | 1,441.2 | 561.4 | 1,429.7 | 398.8 |
| Restore mismatches | 0 | 0 | 0 | 0 | 0 | 0 |

† The N row in this receipt was host-contended (the earlier `041011Z` receipt
measured N at 358.2 ms). N is supplementary; it never replaces a required row.

The same-revision identity check in the ablation receipt compares the official
C write with the ablation runner's C write: strategies, per-tensor manifest set,
physical footprint and bytes written are **identical**. The store head differs
because the harness creates `/tensors` with a random ObjectId seed (only tensor
objects are seeded); that is recorded, not hidden.

## 5. Ablation matrix (7 interleaved repetitions)

All numbers are milliseconds. `min` is the least-contended observation; `median`
and `p95` are the observed distribution (see §3). Footprint, strategies,
requests and exactness are identical in all seven repetitions unless stated.

| Config | min | median | p95 | CPU median | RSS median KiB | Footprint (B) | Written (B) | Requests | Strategies |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 326.4 | 339.7 | 401.1 | 110 | 271,252 | 424,439,296 | 424,439,296 | 101 | 101 files |
| B | 2,840.2 | 9,110.9 | 38,770.6 | 280 | 272,552 | 424,784,917 | 425,207,023 | 911 | raw 101 |
| **B-group** | **2,139.3** | 2,160.8 | 36,154.3 | 240 | 272,452 | 424,608,542 | 424,621,048 | 611 | raw 101 |
| C | 24,229.2 | 49,940.4 | 51,906.5 | 22,890 | 1,188,240 | 282,283,011 | 282,705,117 | 516 | float-tensor 35, zstd 50, raw 16 |
| **C-noz19** | **3,849.9** | 3,926.6 | 12,464.1 | 2,480 | 1,187,360 | 282,283,011 | 282,705,117 | 516 | float-tensor 35, zstd 50, raw 16 |
| **C-nom9** | **22,443.3** | 22,659.3 | 22,798.3 | 21,050 | 893,828 | 330,336,572 | 330,758,678 | 516 | zstd 85, raw 16 |
| **C-both** | **2,132.6** | 2,151.4 | 28,269.4 | 700 | 686,400 | 330,532,422 | 330,954,528 | 516 | zstd 85, raw 16 |
| D | 2,885.6 | 2,979.9 | 54,832.5 | 280 | 272,624 | 424,856,058 | 425,235,823 | 911 | raw 101 |
| E | 24,171.0 | 24,390.9 | 42,194.2 | 22,760 | 1,187,792 | 282,354,152 | 282,733,917 | 516 | float-tensor 35, zstd 50, raw 16 |
| N | 340.7 | 346.6 | 6,966.3 | 100 | 271,280 | 424,477,171 | 424,477,171 | 104 | 101 files + 3 metadata |

Cold restore medians (ms): A 352.1, B 635.6, B-group 536.1, C 1,558.7,
C-noz19 1,427.6, C-nom9 885.3, C-both 840.4, D 552.2, E 1,439.1, N 350.2.
Every configuration restored 101/101 tensors byte-exactly in every repetition.

### What each ablation removes

| Difference | Write wall delta | Footprint delta | Reading |
|---|---:|---:|---|
| C − C-noz19 | **+20,379.3 ms** | **0 B** | Level-19 ZSTD encodes for 18 tensors and is selected for **none**. |
| C − C-nom9 | **+1,785.9 ms** | **−48,053,561 B** | Mode 9 costs ~1.7–1.8 s and saves ~48 MB (14.5 %). |
| C-noz19 − C-both | +1,717.3 ms | −48,249,411 B | Same mode-9 trade without the level-19 noise. |
| C-nom9 − C-both | +20,310.7 ms | −195,850 B | Level-19 saves 0.059 % of footprint when mode 9 is out of the way. |
| B − B-group | +700.9 ms | +176,375 B | One group commit saves ~0.70 s and ~176 KB. |
| B − C-both | +707.6 ms | +94,252,495 B | ZSTD-3 + 101 frames is faster than 405 raw chunk blobs, and 22.2 % smaller. |
| B − C-noz19 | −1,009.7 ms | −142,501,906 B | Mode 9 alone costs 1.0 s over B and saves 33.6 % of footprint. |

The C encoding count is exactly: 101 level-3 ZSTD encodes, 18 level-19 ZSTD
encodes, 101 mode-9 encodes. With `zstd19 = false` the level-19 count is 0 and
the mode-9 count is 101; with `float_tensor = false` the mode-9 count is 0 and
the level-19 count is 18; with both off both counts are 0. The checks are in the
receipt (`ablation_encode_counts_exact`) and in
`tests/float_tensor_ablation.rs`.

## 6. The complete C ledger

Least-contended repetition (C, 24,229.17 ms). Nested phases are shown under
their parent and are never added twice.

| Category | ms | % of wall |
|---|---:|---:|
| init (store create, FORMAT, LOCK, superblock) | 24.67 | 0.10 |
| source read (fixture → memory) | 32.27 | 0.13 |
| **prepare** | **23,443.43** | **96.76** |
| ├ encode total | 22,574.28 | 93.17 |
| │ ├ **ZSTD level-19 candidate** | **20,352.99** | **84.00** |
| │ ├ mode-9 `FLOAT_TENSOR` candidate | 1,743.05 | 7.19 |
| │ ├ ZSTD level-3 candidate | 436.78 | 1.80 |
| │ └ other encode/plan (raw descriptor, pricing, mode-9 logical root) | 41.46 | 0.17 |
| ├ blob publish (frame + manifest blobs, 202 publications) | 777.37 | 3.21 |
| ├ digest (blob ids + content roots) | 66.33 | 0.27 |
| └ prepare unattributed | 25.45 | 0.11 |
| **commit** | **711.51** | **2.94** |
| ├ index segment (advisory, per commit) | 343.47 | 1.42 |
| │ └ of which index attestation | 3.37 | 0.01 |
| ├ manifest commit (commit record + superblock publication) | 362.86 | 1.50 |
| └ commit unattributed | 5.17 | 0.02 |
| protocol | 0.00 | 0.00 |
| footprint walk | 1.43 | 0.01 |
| **accounted** | **24,213.31** | **99.94** |
| **unattributed residual** | **15.86** | **0.065** |
| **measured wall** | **24,229.17** | **100.00** |

**Ordering that drives R18–R22:** ZSTD-19 (84.0 %) ≫ mode 9 (7.2 %) >
blob publish (3.2 %) > commit/index (2.9 %) > ZSTD-3 (1.8 %) > digest (0.3 %) >
source read + init (0.2 %) > residual (0.065 %).

### Residual and what to instrument next

The residual is 15.86 ms for a 24.2 s write (0.065 %). The remaining
un-instrumented work is: the harness loop itself, the `prepare` prologue
(canonicalisation checks, attribute assembly), the `commit` prologue, and the
`fsync` calls, which are **inside** `BlobPublish` and `ManifestCommit` on the
local backend. A separate `BackendSync` phase in `src/backend.rs` would move
`fsync` out of those two categories and make the ledger finer, but it cannot
change the ordering above.

## 7. BF16 split vs Huffman breakdown

Receipt `bf16-plane-profile-20261112T054210Z.json`. Stage timings come from a
harness-local replica of the normative `huffman-canonical-v1` and BF16 split;
the replica's bytes are checked against `weftcodec::huffman::encode` and
`weftcodec::modes::split_float_tensor_payload` on every measured tensor before
any number is recorded (`replica_validated: true`). Five repetitions, median.

**Embedding (272,269,312 B, 136,134,656 BF16 elements):**

| Stage | ms | ms/MB | share of Huffman |
|---|---:|---:|---:|
| BF16 plane split | 307.31 | 1.13 | — |
| Huffman total (shipped encoder) | 337.61 | 1.24 | 100 % |
| ├ histogram | 57.76 | 0.21 | 17.1 % |
| ├ canonical lengths (tree) | 0.023 | 0.0001 | 0.007 % |
| ├ canonical codes | 0.003 | 0.00001 | 0.001 % |
| └ bit emission | 266.55 | 0.98 | 79.0 % |
| Hash: values root | 27.07 | 0.10 | — |
| Hash: payload root | 17.84 | 0.07 | — |
| Hash: content root | 27.06 | 0.10 | — |
| **`encode_float_tensor` total** | **1,163.80** | 4.28 | — |
| frame assembly and codec-internal work (total − the rows above) | 446.90 | 1.64 | — |
| output / input | 0.663 | | |
| exponent Huffman bytes + mantissa bytes | 180.5 MB / 268.4 MB | | |

**Eight 8,716,288 B MLP tensors (same pattern):** split 11.0–12.0 ms, Huffman
9.6–10.9 ms, histogram 1.8–1.9 ms, lengths/codes < 0.01 ms, emission
7.2–8.5 ms, `encode_float_tensor` 35.1–38.0 ms, frame assembly 11.9–13.7 ms,
ratio 0.663–0.669.

**Totals over the nine tensors (341,999,616 B):** split 399.04 ms, Huffman
418.57 ms (histogram 72.38, lengths 0.065, codes 0.018, emission 329.34 ms).

The ledger attributes 1,743.05 ms to all 101 mode-9 encodes; the standalone
embedding encode is 1,163.80 ms (1,284.28 ms in the first run). The difference
is the other 100 tensors plus the planner's context (held candidates,
page-cache and allocator state). The stage proportions, not the absolute wall,
are the transferable result: **emission is ~79 % of Huffman, histogram ~17 %,
and tree/code construction is free**. Frame assembly is the largest
un-reviewed block after emission.

## 8. One-group-commit B: what publication actually costs

B publishes 101 files as 101 separate commits. `B-group` prepares the same
tensors and publishes all 101 state ops in **one** group commit through the
shipped `Store::group()` path; no second transaction engine was added.

| Phase | B (ms) | B-group (ms) |
|---|---:|---:|
| init | 24.4 | 19.7 |
| source read | 30.7 | 30.6 |
| prepare | 2,074.4 | 2,069.1 |
| ├ blob publish (405 chunk blobs + 101 manifests) | 1,948.5 | 1,943.5 |
| ├ digest | 83.8 | 83.7 |
| └ other | 42.1 | 41.9 |
| **commit** | **696.9** | **8.8** |
| ├ index segment | 338.9 | 0 (one segment in the group commit) |
| └ commit record + superblock publication | 353.4 | 8.8 |
| footprint walk | 1.7 | 1.7 |
| residual | 11.7 | 9.3 |
| **wall** | **2,840.2** | **2,139.3** |

Publication of the state (index + commit record + superblock) costs **~689 ms**
for 101 commits, or **~6.8 ms per commit**, and collapses to **8.8 ms total**
when the same operations are batched. Blob publication is unchanged
(1,948.5 → 1,943.5 ms): the group commit removes the *control-plane* tax, not
the data-plane cost. Backend requests fall 911 → 611 and footprint falls
176,375 B because 100 fewer index/commit records are written.

The important cross-check: `C-both` (2,132.6 ms) is as fast as `B-group`
(2,139.3 ms) while being 22.2 % smaller, because its 330 MB of data publishes in
793 ms instead of 1,948 ms and its state commits still cost ~698 ms. The C
configuration without the two expensive candidates is already at the
native-write floor of the store; the cost of compression is not the problem.

## 9. Crash consistency

Every Weft configuration (B, B-group, C, C-noz19, C-nom9, C-both, D, E) ran
three aborts and reopened the store. Result in all eight: **old 2 / new 1 /
partial 0**, full verify consistent, and 101/101 tensors byte-exact after every
reopen.

- `after_objects` → old head; only unreferenced blobs remain.
- `after_commit_file` → old head plus one unreferenced commit tail. `verify`
  reports exactly that one condition by design
  (`commit file for generation N exists above the highest published superblock
  generation M`); recovery ignores it. It is not a partial state.
- `after_superblock_write` → the whole new generation; exact.

The baseline generation is read from the store immediately before the probe,
because D/E publish protocol commits after the tensor writes.

## 10. Losses and limits

- **Host contention.** The medians and p95 above are contaminated by an
  external process (§3). The minima are the attributable observations. This is
  disclosed in the receipt and here; it is not corrected away.
- **ZSTD-19 is pure cost on this fixture.** 18 encodes, 20.35 s (84 % of the
  wall), selected for zero tensors and saves zero bytes, zero restore
  difference. When mode 9 is disabled it saves 195,850 B (0.059 %) for
  20.31 s. The candidate's size gate (1 MiB) and CPU budget (20 s) do not
  predict a win.
- **Mode 9 is a real trade, not free.** 1.74 s write, ~0.57–0.72 s extra cold
  restore, +~496 MB peak write RSS, for 48.1–48.25 MB (14.5–14.6 %). It is the
  only candidate on this fixture that materially reduces footprint.
- **C-both is faster than B** (2,132.6 vs 2,840.2 ms) and 22.2 % smaller. That
  is a protocol+ZSTD-3 result, not a mode-9/zstd-19 result.
- **RSS:** C peaks at 1,188,240 KiB (~1.13 GiB, 37.8 % of the 3 GiB court);
  no run approached the limit. Mode 9 accounts for ~496 MB of C's RSS.
- **Per-phase CPU is not instrumented.** The CPU column is the whole write
  child's user+sys time. Each tensor is encoded on one thread, so a candidate's
  measured wall is an upper bound on its CPU, not a separate CPU measurement.
- **Allocator counters are not exposed** by the standard allocator; peak RSS is
  the memory evidence, reported per configuration and per repetition.
- **The per-tensor detail is repetition 1 only.** Later repetitions publish
  aggregate wall/CPU/RSS/footprint and strategy counts, not another 101 rows.
  In the cited receipt, skipped level-19 candidates are labelled
  `zstd_level: 3` in the per-tensor rows; they are `selected: false` and the
  authoritative encode counts come from the phase collector (`encode_counts`).
  The label was corrected after the run.
- **Single host, warm caches, tmpfs-free local filesystem.** No media,
  power-loss, fleet, cloud, or multi-host claim.
- **The stage profile is a replica**, validated byte-for-byte against the codec
  on every measured tensor, not codec-internal timing.
- **`fsync` is not a separate phase**; it is inside BlobPublish and
  ManifestCommit. The residual is already 0.065 %, so this changes proportions
  only at the ~3 % level.
- **The store head is not reproducible across processes** because `/tensors`
  is created with a random ObjectId seed. Content roots and per-tensor
  manifests are reproducible and were compared.

## 11. What the ledger implies for R18–R22

1. **R18 (candidate elimination):** remove or gate the level-19 ZSTD candidate
   from the planned path. It is 84.0 % of the C wall and 0 bytes of benefit on
   the real fixture. A cheap predictor is not needed to justify removing it on
   this evidence; if it is kept for another corpus, it must be opt-in and
   outside the default critical path.
2. **R19 (BF16 encoder):** mode 9 is 7.2 % of the wall and buys 14.5 % footprint.
   The stage profile says emission (79 % of Huffman) and frame assembly
   (446.9 ms for the embedding) are the targets if that 7.2 % is to fall
   without losing the 48 MB.
3. **R20 (publication tax):** 689 ms of the B/B-group delta is control-plane
   publication; `C-both` still pays ~698 ms of per-tensor commit. Batched
   publication (or the existing group commit applied to C) is worth ~0.7 s.
4. **R21 (bounded parallel pipeline):** after R18, C-noz19 is 3.85 s, of which
   1.75 s is mode-9 encode and ~0.78 s blob publish. Parallel tensor encode and
   publication are the remaining levers.
5. **R22 (Pareto sweep):** with level 19 removed, the Pareto front on this
   fixture is approximately `C-both` (2.13 s / 330.5 MB), `C-noz19`
   (3.85 s / 282.3 MB), and the ladder target `A` (0.33 s / 424.4 MB).
   `C-noz19` is 1.15× the first rung (≤3,360 ms), so G1 still needs ~0.5 s from
   R20/R21; the final rung (≤336 ms) is not reachable with a 330 MB write on
   this backend.

## 12. Not claimed

- No claim that these proportions hold on another corpus, another backend, a
  cold page cache, a fleet, or a GPU.
- No claim that ZSTD-19 is worthless in general; the claim is specific: on the
  frozen 101-tensor Qwen2.5-0.5B-Instruct subset it is never selected and saves
  no bytes.
- No claim that the medians are the intrinsic cost; the host was shared and the
  raw distribution is published.
- No claim of physical power-loss durability; the crash probe is a process-fault
  proxy.
- No claim that the stage-timing replica is a profiler of the codec's internals.
- No change to any frozen format, manifest, commit record, index layout, or
  codec frame byte.
