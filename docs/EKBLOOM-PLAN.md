# EKBLOOM plan — E1–E6 / X22 moved to the front of the executable backlog

**Status: retired (2026-11-13).** `EKB-E1` passed; `EKB-E2` (space) and
`EKB-E3` (bounded random access) were lost; under §8.2 the line retires — no
payload mode, no planner slot, no `ekbloom` dependency. `EKB-E4`/`EKB-E5` were
not run (the retirement condition was met first) and remain unmeasured;
`EKB-E6` and the Go/vector work were not attempted. Results:
[`bench/RESULTS-ekbloom.md`](../bench/RESULTS-ekbloom.md). The plan was written
2026-11-13 at operator request; the gate text below is retained as written and
was not re-scoped after any receipt.

**Read against:** `github.com/infinityabundance/EKBLOOM`, default branch `main`,
commit `8275a41b72c17abdc55bf74b82f319876e587ba0` ("Release 0.7.3:
dictionary-registry Loom model + lib.sh hardening (PATCH)", committed
2026-10-14T23:47:31Z), crate `ekbloom` 0.7.3. The three documents read in full
are `README.md`, `docs/LIMITATIONS.md` and `docs/ACCEPTANCE.md` at that
revision; the crate metadata is from `crates.io`'s API for the published
`0.7.3` (checksum
`1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4`,
`rust-version` 1.89, `MIT OR Apache-2.0`).

**Related:** [`PROGRAM.md`](PROGRAM.md) (forward pointer), [`STATUS.md`](STATUS.md)
(the dated log entry), `bench/CONTRACTS-context-advantage.md` (the precedent for
a contract committed before measurement), `docs/KV-PROV-REPRESENTATION.md` §7
(the precedent for a space-for-speed proposal that was measured and declined).

---

## 1. Position and framing

This document moves the optional EKBLOOM trials to the front of the executable
backlog at the operator's request, dated 2026-11-13. It does not drop, merge
away, or silently postpone any other item: the ordering of `docs/PROGRAM.md`
§A–§E and of F0–F9 is unchanged, and nothing below changes a shipped format,
court, receipt or claim.

EKBLOOM is, in the operator's own words in `AGENTS.md` rule 14, *"an experiment
in the opposite trade — spending space to buy execution speed — and belongs in
the representation portfolio only if it wins a declared axis."* That is the
whole of its standing here. It is **not** a core prerequisite for WeftCodec,
for WeftFS, for the context plane, or for any other item in this programme. It
is not a competitor to, and must never be benchmarked against, ext4, XFS, ZFS,
Btrfs, 3FS, Lustre, CephFS, JuiceFS, DAOS, WEKA or VAST (rule 13). Every court
below compares **one backend with and without the augmentation**, and every
gate declares the single axis it is allowed to win.

Two housekeeping facts are recorded here rather than papered over:

* **The operator's backlog line is not in this repository.** The line quoted in
  the request — *"E1–E6/X22 — optional EKBLOOM/DSFB trials retained explicitly,
  under their original falsifiable gates, without being silently discarded or
  promoted into core prerequisites."* — does not occur anywhere in the tree. A
  full-tree search for `EKBLOOM`, `DSFB`, `X22`, `E1–E6` and `E1-E6` finds only
  (a) the `AGENTS.md` positioning paragraph, (b) **six** negative non-claims about
  time travel (`README.md` F3, `NOTES.md`, `verification/README.md`,
  `docs/PROGRAM.md` A4, `docs/STATUS.md` twice), and (c) unrelated `0x22` bytes
  in test fixtures and store manifests. The line is therefore treated as
  **operator-supplied** and quoted as such; no acceptance text for it exists in
  the tree to quote verbatim.
* **`E1`–`E6` collides with an existing id space.** `docs/PROGRAM.md` §E already
  uses `E1`–`E7` for strategic pillars (identity, causal closure, codec modes,
  agents, training-ready slices, guarantees, adoption). To keep one id from
  meaning two things, the executable subphases below are written **`EKB-E1` …
  `EKB-E6`** and **`EKB-X22`**, each explicitly mapped to the operator's label.

`DSFB` is not located. It does not appear in this repository, and it does not
appear in the three EKBLOOM documents read for this plan. This document covers
EKBLOOM only; if `DSFB` is a separate artefact, it needs its own pointer.

---

## 2. What is in the tree today

### 2.1 Existing EKBLOOM references

All of them are negative non-claims. None describes an implementation, a plan,
a court or a measurement.

| File:line | Text |
|---|---|
| `AGENTS.md:142` | "EKBLOOM is an experiment in the opposite trade — spending space to buy execution speed — and belongs in the representation portfolio only if it wins a declared axis." |
| `README.md:289` (F3 row) | "Time travel is correctness-only: no io_uring/EKBLOOM acceleration is implemented or claimed." |
| `NOTES.md:246` | "io_uring, EKBLOOM, and similar are not implemented and not claimed." |
| `verification/README.md:494` | "Correctness only; no io_uring/EKBLOOM acceleration and no latency claim" |
| `docs/PROGRAM.md:16` (A4) | "…no io_uring/EKBLOOM." |
| `docs/STATUS.md:928` | "**Not claimed:** io_uring, EKBLOOM, or any acceleration." |
| `docs/STATUS.md:1461` | "**F3 time travel is correctness-only.** There is no io_uring, EKBLOOM, or other acceleration; none is implemented or claimed." |

