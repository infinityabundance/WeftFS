# Checkpoint object model and topology-independent restore

Status: **implemented and measured** (P3, 2026-11-05). The code is
`src/checkpoint.rs` and `src/safetensors.rs`; the tests are
`tests/checkpoint_topology.rs`; the real-weight court and its receipt are in
`bench/RESULTS-checkpoint-topology.md` and
`bench/receipts/checkpoint-topology-20261105T061223Z.json`.

This is a store-level schema, not a WeftCodec wire mode: it composes the
existing frozen pieces (v1 file chunks, v2 tensor manifests with modes 10/11/14,
group commit) and adds one canonical record.

## 1. Object model

A checkpoint is a store directory of named tensors:

```
/checkpoints/<name>/manifest          weft.checkpoint.v1 (canonical WCB), raw file
/checkpoints/<name>/t00000.s000       one mode-10 tensor object per shard
/checkpoints/<name>/t00000.s001
...
```

The manifest is the authority. Shard file names are an implementation detail;
restore resolves each `object_name` and requires the resolved ObjectId to equal
the recorded `object_id`.

### 1.1 Manifest (`weft.checkpoint.v1`)

Canonical WCB map. Every field is required; `tensors` is sorted by `name` and
each `shards` array is sorted by `index` (a manifest that is not in this order
is `NonCanonical`).

```
{
  "schema":             text   = "weft.checkpoint.v1",
  "name":               text,
  "container":          text   = "safetensors",
  "complete":           bool,
  "element_bytes":      u64,             // sum of tensor element_bytes
  "source_header":      bytes | null,    // exact u64le(len) || JSON header
  "source_file_bytes":  u64 | null,
  "source_sha256":      bytes32 | null,  // SHA-256 of the source container
  "tensors": [
    {
      "name":           text,
      "dtype":          text,            // canonical WEFT-TENSOR dtype
      "shape":          [u64],
      "logical_root":   bytes32,         // WeftTensor LogicalTensorRoot
      "content_root":   bytes32,         // H("weft/chunk/v1", row-major bytes)
      "element_bytes":  u64,
      "source_offset":  u64,             // data_offsets[0] in the source
      "shards": [
        {
          "axis":         u64,           // u32 range
          "shards":       u64,           // u32 range
          "index":        u64,           // u32 range
          "object_name":  text,          // checkpoint directory entry
          "object_id":    bytes32,
          "content_root": bytes32,       // H("weft/chunk/v1", shard bytes)
          "logical_root": bytes32,       // shard sub-tensor root (logical dtype)
          "byte_len":     u64
        }
      ]
    }
  ]
}
```

```
CheckpointId = H("weft/checkpoint/manifest/v1", canonical WCB(manifest))
```

The manifest is written as the checkpoint directory's `manifest` file with
`weft.checkpoint.schema` and `weft.checkpoint.id` attributes.

`complete` means every tensor of the source container is present. It requires
`source_header`, `source_file_bytes`, and `source_sha256`; an incomplete
checkpoint (written with `--tensor-filter`) stores the header for metadata but
cannot reproduce the whole source file.

### 1.2 Shard transport

Each shard object is a normal store tensor object:

* representation `kind:"tensor"`, `tensor_mode = 10` (`TENSOR_SHARDED`);
* dtype `q8_0`: every payload byte is one quantized word, so the mode-10 round
  trip is bit-exact. This is a **byte transport, not a lossy quantization of the
  weights**;
* shape = the logical shard shape with its **last dimension multiplied by the
  element size**. A fixed-width tensor's payload is contiguous on the last
  axis, so this shape is exactly the payload's row-major byte order. It lets the
  codec region API address a slice of a shard with the logical coordinates;
* layout `row-major`;
* the shard payload is the shard sub-tensor in row-major order
  (`WEFT-TENSOR.md` §4.2), for arbitrary split axis.

Two independent identities are recorded and checked:

