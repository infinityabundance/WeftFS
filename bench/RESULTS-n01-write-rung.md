# N01 — closing local durable compressed-write parity (the 336 ms rung)

**Status:** executed on `staging`. The measurement ran on the tree that became
commit `6d79c4c` on top of base `581f364`; only receipts and documentation were
added afterwards. Submodule `WeftFS-Go` is `a8e8e7a` for N05 and is not on this
path.

**Headline.** The pre-change durability tail was one barrier lane: the early
barrier `fdatasync`ed 202 published files serially while the encoders were still
running, and that lane was the longest non-overlappable term. Running the same
barriers on a bounded lane pool makes the tail shorter than the encode chain, so
it is now hidden. The rung is **not met**: the shipped row lands at
<!-- BEGIN GENERATED:n01-headline (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-headline -->
least-contended against a 336 ms target. The remaining floor is named in §4 and
is no longer durability.

Receipts:

| Receipt | What it is |
|---|---|
| [`r22-pareto-20261114T195000Z.json`](receipts/r22-pareto-20261114T195000Z.json) | **cited court.** Six rows × seven interleaved repetitions, seeded shuffled order, 11/11 checks pass. Pre-change `C-pipe4-wb1`, shipped `C-pipe4` (4 lanes), best-lane `C-pipe4-wb8`, plus `A`, `C`, `C-legacy` for the identity and decision-diff checks. |
| [`augmentation-local-20261114T195000Z.json`](receipts/augmentation-local-20261114T195000Z.json) | **cited.** The unmodified R3 contract court at the same stamp; `default_c_matches_official_court=true`. |
| [`n01-write-rung-20261114T195000Z.json`](receipts/n01-write-rung-20261114T195000Z.json) | **cited.** The assembled N01 receipt: rows, delta, split, floor, identity and 7B cells. |
| [`n01-phase-split-20261114T195000Z.json`](receipts/n01-phase-split-20261114T195000Z.json) | **cited.** One phase-profiled write in the same session: `blob_publish`, `encode`, `analyse`, `digest`, `index_segment`, `manifest_commit`, early barrier and the publication ledger. |
| [`n01-identity-20261114T195000Z.json`](receipts/n01-identity-20261114T195000Z.json) | **cited.** `wb1` vs `wb8` field-by-field: output content roots, representation choices, physical bytes, footprint, generation, storage and publication ledger all equal. |
| [`scale-7b-20261114T195110Z.json`](receipts/scale-7b-20261114T195110Z.json) | **cited for the 7B no-regression cells only.** Planned 8-way and fast-raw 8-way, unchanged limits, one store alive at a time. |
| `r22-pareto-20261114T192435Z.json.zst`, `…T193010Z.json.zst`, `…T193439Z.json.zst` | **Retained, not cited as gates.** Narrowed exploratory sweeps whose structural checks fail by construction (no `C`/`C-legacy` row, or no matching contract receipt). Their timings are real and were used to design the lane change. |
| [`n02-durability-tail-20261114T201211Z.failed.log`](receipts/n02-durability-tail-20261114T201211Z.failed.log) | **Retained failure.** The first N02 probe run: the grouped variants passed the store directory as a file. No receipt was produced. |

---

## 1. The split before the change

The contract statistic is the **least-contended** repetition; median, p95 and
p99 come from the same seven samples and are published unchanged. The pre-change
row is the R21/R22 default with one writeback lane.

<!-- BEGIN GENERATED:n01-baseline (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-baseline -->

The phase profile of one write (same session, shipped lanes) separates the
mechanism from the wall. `encode`, `analyse` and `digest` are thread-additive,
so they are CPU work rather than critical-path time; `blob_publish` and the
early barrier are I/O work; `index_segment` and `manifest_commit` are the
control plane.

<!-- BEGIN GENERATED:n01-split (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-split -->

The publication ledger is exact: 186 manifest blobs + 16 object blobs, one file
barrier each, and 161 directory barriers, which is the same count R20 measured.

## 2. What changed

`TensorPipelineOptions::writeback_threads` (default
`min(available_parallelism, 4)`, bounded 1..=8). The early barrier partitions
the drained file list round-robin across lanes; each lane opens and
`fdatasync`s its own files; a failure on any lane fails the pipeline after every
lane has joined, so nothing is committed. The commit's own barrier still closes
the deferred batch and `fsync`s every remaining file and directory before the
commit record. The **number of barriers is unchanged**: exactly one per
published file, and `tests/tensor_pipeline.rs` asserts the same head, per-object
ids and manifest bytes for 1 versus 8 lanes.

The change is deliberately narrow. It does not touch encode, publication
bytes, the pack plane, the planner or the commit protocol, because those are
what the footprint and the roots depend on.

## 3. The result

<!-- BEGIN GENERATED:n01-result (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-result -->

Identity, on the same fixture and seed:

<!-- BEGIN GENERATED:n01-identity (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-identity -->

So the lane change buys wall time at an identical committed state. The best
measured lane count (`wb8`) is within a few milliseconds of the shipped 4-lane
default on the least-contended statistic; the 4-lane default is kept because it
matches the CPU count and the difference is not separable on the median in this
session (one 678.0 ms sample against 579.3 ms; the minima are 542.3 and 544.4 ms).

## 4. The floor, and why 336 ms is not reached

<!-- BEGIN GENERATED:n01-floor (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-floor -->

The tail that dominated R22 is now hidden: the barrier's busy time is shorter
than the pipeline it runs under. What remains is the embedding's own
read → analyse → encode chain plus the final fragment write and the commit. A
336 ms wall would need that chain to fall by the amount the generated block
states, which is an encoder problem, not a durability one.

Two changes were considered and **not** made:

* **Packing the blob plane** changes the footprint (measured `282,102,521 B` for
  `pack1m` against `282,106,049 B`) and is slower for this workload; N02 measures
  the device-level win and the store-level loss.
* **A seal/catalog lane** would move pack sealing off the publisher, but the
  pack plane is not the shipped representation and its footprint differs, so it
  cannot satisfy this gate's "footprint and every root unchanged" clause.

## 5. No regression on the 7B cells

<!-- BEGIN GENERATED:n01-7b (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-7b -->

The changed module is not on the checkpoint writer's path
(`src/checkpoint.rs` never calls `TensorPipelineOptions` or
`prepare_tensor_group_pipelined`), so these cells are a no-regression check, not
an equal-treatment comparison. The planned cell is ~1.9× slower than the X04
session's published value while writing the same bytes; because the changed code
is unreachable from that phase, it is host variance and is labelled as such.

## 6. Losses and non-claims

<!-- BEGIN GENERATED:n01-losses (scripts/perf_numbers.py) -->
<!-- END GENERATED:n01-losses -->

* Single host, one writer, local POSIX backend, warm page cache; no media,
  power-loss, fleet, cloud or Go claim.
* The pipeline is a store-API path. No shipped CLI verb reaches it, which R25
  already disclosed; this document does not call it the product default.
* The 336 ms target is not redefined. The achieved values are the least
  contended and the median of seven shuffled repetitions.
* CPU and peak RSS are unchanged by the lanes; the change is wall-only.
* Crash ordering is unchanged and re-proved by the court's
  `crash_proxy_old_or_new` check on every Weft row, plus the packed-race suite
  (`tests/packed_races.rs`, 5/5) run separately.

## 7. Reproduce

```
WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local
WEFT_R22_STAMP=<stamp> WEFT_R22_REPS=7 WEFT_R22_ORDER_SEED=20261114 \
  WEFT_R22_CONFIGS='A,C,C-legacy,C-pipe4-wb1,C-pipe4,C-pipe4-wb8' \
  scripts/docker-court.sh bench "bash bench/scripts/r22_pareto_court.sh"
WEFT_SCALE_PHASES="planned flat" bash bench/scripts/scale_7b_court.sh
python3 bench/scripts/n01_assemble_receipt.py \
  bench/receipts/r22-pareto-<stamp>.json \
  bench/receipts/n01-phase-split-<stamp>.json \
  bench/receipts/n01-identity-<stamp>.json \
  bench/receipts/scale-7b-<stamp2>.json \
  bench/receipts/n01-write-rung-<stamp>.json <stamp>
```

## Claim register

- `claim:n01_rung_not_met` — the 336 ms rung is published as not met with the achieved least-contended and median walls, and is not redefined.
- `claim:n01_lane_identity` — one versus eight writeback lanes produce identical content roots, representation choices, physical bytes, footprint and generation on the fixture.
- `claim:n01_barrier_once_per_file` — every published file is still `fdatasync`ed exactly once; the lanes change only when, never whether.
- `claim:n01_floor_named` — the remaining non-overlappable term is the embedding's read + encode chain, not durability.
- `claim:n01_no_7b_regression` — the reduced 7B planned and fast-raw cells were re-run on the changed tree and published, with the host-variance caveat.
- `claim:n01_host_spread_published` — p95/p99 and every raw sample are published, and the host stalls are disclosed rather than trimmed.
