# Claim audit — README and v1.2.0 release note

**Status:** first performed at R13 (2026-11-11) and re-run at the v1.4.0 tree
(2026-11-12). This is an adversarial audit of the externally visible claims in
[`README.md`](../README.md) and the release notes.

**Audit revision.** The rows below were written against the trees they name —
mostly the R13 tree, whose aggregate was 45/45, and the v1.3.0 tree. The
current aggregate is **46/46** (`verify-20261112T223540Z`) and the current test
counts are in the generated block at the top of [`STATUS.md`](STATUS.md). A
number in a row below is the value at that row's revision, not a current claim;
current claims are generated from receipts by `scripts/perf_numbers.py`.

Method: each claim was opened against the receipt or specification it cites.
Where a receipt is machine-readable, the cited number was re-read from the JSON
or recomputed from its log; prose in `bench/RESULTS-*.md` was not treated as
evidence. "Revision" is the commit recorded in the receipt (or the tag for a
historical release claim). A claim that only held at a historical revision is
marked **historical**, not silently rewritten. A claim that did not match its
receipt was corrected in place.

The audit found **ten current-state README claims that did not match their evidence**
and two material claims in other documents. All corrections are listed in §3.

---

## 1. README claims

| # | Claim (abridged) | Location | Evidence | Revision | Verdict |
|---:|---|---|---|---|---|
| A1 | Four independent implementations share one normative format. | README intro | `spec/` + four repositories; cross-language gates in the 42/42 receipt | `verify-20261110T234609Z` | ✔ — qualified: Go does not implement every store surface (see A2). |
| A2 | "Frozen on-disk and wire formats are byte-identical across Rust and Go." | README intro | Cross gate and conformance receipts show byte identity for the frozen wire modes both implement; Go gaps are explicit in STATUS. | `verify-20261110T234609Z`, `conformance-rust/go-20261111T011110Z` | **Corrected.** Overbroad. Now: byte identity where both implement the surface; Go gaps named. |
| A3 | MW2 provides "linearizable head publication". | README writer section | TLA+ model (`tla-20261109T174959Z`) and finite-history check (`multiwriter-court-20261109T175012Z`, "finite history check, not a linearizability proof"). | `a3256af` | **Corrected.** Now: one authoritative head sequence, model-checked and finite-history tested, not a machine-checked linearizability proof. |
| A4 | Retention is a binding, not an identity; class-aware GC computes reachability first. | README retention paragraph | `spec/WEFT-RETENTION.md`; `retention-20261109T220237Z` | `9de0e09` | ✔ |
| A5 | Local POSIX backend: implemented and tested on a real backend. | Integration status | `backend-20261104T232908Z` (identical state/bytes, crash proxy, verify) | `8683ecc` | ✔ |
| A6 | MinIO S3: implemented and tested on a real backend. | Integration status | `s3-court-20261109T112958Z` | `112958Z` run | ✔ (single node, tmpfs; not AWS). |
| A7 | Ceph RGW 19.2.0: implemented and tested on a real backend. | Integration status | `ceph-rgw-20261109T112958Z` | `112958Z` run | ✔ (single node; not a cluster claim). |
| A8 | `fake-gcs-server`: emulator-tested. | Integration status | `gcs-court-20261107T034113Z` | `034113Z` run | ✔ (emulator, not Google Cloud). |
| A9 | Azurite 3.35.0: emulator-tested. | Integration status | `azure-20261107T034219Z` | `034219Z` run | ✔ (emulator, not Azure; SAS is effectively read-only). |
| A10 | 3FS: ABI/conformance-tested only, not a deployed cluster. | Integration status | `threefs-20261107T020623Z` | `020623Z` run | ✔ |
| A11 | Buck2 CAS adapter: prototype, correctness 41/41. | Integration status | `buck2-cas-store-correctness-20261109T113544Z` (`passed: 41, total: 41`) | `113544Z` run | ✔ (not a Buck2 plugin, no REAPI). |
| A12 | DAOS/Lustre/BeeGFS/CephFS/JuiceFS/IBM/WEKA/VAST/AIStore: designed, not executed. Live clouds inaccessible. | Integration status | No adapter/court; `docs/AI-STORAGE-AUGMENTATION.md` §7 | — | ✔ (stated as non-claims). |
| A13 | Qwen2.5-0.5B family: 1,276,128,956 B, 17 % smaller than 1,535,945,154 B per-tensor zstd-19; 3.07 % under ZipNN. | What is measured | `planner-float-20261106T035316Z` | `ba5a0e6` | **Fails at R24.** No receipt in the tree contains 1,535,945,154; the sourced baseline is **1,528,909,007** (`real-model-20261105T012000Z`), i.e. **16.5 %**. The claim is now generated from that receipt; the historical figure remains only in the v1.2.0/v1.3.0 notes. |
| A14 | Embedding 1-row read 749.26 → 2.12 ms (353×) and 100-row 777.08 → 2.30 ms (337×) at +0.51 % bytes. | What is measured | `float-tiled-20261107T042955Z` | `042955Z` run | ✔ |
| A15 | "41/41 verification courts ... 355 conformance vectors." | What is measured | Current receipt is 45/45; current manifest is 420. The 41/41/355 pair was the R4 tree. | `verify-20261111T131235Z`; manifest `6287b817…` | **Corrected.** Now 45/45 and 420. |
| A16 | Multiwriter: 4,000 tx/s at N=1 → 638 tx/s at N=32, efficiency 0.0050; hot name one winner; MinIO N=4 212.8 tx/s. | What is measured | `bench-multiwriter-20261109T150423Z` rows N=1/N=32 and S3 N=4 | `b0bcbc5` | ✔ (later runs measured 542–580 tx/s at N=32; the cited receipt is the one quoted). |
| A17 | Pythia step0→1: 42,211 B vs 30,265 B whole-file patch-from; the baseline is 28 % smaller. | Demonstrated results | `family-lifecycle-20261108T053655Z` | `053655Z` run | ✔ (42,211 / 30,265 = 1.395, so the patch is 28.3 % smaller). |
| A18 | Model family stores in 1.276 GB vs 1.536 GB independent zstd-19, 17 % smaller, 3.07 % under ZipNN. | Demonstrated results | same as A13 | `ba5a0e6` | **Fails at R24** for the same reason as A13; superseded by the generated bullet (1,528,909,007 B, 16.5 %). |
| A19 | Region reads: 749.3 → 2.12 ms, +0.51 % bytes, 3.25× write. | Demonstrated results | same as A14 (`write_ms` 2,286.7 → 7,434.7) | `042955Z` run | ✔ |
| A20 | "60 codec, 20 WCB, 22 selector, 16 context, 10 causal, 36 adversarial vectors." | Status P2 | Manifest counts: codec 91, WCB 20, selector 22, context 16, causal 10, adversarial 36. | manifest `6287b817…` | **Corrected.** 60 → 91. |
| A21 | Rust store core: "39 core tests". | Status P4 | 45/45 receipt log: 771 tests across 158 binaries, 0 failed. | `verify-20261111T131235Z` | **Corrected.** 39 → 771. |
| A22 | Rust FUSE: "232 Rust FUSE-court tests pass". | Status P5 | 45/45 receipt log: 814 passed, 0 failed, 2 ignored. | `verify-20261111T131235Z` | **Corrected.** 232 → 814. |
| A23 | Go: "31 Go tests + 13 GC tests". | Status P6 | R12 records 125 top-level Go tests, 7 skipped under `-race`. | `docs/STATUS.md` R12; `verify-20261110T234609Z` | **Corrected.** 31+13 → 125 top-level (7 skipped). |
| A24 | "Latest full verification at the release tree: 37/37." | Status P7 | 37/37 is the release tree; the latest is 45/45. | `verify-20261109T112958Z` vs `verify-20261111T131235Z` | **Corrected.** Both stated, with which tree each covers. |
| A25 | 1,000 concurrent context mounts, 4,250 file checks, 1,000 digests, at 52g/8/16384; 692/1000 at 24g/8/4096. | Status F3 | `context-mounts-20261105T122643Z`, `…131116Z`, `…140342Z` | `6e4aea1` | ✔ (explicit overrides recorded; not a default-court fit). |
| A26 | 100,000/100,000 crash iterations, 19.2 it/s, 0 partial. | Status F5 | `crash-torture-20261104T104233Z` | `6316fd4` | ✔ (process-fault proxy, not physical power loss). |
| A27 | AI codec modes 6–20 implemented; mode 20 update 4/64 groups 62.7 ms / 16.79 MB vs 499.7 ms / 118.73 MB. | Status F6 | `engram-20261109T232921Z`; conformance mode 20 6 valid / 14 reject | `232921Z` run | ✔ (and loses whole-table size/decode to ZSTD). |
| A28 | ai-v1: 60 files, 1.94 MB source / 1.52 MB visible, synthetic. | Status F7 | `ai-corpus-20261104T021627Z`; `bench/corpora/ai-v1` | `021627Z` run | ✔ |
| A29 | Buck2 CAS prototype 15/15; agent memory 7/7; mode 8 byte-identical. | Status F8 | `buck2-cas-20261104T022139Z`, `agent-memory-*`, cross gate | F8 tree | ✔ (prototype; not a plugin). |
| A30 | TLA+ model with buggy counterexample; QEMU/dm-flakey/blkdebug; ZFS/Btrfs/ext4/XFS; SQLite. Physical power loss and RAPL not claimed. | Status F9 | `tla-20261109T174959Z`; block-layer and `fio` receipts; `RESULTS-comparative-v1` | F9 tree | ✔ |
| A31 | "parallel reconstruction is not implemented." | Status F10 | R2 introduced bounded parallel restore (`parallel-restore-20261106T043524Z`); R10 added pipelined restore (`throughput-20261110T134917Z`). | `ba5a0e6`, `bc86abd` | **Corrected.** Now names bounded parallel and pipelined restore; materialize remains writeback-bound. |
| A32 | "no headline performance claim is made." | Status P7 | The same README has a "What is measured" table with headline numbers. The intended claim was no *superiority* claim. | — | **Corrected.** Now "no performance-superiority claim is made". |