Every one of those statements remains true after this document: nothing is
implemented. Ignored copies under `.docker-cache/s12-before/**` and
`.docker-cache/target-rust/package/**` repeat the same text and are build
artefacts, not sources.

### 2.2 Reusable scaffolding that exists

* **Experimental entropy coders** — `WeftCodec/src/experimental/entropy/`
  (`mod.rs` plus `huffman.rs`, `tunstall.rs`, `rans.rs`, `zstd.rs`, `raw.rs`,
  `planes.rs`). The module is explicitly *"a measurement module, not a
  wire-format module"*; `rans.rs` is a static order-0 rANS, the same family as
  EKBLOOM machines 2/4. `planes.rs` extracts the exact byte planes that the
  shipped modes 9, 11, 12 and 15 carry, using the shipped encoders. This is the
  right place to extend if an entropy comparison is wanted; it is **not** a
  container reader.
* **Planner candidate plumbing** — `TensorStrategy` in `src/tensor_store.rs`
  (raw, zstd, zstd-patch, float-tensor, tensor-sharded, tensor-tiled,
  float-tiled, float-tiled-streams, weight-delta, sparse-delta,
  semantic-delta, kv-cache, activation, engram-shard), `encode_tensor_object` /
  `publish_encoded_tensor`, `prepare_candidates`, `retain_candidate`; the
  two-stage analyse/select/materialise path; `CostClass`/`Coefficients` and
  `EncodeClass` in `src/cost_model.rs`; `src/plan_graph.rs` for whole-plan
  selection. A new representation is added here as a candidate with a
  **measured** cost row, never a declared one.
* **Region-addressable storage** — mode 15 `TENSOR_TILED` (per-tile roots,
  `read_region`/`read_region_with`), mode 17/18 tile variants, mode 20
  `ENGRAM_SHARD` (row groups), mode 19 `KV_CACHE_PROV`, and the store's
  `read_tensor_region` path with prefix and per-tile range reads. This is the
  representation EKBLOOM must beat on the read axis, not ext4.
* **Bounded-work infrastructure** — `src/parallel.rs` (`run_bounded`),
  `src/restore_pipeline.rs`, `src/tensor_pipeline.rs` (byte reservations, FIFO
  admission), `src/read_pool.rs`, `src/singleflight.rs`, `src/object_cache.rs`.
  A new reader that claims a bounded step count can be measured against these.
* **Court and receipt conventions** — `scripts/docker-court.sh` with hard limits
  (3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded `/tmp`, never raised),
  `examples/*_court.rs` + `scripts/*_court.sh`, receipts under
  `bench/receipts/<name>-<stamp>.json` with a `.log` beside it, and
  `scripts/verify-all.sh` registration.

### 2.3 What is absent

* No `.ekb` reader, writer, parser, validator or dependency anywhere
  (`grep` for `.ekb`/`EKBLOOM` across `*.rs` finds nothing).
* No `ekbloom` dependency in `Cargo.toml`/`Cargo.lock`, and no vendored copy.
* No conformance vector, no payload mode, no wire value, no `CostClass` row and
  no planner strategy for it.
* No contract (`bench/CONTRACTS-ekbloom.md` does not exist), no court, no
  receipt, no result document.
* No Go implementation of the `OrANS-NL2` machine or of the `.ekb` container.
  A wire-level adoption would require one, written independently from EKBLOOM's
  `docs/CORE-LAYOUT.md`, because this programme's non-negotiable is that Rust
  and Go remain independent with no FFI or subprocess in a production path.
* **MSRV conflict, measured, not guessed.** `Cargo.toml` in this repository
  declares `rust-version = "1.77"`; the published `ekbloom` 0.7.3 declares
  `rust-version = "1.89"`. The court toolchain is `rustc 1.99.1` (measured in
  `weft/rust-court:1` while preparing this plan), so EKBLOOM *builds* here, but a
  **normal** dependency would raise this programme's declared MSRV. E1–E5 avoid
  that by compiling a scratch harness crate under `.docker-cache/` with a path
  dependency back to the repository; only EKB-E6, which would put the code in
  the shipped graph, forces the decision. That decision belongs to the operator
  (§6).

---

## 3. What EKBLOOM is, per its own repository

### 3.1 Factual summary

At the revision above, EKBLOOM is a single-package Rust crate (`ekbloom`,
library plus one `ekbloom` binary) implementing a *concealed-expansion* codec:
one logical object is presented through a small visible *core* while a larger
*manifested operational body* is stored beside it, and that body is what turns
replay into memory movement. Its sections (`HISTORY_FULL`,
`HISTORY_SEGMENTED`, `CHECKPOINTS`, `BLOOM_GLOBAL`, `BLOOM_CONTEXT`,
`BLOOM_REGION`, `BLOOM_PATH`, `CERTIFICATE`, `MANIFEST`, `AUTH`) are strictly
additive across generations. Its container is `.ekb`: an 80-byte genesis header
(`EKBLOOM\0`, `format_major.minor = 0.3`, a `core_version` at offset 68), the
core bytes, then checksummed commit blocks each ending in `EKBCOMM\0`.

