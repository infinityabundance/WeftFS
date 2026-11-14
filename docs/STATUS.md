# Implementation status — 2026-11-13 (v1.4.2 released)

<!-- BEGIN GENERATED:status-head (scripts/perf_numbers.py) -->
**At a glance (generated from receipts).** v1.4.2 is released. The cited aggregate is
`bench/receipts/verify-20261113T033408Z.json` — **47/47** courts pass,
0 failed, 0 skipped — at WeftFS `52da9888f3726e6af5c364af7707fd98574fc954`.
The shipped planned write is **974.1 ms** least-contended /
1,175.3 ms p50 at **282,106,049 B** (33.53 % below
the native control, 2.98× its wall). Rust all-targets
**807** tests pass; FUSE **850** passed /
0 failed / 2 ignored; the conformance manifest holds
**420** vectors (aggregate `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`); **1,205** claims are
audited. Every number in this block is emitted by `scripts/perf_numbers.py` between
`BEGIN GENERATED` sentinels and re-validated against its receipt by the `perf_numbers`
court; the entries below this block are a chronological log of subphases, quoted with
the receipt and revision they were written at.
<!-- END GENERATED:status-head -->

## 2026-11-14 (Context-plane RFC-0001 Phase 0/1)

**Deliverables.** Three operator-authored proposals are placed verbatim with a
front-matter status block: `spec/rfc/RFC-0001-context-plane.md`,
`spec/rfc/RFC-0001-adoption-plan.md`, `spec/rfc/RFC-0001-api.md`. Each states
**NOT an IETF RFC, NOT ratified, NOT an implementation claim**, keeps the
supplied baseline line, and adds `placed at revision: f8f352ba9db37dfa08d9305fe3ba7f9288d6be46`
(the staging head this work was added on top of).

`include/weft_context_plane.h` is a compilable **interface declaration only**:
no implementation, no exported symbol, no shared library, no binary-
compatibility certification. It is valid C11 and C++17 under
`-Wall -Wextra -Werror` (host `cc 16.2.1`/`c++ 16.2.1`, and the rust court's
gcc via build-essential). The compile gate is `scripts/check-context-plane-header.sh`
and its two translator units.

`conformance/REQUIREMENTS.md` expands RFC §15 into **38 requirements**. Of these,
**16** are exercised in full by the frozen corpus, **8** only in part, and **14**
require new corpus or new implementation work; each row names the existing
vector paths and the missing fixtures.

**Phase 0 findings.** The mode-registry contradiction was real:
`WEFTCODEC-FORMAT.md` §3 said `19..127` were reserved, while
`WEFT-AI-CODEC.md` §0 defines mode 19 `KV_CACHE_PROV` and mode 20
`ENGRAM_SHARD` as normative and implemented. The framing document was
reconciled **editorially only**: two rows added, `21..127` reserved, the
AI-mode rule range extended, §13 allocations completed, and
`WEFT-AI-CODEC.md` §0 named the normative mode registry. No frame byte, mode
number, hash domain or vector changed; the same reconciliation was exported
byte-for-byte into all three submodule mirrors and committed there.

`spec/CONTEXT-PLANE-FREEZE.md` publishes a matrix of identity rows plus 15
descriptor-only schemas, each with its identity, exact hash domain, version,
normative document and Rust/Go/Python/verifier status. It records the
divergences (checkpoint is Rust-only; Go provenance is write-only; KV prefix
and MW3 domains are Rust-only) and states that frozen identities are
incorporated by reference and unchanged. It also records that several domains
used by code and vectors (`weft/checkpoint/manifest/v1`, `weft/index/*`,
`weft/pack/*`, `weft/load/*`, `weft/mw2/key/v1`) are absent from the frozen
domain table in FORMAT §1.1 — a documentation gap, not a byte disagreement.

The tokenizer identity gap is confirmed in both codec implementations:
`tokenizer_root` hashes only `{scheme, vocab_size}` (Rust
`kv_prov::tokenizer_root`, Go `modes.TokenizerRoot`), so it does not bind
vocabulary contents, merges, normaliser, special tokens or chat-template
semantics. Mode-19 bytes were not changed.

The two-language vector run at this revision (built in the rust/go courts, run
by the host-only reference runner) and the independent verifier results are
recorded in the generated block below. Skips are explicit and are never counted
as passes.

**Phase 1 verifier.** `verifier/` is a clean-room Python 3 stdlib-only
implementation (own BLAKE3, WCB, Ed25519), with no import, FFI or subprocess
call into WeftFS code. The provenance self-test is labelled separately because
no frozen `weft.provenance.v1` corpus exists. Receipt
`bench/receipts/context-plane-verifier-20261114T063213Z.json` (+ `.log`),
analysis `bench/RESULTS-context-plane-verifier.md`. It is independent of the
implementation code path but not yet an external third-party implementation;
the work does not claim otherwise. The eight rules found under-specified in the
spec text are listed in the verifier README and in the receipt's `unspecified`
array; the largest are the non-self-contained KV provenance `page` map, the
selector namespace grammar living in the Context Root document, and the
provenance record's unstated unknown-key rule.

<!-- BEGIN GENERATED:context-plane-status (scripts/perf_numbers.py) -->
Vector run (built in the rust/go courts, run by the host-only reference runner): Rust 420 total, 288 pass, 0 fail, 132 skip (`bench/receipts/conformance-rust-20261114T062338Z.json`); Go 420, 288, 0, 132 (`bench/receipts/conformance-go-20261114T062338Z.json`); independent Python reader 420, 229, 0, 191 (`bench/receipts/conformance-python-20261114T062338Z.json`). All three name manifest aggregate `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8` over 420 vectors.
Independent verifier: 136 cases, 131 pass, 0 fail, 5 skip; negative suite 7/7; provenance self-test 7/7. Receipt `bench/receipts/context-plane-verifier-20261114T063213Z.json`.
Freeze matrix: 24 identity rows.
<!-- END GENERATED:context-plane-status -->

This subphase assigns no new wire value. It adds two courts to
`scripts/verify-all.sh`: `context_plane_header` (compile-only) and
`context_plane_verifier` (clean-room corpus run, with the standard explicit
skip path). `verify-all.sh` itself was **not** re-run for this subphase; the
cited aggregate remains `verify-20261113T033408Z.json` (47/47) and the next
aggregate will include 49 courts. No Go or Python implementation of the new
proposed schemas exists, and no C ABI implementation exists; those are
non-claims, not omissions hidden behind prose.

## 2026-11-14 (T019-T021/T027: MW3 conflict domains, incremental rebase, writer envelope)

<!-- BEGIN GENERATED:mw3-status (scripts/perf_numbers.py) -->
MW3 (generated): disjoint domain writers reach **56,140 tx/s** at N=8 against **11,111 tx/s** at N=1 (**0.632** efficiency), then plateau at four vCPUs; every disjoint cell reports **3.04** backend reads per commit and **0** intervening commits read. Same-domain N=32: **1** winner, **319** typed conflicts, p99 **0.465 ms** (stamp 20261114T070000Z, revision 2dc840e13dda).
T021 (generated): across 7 court runs MW2 N=32 reads/commit **68.90 → 32.22** and p99 **206.3 → 104.7 ms**; p99 fell in **7 of 7** runs and reads/commit in **6 of 7**, so the read count is not a stable claim; the published R4 figure was **31.39** reads/commit (stamp 20261114T070000Z, revision 2dc840e13dda).
T027 (generated): **1,018** of 1024 writer threads spawn under `--hold` (**6** refused by the court's pid bound), peak RSS **110,784 KiB** = **111,437 B/writer**; **0** writers ever retried; peak aggregate **64,730 tx/s** at N=16 (stamp 20261114T070000Z, revision 2dc840e13dda).
<!-- END GENERATED:mw3-status -->

Normative contract: `spec/WEFT-MULTIWRITER.md` §15 (`weft.multiwriter/v2`),
exported to all three submodule mirrors. MW3 is opt-in (`--writer-mode
sharded`), additive, Rust-only, and local-plane-only; MW1 remains the default
and MW2's frozen bytes are untouched. `tests/mw3.rs` 13/13 covers domain
identity and discovery idempotence, the discovery record resolving each domain
to its live head, independent heads, same-domain single-winner (deterministic
and threaded), MW1/MW2 refusal, cross-domain refusal, GC refusal, chain replay,
corrupt-chunk detection, a missing-domain view, and the create-once lineage
record. T021 changes no protocol transition: only the retry delay distribution,
an explicit touched-key set, a bounded intervening-commit cache and
`RebaseStats`. The bounded TLA model was **not** re-run; no abstract transition
changed.

This entry was verified after the WIP checkpoint `f8f352b`. Two things in that
checkpoint were wrong and are corrected rather than carried forward: the MW3
unit test `view_root_separates_domains_and_generations` failed (it asked a read
view to name one domain twice, which the protocol refuses), and the T021
figures were a single court run whose after arm contained a 67.1 reads/commit
outlier. The tables now come from **seven runs** with a merge receipt; one of
the seven still regresses on reads/commit and it is published. T019 also gained
the two surfaces its acceptance named: the discovery record now carries the
derived `head_key` of each domain (resolved live by `discover_heads` and
`weftfs mw3 list`), and `weft.domain.link.v1` is implemented as create-once
lineage rather than specified-only.

The gate verdicts are `bench/RESULTS-mw3.md` §6. T020's scaling is real up to
the CPU count and is published as a plateau beyond it; T021's p99 result is
stable across every repetition and its reads/commit result is **not** (parity
with the published R4 figure, a fall only against the same-session baseline);
T027's pid limit, not the protocol, refuses threads at 1024 writers.

Not claimed, and listed in `bench/RESULTS-mw3.md` §5: cross-domain atomicity
(`Class::CrossDomain`), coordinated cross-domain GC (GC is refused on a sharded
store), index attestation on domain commits, Go MW3 parity, per-domain
transaction ids, a data merge (lineage only), multi-node/fleet, physical power
loss, a crash court for a sharded store, and a global linearizable view.

## 2026-11-13 (T012/N13 aged-store index court, X02 DCP completion, X03 guards)

<!-- BEGIN GENERATED:status-t012 (scripts/perf_numbers.py) -->
Aged-store index court (generated): 10k generations publishes at 0.1 ms p50 with 8,388,211 B of index for 1,363,694 B of data; 1M generations publishes at 5.8 ms p50 with 837,228,211 B of index and 2,752,788 KiB peak RSS. A 100k-object store carries 0 checkpoint state manifests and its cold open is 2,295.5 ms; the 50k-object cell is refused at generation 229.
DCP completion (generated): shipped plan 39.78 ms p50 / 40.39 ms p95, 3.65 ms better than the forced oracle encoder (43.43 ms p50); the 20 ms target is not reached.
<!-- END GENERATED:status-t012 -->



Not claimed: a 1M live-object cell (checkpoint state manifests exceed the
codec's default WCB item bound above ~50k objects, measured; the target attempt
also OOM-killed the 3 GiB court); Go reading a 1M-generation store (the store
is ~8.4 GB and is not retained inside the 1 GiB court `/tmp`, and Go's open
path at 1M commits is unmeasured); physical power loss; a DCP completion under
20 ms. Provenance: `bench/RESULTS-incremental-index.md` (aged section),
`bench/RESULTS-dcp-metadata.md` (X02 section), and the per-cell receipts named
there.



This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. Current claims are generated from
receipts by `scripts/perf_numbers.py` into the block above and into
[`docs/PERF-NUMBERS.md`](PERF-NUMBERS.md); a `perf_numbers` court fails closed
when any generated number drifts from its receipt. Everything below the block
is a chronological log of subphases, retained verbatim: each entry is a
**historical quotation** written at the receipt and revision it names, not a
statement about the current tree. The v1.4.2 note with every loss and every
non-claim is [`docs/RELEASE-v1.4.2.md`](RELEASE-v1.4.2.md); the previous notes are
[`docs/RELEASE-v1.4.1.md`](RELEASE-v1.4.1.md) and
[`docs/RELEASE-v1.4.0.md`](RELEASE-v1.4.0.md), then
[`docs/RELEASE-v1.3.0.md`](RELEASE-v1.3.0.md),
[`docs/RELEASE-v1.2.0.md`](RELEASE-v1.2.0.md) and
[`docs/RELEASE-v1.1.0.md`](RELEASE-v1.1.0.md). Both codec implementations cover
every allocated mode 0–20; the independent Python reader covers every allocated
mode 0–18 (modes 19 and 20 are rejected `UnsupportedMode`).

**Subphase R4 (multi-writer) status, 2026-11-09 (amended by R14d):** the
default writer level is still **MW1 — many producers, one committer**. The pre-R4
claim that MW2 and MW3 are absent is now split: a bounded, opt-in **MW2**
transaction protocol exists for the explicit transaction API on
`local`/`object`/`3fs`/`s3`/`azure` in Rust and on `local`/`object`/`azure` in
Go (GCS refused `UnsupportedVersion`); **MW3 was not implemented and not claimed
at R4** *(superseded 2026-11-14: MW3 is an opt-in, Rust-only, local-plane
prototype — see the T019–T027 entry at the top of this file)*. The
normative contract is [`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md);
the design, backend tradeoffs and every loss are in
[`docs/MULTIWRITER-ARCHITECTURE.md`](MULTIWRITER-ARCHITECTURE.md) and
[`bench/RESULTS-multiwriter.md`](../bench/RESULTS-multiwriter.md). FUSE file
mutations and generic checkpoint publication remain MW1. The full R4 entry
with measured tables is at the end of this document. The R4 aggregate is
`bench/receipts/verify-20261109T165707Z.json` (**41/41** courts `pass`, 0
failed, 0 skipped) at WeftFS `a3256af`; it now includes `tla`,
`multiwriter`, and `multiwriter_failure`. Note that the MW2 throughput table
was measured at `b0bcbc5e` / `f46b351f`, before R14d and the R17–R22 write
work; it is not re-measured here.

**Subphase R13 (restore scaling), 2026-11-11:** the R10 bottleneck is proven
and fixed. One backend mutex held **0.41–0.65 of the restore wall** at four
workers while four threads waited **0.50–0.75 of the wall** in aggregate; the
lock took 37–60 % of every tensor's wall time. Immutable blob reads now use a
bounded `ReadHandlePool` (64 idle handles) and verify their hash after the
handle is released, so the state mutex covers only mutable control state; a
packed-plane handle is invalidated by every mutation; 3FS is deliberately not
pooled. A tensor's shards are read and verified with the shipped bounded
runner (`shard_threads`, default 1). Evidence: state-mutex hold share
**0.000** in every configuration; store-only Qwen2.5-0.5B restore
1,188 → 2,792 MB/s at four workers and 3B 1,177 → 3,073 MB/s; shard
concurrency lifts the single 0.5B embedding from 552 to 454 ms and the 3B
embedding from 1,303 to 1,072 ms; four independent readers reach 1,860–2,658
MB/s aggregate; MinIO reaches 495 MB/s at four workers. Losses: the
whole-file SHA remains a serial floor (3B one-worker −1.5 %), 4×4 shard
oversubscription is slower than 2×4, peak RSS rises 14–20 %, 3FS is not
pooled, and region restores remain sequential. Every worker count is
byte-identical at 1/2/4/8/16 × 1/2 tile × 1/4 shard threads. Receipts
`restore-scaling-20261111T090904Z`, `restore-scaling-3b-final-20261111T092000Z`,
`restore-scaling-readers-20261111T091500Z`,
`restore-scaling-minio-20261111T085500Z`; analysis
[`bench/RESULTS-restore-scaling.md`](../bench/RESULTS-restore-scaling.md).

**Stage S6 update (2026-11-08):** the current verification receipt is
`bench/receipts/verify-20261108T165021Z.json` (**37/37** courts `pass`, 0 failed,
0 skipped) after the Stage 1–6 work below; the previous T2 receipt
`verify-20261108T050823Z.json` (30/30) is retained, and the first Stage 6 run
`verify-20261108T090639Z.json` (33/37) is retained because it found four real
defects (a decoder window regression, a KV sample-count floor, an
orchestration nesting error, and 2,946 fuzz-differential mismatches caused by
the same window regression). The v1.1.0-era verification receipt was
`bench/receipts/verify-20261107T155516Z.json` (30/30 courts `pass`, 0 failed, 0 skipped,
including the host-only `claims` documentation gate, spec drift, generated-type
drift, `conformance_manifest`, `conformance_coverage`, quantized cross-decode,
context and authenticated-capability cross-language gates, Buck2 CAS, agent
memory, the R22 `threefs` court (3FS POSIX byte-identity plus the USRBIO ABI
conformance court), `ceph_rgw` — Ceph RGW 19.2.0, an independent S3
implementation, now running both the S3 backend suite and the R17 fenced-writer
suite — the MinIO `s3_court` (native S3 backend and fenced-writer suites), and
`fuzz_diff`), at
WeftFS `817687d`, WeftCodec `99cb87a`, WeftCodec-Go `a0a1ebf`, WeftFS-Go
`dc7cfdb`. The tag adds one evidence-only commit. The superseded receipt
`verify-20261107T154010Z.json` (30/30 at `1901336`) is retained. The v1.0.0 release receipt is `bench/receipts/verify-20261107T065848Z.json`
(30/30), run after the 1.0.0 version bump at WeftFS `7c10ad4`, WeftCodec
`d61ed76`, WeftCodec-Go `86ded1e`, WeftFS-Go `9fa39d3`;
`verify-20261107T014045Z.json` (30/30 at the pre-bump tree, WeftFS `0cfbea1`,
WeftCodec `57288e1`, WeftCodec-Go `86ded1e`, WeftFS-Go `39b58f2`) is retained.
One intermediate run was aborted because the shared host volume reached 100 %
(`No space left on device`); its log `verify-20261107T020631Z.log` is retained
and **no receipt exists** for it, because the script never reached receipt
writing; after deleting only gitignored `.docker-cache` scratch, the rerun
passed 30/30. Standalone courts not in the aggregate are cited in
`docs/RELEASE-v1.0.0.md` and in the per-subphase notes: Azure/Azurite
`azure-20261107T015947Z.json` (19 backend + 7 fencing, 0 failures), Go S3
`WeftFS-Go/bench/receipts/go-s3-20261107T020137Z.json` (17 pass / 1 skip
focused, full `-race` 0 failures, 17/17 cross-language), 3FS `--bench`
`threefs-20261107T020623Z.json`, and the Python court
`python-bindings-20261107T015918Z.json` (349 tests, 0 failures, 9 skipped). The
previous 30/30 receipt (`verify-20261106T232858Z.json` at `4704ded`), the
29/29 receipt (`verify-20261106T225457Z.json` at
`132e713`), the 29/29 receipt before it
(`verify-20261106T214749Z.json` at `9a6156c`), its 28/28 predecessor
(`verify-20261106T174703Z.json` at `37a3a39`), and the 28/28 receipt before that
(`verify-20261106T162208Z.json` at `0e3670a`) are retained. The failing first
R20 aggregate (`verify-20261106T212741Z.json`, 28/29) is also retained; that
failure was the pre-existing `tests/quota.rs` writer-lock flake,
`usage_survives_reopen_and_still_denies`, which passed standalone immediately
after and in the rerun. A court that cannot run in the aggregate's environment
(non-x86_64, no Docker daemon, a pinned image or binary unobtainable, or a
court's private network already taken) is recorded as `skip` with its exact
reason under `skips` in the receipt, and a skip is not a pass; this run had
none. The v0.9.0 release receipt
`bench/receipts/verify-20261106T141623Z.json` (27/27 at WeftFS `cef37bc`,
before `ceph_rgw` joined the aggregate) remains the release evidence, with
`docs/RELEASE-v0.9.0.md` and the Python court at that tree: 331 tests,
0 failures, 0 errors, 9 skips
(`bench/receipts/python-bindings-20261106T142616Z.json`). The earlier
`verify-20261106T083428Z.json` (27/27 at the R7 shared-mount tree) is retained.
The previous release receipt,
`verify-20261106T011444Z.json` (also 27/27), is retained for the v0.8.0 release
and is cited in `docs/RELEASE-v0.8.0.md`; its own first run at that tree,
`verify-20261106T010756Z.json`, is retained because `rust_store` hit the
pre-existing `tests/quota.rs` writer-lock flake (`usage_survives_reopen_and_still_denies`,
`Busy` on reopen); that test passed standalone five times and in three full
`cargo test --all-targets` runs. The earlier
`verify-20261105T175154Z.json`, `verify-20261105T174254Z.json`, and
`verify-20261105T034941Z.json` are retained
as history; the last of those had one pre-existing `tests/representation.rs`
shared atomic decode counter race (`expected exactly one decoded chunk, got 2`)
that passed standalone and in the 035513Z rerun. Earlier
`verify-20261104T022254Z`,
`verify-20261103T220311Z`,
`verify-20261103T172416Z`, and `verify-20261103T165944Z` receipts are retained
as history. The P6 store-backed Buck2 CAS court is separate from that full
`verify-all` receipt: it passes 41/41 assertions at WeftFS `d175fe7` and is
recorded in `bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json`,
with the benchmark in
`bench/receipts/buck2-cas-store-20261105T163930Z.json` and analysis in
`bench/RESULTS-buck2-cas-store.md`.
The Q5 PyTorch Distributed Checkpoint adapter receipt is
`bench/receipts/pytorch-dcp-20261106T000734Z.json` (real torch 2.15.0+cpu,
topology matrix, 8 x 4 MiB bench); analysis in `docs/PYTORCH-DCP.md`.
The
twelve-step showcase receipt is
`bench/receipts/showcase-20261103T171440Z.json` (twelve of twelve `pass`).
The P7 benchmark receipts are `bench/receipts/bench-codec-20261103T175847Z.json`,
`bench/receipts/bench-store-20261103T175920Z.json`, and
`bench/receipts/bench-fs-20261103T180751Z.json`; the analysis, losses, and
gaps are in `bench/RESULTS-v1.md`. The comparative pass receipts are
`bench/receipts/bench-external-20261104T000122Z.json`,
`bench/receipts/bench-codec-matched-20261103T232158Z.json`,
`bench/receipts/bench-fs-cold-20261103T232216Z.json`,
`bench/receipts/bench-store-20261103T235127Z.json` (same-revision store side),
and `bench/receipts/energy-probe-20261103T225547Z.json`; analysis is
`bench/RESULTS-comparative-v1.md`. The F2 receipt is
`bench/receipts/f2-20261103T205045Z.json` with its raw log beside it. The F3
receipts are `bench/receipts/context-mounts-20261103T210625Z.json` (6/6),
`bench/receipts/ambiguity-20261103T210633Z.json` (4/4),
`bench/receipts/time-travel-20261103T210633Z.json` (4/4), and
`bench/receipts/replay-bundle-20261103T210824Z.json` (4/4), each with its raw
`.log` and `.results.tsv`; the determinism comparison is
`bench/receipts/context-mounts-determinism.txt` (`semantic_match=true`). The
1,000-mount scale receipts are
`bench/receipts/context-mounts-20261105T122643Z.json` and
`bench/receipts/context-mounts-20261105T131116Z.json` (5/5 each, 1000/1000
mounts, `semantic_match=true`), with the directed-limit ceiling in
`bench/receipts/context-mounts-20261105T140342Z.json` (INCOMPLETE 692/1000) and
`bench/receipts/context-mounts-20261105T135805Z.json` (400/400 at 24 GiB /
4,096 pids); analysis `bench/RESULTS-context-mounts-1000.md`. Raw logs and
results TSVs for those runs are stored as `.log.zst` / `.results.tsv.zst`.
The F6 cross-language gate is `WeftCodec-Go/hack/cross.sh` (tensor/tokens/
quantized byte-identical in both directions). The current F7 receipt is
`bench/receipts/ai-corpus-20261104T021627Z.json` with cost graph JSON/CSV beside
it (whole-pair-aware planner); analysis, losses, and caveats are in
`bench/RESULTS-ai-v1.md`. The historical frame-smallest receipt
`ai-corpus-20261103T220113Z.json` is retained. F8 receipts are
`bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json` (41/41,
store-backed CAS with mount and cross-language) and
`bench/receipts/agent-memory-20261104T022512Z.json` (7/7 simulated-timestamp
months-later replay); the earlier prototype receipt
`bench/receipts/buck2-cas-20261104T022511Z.json` (15/15) is retained. D2 authenticated capability grants are implemented and
released in v0.6.0: Ed25519 `weft.capability.envelope.v1`, offline verification
against an explicit revocation set, enforcement layered on POSIX and explicit
capabilities, a two-tenant isolation court, and the envelope benchmark receipt
`bench/receipts/capability-envelope-20261105T034858Z.json` (analysis in
`bench/RESULTS-capability-envelope.md`). The cross-language gate is
`WeftCodec-Go/hack/capability-cross.sh`: byte-identical signed blobs and
identical `BadSignature`/`WrongGrantee`/`Expired`/`Revoked`/`NonCanonical`
rejections. Generated-type drift is `scripts/gen-types.sh --check`,
wired into `verify-all`. FLOAT_TENSOR (mode 9) now encodes its exponent plane with `huffman-canonical-v1`: `cargo test --all-targets` (133 tests) and `go test ./...` pass in the Docker courts, and `WeftCodec-Go/hack/cross.sh compare` reports `PASS identical float_tensor`, `float_tensor_single`, and `float_tensor_uniform`; this change is covered by the `codec_cross_compare` court in `verify-20261105T035513Z.json`.
The F9 TLA+ receipt is `bench/receipts/tla-20261103T224446Z.json` (corrected
model pass; flawed model counterexample). The F9 power-loss receipts are
`bench/receipts/powerloss-20261103T224453Z.json` (30/30, `cache=none`),
`bench/receipts/powerloss-20261103T224652Z.json` (10/10, `cache=writeback`), and
`bench/receipts/powerloss-20261104T014805Z.json` (30/30, `cache=unsafe`,
lying-fsync device model). The block-layer lying-disk matrix is
`bench/receipts/blkdebug-20261104T072008Z.json` (30 iterations; dm-flakey
silent drop 5/5 lost, dm-flakey EIO 5/5 fail-closed, snapshot total drop 5/5;
blkdebug write/flush error 10/10 blocked at ext4 mount). The host-cache probe is
`bench/receipts/hostcache-probe-20261104T072314Z.json` (global drop refused:
`Read-only file system`). The B2 100,000-iteration receipt is
`bench/receipts/crash-torture-20261104T104233Z.json` (raw log and TSV stored
losslessly as `.zst` beside it; the 10,000-iteration receipt
`crash-torture-20261104T013521Z.json` is retained). The counter/energy probe is
`bench/receipts/perf-counters-20261104T015026Z.json` (perf counters available;
RAPL energy still unreadable). The whole-disk ZFS/Btrfs fio run is
`bench/receipts/bench-fs-cold-20261104T071922Z.json` (ext4/XFS/Btrfs/ZFS,
`zfs-whole` uses raw `/dev/vdb`; one repetition per cell). The earlier
file-backed ZFS run `bench-fs-cold-20261104T014727Z.json` is retained and
separately labelled. The abstraction mapping, assumptions, and limitations are
in `verification/README.md`.

**CHECKPOINT TOPOLOGY (P3), 2026-11-05.** A checkpoint is now a store directory
of named tensors with a canonical `weft.checkpoint.v1` manifest
(`docs/checkpoint-topology.md`). The manifest records, per tensor, the logical
dtype/shape/`LogicalTensorRoot`, the content root, the element bytes, and the
storage topology of every shard (`{axis, shards, index, object_name, object_id,
content_root, logical_root, byte_len}`). Each shard is a mode-10
`TENSOR_SHARDED` tensor object whose `q8_0` words are the shard's exact bytes
(a byte transport, not a lossy quantization), written with the shard shape's
last dimension scaled by the element size so the codec region API addresses
logical slices. `checkpoint write` splits a safetensors container along an axis,
rejects non-divisible splits with "no silent padding", and publishes all shard
objects plus the manifest in one group commit. `checkpoint restore` reassembles
from the shard objects, verifies every shard root and the full tensor root, and
can re-express the bytes under `sharded 8/4/2/1`, row-major, column-major, or
blocked topologies before writing the container; complete restores reproduce
the source header and require the source SHA-256. A region restore
(`--tensor NAME --range ...`) reads only intersecting shard objects through the
codec region API and reports shard/unit/byte counters. Real-weight court on the
cached Qwen2.5-0.5B container (BF16, 290 tensors, 988,097,824 bytes, SHA-256
`88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`): 8-way
ingest 41.53 s / 582,028 KiB peak RSS / 825,180,192 store bytes / 2,320 shard
objects (ingest wall varied 37.04–83.77 s across three runs of the same code);
complete restores under topology 8 and 4 read the same 821,882,936 bytes and
reproduce the source SHA-256 exactly (warm pair 20.44 s and 19.89 s; the first
restore after ingest varied 56.30–111.50 s with host writeback state);
all 290 logical roots are identical across topologies; a 100-row slice reads
one shard and 28,053,320 bytes instead of 224,668,493; and a plain mount plus
the unmodified `safetensors` 0.8.0 loader with NumPy 2.4.6 + `ml_dtypes` 0.6.0
reads all 290 tensors from the mount and matches the source SHA-256. Receipt:
`bench/receipts/checkpoint-topology-20261105T062409Z.json`; analysis:
`bench/RESULTS-checkpoint-topology.md`. Tests `tests/checkpoint_topology.rs`
9/9 (topology matrix, partial slice counters, missing/corrupt shard, manifest
root mismatch, non-divisible split, CLI end to end). `cargo test --all-targets`
154 passed / 0 failed; `cargo test --features fuse --all-targets` 182 passed /
0 failed / 2 ignored (the pre-existing CAP_SETUID and shared-counter tests).
The Go store does not implement `weft.checkpoint.v1`; this phase is Rust-only.

**TENSOR_SHARDED (mode 10), 2026-11-04.** Both codecs now implement an
additive sharded quantized tensor mode (`spec/WEFT-AI-CODEC.md` §11,
`spec/WEFT-TENSOR.md` §2.1). A shard frame carries a canonical
`weft.ai.tensor_sharded.v1` descriptor (`q8_0`/`q4_0`, whole shape, global
per-block f32 scale bits and i64 zero points, shard axis/count/index, and the
WeftTensor `layout_root`) plus one `huffman-canonical-v1` stream over the
shard's quantized words and the global scale plane. Decoding reconstructs the
exact quantized values and scale bits; dequantization is out of band. The
logical root is computed over the whole canonical row-major quantized words
and block records, so it does not change with shard axis or count. Evidence:
`cargo test --all-targets` 150 passed / 0 failed / 0 ignored; `go test ./...`
all packages pass; `WeftCodec-Go/hack/cross.sh compare` reports byte-identical
mode-10 frames, decode equivalence, and identical roots for all three sets.
Vectors are `conformance/v1/sharded/001`–`003` (valid) and `101`–`108`
(rejects), byte-identical in both repositories. The historical
`017_unknown_mode_reserved10` vector is superseded by
`017_unknown_mode_reserved11`; that vector is now itself superseded by
`017_unknown_mode_reserved12` because mode 11 is allocated (below). Receipt:
`bench/receipts/tensor-sharded-20261104T211555Z.json`.

**WEIGHT_DELTA (mode 11), 2026-11-04.** Both codecs now implement an exact,
dependency-linked tensor delta (`spec/WEFT-AI-CODEC.md` §12). A frame carries a
canonical `weft.ai.weight_delta.v1` descriptor (`bf16`, `fp16`, `fp32`, `q8_0`,
`q4_0`; shape, element count, word bits, base frame hash, `op: "mod-sub"`, plane
count/coder, `delta_root`, and stored payload length) and one
`huffman-canonical-v1` stream per byte plane. The base is a single dependency
frame; decoding resolves it through the caller's resolver (DAG depth/cycle/
missing/hash rules apply) and, when the base frame bytes are available, checks
its own dtype and shape. All arithmetic is on unsigned words, so every bit
pattern (NaNs, denormals, signed zeros, infinities) reconstructs exactly. A
4 MiB bf16 base/target pair differing in 1% of elements produces a 533,110-byte
delta frame versus 4,194,410 independent zstd-3 bytes (3,649,949 at zstd-19);
the two Huffman planes are 265,022 + 267,643 bytes of 2,097,152 raw bytes each;
encode median 12.86 ms, decode median 16.99 ms; reconstruction and the logical
tensor root match the original. Evidence: `cargo test --all-targets` 161 passed
/ 0 failed / 0 ignored; `go test ./...` all packages pass (103 top-level tests);
`WeftCodec-Go/hack/cross.sh compare` reports `PASS identical
weight_delta_bf16`, `weight_delta_fp32`, `weight_delta_q8` and decode
equivalence in both directions. Vectors are
`conformance/v1/weight-delta/001`–`003` (including an all-zero delta) and
`101`–`108` (base dtype mismatch, base shape mismatch, unknown op, word-bits
inconsistency, delta-root mismatch, payload-length mismatch, missing base,
dependency cycle), byte-identical in both repositories. Receipt:
`bench/receipts/weight-delta-20261104T214337Z.json` (synthetic, single host,
Docker court).

## 1. Repositories and revisions

| Repo | `staging` revision pinned by the v1.3.0 aggregate | Role |
|---|---|---|
| `WeftFS` | `3ab54c7` | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `d9234e5` | Rust codec v1 + AI modes, checkpoint and capability support |
| `WeftCodec-Go` | `fe05240` | independent Go codec, capability parity |
| `WeftFS-Go` | `80ddb5b` | independent Go store + FUSE, spec mirror |

These are the `staging` heads the v1.3.0 release-tree aggregate ran against
(`git rev-parse HEAD` in each repository; the aggregate was run while the
release candidate was still unpushed, and `staging` has since been pushed and
then advanced by the R14d work in progress).
The aggregate is `bench/receipts/verify-20261111T182601Z.json` (**45/45** courts
pass, 0 failed, 0 skipped). The heads at the start of the release-candidate
pass were WeftFS `e77f0e5`, WeftCodec `67cea30`, WeftCodec-Go `5e047c3`,
WeftFS-Go `225eb0d`; the aggregate immediately before the release candidate is
`bench/receipts/verify-20261111T131235Z.json` (**45/45**) at WeftFS `40df0cd`.

## 2. What is implemented and tested

### WeftCodec (Rust) — implemented

- Exact WFT1 framing: 96-byte header, 48-byte dependency/chunk entries, header
  and trailing CRC32C, checked lengths, reserved-bit rejection.
- Domain-separated BLAKE3, minimal LEB128, canonical WCB with encoded-key map
  ordering and non-canonical rejection.
- RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE,
  QUANTIZED_TENSOR; chunked RAW/ZSTD; `read_range` decodes only intersecting
  chunks.
- Resolver + frame-DAG decoding with cycle detection and depth/fetch/decoded
  bounds; WFA1 self-contained archives.
- WeftQL parser, normalization, canonical selector bytes, SelectorId.
- CLI (`encode`, `decode`, `inspect --json`, `verify`, `archive`, `selector`,
  `conformance`), including `--mode tensor` and `--mode tokens`.
- **110 tests pass** in the Rust court. Frozen vectors: 60 codec (including 28
  AI-mode valid/reject vectors), 20 WCB, 22 selector, 16 context, 10 causal,
  and 36 adversarial.

### WeftCodec-Go — implemented, independent

- Native Go implementation of the same v1 bytes (no cgo, no Rust calls).
- **57 top-level tests pass**, including Rust-vector replay and malformed-input
  classes; `go vet` and a CGO-disabled build pass.
- Cross-language gates pass in both directions for RAW, ZSTD, REFERENCE, DELTA,
  DICTIONARY, chunked, STRUCTURED, TENSOR, TOKENS, and QUANTIZED_TENSOR.
  RAW/REFERENCE/DELTA/STRUCTURED/TENSOR/TOKENS/QUANTIZED_TENSOR frames are
  byte-identical; ZSTD/DICTIONARY are decode-equivalent by spec.

### F6 — AI codec modes (implemented)

- `TENSOR` (mode 6): exact little-endian row-major payload plus a canonical WCB
  descriptor (`weft.tensor.v1`) carrying dtype, shape, layout, endianness,
  element count, payload bytes, and a `weft/tensor/payload/v1` hash binding.
  Supported dtypes: `f16`, `bf16`, `f32`, `f64`, `i8`, `u8`, `i16`, `i32`,
  `i64`. Checked shape arithmetic; scalar and zero-element tensors; rank ≤ 8.
- `TOKEN_SEQUENCE` (mode 7): canonical WCB descriptor (`weft.tokens.v1`) and a
  deterministic `u64-zigzag-delta-leb128` payload: first id absolute, later ids
  wrapping 64-bit signed zigzag deltas, minimal LEB128, exact count and
  trailing-byte checks.
- Both modes: `decoded_len` is the payload length and `content_hash` is
  `H(weft/chunk/v1, payload)`; the descriptor binds the payload under its own
  domain. Frame `domain=0`, no dependencies, no chunks.
- `QUANTIZED_TENSOR` (mode 8, added 2026-11-04): packed `i8`/`u8` row-major
  payload plus a `weft.quantized-tensor.v1` descriptor carrying dtype, shape,
  layout, `affine-asymmetric-per-tensor`, exact f64 `scale_bits_le`, i64
  `zero_point`, element count, payload bytes, and a
  `weft/quantized-tensor/payload/v1` hash binding. Finite positive scale and
  dtype-range zero point are enforced; three valid and eight reject vectors;
  Rust↔Go frames are byte-identical.
- `FLOAT_TENSOR` (mode 9, added 2026-11-04): `huffman-canonical-v1` exponent plane plus raw sign+mantissa plane, bit-exact including NaN payloads, infinities, signed zeros and denormals; 3 valid + 8 reject vectors; Rust↔Go frames are byte-identical.
- **Not implemented and not allocated a wire value in mode 6:** blocked/
  column-major/sharded layout *fields*, i4 nibble packing and per-axis
  quantized scales, activation/gradient streams, embedding/KV-cache pages,
  attention masks, vocabulary-artifact binding, weight-delta descriptors, and
  semantic/structured deltas. They are `specified, not implemented` in
  `spec/WEFT-AI-CODEC.md` §6. The layouts now have an **additive** identity and
  addressing layer outside mode 6; see WEFT-TENSOR below.
- Cross-language: Rust→Go and Go→Rust decode gates pass; TENSOR, TOKENS, and
  QUANTIZED_TENSOR frames are byte-identical. Historical
  `017_unknown_mode_reserved8` (mode 8 reserved) is retained byte-for-byte and
  marked superseded by the new mode-9 reject vector.

### WEFT-TENSOR — logical identity and region addressing (implemented, both codecs)

- `spec/WEFT-TENSOR.md` defines a layout-independent identity:
  `LogicalTensorRoot = H("weft/tensor/logical/v1", WCB({schema, dtype, shape,
  element_count, values_hash}))` with
  `values_hash = H("weft/tensor/values/v1", row-major element bytes)`.
- Canonical `weft.tensor.layout.v1` descriptors for `row-major`,
  `column-major`, `blocked`, and `sharded`; rank/zero/overflow/divisibility and
  field-presence rules fail closed. Additive only: mode 6 stays `row_major`.
- Physical arrangements and `read_region` report the number of physical units
  touched, proving that a region inside one shard touches exactly one shard.
- `weft.tensor.shard.v1` containers (`u32le || WCB || payload`, payload-hash
  bound) and `shard_root`, which accepts shuffled input and reorders by `index`
  deterministically; duplicate, missing, out-of-range, and inconsistent shards
  are rejected. Rust and Go agree.
- Conformance vectors `conformance/v1/tensor/` (3 valid logical, 10 logical
  rejects, 4 valid layouts, 10 layout rejects, 2 shard sets with byte-identical
  containers) are byte-identical in both repos. Cross gate:
  `WeftCodec-Go/hack/tensor-cross.sh` (roots and containers byte-identical; each
  language reassembles the other's shards). Bench receipt:
  `bench/receipts/wefttensor-20261104T205020Z.json` (64 MiB bf16; in-memory,
  synthetic, single host).
- Not done: no FUSE/store/object-store binding of the layout layer; no
  partially resident tensors, arbitrary irregular block lists, or padded
  blocks.

### F7 — AI corpus and cost graph (implemented, synthetic)

- Committed deterministic corpus `bench/corpora/ai-v1/`: 60 files, 1,939,911
  source bytes, 1,516,737 visible bytes; prompts, JSON/text tool outputs,
  near-duplicate agent intermediates, 12 typed tensors, 8 token sequences, and
  near-duplicate checkpoint-like blobs. License CC0-1.0, synthetic provenance,
  per-file SHA-256 manifest.
- Receipt `bench/receipts/ai-corpus-20261104T021627Z.json` + cost graph JSON/CSV
  beside it. Historical `ai-corpus-20261103T220113Z.json` retained. Analysis,
  every loss, and caveats in `bench/RESULTS-ai-v1.md`.
- Current measured physical bytes: `zstd_forced` 575,808; `weft_auto` 575,713;
  `cdc_zstd` 603,094 (+4.74%); `weft_context` 514,506 (−10.65% vs forced zstd)
  with raw pair bytes 697,014 (45.96% of visible), 8 pair losses, and 8
  contextual objects. The current whole-pair-aware planner rejects contextual
  candidates whose base is not amortized and whose pair cost does not beat the
  context-free frame; 24 such rejections are recorded. The historical
  frame-smallest planner produced 176,564 physical bytes but 1,367,212 pair
  bytes and 20 pair losses; it is retained as a superseded result.
- Typed TENSOR/TOKENS modes were evaluated for all 20 typed objects and chosen
  0 times; descriptor overhead 272–288 bytes. QUANTIZED_TENSOR is implemented
  and cross-language tested but is not part of the ai-v1 corpus. No superiority
  claim.

### F8 — Hooks: store-backed Buck2 CAS, agent memory, generated types (implemented)

- **D1 Buck2 adapter (store-backed, not a plugin):** `adapters/buck2/` is now a
  CAS on the WeftFS store, not a private directory. Every blob is an ordinary
  WeftFS file whose `ContentId` is `H("weft/chunk/v1", bytes)`; the action
  record is a canonical WCB file (`weft.buck2.action-record.v1`); `cas_action`
  (record→output) and `cas_output_of` (output→record) are typed edges, so
  `by:action=<key>` is a one-hop WeftQL selector. `ActionKey =
  H("weft/buck2/action/v1", WCB({action_type, toolchain, sorted input
  ContentIds, canonical params}))` — input paths are metadata, not key material.
  Verbs: `store --key-file --blob`, `fetch --key --out`, `query --by
  action=<key>`, `verify --key --blob`, `context`, `resolve`, `delete`, `gc`.
  Same key + identical bytes is an idempotent hit; same key + different bytes
  fails closed with `Conflict` before any store mutation. The correctness court
  runs in the FUSE court and passes **41/41** assertions, including:
  `mount --context by:action=<key>` reads the artifact byte-exactly; a flipped
  manifest byte is detected by `verify`/`fetch`; GC collects an unreferenced
  action while a named snapshot + Context Root keep another output exact; the
  Rust CLI verifies/cats/stats/resolves a Go-written store; the Go adapter
  fetch/verify/query reads a store written entirely by the Rust CLI. Go tests
  are 7/7 in the FUSE court and 5/7 pass with 2 honest skips in the Go-only
  court (no Rust oracle). Receipts:
  `bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json` (41/41)
  and `bench/receipts/buck2-cas-store-20261105T163930Z.json` (bench); analysis
  `bench/RESULTS-buck2-cas-store.md`. The historical prototype receipt
  `bench/receipts/buck2-cas-20261104T022511Z.json` (15/15) is retained but
  superseded. `buck2` itself was **absent** in the court; no Buck2 run, no
  plugin/worker ABI, no REAPI gRPC.
- **D2 agent long-term memory:** `scripts/demo-agent-memory.sh` creates
  prompt/model/tool/dataset/output objects and five typed edges, three Context
  Roots (task/model/time), a January snapshot, an exported bundle, then
  simulates September writes and replays the January bundle later. Verified:
  run-001 sha256 exact in all January views; run-002 absent from January;
  September time root keeps old bytes exact; replayed bundle reproduces all
  three ContextRootIds and five lineage edges with Go only. Receipt
  `bench/receipts/agent-memory-20261104T022512Z.json` (7/7). Timestamps are
  simulated; this is not a real-time aging or retention measurement.
- **D3 spec-as-code:** `spec/schema/identity-v1.schema` is the canonical source;
  `scripts/gen-types.sh` generates `WeftCodec/src/generated.rs` and
  `WeftCodec-Go/generated/generated.go` (schema/domain constants, limits,
  record types, field lists, source SHA-256). The runtime identity layer
  re-exports generated constants; Rust and Go tests compare generated values to
  runtime values; `scripts/gen-types.sh --check` fails on drift and is a court
  in `verify-all`. The schema is exported to subproject `spec/schema/` and
  checked by `scripts/check-spec-drift.sh`.

### WeftFS (Rust) — implemented core + FUSE (with documented limitations)

- Portable store: `FORMAT`, `LOCK`, dual CRC32C superblock slots, `WFC1`
  commits, canonical commit/checkpoint schemas, immutable chunks/manifests,
  snapshots, relations, tips, xattrs, saved contexts.
- Atomic publication with fsync ordering; one writer (`flock` on a local or
  object namespace, a fenced lease with a conditional compare-and-swap on S3,
  GCS, and Azure); generation fallback; recovery; full verify; mark-and-sweep
  GC; checkpoints; selector evaluation with POSIX + `weft.acl.read`
  authorization; ambiguity errors.
- FUSE: lookup/getattr/setattr/open/create/read/write/flush/fsync/release,
  readdir(+plus), mkdir/rmdir/unlink/rename(+NOREPLACE/EXCHANGE)/link/symlink/
  readlink, xattrs, statfs, access, mknod(regular; FIFO/device `EPERM`),
  fallocate(0/KEEP_SIZE/PUNCH_HOLE/ZERO_RANGE/COLLAPSE/INSERT),
  copy_file_range(server-side), lseek(SEEK_DATA/SEEK_HOLE), O_APPEND/O_TRUNC/
  O_EXCL, sparse writes, open-unlinked, snapshot mounts, `/.weft/id/<hex>`,
  saved context views. Explicit binding boundaries: Rust `O_TMPFILE` and
  daemon-side flock/POSIX locks are unavailable in fuser 0.15.1.
- Deterministic store fixture `conformance/store/v1/` (head
  `8959c65b...`) with `EXPECTED.json`.
- **64 core tests pass** (lib 7, CLI 6, crash proxy 3, recovery 11, store core
  11, extensions 11, bounded recovery 3, context 12). `tests/bounded_recovery.rs`
  enumerates every one of generations 3–5 corrupted in three ways (empty, half,
  bit flip) and every superblock-slot corruption, asserting the previous whole
  state and replay idempotence. This is bounded enumeration, **not** TLA+/Alloy
  model checking.
- **86 FUSE-feature tests pass, 0 fail, 2 ignored** (fuse 22, core 64):
  - ownership test needs `CAP_SETUID` (court has only `CAP_SYS_ADMIN`);
  - same-inode concurrent direct read/write can return `EIO` on kernel 7.3
    with fuser 0.15.1. The daemon logs successful replies and the failing
    request never arrives; rate depends on timing. This is documented, with
    the exact reproducer, in `docs/fuse-status.md`. Store-level atomicity is
    tested separately. A same-description write after shared `mmap` also
    returned `EIO` once; shared mmap/write/msync/read passes with a fresh
    description.

### WeftFS-Go — implemented with documented gaps

- Independent Go reader/writer for the identical on-disk format, all eight
  commit ops, checkpoints, superblock recovery, manifests/chunks, selector
  evaluation, authorization, `verify --full`.
- Native `go-fuse/v2` frontend with the core POSIX surface, snapshot mounts,
  `/.weft/id/<hex>`, and saved-context views. Added in F8: `getlk`/`setlk`/
  `setlkw` (POSIX byte ranges + flock through an in-memory manager and
  `EnableLocks`), `mknod` (regular files only; FIFO/socket/device `EPERM`),
  `tmpfile` (O_TMPFILE plus link through `/proc/self/fd`), `fallocate` modes
  0/`KEEP_SIZE`/`PUNCH_HOLE`/`ZERO_RANGE` (with store-level COLLAPSE/INSERT),
  `copy_file_range`, and `lseek` `SEEK_DATA`/`SEEK_HOLE`. `ioctl` is
  intentionally unimplemented (`ENOTTY`).
- **Online/background GC:** `store.GCBounded`/`GCWatch`, `gc --apply --watch`,
  and `mount --gc-watch`. Passes require the writer lock, run under the FUSE
  server mutex, delete at most a configured number of candidates, are
  cancellable, preserve snapshots/checkpoints/pins, and are disabled by
  default.
- **74 top-level tests pass, 2 skipped (pre-existing child-process guards),
  0 failed** under `go test -race -count=1 ./...`; `go vet ./...` and
  `CGO_ENABLED=0 go build ./...` pass. New verb/GC tests are listed in
  `WeftFS-Go/NOTES.md` §F8.
- Rebuilding the Rust fixture through the Go writer reproduces the same head
  and byte-identical `FORMAT`, `superblock`, commits, manifests, chunks, and
  snapshot pointer.
- Bidirectional handoff gates pass: Rust fixture → Go mutations → Rust verify;
  Go init/mutate → Rust verify/mount; alternating generations; snapshots and
  checkpoints both ways; crash proxy through both; and Go background GC → Rust
  `verify --full` + snapshot mount.
- Remaining limits: `PUNCH_HOLE`/`ZERO_RANGE` define content but do not reclaim
  physical chunks (dense v1); 7.3.1 FUSE rejects COLLAPSE/INSERT before
  forwarding; locks are per-daemon advisory and do not implement the POSIX
  close-any-fd release for owners never seen on the closed handle; `ioctl`,
  mmap writeback, and persisted atime/ctime are not implemented.

### F8 — WeftFS-Go FUSE verb closure and online GC (implemented 2026-11-03)

- Commands: `scripts/docker-court.sh go "cd /work/WeftFS-Go && gofmt -l . &&
  go vet ./... && CGO_ENABLED=0 go build ./..."` (clean) and
  `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race -count=1
  -v ./..."` (**74 passed, 2 skipped, 0 failed**; skips are the existing
  child-process guards).
- CLI watch smoke: `gc --apply --watch --interval 100ms --max-deletes 1
  --grace-secs 0` deleted two unreachable blobs in two bounded passes, then
  reported zero; SIGTERM exited 0; `verify --full` reported `ok=true`.
- Cross-language: the Rust oracle created a snapshot plus garbage; the Go
  watcher swept it; Rust `verify --full`, Rust live read, and a Rust snapshot
  mount all passed. Existing GC/handoff gates A–D also pass.
- Blocked verb evidence: on kernel 7.3.1, COLLAPSE/INSERT return
  `EOPNOTSUPP` with go-fuse debug showing no `FUSE_FALLOCATE` frame; the store
  implementations are directly tested. `PUNCH_HOLE` without `KEEP_SIZE` is
  also rejected at that boundary. Details and safe-use notes are in
  `WeftFS-Go/NOTES.md` §F8.

### F2 — Context Roots, capabilities, and causal identity (implemented in Rust and Go)

- **Context Root persistence:** `context_root` relations from the root object
  with the canonical record hex and authoritative `weft://` URI. Duplicate names
  are `Exists`; `--replace` is delete+add in one commit. Capability records are
  stored in the same commit when first referenced.
- **Resolution:** recomputes `ContextRootId`, verifies the stored URI, requires
  the pinned snapshot to replay, accepts only `weft-resolver-v1`, authorizes
  before counting, and returns ordered results plus `context_root_id`,
  `selector_id`, `snapshot_id`, `matched_count`, `returned`, and closure
  entries. 0/1/n is always explicit.
- **CLI:** `context create|list|show|resolve|materialize|uri|delete`,
  `causal show`, `uri canonicalize|show`, and `--causal` on `create`/`write` in
  both languages.
- **Capabilities:** `weft.capability.v1` JSON/hex files; enforcement for
  namespace/root grants, selector universe, relation/tag predicates, exact-id
  access, saved-context views, and materialization. Denials are
  `Unauthorized`/`NotFound` and do not expose counts, ids, evidence, or
  existence. v1 makes no cryptographic unforgeability or timing claim.
- **Budgets:** effective bound is the component-wise minimum of root and
  capability budgets. No truncation policy exists in v1: exceeding a bound is
  `LimitExceeded`, never a silent truncation. `max_bytes` bounds materialized
  bytes, `max_results` bounds returned results, `max_depth` is one-hop, and
  `max_work` is deterministic **authorized** objects inspected plus **visible**
  relation edges. Authorization and namespace-root readability are decided
  before work accounting, and `max_work`/`max_depth` failures emit fixed,
  non-disclosing messages (`resolver work budget exceeded`,
  `traversal depth exceeds max_depth`). This closes a real defect: the earlier
  implementation counted the whole state (including unauthorized objects and
  relations) and echoed the count. Proven by
  `tests/capability_nondisclosure.rs`: two stores with identical authorized
  views and different hidden populations return identical results, work units,
  and budget outcomes.
- **Causal identity:** `--causal` computes explicit parents plus the available
  transitive closure with a visited set (cycles terminate), stores the four
  reserved attrs, publishes the closure record content-addressed, and includes
  `identity_mode`/`causal_hash` in `VersionId` only for `causal_exact`.
  `bytes_only` remains byte-identical to the frozen v1 record.
- **Materialization:** all content hashes are verified before output is
  published; writes go to a sibling temporary directory and are renamed into
  place. A mismatch leaves no partial output. The manifest records root id,
  snapshot id, selector id, closure, capability hash, budget, counts, and
  per-file path/hash/size/ids/causal closure.
- **Verification and GC:** `verify --full` validates context records/URIs,
  re-resolves roots and reproduces recorded ids, checks causal attrs and
  closure blobs, and re-hashes stored capability records. GC retains causal
  closure blobs while their referencing object is retained; context/capability
  relations are ordinary metadata and never keep content alive by themselves.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse --all-targets"`
  reports **70 passed, 0 failed, 2 ignored** (the existing CAP_SETUID and
  kernel-7.3 direct-I/O limitations). `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race ./..."`
  passes for `store` and `fusefs`, including Rust→Go and Go→Rust context,
  causal, capability, and materialization handoff gates.
  `go vet ./...` and `CGO_ENABLED=0 go build ./...` pass in the Go court.
  Golden causal vectors are `conformance/context/v1/causal_versions.json`.

### Codec-backed representation manifests (implemented 2026-11-04, opt-in, Rust + Go)

- **What it is:** a `weft.manifest.v2` representation manifest can store an
  object's bytes as a WFT1 frame (`kind:"codec"`) instead of raw chunks, with
  `strategy` (`zstd`/`delta`/`reference`), a `frame` blob, and a
  `dependencies` list of base content roots. v2 also permits `kind:"raw"`.
  The frozen `weft.manifest.v1` is unchanged and remains the default.
- **Selection:** `Options.representation = RepresentationMode::CodecAuto` plus
  `codec_min_bytes` (default 4096), reachable as `--representation
  raw|codec-auto` and `--codec-min-bytes` on both the CLI and `mount`. The
  default stays `raw`. The writer picks DELTA against the causal
  parent's content root only when it beats independent ZSTD by the configured
  margin; otherwise it falls back to a v2 raw manifest. There is exactly one
  v2 schema spelling (`weft.manifest.v2`); unknown schemas fail closed.
- **Read path:** range reads decode only intersecting codec chunks
  (`weftcodec::stream::read_range`), verify the frame's payload mode against
  `strategy`, bound dependency depth (8) and reject cycles, and verify the
  frozen whole-content hash on full reads. Missing/unreadable/mismatched
  dependencies fail closed; no partial or approximate reconstruction.
- **Retention:** GC treats representation dependencies as roots, so a retained
  dependent keeps its base; `verify --full` decodes representations and
  re-hashes them.
- **Measured** (8 MiB synthetic, release, rust court, host page cache; receipt
  `bench/receipts/representation-20261104T093437Z.json`): repetitive text
  8,395,250 B raw → 16,853 B ZSTD, encode 227.56 ms → 12.84 ms; near-duplicate
  8 MiB pair 8,395,250 B independent → 1,153,697 B DELTA, at the cost of
  full-object DELTA range reads (6.17 ms vs 0.05 ms raw). Incompressible
  near-duplicates correctly fell back to raw.
- **Byte-identity evidence:** with `Raw` (the default) the committed
  `conformance/store/v1` fixture still rebuilds to generation 11 and head
  `8959c65b…e1d81`, and `tests/representation.rs` asserts the v1 manifest
  layout, keys, and byte-identical manifests across two Raw stores.
- **Go parity (2026-11-04, `WeftFS-Go` `d7d8d06`):** the Go store now parses,
  writes, verifies, and GCs v2 manifests with the same selection rule, limits
  (depth 8, cycles, chunk count), fail-closed dependency handling, and CLI /
  mount flags. Cross-language tests run both directions for real: Rust writes
  zstd + delta + raw-fallback, Go verifies and reads byte-exactly; Go writes
  the same shape, Rust verifies and reads byte-exactly; Go GC retains a
  Rust-written delta base.
- **Not claimed:** experimental. V2 **frame bytes are not byte-identical**
  between the Rust and Go zstd encoders; the claim is mutual read/verify, not
  identical frames. DELTA/REFERENCE and unchunked ZSTD ranged reads materialise
  the whole plaintext in both implementations. A global representation
  planner and compaction/migration of existing objects remain future work.
  Bench numbers are synthetic, not production data.

### Storage-path performance: extent COW and bounded open (implemented 2026-11-04)

- **Extent-level COW range writes.** `Store::write_file`/`write_file_causal` no
  longer buffer or rewrite the whole object. A range write reads the manifest
  and only the chunks it touches; untouched `ChunkRef`s are reused verbatim and
  their blobs are neither read nor rewritten. New chunks are content-addressed
  and deduplicated; a byte-identical rewrite adds zero blobs and zero
  manifests. Measured on a 64 MiB object, 64 KiB chunks, 20 × 4 KiB aligned
  overwrites (release, rust court, medians): 33.01 ms vs 69.64 ms, `rchar`
  64.1 MiB vs 128.0 MiB, peak RSS 3.8 MiB vs 131.7 MiB; `wchar` identical
  (122,431 B) because both paths already dedupe unchanged chunks. Receipt
  `bench/receipts/extent-cow-20261104T090112Z.json`. **Limit:** v1's
  whole-file `ContentId` is a hash of every byte, so the digest pass remains
  O(file size); the gain is in writes, read-verification, and memory. A
  Merkle-root content identity that would make hashing O(changed chunks) is a
  v2 format change, not claimed.
- **Bounded replay / fast open.** `recovery::load_state_bounded` starts at the
  selected head generation and walks parents by generation number, stopping at
  the newest commit whose checkpoint verifies; it does not enumerate older
  commit files. Any inconsistency falls back to the authoritative full-scan
  recovery. Measured on a 10,000-generation store with `checkpoint_interval`
  256 (release, rust court, tmpfs): open median 71.6 ms / p95 73.2 ms reading
  **18** commit files, versus 3,789.1 ms / 3,892.0 ms reading 10,001 for the
  forced full path. Receipt
  `bench/receipts/fast-open-20261104T090945Z.json`.
- **Go parity for bounded open (2026-11-04).** WeftFS-Go `store/recovery.go` now
  implements the same generation-addressed walk with the full scan as the
  authoritative fallback; a read-only open never enumerates the commit
  directory and a writable open enumerates once for `next_generation`.
  Measured on a 100,000-generation store, interval 256 (go court, tmpfs,
  synthetic): bounded open median 2.80 ms / 162 commit reads, forced full scan
  2155.65 ms / 100,001 reads, identical recovered states. Receipt
  `bench/receipts/go-fast-open-20261104T190732Z.json`; courts
  `store/fast_open_test.go` (4 tests).
- **Semantics made explicit.** A bounded open records the note `bounded replay
  resumed from the checkpoint at generation N; commit files below it were not
  enumerated`, and `spec/WEFT-MODEL.md` now requires that note plus the
  fallback rules. Older-history gaps below a checkpoint are therefore not
  detected by open; `verify --full` still enumerates every commit and reports
  them. Tests: `tests/extent_cow.rs` (6) and `tests/fast_open.rs` (3),
  including corrupted-chain fallbacks and checkpoint corruption.
- **Not claimed:** these are single-host, page-cache measurements on synthetic
  fixtures, not device-level or fleet claims.
- **Group commit.** `Store::group` applies mutations to a journalled in-memory
  state and publishes exactly one durable commit; rollback and crash-proxy
  tests cover the failure path (`tests/group_commit.rs`). Measured on tmpfs
  over 5 fresh stores, release, rust court: 1,000 creates 89.87 ms → 22.01 ms
  and 1,000 commits → 1; 1,000 grouped writes 21.86 ms. Receipt
  `bench/receipts/group-commit-20261104T092015Z.json`. This is a store-API
  timing on tmpfs, not a device, FUSE, or fleet claim.

- **Streaming I/O and the file bound.** `create_file_streaming*` and
  `append_file_streaming` read the source in 1 MiB steps, compute the frozen
  content hash incrementally, and commit once. A 256 MiB streaming create uses
  ~7 MiB peak RSS versus ~263 MiB for the buffered path (190.7 ms vs 138.4 ms;
  receipt `bench/receipts/streaming-20261104T094742Z.json`). `MAX_FILE_BYTES`
  is now a 1 TiB defensive bound plus `MAX_CHUNKS_PER_FILE = 16,777,216`; every
  offset/size conversion is checked and returns `LimitExceeded` rather than
  truncating. A 512 MiB file is created and fully verified in the court at
  ~15 MiB peak RSS (`tests/streaming.rs`). Go matches both constants with no
  format change (`WeftFS-Go` `b6408fe`).
- **Effective large-file limit (honest):** the manifest is a WCB array, so the
  codec's `max_array_items` decode limit caps readable chunk counts far below
  1 TiB (at 64 KiB chunks, ~200k chunks ≈ 12.8 GiB for the default 1,000,000
  item limit). Neither implementation truncates; both fail closed with
  `LimitExceeded`. Raising that ceiling requires a chunk-table format change
  (the codec already has a chunk table for frames) and is future work.
- **Not claimed:** codec-backed streaming (`CodecAuto` above
  `codec_min_bytes` writes a v2 raw manifest) and streaming arbitrary-offset
  writes (append is the streaming write form; range writes remain
  chunk-materialising).

### Contextual indexes for selector resolution (implemented 2026-11-04, in-memory)

- **What it is:** `src/index.rs` `StateIndex` indexes attribute presence and
  scalar equality (`objects_with_attr`, `objects_by_attr_eq`), outgoing
  relations per `(subject, predicate)`, and `tagged_as` value edges. Candidate
  sets are computed per AST node (`And` intersects, `Or` unions, `Not` uses the
  authorized-set difference only when the inner set is exact) and intersect the
  authorized universe computed before evaluation.
- **Semantics:** candidate narrowing only — `eval_expr` remains authoritative and
  confirms every candidate, so results, evidence, ordering, `matched_count`, and
  ambiguity are identical to the index-free scan. This is enforced by the
  differential court `tests/index_equivalence.rs` (3 randomized states, seeds
  11/29/47, 315 selector/view comparisons, 1,566 assertions, compared against a
  retained scan oracle).
- **Cache:** bounded 8-entry LRU keyed by the snapshot commit hash that produced
  the state; the guard fails closed with `Corrupt` if the index does not
  describe the state (never a silent under-selection).
- **Persistence:** `indexes/<ab>/<commit>.idx` holds a compact binary WFX2
  segment (magic/version, commit, generation, three counts, sorted fixed-width
  sections, trailing `BLAKE3("weft/index/v2", preceding bytes)`) written
  atomically after a commit publishes. Arrays are sorted once at write time;
  load does one sequential read, bounds-checked slicing, and pre-sized map
  insertion. The segment is advisory: the read path accepts it only when
  magic/version, exact lengths, digest, commit key, generation, and
  object/relation/entry counts agree with the replayed state. Any failure
  records a reject in `Store::index_stats()` and rebuilds from state, so query
  results are unchanged. WFX1 segments are not interpreted; they are discarded
  and rebuilt. Only complete indexes are persisted. Read-only opens never write
  segments; GC deletes segments not named by the head, a snapshot, or a
  checkpoint commit; `verify` ignores them. See `tests/index_persistence.rs`.
- **Measured:** synthetic in-memory state, 10,000 objects + 10,000 relations,
  100 queries per kind, release, medians. Relation 132.795 ms → 0.689 ms warm
  (13.028 ms cold); tag 148.015 ms → 2.051 ms warm (14.044 ms cold); attr-eq
  1.640 ms → 1.239 ms warm. Receipt
  `bench/receipts/index-bench-20261104T085157Z.json`; labelled as a resolver-only
  synthetic measurement, not disk/FUSE/end-to-end.
- **Persistence measurement** (`bench/receipts/index-persist-20261104T141248Z.json`):
  10,000 objects / 10,000 relations, release, synthetic/tmpfs. Cold build
  9.83 ms; write including build 12.00 ms; WFX2 segment 2,423,650 B; peak RSS
  171,756 KiB. First relation query after a fresh reopen is 4.70 ms from the
  segment vs 12.44 ms rebuilding (tag 6.23 vs 14.21 ms). The superseded WFX1
  measurement (`index-persist-20261104T140435Z.json`, 3,065,209 B) was slower
  than rebuilding (19.05 vs 14.17 ms relation; 21.11 vs 12.96 ms tag); its
  canonical-WCB decode was the cost. WFX2 is retained for restart survival and
  is now measured as a first-query win.
- **Not claimed:** segments are per-commit full snapshots, not
  snapshot-incremental; the write path builds the whole index after each commit.
  GC retention is exact-commit membership, not graph reachability. Go had no
  segment parity at this subphase; R26 adds WFX2 persistence in both
  directions.

### F3 — Context mounts, ambiguity court, time travel, and replay bundle (Rust + Go)

- **`mount --context NAME|weft://...`** (both frontends) makes the resolved
  Context Root's ordered, capability-authorized result set the read-only mount
  root. Entries use the established deterministic names (percent-escaped
  basename + `--` + 8 hex ObjectId, extended in 8-character steps on
  collision). `readdir` and `lookup` are fed by one helper, so they cannot
  disagree; every authorized result appears. The constant `.weft` exact-id
  surface remains reachable. Capability-scoped mounts are forced read-only and
  resolve (and therefore filter) before names or counts are produced.
- **`mount --at RFC3339|unix-ns`** (both frontends) walks the published parent
  chain of the base snapshot (or head), selects the newest commit with
  `timestamp_ns <= t` (ties: higher generation), and mounts that state
  read-only. A time before the first commit fails with
  `NotFound: no commit at or before … ns in the mounted lineage`; no state is
  substituted. `--context` pins its own snapshot, so `--context` + `--at` is
  rejected explicitly.
- **Concurrent mounts:** read-only opens take no writer lock in either
  implementation. A FUSE integration test mounts three different roots at once
  and then commits through a normal writer; the writer succeeds and the pinned
  mount does not observe the later commit. The scaled court spawns **100 FUSE
  daemons concurrently** (50 Rust, 50 Go) over 100 genuinely distinct Context
  Roots — 50 `by:task`, 25 `by:model`, and 25 `derived_from` views over 200
  corpus objects, 25 model objects, 10 grounding docs, and 599 typed relations.
  All 100 mounted; 425 file reads matched their declared SHA-256, one
  representative `weftfs cat` per mount matched the store bytes, all 100
  listing digests were distinct, and 0 daemons survived unmount. Mount latency
  under that contention was p50 31.6 s / p95 32.5 s / max 32.6 s; wall to
  100-mounted-and-verified was 100.0 s; daemon RSS sum 1.62 GiB and cgroup peak
  1.56 GiB, inside the unchanged 3 GiB court limit.
- **1,000 concurrent mounts (scale court, explicit overrides):**
  `WEFT_CONTEXT_MOUNTS=1000` builds 2,000 corpus files, 250 models, 10 grounding
  docs, 5,999 typed relations, and 1,000 distinct Context Roots (500 `by:task`,
  250 `by:model`, 250 `by:derived`), spawns 500 Rust + 500 Go daemons before
  awaiting any, and verifies every mount: listing digest against the corpus
  construction oracle (all 1,000 distinct), every one of 4,250 mounted files
  against its declared SHA-256, one `weftfs cat` byte comparison per mount,
  `context resolve` id-for-id on every 50th mount, read-only denial, clean
  unmount, no survivors. With `24g / 8 CPUs / 4,096 pids / 4,096 nproc` the
  court **cannot** reach 1,000: 747 daemons spawned, 692 mounted, 55 mount
  failures, 107 cgroup OOM kills and 98 pids-limit fork refusals, 156 daemons
  lost — 400 is the largest N tested that passes every step at that budget.
  With `52g / 8 CPUs / 16,384 pids / 16,384 nproc` **1,000/1,000 mounts pass
  twice** (1,000 distinct digests, 4,250 file hashes, 1,000 cat comparisons,
  1,000 clean unmounts, 0 survivors; `semantic_match=true`), at mount latency
  p50 197.0 s / p95 330.2 s / max 331.7 s, 1,785.8 s to all-mounted-and-verified,
  46.44 GiB daemon RSS sum (max 80.5 MiB per daemon), 6,751 threads, cgroup peak
  40.92 GiB. Above 100 mounts the corpus is built by
  `examples/context_mount_corpus.rs` through the shipped store API with group
  commits (the corpus is byte-identical to the CLI fixture; one commit per
  operation would make commit-chain replay, not mounting, the measured cost).
  Every limit override is recorded in the receipt; none is disabled. Details,
  the per-daemon memory curve, and the exact failure modes are in
  `bench/RESULTS-context-mounts-1000.md`.
- **Ambiguity:** `require_one` now re-evaluates without pagination and lists
  every competing ObjectId in the `Ambiguous` message. `context materialize
  --require-one` checks cardinality before any filesystem work and leaves no
  output path on failure.
- **Replay bundle:** Rust `export-bundle` writes `weft-receipt.tar.zst` with the
  portable store, the complete relation graph including Context Root records,
  `manifest.json` (ContextRootIds, snapshot/selector ids, content hashes,
  source revisions, court command), `contexts.tsv`, `checks.sha256`,
  `materialized.sha256`, `replay.sh`, and a README. Go extracts it with
  `tar`/`zstd`, opens it, runs `verify --full`, resolves and materializes every
  Context Root, and re-hashes every store entry and output file against
  `manifest.json` using a stdlib-only verifier. No Rust runs at Go replay time.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse
  --all-targets"` reports **81 passed, 0 failed, 2 ignored** (unchanged
  CAP_SETUID and kernel-7.3 limitations). `scripts/docker-court.sh fuse "cd
  /work/WeftFS-Go && go test -race ./..."` passes for `store` and `fusefs`,
  including four new context-mount/time-travel/concurrency tests and an
  ambiguity message test; `go vet ./...` and `CGO_ENABLED=0 go build ./...`
  pass.
- **Demo receipts (all `pass`):**
  - `bench/receipts/context-mounts-20261103T210625Z.json` (6/6) — three
    simultaneous mounts, per-file SHA-256 and ContentId agreement, one
    content-addressed manifest shared by two contexts, read-only denials, and a
    live writer while mounted.
  - `bench/receipts/ambiguity-20261103T210633Z.json` (4/4) — full ordered set,
    `Ambiguous` with both ids, two-result mount listing, no singular output.
  - `bench/receipts/time-travel-20261103T210633Z.json` (4/4) — 100 versions,
    snapshots 1/50/100 mounted together, exact historical bytes after a live
    rename/retag.
  - `bench/receipts/context-mounts-20261104T125344Z.json` (5/5) — scaled
    `WEFT_CONTEXT_MOUNTS=100`: 100 concurrent mounts, 425/425 file checks,
    100/100 distinct listing digests, 100/100 clean unmounts, 0 survivors;
    determinism `context-mounts-20261104T125344Z.determinism.txt` =
    `semantic_match=true`.
  - `bench/receipts/context-mounts-20261105T122643Z.json` (5/5) and
    `bench/receipts/context-mounts-20261105T131116Z.json` (5/5) —
    `WEFT_CONTEXT_MOUNTS=1000` at `52g / 8 CPUs / 16384 pids / 16384 nproc`:
    1000/1000 mounts, 4250 file SHA-256 checks, 1000 distinct listing digests,
    1000 cat comparisons, 20 sampled resolves equal to the construction oracle,
    1000 clean unmounts, 0 survivors; determinism
    `context-mounts-20261105T131116Z.determinism.txt` = `semantic_match=true`.
    Analysis in `bench/RESULTS-context-mounts-1000.md`.
  - `bench/receipts/context-mounts-20261105T135805Z.json` (5/5) — the same
    court at `24g / 8 CPUs / 4096 pids / 4096 nproc` with 400 mounts: the
    largest N tested that passes every step at the directed budget.
  - `bench/receipts/context-mounts-20261105T145819Z.json` (3/5) — 500 mounts at
    the directed budget: all 500 mounted and all file/cat checks passed, but 4
    Go daemons died mid-run (transient thread spikes), so 496 unmounted cleanly.
  - `bench/receipts/context-mounts-20261105T140342Z.json` (2/5,
    `INCOMPLETE 692/1000`) — the directed budget at 1,000 requested: 747
    spawned, 692 mounted, 55 mount failures, 107 OOM kills, 98 pids-limit
    refusals, 156 daemons lost. Records the exact break point.
  - `bench/receipts/context-mounts-20261105T150653Z.json` (5/5) —
    `WEFT_CONTEXT_MOUNTS=100` in the unchanged default court
    (`3g / 4 CPUs / 1024 pids`).
  - `bench/receipts/time-travel-20261104T125947Z.json` (6/6) — scaled
    `WEFT_TIME_TRAVEL_VERSIONS=10000`: 10,000 sequential versions, 101
    snapshots, 10 mounted at once (`s1`, `s1100` … `s10000`), 164.1 s ingest
    (60.9 versions/s), reopen p50 6 ms, mount p50 2.28 s, verification 77 ms,
    all snapshots byte-identical after a later write and live rename;
    determinism `time-travel-20261104T125947Z.determinism.txt` =
    `semantic_match=true`.
  - `bench/receipts/replay-bundle-20261103T210824Z.json` (4/4) — bundle
    sha256 `940579db909ba8a3360f73377448ce90df455278a37c5a0b02f6bb7a584bdbd2`,
    17 store entries, 2 contexts, 3 context files, 2 resolved identities, zero
    mismatches.
  - `bench/receipts/context-mounts-determinism.txt` — `semantic_match=true`
    between two Context-mount runs (run ids and wall-clock fields excluded).
- **Bundle:** `bench/bundles/weft-receipt.tar.zst` (6,662 bytes), sha256
  `940579db…`. Reproduction:
  `scripts/docker-court.sh fuse "bash scripts/replay-bundle.sh --bundle
  bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out"`.
- **Not claimed:** io_uring, EKBLOOM, or any acceleration. Time travel is
  correctness-only. The SHA-256 value in the demos is a declared attribute and
  a byte-identity check; the normative content root is the store's BLAKE3
  ContentId, which `verify --full` re-checks (the scaled modes run it before
  mounting). The scaled receipts are labelled single-host Docker FUSE court,
  synthetic deterministic fixtures, local court bind-mount store, explicit
  court limit overrides recorded in the receipt, and **not a fleet claim**. The
  100-mount run uses the unchanged default court (`3 GiB / 4 CPUs / 1,024
  pids`); the 1,000-mount run raises the hard limits to `52g / 8 CPUs / 16,384
  pids / 16,384 nproc` and the ceiling run uses `24g / 8 / 4,096`. No limit is
  ever disabled, and 1,000 is **not** claimed to fit the default court: at the
  directed 24 GiB / 4,096 budget the court reached 692 mounted and a complete
  pass at 400. Mount latency includes CPU contention among all daemons starting
  at once; it is not a per-mount steady-state cost. Each receipt records its
  exact revisions: the context mount, ambiguity, and time-travel default runs
  used WeftFS `f3d1bc1` / WeftFS-Go `acf6df6`; the replay run used WeftFS
  `bcedd26` / WeftFS-Go `7693ad4`; the 2026-11-04 scaled runs used WeftFS
  `6316fd4` / WeftFS-Go `fce700a`; the 2026-11-05 1,000-mount and ceiling runs
  used WeftFS `6e4aea1` / WeftFS-Go `8372e6f`.

### Twelve-step showcase — implemented, all twelve pass

- `scripts/showcase.sh` executes the mandated twelve scenarios inside the FUSE
  court and writes a schema-versioned receipt plus a raw console log:
  `bench/receipts/showcase-20261103T171440Z.json` and `.log` (all twelve
  `pass`; schema `weftfs.showcase.receipt/1`).
- A second run under a different receipt name compared every field except
  wall-clock timestamps: `semantic_match=true`
  (`showcase-determinism-20261103T171440Z.determinism.txt`).
- Every step records its exact commands, fixture hashes, machine-checked
  assertions (`cmp`, SHA-256, JSON field equality), observed outputs, court
  limits, image/kernel/tool versions, and repository revisions (WeftFS
  `4cb40af`, WeftFS-Go `74c8c90`, WeftCodec `a60d9f5`, WeftCodec-Go `b2beeab`).
- Step 9 is explicitly labelled a **process-fault proxy** (`WEFT_CRASH_PHASE`),
  not physical power loss. Step 10 records that the codec CLI has no remote
  fetch policy; missing/mismatched dependencies fail closed, while
  `DependencyDenied` remains an API-level class.
- The codec-cycle fixture generator is `examples/codec_cycle_probe.rs`; the
  real `weftcodec archive create` CLI is what rejects the forged cycle.
- Deterministic fixtures use fixed ObjectId seeds and a fixed clock; a re-run
  with `SHOWCASE_COMPARE_WITH` must match on every field except wall-clock
  timestamps.

### P7 benchmark court — implemented, first measured results

- `docker/bench.Dockerfile` and `scripts/docker-court.sh bench` add a
  FUSE-capable, OOM-limited benchmark court with `zstd`, `xz`, `gzip`,
  `brotli`, `sqlite3`, `fio`, Rust, and Go. Unknown court kinds are still
  rejected and the memory/swap/pids/CPU limits are unchanged.
- The existing `fuse` court was re-run after the runner change at `244dc26`:
  `fuse_posix` reported 9 passed and 1 ignored (the ownership test needs
  `CAP_SETUID`). The added `/mnt/weft-bench` tmpfs does not change the POSIX
  result.
- Codec baselines (`bench-codec-20261103T175847Z.json`): external zstd 3/9/19,
  gzip 9, xz 9, and brotli 9 on seven deterministic corpora, plus WeftCodec
  RAW/ZSTD/DELTA/DICTIONARY/auto with marginal and whole-object accounting.
  Every decode is verified before its sample is accepted.
- Store benchmark (`bench-store-20261103T175920Z.json`): 100/1,000/10,000
  objects, 1–3 relations each, snapshots, selectors, churn, full verify, GC,
  and on-disk bytes against the real Rust store API. Peak RSS 169 MiB.
- Filesystem benchmark (`bench-fs-20261103T180751Z.json`): `fio` against a
  Rust WeftFS mount, a Go WeftFS mount, and a tmpfs reference in one container.
  The Go mount completed reliably (`go_status: ok`). The tmpfs column is
  reference-only and explicitly non-equivalent.
- Headline scoped observations (not claims of superiority):
  - WeftCodec ZSTD-3 is 545 bytes on 4 MiB repetitive data and RAW is
    4,194,404 bytes on 4 MiB random data; `auto` selects accordingly.
  - DELTA/DICTIONARY do not reduce whole-pair bytes when the required base is
    counted (near-dup 1 MiB: 1,128,560 / 1,073,309 vs 1,048,709 for ZSTD-3).
  - Store relation/tag queries cost ~414–424 ms at 10,000 objects because
    evaluation scans all relations per object; the n-result kind query costs
    1.93 ms. Replay is 32.6 s and full verify 70.3 s at 10,000 objects.
  - Filesystem writes rewrite the whole file per write: ~20 IOPS for 1 MiB
    sequential writes and ~73 IOPS for 4 KiB random writes (Rust), with
    ~7.6 ms p50 for a 4 KiB write with `fsync`.
  - A 1,000-object ingest measurement varied by 86% between runs; the range is
    reported rather than a single point.
- `bench/scripts/summarize_bench.py` regenerates the tables and
  `bench/scripts/compare_bench.py` compares two receipts for stability.
  `bench/RESULTS-v1.md` publishes every loss and the explicit gaps.

### P7 comparative pass — external SQLite, matched codec, cold-cache fio (implemented)

- External context/query baseline: `bench-external-20261104T000122Z.json`
  (plus corpus manifest `external-corpus-20261104T000122Z.json`). Schema-indexed
  SQLite relational and adjacency-table variants at 100/1,000/10,000 objects
  using the exact `bench_store` payloads, ObjectIds, relations, snapshots, and
  generations. Warm and cold (per-file page-cache eviction) samples, query
  plans, churn, index build time/bytes, and bytes on disk are recorded. The
  adjacency variant is labelled “SQLite relational/adjacency baseline”; no
  Neo4j/SPARQL/dedicated graph database was run.
- Same-revision store baseline for the comparison: `bench-store-20261103T235127Z.json`
  at HEAD `2388352` (487.9 s wall, peak RSS 169,356 KiB), 100/1,000/10,000
  objects, with the query/churn/snapshot/open/bytes medians used in
  `bench/RESULTS-comparative-v1.md` §3.4.
- Matched Rust/Go WeftCodec court: `bench-codec-matched-20261103T232158Z.json`
  (first run `T225536Z` retained for stability). RAW/ZSTD/REFERENCE/DELTA/
  DICTIONARY/TENSOR/TOKENS as CLI processes, medians and p95, CPU user/sys,
  max RSS, frame bytes, per-iteration decode verification, and cross-decode in
  both directions. Process overhead measured separately (Rust 0.326 ms, Go
  0.387 ms) and not subtracted; no library-vs-library comparison is claimed.
- Cold-cache concurrent fio: `bench-fs-cold-20261103T232216Z.json` (stability
  run `T233740Z`). **Provenance only, per rule 13: the Rust/Go reference
  filesystems over FUSE are not performance contenders and this row is excluded
  from every claim.** QEMU guest with a genuine guest
  `sync; echo 3 > /proc/sys/vm/drop_caches` cold run, `numjobs` 1/4/16,
  ext4/XFS plus Rust and Go WeftFS over FUSE, two repetitions per cell. The
  Rust and Go mounts differ in `allow_other`/`max_read` defaults; that is
  published as a limitation.
- Whole-disk ZFS and Btrfs (2026-11-04), **provenance only and excluded from
  every claim under rule 13**:
  `bench-fs-cold-20261104T071922Z.json` runs ext4/XFS/Btrfs/`zfs-whole` in one
  guest court, one repetition per cell, 30 fio runs per target. For `zfs-whole`
  the guest passed the raw `/dev/vdb` block device to `zpool create -f -m
  /data/tank tank /dev/vdb`; `zpool status -P` shows the GPT partition
  `/dev/vdb1` that ZFS created and owns. No partition, loop device, or
  file-backed vdev is used. The guest runs stock `systemd-udevd 252.39` so
  libzfs' libudev partition wait can complete; without udevd the exact error is
  `cannot label 'vdb': failed to detect device partitions on '/dev/vdb1': 19`.
  OpenZFS 2.1.11-1+deb12u1 (DKMS, kernel 6.1.0-54-amd64); Btrfs uses
  btrfs-progs 6.2 on the host and the guest `btrfs.ko`. Scoped `numjobs=1` cold
  medians: seq-write-1m ext4 1,142.9, XFS 1,142.9, Btrfs 1,142.9, ZFS 1,333.3;
  fsync-write-4k ext4 410.3, XFS 390.2, Btrfs 457.1, ZFS 800.0; rand-read-4k
  ext4 9,846.2, XFS 9,846.2, Btrfs 8,533.3, ZFS 128,000 (ARC-served). At
  `numjobs=16` fsync-write-4k: ext4 3,160.5, XFS 1,855.1, Btrfs 911.0, ZFS
  6,736.8. ZFS reads are **not cold**: `drop_caches` does not evict the ARC;
  ranking or superiority claim is made. The earlier file-backed ZFS run
  `bench-fs-cold-20261104T014727Z.json` is retained as a separate, labelled
  result, not as the whole-disk claim.
- Block-layer lying-disk matrix (2026-11-04): `bench/receipts/blkdebug-20261104T072008Z.json`,
  30 iterations, 5 per mode, in the vm court. `dm-flakey drop_writes` (2 s up,
  then silent drop) lost acknowledged commits in 5/5 iterations: recovered
  generations 87/89/89/88/89 versus last returned 319/218/334/292/246
  (232/129/245/204/157 commits lost), 0 partial, both implementations agreed,
  `verify --full` clean. `dm-flakey error_writes` failed the first post-window
  write in 5/5, recovered exactly the last returned generation (88/79/8/7/6),
  0 loss, 0 partial. QEMU `snapshot=on` total write drop recovered generation 2
  in 5/5 and lost all acknowledged commits (last returned 52/77/205/182/128).
  QEMU blkdebug write-error and flush-error were attempted and are **blocked at
  ext4 mount** in 10/10: the first injected write is the mount superblock at
  block 0, then `JBD2: recovery failed` / `EXT4-fs (vdb): error loading
  journal`, and the guest never reaches `GUEST_WRITING`. No result is invented
  for those two modes. This is a block-layer lying-disk model, not physical
  media power loss.
- Host page-cache attempt (2026-11-04):
  `bench/receipts/hostcache-probe-20261104T072314Z.json`. The unprivileged court
  (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches`: mode `200`, exact
  refusal `Read-only file system`; no host cache was dropped. A per-file
  `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file succeeded and
  is recorded as per-file eviction only. QEMU `cache=none` uses `O_DIRECT`, so
  guest data does not occupy the host page cache on that path. Host page-cache
  loss remains **not tested**.
- Energy/counter probe: `energy-probe-20261103T225547Z.json` recorded that
  `perf` was not installed and no readable RAPL `energy_uj` existed. The
  2026-11-04 probe `perf-counters-20261104T015026Z.json` installs Debian
  `linux-perf 6.1.190-1` inside the ephemeral bench court and records
  cycles/instructions/branches/branch-misses/cache/context-switch/page-fault
  counters for RAW/ZSTD encode and decode (3 repetitions, all runs complete).
  RAPL is still unavailable: `/sys/class/powercap` exists but no `energy_uj`
  file is visible inside Docker (`/sys/devices/virtual/powercap` is masked).
  The VM guest reports `powercap present`, `energy_uj files 0`, `perf absent`.
  CPU time and perf counters are **not** called energy; no joule/watt number is
  reported.
- Analysis: `bench/RESULTS-comparative-v1.md` contains the exact commands,
  environments, revisions, tables, every loss, stability deltas, and explicit
  limits. Scoped examples: SQLite point lookups are ~1.2–1.8 µs warm at 10,000
  versus ~457.6–494.4 ms for the store’s relation/tag selectors; WeftFS
  sequential 1 MiB writes are 9.9–12.1 IOPS (Rust) and 2.4–3.2 IOPS (Go)
  versus 1,143–2,784 (ext4) and 1,333–2,702 (XFS); Rust `fsync`-write p99
  reaches 1.13 s at `numjobs=16`. No winner or world-record claim is made.

### F5 — proof courts (B1–B4)

The F5 code revision is parent `9d767d5`, `WeftCodec` `127bc85`,
`WeftCodec-Go` `71066cf`; `WeftFS-Go` remains `7693ad4` (no F5 code change).
All three F5 receipts below were generated at those revisions after the code
commit, in the unchanged hard-limited courts.

**B1 — decoder bombs (36/36 pass).** `bench/receipts/decoder-bombs-20261103T213314Z.json`
and its raw `.log`. 36 committed adversarial vectors in both codec repos
(byte-identical mirror checked by the court). Both CLIs were run twice per
vector under `timeout 10s /usr/bin/time`; every run exited nonzero with the
expected class in both implementations, no output file, no timeout, no
OOM-kill (137), stable classes across repeats, and peak RSS below 5 MiB.
Class matrix: LimitExceeded 15, Malformed 12, Corrupt 2, DependencyCycle 2,
UnsupportedFeature 2, NonCanonical 1, UnsupportedMode 1, DependencyMismatch 1.
The tiny-body/4 GiB-declared vector is the regression for the fixed zstd
preallocation bug; Rust and Go both report Corrupt with ~3.5/4.7 MiB RSS.
Limits are configurable through the ten requested `decode` flags (plus
structured-item, selector-depth/operand, and archive-entry bounds); defaults
remain the normative spec values.

**B5 — deterministic differential fuzz (1,000,000 inputs; 0 mismatches).**
`bench/receipts/fuzz-diff-20261104T132310Z.json` (direct run) and
`bench/receipts/fuzz-diff-20261104T132413Z.json` (inside `verify-all`). A shared
seed corpus of 44 valid WFT1 frames (RAW, ZSTD, REFERENCE, DELTA, DICTIONARY,
STRUCTURED, TENSOR, TOKEN_SEQUENCE, QUANTIZED_TENSOR, chunked) lives in
`conformance/fuzz/v1/seeds.txt`. Identical xorshift64* generators in Rust
(`WeftCodec/examples/fuzz_differential.rs`) and Go
(`WeftCodec-Go/cmd/fuzzdiff`) produce 1,000,000 inputs with a fixed split:
1/8 raw random bytes (0–2048), 1/8 un-repaired seed mutations (1–8 legacy
operations), 4/8 repaired body/table mutations (1–4 operations with `body_len`,
header CRC32C, and trailer CRC32C recomputed; content hash deliberately left
stale), 1/8 semantic faults (missing/lying/denying dependency, self-cycle
REFERENCE through the DAG entry point, mode bytes 9/127/128/255,
`decoded_len = u64::MAX`, bad chunk extent, wrong content hash, unknown
critical flag, non-canonical STRUCTURED body), and 1/8 clean seeds. Both
decoders ran with `Limits::default()` inside the unchanged hard-limited courts;
corpus hash `4ed6da22ac497a2494a56d53abda1689eef8a5bcbbed381b64f2c3fecf6e12a5`
(BLAKE3-256 over `LE64(len) || input` per input) matched before comparison.

Normative zstd preamble rule (WEFTCODEC-FORMAT §3, applied identically in Rust
and Go to ZSTD, DICTIONARY, and every chunked ZSTD chunk): the payload MUST be
a complete well-formed zstd stream; an empty payload is `Malformed` even when
`decoded_len == 0`; if the zstd frame header declares a content size it MUST
equal `decoded_len` (`Corrupt` otherwise, even when the actual byte count
matches); and the actual decompressed length MUST equal `decoded_len`
(`Corrupt` otherwise). Both languages enforce this with existing dependencies:
Rust `zstd::zstd_safe::get_frame_content_size` and Go `zstd.Header.Decode`
(`HasFCS`/`FrameContentSize`) from klauspost/compress v1.17.11. The Go encoder
now sets `zstd.WithZeroFrames(true)` so empty plaintext emits a valid minimal
frame instead of an empty body. New reject vectors
`038_zstd_empty_body` (`Malformed`) and
`039_zstd_content_size_mismatch` (`Corrupt`) are byte-identical in both repos.

Result lines: 1,000,000 each, 0 panics, **0 mismatches**, identical class
distributions: ok 172,294, Corrupt 174,899, Malformed 466,548, NonCanonical
67,975, DependencyMissing 35,987, LimitExceeded 16,900, DependencyMismatch
14,081, UnsupportedMode 12,608, UnsupportedFeature 12,347, DependencyCycle
12,349, DependencyDenied 12,313, UnsupportedVersion 1,699. Peak RSS 6,304 KiB
(Rust) and 205,888 KiB (Go), inside the 256 MiB bar; elapsed 1,205 ms and
1,964 ms, total 7,000 ms. The historical failing receipt
`bench/receipts/fuzz-diff-20261104T102754Z.json` (544 mismatches: 451 Rust ok /
Go Corrupt on a zstd frame declaring content size 2 while yielding 0 bytes, 93
Rust Corrupt / Go ok on an empty zstd body with `decoded_len = 0`) is retained
as the evidence that motivated the rule. The court is not coverage-guided
fuzzing and not a substitute for the curated B1 decoder-bomb vectors. Wired
into `scripts/verify-all.sh`; `verify-20261105T035513Z.json` passes all 25
courts including `claims`, the authenticated-capability gates, and `fuzz_diff`.

**B4 — randomized POSIX torture (6/6 pass; 1,202,560 completed operations).**
Three FUSE-court invocations at WeftFS `42c82f7` and WeftFS-Go `d7d8d06`, same
`weft/fuse-court:1` image, all six writer/reader configurations (seeds 1–3 ×
both directions): **2,640,000 requested / 1,202,560 completed / 1,437,440
skipped by design**, 9,315,108 ms of torture wall (2 h 35 min 15 s), **129.10
completed ops/s** (283.4 requested ops/s).

| Seed | Direction | Requested | Completed | Skipped | Entries | Manifest SHA-256 (writer = reader) |
|---|---|---:|---:|---:|---:|---|
| 1 | rust→go | 240,000 | 112,098 | 127,902 | 96 | `5a5c73ea32f98d7a3995d76b535758217a9e9889fde29739aebd51128ff244bf` |
| 1 | go→rust | 640,000 | 298,004 | 341,996 | 97 | `05b5b5b82f5b1882584e71b2f5c2606cf922dd6de099d1b6ab3a61468b2332af` |
| 2 | rust→go | 240,000 | 112,386 | 127,614 | 97 | `dc82c98d33d46158d08abd8c510eb3a9d8a354d73b642b3001f25e44e9b798d0` |
| 2 | go→rust | 640,000 | 270,201 | 369,799 | 96 | `1aa7fa3c7d6a843075b9f01e1e4d04383d69c0f3eab65301680beefb2c52f679` |
| 3 | rust→go | 240,000 | 112,025 | 127,975 | 97 | `3f16185c83686d80edb64d365195f179b9247ac5b114186bcbb6a4f6e616d1da` |
| 3 | go→rust | 640,000 | 297,846 | 342,154 | 97 | `81422cb2944b6ca2963319365ba06885b657565da371b62ae0bd8a0a315fa299` |

The three receipts are `bench/receipts/posix-torture-20261104T162109Z.json`
(rust→go, seeds 1–3: 336,509 completed of 720,000 requested, 3,638,847 ms),
`bench/receipts/posix-torture-20261104T172156Z.json` (go→rust, seeds 1–2:
568,205 completed of 1,280,000 requested, 3,807,418 ms), and
`bench/receipts/posix-torture-20261104T182537Z.json` (go→rust, seed 3:
297,846 completed of 640,000 requested, 1,868,843 ms), each with its raw `.log`
and `.results.tsv` beside it. Totals were recomputed independently from the six
raw per-run result JSON files and the six manifest files, not from the harness
summary: in every run `completed + skipped = requested`, `op_counts` sums to the
requested count, every manifest SHA-256 was recomputed from its entries and
equals both recorded digests, and all six reader mounts reproduced the writer's
exact manifest. Every configuration passed all assertions (per-operation bytes,
size, mode, nlink, symlink target, xattrs; a full-tree check every 100
operations; `verify --full` after unmount; writer/reader manifest equality).
Operations cover create/write/read/truncate/append/rename/rename-over/link/
unlink/mkdir/rmdir/symlink/readlink/xattr/fsync/open-unlinked.

The size asymmetry is deliberate and is a real product finding from the run
itself. At that revision the Go store's `Open` (`store.CommitIndex` →
`LoadStateAt`) parsed every commit file and had no bounded open: three
consecutive probes measured 18.65–19.07 s to mount a store with 287,759 commits,
while Rust mounted the same store in 305 ms. The harness's mount wait is a fixed
15 s (300 × 50 ms), so Go-reader runs are capped at 240,000 requested (~133k
commits, ~9 s mount) while Rust-reader runs use 640,000. The retained failing
receipt `bench/receipts/posix-torture-20261104T153125Z.json` is the
520,000-request rust→go attempt that completed 242,422 operations and then
exceeded the 15 s wait at 287,759 commits; its step store is not kept. No product
code, assertion, or harness default was changed. **Update 2026-11-04:** Go now
implements the same bounded replay (`WeftFS-Go` `1461c04`,
`bench/receipts/go-fast-open-20261104T190732Z.json`: 100,000 generations open in
2.80 ms / 162 commit reads versus 2155.65 ms / 100,001 reads full-scan). The
numbers above were measured before that change and the court sizes were left
unchanged, so they remain a record of the old path, not of current behaviour.

Caveat (unchanged): this is a self-written fsx-style harness in a single-host
Docker court, not `pjdfstest`/`fsstress` (not installed in the court; no
substitute run is claimed), and it is not a fleet claim. The earlier
1,000,002-request run (`bench/receipts/posix-torture-20261104T142424Z.json`,
453,432 completed) and the 13,694-operation run
(`bench/receipts/posix-torture-20261103T213317Z.json`) are retained as history;
the harness still accepts `WEFT_POSIX_OPS`, `WEFT_POSIX_OPS_MINIMUM`,
`WEFT_POSIX_SEEDS`, and `WEFT_POSIX_ONLY` with unchanged defaults (3,200 /
2,000 / all seeds / all six configurations).

**B2 — crash-consistency torture (100,000/100,000 pass).**
`bench/receipts/crash-torture-20261104T104233Z.json` (18 MB; log and TSV stored
losslessly as `.zst`). The harness uses a fresh three-generation store per
iteration, a pre-generated payload pool, and a clean fast path for
`verify --full`; the old-or-new/partial and phase-specific assertions are
unchanged (`WEFT_CRASH_FRESH_STORE=0` retains the historical growing-store
mode). Result: 100,000 iterations, 100,000 completed, 0 failures, 0 bad verify
states; observed old 66,667 / new 33,333 / partial or missing 0; verify clean
66,667 / accepted unpublished-tail 33,333; phases 33,334/33,333/33,333;
5,196,158 ms (86 min 36 s), **19.2 iterations/s**. Every iteration was
recomputed from the 14,822,217-byte NDJSON independently of the harness
counters. The 10,000-iteration receipt `crash-torture-20261104T013521Z.json`
is retained. Alternating Rust/Go writers and readers; every
crash followed by the other implementation's `verify --full`, exact `/f` byte
check, and exact `/` listing. This remains a **process-fault proxy**, not
physical power loss. The earlier 1,000-iteration receipt
`crash-torture-20261103T213446Z.json` (2.9 it/s) is retained as history.

**B3 — receipt bundle.** `scripts/package-receipts.sh` writes
`bench/bundles/receipts-<stamp>.tar.zst` with manifest, checksums, revisions,
receipts/raw logs, conformance corpus, harness/Dockerfiles, and dmesg status.
`scripts/verify-receipts.sh <bundle> --repo-root .` extracts to a private
temporary directory and independently verifies every sha256, rejects unlisted
files, checks all four revisions against git, and rejects unsafe tar entries.
The bundle records `dmesg: unavailable (no CAP_SYSLOG)` when applicable.

### F9 — machine-checked commit/recovery model and VM power-cut court (implemented 2026-11-03)

**Model checking.** `verification/tla/WeftCommit.tla` is a bounded TLA+
abstraction of the commit/recovery state machine: immutable commit records,
two alternating superblock slots, objects that must be durable before a commit,
arbitrary crash boundaries, recovery scanning for the highest valid generation,
and replay. TLC 1.7.4 (jar pinned by sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`) checks
`TypeOK`, `NoPartial`, `HighestValid`, `NoLostDependency`, `Durability`,
`ReturnedObjectsDurable`, and `ReplayIdempotent` at `MaxGen=8`: 615 states
generated, 591 distinct, search depth 68, no error. Receipt
`bench/receipts/tla-20261103T224446Z.json`.

`verification/tla/WeftCommitBuggy.tla` is deliberately flawed: recovery trusts
the highest commit file, ignoring superblock visibility and CRC validation.
TLC finds the expected `NoPartial` violation at `MaxGen=3` (32 generated, 29
distinct, depth 9). The counterexample is
`verification/tla/WeftCommitBuggy.counterexample.txt`; it shows a torn generation-1
commit replayed as an operation prefix.

**Power-loss rig.** `scripts/powerloss_court.sh` (new `vm` Docker court,
`docker/vm.Dockerfile`) builds both CLIs, a read-only raw ext4 root disk with a
Debian 6.1.0-54 kernel/initramfs, and a fresh qcow2 data overlay per iteration.
The guest initialises the store and commits 64 KiB writes in a loop without
`sync(1)`; the host waits for returned commits, sleeps a seeded pseudo-random
delay, and `SIGKILL`s QEMU. Reboot uses the same image with ext4 journal replay
and **no fsck**; Rust and Go independently recover, `verify --full`, and read
the file. The host asserts identical generation and snapshot root, exact
expected content, and that every returned commit survived. A never-fsynced
control file on the data disk distinguishes this from a process abort.

Published primary run (`cache=none`, KVM): 30/30 pass, 28 old states, 2 new
states, 0 partial, 0 durability failures, 0 Rust/Go disagreements, 1 accepted
documented unpublished tail, unsynced-control loss 30/30. Receipt
`bench/receipts/powerloss-20261103T224453Z.json`. Secondary writeback run:
10/10 pass, 0 unpublished tails, control loss 10/10; receipt
`bench/receipts/powerloss-20261103T224652Z.json`. The acceptance predicate
allows only the documented `above the highest published superblock generation`
note or an unparseable commit file strictly above the recovered generation;
all other verifier problems fail the iteration.

**Lying-fsync device model (2026-11-04).** `POWERLOSS_CACHE_MODE=unsafe` uses
QEMU's `cache=unsafe`, which accepts guest flush requests and does not honour
them. The same SIGKILL/reboot/verify matrix ran 30 iterations
(`bench/receipts/powerloss-20261104T014805Z.json`): 30/30 pass, 27 old, 3 new,
0 partial, 0 durability failures, 0 Rust/Go disagreements, unsynced-control
loss 30/30. **No data loss was observed.** That is the honest result and it has
an exact explanation: SIGKILL of QEMU discards the guest page cache and QEMU
state, but the host page cache holding QEMU's already-written image data is not
removed, so a lying guest-visible flush does not create loss under a process
kill. The mode is labelled a **lying-fsync device model**, not physical media
power loss; real disk-firmware lying and host power loss remain untested.

**Block-layer lying-disk court (2026-11-04).** `scripts/blkdebug_court.sh`
prepares one clean ext4 store base, copies it per iteration, and combines a
block-layer fault with the same SIGKILL/reboot/Rust+Go verification. Modes are
`control`, `dm-flakey-drop`, `dm-flakey-error`, `write-drop-all` (QEMU
`snapshot=on`), and two blkdebug attempts. `dm-flakey` is created inside the
guest over `/dev/vdb` with a 2 s up interval so mount completes, then a 1 h down
interval; `drop_writes` silently discards writes while returning success, and
`error_writes` returns EIO. In `bench/receipts/blkdebug-20261104T072008Z.json`:
control 5 old; dm-flakey-drop 5/5 lost (232/129/245/204/157 acknowledged
commits lost, recovered 87/89/89/88/89); dm-flakey-error 5/5 old, 5/5
COMMIT_FAIL, 0 loss; write-drop-all 5/5 recovered generation 2 and lost all
acknowledged commits; 0 partial in all completed modes. blkdebug write-error
and flush-error are 10/10 **blocked**, not passed: blkdebug arms at open, so the
first injected write is ext4's mount superblock (`I/O error, dev vdb, sector 0`,
`JBD2: recovery failed`, `EXT4-fs (vdb): error loading journal`) and the guest
never reaches `GUEST_WRITING`. QEMU 7.2 blkdebug accepts `read_aio`,
`write_aio`, and `flush_to_os`; it rejects `flush_do_sync`, and `errno=0` still
writes, so there is no silent selective write-drop primitive. `dm-flakey` is
the arm-after-mount equivalent used for the data results.

**Host page-cache probe (2026-11-04).**
`bench/receipts/hostcache-probe-20261104T072314Z.json` records that the
unprivileged court (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches` (mode
`200`, exact refusal `Read-only file system`), so no host cache was dropped. A
per-file `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file
succeeded and is labelled per-file eviction only. QEMU `cache=none` uses
`O_DIRECT` for guest data. Host page-cache loss remains untested.

**Claim boundary.** This is a QEMU VM power-cut proxy plus a lying-fsync device
model and a block-layer lying-disk model, not physical media power loss or host
page-cache loss. The TLA+ model is checked; the implementation is not verified,
and there is no refinement proof. See `verification/README.md` for the exact
mapping, assumptions, and remaining gaps (checkpoints, GC, capabilities,
multi-writer, physical rig).

### E — Independent Python reader and PyTorch dataset (implemented 2026-11-04)

`python/` ships `weftcodec` 0.5.0 (Apache-2.0) as a **third independent
implementation** with no Rust or Go extension modules. It provides canonical
WCB encode/decode, WFT1 header/table parsing with CRC32C and strict structural
validation, domain-separated BLAKE3 content roots, and exact decoders for RAW,
ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE, and
QUANTIZED_TENSOR. Modes 9–14 (`FLOAT_TENSOR`, `TENSOR_SHARDED`, `WEIGHT_DELTA`,
`KV_CACHE`, `ACTIVATION_STREAM`, `SEMANTIC_DELTA`) raise the typed
`UnsupportedMode`; there is no approximate decode and no opaque-body fallback.
`weftcodec.torch.WeftMapDataset` walks a mounted tree in sorted POSIX-relative
order and returns `(path, bytes)` or, when `dtype` is supplied,
`torch.frombuffer` tensors; `torch` is an optional extra and is never imported
at module import time.

**Court and evidence.** New minimal image `weft/python-court:1`
(`docker/python-court.Dockerfile`, from `python:3.12-slim`; image id
`sha256:67d4cf7ba67c9c2e3c6229e271b1b012a61e909852f422da0852bfaa755797e0`),
wired as the `python` mode of `scripts/docker-court.sh` with the same hard
limits as every other court (3 GiB memory/swap, 1024 pids, 4 CPUs, 1 GiB
`/tmp`, `no-new-privileges`, `cap-drop=ALL`). Receipt
`bench/receipts/python-bindings-20261105T000147Z.json` with its raw `.log`
(WeftFS `1c4b249`, WeftCodec `f1d32e3`, kernel `7.3.1-1-cachyos`).

**Counts (from the receipt, not estimated).** 111 frame vectors discovered; 61
asserted, 61 passed, 0 failed. The 50 skips are explicit and labelled: 7
historical `superseded_by` vectors and 43 frames in unimplemented modes 9–14
(13 valid, 30 reject; the Python reader returns `UnsupportedMode` where Rust
returns the mode-specific class). All asserted valid vectors decode
byte-exactly and match the recorded plaintext hash, content hash, mode, flags,
features, and chunk count; all asserted reject vectors raise the recorded
class. Cross-language: 11/11 cases pass — Rust CLI encodes RAW, ZSTD, chunked
RAW/ZSTD, REFERENCE, DELTA, DICTIONARY, TENSOR, TOKENS, QUANTIZED_TENSOR, and
STRUCTURED, and the Python reader verifies and decodes each byte-exactly, with
`inspect --json` fields matching Rust's. Full suite 166 tests: 115 passed, 51
skipped (50 vector skips plus the PyTorch module, absent by design), 0 failed.

**Artifact.** `python/dist/weftcodec-0.5.0-py3-none-any.whl`, 27,070 bytes,
sha256 `cc0afe3b9b68490c5ad29a9fe54b830bf6266fd704e9ad962a674cdb4a2781f1`.
It is **not published to PyPI** (no API token); it is a release artifact.

## 3. Evidence discipline

- Every build/test ran through `scripts/docker-court.sh` in a minimal official
  image with `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp`, `no-new-privileges`, and `cap-drop=ALL` (FUSE adds only
  `/dev/fuse` and `CAP_SYS_ADMIN`).
- `scripts/verify-all.sh` re-ran all twenty-five courts and produced
  `bench/receipts/verify-20261105T035513Z.json` with revisions, host, command
  log, and per-court results.
- The P7 benchmark court ran codec, store, and filesystem benchmarks under the
  same hard limits. Every receipt contains raw samples, environment, and
  revisions. The codec receipt also records corpus hashes and per-tool command
  templates; the filesystem receipt embeds the full `fio` command log. The
  exact court commands are recorded in `bench/RESULTS-v1.md` §2. The results
  are single-host observations; `bench/RESULTS-v1.md` states the losses and
  gaps and makes no headline performance claim.
- Crash tests are process-fault proxies, not physical power-loss tests.
- The TLA+ **abstraction** is model-checked by bounded TLC runs. The Rust and
  Go implementations are **not** formally verified; there is no refinement
  proof. Implementation invariants remain test- and receipt-backed, and
  checkpoint/GC/capability/selector/multi-writer behaviour is outside the
  checked model.
- The F9 power-loss court is a QEMU virtual-machine power-cut proxy: `SIGKILL`
  loses the guest page cache and QEMU user-space state, not physical media or
  host/device caches. The block-layer court adds dm-flakey silent write drop,
  dm-flakey EIO, and a snapshot total-write-drop model; blkdebug write/flush
  errors are recorded as blocked at mount. It runs under the same hard limits as
  every other court.

## 4. What is **not** done (ranked)

1. **Remaining benchmark gaps** (P7): the old comparative pass closed the
   external SQLite, ext4/XFS cold-cache, concurrent-fio, matched Go/Rust codec,
   and whole-disk ZFS/Btrfs gaps (`bench/RESULTS-comparative-v1.md`). **Those
   reference-filesystem rows are provenance only and excluded from every claim
   under rule 13.** The 2026-11-04 pass adds `bench-fs-cold-20261104T071922Z.json` (whole-disk `zfs-whole` on raw
   `/dev/vdb` plus Btrfs), `blkdebug-20261104T072008Z.json` (dm-flakey lying-disk
   matrix; blkdebug mount blocker), and `hostcache-probe-20261104T072314Z.json`
   (global cache drop refused). Still open: physical media power loss, host/
   device cache loss, RAPL/joule energy, direct-I/O, metadata storms, AI-corpus
   external baselines, 10^5–10^6-object scaling, and a library-vs-library codec
   court. See `bench/RESULTS-v1.md` §8 and `bench/RESULTS-comparative-v1.md` §8.
2. **Formal model coverage beyond the commit/recovery abstraction.** The F9 TLA+
   model checks bounded commit/recovery safety, durability, and replay
   idempotence, but not checkpoints, GC, capability filtering, selector
   semantics, multi-writer concurrency, or a refinement proof to Rust/Go.
3. **Showcase re-run at a later revision.** The committed receipt is evidence
   for the exact revision it records; re-running after code changes requires a
   new receipt, not an inference from the old one.
4. **FUSE binding limits in Rust.** `fuser 0.15.1` has no `FUSE_TMPFILE`
   dispatch (7.37) and drops `lk_flags`, so Rust cannot express `O_TMPFILE` or
   distinguish flock from POSIX byte-range locks; both are explicit boundaries
   tested in `tests/fuse_verb_matrix.rs`, with kernel-local locking as the
   fallback. Go implements both. COLLAPSE/INSERT store code exists in both, but
   kernel 7.3 rejects those flags before forwarding. `ioctl` and cross-daemon
   lock coordination remain absent.
5. **Same-inode concurrent direct FUSE I/O** on kernel 7.3: blocked on the
   kernel/fuser EIO interaction; no workaround is claimed. A same-description
   write after a shared `mmap` also returned EIO once; the matrix test uses a
   fresh description. Shared mmap itself (map, write, `msync`, exact read) passes.
6. **Physical power-loss court** (real media/device write-cache loss), host
   page-cache loss, and real disk firmware that lies about flushes; the F9 QEMU
   VM proxy and its `cache=unsafe` lying-fsync device model are not physical.
   Remote/object-storage backends are implemented and court-tested (S3 on MinIO
   and Ceph RGW, GCS on `fake-gcs-server`, Azure Blob on Azurite, 3FS through
   its POSIX/USRBIO shims); no live cloud account and no deployed 3FS cluster
   was available.
7. **MW3, encryption at rest, and distributed consensus**; explicitly out of
   scope for v1 claims. MW1 is the default writer level and the bounded MW2
   transaction protocol is implemented and opt-in.
8. **Streaming codec API for files larger than memory**; chunked/range APIs are
   real, but the CLI materializes whole inputs within limits.
9. **atime/ctime persistence**, device/FIFO/socket nodes, and Rust `O_TMPFILE`;
   not implemented. `mknod` regular files and FIFO/device rejection are
   implemented and tested in both frontends; Go implements `tmpfile`.
10. **Spec-to-Go export automation in CI**; `scripts/export-spec.sh` and
    `scripts/gen-types.sh --check` exist, but no CI workflow pins them. The
    drift court runs only when `verify-all` is invoked.
11. **F3 time travel is correctness-only.** There is no io_uring, EKBLOOM, or
    other acceleration; none is implemented or claimed. The 100-version demo
    measures exactness and pinning, not latency.
12. **Long-horizon agent memory (D2) is simulated, not measured.** Typed edges,
    task/model/time mounts, a January snapshot, September writes, and a
    months-later bundle replay pass with exact bytes and lineage. The timestamps
    are fixture nanoseconds and the replay is a point-in-time export; no real
    months of wall-clock retention, no background aging, and no physical media
    aging were tested.
13. **The Buck2 adapter is a local adapter, not an integration.** It is a
    store-backed CAS plus canonical action keys. It is not a Buck2 plugin,
    worker, or REAPI gRPC backend. It does not execute actions, fetch remotely,
    or integrate with Buck2's scheduler. The generated Context Root pins a
    named snapshot; a Context Root whose commit is not a named snapshot is not
    itself a v1 GC root.
13. **Replay bundle scope.** The bundle carries one store and its Context
    Roots; it does not yet carry a filtered/partitioned subset, remote fetch,
    or a signature. `tar` and `zstd` are required tools and a missing tool is a
    hard error (no uncompressed fallback).
14. **Context mount root adds a constant `.weft` entry** for exact-id access;
    it is not part of the Context Root result set. Listing digests in the demo
    exclude that constant name explicitly.

## 5. Next work queue (dependency order)

1. **Physical media power loss, host page-cache loss, and RAPL/joule energy.**
   Prerequisites already recorded: QEMU VM power-cut proxy and lying-fsync
   device model (`bench/receipts/powerloss-20261104T014805Z.json`), block-layer
   lying-disk matrix (`bench/receipts/blkdebug-20261104T072008Z.json`),
   unprivileged host-cache probe (`bench/receipts/hostcache-probe-20261104T072314Z.json`),
   and perf counters without readable RAPL
   (`bench/receipts/perf-counters-20261104T015026Z.json`). The exact remaining
   limits are in `verification/README.md` §2.5.
2. **TLA+ implementation refinement.** Prerequisite: the bounded
   commit/recovery model and its deliberate counterexample
   (`bench/receipts/tla-20261103T224446Z.json`, `verification/tla/`). The model
   is abstract; there is no refinement proof to the Rust or Go state machines.
3. **Go parity for `weft.manifest.v2` — closed 2026-11-04.** Prerequisite: the
   Rust-only v2 representation result
   (`bench/receipts/representation-20261104T093437Z.json`) and the unchanged
   default raw fixture (`8959c65b…e1d81`). The Go store now parses and writes
   v2, and the cross-language tests build a Rust `--representation codec-auto`
   store and read it in Go; the historical sentence “the Go store rejects v2
   today” no longer holds.
4. **Go parity for persisted selector-index segments — closed 2026-11-06
   (R26).** Rust writes advisory `indexes/<ab>/<commit>.idx` segments (see §F2
   above); the portable format does not require them. Go now writes and loads
   WFX2 segments, byte-compatibly with Rust in both directions. Segments are
   not snapshot-incremental on either side.
5. **Go bounded open — closed 2026-11-04.** Go `Store.Open` now uses the
   Rust-reference generation-addressed bounded replay in `store/recovery.go`,
   with the authoritative full scan as the fallback; on a 100,000-generation
   store the read-only open is 2.80 ms median / 162 commit reads versus
   2155.65 ms / 100,001 reads on the forced full scan
   (`bench/receipts/go-fast-open-20261104T190732Z.json`, synthetic tmpfs,
   single host, states identical). The earlier measurement (18.65–19.07 s to
   open 287,759 commits versus 305 ms Rust) is the old full-scan path; the
   POSIX court above therefore still sizes rust→go runs at 240,000 requested
   and go→rust runs at 640,000 (fixed 15 s harness mount wait). Bounded commit
   index is not implemented and is not claimed. **1,000,000 completed
   operations is met** (1,202,560 across
   `bench/receipts/posix-torture-20261104T162109Z.json`,
   `…T172156Z`, `…T182537Z`); the earlier 1,000,002-request run
   (`…T142424Z`, 453,432 completed) and the 13,694-operation run
   (`posix-torture-20261103T213317Z.json`) remain history.
   `pjdfstest`/`fsstress` are not installed in the court and no substitute run
   is claimed.
6. **Remaining AI codec modes:** float-aware, KV-cache, activation/gradient,
   and semantic-delta. Mode 10 `TENSOR_SHARDED` covers sharded/per-block
   quantized storage (WEFT-AI-CODEC §11) and mode 11 `WEIGHT_DELTA` covers
   dtype/shape-checked exact weight deltas (§12).
   Prerequisite: modes 6–11 byte-identical across Rust and Go
   (`WeftCodec-Go/hack/cross.sh`) and the `specified, not implemented` list in
   `spec/WEFT-AI-CODEC.md` §6. No wire value exists for the remaining modes.
7. **Layout-independent tensor identity.** Implemented for the identity,
   layout, region, and shard layers in both codecs (WEFT-TENSOR v1); no
   FUSE/store binding yet, so this is not a topology-independent restore
   receipt.
8. **A real public model-family corpus.** Prerequisite: the synthetic `ai-v1`
   corpus and cost graph (`bench/receipts/ai-corpus-20261104T021627Z.json`,
   `bench/RESULTS-ai-v1.md`). `ai-v1` is synthetic; no public corpus or
   superiority claim is made.
9. **Native async API and object-store backend.** Prerequisite: none; not
   started, and no claim is made.
10. **Buck2 plugin/REAPI integration.** Prerequisite: the store-backed local
    CAS (`bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json`,
    41/41), which is not a plugin, worker, or REAPI gRPC backend.
11. **Real-time aging.** Prerequisite: the simulated-timestamp agent-memory demo
    (`bench/receipts/agent-memory-20261104T022512Z.json`, 7/7). Months of
    wall-clock retention were not measured.

    ## 2026-11-04 — A2: KV_CACHE (12) and ACTIVATION_STREAM (13)

    Two page-addressed AI modes are implemented in Rust and Go with byte-identical
    frames and bounded lazy page/chunk reads.

    - **Mode 12 `KV_CACHE`** (`weft.ai.kv_cache.v1`): one frame per page; exact
      descriptor (`model`, `layer`, `session`, `page_index`, `page_count`,
      `seq_start`, `seq_end`, dtype, head geometry, shape, `page_bytes`,
      `plane_codec`, `payload_len`). Payload is canonical row-major element bytes
      split into 2 (bf16/fp16) or 4 (fp32) Huffman-coded byte planes. Pages have no
      dependencies and no cross-page state; `KvPageSet` decodes exactly one page at
      a time and its counter is the independence evidence.
    - **Mode 13 `ACTIVATION_STREAM`** (`weft.ai.activation_stream.v1`): one frame
      per activation/gradient chunk with `digest_root = H("weft/tensor/values/v1",
      elements)`. `assemble_stream` rejects duplicate/missing indices, orders by
      `chunk_index`, verifies every digest, and returns
      `H("weft/ai/stream/v1", WCB({stream,step,rank,world,chunk_count,digest_roots}))`.
      `append_chunk` never decodes a previous chunk; the writer's decoded counter
      proves it.
    - **Vectors:** `conformance/v1/kv-cache/` and `conformance/v1/activation-stream/`
      are byte-identical in both codec repos: 3 valid + 8 rejects per mode
      (22 files). Reserved-mode vector moved to mode 14; the mode-12 vector is
      retained and marked `superseded_by`.
    - **Cross gate:** `WeftCodec-Go/hack/cross.sh compare` reports
      `YES byte-identical` for `kv_cache_p0`, `kv_cache_p1`, `activation_c0`, and
      `activation_c1`, and all existing modes still pass both directions.
    - **Bench:** `bench/receipts/kv-activation-20261104T220008Z.json`. Synthetic
      single-host rust court: 16 MiB KV (16 pages of 1 MiB bf16) encode median
      3.18 ms/page, whole-set decode 129.5 ms, single-page decode 8.88 ms; 64 MiB
      activation (64 chunks of 1 MiB bf16) encode median 2.99 ms/chunk, whole-set
      decode 580.9 ms, assembly 612.6 ms, single-chunk decode 9.01 ms.
    - **Tests:** Rust `cargo test --all-targets` and Go `go test ./...` in the
      Docker courts; new Rust integration tests cover bit-exact NaN/denormal/
      signed-zero round-trips, page-7-of-8 independence, chunk independence,
      append counters, shuffled/duplicate/missing assembly, and range reads.
    - **Residual:** Huffman planes are not seekable, so a range read decodes one
      page/chunk and slices it; mode allocation is now 13, reserving 14+.

    ## 2026-11-04 — A3: semantic roots and SEMANTIC_DELTA (14)

    Near-duplicate tensors are now findable by an explicit semantic root and
    exactly delta-encodable by mode 14. No lossy decode path exists.

    - **Semantic root:** `H("weft/tensor/semantic/v1",
      WCB(weft.tensor.semantic.v1))`. Policies are `none`,
      `mantissa_truncate:N` (bf16/fp16/fp32; 1 <= N <= mantissa bits; sign,
      exponent, NaN/inf preserved), and `block_quantize:B:L` (B divides the
      element count; signed two's-complement words; symmetric `[-amax, amax]`
      grid with the exact integer formulas in §15.1). `none` uses the exact
      `weft/tensor/values/v1` hash; the other schemes use
      `weft/tensor/semantic-values/v1`. `semantic_root` and `semantic_matches`
      exist in both languages and are byte-identical.
    - **Mode 14 `SEMANTIC_DELTA`** (`weft.ai.semantic_delta.v1`): the mode-11
      payload verbatim (modular words, byte planes, one Huffman stream per
      plane) plus a descriptor binding `base_semantic_root` and `policy`.
      Decode verifies base content hash, dtype, shape, the semantic root
      recomputed from the actual base bytes under the declared policy, the delta
      root, and the reconstructed content hash. Every mismatch fails closed.
    - **Candidate selection:** `select_semantic_base` requires both tensors
      present, rejects different dtype/shape/root, and picks the smallest exact
      per-plane Huffman size of the modular difference, tie-breaking on the
      candidate content hash. Deterministic; different roots are never offered.
    - **No-lossy statement:** approximate decoding is not implemented. The
      semantic policy only groups and verifies a base; reconstruction is
      bit-exact and an approximate base is `DependencyMismatch`, never a
      fallback.
    - **Vectors:** `conformance/v1/semantic-root/` (3 valid + 6 rejects) and
      `conformance/v1/semantic-delta/` (3 valid + 6 rejects), byte-identical in
      both codec repos. The previous reserved-mode vector moved from 14 to 15;
      `017_unknown_mode_reserved14` is retained with `superseded_by`.
    - **Cross gate:** `WeftCodec-Go/hack/cross.sh compare` reports
      `YES byte-identical semantic_delta_bf16`, `..._fp32`, and `..._q8`, and
      decode-equivalent both directions.
    - **Tests:** Rust `cargo test --all-targets` 182 passed / 0 failed
      (`tests/semantic_delta.rs` 6/6); Go `go test ./...` all packages pass
      (`conformance` semantic vectors and `modes` near-duplicate selection).
    - **Bench:** `bench/receipts/semantic-delta-20261104T222029Z.json`. Synthetic
      single-host rust court, 8 MiB bf16 pair, 4,195/4,194,304 changed (0.10002%):
      semantic root median `none` 0.68 ms, `mantissa_truncate:3` 8.95 ms,
      `block_quantize:128:256` 15.91 ms; selection over 32 same-root candidates
      983.2 ms, selected index 0; delta frame 1,050,660 B vs independent zstd-3
      6,127,332 B (5.83x) and zstd-19 4,976,379 B; encode median 31.93 ms,
      decode median 44.22 ms; `exact` and `logical_root_match` true.
    - **Residual:** the semantic root is an index hint, not a remote search
      service; candidate selection decodes every candidate; only three numeric
      policies exist in v1.

    ## 2026-11-04 — B1: tensor representations and the per-object planner

    WeftTensor frames are a first-class physical representation in the Rust
    store, selected by a deterministic cost-aware planner.

    - **Manifest extension:** additive v2 `kind:"tensor"` with `frame`,
      `tensor_mode` (10/11/12/13/14), `dtype`, `shape`, `logical_root`,
      canonical `layout`, `dependencies` (base content roots for 11/14),
      `decoded_size`, and `strategy`. Raw/codec v2 bytes are unchanged.
      `spec/WEFT-MODEL.md` §2.3.1 is normative.
    - **Store API:** `write_tensor_file`, `write_tensor_file_with_spec`,
      `plan_tensor`, `read_tensor`, `read_tensor_region`,
      `tensor_logical_root`, `tensor_metadata`, `write_tensor_range`. The store
      writes one object per tensor with one frame; multi-shard declarations are
      rejected rather than truncated. Row-major/column-major/blocked/single-
      shard inputs canonicalise to the same logical bytes and root.
    - **Planner:** `physical_bytes + lambda_ns * estimated_decode_ns`, default
      `lambda_ns = 0`; deterministic candidate order and tie-break; estimates
      are a documented integer model, not measurements. Candidates are raw,
      zstd, mode 10 (q8_0), mode 11 (causal parent), mode 14 (bounded
      same-semantic-root candidate set). Delta chains respect the frozen
      dependency depth bound. Planner prices are surfaced by `plan --json`
      only; the manifest carries no non-canonical fields.
    - **Reads/GC/verify:** full reads verify frame/mode/strategy, dtype/shape,
      dependency closure, recomputed `LogicalTensorRoot`, and the frozen
      content hash. GC recursively retains delta bases; `verify --full` names
      the object/manifest/frame/dependency on failure. Missing dependencies
      fail closed with `Corrupt`.
    - **Identity for raw/zstd tensors:** reserved attributes
      `weft.tensor.dtype|shape|logical_root|layout|strategy`.
    - **CLI:** `write --tensor --dtype --shape --layout`, `cat --region`,
      `plan --json`.
    - **Tests:** `tests/tensor_store.rs` 9/9 (mode 10/11 round trips and layout
      roots, raw selection for incompressible data, weight-delta under 50% of
      independent zstd, latency-weight switch, semantic-root gating,
      missing-dependency/GC/verify, canonical layout encoding, byte-rewrite
      drops stale tensor identity). Full Rust
      `cargo test --all-targets`: 112 passed / 0 failed. FUSE `--features fuse
      --all-targets`: 140 passed / 2 ignored.
    - **Bench:** `bench/receipts/tensor-planner-20261104T224929Z.json`,
      synthetic 10 checkpoints x 8 tensors x 1 MiB bf16, 1% change per step,
      bench court (`weft/bench-court:1`, 3 GiB, 4 vCPU, kernel
      7.3.1-1-cachyos), labelled single-host/no-fleet. Physical data bytes /
      ingest / restore (1 worker): zstd 15,917,707 / 8.22 s / 93 ms; bytes-only
      planner 11,187,086 / 24.37 s / 1.79 s; latency-weighted planner
      83,985,344 / 7.77 s / 37 ms. Restore with 2/4 workers: zstd 48/26 ms,
      bytes 906/467 ms, latency 22/15 ms; peak RSS 51,680/52,164/52,148 KiB;
      all three verify clean. The bytes planner saves 29.7% physical bytes at
      ~3x ingest and ~19x restore cost (delta chains are decoded per read); the
      latency planner is fastest to restore at 5.3x the physical bytes.
    - **Residual:** Go store parity for `kind:"tensor"`; multi-shard objects;
      q4_0 store representation; modes 12/13 are read-only in the store;
      region reads decode the whole delta/Huffman frame; the planner's decode
      estimate is not calibrated to a device.

    ## 2026-11-04 — D1: provenance receipts and provenance-guided recovery

    Provenance is now a first-class store object, and recovery is planned from
    verified bytes, representation chains, and recorded computation.

    - **Record:** `weft.provenance.v1` canonical WCB with `operation`, bounded
      `params`, `inputs[{role,content,object}]`, sorted unique `outputs`,
      `environment{tool,version,env_hash,...}`, `determinism`
      (`exact|declared|unknown`), `principal{uid,gid}`, and `timestamp_ns`.
      Inputs are strictly ordered by `(role, content)`; duplicate inputs or
      outputs are `NonCanonical`. Bounds: params 64 KiB, environment 16 KiB,
      1,024 inputs, 1,024 outputs, 1,024-byte text fields, 1 MiB record.
      `ProvenanceId = H("weft/provenance/v1", WCB(record))`.
    - **Storage/attachment:** the record is a content-addressed blob in the
      manifest namespace under the provenance domain (never parsed as a
      manifest). `record_provenance(outputs, record)` rejects any declared
      output root that does not equal a supplied object's actual content root,
      publishes the blob, then attaches `predicate="provenance"` value-target
      relations (lowercase hex ProvenanceId) to every output in one commit.
      API: `record_provenance`, `provenance_of`, `provenance_records_of`,
      `provenance_ids_of`, `provenance_closure` (bounded, cycle-rejecting).
    - **Retention:** GC marks a receipt, its declared inputs, and its outputs
      while any output object that names it is retained; external inputs are
      skipped. Object deletion now emits explicit `del_relation` ops for
      relations whose subject is the removed object, so the last output
      deletion releases the receipt and otherwise-unreferenced inputs. Replay
      and cross-language state see the same explicit ops; `apply_op` was not
      changed.
    - **Verify:** `verify --full` validates the relation value target, subject
      existence, blob hash/canonical re-encoding, and that the receipt still
      declares the subject's current content root. Report field
      `checked_provenance`. A rewrite without a new receipt is reported as
      stale provenance.
    - **Planner:** pure and deterministic. Eligibility is `available` for
      fetch/decode; recompute additionally requires `determinism == "exact"`
      and every input verified. Cost is the saturating sum of the declared
      fetch/decode/recompute components; the winner minimises
      `(cost, fetch < decode < recompute, name)`. With no eligible candidate it
      returns `NoEligibleStrategy` naming every candidate and reason; there is
      no fallback. `Store::recovery_candidates` offers fetch for raw chunked
      manifests, decode for frame-backed codec/tensor manifests, and one
      recompute candidate per attached receipt with verified-input checks.
    - **CLI/spec:** `provenance record|show|closure`, `recover --plan` (prints
      strategy and three cost components), and `spec/WEFT-PROVENANCE.md`.
    - **Tests:** `tests/provenance.rs` 19/19 (round trip, order-independent
      identity, unsorted/duplicate rejection, output mismatch without writing,
      bounded params, atomic multi-output attachment, cost choice/component
      sums, exact-determinism and missing-input exclusion, tie-break, typed
      no-eligible error, closure/depth/cycle, GC retention and reclamation,
      tampered record, stale output, malformed target, reopen). Rust
      `cargo test --all-targets`: 135 passed / 0 failed / 0 ignored. FUSE
      `cargo test --features fuse --all-targets`: 163 passed / 0 failed /
      2 ignored (existing CAP_SETUID and kernel direct-I/O guards).
    - **Bench:** `bench/receipts/provenance-20261104T235013Z.json`, bench court
      `weft/bench-court:1` (3 GiB, 4 vCPU, 1024 pids, kernel
      7.3.1-1-cachyos), synthetic 10 operations x 10 artefacts x 1 MiB
      (104,857,600 logical bytes; 80 fetch-capable, 20 decode-capable), one
      full operation (10 artefacts) unlinked and collected (141 blobs /
      8,579,129 bytes), receipt retained by the harness. Injected plan
      (fetch 10 ms/MiB, decode 30 ms, recompute 100 ms): 72 fetch / 18 decode /
      10 recompute, 68.648 ms wall, 159,629,377 physical bytes, 2,260 ms
      declared. Measured plan (warm-cache medians: fetch 318,013 ns across 72
      artefacts, decode 374,498 ns across 18): same mix, 68.823 ms wall, 1,029
      ms declared. Forced recompute: 100 recompute, 311.583 ms wall,
      756,138,570 physical bytes, 10,000 ms declared. `verify --full` clean
      with 90 checked provenance relations. Label: synthetic, warm-cache,
      single host, no fleet claim.
    - **Residual:** no Go `weft.provenance.v1` parity; the planner's store-side
      fetch/decode availability is determined by representation (raw vs
      frame-backed), so the bench's measured costs do not change the mix;
      closure follows an explicit input object or a unique content-root match,
      otherwise the input is an external leaf; a rewrite after recording makes
      the receipt stale and `verify --full` reports it.

## Subphase F — real public checkpoint family (storage and restore)

**Implemented, measured once, single host.** The headline experiment ingests a
real public base/fine-tune pair into WeftFS as tensors, chooses representations
with the shipped bytes-only planner, and compares complete storage and restore
against strong conventional baselines at matched exactness. Receipt:
`bench/receipts/real-model-20261105T012000Z.json`; analysis, every loss, and the
reproduce commands: `bench/RESULTS-real-model.md`.

- **Corpus:** `Qwen/Qwen2.5-0.5B` (`060db649…`) and
  `Qwen/Qwen2.5-0.5B-Instruct` (`7ae55760…`), one 988,097,824-byte
  `model.safetensors` each, both BF16, 290 identical tensor names/shapes,
  SHA-256 verified against the Hugging Face LFS pointers. The operator's first
  choice (`HuggingFaceTB/SmolLM-135M` + Instruct) publishes the base as F32 and
  the instruct model as BF16 despite both configs saying bfloat16, so no exact
  causal delta is possible; it was ingested as a cross-dtype secondary instead.
  No model weights are committed; they live under `.docker-cache/real-corpus/`.
- **Harness:** `examples/real_model_bench.rs` parses safetensors headers
  directly and calls `write_tensor_file_with_spec` per tensor with no strategy
  hint, offering the corresponding base tensor version as the causal parent for
  the instruct checkpoint. The header is stored as a raw object so the complete
  file can be reassembled. Ingest and restore are separate processes for honest
  peak RSS.
- **Exactness:** both reassembled files match the LFS SHA-256
  (`88c14255…0ed342`, `fdf756fa…88fb7fe`); every tensor hash and every logical
  root matches; `verify --full` is clean. All four judged methods are lossless.
- **Storage, both checkpoints (bytes):** ZipNN 0.5.4 **1,316,518,229** (winner);
  WeftFS store **1,389,785,926** (+5.57 %); WeftFS data-only 1,313,652,215
  (−0.22 %, i.e. a tie once 75,549,540 bytes of rebuildable index segments and
  575,964 bytes of commits are excluded); zstd `--patch-from` `-12` both files
  1,503,140,795 (+14.18 %); zstd `-19` independent 1,528,909,007 (+16.13 %).
- **Per checkpoint:** WeftFS base 788,405,375 (loses 19.8 % to ZipNN and 3.1 %
  to zstd `-19`); WeftFS instruct marginal 601,380,551 (**wins 8.64 % vs
  ZipNN** and 21.3 % vs zstd `-19`). The planner chose 241 weight deltas, 1
  zstd frame, and 48 raw tensors for the instruct checkpoint; the base chose
  242 zstd + 48 raw. No semantic delta was applicable.
- **Restore:** WeftFS base read 968.8 ms / materialize 6,464 ms; instruct read
  14,367 ms / materialize 23,752 ms. zstd `-19` read (to null) 1,413 / 1,488 ms
  and materialize 976 / 727 ms; ZipNN 240 / 196 ms in-memory and 508 / 455 ms
  to file. **WeftFS loses full instruct restore by 9.7× to zstd and 73× to
  ZipNN**, the cost of Huffman-decoding weight deltas plus the base dependency.
  A single tensor (`model.layers.10.mlp.down_proj.weight`) restores in 7.3 ms
  reading 6,842,059 bytes (base) and 118.6 ms reading 11,643,580 bytes
  (instruct, delta + base), versus a whole-file decode of 658–765 MB for the
  file-at-a-time formats.
- **Ingest:** 136.0 s total (30.7 s base + 105.3 s instruct), peak RSS
  2,540,908 KiB; the instruct phase reads 19.07 GB from the store backend while
  pricing raw/zstd/weight-delta candidates. zstd `-19` took 538.9 s; ZipNN
  0.50 s.
- **Unavailable baselines:** `zstd --patch-from -19` was killed by the 3 GiB
  cgroup limit after 510.3 s (`returncode 137`, `maxrss 3,131,264 KiB`,
  622,206,976 partial bytes); the reported patch baseline is `-12` for both
  files. DFloat11 0.5.0 installs but cannot import: it builds a CUDA `RawModule`
  at module load and the court exposes no `/dev/nvidia*`; cupy 14.2.0 reports
  `cudaErrorInsufficientDriver`. No DFloat11 bytes or time are claimed.
- **Secondary:** SmolLM-135M (F32) + Instruct (BF16) ingested with no causal
  parent offered, all tensors stored independently (512.7 MB data), both files
  restored byte-exactly; confirms the dtype mismatch fails closed.
- **Residual:** single host, warm caches, no cache dropping; ZipNN was run
  whole-file and independent, not per-tensor or delta-aware; WeftFS restore
  timings are single observations for the full-file rows; no Go parity for this
  court.

## Subphase R1 — measured decode cost, mode-9 planner candidate, rebasing

**Implemented, measured once, single host; Go parity in the store API.**
Receipt `bench/receipts/planner-float-20261106T025351Z.json`; analysis and
old-vs-new tables in `bench/RESULTS-real-model.md` (R1 section).

- **Candidate set:** mode 9 `FLOAT_TENSOR` is now priced by the planner for
  BF16/FP16/FP32 tensors written through `write_tensor_file*`, alongside raw,
  zstd, mode 10, mode 15 (opt-in), weight-delta, and semantic-delta. Mode 9 is
  only offered when it saves at least 1/64 (1.56 %) of the zstd frame, because
  it decodes ~19x slower; this prevents a noise-sized win on incompressible
  data from selecting the slow decoder. FP32 remains offered but loses on the
  measured corpora (P2: +62 % physical), so the planner does not select it.
- **Cost model:** one affine table per language (`src/cost_model.rs`,
  `WeftFS-Go/store/cost_model.go`) with `fixed_ns + decoded_bytes * ns_per_mib
  / 1048576`, integer-only. Default `measured` (median of three 7-repetition
  court runs at 128 KiB and 16 MiB: raw 10,000 + 318,321/MiB; zstd 17,000 +
  814,267; mode 9 0 + 15,403,165; mode 10 32,000 + 13,788,933; mode 15 14,000
  + 10,056,794; weight-delta 31,000 + 5,297,277 incremental; semantic-delta
  18,000 + 7,228,350 incremental). `--planner-cost-model=declared` reproduces
  the old hand-written linear model. Delta rows are incremental and the rebase
  planner sums the dependency chain. Modes 12/13 are never produced and keep
  declared rows. Calibration is host/court-bound, not a portable constant; this
  run reproduced mode 9 to 0.003 %, zstd 5 %, raw 0.7 % in this run (a previous run varied 20 %, host noise).
- **Access counters:** advisory, per-object, bounded (1,000,000 counters;
  8,192 journal records), persisted as `access/counts.v1` (epoch + digest) and
  `access/journal.v1`, rebuildable from the journal, and never consulted by
  commits, recovery, verification, GC, or any frozen format. Read-only opens
  never write; the read path flushes every 256 observations and on close.
  Go writes the same files and reads the same epoch/digest rules.
- **Rebase:** `weft rebase --root <path|id> --threshold <ns> [--dry-run]
  [--strategy ...]`. A candidate needs `reads > 0`,
  `reads x current_decode_cost > threshold`, and a standalone representation
  that decodes strictly faster and is no larger than the logical bytes. Apply
  publishes a **new version** (causal parent = previous version), records a
  canonical `weft.provenance.v1` receipt (`operation: "rebase"`, input = old
  content root, output = new content root, params include reads/threshold/
  strategies), consumes the counter, and leaves every previous version and
  snapshot intact. A second run with no intervening reads is a no-op. Go
  implements the same decision and writes a receipt the Rust verifier reads
  (`store/rebase.go`; cross-language test).
- **Qwen2.5-0.5B family with the new candidate set:** family store total
  **1,276,128,956 B** (F10 1,389,785,926), data-only **1,199,958,505 B**
  (1.20 GB, matching the reviewer's arithmetic), indexes 75,584,928 B (5.92 %).
  **WeftFS now beats the recorded ZipNN total 1,316,518,229 B by 3.07 %**
  (F10 lost by 5.27 %); per checkpoint base 674,729,302 B and instruct marginal
  601,399,654 B. Base strategy mix: 169 mode 9 + 73 zstd + 48 raw; instruct:
  241 weight-delta + 1 zstd + 48 raw. Exactness unchanged (both files match the
  LFS SHA-256, every logical root matches, `verify --full` clean).
- **Restore cost (the honest loss):** base read 13,776 ms / materialize
  16,737 ms (F10 968.8 / 6,464); instruct read 26,936 ms / materialize
  29,951 ms (F10 14,367 / 23,752). Single tensor base 111.4 ms, instruct
  221.9 ms (F10 7.3 / 118.6). Mode 9 is 14.2x slower than the F10 base path and
  ~57x slower than ZipNN's in-memory decompressor; the storage win is bought
  with decode time. Ingest also costs more (147.6 s base + 248.4 s instruct;
  peak RSS 2,720,500 KiB of 3 GiB) because both candidates are encoded.
- **Rebase dry run (hot instruct):** after 10 reads per tensor (9.88 GB
  logical), threshold 1e9 ns selected 73 of 241 weight-delta objects, all
  `float-tensor`, no writes; e.g. the 272 MB embedding current decode
  5.375 s/read -> 4.000 s/read, 180,547,519 B independent.
- **Tests:** Rust `cargo test --all-targets` 244 passed / 0 failed / 0 ignored;
  FUSE `cargo test --features fuse --all-targets` 278 passed / 0 failed /
  2 ignored (one earlier FUSE invocation reported a transient failure; two
  subsequent clean runs passed all 278); Go `go test -race -count=1 ./...` all
  packages pass, including the cross-language rebase/provenance test. New tests:
  `tests/planner_float.rs` 4/4 and `WeftFS-Go/store/planner_float_test.go` 3/3.
- **Residual:** one host, warm cache; the measured table is court-bound; mode 9
  restore is slower than zstd/ZipNN; the rebase dry run only plans (no apply in
  the real-model court); the Go store has no CLI `rebase` verb yet (API + tests
  only); index segments are 5.92 % of the total.

## Subphase R2 — decoder throughput (no wire change)

**Implemented, measured once, single host, one subphase.** Receipt
`bench/receipts/planner-float-20261106T035316Z.json`; analysis and both
old-vs-new tables in `bench/RESULTS-real-model.md` (R2 section). Revisions:
WeftFS `ba5a0e6`, WeftCodec `1ee5d90`, WeftCodec-Go `d1aab12`, WeftFS-Go
`bb15514`. This is the governing objective from the R1 review: reconstruction
cost, not more format coverage.

- **Huffman decode:** `huffman-canonical-v1` now uses a 12-bit primary table
  over an MSB-aligned 64-bit accumulator; the original canonical bit-at-a-time
  loop is kept for codes longer than 12 bits and for stream tails. Wire format,
  encoder bytes, validation order, error classes, padding and consumed-byte
  accounting are unchanged. Isolated microbench: 4.7-5.0x (Rust), 4.1-5.3x
  (Go).
- **FLOAT_TENSOR plane join:** `join_float_tensor_payload` /
  `JoinFloatTensorPayload` extract `sign+mantissa` once per element and write in
  place, replacing a loop that read two bit fields one bit at a time. Microbench:
  3.1-5.7x (Rust), 4.8-6.5x (Go).
- **Proof the bytes did not move:** a test-only verbatim reference decoder in
  each language plus 10,000-case randomized differentials comparing output
  bytes **and** error class, in Rust and Go; the cross-language `compare` gate
  and both conformance replays unchanged at 206 pass / 0 fail / 101 skip.
- **Planner table re-measured and re-baked:** median of three independent
  7-repetition calibrations. mode 9 15,403,165 -> **4,683,726 ns/MiB (3.29x)**;
  mode 15 10,056,794 -> **3,366,772 (2.99x)**; mode 10 13,788,933 ->
  **9,123,307 (1.51x)**; weight-delta 5,297,277 -> 5,293,987 (1.00x);
  semantic-delta 7,228,350 -> 6,458,523 (1.12x). `src/cost_model.rs` and
  `WeftFS-Go/store/cost_model.go` carry identical integers and
  `MEASURED_CALIBRATION` moves to `weftfs-r2/decode-calibration/v1`.
- **Restore:** base read 13,775.8 -> **4,627.5 ms (2.98x)**, base materialize
  16,736.9 -> 6,494.3 ms; instruct read 26,935.7 -> **11,371.2 ms (2.37x)**,
  instruct materialize 29,950.5 -> 12,092.2 ms; single tensor base 111.4 ->
  **39.8 ms**, instruct 221.9 -> **92.7 ms**. Storage unchanged at
  1,276,128,956 B (3.07 % under ZipNN) because the planner is bytes-only at
  `lambda = 0`. Ingest also fell (base 147.2 -> 65.2 s, family 241.4 -> 111.6 s)
  because planning and verification decode candidates.
- **Rebase:** with the cheaper measured table, the 1e9 ns threshold on the hot
  instruct checkpoint selects **1 of 241** objects (R1: 73 of 241); the check
  remains non-zero and the dry run still writes nothing.
- **Still a loss:** base read is 4.8x the F10 zstd-strategy read (968.8 ms) and
  ~19x ZipNN's 240 ms; mode 9 is 6.0x zstd per MiB (4.68 vs 0.78). At this
  measurement decode is sequential across tensors and frames are not yet
  block-independent streams with an offset table; the parallel additions below
  are later in this section. `tensor-sharded` is now the slowest class
  (9.12 ms/MiB).
- **Tests at that tree:** WeftCodec `cargo test --all-targets` 204 passed / 0
  failed; WeftCodec-Go `go test ./...` + `go vet` all packages; parent 244
  passed; FUSE `--all-targets` 278 passed / 0 failed / 2 ignored; WeftFS-Go
  `go test -race -count=1 ./...` all packages. One transient
  `extent_cow::chunk_size_comes_from_the_manifest` failure appeared in the
  first fully loaded parallel FUSE run; 10/10 standalone repeats and the rerun
  passed. It is recorded, not deleted.
- **Parallel reconstruction (R2 addition):** `Store::read_tensors_bounded`
  (`src/parallel.rs`) decodes independent tensors on a bounded
  `std::thread::scope` pool and returns exact input order for every thread
  count; the harness gained `--threads` and `--max-inflight-mib`. Read at
  4 threads is **2,206.4 ms** base and 5,275.7 ms instruct (**2.09x** / 2.08x
  against one thread, 95-99 % of the Amdahl cap set by the 272 MB embedding,
  which alone exceeds the staging bound and runs alone). Backend bytes read and
  every restored byte are identical for all thread counts. Materialize does
  **not** improve reliably (4-thread base median 10,987.9 ms, 0.43x, in the
  committed pass; host writeback dominates a sequential 988 MB write). Peak RSS
  1.94-2.18 GiB (2,036,324-2,290,452 KiB) of the 3 GiB court. Receipt
  `bench/receipts/parallel-restore-20261106T043524Z.json`, two further passes
  in the same court, analysis in `bench/RESULTS-real-model.md`. Tests:
  `tests/parallel_read.rs` 9/9, parent 253 passed / 0 failed, FUSE 287 passed /
  0 failed / 2 ignored (pre-existing). Single-host, 4-vCPU, warm-cache; no
  fleet claim.
- **Residual:** single host, warm cache; the table is court-bound; parallel
  reconstruction is per-object only (no block-independent streams inside one
  frame) and does not make materialize reliable; no Go writer for the
  checkpoint/mode-15 paths; no fleet claim.
- **Planner tiled candidate (additive):** `TensorPlanOptions.tile_shape` and
  `weft plan --tile-shape A,B` price mode 15 without changing defaults. On the
  real BF16 base embedding (272,269,312 B), tiled `[128,224]` is **184,633,502 B
  vs 180,623,816 B for mode 9 (+2.22 %)** and estimates **874,217,523 ns vs
  1,216,176,993 ns (-28 %)**; mode 9 still wins at the bytes-only default. Test
  `planner_offers_mode15_only_when_a_tile_shape_is_supplied`. Intra-tensor
  parallel tile decode is covered by the next bullet.
- **Bounded parallel tile decode + auto-tiling (additive, wire unchanged):**
  `TileDecodeOptions { threads, max_inflight_bytes }` (default `threads = 1`)
  and `Store::read_tensor_with_options` /
  `read_tensor_region_with_options` decode mode-15 tiles on the same bounded
  runner as `read_tensors_bounded`: deterministic order, lowest-index error,
  panics caught, exact staging bound; every touched tile root is verified, the
  full-object path also verifies the logical root and content hash, and the
  region path is documented as not re-reading the whole-frame trailer.
  `TensorPlanOptions.auto_tile_bytes` derives a dividing rank-2 tile shape
  (largest element count in budget, then balance, then lexicographic), offers
  mode 15 to the **unchanged** price, and records the shape in the candidate
  detail and in `weft.tensor.tile_shape`. `weft cat --threads N`,
  `weft plan --auto-tile-bytes N`, `weft write --auto-tile-bytes N` expose it.
  On Qwen2.5-0.5B base the bytes-only price selected mode 15 for **0/290**
  tensors (float-tensor is 2.17 % smaller), so auto-tiling at 65,536 B changed
  no representation. A labelled forced-tiled control (169 rank-2 tensors, shapes
  `[128,224]`/`[128,256]`/`[256,128]`) costs **+14,310,892 B (+2.174 %)** of
  tensor data and measures full-restore read **3,038.3 -> 1,062.1 ms (2.86x)**
  at 4 workers, materialize 4,215.6 -> 2,871.4 ms (1.47x, still writeback
  bound), embedding whole-region **738.7 -> 203.0 ms (3.64x)**, and 1-row region
  3.5 ms with 4 tiles and **730,133 B fetched vs 180,625,100 B** (247x).
  Logical-root recheck stays sequential (0.99x); 100-row/1-row regions touch 4
  tiles and do not parallelise. Receipt
  `bench/receipts/tiled-parallel-20261106T062146Z.json`, analysis
  `bench/RESULTS-tiled-parallel.md`, `tests/tiled_parallel.rs` 8/8, parent 265
  passed, FUSE 299 passed / 2 ignored / 0 failed (one transient
  `representation` failure in a loaded first run passed standalone and on
  rerun). Single host, 4 vCPU, warm cache; no fleet claim; no Go parity; no
  partial boundary tiles.

## Subphase P2 — real-weight FLOAT_TENSOR and WEIGHT_DELTA court

**Implemented, measured once, single host.** This court settles two empirical
questions on real public weights, reported separately: (A) whether
`FLOAT_TENSOR` (mode 9) is competitive with zstd and ZipNN-class coding, and
(B) whether `WEIGHT_DELTA` (mode 11) helps more on consecutive training
checkpoints than on an independent fine-tune. Receipt:
`bench/receipts/real-float-tensor-20261105T053958Z.json`; tables, method, every
loss, and reproduce commands: `bench/RESULTS-float-tensor-real.md`. Label:
single host, public weights, warm cache, no fleet claim; every method lossless.

- **Corpus:** seven files, all SHA-256-verified against the Hugging Face LFS
  pointers for the exact revision. `Qwen/Qwen2.5-0.5B` and `-Instruct` are
  BF16 (290 tensors, 988,097,824 bytes each). `EleutherAI/pythia-70m` branches
  `step0`, `step1`, `step2`, `step4`, `step8` are **F32** (76 tensors,
  281,715,176 bytes each) and are not reported as bf16.
  The F32 containers hold fp16-precision values widened to F32 (the receipt's
  byte-plane probe finds entropy 0.0 in byte plane 0 and 8 symbols in plane 1
  of the largest tensor);
  no claim is made for full-precision random F32. The published `step0` and
  `step1` files differ in **exactly 19 bytes**, so that pair is reported
  separately and is not the headline consecutive result.
- **Harness:** `WeftCodec/examples/real_float_tensor_bench.rs` parses
  safetensors headers directly and calls the shipped `encode_float_tensor` /
  `decode_float_tensor` and `encode_weight_delta` / `decode_with_base_frame`
  entry points. zstd baselines are the real 1.5.4 CLI, per tensor at `-19 -T1`
  and default `-3 -T1`; whole-file `-19` is reported separately. ZipNN 0.5.4
  is the official whole-file byte path; it is not delta-aware.
- **(A) BF16:** `FLOAT_TENSOR` is **14.4 % smaller than per-tensor zstd -19**
  on the base (657,975,743 vs 768,347,895) and 14.3 % smaller on instruct
  (657,891,990 vs 767,597,259); it is 14.0 % smaller than whole-file zstd -19.
  ZipNN 0.5.4 gives 658,281,885 / 658,236,344, so mode 9 is ahead by 306,142 /
  344,354 bytes (0.05 %) — a tie, not a claimed win. Mode-9/raw is 0.6659.
  All 121 tensors of at least 1 MiB win; 169/290 win overall; the worst loss is
  +663 bytes on a 1,792-byte layernorm.
- **(A) F32:** `FLOAT_TENSOR` is **62.0 % larger than per-tensor zstd -19**
  (233,958,945 vs 144,397,747 on step0) and **95.5 % larger than ZipNN 0.5.4**
  (119,680,613); 0/76 tensors win. Mode-9/raw is 0.8305. The structural reason:
  mode 9 stores the 24-bit sign+mantissa plane raw for F32 and the pythia
  mantissas have three low-entropy bytes that zstd removes.
- **(B) Consecutive pairs** (delta bytes vs the newer checkpoint's per-tensor
  zstd -19; patch-from is the whole-file baseline): step0→step1 7,434,959 /
  144,403,851 (0.0515; patch 30,265, so delta is 245.66× worse on this
  19-byte-change artifact); step1→step2 28,030,116 / 144,272,053 (0.1943;
  patch 67,686,695); step2→step4 33,412,606 / 144,303,717 (0.2315; patch
  111,529,080); step4→step8 49,198,709 / 144,464,688 (0.3406; patch
  141,228,981). Delta is 19.4-34.1 % of independent per-tensor zstd -19 on the
  meaningful consecutive pairs and 2.4–3.3× smaller than the best
  `zstd --patch-from` there.
- **(B) Independent fine-tune:** base→instruct changes 97.93 % of words; mode
  11 is 541,188,577 bytes, which is **70.5 % of zstd -19** (767,597,259) and
  72.2 % of the best patch (749,534,915 at `-12`; `-19` was killed by the
  3 GiB limit at 3,128,344 KiB). So delta helps, but 2–3.6× less than on
  meaningful consecutive checkpoints. The two case sets are never aggregated.
- **Restore:** single-threaded, 5 reps per tensor, sum of per-tensor medians,
  excluding storage I/O. Mode 9 is 6.7× (pythia F32) to 12.0× (Qwen BF16)
  slower than zstd -19 decode; delta is 16.1–20.6× slower on the four pythia
  pairs and 44.0× slower on the Qwen fine-tune. Delta restore also reads the
  base frame (the Qwen pair reads 1,199,164,320 bytes vs zstd's 767,597,259).
- **Losses kept explicit:** mode 9 loses on F32 by 62.0 %/95.5 %; only ties
  ZipNN on BF16; decodes 6.7–12× slower. Mode 11 loses 245.66× to patch-from
  on the 19-byte step0→step1 pair because any non-constant Huffman plane costs
  at least one bit per element; it reaches only 70.5 % of zstd -19 on the
  fine-tune; and its restore is 16–44× slower.
- **Residual:** one host, warm caches, no cache dropping; restore totals are
  sums of medians rather than one pass; ZipNN is whole-file and not
  delta-aware; mode 9/11 are measured as codec frames, not through the store
  planner; no Go parity for this court.

## Subphase P5a — write budgets and branching time travel

Implemented in both stores (`spec/WEFT-QUOTA.md`, `docs/quota-branching.md`).

- **Commit-time budgets.** Admission is checked inside the writer lock before
  any commit bytes exist. A commit that would exceed a configured
  `{posix(uid,gid) | tenant(name), max_new_bytes, max_objects}` budget fails
  with `BudgetExceeded: write budget exceeded`; generation, head, and state are
  unchanged. Charges are the canonical object-record bytes plus newly reachable
  manifest/frame/chunk blob bytes (content already reachable costs zero,
  deduped per commit). POSIX attribution is the record's own owner; tenant
  attribution is the reserved `weft.tenant` attribute (`create --tenant`).
- **Usage storage.** The store-root `quota` file (`WFQ1 || u64le len || WCB ||
  BLAKE3("weft/quota/v1", WCB)`) carries the policy and cumulative counters
  keyed by the committed generation and head. It is written atomically after
  each commit and is never authoritative over content. On open, a record that
  does not name the committed head fails writes closed (`Corrupt`) and is
  reported by `verify`; `quota rebuild` recomputes usage from the published
  commit chain, `quota reset` zeroes counters, and both are explicit admin
  operations. `quota show` returns only the caller's own counters and limits.
  **(Superseded by Q3 below: usage is now a `set_quota_usage` op inside the
  commit; `WFQ1` is read only for migration.)**
- **Branching.** `snapshot branch` adds a ref to an immutable commit.
  `context branch` persists a new Context Root pinned to the source root's fork
  commit (optionally conjoined with `attrs.task`/`attrs.model`) and records a
  `derived_from` lineage relation. `merge` is an explicit **union** merge: a
  path present in both inputs or already at the live head must have identical
  content, else `Conflict` naming the path; it never overwrites the head and is
  not a three-way content merge.
- **Tests.** Rust `tests/quota.rs` 10/10 and `tests/branching.rs` 9/9;
  `cargo test --all-targets` and `cargo test --features fuse --all-targets`
  pass (the same two ignored ownership/concurrency cases as before). Go
  `store/quota_test.go` 7/7 and `store/branch_test.go` 7/7 (including both
  cross-language directions), full Go suite with `-race` passes in the fuse
  court, `go vet` and `CGO_ENABLED=0 go build` clean.
- **Cross-language.** Rust writes a store with quotas and a snapshot branch;
  Go loads the persisted policy, verifies, reads the branch, enforces a denial,
  and advances usage. The reverse direction does the same with the Rust CLI
  reading and enforcing the Go-written record.
- **Bench** (`bench/RESULTS-quota-branch.md`, receipt
  `bench/receipts/quota-branch-20261105T071244Z.json`): 1,000 commits p50
  11.35 ms without budgets and 13.75 ms with budgets (+2.41 ms, +21.2 %; the
  delta is the durable usage record); branch creation p50 9.11 ms over 50;
  merge of 1,000 disjoint paths 255.6 ms, 1,000 objects/entries, full verify
  ok.
- **Demo** (receipt `bench/receipts/branching-20261105T071457Z.json`, 7/7):
  10,000 versions, 21 snapshots, branch from `s5000`
  (`72ace03186863e95c9fe2c7f3ae166c8d5cfe2eed9a118cea6a34f5e0443cc7d`) into
  `by:task=agent-43`, five later writes, then `s5000`, `s10000`
  (`da172164095e35da79952b4a22b52130cc47a50a65b10367ec004088036a2762`), the
  snapshot ref, and the branch Context Root all read their exact historical
  bytes; live reads the 10,005th version; four mounts, zero surviving daemons.
- **Residual:** the quota record is the policy authority, so deleting it is a
  store-local admin act that disables enforcement (a present-but-unusable
  record always fails closed). Budgets charge admission, not footprint, so
  deletions do not return budget. The merge is a union, not a three-way merge.
  Tenant attribution is not yet set by the FUSE write path.

## Subphase P4 — Merkle content identity (`weft.manifest.v3`)

Implemented in both stores and specified in `spec/WEFT-MODEL.md` §2.3.2.
The measured bottleneck was the frozen v1 `ContentId =
H("weft/chunk/v1", whole_plaintext)`: every range write recomputed it by reading
the whole file (108 ms Rust / 392 ms Go in the FUSE court). v3 replaces it with
a Merkle root over the chunk table.

- **Format.** `weft.manifest.v3` carries the v2 fields with `content_root` in
  place of `content_hash`, plus `content_identity:"merkle-v1"`. Leaves are
  `H("weft/merkle/leaf/v1", u64le(len) || chunk_hash)`, nodes are
  `H("weft/merkle/node/v1", l||r)`, an odd trailing node is promoted unchanged,
  and an empty file is `H("weft/merkle/empty/v1", empty)`. The chunk domain for
  `chunk_hash` is the existing `weft/chunk/v1`. The root is chunk-size dependent
  by design; `chunk_size` is part of the manifest.
- **Selection.** `Options.content_identity = WholeFileV1 (default) | MerkleV3`
  and the global `--content-identity whole-file|merkle` flag in both CLIs.
  `merkle` + `codec-auto` is rejected. The policy is per invocation and not
  persisted. The default and every existing fixture stay byte-identical.
- **Write path.** A range write re-hashes and re-stores only changed chunks and
  folds the root from the manifest's stored chunk hashes plus changed leaves. No
  whole-file read for hashing. A `weft.manifest.v3` file is never implicitly
  rewritten as v1/v2; `truncate`, `zero/collapse/insert`, and reopen preserve
  it. Unknown schemas fail closed.
- **Read/verify.** Full reads verify every chunk blob and recompute the root;
  range reads verify touched blobs. `Store::verify_region` /
  `Store::VerifyContentRegion` verify the touched blobs and recompute the root
  from all chunk hashes (O(#chunks) small hashes; no stored sibling table) and
  name the failing chunk index. `verify --full` handles v1, v2, and v3.
- **Tests.** Rust `tests/merkle_identity.rs` 7/7 (determinism, chunk-size
  dependence, empty/single/multiple sizes, 128 MiB 4 KiB overwrite with at most
  two chunk reads, tampered blob naming the index, tampered chunk hash →
  `Corrupt`, unknown schema, reopen + GC). Go
  `store/merkle_identity_test.go` 6 tests. Cross-language
  `store/merkle_handoff_test.go` in the FUSE court: Rust→Go and Go→Rust read
  exact bytes, preserve v3, and report the identical root.
- **Bench** (`bench/RESULTS-merkle-identity.md`, receipt
  `bench/receipts/merkle-identity-20261105T161026Z.json`): 128 MiB, 64 KiB
  chunks, 500 random 4 KiB overwrites. Rust whole-file p50 50.43 ms / 67.2 GB
  `rchar` / 1,024,537 chunk reads; Rust Merkle p50 1.81 ms / 87.9 MB / 537.
  Go whole-file p50 202.73 ms / 134.3 GB / 437 MiB peak RSS; Go Merkle p50
  4.38 ms / 87.9 MB / 43 MiB. Every run verified full.
- **Residual.** The leaf fold is O(#chunks) per write (small, but not O(log n));
  no per-level sibling table; the root changes with `chunk_size`; there is no
  explicit v3→v1/v2 conversion command (implicit downgrade is refused); Go's
  v1 whole-file path is unchanged; one host and warm caches.

## Subphase P7 — training-ready is a storage refusal

Implemented in `src/training_ready.rs`, the `DependencyMismatch` error class,
snapshot-aware checkpoint/tensor verification helpers, the CLI verbs
`weft materialize` and `weft training-ready`, the normative
`spec/WEFT-TRAINING-READY.md`, and `tests/training_ready.rs` (10/10).

- **Contract.** A Context Root may be materialized as training-ready only when
  all six preconditions hold at the pinned snapshot: exact untruncated
  resolution; every decoded object verifies (v1 whole-file, v3 Merkle root, v2
  representation decode, symlink digest); every declared and transitively
  reachable causal parent is present (cycles terminate; a missing identity is
  `DependencyMismatch` naming it); every object and selector predicate is
  authorized under the supplied capability/envelope; every tensor logical root
  is recomputed (and every checkpoint manifest is fully decoded and reassembled
  when `--require-tensors` is given); and the estimate and the measured cost
  stay inside `--max-bytes`/`--max-objects`/`--max-decode-ms`. Any failure is a
  typed error, creates no output directory, and leaves no partial file.
- **Commands.** `weft materialize --root N --training-ready --out DIR
  [--max-*] [--capability FILE] [--require-tensors]`;
  `weft training-ready --root N --check`; and
  `weft training-ready --verify-out DIR`. An existing output path is `Exists`.
- **Output.** Verified decoded bytes at their store paths, plus canonical WCB
  `manifest.wcb` (`weft.training_ready.manifest.v1`) and
  `training-ready.wcb` (`weft.training_ready.v1`). No JSON is written into the
  output directory. Publication is a sibling temporary directory renamed into
  place after every check and write succeeds.
- **Reused verification.** `checkpoint::verify_checkpoint_at` and
  `tensor_store::tensor_logical_root_at` resolve and read against the pinned
  state; no second hashing implementation was added. The proof is the store's
  ordinary verified read path, not a trusted field.
- **Tests.** `tests/training_ready.rs` 10/10: happy path with relations and a
  real `checkpoint write` tensor; tampered chunk blob → `Corrupt`, no out dir;
  removed causal parent → `DependencyMismatch` naming the root; budget below
  actual → `BudgetExceeded`; corrupted `weft.tensor.logical_root` → `Corrupt`;
  capability that grants another namespace → `Unauthorized`; second run into an
  existing directory → `Exists`; a causal cycle terminates; check mode writes
  nothing; the CLI check/materialize/verify-out surface. `cargo test
  --all-targets` and `cargo test --features fuse --all-targets` both pass in
  the Docker courts (one pre-existing parallel `tests/quota.rs` lock flake
  passed standalone and on rerun).
- **Real-data demo.** Receipt
  `bench/receipts/training-ready-20261105T171104Z.json`, raw log beside it;
  `scripts/demo-training-ready.sh` in the FUSE court. Corpus: cached
  Qwen/Qwen2.5-0.5B BF16 container (SHA-256
  `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`),
  290 tensors, 988,065,536 element bytes, 2 raw shards per tensor (580 shard
  objects); the store is a copy of the P3 cached base store. Checkpoint write
  66.35 s, max RSS 1,174,908 KiB. `training-ready --check` 77.25 s, 552,156 KiB,
  all six preconditions true, 581 objects, 988,283,592 bytes, estimate 30,034 ms
  vs actual 77,099 ms. Materialize 98.14 s, 552,236 KiB, 581 files, 580 tensor
  objects verified, 290 checkpoint tensors verified. Offline `--verify-out`
  re-reads all 581 files, recomputes 580 tensor roots, reassembles all 290
  checkpoint tensors, and matches the source digest. The materialized
  checkpoint's `source_sha256` equals the source container's SHA-256.
- **Residual.** Only the Rust store implements training-ready (Go parity is
  not claimed); the output contains decoded shard bytes and a canonical
  manifest, not a single restored safetensors container; output verification
  trusts the canonical manifest for paths and only checks that every recorded
  byte and tensor root matches; the demo is one host, warm caches, and a copy
  of a cached public-weight store. The cost estimate is a declared
  throughput model, not a calibration.

## Subphase P8 — conformance manifest, third-party runner, decoder guide

- **Manifest.** `conformance/manifest.json` (295 entries) is generated by
  `scripts/gen-conformance-manifest.sh` (stdlib Python, no Docker) and committed.
  Each entry carries `path`, `kind`, `mode`/`mode_name` where a frame mode
  applies, `expect` (`ok` with `content_root` + `decoded_sha256`, or `error`
  with the normative class), `superseded_by`/`supersedes`, the vector file
  SHA-256, and byte-identical mirror paths. The top-level aggregate is
  `3c981f03fca856c0632ec48d5be166a1ae6513f87b393f934f3555482c2c0bda`;
  `--check` fails on drift and on any cross-repository mirror difference.
- **Third-party contract.** `scripts/run-conformance.sh <adapter>` invokes
  `<adapter> VECTOR.json`, requires exactly one JSON object
  (`ok`/`error`/`skip`), prints
  `VECTOR MISMATCH <path>: expected ... got ...` on any mismatch, and exits
  non-zero. `docs/CONFORMANCE.md` documents the contract, kinds, null
  semantics, skip rules, and exact court commands.
- **Reference adapters.** Rust, Go, and independent Python adapters shell out to
  the shipped CLIs; they never import library code. Committed receipts:
  `bench/receipts/conformance-rust-20261105T180000Z.json` (206 pass / 0 fail /
  89 skip), `conformance-go-20261105T180100Z.json` (206/0/89), and
  `conformance-python-20261105T175000Z.json` (56/0/239). Every skip carries a
  reason; the Python reader implements modes 0–8, and the record-level rejects
  and raw WCB/checkpoint kinds have no public CLI verb in the codec CLIs.
- **Decoder guide.** `docs/WRITE-YOUR-OWN-DECODER.md` gives the normative
  pointers for framing, WCB, hashing, modes 0–14, limits, failure classes, the
  exact check order (header CRC before lengths, trailer CRC after structural
  consistency, decoded length before content hash), frozen/experimental status,
  and a 50-line skeleton.
- **Coverage.** `scripts/check-conformance-coverage.sh` fails on a stale
  manifest, a mode 0–14 without a valid or reject vector, an identity kind
  without a valid vector, an incomplete expectation, or a mirror mismatch. It
  and the manifest check are `conformance_coverage` and
  `conformance_manifest` courts in `scripts/verify-all.sh`, both host-only.
- **Gaps filled.** The review found two identity kinds with no vectors. Added
  `WeftCodec/conformance/v1/capability/` (2 valid envelopes + 5 rejects;
  byte-identical in `WeftCodec-Go`) and `conformance/checkpoint/v1/`
  (`weft.checkpoint.v1` canonical manifest + 4 rejects), with Rust replay tests
  (`WeftCodec/tests/conformance.rs`, `tests/conformance_checkpoint.rs`) and a
  Go capability replay test. All modes 0–14 already had valid and reject
  vectors; no codec-mode vector was added.
- **CLI parity fixes found by the runner.** Rust `capability id --json` now
  reports `canonical_hex`; Go `capability id --json` does the same; Go
  `causal id` accepts an explicitly empty `--parents` (empty closure) while
  still requiring the flag; Go `decode` exposes the tensor/token limit flags.
- **Verification.** `bench/receipts/verify-20261105T174254Z.json` is 27/27
  courts pass at WeftFS `df9dcb0`, WeftCodec `b15353a`, WeftCodec-Go `9d275c0`,
  WeftFS-Go `4278445`. Residual: the reference adapters skip the kinds named
  above; the Python reader is not a full codec; the manifest is a snapshot and
  must be regenerated when vectors are added.

## Subphase Q1 — mount write path (merkle default, write-back, group commit)

Implemented and measured at WeftFS `staging` (this commit) and `WeftFS-Go`
staging (this commit), both buildable and testable only through
`scripts/docker-court.sh`.

- **Merkle identity is the mount default.** `mount` now defaults
  `--content-identity` to `merkle` (explicit `whole-file` still available); every
  non-mount command keeps the frozen `whole-file` default, so existing fixtures
  are unchanged. Reads accept v1/v2/v3. `mount --representation codec-auto`
  requires an explicit `--content-identity whole-file` because a v3 manifest
  carries a raw representation only.
- **`--write-back` (off by default).** Per-handle buffered ranges, published as
  one commit at `flush`/`fsync`/`release`; the writing handle reads its own
  buffered bytes, other readers see the committed version until the flush;
  `fsync` is durable before it replies; a crash loses unflushed ranges. Buffers
  are bounded (32 MiB per handle, early flush on overflow).
- **`--group-commit` (off by default).** Independent mutations share one durable
  commit within a 10 ms window (max 256 ops), also published on `fsync` and
  unmount. A crash leaves the previous whole generation. Publish failures after
  a reply are surfaced as `EIO` on later mutations and logged.
- **Redundant work removed.** `Store::write_file_ranges` applies many ranges
  with one manifest read, one manifest encode and one commit (used by a
  write-back flush); `Store::write_file_cached` reuses a handle's manifest only
  when it still names the object's current content id; `Store::manifest_of`
  seeds that cache. The store exposes `begin_group`/`commit_group`/
  `abort_group`; Go gained the same group API with a pre-image journal and
  `WriteFileRanges`/`WriteFileCached`.
- **Go FUSE parity.** Same `--write-back`/`--group-commit` semantics, same mount
  default, a per-handle buffer and overlay read, and a lifecycle-safe
  background group publisher (`OnForget`, unmount, and tests can stop it
  concurrently).
- **Tests.** Rust `cargo test --all-targets`: 200 passed, 0 failed. FUSE
  all-targets: 234 passed, 0 failed, 2 ignored (both pre-existing: kernel 7.3
  same-inode EIO and `CAP_SETUID`). New `tests/fuse_writeback.rs` (6),
  `tests/range_write.rs` (6), and three group-API tests in
  `tests/group_commit.rs` (11 total). Go `go vet`, `CGO_ENABLED=0 go build`, and
  `go test -race ./...`: all packages pass; new `store/group_test.go` (5) and
  `fusefs/writeback_test.go` (4).
- **Measurement.** `bench/receipts/fuse-writeback-20261105T202300Z.json`
  (single host, synthetic fixtures, buffered I/O, no device claim; 500 × 4 KiB
  overwrites of 128 MiB, 20×10 tree, crash proxy 8/8 pass). Key results:
  write-back takes merkle write p50 from 27.3 ms to 8.8 µs (Rust) and 33.6 ms to
  10.7 µs (Go); group-commit takes create from 162 to 26,548 ops/s (Rust) and
  310 to 6,264 (Go), unlink from 162 to 39,126 (Rust) and 311 to 9,610 (Go);
  whole-file writes are 4.4× (Rust) to 12.6× (Go) slower than merkle by
  default. Group-commit makes the write tail worse (p99 611–804 ms Rust,
  320–432 ms Go) because a request can wait behind a group publish. Crash proxy: unflushed buffered writes lost, flushed/durable kept,
  grouped metadata all-or-nothing, `verify --full` clean in 8/8 cases.
- **Recommendation.** `--content-identity merkle --write-back --group-commit`
  for AI-artifact mounts. Residual: write-back defers visibility (and a crash
  loses unflushed ranges); group commit defers durability; both are opt-in and
  the default remains commit-per-mutation; blob publication inside a group is
  still one durable write per touched chunk, which is why group commit helps
  metadata far more than content writes. This is a process-fault crash proxy,
  not a physical power-loss claim.

## Subphase Q2 — independently decodable tensor tiles (mode 15)

Implemented at WeftFS `staging` (this commit), WeftCodec `staging` and
WeftCodec-Go `staging` (this commit), all buildable and testable only through
`scripts/docker-court.sh`.

- **New payload mode 15 `TENSOR_TILED`** (`weft.ai.tensor_tiled.v1`,
  `WEFT-AI-CODEC` §16). Mode 10 is unchanged. A tensor is split into a
  rank-equal, exactly dividing tile grid; every tile stores its canonical
  row-major bytes as byte planes, each plane coded independently with
  `huffman-canonical-v1`. The descriptor records `tile_shape`, `tile_counts`,
  and per-tile `{index, dims, offset, compressed_len, element_len, tile_root}`.
  `tile_root = H("weft/tensor/tile/v1", WCB({dtype, dims, element_len,
  values_hash}))`; the whole-tensor `LogicalTensorRoot` is unchanged, so tiling
  is a physical representation, not a new identity.
- **Region API in both codecs.** `read_region(descriptor, region)` returns the
  region's canonical row-major bytes and `tiles_touched`, decoding and
  verifying only intersecting tiles. `read_region_with` additionally lets a
  range reader supply the stored tile bytes, so the WeftFS store reads only the
  descriptor prefix and the intersecting tiles' byte ranges from the backend.
  Full decode still verifies every tile root, the logical root, the frame
  trailer, and the content hash.
- **Checkpoint integration.** `checkpoint write --tile-shape A,B,...` writes
  mode-15 shard objects (mode 10 stays the default). A non-dividing tile shape
  is rejected with "no silent padding"; a tiled strategy without a tile shape
  is rejected. Restore detects the shard's representation and uses element
  coordinates for tiled shards; region restores report `units_touched` as the
  number of decoded tiles.
- **Tests.** Rust codec `cargo test --all-targets`: 194 passed, 0 failed,
  including `tests/tensor_tiled.rs` (7: roundtrips, region counts, encoder
  rejects, committed-vector replay 3 valid + 8 rejects). Go codec `go test
  ./...`: all packages pass, including `modes/tensor_tiled_test.go` and
  `conformance/tensor_tiled_test.go` (11 vectors replayed byte-exactly).
  Rust WeftFS `cargo test --all-targets`: 203 passed, 0 failed;
  `tests/checkpoint_tiles.rs` (3) proves exact whole-checkpoint restore, tiled
  region accuracy, two touched tiles for the sampled window, and lower backend
  bytes than mode 10. FUSE all-targets: 237 passed, 0 failed, 2 ignored
  (pre-existing). Go store/fuse `go test -race -count=1 ./store ./fusefs`
  passes.
- **Conformance.** `conformance/v1/tensor-tiled/` has 3 valid + 8 reject
  vectors, byte-identical in both codec repositories. The cross gate prints
  `YES byte-identical tensor_tiled_bf16` and `YES byte-identical
  tensor_tiled_fp32`, and both directions decode to the original bytes.
- **Measurement.** `bench/receipts/tensor-tiles-20261105T222531Z.json`
  (single-host fuse court, one real Qwen2.5-0.5B BF16 tensor
  `model.layers.0.mlp.down_proj.weight`, shape `[896, 4864]`, tile `[32, 256]`,
  10 reps per row, in-process store API). 100-row slice: mode-10 130.1 ms →
  mode-15 12.1 ms (10.8×), backend bytes 7,213,077 → 934,413 (−87.0%), touched
  units 1 shard → 76 tiles. 1-row slice: 130.0 ms → 3.2 ms (40.4×), bytes
  7,213,077 → 281,816 (−96.1%), units 1 → 19. The restored 100-row slices are
  byte-identical between the two strategies. After a tiled restore, the mount
  was read by the unmodified `safetensors` 0.8.0 + NumPy 2.4.6 + ml_dtypes
  0.6.0 loader: 1/1 tensor materialised, tensor SHA-256 equal to the source
  (`db708f9a…d572c6`), 5/5 court checks pass.
- **Residual.** Tiles are full-size only (no partial boundary tiles); the
  descriptor grows with tile count; a prefix range read deliberately skips the
  whole-frame trailer/content-hash check and relies on per-tile roots; the
  benchmark is one host, one public tensor, and a warm in-process store, not a
  fleet or remote-object-store claim; WeftFS-Go does not implement
  `weft.checkpoint.v1` and therefore does not yet write or read tiled
  checkpoints.

## Subphase Q3 — quota accounting inside the commit transaction

Implemented in Rust and Go; normative text in `spec/WEFT-MODEL.md` §2.2 and
`spec/WEFT-QUOTA.md` §1.3–1.5; operational notes in
`docs/quota-branching.md`.

- **Where usage lives.** A quota-enabled commit appends exactly one
  `set_quota_usage` op as its last op. The op carries the cumulative admitted
  usage (sorted principal array, `reset:false`) after all other ops in the same
  commit, or is a standalone `reset:true` op with an empty array. It changes no
  namespace state, so checkpoints are unchanged. The commit schema stays
  `weftfs.commit.v1`; old commits with no usage op replay unchanged.
- **No second durable write.** The store-root `quota` file is now policy-only
  (`WFQ2 || u64le len || WCB || BLAKE3("weft/quota/v2", WCB)`); it is written
  only when a policy is installed/replaced or a legacy record is migrated.
  Ordinary commits perform one commit publication and no quota-file write.
- **Admission and replay.** Admission is still computed before publication;
  denial is `BudgetExceeded: write budget exceeded` with nothing published.
  Replay recomputes the admission totals and rejects a `reset:false`
  assertion that disagrees as `Corrupt`; a reset must be alone and empty.
  `verify --full` validates every assertion and compares the recomputed totals
  with the in-memory counters. `quota rebuild` validates, then publishes a
  corrective op; `quota reset` publishes one reset op. Both are explicit admin
  operations.
- **Migration.** A legacy `WFQ1` record is imported once on writable open:
  usage is recomputed from the chain, the exact legacy bytes are preserved as
  `quota.migrated`, `quota` becomes `WFQ2`, and a corrective usage op is
  published when needed. If `quota` and `quota.migrated` exist with different
  bytes, or a preserved record naming the current head disagrees with committed
  usage, writes fail closed with `Corrupt`. A stale legacy record has its usage
  recomputed from the authoritative chain and the repair is noted.
- **Cross-language.** Rust-written usage ops are read, verified, and enforced
  by Go; Go-written usage ops are read, verified, and enforced by Rust.
  `TestQuotaUsageOpCrossLanguageEquality` compares numeric counters in both
  directions and proves both implementations deny an oversized write.
- **Tests.** Rust `tests/quota_usage_op.rs` 9/9; `tests/quota.rs` 10/10.
  `cargo test --all-targets`: 212 passed, 0 failed. `cargo test --features fuse
  --all-targets` in the fuse court: 246 passed, 0 failed, 2 ignored
  (pre-existing). Go `go test -race -count=1 ./store/...` passes, including 6
  new Q3 tests; `go vet` and `CGO_ENABLED=0 go build` clean.
- **Bench** (`bench/RESULTS-quota-branch.md`, receipt
  `bench/receipts/quota-branch-20261105T224337Z.json`, bench court): 1,000
  single-object commits, p50 11.278 ms without a budget and 11.344 ms with a
  budget (+0.066 ms, +0.59 %); p95 14.928 → 14.848 ms; wall 11.661 → 11.736 s.
  Historical P5a before was 13.754 ms p50 (+21.2 %); Q3 is −17.5 % against it.
  Store bytes 1,926,625 → 2,028,648 (+102,023 B, the 1,000 usage ops). Usage
  after the run is 1,000 objects / 1,383,000 bytes; verify ok.
- **Residual.** Trusting the head assertion on open is O(1); full re-derivation
  and rejection of an inconsistent assertion happen in `verify --full`,
  `quota rebuild`, and any open that must replay the chain. The policy file is
  still the enforcement authority: deleting it disables enforcement. Usage is
  an admission counter, not a reclaimable footprint; deletes do not return
  budget. One host, synthetic fixtures, one repetition.

## Subphase Q4 — real S3-compatible backend and MinIO court

Implemented in Rust on `staging` at `8683ecc` (code commit); not pushed.

- **Backend.** `S3Backend` implements `StorageBackend` over SigV4 with a minimal
  blocking client (`ureq`, rustls-capable). It uses the local logical key
  layout verbatim (`objects/<ab>/<hash>`, `manifests/<ab>/<hash>`,
  `commits/<gen>.commit`, `indexes/...`, `refs/...`, `FORMAT`, `superblock`,
  `LOCK`, `quota`). Blobs use `PUT` + `If-None-Match: *` (identical bytes
  idempotent, different bytes `Exists`), range reads use HTTP `Range`, and the
  two-slot superblock is updated by whole-object `GET` + `PUT` with
  `If-Match: <ETag>`. A lost conditional update retries from a fresh read and
  then fails `Busy`; it never overwrites the head blindly. Writable open probes
  both conditional forms and refuses (`UnsupportedVersion`) if the endpoint
  ignores either. CLI: `--backend s3` plus `--s3-endpoint`, `--s3-bucket`,
  `--s3-region`, `--s3-access-key`, `--s3-secret-key` (or the AWS_* variables).
  `--store` is optional for S3.
- **Lock.** The writer lock is a `LOCK` object carrying an owner, a monotonic
  fencing token, an absolute lease deadline, and the head being published,
  created with `If-None-Match: *`. A live second writer fails `Busy`; a crashed
  writer's lease is reclaimed with `If-Match` at the next fencing token. Since
  R27 the record itself publishes the head (one conditional write), so a stale
  writer cannot publish after a takeover; the superblock is an attested cache.
  This is advisory and is not a POSIX `flock` or a multi-host protocol. See
  [`FENCING.md`](FENCING.md); the Q4 paragraph below records the pre-R27 shape.
- **Capabilities.** `atomic_publish`, `durable_fsync` (provider
  acknowledgement only; no local fsync), `true_range_reads`, max object bound
  1 GiB, consistency `single-node S3-compatible endpoint; read-after-write and
  list-after-write; conditional writes required`. Publication text states the
  `If-None-Match`/`If-Match` protocol.
- **MinIO court.** The official `minio/minio` Docker Hub repository was not
  pullable on 2026-11-05 (registry 404/unauthorized).
  `scripts/s3_court.sh` runs the official linux-amd64 MinIO release binary
  `RELEASE.2025-04-22T22-12-26Z` (sha256
  `53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`) in a
  minimal `debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc`
  container with the same hard limits (`3g`, swap `3g`, `1024` pids, `4`
  CPUs, bounded tmpfs, no-new-privileges, `cap-drop=ALL`) and a tmpfs data
  directory. It creates a bucket, runs `tests/s3_backend.rs`, and tears down the
  server and network. `scripts/docker-court.sh` gained a validated optional
  network and forwards only named `WEFT_S3_*`/`AWS_*` variables.
- **Tests.** With MinIO: `tests/s3_backend.rs` 6 passed / 0 failed / 0 ignored
  (init/open/create/read/range/write/truncate/rename/snapshot/`verify --full`;
  live second-writer `Busy`; lost `If-Match` leaves the previous head readable;
  process-fault crash proxy at `after_objects`, `after_commit_file`,
  `after_superblock_write`; GC removes only unreachable content;
  byte-identical local-vs-S3 artifacts). Without the S3 environment each test
  prints `SKIP s3_backend: WEFT_S3_TEST_ENDPOINT is not set` and passes.
  Rust `cargo test --all-targets`: 221 passed / 0 failed / 0 ignored. FUSE
  `--features fuse --all-targets`: 255 passed / 0 failed / 2 ignored
  (pre-existing).
- **Bench** (`bench/receipts/s3-backend-20261105T233154Z.json`, court receipt
  `bench/receipts/s3-court-20261105T233154Z.json`): single-node MinIO on
  localhost, same seeded operations, same head `8e47145e1a09c3bf…`, generation
  1101. 1,000 creates: local 105.9 ms / 9,442 ops/s (p50 100 us, p95 154 us)
  vs S3 1,809.7 ms / 552.6 ops/s (p50 1,781 us, p95 1,967 us). 100 MiB in 100
  files: 192.0 ms / 520.9 MiB/s vs 1,168.3 ms / 85.6 MiB/s; p50 664 vs 10,300
  us. 1,000 reads: 4.4 ms / 224,799 ops/s vs 345.8 ms / 2,892 ops/s; p50 4 vs
  334 us. Backend bytes written 111,581,685 vs 116,091,480 (+4.0%, from
  whole-superblock rewrites and lock/probe traffic). `verify --full` ok on both.
- **Residual.** Rust only. MinIO is one node on localhost with a tmpfs data
  directory: protocol and conditional-write safety, not media durability or
  power loss. The writer lock is a lease (default 900 s; court 2 s); a paused
  writer longer than the lease can lose it. No AWS, GCS, 3FS, cloud,
  multi-node, or fleet claim.

## Q5 — PyTorch Distributed Checkpoint adapter (2026-11-06)

Implemented on `staging` at `d732a9631d0fae395ddacf36d99e816a214dc289`
(code `464b38f`; one test-only follow-up). The Python court now carries
**torch 2.15.0+cpu** (official CPU wheel, 209.5 MB as downloaded). Real
`torch.distributed.checkpoint.save`/`load` ran; this is not a contract-only
fixture and no stock DCP storage plugin was used.

- **Adapter.** `python/weftcodec/dcp.py` implements
  `WeftStorageWriter`/`WeftStorageReader` with `reset`,
  `write_metadata`/`write_data`/`finish`, `storage_meta`, `read_metadata`,
  `read_data`, `set_up_storage_*`, the plan hooks, and
  `validate_checkpoint_id`. Tensor bytes become one canonical
  `weft.checkpoint.v1` checkpoint through `weftfs checkpoint write`; DCP
  protocol metadata is canonical WCB (`weft.dcp.metadata.v1`) at
  `/checkpoints/<name>/dcp.metadata`. Every content root, dtype and shape is
  cross-checked against the Rust manifest on write and after every restore.
  A Rust-written plain-safetensors checkpoint is read back through synthesised
  metadata (`metadata_source == "manifest"`).
- **Topology.** `topology=n` stores n-way axis splits; a non-divisible extent is
  rejected by the Rust CLI ("no silent padding"). The test writes `16x8`,
  `16` and `8x4` tensors 8-way, loads into a fresh module, and restores each
  entry through `--topology 8|4|1`; 3 tensors x 3 topologies were byte-identical
  with the same content root (`10 passed`, `python/tests/test_dcp.py`).
- **Async.** `save_checkpoint_async` runs `dcp.save` on a ThreadPoolExecutor
  worker with an explicit `staging_bytes` bound. Bench: submit 0.72 ms
  (median), completion 1,329.0 ms vs synchronous 1,316.5 ms. Tensors are copied
  through host memory; it is **not a GPU-direct path**.
- **Court.** Built wheel
  `bench/receipts/python-bindings-20261106T000719Z.json`: 191 tests, 0 failures,
  62 skipped. A superseded failing receipt
  `python-bindings-20261106T000301Z.json` is retained: the newly-enabled
  dataset import test walked the repository root across build caches and was
  OOM-killed; the test now uses a bounded fixture root.
- **Bench.** `bench/receipts/pytorch-dcp-20261106T000734Z.json` (revision
  `d732a96`, `weft/python-court:1`, kernel `7.3.1-1-cachyos`, 3 GiB / 4 CPUs /
  1024 pids, single host, CPU, synthetic 8 x 4 MiB f32, no GPU claim). Full save
  1,223.6 ms; full load 152.2 ms; single-tensor load 67.2 ms; staging peak
  4,194,304 B; peak RSS 381,424 KiB; WeftFS physical 31,187,325 B vs
  `torch.save` 33,557,979 B (0.9294); mode-10 tensor-sharded save 913.3 ms /
  32,826,050 B. Rust `checkpoint verify` 8 tensors and `verify --full` ok, no
  problems. The superseded `pytorch-dcp-20261106T000232Z.json` is retained.
- **Docs.** `docs/PYTORCH-DCP.md` (API, metadata schema, exactness table,
  topology evidence, bench, non-claims) and the DCP section in
  `python/README.md`.
- **Not done / residual.** No GPU-direct path, no multi-rank cluster run, no
  media power-loss claim. `BYTE_IO` objects under a split topology need
  divisible lengths (reject, never pad). Reads import restored containers into
  the store. Save/load are ~141x/~30x slower than one `torch.save`/`torch.load`
  file on this fixture, while writing 7.1 % fewer physical bytes.

## Q6 — signed, verifiable receipt bundles with store provenance linkage (2026-11-06)

Implemented on `staging`; packaging and verification were run end to end on a
real ~1,061-file bundle.

- **Canonical manifest.** `src/receipt_bundle.rs` defines
  `weft.receipt-bundle.manifest.v1`: `generated_at`, `stamp`, `files`
  (`path`, `size`, `sha256`), and `revisions` (`name`, `revision`, `branch`,
  `worktree`, `changed_files`). Entries are strictly sorted by canonical encoded
  bytes; duplicate, unsorted, absolute, traversing, backslash, or control-byte
  paths are rejected. Decode re-encodes and requires byte equality, so a
  non-canonical manifest is refused before any signature check.
- **Signing.** `scripts/package-receipts.sh --sign-key FILE` (32 raw bytes or 64
  hex) signs `H("weft/receipt-bundle/v1", manifest_canonical_bytes)` with
  Ed25519 and writes `signature` and `public_key` next to
  `receipt-manifest.wcb`. It uses `weftcodec::capability::sign_bytes` and
  `verify_bytes`, the same code path as capability envelopes. Output is
  `receipts-<stamp>.signed.tar.zst`. Without a key the bundle is
  `receipts-<stamp>.tar.zst`, carries `UNSIGNED.txt`, and says it is unsigned.
  The seed is streamed to the court on stdin (`--sign-key -`) and is never
  copied into the repository or `.docker-cache`; it never enters the archive.
  The manifest, signature, public
  key, and `CHECKSUMS.sha256` are metadata and are deliberately not listed in
  the manifest.
- **Verification.** `scripts/verify-receipts.sh BUNDLE --public-key <64-hex>`
  (or `--signature-from-bundle`) re-hashes every listed file, rejects unlisted
  payload files, decodes the canonical manifest, then verifies the signature.
  It prints `SIGNATURE OK ed25519:<fingerprint>` or fails closed with a precise
  reason. An unsigned bundle fails under either key mode with
  `missing signature file; bundle is unsigned`; a wrong key fails with
  `public_key file does not match the requested key`; tampering fails with
  `sha256`, `unlisted payload file`, or `signature does not verify under ...`.
  Legacy bundles without `receipt-manifest.wcb` still verify structurally and
  report `signature: not applicable`.
- **Tests.** `tests/signed_bundle.rs` 11/11: sign/verify round trip, wrong key,
  tampered same-size file, missing signature, unsigned-with-key, unsigned
  default, wrong-key library path, deterministic manifest/digest/signature,
  non-canonical and trailing-byte rejection, traversal rejection, unlisted
  payload, and `weft provenance show` linkage. Rust `cargo test --all-targets`
  234 passed / 0 failed across 66 test binaries; FUSE `--features fuse
  --all-targets` 268 passed / 0 failed / 2 ignored; Go `go test ./...` in
  `WeftCodec-Go` passed every package. Two earlier full runs hit pre-existing
  flakes (`tests/quota.rs` writer lock, `tests/representation.rs` chunk
  counter); each passed standalone and the published full run was clean.
- **Store linkage.** `scripts/demo-signed-bundle.sh BUNDLE` stores the canonical
  manifest as a real file, creates snapshot `q6-signed-bundle`, records a
  `weft.provenance.v1` receipt whose output is the stored manifest's content
  root, and publishes `derived_from` to the snapshot. `weft provenance show
  /bundle.manifest` resolves the provenance id, the `derived_from` input, and
  the snapshot id; `weft relation list --predicate derived_from` resolves the
  edge. Tampering changes both the signed digest and the recomputed output
  content root, and the old signature no longer verifies.
- **Go cross-check.** `WeftCodec-Go/capability` gained `SignBytes`/`VerifyBytes`
  (same `crypto/ed25519` path) and `cmd/verify-ed25519` verified the Rust
  signature over the real manifest digest: `VERIFY OK`; a changed digest
  produced `VERIFY FAIL`. This was run in the Go court.
- **Bench** (`bench/receipts/signed-bundles-20261106T010118Z.json`, raw log
  beside it; single host, clean worktree at revision `259c29f`, ephemeral
  benchmark-only keys, kernel `7.3.1-1-cachyos`, 3 GiB / 4 CPUs / 1024 pids):
  1,064 payload files, 1,139 tar entries. Signed package 19,648 ms; unsigned
  12,091 ms. Pinned-key verify 9,029 ms; unsigned verify 8,981 ms. Signature
  64 raw bytes (129-byte hex file), public key 65 bytes, manifest 128,731 bytes;
  raw metadata added (manifest + signature + public key − `UNSIGNED.txt`)
  128,633 bytes. The observed compressed bundle delta was −54 bytes because the
  two runs have different timestamps and one has `UNSIGNED.txt`; it is not
  attributable to signing. All 15 rejection/acceptance cases passed, including
  the Go checks, the seed-not-cached and seed-absent-from-bundle checks, and the
  provenance demo.
- **Non-claims.** No certificate authority, no transparency log, no key
  distribution or rotation, and no claim that the signer is trustworthy.
  Signing proves bundle integrity relative to a key, not the truth of the
  measurements. Verification is offline; it uses the local repository and its
  Docker court. A leaked seed permits forged bundles under that key. Bundle
  timings are one host and warm caches, not a fleet or storage-media claim.
  The canonical manifest is an advisory artifact for this bundle format, not a
  new frozen WeftCodec wire mode; the frozen v1/v2 store and codec formats are
  unchanged.

## Q7 — v0.8.0 release (2026-11-06)

v0.8.0 is cut from `staging` after the Q1–Q6 subphases. Versions:
`weftfs` 0.8.0 and `weftcodec` 0.8.0 in `Cargo.toml` and `Cargo.lock`; the
parent pins `weftcodec = "0.8.0"`. Python wheel
`python/dist/weftcodec-0.8.0-py3-none-any.whl`, 45,400 bytes, sha256
`5a41c419a43e9d7ba16dd14c2d170ecd34d102f16c69d51652af5245e0fe91ab`, built and
tested from the installed wheel in the Python court (191 tests, 0 failures,
0 errors, 62 skipped; receipt
`bench/receipts/python-bindings-20261106T012031Z.json`). **Not published to
PyPI** (no token); the wheel is a release asset.

Release verification: `bench/receipts/verify-20261106T011444Z.json`, **27/27
courts pass**, at WeftFS `d6ba189`, WeftCodec `3f3eae0`, WeftCodec-Go
`65bc14d`, WeftFS-Go `f920ae0` (before the version-literal bump). The retained
first run `bench/receipts/verify-20261106T010756Z.json` failed on the
pre-existing quota writer-lock flake; the retained Python receipt
`bench/receipts/python-bindings-20261106T011946Z.json` failed because the shared
release binary was fuse-linked and the Python court image has no
`libfuse3.so.3`. Both clean reruns are the published receipts. Full note with
every loss: `docs/RELEASE-v0.8.0.md`.

Subphase evidence: Q1 `bench/receipts/fuse-writeback-20261105T202300Z.json`,
Q2 `bench/receipts/tensor-tiles-20261105T222531Z.json`, Q3
`bench/receipts/quota-branch-20261105T224337Z.json`, Q4
`bench/receipts/s3-backend-20261105T233154Z.json` and
`bench/receipts/s3-court-20261105T233154Z.json`, Q5
`bench/receipts/pytorch-dcp-20261106T000734Z.json`, Q6
`bench/receipts/signed-bundles-20261106T010118Z.json`.

## R3 — mode 16 `SPARSE_DELTA` (2026-11-06)

Additive payload mode 16 (`weft.ai.sparse_delta.v1`) in both codecs and both
stores. Mode 11 `WEIGHT_DELTA` is untouched and byte-identical; mode 16 is the
sparse counterpart, not a replacement.

- **Format.** Exactly nine descriptor keys (`schema`, `dtype`, `shape`,
  `element_count`, `storage_bytes`, `op` = `mod-sub`, `sparse_codec` =
  `zero-run-v1`, `base_content_root`, `delta_root`); one dependency naming the
  base tensor; body is `u32le(meta_len) || WCB(meta) || u32le(run_count) ||
  run_count records`, each record `varint(zero_run_len) ||
  storage_bytes(dtype) bytes`. Trailing zeros are implicit; an all-zero delta is
  `run_count = 0`; a stored zero word and trailing bytes are rejected. Spec:
  `spec/WEFT-AI-CODEC.md` §17; mode tables and the reserved range (now
  `17..=127`) updated in `WEFTCODEC-FORMAT` and `WEFT-COMPATIBILITY`.
- **Limit.** New `max_sparse_runs` (default 16,777,216, CLI
  `--max-sparse-runs` in both codecs). A denser delta is `LimitExceeded` at
  encode and decode; the store planner then keeps mode 11.
- **Vectors.** `conformance/v1/sparse-delta/` 18 files (4 valid: all-zero, one
  word, several runs ending exactly at `element_count`, fully dense;
  14 rejects), byte-identical in both codec repositories.
  `017_unknown_mode_reserved16` is retained byte-for-byte and marked
  `superseded_by` `017_unknown_mode_reserved17`. Manifest 326 entries
  (aggregate `1fc710a8…`), coverage mode 16 = 4 valid / 14 reject. Rust and Go
  reference adapters replay 326 total, **224 pass, 0 fail, 102 skip**, no
  sparse-delta skips.
- **Cross-language.** `WeftCodec-Go/hack/cross.sh` gains three mode-16 fixtures;
  `YES byte-identical sparse_delta_bf16 / sparse_delta_q8 /
  sparse_delta_fp32` and decode equivalence in both directions; the whole gate
  reports "all checks passed".
- **Planner.** `TensorStrategy::SparseDelta` / `CostClass::SparseDelta` in Rust
  and Go, offered next to mode 11 whenever a causal parent has the same
  dtype/shape and priced by the same integer cost table (the SparseDelta row
  **reuses** the measured weight-delta coefficients; it was not separately
  calibrated and the table says so). `tests/planner_sparse.rs` (3 tests): a
  19-word change selects mode 16 and reads back exactly with an independently
  recomputed `delta_root`; a fully dense delta selects mode 11; a forced dense
  mode-16 object is still readable after reopen. Go reads and verifies
  Rust-written mode-16 objects and vice versa (`TestTensorHandoff*` pass in
  both directions with both modes).
- **Measurement** (`bench/receipts/sparse-delta-20261106T052333Z.json`,
  bench court `sha256:4041d66e…`, revisions `weftfs eac3687`, `weftcodec
  45a319b`, `weftcodec_go e47fa03`, `weftfs_go 47fd4d2`; analysis
  `bench/RESULTS-sparse-delta.md`). Consecutive pythia-70m pairs:
  step0→step1 mode 16 **29,887 B vs 7,434,959 B** mode 11 (0.0040×; 0.0002×
  per-tensor zstd-19; 0.988× the whole-file `zstd --patch-from -3` recorded by
  P2), 76/76 tensors smaller. step1→step2 0.647× (27/76 tensors smaller);
  step2→step4 **2.22× (loss)**; step4→step8 **3.29× (loss)** with
  `embed_out.weight` refused by `max_sparse_runs`. Fine-tune Qwen
  base→instruct: **1.94× (loss)**, 48/290 tensors smaller,
  `model.embed_tokens.weight` refused. Mode 16 encoded and decoded faster than
  mode 11 in every pair (decode 653–815 MB/s vs 290–409 MB/s consecutive;
  319 vs 195 MB/s fine-tune) but both remain 1.8–9.6× slower than `zstd -19`.
  Consequences: the planner now stores a 19-byte checkpoint change in ~30 KB
  instead of ~7.4 MB, and keeps mode 11 for dense pairs instead of paying the
  run stream.
- **Tests.** Rust codec `cargo test --all-targets` all pass (new
  `tests/sparse_delta.rs` 12/12); parent `cargo test --all-targets` 257 passed /
  0 failed and `--features fuse --all-targets` 291 passed / 0 failed / 2 ignored
  (the two pre-existing ones); Go codec `go test ./...` all packages pass; Go
  store `go test -race -count=1 ./...` store package passes and `./fusefs`
  passes in the fuse court.
- **Changed expectations (honest, not hidden).** Two Rust planner tests and one
  Go planner test used a sparse fixture and expected mode 11; the smaller
  representation is now mode 16, so they expect mode 16 and assert mode 11 is
  still priced and larger. The mode-reserved structural tests moved 16 → 17.
  Cross-language handoff fixtures now write both an auto (mode 16) and a forced
  mode-11 object.
- **Residual.** Single host and warm cache; the receipt is codec-level
  per-tensor frames, not store-planner bytes; zstd baselines are copied from a
  prior receipt; mode 16 is not entropy-coded, so medium-density deltas still
  lose to mode 11; `max_sparse_runs` refused one tensor in each of two pairs.

## Subphase R5 — entropy-coder court (2026-11-06)

Experimental, no wire change. `WeftCodec/src/experimental/entropy/` adds a
Tunstall coder (k=8/10/12), a static order-0 32-bit rANS coder, zstd levels
3/19, raw, and the shipped `huffman-canonical-v1` as the baseline. No
persistent encode/decode path calls the module; the court script fails if a
production source file references `experimental::`. No wire mode, table, or
frozen format changed.

- **Inputs.** Real cached weights (all 7 files re-hashed in court; manifest
  sha256 `89fce691…`): Qwen2.5-0.5B/Instruct BF16 and pythia-70m step0/1.
  Plane inputs are extracted through the shipped encoders for modes 9 (exponent
  and sign+mantissa), 11 (dense fine-tune and sparse step0→step1 residuals,
  anchored at changed row 2304), 12 (1 MiB KV page), and 15 (tile [64,128],
  532 tiles). Workload shapes: whole, 100 rows, 1 row (KV: page, 100 rows,
  1 row). 252 rows, 14 coder configurations.
- **Correctness.** Every row round-trips byte-exactly. `tests/
  entropy_experimental.rs` 10/10 covers empty/one-symbol/all-256/skewed/
  incompressible/non-granular inputs, the documented Tunstall rule, shared
  dictionaries over unseen symbols, malformed headers, and 5,000 randomised
  cases plus 5,000 mutated streams each for Tunstall and rANS against
  independent reference decoders. WeftCodec and parent `cargo test
  --all-targets` pass. Cross gate: all checks passed, modes 14/15/16
  byte-identical. Conformance replays: Rust and Go 224 pass, 0 fail, 102
  skip against manifest
  `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0`.
- **Bytes.** rANS is the smallest coder on mode-9 BF16 whole planes
  (**5,753,076 B**, 0.6600; 0.86% under zstd-19 and 0.64% under Huffman),
  mode-11 dense whole planes (**4,357,033 B**, 0.4999; 2.40% / 3.58% under
  them), mode-15 tiles (5,804,101 B), KV pages (698,896 B), and the sparse
  whole planes (**1,188 B** vs 3,288 B zstd-19). zstd-19 is smallest on FP32
  mode-9 (**2,014,728 B**, 12.1% under rANS) and on 1-row regions.
- **Speed.** Shipped Huffman 1,161/1,229 MiB/s encode/decode; rANS
  312/305 MiB/s; zstd-19 28.5/9,652 MiB/s; zstd-3 1,807/6,808 MiB/s.
  rANS is 2.0× slower than Huffman on BF16 decode and 23.7× slower than
  zstd-19; on sparse whole planes the gap is 74.4×.
- **Tunstall.** k=8 cannot split (`256+255 > 256`), so it is raw plus
  dictionary (ratio 1.0001–1.0002). k=10/12 compress some planes but are
  47–59% above the best coder on non-sparse whole planes and ~3,000× above
  zstd-19 on the sparse whole planes; on 1-row regions their total is
  6.6–30.5× the best non-sparse coder. Tunstall wins nothing.
- **Dictionary scopes.** Per-plane is the best byte scope at k=12 for whole
  and 100-row multi-plane sets in five of six classes (34,465 B mode 9/tiles,
  38,290 B dense, 34,210 B KV, 171,440 B sparse). The sparse whole planes
  are the exception: 171 KB per-plane loses to 42,860 B per-tensor on total
  bytes. Per-tensor is not a default. Family-shared is an amortisation tool,
  not a compression win: 18,125–42,860 B charged once and amortised to
  62–564 B per tensor (N=290/76/169) or 2,234 B for 8 KV pages; it does not
  improve stream bytes. At 1-row granularity every static dictionary loses.
- **Memory.** The dense full-checkpoint family histogram peaked at
  **2,248,448 KiB (2.14 GiB)** through the shipped delta encoder on the 272 MB
  embedding; the per-row matrix peak was 520,708 KiB. Inside the 3 GiB court,
  about 0.86 GiB headroom.
- **Recommendation: Promote nothing.** Keep `huffman-canonical-v1`. rANS's
  0.5–3.6% size win is not worth a wire change plus a 2× decode loss; zstd-19
  is a size reference but its 28.5 MiB/s encode rules it out as a write path;
  Tunstall is rejected at every k and scope. Conditional follow-up only:
  rANS with a decoder at Huffman throughput; a frame-based coder if 1-row
  reads become first-class.
- **Evidence.** Receipt `bench/receipts/entropy-coder-20261106T065154Z.json`
  (252 rows; supersedes `entropy-coder-20261106T064820Z.json`, whose build-peak
  field was captured after a reset and is not meaningful). Revisions `weftfs
  4e5cb95`, `weftcodec 72cd119`, `weftcodec_go e47fa03`, `weftfs_go 47fd4d2`.
  Analysis `bench/RESULTS-entropy-coders.md`. Court runner
  `bench/scripts/entropy_coder_court.sh`, renderer
  `bench/scripts/render_entropy_results.py`.
- **Residual.** One host/warm cache; one tensor per class; codec-level plane
  streams, not a shipped frame or store planner; no zstd dictionary/long-window;
  rANS not micro-optimised; no substring-frequency Tunstall dictionary.

## Subphase R6 — Python reader: all AI modes 0–16 (2026-11-06)

The independent Python reader (`python/weftcodec/`) now implements every
allocated WFT1 payload mode: 0–8 as before, plus 9 `FLOAT_TENSOR`, 10
`TENSOR_SHARDED`, 11 `WEIGHT_DELTA`, 12 `KV_CACHE`, 13 `ACTIVATION_STREAM`, 14
`SEMANTIC_DELTA`, 15 `TENSOR_TILED`, and 16 `SPARSE_DELTA`. No wire format,
vector, descriptor, or manifest changed.

- **New code.** `python/weftcodec/huffman.py` (212 lines) is a standalone
  `huffman-canonical-v1` decoder with the §5.3.1 validation order and classes
  (length > 24 and Kraft over/incomplete → `NonCanonical`; truncated stream,
  trailing bytes, non-zero padding → `Corrupt`). `python/weftcodec/ai_modes.py`
  (2,570 lines) implements descriptor parsing through the existing canonical
  WCB reader, FLOAT_TENSOR plane join for bf16/fp16/fp32/fp8-e4m3/fp8-e5m2
  (bit-exact, including NaN payloads, denormals, signed zeros), q4_0/q8_0
  shard dequantised-byte reconstruction and shard-set roots, WEIGHT_DELTA and
  SEMANTIC_DELTA modular word reconstruction with `delta_root` and semantic-root
  verification, KV_CACHE and ACTIVATION_STREAM byte planes (plus `stream_root`),
  TENSOR_TILED per-tile roots and the whole-tensor logical root, and
  SPARSE_DELTA `zero-run-v1`. `modes.py` dispatches all sixteen modes;
  `dependency.py` gains frame-source resolution (`resolve_frame`,
  `FrameSourceResolver`, cycle/depth/fetch/decoded bounds); `cli.py` gains
  `--base`, `--resolve`, and `--frame-source`. Total reader core (excluding the
  optional `dcp.py`/`torch.py`) is 5,288 lines; the new AI code is
  2,782 lines (`huffman.py` + `ai_modes.py`).
- **No new dependency.** The reader keeps its existing `blake3` and
  `zstandard` dependencies and adds none (Huffman and all AI payload work are
  pure standard-library Python).
- **Conformance adapter.** `scripts/conformance/adapters/python.py` now imports
  the package directly and executes codec frames, adversarial decode vectors,
  shard sets, delta bases (including frame-source cycle vectors), KV pages,
  activation chunks, tiled frames, semantic roots, and canonical WCB values.
  Skips are explicit and only for surfaces the reader does not claim.
- **Court command and result.**
  `bash scripts/docker-court.sh python "cd /work && PYTHONPATH=/work/python WEFT_PYTHONPATH=/work/python scripts/run-conformance.sh scripts/conformance-adapter-python.sh --quiet --json bench/receipts/conformance-python-20261106T072745Z.json"`
  → **326 total, 218 pass, 0 fail, 108 skip** (manifest
  `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0`). The
  previous Python result was 56/0/270. No mode 0–16 vector is skipped.
  Remaining skips: 29 WEFT-TENSOR layout/shard containers, 22 selector, 17
  Context Root (including `causal_versions`), 10 causal closure, 7 capability,
  5 checkpoint, 7 WFA1 archive verbs, 2 `selector-id` adversarial verbs,
  2 composite store/fuzz fixtures, and 9 superseded vectors.
- **AI vector suite.** `python/tests/test_ai_modes.py` asserts **128 AI vectors
  (40 valid, 88 rejects)**, 0 skipped. The suite reports 128 AI vectors (40 valid, 88 rejects). Valid vectors decode to their recorded
  bytes and recompute every recorded root (`content_hash`, `layout_root`,
  `delta_root`, `logical_root`, per-tile roots, `stream_root`, semantic root);
  every reject raises the declared class. The same test decodes each valid
  frame with the Rust CLI and requires identical bytes (and, for mode 10, the
  same `tensor root --frames` value).
- **Randomized differential.** `python/tests/test_random_differential.py`
  generates **16,003 frames** (at least 2,000 per mode 9–16) from a
  deterministic xorshift64* PRNG via `python/tests/ai_frames.py`, decodes each
  with the Python reader and
  `/work/.docker-cache/target-rust/release/weftcodec decode`, and asserts
  identical accept/reject and identical bytes. The court reports 1,600 body mutants. Counts: mode 9 2,000; mode 10
  2,002 (627 shard-set roots, 627/627 matching); mode 11 2,000; mode 12 2,000;
  mode 13 2,001; mode 14 2,000; mode 15 2,000; mode 16 2,000. All 16,003
  frames were accepted by both and byte-identical. A further **1,600 body
  mutants** (CRC repaired so they reach semantic checks) agree on
  accept/reject in 1,600/1,600 cases and on error class in 1,581/1,581
  meaningful reject pairs; 19 mutants remain valid and decode identically.
- **Python test receipt.** `bench/receipts/python-bindings-20261106T072822Z.json`
  records **331 tests, 0 failures, 0 errors, 9 skips** (the skips are the
  superseded historical `017_unknown_mode_reserved*` vectors). The receipt records 331 tests, 0 failures, 0 errors, 9 skips. Wheel
  `weftcodec-0.8.0-py3-none-any.whl`, 66,163 bytes, sha256
  `39898a4a0d206d12528f32f6d0282f7e261f4bc8a5105eb127d701854f12c6ba`.
  Court image `weft/python-court:1` id
  `sha256:af751fc866908fb6e6abb190c6e139739b62246ea019447a6a754e5c3b2a849e`
  (3 GiB / 4 CPUs / 1,024 pids).
- **Court image detail.** `docker/python-court.Dockerfile` is now pinned to
  `python:3.12-slim-bookworm` (the floating `python:3.12-slim` moved to
  trixie, where `libfuse3-3` no longer exists) and installs `libfuse3-3` so the
  fuse-linked `weftfs` CLI used by the existing DCP tests runs in this court.
  No `/dev/fuse` is passed to the Python court.
- **Honest residual.** The Python reader still does not claim WeftQL selectors,
  Context Roots, capability envelopes, `weft.checkpoint.v1` manifests, WFA1
  archives, or `weft.tensor.layout.v1` / `weft.tensor.shard.v1` records;
  TENSOR_TILED supports full decode but not the Python region reader. The
  randomized differential is one host with synthetic frames (Rust CLI is the
  oracle); overflow and absurd-limit vectors are covered by the curated
  conformance corpus, not by the generator. No Go code, no wire change, and no
  manifest regeneration was performed.

## Subphase R7 — shared multi-mount daemon (2026-11-06)

One process can serve N read-only context mounts over one store, sharing the
store state, selector index cache, and backend handle instead of paying a
process, a thread pool, and a replayed snapshot per mount.

- **Architecture.** `src/fuse.rs` now reaches the store through `StoreHandle`:
  `Exclusive(Arc<Mutex<Store>>)` for a single mount (unchanged behaviour) or
  `Shared(Arc<Store>)` for shared views. A shared view borrows `&Store`
  immutably and can never obtain `&mut Store`, so create/write/truncate/
  setattr/xattr/rename/link/unlink/mkdir/mknod/symlink/fallocate/
  copy_file_range are refused with `ReadOnly` → `EROFS`, the same class a
  single `--read-only` mount uses. Pins are skipped for shared views (they need
  `&mut Store`, and this process runs no writer or GC).
- **Memory.** `MountOptions.snapshot_state` is `Option<Arc<State>>`, and the
  CLI replays each snapshot at most once per server start (`cached_state`), so
  N views pinned to one commit hold one `State`, not N clones.
- **API.** `SharedViewSpec`, `SharedMount` (`start`, `mountpoints`,
  `store`, `is_running`, `any_running`, `unmount`) and `run_shared` in
  `weftfs::fuse`. One `fuser` session thread per view plus the supervisor;
  `MAX_SHARED_VIEWS = 256`, refused with `LimitExceeded` before mounting.
  Starting is all-or-nothing and SIGINT/SIGTERM unmounts every view.
- **CLI.** `weftfs --store DIR mount-many --spec FILE [--allow-other]`. The
  spec is strict JSON (`version: 1`, `views[]` with `mountpoint` and optional
  `snapshot` | `context` | `at`, plus `capability`); unknown fields, duplicate
  mountpoints, and unknown versions are rejected. The single-mount CLI is
  unchanged. Documented in `docs/shared-mounts.md`.
- **Demo.** `scripts/demo-context-mounts.sh` accepts
  `WEFT_CONTEXT_MOUNTS_MODE=shared` and starts one process for all N views;
  the per-daemon mode and its receipts are unchanged. New knobs:
  `WEFT_CONTEXT_CORPUS_BUILD=cli|harness`, `WEFT_DEMO_SKIP_BUILD`,
  `WEFT_DEMO_BUILD_ONLY`, `WEFT_WAIT_MOUNT_TRIES`; new metrics
  `mount_mode`, `mount_processes`, `all_mounted_ms`,
  `mount_latency_min_ms`, `verified_files_sha256`, and
  `listing_digests_sha256` (a digest over every per-view listing digest, so
  two runs can be compared from receipts alone).
- **Court.** `bash bench/scripts/shared_mounts_court.sh 100` runs the same
  oracle-built corpus four times: per-daemon and shared at 3 GiB, then both at
  a 512 MiB limit. It requires identical listing digests, identical file
  SHA-256 counts, identical verified mount counts, a clean shared run at
  512 MiB with zero OOM kills, and a per-daemon run at the same limit that
  does not complete. Receipt
  `bench/receipts/shared-mounts-20261106T082042Z.json`, 19/19 checks pass;
  analysis `bench/RESULTS-shared-mounts.md`.
- **Measured at N=100** (same corpus, same verification, binaries built once):
  per-daemon 100 processes / 692 threads / summed RSS 1,963,056 KiB /
  cgroup peak 1,351,827,456 B / 100 mounts verified / 425 file hashes;
  shared 1 process / 102 threads / RSS 27,692 KiB / cgroup peak
  69,402,624 B / 100 mounts verified / 425 file hashes. The digest over all
  100 per-view listing digests is identical
  (`1d30af9ccebcab723acfdc0cb9ceee56a6fe93a73c8be0220590593eb714566e`) in
  per-daemon at 3 GiB, shared at 3 GiB, and shared at 512 MiB. cgroup peak is
  19.5x smaller, summed RSS 70.9x smaller, threads 6.8x fewer.
- **Small limit.** The same N completed in shared mode at 512 MiB with peak
  73,646,080 B (70.2 MiB), i.e. 441.8 MiB of headroom. Per-daemon at the same
  512 MiB mounted 51/100 and lost 49 daemons to cgroup OOM kills
  (49 `oom_kill` events, peak pinned at the limit).
- **Tests.** `tests/shared_mounts.rs` 8/8: eight shared views match eight
  equivalent single mounts (listing digest and every file byte), write-like
  operations fail with `EROFS` and change nothing, a root-scoped capability
  view can read its granted exact id but gets `ENOENT` for an unauthorized one
  while an unscoped sibling can read it, unmounting one view leaves the others
  serving and the supervisor exits after the last, one session thread per view
  with an oversized spec refused `LimitExceeded`, no panic escapes a view
  under invalid operations, and the spec schema rejects unknown fields,
  duplicates, and a writable field. Full
  `cargo test --features fuse --all-targets`: **307 passed / 0 failed /
  2 ignored** (the two pre-existing ignored tests).
- **Losses.** Time to the *first* view is 3,964 ms vs 523 ms per-daemon (one
  process resolves all contexts and creates all mounts sequentially); time to
  all 100 is 4,064 ms vs 4,428 ms, so completion is not worse. One backend
  mutex serialises physical I/O across views (not measured under concurrent
  large reads), and one process is one failure domain. Shared views are
  read-only by construction; writes stay on the single-mount path.
- **Not done.** No Go frontend parity, no per-view streaming start (resolve
  and mount one at a time), no shared-mount run at 1,000, no concurrent
  large-read comparison against independent daemons, no multi-host claim.
- **Verification.** Full `scripts/verify-all.sh` at the R7 docs tree
  (`a4e2c3d`): **27/27 pass**, receipt
  `bench/receipts/verify-20261106T083428Z.json` (WeftFS `a4e2c3d`, WeftCodec
  `72cd119`, WeftCodec-Go `e47fa03`, WeftFS-Go `47fd4d2`). The first run at
  the same tree, `verify-20261106T082853Z.json`, failed only `rust_store`:
  `tests/representation.rs::codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks`
  observed 5 decoded chunks instead of 1 because that test reads a
  process-global codec decode counter while other test binaries' tests run in
  parallel; it passes with `--test-threads=1` and in the full rerun. The
  failing receipt is retained. `check-claims.sh` 258/258 OK;
  `check-spec-drift.sh` 42/42 OK (no spec change).

## Subphase R8 — reconstruction single-flight and dependency-aware cache (2026-11-06)

Code in `src/singleflight.rs`, `src/object_cache.rs`, the read path in
`src/store.rs`, and the CLI/FUSE wiring; court harness
`examples/cache_bench.rs` + `bench/scripts/cache_court.sh`; analysis
`bench/RESULTS-cache.md`.

### Flake fix (committed first, `tests: make the codec chunk-decode counter
per-thread`)

`codec_chunk_decode_count` was one process-global atomic. libtest runs the
tests of one binary on a thread pool, so an unrelated test that decoded chunks
on another thread inflated
`representation::codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks`
(the retained `verify-20261106T082853Z.json` observed 5 chunks instead of 1).
The observation is now **thread-local** — exactly the work the calling test
performed, because representation reads decode on the calling thread — and the
process-wide aggregate remains available as `codec_chunk_decode_total`. The
assertion is unchanged. New regression test
`chunk_decode_counter_observes_only_the_calling_thread` decodes four chunks on
another thread, waits for it to finish, and proves this thread still observes
0 and then exactly 1. Verified with **20 consecutive parallel runs** of
`cargo test --release --test representation -- --test-threads=8`: 9/9 pass each.

### Single-flight (implemented)

`SingleFlight<K, V>` is a bounded `Mutex` + `Condvar` registry (no `unsafe`).
Identity is `(object id, content/representation version, region, verified)`;
a whole read and a range read covering the whole object coalesce. One leader
runs the read+decode; identical concurrent callers wait and receive a clone of
the leader's result or its exact error. A panicking operation is converted to
`Corrupt` so waiters can never hang. Cancellation is a waiter's decision only
(`CancelToken`, polled every 20 ms, new `Class::Cancelled`): the leader always
finishes for the others, and a token already cancelled before leadership
declines to start. `max_entries` (default 64) and `max_bytes` (default 256 MiB)
are hard admission bounds; a request that does not fit runs independently and
increments `degraded` — it never queues and never deadlocks. Off by default.

### Decoded-object cache (implemented)

`ObjectCache` holds the **verified decoded bytes**, keyed by
`(object id, content version)`: a new version has a new content root and can
never hit an old entry (an admission also removes the old version's entry).
Admission/eviction score is
`decode_cost_ns(class, decoded_bytes) * dependent_count * observed_read_frequency`
from the existing measured cost table, the `derived_from` relation graph, and
the existing advisory access counters; ordering is exact integer
cross-multiplication of cost per byte, then lower score, then larger bytes, then
lower content/object id. Shared bases with many dependents outrank one-off
large tensors. `pin_cache_object` makes an entry unevictable; if pins fill the
bound, unpinned admission is refused (the bound is never exceeded). Sized by
`--cache-bytes` (default 0 = disabled) plus a `--cache-entries` bookkeeping
bound. Counters: hits, misses, admissions, evictions, stale removals,
rejections, bytes served, pinned skips. In process memory only; absent from
every frozen format and from commits/verify/GC.

### Authorization

The cache and single-flight are only reachable through an `AuthorizedRead`
token produced by `Store::authorize_read` / `authorize_read_at` /
`authorize_read_in` (POSIX mode + `weft.acl.read` + capability-envelope
validity). `read_object_authorized`, `read_range_authorized`,
`read_content_authorized` (plus `*_cancellable`) take the token, and dependency
reconstructions inherit it. A denial is `Unauthorized` and never consults the
cache; `read_content_authorized` refuses a token for a different content root.
Internal reads (`verify`, planning, rebase, GC, recovery) keep the shipped
uncached path. FUSE gates the layers with the request's uid/gid, and only when
a layer is enabled (default off = unchanged behaviour).

### Evidence

Receipt `bench/receipts/cache-20261106T091322Z.json` (**26/26 checks**, at
WeftFS `dbeaf0a`) in the bench court on the real Qwen2.5-0.5B/Instruct pair
(both files re-hashed), 26 objects / 226,623,488 logical bytes per round, 4
rounds, 256 MiB cache: `none` 1 thread 160 decodes / 754,432,204 B read /
p50 61.17 ms / 6,440.7 ms; `singleflight` 4 threads 160 decodes /
754,570,204 B / 6,384.4 ms (vs `none` 4 threads 640 decodes / 3,017,728,816 B
/ 6,645.9 ms); `cache` 1 thread 26 decodes / 107,411,220 B / p50 0.224 ms /
1,121.9 ms; `both` 4 threads 26 decodes / 107,549,220 B / 1,140.6 ms. Every run: all bytes hash-equal and
`verify --full` ok. Adversarial 5/5: waiter-only cancellation, error fan-out,
disabled cache, pinned base retained, unpinned base evicted. Losses recorded in
`bench/RESULTS-cache.md`: single-flight is not a wall-time win on 4 otherwise
idle vCPUs (work −75 %, wall −3 %), single-flight at 1 thread is neutral,
cache-only still races on the first round (104 vs 26 decodes), the cache costs
+233 MiB RSS at 1 thread for a 226 MB working set, a bound below the working set
re-decodes, p99 stays bimodal, and the cache is per process.

Tests: `tests/singleflight.rs` 9/9, `tests/object_cache.rs` 10/10, unit tests
in both modules, `tests/representation.rs` 9/9. `cargo test --release
--all-targets`: **299 passed / 0 failed** (75 binaries). `cargo test --release
--features fuse --all-targets`: **341 passed / 0 failed / 2 ignored** (the two
pre-existing ignored tests). `check-claims.sh` and `check-spec-drift.sh` pass;
no normative spec changed (R8 is read-path only, no wire/format change).
Not done: no Go parity, no mode-15 whole-object caching, no FUSE range caching,
no persistence across processes, no multi-host claim.

## Subphase R9 — immutable write overlays and compaction (experimental, Rust write path)

`weft.manifest.v4` (`spec/WEFT-MODEL.md` §2.3.3) adds `kind:"overlay"`: an
immutable base manifest plus an ordered list of immutable extents. A v4
manifest has no top-level chunk table; the canonical table is derived from the
base chain, and `content_root` is the Merkle root a canonical single-table v3
manifest would produce over the same bytes at the same chunk size. The reader
accepts chains up to 256 manifests, at most 64 extents per manifest, requires
extents to be chunk-aligned, sorted, non-overlapping within a layer, and exact
in count/length/sum, and fails closed (`Malformed`, `Corrupt`, `LimitExceeded`,
`UnsupportedVersion`) on every violated rule. v1/v2/v3 encodings are
byte-identical (the frozen fixture tests are unchanged).

Read path: the resolver reads manifest blobs only, validates every base
cross-check (`base_content_root`, `base_size`, `chunk_size`, Merkle identity,
depth accounting, `overlay_bytes`), merges layers newest-wins, and checks the
resolved table's root before any content chunk is fetched. Range reads fetch
only intersecting chunks; tests prove this with the per-thread overlay chunk
counter and the backend byte counter. GC marks overlay extents and the base
chain, so bases and chunks reachable only through an overlay are retained and
unreferenced overlay chunks are collectable. Shallow `verify` resolves the
chain; full `verify` decodes every logical chunk and reports the root.

Write path (stage 4): `RepresentationMode::Overlay` and
`--representation overlay` publish one new manifest version per range write —
the previous version as an immutable base plus one extent. Only chunks the edit
touches are read and, when their bytes change, stored. A misaligned edit is
widened to the enclosing chunk grid and the untouched bytes of the widened
chunks come from the previous version, so the logical content is exact.
Adjacent or overlapping extents fold into the current layer (up to 64 extents
per manifest), so chain depth grows per 64 disjoint extents rather than per
write. Before publishing, the writer derives the canonical table twice (from
the previous table plus the edit, and from the base manifest plus the new
extent list) and refuses with `Corrupt` unless they are identical, so the
manifest a reader resolves is exactly the table the writer priced. Bounds:
`Options::max_overlay_depth` (default 64; the hard reader bound is 256) and
`Options::max_overlay_bytes` (default 1 GiB) refuse with `LimitExceeded` naming
`weftfs compact`. Publication uses the unchanged commit protocol, so old
versions and their chunks stay valid for snapshots. `--representation overlay`
selects the Merkle identity when no identity is named; an explicit
`--content-identity whole-file` is refused rather than upgraded.

Whole-file helpers never silently materialise an overlay object: `zero_range`
(both modes) and a growing `truncate` publish an overlay layer (a full zero
chunk is content-addressed once, so memory stays at one chunk); `truncate` to
the same size is a no-op; a shrinking `truncate`, `collapse_range`, and
`insert_range` return the new `UnsupportedRepresentation` class because an
immutable extent can replace or append chunks but cannot shorten or shift the
tail. `append_file_streaming` keeps refusing overlay objects; a range write at
EOF is the supported append.

Compaction (stage 5): `Store::compact(id)` re-reads and re-hashes every logical
chunk in order (one chunk of memory), recomputes the Merkle root, and refuses
to publish unless it equals the pre-compaction root. It then encodes one
canonical `weft.manifest.v3` table, decodes it back, checks chunks/size/root
again, and publishes through the normal commit protocol. No content blob is
rewritten: every chunk is content-addressed and was just verified. A
non-overlay object is refused with `UnsupportedRepresentation`. CLI:
`weftfs compact <path>` (`--json` reports `content_root`, `chunks`,
`overlay_depth`, `overlay_bytes`, `verified_bytes`).

Court (stage 6): `bench/scripts/overlay_court.sh` runs the same seeded workload
through both writers in separate child processes — one 128 MiB object with
64 KiB chunks and 500 4 KiB overwrites at 4 KiB-aligned random offsets — plus a
4 KiB range read (warm and through a freshly opened handle), a full read,
`verify --full`, compaction, GC, and six checks. It runs over the local backend
and, through `scripts/s3_court.sh --overlay`, over single-node MinIO.

| metric (local / MinIO) | chunk COW | overlay |
|---|---|---|
| 500 writes, wall | 917.3 / 2,308.3 ms | **415.5 / 1,702.9 ms** |
| write p50 / p99 (local) | 1.819 / 1.912 ms | 0.717 / 1.698 ms |
| backend bytes written | 87,403,237 / 89,451,237 | **36,731,767 / 38,779,767** |
| backend requests (writes) | 3,501 | 3,993 |
| 4 KiB range read, warm p50 | 0.919 / 1.422 ms | **0.030 / 0.414 ms** |
| 4 KiB range read, cold first | 0.920 / 1.502 ms, 2 req | **5.594 / 7.557 ms, 10 req** |
| full read before compaction | 40.6 / 439.3 ms | 39.9 / 437.8 ms |
| `verify --full` before | 53.5 / 1,773.0 ms, ok | 52.0 / 782.5 ms, ok |
| compaction | refused (`UnsupportedRepresentation`) | 34.0 / 444.1 ms, root unchanged |
| `verify --full` after | 53.2 / 1,759.5 ms, ok | 52.9 / 1,879.8 ms, ok |
| retained blob bytes before / after compact / after GC | 134,322,319 each | 166,480,149 / 134,322,319 / 134,322,319 |
| GC after compaction | 775 blobs, 70,338,845 B | 501 blobs, 2,289,555 B |
| on-disk bytes after GC | 149,348,604 | 166,831,597 (about 32.5 MB is the snapshot-pinned old chain) |
| peak RSS per child | 402,348 / 403,964 KiB | 411,248 / 412,348 KiB |

The content root after the writes is identical in all four runs
(`c00367f0fd99b612a7800b863c66c5c959fcdbe42bafaa36b747d1f8ff164c22`), and so is
the full-read digest. Compaction changes no byte and no root. A snapshot taken
before compaction reads byte-identically after compaction and after GC, so the
old chain is retained exactly as long as the snapshot exists.

Losses, recorded in `bench/RESULTS-overlay.md`: a cold range read is 6.1x
(local) / 5.0x (MinIO) slower because a new process resolves the depth-8 chain
(10 requests / 215,317 B instead of 2 / 170,127 B); extent coalescing widens
extents so `overlay_bytes` reached 30.6 MiB for 2.0 MiB of logical writes (the
accounting metric the byte bound limits, not extra I/O); full reads are not
cheaper; a snapshot pins the old chain, so the overlay store keeps 17.5 MB more
on disk after compaction + GC in this court; compaction is O(file size); the
overlay child peaks 8.9 MB higher; S3 `verify --full` timings are noisy; and
this is not a durability, media, power-loss, cloud, or fleet claim.

Go reads the same v4 objects: `model.ManifestFromWCB` validates the exact key
set and grid, and the store resolves the chain, verifies every chunk/root, adds
overlay retention to GC and quota accounting, and refuses range writes to an
overlay object with `InvalidArgument`. **At this subphase Go had no v4 writer
and no compaction**; R20 adds both, with byte-identical courts. The committed fixture
`conformance/overlay/v1` (also mirrored at `WeftFS-Go/conformance/overlay/v1`)
is generated by `examples/gen_overlay_fixture.rs` and read by both
`tests/overlay_fixture.rs` and `WeftFS-Go/store/overlay_handoff_test.go`.

Evidence (all in the Docker courts):

- `bash scripts/docker-court.sh rust "cd /work && cargo test --release --all-targets --no-fail-fast"`
  -> **336 passed / 0 failed**, including `overlay_format` 6/6,
  `overlay_read` 9/9, `overlay_fixture` 1/1, `overlay_write` 13/13, and
  `overlay_compact` 8/8.
- `bash scripts/docker-court.sh fuse "cd /work && cargo test --release --features fuse --all-targets"`
  -> **378 passed / 0 failed / 2 ignored** (the two pre-existing ignored tests).
- `bash scripts/docker-court.sh go "cd /work/WeftFS-Go && go test -race -count=1 ./store/..."`
  -> **ok**.
- `bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/overlay_court.sh --local"`
  -> **6/6 checks**, receipt `bench/receipts/overlay-20261106T103115Z.json`.
- `bash scripts/s3_court.sh --overlay` -> **6/6 checks**, receipt
  `bench/receipts/overlay-s3-20261106T103120Z.json`, court receipt
  `bench/receipts/s3-court-20261106T103120Z.json`.
- `scripts/check-spec-drift.sh` 42/42; `scripts/check-claims.sh` all entries.

Stage commits: `fa37197` (format + spec mirrors), `eaad6eb` (read path),
`4fc75fa` (Go read + fixture), `ca34643` (Rust write path), `9776c5e`
(compaction + `weftfs compact`), `e9b9bae` (court harness + S3 flag), and the
docs/receipts commit. The writer-bound spec paragraph was exported to WeftCodec
`54cb4f6`, WeftCodec-Go `8957998`, WeftFS-Go `17cef27`.

Residual at R9 (write/compaction parity closed in R20): write and compaction
parity was Rust-only at this subphase; R20 adds both to Go. The truncate-shrink,
insert, and collapse paths refuse rather than rewrite; the writer bounds are
library options, not CLI flags; coalescing can inflate `overlay_bytes`; a
fresh-process read pays chain resolution; the court is one host with a warm
page cache and one synthetic workload; no physical power loss or fleet claim.

## Subphase R10 — blob and commit planes (2026-11-06)

`StorageBackend` used to mix immutable content bytes with POSIX-shaped control
state (`write_at` into the superblock, `sync_dir`, an exclusive lock, in-place
zero-fill). It worked for one local filesystem while blocking the next two
programme items: physical object packs need a non-directory metadata plane, and
distributed backends need a conditional-head publication contract rather than
an in-place 4 KiB write.

R10 splits the contract into `BlobPlane` (immutable bytes: `get`, `get_range`,
`put_if_absent`, `delete`, `head_part` probe, `list`) and `CommitPlane`
(versioned control state: `read_control`, `compare_exchange`,
`compare_exchange_head`, `load_head`, `probe`), keeps `StorageBackend` as the
store's one I/O adapter, and routes each `Key` to a plane through
`CompositeBackend`. `BlobPlane` and `CommitPlane` are implemented for
`LocalBackend`, `ObjectBackend`, and `S3Backend` by delegating to the existing
primitives, so the plane contract and the legacy contract cannot drift.

**No on-disk format change.** The key layout, commit-file bytes, two-slot 8 KiB
superblock, publication order, and every content identity are unchanged, and
all pre-existing tests pass unmodified.

Capabilities are **probed**, not declared. A writable open runs a blob probe
(create-once publication, exact length, a 17-byte ranged read, delete) and a
commit probe (create-once CAS, matching CAS, and a **stale** CAS that must be
rejected with `Busy`). A probe may only refute a declared capability, never
upgrade one; a declared-absent capability is skipped and refused where the
store needs it. Durability stays an attested property of the publication
primitive — no probe can prove media durability without power loss, and none is
claimed. Read-only opens never probe and never write.

The probe result is kept on the handle (`Store::plane_probes`) and is recorded
in the court receipt. A writable open now also refuses a backend without
conditional head publication (`conditional_head`), which is the contract a
distributed metadata engine must satisfy.

### Split configuration (opt-in; the default is unchanged)

```
weftfs --blob-backend s3    --commit-backend local --store DIR ...
weftfs --blob-backend local --commit-backend s3    --blob-root DIR --store DIR ...
weftfs --blob-backend local --commit-backend local --blob-root DIR --store DIR ...
```

Omitted flags mean "same as `--backend`", so the historical layout is the
default and is never silently changed. `BackendKind::split` refuses nesting.
`weftfs info --json` reports `"backend"` and a `"planes"` object with both
planes' capabilities. A split is refused before touching disk when the blob
plane lacks `atomic_publish`/`durable_fsync` (a half-written manifest or chunk
would break exactness) or the commit plane lacks
`atomic_publish`/`conditional_head`/`durable_fsync` (a torn or overwritten head
would make a commit ambiguous). The S3 commit plane is expressible because S3
already has `If-None-Match`/`If-Match`; the court runs it.

### Evidence

* `cargo test --release --all-targets` -> **350 passed / 0 failed** (83 test
  binaries `ok`), including `tests/planes.rs` 14/14.
* `cargo test --release --features fuse --all-targets` -> **392 passed / 0 failed / 2 ignored** (the two pre-existing `#[ignore]`s), 83 binaries `ok`.
* `bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/planes_court.sh"`
  -> 14/14 plane tests, receipt `bench/receipts/planes-20261106T110054Z.json`
  (local layout only; the S3 layouts are recorded as skipped with the reason).
* `bash scripts/s3_court.sh --planes` -> 14/14 plane tests and **4/4 layouts**
  with `exactness.match = true`, receipts
  `bench/receipts/planes-20261106T110154Z.json` and
  `bench/receipts/planes-20261106T110216Z.json` (independent repeat), court
  receipts `bench/receipts/s3-court-20261106T110154Z.json` /
  `...T110216Z.json`.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass (see the
  claim entries; no spec text changed, because no wire format changed).

All four layouts produced the identical content root
`7ebb1bdb425b86b04dfe7aa7018136b52648da0f00a148f9356eabdddb80a470`, generation
`145`, 6,291,456 verified bytes, and digest
`74d54de57fd438a492b7966810ca4a631cb7eb9cc23b7e20da8f4f54d1bf5b59`:
single local, single S3, S3-blobs/local-commits, and local-blobs/S3-commits.

Create / 96-write / full-read medians, run 1 (run 2 in parentheses):
local/local 5.90 (5.84) / 14.06 (14.21) / 3.24 (3.30) ms;
s3/s3 129.90 (131.52) / 286.44 (287.32) / 30.03 (29.99) ms;
s3-blobs/local-commits 98.38 (97.75) / 220.68 (221.37) / 31.31 (36.96) ms;
local-blobs/s3-commits 55.68 (57.30) / 117.49 (119.32) / 3.37 (3.44) ms.
Per-plane request counts and bytes, recoveries, peak RSS, and every loss are in
[`bench/RESULTS-planes.md`](../bench/RESULTS-planes.md); the contract and the
precise list of what an external metadata engine must implement are in
[`docs/PLANES.md`](PLANES.md).

### Crash proxy

`tests/planes.rs` kills the writer at `after_objects`, `after_commit_file`, and
`after_superblock_write` for a local split and for an S3-blobs/local-commits
split. Every iteration reopened to the old whole state (phases 1-2) or the new
whole state (phase 3), never partial; an unpublished newer commit file in the
early phases is the documented state, exactly as in the pre-existing
single-backend proxies. The split publishes the head with
`compare_exchange_head`, and a second live writer is refused with `Busy`
(`split_publication_uses_one_head_compare_and_swap`).

### Residual (unchanged by this subphase)

* At R10 split parity was Rust-only and WeftFS-Go kept the single backend
  (R20 adds the Go split planes, R25 the Go S3 plane, with cross-language
  courts).
* The commit plane is single-writer; there is no distributed coordination or
  fleet claim. The single writer is enforced by `flock` locally and by a fenced
  lease with a conditional compare-and-swap on object stores. An external
  metadata engine is specified but not implemented.
* Process-fault crash proxy only; no physical power loss, host cache loss, or
  media error injection.
* One host, warm caches, synthetic 6 MiB fixture; MinIO is a single node with a
  tmpfs data directory. Absolute times are court-bound.
* `weftfs mount` accepts the split flags but the court exercises the store and
  CLI paths, not a mounted split filesystem.

## Subphase R11 — physical object packs (2026-11-06)

A content-addressed namespace costs one physical request per small blob:
10,000 one-kilobyte chunks were 10,000 `PUT`s and 10,000 `GET`s. The R10 commit
plane makes a real pack catalog possible.

`src/packs.rs` adds `PackedBlobPlane`, a `BlobPlane` over an inner blob plane and
a `CommitPlane`-backed catalog:

* members at or below `member_threshold_bytes` are buffered and sealed into a
  content-addressed pack (`packs/<ab>/<hex>`) when `pack_target_bytes` or
  `max_members_per_pack` is reached; larger blobs stay flat;
* membership is `key -> (pack, offset, length, digest)` in
  `catalog_segments` immutable catalog blobs (`pack-catalog/<ab>/<hex>`), named
  by one mutable `pack-catalog.root` control record replaced with a
  compare-and-swap, so a reader sees the old or the new whole catalog;
* the pack is published **before** the catalog names it: a crash between them
  leaves an unreferenced pack (reclaimed by GC), never a dangling entry;
* a read is one catalog lookup then one `get_range` for exactly the member's
  bytes; whole-member reads verify the catalog digest and fail closed
  (`Corrupt`) on a mismatch, a short read, or a truncated pack;
* `Store::publish_commit` calls the new `flush_buffered_blobs` before writing the
  commit record, so a commit never names a non-durable blob;
* `gc --apply` deletes candidates in one batch (`delete_many`) and then reclaims
  packs no catalog entry names; a pack with one live member survives;
* when the catalog bound (`max_catalog_bytes`) is reached, packing stops and new
  blobs are published flat;
* a plane without `true_range_reads` is refused at construction
  (`UnsupportedVersion`).

New key namespaces are `packs/`, `pack-catalog/`, and `pack-catalog.root`; the
existing logical keys, manifest bytes, commit bytes, and superblock are
unchanged. `--pack`, `--pack-target-bytes`, `--pack-member-threshold`,
`--pack-catalog-bytes`, and `--pack-segments` select the policy.

### Bounds chosen

Pack target 16 MiB, member threshold 64 KiB, 65,536 members per pack, 8 MiB
catalog bound, 256 catalog segments (each catalog entry is 113 bytes; the bound
admits 74,288 entries). `PackOptions::validate` refuses a threshold above the
pack target, a non-power-of-two segment count, and a catalog smaller than one
entry per segment.

### Evidence

* `cargo test --all-targets` -> **369 passed / 0 failed**; the new
  `tests/packs.rs` is 13/13: byte-identical whole and ranged reads, a narrow
  range asserted with the backend byte counter, digest mismatch, truncated
  pack, missing catalog and missing segment, the two-phase pack crash proxy,
  GC reclamation (one live member keeps the pack; all-unreferenced collects it),
  the catalog-bound flat fallback, deterministic layout, and a store-level
  round trip/verify/GC on a packed plane.
* `cargo test --features fuse --all-targets` -> **411 passed / 0 failed / 2
  ignored** (the two pre-existing ignores).
* `bash scripts/s3_court.sh --packs` -> four configurations, exactness
  `match = true`: same generation `2`, snapshot root
  `57c24012a9483db06f5c6fb3ca9b8860430492276e48e64cdbb21df034c37975`, read
  digest `a8812b26c57dc49c2371b6d52fb2b6063e21e84023eef360fb9b772695871b99`,
  zero verify problems. Receipt `bench/receipts/packs-20261106T113605Z.json`,
  MinIO provenance `bench/receipts/s3-court-20261106T113605Z.json`, log and test
  summary beside them. Revision `8b42103`.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass (no spec text
  changed; no wire format changed).

### Measured result (see `bench/RESULTS-packs.md`)

10,000 x 1 KiB files in one group commit: object requests **20,003 -> 262**
(local) and **20,003 -> 261** (S3), wall **298.3 -> 185.4 ms** (local) and
**7,321.5 -> 2,791.7 ms** (S3). Losses: written bytes **+13.8%**
(18,809,103 vs 16,524,362/16,528,458), physical footprint **+19.2%**
(14,112,521 vs 11,840,000), cold 1,000-read pass **1.25x requests / 3.19x
bytes**, S3 warm read p50 **0.3437 vs 0.1685-0.1729 ms**, S3 `verify --full`
**23,103.2 vs 16,202.3 ms (+42.6%)**, peak RSS **+6,036 KiB (local) / +2,228
KiB (S3)**. A 4 KiB range read inside a 16,777,216-byte pack costs exactly **1
request and 4,096 bytes**.

### Residual

* Rust only at this subphase; R20 adds the Go pack plane and cross-language
  pack courts. Packs are immutable and never rewritten; dead members occupy
  pack space until the whole pack is unreferenced (no pack compaction).
* One catalogue root per store, 256 segments; reclamation scans the catalog.
* Process-fault crash proxy only; no physical power loss or media errors.
* Single host, synthetic fixture, MinIO single node with a tmpfs data
  directory; absolute times are court-bound.
* A range read cannot verify the member digest (it is a partial read); only
  whole-object reads are digest-checked, matching the flat plane.

## Subphase R12 — rebase parity and host calibration (2026-11-06)

`weftfs rebase` existed in Rust with provenance receipts and in WeftFS-Go only as
a store API. At the same time the planner's `measured` decode table was a
constant baked into the crate: it was calibrated in this bench court, so a host
with different decode throughput had no supported way to use its own numbers.
R12 closes both gaps. No wire format changed; manifests, commits, vectors, and
the superblock are unchanged.

### Host-local calibration

The measurement code that lived in `examples/calibrate_decode.rs` now lives in
`src/calibration.rs`; the example is a thin wrapper over the library, so the CLI
verb and the bench harness cannot drift. A calibration document uses the
schema `weft.calibration.v1` and carries the created time, the host/kernel/image
identity, the measurement method, exactly ten class rows (`fixed_ns`,
`ns_per_mib`, `samples`, `origin`), and a digest
`H("weft/decode-calibration/v1", compact JSON without the digest field)`.

The loader is strict and fails before the planner sees a row:

* wrong schema, unknown or missing keys, a non-object, or a file above 1 MiB;
* missing, duplicated, or unknown cost classes (completeness is ten rows);
* an unknown `origin`, or a `measured` row with fewer than 5 samples;
* `fixed_ns > 60 s`, `ns_per_mib == 0`, or `ns_per_mib > 100 s/MiB`;
* any row that is not strictly monotonic between 1 MiB and 64 MiB;
* a digest mismatch.

There is no partial parse, no silent fallback to the baked table, and no merge of host
and baked rows. `weftfs calibrate --out FILE` measures, writes the record, and
prints the per-class divergence against the baked `measured` table;
`weftfs calibrate --from FILE --json` reports divergence for an existing file;
`weftfs calibrate --from FILE --install --store DIR` copies it to the advisory
`<store>/calibrations/<id>.json` (ignored by replay, verify, and GC, like
`indexes/`). `--planner-cost-model measured --calibration FILE` uses the file's
rows for the tensor planner and `rebase`; with `declared` it is refused with the
exact error `--calibration requires --planner-cost-model measured`. Without the
flag, behaviour is byte-identical to R11.

In the final scale court the host table differed from the baked table by
+1.962% (float-tensor) to +8.720% (semantic-delta), and -2.541% for
tensor-sharded, measured at 1 MiB. The table is host-bound by construction; the
record names the exact image, kernel, CPU count, and WeftFS revision.

`tests/calibration.rs` is 8/8: round trip through a file, digest over the
canonical document (whitespace-insensitive), 18 malformed/incomplete/out-of-
range rejection cases, oversized-file refusal, planner equality with the file's
rows, default behaviour unchanged, rebase chain pricing with the file, and an
advisory install that replay/verify ignore. Unit tests in `src/cost_model.rs`
pin the baked tables.

### Go rebase CLI parity

`weftfs rebase` now exists in WeftFS-Go with the same flags and decision rule as
Rust: `--root`, `--threshold`, `--dry-run`, `--strategy`, `--lambda-ns`, and
`--max-objects`. It opens read-only for a dry run, writes a
`weft.provenance.v1` receipt on apply, and emits the same JSON field names. A
dependency strategy (`weight-delta`, `sparse-delta`, `semantic-delta`) is
refused with an explicit message in both languages; `--strategy` is for the
standalone target.

Cross-language tests run in both directions:

* Go applies a rebase; Rust `verify --full` reports ok, `provenance show`
  returns the Go receipt, and the Go read equals the original bytes with the
  same logical root.
* Rust applies a rebase; Go `Verify(true)` reports ok, `ReadTensor` equals the
  original bytes, the logical root is unchanged, the `before-rebase` snapshot is
  byte-identical, and the provenance relation is present in Go state.

`cmd/weftfs/rebase_test.go` also proves a dry run writes nothing (generation,
head, version, and counters unchanged), a high threshold selects nothing, the
second apply after a reopen is a no-op, and an unsupported strategy is refused.
`TestRustRebaseReadableByGo` is the Rust-to-Go direction.

The new reopen test exposed a real bug in both implementations: `flush`
early-returned when the pending map was empty, so a **pure consumption** (the
rebase counter reset, which writes nothing else) was never persisted. A reopened
store then resurrected the count and a second rebase re-selected an object that
was already rebased. Both languages now keep a zero-delta pending entry so the
consumption publishes a compacted snapshot; `src/access.rs` and
`WeftFS-Go/store/access_consume_test.go` pin the behaviour across a reopen.

### Scale court (`bench/scripts/rebase_scale_court.sh`)

The court runs the cached Qwen2.5-0.5B / Instruct pair in the bench court: both
files are re-hashed against the LFS SHA-256, a release calibration is measured
and used for every planner call, the family is ingested (241 instruct tensors
are weight deltas), the pre-rebase state is snapshotted, 16 tensors are read 32
times and a different 16 are read twice, and per-tensor reads plus a full
checkpoint restore are timed before and after. A second dry run at threshold=1
provides the counterfactual economics for every read tensor without writing.

With a 200 ms threshold, the policy selected 16 of 32 read tensors, exactly the
predicted set (0 mismatches, 291 objects scanned). Selected per-tensor decode
went from `4501.537 ms -> 1900.812 ms` (-57.8%) and the full instruct restore
from 10,938.771 ms to 8,430.865 ms (-22.9%). The warm 16 (two reads, ~174 ms of
decode each) stayed deltas. Representation bytes for the 16 selected tensors
grew by 43575360 bytes (~19.4%) and the current-state representation total by
43575360 bytes; the store tree grew 277,231,717 bytes because the snapshot and
commit history retain the old delta frames and the advisory indexes grew
9,555,328 bytes. All 13 hard checks pass: `verify --full`, every BLAKE3 hash,
every recomputed logical root, the unchanged `before-rebase` snapshot, the
second apply writing nothing, the provenance read-back, and exact source
reassembly (`source_reassembly_exact`, SHA-256
`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`).

The economics are reported, not flattened: the measured chain cost is ~87.2
ms/read for an 8.7 MB BF16 MLP tensor, so the policy break-even is 3 reads; the
embedding costs ~2.724 s/read, so its break-even is 1. The bytes/ns trade would
have paid back after one read for the two-read tensors, but the fixed 200 ms
policy threshold is the binding constraint. That is recorded as
`policy_more_conservative_than_storage`, alongside the physical growth per
selected tensor. Receipt
`bench/receipts/rebase-scale-20261106T130054Z.json`; every table, method, and
loss is in `bench/RESULTS-rebase-scale.md`.

### Evidence (this subphase; same court limits)

* `cargo test --all-targets` (rust court): **380 passed / 0 failed / 0 ignored**.
* `cargo test --features fuse --all-targets` (fuse court): **422 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `go test -race -count=1 ./store/... ./cmd/weftfs/...`: **125 passed / 0 failed / 7 skipped**;
  the skips are the two pre-existing child-process guards and the five FUSE-mount
  handoff tests that need a `--features fuse` Rust oracle and `/dev/fuse`, which
  this court does not have. `go vet ./...` is clean and `CGO_ENABLED=0 go build
  ./...` succeeds.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* The calibration is a host artifact. It is not portable, not distributed by the
  store, and not signed; an operator copies it. Go's planner and rebase CLI still
  use the baked table (there is no Go `--calibration` in R12).
* The scale court is one host, one process at a time, warm page cache, cached
  public weights, and a synthetic calibration workload. Absolute times are
  court-bound. No physical power loss, host cache loss, fleet, or concurrent
  reader claim.
* A rebase is a new version. The before-rebase snapshot and the commit history
  retain the old frames, so the on-disk tree grows more than the current-state
  representation and no GC was run in the court.
* Only 32 of 290 instruct tensors were read in the scale court; the 41 unread
  rebaseable deltas are untouched. The court does not claim the full family was
  rebased.
* Go rebase parity is tested at the store and CLI levels; the scale court itself
  is Rust-only.

## Subphase R13 — distributed correctness court (2026-11-06)

The S3 backend, the blob/commit plane split, conditional head publication with
a probed stale-CAS refusal, GC, capabilities, and snapshots each had a court,
but none of them drove **separate writer/reader/GC processes** through the
distributed contract under failure. R13 adds that court before any further
performance claim. No wire format, manifest, commit, superblock, or vector
changed.

### The court

`bench/scripts/distributed_court.sh` runs in the bench court. It starts the
verified MinIO `RELEASE.2025-04-22T22-12-26Z` binary
(sha256 `53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`)
itself on loopback so it can stop and restart the server process. The harness
(`examples/distributed_court.rs`) drives real processes and records, for every
scenario, the exact command line, PID, exit code or signal, observed
generations, verification result, and raw log path:

* **S1** single-writer interlock: 20 alternating pairs; every second open is
  refused `Busy`; a third process opens after the first exits and the store
  verifies (64/64 checks).
* **S2** atomic publication under a process fault at `after_objects`,
  `after_commit_file`, and `after_superblock_write` (15 runs): 10 old, 5 new,
  **0 mixed/partial**; the 5 pre-head tails are reported by `verify`, never
  hidden (34/34).
* **S3** at-least-once retry: after `after_objects` the retried commit is
  **byte-identical** to the uninterrupted head (`32a0ba3d…9431`); after
  `after_commit_file` the visible state is identical and one entry per path
  exists, with lineage `[3, 1]` and the abandoned generation never reused;
  after publication the retry is refused `Exists` and writes nothing (21/21).
* **S4** 8 reader processes keep the exact pinned bytes across 10 commits
  (64/64 reads at pinned generation 3, published 4–13) (19/19).
* **S5** an open reader keeps its generation and bytes after the writer
  publishes; only a reopen observes generation 3 (4 stale + 4 fresh reads)
  (4/4).
* **S6** an endpoint with no listener fails at open with `Io`; stopping the
  live MinIO process makes a read fail with `Io` and emit **0** bytes;
  restarting on the same data verifies and serves the same pinned bytes
  (2 starts, 1 stop) (9/9).
* **S7** `gc --apply` racing a reader pinned to a snapshot-only object:
  48 blobs / 16,435 bytes reclaimed; the snapshot-only object, the live file,
  the snapshot state, the checkpoint, and the provenance receipt all still
  read exactly; `verify --full` is clean (8/8).
* **S8** knowing a `ContentId` is not authority under a signed capability
  envelope: the denial is `NotFound: no such object`, byte-identical to an
  absent hash; a token for another object cannot cover the content root; the
  CLI denies `/secret` with the same text and no bytes (10/10).
* **S9** S2/S4/S5 repeated for `--blob-backend s3 --commit-backend local`
  (60/60).

The passing receipt is
`bench/receipts/distributed-20261106T134954Z.json`: **9/9 scenarios, 229 checks
passed, 0 failed, 120 processes recorded**, 42 isolated buckets, 5,968 ms of
harness wall time, 7,460 KiB peak RSS. The first run,
`bench/receipts/distributed-20261106T133534Z.json`, is retained as the failing
receipt (206 passed / 23 failed) that found the defects below. Analysis and
every limit: `bench/RESULTS-distributed.md`.

`tests/distributed_semantics.rs` is the always-on half: **12 passed / 0
failed**. It pins the interlock, stale reader (local and local split),
8-thread concurrent readers, the crash matrix (local and split), the three
retry cases, the unreachable endpoint, the GC race, and content-hash
non-disclosure without needing MinIO.

### Defects found and fixed

1. Opening a store whose `FORMAT` read failed reported every error as
   `Corrupt`, including a connection refusal. `Store::open_impl` now preserves
   `Class::Io` with the same context text.
2. A transport failure during a read was reported as
   `Corrupt: missing blob …: Io: …`. The recovery blob/superblock/commit
   wrappers and the three chunk-read wrappers now preserve `Class::Io`; a
   genuinely missing or mismatched record is still `Corrupt`. This is what
   makes S6's "fails closed with a transport error" literally true.

### Verification and gates

* `cargo test --all-targets` (rust court): **392 passed / 0 failed**.
* `cargo test --features fuse --all-targets` (fuse court): **434 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `scripts/s3_court.sh` (single-node MinIO unit court): **6/6**.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* Not a cloud, multi-node, fleet, device-cache, media, or production claim:
  one host, one MinIO process on loopback, a tmpfs data directory.
* No TCP-level partition: the only network faults are an endpoint with no
  listener and stopping/restarting the server process. There is no packet
  filter, delay, duplication, or split-brain case.
* No physical media or power-loss claim; S3 durability means the server
  acknowledged the write.
* Writers are exclusive by design (one at a time); no multi-writer protocol is
  claimed or tested.
* The `after_commit_file` retry produces a new generation (3 instead of the
  reference 2) because an unconfirmed commit file is never reused. Its visible
  state is identical and there is no second visible state.
* A crashed S3 writer's 30 s lease is cleared explicitly by the court before
  each retry; production waits it out and the lease is never disabled. The
  receipt records this.
* The court is Rust-only; the split layout's commit plane is local in S9.

## Subphase R14 — GCS backend (2026-11-06)

`src/planes.rs` splits durable storage into an immutable **blob plane** and a
versioned **commit plane**; `src/s3.rs` implements both for S3-compatible
services with SigV4, `If-None-Match: *` / `If-Match`, and a probed stale-CAS
refusal. GCS is the next backend target, but its concurrency model is
**generation preconditions** (`ifGenerationMatch`), not ETags. R14 adds a GCS
JSON-API backend against the same two planes. No manifest, commit record,
superblock slot, logical key name, or conformance vector changed.

### What is implemented

* `src/gcs.rs` — `GcsConfig` and `GcsBackend`.
  * `BlobPlane`: `get` (`alt=media`), `get_range` (`Range` header, `206`),
    `put_if_absent` (`ifGenerationMatch=0`), `len`/`exists` (object metadata),
    `delete`, `list` (object collection with `prefix`/`pageToken`), `head_part`/
    `probe` (the shared plane probes), `clean_stale`, `init_layout`.
  * `CommitPlane`: `read_control`, `compare_exchange` (read the bytes and the
    observed generation, refuse unless the caller's content token matches, then
    upload with `ifGenerationMatch=<generation>`), `write_control` (generation-
    conditioned retry), `create_control`, `put_control_if_absent`,
    `delete_control`, `control_len`, `control_exists`, `list_control`,
    `sync_dir` (provider acknowledgement), `acquire_lock` (a `LOCK` object with
    an owner and lease; an expired lease is reclaimed with
    `ifGenerationMatch=<observed>`), and `probe`.
  * HTTP `412 Precondition Failed` maps to the same `Busy` class the S3 backend
    reports for a lost conditional write. `401`/`403` are `Unauthorized`; every
    transport failure is `Io` and is never reported as `Corruption`; `413` is
    `LimitExceeded`.
  * Uploads: payloads up to 5 MiB use the JSON API simple media upload; larger
    payloads use the documented two-step resumable upload (session initiation
    carries the precondition, the bytes go to the returned session URI in one
    bounded request, and `308` is a hard error — a partial upload is never
    published).
  * A generation is read *and re-checked after the bytes*, so a concurrent
    replacement can never be mistaken for the observed state. That read is
    bounded to four attempts and then fails `Busy`.
* `StorageBackend for GcsBackend` delegates to those same plane operations, so
  one implementation of each physical operation carries the CLI, the store,
  FUSE, GC, verify, and recovery. `BackendKind::Gcs { config }` participates in
  split layouts (`--blob-backend gcs --commit-backend local` and the reverse).
* CLI: `--backend gcs` (and `--blob-backend`/`--commit-backend gcs`) with
  `--gcs-endpoint`, `--gcs-bucket`, `--gcs-token`, `--gcs-project`,
  `--gcs-anonymous`; environment `WEFT_GCS_ENDPOINT`, `WEFT_GCS_BUCKET`,
  `WEFT_GCS_ACCESS_TOKEN` / `GOOGLE_OAUTH_ACCESS_TOKEN`, `WEFT_GCS_PROJECT` /
  `GOOGLE_CLOUD_PROJECT`, `WEFT_GCS_ANONYMOUS`, `WEFT_GCS_LOCK_LEASE_SECS`.
* Authentication is an OAuth2 **bearer token** supplied by flags or environment,
  or anonymous mode for an emulator. Service-account JSON key signing needs
  RS256 and is **not** implemented: setting `GOOGLE_APPLICATION_CREDENTIALS`
  alone is refused with a message saying so. No RSA dependency was added.
  `GcsConfig`'s `Debug` redacts the token, and no token reaches diagnostics,
  error text, receipts, or the court's environment list.
* The logical listing helpers (`objects/`, `manifests/`, `indexes/`, `refs/`,
  `tmp/`, `packs/`) moved from `src/s3.rs` to `src/backend.rs` verbatim and are
  now shared, so the S3 and GCS listing paths cannot drift from the normative
  layout.

### Generation preconditions and the stale-must-fail probe

The probe runs on every writable open (`init_layout`/`ensure_layout`, before any
commit is published) and again as `CommitPlane::probe`, both against a private
`tmp/` key:

1. publish with `ifGenerationMatch=0` — must succeed (create-once);
2. publish a second time with `ifGenerationMatch=0` — must be refused `412`,
   otherwise the store is refused `UnsupportedVersion` because create-once
   cannot be guaranteed;
3. read the bytes and the observed generation;
4. publish with `ifGenerationMatch=<observed>` — must succeed;
5. publish **again** with the same, now stale, generation — must be refused
   `412`. An endpoint that accepts it is refused `UnsupportedVersion`, because
   head publication could then lose a race silently.

A probe may only refute a declared capability, never upgrade one. Durability is
not probed: `durable_fsync` means the provider acknowledged the write, exactly
as for the S3 backend, and no media or power-loss claim is made.

### Court

`bench/scripts/gcs_court.sh` downloads the pinned
`fake-gcs-server` `v1.56.1` `linux-amd64` release tarball (sha256
`0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`), verifies
it, and runs the binary **inside** the rust court on loopback with the memory
backend. If the binary cannot be obtained the script writes an
`emulator_unavailable` receipt naming the exact command and error; it never
substitutes another emulator.

Passing receipt `bench/receipts/gcs-20261106T150444Z.json` (harness, at
WeftFS `ca7004c`) and `bench/receipts/gcs-court-20261106T150444Z.json` (court):
12 files x 32 KiB, 8 KiB chunks, eight 4 KiB range writes, deterministic ids
and fixed clock.

* local and GCS produce the **same content root**
  (`0a8198f56ed59a11…`), the same **generation 21**, the same **commit sequence
  1…21**, the same 393,216 verified bytes, and the same content digest
  (`2e3b9902…`).
* GCS `verify --full` is clean; a read-only reopen recovers generation 21 and
  the same root with a clean full verify.
* The `superblock` control object's GCS generation was sampled three times and
  is strictly increasing (`1793977493537315`, `…551975`, `…563518`): every head
  publication replaced it under `ifGenerationMatch`.
* The plane probe recorded `stale_compare_exchange_rejected: true` against the
  real emulator, not only the in-process fake.
* Crash proxy (CLI aborted by signal at each phase): `after_objects` → old
  state, clean verify; `after_commit_file` → old state plus the documented
  unpublished commit tail (the only problem accepted, and labelled); 
  `after_superblock_write` → new state, clean verify. 3/3 old-or-new, **0
  partial**.
* 14/14 in-process fake-GCS unit tests in the same run.

### Defects found and fixed by running the emulator

1. `buckets.insert` was sent to `/storage/v1/b/<bucket>` with an empty body.
   GCS takes the bucket resource in the body of `POST /storage/v1/b?project=…`;
   the emulator returned `400 EOF`. The backend now sends `{"name": …}` to the
   collection URL. Retained failing receipt `gcs-court-20261106T145714Z.json`.
2. Object listing used `/storage/v1/b/<bucket>?prefix=…` (bucket metadata)
   instead of `/storage/v1/b/<bucket>/o?prefix=…`. The emulator's listing was
   therefore empty and `verify --full` reported a broken parent chain. Fixed.
   Retained failing receipt `gcs-court-20261106T145838Z.json` (a first run that
   also over-asserted a clean verify for the documented unpublished tail).

### Tests and gates

* `cargo test --test gcs_backend -- --test-threads=1`: **14 passed / 0 failed**.
* `cargo test --all-targets` (rust court, WeftFS `ca7004c`): **416 passed / 0
  failed / 0 ignored**.
* `cargo test --features fuse --all-targets` (fuse court): **458 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `bench/scripts/gcs_court.sh`: unit tests 14/14 and the court above.
* `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* The emulator is `fake-gcs-server` with a **memory** backend on loopback. It is
  not Google Cloud Storage: no IAM, no bucket policies, no requester-pays, no
  object composition, no signed URLs, no multi-region behaviour, no media
  durability, and no physical power loss. Nothing here is a cloud, fleet,
  multi-node, or production claim.
* Only bearer-token authentication is implemented. No OAuth2 refresh flow, no
  service-account JWT (RS256), no workload identity, no `GOOGLE_APPLICATION_CREDENTIALS`.
  Anonymous mode is for emulators and is refused together with a token.
* Writers are exclusive by design (one `LOCK` object with a lease); no
  multi-writer protocol is claimed.
* The generation-precondition read is bounded to four attempts; a store under
  continuous concurrent replacement fails `Busy` instead of guessing.
* `modified()` returns `None` (the JSON API reports a timestamp string and this
  crate has no date parser); the store does not depend on it.
* No batch/JSON-multipart upload path, no retry on `429`/`5xx`, no client-side
  rate limiting. A `429` is an `Io` error.
* At R14 only Rust had a GCS backend (Go GCS landed in R26); that subphase
  made no cross-language GCS parity claim, and the on-disk format was unchanged.
* The in-process fake service returns one listing page and does not paginate;
  the real emulator court exercises pagination-free listings of 21 commits plus
  objects. Pagination is implemented in the backend but only exercised for the
  single-page case.
* The crash proxy is a process-fault proxy (the CLI is aborted by signal), not
  a physical power-loss test; each phase waits out the 2 s lock lease before
  reopening, which the receipt records.

## Subphase R15 — Orbax/TensorStore adapter (2026-11-06)

`python/weftcodec/orbax.py` adds an Orbax-compatible `AsyncCheckpointHandler`
over the same canonical `weft.checkpoint.v1` checkpoint and Rust CLI path as the
DCP adapter. The importable module is `weftcodec.orbax`. It flattens a JAX
PyTree with
`jax.tree_util.tree_flatten_with_path`, writes the leaves into one safetensors
staging container, ingests it with `weftfs checkpoint write`, and records a
canonical WCB `weft.orbax.v1` record binding each leaf's path, path-key types,
dtype, shape, total element bytes, content root, and logical root. The store
record lives at `/checkpoints/<name>/orbax.metadata`; the Orbax directory
carries a small canonical `weft.orbax.wcb` locator naming the store, checkpoint,
storage generation, and the metadata record's BLAKE3 root. No new wire format
was introduced: both records are ordinary canonical WCB application records and
the checkpoint is unchanged.

Restore flattens the target PyTree, requires exact path/dtype/shape agreement,
re-reads the Rust manifest, restores the requested entries through
`weftfs checkpoint restore`, re-verifies every leaf's content root, and rejects
missing, extra, or mismatched leaves (`MissingLeaf`, `UnexpectedLeaf`,
`MismatchedLeaf`). `partial_restore=True`, a direct `restore_leaf(path)`, and a
requested JAX target sharding are supported; a Rust-side
`restore_topology` proof (`--topology N`) is independent of the JAX placement.
The asynchronous path returns immediately and starts the save on Orbax's
background commit thread only after Orbax has created its temporary directory;
`staging_bytes` bounds the staging container and the restored bytes. Real
`orbax.checkpoint.Checkpointer`, `AsyncCheckpointer`, and `CheckpointManager`
all drive the handler in tests.

### Installation evidence

JAX and Orbax were installed **inside the python court** with `pip --target`
into `/work/.docker-cache/py-orbax`; the image was not modified. The install
succeeded from PyPI on 2026-11-06: `jax 0.12.0.1`, `jaxlib 0.12.0.1`,
`orbax-checkpoint 0.12.9`, `tensorstore 0.1.86`, `numpy 2.5.4`. The exact install
command, status, and tail are in
`bench/receipts/orbax-20261106T152612Z.install.log` and the receipt's `install`
object. The adapter tests in that court were **18 passed**
(`orbax-20261106T152612Z.tests.log`). The measured harness checked **13/13**
conditions and wrote `bench/receipts/orbax-20261106T152612Z.json` with
`status: ok`. The Python court receipt
`bench/receipts/python-bindings-20261106T153054Z.json` records **349 tests / 9
skipped** (340 passed, 0 failures, 0 errors) and the four optional dependency
versions.

### Workload and measured comparison

One deterministic 8-way sharded PyTree on a forced 8-device CPU mesh
(`XLA_FLAGS=--xla_force_host_platform_device_count=8`): 11 leaves,
58,765,352 logical bytes, dtypes bf16/f16/f32/i8/u8/i32, random values from a
fixed PRNG seed. Orbax's own side is
`PyTreeCheckpointHandler(use_ocdbt=True, use_compression=True)` under real
`AsyncCheckpointer`; the WeftFS side is `WeftOrbaxHandler` under the same
orchestrator.

| metric | Orbax OCDBT | WeftFS Orbax adapter | delta |
|---|---:|---:|---:|
| save pause (ms) | 10.273 | 1.017 | -9.256 (deferred work) |
| total save (ms) | 79.631 | 1,770.713 | +1,691.082 (**22.2x**) |
| full restore (ms) | 21.243 | 2,690.338 | +2,669.095 (**126.7x**) |
| single-leaf restore (ms) | 5.248 | 380.432 | +375.184 (**72.5x**) |
| checkpoint physical bytes | 50,644,895 | 50,866,816 | +221,921 (**+0.44 %**) |
| physical / logical | 0.861816 | 0.865592 | +0.003776 |
| physical files | 13 | 189 | +176 |
| exact leaves | 11/11 | 11/11 | 0 |
| peak process RSS (KiB) | — | 1,693,668 | includes JAX + both stores |

The physical comparison is the store immediately after publish, excluding the
restored-container imports. The CLI's `checkpoint restore` imports each
restored container under `/dcp-reads/<uuid>`; repeated restores grew the store
by **67,615,378 B** in this run (50,866,816 -> 118,482,194 B). That overhead is
reported separately and is not part of the checkpoint-byte comparison.

### Exactness, topology, and Rust verification

All **11/11 leaves exact** on both sides: dtype, shape, and SHA-256 of the exact
bytes. The Rust manifest, the canonical metadata record, and every leaf content
root and logical root agree. `checkpoint verify` verified 11 tensors;
`verify --full` was **ok** immediately after save (generation 3) and **ok**
after all restores (generation 14, 95 objects, 3,719 chunks, 95 manifests).

A requested 2-device `NamedSharding` restore target is honoured by the WeftFS
adapter (`requested_sharding_honored: true`) with 11/11 exact leaves. Orbax
0.12.9 restored with the saved 8-device sharding and emitted its own warning
(`requested_sharding_honored: false`) but was still 11/11 exact; that is an
Orbax limitation on this path, not a WeftFS loss. The Rust-side topology proofs
`--topology 8` and `--topology 2` were both exact (242.785 ms and 253.270 ms).

### Every measured loss

* Total save **22.2x slower** (1,770.7 vs 79.6 ms); the Rust
  `checkpoint write` command alone was 1,701.4 ms because the default `zstd`
  shard strategy is slow on incompressible random weights.
* Full restore **126.7x slower** (2,690.3 vs 21.2 ms); the CLI process reopens
  the store, decodes shards, and imports a restored container.
* Single-leaf restore **72.5x slower** (380.4 vs 5.2 ms) for the same reasons.
* Physical bytes **+0.44 %** (+221,921 B) and 189 files vs 13; the store also
  carries manifests, commits, indexes, and the metadata object.
* Restored-container imports add **67,615,378 B** (`/dcp-reads/...`) across the
  restores in this run.
* The adapter writes an extra full-size safetensors staging container before
  the Rust CLI ingests it, then removes it.
* The smaller **save pause** is a scheduling effect (work deferred to Orbax's
  background commit thread), not a throughput win; total save is 22.2x slower.
* Peak process RSS 1,693,668 KiB (~1.62 GiB) includes JAX, Orbax, TensorStore,
  the source tree, both restored trees, and the store; it is not an isolated
  adapter number.

### Non-claims and gates

**CPU only** (eight forced CPU devices in one process); **no TPU and no GPU**.
**Single process**, single host; **no multi-host** and no true distributed JAX.
The Orbax checkpoint is OCDBT/TensorStore and the WeftFS checkpoint is a
separate on-disk format; this is not file-level interoperability. Unexercised
Orbax features include Zarr3, per-leaf `save_args`/`restore_args`, transforms,
custom metadata, step-metadata contents, retention/deletion internals,
emergency checkpointing, and multi-axis meshes. Supported leaf dtypes are only
the exact WEFT-TENSOR v1 set; bool, float8, complex, and unsigned 16/32/64-bit
arrays are rejected, never coerced. There is no process-crash, power-loss, or
media-failure court for this adapter, no Go-side Orbax adapter, and no fleet or
production claim. The workload is synthetic random weights, one run, warm
caches; the results in `bench/RESULTS-orbax.md` are the measured record and
include every loss above.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass, and the Python court receipt is
`bench/receipts/python-bindings-20261106T153054Z.json`.

## Subphase R16 — independent S3 implementation (Ceph RGW) (2026-11-06)

MinIO (Q4) and `fake-gcs-server` (R14) are single implementations of their
protocols, so neither can answer whether "S3-compatible" means the wire
behaviour the store depends on. This subphase adds a **Ceph RADOS Gateway**
court: a genuinely independent S3 implementation (Ceph shares no code with
MinIO), plus a reusable compatibility matrix that can be pointed at any
S3-compatible endpoint.

### What is implemented

* `scripts/ceph_court.sh` — starts `quay.io/ceph/demo` (Ceph 19.2.0 squid:
  mon + mgr + one bluestore OSD + rgw/beast, pinned by image digest) on a
  dedicated bridge, waits for a real HTTP answer from the frontend, runs the
  existing S3 court logic and the matrix against it, optionally runs the same
  matrix against MinIO, and writes a court receipt plus a side-by-side
  comparison.
* `examples/s3_compat_matrix.rs` — the eight behaviours that commonly differ
  between "S3-compatible" servers, measured rather than assumed. Harness check
  names: `if-none-match-create-once`, `if-match-conditional-overwrite`,
  `range-open-ended`, `range-out-of-range`, `head-missing-key`,
  `delete-idempotent`, `list-pagination`, `stale-cas-probe`
  (`If-None-Match: *` create-once, `If-Match` conditional overwrite,
  open-ended range read, out-of-range `416`, `HEAD` on a missing key, delete
  idempotence, `ListObjectsV2` pagination with `max-keys` +
  `continuation-token`, and a stale compare-and-swap probe on an
  8192-byte superblock-shaped object). `--compare a.json b.json` prints and
  writes the two-implementation matrix.
* `scripts/s3_court.sh --matrix` — the same matrix against MinIO, sharing the
  run stamp so the two receipts compare cleanly.
* `S3Backend` now **measures** the `If-Match` comparison form at writable open
  (`IfMatchForm::{AsReturned, Dequoted}`) instead of assuming the AWS/MinIO
  rule. The chosen form is reported in diagnostics as
  `S3 If-Match form measured at open: ...`. It is accepted only if a matching
  value succeeds, a stale value in the *same* form is refused `412`, and the
  winner's bytes survive; otherwise the endpoint is refused
  (`UnsupportedVersion`) exactly as before. No capability is inferred and no
  check is skipped. The writer lock negotiates the form itself, because the
  store locks before it probes the planes.

### Evidence

Ceph RGW 19.2.0, `ceph version 19.2.0 (16063ff2022298c9300e49a547a16ffda59baf13)
squid (stable)`, image
`quay.io/ceph/demo@sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723`:

* court: `bench/receipts/ceph-rgw-20261106T160906Z.json`
  (+ `.cluster.txt` for the raw `ceph -s`)
* matrix: `bench/receipts/s3-compat-ceph-20261106T160906Z.json`
* MinIO matrix: `bench/receipts/s3-compat-minio-20261106T160906Z.json`
* comparison: `bench/receipts/s3-compat-matrix-20261106T160906Z.json`
* MinIO regression receipt for the modified client:
  `bench/receipts/s3-court-20261106T160906Z.json`

The existing S3 court logic passes against RGW: `cargo test --test s3_backend`
reports **6 passed; 0 failed** (44.77 s) — store round trip (content root,
generation sequence, read-only reopen match, clean `verify --full`), writer
lock with a lost conditional head update, the three-phase crash proxy, GC, and
local-vs-S3 byte identity.

Both implementations are **8/8** on the matrix. The one deviation is the
`If-Match` entity-tag quoting rule: MinIO accepts the quoted ETag it returned;
Ceph RGW compares the header bytes literally, so its own quoted ETag is
**accepted only after de-quoting** — `412 PreconditionFailed` for
`If-Match: "9abcde3c..."` and `200` for `If-Match: 9abcde3c...`, with the stale
value still refused `412`. Without the measured form the store refused RGW
writable open (`S3 conditional probe: a matching If-Match was unexpectedly
rejected`) and five store-level tests could not run.

Two RGW-specific configuration facts are recorded in the receipt:
`--rgw-thread-pool-size 32` (the default 512-thread RGW pool plus
`radosgw-admin`'s own 512-thread pool exceeds the 1024-pid court bound; the
kernel refuses thread creation and `ceph-mon`/`radosgw` abort silently — the
pids limit was not raised), and IP-literal addressing (RGW's demo config sets
`rgw dns name` to the container hostname and treats a matching `Host` as a
virtual-hosted bucket, so path-style requests need an endpoint whose Host is not
that name).

### Repository suites at this change

* `cargo test --all-targets`: 92 suites, **418 passed, 0 failed**.
* `cargo test --features fuse --all-targets`: 92 suites,
  **460 passed, 0 failed, 2 ignored** (the two pre-existing ignores for kernel
  7.3 direct-I/O EIO and the missing CAP_SETUID ownership test).
* Sidecar limits: 3g memory/swap, 1024 pids (observed 316 used, 319 threads,
  0 OOM kills), 4 cpus, bounded tmpfs, `cap-drop=ALL` plus CHOWN,
  DAC_OVERRIDE, FOWNER, SETGID, SETUID for the Ceph daemons' uid 167; peak
  container memory 1,120,411,648 B.

### What is not covered

No physical media or power-loss claim (the RGW OSD is a tmpfs file in one
container, and there is no RGW process-crash scenario); no multi-node,
multisite, or zone-replication claim; only the eight matrix behaviours (no TLS,
presigned URLs, multipart upload, policies/ACLs, versioning, object lock,
lifecycle, SSE); listing pagination exercised at `max-keys=2`, not at the
1000-key boundary; no Go-side S3 backend, so no cross-language claim against
RGW; no AWS or third-vendor measurement; no concurrency court beyond two
writers; no performance claim. The analysis, exact commands, and the raw
`If-Match` evidence are in `bench/RESULTS-ceph.md`.

### Aggregate coverage

`scripts/verify-all.sh` runs this court as its 28th entry (`ceph_rgw`), so the
aggregate now proves the S3 backend against an independent implementation on
every verification run, not only on the dedicated R16 receipts. The court
exit status is mapped explicitly: `0` is `pass`, a real failure is `fail` and
fails the aggregate, and exit `78` is `skip` — recorded as `skip` together with
the court's own reason string under a `skips` object in the verify receipt.
Skip is reserved for environment refusals detected *before* the server starts
(non-x86_64, no Docker daemon, the pinned Ceph image cannot be obtained, or the
court's private subnet is already in use); the rule is that **a skip is not a
pass**, and the receipt's `summary` block carries `courts`/`passed`/`failed`/
`skipped` so a skip cannot hide inside a percentage.

The first aggregate run containing this court is
`bench/receipts/verify-20261106T162208Z.json`: **28/28 courts pass, 0 skipped**
at WeftFS `0e3670a`, with the Ceph court's own receipt at
`bench/receipts/ceph-rgw-20261106T162208Z.json` (named in the verify receipt's
`receipts` block).

The skip path itself is measured: a second aggregate run at the same code with
`WEFT_CEPH_IMAGE` pointed at a nonexistent digest produced
`bench/receipts/verify-20261106T162933Z.json` (27 passed, 0 failed, 1 skipped;
`ceph_rgw: skip` with the reason `cannot obtain pinned image ... manifest
unknown`). That run is a deliberate negative test of the skip mechanism and is
not the current pass receipt; the all-pass run above is.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass (the frozen formats and spec files are untouched by this subphase).

---

## Subphase R17 — fenced writer protocol (2026-11-06)

Closes the reviewer-reported defect in `src/s3.rs`: `release_lock()` read
`LOCK`, checked the owner, then issued an **unconditional `DELETE`**, so a
writer whose lease a successor had just reclaimed could delete that successor's
lock; and nothing consulted the lease at publication time, so an expired writer
could keep publishing. A conditional head compare-and-swap alone is not
ownership proof, because it says nothing about who holds the lease.

### What is implemented

* `LOCK` is now `weftfs-s3-lock v2`: `owner`, a **monotonic fencing token**
  (`fence`), `sequence`, an absolute `lease_deadline_ns`, `renewed_ns`, and the
  head being published (`pending_generation`, `pending_commit`). The first
  lease takes fence 1; every takeover takes `previous + 1` (checked); a legacy
  v1 record is read as fence 0 so the chain stays monotonic across the change.
* **The fence is one conditional write.** Before every publication the store
  writes `LOCK` with `If-Match: <ETag of the bytes this handle last
  published>`. That succeeds only while the object still carries this writer's
  token, so a successor's takeover (which rewrote the object) makes a stale
  writer fail `412` → `Busy`, *before* the head is written. The store calls the
  proof before the commit record and again — this time naming the head — as the
  last write before the superblock compare-and-swap.
* **Lease renewal** happens inside that same conditional write, once per
  publication, when at most one publication budget remains (`deadline - now <=
  lease/2`), extending the deadline to `now + lease`. The interval is bounded
  by the lease.
* **Own-deadline stop.** A writer refuses to publish when its own deadline has
  passed, without consulting anyone and even with no successor. The refusal
  names the expired token and, read-only, who holds the lock now.
* **Release is a conditional tombstone**, never an unconditional delete:
  `weftfs-s3-lock-released v1` with `If-Match` on the observed ETag, carrying
  the released token and `released_from` (sha256 of the exact bytes replaced).
  A record that names another writer is never touched. A tombstone is preferred
  over a conditional `DELETE` because deleting the object would reset the
  fencing token. The writable-open probe already refuses an endpoint whose
  conditional writes cannot be proven, which is the primitive this needs.
* **The same question for POSIX backends.** `LocalBackend` and `ObjectBackend`
  refuse to publish when the `LOCK` name no longer resolves to the exact
  `(dev, ino)` this handle flocked, i.e. when the namespace was handed to
  someone else.
* `CompositeBackend` delegates the proof to its commit plane (the plane that
  owns `LOCK` and the head), so a split store with S3 blobs and local commits
  is fenced by its local lock and an S3 commit plane is fenced by its lease.

### Exact atomicity statement

S3 allows one conditional write per object and no multi-object transaction, so
the fence and the head write are two operations on two objects and cannot be
fused. Safety does not depend on fusing them: a publication may only start
while `now + budget <= deadline` (`budget = lease/2`), and a takeover is only
allowed at `now > deadline`, so the publication window closes before any
successor may touch the lock; the gate is the last write before the head CAS.
A lost network reply remains the standard at-least-once ambiguity of any
conditional `PUT` and is recorded as a residual, not as a pass.

### Evidence

* `tests/s3_fencing.rs` (MinIO court): **8/8** in 15.23 s. Real `SIGSTOP` child
  (the test binary re-executes itself), reclaimed lease, `SIGCONT`; paused
  writer fence 2 → successor fence 3, `Busy`, paused generation and head
  unchanged, published head the successor's, `/fenced` absent, `verify --full`
  ok. Stale release leaves the successor's `LOCK` bytes and ETag identical.
  20 release/acquire races each advance the token by exactly one and never
  delete the object. Two renewal windows extend the deadline with a constant
  token. Five leases produce fences 1–5.
* `examples/distributed_court.rs` scenario **S10**: 26 checks, **10/10
  scenarios, 255 checks, 0 failed, 126 processes**
  (`bench/receipts/distributed-20261106T174634Z.json`). Token map
  `{paused 2, successor 3, release_holder 2, release_racer 3, renew 2, expired
  2}`, generation sequence `[2, 2, 2]`.
* `tests/writer_fencing.rs` (local + object): 4/4.
* Rust `cargo test --all-targets`: 94 targets, **431 passed, 0 failed**.
  FUSE `--features fuse --all-targets -- --test-threads=1`: 94 targets,
  **473 passed, 0 failed, 2 ignored** (both pre-existing).
* S3 court receipt `bench/receipts/s3-court-20261106T174609Z.json` (6 backend +
  8 fencing tests, MinIO): s3_backend 6/6 in 0.53 s, s3_fencing 8/8 in 15.23 s.
* Independent implementation: `scripts/ceph_court.sh` now runs the fencing
  suite too. Against Ceph RGW 19.2.0 (literal `If-Match`, dequoted ETag, and
  roughly an order of magnitude slower per round trip) `s3_backend` is 6/6 in
  41.96 s and `s3_fencing` 8/8 in 80.85 s. Receipts
  `bench/receipts/ceph-rgw-20261106T174703Z.json` and the aggregate
  `bench/receipts/verify-20261106T174703Z.json` (**28/28 courts pass** at
  `37a3a39`).
* Analysis and exact commands: `bench/RESULTS-fencing.md`.

### Lease configuration (and one behaviour change)

The fencing suite needs a lease a court can expire (`WEFT_S3_FENCE_LEASE_SECS`,
3 s); the general suites use 60 s (`WEFT_S3_LOCK_LEASE_SECS`). The split exists
because a writer that stays **idle past its deadline must reopen**: the deadline
check fires before the lock is read, so a long-idle handle gets `Busy` instead
of silently re-acquiring. That is deliberate (the reviewer's requirement is
that a writer stops once its deadline has passed), but it is a real behaviour
change for a rarely-writing long-lived handle. The first aggregate run used a
2 s lease for every S3 court and `store_round_trip_over_s3` failed on RGW with
`S3 writer lease deadline passed 216 ms ago`; that receipt
(`verify-20261106T171925Z.json`) is retained as the evidence.

Two more defects the RGW run found were in the harness, and are fixed: the
crash-proxy child's 1 s lease expired *mid-publication* on the slower endpoint
(the child now gets a realistic lease and the reset is the fenced tombstone),
and `release_stale_lease` did not negotiate the endpoint's `If-Match` form on a
fresh handle, so every court reset was a `412`. The release/acquire race test
also genuinely races now: the acquirer retries until it has observed the live
lock, records at least one `Busy`, and then wins with the next token.

### Not claimed

Not a multi-writer protocol: exactly one writer is admitted, and there is no
merge, queue, or consensus point for the head. No single atomic fence+head
operation (not expressible on plain S3). No defence against a badly skewed
writer clock beyond the budget assumption. The **GCS backend kept its older
lease code and no fence** in R17 (`verify_write_lease` defaulted to a no-op
there); that gap is closed in R18 below. Go parity was absent at this
subphase (WeftFS-Go had no S3 backend); R25 adds the Go S3 backend. No
physical-media or power-loss claim.
`scripts/verify-all.sh` passes 28/28 after this subphase
(`bench/receipts/verify-20261106T174703Z.json`); the aggregate includes the
MinIO and Ceph-RGW courts with the fencing suite, but not the separate
multi-process `s3_court.sh --distributed` run.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass; no frozen format changed, so no spec edition was needed.

## Subphase R18 — fenced writers on GCS (2026-11-06)

Closes the reviewer-reported gap in `src/gcs.rs`. Before this subphase the GCS
backend still had the pre-R17 shape:

* `release_lock()` read `LOCK`, checked that the body named this owner, then
  issued an **unconditional delete**; a successor that had reclaimed the lease
  in between lost its lock.
* `acquire_lock` wrote `owner` + `expires_ns` (`weftfs-gcs-lock v1`) and
  nothing consulted the lease at publication time.
* `verify_write_lease` was not implemented, so the trait default (`Ok(())`)
  let a stale writer publish, and `fence_head_publication` fell back to the
  same no-op. The head compare-and-swap only proves the head did not move; it
  says nothing about who holds the lease.

### What is implemented

* `LOCK` is now `weftfs-gcs-lock v2` with the **same field set as S3's record**:
  `owner`, monotonic `fence`, `sequence`, absolute `lease_deadline_ns`,
  `renewed_ns`, `pending_generation`, `pending_commit`. A legacy
  `weftfs-gcs-lock v1` record is read as fence 0, so the chain stays monotonic
  across the upgrade. Encode/parse/describe now live in `src/writer_fence.rs`
  and are shared by both backends; only the schema header differs.
* **Takeover** of an expired lease or a released tombstone is one upload with
  `ifGenerationMatch=<the generation read>`, taking `previous.fence + 1`
  (checked). A lost race retries from a fresh read.
* **The fence is one generation-conditioned upload.** Before every publication
  the store uploads `LOCK` with `ifGenerationMatch=<the generation this handle
  last published>`. That succeeds only while the object still carries this
  writer's token, so a successor's reclaim (which changes the generation) makes
  a stale writer fail `412` → `Busy` before the head is written. The store calls
  the proof before the commit record and again, naming the head, immediately
  before the superblock CAS.
* **The upload path reports the generation** (`upload_tracked`). The JSON API
  returns the object resource in the reply; if an endpoint omits it, the
  generation is confirmed by a read-after-write that must return exactly the
  bytes just published. An endpoint that can do neither is refused
  `UnsupportedVersion` rather than guessed at.
* **Renewal** happens inside the same conditional upload, once per publication,
  when at most one publication budget remains (`deadline - now <= lease/2`),
  extending the deadline to `now + lease`.
* **Own-deadline stop**: a writer whose `now >= lease_deadline_ns` refuses
  before any head object is touched, even with no successor.
* **Release is a conditional tombstone** (`weftfs-gcs-lock-released v1`) written
  with `ifGenerationMatch` on the observed generation, carrying the released
  token and `released_from` (sha256 of the exact bytes replaced). A lock that
  now names another writer is never touched. `LOCK` is never deleted after a
  read.
* `verify_write_lease` and `fence_head_publication` are implemented on both the
  `StorageBackend` and `CommitPlane` impls; `CompositeBackend` already delegates
  to its commit plane, so a split store with GCS commits is fenced too.
* Diagnostics gained the same counters as S3 (`fence_checks`,
  `lease_renewals`, `fence_refusals`, `lease_expired_publications`). The
  end-to-end court observed **42 fence CAS operations for 21 commits**
  (proof before the commit record and fenced proof before the head).

### Protocol mapping

| concern | S3 (R17) | GCS (R18) |
|---|---|---|
| conditional primitive | `PUT` with `If-Match` | upload with `ifGenerationMatch` |
| create once | `If-None-Match: *` | `ifGenerationMatch=0` |
| observed token | ETag string | object generation (`u64`) |
| token source | response `ETag` header | upload reply object resource (verified re-read fallback) |
| stale write | `412` → `Busy` | `412` → `Busy` |
| takeover | `If-Match` on observed ETag | `ifGenerationMatch` on observed generation |
| fence | conditional rewrite of `LOCK` | conditional upload of `LOCK` |
| renewal | same conditional write | same conditional upload |
| release | tombstone with `If-Match` | tombstone with `ifGenerationMatch` |
| record | `weftfs-s3-lock v2` | `weftfs-gcs-lock v2` (identical fields) |
| tombstone | `weftfs-s3-lock-released v1` | `weftfs-gcs-lock-released v1` |
| writable-open proof | `If-Match` accepted, stale refused | matching generation accepted, stale refused |

### Shared invariant tests

`tests/support/fencing.rs` holds the test bodies; `tests/s3_fencing.rs` and
`tests/gcs_fencing.rs` import them and supply only the court configuration and
observer hooks. The same five invariants therefore run on MinIO, Ceph RGW, and
`fake-gcs-server`:

1. an expired lease with **no successor** refuses publication; head,
   generation, commit count, bytes, and the still-live token are unchanged;
2. a **paused** writer (real `SIGSTOP` child, reclaimed lease, `SIGCONT`) fails
   `Busy`; its own generation and head do not move; the successor's head is
   published and its file is the only visible one;
3. **release of a replaced lock** leaves the successor's `LOCK` bytes and
   conditional token byte-for-byte and value-for-value identical;
4. **renewal** extends the deadline under the same conditional write, keeps
   `fence` constant, and advances the sequence across two windows;
5. **takeover monotonicity**: five leases produce fences 1–5 and every
   publication names its pending head generation.

The S3 court additionally keeps its release/acquire race (20 iterations, the
acquirer records at least one `Busy` and wins with `fence + 1`, the object is
never deleted).

### Exact atomicity statement

Unchanged from R17 in form, but GCS makes the two operations even more explicit:
the fence is an upload of `LOCK` conditioned on the old `LOCK` generation, and
the head is a compare-and-swap on `superblock` conditioned on the old superblock
generation. These are two conditional writes on two objects; GCS offers no
multi-object transaction. Safety does not depend on fusing them: a publication
may only start while `now + budget <= deadline` (`budget = lease/2`), and a
takeover is only allowed at `now > deadline`, so the publication window closes
before any successor may touch the lock; the fence is the last write before the
head CAS. A lost upload reply remains the standard at-least-once ambiguity of
any conditional write and is a recorded residual.

### Evidence

* **GCS (`fake-gcs-server` v1.56.1, tarball sha256
  `0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`)** at
  `5667c7c`: `tests/gcs_backend` **15/15** (1.42 s, including the
  omitted-generation fallback) and `tests/gcs_fencing` **7/7** (14.60 s, 3 s
  lease), then the end-to-end court with `content_root_match=true`,
  `generation_match=true`, `generation_sequence_match=true`. Court receipt
  `bench/receipts/gcs-court-20261106T182003Z.json`; harness receipt
  `bench/receipts/gcs-20261106T182003Z.json`; raw logs
  `gcs-20261106T182003Z.fencing.log` and `.tests.log`.
* **MinIO `RELEASE.2025-04-22T22-12-26Z`**: `s3_backend` **6/6** and the
  refactored shared suite `s3_fencing` **8/8** (15.19 s). Receipt
  `bench/receipts/s3-court-20261106T182043Z.json`.
* **Ceph RGW 19.2.0** (`quay.io/ceph/demo@sha256:522483cf...`): `s3_backend`
  **6/6** (41.94 s), `s3_fencing` **8/8** (80.87 s), compatibility matrix
  **8/8**. Receipts `bench/receipts/ceph-rgw-20261106T183427Z.json` and
  `bench/receipts/s3-compat-*-20261106T183427Z.json`.
* Rust `cargo test --all-targets`: **95 test binaries, 445 passed, 0 failed**.
  FUSE `cargo test --features fuse --all-targets -- --test-threads=1`:
  **95 test binaries, 487 passed, 0 failed, 2 ignored** (both pre-existing).
* Single-run cost observation (not a benchmark; two runs, not interleaved):
  the same 21-commit GCS workload was 14.64 → 15.96 ms create, 11.52 → 12.62 ms
  range write, 4.49 → 4.89 ms read, 9.86 → 9.22 ms reopen, 27.84 → 27.56 ms
  verify against `bench/receipts/gcs-20261106T150444Z.json` (pre-fence). The
  fence is two extra small conditional uploads per commit.
* Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
  pass. No frozen format changed, so no spec edition was needed.

### Flake disclosure

The first full Rust run after stage 1 failed
`distributed_semantics::single_writer_interlock_is_exclusive_and_reopens_cleanly`
at its `Store::open(...).unwrap()` after the first handle was dropped. It passed
standalone immediately, that whole binary passed **5/5** consecutive runs, and
the full suite rerun was **445 passed / 0 failed**. R18 touches no local-backend
code. Recorded here rather than silently omitted; the same intermittent
fresh-reopen shape was already noted for `tests/quota.rs` in R9.

One harness defect was found by the first court run and fixed:
`scripts/docker-court.sh` forwards an explicit environment allowlist, so
`WEFT_GCS_FENCING_LOG` did not reach the container. The failed court receipt
`bench/receipts/gcs-court-20261106T180729Z.json` is retained as evidence.

### Not claimed

No multi-writer protocol: exactly one writer is admitted, and there is no merge,
queue, or consensus point for the head. No single atomic fence+head operation
(not expressible on GCS's per-object preconditions). No defence against a badly
skewed writer clock beyond the budget assumption. **At R14 there was no Go
parity; WeftFS-Go has a GCS backend since R26.** No claim about a real Google
Cloud Storage service: the
evidence is one single-node emulator with a memory backend, so no multi-node,
device-cache, media, power-loss, or production claim is made. No performance
claim from the single-run phase comparison above.

`scripts/verify-all.sh` was **not** rerun for R18 (the GCS court is not part of
the 28-court aggregate); the suites, both gates, and the three fencing courts
were run directly and are cited above.

## Subphase R19 — pipelined restore (2026-11-06)

Closes the reviewer's top remaining item: parallel tensor decode existed, but
`materialize` still decoded, returned one `Vec<Vec<u8>>`, and only then wrote the
988 MB container, so the write was serial after the decode. The R4 court
measured that as a 0.43x 4-thread regression on the host bind mount.

### What is implemented

* `src/restore_pipeline.rs`:
  * `run_ordered_pipeline(inputs, sizes, options, work, measure, consume)` — an
    ordered producer/consumer runner. Workers decode with a bounded count and a
    bounded declared-byte reservation; **one writer (the calling thread) calls
    `consume` for indices 0, 1, 2, ... in exact source order**, outside the
    queue lock. There is no whole-file buffer.
  * `RestorePipelineOptions { threads, max_inflight_bytes }` and
    `RestorePipelineStats`: tasks, tensors/bytes written and dropped, observed
    in-flight bytes/tasks, oversized tasks, `decode_wall_ms`, `decode_sum_ms`,
    `transfer_ms`, `write_ms`, `total_ms`.
  * `Store::restore_tensors_pipelined` and
    `Store::restore_tensors_pipelined_authorized`, bound to the shipped
    `read_tensor` / `read_tensor_authorized` path (R8 cache/single-flight still
    apply where they applied before).
* Bounds and policy:
  * workers: `0 -> 1`, capped at `available_parallelism()`;
  * **reservations are granted in input order**, which is what makes the
    in-order writer deadlock-free against the staging bound;
  * non-oversized reservations (decoding + published-but-unwritten + the tensor
    being written) are bounded by `max_inflight_bytes` (default 64 MiB); an
    oversized tensor is tracked separately, at most one outstanding, so the peak
    is at most `max_inflight_bytes + largest tensor`;
  * the writer consumes strictly in order, so the first failure it sees **is**
    the lowest failing input index; it then stops claiming work, drains every
    in-flight decode, frees every reservation, joins every worker, and returns
    that error. The sink holds exactly the prefix before the failure.
  * the store is only read; the sink is the caller's `Write`, never a store
    namespace.
* Harness: `real_model_bench restore --materialize-pipeline --threads N` writes
  the exact safetensors header and then streams tensors through the pipeline.
  `--checkpoint base|instruct` scopes a bench run (library/CLI unchanged).
* **Read-induced store mutation removed**: `weftfs checkpoint restore` is now
  read-only in the store by default. It no longer reopens read-write and imports
  the container under `--out` unless `--import` is passed (`--out`/`--out-name`
  are rejected without it). The Python DCP/Orbax readers gained
  `import_restored` (default `False`) and pass `--import` only when asked, so
  the `/dcp-reads/<uuid>` copies seen in the Q5/R15 receipts are no longer an
  implicit cost. Both docs were updated.

### Evidence

* Court `bench/scripts/restore_pipeline_court.sh` +
  `bench/scripts/assemble_restore_pipeline.py`, receipt
  `bench/receipts/restore-pipeline-20261106T204445Z.json` (20/20 checks),
  analysis `bench/RESULTS-restore-pipeline.md`, at WeftFS `eb72c72`. Cached
  Qwen2.5-0.5B pair, source SHA-256 re-hashed in court, one cached store, three
  interleaved repetitions, output removed and `sync` between runs.
* **tmpfs (controlled, base checkpoint; the write is effectively free):**

  | threads | batch ms | pipeline total ms | decode wall ms | transfer ms | write ms | pipeline vs batch |
  | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
  | 1 | 4,913.0 | 4,760.1 | 4,661.0 | 0.0 | 96.7 | 1.032x |
  | 2 | 3,140.9 | 2,844.0 | 2,842.6 | 2,713.1 | 134.3 | 1.104x |
  | 4 | 2,354.2 | 2,157.3 | 2,155.9 | 2,005.9 | 151.0 | 1.091x |

  Pipeline scaling is 1.674x at 2 workers and 2.207x at 4; at 2/4 workers
  `total ≈ decode`, i.e. the write is overlapped. Peak RSS 727,488/857,100/
  853,040 KiB (batch) and 726,936/864,040/897,776 KiB (pipeline).
* **Host bind mount (real path, both checkpoints):** the same work ranged
  **2,526.4–15,325.7 ms** for batch materialize and **2,976.0–17,519.3 ms** for
  the pipeline total across three repetitions; the pipeline's own write ranged
  237.2–11,967.0 ms. That host writeback spread is larger than every mode
  difference, in both directions. **No performance claim is made from the disk
  rows**; they are kept in the receipt precisely to show the variance. On the
  disk the pipeline wins 1.177x at 4 threads (instruct) and loses 1.35x at 4
  threads (base) within the same court.
* Exactness in every mode/thread/destination: 290/290 per-tensor BLAKE3, every
  logical root, restored bytes identical, and the final SHA-256 equal to
  `88c14255...0ed342` (base) / `fdf756fa...88fb7fe` (instruct). Pipeline wrote
  290 tensors and 988,065,536 B per checkpoint, and observed in-flight bytes
  never exceeded 64 MiB + 272,269,312 B (the one oversized embedding).
* Tests `tests/restore_pipeline.rs` **14/14**: byte identity at 1/2/4 threads,
  ordering under skewed decode times, the observed staging bound, one oversized
  task at a time plus an oversized-between-normal case, lowest-index error with
  a deterministic written prefix (synthetic and corrupted-manifest forms), sink
  failure, declared-size errors not stalling later reservations, empty input,
  **no store mutation on a read-only restore** (head, generation, full tree
  digest, `verify --full`), and a real `SIGABRT` child killed mid-write that
  leaves exactly a truncated source prefix and an unchanged store.
* Suites: Rust `cargo test --all-targets` **96 test binaries, 460 passed, 0
  failed, 0 ignored**; FUSE `cargo test --features fuse --all-targets --
  --test-threads=1` **96 test binaries, 502 passed, 0 failed, 2 ignored** (the
  same two pre-existing ignores). That is 15 new tests (`tests/
  restore_pipeline.rs` 14, plus
  `cli_checkpoint_restore_is_read_only_unless_imported`). Python in the python
  court with the rebuilt CLI: `tests/test_dcp.py` **10 passed**,
  `tests/test_orbax.py` **18 passed**. `scripts/check-claims.sh` **393/393**,
  `scripts/check-spec-drift.sh` **42/42**. `verify-all` was not rerun (no frozen
  format, court list, or aggregate input changed).
* Flake disclosure: the first full Rust run failed
  `backend::capabilities_fail_closed` at its read-write
  `Store::open_with_backend(...).unwrap()` on a fixture it had just built. It
  passed **5/5** standalone immediately after, and the full suite rerun was
  **460 passed / 0 failed**. R19 touches no store-open code; this is the same
  intermittent fresh-reopen shape already disclosed in R9 and R18.

### Losses and not claimed

* The pipeline does not make the write faster and cannot control disk
  bandwidth. On this host the disk end-to-end claim is not measurable.
* One worker is only 3.0 % faster on tmpfs; overlap needs at least two workers.
* The 272 MB embedding remains the serial oversized task; the pipeline does not
  split one tensor.
* Full-pair restore still peaks at ~2.0–2.2 GB RSS on the host mount because the
  harness restores both checkpoints in one process.
* No Go parity for the ordered pipeline; no change to the on-disk format, codec,
  CLI restore semantics beyond the opt-in import, FUSE path, or selectors.
* Single host, warm cache, one store, three repetitions; no cold-cache, fleet,
  GPU, or multi-node claim.

## Subphase R20 — Go parity for the R9–R12 storage surfaces (2026-11-06)

The two-implementation guarantee covered the codec, store, contexts, GC, quota,
and FUSE, but the Rust storage work from R9–R12 had no Go counterpart. R20
closes four of those gaps in dependency order on `staging`.

### What is implemented

* **`weft.manifest.v4` overlay writer and compaction (Go).** The previous
  version is the immutable base plus one merged extent; a misaligned edit is
  widened to the enclosing chunk grid with untouched bytes read from the
  previous version; adjacent or overlapping extents fold into the current layer
  while it stays at or below `model.MaxOverlayExtentsPerManifest` (64) and a new
  layer is published only when folding would exceed that bound or the writer's
  configured `MaxOverlayDepth` / `MaxOverlayBytes`. The writer derives the
  canonical table twice — previous table + edit, and the manifest the reader
  will resolve — and refuses `Corrupt` unless both are identical. `Store.Compact`
  re-reads and re-hashes every chunk (one chunk of memory), requires the
  pre-compaction Merkle root, then publishes a canonical v3 table through the
  normal commit protocol. `Truncate` (grow), `ZeroRange`, and range writes publish
  layers; shrinking truncate, `collapse_range`, and `insert_range` are refused
  with the same wording as Rust. CLI: `compact`, `--representation overlay`
  (which requires `--content-identity merkle`).
* **Blob/commit plane split (Go).** `BlobAccess` routes `objects/`,
  `manifests/`, `indexes/`, `packs/`, and `pack-catalog/`; the commit plane owns
  `FORMAT`, `LOCK`, the superblock, `commits/`, `refs/`, quota, and access
  counters. The default local/local layout writes the historical bytes. `local`
  publishes with `rename(2)` over a fsynced temporary file; `object` uses
  `link(2)`. Capabilities are **measured** on every writable open: the blob probe
  publishes a private 192-byte object, HEADs its length, reads a 17-byte
  mid-range slice, and deletes it; the commit probe performs a create-once CAS, a
  matching CAS, and a stale CAS that must be refused with `Busy`. A
  declared-absent `posix` plane is refused for writing (and `Init` leaves no
  `FORMAT` marker), while read-only opens never probe or write. CLI:
  `--blob-backend`, `--commit-backend`, `--blob-root`; `info` reports both
  planes, their roots, and the probe evidence.
* **Physical pack plane (Go).** Identical wire format to Rust: `WPS1` catalog
  segments (magic, version, index, count, 113-byte entries, domain digest),
  `WPR1` root (magic, version, segment count, total, ids, digest), the same four
  domains, segment index masking, and member digests. Packs are published
  before any catalog record names them; the open pack is flushed before every
  commit record; the catalog is bounded and falls back to flat publication
  without failing. CLI: `--pack`, `--pack-target-bytes`,
  `--pack-member-threshold`, `--pack-catalog-bytes`, `--pack-segments`; `info`
  reports catalog truth. GC reclaims unreferenced packs and orphaned segments;
  `verify --full` reports catalog problems and reads through the packed plane.
* **Host calibration (Go).** `weft.calibration.v1` documents are loaded with
  exact schema, key-set, row-set, sample, bound, monotonicity, and digest
  validation and no fallback. The digest is computed over the compact key-sorted
  document with the digest key removed, matching `serde_json`'s BTreeMap form.
  `--planner-cost-model declared|measured` and `--calibration FILE` are accepted
  (`--calibration` requires `measured`); the validated table overrides the baked
  coefficients in tensor planning and rebase pricing.

### Evidence: cross-language byte equality

All values below are asserted by the named tests; they are not estimates.

* **Overlay writer, fixed clock `1700000000000000000` and seed
  `overlay-same-seed`, six 64 KiB chunks + 77 bytes, writes at 100, 150000, and
  400000:** Go and Rust produce the **same overlay manifest id**
  `f04879ebaf27059d16f8f1ef09543c89bf43e5cd30d4479252e0ecf20beca5f9`, the
  **same 605 manifest bytes**, and the **same published head**
  `64103f3b7d1961f415b694c954271e9f363c3e60b7582cb9d1a3051c1b2c3f48`
  (`TestOverlayWriterMatchesRustByteForByte`). Rust `verify --full` accepts the
  Go store.
* **Continuation and compaction both ways:** Rust extends and compacts a
  Go-written overlay; Go extends and compacts a Rust-written overlay; every
  result is read byte-exactly by the other language with `verify --full` clean
  (`TestOverlayGoExtendsRustStore`, `TestOverlayGoCompactsRustStore`,
  `TestOverlayRustCompactsGoStore`).
* **Four split layouts** — local/local, object/object, object-blobs/local-commits,
  local-blobs/object-commits — publish the **same head**
  `6ce4fb88479aa685bcb749f7fba061bf1bb86b0a31de008b1b30d093b9754a9f`,
  **generation 3**, and **manifest**
  `fe6b7cff0e29a36c6a11b7a3797969c84b622da054f6780d041cb22753105e73`
  (`TestSplitPlanesMatchTheSingleBackendStore`). Rust reads and verifies a
  Go-split store; Go reads and verifies a Rust-split store, with matching head
  and manifest id (`TestGoSplitStoreRustReadsAndVerifies`,
  `TestRustSplitStoreGoReadsAndVerifies`).
* **Packs:** Go's 300,000-byte file seals **2 packs / 8 members / 366,270 packed
  bytes with 0 flat members** under the default policy (head
  `6ba667e1…e76a`, manifest `8635b945…af33`). Rust `verify --full` and `cat`
  agree; Go reads and verifies Rust-written packs; Go GC reclamation is then
  re-verified by Rust (`TestGoPacksRustReadsAndVerifies`,
  `TestRustPacksGoReadsAndVerifies`, `TestGoGCReclaimsPacksThenRustVerifies`).
* **Calibration:** a document produced by the Rust `calibrate` command loads in
  Go with the **identical 64-hex digest** and seven measured rows
  (e.g. `12bc635c1620b5d4ed4c47f39c6817fe386dd5263bfe8f7d1676aef367a38cd5`);
  loading re-computes the digest, so acceptance is evidence that Go's JSON
  canonicalisation matches Rust's (`TestRustCalibrationDocumentIsAccepted`).
* **Commits:** WeftFS-Go `fa186a3` (stage 1), `9d78940` (stage 2), `cc988fb`
  (stage 3), `f577cb7` (stage 4), `8670d77` (evidence logging, the measured
  tree of the aggregate above), and `447b702` (NOTES.md only). Parent pins:
  `ae11484`, `d0e0da9`/`9a6156c`, then the final pin. The final pin adds only
  NOTES.md to the measured submodule tree.

### Repository suites at this change

* Go: **168** top-level tests in `store`, **15** in `cmd/weftfs`, and **24** in
  `fusefs`, run as `go test -race ./... -count=1` in the FUSE court: **0
  failures**. **51 new top-level tests** were added (46 store + 5 CLI), plus the
  32-case calibration rejection matrix as subtests. Cross-language tests invoke
  the Rust binary only as an oracle.
* Honest note on skips: the Go court image has no `libfuse3`, so when the shared
  Rust release binary was built with `--features fuse`, oracle tests skip there
  with the loader error. The aggregate runs the same tests in the FUSE court,
  where they execute and pass. A skip is never counted as a pass.
* Rust: `cargo test --all-targets` **460 passed / 0 failed** (96 binaries);
  `cargo test --features fuse --all-targets -- --test-threads=1` **502 passed /
  0 failed / 2 ignored**; WeftCodec **226 passed / 0 failed**.
* Gates: `scripts/check-spec-drift.sh` **42/42**; `scripts/check-claims.sh`
  **399/399**; conformance manifest and coverage unchanged.

### Aggregate coverage

`scripts/verify-all.sh` now runs the MinIO court as its own **29th** court
(`s3_court`) with the same environment-only skip semantics as `ceph_rgw`:
exactly one `s3_court: skip <reason>` line and exit 78 for a non-x86_64 host, no
Docker CLI or daemon, no `curl`, an unobtainable pinned MinIO release or
`debian:bookworm-slim` image, or a Docker network that cannot be created.
Everything after preflight is a hard failure. One helper now handles both
orchestrated courts (pass / explicit skip / fail) and the receipt gains an
`s3_court` receipt pointer. The R20 aggregate
`bench/receipts/verify-20261106T214749Z.json` is **29/29 pass, 0 failed, 0
skipped** at WeftFS `9a6156c` / WeftFS-Go `8670d77`. The failing first R20 run
`verify-20261106T212741Z.json` (28/29) is retained: `rust_store` hit the
pre-existing `tests/quota.rs` writer-lock flake disclosed in R9, R18, and R19;
the test passed standalone immediately after and in the rerun.

### Losses, non-claims, and what Go still lacks

Revised after R24–R27. The original R20 text below was written when the Go
store had no S3 backend, no calibration measurement, no pack/plane courts, and
no index persistence; those are now implemented. The reconciled list is:

* Go **has** a v4 overlay writer and compaction, the physical pack plane, a
  probed blob/commit plane split (`--blob-backend`/`--commit-backend`, all four
  local/object/S3 layouts), `weft.calibration.v1` **loading**, `calibrate`
  **measurement** (a Rust-accepted document with the identical id), a real
  **S3 backend** (SigV4, `If-None-Match`/`If-Match`, the fenced writer), and
  **WFX2** selector-index persistence readable both ways.
* Go **has** S3 and GCS backends (R25/R26), each with the fenced v3 writer and
  all four plane layouts. Go still has **no Azure backend**; there is no
  cross-vendor Go claim beyond S3/GCS.
* Go **has `calibrate --install`** (R27), matching Rust's strict load, temp
  write, fsync, rename, and idempotence.
* Go has **read-only FUSE over S3 and GCS** (R26, 3/3 files byte-identical
  each). There is **no writable FUSE-over-object court**.
* Packs are **never rewritten**: a dead member's bytes stay until the whole pack
  is unreferenced. A sub-member read cannot verify the member digest by design;
  only whole-member reads do.
* Overlay: Go refuses shrinking truncate, `collapse_range`, and `insert_range`
  (matching Rust); there is no streaming overlay writer.
* No performance claim, no physical power loss, no multi-writer, no fleet or
  cloud claim. Crash evidence is the process-fault proxy used elsewhere.

## Subphase R21 — Azure Blob and GCS service-account auth (2026-11-06)

Two backend gaps are closed: there is a real Azure Blob REST backend, and the
GCS backend can authenticate with a service-account key instead of a bearer
token supplied out of band.

### What is implemented

**Azure Blob** (`src/azure.rs`, 2,679 lines with tests)

* Blob REST over the existing blocking `ureq` client: `Put Blob` with
  `x-ms-blob-type: BlockBlob` (`If-None-Match: *` for create-once, `409
  BlobAlreadyExists`; `If-Match: <ETag>` for compare-and-swap, `412`),
  `Get Blob` (whole and `Range`), `Get Blob Properties`, `Delete Blob`,
  `List Blobs` (`restype=container&comp=list`), and container create/properties.
* `BlobPlane`, `CommitPlane`, and `StorageBackend` implementations. The public
  `ControlRecord.token` remains the **content token** of the record's bytes; the
  service ETag is the private transport token, so a caller can never confuse the
  two. `--backend azure` also works for `--blob-backend`/`--commit-backend`.
* Credentials: **SharedKey** (HMAC-SHA256 over the documented canonicalised
  headers and resource, implemented with `sha2` only), a **SAS** query string,
  or **anonymous** for an emulator. The three forms are mutually exclusive and
  refused when mixed.
* The R17/R18 fenced writer is ported verbatim: monotonic fencing token in the
  `LOCK` blob, one conditional write before each publication naming the pending
  head, renewal when one publication budget remains, refusal after the writer's
  own deadline, and a content-addressed conditional tombstone for release.
* Endpoint behaviour is **measured**, not assumed: the writable-open probe
  requires create-once, a matching `If-Match`, and a **refused stale**
  `If-Match` with the winner's bytes surviving. It also measures whether the
  endpoint compares the quoted ETag or a dequoted value (the Ceph RGW lesson
  from R16). An endpoint that accepts a stale value is refused
  `UnsupportedVersion`.
* Resource canonicalisation is `/{account}{request path}`. On a path-style
  endpoint (Azurite) the account appears twice; that is what both Azurite and
  Azure recompute. The first court run used `/{account}/{container}/{blob}` and
  every request failed authentication (`azure-court-20261106T221522Z.json` is
  retained for that defect).

**GCS service-account auth** (`src/service_account.rs`)

* RS256 JWT-bearer flow with no RSA dependency: strict DER for PKCS#8
  (`BEGIN PRIVATE KEY`) and PKCS#1 (`BEGIN RSA PRIVATE KEY`), fixed-width
  big-integer Montgomery arithmetic, EMSA-PKCS1-v1_5 with the SHA-256
  `DigestInfo` prefix, and CRT signing. `#![forbid(unsafe_code)]` still holds.
* The key is proved at load time: `p·q == n`, the CRT result equals
  `m^d mod n`, and the public exponent inverts the private operation. A key that
  fails any check is refused `InvalidArgument` before it can sign.
* Assertion: `{"alg":"RS256","typ":"JWT"}` plus `kid` when `private_key_id` is
  present, and `iss`/`scope`/`aud`/`exp`/`iat` with a one-hour lifetime.
* Token exchange with a single-slot cache and a refresh window of
  `min(300 s, lifetime/2)`. One exchange under concurrency. Provider refusals
  are typed (`Unauthorized` for 400/401/403, `Corrupt` for an unreadable or
  lifetime-less reply, `UnsupportedVersion` for a non-Bearer token type, `Io`
  for an unreachable endpoint).
* Explicit refusals, with what would be required: a non-`service_account`
  credential (`authorized_user` needs a refresh-token flow), an encrypted
  private key (PBES2/PBKDF2), a non-RSA key, a modulus outside 2048..=8192 bits,
  and an inconsistent key.
* Key material, the assertion, and the access token are redacted from `Debug`
  and never appear in an error message. Diagnostics name only the key's
  SHA-256 fingerprint and modulus width.

### Evidence

* **RFC 7515 Appendix A.2.** `tests/service_account.rs` signs the published
  RS256 signing input with the published 2048-bit key and asserts the exact
  published signature octets, in both PKCS#1 and PKCS#8 encodings. This is the
  external cross-check of the RSA and JWS code; it needs no network.
* **Azure emulator.** `bench/scripts/azure_court.sh` runs pinned Azurite
  `3.35.0`, image
  `mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`,
  as a sibling container with the same hard limits (3g/3g, 1024 pids, 4 cpus,
  bounded tmpfs, `cap-drop=ALL`, `no-new-privileges`). Court receipts
  `azure-court-20261106T231026Z.json` (current) with
  `azure-court-20261106T230714Z.json` and
  `azure-court-20261106T221838Z.json` as earlier runs of the same workload
  (same root and counts):
  * local and Azure runs have the **same content root**
    `0a8198f56ed59a11d3a704b05814232359e4fb6544d4dc241d41d2fd59756c5f`, the
    same generation sequence 1..21, the same verified bytes (393,216) and the
    same content digest.
  * the `superblock` ETag differs at every sampled publication (three samples in
    the current run: `"0x20B1B3025C57900"`, `"0x211D4915F03EF60"`,
    `"0x276CB5EE4E027E0"`), which is direct evidence that each head went
    through `If-Match`.
  * process-fault crash proxy: 3/3 phases old-or-new, 0 partial states.
  * backend suite **14 passed, 0 failed**; shared fenced-writer invariants
    **7 passed, 0 failed** (`tests/support/fencing.rs`: expired-lease stop,
    SIGSTOP reclaim then `Busy`, replaced-lock release, renewal, monotonic
    token).
  * four of the backend tests run against the live emulator; ten run against an
    in-process Azure service that independently recomputes the SharedKey
    signature, so a wrong canonicalisation or an unsigned header fails.
* **GCS auth court.** `bench/scripts/gcs_court.sh` now runs
  `cargo test --test service_account` every time and records an `auth` object in
  `gcs-court-20261106T230852Z.json` (current; `gcs-court-20261106T225115Z.json`
  is the earlier run with the same result): **14 passed, 0 failed**, status
  `no_credentials`, `live_probe: null`. The emulator court itself is unchanged
  (same content root, commit sequence, fencing 7/7). A retained failing receipt
  `gcs-court-20261106T225005Z.json` records one auth test asserting a project
  from the key file while the court environment set `WEFT_GCS_PROJECT`; explicit
  project precedence is what is asserted now.
* **Aggregate.** `bench/receipts/verify-20261106T225457Z.json` is **29/29
  courts pass**, 0 failed, 0 skipped, at WeftFS `132e713`. The standalone court
  receipts above were re-generated from committed trees after the aggregate:
  the GCS one records WeftFS `c283131` and the Azure one `d67855c`; the backend
  and auth code is unchanged from `132e713`, and the Azure workload root is
  identical across all three runs. The new Azure and GCS
  auth courts are standalone like `gcs_court`, so they are not counted in that
  29; their own receipts above are the evidence.
* Repository suites, counted from the aggregate's logs: `rust_store`
  (`cargo test --all-targets`) **101 suites / 513 passed / 0 failed**;
  `rust_fuse` (`cargo test --features fuse --all-targets`) **101 suites / 555
  passed / 0 failed / 2 pre-existing ignored**. New tests: 8 Azure unit, 14
  Azure backend (10 in-process fake + 4 live Azurite), 7 Azure fencing, 6
  service-account arithmetic/DER, 14 service-account integration.
* Gates: `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass in the
  same aggregate. No spec changed: both additions are transport/auth surfaces,
  not wire formats, so the subproject spec mirrors are untouched.

### Losses and non-claims

* **Azure SAS acceptance is measured; anonymous acceptance is not.** R23
  (`bench/receipts/azure-20261106T235445Z.json`) proves a container-scoped
  service SAS is accepted by Azurite 3.35.0 for read-only opens, object reads,
  `verify(true)`, and blob writes. The same SAS is refused at the
  `HEAD ?restype=container` check a writable open performs, so writable opens
  need SharedKey (proven) or an account SAS (untested). Anonymous is refused on
  a private container; acceptance needs a real account with container ACL
  `blob` or `container` and remains unproven. Neither form is claimed as
  accepted on a real Azure deployment.
* **No live cloud run.** Azure has never been contacted; GCS has never received
  a token exchange. The precise gap is a credential and an account:
  * GCS needs a service-account JSON key with Storage permissions. Provide it
    with `WEFT_GCS_SA_KEY=/path/key.json bench/scripts/gcs_court.sh`; the script
    copies it to the gitignored court cache and runs
    `examples/gcs_auth_probe.rs`, which performs one real exchange and prints
    only the fingerprint, modulus width, and remaining lifetime. Until that is
    run, `no_credentials` is the whole claim.
  * Azure needs a storage account and a SharedKey or SAS credential. The same
    `--azure-endpoint`/`--azure-account`/`--azure-key` path applies; Azurite is
    not Azure, and untested differences include service-version acceptance,
    large-block-blob limits, `x-ms-*` header edge cases, and listing paging.
* **RS256: constant-time arithmetic, audit still outstanding.** The private
  operation is now a **Montgomery ladder** over the existing fixed-width
  big-integer code: both CRT half-exponentiations run exactly
  `modulus.bit_len()` steps with branch-free mask swaps; Montgomery
  multiplication is fixed-width CIOS with unrolled carry propagation and a
  borrow-mask conditional subtraction; the `m1 - m2 mod p` recombination is
  branchless; and the final `h*q + m2` uses fixed-width low-limb loops. A
  mandatory **fault check** recomputes `sig^e mod n` with the public exponent
  through a value-independent path and refuses to return a signature that does
  not reproduce the EMSA-PKCS1-v1_5 encoding (this also covers CRT
  recombination faults; RSA is multiplicative, so one faulted half can reveal
  a factor of `n`). The RFC 7515 A.2 vector and all 14 service-account tests
  still pass byte-for-byte.
  A `dudect`-style timing smoke test (`bench/receipts/rsa-timing-20261108T041500Z.json`,
  20,000 signs per class × 3 rounds, randomized interleaving, two message
  classes with digest prefixes `0000`/`ffff`) measured medians within 0.05 %
  and worst |Welch t| **4.04** (rounds 2.91 / 4.04 / 3.64). One earlier
  single-round run measured **6.24** (receipt
  `rsa-timing-20261108T040000Z.json`, retained): the variation is exactly why
  this is a smoke test, not a proof. **Residual, not claimed:** no formal
  side-channel audit, one microarchitecture and one compiler, no
  cache/branch-predictor/frequency adversary model, and `BigUint::rem` still
  runs on public message bytes. The Go GCS backend already uses the vetted
  `crypto/rsa` (`rsa.SignPKCS1v15`); this work is Rust-only.
* **Azurite data is a tmpfs.** The court proves protocol behaviour, signature
  acceptance, conditional-write safety, and fencing — not media durability,
  replication, zone failure, or physical power loss. `durable_fsync` for both
  backends means "the provider acknowledged the write".
* **Fencing is still two conditional writes on two objects.** The fence and the
  head compare-and-swap are not one atomic operation (not expressible on plain
  S3/Azure/GCS semantics); a lost network reply retains the usual at-least-once
  ambiguity. No multi-writer, clock-skew, or fleet claim.
* **Service-account assertions are re-signed per token**, not cached; a token is
  cached for its lifetime and refreshed in the bounded window. A clock skewed by
  more than the one-hour assertion lifetime will be refused by the provider.
* **At R21 there was no Go parity** for Azure or S3/GCS and no Go
  service-account flow. R25/R26 add Go S3, Go GCS, and Go RS256
  service-account auth; **Go Azure remains absent**. `src/azure.rs` is still
  Rust-only.
* The Azure backend has **no performance claim** and was not added to
  `scripts/verify-all.sh`. The GCS auth suite likewise runs inside the existing
  standalone `gcs_court`, not the aggregate.

## Subphase R22 — 3FS transports: POSIX and USRBIO (2026-11-06)

The store can now run over DeepSeek 3FS through two transports that share the
normative layout: `posix` (positional I/O through the 3FS FUSE mount) and
`usrbio` (the hf3fs user-space ring API for the immutable data plane).
**No 3FS deployment is run by this repository**, so nothing here is a 3FS
throughput, latency, RDMA, durability, or multi-node claim.

### What is implemented

**Buffer-oriented plane boundary** (`src/planes.rs`, `src/backend.rs`)

* `BlobPlane::read_into` / `StorageBackend::read_into` fill a caller-owned
  buffer. The local and 3FS backends implement it as one positional `pread`
  into that buffer — no intermediate `Vec`.
* `read_ranges(&[RangeRead]) -> Vec<Vec<u8>>` expresses a batch; output order is
  input order and each entry keeps the exact `read` contract (short tail at EOF,
  `InvalidArgument` past EOF).
* `publish_file` fills its staging file through the write ring when the
  transport is `usrbio`; `fsync` + `rename` + directory `fsync` still order the
  publication, so the commit protocol is unchanged.

**`src/threefs/`** — the only module allowed to use `unsafe`
(`src/lib.rs` is `#![deny(unsafe_code)]`, and `tests/threefs.rs` fails if
`unsafe` appears anywhere else under `src/`).

* `ffi.rs` transcribes `hf3fs_usrbio.h`: `hf3fs_iov` (312 B), `hf3fs_ior`
  (600 B), `hf3fs_cqe` (24 B), and the eleven entry points. Sizes and field
  offsets are pinned by `const` assertions here and by `_Static_assert` in the C
  conformance library, so a transcription error fails a build.
* `dlopen(RTLD_NOW|RTLD_LOCAL)`; a missing library or missing symbol is
  `UnsupportedVersion` naming every missing symbol. No fallback to POSIX.
* Registration lifetime is owned by a `Drop` guard that deregisters and closes
  on every path; one mutex serialises prep/submit against wait, which is what
  makes `Send`/`Sync` sound; destinations reach the ABI only through
  `region_contains`, so `[ptr, ptr+len)` is inside the region by construction.
* Bounds: entries ≤ 4096, one request ≤ 32 MiB, region ≤ 1 GiB.
* `Usrbio::counters()` reports submissions, requests, completions, bytes,
  direct versus staged reads, and registrations; `ThreeFsStatus` carries the
  library path and the measured `hf3fs_is_hf3fs` answer.
* `BackendKind::ThreeFs { config }`, `--backend 3fs`, `--threefs-mode`,
  `--threefs-mount`, `--threefs-lib`, `--threefs-entries`, `--threefs-io-bytes`,
  `--threefs-require-mount`. An unrelated `.so` is refused by name.
* Declared capabilities carry the literal text "Declared, not measured", and a
  test asserts that wording is present.

### Court

`scripts/threefs_court.sh` builds `bench/threefs/hf3fs_shim.c` — a conformance
implementation of the published ABI over real POSIX `pread`/`pwrite` with
layout assertions — then runs `tests/threefs.rs` **13/13** single-threaded:

* POSIX transport byte-identical to local: commits, chunks, manifests, indexes,
  generation, snapshot root, full verify, reopen;
* the shim's `hf3fs_is_hf3fs` is `false` and the adapter records it;
* 300 requests over 128-entry rings produce exactly 3 submissions, exact bytes;
* partial completion (two CQEs per wait) still yields exact bytes;
* short read at EOF, `InvalidArgument` past EOF, and an empty request;
* direct path inside the region (`direct_reads=1`, `staged_reads=0`) versus a
  staged read;
* injected `prep_io` failure propagates as `Io` with zero live registrations
  afterwards (the `Drop` guard ran);
* a full store round trip over the ring is byte-identical to local, including
  `verify --full` and a read-only reopen;
* `unsafe` is confined to `src/threefs/`;
* unbounded configuration is refused, and `require_3fs_mount` refuses the shim.

Receipt `bench/receipts/threefs-20261106T232643Z.json`, analysis
`bench/RESULTS-threefs.md`, design `docs/THREEFS.md`.

### Measured (single host, warm cache, shim)

300 × 256 KiB range reads, 5 reps: positional **9.899 ms**, ring **11.159 ms
(1.127× slower)**; 1,500 requests in **50 submissions**; bytes identical. Store
round trip 100 × 16 KiB: POSIX 11.31 ms, USRBIO 12.28 ms (+8.5 %); same
generation 101 and same snapshot root; reopen + verify 1.69 ms. Peak RSS
397,196 KiB.

The loss has three named causes, all recorded in the receipt: the shim performs
`pread` per request inside `wait_for_ios` and has no RDMA or kernel bypass; the
adapter opens and registers one descriptor per read (`registrations = 1500`); and
the positional side reuses destinations while the ring side returns owned
vectors.

### Environment fact

The court's `/tmp` is a Docker tmpfs mounted `noexec`, so `dlopen` of a shared
object there fails with *"failed to map segment from shared object"*. The court
builds the shim under `./.docker-cache/`, an executable bind mount.
`scripts/docker-court.sh` now forwards `WEFT_3FS_LIB`, `WEFT_3FS_SHIM`,
`WEFT_3FS_SHIM_DIR`, `WEFT_THREEFS_REPS`, `WEFT_THREEFS_RECEIPT`,
`WEFT_THREEFS_LOG`, and `WEFT_STAMP`.

### Not done

* No real 3FS deployment, RDMA, kernel bypass, multi-node consistency, or
  power-loss scenario.
* No registration cache: each read opens and registers a descriptor.
* `read_batch` still returns owned vectors at the store boundary; the zero-copy
  property covers storage → shared region only.
* No Go 3FS transport.
* The writer lock remains the store's single-writer `LOCK` file on the POSIX
  plane; USRBIO does not add or replace coordination.

## Subphase R23 — Azure SAS acceptance (2026-11-06)

R21 shipped the Azure SAS and anonymous credential forms with
request-construction tests only. R23 mints a real container-scoped **service
SAS** (15-field string-to-sign for `sv=2018-11-09`, HMAC-SHA256 over the account
key, base64 with `+ / =` escaped) and requires Azurite to accept or refuse it.
Code revision `dcbe066`.

### Environment and command

Azurite Blob `3.35.0`,
`mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`
(image id `sha256:03f7cb26ef…`), tmpfs `/data` 1 GiB, sibling container on a
private network. Test court `weft/rust-court:1`: memory/swap 3g/3g, pids 1024,
cpus 4, nproc 2048, `--cap-drop=ALL`, `no-new-privileges`, tmpfs `/tmp` 1 GiB.

```sh
bash bench/scripts/azure_court.sh
```

The inner court ran `cargo test --test azure_backend -- --test-threads=1
--nocapture`, `cargo test --test azure_fencing -- --test-threads=1
--nocapture`, then `cargo run --release --quiet --example azure_court`. The image
was obtained; the court did not skip.

### Result

* `azure_backend`: **19 passed / 0 failed** (12.93 s). New live tests:
  `live_sas_token_authenticates_and_preserves_the_store`,
  `live_read_only_sas_refuses_a_writable_open`,
  `live_a_tampered_sas_is_rejected`,
  `live_anonymous_is_refused_on_a_private_container`, and
  `live_sas_authorizes_blob_data_but_not_container_properties`.
* `azure_fencing`: **7 passed / 0 failed** (15.02 s), unchanged.
* End-to-end court: status `ok`, local and Azure content digest `2e3b9902…`,
  generation 21, `content_root_match` / `generation_match` /
  `generation_sequence_match` all `true`, peak RSS 7,488 KiB.
* A service SAS with `sp=rcwdl` **was accepted** for a read-only store open
  (FORMAT, commits, superblock, manifests, objects), an object read, a full
  `verify(true)`, and a create-once blob PUT + GET.
* The same SAS **was refused** on a writable open at
  `HEAD ?restype=container` (Get Container Properties): class `Unauthorized`,
  `Azure denied reading container \`…\``. Azurite returns 403 for that call
  under `sp=r`, `sp=rcwdl`, and `sp=racwdl` alike, while blob data operations
  are accepted when `w` is present. A writable open therefore needs SharedKey
  (proven) or an account SAS (not tested). This is recorded as a limitation;
  the container-properties check was not skipped to make SAS writable.
* `sp=r` is refused earlier on a writable open, at the conditional-write probe:
  `Unauthorized: Azure denied PUT \`tmp/conditional-probe-…\``.
* A signature tampered from `sp=r` to `sp=rcwdl` is refused at the FORMAT read
  (Azurite `AuthorizationFailure`) as `Unauthorized`.
* Anonymous is **refused** at the FORMAT read of a private container
  (`AuthorizationFailure`, `Unauthorized`). Acceptance remains unproven.

### Defect found and fixed

`Store::open_impl` wrapped every non-`Io` FORMAT-read failure as `Corrupt`, so a
rejected credential was reported as damaged data (`Corrupt: cannot read FORMAT:
Unauthorized: …`). The tampered-SAS test exposed it. `src/store.rs` now preserves
`Class::Unauthorized` as well as `Class::Io`; a genuinely missing or mismatched
FORMAT remains `Corrupt`. The failing run that found it
(`azure-20261106T234510Z`, `15 passed; 2 failed`) is retained.

### Receipts

`bench/receipts/azure-20261106T235445Z.json` (sha256
`f4e29469…d9f551`) and `bench/receipts/azure-court-20261106T235445Z.json` (sha256
`13809ef7…686a8d`), with `.tests.log`, `.tests.txt`, `.fencing.log`, and
`.fencing.txt`. Analysis: `bench/RESULTS-azure-sas.md`. The retained failing
run is `azure-20261106T234510Z.*`.

### Losses and non-claims

* No live Azure account: Azurite is not Azure. Service-version acceptance,
  block-blob limits, `x-ms-*` edge cases, and listing paging are untested.
* No account-SAS acceptance test.
* Anonymous acceptance is unproven; only refusal on a private container is
  proved. Proving it needs a real account with container ACL `blob` or
  `container`.
* Writable SAS opens are refused by design of the endpoint's container check,
  so SAS is effectively read-only on this backend.
* One host, loopback, tmpfs, warm caches; no performance, power-loss,
  multi-node, or fleet claim.

## Subphase R24 — Go calibrate measurement and packed/split mount coverage (2026-11-06)

Two named Go gaps are closed. Work is in WeftFS-Go `staging` `af8ad88`; the
parent pins that revision.

**Go `weftfs calibrate` (measurement).** `store/calibrate.go` measures the
planner classes with the shipped Go encoders/decoders (raw, zstd, modes 9, 10,
15, 11, 14) at two sizes with medians, a warm-up, and incremental delta rows
(base decode subtracted), then fits the same affine
`fixed_ns + decoded_bytes*ns_per_mib/2^20` model and rounding as Rust.
`sparse-delta` reuses the weight-delta row; `kv-cache` and `activation` stay
declared. The command is `weftfs calibrate [--out|--from|--reps|--large-mib|
--small-kib]`, with `--json` output shaped like Rust.

Rust accepts the Go document: `weftfs --json calibrate --from
go-calibration.json` exits 0 with schema `weft.calibration.v1`, 10 rows
(7 measured) and calibration id
`b07fbff6f59d6bbd284f15520673ac5d51635a7a2d86d1bd230c9f0b56a9bec7`, identical
in both languages. Automated as `TestRustAcceptsGoCalibrationDocument`, which
ran rather than skipped.

**Real defect found and fixed.** Go's `q8BlockSize` preferred the *smallest*
dividing mode-10 block (16) while Rust prefers the largest (256). At 8 MiB any
`q8_0` write failed `LimitExceeded: WCB item count 1000001 exceeds limit
1000000`, and mode-10 bytes diverged from Rust. The order is now
`256,128,64,32,16`, matching Rust's `q8_0_block_size`.

Measured, host-local and not portable (default 7 reps, 128 KiB / 16 MiB, go
court image, kernel 7.3.1, 4 CPUs, wall 4.422 s), `ns_per_mib` under the Go
codec-level method: raw 819,494; zstd 1,115,176; float-tensor 2,837,430;
tensor-sharded 3,475,654; tensor-tiled 4,828,817; weight-delta 6,003,877;
semantic-delta 7,206,540. Go measures codec frames while Rust measures store
reads, so the divergence from the baked Rust table is method and machine, not a
speed claim. Two minimum-size runs drifted ≤11.1 % on slope and ≤57,648 ns on
intercept.

**Packed and split mount coverage.** `fusefs/packed_mount_test.go` builds three
layouts with the Go CLI — `--pack`, `--blob-backend object --commit-backend
local`, and `--pack` over that split — creates 9 files (12 B…300,000 B) and 3
directories in each, asserts `info --json` reports the expected pack and plane
state, mounts a real read-only daemon, compares every file byte-for-byte and by
SHA-256, performs an 8 KiB `ReadAt` at offset 100,000, checks listing digests
(identical across layouts), unmounts, and runs `verify --full` three times.

Court: `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race
-count=1 ./..."` → **0 failures**; top-level tests store 173, `cmd/weftfs` 17,
`fusefs` 25 (was 168/15/24). `gofmt`, `go vet`, and a CGO-disabled build are
clean.

**Not done:** no Go `calibrate --install`; the calibration exercises the codec,
not a store-side mode-15/14 read path (Go has no writer for those); the FUSE
coverage mounts read-only; no power-loss, multi-host, or fleet claim; a
calibration is host-local by construction.

## Subphase R25 — Go S3 backend (2026-11-06)

The largest remaining dual-implementation gap is closed: WeftFS-Go has a real
S3 backend with SigV4, conditional publication, and the fenced writer. Work is
in WeftFS-Go `staging` `c421590` (tested code `3e14f26`); the parent pins it.

**What is implemented.**

* `store/s3client.go` — SigV4 over `net/http`, standard library only. Canonical
  path/query, every body hashed, no `UNSIGNED-PAYLOAD` path. Pinned by the AWS
  published "GET Object" vector plus a payload-digest test. Config precedence is
  `--s3-*` flags, then `WEFT_S3_*`, then `AWS_*`.
* `store/s3blob.go` — create-once `PUT` with `If-None-Match: *` (identical bytes
  idempotent, different bytes `Exists`), whole and ranged `GET` (an ignored
  `Range` is sliced locally; `416` is EOF), `HEAD` length, `DELETE`, and
  `ListObjectsV2` with continuation tokens, under the same 1 GiB object bound.
* `store/s3commit.go` — control records create-once or `If-Match` CAS, and the
  8192-byte two-slot superblock via read-modify-write + `If-Match` with bounded
  retry. The writable-open probe requires a matching conditional write to
  succeed **and** a stale one to be refused with the winner's bytes intact; an
  endpoint that ignores either form is `UnsupportedVersion`.
* `store/s3fence` (fenced writer ported verbatim): `weftfs-s3-lock v2` with
  owner, monotonic fence, sequence, absolute deadline, renewal stamp, and the
  pending head. Fence+1 takeover, a conditional `LOCK` rewrite as the last gate
  before the head CAS, renewal at one publication budget, refusal after the
  writer's own deadline, and a content-addressed tombstone release under
  `If-Match` — never an unconditional delete.
* `store/control.go` — `--backend s3`, `--blob-backend s3`, `--commit-backend
  s3`, and all four split layouts. Snapshots, GC, verify, quota, access
  counters, checkpoints, and packs are unchanged.

**Measured If-Match form.** MinIO accepts the quoted ETag as returned
(`S3 If-Match form measured at open: quoted-as-returned`, also in the receipt as
`measured_if_match_form`). The dequoted form (Ceph RGW's literal comparison,
found in R16) is implemented but **unexercised** in the Go court.

**A real defect the full suite caught.** Control-record replacement staged under
`tmp/tmp` (and would have created `access/tmp`); fixed in `6fc87cc`, failing log
retained.

**Courts.** `scripts/go-s3-court.sh` (and `--focused-only`) start the official
MinIO release `RELEASE.2025-04-22T22-12-26Z` (binary sha256 `53e2a2cb…bb817c`)
as a sibling container under the usual hard limits, with a tmpfs data directory
and a private bridge, path-style over `http://<container>:9000`.

* Focused (go court): `gofmt -l`, `go vet`, `CGO_ENABLED=0 go build` clean;
  **17 pass / 0 fail / 1 skip** (the skip is a crash-child helper).
* Full `go test -race -count=1 ./...` (fuse court): all four packages ok,
  **0 failures** — `cmd/weftfs` 24.8 s, `fusefs` 7.5 s, `model` 1.0 s, `store`
  116.9 s.
* Cross-language `scripts/go-s3-xlang.sh`: **17/17** checks. Go init + files +
  snapshot → Rust `verify --full`, same head, byte-exact reads including a
  70,000-byte file; Rust init + files → Go `verify --full`, same head,
  byte-exact; then Go unlinks and runs `gc --apply` on the Rust-written store
  and both implementations agree the file is gone.

Receipts live in `WeftFS-Go/bench/receipts/go-s3-20261107T005722Z.json` and the
focused/full/xlang logs beside it; `WeftFS-Go/NOTES.md` §R25 records commands,
counts, host-local request counters (writer 110 requests / 30 writes / 101,104 B;
reader 68 requests), and every limitation.

**Not done at R25** (closed by R26/R27 where noted). No Go GCS backend then
(GCS landed in R26); no Azure backend; no FUSE-over-S3 mount court then
(read-only FUSE over S3/GCS landed in R26); no AWS, multi-node,
replication, media, or power-loss claim; MinIO is loopback on tmpfs; the fence is
still two conditional writes on two objects, not one atomic operation; the Ceph
RGW dequoted form and the pack-plane `ReclaimUnreferencedPacks`/`VerifyPacks`
are unexercised on S3; no performance claim.

## Subphase R26 — Go selector-index persistence (WFX2) (2026-11-06)

Go now persists its selector index in the same `WFX2` segment format Rust
writes, so a reopen no longer replays and re-indexes the whole state. Work is in
WeftFS-Go `staging` `39b58f2`; the parent pins it.

**Implemented.** `index/` ports Rust's `StateIndex` and the WFX2 codec
(presence/equality attribute maps, `(subject,predicate)` relation edges,
`tagged_as` tags; equality keys are the canonical WCB `["weft.eq.<tag>", v]`, and
numeric families share `["weft.eq.num","<decimal>"]`). `resolver/indexed.go`
ports `candidates_for` and keeps `evalExpr` authoritative for every candidate.
`store/index.go` writes a segment after **all four** publish sites (`init`,
`commitOps`, `publishGroupOps`, `publishQuotaUsageOp`); a write failure is a
note and never fails the commit. Load validates the commit key, generation, all
three counts, state shape, digest and canonical ordering, and rejects with a
note + rebuild otherwise. Only complete indexes are written. Read-only opens
never write. `IndexStats{ DiskLoads, Builds, Rejects, Writes, WriteErrors,
GCDeleted, Notes }` is the observable surface. GC deletes segments not named by
the head, a snapshot, or a checkpoint (grace honoured) and reports them in
`GCReport.DeletedIndexes`; `verify` never reads segments.

**Byte layout, measured on a real 10,000-object segment.** 2,423,650 bytes —
the same length as Rust's own 10k WFX2 file. Header is 72 B
(`WFX2 | u32 version | commit[32] | u64 generation | u64 objects | u64
relations | u64 entries`), then sorted fixed-width sections, then
`BLAKE3("weft/index/v2", preceding bytes)`. Consistency of length is not used
as an equality proof; the cross-language tests below are.

**Cross-language, real binaries in the fuse court.**

* Rust loads a Go-written segment: Go writes; Rust `query` agrees. Then the Go
  segment's `tags` section (and separately `attr_eq`) is emptied and re-sealed
  with a valid digest: Rust answers **0 matches** where a rebuild would have
  returned matches — proof that Rust decoded and trusted the Go bytes. Bytes
  restored, Rust answers again.
* Go loads a Rust-written segment: Rust writes init/creates with string and u64
  attributes, `tagged_as`, `links`, and a snapshot; Go opens read-only and
  answers five query shapes with `DiskLoads=1, Builds=0, Rejects=0`, equal to the
  scan oracle for two principals. `DecodeFile`→`EncodeFile` of the Rust file is
  byte-identical.

**Tests.** `go test -race -count=1 ./...` in the fuse court: all packages ok
(`store` 112.4 s). Top-level test functions: store 197, fusefs 25, index 4,
model 6, `cmd/weftfs` 18. Nine corruption modes each assert `Rejects≥1`,
`DiskLoads=0`, `Builds≥1`, a note naming the index, unchanged answers versus the
scan oracle, and a clean verify: bad magic, wrong version, wrong commit, wrong
generation, wrong object count, bad digest, truncated, trailing bytes, and WFX1
magic.

**Measured, host-local, synthetic, warm cache, `/tmp` tmpfs, 5 samples, two
independent runs.** With the segment present, the first relation query after
reopen is **7.749 / 8.042 ms** versus **16.838 / 16.852 ms** rebuilt
(ratio 0.460 / 0.477); the first tag query is **16.583 / 16.884 ms** versus
**24.841 / 23.851 ms** (ratio 0.668 / 0.708). Cold build 8.5–10.4 ms; write
including build 25.4–25.7 ms for 2,423,650 B; peak RSS 419–423 MiB.

**Published losses.** The index is rewritten per commit, so its cost is O(state)
in time and bytes (~17 ms on top of the build at 10k objects here); absolute
times are not comparable to Rust's different fixture; queries that cannot narrow
(`select true`, `score > 5`) still pay the load for nothing and are **not
measured**; packing makes pack layout depend on commit timestamps, so
`TestPackedLayoutIsDeterministic` now pins its clock; `RequireOne` and
saved-context resolution still scan; there is no cross-language proof for
I64/Timestamp attribute keys (the Rust CLI cannot author them); the default GC
grace is 3600 s; and there is no CLI switch to disable segment writes.

**Not done.** No index in `RequireOne`/saved-context paths; the in-memory index
cache is not persisted; single host, synthetic fixture, warm cache — no
device, power-loss, or fleet claim.

## Subphase R27 — one conditional write for the fenced head (2026-11-07)

**Status: implemented and tested on MinIO, Ceph RGW, fake-GCS, Azurite, and Go
MinIO.** No wire format, codec mode, manifest, commit record, or superblock
layout changed.

### The defect, as found in source

R17/R18 wrote the fence as one conditional `LOCK` rewrite and the head as a
*second* conditional write on the `superblock`. An external review identified the
interleaving no existing test covered: A's fence succeeds, A is suspended before
the head CAS, A's lease expires, B takes over the fence without publishing a new
head, A resumes, and A's head CAS still succeeds because the head is the value A
read. The `SIGSTOP` tests only stopped a writer *before* the fence.

### What changed

* The lock record is now `weftfs-s3-lock v3` / `weftfs-gcs-lock v3` /
  `weftfs-azure-lock v3`: `owner`, `fence`, `sequence`, `lease_deadline_ns`,
  `renewed_ns`, `pending_generation`, `pending_commit`, **`pending_commit_len`**.
* **The head is published by writing that record**, with `If-Match` (S3/Azure) or
  `ifGenerationMatch` (GCS) on the bytes this handle last published. One
  conditional write carries both the fence and the new head. A takeover rewrites
  the same object, so the stale write is `412 -> Busy`.
* The two-slot superblock is an **attested cache** on an object-store commit
  plane: written after the record, never moved backwards by a cache refresh,
  merged as the authoritative head by recovery, and repaired on a writable open.
  A cache *ahead* of the record is refused `UnsupportedVersion` (mixed-version
  store). Local/POSIX backends keep the superblock as the publication point:
  `flock` cannot be taken by a suspended process.
* Every non-publication record write (pre-commit fence, renewal, release
  tombstone) carries the current head forward. Tombstones are
  `weftfs-<backend>-lock-released v2` with the head; v1 tombstones stay readable.
* v2 records stay readable (`pending_commit_len` reported as 0 = unknown); v1
  reads as fence 0. A writable open upgrades an expired/released v2/v1 record to
  v3 in the same takeover conditional write; a live v2 record still refuses the
  open `Busy`.
* A local own-deadline re-check runs between the fence and the commit record, so
  a writer suspended past its lease leaves no unpublished commit tail.
* Implementation: Rust `src/writer_fence.rs`, `src/s3.rs`, `src/gcs.rs`,
  `src/azure.rs`, `src/backend.rs`, `src/planes.rs`, `src/recovery.rs`,
  `src/store.rs`, `src/fence_pause.rs`; Go `store/s3client.go`,
  `store/s3commit.go`, `store/control.go`, `store/recovery.go`, `store/store.go`,
  `store/fence_pause.go`.

### The acceptance test

`tests/support/fencing.rs::paused_after_the_fence_cannot_publish_after_a_takeover`
(shared by the S3, GCS, and Azure suites) and
`WeftFS-Go/store/s3_integration_test.go::TestS3FencedWriterPausedAfterFenceCannotPublish`.

1. A child opens and fences, then suspends itself at the hook
   `WEFT_FENCE_PAUSE_AFTER_FENCE=1` (it first writes
   `WEFT_FENCE_AFTER_FENCE_READY` with pid, generation, and head). Rust uses
   `raise(SIGSTOP)`; Go blocks on a named FIFO because its runtime does not
   deliver a self-sent `SIGSTOP` as a group stop in this court. The hook is inert
   unless armed and fires at most once per process.
2. The parent lets the lease expire and writer B takes over the fence **without
   publishing a head**.
3. The parent resumes A (`SIGCONT` in Rust, one byte on the FIFO in Go). A's
   publication is `Busy`; the error text names the takeover
   (`the LOCK now names owner … fence …`) or the taken-over CAS; generation and
   head are unchanged.
4. B then publishes and succeeds; `verify --full` is clean and A's file is not
   visible.
5. The inverse sanity test (suspended *before* the fence) is retained and still
   passes.

`tests/s3_fencing.rs::a_replaced_lock_record_makes_the_stale_publication_fail_412`
forces the same `412` deterministically while the stale writer's own lease is
still valid, isolating the conditional-write refusal from the own-deadline guard.

### Evidence (this host, Docker courts, all limits unchanged)

* MinIO (`scripts/s3_court.sh`): `s3_backend` 6/6, `s3_fencing` **11/11**
  including both new tests; receipt `bench/receipts/s3-court-20261107T025420Z.json`.
* Ceph RGW 19.2.0 (`scripts/ceph_court.sh`): `s3_backend` 6/6, `s3_fencing`
  **11/11**; receipt `bench/receipts/ceph-rgw-20261107T025545Z.json`.
* `fake-gcs-server` v1.56.1 (`bench/scripts/gcs_court.sh`): `gcs_fencing`
  **9/9**, including the new test; receipt
  `bench/receipts/gcs-20261107T034113Z.json`.
* Azurite 3.35.0 (`bench/scripts/azure_court.sh`): `azure_fencing` **9/9**,
  including the new test; receipt
  `bench/receipts/azure-20261107T034219Z.json`.
* Go MinIO (`WeftFS-Go/scripts/go-s3-court.sh --focused-only`): **20 pass /
  0 fail / 1 skip** (the crash child helper), including
  `TestS3FencedWriterPausedAfterFenceCannotPublish`; receipt
  `WeftFS-Go/bench/receipts/go-s3-20261107T032512Z.json`.
* Rust `cargo test --all-targets`: **540 passed / 0 failed / 0 ignored**, 103
  suites. FUSE `cargo test --features fuse --all-targets`: **582 passed /
  0 failed / 2 ignored** (the pre-existing `CAP_SETUID` and kernel-7.3 direct-I/O
  tests).
* Go `go test -race -count=1 ./...` in the FUSE court: all packages ok;
  `go vet ./...` clean; `CGO_ENABLED=0 go build ./...` clean.
* The crate-level unsafe guard was extended with one explicit, narrow exception:
  `src/fence_pause.rs` may contain exactly one `unsafe` line
  (`libc::raise(SIGSTOP)`), and the guard fails if a second appears.

### Migration

A store last written by R17/R18 has a v2 `LOCK` record. The next writable open
takes the lease over (the record must not be live) and writes a v3 record in that
one conditional write, filling `pending_commit_len` from the record (0 = unknown
until the next publication). If a v2 writer publishes after that upgrade, the
superblock cache can be ahead of the record; the store then refuses to open with
`UnsupportedVersion` naming both generations rather than silently choosing one.
Upgrade every writer before opening the store again.

### Residual

* Still one writer; not a multi-writer or fleet protocol.
* No clock-skew defence; deadlines are the writer's wall clock.
* A lost network reply to the publication write is the usual at-least-once
  ambiguity (the next write fails `Busy` and the handle reopens).
* The cache write is a second object write, but it follows the publication and
  can never move the cache backwards or be trusted over the record.
* `WeftFS/AGENTS.md` is gitignored by operator directive and is not committed;
  this section records the protocol change that the program memory also notes.
* No physical media power loss; the courts remain process-fault and single-node
  device-model proxies.

## Subphase S1 — decode floor, wide-word kernels, mode 17, access-pattern planner (2026-11-07)

This subphase answers the reviewer's decode-throughput requirement with measured
numbers, including every loss. Receipts:
`decode-floor-20261107T034727Z.json` (baseline),
`decode-floor-20261107T040250Z.json` (kernels),
`float-tiled-20261107T042955Z.json` (real Qwen),
`access-pattern-20261107T044101Z.json` (planner),
`restore-pipeline-20261107T045603Z.json` (988 MB). Analysis:
`bench/RESULTS-decode-simd.md`, `bench/RESULTS-mode17.md`.

### Measured decode floor and kernels

* Baseline, single-threaded, 16 MiB BF16: Huffman 15.4646 ms (1034.6 MiB/s of
  exponent plane), plane join 18.0656 ms, whole mode-9 decode 34.7865 ms
  (459.9 MiB/s). FP32 mode 9: 21.4125 ms (747.2 MiB/s).
* Rust Huffman: an AVX2 eight-byte-refill, two-symbol-unroll kernel is
  **1.07–1.10x** the retained scalar kernel in the same process. Cross-run
  absolute change is +1.0% (1 MiB) to +3.7% (16 MiB), i.e. **no absolute win**;
  a single Huffman bitstream cannot be lane-parallelised without a wire change.
* Rust BF16 plane join: the byte-wise formulation is **11.2x** faster
  end-to-end at 16 MiB (18.0656 → 1.6166 ms) because LLVM auto-vectorises it.
  The explicit AVX2 kernel is a tie at ≥1 MiB (1.01–1.04x) and 3.8x slower at
  128 KiB, so it is not selected.
* Whole BF16 mode-9 decode: 34.7865 → 18.2186 ms (**1.91x**); 1 MiB 2.2621 →
  1.2181 ms (1.86x). FP32 is unchanged; its +2–8% cross-run deltas are noise.
* `unsafe` is confined to `WeftCodec/src/simd.rs`; the crate root is
  `deny(unsafe_code)` and `tests/no_unsafe_outside_simd.rs` fails otherwise.
  10,000-case differentials per kernel (Huffman streams including mutations,
  BF16 plane pairs) pass.
* Go: the word-level Huffman variant is **0.93x** (274.8 vs 294.0 MB/s) and is
  not selected; the BF16 direct pack is **1.45x** (2044 vs 1413 MB/s). Go can
  never match the Rust join because it has no SIMD intrinsics.

### Mode 17 `FLOAT_TILED`

* Wire: mode 17, schema `weft.ai.float_tiled.v1`; per-tile mode-9 plane split;
  `tile_root = H("weft/tensor/float_tile/v1", WCB({dtype, dims, element_len,
  exponent_plane_len, mantissa_plane_len, values_hash}))`; full decode verifies
  every tile root and the logical root; region reads return `tiles_touched`.
  Modes 0–16 are byte-identical and the reserved reject is now 18.
* Conformance: **3 valid + 8 reject** vectors in both codec repos; manifest 338
  entries, coverage gate `17:3/8`. Cross-language gate:
  **YES byte-identical float_tiled_bf16** and **float_tiled_fp32**, both
  directions decode exactly.
* Store: `TensorStrategy::FloatTiled`, planner opt-in tile shape, write/read/
  region paths, `checkpoint write --shard-strategy float-tiled`, full verify.
  Go store: same planner option, writer, and reader (whole-object region
  decode only); the same fixture selects the same strategies.
* Real Qwen BF16 embedding `[151936,896]`, tile `[64,896]`: mode 9
  **180,623,816 B** vs mode 17 **181,541,557 B (+0.51%)**. Write 2,287 vs
  7,435 ms (3.25x slower). Whole read 745.7 → 486.6 ms at one thread and
  740.2 → 235.9 ms at four. 100-row read 777.1 → 2.30 ms (337x); 1-row read
  749.3 → 2.12 ms (353x). Every read byte-exact.
* No whole-model mode-17 checkpoint: one tile shape cannot divide every tensor
  rank/extent in the model. Mode-17 restore evidence is the embedding plus the
  checkpoint test, not a 290-tensor rewrite.

### Access-pattern planner

* `TensorPlanOptions.access_pattern` (`AccessPattern::Whole` default, `Region {
  region_bytes, reads_per_write }`, `Mixed { ... }`), `weft plan
  --access-pattern`, and the same integer objective
  `physical_bytes + lambda_ns * decode_at_pattern`. Tile-independent modes are
  charged only for fetched region bytes per read; every other mode is charged a
  whole-tensor decode per read. Defaults are unchanged and a test proves the
  default and explicit `Whole` plans are identical (chosen, bytes, ns, cost,
  every candidate).
* Real embedding: `whole` at lambda 0 → `float-tensor` (180,623,816 B);
  `region:1792:1000` and `region:179200:1000` → `float-tiled`;
  `mixed:0:1792:1000` → `float-tiled`; `whole` at lambda 1 → `raw` (the model
  prices no-decode highest under latency-only weighting); a whole-size region
  read ×100 also selects `raw`.
* Loss: the objective has no write term. `region:1792:1` selects `float-tiled`
  by estimate, but measured end-to-end it is 7,437 ms (7,435 write + 2.12 read)
  versus mode 9 3,036 ms (2,287 + 749). Break-even is about seven 1-row reads.
* Go parity: the same 512x512 weight-like fixture selects `float-tensor` for
  Whole and `float-tiled` for Region in both languages.

### Restore pipeline

* 988,097,824 B / 290 tensors, tmpfs, one repetition: batched read 2,822.4 /
  1,837.0 / 1,420.8 ms and pipelined total 2,764.5 / 1,678.8 / 1,317.9 ms at
  1/2/4 workers. Pipeline decode wall is 95.6% of total at one worker and 99.9%
  at four: `total ≈ decode` still holds and decode fell 2.0x. Disk-destination
  runs are writeback-bound (10–14 s at 2/4 workers) and are not the basis of the
  claim.
* Mode 9 has no sound intra-frame parallelism (one sequential Huffman stream),
  so none was added. Mode-17 tiles use the bounded pool and do not parallelise a
  1–2-tile region.

### Not done here

* Whole-model mode-17 checkpoint/restore; Go range fetch that reads only tile
  bytes; write-cost term in the planner; per-tile rounding in the region
  estimate; cold cache, GPU, or fleet claims.

## Subphase S3 — persistent checkpoint service, memfd restore, DCP/Orbax re-measure (2026-11-07)

Closes the external review item: "the Orbax adapter stages a safetensors file,
launches the Rust CLI, and restores through a path that imports reconstructed
files into the store." Save and load now use one long-lived
`weftfs checkpoint-service` process, payloads cross as `SCM_RIGHTS` memfds, and
a restore cannot write to the store at all.

Receipts: `bench/receipts/checkpoint-service-20261107T063519Z.json` (11/11),
`bench/receipts/orbax-20261107T063412Z.json` (13/13),
`bench/receipts/pytorch-dcp-20261107T063540Z.json`. Analysis:
`bench/RESULTS-checkpoint-service.md`. Protocol:
`docs/CHECKPOINT-SERVICE.md`.

### Protocol and bounds

Frame `u32 length | u8 opcode | u32 request_id | u8 status | body` over an
`AF_UNIX` stream; responses echo the id; errors carry the normative `Class`
string. Opcodes: `INFO`, `LIST_CHECKPOINTS`, `CHECKPOINT_MANIFEST`,
`RESTORE_TENSOR`, `WRITE_CHECKPOINT`, `VERIFY_CHECKPOINT`, `REFRESH`,
`WRITE_FILE`, `STAT_FILE`, `READ_FILE`, `SHUTDOWN`. One request in flight.
Bounds: 1 MiB per control frame, 4 GiB per tensor or staged container, 16 MiB
per metadata file, one descriptor per frame, 8 cached manifests. A framing
violation or unknown opcode answers `Malformed` and exits non-zero rather than
resynchronising.

The store is opened read-only once. A write opens a read-write handle for that
operation, publishes, and reopens the read handle. `REFRESH` is the only way a
pinned read handle sees another process's commits; the adapters call it before
a read.

### Service court (32 x 1 MiB f32, 4-way zstd)

| operation | service | CLI per call | ratio |
| --- | ---: | ---: | ---: |
| restore one tensor (median of 20) | **2.550 ms** | 10.386 ms | 4.07x |
| restore all 32 (median of 3) | **79.465 ms** | 96.363 ms | 1.21x |
| write checkpoint | **1,206.718 ms** | 1,286.272 ms | 1.07x |
| manifest | 0.467 ms | – | – |

230 requests / 216 restores / 226,492,416 B in one process. After 100 restores
the store was identical: **34,027,955 B, 270 files, generation 2**.

### Orbax (same fixture as R15)

11 leaves, 8-way sharded, 58,765,352 logical bytes, `zstd`.

| metric | Orbax OCDBT | WeftFS S3 | CLI control (same revision) | R15 before |
| --- | ---: | ---: | ---: | ---: |
| save total | **77.958 ms** | 1,859.152 ms | 2,603.080 ms | 1,770.713 ms |
| full restore | **22.927 ms** | 185.788 ms | 205.633 ms | 2,690.338 ms |
| single leaf | **5.327 ms** | 21.948 ms | 27.049 ms | 380.432 ms |
| physical bytes | 50,643,641 | 50,866,872 (+0.44 %) | – | 50,866,816 |
| store growth from restores | – | **0 B** | – | +67,615,378 B |

Save is 1.40x faster than the CLI path it replaces, full restore 1.11x, single
leaf 1.23x. Requested 2-device restore sharding is honoured (Orbax 0.12.9
returns the saved 8-device sharding). 11/11 leaves are byte-identical, and
`checkpoint verify` + `verify --full` pass after save and after all restores.

Remaining deficits: save is still **23.85x** Orbax and restore **8.10x**; the
Rust checkpoint write (1,808 ms of the 1,859 ms save) and 11 per-leaf decodes
dominate, not the boundary.

### DCP (8 x 4 MiB f32, topology 8)

| metric | Q5 before | S3 | CLI control (same revision) |
| --- | ---: | ---: | ---: |
| full save (median of 3) | 1,223.642 ms | 1,498.649 ms | 1,521.614 ms |
| full load (median of 3) | 152.208 ms | **101.006 ms** | 106.475 ms |
| single-tensor load (median of 5) | 67.171 ms | **24.135 ms** | 22.362 ms |
| async submit | 0.720 ms | 1.549 ms | – |
| physical bytes / `torch.save` | 0.9294 | 0.9294 | – |
| store growth from restores | 0 B | 0 B | – |

Save is 1.5 % faster than the CLI control and 22.5 % slower than the Q5 record
(the difference is the Rust write, not the boundary). Single-tensor load is
7.8 % slower than the control because the control omits `dcp.load` metadata and
framework work; per leaf the service is 4x faster. Both adapters report
**0 CLI subprocess calls** in save and load (`store_command_calls: 0`).

### No PyO3

`pyo3` is absent from `Cargo.lock` (0 matches) and from the court's cargo
registry cache and extracted sources (0 matches each), so an in-process
extension module would need a network dependency. No crate was added. The
persistent service is the delivered in-process boundary; that limitation is
recorded here and in `docs/CHECKPOINT-SERVICE.md` §1.

### Fixes found on the way

* A pre-existing region-restore bug: the region path byte-scaled the last axis
  for every shard strategy, but only the mode-10 `q8_0` transport is
  byte-addressed. A region read of a `zstd`-sharded checkpoint failed
  `NonCanonical` ("region axis 2 end 32 exceeds shape extent 8"). Fixed in
  `src/checkpoint.rs`; `test_service::test_region_restore_writes_only_the_region`
  covers it.
* `WeftStorageReader(import_restored=True)` fails closed: the service has no
  container to import.

### Tests

Rust `cargo test --all-targets`: **106 test binaries, 0 failed** (includes the
new `tests/checkpoint_service.rs` 7/7 and the second `unsafe` island guard in
`tests/threefs.rs`). FUSE all-targets is reported with the release verification.

Python court receipt `bench/receipts/python-bindings-20261107T064323Z.json`
(built wheel, not the source tree): **369 tests, 0 failures, 10 skipped**
(`tests/test_service.py` 8, `tests/test_dcp.py` 10, `tests/test_orbax.py` 18).
The first S3 Python court run failed 12 checks: the independent Python reader
did not implement mode 17, so every `float-tiled` vector failed
(`bench/receipts/python-bindings-20261107T063910Z.json`, retained). Mode 17 is
now implemented in `python/weftcodec/ai_modes.py` (per-tile Huffman exponent
plane + mantissa plane, tile roots, logical root, all reject classes) and those
vectors pass.

Intermediate runs of the bench harnesses before the final control columns were
added are not retained; the final receipts above supersede them and no claim
depends on the removed files.

### Not done

No Go client, no multi-client server, no parallel decode inside the service, no
watch (only `REFRESH`), no TLS/TCP, and no claim that a caller buffer is grown
for it. No GPU-direct path: DCP still copies into the planner's tensor because
that is DCP's own contract; Orbax feeds `device_put` from the mapping without an
extra host copy. Single host, warm cache, synthetic fixtures.

### Verification of the checkpoint-service tree

`bench/receipts/verify-20261107T065848Z.json`: **30/30 courts pass, 0 failed, 0
skipped** at WeftFS `468c1cf`. The immediately preceding run,
`verify-20261107T064719Z.json` (29/30), is retained: `rust_store` failed on that
run and passed on the rerun at the same revision. The failing court is the
pre-existing `tests/quota.rs` writer-lock contention flake described above; no
test was changed, skipped, or deleted.

## E1–E4 (2026-11-07): the end-to-end economics court and three governance deliverables

**E1 — checkpoint-family lifecycle court.** `examples/family_lifecycle.rs` +
`bench/scripts/checkpoint_family_court.sh` measure the whole artifact lifecycle
in one court on real public weights (Qwen2.5-0.5B → Instruct as an independent
fine-tune; pythia-70m step0 → step1 as consecutive training checkpoints), with
every corpus SHA-256 verified in the court first. Receipt
`bench/receipts/family-lifecycle-20261107T132359Z.json` (7 configurations),
analysis `bench/RESULTS-family-lifecycle.md`.

* Consecutive second checkpoint: **42,211 B** (sparse delta) vs 144,403,851 B for
  per-tensor `zstd -19`; family data 173,233,994 B.
* Fine-tune second checkpoint: **541,273,184 B** (weight delta) vs 767,597,259 B;
  family data 1,199,958,249 B; base checkpoint 0.856× `zstd -19`.
* Access-pattern plan: a 1-row read goes 2,366.7 ms / 333,871,119 B → **5.22 ms
  / 2,896,846 B** (454×) at +9.8 % (region1) / +14.6 % (region100) storage.
* Cross-topology restore verifies every root: 197,146,951 B (pythia, 0.700× raw)
  and 823,438,236 B (qwen, 0.833×), but 2.9 s / 10.2 s at one thread.
* Process-fault recovery at all three publication phases, 3 runs each: no
  partial state, open 7.7–9.6 ms, time-to-usable 0.13–0.27 ms (old
  `after_objects`/`after_commit_file`) or 114–266 ms (new).
* GC: 141,682,009 B (pythia) / 231,628,264 B (qwen) reclaimed with no snapshot;
  **0 blobs** (index files only) with a pinned snapshot.
* Losses published: F32 base 1.20× `zstd -19` (the planner's zstd candidate is
  codec level 3), 50–217 raw small tensors under region plans, the 1 MiB tile
  plan **OOM-killed at the 3 GiB court** (all local configurations were then
  measured at 6 GiB, recorded per configuration), MinIO ingest +17.9 %, and the
  gated second checkpoint loses to a whole-file `--patch-from -12` on pythia.
* The combined court log was overwritten when the object-store configuration ran
  as a later invocation on the same path; the surviving log is that invocation
  and every number is in the receipt.

**E2 — zero-trust KV cache.** `src/kv_capability.rs`, `tests/kv_capability.rs`
(13/13), `docs/KV-CACHE-GOVERNANCE.md`, receipt
`bench/receipts/kv-capability-20261107T152958Z.json`. A frozen mode-12 page
carries a canonical `weft.kv.capability.v1` descriptor (tenant, model version,
session, layer, page position, token range, dtype, shape, page bytes) whose id is
granted in an authenticated envelope's `roots` and whose `kv.<tenant>` namespace
is granted in `namespaces`. Every pre-byte failure — absent object, torn
descriptor, absent/expired/foreign grant, POSIX denial — is the same
`Unauthorized` with the text `kv page is not available to this principal`; a
content hash is not a credential. Measured cost (2,000 samples/path): cached check
33.0/42.1 µs p50/p99 vs 18.0/21.2 µs decode-only control (**+15 µs fixed, +83 %
at 1 KiB, +8 % at 64 KiB**); per-read envelope verify 81.2/96.6 µs; denied
6.4 µs. Not a constant-time claim.

**E3 — 3FS context plane.** `docs/THREEFS-CONTEXT-PLANE.md`. Layering statement,
per-operation placement, what the USRBIO shim court proved (ABI transcription,
registration lifetime, 1,500 requests in 50 submissions, byte-identical output)
and did not (it is 1.127× slower and is not 3FS), the measured context-plane
budget, and the eight-step experiment required on a deployed cluster. **No 3FS
deployment was run.**

**E4 — normative consolidation.** `spec/WEFT-AI-CODEC.md` §0 is an RFC-shaped
mode index (mode, name, schema id, mode-specific root domains, version, status,
conformance-vector directory) for modes 6–17 plus the reserved 18; exported to
all three spec mirrors (`WeftCodec` `9cffbcf`, `WeftCodec-Go` `a0a1ebf`,
`WeftFS-Go` `16d54ca`). Conformance manifest 338 entries and coverage OK; spec
drift 42/42; claims 515 OK (one stale location corrected to
`WeftFS-Go/store/fence_record.go`, the file that carries the literal).

**Gates.** `cargo test --all-targets`: 109 binaries, 568 passed, 0 failed.
`cargo test --features fuse --all-targets --no-fail-fast`: 109 binaries, 610
passed, 0 failed, 2 ignored. One fail-fast FUSE invocation and one rust
invocation hit the pre-existing `tests/quota.rs`
`usage_survives_reopen_and_still_denies` writer-lock flake; both passed on
rerun and no test was changed. `scripts/verify-all.sh`:
`bench/receipts/verify-20261107T154010Z.json`, **30/30 courts pass, 0 failed, 0
skipped**. The two new courts are standalone, not in the aggregate: the
lifecycle court takes ~90 minutes of real-weight work and the KV court is a
benchmark, while their *tests* (`kv_capability`, and the store tests the
lifecycle harness exercises) run inside `rust_store` in the aggregate.

**Not done:** Go parity for KV pages, a FUSE read path for KV pages, a
constant-time capability check, KV encryption at rest, retention/expiry policy
for snapshots, intra-frame block-independent mode-9 streams, any 3FS cluster
run, and any fleet or cloud claim.

**Mode 18 — `FLOAT_TILED_STREAMS` (additive, post-v1.1.0).** Mode 18 is the
multi-stream form of mode 17: the same tile grid, plane split, offsets and
per-tile roots, with each tile's Huffman exponent plane split round-robin into
`n_streams` independently coded `huffman-canonical-v1` streams and its raw
mantissa plane split round-robin into `n_streams` raw streams. `1 <= n_streams
<= 16`; the tile entry carries a fixed-width `stream_offsets` table of `2 *
n_streams` u64 entries. The tile root is
`H("weft/tensor/float_tile_streams/v1", WCB({dtype, dims, element_len,
exponent_plane_len, mantissa_plane_len, n_streams, stream_offsets,
values_hash}))`, so the stream count and table are part of the identity.
`n_streams = 1` is **byte-identical** to mode 17's stored tile payload; a unit
test and the `001_streams_bf16_4x4_n1` vector prove it. The mantissa stays raw
because mode 17's is raw and the identity anchor is normative.

* Wire: mode 18, schema `weft.ai.tensor_tiled_streams.v1`, domain 0, no
  dependencies/chunks, `decoded_len` and `content_hash` as every AI mode.
  Modes 0–17 are byte-identical; the authoritative reserved-mode reject moved
  to 19 (`017_unknown_mode_reserved18` retained and superseded).
* Conformance: `conformance/v1/float-tiled-streams/` has 4 valid + 12 reject
  vectors, byte-identical in both codec repos. Manifest 355 entries; coverage
  `18:4/12`; spec drift 42/42.
* Rust↔Go: `TestFloatTiledStreamsVectors` re-encodes every valid vector and
  compares the frame bytes with the Rust vector. All four are byte-identical.
* Rust↔Go store handoff: `TestTensorHandoffRustThenGo` writes `/streams.bin`
  with the Rust CLI as mode 18 and reads it from Go (whole object, exact
  logical root, exact region); `TestTensorHandoffGoThenRust` writes it with the
  Go store and reads it with `weftfs cat` (whole and `--region 10,0:12,512`),
  and Rust `verify --full` accepts the Go store.
* Decode: per-tile streams run on a bounded pool (`HuffmanDecode` per stream);
  the whole-frame path runs tiles on a bounded pool. A new wide-word AVX2
  four-stream lockstep kernel (`simd::unpack_multi`) is runtime-dispatched and
  differential-tested against the canonical per-stream decoder over 10,000
  randomised stream sets (bytes and error class), with the scalar fallback
  retained; `simd_path=avx2` in the court.
* Python reader: modes 0–18, 355 vectors, 229 pass / 0 fail / 126 skip. No
  mode 0–18 frame vector is skipped.
* Planner: `TensorStrategy::FloatTiledStreams` (mode 18) in Rust and
  `TensorStrategyFloatTiledStreams` in Go, offered only with an explicit/auto
  tile shape and `n_streams` (default 4). The baked row is measured separately:
  `fixed_ns=0`, `ns_per_mib=3,674,824` (1 warmup + 7 reps at 128 KiB and
  16 MiB, synthetic weight-like BF16) vs mode 17's `14,131 / 3,366,772`. The
  bytes-only default is unchanged and never selects mode 18; a test proves the
  row is distinct and a separate test proves the default is unchanged. Go has
  the same candidate and row (`EstimateMode18NS`).
* Court (`bench/receipts/mode18-20261107T165215Z.json`, `bench/RESULTS-mode18.md`):
  Qwen2.5-0.5B BF16 embedding `[151936,896]`, tile `[64,896]`, 259.66 MiB.
  Bytes: mode 9 180,623,816; mode 17 181,541,557; mode 18 n=1 181,598,555
  (+0.03 %), n=2 182,211,848 (+0.37 %), n=4 183,446,037 (+1.05 %), n=8
  185,923,029 (+2.41 %). Whole decode 1 thread: 840.1 / 715.9 / 602.1 / 453.5 /
  428.2 / 408.2 MiB/s. 4 threads (tile-parallel for 17/18): 843.6 (mode 9 has
  no independent tile) / **1,828.3** / 1,800.4 / 1,501.0 / 1,390.3 / 1,342.7
  MiB/s. Region 100 rows: 0.283 ms mode 17 vs 0.416/0.515/0.434/0.440 ms mode
  18; 1 row: 0.133 ms vs 0.256/0.321/0.336/0.395 ms. Encode n=4 1,522.7 ms vs
  mode 17 1,167.0 ms.
* **Result: multi-stream lost on every measured axis.** It is 1.5–26.6 %
  slower at 4 threads, 15.9–43.0 % slower at 1 thread, +0.03–2.41 % larger,
  and 1.5–3.0x slower on small regions. The measured cause is the gather
  kernel (one symbol per lane per probe vs the shipped two-symbol AVX2 probe)
  plus a full deinterleave pass; the wire format itself is not the problem.
  The multi-stream row is honest; the bytes-only default never chooses it. A
  small-region access plan *can* choose it because the fitted fixed term is 0
  while mode 17's is 14,131 ns/read; the court's 1-row measurement (0.256 ms
  vs 0.133 ms) contradicts that model, so the choice is a model artifact and
  not a latency claim. No GPU was present and no GPU path was executed or
  claimed. This is the same shape as
  the R5 entropy-coder result: the shipped Huffman decoder is already fast
  enough that extra stream machinery costs more than it saves.
* Residual: no GPU/CUDA execution or build, one host, one public tensor, warm
  caches, `n_streams` rows are measured single-thread only (4-thread mode 18
  is still slower than mode 17), no Go store region path for mode 18 (whole
  decode then slice, like mode 17).
* Gates: WeftCodec `cargo test --all-targets` 39 binaries / 245 passed / 0
  failed; parent 111 binaries / 572 passed / 0 failed; FUSE 111 binaries / 614
  passed / 0 failed / 2 pre-existing ignored; WeftCodec-Go `go test -race
  ./...` all packages; WeftFS-Go `go test -race ./...` store+fusefs all pass;
  conformance Rust 246 pass / 0 fail / 109 skip, Go 246/0/109, Python 229/0/126
  (355 total); claims 555 OK; spec drift 42/42; `scripts/verify-all.sh`
  **30/30 pass, 0 failed, 0 skipped** (`bench/receipts/verify-20261107T172305Z.json`).

## Subphase T1 — hot raw / cold planned, trajectory, load receipts, recovery-first (2026-11-08)

New mechanisms: an explicit critical-path **write policy**, a bounded
**background transcode** pass, a causal-checkpoint link for the checkpoint
writer, **load receipts** (`weft.load.receipt.v1`), and **recovery-first**
restore. All work on `staging`; receipts and results are cited per section.

### Write policy (receipt `write-policy-20261107T235728Z.json`)

`--write-policy fast|planned|auto` (`Options`-level:
`TensorPlanOptions::write_policy`; checkpoint:
`CheckpointWriteOptions::write_policy`) plus `--fast-codec raw|zstd` and
`--fast-zstd-level`. `Fast` restricts the candidate set to raw chunks or one
ZSTD frame and never runs a mode-9/10/15/16/17/18 or delta encoder; `Auto`
resolves fast when the caller declares `--critical-path` or `--deadline-ms`.
`CheckpointWriteOptions.plan.causal_checkpoint` links the previous checkpoint's
matching shard versions, and `--shard-strategy planned` lets the planner choose
per shard. The fast codec defaults to **ZSTD level 1** (measured: within 9 % of
raw's CPU and 21.8 % smaller; raw won the base wall on this single run).

Qwen2.5-0.5B base + Instruct, 8-way shards, 290 tensors (pause/wall/CPU):

| policy | base pause | base CPU | instruct pause | instruct CPU | store B |
|---|---:|---:|---:|---:|---:|
| planned | 98,882.7 ms | 18,770 ms | 108,088.5 ms | 18,780 ms | 824.0/824.5 MB |
| fast raw | **55,689.4** | **2,470** | 110,191.2 | 2,780 | 992.5 MB |
| fast zstd-1 | 75,815.3 | 2,690 | **76,572.7** | 2,720 | 775.7/776.2 MB |
| fast zstd-3 | 137,135.5 | 2,910 | 95,997.7 | 2,800 | 775.6 MB |

Encode CPU falls **6.5–7.6×**. Wall/pause is dominated by 2,320 object
publications and host cache state; the codec ordering is not stable on single
runs, so only the CPU reduction is claimed as a stable result. Raw writes
28.0 % more bytes than zstd-1.

### Background transcode (same receipt)

`weftfs transcode --root … [--dry-run] [--max-objects/--max-bytes/--max-millis]
[--min-saved-percent]` (`Store::transcode`): exclusive single-writer bounded
pass, per-object one-encode decision (`rebase_tensor_object_decided`), new
version of the same object id, old version retained, `weft.provenance.v1`
`operation: "transcode"`, read-back byte comparison. Base raw 123.6 s /
34.1 s CPU, 987.9 → 658.4 MB (1,352 rewritten); raw-source objects whose
planned form is also raw are left alone. Base zstd-1 227.6 s / 60.6 s CPU,
772.0 → 658.6 MB (2,320). Instruct zstd-1 242.8 s / 61.4 s CPU, 772.5 →
658.5 MB. **Transcode is a storage win and a restore loss**: full restore
1,675.8 → 4,431.5 ms (base raw) and 2,194.5 → 4,755.9 ms (base zstd-1); the
planned form is mode 9, ~2–2.7× slower to decode. Crash proxy: `SIGABRT` after
1 publication and just before the next → exit 134, after reopen 1/2
float-tensor + 2,319/2,318 raw, `checkpoint verify` exact, 0 problems; a clean
pass then completes. Process-fault proxy only.

### Adapter pause (`adapter-pause-20261108T004305Z.json`)

Checkpoint-service protocol v2 carries the write policy (v1 bodies still work).
`WEFT_WRITE_POLICY` / `WEFT_FAST_CODEC` / `WEFT_FAST_ZSTD_LEVEL` are the
deployment knobs. Synthetic 32 MiB fixture, every save verified + restored:

| adapter | planned | fast | speedup | standard |
|---|---:|---:|---:|---:|
| PyTorch DCP | 1,384.5 ms | 114.8 ms | 12.06× | — |
| Orbax handler | 1,394.6 | 131.3 | 10.62× | 31.9 ms (OCDBT) |

On this fixture the Orbax deficit is **43.7× planned → 4.12× fast**; the
remaining gap is staging/memfd/service/commit, not encode. The prior 58.7 MB
receipt's 22.2× is not re-run, so the cross-receipt comparison is indicative.

### Trajectory (`trajectory-20261107T220652Z.json`, `bench/RESULTS-trajectory.md`)

* Pythia-70M steps 0/1/2/4/8 plus **step16/64/512/1000** fetched and LFS-SHA
  verified in a python court: 9 checkpoints, 281.7 MB each. Cumulative
  **173,235,935 → 660,220,076 B**; nine independent `zstd -19` checkpoints sum
  to 1,299,182,221 B (49.2 % larger). Final-step baselines: `final + Σ
  zstd19(XOR delta)` 672,210,950 (ours 1.8 % smaller), `first + Σ
  zstd19(--patch-from)` 1,031,422,235 (ours 36.0 % smaller). Early steps lose
  to a single `zstd -19`; cross-over after step 2. 9/9 exact (684 tensor + 9
  file SHA-256). Restore grows 683.3 → 9,520.8 ms with depth 0 → 8.
* Qwen base → Instruct: 658,187,354 → **1,199,651,657 B** (marginal
  541,464,303; 241 weight-delta + 1 zstd + 48 raw). Independents 1,535,945,154
  (+21.9 %), `final + Σ XOR` 1,384,249,213 (+13.3 %); `--patch-from` is
  unavailable (both `-19` and `-12` failed with `long mode automatically
  triggered`). 2/2 exact (580 tensor + 2 file checks). Full restore
  3,956.8 → 10,082.7 ms.
* Declared **synthetic 50-step** series (64 × [256,256] BF16, documented rule):
  4,652,647 → **34,894,788 B**, 6.6× below 50 independent zstd-19 checkpoints;
  50/50 exact (3,200 tensor + 50 file). Restore 23.4 → 220.9 ms at depth 8. The
  depth-8 codec bound forces a full checkpoint every 9th step (5 resets,
  23.5 MB). **Loss: `final + Σ zstd19(XOR)` is ≈4.88 MB, far below 34.9 MB** —
  sparse-delta framing dominates when only a few dozen words change.
* GC: with 50 snapshots, 49 unlinked checkpoints free **0 bytes**; without
  snapshots, 49 unlinked checkpoints free **150,596 B / 142 blobs** and the
  final restore is exact. `GcReport::deleted_bytes` (3,467,041,895) is
  inconsistent with the measured tree delta and is not used as a claim.
* Court-side fix: the mode-14 semantic candidate search decoded up to
  `max_candidates` live tensors and OOM-killed pythia; it is now bounded by
  `TensorPlanOptions::max_semantic_bytes` (512 MiB) and can be disabled with
  `--no-semantic`. Both code paths are tested/computed; the trajectory run uses
  `--no-semantic`.

### Load receipts + recovery-first (`recovery-load-20261108T004358Z.json`, `docs/LOAD-RECEIPTS.md`)

* `weft.load.receipt.v1` (canonical WCB) per replica: per-tensor attested and
  loaded roots, content root, SHA-256, bytes, units verified, loaded/attested
  set roots, exactness, container SHA-256, lazy/full. CLI
  `checkpoint restore --load-receipt`, DCP/Orbax via `WEFT_LOAD_RECEIPT`,
  FUSE `mount --verify-load --load-receipt-dir`.
* 512 MiB synthetic tiled checkpoint (16,384 tiles): lazy median 2,181.2 ms
  (4,362 ms/GiB), full 3,836.6 ms (7,673 ms/GiB), same set root and exact
  output; full is 1.76× lazy. Corrupted one tile on disk → typed `Corrupt`
  naming `tile 0`, **0 bytes emitted** (`tests/load_receipt.rs`).
* Recovery-first: 4 required of 16 → first usable **1,037.0 ms** vs complete
  8,261.7 ms; 15,336,426 of 245,382,816 B (6.3 %) fetched before the first
  usable tensor. After a real `SIGABRT` once the required set was cached, the
  restart serves it from the verified cache: first usable **51.9 ms**, 0 bytes
  fetched, exact. Cold time-to-complete is slower than a plain restore
  (8.26 s vs 2.18 s) because the mode re-resolves the manifest per tensor.

### Gates

* Rust `cargo test --all-targets`: **585 passed / 0 failed** (117 binaries),
  including new `tests/transcode.rs` 7/7, `tests/load_receipt.rs` 3/3,
  `tests/recovery_first.rs` 3/3.
* FUSE `cargo test --features fuse --all-targets`: **628 passed / 0 failed /
  2 pre-existing ignored** (117 binaries), including
  `tests/fuse_verify_load.rs` 1/1.
* WeftFS-Go `go test -race -count=1 ./...`: all packages pass (store 189.4 s).
* Checkpoint service protocol v2 test: 7/7.
* Claims: 581 OK (`bash scripts/check-claims.sh`).
* `scripts/verify-all.sh`: **30/30 pass, 0 failed, 0 skipped**
  (`bench/receipts/verify-20261108T005940Z.json`). The new courts
  (`trajectory`, `write_policy`, `recovery_load`, `adapter_pause`) are run
  standalone and are not yet added to `verify-all.sh`.

### Residual

* Wall/pause ordering across fast codecs is not stable on single runs; CPU is.
* Transcode is a separate exclusive pass (not concurrent with the writer) and
  is slower than the save; Go has no write policy, transcode, load receipt or
  recovery-first path.
* The FUSE receipt is per file; partial reads pay full verification.
* The trajectory court is one host, warm caches, one public family plus a
  synthetic series; the synthetic curve loses to the XOR baseline.
* No physical power-loss court for transcode (process abort only); no
  multi-writer, fleet, RAPL, or GPU claim.

### Prepare/commit checkpoint writes: N blob writers, one committer (`docs/PARALLEL-CHECKPOINT.md`)

* `Store::write_checkpoint_parallel` splits the checkpoint write into a
  **prepare** phase (N worker handles publish immutable content-addressed blobs
  and return manifest fragments) and **one commit** (a single committer
  re-verifies every fragment against its own backend, then publishes one group
  commit). A prepare task is one tensor, because the tensor-level
  `LogicalTensorRoot` spans its shards.
* The single-writer proof is untouched and that is the point: workers hold
  read-only handles and only call create-once blob publication; they never
  touch the superblock, a commit record, the writer lock, the lease, the fence,
  or the pack catalog. No multi-writer consensus exists or is needed.
* Failure semantics: a worker error or panic publishes nothing and names the
  tensor/shard; the next `gc --apply` reclaims the orphans; a killed writer
  process mid-prepare leaves only the previous whole generation (proved with a
  real `SIGKILL`); a duplicate checkpoint name is `Exists`; a blob whose bytes
  disagree with its key is `Corrupt`.
* Bounds: `--ranks` (hard cap 64), `--max-inflight-bytes` (1 GiB),
  `--per-rank-bytes` (512 MiB), `--prepare-deadline-ms`; the committer's
  fragment re-verification is on by default (`--no-fragment-verify` disables).
* `CheckpointWriteOptions::deterministic_seed` / `--checkpoint-seed` makes the
  sequential and split paths publish identical bytes for the same input.
  Local court (Qwen2.5-0.5B, 8 shards, 5 reps, 4 vCPU): best prepare
  13.61/11.55/6.36/3.50 s at 1/2/4/8 ranks (3.89× at 8 vs 1); best wall
  15.40/13.37/8.16/5.31 s; commit 73–74 ms; verify 1.29 s; peak RSS
  393→507 MiB; every configuration stored 775,693,224 B with head
  `53a8c743a9e0b451…`. MinIO subset court: prepare 192.4/121.0/64.9/58.0 ms,
  commit ~4.5 ms, identical head `2438460a33504a09…`.
* Losses: single runs are bimodal because of the host page cache (medians
  published); 8 ranks on 4 vCPUs is worse than 4 in the median; RSS grows with
  ranks; the S3 phase is a tensor subset on a 1 GiB tmpfs; Rust only.
* Receipts `bench/receipts/parallel-checkpoint-20261108T033000Z.json` and
  `bench/receipts/parallel-checkpoint-s3-20261108T033921Z.json`; tests
  `tests/parallel_checkpoint.rs` 7/7. Superseded receipts `030000Z`/`015555Z`
  measured the same data before the v2 commit schema (identical manifest root,
  different commit id).

### RS256 constant-time ladder + fault check + timing smoke test (`examples/rsa_timing.rs`)

* Both CRT half-exponentiations use a **Montgomery ladder**: exactly
  `modulus.bit_len()` steps (public width), one squaring + one multiply per
  step, exponent-bit operand selection by mask-only `cswap`, fixed-width CIOS
  Montgomery multiplication with unrolled carry propagation and a borrow-mask
  conditional subtraction, branchless `m1 - m2 mod p`, and fixed-width
  `h*q + m2`.
* **Mandatory fault check**: after signing, `sig^e mod n` is recomputed with
  the public exponent through a value-independent square-and-multiply and must
  equal the exact EMSA-PKCS1-v1_5 encoding; otherwise signing returns
  `Corrupt` and nothing is emitted.
* Equivalence is tested two ways: the ladder/multiplication match the previous
  square-and-multiply and Montgomery routines over random moduli, bases, and
  exponents (`src/service_account.rs` unit tests, 9 pass), and every existing
  service-account test passes unchanged, including the RFC 7515 A.2 vector
  (`tests/service_account.rs` 14/14).
* Timing smoke test, 20,000 signs per class × 3 rounds, randomized
  interleaving, classes = digest prefixes `0000` vs `ffff`: median difference
  0.05 %, Welch t = 2.91 / 4.04 / 3.64 (worst |t| 4.04, below the conventional
  4.5 dudect cutoff). One earlier single-round run measured 6.24; that receipt
  is retained. Receipts `bench/receipts/rsa-timing-20261108T041500Z.json` and
  `bench/receipts/rsa-timing-20261108T040000Z.json`.
* **Not proven**: constant-time execution. The test is a smoke test; no formal
  audit, one microarchitecture/compiler, no cache/branch-predictor/frequency
  adversary model. `BigUint::rem` still runs on public message bytes. The Go
  GCS backend uses vetted `crypto/rsa`; this is Rust-only.

### Selector-index trust: commit-attested WFX2 (`docs/INDEX-TRUST.md`)

* The reported gap is closed: a segment is accepted only when its WFX2 digest
  is valid, its commit/generation/counts match the replayed state, **and** the
  commit record re-hashes to that commit and attests the expected index shape
  (`object_count`, `relation_count`, `entry_count`,
  `H("weft/index/state/v1", six-field v1 WCB)`, and
  `H("weft/index/candidates/v1", WFX2 section bytes)`). A forged,
  self-consistent subset fails the candidate-root check and is rebuilt; the
  reject is a diagnostic note and never affects commit durability or `verify`.
* New additive commit schema `weftfs.commit.v2` (v1 keys + `index_attestation`);
  the commit identity domain is unchanged. `weftfs.commit.v1` remains fully
  valid, and an index named by a v1 commit is **never trusted** (rebuild-only).
  The frozen conformance fixture still reproduces v1 byte-for-byte via
  `attest_indexes: false` / `AttestIndexes: false`. The WFX2 segment layout and
  digest domain are unchanged.
* Adversarial proof in both languages: remove one candidate, keep every header
  count, recompute the digest, reopen → Rust
  `tests/index_persistence.rs::forged_self_consistent_subset_is_rejected_by_the_commit_attestation`
  and Go `TestForgedSubsetIndexIsRejectedByCommitAttestation` both report
  `rejects >= 1`, `disk_loads == 0`, `builds >= 1`, identical scan answers.
  The cross-language court now also proves a genuine Go segment is *loaded* by
  Rust (`index.loads > 0`, `builds == 0`) and that Rust rejects the forged
  subset; Rust segments load in Go with `DiskLoads=1, Builds=0`.
* Residual: an attacker who can rewrite the commit record is a different threat
  class (no signed/anchor-authenticated commit chain yet); the index stays
  advisory, so a forged segment costs at most a rebuild.

### T1 hardening gates (this revision)

* Rust `cargo test --all-targets`: **596 passed / 0 failed** across 120 test
  binaries. The pre-existing `tests/quota.rs::usage_survives_reopen_and_still_denies`
  flake is now measured precisely: **15/15 pass with `--test-threads=1` and
  12/15 with the default thread count**, and a 12,000-iteration
  init/drop/reopen probe across 4 threads had **0** failures. The class is
  always `Busy` at the reopen. Cause: `flock(2)` locks belong to the **open file
  description**, which a concurrently forked child inherits until `exec` closes
  it (Rust sets `O_CLOEXEC`, so the window is only fork→exec). `tests/quota.rs`
  also spawns the CLI, so under parallel test threads another test's fork can
  briefly hold this test's `LOCK` fd. It is a test-harness/fork artifact, not a
  store bug; no test was changed or skipped.
* FUSE `cargo test --features fuse --all-targets`: **639 passed / 0 failed /
  2 pre-existing ignored** across 120 binaries.
* WeftFS-Go `go test -race -count=1 ./...` (fuse court): all packages pass
  (store 118.5 s, cmd 41.5 s, fusefs 9.2 s, index, model); `go vet` and
  `CGO_ENABLED=0 go build` clean; `gofmt -l` empty.
* WeftCodec-Go `go test -race -count=1 ./...`: all packages pass (spec mirror
  only changed there).
* Claims: **590 OK** (`bash scripts/check-claims.sh`); spec drift 42/42 across
  three mirrors.
* `scripts/verify-all.sh`: **30/30 pass, 0 failed, 0 skipped**
  (`bench/receipts/verify-20261108T031839Z.json`) at WeftFS `bc5c35d`,
  WeftCodec `6dd7e13`, WeftCodec-Go `70e4701`, WeftFS-Go `4b0254f`. Retained
  failing aggregates at the same code (`025347Z`, `030406Z`, `031121Z`) are
  the lock flake below; a direct probe (2,000 init/drop/reopen cycles while
  another thread repeatedly spawns `/bin/true`) reproduced it **269/2,000**,
  which pins the cause: `flock(2)` is held by the forked-but-not-yet-exec'd
  child until `exec`. Fixing it needs OFD locks with a mixed-version
  compatibility story, which is **not** done here.

Stage receipts: `parallel-checkpoint-20261108T033000Z.json` (local) and
`parallel-checkpoint-s3-20261108T033921Z.json` (MinIO subset);
`rsa-timing-20261108T041500Z.json` (and the retained single-round
`rsa-timing-20261108T040000Z.json`). The new courts are standalone and are not
yet added to `verify-all.sh`.

Residual for this revision: the parallel checkpoint path is Rust-only (Go has
no checkpoint writer); constant-time execution is not formally audited; an
attacker who can rewrite the commit record is outside the index-attestation
threat model; no multi-writer, fleet, RAPL, GPU, or physical power-loss claim.

### T2 — writer-lock flake: root cause, fix, and a refuted assumption (2026-11-08)

The `tests/quota.rs::usage_survives_reopen_and_still_denies` flake that appeared
in three T1 aggregate receipts (and in R9/R18/R19 before it) is fixed at the
root. Nothing about the on-disk `LOCK` file changed: it is still an empty
marker, and the lock is not a stored format.

**Root cause, measured.** `examples/lock_probe.rs` isolates the mechanism.
On the court kernel (`7.3.1-1-cachyos`, tmpfs), 2,000 acquire/drop/reopen
cycles per row while another thread continuously forks and execs `/bin/true`:

| mechanism | close only | explicit unlock before close |
|---|---|---|
| `flock(2)` | **289 spurious `Busy`** | 0 |
| `fcntl(F_OFD_SETLK)` | **478 spurious `Busy`** | 0 |
| `fcntl(F_SETLK)` (POSIX) | 0 | 0 |

The row-by-row receipt is `bench/receipts/lock-probe-mechanisms.json`.

A second run of the same probe measured 519 and 592 respectively; the counts
are race-dependent, the ordering and the zeroes are not. `fork-window` rows
agree: with close only, a child that never execs keeps the lock for both
`flock` and OFD; with the explicit unlock, the reopen acquires immediately.

**The assumption in the T1 note was wrong.** OFD locks are *not* exempt from
`fork` inheritance: the child shares the open file description, so both the
`flock` and the OFD record lock stay held until it execs. Switching to OFD
alone would have left the flake in place. This is now an executable test
(`ofd_close_only_is_also_blocked_by_a_forked_child`), not a comment.

**A second, separate defect was found in the same change.** Linux `flock` and
OFD/POSIX record locks live in separate lock namespaces and do not conflict.
An OFD-only writer would therefore have silently shared a store with an older
`flock`-only binary. The shipped lock takes **both** — `flock(2)` first as the
mixed-version compatibility lock, then `F_OFD_SETLK` as the primary ownership
lock — and proves exclusion in both directions
(`a_legacy_flock_holder_blocks_the_shipped_writer`,
`the_shipped_writer_blocks_a_legacy_flock_holder`).

**The fix.** `src/writer_lock.rs`: `O_CLOEXEC` on open, `FD_CLOEXEC` via
`F_SETFD`, `flock` + OFD acquisition with the `flock` released on any failed
OFD acquisition, and — the actual root fix — an explicit `F_UNLCK` + `LOCK_UN`
in `Drop` **before** the descriptor closes. An explicit unlock removes the lock
from the shared open file description, so a forked child cannot keep it alive.
`flock(2)` remains the documented fallback when `F_OFD_SETLK` returns `EINVAL`.
`fs2` is no longer a dependency.

**Store-level probe** (`bench/receipts/lock-probe-store.json`): a real `LOCK`
file and real `Store::open`. Legacy `flock` acquired before the fork, then
close-only → reopen saw `Busy` **true**; shipped `WriterLock` in the same shape
→ **false**; 2,000 `Store::open` calls under **1,182** concurrent forks →
**0** spurious `Busy`, 0 other errors.

**Courts.**
* Rust `tests/writer_lock.rs` **8/8** (two executable bug documents, the fix,
  the fallback, mixed-version exclusion both ways, live-handle refusal, a
  200-iteration store reopen under concurrent forks).
* `cargo test --test quota -- --test-threads=8` **20/20** (was 12/15 at T1).
* WeftFS-Go `store/lock_inheritance_test.go` **5/5**. Go cannot fork without
  exec; `exec.Cmd.ExtraFiles` duplicates the descriptor into the child, which
  shares the open file description — the same property. Go now also holds
  `F_OFD_SETLK` (via `golang.org/x/sys/unix`) plus `flock`, and already
  unlocked before close; this is now explicit and tested.
* Rust `cargo test --all-targets`: **604 passed / 0 failed**; FUSE
  `--features fuse --all-targets`: **647 passed / 0 failed / 2 pre-existing
  ignored**. Go `go test -race ./...` is run at the end of the subphase.

**Losses / residuals.** `flock` and OFD are two locks on one descriptor, so
acquisition costs two syscalls (not measured separately; below noise). If a
process is killed without `Drop`, a forked child can keep the lock until it
execs; that window belongs to the forking process and is identical for every
mechanism. The POSIX `F_SETLK` row is measured but not used: it fixes `fork`
inheritance and introduces the "any close drops the lock" hazard.

Docs: `docs/FENCING.md` §9; `docs/PLANES.md` §6; claims `ofdlock_*`.

### T2 — KV prefix cache: storage primitive + declared reuse decision (2026-11-08)

Implemented in Rust. `src/kv_prefix.rs` adds:

* the canonical `weft.kv.prefix.v1` record (20 fixed fields, exact key set,
  canonical re-encoding, structural checks) under `/kv/prefixes/<name>`;
* mode-12 pages under `/kv/pages/p-<descriptor-id>-<sha256>` — page names bind
  descriptor **and** payload, so different bytes can never alias one object;
* the prefix token-hash chain
  `root_{i+1} = H("weft/kv/token-chain/v1", root_i || u64le(token_i))`;
* `kv_prefix_put[_seeded]` (one commit, parent-chain page reuse, page-aligned
  parents only), `kv_prefix`, `kv_prefix_list`, `kv_prefix_lookup`,
  `kv_prefix_get`, `kv_prefix_delete`, `kv_gc`, `kv_decide_reuse`;
* CLI `weftfs kv put|get|list|delete|gc|plan`.

**Compatibility is exact and fails closed.** Tenant, model, tokenizer hash,
attention-configuration hash, dtype, head count, head dim, layer count, page
rows, `position_start == 0`, `position_end <= request length`, and the token
chain at the stored length must all match. There is no nearest match. A miss
returns `Recompute` with per-candidate reasons (model / tokenizer / attention /
token-chain / longer-than-request / …); the caller recomputes. 9 mismatched
fields plus token-chain and length cases are tested
(`every_compatibility_field_fails_closed_to_recompute`).

**Capability enforcement at get.** `kv_prefix_get` requires an authenticated
envelope whose principal equals the record tenant and which grants
`kv.<tenant>`; every page is then read through the existing per-page
`read_kv_page` (descriptor must be in `roots`), and decoded metadata is compared
with the record. Missing/expired/foreign/partial grants all return the same
non-disclosing `Unauthorized` text.

**Dedup, measured** (`bench/receipts/kv-prefix-20261108T052500Z.json`; shapes
BF16, 14 layers, 2 KV heads, head dim 64, 64-token pages = 3,584 B/token):

* one 512-token prefix: 1,835,008 logical B, 112 page objects, 1,931,305
  physical page-object B, 55.8 ms put;
* a 1,024-token child: 3,670,016 logical B, **112 pages reused / 112 new**,
  1,835,008 new B;
* 100 prefixes over one 512-token system prompt: 286,720,000 logical B →
  **103,219,200 stored B, dedup ratio 0.64**; 6,412 objects /
  110,569,036 B on disk; 4.03 s;
* authorized `get` (126–210 pages): p50 **23.4 ms**, p95 35.9 ms, max 36.3 ms;
* compatibility scan over 101 prefixes: p50 **10.4 ms** (the dominant reuse
  term; no persisted KV index yet).

**The decision.** `T_reuse = lookup + transfer + decode + placement`;
`T_recompute = prefill_fixed_ns + tokens * prefill_ns_per_token`. Transfer and
placement are declared in `bench/calibration/kv-prefill-v1.json`
(`weft.kv.prefill.v1`); decode uses the existing measured `KV_CACHE` row; the
lookup term is `max(declared, observed scan)`. The prefill model is
**declared, not measured against an engine** (default 120 µs/token; the
Qwen-0.5B row is 90 µs/token). Both estimates and every component are in
`KvReuseDecision` and in `kv plan --json`. 13 declared requests: **6 reuse / 7
recompute**; categories model-mismatch 3, tokenizer 1, attention 1, token-chain
1, longer-than-request 1.

A first court run, `bench/receipts/kv-prefix-20261108T042229Z.json`, has the
same measurements but predates the receipt's `court`/`revisions` block; it is
retained and not cited as current.

**GC.** `kv gc` keeps every page named by a live prefix plus open-handle pins,
and removes unreferenced KV pages from current state; physical reclamation stays
with `weftfs gc`. A pinned prefix cannot be replaced or deleted, so its pages are
always live. Tests cover dry-run/applied counts, pin retention, and
`verify --full` after put + parent extension + delete + gc.

**Not claimed.** No vLLM/SGLang integration, no TTFT or serving claim, no GPU
or fleet claim, no real-engine prefill measurement, no Go parity. The exact
validating experiment is in `docs/KV-PREFIX.md` §9.

Tests: `tests/kv_prefix.rs` **12/12**. Docs: `docs/KV-PREFIX.md`,
`bench/RESULTS-kv-prefix.md`. Claims `kvprefix_*` and `ofdlock_*`.

### T2 gates (2026-11-08)

* Rust `cargo test --all-targets`: **616 passed / 0 failed** (604 before T2 + 12
  KV-prefix tests).
* FUSE `cargo test --features fuse --all-targets`: **659 passed / 0 failed /
  2 pre-existing ignored**.
* `tests/writer_lock.rs` 8/8; `cargo test --test quota -- --test-threads=8`
  **20/20** (12/15 before the fix); `tests/kv_prefix.rs` 12/12.
* WeftFS-Go `go test -race -count=1 ./...` all packages pass; `go vet` and
  `CGO_ENABLED=0 go build` clean; WeftFS-Go `store/lock_inheritance_test.go`
  5/5.
* Aggregate `scripts/verify-all.sh` run **five times** after the T2 code and
  the Buck2-module `go.sum` fix: **5/5 runs 30/30 pass**. Receipts
  `verify-20261108T043951Z.json`, `…044722Z.json`, `…045418Z.json`,
  `…050121Z.json`, `…050823Z.json` (the last is the current one, at WeftFS
  `c88b199`, WeftFS-Go `b475d2d`). The first aggregate attempt
  `verify-20261108T043039Z.json` is **retained** at 28/30: adding
  `golang.org/x/sys/unix` to `WeftFS-Go/store` broke the independent
  `adapters/buck2` module's `go.sum`. `go mod tidy` in that module fixed it and
  the failing receipt is kept as the evidence.
* Disk: the host volume reached 100 % during the KV work; only gitignored
  `.docker-cache` artifacts were deleted (`target-rust/debug`, `package`), never
  source or receipts, and no claim depends on a deleted artifact.
* Claims: **617 OK** (`bash scripts/check-claims.sh`); spec drift 42/42.

## Stage S1–S6 — review follow-up (2026-11-08)

Six items from the external review, each measured in the Docker court.

### S1 — GC byte accounting is correct and reconciled

`GcReport::deleted_bytes` mixed data-blob bytes with advisory index-segment
bytes, so the trajectory court's pinned-snapshot phase (0 data blobs deleted)
reported **3,466,466,150 B**. The field is now data-only; `deleted_index_bytes`
and `deleted_total_bytes` are separate, in Rust and Go. Corrected on the same
fixture: pinned **0 B** data / 3,466,466,150 B index; unpinned **150,596 B**
(data tree delta, exact) / 3,466,891,299 B index. `tests/gc_accounting.rs` 3/3.
The corrected split also exposes a real 3.47 GB index directory for a 74 MB
data tree (per-commit full-state WFX2 segments); this is reported, not fixed.
See `bench/RESULTS-gc-accounting.md`.

### S2 — write cost, tile-touch accounting, mode-18 gate

The objective is now
`physical + lambda_read*decode(reads_per_write) + lambda_write*encode`.
`lambda_write_ns` defaults to `lambda_ns`, so bytes-only plans are unchanged.
Region reads count the whole tiles a request intersects (`TileGeometry`), not
logical region bytes. Mode 18's fitted fixed term was a clamped negative
intercept; the floor is now the measured 96,601 ns small-region bound, and
mode 18 is gated off automatic selection in both languages
(`--allow-experimental-modes` or an explicit strategy hint). Access-pattern
court `access-pattern-20261108T073433Z.json`: whole bytes-only → mode 9; whole
latency → raw; `region:1-row:once` → raw; `region:1-row:1000` and
`region:100-row:1000` → mode 17. The model's break-even is **5 reads**; the
measured break-even is **6.89 reads**. `tests/planner_write_cost.rs` 8/8.

### S3 — stronger candidates

Level-19 ZSTD (planned policy, ≥1 MiB, 20 s CPU budget) and a whole-base
`zstd-patch` candidate (mode-4 dictionary frame, base-covering window). On the
cached corpora: Pythia step0→step1 18,763 → **17,923 B** (patch wins 6/76
tensors), versus a whole-file `zstd --patch-from -12` of 30,265 B; Pythia F32
base 173,194,756 → **163,872,074 B** (−5.4 %) but still **+15.2 %** versus
whole-file `zstd -19`; Qwen base → Instruct stays at **541,151,674 B** (the
patch changes 0 B; mode 11 already wins). Every loss is in
`bench/RESULTS-compression-candidates.md`.

### S4 — 3FS note unit error

`docs/THREEFS-CONTEXT-PLANE.md` now records the lifecycle ingest as
**11.2–159.1 s** (148–549 ms/tensor), not 18.2–145.3 ms: the old text was wrong
by 1,000×. The conclusion is conditional on the unrun deployed-3FS experiment.

### S5 — README repositioning

The opening is now AI-workload-first, with a loss-leading "Demonstrated results"
block (the 42,211 B family total versus the 30,265 B whole-file patch baseline
comes first), then the architecture, then the repository table, then status.
`WeftCodec/README.md` has the new heading.

### S6 — aggregate coverage

`scripts/verify-all.sh` now includes `checkpoint_family_lifecycle`, `trajectory`,
`write_policy`, `recovery_load`, `kv_capability`, `parallel_checkpoint` and
`index_trust`. Long courts run reduced configurations that are recorded in their
own receipts (lifecycle: one family/one configuration/one rep, no S3;
trajectory: two Pythia steps, eight synthetic steps, no Qwen half; write policy:
two configurations; KV: the minimum accepted 1,000 samples).

Gates: Rust `cargo test --all-targets` pass; FUSE
`cargo test --features fuse --all-targets` **673 passed / 0 failed / 2
pre-existing ignored**; WeftFS-Go `go test -race ./...` all packages pass;
claims OK; spec drift 42/42; aggregate **37/37**
(`verify-20261108T165021Z.json`). The first aggregate run
(`verify-20261108T090639Z.json`, 33/37) is retained because it found the four
defects listed at the top of this file.

## Subphase S12 — bounded / streaming ingest (2026-11-08)

Goal: ingest the real Qwen2.5-3B BF16 family inside the program's default
**3 GiB** bench court. Memory-only change: no wire format, frame byte,
manifest field, price, candidate order, tie-break or selected strategy
changed.

### What changed

- `retain_candidate` keeps at most the frames that can still be chosen
  (first applicable candidate for a strategy hint, else the running
  minimum-cost candidate). Peak retained frames drop from ~6 per tensor to 2;
  candidate order, pricing, ties and the chosen strategy are untouched. Both
  zstd-3 and zstd-19 map to `TensorStrategy::Zstd`, so the second frame is
  dropped.
- `TensorLayoutSpec::canonicalize_cow` borrows a row-major caller's bytes
  instead of cloning them (same validation; other layouts still allocate).
- `SafetensorsFile::{read_tensor_range, read_tensor_shard,
  hash_tensor_digests}`: exact shard reads (contiguous on axis 0, strided on
  other axes) and streamed content/values digests in fixed windows. The
  codec side is `DomainHasher`, `values_hasher` and
  `logical_root_from_values_hash` (WeftCodec `cffcc1a`).
- `write_checkpoint` streams: whole-tensor digests are incremental, the
  logical root comes from the streamed values hash, and each shard is read
  separately. `CheckpointWriteOptions::max_ingest_bytes` (default 1 GiB) and
  `checkpoint write --max-ingest-bytes` bound it; the prepare/commit path
  enforces the same bound.
- `TensorPlanOptions::max_delta_bytes` (default 512 MiB) skips the
  whole-tensor weight/sparse/patch candidates; see the loss below.
- The family-lifecycle harness compares/digests in 4 MiB windows and drops
  the subject tensor after slicing the region expectations.
  `WEFT_FAMILY_PROGRESS=1` and `WEFT_TRACE_CANDIDATES=1` are opt-in traces.

### Measurement (reduced configuration)

Corpus re-verified in the court: `Qwen2.5-3B.safetensors` 6,171,927,112 B
`52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab`;
`Qwen2.5-3B-Instruct.safetensors` 6,171,927,112 B
`54917aba5570e50db3f135b6e268a2b67bf951d06610713e5b6e69b8b20d91d2`; 434
BF16 tensors each, largest 622,329,856 B.

Command: `scripts/docker-court.sh bench` with
`WEFT_FAMILY_FAMILIES=qwen3b-finetune WEFT_FAMILY_CONFIGS=default
WEFT_FAMILY_INGEST_REPS=1 WEFT_FAMILY_MEASURE_REPS=1
WEFT_FAMILY_TOPOLOGY_REPS=1 WEFT_FAMILY_RECOVERY_REPS=1
WEFT_FAMILY_S3=0 bash bench/scripts/checkpoint_family_court.sh`
(one family, one configuration, one rep per phase — not the full
three-configuration court).

Peak RSS per phase (`VmHWM` of a fresh process):

| phase | peak |
|---|---:|
| ingest | **2,765,636 KiB** |
| topology | 1,863,172 KiB |
| measure | 1,840,524 KiB |
| baselines | 1,228,240 KiB |
| gc without snapshot | 1,089,068 KiB |
| gc with snapshot | 1,089,536 KiB |
| checkpoint (8-way) | **614,920 KiB** |
| recovery | 250,388 KiB |

Every phase completed under the 3 GiB limit; no OOM kill and no limit was
raised. Ingest wall 976,527.164 ms; `verify --full` ok (870 objects, 873
manifests, 731 representations, 139 chunks, 0 problems). Base strategies:
270 float-tensor / 92 zstd / 72 raw. Instruct: 255 weight-delta / 113
sparse-delta / 1 float-tensor / 65 raw. Checkpoint: 434 tensors, 3,472 shard
objects, 187,089.716 ms, 6,171,877,376 element bytes, verify ok.
Store: data-only 6,775,861,261 B, indexes 169,652,106 B (2.442 %), total
6,946,533,343 B.

0.5B, same command: the pre-change tree (`d5e78ce` / `3d9ae04`) is
**OOM-killed, exit 137** at 3 GiB; this tree (`9822d56` / `cffcc1a`) peaks at
**2,257,928 KiB** with verify ok. Across the two new-code runs, 580/580
per-tensor rows match on `logical_root`, `strategy`, `data_bytes` and source
`sha256`, and the strategy distribution is unchanged.

Byte identity: `tests/checkpoint_streaming.rs` writes the same container with
the same `deterministic_seed` at `max_ingest_bytes` = largest tensor vs 1 GiB
and requires the same head, manifest id, manifest root and canonical manifest
bytes; it also recomputes the one-shot content and logical roots. Streamed
shard bytes equal `weftcodec::tensor::shard_bytes` exactly.

Receipts: `bench/receipts/family-lifecycle-20261108T143027Z.json` and
`bench/receipts/s12-bounded-ingest-20261108T143027Z.json`; analysis
`bench/RESULTS-bounded-ingest.md`.

### Losses and remaining work

- The 593.5 MiB embedding is above the 512 MiB delta bound, so weight-delta,
  sparse-delta and zstd-patch are not offered for it. The skipped candidates
  were not the cheapest (float-tensor 413,019,145 B vs raw 622,329,856 B), but
  this is a planner-policy bound, not equivalence with the unbounded plan.
- Mode-9 encoding of that tensor still defines the ingest peak; reducing it
  further needs a streaming mode-9 encoder.
- zstd-19 is CPU-gated off for the embedding (estimated 20.8 s > 20 s).
- The prepare/commit split still materialises one whole tensor per worker.
- Rust only; no Go parity for the streaming codec API, the ingest bound or the
  delta bound. Single host, warm cache, process-fault recovery only.
- `verify-all` was not re-run for S12.

Gates at `9822d56` / `cffcc1a`: WeftCodec `cargo test --lib` 75 passed; Rust
`cargo test --all-targets` 128 suites, 640 passed, 0 failed; FUSE
`cargo test --features fuse --all-targets` 128 suites, 683 passed, 0 failed,
2 pre-existing ignored; claims OK; spec drift 42/42; conformance manifest OK.

## Subphase S13 — dependency-graph planning (2026-11-08)

Goal: make the parent of each trajectory version a **decision**, not an
accident. The pythia 9-checkpoint trajectory stored every version as a delta on
the previous one, so chain depth reached 8 and cold restore grew from 683 ms to
9.5 s. S13 adds a write-time planner that chooses, for every version, an earlier
parent or no parent.

### What changed

- `src/plan_graph.rs` (new) is a pure optimizer over a measured
  `VersionGraph`. The objective is
  `Σ edge_bytes + λ · reads · Σ chain_decode_ns`, where the chain cost is the
  sum of the existing measured `src/cost_model.rs` rows, not a new formula.
  Three solvers:
  * `dp` — chain/restart dynamic program, state `(version, depth)`, exact over
    `parent = none | previous`, `O(n · max_depth)`;
  * `brute-force` — exhaustive oracle over **every** earlier compatible parent,
    exact `O(n!)`, refused above 12 versions with a typed `LimitExceeded`;
  * `greedy` — deterministic one-pass over every earlier parent, not claimed
    optimal.
  The tie-break is `(objective, fewer dependencies, lower parent index)`,
  lexicographic in version order; there is no randomness.
- Hard constraints: `max_depth` (default 8, the frozen
  `MAX_CODEC_DEPENDENCY_DEPTH`), optional `sla_ns` on the worst cold restore,
  and optional `storage_budget`. A plan that cannot satisfy the SLA is refused
  with `LimitExceeded`; a DP optimum over budget falls back to the exact
  budget-pruned oracle. Nothing degrades silently.
- `Store::price_parent_graph` prices every earlier parent with the shipped
  `Store::plan_tensor` (with and without a causal version); `Store::
  write_parent_graph` writes the chosen graph in version order through the
  existing `causal_checkpoint` path. **No manifest, frame, commit or
  representation field changed.**
- `weftfs plan-parents --versions NAME=FILE,... [--max-depth] [--lambda-ns]
  [--reads-per-version] [--sla-ms] [--storage-budget] [--solver]
  [--apply --out DIR] [--scratch DIR]`. The dry run writes probes into a
  private scratch store, prints per-version edges and predicted bytes/restore,
  and removes the scratch; `--apply` writes the graph into a new store.
- `tests/parent_graph.rs` 6/6 (DP == oracle on a chain-or-restart optimum;
  the oracle's non-adjacent optimum is recorded as the DP's stated limitation;
  determinism; depth bound for all three solvers and depths 0–8; typed SLA
  refusal; a store round trip proving every chosen version keeps the same
  per-tensor `LogicalTensorRoot` and content root as an independent write).
  `src/plan_graph.rs` unit tests 12/12.

### Measurement

Receipt `bench/receipts/parent-graph-20261108T192017Z.json` (schema
`weft.s13.parent-graph.v1`) at WeftFS `163a6a2` / WeftCodec `cffcc1a`, default
bench court (3 GiB / 4 cpus / 1024 pids, `weft/bench-court:1` id
`sha256:4c6c41fb…`). Corpus: nine pythia-70m checkpoints, 281,715,176 B and 76
tensors each, every file re-hashed in the court (step 0 `17a077ab…af8e`, step
1000 `09883f90…ee88`; all nine in the receipt). The independent probe store is
1,447,239,539 B; pricing all 45 edges took 1,388.8 s.

`lambda_ns` is the smallest integer at which any restart interval beats the
chain: break-evens 0.008408 / 0.005266 / 0.004714 bytes/ns for k=2/3/4, so the
court ran at λ=1 (1 GB/s) with a 4 s SLA.

Model prices from the measured graph:

| plan | depth | edge bytes | Σ chain decode | worst restore |
|---|---:|---:|---:|---:|
| current chain (`−1,0..7`) | 8 | 647,175,737 | 53,146.7 ms | 11,599.9 ms |
| restart every 2 | 1 | 1,030,180,485 | 7,594.5 ms | 1,634.8 ms |
| restart every 3 | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| restart every 4 | 3 | 808,225,589 | 18,983.3 ms | 4,482.4 ms |
| **DP at λ=1, SLA 4 s** | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |
| DP at λ=0, SLA 4 s | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| brute force at λ=1 | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |

Realised stores (actual data bytes, median of 3 restores per version, every
version exact, `verify --full` 0 problems):

| plan | data bytes | predicted | error | worst restore | Σ restore |
|---|---:|---:|---:|---:|---:|
| chain | 647,411,616 | 647,175,737 | +235,879 (+0.036 %) | 9,336.3 ms | 41,218.8 ms |
| restart-2 | 1,030,285,678 | 1,030,180,485 | +105,193 (+0.010 %) | 2,448.9 ms | 12,583.3 ms |
| restart-3 | 849,712,592 | 849,572,567 | +140,025 (+0.016 %) | 3,248.2 ms | 16,289.9 ms |
| restart-4 | 808,373,400 | 808,225,589 | +147,811 (+0.018 %) | 4,650.7 ms | 18,856.6 ms |
| optimizer λ=1 | 1,447,239,539 | 1,474,646,073 | −27,406,534 (−1.86 %) | 801.5 ms | 7,128.7 ms |

The DP and the exhaustive oracle agree exactly (`dp_matches_brute_force:
true`). Per-version restore medians and the complete graph (all 45 edge
prices, coupled-tensor counts) are in the receipt. Analysis:
`bench/RESULTS-parent-graph.md`; design note: `docs/PARENT-GRAPH.md`.

### Losses and limitations

- **The optimizer does not create restart points for free.** At λ=0 and no SLA
  the chain is the smallest plan, so the plan is exactly today's behaviour. The
  4 s SLA is what forces restart-3; λ=1 forces all-independent. The planner
  chooses the trade only when the caller states a latency objective or SLA.
- **The SLA is a model SLA.** The cost model over-predicts the chain worst
  restore by 24.2 % (11.60 s vs 9.34 s) and under-predicts the all-independent
  worst by 73.7 % (0.211 s vs 0.802 s) because the fixed per-version container /
  manifest / SHA-256 work is not in the affine table. A 4 s SLA is still met
  measured, but an SLA between 0.211 s and 0.802 s would be accepted by the
  model and missed in practice. No measured-latency feedback loop exists yet.
- The DP's stated space is `parent = none | previous`; the oracle can prefer a
  non-adjacent parent and the test records that the DP then loses. On the
  measured pythia matrix the oracle chose adjacent edges only, so DP == oracle
  there; this is measured, not assumed.
- The all-independent store is 1.86 % smaller than the sum of the per-version
  edge prices because identical chunks deduplicate across independent versions.
  The planner's byte model does not credit that dedup, so it over-prices the
  restart plan slightly.
- Pricing is expensive (1,388.8 s for nine versions): every edge re-encodes the
  full candidate set. This is a write-time planning tool, not a read path.
- Restores are measured with a warm OS page cache and a fresh read-only handle
  per repetition; the sum is decode-dominated, but this is not a cold-media
  claim.
- Rust only. No Go parity for the planner or the CLI. Single host, synthetic
  plan evaluation on real public weights, no fleet/cloud claim.
- A first working-tree run (`parent-graph-20261108T180609Z`, discarded from the
  repo) exercised the harness but the court script used a stale `weftfs` binary
  for the CLI phase; the script now always rebuilds the CLI. The cited receipt
  is the rerun at the committed revision.

Gates at `163a6a2` / `cffcc1a`: Rust `cargo test --all-targets` **658 passed /
0 failed** (128 suites); FUSE `cargo test --features fuse --all-targets` **701
passed / 0 failed / 2 pre-existing ignored**; `scripts/check-claims.sh` OK;
spec drift 42/42; conformance manifest OK; aggregate **37/37**
(`verify-20261108T165021Z.json`) at the S12 revision `94a4818`.

## Subphase S13b — measured restore floor / honest SLA (2026-11-08)

Goal: make the parent-graph planner's restore prediction honest. S13's court
found it was not: the chain was over-predicted by 24.2 % and the
all-independent floor under-predicted by 73.7 % (0.211 s predicted, 0.802 s
measured for a 76-tensor, 281,715,176 B pythia version). An SLA chosen between
those numbers was accepted by the model and missed in practice.

Revision: receipts were produced from the S13b working tree on base `4fc1ece`;
the code is commit `f385450`, this evidence `d351da8`, docs/claims follow.
`main` untouched. Submodules unchanged (`WeftCodec cffcc1a`, `WeftCodec-Go
0db665e`, `WeftFS-Go 6d58137`).

### Hypotheses and what the measurement said

The new harness `examples/calibrate_restore.rs` times real
`Store::restore_checkpoint` calls (not in-memory frame decode) on checkpoints
written with the writer's `Planned` representation, 7 repetitions per point,
and fits `residue = measured_restore − Σ measured decode rows`.
Receipt `bench/receipts/s13b-restore-floor-20261108T230417Z.json` (154 fitted
samples; court 3 GiB / 4 cpus / 1024 pids, `weft/bench-court:1` id
`sha256:4c6c41fb…`).

1. **Per-object fixed cost: refuted for tensors ≥ ~256 KiB.** At a fixed 64 MiB
   total, one 64 MiB tensor has 136.2 ms of residue and sixty-four 1 MiB
   tensors 127.0 ms. The count slope only turns positive for sub-256-KiB
   tensors (a 4 KiB tensor carries ~30–40 µs of non-decode restore work).
2. **Per-version cost: real but tiny.** Manifest read: 14.0 µs (1 tensor),
   0.31 ms (76 tensors), 77 ms (16,384). The fitted intercept is ~0; the baked
   13,285 ns per-version term is the measured one-tensor manifest read.
3. **Chain over-prediction is not a misplaced constant.** The controlled chain
   (8 MiB tensor, 8 hops) measures 14.9–15.9 ms/hop for sparse-delta and
   35.6–38.7 ms/hop for weight-delta against the 42.4 ms row: the error is per
   hop and class-dependent (sparse-delta and zstd-patch reuse the weight-delta
   row; zstd-19 reuses the level-3 zstd row).

### What changed

- `src/cost_model.rs`: `RestoreOverhead { per_version_ns, per_tensor_ns,
  per_mib_ns }` and `RESTORE_OVERHEAD_CALIBRATION =
  "weftfs-s13b/restore-overhead/v1"`. Baked measured term **13,285 / 29,205 /
  2,114,993 ns**, integer arithmetic only.
- `src/plan_graph.rs`: `restore_ns(i) = decode_chain_ns(i) + overhead(i)`,
  charged **once per version restore**, never per dependency hop (the first
  draft added it to the recursive chain and would have over-predicted a
  depth-8 chain by an extra 8 × 0.6 s). `PlanOptions::restore_overhead`
  defaults to the measured term. All three solvers prune on the corrected
  restore, not the raw decode chain. An SLA claim with the model withheld is a
  typed `InvalidArgument`; the plan records the model it used
  (`predicted_decode_chain_ns`, `predicted_overhead_ns`, `predicted_chain_ns`,
  `restore_overhead_calibration`).
- `weftfs plan-parents --restore-overhead measured|none` and the corrected
  fields in the report.
- `examples/parent_graph_court.rs`: 7 restore reps (minimum 5), a
  λ × SLA sweep, per-version prediction error, chain-hop diagnostics, and the
  `floor_prediction_within_10_percent` / `no_accepted_plan_misses_its_sla`
  checks. Receipt schema `weft.s13b.parent-graph.v2`.
- The persisted `weft.calibration.v1` document is **unchanged**: it still has
  exactly ten decode rows and no new field, so an existing v1 document loads
  byte-for-byte. The restore overhead is a separate documented constant, and a
  plan records `restore_overhead_calibration` (or `none`) so a reader can see
  which model the prediction used.
- `bench/scripts/s13b_restore_floor_court.sh` (calibration court) and
  `bench/scripts/parent_graph_court.sh` (7 reps + sweep).
- `tests/parent_graph_restore_floor.rs` 6/6: the baked constants equal the
  measured receipt and the receipt carries samples; the graph charges the
  term once; an SLA that only fits without the term is refused
  (`LimitExceeded`) with no store created; a withheld model cannot claim an
  SLA (`InvalidArgument`); determinism; and a store round trip whose
  logical/content roots are unchanged.

### Corrected prediction vs measurement

Court receipt `bench/receipts/parent-graph-20261108T232016Z.json`, the same
nine pythia-70m checkpoints and the same λ/SLA values as S13, 7 medians per
version. Every version of every realised plan restored byte-exactly and
`verify --full` was clean.

| plan (λ, SLA) | predicted worst | measured worst | error | note |
|---|---:|---:|---:|---|
| chain | 12,170.3 ms | 9,238.2 ms | +31.7 % | conservative |
| restart-2 | 2,205.3 ms | 2,049.9 ms | +7.6 % | conservative |
| restart-3 (λ=0, 4 s) | 3,629.1 ms | 3,234.3 ms | +12.2 % | conservative |
| restart-4 | 5,052.8 ms | 4,503.9 ms | +12.2 % | conservative |
| all-independent floor (λ=1, 4 s) | 781.6 ms | 886.5 ms | **−11.8 %** | cold-cache step |

Calibration validation, not the court: a real step-0 container predicts
781.6 ms vs 780.1 ms measured (+0.19 %); all nine independent checkpoints in a
fresh store measure 778.2–793.3 ms against 781.6 ms (−1.5 %..+0.4 %).

### SLA sweep and plan choices

`500, 850, 1000, 2000, 2500, 3000, 4000, 6000, 14000` ms at λ = 0 and λ = 1:

- 500 ms: refused (`LimitExceeded`) at both λ. S13 would have accepted the
  floor (211 ms prediction).
- 850 ms: all-independent accepted (781.6 ms prediction); measured 886.5 ms
  because of the cold versions, so this accepted plan **misses** its SLA.
- 1000–2000 ms: all-independent, holds.
- 2500/3000 ms: restart-2 at λ=0, all-independent at λ=1, holds.
- 4000 ms: restart-3 at λ=0 (unchanged from S13), all-independent at λ=1
  (unchanged), holds.
- 6000 ms: restart-4 at λ=0, all-independent at λ=1, holds.
- 14000 ms: chain at λ=0, all-independent at λ=1, holds.

No plan choice at the S13 λ/SLA values changed. The correction shows up as
the typed 500 ms refusal and as the 850 ms acceptance becoming marginal. The
court's `no_accepted_plan_misses_its_sla` check is **false** (2 of 16 measured
acceptances miss, both the same floor at 850 ms).

### Losses and limitations

- **The floor residual is a page-cache-capacity boundary, not a fitted error.**
  In the 3 GiB court the all-independent store is 1.5 GB over nine versions;
  versions 0–3 restore warm at 796–807 ms (+1.8..+3.3 % vs 781.6 ms) and
  versions 4–8 at 878–886 ms (−11.3..−11.8 %), with identical 163.9 MB reads.
  A fresh-store nine-checkpoint calibration run measures all nine at
  778–793 ms. Every cold restore costs ~100 ms per 164 MB. The first S13b
  court run (retained, `parent-graph-20261108T215207Z`) showed the same step
  with a larger penalty (782–805 warm, 964–987 cold). **The constant was not
  raised to cover the cold case.**
- **Chain and restart predictions are conservative** because per-candidate
  pricing is unchanged in this subphase: sparse-delta and zstd-patch use the
  weight-delta row, and a level-19 zstd frame uses the level-3 row. The chain
  measured hops are 604/515 ms for the sparse hops and 1,180–1,276 ms for the
  weight-delta hops against a 1,423 ms row. Correcting those rows could flip
  stored strategies across corpora and is a separate decision.
- The count sweep's linear per-tensor term over-predicts mid-size many-tensor
  checkpoints (worst fit residual 31.6 % at a 128 × 512 KiB point) because the
  per-object cost only turns on below ~256 KiB.
- Rust only. WeftFS-Go has no parent-graph planner; the constant is documented
  in the Rust cost model only and **no cross-language agreement is claimed**.
- Restores are single-threaded; the model is warm-cache, one host, one public
  corpus, no fleet/cloud/cold-media claim.
- The 4 KiB–64 MiB sweep and the trajectory are synthetic/single-corpus; the
  fit is a host-local measurement, not a universal constant.

### Gates

Rust `cargo test --all-targets` **668 passed / 0 failed / 0 ignored** (132
test binaries, including the 10 new S13b tests); FUSE `cargo test --features
fuse --all-targets` **711 passed / 0 failed / 2 pre-existing ignored**;
`scripts/check-claims.sh` **659/659 OK**; `scripts/check-spec-drift.sh` **42/42
OK**; `scripts/gen-conformance-manifest.sh --check` **355 entries OK**;
`scripts/check-conformance-coverage.sh` **355 entries OK**. No aggregate
`verify-all` run was made for S13b; the S13 aggregate
(`verify-20261108T165021Z.json`, 37/37 at `94a4818`) predates this change and
is **not** cited for it.

## R1 — v1.2.0 release (2026-11-09)

Release note: [`docs/RELEASE-v1.2.0.md`](RELEASE-v1.2.0.md). Artifact receipt:
`bench/receipts/release-artifacts-20261109T124325Z.json`.

### Verification at the release tree

`bench/receipts/verify-20261109T112958Z.json`: **37/37 courts pass, 0 failed,
0 skipped**, at WeftFS `135a382`, WeftCodec `d95fa78`, WeftCodec-Go `0db665e`,
WeftFS-Go `6d58137`. The parent tag is one evidence-only commit later
(`122d86e` adds the receipt and its per-court receipts). The codec cross gate
is 105 `PASS` / 14 `YES byte-identical`; conformance manifest/coverage pass at
355 entries. The prior aggregate `verify-20261108T165021Z.json` (37/37 at
`94a4818`) predates S12/S13b and is not cited for them. An earlier run at
`20261109T103921Z` was aborted at `parallel_checkpoint` and produced **no
receipt**; its partial artifacts are not cited.

### Published artifacts

* **Tags** (annotated, not GPG-signed): `WeftFS` `v1.2.0` -> `122d86e`,
  `WeftCodec` -> `d95fa78`, `WeftCodec-Go` -> `0db665e`, `WeftFS-Go` ->
  `6d58137`. `staging == main` in all four repos at the tagged revisions.
* **crates.io** (`scripts/publish-crates.sh`, weftcodec first): `weftcodec`
  1.2.0 checksum
  `77a95cd4fd9aeb2888a3f6dd175a96748cb04b221f274496e9b59f18640e3d92`
  (created 2026-11-09T12:38:45Z) and `weftfs` 1.2.0 checksum
  `4b7312b14cf3264216335a2718e39c6a5982e2f64184058f8b430a465cf6e909`
  (12:39:05Z); both `yanked: false`. Read back from the crates.io API.
* **PyPI**: `weftcodec 1.2.0` is live. Wheel 92,913 B sha256
  `699b25fd7f6aef36f98f5baf31a130ef1c816a00ca3c93ac4c324667266b9bbb`; sdist
  114,324 B sha256
  `50678df7ebb559fa3ec632520822ff2c88155788fcf17e54ec5c817388f6dff0`; both
  `yanked: false`. The upload token was deleted after use and was not reused.
* **GHCR**: all four tag workflows completed `success` (runs 42618667115,
  42618672142, 42618675687, 42618679797). Observed index digests:
  `weftfs` `sha256:1505110d…b6eada`, `weftcodec` `sha256:affee3a6…52ce4f`,
  `weftcodec-go` `sha256:bc4de382…2723c6`, `weftfs-go` `sha256:56aa4559…f489c`.
  Each config blob's `org.opencontainers.image.source` names its own
  repository and its `revision` label is the tagged commit. Builds are not
  bit-reproducible; these are observed digests, not a reproducibility claim.
* **GitHub releases**: created in all four repositories; the parent release
  carries the wheel and sdist, and both assets read back with the digests
  above.

### Unverified or imperfect

* **PyPI metadata is stale.** The published 1.2.0 long description was built
  before the post-upload `python/README.md` correction: it still says “PyPI
  publication requires an API token we do not have” and its mode table ends at
  mode 16. The files were not rebuilt or re-uploaded, per the release
  instruction. The wheel/sdist digests are correct.
* **No live cloud run.** No AWS, Azure, or Google credential/bucket was used;
  the cloud evidence remains MinIO, Ceph RGW, Azurite, and
  `fake-gcs-server`.
* **GHCR tags are not bit-reproducible** and the image digests are not pinned
  in the release note as reproducible identities.
* **S13b's 850 ms SLA is accepted and missed** (measured 886.5 ms). The court's
  `no_accepted_plan_misses_its_sla` and
  `floor_prediction_within_10_percent` checks are both false.
* **Still not done**: physical media power loss, host page-cache loss, RAPL
  energy, GPU/TPU execution, live cloud services, Go Azure, constant-time
  crypto/capability proof, writable FUSE over objects, multi-writer/fleet
  operation, Go parity for S12 streaming ingest / S13 parent-graph planning /
  S13b restore overhead, and the 3.47 GB index amplification (reported, not
  fixed).
* The release-note, README and claims commits are **after** the tag; they
  change no source, wire, or on-disk byte.

## R2 — positioning: protocol, codec, and reference filesystems (2026-11-09)

Documentation-only. No Rust or Go source, wire format, on-disk byte, or
conformance vector changed, so no court rerun is claimed for R2.

### What changed

* **`README.md` first screen.** The opening now states the program's role
  unambiguously: WeftFS is a context-addressing, identity, provenance and
  reconstruction protocol plus WeftCodec, intended to **augment** mature AI
  storage systems. The Rust and Go filesystems are called **reference
  filesystems / an executable specification**, not data-plane competitors.
  The opening states that the underlying storage system owns replication,
  erasure coding, placement, RDMA, scheduling, caching and GPU-direct I/O, and
  that compatibility with an interface does not imply a native deployment was
  benchmarked.
* **Architecture at a glance.** A tall mermaid diagram places AI consumers →
  the WeftFS context protocol (identity, selection, lineage, policy,
  provenance, verification, recovery) → WeftCodec (exact tensors, tiles,
  deltas, KV pages) → the `BlobPlane`/`CommitPlane` integration → established
  storage (3FS, DAOS, Lustre, CephFS, S3, GCS, Azure, local).
* **Integration status matrix.** One row per integration, using exactly these
  categories: *implemented and tested on a real backend* (local POSIX, MinIO
  S3, Ceph RGW), *emulator-tested* (`fake-gcs-server`, Azurite),
  *ABI/conformance-tested only* (3FS; the shim is explicitly **not a deployed
  cluster**), *prototype* (Buck2 CAS), *designed but not executed* (DAOS,
  Lustre, BeeGFS, CephFS, JuiceFS, IBM Storage Scale, WEKA, VAST, AIStore), and
  *inaccessible* (live AWS/GCS/Azure; no credentials).
* **What is measured.** Three rows pointing to receipts and `bench/RESULTS-*`:
  the Qwen family total (1,276,128,956 B, 17 % under independent per-tensor
  `zstd -19`, 3.07 % under ZipNN), the mode-17 one-row region read (749.26 ms
  → 2.12 ms, 353×, +0.51 % bytes; 100 rows 777.08 → 2.30 ms), and the exactness
  base (37/37 courts, 355 vectors, 100,000/100,000 crash iterations with 0
  partial states). The release note is [`RELEASE-v1.2.0.md`](RELEASE-v1.2.0.md).
* **Repository table.** Kept, moved above the phase history, and the Rust/Go
  roles now say “Reference filesystem”. The long phase table and release
  history remain below the architecture/status sections, unchanged in
  substance.
* **`docs/AI-STORAGE-AUGMENTATION.md` (new).** Protocol/codec/data-plane
  separation; what WeftFS owns versus what the backend owns; the
  `BlobPlane`/`CommitPlane` contract; the five writer levels MW0–MW3 with
  today's true status (**MW1: many producers, one committer**; MW2 and MW3 are
  not implemented and not claimed); per-backend guarantees; integration
  without a FUSE-on-FUSE layer; the explicit non-claims; and a skeptical
  storage-engineer reader check.
* **3FS contradictions.** `docs/THREEFS.md` and
  `docs/THREEFS-CONTEXT-PLANE.md` now carry a prominent status paragraph: the
  shipped court is an **ABI-compatible shim** loaded with `dlopen`, not a
  deployed cluster, and any deployed-3FS latency conclusion is conditional on
  the unrun §6 experiment. The earlier 1,000× ingest unit error remains
  corrected (148–549 ms per tensor, not microseconds). The context-plane mode
  range is updated from 0–17 to 0–18.
* **Go parity corrections.** `docs/store-status.md` no longer says Go rejects
  `weft.manifest.v2`, rejects `kind:"tensor"`, or has no v4 writer/compaction;
  it now records the Go v2 reader/writer, the Go tensor store surface, and the
  Go v4 writer/compaction with the cross-language byte-equality tests. The
  historical R9/R11/R17 residuals in this file are annotated as closed by
  R20/R25 rather than presented as current. `docs/PLANES.md` records the Go
  plane split and the S3, GCS, and Azure planes, including the R14 Go Azure
  Blob backend. The
  “next work queue” entries for v2 and WFX2 parity are marked closed.
* **Claims.** 20 new entries cover the positioning wording, the six matrix
  categories, the 3FS shim statement, the MW1 statement, the MW2/MW3 absence,
  FUSE-on-FUSE rejection, the no-data-plane-competition statement, the Go
  S3/GCS statement, and the Go v2/v4 parity corrections. The stale
  current-tense sentence about the Go v4 writer is gone; the manifest
  keeps an `absent` guard against it. `readme_region_read_ms`
  is corrected from `749 ms` to `749.3 ms` and from “100-row” to “one-row”.

### Gates at this change

`scripts/check-claims.sh` **697/697 OK**;
`scripts/check-spec-drift.sh` **42/42 OK**;
`scripts/gen-conformance-manifest.sh --check` **355 entries OK** (aggregate
`9818b5d3eb1e1f35…`). No Rust/Go suite, FUSE court, or `verify-all` run is
claimed because no code changed. The previous release-tree aggregate remains
`verify-20261109T112958Z.json` (37/37).

## Subphase R3 — same-backend augmentation A/B/C/D/E (2026-11-09)

**Status: implemented, executed, and published, including every loss.** This is
the first augmentation result measured on one backend per court with codec and
protocol contributions isolated. Method is frozen in
`bench/CONTRACTS-augmentation.md`; evidence is
`bench/RESULTS-augmentation.md`; harness is `examples/augmentation_bench.rs` plus
`scripts/augmentation_court.sh`.

### What was added

- Five configurations defined so they cannot drift: **A Native** (plain
  per-tensor files or one PUT per object), **B Pass-through** (WeftFS raw, no
  protocol metadata), **C Codec** (WeftFS planned representation, no protocol),
  **D Protocol** (raw + Context Root, selector, provenance), **E Combined**
  (C + D). A supplementary **N** (`native-app-equivalent`) baseline adds a
  checksummed manifest, an atomically published key-set context, and a
  provenance record over plain files, so the protocol delta is not confused
  with the absence of any application scheme.
- One command per backend runs the configurations in fresh child processes and
  writes `weft.augmentation.v1` plus a raw log. Local:
  `scripts/augmentation_court.sh --local`. MinIO:
  `scripts/augmentation_court.sh --minio`. Reduced aggregate:
  `WEFT_AUGMENTATION_REDUCED=1 scripts/augmentation_court.sh --local`.
- Fixture: real Qwen2.5-0.5B-Instruct safetensors, whole-file SHA-256 verified in
  the court. The whole-fixture gate is measured as `fixture_verify_ms`
  (412–438 ms), reported, and excluded from every configuration's write wall
  equally because it is a court integrity check, not an augmentation cost.
  Published subset: the 272,269,312-byte embedding plus 100 evenly spaced
  name-sorted tensors, **101 tensors / 424,439,296 logical bytes**. The
  aggregate reduced subset is 16 non-embedding tensors / 1,849,344 bytes.
- Metrics: durable write wall, logical bytes, physical bytes written (backend
  counters, including probe/LOCK/FORMAT/superblock/commits/manifests/chunks/
  index segments), footprint (file walk or object listing), cold and warm
  restore wall, physical bytes read, write/read amplification, p50/p95/p99,
  `VmHWM`, CPU user+sys, request counts, logical and physical throughput
  separately, per-tensor chosen representation, and exactness. Unexecuted
  backends are `NOT MEASURED` with a blocker, never zero.

### Measured (local POSIX, 424,439,296 logical bytes)

| Metric | A | B | C | D | E | N |
|---|---:|---:|---:|---:|---:|---:|
| Write wall (ms) | 335.9 | 2,852.7 | 24,840.1 | 2,879.2 | 24,789.2 | 337.7 |
| Physical written (B) | 424,439,296 | 427,053,105 | 284,561,645 | 427,391,455 | 284,899,995 | 424,477,171 |
| Footprint (B) | 424,439,296 | 426,630,999 | 284,139,539 | 427,146,560 | 284,655,442 | 424,477,171 |
| Cold restore (ms) | 348.2 | 543.6 | 1,425.8 | 552.1 | 1,418.2 | 345.8 |
| Warm restore (ms) | 191.7 | 284.0 | 1,301.4 | 284.4 | 1,277.8 | 190.1 |
| Cold physical read (B) | 424,439,296 | 424,782,640 | 282,220,893 | 425,177,636 | 282,616,733 | 424,477,171 |
| Write / read amplification | 1.0000 / 1.0000 | 1.0062 / 1.0008 | 0.6704 / 0.6649 | 1.0070 / 1.0017 | 0.6712 / 0.6659 | 1.0001 / 1.0001 |
| Logical restore throughput (MB/s) | 1,218.9 | 780.8 | 297.7 | 768.8 | 299.3 | 1,227.5 |
| Physical restore throughput (MB/s) | 1,218.9 | 781.5 | 197.9 | 770.1 | 199.3 | 1,227.6 |
| p50 / p95 / p99 (µs) | 74 / 7,159 / 7,209 | 104 / 10,500 / 11,078 | 127 / 28,485 / 29,341 | 109 / 10,515 / 11,248 | 128 / 27,921 / 28,985 | 74 / 7,091 / 7,226 |
| Write / restore peak RSS (KiB) | 270,584 / 269,184 | 271,692 / 273,032 | 1,187,552 / 714,360 | 271,424 / 273,628 | 1,187,136 / 714,828 | 270,380 / 269,548 |
| Write / restore CPU (ms) | 100 / 190 | 270 / 310 | 23,210 / 1,300 | 280 / 320 | 23,180 / 1,300 | 100 / 190 |

Deltas against A: B −56.1 % latency, −35.9 % logical throughput, −0.52 %
storage, **+396.5 %** end-to-end overhead; C −309.5 %, −75.6 %, **+33.1 %**
storage, **+3,739.6 %** overhead; D −58.6 %, −36.9 %, −0.64 %, **+401.6 %**;
E −307.3 %, −75.4 %, **+32.9 %**, **+3,731.1 %**; N +0.7 %, +0.7 %, −0.009 %,
−0.08 %.

### Measured (MinIO RELEASE.2025-04-22T22-12-26Z, same subset)

| Metric | A | B | C | D | E | N |
|---|---:|---:|---:|---:|---:|---:|
| Write wall (ms) | 825.7 | 1,603.5 | 23,708.1 | 1,653.1 | 23,605.9 | 838.2 |
| Physical written (B) | 424,439,296 | 427,529,404 | 285,037,944 | 427,867,754 | 285,376,294 | 424,439,296 |
| Footprint (B) | 424,439,296 | 426,631,264 | 284,139,804 | 427,146,826 | 284,655,708 | 424,477,171 |
| Cold restore (ms) | 293.2 | 760.8 | 1,522.5 | 808.5 | 1,566.1 | 299.1 |
| Cold physical read (B) | 424,439,296 | 424,782,640 | 282,220,893 | 425,177,636 | 282,616,733 | 424,477,171 |
| Write / read amplification | 1.0000 / 1.0000 | 1.0073 / 1.0008 | 0.6716 / 0.6649 | 1.0081 / 1.0017 | 0.6724 / 0.6659 | 1.0000 / 1.0001 |
| Logical restore throughput (MB/s) | 1,447.7 | 557.9 | 278.8 | 524.9 | 271.0 | 1,419.3 |
| Write / restore peak RSS (KiB) | 272,404 / 274,408 | 272,692 / 274,380 | 1,189,004 / 716,300 | 272,692 / 274,744 | 1,188,600 / 717,152 | 272,500 / 272,356 |
| Write / restore CPU (ms) | 250 / 220 | 450 / 310 | 22,840 / 1,330 | 460 / 310 | 22,800 / 1,310 | 250 / 220 |

Deltas against A: B −159.5 % latency, −61.5 % logical throughput, −0.52 %
storage, **+111.3 %** overhead; C −419.3 %, −80.7 %, **+33.1 %**,
**+2,155.1 %**; D −175.8 %, −63.7 %, −0.64 %, **+120.0 %**; E −434.2 %,
−81.3 %, **+32.9 %**, **+2,149.8 %**; N −2.0 %, −2.0 %, −0.009 %, +1.65 %.

### Exactness

Every configuration restored all 101 tensors and every SHA-256 matched the
source on both backends: `mismatches: 0` in every A–E/N row of the three
published receipts. The reduced receipt also has `mismatches: 0` for all six
rows. D/E additionally verified the resolved object set equals the written set
and the provenance record's 101 output roots equal the written manifest
contents.

### Losses (all published in the results document)

- **C/E win bytes and lose time.** −33.1 %/-32.9 % storage and −33.5 % physical
  reads, but **24,840.1 ms vs 335.9 ms** local write wall (73.9x), 1,425.8 ms
  vs 348.2 ms cold restore (4.1x), 1.19 GiB vs 270 MiB write RSS. ZSTD-19 and
  mode-9 encoding are 23.2 s of CPU. MinIO is the same shape (28.7x write,
  5.2x restore).
- **B and D are slower than A and slightly larger.** B footprint is +0.52 %,
  cold restore +56 % local, write wall 8.5x. D is +0.64 % footprint and +58.6 %
  latency. At 101 objects the store's commits/manifests/indexes do not pay for
  themselves on a plain file-tree workload.
- **Protocol cost is small but real.** D vs B: +0.9 % write wall, +1.6 % cold
  restore locally; +3.1 % and +6.3 % on MinIO. D's footprint is +0.12 % over B.
- **Small-object cost is severe.** The 1.8 MB reduced run measured B
  **+1,032 %** and D **+1,246 %** end-to-end overhead versus A (19.2 ms write /
  4.0 ms restore); per-commit metadata dominates. C still cut storage 27.4 % but
  was 3.0x slower to restore.
- **N is essentially A.** It achieves the protocol function with three metadata
  objects and no store machinery (+0.5 % write local, +1.5 % MinIO). D's cost
  buys snapshots, a selector engine, capability/version history, and
  reconstruction semantics that N does not implement.
- **Warm/cold is not fully controlled.** Local cold uses `fadvise(DONTNEED)`;
  warm is a second pass. An earlier clean run measured A 563 ms / B 777 ms
  against this run's 348 / 544 ms; the difference is page-cache state. A repeat
  (`augmentation-local-20261109T133923Z.json`) was contaminated by host I/O
  wait (D write 15.3 s with 0.3 s CPU; E 49.2 s) and is retained but not used
  in the table. MinIO has no cold-cache control and its A cold restore is
  faster than local A. No cold-media claim.
- **MinIO tmpfs restart.** MinIO on a 1 GiB tmpfs charges deleted-but-open bytes
  until process restart (`du` 0, `df` 406 MiB measured). The court restarts the
  container between configurations; each configuration's write and restore
  complete before its restart. Recorded in the receipt and contract.
  `bench/receipts/augmentation-minio-20261109T131418Z.json` is the retained
  failure that found it.

### Receipts and gates

- Local full: `bench/receipts/augmentation-local-20261109T134132Z.json`
- MinIO full: `bench/receipts/augmentation-minio-20261109T134257Z.json`
- Local reduced (aggregate configuration):
  `bench/receipts/augmentation-local-20261109T134434Z.json`
- Retained contaminated repeat:
  `bench/receipts/augmentation-local-20261109T133923Z.json`
- Retained MinIO failure (tmpfs):
  `bench/receipts/augmentation-minio-20261109T131418Z.json`
- The aggregate `verify-all.sh` now includes `augmentation_local` with
  `WEFT_AUGMENTATION_REDUCED=1`; a court that cannot run prints
  `augmentation_court: skip <reason>` and exits 78, which the aggregate records
  as `skip`, never as a pass.

Gate results for this change: `cargo test --all-targets` in the rust court
**675 passed / 0 failed / 0 ignored across 134 suites**; the FUSE suite in the
fuse court **718 passed / 0 failed / 2 pre-existing ignored across 134 suites**;
`scripts/check-claims.sh` **709/709 OK**; `scripts/check-spec-drift.sh`
**42/42 OK**; `scripts/gen-conformance-manifest.sh --check` **355 entries OK**.
`scripts/verify-all.sh` was not rerun for R3: the change is a new example, a
new court script, a one-line environment-forwarding addition to
`scripts/docker-court.sh`, and documentation; no library code changed, and both
the full local/MinIO courts and the reduced aggregate configuration were
executed directly. The aggregate wiring is syntax-checked (`bash -n`) and the
skip path is measured: with an unreachable Docker daemon the court prints
`augmentation_court: skip no reachable docker daemon` and exits 78.

### Residual

Rust-only harness (no Go augmentation court); one host, warm source reads, one
published run per cell plus the retained repeats; no live AWS/GCS/Azure; no
3FS/CephFS/Lustre/BeeGFS/DAOS/JuiceFS/IBM Storage Scale/WEKA/VAST/AIStore
execution; no physical media or power-loss claim; no multi-writer or fleet
claim; MinIO restart between configurations; the reduced aggregate configuration
is not a performance claim.

---

## Subphase R4 — multi-writer (2026-11-09)

**Declared level at R4: MW1 is the default; MW2 is implemented and opt-in; MW3
was not implemented and not claimed.** *(Superseded 2026-11-14 for MW3 only; the
MW2 statement still holds.)* The normative contract is
[`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md). The design and
backend tradeoffs are in
[`docs/MULTIWRITER-ARCHITECTURE.md`](MULTIWRITER-ARCHITECTURE.md); the frozen
method is [`bench/CONTRACTS-multiwriter.md`](../bench/CONTRACTS-multiwriter.md);
measurements and every loss are in
[`bench/RESULTS-multiwriter.md`](../bench/RESULTS-multiwriter.md).

### What was built

* **Specification.** MW0-MW3 capability levels; per-operation consistency
  contract; transaction lifecycle (`prepared`, `committed`, `visible`,
  `durable`, `aborted`, `outcome_unknown`); conflict table with no
  last-writer-wins path; transaction-id idempotency and the op-digest proof;
  the durability boundary of an acknowledgement.
* **Executable model.** `verification/tla/WeftMultiWriter.tla` (2 committers,
  2 keys, MaxGen=2, MaxAttempts=1): 2,690,909 states generated, 268,641
  distinct, depth 20; `TypeOK`, `HeadWellFormed`, `NoPartial`, `NoLostAck`,
  `NoBlindOverwrite`, `CommittedBlobsDurable`, `LivePreparedDurable`,
  `ReaderIsAPrefix`, `GcNeverReclaimsReachable`, and `CommitOrderIsStable`
  pass. Two deliberate flaws produce counterexamples:
  `WeftMultiWriterBuggy.tla` without the observed version check
  (`NoBlindOverwrite` violation, last-writer-wins) and
  `WeftMultiWriterGcBuggy.tla` marking only from the head
  (`GcNeverReclaimsReachable` violation, GC reclaims a live prepared
  transaction's blob). Receipt `bench/receipts/tla-20261109T174959Z.json`
  (5 models: 2 `pass`, 3 `expected-violation`).
* **Rust MW2.** `src/mw2.rs` and `Store::{begin_transaction`,
  prepare_content, prepare_transaction, execute_transaction,
  transaction_outcome, refresh_to_head}`; conditional head publication through
  `CommitPlane` for `local`/`object`/`3fs`/`s3`; one conditional `PUT If-Match`
  on S3; `--writer-mode single|optimistic` (default `single`). The single-writer
  path and every pre-R4 test are unchanged.
* **Go parity.** `WeftFS-Go/store/mw2.go` and `cmd/mw2writer` implement the same
  protocol on `local`/`object`. Deterministic stores are byte-identical:
  `commits/2.commit` and the 8 KiB `superblock` image match, and each language
  reads the other's store.
* **Failure court.** Both languages killed after the prepared record, before
  the head write, after the head write, and after the acknowledgement. Every
  reopened store is whole; initial outcome is `unknown` except after the
  acknowledgement; the retry resolves it; the name is absent or exact, never
  wrong.
* **Benchmark.** 14 rows: local disjoint N=1..32, local high contention
  N=1..32, mixed Rust/Go N=8, MinIO N=4.

### Conflict classes and outcomes

`NameExists`, `ObjectExists`, `ObjectChanged`, `EntryChanged`,
`RelationConflict`, `SnapshotConflict`, `TipChanged` -> typed `Conflict`, no
publication. `QuotaExceeded` -> `BudgetExceeded`. `CapabilityDenied` ->
`Unauthorized`. `TransactionIdReused` -> `Conflict`. `GenerationCollision` ->
`Busy` -> retry. `ParentAdvanced` -> retry, then re-validate. A create uses
`put_entry_create`/`put_object_create`; a replace requires the observed version.
There is no last-writer-wins path.

### Local disjoint scaling (receipt `bench-multiwriter-20261109T175227Z`)

| N | committed | tx/s | efficiency | p50 ms | p99 ms | retry amp | reads/commit |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 60/60 | 4,000.0 | 1.0000 | 0.182 | 0.224 | 1.000 | 3.2 |
| 2 | 120/120 | 3,333.3 | 0.4167 | 0.211 | 0.468 | 1.050 | 3.8 |
| 4 | 240/240 | 2,105.3 | 0.1316 | 0.242 | 17.350 | 1.096 | 4.9 |
| 8 | 480/480 | 1,672.5 | 0.0523 | 0.327 | 60.835 | 1.146 | 7.0 |
| 16 | 240/240 | 710.1 | 0.0111 | 0.263 | 272.478 | 1.683 | 13.1 |
| 32 | 224/224 | 580.3 | 0.0045 | 0.285 | 302.229 | 2.549 | 31.4 |

High contention: 1 winner at every N; conflict rate 0.90 (N=1) to 0.997
(N=32); no last-writer-wins. Mixed N=8: 240/240, 1,804.5 tx/s, retry 1.212,
all 16 cross-language verifiers pass. MinIO N=4: 40/40, 224.7 tx/s, p50
4.638 ms, p99 64.074 ms, retry 1.275, `verify --full` ok. Per-writer p50/p95/p99
are in the receipt and in `bench/RESULTS-multiwriter.md`; at N=32 the per-writer
p99 ranges 1.36-377 ms (median 139 ms), so the tail is not shared evenly.
`aborted` was 0 in every row.

### Defects found and fixed during R4

* **Retry-state pollution.** The optimistic loop assigned the candidate state
  before publication and did not restore it after a lost race, so a retry
  evaluated quota, preconditions, and authorization against a state containing
  its own unpublished ops. Fixed by restoring the last published base; the
  regression test `a_lost_publication_race_does_not_pollute_the_retry_state`
  fails when the restore is removed (verified by temporarily disabling it).
* **Ambiguous head write.** A non-`Busy` failure of the conditional head write
  used to delete the commit record. The write may have landed, so deleting it
  could destroy a published commit. Deletion is now limited to a definitive
  `Busy`; an ambiguous record is left for outcome lookup and `verify`. This is
  reasoned, not fault-injection-tested (no I/O fault injector exists).
* **S3 commit-file collision.** A create-once `Exists` on `commits/<g>.commit`
  is now `Busy` (retry) instead of a fatal `Exists`; the retained failing
  receipt is `bench-multiwriter-20261109T144320Z.json`.

### Every loss

Scaling efficiency falls to 0.0045 at N=32; p99 rises to 302 ms; retry
amplification reaches 2.55 and 31.4 backend reads per commit; RSS is about
11.0 MiB per idle writer at N=32; a hot name is unproductive by design; MinIO
is 9.4x slower than local at N=4; there is no validate/publish split, logical
bytes exclude canonical records, CPU is USER_HZ ticks (not energy), Go exposes
no backend counters, the run is a single repetition on 4 vCPUs with up to 19 %
spread against the retained earlier run; GCS/Azure are refused for optimistic
mode; Go has no S3 MW2, no FUSE MW2, and no generic checkpoint transaction;
FUSE and checkpoint publication remain MW1; GC is not run concurrently with
optimistic publishers; there is no physical power loss, no fleet, and no MW3.

### Gates and receipts

* Rust + FUSE in the fuse court: **136 suites, 733 passed, 0 failed, 2
  pre-existing ignored**.
* Go: `gofmt -l`, `go vet`, `CGO_ENABLED=0 go build`, and
  `go test -race -count=1 ./...` clean in the fuse court.
* Mixed court: `bench/receipts/multiwriter-court-20261109T175012Z.json`
  (160/160 disjoint, 1/80 contended winner, 16 verifiers, `verify --full`).
* Failure court: `bench/receipts/multiwriter-failure-20261109T175014Z.json`
  (8 process faults, replay, delayed writer, quota race).
* Benchmark: `bench/receipts/bench-multiwriter-20261109T175227Z.json` and
  `.log`; retained superseded runs `…150423Z` and `…144401Z`; retained failures
  `…144140Z`, `…144235Z`, `…144320Z`, `…144333Z`, `…144351Z.log`.
* TLA+: `bench/receipts/tla-20261109T174959Z.json`.
* Aggregate: `bench/receipts/verify-20261109T165707Z.json` (**41/41** courts
  `pass`, 0 failed, 0 skipped) at WeftFS `a3256af`; the earlier 41/41 run
  `verify-20261109T160406Z.json` (at `257f31a`, before the spec-wording commit)
  and the 38/38 run `verify-20261109T150758Z.json` are retained.
* Code revisions: WeftFS `b0bcbc5` (protocol, courts, benchmark), WeftFS-Go
  `f46b351`; aggregate at `a3256af`.

### What remained for MW3 at R4

*(2026-11-14: the conflict-domain item is now an opt-in Rust prototype —
`spec/WEFT-MULTIWRITER.md` §15 — and the remaining items are listed in
`bench/RESULTS-mw3.md` §5 and `docs/MULTIWRITER-ARCHITECTURE.md` §11.)*
Conflict domains or sharded heads so independent transactions stop contending
on one publication point; a shared multi-writer daemon instead of one process
per writer; a coordinated multi-writer GC protocol; a multi-node court with a
real consensus substrate and a fleet throughput claim; a publication path that
does not re-read the chain on every retry; Go parity for S3/GCS/Azure MW2; and
MW2 for FUSE file mutations and generic checkpoint publication.

## Subphase R5 — provenance-bound KV caches (2026-11-09)

**Goal.** Mode 12 `KV_CACHE` binds a page's bytes and geometry but not the model
weights, adapters, tokenizer, attention configuration, positional treatment,
layout, or sharing domain that produced it. Provenance lived only in store
metadata, so a consumer resolving by content hash alone could reuse an
incompatible prefix silently. R5 makes that structurally impossible at the byte
level: the provenance dimensions are inside the frame descriptor and the page
identity is a hash over them.

### What was added

* **`spec/WEFT-KV-PROVENANCE.md`** (normative, exported to all three mirrors):
  the ordered dimension registry, the exact descriptor key set, the
  cross-checks against the frozen page, the sharing-domain salt's purpose and
  non-claims, the validation order, and the store/capability scope.
* **Mode 19 `KV_CACHE_PROV`** (`weft.ai.kv_prov.v1`) in Rust `weftcodec::kv_prov`
  and Go `modes`. The payload is the mode-12 payload verbatim; the only wire
  difference is the descriptor envelope. Identity:
  `descriptor_root = H("weft/kv/prov/v1", WCB(descriptor))`. Mode 12 is
  byte-identical; the reserved reject moved to mode 20 with `017_unknown_mode_reserved19`
  retained and superseded.
* **20 conformance vectors** in `conformance/v1/kv-prov/` (8 valid, 12 reject),
  byte-identical in both codec repositories, replayed by both test suites.
* **Adversarial court** `tests/kv_provenance.rs` (Rust store + codec) and
  `WeftCodec-Go/conformance/kv_prov_test.go` (codec cases).
* **Store/capability path** (`src/kv_capability.rs`): `write_kv_prov_page`,
  `read_kv_prov_page`, `kv_prov_descriptor`, `kv_page_mode`. The capability
  descriptor `weft.kv.prov_capability.v1` adds `provenance_root` to the mode-12
  fields, so a grant that names its id names the provenance as well as the page.
  The uniform denial text and the authorization-before-decode ordering are
  unchanged.
* **Bench** `examples/kv_provenance_bench.rs` and
  `bench/RESULTS-kv-provenance.md`.

### Collision and fail-closed results

Receipt `bench/receipts/kv-provenance-court-20261109T182750Z.json` (5/5 tests,
raw log beside it; the earlier `…182414Z` receipt is retained, it predates the
`verify --full` row). Two frames with byte-identical 4,096-byte payloads and the
same payload hash (`a126fc3d…7246c9`) but different adapter roots have different
roots (`ba5ec110…5d7f5f` vs `ef7309ea…24618e`); cross-resolving either under the
other's root returns `Unauthorized` with **0 bytes emitted**, while the matching
scope returns 4,096 bytes. Salt `b0…`/`b1…` changes the root and nothing else.
`{A,B}` and `{B,A}` are one frame; `{A}` differs. A tampered dimension root is
`Malformed`; a swapped payload is `Corrupt`.

Store scope (one 4,096-byte page, capability grants listed by `roots`):

| scope named by the capability | emitted bytes | class |
|---|---:|---|
| provenance descriptor id | 4096 | `ok` |
| plain mode-12 descriptor id | 0 | `Unauthorized` |
| ContentId + ObjectId | 0 | `Unauthorized` |
| another page's provenance descriptor | 0 | `Unauthorized` |
| no envelope | 0 | `Unauthorized` |
| mode-19 object through the mode-12 reader | 0 | `Corrupt` |
| mode-12 object through the provenance reader | 0 | `Corrupt` |
| `Store::verify(true)` over the mode-19 objects | — | `ok` (2 objects) |

A raw `read_object` returns 5,509 bytes that start with `WFT1` and are not the
plaintext, so a content-hash fetch yields a frame, never a decode.

### Cost

Receipts `bench/receipts/kv-provenance-20261109T181406Z.json` (cited) and
`…181706Z.json` (repeat). BF16, 4×64 heads, two adapters, one extension, 40 reps.

| page | mode-12 frame | mode-19 frame | descriptor | overhead |
|---:|---:|---:|---:|---:|
| 1 KiB | 1,858 B | 2,501 B | 909 B | +643 B (+34.61 %) |
| 64 KiB | 66,422 B | 67,066 B | 914 B | +644 B (+0.97 %) |

p50 encode 266.9 → 276.1 µs (1 KiB) and 508.2 → 541.8 µs (64 KiB); p50 decode
11.1 → 28.5 µs and 159.0 → 182.2 µs; descriptor validation alone 16.9 / 19.0 µs;
authorized store read 35.0 → 53.0 µs and 280.5 → 301.8 µs. Repeat run within
noise (decode 11.2 → 28.4 and 159.2 → 179.4 µs). Peak RSS 5,948 KiB.

### Every loss

Small pages pay a large relative cost (+34.61 % bytes; decode more than
doubles) because descriptor validation is a fixed ~17-19 µs. The salt changes
identifiers only: it does not hide timing, sizes, access patterns, or existence,
and anyone who can read the frame sees it. A caller that holds the raw bytes and
bypasses the store can still call the codec directly; the guarantee is about the
store API. No Go store parity (the KV capability surface is Rust-only). No CLI
verb for provenance-bound KV writes. No serving integration, no
time-to-first-token claim, synthetic pages, one host, warm cache. Not added to
`verify-all`; the court is a normal test and the bench is run manually.

### Gates

* WeftCodec `cargo test --all-targets`: all suites pass; new `tests/kv_prov.rs`
  9/9.
* WeftCodec-Go `gofmt -l` empty, `go vet`, `CGO_ENABLED=0 go build`,
  `go test -race -count=1 ./...`: all pass; new conformance replay 20/20.
* WeftFS `cargo test --all-targets`: **695 passed, 0 failed, 0 ignored**.
* FUSE court `cargo test --features fuse --all-targets`: **738 passed, 0 failed,
  2 pre-existing ignored**.
* Cross gate: `PASS rust->go kv_prov_p0`, `PASS go->rust kv_prov_p0`,
  `PASS identical kv_prov_p0`, `YES byte-identical kv_prov_p0`, all checks
  passed.
* Conformance manifest 376 entries (aggregate `9fb69ebf…a91177`), coverage
  mode 19 `8/12`; Rust and Go adapters 222 pass / 0 fail / 33 skip (frame kind).
* Spec drift 48/48.

## R6 — retention bindings and class-aware GC (2026-11-09)

A storage layer cannot distinguish minute-scale utility from day-scale utility
from bytes alone. This subphase adds a **retention binding** as a second,
mutable, versioned layer that a GC planner can read, without touching artifact
identity. Normative: `spec/WEFT-RETENTION.md` (`weft.retention.v1`).

### The identity separation

For a mode-19 KV page, identity remains
`descriptor_root = H("weft/kv/prov/v1", WCB(descriptor))`. A binding references
that root and declares `class`, `declared_lifetime_ms` (admission hint only),
`producer`, `policy_epoch`, `issued_at`, optional `supersedes`, and an optional
Ed25519 `signature`; its identity is
`RetentionId = H("weft/retention/v1", WCB(record))`. No binding field is an
input to any descriptor, content root, `ContentId`, `ObjectId`, manifest, or
dedup key. Publishing, rebinding, or deleting a binding writes **zero artifact
bytes**. Two artifacts with identical bytes and different policies have
identical content roots and different `RetentionId`s. Rebinding is append-only
(`supersedes`) and epoch-fenced (a lower epoch is `Conflict`).

### Classes and the safety invariant

`volatile` (seconds-to-minutes), `session` (hours), `ephemeral` (days),
`durable` (indefinite; requires an in-memory pin, a snapshot root, or a
checkpoint root, or the bind is refused). Class is never inferred from size,
age, path, dtype, mode, or any other observable. GC computes reachability
exactly as reachability-only GC does; class affects **ordering**, never
reachability safety:

> A class-aware pass reclaims a subset of what a reachability-only pass would
> reclaim. It may retain unreachable bytes for a live binding's declared
> window; it may never reclaim a blob that reachability GC would keep.

Tests: `tests/retention.rs` 8/8, including
`class_aware_gc_never_reclaims_a_reachable_blob` (snapshot-, checkpoint-,
content-pin-, and provenance-retained artifacts, all expired, zero-budget
plan); Go `store/retention_test.go` 7/7 with the same invariant.

### Rust and Go surfaces

* Rust: `src/retention.rs`, `Store::{set_retention, retention_of,
  retention_history, retention_pointer, retention_binding, gc_plan,
  retention_gc}`, `RetentionGcPlan` reporting per-class retained/reclaimable
  blobs and bytes, plus the reason for every retained-but-expired artifact
  (`reachable from <root>` or `retention live until <ns>`). The store's GC
  reachability code was factored into one `compute_reachability` used by both
  `Store::gc` and the retention planner, and now records root labels
  (`head:`, `snapshot:`, `checkpoint:`, `transaction:`, `pin:`).
* CLI: `weftfs retention set|show|history|plan|gc --apply`.
* Go: `WeftFS-Go/store/retention.go` implements the same canonical record,
  fixed 104-byte pointer, history, durable-root check, signed-record
  verification, and GC ordering, reading the same `/retention/...` committed
  layout. Go has no mode-19 KV writer, so its request supplies the artifact
  anchor explicitly.
* Cross-language (`scripts/retention_cross.sh`): **12/12 checks**, Rust record →
  Go and Go record → Rust identical SHA-256/`RetentionId`; a Rust-written store
  binding read by Go; a Go rebind read by Rust with the same bytes, id, and
  class. `YES byte-identical` for both the record and the store binding.

### The SWA-style court

`examples/retention_court.rs` + `scripts/retention_court.sh`, receipt
`bench/receipts/retention-20261109T220237Z.json` at WeftFS `9de0e09` /
WeftCodec `813171b` / WeftCodec-Go `c872f26` / WeftFS-Go `9a87f56`. Synthetic,
deterministic; bench court; 1,000 volatile pages (300 s) and 300 ephemeral pages
(6 h), 360 × 60 s ticks, two request rates, 3.0 MB capacity, 20 snapshot-
protected pages.

| Rate | Policy | Safe-hit | Retained B | Reclaimed B | Unsafe | Physical B |
|---|---|---:|---:|---:|---:|---:|
| fast | fixed TTL + LRU | 35.31 % | 123,264 | 12,302,432 | 45 | 6,457,356 |
| fast | reachability-only | 1.50 % | 68,480 | 16,524,224 | 0 | 5,012,636 |
| fast | **class-aware** | **74.94 %** | 3,002,848 | 4,536,800 | 0 | 4,340,836 |
| slow | fixed TTL + LRU | 0.97 % | 34,240 | 6,858,272 | 37 | 4,416,301 |
| slow | reachability-only | 2.64 % | 68,480 | 6,782,944 | 0 | 2,766,411 |
| slow | **class-aware** | **73.47 %** | 3,002,848 | 2,102,336 | 0 | 4,051,586 |

Every hit re-reads the exact frame and compares it byte-for-byte; every run
ends with `verify --full`. Class-aware beats fixed TTL by 39.6 points (fast) and
72.5 points (slow). Fixed TTL evicts 45 / 37 still-reachable artifacts; the
snapshot keeps their bytes, which is why an unsafe cache also cannot free that
capacity. Reachability-only and class-aware report zero unsafe reclaims.

Misclassification (class-aware, fast rate): declaring volatile pages
`ephemeral` retains **+1,448,352 B** and reclaims 0 (a capacity loss);
declaring ephemeral pages `volatile` costs **−7.69 points**, 277 extra misses,
and 453 early reclaims, with **0 unsafe reclaims** — reachability still governs.

### Every loss

Class-aware retains 3.00 MB against a 3.00 MB budget: 2,848 B over at the end
(0.09 %), with a 1,451,200 B peak over-budget from the initial 1,300-page
seeding burst. Retention is a hint: no wall-clock guarantee, no clock-skew
defence, and no verification that a producer's declaration is truthful. The
trace is synthetic; the win is a property of this reuse distance and would be
published as a tie if the distance were shorter than every window. No fleet,
media, or serving claim. The court is 1,120.6 s and is not part of `verify-all`.
The Go store has no mode-19 writer, so the anchor is explicit. Signatures verify
against an embedded key; without a configured trust set they are not an
authorization claim. `durable` is protected by reachability, not by a clock:
once unreachable it is ordered last and reclaimed.

### Gates

* Rust `cargo test --all-targets`: **708 passed, 0 failed, 0 ignored** (141
  suites).
* FUSE court `cargo test --features fuse --all-targets`: **751 passed, 0
  failed, 2 pre-existing ignored** (141 suites).
* WeftFS-Go: `gofmt` clean, `go vet ./...` clean, `CGO_ENABLED=0 go build ./...`
  clean, `go test -race -count=1 ./...` pass in the fuse court (cmd 45.8 s,
  fusefs 7.1 s, store 209.6 s). The go court alone lacks `/bin/fusermount`; the
  fuse court is the documented environment for that package.
* Superseded receipts `retention-20261109T205135Z.json` and
  `retention-20261109T211525Z.json` measured the court before fixed-TTL
  evictions were batched into one group commit; their fixed-TTL physical bytes
  and wall time are inflated by one commit per unlink and are not cited as the
  result.
* Claims, spec drift, and the conformance manifest are checked in the R6 docs
  commit.

## R7 — mode 20 `ENGRAM_SHARD`: region-addressable, incrementally updateable row groups

Normative spec: `spec/WEFT-ENGRAM.md` (`weft.ai.engram_shard.v1`, mode 20).
The reserved-mode reject moved to 21; `017_unknown_mode_reserved20` and the
kv-prov `112_reserved_mode_20` vector are retained as superseded. The spec is
exported to `WeftCodec/spec`, `WeftCodec-Go/spec`, and `WeftFS-Go/spec`.

### Wire format

Frame: domain 0, mode 20, no chunk table, `body = u32le(descriptor_len) ||
WCB(descriptor) || stored_payload`. Descriptor key set (exactly):
`schema`, `hash_function`, `vocab_size`, `table_shape`, `element_dtype`,
`precision`, `row_group_edges`, `group_count`, `groups`, `payload_len`,
`payload_hash`. Each group entry has exactly `index`, `row_start`, `row_end`,
`storage`, `offset`, `compressed_len`, `group_root`, `supersedes`,
`dependency`. `precision` has `storage_dtype` plus an optional `quantization`
block; only `scheme = "none"` with `lossless = true` is accepted, and any other
scheme is `UnsupportedFeature`.

A group is split into byte planes, each coded independently with the frozen
`huffman-canonical-v1` coder (the mode-15 coder). Its identity is
`H("weft/engram/group/v1", WCB({group_index, row_start, row_end, dtype,
precision, values_hash}))`; the descriptor identity is
`H("weft/engram/v1", WCB(descriptor))`. A group may be `storage:"reference"` to
an earlier artifact's group root; the frame's dependency table names that
artifact and a caller-supplied resolver must produce the exact group bytes.
Supersession edges are acyclic and must resolve to a known group root or the
decode is `Corrupt`.

### Codec surface and vectors

Rust `weftcodec::engram` (`encode_engram`, `encode_engram_with_dependencies`,
`decode_engram`, `decode_engram_without_dependencies`, `parse_engram`,
`engram_metadata`, `decode_engram_group`, `read_engram_groups`,
`read_engram_groups_with`, `read_engram_region`, `read_engram_region_with`,
`engram_group_root`, `engram_region_groups`). Go `modes` has the same decode and
region surface plus `EncodeEngram` / `EncodeEngramWithDependencies`. CLI
`--mode engram-shard --dtype D --shape ROWS,DIM --group-edges A,B,...
--hash-function ngram-{sha256,blake3}-v1 --vocab-size N`.

`conformance/v1/engram/`: **6 valid** (`001` bf16 one group, `002` fp16 four
groups, `003` fp32 two groups, `004` empty supersession, `005` two-group
supersession chain, `006` region subset) and **14 rejects** (`101` edges not
increasing, `102` last edge != rows, `103` group count mismatch, `104` group
root mismatch, `105` truncated payload, `106` payload-hash mismatch, `107`
supersession cycle, `108` supersession missing, `109` incompatible supersession,
`110` unknown hash function, `111` unknown precision, `112` reserved mode 21,
`113` non-canonical descriptor, `114` reference missing). Byte-identical in
both codec repos. The cross gate prints `PASS rust->go engram_bf16`,
`PASS go->rust engram_bf16`, `PASS rust->go engram_fp32`,
`PASS go->rust engram_fp32`, `YES byte-identical engram_bf16`,
`YES byte-identical engram_fp32`, and `cross-language gate: all checks passed`.

### Store integration

Rust `TensorStrategy::EngramShard` is offered only when the caller supplies
`TensorPlanOptions.engram` (`EngramWriteSpec` with the edge list, hash
function, vocabulary size and optional precision); it is never auto-selected.
Full reads, row-region reads (only intersecting groups; Rust issues per-group
backend range reads), and `Store::update_engram_groups` are implemented.
`update_engram_groups` writes only the changed groups, emits the unchanged
groups as references to the previous artifact, records that artifact as the
representation dependency, and publishes a new version without rewriting the
old one. `tests/engram_store.rs` proves the exact full read, a one-group region,
a three-group region, the incremental update, the snapshot-visible old version,
and the reference-only payload.

Go (`WeftFS-Go`) reads, verifies and region-reads Rust-written mode-20 objects:
`store/tensor_read.go` resolves dependency frames and decodes mode 20;
`Store.ReadTensorRegion` decodes only the groups intersecting the region.
**Go has no mode-20 writer**; that is stated rather than implied.

### Court

`examples/engram_court.rs` + `scripts/engram_court.sh`; receipt
`bench/receipts/engram-20261109T232921Z.json` (`weft.engram.v1`), 17/17 checks
pass, run at WeftFS `5d63217`, WeftFS-Go `281e268`, WeftCodec `37bb199`,
WeftCodec-Go `9f49b0b`. Fixture: 131,072 × 1,024 bf16 (256 MiB), 64 groups of
2,048 rows; a deterministic xorshift64* generator makes `row % 64 == 0` active
(1.5625 %) and repeats a template row otherwise. Full results and every loss
are in `bench/RESULTS-engram.md`.

Headline numbers (one run, bench court, 3 GiB / 4 CPUs): mode-20 encode
499.8 ms / 108.74 MB frame; full decode 616.2 ms; single group 9.15 ms /
1.70 MB stored read / 1 group decoded; four non-contiguous groups 37.8 ms;
four contiguous groups 37.7 ms; sequential eight 75.6 ms; random eight 74.7 ms.
Baselines: raw `pread` one group 0.414 ms, four groups 2.68 ms; one ZSTD-3
frame 4.24 MB, full decode 57.0 ms; mode-15 tiles 108.74 MB, full decode
603.2 ms, single region 9.40 ms, contiguous four 37.2 ms. Incremental update of
4/64 groups: 62.7 ms / 16.79 MB vs full rewrite 499.7 ms / 118.73 MB
(7.97× / 7.07×), but incremental decode is 2.30 s vs 616 ms.

Untouched-group counter evidence: one group decodes 1; four non-contiguous 4;
four contiguous 4; sequential eight 8; random eight 8. The thread-local counter
equals the touched-set length on every read, so untouched groups are not
decoded.

Published losses: whole-table size 25.7× and decode 10.8× worse than one ZSTD
frame; single-group 22.1× slower than raw `pread`; four-group reads lose to raw
`pread`; contiguous four is 1.4 % slower than mode 15; single-group is
effectively tied with mode 15 (9.15 vs 9.40 ms, within noise); incremental
decode is 3.74× slower; per-group Huffman table overhead; partial reads do not
verify the trailer/payload hash/content hash; sequential and random reads are
not differentiated in memory; the Go store region read reads the whole frame
blob; the fixture is synthetic. No cache, fleet, or power-loss claim.

### Gates

* WeftCodec `cargo test --all-targets`: pass (including the new `tests/engram.rs`
  4/4 and the reserved-mode update).
* WeftCodec-Go `go test ./...`: pass (including `conformance` engram replay
  6 valid / 14 rejects, generic dispatch, and the kv-prov superseded skip);
  `gofmt`/`go vet`/CGO-disabled build clean.
* Rust `cargo test --all-targets`: 709 passed / 0 failed across 144 suites
  (new `tests/engram_store.rs` 1/1).
* FUSE suite: 752 passed / 0 failed / 2 pre-existing ignored across 144 suites.
* WeftFS-Go `go test -race ./...`: all packages pass, including the mode-20
  handoff against the committed Rust fixture; `gofmt -l` empty, `go vet` and
  `CGO_ENABLED=0 go build` clean.
* Claims 767/767, spec drift 54/54, conformance manifest 398 entries (aggregate
  `150115b4…d55a24`), coverage mode 20 `6/14`.
* Fixed a latent `tests/kv_provenance.rs` flake found by the first R7 full run:
  the helper passed two grant roots unsorted, and `CapabilityScope::new`
  canonicalizes, so ~half of runs got `NonCanonical`. The helper now sorts and
  deduplicates, matching `tests/kv_prefix.rs`; 40/40 repeated runs pass.

### Residual

Mode 20 loses whole-table size/decode to ZSTD and single/four-group time to raw
`pread` on this fixture. The Go store has no writer and reads the frame blob
whole for a region. The incremental decode re-resolves referenced groups.
`supersedes` is provenance, not a compaction mechanism. No per-group
calibration row was added to the planner (mode 20 is opt-in and priced with the
mode-15 coefficient). No physical power loss, fleet, or cache claim.

## R8 — transport-aligned container `weft.transport.v1` (2026-11-09)

This R8 is the transport-layout subphase; the earlier 2026-11-06 "R8" section
above is the reconstruction single-flight/cache subphase and is unrelated.

The 3FS court still proves an ABI-compatible `dlopen` shim, not a deployed
cluster. This subphase adds the one thing that can be stated honestly without
hardware: a **transport-aligned container** whose section can be fetched with
one aligned range read, plus proof of the alignment/parse properties and the
exact byte cost.

### Spec and wire layout

[`spec/WEFT-TRANSPORT.md`](../spec/WEFT-TRANSPORT.md) (`weft.transport.v1`) is
an additive envelope around complete, unmodified WFT1 frames. Enclosed bytes,
mode numbers, and content roots are unchanged.

* Fixed **4096-byte header**: magic `WFTT`, major/minor 1.0, flags (0 in v1),
  `section_count` (1..=4096), `section_table_offset` (4096), `total_len`,
  `payload_hash = H("weft/transport/v1", enclosed frames)`, reserved zero.
* **56-byte section entry**: `offset`, `length`, `alignment`
  (512, 4096, or 65536), mode byte, role byte, reserved zero, and the enclosed frame's
  `content_hash` as `root`.
* Every offset is a multiple of its declared alignment. Every section is
  padded outside its frame with zeros; padding is not hashed.
* Reader classes: overlap, out-of-bounds, misalignment, duplicate offsets,
  bad table count, non-zero padding, disallowed alignment and unknown role are
  `Malformed`; a wrong section root or mode is `Corrupt`.
* A reader may fetch one section without touching the others. A single-section
  reader verifies the section root, never the container-level `payload_hash`.
  A sub-section slice is not independently rooted; the enclosed mode-15/17/18
  per-tile roots or mode-20 per-group roots are what apply.

Non-claims are normative in the spec: this is a **layout, not a transport**;
no DMA, GPU memory, RDMA, kernel-bypass, or deployed-3FS claim is made, and
alignment alone does not make a decoder zero-copy.

### Implementation

* Rust: `weftcodec::transport` (`create`, `read_header`,
  `read_section_table`, `parse_metadata`, `verify`, `verify_payload`,
  `fetch_section`, `verify_section_bytes`), CLI
  `transport create|inspect|verify|read-section`.
* Go: `WeftCodec-Go/transport` with the same surface, plus
  `hash.DomainHasher` for the container digest. CLI parity in
  `cmd/weftcodec`.
* Conformance: `conformance/v1/transport/` — **5 valid, 17 reject**, mirrored
  byte-identically in both codec repos. Rust adapter 22/22 pass, Go adapter
  22/22 pass.
* Cross gate: `YES byte-identical transport_4096` and `transport_512`, with
  `PASS rust verifies go`, `PASS go verifies rust`, and
  `PASS single-section fetch` for both.

### Measured (`bench/receipts/transport-20261109T234518Z.json`)

One host, bench court (3 GiB / 4 CPUs / pids 1024), warm cache, one real
Qwen2.5-0.5B-Instruct tensor (`model.layers.0.mlp.down_proj.weight`, bf16
`[896, 4864]`, 8,716,288 B; corpus SHA-256
`fdf756fa…88fb7fe`). Mode-17 frame 6,019,594 B, mode-20 frame 5,846,747 B,
six small frames 834 B, container 11,902,976 B.

Header+table parse (p50): **0.541 / 0.551 / 0.681 µs** at 1 / 8 / 64 sections
(541.0 / 68.9 / 10.6 ns per entry); full metadata parse also reports
**2.134 / 7.715 / 52.327 µs**.

Single-section fetch of the 6,019,594-byte mode-17 section: **0.318 ms**
section only, **0.312 ms** for header+table+section (within noise); 6,024,138
bytes versus 11,902,976 for the whole container (**50.61 %**).

Padding overhead, 11,867,175 B of real frames: **0.064 % / 0.302 % / 4.374 %**
at 512 / 4096 / 65536. Small-frame set (834 B): **820.9 % / 3,829.0 % /
54,906.2 %** — 64 KiB alignment is expensive for small frames and is published
as such.

A 4 KiB-aligned slice at container offset 94,208 was fetched with one range
read; hashing it does not equal the section root, as specified. The enclosing
tile (index 0, 11,147 B, root `73129b63…77b422`) verified through
`decode_float_tile` after an exact range read, and the full section verified,
decoded, and matched the source (14.90 ms). The mode-20 sibling section also
verified and decoded exactly. The mode-17 and mode-20 frames share content root
`fda56ddd…94aaefb` because they encode the same plaintext; content identity is
representation-independent. Incrementing a section offset by one byte is
rejected `Malformed`, which is the alignment regression test.

### Losses

Containers never reduce bytes (4 KiB header, 56 B/section, padding up to 64 KiB
per section). The court materialises owned buffers and proves no zero-copy path.
A single-section reader cannot verify `payload_hash`; a sub-section slice
cannot be verified against the section root. The parse/fetch numbers are warm-cache,
single-host medians, not a throughput claim. No 3FS deployment, DMA, GPU, RDMA,
or fleet claim. The README 3FS row stays **ABI/conformance-tested only**.

### 3FS documentation

[`docs/THREEFS-CONTEXT-PLANE.md`](THREEFS-CONTEXT-PLANE.md) §8 records what the
container would let a USRBIO integration do (one aligned read per section,
destinations inside a registered region, batched independent sections), what
remains unproven (registration, rings, completion handling, DMA), and the
five measurements a deployed-3FS experiment would have to make.

### Gates at this change

* WeftCodec `cargo test --all-targets`: **267 passed / 0 failed** across 41
  suites (includes the new `transport` unit tests).
* WeftCodec-Go: `gofmt -l` empty, `go vet ./...` and `CGO_ENABLED=0 go build
  ./...` clean, `go test -race -count=1 ./...` all packages pass (including
  `transport` and the 22-vector `conformance` replay).
* Rust `cargo test --all-targets`: **709 passed / 0 failed** across 145 suites.
* FUSE `cargo test --features fuse --all-targets`: **752 passed / 0 failed / 2
  pre-existing ignored** across 145 suites.
* Claims **782/782**, spec drift **57/57**, conformance manifest **420 entries**
  (aggregate `6287b817…8dfbe8`), coverage OK including `transport`
  5 valid / 17 reject.


## Subphase R9 — planner v2 and incremental advisory index (2026-11-10)

Two measured defects are closed: the parent-graph planner had no write term, one
shared hop row, and no uncertainty bound (an 850 ms SLA was accepted and missed
at 886.5 ms); advisory index segments reached 3,466,466,150 B for a 74 MB data
tree. Evidence: `bench/RESULTS-planner-v2.md`,
`bench/RESULTS-index-amplification.md`, and the receipts named there.

### Planner v2 (implemented, measured)

The graph objective is now
`storage(bytes) + lambda_write*encode_ns + lambda_retention*bytes +
lambda_read*reads*chain_ns`. Coefficients come only from the measured tables:
`weftfs-write-cost/v1` for encode, `weftfs-r2/decode-calibration/v1`,
`weftfs-mode18/decode-calibration/v1`, `weftfs-s13c/chain-hop/v1`, and
`weftfs-r9/zstd-patch-hop/v1` for per-representation hops,
`weftfs-s13b/restore-overhead/v1` for the per-version restore floor, and
`weftfs-s13c/sla-margin/v1` for the margin. There is no measured retention
table, so `lambda_retention` is explicit and defaults to 0; that is stated, not
papered over.

Each graph edge is priced with the representation the parent actually uses
(raw, zstd, float-tensor, tensor-sharded, tensor-tiled, weight-delta,
sparse-delta, zstd-patch, semantic-delta, float-tiled-streams). Mode 17
(`FLOAT_TILED`), mode 20 (`ENGRAM_SHARD`), and the read-only KV/activation
strategies have no measured row and are refused with the typed
`UnmeasuredCost` class; no neighbouring row is substituted. The uncertainty
bound is the measured under-prediction per class (zstd-patch 62.5 %, weight-delta
17.8 %, sparse-delta 15.3 %, decode rows 1.0–1.05 %), summed per hop; an SLA is
accepted only if `prediction + margin + bound <= sla`.

The court (`bench/receipts/parent-graph-20261110T093000Z.json`) is the same nine
pythia-70m checkpoints and lambdas. **22/22 accepted plans held their SLA, 0
missed**; every plan class predicts within 15 % and eleven of thirteen within
5 %. The all-independent floor is −14.96 % in this receipt (−16.02 % in the
retained first run) because of cgroup page-cache state (warm 796–807 ms vs cold
878–886 ms for the same 163.9 MB read); it is published rather than tuned, and
it sits at the 15 % boundary. Every refusal is a typed `LimitExceeded` naming
the requested SLA and the best achievable bound. The SLA accept/refuse boundary
moved from 850 ms (accepted, then missed) to 950 ms (the floor's bound is
949.77 ms; the measured floor is 919.0 ms). At `λ=1` the exact oracle selects
the eight-dependency plan at 654,498,531 B predicted / 654,600,088 B realised;
the bytes-only plan is 809,012,976 B. `Solver::Auto` is now the default (exact
oracle at ≤ 12 versions, DP above).

### WFX3 incremental index segments (implemented in Rust and Go, measured)

A segment is now full or a delta against the immediately previous generation,
with element-level patches (`added ids/edges`, `removed ids/edges`). The commit
attestation grew to `weftfs.commit.v3` with `segment_kind`, `segment_base`,
`segment_depth`, and `segment_root = H("weft/index/chain/v1",
WCB({generation, chain}))`; v1/v2 commits are unchanged and never trust a chain.
The loader resolves the chain, checks every segment id and generation, then the
counts, the candidate root, the chain shape, and the chain root; any failure is
a diagnostic note and a rebuild. GC expands every retained commit's chain first
and refuses to delete anything if a chain cannot be resolved. Bounds: 32 deltas
by default (hard reader bound 256), an optional total-byte budget that skips an
advisory write (never the commit), and compaction to a full segment.

Per-commit court (1,250 commits, 1,192,538 B data): index **97,066,019 B →
3,342,744 B (29.0x)**; per-commit index bytes p50 77,892 → 406; commit wall p50
0.627 → 1.232 ms (+0.605 ms); first relation query after reopen 0.258 → 1.411 ms;
GC removes 96.6 MB / 1,245 segments → 2.85 MB / 1,141, leaving ~0.47–0.50 MB; a
snapshot at every commit keeps the chain and reclaims 0.

Trajectory re-run (50 synthetic steps, 34,894,788 B data): series index
104,637,501 B (full control) → **6,237,283 B** at the default bound and
**2,119,849 B** at 256 deltas. The historical pinned-GC peak of 3,531,842,070 B
is **113,586,041 B** now (31.1x lower); that pass reclaimed 0 B because the
chain was retained. The unpinned GC leaves ~13.2 MB in every mode. The
historical 3.47 GB cannot be reproduced in full mode on today's harness (the
checkpoint writer now group-commits, and the full-segment control peaks at
212 MB), so both the per-commit court and the current control are reported
rather than conflated. Read cost is the real loss: 9.3 ms at depth 16–31 and
60.7 ms at depth 99 for the 3,200-object index; compaction returns it to
0.24 ms. Go writes the same full segment byte-for-byte and each language loads
the other's delta chain; no Go timing is claimed.

### Residual

* Index is still one segment per commit (O(commits), not O(state)); ~406 B per
  append, ~33 KB per tensor delete.
* A forced full segment every 33 commits at the default bound.
* Snapshots retain their chain; a snapshot per step makes the tree
  un-reclaimable until those snapshots are dropped or the head is compacted.
* Chain reads cost D+1 file reads and decodes.
* Mode 17/20 and KV/activation remain unpriced; no measured retention table;
  no cold-media, fleet, or multi-writer claim.

### Gates at this change

* Rust `cargo test --all-targets`: **723 passed / 0 failed / 0 ignored**
  across 148 suites.
* FUSE `cargo test --features fuse --all-targets`: **766 passed / 0 failed /
  2 pre-existing ignored** across 148 suites.
* Go `go test -race -count=1`: `store` **ok (215.2 s)**, `cmd/weftfs` ok,
  `fusefs` ok, `index` ok, `model` ok. One full-suite run
  (`calibrate_test.go::TestCalibrationMeasurementIsDeterministicWithinTolerance`)
  saw a timing drift under whole-suite load and passed standalone and in the
  `store` rerun; it measures decode throughput and is unrelated to this change.
* Host gates: claims **791/791**, spec drift **57/57**, conformance manifest
  **420 entries** (aggregate `6287b817…8dfbe8`), coverage OK.
* `scripts/verify-all.sh`: **41/41** courts pass, 0 failed, 0 skipped;
  receipt `bench/receipts/verify-20261110T111645Z.json`.
* A stale cached Go CLI (`/work/.docker-cache/target-go/weftfs`, built before
  commit v3) made `tests/branching.rs::go_reads_and_verifies_a_rust_branch`
  fail against a freshly written v3 store; rebuilding the cached Go binary
  fixed it. The test skips when no Go binary is present, so a clean checkout
  is unaffected.

## Subphase R10 — throughput: phase profile, batched publication, pipelined restore (2026-11-10)

Full evidence and every loss: [`bench/RESULTS-throughput.md`](../bench/RESULTS-throughput.md).
Receipts: `bench/receipts/throughput-20261110T134917Z.json` (Qwen2.5-0.5B) and
`bench/receipts/throughput-20261110T145341Z.json` (Qwen2.5-3B), both from
`bench/scripts/throughput_court.sh` inside `weft/bench-court:1`
(`sha256:4c6c41fb…f66501559`) at the default hard limits (3 GiB / 4 CPU / 1024
pids / 1 GiB `/tmp`). No wire format, frame, manifest field, planner price,
candidate order or selected strategy changed; frozen fixtures are untouched.

### What the profile found

* **Family ingest, planned writer: encode dominates.** 3B `encode`
  865,052.9 ms of 920,052.4 ms wall (**89.8 %**), `blob_publish` 19,480.9 ms
  (2.0 %), `source_read` 11,038.7 ms, `index_segment` 7,184.4 ms,
  `manifest_commit` 6,854.2 ms, `digest` 1,617.7 ms; effective rate
  ≈ 13.4 MB/s. 0.5B: `encode` 434,347.1 ms of 533,551.7 ms (81.4 %),
  `blob_publish` 25,455.2 ms.
* **Checkpoint save, fast raw, 8-way: publication dominates.** 3B
  `blob_publish` 771,452.7 ms of 798,331.8 ms wall (**96.6 %**) across
  **99,103** publications, `encode` 7.5 ms. 0.5B: 137,556.7 ms of
  139,035.7 ms (98.9 %) across 19,170 publications.

The profiler is `src/phase_profile.rs`: seven wall-clock phases
(`source_read`, `digest`, `encode`, `blob_publish`, `manifest_commit`,
`index_segment`, `verify`), **off** unless a harness calls
`phase_profile::begin()` or sets `WEFT_PHASE_PROFILE=1`, thread-safe, bounded
(250,000 samples per phase), with per-tensor attribution by snapshot diff. It is
instrumented at the publication sites (`Store::store_blob`, `publish_commit`,
`index_attestation_for`, `persist_built_index`, `Store::verify`,
`prepare_candidates`, `store_tensor_artifact`, `SafetensorsFile` reads and
digests), never estimated in the harness.

### Batched durable publication (the pack plane as a batching layer)

Small blobs (≤ 64 KiB members) are sealed into content-addressed packs named by
a compare-and-swap catalog. Identity is unchanged: identical head, identical
checkpoint manifest bytes, identical manifest root for flat vs packed at ranks
0/1/2/4/8 with a fixed clock and deterministic seed
(`tests/checkpoint_packs.rs` 5/5).

Two real defects were found and fixed by those tests: (a) workers sealing
concurrently raced the local catalog compare-and-swap, which is atomic only
under the writer lock, and could silently lose members — blob workers now run in
**deferred catalog** mode and the single committer merges every worker's members
in one catalog publication, so no worker writes a control record; (b) the merge
detected a member packed by two workers and then *dropped* it instead of keeping
the existing entry.

Measured (fast raw, best of 2 for 0.5B): 0.5B sequential 108,889.2 → 29,008.0 ms
(**3.75×**), ranks-8 22,608.8 → 7,353.9 ms (**3.07×**); 3B sequential
800,031.6 → 507,126.8 ms (**1.58×**), ranks-8 412,384.2 → 145,692.7 ms
(**2.83×**). Losses: zstd-1 packs **0.67×/0.77×** and planned ranks-8 **0.85×**
(packing helps only when publications are small and numerous); 3B sequential
footprint **+12.3 %** because the sequential path republished 46,069 catalog
segments for 278 seals; the 8 MiB catalog bound left 24,870 members (1.54 GB)
flat on the 3B save (fallback, not failure); peak RSS **+31-43 %** with packs;
split-path request counters are committer-only.

### Pipelined full-checkpoint restore

`Store::restore_checkpoint_pipelined[_at]` drives the shipped ordered pipeline:
bounded tensor workers, a bounded in-flight byte budget, exact container order,
per-tensor tile threads, and the same header/order/exactness prologue as
`restore_checkpoint` (`tests/checkpoint_restore_pipeline.rs` 4/4: byte-identical
at 1/2/4/8 workers × 1/2 tile threads, single-tensor and region parity, tiled
mode-15 shards).

Measured into a SHA-256 hashing sink (decode + read + hash, no host writeback),
container SHA-256 equal to the recorded source SHA-256 in every cell:

| family | 1 worker | 2 | 4 | 8 | Amdahl cap |
|---|---:|---:|---:|---:|---:|
| 0.5B (988 MB, largest 272 MB) | 1,978.3 ms / 503.0 MB/s | **1,434.7 / 695.5** | 1,479.6 / 670.4 | 1,459.7 / 677.1 | 3.63× |
| 3B (6.17 GB, largest 622 MB) | 31,670.1 ms / 194.9 MB/s | 26,737.9 / 230.8 | 26,432.6 / 233.5 | **26,238.8 / 235.2** | 9.92× |

**The measured limit is not the largest tensor: it is one backend mutex.** 8
workers buy 1.21× on 3B and 2 workers buy 1.36× on 0.5B against a 9.92× cap,
because every blob read serializes on the store's single backend lock and each
tensor's shards decode on one thread. Tile threads are within noise for
non-tiled checkpoints. Peak RSS for the 3B restore is 1,534,292 KiB.

### Streaming encode: not implemented, with the evidence

The task's condition is met (encode dominates the planned ingest), but the
profile localises the cost inside the codec candidate encoders (mode 9 for the
large matrices: p95 3,933.1 ms, max 4,532.0 ms per tensor), not in store-side
buffering — the checkpoint writer already reads and encodes one shard at a time
and the restore already decodes tiles on a bounded pool. A store-level
streaming path cannot remove that work; a streaming mode-9/mode-15 encoder is a
WeftCodec change with its own conformance obligations. **No streaming path was
implemented rather than implement one whose only "win" would come from changing
what the planner selects.**

### Gates at this change

* New/updated tests: `tests/checkpoint_packs.rs` 5/5 (including 1/2/4/8-rank
  deferred packs with heavy dedup and real SIGABRT crash proxies at
  `pack_after_pack_object` / `pack_after_catalog_segments`),
  `tests/checkpoint_restore_pipeline.rs` 4/4, `src/phase_profile.rs` 4/4,
  `tests/packs.rs` 13/13.
* `bench/scripts/throughput_court.sh` is registered in `scripts/verify-all.sh`
  in a reduced configuration (one family, one save configuration, one rank
  count, a 40-tensor ingest prefix, two restore thread counts); the full-size
  receipts above are the standalone runs.
### Gates at this change (measured)

* Rust `cargo test --all-targets`: **736 passed / 0 failed / 0 ignored** across
  150 suites.
* FUSE `cargo test --features fuse --all-targets -- --test-threads=1`:
  **779 passed / 0 failed / 2 pre-existing ignored** across 150 suites.
* Go `go test -race -count=1 ./...` in the FUSE court: `store` ok (136.5 s),
  `cmd/weftfs` ok (45.7 s), `fusefs` ok, `index` ok, `model` ok.
* Host gates: claims **808/808**, spec drift **unchanged (no spec edit)**,
  conformance manifest/coverage unchanged (no wire change).
* `scripts/verify-all.sh`: **42/42** courts pass, 0 failed, 0 skipped; receipt
  `bench/receipts/verify-20261110T181259Z.json`. The R10 court is one of them
  (`throughput`, reduced configuration). One earlier aggregate run
  (`verify-20261110T160409Z.json`, 40/42) is **retained**: it exposed that the
  new court was invoked on the host instead of in the bench court (no `/work`),
  and a `rust_fuse` failure that did not reproduce standalone (779 passed) or in
  the passing reruns. A second run (`verify-20261110T170837Z.json`, 41/42) is
  retained because it found a `local` declaration outside a function in the new
  court script.

## Subphase R11 — training goodput and time-to-safe-resume (2026-11-10)

Full evidence and every loss:
[`bench/RESULTS-training-goodput.md`](../bench/RESULTS-training-goodput.md).
Receipt `bench/receipts/training-goodput-20261110T200930Z.json` (raw log
beside it), measured at WeftFS `a40c17d` in `weft/python-court:1`
(`sha256:aec5deb0…76152`), Linux `7.3.3-1-cachyos`, 3 GiB / 4 CPU / 1024 pids /
1 GiB `/tmp`, `cap-drop=ALL`, **torch 2.15.0+cpu, no GPU**. No wire, frame,
manifest, planner or store code changed.

### The loop, not a fixture

`python/courts/training_goodput.py` trains a real three-block transformer stack
(414,432 parameters; explicit causal multi-head attention, GELU MLP,
LayerNorm, token+position embeddings) with AdamW on a deterministic seeded
next-token dataset: 256 steps, 5 repetitions, checkpoint every 64 steps (4
checkpoints), batch 8 × sequence 48 × vocabulary 384. Fixed: seed, data,
batch order, no dropout, one BLAS thread,
`torch.use_deterministic_algorithms(True)`. Checkpoints are asynchronous in
all configurations (clone + submit, encode on a worker, hard
`staging_bytes=64 MiB`); the training thread never runs the encoder. All 15
runs produced the same loss-bit trajectory and the same final state hash.

### A / B / C on identical steps and data

A = `torch.save` file, B = local PyTorch DCP directory, C = DCP through
`weftcodec.dcp` / `save_checkpoint_async`. Medians of 5 runs; step p50/p95
pooled over 1,245 clean steps per configuration.

| metric | A | B | C (WeftFS) |
|---|---:|---:|---:|
| loop wall (ms) | **2,667.0** | 2,747.9 | 3,190.7 |
| training steps/s (step sum) | 96.22 | 93.75 | **94.17** |
| end-to-end steps/s (wall) | **95.99** | 93.16 | 80.23 |
| step p50 / p95 (ms) | 10.12 / 10.63 | 10.07 / 10.85 | **10.02 / 10.58** |
| save pause p50 / max (ms) | 0.68 / 0.86 | 0.74 / 0.96 | 3.41 / 3.70 |
| save completion p50 (ms) | **3.86** | 20.11 | 444.13 |
| final checkpoint completion (ms) | **3.61** | 18.35 | 459.02 |
| step the save lands at | 1 | 1 | 29 |
| physical bytes/checkpoint | 5,025,099 | 5,277,059 | **4,924,427** |
| peak RSS (KiB) | **370,156** | 371,152 | 379,272 |
| resume load p50 (ms) | **7.1** | 21.9 | 231.4 |

The training steps are not slower (p50 10.02 vs 10.12/10.07 ms; p95 10.58 vs
10.63/10.85). Training rates are **96.22 / 93.75 / 94.17 steps/s** in A/B/C.
The wall deficit is the final in-flight checkpoint having no later step to
overlap (472.5 ms post-loop tail vs 6.5/17.7); the WeftFS step sum is 2.2 %
above A and 0.4 % below B. WeftFS writes 2.00 % fewer physical bytes per
checkpoint than `torch.save` and 6.68 % fewer than DCP.

### Why a WeftFS checkpoint is 444 ms

Adapter spans: 380.2 ms `checkpoint_write_ms` (memfd handoff + service round
trip + manifest verification), 284.5 ms bare service round trip (Rust candidate
encode + group commit), ~64 ms Python staging/service start/metadata commit.
A control in the receipt (`store_growth_probe`): a checkpoint in a **fresh
store** is 210.5 ms (`checkpoint_write_ms` 186.8); the second and later
checkpoints in the same store are 383.8/433.3/455.8 ms for the same 4.99 MB
container. Per-checkpoint deltas are constant (manifest 4.59 MB, objects
0.16 MB, commit 0.12 MB, index 62 KB), so the growth is store-state bookkeeping,
not more data; this court measures it and does not localise it further.

### Exactness and resumption

Every checkpoint of every normal run was restored by a **fresh process**:
**60/60** exact tensor/optimizer/step comparisons, **60/60** next-step loss
bits, **45/45** post-step states (the last checkpoint of each run has no next
step); cross-configuration loss trajectory and final state are identical.
Staging bound is fail-closed: 1,024 B is refused with `StagingLimitExceeded`
naming the tensor; the 64 MiB production bound accepts with a 147,456 B peak.

SIGKILL court (WeftFS only, 3 repetitions per scenario; TTSR = kill → first
completed optimizer step, including store discovery):

| scenario | TTSR p50 / p95 (ms) | steps lost | exposed |
|---|---:|---:|---|
| kill after the 128 checkpoint was acked | 1,385.1 / 1,386.0 | 12 | 128 |
| kill immediately after submit | 1,381.5 / 1,386.5 | 64 | 64 only (3/3); 128 absent |
| kill 225 ms into the save | 1,366.5 / 1,371.6 | 64 | 64 only (3/3); 128 absent |
| kill 405 ms into the save | 1,360.5 / 1,376.5 | 0 | 128 complete (3/3) |
| kill 518 ms into the save | 1,379.4 / 1,398.6 | 0 | 128 complete (3/3) |

All **15/15** kill runs resumed from a complete checkpoint (old-or-new, never
partial); `checkpoint verify` and `verify --full` were clean, and every resumed
state and next-step loss was exact. Of the ~1.36-1.39 s TTSR, store discovery is
27-62 ms and the checkpoint load 217-231 ms; `import weftcodec.dcp` (including
torch 2.15) alone is 889 ms in a fresh process. A long-lived worker that keeps
torch imported is not implemented and is not claimed.

### Losses

* Loop wall **1.20×** `torch.save`, **1.16×** local DCP (mostly the final
  checkpoint tail; step p50 is not slower).
* Pause 3.41 ms vs 0.68/0.74 ms (4.6-4.9×), almost all of it the
  `save_checkpoint_async` submit call; the clone is 0.59 ms.
* Completion 444.1 ms: **22.1×** local DCP, **115×** the `torch.save` worker.
* Later checkpoints in the same store cost 384-456 ms vs ~210 ms for the first
  (store-state bookkeeping).
* Resume load 231.4 ms, **10.6×** local DCP.
* Worst single step 68.1 ms vs 15.2/15.5 ms (host scheduling/background encode
  contention; the pause max is 3.70 ms).
* Any kill loses the steps after the last acked checkpoint (12 or 64 here).
* Peak RSS +2.5 % vs `torch.save`, +2.2 % vs DCP.

### Gates at this change

* Python court: **364 passed / 70 skipped / 0 failed** at `a40c17d`
  (`PYTHONPATH=/work/python python3 -m pytest -q python/tests`), including the
  new `python/tests/test_goodput_harness.py` 4/4 (exact capture/install,
  deterministic data, loss bits, fail-closed staging bound). The court image
  was rebuilt with numpy (`sha256:aec5deb0…76152`) so the documented tensor-view
  test runs; numpy is present equally in all three configurations.
* Three pre-existing drifts were found by this gate and fixed minimally: the
  Orbax collection guard raised when the parent package was absent,
  `test_service` asserted protocol 1 while the shipped protocol is 2, and the
  Python reader's unimplemented-mode table omitted the new modes 19
  (`KV_CACHE_PROV`) and 20 (`ENGRAM_SHARD`), so their vectors failed instead of
  skipping.
* Host gates: claims **820/820**, spec drift **57/57**, conformance manifest and
  coverage **420 entries**.
* Rust `cargo test --all-targets` was **not run**: no Rust library code changed
  (the only non-Python changes are docs, the Dockerfile and the court's
  environment-forwarding list).
* `scripts/verify-all.sh` was not rerun; this court is standalone and is not
  registered there.

### Non-claims

Single CPU host, no GPU or multi-rank/distributed claim; synthetic model/data;
SIGKILL process faults, not physical power loss; single-writer MW1; no cloud
backend; no production queueing/skip policy; TTSR includes fresh-process
startup, with the store share reported separately. The court is standalone and
is not part of `verify-all`.

## Subphase R12 — 7B scale, fleet cold-start, attested loads (2026-11-10)

Three measured questions and one new signed record. Every number came from a
court run inside `scripts/docker-court.sh`; the only limit that differs from the
default is the fleet court's 8 GiB (recorded there, not hidden).

### 1. 7B — real Qwen2.5-7B-Instruct, and the limit it hits

Corpus: four published shards merged into one 15,231,272,088 B container
(`5527ae977f6b2a2fc0add1d76834f1300a607f79aa4bdce1457bd738523c1ef1`, 339 BF16
tensors, largest 1,089,994,752 B), fetched through the LFS-verifying fetch
script and merged by a per-tensor-verified merge tool.

* The **per-tensor planner ingest is OOM-killed** in the default 3 GiB bench
  court: exit 137 on `model.embed_tokens.weight`, cgroup peak 3,221,225,472 B (the
  limit), store left empty. The limit was not raised.
* The **bounded/streaming checkpoint writer fits**, 8-way, fast raw:
  **flat 1,406,533.6 ms / 10.83 MB/s / 173,032 KiB RSS / +0.11 % bytes**;
  **packed 1,088,654.1 ms / 13.99 MB/s / 287,420 KiB RSS / +51.16 % bytes**
  (911 packs, 236,237 members, **148,694 catalog-segment publishes**). Both
  `verify --full` clean (2,713 objects, 2,713 manifests, 233,539 chunks).
* **Full restore** from the packed store is exact at 1/4/8 workers:
  **337.7 / 367.5 / 367.8 MB/s**, i.e. **1.089x**, against an Amdahl cap of
  13.97x set by the 1.015 GiB embedding. Peak RSS 2.2-2.4 GiB.
* 70B is stated only as a **linear extrapolation** (flat ≈ 3.6 h, packed ≈ 2.8 h,
  restore ≈ 6.4 min at 8 workers); the ingest path cannot be extrapolated
  because it already fails at 7B.

### 2. Fleet cold-start — 100 replicas, one shared store handle

The real SmolLM-135M-Instruct checkpoint (272 BF16 tensors, 269 MB) in a real
MinIO instance over loopback; one shared read-only server with 100 FUSE views;
100 replica processes each reading the whole model.

| | single-flight + cache | control |
|---|---:|---:|
| backend bytes | **28,248,268,423** | 442,325,447,419 |
| backend requests | **61,908** | 112,368 |
| single-flight waiters | 37,327 (87 %) | — |
| cache hits / resident | 13,133 / 53.2 MB | — |
| time-to-first p50 / p95 | **118.3 / 141.9 ms** | 134.7 / 205.5 ms |
| wall until all 100 ready | **51,827.3 ms** | 672,765.2 ms |
| cgroup peak / pids peak | 1.64 GiB / 315 | 1.83 GiB / 322 |

100/100 replicas in both, 272 tensors and 269,030,016 B each, all 100 byte
digests identical, **0 OOM kills and 0 pids refusals**. The honest limits:
startup is serialised in one process, one backend mutex is both the reason the
control is only ~13x slower (not 100x) and the reason it cannot scale further,
and the decode counter is instrumented only on the gated read path, so the
control has **no** decode-call number (none is invented).

### 3. Attested loads — a signed, independently verifiable record

New `weft.load-attestation.v1` (`docs/LOAD-ATTESTATION.md`): canonical WCB with
checkpoint/manifest roots, per-tensor logical roots, the **exact loaded
ranges** with domain-separated digests, the shard **groups** whose roots were
verified while consuming them, the verifier identity, a timestamp, and an
Ed25519 signature over the record without its signature key.

* Full load of 272 tensors: 2,133.2 ms (126.1 MB/s), 2,176 groups, receipt
  456,430 B. Verification in a second process: **2,241.0 ms, 8,734.4 µs/MiB
  (8.73 ms/MiB), 1.05x the load**, 272 ranges replayed.
* A 20-tensor load reports the other **252 as `unverified` with a reason**; a
  50-row region load says `full_root_verified: false` and lists 271 unverified.
* Tampering: a changed range digest fails the **signature**; a corrupted store
  blob fails the **replay**; the court's flipped-byte record is rejected as
  non-canonical. `tests/attested_load.rs` 9/9.
* The signature format is shared: `WeftFS-Go/cmd/attest-verify` verified the
  Rust-produced record (`signed=true`, 272 loaded, 2,176 groups) and rejected
  the tampered one. It does **not** replay bytes (`byte_replay=not_performed`).

### Losses (all three increments)

Per-tensor ingest fails at 7B; packed sequential footprint +51.2 %; restore
parallelism 1.089x of a 13.97x cap; every write/restore cgroup peak is pinned at
the 3 GiB limit by page cache (process RSS is reported separately); fleet
startup serialisation, one backend mutex, 128 MiB cache cannot hold the model,
tail latency, and no decode count for the control; attestation costs a second
full pass and ~1.7 KB of receipt per tensor, a lazy record cannot prove unread
bytes, the signature proves origin not truth, regions must be row-aligned, and
Go verification is signature-only. Single host, warm caches, no power-loss,
media, multi-host, fleet, cloud or serving claim.

### Evidence and commits

Receipts `bench/receipts/scale-7b-20261110T213731Z.json`,
`bench/receipts/fleet-coldstart-20261110T225018Z.json`,
`bench/receipts/attested-loads-20261110T232937Z.json` (+ `.log` beside each).
`bench/receipts/scale-7b-20261110T204331Z.json` is retained: its `verify_packed`
and restores failed because the court re-opened the packed store without
`--pack` (a court bug, corrected in the run above).

### Gates at this change

* Rust `cargo test --all-targets` at the final revision: **153 suites, 747
  passed / 0 failed / 0 ignored**.
* FUSE `cargo test --features fuse --all-targets` at the final revision: **154
  suites, 790 passed / 0 failed / 2 ignored** (the two pre-existing ignores).
* Go `go test -race -count=1 ./...` in the **fuse** court: all packages pass
  (cmd 46.2 s, fusefs 12.7 s, index, model, store 212.1 s). The same command in
  the *go* court fails only because that image has no `/bin/fusermount`
  (`mount: exec: "/bin/fusermount": stat … no such file or directory`); that is
  an environment gap, not a test result, and is recorded as such.
* Host gates: claims **836/836**, spec drift **57/57**, conformance manifest
  **420 entries** (aggregate `6287b817…dfbe8`).
* The three R12 courts are **standalone and not registered in
  `verify-all.sh`**; `verify-all` still reports the 42 courts it already had.
  Their receipts are the evidence above.
* Full aggregate: `bench/receipts/verify-20261110T234609Z.json` — **42/42 courts
  pass, 0 failed, 0 skipped**, at the revision that carries this section.

---

## Subphase R13 — formal artifacts for an outside evaluator (2026-11-11)

**Purpose.** The engineering evidence already existed; the formal artifacts that
let an outside storage engineer evaluate it without running our courts did not.
R13 adds them and makes every headline number traceable to a committed receipt.
No wire format, on-disk format, planner coefficient, or court measurement was
changed. There is no new performance claim.

### Deliverables

* [`docs/PAPER.md`](PAPER.md) — a three-page falsifiable note with seven numbered
  claims (C1–C7), each with its experiment, receipt, measured numbers, failure
  condition, and a "what would falsify this" table. The abstract leads with the
  loss: **73.9× slower to write for 33.1 % fewer bytes**, and the 28 % loss to a
  whole-file `zstd --patch-from` on a 19-byte checkpoint change.
* [`docs/RFC.md`](RFC.md) — stable `WEFT-001` … `WEFT-018` identifiers, title,
  status (frozen / experimental / policy / non-wire), version, governed modes,
  and vector directories. It states what "frozen" means, how an additive mode is
  added, how a reserved value moves, what a compliant implementation must
  reject, and a third-party checklist extended to modes 17–20
  (`RFC.md` §4.2).
* [`spec/README.md`](../spec/README.md) — the spec-directory navigation index,
  exported byte-for-byte to all three codec/filesystem mirrors.
* `scripts/package-conformance.sh` and `scripts/verify-conformance-python.sh` —
  the third-party distribution and its second-opinion phase.
* [`docs/CLAIM-AUDIT.md`](CLAIM-AUDIT.md) — an adversarial audit of every
  headline claim in the README and the v1.2.0 release note.

### Conformance distribution

`bench/bundles/conformance-1.2.0.tar.zst`:

* **538 files**, **1,328,679 bytes**, SHA-256
  `5ece88b51ff05cf18979cc90598cdeeb2c928c48e1a3563d3954057511c10b43`.
* Contents: `conformance/manifest.json` (420 entries, aggregate
  `6287b817…`), all vectors, the checkpoint/context/overlay/store/engram/fuzz
  fixtures, `RUN.md`, the runner, the adapter contract, the Rust/Python
  adapters, `docs/CONFORMANCE.md`, `docs/WRITE-YOUR-OWN-DECODER.md`,
  `docs/RFC.md`, `docs/PAPER.md`, and the independent Python reader.
* The bundle is shaped as `weftfs.receipt-bundle/1`, so
  `scripts/verify-receipts.sh` verifies its checksums and revisions. Result:
  **pass**.
* Extracted-copy run only: Rust **288 pass / 0 fail / 132 skip**; Python
  **229 / 0 / 191**. Python skips per mode: **18 → 17, 19 → 22, 20 → 21**;
  every other skip is a non-codec surface or a superseded historical reject.
  The receipt's `documented_skips` check is true.
* Receipt: `bench/receipts/conformance-bundle-20261111T012513Z.json`.
  The earlier `…011629Z` receipt is retained because the verification caught a
  real packaging defect (the checksum list included its own temporary file).

### Claims corrected by the audit

Ten current-state README claims and two document claims did not match their
evidence and were corrected in place:

1. blanket "byte-identical Rust and Go" → byte identity where both implement
   the surface, with the Go gaps named;
2. "linearizable head publication" → one authoritative head sequence,
   model-checked and finite-history tested, not a machine-checked
   linearizability proof;
3. 41/41 verification courts → **42/42**; 355 vectors → **420**;
4. 60 codec vectors → **91**;
5. 39 store tests → **746** in the 42/42 log (747 at the final R12 commit);
6. 232 FUSE tests → **789** in the 42/42 log (790 at the final R12 commit);
7. 31 Go + 13 GC → **125 top-level Go tests, 7 skipped under `-race`**;
8. "latest verification 37/37" → latest **42/42**; the release tree's 37/37 is
   stated separately;
9. "parallel reconstruction is not implemented" → bounded parallel restore
   (R2) and pipelined restore (R10) named; materialize remains writeback-bound;
10. "no headline performance claim" → "no performance-superiority claim".
11. `docs/CONFORMANCE.md` claimed the Python reader executed every mode 0–18
    vector and reported 229/0/109. It now reports **229/0/191** and states the
    mode 18/19/20/transport skips.
12. `docs/WRITE-YOUR-OWN-DECODER.md` said modes 17–127 were reserved and
    stopped at mode 16. It now covers modes 17–20, names mode 21 as the
    reserved reject, and points to the extended checklist.

The release note is a tagged historical document. Its 355-entry claim is true
at `122d86e` and is marked historical, not rewritten; current documents carry
the 420-entry manifest.

### Gates

* `scripts/check-claims.sh` — **855/855** entries.
* `scripts/check-spec-drift.sh` — **60/60** file comparisons across the three
  mirrors.
* `scripts/gen-conformance-manifest.sh --check` — **420 entries**, aggregate
  `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`.
* `scripts/check-conformance-coverage.sh` — **OK**; modes 0–20 all have valid
  and reject vectors.
* `scripts/package-conformance.sh` in the fuse court — bundle built, Rust
  extracted-copy run clean, `verify-receipts.sh` **pass**. Registered as the
  `conformance_bundle` court (below).
* `scripts/verify-conformance-python.sh` in the python court — **229/0/191**,
  documented skips true, receipt merged. Registered as the
  `conformance_python` court (below).
* The 42/42 aggregate (`verify-20261110T234609Z.json`) is **not** re-run for
  R13: no wire format, planner coefficient, or court measurement changed. R13
  later registered its two packaging courts in `verify-all.sh`, so the next
  aggregate runs **44** courts; the 42/42 receipt stays the cited aggregate
  until such a run exists.

### Court registration (R13 completion)

Registration adds no measurement and re-runs no package:

* `conformance_bundle` (`scripts/conformance_bundle_court.sh`) runs
  `scripts/package-conformance.sh` in the fuse court after building
  `weftcodec` from the mounted `WeftCodec` tree, then verifies the bundle with
  `scripts/verify-receipts.sh --repo-root .` **again from the host**, so the
  receipt's revisions must resolve to real commits in the mounted
  repositories. The packaging script has no cost knob, so this is its full
  default configuration (all 420 vectors, extracted-copy Rust run, checksums,
  unlisted-file rejection, revision checks); the only deferred phase is the
  Python second opinion, and the receipt's `configuration` block names that
  split. `configuration.reduced` is therefore `false` — the aggregate does not
  run a cheaper packaging run.
* `conformance_python` (`scripts/conformance_python_court.sh`) runs
  `scripts/verify-conformance-python.sh` in the python court against the
  bundle and receipt of the same stamp, re-checks the bundle SHA-256 before
  anything runs, and fails unless the merged receipt reports zero Python
  failures and only documented skips.
* Both print exactly one `conformance_bundle: skip <reason>` /
  `conformance_python: skip <reason>` line and exit 78 when the environment
  cannot run them (no Docker, no court image, or no bundle/receipt for this
  stamp). `verify-all.sh` records that as `skip`, never as a pass.
* Both ran once at this tree, stamped `20261111T015329Z`:
  **`conformance_bundle` pass** — `bench/bundles/conformance-1.2.0.tar.zst`,
  **1,328,473 bytes**, **538 files**, SHA-256
  `e12853f46f3588c9832d268d04842bc0795e53241ec00fe6eda58b4bd9296207`; Rust
  **288 pass / 0 fail / 132 skip** from the extracted copy only;
  `verify-receipts.sh` pass in the court and on the host.
  **`conformance_python` pass** — **229 pass / 0 fail / 191 skip**, per-mode
  skips 18 → 17, 19 → 22, 20 → 21, no undocumented or non-codec skip.
  Receipt: `bench/receipts/conformance-bundle-20261111T015329Z.json` (both
  courts' blocks), host verification log
  `bench/receipts/conformance-bundle-20261111T015329Z.verify.log`.
* The run happened at parent revision `ff89472` **plus the uncommitted court
  and packaging-script changes that are this section's payload**. None of
  those files is part of the bundle payload (the bundle carries the corpus,
  `docs/CONFORMANCE.md`, `docs/WRITE-YOUR-OWN-DECODER.md`, `docs/RFC.md`,
  `docs/PAPER.md`, the runner, the adapters and the Python package), so the
  bundle content corresponds to `ff89472`; the receipt records that revision
  and the exact commands in its `courts` blocks.

### Packaging-court defects found while finishing R13

* The first registration attempt failed: the host-side verification could not
  update a receipt written by a root court. The producer now makes its receipt
  world-writable (`chmod 0666`), exactly like the receipts/bundles directories
  the court already widens, and `verify-conformance-python.sh` does the same
  after it merges. No court result changed; the failing attempt produced no
  retained receipt.
* A rebuilt bundle does **not** reproduce the historical SHA-256
  (`5ece88b5…`): the bundle's own `manifest.json` embeds `generated_at`, and
  `docs/CONFORMANCE.md`, `docs/RFC.md` and `docs/PAPER.md` changed after
  `3b475f7`. The receipt's `bundle_sha256` names the artifact that run
  verified; it is not a bit-reproducibility claim.

### Working-tree state

* Three root-owned `bench/receipts/.r13-conformance-*-probe.json` files were
  deleted. No committed receipt, script, or document referenced them, and
  `git grep`/`git log -S` find no reference in history; they were unreferenced
  byproducts of the R13 runs.
* The PyPI build byproducts `python/build/` and `python/weftcodec.egg-info/`
  were deleted. Both are gitignored and untracked; nothing in the tree
  references them.
* `WeftFS-Go/store/azureclient.go` was, at R13, an unfinished and untracked Go
  Azure backend attempt. The R13 disk-full event truncated it to 0 bytes; R14
  replaced it with a real implementation (`store/azureclient.go`,
  `azureblob.go`, `azurecommit.go`) that is tracked, tested, and courted. It is
  not a recovery of the truncated bytes.

### What R13 does not establish

* No new measurement. Every number in the paper and audit is from a pre-existing
  receipt; a number without a receipt was deleted.
* The bundle proves corpus/runner self-sufficiency, not implementation quality.
  The Rust reference run supplies its own binary; the bundle supplies the
  corpus, contract, and adapters.
* The Python reader still omits modes 19 and 20 and `weft.transport.v1`; those
  are explicit skips, not passes. It decodes mode 18, but the reference adapter
  does not dispatch the `float-tiled-streams` directory, so those vectors are
  skipped as well.
* Live PyPI/crates.io/GHCR state was not re-queried; the artifact receipt is the
  record at the tag.
* No physical power loss, host-cache loss, GPU, fleet, or live-cloud claim.
* `Go` still lacks the parent-graph planner, the KV prefix tier, Ed25519
  envelope verification, and MW2 on `azure`; MW3 was not implemented at that
  release (an opt-in Rust prototype landed 2026-11-14).

## Subphase R12b — checkpoint completion latency (2026-11-11)

Numbering: the earlier **R12** (7B scale, fleet cold-start, attested loads)
keeps its name. This subphase is the follow-up to R11's 444.1 ms checkpoint:
the same fixture, decomposed, then driven down.

Full evidence and every loss:
[`bench/RESULTS-checkpoint-latency.md`](../bench/RESULTS-checkpoint-latency.md).
Receipts `bench/receipts/checkpoint-latency-20261111T024824Z.json` (current),
`bench/receipts/checkpoint-latency-20261111T022638Z.json` (before the atomic
publication) and `bench/receipts/checkpoint-latency-20261111T021640Z.json`
(the profiler baseline), measured in
`weft/python-court:1` (`sha256:aec5deb0…76152`), Linux `7.3.3-1-cachyos`,
3 GiB / 4 CPUs / 1024 pids / 1 GiB `/tmp`, `cap-drop=ALL`, torch 2.15.0+cpu.
No wire, frame, manifest or planner-identity change.

### Increment 1 — the completion profiler

The fixture is the R11 fixture by construction: the court imports
`build_model`/`make_optimizer`/`capture_state` from
`python/courts/training_goodput.py`, runs one real optimizer step, and saves
the same **169 entries / 4,973,352 bytes** with a fresh writer per save.

The shipped profiler (`src/phase_profile.rs`) gained eleven phases (`store_open`,
`state_replay`, `plane_probe`, `index_build`, `index_load`, `index_attest`,
`metadata_commit`, `encode_zstd`, `encode_zstd19`, `encode_float_tensor`,
`encode_semantic`), each nested phase carrying `nested_in` so it is never added
to its parent; the service exposes them over a new protocol-3 `PROFILE` op
(modes 0/1/2), and `weftcodec.service.set_probe_sink` reports twelve client
sections. Instrumentation is off unless a court asks for it.

Measured (median of 3 fresh stores, 1 first save, 5 later saves):

| | fresh store | first in a store | later in a store |
|---|---:|---:|---:|
| completion | 211.1 ms | 211.1 ms | 472.6 ms |
| `encode_semantic` (mode-14 search) | **130.4** | **131.8** | **344.5** |
| `encode_float_tensor` (mode 9) | 22.8 | 23.2 | 23.0 |
| `encode_zstd` (level 3) | 5.9 | 5.9 | 6.8 |
| `store_open` (4 opens) | 12.4 | 12.8 | 44.9 |
| `state_replay` (nested) | 9.8 | 10.2 | 42.2 |
| client `write_request` | 185.7 | 186.8 | 422.2 |
| client `metadata_commit` (`WRITE_FILE`) | 9.3 | 9.7 | 28.0 |
| accounted share of wall | 96.8 % | 97.1 % | 98.7 % |

**The 444 ms was the planner, not the store.** `tensor_semantic_candidates`
scans every file object in the store state and decodes every same-dtype,
same-shape tensor to price a mode-14 semantic delta. With no explicit candidate
list that is `O(objects)` per tensor, so each checkpoint's 169 new objects make
every later checkpoint's planner slower — the "store-state bookkeeping" that
R11 measured and did not localise. Reopening the store four times per save
(12.4 → 44.9 ms) and opening it again for the 30 KB DCP metadata blob
(9.3 → 28.0 ms) are the second and third causes. Python staging, hashing and
the memfd handoff together are under 5 ms.

No optimisation was made before this measurement.

### Increment 2 — session, and two planner changes the profile justified

**2a. The implicit mode-14 search is now opt-in.** New
`TensorPlanOptions::semantic_scan` (default **false**) gates only the implicit
candidate source: with no explicit `candidates` list, mode 14 is reported
`skipped` instead of walking the whole store. Explicit candidate lists are
unchanged, `semantic_scan = true` restores the search (bounded by
`semantic_scan_max_objects`, default 256, ascending object id),
`weftfs checkpoint write --semantic-scan` opts in, and `--no-semantic` still
disables the candidate. This is a planner candidate-set change for callers
that never set the option, disclosed here: the old default was `O(objects)`
per tensor with no index behind it. The fixture's chosen representation is
unchanged (every shard is one ZSTD frame).
**211.1 / 472.6 → 67.9 / 81.5 ms.**

**2b. A caller-pinned strategy no longer builds candidates it cannot choose.**
`checkpoint_shard_spec` pins `TensorStrategy::Zstd` or `Raw` for the `zstd` /
`raw` shard strategies and `choose_candidate` returns only that strategy, yet
the planner still encoded mode 9, mode 10, the deltas and the patch. The
planner now returns after building the pinned `Raw`/`Zstd` candidate for a
non-q8_0 tensor; every other hint keeps the full set and an inapplicable hint
still fails with the same typed error. Rust `encode` fell 29.7 → 5.8 ms per
save on the profiled baseline, so a save builds one ZSTD frame per shard and
nothing else. **67.9 / 81.5 → 42.6 / 60.6 ms.**

**2c. `CheckpointSession` (reusable service + one store handle).**
`weftcodec.dcp.CheckpointSession` owns one service across saves and
`WeftStorageWriter(session=...)` binds to it; `save_checkpoint_async` accepts
the same. The service keeps **one** handle: read-only until the first
publication, then writable, with reads going through it, so the four opens per
save become one (`store_open` 12.4-44.9 ms → **0**). Ownership: the writer lock
is released after `--session-idle-secs` without a publication (default 30 s;
`0` = release after every publish), decided on a client request, never a timer
timer thread; a writable open re-proves both plane capabilities; a stale fence
is returned as a typed `Busy`/`Unauthorized` and never replaced. The session
refuses `service()` across `fork()`, and `CheckpointService` refuses
`close()`/`_call()` in a child so a child cannot `SHUTDOWN` the parent's
helper. `session.save` retries a transport failure at most once and only when
the store proves nothing was published under the allocated name; a lost
response that did land raises an explicit "published but not finished" error
rather than publishing a second generation.

Measured, 8 saves in one store (seeded fixture, `flat_sha256` recorded):

| | first | median | last |
|---|---:|---:|---:|
| session | 38.5 ms | **42.2 ms** | 46.1 ms |
| writer per save | 40.0 ms | 60.2 ms | 85.7 ms |

Every later save writes the same 277,402 store bytes and the commit is
0.43-0.45 ms at every index, so commit cost no longer grows with store
history; only the index chain load (2.6 ms) and the WFX3 segment size do. One
93.0 ms session save is recorded as a host-scheduling outlier and is not
smoothed away. **Milestones reached: 100 ms and 50 ms. 20 ms is not yet
reached** — the remaining measured shares are `index_segment` 5.2 ms, the
second commit for DCP metadata 3.8 ms, `encode_zstd` 5.8 ms, `digest` 3.0 ms,
Python metadata serialisation 4.2 ms and ~6 ms of DCP/writer overhead.

### Increment 3 — prepare/finish, one atomic publication

`StorageWriter` already had the seam; the adapter committed tensors in
`write_data` and the DCP metadata in `finish`, so a failure between them left a
checkpoint directory without metadata. Now:

* `CheckpointWriteOptions::defer_commit` runs the checkpoint body inside an
  explicitly opened group and returns without publishing;
  `Store::commit_deferred_checkpoint_metadata(path, bytes)` adds the metadata
  file and publishes **one** commit; `abort_deferred_checkpoint()` discards it.
* Protocol 3 adds `PREPARE_CHECKPOINT` (0x0c), `FINISH_CHECKPOINT` (0x0d) and
  `ABORT_CHECKPOINT` (0x0e). One deferred checkpoint per connection, a second
  prepare is `InvalidArgument`, the metadata path must be under
  `/checkpoints/<name>/`, `SHUTDOWN` aborts an open group, and the idle release
  never drops a handle with an open group.
* `WeftStorageWriter(atomic_publish=True)` is the default;
  `atomic_publish=False` keeps the historical two-commit shape for comparison.

`tests/checkpoint_atomic.rs` **6/6** with the process-fault proxy: a prepare
that is never finished publishes **nothing** (the old window no longer
exists); prepare+finish publishes exactly **+1** generation containing both the
checkpoint and `dcp.metadata`; crashes at `after_objects` and
`after_commit_file` are old-or-new whole state with only an unpublished-tail
note from `verify`; the historical shape costs **+2** generations for the same
bytes and the metadata blob is identical between the two.

Measured on the same session and fixture: **40.7 → 37.7 ms p50** (first 37.2,
last 39.6). The second commit and its index work disappear (`metadata_commit`
4.3 ms → 0 folded in; `index_segment` 5.6 → 3.7 ms). Honest cost: `finish()` is
now the durability point, so a caller that never calls it publishes nothing —
which is the repair, and a behaviour change for a caller that used
`write_data` alone.

### Increment 4 — the index chain is kept, not re-read (partial)

`index_attestation_for` re-resolved up to 32 segments on every commit and
always encoded a full-state segment even when the delta won. The writer now
keeps the chain it published (`AttestedCache`), validates it against the
previous **commit record** (counts, `candidates_root`, kind, base, depth, chain
root) before every use, and falls back to the existing disk path on a miss or
mismatch; the full-state encode is skipped when the delta is ≤ 64 KiB.

R9's 1,250-commit court (`index-chain-20261111T031356Z` vs the pre-change
`…T025745Z`): delta commit **p50 1.178 → 0.695 ms**, p95 2.151 → 1.196, first
quartile 0.537 → 0.271, last quartile 1.812 → 1.106, index bytes unchanged at
3,342,744, against the same run's full-segment path at 0.650 ms. **Attestation is
unchanged** (the same checks run on both paths; `a_forged_chain_root_is_
rejected` and `a_partial_chain_is_rejected_and_rebuilt` still pass) and the new
`the_writer_chain_cache_answers_like_a_cold_oracle` asserts the hot answers
equal a full-segment store and a cold chain load.

**Not done:** the delta is still `O(state)` (`StateIndex::build` + whole-index
diff), so the last quartile remains ~4x the first. A row-level incremental
index that consumes committed ops is not implemented; `StateIndex::build`
stays the oracle. On the 169-object checkpoint fixture the change is inside the
run-to-run noise (chain depth 1-2).

### Increment 5 — physical aggregation for small frames: measured, and not adopted

Receipt `bench/receipts/checkpoint-latency-20261111T034200Z.json`. The court now
reports the frame-size histogram of the files a save creates, per-save backend
writes/bytes/fsync counts (new `BackendStats::fsyncs`, counting `sync_all` and
directory `fsync`), and a restore pass over every tensor.

A first save writes **265 files / 4,890,427 B / 528 fsyncs** (147 files ≤512 B,
67 ≤4 KiB, 16 in 16-64 KiB, 34 in 64-256 KiB); a later save of the same state
writes **7 files / 276,794 B / 14 fsyncs** because the frames are
content-addressed and already present. Deferred packing
(`--pack` at 1 MiB/64 KiB, 4 MiB/256 KiB, 16 MiB/1 MiB) cuts the first save to
**189 / 184 / 165 files** and **380 / 372 / 332 fsyncs** with slightly fewer
bytes, but completion is unchanged within noise (47.0 → 40.0 / 42.3 / 43.5 ms,
one sample each) and the steady state **loses** 2-6 fsyncs and 0.5-1.6 KB per
save for the catalog. Restore is exact and unchanged (same digest, 147.3 ms
unpacked vs 147.0-154.2 ms packed).

**Not adopted for this workload:** `CheckpointSession` does not enable packing,
and the R11 packing result stands on its own (large single-ingest) evidence. A
packed store is only readable through the packed plane, so the CLI `checkpoint
verify` cannot see one; exactness here was proven by restoring all 169 tensors
through the session. Peak RSS 304 MB is the Python process, not the service.

### Increment 6 — write-policy comparison

Receipt `bench/receipts/checkpoint-latency-20261111T035900Z.json`; the policy is
now plumbed through `WeftStorageWriter(write_policy=, fast_codec=,
fast_zstd_level=)`.

| policy | p50 | store bytes | restore 169 tensors | digest |
|---|---:|---:|---:|---|
| planned (level-3 ZSTD) | 41.2 ms | 5,453,026 | 144.1 ms | `b2a00193…` |
| fast + raw | **36.0 ms** | 5,912,028 (+8.4 %) | 148.7 ms | same |
| fast + zstd-1 | 42.3 ms | 5,453,389 | 147.1 ms | same |
| fast + zstd-1, then transcode | 39.2 ms (+584 ms later) | → 12,577,339 | 148.5 → 154.2 ms | same |

Raw is the only fast representation that wins (36.0 ms, −5.2 ms, +8.4 % bytes);
zstd-1 ties level 3 on 29 KB shards; deferred transcode costs 584 ms and grows
the store by 7.1 MB because the old version is retained. **Every variant is
acknowledged only after its frame is published and committed**, so exactly one
valid representation exists at ack time and the transcode is a separate,
budgeted later pass.

### Increment 7 — the container is built in the memfd

`_execute` used to assemble a `bytearray` and then copy it into a memfd (two
full-buffer copies, ~5 MB of Python staging). It now allocates a memfd of the
exact final size, maps it writable, writes the header and copies each payload
straight into the mapping, hashes from the mapping, and passes the descriptor
to `WRITE_CHECKPOINT` (`container_fd=`/`container_size=`).

Receipt `bench/receipts/checkpoint-latency-20261111T041200Z.json`: copies
**2 → 1**, staging buffer **4,989,309 B → 0 B**, completion 40.2 → 40.1 ms
(unchanged), `stage_total` 2.48 → 3.06 ms (slightly worse: a per-tensor
`ctypes.memmove` into a mapped page costs more than one bulk write). Restored
digest identical, `container_bytes` identical, and
`test_direct_container_matches_the_staged_path` asserts the copy count, the
zero staging buffer and identical per-tensor hashes from both stores. The
Python content root is kept deliberately as the independent check against the
Rust manifest; it now reads from the mapping, so one buffer serves both.

### Increment 8 — the goodput court re-run with an equivalent-durability baseline

Receipts `bench/receipts/training-goodput-20261111T044800Z.json` (5 configs,
2 reps, 128 steps, every 32, 1 kill rep) and `…T050200Z.json` (1 rep, 64 steps,
**every 4**, i.e. an interval shorter than a completion). Both are **reduced**
runs and say so in the receipt.

Two configurations added: **D `torchsave-durable`** (write temp, `fsync`,
`os.replace`, `fsync` the directory — exactly "bytes durable before return" and
"old-or-new destination", and nothing else WeftFS offers) and **E `weft-fast`**
(`write_policy=fast`, `fast_codec=raw`, no semantic/typed candidates or
deltas).

Standard interval: A 1,411.5 ms loop / 3.84 ms completion; **D 1,414.8 /
4.20 ms**; B 1,528.1 / 21.09; C WeftFS 1,587.8 / **75.57**; E 1,604.2 / 72.98.
So the durability promise costs 0.36 ms locally, and WeftFS's completion is
**17-18x D**, not 20x the non-durable A. The performance floor does **not**
win (73.0 vs 75.6 ms, +8.6 % bytes): the remaining cost is the round trip,
publication and commit, not the optional planner services. Step p50 is not
slower (10.27 ms, the lowest of the five). Exactness 40/40 states, 40/40 losses,
30/30 post-step states; kill court 5/5 scenarios with every resume exact and
`verify --full` clean.

Short interval (every 4 steps ≈ 40 ms): WeftFS waits **149.9 ms** for the
previous save (`weft-fast` 161.8 ms), lands four steps late and costs a
**2,490 ms** loop against 951/939/1,188 ms for A/D/B — **2.6x** `torch.save`
and 2.1x the durable baseline. All 80 checkpoints restored exactly. This is the
regression R11 predicted and did not measure, and it is not fixed.

### R12b — where it landed

| increment | p50 completion | receipt |
|---|---:|---|
| R11 baseline | 444.1 ms | `training-goodput-20261110T200930Z` |
| 1 profiler | 472.6 later / 211.1 fresh | `checkpoint-latency-20261111T021640Z` |
| 2a semantic-search opt-in | 81.5 / 67.9 | `…T022409Z` |
| 2b pinned-strategy shortcut | 60.6 / 42.6 | `…T022557Z` |
| 2c session | **42.2** | `…T022638Z` |
| 3 one atomic commit | **37.7** | `…T024824Z` |
| 4 index chain kept | 40.6 (noise on this fixture) | `…T030522Z` |
| 5 packing (not adopted) | 39.3 unpacked | `…T034200Z` |
| 6 fast policy | **36.0** raw / 41.2 planned | `…T035900Z` |
| 7 container in the memfd | 40.1 | `…T041200Z` |
| 8 goodput with D/E | 75.6 async on a contended box | `training-goodput-20261111T044800Z` |

**444.1 → 37.7 ms p50 (11.8x) with the planned policy, 36.0 ms with raw
frames.** 20 ms was **not** reached, and the measured shares say why: the round
trip is 21-23 ms (encode 5.7, digest 3.0, blob publish 1.6, index 3.7, commit
0.45, ~6 ms of chunking/manifest/state apply), Python DCP-metadata
serialisation 4.2, the finish commit 4.0, Python staging + hashing + memfd 3.4,
and ~6 ms of DCP overhead outside every instrumented section. The candidates
for the last 16-20 ms — a row-level incremental index, optimising the Python WCB
encoder, folding the two round trips, packing, and in-service parallel encode —
are each either unimplemented or measured as insufficient alone (packing was
measured and rejected; the index chain cache only removed the re-read).

The short-interval regression (a save completing slower than the checkpoint
interval) is measured (149.9 ms wait, 2.6x `torch.save`) and not fixed. Go has
no R12b parity because every change is Rust or Python.

Evidence trail: `bench/RESULTS-checkpoint-latency.md` (Sections 1-13),
`bench/RESULTS-training-goodput.md` Section 10, `docs/INDEX-AMPLIFICATION.md`
(R12b section), `docs/PYTORCH-DCP.md` Sections 9.1-9.2, and the receipts named
above. Gates at the final commit: Rust all-targets 758 passed / 0 failed,
FUSE 801 passed / 0 failed / 2 pre-existing ignored, Python 368 passed / 70
skipped, claims 884, spec drift 60/60, conformance manifest 420 entries.

## Subphase R13 — restore scaling: the backend mutex (2026-11-11)

R10 measured 1.21x at 8 requested workers against a 9.92x Amdahl cap and named
one backend mutex. R13 proves it, removes it for immutable content, adds
within-tensor shard concurrency, and measures the result. Full analysis and
every loss: [`bench/RESULTS-restore-scaling.md`](../bench/RESULTS-restore-scaling.md).

**Proof (525f884).** An opt-in lock probe records mutex wait, hold, demand,
and per-tensor attribution (`tests/lock_probe.rs` proves one thread cannot hold
longer than its wall time). 0.5B, whole-file SHA: 806/1464/1411/1403/1408 MB/s
at 1/2/4/8/16 requested workers; hold share 0.21/0.41/0.41/0.41/0.41 and wait
share 0.00/0.14/0.50/0.50/0.51, demand 1/2/4/4/4. 3B: 818/1709/1976/1975,
hold 0.23/0.53/0.65/0.65, wait 0.00/0.18/0.75/0.74. At four workers the lock
was 42 % of the 0.5B embedding's wall and 53-60 % of a 64 MB MLP tensor's
(37 % and 53-55 % for 3B). Hold p50/p95 15/20 us and 19/28 us; wait p95 48/60
us. One thread holds; four wait.

**Design.** `src/read_pool.rs` — at most 64 idle handles for `objects/` and
`manifests/`; borrow, read, release, then hash. Every mutation bumps the pool
epoch and drops idle handles (a packed-plane handle caches its catalog); a
released handle's counters are harvested into `BackendStats`, which
`backend_stats`/`plane_stats` merge. 3FS is not pooled (ring duplication) and
keeps the old path. `src/checkpoint.rs` reads a tensor's shards with
`parallel::run_bounded` (`shard_threads`, default 1; `shard_max_inflight_bytes`,
default 64 MiB); per-shard checks run in the worker, assembly and the
whole-tensor root check stay in order, lowest-index error wins.

**After (9e821dd).** State-mutex hold share **0.000** everywhere (12 prologue
entries, 1.7 us). Store-only restore:

| fixture | 1 | 2 | 4 | 8 requested |
| --- | ---: | ---: | ---: | ---: |
| 0.5B shard_threads=1 | 1188 | 2233 | 2776 | 2780 MB/s |
| 0.5B shard_threads=4 | 2354 | 3801 | 2903 | 3394 |
| 3B shard_threads=1 | 1177 | 2090 | 2998 | 3073 |
| 3B shard_threads=4 | 2390 | 3480 | 2439 | 2438 |

Same method as the baseline (whole-file SHA): 0.5B 807/1560/1549/1541/1550
(+1.4/+5.3/+11.2/+9.0/+8.7 %); 3B 792/1896/2039/2050 (**-1.5**/+9.9/+1.5/+2.9
%). Single embedding: 0.5B 552 → 454 ms, 3B 1303 → 1072 ms. Four independent
readers: 1,860-2,658 MB/s aggregate (mean 2,233) vs 1,188 single. MinIO:
179/345/491/495 MB/s at 1/2/4/8; readers 219-379 MB/s. Peak RSS +20.2 % (0.5B)
and +14.4 % (3B) for shard_threads=4. Exact bytes and container SHA-256 at
1/2/4/8/16 workers x 1/2 tile threads x 1/4 shard threads.

**Losses.** The whole-file SHA stays a serial 0.39 s / 2.4 s floor (the 3B
one-worker change is negative and the four-worker change is +1.5 %); 4x4
oversubscription is slower than 2x4 and triples 3B backend I/O; there is no
automatic worker budget; 3FS is not pooled; region restores remain sequential;
MinIO is one loopback process and not a cloud claim; one host, warm cache.

**Receipts.** `restore-scaling-20261111T090904Z` (0.5B),
`restore-scaling-3b-final-20261111T092000Z` (3B),
`restore-scaling-readers-20261111T091500Z` (5-repetition readers),
`restore-scaling-minio-20261111T085500Z` (MinIO); baselines
`restore-scaling-20261111T063612Z` and
`restore-scaling-3b-baseline-20261111T071500Z`. Retained failure:
`restore-scaling-minio-20261111T083500Z.log` (an uppercase, non-DNS bucket
name refused by MinIO). A cold-cache 3B baseline run was discarded rather than
cited: it mixed cache state with the worker count.

**Gates.** `cargo test --all-targets` 770 passed / 0 failed; `cargo test
--features fuse --all-targets` 813 passed / 0 failed / 2 pre-existing ignored.

### R13 follow-up — the aggregate caught a packed-write regression (dd86171)

The 45-court aggregate on the registered tree
(`verify-20261111T103327Z.json`) ran 44 pass / 1 fail: `go_store_and_fuse`
failed `TestRustPacksGoReadsAndVerifies` because `weft --pack write` exited 1
with `Corrupt: missing blob objects/49/4986e1ee…: does not exist`. This is a
real defect in increment 2, not a test artefact, and it is reproduced in the
fuse court and in Rust.

**Cause.** A packed blob plane keeps the members of its open pack in memory
until `seal` publishes the catalog. `Store::store_file_ranges` stores the
rewritten chunk, then re-reads *every* chunk to feed the frozen whole-file
`ContentId`. Before R13 that read-back used the writer's own handle; after the
pool landed it used a pooled handle, which is a different plane instance whose
catalog predates the member, so the key resolved as missing.

**Fix (implemented, `dd86171`).** `BlobPlane::buffered_blob_kinds()` reports
which kinds a handle is holding unpublished (`BUFFERED_OBJECT`,
`BUFFERED_MANIFEST`); `Store::read_blob_verified_keyed` keeps the pool for every
other read and counts the bypassed ones as
`ReadPoolStats::buffered_blob_reads`. The mask is widened before each mutation
and narrowed from the plane after it (no reader can see a narrow mask while a
member is unpublished); `CatalogState::buffered_kinds` is maintained with the
member lists instead of scanning them. Index segments are packable but always
read through the writing handle, so they carry no bit and do not disable
pooling. Publication order, content addressing, the state mutex, and the frozen
formats are untouched.

**Evidence.** The full 0.5B local matrix was re-run on the fixed tree:
`restore-scaling-20261111T120026Z`, 0.5B store-only 1,198.7 / 2,248.7 / 2,808.2
/ 2,813.6 / 2,803.4 MB/s at 1/2/4/8/16 workers against 1,187.9 / 2,232.9 /
2,776.4 / 2,779.9 / 2,792.2 before; identical backend bytes (992,589,671),
decode bytes (988,065,536) and container SHA-256; `buffered_blob_reads` 0 in
every restore row (restores never bypass the pool). The shard-4 8-worker cell
is 2,762 vs 3,394 MB/s and the shard-4 16-worker cell 2,537 vs 2,953: one host,
one repetition, cache-dependent — reported, not smoothed. Regression test
`tests/read_pool.rs::a_packed_write_reads_back_the_members_it_has_not_yet_catalogued`.
Gates after the fix: Rust all-targets 771 passed / 0 failed (158 suites), FUSE
all-targets 814 passed / 0 failed / 2 ignored, WeftFS-Go `go vet` + CGO=0 build
+ `go test -race ./store/...` ok (136.3 s), claims 892, spec drift 60/60,
conformance manifest/coverage 420.

**Aggregate (45/45).** With the fix in place the full court aggregate passes:
`bench/receipts/verify-20261111T131235Z.json`, **45 courts, 45 passed, 0
failed, 0 skipped** at WeftFS `40df0cd`, including the reduced
`restore_scaling` court registered by `3f96fe0`. The `rust_store` court records
771 passed / 0 failed (158 suites) and `rust_fuse` 814 passed / 0 failed / 2
pre-existing ignored. The failing run that found the packed regression,
`bench/receipts/verify-20261111T103327Z.json` (44/45), is retained. A second
attempt at 45 courts (`verify-20261111T120727Z`) is retained as an
**environmental** failure, not a code result: the host filesystem ran out of
space during `restore_scaling` and the court log stops mid-write; the run left
no receipt. The untracked, non-R13 WIP file `WeftFS-Go/store/azureclient.go`
was truncated to 0 bytes by that disk-full event while the isolation helper
moved it aside (its original sha256 was `d8eb00d9…351e`); it is not part of any
R13 commit, it never compiled (`unexpected EOF` at line 208), and the helper is
now copy-then-verify so a full disk can no longer destroy the only copy.

- 2026-11-11 (R14, Go parity: Azure Blob): parent `staging` `e2b25b1`,
  WeftFS-Go `staging` `318d4b0`; pushed. Closed the largest remaining Go
  gap: the `azure` plane was Rust-only. `store/azureclient.go` /
  `azureblob.go` / `azurecommit.go` implement Azure Blob REST from the same
  contract the Rust backend speaks (`Put Blob` + `x-ms-blob-type: BlockBlob`
  with `If-None-Match: *`; `If-Match: <ETag>` for CAS and for the fenced
  `LOCK`; ranged `Get Blob`; `HEAD`; `Delete Blob`; `List Blobs` with marker
  paging). SharedKey is HMAC-SHA256 over the documented string-to-sign with the
  standard library only; SAS is appended verbatim; anonymous is emulator-only;
  exactly one credential form is accepted and secrets never reach diagnostics.
  The If-Match spelling is measured at a writable open (matching accepted,
  stale refused, winner bytes intact); an endpoint that ignores
  `If-None-Match: *`, accepts a stale ETag, or passes neither form is refused
  `Unsupported`. The head is one conditional `LOCK` write carrying the fence
  and the head; the superblock is a cache; release is a content-addressed
  tombstone.
  * The predecessor of `store/azureclient.go` was the 7,178-byte WIP that the
    R13 disk-full event truncated to 0 bytes. This is a **new** implementation
    from the Rust contract and the Blob documentation, not a recovery of those
    bytes.
  * Court `bench/scripts/azure_court.sh` (pinned Azurite 3.35.0,
    `sha256:647c63a9…`): Rust `azure_backend` 19 passed / 0 failed (13.68 s),
    Rust `azure_fencing` 9 passed / 0 failed (18.68 s), Go
    `go test -run 'TestAzure|TestDecodeBase64' ./store` **18 passed / 0 failed
    / 0 skipped** (0.476 s). Receipts
    `bench/receipts/azure-court-20261111T144613Z.json` and
    `bench/receipts/azure-20261111T144613Z.json`; log
    `bench/receipts/azure-go-20261111T144613Z.log`. Earlier stamps
    `144136Z` (a Go credential test described a bare caller while the court
    exported `WEFT_AZURE_KEY`), `144308Z`, and `144452Z` are retained; the
    first caught a real test defect, and `144452Z` ran 17 of 18 Go tests
    because the `-run 'TestAzure'` filter excluded the base64 strictness test.
  * The Go suite includes two adversarial endpoints a well-behaved emulator
    cannot produce — one that ignores `If-None-Match: *` and one that accepts a
    stale `If-Match` — both refused `Unsupported`, plus a literal-comparison
    endpoint whose dequoted form is negotiated and recorded. One container
    proves the handoff both ways: Go writes → Rust `info`/`verify --full`/`cat`
    exact; Rust writes + `checkpoint` → Go reads exact, `verify(true)` clean;
    Go then reclaims the Rust tombstone and Rust verifies again.
  * Go MW2 remains `local`/`object`; `--writer-mode optimistic` on the `azure`
    commit plane is refused `Unsupported`, asserted by test.
  * Full Go suite `go test -race ./...` in the **fuse** court: 5 packages
    `ok`, 0 failed (the `go` court lacks `/bin/fusermount`, so `fusefs` fails
    there — an environment gap, recorded, not a code result).
  * Not claimed: no live Azure; no SAS-acceptance or anonymous-acceptance
    court in Go; no performance numbers; single node, tmpfs, loopback; no
    media/power-loss/fleet/cloud claim; multi-page listing and packing over
    Azure unexercised. Details and every loss:
    `bench/RESULTS-azure-go.md`.

- 2026-11-11 (R14, Go parity: read-handle pool): WeftFS-Go `staging` `de9a4b9`;
  pushed. `store/read_pool.go` is the counterpart of the Rust
  `src/read_pool.rs`: at most `Options.ReadPoolIdle` idle handles (default 64,
  negative disables), oldest closed first, the mutex protects only the idle map
  and never the I/O, and reads use `ReadAt` so concurrent readers cannot disturb
  a shared offset. The hash is computed after the handle is released, so
  verify-then-trust is unchanged; remote planes have no descriptors and are
  never pooled. `Store.ReadPoolStats` reports opens, reuses, refreshes,
  evictions, invalidations, reads and bytes.
  * The first version reused an idle descriptor without re-checking the path,
    and the full suite found two real regressions: both
    `TestOverlayRangeReadsOnlyIntersectingChunks` (a range succeeded with its
    chunk missing) and `TestTensorMissingDependencyFailsClosedAndGC` (a read
    with a missing dependency returned bytes) delete a blob behind the store's
    back and were served from the still-open descriptor. An idle handle is now
    validated with one `stat` before reuse: a vanished path closes the handle
    and returns `NotFound`; a changed size or mtime reopens instead of reusing.
    Both tests pass again and `TestReadPoolVanishedBlobFailsClosed` asserts it
    directly.
  * Tests `store/read_pool_test.go` 9/9 under `-race`; full Go suite in the
    fuse court at `de9a4b9`: 5 packages `ok` / 0 failed, `store` 210.618 s.
  * Measured descriptor cost only (go court, tmpfs, warm cache, 73,728-byte
    file, `go test -run '^$' -bench 'ReadBlob' -benchtime 300x -count 3`):
    unpooled 6,939 / 7,240 / 8,420 ns/op, pooled 6,321 / 5,156 / 5,334 ns/op.
    Spread is wide; no tighter figure is asserted and **no restore-throughput,
    p50/p99, Rust-comparison or fleet claim** is made. Details:
    `bench/RESULTS-go-read-pool.md`.

- 2026-11-11 (R14, documentation and tool-version hygiene): review items
  T001–T003 and T006–T007, which were the cheap credibility defects.
  * README engineering rule 6 no longer says "Not a multi-writer protocol":
    it now states MW1 default, MW2 opt-in (`--writer-mode optimistic`, explicit
    transaction API, not FUSE or generic checkpoint publication), and MW3 not
    implemented and not claimed, pointing at `docs/FENCING.md` and
    `spec/WEFT-MULTIWRITER.md`.
  * README Rust install examples moved from `1.1.0` to `1.2.0`
    (`cargo add weftcodec@1.2.0`, `cargo add weftfs@1.2.0`), matching the live
    crates.
  * README gained a `## What's new in v1.2.0` section above the historical
    v1.1.0 one, summarising S7–S13b and the MW2/mode-19/retention/transport/
    index work, and repeating the stale-PyPI-long-description defect that only
    a 1.2.1 upload can fix.
  * `docs/PROGRAM.md` E1, E2, E5 and E7 said "not started" while Context Roots,
    `causal_exact`, training-ready materialization and the Buck2/agent-memory/
    adapter surfaces all exist. They now state what exists, with the receipt,
    and name what is still absent (minimal sufficient closure queries, dataset
    and eval slices, any upstream adoption).
  * WeftFS-Go `store/rebase.go` recorded tool version `1.1.0` through the
    v1.2.0 release, so a Go rebase provenance record named a tool version that
    was two releases stale while Rust records `env!("CARGO_PKG_VERSION")`.
    R14 aligned it to `1.2.0`; the constant is now `1.3.0` for this release and
    carries a comment that it must track the released version. Old records are
    unchanged (provenance ids are content-addressed); new ones name the right
    tool. `TestRebase*` and `TestPlanner*` pass in the go court.

- 2026-11-11 (R15, store-level KV parity in Rust and Go): parent `08456c1`,
  WeftFS-Go `225eb0d`, WeftCodec `67cea30` (spec mirror), WeftCodec-Go
  `5e047c3` (spec mirror); pushed. Closes the R14c bookkeeping gap and the
  reviewer-visible weakness behind it.
  * The Rust CLI had no page-level KV writer, so the old Rust→Go mode-19 test
    copied attribute values Rust had printed for a **Go-written** object: it
    proved Rust's byte copy and Go's reader, not that Rust's
    `write_kv_prov_page` derives the same descriptor. `weftfs kv prov-write`
    now exists (additive; frozen formats untouched). It takes the element bytes
    verbatim, every mode-12 geometry field and every provenance dimension,
    calls the shipped writer, and prints object/descriptor/provenance-root/
    frame/page sizes.
  * `tests/kv_provenance.rs::cli_prov_write_matches_the_store_api` (rust court
    6/6 for the file) proves the CLI and `Store::write_kv_prov_page` publish
    the same descriptor id, provenance root and frame bytes for identical
    inputs. The full parent suite at `08456c1` is 158 suites / 772 passed /
    0 failed / 0 ignored. WeftFS-Go's `TestKvProvHandoffRustThenGo` now calls
    the verb and asserts the same equality in the other direction. Observed
    identity: descriptor `9c8344b0…6ea3b1a`, provenance root
    `ba5ec110…0d5d7f5f`, 5,509-byte frame, 4,096 exact bytes — equal in both
    languages.
  * Go store parity itself (`store/kv.go`, WeftFS-Go `db35a4e`) covers mode 12
    and mode 19 write/read, the two canonical descriptors and strict parsers,
    the reserved attribute set, the one denial text and the normative
    authorization order. `KvPrincipal{Principal, Capability}` is the documented
    API difference: Go has no envelope type and verifies no signature on this
    path (pre-existing gap, not widened here).
  * Measured Go costs (go court, tmpfs, warm cache, 200 iterations, one commit
    per write, benchmem): 4 KiB page write 10.56 ms/op / 4.08 MB alloc,
    4 KiB read 0.697 ms / 238 KB alloc, 64 KiB read 2.46 ms / 406 KB alloc.
    Published as costs, not a competitive result: the Go store has no
    decoded-object cache or single-flight, and every page write is its own
    commit. No p50/p99/Rust-comparison/fleet claim.
  * Go full suite in the fuse court at `225eb0d`: cmd/weftfs 44.544 s,
    fusefs 7.273 s, index 1.004 s, model 1.009 s, store 215.102 s, 0 failures;
    gofmt/vet/CGO-disabled build clean. Logs
    `bench/receipts/kv-go-parity-fuse-20261111T154406Z.log` (plus the focused
    Rust and Go logs of the same stamp) and
    `kv-go-bench-20261111T154406Z.log`. Analysis:
    `bench/RESULTS-kv-go-parity.md`.
  * Spec: `spec/WEFT-KV-PROVENANCE.md` §10 now covers both stores and §11 no
    longer says the capability surface is Rust-only. Mirrors re-exported;
    spec drift 60/60.
  * Still absent: Go KV prefix tier (`kv_prefix_*`, `kv gc`), Go envelope
    verification, any FUSE or serving-engine KV path, and a Go CLI KV page
    writer.

- 2026-11-11 (R16, v1.3.0 release): parent `staging` == `main` `e0d8cb6`;
  tags exist in all four repos. Nothing was pushed, tagged, merged, or
  published while recording this entry.
  * The parent `v1.3.0` tag resolves to `e0d8cb6` and was **moved twice after
    creation**: `37c8942` (release aggregate) -> `c68b3d5` (added `COPY examples`
    to `Dockerfile.weftfs`) -> `e0d8cb6` (removed the explicit `[[example]]
    fleet_coldstart` stanza and made the example gate itself, because the CI
    image build context does not contain `examples/`). The submodule tags were
    unaffected by those moves: WeftCodec `d9234e5`, WeftCodec-Go `fe05240`,
    WeftFS-Go `80ddb5b`.
  * The 45/45 aggregate `bench/receipts/verify-20261111T182601Z.json` was run
    at parent `3ab54c7`, which is **three** commits before the final tag (the
    hand-off said two; the commit graph says three). Between them are
    `37c8942` (aggregate, README, docs and receipts), `c68b3d5`
    (`Dockerfile.weftfs` +4) and `e0d8cb6` (that +4 removed, the
    `Cargo.toml` `[[example]]` stanza removed, `examples/fleet_coldstart.rs`
    +30 and self-gating). The net `Dockerfile.weftfs` delta is zero; no codec,
    store, or runtime code changed, and the aggregate's codec/store revisions
    are exactly the submodule tag commits.
  * crates.io `weftcodec 1.3.0` (checksum `45cfbd12...`) and `weftfs 1.3.0`
    (`f7e394ac...`) are live and neither is yanked. All four GHCR images exist
    at `:v1.3.0` and `:latest`, and each pair resolves to one index digest:
    weftfs `07c70fd7...`, weftcodec `0b8cc85f...`, weftcodec-go `ad627083...`,
    weftfs-go `396cb94b...`. Running the `weftfs:v1.3.0` image with `--version`
    printed `weftfs 1.3.0` (`docker run --rm` on the image above).
  * Python artifacts are built and `twine check` passed: wheel
    `weftcodec-1.3.0-py3-none-any.whl` sha256 `ba1ddf26...` (99,438 B) and sdist
    `weftcodec-1.3.0.tar.gz` sha256 `d6eb391e...` (124,756 B). **PyPI is not
    published**: `scripts/publish-pypi.sh` requires `.docker-cache/.pypi-token`,
    that file does not exist, and no token was available. This is a blocked
    item, never a completed publication.
  * `bench/receipts/release-artifacts-20261111T200115Z.json` records the four
    tag commits, the live crates.io checksums, the image index digests, the
    Python artifacts and the PyPI block. The tag-move history is
    operator-recorded because no local tag reflog is retained; the current tag
    target and each named commit's contents were verified, as were the registry
    values.
  * `docs/RELEASE-v1.3.0.md` gained a "Release engineering notes" section with
    the tag moves, the aggregate delta and the PyPI block. No measured number
    or existing claim was changed.

## Subphase R12c — persistent checkpoint sessions (2026-11-11)

Plan: [`docs/PLAN-R12C.md`](PLAN-R12C.md) (seven items; this session covers
subphase A and subphase B = item 1). Starting revision `d3db8aa`.

### A — credibility and reproducibility gaps

* The plan exists and labels each item with mechanism / acceptance / evidence /
  not-claimed.
* Fixed now: `python/README.md` claimed **"weftcodec 1.3.0 is published on
  PyPI"**; the release artifact receipt says `published: false` (no token at
  `.docker-cache/.pypi-token`). It now states the block, the wheel/sdist sha256,
  and that `pip install weftcodec==1.2.0` is what PyPI serves today. The parent
  README v1.3.0 artifacts bullet said the crates/PyPI/images "are cut by this
  release's publishing step"; it now records the outcome (crates 1.3.0 live
  with checksums, four GHCR index digests, PyPI blocked) from
  `bench/receipts/release-artifacts-20261111T200115Z.json`.
  `docs/containers.md` carried the same two stale sentences and was corrected.
* Fixed now: the README verification and multi-writer headlines named a receipt
  but no revision. They now name parent `3ab54c7` (three commits before the tag,
  no codec/store/runtime change in the delta) and WeftFS `b0bcbc5e` /
  WeftFS-Go `f46b351f`, and say that **no aggregate exists at the current
  `staging` HEAD**.
* Fixed now: the README marked the older per-release sections as historical, so
  the v1.1.0 "PyPI remains unpublished" sentences and the v1.2.0 "the 1.3.0
  upload rebuilds the description" sentence read as history, not as current
  status.
* Fixed now: `docs/PROGRAM.md` P8 described the conformance surface as it was
  when the phase opened (295 entries, 288-file corpus, adapters 206/0/89 and
  56/0/239, coverage modes 0–14, Python reader to mode 8). It now states 420
  entries / aggregate `6287b817` / 413 identical shared files at `aa3abcad` /
  Rust and Go 288/0/132 / Python 229/0/191 / coverage modes 0–20 / Python reader
  0–18, and keeps the old numbers in a paragraph labelled historical.
* Fixed now: this file's head called v1.3.0 a release candidate with "no receipt
  claimed yet"; it now records the release, the 45/45 aggregate at `3ab54c7`,
  and the absence of an aggregate at HEAD.
* Verified already fixed by `81d2da4` and unchanged since: README engineering
  rule 6 (MW1 default, MW2 opt-in via `--writer-mode optimistic` on the explicit
  transaction API, MW3 not implemented); `WeftFS-Go/store/rebase.go`
  `storeVersion = "1.3.0"` (line 28), matching `Cargo.toml` and the release.
* Gates after the edits: `scripts/check-claims.sh` OK (927 entries), spec drift
  OK (60/60).

### B — persistent sessions inside the training-goodput court

Full analysis and every loss:
[`bench/RESULTS-training-goodput-session.md`](../bench/RESULTS-training-goodput-session.md).
Receipt `bench/receipts/training-goodput-session-20261111T220500Z.json` at
WeftFS `76f0981`, WeftCodec `d9234e5`, WeftCodec-Go `fe05240`, WeftFS-Go
`146b517`; earlier runs `…T210500Z` (`7d5f5eb`) and `…T213500Z` (`7d0d19c`)
are retained and are not the current number.

The harness gained an **opt-in** `weft-session` configuration: one
`CheckpointSession` per (store, run), every checkpoint submitted through
`session.save`. The default `run` court is unchanged (still the five R12b
configurations; a reduced default run was executed to confirm). New `sweep`
subcommand and `bench/scripts/training_goodput_session_court.sh`.

192 steps, 3 repetitions, 4/8/16/32 steps between checkpoints, same 169-entry
4,989,309-byte fixture, same durability (528 backend fsyncs and 265 writes per
save in **both** arms). Warm completion p50 (ms): **339.0 → 62.9** at 4 steps,
**173.6 → 50.6** at 8, **119.9 → 47.7** at 16, **94.7 → 46.3** at 32. Loop wall
17,615.8 → **4,476.3 ms** at 4 steps; training-thread pause p50 3.99 → **0.80
ms**. Server-side publish p50 115.5 → 28.0 ms (4 steps): the win is the reused
writable handle, not a faster encoder.

Losses published: at 4 steps the clean step p50 rises 13 % (11.99 vs 10.61 ms,
p95 +45 %) because more save work overlaps training; the first checkpoint is
~2x slower (helper start); at 32 steps the loop wall is 4.3 % **worse**; a warm
session holds the writer lock for up to `session_idle_secs`; `--resume final`
verifies every checkpoint of the first repetition only.

Exactness: 196/196 resumed states, 196/196 next-step loss bits, 172/172
post-step states; identical loss trajectory and final state in every cell.
SIGKILL through a session (`checkpoint_every=16`, 3 repetitions): after-ack and
after-completion resume the new checkpoint (3/3 each); `before_ack_half`
resumes the previous one (**3 old / 0 new**, 16 steps lost, 3/3 exact). All
`verify --full` clean.

Five python-court session tests pass, including two new ones: a warm session
still refuses an independent writer with `Busy` and releases it on close, and
`WEFT_CRASH_PHASE` through a session exposes old-or-new at `after_objects`,
`after_commit_file` and `after_superblock_write`. The crash test recorded a real
observation: a kill between the commit-file fsync and the superblock leaves the
old complete state visible and `verify --full` reports
`commit file for generation N exists above the highest published superblock
generation N-1` — reported, not hidden; the test asserts it is the only problem
in that phase.

- 2026-11-11 (R12c item 2, operation-derived incremental index maintenance):
  parent `staging` code `7d7e2af` (baseline evidence `036add1`); not pushed yet.
  * `Store::index_attestation_for` no longer calls `StateIndex::build` on every
    publish. New `src/index_incremental.rs` takes the previous head's index,
    applies only the rows the committed ops changed, and records element-level
    membership before each mutation; `index_chain::encode_delta_changed` writes
    those rows with the unchanged WFX3 tags/patch rules. A unit test proves the
    derived delta bytes equal `encode_delta`'s whole-state diff.
  * Pre-images come from the pre-op state (single commit), the group undo
    journal (`group_pre_relations` added), or the MW2 transaction base. Any
    missing pre-image, unexplained row, generation gap or failed `Arc` unwrap
    returns "rebuild": `maintenance_fallbacks`, a counted `StateIndex::build`,
    and a full segment. `maintenance_updates` counts the fast path. The
    in-memory chain is validated against the published commit record before
    use (counts, chain kind/base/depth/root, recorded `candidates_root`); the
    disk path still recomputes the candidates root. Trust, format and the
    `weftfs.commit.v2` attestation are unchanged.
  * Reader-merge bound is the existing writer bound: `index_chain_max_deltas`
    deltas plus one full ancestor, **32 + 1 = 33** by default, hard reader cap
    256 + 1. No new segment format; `weftfs compact` is unchanged.
  * Baseline at `b69fa0e` (bench court, 3g/4cpu/pids 1024, bounded /tmp,
    release, tmpfs, warm cache), history sweep with a constant 64-object live
    set and one `set_xattr` commit per step: commit p50 0.198 / 0.201 /
    0.199 ms at 1k / 10k / 100k generations; index tree 0.83 / 8.20 / 81.82 MB.
    At 1,000 generations: 0.198 ms (64 live), 2.077 ms (1,000 live),
    22.577 ms (10,000 live); `index_build` was 0.848 ms and 9.630 ms of those.
  * Post-change, same fixture and sizes: p50 **0.110 ms / 0.112 ms / 0.112 ms**
    at 1k / 10k / 100k — flat within 1.8 % while history grows 100x — and the
    index bytes are identical in every cell. Live-state cells: 0.641 ms at
    1,000 live (3.24x faster), **5.569 ms** at 10,000 live (4.05x), 0.353 ms
    at 512 live (3.08x). Full-segment control 0.155 → 0.114 ms. A same-revision
    control with `incremental_index_maintenance = false` reproduces the
    baseline within 0.5 % (2.072 vs 2.077 ms), so the comparison is the change.
    Cold first-query p50 is unchanged within noise; peak RSS fell
    (64 live 16.2 -> 12.2 MiB, 10,000 live 167.2 -> 86.0 MiB).
  * Still O(index) per commit and published as such: the attested
    `candidates_root` (the trust contract) and the forced full segment every
    33 commits; live-state cost is ~0.5 us/object (was ~2.2). Removing that
    needs a Merkle row root (new attestation) or a larger merge bound.
  * Correctness: `tests/index_incremental.rs` 3/3 — 10,000 randomized ops per
    seed for seeds 1, 7, 2026 (create, attr update, unrelated metadata edit,
    delete, rename, object relation, tag add/delete) with byte-exact WFX2
    section equality against `StateIndex::build` every 500 ops and at the end,
    query equality against the scan oracle, group commits, and damaged/absent/
    orphan segment rejection; unit tests 5/5; `tests/index_chain.rs` gains a
    self-consistent wrong-ancestor rejection. Rust all-targets 784 passed /
    0 failed (at `8d324dc`; the `7d7e2af` message's 782 predates the last two
    unit tests); FUSE 827 passed / 0 failed / 2 pre-existing ignored; Go store
    `Index|Handoff` suite PASS (78.7 s).
  * Cross-language is format compatibility only: Go resolves the Rust delta
    chain and re-encodes it byte-identically, and a Rust store at this revision
    verifies from Go (`verify --full`, `generation=6`). **No Go incremental
    maintenance and no Go timing** are claimed.
  * Found and **not fixed** (out of item scope): `expand_retained_index_chains`
    applies `HARD_MAX_CHAIN_DELTAS` to one `visited` set shared by every
    retained root, so index GC refuses once a store retains a few checkpoints —
    `index GC skipped: cannot resolve a retained segment chain: LimitExceeded`.
    The index tree then grows with commits; the bounded /tmp filled at
    generation 80,785 (512 live) and 9,750 (10,000 live), both retained.
    1,000,000 generations was not attempted for that reason (linear
    ~0.4 KB/commit) and is not extrapolated.

- 2026-11-11 (R12c item 2.1, index GC chain bound per retained root):
  parent `staging` code `686bce5` + receipts `4c4f459`; WeftFS-Go `a7170f5`.
  Full analysis: [`bench/RESULTS-incremental-index.md`](../bench/RESULTS-incremental-index.md),
  "GC bound fix".
  * Defect (found by item 2, fixed here):
    `Store::expand_retained_index_chains` walked the head, every snapshot and
    every checkpoint-carrying commit through **one** `visited` set and compared
    that union against `HARD_MAX_CHAIN_DELTAS` (256). The bound is meant for one
    chain walk, so a store retaining more than ~8 roots refused index GC
    outright (`index GC skipped: cannot resolve a retained segment chain:
    LimitExceeded: index segment chain exceeds the hard depth bound`), reclaimed
    0 B, and grew the index tree with the commit count.
  * Reproduction (retained failing receipt
    `bench/receipts/index-gc-bound-20261111T194000Z-prefix-tail.json`, revision
    `e1ab002`, fixed clock and content, 64 objects): 12 snapshots each pinned at
    the writer's maximum 32-delta depth plus one forced checkpoint commit and
    400 unretained tail commits — 14 retained roots, 801 commits, union of the
    retained chains **404 segments**, index tree 369,010 B. GC: refused, 0
    segments deleted, 0 B reclaimed. Eight snapshots already reproduce it
    (`…T190100Z-prefix-8roots.json`, union **268 segments**).
  * Fix: `src/store.rs` walks each retained root on its own path, memoising the
    distance from a commit to its full ancestor so a shared commit is read once
    and each root is still charged its own cumulative depth; Go
    `expandRetainedChains` (`WeftFS-Go/store/index.go`) had the identical defect
    and is fixed the same way with the same texts.
  * Invariant, in the code comment and the results doc: for every retained root,
    the walk reaches a full segment (or a commit with no WFX3 segment) within
    `HARD_MAX_CHAIN_DELTAS` delta hops **from that root**; the union is never
    the bound; sharing cannot hide an over-long chain; an over-long chain or a
    cycle still refuses the whole pass with the same typed class and deletes
    nothing.
  * Before/after, identical fixture: 801 segments / 369,010 B → 397 segments /
    **182,321 B** reclaimed, 404 segments / 186,689 B left, 0 problems,
    `verify --full` clean, and all 12 snapshots still load their whole chain on
    a fresh handle (`disk_loads >= 1`, `builds = 0`, answers = scan oracle).
    Head-only control unchanged in both revisions: 199 segments / **87,891 B**.
  * Sizes with GC at each checkpoint (bench court, default limits): 64 live at
    100,000 generations publishes 8,754,798 B instead of 81,816,646 B (0.674x
    data, 84,057 segments reclaimed); **512 live at 100,000 generations
    completes with GC** (54,986,166 B / 102,983,199 B = 0.534x); a 64-live sweep
    reaches **200,000 generations** with the post-GC index/data ratio flat at
    0.673–0.676.
  * 1,000,000 generations was attempted and still does not fit, for a reason
    that is not this defect: ENOSPC at generation **169,701** with the allocated
    breakdown at exactly 1,073,741,824 B, of which `commits/` is 695,152,640 B
    for 169,702 files — **4,096 B of commit pages per generation**. One commit
    file per generation is the history, so 1M needs ~4 GiB of `commits/` alone
    in a 1 GiB `/tmp`. Reported as the measured limiter; no extrapolation and no
    limit raised. The 512-live cell with GC only at 1k/10k/100k shows the same
    effect (ENOSPC at 85,758; `indexes/` 628,711,424 B allocated).
  * Tests: `tests/index_gc_bound.rs` 3/3 (many roots each with a long chain
    reclaim and every held chain still loads; a forged 300-delta chain still
    refuses and deletes nothing; the no-retention control reclaims exactly the
    unnamed segments). The reproducing test fails on `e1ab002`'s `src/store.rs`
    and passes on the fix. Go twins
    `TestGcChainBoundIsPerRetainedRoot` (fails on the previous Go code) and
    `TestOverLongRetainedChainStillRefusesIndexGc`. Item-2 oracle/adversarial
    re-run unchanged: `index_incremental` 3/3, `index_chain` 8/8,
    `index_persistence` 7/7, `index_equivalence` 1/1; commit p50 still 0.110 ms
    at 1k/10k/100k.
  * Gates at `4c4f459`: Rust 163 binaries / 787 passed / 0 failed / 0 ignored;
    FUSE 830 passed / 0 failed / 2 pre-existing ignored; Go `gofmt`/`go vet`/
    `CGO_ENABLED=0 go build` clean and `go test -race ./store` passing except the
    pre-existing `TestAzureOptimisticTwoDisjointCommitters` (fails identically
    on the unmodified tree, from the unverified R14d Azure-MW2 WIP). No
    `verify-all` run (item 7).
  * Not claimed: Go incremental index maintenance (still absent); flat index
    footprint (the retained set is still one root per checkpoint and snapshot,
    so it grows at 0.674x data); O(1) GC (it lists and reads every commit); any
    wire-format change (none: WFX2/WFX3, `weftfs.commit.v2` and every attested
    root are untouched).

- 2026-11-12 (R12c item 3, DCP metadata serialisation cost): parent `staging`
  code `4947ef2`; not pushed. Full analysis:
  [`bench/RESULTS-dcp-metadata.md`](../bench/RESULTS-dcp-metadata.md).
  * The R12b figure is confirmed, not assumed. `bench/scripts/dcp_metadata_court.sh`
    (python court, real `torch.distributed.checkpoint` metadata from one genuine
    save per size) measures the pure-Python encoder at **3.96 ms p50** for the
    169-entry fixture (71,559 B), and the `adapter.metadata_serialize` probe
    inside a real save reads 4.32-4.68 ms in the same session.
  * Breakdown at 169 entries: tree construction 0.68 ms (17 %), `wcb.encode`
    3.29 ms (83 %), memfd copy 0.008, sha256 0.03, and **zero Rust round trips**
    (0 `CheckpointService._call` during the window). Inside the encoder the
    cost is dispatch and per-byte `append`: 26,233 `isinstance` calls, 17,787
    `bytearray.append`, 8,784 `varint.encode`, 847 key sorts per encode.
  * Options measured on the same fixture (30 iterations): (a) reusable plan
    **0.555 ms** warm (0.121 signature + 0.400 assembly), build 6.28 ms once;
    (b) native floor — one `stat_file` round trip + client `struct.pack`
    marshalling of the entry table — **0.443 ms**, i.e. 80 % of (a) *before any
    Rust encoding, reply copy or service lookup*, so (b) is refuted without
    being built; (c) generic fast encoder **1.63 ms** (2.0x). Chosen: (a) with
    (c) as the fallback; the format stays defined once, by the oracle.
  * Shipped: `weftcodec/wcb_fast.py` (generic canonical encoder, exact-type
    dispatch, bounded key/varint caches, one join) and
    `dcp.serialize_metadata_fast` + `MetadataPlan`/`MetadataPlanCache` in
    `python/weftcodec/dcp.py`. `serialize_metadata` is unchanged and documented
    as the reference oracle; `metadata_parts` is the single schema description.
    `writer.stats["metadata_encode"]` reports mode + counters;
    `WEFT_DCP_METADATA_ENCODING=reference` forces the oracle for an A/B.
  * Safety: a plan is cached only after its assembled bytes equal the oracle's
    bytes for the same object (`oracle_checks`/`oracle_mismatches`); every save
    re-derives the signature from the current metadata before emitting any
    fragment, so a changed entry set, dtype, shape, chunk layout, storage info,
    offset or index (and a changed `MetadataIndex.index`, which torch compares
    as `compare=False`) falls back, counted, and rebuilds. Tests: 5 seeds x 200
    randomized metadata trees byte-identical cold and warm; a 12-case stale-plan
    matrix; invalid metadata, out-of-range values and lowered limits raise the
    same classes as the oracle; the 169-entry store-backed save writes the
    oracle's bytes and restores exactly. `test_wcb_fast.py`: 5 seeds x 200 value
    trees, boundary varints, unicode/bytes/empty containers, subclasses, a
    duplicate-yielding mapping (same bytes, decoder rejects), and a cache-leak
    test proving a cached key cannot bypass a lowered limit. Python suite
    423 passed / 70 skipped / 0 failed in the python court at `4947ef2` (the
    skips are the pre-existing guards; both new files pass in full).
  * End to end on one revision, A/B by encoder only:
    `checkpoint-latency-20261112T000500Z` (reference) vs `…T001500Z` (auto), same
    fixture/court/config: `adapter.metadata_serialize` session 4.679 → **0.631
    ms (-4.05)**; completion p50 40.03 → **34.58 ms**; client top-level 32.84 →
    28.27; Rust top-level unchanged. Modes counted: before `{reference-forced:
    21}`, after `{plan: 20, rebuilt: 1}` with 1 plan build, 1 oracle check, 0
    mismatches — the fast path is taken and the oracle is never silently
    substituted. Training-goodput session court (`…T003000Z` vs `…T004500Z`,
    4/8/16/32-step sweep, 128 steps, 3 reps): `weft-session` warm p50 46.85 /
    45.02 / 45.19 / 44.76 → 43.21 / 42.36 / 42.14 / 40.55 ms (mean **-3.39**);
    exactness 136/136 in both.
  * Losses: the first save of a shape pays build + oracle proof (6.28 ms + a
    2.4 ms generic encode vs 4.06 ms reference, break-even at the second save),
    visible as a 3.5-20.7 ms rise in `weft-session` cold p50 (n=3); one save per
    process never amortises; `matches` is O(entries) (0.121 / 0.789 ms at
    169 / 1024); 0.555 ms remains; 52,613 B cached at 169 entries against an
    8 MiB / 4-plan bound; the fallback path is worse than before when the shape
    changes every save; p95 stays bimodal at 1024 entries (GC).
  * Not claimed: any format, wire or store change; Rust/Go involvement in
    metadata encoding; Orbax (its encoder is untouched and unmeasured); 20 ms
    completion; multi-rank, fleet, GPU or durability. No Rust native
    encoder was written — option (b) is a measurement, not an implementation.

## Subphase R17 (write-cost) — attribution and ablation (2026-11-12)

**Status: executed; attribution and ablation only — no optimisation was
performed in this subphase.** The deliverable is
[`bench/RESULTS-write-cost-ledger.md`](../bench/RESULTS-write-cost-ledger.md)
and the six-phase plan [`docs/PLAN-R17-R22.md`](PLAN-R17-R22.md).

- **Controls (additive, defaults unchanged).** `TensorPlanOptions::float_tensor`
  (default `true`) and the existing `zstd19` gate are the two independent
  controls; both gain a CLI flag (`--no-float-tensor`, `--no-zstd19`). The
  default path is proven unchanged **at this revision**: the ablation runner's C
  row and the unmodified contract court's C row have identical strategies,
  per-tensor manifest sets, physical footprint and bytes written
  (`default_c_matches_official_court: true`). `tests/float_tensor_ablation.rs`
  additionally proves the exact encode counts (1/1 default, 1/0 and 0/1 for
  each control, 0/0 for both) and that the chosen manifest bytes are unchanged
  when the removed candidate was not selected. No frozen format, frame,
  manifest, commit or index byte changed.
- **Baseline at this revision** (`augmentation-local-20261112T041225Z`, one run
  per row): A 364.5 ms / 424,439,296 B; B 2,892.1 / 424,784,917; C 24,459.1 /
  282,283,011; D 2,910.4 / 424,856,058; E 24,174.7 / 282,354,152; N 5,504.4
  (host-contended; earlier run 358.2). 101/101 exact restores in every row.
- **Ablation matrix** (`write-cost-ablation-20261112T052203Z`, 7 interleaved
  repetitions × 10 configurations, all checks pass; least-contended values):
  C 24,229.2 ms / 282,283,011 B; C-noz19 3,849.9 / 282,283,011;
  C-nom9 22,443.3 / 330,336,572; C-both 2,132.6 / 330,532,422;
  B 2,840.2 / 424,784,917; B-group 2,139.3 / 424,608,542;
  D 2,885.6 / 424,856,058; E 24,171.0 / 282,354,152.
  C encodes 101 level-3, 18 level-19 and 101 mode-9 frames; `--no-zstd19`
  leaves 0 level-19 encodes, `--no-float-tensor` leaves 0 mode-9 encodes, both
  off leaves 0/0. **Level 19 is selected for zero tensors and saves zero bytes.**
- **Ledger (C, 24,229.17 ms):** ZSTD-19 20,352.99 ms (84.00 %); mode 9
  1,743.05 (7.19 %); blob publish 777.37 (3.21 %); commit/index 711.51
  (2.94 %, of which index 343.47 and manifest+superblock 362.86);
  ZSTD-3 436.78 (1.80 %); digest 66.33; source read 32.27; init 24.67;
  other encode/plan 41.46; prepare unattributed 25.45; footprint walk 1.43;
  **unattributed residual 15.86 ms (0.065 %)**.
- **Mode 9 trade:** 1.74 s write and ~0.6–0.7 s extra cold restore for
  48.05–48.25 MB (14.5–14.6 %), +~496 MB peak write RSS. It is the only
  footprint-reducing candidate on this fixture.
- **One-group-commit B:** commit/index 696.9 → 8.8 ms (-688.1 ms), wall
  2,840.2 → 2,139.3 (-24.7 %), footprint -176,375 B, requests 911 → 611;
  blob publish unchanged (1,948.5 → 1,943.5 ms). C-both (2,132.6 ms /
  330.5 MB) matches B-group's wall while being 22.2 % smaller.
- **BF16 stage profile** (`bf16-plane-profile-20261112T054210Z`, embedding +
  8 × 8.7 MB MLP tensors, 5 reps, replica validated byte-for-byte against the
  codec): embedding split 307.31 ms; Huffman 337.61 (histogram 57.76, lengths
  0.023, codes 0.003, emission 266.55); hashes 27.07 + 17.84 + 27.06;
  `encode_float_tensor` 1,163.80 ms; frame assembly 446.90 ms; ratio 0.663.
- **Exactness and crash:** every configuration restored 101/101 tensors with
  SHA-256 equality in every repetition; every Weft configuration's three-phase
  process-fault proxy reopened old-or-new-whole (old 2 / new 1 / partial 0) with
  a full verify and 101 exact tensors. The `after_commit_file` tail is the one
  expected `verify` note, not a partial state.
- **Host contention, disclosed:** the host ran an external CPU/I/O-heavy
  process during the matrix, producing bimodal walls (2.8 s clean vs 26–43 s
  stalled). Raw samples, median and p95 are published; the ledger uses the
  least-contended repetition and records both walls. The store configurations'
  minima agree with the one-run contract court within 0–2 %; A's raw copy is
  more host-sensitive (326.4 vs 364.5 ms).
- **Losses:** the medians and p95 are not intrinsic costs; level-19 is pure cost
  on this fixture; mode 9 raises RSS by ~496 MB and slows restore; C-both is
  faster than B only because it publishes fewer bytes; `fsync` is not a
  separate phase; the stage profile is a harness replica, not codec-internal;
  the store head is not reproducible because `/tensors` uses a random ObjectId
  seed.
- **Gates:** Rust `cargo test --all-targets` 165 suites / 788 passed / 0 failed
  / 0 ignored; FUSE `--features fuse --all-targets` 165 / 831 passed / 0 failed
  / 2 pre-existing ignored; claims and spec drift updated in the docs commit.
  No `verify-all` run (not required for this subphase; its 45/45 receipt remains
  the cited aggregate and predates R17).
- **Not claimed:** no write-path optimisation, no format change, no Go parity
  for the R17 harness, no fleet/cloud/cold-media/power-loss claim, no claim that
  these proportions transfer to another corpus or backend.

## Subphase R18 (write-cost) — two-stage planner: analyse, select, materialise (2026-11-12)

**Status: implemented and measured.** The planned write no longer encodes every
candidate and discards the losers. Deliverable:
[`bench/RESULTS-planner-two-stage.md`](../bench/RESULTS-planner-two-stage.md).

- **Change.** `src/two_stage.rs` adds the three stages. **Analyse** compresses a
  bounded deterministic sample of whole codec chunks (4 at level 3, the first 2
  of those at level 19) and predicts the mode-9 frame length **exactly** from a
  full exponent histogram via the new WeftCodec function
  `modes::float_tensor_encoded_len_from_histogram` (WeftCodec `0aa3881`;
  `huffman::encoded_len[_from_weights]`, `modes::float_tensor_exponent_histogram`,
  `modes::float_tensor_encoded_len`). **Select** encodes a candidate only when
  its physical-size interval could change the winner or the mode-9 `1/64` gate;
  level 19 is decided *after* level 3 is measured, against an incumbent built
  only from measured or exact sizes, using the sampled 19/3 ratio applied to the
  measured level-3 frame. **Materialise** checks every measured size against its
  interval and re-plans exhaustively on a violation. `PlannerMode::{TwoStage
  (default), Exhaustive}` + `--planner` on `write`/`plan`; the write path is
  two-stage, `plan_tensor`/rebase stay exhaustive so every candidate's measured
  size stays visible; a strategy hint keeps its historical path.
- **Receipts.** `planner-two-stage-20261112T081151Z` (assembled, 15/15 checks),
  `…-matrix-20261112T081151Z` (7 interleaved reps × 11 configurations, per-tensor
  decision stream), three cross-family receipts (Qwen2.5-0.5B base, pythia-70m
  step0/step8), `augmentation-local-20261112T070000Z` (contract court, identity
  proof). Three aborted court invocations (`…T071300Z`, `…T073226Z`,
  `…T075100Z`) are retained: the runner rejected its own family fixtures
  (`pythia` has no `model.embed_tokens.weight`; a cross-family row asserted the
  Qwen-only R3 strategy counts).
- **Headline.** 7 interleaved repetitions, least-contended values: C two-stage
  **4,117.8 ms / 282,283,011 B**; C-legacy exhaustive 24,175.8 / 282,283,011;
  C-noz19 (exhaustive) 3,936.0 / 282,283,011; C-both 2,136.6 / 330,532,422;
  B 2,848.0 / 424,784,917; B-group 2,144.3 / 424,608,542; D 2,892.0;
  E 4,145.5 / 282,354,152; A 327.8; N 339.0. C is **5.87× faster** than the
  exhaustive planner with an **identical footprint** and **0 of 101** strategy or
  chosen-size changes; 101/101 exact restores and the three-phase crash proxy
  old-or-new-whole in every Weft configuration.
- **Decision diff.** Exhaustive → two-stage: level-3 encodes 101 → 51, level-19
  18 → **0**, mode-9 101 → 35, chosen bytes 281,951,111 → identical, footprint
  identical, 35/35 mode-9 predictions equal to their measured frames, 50
  level-3 skip bounds and 18 level-19 skip bounds recorded, 0 interval
  violations, 0 exhaustive fallbacks.
- **Cross-family.** Qwen2.5-0.5B base: 0 changes, 0 bytes. pythia-70m step0 and
  step8: 0 changes, 0 bytes, and **level 19 is encoded for 26 of 76 tensors
  where it is genuinely smaller** (163,863,698 vs 173,186,380 B at step0). Level
  19 was made **conditional**, not deleted: deleting it would have cost 5.4 % of
  that family's footprint.
- **The target was missed.** R18's stated expectation was `C ≤ ~3,850 ms`; the
  measured 4,117.8 ms is **7.0 % above** it and 1.23× the G1 rung. Measured
  cause: analysis costs **348.8 ms** (224.0 exact mode-9 histogram + 124.8 ZSTD
  sampling), while the 66 skipped mode-9 encodes were **93,696 bytes** in total
  (0.022 % of the fixture), so they were almost free. Two-stage C is therefore
  182 ms *slower* than the C-noz19 ablation. The remaining wall is 1,790 ms of
  mode-9 encode, 776 ms blob publish and 716 ms commit — R19 and R21 territory.
- **Corrections to R17.** `bench/RESULTS-write-cost-ledger.md` gains a §12 that
  corrects three derived statements: a predictor *is* needed to keep level 19 for
  other corpora; two-stage C is 182 ms slower than C-noz19 rather than at its
  level; and skipping the losing mode-9 encodes saves ~0 ms, not ~1.1 s.
- **Gates.** WeftCodec `cargo test --all-targets` (82 unit tests + every
  integration binary incl. the new `tests/float_tensor_len.rs` 5/5), 0 failed.
  Rust `cargo test --all-targets` **795 passed / 0 failed / 0 ignored, 166
  suites**; FUSE `cargo test --features fuse --all-targets` **838 passed / 0
  failed / 2 pre-existing ignored, 166 suites**; Go planner tests unchanged and
  passing at the pinned `WeftFS-Go` revision. Spec drift 60/60, conformance
  manifest and coverage 420 entries OK, `check-claims.sh` 985 entries OK. No
  `verify-all` run (not required for this subphase; its 45/45 receipt remains the
  cited aggregate and predates R18).
- **Not claimed.** No Go two-stage rule and no Go parity measurement (the
  `WeftFS-Go` pointer is frozen here; Go reaches the same decisions by exhaustive
  encoding and pays that cost); no power-loss, fleet, cloud or cold-media claim;
  the level-3 sampler cannot describe a heterogeneous tensor (the 272 MB
  embedding's interval degenerates and forces one safe extra encode); the 500 bps
  interval floor is calibrated on four real families only; `plan_tensor` and
  rebase remain exhaustive.

## Subphase R19 (write-cost) — mode-9 encoder: byte-identical, 2.85x faster (2026-11-12)

* **Parents.** `WeftFS` `c3f5e1a` (store wiring, bench, court scripts),
  `WeftCodec` `1412227` (fast encoder; `dfc60f4`). Both pushed on `staging`.
  No wire change: the frame, descriptor, canonical table, padding and every
  content root are untouched.
* **Where the time was.** R17's "frame assembly/other = 446.90 ms" was a
  mis-attribution caused by measuring stages in isolation. A faithful
  line-by-line replica of the pre-R19 encoder adds up to its wall (embedding:
  1,185.63 ms replica sum vs 1,187.99 ms wall) and shows the real split:
  generic plane split **705.8 ms**, Huffman emission **301.12 ms**, histogram
  57.71, plane concatenation 13.96, `payload_hash` 17.59, body copy 13.82,
  `encode_frame` copy + two CRC passes 46.62, `content_hash` 26.96. The
  pre-R19 encoder also took **9,814 minor page faults** per embedding encode
  because it held five ~272 MB mappings at once.
* **What changed** (all in WeftCodec, mirrored by the store):
  `split_bf16_planes` derives `(w >> 7)` and `((w >> 8) & 0x80) | (w & 0x7f)` over
  eight-byte windows into pre-sized planes **and** returns the four-bank
  exponent histogram from the same pass; `huffman::EncoderTable` packs
  `code | length << 32` per symbol and `huffman::BitStream` flushes four bytes
  at a time into a pre-sized slice (replacing `append_bits`, which is kept
  verbatim as the test oracle); `encode_float_tensor` plans the exact frame
  length, allocates **one** buffer, writes the descriptor and both planes into
  it (`frame::encode_frame_with_body`), hashes the stored region in place and
  appends the trailer CRC. `two_stage::Mode9Prediction` carries the analyser's
  exact histogram and the store calls `encode_float_tensor_with_histogram`.
* **Bounded parallel emission.** `FloatTensorEncodeOptions` splits the element
  range, prefix-sums the exact per-partition bit lengths, emits into disjoint
  byte ranges at per-partition bit offsets, spills one byte per partition into a
  one-byte scratch slot and ORs the shared bytes serially. Byte-identical to
  sequential at every thread count. Extra memory beyond the frame: one `u64`
  and one `u8` per partition. Bounds 1 MiB minimum bitstream, 512 MiB maximum,
  65,536 elements minimum per partition; the store defaults to
  `min(available_parallelism, 4)` and `--`/`WEFT_AUG_ENCODE_THREADS` can force 1.
* **Headline.** 7 interleaved repetitions, least-contended: **C 4,101.6 →
  2,328.4 ms (−43.2 %)** and **E 4,156.4 → 2,363.2 ms (−43.1 %)**, footprint
  **unchanged** (282,283,011 B and 282,354,152 B), 101/101 exact restores, same
  strategies and chosen sizes (`two_stage_matches_exhaustive`,
  `two_stage_storage_unchanged` true). A, C-nom9, C-both and D are within noise
  of their before values, which is the control that the delta is the encoder.
  CPU C 2,690 → 1,240 ms; peak RSS C 1,188,052 → 687,864 KiB (−42 %); cold
  restore unchanged (1,432.0 → 1,432.7 ms). C/A = 12.44x → 7.06x.
* **Encoder, store level.** The wall court's own phase profile (one
  attribution repetition, `configs.C.attribution_rep.ledger`) gives the
  in-place delta: **mode-9 encode 1,791.4 → 194.4 ms (9.2x)** and **analyse
  346.7 → 181.7 ms (1.91x)**, the second because the analyser's histogram is
  handed to the encoder and its kernel is now four-bank. Prepared total
  3,323.4 → 1,559.4 ms.
* **Encoder, codec level.** 9 tensors / 341,999,616 B, 7 reps: **262.28 →
  748.37 MB/s (2.85x)** sequential and **1,524.81 MB/s (5.81x)** at four
  emission threads.
  Embedding 272,269,312 B: 1,044.2 (reference) / 1,188.0 (replica wall) →
  363.87 ms, 177.24 ms at 4 threads, faults 9,814 → 644.
* **Byte identity.** `WeftCodec/tests/mode9_encoder_identity.rs` 6/6: every
  committed mode-9 vector re-encoded byte-exactly (shipped, reference,
  histogram hand-off, 2/3/4/8 threads); the specialised splitter vs the generic
  oracle over **every 16-bit word** and randomised planes; whole-frame
  differential over 5 dtypes × 30 sizes × 5 thread counts including a
  forced-parallel configuration with one element per partition; a
  300,000-element plane at 1..8 threads. The court compares shipped vs
  reference on 9 tensors × 7 reps, 0 mismatches. Conformance vectors and the
  Rust↔Go cross gate are unchanged.
* **Receipts.** `mode9-encoder-20261112T160000Z.json` (stage table, faults,
  allocation probe), `r19-wall-before-20261112T150000Z.json`,
  `r19-wall-after-20261112T140000Z.json`,
  `augmentation-local-20261112T140000Z.json` / `…T150000Z.json` (R3 contract
  court, after / before). Retained and **not** used: the first "official"
  attempt `augmentation-local-20261112T131500Z.json` (host write latency ~4x
  worse: A 6,275.9, C 17,950.7 ms) and the 1-rep/4-configuration probe
  `r19-diag.json`. Courts: `bench/scripts/mode9_encoder_court.sh` (inside the
  bench court), `bench/scripts/r19_wall_court.sh` (inside the bench court),
  `scripts/augmentation_court.sh --local` (host orchestrator).
* **Next bottleneck.** Publication is now 63.1 % of C's wall: blob publish
  769.2 ms + commit 700.4 ms (index segment 337.8, manifest commit 356.9).
  Encoding is 30.1 %. That is R20.
* **Gates.** WeftCodec `cargo test --all-targets`: all suites pass. Rust
  `cargo test --all-targets` and FUSE `cargo test --features fuse --all-targets`:
  pass, with the two pre-existing ignored FUSE tests. Conformance: Rust and Go
  adapters, all mode-9 vectors; cross gate Rust↔Go byte-identical.
  `check-claims.sh`, spec drift, conformance manifest/coverage: pass. No
  `verify-all` run for this subphase (not required); the 45/45 aggregate
  receipt remains the cited one and predates R19.
* **Not claimed.** No Go encoder change or Go performance claim (the Go codec
  already emitted these exact bytes and the format did not move); no AVX2 split
  kernel (measured headroom does not justify a second path); FP16/FP32/FP8 keep
  the generic splitter; no inter-tensor parallelism (R21), no shared pool, no
  power-loss, fleet, cloud or cold-media claim; one host, warm page cache.

## Subphase R20 (write-cost) — one atomic publication per checkpoint (2026-11-12)

* **Parents.** `WeftFS` `ad0ca3b` (ledger + deferred group durability + court).
  `WeftCodec` unchanged. Pushed on `staging`. **No wire change**: frame,
  manifest v1–v4, commit v1/v2, the WFX3 index and every conformance vector and
  fixture are byte-identical; the change is *how many* commit records and index
  segments a checkpoint writes, not what one contains.
* **The mechanism (product, not harness).** `Store::begin_group` now also opens
  a **deferred durability batch** (`Options::defer_group_blob_sync`, default
  on). Inside the batch, `LocalBackend::publish_file` writes and renames each
  blob and records `(file, directory)`; the barrier the commit path already
  performed before the commit record (`flush_buffered_blobs`) then `fsync`s
  **every recorded file** and **every touched directory once**
  (`StorageBackend::begin_deferred_sync` / `end_deferred_sync`, timed as the new
  nested `Phase::BackendSync`). Ordering is unchanged and enforced at the
  barrier: bytes durable → commit record → head. Every published file still
  gets its own data barrier; `commit_group` and `abort_group` both close the
  batch. `write_checkpoint`, `write_checkpoint_parallel` and the checkpoint
  service inherit it because they already publish through one group.
* **Measurement surface.** `IndexStats` gains `write_bytes`; new
  `Store::publication_ledger()` reports blob publications by kind (count and
  bytes), duplicate hits, commit records, superblock slots, index segments and
  — through `StorageBackend::durability_barriers()` — file data barriers and
  directory barriers separately (`None` for a provider-ack backend, which never
  claims `fsync`). Counted where each artifact is handed to the backend, so it
  closes against `BackendStats`. It is diagnostic only: no format, commit,
  recovery or receipt schema reads it, and no out-of-scope backend file needed
  a change.
* **Where the 202 blob publications go.** 186 `manifest_blobs` (one frame + one
  tensor-descriptor manifest per object) carrying 281,977,361 B, plus 16
  `object_blobs` (the 16 raw tensors' single 256-byte chunks) carrying 4,096 B.
  The descriptor manifests are ~1 KiB each; the frames carry the data.
* **Headline.** 7 interleaved repetitions, least-contended: **C 2,343.2 →
  1,236.8 ms (−47.2 %)** and its footprint **282,283,011 → 282,106,049 B
  (−176,962 B)**; 101/101 exact restores in every row and repetition; same
  strategies and same blob bytes (`two_stage_matches_exhaustive`,
  `two_stage_storage_unchanged`, `default_c_matches_official_court` all true).
  Target C ≤ 1,600 ms **met**; **G2 (≤ 1,344 ms) met** on the least-contended
  observation; C/A = 3.65x. Also measured (minima, this session):
  **B 2,849.9 → 900.2 ms**, **B-percommit**, **D 957.5**, **E 1,257.1**. The
  retained `…T133000Z` session measured the same minima within 1 % (C 1,225.4,
  C-percommit 2,341.6) with clean medians; it is not cited because that
  revision's tree is incomplete (see the final bullet).
* **Split of the win.** One group commit: **−718.1 ms and −176,962 B**. One
  deferred barrier: **−388.3 ms at identical bytes**. Blob publication was
  fsync-bound, not byte-bound: 778.5 → 272.2 ms for 282 MB (362 → 1,036 MB/s)
  plus a new 97.0 ms barrier (213 file + 161 directory syncs; 75.2 ms in the
  retained session — how much of the 282 MB the page cache had already written
  back). The control plane was 702.6 ms for 101 commits (342.5 index segments +
  360.1 commit records/superblocks, 7.0 ms per commit) and is now 8.3 ms.
  Footprint closure: −44,397 B fewer commit records + −132,565 B fewer index
  segments = −176,962 B exactly.
* **Options rejected, with their storage delta.** Packed blob plane at a
  64 KiB / 1 MiB / 8 MiB member bound: **+265.3 / +246.9 / +455.3 ms** and
  +14,651 / +16,956 / +95,794 bytes written, for −5,929 / −3,576 / +9,084 B of
  footprint (0.001 %). It publishes 120–195 catalog segments for 170–204
  members inside the commit's flush. Faster-and-larger and slower-and-larger
  both lose to C; the pack plane stays available and is not the default.
* **Crash and retention.** Three-phase process-fault proxy on **every** Weft
  row (group rows crash inside the group commit via `WEFT_CRASH_GROUP=1`):
  `after_objects` → old, `after_commit_file` → old, `after_superblock_write` →
  new; 0 tensor mismatches and 0 missing in all 30 verdicts;
  `crash_proxy_old_or_new: true`. The `after_commit_file` row keeps the
  pre-existing benign note about an unreferenced commit tail. Product-path
  evidence `bench/receipts/r20-checkpoint-verify-20261112T121955Z.log`:
  `weftfs checkpoint write` (12 `model.layers.0.*` tensors, 12 shard objects,
  29,824,768 element bytes) then **`verify --full` ok, 2 commits checked, 0
  problems**, `checkpoint verify` 12 tensors, `checkpoint restore --topology 1`
  29,826,096 B with SHA-256 `3a0f1570…` identical over two runs. Index GC, the
  per-root index rule and retention are untouched: `index_persistence` 7/7,
  `index_gc_bound` 3/3, `retention` 8/8, `gc_accounting` 3/3, `crash_proxy`
  3/3, `bounded_recovery` 3/3, `parallel_checkpoint` 7/7,
  `checkpoint_atomic`/`_packs`/`_restore_pipeline`/`_service`/`_streaming` all
  pass, plus the new `tests/publication_batch.rs` 6/6.
* **Receipts.** **Cited:** `bench/receipts/r20-publication-20261112T150000Z.json`
  (+ `.log`) and `augmentation-local-20261112T150000Z.json` (contract court,
  same revision and stamp). Retained and **not cited**:
  `r20-publication-20261112T133000Z.json.zst` and its contract court — same 11/11
  checks and the same minima within 1 %, but produced at `804fd38`, whose tree
  is incomplete: the barrier split was then carried by two new `BackendStats`
  fields whose zero-initialisers in `src/azure.rs`, `src/gcs.rs` and
  `src/s3.rs` were never committed, so that revision does not build from a
  clean checkout. The final design reports the split through
  `StorageBackend::durability_barriers()` and touches no out-of-scope file.
  Also retained: `r20-publication-20261112T120000Z.json.zst`
  (11/11 checks but the packed rows' crash proxy reopened without the pack
  plane — fixed; it is the receipt that found the harness bug), the matching
  `augmentation-local-…T120000Z.json`, and three single-repetition probes
  (`…T112000Z.json.zst`, `…T113000Z.json.zst`) whose `at_least_seven_reps` fails by construction
  and whose timings are not cited. Also retained: the log of the **failed**
  first attempt `r20-publication-20261112T105952Z.log` (no receipt produced),
  where the restore of a packed row opened the store without its pack plane. Court:
  `bench/scripts/r20_publication_court.sh` inside the bench court plus
  `WEFT_R20_*` forwarding in `scripts/docker-court.sh`.
* **Gates.** Rust `cargo test --all-targets`: **801 passed / 0 failed / 0
  ignored, 168 suites**. FUSE `cargo test --features fuse --all-targets`:
  **844 passed / 0 failed** (the two pre-existing ignored tests).
  `check-claims.sh` 1020 entries OK, spec drift 60/60, conformance manifest and
  coverage 420 entries OK. No `verify-all` run for this subphase; its 45/45
  receipt remains the cited aggregate and predates R20. The R20 court is
  standalone, like the R19 wall court: `verify-all` keeps the reduced
  `augmentation_local` contract court and does not carry the 14-row
  7-repetition matrix.
* **Next bottleneck.** Encode 704.6 ms (57.0 %; ZSTD-3 286.6 + mode-9 192.8 +
  analyse 184.4), blob publication 272.2 ms (22.0 %), barrier 97.0 ms (7.8 %),
  digest 67.0 ms (5.4 %), source read + init 52.2 ms (4.2 %), commit + index
  9.0 ms (0.7 %), residual 13.4 ms (1.1 %). R21 is the bounded parallel
  pipeline.
* **Not claimed.** No Go change or Go parity for the deferred barrier (the Go
  store has its own group commit and is untouched here); no pack+deferred
  combination (the packed plane refuses deferred sync, so that option is not
  measured); no read-path improvement (cold restore median 1,604.6 → 1,511.5
  ms, a difference that is host noise, not a read-path change); `RemoteObject`/S3/Azure/GCS durability is a provider ack and is
  not affected; no media, power-loss, fleet, cloud or multi-writer claim; one
  host, warm page cache; the `C-legacy`/`C-nom9` rows remain ~21.5 s controls
  because the exhaustive planner encodes level-19 even when it selects nothing.

## Subphase R21 (write-cost) — bounded parallel encode-and-publish pipeline (2026-11-12)

**Status:** implemented, measured, tested. Rust store, harness and court on
`staging`; WeftCodec and WeftFS-Go are unchanged and **no frame, manifest,
commit, index or identity byte changed** — every row of the matrix publishes
the same 282,106,049 B fixture, and the committed state is identical for
1/2/4/8 workers (`tests/tensor_pipeline.rs`).

* **What moved.** The planned writer went from **1,206.2 ms to 623.3 ms**
  (−582.9 ms, −48.3 %) at a byte-identical footprint, 101/101 exact restores
  and the same chosen representations. **G3 (≤ 672 ms) is met** on the
  least-contended repetition *and* on the median (653.9 ms); the best measured
  configuration (`--pipeline-cpu-tokens 3`) is 593.9 ms and the protocol row E
  goes 1,259.5 → 663.0 ms. Restore is unchanged (1,406.0 vs 1,413.1 ms, host
  noise); R21 is a write-path change with no read claim.
* **The design.** `Store::encode_tensor_object` (planner + candidates + logical
  root + content hash, publishing nothing) and
  `Store::publish_encoded_tensor` (the chosen frame or raw chunk table plus the
  descriptor manifest) split the prepare half into a pure CPU stage and a pure
  I/O stage. `Store::prepare_tensor_group_pipelined` (`src/tensor_pipeline.rs`)
  runs reader → N encoder workers → one publisher behind bounded queues with
  **byte** reservations (`staging_bytes` 256 MiB, `worker_bytes` 1 GiB,
  `encode_bytes` 256 MiB, `publish_bytes` 128 MiB), a CPU-token budget that
  caps busy encoder threads, FIFO admission and oversize admission. A worker's
  reservation is `3·n + 1 MiB` (decoded bytes + the frame being built + the
  single frame `retain_candidate` keeps), so the state bound is
  **1,744,830,464 B** and the measured peak is 921,388 KiB (54 % of the bound,
  30 % of the court). Nothing is committed by the pipeline: the caller installs
  the fragments inside its own group, so the control plane stays on the single
  writer (one group commit, one head publication, one index segment).
* **Concurrent candidates.** `TensorPlanOptions::parallel_candidates` encodes
  level 3 and mode 9 on two threads for one tensor *only* when the R18 decision
  stream already guarantees both run, and consumes them in the historical
  order, so no representation decision or frame byte changes. The 272 MB
  embedding's encode falls 529.4 → 448.8 ms at four workers (→ 412.8 ms with
  three CPU tokens): `max(295.0, 121.6)` instead of `295.0 + 121.6`.
* **Overlap is measured, not asserted.** Per-tensor stage intervals are
  recorded by the owning thread; at four workers **90.1 % of encoder-thread
  time overlapped publication** and **89.9 % of publication time was hidden
  under encoding**. Even one worker overlaps (32.7 % / 57.7 %, wall 1,114.1 ms
  vs 1,206.2 sequential) because the reader, the encoder and the publisher are
  separate stages.
* **Early barrier.** The publisher drains the open deferred batch's *file* list
  every 8 MiB and a writeback thread `fsync`s those files outside the backend
  lock (`take_pending_writeback_files` / `note_external_file_barriers`; no
  `unsafe`, counters stay exact). 202 files, 9 passes, 502.3 ms of thread time:
  the commit barrier falls **329.6 ms → 1.3 ms**. The batch stays open, the
  commit path still `fsync`s every remaining file and every directory before
  the commit record, and the pipeline joins the writeback thread before it
  returns, so a drained file is durable or the write fails with nothing
  committed. The ablation `sync0`/`nosync` (no early barrier) costs 892.2 ms:
  the same writeback on the critical path.
* **Scaling** (7 interleaved reps, min/median): 1 worker 1,114.1/1,124.1,
  2 workers 986.8/1,016.0, 4 workers **623.3/653.9**, 8 workers 612.1/664.8.
  Speedup 1→4 is 1.71× of an ideal 4×: at 2, 4 and 8 workers the encode wall
  *equals the embedding's own encode* (448.8 ms), so inter-tensor parallelism is
  exhausted and the largest tensor is the Amdahl cap exactly as R20 predicted.
  Two workers is publisher-bound (a single publisher cannot publish 282 MB
  faster than ~430 ms); eight buys nothing inside host noise.
* **Determinism.** `tests/tensor_pipeline.rs` (6/6) requires an identical store
  head, per-object manifest ids and content roots, and manifest blob bytes
  across 1/2/4/8 workers, with `verify --full` on each store; candidate
  parallelism off vs on for every tensor is also byte-identical. Court identity:
  `default_c_matches_official_court`, `strategies_stable_across_reps` and
  `two_stage_storage_unchanged` all true. Crash proxy `true` on every row
  (three fault points, grouped rows). The court also writes a store through the
  pipeline and runs the shipped CLI `verify --full`: ok, 0 problems.
* **Losses, published.** Peak RSS 687 → 921 MiB (+34 %); CPU 1,180 → 1,320 ms
  (+11.9 %); `nosync` 892.2 ms; two workers publisher-bound 986.8 ms; eight
  workers no better than four; 8 MiB is the measured sync granularity (16 MiB
  638.4, 32 MiB 676.9, 64 MiB 712.6, none 892.2); the default `cpu_tokens` is
  `encode_threads`, not the measured optimum (`--pipeline-cpu-tokens 3` is
  593.9 ms); `prepare_phases` for a pipeline row is the whole-pipeline delta
  (the phase collector is thread-additive, so a phase total can exceed the
  wall); `E-pipe4`'s footprint is 6 B above `E`'s (directory accounting for the
  protocol's snapshot/context objects; every tensor object id, manifest id and
  content blob is identical); `C-legacy` remains 21,652.9 ms as the
  decision-diff control; single host, warm cache, local POSIX backend, no Go
  parity and no power-loss, fleet, cloud or multi-writer claim.
* **Receipts.** `bench/receipts/r21-pipeline-20261112T201500Z.json` (13 rows ×
  7 reps, 11/11 checks, `result: pass`) and the same-session contract court
  `augmentation-local-20261112T201500Z.json` (`result: pass`). Retained:
  `r21-pipeline-20261112T193000Z.pre-encode-counts-fix.json` (all timing checks
  passed; `ablation_encode_counts_exact` failed because the pipeline branch did
  not fill `prepare_phases`), `r21-pipeline-20261112T190000Z.json` (the earlier
  13-row matrix at `209858e`, C-pipe4 621.2 ms), and the two single-repetition
  tuning probes `r21-pipeline-smoke3/4.json`. Analysis in
  [`bench/RESULTS-parallel-pipeline.md`](../bench/RESULTS-parallel-pipeline.md).
  Court: `bench/scripts/r21_pipeline_court.sh` inside the bench court plus
  `WEFT_R21_*` forwarding in `scripts/docker-court.sh`.
* **Gates.** Rust `cargo test --all-targets`: **807 passed / 0 failed / 0
  ignored, 169 suites**. FUSE `cargo test --features fuse --all-targets`:
  **850 passed / 0 failed** (the two pre-existing ignored tests).
  `check-claims.sh` 1,030 entries OK, spec drift 60/60, conformance manifest and
  coverage 420 entries OK. No `verify-all` run for this subphase; its 45/45
  receipt remains the cited aggregate and predates R21. The R21 court is
  standalone, like the R19/R20 courts: `verify-all` keeps the reduced
  `augmentation_local` contract court and does not carry the 13-row
  7-repetition matrix.
* **Next bottleneck.** Of the 623.3 ms, **448.8–468.1 ms (72–75 %) is one
  tensor's own encode** (the 272 MB embedding, repetition 1 at four and eight
  workers), of which level 3 is ~295–337 ms and mode 9 ~122–227 ms (already
  overlapped). Going from four to eight workers buys 11.2 ms on the minimum
  (623.3 → 612.1) and nothing on the median, so inter-tensor parallelism is
  exhausted. `weftcodec::modes::encode_zstd` with a codec chunk size emits one
  independent ZSTD frame per chunk under the CHUNKED flag (4,155 frames for the
  embedding), so R22 should parallelise the *chunk* encodes of one tensor — a
  byte-identical change with ~3× headroom on level 3, which projects a ~430 ms
  write (1.29× A) without touching a single representation decision. After
  that: fuse `analyse` with the encodes that re-read the same bytes (keeping
  the exact mode-9 prediction), and ship a measured CPU-token default.

## Subphase R22 (write frontier) — chunk-parallel ZSTD, exact analyse, measured defaults (2026-11-12)

**Headline.** The shipped write is **592.6 ms least-contended / 600.9 ms
median** for the 101-tensor Qwen2.5-0.5B-Instruct subset (424,439,296 logical
bytes) at **282,106,049 B**, byte-identical to R18-R21, with 101/101 exact
restores and the crash proxy old-or-new-whole. That is **G3 (<= 672 ms) on both
statistics, 1.78x the native control A = 332.0 ms**. The final rung (<= 336 ms)
is **not reached**, and section 7 of the results document says exactly why: the
write is now durability-bound, not encode-bound.

* **22.1 chunk-parallel ZSTD - implemented, proved, and not the wall win.**
  `weftcodec::modes::encode_zstd_with` + `ZstdEncodeOptions` encode a CHUNKED
  frame's independent per-chunk frames on a bounded pool in ordered
  byte-bounded batches, each worker reusing one zstd context. Measured on the
  272,269,312 B embedding (64 KiB chunks, 4,155 chunks): level 3 **280.3 ->
  189.0 (2 threads) -> 135.7 ms (4)** and level 19 **13,597.4 -> 3,667.1 ms (4)**
  with **every timed frame byte-identical** (40 checks in the receipt; 2,800
  boundary comparisons, 2,000 randomised cases and the fallback paths in
  `WeftCodec/tests/zstd_chunk_parallel.rs`, 5/5). It does **not** move the frozen
  fixture's wall: the pipeline is 552 ms of which publication is 452.5 ms and
  the durability writeback 461.6 ms for 202 files. The plan's "~430 ms"
  projection is **refuted** at this operating point and the refutation is the
  recorded result.
* **22.2 exact parallel analyse.** The analyse stage's only full-tensor pass is
  the mode-9 exponent histogram (37.1 ms, 7.35 GB/s); it cannot be fused into an
  encode because it decides whether that encode runs. It is now accumulated on
  threads in whole-element windows and summed - exact, so the R18 bit-exact
  mode-9 prediction is unchanged (`WeftCodec/tests/histogram_parallel.rs` 3/3,
  `tests/two_stage_planner.rs` (7 tests) still green, court re-proof
  `two_stage_matches_exhaustive` / `two_stage_storage_unchanged` both true). The
  measured wall effect is **nil** (`an1` control 586.6/614.7 against 592.6/600.9)
  and the dominant analyse cost turns out to be the **level-19 sample**
  (156.9 ms against 31.6 ms with level 19 disabled).
* **22.3 measured defaults, both shipped.** `default_cpu_tokens(n) = n - 1`
  (592.6/600.9 at 3 tokens against 612.1/644.0 at 4; 1 and 2 tokens are
  912.8/959.8 ms because the single publisher binds), and
  `default_zstd_encode_threads() = min(available, 4)` with the rule that the
  chunk pool is used **only when the tensor's encode has the CPUs to itself**
  (no concurrent mode-9 candidate). The rule is what keeps the frozen fixture
  neutral while pythia-70m is 1.50x and Qwen 3B 1.18x faster.
* **22.4 the frontier** (25 rows x 7 interleaved reps, device control 3,200 MB/s
  `clean`, 11/11 checks, receipt `r22-pareto-20261112T230000Z`): Pareto-optimal
  set = **A** (332.0 ms / 424,439,296 B), **C-pipe4 shipped** (592.6 ms /
  282,106,049 B), **C-pipe4-zpar4** (584.8 ms, same bytes, not adopted),
  **t19off** (575.9 ms, same bytes, *not portable* - level 19 is where pythia is
  won) and **tm9off** (593.6 ms at **330,355,860 B**, +17.1 %). Everything else
  is dominated. Measured losses: the pack plane **third rejection** (962.7 ms;
  publisher busy 391 -> 220 ms but sealing costs more), `znochunk` **3,518.1 ms**
  (the two-stage sampler's unit *is* the chunk: 50 prediction violations, 50
  exhaustive fallbacks, level 19 actually runs, for 2.4 KB), `leeway4`
  **2,552.1 ms** for zero bytes, 16 KiB chunks 13 % worse, `sync16/32` equal to
  8 MiB, 8 workers worse than 4 (648.7 against 592.6), and no statistic
  separating the 576-612 ms cluster (the default's median moved 13 % between
  sessions while its minimum moved 2.5 %).
* **22.5 cross-family and MinIO.** Qwen2.5-0.5B-Instruct: C 1,217.6 -> pipeline
  **628.4 ms**, 282,106,049 B (33.5 % below A = 346.6), 35 mode-9 / 50 zstd / 16
  raw. pythia-70m step 0: 12,204.4 -> **8,143.8 ms**, 163,964,176 B (**41.8 %**
  below A = 240.4), all 76 tensors zstd, and the exhaustive planner produces the
  **same footprint** - level 19 still wins where it used to. pythia step 8:
  12,176.8 -> 7,512.6 ms, 163,830,373 B. Qwen2.5-3B-Instruct (635 MB subset):
  1,554.3 -> 1,319.7 ms at 421,550,249 B, peak RSS 1,980,292 KiB (63 % of the
  court). MinIO (real server, one loopback process, tmpfs): A 842.9 ms / 101
  writes; **C 1,275.4 ms / 225 writes / 282,132,555 B written / 33.53 %
  reduction**; 8/8 checks. **Left open:** Qwen2.5-3B base, Qwen2.5-7B, and the
  R22 *pipeline* over S3 (the S3 arm measures the planned writer).
* **Gates.** Rust `cargo test --all-targets`: **807 passed / 0 failed / 0
  ignored (170 binaries)**. FUSE `cargo test --features fuse --all-targets`:
  **850 passed / 0 failed / 2 ignored**. WeftCodec `cargo test --all-targets`:
  **290 passed / 0 failed / 0 ignored** (45 binaries, including
  `zstd_chunk_parallel` 5/5 and `histogram_parallel` 3/3); Rust-Go cross gate
  `all checks passed` (41 frames
  each way, every mode fixture byte-identical); `verify --full` ok on a
  chunk-parallel-written store; crash proxy old-or-new on every row. There is
  no verify-all aggregate for this subphase: `verify_receipt_current` still
  cites `verify-20261111T182601Z` (45/45).
* **Environment, disclosed.** Between the clean runs the host's durability path
  degraded from 1.28 GB/s to 125-180 MB/s and the native control A wrote the
  424 MB fixture in **11,482.6 ms**; those attempts produced no cited row
  (`r22-probe-20261112T163717Z` and the superseded pareto receipts are retained
  as the record). The cited courts carry a `dd conv=fsync` device control and a
  native-control band check, and the sweep's own 25-row matrix re-proves the
  footprint on every row.
* **Analysis**: [`bench/RESULTS-pareto.md`](../bench/RESULTS-pareto.md).
  Courts: `bench/scripts/r22_pareto_court.sh`, `r22_family_court.sh`,
  `r22_probe_court.sh` + `r22_probe.sh`; `WEFT_R22_*` forwarding in
  `scripts/docker-court.sh`. The R22 courts are standalone, like R20/R21.

- 2026-11-12 (R12c item 4, bounded checkpoint queue with admission control):
  parent `staging` `1d71663`; full analysis
  [`bench/RESULTS-checkpoint-queue.md`](../bench/RESULTS-checkpoint-queue.md),
  receipts `checkpoint-queue-bound-after-20261112T043000Z.json`,
  `checkpoint-queue-overload-baseline-20261112T020000Z.json`,
  `checkpoint-queue-sweep-baseline-20261112T020000Z.json`.
  * **The pre-change behaviour was measured, not assumed.** With two callers
    that do not wait, a save raised
    `ServiceError: InvalidArgument: a deferred checkpoint is already open on
    this connection` and **which** save survived was decided by timing: 2 of 4
    landed, 2 of 8 landed, and `save_checkpoint_async` grew the process from 2
    to 19 threads because it creates a pool per call. A caller that waits
    (the training harness) simply blocked: the maximum wait for the previous
    save was 1,688.2 ms (`weft`) and 82.3 ms (`weft-session`) at a 1-step
    interval. Nothing was ever silently lost, and `landed == store
    checkpoints` in every run.
  * **Shipped:** `CheckpointSession` admits saves under a documented bound —
    `max_outstanding` (default **2**), a staged-byte budget (1 GiB), and
    `admission_policy` `block` (default, 30 s ceiling) or `fail`. A refused
    save publishes nothing and raises `QueueFull` naming every bound;
    cancellation is only possible before a save starts (`SaveNotStarted`); a
    closed or draining session raises `QueueClosed`. `CheckpointSession.close`
    drains and reports `weft.checkpoint.queue.drain.v1`
    (`no_silent_skips`, admitted/completed/failed/refused, queue high-water,
    staged-byte high-water).
  * **Measured bound sweep** (96 steps, 2 reps, 1 and 4-step intervals, same
    169-entry fixture and court): at a 1-step interval the loop wall falls
    **7,415.3 → 5,761.7 ms (−22.3 %)** at bound 2; 4 and 8 are within 2 % and
    worse in the tail. Every cell: 0 refused, 0 failed, 0 cancelled,
    `completed == submitted`, `outstanding 0`, `pending 0`,
    `no_silent_skips` true, `resume_exact` equal to the requested
    resumptions (97/97 and 25/25-style per cell). At a 4-step interval the loop
    is save-bound and no bound helps.
  * **Losses, published:** completion p95 gets *worse* than the serialized
    baseline (47.7 → 71.5 ms at bound 1, 85.9 ms at bound 2) because more
    saves are in flight; peak RSS rises 12–19 %; a deeper queue buys nothing;
    the queue cannot make a save finish sooner. Tests:
    `python/tests/test_checkpoint_queue.py` covers refusal, cancellation
    before start, drain, and the writer fence; the first two court attempts
    (bounded `/tmp` full; a `TypeError` on an empty clean-step sample) are
    retained as `…T000000Z.tmpfs-full.log` and `…T001500Z.log`.
  * Not claimed: multi-writer training, a queue across processes, GPU, fleet.

- 2026-11-12 (R14d, Azure MW2 in Go — the failing test, its cause, and the fix):
  WeftFS-Go `staging` `19615ea` (parent pin `46e8b98`); reproduction log
  `bench/receipts/r14d-repro-20261112T230000Z.log`.
  * **Reproduced** in the go court at WeftFS-Go `a7170f5`:
    `--- FAIL: TestAzureOptimisticTwoDisjointCommitters (0.01s)` /
    `azure_integration_test.go:406: verify: ok=false err=<nil>
    problems=[{UnsupportedVersion recovery failed: UnsupportedVersion: the
    superblock cache names generation 3 but the writer lock record attests
    generation 1; this store was written by a pre-v3 writer after the lock
    record was upgraded}]`. Only that test failed.
  * **Cause.** Not the Azure client, not the fake, not the test. Go recovery
    treated the writer `LOCK` record as the head authority on every path, and
    only two call sites (`openWithStrategy`'s optimistic branch and
    `reloadFromPlane`) wrapped the commit source in `headAttestHider`. `Verify`
    and a read-only open passed the raw commit source, so an optimistic (MW2)
    store whose `LOCK` still attested the single-writer init's generation 1 was
    refused against its superblock generation 3. Rust already gates the same
    check on `WriterMode::policy` inside `recover_inner_via`
    (`src/recovery.rs:815-853`), which is why the Rust half passed its own
    Azure suite.
  * **Fix (`19615ea`).** `commitSource` gained `ReadRecord`;
    `recoverWithStrategy` reads the `writers` policy record itself, skips the
    attested-head reconciliation when the mode is `optimistic`, and fails closed
    on a record that exists but cannot be decoded. `headAttestHider` and its two
    call sites are gone, so there is exactly one authority decision for open,
    reload and verify. Tests: the Azure two-committer test now also reopens
    read-only and verifies (that path failed before the commit), and
    `TestMalformedWritersPolicyFailsClosed` pins the fail-closed default.
  * **Evidence.** gofmt/vet/`CGO_ENABLED=0` build clean; `go test -race
    ./...` all packages pass in the fuse court (`store` 210.4 s, `cmd/weftfs`
    40.8 s, `fusefs` 7.2 s). Rust's Azure suite was re-run unchanged:
    `cargo test --test azure_backend` 22 passed / 0 failed, including
    `two_optimistic_committers_publish_disjoint_files_on_azure`.
  * **Submodule pointer.** The parent recorded `146b517` while the worktree was
    two commits ahead; `46e8b98` moves the pin to `19615ea`, whose suite is
    green. The earlier WIP commit message stays as written — history is not
    rewritten — and this entry records the verification it did not have.

- 2026-11-12 (R24, generated performance numbers; R14d re-verified at the pin):
  parent `staging` `9a0118e`. Submodules unchanged.
  * **Every current performance number is generated from a receipt.**
    `scripts/perf_numbers.py` reads `docs/perf-numbers.json` and rewrites only
    the text between `BEGIN GENERATED` / `END GENERATED` sentinels; 147 numbers
    are pinned to a receipt path or to an arithmetic expression over other
    pinned numbers, and `docs/PERF-NUMBERS.md` is the canonical table (value,
    unit, receipt, stamp, revision). `--check` fails closed on a stale region, a
    missing receipt, a declared stamp/revision that the receipt no longer
    carries, a manifest number no block prints, or a sentinel with no manifest
    block; the `perf_numbers` court runs it in `scripts/verify-all.sh` (46
    courts from this commit). README, the STATUS head, PAPER, RFC and the
    v1.4.0 note read from the generated regions. Operator directive 12 records
    the rule.
  * **Audit corrections** (each was a real stale or wrong number): the
    independent per-tensor `zstd -19` baseline `1,535,945,154 B` appears in no
    receipt — the sourced value is `1,528,909,007 B` (16.5 %, not 17 %); the R17
    citation named `write-cost-ablation-20261112T050102Z`, whose C wall is
    24,132.9 ms, while the quoted 24,229.2 ms is the `052203Z` receipt;
    "peak RSS 939,256 KiB (34 % of the court)" is 29.9 % of 3 GiB; the 100-row
    tile speed-up is 338.5×, not 337×.
  * **Pareto terminology corrected.** The shipped default is **not** on the
    fixture numerical frontier: the non-dominated set on (least-contended wall,
    footprint) is computed from the receipt and is `A`, `C-pipe4-t19off`,
    `C-pipe4-pack1m`; the portable/eligible set excludes the level-19-off row
    and is `A`, `C-pipe4-sync16`, `C-pipe4-pack1m`. `C-pipe4` is dominated on the
    point estimates and sits in a band the measurement cannot rank, so the
    "Pareto-optimal"/"knee" wording is withdrawn. Generated by a `frontier`
    block, not asserted by hand.
  * **R14d is verified, not deferred.** The whole Go suite passes under `-race`
    in the fuse court at the pinned WeftFS-Go `1fa3d32`: `cmd/weftfs` (40.9 s),
    `fusefs` (7.2 s), `index`, `model`, `store` (134.0 s) all `ok`, 0 failures,
    `go vet` clean, `CGO_ENABLED=0` build clean, and
    `TestAzureOptimisticTwoDisjointCommitters` PASS. Receipt
    `bench/receipts/r14d-go-suite-20261112T221649Z.json` (+ the two raw logs)
    also records the honest limitation of the minimal `go` court: its `./...`
    run fails the 17 `fusefs` mount tests because the image has no
    `/bin/fusermount`; the `store` package itself passed there. R14d stays on
    `staging`; no `r14d/azure-mw2` branch is needed.
  * Not claimed: no new measurement, no wire change, no aggregate at this
    commit yet — the v1.4.0 release run produces one.

- 2026-11-12 (v1.4.0 release, R24): merged and tagged.
  * `main` merge commit `50c8b21c543332f8a038c4035a40981421cd6cec`
    ("Merge staging: v1.4.0"); annotated tag **`v1.4.0`** at that commit,
    pushed to `origin/main` and `origin` (`e067e72..50c8b21`). Submodule pins:
    WeftCodec `58db5aa`, WeftCodec-Go `9812d15`, WeftFS-Go `1fa3d32`. This entry
    and the `tag.target` field in the artifacts receipt are written **after** the
    tag because a file cannot contain the hash of the commit that contains it;
    no code, format or generated number changed in this follow-up.
  * Post-tag docs corrections, disclosed rather than rewritten into the tag: the
    release badge links to `releases/latest` instead of a hard-coded `v1.3.0`,
    and the README "Demonstrated results" family/region bullets are generated
    (the pythia `--patch-from` bullet stays a labelled historical quotation).
    Both are docs only; every generated region is re-checked by
    `perf_numbers --check`, and no settled number in the tagged tree changed.
  * Aggregate cited by the release: `bench/receipts/verify-20261112T223540Z.json`
    — **46/46** courts pass, 0 failed, 0 skipped, at the release-candidate tree
    `0461132`, which is the tree of the tag except for docs, claims, receipts and
    the generated-number re-pin. `scripts/check-claims.sh`,
    `scripts/perf_numbers.py --check`, spec drift and the conformance
    manifest/coverage gates were re-run on the final tree and pass.
  * Artifacts (`bench/receipts/release-artifacts-20261112T233126Z.json`):
    `weftcodec-1.4.0-py3-none-any.whl` 115,851 B sha256 `9b6ff9bb…64928`,
    `weftcodec-1.4.0.tar.gz` 156,977 B sha256 `cc86abd9…388295`, both twine
    checked in the python court. `cargo publish --dry-run` passes for
    `weftcodec 1.4.0`; the `weftfs 1.4.0` dry run fails to resolve
    `weftcodec = "^1.4.0"` because 1.4.0 is not on crates.io yet — recorded,
    not hidden, with the publish-order fix.
  * **Nothing was published to a registry.** `.docker-cache/.pypi-token` does
    not exist (so no PyPI upload and the 1.4.0 Python description was not
    rebuilt); `~/.cargo/credentials.toml` holds a crates.io `[registry]` token
    (57 bytes). Registry steps are the operator's:
    `scripts/publish-crates.sh` (weftcodec first), then
    `scripts/publish-pypi.sh` once a token is present. GHCR images come from the
    per-repo tag workflows.

- 2026-11-13 (R25 / v1.4.1): the augmentation numbers re-measured, rule 13
  enforced, and the stale prose corrected. Parent `staging` `1af4218` at the
  aggregate; submodules WeftCodec `5104fa0` (1.4.1), WeftFS-Go `725229d` (Go
  provenance tool version 1.4.1), WeftCodec-Go `9812d15` (unchanged).
  * **Contract court.** The frozen `bench/CONTRACTS-augmentation.md` method now
    runs seven interleaved, one-position-rotated repetitions, and every
    `configurations.<row>.reproduction` records `shipped_default`,
    `requires_flag` and `reproduces_with`. Only A (the backend's own path) and
    C (`weftfs checkpoint write`, no flags) are shipped defaults; B, D, E and N
    need flags or harness composition. Receipts:
    `augmentation-local-20261113T004000Z.json` (local, A/N/B/C/D/E),
    `augmentation-minio-20261113T010500Z.json` (MinIO A and C),
    `augmentation-local-20261113T011500Z.json` (pythia-70m step 0),
    `…T012000Z.json` (step 8) and `…T013000Z.json` (Qwen2.5-3B-Instruct). The
    local host was shared and several cells contain multi-second durability
    stalls; every raw sample is published and the least-contended repetition is
    used for attribution (contract §6.10), with the median and p95 unchanged.
  * **Shipped default vs frontier.** The shipped planned write re-measures at
    974.1 ms least-contended / 1,175.3 ms p50 at 282,106,049 B (2.98x native A,
    33.53 % fewer bytes) in `augmentation-local-20261113T004000Z.json`. The
    R22 `C-pipe4` row (592.6 ms, 1.78x) drives
    `Store::prepare_tensor_group_pipelined`, which **no shipped CLI verb
    reaches**; it is published as a separate frontier row, never as the default.
    The <= 336 ms rung is not reached. The cross-family rows are a real loss on
    pythia-70m (roughly 51-53x native); the 3B row is a nine-tensor 626 MB
    subset, not the whole model.
  * **Rule 13.** Every write result is one backend with and without the
    augmentation; the reference filesystem is not a comparator. The existing
    ext4/XFS/ZFS reference-filesystem courts
    (`bench-fs-cold-*`, `RESULTS-comparative-v1.md`, the STATUS entries and the
    README F9 row) are relabelled **provenance only and excluded from every
    claim**. No doc in the tree now headlines a reference-filesystem
    comparison.
  * **Generated numbers.** `docs/perf-numbers.json` gains the `contract`,
    `frontier`, `family` and extended `ladder` groups (316 numbers, 62 blocks).
    The paper, README current-result block, STATUS head, RFC §6, PERF-NUMBERS,
    `python/README.md` and `RELEASE-v1.4.1.md` read from the new receipts;
    `perf_numbers.py --check` is green. The v1.4.0 release note keeps its own
    receipt through `verify.v140`/`tests.v140` numbers.
  * **Paper.** The abstract leads with the shipped 2.98x / 33.53 % result and
    states that native parity is not reached; the R22 frontier is named
    separately. Results are the R3 -> R17 -> R18 -> R19 -> R20 -> R21 -> R22 ->
    R25 ladder plus the cross-family table, each rung from its own receipt.
    Threats now include one host, warm cache, tmpfs, MinIO loopback, the pythia
    loss, the unreached 336 ms rung, and the fact that the frontier is a
    numerical claim about one fixture.
  * **Prose corrections.** `python/README.md` previously said PyPI served 1.2.0
    and that 1.4.0 was not uploaded. 1.4.0 is live (wheel
    `41ad31b4…d3c6`, 115,851 B; sdist `2fd174fc…e69c0`, 156,858 B, read from
    `https://pypi.org/pypi/weftcodec/json` on 2026-11-13). crates.io serves
    `weftcodec 1.4.0` (`7f24d383…56882`) and `weftfs 1.4.0`
    (`d94ff6e7…06be0`); GHCR serves `v1.4.0` (digests recorded in
    `bench/receipts/release-artifacts-20261113T011500Z.json`). The v1.4.0 note's
    "Nothing was uploaded" paragraph is corrected. `scripts/publish-pypi.sh`
    now stages a private 0600 copy and deletes only that copy; an
    operator-supplied `WEFT_PYPI_TOKEN_FILE` is never deleted, and `PYPI_TOKEN`
    is accepted directly.
  * **Aggregate.** `bench/receipts/verify-20261113T022751Z.json`: **46/46**
    courts pass, 0 failed, 0 skipped at WeftFS `e67e63186456`. Test counts:
    rust_store 807, rust_fuse 850 passed / 0 failed / 2 pre-existing ignored,
    rust_codec 290. Claims 1,046; spec drift 60/60; conformance 420.
  * **Release.** Versions 1.4.1 in `Cargo.toml`, `WeftCodec/Cargo.toml`,
    `python/pyproject.toml`, `python/weftcodec/__init__.py` and the Go
    provenance tool version. Artifacts built in the python court and
    `twine check`-clean: wheel `74779b5c…b176` (115,540 B), sdist
    `9fa6cfb6…79a0` (156,293 B). `cargo publish --dry-run` passes for
    `weftcodec 1.4.1` (528 files); the `weftfs` dry run is blocked on publish
    order as in 1.4.0. PyPI upload needs a token; none was available to this
    session, and the exact blocker is recorded rather than papered over.

- 2026-11-13 (v1.4.1 release): merged and tagged in all four repositories.
  WeftFS `abe4d989bb1e` (tag `v1.4.1`), WeftCodec `60f32fce1bb9`,
  WeftCodec-Go `20c320c91056`, WeftFS-Go `67fb2fc7a690`; all four `staging`
  and `main` trees pushed, all four tags pushed (they drive GHCR), and four
  GitHub releases created. The WeftFS release carries the 1.4.1 wheel and
  sdist.
  * crates.io: `weftcodec 1.4.1` checksum `a83e08ef…4283f` (3,020,483 B) and
    `weftfs 1.4.1` checksum `85870e61…ad73c` (1,850,794 B), both yanked false,
    published in order (weftcodec then weftfs).
  * GHCR: `:v1.4.1` and `:latest` verified for all four images and identical
    per image — `weftfs` `717683c1…f1f46`, `weftcodec` `4c6f8586…ee886`,
    `weftcodec-go` `80239779…e592f`, `weftfs-go` `b36e36d6…48c27`.
  * PyPI: **not published**, exact blocker `publish-pypi: no token available`.
    PyPI still serves 1.4.0 with the stale long description that this release
    corrects. The built 1.4.1 wheel (`74779b5c…b176`, 115,540 B) and sdist
    (`9fa6cfb6…79a0`, 156,293 B) are attached to the GitHub release, and
    `scripts/publish-pypi.sh` now leaves an operator-supplied token file
    intact.
  * Post-tag observation receipt:
    `bench/receipts/release-artifacts-20261113T032500Z.json`. The aggregate
    receipt for the release tree is `verify-20261113T022751Z.json`
    (**46/46**, at `e67e631`).

- 2026-11-13 (v1.4.2, packaging correctness): the wheel long description no
  longer describes its own publication. `python/README.md` is baked into the
  wheel at build time, and every release so far wrote a block about its own
  upload state — the live 1.4.0 text said PyPI served 1.2.0 and that 1.4.0 was
  "not uploaded"; the live 1.4.1 text said PyPI served 1.4.0 and pointed
  elsewhere for the 1.4.1 hashes. That sentence is false the instant the
  artifact is uploaded, so the class is removed, not reworded.
  * `python/README.md`: install commands are unpinned; one stable paragraph says
    the package is published on PyPI and crates.io and sends the reader to the
    project pages and `docs/RELEASE-v*.md` for the current version, sizes and
    hashes; the file names no version and no digest. The
    `python-readme-current` generated block and its manifest entry are deleted.
  * `scripts/check_long_description.py` + `scripts/check_long_description_court.sh`
    fail closed on version-pinned installs, local-artifact/wheel installs,
    "PyPI serves …", upload-state claims, a version literal bound to
    `weftcodec`, and any 64-hex digest; the three stable statements are
    required. Registered in `scripts/verify-all.sh` as the `long_description`
    court with the aggregate's explicit skip semantics. Demonstration (old text
    fails, new text passes) in
    `bench/receipts/long-description-court-20261113T045732Z.json`.
  * 1.4.1 reality recorded: `weftcodec 1.4.1` exists as three builds —
    `74779b5c…` (named at tag `v1.4.1`), `92efd39d…` (GitHub release assets) and
    `73cd3422…` (the PyPI wheel; sdist `93a0b8c8…`, uploaded
    2026-11-13T03:29Z). Python builds here are not bit-reproducible. Digests,
    sizes, build sources and fetch times are in
    `bench/receipts/release-artifacts-20261113T045609Z.json`, and both release
    notes print a three-build table instead of one "the" digest.
  * Versions 1.4.2 in `Cargo.toml`, `WeftCodec/Cargo.toml`,
    `python/pyproject.toml`, `python/weftcodec/__init__.py` and the Go
    provenance tool version. Wheel and sdist built once in the python court and
    `twine check`-clean; the wheel `METADATA` description is byte-identical to
    `python/README.md`. `cargo publish --dry-run` packages and verifies
    `weftcodec 1.4.2`; the `weftfs` dry run cannot resolve it from the index
    until it is published (same publish-order blocker as 1.4.0/1.4.1).
  * Aggregate: `bench/receipts/verify-20261113T033408Z.json` — 47/47 courts
    pass, 0 failed, 0 skipped, at WeftFS `52da9888f372` (stamp
    20261113T033408Z, review of the release-candidate tree). `perf_numbers`
    `--check`, claims, spec drift and the conformance manifest/coverage are
    green on the final documentation tree; the docs commit follows the measured
    tree and changes no code.
  * Not claimed: no format, protocol, code path or measurement changed; no
    upload of 1.4.2 to PyPI or crates.io was performed by this session; the
    1.4.0/1.4.1 descriptions stay wrong until 1.4.2 is uploaded; the guard is a
    rule set, not a proof; no power-loss, media, fleet, cloud, GPU or live
    Azure/AWS/GCS claim.

- 2026-11-13 (v1.4.2 released): merged and tagged in all four repositories.
  WeftFS `aac23a9a11e7` (tag `v1.4.2`, tag object `1c8fcc5b6381`), WeftCodec
  `926410bd7572`, WeftCodec-Go `20c320c91056` (no source change; the tag names
  the tree already merged from staging), WeftFS-Go `480eb9e6a924`; all four
  `staging` and `main` trees pushed, all four annotated tags pushed (they drive
  GHCR), and four GitHub releases created. The WeftFS release carries the 1.4.2
  wheel and sdist, and `gh release view` reports the same digests as the local
  build recorded in `bench/receipts/release-artifacts-20261113T045609Z.json`.
  * Post-tag observation receipt:
    `bench/receipts/release-artifacts-20261113T050602Z.json` (merge/tag objects,
    release URLs, asset digests, all four GHCR workflow runs and image-index
    digests, and the PyPI/crates.io `404`s at the stamp).
  * Registry uploads were **not** performed by this session, by instruction.
    `weftcodec 1.4.2` and `weftfs 1.4.2` are absent from PyPI and crates.io at
    the stamp; the coordinator uploads the attached wheel/sdist and runs
    `scripts/publish-crates.sh`, then records the live checksums.
  * The live 1.4.1 wheel and sdist on PyPI are the third 1.4.1 build
    (`73cd3422…`, `93a0b8c8…`), not the GitHub assets; all three builds and the
    non-reproducibility statement are recorded in
    `bench/receipts/release-artifacts-20261113T045609Z.json` and in both release
    notes.

- 2026-11-13 (R26, Context Advantage Court): parent `staging` (code
  `5f3f42f`/`21a16be`, receipt + docs after it); submodules unchanged. The court
  required by programme rule 14: one backend, with and without the context
  plane, and a **strong application-level baseline** that decides whether the
  contribution is real.
  * Contract `bench/CONTRACTS-context-advantage.md`, committed before any
    measurement: arms N (backend alone, plain durable objects, no metadata), M
    (zstd-3 blob + canonical manifest + a real key→value index on the brief's
    five dimensions plus position, full value validation before reuse,
    TTL/eviction, own subset selection), W (mode-19 provenance-bound KV pages,
    capability-scoped reads, `weft.checkpoint.v1` manifests, causal closure).
    Held constant: local POSIX data plane, commit protocol and durability
    barrier, 65,536 B chunks, no decoded-object cache, no single-flight, one
    thread, identical logical objects and demanded output set, fixed clock, and
    the unchanged 3 GiB/1024-pid/4-cpu court.
  * Harness `examples/context_advantage_court.rs`, runner
    `scripts/context_advantage_court.sh`, seven interleaved repetitions with the
    arm order rotated per repetition, raw samples in the receipt.
  * Receipt `bench/receipts/context-advantage-20261113T054528Z.json` (stamp
    `20261113T054528Z`, WeftFS `21a16be12b66`), analysis
    `bench/RESULTS-context-advantage.md`. Measurement validity: **pass**;
    **`claim_holds: false`**.
  * **M wins on the fast path.** All three arms serve the happy path exactly
    (64/64 byte-exact reads each), but a plain zstd blob with an in-memory index
    and a SHA-256 check is faster than a scoped mode-19 read.
  * **M ties on materialized bytes**: for the identical demanded recovery output
    set (six tensors, two activations, one derived artifact) both arms
    materialize the contract's bytes exactly. W does not win by materializing
    less.
  * **W's safety result is 0**: 56/56 declared cross-scope probes refused, zero
    bytes read and zero bytes returned, because authorization precedes the
    read. M also refuses all 56 — but reads 766,026 B over 48 requests to reject
    the 24 whose five-dimension key matches, and 0 for the 32 it misses. N, the
    floor, accepts 48 and returns the wrong payload for all 48.
  * **W's availability result**: after the metadata database (index + manifest
    objects) is removed from the same store, M reuses 0 of 16 compatible pages
    and W reuses 16 of 16 byte-exactly. N reuses 16 of 16 but was never safe.
    This is the only capability that survived all five pre-registered falsifiers
    (F1–F4 hold; C1/C2 do not, so the fast-path claim is excluded).
  * **The fast-path tolerance was declared in advance and failed.** Declared:
    W p50 ≤ 2.0× N, p95 ≤ 2.5× N, bytes ≤ 1.10× N. Measured: p50 187.8 µs vs
    28.9 µs (6.51×), p95 250.8 µs vs 31.7 µs (7.91×), bytes 0.431×. The
    tolerance was not widened. M is also outside it (3.61× p50), which shows the
    penalty is mostly payload decode, not the context plane: decoding the same
    page in memory as mode 12 vs mode 19 costs 116.4 µs vs 132.4 µs, so the
    descriptor is **+13.7 %** (frame +1.90 %).
  * **W's recovery losses**: identical logical bytes, but 2.07× M's backend
    reads and 8.23× M's wall time, because `restore_named_tensor_at` reloads the
    checkpoint manifest on every call. Published, not hidden.
  * Two earlier seven-repetition receipts (`...T053807Z`, `...T053930Z`) are
    retained but not cited: the first recorded a null stamp (the runner did not
    forward it into the container) and both used a fixture whose payload was
    identical across scopes, which made "accepted but wrong bytes" structurally
    unobservable.
  * Courts: `context_advantage` is registered in `scripts/verify-all.sh` with
    the reduced three-repetition configuration.
  * Not claimed: no fleet, cloud, multi-writer, media or power-loss result; no
    GPU or production-prefill result (`recompute` is a scalar kernel that
    regenerates the page and is labelled as such); no codec comparison; no
    timing or access-pattern privacy from the salt; no "impossible without
    WeftFS" claim. An application baseline that embeds a canonical scope record
    in every blob and rebuilds its index by scanning is **available and
    untested** — it is the most important open baseline, named in
    `bench/RESULTS-context-advantage.md` §9.

- 2026-11-13 (R27, the fast path attributed, and the duplicated decodes removed):
  parent `staging` `86c403b` (code + tests, pushed first) and this docs/receipt
  commit; submodules unchanged. R26 could only say "the frame must be decoded";
  this session measured where the 187.8 µs went, fixed what the ledger
  justified, and published the floor that remains. **The declared tolerance is
  unchanged and the gate still fails.**
  * Evidence for every number below: receipt
    `bench/receipts/context-advantage-20261113T061907Z.json` (stamp
    `20261113T061907Z`, WeftFS `20e66147a7f8ee6fcdc233a94b367359f415262e`),
    label `full`, 7 interleaved reps; reduced three-rep receipt
    `...T061953Z` for the aggregate court. Analysis
    `bench/RESULTS-context-advantage.md`; every number in that file is emitted
    from this receipt by `scripts/perf_numbers.py`.
  * **Instrumentation, benchmark-only.** `src/phase_profile.rs` gained sixteen
    disjoint read-path phases (path resolution, object-record lookup, descriptor
    decode + identity, capability envelope + grants, POSIX/ACL, manifest I/O,
    manifest hash, manifest decode, object I/O, object blob hash, chunk
    assembly, whole-content verification, frame parse, payload decode, exactness
    compare). No phase nests inside another, so a ledger sums. The court runs an
    extra instrumented pass per arm (`fast_path_ledger`) over the same 64 reads
    and records, per phase, sections / total / µs per read / p50 / p95, plus its
    own wall and the unnamed residual. `decode_only` was extended to split the
    mode-19 decode into byte planes, reconstructed-page hash, descriptor parse
    and descriptor identity.
  * **The ledger (W, µs per read, medians of seven interleaved reps).** Payload
    decode dominates at 151.7; object blob hash 9.6, whole-content verification
    9.6, object I/O 2.2, manifest I/O 1.6, path resolution 1.1, manifest decode
    0.9, frame parse 0.9, compare 0.9, and the **whole authorization preamble
    0.59** (record 0.23, descriptor 0.16, capability 0.14, POSIX 0.05). N's
    entire read is 29.4 µs with the same hash and I/O rows. The instrumented
    wall is 183.6 µs against the headline p50 of 169.7 µs; the ~14 µs
    difference is the profiler's own global-lock-per-section cost, stated in the
    results.
  * **Cause found and fixed: the same immutable descriptor was decoded twice per
    read.** The stored descriptor was hex-decoded, WCB-decoded, re-encoded and
    hashed on every read (9.2 µs), and the same capability envelope was decoded
    twice — once for the tenant/namespace/root grants, again inside
    `authorize_read` (~5 µs each). Before this session the authorization
    preamble was ~19 µs of the 187.8 µs in the R26 measurement; it is now 0.59 µs.
    * `KvReadCache` (`src/kv_capability.rs`, field on `Store`) keeps the
      validated descriptor and its id, keyed by the object record's
      `(id, content, version)`, bounded at 4,096 entries and **dropped whenever
      the head generation changes** (`fence()`); a hit can only describe the
      record it was derived from.
    * The decoded capability scope is kept beside the envelope it came from and
      is reused **only when that envelope is equal to the caller's**, so a
      forged `envelope_id`/`blob_id` cannot borrow another grant's scope.
    * `Store::authorize_read_with_capability` hands the already-decoded context
      to the unchanged authorization body; `authorize_read`/`authorize_read_in`
      are thin wrappers, so every caller keeps the same order and the same
      checks.
    * **No authorization decision is cached**, no byte is read to decide access,
      and every read still evaluates validity window, tenant, namespace, root
      and POSIX/ACL. `tests/kv_fast_path_cache.rs` 6/6: cached descriptor reuse,
      an expired grant refused on all three reads, a second grant refused while
      another grant's decode is warm, a forged envelope identity refused, a
      commit fencing the cache, and a rewrite authorised only by its own
      descriptor.
  * Measurement validity **pass**, **`claim_holds: false`**.
    * Fast path: W p50 **169.7 µs** vs N **28.9 µs** = **5.84×** (was 6.51×),
      p95 **232.3 µs** vs **31.3 µs** = **7.03×** (was 7.91×), bytes
      **0.431×**. C1 and C2 still fail; the tolerance was not widened. W is now
      **1.63×** M on the same path.
    * Safety unchanged: W accepted 0 of 56 probes, returned 0 bytes, read
      **0 backend bytes**; M accepted 0 but read 766,026 B over 48 requests; N
      accepted 48 and returned the wrong payload 48 times.
    * Availability unchanged: after the metadata database is removed M reuses
      0/16, W 16/16 byte-exactly.
    * Happy path 64/64 byte-exact in every arm; recovery materialises the same
      270,336 B in M and W.
  * **The floor, measured.** Of the remaining cost: `huffman-canonical-v1`
    symbol decode of two 65,536-symbol byte planes (~107 µs in memory), one
    normative hash of the reconstructed page (~10 µs), the descriptor's identity
    (~9 µs of re-encode + hash), and the first touch of the decode buffers
    (~20 µs of page faults for ~196 KiB of fresh allocation). The largest item is
    the payload representation the mode-19 wire format fixes; removing it is a
    format change, not a read-path fix. Nothing in the store read path remains
    unaccounted for.
  * **Recovery gap attributed, not fixed.** W reads the same 10 objects as M and
    **34 manifests against M's 10**; the whole 306,963 B byte gap is 51,160 B per
    demanded tensor — `restore_named_tensor_at` re-reads the ~51 KiB checkpoint
    manifest on every call, where M loads its manifest once. The repair is a
    `restore_named_tensor_with_manifest` entry point; it is **not** in this
    receipt because it would change what the W arm does and break the before/
    after comparison. Named as the next increment.
  * Ledger numbers are generated: `docs/perf-numbers.json` grew from 420 to 483
    numbers (phase ledger, decode split, recovery counts, and a
    receipt-against-receipt comparison) in 78 blocks, and
    `scripts/perf_numbers.py --check` is green.
  * Gates: `cargo test --all-targets` 172 suites / 813 passed / 0 failed / 0
    ignored; `cargo test --features fuse --all-targets` 172 suites / 856 passed
    / 0 failed / 2 pre-existing ignored; `tests/kv_fast_path_cache.rs` 6/6 in
    the rust court; `scripts/check-claims.sh` 1070/1070; `perf_numbers --check`
    483 numbers / 78 blocks; `scripts/check-spec-drift.sh` 60/60 (no spec or
    wire-format change: the profiler phases and the caches are internal);
    conformance manifest and coverage 420 entries OK. `scripts/verify-all.sh`
    was **not** re-run; the cited aggregate remains
    `bench/receipts/verify-20261111T131235Z.json` (45/45) until the next
    aggregate run.
  * Residual: the fast-path gate still fails (5.84×/7.03× against 2.0×/2.5×);
    the dominant remaining cost is the mode-19 payload codec, which is a
    wire-format question, not a read-path one; the recovery gap is attributed
    but not fixed; the descriptor cache is per store handle (not shared); the
    measured configuration keeps the decoded-object cache and single-flight
    **off** for every arm, as the contract requires, so none of these numbers
    come from caching decoded pages.

- 2026-11-13 (R28, the recovery manifest resolved once, and the representation
  question answered): parent `staging` `923c7bc` (code `c45d5de`, harness and
  contract `dd4a027`, receipt fields `ee13bc5`, field name `5752a83`, loss text
  `923c7bc`);
  submodules unchanged. Evidence: receipt
  `bench/receipts/context-advantage-20261113T070714Z.json` (stamp
  `20261113T070714Z`, WeftFS `923c7bc4d1871ef0ba66faeb8cac87994fae3503`, label
  `full`, 7 interleaved reps), reduced three-rep receipt
  `bench/receipts/context-advantage-20261113T070956Z.json`; analysis
  `bench/RESULTS-context-advantage.md`, design
  `docs/KV-PROV-REPRESENTATION.md`. Measurement validity **pass**,
  **`claim_holds: false`**.
  * **Recovery manifest reuse (new API).** `Store::load_checkpoint_manifest[_at]`
    returns a `LoadedCheckpointManifest` (name, content address, root, manifest);
    `Store::restore_named_tensor_with_manifest` restores one named tensor from
    it through the same per-tensor code as `restore_named_tensor_at`
    (`restore_manifest_tensor`): every shard is still read through the
    content-addressed object path, every shard's content root and own logical
    root is still recomputed and compared, the assembled tensor root and any
    requested topology root are still compared, and a manifest naming another
    checkpoint is refused. A forged manifest cannot make the store return bytes
    that do not hash to the stored shard roots, because the roots are compared
    with the bytes actually returned. `tests/checkpoint_manifest_reuse.rs` 6/6:
    byte-identical bytes/regions/topologies against the per-call path, exact
    measured saving (only the manifest read), wrong-checkpoint refusal, forged
    shard root and byte length both `Corrupt`, pinned manifest equals live.
  * **Measured recovery, before and after** (same court, same fixture; R27
    receipt `...T061907Z`, R28 `...T070714Z`): W backend bytes **593,503 →
    321,343**, wall **3.340 → 1.305 ms**, checkpoint manifest loads **6 → 1**,
    manifest-blob read sections **34 → 28**. The removed **272,160 B** is exactly
    six loads of **45,360 B** (the 45,197 B manifest object plus the 163 B
    manifest blob describing its single chunk). Gap against M: **306,963 →
    34,803 B** (**2.07× → 1.12×**), wall **8.32× → 3.21×** M. Materialized
    bytes were and remain identical (270,336 B). What is left is decode and
    root recomputation, measured per output: W **97.87 µs** vs M **24.70 µs**.
  * **Contract reconciled (task 4).** `bench/CONTRACTS-context-advantage.md` is
    now version 2: §3 C3 printed 1.25× while the harness declared and enforced
    1.10× in every receipt since R26. The document was corrected to the enforced,
    stricter value with an explicit change log; no arm, invariant, metric, gate
    or tolerance changed, and no run was measured under the printed value.
  * **Representation question: no existing representation exists.** Mode 19's
    payload is the mode-12 payload verbatim and the mode-12 descriptor (embedded
    verbatim) fixes `plane_codec` to exactly `huffman-canonical-v1`
    (`spec/WEFT-AI-CODEC.md` §13.2/§13.3, `WeftCodec/src/kv_cache.rs`); the eight
    provenance dimensions exist only in mode 19, and tensor/raw store strategies
    carry a tensor logical root, not provenance. So a page cannot carry mode-19
    identity with a raw payload today. Measured payload step on the same page:
    Huffman planes **103.61 µs** / **632.5 MB/s**, one-allocation-plus-copy floor
    **0.70 µs** / **93,489.3 MB/s** (ratio **147.8×**). **Storage delta of this
    receipt: zero** — no representation changed, the pages re-encode to the same
    225,201 B of mode-19 frames and the store footprint is unchanged.
  * **Decision on the format.** Report the design, do not implement it blind.
    `docs/KV-PROV-REPRESENTATION.md` proposes an additive `plane_codec` value
    (`raw-le-v1`) in the existing field, a representation-independent
    `scope_root` derived over the eight dimensions plus `payload_hash` so "same
    provenance identity, either representation" is checked rather than asserted,
    old readers failing closed on the unknown value, no frozen vector touched,
    and a court that must publish the storage delta beside any latency win. It is
    not implemented: replacing the payload step is not shown to meet C1, because
    the descriptor parse, the descriptor identity and the reconstructed-page hash
    are normative per-read checks a raw payload still pays, and the store path's
    Huffman cost is bounded only between the in-memory row and the ledger row.
    No representation claim is made, and the receipt's `not_measured` says so.
  * **Unchanged results, re-measured.** W **0/56** cross-scope probes accepted,
    **0** bytes returned, **0 backend bytes and 0 requests** to refuse; M
    **0/56** but **766,026 B over 48 requests**; N accepts **48** and returns the
    wrong payload **48** times. After the metadata database is removed M reuses
    **0/16**, W **16/16** byte-exactly. Happy path **64/64** byte-exact in every
    arm; recovery materializes the same 270,336 B in M and W.
  * **Fast path, still failed and not re-framed.** W p50 **169.2 µs** vs N
    **29.3 µs** = **5.78×** (declared ≤ 2.0×), p95 **239.1 µs** vs **32.8 µs** =
    **7.29×** (declared ≤ 2.5×), backend bytes **0.431×** (declared ≤ 1.10×, so
    C3 passes). The tolerance was not widened and no metric changed. The floor
    is the representation plus the plane's normative per-read verification, and
    that is now stated in those words.
  * Gates at `923c7bc`: `cargo test --all-targets` **173 suites / 819 passed /
    0 failed / 0 ignored**; `cargo test --features fuse --all-targets` **173
    suites / 862 passed / 0 failed / 2 pre-existing ignored**;
    `scripts/check-claims.sh` **1079/1079**; `scripts/perf_numbers.py --check`
    **512 numbers / 81 blocks**; `scripts/check-spec-drift.sh` 60/60;
    conformance manifest and coverage 420 entries OK. No spec, wire format,
    vector or codec changed, so the WeftCodec suites and the Rust↔Go cross gate
    were **not** run; `scripts/verify-all.sh` was **not** run, and the cited
    aggregate remains `bench/receipts/verify-20261111T131235Z.json` (45/45)
    until the next aggregate.
  * Residual: the fast-path gate still fails by the same margin; the recovery gap
    is now decode and per-shard identity verification rather than metadata; the
    representation design is unimplemented and its storage delta and latency are
    therefore unknown; W's recovery is still slower than M's (3.24×) because it
    verifies more; one host, one backend, no fleet/cloud/media/power-loss claim.
  * Two comments written after the cited run (`src/checkpoint.rs` and the court's
    W arm) drop a fixture-specific size from their prose; they change no code,
    so the cited revision `923c7bc` and the current tree differ only in comment
    text.

## Subphase R29 — Context Advantage Court on a second backend, and the representation decision (2026-11-13)

The flagship court now runs on **two** backends with **the same declared
tolerances**, and the primary row keeps its result. Contract
`bench/CONTRACTS-context-advantage.md` **version 3** was committed before the
second backend was measured: it adds §2.1 (one loopback MinIO process, one
bucket per arm/repetition/store, the same arms, fixture, durability contract,
concurrency and repetitions) and states that **no tolerance is added, relaxed or
widened** and that the local row remains the primary result.

* **Second backend, measured.** Pinned MinIO `RELEASE.2025-04-22T22-12-26Z`
  (binary sha256 `53e2a2cb…bb817c`) in a `debian:bookworm-slim` sibling
  container, single node, tmpfs data directory, same hard limits
  (3g / 3g swap / 1024 pids / 4 cpus / bounded `/tmp`, never raised). Receipt
  `bench/receipts/context-advantage-s3-20261113T073016Z.json` (stamp
  `20261113T073016Z`, WeftFS `ad26381`, seven interleaved repetitions),
  `result=pass`, **`claim_holds: true`**: W p50 **595.0 µs** against N
  **407.4 µs** = **1.46×** (declared ≤ 2.0×), p95 **731.9** against **507.4** =
  **1.44×** (≤ 2.5×), backend bytes **0.431×** (≤ 1.10×). **C1, C2 and C3 hold
  on this backend.** That is neutrality, not a win: W is still slower than the
  floor, and it is published that way.
* **Primary row re-run through the same harness** and re-published: receipt
  `bench/receipts/context-advantage-20261113T072921Z.json` (stamp
  `20261113T072921Z`, WeftFS `ad26381`), `result=pass`,
  **`claim_holds: false`**; W p50 **169.6 µs** against N **29.1 µs** =
  **5.84×**, p95 **6.62×**. The tolerance was **not** widened. Local p50 moved by
  under one percent and p95 by a few percent versus the R28 receipt, which is
  run-to-run noise on one host; the newer receipt supplies every current number
  so both rows come from one harness.
* **Structural results are identical on both backends.** W refuses **56/56**
  cross-scope probes with **0 backend bytes and 0 bytes returned**; M refuses
  56/56 but reads **766,026 B** over 48 requests to find the mismatch; N accepts
  **48** and returns the wrong payload **48** times. After the metadata database
  is removed M reuses **0/16** and W **16/16** byte-exactly. Happy path **64/64**
  byte-exact in every arm; recovery materializes **270,336 B** in both M and W.
* **Crossover, derived and labelled.** Per-request cost on the MinIO row ≈
  **189.2 µs** (from N's two rows), which predicts a W p50 of **547.9 µs**
  against **595.0 µs** measured (+8.6 %). The declared 2.0× p50 tolerance is
  reached at **≥ 55.7 µs** per backend request; the byte-only crossover is
  **3.76 ns/B**, i.e. ≈ **266 MB/s** of byte-proportional throughput. Two
  measured operating points bracket the crossover; no point was measured *at*
  it and no backend cost was varied, so every crossover number is **derived,
  not measured**. Shaping the link needs `CAP_NET_ADMIN`, which the court does
  not have (`cap-drop=ALL`) and which is not raised for a benchmark.
* **Representation decision (`raw-le-v1`): not accepted; not implemented.** On
  the same pages a raw payload is **2.33×** the shipped frames
  (**+132.8 %** on the page payload); with the **entire payload step removed**
  the local fast path is still **65.9 µs**, i.e. **2.27×** N — above the
  declared 2.0× — because the reconstructed-page hash, the descriptor parse, the
  descriptor identity and the decode-buffer first touch are normative per-read
  checks a raw payload still pays. The shipped codec already meets the gate on
  the second backend (**1.46×**), so the change would alter no measured decision
  on either backend, and it costs storage exactly where storage is scarce. The
  design stays specified in `docs/KV-PROV-REPRESENTATION.md` §7, which names
  the single condition that would reopen it.
* **README** now reaches the flagship without hunting: a same-backend A/B
  (rule 13), the three capabilities that hold, the honest local negative with
  its reason, the measured second row, the derived crossover and the
  representation decision — every number emitted from `docs/perf-numbers.json`.
  `scripts/verify-all.sh` registers `context_advantage_s3` (reduced); the
  aggregate was **not** run, so the cited aggregate remains
  `bench/receipts/verify-20261111T131235Z.json` (45/45).
* **Revision note.** The six receipts record `environment.revisions.WeftFS =
  ad26381…`, the base they were branched from. The tree they measured is
  byte-identical, for every file this subphase touches, to the commit that
  carries the harness (`824c946`): `git diff 824c946 --
  examples/context_advantage_court.rs scripts/context_advantage_court.sh
  scripts/docker-court.sh scripts/verify-all.sh` is empty, because those files
  were already in their committed form when the courts ran and were not edited
  afterwards. A receipt is not evidence if its revision field names a tree that
  cannot produce it; this one names a tree that differs only by not yet
  containing the very files the measured tree already had.
* Gates: `cargo test --all-targets` **173 suites / 819
  passed / 0 failed / 0 ignored**; `cargo test --features fuse --all-targets`
  **173 suites / 862 passed / 0 failed / 2 pre-existing ignored**;
  `scripts/perf_numbers.py --check` **587 numbers / 85 blocks**;
  `scripts/check-claims.sh` **1087/1087**; `scripts/check-spec-drift.sh` 60/60;
  conformance manifest and coverage **420 entries OK**. No wire format, frame,
  descriptor, vector, codec or store format changed, so the codec suites and the
  Rust↔Go cross gate were **not** run, and `scripts/verify-all.sh` was **not**
  run.
* Residual: the crossover is derived from two points rather than swept; the
  second row is one loopback server (not a cloud, fleet, media or power-loss
  court); `raw-le-v1` remains unimplemented so its latency is unknown; the
  local gate still fails and is not re-framed.

## Plan reprioritisation — EKBLOOM `E1–E6`/`X22` moved to the front of the backlog (2026-11-13)

At operator request the optional EKBLOOM representation trials were moved to the
front of the executable backlog. **No code, format, court, contract or receipt
changed; nothing was measured; and no performance, storage, latency, throughput
or resource number is claimed in this entry.** The deliverable is
[`docs/EKBLOOM-PLAN.md`](EKBLOOM-PLAN.md) plus a forward pointer in
[`docs/PROGRAM.md`](PROGRAM.md) §E.

* **What the plan contains.** Position and framing under rules 13–14; a precise
  inventory of what already exists in the tree; the EKBLOOM revision it was
  written against (`infinityabundance/EKBLOOM`
  `8275a41b72c17abdc55bf74b82f319876e587ba0` on `main`, crate `ekbloom` 0.7.3,
  checksum `1c56ae95…5da4`); verbatim quotations of EKBLOOM's own claims with
  URLs, separated from "WeftFS has measured", which is **nothing** — WeftFS has
  never executed EKBLOOM; a falsifiable gate per subphase with its declared
  axis, court, receipt pattern, win and null result; a dependency-ordered
  sequence whose first step is the cheapest decisive falsifier; the
  contract-first requirement; explicit non-claims; and the condition that
  retires the experiment.
* **Id collision recorded, not papered over.** `docs/PROGRAM.md` §E already uses
  `E1`–`E7` for strategic pillars, so the executable subphases are written
  `EKB-E1` … `EKB-E6` and `EKB-X22`, each mapped to the operator's label.
* **Operator text not in the tree.** The backlog line the operator quoted
  (`E1–E6/X22 — optional EKBLOOM/DSFB trials retained explicitly …`) does not
  occur anywhere in this repository. A full-tree search for `EKBLOOM`, `DSFB`,
  `X22`, `E1–E6` and `E1-E6` finds only the `AGENTS.md` positioning paragraph,
  **six** negative non-claims about time travel (`README.md` F3, `NOTES.md`,
  `verification/README.md`, `docs/PROGRAM.md` A4, `docs/STATUS.md` twice), and
  unrelated `0x22` bytes in test fixtures and store manifests. The line is
  treated as operator-supplied and quoted as such. `DSFB` is not located in this
  repository or in the three EKBLOOM documents read.
* **Prior pre-registered.** The plan records, before any measurement, the
  prediction that EKBLOOM loses the space, hot-read, cold-read, decode and
  sequential axes on real weights and may win only bounded-step exact small
  reads at a declared retained footprint. The reasoning cites EKBLOOM's own
  `LIMITATIONS.md`/`E2E.md` and the WeftFS R5 entropy-coder court (receipt
  `entropy-coder-20261106T065154Z`), by stamp rather than by number.
* **What was deliberately not touched.** `README.md` keeps its F3 row
  unchanged: the row's only EKBLOOM sentence is a negative non-claim that is
  still true, so the condition for editing it was not met. `docs/claims.json`
  and `docs/perf-numbers.json` are unchanged, because adding a claim entry
  would make the generated claims count in this file's header block stale and
  neither file gains a number from a plan that measures nothing. No dependency,
  payload mode, `CostClass` row, court or receipt was added.
* **Not done.** `bench/CONTRACTS-ekbloom.md` does not exist yet and **must** be
  committed before the first measurement; `EKB-E1` has not been run; the
  MSRV consequence of a normal `ekbloom` dependency (this crate declares 1.77,
  the published crate 1.89) is recorded as an operator decision rather than
  resolved here.

## Subphase X22 — the EKBLOOM line retires: E1 pass, E2 loss, E3 loss (2026-11-13)

The optional EKBLOOM representation trial was executed at operator request and
ended in a published null result. **E1 passed; E2 lost; E3 lost; E4/E5 not run.**
The verdict, the method, every loss and the reproduction commands are in
[`bench/RESULTS-ekbloom.md`](../bench/RESULTS-ekbloom.md); the frozen contract is
[`bench/CONTRACTS-ekbloom.md`](../bench/CONTRACTS-ekbloom.md) (version 1 for
E1/E2, version 2 §9 for E3), and the plan is
[`docs/EKBLOOM-PLAN.md`](EKBLOOM-PLAN.md).

* **What ran.** `EKB-E1` (boundary, exactness, resource bound) passed on the
  pinned crate: every object of both declared containers restored byte-exactly,
  the crate's own deep verifier accepted every container, no object was refused
  for `memory_bound`, and no court limit was raised. Receipt
  `bench/receipts/ekbloom-20261113T081205Z.json` (stamp `20261113T081205Z`,
  WeftFS `bc75d63b68e8`).
* **E2 (space) lost.** EKBLOOM's aggregate `.ekb` container is materially larger
  than the shipped planner's own footprint for the same bytes, with no per-object
  win on the declared class-sampled set; the receipt publishes the aggregates,
  the per-object table and the ratio. Receipt
  `bench/receipts/ekbloom-space-20261113T081552Z.json` (stamp
  `20261113T081552Z`, WeftFS `f64287c95475`).
* **E3 (bounded random access) lost on both declared conditions.** Its retained
  footprint was larger than the shipped mode-15 tiled representation (though
  smaller than the raw baseline), and its pooled p95 was far outside the
  tolerance borrowed from `bench/CONTRACTS-context-advantage.md` §3 C2, even
  though it was faster than the tiled store on that statistic. The bounded-step
  mechanism itself was real: the in-memory `Engine` probe decoded every request
  exactly with no positioning-bound violation. A working mechanism is not a
  portfolio win, and the results document says so. Receipt
  `bench/receipts/ekbloom-read-20261113T083231Z.json` (stamp
  `20261113T083231Z`, WeftFS `ef2aa0a3a21a`).
* **The §4.0 prediction was confirmed on the measured axes.** EKBLOOM lost space
  and did not produce a portfolio win on the one axis it was allowed to win. The
  decode and sequential predictions were never tested because retirement fired
  first, and they are not claimed either way.
* **Retirement under plan §8.2.** Both portfolio axes are lost, so the line
  retires: no payload mode, no planner slot, no `ekbloom` dependency. No format,
  wire value, cost row, conformance vector, store path or shipped source file
  references the experiment. The harness, runner, contract, plan, receipts and
  results document stay as the reproduction path.
* **Dependency graph verified, not asserted.** At `563348e`, inside the rust
  court, `cargo tree -e normal --offline` and
  `cargo metadata --format-version 1 --offline` were run. `cargo tree -e normal` contains no `ekbloom`, and `cargo metadata` contains no
  `ekbloom`. The harness
  crate is excluded from the workspace in the root `Cargo.toml`.
* **E4 and E5 were not run.** `EKB-E4` (whole-object decode) and `EKB-E5`
  (write pause) are conditional on an E2/E3 win; they were skipped by the
  retirement rule and remain unmeasured. `EKB-E6` and the Go/vector work were
  not attempted. No receipt, partial run or number implies a pass or a failure
  for them.
* **No shipped code or format changed**, so `scripts/verify-all.sh` was **not**
  rerun and the cited aggregate is unchanged
  (`bench/receipts/verify-20261113T033408Z.json`). The only host gates run were
  `scripts/perf_numbers.py --check` and `scripts/check-claims.sh`, both after
  regeneration; the dependency check ran in the rust court.
* **Claims and numbers.** The verdict claims are in `docs/claims.json` with the
  three receipt stamps and revisions; the published figures are generated from
  the receipts by `scripts/perf_numbers.py` and catalogued in
  [`docs/PERF-NUMBERS.md`](PERF-NUMBERS.md). No number in the results document
  is hand-written.
* **Residual.** One host, one local POSIX backend, warm page cache and a
  process-cold first pass; no fleet, cloud, media, power-loss, GPU or serving
  claim. No mature filesystem was measured (rule 13). The contract's stated
  expected sample counts did not match the achieved counts because its own
  `L >= len` rule excludes rank-1 tensors from the larger region classes; the
  gate uses the pooled p95 and is unaffected, and the receipt records the
  arithmetic.

## Subphase X11/X12/N06/N12/T009 — release CI and truthful release/artifact status (2026-11-13)

The audit that opened this subphase described the programme as if v1.3.0 were the
latest release and PyPI were unpublished. Both were stale. This entry records
what was observed, what was added, and what remains unverifiable from the
development host.

* **Ground truth, observed before any edit.** `git tag` lists `v1.4.2` in the
  parent and in all three subprojects; `gh release list` shows a `v1.4.2`
  release (not draft, not prerelease) in all four repositories, and the WeftFS
  release assets carry the same two digests as the PyPI files. The PyPI JSON API
  reports `info.version` `1.4.2` with 1.2.0, 1.4.0, 1.4.1 and 1.4.2 present and
  none yanked; crates.io reports `weftcodec` and `weftfs` `1.4.2`, none yanked.
  The live long description is the R25 replacement text: it names no version, no
  artifact file and no digest, and makes no statement about its own upload.
* **Added `.github/workflows/ci.yml`.** The program had only the tag-driven
  `containers.yml`. The new workflow runs the cheap deterministic gates on
  `pull_request`, on pushes to `main`/`staging`, and on dispatch: rustfmt,
  clippy, the store and codec Rust suites, the Go gates for both subprojects, and
  the host-only documentation and conformance guards. A TLA job is
  dispatch-only. Top-level `permissions: contents: read`, no secrets, no
  `pull_request_target`, `concurrency` with cancellation, and a
  `timeout-minutes` on every job. Every action is pinned to a commit SHA
  resolved from the action's own repository on 2026-11-13, with the tag in a
  trailing comment.
* **The workflow is unrun, and says so.** GitHub Actions cannot be executed from
  this host. The file states that it has never had a run; the YAML was parsed
  and every `run:` script path was checked to exist. No CI result is claimed.
* **Two gates are non-blocking on purpose.** At this tree rustfmt reports open
  drift across a set of files that includes `src/store.rs`, and clippy reports a
  large warning backlog plus one deny-by-default lint error in
  `tests/packs.rs` (an expression with a multiplied-by-zero operand). Neither is
  rewritten here: mass reformatting and mass lint fixes are unrelated to this
  subphase and would obscure review. Both jobs carry a `TODO` naming the current
  counts and run with `continue-on-error`, so the drift is visible without
  blocking a merge.
* **FUSE stays in the courts.** `cargo test --features fuse` and
  `WeftFS-Go/fusefs` need `/dev/fuse` and `fusermount`; the Go court image has
  neither. The PR jobs therefore run the store, model, index and `cmd` packages,
  and the FUSE suites remain in `scripts/verify-all.sh`.
* **Every command was run in this session.** The Rust store suite, the WeftCodec
  suite, both Go gates and the six host guards passed in the rust and go courts
  or on the host (the guards are host-only because the court images lack `jq`
  and a Python interpreter). The exact commands, the observed results and the
  counts are in the workflow comments and `$GITHUB_STEP_SUMMARY` text, and in
  the subphase report. No command is cited that was not run.
* **N06 placeholders: none remain.** A tree-wide markdown search for
  `PLACEHOLDER` finds nothing in `docs/RELEASE-v1.4.0.md`,
  `docs/RELEASE-v1.4.1.md` or `docs/RELEASE-v1.4.2.md`; the verification and
  artifact sections of those notes are filled from receipts. Nothing was
  invented to fill a section.
* **N12/X12 corrections, added as dated lines rather than rewrites.** The
  README gains a dated "how to read this page" note after the generated
  current-result block: the current shipped write names its receipt, stamp and
  revision; the larger ratios in the release sections are labelled historical
  self-improvements from the bounded pipeline at their own receipt and revision;
  release-vs-staging position is stated for all four repositories; and the
  observed registry state is stated. The `RELEASE-v1.4.2` note gains a dated
  post-upload section recording the live digests and the live long description;
  its earlier "not uploaded / 404" sentences are retained as written, because
  they were true at that note's stamp. The `RELEASE-v1.4.0` note gains a dated
  line recording that its "the 1.4.1 upload replaces it" prediction resolved
  only partly. No generated region and no receipt was edited.
* **What could not be verified.** GHCR package versions: the packages API
  returns HTTP 403 without `read:packages`, so no image digest is confirmed or
  contradicted here; the digests quoted in the release note remain that
  session's observation. GitHub Actions execution: unrun. Nothing else was taken
  on trust: versions, checksums, yanked flags, release assets and tag commits
  were read from the registries or `gh` in this session.
* **Residual.** `containers.yml` still references actions by mutable major tag;
  it is the publishing path and was deliberately not retargeted here. The
  rustfmt and clippy backlogs remain open, as does the deny-by-default lint in
  `tests/packs.rs`. `scripts/check-commit-identity.sh` does not exist in this
  tree, so the guards job does not invoke it.

## Subphase X13/N14 — Azure MW2 mixed-language and ambiguous-success histories (2026-11-13)

The question was not whether Azure MW2 compiles or passes its own unit tests —
it did — but whether *independent Rust and Go writer processes* can share one
Azure-compatible store safely under the failure modes the protocol claims to
handle. The pin was Azurite 3.35.0
(`mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`),
a sibling container under the unchanged court limits. **Azurite is not Azure**:
service-version acceptance, block-blob limits, listing paging, replication,
media and cloud failure modes are not exercised, and no cloud, fleet,
power-loss, media or serving claim is made. MW1 remains the default; every
writer opens with `--writer-mode optimistic`.

* **New court.** `scripts/azure_mw2_court.sh` (host half) with
  `bench/scripts/azure_mw2_inner_court.py` and
  `bench/scripts/azure_mw2_proxy.py` (inside the dual-toolchain `fuse` court)
  runs eight scenarios: disjoint mixed writers; conflicting mixed writers on one
  name; ambiguous success on the conditional head write in both
  kill/resolve directions; fenced takeover with a suspended stale writer in both
  languages; a held reader that must never observe a newer generation;
  concurrent GC with mixed writers and a snapshot root; fail-closed mode
  detection against an endpoint that drops `If-Match` or `If-None-Match`
  (re-signed by the harness proxy, never by weakening the client); and
  cross-language verification plus deterministic parity. The counted table is
  generated from the receipt in
  [`bench/RESULTS-azure-mw2.md`](../bench/RESULTS-azure-mw2.md).
  Positive receipt `azure-mw2-20261113T103500Z.json` at WeftFS `103dd99` /
  WeftFS-Go `a6c1c60`: every scenario and every check passed.
* **Defect 1 (S1, first run), fixed at the root.** Both Azure `tmp/` sweeps
  deleted every blob under `tmp/` and ignored the grace argument, assuming the
  writer lock was held. An optimistic open holds none, so concurrent writers
  deleted each other's live capability probes and four Rust writers each
  refused a healthy Azurite as `UnsupportedVersion`. The sweep is now age-gated
  by `Last-Modified`, and an entry whose age cannot be established is kept. The
  same defect exists in Go (`61ac5c9`) and the identical un-gated pattern
  remains in the Rust and Go S3/GCS sweeps (recorded as a residual, not fixed
  here).
* **Defect 2 (S6, first run), fixed at the root.** `AzureBackend::modified`
  returned `None` because the crate had no HTTP-date parser, so the GC age gate
  always measured zero and a sweep on Azure could never reclaim anything.
  `modified` now `HEAD`s `Last-Modified` and parses RFC 1123 with a new
  `parse_rfc1123_secs`/`days_from_civil` pair.
* **Negative result retained.** With the sweep grace at 1 s — shorter than a
  transaction's prepare-to-publish interval — a concurrent pass reclaims
  prepared blobs and a committed generation becomes unreadable. Receipt
  `azure-mw2-20261113T103700Z.json`, same two revisions, one variable changed.
  The court therefore ages its garbage past a 10 s window instead of relaxing
  any check, and the results document states the operational contract: size
  `gc_grace_secs` above transaction latency, or collect while quiescent. MW2 GC
  has no exclusive coordinator.
* **Verification run.** Rust `cargo test --all-targets` passes; the existing
  Azure backend and fencing suites and the Go Azure tests re-ran unchanged
  against the same image digest (receipt `azure-20261113T094005Z.json`), and
  WeftFS-Go `go test -race ./store/...` passes. `scripts/verify-all.sh` was
  **not** rerun, so the cited aggregate receipt stays
  `verify-20261113T033408Z.json`; the next full run carries `azure_mw2` with
  its own skip path.
* **Not proven.** No live Azure/GCS/S3 run; process faults only, not power loss;
  S4 is the fenced *lease* (MW1) and not two MW2 committers racing; no
  per-operation latency is claimed.

## CI defect closure: the first real workflow run (2026-11-13)

The first run of `.github/workflows/ci.yml` on a stock runner
(`43398242038`, head `3f9f327`) failed two blocking jobs. Both are closed at
the root here. Nothing about the on-disk format, the wire format, or the
writer-lock contract changed.

### Defect A — the quota CLI tests were root-only, not a writer-lock flake

The `rust-test` job failed on `cli_quota_denial_is_non_disclosing` and
`cli_quota_show_is_limited_to_the_callers_principal`, at the assertions that a
256 KiB write is refused and that a small write is charged. The CI comment and
the earlier status note attributed this to the `flock(2)`/`fork` window; **that
attribution was wrong**, and the run itself refutes it: every writer-lock suite
(`usage_survives_reopen_and_still_denies`, the distributed interlock, the MW2
courts) was green in the same job.

Root cause, in two parts:

* `weftfs create` (like every write verb) defaults the object owner and the
  charged quota principal to the **invoking process's real uid/gid**
  (`current_ids()`, read from `/proc/self/status`). The Docker court runs as
  root, so the ambient principal was `0:0` and matched the `--quota 0:0:...`
  policy. A stock GitHub runner runs as uid 1001, the write was charged to
  `1001:1001`, which has no configured budget (a budget is per-principal and
  opt-in), so it was admitted; and usage looked empty to a `0:0` query. The two
  tests therefore encoded an environment assumption, not a lock defect.
* `weftfs quota show` additionally defaulted `--uid`/`--gid` to `0:0` instead
  of the caller, so an unprivileged caller was shown a principal that was not
  theirs (and root's counters) — a real defect against the documented
  "show the caller's own usage" contract, independent of the tests.

Reproduced before the fix, in the rust court, by making the caller explicit
(the same thing a non-root runner does implicitly): with
`--quota 0:0:4096:100`, `create --uid 1001 --gid 1001` of a 256 KiB file
exited 0, `quota show --uid 0 --gid 0` reported zero bytes and zero objects,
and `quota show --uid 1001 --gid 1001` reported the write. That is exactly the
pair of CI assertions that failed.

The fix: `quota show` resolves an omitted `--uid`/`--gid` to the caller
(`src/main.rs`), so the command now means what it says on any host; the two
CLI tests name the principal they are writing as (`--uid 0 --gid 0`), so they
no longer depend on the ambient identity. Two tests pin the behaviour:
`cli_quota_show_defaults_to_the_caller` (an implicit query equals an explicit
query for the caller, and a non-root caller does not see root's counters) and
`cli_quota_budget_applies_only_to_the_configured_principal` (the semantic the
old tests relied on). `scripts/docker-court.sh` gained `WEFT_COURT_UID`/
`WEFT_COURT_GID` (validated, default `0:0`, no limit affected) because a court
cannot `setuid` under `cap-drop=ALL`; the quota suite is run under a non-root
uid to prove the independence. Mutation-checked: restoring the old
`quota show` default makes `cli_quota_show_defaults_to_the_caller` fail under
uid 1001 with `left: "0:0", right: "1001:1001"`, and the fixed tree passes.

**Writer lock (Task A of the brief), verified not changed.** The single-writer
lock is already `F_OFD_SETLK` plus a compatibility `flock(2)`, both released
explicitly before the descriptor closes (`src/writer_lock.rs`; landed in
`580444c`, documented in `docs/FENCING.md` §9 and `docs/PLANES.md` §6). The
explicit unlock is the part that removes the `fork`-inheritance window: OFD
alone is inherited exactly like `flock` and would not have fixed the historical
flake. Mixed-version exclusion is symmetric (the shipped writer blocks a
legacy `flock` holder and vice versa); `flock(2)` remains the documented
fallback on `EINVAL`. The court now also asserts the historical `Busy` text
(`a_second_live_writer_is_still_refused`) and proves real cross-process
exclusion with a child that holds both mechanisms on its own open file
description: this process is refused `Busy`, and the kernel releases the lock
when the holder is `SIGKILL`ed
(`another_process_holding_the_lock_refuses_this_one`). WeftFS-Go holds the
same two locks and unlocks both (`WeftFS-Go/store/lock.go`, `lock_linux.go`,
`store/lock_inheritance_test.go`), so no Go change was needed. The false
"known flake" caveat was removed from `ci.yml` and replaced with the true
account; the runner stays non-root and the suites are not filtered.

### Defect B — claims cited a receipt that was never committed

The `guards` job failed because `docs/claims.json` cited
`bench/receipts/r22-pareto-20261112T230000Z.json`, which existed only in the
authoring working tree. The R22 group in `docs/perf-numbers.json` cited it for
every number it generates. `check-claims.sh` only tested that the
file existed, so it passed locally and failed on a fresh checkout.

The file was the genuine `weft.write_cost_ablation.v1` receipt for the claims
(`result: pass`, every check true). Its values were re-read against the claim
text before committing: the frontier row's least-contended and median wall,
the footprint, the native anchor and the ratio, the one-token-per-worker
control, and the pack-plane row all match. It is committed as-is; no receipt
was edited to fit a claim.

The supersession audit found that the guard had a second blind spot: a claim
could cite a receipt its own results document labels superseded. Three were
named by the brief and confirmed, plus two earlier verification aggregates
cited by historical claims. Each decision (all recorded as fields on the claim
itself):

* **Re-pointed** where the value is present in the current receipt:
  `r9_index_rewritten_rows` (row-level -> element-level index chain);
  `s13_chain_data_bytes`, `s13_optimizer_data_bytes`,
  `s13_break_even_restart2`, `s13_dp_matches_oracle`,
  `s13_corpus_step1000_sha` (S13 -> S13b parent graph);
  `trajectory_gc_reclaim` (the only trajectory claim reading the field the
  defect corrupted, and the corrected receipt records the same value).
  `claim_audit_current_receipt` also had the wrong receipt for its own value
  and was re-pointed.
* **Kept with `superseded_by`** where only the historical receipt records the
  value: `s13_parent_graph_receipt`, `s13_chain_worst_restore`,
  `s13_optimizer_worst_restore` -> the S13b receipt; the four `r120_*`
  release-note claims and `r130_aggregate_revision` -> the current aggregate.
* **Kept with `supersession_note`** where the receipt is the only measurement
  of the run and the defect did not touch the field read: the nine remaining
  trajectory size/restore/exactness/depth claims, each naming
  `bench/RESULTS-gc-accounting.md` and the corrected receipt. Their values
  were re-derived from the receipt, not taken from prose (the zstd baseline
  sums, the synthetic `final + Σ zstd19(XOR)` figure and the per-tensor
  exactness count all reproduce exactly).

`docs/superseded-receipts.json` makes the seven supersessions machine-readable
(successor, reason, and the document that declares it). `check-claims.sh` now
also requires every cited receipt to be **committed** and every citation of a
registered superseded receipt to declare `superseded_by` (matching the
registry) or a `supersession_note`. Prose is deliberately not scanned: a
window scan on this tree produced false positives for three receipts that are
explicitly cited. Negative tests were run against the live manifest (untracked
receipt, missing note, wrong successor) and each fails with a named
`CLAIM MISMATCH`.

### Gates actually run for this entry

`scripts/docker-court.sh rust "cargo test --test quota"` and the same suite as
uid 1001 (the compiled test binary directly: the shared cargo target directory
is root-owned, so a non-root `cargo` cannot take its build lock; the test
binary is the same code and spawns the real CLI); `cargo test --test
writer_lock`; the full `cargo test --all-targets`; `cargo test --test
writer_fencing`; `cargo test --test distributed_semantics`; `cargo test --test
multiwriter`; WeftFS-Go `go test -race ./store/...`; `bash
scripts/check-claims.sh`; `python3 scripts/perf_numbers.py --check`; `bash
scripts/check-spec-drift.sh`; the long-description, conformance-manifest and
conformance-coverage guards. `scripts/verify-all.sh` was **not** rerun, so the
cited aggregate stays `verify-20261113T033408Z.json`; no receipt was produced
or altered for this entry and no performance number is claimed.

Residual and stated plainly: the GitHub workflow has not been re-run, so the
green result is local court evidence, not a CI result. A process killed
without running `Drop` still leaves a forked-but-not-yet-exec'd child holding
the lock until it execs; that window belongs to the forking process and is the
same for every mechanism. The claim audit is now enforced for committed-ness
and declared supersession only; it cannot detect a receipt that supports a
different number than the prose cites, which remains a review duty.

## Subphase X04 — bounded 7B ingestion, streaming mode 9, and the catalog cost (2026-11-13)

**Scope.** Three defects measured in `scale-7b-20261110T213731Z`: a 1.09 GB
tensor could not be encoded by the planner at 3 GiB, the packed plane wrote
+51.16 % metadata, and no planned 7B write had ever been attempted. This
subphase fixes all three without changing a frozen byte: the new encoders are
byte-identical schedules, and the full-store identity court compares every file.

**Code.** `WeftCodec` `7c8dac3` adds `ByteSource`,
`encode_float_tensor_stream` (bounded histogram/hash pass, then a bounded
emission pass into the exactly-sized frame buffer) and `encode_zstd_stream`
(chunked frames only), plus `frame::encode_preamble` so the streaming assembler
shares the header/table code. Parent `cbfe86f` adds
`Store::prepare_tensor_object_streamed` / `write_tensor_file_from_source`, the
streamed `two_stage::analyse_source`, and the N15 catalog deferral in the packed
plane. `62845b6` adds the mode-9 streaming court.

**Identity.** `WeftCodec/tests/mode9_streaming.rs` 9/9: five dtypes, twelve
element counts, five window sizes and all 290 real Qwen2.5-0.5B tensors encode
byte-identically. `tests/streamed_plan_identity.rs` 2/2: the slice and streamed
planned writers publish **identical store trees**, head, ObjectId, logical root
and strategy over four dtype/scale cases at two windows. `tests/packs.rs` 15/15,
including one catalog publication per flush with the same final root and flat/
packed heads equal. Flat and packed 7B writes publish the same head
`8cd86ce5…`.

**Measured** (unchanged 3 GiB / 3 GiB / 1024 pids / 4 cpus court; real
Qwen2.5-7B-Instruct, 15,231,272,088 B, 339 tensors).

<!-- BEGIN GENERATED:scale7b-status (scripts/perf_numbers.py) -->
X04 (2026-11-13): bounded 7B ingestion. The planned 8-way 7B checkpoint write runs in
**119,174.9 ms** with a **442,672 KiB** process peak and publishes
**10,160,456,499 B**; the whole-tensor streamed ingest fits the same court at
**2,873,124 KiB**; the packed store is **15,274,042,488 B** with a
**26,707,069 B** catalog (**256** segment publications,
148,694 before); restore is **387.8 MB/s** at 4 workers; and the
streaming mode-9 encoder is **2.492×** smaller and
**0.783×** the one-shot wall on a 1.09 GB tensor.
<!-- END GENERATED:scale7b-status -->

**Losses.** `verify --full` is the memory peak for a whole-tensor store, not the
write (the generated row above is that peak; the per-tensor row in
`scale-7b-20261113T105331Z`, stamp `20261113T105331Z`, revision
`cbfe86f6c90a448e591ac062af1b36c5acdd4d10`, is lower). Flat fast-raw is several
times slower than packed and its wall is dominated by one durability barrier
over every chunk file (both walls are generated above). Flat peak RSS rose from
the pre-X04 receipt's peak (173,032 KiB in `scale-7b-20261110T213731Z`, stamp
`20261110T213731Z`, revision `118015f364d05cc51af5d978a28bf469913303ae`) to the
generated value with no attributable cause; packed RSS rose for the deferred
catalog entries. No Go parity, no power-loss, fleet, cloud or serving claim. The
7B court is **not** in `verify-all` (it needs about 31 GB of free disk beside the
corpus).

**Analysis:** [`bench/RESULTS-scale-7b.md`](../bench/RESULTS-scale-7b.md).

## Subphase X05 — committed evidence, and a lock test that does not race its child (2026-11-13)

**Defect A: a second manifest cited a receipt that was never committed.**
CI run 43408465922 (`documentation and conformance guards`) printed
`perf_numbers: missing receipt bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json`
and failed. `docs/perf-numbers.json` is a manifest that cites receipts, and it
did not inherit the committed-file rule that `scripts/check-claims.sh` had just
gained for `docs/claims.json`; the defect class is the same, in the second place
it can occur.

The whole manifest was audited, not just the named entry: every `receipt`, every
`count_of` source and every generated file was compared with `git ls-files`. One
path was untracked — the one CI named. Before committing it, its declared values
were compared field by field with the receipt: stamp `20261112T043000Z`,
revision `1d716632…`, and `bound_sweep.rows[interval_steps=1, max_outstanding=2]`
carries `loop_wall_ms_median`, `completion_warm.p50_ms` and
`completion_warm.p95_ms`. The generated text already matched those values, so no
region was regenerated and no number changed; the receipt and its `.log` are now
committed.

**The guard that makes the class impossible.** `scripts/tracked_evidence.py`
(Python) and `scripts/lib-tracked.sh` (shell) answer one question — is this path
committed? — and resolve the **nearest enclosing repository**, so a file inside
`WeftCodec/`, `WeftCodec-Go/` or `WeftFS-Go/` is checked against that
repository's index rather than the parent's gitlink. The rule is wired into
`perf_numbers.py` (receipts, `count_of` sources, generated files),
`check-claims.sh` (locations, the manifest), `check-spec-drift.sh` (parent and
exported spec/schema files), `check_conformance_coverage.py` (manifest, vectors,
fixture members, mirrors) and `check_long_description.py`. Outside a git work
tree every guard skips the rule, so an exported tarball and the conformance
bundle still verify.

`scripts/untracked_evidence_demo.sh` creates an uncommitted copy for each guard
and requires the guard to refuse it; it prints `PASS` for all four and leaves
nothing behind. `scripts/perf_numbers_untracked_demo.sh` reproduces Defect A
exactly: on the tree before the receipt was committed, `--check` failed with
`… exists but is not tracked by git in …; an untracked file is not evidence (a
fresh checkout would not have it)`. The guard set in CI did not change.

**Defect B: the two-process lock test raced its own holder.**
`tests/writer_lock.rs::another_process_holding_the_lock_refuses_this_one` closed
the child's `read_fd` and then called `read(read_fd, …)` to hold the open file
description open. The read failed `EBADF`, so the holder called `_exit(0)`
immediately after reporting success, and the parent's probe raced the kernel
releasing the lock. When the parent won the race the test passed; when the exit
won it failed at `tests/writer_lock.rs:308:42`, which is how CI saw it. This is a
test defect, not an environment difference: the store code refuses the probe
correctly for as long as the holder lives, and a reduced C reproduction of the
child reported `EBADF` for the hold read in every iteration and saw the parent
acquire the lock only in the iterations where the holder had already exited
(the reproduction is local, not committed).

The fix is explicit synchronisation, with no sleeps, no polling and no retries.
The holder takes `flock(2)`, then `F_OFD_SETLK`, reports a fixed record (phase,
acquired, both `errno`s), and blocks in `poll(2)` on a control pipe it keeps
open. The parent accepts that record, pings the control pipe, and waits for a
second record that can only be written from the blocked state; every read has a
`poll(2)` deadline, and a holder that dies is reported with its exit status
instead of hanging. Only then does the parent probe the store. The assertions
are unchanged: `Busy` with the historical `store is locked by another writer`
text, and acquisition after `SIGKILL`. `with_forked_child`, used by the other
four probes in the file, lost its fixed `usleep` in the same way: the child now
blocks until the parent releases it after the probe, so the window is the probe
and not a duration a loaded runner can overrun. Re-injecting the
close-before-read defect fails deterministically at the liveness round-trip.

**Evidence.** `bench/scripts/writer_lock_court.sh` repeats the suite and writes
a receipt under the standard court limits; the receipts and their logs are
committed. The court is standalone and is not in `verify-all`.

<!-- BEGIN GENERATED:writer-lock-status (scripts/perf_numbers.py) -->
X05 (2026-11-13): the writer-lock suite was repeated **60** times at `--test-threads=8`
with **0** failures; **25** times beside
8 busy CPU processes with **0** failures; and
**10** times as uid 1001 with
**0** failures.
<!-- END GENERATED:writer-lock-status -->

The four sibling suites (`quota`, `writer_fencing`, `multiwriter`,
`distributed_semantics`) pass in the same court, both as uid 0 and as uid 1001.

**Task C: the other guards that consume evidence.** "Fails closed" here means
the guard refuses a file that exists in the working tree but is not committed,
so a local pass and a fresh-checkout pass agree.

| Guard | Fails closed on untracked input before | after |
| --- | --- | --- |
| `docs/claims.json` cited receipts | yes (r22 fix) | yes |
| `docs/claims.json` locations and manifest | no | yes |
| `docs/superseded-receipts.json` receipt, target, `declared_in`, registry | yes | yes |
| `docs/perf-numbers.json` receipt | **no** — Defect A | yes |
| `docs/perf-numbers.json` `count_of` source and generated file | no | yes |
| `conformance/manifest.json` and its vectors/mirrors | only via a stale manifest (the generator walks the tree) | yes, with the file named |
| `conformance` packaging (`scripts/package-conformance.sh`) | no: it copied whatever was in `conformance/` and `WeftCodec/conformance/v1`; the court image has no git, so the in-court check cannot run | yes: the script refuses an untracked staged file where git exists, and `scripts/conformance_bundle_court.sh` runs the same gate on the host before entering the court (a probe file in `conformance/fuzz/v1` is refused there) |
| `bench/bundles/` (`scripts/package-receipts.sh`) | yes: a dirty or untracked work tree in any of the four repositories aborts packaging; `--allow-dirty` is an explicit override | unchanged |
| spec-drift export list | only via the missing-mirror report | yes, directly |
| `scripts/verify-receipts.sh` | not applicable: it reads the extracted bundle and checks revisions with `git cat-file`, never working-tree evidence | unchanged |
| long description (`python/README.md`) | only at the clone (the file is absent) | yes, directly |

**Residual.** The tracked rule answers "is it committed", not "does it support
the number": a receipt committed at the wrong revision, or a claim whose prose
disagrees with its receipt, is still a review duty. Guards skip the rule outside
a git work tree by design; the packaging court records `worktree=unknown`
because its image has no git, which is why the bundle gate also runs on the
host. `git status --porcelain` does not list ignored files, so a receipt moved
under an ignore pattern would escape `package-receipts.sh` (no such pattern
exists today).

## 2026-11-13 — X06/X07/X08/N08/N09: one budget, one handle cap, packed races, restore cost

Parent `staging` `4d55d74` (code+receipts) plus this docs commit; WeftFS-Go
`d2fd5dd`. Not pushed. Receipts: `restore-budget`, `handle-cap`, `packed-races`,
`restore-cost`, `stage-budget`, all stamp `20261113T125506Z` at revision
`5b49da3f`, all checks pass. `scripts/resource_courts.sh` is registered in
`scripts/verify-all.sh` as `resource_courts` (reduced: 2 reps, sizes 1 and 10;
environment-only skip path).

* **X06** `src/stage_budget.rs`: one admission object (FIFO tickets, worker cap,
  in-flight byte budget, oversize-single, RAII release, per-request and
  budget-wide cancellation). The restore admits one group per tensor covering
  its tensor worker, shard workers and tile workers; widths are clamped
  deterministically (shards first) and `effective_widths()` is the pure
  function behind them. `RestorePipelineOptions` gains `budget`/`cancel`/`stage`;
  `::budgeted(budget)` is the measured default (2 shard workers,
  `max(1, cap/2)` tensor workers). Tests `tests/restore_budget.rs` 8/8.
* **X07** `ReadHandlePool` caps simultaneously active handles
  (`MAX_ACTIVE_HANDLES = 64`, RAII lease, `0` = pre-X07 control), reported as
  `observed_max_active_handles`/`cap_waits`. Go had the same unbounded path;
  `WeftFS-Go/store/read_pool.go` gains the same cap with
  `SetReadPoolActiveCap`/`Options.ReadPoolActive`. Tests
  `tests/handle_cap.rs` 6/6 and `store/read_pool_active_test.go`.
* **X08** `tests/packed_races.rs` states the discovery invariant and runs five
  scenarios (racing seal/catalog/commit with readers and same-handle
  read-backs; aborted group; real `abort()` mid-seal; GC with readers and a
  writer; cancelled reader), each ending `verify --full` clean.
  `store/packed_races_test.go` adds both cross-language directions. No product
  defect found; two court defects (a flat handle cannot read a packed store; a
  reader must snapshot the published list before it opens) are recorded.
* **N08** new `tensor_decode`/`logical_root` phases; a whole-tensor read no
  longer copies the decoded buffer (`WEFT_TENSOR_READ_COPY=1` control) and the
  report SHA-256 moved to the decode worker (`WEFT_RESTORE_WRITER_SHA=1`
  control). Both are measured as **not** wins at 100 x 1 MiB (268.4 / 268.0 /
  268.1 ms) and are published as such. The dominant term is the mode-9 decode;
  a single 64 MiB tensor restores 657.7 ms at 1 shard worker vs 381.4 ms at 2
  (1.72x), which is why the budgeted default uses shard parallelism.
  `examples/restore_cost_court.rs`; `bench/RESULTS-resource-courts.md`.
* **N09** `TensorPipelineOptions::budget` charges encode workers and worker
  bytes to the same `StageBudget`. Stress arms at caps 1/2/4/8, an oversize
  tensor, twelve cancellation rounds, a maintenance thread and concurrent
  read+write; high-water marks never exceed the cap, nothing is lost, no stage
  is starved. Overlap is 1.02x sequential at cap 4 and 0.88x at cap 8 — reported
  as a mixed result, not a win. `docs/STAGE-BUDGET.md`.

## 2026-11-13 — X31: oversize admission, cancelled tickets and the ordered-pipeline deadlock

Parent `staging` `a6ac2b2` (stage budget) and `a6ab12e` (ordered pipeline) plus
this evidence commit. Not pushed. The primitive is **unreleased**: `4d55d74` is
not on `main` (`v1.4.2` predates it), so no published signature or format
changed and there is no compatibility burden.

**Defects.** An independent session reported two broken tests at `0a7beb7`.
Both were product defects, and reproducing them found two more:

1. **Oversize did not run alone.** An oversize grant stored `bytes: 0`, so the
   byte test passed for every later request and they were admitted beside it.
   The test asserting isolation failed; the module doc claimed a property the
   code did not implement.
2. **The reservation test required its own deadlock.** It joined a waiter whose
   bytes were held until after the join. `release_workers()` was correct; the
   test asserted the opposite of the contract.
3. **A cancelled non-head waiter left a FIFO hole.** `abandon` advanced the
   cursor only for a head cancellation. A cancelled middle ticket was
   forgotten, the cursor later pointed at a ticket nobody owned, and every
   later request waited forever. Reproduced with a public-API test at `0a7beb7`:
   `worker did not finish in 5s: FIFO hole`.
4. **Oversize exclusivity deadlocked the ordered restore pipeline.** Once
   defect 1 was fixed, `tests/restore_budget.rs` hung intermittently: the
   pipeline reserved staging space in input order but called `admit` in worker
   order, so a later-index oversize grant could hold the budget while the writer
   waited forever for an earlier index blocked in admission. Committed hang log:
   `bench/receipts/stage-budget-prefix-pipeline-hang-20261113T221305Z.log`.

**Contract** (now normative in `docs/STAGE-BUDGET.md`). An oversize request is
granted only into a completely idle budget (`active_workers == 0` and
`inflight_bytes == 0`), claims no byte budget (so the published high-water mark
still respects the bound), and holds an `exclusive` flag until its `Admission`
is dropped, including during unwinding. Strict FIFO prevents starvation by a
stream of small requests. The byte reservation belongs to the request:
`release_workers()` reduces only the worker count. Abandoned tickets are
recorded and skipped when the cursor reaches them. The liveness argument is
stated as two invariants (every blocker is granted at a strictly smaller ticket
and is not waiting in `admit`; nobody waits while holding capacity it could
release), so the wait-for graph is acyclic; cancellation strictly shrinks the
queue and `cancel_all()` releases everything. There is deliberately no
admission timeout.

**Fix.** `src/stage_budget.rs`: `exclusive` lifetime, oversize idle grant, an
`abandoned` ticket set with cursor skipping, and the doc contract. There is no
API break; `Admission` remains the sole release path and `release_workers()`
keeps its meaning. `src/restore_pipeline.rs`: `Queue::next_to_admit` is a
second input-order turn, held only across the `admit` call (never across the
queue mutex), so the budget FIFO is the writer's cursor order; a waiting worker
does not hold the lock, and an aborted pipeline releases waiters. Three unit
tests pin the turn. `tests/restore_budget.rs`: the cancelled-waiter test now
waits until the second worker has entered `admit` (it was already racy before
this work: it failed in a pre-fix repetition run recorded by the retained
failing suite receipt `bench/receipts/stage-budget-suites-20261113T221717Z.json`).

**Proof.** `scripts/stage_budget_court.sh` repeats the module at thread caps 8,
2 and 1 and the ordered-pipeline suite at cap 4, every run through `timeout
120`; a hang is a recorded 124. Receipt
`bench/receipts/stage-budget-fix-20261113T224055Z.json` at `a6ab12e`:

<!-- BEGIN GENERATED:stage-budget-fix-status (scripts/perf_numbers.py) -->
X31 (2026-11-13): **120** repetitions (**90** of the **13**-test
`stage_budget` module at caps 8/2/1, **30** of the **8**-test `restore_budget` suite at cap 4) produced
**0** failures and **0** timeouts (a hang is exit 124, recorded, never a pass).
<!-- END GENERATED:stage-budget-fix-status -->

Full suites at the same revision are recorded in
`bench/receipts/stage-budget-suites-20261113T224133Z.json` and published in
`bench/RESULTS-stage-budget-fix.md`; the Rust suite has no ignored tests, and
the two FUSE `#[ignore]`s are pre-existing environment blocks (CAP_SETUID and a
kernel+fuser direct-I/O EIO), not filters added here.

**Numbers.** The resource courts were re-recorded at the same revision
(`bench/receipts/restore-budget-20261113T225001Z.json` and the four beside it,
all checks pass) and the old/new comparison is
`bench/receipts/stage-budget-delta-20261113T225001Z.json`. The unbudgeted X06
control arm — which never calls `admit` and does not use the ordered pipeline —
moved by more than any budgeted row, so no delta in this court is attributable
to the patch; the generated figures and the full reading are in
`bench/RESULTS-stage-budget-fix.md`. The generated numbers in
`bench/RESULTS-resource-courts.md` were regenerated, and the hand-written `+21 %`
in its generated region is now a computed ratio. The first passing repetition
receipt (`…-20261113T221305Z.json`, before defect 4 was found) is retained and
superseded.

**Not claimed.** One host and warm cache; the deltas are not a latency claim. No
Go equivalent exists: `WeftFS-Go` has no stage-admission primitive (quota
admission is a per-commit byte budget). Physical power loss, fleet and cloud
remain unclaimed.

## N04 / T030 / T038 / T081 (2026-11-14, staging)

**N04 — the writer pipeline on a remote object backend.** The R21 pipeline now
knows how a backend expresses durability (`StorageBackend::
durability_is_provider_ack`; S3, GCS and Azure are provider-ack, local /
immutable-object / POSIX are file-barrier, a split store follows its blob
plane). On a provider-ack backend the publisher records acknowledged bytes,
issues no barrier, and claims none; the ordering rule is unchanged. A real
deadlock was found and fixed by the adverse court: the publisher returned on a
publication error without closing the early-barrier queue, so the scope joined a
parked thread forever. The six-arm court plus four pipelined rows runs on the
pinned MinIO in the court, with seven interleaved repetitions, and the adverse
court proves provider abort and lost acknowledgement both fail closed and retry
to the reference head. Evidence and every loss:
[`bench/RESULTS-object-pipeline.md`](../bench/RESULTS-object-pipeline.md).

**T030/T038/T081 — one deployed distributed filesystem.** CephFS is running:
Ceph `19.2.0 (squid)` from the pinned demo image, one mon, one mgr, one
bluestore OSD and one MDS, mounted with `ceph-fuse` in a second hard-limited
container (the court kernel has no `ceph.ko`), and the same A/N/B/C/D/E court
runs on that mount with no WeftFS FUSE in the data path. JuiceFS and GlusterFS
were not attempted because CephFS worked. Evidence, the deployment record and
every loss: [`bench/RESULTS-cephfs.md`](../bench/RESULTS-cephfs.md). The
deployment is **single-node** and is labelled as such everywhere; it is not a
fleet, clustered, cloud, media or power-loss result.

**Not claimed.** No cloud or live-provider run; the MinIO and Ceph servers are
loopback containers on one host; the pipelined writer is a store API, not a
shipped CLI verb; restore is unaffected by N04; the crash phases are process
faults. `verify-all.sh` was not re-run for these two courts (the cited receipts
are the courts themselves), so the aggregate count above still names the last
full run.

## EKBLOOM Context Court (2026-11-14) — the manifested-decision direction is retired

A fresh court, with its own contract (`bench/CONTRACTS-ekbloom-context.md`,
committed before measurement), asks whether EKBLOOM's manifested machinery can
accelerate the context plane's **reasoning** — certified compatibility
decisions — rather than tensor bytes. It does not revise or rehabilitate the
retired tensor-codec trial.

<!-- BEGIN GENERATED:ekbloom-context-status (scripts/perf_numbers.py) -->
**Direction: retired.** E p99 **138.19 µs** against the stronger conventional cache's **33.87 µs** (**4.08×**); E p50 **18.19 µs** against M **11.39 µs**. E retains **1,001,392 B** of the **1,048,576 B** budget (M retains **1,048,576 B**). The insufficient-key control produced **1,806** false acceptances, so G1 is not vacuous. Over **72,000** measured requests per arm (stamp 20261114T100000Z, revision 29ae91d0ec45; `bench/receipts/ekbloom-context-20261114T100000Z.json`).
Scale: bounded key-sufficiency certificate **29.249 ms** at 1,000,000 keys; EKBLOOM table build **1,558.1 ms** and slot lookup **1,784 ns** p50 versus a conventional index at **30 ns** (**59.5×**); quadratic `first_conflict` **398.56 ms** at only 64,000 keys. Lifecycle: **2,000** replayed requests, **0** mismatches after deleting the manifestation (stamp 20261114T100000Z, revision 97a79d8254c4).
<!-- END GENERATED:ekbloom-context-status -->

G1 (zero incorrect acceptances) and G4 (correctness under change) hold; G3
(equal retained-byte budget) holds; G2 (p99 margin) fails. Under the
retirement rule the manifested-decision direction retires: no decision wire
format, no shipped decision cache, no planner slot, no `ekbloom` dependency.
The mechanism was real — the table was built, verified, read through the
pinned crate's caller-buffer path, and every hit compared the full 144-byte
sufficient key — but it is slower and larger per useful entry than an
exact-keyed in-memory cache at the same retained-byte budget. The lifecycle
phase deleted the manifestation, replayed 2,000 requests, and reproduced every
verdict; the manifestation is collectable and never load-bearing. Full results,
every loss and the reproduction commands:
[`bench/RESULTS-ekbloom-context.md`](../bench/RESULTS-ekbloom-context.md).

Receipts: `bench/receipts/ekbloom-context-20261114T100000Z.json` (decisions),
`...-scale-20261114T100000Z.json` (1k/100k/1M key-sufficiency, EKBLOOM table
and conventional index), `...-lifecycle-20261114T100000Z.json`. The court is
registered in `verify-all.sh` as `ekbloom_context`; the cited aggregate above
is unchanged until that aggregate is rerun.