---

## 2. v1.2.0 release-note claims

The release note is a historical document pinned to the tag. Claims below are
checked at the tag, not at the R13 tree; where the current tree differs, that is
recorded, not rewritten.

| # | Claim (abridged) | Evidence | Revision | Verdict |
|---:|---|---|---|---|
| R1 | Aggregate `verify-20261109T112958Z`: 37/37 pass, 0 failed, 0 skipped. | Receipt `summary` | `135a382` | ✔ |
| R2 | The committed manifest has 355 entries. | `git show 122d86e:conformance/manifest.json` → `vector_count: 355`, aggregate `9818b5d3…` | tag `122d86e` | ✔ **historical**; current manifest is 420 / `6287b817…`. |
| R3 | Codec cross gate: 105 `PASS`, 14 `YES byte-identical`, all checks passed. | `verify-20261109T112958Z.log`, `codec_cross_compare` section | `135a382` | ✔ |
| R4 | Mode 18 table: physical bytes 180,623,816 / 181,541,557 / 181,598,555 / 182,211,848 / 183,446,037 / 185,923,029; 1-row 309.1 / 0.133 / 0.256 / 0.321 / 0.336 / 0.395 ms. | `mode18-20261107T165215Z` | `165215Z` run | ✔ |
| R5 | S12 3B ingest: 2,765,636 KiB peak, 976,527 ms wall, verify clean; data 6,775,861,261 B. | `family-lifecycle-20261108T143027Z`, `s12-bounded-ingest-20261108T143027Z` | `143027Z` run | ✔ (reduced configuration stated in the receipt). |
| R6 | S13b: the 850 ms SLA is accepted (all-independent) and measured 886.5 ms — a miss. | `parent-graph-20261108T232016Z` `sla_sweep` accepts 850 ms; `realizations` records 886.5 ms | `232016Z` run | ✔ (the defect S13b publishes). |
| R7 | PyPI 1.2.0, crates.io 1.2.0, GHCR `:v1.2.0` published. | `release-artifacts-20261109T124325Z` | tag `122d86e` | ✔ **receipt-recorded**; live external state was not re-queried in R13. |
| R8 | No live AWS, Google Cloud, or Azure run. | `docs/RELEASE-v1.2.0.md` §"Live cloud services"; integration matrix | — | ✔ |
| R9 | One observed defect: the published PyPI long description is stale. | Release note text; no receipt can prove a registry's prose | — | ✔ (honest self-report; not independently rechecked). |
| R10 | No frozen wire or on-disk byte changed in 1.2.0. | Cross gate; conformance manifest unchanged at the tag; `weft.manifest.v1`–`v4` fixtures | `135a382` | ✔ |

