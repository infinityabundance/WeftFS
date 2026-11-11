# WeftFS v1.3.0 — throughput profiling, a mutex-free restore path, 7B scale, and Go object-store parity

Release candidate. v1.3.0 is the first release after v1.2.0. It changes **no
frozen wire or on-disk byte**: the WFT1 frame, payload modes 0–20, canonical
WCB, `weft.manifest.v1`–`v4`, the selector grammar, the store layout, the
superblock and recovery protocol, the transport container, and every frozen
store and conformance fixture are unchanged. No mode value was added, removed,
or renumbered in this release.

The release is **R10, R12, R13, R14, and R15**. R10 removes measured
publication and restore bottlenecks and publishes the losses; R12 measures the
real Qwen2.5-7B-Instruct checkpoint at the default 3 GiB court, a 100-replica
cold start, and attested loads; R13 removes the single backend mutex from
immutable reads and caught a real packed-write regression; R14 and R15 close
the Go gaps that were still open at v1.2.0 (Azure Blob, a read-handle pool, and
store-level KV parity for modes 12 and 19).

The version bump is package metadata plus the tool-version field in newly
written provenance records (Rust `env!("CARGO_PKG_VERSION")`, Go
`storeVersion`). `1.3.0` means “the release after 1.2.0”, not production
readiness.

Everything was built and measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

**Date:** 2026-11-11 · **Tag:** `v1.3.0` (created by the release pass, after
verification) · **Branch:** `staging`.

**Release-tree aggregate: PENDING VERIFICATION PASS**

## Highlights

- **Measured phases instead of guesses (R10).** An opt-in profiler
  (`WEFT_PHASE_PROFILE=1`, or a harness calling `begin()`) shows that a
  Qwen2.5-3B planned family ingest spends **865,052.9 ms of 920,052.4 ms
  (89.8 %)** in encode, and a 3B fast-raw 8-way save spends **771,452.7 ms of
  798,331.8 ms (96.6 %)** publishing blobs across 99,103 publications.
  Receipts `throughput-20261110T134917Z.json`,
  `throughput-20261110T145341Z.json`; analysis
  [`bench/RESULTS-throughput.md`](../bench/RESULTS-throughput.md).
- **Batched publication pays for fast-raw saves (R10).** A deferred pack
  catalog cuts a 0.5B Qwen fast-raw save from **108,889.2 ms to 29,008.0 ms
  (3.75x)** and the 3B save from **800,031.6 ms to 507,126.8 ms (1.58x)**; the
  same mechanism is a **loss** for zstd-1 split saves (0.67x / 0.77x) and for
  planned saves at 8 ranks (0.85x), and it costs **+12.3 %** store bytes for
  the 3B sequential case and **+31–43 %** peak RSS. Every loss is in the same
  receipt.
- **Restore without the backend mutex (R13).** The state mutex held
  **0.41–0.65 of the restore wall**; it now holds **0.000** in every
  configuration. Store-only Qwen2.5-0.5B restore is
  1,198.7 / 2,248.7 / 2,808.2 / 2,813.6 / 2,803.4 MB/s at 1/2/4/8/16 requested
  workers, and 3B is 1,177 / 2,090 / 2,998 / 3,073 MB/s; the single 0.5B and 3B
  embedding falls from 552 to 454 ms and from 1,303 to 1,072 ms. Receipts
  `restore-scaling-20261111T090904Z.json`,
  `restore-scaling-3b-final-20261111T092000Z.json`,
  `restore-scaling-20261111T120026Z.json`; analysis
  [`bench/RESULTS-restore-scaling.md`](../bench/RESULTS-restore-scaling.md).
- **The aggregate found a real defect (R13).** The 45th court run
  (`verify-20261111T103327Z.json`, 44/45) failed
  `TestRustPacksGoReadsAndVerifies`: a packed plane holds an open pack's members
  in memory until the catalog is sealed, and the new pooled read handle resolved
  a just-written chunk as missing. Fixed in `dd86171`; the failing receipt is
  retained.
