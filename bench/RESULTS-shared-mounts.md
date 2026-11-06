# Shared multi-mount daemon — measured (R7)

Measured 2026-11-06 in the OOM-protected Docker FUSE court at WeftFS
`185d008`. One court receipt:
`bench/receipts/shared-mounts-20261106T082042Z.json` (19/19 checks pass), with
the four raw demo receipts it assembles:

| case | raw receipt |
|---|---|
| per-daemon at 3 GiB | `bench/receipts/shared-mounts-per-daemon-3g-20261106T082042Z.json` |
| shared at 3 GiB | `bench/receipts/shared-mounts-shared-3g-20261106T082042Z.json` |
| shared at 512 MiB | `bench/receipts/shared-mounts-shared-small-20261106T082042Z.json` |
| per-daemon at 512 MiB | `bench/receipts/shared-mounts-per-daemon-small-20261106T082042Z.json` |

Raw per-case logs and results TSVs are stored beside each receipt as
`.log.zst` / `.results.tsv.zst`.

## What was measured

`bash bench/scripts/shared_mounts_court.sh 100` runs the same deterministic
corpus four times. The corpus is built by the construction oracle
(`examples/context_mount_corpus.rs`) for exactly 100 mounts:

| item | value |
|---|---|
| corpus files | 200 (`/corpus/f-000.bin` … `f-199.bin`) |
| model objects | 25 |
| grounding documents | 10 |
| typed relations | 599 |
| Context Roots | 100 (50 `by:task`, 25 `by:model`, 25 `by:derived`) |
| one snapshot (`all`) | yes |

Every case then verifies, per mount:

* the listing digest against the construction oracle, and that all 100
  digests are distinct;
* every listed file's SHA-256 against its declared content hash (425 files);
* one `weftfs cat` byte comparison and one ContentId comparison;
* `context resolve` on a deterministic sample, compared id-for-id with the
  oracle;
* read-only denial on the first two mounts;
* clean unmount of every mount and zero surviving processes.

`per-daemon` starts 100 `weftfs mount --context … --foreground` processes.
`shared` starts one `weftfs mount-many --spec …` process with 100 views. The
binaries are built once in a roomy container (`WEFT_DEMO_BUILD_ONLY=1`); every
case runs with `WEFT_DEMO_SKIP_BUILD=1`, so the memory figures measure the
mount service, not the compiler.

The corpus, store, and verification commands are identical in both modes, so
the listing digest over all 100 per-view digests must be identical. It is:
`1d30af9ccebcab723acfdc0cb9ceee56a6fe93a73c8be0220590593eb714566e` in
per-daemon at 3 GiB, shared at 3 GiB, and shared at 512 MiB.

## Environment and limits

* Court image `weft/fuse-court:1`, image id
  `sha256:1a0e1337179d9bcc5c05efd00b94b40b674d9586a344dacb977625a1b3273157`
* Kernel `Linux 7.3.1-1-cachyos x86_64`; host `nproc` = 16
* The court's hard limits are unchanged everywhere: memory == swap, CPUs 4,
  pids 1,024, RLIMIT_NPROC 2,048, bounded `/tmp` (1 GiB),
  `no-new-privileges`, `cap-drop=ALL`. The only override is the explicitly
  recorded small memory limit (512 MiB) for the two small-limit cases.

## Results

| case | memory | processes | mounts verified | files SHA-256 | distinct digests | summed RSS | max RSS | threads | cgroup peak | OOM kills | pids.events max | all mounted | p50 / p95 / max mount |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| per-daemon | 3 GiB | 100 | 100/100 | 425 | 100 | 1,963,056 KiB | 23,524 KiB | 692 | 1,351,827,456 B (1.259 GiB) | 0 | 0 | 4,428 ms | 2,632 / 4,109 / 4,159 ms |
| shared | 3 GiB | 1 | 100/100 | 425 | 100 | 27,692 KiB | 27,692 KiB | 102 | 69,402,624 B (66.2 MiB) | 0 | 0 | 4,064 ms | 4,014 / 4,057 / 4,062 ms |
| shared | 512 MiB | 1 | 100/100 | 425 | 100 | 27,896 KiB | 27,896 KiB | 102 | 73,646,080 B (70.2 MiB) | 0 | 0 | 4,040 ms | 3,991 / 4,034 / 4,039 ms |
| per-daemon | 512 MiB | 100 | **51/100** | 210 | 51 | 624,820 KiB | 23,340 KiB | 546 | 536,891,392 B (limit) | **49** | 0 | 262,862 ms | 141,112 / 256,350 / 257,623 ms |

