# R17/R18 — fenced writer protocol for the S3 and GCS writer leases

Status: **implemented and tested** on `staging` (Rust only; Go has no S3 or GCS
backend). R17 covered S3; R18 maps the same protocol onto GCS generation
preconditions and makes the invariant tests shared. This document is the
evidence, the exact atomicity statement, and the list of what is still missing.
It is the companion to the `Subphase R17` and `Subphase R18` sections of
`docs/STATUS.md`.

Label: one host, one OOM-protected Docker court, one MinIO process on loopback
with a tmpfs data directory. Real conditional writes against a real endpoint.
Not a cloud, multi-node, fleet, media, or power-loss claim — and **not a
multi-writer protocol**: writers remain exclusive by design.

---

## 1. The defect as reported, and as found in the source

The reviewer's report was accurate. In `src/s3.rs` before this subphase:

1. `release_lock()` performed `GET LOCK`, checked that the body named this
   owner, then issued `self.request("DELETE", "LOCK", ...)`. The `DELETE` is
   unconditional. If another writer reclaimed the expired lease between the
   read and the delete, the departing writer deleted its successor's lock.
   Time-of-check to time-of-use.
2. There was no lease renewal and no publication fencing. `acquire_lock`
   recorded an owner and an `expires_ns`; nothing consulted the lease at
   publication time. A writer whose lease had expired could keep publishing.
3. A conditional head compare-and-swap alone was **not** ownership proof: it
   proves only that the *head* did not move. A writer can still win that race
   while another writer has already taken the lease.

R18 found the same three defects in `src/gcs.rs`, in the generation-precondition
shape: `release_lock()` read `LOCK`, checked the owner line, then deleted it
unconditionally; `acquire_lock` wrote `owner` + `expires_ns` and nothing read the
lease at publication time; and `verify_write_lease` was not implemented, so the
trait default `Ok(())` let a stale writer publish and
`fence_head_publication` fell back to the same no-op. That gap is closed by
section 2.6 and the shared courts in section 4.5.

## 2. The protocol

### 2.1 The `LOCK` object

`weftfs-s3-lock v2` is a small text record, one field per line:

```
weftfs-s3-lock v2
owner=462-1-1793984165314626737
fence=2
sequence=7
lease_deadline_ns=1793984170122999455
renewed_ns=1793984168314626737
pending_generation=3
pending_commit=90dc1bf3e66b8b5a893f307648f2c0c235f2992816ad13df82a82cd040a34f4b
```

* `owner` — `pid-counter-now_ns`.
* `fence` — the **monotonic fencing token**. The first lease takes `1`; every
  takeover (of an expired lease or of a released tombstone) takes
  `previous.fence + 1`, checked for overflow. A legacy `weftfs-s3-lock v1`
  record (owner + `expires_ns`) is read as fence `0`, so the first takeover of
  a v1 lock takes `1` and every later one is monotonic.
* `sequence` — conditional writes this handle has made to `LOCK`.
* `lease_deadline_ns` — absolute deadline. Never extended by a publication that
  is outside the renewal window.
* `pending_generation` / `pending_commit` — the head this lease holder is
  publishing, written by the fenced CAS that immediately precedes the head
  write (`-` / `0` when nothing is in flight).

Release writes `weftfs-s3-lock-released v1`:

```
weftfs-s3-lock-released v1
owner=462-1-1793984165314626737
fence=2
sequence=8
released_ns=1793984169...
released_from=<sha256 hex of the exact lock bytes this release replaced>
```

`released_from` makes the tombstone content-addressed to the record it
replaced, which is what makes the release auditable after the fact.

### 2.2 Acquire, takeover, renewal, deadline

* **Create-once** (`If-None-Match: *`): fence 1.
* **Takeover** reads the record and CASes it with `If-Match: <etag>` to fence
  `previous + 1`. It refuses `Busy` while `now <= deadline_ns` and while the
  record is not a released tombstone. A lost race is retried from a fresh read
  (four bounded attempts), never blind.
* **Renewal** happens inside the same conditional write that proves ownership,
  and fires when at most one publication budget remains (`deadline - now <=
  lease/2`). The new deadline is `now + lease`. A writer therefore renews at
  half-life, once per publication, and the interval is bounded by the lease.
* **Deadline.** Before the conditional write, a writer refuses to publish when
  `now >= deadline_ns`, with no network round trip and no successor involved:

  ```
  S3 writer lease deadline passed 407 ms ago (owner 463-1-… fence 2);
  publication refused before the head was touched; the LOCK still names this
  writer, but its own lease expired
  ```