It defines nine machines. Machine 1 `OrANS-NL2` is an exactly invertible
byte-state machine (`V = N`, cursor equals position): a fixed GF(2)⁸
permutation makes each step depend on the last two symbols, so a precomputed
path table turns positional reads into table lookups. Machines 2/4/7/8 are
order-0 rANS variants (compressing); machines 2 and 4 are the cursor-divergent
pair whose bounded-window reads EKBLOOM measures. Machines 3/5/6/9 are
identity, fixed-width packed, segmented, and shared-dictionary respectively.
It ships a budget chooser (`choose_plan`), a bounded-access engine with an
explicit `worst_case_steps_for`, an optional read-only FUSE projection of an
in-process namespace (`OrFS`), an optional keyed-MAC `AUTH` mode, and an
optional `io_uring` publication backend.

### 3.2 Quotations (external; excluded from every WeftFS claim)

Everything in this subsection is **EKBLOOM's own assertion**, quoted verbatim
with its URL, at revision `8275a41`. None of it is a WeftFS measurement. No
WeftFS claim depends on any number inside a quotation; if the coordinator
prefers a stricter reading of rule 12, these can be replaced by links without
changing the plan.

| # | Quoted text | Where |
|---|---|---|
| Q1 | "EKBLOOM is a **concealed-expansion codec** … It presents a logical object through a small visible surface while maintaining a deliberately larger _manifested operational body_ behind that surface." | `README.md` |
| Q2 | "**Spend manifested space to maximise speed.**" | `README.md` |
| Q3 | "**No read-speed win over `raw-vec`.**" | `README.md` |
| Q4 | "EKBLOOM is **not** a faster array and **not** a smaller one; it is a richer one." | `README.md` |
| Q5 | "**No hot-read win vs raw/LRU or pinned-hot.** … The engine's real win is **footprint at non-evicting hot access, not latency**." | `LIMITATIONS.md` §11 |
| Q6 | "**Does not beat zstd on `V/N` for many corpora.**" Order-0 modelling only compresses *skewed* data; on real text/binary the pinned-hot codecs and LZ4 "are smaller than EKBLOOM … at an equal budget". | `LIMITATIONS.md` §1 |
| Q7 | "**Does not win cold 4 KiB random access vs LZ4**" — the best decoder stores less but reads slower, and "closing it needs a non-rANS symbol engine, not another variant." | `LIMITATIONS.md` §5 |
| Q8 | "**On real data EKBLOOM still loses to the pinned-hot codecs and to raw/LZ4 cold.**" | `LIMITATIONS.md` |
| Q9 | "**io_uring is genuine but slower here.**" | `README.md` |
| Q10 | "**The decoder court is complete — and every variant was rejected**"; the best decoder "remains machine 4". | `README.md`; `docs/DECODERS.md` |
| Q11 | "**Selective hot tiles match whole-object hot latency for ≈68–72× fewer added bytes.**" | `README.md`; `docs/TILES.md` |
| Q12 | "**Bounded encoded windows**" — a small read of a large machine-2 object fetches a bounded window instead of the whole core, "at the **same two `pread` syscalls**". | `README.md`; `docs/WINDOW.md` |
| Q13 | "A `.ekb` file is readable by this build **iff all three hold**" — `format_major == 0` and `format_minor ≤ 3`, a known `machine_id` with `core_version ∈ {0, 1}`, and `manifest_version == 1`; otherwise a typed error and "never partially decodes". | `README.md` |
| Q14 | "The minimum supported Rust version is **1.89**." | `README.md` |

URLs: `https://github.com/infinityabundance/EKBLOOM/blob/8275a41b72c17abdc55bf74b82f319876e587ba0/README.md`,
`…/docs/LIMITATIONS.md`, `…/docs/ACCEPTANCE.md`.

### 3.3 What EKBLOOM asserts about its own failures

The repository is unusually explicit, and the plan takes it at its word rather
than re-litigating it: the speed win it reports is against **its own
authoritative replay**, not against a conventional direct-access baseline (Q3,
Q5). Against `raw-vec` it ties; against an ordinary LRU it loses hot reads;
against pinned-hot LZ4/Zstd on real data it is larger and slower hot, and it is
slower cold and decode-bound sequentially (Q6–Q8). Its own mitigation is not a
speed claim at all but a *footprint* claim — smaller retained bytes at
non-evicting hot access (Q5, Q11) — plus bounded-step exact access (Q12).

**WeftFS has measured nothing about EKBLOOM.** There is no WeftFS receipt naming
it, no execution of it in any court, and no comparison of any kind. This is the
first and only honest entry in the "WeftFS has measured" column, and every gate
below exists to fill that column or retire the experiment.

---

## 4. The falsifiable gates

### 4.0 The prior, stated before any measurement

This is pre-registered so that a null result cannot later be presented as a
surprise, and a win cannot be claimed on an axis nobody declared.

**Predicted outcome on real weights: EKBLOOM loses the space, hot-read,
whole-object-decode and sequential axes; it may win one thing — bounded-step
exact small reads at a declared retained footprint.**

The reasoning, from evidence already in the tree and from EKBLOOM's own text:

1. **Space.** EKBLOOM's compressing machines are order-0 models. WeftFS's
   planner already exploits structure order-0 cannot see: BF16 exponent planes
   in mode 9, causal deltas against the previous checkpoint in modes 11/16, and
   zstd level 19 where it wins. The R5 entropy-coder court
   (`bench/receipts/entropy-coder-20261106T065154Z.json`) measured the same
   order-0 rANS family against the shipped Huffman and zstd-19 coders and
   recommended promoting nothing. EKBLOOM's own `LIMITATIONS.md` §1 says the
   same about its own model. Expected: loss.
2. **Decode.** The same R5 court found the order-0 rANS decoder materially
   slower than the shipped Huffman decoder, and far slower than zstd-19.
   EKBLOOM's best decoder is an rANS machine (Q10), and its own decoder court
   rejected every alternative it tried. Expected: loss on decode CPU and on
   whole-object restore wall.
3. **Hot reads.** EKBLOOM ties `raw-vec` and loses to a cache (Q3, Q5). WeftFS
   can already serve a hot region from tiles; a tie is not a portfolio reason.
   Expected: no win.
4. **Small random reads at a declared footprint.** This is the only place
   EKBLOOM's mechanism is genuinely different from anything in WeftFS. WeftFS's
   compressed representations are decoded at whole-object, shard or tile
   granularity; it has no *certified worst-case-step* byte reader inside a
   compressed body. EKBLOOM does (Q12, plus its bounded-access engine). Whether
   that is worth anything is an empirical question, and E3 answers it against a
   baseline that already gets it right cheaply (raw chunks plus an index).
5. **Write pause.** A byte-state machine is cheap to encode, but the manifested
   structures are additive bytes and extra passes, and EKBLOOM's own adaptive
   shell buys latency by appending a large structure. Expected: parity at best
   against the shipped `--critical-path` fast path, loss against planned writes.

If the prior holds, the honest deliverable is a published null result, not a
portfolio slot. That is what §8 encodes.

### 4.1 Gate format

Every gate below states: its id and operator mapping; one-line goal; the exact
acceptance test; the court and image; the receipt pattern; what is a **win**;
what is a **null result that must be published as a loss**; its dependencies;
and **the single axis it is allowed to win**. No gate may be widened, retuned or
re-scoped after its receipt exists; a changed rule is a new contract version and
a new receipt, with the old one retained.

Thresholds: where the operator's text gives none (it gives none — see §1), the
numeric target is **declared in `bench/CONTRACTS-ekbloom.md` before the first
measurement**, following `bench/CONTRACTS-context-advantage.md`. The recommended
defaults in §4.9 avoid inventing numbers by reusing gates the tree already
enforces.

### 4.2 `EKB-E1` (operator label `E1`) — boundary, exactness and build feasibility

* **Goal.** A pinned EKBLOOM body can be produced from, and read back to, real
  WeftFS bytes inside the court, byte-exactly, inside the hard limits.
* **Acceptance test.** A scratch harness crate under
  `.docker-cache/ekbloom-court/<stamp>/` pins `ekbloom = "=0.7.3"` (and the
  published checksum in the receipt) and path-depends on this repository. For a
  declared tensor set from `bench/corpora/real-models/qwen2.5-0.5b-pair.json`
  (whose container SHA-256 the existing courts already verify), the harness
  runs `create` → `write_body` → `open` → `read_file` → `verify(deep)`, and
  asserts SHA-256 equality of every restored tensor with the source. It records
  the manifest profile/plan chosen, the exact body size, and the cgroup peak
  RSS. It must not exceed the court's unchanged 3 GiB.
* **Court.** `scripts/docker-court.sh rust …`; image `weft/rust-court:1`.
* **Receipt.** `bench/receipts/ekbloom-<stamp>.json` plus
  `bench/receipts/ekbloom-<stamp>.log`.
* **Win (proceed).** Build succeeds from the pinned crate; every tensor is
  byte-exact; the deep verifier passes; cgroup peak is inside the limit.
* **Null (retire or record a blocker).** A build failure that cannot be fixed by
  pinning/vendoring, any byte mismatch, a typed refusal on a file EKBLOOM wrote,
  or a limit breach. Published as a failure; **not** worked around by raising a
  limit.
* **Axis.** None. This is a feasibility gate, not a performance claim.
* **Depends on.** The contract (§5).

### 4.3 `EKB-E2` (operator label `E2`) — space axis

* **Goal.** Is an EKBLOOM manifested body smaller than the representation
  WeftFS already chooses for the same bytes?
* **Acceptance test.** Same backend, same barrier, one durability protocol,
  interleaved repetitions. For each object in the declared set, record:
  (a) the EKBLOOM body size at the plan `choose_plan` selects under the declared
  byte budget; (b) the WeftFS planned representation size for the same object
  (`TensorStrategy` chosen by the shipped planner, with its candidate set
  spelled out in the contract); (c) raw. Every arm restores every tensor and
  every restored tensor is compared by SHA-256.
* **Court.** `scripts/docker-court.sh rust …`; rust court.
* **Receipt.** `bench/receipts/ekbloom-space-<stamp>.json` (+ `.log`).
* **Win.** EKBLOOM is strictly smaller than the WeftFS representation on the
  declared corpus, at the declared margin, with exact restore.