- **7B is honest about what does not fit (R12).** Per-tensor planned ingest of
  the real Qwen2.5-7B-Instruct checkpoint is **OOM-killed at the default 3 GiB
  court** (exit 137, cgroup peak 3,221,225,472 B). A streaming 8-way fast-raw
  write does complete. Receipt `scale-7b-20261110T213731Z.json`.
- **Go Azure Blob, read pool, and KV parity (R14/R15).** Go now speaks the same
  Azure contract as Rust and passes a two-way handoff court; its read path uses
  a bounded handle pool; its store writes and reads mode 12 and 19 pages.
  Receipt `azure-court-20261111T144613Z.json`; logs
  `kv-go-parity-fuse-20261111T154406Z.log`,
  `kv-go-bench-20261111T154406Z.log`.

## Storage and identity

The representation planner, logical tensor roots, tiles, deltas, and the
rebase/transcode paths are unchanged from v1.2.0 and are not re-measured here.
The carried state, with its original receipts:

- **Qwen2.5-0.5B + Instruct family:** **1,276,128,956 B** total,
  **1,199,958,505 B** of tensor data, against **1,535,945,154 B** for
  independent per-tensor `zstd -19` and 1,316,518,229 B for ZipNN
  (`planner-float-20261106T035316Z.json`,
  [`bench/RESULTS-real-model.md`](../bench/RESULTS-real-model.md)).
- **Region reads:** a one-row read of the Qwen embedding falls from 749.3 ms to
  2.12 ms as independently decodable tiles (353x) at **+0.51 %** bytes and a
  3.25x write (`float-tiled-20261107T042955Z.json`,
  `tensor-tiles-20261105T222531Z.json`).
- **Restore:** v1.3.0's own numbers are the R13 matrix above. Pipelined restore
  is 1,434.7 ms / 695.5 MB/s on the 0.5B container at two workers (R10), and a
  7B packed restore is 337.7 / 367.5 / 367.8 MB/s at 1/4/8 workers against a
  13.97x Amdahl cap (`scale-7b-20261110T213731Z.json`) — the loss is published,
  not hidden.
- **Fleet cold start:** 100 replicas behind one shared read-only handle finish
  in **51,827.3 ms** with single-flight and cache, against **672,765.2 ms**
  without; backend bytes fall from 442,325,447,419 to 28,248,268,423 and reads
  from 112,368 to 61,908, with 37,327 coalesced waiters (87 %) and 13,133 cache
  hits (`fleet-coldstart-20261110T225018Z.json`).
- **Attested loads:** a 272-tensor load produces a
  `weft.load-attestation.v1` receipt in 2,133.2 ms (456,430 B) and independent
  verification costs 2,241.0 ms — a second pass, not free
  (`attested-loads-20261110T232937Z.json`).

## Multi-writer

