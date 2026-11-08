# Parent-graph planning on a real pythia-70m trajectory (S13)

Receipt: `bench/receipts/parent-graph-20261108T192017Z.json`
(schema `weft.s13.parent-graph.v1`), measured at WeftFS `163a6a2` /
WeftCodec `cffcc1a` in the default bench court (3 GiB, 4 cpus, 1024 pids,
`weft/bench-court:1` id `sha256:4c6c41fb…`).

Reproduce:

```sh
scripts/docker-court.sh bench \
  "cd /work && bash bench/scripts/parent_graph_court.sh"
```

Corpus (re-hashed inside the court; 76 tensors each, 281,715,176 B each):

| step | bytes | sha256 |
|---|---:|---|
| 0 | 281,715,176 | `17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e` |
| 1 | 281,715,176 | `652e38fd15e118cc7c43d0aeb5e30675c7180cf1d8782cb401ba5d0f933fa109` |
| 2 | 281,715,176 | `491b4bafde38f7b58871b997597d5ca8447e683718879f846501b20a719127b2` |
| 4 | 281,715,176 | `cf8a0b16691ab56ed52e5c6ace2c14f99637e7de755eba9ee3b113417d996223` |
| 8 | 281,715,176 | `17254dbf6e86d12b32a5da7b7fa5d51df3e5d3a12907b3b1ea1442ef51870e48` |
| 16 | 281,715,176 | `c93333b4a7b6ca6337b5616167aac28a025c1a9b28763e240c904fc504a3ad09` |
| 64 | 281,715,176 | `d6091e017aeea5319a2209bf95c71ecfc44a71302eb2553135bce3a834ad4616` |
| 512 | 281,715,176 | `a46bd4c254e28664926b12762616596de5783608ca5403765586c126bdf5bcd4` |
| 1000 | 281,715,176 | `09883f904c4a50de85c682cdfa0434aaf34e62392fe883e58410d12d39b1ee88` |

Every container has 76 tensors and 281,706,496 element bytes; the 8,680-byte
difference from the file size is the safetensors header.

The receipt is the authority for the full list; the table reproduces it.

## What was measured

1. Every version was written **independently** into a private probe store
   (1,447,239,539 B actual, 163,913,246 B for step 0 growing to 1,447,239,539 B
   for all nine).
2. Every earlier-version parent was priced with the shipped
   `Store::plan_tensor` (45 version-prices; 1,388.8 s in the court). The graph
   is in the receipt (`graph.versions[i].parents[j]`).
3. The chain/restart DP, the exhaustive oracle (`N = 9 <= 12`) and the greedy
   pass ran on that measured matrix.
4. Five plans were realised in separate stores; every version was restored
   three times from a freshly opened read-only handle, compared with the source
   container, and the store was `verify --full`-ed.

`lambda_ns` defaults to the smallest integer at which any restart interval
beats the chain on the measured matrix. The break-even values are
0.008408 (k=2), 0.005266 (k=3), 0.004714 (k=4) bytes/ns, so the court ran at
`lambda_ns = 1` (1 byte per ns = 1 GB/s). The SLA was 4 s.

## Plan comparison (model prices from the measured graph)

| plan | parents | depth | edge bytes | Σ chain decode | worst restore |
|---|---|---:|---:|---:|---:|
| current chain | `−1,0,1,2,3,4,5,6,7` | 8 | 647,175,737 | 53,146.7 ms | 11,599.9 ms |
| restart every 2 | `−1,0,−1,2,−1,4,−1,6,−1` | 1 | 1,030,180,485 | 7,594.5 ms | 1,634.8 ms |
| restart every 3 | `−1,0,1,−1,3,4,−1,6,7` | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| restart every 4 | `−1,0,1,2,−1,4,5,6,−1` | 3 | 808,225,589 | 18,983.3 ms | 4,482.4 ms |
| **DP @ λ=1, SLA 4 s** | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |
| DP @ λ=0, SLA 4 s | = restart every 3 | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| greedy @ λ=1 | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |
| brute force @ λ=1 | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |

The DP and the exhaustive oracle agree exactly (`dp_matches_brute_force:
true`). The greedy happens to agree at this λ; it is not claimed optimal.

At λ=0 **without** an SLA the objective is bytes only, and the chain is the
smallest plan in the measured table (647,175,737 B against 808,225,589 B for
the next smallest), so that is what the DP returns. The 4 s SLA is what makes
the DP restart: the chain's model worst case is 11.6 s. This is the honest
answer to "why does any of this exist" — the planner chooses the trade; it
does not impose one. With no latency objective and no SLA, the old behaviour
is still the optimum.

## Realised stores (actual bytes, median-of-3 restore)

