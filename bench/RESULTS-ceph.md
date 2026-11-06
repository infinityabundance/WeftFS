# Ceph RGW court — an independent "S3-compatible" implementation (R16)

Date: 2026-11-06. Subphase R16. Revision: WeftFS `staging` at the commit that
carries this file; the receipts record the exact tree through
`WEFT_REPO_WEFTFS` in every court container.

The Q4 court tests the S3 backend against MinIO, and the GCS court tests the GCS
backend against `fake-gcs-server`. Each is a *single* implementation of its
protocol, so neither can show whether "S3-compatible" means the wire behaviour
WeftFS depends on. This court runs the same tests and the same behaviour matrix
against **Ceph's RADOS Gateway** (RGW), which shares no code with MinIO.

Everything below was measured in the OOM-protected Docker courts on one host.
No run was performed on the host.

## 1. Did RGW run?

Yes. `quay.io/ceph/demo` — `ceph version 19.2.0
(16063ff2022298c9300e49a547a16ffda59baf13) squid (stable)` — mon + mgr + one
bluestore OSD + rgw (beast) in one container, pinned by digest
`sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723`
(image id `sha256:637a5761b2549743c465dae594a6a48f86d96e969cb3a3d5193894c1dcb742d5`).

Court receipt: `bench/receipts/ceph-rgw-20261106T160906Z.json`
(full `ceph -s` beside it in `bench/receipts/ceph-rgw-20261106T160906Z.cluster.txt`).

Observed cluster at the end of the run: `mon` 1 daemon (quorum), `mgr` active,
`osd` 1 up/1 in, `rgw` 1 daemon active. Health is `HEALTH_WARN` only because the
court uses one OSD with `osd_pool_default_size 2` (`Degraded data redundancy`,
`OSD count 1 < osd_pool_default_size 2`). That is a deliberate single-node
fixture, not a storage failure.

Container, with the same hard limits as every other court:

| limit | value |
|---|---|
| memory / swap | 3g / 3g |
| pids | 1024 (observed 316 of 1024, 319 threads, 0 OOM kills) |
| cpus | 4 |
| nproc (RLIMIT) | 2048 |
| tmpfs | `/var/lib/ceph` 2g, `/etc/ceph` 64m, `/tmp` 512m, `/var/log` 256m |
| capabilities | `--cap-drop=ALL` plus CHOWN, DAC_OVERRIDE, FOWNER, SETGID, SETUID |
| no-new-privileges | yes |

The capability set is the one difference from the MinIO sidecar and it is
required, not convenient: Ceph runs its daemons as uid/gid 167 and writes
keyrings owned by that user, which fails closed under pure `cap-drop=ALL`
(`mkdir: cannot create directory '/var/lib/ceph': Permission denied`).
The *test* container keeps `cap-drop=ALL` unchanged.

Peak container memory was **1,120,411,648 B (1.04 GiB)** of the 3 GiB bound.

## 2. Two RGW-specific facts this court had to discover

Both are recorded in the court receipt because they change how the endpoint is
configured, not how WeftFS behaves.

### 2.1 The 1024-pid bound kills `mon` and `rgw`

The first attempt used default RGW settings. RGW (squid) creates a 512-thread
`io_context` pool, and `radosgw-admin` creates its own 512-thread pool. Together
with mon, mgr and the OSD they exceed the court's 1024-pid limit; the kernel then
refuses thread creation and **`ceph-mon` and `radosgw` both abort silently** —
the container log simply stops, and `radosgw-admin user create` hangs against a
dead monitor. Evidence from the cgroup at that moment: `pids.current 719`,
`pids.max 1024`, `pids.events max 2`, and `radosgw-admin` alone had 570 threads
(`ps -eL`). No OOM kill occurred (`memory.events oom_kill 0`).

The court therefore sets `CEPH_ARGS="--rgw-thread-pool-size 32"`. The whole
container then uses 319 threads. The pids limit was **not** raised.

### 2.2 `rgw dns name` turns a path-style request into a virtual-hosted one

The demo config writes `rgw dns name = <container hostname>`. When the client's
`Host` header equals that name, RGW parses the request as *virtual-hosted* style
and takes the whole hostname as the bucket name. Addressing the container by its
Docker name failed with:

```
S3 CreateBucket weft-ceph-court returned HTTP 404:
<Error><Code>NoSuchBucket</Code><Message></Message>
<BucketName>weft-ceph-rgw-test</BucketName>...
```