---

## 3. Corrections applied

### README (10 current-state claims)

1. **A2** — removed the blanket "byte-identical across Rust and Go" claim and
   named the Go gaps.
2. **A3** — replaced "linearizable head publication" with "one authoritative
   head sequence", explicitly not a machine-checked linearizability proof.
3. **A15** — 41/41 → **45/45**; 355 → **420** vectors.
4. **A20** — 60 codec vectors → **91**.
5. **A21** — 39 store tests → **771** tests across 158 binaries.
6. **A22** — 232 FUSE tests → **814 passed / 2 ignored**.
7. **A23** — 31 Go + 13 GC → **125 top-level Go tests, 7 skipped under
   `-race`**.
8. **A24** — "latest 37/37" → latest **45/45** at the R13 tree; the release
   tree's 37/37 is stated separately.
9. **A31** — "parallel reconstruction is not implemented" → bounded parallel
   restore (R2) and pipelined restore (R10) named; materialize remains
   writeback-bound.
10. **A32** — "no headline performance claim" → "no performance-superiority
    claim".

### Other documents (material, same audit)

11. [`CONFORMANCE.md`](CONFORMANCE.md) said the Python reader executed every
    mode 0–18 vector and reported 229/0/109. The current receipt is
    **229 pass / 0 fail / 191 skip**; the Python reader has no mode-18/19/20
    or transport path. The document now states the per-mode skips and corrects
    the old claim.
