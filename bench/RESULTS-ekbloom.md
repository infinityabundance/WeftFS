# EKBLOOM evaluation — results (EKB-X22; the line is retired)

The frozen contract is [`bench/CONTRACTS-ekbloom.md`](CONTRACTS-ekbloom.md)
(version 1 for E1/E2, version 2 §9 for E3), committed before the corresponding
measurement. The gates and the pre-registered prediction are in
[`docs/EKBLOOM-PLAN.md`](../docs/EKBLOOM-PLAN.md) §4.

Receipts (each with its `.log` beside it):

| Gate | Receipt | WeftFS revision | Stamp |
|---|---|---|---|
| `EKB-E1` boundary | `bench/receipts/ekbloom-20261113T081205Z.json` | `bc75d63b68e8` | `20261113T081205Z` |
| `EKB-E2` space | `bench/receipts/ekbloom-space-20261113T081552Z.json` | `f64287c95475` | `20261113T081552Z` |
| `EKB-E3` read | `bench/receipts/ekbloom-read-20261113T083231Z.json` | `ef2aa0a3a21a` | `20261113T083231Z` |

Every number below is emitted from those receipts by `scripts/perf_numbers.py`
between `BEGIN GENERATED` sentinels; the regions are not hand-edited.

---

## 1. The verdict

<!-- BEGIN GENERATED:ekbloom-verdict (scripts/perf_numbers.py) -->
**Verdict: retired.** Three gates ran. `EKB-E1` (boundary, exactness and resource bound) **passed** on **580/580** objects with **0** refused and no limit breach. `EKB-E2` (space) was **lost**: EKBLOOM's aggregate container is **1.168938×** the shipped planner's own footprint, with **98** per-object losses and **0** wins. `EKB-E3` (bounded random access) was **lost** on both declared conditions: its retained footprint is **1.14626×** the shipped tiled representation, and its pooled p95 is **84.52×** the best baseline's. Under `docs/EKBLOOM-PLAN.md` §8.2, two lost portfolio axes retire the line: no payload mode, no planner slot, no `ekbloom` dependency.

That is the whole result. The bounded-step mechanism itself worked — see §5 — but a working mechanism is not a portfolio win, and this document keeps the two apart everywhere. A retire verdict is a **successful** `EKB-X22` under `docs/EKBLOOM-PLAN.md` §4.8; leaving the verdict open would have been the failure mode.
<!-- END GENERATED:ekbloom-verdict -->

**Summary: E1 passed; E2 lost; E3 lost; E4/E5 not run.** Every loss is listed in
§5; the gates that were not run are named in §7.

## 2. What EKBLOOM is

EKBLOOM is a *concealed-expansion* codec. One logical object is presented
through a small visible **core**, while a larger **manifested operational body**
is stored beside it; the body is what turns replay into memory movement. Its
container is `.ekb`: an 80-byte genesis header (`EKBLOOM\0`, format major/minor,
a `core_version` at offset 68), the core bytes, then checksummed commit blocks
ending in `EKBCOMM\0`. Its sections (`HISTORY_FULL`, `HISTORY_SEGMENTED`,
`CHECKPOINTS`, `BLOOM_GLOBAL`, `BLOOM_CONTEXT`, `BLOOM_REGION`, `BLOOM_PATH`,
`CERTIFICATE`, `MANIFEST`, `AUTH`) are strictly additive across generations.

It defines nine machines. Machine 1 `OrANS-NL2` is an exactly invertible
byte-state machine (cursor equals position): a fixed GF(2)⁸ permutation makes
each step depend on the last two symbols, so a precomputed path table turns
positional reads into table lookups. Machines 2/4/7/8 are order-0 rANS variants;
machines 2 and 4 are the cursor-divergent pair whose bounded-window reads
EKBLOOM measures. The crate also ships a budget chooser (`choose_plan`), a
bounded-access engine with an explicit `worst_case_steps_for`, an optional
read-only FUSE projection of an in-process namespace (`OrFS`), an optional
keyed-MAC `AUTH` mode, and an optional `io_uring` publication backend.

