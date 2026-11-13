# EKBLOOM evaluation contract — frozen (EKBLOOM v1)

This contract is written and committed **before** the first EKBLOOM measurement,
matching `bench/CONTRACTS-context-advantage.md`. It fixes the pinned upstream, the
corpus, the tensor selection rules, the two gate shapes in scope for this
subphase (`EKB-E1` boundary and `EKB-E2` space), the metric definitions, the
pass/fail rules, the stop rules and the label every receipt carries. Nothing here
is edited after a measurement; a changed rule is a new contract version and a new
receipt, and the earlier receipt stays published under the version it was
produced with.

It contains **no** performance number. No latency, throughput or resource
threshold is invented here; where a margin is needed it is reused from a rule the
tree already enforces (§4.4). Fixture sizes and digests appear only as fixture
identity, not as a result.

Gates not in scope for this subphase — `EKB-E3` (bounded random access),
`EKB-E4` (whole-object decode), `EKB-E5` (write pause), `EKB-E6` (portfolio
integration) and `EKB-X22` (verdict) — are declared in
`docs/EKBLOOM-PLAN.md` §4 and are **not** graded, estimated or reported here.
The court script refuses them with `not implemented in this subphase` and a
non-zero exit rather than reporting a success it did not measure.

**Related:** [`docs/EKBLOOM-PLAN.md`](../docs/EKBLOOM-PLAN.md) (gates and the
pre-registered prediction), `bench/CONTRACTS-context-advantage.md` (contract
precedent), `bench/RESULTS-context-advantage.md` (the precedent that a failed
tolerance is published as a failure, never widened).

---

## 0. Standing of this experiment

* EKBLOOM is **not** a core prerequisite for WeftCodec, WeftFS, the context plane
  or any other item of `docs/PROGRAM.md`. It is an experiment in spending space
  to buy execution speed, and it enters the representation portfolio only if it
  wins a declared axis (`AGENTS.md` rule 14).
* **No mature filesystem is a comparison target** (rule 13). Nothing here
  measures ext4, XFS, ZFS, Btrfs, 3FS, Lustre, CephFS, JuiceFS, DAOS, WEKA, VAST
  or any other system, and no such number may be added to this court. The only
  comparison is EKBLOOM against **the representation WeftFS itself already
  chooses for the same bytes**, on one host, in one OOM-protected court.
* **A null result is published as a loss** (rule 6). A size tie is not a win.
  A win that exists only on a synthetic object is a loss
  (`docs/EKBLOOM-PLAN.md` §8.3).
* `docs/EKBLOOM-PLAN.md` §4.0 pre-registers the prediction that E2 is **lost**.
  This subphase must either confirm that prediction with its own numbers or
  explicitly refute it with evidence. It may not quietly drop it.
* Everything below is one host, a Docker court with hard limits, public
  published model weights and warm page cache. It is **not** a fleet, cloud,
  media, power-loss, GPU or serving claim.

---

## 1. Pinned upstream

| Field | Value |
|---|---|
| Repository | `github.com/infinityabundance/EKBLOOM` |
| Revision read for the plan | `8275a41b72c17abdc55bf74b82f319876e587ba0` ("Release 0.7.3") |
| Crate | `ekbloom` |
| Version pin | `=0.7.3` (exact; no caret range anywhere in the harness) |
| crates.io checksum | `1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4` |
| Declared `rust-version` | `1.89` |
| License | `MIT OR Apache-2.0` |
| Binary names in the package | `ekbloom` |

The checksum above was read from the crates.io version API
(`https://crates.io/api/v1/crates/ekbloom/0.7.3`) for the exact published
version, on 2026-11-13, before the contract was committed; it equals the value
recorded in `docs/EKBLOOM-PLAN.md`. The harness commit fixes the resolved graph
in `bench/ekbloom-harness/Cargo.lock`, and every receipt records the checksum
that `Cargo.lock` resolved, so a reader can tell whether the crate that ran is
the crate named here.

