# WeftFS v0.1.0 — research preview

This is a **research preview**, not a production storage system. It is the first
coordinated release of four independent implementations:

- `WeftCodec` (Rust) and `WeftCodec-Go`: the WeftCodec v1 format, including
  TENSOR and TOKEN_SEQUENCE experimental modes.
- `WeftFS` (Rust) and `WeftFS-Go`: the portable store, Context Roots, causal
  closure, capabilities, FUSE frontends, and handoff courts.

All evidence was produced in OOM-protected Docker courts. No build, test, mount,
benchmark, or VM run was performed on the host.

## Verified in this release

- **14/14 verification courts pass**: codec suites, store suites, FUSE suites,
  bidirectional codec cross-decode, spec-drift, and Context Root cross-language
  canonicalization. Receipt: `bench/receipts/verify-20261104T000230Z.json`.
- **TLA+/TLC** checks commit/recovery safety at `MaxGen=8` (615 states, depth
  68): `NoPartial`, `HighestValid`, `NoLostDependency`, `Durability`,
  `ReturnedObjectsDurable`, `ReplayIdempotent`. A deliberately flawed model is
  refuted with a counterexample. The implementation is **not** proven refined.
- **VM power-cut court**: 30/30 iterations, 28 old / 2 new, 0 partial, 0
  cross-implementation disagreements; a never-fsynced control is lost every
  time. This is a VM power-cut proxy, not physical media power loss.
- **Decoder bombs**: 36 adversarial vectors, identical error classes in Rust and
  Go, <5 MiB peak RSS. Found and fixed a real Rust 4 GiB preallocation bug.
- **POSIX torture**: 13,694 randomized operations over 3 seeds, both directions,
  exact cross-implementation state.
- **Crash torture**: 1,000 process-fault iterations, 0 partial states.
- **Context demo**: three simultaneous mounts (Rust + Go) of one store, different
  listings, every file hash verified, one shared chunk for byte-identical
  objects. Ambiguity fails closed. 100-version time travel is byte-exact.
- **Replay bundle**: Rust writes, Go independently verifies/materializes.
- **AI modes**: exact cross-language TENSOR (mode 6) and TOKEN_SEQUENCE (mode 7).
- **Comparative evidence**: SQLite relational/adjacency baselines, ext4/XFS
  cold/warm/concurrent fio, matched Rust/Go codec court, every loss published.

## Known blockers and non-claims

- Physical media power loss and lying `fsync` are not modeled; the VM rig cuts
  power to a guest, not to a physical device cache.
- ZFS baseline was not run (`zfs`/`mkfs.zfs` unavailable); energy/RAPL and
  `perf` were unavailable. CPU user/sys is reported, not called energy.
- 10,000 crash iterations were not run (1,000 measured; projection documented).
- Same-inode concurrent direct FUSE I/O can return `EIO` on kernel 7.3 with
  fuser 0.15.1; documented with a reproducer, not hidden.
- Most AI modes (blocked/quantized, activations, KV-cache, weight deltas,
  semantic deltas) are specified only; no wire values.
- Context-conditioned encoding loses on whole-pair accounting for many synthetic
  corpus objects; the AI-corpus receipt publishes 20 pair losses and 90.14%
  pair bytes. No superiority claim is made.
- Buck2 CAS and generated types are not implemented; agent-memory demonstration
  is partial.
- TLA+ checks an abstract model, not the Rust/Go implementations.

## Reproduce

```sh
scripts/verify-all.sh
scripts/docker-court.sh fuse "bash scripts/showcase.sh"
scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_decoder_bomb_court.sh"
scripts/run_tla.sh
```

Authoritative gap lists: `docs/STATUS.md`, `docs/PROGRAM.md`,
`bench/RESULTS-v1.md`, `bench/RESULTS-ai-v1.md`,
`bench/RESULTS-comparative-v1.md`, `verification/README.md`.
