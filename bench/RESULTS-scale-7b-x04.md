# X04 — 7B ingestion: bounded, planned, and with the catalog cost fixed

**Status:** executed on `staging` in the unchanged 3 GiB bench court. No frame,
manifest, commit, index, identity byte, mode number or root changed. The new
encoders are *schedules*: the streaming mode-9 and chunked-ZSTD outputs are
byte-identical to the historical ones, and `tests/streamed_plan_identity.rs`
compares the complete store trees of the slice and streamed planned writers.

**What was wrong.** The measured starting point is receipt
[`scale-7b-20261110T213731Z.json`](receipts/scale-7b-20261110T213731Z.json):
per-tensor **planned** ingest of Qwen2.5-7B-Instruct was OOM-killed (exit 137) at
3 GiB on the first tensor, and a **packed** fast-raw save of the same checkpoint
wrote 148,694 catalog segment blobs for 911 packs, +51.16 % bytes over the source.
The R21/R22 work fixed the 424 MB fixture's wall; it did not touch either number.

**What changed.**

1. A bounded streaming mode-9 encoder (`weftcodec::modes::encode_float_tensor_stream`)
   and a bounded streaming chunked-ZSTD encoder (`encode_zstd_stream`), both
   driven by a `ByteSource` and both byte-identical to the one-shot encoders.
2. `Store::prepare_tensor_object_streamed` / `write_tensor_file_from_source`:
   the automatic two-stage plan over a bounded source. Same candidates, same
   decisions, same encoders.
3. The packed blob plane now publishes the catalog **once per flush** (the store
   flushes before every commit record) instead of once per sealed pack.

---

## 1. Method

* **Corpus.** Qwen/Qwen2.5-7B-Instruct, four published shards merged into one
  15,231,272,088 B container by `bench/scripts/safetensors_merge.py` (which
  verified every tensor against its source shard). SHA-256
  `5527ae977f6b2a2fc0add1d76834f1300a607f79aa4bdce1457bd738523c1ef1`, re-checked
  by the court before every run. 339 BF16 tensors, largest 1,089,994,752 B.
  The corpus is read in place from `.docker-cache/real-corpus/` and never copied.
* **Court.** `scripts/docker-court.sh bench`: 3 GiB memory, 3 GiB swap, 1024
  pids, 4 CPUs, 1 GiB `/tmp`, `cap-drop=ALL`, `no-new-privileges`. **No limit was
  raised for 7B.** Each phase is its own process, so `VmHWM` and the cgroup peak
  are honest per phase; the cgroup peak is dominated by reclaimable page cache
  and is reported beside the process peak, never instead of it.
* **Reproducibility.** The court harness uses a fixed clock
  (`fixed_timestamp_ns = 1,700,000,000,000,000,000`), a fixed per-tensor ingest
  seed (`H("weft/scale7b/ingest-seed/v1", tensor name)`) and a fixed checkpoint
  seed (`0x5a * 32`), so two runs of the same logical write can be compared by
  head. That is how slice-vs-streamed and flat-vs-packed identity are claimed.
* **Stage attribution.** `WEFT_PHASE_PROFILE=1` records the seven existing
  profiler phases (`source_read`, `digest`, `encode`, `blob_publish`,
  `manifest_commit`, `index_segment`, `verify`) plus the nested encode classes,
  so every wall number below can be attributed to a stage.
* **Disk discipline.** `df -h` is printed at the start, every phase declares the
  bytes it needs and refuses to start without them, exactly one store is alive at
  a time, and each phase's store is deleted before the next phase starts.

## 2. Streaming mode-9: byte-identical, less memory, and faster

<!-- BEGIN GENERATED:scale7b-mode9 (scripts/perf_numbers.py) -->
**Byte identity.** Both real tensors encode to the same frame either way: the
272,269,312 B Qwen2.5-0.5B embedding to 180,547,519 B, and the
1,089,994,752 B Qwen2.5-7B `lm_head` to 726,141,127 B.

**Qwen2.5-0.5B embedding**: one-shot **478.083 ms** at
**444,652 KiB** peak RSS; streamed **386.554 ms** at
**179,896 KiB** — **2.472×** less memory and
**0.809×** the one-shot wall.

**Qwen2.5-7B `lm_head`**: one-shot **1,950.528 ms** at
**1,776,132 KiB**; streamed **1,526.492 ms** at
**712,720 KiB** — **2.492×** less memory and
**0.783×** the one-shot wall.

