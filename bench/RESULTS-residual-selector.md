# T093 — residual-fingerprint selector: a null result with a measured boundary

**Status:** research gate, executed on `staging`. The contract
([`bench/CONTRACTS-selector.md`](CONTRACTS-selector.md)) was committed **before**
any held-out measurement existed. Receipt:
`bench/receipts/residual-selector-<stamp>.json` (court
`bench/scripts/residual_selector_court.sh`, 6/6 checks).

Everything numeric below is emitted from that receipt by
[`scripts/perf_numbers.py`](../scripts/perf_numbers.py); the canonical table is
[`docs/PERF-NUMBERS.md`](../docs/PERF-NUMBERS.md).

## 1. Hypothesis

Cheap fingerprints of a tensor **and of its residual against its causal parent**
— byte entropy, zero-word fraction, high-plane (exponent) entropy, residual
entropy, residual zero fraction — carry enough information to choose an exact
representation nearly as well as measuring every candidate, and better than the
trivial selectors. This re-opens the earlier entropy-coder court's conclusion
("promote nothing") with the one hypothesis that has a plausible mechanism:
residual structure, not whole-tensor entropy.

## 2. Method

**Split**, declared and committed before measurement: FIT is Qwen2.5-0.5B base
and Instruct (residual against the base) plus pythia-70m steps 0 and 8 (residual
against step 0); VALIDATION is Qwen2.5-3B base, used only to pick the tree depth
and to fit the trivial thresholds; HELD-OUT is Qwen2.5-3B-Instruct with the 3B
base as parent, measured **once**. Tensors are sampled by byte length (an
alphabetical sample is biased towards `*.bias`), under declared byte and count
caps.

**Candidates** are exact only: `raw`, `zstd-3`, `float-tensor` (mode 9). Every
candidate is decoded and compared with the input on every tensor of every split.

**Objective**: `bytes + (encode + decode) × 0.001` — one millisecond of CPU is
worth one kilobyte of storage. The bytes-only total is reported as a second
reading.

**Policies**: always-zstd, always-best-of-two (measure zstd and mode 9, take the
cheaper), a fitted residual-entropy threshold, a fitted size threshold, a
cost-sensitive decision tree over the five features, and an oracle that measures
all three candidates.

## 3. Result

<!-- BEGIN GENERATED:t093-fit (scripts/perf_numbers.py) -->
FIT **160** tensors, VALIDATION **24**, HELD-OUT
**24** tensors measured once. The tree depth chosen on VALIDATION is
**2**.
<!-- END GENERATED:t093-fit -->

<!-- BEGIN GENERATED:t093-policies (scripts/perf_numbers.py) -->
Held-out total cost (objective units) and regret over the oracle (**28,099,256.52**,
**26,636,085 B**): always-zstd **31,570,290.89** (regret
**3,471,034.37**, **0.12353**); always-best-of-two regret
**38.19**; entropy threshold regret **3,471,034.37**; size
threshold regret **38.19**; fingerprint tree **28,099,272.97** (regret
**16.45**, **0.0000006** of the oracle).
<!-- END GENERATED:t093-policies -->

<!-- BEGIN GENERATED:t093-planning (scripts/perf_numbers.py) -->
The fingerprint pass over the held-out bytes takes **620,265,151 ns**
(**16,308,098 ns/MiB**); the decision itself takes
**33.4 ns** per tensor. At the declared exchange rate the pass costs
**620,265.15** objective units.
<!-- END GENERATED:t093-planning -->

<!-- BEGIN GENERATED:t093-verdict (scripts/perf_numbers.py) -->
The tree beats the best trivial selector by **21.73** units, i.e. it is
**0.9999992×** its cost, while its own fingerprint pass costs
**620,265.15** units: a net **-620,243.42** units. The
advantage is **not** claimed: it is below the resolution of two-repetition encode/decode
medians, and the feature pass is four orders of magnitude more expensive than the saving.
<!-- END GENERATED:t093-verdict -->

## 4. What the numbers say

* **The trivial baseline is already at the oracle.** Always-best-of-two is within
  a rounding error of trying everything, because `raw` only ever wins on tensors
  small enough that the frame headers dominate. The whole selection problem is a
  size problem, not a content problem.
* **The tree's arithmetic pass of G1 is not a win.** It beats the best trivial
  selector by a fraction of a part per million of total cost — below the
  resolution of the encode/decode medians (two repetitions per candidate), so it
  is not distinguishable from noise and it is **not** claimed as a win.
* **Its own feature pass costs thousands of times what it saves.** The
  fingerprint pass is a full extra read of the tensor; charged at the declared
  exchange rate it exceeds the entire saving by four orders of magnitude. A
  selector that has to read the bytes it is selecting for is only viable if the
  information it extracts replaces work that would otherwise happen — here it
  does not.
* **Two features produced degenerate rules.** The fitted residual-entropy
  threshold chose zstd-3 on *both* sides (no usable signal in range), and the
  tree's second split has the same leaf strategy on both sides. The only split
  that carries information is `log2_bytes`.
* **The one real finding is free.** `raw` beats both framed candidates on
  sub-kilobyte tensors, and a size threshold captures that with no feature pass
  at all. That is the recommendation the data supports: a size rule, not a
  learned model.

## 5. Every loss

1. The learned selector does not materially beat the trivial selectors: its
   advantage is below the measurement resolution.
2. The fingerprint pass costs far more than it saves (§4).
3. Residual entropy is not informative on this data; the fitted threshold is
   degenerate.
4. Half of the tree is degenerate (both leaves the same strategy).
5. Depth selection on VALIDATION is a tie between depths 2 and 3, so the extra
   depth is not justified by the data.
6. Encode and decode medians come from two repetitions; a 1e-6 difference between
   policies cannot be separated from noise. The court's G1 pass is therefore
   reported as arithmetic, not as evidence.
7. The held-out split is one family. A different family could rank the policies
   differently; nothing here bounds that.
8. The candidate set excludes zstd-19 and the tiled/sharded modes, so the oracle
   is an oracle over three candidates, not over the product's full candidate set.
9. The 0.001 bytes-per-nanosecond exchange rate is a declared modelling choice,
   not a measured business constant. The bytes-only reading is published too;
   under bytes-only, always-best-of-two is still within a rounding error of the
   oracle.
10. One measurement run of this harness (same revision, before the court script
    existed) wrote its output to the container's tmpfs and was discarded because
    the file could not be retrieved; only its stdout summary existed. It fitted
    the same model on the same FIT/VALIDATION data, so it could not have
    influenced the contract or the fit, and the published receipt is the only
    held-out reading cited.

## 6. What is not claimed

* No shipped planner change; no default path was touched.
* No claim that learned selection cannot work; the boundary is that these five
  cheap features over this objective do not beat a size rule.
* No lossy comparison, no zstd-19/mode-15 candidate, no Go parity.
* Nothing about cold media, power loss, a fleet, a cloud or another filesystem.

## 7. Reproduce

```
bash bench/scripts/residual_selector_court.sh
```

Reduced knobs: `WEFT_RS_REPS`, `WEFT_RS_FIT`, `WEFT_RS_EVAL`,
`WEFT_RS_FIT_BYTES`, `WEFT_RS_EVAL_BYTES`. The script prints
`residual_selector_court: skip <reason>` and exits 78 when Docker, the images or
the corpus are unavailable.
