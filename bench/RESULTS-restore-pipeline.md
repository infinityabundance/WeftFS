# Pipelined restore (Qwen2.5-0.5B, single host)

Receipt: `bench/receipts/restore-pipeline-20261106T204445Z.json`
Harness: `bench/scripts/restore_pipeline_court.sh`,
`bench/scripts/assemble_restore_pipeline.py`, `examples/real_model_bench.rs`
(`restore --materialize-pipeline`, `--checkpoint`)
Court: `weft/bench-court:1`, image id
`sha256:4041d66ed25626e875de58f79e6527997c85ca81541566cbabe09451b40f2041`,
kernel `7.3.1-1-cachyos`, 3 GiB, 4 vCPU, 1024 pids, no swap.
Revisions: WeftFS `eb72c72`, WeftCodec `a888d79`, WeftCodec-Go `8957998`,
WeftFS-Go `007bf11`.

Label: single host, warm page cache, one cached store reused by both modes,
three interleaved repetitions per configuration. No fleet, multi-node, or
cold-cache claim. The mode-15/9/10/11/14 wire layouts are untouched: this
receipt measures a restore scheduler.

## What was added

`src/restore_pipeline.rs`:

* `run_ordered_pipeline(inputs, sizes, options, work, measure, consume)` — an
  ordered producer/consumer runner. Workers decode with a bounded count and a
  bounded declared-byte reservation; one writer (the calling thread) calls
  `consume` for indices `0, 1, 2, ...` in exact source order. `consume` runs
  outside the queue lock, so decoding continues while a tensor is written.
* `RestorePipelineOptions { threads, max_inflight_bytes }` and
  `RestorePipelineStats` (tasks, tensors/bytes written and dropped, observed
  in-flight bytes/tasks, oversized tasks, `decode_wall_ms`, `decode_sum_ms`,
  `transfer_ms`, `write_ms`, `total_ms`).
* `Store::restore_tensors_pipelined` and
  `Store::restore_tensors_pipelined_authorized`, which bind the runner to the
  shipped `read_tensor` / `read_tensor_authorized` path (so R8 cache and
  single-flight still apply where they apply today).

Bounds and policy:

* **Workers.** `threads = 0` means one; the effective count is capped at
  `available_parallelism()`. Reservations are granted **in input order**, which
  guarantees that the index the writer is waiting for can always reserve.
* **Bytes.** Non-oversized reservations (decoding + published-but-unwritten +
  the tensor being written) are bounded by `max_inflight_bytes`; the default is
  the existing 64 MiB. A tensor whose declared size alone exceeds the bound is
  tracked separately and at most one is outstanding, so the peak is at most
  `max_inflight_bytes + largest tensor`. The reservation is the declared
  logical size, exactly as in `read_tensors_bounded`.
* **Order and error.** The writer consumes strictly in order, so the first
  failure it sees *is* the lowest failing input index. It then stops claiming
  new work, drains every in-flight decode, frees every reservation, joins every
  worker, and returns that error. The sink therefore holds exactly the prefix
  before the failing index. A sink (`write_all`) failure has the same shape.
* **Read-only.** The pipeline only calls the shipped tensor read path. The sink
  is the caller's file or `Write`, never a store namespace. `tests/restore_pipeline.rs`
  asserts no store mutation (head, generation, full tree digest, `verify --full`).

The harness writes the exact safetensors header first, then calls
`restore_tensors_pipelined_authorized(&tokens, &mut file, ...)`; the bytes are
the same as the batched path, only the schedule differs.

`real_model_bench restore --materialize-pipeline --threads N` selects it. The
harness also gained `--checkpoint base|instruct` for scoping a bench run; the
library and CLI always restore exactly what is asked for.

## Store read path: import is now opt-in

`weftfs checkpoint restore` used to reopen the store read-write and publish the
restored container under `--out` (the DCP/Orbax readers then grew the store by
`/dcp-reads/<uuid>` copies). Restore is now read-only in the store by default:

* no `--import`: only the store is read; the exact container goes to
  `--host-out`/`--spool`; `--out`/`--out-name` are rejected.