**Counting pass only** (histogram + content hash + exact frame length): the 1.09 GB tensor
in **269.019 ms** (**4,051.7 MB/s**) at
**3,856 KiB** peak RSS; the 272 MB tensor in
**67.878 ms**.
<!-- END GENERATED:scale7b-mode9 -->

The streamed encoder is faster *and* smaller because it never holds the
plaintext: the historical encoder materialises the payload and then allocates the
frame beside it, while the streamed encoder reads a 1 MiB window and writes both
planes straight into the exactly-sized frame buffer. There is **no slowdown to
report**; the 1.09 GB tensor drops from 1.69 GiB to 696 MiB of peak RSS.

Both real frames are byte-identical (`frame_sha256` equal in the receipt's
checks, 3 repetitions each), and `WeftCodec/tests/mode9_streaming.rs` proves the
same over five dtypes, twelve element counts (including primes and non-multiples
of any window) and five window sizes, plus all 290 tensors of the real
Qwen2.5-0.5B-Instruct container at two windows
(`WeftCodec/tests/mode9_streaming.rs`,
`every_real_qwen_tensor_streams_byte_identically`).

## 3. Planned 7B ingest at 3 GiB

<!-- BEGIN GENERATED:scale7b-planned (scripts/perf_numbers.py) -->
**Planned 8-way checkpoint write (339 tensors, 2,712 shard objects):** wall
**119,174.9 ms** (127.8 MB/s of source bytes), process peak
**442,672 KiB**, cgroup peak 3,221,225,472 B (the court limit, page
cache), store **10,160,456,499 B**.

Of that wall, `verify --full` is **51,767.2 ms**, publication
**29,984.4 ms**, encoding **16,797.0 ms** (mode 9
6,949.9 ms) and the commit's durability barrier **12,523.8 ms**.
`verify --full` checks 2,712 objects and 2,032 representations.

**Whole-tensor streamed ingest (`ingest --stream`), the same 339 tensors:** wall
**220,047.8 ms** (69.2 MB/s), process peak **2,873,124 KiB**,
store **10,162,716,887 B**, mix 240 mode 9 /
59 ZSTD / 40 raw. `verify --full` is
**49,296.1 ms** of that wall, publication **139,191.0 ms** and
mode-9 encoding **14,398.4 ms**.

**The recorded starting point** (`scale-7b-20261110T213731Z`): per-tensor planned ingest
**exit 137**, fast-raw flat **1,406,533.6 ms** /
**173,032 KiB** at **15,247,327,175 B**, fast-raw packed
**1,088,654.1 ms** / 287,420 KiB at **23,023,484,778 B**
with **148,694** catalog segment publishes and
**911** root publications.
<!-- END GENERATED:scale7b-planned -->

**The headline: the planned 8-way checkpoint write of all 339 tensors fits at
3 GiB.** The generated row above carries the wall, the process peak and the
footprint; `verify --full` reports no problems on any object or representation.
Read the process peak against the court's 3 GiB: the write has room to spare.
The store is smaller than the source container because the shards that could
profit from a codec representation picked one instead of raw chunks.

**The whole-tensor path also fits now, but only just.** The pre-X04 per-tensor
planner needed the raw bytes plus every candidate frame; the streamed writer
needs the frame only. Its process peak is *not* the ingest loop: the per-tensor
row for `lm_head.weight` in `scale-7b-20261113T105331Z` (stamp
`20261113T105331Z`, revision `cbfe86f6c90a448e591ac062af1b36c5acdd4d10`) records a
lower `VmHWM` than the process peak, which is reached later inside
`verify(true)`. That is the honest ceiling: **a whole-tensor 7B planned ingest
fits the 3 GiB court, and `verify --full` of a large whole-tensor mode-9
representation — not the write — is what consumes the last gigabyte.** A
`verify` that decodes in bounded windows would remove that; it is not
implemented here.

**Delta candidates.** A fresh 7B checkpoint has no causal base, so weight/sparse/
semantic/patch deltas are not offered at all — the planner offers raw, ZSTD-3,
ZSTD-19 and mode 9. With a causal base the sharded writer binds residency to a
shard: the corpus's largest tensor is 1,089,994,752 B (a corpus fact recorded in
§1), so at 8 shards a delta candidate can hold at most
2 × ceil(1,089,994,752 / 8) = 272,498,688 B of base plus target (arithmetic on
that fact). The streamed whole-tensor writer refuses a causal base with a typed
error naming the sharded writer.

## 4. The catalog amplification (N15)

