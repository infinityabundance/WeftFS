# WeftFS v1.1.0 — fenced publication, decode floor, checkpoint service, Go GCS, and the lifecycle courts

Research preview. v1.1.0 is the first release after v1.0.0. It changes **no
frozen wire or on-disk byte**: the WFT1 frame, codec payload modes 0–17, the
canonical WCB, `weft.manifest.v1`–`v4`, the selector grammar, the store layout,
the commit protocol, and every conformance vector are unchanged. The version
bump is package metadata plus the tool-version field in newly written
provenance records.

`1.1.0` means “the release after 1.0.0”, not production readiness. Every
measured loss and every non-claim below is part of the release.

Everything was built and measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

## Verification at the release tree

**Aggregate.** `bench/receipts/verify-20261107T155516Z.json` —
**30/30 courts `pass`, 0 failed, 0 skipped**, at:

| Repo | Verified revision | v1.1.0 tag peels to |
|---|---|---|
| `WeftFS` | `817687d63a6092ceaf6d2406c8b93abd155feb02` | `238d4e5aa41d7747e600aae16225a26ee1b17c29` |
| `WeftCodec` | `99cb87a0a8a298d39363deec33c6539b050037aa` | `99cb87a0a8a298d39363deec33c6539b050037aa` |
| `WeftCodec-Go` | `a0a1ebfddc87ec536aedf77c28ed8899d07eb07c` | `a0a1ebfddc87ec536aedf77c28ed8899d07eb07c` |
| `WeftFS-Go` | `dc7cfdb7d9bd66ea7a39f38f3b198eb124d20a3d` | `dc7cfdb7d9bd66ea7a39f38f3b198eb124d20a3d` |

The parent tag is one evidence-only commit after the verified revision
(`238d4e5` adds the receipt, conformance, wheel, and publication-attempt
artifacts; no source file differs). The submodule tags are the verified
revisions. `staging == main` in all four repositories. Two later commits on
`staging`/`main` (parent `c3d0809` onward; WeftFS-Go `ab7218b`) add this
release note, the README/STATUS refresh, the claims, and the stale-statement
reconciliation; they change no source, wire, or on-disk byte and are not part
of the tagged tree.

Courts: `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`,
`go_store_and_fuse`; codec cross-encode/decode in both directions and the byte
comparison; `spec_drift`, `claims`, `conformance_manifest`,
`conformance_coverage`, `generated_types`; `buck2_cas`, `buck2_cas_store`;
`threefs`; `agent_memory`; `ceph_rgw` (Ceph RGW 19.2.0, S3 backend plus the
fenced-writer suite); `s3_court` (MinIO, same two suites); the four context
cross-language courts; the five authenticated-capability courts; and
`fuzz_diff` (1,000,000 differential inputs, identical classes, no panics).

No court failed and no rerun was needed for `verify-20261107T155516Z.json`.
The immediately prior aggregate, `verify-20261107T154010Z.json` (30/30 at
`1901336`), is retained. The pre-existing `tests/quota.rs` writer-lock flake
was not observed in this run.

## Version bump and byte identity

Changed, and nothing else:

- `Cargo.toml`: `weftfs` `1.0.0 -> 1.1.0`; `weftcodec = "1.1.0"`.
- `WeftCodec/Cargo.toml`: `weftcodec` `1.0.0 -> 1.1.0`.
- `Cargo.lock` and `WeftCodec/Cargo.lock`: the `weftfs`/`weftcodec` package
  entries only.
- `python/pyproject.toml` and `python/weftcodec/__init__.py`: `1.1.0`.
- `WeftFS-Go/store/rebase.go`: `storeVersion` `1.0.0 -> 1.1.0`, the tool version
  recorded inside generated rebase provenance. Rust uses
  `env!("CARGO_PKG_VERSION")`, so the two agree.

`python/weftcodec/dcp.py` keeps `dcp_version = "1.0.0"` deliberately: that field
is the PyTorch Distributed Checkpoint metadata version, not the Python package
version. WeftCodec-Go has no version string and needed no source change.

