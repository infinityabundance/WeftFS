# `weft.transport.v1` — transport-aligned container (R8)

**Status:** implemented and measured as a **layout**. Not a transport.
Receipt: `bench/receipts/transport-20261109T234518Z.json` (`weft.transport.v1`,
14/14 checks). Run at WeftFS `9d52de4`, WeftCodec `d17d058`,
WeftCodec-Go `c828e16` in the `weft/bench-court:1` image (3 GiB memory,
3 GiB swap, 4 CPUs, pids 1024, bounded `/tmp`) with a warm page cache on one
host.

Normative specification: [`spec/WEFT-TRANSPORT.md`](../spec/WEFT-TRANSPORT.md).

## 1. What was built

`weft.transport.v1` encloses complete, unmodified WFT1 frames:

```text
4096-byte header || 56-byte entry per section || zero padding || sections
```

* header: `WFTT`, version 1.0, flags 0, section count, table offset 4096,
  `total_len`, and `payload_hash = H("weft/transport/v1", enclosed frames)`;
* per section: `offset` (multiple of the declared alignment), `length`
  (exact frame length), `alignment` (512/4096/65536), mode byte, role byte,
  and the enclosed frame's own `content_hash`;
* every section offset is a multiple of its alignment; padding is zero and is
  outside every frame; padding is not hashed.

The enclosed frames keep their bytes, mode numbers, and content roots. The
container is additive and changes no frame mode.

Rust (`weftcodec::transport`) and Go (`WeftCodec-Go/transport`) implement the
encoder, strict parser, section fetch, and verification. CLI verbs:
`transport create|inspect|verify|read-section` in both. The Rust
`read-section` and the Go `read-section` issue exactly one positional range
read for the section body after reading the header and table.

## 2. Fixture

| Item | Value |
|---|---|
| corpus | `.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors` |
| corpus SHA-256 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |
| tensor | `model.layers.0.mlp.down_proj.weight` |
| dtype / shape | bf16, `[896, 4864]` |
| tensor bytes | 8,716,288 |
| mode-17 frame (float-tiled, tiles `[32,256]`) | 6,019,594 B |
| mode-20 frame (engram, 8 row groups) | 5,846,747 B |
| six small metadata frames | 834 B |
| container (8 sections, 4096 alignment) | 11,902,976 B |

The mode-17 and mode-20 frames encode the same plaintext, so both have the same
content root (`fda56d…aaefb`). That is correct: a content root is content
identity, not representation identity. The container therefore carries two
sections with equal roots; the format allows it, and the section table still
distinguishes them by offset, length, mode, and role.

## 3. Parse cost of the header and table

Two measurements, both `p50` over 500 repetitions (200 for 64 sections), on
containers built from small raw frames at 4096 alignment:

| Sections | Container | header + table | full metadata parse |
|---:|---:|---:|---:|
| 1 | 12,288 B | **0.541 µs** (541.0 ns/section) | 2.134 µs |
| 8 | 40,960 B | **0.551 µs** (68.9 ns/section) | 7.715 µs |
| 64 | 270,336 B | **0.681 µs** (10.6 ns/section) | 52.327 µs |

The p50 header+table parse is 0.541 / 0.551 / 0.681 µs at 1 / 8 / 64
sections. `header + table` is `read_header` plus `read_section_table`: a fixed 4096-byte
header parse and a linear scan of 56-byte entries. It scales with section
count at tens of nanoseconds per entry. The full metadata parse additionally
validates every section's 96-byte frame prefix, each section root and mode, the
zero-padding rule, and the final aligned length; that is why it grows with both
section count and padding volume (hex-padding scan included).

## 4. Single-section fetch

Section 0 (the mode-17 frame): offset 8,192, length 6,019,594.

| Measurement | Value |
|---|---|
| section-only `BlobPlane::get_range` p50 (20 reps) | **0.318 ms** |
| header + table + section (three range reads) p50 | **0.312 ms** |
| bytes: section only | 6,019,594 |
| bytes: single-section path (header 4096 + table 448 + section) | 6,024,138 |
| bytes: whole container | 11,902,976 |
| section / container | **50.57 %** |
| single-section path / container | **50.61 %** |

The three-read path and the one-read path are within noise at this size: the
header and table are 4,544 bytes against a 6 MB section. The important number
is the byte count, not the microseconds: one section costs 6,024,138 bytes
instead of 11,902,976, and a consumer that never needs the other sections never
reads them.

Whole-container verification (all frames fully parsed and the payload digest
recomputed) reads every byte by definition. It is reported as a separate
property; a single-section reader cannot claim it.

## 5. Padding overhead

The container never reduces bytes. Overhead is the 4096-byte header, 56 bytes
per section, and align-up padding.

Real frame set (11,867,175 B of frames):

| Alignment | Container | Overhead | Overhead % |
|---:|---:|---:|---:|
| 512 | 11,874,816 | 7,641 | **0.064 %** |
| 4096 | 11,902,976 | 35,801 | **0.302 %** |
| 65536 | 12,386,304 | 519,129 | **4.374 %** |

Small-frame set (six frames, 834 B total):

