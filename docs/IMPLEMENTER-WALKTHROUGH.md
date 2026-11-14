# Implementer walkthrough: the shortest honest path

This is for a stranger who wants to read, decode, verify or re-implement
WeftFS. It is not a sales document. It tells you what to read **in order**,
which vectors to run, how to use the clean-room verifier as an oracle, and what
you must not conclude when you are done.

## 0. Before you start: what can and cannot be concluded

What exists today:

* four repositories with frozen formats and a conformance corpus;
* **two independent implementations of the codec** (Rust `WeftCodec`, Go
  `WeftCodec-Go`) that decode each other and byte-match on the deterministic
  modes;
* **two implementations of the store** (Rust `weftfs`, Go `WeftFS-Go`) that
  share the on-disk format and hand off to each other;
* a **clean-room verifier** in Python standard library (`verifier/`) written
  against the prose, with its own BLAKE3, WCB and Ed25519, importing no WeftFS
  code.

What does **not** exist: a third-party implementation. The verifier is
first-party, though it is independent of the Rust and Go code paths by
construction (it is a separate implementation, not a binding). Passing the
corpus therefore tells you that three implementations written by the same
programme agree. It does not tell you that an outside implementer would agree
without asking questions — eight under-specified points found by writing the
verifier are recorded in `spec/CONTEXT-PLANE-FREEZE.md` and
`conformance/REQUIREMENTS.md`, and you should expect to hit them.

## 1. Reading order

Read only what your task needs, in this order. Each step is short.

