# N02 — the 202-file durability tail, quantified

**Status:** executed on `staging` at `6d79c4c`. One host, one bench court,
unchanged limits (`3 GiB` memory == swap, `1024` pids, `4` CPUs, bounded
`/tmp`).

**Headline.** The tail is real and it is per-file barrier latency, not block
bandwidth. Writing the store's own 202 published blob files costs
<!-- BEGIN GENERATED:n02-variants (scripts/perf_numbers.py) -->
| layout | files | min ms | median ms |
|---|---:|---:|---:|
| per-object (shipped) | 202 | 368.3 | 7,405.3 |
| 17 larger files + index | 17 | 149.6 | 563.1 |
| one contiguous pack + index | 2 | 140.5 | 3,117.2 |
<!-- END GENERATED:n02-variants -->
least-contended; the same bytes in fewer, larger files save most of it, and one
contiguous pack with an in-file index saves about the same. The store-level pack plane
**does not capture that win**: it is slower than the shipped flat plane because
sealing and catalog publication are charged to the publisher. The device-level
comparison clears the 10 % gate with a paired interval; the product change does
not follow from it, and is not made.

Receipts:

| Receipt | What it is |
|---|---|
| [`n02-durability-tail-20261114T195000Z.json`](receipts/n02-durability-tail-20261114T195000Z.json) | **cited.** The traced account and the three layouts: 7 interleaved, seeded-order repetitions each, same device, same acknowledgement contract. |
| [`n02-durability-tail-20261114T195000Z.log`](receipts/n02-durability-tail-20261114T195000Z.log) | **cited.** Device control, the phase-profiled store write and the probe output. |
| [`r22-pareto-20261114T201500Z.json`](receipts/r22-pareto-20261114T201500Z.json) | **cited for the store-level comparison only.** `C-pipe4` versus `C-pipe4-pack1m`, 7 repetitions. Its structural checks fail by construction (narrowed row set, no contract receipt); the timings are real and the direction agrees with the earlier sessions. |
| [`n02-durability-tail-20261114T201211Z.failed.log`](receipts/n02-durability-tail-20261114T201211Z.failed.log) | **Retained failure.** First probe run passed the store directory as a source file; no receipt. |
| `r22-pareto-20261114T192435Z.json.zst`, `…T193010Z.json.zst`, `…T193439Z.json.zst` | Retained exploratory sweeps, not cited as gates. |

---

## 1. The traced account

The store write is one `C-pipe4` write of the frozen 101-tensor fixture with the
phase collector on. The account below is a **critical-path** account, not a sum
of thread-additive phase totals: the early barrier overlaps encode and
publication, so it is not added to the path. The residual is the harness loop
and the prepare prologue.

<!-- BEGIN GENERATED:n02-account (scripts/perf_numbers.py) -->
| term | ms |
|---|---:|
| init | 20.3 |
| reader | 44.0 |
| encode wall | 415.9 |
| publisher finish tail | 109.6 |
| commit | 10.1 |
| footprint walk | 0.7 |
| **accounted** | **600.6** |
| **measured wall** | **614.7** |
| residual | 14.1 (2.299 %) |
<!-- END GENERATED:n02-account -->

Closure check: the six terms sum to the accounted total, and
`measured − accounted = residual`, which the receipt states in milliseconds and
percent. The closure was verified arithmetically from the receipt and by
requiring the residual to be non-negative and below 5 % of the wall; it is never
reported as zero.

The I/O cross-check compares the store's own publication ledger with the bytes
the probe copied, and the barrier counts the store actually issued:

<!-- BEGIN GENERATED:n02-io (scripts/perf_numbers.py) -->
* published blob payload: **281,981,457 B**; backend bytes written: **282,118,555 B**. The difference is the control plane and source/probe writes, itemised in the receipt.
* barriers the store issued: **213** file barriers and **161** directory barriers.
<!-- END GENERATED:n02-io -->

The difference between backend bytes and blob payload is the control plane
(commit records, superblock slots, index segments and `FORMAT`/`LOCK`/probe
writes); it is accounted, not called overhead-free. `/proc/diskstats` deltas are
recorded in the receipt but are host-wide, so they are published as an upper
bound and are not used to claim device throughput.

## 2. Fewer larger files versus one contiguous pack

All three variants write the *same* payload — the store's own published blob
ranges, whose byte count is in the generated I/O block above — on the same
device, with the same acknowledgement contract: every byte written, one
`fdatasync` per file, one directory `fsync`. Nothing is transcoded before
acknowledgement.