**Pinned upstream.** Repository `github.com/infinityabundance/EKBLOOM`, revision
`8275a41b72c17abdc55bf74b82f319876e587ba0` — *"Release 0.7.3"*, committed
2026-10-14T23:47:31Z. Crate `ekbloom 0.7.3`, crates.io checksum
`1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4`,
declared `rust-version` `1.89`, `MIT OR Apache-2.0`. The harness pins the exact
version (`ekbloom = "=0.7.3"`) and the receipts record the checksum that
`bench/ekbloom-harness/Cargo.lock` resolved, so the crate that ran is the crate
named here. Substitution — a vendored tree, a patched source, a different
revision — is forbidden by the contract and was not used.

EKBLOOM's own `LIMITATIONS.md` already says it loses to pinned-hot codecs and to
raw/LZ4 on real data and that its real claim is footprint at non-evicting hot
access, not latency. The trial did not re-litigate that text; it asked the
narrower question the programme cares about: does the pinned crate beat the
representation **WeftFS already chooses** for the same real weights, on a
declared axis, in this court?

## 3. Method

### 3.1 Corpus and pinned digests

The corpus is the Qwen2.5-0.5B pair already pinned in
`bench/corpora/real-models/qwen2.5-0.5b-pair.json`. Both containers are part of
the declared corpus, and the harness re-computes size and SHA-256 in the court
before encoding anything:

| Arm | File (under `.docker-cache/real-corpus/`) | Bytes | SHA-256 |
|---|---|---:|---|
| base | `Qwen2.5-0.5B.safetensors` | 988,097,824 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| instruct | `Qwen2.5-0.5B-Instruct.safetensors` | 988,097,824 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |

Both are BF16 with 290 tensors each, identical names and shapes. A missing or
mismatched fixture is an environment **skip** (`ekbloom_court: skip <reason>`,
exit 78), never a substitution; neither receipt records a skip.

### 3.2 Tensor selection

<!-- BEGIN GENERATED:ekbloom-selection (scripts/perf_numbers.py) -->
| Gate | Objects | Logical bytes | Selection rule |
|---|---:|---:|---|
| `EKB-E1` | 580 | 1,976,131,072 | every tensor of both containers, ascending name order (contract §3.1) |
| `EKB-E2` | 98 | 852,852,736 | every k-th tensor per `(dtype, shape)` class, `k = ceil(class_size / 8)` (contract §3.2) |
| `EKB-E3` | 14 | 583,077,888 | the lexicographically smallest tensor per shape class, both containers independently (contract §9.2) |
<!-- END GENERATED:ekbloom-selection -->

E2's rule covers all seven shape classes present, including the 1-D norm
vectors; E3's smaller set is one representative per class per container. The
rules are computed from the container header alone, never from a result. The
harness processes one tensor at a time under contract §3's bounds (per-tensor
logical cap, a body cap of three times the logical length, and a process-RSS
abort that turns a would-be OOM into a typed `memory_bound` result).

### 3.3 The E3 arms

One backend — the local POSIX data plane — one fresh root per arm, identical
logical byte ranges in all three:

* **W** — WeftFS mode 15 (`TENSOR_TILED`) through the shipped store. Tiles are
  `[128]` for rank-1 and `[128, 128]` for rank-2 tensors; every selected shape
  divides the grid. The read handle is a read-only `Store` with the default
  options: no decoded-object cache and no single-flight, held constant exactly
  as the context-advantage court holds them.
* **A** — the strong application-level baseline rule 14 requires: the raw
  element bytes of every selected tensor in one `payload.raw` file at 8-byte
  alignment, beside a canonical JSON manifest and a canonical JSON index. A read
  is one `pread` of exactly the requested range into a reused buffer. It is
  O(1) positioning at 1.0× raw, not a crippled control.
