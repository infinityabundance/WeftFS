# WeftFS v0.4.0 — correctness, measured storage-path, and scale

Research preview. v0.4.0 changes the **implementation** substantially while
keeping the frozen v1 on-disk formats byte-identical by default. It also
publishes a correctness defect fix, a 1,000,000-input cross-language fuzz court,
and the largest crash/mount/time-travel runs to date.

## Container images

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.4.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.4.0
docker pull ghcr.io/infinityabundance/weftfs:v0.4.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.4.0
```

`latest` is also published. `weftfs`/`weftfs-go` need `--device /dev/fuse
--cap-add SYS_ADMIN` only for mounting. See `docs/containers.md`.

## Rust crates

```toml
weftcodec = "0.4.0"
weftfs = "0.4.0"
```

## Correctness

- **Capability non-disclosure fixed.** Resolver work accounting previously
  counted every object and relation before authorization and echoed the count
  on budget failure. `work_units` now counts only readable objects and visible
  relation edges, authorization is decided before accounting, and budget
  denials use fixed, non-disclosing messages. `tests/capability_nondisclosure.rs`
  builds two stores with identical authorized views and different hidden
  populations (2 vs 7 hidden files, hidden relation endpoints) and requires
  identical results, work units, and budget outcomes. Normative in
  `spec/WEFT-CAPABILITIES.md` §4.1 and §6.
- **Cross-language fuzz court (1,000,000 inputs).** Deterministic
  xorshift64* corpus with checksum-repaired body/table mutations and semantic
  faults, fed to the independent Rust and Go decoders. The first deep run found
  **544 real zstd strictness mismatches**. One normative rule now applies in
  both codecs: an empty zstd payload is invalid; a zstd header's declared
  content size must equal `decoded_len`; the actual decoded length must match.
  Re-run: **0 mismatches, 0 panics**, 172,294 accepted, peak RSS 6.3 MiB /
  205.9 MiB. Two new byte-identical reject vectors. The failing receipt is
  retained.

## Measured storage path (all published with commands and receipts)

| Change | Before | After |
|---|---|---|
| Relation selector, 10k objects × 10k relations | 132.8 ms | 0.69 ms warm |
| Tag selector, same store | 148.0 ms | 2.05 ms warm |
| 4 KiB overwrite of a 64 MiB file | 69.6 ms, 131.7 MiB RSS | 33.0 ms, 3.8 MiB RSS |
| Open a 10,000-generation store | 3,789 ms, 10,001 commit reads | 71.6 ms, 18 commit reads |
| 1,000 creates | 89.87 ms, 1,000 durable commits | 22.01 ms, 1 commit |
| 256 MiB create | 269 MiB peak RSS | 6.8 MiB peak RSS |

- **Selector index**: per-state index narrows candidates; `eval_expr` remains
  authoritative, enforced by a scan-equivalence court (3 states, 315
  selector/view comparisons, 1,566 assertions). The cache guard fails closed.
- **Extent-level COW**: range writes read the manifest and only touched chunks;
  unchanged chunk references are reused verbatim.
- **Bounded open**: generation-addressed replay to the newest verified
  checkpoint, with a mandatory recovery note naming it. Any inconsistency
  falls back to full enumeration. `verify --full` still enumerates every
  commit.
- **Group commit**: `Store::group` batches mutations with a journalled
  in-memory apply and exactly one durable publish; rollback and crash-proxy
  tests included.
- **Streaming I/O**: `create_file_streaming*` and `append_file_streaming` use
  bounded memory. The file bound is now 1 TiB plus 16,777,216 chunks, with
  checked conversions. The effective cap for chunked files is the WCB array
  decode limit, documented rather than hidden.

Receipts: `index-bench-20261104T085157Z.json`,
`extent-cow-20261104T090112Z.json`, `fast-open-20261104T090945Z.json`,
`group-commit-20261104T092015Z.json`, `streaming-20261104T094742Z.json`.

## Codec as physical representation (opt-in, experimental, Rust-only)

`weft.manifest.v2` can store an object's bytes as a WFT1 frame
(`zstd`/`delta`/`reference`) with recorded dependencies, a verified strategy,
bounded dependency depth, cycle rejection, and fail-closed reads when a base is
missing. GC retains representation dependencies; `verify --full` decodes and
re-hashes them. Selection is explicit: `--representation codec-auto` on the CLI
and on `mount`. The default `raw` path is byte-identical to v0.3.0 and the
committed fixture rebuilds to head `8959c65b…e1d81`.

Measured on 8 MiB synthetic data: repetitive text 8,395,250 B → **16,853 B**;
near-duplicate pair 8,395,250 B → **1,153,697 B** DELTA. Incompressible input
falls back to raw. There is exactly one v2 schema spelling.

**The Go store has no v2 parser and rejects such manifests; the Rust→Go
handoff court remains on the raw path.** Cross-language v2 parity is future
work.

## Scale courts

- **Crash consistency: 100,000/100,000 iterations pass.** 0 partial states,
  0 bad verify states, 66,667 old / 33,333 new, phases
  33,334 / 33,333 / 33,333, 86 min 36 s at 19.2 it/s. Counts were recomputed
  independently from the 14.8 MB NDJSON. This is a process-fault proxy: no
  power cut, no cache drop, no torn sectors.
- **100 concurrent context mounts** over one store: 100 distinct roots,
  200 objects, 599 relations, 50 Rust + 50 Go daemons, 425/425 file SHA-256
  matches, 100/100 distinct listing digests, 0 surviving daemons. Mount p50
  31.6 s (includes 100-way startup contention on 4 vCPUs); daemon RSS 1.62 GiB.
  That memory cost is real and is why a shared multi-mount daemon is now listed
  as required future work.
- **10,000-version time travel**: 101 snapshots, 10 mounted at once, ingest
  60.9 versions/s, reopen p50 6 ms, every snapshot byte-exact and unchanged
  across a live rename and retag.

Receipts: `crash-torture-20261104T104233Z.json` (18 MB; log and TSV stored
losslessly as `.zst`), `context-mounts-20261104T125344Z.json`,
`time-travel-20261104T125947Z.json`, plus determinism files.

## Verification

`scripts/verify-all.sh` passes **19/19 courts** at `verify-20261104T103447Z`,
including spec drift (27 comparisons), generated types, codec cross-decode in
both directions, context cross-language gates, the new fuzz differential, Buck2
CAS, and agent memory. Rust: 328 passed, 2 ignored (the documented CAP_SETUID
and kernel-7.3 same-inode direct-I/O EIO cases).

## Still not claimed

- Go parity for `weft.manifest.v2`.
- On-disk/persistent index segments; the index is per-state and rebuilt on
  first use.
- Physical media power loss, host page-cache loss, RAPL/joule energy.
- Float-aware, sharded/quantized-layout, weight-delta, KV-cache,
  activation/gradient, and semantic-delta codec modes; layout-independent
  tensor identity.
- Native async API and object-store backend; Buck2 plugin/REAPI; real-time
  aging.
- 1,000,000-operation POSIX torture (13,694 measured); a real public
  model-family corpus versus ZipNN/DFloat11/`zstd --patch-from` (still the
  synthetic `ai-v1` corpus).
- TLA+ checks the abstract commit/recovery model, not implementation
  refinement.

The authoritative gap lists remain `docs/STATUS.md`, `docs/PROGRAM.md`,
`bench/RESULTS-*.md`, and `verification/README.md`.
