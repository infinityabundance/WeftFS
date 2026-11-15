# The artifact API (T072)

One explicit, snapshot-scoped surface for AI artifacts: **resolve**, **plan**,
**materialize**, **verify**, **pin**, **release**, and **transactions**. It is
implemented once in Rust (`src/artifact_api.rs`, `ContextPlane`) and is the only
path the PyTorch DCP adapter, the Orbax adapter and the agent-state consumer use
for reads and lifetime. `python/weftcodec/artifact_api.py` is a thin binding over
the `weftfs artifact …` verbs; it contains no store logic.

No mount is involved anywhere in this document.

## 1. Signatures

Rust (`weftfs::artifact_api`):

```rust
pub enum SnapshotRef { Id(Hash32), Name(String), Head }
pub struct PinnedView { pub snapshot: Hash32, pub provisional: bool }
pub enum ArtifactRef {
    ContextRoot { name_or_uri: String },
    Checkpoint { name: String, tensor: Option<String> },
    Object { id: Hash32 },
}

impl ContextPlane {
    pub fn open(root, mode) -> Result<ContextPlane>;
    pub fn open_with(root, mode, options) -> Result<ContextPlane>;
    pub fn from_store(store: Store, principal: Principal) -> ContextPlane;

    pub fn observe_snapshot(&self, &SnapshotRef) -> Result<PinnedView>;

    pub fn resolve(&self, PinnedView, &ArtifactRef) -> Result<ArtifactResolution>;
    pub fn plan(&self, PinnedView, &ArtifactRef) -> Result<ArtifactPlan>;
    pub fn materialize(&self, PinnedView, &ArtifactRef, out: &Path) -> Result<Materialized>;
    pub fn materialize_tensor(&self, PinnedView, checkpoint: &str, tensor: &str)
        -> Result<MaterializedTensor>;
    pub fn materialize_tensor_with_topology(&self, PinnedView, checkpoint: &str,
        tensor: &str, &RestoreTopology) -> Result<MaterializedTensor>;
    pub fn verify(&self, PinnedView, &ArtifactRef, out: Option<&Path>)
        -> Result<ArtifactVerification>;

    pub fn pin(&mut self, PinnedView, &ArtifactRef, policy_epoch: u64,
        declared_lifetime_ms: u64) -> Result<PinOutcome>;
    pub fn release(&mut self, PinnedView, &ArtifactRef) -> Result<ReleaseOutcome>;

    pub fn transaction<T>(&mut self,
        f: impl FnOnce(&mut ArtifactTransaction<'_>) -> Result<T>)
        -> Result<(T, Hash32)>;
}
```

CLI (`weftfs artifact …`):

```
resolve      --root NAME|--checkpoint NAME [--tensor T]|--object-id HEX
             [--snapshot S] [--max-results N] [--json]
plan         <selector> [--snapshot S] [--json]
materialize  <selector> --out DIR|- [--snapshot S] [--topology N --axis A] [--json]
verify       <selector> [--snapshot S] [--out DIR] [--json]
pin          <selector> --snapshot S [--policy-epoch N]
             [--declared-lifetime-ms N] [--json]
release      <selector> --snapshot S [--json]
txn          --ops FILE [--snapshot S] [--json]
```

`<selector>` is exactly one of `--root`, `--checkpoint [--tensor]`, or
`--object-id`. `--out -` streams one checkpoint tensor's raw bytes on stdout;
with `--json` the tensor metadata (roots, shard count, plan root) is written to
stderr as one JSON line.

Python:

```python
from weftcodec.artifact_api import ArtifactClient, artifact_ref

client = ArtifactClient(store_root, cli=None, timeout=600.0)
client.resolve(artifact, snapshot=...)
client.plan(artifact, snapshot=...)
client.materialize(artifact, out, snapshot=..., topology=None, axis=0)
client.materialize_tensor(checkpoint, tensor, snapshot=..., topology=None,
                          axis=0, expected_size=None)   # -> ArtifactTensorBuffer
client.verify(artifact, snapshot=..., out=None)
client.pin(artifact, snapshot=..., policy_epoch=0, declared_lifetime_ms=...)
client.release(artifact, snapshot=...)
client.execute(ops, snapshot=...)                       # one group commit
with client.transaction(snapshot=...) as txn:           # commits on exit
    txn.pin(...); txn.release(...)
client.wrapped(service, checkpoint=...)                 # tensor reads via the API
```

