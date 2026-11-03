# WeftFS

**Context-addressed storage, with byte-exact reconstruction.**

WeftFS is a research program building four independent, interoperable
implementations of one storage architecture:

| Project | Language | Role |
|---|---|---|
| [`WeftCodec`](WeftCodec/) | Rust | Lossless context-conditioned codec, normative format |
| [`WeftCodec-Go`](WeftCodec-Go/) | Go | Independent codec implementation, cross-decode conformance |
| [`WeftFS`](.) | Rust | Transactional store + Linux FUSE filesystem |
| [`WeftFS-Go`](WeftFS-Go/) | Go | Independent store + Linux FUSE filesystem |

The central question is whether **typed, versioned contextual relationships**
can be a native addressing and reconstruction primitive — distinct from paths,
content hashes, and object IDs — while retaining exact materialization, durable
POSIX access, reproducible history, and competitive performance.

This repository is the program home. It owns the normative specification
(`spec/`) and exports versioned copies to the subprojects.

## Status

Status is deliberately blunt. Nothing is claimed beyond what has been built and
tested in the Docker court.

| Phase | Deliverable | Status |
|---|---|---|
| P0 | Repositories, staging workflow, OOM-protected Docker courts, licenses | **Implemented** |
| P1 | Normative specs: model, selectors, codec format, compatibility | In progress |
| P2 | Rust WeftCodec v1 (all modes, CLI, tests) | Not started |
| P3 | Golden vectors + independent Go WeftCodec + bidirectional cross-decode | Not started |
| P4 | Rust WeftFS core (transactions, snapshots, graph, recovery) | **Implemented** (store core, no FUSE) — see [`docs/store-status.md`](docs/store-status.md) |
| P5 | Rust FUSE frontend, POSIX matrix, context views | **In progress** — frontend built; 55 court tests pass and one ownership test is `#[ignore]`d for a missing court capability — see [`docs/fuse-status.md`](docs/fuse-status.md) |
| P6 | Go WeftFS core + FUSE + storage handoff court | Not started |
| P7 | Bench courts, receipt bundle, showcase, release | Not started |

Labels used throughout: **implemented** (built and tested), **experimental**
(built, not format-frozen), **conjectured** (design claim without measurement).

## Non-negotiable engineering rules

1. **Exactness.** Every decoded byte sequence must match its declared content
   root. Hash mismatch fails closed. No lossy fallbacks.
2. **No invented evidence.** Every performance, reliability, or compatibility
   claim carries a command, environment, fixture, and raw output.
3. **Independent implementations.** Go never calls Rust. The two codecs share a
   format, not code. The two filesystems share an on-disk format, not code.
4. **Bounded resources.** Decoders enforce depth, byte, count, and work limits.
   Containers enforce hard memory/swap/pids/CPU limits.
5. **Identity separation.** A selector is a query, not an object. A hash is not
   an authorization token. Ambiguity is returned, never silently resolved.
6. **One writer.** The store is single-host, single-writer with an explicit
   `flock` ownership protocol.

## Testing: Docker only

All builds and tests execute inside minimal official images. The host runs
only `git`, `gh`, and Docker. The runner hard-codes OOM protection and refuses
unbounded values:

```sh
scripts/docker-court.sh rust "cargo test --all-targets -- --test-threads=1"
scripts/docker-court.sh go   "cd WeftCodec-Go && go test ./..."
scripts/docker-court.sh fuse "cargo test --features fuse --all-targets -- --test-threads=1"
```

Container defaults: `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
`--tmpfs /tmp:size=1m`-bounded (1 GiB), `no-new-privileges`, `cap-drop=ALL`,
and for FUSE only: `/dev/fuse` plus `CAP_SYS_ADMIN`.

## Repository layout

```
spec/          Normative specifications (single source of truth)
docs/adr/      Architecture decision records
scripts/       Docker court and spec export
docker/        Minimal court images
WeftCodec/     Rust codec (independent git repo later; directory now)
WeftCodec-Go/  Go codec
WeftFS-Go/     Go filesystem
src/, crates/  Rust filesystem implementation
tests/         POSIX, crash, snapshot, graph, interop courts
conformance/   Frozen golden and invalid vectors
security/      Threat model and audit notes
bench/         Contracts, corpora manifests, scripts, receipts
research/      Local-only research notes (gitignored)
```

## Licensing

Code: Apache-2.0. Specification and research prose: CC BY 4.0.
See `LICENSE` and `spec/` headers. Third-party notices are recorded as
dependencies are added.