* **Null (a published loss).** EKBLOOM is not smaller, or is smaller only when
  the comparison ignores EKBLOOM's own metadata. E2 is then recorded as lost and
  the experiment continues only if E3 wins; a size tie is not a win (Q4).
* **Axis.** **Retained bytes per object**, nothing else.
* **Depends on.** EKB-E1.
* **Prediction.** Loss (§4.0.1).

### 4.4 `EKB-E3` (operator label `E3`) — bounded random-access axis

* **Goal.** The one axis EKBLOOM can plausibly win: exact small random reads
  over a compressed body with a certified worst-case step count, at a declared
  retained footprint.
* **Acceptance test.** One backend, three arms, same objects, same logical byte
  ranges, interleaved repetitions:
  * **W** — WeftFS mode 15 (`TENSOR_TILED`) region read through the shipped
    `read_tensor_region`, i.e. the representation WeftFS actually ships for
    region addressing.
  * **A** — raw chunks plus an index: the application-level strong baseline rule
    14 requires. It has trivial O(1) access at 1.0× raw, so any EKBLOOM win here
    can only be *space at equal-or-better latency*, never raw latency.
  * **E** — the EKBLOOM engine (`Engine`/`BlockIndex`, bounded encoded window)
    reading the same ranges from the `.ekb` body.
  Workloads: one byte, a small fixed read, and a multi-row region, cold and
  warm. Record p50/p95/p99, backend bytes and request count per read, and — for
  E — the claimed `worst_case_steps_for` against actual positioning.
* **Court.** `scripts/docker-court.sh rust …`; rust court.
* **Receipt.** `bench/receipts/ekbloom-read-<stamp>.json` (+ `.log`).
* **Win.** At a retained footprint **strictly below the better of W and A**,
  E's p95 is not worse than the declared non-inferiority tolerance; or E's p95
  is strictly better at an equal-or-smaller footprint. Any claim must name which
  of those two shapes it is.
* **Null (a published loss).** E loses p95 to A or to W, or wins only on a
  synthetic skewed object while losing on real weights. EKBLOOM's footprint
  claim then does not transfer to this corpus, and that is the finding.
* **Axis.** **p95 small-read latency at a declared retained footprint.**
* **Depends on.** EKB-E1; EKB-E2 is not a prerequisite (a space loss with a read
  win is still a portfolio argument).
* **Prediction.** A wins latency everywhere; W wins footprint at tile
  granularity; E may win footprint on a synthetic skewed object only.

### 4.5 `EKB-E4` (operator label `E4`) — whole-object decode / restore axis

* **Goal.** Is EKBLOOM's whole-object decode cheaper than WeftFS's chosen
  representation for restoring the same bytes?
* **Acceptance test.** Restore the full declared object set on both arms; record
  CPU (user+sys, from the same source the existing courts use), wall, and
  per-object p50/p95; interleaved repetitions; byte-exact comparison.
* **Court.** rust court.
* **Receipt.** `bench/receipts/ekbloom-restore-<stamp>.json` (+ `.log`).
* **Win.** Strictly lower CPU at equal-or-better wall, or strictly lower wall at
  equal-or-lower CPU, beyond the declared margin.
* **Null (a published loss).** A tie is a loss for portfolio purposes: WeftFS
  already has the path, so a tie buys nothing.
* **Axis.** **Decode CPU and restore wall.**
* **Depends on.** EKB-E1; run only if E2 or E3 has a win (§8).
* **Prediction.** Loss (§4.0.2). Supporting stamps for the existing side of the
  comparison: `entropy-coder-20261106T065154Z` (order-0 rANS decode),
  `planner-float-20261106T035316Z` (decode-cost ledger),
  `restore-scaling-20261111T120026Z` (parallel restore).

### 4.6 `EKB-E5` (operator label `E5`) — write-pause / encode axis

* **Goal.** Does EKBLOOM's encode cost less than the write WeftFS ships,
  including its declared critical-path mode?
* **Acceptance test.** Per-object encode CPU and the critical-path pause as the
  store defines it, for three arms: WeftFS planned, WeftFS fast-raw, and EKBLOOM
  `create` at the E2-selected plan. Same backend, one durability barrier,
  interleaved repetitions, byte-exact restore afterwards.
* **Court.** rust court.
* **Receipt.** `bench/receipts/ekbloom-write-<stamp>.json` (+ `.log`).
* **Win.** Strictly lower pause and CPU than WeftFS fast-raw beyond the declared
  margin, with E2/E3 not already lost.
* **Null (a published loss).** Equal or higher cost, or a win that requires
  disabling a durability step the other arm performs.
* **Axis.** **Write pause and encode CPU.**
* **Depends on.** EKB-E1; run only if E2 or E3 has a win.
* **Prediction.** Parity for machine 1 alone; loss once any manifested structure
  is added (§4.0.5).

### 4.7 `EKB-E6` (operator label `E6`) — portfolio integration

* **Condition.** Built **only** if E2, E3, E4 or E5 produces a declared-axis
  win. Otherwise it is recorded as *not implemented*, which is a legitimate
  outcome, not a gap.
* **Goal.** Make EKBLOOM an opt-in representation whose logical identity is
  invariant, or do not ship it.
