# Azure Blob in the independent Go store (subphase R14)

**Status:** implemented and exercised on one pinned Azurite emulator in the
OOM-protected Docker court. This is protocol and parity evidence, **not** an
Azure, cloud, multi-node, media, power-loss, or performance claim.

## What was added

`WeftFS-Go/store/azureclient.go`, `azureblob.go`, and `azurecommit.go` are a
real Azure Blob REST backend written from the same contract the Rust backend
speaks — not by calling Rust:

| Operation | Protocol |
|---|---|
| create-once publication | `Put Blob` + `x-ms-blob-type: BlockBlob` + `If-None-Match: *`; a second create is `409 BlobAlreadyExists` |
| compare-and-swap | `Put Blob` + `If-Match: <ETag>`; a stale ETag is `412` |
| ranged read | `Get Blob` + `Range: bytes=<start>-<end>` (`206`) |
| length / existence | `Get Blob Properties` (`HEAD`) |
| delete | `Delete Blob` (`202`/`204`) |
| listing | `List Blobs` (`?restype=container&comp=list`, marker paging) |
| head publication | one conditional `LOCK` write carrying the fencing token **and** the new head; the two-slot `superblock` is a cache; release is a content-addressed tombstone |

Credential forms are SharedKey (HMAC-SHA256 over the documented
string-to-sign, standard library only), SAS (appended verbatim), and anonymous
(emulator). Exactly one form is accepted; supplying two is refused rather than
guessed, and no secret is written to diagnostics or error text.

The ETag spelling is **measured** at a writable open, never assumed: a matching
value must be accepted **and** a stale value in the same form refused with the
winner's bytes surviving. An endpoint that ignores `If-None-Match: *`, that
accepts a stale `If-Match`, or that passes neither quoted nor dequoted form is
refused `Unsupported`.

## Court

Command: `bench/scripts/azure_court.sh` (starts a pinned
`mcr.microsoft.com/azure-storage/azurite` release as a sibling container under
the same hard limits, then runs both implementations).

| Layer | Command | Result |
|---|---|---|
| Rust backend | `cargo test --test azure_backend` | 19 passed / 0 failed (13.68 s) |
| Rust fencing | `cargo test --test azure_fencing` | 9 passed / 0 failed (18.68 s) |
| Go store | `go test -run 'TestAzure\|TestDecodeBase64' ./store` | **18 passed / 0 failed / 0 skipped** (0.476 s) |

* Stamp: `20261111T144613Z`
* Revisions: parent `e2b25b1`, WeftFS-Go `318d4b0`
* Emulator: Azurite 3.35.0,
  image digest `sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`
* Courts: `weft/go-court:1` (`sha256:da549900…ab59`), `weft/rust-court:1`
* Receipts: `bench/receipts/azure-court-20261111T144613Z.json`,
  `bench/receipts/azure-20261111T144613Z.json`,
  `bench/receipts/azure-go-20261111T144613Z.log`

The Go suite covers, by name:

* the exact SharedKey string-to-sign, including the path-style resource where
  the account legitimately appears twice, and the rule that an empty body signs
  an empty `Content-Length` even though the header is transmitted as `0`;
* canonical query ordering/encoding and the decoded resource lines;
* path encoding (`/` preserved for logical keys, everything else escaped);
* the strict base64 decoder (both alphabets, optional but consistent padding,
  non-canonical trailing bits rejected);
* configuration refusals: two credential forms, no credential form, empty or
  non-http endpoint, uppercase account/container, short container, leading
  hyphen, non-base64 key, short key, SAS starting with `?`;
* the full store over an in-process double: init, three writes, `verify(true)`,
  close, read-only reopen with exact bytes, unlink + snapshot + GC + verify;
* the blob-plane contract over the live emulator and the double: create-once,
  identical-byte idempotence, different-byte conflict, 17-byte ranged read,
  listing, honest `NotFound` after delete, and the 192-byte capability probe;
* two adversarial endpoints a well-behaved emulator cannot produce: one that
  ignores `If-None-Match: *` and one that accepts a stale `If-Match` — both are
  refused `Unsupported` at writable open;
* one endpoint that returns quoted ETags but compares `If-Match` literally: the
  probe measures the dequoted form, records it in diagnostics, and the store
  then works;
* a wrong account key maps to `Unauthorized`, not `Corrupt`;
* the Go MW2 boundary: `--writer-mode optimistic` over an `azure` commit plane
  is refused `Unsupported` (MW2 uses the POSIX publication lock and is
  implemented for the local and object commit planes only).

## Cross-language handoff

One container, both implementations, in the same court:

1. Go `Init` + `CreateFile` + `verify(true)` + close.
2. Rust `--json info` reports `"backend": "azure"` and the same generation;
   `verify --full` is clean; `cat /go-alpha.txt` returns byte-identical content.
3. Rust `create /rust-beta.txt --text …` + `checkpoint`.
4. Go read-only reopen observes the newer generation, reads
   `/rust-beta.txt` byte-identically, and `verify(true)` is clean.
5. Go writable reopen reclaims the Rust writer's released tombstone and
   publishes a new generation; Rust `verify --full` is clean afterwards.

## What is not claimed

* **No live Azure account.** SharedKey acceptance is proven against Azurite
  only. The SAS path is implemented and unit-tested for construction and
  credential exclusivity, but its acceptance is not exercised here; the Rust
  R23 court documents that Azurite 3.35.0 answers `403` to the container
  `HEAD` a writable open performs under a container-scoped service SAS.
  Anonymous acceptance is unproven (a private container refuses it).
* **No performance numbers.** Nothing here is a throughput, latency, or
  request-count result; no comparison with the Rust client is published.
* **Single node, tmpfs data, loopback.** No media, power-loss, replication,
  multi-node consistency, fleet, or cloud claim.
* **List Blobs paging** is implemented with markers, but the court exercised a
  single page (fewer than 1,000 keys); multi-page behaviour is unexercised.
* **Packing over Azure** is untested (the pack plane needs true ranged reads,
  which Azurite provides, but no court run has been made).
* **Go MW2** remains `local`/`object`; the refusal on `azure` is asserted, not
  a silent downgrade.
* `Modified()` reports Azure's `Last-Modified` from `HEAD`; the store does not
  depend on it for correctness.
