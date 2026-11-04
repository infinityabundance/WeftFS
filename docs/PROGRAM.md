# WeftFS program of record — full scope, no wedges

**Status:** Binding implementation plan. Every item below is in scope. The
ordering is dependency-driven, not a reduction of scope. Items marked
`specified` are normative design without implementation yet; `implemented`
means built and tested in the Docker courts. Nothing is claimed beyond its
evidence.

## A. Demos

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| A1 | Context Views as mounts: one store, ≥3 simultaneous mounts rooted at typed contexts (`by:task=...`, `by:model=...`, `at:<timestamp>`); different listings; every file `sha256` matches its declared root | Context Root, mount `--context` | **implemented** — `bench/receipts/context-mounts-20261103T210625Z.json` (6/6 pass) |
| A2 | Reproducible Agent Replay Bundle: one `weft-receipt.tar.zst` with context graph, codec snapshots, exact replay command; Rust writes, Go replays byte-exactly with no code sharing | A1, replay/export | **implemented** — `bench/receipts/replay-bundle-20261103T210824Z.json` (4/4 pass; bundle sha256 `940579db…`) |
| A3 | Ambiguity Court: selector matching ≥2 distinct content roots; live query returns the full set; singular API fails closed with `Ambiguous`; FUSE never silently picks | ambiguity errors (exist), court | **implemented** — `bench/receipts/ambiguity-20261103T210633Z.json` (4/4 pass) |
| A4 | Time-Travel POSIX: many versions of one path mounted at once; historical reads exact and pinned; no withdrawal | versioned views, context mounts | **implemented, correctness-only** — `bench/receipts/time-travel-20261103T210633Z.json` (4/4 pass); no io_uring/EKBLOOM acceleration |

## B. Proofs

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| B1 | Decoder-bomb court: ≥20 adversarial vectors (≥10 GiB claims, expansion bombs, loops/cycles, depth/count overflows), Rust and Go fail closed with identical classes under hard limits; receipt with RSS | limit flags, vectors | **implemented** — 36 vectors, 36/36 class-identical, `bench/receipts/decoder-bombs-20261103T213314Z.json`; curated corpus, not fuzzing |
| B2 | Crash-consistency torture: 10,000 interrupted transactions across both frontends; Rust recovers, Go reads; no fsck; receipt | crash proxy harness | **implemented for 10,000 process-fault iterations** — `bench/receipts/crash-torture-20261104T013521Z.json`: 10,000/10,000 pass, 6,667 old + 3,333 new, 0 partial, 0 failures, 19.7 it/s. The earlier 1,000-iteration receipt is retained. F9 adds a QEMU VM power-cut proxy: 30/30 (`cache=none`) and 30/30 under a lying-fsync `cache=unsafe` device model, 0 partial, 0 durability failures (`powerloss-20261103T224453Z.json`, `powerloss-20261104T014805Z.json`). Physical power loss remains **not done**. |
| B3 | One-command receipt bundle: Dockerfile hash, corpus hash, environment, raw output, dmesg, sha256; rerunnable by a stranger | receipts (exist) | **implemented** — `scripts/package-receipts.sh` + independent `scripts/verify-receipts.sh`; dmesg unavailable without CAP_SYSLOG |
| B4 | POSIX conformance court: `fsx`-style randomized ops, `fsstress`/`pjdfstest` where available, Rust and Go, green matrix | FUSE frontends | **implemented** — self-written fsx-style court, 6/6 runs, 13,694 ops, `bench/receipts/posix-torture-20261103T213317Z.json`; `pjdfstest`/`fsstress` not available and not substituted |