<!-- BEGIN GENERATED:scale7b-catalog (scripts/perf_numbers.py) -->
**After (this change).** Packed 7B write: wall **258,071.4 ms** (59.0 MB/s),
process peak **404,812 KiB**, store **15,274,042,488 B** for
**15,242,664,309 B** of packed data: **236,237** members in
**911** packs, catalog **26,707,069 B** in **256**
segment files, **256** segment
publications and **1** root publication.
17 members (2,721,040 B) bypassed packing because they
exceed the member threshold; the catalog phase's own read of the store reports
15,274,042,488 B.

**Before (R12).** Packed 7B write: wall **1,088,654.1 ms**, peak
**287,420 KiB**, store **23,023,484,778 B**,
**148,694** segment publishes and **911** root
publications — +51.16 % bytes over the source, quoted from that receipt's own analysis.
<!-- END GENERATED:scale7b-catalog -->

**Diagnosis.** The amplification was never per-entry overhead. It was
**per-seal republication**: with 256 catalog segments and members hashed across
all of them, every pack seal touched nearly every segment, and each touched
segment was written out in full, growing as it went. A save with hundreds of
seals therefore wrote the whole catalog hundreds of times over. The generated
"before" row above carries that run's store bytes and publication count.

**Fix.** The packed plane buffers a sealed pack's members and publishes the
catalog once, from `flush`, which the store calls before every commit record. The
ordering invariant is unchanged — the pack object is durable *before* any catalog
record names it, and the root is still a single compare-and-swap — so a crash
still leaves unreferenced packs, never a dangling entry. Pending members stay
readable through the writing handle, and GC now treats their packs as referenced.

**Identity.** The pack layout is untouched (`tests/packs.rs`,
`packing_does_not_change_state_or_identity` and
`the_catalog_is_published_once_per_flush_and_ends_identical`): both runs report 911 packs,
236,237 members and the *same* `packed_bytes`. Flat and packed 7B writes publish
the same head
(`8cd86ce5171c3214c65bf778aeb3da5a8732061e7625f25eeb1d41af7ffda2ad`), the same
`manifest_root` and the same `checkpoint_id`; the only footprint difference is
the catalog (arithmetic on the two generated store totals above).

**Remaining overhead.** One catalog segment blob per segment index, each the full
merged segment, plus one small root control record (the generated rows carry both
counts and both byte totals; divide them for the ratio). The catalog still has a
hard bound (`--pack-catalog-bytes`, set to 64 MiB in this court) and falls back
to flat publication beyond it.

## 5. Restore

<!-- BEGIN GENERATED:scale7b-restore (scripts/perf_numbers.py) -->
Restores from the packed store, all with `sha256_match: true` against the source container:
1 worker **350.5 MB/s** (43,452.9 ms, 2,254,132 KiB peak);
4 workers **387.8 MB/s** (39,276.1 ms, 2,433,784 KiB);
8 workers **377.9 MB/s** (40,303.3 ms, 2,511,704 KiB).

Fast-raw **flat** write for comparison: **783,916.2 ms** at 19.4 MB/s,
236,263 backend writes, **464,189.0 ms** of it in the single durability
barrier, peak **349,608 KiB**, store **15,247,327,175 B**.
<!-- END GENERATED:scale7b-restore -->

Every restore reports `sha256_match: true` against the source container and
reads the whole packed store through the pack plane. Nothing regressed against
the R12 numbers (337.7 / 367.5 / 367.8 MB/s at 1/4/8 workers, quoted from
`scale-7b-20261110T213731Z`, stamp `20261110T213731Z`, revision
`118015f364d05cc51af5d978a28bf469913303ae`): 4 workers is still the best of the
three and 8 is still slightly worse, which is the same one-backend-mutex-plus-
decode-threads limiter R10 recorded.

## 6. Losses, limits and non-claims

* **`verify --full` is the 7B memory peak for a whole-tensor store.** The
  generated streamed-ingest row is that process peak, and it is reached inside
  `verify(true)`, not in the ingest loop (the per-tensor row quoted in §3 shows
  the lower loop peak). It decodes one representation at a time, but a large
  whole-tensor mode-9 representation needs its plaintext. Not fixed here.
* **The streamed whole-tensor path is close to the ceiling.** A larger tensor, or
  a court with less headroom, would fail. The sharded checkpoint writer is the
  path with margin (its generated peak is a small fraction of the court).
* **Flat fast-raw is now much slower than packed** — the generated rows above
  carry both walls and both store sizes. The packed plane turns the flat path's
  individual small-file writes into one pack write per pack, and the flat wall is
  dominated by a single durability barrier over all of them. The flat path itself
  was not changed.