**Substitution is forbidden.** A different revision, a vendored copy, a
patched source tree, a locally built copy of the repository, or a crate fetched
on the host and copied in is a **new contract version** and may not decide these
gates. If the pinned crate cannot be fetched inside the court, the run stops and
the exact error is recorded; a blocked run is a legitimate documented outcome.

**MSRV.** The crate declares `rust-version = 1.89`; this repository declares
`rust-version = "1.77"`. The harness is therefore an independent crate outside
the published package's graph: it must not become a workspace member, must not
enter `weftfs`'s dependency graph, and must not change the published crate's
MSRV. The court's toolchain version is recorded in the receipt rather than
assumed.

---

## 2. Corpus

The corpus is the Qwen2.5-0.5B pair already pinned in the tree at
`bench/corpora/real-models/qwen2.5-0.5b-pair.json`
(schema `weft.real-model.corpus.v1`):

| Arm | File (under `.docker-cache/real-corpus/`) | Bytes | SHA-256 |
|---|---|---|---|
| base | `Qwen2.5-0.5B.safetensors` | 988,097,824 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| instruct | `Qwen2.5-0.5B-Instruct.safetensors` | 988,097,824 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |

Structure (from the committed manifest and independently confirmed from the
container headers): 290 tensors each, BF16, identical names and shapes, element
bytes 988,065,536 each, header 32,280 bytes each, largest tensor
`model.embed_tokens.weight` at 272,269,312 bytes.

Rules:

1. **Both** arms of the pair are part of the declared corpus. Reporting only the
   base would hide the fine-tune case, and the pair is what the neighbouring
   real-model courts use.
2. The harness **verifies size and SHA-256 in the court, before encoding
   anything**, and fails closed on any mismatch. A receipt may only quote
   digests the harness computed.
3. **A missing or mismatched fixture is a skip, never a substitution**
   (`ekbloom_court: skip <reason>`, exit 78). No other corpus may be used to
   decide E1 or E2 in this contract version. If the operator wants a different
   corpus, it becomes contract v2 before the next measurement.
4. Both containers' safetensors headers are byte-identical
   (`sha256 = bd808652be103f0e…` prefix, 32,280 bytes), so tensor names, dtypes,
   shapes and data offsets are the same in both files. The harness still
   re-derives them from each file rather than assuming it.

---

## 3. Tensor selection rules (numeric, declared before measurement)

The harness processes exactly one tensor at a time and releases it before the
next. Three bounds keep it inside the court's unchanged 3 GiB:

* **Per-tensor logical cap: 384 MiB (402,653,184 bytes).** A tensor above the cap
  is refused with the typed reason `memory_bound` and is **not** silently
  dropped from the denominator: it is counted, named and reported.
* **Body cap: 3.0 × the tensor's logical byte length.** If EKBLOOM's chosen plan
  produces a body above the cap, the object is refused with `memory_bound`,
  counted and reported. The cap is generous to EKBLOOM (EKBLOOM is free to
  expand threefold) and exists only so the harness cannot be asked to hold an
  unbounded body.
* **Process RSS abort: 2.5 GiB**, read from `/proc/self/status` `VmRSS` after
  each object. The harness aborts with `memory_bound` before the court's own
  limit can OOM-kill it, so a limit breach is a typed result rather than an
  unobservable kill.

Justification from the memory bound, not from a desired result: the harness holds
at most source + body + restored bytes for one tensor. With the per-tensor cap
and the body cap, the worst case is `384 MiB × (1 + 3 + 1) = 1,875 MiB`, still
inside 3 GiB, and for the actual largest tensor (272,269,312 B) the worst case is
`259.7 MiB × 5 ≈ 1.27 GiB`, about 42 % of the court's memory bound. The cap
therefore only excludes tensors that could not have been measured at all in this
court.

### 3.1 E1 set — every tensor

**All 290 tensors of each container, in ascending name order, for both arms:**
580 objects, 1,976,131,072 element bytes total. No sampling. E1 is the cheapest
gate and the one that can end the experiment, so it is not weakened by a subset.

### 3.2 E2 set — every shape class, sampled

