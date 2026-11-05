# WeftFS v0.7.0 — real weights, checkpoint topology, quotas, 1,000 mounts, Merkle identity, CAS, and conformance

Research preview. v0.7.0 is the P2–P8 subphase release: real-model codec
measurements, topology-independent checkpoint restore through a normal mount,
commit-time write budgets and branching, a 1,000-mount scale court with its
break point, an opt-in Merkle content identity, the Buck2 CAS moved onto the
WeftFS store, a training-ready refusal contract, and a third-party conformance
manifest with reference runners.

This is an **additive** release. Frozen v1 frame and store formats,
`weft.capability.v1` and `weft.capability.envelope.v1` records, selector
grammar, and the default `raw` representation are unchanged. Whole-file v1
content identity remains the default; `weft.manifest.v3` Merkle identity is
opt-in.

Everything below is measured in the Docker courts with the hard limits in
`AGENTS.md`. No claim is a fleet, production, physical-media power-loss, RAPL,
GPU, or hardware-independent claim. Every loss we know about is stated.

## Version and artifacts

```toml
weftcodec = "0.7.0"
weftfs = "0.7.0"
```

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.7.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.7.0
docker pull ghcr.io/infinityabundance/weftfs:v0.7.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.7.0
```

- Rust: `weftfs` 0.7.0 and `weftcodec` 0.7.0 in both `Cargo.toml` and
  `Cargo.lock`; the parent depends on `weftcodec = "0.7.0"`. The bump changed
  version literals and both lock files only.
- Python: `python/dist/weftcodec-0.7.0-py3-none-any.whl`, **27,073 bytes**,
  sha256 `8064f27d46f171aca009bd2075a7e76dc7a7d8aa8d1ae9377b25ebcef3dd9f56`.
  Built in the Python court and tested from the installed wheel:
  166 tests, 0 failures, 0 errors, 51 skipped
  (`bench/receipts/python-bindings-20261105T175700Z.json`). The Python package
  now reports version 0.7.0. **Not published to PyPI** (no token); the wheel is
  attached to the GitHub release.
- Images are published by each repository's `container.yml` on the release tag.
  FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and
  store-only commands do not. See `docs/containers.md`.

## Verification at the release tree

`bench/receipts/verify-20261105T175154Z.json` — **27/27 courts `pass`**, run
from WeftFS `067bb88` before the version bump:

- `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`, `go_store_and_fuse`;
- codec cross-decode in both directions plus byte comparison;
- `spec_drift`, `generated_types`, `claims`, `conformance_manifest`,
  `conformance_coverage`;
- Buck2 CAS unit court and store-backed demo;
- agent memory, context cross-language gates, capability cross-language gates;
- `fuzz_diff` (1,000,000 differential inputs).

Revisions pinned by that receipt:

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `067bb88` | program home, Rust store + FUSE, specs, courts |
| `WeftCodec` | `b15353a` | Rust codec, AI modes, checkpoint/capability support |
| `WeftCodec-Go` | `9d275c0` | independent Go codec and capability parity |
| `WeftFS-Go` | `4278445` | independent Go store + FUSE, spec mirror |

The tag is cut after the version and documentation commits, so the tagged
commits are later than the verified code revisions; the only changes between
them are version literals and documentation.

The host-only `claims` court checks the documentation claims in
`docs/claims.json` against the files that carry them. After the v0.7.0 entries
were added, the gate was re-run on the release tree: **102 entries OK**.

## Real weights: FLOAT_TENSOR and WEIGHT_DELTA (P2)

Receipts: `bench/receipts/real-float-tensor-20261105T053958Z.json`; analysis
`bench/RESULTS-float-tensor-real.md`. Corpus re-hashed in the court:
Qwen2.5-0.5B and -Instruct (BF16, 290 tensors, 988,097,824 bytes each,
`88c14255…0ed342` / `fdf756fa…88fb7fe`) and EleutherAI/pythia-70m step0/1/2/4/8
(F32, 76 tensors, 281,715,176 bytes each). Both are codec-level per-tensor
measurements, not the store planner.

### BF16: a win against per-tensor zstd -19, a tie with ZipNN

| Checkpoint | FLOAT_TENSOR | per-tensor zstd -19 | ratio | ZipNN 0.5.4 |
|---|---:|---:|---:|---:|
| Qwen2.5-0.5B | 657,975,743 B | 768,347,895 B | **0.8564** (14.4 % smaller) | 658,281,885 B |
| Qwen2.5-0.5B-Instruct | 657,891,990 B | 767,597,259 B | **0.8571** (14.3 % smaller) | 658,236,344 B |

169/290 tensors are no larger than zstd -19; **all 121 tensors of at least
1 MiB win**. The worst absolute loss is +663 B on a tiny layernorm. Against
ZipNN, mode 9 is a tie: smaller by 306,142 B and 344,354 B (0.05 %).

### F32: a real loss

| pythia-70m step | FLOAT_TENSOR | per-tensor zstd -19 | ratio | ZipNN |
|---|---:|---:|---:|---:|
| step0 | 233,958,945 B | 144,397,747 B | 1.620 | 119,680,613 B |
| step1 | 233,958,945 B | 144,403,851 B | 1.620 | 119,680,613 B |
| step2 | 233,963,826 B | 144,272,053 B | 1.622 | 119,682,001 B |
| step4 | 233,965,750 B | 144,303,717 B | 1.621 | 119,685,713 B |
| step8 | 233,963,977 B | 144,464,688 B | 1.620 | 119,681,706 B |

FLOAT_TENSOR is **62 % larger than per-tensor zstd -19** and **95.5 % larger
than ZipNN** on F32; 0/76 tensors win. The published pythia F32 values are
fp16-precision widened to f32 (byte-plane probe: plane 0 entropy 0), and mode 9
stores sign+mantissa raw, so it cannot remove that redundancy. Worst absolute
loss: +33.37 MB (`embed_out`).

### WEIGHT_DELTA: strong on consecutive checkpoints, weak on a 19-byte change

| Pair | Changed fraction | Delta bytes | vs per-tensor zstd -19 | vs best `--patch-from` |
|---|---:|---:|---:|---:|
| pythia step0→1 | 0.000027 % (19 words) | 7,434,959 | 5.1 % | **245.66× worse** (patch 30,265 B) |
| pythia step1→2 | 5.14 % | 28,030,116 | 19.4 % | 0.41× |
| pythia step2→4 | 21.06 % | 33,412,606 | 23.2 % | 0.30× |
| pythia step4→8 | 77.59 % | 49,198,709 | 34.1 % | 0.35× |
| Qwen base→instruct | 97.93 % | 541,188,577 | 70.5 % | 0.72× |

Consecutive checkpoints (excluding the 19-byte step0→step1 artifact) are
**19.4–34.1 % of independent zstd -19**, and 2.4–3.3× smaller than the best
`zstd --patch-from` on those meaningful pairs. The independent fine-tune is
**70.5 %** of zstd -19. The 19-byte pair is reported separately: there mode 11
loses 245.66× to `--patch-from`, because every non-constant Huffman plane costs
at least one bit per element. Consecutive and fine-tune results are never
aggregated.

### Restore cost: delta wins bytes, loses time

Decode is single-threaded, median of five reps per tensor, summed over tensors,
excluding storage I/O:

- FLOAT_TENSOR is **6.7× (F32) to 12.0× (BF16) slower** than zstd;
- pythia deltas are **16.1–20.6×** slower;
- the Qwen fine-tune delta is **44.0×** slower;
- restoring the Qwen Instruct delta reads 1,199,164,320 B against zstd's
  767,597,259 B, because the base frame is read too.

`zstd --patch-from -19` on Qwen was OOM-killed at the 3 GiB court limit
(3,128,344 KiB, 509.6 s); the -12 baseline was used. ZipNN is whole-file only
and not delta-aware. All runs are on one host with a warm page cache.

## Topology-independent checkpoint restore (P3)

Receipt: `bench/receipts/checkpoint-topology-20261105T062409Z.json`; analysis
`bench/RESULTS-checkpoint-topology.md`; design `docs/checkpoint-topology.md`.

The Qwen2.5-0.5B container (BF16, 290 tensors, source SHA-256
`88c14255…0ed342`) was written as **8 shards per tensor** in one group commit:
41.53 s, 582,028 KiB peak RSS, 2,320 shard objects, 825,180,192 store bytes
(0.835 of source). Shard payloads are exact q8_0 byte words, not quantized
weights.

- **Every restore topology reproduces the source.** The topology test matrix
  covers 8/4/2/1-way sharded and row-major, column-major, and blocked re-layout
  proofs, with 290/290 logical roots identical; the receipt's full restores
  are the 8-way and 4-way cases, and each reproduces the source SHA-256 and
  header.
- **Through an ordinary mount.** An unmodified PyTorch-ecosystem loader
  (`safetensors` 0.8.0 + NumPy 2.4.6 + ml_dtypes 0.6.0) read **290/290 tensors**
  from the mounted file with matching per-tensor SHA-256 (13.61 s); the mount
  daemon's RSS rose from 27,224 KiB to 39,320 KiB.
- **Region restore reads less.** A 100-row slice inside shard 0 read one shard
  and **28,053,320 B** instead of the full **224,668,493 B** (0.89 s vs 15.26 s,
  **8.0× fewer bytes**).
- Restore timing: 111.50 s for the first restore after ingest (topology 8),
  then 20.93 s (topology 4); warm repeats 20.44 s and 19.89 s. The first number
  includes host writeback state and is labelled as such.

Losses: Rust-only (the Go store rejects `weft.checkpoint.v1`); mode-10 shard
frames are not seekable, so a slice reads only intersecting objects but decodes
each whole shard; restore spools to a host file; one host, warm cache.

## Commit-time quotas and branching (P5a)

Spec `spec/WEFT-QUOTA.md`; operations `docs/quota-branching.md`; benchmark
`bench/receipts/quota-branch-20261105T071244Z.json`; analysis
`bench/RESULTS-quota-branch.md`.

- **Budgets are enforced before publication.** An over-budget write fails with
  the fixed, non-disclosing `BudgetExceeded: write budget exceeded`; no usage,
  limit, or principal is disclosed, and generation/head/state are unchanged.
- **Usage is durable and committed.** The `quota` record names the committed
  generation and head, survives reopen, and fails closed on mismatch until
  `quota rebuild`. Tenant attribution uses the reserved `weft.tenant`
  attribute.
- **Measured commit overhead.** 1,000 single-object commits (1 KiB each):
  p50 13.754 ms with budgets vs 11.348 ms without, **+2.406 ms, +21.2 %**
  (ratio 1.212). The quota run rewrites the quota record after every commit
  (tmp+fsync+rename+directory fsync), which is the honest cost of restart-safe
  usage.
- **Branching.** 50 branches cost p50 9.11 ms each. In the 10,000-version demo,
  a branch was taken from snapshot `s5000`
  (`72ace031…0443cc7d`); five later writes advanced the live head to version
  10005 while `s5000`, `s10000`, and the branch ref stayed byte-identical.
  Four mounts (source snapshot, descendant snapshot, branch snapshot, branch
  Context Root) read the fork exactly, and 0 daemons survived unmount.
- **Union merge.** 1,000 disjoint paths merged in 255.64 ms (generation
  3003→3004) with `verify --full` clean. Identical content dedupes, different
  content on the same path fails closed with `Conflict` naming the path, and
  the live head is never overwritten. It is a union merge, **not** a three-way
  content merge.

Loss: the +21 % per-commit cost is the price of durable usage; the FUSE write
path does not yet set `weft.tenant` (API callers and fixtures can).

## 1,000 concurrent context mounts, and where it breaks (P5b)

Receipts: `context-mounts-20261105T122643Z.json` and
`…131116Z.json` (5/5 each, `semantic_match=true`); analysis
`bench/RESULTS-context-mounts-1000.md`.

The store holds 2,000 corpus files, 250 model objects, 10 grounding documents,
5,999 typed relations, and 1,000 distinct Context Roots (2,264 objects / 6,999
relations as counted by `weftfs info`). 1,000 daemons alternate Rust and Go.

At **52 GiB / 8 CPUs / 16,384 pids** the court completed **twice**:

- 1,000/1,000 mounts and 1,000 pairwise-distinct listing digests matched the
  construction oracle;
- 4,250/4,250 mounted files matched their declared SHA-256;
- 1,000/1,000 `weftfs cat` outputs matched; 20/20 sampled `context resolve`
  calls matched id-for-id;
- 1,000/1,000 clean unmounts, 0 surviving daemons, 0 OOM kills, 0 pids-limit
  events;
- peak cgroup 40.92 GiB / 40.22 GiB; daemon RSS sum 46.44 / 45.89 GiB; max
  80.5 / 81.3 MiB per daemon; 6,751 / 6,771 tasks;
- mount p50 197.0 / 195.2 s, p95 330.2 / 328.3 s, max 331.7 / 329.7 s; all
  mounted+verified 1,785.8 / 1,916.1 s; run wall 2,636 / 2,769 s.

At **24 GiB / 8 CPUs / 4,096 pids** it **does not hold 1,000**:

- 1,000 requested → 747 spawned → 692 mounted → 55 mount failures, 107 cgroup
  OOM kills, 98 pids-limit refusals, 156 daemons lost, peak 24.00 GiB;
  **INCOMPLETE 692/1000** (2/5 steps).
- 500 mounts all mounted and all file/cat checks passed, but 4 Go daemons died
  mid-run, leaving 496 clean unmounts (3/5).
- **400 is the largest size that passes every step at those limits** (400/400,
  1,700 file hashes, 400 distinct digests, 400 clean unmounts, peak 11.72 GiB).
- **Pids binds before memory.** At `pids.max=4096` spawning stopped at mount 747
  with 3,841 tasks; Go daemons failed with
  `fatal error: newosproc` / `failed to create new OS thread`, Rust daemons with
  `fuse: fork() failed: Resource temporarily unavailable`.
- Daemon RSS scales with the pinned snapshot state: mean 17.7 MiB at 235
  objects, 31.1 at 914, 35.6 at 1,139, 47.5 at 2,264. The old 16 MiB/mount
  extrapolation understated 1,000 by about 3×.

A **shared multi-mount daemon** (one process serving many contexts) is required
to fit many mounts inside the default 3 GiB / 1,024-pid court; 1,000
single-context daemons are not claimed to fit it. This is a single-host court
with synthetic fixtures; it is not a fleet claim.

## Opt-in Merkle content identity (P4)

Spec `spec/WEFT-MODEL.md` §2.3.2; store notes `docs/store-status.md`; receipt
`bench/receipts/merkle-identity-20261105T161026Z.json`; analysis
`bench/RESULTS-merkle-identity.md`.

The frozen v1 `ContentId` hashes the whole plaintext, so every range write
re-read the whole file. `--content-identity merkle` writes
`weft.manifest.v3`, whose root folds the chunk hashes
(`H("weft/merkle/leaf/v1", u64le(len)||chunk_hash)`,
`H("weft/merkle/node/v1", l||r)`). Measured on a 128 MiB file at 64 KiB chunks
with 500 deterministic 4 KiB overwrites:

| Harness | identity | p50 | p95 | max | `rchar` | chunk reads | peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| Rust | whole-file v1 | 50.43 ms | 53.09 ms | 54.13 ms | 67.20 GB | 1,024,537 | 6,616 KiB |
| Rust | Merkle v3 | **1.81 ms** | 1.87 ms | 2.16 ms | **87.91 MB** | **537** | 6,796 KiB |
| Go | whole-file v1 | 202.73 ms | 210.86 ms | 251.02 ms | 134.29 GB | — | 437,328 KiB |
| Go | Merkle v3 | **4.38 ms** | 4.66 ms | 5.37 ms | **87.91 MB** | **537** | **43,392 KiB** |

Rust p50 improves 27.8×, Go 46.3×; bytes read fall 764× and 1,528×; Go peak RSS
falls 10.1×. `wchar` is flat (~89 MB): the gain is reads, not writes. The root
still folds all 2,048 leaves per write (O(#chunks), not O(1)), changing
`chunk_size` changes the root, and there is no v3→v1 conversion command. **The
default remains whole-file v1.**

## Buck2 CAS over the WeftFS store (P6)

Spec `spec/WEFT-BUCK2.md`; receipt
`bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json` (41/41) and
`bench/receipts/buck2-cas-store-20261105T163930Z.json`; analysis
`bench/RESULTS-buck2-cas-store.md`.

`ActionKey = H("weft/buck2/action/v1", WCB({action_type, toolchain, sorted input
ContentIds, canonical params}))`; input paths are metadata, not key material.
The CAS address is the WeftFS `ContentId`; the record is canonical WCB under
`/cas/records/<key>`; `cas_action` / `cas_output_of` edges make
`by:action=<key>` a real selector; a mount of that Context Root reads the
artifact.

- Same declaration → same key; same key + same bytes is idempotent; same key +
  different bytes is `Conflict` **before any mutation**, with the existing
  output still fetching.
- Cross-language both ways: Rust `verify --full`/`cat`/`stat`/`context resolve`
  accept a Go-written store, and the Go adapter reads a Rust-written store.
- GC collected 4 files / 869 bytes for an unreferenced action while a
  named-snapshot + Context Root output stayed exact.
- Bench: 1,000×128 B and 100×1 MiB. `store_new` p50 33.34 / 80.25 ms (four
  fsync-heavy commits per action); hit 0.074 / 0.790 ms; fetch+verify 0.071 /
  1.330 ms; miss 0.0047 / 0.0045 ms. Physical 144,961,123 B. Mount read of one
  1 MiB artifact p50 **1.236 ms**; CLI fetch p50 367.75 ms is explicitly a
  process-open measurement.

**This is not a Buck2 plugin, not REAPI, and not gRPC. Buck2 itself was not
run** (only `--version` would have been). No remote-execution or worker claim.

## Training-ready refusal (P7)

Spec `spec/WEFT-TRAINING-READY.md`; code `src/training_ready.rs`; demo receipt
`bench/receipts/training-ready-20261105T171104Z.json`.

`weft materialize --root N --training-ready --out DIR` publishes **no output**
unless all six preconditions hold at the pinned snapshot: exact untruncated
resolution; verified reads of every object; complete causal closure; capability
authorization; tensor-root and full checkpoint decode/reassembly under
`--require-tensors`; and a byte/object/decode-ms envelope. A failure names the
class (`DependencyMismatch` for a missing causal parent) and writes nothing.
`weft training-ready --root N --check` and `--verify-out DIR` are the
inspection paths.

Qwen2.5-0.5B BF16 demo in the FUSE court (290 tensors, 988,065,536 bytes, two
shards, 580 shard objects):

| Step | Wall | Peak RSS |
|---|---:|---:|
| checkpoint write | 66.35 s | 1,174,908 KiB |
| training-ready check | 77.25 s | 552,156 KiB |
| materialize | 98.14 s | 552,236 KiB |
| offline verify-out | 0.76 s | 543,892 KiB |

The check and materialize verified 581 objects / 988,283,592 B and all six
preconditions; the source SHA-256 matched. The declared cost estimate (30,034
ms) under-predicted the actual decode (77,099 ms) by 2.57×; it is a declared
throughput model, not calibrated. Output is decoded shard files plus canonical
`manifest.wcb` and `training-ready.wcb`, not one restored container. Rust-only.

## Conformance manifest and reference runners (P8)

Docs `docs/CONFORMANCE.md` and `docs/WRITE-YOUR-OWN-DECODER.md`; manifest
`conformance/manifest.json`.

- **295 vector entries**, aggregate
  `3c981f03fca856c0632ec48d5be166a1ae6513f87b393f934f3555482c2c0bda`;
  288 files shared byte-for-byte between `WeftCodec` and `WeftCodec-Go`
  (aggregate `5f428820e4ad6a2aa9dcf107352106f1db059187db07ce6e1f89c6e2fb04eab1`).
- Reference runners: **Rust 206 pass / 0 fail / 89 skip**, **Go 206 / 0 / 89**,
  **Python 56 / 0 / 239**. Skips are explicit, reasoned in the receipt, and
  never counted as passes: they are vector kinds whose public CLI has no verb
  (raw WCB, checkpoint manifests, record-level rejects, archives, layout-only
  tensor descriptors, semantic roots, composite fixtures).
- The coverage gate requires at least one valid and one reject vector for every
  frame mode 0–14 and at least one valid vector for every identity kind; the
  manifest drift check and the gate are both courts in `verify-all`.
- Receipts: `conformance-rust-20261105T180000Z.json`,
  `conformance-go-20261105T180100Z.json`,
  `conformance-python-20261105T175000Z.json`.

Loss: the reference runners are CLI-contract evidence, not proof that every
vector is replayed through every language's public surface; the implementation
test suites replay the skipped families directly, and that distinction is
stated rather than blurred.

## State of the rest of the program

Nothing else changed after the v0.6.0 tag. All v0.6.0 results still stand:
authenticated capability envelopes and cross-tenant isolation, tensor
representations and the cost-aware planner, provenance receipts and the
recovery planner, Local/Object backends, agent memory, the Qwen storage
comparison, the 100,000-iteration crash proxy, the 1,202,560-operation POSIX
torture, and the block-layer/power-loss proxies.

The previously reported gaps are unchanged and remain the top follow-ups:

- FUSE write throughput remains dominated by commit/fsync costs; Merkle identity
  is opt-in and does not change the default.
- Go has no `weft.checkpoint.v1`, training-ready, or `weft.provenance.v1`
  parity; no persisted Go selector index or bounded commit index.
- Region reads of delta/Huffman tensor frames decode the whole frame; planner
  costs are uncalibrated estimates.
- The 1,000-mount court needs explicit overrides and a shared multi-mount
  daemon is future work.
- Physical media power loss, host page-cache loss, and RAPL energy are not
  measured or claimed.
- No 3FS run; the POSIX backend remains a capability configuration.

## Still not claimed

- **No physical media power loss.** Crash courts are process-fault, QEMU, and
  dm-flakey/blkdebug device-model proxies; host page cache was not dropped.
- **No RAPL energy.** `/sys/.../energy_uj` was unreadable in the court; CPU
  user/sys time is the supported metric and is not called energy.
- **No GPU or SIMD path.** All codec and store work is plain single-threaded
  CPU code; ZipNN/DFloat11 comparisons are external baselines only.
- **No 3FS run.** No 3FS deployment or benchmark exists.
- **No PyPI publication.** No token; the wheel is a release asset.
- **No multi-writer or distributed store.** Single-host, single-writer with
  `flock`.
- **No Go parity for checkpoint or training-ready.** Those paths are Rust-only.
- **No O(1) region read.** Region reads of mode-10/11/14 frames decode whole
  shard/frame payloads; Merkle fold is O(#chunks).
- **No calibrated planner.** Cost estimates are declared throughput models, not
  measurements against every backend.
- **No fleet claim.** Every scale number is one host, one court, synthetic or
  public fixtures as labelled.
- **No Buck2/REAPI plugin claim.** The adapter is a local store-backed CAS
  library and CLI; Buck2 was not run.