12. [`WRITE-YOUR-OWN-DECODER.md`](WRITE-YOUR-OWN-DECODER.md) said modes 17–127
    were reserved and stopped at mode 16. Modes 17–20 are allocated; mode 21 is
    the reserved reject. The table, mode list, limits, and the Python coverage
    sentence are corrected, and the extended check order points to
    [`RFC.md`](RFC.md).
13. `spec/README.md` did not exist; the normative index is now in
    [`RFC.md`](RFC.md) and the spec directory has a navigation README.

---

## 4. What remains unverified

- **Live external state.** PyPI, crates.io, and GHCR were not re-queried in
  R13. `release-artifacts-20261109T124325Z` records what was observed at the
  tag; the audit can only confirm that the receipt says so.
- **Historical test counts.** A20–A23 now use the 45/45 receipt log at the R13
  tree. The exact per-binary counts are in that log, not in the receipt
  summary; the log is committed beside the receipt.
- **No physical media or fleet evidence exists anywhere in this repository.**
  Any claim that reads like a deployment result is a non-claim in the source
  documents; the audit did not find a headline that asserts one.
- **The 1.2.0 release note is not retroactively corrected.** It is a tagged
  historical document; its 355-entry claim is true at the tag and marked
  historical here. Current-state documents carry the 420-entry manifest.

If a row in §1 or §2 disagrees with its receipt, the receipt wins. Open an
issue rather than editing the receipt.
