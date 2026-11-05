# WeftFS P5 status — Linux FUSE frontend

**Date:** 2026-11-03
**Scope:** parent repository `/mnt/1tb_kingston/WeftFS`. The `WeftCodec`,
`WeftCodec-Go`, and `WeftFS-Go` submodules were not modified.
**Claim level:** implemented and tested through the OOM-protected Docker court on
the stated environment. No physical power-loss, performance, or model-checking
claim is made. Crash tests are **process-fault proxies**.

---

## 1. Environment and verification

All builds, mounts, and tests ran in `weft/fuse-court:1` (image built from
`docker/fuse.Dockerfile`: `rust:1-slim-bookworm` + Go + `libfuse3-dev`, `fuse3`,
`attr`, `acl`, `psmisc`). The court enforces
`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`, a 1 GiB `/tmp`
tmpfs, `no-new-privileges`, `cap-drop=ALL`, and adds `/dev/fuse` +
`CAP_SYS_ADMIN` for FUSE.

Observed environment inside the court:

```
Linux 7.3.1-1-cachyos x86_64
rustc 1.99.1 (58cf8f9c3 2026-10-13)
cargo 1.99.1 (5f94df478 2026-08-27)
fusermount3 version: 3.14.0
```

Pinned frontend dependency (exact versions in `Cargo.lock`):

```
fuser 0.15.1  (features = ["abi-7-31"], default "libfuse")
ctrlc 3.5.2
log   0.4.35
libc  0.2.191
```

`abi-7-31` is required: without an ABI feature `fuser` does not dispatch
`FUSE_RENAME2`/`FUSE_LSEEK`/`FUSE_COPY_FILE_RANGE`, and the kernel turns the
resulting `ENOSYS` into `EINVAL` for `renameat2` flags.

### Commands and observed results

Core without FUSE (also proves the core has no FUSE dependency):

```
scripts/docker-court.sh rust "cargo test --all-targets -- --test-threads=1"
```

Current result: **200 passed, 0 failed, 0 ignored**. The earlier P5 per-file
breakdown (39 tests) is superseded by the Q1 suites.

With FUSE:

```
cd /mnt/1tb_kingston/WeftFS
scripts/docker-court.sh fuse "cargo test --features fuse --all-targets -- --test-threads=1"
```

Observed in the final configuration (`2026-11-05`, Q1):

```
cargo test --features fuse --all-targets
  total 234 passed, 0 failed, 2 ignored
cargo test --all-targets
  total 200 passed, 0 failed, 0 ignored
```

The earlier per-file table below is kept as the P5 baseline. The two ignored
tests are unchanged and are not claimed as passing:

1. `permissions_with_allow_other` needs `CAP_SETUID` (the court has only
   `CAP_SYS_ADMIN`), so it cannot drop to uid 65534.
2. The same-inode direct-I/O concurrency test is ignored because of a
   reproducible kernel/fuser EIO described in §2.1. Store-level atomicity is
   still covered by the core tests and by the FUSE crash/POSIX suites.

Mount smoke checks (manual, same court) covered `cat`, `ls -la`, `ln`, `ln -s`,
`mv`, `setfattr`/`getfattr`, `rm`, `.weft/id`, and SIGTERM unmount.

### 2.1 Same-inode concurrent direct I/O: known court limitation

`tests/fuse_concurrency.rs` runs four direct-I/O readers against one write
handle on the same file. On the court kernel (`7.3.1-1-cachyos`) with
`fuser 0.15.1`, a `pwrite` can return `EIO` after a few iterations. This was
investigated, not hidden:

- The daemon stayed mounted and running with empty stdout/stderr.
- An opt-in daemon log recorded `write enter` **and** `write ok` for every
  request it received; the failing request never reached the daemon.
- The failure persisted with `max_write` at the default and at 4096, with
  `max_background=256`, with direct and buffered write handles, with `pwrite`
  and sequential `write`, and with long-lived and re-opened writer handles.
- Its rate depends on request timing/reader count (4 readers ~25%; 8 readers
  0/20 in one run), which is characteristic of a kernel-side race rather than
  store-level corruption.
- `fuser 0.17/0.18` negotiate newer ABIs but changed the reply API
  incompatibly (241 compile errors); migration was not completed.

Consequences, stated plainly:

- We do **not** claim reliable same-inode concurrent direct read/write on this
  kernel. The reproducer is committed as an ignored test.
- The store's per-commit atomicity (a reader sees one immutable content root)
  is tested independently and passes. Sequential FUSE writes, reads, crash
  recovery, snapshots, and virtual views pass.