### 2.3 The fence, and where it sits in the publication

`Store::publish_commit` now calls the lease proof twice:

1. `verify_write_lease()` before the commit record is written.
2. `fence_head_publication(generation, commit)` immediately before the
   superblock slot write.

Both are **one conditional write on `LOCK`** whose `If-Match` is the ETag of
the bytes *this handle last published*. The gate therefore succeeds only while
the object still carries this writer's fencing token: a successor's takeover
rewrote the object, which changes its ETag, so the stale writer receives `412`
and the store returns `Busy`:

```
S3 writer lease was taken over: the conditional fence CAS for owner
456-1-… fence 2 was refused, so the publication was abandoned without writing
the head (the LOCK now names owner 458-1-… fence 3)
```

The second call also writes `pending_generation` / `pending_commit`: the
fencing token is carried by the same atomic operation that names the head about
to be published.

### 2.4 Release

`release_lock` never sends an unconditional `DELETE`. It reads `LOCK`, requires
that the record still names this owner **and** this fencing token, and then
writes the tombstone with `If-Match: <observed etag>`. Three outcomes:

| situation | result |
|---|---|
| record names this writer | tombstone published (fence preserved) |
| record names another writer | refused, nothing written |
| CAS rejected (replaced in between) | refused, successor's bytes untouched |

A tombstone CAS is used in preference to a conditional `DELETE` because
deleting the object would reset the fencing token on the next create-once,
reopening the possibility of token reuse across a failed release. The
writability probe already proves that `If-Match` is honoured (matching accepted,
stale refused), which is exactly the primitive the tombstone needs; an endpoint
that cannot express it is refused at writable open, as before.

### 2.5 POSIX backends

A local or object store has no lease, but it has the same fencing question. Its
"lease" is the `flock` on `LOCK`; before every publication the store proves that
the `LOCK` *name* still resolves to the exact `(dev, ino)` this handle locked.
Replacing or removing the file hands the namespace to someone else, and the old
handle then fails `Busy` before the commit record or the head is written.

### 2.6 The same protocol on GCS (R18)

GCS has no ETags: its conditional write is a **generation precondition**. The
records are the same
(`weftfs-gcs-lock v2` / `weftfs-gcs-lock-released v1`, identical fields to S3's;
encode/parse/describe live in `src/writer_fence.rs`), and only the primitive and
the token source change:

| concern | S3 | GCS |
|---|---|---|
| create once | `If-None-Match: *` | `ifGenerationMatch=0` |
| observed token | ETag string | object generation (`u64`) |
| token source | response `ETag` header | upload reply object resource; if absent, a read-after-write that must return exactly the published bytes |
| takeover | `If-Match` on the observed ETag | `ifGenerationMatch` on the observed generation |
| fence | conditional rewrite of `LOCK` | conditional upload of `LOCK` |
| renewal | same conditional write | same conditional upload |
| release | tombstone with `If-Match` | tombstone with `ifGenerationMatch` |
| writable-open proof | `If-Match` accepted, stale refused | matching generation accepted, stale refused |

Everything that is *not* the primitive is identical: the fence precedes the
head CAS, it carries `pending_generation` / `pending_commit`, it renews the
deadline when one publication budget remains, a writer refuses once its own
deadline passed, and release is a conditional tombstone that never deletes
`LOCK`. A stale writer's conditional upload names an old generation, so a
successor's reclaim makes it fail `412` → `Busy` before the head is touched.

Because the generation must name *this* writer's bytes, an endpoint that
returns neither the object resource nor the same bytes on re-read is refused
`UnsupportedVersion`: a guessed token could let a successor's generation be
recorded as this writer's own.

## 3. Exact atomicity statement

S3 and GCS each give one conditional write per object and no multi-object
transaction. The protocol is built to be safe under that constraint, and the
boundary is stated plainly:

* The fence and the head write are **two** operations on **two** objects.
  They cannot be fused on a plain S3 endpoint.
* Safety does not rely on them being fused. It relies on the ordering rule:
  a publication is only *started* while `now + budget <= deadline`, where
  `budget = lease/2`; a takeover is only allowed at `now > deadline`. The
  publication window therefore closes before any successor may touch the lock,
  and the gate is the last write before the head CAS. A successor that takes
  over has, by construction, replaced the lock bytes, so the stale writer's
  gate fails.