* **Flat peak RSS rose.** The flat path was not modified by X04, and the R12
  receipt predates R18–R22 (two-stage planning, the pipeline). The two peaks are
  in the generated rows; the cause is **not attributed** here.
* **Packed peak RSS rose** for the same 7B save because the deferred catalog
  holds the member entries and the segment buffers until flush (see the two
  generated packed peaks at the top of this document; the "before" one is from a
  different code revision).
* **Snapshot of the change, not a fleet claim.** One host, warm page cache, one
  loopback filesystem at 98 % capacity; no media, power-loss, multi-node, cloud,
  GPU or serving claim. The corpus is a public checkpoint, not a production
  training run.
* **No Go parity** for the streaming encoders or the catalog deferral. The
  pipeline worker already had a deferred mode; the sequential path is Rust-only
  in this change.
* **The streamed writer implements the automatic plan only.** Pinned strategy
  hints, the fast policy, causal deltas, tile shapes, experimental modes, `q8_0`
  and non-row-major layouts are refused with a typed error, not silently
  approximated.
* **The catalog fix does not rewrite existing packs.** A store written before
  this change keeps its superseded segment blobs until GC reclaims them
  (`reclaim_unreferenced_packs` deletes catalog segments not named by the root).

## 7. Receipts

| Receipt | What it is |
|---|---|
| [`mode9-stream-20261113T113138Z.json`](receipts/mode9-stream-20261113T113138Z.json) | **cited.** Streaming mode-9 vs one-shot on two real tensors, 3 repetitions each, frames compared byte-for-byte. |
| [`scale-7b-20261113T104950Z.json`](receipts/scale-7b-20261113T104950Z.json) | **cited.** Planned 8-way checkpoint write of all 339 tensors at 3 GiB, with `verify --full`. |
| [`scale-7b-20261113T105331Z.json`](receipts/scale-7b-20261113T105331Z.json) | **cited.** The same checkpoint through the bounded whole-tensor writer (`ingest --stream`). |
| [`scale-7b-20261113T105851Z.json`](receipts/scale-7b-20261113T105851Z.json) | **cited.** Fast-raw packed write, catalog accounting, and restores at 1/4/8 workers. |
| [`scale-7b-20261113T110624Z.json`](receipts/scale-7b-20261113T110624Z.json) | **cited.** Fast-raw flat write and `verify --full`; its head is the packed run's head. |
| [`scale-7b-20261110T213731Z.json`](receipts/scale-7b-20261110T213731Z.json) | **cited as the starting point only.** Pre-X04: per-tensor planned ingest exit 137, packed +51.16 % bytes, 148,694 segment publishes. Its code revision predates R18–R22, so its timings are not compared per stage. |
| `mode9-stream-20261113T112342Z`, `…T112350Z` | **Retained failures: court-script bugs.** A doubled `/work` prefix and a `chmod` on a host-owned directory. Both wrote measurements; neither is cited. |
| `mode9-stream-20261113T112517Z`, `…T112913Z` | **Retained: superseded.** They lack the court revision fields, so the receipt cannot be pinned; the measurements are the same kind and are not cited. |

## 8. Reproduce

```sh
# streaming mode-9 (two real tensors, 3 reps, byte-identity gate)
bench/scripts/mode9_stream_court.sh

# planned / streamed / flat / packed / catalog / restore at 7B
WEFT_PHASE_PROFILE=1 WEFT_SCALE_PHASES="planned" bench/scripts/scale_7b_court.sh
WEFT_PHASE_PROFILE=1 WEFT_SCALE_PHASES="ingest_streamed" bench/scripts/scale_7b_court.sh
WEFT_PHASE_PROFILE=1 WEFT_SCALE_PHASES="packed catalog restore_1 restore_4 restore_8" \
  bench/scripts/scale_7b_court.sh
WEFT_PHASE_PROFILE=1 WEFT_SCALE_PHASES="flat verify_flat" bench/scripts/scale_7b_court.sh

# the unit-level proofs
scripts/docker-court.sh rust "cd /work && cargo test --test streamed_plan_identity"
scripts/docker-court.sh rust "cd /work && cargo test --test packs"
scripts/docker-court.sh rust "cd /work/WeftCodec && cargo test --test mode9_streaming"
```

The 7B court needs about 31 GB of free disk (15.24 GB corpus + one 15.3 GB
store). It is **not** registered in `scripts/verify-all.sh`: the aggregate runs
38–45 courts inside one budget and a 7B store does not fit beside the corpus on
this host at 98 % capacity. The two fast courts
(`mode9_stream_court.sh`, `tests/streamed_plan_identity`) are the regression
guards; the 7B court is the scale court.
