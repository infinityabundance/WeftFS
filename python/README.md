# weftcodec (Python)

An independent Python implementation of the WeftCodec v1 **reader**: WFT1 frame
parsing, canonical WCB decode/encode, domain-separated BLAKE3 content roots, and
a PyTorch-facing dataset over a mounted WeftFS directory.

There are **no Rust or Go extension modules**. This is a third implementation,
written from the normative specification and tested against Rust-generated
conformance vectors.

## Install

```sh
pip install dist/weftcodec-1.2.0-py3-none-any.whl
# with the optional dataset extra
pip install "dist/weftcodec-1.2.0-py3-none-any.whl[torch]"
# with the optional JAX/Orbax adapter extra
pip install "dist/weftcodec-1.2.0-py3-none-any.whl[orbax]"
```

Dependencies: `blake3`, `zstandard`. `torch`, `jax`, and `orbax-checkpoint`
are optional and are only imported by their adapter modules.

> **PyPI publication requires an API token we do not have.** The wheel is built
> in the Docker court and attached to the GitHub release; it is not published to
> PyPI.

## Usage

```sh
python -m weftcodec verify FRAME                    # validate structure + hashes
python -m weftcodec decode FRAME OUT                # exact plaintext bytes
python -m weftcodec inspect FRAME                   # metadata as JSON
python -m weftcodec verify FRAME --deps DEPS_DIR    # REFERENCE/DELTA/DICTIONARY
```

Exit codes: `0` success, `1` codec or I/O failure, `2` usage error.

```python
from weftcodec import parse, content_root, verify_frame
from weftcodec.dependency import DirectoryResolver
from weftcodec.modes import decode_bytes

resolver = DirectoryResolver("deps")            # files named by lowercase hex
frame_bytes = open("frame.wft1", "rb").read()
frame = parse(frame_bytes)
plaintext = decode_bytes(frame_bytes, resolver)
assert content_root(plaintext) == frame.header.content_hash

result = verify_frame("frame.wft1", resolver)
print(result.ok, result.mode, result.content_hash, result.computed_content_hash)
```

### WCB

```python
from weftcodec import wcb

value = wcb.decode(bytes.fromhex("08030501610301050162050374776f05016300"))
assert wcb.encode(value) == bytes.fromhex("08030501610301050162050374776f05016300")
```

Python/WCB mapping: `None`→null, `bool`, `U64`, `I64`, `str`, `bytes`,
`list`/`tuple`, `dict` (text keys), `Timestamp`. Map keys are sorted by their
full encoded bytes; duplicates are rejected.

### PyTorch dataset

```python
from weftcodec.torch import WeftMapDataset

dataset = WeftMapDataset("/mnt/weft/dataset")
path, raw = dataset[0]                     # (relative path, bytes)

import torch
tensors = WeftMapDataset("/mnt/weft/dataset", dtype=torch.uint8)
path, tensor = tensors[0]                  # torch.frombuffer view
```

Entries are sorted by POSIX-relative path, so ordering is deterministic across
processes.

### PyTorch Distributed Checkpoint adapter

`weftcodec.dcp` implements the shape of
`torch.distributed.checkpoint.StorageWriter` / `StorageReader` over a WeftFS
store. The training process does not need a FUSE mount: every durable byte goes
through the Rust `weftfs` CLI (`checkpoint write` / `checkpoint restore`).

```python
import torch
import torch.distributed.checkpoint as dcp
from weftcodec.dcp import WeftStorageWriter, WeftStorageReader, init_store

init_store("/data/weft-store")

model = MyModel()
dcp.save(
    model.state_dict(),
    storage_writer=WeftStorageWriter(
        "run-0001",                 # single WeftFS checkpoint name
        store_root="/data/weft-store",
        topology=8,                 # store every tensor split 8 ways on axis 0
        shard_strategy="zstd",      # exact; tensor-sharded is the mode-10 option
    ),
    no_dist=True,
)

fresh = MyModel()
dcp.load(
    fresh.state_dict(),
    storage_reader=WeftStorageReader("run-0001", store_root="/data/weft-store"),
    no_dist=True,
)
```

Async save (the encode/store work runs on a `ThreadPoolExecutor` worker, with a
hard per-tensor staging bound; tensors are still copied through host memory):

```python
from weftcodec.dcp import save_checkpoint_async

handle = save_checkpoint_async(
    model.state_dict(),
    checkpoint_id="run-0002",
    store_root="/data/weft-store",
    topology=8,
    staging_bytes=256 * 1024 * 1024,   # 256 MiB bound on one resolved tensor copy
)
metadata = handle.result()             # raises the save error on this thread
```

Every checkpoint is a canonical `weft.checkpoint.v1` manifest plus mode-10 or
exact codec shards; the DCP metadata is canonical WCB at
`/checkpoints/<name>/dcp.metadata`. The adapter verifies each tensor's BLAKE3
content root against the Rust manifest before/after every transfer. Verify with:

```sh
weftfs --store /data/weft-store checkpoint verify --name run-0001 --json
weftfs --store /data/weft-store verify --full --json
```

