# Whole training trajectory in near-final size — measured

Receipt: `bench/receipts/trajectory-20261107T220652Z.json`
Court: `bench/scripts/trajectory_court.sh` in `weft/bench-court:1`
(`sha256:4041d66e…`), Linux 7.3.1, 3 GiB, 4 CPUs, pids 1024, bounded `/tmp`.
Single host, warm page cache, no fleet claim.

Each step is one `weft.checkpoint.v1` checkpoint written with
`--causal-checkpoint <previous>`; a tensor with the same name, dtype and shape
offers the previous step's matching shard version as a causal parent, so the
planner can choose a weight/sparse delta. `--shard-strategy planned` (new in
this subphase) lets the planner choose per shard. `--no-semantic` skips the
mode-14 candidate search (it decodes live tensors; see the OOM note below).

Cumulative bytes = `objects/` + `manifests/` after the step (indexes are
reported separately). All restores reproduce the exact source container
(header included) and every tensor's SHA-256.

## Pythia-70M, real public weights

Steps `0, 1, 2, 4, 8, 16, 64, 512, 1000` of
`EleutherAI/pythia-70m`. The first five were already cached; `step16`,
`step64`, `step512`, `step1000` were fetched in a python court and each verified
against the HF LFS SHA-256 (`c93333b4…`, `d6091e01…`, `a46bd4c2…`,
`09883f90…`). F32, 76 tensors, 281,715,176 B each (fp16-precision values
widened to f32; recorded in the P2 receipt).

| step | cumulative B | marginal B | depth | full restore | single | 1-row | strategies |
|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | 173,235,935 | 173,235,935 | 0 | 683.3 ms | 203.9 ms | 112.7 ms | zstd 76 |
| 1 | 173,308,819 | 72,884 | 1 | 1,338.2 | 472.2 | 360.6 | sparse 26, zstd 50 |
| 2 | 190,613,356 | 17,304,537 | 2 | 2,091.5 | 735.5 | 635.4 | sparse 14, weight 12, zstd 50 |
| 4 | 224,046,831 | 33,433,475 | 3 | 3,315.8 | 1,212.9 | 1,103.1 | weight 26, zstd 50 |
| 8 | 273,269,777 | 49,222,946 | 4 | 4,527.1 | 1,659.1 | 1,563.0 | weight 26, zstd 50 |
| 16 | 334,456,297 | 61,186,520 | 5 | 5,812.6 | 2,168.8 | 2,057.3 | weight 32, zstd 44 |
| 64 | 426,381,372 | 91,925,075 | 6 | 7,187.6 | 2,649.8 | 2,498.1 | weight 37, zstd 39 |
| 512 | 542,994,594 | 116,613,222 | 7 | 8,267.9 | 3,062.7 | 3,013.5 | weight 32, zstd 44 |
| 1000 | 660,220,076 | 117,225,482 | 8 | 9,520.8 | 3,532.7 | 3,470.8 | weight 36, zstd 40 |

**First → last: 173.2 MB → 660.2 MB for nine checkpoints.** Nine independent
`zstd -19` checkpoints sum to 1,299,182,221 B, so the trajectory is **49.2 % smaller**.
All 9/9 restore exactly (684 tensor SHA-256 checks, 9 file SHA-256 checks).

Baselines per step (whole-file, external `zstd`, all ran at `-19`):

| step | `final + Σ zstd19(XOR delta)` | `first full + Σ zstd19(--patch-from)` | ours |
|---:|---:|---:|---:|
| 0 | 144,397,747 | 144,397,747 | 173,235,935 |
| 1 | 144,414,388 | 144,673,407 | 173,308,819 |
| 2 | 151,541,067 | 212,360,102 | 190,613,356 |
| 4 | 172,018,136 | 323,889,182 | 224,046,831 |
| 8 | 216,478,584 | 465,118,163 | 273,269,777 |
| 16 | 279,351,633 | 606,535,826 | 334,456,297 |
| 64 | 384,887,896 | 748,074,410 | 426,381,372 |
| 512 | 527,393,066 | 889,759,026 | 542,994,594 |
| 1000 | 672,210,950 | 1,031,422,235 | 660,220,076 |

