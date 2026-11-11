# Persistent checkpoint service (`weftfs checkpoint-service`)

Status: **implemented and measured** on `staging`. Evidence:
`bench/receipts/checkpoint-service-20261107T063519Z.json`,
`bench/receipts/orbax-20261107T063412Z.json`,
`bench/receipts/pytorch-dcp-20261107T063540Z.json`; analysis in
`bench/RESULTS-checkpoint-service.md`.

The Python DCP and Orbax adapters used to start the `weftfs` CLI once per
operation and to stage every restore through a host safetensors file. This
document is the normative description of the replacement: one long-lived helper
process per client, a bounded binary protocol, and tensor payloads that cross
the process boundary as file descriptors.

## 1. Why a socket and not stdin/stdout

A restore decodes into a **caller-owned memfd** so the payload never becomes a
file and never needs a second copy inside Python. File descriptors can only be
passed between processes with `sendmsg(2)`/`recvmsg(2)` and `SCM_RIGHTS`, which
requires a socket. stdin/stdout cannot carry ancillary data. Control frames stay
small (1 MiB bound); payloads always travel as descriptors.

There is no FFI crate. `pyo3` is **not** in `Cargo.lock` and is not present in
the court's cargo registry cache or extracted sources (0 matches in both), so
adding an in-process extension module would require a network dependency. The
persistent service is therefore the delivered boundary.

## 2. Lifecycle

```
weftfs --store DIR checkpoint-service --fd N [--max-frame-bytes B] [--max-tensor-bytes N]
weftfs --store DIR checkpoint-service --socket PATH [--max-frame-bytes B] [--max-tensor-bytes N]
```

* Exactly one of `--fd` (an inherited, connected `AF_UNIX` stream descriptor)
  or `--socket` (bind `PATH`, print one `ready` line on stdout, accept one
  connection) must be given.
* The service opens the store **read-only once**. That handle serves every
  read. It is *pinned* to the state at open: commits made by another process
  are invisible until `REFRESH`.
* A write opens a separate read-write handle for that one operation, publishes,
  drops it, and reopens the read handle (so the checkpoint it just wrote is
  visible to the rest of the connection).
* `SHUTDOWN` answers and exits 0. A closed socket also exits 0.

## 3. Wire format

All integers are little-endian. One request is in flight at a time; responses
carry the request id.

```
request  := u32 length | u8 opcode | u32 request_id | u8 status(=0) | body
response := u32 length | u8 opcode | u32 request_id | u8 status(0|1) | body
```

`length` counts everything after the length field (`6 + len(body)`), so a
request with an empty body has `length = 6`.

Response status `1` carries `u32 class_len | class | u32 message_len | message`,
with the normative `Class` spelling (`NotFound`, `Corrupt`, `LimitExceeded`,
`InvalidArgument`, `Malformed`, `Io`, ...). A status other than 0 in a request
frame, a length below 6 or above `--max-frame-bytes`, an unknown opcode, more
than one attached descriptor, or a truncated frame is a protocol violation.

### 3.1 Opcodes

| op | name | request body | response body | fd |
| --- | --- | --- | --- | --- |
| `0x01` | `INFO` | empty | `u32 protocol`, `u32 max_frame`, `u64 max_tensor`, `u64 generation`, `32B head`, `str store_root`, `u64 elapsed_us` | – |
| `0x02` | `LIST_CHECKPOINTS` | empty | `u32 count`, `str name...` | – |
| `0x03` | `CHECKPOINT_MANIFEST` | `str name` | manifest (3.2) | – |
| `0x04` | `RESTORE_TENSOR` | see 3.3 | see 3.4 | in: caller buffer |
| `0x05` | `WRITE_CHECKPOINT` | see 3.5 | see 3.6 | in: container |
| `0x06` | `VERIFY_CHECKPOINT` | `str name` | `u32 tensors_verified` | – |
| `0x07` | `REFRESH` | empty | `u64 generation`, `32B head`, `u64 elapsed_us` | – |
| `0x08` | `WRITE_FILE` | `str store_path` | `u64 bytes`, `u64 generation`, `u64 elapsed_us` | in: payload |
| `0x09` | `STAT_FILE` | `str store_path` | `u8 exists`, `u64 size` | – |
| `0x0a` | `READ_FILE` | `str store_path` | `u64 bytes` | in: caller buffer |
| `0x0b` | `PROFILE` | `u8 mode` (0 begin, 1 take, 2 end) | `u32 phase_count`, per phase `str name, u64 count, u64 total_ns, u64 max_ns` | – |
| `0x7f` | `SHUTDOWN` | empty | empty | – |

`str` is `u32 length` followed by UTF-8 bytes. A `32B` field is the raw digest.

Protocol **3** adds `PROFILE` after `READ_FILE`. It is a measurement aid: it
controls the phase profiler in `src/phase_profile.rs` (mode 0 clears and
enables it, 1 returns the totals without clearing, 2 stops and returns), and
it never changes published bytes. Nested phases are reported with their own
names (`state_replay`/`plane_probe` inside `store_open`, `index_build`/
`index_load`/`index_attest` inside `index_segment`, `encode_zstd`/
`encode_zstd19`/`encode_float_tensor`/`encode_semantic` inside `encode`); a
client must not add a nested total to its parent. `PROFILE` is a client-driven
window, so a service that is not being profiled pays one relaxed atomic load
per instrumented site and allocates nothing.

### 3.2 Manifest