* A `SIGSTOP`ped writer whose lease is reclaimed is fenced by both mechanisms
  at once: its own deadline has passed *and* its gate token is stale. The court
  records which sentence fired and which token the successor holds.

What is **not** covered: a request whose reply is lost in the network. S3 has
no idempotency token for `PUT`, so a head write can succeed while the writer
believes it failed. That is the standard at-least-once ambiguity of any
networked conditional write; it does not create divergence (the successor
observes a newer valid head and adopts it on reopen, and `verify --full`
checks it), but it means a "failed" publication is not provably absent. This is
recorded as a residual, not a pass.

## 4. Evidence

All runs are in the repository's Docker courts under unchanged hard limits
(memory 3g / swap 3g, pids 1024, cpus 4, bounded `/tmp`, `cap-drop=ALL`,
`no-new-privileges`).

### 4.1 `tests/s3_fencing.rs` — 8/8 in the MinIO court (15.19 s at the R18
tree; 15.23 s at R17, 3 s lease)

Receipt `bench/receipts/s3-court-20261106T174609Z.json` (6 backend + 8 fencing
tests, MinIO RELEASE.2025-04-22T22-12-26Z sha256 `53e2a2cb…bb817c`). The same
courts re-ran at the R18 tree (shared test bodies) as
`bench/receipts/s3-court-20261106T182043Z.json`: 6/6 and 8/8 in 15.19 s.

| test | property | observed |
|---|---|---|
| `expired_lease_blocks_publication_without_a_successor` | own deadline refuses; no successor | `Busy`; head, generation, content, commit key set unchanged; `LOCK` still fence 1 |
| `paused_writer_is_fenced_after_a_lease_reclaim` | real `SIGSTOP` child, reclaim, `SIGCONT` | paused fence 2, successor fence 3; `Busy`; paused writer's generation and head unchanged; published head is the successor's; `/fenced` absent; `verify --full` ok |
| `release_of_a_replaced_lock_fails_closed` | stale release vs a successor | successor's `LOCK` bytes **and** ETag identical before/after the stale `drop`; fence 2 intact; successor still publishes; its own release writes a tombstone |
| `two_writers_racing_on_release_leave_one_valid_lock` | 20 release/acquire races | every iteration: token advances by exactly one, object never deleted, final state a tombstone at the racer's token |
| `renewal_keeps_the_writer_valid` | two renewal windows | two publications succeed, fence constant, `deadline_ns` strictly increasing, sequence advancing |
| `fencing_token_is_monotonic_across_reclaims` | five leases | fences `1,2,3,4,5`; every publication names its own `pending_generation` |
| `observer_reads_the_published_lock_without_a_lease` | read-only observer | sees fence 1 and `pending_generation=1` with a 64-hex `pending_commit` |
| `child_fenced_writer` | the child half of the `SIGSTOP` test | no-op unless re-executed with `WEFT_FENCE_CHILD` |

Exact refusal from the paused-writer run:

```
S3 writer lease deadline passed 540 ms ago (owner 456-1-1793984161444951913 fence 2);
publication refused before the head was touched; the LOCK now names owner
458-1-1793984164957146535 fence 3
```

### 4.2 `examples/distributed_court.rs` scenario S10 — 26 checks

Receipt `bench/receipts/distributed-20261106T174634Z.json`: **10/10 scenarios,
255 checks, 0 failed, 126 processes**. S10 drives the same four cases through
separate child processes and records every token, head, and generation:

| case | evidence |
|---|---|
| F1 `SIGSTOP` / reclaim / `SIGCONT` | paused fence 2 → successor fence 3; `Busy`; paused generation/head unchanged (`2`, `1766fa06…`); successor generation `3`; `/alpha` visible, `/must-not-land` absent; `keep` byte-exact; verified commit equals the successor's head |
| F2 release racing a reclaim | holder fence 2, racer fence 3; `LOCK` live and naming the racer during the race; final tombstone at fence 3 |
| F3 bounded renewal | two publications, deadlines strictly increasing, fence constant at 2 |
| F4 expired lease, no successor | `Busy`; head and generation untouched; `LOCK` still fence 2 while held; the exiting writer's own release is a tombstone at fence 2 |

Recorded token map: `{paused: 2, successor: 3, release_holder: 2,
release_racer: 3, renew: 2, expired: 2}`; generation sequence `[2, 2, 2]`.

### 4.3 Independent implementation: Ceph RGW 19.2.0