| Identity | Computed over | Verified by |
|---|---|---|
| shard object `layout_root` (mode 10) | q8_0 words + scale plane | the store while decoding |
| checkpoint `shards[].logical_root` | logical dtype + shard shape + shard bytes | restore |
| checkpoint `tensors[].logical_root` | logical dtype + full shape + assembled bytes | restore |
| `content_root` | `H("weft/chunk/v1", bytes)` | restore |

The mode-10 block size is chosen as the largest of `256,128,64,32,16` that
divides the shard byte length, so the scale plane costs about 1.6 % rather than
25 %. (This is a small change to the store planner: prior q8_0 writes preferred
the smallest divisor. No existing conformance fixture or store uses q8_0, and a
q8_0 object's mode-10 descriptor still records its own block size, so older
objects remain readable.)

**Design decision: one frame per shard object.** A `kind:"tensor"` manifest
names exactly one frame, and the store's mode-10 reader requires
`shard_count = 1` because `read_tensor` returns that object's whole tensor. The
checkpoint therefore stores each shard as its own single-shard mode-10 frame
and carries the storage topology `{axis, shards, index}` in the checkpoint
manifest. The codec's multi-shard mode-10 descriptor is used for standalone
shard-set tools, not for store objects; this keeps the frozen store invariant
unchanged.

### 1.3 Publication

Shard objects and the manifest are written inside one `Store::group` closure and
published by a single commit. A failed split is rejected before anything is
written. A failed write publishes nothing (the existing group rollback).

## 2. Write

Library:

```rust
store.write_checkpoint(
    "qwen",
    Path::new("Qwen2.5-0.5B.safetensors"),
    &CheckpointWriteOptions {
        shards: 8,
        axis: 0,
        filters: Vec::new(),           // glob patterns over tensor names
        strategy: ShardStrategy::TensorSharded,
    },
)? -> CheckpointWriteReport
```

`write_checkpoint` parses the safetensors header directly (no model framework),
reads one tensor at a time, and rejects a split whose axis extent is not
divisible by `shards`:

```
NonCanonical: tensor `weight` shape [64, 32, 16] cannot be split 5-way on
axis 0 (no silent padding): shape[0] = 64 is not divisible by shard count 5
```

There is no padding. `--shards 1` is supported. `--shard-strategy zstd|raw` is
an exact fallback for shard byte lengths that are not mode-10 representable.

CLI:

```
weftfs --store STORE checkpoint write --from-dir DIR | --file FILE \
  --name NAME --shards N --axis A \
  [--tensor-filter GLOB ...] [--shard-strategy tensor-sharded|zstd|raw]
```

`--from-dir` accepts a directory with exactly one `*.safetensors` file; a
directory with more than one is rejected and `--file` must select one.

## 3. Restore

Library:

```rust
store.restore_checkpoint(
    "qwen",
    &CheckpointRestoreOptions {
        tensor: None,                        // or Some(name)
        region: None,                        // or a full-tensor Region
        topology: Some(RestoreTopology::Sharded { axis: 0, shards: 4 }),
    },
    &mut writer,
)? -> CheckpointRestoreReport
```

Algorithm, per tensor:

1. resolve every shard entry and require `resolved_id == object_id`;
2. read the shard through the tensor store, which verifies the mode-10
   `layout_root` and the content hash;
3. verify `byte_len`, the checkpoint `content_root`, and the per-shard
   `logical_root`;
4. reassemble with `PhysicalTensor::sharded(axis, payloads)` and verify the
   full `content_root` and `LogicalTensorRoot`;
5. if a topology was requested, re-express the assembled bytes in that physical
   layout, reassemble, require the same bytes and the same logical root.

The last step is what makes "restore 4-way after writing 8-way" real: the
storage topology is immutable, but the *logical* identity is proven invariant
across any requested topology. `row-major`, `column-major`, `blocked`, and
`sharded M on axis B` are all supported. A 1-way restore is
`sharded axis 0 shards 1`.