| # | Document | Why here |
|---|---|---|
| 1 | [`spec/README.md`](../spec/README.md) | one-page index and status vocabulary (frozen / experimental / proposed) |
| 2 | [`spec/WEFTCODEC-FORMAT.md`](../spec/WEFTCODEC-FORMAT.md) | the frame, WCB canonical bytes, modes 0–5, the error taxonomy (§11) |
| 3 | [`docs/WRITE-YOUR-OWN-DECODER.md`](WRITE-YOUR-OWN-DECODER.md) | exact check order, the 50-line skeleton, the traps |
| 4 | [`spec/WEFT-AI-CODEC.md`](../spec/WEFT-AI-CODEC.md) | modes 6–20, the Huffman coder shared by 9–16, mode 19 provenance binding |
| 5 | [`spec/ERROR-CLASSES.md`](../spec/ERROR-CLASSES.md) | the class registry and the RFC §13 reconciliation, so you classify the same way we do |
| 6 | [`spec/WEFT-MODEL.md`](../spec/WEFT-MODEL.md) | store identity, state, commit protocol, durability |
| 7 | [`spec/WEFT-CONTEXT-ROOT.md`](../spec/WEFT-CONTEXT-ROOT.md), [`spec/WEFT-CAPABILITIES.md`](../spec/WEFT-CAPABILITIES.md), [`spec/WEFT-PROVENANCE.md`](../spec/WEFT-PROVENANCE.md) | the context plane: addressing, authorization, receipts |
| 8 | [`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md), [`spec/WEFT-TRANSPORT.md`](../spec/WEFT-TRANSPORT.md), [`spec/WEFT-KV-PROVENANCE.md`](../spec/WEFT-KV-PROVENANCE.md), [`spec/WEFT-RETENTION.md`](../spec/WEFT-RETENTION.md) | optional surfaces, each with its own implemented/refused boundary |
| 9 | [`docs/ARTIFACT-API.md`](ARTIFACT-API.md) | the one API DCP, Orbax and an agent consumer share: `resolve`, `plan`, `materialize`, `verify`, `pin`, `release`, transactions |
| 10 | [`spec/rfc/RFC-0001-context-plane.md`](../spec/rfc/RFC-0001-context-plane.md) | **a proposal, not a standard.** Read it last, and read its status line first |
| 11 | [`docs/DRIVER-BOUNDARY.md`](DRIVER-BOUNDARY.md) | if you are writing a backend/transport adapter rather than a decoder |

Do **not** read `docs/STATUS.md` as specification: it is a dated log with
receipts, including losses.

## 2. Vectors: what to run

The corpus is data, not tests: `conformance/` holds vector files and
`conformance/manifest.json` indexes them with kind, mode, expectation
(accept/reject + class), and a sha256 per file.

```bash
# 1. Is the corpus intact and complete?
bash scripts/gen-conformance-manifest.sh --check
bash scripts/check-conformance-coverage.sh      # modes 0-20: valid + reject

# 2. Run the corpus against your decoder through the adapter contract.
bash scripts/run-conformance.sh ./my-adapter.sh
```

The adapter contract is one JSON object per invocation, documented in
[`docs/CONFORMANCE.md`](CONFORMANCE.md). Reference adapters (Rust, Go, Python)
shell out to a CLI and import no library. A vector's expected class is part of
the contract; a decoder that returns `Malformed` where the vector says
`Corrupt` has failed the vector, even though both are refusals.

Start with modes 0–5 (RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED).
Do not move on until every reject vector returns the recorded class. Then
modes 6–20; several of them share the byte-plane Huffman coder, so a single
bug there fails many vectors at once (that is a feature: it is how we caught a
4 GiB preallocation bug and two strictness divergences).

## 3. The clean-room verifier as an oracle

`verifier/` is Python standard library only: its own BLAKE3, its own WCB
reader, its own Ed25519, no import of `weftfs`/`weftcodec`, no subprocess, no
FFI. It exists so that "the corpus passes" is not only a statement about our
own code.

```bash
bash scripts/context-plane-verifier_court.sh
# -> counts, plus a receipt in bench/receipts/context-plane-verifier-<stamp>.json
```

Use it three ways:

1. **As a differential oracle.** Implement a record type, then compare your
   canonical bytes and root against the verifier's on the same input. If they
   differ, the prose is ambiguous or one of you is wrong; both are worth
   knowing before you ship.
2. **As a negative oracle.** Feed your implementation the verifier's reject
   cases: a valid signature with a mutated body, a rolled-back revocation
   record, a forged provenance record. It must refuse with the same class.
3. **As a completeness check.** The verifier deliberately implements the
   *frozen* subset and skips what the prose does not pin. A skip is a gap in the
   specification, not a free pass: the reasons are printed and recorded.

`verifier/history_check.py` is a standalone transactional-history checker (JSONL
in, verdict out); `verifier/producer_attest.py` verifies a producer attestation
without the Rust code (see `docs/PRODUCER-ATTESTATION.md`).

## 4. Order of checks

Classification depends on order. `docs/WRITE-YOUR-OWN-DECODER.md` lists the
exact order the reference decoders use; the short version for a frame is:
magic → version → feature words → dependency table → chunk table → payload
mode → declared lengths → body decode → trailing frame checksum →
content hash. A decoder that checks the content hash first will report `Corrupt`
for frames that are structurally `Malformed`, and will fail the corpus for a
reason that has nothing to do with cryptography.

For the store: `FORMAT` → superblock slot validity (CRC) → generation order →
commit record canonicality → state replay → object/manifest hash → optional
index attestation. A rejected index is rebuilt, never trusted.

## 5. What you may not conclude

* **That a third-party implementation exists.** None does. The verifier is
  first-party.
* **That the RFC is normative.** `spec/rfc/RFC-0001-*.md` is a draft candidate
  proposal. Where it disagrees with `spec/WEFT-*.md`, the latter wins.
* **That the corpus is exhaustive.** It is a curated corpus (plus a
  deterministic fuzzer diff between Rust and Go). Two implementations agreeing
  does not prove a third would.
* **That the modes are equally mature.** Mode 18 is opt-in and experimental in
  selection; several AI modes are specified-only with no wire value. Read
  `docs/STATUS.md` for the per-mode line.
* **That performance claims transfer.** Every published number names a court,
  a fixture, a receipt stamp and a revision. None is a fleet or cloud claim.
* **That error strings are stable.** Only the class is.
* **That encryption at rest hides equality.** It does not
  (`docs/ENCRYPTION.md` §4).
* **That a signature proves computation ran.** It authenticates a declaration
  (`docs/PRODUCER-ATTESTATION.md` §6).

## 6. If you only have an hour

1. `spec/README.md`, `spec/WEFTCODEC-FORMAT.md` §1–§4 and §11.
2. `docs/WRITE-YOUR-OWN-DECODER.md`.
3. `bash scripts/gen-conformance-manifest.sh --check` and one run of
   `scripts/run-conformance.sh` against the Rust adapter — watch the classes.
4. `spec/ERROR-CLASSES.md` §3 (what we deliberately did *not* invent).
5. Skim `docs/ARTIFACT-API.md` for the shape of the context plane.

That is enough to decode a frame and to know what you have not verified.