Unchanged from v1.2.0: **MW1 is the default**, **MW2 is implemented and
opt-in** (`--writer-mode optimistic`, explicit transaction API only), and
**MW3 is not implemented and not claimed**. The contract is
[`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md).

- The bounded TLA+ model checks 2,690,909 states / 268,641 distinct for two
  committers, two keys, `MaxGen = 2`; two deliberately flawed variants produce
  counterexamples (`tla-20261109T174959Z.json`).
- Mixed Rust/Go committers pass the correctness court
  (`multiwriter-court-20261109T175012Z.json`) and the four-stage failure court
  (`multiwriter-failure-20261109T175014Z.json`).
- **The scaling loss is the headline:** on the local backend, disjoint
  publication goes from **4,000 tx/s at N=1 to 580.3 tx/s at N=32**
  (efficiency 0.0045), p99 0.2 → 302.2 ms, and retry amplification 1.0 → 2.55.
  A hot name still yields exactly one winner per round. MinIO at N=4 reaches
  224.7 tx/s / p99 64.1 ms (`bench-multiwriter-20261109T175227Z.json`,
  [`bench/RESULTS-multiwriter.md`](../bench/RESULTS-multiwriter.md)).
- Azure is a store backend in Rust and Go, but **MW2 on `azure` is refused
  `Unsupported` in both languages** and is asserted by test.

## KV and retention

- **Mode 19 `KV_CACHE_PROV`** (unchanged from v1.2.0, carried): it adds
  **643 B (+34.61 %)** at a 1 KiB page and **644 B (+0.97 %)** at 64 KiB;
  decode rises from 11.1 to **28.5 us** and an authorized read from 35.0 to
  53.0 us (`kv-provenance-20261109T181406Z.json`,
  [`bench/RESULTS-kv-provenance.md`](../bench/RESULTS-kv-provenance.md)).
  Descriptor validation costs 16.9–19.0 us. The adversarial court proves
  collision roots differ, cross-scope reads emit 0 bytes with `Unauthorized`,
  and forged descriptors are rejected
  (`kv-provenance-court-20261109T182750Z.json`).
- **Go store parity (new in R15):** `store/kv.go` writes, reads, authorizes,
  and verifies modes 12 and 19 with the same bytes as Rust. The measured cost
  is **10.56 ms and 4.08 MB of allocation per 4 KiB page write**, because every
  page write is its own commit and Go has no decoded-object cache or
  single-flight; a 4 KiB read is 0.697 ms and a 64 KiB read 2.46 ms
  (`kv-go-bench-20261111T154406Z.log`,
  `kv-go-parity-fuse-20261111T154406Z.log`,
  [`bench/RESULTS-kv-go-parity.md`](../bench/RESULTS-kv-go-parity.md)). These are
  published as costs, not as a competitive result. Go still has no KV prefix
  tier and verifies no envelope signature on this path.
- **Retention** (unchanged): a retention binding is not an identity; class-aware
  GC computes reachability first and only orders the unreachable set. On the
  3,600-request trace the class-aware policy reaches a **74.94 %** safe hit rate
  (fast) and **73.47 %** (slow) against 35.31 % / 0.97 % for fixed TTL, with
  **0 unsafe reclaims**; fixed TTL loses 45 and 37 reachable frames
  (`retention-20261109T220237Z.json`,
  [`bench/RESULTS-retention.md`](../bench/RESULTS-retention.md)). The hint is
  not a guarantee: there is no wall-clock or skew defence.

## Formats

- **Mode 19 `KV_CACHE_PROV`** — identity binds a KV page to the model,
  adapters, tokenizer, attention configuration, positional treatment, and
  layout, with a 32-byte salt and sorted extension set. Normative text:
  [`spec/WEFT-KV-PROVENANCE.md`](../spec/WEFT-KV-PROVENANCE.md).
- **Mode 20 `ENGRAM_SHARD`** — independently rooted, region-addressable row
  groups with an explicit supersession chain. Normative text:
  [`spec/WEFT-ENGRAM.md`](../spec/WEFT-ENGRAM.md). **The whole-table loss is
  published:** mode 20 stores 108,743,984 B and decodes the whole table in
  616.2 ms, while one ZSTD-3 frame is 4,236,607 B and decodes in 57.0 ms.
  Mode 20 wins only on sparse reads (one group 9.15 ms / 1.70 MB read) and on
  incremental update (4/64 groups: 62.7 ms / 16.79 MB against a 499.7 ms /
  118.73 MB rewrite — but the incremental decode is 2.30 s against 616 ms). The
  Rust store writes and updates it; **Go reads, verifies, and region-reads it
  and has no writer** (`engram-20261109T232921Z.json`,
  [`bench/RESULTS-engram.md`](../bench/RESULTS-engram.md)).
- **`weft.transport.v1`** — a 4,096-byte header and a section table over
  unmodified enclosed frames, deterministic align-up layout, and a
  `payload_hash` over the enclosed bytes. It costs bytes (0.064 % / 0.302 % /
  4.374 % padding at 512/4,096/65,536-byte alignment on real frames), a
  64 KiB alignment can add up to 64 KiB per section, a single-section reader
  cannot verify `payload_hash`, and a sub-section slice is not independently
  rooted (`transport-20261109T234518Z.json`,
  [`bench/RESULTS-transport.md`](../bench/RESULTS-transport.md)).
- Conformance: **420 vectors**, aggregate
  `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`;
  Rust and Go adapters execute modes 0–20, and the independent Python reader
  covers every allocated mode 0–18 (19–20 rejected `UnsupportedMode`).

## Performance and scale results

The release adds several measured losses, and they are the point:

- **Same-backend augmentation is expensive.** On the 424,439,296-byte Qwen
  fixture (local backend), configuration A writes in **335.9 ms** while B
  writes in **2,852.7 ms** and E writes in **24,789.2 ms**; C and E save
  **33.1 % / 32.9 %** of physical bytes but restore 3.0–4.1x slower and use
  714 MB of restore RSS against A's 269 MB (`augmentation-local-20261109T134132Z.json`,
  `augmentation-minio-20261109T134257Z.json`,
  [`bench/RESULTS-augmentation.md`](../bench/RESULTS-augmentation.md)). A
  plain-files-plus-manifest implementation (N) is within 0.5 % of A, which is
  the honest comparator for “storage with metadata”.
- **Concurrency does not scale in MW2** (4,000 → 580.3 tx/s, above), and the
  aggregate restore is bound by the largest shard, not the largest tensor, only
  after R13; before it, one mutex capped the whole path.
- **7B ingest does not fit** the default court (OOM, exit 137); packed writes
  cost +51.0 % store bytes (23,023,484,778 vs 15,247,327,175) and restore
  reaches only 1.089x against a 13.97x cap
  (`scale-7b-20261110T213731Z.json`).
- **Small regions stay serial.** Tiled region reads, per-tensor parallel
  restore, and the read pool all have fixed costs; the receipts above publish
  the small-region cases where parallelism loses.

Single host, warm caches, synthetic or public-weight fixtures. No fleet,
device, or cloud claim follows from any of these numbers.

## Go parity

| Surface | State at v1.2.0 | State at v1.3.0 | Evidence |
|---|---|---|---|
| Azure Blob backend | absent | **implemented**, SharedKey/SAS, measured `If-Match` form, v3 fenced `LOCK`, two-way handoff | `azure-court-20261111T144613Z.json`, `azure-20261111T144613Z.json`, [`bench/RESULTS-azure-go.md`](../bench/RESULTS-azure-go.md) |
| Read handles | one backend mutex per read | **bounded pool (64 idle)** with `stat` validation before reuse | [`bench/RESULTS-go-read-pool.md`](../bench/RESULTS-go-read-pool.md) |
| KV modes 12/19 | codec only | **store write/read/authorize/verify** | `kv-go-parity-fuse-20261111T154406Z.log`, [`bench/RESULTS-kv-go-parity.md`](../bench/RESULTS-kv-go-parity.md) |
| MW2 on `azure` | refused | refused (`Unsupported`) | asserted by test |

The Azure court is **Azurite 3.35.0 only**. It is a well-behaved emulator plus
two adversarial endpoints the court injects; it is not Azure, and no live
Azure account was used. Go's read-pool measurement is descriptor cost only
(6,939/7,240/8,420 ns/op unpooled against 6,321/5,156/5,334 ns/op pooled on a
73,728-byte file); the spread is wide and no restore-throughput claim is made.

Remaining Go gaps, stated plainly: **KV prefix tier**, **Ed25519 envelope
verification**, **parent-graph planner**, **MW2 on `azure`**, and **writable
FUSE over object stores**. Go has no mode-20 writer.

## Verification

No aggregate exists yet for the v1.3.0 tree; the verification pass records it
and replaces the PENDING line above. The state this release is built on:

- **45/45** courts pass at the R13 tree (`verify-20261111T131235Z.json`):
  `rust_codec`, `rust_store` (771 passed / 158 suites), `go_codec`,
  `rust_fuse` (814 passed / 0 failed / 2 pre-existing ignored),
  `go_store_and_fuse`, the four codec cross courts, `spec_drift`, `claims`,
  `conformance_manifest`, `conformance_coverage`, `generated_types`,
  `buck2_cas`, `buck2_cas_store`, `threefs`, `checkpoint_family_lifecycle`,
  `trajectory`, `write_policy`, `recovery_load`, `kv_capability`,
  `parallel_checkpoint`, `throughput`, `restore_scaling`, `index_trust`,
  `agent_memory`, `ceph_rgw`, `s3_court`, `augmentation_local`,
  `conformance_bundle`, `conformance_python`, the four Context Root cross
  courts, the five authenticated-capability cross courts, `fuzz_diff`, `tla`,
  `multiwriter`, and `multiwriter_failure`.
- The **44/45** run that found the packed-write regression is retained:
  `verify-20261111T103327Z.json` (raw log beside it).
- The aggregate immediately before it, **42/42**, is
  `verify-20261110T234609Z.json`; the v1.2.0 release tree was **37/37**
  (`verify-20261109T112958Z.json`).
- Conformance: 420 vectors, Rust and Go 288 pass / 0 fail / 132 skip in the
  bundle court, Python 229 / 0 / 191 with every skip itemised.
- Crash: 100,000/100,000 process-fault iterations, 0 partial, 0 bad
  (`crash-torture-20261104T104233Z.json`); **not** physical power loss.

Host-only gates run for this candidate: `scripts/check-claims.sh` (920
entries), `scripts/check-spec-drift.sh` (60 comparisons across three repos),
and `scripts/gen-conformance-manifest.sh --check` (420 entries). The Python
suite (369 passed / 71 skipped) and `cargo check --locked` pass in the python
and rust courts after the 1.3.0 lockfile regeneration.

## Distribution and artifacts

- Version `1.3.0` in `Cargo.toml`, `WeftCodec/Cargo.toml`,
  `python/pyproject.toml`, `python/weftcodec/__init__.py`, and Go's
  `storeVersion`. Both `Cargo.lock` files were regenerated by `cargo check`
  without `--locked`; the only lock change is the two version stanzas.
- The v1.3.0 tag, the four GitHub releases, the four GHCR images, the
  `weftcodec`/`weftfs` crates, and the PyPI wheel and sdist are **cut by this
  release's publishing step**, not before it. The v1.2.0 artifacts remain
  published. The previous release's artifact record is
  `release-artifacts-20261109T124325Z.json`; the v1.3.0 artifact receipt is
  produced by the publishing step.
- The **published 1.2.0 PyPI long description is stale** (it still says PyPI is
  unpublished and stops at mode 16). It is not re-uploaded; the 1.3.0 wheel
  rebuilds the description from `python/README.md`.
- `python/tests/test_version.py` now fails the suite if
  `weftcodec.__version__` disagrees with `python/pyproject.toml` (or with
  installed distribution metadata when the package is installed). It exists
  because `__version__` was left at `1.1.0` through the 1.2.0 release.

## Not claimed

- **MW3**, distributed consensus, conflict domains, sharded heads, coordinated
  multi-writer GC, and any multi-node publication.
- **MW2 on Azure**, MW2 for FUSE mutations, and MW2 for generic checkpoint
  publication.
- **Go KV prefix tier**, **Go Ed25519 envelope verification**, the **Go
  parent-graph planner**, a **Go mode-20 writer**, and **writable Go FUSE over
  object stores**.
- **Live AWS, Azure, or Google Cloud Storage.** The evidence is MinIO, Ceph
  RGW 19.2.0, `fake-gcs-server`, and Azurite 3.35.0.
- **KV serving connectors** (no vLLM/SGLang integration, no TTFT claim) and any
  production serving path.
- **Physical power loss, host page-cache loss, RAPL/joule energy, GPU/TPU
  execution**, and any deployed fleet or 3FS cluster.
- **Performance superiority** over any external system. Every result above is
  single-host, warm-cache, and includes its losses.