- Read-only handles use `FOPEN_DIRECT_IO`; writable handles use write-through.
  Shared writable `mmap` through the kernel page cache works and is covered by
  `fuse_verb_matrix` (map, write, `msync`, read exact bytes through another
  description). In the same test, a subsequent `write(2)` on the description
  that held the mapping returned `EIO` once on kernel 7.3; the test uses a fresh
  description. We do not claim reliable same-description mmap-then-write on this
  kernel and do not hide the boundary.

A rerun on a different kernel/fuser version is required before changing this
status. The command is:

```sh
scripts/docker-court.sh fuse "cargo test --features fuse --test fuse_concurrency -- --ignored --test-threads=1"
```

---

## 2. POSIX matrix

`implemented` means the `fuser::Filesystem` method is present with the stated
semantics and is exercised by `tests/fuse_posix.rs`, `fuse_crash.rs`, or
`fuse_concurrency.rs`. `ENOSYS` is explicit, never an accidental default.

| Operation | Status | Notes |
|---|---|---|
| `lookup` | implemented | `.`/`..`/empty/>255 names rejected; `.weft` virtual entry at root; ACL (`weft.acl.read`) denial is indistinguishable from absence |
| `forget` | implemented (no-op) | inode table is retained for the mount lifetime; see §6 |
| `getattr` | implemented | size/mode/uid/gid/nlink from replayed state; dirs report size 0; atime/ctime = mtime |
| `setattr` | implemented | mode, uid, gid, size (truncate), mtime (`Now`/specific); atime/ctime accepted but **not stored**; each changed attribute group is one commit (size is a separate commit from mode/owner/mtime) |
| `readlink` | implemented | raw target bytes from `attrs["weft.symlink.target"]` |
| `mknod` | implemented (regular files only) | `S_IFREG` creates an empty file; FIFO/socket/device → `EPERM` |
| `mkdir` | implemented | `mode & ~umask`; `uid`/`gid` from the request |
| `unlink` | implemented | pinned object records survive until the last handle closes |
| `rmdir` | implemented | non-empty → `ENOTEMPTY`; root refused; a directory object pinned by an open handle survives until the last release |
| `rename` | implemented | POSIX replacement (non-dir↔non-dir, dir↔empty dir), `ENOTEMPTY`/`EISDIR`/`ENOTDIR`, no move into own subtree; one commit |
| `rename` flags | implemented | `RENAME_NOREPLACE` and `RENAME_EXCHANGE`; exchange of directories is `EINVAL` (cycle safety, explicit) |
| `link` | implemented | non-directories only; directories → `EPERM` |
| `symlink` | implemented | target stored as bytes |
| `open` | implemented | `O_TRUNC` commits a truncate; write handles use write-through, read-only handles use direct I/O |
| `read` | implemented | exact range from one content root; short reply only at EOF |
| `write` | implemented | `O_APPEND` appends at the current committed size under the store lock; sparse writes zero-fill; each request is one commit; 256 MiB cap → `EFBIG` |
| `flush` | implemented | returns `Ok`; **not** fsync — every write already committed |
| `release` | implemented | drops the object/content pin; an unlinked, unpinned object record is removed in a commit |
| `fsync` / `fsyncdir` | implemented | `Ok`; no page-cache flush is claimed or needed for committed data |
| `opendir` | implemented | captures a stable listing snapshot |
| `readdir` / `readdirplus` | implemented | `.`/`..`, stable cookies, raw byte ordering |
| `releasedir` | implemented | |
| `statfs` | implemented | counters derived from state; free counters are 0 (§6) |
| `setxattr` / `getxattr` / `listxattr` / `removexattr` | implemented | `attrs["xattr.<name>"] = bytes`; `XATTR_CREATE`/`XATTR_REPLACE`; 64 KiB value bound; 1024 attributes; reserved `weft.*` refused |
| `access` | implemented | virtual views checked in the daemon; live objects rely on kernel `default_permissions` |
| `create` | implemented | `O_EXCL` → `EEXIST`; opens the new file; one commit for the create (+ one only if truncating an existing file) |
| `fallocate` | implemented | modes `0`, `KEEP_SIZE`, `PUNCH_HOLE|KEEP_SIZE`, `ZERO_RANGE`, `ZERO_RANGE|KEEP_SIZE`, `COLLAPSE_RANGE`, `INSERT_RANGE`. Dense v1 files define zero content but do not reclaim physical chunks. Kernel 7.3 rejects COLLAPSE/INSERT before forwarding with `EOPNOTSUPP`; the store methods are tested directly. Invalid combinations are `EINVAL`/`EOPNOTSUPP` |
| `lseek` | partial, explicit | `SEEK_SET` answered; `SEEK_DATA`/`SEEK_HOLE` for a dense file (data from start, hole at EOF); `ENXIO` past EOF; `SEEK_CUR`/`SEEK_END` → `ENOSYS` (the kernel implements those itself and does not normally forward them) |
| `copy_file_range` | implemented | server-side copy within the daemon (matching Go), including non-zero source/destination offsets and short copy at EOF; unsupported flags → `EINVAL` |
| `getlk` / `setlk` | `ENOSYS`, binding-limited | `fuser 0.15.1` drops the FUSE `lk_flags` field, so `FUSE_LK_FLOCK` cannot be distinguished from a POSIX byte-range lock. Implementing a single manager would silently conflate two independent Linux lock namespaces. The kernel therefore performs local POSIX/flock locking (tested: two opens conflict and release correctly). Go implements daemon-side locks because go-fuse exposes the flags |
| `tmpfile` | unavailable, binding-limited | `O_TMPFILE` needs FUSE protocol 7.37 `FUSE_TMPFILE`; `fuser 0.15.1` negotiates 7.31 and has no dispatch, so the kernel fails closed (`EOPNOTSUPP`/`EISDIR`). Go implements it |
| `ioctl` / `bmap` / `poll` | `fuser` defaults | not implemented; `ENOSYS`/default |