**No frozen wire or on-disk byte changed.** Proof at the bumped tree:

1. **Codec cross gate** (`bench/receipts/release-1.1.0-cross.log`): Rust wrote
   47 frames, Go wrote 47 frames, Rust decoded 38 Go frames, Go decoded 38
   Rust frames; **105 `PASS` lines** and **14 `YES byte-identical` lines**
   (BF16/FP32 tiled, sparse deltas, KV/activation, semantic deltas). zstd and
   dictionary are decode-equivalence modes and are labelled as differing bytes
   by spec §12. `cross-language gate: all checks passed`.
2. **Conformance adapters** at the same tree, one manifest:

| Adapter | pass | fail | skip | total | manifest aggregate |
|---|---:|---:|---:|---:|---|
| Rust (`conformance-rust-20261107T155350Z.json`) | 246 | 0 | 92 | 338 | `51fa3dd9259f36740e41f9437c4d7467f04013a8f66906596e0bb61f40a70126` |
| Go (`conformance-go-20261107T155350Z.json`) | 246 | 0 | 92 | 338 | same |
| Python (`conformance-python-20261107T155350Z.json`) | 229 | 0 | 109 | 338 | same |

The aggregate is the committed manifest's aggregate and did not change when
the version was bumped. No fixture, vector, manifest, commit record,
superblock, or chunk byte was edited; `conformance_manifest` and
`conformance_coverage` both pass in the aggregate.

**A real tooling defect was found and fixed while producing this proof.**
`scripts/conformance/adapters/_shared.py` never learned the `tensor-tiled` and
`float-tiled` path prefixes, so both reference adapters reported
`kind None has no adapter` for 22 vectors (11 mode-15 + 11 mode-17), and the
independent Python adapter had no `float-tiled` branch. Commit `817687d` adds
the two mappings and the branch. The vectors now run: Rust/Go move from
224 pass / 102 skip to **246 pass / 92 skip**, and Python from 218 / 108 to
**229 / 109** (all 11 `float-tiled` vectors are now executed; the twelfth new
manifest entry is the reserved-mode reject under the codec directory). This is
a coverage fix in release tooling, not a wire change.

## What changed, S1–S5

The internal subphase names in `docs/STATUS.md` differ slightly; the mapping is
given in each heading.

### S1 — fenced publication v3 (`docs/STATUS.md` R27)

The R17/R18 protocol wrote the fence as one conditional `LOCK` write and the
head as a **second** conditional write on the `superblock`. The uncovered
interleaving: writer A takes the fence, is suspended before the head CAS, its
lease expires, writer B takes over without publishing a head, A resumes and its
head CAS still succeeds because the head is the value A read.

The fix makes the **head publication the conditional write of the `LOCK`
record itself**. The record is `weftfs-s3-lock v3` / `weftfs-gcs-lock v3` /
`weftfs-azure-lock v3` with `owner`, monotonic `fence`, `sequence`,
`lease_deadline_ns`, `renewed_ns`, `pending_generation`, `pending_commit`, and
`pending_commit_len`. One conditional write carries both the fence and the new
head; a successor's takeover rewrites the object, so a stale write is
`412`/precondition failure -> `Busy` with the head untouched. The two-slot
superblock is an **attested cache** on an object-store commit plane, rebuilt
from the record on a writable open; a cache ahead of the record is refused
`UnsupportedVersion`. Local/POSIX keep `flock` + superblock publication.
Release is a content-addressed tombstone, never an unconditional delete.