## C. Numbers

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| C1 | Context-conditioned vs content-only dedup on an AI corpus (prompts, tool outputs, intermediate checkpoints, model deltas), whole-store accounting | AI codec modes, corpus | **implemented on a synthetic corpus** — current receipt `bench/receipts/ai-corpus-20261104T020943Z.json` (whole-pair-aware planner); historical `ai-corpus-20261103T220113Z.json` retained. No Meta/production data. Current relation-conditioned physical bytes 514,506 vs forced-zstd 575,808; raw pair bytes 697,014. |
| C2 | Manifested-vs-visible cost graph: visible bytes, physical/manifested space, historical read latency, dependency depth | codec accounting, bench | **implemented** — cost graph JSON/CSV beside the receipt; analysis and every loss in `bench/RESULTS-ai-v1.md` |
| C3 | POSIX matrix results with reproducible commands | B4 | **implemented** — Rust matrix in `docs/fuse-status.md` with Rust/Go status and exact boundaries; `tests/fuse_verb_matrix.rs` (fallocate modes, server-side copy, mknod/FIFO rejection, O_TMPFILE boundary, mmap, locks, timestamps) passes in the FUSE court. Rust `O_TMPFILE`/flock are binding-limited; Go implements both. |
| C4 | Comparative performance: external SQLite relational/adjacency context baseline, matched Rust/Go codec court, cold-cache concurrent fio on ext4/XFS/WeftFS, energy probe | C3, P7 | **implemented, scoped** — `bench/RESULTS-comparative-v1.md`; receipts `bench-external-20261104T000122Z.json`, `bench-codec-matched-20261103T232158Z.json`, `bench-fs-cold-20261103T232216Z.json`, `energy-probe-20261103T225547Z.json`. 2026-11-04 adds a real ZFS guest baseline (`bench-fs-cold-20261104T014727Z.json`; file-backed vdev, ARC caveat) and perf hardware counters (`perf-counters-20261104T015026Z.json`). RAPL/joule energy remains unavailable; no winner claim. |

## D. Hooks

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| D1 | Buck2 artifact-store backend: action key = typed context + toolchain version; exact rebuild; query by context | Context Root, CAS API | **implemented as a prototype adapter** — `adapters/buck2/` is a local CAS plus Buck2-compatible key/digest mapping, explicitly **not** a Buck2 plugin or REAPI gRPC server. Receipt `bench/receipts/buck2-cas-20261104T021121Z.json` (15/15). |
| D2 | Agent long-term memory: typed edges (`generated_by`, `grounded_in`, `derived_from`, `prompted_by`), mountable by task/model/time, months-later byte-exact | A1, causal closure | **implemented with simulated timestamps** — `scripts/demo-agent-memory.sh`, receipt `bench/receipts/agent-memory-20261104T021117Z.json` (7/7): 5 typed edges, 3 Context Roots, January snapshot, September writes, January mounts and bundle replay byte-exact. Real-time multi-month aging is not measured. |
| D3 | Spec-as-code: generate/validate Rust and Go types from the normative spec; CI fails on drift | spec export | **implemented** — canonical `spec/schema/identity-v1.schema`; `scripts/gen-types.sh [--check]` generates `WeftCodec/src/generated.rs` and `WeftCodec-Go/generated/generated.go`; runtime constants are re-exports; drift court wired into `verify-all`. |

## E. Strategic pillars (identity, lineage, codec, agents, reproducibility, guarantees, adoption)

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| E1 | Context Root as primary public identity; `weft://` URI is the address; paths are a compatibility projection | — | not started |
| E2 | Native causal closure; derived identity modes `bytes_only` vs `causal_exact`; minimal sufficient closure queries | E1 | not started |
| E3 | AI-native codec modes: tensor layouts (row/col/blocked/sharded/quantized), activation/gradient streams, embedding/KV-cache pages, token sequences/masks, weight deltas (LoRA/full/quantized), structured semantic delta | E1 | **partial** — `TENSOR` (6, row-major little-endian), `TOKEN_SEQUENCE` (7, u64 zigzag-delta LEB128), and `QUANTIZED_TENSOR` (8, affine per-tensor i8/u8 with exact f64 scale bits and i64 zero point) implemented and cross-language byte-identical; every other layout/stream/delta mode is `specified, not implemented` with no wire value (`spec/WEFT-AI-CODEC.md` §5) |
| E4 | Agents as capability principals: allowed roots/predicates/namespaces, budgets, revocation, no ambient authority | E1, auth | partial (uid/gid + ACL) |
| E5 | Training-ready world slices: exact, capability-checked materialization of datasets/models/configs/evals, fail closed before compute | E1–E4 | not started |
| E6 | Guarantees: bit-identical cross-impl CI, machine-checked invariants, resource-in-identity, zero silent degradation, public AI workload court | all | **partial** — F9 model-checks the commit/recovery abstraction in TLA+ (corrected model pass; flawed model counterexample) and adds a QEMU VM power-cut proxy; no refinement proof, and checkpoint/GC/capability/selector/multi-writer invariants remain test-only. Physical power loss and full CI remain outstanding. |
| E7 | Adoption surfaces: checkpoint+delta format, dataset versioning/subset materialization, agent memory, experiment reproducibility | E3–E5 | not started |

