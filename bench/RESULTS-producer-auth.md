# Producer attestation — measured results (T074)

Receipt: `bench/receipts/producer-auth-<stamp>.json` + `.log`, written by
`scripts/producer_auth_court.sh`. The court runs Rust, Go and Python verifiers
over the same canonical records; every container keeps the default limits
(3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded tmpfs).

Current receipt: `producer-auth-20261115T010000Z.json`.

<!-- BEGIN GENERATED:producer-auth-verdict (scripts/perf_numbers.py) -->
<!-- END GENERATED:producer-auth-verdict -->

Retained, not cited: `producer-auth-20261114T225327Z.json`, measured from a tree
ahead of the revision it recorded; its checks and its conclusions are the same,
and the receipt above supersedes it.

## What is proven

* Rust signs; Rust, Go and the clean-room Python verifier all accept the same
  bytes and agree on `signer`, `key_set_root`, `key_status`, and
  `currently_trusted`.
* Go signs; Rust and Python accept.
* A one-byte change inside the signature is refused by all three.
* A key rotated before verification is refused in `current` mode by all three;
  Rust `historical` mode accepts it while reporting `currently_trusted=false`
  and `key_status=retired`.
* A revoked key is refused in **both** modes by all three.
* The store cross-check accepts a declaration whose input and output roots are
  content at the head, and refuses a declaration naming an input root that the
  store does not hold.

## What it costs

* The store cross-check enumerates every object's manifest (`store.manifest_of`
  per file) and every checkpoint manifest at the snapshot. It is O(objects +
  checkpoints) and is not on a hot path.
* Verification is one canonical decode + one re-encode + one Ed25519 verify per
  record; the adversarial court performs 31 such operations in a few seconds
  including container startup.
* No performance number is claimed beyond "this completes"; rule 12 applies to
  every number published in tracked docs, so none is written here.

## Losses and limits

* The signature proves a declaration, not execution. This is stated in the
  document, the CLI output, and every verifier's `limits` field.
* Epochs are operator-declared integers. There is no transparency log, no clock
  skew defence, and no cross-store key-set distribution; the verifier is handed
  the key set.
* `determinism: exact` is a declared class. Nothing measures determinism.
* Historical mode is an audit convenience and is **not** the default; a consumer
  that wants current trust must use `current`.
* The clean-room verifier only verifies. It does not sign, because the verifier
  deliberately contains no signing code.
