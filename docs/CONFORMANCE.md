# Conformance: manifest, runner contract, and coverage

This document is the third-party entry point for WeftCodec v1 conformance. It
describes the committed manifest, the minimal implementation contract, the
reference adapters, and the checks that fail closed in CI.

Normative format text lives in [`../spec/`](../spec/). The vectors live in
[`../WeftCodec/conformance/v1/`](../WeftCodec/conformance/v1/) and are mirrored
byte-for-byte into `WeftCodec-Go/conformance/v1/`.

For the decoder itself, see
[`WRITE-YOUR-OWN-DECODER.md`](WRITE-YOUR-OWN-DECODER.md).

---

## 1. Manifest

[`../conformance/manifest.json`](../conformance/manifest.json) is generated and
committed. It enumerates every vector file in:

- `WeftCodec/conformance/v1/**` (the canonical Rust-produced corpus),
- `conformance/context/v1/causal_versions.json`,
- `conformance/checkpoint/v1/**` (the `weft.checkpoint.v1` identity record),
- the composite `conformance/store/v1` fixture and the fuzz seed corpus.

Each entry:

```json
{
  "path": "WeftCodec/conformance/v1/codec/001_raw_empty.json",
  "kind": "frame",
  "vector_name": "001_raw_empty",
  "sha256": "<sha256 of the vector file>",
  "expect": {
    "ok": true,
    "content_root": "<32-byte BLAKE3, lowercase hex>",
    "decoded_sha256": "<sha256 of the exact decoded bytes>"
  },
  "mode": 0,
  "mode_name": "raw",
  "mirrors": [
    "WeftCodec-Go/conformance/v1/codec/001_raw_empty.json"
  ]
}
```

Reject entries use `"expect": {"error": "<Class>"}`. Superseded historical
vectors carry `"superseded_by": "<new vector name>"`; the replacing entry
carries `"supersedes": ["<old name>"]`. Replay suites must skip superseded
vectors.

`content_root` is always the format's own identity for the decoded object:

| Kind | `content_root` |
|---|---|
| frame | the frame's `content_hash` (`H("weft/chunk/v1", decoded bytes)`) |
| wcb | `H("weft/structured/v1", canonical WCB bytes)` |
| selector | `SelectorId` |
| context (Context Root) | `ContextRootId` |
| context (causal closure) | `closure_hash` |
| tensor logical/shards | `logical_root` |
| tensor layout | `null` (the descriptor has no independent root) |
| sharded set (mode 10) | `layout_root` |
| weight/semantic delta (11/14) | the frame content hash (`target_hash`) |
| kv-cache (12) | `null` (no aggregate format identity) |
| activation-stream (13) | `null` (no aggregate CLI identity) |
| capability | `EnvelopeId` |
| checkpoint | `H("weft/checkpoint/manifest/v1", canonical bytes)` |
| transport | the container's `payload_hash` (`H("weft/transport/v1", enclosed frames)`); `decoded_sha256` is over the concatenated enclosed frame bytes |

`decoded_sha256` is SHA-256 over the exact byte string the vector records as the
decoded result (`plaintext_hex`, `target_hex`, `values_hex`, canonical record
bytes, concatenated page/chunk elements). `null` means the format does not
define a single decoded byte string for that kind.

The top-level `aggregate_sha256` is SHA-256 over sorted
`"<sha256>  <path>\n"` lines of every entry that is not the `store` or `fuzz`
fixture. The current manifest has 420 entries and aggregate
`6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`. The
`mirrors` array records cross-repository byte identity and aggregates; see §4.

### Generate and check

```sh
scripts/gen-conformance-manifest.sh          # write conformance/manifest.json
scripts/gen-conformance-manifest.sh --check  # fail on drift
```

The generator is stdlib-only Python 3, deterministic, and never compiles or
executes an implementation. It fails closed if a mirror is missing or differs.

---

## 2. Minimal implementation contract

An implementation is a program that accepts one vector path and prints one JSON
object. The runner invokes it exactly as:

```sh
<adapter> VECTOR.json
```

The adapter prints **exactly one** object on stdout:

```json
{"result": "ok", "content_root": "<hex>", "decoded_sha256": "<hex>"}
{"result": "error", "class": "Malformed"}
{"result": "skip", "reason": "not implemented"}
```

- `result` is required and is `ok`, `error`, or `skip`.
- `ok` carries `content_root` and/or `decoded_sha256`. Compare only the fields
  the manifest declares non-null.
