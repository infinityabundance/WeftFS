# Shared multi-mount daemon (`weftfs mount-many`)

Status: implemented (Subphase R7, 2026-11-06). Rust only; the Go frontend has
no equivalent. The single-mount CLI (`weftfs mount`) is unchanged.

## What it is

`weftfs mount-many` serves N **read-only** views of one store from **one
process**:

* one `Store`, opened `OpenMode::ReadOnly`, shared by every view (state,
  selector index cache, storage backend handle, access counters);
* one pinned snapshot per view, expressed as `Arc<State>` so views pinned to
  the same commit share one replay and one allocation;
* one `fuser` session thread per view, and one supervisor thread. The process
  therefore holds at most `MAX_SHARED_VIEWS + 1 = 257` threads, with
  `MAX_SHARED_VIEWS = 256` enforced before any mount is attempted.

Every view is the same `WeftFuse` frontend used by a single mount. The only
difference is how it reaches the store: a shared view holds `Arc<Store>` and
borrows it immutably (`&Store` is `Sync`); a single mount holds
`Arc<Mutex<Store>>` and may borrow it mutably. A shared view has no way to
obtain `&mut Store`, so it cannot create, write, truncate, chmod, xattr,
rename, link, unlink, mkdir, or publish a commit. Write-like requests fail
closed with the store's `ReadOnly` class, which the frontend maps to `EROFS`,
exactly like a single `--read-only` mount.

## Read-only is a deliberate constraint

Writes stay on the single-daemon path. A shared view also cannot pin objects
(pins need `&mut Store`), and it does not need to: the process never runs a
writer, GC, or recovery over the shared store. Snapshot pinning and content
reads are unaffected.

## CLI

```
weftfs --store DIR mount-many --spec FILE [--allow-other]
```

`--store` is required. `--allow-other` applies to every view. There are no
other flags: every view is read-only, in the foreground, for the life of the
process.

### Spec schema

The spec is strict JSON. Unknown fields, duplicate mountpoints, and an
unknown `version` are rejected before anything is mounted.

```json
{
  "version": 1,
  "views": [
    {"mountpoint": "/mnt/views/task-42", "context": "by:task=agent-42"},
    {"mountpoint": "/mnt/views/model", "snapshot": "s1100"},
    {"mountpoint": "/mnt/views/yesterday", "at": "2026-01-01T00:00:00Z"},
    {"mountpoint": "/mnt/views/scoped", "context": "by:project=demo",
     "capability": "/etc/weft/demo.cap.json"}
  ]
}
```

Per view:

| field | meaning |
|---|---|
| `mountpoint` | existing empty directory (required) |
| `snapshot` | snapshot name or commit hex; absent means the state visible at open |
| `context` | Context Root name or `weft://` URI; implies its pinned snapshot |
| `at` | newest commit at or before RFC 3339 / unix ns (mutually exclusive with `context`) |
| `capability` | path to a capability JSON (or canonical-hex) file |

Views with neither `snapshot`, `context`, nor `at` see the store state loaded
at open. Because the store is opened read-only once, that state never changes
while the server runs — the same behaviour as a single read-only mount.

### Bounds and failure

* More than `MAX_SHARED_VIEWS` (256) views fails with `LimitExceeded` before
  any mount is attempted.
* Starting is all-or-nothing: if one `spawn_mount2` fails, every view already
  mounted is unmounted and the process exits with `Io`.
* SIGINT/SIGTERM unmounts every view and exits.
* Unmounting every view externally ends the supervisor by itself.
* Unmounting one view leaves the store and the other views untouched.

## Equivalence bar

A shared view must resolve exactly like the equivalent single-daemon mount:
the same listing names and digests, the same file bytes, the same capability
denials, and the same snapshot pinning. `tests/shared_mounts.rs` checks this
against 8 real single mounts, and `bench/scripts/shared_mounts_court.sh`
checks it at N=100 by comparing a digest over all per-view listing digests and
the number of file SHA-256 checks across shared and per-daemon runs.

## Measured behaviour and losses

See `bench/RESULTS-shared-mounts.md`. In short, at 100 views on this court:

* cgroup peak 66 MiB shared vs 1,259 MiB per-daemon (19.5x smaller);
* summed process RSS 27 MiB vs 1,917 MiB (70.9x smaller);
* 102 threads in 1 process vs 692 threads in 100 processes;
* identical listing digests and file bytes;
* **loss:** the first view appears ~7.6x later than the first per-daemon mount
  because one process resolves every context and creates every mount
  sequentially; time to *all* views is comparable (4.1 s vs 4.4 s);
* **loss:** one backend mutex serialises physical I/O across views, and one
  process is a single failure domain.