**Rule (deterministic, computed from the container header alone):** group all
tensors by `(dtype, shape)`; within each group, order by ascending tensor name
and take every `k`-th tensor where `k = ceil(group_size / 8)`; a group of size 1
is taken in full. Applied to each container independently.

For this corpus the rule yields **49 tensors per container
(426,426,368 element bytes per container; 98 objects and 852,852,736 element
bytes over both arms)** and covers **all seven** shape classes present:

| Shape class | In container | Selected |
|---|---|---|
| `[151936, 896]` (embedding) | 1 | 1 |
| `[4864, 896]` | 48 | 8 |
| `[896, 4864]` | 24 | 8 |
| `[896, 896]` | 48 | 8 |
| `[128, 896]` | 48 | 8 |
| `[896]` | 73 | 8 |
| `[128]` | 48 | 8 |

Justification: E2 must run the **shipped planner**, which encodes every
candidate it is offered (including zstd level 19 on tensors ≥ 1 MiB); the
per-object cost is therefore one to two orders of magnitude above E1's. Covering
every shape class rather than a fixed stride keeps each distinct tensor geometry
in the comparison, including the 1-D norm vectors, which a name-stride sample
misses. The rule is fixed by the header, not by any result.

---

## 4. Gates in scope

### 4.1 `EKB-E1` (operator label E1) — boundary, exactness, resource bound

**Goal.** A pinned EKBLOOM body round-trips real WeftFS bytes exactly, inside the
court's hard limits.

**Axes: none.** E1 makes no throughput, latency or space claim. Encode and decode
milliseconds are recorded as *observational* per-object fields only; they may not
be quoted, compared, aggregated into a claim, or used to decide anything in this
subphase.

**Checks (each is pass/fail and all are reported):**

| Id | Check | Pass condition |
|---|---|---|
| `C1_build` | The pinned crate builds and the harness links in the rust court | exit 0, no substitution |
| `C2_round_trip` | For every object in the §3.1 set: encode the exact element bytes, decode them back, compare | restored bytes equal source bytes **byte for byte** (`memcmp` over the whole buffer) **and** SHA-256 equal; zero objects refused for a reason other than the declared `memory_bound` |
| `C3_upstream_verifier` | EKBLOOM's own container verifier, at the deepest level the public API exposes, accepts the container EKBLOOM just wrote | success from that function. **If the crate exposes no verifier, this check reports `not_available` and is excluded from the verdict**, and the fact is named in the receipt and in the final message. This exclusion is declared here, before the API is known, and may not be extended to `C2` or `C4`. |
| `C4_resource` | No OOM kill, no limit breach, and no object refused by the process RSS abort | cgroup `oom_kill` delta 0, cgroup `pids.events max` delta 0, total objects refused by `memory_bound` = 0 |

**Typed boundary failures (any one ends the experiment, published as a loss with
no workaround):**

* `api_surface_insufficient` — the crate exposes **no public whole-object
  encode and decode path suitable for exact bytes**. The receipt records the
  exact public API surface found (module/function names and signatures). The
  harness must not emulate the container, shell out to the `ekbloom` binary, or
  approximate a round trip.
* `fetch_blocked` — the pinned crate cannot be fetched in the court. The exact
  cargo error text is recorded.
* `build_blocked` — the pinned crate does not build in the court's toolchain.
* `byte_mismatch` — any object's restored bytes differ.
* `unexpected_refusal` — EKBLOOM writes a container and then refuses to read it
  back, or returns a typed error the harness did not predict.
* `memory_bound` — any object refused by §3's bounds, or any limit breach.

**Do not raise a limit, do not retry with a smaller tensor set, do not fall back
to a different codec.** A limit breach ends the gate.

### 4.2 `EKB-E2` (operator label E2) — space axis

**Goal.** Is an EKBLOOM manifested body smaller than the representation WeftFS
already chooses for the same bytes?

**Axis: retained body bytes per object. Nothing else.**

Three numbers per object, in one run, same bytes, same host, same court:

* **`raw_bytes`** — the tensor's exact element byte length. Nothing added. This
  is the floor no representation can beat and is a reference, not the opponent.
