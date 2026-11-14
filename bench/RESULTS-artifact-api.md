# Artifact API — measured results (T072)

Receipt: `bench/receipts/artifact-api-20261114T224431Z.json` (11/11 checks) and
its `.log`. Retained failing run: `artifact-api-20261114T224412Z.json` (10/11:
the shared-surface check required `verify`, which the adapters only run when
`WEFT_ARTIFACT_VERIFY=1`; the court now sets it).

Environment: python court (3 GiB / 4 CPUs / 1024 pids, `no-new-privileges`,
bounded `/tmp`), release `weftfs` built in the rust court, `torch 2.15.0+cpu`,
`jax 0.12.0.1`, `orbax-checkpoint 0.12.10` from the cached adapter tree. Single
host, synthetic fixtures, no mount, no fleet and no performance claim.

## What was measured

| Check | Result |
|---|---|
| DCP restored tensors equal the source | pass (`embed`, `block.weight`, `bias`) |
| DCP artifact-API bytes equal the legacy service path | pass, sha256 identical per tensor |
| DCP exercises `resolve`, `plan`, `materialize`, `verify` | pass |
| Orbax restored leaves equal the legacy path | pass, sha256 identical per leaf |
| Orbax exercises `resolve`, `plan`, `materialize`, `verify` | pass |
| Agent consumer runs end to end | pass |
| Agent consumer exercises all six verbs | pass (`resolve`, `plan`, `materialize`, `verify`, two `txn`) |
| Agent `verify` ok and transactions committed | pass |
| All three consumers share the same function set | pass |

The checkpoint resolved through the artifact API is **byte-identical** to the
same checkpoint read through the old path for every tensor in both adapters.
The agent consumer demonstrates pin and release of two artifacts, each pair in
one group commit.

## Costs and losses, stated

* The court sets `WEFT_ARTIFACT_VERIFY=1`, which makes the adapters call
  `verify`; that re-reads every tensor, roughly doubling read work. It is off by
  default for that reason. The cheap default path still calls `resolve`, `plan`
  and `materialize`.
* Every tensor restore is now one `weftfs artifact materialize` process (the
  Python binding). On large checkpoints that is more process overhead than the
  persistent checkpoint service. No throughput claim is made; the equivalence
  and the shared surface are the point.
* Region restores and rank-directed restores stay on the checkpoint verbs; the
  streaming artifact path materializes whole tensors.
* Backend request counters are not part of the streaming response, so
  `store_bytes_read`/`store_reads` on the Python buffer are reported as 0 rather
  than invented.
* `pin`/`release` need a writable handle. The DCP and Orbax **readers** therefore
  do not pin; the agent consumer and any training driver do. The Rust tests cover
  pin/release and transactions directly.