### Error mapping

| Store class | errno |
|---|---|
| `NotFound` | `ENOENT` |
| `Exists` | `EEXIST` |
| `NotDir` | `ENOTDIR` |
| `IsDir` | `EISDIR` |
| `NotEmpty` | `ENOTEMPTY` |
| `ReadOnly` | `EROFS` |
| `Unauthorized` | `EACCES` |
| `InvalidName`, `InvalidArgument`, `Parse` | `EINVAL` |
| `LimitExceeded` | `EFBIG` (size) / `ERANGE` (xattr value at the API edge) |
| `Busy` | `EBUSY` |
| `Corrupt`, `Malformed`, `NonCanonical`, `UnsupportedVersion`, `Io` | `EIO` |

Hash or length mismatch fails closed: a read never returns bytes from a content
root that did not verify.

### Rust vs Go verb status (2026-11-04)

| Verb | Rust | Go | Boundary |
|---|---|---|---|
| fallocate 0 / KEEP_SIZE / PUNCH_HOLE / ZERO_RANGE | implemented | implemented | dense files: content semantics, no physical reclamation |
| fallocate COLLAPSE_RANGE / INSERT_RANGE | store implemented; FUSE kernel returns `EOPNOTSUPP` on 7.3 | store implemented; kernel returns `EOPNOTSUPP` | kernel rejects before forwarding; store methods tested directly |
| copy_file_range | server-side | server-side | both tested with non-zero offsets and short copy |
| mknod regular / FIFO/device rejection | implemented / `EPERM` | implemented / `EPERM` | tested both sides |
| tmpfile (`O_TMPFILE`) | unavailable | implemented | `fuser 0.15.1` has no `FUSE_TMPFILE` (7.37) dispatch |
| getlk/setlk byte-range + flock | `ENOSYS` (kernel-local locks) | daemon-side manager | `fuser 0.15.1` drops `lk_flags`, so flock cannot be separated from POSIX locks |
| SEEK_DATA / SEEK_HOLE | implemented (dense) | implemented (dense) | hole only at EOF |
| mmap MAP_SHARED / msync | implemented (tested) | not re-tested this pass | same-description write-after-mmap EIO on 7.3 |
| xattrs, rename exchange/noreplace, statfs, open-unlinked | implemented | implemented | both suites |
| atime/ctime | projected from mtime; not persisted | projected from mtime; not persisted | documented, tested on Rust |
| direct I/O vs page cache | read handles `FOPEN_DIRECT_IO`, write handles write-through | same | no writeback-cache claim |

---

## 3. Durability and caching semantics

* **One commit per mutation (default).** `create`, `write`, `truncate`, `mkdir`,
  `rmdir`, `unlink`, `rename`, `link`, `symlink`, `setattr`, xattr changes, and
  the unlink cleanup on `release` publish exactly one store commit. The reply is
  sent only after the commit file, commit directory, superblock slot, and store
  root have been fsynced by the store core. `--write-back` and `--group-commit`
  change only *when* independent mutations are published; see §3.1–3.2.
* **Writable handles use write-through.** No `FUSE_WRITEBACK_CACHE` capability
  is negotiated. The kernel sends each `write(2)` to the daemon synchronously.
  With `--write-back`, the daemon buffers the range per handle and publishes at
  `flush`/`fsync`/`release`; without it, each write is a commit.
  Shared writable `mmap` nevertheless works through the kernel page cache: the
  verb-matrix test maps a file `MAP_SHARED`, writes through the mapping,
  `msync`s, and reads the exact bytes through a separate file description.
  A `write(2)` on the *same* open description that held the mapping returned
  `EIO` once on kernel 7.3; the test uses a fresh description and the limitation
  is recorded in §2.1 rather than hidden.