Evidence: MinIO `s3_backend` 6/6 and `s3_fencing` **11/11**
(`bench/receipts/s3-court-20261107T025420Z.json`); Ceph RGW 19.2.0 6/6 and
**11/11** (`ceph-rgw-20261107T025545Z.json`); fake-GCS `gcs_fencing` **9/9**
(`gcs-20261107T034113Z.json`); Azurite 3.35.0 `azure_fencing` **9/9**
(`azure-20261107T034219Z.json`); Go MinIO focused **20 pass / 0 fail / 1 skip**
(`WeftFS-Go/bench/receipts/go-s3-20261107T032512Z.json`). The new acceptance
test suspends a writer *after* the fence (`WEFT_FENCE_PAUSE_AFTER_FENCE`), lets
the lease expire, takes over, resumes, and asserts `Busy`, unchanged generation
and head, and that B's publication succeeds. The old before-the-fence test is
retained.

Losses and non-claims: still one writer; no clock-skew defence; a lost network
reply is the usual at-least-once ambiguity; the superblock cache is a second
object write (it follows publication and can never move the cache backwards);
no physical media power loss; single-node emulators only.

### S2 — decode floor, SIMD kernels, mode 17, access-pattern planner (`STATUS.md` S1)

**Decode floor** (single-thread, 16 MiB BF16): Huffman 15.4646 ms (1034.6 MiB/s
of exponent plane), plane join 18.0656 ms, whole mode-9 decode 34.7865 ms
(459.9 MiB/s); FP32 mode 9 21.4125 ms (747.2 MiB/s).

- Rust AVX2 Huffman eight-byte refill + two-symbol unroll is **1.07–1.10x** the
  retained scalar kernel in the same process, but +1.0 % (1 MiB) to +3.7 %
  (16 MiB) across runs: **no absolute win**, so it is not selected. One
  Huffman bitstream cannot be lane-parallelised without a wire change.
- Rust BF16 byte-wise plane join is **11.2x** faster at 16 MiB (18.0656 ->
  1.6166 ms) via LLVM auto-vectorisation. The explicit AVX2 kernel ties at
  >=1 MiB and is **3.8x slower** at 128 KiB, so it is not selected. Whole BF16
  mode-9 decode 34.7865 -> 18.2186 ms (**1.91x**); 1 MiB 2.2621 -> 1.2181 ms
  (1.86x). FP32 is unchanged; its +2–8 % deltas are noise.
- Go: the word-level Huffman variant is **0.93x** and is not selected; the BF16
  direct pack is **1.45x**. Go has no SIMD intrinsics and cannot match the Rust
  join.
- `unsafe` is confined to `WeftCodec/src/simd.rs`; 10,000-case differentials per
  kernel pass.

**Mode 17 `FLOAT_TILED`** (additive): per-tile mode-9 plane split, per-tile
`tile_root`, full decode verifies every tile and the logical root, region reads
report `tiles_touched`. Modes 0–16 are byte-identical; the reserved reject is
now 18. **v1.1.0 covers codec modes 0–17.** Mode 18 (`FLOAT_TILED_STREAMS`) is
additive and post-v1.1.0: it is not part of this release and did not change any
v1.1.0 byte. 3 valid + 8 reject vectors in both codec repos, byte-identical frames.
Real Qwen BF16 embedding `[151936,896]`, tile `[64,896]`: mode 9
**180,623,816 B** vs mode 17 **181,541,557 B (+0.51 %)**; write 2,287 vs
7,435 ms (**3.25x slower**); whole read 745.7 -> 486.6 ms at one thread and
740.2 -> 235.9 ms at four; 100-row read 777.1 -> **2.30 ms** (337x); 1-row
749.3 -> **2.12 ms** (353x); every read byte-exact.

**Access-pattern planner**: `AccessPattern::{Whole, Region, Mixed}`, integer
objective `physical_bytes + lambda_ns * decode_at_pattern`. Whole/λ0 selects
`float-tensor`; `region:1792:1000` and `mixed:0:1792:1000` select
`float-tiled`; whole/λ1 selects `raw`. Default plans are identical to explicit
`Whole` (tested, every candidate). Go selects the same strategies on the same
fixture.

**Restore pipeline**: 988,097,824 B / 290 tensors on tmpfs: batched read
2,822.4 / 1,837.0 / 1,420.8 ms and pipelined total 2,764.5 / 1,678.8 /
1,317.9 ms at 1/2/4 workers; decode wall is 95.6 % of total at one worker and
99.9 % at four. Disk destinations are writeback-bound (10–14 s) and are not
the basis of a claim.

