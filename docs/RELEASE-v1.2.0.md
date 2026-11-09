# WeftFS v1.2.0 — bounded 3B ingest, planned checkpoint graphs, a measured restore floor, and a published SLA failure

Research preview. v1.2.0 is the first release after v1.1.0. It changes **no
frozen wire or on-disk byte**. The WFT1 frame, codec payload modes 0–17, every
conformance vector, canonical WCB, `weft.manifest.v1`–`v4`, the selector
grammar, the store layout, the superblock and recovery protocol, and the
frozen store fixtures are unchanged.

The codec gained exactly one **additive** mode since v1.1.0: mode 18
`FLOAT_TILED_STREAMS`. It is part of v1.2.0, it is never auto-selected without
`--allow-experimental-modes` (or an explicit strategy hint), and modes 0–17 are
byte-identical to v1.1.0. `n_streams = 1` is byte-identical to mode 17's stored
tile payload; the authoritative reserved-mode reject moved from 18 to 19.

One on-disk addition is **additive** rather than a change to a frozen format:
new commits may carry `weftfs.commit.v2` with an `index_attestation`.
`weftfs.commit.v1` remains fully valid, a v1 commit's selector index is never
trusted, and the frozen conformance store fixture still reproduces v1
byte-for-byte via `attest_indexes: false`.

The version bump is package metadata plus the tool-version field in newly
written provenance records. `1.2.0` means “the release after 1.1.0”, not
production readiness. Every measured loss, every non-claim, and one published
SLA miss are part of this release.

Everything was built and measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

## Verification at the release tree

**Aggregate.** `bench/receipts/verify-20261109T112958Z.json` —
**37/37 courts `pass`, 0 failed, 0 skipped**, at:

| Repo | Verified revision | `v1.2.0` tag peels to |
|---|---|---|
| `WeftFS` | `135a3820df6769dd44bc47fd84470b345c2beef6` | `122d86e70927899ee179bb032e5253887f9cc76a` |
| `WeftCodec` | `d95fa78c805eef43b32c3da5025981eab7325513` | `d95fa78c805eef43b32c3da5025981eab7325513` |
| `WeftCodec-Go` | `0db665ea4c6b1d69d1d1640801ed475848d361f1` | `0db665ea4c6b1d69d1d1640801ed475848d361f1` |
| `WeftFS-Go` | `6d58137ca888c0b10d01f2fd93f3af00d544e5f6` | `6d58137ca888c0b10d01f2fd93f3af00d544e5f6` |

The verified parent tree is `135a382`; the tag itself is one evidence-only
commit later (`122d86e` adds this receipt and its per-court receipts; no source
file differs). `staging == main` in all four repositories at the tagged
revisions. Later commits on `staging`/`main` add this release note, the
STATUS/README refresh, the claims and the artifact receipt; they change no
source, wire, or on-disk byte and are not part of the tagged tree.

Courts: `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`,
`go_store_and_fuse`; the four codec cross-encode/decode/byte-comparison courts;
`spec_drift`, `claims`, `conformance_manifest`, `conformance_coverage`,
`generated_types`; `buck2_cas`, `buck2_cas_store`; `threefs`;
`checkpoint_family_lifecycle`, `trajectory`, `write_policy`, `recovery_load`,
`kv_capability`, `parallel_checkpoint`, `index_trust`; `agent_memory`;
`ceph_rgw` (Ceph RGW 19.2.0, S3 backend and fenced-writer suite); the MinIO
`s3_court`; the four Context Root cross-language courts; the five
authenticated-capability cross-language courts; and `fuzz_diff` (1,000,000
differential inputs, identical classes, no panics).

The long courts run reduced configurations inside the aggregate, recorded in
their own receipts: lifecycle one family/one configuration/one rep; trajectory
two pythia steps and eight synthetic steps with no Qwen half; write policy two
configurations; KV the minimum accepted 1,000 samples. The full-size standalone
receipts are cited below. No court failed and no rerun was needed. The
immediately prior aggregate, `verify-20261108T165021Z.json` (37/37 at
WeftFS `94a4818`), predates S12 and S13b and is **not** cited for them.

## Version bump and byte identity

Changed, and nothing else:

- `Cargo.toml`: `weftfs` `1.1.0 -> 1.2.0`; `weftcodec = "1.2.0"`.
- `WeftCodec/Cargo.toml`: `weftcodec` `1.1.0 -> 1.2.0`.
- `Cargo.lock` and `WeftCodec/Cargo.lock`: the `weftfs`/`weftcodec` package
  entries only.
- `python/pyproject.toml` and `python/README.md`: `1.2.0`.
- `WeftFS-Go/store/rebase.go`: `storeVersion` follows the release, the tool
  version recorded inside generated rebase provenance. Rust uses
  `env!("CARGO_PKG_VERSION")`, so the two agree.

**No frozen wire or on-disk byte changed.** Proof at the bumped tree:

1. **Codec cross gate** (`codec_cross_*` courts in the release aggregate):
   Rust wrote 47 frames, Go wrote 47 frames, Rust decoded 38 Go frames, Go
   decoded 38 Rust frames; **105 `PASS` lines** and **14 `YES byte-identical`**
   lines (BF16/FP32 tiled, sparse deltas, KV/activation, semantic deltas).
   zstd and dictionary are decode-equivalence modes and are labelled as
   differing bytes by spec §12. `cross-language gate: all checks passed`.