| plan | data bytes | predicted | error | worst restore | Σ restore | exact | verify |
|---|---:|---:|---:|---:|---:|---|---|
| chain | 647,411,616 | 647,175,737 | +235,879 (+0.036 %) | 9,336.3 ms | 41,218.8 ms | 9/9 | ok |
| restart-2 | 1,030,285,678 | 1,030,180,485 | +105,193 (+0.010 %) | 2,448.9 ms | 12,583.3 ms | 9/9 | ok |
| restart-3 | 849,712,592 | 849,572,567 | +140,025 (+0.016 %) | 3,248.2 ms | 16,289.9 ms | 9/9 | ok |
| restart-4 | 808,373,400 | 808,225,589 | +147,811 (+0.018 %) | 4,650.7 ms | 18,856.6 ms | 9/9 | ok |
| optimizer (λ=1) | 1,447,239,539 | 1,474,646,073 | −27,406,534 (−1.86 %) | 801.5 ms | 7,128.7 ms | 9/9 | ok |

Per-version restore medians (ms; step order 0, 1, 2, 4, 8, 16, 64, 512, 1000):

| plan | 0 | 1 | 2 | 4 | 8 | 16 | 64 | 512 | 1000 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| chain | 790.9 | 1387.8 | 1921.7 | 3096.1 | 4290.8 | 5523.9 | 6826.8 | 8044.5 | 9336.3 |
| restart-2 | 799.7 | 1403.3 | 801.7 | 2346.0 | 802.0 | 2393.0 | 787.1 | 2448.9 | 801.6 |
| restart-3 | 791.3 | 1399.5 | 1971.8 | 798.2 | 2017.8 | 3248.2 | 800.5 | 2023.5 | 3239.0 |
| restart-4 | 784.2 | 1389.2 | 1945.0 | 3141.6 | 797.3 | 2044.1 | 3310.3 | 4650.7 | 794.3 |
| optimizer | 787.0 | 787.9 | 790.8 | 792.9 | 801.5 | 795.1 | 791.8 | 793.1 | 788.6 |

All nine versions in every plan matched the source container byte for byte
(`exact_source_file` and per-tensor SHA-256) and every store passed
`verify --full` with 0 problems.

## Honest verdict

* The chain is the smallest plan (647.4 MB) and the slowest (9.34 s worst,
  41.2 s for all nine). Restart points buy restore time with bytes:
  k=2 is +59.1 % bytes for −73.8 % worst restore; k=3 is +31.3 % for −65.2 %;
  k=4 is +24.9 % for −50.2 %.
* At λ=1 the optimizer goes all the way to independent representations:
  1,447.2 MB (2.24× the chain) but 0.80 s worst and 7.13 s for all nine
  (11.6× / 5.8× faster). That is the correct decision for this objective; it is
  not a recommendation for every deployment.
* The model over-predicts chain restore by 24.2 % (11.60 s vs 9.34 s) and
  under-predicts the all-independent restore by 73.7 % (0.211 s vs 0.802 s).
  The all-independent gap is the fixed per-version container/manifest/SHA-256
  work, which the affine decode table does not model. **The SLA is therefore a
  model SLA, not a measured guarantee.** It accepted the 4 s SLA with a 0.211 s
  prediction; the measured value is 0.802 s (still inside 4 s), but an SLA
  chosen between those two numbers would be accepted and missed. This is a
  published limitation, not a hidden one.
* The all-independent store is 1.86 % smaller than the sum of its per-version
  edge prices because identical chunks are deduplicated across independent
  versions. The chain/restart stores are within +0.04 % of their predictions.
* Break-even reads per version at λ bytes/ns is `λ_be / λ`. At λ=0.001
  (1 MB/s) restart-2 needs 8.4 reads/version, restart-3 5.3, restart-4 4.7; at
  λ=1 (1 GB/s) any read justifies them. For a cold archive read once, the
  chain wins on bytes; for a hot trajectory, independent representations win on
  time. The API's integer λ cannot express values below 1, which is why the
  court publishes the break-even floats and the read-count interpretation
  instead of picking a "realistic" λ silently.
* The pricing pass is expensive: 1,388.8 s for nine versions because every
  edge re-encodes the full candidate set. The planner is a write-time tool, not
  a per-read cost.
* `lambda_ns = 0` with no SLA reproduces the current chain exactly. The change
  is opt-in through options, and no on-disk format changed.

## What is not claimed

Not a Go implementation; not a multi-writer protocol; no physical power-loss
test; one host with a warm OS page cache (the restore is decode-dominated and
labelled); no fleet or cloud claim. The `restart-4` measured worst (4.65 s)
exceeds the 4 s SLA even though its *model* price (4.48 s) does too — the
optimizer never chose it, which is the intended behaviour.
