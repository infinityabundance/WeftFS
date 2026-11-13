# Azure MW2 mixed-language court (X13/N14)

**Status: implemented and measured; every gate in the subphase passes.** This
document reports an *emulator* court, not a cloud court. The store is one
pinned Azurite Blob emulator on loopback inside the OOM-protected Docker court.
Azurite is not Azure: service-version acceptance, block-blob limits, listing
paging, replication, media behaviour and cloud failure modes are not exercised.

The default writer level is unchanged: **MW1 — many producers, one committer**.
Every scenario here opens its writers with `--writer-mode optimistic` (MW2)
explicitly. Nothing in this work enables Azure MW2 implicitly, and FUSE
mutations plus generic checkpoint publication remain MW1.

The counted results below are emitted from the receipt by
`scripts/perf_numbers.py`; nothing in a generated region is hand-edited.

---

## 1. Verdict

<!-- BEGIN GENERATED:azure-mw2-verdict (scripts/perf_numbers.py) -->
**Verdict: the emulator court passes.** All **8/8** scenarios and all **167/167** checks pass (0 failures) on the pinned Azurite emulator, with a **34.3 s** court wall and a **52,301,824 B** cgroup memory peak. Every scenario is a mixed Rust/Go writer experiment through the opt-in MW2 transaction API; the default remains MW1.
<!-- END GENERATED:azure-mw2-verdict -->

Receipt: `bench/receipts/azure-mw2-20261113T103500Z.json` (stamp
`20261113T103500Z`, WeftFS revision `103dd99c7fd9d23345589ffc484ab5f43223729b`,
WeftFS-Go `a6c1c602448ba88d0230673d67690a793c0fc74c`, whose parent
`61ac5c9` carries the Azure fix). Pinned emulator:
`mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`
(release 3.35.0, image id `sha256:03f7cb26ef2cda2c06ec6760c91975a85460e7bb63277a0df5c9f1d02071ead3`).

**The negative run is retained, deliberately.** With the sweep grace shortened
to 1 s — shorter than a writer's prepare-to-publish interval — a GC pass running
concurrently with mixed writers reclaims prepared blobs and a committed
generation becomes unreadable. Receipt
`bench/receipts/azure-mw2-20261113T103700Z.json` (same two revisions, one
variable changed) records exactly that. It is evidence that the age gate is
load-bearing, and it is not a pass.

## 2. What each scenario does, and what it establishes

<!-- BEGIN GENERATED:azure-mw2-scenarios (scripts/perf_numbers.py) -->
| scenario | experiment | checks | result |
|---|---|---|---|
| **S1** | disjoint mixed-language writers | 30/30 | pass |
| **S2** | conflicting mixed-language writers | 28/28 | pass |
| **S3** | ambiguous success on the conditional head write | 22/22 | pass |
| **S4** | fenced takeover with a suspended stale writer | 18/18 | pass |
| **S5** | held reader never observes a newer generation | 18/18 | pass |
| **S6** | concurrent GC while mixed writers publish | 24/24 | pass |
| **S7** | fail-closed mode detection against a permissive endpoint | 18/18 | pass |
| **S8** | cross-language verification and deterministic parity | 9/9 | pass |
<!-- END GENERATED:azure-mw2-scenarios -->