* **E** — EKBLOOM's file-backed bounded reader: one `.ekb` per selected tensor
  from `create_auto(bytes, ManifestationProfile::Sparse { interval: 1024 })`,
  opened once with `FileBody::open`, read with `FileBody::read`. The in-memory
  `Engine` is probed outside every timed region as supporting evidence for the
  claimed step bound; it is **not** the gating path.

### 3.4 Interleaving, repetitions and samples

Seven repetitions. Repetition 0 is the **cold-first** pass (first timed pass,
no warm-up); repetitions 1–6 are **warm** and preceded by one untimed full pass.
The three arms run in an order rotated by one position per repetition
(`[W,A,E]`, `[A,E,W]`, `[E,W,A]`, …). Within an arm: tensors ascending by name,
region classes ascending by length (1, 4,096, 65,536 bytes), offsets ascending
by `floor(i * (L - len) / 7)` for `i = 0..7`. Percentiles are nearest-rank.
"Cold" here is **process-cold**, not media-cold: the court cannot drop the host
page cache and the data was just written.

<!-- BEGIN GENERATED:ekbloom-samples (scripts/perf_numbers.py) -->
| Measure | Value |
|---|---:|
| timed reads per arm | 1,904 |
| reads in the 1 B / 4,096 B / 65,536 B class | 784 / 560 / 560 |
| exact reads, W / A / E | 1,904 / 1,904 / 1,904 |
| exact reads, all arms together | 5,712 |
| backend bytes per read, W / A / E | 544,033.8 / 20,480.4 / 37,934.3 |
| backend requests per read, W / A / E | 18.84 / 1.00 / 2.00 |
| in-memory `Engine` probe requests, every one exact | 272 |
<!-- END GENERATED:ekbloom-samples -->

The contract's stated expected counts assumed every selected tensor was eligible
for every region class; the contract's own `L >= len` rule excludes the rank-1
tensors from the two larger classes, so the actual counts differ and are the
ones above. The gate uses the pooled p95 and is unaffected; the pooled p99 is
reported ineligible and every per-class p99 is eligible.

### 3.5 The declared gates, quoted verbatim

E2's verdict rule (contract §4.2):

> **E2 win** iff the aggregate over the declared §3.2 set satisfies
> `sum(body_bytes) × 64 ≤ sum(planned_bytes) × 63` **and** every object in both
> arms restored byte-exactly.
> **E2 loss** otherwise. A tie is a loss.

E3's declared gate (contract §9.5):

> * **G2 footprint.** `E_bytes < W_bytes` **and** `E_bytes < A_bytes`, strictly.
>   This is the "declared retained footprint" of EKBLOOM's own claim.
> * **G3 non-inferiority on p95.** With `p95_best = min(p95_W, p95_A)` over the
>   pooled samples, the integer comparison
>
>   ```
>   2 * p95_E_ns <= 5 * p95_best_ns
>   ```
>
>   must hold, i.e. E's p95 is at most **2.5x** the better baseline's p95.

The **2.5×** factor is borrowed, not invented: it is
`bench/CONTRACTS-context-advantage.md` §3 (C — fast-path neutrality), **C2**
("W fast-path p95 <= 2.5 x N fast-path p95"). It was declared before
measurement and flagged for operator veto in `docs/EKBLOOM-PLAN.md` §9 item 1.
No gate was widened, retuned or re-scoped after a receipt existed, and no lost
gate was re-opened.

## 4. Results

### 4.1 `EKB-E1` — boundary, exactness, resource bound: **pass**

<!-- BEGIN GENERATED:ekbloom-e1 (scripts/perf_numbers.py) -->
| Check | Result | Evidence |
|---|---|---|
| `C1_build` | pass | the pinned crate built from the court's lock; no substitution |
| `C2_round_trip` | pass | 580/580 objects restored byte-exactly; 0 refused for `memory_bound` |
| `C3_upstream_verifier` | pass | `ekbloom::verify_deep` accepted every container the crate wrote |
| `C4_resource` | pass | no cgroup OOM kill, no `pids.events max` event, no limit raised; process peak 776,292 KiB |
<!-- END GENERATED:ekbloom-e1 -->