<!-- BEGIN GENERATED:n02-variants-b (scripts/perf_numbers.py) -->
| layout | files | min ms | median ms |
|---|---:|---:|---:|
| per-object (shipped) | 202 | 368.3 | 7,405.3 |
| 17 larger files + index | 17 | 149.6 | 563.1 |
| one contiguous pack + index | 2 | 140.5 | 3,117.2 |
<!-- END GENERATED:n02-variants-b -->

The unpaired distributions are contaminated: the host stalled for seconds in
individual cells, which is why the spreads are published as measured rather than
used as the gate. The gate statistic is **paired** within each repetition, with
a seeded percentile bootstrap:

<!-- BEGIN GENERATED:n02-paired (scripts/perf_numbers.py) -->
| layout | paired median saving | bootstrap 95 % interval |
|---|---:|---|
| 17 larger files + index | 60.80 % | 54.67 % .. 92.40 % |
| one contiguous pack + index | 60.82 % | 56.95 % .. 63.17 % |
<!-- END GENERATED:n02-paired -->

**Verdict: the winning variant clears the 10 % gate with a paired median of about
60 % and a bootstrap interval entirely above 10 %.** Two variants tie — the larger-file layout and
one contiguous pack — because both remove most of the per-file barrier
count, and the device-level difference between the two file counts is small next
to the step away from one barrier per published object.

## 3. What the store-level pack plane does with that win

<!-- BEGIN GENERATED:n02-store-level (scripts/perf_numbers.py) -->
| store-level row | min ms | footprint B |
|---|---:|---:|
| flat plane (`C-pipe4`) | 597.7 | 282,106,049 |
| 1 MiB pack plane (`C-pipe4-pack1m`) | 1,221.3 | 282,102,473 |
<!-- END GENERATED:n02-store-level -->

The pack plane is **not** the same experiment: it seals members into
content-addressed packs and publishes a catalog, so it adds work the probe does
not do. It also changes the footprint. It loses at this operating point, as it
did in R20 and R22. This receipt therefore supports the *device-level* finding
and not a product change.

## 4. Durability, precisely

| Variant | What is guaranteed after the barrier |
|---|---|
| per-object (shipped layout) | Each published file is individually durable after its own `fdatasync`; its directory entry is durable after the directory `fsync`. A crash between files leaves a durable prefix and no torn file. |
| 17 larger files + index | Each group file is individually durable. The index is a separate file and is **not** atomic with the groups, so a crash can leave a durable group with no durable index; reconstructing a member additionally needs the index. |
| one contiguous pack + in-file index | One `fdatasync` makes the whole pack durable, but members and index share one barrier; a torn append is visible unless an external record names a verified length. The store's pack plane adds exactly that catalog and commit ordering. |

R22's shipped row is the first variant. The second and third are compared as
layouts, not adopted as store formats. Crash and GC for the *shipped* plane are
re-proved in the same session by the court's three-phase crash proxy and by
`tests/packed_races.rs` (5/5) and `tests/packs.rs` (15/15).

## 5. Losses and non-claims

* The host spread is enormous in this session (seconds), and the unpaired p95 is
  not a usable statistic. The paired interval is published *and* the unpaired
  spread, so a reader can see the difference.
* The probe copies published bytes with Python; it is a device/acknowledgement
  comparison, not a store write. Its absolute wall is not comparable to the
  store's wall.
* `/proc/diskstats` is host-wide and is an upper bound.
* The store-level pack plane loses; no default, format or footprint changes.
* `fdatasync` versus `fsync`: the shipped backend already uses `sync_data` for
  the early barrier and `sync_all` for the commit barrier. This document does
  **not** claim `fdatasync` is sufficient for every future layout; it records
  what the shipped plane does.
* One host, warm page cache, no media, power-loss, fleet or cloud claim.

## 6. Reproduce

```
WEFT_N02_STAMP=<stamp> WEFT_N02_REPS=7 \
  scripts/docker-court.sh bench "bash bench/scripts/n02_durability_tail_court.sh"
python3 bench/scripts/n02_assemble_receipt.py \
  .docker-cache/n02-durability-<stamp>/write.json \
  .docker-cache/n02-durability-<stamp>/probe.json \
  bench/receipts/n02-durability-tail-<stamp>.json <stamp>
```

## Claim register

- `claim:n02_trace_closes` — the critical-path terms plus the stated residual sum to the measured store wall, and the residual percentage is published.
- `claim:n02_paired_gate_clears` — the winning layout clears 10 % on the paired median with a bootstrap interval above 10 %.
- `claim:n02_store_pack_loses` — the store-level pack plane is slower and changes the footprint; no product change follows.
- `claim:n02_durability_stated` — each layout's exact durability is stated, including the index non-atomicity of the grouped variants.
- `claim:n02_host_spread_published` — unpaired min/median/p95 and every sample are published next to the paired statistic.