Losses and non-claims: no whole-model mode-17 checkpoint (one tile shape cannot
divide every tensor extent); mode 17 costs +0.51 % bytes and 3.25x write time;
the planner has **no write term**, so `region:1792:1` selects `float-tiled` by
estimate yet measures 7,437 ms (7,435 write + 2.12 read) versus mode 9 3,036 ms
(2,287 + 749) — break-even is about seven 1-row reads; Go range fetch reads the
whole object; no cold-cache, GPU, or fleet claim.

### S3 — persistent checkpoint service, memfd restore, DCP/Orbax re-measure

`weftfs checkpoint-service` is one long-lived `AF_UNIX` process; payloads cross
as `SCM_RIGHTS` memfds; the store is opened read-only once and a restore cannot
write to it. Framing `u32 length | u8 opcode | u32 request_id | u8 status |
body`; bounds 1 MiB per control frame, 4 GiB per tensor/container, 16 MiB per
metadata file, one descriptor per frame, 8 cached manifests; an unknown opcode
or framing violation answers `Malformed` and exits rather than resynchronising.
`REFRESH` is the only way a pinned read handle sees another process's commits.

Service court (32 x 1 MiB f32, 4-way zstd):

| operation | service | CLI per call | ratio |
|---|---:|---:|---:|
| restore one tensor (median of 20) | **2.550 ms** | 10.386 ms | 4.07x |
| restore all 32 (median of 3) | **79.465 ms** | 96.363 ms | 1.21x |
| write checkpoint | **1,206.718 ms** | 1,286.272 ms | 1.07x |

230 requests / 216 restores / 226,492,416 B in one process; after 100 restores
the store was byte-identical: 34,027,955 B, 270 files, generation 2.

Orbax (11 leaves, 8-way sharded, 58,765,352 logical bytes): save **77.958 ms
Orbax vs 1,859.152 ms WeftFS**; full restore 22.927 vs 185.788 ms; single leaf
5.327 vs 21.948 ms; bytes 50,643,641 vs 50,866,872 (**+0.44 %**); store growth
from restores **0 B** (was +67,615,378 B). DCP (8 x 4 MiB f32, topology 8):
save 1,498.649 vs 1,521.614 ms CLI control; full load **101.006 ms** vs
106.475; single tensor **24.135 ms** vs 22.362; physical/store = **0.9294** of
`torch.save`; restore growth 0 B.

Losses and non-claims: Orbax save is still **23.85x** slower and restore
**8.10x** slower; the Rust checkpoint write and 11 per-leaf decodes dominate;
DCP save is 22.5 % slower than the Q5 record; no GPU-direct path; no Go
client; no multi-client server; no parallel decode inside the service; no
watch (only `REFRESH`); no TLS/TCP. A pre-existing region-restore bug (byte-
scaling the last axis for every shard strategy, not just mode-10 `q8_0`) was
found and fixed; `import_restored=True` now fails closed because the service
has no container to import.

### S4 — Go GCS backend, `calibrate --install`, read-only FUSE over objects (`WeftFS-Go/NOTES.md` R26/R27)

Go now has a real GCS JSON-API backend: `alt=media` reads with `Range`,
media and two-step resumable uploads, `ifGenerationMatch` create-once and CAS,
strict generation re-reads, percent-encoded object names, bounded response
bodies, and transport failures kept as `Io`. Authentication is exactly one of a
bearer token, a service-account RS256 JWT-bearer key (PKCS#8/PKCS#1,
2048–8192 bits, one-hour assertion, `min(300 s, lifetime/2)` refresh), or
`--gcs-anonymous` for an emulator; mixed forms are `UnsupportedVersion`.
The writable open **measures** create-once and stale-generation refusal and
refuses an endpoint that accepts a stale generation. The fenced writer is the
same v3 protocol as Rust, with the single publication point being one
conditional `LOCK` upload before the superblock cache CAS.

