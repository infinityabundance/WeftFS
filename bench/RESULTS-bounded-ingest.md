# S12 — bounded / streaming ingest (2026-11-08)

Goal: ingest the real Qwen2.5-3B (BF16) family inside the program's default
3 GiB bench court, with the peak RSS recorded per phase and every tensor's
sha256 / logical root verified.

This is a **memory-only** change. No wire format, frame byte, manifest field,
pricing rule, candidate order, tie-break or selected strategy changed. The
streaming digests are byte-identical to the v1 one-shot functions.

## Corpus (re-verified in the court)

| file | bytes | sha256 |
|---|---:|---|
| `Qwen2.5-3B.safetensors` | 6,171,927,112 | `52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab` |
| `Qwen2.5-3B-Instruct.safetensors` | 6,171,927,112 | `54917aba5570e50db3f135b6e268a2b67bf951d06610713e5b6e69b8b20d91d2` |

434 BF16 tensors per checkpoint; the largest is `model.embed_tokens.weight`
at 622,329,856 bytes (593.5 MiB).

## Court

`scripts/docker-court.sh bench` with `--memory=3g --memory-swap=3g
--pids-limit=1024 --cpus=4 --tmpfs /tmp:size=1g --cap-drop=ALL
--security-opt no-new-privileges`, image `weft/bench-court:1`
`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel 7.3.3-1-cachyos.

Command (reduced configuration: one family, `default` config only, one rep per
phase):

```
scripts/docker-court.sh bench "cd /work && \
  WEFT_FAMILY_FAMILIES=qwen3b-finetune WEFT_FAMILY_CONFIGS=default \
  WEFT_FAMILY_INGEST_REPS=1 WEFT_FAMILY_MEASURE_REPS=1 \
  WEFT_FAMILY_TOPOLOGY_REPS=1 WEFT_FAMILY_RECOVERY_REPS=1 \
  WEFT_FAMILY_S3=0 bash bench/scripts/checkpoint_family_court.sh"
```

Receipt: `bench/receipts/family-lifecycle-20261108T143027Z.json` (raw log
beside it). Summary: `bench/receipts/s12-bounded-ingest-20261108T143027Z.json`.

## Peak RSS per phase (KiB, `VmHWM` of a fresh process)

| phase | peak RSS | note |
|---|---:|---|
| ingest | **2,765,636 KiB** | base + Instruct whole-tensor ingest (the peak) |
| topology | 1,863,172 KiB | 8/4/row-major/blocked restores |
| measure | 1,840,524 KiB | 5 shapes x 1/4 threads |
| baselines | 1,228,240 KiB | per-tensor `zstd -19`; whole-file `--patch-from -12` exited 42 |
| gc without snapshot | 1,089,068 KiB | small 8-tensor fixture |
| gc with snapshot | 1,089,536 KiB | small 8-tensor fixture |
| checkpoint (8-way) | **614,920 KiB** | streaming `checkpoint write` |
| recovery | 250,388 KiB | 3 process-fault phases |

All phases completed inside the 3 GiB limit (3,145,728 KiB); no OOM kill, no
limit was raised.

## 3B results

Ingest wall 976,527.164 ms; `verify --full` ok: 870 objects, 873 manifests,
731 representations, 139 chunks, 0 problems.

Strategies: base 270 float-tensor / 92 zstd / 72 raw; Instruct 255
weight-delta / 113 sparse-delta / 1 float-tensor / 65 raw. The one
float-tensor on the Instruct side is the 593.5 MiB embedding (see the loss
below).

Store retention: data-only 6,775,861,261 B, indexes 169,652,106 B (2.442 %),
total 6,946,533,343 B.

Checkpoint (8-way, mode-10 transport): 434 tensors, 3,472 shard objects,
187,089.716 ms, 6,171,877,376 element bytes, `verify` ok. Cross-topology
restores 434/434 roots exact for sharded 8, sharded 4, row-major and a blocked
largest-tensor restore. Full-family restore 57,551.7 ms at 1 thread /
50,759.94 ms at 4 threads (6,361,877,101 backend bytes, sha256 match). GC with
no snapshot: 1,185,383,670 -> 592,708,039 data bytes (verify ok); with the
snapshot pinned it deletes nothing (1,185,383,670 unchanged).

## 0.5B before/after (same command, same court)

| build | result |
|---|---|
| pre-change tree `d5e78ce` / `3d9ae04` | **OOM-killed, exit 137** at 3 GiB |
| this tree `9822d56` / `cffcc1a` | **2,257,928 KiB** peak, `verify` ok, 582 objects |

The 580 (base+next) per-tensor rows of the post-retention run and the final
run are identical in `logical_root`, `strategy`, `data_bytes` and source
`sha256` (0 differing rows), and the strategy distribution is unchanged
(169 float-tensor / 73 zstd / 48 raw; 241 weight-delta / 1 zstd / 48 raw).

## Byte identity

`tests/checkpoint_streaming.rs::streaming_ingest_publishes_the_one_shot_identity`
writes the same container twice with the same `deterministic_seed`, once with
`max_ingest_bytes` equal to the largest tensor length and once at 1 GiB, and
requires the same `head`, `manifest_id`, `manifest_root` and canonical
manifest bytes. It also recomputes `identity::content_hash` and
`weftcodec::tensor::logical_root` over the whole tensor and compares them to
the manifest.

`streaming_shard_reads_match_shard_bytes_and_manifest_roots` compares
`read_tensor_shard` to `weftcodec::tensor::shard_bytes` and the shard manifest
roots; `src/safetensors.rs` tests compare the streamed content/values digests
to the one-shot functions at window sizes 1, 3, 64, 4096 and 8 MiB.

## Losses and limits

- The 593.5 MiB embedding is above
  `TensorPlanOptions::max_delta_bytes` (default 512 MiB), so the weight-delta,
  sparse-delta and zstd-patch candidates are **not offered** for it. On this
  corpus the skipped candidates were not the cheapest: the tensor is stored as
  float-tensor (413,019,145 B vs raw 622,329,856 B).
- The peak is still an encoder transient: mode-9 (float-tensor) encoding of a
  622 MB tensor defines the 2,765,636 KiB ingest peak.
- The zstd-19 CPU gate (20 s) skips the embedding (estimated 20.8 s), so the
  level-19 baseline is not a candidate there.
- The full court was run in the reduced configuration only; `region1` and
  `region100` were not re-run for 3B.
- The prepare/commit split still materialises one whole tensor per worker (it
  re-verifies the reassembled tensor); `max_ingest_bytes` is enforced there
  but the memory profile is unchanged.
- Rust only: no Go parity for the streaming codec API, the ingest bound or the
  delta bound. No physical power-loss / media claim. Single host, warm cache.
- `verify-all` was not re-run for S12; the evidence is the two suites above
  plus this court.