2. **Conformance**: the committed manifest has 355 entries and
   `conformance_manifest` and `conformance_coverage` both pass in the
   aggregate. The manifest aggregate and every vector byte are unchanged.
3. **Mode 18 vectors** are additive: 4 valid + 12 reject vectors under
   `conformance/v1/float-tiled-streams/`, byte-identical in both codec repos;
   coverage reports `18:4/12`; the Python reader executes every mode 0–18
   vector.

No fixture, vector, manifest, commit record, superblock, or chunk byte was
edited. The new `weftfs.commit.v2` schema is exercised only by newly written
commits; the frozen fixture opts out and remains v1.

## What changed, S7–S13b

### S7 — mode 18 `FLOAT_TILED_STREAMS` (additive; kept experimental)

Mode 18 is the multi-stream form of mode 17: the same tile grid, plane split,
offsets and per-tile roots, with each tile's Huffman exponent plane split
round-robin into `1..=16` independently coded `huffman-canonical-v1` streams and
its raw mantissa plane split into the same number of raw streams. The tile
entry carries a `2 * n_streams` u64 `stream_offsets` table, and the tile root
covers dtype, dims, element length, plane lengths, `n_streams`, that table and
the values hash. The wire schema is `weft.ai.tensor_tiled_streams.v1`.

Court (`bench/receipts/mode18-20261107T165215Z.json`,
`bench/RESULTS-mode18.md`): real Qwen2.5-0.5B BF16 embedding `[151936,896]`,
tile `[64,896]`, 259.66 MiB.

| Configuration | Physical bytes | 1 thread | 4 threads | 1-row read |
|---|---:|---:|---:|---:|
| mode 9 `FLOAT_TENSOR` | 180,623,816 | 840.1 MiB/s | 843.6 MiB/s | 309.1 ms |
| mode 17 `FLOAT_TILED` | 181,541,557 | 715.9 | **1,828.3** | **0.133 ms** |
| mode 18, n=1 | 181,598,555 (+0.03 %) | 602.1 | 1,800.4 | 0.256 ms |
| mode 18, n=2 | 182,211,848 (+0.37 %) | 453.5 | 1,501.0 | 0.321 ms |
| mode 18, n=4 | 183,446,037 (+1.05 %) | 428.2 | 1,390.3 | 0.336 ms |
| mode 18, n=8 | 185,923,029 (+2.41 %) | 408.2 | 1,342.7 | 0.395 ms |

**Multi-stream lost every measured axis**: 1.5–26.6 % slower at four threads,
15.9–43.0 % slower at one thread, +0.03–2.41 % larger, and 1.5–3.0x slower on
small regions. The measured cause is the wide-word gather kernel and a full
deinterleave pass, not the wire format. The bytes-only default never selects
it. A small-region access plan *can* select it because its fitted fixed term is
0 while mode 17's is 14,131 ns; the 1-row measurement contradicts that model,
so the selection is documented as a model artifact, not a latency claim.

Losses and non-claims: kept experimental; no GPU path was built or executed;
one host, one public tensor, warm caches; `n_streams` cost rows are single-thread
measurements; Go has no mode-18 store region path (whole decode then slice).

### S8 — write policy, transcode, trajectory, load receipts, recovery-first (`STATUS.md` T1)

**Write policy.** `--write-policy fast|planned|auto`, `--fast-codec raw|zstd`,
`--fast-zstd-level`, `--critical-path`, `--deadline-ms`. `Fast` restricts the
candidate set to raw chunks or one ZSTD frame and never runs a
mode-9/10/15/16/17/18 or delta encoder. Qwen2.5-0.5B base + Instruct, 8-way
shards, 290 tensors:

| policy | base pause | base CPU | instruct pause | instruct CPU |
|---|---:|---:|---:|---:|
| planned | 98,882.7 ms | 18,770 ms | 108,088.5 ms | 18,780 ms |
| fast raw | 55,689.4 | **2,470** | 110,191.2 | 2,780 |
| fast zstd-1 | 75,815.3 | 2,690 | **76,572.7** | 2,720 |
| fast zstd-3 | 137,135.5 | 2,910 | 95,997.7 | 2,800 |

Encode CPU falls **6.5–7.6x**. Wall/pause ordering across fast codecs is not
stable on single runs because 2,320 object publications and host cache state
dominate; only the CPU reduction is claimed as stable. Raw writes 28.0 % more
bytes than zstd-1.

**Background transcode.** `weftfs transcode` is a bounded exclusive pass: one
encode decision per object, a new version of the same object id, the old version
retained, a `weft.provenance.v1` record, and read-back verification. Base raw:
123.6 s / 34.1 s CPU, 987.9 -> 658.4 MB. Base zstd-1: 227.6 s / 60.6 s CPU,
772.0 -> 658.6 MB. Instruct zstd-1: 242.8 s / 61.4 s CPU, 772.5 -> 658.5 MB.
**Transcode is a storage win and a restore loss**: full restore 1,675.8 ->
4,431.5 ms (base raw) and 2,194.5 -> 4,755.9 ms (base zstd-1), because the
planned form is mode 9. A `SIGABRT` proxy after/before publication leaves the
old or the new whole state, never a partial one.

**Adapters.** Checkpoint-service protocol v2 carries the write policy. On the
32 MiB court fixture, DCP save pause falls **1,384.5 -> 114.8 ms (12.06x)** and
the Orbax handler **1,394.6 -> 131.3 ms (10.62x)**; standard Orbax OCDBT is
31.9 ms, so the remaining deficit is **4.12x** (was 43.7x planned). That is a
pause result on one synthetic fixture, not a throughput or Orbax-competitiveness
claim.