* **Read-only handles use `FOPEN_DIRECT_IO`.** Reads bypass the page cache and
  are served from one immutable content root. A single read request therefore
  cannot mix two committed versions. Reads larger than one FUSE request are
  split by the kernel and are not atomic across a concurrent writer — normal
  POSIX behaviour for a file being written concurrently.
* **`fsync` / `fsyncdir` are `Ok`.** By default there is nothing left to flush:
  every mutation was durably published before its reply. With `--write-back`,
  `fsync` is the durability point and publishes the handle's buffer before it
  replies. The daemon does **not** claim to flush page cache it never populated
  for those handles.
* **`flush` is not `fsync` (default).** It reports that no deferred write error
  is pending and does nothing else. With `--write-back` it publishes the
  handle's buffer. `release` errors cannot be reported to `close(2)`; a failed
  cleanup commit is left for GC.
* **No power-loss claim.** All tests are process-fault or ordinary-operation
  tests on a host filesystem. The underlying filesystem's `fsync` contract is
  the limit of the durability statement, exactly as in `docs/store-status.md`.
* `WEFT_CRASH_PHASE` is a test hook in the store core, not a production
  feature; it aborts the process at a named publication phase.

### 3.1 Mount options added for AI-artifact workloads

Both frontends accept the same flags and implement the same semantics.

| Option | Default | Meaning |
| --- | --- | --- |
| `--content-identity whole-file\|merkle` | **`merkle` for `mount`**, `whole-file` for every other command | Identity of files *created* through the mount. `merkle` writes `weft.manifest.v3`, so a range write costs O(changed chunks + manifest) instead of O(file size). Reads accept both. Existing v1/v2/v3 files keep their identity on rewrite (v3 is never downgraded; v1/v2 only upgrade when the flag asks). |
| `--write-back` | off | Buffer each handle's writes; publish them as one commit at `flush`, `fsync`, or `release`. Off keeps commit-per-write. |
| `--group-commit` | off | Buffer independent mutations and publish one commit when a 10 ms window elapses, 256 ops accumulate, `fsync` is called, or the mount is torn down. |

The mount default is `merkle` because the mount is the interactive path that
creates AI-artifact-sized files, and commit-per-write plus whole-file hashing is
what made a 4 KiB overwrite cost 108 ms (Rust) / 392 ms (Go) in the v0.7 FUSE
court. Non-mount commands keep the frozen `whole-file` default so every existing
fixture stays byte-identical. `--representation codec-auto` is incompatible with
a v3 manifest, so `mount --representation codec-auto` requires an explicit
`--content-identity whole-file`; it never silently substitutes one.

The mount defaults are also exposed by `weftfs --help` and the Go `mount -help`;
the store-level defaults (`Options::default()`, `store.DefaultOptions()`) are
unchanged.

### 3.2 Durability table

| Mode | `write` reply means | Visible to other readers | Durable after | Crash loses |
| --- | --- | --- | --- | --- |
| default | the new version is committed and fsynced | immediately | the write reply | nothing acknowledged |
| `--write-back` | bytes are in the daemon's per-handle buffer (returned to that handle on read) | only after `flush`/`fsync`/`release` | `fsync` reply (and `flush`/`release` success) | unflushed buffered ranges of that handle |
| `--group-commit` | the mutation is in the open group and visible in memory | immediately within the mount | the group is published (window, 256 ops, `fsync`, unmount) | every mutation in an unpublished group |
| `--write-back --group-commit` | both of the above | after the handle flush; unrelated mutations batch | `fsync` reply | unflushed handle ranges and unpublished groups |

A crash at any point leaves the previous whole generation for anything not yet
published; an in-flight commit is the ordinary store crash set (old generation
or the whole new one, never a partial one). Group publication failures after the
mutation already replied are reported as `EIO` on later mutations and logged;
the store stays at the last published generation.

### 3.3 Measured cost (Q1)

Receipt `bench/receipts/fuse-writeback-20261105T200833Z.json`, one host, one
repetition of 500 × 4 KiB overwrites of a 128 MiB file (one `fsync` at the end)
and a 20-directory × 10-file tree. `p50` is the per-write latency; `flush` is
the final `fsync`.