| Alignment | Container | Overhead | Overhead % |
|---:|---:|---:|---:|
| 512 | 7,680 | 6,846 | **820.9 %** |
| 4096 | 32,768 | 31,934 | **3,829.0 %** |
| 65536 | 458,752 | 457,918 | **54,906.2 %** |

64 KiB alignment is expensive for small frames — up to 64 KiB of padding per
section. It is appropriate only when the consumer's registration granularity
requires it. The 64 KiB case is published rather than hidden.

## 6. A 4 KiB-aligned slice inside a mode-17 section

The container holds the mode-17 frame as section 0 and the mode-20 frame as
section 1. Section 0 starts at 8,192, so every 4 KiB-aligned offset inside it
is a multiple of 4096 in the container.

| Item | Value |
|---|---|
| slice offset | **94,208** (94,208 % 4096 == 0) |
| slice length | 4,096 |
| slice SHA-256 | `abfff280…03a12f` |
| section root | `fda56ddd…94aaefb` |
| slice hashed under `weft/chunk/v1` equals section root | **false** |
| tile intersecting the slice | tile 0 |
| tile offset in section / length | 87,813 / 11,147 |
| tile root | `73129b63…77b422` |
| tile root verified by `decode_float_tile` after a range read of exactly 11,147 B | **true** |
| full section fetched, root verified, decoded, compared to source | **true** (14.90 ms) |
| mode-20 section root verified and decoded exactly | **true** |

Two distinct facts are being stated, and they must not be conflated:

1. **The slice is aligned and fetchable with one 4 KiB-aligned range read.**
   That is the layout property this subphase adds.
2. **The slice is not independently rooted.** Its bytes do not hash to the
   section root and nothing in the container claims they should. The root that
   applies to a slice's data is the enclosed frame's own per-tile root
   (`tile_root`, mode 17 here) or per-group root (mode 20). The section root
   applies to the whole enclosed frame.

A mode-17 tile is not nested inside a mode-20 group: v1 has no format that
places one frame's bytes inside another frame's stored payload. The container
holds them as sibling sections, which is the strongest composition the format
supports. Both frames independently decode to the same content root, and each
keeps its own inner roots.

## 7. Alignment invariant

* Every section offset in the shipped container is a multiple of its declared
  alignment.
* A copy with the first section offset incremented by one byte is rejected
  `Malformed` by `parse_metadata`. The check is in the receipt as
  `off_by_one_offset_class: "Malformed"`, so the test would fail if the
  invariant regressed.

## 8. Conformance and cross-language gate

* `conformance/v1/transport/`: **5 valid** (512 single, 4096 single, 65536
  single, 4096 eight sections, 512 two roles) and **17 rejects** (bad magic,
  bad version, non-zero flags, zero sections, wrong table offset, misaligned
  offset, overlapping sections, out-of-bounds section, duplicate offsets,
  table count mismatch, non-zero padding, wrong section root, truncated
  container, mode mismatch, disallowed alignment, unknown role, non-zero
  header reserved bytes). Files are byte-identical in WeftCodec and
  WeftCodec-Go.
* Reference adapters: Rust 22/22 pass, Go 22/22 pass
  (`conformance-transport-rust.json`, `conformance-transport-go.json`).
* Cross gate (`WeftCodec-Go/hack/cross.sh compare`), at these revisions:

```text
== weft.transport.v1 byte identity and cross verification ==
YES byte-identical transport_4096
PASS rust verifies go transport_4096
PASS go verifies rust transport_4096
PASS single-section fetch transport_4096
YES byte-identical transport_512
PASS rust verifies go transport_512
PASS go verifies rust transport_512
PASS single-section fetch transport_512
```

## 9. Losses and non-claims

* **Containers do not reduce bytes.** They add a 4 KiB header, 56 bytes per
  section, and padding (up to 64 KiB per section at 65536 alignment).
* **Alignment alone does not make a decoder zero-copy.** The court
  materialises owned `Vec<u8>` buffers; it does not demonstrate DMA, GPU
  memory, RDMA, kernel bypass, or a deployed 3FS cluster.
* **A single-section reader cannot verify the container-level
  `payload_hash`.** That requires reading every section. The per-section root
  is what a single-section reader can and must verify.
* **A sub-section slice cannot be verified against the section root.** Only the
  enclosed frame's per-tile/per-group roots apply.
* **The parse numbers are not a throughput claim.** They are warm-cache
  single-host medians; the 6 MB fetch is page-cache-served.
* **A reader that decodes a section still copies.** The envelope is the outer
  half of a zero-copy path, not the whole path.

## 10. Reproduce

```bash
scripts/transport_court.sh
scripts/docker-court.sh go \
  "cd /work/WeftCodec-Go && hack/cross.sh rust-gen && hack/cross.sh go-build \
   && hack/cross.sh go-gen && hack/cross.sh go-decode \
   && hack/cross.sh rust-decode && hack/cross.sh compare"
scripts/run-conformance.sh scripts/conformance-adapter-rust.sh --kind transport
scripts/run-conformance.sh scripts/conformance-adapter-go.sh --kind transport
```