- `error.class` is the normative class from
  [`WEFTCODEC-FORMAT.md` §11](../spec/WEFTCODEC-FORMAT.md#11-error-taxonomy-normative-classes).
  For `kind: capability`, it is the capability rejection name from
  [`WEFT-CAPABILITIES.md` §9](../spec/WEFT-CAPABILITIES.md) (`BadSignature`,
  `WrongGrantee`, `Expired`, `Revoked`, `UnknownSchema`, `NonCanonical`,
  `Malformed`), because that vocabulary is the interoperable one for envelopes.
- `skip` is explicit and counted separately. It is never evidence of a pass.
- The adapter exits 0 when it printed a contract object. Any non-zero exit, a
  missing object, or invalid JSON is reported as an adapter failure and fails
  the run.

For implementations that expose a single-frame decode verb, the natural
spelling of the contract is:

```sh
decode --json FILE.wft1
```

and it must print the objects above. Both shipped CLIs instead separate the
input and output paths (`decode --input FILE --output FILE`); the reference
adapters materialize the vector's frame, run that command, and emit the same
contract object. The runner never depends on a particular CLI spelling.

Dependencies are supplied as files named by their lowercase hex content hash
(the format's resolver contract). Compound AI-mode vectors are decoded through
the implementation's own shard/page/chunk verbs; see the adapter sources.

---

## 3. Runner

```sh
scripts/run-conformance.sh <adapter-command> [--json RECEIPT] [--kind KIND]
                           [--include-fixtures] [--fail-on-skip] [--quiet]
```

The runner:

1. reads the committed manifest (no regeneration),
2. skips superseded vectors and, by default, the `store`/`fuzz` fixtures,
3. invokes the adapter once per vector,
4. compares against `expect`,
5. prints `VECTOR MISMATCH <path>: expected ... got ...` on any mismatch,
6. exits non-zero if any vector mismatched or any adapter failed.

`--json` writes a receipt shaped like:

```json
{
  "schema": "weft.conformance.run.v1",
  "implementation": "scripts/conformance-adapter-rust.sh",
  "manifest_aggregate_sha256": "...",
  "total": 420,
  "pass": 288,
  "fail": 0,
  "skip": 132,
  "result": "pass",
  "mismatches": [],
  "skips": [{"path": "...", "reason": "..."}]
}
```

Skips are never silently converted to passes. `--fail-on-skip` is available
when a stricter gate is wanted.

### Reference adapters

| Adapter | Implementation | Court |
|---|---|---|
| `scripts/conformance-adapter-rust.sh` | Rust `weftcodec` | `rust` (build), `fuse` (run) |
| `scripts/conformance-adapter-go.sh` | Go `weftcodec` | `go` (build), `fuse` (run) |
| `scripts/conformance-adapter-python.sh` | independent Python `weftcodec` | `python` |

Environment:

- `WEFTCODEC_BIN`: the codec binary. Defaults are
  `.docker-cache/target-rust/release/weftcodec` and
  `.docker-cache/weftcodec-go`.
- `WEFT_PYTHON`: Python interpreter for the Python adapter (default
  `sys.executable`).
- `WEFT_PYTHONPATH`: directory containing the `weftcodec` package
  (`/work/python` in the court).
- `WEFT_CONFORMANCE_TIMEOUT`: per-vector timeout, seconds (default 60).

Exact court commands used for the committed receipts:

```sh
# Build once per language.
scripts/docker-court.sh rust \
  "cd /work/WeftCodec && cargo build --release --quiet"
scripts/docker-court.sh go \
  "cd /work/WeftCodec-Go && CGO_ENABLED=0 go build -o /work/.docker-cache/weftcodec-go ./cmd/weftcodec"

# Run the contract in a court that has Python 3.
scripts/docker-court.sh fuse \
  "PYTHONPATH=/work/python scripts/run-conformance.sh \
     scripts/conformance-adapter-rust.sh --quiet --json bench/receipts/conformance-rust.json"
scripts/docker-court.sh fuse \
  "PYTHONPATH=/work/python scripts/run-conformance.sh \
     scripts/conformance-adapter-go.sh --quiet --json bench/receipts/conformance-go.json"
scripts/docker-court.sh python \
  "PYTHONPATH=/work/python WEFT_PYTHONPATH=/work/python \
     scripts/run-conformance.sh scripts/conformance-adapter-python.sh --quiet \
     --json bench/receipts/conformance-python.json"
```

Committed reference results (current 420-entry manifest, aggregate
`6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`; receipts
`bench/receipts/conformance-rust-20261111T011110Z.json`,
`bench/receipts/conformance-go-20261111T011110Z.json`,
`bench/receipts/conformance-python-20261111T011110Z.json`):

| Implementation | Pass | Fail | Skip | Modes executed |
|---|---:|---:|---:|---|
| Rust `weftcodec` | 288 | 0 | 132 | 0–20 |
| Go `weftcodec` | 288 | 0 | 132 | 0–20 |
| Python `weftcodec` | 229 | 0 | 191 | 0–17 |

The Python replay is 229 pass, 0 fail, 191 skip; those skips are itemised
below and none is converted to a pass.

The transport family (`transport/`, 22 files: 5 valid, 17 reject) is executed
by both the Rust and Go reference adapters (22/22 pass each). The Python
adapter reports those vectors as explicit skips because the Python reader has
no `weft.transport.v1` parser. The same is true for modes 18, 19, and 20: the
Python adapter has no `float-tiled-streams`, `kv-prov`, or `engram` path, so
those vectors are explicit skips, not passes.

The Rust/Go 132 skips are non-codec surfaces plus supersession:

| Reason | Count |
|---|---:|
| record-level reject has no CLI verb | 42 |
| adversarial frame vectors with no adapter (`kind: frame`, 36 files) | 36 |
| canonical WCB values (`kind: wcb`) | 20 |
| `weft.checkpoint.v1` manifests | 5 |
| WFA1 archive verbs (`archive-decode` 4, `archive-create` 1) | 5 |
| tensor layout descriptors | 4 |
| semantic roots | 3 |
| composite store/fuzz fixtures (excluded by default) | 2 |
| `causal_versions` composite fixture | 1 |
| superseded historical vectors (including the reserved-mode rejects) | 14 |

(The 36 `kind: frame` entries are the adversarial vectors, whose manifest kind
is `frame` but whose adapter dispatch is the adversarial subkind; the receipt
prints `kind None has no adapter` for the two adapter tables that do not model
that subkind.)

The Python reader executes every vector for the kinds it implements: codec
frames, adversarial decode vectors, shard sets, delta bases (including
frame-source cycles), KV pages, activation chunks, tiled frames, semantic
roots, and canonical WCB values. Its 191 skips are:

| Reason | Count |
|---|---:|
| WEFT-TENSOR layout/shard containers (outside the WeftCodec reader) | 29 |
| WeftQL selector grammar | 22 |
| `weft.transport.v1` containers | 22 |
| mode 20 `ENGRAM_SHARD` vectors | 20 |
| mode 19 `KV_CACHE_PROV` vectors | 20 |
| Context Root resolution (includes `causal_versions`) | 17 |
| mode 18 `FLOAT_TILED_STREAMS` vectors | 16 |
| causal closure identity | 10 |
| capability envelope verification | 7 |
| `weft.checkpoint.v1` manifests | 5 |
| WFA1 archive verbs (`archive-decode` 4, `archive-create` 1) | 5 |
| `selector-id` adversarial verb | 2 |
| composite store/fuzz fixtures (excluded by default) | 2 |
| superseded historical mode-reject vectors | 13 |
| reserved-mode 21 reject (`112_reserved_mode_21`) | 1 |

Per mode, the Python skips are: mode 18 `17`, mode 19 `22`, mode 20 `21`,
and every other mode `0` except the one superseded historical reject in modes
6, 8, 9, 10, 11, 12, 14, 15, 16, and 17. The mode-19 and mode-20 totals
include the historical reserved-value rejects that carry those mode numbers in
their frame headers.

For modes 0–17 every valid and reject vector is executed; the other skips are
non-codec surfaces. The previous claim that the Python reader executed every
mode 0–18 vector was true before mode 18 existed and is corrected here.

The earlier aggregate `51fa3dd9…` receipt triple (Rust/Go 246/92, Python
229/109) is superseded and retained. The current Python test receipt
`bench/receipts/python-bindings-20261107T160721Z.json` records 369 tests, 0 failures, 0 errors, 10 skips. `test_ai_modes.py` decodes every valid AI-mode vector to its recorded bytes; `test_random_differential.py` generates at least 2,000 frames per mode 9–16, plus 1,600 body mutants, and compares them with the Rust CLI.

The Rust and Go reference adapters intentionally skip vector kinds their public
CLI does not expose as a verb: raw WCB decoding, `weft.checkpoint.v1`
manifests, record-level rejects that only have a bytes-in API, archive vectors,
tensor *layout descriptors* (no values), semantic roots, the composite
`causal_versions` fixture, and the two composite fixtures. Each skip carries a
reason in the receipt. The implementation test suites still replay those
vectors directly; the skip is about the CLI contract, not about the format.

---

## 4. Cross-repository identity

`WeftCodec/conformance/v1` and `WeftCodec-Go/conformance/v1` are compared file
by file. The generator fails if any file is missing or differs. The manifest
records:

- `files_from` / `files_to`, `common_files`,
- `identical: true`,
- `aggregate_sha256_from`, `aggregate_sha256_to`,
- `aggregate_sha256_common` (the value to quote for the shared corpus),
- `only_from` / `only_to` / `differing`.

Current shared corpus: 413 files, aggregate
`aa3abcad1f73573990b64ee8d0397265627f1221b37c05a726a2b324c32b50c9`.
The store fixture (`conformance/store/v1` ↔
`WeftFS-Go/conformance/store/v1`) shares 25 data files with common aggregate
`d3ceef9e14518fd99fd7f9c05dde6e677bc500cd892d440da8a5cec7e68a2d4f`; the Go
repo additionally carries `PROVENANCE.md`, which is metadata and is listed in
`only_to`.

---

## 5. Coverage gate

```sh
scripts/check-conformance-coverage.sh
```

Fail-closed checks:

1. `conformance/manifest.json` is current (`gen-conformance-manifest.sh --check`).
2. Every frame mode 0–20 has at least one valid vector **and** at least one
   reject vector.
3. Every identity record kind (`wcb`, `selector`, `context`, `tensor`,
   `checkpoint`, `capability`) has at least one valid vector.
4. Every valid expectation carries a content root or a decoded SHA-256; every
   reject expectation carries a non-empty class.
5. Every entry's `sha256` matches the file (directory aggregate for `store`),
   and every mirror exists and matches.

`scripts/verify-all.sh` runs this as the `conformance_coverage` court and the
manifest drift check as `conformance_manifest`; both are host-only and fast.

Current coverage:

```
mode  valid/reject
0     6/20
1     2/6
2     1/2
3     1/9
4     1/2
5     1/5
6     3/8
7     2/6
8     3/8
9     3/14
10    3/8
11    3/8
12    3/8
13    3/8
14    3/6
15    3/8
16    4/14
17    3/8
18    4/12
19    8/12
20    6/14
```

Identity kinds: WCB 12 valid, selector 22, context 11, tensor 12,
checkpoint 1, capability 2.

---

## 6. Adding a vector

Vectors are append-only. Never rewrite, reorder, or "fix" an existing vector.

1. Add the new file in `WeftCodec/conformance/v1/<family>/` with a
   deterministic name and, for rejects, the exact normative class.
2. Copy it byte-for-byte into the same relative path under
   `WeftCodec-Go/conformance/v1/` (and add a Go replay assertion where the
   family has one).
3. Add a Rust replay assertion (and a Go one) so `cargo test` / `go test` fails
   if the vector is wrong.
4. Run `scripts/gen-conformance-manifest.sh` and commit
   `conformance/manifest.json`.
5. Run `scripts/check-conformance-coverage.sh`.
6. Run the language courts and the runner; attach the receipts.

If a new mode is allocated, add a valid vector and at least one reject vector;
the coverage gate will fail otherwise.

---

## 7. Third-party distribution

```sh
scripts/package-conformance.sh
```

builds `bench/bundles/conformance-<version>.tar.zst` and verifies it by
extracting it to a private directory and running the runner **from the
extracted copy only**. The bundle contains the manifest, every vector, the
runner, the adapter contract, `docs/WRITE-YOUR-OWN-DECODER.md`, `docs/RFC.md`,
the reference Rust/Python adapters, and the independent Python reader. Its
`RUN.md` gives the two commands a third party needs.

The bundle is also shaped as a `weftfs.receipt-bundle/1`, so
`scripts/verify-receipts.sh` checks its checksums and revisions without trusting
the producer. `scripts/verify-conformance-python.sh` runs the Python second
opinion in the python court (where `blake3` is installed) and merges its
per-mode skip table into the receipt.

Current evidence: `bench/receipts/conformance-bundle-20261111T012244Z.json`.
Bundle SHA-256 `3d945b516a6c3bd38a7ab8f6f90e6a7a53caed5469758f5dec9dc86abd46b8df`,
538 files, 420 manifest entries, aggregate `6287b817…`. Extracted-copy runs:
Rust 288 pass / 0 fail / 132 skip; Python 229 / 0 / 191. Python skips per mode:
18 → 17, 19 → 22, 20 → 21; every other skip is a non-codec surface or a
superseded historical reject. `verify_receipts: pass`.