**Trajectory.** Pythia-70M steps 0/1/2/4/8 plus fetched and LFS-SHA-verified
steps 16/64/512/1000: nine checkpoints, 281.7 MB each. Cumulative
**173,235,935 -> 660,220,076 B**; nine independent `zstd -19` checkpoints sum to
1,299,182,221 B. The final-step baseline `final + Σ zstd19(XOR delta)` is
672,210,950 B (ours 1.8 % smaller) and `first + Σ zstd19(--patch-from)` is
1,031,422,235 B (ours 36.0 % smaller). Early steps lose to a single `zstd -19`;
cross-over is after step 2. 9/9 exact. Qwen base -> Instruct:
658,187,354 -> 1,199,651,657 B (independents 1,535,945,154; XOR baseline
1,384,249,213; `--patch-from` unavailable). The declared synthetic 50-step
series: 4,652,647 -> 34,894,788 B, exact, with a full checkpoint every ninth
step at the depth-8 bound; **it loses to `final + Σ zstd19(XOR)` at 4,882,259 B**.
GC with 50 snapshots frees 0 bytes; without snapshots, 49 unlinked checkpoints
free 150,596 B / 142 blobs. `GcReport::deleted_bytes` reported 3,467,041,895,
which does not reconcile with the tree delta; it is not used as a claim (see S11
for the accounting fix).

**Load receipts and recovery-first.** `weft.load.receipt.v1` is a canonical WCB
record of per-tensor attested/loaded roots, set roots, exactness and container
SHA-256. On a 512 MiB synthetic tiled checkpoint (16,384 tiles): lazy median
**2,181.2 ms (4,362 ms/GiB)** vs full **3,836.6 ms (7,673 ms/GiB)**, same set
root and exact output. A corrupted tile produces a typed `Corrupt` naming tile
0 and emits **0 bytes**. Recovery-first serves 4 required tensors of 16 at
**1,037.0 ms** vs 8,261.7 ms to complete, after fetching 15,336,426 of
245,382,816 B. After a real `SIGABRT` once the required set was cached, restart
serves it at **51.9 ms** with 0 bytes fetched. Cold time-to-complete is slower
than a plain restore (8.26 s vs 2.18 s) because the mode re-resolves the
manifest per tensor.

Losses: wall ordering is not stable; transcode is a separate exclusive pass and
is slower than the save; the FUSE load receipt is per file; no Go parity for the
write policy, transcode, load receipt or recovery-first path; process-fault
recovery only.

### S9 — parallel checkpoint prepare/commit, RSA ladder, commit-attested index (`STATUS.md` T1 hardening)

**Prepare/commit split.** `Store::write_checkpoint_parallel` publishes
create-once blobs from N read-only blob-worker handles, then one committer
re-verifies every fragment and publishes one group commit. Workers never touch
the superblock, commit record, writer lock, lease, fence, or pack catalog; the
single-writer proof is unchanged. Qwen2.5-0.5B, 8 shards, 5 reps, 4 vCPU: best
prepare **13.61 / 11.55 / 6.36 / 3.50 s** at 1/2/4/8 ranks (**3.89x** at 8 vs
1), best wall 15.40 / 13.37 / 8.16 / 5.31 s, commit 73–74 ms, verify 1.29 s,
peak RSS 393 -> 507 MiB. Every configuration stored 775,693,224 B with head
`53a8c743a9e0b451…`; the MinIO tensor subset has head `2438460a33504a09…`.
`deterministic_seed` makes the sequential and split paths byte-identical. A real
`SIGKILL` mid-prepare leaves only the previous whole generation; a refused
prepare publishes nothing and GC reclaims the orphans.

Losses: 8 ranks on 4 vCPUs is worse than 4 in the median; RSS grows with ranks;
the S3 phase is a tensor subset on a 1 GiB tmpfs; Rust only.

**RSA.** Both CRT half-exponentiations use a Montgomery ladder with a fixed
modulus-width step count, mask-only cswap, fixed-width CIOS multiplication and
branchless recombination. A mandatory public-exponent fault check refuses any
signature whose `sig^e` does not equal the exact EMSA-PKCS1-v1_5 encoding.
Timing smoke test, 20,000 signs/class x 3 rounds: worst |Welch t| **4.04**
(2.91 / 4.04 / 3.64), below the conventional 4.5 cutoff; the earlier
single-round 6.24 receipt is retained.

Losses and non-claims: **constant-time execution is not proved**; this is a
smoke test on one microarchitecture and compiler, with no cache/branch/frequency
adversary model; `BigUint::rem` still runs on public message bytes; Go uses
vetted `crypto/rsa`.

**Index trust.** A WFX2 selector-index segment is accepted only when its own
digest is valid, its commit/generation/counts match the replayed state, **and**
the commit record re-hashes to that commit and attests the expected state root
and WFX2 candidate root. A forged, self-consistent subset is rejected and
rebuilt in both languages; a genuine Go segment loads in Rust without a rebuild,
and vice versa. New additive `weftfs.commit.v2`; v1 commits stay valid and their
segments are never trusted. A commit-record rewriter is outside this threat
model (no signed commit chain yet).

### S10 — writer-lock root cause and the KV prefix tier (`STATUS.md` T2)

