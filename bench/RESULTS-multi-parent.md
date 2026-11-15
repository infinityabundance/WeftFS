# T091 — multi-parent reconstruction: a null result with a measured boundary

**Status:** research gate, executed on `staging`. The container described here is
a **research artifact**: it is not a wire mode, not a planner candidate, and not
on any shipped path. Receipt: `bench/receipts/multi-parent-<stamp>.json`
(court `bench/scripts/multi_parent_court.sh`, 10/10 checks).

Everything numeric below is emitted from that receipt by
[`scripts/perf_numbers.py`](../scripts/perf_numbers.py); the canonical table is
[`docs/PERF-NUMBERS.md`](../docs/PERF-NUMBERS.md).

## 1. Hypothesis

An exact tensor region might be cheaper to reconstruct from **several** causal
sources than from one: checkpoint *A* preserves the layers that did not move
since initialisation while checkpoint *B* preserves the recently drifted ones,
so a container that may copy from either could shrink below the best
single-parent delta.

## 2. Method

**Container.** `WMP1` version 1: magic, version, dtype, shape, one 32-byte
BLAKE3 root per parent, target length, body length, the op stream, and a
trailing BLAKE3 of the target. The op stream is the **shipped mode-3**
COPY/INSERT encoding — 4-byte keys, at most 1,000,000 sampled index positions,
four candidates per key, greedy longest match of at least 8 bytes — with one
extra byte per COPY naming the parent. Decode verifies every declared parent
root, the exact decoded length, and the trailing target root; a missing parent
or a parent whose bytes do not hash to the declared root is a typed refusal that
returns no bytes.

**Corpus.** Real pythia-70m checkpoints at steps 0, 1, 2, 4, 8, 16, 64, 512 and
1000. Six targets (1, 8, 16, 64, 512, 1000); for each, a bounded parent list of
the nearest earlier checkpoint, step 0, then the next nearest; tensors sampled
by **byte length** (alphabetical sampling is biased: every pythia `*.bias` sorts
first and is 2 KiB, which the first run of this court actually produced and is
retained as a defect, see §6). Every subset of every fan-out is encoded, so the
reported multi-parent number is the best a planner could do at that fan-out.

**Baselines**, all on the same bytes: the best single-parent mode 3 (`DELTA`)
frame, the best single-parent mode 11 (`WEIGHT_DELTA`) frame against that
parent's mode-9 frame, the best single-parent mode 16 (`SPARSE_DELTA`), and
mode 9 on the target itself.

**Three byte metrics**, because the first two are confounded:
1. *on-disk* bytes — the container or frame as stored;
2. *payload-normalised* op-stream bytes — the container body minus one
   parent-index byte per COPY and minus the terminator, which at fan-out 1 is
   **byte-identical to the shipped mode-3 body** (checked on every sample);
3. the reconstructed content bytes.

## 3. Result

<!-- BEGIN GENERATED:t091-result (scripts/perf_numbers.py) -->
Measured on **72 tensors** (**50,503,680 B**, **48.16 MiB**, of real pythia-70m weights);
the sampled tensors differ from their nearest parent in **0.6952** of 16-bit words.
Best single-parent bytes: mode 3 **42,082,557 B**, mode 11 **13,360,077 B**,
mode 16 **51,940,819 B**; mode 9 on the target itself **41,983,061 B**.
Best multi-parent container: **42,090,550 B**, i.e. **1.0002×** the best
single-parent mode 3 frame and **3.1505×** the best single-parent mode 11 frame.
<!-- END GENERATED:t091-result -->

<!-- BEGIN GENERATED:t091-fanout (scripts/perf_numbers.py) -->
Aggregate bytes by fan-out:
fan-out 1 **42,092,229 B** (1.0002× mode 3),
fan-out 2 **42,093,300 B** (1.0003×),
fan-out 3 **42,096,402 B** (1.0003×),
fan-out 4 **42,099,436 B** (1.0004×).
The best configuration is fan-out 1 on **65** of 72 tensors.
<!-- END GENERATED:t091-fanout -->

<!-- BEGIN GENERATED:t091-payload (scripts/perf_numbers.py) -->
Payload-normalised (op-stream only) multi-parent **42,052,505 B** against
**42,071,901 B** for the best single-parent mode 3 body: a win of
**19,396 B**, **0.046%** of the single-parent body, on
**17** tensors. The largest single-tensor payload win is
**2,738 B** (a fraction **0.0176** of that tensor).
<!-- END GENERATED:t091-payload -->