* `--import --out STORE_DIR`: the restored container is additionally published
  with `create_file_streaming` (bounded memory), as before.

The Python DCP/Orbax adapters gained `import_restored` (default `False`) and
pass `--import` only when asked. `docs/PYTORCH-DCP.md` and
`docs/checkpoint-topology.md` document the difference.

## Corpus and court destinations

Qwen/Qwen2.5-0.5B and Qwen2.5-0.5B-Instruct, BF16, 290 tensors, 988,065,536
tensor bytes and 988,097,824 file bytes per checkpoint, largest tensor
272,269,312 B. Both source files were re-hashed in the court against
`bench/corpora/real-models/qwen2.5-0.5b-pair.json` (`88c14255...0ed342`,
`fdf756fa...88fb7fe`). The cached planner-float store was reused; ingest was
skipped (recorded as `ingest_source: reused-cached-planner-float`).

Two destinations are measured because the host bind mount is much noisier than
the decoder:

| destination | checkpoints | mount | why |
| --- | --- | --- | --- |
| `tmpfs` | base | court tmpfs, 1 GiB | No writeback; isolates decode/transfer overlap. 988,097,824 B leaves ~80 MiB under the mount. |
| `disk` | base + instruct | host bind mount (`.docker-cache`) | The real end-to-end path; its write time is host-state dependent. |

Every run writes to a fresh directory, is SHA-256 verified, then the output is
removed and `sync` is run before the next timed run, so the court does not
accumulate 2 GB of dirty output per configuration.

## Results — tmpfs (controlled, base checkpoint)

Medians of three repetitions. `read` is the unchanged first pass that hashes
every tensor; `batch` is `materialize_ms` of the shipped
`read_tensors_bounded_authorized` reassembly; `pipeline` is the new
`total_ms`. `decode` is the wall time to the last decode, `transfer` is writer
wait for the next in-order result, `write` is time inside `write_all`.

| threads | read ms | batch ms | pipeline total ms | decode wall ms | transfer ms | write ms | pipeline vs batch | peak RSS batch/pipeline KiB |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 4,835.1 | 4,913.0 | **4,760.1** | 4,661.0 | 0.0 | 96.7 | 1.032x | 727,488 / 726,936 |
| 2 | 3,102.5 | 3,140.9 | **2,844.0** | 2,842.6 | 2,713.1 | 134.3 | 1.104x | 857,100 / 864,040 |
| 4 | 2,316.6 | 2,354.2 | **2,157.3** | 2,155.9 | 2,005.9 | 151.0 | 1.091x | 853,040 / 897,776 |

Pipeline scaling against its own one-worker run: **1.674x** at 2 threads and
**2.207x** at 4. Batch materialize scaling for the same runs is 1.564x and
2.087x. `write_ms / decode_wall_ms` is 0.021, 0.047, and 0.070: the write is
almost entirely overlapped, and `total ≈ decode` at 2 and 4 workers.

Bytes written are 988,065,536 per run (290 tensors); the restored file is
988,097,824 B with the source SHA-256 in every run.

## Results — host bind mount (real path, both checkpoints)

| threads | checkpoint | read ms | batch ms | pipeline total ms | decode wall ms | write ms | pipeline vs batch | RSS batch/pipeline KiB |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | base | 4,771.6 | 4,872.2 | 5,684.9 | 4,592.7 | 1,099.3 | 0.857x | 2,033,976 / 2,037,764 |
| 1 | instruct | 10,970.1 | 11,045.0 | 13,070.7 | 10,770.9 | 2,277.9 | 0.845x | same |
| 2 | base | 3,089.7 | 3,174.5 | 3,802.5 | 3,471.9 | 1,548.4 | 0.835x | 2,132,300 / 2,171,208 |
| 2 | instruct | 7,081.6 | 7,475.9 | 7,014.5 | 6,836.5 | 2,667.8 | 1.066x | same |
| 4 | base | 2,303.8 | 3,336.9 | 4,495.7 | 3,711.4 | 2,497.5 | 0.742x | 2,243,036 / 2,244,636 |
| 4 | instruct | 5,281.0 | 6,013.3 | 5,108.4 | 5,107.1 | 489.2 | 1.177x | same |