Restore writes a safetensors container. When every tensor of a complete
checkpoint is restored, the exact source header is written and the final
SHA-256 is required to equal `source_sha256` (a mismatch is `Corrupt`). A
partial restore (one tensor or a region) writes a synthesized header and
reports `header_reproduced = false`; `exact_source_file` is *measured* by
comparing the resulting SHA-256 with the recorded source SHA-256, never assumed.

CLI:

```
weftfs --store STORE checkpoint restore --name NAME [--import --out STORE_DIR] \
  [--out-name FILE] [--tensor NAME] [--range "s0:e0,s1:e1,..."] \
  [--topology N --axis B] \
  [--layout row-major|column-major|blocked|sharded] \
  [--block-shape B0,B1,...] [--host-out FILE] [--spool FILE]
```

A restore is **read-only in the store** unless `--import` is given. Without it,
the exact container is written to `--host-out` (or the spool file) and no store
object or commit is created; `--out` and `--out-name` are rejected without
`--import`. With `--import`, `--out` is a path inside the store and the restored
container is imported with `create_file_streaming` (bounded memory), so it is
visible through `weftfs ls`/`cat` and through a FUSE mount. `--host-out` also
keeps a host copy; the default spool is a temporary host file that is deleted
after import.

`weftfs checkpoint list|show|verify` complete the surface. `verify` decodes
every shard and re-checks every per-shard and per-tensor root.

### 3.1 Partial materialization

`--tensor NAME --range ...` restores only the shards whose slab on the split
axis intersects the region. Each intersecting shard is read through
`Store::read_tensor_region`, which uses the codec region API
(`PhysicalTensor::read_region`) and reports `units_touched = 1` per shard. The
report exposes:

```
shards_read          intersecting shard objects read
units_touched        codec region units touched (1 per intersecting shard)
store_bytes_read     backend bytes read (manifest + frame blobs)
store_reads          backend read + range-read calls
```

Mode-10 frames are not independently seekable, so an intersecting shard's whole
frame is decoded; the saving is that non-intersecting shard **objects** are
never read. For a region that does not touch the split axis, every shard
intersects and the saving is zero.

A region restore does not recompute the per-shard `logical_root` (the region is
not the whole shard) or the full-tensor root; the report sets
`shard_roots_verified = false` and `full_root_verified = false`. The shard's
stored mode-10 root is still verified while decoding.

## 4. Failure semantics

| Failure | Class | Message names |
|---|---|---|
| shard entry removed | `NotFound` | tensor and `shard i/N` |
| shard blob corrupted | `Corrupt` | tensor and `shard i/N` |
| shard content root mismatch | `Corrupt` | tensor and `shard i/N` |
| shard logical root mismatch | `Corrupt` | tensor and `shard i/N` |
| reassembled root mismatch | `Corrupt` | tensor and both roots |
| manifest logical root disagrees with shards | `Corrupt` | tensor and both roots |
| non-divisible split | `NonCanonical` | tensor, shape, axis, shards, "no silent padding" |
| manifest not canonical | `NonCanonical` | manifest |
| shard object id mismatch | `Corrupt` | tensor, shard, both ids |

A failed write publishes nothing. A failed restore leaves the store unchanged
(the spool is removed by the CLI).

## 5. Limitations

* Mode-10 shard frames are not independently seekable (see §3.1).
* The `q8_0` transport adds a scale plane of one 4-byte record per block
  (about 1.6 % at block size 256).
* The storage topology is immutable; alternate restore topologies are
  re-layout round-trip proofs over the reassembled bytes.
* Whole-file byte-exact reproduction requires the source header, which is stored
  in the manifest; a partial restore cannot reproduce it.
* The source container must have a contiguous data section (the standard
  safetensors layout). Non-contiguous layouts are rejected rather than guessed.
* The manifest is a store file object, not a content-addressed blob with its own
  pin; deleting the checkpoint directory's `manifest` entry makes the
  checkpoint unreadable (the shards remain until GC).
