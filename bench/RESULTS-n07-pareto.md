# N07 — strict Pareto presentation

**Status:** generated, not hand-selected. Source receipt
[`r22-pareto-20261112T230000Z.json`](receipts/r22-pareto-20261112T230000Z.json)
(the R22 25-row frontier), generator `scripts/pareto_strict.py`, output
[`n07-pareto-strict-20261114T204000Z.json`](receipts/n07-pareto-strict-20261114T204000Z.json).

**The correction.** R22 called several equal-footprint, slower rows
"Pareto-optimal" because they are useful policies. On the measured axes they are
dominated. This document publishes **two distinct sets** and no row is called
Pareto-optimal unless it is non-dominated in one of them.

* **Strict two-axis set.** A point is in the set only if no other point is
  better-or-equal on both axes and strictly better on one. Axes:
  least-contended durable wall, physical footprint.
* **Eligible/portable policy set.** The same computation over rows whose
  configuration does not remove a candidate another family needs, change the
  codec chunk layout, change the physical pack plane, or use the exhaustive
  control planner. Every included and excluded row carries a reason derived from
  its configuration name.

## 1. The generator

`scripts/pareto_strict.py` reads the receipt, classifies every row by rule, and
computes both sets. There is no allow-list of points. `--check`:

1. recomputes the frontier with an **independent quadratic domination test**
   (written separately from the set builder) and asserts equality;
2. asserts that no dominated point is in the strict set;
3. asserts that every excluded point is dominated by a frontier point;
4. repeats (2) for the eligible set;
5. re-renders the document and asserts byte-identical output;
6. runs a four-case synthetic self-test of the domination predicate — strict
   improvement, an equal-on-both-axes pair (neither dominates, both stay), a
   crossing pair, and a chain — so the test is not tied to this fixture.

The command fails closed on any violation.

## 2. The strict set

<!-- BEGIN GENERATED:n07-strict (scripts/perf_numbers.py) -->
**3 points**: the native anchor (332.0 ms, largest), the level-19-off row (575.9 ms, 282,106,049 B), and the 1 MiB pack row (282,102,521 B, slowest). The shipped default (592.6 ms) is dominated.
<!-- END GENERATED:n07-strict -->

The strict set is small because the measurements are: the native anchor is
fastest and largest; the level-19-off row is faster at the same bytes; the
1 MiB-pack row is the smallest footprint at a much higher wall. Every other row,
**including the shipped default**, is dominated on these two axes.

## 3. The eligible set

<!-- BEGIN GENERATED:n07-eligible (scripts/perf_numbers.py) -->
**2 points**: the native anchor and `C-pipe4-sync16` (**582.0 ms** at 282,106,049 B). The level-19-off row is excluded by policy; the pack rows by plane; the exhaustive planner as a control.
<!-- END GENERATED:n07-eligible -->

The level-19-off row is excluded by policy: disabling the level-19 gate wins on
this fixture and loses pythia-70m, which is a measured cost, not a taste. The
pack rows are excluded because they change the physical plane and therefore the
footprint. The exhaustive planner is excluded because it is a control, not a
policy. `C-pipe4-sync16` is the eligible frontier; it is not claimed to be
separable from the default on the median (see §5).

## 4. The frontier points and their policy status

<!-- BEGIN GENERATED:n07-table (scripts/perf_numbers.py) -->
| point | min ms | footprint B | set |
|---|---:|---:|---|
| native anchor | 332.0 | 424,439,296 | strict + eligible (comparator) |
| level-19-off | 575.9 | 282,106,049 | strict (ineligible by policy) |
| 1 MiB pack | 962.7 | 282,102,521 | strict (ineligible by plane) |
| `C-pipe4-sync16` | 582.0 | 282,106,049 | eligible |
| shipped default | 592.6 | 282,106,049 | neither (dominated) |
<!-- END GENERATED:n07-table -->

## 5. Confidence

<!-- BEGIN GENERATED:n07-confidence (scripts/perf_numbers.py) -->
25 rows are classified; 0 are single-run. Each row carries a bootstrap interval from its own repetitions in the output receipt, and the receipt states that no paired statistic exists for this unpaired data.
<!-- END GENERATED:n07-confidence -->

The receipt carries a seeded 10,000-resample bootstrap interval of each row's
own median wall. It has repeated but **unpaired** cells, so no paired difference
and no significance claim is made; the generator records that explicitly. The
rows in the middle of the table are one measurement band: R22 measured a large
median shift for the same row between sessions while its least-contended value
barely moved. Differences smaller than the declared ~10 % band are not treated
as real.

## 6. Losses and non-claims

* The eligible set is a **policy** statement; it is not a claim that the
  excluded rows are invalid. Each exclusion has a measured or declared reason.
* The strict set is only two-axis (wall, footprint). Cold restore, CPU, RSS and
  the restore frontier are separate axes and are reported elsewhere; a row
  dominated here can win there.
* The native anchor is in the eligible set as the comparator, not as a WeftFS
  policy.
* No row is called "Pareto-optimal" without the set qualifier.
* Single-run cells would be marked; this receipt has seven repetitions for every
  row, so none are. The generator still implements and reports the marker.
* One host, one fixture; the generator is descriptive and makes no fleet claim.

## 7. Reproduce

```
python3 scripts/pareto_strict.py --self-test
python3 scripts/pareto_strict.py \
  --receipt bench/receipts/r22-pareto-20261112T230000Z.json \
  --out bench/receipts/n07-pareto-strict-<stamp>.json --check
```

## Claim register

- `claim:n07_strict_set_generated` — the strict set is computed from the receipt by `scripts/pareto_strict.py`; no point is hand-selected.
- `claim:n07_two_sets` — a strict two-axis set and a separately named eligible policy set are published, with a reason for every inclusion and exclusion.
- `claim:n07_domination_asserted` — the generator asserts that no dominated point appears in either set and that an independent quadratic computation agrees.
- `claim:n07_self_test` — the domination predicate is tested on synthetic tie, duplicate and crossing cases.
- `claim:n07_unpaired_confidence` — per-row bootstrap intervals are published; no paired statistic is invented for this unpaired receipt.