* **S1 — disjoint mixed-language writers.** Four Rust and four Go writer
  *processes* start together on one generation, two transactions each, disjoint
  names. Every transaction commits; the final head is one of the writers'
  observed heads; both languages report the same head and generation; every
  file is read back byte-exactly by both languages, including cross-reads
  (Rust reads the Go writers' files and vice versa); `verify --full` is clean in
  both languages. A concurrent open may observe another writer's
  not-yet-published commit record and skip that generation number (the store
  never reuses one), so the court asserts `head generation >= genesis + commits`
  and proves the exact state with `verify --full` rather than by arithmetic on
  gaps.
* **S2 — conflicting mixed-language writers.** Six rounds, one Rust and one Go
  transaction writing the *same* name with different bytes. Both processes open
  and prepare, then pause, so both have observed the same head when the race is
  released; a slow open could otherwise skip a generation for a reason
  unrelated to the property under test.
  Each round has exactly one winner, the loser reports a typed `Conflict` (never
  a silent success), the winner's bytes are exactly present, and the head
  advances by exactly one generation. No round ends with both or neither.
* **S3 — ambiguous success on the conditional head write.** The harness proxy
  forwards the conditional head `PUT`, applies it upstream, writes a marker, and
  withholds the response. The writer has the `WEFT_AZURE_AMBIGUOUS_READY` fault
  point armed and sends itself `SIGKILL` when the marker appears, so it dies
  after issuing the conditional write and before consuming the answer. The
  transaction record is left `prepared`; a lookup in the *other* language
  reports `unknown` (neither success nor failure); re-running the same
  transaction id resolves it by op digest and reports the already-published
  commit without publishing a second generation; the bytes are exactly present
  and `verify --full` is clean. Run Rust-killed/Go-resolved and
  Go-killed/Rust-resolved.
* **S4 — fenced takeover.** A single-writer open is suspended at the shipped
  fence hook (`WEFT_FENCE_PAUSE_AFTER_FENCE`), the lease is shortened to 3 s, a
  successor in the *other* language reclaims it and publishes, the stale writer
  is resumed, and it fails `Busy` with the head unchanged and no file published.
  Run Rust-stale/Go-successor and Go-stale/Rust-successor.
* **S5 — stale reader.** A read handle is opened and held at generation G while
  newer transactions publish. The held handle still reports G and never
  observes the newer object; fresh opens in both languages do observe it and
  read the newer bytes.
* **S6 — concurrent GC while mixed writers publish.** A snapshot root is
  published, an unreachable file is created after it and then deleted, and the
  garbage is aged past the sweep grace. Two Rust and two Go writers then publish
  while three `gc --apply` passes run. Every writer succeeds, every GC pass
  succeeds, GC reclaims the unreachable blobs, `verify --full` is clean in both
  languages, the snapshot-protected object reads back exactly by both
  languages, and every writer's files are still exact. The root used is a
  snapshot because a *checkpoint* or a generic *provenance* publication is an
  MW1 surface in this version: the `writers` policy record refuses a
  single-writer handle on an optimistic namespace, so an MW2 store can only be
  given snapshot roots through its own transaction API. The GC root machinery
  is the same one; it is the publication path that differs.
* **S7 — fail-closed mode detection.** A network fake drops `If-Match` or
  `If-None-Match` and **re-signs** the modified request with the emulator's
  development key (the Blob SharedKey string-to-sign includes both headers, so
  a modified request must be re-signed or the emulator would answer 403 instead
  of accepting the precondition). A positive control against the real endpoint
  succeeds; the same writable open then must refuse `UnsupportedVersion` against
  the fake, for both languages and both modes, and for both the single-writer
  and optimistic open. The real client is not weakened anywhere.
* **S8 — cross-language verification and deterministic parity.** The same
  transaction in Rust and Go, on separate containers, produces byte-identical
  `head`, `manifest`, `content_root`, `object_id`, `entry_id`, `version` and
  `size`; both languages verify a store each of them wrote; and each language
  reads the other's parity store.

## 3. Defects found and fixed

Both were found by the court, not by inspection, and both are fixed at the root
cause.

### 3.1 The `tmp/` sweep deleted live capability probes (S1, first run)

`AzureBackend::clean_stale` and `azureControl.CleanTmp` deleted **every** blob
under `tmp/` and ignored the `grace_secs` argument they were given. The comment
in both implementations asserted that the writer lock is held, which is true for
MW1 and false for an optimistic open. Four concurrent Rust writers therefore
deleted each other's live capability-probe blobs; each then saw its second
create-once probe succeed and refused a healthy Azurite as
"endpoint ignored If-None-Match: \*". The Go writers lost the same race from the
other side.

The sweep is now age-gated by the listing's `Last-Modified`, and an entry whose
age cannot be established is kept. Blast radius: open-time `tmp/` hygiene only.
An abandoned probe now persists until it is older than the grace window
(3600 s by default) instead of being deleted by the next open. No commit,
blob, format, wire byte or MW1 default changes. The Rust S3 and GCS sweeps still
carry the old, un-gated premise; see §5.

### 3.2 GC could never reclaim anything on Azure (S6, first run)

`AzureBackend::modified` returned `None`, with a comment saying the crate had no
HTTP-date parser. The store's GC age gate computes `age` as
`modified -> None => 0`, and `0 < grace`, so **every** unmarked blob was treated
as fresh and a sweep on Azure reclaimed nothing regardless of configuration.
The crash surface is small but the function was simply unimplemented.

`modified` now `HEAD`s the blob and parses `Last-Modified`
(`parse_rfc1123_secs` + `days_from_civil`, both new and unit-tested through the
court). Blast radius: GC's candidate-age read. No other caller relies on a
missing age.

## 4. The negative result, kept

<!-- BEGIN GENERATED:azure-mw2-negative (scripts/perf_numbers.py) -->
The retained negative run passed **7** of eight scenarios and **163/167** checks, with **4** failures — all of them in S6, and all of them reachable-data losses.
<!-- END GENERATED:azure-mw2-negative -->

With `WEFT_AZURE_MW2_GC_GRACE=1` the S6 experiment loses live data: a Go
writer's generation is rejected at `verify --full` because a manifest blob it
had prepared is gone, and that writer's three files are unreadable afterwards.
This is the documented contract failing under a configuration outside it: a
blob older than the grace window is presumed abandoned, and a transaction whose
prepare-to-publish interval exceeds the grace window can therefore lose its
blobs to a concurrent sweep. The MW2 surface has no exclusive-GC coordinator, so
an operator must size `gc_grace_secs` above the transaction latency (or run GC
while writers are quiescent). The court therefore ages its garbage past a 10 s
window and keeps the 1 s run as evidence instead of relaxing any check.

A stronger fix — refusing to sweep when the authoritative head has moved since
the mark — was considered and not implemented in this subphase: with writers
publishing continuously every pass would abort, the court would then reclaim
nothing while writers run, and the S6 property would be untested rather than
fixed. It is recorded as the next design step.

## 5. Limits, losses and what is not claimed

* **Azurite is not Azure.** No AWS/Azure/GCS live run, no cloud, fleet,
  multi-node, replication, media, power-loss, device-cache or serving claim.
* **Process faults only.** The ambiguous-success case is a real `SIGKILL` at a
  real publication boundary, but it is a process fault, not a power cut.
* **Single writer proof style.** S4 exercises the fenced *writer lease* (MW1);
  MW2 has no lease by design, and the S4 head-unchanged requirement is about the
  fenced lease, not about two MW2 committers racing (S2 covers that).
* **S1's generation arithmetic is coarse on purpose.** Concurrent opens can skip
  a generation number; only `verify --full` proves the state exactly.
* **The S3 fault point lives in the court driver, not the library.** A library
  hook could not make "issued but not consumed" literal with the HTTP client in
  use; the proxy withholds the response and the driver kills itself on the
  marker. The hook is inert unless its environment variable is set.
* **Latency numbers are absent on purpose.** This is a correctness court. Its
  receipt records one duration for the whole run and a cgroup peak, both
  generated above; no per-operation latency is claimed, and none should be
  compared with the R4 MW2 benchmark, which ran on a local plane.
* **Residual, not fixed here:** the Rust S3 and GCS `clean_stale` sweeps still
  delete every `tmp/` blob regardless of age, with the same false premise; they
  are latent for the same reason (`--writer-mode optimistic` is implemented for
  `s3`). Go's S3/GCS sweeps have the same shape. Only Azure was in scope and
  exercised.
* **Residual:** MW2 GC is not coordinated with concurrent committers beyond the
  age gate (see §4). No claim of a safe arbitrary-concurrency GC is made.

## 6. Reproduction

Host half (starts the pinned emulator, runs the dual-toolchain court, writes the
receipt and the log):

```bash
bash scripts/azure_mw2_court.sh
```

Negative run (same revisions, one variable):

```bash
WEFT_AZURE_MW2_GC_GRACE=1 WEFT_AZURE_MW2_STAMP=<stamp> bash scripts/azure_mw2_court.sh
```

Both run in the `fuse` court image under the unchanged hard limits
(3 GiB / 3 GiB swap, 4 cpus, 1024 pids, bounded 1 GiB `/tmp`, `cap-drop=ALL`,
`no-new-privileges`); the emulator is a sibling container with the same limits
and a 1 GiB tmpfs data directory. The court prints exactly one
`azure_mw2_court: skip <reason>` line and exits 78 when the image or the Docker
daemon is unavailable, and the aggregate records a skip — never a pass.