## Implementation order (each stage ends with tests and a receipt)

1. **F0 — Program foundation:** normative `WEFT-CONTEXT-ROOT.md`,
   `WEFT-CAUSAL.md`, `WEFT-CAPABILITIES.md`; Context Root WCB schema; `weft://`
   grammar/canonical bytes; spec-drift check. (E1, E2 start, D3 start)
2. **F1 — Context Root + causal closure in Rust and Go:** CLI/API, canonical
   vectors, identical IDs, causality modes, cross-language tests.
3. **F2 — Capability contexts:** enforcement in resolver, counts/evidence
   filtering, budgets; cross-language tests. (E4)
4. **F3 — Context mounts + ambiguity + time travel:** `--context`/`--at` mount
   roots in Rust and Go, A1/A3/A4 + receipts, sha256 verification. **Implemented**
   (2026-11-03); receipts under `bench/receipts/`.
5. **F4 — Replay bundle:** export/import, A2, one-command rerun from Go.
   **Implemented** (2026-11-03) as part of F3.
6. **F5 — Proof courts:** B1/B2/B3/B4, receipts. **Implemented** (2026-11-03);
   B2 ran 10,000 process-fault iterations on 2026-11-04 (0 failures, 19.7 it/s),
   not physical power loss.
7. **F6 — AI codec v1:** `WEFT-AI-CODEC.md`, one real tensor mode + one token
   mode, cross-language, vectors for every implemented mode. **Implemented**
   (2026-11-03; QUANTIZED_TENSOR added 2026-11-04): `TENSOR` (6),
   `TOKEN_SEQUENCE` (7), and `QUANTIZED_TENSOR` (8); semantic delta and all
   other brainstormed modes are `specified, not implemented` with no wire value.
8. **F7 — AI corpus + cost graph:** C1/C2, real measurements with losses.
   **Implemented** (2026-11-03) on a committed synthetic corpus; results and
   caveats in `bench/RESULTS-ai-v1.md`. (E3, C)
   A comparative performance pass (2026-11-03) adds an external schema-indexed
   SQLite relational/adjacency baseline, a matched Rust/Go codec court, and a
   guest cold-cache concurrent `fio` court on ext4/XFS/Rust/Go; results, every
   loss, and blockers (ZFS absent, `perf`/RAPL unavailable) are in
   `bench/RESULTS-comparative-v1.md`.
9. **F8 — Hooks:** Buck2 CAS adapter, agent memory example, generated types and
   drift CI. (D1–D3, E7) **Implemented** (2026-11-04) as a prototype adapter,
   simulated-timestamp agent-memory demo, and generated identity types with a
   drift court. Buck2 plugin integration and real-time aging remain out of
   scope.
10. **F9 — Formal guarantees:** TLA+/Alloy or exhaustive bounded + machine
    checked invariants; bit-identical CI; AI workload court; release.
    **Partly implemented** (2026-11-03): `verification/tla/` checks the bounded
    commit/recovery state machine (`NoPartial`, `HighestValid`,
    `NoLostDependency`, `Durability`, `ReturnedObjectsDurable`,
    `ReplayIdempotent`) and includes a deliberately flawed model with a TLC
    counterexample; `scripts/powerloss_court.sh` runs a QEMU VM power-cut proxy
    with Rust and Go verification (30/30 published iterations, 0 partial, 0
    durability failures) and a `cache=unsafe` lying-fsync device model (30/30,
    0 partial, 0 durability failures; not physical media power loss). Still
    outstanding: checkpoints/GC/capability/selector/multi-writer models, a
    refinement proof, physical power loss, and the remaining release/CI items.

## Non-negotiables

- Every claim carries command, environment, fixture, revisions, raw output.
- Rust and Go remain independent; no FFI/subprocess in production paths.
- Graph cycles are legal; codec dependency graphs stay acyclic.
- Selector cardinality is always exposed; singular access fails closed.
- Context Root is a versioned, canonical, typed expression with an explicit
  snapshot and resolver/policy version; it is not a mutable tag.
- No stub modes. A spec without an implementation is labelled `specified`, not
  `implemented`.