## 2. Snapshot scoping (normative)

* Every call names a snapshot or accepts the head. The head is the **only**
  provisional form and is reported as `provisional: true` in every result.
* A named or hashed snapshot absent from this store is
  `SnapshotMissing`. It is never substituted by the head.
* A Context Root pins its own commit snapshot. Naming a different snapshot in
  the request is `InvalidArgument` ("a pinned root is never retargeted").
* `pin` requires a non-provisional snapshot. A head pin would silently move, so
  it is refused.
* A `plan` with an unmet declared dependency is a truthful plan with
  `complete:false` and a `missing` list. `materialize` refuses it with
  `DependencyMismatch` **before** creating any output.

## 3. What each verb returns

`resolve` → `kind`, `snapshot`, `provisional`, `context_root_id`,
`selector_id`, `manifest_root`, `matched_count`, `complete`, `tensor_count`, and
ordered `entries` (name, kind, object/version/content ids, verified
`content_root`, `logical_root`, bytes, dtype, shape, shards, dependencies).
`matched_count` is the authorized pre-limit count.

`plan` → the same entries plus `dependencies` (sorted unique), `missing`, and a
`plan_root = H("weft/artifact/plan/v1", WCB(body))`. A plan is a statement about
what *would* be read; it is not a read.

`materialize` → a directory containing one file per entry plus
`.weft-artifact.json` (schema `weft.artifact.manifest.v1`, `plan_root`,
per-file sha256), published atomically through a sibling temporary directory.
Checkpoint tensors are decoded and verified against their per-shard, whole-tensor
and (when requested) topology roots before a byte is written.

`verify` → `ok`, `store_checked`, `output_checked`, `problems`. Store-side
verification re-reads every planned byte; output verification re-hashes every
produced file against the manifest.

`pin` → `root` (the artifact's identity: ContextRootId, checkpoint manifest
root, or content root), `marker_created`, `retention_binding`, `commit`.
`release` → `marker_removed`, `retention_binding`, `reclaimable_after_gc`,
`commit`.

## 4. Lifetime effect (pin / release)

A pin is two real effects in **one** group commit:

1. an ordinary store file at `/pins/<root-hex>` holding a canonical
   `weft.artifact.pin.v1` record. Being reachable from the head makes it a GC
   root, so the pinned artifact's blobs survive `gc`.
2. when the artifact carries a KV provenance descriptor root
   (`weft.kv.prov.root`), a `durable` `weft.retention.v1` binding at the
   requested policy epoch, through the existing retention module.

Release unlinks the marker and rebinds the descriptor to `ephemeral` at the next
policy epoch. Reclamation itself is a separate, explicit `gc`/`retention gc`
call: `reclaimable_after_gc` says a sweep *may* now reclaim, not that it has.

## 5. Transactions

`ContextPlane::transaction` (CLI `txn`, Python `client.transaction`) runs every
pin/release inside one `Store::group` commit. A failure inside the closure
publishes nothing: the generation, head and in-memory state are exactly as
before. Nested transactions are refused, never flattened. This is the MW1 group
commit, not the MW2 optimistic protocol; MW2 remains its own opt-in surface.

## 6. Three consumers, proven identical

`scripts/artifact_api_court.sh` builds the CLI in the rust court and runs the
proof in the python court:

* **DCP** (`WeftStorageReader`, default `artifact_api=True`) resolves the
  checkpoint through `resolve`, plans through `plan`, restores through
  `materialize --out -`, and verifies through `verify`. Tensor bytes are
  identical to the legacy checkpoint-service path (`artifact_api=False`).
* **Orbax** (`WeftOrbaxHandler`, default `artifact_api=True`) uses the same four
  verbs with the same equivalence check.
* **Agent-state consumer** (`examples/agent_state_consumer.py`) uses all six
  verbs, including two transactions that pin and then release two artifacts.

Receipt `bench/receipts/artifact-api-<stamp>.json` records the per-consumer verb
list, per-tensor sha256 from both paths, and 11 checks. The court enables
`WEFT_ARTIFACT_VERIFY=1`, so `verify` runs in the adapters too; that re-reads
every tensor and is **off by default** because it doubles read cost.

## 7. Error classes