The court therefore addresses RGW by an IP literal on a dedicated bridge
(`http://10.213.77.10:8080`), which is path-style and works. This is a
configuration fact about RGW's default, not a client defect: our backend only
offers path-style addressing (`path_style: true`), which is what a
`rgw dns name`-free deployment or an IP endpoint gives you.

## 3. Compatibility matrix

Harness: `examples/s3_compat_matrix.rs`. Same harness, same checks, same client
for both servers. Receipts:

* MinIO: `bench/receipts/s3-compat-minio-20261106T160906Z.json`
* Ceph RGW: `bench/receipts/s3-compat-ceph-20261106T160906Z.json`
* side-by-side: `bench/receipts/s3-compat-matrix-20261106T160906Z.json`

| behaviour (harness check name) | MinIO `RELEASE.2025-04-22T22-12-26Z` | Ceph RGW 19.2.0 | observation |
|---|---|---|---|
| `if-none-match-create-once` | supported | supported | create `200`, repeat `412 PreconditionFailed`, first bytes intact |
| `if-match-conditional-overwrite` | supported | supported, accepted only after de-quoting | RGW rejects the quoted ETag it just returned |
| `range-open-ended` | supported | supported | `206`, `Content-Range: bytes 4-15/16`, exact tail |
| `range-out-of-range` | supported | supported | `416 InvalidRange` for `bytes=999-` and for `bytes=16-` on a 16-byte object |
| `head-missing-key` | supported | supported | `404` |
| `delete-idempotent` | supported | supported | `204` then `204` |
| `list-pagination` | supported | supported | `max-keys=2`, 3 pages, 5 unique keys, 0 duplicates, `NextContinuationToken` honoured |
| `stale-cas-probe` | supported | supported | winner `200`, stale `412`, winner bytes kept |

Both implementations are **8/8** for what the store requires. The single
behavioural deviation is the `If-Match` quoting rule.

### 3.1 The `If-Match` deviation, exactly

One run, one object, the ETag RGW itself returned:

```
ETag returned by HEAD:        "9abcde3c584628a02620bf796dee1204"
PUT If-Match: "9abcde3c..."   -> 412 PreconditionFailed   (matching value, rejected)
PUT If-Match:  9abcde3c...    -> 200                      (same value, de-quoted)
PUT If-Match:  9abcde3c...    -> 412 PreconditionFailed   (now stale, refused)
GET                           -> 200, winner's bytes
```

The ETag is an HTTP entity tag and is quoted on the wire. AWS S3 and MinIO
compare the opaque value and accept the quoted form; RGW compares the header
bytes literally against its unquoted ETag. Before this subphase the store sent
the quoted form only, so a writable open against RGW failed closed with
`S3 conditional probe: a matching If-Match was unexpectedly rejected` and the
five store-level tests could not run.

### 3.2 Client change (measured, not guessed)

`S3Backend` now **measures** the comparison form at writable open and keeps it
for every conditional write (`IfMatchForm::{AsReturned, Dequoted}`):

1. `If-None-Match: *` must create once and refuse the second create.
2. A matching `If-Match` in the as-returned (quoted) form is tried first.
3. Only if that is refused, the de-quoted form is tried.
4. The chosen form must then pass all three of: a stale value in the **same**
   form is refused `412`, the object still holds the winner's bytes, and the
   matching write was readable.
5. If no form passes all three, the endpoint is refused exactly as before
   (`UnsupportedVersion`), never downgraded.

The choice is recorded in `StorageBackend::diagnostics` as
`S3 If-Match form measured at open: quoted-as-returned|dequoted`, and the
compatibility receipt records `"if_match_form"` on the CAS row. MinIO measures
`as-returned`; RGW measures `dequoted`. The writer lock is acquired before the
plane probe in `Store::open`, so `acquire_lock` negotiates the form itself;
otherwise an expired-lease reclaim could never succeed on RGW.

## 4. The existing S3 court logic, against RGW

`cargo test --test s3_backend -- --test-threads=1 --nocapture` in the rust court
with the RGW endpoint: **6 passed; 0 failed** in 44.77 s.

| test | what it proves |
|---|---|
| `capabilities_and_conditional_put_absent_or_idempotent` | declared capabilities, create-once/idempotent PUT, `Exists` on different bytes, range read, list, delete |
| `store_round_trip_over_s3` | init, seeded create, write, truncate, rename, snapshot, relation, `verify --full`, generation, head, **read-only reopen match** |
| `writer_lock_and_lost_conditional_head_update` | second live writer `Busy`; two writers with one ETag: first wins, second refused, head unchanged |
| `crash_proxy_over_s3_yields_old_or_new` | child process aborts after objects / after the commit file / after the superblock write; reopen is old-whole or new-whole, never partial |
| `gc_over_s3_deletes_only_unreachable_objects` | mark/sweep over S3 deletes only unreachable blobs, snapshot roots survive |
| `local_and_s3_artifacts_are_byte_identical` | the same operations produce identical commit/manifest/chunk bytes locally and over RGW |