Evidence (`WeftFS-Go/bench/receipts/go-gcs-20261107T115759Z.json`): focused
`TestGCS` **28 pass / 0 fail / 1 skip**; full `go test -race` **5/5 packages
ok**; read-only FUSE over GCS **3/3 files byte-identical and 3/3 SHA-256
matches**, listing digest `a4b80dbc58…`, ranged-read SHA-256 `d284cd81…`, a
write refused `Read-only file system`, daemon exited on unmount, and
`verify --full` passed after unmount (generation 6). S3
(`go-s3-20261107T120121Z.json`): focused **20/0/1**, full **5/5**, cross-language
**17/17**, FUSE **3/3** with listing `eeaf8a4d…` and ranged `16a0d69c…`.
`weftfs calibrate --install` now matches Rust: strict validation, temp file,
`fsync`, rename to `<calibration_id>.json`, directory `fsync`, idempotent,
advisory 0644 document; a store test shows the baked table choosing `raw`
(2,097,152 B, 625,795 ns) while the installed document chooses `float-tensor`
(1,311,519 B, 2 ns).

Losses and non-claims: **no writable FUSE-over-object court** — the only
evidenced mount is read-only; no live Google Cloud run; the emulator is
`fake-gcs-server` on a private bridge with a memory backend; the fence is still
two conditional writes on two objects (the publication point on `LOCK` and the
superblock cache), not one atomic primitive; single writer by design; no
clock-skew defence; `durable_fsync` means provider acknowledgement; Go still has
**no Azure backend** (choosing `--backend azure` in Go is `InvalidArgument`);
listing paging uses a test-only page-size knob.

### S5 — checkpoint-family lifecycle, zero-trust KV capability, 3FS context plane, mode index (`STATUS.md` E1–E4)

**Lifecycle court** (`family-lifecycle-20261107T132359Z.json`, 7 configurations,
real public weights, every corpus SHA-256 verified in the court): consecutive
second checkpoint **42,211 B** vs 144,403,851 B per-tensor `zstd -19`; family
data 173,233,994 B. Fine-tune second checkpoint **541,273,184 B** vs
767,597,259 B; family data 1,199,958,249 B; base checkpoint **0.856x**
`zstd -19`. A one-row access plan goes 2,366.7 ms / 333,871,119 B ->
**5.22 ms / 2,896,846 B** (454x) at +9.8 % (region1) / +14.6 % (region100)
storage. Cross-topology restore verifies every root: 197,146,951 B (pythia,
0.700x raw) and 823,438,236 B (qwen, 0.833x), but 2.9 s / 10.2 s at one thread.
Process-fault recovery at all three publication phases, 3 runs each: no partial
state, open 7.7–9.6 ms, time-to-usable 0.13–0.27 ms (old
`after_objects`/`after_commit_file`) or 114–266 ms (new). GC reclaims
141,682,009 B (pythia) / 231,628,264 B (qwen) with no snapshot, and **0 blobs**
(index files only) with a pinned snapshot.

Losses: F32 base is **1.20x** `zstd -19` (the planner's zstd candidate is codec
level 3); 50–217 small tensors stay raw under region plans; the 1 MiB tile plan
was **OOM-killed at the 3 GiB court** (local configurations were then measured
at 6 GiB and the override is recorded per configuration); MinIO ingest is
**+17.9 %**; the gated second checkpoint loses to a whole-file
`--patch-from -12` on pythia.

**Zero-trust KV capability** (`kv-capability-20261107T152958Z.json`, 13/13
tests): a frozen mode-12 page carries a canonical `weft.kv.capability.v1`
descriptor whose id is granted in an authenticated envelope's `roots` and whose
`kv.<tenant>` namespace is granted in `namespaces`. Every pre-byte failure
(absent object, torn descriptor, absent/expired/foreign grant, POSIX denial) is
the same `Unauthorized` with `kv page is not available to this principal`. A
content hash is not a credential. Measured cost (2,000 samples/path): cached
check 33.0/42.1 µs p50/p99 vs 18.0/21.2 µs decode-only control (**+15 µs fixed,
+83 % at 1 KiB, +8 % at 64 KiB**); per-read envelope verify 81.2/96.6 µs;
denied 6.4 µs.