Ratios at the same N = 100 (per-daemon → shared, 3 GiB):

* cgroup peak: 1,351,827,456 B → 69,402,624 B, **19.5x smaller**;
* summed process RSS: 1,963,056 KiB → 27,692 KiB, **70.9x smaller**;
* threads: 692 → 102, **6.8x fewer**; processes: 100 → 1;
* file SHA-256 checks and listing digests: identical (425 files, 100 distinct
  digests, same digest over the digest set).

Per-daemon RSS `max` is one 23 MiB daemon; summed RSS double-counts shared
read-only pages (the same binary and standard library are mapped by every
process), which is why the sum (1.87 GiB) exceeds the cgroup peak (1.26 GiB).
The cgroup figure is the honest, de-duplicated number and is the one to use.

## The smallest limit at which the same N completed

The shared run completed 100/100 mounts, all 425 file hashes, all 100 distinct
digests, and clean unmounts at **512 MiB**, with peak 73,646,080 B
(70.2 MiB) — **441.8 MiB of headroom below the limit**.

The per-daemon run at the same 512 MiB mounted **51 of 100** views and lost
**49 daemons to cgroup OOM kills** (`memory.events oom_kill`), then failed the
verification steps: 210 of 425 files checked, 51 distinct digests. This is the
measured reason the shared server exists: at 512 MiB per-daemon cannot hold
the same N, and shared mode uses 14% of the limit.

512 MiB is the limit chosen from this measurement (`WEFT_SHARED_LIMIT`, default
`512m`). It is deliberately not pushed to the exact edge; the court asserts
`peak <= limit`, zero OOM kills, and zero `pids.events max` events.

## Losses and other honest findings

1. **Time to the first view is worse.** The first shared view appears at
   3,964 ms versus 523 ms for the first per-daemon mount (7.6x later), because
   one process resolves all 100 Context Roots and then creates all 100 mounts
   sequentially before the last view is available. The last view appears at
   4,062 ms versus 4,159 ms, and time-to-all-mounted is 4,064 ms versus
   4,428 ms, so the *completion* time is not worse. Per-view p50 is higher
   (4,014 ms vs 2,632 ms) because the distribution is compressed at the end.
   The natural fix (resolve and mount one view at a time) is not implemented.
2. **One backend mutex.** Every durable read goes through `Store`'s backend
   mutex, so views serialise physical I/O inside the process. Independent
   daemons overlap it. The court's verification is largely sequential, so this
   is not exercised here; a concurrent large-read workload could be slower in
   shared mode. Not measured.
3. **One failure domain.** A panic or fatal error in one view ends the process
   and therefore every view. `tests/shared_mounts.rs` drives invalid and
   write-like requests through a view and asserts no panic reaches the server,
   but this remains a real difference from independent daemons.
4. **Read-only by construction.** `mount-many` has no writable mode and its
   spec schema has no `read_only`/writable field, so a spec cannot turn a view
   writable (`shared_spec_tampering_does_not_start_a_writable_view`). Writes
   remain on the single-mount path.
5. **No pins.** A shared view cannot pin objects or content (pins need
   `&mut Store`) and does not need to: this process runs no writer or GC over
   the shared store. A single read-only mount's pins are likewise process-local
   and invisible to another process's GC, so no cross-process guarantee was
   lost.
6. **Snapshot pinning is at open.** Views without a snapshot see the state
   loaded at open and never re-read commits made by another process while the
   server runs — the same as a single read-only mount.
7. **One host, synthetic corpus, warm page cache.** No fleet, multi-host, or
   production-workload claim. The 1,000-mount context is in
   `bench/RESULTS-context-mounts-1000.md`; the shared server was not run at
   1,000 in this court (it would fit, on this evidence, but has not been
   measured).

## Reproduce

```
bash bench/scripts/shared_mounts_court.sh 100
```

The court is a host orchestrator (like `scripts/fuzz-diff.sh`): each case runs
in its own OOM-protected FUSE container, and the host only assembles the
receipt. It writes `bench/receipts/shared-mounts-<UTC>.json` and `.log`, plus
the four raw demo receipts and logs.

Scale, limits, and mode are overridable without changing the code:
`WEFT_SHARED_N`, `WEFT_SHARED_LIMIT`. The demo itself accepts
`WEFT_CONTEXT_MOUNTS`, `WEFT_CONTEXT_MOUNTS_MODE=shared|per-daemon`,
`WEFT_CONTEXT_CORPUS_BUILD=cli|harness`, `WEFT_DEMO_SKIP_BUILD`,
`WEFT_DEMO_BUILD_ONLY`, and `WEFT_WAIT_MOUNT_TRIES`.