| Impl | Identity | Mode | write p50 | write p95 | write p99 | 500 ops + fsync | commits | create ops/s | unlink ops/s |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Rust | whole-file | default | 120.0 ms | 125.1 ms | 127.1 ms | 60.1 s | 1443 | 163 | 164 |
| Rust | whole-file | `--write-back` | 8.6 µs | 13.8 µs | 24.4 µs | 1.47 s | 444 | 145 | 155 |
| Rust | whole-file | `--group-commit` | 120.2 ms | 125.0 ms | 127.0 ms | 60.2 s | 1002 | 24,754 | 39,958 |
| Rust | merkle | default | 27.3 ms | 32.5 ms | 35.4 ms | 15.1 s | 1443 | 162 | 165 |
| Rust | merkle | `--write-back` | 8.4 µs | 11.2 µs | 15.0 µs | 1.44 s | 444 | 145 | 156 |
| Rust | merkle | `--group-commit` | 21.4 ms | 26.7 ms | 33.3 ms | 10.7 s | 513 | 11,697 | 17,781 |
| Go | whole-file | default | 417.2 ms | 428.7 ms | 452.8 ms | 209.4 s | 1443 | 384 | 387 |
| Go | whole-file | `--write-back` | 10.8 µs | 16.1 µs | 24.7 µs | 1.61 s | 444 | 382 | 388 |
| Go | whole-file | `--group-commit` | 418.9 ms | 437.0 ms | 466.4 ms | 210.5 s | 1005 | 7,314 | 9,313 |
| Go | merkle | default | 26.7 ms | 31.3 ms | 34.5 ms | 13.4 s | 1443 | 384 | 377 |
| Go | merkle | `--write-back` | 11.0 µs | 16.3 µs | 23.1 µs | 1.45 s | 444 | 365 | 381 |
| Go | merkle | `--group-commit` | 27.1 ms | 31.8 ms | 34.2 ms | 13.6 s | 929 | 3,453 | 7,589 |

`--write-back --group-commit` measured within noise of `--write-back` for the
same cell (Rust merkle 8.1 µs p50 / 1.42 s flush; Go merkle 10.7 µs p50 /
1.48 s flush) because a flush already publishes one commit and metadata in the
tree is not buffered by `--write-back`.

What the numbers say, without over-claiming:

* `--write-back` is the large win for overwrite-heavy workloads: p50 falls from
  27.3 ms to 8.4 µs (Rust, merkle) and from 26.7 ms to 11.0 µs (Go, merkle), and
total time for 500 overwrites falls ~10×. The price is deferred visibility and
an fsync whose cost is the 500-range commit (1.4–1.6 s here; it publishes every
touched chunk blob durably).
* Whole-file identity is 4.4× (Rust) to 15.6× (Go) slower per write than merkle
  in the default mode because every write re-hashes the whole 128 MiB.
* `--group-commit` is the metadata-storm win: creates 163 → 24,754 ops/s and
  unlinks 164 → 39,958 ops/s (Rust), 384 → 7,314 and 387 → 9,313 (Go), with one
  commit per group instead of one per mutation. It barely helps `write` because
each range write still publishes its chunk blob durably inside the group; the
commit is only the tail of the cost.
* Go is slower than Rust on whole-file overwrites (417 ms vs 120 ms p50) but
  reaches parity on merkle defaults (26.7 vs 27.3 ms) once the whole-file digest
  is out of the path.
* The recommended mount for AI-artifact workloads is `--content-identity merkle`
  with `--write-back --group-commit`: merkle removes the O(file) digest,
  write-back removes commit-per-write from the steady state, and group-commit
  collapses metadata storms. The crash proxy in the same receipt shows unflushed
  buffered writes lost and flushed/durable ones kept, with a clean
  `verify --full` in every case.

---

## 4. Inodes, generations, and readdir

* **Per-mount inode numbers.** They are stable for the mount's lifetime and are
  not claimed to be stable across mounts.
* Root ObjectId (`32` zero bytes) → inode `1`. Fixed virtual inodes:
  `2 = /.weft`, `3 = /.weft/id`, `4 = /.weft/contexts`.
* Other ObjectIds map deterministically from the first eight bytes
  (big-endian). If the candidate is `<= 4`, lies in the virtual range
  (`>= 0x4000_0000_0000_0000`), or is already occupied, the per-mount allocator
  assigns a fresh number. Collisions are resolved, never aliased.
* **Generation** is 1 for a new inode and is bumped whenever a mutation changes
  the object's recorded version through the mount (`write`, `truncate`,
  `setattr`, xattr change).
* **`readdir` snapshots.** `opendir` captures the entry list, attributes, and
  inode numbers. Cookies are indexes (`.` = 0, `..` = 1, entries from 2), so a
  handle keeps a stable order for its lifetime even if the directory changes.
* Names are **raw bytes**, sorted unsigned byte-wise. Non-UTF-8 names are
  preserved end to end (tested with `caf\xe9` and `\xff\xfe`).
