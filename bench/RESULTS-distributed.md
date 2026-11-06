# Distributed correctness court (R13)

**Receipt:** `bench/receipts/distributed-20261106T134954Z.json`
**Raw logs:** `bench/receipts/distributed-20261106T134954Z.log` and
`bench/receipts/distributed-20261106T134954Z.s{1..11}.log`
**Failing receipt retained:** `bench/receipts/distributed-20261106T133534Z.json`
(206/229 checks; it found the two defects in §5)
**Court:** `weft/bench-court:1`
`sha256:4041d66ed25626e875de58f79e6527997c85ca81541566cbabe09451b40f2041`,
kernel `7.3.1-1-cachyos`, memory `3g` (swap `3g`), cpus `4`, pids `1024`,
nproc `2048`, `no-new-privileges`, `cap-drop=ALL`.
**Server:** MinIO `RELEASE.2025-04-22T22-12-26Z`, binary sha256
`53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`,
started by the harness on loopback inside the bench container, data directory
on the court's `/tmp` tmpfs.
**Revision:** WeftFS `1e857e249ef9b685ebddc2bb9b08855d55be4d0c` (clean tree).
**Reproduce:** `bash scripts/s3_court.sh --distributed` (host; the script
verifies the MinIO binary, then runs everything inside the bench court).

## 1. What this court is for

The S3 backend, the blob/commit plane split, conditional head publication with a
probed stale-CAS refusal, GC, capabilities, and snapshots were already covered
by single-process and two-process courts. The reviewer required one court that
demonstrates the **distributed contract under failure** before any performance
claim. This is that court. It drives *separate OS processes* against one real
MinIO bucket (and a local split control) and records, for every scenario, the
exact command lines, process PIDs and exit codes (including signals), observed
generation sequence, verification result, and raw log path.

`tests/distributed_semantics.rs` is the always-on half: 12 tests pin the same
semantics with deterministic child processes (crash matrix, stale reader,
concurrent readers, GC race, content-hash denial) so the contract does not
depend only on a shell harness.

## 2. Result

| scenario | what it establishes | checks |
|---|---|---|
| S1 | single-writer interlock, 20 alternating pairs | 64 / 64 |
| S2 | atomic publication at every exposed fault point (single S3) | 34 / 34 |
| S3 | idempotent retry after a crash | 21 / 21 |
| S4 | 8 concurrent readers during 10 commits | 19 / 19 |
| S5 | stale reader waits for reopen | 4 / 4 |
| S6 | unreachable/stopped store fails closed | 9 / 9 |
| S7 | `gc --apply` racing a snapshot reader | 8 / 8 |
| S8 | a known content hash is not authority | 10 / 10 |
| S9 | S2/S4/S5 on `--blob-backend s3 --commit-backend local` | 60 / 60 |
| **total** | | **229 / 229** |

The harness recorded **120 processes** (argv, PID, exit/signal, stdout/stderr
tail), **42 isolated buckets**, and took **5,968 ms** (harness wall time; the
release build and the 12-test court run separately). Harness peak RSS was
**7,460 KiB**. The server was started **2** times and stopped **1** time.

All 9/9 scenarios passed and 229 checks passed with none failed. This is not a
cloud, multi-node, fleet, device-cache, media, power-loss, or production claim.
Writers are exclusive by design, and the court is Rust-only.

All completed runs verified with `verify --full` (`ok: true`, no problems)
except the documented unpublished-tail case in S2/S9a, which is reported as a
problem by design and is not a visible state.

## 3. Scenario evidence

### S1 — single-writer interlock

20 iterations; the process that opens first alternates (`first-a`/`first-b`).
Each iteration spawns the first writer, waits for its `READY` record (generation
and head), spawns the second writer, records its refusal, releases the first,
and spawns a third writer to prove the store reopens.

* 20/20 second opens were refused with class `Busy` and a nonzero exit; the
  message is the same non-disclosing sentence every time
  (`store is locked by another S3 writer (live LOCK object)`).
* 20/20 third opens succeeded at the same generation as the first.
* Final `verify --full` is clean and `/f` still returns `interlock-bytes`.

### S2 — atomic publication under a process fault

For each of `after_objects`, `after_commit_file`, and `after_superblock_write`,
five independent runs on a fresh bucket. The writer publishes two file writes in
one group commit and is killed with `SIGABRT` (`WEFT_CRASH_PHASE`); the parent
reopens read-only and reads **both** files, so a partially applied group would
be visible.

* **15/15** runs produced a whole state: **10 old, 5 new, 0 mixed/partial**.
* The five `after_superblock_write` runs observe generation `G+1`; the ten
  pre-publication runs observe the old generation `G` and the old bytes.
