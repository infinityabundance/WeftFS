# Azure service-SAS acceptance (R23)

Date: 2026-11-06. Subphase R23. Revision: WeftFS `staging` `dcbe066`.
Artifacts: `bench/receipts/azure-20261106T235445Z.json` (sha256
`f4e294690f0040ccf88ab3982bcd4f2e603d8a3c24b45687838de120b4d9f551`) and its
`.tests.log`, `.tests.txt`, `.fencing.log`, `.fencing.txt`, plus the court
receipt `bench/receipts/azure-court-20261106T235445Z.json`.
Retained failing run that found the two defects below:
`bench/receipts/azure-20261106T234510Z.*`.

## 1. The gap this closes

R21 shipped three Azure credential forms but only SharedKey had been accepted
by a real service. SAS was tested for *request construction* (the query string
is appended verbatim, no `Authorization` header) and anonymous for sending no
credential at all. Neither had been through an endpoint that verifies them.

R23 mints a container-scoped **service SAS** from the documented 15-field
string-to-sign for `sv=2018-11-09` (HMAC-SHA256 over the account key, base64,
`+ / =` percent-escaped) and requires Azurite to decide.

## 2. Environment

| item | value |
|---|---|
| emulator | Azurite Blob service `3.35.0` |
| image | `mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37` |
| image id | `sha256:03f7cb26ef2cda2c06ec6760c91975a85460e7bb63277a0df5c9f1d02071ead3` |
| data | tmpfs `/data`, 1 GiB, memory-backed |
| test court | `weft/rust-court:1`, memory/swap 3g/3g, pids 1024, cpus 4, nproc 2048, `--cap-drop=ALL`, `no-new-privileges`, tmpfs `/tmp` 1 GiB |
| network | private user-defined bridge; the emulator is a sibling container on loopback |
| account | `devstoreaccount1` + the published Azurite development key |

Azurite is distributed on `mcr.microsoft.com`, not Docker Hub; no Docker Hub
image is claimed.

## 3. Commands

```sh
bash bench/scripts/azure_court.sh
# inside the court:
cargo test --test azure_backend -- --test-threads=1 --nocapture
cargo test --test azure_fencing -- --test-threads=1 --nocapture
cargo run --release --quiet --example azure_court -- --out bench/receipts/azure-20261106T235445Z.json
```

`azure_court.sh` prints exactly one `azure_court: skip <reason>` and exits 78
when Docker or the pinned image is unavailable; this run obtained the image and
did not skip.

## 4. What the service did

Counts at `dcbe066`: `azure_backend` **19 passed / 0 failed** (12.93 s);
`azure_fencing` **7 passed / 0 failed** (15.02 s); end-to-end court status
`ok`, local and Azure content digest `2e3b9902…`, generation 21,
`content_root_match` / `generation_match` / `generation_sequence_match` all
`true`.

| operation | credential | result |
|---|---|---|
| read-only store open (FORMAT, commits, superblock, manifests, objects) | service SAS `sp=rcwdl` | **accepted** |
| object read + generation equality | service SAS `sp=rcwdl` | **accepted** |
| `verify(true)` over the whole store | service SAS `sp=rcwdl` | **accepted** |
| create-once blob PUT + blob GET | service SAS `sp=rcwdl` | **accepted** |
| `HEAD ?restype=container` (Get Container Properties) | service SAS `sp=r` | **refused**, `Unauthorized` |
| `HEAD ?restype=container` | service SAS `sp=rcwdl` | **refused**, `Unauthorized` |
| `HEAD ?restype=container` | service SAS `sp=racwdl` | **refused**, `Unauthorized` |
| blob create-once | service SAS `sp=r` | **refused**, `Unauthorized` |
| blob create-once | service SAS `sp=rcwdl` / `sp=racwdl` | **accepted** |
| writable store open | service SAS `sp=rcwdl` | **refused** at `ensure_container`, `Unauthorized: Azure denied reading container \`…\`` |
| writable store open | service SAS `sp=r` | **refused** earlier, at the conditional-write probe: `Unauthorized: Azure denied PUT \`tmp/conditional-probe-…\`` |
| read-only open with a signature tampered `sp=r` → `sp=rcwdl` | — | **refused** at the FORMAT read, Azurite `AuthorizationFailure`, class `Unauthorized` |
| read-only open of a private container | anonymous | **refused** at the FORMAT read, Azurite `AuthorizationFailure`, class `Unauthorized` |

Raw evidence lines are in `azure-20261106T235445Z.tests.log` (prefix `R23 `).

### Why a writable open fails over a SAS

A writable open runs, in order: read FORMAT → `acquire_writer_lock` (which
first probes the two conditional-write forms with a private blob PUT) →
`ensure_layout` → `ensure_container` (a `HEAD ?restype=container`) →
`probe_planes`. Azurite accepts the blob data operations at every one of those
steps **except** the container-properties HEAD, under every permission set
tested, including full `racwdl`. The refusal is HTTP 403 and is surfaced as
`Unauthorized` naming the container.

This is recorded as a limitation, not worked around: skipping the
container-properties check when a SAS is configured would hide a real service
refusal and weaken the writable-open proof. A read-only open never calls
`ensure_container`, which is why SAS is fully usable for reading.

## 5. A real classification defect found and fixed

Before R23, `Store::open_impl` wrapped every non-`Io` error from the FORMAT
read as `Corrupt`. The tampered-SAS test showed the consequence: a service
`AuthorizationFailure` was reported as damaged data
(`Corrupt: cannot read FORMAT: Unauthorized: …`). A bad credential and a
damaged store are not the same diagnosis.

`src/store.rs` now preserves `Class::Unauthorized` (and `Class::Io`, as
before); a genuinely missing or mismatched FORMAT remains `Corrupt`. The
retained failing receipt `azure-20261106T234510Z` records the old behaviour
(`15 passed; 2 failed`).

## 6. Conclusions and non-claims

* A container-scoped service SAS **is** accepted for blob data operations on
  Azurite 3.35.0, including a full `verify`.
* It is **not** accepted for the container-properties check a writable store
  open performs, so a writable open requires SharedKey. An account SAS is the
  other documented escape hatch but was **not tested**; no claim is made for it.
* Anonymous **acceptance is unproven**. The court only proves that a private
  container refuses an anonymous request. Proving acceptance needs a real
  Azure account whose container ACL is `blob` or `container`; Azurite's public
  container access was not configured and is not claimed.
* No live Azure run, no cloud/multi-node/fleet claim, no media or power-loss
  claim, no throughput claim. Azurite is not Azure: service-version acceptance,
  large-block-blob limits, `x-ms-*` header edge cases, and listing paging
  remain untested against the real service.

## 7. Reproduce

```sh
bash bench/scripts/azure_court.sh
# environment: Docker host, network access to mcr.microsoft.com for the image
# record: revision_weftfs in the receipt, image digest, and both test summaries
```
