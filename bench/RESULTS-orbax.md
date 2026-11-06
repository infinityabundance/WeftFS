# Orbax / WeftFS comparison (subphase R15)

Receipt: `bench/receipts/orbax-20261106T152612Z.json`
Court: Docker python court (`weft/python-court:1`), kernel 7.3.1, 3 GiB / 4
CPUs / 1024 pids, `--tmpfs /tmp:size=1g`, `cap-drop=ALL`,
`no-new-privileges`. Command:

```sh
bash scripts/docker-court.sh python "bash bench/scripts/orbax_court.sh"
```

The court installs real JAX and Orbax into the court cache, runs the adapter's
18 unit tests in the same court, and then runs the measured comparison. The
image is not modified.

## What was installed

`python -m pip install --disable-pip-version-check --no-cache-dir --upgrade
--target /work/.docker-cache/py-orbax 'jax[cpu]' orbax-checkpoint` succeeded
from PyPI on 2026-11-06:

| package | version |
|---|---|
| jax | 0.12.0.1 |
| jaxlib | 0.12.0.1 |
| orbax-checkpoint | 0.12.9 |
| numpy | 2.5.4 |
| tensorstore | module does not export `__version__`; installed by Orbax |

The install log is `bench/receipts/orbax-20261106T152612Z.install.log`. The court
unit tests are `bench/receipts/orbax-20261106T152612Z.tests.log`: **18 passed**.
The installed versions are `jax 0.12.0.1` and `orbax-checkpoint 0.12.9`. The
harness checked **13/13** conditions and wrote
`status: ok`.

## Workload

A real 8-device CPU mesh was forced with
`XLA_FLAGS=--xla_force_host_platform_device_count=8`. The PyTree is one
`jax.NamedSharding(mesh, P('x'))` 8-way sharded tree with 11 leaves and
**58,765,352 logical bytes**:

| leaf | dtype | shape | bytes |
|---|---|---:|---:|
| `params.embed` | bf16 | 8192x512 | 8,388,608 |
| `params.layers[0].w` | bf16 | 4096x1024 | 8,388,608 |
| `params.layers[0].b` | f32 | 4096 | 16,384 |
| `params.layers[0].scale` | f16 | 4096 | 8,192 |
| `params.layers[1].w` | f32 | 4096x1024 | 16,777,216 |
| `params.layers[1].b` | f32 | 4096 | 16,384 |
| `params.layers[1].mask` | i8 | 4096 | 4,096 |
| `opt_state.momentum.embed` | bf16 | 8192x512 | 8,388,608 |
| `opt_state.variance.embed` | f32 | 8192x512 | 16,777,216 |
| `opt_state.step` | i32 | 8 | 32 |
| `counters` | u8 | 8 | 8 |

Values come from `jax.random.PRNGKey(20261106)`. This is a deterministic,
synthetic random-weight checkpoint, not a real model training checkpoint.

## Comparison

Both sides use Orbax's real orchestration. Orbax's own side is
`PyTreeCheckpointHandler(use_ocdbt=True, use_compression=True)` under
`AsyncCheckpointer`; the WeftFS side is `WeftOrbaxHandler` under the same
`AsyncCheckpointer`. Timings are one run, warm page cache, single process.

| metric | Orbax OCDBT | WeftFS Orbax adapter | delta |
|---|---:|---:|---:|
| save pause (ms) | 10.273 | 1.017 | **-9.256** |
| total save (ms) | 79.631 | 1,770.713 | **+1,691.082 (22.2x)** |
| full restore (ms) | 21.243 | 2,690.338 | **+2,669.095 (126.7x)** |
| single-leaf restore (ms) | 5.248 | 380.432 | **+375.184 (72.5x)** |
| checkpoint physical bytes | 50,644,895 | 50,866,816 | +221,921 (**+0.44 %**) |
| physical / logical | 0.861816 | 0.865592 | +0.003776 |
| physical files | 13 | 189 | +176 |
| leaf count | 11 | 11 | 0 |
| exact leaves (dtype+shape+bytes) | 11/11 | 11/11 | 0 |
| peak process RSS (KiB) | — | 1,693,668 total | includes JAX + both stores |

The **save pause** is the time until `AsyncCheckpointer.save(...)` returns. The
WeftFS adapter returns after scheduling; the actual copy/store work runs on
Orbax's background commit thread through a bounded worker. The pause is smaller
because the work is deferred, not because the store path is faster. The total
save is 22.2x slower.

### Physical bytes

