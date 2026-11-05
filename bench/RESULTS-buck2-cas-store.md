# P6 — store-backed Buck2 CAS: correctness and bench v1

**Receipts**

- Correctness (FUSE court, 41/41): `bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json`
- Benchmark (bench court): `bench/receipts/buck2-cas-store-20261105T163930Z.json`
- Raw logs: the matching `.log` files beside each receipt.
- Revision: WeftFS `d175fe7`, WeftCodec `baeb069`, WeftCodec-Go `8c5b07b`,
  WeftFS-Go `2e2575b`.
- Courts: `weft/fuse-court:1` (correctness) and `weft/bench-court:1` (bench),
  kernel `7.3.1-1-cachyos`, `--memory=3g --memory-swap=3g --pids-limit=1024
  --cpus=4`, bounded `/tmp`.

**Label.** Single host, synthetic deterministic fixtures, local WeftFS store,
Docker court. This is not a fleet, Buck2, or remote-cache claim.

## What is measured

The action cache now lives on the WeftFS store:

- every blob is an ordinary WeftFS file; its CAS address is the WeftFS
  `ContentId` `H("weft/chunk/v1", bytes)`;
- the action record is canonical WCB at `/cas/records/<ActionKey>`;
- `cas_action` (record→output) and `cas_output_of` (output→record) are store
  relations;
- `by:action=<key>` is the selector
  `select relation(cas_output_of, obj:<record-object-id>)`.

`ActionKey = H("weft/buck2/action/v1", WCB({action_type, toolchain, sorted
input ContentIds, canonical params}))`. Input paths are metadata, not key
material.

## Correctness court (41/41)

`scripts/docker-court.sh fuse "bash scripts/demo-buck2-cas.sh"`.

- 7 Go tests pass in the FUSE court (2 of them skip in a Go-only court because
  no Rust oracle is present).
- Key derivation: same declaration → same key; different toolchain/input →
  different key.
- Publish: first store is new; repeat is an idempotent hit; `fetch` copies exact
  bytes and the recorded ContentId.
- `query --by action=<key>` and `context create` resolve exactly one object
  through WeftQL; the retained Context Root reads exact bytes after GC.
- Conflict: same key with different bytes returns `Conflict` with the live head
  and physical store bytes unchanged; the existing output still fetches.
- Tamper: flipping a manifest byte makes `verify` and `fetch` fail; restoring
  the byte makes both pass.
- GC: a dedicated store is snapshotted and a Context Root pinned to the named
  snapshot; a later action is deleted from the live head; `gc --apply
  --grace-secs 0` deletes 4 files / 869 bytes (2 manifests, 2 chunks) for the
  unreferenced action while the retained output stays byte-exact. The collected
  action no longer fetches.
- Cross-language: Rust `verify --full`, `cat`, `stat` (`content_hash` equal to
  the CAS ContentId), and `context resolve` accept the Go-written store; the Go
  adapter `fetch`/`verify`/`query` reads a store written entirely by the Rust
  CLI.
- Mount: `weftfs mount --context by:action=<key>` lists exactly
  `object.bin--bc560cec` and reads 50 bytes whose SHA-256 equals the recorded
  artifact. `buck2` was absent; only `--version` would have been run.

## Benchmark

`scripts/docker-court.sh bench "bash scripts/bench-buck2-cas.sh"`.

1,000 small actions (128-byte outputs) and 100 actions with 1 MiB outputs.
Latency is wall clock around the adapter library path with one open writer.
`store_new` publishes the output file, the record file, and both relations
(4 commits; the Go store has no group-commit API). `store_hit` is the
idempotent repeat. `fetch_verify` is `Fetch` plus an independent re-hash in
`Verify`. `store_miss` fetches a never-stored key.

| Phase | Count | p50 | p95 |
|---|---:|---:|---:|
| small 128 B `store_new` | 1,000 | 33.34 ms | 43.56 ms |
| small 128 B `store_hit` | 1,000 | 0.074 ms | 0.123 ms |
| small 128 B `fetch_verify` | 1,000 | 0.071 ms | 0.078 ms |
| small 128 B `store_miss` | 64 | 0.0047 ms | 0.0058 ms |
| 1 MiB `store_new` | 100 | 80.25 ms | 84.75 ms |
| 1 MiB `store_hit` | 100 | 0.790 ms | 1.042 ms |
| 1 MiB `fetch_verify` | 100 | 1.330 ms | 1.720 ms |
| 1 MiB `store_miss` | 64 | 0.0045 ms | 0.0051 ms |

Physical bytes after all writes (checkpoint interval 256):

| Component | Bytes | Files |
|---|---:|---:|
| objects | 105,570,228 | 3,702 |
| manifests | 35,463,015 | 2,227 |
| commits | 3,919,608 | 6,612 |
| other (refs, superblock, FORMAT, LOCK) | 8,272 | 5 |
| **total** | **144,961,123** | **12,545** |

Mount read of one 1 MiB artifact (`cat` from the FUSE context view, 5 reads):
p50 **1.236 ms**, p95 1.297 ms, min 1.193 ms, max 1.297 ms.

CLI fetch of the same 1 MiB artifact (`buck2cas fetch --out`, 5 runs) p50
**367.75 ms**, p95 394.42 ms. This includes process start and a read-only store
open over the 6,612-commit chain; it is not the in-process fetch path. It is
reported so the wrapper boundary is not mistaken for the library measurement.

## Interpretation and losses

- The in-process hit/miss path is cheap; the publish path is dominated by four
  fsync-heavy commits per action. A group-commit API on the Go store would cut
  that, but this phase does not change the subproject.
- Checkpoint interval matters to physical bytes: the same store with
  `--checkpoint-interval 1000000` wrote 109.9 MB (fewer state manifests) but a
  cold read-only open replayed the whole chain. The benchmark uses 256 and
  reports both costs honestly.
- The CLI fetch number is a process-boundary measurement, not a cache hit
  latency. The mount read is the relevant read path for a mounted consumer.
- No Buck2, REAPI, worker, or remote-execution comparison exists. No superiority
  claim is made.