* **Acceptance test.**
  1. A new WeftCodec payload mode with the **next free wire value** — 21 when
     this plan was written; the executor must re-check, because 20 is
     `ENGRAM_SHARD` — named for the body, whose descriptor carries the `.ekb`
     genesis fields (`format_major.minor`, `machine_id`, `core_version`,
     `manifest_version`), the body's domain-separated BLAKE3, the ordered
     additive structure set and the declared manifested size. Decode recomputes
     the WeftFS logical root over the **decoded** bytes and fails closed on any
     mismatch; the WeftFS `ContentId` remains the identity.
  2. Valid **and** reject vectors, byte-identical in both codec repositories;
     the conformance manifest and coverage court regenerated.
  3. An independent Go implementation of the machine and the `.ekb` reader from
     EKBLOOM's `docs/CORE-LAYOUT.md`; no FFI, no subprocess.
  4. `TensorStrategy::Ekbloom` (or equivalent) in both stores, offered only when
     the declared axis wins, priced with a **measured** `CostClass` row produced
     by the calibration harness, never a declared one; never a default.
  5. GC, recovery, `verify --full`, snapshot and time-travel tests unchanged and
     passing on an object stored this way.
* **Court.** rust court (WeftCodec + WeftFS), `go` court (WeftCodec-Go), then
  the cross gate `WeftCodec-Go/hack/cross.sh` and
  `scripts/run-conformance.sh`.
* **Receipt.** `bench/receipts/ekbloom-portfolio-<stamp>.json` plus the
  conformance receipts of the stamp.
* **Win.** All of the above, plus the declared-axis win measured at the
  portfolio level (not just in the isolated court).
* **Null.** Any of the above impossible or failing → not shipped; X22 publishes
  the reason.
* **Axis.** Whatever E2–E5 declared, and only that.
* **Depends on.** A win in E2/E3/E4/E5.
* **Note.** This is the only gate that forces the MSRV decision in §2.3.

### 4.8 `EKB-X22` (operator label `X22`) — retain-or-retire decision

* **Goal.** End the experiment honestly, either way.
* **Acceptance test.** A `bench/RESULTS-ekbloom.md` naming every receipt, every
  axis, every loss and every non-claim; a status line in this document; and
  exactly one of:
  * **Retain** — the E6 opt-in slot exists with conformance vectors, a measured
    cost row, and the declared-axis receipt.
  * **Retire** — no shipped code path references EKBLOOM; the null result is
    published; the in-tree "not claimed" lines stay true and unchanged.
  If the verdict quotes a receipt, it also gets a `docs/claims.json` entry.
  `scripts/check-claims.sh` and `scripts/perf_numbers.py --check` pass.
* **Court.** Host-only guards; `scripts/verify-all.sh` only if a code path
  changed (and then a new aggregate receipt, not a reused one).
* **Receipt.** `bench/receipts/ekbloom-retire-<stamp>.json` (retire) or the E6
  portfolio receipt (retain).
* **Win.** A defensible verdict. A retire verdict is a **successful** X22.
* **Null.** There is none. Leaving X22 open is the failure mode.
* **Depends on.** E1–E5 (E6 if and only if a win).

### 4.9 Recommended threshold defaults (operator decision, with justification)

No new number is invented here. Three of the four defaults are gates the tree
already enforces.

* **E2 (space).** Reuse the shipped planner's own rule: a candidate is offered
  only when it strictly reduces the objective, and the mode-9 candidate
  additionally uses a minimum size-saving fraction defined in
  `src/tensor_store.rs`. Rather than choose a new margin, require EKBLOOM to
  clear that same fraction. Justification by stamp:
  `planner-float-20261106T035316Z` records the decision stream the shipped gate
  produced, and the R18 two-stage work (status `two_stage`) removed a candidate
  that changed no selection — the precedent is that a candidate which does not
  change a decision is not kept.
* **E3 (read).** Require **non-inferiority**, not a speedup, on p95, using the
  fast-path tolerance already declared and enforced by
  `bench/CONTRACTS-context-advantage.md`; and require E's retained footprint to
  be strictly below the better of W and A. This is the only shape in which
  EKBLOOM's own claim can be true (Q5). Justification by stamp:
  `context-advantage-20261113T072921Z` and `...T073016Z` for the tolerance's
  use on two backends, and `overlay-20261106T103115Z` for the precedent that a
  read-path win is stated at a declared footprint.
* **E4/E5 (decode, write).** Require **strict** improvement, not
  non-inferiority: WeftFS already ships both paths, so a tie is not a portfolio
  argument. Justification by stamp: `planner-float-20261106T035316Z` (decode
  ledger) and `throughput-20261110T134917Z` (write-cost attribution), both of
  which only kept changes that moved a decision.
* **E1/X22 (feasibility/verdict).** No threshold; binary.

**The one decision genuinely requiring the operator** is therefore narrow: may
E3 be a *non-inferiority* gate on p95 (recommended, because EKBLOOM claims no
latency win), or must it be a strict latency win (which EKBLOOM's own evidence
predicts it will fail)? See §6.

---

## 5. Contract-first requirement

`bench/CONTRACTS-ekbloom.md` **must be committed before the first measurement**,
matching `bench/CONTRACTS-context-advantage.md`, whose opening line is that it
"is written and committed **before** the court is run". The contract fixes, in
advance:

1. the arms and their exact definitions (W, A, E for E3; planned/fast/EKBLOOM
   for E5);