* `forget` does not evict inodes; the inode table (`ino → node`, object view,
  context views, parents, generations) is retained until unmount. Memory is
  proportional to the number of distinct inodes observed by the mount.

---

## 5. Virtual surfaces (read-only)

### `/.weft/id/<64 lowercase hex>`

* Exact-object surface, independent of selectors and paths.
* Lookup requires the object to exist in the mounted state and to be readable
  by the caller (`can_read`, including `weft.acl.read`); otherwise `ENOENT`.
* Files return their exact content bytes (every chunk hash and, for a full
  read, the whole-content hash are verified by the store). Symlinks expose
  `readlink` and their target bytes. Directory objects are traversable as
  read-only directories; all descendants remain scoped to the id surface.
* Writes, truncation, setattr, xattrs, and namespace mutations return `EROFS`.
* `readdir` of `/.weft/id` lists every authorized object id. Cost is
  proportional to the object count; it is a diagnostic surface, not a
  production directory.
* `/.weft` shadows a real `.weft` entry in the store. A real `.weft` object is
  still reachable through `/.weft/id/<hex>`; its name is not listed.

### `/.weft/contexts/<name>/`

* A saved context is a `saved_context` relation from the root object, with the
  text name as target and `attrs["selector_hex"]` as lowercase hex of the
  canonical selector bytes (`spec/WEFT-MODEL.md` §2.5).
* The view resolves the saved selector at the mounted snapshot with the
  caller's principal. Every authorized result is exposed; ambiguous result sets
  are **not** collapsed to one object.
* Display name = percent-escaped basename of the object's first path, then
  `--<8 hex characters of the ObjectId>`. If two names would collide the prefix
  is extended in 8-character steps (up to the full 64 hex characters), and the
  escaped basename is truncated only as needed to stay within 255 bytes. Bytes
  outside printable ASCII, `%`, and `/` are `%XX`-escaped; names are raw bytes.
* Entries are sorted by final name bytes. `readdir` and `lookup` both derive
  from the same computed list, so they always agree.
* Results that are directories are traversable within the context; the whole
  context subtree is read-only.
* Writes into a context directory, or at the `contexts` level, return `EROFS`
  (or the kernel's `EACCES` from the 0555 directory mode before the daemon is
  called; both are refusals).
* A context is visible only when the caller may read the namespace root.
  Contexts themselves use uid, primary gid, and `weft.acl.read`; FUSE does not
  forward supplementary groups (documented limitation).

---

## 6. Mounting, locking, and authorization

CLI:

```
weftfs mount --store <DIR> --mountpoint <DIR> \
  [--foreground] [--allow-other] [--snapshot <name-or-hex>] [--read-only] \
  [--content-identity whole-file|merkle] [--write-back] [--group-commit]
```

The identity flag defaults to `merkle` for `mount` (see §3.1); every other
command keeps `whole-file`. `--write-back` and `--group-commit` are refused on a
read-only mount instead of being silently ignored. The Go frontend accepts the
same flags (`result`-style `-flag` spellings as well).

* Mount options: `FSName("weftfs")`, `DefaultPermissions`, `AutoUnmount`, plus
  `AllowOther` for `--allow-other` or `AllowRoot` otherwise. `AutoUnmount`
  requires an allow-* option; `allow_root` is the least-privilege fallback when
  `--allow-other` was not requested. In the court the mounting user is root, so
  `AllowRoot` does not broaden anything.
* SIGINT/SIGTERM set a flag; the process drops the FUSE session (clean
  unmount) and exits 0. `ctrl-c`/`ctrlc` handles both signals.
* A missing `/dev/fuse` produces a clear error before mounting.
* `--foreground` is accepted; v1 never daemonizes, so it is also the default
  behaviour. Logging goes to the inherited stderr.
* `--snapshot <name-or-hex>` opens the store **read-only**, resolves the name
  in the snapshot map (or accepts a 64-hex commit), replays that state once, and
  serves exactly that tree. All writes return `EROFS`. A snapshot mount does not
  take the writer `flock`, so a live read-write mount of the same store can
  coexist.
* `--read-only` without a snapshot pins the state observed at open; later
  commits by another process are not seen.

Authorization:

* The kernel's `default_permissions` checks mode/uid/gid for live namespace
  objects (including supplementary groups the FUSE protocol cannot see).
* The daemon additionally enforces `weft.acl.read` on live lookups (denial is
  `ENOENT`) and full `can_read` on the exact-id and context surfaces.
* uid `0` has no special status in the store's ACL predicate; root access to
  ordinary objects follows the kernel's `default_permissions` rules.
* uid/gid wider than `u32` are clamped for `struct fuse_attr`; such stores are
  outside the supported uid range.