**3FS context plane** (`docs/THREEFS-CONTEXT-PLANE.md`): layering statement,
per-operation placement, what the USRBIO shim court proved (ABI transcription,
registration lifetime, 1,500 requests in 50 submissions, byte-identical output)
and did not (it is 1.127x slower and is **not 3FS**), the measured context-plane
budget, and the eight-step experiment required on a deployed cluster.

**Normative consolidation**: `spec/WEFT-AI-CODEC.md` §0 is an RFC-shaped mode
index (mode, name, schema id, root domains, version, status, vector directory)
for modes 6–17 plus reserved 18, exported to all three mirrors. Manifest 338
entries; spec drift 42/42.

Not done: Go parity for KV pages; a FUSE read path for KV pages; a
**constant-time** capability check (explicitly not claimed); KV encryption at
rest; retention/expiry policy; block-independent mode-9 streams; any 3FS
cluster run; Go checkpoint manifests.

## Standalone courts not in the aggregate

These are cited separately. None is part of `verify-20261107T155516Z.json`.

| Court | Receipt | Result |
|---|---|---|
| Azure / Azurite 3.35.0 | `bench/receipts/azure-20261107T034219Z.json`, `azure-court-20261107T034219Z.json` | `azure_fencing` **9/9** including paused-after-fence; earlier `azure-20261107T015947Z.json` has 19 backend / 0 failed and a 7/7 fencing run. Service SAS is read-only; a writable SAS open is refused by Azurite's container check. |
| Go S3 / MinIO `RELEASE.2025-04-22T22-12-26Z` | `WeftFS-Go/bench/receipts/go-s3-20261107T120121Z.json` | focused **20 pass / 0 fail / 1 skip**; full `-race` **5/5 packages**; cross-language **17/17**; read-only FUSE 3/3; `If-Match` form `quoted-as-returned`. |
| Go GCS / `fake-gcs-server` 1.56.1 | `WeftFS-Go/bench/receipts/go-gcs-20261107T115759Z.json` | focused **28/0/1**; full `-race` **5/5 packages**; read-only FUSE 3/3; measured create-once and stale-generation refusal. |
| 3FS `--bench` (conformance shim, not a 3FS run) | `bench/receipts/threefs-20261107T020623Z.json` | 300 files x 256 KiB x 5 reps: scalar 8.849701 ms, ring 10.963013 ms (**1.2388x slower**), byte-identical; 1,500 requests / 50 submissions; same store generation 101 and root. |
| Python court (installed wheel) | `bench/receipts/python-bindings-20261107T160721Z.json` | **369 tests, 0 failures, 0 errors, 10 skipped** in `weft/python-court:1`; wheel 90,884 B. |
| Checkpoint-family lifecycle | `bench/receipts/family-lifecycle-20261107T132359Z.json` | 7 configurations; numbers and losses above. |
| Zero-trust KV capability | `bench/receipts/kv-capability-20261107T152958Z.json` | 13/13 tests; cost above. |

## Publication outcomes (Stage 1 and Stage 2)

**PyPI: not published. No credential exists.** Searched, without printing a
value: `PYPI_API_TOKEN`, `TWINE_PASSWORD`, `TWINE_USERNAME`, `PYPI_TOKEN`
(absent); `~/.pypirc`, `~/.config/pypi/token`, `~/.config/pypi/credentials`,
`~/.netrc` (absent). The 1.1.0 wheel was built and tested in the Python court,
then the real upload was attempted with twine 6.1.0:

```
PYTHONPATH=/work/.docker-cache/py-twine timeout 180 python -m twine upload \
  --non-interactive --repository-url https://upload.pypi.org/legacy/ \
  python/dist/weftcodec-1.1.0-py3-none-any.whl
Uploading distributions to https://upload.pypi.org/legacy/
ERROR    NonInteractive: Credential not found for API token.
exit code 1
```

`https://pypi.org/pypi/weftcodec/json` returns **HTTP 404**, so the project has
no published 1.1.0 metadata. The wheel is a release artifact only. No token was
invented. Full record: `bench/receipts/publication-20261107T160818Z.json`.

**Live GCS: not attempted.** `WEFT_GCS_SA_KEY`,
`GOOGLE_APPLICATION_CREDENTIALS`, `WEFT_GCS_BUCKET`, `WEFT_GCS_PROJECT` are
absent, as are `~/.config/gcloud/application_default_credentials.json` and
`~/.config/gcloud/credentials.db`. With no credential **and no bucket name**,
no live probe or bucket round trip was possible. The only GCS evidence remains
the `fake-gcs-server` emulator.

**Live Azure: not attempted.** `AZURE_STORAGE_ACCOUNT`, `AZURE_STORAGE_KEY`,
`AZURE_STORAGE_SAS_TOKEN`, `WEFT_AZURE_ACCOUNT`, `WEFT_AZURE_KEY`, and
`WEFT_AZURE_SAS` are absent, as are `~/.azure/azureProfile.json` and
`~/.azure/accessTokens.json`. No `*.blob.core.windows.net` endpoint was
contacted; the only Azure evidence remains Azurite.

No AWS credential was present either (`AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`), so no live S3 run is claimed.

## Artifacts

### Tags

| Repo | Tag | Commit |
|---|---|---|
| `WeftFS` | `v1.1.0` | `238d4e5aa41d7747e600aae16225a26ee1b17c29` |
| `WeftCodec` | `v1.1.0` | `99cb87a0a8a298d39363deec33c6539b050037aa` |
| `WeftCodec-Go` | `v1.1.0` | `a0a1ebfddc87ec536aedf77c28ed8899d07eb07c` |
| `WeftFS-Go` | `v1.1.0` | `dc7cfdb7d9bd66ea7a39f38f3b198eb124d20a3d` |

### Rust crates (crates.io)

Published with `scripts/publish-crates.sh` (rust court, weftcodec first):

| Crate | Version | Live checksum | Yanked |
|---|---|---|---|
| `weftcodec` | 1.1.0 | `38a8dccfaa8a4223c03631131909676117b3b7a85fd7ce001b487c32b54bd8a7` | false |
| `weftfs` | 1.1.0 | `5b318b528368dc7dad0dccc5404005d79cbe7276834c8ab32a99a351fc21f13d` | false |

Checksums read back from the crates.io API after publication.

### Python wheel

`python/dist/weftcodec-1.1.0-py3-none-any.whl`, **90,884 bytes**, sha256
`33e3d7e8b3fce8683245e16c59e5a97d719a574038eadc511af64fd8a59c9e58`. Built and
tested from the installed wheel in the Python court. Attached to the GitHub
release; **not** published to PyPI.

### Container images (GHCR)

Each repository publishes only the image it owns. All four tag runs completed
`success` (WeftFS run 42389868928). Each image was pulled and its config label
read:

| Image | Index digest (`@sha256`) | `org.opencontainers.image.source` | `org.opencontainers.image.revision` |
|---|---|---|---|
| `ghcr.io/infinityabundance/weftfs:v1.1.0` | `038f4989bc507c03e1684cec6b23d3239126ca203e0bfa0689322e2129538a07` | `https://github.com/infinityabundance/WeftFS` | `238d4e5aa41d7747e600aae16225a26ee1b17c29` |
| `ghcr.io/infinityabundance/weftcodec:v1.1.0` | `40238751fe927ca014fe7d2f7717bdc0c5515a01d427a174e061f55ddebed894` | `https://github.com/infinityabundance/WeftCodec` | `99cb87a0a8a298d39363deec33c6539b050037aa` |
| `ghcr.io/infinityabundance/weftcodec-go:v1.1.0` | `15ff5b4293084244766f63780c588b52ca1a2e8472d260f1c4975decb3b88f9d` | `https://github.com/infinityabundance/WeftCodec-Go` | `a0a1ebfddc87ec536aedf77c28ed8899d07eb07c` |
| `ghcr.io/infinityabundance/weftfs-go:v1.1.0` | `cfe0815609e5faed31eb12e4a19114f79018bc0ed670eb1e44ab216fad67bd50` | `https://github.com/infinityabundance/WeftFS-Go` | `dc7cfdb7d9bd66ea7a39f38f3b198eb124d20a3d` |