2. the fixture set (containers by SHA-256, tensors by name and index) and the
   rule that a missing fixture is an environment skip, never a substitution;
3. the metric definitions and the declared tolerance for every gate;
4. the repetitions, interleaving rule and percentile definition;
5. the stop rules and the retire condition of §8;
6. the label every receipt must carry (single host, OOM-protected court,
   synthetic or public fixture, not a fleet/cloud/media claim).

A changed rule is a new contract **version** and a new receipt. Earlier receipts
stay published under the version they were produced with. This is the mechanism
that stops a null result from being re-framed after the fact, which is the
failure mode rule 6 and this programme's history explicitly guard against.

---

## 6. Dependency-ordered execution sequence

One subagent per step; one court run per step; nothing runs on the host; no
limit is raised. Steps 5–7 are conditional on a win, and the sequence is
designed so that the cheapest decisive falsifier comes first.

| # | Step | Runnable in one session? | Exact command |
|---|---|---|---|
| 0 | Write and commit `bench/CONTRACTS-ekbloom.md` (no measurement) | Yes | `git` only |
| 1 | **EKB-E1** boundary/exactness/feasibility | Yes | `scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase boundary"` |
| 2 | **EKB-E2** space | Yes | `scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase space"` |
| 3 | **EKB-E3** bounded small reads (W/A/E) | Yes | `scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase read"` |
| 4 | Stop-rule evaluation: if E2 and E3 are both losses → step 7 | Yes | `git` + `bench/RESULTS-ekbloom.md` |
| 5 | **EKB-E4** decode/restore | Yes | `scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase restore"` |
| 6 | **EKB-E5** write pause | Yes | `scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase write"` |
| 7 | **EKB-E6** portfolio integration (only if a win) | No — the Rust and Go courts plus the four cross-gate courts | `scripts/docker-court.sh rust "cd WeftCodec && cargo build --release --quiet && cd /work/WeftCodec-Go && bash hack/cross.sh rust-gen"`; `scripts/docker-court.sh go "cd WeftCodec-Go && bash hack/cross.sh go-build && bash hack/cross.sh go-gen && bash hack/cross.sh go-decode"`; `scripts/docker-court.sh rust "cd /work/WeftCodec-Go && bash hack/cross.sh rust-decode"`; `scripts/docker-court.sh go "cd WeftCodec-Go && bash hack/cross.sh compare"` (see `scripts/verify-all.sh` `codec_cross_*`) |
| 8 | **EKB-X22** verdict | Yes | host-only guards, then `git` |

The runner script does not exist yet; step 1 creates it along with the harness
crate. Its stamp convention matches the other courts
(`stamp="${WEFT_EKBLOOM_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"`), it writes the
receipt and `.log` under `bench/receipts/`, and it prints exactly one
`ekbloom_court: skip <reason>` line and exits 78 when a precondition is absent
(no docker, missing fixture), following `scripts/augmentation_court.sh`.

**Why E1 first.** It is the cheapest thing that can falsify the whole line: a
pinned crate that does not build under the court's hard limits, or does not
round-trip real WeftFS bytes exactly, ends the experiment before any benchmark
is designed. It costs one court run and it also produces the body sizes that
make E2 meaningful.

**Why E3 before E4/E5.** E4 and E5 test axes the prior already expects to lose
and that WeftFS already ships; E3 tests the only axis on which the mechanism is
genuinely different. Running E4/E5 first would spend two sessions to confirm a
prediction without touching the decision.

---

## 7. Explicit non-claims

* EKBLOOM is **not** a core prerequisite for WeftCodec, WeftFS, the context
  plane, or any other backlog item, and this plan does not create one.
* This document makes **no** WeftFS performance, storage, latency, throughput or
  resource claim. Nothing has been measured.
* No production, fleet, cloud, GPU, media, power-loss or serving claim is made
  or implied by any gate above. Every court is one host inside the OOM-protected
  Docker court.
* No comparison against ext4, XFS, ZFS, Btrfs, 3FS, Lustre, CephFS, JuiceFS,
  DAOS, WEKA or VAST is made, and none may be added to these courts (rule 13).
* EKBLOOM's `V = N` machine is not a compression claim; its compressing machines
  are not claimed to generalise to real weights, and EKBLOOM's own
  `LIMITATIONS.md` says so.
* Nothing here claims that a capability is impossible elsewhere. A materially
  more expressive, reliable, portable or efficient mechanism is already the
  contribution; beating a mature system is not required and is not attempted.
* A "retain" verdict would not promote EKBLOOM into the fast path: the shipped
  default stays the shipped planner, and any EKBLOOM slot is opt-in.

---

## 8. What would retire it

The experiment ends, with a published null result, on the first of:

1. **Feasibility fails (E1).** The pinned crate does not build in the court, or
   a real WeftFS tensor does not round-trip byte-exactly, or the court's
   unchanged limits are exceeded.
2. **Both portfolio axes are lost (E2 and E3).** The body is not smaller than
   the representation WeftFS already chooses, **and** it delivers no
   footprint-at-equal-p95 advantage over either the shipped tiled representation
   or a raw-chunk-plus-index baseline. This is the predicted outcome.