* **`planned_bytes`** — the shipped planner's own size for the same object:
  `TensorPlan::chosen_physical_bytes` returned by `Store::plan_tensor` with
  `TensorPlanOptions::default()` on the tensor's exact bytes, dtype, shape and a
  row-major layout, with **no causal parent, no tile shape, no auto-tiling, no
  candidate list and no calibration table**. That default offers, for a
  standalone tensor with no base: `raw`, `zstd` (level 3, plus level 19 where
  it is offered and not CPU-gated), `float-tensor` (mode 9, offered only when it
  saves at least 1/64 of the zstd frame), and `tensor-sharded` (mode 10). The
  delta, patch, tiled and engram candidates are not applicable without a base,
  a tile shape or engram metadata and are not offered by this call. The receipt
  records the chosen strategy **and every candidate's size and applicability**,
  so the candidate set is evidenced, not asserted.
* **`body_bytes`** — **every byte EKBLOOM writes for that object**, i.e. the
  complete `.ekb` container on disk (header, core, manifested structures,
  checksums, manifest and auth sections, padding — all of it). No dedup across
  tensors is allowed: each tensor is encoded independently into its own
  container and charged in full. This asymmetry is deliberate and runs **against**
  EKBLOOM — the WeftFS number is the planner's own frame accounting, while the
  EKBLOOM number includes its container metadata.

**Plan choice for the EKBLOOM arm.** The primary row uses the crate's own
default plan for a fresh object: the harness passes no tuning parameter that the
crate does not itself default to. The receipt records the exact arguments passed
and the plan name/identifier EKBLOOM reports for the object. If the public API
exposes a plan or budget ladder, the harness sweeps it and publishes every rung
as a secondary table, but **a verdict may not be taken from a non-default row**.

**Margin.** Reused, not invented: a candidate must beat the alternative by at
least **1/64 (1.5625 %)**, the saving fraction the shipped mode-9 gate already
requires (`src/tensor_store.rs`). Integer form, so there is no floating-point
ambiguity: a per-object EKBLOOM win is
`body_bytes × 64 ≤ planned_bytes × 63`.

**Verdict (fixed here, in advance):**

* **E2 win** iff the aggregate over the declared §3.2 set satisfies
  `sum(body_bytes) × 64 ≤ sum(planned_bytes) × 63` **and** every object in both
  arms restored byte-exactly.
* **E2 loss** otherwise. A tie is a loss. A per-object win on the embedding that
  reverses elsewhere is reported as a per-object table and does not change the
  aggregate verdict.
* The receipt always states: aggregate `body_bytes`, aggregate `planned_bytes`,
  aggregate `raw_bytes`, the per-object win/loss/tie counts, the largest
  per-object loss and the largest per-object win, and the ratio of aggregates in
  integer form (`body_sum × 1000000 / planned_sum` parts per million). No
  decimal is rounded in prose: a ratio in a document must be recomputed from
  these integers.

**Restore requirement.** Both arms restore every object in the same run and
compare by `memcmp` and SHA-256. A size comparison without exact restore is not
an E2 result. The WeftFS arm restores through the shipped store
(`write_tensor_file` → `read_tensor`) so its number is a real object, not only a
planner estimate; the receipt records both `planned_bytes` and the observed
on-disk footprint delta for the object, and names any difference.

---

## 5. Measurement rules

1. **One court, one phase, one invocation.** `boundary` and `space` are separate
   invocations and separate receipts. Neither phase may borrow a number from the
   other.
2. **Determinism.** Both phases are deterministic: the same tensor set, the same
   order (ascending name, per §3), the same encoder settings. A repeated run
   must reproduce every `body_bytes`, `planned_bytes` and exactness result
   identically; the receipt records a digest over the per-object size rows so a
   second run can be compared.
3. **Object order** is ascending tensor name within a container, base before
   instruct. No randomisation, no dropout.
4. **Retries are forbidden.** A failing object is recorded and fails the check.
   The only permitted exception is an infrastructure failure before any object
   was measured (for example a fetch failure), which produces no receipt and is
   reported as blocked with the exact error.