<!-- BEGIN GENERATED:t091-decode (scripts/perf_numbers.py) -->
Decode wall summed over the measured tensors: multi-parent **29.27 ms**,
mode 3 **41.97 ms**, mode 11 **1,938.70 ms** (single-threaded,
in-memory, including every format's own hash verification). Availability: **144**
typed refusals out of **144** checks, no bytes returned.
<!-- END GENERATED:t091-decode -->

## 4. What the numbers say

* **Payload level: a real but negligible win.** Multi-parent reduces the op
  stream on a minority of tensors, by bytes that are irrelevant next to the
  tensor. The wins concentrate on the large attention/MLP matrices at later
  steps, where a second parent covers runs the nearest parent has already lost.
* **On-disk level: a loss.** Charging the container's own metadata — one 32-byte
  parent root per parent, one parent-index byte per COPY, and the op-stream
  terminator — the best multi-parent container is *larger* than the best
  single-parent mode-3 frame in aggregate. The per-tensor on-disk wins that do
  exist are a **header-size** effect (the container header is about fifty bytes
  smaller than a mode-3 frame), not a reconstruction gain: the fan-out-1 control
  exhibits them too.
* **Against the best single-parent semantic delta, it is not close.** Mode 11
  stores the same tensors in a fraction of the bytes multi-parent needs; the
  byte-level matcher is simply the wrong representation for float weights. Any
  multi-parent proposal has to be built on the mode-11/16 machinery, and on this
  corpus the nearest parent already covers every plane run the second parent
  could add.
* **Fan-out never pays.** The best configuration at fan-out 1 is chosen for most
  tensors, and the aggregate grows monotonically with fan-out. A second parent
  costs a root plus one byte per copy and must recover more than that; on
  scattered edits it does not.
* **Decode is not the objection.** The container decodes at a higher throughput
  than mode 3 and far above mode 11 on these tensors, so the loss is bytes, not
  CPU.

### 4.1 Where it could still win

The mechanism can only win when **different parents cover long, disjoint runs**
that the nearest parent does not have, and when those runs are long enough to
amortise a parent root plus one byte per copy. Pythia's steps are scattered
edits, not layer-disjoint rewrites; a mixture-of-experts fine-tune, a merged
adapter family, or a sharded checkpoint where one shard is stale in one parent
and current in another is the shape of data this court did **not** test.

## 5. Every loss in this subphase

1. On-disk aggregate is worse than the best single-parent mode 3 (generated
   above).
2. Aggregate is far worse than the best single-parent mode 11.
3. Fan-out 2, 3 and 4 are all worse than fan-out 1 in aggregate.
4. Per-COPY parent index: one byte per copy, a permanent cost of the format.
5. One 32-byte root per parent, so a second parent costs 32 bytes before any
   benefit.
6. Greedy matching is not monotone: adding a parent can *shift* a match and lose
   bytes overall (measured: fan-out 2 is worse than fan-out 1 on some tensors
   even before the root is charged).
7. The first run of this court sampled tensors alphabetically and therefore
   measured only 2 KiB bias tensors; its receipt
   `multi-parent-20261115T153633Z.json` is **retained** and superseded.
8. A second run recorded a fan-out-1 control that differed from mode 3 by up to
   3,384 bytes; the cause was the per-COPY parent byte, and the retained receipt
   `multi-parent-20261115T153851Z.json` is what forced the metric to be
   split into on-disk and payload forms.
9. Availability refusals are checked with a missing parent and a corrupted
   parent only; a partially written parent (truncated but self-consistent) is not
   modelled.
10. Restore **wall** into a materialised file is not measured here; the measured
    decode is in-memory. The earlier programme receipts show that writing a
    multi-gigabyte container is writeback-bound, so a wall-clock claim would be a
    disk claim, not a reconstruction claim.

## 6. What is not claimed

* No shipped mode, no planner slot, no dependency edge, no spec change.
* Nothing about Go.
* Nothing about cold media, power loss, a fleet, a cloud, or another filesystem.
* No claim that multi-parent cannot win; the boundary in §4.1 is untested data
  shapes.
* The wall-clock numbers are one host, warm cache, single-threaded.

## 7. Reproduce

```
bash bench/scripts/multi_parent_court.sh
```

A bench court measures and a fuse court assembles the receipt. Reduced knobs:
`WEFT_MP_TARGETS`, `WEFT_MP_TENSORS`, `WEFT_MP_PARENTS`, `WEFT_MP_FANOUTS`,
`WEFT_MP_REPS`, `WEFT_MP_MAX_TENSOR_BYTES`. The script prints
`multi_parent_court: skip <reason>` and exits 78 when Docker, the images or the
corpus are unavailable.