`scripts/ceph_court.sh` now runs both suites against Ceph RGW (an independent
S3 implementation), not only `s3_backend`:

| suite | RGW |
|---|---|
| `tests/s3_backend.rs` | **6 passed, 0 failed** (41.96 s) |
| `tests/s3_fencing.rs` | **8 passed, 0 failed** (80.85 s) |

This is the first time the fence runs against an endpoint that (a) compares
`If-Match` literally against a dequoted ETag and (b) is roughly an order of
magnitude slower per round trip than MinIO. Both exposed real defects in the
harness, not in the protocol (section 6). At the R18 tree the same suites re-ran
against RGW as `bench/receipts/ceph-rgw-20261106T183427Z.json` (6/6 in
41.94 s, 8/8 in 80.87 s) with the shared invariant bodies.

### 4.4 `tests/gcs_fencing.rs` — 7/7 against `fake-gcs-server` v1.56.1

The pinned emulator is a single static binary (`fake-gcs-server_1.56.1_Linux_amd64.tar.gz`,
sha256 `0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`)
started on loopback inside the OOM-protected rust court with a memory backend.
Receipts: `bench/receipts/gcs-court-20261106T182003Z.json` (court, with the
emulator release, tarball sha256, container limits, commands, logs) and
`bench/receipts/gcs-20261106T182003Z.json` (end-to-end harness), plus
`.fencing.log` / `.tests.log`.

| suite | result |
|---|---|
| `tests/gcs_backend.rs` (in-process fake service) | **15/15**, 1.42 s |
| `tests/gcs_fencing.rs` (emulator, 3 s lease) | **7/7**, 14.60 s |
| end-to-end court (local vs GCS) | `content_root_match=true`, `generation_match=true`, `generation_sequence_match=true` |

The default GCS lease is 900 s. The crash-proxy court keeps its 2 s lease
(`WEFT_GCS_LOCK_LEASE_SECS`); the fencing suite uses 3 s
(`WEFT_GCS_FENCE_LEASE_SECS`) because it must be able to expire a lease.

GCS-specific observed detail: the end-to-end court ran 21 commits and the
backend diagnostics reported **`GCS fence CAS proved the lease 42 time(s), 0
with a renewal`** — exactly two conditional `LOCK` uploads per commit (the
proof before the commit record and the fenced proof naming the head).

### 4.5 One test body, three courts

`tests/support/fencing.rs` contains the invariants; the two test files import
it and supply only endpoint configuration and observer hooks. The S3 court
runs the file on MinIO **and** on Ceph RGW; the GCS court runs it on
`fake-gcs-server`.

| invariant | MinIO (s3) | Ceph RGW 19.2.0 (s3) | fake-gcs-server 1.56.1 (gcs) |
|---|---|---|---|
| expired lease, no successor | pass | pass | pass |
| paused writer reclaimed → `Busy`, head unchanged | pass | pass | pass |
| release of a replaced lock leaves successor bytes + token | pass | pass | pass |
| renewal: fence constant, deadline increases | pass | pass | pass |
| takeover tokens `1..5`, each naming its head | pass | pass | pass |
| `SIGSTOP` child (`child_fenced_writer`) | pass | pass | pass |
| observer reads the live record without a lease | pass | pass | pass |
| release/acquire race (S3-only) | pass (20/20) | pass (20/20) | not applicable |

The GCS observer test additionally asserts that the conditional token is a real
object generation (`generation_for_test(LOCK) > 0`), so the fence is proven to
condition on the provider's own token rather than a local guess.

### 4.6 Backend and suite counts (R18 tree)

| suite | result |
|---|---|
| `cargo test --all-targets` (rust court) | 95 targets, **445 passed, 0 failed, 0 ignored** |
| `cargo test --features fuse --all-targets -- --test-threads=1` (fuse court) | 95 targets, **487 passed, 0 failed, 2 ignored** (both pre-existing: same-inode direct-I/O EIO, and `CAP_SETUID` absent) |
| `tests/writer_fencing.rs` (local + object) | 4/4 |
| `tests/s3_backend.rs` (MinIO) | 6/6 |
| `tests/s3_fencing.rs` (MinIO) | 8/8, 15.19 s |
| `tests/s3_backend.rs` (Ceph RGW) | 6/6, 41.94 s |
| `tests/s3_fencing.rs` (Ceph RGW) | 8/8, 80.87 s |
| `tests/gcs_backend.rs` (in-process fake) | 15/15, 1.42 s |
| `tests/gcs_fencing.rs` (`fake-gcs-server`) | 7/7, 14.60 s |
| `distributed_semantics` + harness (R17) | 12/12 tests, 10/10 scenarios |
| `scripts/verify-all.sh` | **not rerun for R18**; last aggregate at R17 is 28/28 (`verify-20261106T174703Z.json` at `37a3a39`) |

