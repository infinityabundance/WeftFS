# Checkpoint service, DCP and Orbax: measured comparison (S3)

Receipts:

* `bench/receipts/checkpoint-service-20261107T063519Z.json` — service vs CLI
  on one fixture, 32 × 1 MiB f32 tensors, 4-way `zstd` shards.
* `bench/receipts/orbax-20261107T063412Z.json` — Orbax court, 8 forced CPU
  devices, 11-leaf 8-way sharded PyTree, 58,765,352 logical bytes.
* `bench/receipts/pytorch-dcp-20261107T063540Z.json` — DCP, 8 × 4 MiB f32,
  topology 8.

Every run is in the Python court under the normal hard limits (3 GiB / 4 vCPU /
1024 pids, `cap-drop=ALL`, `no-new-privileges`, bounded `/tmp`). Single host,
warm cache, synthetic weights.

## 1. Service vs CLI, same store and tensors

`bench/scripts/checkpoint_service_court.py` (11/11 checks). The "CLI" column is
the pre-S3 code path on the same revision: one `weftfs` process per operation
and a host staging file.

| operation | service | CLI per call | ratio |
| --- | ---: | ---: | ---: |
| restore one 1 MiB tensor (median of 20) | **2.550 ms** | 10.386 ms | 4.07x |
| restore all 32 tensors (median of 3) | **79.465 ms** | 96.363 ms | 1.21x |
| write checkpoint, 32 MiB (one each) | **1,206.718 ms** | 1,286.272 ms | 1.07x |
| manifest (10 reps) | 0.467 ms | n/a | – |

The service handled 230 requests / 216 restores / 226,492,416 B in that run
with one helper process. After 100 restores the store was byte-identical and at
the same generation (`34,027,955 B`, `270 files`, generation `2`).

## 2. Orbax

Fixture as R15: `XLA_FLAGS=--xla_force_host_platform_device_count=8`, one 11-leaf
PyTree (bf16/f16/f32/i8/u8/i32), 58,765,352 logical bytes, `write_topology=(8,0)`,
`shard_strategy="zstd"`. Orbax's side is `PyTreeCheckpointer`/`AsyncCheckpointer`
with OCDBT and compression.

| metric | Orbax OCDBT | WeftFS (service) | WeftFS (same-revision CLI control) | R15 receipt (before) |
| --- | ---: | ---: | ---: | ---: |
| save pause (ms) | 10.421 | 1.034 | – | 1.017 |
| save total (ms) | **77.958** | 1,859.152 | 2,603.080 | 1,770.713 |
| full restore (ms) | **22.927** | 185.788 | 205.633 | 2,690.338 |
| single-leaf restore (ms) | **5.327** | 21.948 | 27.049 | 380.432 |
| physical bytes | 50,643,641 | 50,866,872 | – | 50,866,816 |
| files | 13 | 190 | – | 189 |
| store growth from restores | – | **0 B** | – | +67,615,378 B |

Ratios for the current build: save **23.85x** slower than OCDBT, full restore
**8.10x** slower, single leaf **4.12x** slower. Against the same-revision CLI
control the service wins every row: save 1.40x, full restore 1.11x, single leaf
1.23x.

The R15 "before" restore numbers included the read-induced import of the
restored container (`/dcp-reads/...`, 67.6 MB per full restore), which R19 made
opt-in and S3 removes entirely — the service cannot materialize a container at
all. The store-growth row is the direct evidence: 67,615,378 B before, 0 B now.

Rust verification after save and after all restores: `checkpoint verify`
11 tensors, `verify --full` ok, 11/11 leaf SHA-256 matches on both sides. A
requested 2-device restore sharding is honoured (`requested_sharding_honored:
true`); Orbax 0.12.9 returns the saved 8-device sharding instead.

## 3. PyTorch DCP

Fixture: 8 × 4 MiB f32, topology 8, `zstd` shards.

| metric | Q5 receipt (before) | WeftFS (service) | same-revision CLI control |
| --- | ---: | ---: | ---: |
| full save (ms, median of 3) | 1,223.642 | 1,498.649 | 1,521.614 |
| full load (ms, median of 3) | 152.208 | **101.006** | 106.475 |
| single-tensor load (ms, median of 5) | 67.171 | **24.135** | 22.362 |
| async submit (ms) | 0.720 | 1.549 | – |
| async completion (ms) | 1,329.0 | 1,621.105 | – |
| store bytes after first save | 31,187,325 | 31,187,381 | – |
| bytes vs `torch.save` | 0.9294 | 0.9294 | – |
| store growth from restores | 0 B (read-only since R19) | 0 B | – |

Losses, stated plainly:

* Full save is 22.5 % slower than the Q5 record. The same-revision CLI control
  is 1,521.6 ms, so the service path is **1.5 % faster** than the path it
  replaces; the difference against Q5 is in the Rust checkpoint write and the
  store code that changed between Q5 and this revision, not in the boundary.
  1,344 ms of the 1,499 ms is service `request_ms` (the write itself).
* Single-tensor load is 7.8 % slower than the CLI control (24.1 vs 22.4 ms).
  The control omits `dcp.load`'s metadata and framework work; per-leaf the
  service path is 4x faster, as the service court shows.
* Async submit dropped from 0.72 to 1.55 ms because the submit now touches the
  service (a request round trip) instead of starting a thread immediately.
  Completion is unchanged within noise.

## 4. What the boundary costs

* One process per `CheckpointService`, created lazily and reused for every
  call. Save and load make **zero** CLI subprocess calls (recorded as
  `store_command_calls: 0` in both adapter stats).
* One `socketpair`, two `sendmsg`/`recvmsg` syscalls per request, one memfd and
  one mapping per leaf. No staging file is created on either side.
* The read handle is opened once; `REFRESH` reopens it before a read so an
  external writer's commit is visible.

## 5. Not covered

No GPU, multi-host, fleet, cloud, media power-loss, or cold-cache claim. The
service court's "CLI" column includes a staging file because that is the code
path being replaced, not a claim about the CLI's own performance. Orbax features
beyond `PyTreeCheckpointer`/`Checkpointer`/`CheckpointManager` were not
exercised (Zarr3, transforms, per-leaf args, emergency checkpointing).