(`final + Σ` = this step's per-tensor `zstd -19` total plus every
byte-wise-XOR delta compressed at `-19` up to this step; `first + Σ` = step 0's
per-tensor `zstd -19` plus every whole-file `--patch-from` up to this step.
The harness records the per-step marginal; the sums here are re-derived from the
receipt's per-step fields.)

At the last step our cumulative is 1.8 % below `final + Σ XOR deltas` and
36.0 % below `first + Σ patch-from`. **The engine wins on this series only
narrowly against the XOR baseline, and loses on none of the three baselines at
the final step** — but early steps lose: at step 0 a single `zstd -19`
checkpoint (144.4 MB) is smaller than our stored first checkpoint (173.2 MB),
and the cross-over is after step 2. The `final + Σ` baseline is anchored at the
current final checkpoint, not at a chain of independent checkpoints, so the
comparison is only exact at the last row.

## Qwen2.5-0.5B base → Instruct, real public weights

| step | cumulative B | marginal B | depth | full restore |
|---:|---:|---:|---:|---:|
| base | 658,187,354 | 658,187,354 | 0 | 3,956.8 ms |
| instruct | 1,199,651,657 | 541,464,303 | 1 | 10,082.7 ms |

Mix: base `float-tensor 169, zstd 73, raw 48`; instruct marginal
`weight-delta 241, zstd 1, raw 48`. Both restore exactly (580 tensor checks,
2 file checks). This reproduces the R1 family total (1,199,958,505 B data-only
vs 1,199,651,657 B here; the difference is manifest/index bookkeeping).

Baselines at the last step: independent `zstd -19` 1,535,945,154 (ours 21.9 %
smaller); `final + Σ XOR` 1,384,249,213 (ours 13.3 % smaller); whole-file
`zstd --patch-from` **unavailable** — `-19` was OOM-killed and the `-12` retry
also failed with `zstd failed: long mode automatically triggered` (the exact
stderr is in the receipt). No patch number is claimed.

## Declared synthetic 50-step series (not real weights)

64 tensors of shape `[256,256]` BF16, 8,394,680 B per step; rule
`(i + 131·t) mod 4093 == step mod 4093 → BF16 word + 0x0100` (≈0.024 % of
elements per step). Labelled synthetic everywhere.

* step 1: 4,652,647 B (`zstd 64`); step 50: **34,894,788 B**
  (`sparse-delta 64`, depth 4 after the most recent reset).
* 50 independent `zstd -19` checkpoints would be ≈231.8 MB, so the curve is
  ≈6.6× smaller.
* Every step restores exactly: 3,200 tensor checks + 50 file checks.
* **Restore grows with depth**: full 23.4 ms (depth 0) → 220.9 ms (depth 8);
  single 0.6 → 4.1 ms; 1-row 0.44 → 4.1 ms.
* **The dependency-depth bound costs a full checkpoint every 9th step**: at
  depth 8 the planner refuses another delta and writes an independent
  representation, so the marginal jumps from ≈0.1–0.17 MB to ≈4.7 MB (steps
  10, 19, 28, 37, 46). Five resets are 23.5 MB of the 34.9 MB total.
* **Loss against the XOR baseline**: `final + Σ zstd19(XOR)` for this series is
  4,882,259 B (every XOR delta is ≈5 KB because only a few dozen words move), far
  below our 34.9 MB. Per-tensor sparse-delta framing (~1.6 KB per tensor per
  step, ~100 KB per step) dominates when the true change is a few dozen words.
  This is an honest loss for mode 16 on extremely sparse deltas; it is not
  hidden by choosing a different baseline.

## GC / retention

Two synthetic stores, one with a snapshot pinned after every step, one without.

| store | tree before GC | after GC | blobs deleted | final restore |
|---|---:|---:|---:|---|
| 50 snapshots, 49 checkpoints unlinked | 75,407,219 | 75,407,219 | **0** | exact |
| no snapshots, 49 checkpoints unlinked | 74,216,970 | 74,066,374 | 142 | exact |

With snapshots, the unlinked checkpoints are still reachable and nothing is
reclaimed. Without snapshots, deleting 49 of 50 checkpoints reclaims only
**150,596 B**: the final checkpoint's delta chain reaches back through the
earlier steps, so almost every blob is still a dependency. That is the
storage-shape claim made concrete — the trajectory is retained as *final plus
its dependency closure*, not as 49 independent copies.

`GcReport::deleted_bytes` reported 3,467,041,895 for 142 deleted blobs while the
measured tree changed by 150,596 B. The field is inconsistent with the tree
and is not used as a byte claim here; the table uses the measured
`objects/`+`manifests/` sizes. (This is a pre-existing diagnostic field, not
introduced by this subphase.)

## Losses and non-claims

* Pythia deltas are large (steps 512→1000 cost 117.2 MB) because the checkpoints
  are far apart in training; the curve is not "free".
* The chain-depth bound (8) forces periodic full checkpoints; a longer
  trajectory pays that reset repeatedly.
* Restore time grows with depth: 14× for a 9-checkpoint pythia chain
  (0.68 → 9.52 s). No parallel or cached restore is measured here.
* The XOR-delta baseline beats mode 16 on the synthetic sparse series.
* One host, warm caches, one public model family plus a synthetic series; no
  denser pythia series beyond step1000 was fetched (the four extra points are
  the densification; `step1000` is the last fetched).
* `--no-semantic` was required: the mode-14 candidate search decoded several
  live tensors and OOM-killed a pythia step at the 3 GiB court bound before the
  bound was added (the bound now skips the candidate when
  `tensor_bytes × max_candidates > 512 MiB`).
* Baselines are external `zstd` CLI runs on the same files, per step; the
  XOR-delta baseline is a byte-wise XOR of consecutive tensor payloads, not a
  semantically-aware model delta.
