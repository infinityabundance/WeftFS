# Buck2 action-cache adapter (prototype)

**Status: prototype adapter, not a Buck2 plugin.** This directory implements a
real local CAS plus a Buck2-compatible key/digest mapping. It does not speak
REAPI gRPC, does not load into Buck2, and makes no integration claim.

The prototype proves the identity mapping and exact-rebuild behavior the
architecture requires:

- Action key = a real `weft.context-root.v1` record. Its selector is
  `select true` in namespace `buck2`; its pinned `snapshot` is the BLAKE3 digest
  of the typed action context: action type, toolchain identity, and the sorted
  list of input paths with `sha256:<hex>` digests and sizes.
- Blob address = SHA-256 (`sha256:<hex>`), stored at
  `cas/sha256/<first2>/<hex>`, matching the REAPI bytestream blob layout and
  Buck2's default CAS digest.
- Action record = `actions/<ContextRootId>.json` with the typed action context,
  output path, output digest, and the derived URI
  `weft://buck2/c/<ContextRootId>`.

## Commands

```sh
# Inside the Go court.
cd adapters/buck2

go run ./cmd/buck2cas init --root /tmp/cas
printf 'input\n' > /tmp/in.txt
go run ./cmd/buck2cas store --root /tmp/cas \
  --action-type compile_object --toolchain rust-1.85.0 \
  --input src/in.txt=/tmp/in.txt --output /tmp/out.bin --output-path out/object.bin
go run ./cmd/buck2cas fetch --root /tmp/cas \
  --action-type compile_object --toolchain rust-1.85.0 \
  --input src/in.txt=/tmp/in.txt --out /tmp/materialized
go run ./cmd/buck2cas query --root /tmp/cas --action-type compile_object --json
go run ./cmd/buck2cas verify --root /tmp/cas
go run ./cmd/buck2cas demo --root /tmp/cas-demo \
  --receipt bench/receipts/buck2-cas-<stamp>.json
```

The demo asserts: the same key returns identical bytes; a different toolchain
or input misses; query-by-context (action type, toolchain, input digest) works;
and `verify` re-derives every key and re-hashes every blob. Storing different
output bytes under the same key fails closed.

## Explicit non-goals

- No Buck2 worker/plugin ABI, no REAPI gRPC, no remote execution.
- No claim that this is a drop-in Buck2 artifact backend.
- No performance claim. The receipt records correctness only.