`statfs` reports counters derived from state: `block_size = 4096`, `blocks =
ceil(content bytes / 4096) + object count`, `files = object count`,
`namelen = 255`, and free counters `0`. The store has no independent capacity;
it grows with its host filesystem, so reporting a fabricated free-space number
would be dishonest. `df` therefore shows the used size as the total.

---

## 7. Crash proxy (process-fault, not power loss)

`WEFT_CRASH_PHASE` aborts the mount daemon during a commit. Each test writes
9 bytes at offset 0 of an existing file, then reopens the store with
`verify --full`.

| Phase | Abort point | Observed after reopen | `verify --full` |
|---|---|---|---|
| `after_objects` | content blobs/manifests written, before the commit file | old bytes | `ok = true`, 0 problems |
| `after_commit_file` | commit file fsynced, before the superblock slot | old bytes | `ok = false`; names the unpublished generation above the published superblock; no partial file |
| `after_superblock_write` | superblock slot written, before the store-root fsync | new bytes | `ok = true`, 0 problems |

In every case the file had exactly one of the two whole contents (old or new),
never a mix. The commit CAS and superblock validation in
`docs/store-status.md` are the protocol being demonstrated.

---

## 8. Concurrency

`tests/fuse_concurrency.rs`: four reader threads issue 32 KiB `pread`s at
offset 0 in a tight loop while one writer issues 20 alternating 32 KiB
`pwrite`s at offset 0. Readers use `FOPEN_DIRECT_IO` handles, so each `pread`
is one FUSE read served from one immutable content root. Every observed buffer
was uniformly one version (never mixed), and the final file was uniform.
15/15 consecutive runs passed in the final configuration, in addition to the
two full-suite runs above.

This demonstrates atomicity of a single read with respect to commits. It does
**not** claim that a multi-request user read (larger than the negotiated
`max_read`) is atomic across a concurrent writer; that is not a POSIX guarantee.

---

## 9. Tests in the court

| File | Tests | State |
|---|---|---|
| `tests/fuse_posix.rs` | 11 | 10 passed + `permissions_with_allow_other` ignored (codec-auto now passes an explicit whole-file identity) |
| `tests/fuse_verb_matrix.rs` | 4 | 4 passed (fallocate modes/server copy, mknod/tmpfile, mmap, locks/timestamps) |
| `tests/fuse_context_mount.rs` | 6 | 6 passed |
| `tests/fuse_crash.rs` | 3 | 3 passed |
| `tests/fuse_writeback.rs` | 6 | 6 passed (mount default merkle, explicit whole-file, write-back read-back/fsync/release, group-commit tree, combined) |
| `tests/fuse_concurrency.rs` | 1 | 1 ignored (kernel/fuser EIO; see §2.1) |
| `tests/group_commit.rs` | 11 | 11 passed (adds begin/commit/abort and an empty group) |
| `tests/range_write.rs` | 6 | 6 passed (multi-range equivalence, one commit, overlap order, cache current/stale) |
| `tests/store_extensions.rs` | 11 | 11 passed (also runs without FUSE) |
| other core tests | 53 | 53 passed |
| **total (fuse all-targets)** | **234 collected** | **232 passed, 2 ignored, 0 failed** |

Latest court runs: `cargo test --features fuse --all-targets` → 234 passed,
0 failed, 2 ignored; `cargo test --all-targets` → 200 passed, 0 failed;
`go test -race ./...` in `WeftFS-Go` → all packages pass, plus `go vet` and a
`CGO_ENABLED=0 go build`. (The counts in the table are by file; the 234 total
includes the harness support targets and the library unit tests.)

### Ignored test and precise reason

```
permissions_with_allow_other
```

Reason: the FUSE court runs with `cap-drop=ALL` and only `CAP_SYS_ADMIN`
added, so `setpriv --reuid=65534` fails with `EPERM` (`setresuid` requires
`CAP_SETUID`). The test creates the 0600/0644 files owned by uid 1000 through
the store CLI and mounts with `--allow-other`, but it cannot switch the caller
identity to exercise the kernel's ownership denial in this court. It is
`#[ignore]`d, not faked. Re-running it needs a FUSE court with `CAP_SETUID`
(or a non-root test runner), which is an operator decision about court
capabilities.

---

## 10. Store extensions added for P5

All are covered by `tests/store_extensions.rs`:

* `rmdir` (empty-only, `NotEmpty`; pinned dirs survive until unpin).
* Directory rename and POSIX replacement in one commit; `RENAME_NOREPLACE`
  (`rename_no_replace`) and `RENAME_EXCHANGE` (`rename_exchange`, files only).
* `set_xattr` / `get_xattr` / `list_xattr` / `remove_xattr` with the
  `attrs["xattr.<name>"] = bytes` representation, 64 KiB value bound, 1024
  attribute bound, and reserved `weft.*` names.
