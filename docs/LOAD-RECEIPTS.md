# Load receipts and recovery-first restore

Receipt: `bench/receipts/recovery-load-20261108T004358Z.json`
Court: `bench/scripts/recovery_load_court.sh` in `weft/bench-court:1`,
Linux 7.3.1, 3 GiB, 4 CPUs, pids 1024, bounded `/tmp`.
Unit evidence: `tests/load_receipt.rs` (3/3) and `tests/recovery_first.rs`
(3/3); FUSE wiring: `tests/fuse_verify_load.rs` (1/1).

## `weft.load.receipt.v1`

Canonical WCB record, emitted only after the loader has produced the bytes and
recomputed their root. Schema (see `src/load_receipt.rs`):

```
schema            "weft.load.receipt.v1"
checkpoint        checkpoint name
manifest_root     checkpoint manifest identity
replica           caller label (host, rank, device set; "fuse" on a mount)
verification      "lazy" | "full"
tensors[]         name, dtype, shape, attested_root, loaded_root,
                  content_root (BLAKE3), sha256 (SHA-256 of the payload),
                  bytes, units_verified
loaded_set_root   H("weft/load/set/v1", WCB(sorted [{name, logical_root}]))
                  over the roots recomputed from the produced bytes
attested_set_root the same roster over the manifest roots
bytes_produced    container bytes (header included)
units_verified    tiles for modes 15/17/18, shards otherwise
wall_ms
exact_source_file true when the whole source container was reproduced
file_sha256       SHA-256 of the produced container
```

`root = H("weft/load/receipt/v1", canonical_bytes)`. `decode` re-derives both
roster roots and rejects a receipt whose stored roots do not match its own
tensor rows, or whose loaded set root differs from the attested set root. A
receipt is never written on a failed load.

* **Lazy** verifies every independent unit the format provides — a tile root
  for modes 15/17/18, a shard root for the checkpoint topology — while the bytes
  are decoded, and compares the loaded tensor root before emitting that tensor.
  It does not re-hash the whole frame trailer.
* **Full** is lazy plus a whole-object re-read of every stored shard through the
  verifying store path and a re-check of its whole-content BLAKE3.

### Corruption test

`tests/load_receipt.rs::corrupted_tile_is_refused_and_named` mutates one byte
in the middle of the first stored tile of a mode-15 frame on disk, then loads
lazily. The loader returns `Corrupt` naming the tile (`tile 0 (offset …)`), and
the caller's writer received **0 bytes**: the restore buffers the container and
only writes it after the set-root check. The same mutation under full
verification is caught earlier by the whole-frame hash, also `Corrupt`, also
before any byte is emitted.

### Verification cost (512 MiB, 16 × [4096,4096] BF16, 16,384 tiles)

| mode | median (3 reps) | projected per GiB | store reads | units verified |
|---|---:|---:|---:|---:|
| lazy | 2,181.2 ms | 4,362 ms/GiB | 32,864 ranged reads | 16,384 tiles |
| full | 3,836.6 ms | 7,673 ms/GiB | 32 whole-object reads | 16 shards |

Full is 1.76× lazy. Both produce the same set root
(`fd224104…30caa`) and the same exact container. The lazy mode's 32,864 ranged
reads are one per tile plus descriptor prefixes; the full mode reads whole
objects (one per tensor) and re-hashes them.

## Recovery-first restore

`weftfs checkpoint restore --first-needed FILE --first-cache DIR`
(`Store::restore_checkpoint_recovery_first`). Required tensors are verified and
written first, ordered by representation dependency depth (ties by manifest
order), then the rest of the container. The report carries
time-to-first-usable, time-to-complete, and backend bytes/read counts before
the first usable tensor.

Cold cache, 4 required of 16 tensors:

| metric | value |
|---|---:|
| time to first usable | **1,037.0 ms** |
| time to complete | 8,261.7 ms |
| bytes fetched before first usable | 15,336,426 (6.3 % of 245,382,816) |
| store reads before first usable | 4 |
| exact source file | true |

Restart with the verified cache (after a real `SIGABRT` once the required set
was verified and cached, exit 134):

| metric | cold | restart |
|---|---:|---:|
| time to first usable | 1,037.0 ms | **51.9 ms** |
| cached required | 0 | 4 |
| bytes fetched before first usable | 15,336,426 | **0** |
| time to complete | 8,261.7 ms | 6,038.6 ms |
| exact source file | true | true |

The cache (`--first-cache DIR`) stores one canonical
`weft.recovery.cache.v1` record per tensor with the payload, SHA-256, BLAKE3
content root and `LogicalTensorRoot`. A cache hit requires the recorded SHA-256
to match the bytes, the BLAKE3 content root to match the manifest, and the
recomputed logical root to match the manifest; otherwise the tensor is re-read
from the store (a tampered cache entry is rejected; see
`tests/recovery_first.rs`).

Because recovery-first restores each tensor through the named-tensor path, its
time-to-complete (8.26 s cold) is higher than a plain checkpoint restore of the
same 512 MiB (2.18 s lazy receipt): it re-resolves the manifest per tensor and
does not take the tiled whole-object fast path. It is a prioritisation mode, not
a faster full restore.

## Wiring

* CLI: `checkpoint restore --load-receipt PATH --verify-load lazy|full
  --replica LABEL`; `checkpoint restore --first-needed FILE --first-cache DIR
  [--abort-after-first]`.
* DCP / Orbax: the Python service speaks checkpoint protocol v2 (write policy)
  and the adapters' **container restore helper** (`_Storage.restore`) forwards
  `--load-receipt` when `WEFT_LOAD_RECEIPT` is set, with `WEFT_VERIFY_LOAD` and
  `WEFT_REPLICA`. The write policy is selected with `WEFT_WRITE_POLICY`,
  `WEFT_FAST_CODEC`, `WEFT_FAST_ZSTD_LEVEL`. The per-tensor service restore
  path (used by `dcp.load` and the Orbax handler's tree restore) does **not**
  emit a receipt; that path is not claimed.
* FUSE: `weftfs mount --verify-load --load-receipt-dir DIR` verifies each tensor
  file through the load path on `open`, writes one `.wcb` receipt per file, and
  fails the open with an error naming the tile if verification fails
  (`tests/fuse_verify_load.rs`). Non-tensor files are read normally.

## Losses and non-claims

* The load receipt is produced per *tensor file* on FUSE, not per handle; a
  partial read of a file still pays the full verification at open.
* Full verification re-reads every shard; it is 1.76× lazy on this fixture and
  has no lazy alternative for whole-object representations (raw/zstd/mode 9/10)
  because they expose no independent unit.
* The FUSE receipt's `manifest_root` is zero and its checkpoint name empty: a
  mount file is not necessarily a checkpoint member. The tensor roots are the
  attestation.
* The recovery cache is host-local, per tensor, and not shared across replicas.
* DCP/Orbax tree restores go through the per-tensor service and do not produce
  a load receipt; only the container-restore helper does. Extending the service
  opcode would be the next step and is not done.
* One host, synthetic tiled fixture, warm caches; the 0.5B pair is not tiled
  (one global tile shape cannot divide every tensor).
