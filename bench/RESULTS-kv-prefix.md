# KV prefix cache — measured results (T2)

Receipt: `bench/receipts/kv-prefix-20261108T052500Z.json`
Harness: `examples/kv_prefix_bench.rs`
Court: `scripts/docker-court.sh bench` (3 GiB / 3 GiB swap, 4 CPUs, 1024 pids,
1 GiB `/mnt/weft-bench` tmpfs, `no-new-privileges`)
Kernel: `7.3.1-1-cachyos`; bench court image `sha256:4041d66e…`; WeftFS
`c88b199`, WeftFS-Go `b475d2d` (recorded in the receipt together with the
image id and every hard limit).

Everything below is **storage-side**. No inference engine ran. No
time-to-first-token claim is made.

A first court run, `bench/receipts/kv-prefix-20261108T042229Z.json`, produced
the same numbers but predates the receipt's `court`/`revisions` block; it is
retained as history and is not the current receipt.

## 1. Declared workload

| property | value |
|---|---|
| model identity | `synthetic-0.5b` (label only) |
| dtype | bf16 (2 B/element) |
| layers | 14 |
| KV heads | 2 (GQA) |
| head dim | 64 |
| tokens per page | 64 |
| bytes per token (all layers) | 3,584 |
| bytes per page (all layers) | 229,376 |
| page payload | deterministic bit pattern, not captured model state |

The geometry matches Qwen2.5-0.5B's GQA shape; the bytes are synthetic.

## 2. Bytes

| row | tokens | pages/layer | page objects | logical page bytes | stored page bytes | put wall |
|---|---|---|---|---|---|---|
| one prefix | 512 | 8 | 112 | 1,835,008 | 1,835,008 | 55.8 ms |
| child of it | 1,024 | 16 | 224 (+112) | 3,670,016 | 1,835,008 | 55.3 ms |
| child reuse | | | | | **112 pages reused** | |

Physical page-object bytes for the 512-token prefix are 1,931,305 (frames add
~5.2 % over the logical page payload). The child adds exactly 112 page objects
and 1,835,008 logical bytes; its first 8 pages per layer are the parent's
objects, verified byte-identical through `kv_descriptor`.

## 3. 100 prefixes over one system prompt

100 prefixes share a 512-token system prefix and add 64–512 tokens of their own
(1–8 new pages per layer, cycling).

| metric | value |
|---|---|
| prefixes | 100 |
| logical bytes, all prefixes | 286,720,000 |
| stored page bytes, all prefixes | 103,219,200 |
| **dedup ratio (1 − stored/logical)** | **0.64** |
| page objects on disk | 6,412 |
| page-object bytes on disk | 110,569,036 |
| pool build wall | 4.03 s |

The 0.64 figure is the fraction of logical KV bytes that is not stored again.
It is high because 512 of every 576–1,024 tokens are the shared system prompt;
a workload with less shared prefix would converge toward 0.

## 4. Latency

| operation | p50 | p95 | max |
|---|---|---|---|
| authorized `kv get` (126–210 pages, 2.0–3.4 MB) | **23.4 ms** | 35.9 ms | 36.3 ms |
| compatibility scan over 101 prefixes | **10.4 ms** | 10.5 ms | 10.6 ms |

`get` includes per-page capability verification and mode-12 decode. The scan
re-reads and parses every prefix record; it is the dominant term in `T_reuse`
and is a known limitation (no persisted KV index).

## 5. Reuse-vs-recompute distribution

13 declared requests against the pool, declared model
`bench/calibration/kv-prefill-v1.json` (120 µs/token default):

| label | reuse | T_reuse | T_recompute | reason |
|---|---|---|---|---|
| compatible 512 | yes | 13.7 ms | 63.4 ms | prefix found |
| compatible 1,024 | yes | 26.0 ms | 124.9 ms | prefix found |
| compatible 4,096 | yes | 25.9 ms | 493.5 ms | prefix found |
| compatible 8,192 | yes | 25.8 ms | 985.0 ms | prefix found |
| compatible 32,768 | yes | 25.8 ms | 3,934.2 ms | prefix found |
| compatible 131,072 | yes | 25.8 ms | 15,730.6 ms | prefix found |
| model mismatch (3 sizes) | no | 10.3–10.4 ms | 63–3,934 ms | `model mismatch` (listed per candidate) |
| tokenizer mismatch | no | 10.3 ms | 63.4 ms | `tokenizer hash mismatch` |
| attention mismatch | no | 10.4 ms | 63.4 ms | `attention configuration hash mismatch` |
| token-chain mismatch (2,048 tokens) | no | 23.3 ms | 247.8 ms | `token-chain mismatch` |
| shorter than every prefix (64 tokens) | no | 10.5 ms | 9.7 ms | `stored prefix is longer than the request` |

**6 reuse / 7 recompute.** The compatible requests all win because the resolved
prefix is the 512-token system prompt: reuse costs ~26 ms (scan + 1.8 MB
transfer/decode), while recompute is 63 ms even at 512 tokens.

## 6. Losses and limits (published, not hidden)

* **The scan is expensive.** 101 records cost 10.4 ms per lookup. At 1,000
  prefixes this would dominate every decision. A persisted KV index is future
  work.
* **`get` re-verifies the envelope for every page** (existing per-page KV rule),
  so 126–210 pages cost 23.7 ms p50 even warm. That is a real per-page cost of
  the zero-trust KV surface, not a decode cost.
* **Physical bytes exceed logical page bytes by ~5.2 %** (frame headers and
  descriptors).
* **The pool row is a synthetic upper-ish bound**: 512 of 576–1,024 tokens are
  shared. Real chat workloads share less; a 0.64 dedup ratio is not a general
  claim.
* **The decision is only as good as the declared coefficients.** 120 µs/token
  is a declared guess, not measured against a real engine. A real engine could flip every
  "reuse" row.
* **No engine integration.** No vLLM/SGLang plugin, no cache placement, no TTFT
  measurement. The exact validating experiment is named in
  `docs/KV-PREFIX.md` §9.
* One host, one court, warm cache; no fleet, GPU, or power-loss claim.
* Rust-only. There is no Go prefix/chain/decision parity yet.

## 7. Reproduce

```sh
bash scripts/docker-court.sh bench "cd /work && cargo build --release --example kv_prefix_bench \
  && .docker-cache/target-rust/release/examples/kv_prefix_bench --out /work/bench/receipts/kv-prefix-<stamp>.json"
```
