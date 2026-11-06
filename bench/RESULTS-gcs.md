# R14 — GCS backend: court results

Two receipts from one run at WeftFS `ca7004c`:

| artifact | path |
|---|---|
| court (emulator identity, container limits, commands) | `bench/receipts/gcs-court-20261106T150444Z.json` |
| harness (exactness, generation sequence, crash proxy) | `bench/receipts/gcs-20261106T150444Z.json` |
| unit-test log / summary | `bench/receipts/gcs-20261106T150444Z.tests.log`, `.tests.txt` |

Produced by `bash bench/scripts/gcs_court.sh`, which:

1. downloads the pinned `fake-gcs-server` release
   `v1.56.1` (`fake-gcs-server_1.56.1_Linux_amd64.tar.gz`, sha256
   `0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`) into
   `.docker-cache/gcs`;
2. runs the binary **inside** the `weft/rust-court:1` container
   (`sha256:d7c672a4…5b8d03`) on `127.0.0.1:4443` with `-backend memory`;
3. runs the in-process fake-GCS unit suite and the end-to-end harness in that
   same container.

Every hard limit is unchanged: 3 GiB memory == swap, 4 CPUs, 1024 pids,
`/tmp` 1 GiB tmpfs, `no-new-privileges`, `cap-drop=ALL`.

If the tarball cannot be obtained or its checksum does not match, the script
writes a `weft-gcs-court-v1` receipt with `"status": "emulator_unavailable"`,
the exact command, and the captured error, and exits without running the court.
That path was not taken in this environment (the download succeeded), so no
`emulator_unavailable` receipt exists for this run.

## Emulator identity

| field | value |
|---|---|
| product | `fake-gcs-server` (fsouza/fake-gcs-server) |
| release | `v1.56.1` |
| tarball sha256 | `0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469` |
| backend | memory |
| endpoint | `http://127.0.0.1:4443` |
| auth | anonymous (the emulator court sends no `Authorization` header) |

`fake-gcs-server` is a test emulator. It implements the JSON API subset used
here — object metadata, `alt=media` with `Range`, simple and resumable uploads
with `ifGenerationMatch`, object listing, bucket insert, delete — and it returns
`412` for a failed precondition. It is not Google Cloud Storage: no IAM, no
bucket policy, no requester-pays, no multi-region behaviour, no media
durability.

## Workload

Deterministic, seeded ObjectIds, fixed clock (`1_700_000_000_000_000_000`):

* 12 files × 32 KiB (393,216 visible bytes), chunk size 8 KiB, checkpoint
  interval 4;
* 8 range writes of 4 KiB;
* full read of every file with a BLAKE3 digest;
* `verify --full`;
* close, reopen read-only, `verify --full` again;
* three CLI crash phases over GCS (below).

## Exactness

| field | local | GCS | match |
|---|---|---|---|
| content root | `0a8198f56ed59a11…` | same | **yes** |
| commit generation | 21 | 21 | **yes** |
| commit sequence | 1…21 | 1…21 | **yes** |
| verified bytes | 393,216 | 393,216 | **yes** |
| content digest | `2e3b9902…` | same | **yes** |
| `verify --full` problems | none | none | — |
| read-only reopen | gen 21, same root | gen 21, same root | **yes** |

Local and GCS publish the same logical bytes because both carry the identical
manifest/commit/object layout; only the physical system differs.

## Generation preconditions

The superblock is a single control object replaced on every commit. Its GCS
generation was observed three times during the run:

```
1793977493537315   (after init)
1793977493551975   (after 12 creates, 21 commits)
1793977493563518   (after 8 range writes)
```

Strictly increasing. Each value is a different GCS generation of the same
object, which is only possible because every replacement passed
`ifGenerationMatch=<observed generation>`; an unconditional overwrite would not
advance the generation through a precondition at all (and the probe would have
refused the endpoint).

The plane probe recorded, against the real emulator:

```json
{"backend": "gcs", "attempted": true, "create_ok": true,
 "matching_compare_exchange_ok": true,
 "stale_compare_exchange_rejected": true,
 "token_kind": "content-token"}
```

That `stale_compare_exchange_rejected` is the load-bearing check: the probe
performs a *stale* `ifGenerationMatch` and requires `412`. The in-process fake
has a matching test (`probe_refuses_an_endpoint_that_accepts_a_stale_generation`)
that makes the fake accept stale generations and asserts the backend refuses it
with `UnsupportedVersion`, including at `Store::init_with_backend`.

## Crash proxy

The `weftfs` CLI is re-executed with `WEFT_CRASH_PHASE` set and aborts by signal
inside the publication path. After waiting out the 2 s lock lease, the store is
reopened read-write and read:

| phase | abort signal | visible state | generation | `verify --full` |
|---|---|---|---|---|
| `after_objects` | yes | old | 2 (unchanged) | clean |
| `after_commit_file` | yes | old | 2 (unchanged) | unpublished tail (documented) |
| `after_superblock_write` | yes | new | 3 | clean |

3/3 old-or-new, **0 partial**. The `after_commit_file` case leaves a commit file
for generation 3 above the published head (generation 2). That is the documented
early-crash state: recovery ignores it, and `verify --full` reports it rather
than hiding it. It is the only crash-phase problem the harness accepts, and the
receipt labels it `"verify_state": "unpublished-tail"`.

## Timing (single run, no statistics claimed)

| phase | local | GCS |
|---|---|---|
| create 12 files | 1.44 ms | 14.64 ms |
| 8 range writes | 0.95 ms | 11.52 ms |
| full read | 0.46 ms | 4.49 ms |
| `verify --full` | 1.76 ms | 27.84 ms |
| read-only reopen | 0.21 ms | 9.86 ms |
| harness wall (includes 9 s of lease waits) | 6.7 ms | 100.1 ms |

Peak harness RSS 6,908 KiB. One run, one host, warm loopback; these are not
throughput claims. GCS is slower because every operation is an HTTP
request/response pair with metadata plus media round trips.

## Unit tests (in-process fake service)

`cargo test --test gcs_backend -- --test-threads=1` → **14 passed / 0 failed**
(0.89 s):

create-once success/idempotent/conflict, matched generation, stale generation
`Busy` at both the plane and the wire level, ranged reads and end-of-object,
missing objects as `NotFound`, transport failure as `Io` without token leakage,
listings in the normative layout, control create/delete, the honest probe,
the stale-accepting refusal (probe and store level), the full store round trip
against a local store, resumable upload above 5 MiB, anonymous mode, and
ambiguous-credential refusal.

## Defects found and fixed

1. **Bucket insert** was posted to `/storage/v1/b/<bucket>` with an empty body.
   GCS takes the bucket resource in the body of `POST /storage/v1/b?project=…`.
   The emulator answered `400 EOF`. Fixed to `{"name": …}` on the collection URL.
2. **Object listing** used `/storage/v1/b/<bucket>?prefix=…`, which is bucket
   metadata, not the object collection. Listings were empty and `verify --full`
   reported a broken commit parent chain. Fixed to
   `/storage/v1/b/<bucket>/o?prefix=…`.
3. **Court assertion**: the first end-to-end run asserted a clean
   `verify --full` for every crash phase, but `after_commit_file` legitimately
   leaves the documented unpublished tail. The harness now accepts exactly that
   one problem and labels it.

Retained failing receipts: `bench/receipts/gcs-court-20261106T145714Z.json`
(defect 1) and `bench/receipts/gcs-court-20261106T145838Z.json` (defects 2 and
3). They are kept because they found real defects.

## Losses and limits, stated plainly

* The emulator is not GCS. Nothing here is a cloud, fleet, multi-node,
  device-cache, media, power-loss, or production claim.
* Only bearer-token authentication is implemented. RS256 service-account
  signing needs an RSA implementation and none was added;
  `GOOGLE_APPLICATION_CREDENTIALS` alone is refused. There is no OAuth2 refresh
  flow, no workload identity, and no signed-URL support.
* Anonymous mode sends no credential. It exists for emulators and is refused if
  a token was also supplied.
* Uploads above 5 MiB take the resumable path with a single bounded request;
  there is no chunked retry, no `Content-Range` resumption after a dropped
  connection, and a `308` is a hard error. The in-process fake exercises the
  path; the emulator run does not (its objects are small).
* `read_control`/`compare_exchange` cost one metadata request plus one media
  request, and the generation is re-checked afterwards. On a busy object that
  means up to four attempts and then `Busy`.
* `modified()` returns `None`: the JSON API reports a timestamp string and this
  crate has no date parser. The store does not use it.
* No batch endpoint, no JSON multipart upload, no client-side retry/backoff on
  `429`/`5xx` (they surface as `Io`), no request signing of any kind.
* The in-process fake serves one listing page; pagination is implemented in the
  backend and exercised only against the single-page case.
* Writers are exclusive by design (one `LOCK` object, advisory lease). No
  multi-writer protocol is claimed or tested.
* The crash proxy is a process-fault proxy, not physical power loss. Each phase
  waits out the 2 s lease before reopening.
* Rust only. WeftFS-Go has no GCS backend; there is no cross-language GCS
  parity claim. The on-disk and on-wire WeftFS formats are unchanged.