E1 has **no performance axis** (contract §4.1). Its encode/decode milliseconds
are observational per-object fields and are not quoted, compared or used to
decide anything here. What E1 establishes is that real WeftFS bytes survive the
pinned codec exactly inside the unchanged court, so the later gates measure a
feasible thing.

### 4.2 `EKB-E2` — space: **loss**

<!-- BEGIN GENERATED:ekbloom-e2 (scripts/perf_numbers.py) -->
| Measure | EKBLOOM | Shipped planner | Ratio |
|---|---:|---:|---:|
| aggregate retained bytes | 662,410,464 | 566,677,130 | 1.168938× |
| raw element bytes (reference floor) | — | 852,852,736 | — |
| per-object wins / losses | 0 / 98 | — | — |
| largest single per-object loss | 30,522,289 | — | — |

EKBLOOM's aggregate body is **16.89 %** above the shipped planner's footprint. Both arms restored every object byte-exactly (98/98), so this is a size loss, not an exactness failure.
<!-- END GENERATED:ekbloom-e2 -->

The comparison is deliberately unflattering in EKBLOOM's favour in one respect
and against it in another, and both are stated in the contract before the run:
`body_bytes` charges EKBLOOM **every** byte it writes — header, core, manifested
structures, checksums, manifest and auth sections, padding — while
`planned_bytes` is the shipped planner's own frame accounting. That asymmetry
runs **against** EKBLOOM; the receipt also records the observed on-disk
footprint, which differs from the planner number only by store metadata. The
secondary profile ladder on the probed tensor offered no rescue: the default
`root` row was the smallest, and every other rung was larger. A verdict may not
be taken from a non-default row, and none was.

### 4.3 `EKB-E3` — bounded random access: **loss**

**Footprint (G2).**

<!-- BEGIN GENERATED:ekbloom-e3-footprint (scripts/perf_numbers.py) -->
| Arm | Retained footprint |
|---|---:|
| W — WeftFS mode-15 `TENSOR_TILED` region reads | 400,293,613 B |
| A — raw payload + manifest + index | 583,083,071 B |
| E — EKBLOOM `.ekb` containers | 458,840,944 B |

G2 requires E strictly below **both** W and A. E is **1.14626×** W (larger) and **0.78692×** A (smaller). The condition fails because the shipped tiled representation, not the raw baseline, is the smaller footprint.
<!-- END GENERATED:ekbloom-e3-footprint -->

**p95 non-inferiority (G3).**

<!-- BEGIN GENERATED:ekbloom-e3-latency (scripts/perf_numbers.py) -->
| Arm | Pooled p95 |
|---|---:|
| W | 6,495,913 ns |
| A | 2,634 ns |
| E | 222,636 ns |

With `p95_best = min(p95_W, p95_A)` = 2,634 ns (A), E's p95 is **84.52×** the best baseline — far outside the borrowed tolerance — while still being **29.18×** faster than W on the same statistic. The second fact is real and does not satisfy the gate: W is not the p95 baseline.
<!-- END GENERATED:ekbloom-e3-latency -->

## 5. Every loss, stated plainly

<!-- BEGIN GENERATED:ekbloom-losses (scripts/perf_numbers.py) -->
**`EKB-E2` is lost on space.** EKBLOOM's aggregate container is **1.168938×** the shipped planner's footprint and loses **every** object: **98** losses, **0** wins, and **30,522,289 B** at the single worst object (the Instruct embedding). A tie would also have been a loss for portfolio purposes; this is not even a tie.

**`EKB-E3` is lost on footprint.** EKBLOOM's retained footprint was **1.14626× W** — larger than WeftFS mode-15 tiling — and **0.78692× A**, smaller only than the raw baseline. EKBLOOM's own claim is a footprint claim at non-evicting hot access, so losing footprint to the representation WeftFS already ships removes the premise of the claim on this corpus.

