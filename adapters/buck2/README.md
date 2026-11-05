# Buck2 action-cache adapter (WeftFS store backend)

**Status: adapter, not a Buck2 plugin.** This directory is a command-line
action cache keyed by a canonical Weft `ActionKey` and stored on the WeftFS
store. It does **not** load into Buck2, does not implement a Buck2 worker or
plugin ABI, and does **not** speak REAPI gRPC. Buck2 itself is not exercised:
the court records whether a `buck2` binary is present and, if so, runs only
`buck2 --version`.

Normative contract: [`spec/WEFT-BUCK2.md`](../../spec/WEFT-BUCK2.md).

## What changed from the prototype

The prototype kept a private directory tree (`cas/sha256/...`,
`actions/*.json`). The current adapter moves the cache onto the WeftFS store so
build outputs and AI artifacts share one content-addressed primitive:

- Every CAS blob is an ordinary WeftFS file object at
  `/cas/outputs/<action-key>/<output-path>`. Its **ContentId** is
  `H("weft/chunk/v1", exact bytes)`, not SHA-256.
- The action record is a canonical WCB file (`weft.buck2.action-record.v1`) at
  `/cas/records/<action-key>`.
- `cas_action` points from the record object to the output object;
  `cas_output_of` points back, so `by:action=<key>` is a one-hop WeftQL
  selector.
- The Rust CLI can read and verify the same store; the Go adapter can read a
  store written by the Rust CLI.

Why the Go store library rather than the Rust CLI for the adapter: the adapter
is already a Go program, and `WeftFS-Go/store` gives it the same on-disk format,
relations, Context Roots, bounded reads, and GC without spawning a process per
operation. The Rust CLI is used as an independent oracle in tests and for
mounting the generated Context Root.

## Identity

```
ActionKey  = H("weft/buck2/action/v1",
               WCB({action_type, toolchain,
                    inputs: [sorted ContentId hex],
                    params: canonical map}))
ContentId  = H("weft/chunk/v1", exact blob bytes)
```

Input **paths are metadata**, not key material. The record keeps them; the key
hashes only the sorted input content roots. `RecordObjectId` and
`OutputObjectId` are deterministic from `ActionKey` (see the spec), so either
language can reproduce the layout.

## Commands

All commands run in a Docker court. The example uses the FUSE court because it
has both toolchains and the mount used below.

```sh
scripts/docker-court.sh fuse 'bash scripts/demo-buck2-cas.sh'
```

Inside the court, build the adapter:

```sh
cd adapters/buck2
go build -o /tmp/buck2cas ./cmd/buck2cas
```

### Key file

```json
{
  "action_type": "compile_object",
  "toolchain": "rust-1.85.0",
  "inputs": [{"path": "src/input.txt", "file": "/tmp/in.txt"}],
  "params": {"opt": "2"},
  "output_path": "out/object.bin"
}
```

### Verbs

```sh
# Initialize a WeftFS store.
/tmp/buck2cas init --root /tmp/cas-store

# Derive the key without touching the store.
/tmp/buck2cas key --key-file action.json --json

# Publish an output blob (idempotent for the same key + bytes).
/tmp/buck2cas store --root /tmp/cas-store \
  --key-file action.json --blob /tmp/out.bin

# Fetch by action key; a miss exits 3.
/tmp/buck2cas fetch --root /tmp/cas-store \
  --key "$ACTION_KEY" --out /tmp/materialized.bin

# Resolve by:action=<key> through the WeftQL selector engine.
/tmp/buck2cas query --root /tmp/cas-store --by "action=$ACTION_KEY" --json

# Re-hash the stored output and an optional external blob.
/tmp/buck2cas verify --root /tmp/cas-store \
  --key "$ACTION_KEY" --blob /tmp/materialized.bin

# Persist the generated Context Root, then mount it with the Rust frontend.
/tmp/buck2cas context --root /tmp/cas-store \
  --key "$ACTION_KEY" --name "by:action=$ACTION_KEY" --snapshot keep
weftfs --store /tmp/cas-store mount --mountpoint /mnt/x --foreground \
  --context "by:action=$ACTION_KEY"

# Store GC (mark-and-sweep, delegated to WeftFS-Go/store).
/tmp/buck2cas gc --root /tmp/cas-store --apply --grace-secs 0
```

## Wrapper contract (for a build system that shells out)

- **Environment:** no secret or credential is required. Tests and demos read
  `WEFTFS_RUST_BIN` to locate the independent Rust oracle; runtime verbs do
  not. `NO_COLOR` is accepted and output is plain text/JSON.
- **Key encoding:** 64 lowercase hex characters. Passing uppercase or a short
  key is a usage error.
- **Key file:** canonical JSON as above. `file` inputs are hashed to WeftFS
  ContentIds; a declared `content_id` that disagrees with the file fails
  closed.
- **Exit codes:** `0` success, `1` failure, `2` usage, `3` cache miss,
  `4` verification mismatch/corrupt data, `5` same-key/different-bytes.
- **Concurrency:** the store is single-writer. The adapter takes the writer
  lock for `store`/`context`/`delete`/`gc` and no lock for `fetch`/`query`/
  `verify`. FUSE mounts are read-only and may run concurrently with readers.

## Correctness evidence

Run in the FUSE court (both toolchains, real mount):

```sh
scripts/docker-court.sh fuse 'bash scripts/demo-buck2-cas.sh'
```

It writes `bench/receipts/buck2-cas-store-correctness-<UTC>.json` and proves:

- same declaration → same key; different toolchain/input → different key;
- first publish is new, the repeat is an idempotent hit;
- `fetch` returns exact bytes and the recorded ContentId;
- `query --by action=<key>` resolves exactly one output;
- same key with different bytes fails closed with the live head and physical
  store bytes unchanged;
- a flipped manifest byte is detected by `verify` and `fetch`;
- GC collects an unreferenced action while a named snapshot + Context Root
  keep another output byte-exact;
- the Rust CLI verifies/cats/stats the Go-written store and resolves its
  Context Root;
- the Go adapter fetches/verifies/queries a store written entirely by the Rust
  CLI;
- `mount --context by:action=<key>` lists and reads the artifact byte-exactly.

Go tests (`cas/cas_test.go`, 7 tests) include the cross-language directions and
skip them honestly when no Rust oracle is present (Go-only court).

## Benchmark

```sh
scripts/docker-court.sh bench 'bash scripts/bench-buck2-cas.sh'
```

Receipt: `bench/receipts/buck2-cas-store-<UTC>.json`. It measures 1,000
small-output actions and 100 × 1 MiB actions, physical bytes, and a mount read
of one 1 MiB artifact. Latency is labelled by boundary: library path with one
open writer, FUSE mount read, and CLI fetch (which includes process open and
commit replay).

## Explicit non-claims

- No Buck2 plugin, worker, or scheduler integration.
- No REAPI gRPC, bytestream, or remote execution.
- No claim that Buck2 ran; only a `--version` probe if a binary happens to be
  installed.
- Single output per action in v1.
- No multi-host or distributed cache claim.
