# WeftFS v0.2.0 — research preview

Second coordinated release of `WeftCodec`, `WeftCodec-Go`, `WeftFS`, and
`WeftFS-Go`. All evidence was produced in OOM-protected Docker courts; nothing
was built, tested, mounted, benchmarked, or booted on the host.

## New in v0.2.0

- **QUANTIZED_TENSOR** (mode 8): i8/u8 packed tensors with shape, exact f64
  scale bits and i64 zero-point; byte-identical Rust↔Go frames, 3 valid and
  8 reject vectors; mode 9 reserved.
- **Buck2 CAS prototype** (`adapters/buck2/`): local CAS/ActionCache keyed by a
  typed Context Root (action type + toolchain + input digests), REAPI-style
  `sha256:<hex>` blobs, exact rebuild/miss/query/verify; labelled a prototype,
  not a Buck2 plugin or REAPI gRPC server.
- **Spec-as-code**: canonical identity schema, generated Rust/Go constants and
  record types, drift court in `verify-all`.
- **Agent memory demo**: typed prompt/model/tool/dataset/output edges, task/
  model/time Context Roots, January snapshot, September writes, months-later
  Go-only replay with exact bytes and lineage.
- **Whole-pair-aware context planner**: ai-v1 physical 514,506 bytes (−10.65%
  vs forced zstd), pair bytes 697,014 (45.96%), 8 remaining pair losses (all
  retained bases), 24 rejected candidates.
- **Reliability/performance**: 10,000 crash iterations (6,667 old / 3,333 new,
  0 partial); VM lying-fsync device model 30/30 with 0 data-loss events; real
  OpenZFS baseline (file-backed vdev, ARC caveat); perf counters recorded.
- **POSIX/FUSE parity**: Rust fallocate modes, server-side copy_file_range,
  mknod/FIFO/device rejection, shared mmap, locks, timestamps, lseek, xattrs,
  rename exchange/noreplace, statfs, open-unlinked; 86 Rust tests, 74 Go tests.
- **Verification**: 18/18 courts pass, `verify-20261104T022254Z.json`.

## Blockers and non-claims

- Physical media power loss, host cache loss, and a genuinely lying physical
  disk are not modeled. The VM `cache=unsafe` run is a device-model proxy.
- True energy/joules are not reported: RAPL is unreadable in host/container/
  guest; perf counters and CPU user/sys are not energy.
- ZFS reads are ARC-served, not cold; whole-disk vdev and Btrfs baselines are
  absent.
- Kernel 7.3 same-inode direct-I/O and mmap-then-write EIO remain; documented
  with a reproducer.
- Rust `O_TMPFILE`/daemon flock are binding limits; Go PUNCH_HOLE is
  content-only without physical reclamation; ioctl is absent.
- Buck2 is a prototype adapter; generated types cover identity records, not the
  entire store.
- AI modes other than TENSOR, TOKEN_SEQUENCE, QUANTIZED_TENSOR are specified
  only, with no wire values.
- Context-conditioned encoding still loses on some whole-pair cases; the
  receipt publishes the 8 retained-base losses.
- 4 formal model checks abstract commit/recovery, not implementation
  refinement.

## Reproduce

```sh
scripts/verify-all.sh
scripts/docker-court.sh fuse "bash scripts/showcase.sh"
scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
scripts/docker-court.sh fuse "bash scripts/demo-agent-memory.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_decoder_bomb_court.sh"
scripts/run_tla.sh
```

Authoritative gaps: `docs/STATUS.md`, `docs/PROGRAM.md`,
`bench/RESULTS-v1.md`, `bench/RESULTS-ai-v1.md`,
`bench/RESULTS-comparative-v1.md`, `verification/README.md`.