The fair comparison is the store directory **immediately after publish**:
**50,866,816 B** (189 files: 88 zstd shard objects, manifests, commits,
superblock/index/lock, and the canonical `orbax.metadata` object) versus Orbax's
**50,644,895 B** (13 files). That is **+0.44 %** for WeftFS on this workload.
Both are close to the 58,765,352 logical bytes because random floats are barely
compressible. All **11/11 leaves exact** on both sides (dtype, shape, and exact
bytes); the receipt records each leaf's SHA-256.

`weftfs checkpoint restore` always imports the restored container into the store
under `/dcp-reads/<uuid>` (the same CLI verb the DCP adapter uses). After the
full, single-leaf, topology-8, topology-2, and re-shard restores in this run,
the store had grown to **118,482,194 B** (1,239 files), i.e. **+67,615,378 B**
of read-import overhead. That overhead is reported separately and is **not**
part of the checkpoint-byte comparison above.

### Topology independence

* WeftFS restored the same logical leaves into a requested **2-device**
  `NamedSharding` and returned a 2-device result
  (`requested_sharding_honored: true`), exact on all 11 leaves.
* Orbax 0.12.9 ignored the requested 2-device target and restored with the saved
  8-device sharding, emitting its documented warning
  (`requested_sharding_honored: false`). Exactness was still 11/11. This is an
  Orbax limitation on this path, not a WeftFS loss; the WeftFS adapter owns the
  placement of the restored leaves.
* The Rust-side layout proofs `checkpoint restore --topology 8` and
  `--topology 2` both returned exact leaves: 242.785 ms and 253.270 ms.

### Rust verification

* `checkpoint verify --name orbax-court` verified **11 tensors**.
* `verify --full` immediately after save: **ok**, generation 3, 90 objects, 90
  manifests, 88 representations.
* `verify --full` after all restores: **ok**, generation 14, 95 objects, 3,719
  chunks, 95 manifests (includes the imported read containers).
* The canonical `weft.orbax.v1` record reported 11 leaves; the pointer and the
  store metadata record are both canonical WCB and match the manifest content
  and logical roots.

## Every measured loss

1. **Total save is 22.2x slower**: 1,770.7 ms vs 79.6 ms. The Rust
   `checkpoint write` command alone was 1,701.4 ms; the default `zstd` shard
   strategy is slow on incompressible random weights. The adapter also writes a
   full-size safetensors staging container to the host before the CLI reads it.
2. **Full restore is 126.7x slower**: 2,690.3 ms vs 21.2 ms. The adapter calls
   the Rust CLI per restore, which reopens the store, walks/decodes shards, and
   imports a restored container into `/dcp-reads/...`.
3. **Single-leaf restore is 72.5x slower**: 380.4 ms vs 5.2 ms, for the same
   process-start and store-open reasons.
4. **Physical bytes are 0.44 % larger** (+221,921 B) and use 189 files instead
   of 13. The store carries manifests, commits, indexes, and the metadata
   object in addition to tensor shards.
5. **Read imports grow the store**: repeated restores added 67,615,378 B under
   `/dcp-reads/...` in this run. This is real CLI/adapter overhead; the
   checkpoint itself did not grow.
6. **Staging is an extra full-size host write**: the adapter writes the leaves
   into a safetensors staging container inside the Orbax temporary directory,
   then removes it after ingestion. Orbax's TensorStore path writes its own
   chunk files directly.
7. **The save-pause win is a scheduling effect, not a throughput win**. The
   adapter defers work to Orbax's background commit thread; total save is still
   22.2x slower.
8. **Memory is not an isolated adapter measurement**: peak process RSS was
   1,693,668 KiB (~1.62 GiB), including JAX/TensorStore/Orbax, the source tree,
   both restored trees, and the store. It is not a claim about the adapter
   alone.

## What is not covered

* **CPU only.** Eight forced CPU devices in one process; **no TPU and no GPU**.
* **Single process, single host.** No `jax.distributed.initialize`, no multi-host run,
  and no real distributed JAX.
* Orbax's checkpoint is OCDBT/TensorStore on the local filesystem; WeftFS is a
  separate on-disk format. This compares two implementations, not one
  interchangeable checkpoint directory.
* Orbax features not exercised: Zarr3, per-leaf `save_args`/`restore_args`,
  transforms, custom metadata, step-metadata contents, retention/deletion
  internals, emergency checkpointing, multi-axis meshes, and dtypes outside the
  exact WEFT-TENSOR v1 set (`f16`, `bf16`, `f32`, `f64`, `i8`, `u8`, `i16`,
  `i32`, `i64`). Bool, float8, complex, and unsigned 16/32/64-bit arrays are
  rejected, never coerced.
* No process crash, power loss, or media failure during an Orbax save.
* Synthetic random weights, one run, warm caches. These are not fleet,
  production, or model-training claims.
* No cross-language (Go) parity for this adapter; the Go store has no Orbax
  handler.