```
32B manifest_root
str name
str container
u8  complete
u64 element_bytes
u8  has_source_file_bytes [u64]
u8  has_source_sha256     [32B]
u32 tensor_count
per tensor:
  str name, str dtype, u32 rank, u64 dims...
  32B logical_root, 32B content_root
  u64 element_bytes, u64 source_offset
  u32 shard_count
  per shard: u32 axis, u32 shards, u32 index, str object_name,
             32B object_id, 32B content_root, 32B logical_root, u64 byte_len
```

### 3.3 `RESTORE_TENSOR` request

```
str checkpoint
str tensor
u8  has_region  [u8 rank, u64 start..., u64 end...]
u8  has_topology [u32 shards, u32 axis]
```

The request must carry exactly one descriptor: a caller-created memfd whose
size is at least the payload length. Nothing is written to the store; the
service decodes, verifies, and `pwrite`s the bytes at offset 0 of that
descriptor. The descriptor is `fstat`ed before any decode; a short buffer is
refused with `InvalidArgument`.

### 3.4 `RESTORE_TENSOR` response

```
str dtype
u32 rank, u64 shape...
u64 bytes
32B restored_content_root
32B full_logical_root
u8  full_root_verified
u8  shard_roots_verified
u8  has_topology_root [32B]
u32 shards_read
u64 units_touched
u64 store_bytes_read
u64 store_reads
u64 elapsed_us
```

Every shard's own logical root, the reassembled tensor's root, and the content
root are recomputed and compared before bytes are written. A mismatch returns
`Corrupt` and the caller's buffer is untouched.

### 3.5 `WRITE_CHECKPOINT` request

```
str checkpoint
u32 shards
u32 axis
str strategy            # tensor-sharded | tensor-tiled | float-tiled | zstd | raw
u32 tile_rank [u64 dims...]
```

The attached descriptor holds the canonical safetensors container. The service
parses it once as a preflight (so an oversized tensor is refused before
anything is published) and once through the shipped `Store::write_checkpoint`
path, so the manifest still records the exact source header and SHA-256. The
container never touches the disk.

### 3.6 `WRITE_CHECKPOINT` response

```
32B checkpoint_id
32B head
32B manifest_root
32B source_sha256
u64 source_file_bytes
u64 element_bytes
u32 tensor_count
u32 shard_objects
u8  complete
u64 elapsed_us
u32 manifest_len, manifest (3.2)
```

## 4. Bounds

| bound | value | enforcement |
| --- | --- | --- |
| control frame | 1 MiB (`--max-frame-bytes`) | refused before allocation; a violating frame answers `Malformed` and exits non-zero |
| one restored tensor / staged container | 4 GiB (`--max-tensor-bytes`) | checked from the manifest (restore) or the parsed container (write) before any decode |
| one `WRITE_FILE`/`READ_FILE` payload | 16 MiB | checked with `fstat` |
| requests in flight | 1 | the protocol is strictly request/response |
| cached manifests | 8 | LRU by insertion order; a write or refresh clears the cache |
| descriptors per frame | 1 | extras are refused as malformed |

Total bytes in flight is therefore at most one control frame plus one tensor
buffer; the write path's peak is the container header plus one tensor, because
the shipped writer reads one tensor at a time.

## 5. Failure semantics

* A typed store error is a normal response (`status=1`); the connection stays
  usable. `typed_errors_keep_the_service_alive` in `tests/checkpoint_service.rs`
  proves it for `NotFound`, a short buffer, and a missing descriptor.
* A **framing** violation (bad length, truncated frame, excessive descriptors,
  non-zero status in a request) answers with `Malformed` (best effort) and then
  terminates with a non-zero exit. It never tries to resynchronise a byte
  stream it cannot frame.
* An unknown opcode is a protocol violation: typed `Malformed` response, then a
  non-zero exit.
* A transport error is returned to `main`, which prints `Class: message` and
  exits non-zero.

## 6. What a restore costs now

On the service court fixture (32 × 1 MiB f32, 4-way zstd shards, same host and
court):

| operation | service | CLI per call | ratio |
| --- | ---: | ---: | ---: |
| restore one 1 MiB tensor | 2.550 ms | 10.386 ms | 4.07x |
| restore all 32 tensors | 79.465 ms | 96.363 ms | 1.21x |
| write checkpoint (32 MiB) | 1,206.718 ms | 1,286.272 ms | 1.07x |
| manifest (32 tensors) | 0.467 ms | (n/a) | – |

One-leaf restore is one `sendmsg` + one `recvmsg` plus the decode; the payload is
already in the caller's mapping. The remaining cost is the codec decode, which
the CLI path pays too.

## 7. Guarantees and non-guarantees

Guaranteed and tested:

* A restore never mutates the store: `restore_never_mutates_the_store` and the
  court's 100-restore check compare the store tree and the commit generation
  before and after (34,027,955 B, 270 files, generation 2 in both).
* No import on read. `WeftStorageReader(import_restored=True)` now fails closed:
  there is no container to import.
* Exactness is unchanged: the same manifest, roots, and content hashes are
  verified; per-leaf SHA-256 is compared in the courts.

Not claimed:

* Not a multi-client server: one connection, one request at a time. Parallel
  callers serialize on the client lock.
* The read handle is pinned. `REFRESH` (used by the adapters before a read) is
  the only way to see another process's commits; there is no watch.
* No intra-frame parallelism and no attempt to decode several tensors at once.
* No transport other than `AF_UNIX`; no TLS, no TCP, no Windows.
* No Go client.
* The caller's buffer must already be large enough; the service does not grow
  it.