* The five `after_commit_file` runs are classified by `verify` as exactly one
  problem: `commit file for generation G+1 exists above the highest published
  superblock generation G`. The tail is *reported*, never hidden; the head and
  every reachable byte are the old state.

### S3 — idempotent retry

A local reference store produces the uninterrupted head
`32a0ba3d459c590723930e0598fa8a1066c60c42a0de18ecd089b378511d9431` at
generation 2 with the same seeds, bytes, and fixed clock.

| crash point | retry result | head equal to uninterrupted? |
|---|---|---|
| `after_objects` | generation 2, same object ids, same bytes | **yes** (byte-identical commit) |
| `after_commit_file` | generation 3; lineage `[3, 1]`; one entry per path; same object ids, same bytes, no second visible state | no (see §6) |
| `after_superblock_write` | already the uninterrupted head; the retry is refused `Exists` and writes nothing | **yes** |

The abandoned commit file's generation is never reused, so a pre-head crash
costs one generation number. That is the documented cost of never publishing a
commit the writer did not CAS-confirm. For `after_commit_file` the *visible
state* is identical (same paths, same object identities, same bytes, same
verification) even though the commit hash differs by generation.

### S4 — concurrent readers during commits

Eight reader processes open a read-only store at the pinned generation 3,
perform four verified reads each, and then wait. The writer publishes ten new
versions of the same object (`/hot`, generations 4–13) while all eight are
open; each reader then performs four more verified reads through the same
handle. **64/64 reads** matched the construction oracle; every reader reported
`generation_before = 3`, `stale_reads = 4`, and no mismatch. Final
`verify --full` is clean.

### S5 — stale reader

One reader opens at generation 2, reads the old payload four times, and waits.
The writer publishes a new version (generation 3). After the gate the reader
performs four more reads **through the same handle** and still sees the old
bytes; only after it drops and reopens does it read the new bytes (four
verified reads at generation 3). The server-side state is published and
verifies before the reader's reopen.

### S6 — unreachable store

* **No listener.** A reader is pointed at `http://127.0.0.1:<closed-port>`.
  The open fails with class `Io` (previously `Corrupt`; see §5), emits no
  bytes, and exits nonzero.
* **Stopped server, mid-run.** A reader opens the live store, performs its
  pre-stop reads, and waits. The harness kills the MinIO process (observed
  stopped, port refusing), then releases the reader. Its read fails with class
  `Io`, emits **0** bytes, and the status records no partial data. The harness
  restarts MinIO on the same data directory; a verify process reports
  `ok: true` at the same generation, and a fresh reader returns the same
  pinned bytes. Recorded lifecycle: **2 starts, 1 stop**.

### S7 — GC race

The store holds a live file `keep`, an input `input`, a provenance receipt for
`keep`, a snapshot `pinned`, a forced checkpoint, and a snapshot-only object
`gone` that was unlinked after the snapshot. 24 unique garbage files are
created and unlinked. A reader pins snapshot `pinned` and reads `gone` six
times while `gc --apply` runs in a separate process.

* GC reclaimed **48** blobs / **16,435 bytes** (the garbage manifests and
  chunks) and reported no problems.
* The reader saw the exact pinned bytes through the sweep.
* After GC: the snapshot-only object `gone` still reads exactly, `keep` still
  reads exactly, the snapshot state still names `gone`, the checkpoint state is
  readable, and the provenance receipt is present and declares `keep`'s content
  root. `verify --full` is clean.

### S8 — authorization by content hash

A world-readable `/secret` outside the granted Context Root is selected by a
signed Ed25519 capability envelope that grants only the root containing
`/visible`. The attacker knows the secret's `ContentId`
(`924c21ba409773742ffab3c2e773215189d443fb54089113b6f4da66f103f410`) and its
`ObjectId`.

* `exact_id_object(secret ContentId)` → `NotFound: no such object`.
* An absent ContentId → the **identical** class and text, so the answer is not
  an existence oracle.
* The granted object **is** addressable by its ContentId.
* An authorized token minted for `/visible` cannot read the secret's content
  root: `Unauthorized: authorization token does not cover this content root`.
* The shipped CLI `cat --envelope` reads `/visible` (exit 0) and denies
  `/secret` with `NotFound: no such object` (exit 1, empty stdout).
* Neither denial text mentions the secret's name.

### S9 — the split layout under the same faults

`--blob-backend s3 --commit-backend local` repeats S2, S4, and S5.

* S9a: 15/15 crash runs whole (10 old, 5 new, 0 partial; 5 unpublished tails
  reported).
* S9b: 8 readers × 10 commits, 64/64 reads exact at the pinned generation.
* S9c: 4 stale reads, then 4 exact reads after reopen at the new generation.
* 60/60 checks, all three sub-court logs recorded
  (`...s9.log`, `...s10.log`, `...s11.log`).

