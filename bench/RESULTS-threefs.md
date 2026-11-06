# 3FS transport court — results

Receipt: `bench/receipts/threefs-20261106T232643Z.json` (raw log beside it).
Code: R22 on `staging`. Court: rust (`weft/rust-court:1`), 3 GiB / 4 CPUs /
pids 1024 / bounded tmpfs / no-new-privileges / cap-drop=ALL.

**Label: single host, synthetic fixtures, warm page cache, and a conformance
implementation of the 3FS USRBIO ABI — not a 3FS deployment.** The library
reports `hf3fs_is_hf3fs = 0`, which is recorded in the receipt.

## What was measured

Two transports over one normative layout:

- `posix` — positional `pread`/`pwrite` + `fsync` + `rename`.
- `usrbio` — hf3fs rings (`hf3fs_prep_io`/`submit_ios`/`wait_for_ios`) for the
  immutable data plane, with control state still on POSIX.

### Range reads, 300 × 256 KiB, 5 repetitions

| | median | MiB/s |
|---|---|---|
| positional, destination buffers reused | **9.899 ms** | 7,576.6 |
| batched through the ring | 11.159 ms | 6,720.9 |

Ring/positional ratio **1.127× (12.7 % slower)**. 1,500 requests were issued as
**50 submissions** — a ring-full is bounded by the 8 MiB shared region (31
requests of 256 KiB), so the count is 10 submissions per 300-request pass.
Output bytes are identical.

**Why this loses, and what would change it.** The conformance library performs a
`pread` per request inside `wait_for_ios`; it has no RDMA, no kernel bypass, and
no 3FS servers. It also cannot amortise the adapter's per-read descriptor:
`registrations = 1500` for 1,500 requests, because `read_batch` opens and
registers each file for the duration of the call. The positional side reuses its
destination buffers across repetitions while the ring side returns owned vectors
and allocates. Three separate costs, none of which is 3FS behaviour.

The receipt records these as losses rather than explaining them away. What the
court does establish is structural: correct ABI transcription (pinned by
`_Static_assert` and `const` assertions), registration cleanup on the error path,
partial-completion tolerance, short reads at EOF, past-EOF refusal, buffer
containment, and one submission per ring-full.

### Store round trip, 100 × 16 KiB files

| | POSIX transport | USRBIO transport |
|---|---|---|
| write phase | 11.31 ms | 12.28 ms (+8.5 %) |
| generation | 101 | 101 |
| snapshot root | `9dcdc31f…a0dd` | `9dcdc31f…a0dd` |
| `verify --full` | ok | ok |
| reopen + verify | — | ok, 1.69 ms |

Byte-identical commits, manifests, chunks, and indexes; a read-only reopen over
`usrbio` recovers the same head. Publication still goes through `tmp + fsync +
rename + dirfsync`, so entering the ring only changes how the staging file is
filled.

### Resources

Peak RSS 397,196 KiB. Most of it is the positional phase's 300 × 256 KiB
destination buffers plus the concatenated reference copy; it is not a claim about
the transport.

## What this court does not show

- No 3FS throughput, latency, tail behaviour, RDMA, or multi-node consistency.
- No durability claim beyond the declared `fsync`/`rename` semantics, which the
  literal string "Declared, not measured" in the capability text makes explicit
  (a test asserts that wording).
- No registration-cache optimisation: the intended production shape keeps
  immutable keys registered across reads.
- No Go transport.

## Reproduce

```sh
scripts/docker-court.sh rust "bash scripts/threefs_court.sh --bench"
```

The court builds the shim under `./.docker-cache/threefs/lib/` because the
court's `/tmp` is a `noexec` tmpfs and `dlopen` fails there with "failed to map
segment from shared object".
