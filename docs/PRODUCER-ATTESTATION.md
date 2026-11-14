# Producer-authenticated provenance (T074)

An optional signed **producer attestation** binds a declaration — who produced
an artifact, in which run, with which tool version, from which input content
roots, to which output root, under which determinism class — to an Ed25519 key
the operator trusts. The implementation is `src/producer_auth.rs`; the driver is
`examples/producer_attest.rs`; independent verifiers live in
`WeftFS-Go/cmd/producer-attest-verify` and `verifier/producer_attest.py`.

## 1. The limit, first

**A signature authenticates a declaration, not that the computation ran.**

It does not prove:

* that the tool actually executed, or that the named tool version produced the
  bytes;
* that the declared inputs were read, or read in the declared roles;
* that the declared output was not produced some other way;
* that `determinism: exact` is true — it is a *declared* class, not a measured
  one;
* that the key set's epochs correspond to wall-clock time or to any external
  transparency log.

What it does prove: the declared fields are exactly the bytes the named key
signed, the key was trusted under the supplied key set at the declared signing
epoch (or is currently trusted, in the default mode), and no later revocation
or rotation contradicts it. `verify_attestation_against_store` additionally
proves that every declared input root and the output root are content in the
store at the pinned snapshot. That is a containment check, not an execution
proof.

## 2. Attestation record

Schema `weft.producer-attestation.v1`, canonical WCB map with exactly:

| Key | Type | Meaning |
|---|---|---|
| `schema` | text | `weft.producer-attestation.v1` |
| `producer` | text | declared producer identity (1..512 bytes) |
| `run_id` | text | declared run identifier |
| `tool` | text | declared tool name |
| `tool_version` | text | declared tool version |
| `determinism` | text | `exact`, `declared`, or `unknown` |
| `signing_epoch` | u64 | key-set epoch at which the producer signed |
| `key_set_root` | bytes32 | root of the key set the producer used |
| `signer` | bytes32 | raw Ed25519 verifying key, **inside the signed body** |
| `inputs` | array | `{role:text, content_root:bytes32}`, sorted by `(role, root)`, unique |
| `output` | bytes32 | content root of the produced artifact |
| `snapshot` | bytes32 or null | optional pinned store snapshot the claim is about |
| `issued_at_ns` | u64 | producer clock reading (declared) |
| `signature` | bytes64 | Ed25519 over the body with this key removed |

`AttestationId = H("weft/producer-attestation/v1", WCB(body without signature))`.
Because the signer is inside the body, a record cannot name one signer and be
signed by another: a wrong key id is a `BadSignature` for the named key, not a
search over the key set.

## 3. Key set and lifecycle

Schema `weft.producer-key-set.v1`:

| Key | Type | Meaning |
|---|---|---|
| `schema` | text | `weft.producer-key-set.v1` |
| `epoch` | u64 | this key set's epoch (monotonic; operator-declared) |
| `rotated_from` | bytes32 or null | root of the key set this one rotated from |
| `keys` | array | entries sorted by key bytes, unique |

Each key entry carries `key:bytes32`, `label:text`, `valid_from_epoch:u64`,
`valid_until_epoch:u64|null`, `revoked_at_epoch:u64|null`, and
`rotated_at_epoch:u64|null`. `KeySetRoot = H("weft/producer-keys/v1", WCB(record))`.

Lifecycle rules, all fail-closed:

* **Revoked** (`revoked_at_epoch <= key_set.epoch`) refuses in **every** mode.
  There is no grace window and no "signed before revocation so it is fine"
  exception.
* **Rotated** (`rotated_at_epoch <= key_set.epoch`, or expired) refuses in the
  default `current` mode: the operator has said the key is no longer trusted for
  new verification. `historical` mode accepts an attestation whose key was valid
  at its declared `signing_epoch`, and reports `key_status: retired` and
  `currently_trusted: false`. Historical mode is for audit and replay.
* **Not yet valid** at the signing epoch refuses in both modes.
* **Unknown signer** (key absent) refuses.
* Rotation is recorded explicitly: `rotate_key_set(previous, next_epoch, keys)`
  sets `rotated_from = previous.root()` and requires a strictly greater epoch.
  `validate_rotation_chain` bounds a chain to 64 unique nonzero roots.

Verification reports `signer`, `key_set_root`, `key_set_epoch`,
`signing_epoch`, `key_status`, `valid_at_signing`, and `currently_trusted`.

## 4. Refusal codes

| Code | `Error` class | Cause |
|---|---|---|
| `NonCanonical` | `NonCanonical` | bytes are not canonical WCB / not canonical order |
| `Malformed` | `Malformed` | wrong key set, wrong type, bound exceeded |
| `UnknownSigner` | `Unauthorized` | named signer absent from the key set |
| `BadSignature` | `Corrupt` | signature does not verify under the named signer |
| `RevokedKey` | `Unauthorized` | key revoked at the key set's epoch |
| `KeyRotated` | `Unauthorized` | key rotated/expired; use historical to audit |
| `KeyNotYetValid` | `Unauthorized` | signing epoch precedes the key's validity |
| `KeyExpiredAtSigning` | `Unauthorized` | signing epoch is after the window |
| `KeySetRootMismatch` | `Unauthorized` | attestation names a different key set |
| `InputMismatch` | `Corrupt` | a declared input root is not content in the store |
| `OutputMismatch` | `Corrupt` | the declared output root is not content in the store |

## 5. Cross-language verification, exactly what ran

`scripts/producer_auth_court.sh` produces one receipt with 31 checks:

| Producer | Rust verifies | Go verifies | clean-room Python verifies |
|---|---|---|---|
| Rust (`examples/producer_attest sign`) | yes | yes | yes |
| Go (`WeftFS-Go/cmd/producer-attest-sign`) | yes | (n/a) | yes |
| Tampered signature | refuses | refuses | refuses |
| Rotated key, `current` mode | refuses | refuses | refuses |
| Rotated key, `historical` mode | accepts, `currently_trusted=false` | — | — |
| Revoked key, both modes | refuses | refuses | refuses |

The Go verifier re-decodes the same canonical WCB and recomputes the key-set
root independently. The Python verifier uses `verifier/wcb.py`,
`verifier/blake3.py`, and `verifier/ed25519.py`: its own reader, its own
BLAKE3, and its own Ed25519 (RFC 8032) — no Rust or Go import.

## 6. Store cross-check

`verify_attestation_against_store(store, snapshot, bytes, key_set, mode)`
verifies the signature first, then checks every declared input root and the
output root against the content reachable at the pinned snapshot: object
content roots, plus every checkpoint manifest, tensor and shard root. A missing
root is `InputMismatch`/`OutputMismatch`. The check costs one object enumeration
plus one manifest read per checkpoint; it is a verification path, not a hot
path. It does not attempt to prove that the output was *derived* from the
inputs.

## 7. What is not claimed

* No transparency log, no gossip, no cross-store key-set distribution. The key
  set is a file the verifier is given; trusting it is the verifier's decision.
* Epochs and `issued_at_ns` are declared integers; no clock-skew defence and no
  external ordering proof.
* No hardware attestation, TEE, or runtime measurement.
* The private key is a caller-supplied 32-byte seed file; the example refuses to
  invent key material when no seed is supplied.
* Revoking a key does not retract records already accepted by a consumer; it
  only makes future verification fail closed.