## 4. Verification

* `cargo test --release --test distributed_semantics -- --test-threads=1`:
  **12 passed / 0 failed** (`...tests.log` / `...tests.txt` in the receipt).
* `verify --full` on the relevant store in S1, S3, S4, S5, S6, S7, S8, S9.
* Rust all-targets after the change: **392 passed / 0 failed**.
* FUSE all-targets: **434 passed / 0 failed / 2 ignored** (the two pre-existing
  ignores).
* S3 unit court (`scripts/s3_court.sh`): **6/6**.

## 5. Defects found and fixed

1. **An unreachable store was reported as `Corrupt` at open.** Reading `FORMAT`
   mapped every error to `Corrupt`, so a connection refusal looked like a
   damaged store. `Store::open_impl` now preserves `Class::Io` (same context
   text); every other failure is still `Corrupt`.
2. **A transport failure during a read was reported as `Corrupt`.** With MinIO
   stopped mid-run, a manifest read returned
   `Corrupt: missing blob ...: Io: ... Connection refused`. The recovery blob,
   superblock, and commit wrappers and the three chunk-read wrappers now
   preserve `Class::Io`; a genuinely missing or hash-mismatched record is still
   `Corrupt`. This is what makes S6's "fails closed with a transport error"
   literally true.

Both are visible in the retained failing receipt
`distributed-20261106T133534Z.json` (`S6` checks).

The harness itself also mis-reported child error classes because it wrapped a
typed error into `"open <label>: ..."` and split on the first colon; children
now report `Error::class()`/`message()` directly and a refused open exits
nonzero. That was a court bug, not a product bug.

## 6. Honest limits

* **Not multi-node and not a cloud.** One MinIO process on loopback, one host,
  one Docker court. No AWS/GCS/3FS, no fleet.
* **No TCP-level partition.** The only network faults are (a) an endpoint with
  no listener and (b) stopping/restarting the server process. There is no
  packet filter, delay, duplication, or split-brain scenario.
* **No physical media or power loss.** MinIO's data directory is a tmpfs and a
  server stop is a process-kill. Durability here means "the server
  acknowledged the write"; there is no fsync on S3.
* **One writer at a time by design.** The interlock is the contract; there is
  no multi-writer protocol to test.
* **`after_commit_file` retry head is a new generation.** The visible state is
  identical and there is no second visible state, but the commit hash differs
  (generation 3 vs the reference 2). Making it byte-identical would require
  reusing an unconfirmed generation, which the protocol deliberately refuses.
* **A crashed S3 writer's lease is cleared explicitly by the court** before the
  retry, so the run does not wait 30 s. Production waits out the lease; the
  lease is never disabled. The receipt records this in `S2.details`.
* **The reader's stdout is captured as text.** The court's payloads are ASCII,
  so the construction-oracle comparison is exact; a binary payload would need
  a file-based capture.
* **Rust only.** No Go parity for this court.
* The split layout's commit plane is local; S9 does not exercise a *remote*
  commit plane under these faults (that is the S3/S3 case in S2/S4/S5).

## 7. Artifact hashes

The receipt (recomputed on the host after the run):

```
dc84af7c29f679976b72d4308163e3fd49515d19d210c237d8be8077d7677d47  bench/receipts/distributed-20261106T134954Z.json
```

Scenario logs (recomputed on the host):

```
e79120849a36418be59a41ba25ac8f66bae7439851c85ff39d72e233ba8aec53  s1
2e57541b2e0499ba49c5c7390c3e3f623353299764b56bc8362f29b9d8ad593c  s2
5a719a8d9f01bd85539570fc28b983338d0d925aabfca3a548351d44699e62f2  s3
3bbbf3aa6f6f4b469056e2bf01c96bc51bdaa83664b466e47845a0564e639d05  s4
16063fc7083311498990e99390650013a37c8ef8a59847ec85918cc1201f5192  s5
685e0e8a0fdb70c3f35dbed064c7002cb5b0e058cca354b21f9fad37bddcc514  s6
dd98366a055c5b5c81be65f7f5cde09ba27b9e2777af41167602ca9f165d1eab  s7
c194a1194cd33b783c80c6c1d80505bc7b851a4c762c4bc4fa11828bb14c89ee  s8
69ff662a0f0bc40d6c3e3192aa8d763adf168468765daf3d5404b71def661fab  s9
306602c4fcd895ca250b21325b115999055da784e640b020271b4e98b9e762ae  s10
2a09b4bcd7469dfea1e5903395861cb24cbc71cca9b5f637294a40d9958533c2  s11
```

The court log's own hash is appended by the script after the log is written, so
it is recorded in the log rather than recomputed here. The retained failing
receipt is `distributed-20261106T133534Z.json` (206 passed / 23 failed).
