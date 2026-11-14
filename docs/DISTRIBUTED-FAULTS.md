# Distributed write/read failure court (T025)

Status: **implemented and measured** (Rust). Court:
`scripts/distributed_faults_court.sh`, receipt
`bench/receipts/distributed-faults-20261114T101423Z.json` (revision `fd56f32`).
Proxy: `scripts/fault_proxy.py`. Scenarios: `scripts/distributed_faults_inner.sh`.

## What this is, exactly

* **one host**;
* **one backend instance** — the pinned MinIO release binary in a sidecar
  container under the same hard limits as every other court;
* **multiple independent client processes** (the `mw2_writer` driver), each a
  separate process behind the same proxy;
* a **userspace fault-injecting HTTP proxy** written for this court.

It is **not** a partition of physical nodes, not a storage-server crash, not
media failure, not power loss, not a clock-skew experiment, and not a
multi-host ordering study. Those are non-claims, repeated in the receipt.

## The proxy

`fault_proxy.py` listens on loopback, forwards to MinIO, and applies an ordered
rule list per request (`method`, `path_regex`, `occurrence`, action):

| action | effect |
|---|---|
| `forward` | no fault |
| `delay_request:MS` | hold the request before forwarding |
| `delay_response:MS` | forward, hold the response afterwards |
| `drop` | close the client connection without forwarding |
| `duplicate` | send the request upstream twice, return one response |
| `after_apply_drop` | forward, **wait for the upstream status**, then destroy the response |
| `after_apply_truncate` | forward, apply, then return a truncated body |
| `abort_after_apply` | forward, apply, then close the connection |

`after_apply_*` is the genuine **ambiguous success**: the server applied the
effect and the client never learns it. The schedule for every scenario is
recorded verbatim in the receipt.

## Scenarios and schedules

| id | scenario | fault schedule |
|---|---|---|
| S1 | head acknowledged-then-lost | `after_apply_drop` on the first `PUT /superblock$` |
| S2 | duplicated request (idempotency) | `duplicate` the first `PUT /objects/` |
| S3 | reordered concurrent requests | `delay_request:400` on the second `PUT /superblock$` |
| S4 | client killed between prepare and commit | `delay_request:300` on the first `PUT /superblock$`, then `SIGKILL` |
| S5 | stale reader against a newer head | no fault; generation pinning |
| S6 | conditional write applied, acknowledgement dropped | `after_apply_drop` on the first `PUT /commits/` |

## How an outcome is decided

Every ambiguous writer outcome is resolved with the **shipped**
`transaction_outcome` / `lookup` path, not with a new oracle. The court asserts
the resolution is `Unknown` before the retry (meaning "neither success nor
failure proven"), then retries the same logical operation and proves the store
ends with one commit, one generation, and exact bytes. A duplicated object PUT
must be idempotent; a conditional head write that lost a race must not be
retried into a second commit.

## Machine-checked verdicts

Seven verdicts are computed from the resulting store, not from the driver's own
claims:

* no acknowledged lost update;
* no partial checkpoint (old-or-new whole state, never a mixture);
* no blind overwrite;
* ambiguous success resolved;
* duplicated request idempotent;
* kill between prepare and commit resolved;
* stale reader snapshot isolated.

All seven are `true` in the cited receipt. The earlier failing receipt from this
subphase is retained beside it and is **not** cited as a pass.

## Not checked

Multi-host clusters, consensus, quorum, clock skew, physical network partitions,
server-side crashes, Go parity, and any performance claim. The proxy faults are
in userspace above the TCP connection; they do not model packet loss inside the
kernel or a partition of physical nodes.
