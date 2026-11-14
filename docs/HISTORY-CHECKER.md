# Independent transactional history checker (T026)

Status: **implemented and measured** (Rust emission; independent stdlib Python
checker). Court: `scripts/history_check_court.sh`, receipt
`bench/receipts/history-check-20261114T101358Z.json` (revision `fd56f32`).
Checker: `verifier/history_check.py`. Emission: `src/history.rs`.

## The history format

`weft.history.v1` is append-only JSON Lines, one record per line, emitted only
when `WEFT_HISTORY` (or `Options::history_path`) is set. It is an audit artifact:
turning it on never changes a commit, a root, or a verify result.

Two event families, each with an invocation and a response record:

| event | fields |
|---|---|
| `tx-invoke` | `tx`, `base_generation`, `ops[]` (operation, key(s), values, precondition), `preconditions[]` |
| `tx-response` | `tx`, `outcome` (`committed` / `idempotent` / `rejected`), `generation`, `commit`, `attempts`, or `class` and `error` for a rejection |
| `read-invoke` | `kind`, `key`, `read_generation` |
| `read-response` | `kind`, `key`, `value` or `absent`, `read_generation` |

The keys are the logical keys a transaction touched (object id, entry id,
relation id, tip), so the checker works on the same namespace the transaction
table uses. The same format is emitted for MW2 and MW3; an MW3 record carries
its domain.

## What the checker decides

It is a separate program. It imports no WeftFS code, calls no WeftFS binary, and
reads no store state — only the JSON Lines.

1. **Generation uniqueness** per domain: two different transactions claiming one
   generation with different ops is a violation.
2. **Register linearizability per key**: the successful writes define the value
   at each generation; every read must return the value of the latest write with
   generation `<=` the generation the read observed. Stale, phantom, and
   read-before-any-write are violations.
3. **Per-domain multi-key atomicity**: every key of a committed transaction
   carries its generation, so a read at generation `g` cannot see half a
   transaction.
4. **Conflict justification**: a rejected transaction must name a conflict that
   the history explains (a key changed in `(base_generation,
   observed_generation]`, or a create found the key taken at the base).
5. **Handle monotonicity**: one handle's observed generation never decreases.

The store's publication order *is* the linearization order (one head CAS in
MW2, one head per domain in MW3), so "latest write with generation <= g" is the
whole register check rather than a search over an ordering.

## What it deliberately does not check

Global serializability across MW3 conflict domains, cross-domain snapshot
isolation, and a single global linearization point. Cross-domain disagreement is
expected by the contract (§15.4) and is never flagged: reads and writes are
grouped by domain and never compared across domains.

## Histories run and violation proof

The court generates four histories: MW2 disjoint writers with a three-key read,
MW2 conflicting writers on one name, MW2 process faults (killed at prepare and
after the head, each followed by an idempotent retry), and MW3 with two domains
and per-domain reads. Every history is accepted, and the receipt records its
transaction/record/read counts.

Six deliberately violating histories are supplied and **must be rejected**:
`duplicate_generation`, `partial_transaction`, `phantom_value`,
`read_before_any_write`, `stale_read`, `unexplained_conflict`. Each is rejected
with the expected class. Two valid controls (`mw2_ok`, `cross_domain_allowed`)
are accepted, so the checker is not a blanket rejecter.

Every verdict in the receipt is reproducible from the committed history files
(`bench/receipts/history-check-<stamp>-h1..h4.jsonl`) and the committed
fixtures (`tests/history/{valid,violating}/*.jsonl`):

```
python3 verifier/history_check.py <history.jsonl> --expect accept
python3 verifier/history_check.py <violating.jsonl> --expect reject --expect-class stale_read
```

## Not claimed

No Go emission path, no global linearizability, no cross-domain snapshot
isolation, and no claim that the store's own logic was reused — the point is
that it was not.