The same suite still passes against MinIO with the modified client:
`bench/receipts/s3-court-20261106T160906Z.json` (6 passed; 0 failed).

## 5. Repository test suites at this change

* Rust: `cargo test --all-targets` — 92 suites, **418 passed, 0 failed**.
* FUSE: `cargo test --features fuse --all-targets` — 92 suites,
  **460 passed, 0 failed, 2 ignored** (the two pre-existing ignores:
  `concurrent_readers_and_one_writer_see_whole_versions` for the kernel 7.3
  direct-I/O EIO, and `permissions_with_allow_other` which needs CAP_SETUID).

## 6. What was *not* tested, and why

* **No media, power-loss, or device-cache claim.** The RGW OSD is a file on a
  tmpfs inside one container. A crash of the server process is not modelled
  here at all; R13's crash/restart court remains MinIO-only.
* **Single node, single zone.** No multisite replication, no second RGW, no
  monitor quorum beyond one, no network partition.
* **Only the eight matrix behaviours.** TLS/HTTPS, presigned URLs, multipart
  upload, bucket policies/ACLs, object versioning, object lock, lifecycle,
  server-side encryption, and `If-Unmodified-Since` are untested (the store does
  not use them).
* **Large-object and large-listing behaviour.** Single PUTs stay under the
  backend's own bound; listing pagination was exercised at `max-keys=2` with 5
  keys, not at the 1000-key page boundary.
* **No Go parity.** The WeftFS-Go store has no S3 backend, so there is no
  cross-language S3 claim against RGW.
* **No AWS S3, no other vendor.** Two implementations were measured; a third
  could still differ.
* **No production or fleet claim**, no concurrency court beyond two writers, and
  no claim that the RGW numbers are representative of a tuned cluster. The
  `radosgw-admin` user in the demo image is created with fixed access/secret
  keys on a private bridge with no TLS.

## 7. Aggregate coverage (`scripts/verify-all.sh`)

The Ceph court is the **28th court** in the aggregate, so every verification run
proves the S3 backend against an independent implementation. Its exit status is
mapped explicitly: `0` = pass, a real failure = fail (the aggregate fails), and
`78` = skip. Skip is reserved for environment refusals detected before the
server starts — non-x86_64, no Docker daemon, the pinned image cannot be
obtained, or the court's private subnet is already in use — and it is recorded
as `skip` with the court's own reason under `skips` in the verify receipt. The
rule is explicit: a skip is not a pass. The receipt's `summary` block reports
`courts`/`passed`/`failed`/`skipped`, and its `receipts` block names this
court's receipt for the same stamp.

First aggregate run including the court:
`bench/receipts/verify-20261106T162208Z.json` — **28/28 courts pass, 0 skipped**
at WeftFS `0e3670a`; Ceph court receipt
`bench/receipts/ceph-rgw-20261106T162208Z.json`.

The skip path is measured, not asserted. A second aggregate run at the same
code with `WEFT_CEPH_IMAGE` pointed at a nonexistent digest produced
`bench/receipts/verify-20261106T162933Z.json`:
`summary {courts 28, passed 27, failed 0, skipped 1}`, `"ceph_rgw": "skip"`,
`skips.ceph_rgw = "cannot obtain pinned image ... manifest unknown"`,
`result_note = "1 court(s) skipped explicitly; a skip is not a pass"` and
`result = pass`. The other 27 courts passed. That run also re-ran the Buck2,
agent-memory, and fuzz-diff courts green; their output is inside the run's
committed log, and the duplicate side receipts are not committed separately.
This negative run is **not** the current pass receipt: the all-pass run above
is.

## 8. Reproduce

```sh
# full: MinIO matrix, RGW court + matrix, side-by-side comparison
bash scripts/ceph_court.sh

# RGW only, matrix only
bash scripts/ceph_court.sh --matrix-only --no-minio

# MinIO matrix alone
bash scripts/s3_court.sh --matrix
```

Every container runs through `scripts/docker-court.sh`; the RGW sidecar carries
its own hard limits and is removed on exit by the script's trap.
