# Merkle content identity (`weft.manifest.v3`) — write-cost court

Receipt: `bench/receipts/merkle-identity-20261105T161026Z.json`
(sha256 `d7b9d65da4435cbabbcba79d88732984b7a69b41c9a4844e472ada773c26c13d`).
Raw log: `bench/receipts/merkle-identity-20261105T161026Z.log`.

## What was measured

The measured bottleneck was the frozen v1 `ContentId =
H("weft/chunk/v1", whole_plaintext)`: every range write re-read the whole file
to recompute it. The v3 manifest replaces that identity with a Merkle root over
the chunk table (`spec/WEFT-MODEL.md` §2.3.2), so a small write only touches the
changed chunks and folds stored chunk hashes.

Workload, identical for Rust and Go and for both identities:

- one 128 MiB file at 64 KiB chunks (2,048 chunks);
- 500 deterministic random 4 KiB overwrites;
- offsets from xorshift64* state `0x9E3779B97F4A7C15`, `offset = next() % (file_bytes - 4096 + 1)`;
- one process per identity, so `/proc/self/io` and `VmHWM` describe one policy;
- `VmHWM` reset with `/proc/self/clear_refs` after fixture creation;
- full `verify` after the write loop.

Both implementations used their shipped store APIs. Rust created the fixture
through the streaming writer (no 128 MiB buffer); Go's `CreateFile` requires the
whole buffer, so Go's fixture-creation peak is included in the process high-water
before the reset. The write-phase `VmHWM` is therefore comparable across
identities, not across languages.

Environment: `weft/bench-court:1`
(`sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`),
kernel 7.3.1-1-cachyos, 3 GiB memory / 3 GiB swap / 4 CPUs / 1,024 pids, 1 GiB
bounded `/tmp`. WeftFS revision `4cd146c`, WeftFS-Go `8372e6f` (pre-commit
revisions recorded by the court; the code under test is the working tree).
Single host, warm page cache. Not a fleet or physical-media claim.

## Results

| Harness | Identity | p50 | p95 | max | `rchar` | `wchar` | chunk reads | peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Rust | whole-file v1 | 50.43 ms | 53.09 ms | 54.13 ms | 67.20 GB | 89.13 MB | 1,024,537 | 6,616 KiB |
| Rust | Merkle v3 | **1.81 ms** | 1.87 ms | 2.16 ms | **87.91 MB** | 89.15 MB | **537** | 6,796 KiB |
| Go | whole-file v1 | 202.73 ms | 210.86 ms | 251.02 ms | 134.29 GB | 89.08 MB | — | 437,328 KiB |
| Go | Merkle v3 | **4.38 ms** | 4.66 ms | 5.37 ms | **87.91 MB** | 89.09 MB | **537** | **43,392 KiB** |

Ratios (whole-file ÷ Merkle):

| Harness | p50 | p95 | bytes read | peak RSS |
|---|---:|---:|---:|---:|
| Rust | 27.8x | 28.4x | 764x | 1.0x |
| Go | 46.3x | 45.3x | 1,528x | 10.1x |

`chunk_reads` is the store's per-range-write counter: 1,024,537 for Rust
whole-file (every chunk of every write, twice: reconstruction and the whole-file
digest) versus 537 for Merkle (roughly one changed chunk per 4 KiB write, plus
the occasional two-chunk straddle). Go reports the counter only on the Merkle
path; its whole-file path rebuilds the file buffer and re-checks every existing
chunk blob, which `rchar` shows as 134.3 GB.

`wchar` is essentially flat (~89 MB) because both policies write the same
changed chunk blobs and one manifest per write. The gain is reads, not writes.

Every run: `root_changed = true`, `identity_preserved = true`, `verify_ok =
true`, `verify_problems = 0`.

## Correctness evidence (not in the bench receipt)

`tests/merkle_identity.rs` (Rust, 7/7) and `store/merkle_identity_test.go`
(Go, 6 tests) pin:

- deterministic root for the same bytes/chunk size; different chunk size changes
  the root (documented and asserted);
- empty file, single chunk, exact multiple, non-multiple sizes;
- a 4 KiB overwrite of a 128 MiB file changes the root, keeps every untouched
  chunk reference byte-identical, and reads at most two chunk blobs;
- flipping a byte in a chunk blob makes both `read_range` and `verify_region`
  fail naming the chunk index, and `verify --full` report it;
- a manifest whose chunk hash no longer folds to `content_root` is `Corrupt`;
- an unknown schema is `UnsupportedVersion`;
- reopen, `gc --apply`, and `verify --full` preserve the file; a rewrite through
  a whole-file-configured store keeps v3.

Cross-language: `store/merkle_handoff_test.go` runs in the FUSE court with the
Rust oracle. Rust writes v3 and Go parses it, recomputes the root, reads exact
bytes, writes a range, and Rust then `verify --full` + `cat` exact bytes; Go
writes v3 and Rust reports the same root and bytes. Both directions compare the
identical 64-hex root.

## Interpretation

The Merkle identity does what the bottleneck required: a 4 KiB overwrite of a
128 MiB file costs one or two chunk reads plus ~2,048 small leaf hashes, instead
of a 128 MiB read pass. Rust p50 drops from 50.4 ms to 1.81 ms; Go from 202.7 ms
to 4.38 ms. Go's whole-file path is slower than Rust's because it materialises
the whole file buffer and re-checks every existing chunk blob; the Merkle path
removes both, and peak RSS falls from 437 MiB to 43 MiB.

Limitations, stated plainly:

- The 2,048-leaf fold is still O(#chunks) hashing per write. It is small (tens of
  microseconds here) but not O(1); an explicit per-level sibling table would
  make it O(log n) at the cost of a new on-disk structure. `verify_region`
  currently recomputes the full root rather than using stored siblings.
- The root depends on chunk boundaries; changing `chunk_size` changes every
  root. The chunk size is recorded in the manifest, so this is unambiguous, but
  it is not a content-defined identity.
- There is no explicit v3→v1/v2 conversion command; implicit downgrade is
  refused, so a v3 file stays v3.
- Go's whole-file path remains whole-file. This court does not add COW to v1;
  it adds an opt-in identity whose write path is COW.
- One host, warm page cache, 500 samples, no physical media or power-loss claim.
- `wchar` is flat by design: v3 reduces reads and CPU, not the bytes of the
  changed chunks.