5. **Timestamps and revisions are recorded, never inferred.** The receipt names
   the stamp, the WeftFS revision, the four subproject revisions, the court image
   and its id, the kernel, the court limits, the crate version and the Cargo.lock
   checksum.
6. **Resource accounting** uses the court's own sources: cgroup memory peak,
   cgroup `pids.events max`, cgroup `memory.events oom_kill`, and the harness's
   own `/proc/self/status` peak. Values the court does not expose are recorded as
   `unavailable`, never estimated.

---

## 6. Stop rules

The experiment ends, with a published null result, on the first of
(`docs/EKBLOOM-PLAN.md` §8, restated here so the contract is self-contained):

1. **E1 fails** for any typed reason in §4.1.
2. **E2 is lost** — and, because E3/E4/E5 are out of scope in this subphase, an
   E2 loss does **not** by itself retire EKBLOOM: `docs/EKBLOOM-PLAN.md` retires
   it only when E2 **and** E3 are both lost. What an E2 loss does end is the
   space axis, permanently for this contract version, with the numbers published.
3. A win exists only on a synthetic object (not applicable in this subphase: the
   corpus is real weights only).
4. The pinned artefact cannot be reproduced (contract §1).

Retirement, if reached later, means `bench/RESULTS-ekbloom.md` states the null
result and names every receipt, this plan's status becomes retired, and no shipped
code path refers to EKBLOOM.

---

## 7. Receipts

| Phase | Receipt | Log |
|---|---|---|
| boundary | `bench/receipts/ekbloom-<stamp>.json` | `bench/receipts/ekbloom-<stamp>.log` |
| space | `bench/receipts/ekbloom-space-<stamp>.json` | `bench/receipts/ekbloom-space-<stamp>.log` |

`stamp = $(date -u +%Y%m%dT%H%M%SZ)`, overridable with `WEFT_EKBLOOM_STAMP`.
Schema name: `weft.ekbloom-court.v1` for both, with a `phase` field.

Each receipt carries at least: court name, phase, stamp, WeftFS revision, the
four subproject revisions, court image/id/kernel/limits, EKBLOOM repository
revision, crate version, `Cargo.lock` checksum, rustc and cargo versions, corpus
file size and SHA-256 as computed in the court, the selection rule and the
selected tensor list, per-tensor rows (name, dtype, shape, offsets, logical
bytes, body bytes, exactness, encode/decode ms as observational fields, process
peak), cgroup memory peak, `pids.events`, `oom_kill`, per-check pass/fail, the
phase verdict, and a `limitations` array listing every limitation actually
observed (including `verifier: not_available`, `unavailable` resource sources,
and any refused object).

Every receipt carries the label: **single host, OOM-protected Docker court
(3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded `/tmp`), public model weights
on a warm local page cache, one process, no fleet, cloud, media, power-loss, GPU
or serving claim.**

No `bench/RESULTS-ekbloom.md` is written in this subphase. A number enters a
tracked document only through `docs/perf-numbers.json` and
`scripts/perf_numbers.py`; the receipts are the evidence source.

---

## 8. Explicit non-claims

* No performance claim of any kind is made by E1. E1's axes are exactness and the
  resource bound.
* E2's only claim shape is "EKBLOOM's whole container is or is not at least
  1/64 smaller than the representation the shipped WeftFS planner chooses for the
  same bytes, on this declared set of real Qwen2.5-0.5B and Instruct tensors, in
  this court". It is not a general codec claim, not a claim about other data,
  and not a claim about EKBLOOM's `V/N` on any other corpus.
* No comparison with any mature filesystem, and no statement that anything is
  impossible elsewhere (rule 13, rule 14).
* Nothing here says EKBLOOM is or is not a useful codec in its own right. It
  says whether the pinned public crate round-trips these bytes inside this court,
  and whether its body beats this planner on these objects.
* A `pass` on E1 and a `loss` on E2 is a complete, publishable result: the
  boundary is feasible and the space axis is lost. It is not a partial success of
  the portfolio argument.
