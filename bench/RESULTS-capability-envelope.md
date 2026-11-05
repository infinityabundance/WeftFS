# D2 authenticated capability envelope — measured results

Receipt: `bench/receipts/capability-envelope-20261105T034858Z.json`
Raw log: `bench/receipts/capability-envelope-20261105T034858Z.log`
Court: `weft/bench-court:1` (`sha256:e80c4385…d85a`), kernel
`7.3.1-1-cachyos`, hard limits `--memory=3g --memory-swap=3g
--pids-limit=1024 --cpus=4`, bounded `/tmp`.
Revisions: WeftFS `d5f9572`, WeftCodec `272c2c0`, WeftCodec-Go `b7400ec`,
WeftFS-Go `963dc62`.

**Label:** synthetic workload, single host, warm caches, Docker bench court.
This is not a fleet, power-loss, or timing-side-channel claim. Envelope
verification is CPU-bound Ed25519 verification; resolve overhead is measured on
one 1,000-object store.

## Envelope verification (1,000 distinct signed envelopes)

| Pass | Samples | Median | p95 | Mean | Max |
|---|---:|---:|---:|---:|---:|
| Cold (first verify of each blob) | 1,000 | 43,110 ns | 43,702 ns | 43,314 ns | 76,122 ns |
| Warm (three further passes) | 3,000 | 43,180 ns | 43,621 ns | 43,550 ns | 177,542 ns |

Cold and warm medians differ by 70 ns (0.16%), so there is no material
first-use penalty once the code is resident. This is the full offline check:
WCB decode, canonical re-encode comparison, Ed25519 verification, expiry, and
revocation-set membership.

## Signed resolve overhead

| Case | Samples | Median | p95 |
|---|---:|---:|---:|
| Unsigned resolve (1,000 objects) | 50 | 89,469,087 ns | 92,246,417 ns |
| Verified-envelope resolve | 50 | 89,941,598 ns | 93,162,746 ns |

Median overhead: **472,511 ns (0.53%)**. Both cases matched 1,000 objects and
reported **1,001 work units** (root + 1,000 objects), so the envelope did not
change the authorized universe or the accounting. The overhead is the extra
scope checks in the resolver, not envelope re-verification: the principal
carries an already-verified envelope, and verification itself is measured
separately above.

## Isolation work-unit equality

| Tenant B population | A work units |
|---:|---:|
| 0 unreadable objects | 7 |
| 40 unreadable objects | 7 |

`work_units_equal = true`. Tenant A's content and envelope are identical in
both stores; only unreadable tenant-B objects are added. This is the benchmark
counterpart of `tests/capability_isolation.rs`, which also checks results,
denial text, exact-id denial, deduplicated shared content, and
revoked/expired fail-closed behaviour.

## Honest limits

- The 1,000-object store is synthetic; no external corpus is used.
- Timings are single-host warm-cache observations. No cache dropping, no RAPL
  energy claim, no timing side-channel claim.
- The signed-resolve comparison reuses one verified envelope. Re-verifying the
  envelope on every operation would add the envelope-verification cost above;
  the design verifies once at the trust boundary and attaches the typed result.
- Cross-language parity is proven by `hack/capability-cross.sh` (identical
  canonical bytes, EnvelopeId, deterministic signature, and five rejections),
  not by this timing harness.