**These disk rows are not a performance claim.** Across the three repetitions
the same batched 988 MB materialize ranged **2,526.4–15,325.7 ms** and the
pipeline total ranged **2,976.0–17,519.3 ms**; the pipeline's own write ranged
**237.2–11,967.0 ms**. That host writeback spread is larger than every mode
difference, in both directions (the pipeline wins 1.18x at 4 threads on the
instruct checkpoint and loses 1.35x at 4 threads on base in the same court).
The `tmpfs` rows are the fair comparison; the disk rows are retained in full
precisely so the variance is visible.

The original R4 observation (`materialize` at 4 threads regressed to 0.43x) is
**not reproduced as a code property** here: on tmpfs the batched path itself
scales 2.09x, because these runs write to a fresh file and the decoder, not
writeback, is the limit. What the pipeline removes is the *serial* write after
decode, which is worth 3.2–10.4 % on tmpfs and is exactly what the disk cannot
measure.

## Exactness

Every run of both modes and both destinations:

* per-tensor BLAKE3 equals the ingest record (290/290);
* every `LogicalTensorRoot` recomputes to the recorded value;
* the reassembled file is byte-identical and its SHA-256 equals the source
  (`88c14255...0ed342` base, `fdf756fa...88fb7fe` instruct) in every thread
  count, mode, and destination;
* restored tensor bytes are identical across modes and thread counts;
* pipeline `tensors_written = 290` and `bytes_written = 988,065,536`;
* pipeline `observed_max_inflight_bytes <= 67,108,864 + 272,269,312` (bound plus
  the one oversized embedding).

The receipt's `checks` object is 20/20 true.

## Losses and limits

* **The disk end-to-end claim cannot be made on this host.** Host writeback
  variance (2.4–16.3 s for the same 988 MB write) swamps every mode difference.
  The pipeline does not control disk bandwidth and is not claimed to.
* **One worker is barely better on tmpfs (3.2 %)** and loses on the noisy disk.
  The pipeline's win comes from overlap, so it needs at least two workers to
  matter.
* **The 272 MB embedding is still serial.** It exceeds the 64 MiB staging bound,
  so it runs as the one oversized task; per-tensor decode concurrency is capped
  by that tensor. The pipeline does not split a tensor.
* **Full-pair latency is unchanged at one worker** on a fast sink; only the
  write overlap is removed.
* **`total_ms` is decode-bound on tmpfs** (2,157 ms at 4 workers), matching
  `read_tensors_bounded`'s decode wall; the remaining gap is the embedding.
* **Restore is memory-hungry on the host mount** (~2.0–2.2 GB peak RSS for the
  full pair, both modes) because the harness restores both checkpoints in one
  process; a base-only tmpfs run peaked at 0.73–0.89 GB.
* **No Go parity** for the ordered pipeline; WeftFS-Go still has no
  checkpoint materialize path. The store format and reads are unchanged.
* **Single host, warm cache, one store, three repetitions.** No cold-cache,
  fleet, GPU, or multi-node claim.
* The pipeline's reservation is the *declared* logical size; a decoder whose
  transient allocator use exceeds it is not bounded by that number (same caveat
  as `read_tensors_bounded`).

## Reproduce

```sh
scripts/docker-court.sh bench "cd /work && bash bench/scripts/restore_pipeline_court.sh"
```

`bench/receipts/restore-pipeline-20261106T204445Z.json` is the committed
receipt at WeftFS `eb72c72`. The script re-hashes the cached Qwen files, reuses
`.docker-cache/planner-float/{store,ingest.json}`, runs
`(t1,t2,t4) x (batch,pipeline) x 3 reps` for each destination, verifies every
output, removes it, and assembles the receipt. `WEFT_RESTORE_PIPELINE_REPS` and
`WEFT_RESTORE_PIPELINE_MIB` override the defaults (3 and 64) without changing
any court limit.