3. **Any win is synthetic-only.** A win that exists only on a skewed synthetic
   object and not on real weights is a loss for this programme, because the
   corpus that matters is real model state.
4. **Wire adoption is impossible (E6 only).** If a declared-axis win exists but
   a byte-identical independent Go decoder cannot be built from EKBLOOM's
   published layout, the representation cannot enter this portfolio; it is
   recorded as a genuinely interesting external codec that WeftFS cannot adopt.
5. **The maintained artefact diverges.** If EKBLOOM's published crate cannot be
   pinned to a revision whose bytes the receipt can reproduce, the trial is
   retired rather than tracked against a moving target.

Retirement means: `bench/RESULTS-ekbloom.md` states the null result and names
every receipt; this document's status becomes **retired**; no shipped code path
refers to EKBLOOM; the in-tree "not claimed" lines stay exactly as they are; and
`docs/EKBLOOM-PLAN.md` remains as the record of what was tried and why it
failed. The plan's stop rules are advisory to the coordinator — the operator may
override a stop, but an override must be written into the contract as a new
version **before** the next measurement, never after a receipt exists.

---

## 9. Open items for the operator

1. **E3 gate shape.** Non-inferiority on p95 at a strictly smaller footprint
   (recommended: it is the only shape EKBLOOM's own evidence supports), or a
   strict latency win (which its own `LIMITATIONS.md` predicts it will fail)?
2. **MSRV, only if E6 is reached.** Adopting EKBLOOM into the shipped graph
   raises this repository's declared `rust-version` from 1.77 to at least 1.89.
   Accept the bump, or restrict EKBLOOM to an optional feature whose build is
   never part of the default graph? (Recommended: optional feature, and only
   with a win in E2–E5.)
3. **`DSFB`.** It is named in the operator's backlog line but located nowhere in
   this repository or in the EKBLOOM documents read. Supply a pointer, or drop
   it from the line.
4. **Corpus scope.** The gates default to the Qwen2.5-0.5B pair already used by
   the neighbouring courts because its containers are SHA-256-verified in the
   tree. If the operator wants a different declared corpus, it must be fixed in
   the contract before E2.

   ---

   ## 10. Status log (append-only)

   * **2026-11-13 — EKB-E1 and EKB-E2 executed.** E1 (boundary) **passed** and E2
     (space) was **lost**, which confirms the pre-registered prediction in §4.0.1.
     Evidence: `bench/receipts/ekbloom-20261113T081205Z.json` (boundary, revision
     `bc75d63`) and `bench/receipts/ekbloom-space-20261113T081552Z.json` (space,
     revision `f64287c`), each with its `.log` beside it. The numbers live in the
     receipts and are deliberately not restated here (rule 12). E3–E6 and X22
     remain open; §8.2 retires the experiment only when **both** E2 and E3 are
     lost, so an E2 loss ends the space axis and nothing else.

   * **2026-11-13 — EKB-E3 executed; the EKBLOOM line retires.** E3 (bounded
     random access) was **lost** on both declared conditions: E's retained
     footprint was not strictly below both baselines, and its pooled p95 was
     outside the declared non-inferiority tolerance of the better baseline. The
     bounded-step property itself held — the in-memory `Engine` probe decoded
     every request exactly with no positioning-bound violation — but the
     surviving axis did not transfer to a win. Evidence:
     `bench/receipts/ekbloom-read-20261113T083231Z.json` (read, revision
     `ef2aa0a`) with its `.log` beside it; the contract is
     `bench/CONTRACTS-ekbloom.md` §9 (version 2). The numbers live in the receipt
     and are deliberately not restated here (rule 12). The receipt also records
     a contract-arithmetic defect in §9.3's stated expected sample counts: they
     assumed every selected tensor was eligible for every region class, while
     the contract's own `L >= len` rule excludes the rank-1 tensors from the two
     larger classes; the pooled p99 is therefore ineligible and every per-class
     p99 is eligible. The gate uses the pooled p95 and is unaffected. Under
     §8.2, **E2 and E3 are both lost, so the experiment retires**: no payload
     mode, no planner slot, no dependency. E4/E5/E6 are not run; `EKB-X22` owns
     `bench/RESULTS-ekbloom.md`, `docs/claims.json` and the retirement record.

   * **2026-11-13 — `EKB-X22` executed; per-gate outcome finalised.**
     `EKB-E1` **pass** (`bench/receipts/ekbloom-20261113T081205Z.json`);
     `EKB-E2` **loss** (`bench/receipts/ekbloom-space-20261113T081552Z.json`);
     `EKB-E3` **loss** (`bench/receipts/ekbloom-read-20261113T083231Z.json`);
     `EKB-E4` **not run**; `EKB-E5` **not run**; `EKB-E6` **not attempted**.
     `EKB-E4` and `EKB-E5` were skipped by the retirement rule in §8.2, not
     evaluated and not failed; they remain unmeasured and no receipt exists for
     them. The verdict, every loss and the confirmed §4.0 prediction are
     published in [`bench/RESULTS-ekbloom.md`](../bench/RESULTS-ekbloom.md); the
     retirement claims are in `docs/claims.json`, and the shipped dependency
     graph was verified to contain no `ekbloom` (`cargo tree -e normal`). No
     shipped code, format, court or receipt count changed.