**Writer lock.** The long-standing `tests/quota.rs` flake was not a store bug:
`flock(2)` and OFD locks belong to the open file description, which a forked
but not-yet-exec'd child inherits until `exec`. The probe measured, per 2,000
acquire/drop/reopen cycles with a concurrent `/bin/true` spawner:

| mechanism | close only | explicit unlock before close |
|---|---:|---:|
| `flock(2)` | 289 spurious `Busy` | 0 |
| `fcntl(F_OFD_SETLK)` | 478 | 0 |
| `fcntl(F_SETLK)` | 0 | 0 |

The T1 assumption that OFD locks are exempt from fork inheritance was **wrong**
and is now an executable test. Linux keeps `flock` and OFD/POSIX locks in
separate namespaces, so the shipped lock takes **both** (mixed-version
compatibility) and explicitly unlocks (`F_UNLCK` + `LOCK_UN`) in `Drop` before
the descriptor closes. Store probe: 2,000 `Store::open` calls under 1,182
concurrent forks gave **0** spurious `Busy`. `tests/writer_lock.rs` 8/8;
`cargo test --test quota -- --test-threads=8` 20/20 (was 12/15).

Remaining: two syscalls per acquisition (below noise); a killed process without
`Drop` can leave a forked child holding the lock until it execs; POSIX `F_SETLK`
is measured but not used (any close drops the lock).

**KV prefix tier.** `weft.kv.prefix.v1` plus mode-12 pages, a token-hash chain,
`kv_prefix_*`, `kv_gc`, `kv_decide_reuse`, and CLI `weftfs kv`. Compatibility
is exact and fails closed on tenant, model, tokenizer, attention config, dtype,
head geometry, layer count, page rows, position, and token chain; a miss returns
`Recompute` with reasons. Measured: one 512-token prefix is 1,835,008 logical B
in 112 page objects (1,931,305 physical B, 55.8 ms put); 100 prefixes over one
system prompt are 286,720,000 logical B -> **103,219,200 stored B (0.64)** in
4.03 s; authorized `get` p50 **23.4 ms**, p95 35.9 ms; compatibility scan p50
**10.4 ms**. The reuse decision is 6 reuse / 7 recompute on 13 declared
requests; the prefill model is **declared, not measured against an inference
engine**.

Losses: no persisted KV index (the scan dominates), no FUSE read path, no
constant-time capability check, no encryption at rest, no retention policy, and
no Go parity.

### S11 — GC accounting, planner write cost, mode-18 gate, stronger candidates (`STATUS.md` S1–S6)

**GC byte accounting.** `deleted_bytes` mixed data blobs with advisory index
segments. It is now data-only, with `deleted_index_bytes` and
`deleted_total_bytes` separate in both languages. On the trajectory fixture:
pinned **0 B data / 3,466,466,150 B index**; unpinned **150,596 B data (equal to
the tree delta) / 3,466,891,299 B index**. This exposed a real **3.47 GB index
directory for a 74 MB data tree** (per-commit full-state WFX2 segments). It is
reported, **not fixed**.

**Planner objective.** The objective is now
`physical + lambda_read*decode(reads_per_write) + lambda_write*encode`; region
reads count every whole tile a request intersects. On the access-pattern court,
whole/bytes-only selects mode 9, whole/latency selects raw, one-row/once selects
raw, and one-row or 100-row x1000 selects mode 17. The model break-even is **5
reads**; the measured break-even is **6.89**.

**Mode 18 gate.** The fitted fixed term is the measured 96,601 ns floor (not a
clamped negative intercept), and mode 18 is gated off automatic selection in
both languages unless `--allow-experimental-modes` or an explicit strategy hint
is given.

**Stronger candidates.** Level-19 ZSTD (planned, >=1 MiB, 20 s CPU budget) and
a whole-base `zstd-patch` mode-4 candidate. Pythia step0->step1: 18,763 ->
**17,923 B** (patch wins 6/76 tensors) vs 30,265 B for a whole-file
`zstd --patch-from -12`. Pythia F32 base: 173,194,756 -> **163,872,074 B
(-5.4 %)** but still **+15.2 % versus whole-file `zstd -19`**. Qwen
base -> Instruct stays at **541,151,674 B** (patch changes 0 B; mode 11 already
wins). Every loss is in `bench/RESULTS-compression-candidates.md`.

Gates: Rust, FUSE, Go, claims, spec drift and the aggregate **37/37**
(`verify-20261108T165021Z.json`) at WeftFS `94a4818`, WeftCodec `cffcc1a`,
WeftCodec-Go `0db665e`, WeftFS-Go `6d58137`. The first Stage S6 aggregate
(`verify-20261108T090639Z.json`, 33/37) is retained because it found a decoder
window regression, a KV sample floor, nested-court orchestration, and 2,946
fuzz-differential mismatches.

### S12 — bounded / streaming ingest: the real 3B family inside the default court (`STATUS.md` S12)

Goal: ingest the real Qwen2.5-3B BF16 family inside the default **3 GiB** bench
court. This is a memory-only change: no wire format, frame byte, manifest
field, price, candidate order, tie-break or selected strategy changed.

`retain_candidate` keeps at most the frames that can still be chosen (peak
retained frames ~6 -> 2 per tensor); `canonicalize_cow` borrows row-major input;
the safetensors reader streams per-shard reads and incremental content/values
digests; `write_checkpoint` streams with `max_ingest_bytes` (default 1 GiB);
`TensorPlanOptions::max_delta_bytes` (default 512 MiB) bounds whole-tensor
delta candidates. The codec side adds `DomainHasher`, `values_hasher` and
`logical_root_from_values_hash` (WeftCodec `cffcc1a`).