* `save_context` / `list_contexts` / `load_context` / `delete_context` plus
  state-scoped variants for snapshot mounts. Replacement is delete+add in one
  commit; duplicate names fail closed.
* `pin_object` / `unpin_object` / `pin_content` / `unpin_content` and pin-aware
  `unlink`/`rmdir`. `unpin_object` removes an unreferenced record in a commit;
  `gc --apply` removes crash-left orphan records and consults content pins.
* `nlink` (POSIX counts) and `statfs` (`store::StatFs`) derived from state.
* `resolve_state` for evaluating a saved selector against an already-replayed
  state (no per-lookup replay on snapshot mounts).

Store API changes are additive except for `unlink`/`rename`/`rmdir` semantics,
which are documented in the P5 tests and below.

---

## 11. Remaining risks and explicit limitations

1. **No physical power-loss claim.** Process aborts and torn files are
   exercised; hardware/VM power loss is a separate court.
2. **`max_write` left at the kernel default.** Raising it to 1 MiB with
   `KernelConfig::set_max_write` produced intermittent `EIO` on concurrent
   reads/writes in this kernel/court (kernel 7.3.1). The final daemon does not
   raise it. Performance is unmeasured.
3. **Direct I/O on read-only handles.** `MAP_SHARED` writable mappings on a
   direct-I/O handle fail with `ENODEV` on this kernel; private mappings and
   read-only use are unaffected. Writable handles are buffered and write
   through, so shared writable `mmap` is not coherent with a concurrent
   direct-I/O reader. No mmap coherence claim is made.
4. **atime/ctime are not stored.** `setattr` accepts `atime`/`ctime` and
   returns success, but `getattr` reports `mtime` for `atime`, `mtime`, and
   `ctime`. A test that depends on distinct access/change times would fail.
5. **Supplementary groups.** The FUSE request carries only uid/gid. Virtual
   views and the ACL predicate therefore use uid, primary gid, and
   `weft.acl.read`. A user who can read a file only through a supplementary
   group may see it through the live namespace (kernel check) but not in a
   context view.
6. **No server-side file locking.** `getlk`/`setlk` return `ENOSYS`; only the
   kernel's local locking applies. This is not a distributed lock.
7. **`copy_file_range` and some `lseek` values are not implemented
   server-side.** Both are explicit `ENOSYS`; the kernel falls back for
   `copy_file_range` and implements `SEEK_SET/CUR/END` without asking the
   daemon. No zero-copy or sparse-hole semantics are claimed.
8. **Inode table growth.** `forget` does not evict; monitoring a very large
   number of distinct inodes grows daemon memory. Bounded only by mount
   lifetime.
9. **`/.weft/id` and context listing cost.** Both materialize a list on
   `opendir`/lookup; cost is proportional to the authorized object or result
   count. They are diagnostic/read-only surfaces.
10. **Snapshot mounts do not hold the writer lock.** A concurrent
    `weftfs gc --apply` in another process could sweep content not named by a
    snapshot; named snapshots are GC roots, but exact-id/context reads of
    arbitrary commits are not guaranteed to survive a concurrent GC.
11. **Pins are process-local.** A crash loses open-handle pins; the commit
    chain retains an unreferenced object record until `gc --apply` removes it
    (tested).
12. **`.weft` shadowing.** A real store entry named `.weft` is hidden from
    directory listings and lookups; it remains reachable through
    `/.weft/id/<hex>`.
13. **`--allow-other` ownership test not executed** in this court (§9).
14. **WeftFS-Go compatibility is unverified here.** This frontend changes no
    on-disk bytes beyond the documented P5 store extensions; P6 must
    re-verify the shared format and the saved-context/xattr encodings.

---

## 12. Reproduction notes

Mount manually in the court:

```
scripts/docker-court.sh fuse " \
  B=.docker-cache/target-rust/debug/weftfs; \
  rm -rf /tmp/s /tmp/m; mkdir -p /tmp/s /tmp/m; \
  \$B --store /tmp/s init; \
  \$B --store /tmp/s create /hello.txt --text hi; \
  \$B --store /tmp/s mount --store /tmp/s --mountpoint /tmp/m --foreground & \
  sleep 1; cat /tmp/m/hello.txt; ls -la /tmp/m; \
  kill -TERM %1; wait"
```

Unmounting: the court's `fusermount3 -u` consults `/etc/mtab` and reports
"not mounted"; the daemon's SIGTERM/SIGINT handler performs a clean unmount
(drop of the FUSE session). The test harness uses SIGTERM as the primary
unmount and a lazy `umount -l` as a fallback after a crashed daemon.