**`EKB-E3` is lost on p95.** EKBLOOM's pooled p95 was **84.52× A**, the raw-plus-manifest-plus-index baseline, against the declared non-inferiority tolerance, while being **29.18× faster than W** on the same statistic. That is why the baseline matters: the application-level baseline, not the tiled store, is the thing a bounded small read has to beat or match, and it does not.

**E's read is not free.** E fetched **37,934.3 B** per read over **2.00** requests against A's **20,480.4 B** over **1.00**; its container open validates by streaming the body, and that I/O is reported separately and excluded from read latency by contract §9.4. Neither fact re-frames the gate; both explain why an encoded bounded read is slower than a direct `pread` of the requested range.

**The mechanism itself was real.** The in-memory `Engine` probe decoded **272** requests exactly — every one of them — with **no positioning-bound violation**: the receipt's `C_engine_bound` check passes, and for each request `read_counted_steps` stayed within `worst_case_steps_for`. EKBLOOM's bounded step count is a genuine property of the codec. **The mechanism working is not the same as the portfolio winning**, and this document does not convert one into the other: the bounded read was exact and bounded, and it was still larger than the tiled representation and far slower than the raw baseline.
<!-- END GENERATED:ekbloom-losses -->

## 6. The pre-registered prediction

`docs/EKBLOOM-PLAN.md` §4.0, written before any measurement, predicted:

> **Predicted outcome on real weights: EKBLOOM loses the space, hot-read,
> whole-object-decode and sequential axes; it may win one thing — bounded-step
> exact small reads at a declared retained footprint.**

**The §4.0 prediction was confirmed on the axes this trial measured.** EKBLOOM
lost space (E2) and did not produce a portfolio win on the one axis it was
allowed to win (E3: it lost both footprint and p95). The trial ended as
predicted rather than being rescued, and the retirement is the consequence §8.2
already specified.

The decode and sequential predictions in the same sentence were **never
tested**, because the retirement condition was met first; they are neither
confirmed nor refuted by this trial and are not claimed either way.

## 7. Not run, not claimed

`EKB-E4` (whole-object decode / restore) and `EKB-E5` (write pause / encode)
were **not executed**. The retirement rule fired after E3 — `docs/EKBLOOM-PLAN.md`
§8.2, both portfolio axes lost — and §§4.5–4.6 of the plan run those gates only
if E2 or E3 produced a win. They were **skipped by the retirement rule and
remain unmeasured**. There is no receipt for either, no partial run, and no
number here implies a pass or a failure for them.

`EKB-E6` (portfolio integration: payload mode, conformance vectors, an
independent Go machine and `.ekb` reader, a planner slot, a measured cost row)
was **not attempted**, which §4.7 records as the legitimate outcome when no
declared axis wins. The Go implementation and vector work that E6 would have
required were not started.

No payload mode, no planner slot, no `CostClass` row, no conformance vector, and
no `ekbloom` dependency in the shipped graph exist as a result of this line.

## 8. Non-claims

* EKBLOOM is **not** a core prerequisite for WeftCodec, WeftFS, the context
  plane, or any other programme item, and the trial did not create one.
* Nothing here compares against ext4, XFS, ZFS, Btrfs, 3FS, Lustre, CephFS,
  JuiceFS, DAOS, WEKA or VAST, or any other filesystem (rule 13). The only
  comparison is EKBLOOM against the representation WeftFS itself chooses for the
  same bytes, plus the required application-level baseline.
* No fleet, cloud, media, power-loss, GPU or serving claim is made. Every run is
  one host inside the OOM-protected Docker court (3 GiB / 3 GiB swap / 1024 pids
  / 4 cpus, bounded `/tmp`), on public model weights with a warm local page
  cache, one process.
* E3's "cold" is **process-cold**, not media-cold; the court cannot drop the
  host page cache.
* The result is about these real Qwen2.5-0.5B and Qwen2.5-0.5B-Instruct
  tensors, in this court, at the declared footprint and p95 tolerance. It is
  not a general codec claim, not a claim about other data, and not a claim about
  EKBLOOM's `V/N` on any other corpus.