Qwen2.5-3B base and Instruct: 6,171,927,112 B each, 434 BF16 tensors, every
file re-hashed in the court (SHA-256 `52da3d99…17ab` and `54917aba…d91d2`).
One family, one configuration, one rep per phase (not the full
three-configuration court):

| phase | peak RSS |
|---|---:|
| ingest | **2,765,636 KiB** |
| topology | 1,863,172 KiB |
| measure | 1,840,524 KiB |
| checkpoint (8-way) | **614,920 KiB** |
| recovery | 250,388 KiB |

Every phase completed under 3 GiB; no OOM kill and no limit was raised. Ingest
wall 976,527 ms; `verify --full` clean (870 objects, 873 manifests, 731
representations, 0 problems). Base strategies: 270 float-tensor / 92 zstd / 72
raw. Instruct: 255 weight-delta / 113 sparse-delta / 1 float-tensor / 65 raw.
Store: data 6,775,861,261 B, indexes 169,652,106 B (2.442 %), total
6,946,533,343 B. The 0.5B family, same command: the pre-change tree
(`d5e78ce` / `3d9ae04`) is **OOM-killed (exit 137)** at 3 GiB; this tree
(`9822d56` / `cffcc1a`) peaks at **2,257,928 KiB** with verify ok. 580/580
per-tensor rows match the earlier run on logical root, strategy, data bytes and
source SHA-256.

Byte identity: `tests/checkpoint_streaming.rs` writes the same container with
the same deterministic seed at the largest-tensor and 1 GiB ingest bounds and
requires the same head, manifest id, root and canonical bytes.

Losses: the 593.5 MiB embedding is above the 512 MiB delta bound, so
weight-delta, sparse-delta and zstd-patch are **not offered for it** (the
skipped candidates were not the cheapest; this is a policy bound, not
equivalence). Mode-9 encoding of that tensor still sets the ingest peak
(a streaming mode-9 encoder is not implemented). zstd-19 is CPU-gated off for
it (estimated 20.8 s > 20 s). The prepare/commit split still materialises one
whole tensor per worker. Rust only; no Go parity for the streaming codec API,
the ingest bound or the delta bound. Single host, warm cache, process-fault
recovery only.

### S13 — dependency-graph planning (`STATUS.md` S13, `docs/PARENT-GRAPH.md`)

`src/plan_graph.rs` is a pure optimizer over a measured version graph with the
objective `Σ edge_bytes + λ · reads · Σ chain_decode_ns`, using the existing
measured decode rows. Three solvers: `dp` (state `(version, depth)`, exact over
`parent = none | previous`), `brute-force` (exhaustive oracle over every earlier
compatible parent, refused above 12 versions), and `greedy` (not claimed
optimal). The tie-break is `(objective, fewer dependencies, lower parent index)`.
Hard constraints: depth 8 by default, optional SLA on the worst cold restore,
and an optional storage budget; an unsatisfiable SLA is a typed refusal, never a
silent degradation. `weftfs plan-parents` carries the CLI.

Nine pythia-70m checkpoints (281,715,176 B / 76 tensors each, all re-hashed in
the court; independent probe store 1,447,239,539 B; pricing 45 edges took
1,388.8 s). Realised stores, every version exact and `verify --full` clean:

| plan | data bytes | predicted | error | worst restore | Σ restore |
|---|---:|---:|---:|---:|---:|
| chain | 647,411,616 | 647,175,737 | +0.036 % | 9,336.3 ms | 41,218.8 ms |
| restart-2 | 1,030,285,678 | 1,030,180,485 | +0.010 % | 2,448.9 ms | 12,583.3 ms |
| restart-3 | 849,712,592 | 849,572,567 | +0.016 % | 3,248.2 ms | 16,289.9 ms |
| restart-4 | 808,373,400 | 808,225,589 | +0.018 % | 4,650.7 ms | 18,856.6 ms |
| optimizer (λ=1, SLA 4 s) | 1,447,239,539 | 1,474,646,073 | −1.86 % | **801.5 ms** | 7,128.7 ms |

DP and the exhaustive oracle agree exactly on this matrix.

Losses: the optimizer does not create restart points for free — at λ=0 and no
SLA the chain is the smallest plan, so the plan is today's behaviour; the caller
must state a latency objective or SLA. The SLA is a **model** SLA and the model
was not honest (S13b). The DP's stated space is adjacent parents only; the
oracle can prefer a non-adjacent parent (on this matrix it did not). The byte
model does not credit cross-version chunk dedup. Pricing is expensive
(1,388.8 s for nine versions) and is a write-time tool, not a read path.
Restores are warm-cache, single-threaded, one host. **Rust only**; WeftFS-Go has
no parent-graph planner or CLI.

### S13b — measured restore floor, and the published 850 ms SLA miss (`STATUS.md` S13b)

`examples/calibrate_restore.rs` times real `Store::restore_checkpoint` calls
(7 reps) and fits `residue = restore − Σ measured decode rows` over a
4 KiB–64 MiB count sweep, a 1–64 MiB size sweep, a controlled 8-hop chain, a
real pythia-70m container, and the whole nine-checkpoint trajectory. Receipt
`s13b-restore-floor-20261108T230417Z.json`, 154 fitted samples.