Container builds are not bit-reproducible, so the recorded digests are observed
release digests, not a reproducibility claim. FUSE mounts need
`--device /dev/fuse --cap-add SYS_ADMIN`; codec and store-only commands do not.

## Not claimed

This release does **not** claim any of the following:

- **Physical media power loss.** Crash courts are process-fault proxies; the
  QEMU/block-layer courts use a labelled device model and `cache=unsafe`
  lying-fsync, not a pulled power cable.
- **Host page-cache loss.** The host `drop_caches` was not writable in the
  court; only per-file `fadvise(DONTNEED)` was possible.
- **RAPL / joule energy.** `energy_uj` was unreadable on host, guest, and court.
  CPU user/sys time is not energy and is not presented as such.
- **GPU, TPU, or SIMD decode execution.** All decode and restore measurements
  are scalar CPU work in a 4-vCPU court. The AVX2 kernels are compile-time
  candidates only where measured; no GPU/TPU path exists. Orbax/JAX ran with
  forced CPU devices.
- **Live 3FS, Azure, or GCS deployments.** No live service was reached. GCS was
  `fake-gcs-server`; Azure was Azurite; 3FS was a conformance shim with
  `hf3fs_is_hf3fs = 0`. No RDMA, kernel bypass, replication, or cloud run.
- **A Go Azure backend.** Go has S3 and GCS; `--backend azure` in Go is
  `InvalidArgument`.
- **Constant-time capability checks** (or constant-time RSA). The KV check and
  the GCS RS256 implementation are strict and fail closed, but timing is not
  constant.
- **Multi-writer, multi-node, or fleet operation.** Writers are exclusive by
  design; the fence is a single-writer protocol with no clock-skew defence;
  distributed courts are one host and one emulator instance.
- **Writable FUSE over an object store.** Only read-only mounts over S3 and GCS
  are evidenced.
- **Orbax competitiveness.** The adapter is **23.85x** slower on save and
  **8.10x** on restore, and +0.44 % larger; it is an interoperability and
  exactness result.
- **General-purpose-filesystem performance.** Comparative results include large
  losses (sequence throughput, relation/tag selectors versus SQLite, fsync
  tails). No “faster than ext4/XFS/ZFS” claim.
- **PyPI publication.** No token; the wheel is not on PyPI. The exact upload
  failure is recorded above.
- Production readiness, security audit, or a support commitment of any kind.

## Reproduce

```sh
bash scripts/verify-all.sh
bash scripts/run-conformance.sh scripts/conformance-adapter-rust.sh
bash scripts/run-conformance.sh scripts/conformance-adapter-go.sh
bash scripts/docker-court.sh python "cd /work && WEFT_PYTHONPATH=/work/python bash scripts/run-conformance.sh scripts/conformance-adapter-python.sh"
bash scripts/docker-court.sh python "python/scripts/court-receipt.sh"
bash scripts/publish-crates.sh
bash bench/scripts/azure_court.sh
bash bench/scripts/gcs_court.sh
cd WeftFS-Go && bash scripts/go-s3-court.sh
cd WeftFS-Go && bash scripts/go-gcs-court.sh
bash scripts/docker-court.sh rust "bash scripts/threefs_court.sh --bench"
bash scripts/check-claims.sh
bash scripts/check-spec-drift.sh
```