* Nothing here says a capability is impossible elsewhere (rule 14). EKBLOOM's
  bounded-step read genuinely works; WeftFS's existing representations simply
  win the declared axes on this corpus, and the programme publishes that rather
  than a re-framing.

## 9. Reproduction

The harness is `bench/ekbloom-harness/` (an independent crate, excluded from the
workspace) and the runner is `scripts/ekbloom_court.sh`. The exact commands, run
from the repository root:

```sh
scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase boundary"
scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase space"
scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase read"
```

The harness pins `ekbloom = "=0.7.3"` and builds with `--locked`, so a cold
registry cache needs **crates.io access inside the court**. If the crate cannot
be fetched the court fails closed with the exact cargo text (`fetch_blocked`) and
writes no receipt; it never substitutes a vendored or patched copy. The receipts
record the resolved `Cargo.lock` checksum
(`1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4`), the
court image and its id, the kernel, the four subproject revisions, rustc/cargo,
the per-object rows and the cgroup counters.

For E3 the receipt records a determinism digest over
`(key, W whole-object footprint, A length, A source SHA-256, E body_bytes)` in
ascending key order:

```
51d631473b88d40544606e7bf40fc49e821dd5d045fb6acf94de5493dfdadd0f
```

A repeated run must reproduce every retained size and every exactness result,
and therefore the digest. The digest does **not** cover latency, and no
bit-reproducibility of timings is claimed on one host. `WEFT_EKBLOOM_STAMP`
overrides the stamp for a deliberate re-run; each run writes a new receipt
beside the old one, and the receipts are retained. The court removes its scratch
store on exit unless `WEFT_EKBLOOM_KEEP_WORK=1`; every measured number is already
in the receipt. The `restore` and `write` phases refuse with
`not implemented in this subphase` and a non-zero exit, rather than reporting a
success they did not measure.

## 10. Retirement record

**What stays in the tree, and why.** The harness
(`bench/ekbloom-harness/`), the runner (`scripts/ekbloom_court.sh`), the frozen
contract (`bench/CONTRACTS-ekbloom.md`), the plan
(`docs/EKBLOOM-PLAN.md`), the three receipts with their logs, and this document
remain as the **reproduction path** for these receipts. Removing them would make
the published null result unreproducible. The harness stays an independent
crate excluded from the workspace (`Cargo.toml`:
`exclude = [..., "bench/ekbloom-harness"]`), so it cannot become a member,
cannot enter the published package's dependency graph, and cannot raise the
declared MSRV.

**The shipped graph is clean, verified rather than asserted.** At WeftFS
revision `563348e`, inside the rust court:

```
cargo tree -e normal --offline                 # contains no ekbloom
cargo metadata --format-version 1 --offline    # contains no ekbloom
```

`cargo tree -e normal` contains no `ekbloom` and `cargo metadata` contains no
`ekbloom`. A grep of the shipped trees (`src/`, `tests/`, `examples/`,
`adapters/`, `python/`, `conformance/`, `spec/`, `docker/`, `WeftCodec/src`,
`WeftCodec/tests`, `WeftCodec-Go/`, `WeftFS-Go/`) finds no code reference to
EKBLOOM; the only remaining mentions are the measurement path itself (the
harness crate and `scripts/ekbloom_court.sh`), the explanatory comment in the
root `Cargo.toml`, the plan/contract/results documents and their receipts, and a
negative non-claim in `WeftFS-Go/NOTES.md`. The in-tree "not
claimed" lines about EKBLOOM (README F3, NOTES, `verification/README.md`,
`docs/PROGRAM.md` A4, `docs/STATUS.md` both entries) stay exactly as they are:
they say no acceleration is implemented or claimed, and that remains true.

**What will not be built.** No payload mode (the next free wire value was never
allocated for EKBLOOM), no conformance vector, no independent Go `.ekb` reader
or machine implementation, no `TensorStrategy` slot, no measured `CostClass`
row, no `ekbloom` dependency, and none of E4/E5/E6. The receipts are immutable
and the plan remains the record of what was tried and why it failed.