A checkpoint written by the Rust CLI alone (plain safetensors tensor names) is
readable too: the adapter synthesises DCP metadata from the manifest and reports
`reader.metadata_source == "manifest"`. Supported tensor dtypes are exactly the
WEFT-TENSOR v1 set (`f16`, `bf16`, `f32`, `f64`, `i8`, `u8`, `i16`, `i32`,
`i64`); everything else is rejected, never coerced. ShardedTensor, DTensor and
custom `planner_data` are not persisted. See `docs/PYTORCH-DCP.md` for the full
contract and non-claims.

### JAX / Orbax adapter

`weftcodec.orbax` is an Orbax `AsyncCheckpointHandler` that stores a JAX PyTree
as one canonical `weft.checkpoint.v1` checkpoint through the same Rust CLI and
the same adapter plumbing as the DCP path. Each flattened leaf is recorded in a
canonical WCB `weft.orbax.v1` record with its path, dtype, shape, BLAKE3 content
root and logical root; the Orbax directory only carries a small canonical
locator pointing at the store.

```python
import jax
from weftcodec.orbax import WeftOrbaxHandler, WeftSaveArgs, WeftRestoreArgs

handler = WeftOrbaxHandler(staging_bytes=256 * 1024 * 1024)
target = jax.tree_util.tree_map(
    lambda a: jax.ShapeDtypeStruct(a.shape, a.dtype, sharding=a.sharding), params
)
# Drive it with orbax.checkpoint.Checkpointer, AsyncCheckpointer, or
# CheckpointManager:
#   save(directory, args=WeftSaveArgs(
#       item=params, store_root="/data/weft-store", checkpoint_id="run-0001"))
#   restore(directory, args=WeftRestoreArgs(item=target))
```

Supported leaf dtypes are the same exact WEFT-TENSOR v1 set. A target PyTree
with a missing, extra, differently typed, or differently shaped leaf fails
closed; different JAX sharding topologies are accepted because the stored
logical tensor is topology independent. `staging_bytes` bounds the staging
container and the restored bytes. See `docs/JAX-ORBAX.md` for the full contract,
measured comparison against Orbax's own OCDBT checkpoint, and non-claims (CPU
only, single process, no TPU/GPU, no multi-host JAX).

## Supported modes

| Mode | Value | Python reader |
|---|---:|---|
| `RAW` | 0 | implemented |
| `ZSTD` | 1 | implemented (`zstandard`) |
| `REFERENCE` | 2 | implemented |
| `DELTA` | 3 | implemented |
| `DICTIONARY` | 4 | implemented (raw-content zstd dictionary) |
| `STRUCTURED` | 5 | implemented (canonical WCB validation) |
| `TENSOR` | 6 | implemented (descriptor validation) |
| `TOKEN_SEQUENCE` | 7 | implemented (descriptor + varint payload) |
| `QUANTIZED_TENSOR` | 8 | implemented (descriptor validation) |
| `FLOAT_TENSOR` | 9 | implemented (all five dtypes, `huffman-canonical-v1` exponent plane) |
| `TENSOR_SHARDED` | 10 | implemented (q4_0/q8_0 shards, plane verification, shard-set root) |
| `WEIGHT_DELTA` | 11 | implemented (base resolution, modular word subtraction, `delta_root`) |
| `KV_CACHE` | 12 | implemented (one page per frame, byte-plane decode) |
| `ACTIVATION_STREAM` | 13 | implemented (one chunk per frame, `digest_root`, stream root) |
| `SEMANTIC_DELTA` | 14 | implemented (policy verification against the resolved base) |
| `TENSOR_TILED` | 15 | implemented (per-tile roots and whole-tensor logical root) |
| `SPARSE_DELTA` | 16 | implemented (`zero-run-v1`, `delta_root`) |

Every allocated mode 0–18 is implemented. There is no approximate or lossy
decode path. Reserved mode values 17–127 and experimental values 128–255 are
rejected by `weftcodec.frame` with `UnsupportedMode`.

The reader is deliberately a decoder, not a WEFT-TENSOR container reader: it
does not claim WeftQL selectors, Context Roots, capability envelopes,
`weft.checkpoint.v1` manifests, WFA1 archives, or `weft.tensor.layout.v1` /
`weft.tensor.shard.v1` records. `TENSOR_TILED` supports full decode; the
region reader is not implemented in Python.

## Tests

```sh
pytest -q
```

The suite consumes `../WeftCodec/conformance/v1/**` (Rust-generated vectors) and
runs the Rust CLI for cross-language encode/decode checks when
`/work/.docker-cache/target-rust/release/weftcodec` is available. The PyTorch test
uses `pytest.importorskip("torch")`.

`test_ai_modes.py` decodes every valid AI-mode vector to its recorded bytes and
recomputes every recorded root; `test_random_differential.py` generates at least
2,000 frames per mode 9–16 from a deterministic PRNG and compares the Python
reader against the Rust CLI on every frame, plus 1,600 body mutants.
