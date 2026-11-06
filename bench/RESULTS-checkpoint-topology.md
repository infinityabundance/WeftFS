# Checkpoint topology results (P3, 2026-11-05)

Claim under test: **a checkpoint written under one sharding topology can be
restored under a different topology, through a mounted filesystem, with an
unmodified loader and exact bytes.**

Status: implemented and measured. Receipt:
`bench/receipts/checkpoint-topology-20261105T062409Z.json` (WeftFS
`90998db`). Tests: `tests/checkpoint_topology.rs` (9 tests). Design and schema:
`docs/checkpoint-topology.md`.

## 1. Method

* Corpus: the cached `Qwen/Qwen2.5-0.5B` safetensors container, BF16, 290
  tensors, 988,097,824 bytes. SHA-256
  `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`
  (matches the Hugging Face LFS digest `88c14255…0ed342`). Re-hashed in the
  court.
* Write: 8-way on axis 0, `tensor-sharded` mode-10 shard objects, one group
  commit.
* Restore: complete container under topology 8 and topology 4 (axis 0), each
  twice; one tensor (`model.embed_tokens.weight`, 272,269,312 bytes); one
  region (rows 1000..1100 of the same tensor).
* Mount: a plain `weftfs mount` of the store, then the unmodified
  `safetensors` 0.8.0 library enumerates the container and NumPy 2.4.6 +
  `ml_dtypes` 0.6.0 materialise all 290 BF16 tensors from the mountpoint; each
  tensor and the whole file are compared with the source.
* Environment: `scripts/docker-court.sh fuse` (`weft/fuse-court:1`), 3 GiB
  memory == swap, 4 CPUs, pids 1024, bounded `/tmp`, `no-new-privileges`,
  `cap-drop=ALL` plus `/dev/fuse` and `SYS_ADMIN`. One host, local store.
* Peak RSS is the process's own `VmHWM` from `/proc/self/status`; physical
  bytes are `du -sb` apparent sizes.

## 2. Exact commands

```sh
# build + run everything (FUSE device and SYS_ADMIN only)
scripts/docker-court.sh fuse "bash bench/scripts/checkpoint_topology_court.sh"

# the pieces, from inside the court
CARGO_TARGET_DIR=/work/.docker-cache/target-rust cargo build --release --features fuse --bin weftfs
BIN=.docker-cache/target-rust/release/weftfs
STORE=.docker-cache/checkpoint-p3/store

$BIN --store $STORE init --chunk-size 65536 --checkpoint-interval 256 --gc-grace-secs 0
$BIN --store $STORE --json checkpoint write \
  --file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --name qwen --shards 8 --axis 0 > write.json
$BIN --store $STORE --json checkpoint restore --name qwen --import --out /restored8a \
  --topology 8 --axis 0 --spool spool8a.bin > restore8a.json
$BIN --store $STORE --json checkpoint restore --name qwen --import --out /restored4a \
  --topology 4 --axis 0 --spool spool4a.bin > restore4a.json
$BIN --store $STORE --json checkpoint restore --name qwen --import --out /restored-single \
  --out-name embed.safetensors --tensor model.embed_tokens.weight > single.json
$BIN --store $STORE --json checkpoint restore --name qwen --import --out /restored-slice \
  --out-name embed-rows.safetensors --tensor model.embed_tokens.weight \
  --range "1000:1100,0:896" > slice.json
$BIN --store $STORE --json checkpoint verify --name qwen > verify.json
$BIN --store $STORE mount --mountpoint mnt --foreground &
python3 bench/scripts/mount_loader.py \
  --mounted mnt/restored4b/qwen.safetensors \
  --source .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --json loader.json
```

## 3. Ingest

| Property | Value |
|---|---|
| wall time | 41.53 s (37.04 s and 83.77 s in two earlier runs of the same code) |
| peak RSS | 582,028 KiB (568 MiB) |
| tensors / shard objects | 290 / 2,320 |
| element bytes | 988,065,536 |
| store apparent bytes | 825,180,192 (0.83512 × source) |
| — chunk blobs (`objects/`) | 623,168 |
| — manifests + mode-10 frames | 821,963,667 |
| commit | `2f5713780ece497440067d5bbe1da511dd837e03b1595e465ed60930db3504b3` |
| manifest root | `14da8525174882082ff6b3083ba8e4a02968140f44ae3b5859c9839c66ea77ff` |

The store is smaller than the source because mode-10 Huffman-compresses the
BF16 byte stream (high bytes are structured) while the q8_0 scale plane adds
about 1.6 %. This is a representation measurement, not a claim about other
models.

## 4. Full restore topology matrix

All four runs read **821,882,936 bytes** (the same shard frames) and all
reproduce the source file's SHA-256 exactly, header included.

| Run | Topology | Wall | Peak RSS | Bytes read | SHA-256 | `exact_source_file` |
|---|---|---:|---:|---:|---|---|
| 1 | 8-way axis 0 | 111.50 s | 833,080 KiB | 821,882,936 | `88c14255…0ed342` | true |
| 2 | 4-way axis 0 | 20.93 s | 861,100 KiB | 821,882,936 | `88c14255…0ed342` | true |
| 3 | 8-way axis 0 (warm) | 20.44 s | 827,452 KiB | 821,882,936 | `88c14255…0ed342` | true |
| 4 | 4-way axis 0 (warm) | 19.89 s | 860,884 KiB | 821,882,936 | `88c14255…0ed342` | true |

