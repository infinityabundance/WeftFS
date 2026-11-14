# FUSE mutations under optimistic publication (T029)

Status: **implemented and measured** (Rust, opt-in). Normative text:
`spec/WEFT-MULTIWRITER.md` §17. Court: `scripts/fuse_mw2_court.sh`, receipt
`bench/receipts/fuse-mw2-20261114T101315Z.json` (revision `fd56f32`).
Tests: `tests/fuse_mw2.rs`.

## Activation

MW1 remains the default and is unchanged. A mount takes the transaction path
only when the store's writer mode is `optimistic`:

```
weftfs mount --store DIR --mountpoint MNT --writer-mode optimistic
```

An optimistic mount refuses `--write-back` and `--group-commit`, because both
publish outside the per-operation transaction. It requests
`FUSE_ATOMIC_O_TRUNC`, so `O_TRUNC` arrives in `open`/`create` and the truncate is
one transaction; otherwise the kernel would send an indistinguishable
`SETATTR(size=0)`, which this mode refuses as a bare `truncate(2)`.

## Supported operations

| operation | transaction |
|---|---|
| create | object record + directory entry, precondition `Absent`; a lost create retries and becomes an ordinary open unless `O_EXCL` |
| overwrite / write | whole-file read-modify-write with the observed version as the precondition; bounded at 8 MiB |
| unlink | entry removal plus object and outgoing-relation removal when the last name goes |
| rmdir | the same, and only for an empty directory |
| rename | source entry, target entry and a replaced unreferenced object in one commit; POSIX replacement rules and `RENAME_NOREPLACE` |
| link | one new directory entry; directories refused |
| read | an open handle reads the version it opened (snapshot isolation) |

Each supported mutation is one MW2 transaction and one commit, with the
caller's observed state as the precondition. A lost race is retried against the
newer head with the same transaction id (bounded), so an ambiguous retry is
idempotent.

## Refused operations

`EOPNOTSUPP` for `mkdir`, `mknod`, `symlink`, `chmod`/`chown`/`utimes`,
`truncate(2)`, `setxattr`/`removexattr`, `fallocate`, `copy_file_range`, and
`RENAME_EXCHANGE`. A write past the rewrite bound is `E2BIG`. These are typed
refusals, never silent fallbacks. `mkdir` is refused because the around-the-op
inode bookkeeping is not part of this subphase, not because the transaction
table cannot express it.

## The court

The Rust suite covers create/write/rename/link/unlink, a reader whose fd spans a
concurrent overwrite (`RENAME_EXCHANGE` via raw `renameat2`), and the default
MW1 mount. The shell driver issues the same mutations from **independent
processes**: six concurrent creates, four concurrent renames, four concurrent
whole-file overwrites, four concurrent links and unlinks, an fd-spanning reader,
seven refusals, then unmounts and runs `verify --full`. The default MW1 mount is
exercised in the same court (create, mkdir, rename) so an optimistic refusal
cannot leak into it.

## Losses

* A truncating open is not one transaction: `>` on an existing file is a
  truncate transaction followed by write transactions. A crash between them
  leaves the truncated version.
* Snapshot isolation is per open handle. Reads use the pinned version; `stat` on
  the inode reports the current version.
* The whole-file rewrite is bounded, so large files need MW1 or a future
  range-write transaction.
* A write sequence is not atomic: a refused later write does not roll back
  earlier ones.
* Four concurrent `O_TRUNC` opens of one inode trip a **pre-existing kernel 7.3
  EIO** in both MW1 and MW2. The court measures the control under both modes and
  records it; the successful overwrite scenario uses a non-truncating
  whole-file write so it measures the transaction path, not the kernel.

## Not claimed

No byte-range write transaction, no multi-node mount, no physical power loss,
and no Go filesystem parity for optimistic FUSE mutations.