The two ignored FUSE tests are the pre-existing ones recorded in
`docs/fuse-status.md`; nothing new is ignored or filtered.

One flake is disclosed: the first full Rust run after stage 1 failed
`distributed_semantics::single_writer_interlock_is_exclusive_and_reopens_cleanly`
at its `Store::open(...).unwrap()` after the first handle was dropped. It passed
standalone immediately, that binary passed 5/5 consecutive runs, and the full
suite rerun was 445/0. R18 touches no local-backend code.

## 5. What did not change

* The frozen formats: `superblock`, `commits/<gen>.commit`, `objects/`,
  `manifests/`, `indexes/`. The v1 conformance fixtures are untouched and
  `local_and_s3_artifacts_are_byte_identical` still passes, including the
  superblock and `FORMAT`.
* The commit protocol: blobs, commit record (create-once), superblock slot,
  directory sync.
* Single-writer semantics: a live lease is still refused `Busy` with the same
  fixed, non-disclosing sentence (`store is locked by another S3 writer (live
  LOCK object)`); the owner, token, and remaining lease go to diagnostics, not
  to the caller.
* The writable-open probe: `If-None-Match: *`, a matching `If-Match`, and a
  stale `If-Match` that must be refused, plus the measured quoted/dequoted
  entity-tag form. On GCS the same gate proves `ifGenerationMatch=0` creates
  once, a matching generation succeeds, and a stale generation is refused.
* The GCS logical key layout (`objects/`, `manifests/`, `commits/`,
  `indexes/`, `refs/`, the two-slot superblock) and the commit protocol are
  unchanged. Only the control object `LOCK` changed shape, from
  `weftfs-gcs-lock v1` (`owner` + `expires_ns`) to the fenced v2 record. It is
  not content-addressed and is not part of any commit.

## 6. Costs

* One extra `PUT LOCK` per commit, and a second one on the head path (the
  store calls the proof before the commit record and again before the head).
  Against MinIO on loopback this is not measurable in the S9/S10 receipts; no
  performance claim is made either way.
* `LOCK` is now a small evolving record rather than a write-once object. It is
  a control object: it is not content-addressed, is not part of the commit, and
  no byte-identity court compares it.
* A takeover waits for `deadline_ns`, i.e. up to one full lease. With the
  default 900 s lease that is a 15-minute recovery window after a hard crash;
  the courts lower it (`WEFT_S3_LOCK_LEASE_SECS`, and `WEFT_S3_FENCE_LEASE_SECS`
  for the fencing suite, which needs a lease it can expire: 3 s).
* **A writer that stays idle past its deadline must reopen.** The deadline
  check fires before any read of the lock, so a long-idle handle gets `Busy`
  rather than silently re-acquiring its own expired lease. That is deliberate —
  the reviewer's requirement is that a writer stops once its deadline has
  passed — but it is a behaviour change for a client that keeps a store open
  and writes rarely. The general S3 suites therefore run with a 60 s lease; the
  first aggregate run after this subphase used 2 s and `store_round_trip_over_s3`
  failed on RGW with

  ```
  S3 writer lease deadline passed 216 ms ago (owner 22-20-… fence 1);
  publication refused before the head was touched
  ```

  That receipt (`verify-20261106T171925Z.json`) is retained as the evidence
  for the behaviour; the lease split above is the fix.
* GCS: the fence adds two small conditional uploads per commit (proof before
  the commit record, fenced proof naming the head). A single-run comparison of
  the same 21-commit court workload against the pre-fence receipt
  (`gcs-20261106T150444Z.json`) gave 14.64 → 15.96 ms create, 11.52 → 12.62 ms
  range write, 4.49 → 4.89 ms read, 9.86 → 9.22 ms reopen, 27.84 → 27.56 ms
  verify. These are two runs, not interleaved repetitions, so this is an
  observation and **not** a performance claim.