Runs 2–4 are warm; run 1 is the first full restore after ingest and includes
the cold output path and the first large flush. That first-restore cost was
56.30 s, 57.95 s, and 111.50 s in three runs of identical code, so it tracks
host writeback state, not the restore logic. The warm pair (runs 3 and 4) is
within 2.7 %, so the restore topology is **not** a performance factor at this
scale: the storage topology fixes the bytes read, and the requested topology is
an in-memory re-layout proof.

Logical-root identity across the matrix: 290/290 tensors have the same
`LogicalTensorRoot` in every run, and every requested-topology root equals the
manifest root. (`logical_roots_identical_across_topologies` and
`topology4_reroot_matches_manifest_root` are both true in the receipt.)

Restore spools the container and imports it with bounded streaming; peak RSS
is about one tensor (the 272 MB embedding), not the 988 MB container.

## 5. Partial materialization

| Operation | Wall | Peak RSS | Bytes read | Shard objects | Codec units | Output |
|---|---:|---:|---:|---:|---:|---:|
| Full tensor (`model.embed_tokens.weight`) | 15.26 s | 595,112 KiB | 224,668,493 | 8 | 8 | 272,269,448 B |
| Region rows 1000..1100 | 0.89 s | 187,944 KiB | 28,053,320 | **1** | **1** | 179,336 B |

Rows 1000..1100 lie inside shard 0 of the 8-way axis-0 split
(`151936/8 = 18992` rows per shard). The region restore reads one shard object
and one codec region unit; it reads 28.05 MB instead of 224.67 MB (8.0×
fewer bytes) and runs 17.1× faster. Its output has shape `[100, 896]` and its
bytes equal the source slice. This is the counter proof: only intersecting
shard objects are read.

## 6. Mount-path loader

| Property | Value |
|---|---|
| path | mounted `/restored4b/qwen.safetensors` |
| mounted SHA-256 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| source SHA-256 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| SHA-256 match | **true** |
| unmodified `safetensors` 0.8.0 keys | 290 |
| tensors materialised with NumPy 2.4.6 + `ml_dtypes` 0.6.0 | 290 / 290 |
| tensors byte-identical to the source | 290 / 290 |
| loader wall / peak RSS | 13.61 s / 564,388 KiB |
| FUSE daemon peak RSS | 39,320 KiB |

`safetensors.safe_open` accepts the container and enumerates every key; its
NumPy backend has no BF16 dtype, so the loader materialises BF16 with NumPy's
registered `ml_dtypes.bfloat16` dtype. Both libraries are unmodified upstream
packages.

## 7. Failure and exactness tests

`tests/checkpoint_topology.rs` (9/9 pass) covers, on a synthetic rank-3 BF16
tensor `[64,32,16]`:

* write 8-way axis 0, restore 8/4-way axis 0, 2-way axis 1, 1-way,
  row-major, column-major, and blocked `16×8×4`; every restore reproduces the
  exact source container and the same logical root;
* a single-tensor restore of a one-tensor container (byte-exact) and of a
  two-tensor container (header rebuilt, `header_reproduced=false`, tensor bytes
  equal);
* a region wholly inside one shard touches exactly one shard and one codec
  unit; a two-shard region touches two;
* a removed shard entry fails `NotFound` naming tensor and `shard i/N`;
* a corrupted shard blob fails `Corrupt` naming tensor and `shard i/N`;
* a tampered manifest `logical_root` fails `Corrupt` naming the tensor and the
  root mismatch;
* a non-divisible split fails `NonCanonical` with "no silent padding" and
  publishes nothing;
* the CLI write/restore/list/show/verify path end to end.

`weftfs checkpoint verify --name qwen` decoded all 290 tensors (2,320 shards)
in the bench court.

## 8. Limitations

* One host, one local store, warm page cache after the first run; no fleet or
  network-backend claim. Ingest wall varied 37.04–83.77 s across three runs of
  identical code, so absolute times are host-state dependent; the reported
  receipt is one run.
* Mode-10 shard frames are not independently seekable. A region restore reads
  only intersecting **objects**, but decodes each intersecting shard's whole
  frame. A region that does not touch the split axis intersects every shard.
* The shard payload is a `q8_0` byte-word transport: exact, but it carries a
  scale plane (≈1.6 % at block size 256) and is not a weight-quantization
  result.
* Restore topologies other than the stored one are re-layout round-trip proofs
  over the reassembled bytes; the storage topology is immutable.
* Whole-file SHA-256 equality depends on the exact source header, which is
  stored in the manifest. Partial restores rebuild the header and report the
  difference (`header_reproduced=false`).
* The restore CLI spools to a host file before importing with
  `create_file_streaming`; peak memory is one tensor, not the container, but the
  spool is an extra host write.
* The checkpoint manifest is a store-level schema (`weft.checkpoint.v1`,
  `docs/checkpoint-topology.md`); the Go store does not implement it, so this
  court is Rust-only.

## 9. Residual risk

1. Absolute times include host disk and page-cache behaviour; only the
   same-run comparisons (topology warm pair, slice vs full) are meaningful.
2. The mounted-container claim is verified on one kernel/FUSE build in this
   court; it uses `safetensors` + NumPy, not a model framework.
