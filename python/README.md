# weftcodec (Python)

An independent Python implementation of the WeftCodec v1 **reader**: WFT1 frame
parsing, canonical WCB decode/encode, domain-separated BLAKE3 content roots, and
a PyTorch-facing dataset over a mounted WeftFS directory.

There are **no Rust or Go extension modules**. This is a third implementation,
written from the normative specification and tested against Rust-generated
conformance vectors.

## Install

```sh
pip install dist/weftcodec-0.5.0-py3-none-any.whl
# with the optional dataset extra
pip install "dist/weftcodec-0.5.0-py3-none-any.whl[torch]"
```

Dependencies: `blake3`, `zstandard`. `torch` is optional and is only imported
when a tensor is requested.

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
| `FLOAT_TENSOR` | 9 | `UnsupportedMode` |
| `TENSOR_SHARDED` | 10 | `UnsupportedMode` |
| `WEIGHT_DELTA` | 11 | `UnsupportedMode` |
| `KV_CACHE` | 12 | `UnsupportedMode` |
| `ACTIVATION_STREAM` | 13 | `UnsupportedMode` |
| `SEMANTIC_DELTA` | 14 | `UnsupportedMode` |

An unimplemented mode raises `weftcodec.errors.UnsupportedMode`. It is never
decoded approximately and never treated as opaque bytes.

## Tests

```sh
pytest -q
```

The suite consumes `../WeftCodec/conformance/v1/**` (Rust-generated vectors) and
runs the Rust CLI for cross-language encode/decode checks when
`/work/.docker-cache/target-rust/release/weftcodec` is available. The PyTorch test
uses `pytest.importorskip("torch")`.