* GCS: `upload_tracked` reads the generation from the upload reply. When an
  endpoint omits the object resource it costs one extra read-after-write per
  `LOCK` upload to confirm the bytes (and therefore the generation) are this
  writer's. `fake-gcs-server` returns the resource, so the court does not take
  that path in the court (fake-gcs-server returns the resource). The fallback is
  exercised end-to-end by `tests/gcs_backend.rs`'s FakeGcs
  `omit_upload_generation` mode: a full store init/create/range-write/verify and
  a read-only reopen take the verified-read path for every lease write, and the
  published record still carries one live fence token.

## 6b. Defects the fence found in its own harness (Ceph RGW, GCS)

1. **A 1 s lease is not enough to publish on RGW.** `crash_proxy_over_s3_yields_old_or_new`
   gave its child a 1 s lease so the harness could sleep it out; on RGW the
   child's own deadline expired *mid-publication* (141 ms and 129 ms over) and
   the reopen saw the old generation. The child now gets a realistic lease and
   the reset is the fenced tombstone; the expiry path is covered by the fencing
   suite. The failing aggregate receipt `verify-20261106T170554Z.json` is kept.
2. **`release_stale_lease` did not negotiate the `If-Match` form.** A fresh
   handle defaults to the quoted form; RGW compares literally, so every court
   reset was a 412 (`the LOCK was replaced while the court reset it`). The
   helper now runs the same conditional-write probe a writable open runs.
3. **The release/acquire race was not racing.** The test released before it
   began acquiring, so every iteration won on the first attempt. It now retries
   until it has observed the live lock (at least one `Busy`), and only then
   releases; every iteration records `acquired after 2 attempt(s)`.
4. **The GCS court's environment allowlist was incomplete (R18).**
   `scripts/docker-court.sh` forwards an explicit list of `WEFT_*` names; the
   new `WEFT_GCS_FENCING_LOG` was not on it, so the first court run failed with
   `WEFT_GCS_FENCING_LOG is required`. The allowlist now carries
   `WEFT_GCS_FENCING_LOG` and `WEFT_GCS_FENCE_LEASE_SECS`; the failed court
   receipt `gcs-court-20261106T180729Z.json` is retained.

## 7. What a real multi-writer protocol would still require

This subphase makes **one writer's** lease safe: it cannot release or publish
outside it. It is explicitly **not** a multi-writer protocol, and none of the
following exists:

1. **Concurrent writers.** `LOCK` still admits exactly one writer. Handling N
   writers needs per-writer sessions plus a merge or serialisation point for
   the head (a consensus log, a queue, or an external CAS service).
2. **A single atomic fence+head operation.** On plain S3 that needs the head to
   live in the same object as the fence, or a provider with multi-object
   transactions. WeftFS ships the two-write ordering argument instead, and says
   so.
3. **Recovery of a partially published writer without a reopen.** A successor
   detects and adopts a newer valid head only when it opens or retries; there is
   no in-flight handover protocol.
4. **Clock safety.** The deadline argument assumes the writers' clocks agree
   within the lease budget. A writer with a badly skewed clock is not defended
   against; a monotonic clock source and a bounded skew assumption would be
   needed.
5. **Cross-object atomicity.** The fence and the head write are two
   conditional writes on two objects on both S3 and GCS. Keeping the
   two-write ordering argument is deliberate; a provider with multi-object
   transactions is not used.
6. **The GCS backend is now fenced (R18), but against an emulator.**
   `fake-gcs-server` proves the generation-precondition protocol, not the
   behaviour of a real Google Cloud Storage deployment: no multi-region
   consistency, TLS, credential rotation, service-side throttling, or
   partition was tested. The release is a single-node memory-backed process on
   loopback.
7. **Go.** There is no Go parity: WeftFS-Go has no S3 and no GCS backend, so
   there is no cross-language fence and no Go GCS court.
8. **Physical media and power loss.** Everything here is a process-fault court
   on a tmpfs-backed endpoint or an emulator's memory backend.

## 8. Reproduction

```bash
# S3 unit + fencing court (real MinIO in the rust court)
bash scripts/s3_court.sh

# GCS unit + fencing + end-to-end court (pinned fake-gcs-server)
bash bench/scripts/gcs_court.sh

# Multi-process court including scenario S10
bash scripts/s3_court.sh --distributed

# Independent S3 implementation (Ceph RGW) including the shared fencing suite
bash scripts/ceph_court.sh

# Full suites
bash scripts/docker-court.sh rust "cargo test --all-targets"
bash scripts/docker-court.sh fuse \
  "cargo test --features fuse --all-targets -- --test-threads=1"
```
