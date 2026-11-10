# R12 increment 3 — attested loads

A signed `weft.load-attestation.v1` record for one replica's load, with an
independent re-verification pass. Design and schema: `docs/LOAD-ATTESTATION.md`.

## What was measured

`SmolLM-135M-Instruct` (269,060,552 B, SHA-256 `c1464c9e…32f2d`, 272 BF16
tensors) written as one `weft.checkpoint.v1` checkpoint, **8-way sharded on
axis 0**, fast raw, so every tensor binds eight shard groups. The court runs the
`load` and `verify` commands as **separate processes**, and the Go verifier in a
third.

Court: rust court, `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4
--cap-drop=ALL --no-new-privileges`. Receipt:
`bench/receipts/attested-loads-20261110T232937Z.json` (+ `.log`).

The Ed25519 seed is a fixed test key (bytes 0..31). It is never a production key.

## Result

| phase | wall | loaded / verified | replayed bytes | receipt bytes | RSS |
|---|---:|---:|---:|---:|---:|
| `load_all` (272 tensors) | 2,133.2 ms | 272 loaded, 0 unverified | — | 456,430 | 134,528 KiB |
| `verify_all` | 2,241.0 ms | 272 verified | 269,030,016 | 456,430 | 135,544 KiB |
| `load_partial` (20 tensors) | 126.3 ms | 20 loaded, **252 unverified** | — | 99,504 | 26,440 KiB |
| `verify_partial` | 207.2 ms | 20 verified, **252 unverified listed** | 23,040 | 99,504 | 27,164 KiB |
| `load_region` (50 rows of one tensor) | 12.4 ms | 1 loaded (whole root **not** claimed), 271 unverified | — | 71,564 | 26,708 KiB |
| `verify_region` | 92.1 ms | 1 verified, 271 unverified listed | 57,600 | 71,564 | 26,676 KiB |

`load_all` read 269,030,016 B at 126.1 MB/s, recorded **272 ranges** and
**2,176 groups** (272 tensors × 8 shards), and verified **4,352 units** (the
record counts shards read and transport units touched separately, so each shard
here contributes two). It is signed (`signed: true`) and its receipt root is
`c5e7d281…c99e`.

### Cost

* **Verification µs/MiB: 8,734.4** (8.73 ms/MiB, ≈ 120 MB/s) over the full
  model. It costs **1.05×** the load itself (2,241.0 ms vs 2,133.2 ms) because
  it re-reads and re-hashes every recorded byte through the shipped restore
  path.
* **µs per recorded range: 8,230.2** (272 ranges). A range is one whole tensor
  here; the region case is one range of 57,600 B and costs 92.1 ms, which is
  almost entirely fixed overhead (store open, manifest read, signature), so the
  per-MiB figure for small loads is not meaningful and is not quoted as one.
* **Receipt size: 456,430 B for 272 tensors ≈ 1.68 KB per tensor**, dominated by
  the per-group entries (2,176 groups × 6 fields). A partial load shrinks it to
  99,504 B (20 tensors + 252 unverified entries), and the region record is
  71,564 B because it still lists all 272 tensors.

### Tamper behaviour

* **Record tampered** (last byte flipped): `verify_tampered` exits **1** with
  `NonCanonical: unknown WCB tag 0xff` — the record no longer decodes as
  canonical WCB, so it is rejected before any key is checked. The Rust test
  `tampering_with_a_recorded_range_breaks_the_signature` is the precise case:
  it changes one loaded range's digest, keeps the record canonical, and requires
  the **Ed25519 signature** check to fail.
* **Store tampered**: `tampering_with_the_store_fails_the_replay` flips one byte
  in the manifest blob of a recorded group and requires `verify_against_store`
  to fail with a `Corrupt`/`Malformed` read — the store's own group hashing
  catches it before the receipt digest comparison.
* `tests/attested_load.rs` **9/9 pass**, also covering: a region load does not
  claim a whole-tensor root, an unloaded tensor is reported with a reason, the
  trust set is enforced, wrong totals and an unverified entry without a reason
  are rejected on decode, and the recorded digests reproduce from the store.

### Independent Go verification

`WeftFS-Go/cmd/attest-verify` (independent of the Rust code) checks canonical
WCB, the exact key sets, sorted tensor entries, loaded/unverified consistency,
and the Ed25519 signature over the canonical record without its `signature` key:

```
signed=true
tensors=272 loaded=272 unverified=0 ranges=272 groups=2176
byte_replay=not_performed (this repository has no weft.checkpoint.v1 reader)
```

On the tampered record it exits 1 (`decode: NonCanonical: unknown WCB tag
0xff`) and the court records the rejection as a pass (`go_tamper exit=0` means
"the verifier rejected it"). So the signature format really is shared, and Go
verifies a record Rust produced. **Byte replay stays Rust-only.**

## Every loss

1. **Lazy verification cannot prove bytes that were never read.** The region
   record says `full_root_verified: false` and lists the other 271 tensors as
   `unverified`; it proves only the one group covering the 50 rows.
2. **The signature proves origin at the signer, not the truth of the
   measurement.** A signer can sign a false record; only the store re-read tests
   the measurement, and only for the recorded ranges.
3. **Verification costs a second full pass** (1.05× the load) and a receipt of
   ~1.7 KB per tensor. For a 7B checkpoint (339 tensors, ~2,700 groups) the
   record would be several MB and the replay would cost another full decode.
4. **Only row-aligned regions of rank-2 tensors** are supported; a general
   sub-block is refused rather than approximated.
5. **The Go verifier does not re-read bytes** (no checkpoint reader in that
   repository). It is a signature/record verifier, and its output says so.
6. **The court's tamper test is coarse** (a flipped last byte). The specific
   single-range tamper is the Rust unit test, not a court scenario.
7. One host, local store, no serving integration, no key management beyond a
   fixed test seed.

## Reproduce

```
bash bench/scripts/attested_load_court.sh
```