| Class | Cause at this boundary |
|---|---|
| `SnapshotMissing` | the named/hashed snapshot is absent (never substituted) |
| `InvalidArgument` | wrong selector combination; retargeting a pinned root; head pin; topology with a directory |
| `DependencyMismatch` | the plan is incomplete; `materialize` published nothing |
| `NotFound` | authorized absence inside a snapshot that exists |
| `Corrupt` | bytes, roots, plan or output digest disagree |
| `Unauthorized` | the principal may not read an entry (denial precedes bytes) |
| `Conflict` | pinning an artifact already pinned with a different record |
| `ReadOnly` | pin/release/transaction on a read-only handle |
| `Exists` | the output directory is a non-empty existing directory |
| `LimitExceeded` | a configured bound (entries, chunk count, size) |

## 8. Worked example

```sh
weftfs --store /s init
weftfs --store /s create /prompts/task.txt --text 'Summarize.' \
  --attr task=summarize
weftfs --store /s snapshot create jan
weftfs --store /s context create --name 'by:task=summarize' \
  --selector 'select attrs.task == "summarize"' --snapshot jan

weftfs --store /s --json artifact plan --root 'by:task=summarize' --snapshot jan
weftfs --store /s artifact materialize --root 'by:task=summarize' \
  --snapshot jan --out /tmp/materialized
weftfs --store /s --json artifact verify --root 'by:task=summarize' \
  --snapshot jan --out /tmp/materialized
weftfs --store /s --json artifact pin --root 'by:task=summarize' \
  --snapshot jan --policy-epoch 1
weftfs --store /s --json artifact release --root 'by:task=summarize' \
  --snapshot jan
```

```json
{
  "ops": [
    {"op": "pin", "artifact": {"object_id": "…"}, "snapshot": "jan",
     "policy_epoch": 1, "declared_lifetime_ms": 86400000}
  ]
}
```

`weftfs artifact txn --ops ops.json --json` publishes that file as one commit.

## 9. What this API does not do

* **No scheduling.** It produces ordered requirements and reads bytes. It makes
  no placement, prefetch, cache-policy, concurrency or retry decision.
* **No transport.** There is no network protocol here. A remote caller still
  needs its own authenticated transport; this API is in-process/CLI only.
* **No freshness aliasing.** There is no mutable `latest`: a provisional head
  view exists, is labelled, and cannot be pinned.
* **No partial output.** `materialize` either publishes a complete directory or
  nothing.
* **Region reads are not in this surface.** One checkpoint tensor is the
  streaming unit; region and rank-directed restores remain on the existing
  checkpoint verbs.
* **Not a security boundary by itself.** It enforces the store's POSIX/ACL and
  capability checks before bytes, but it does not authenticate a remote
  principal.

## 10. Evidence

* `tests/artifact_api.rs` — 8 Rust tests: snapshot scoping, missing-dependency
  refusal, pin/release GC roots, one-commit transaction and rollback, checkpoint
  tensor and directory exactness, tamper detection, canonical pin record.
* `bench/scripts/artifact_api_proof.py` + `scripts/artifact_api_court.sh` — the
  three-consumer proof and byte-equivalence receipt.
* `bench/RESULTS-artifact-api.md` — the measured facts and every caveat.

## 11. Composition: the Training Artifact (P01)

The `ContextPlane` verbs operate on a selection. The **Training Artifact**
(`spec/WEFT-TRAINING-ARTIFACT.md`, CLI `weft training-artifact …`) is the
canonical composition of a selection's immutable identities: exact tensor
roots, one checkpoint anchor, the Context Root id, the causal closure and
provenance receipts. It is a sibling surface, not a replacement:

* `weft training-artifact build|show|validate|closure` resolve and check the
  composed record at a pinned snapshot;
* `weft training-artifact grant|grants` edit and read the **separate mutable**
  grant record. A grant edit moves the grant epoch and never changes
  `TrainingArtifactRoot`;
* `pin`/`release` remain `artifact` verbs. Retention and quarantine are shared
  with the rest of the store and are likewise not inputs to the artifact root.

`validate` recomputes every member tensor, the checkpoint manifest, the
Context Root, the closure and the declared policy; `--closure-mode minimal`
selects the parent-edge closure. The Go consumer
(`WeftFS-Go/cmd/training-artifact`) performs the same validation independently;
`bench/RESULTS-training-artifact.md` records what it cannot check.
