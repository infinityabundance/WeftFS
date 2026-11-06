# 3FS transports (R22)

WeftFS addresses DeepSeek's [3FS](https://github.com/deepseek-ai/3FS) through
two transports that share one on-disk layout. The bytes, the commit protocol,
and the recovery rules are identical; only the data-plane syscall shape changes.

| Transport | Data plane | Control plane | Selected by |
|---|---|---|---|
| `posix` | positional `pread`/`pwrite` through the 3FS FUSE mount | `fsync` + `rename` + directory `fsync` | `--backend 3fs` (default) |
| `usrbio` | hf3fs rings: one shared region, read ring + write ring, batched submission | unchanged (POSIX) | `--backend 3fs --threefs-mode usrbio` |

Control state never enters a ring. Commit records, the two-slot superblock, the
writer lock, refs, and indexes are published with the same `tmp + fsync +
rename + dirfsync` sequence the local backend uses, because USRBIO carries data
I/O only.

## Why two transports

The POSIX transport is the correctness baseline and the deployment that needs no
new client library. USRBIO is the interface a storage engine would use to avoid
one round trip per range: it removes the per-request syscall, lets the client
read straight into a pre-registered shared region, and supports batched
submission with completion reaping.

Making both explicit means an operator can pick the boring one, and a receipt
can state which one produced the numbers.

## The ABI boundary

`src/threefs/ffi.rs` is the only module in the crate allowed to use `unsafe`
(the crate root is `#![deny(unsafe_code)]`; `tests/threefs.rs` fails if `unsafe`
appears anywhere else under `src/`). It transcribes the published header:

```c
struct hf3fs_iov  { uint8_t *base; void *iovh; char id[16]; char mount_point[256];
                    size_t size; size_t block_size; int numa; };            // 312 B
struct hf3fs_ior  { struct hf3fs_iov iov; void *iorh; char mount_point[256];
                    bool for_read; int io_depth; int priority; int timeout;
                    uint64_t flags; };                                      // 600 B
struct hf3fs_cqe  { int32_t index; int32_t reserved; int64_t result;
                    const void *userdata; };                                // 24 B
```

Field offsets, sizes, and alignments are pinned by `const` assertions in Rust
*and* by `_Static_assert` in the C conformance library, so a transcription error
fails a build instead of corrupting memory.

The library is opened with `dlopen(RTLD_NOW | RTLD_LOCAL)`. A missing library, or
a library that does not export all eleven required symbols, is
`UnsupportedVersion` with the missing symbol names listed. There is no fallback.

### Invariants the module owns

1. **Allocation ownership.** The upstream API does not allocate the `ior`/`iov`
   structs; both are `Box`es owned by the transport and destroyed once, rings
   first, in `Drop`.
2. **Registration lifetime.** `hf3fs_reg_fd` takes a live descriptor that must
   not be closed while registered. `RegisteredFds` deregisters and closes on
   every path, including errors — proven by a test that injects a `prep_io`
   failure and asserts zero live registrations afterwards.
3. **Single-writer discipline.** `hf3fs_prep_io` is documented as not thread
   safe, and one thread may prepare/submit while one waits. Every submission
   takes a mutex, which is what makes the `Send`/`Sync` impls sound.
4. **Buffer containment.** `[ptr, ptr+len)` must be inside the `iov` handed to
   `hf3fs_prep_io`. `region_contains` is the only source of destination
   pointers; a caller buffer outside the region is staged, not passed.
5. **Bounded work.** Entries ≤ 4096, one request ≤ 32 MiB, region ≤ 1 GiB,
   io_depth/timeout non-negative. Nothing is sized from on-disk input.

`hf3fs_reg_fd` returns "≤ 0 for a handle, errno for failure", so the adapter
treats `> 0` as failure and never mistakes handle `0` for an error.

## The buffer-oriented boundary

The store already returned owned `Vec<u8>` from `read`, which is not enough for a
shared-buffer transport. R22 adds:

```rust
trait StorageBackend {
    fn read_into(&self, key: &Key, offset: u64, buf: &mut [u8]) -> Result<usize>;
    fn read_ranges(&self, requests: &[RangeRead]) -> Result<Vec<Vec<u8>>>;
}
trait BlobPlane {
    fn read_into(&self, key: &Key, offset: u64, buf: &mut [u8]) -> Result<usize>;
    fn read_ranges(&self, requests: &[RangeRead]) -> Result<Vec<Vec<u8>>>;
}
```

`read_into` on the local/3FS backend is one positional read into the caller's
storage — no temporary allocation. `read_ranges` on the `usrbio` backend is one
`hf3fs_submit_ios` per ring-full, and one ring per `--threefs-entries` bound.
Publication uses the same boundary: `publish_file` fills its staging file through
the write ring when the transport is `usrbio`, then `fsync`s and `rename`s as
before, so the commit protocol is untouched.

`Usrbio::region()` exposes the shared region so a consumer can place its buffers
inside it; a read aimed at such a buffer takes the direct path (no staging copy),
which the counters report separately as `direct_reads` versus `staged_reads`.

## The court

`scripts/threefs_court.sh` (runs inside the rust court):

1. builds `bench/threefs/hf3fs_shim.c` — a conformance implementation of the ABI
   over real POSIX `pread`/`pwrite`;
2. runs `cargo test --test threefs --test-threads=1` (13 tests);
3. asserts that an unrelated `.so` is refused by name;
4. with `--bench`, writes a receipt.

The shim is **not 3FS**: it reports `hf3fs_is_hf3fs = 0`, has no RDMA and no
kernel bypass, and performs a `pread` per request inside `wait_for_ios`. It
therefore cannot demonstrate the transport's advantage. What it does establish
is that the ABI transcription is correct and that the adapter's batching,
completion handling, registration lifetime, and error paths are right.

One environment fact worth recording: the court's `/tmp` is a Docker tmpfs
mounted `noexec`, so `dlopen` on a shared object there fails with *"failed to map
segment from shared object"*. The court builds the shim under `./.docker-cache/`,
which is an executable bind mount.

## Measured (receipt `threefs-20261106T232643Z.json`)

Single host, rust court (3 GiB, 4 CPUs, pids 1024), synthetic fixtures, warm
page cache, 5 repetitions per phase.

| Phase | Result |
|---|---|
| 300 × 256 KiB reads, positional with reused buffers | median 9.899 ms |
| the same reads batched through the ring | median 11.159 ms (**1.127× slower**) |
| submission structure | 1,500 requests → **50 submissions** (≤ 31 per ring-full) |
| bytes | **identical** to the positional path |
| store round trip, 100 × 16 KiB | same generation (101) and same snapshot root on both transports |
| store write phase | POSIX 11.31 ms, USRBIO 12.28 ms (+8.5%) |
| reopen + `verify --full` over `usrbio` | ok, 1.69 ms |
| peak RSS | 397,196 KiB |

The USRBIO path is slower here, and honestly so: the conformance library offers no
transport advantage and the adapter opens and registers one descriptor per read.
The structural result — 1,500 requests in 50 submissions with byte-identical
output — is what the court was built to check. A production adapter should keep
immutable keys registered between reads; that cost is visible in this receipt as
`registrations = 1500`.

## Configuration

```
--backend 3fs
--threefs-mode posix|usrbio        # default posix
--threefs-mount DIR                # default: the store root
--threefs-lib FILE                 # default: $WEFT_3FS_LIB, then the soname
--threefs-entries N                # 1..=4096, default 256
--threefs-io-bytes N               # default 8 MiB, >= 64 KiB
--threefs-require-mount            # refuse usrbio unless hf3fs_is_hf3fs says so
```

## Not claimed

- No 3FS deployment was run. No throughput, latency, RDMA, multi-node, or
  durability claim follows from the shim court.
- The declared `posix`/`usrbio` capabilities carry the literal phrase
  "Declared, not measured" and a test asserts that wording is present.
- The USRBIO path can lose the 3FS lease/lock semantics: the writer lock is still
  the store's single-writer `LOCK` file on the POSIX plane.
- Go has no 3FS transport.
- `read_batch` output is still owned `Vec<u8>` at the store boundary; the
  zero-copy property applies to the storage→shared-region hop, not to the
  region→consumer hop.
