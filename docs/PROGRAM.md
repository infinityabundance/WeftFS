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
| B2 | Crash-consistency torture: 10,000 interrupted transactions across both frontends; Rust recovers, Go reads; no fsck; receipt | crash proxy harness | **implemented for 1,000 process-fault iterations** — `bench/receipts/crash-torture-20261103T213446Z.json`; 10,000 command supported but not run (measured 2.9 it/s, ~58.4 min linear projection); physical power loss not done |
| B3 | One-command receipt bundle: Dockerfile hash, corpus hash, environment, raw output, dmesg, sha256; rerunnable by a stranger | receipts (exist) | **implemented** — `scripts/package-receipts.sh` + independent `scripts/verify-receipts.sh`; dmesg unavailable without CAP_SYSLOG |
| B4 | POSIX conformance court: `fsx`-style randomized ops, `fsstress`/`pjdfstest` where available, Rust and Go, green matrix | FUSE frontends | **implemented** — self-written fsx-style court, 6/6 runs, 13,694 ops, `bench/receipts/posix-torture-20261103T213317Z.json`; `pjdfstest`/`fsstress` not available and not substituted |

## C. Numbers

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| C1 | Context-conditioned vs content-only dedup on an AI corpus (prompts, tool outputs, intermediate checkpoints, model deltas), whole-store accounting | AI codec modes, corpus | **implemented on a synthetic corpus** — `bench/receipts/ai-corpus-20261103T220113Z.json`; no Meta/production data; relation-conditioned physical bytes 176,564 vs forced-zstd 575,808 on ai-v1 |
| C2 | Manifested-vs-visible cost graph: visible bytes, physical/manifested space, historical read latency, dependency depth | codec accounting, bench | **implemented** — cost graph JSON/CSV beside the receipt; analysis and every loss in `bench/RESULTS-ai-v1.md` |
| C3 | POSIX matrix results with reproducible commands | B4 | partial |

## D. Hooks

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| D1 | Buck2 artifact-store backend: action key = typed context + toolchain version; exact rebuild; query by context | Context Root, CAS API | not started |
| D2 | Agent long-term memory: typed edges (`generated_by`, `grounded_in`, `derived_from`, `prompted_by`), mountable by task/model/time, months-later byte-exact | A1, causal closure | **partial** — typed edges and task/model/time mounts pass; long-horizon (months) retention is not measured |
| D3 | Spec-as-code: generate/validate Rust and Go types from the normative spec; CI fails on drift | spec export | not started |

## E. Strategic pillars (identity, lineage, codec, agents, reproducibility, guarantees, adoption)

| ID | Deliverable | Depends on | Status |
|---|---|---|---|
| E1 | Context Root as primary public identity; `weft://` URI is the address; paths are a compatibility projection | — | not started |
| E2 | Native causal closure; derived identity modes `bytes_only` vs `causal_exact`; minimal sufficient closure queries | E1 | not started |
| E3 | AI-native codec modes: tensor layouts (row/col/blocked/sharded/quantized), activation/gradient streams, embedding/KV-cache pages, token sequences/masks, weight deltas (LoRA/full/quantized), structured semantic delta | E1 | **partial** — `TENSOR` (6, row-major little-endian) and `TOKEN_SEQUENCE` (7, u64 zigzag-delta LEB128) implemented and cross-language; every other layout/stream/delta mode is `specified, not implemented` with no wire value (`spec/WEFT-AI-CODEC.md` §4) |
| E4 | Agents as capability principals: allowed roots/predicates/namespaces, budgets, revocation, no ambient authority | E1, auth | partial (uid/gid + ACL) |
| E5 | Training-ready world slices: exact, capability-checked materialization of datasets/models/configs/evals, fail closed before compute | E1–E4 | not started |
| E6 | Guarantees: bit-identical cross-impl CI, machine-checked invariants, resource-in-identity, zero silent degradation, public AI workload court | all | partial |
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
   B2 is 1,000 process-fault iterations, not physical power loss, and 10,000
   was not run.
7. **F6 — AI codec v1:** `WEFT-AI-CODEC.md`, one real tensor mode + one token
   mode, cross-language, vectors for every implemented mode. **Implemented**
   (2026-11-03): `TENSOR` (6) and `TOKEN_SEQUENCE` (7); semantic delta and all
   other brainstormed modes are `specified, not implemented` with no wire value.
8. **F7 — AI corpus + cost graph:** C1/C2, real measurements with losses.
   **Implemented** (2026-11-03) on a committed synthetic corpus; results and
   caveats in `bench/RESULTS-ai-v1.md`. (E3, C)
9. **F8 — Hooks:** Buck2 CAS adapter, agent memory example, generated types and
   drift CI. (D1–D3, E7)
10. **F9 — Formal guarantees:** TLA+/Alloy or exhaustive bounded + machine
    checked invariants; bit-identical CI; AI workload court; release.

## Non-negotiables

- Every claim carries command, environment, fixture, revisions, raw output.
- Rust and Go remain independent; no FFI/subprocess in production paths.
- Graph cycles are legal; codec dependency graphs stay acyclic.
- Selector cardinality is always exposed; singular access fails closed.
- Context Root is a versioned, canonical, typed expression with an explicit
  snapshot and resolver/policy version; it is not a mutable tag.
- No stub modes. A spec without an implementation is labelled `specified`, not
  `implemented`.