The baked term is `weftfs-s13b/restore-overhead/v1`:
**per_version 13,285 ns, per_tensor 29,205 ns, per_mib 2,114,993 ns**, integer
arithmetic only. `restore_ns = decode_chain_ns + overhead`, charged once per
version restore — never per dependency hop. An SLA claimed with the model
withheld is a typed `InvalidArgument`, and the plan records the model it used.
The persisted `weft.calibration.v1` document is unchanged (still exactly ten
decode rows), so an existing document loads byte-for-byte.

The hypotheses that were refuted:

- A per-object fixed cost does not explain large tensors: at a fixed 64 MiB,
  one 64 MiB tensor has 136.2 ms of residue and sixty-four 1 MiB tensors
  127.0 ms; the count slope only turns positive below ~256 KiB.
- The per-version manifest cost is real but tiny: 14.0 µs for one tensor,
  0.31 ms for 76, 77 ms for 16,384. The baked 13,285 ns is the measured
  one-tensor manifest read.
- Chain over-prediction is per hop and class-dependent (sparse-delta
  14.9–15.9 ms/hop; weight-delta 35.6–38.7 ms/hop against the 42.4 ms row), not
  a misplaced constant.

Corrected court (`parent-graph-20261108T232016Z.json`, same nine checkpoints,
7 medians/version):

| plan (λ, SLA) | predicted worst | measured worst | error |
|---|---:|---:|---:|
| chain | 12,170.3 ms | 9,238.2 ms | +31.7 % |
| restart-2 | 2,205.3 ms | 2,049.9 ms | +7.6 % |
| restart-3 (λ=0, 4 s) | 3,629.1 ms | 3,234.3 ms | +12.2 % |
| restart-4 | 5,052.8 ms | 4,503.9 ms | +12.2 % |
| all-independent floor (λ=1, 4 s) | 781.6 ms | **886.5 ms** | **−11.8 %** |

**The published failure.** The 850 ms SLA is *accepted* (all-independent,
781.6 ms predicted) and *measured at 886.5 ms*: the plan misses its SLA. The
court's own `no_accepted_plan_misses_its_sla` check is **false** (2 of 16
measured acceptances miss, both the same floor), as is
`floor_prediction_within_10_percent`. A 500 ms SLA is now refused
(`LimitExceeded`); S13 would have accepted the old 211 ms floor prediction.
No plan choice at the S13 λ/SLA values changed.

The floor residual is a **page-cache-capacity boundary**, not a fitted error:
in the 3 GiB court, versions 0–3 restore warm at 796–807 ms and versions 4–8 at
878–886 ms with the same 163.9 MB read; a fresh-store calibration measures all
nine at 778–793 ms. The constant was **not** raised to cover the cold case.

Losses: conservative chain/restart predictions because per-candidate decode
rows are unchanged (sparse-delta and zstd-patch reuse the weight-delta row;
level-19 zstd reuses the level-3 row); the count sweep over-predicts mid-size
many-tensor checkpoints (worst fit residual 31.6 % at 128 x 512 KiB); restores
are single-threaded, warm-cache, one host, one corpus. **No Go parity**; the
constant is documented in Rust only and no cross-language agreement is claimed.

## Standalone courts not in the aggregate

These are cited separately; the aggregate runs reduced configurations of the
long courts (recorded in each receipt).

| Court | Receipt | Result |
|---|---|---|
| Mode 18 | `bench/receipts/mode18-20261107T165215Z.json` | Lost every measured axis; kept experimental. |
| Write policy / transcode | `bench/receipts/write-policy-20261107T235728Z.json` | CPU 6.5–7.6x lower; transcode restore loss. |
| Adapter pause | `bench/receipts/adapter-pause-20261108T004305Z.json` | DCP 12.06x, Orbax 10.62x; 4.12x residual. |
| Full trajectory | `bench/receipts/trajectory-20261107T220652Z.json` | 9 pythia checkpoints; every loss above. |
| Load receipts / recovery-first | `bench/receipts/recovery-load-20261108T004358Z.json` | Lazy 2,181.2 ms; 51.9 ms restart. |
| Parallel checkpoint (full) | `bench/receipts/parallel-checkpoint-20261108T033000Z.json`, `…-s3-20261108T033921Z.json` | 3.89x prepare at 8 ranks; identical head. |
| RSA timing | `bench/receipts/rsa-timing-20261108T041500Z.json` | Worst \|t\| 4.04; not a constant-time proof. |
| Lock probe | `bench/receipts/lock-probe-mechanisms.json`, `lock-probe-store.json` | close-only fork inheritance; 0/2,000 with explicit unlock. |
| KV prefix | `bench/receipts/kv-prefix-20261108T052500Z.json` | 0.64 dedup; 23.4 ms get; declared prefill. |
| Access pattern / candidates | `bench/receipts/access-pattern-20261108T073433Z.json`, `compression-candidates-*.json` | 5 vs 6.89 break-even; candidate losses. |
| Parent graph (S13) | `bench/receipts/parent-graph-20261108T192017Z.json` | DP == oracle; pricing cost. |
| Restore floor (S13b) | `bench/receipts/s13b-restore-floor-20261108T230417Z.json`, `parent-graph-20261108T232016Z.json` | Constants; the 850 ms miss. |

## Publication outcomes

**PyPI: published.** `weftcodec 1.2.0` is live at
<https://pypi.org/project/weftcodec/1.2.0/>, uploaded on 2026-11-09 at
10:33:34Z from the Python court with `scripts/publish-pypi.sh` (twine, no
rebuild). The live JSON API reports:

| File | Size | sha256 | Yanked |
|---|---:|---|---|
| `weftcodec-1.2.0-py3-none-any.whl` | 92,913 B | `699b25fd7f6aef36f98f5baf31a130ef1c816a00ca3c93ac4c324667266b9bbb` | false |
| `weftcodec-1.2.0.tar.gz` | 114,324 B | `50678df7ebb559fa3ec632520822ff2c88155788fcf17e54ec5c817388f6dff0` | false |

The two files in `python/dist/` hash to the same digests. The upload token was
then deleted; it was not reused and no new token was created.

**One observed defect in the published metadata.** The 1.2.0 long
description was frozen before a post-upload correction to `python/README.md`.
It still carries the old sentence “PyPI publication requires an API token we do
not have” and a mode table that ends at mode 16. The repository copy was
corrected, but the published files were **not** rebuilt or re-uploaded (the
release instruction forbids it), so the PyPI page is stale in that prose. The
wheel contents and digests are the release artifacts.

**crates.io: published.** `scripts/publish-crates.sh` published `weftcodec`
first, waited for the index, then published `weftfs`, from the OOM-protected
rust court. The temporary credential copy was removed on exit. Live API
readback:

| Crate | Version | Created (UTC) | Checksum | Yanked |
|---|---|---|---|---|
| `weftcodec` | 1.2.0 | 2026-11-09T12:38:45Z | `77a95cd4fd9aeb2888a3f6dd175a96748cb04b221f274496e9b59f18640e3d92` | false |
| `weftfs` | 1.2.0 | 2026-11-09T12:39:05Z | `4b7312b14cf3264216335a2718e39c6a5982e2f64184058f8b430a465cf6e909` | false |

**GHCR: published.** The `v1.2.0` tag push started one container workflow per
repository; all four completed `success`:

| Repository | Run | Run URL |
|---|---|---|
| `WeftFS` | 42618667115 | <https://github.com/infinityabundance/WeftFS/actions/runs/42618667115> |
| `WeftCodec` | 42618672142 | <https://github.com/infinityabundance/WeftCodec/actions/runs/42618672142> |
| `WeftCodec-Go` | 42618675687 | <https://github.com/infinityabundance/WeftCodec-Go/actions/runs/42618675687> |
| `WeftFS-Go` | 42618679797 | <https://github.com/infinityabundance/WeftFS-Go/actions/runs/42618679797> |

Every image was read back from the registry; the index digest and the
config-blob labels are recorded under Artifacts. Container builds are **not**
bit-reproducible, so the digests are observed release digests, not a
reproducibility claim.

**Live cloud services: not attempted in this release.** No AWS, Azure, or
Google credential or bucket was used. The cloud evidence remains MinIO, Ceph
RGW, Azurite, and `fake-gcs-server` from the earlier courts.

## Artifacts

### Tags

| Repo | Tag | Commit (tag peels to) |
|---|---|---|
| `WeftFS` | `v1.2.0` | `122d86e70927899ee179bb032e5253887f9cc76a` |
| `WeftCodec` | `v1.2.0` | `d95fa78c805eef43b32c3da5025981eab7325513` |
| `WeftCodec-Go` | `v1.2.0` | `0db665ea4c6b1d69d1d1640801ed475848d361f1` |
| `WeftFS-Go` | `v1.2.0` | `6d58137ca888c0b10d01f2fd93f3af00d544e5f6` |

### Rust crates (crates.io)

| Crate | Version | Live checksum | Yanked |
|---|---|---|---|
| `weftcodec` | 1.2.0 | `77a95cd4fd9aeb2888a3f6dd175a96748cb04b221f274496e9b59f18640e3d92` | false |
| `weftfs` | 1.2.0 | `4b7312b14cf3264216335a2718e39c6a5982e2f64184058f8b430a465cf6e909` | false |

Checksums read back from `https://crates.io/api/v1/crates/<crate>/1.2.0` after
publication.

### Python package

| File | Size | sha256 |
|---|---:|---|
| `weftcodec-1.2.0-py3-none-any.whl` | 92,913 B | `699b25fd7f6aef36f98f5baf31a130ef1c816a00ca3c93ac4c324667266b9bbb` |
| `weftcodec-1.2.0.tar.gz` | 114,324 B | `50678df7ebb559fa3ec632520822ff2c88155788fcf17e54ec5c817388f6dff0` |

Both files are attached to the parent GitHub release and are live on PyPI
(verified above).

### Container images (GHCR)

Each repository publishes only the image it owns. Index digests and config-blob
labels were read from the registry after the tag runs; the `revision` label is
the tagged commit.

| Image | Index digest (`@sha256`) | `org.opencontainers.image.source` | `org.opencontainers.image.revision` |
|---|---|---|---|
| `ghcr.io/infinityabundance/weftfs:v1.2.0` | `1505110de71a9d62bb6b4aed7f6f1140dc6d39c1f4ba871bbf0d3ea864b6eada` | `https://github.com/infinityabundance/WeftFS` | `122d86e70927899ee179bb032e5253887f9cc76a` |
| `ghcr.io/infinityabundance/weftcodec:v1.2.0` | `affee3a6d6026bc3f8a18ccdd6958e89c2d97c7db33943be63d56b230052ce4f` | `https://github.com/infinityabundance/WeftCodec` | `d95fa78c805eef43b32c3da5025981eab7325513` |
| `ghcr.io/infinityabundance/weftcodec-go:v1.2.0` | `bc4de382c3deeeb7c94cae9e62cfa2bd54d575d4bb1717a6bcbc5e96ce2723c6` | `https://github.com/infinityabundance/WeftCodec-Go` | `0db665ea4c6b1d69d1d1640801ed475848d361f1` |
| `ghcr.io/infinityabundance/weftfs-go:v1.2.0` | `56aa45592cd3b714efbf05ba329f0672cded42068c1e4475604da02f6dbf489c` | `https://github.com/infinityabundance/WeftFS-Go` | `6d58137ca888c0b10d01f2fd93f3af00d544e5f6` |

FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and store-only
commands do not.

### GitHub releases

| Repo | Release URL |
|---|---|
| `WeftFS` | <https://github.com/infinityabundance/WeftFS/releases/tag/v1.2.0> |
| `WeftCodec` | <https://github.com/infinityabundance/WeftCodec/releases/tag/v1.2.0> |
| `WeftCodec-Go` | <https://github.com/infinityabundance/WeftCodec-Go/releases/tag/v1.2.0> |
| `WeftFS-Go` | <https://github.com/infinityabundance/WeftFS-Go/releases/tag/v1.2.0> |

The parent release carries `weftcodec-1.2.0-py3-none-any.whl` and
`weftcodec-1.2.0.tar.gz`; both assets read back with the digests above. The
machine-readable record of every identity in this section is
`bench/receipts/release-artifacts-20261109T124325Z.json`.

## Not claimed

This release does **not** claim any of the following:

- **Physical media power loss.** Crash courts are process-fault proxies; the
  QEMU/block-layer courts use a labelled device model and `cache=unsafe`
  lying-fsync, not a pulled power cable.
- **Host page-cache loss.** The host `drop_caches` was not writable in the
  court; only per-file `fadvise(DONTNEED)` was possible.
- **RAPL / joule energy.** `energy_uj` was unreadable on host, guest, and court.
  CPU user/sys time is not energy and is not presented as such.
- **GPU, TPU, or SIMD decode execution.** All decode and restore measurements
  are scalar CPU work in a 4-vCPU court. Mode 18's multi-stream result is a CPU
  result; no GPU/CUDA path was built or run. Orbax/JAX ran with forced CPU
  devices.
- **Live 3FS, Azure, or GCS deployments.** No live service was reached in
  v1.2.0; the evidence remains `fake-gcs-server`, Azurite, and the 3FS
  conformance shim. No RDMA, kernel bypass, replication, or cloud run.
- **A Go Azure backend.** Go has S3 and GCS; `--backend azure` in Go is
  `InvalidArgument`.
- **Constant-time capability checks or constant-time RSA.** The RSA ladder has
  a timing smoke test (worst |t| 4.04) and a fault check, not a proof or an
  audit. The KV check is strict and fails closed, but timing is not constant.
- **Multi-writer, multi-node, or fleet operation.** Writers are exclusive by
  design; distributed courts are one host and one emulator instance. The
  parallel checkpoint path is one committer and N blob workers, not consensus.
- **Writable FUSE over an object store.** Only read-only mounts over S3 and GCS
  are evidenced.
- **Orbax or DCP competitiveness.** The adapter pause improvements are on one
  32 MiB synthetic fixture; Orbax's own OCDBT remains substantially faster on
  save/restore, and the remaining pause deficit is 4.12x.
- **General-purpose-filesystem performance.** Comparative results include large
  losses (sequence throughput, relation/tag selectors versus SQLite, fsync
  tails). No “faster than ext4/XFS/ZFS” claim.
- **Mode 18 as a compression or decode win.** It lost every measured axis and
  is never auto-selected without an explicit opt-in.
- **A 3B corpus scale claim beyond the court.** The 3B ingest is one family, one
  configuration, one rep per phase, in a 3 GiB court on one host; it is not a
  fleet, cold-media, or multi-tenant result.
- **An honest enforced SLA at every value.** The 850 ms SLA was accepted and
  measured at 886.5 ms; the court records that miss.
- **Go parity for S12/S13/S13b.** Go has no streaming ingest bound, no
  parent-graph planner, and no restore-overhead model.
- **A fix for index amplification.** 3.47 GB of advisory index segments for a
  74 MB data tree is reported, not fixed.
- **PyPI metadata freshness for 1.2.0.** The published 1.2.0 description was
  built before the post-upload README correction, so it still carries the
  pre-publication sentence and a mode table that ends at mode 16. The files are
  correct and were not rebuilt or re-uploaded.
- Production readiness, security audit, or a support commitment of any kind.

## Reproduce

```sh
bash scripts/verify-all.sh
bash scripts/run-conformance.sh scripts/conformance-adapter-rust.sh
bash scripts/run-conformance.sh scripts/conformance-adapter-go.sh
bash scripts/docker-court.sh python "cd /work && WEFT_PYTHONPATH=/work/python bash scripts/run-conformance.sh scripts/conformance-adapter-python.sh"
bash scripts/publish-crates.sh
bash bench/scripts/checkpoint_family_court.sh
bash bench/scripts/trajectory_court.sh
bash bench/scripts/write_policy_court.sh
bash bench/scripts/recovery_load_court.sh
bash bench/scripts/parallel_checkpoint_court.sh
bash bench/scripts/parent_graph_court.sh
bash bench/scripts/s13b_restore_floor_court.sh
bash scripts/check-claims.sh
bash scripts/check-spec-drift.sh
```
