# R12 rebase scale court

Receipt: `bench/receipts/rebase-scale-20261106T130054Z.json`. Generated 2026-11-06T13:00:54Z. One host, warm page cache, cached public Qwen2.5-0.5B weights. Nothing here is a fleet or media claim.

## Calibration divergence (host vs baked measured)

| class | host ns/MiB | baked ns/MiB | 1 MiB delta |
|---|---:|---:|---:|
| raw | 318866 | 307019 | +5.376% |
| zstd | 799184 | 779888 | +4.375% |
| float-tensor | 4776391 | 4683726 | +1.962% |
| tensor-sharded | 8863537 | 9123307 | -2.541% |
| tensor-tiled | 3549201 | 3366772 | +4.978% |
| weight-delta | 5577239 | 5293987 | +5.707% |
| sparse-delta | 5577239 | 5293987 | +5.707% |
| semantic-delta | 7005272 | 6458523 | +8.720% |
| kv-cache | 1048576 | 1048576 | +0.000% |
| activation | 1048576 | 1048576 | +0.000% |

## Workload and selection

- threshold: 200000000 ns
- hot: 16 tensors x 32 reads
- warm: 16 tensors x 2 reads
- rebaseable deltas not read: 41
- dry run: 16 candidates, 0 skipped, 291 scanned
- applied: 16; second apply: 0
- predicted: 16; mismatches: 0

## Per-tensor economics

| tensor | seg | reads | old ns/read | new ns/read | old B | new B | policy BE reads | storage BE reads | selected | before ms | after ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|:---:|---:|---:|
| `model.embed_tokens.weight` | hot | 32 | 2688453967 | 1240237623 | 153245489 | 180547519 | 1 | 1 | yes | 3072.953 | 1305.284 |
| `model.layers.0.mlp.down_proj.weight` | hot | 32 | 86133777 | 39721598 | 4519095 | 5798340 | 3 | 1 | yes | 94.623 | 40.246 |
| `model.layers.0.mlp.gate_proj.weight` | hot | 32 | 86133777 | 39721598 | 4564502 | 5802340 | 3 | 1 | yes | 94.893 | 39.406 |
| `model.layers.0.mlp.up_proj.weight` | hot | 32 | 86133777 | 39721598 | 4544048 | 5781631 | 3 | 1 | yes | 94.734 | 39.122 |
| `model.layers.1.mlp.down_proj.weight` | hot | 32 | 86133777 | 39721598 | 4644714 | 5787655 | 3 | 1 | yes | 95.168 | 39.665 |
| `model.layers.1.mlp.gate_proj.weight` | hot | 32 | 86133777 | 39721598 | 4611663 | 5793457 | 3 | 1 | yes | 95.735 | 39.874 |
| `model.layers.1.mlp.up_proj.weight` | hot | 32 | 86133777 | 39721598 | 4668718 | 5779383 | 3 | 1 | yes | 95.388 | 39.875 |
| `model.layers.10.mlp.down_proj.weight` | hot | 32 | 86133777 | 39721598 | 4800841 | 5832466 | 3 | 1 | yes | 95.853 | 40.397 |
| `model.layers.10.mlp.gate_proj.weight` | hot | 32 | 86133777 | 39721598 | 4828071 | 5819053 | 3 | 1 | yes | 95.289 | 39.444 |
| `model.layers.10.mlp.up_proj.weight` | hot | 32 | 86133777 | 39721598 | 4786331 | 5802438 | 3 | 1 | yes | 95.09 | 39.481 |
| `model.layers.11.mlp.down_proj.weight` | hot | 32 | 86133777 | 39721598 | 4807570 | 5831858 | 3 | 1 | yes | 95.891 | 40.05 |
| `model.layers.11.mlp.gate_proj.weight` | hot | 32 | 86133777 | 39721598 | 4839511 | 5811899 | 3 | 1 | yes | 96.176 | 39.602 |
| `model.layers.11.mlp.up_proj.weight` | hot | 32 | 86133777 | 39721598 | 4777926 | 5803482 | 3 | 1 | yes | 95.795 | 39.596 |
| `model.layers.12.mlp.down_proj.weight` | hot | 32 | 86133777 | 39721598 | 4797509 | 5827520 | 3 | 1 | yes | 95.9 | 39.635 |
| `model.layers.12.mlp.gate_proj.weight` | hot | 32 | 86133777 | 39721598 | 4845778 | 5815799 | 3 | 1 | yes | 94.327 | 39.655 |
| `model.layers.12.mlp.up_proj.weight` | hot | 32 | 86133777 | 39721598 | 4777486 | 5799772 | 3 | 1 | yes | 93.722 | 39.48 |
| `model.layers.13.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4781011 | 5827409 | 3 | 1 | no | 95.882 | 94.137 |
| `model.layers.13.mlp.gate_proj.weight` | warm | 2 | 86133777 | 39721598 | 4847579 | 5817948 | 3 | 1 | no | 95.021 | 94.857 |
| `model.layers.13.mlp.up_proj.weight` | warm | 2 | 86133777 | 39721598 | 4763404 | 5800177 | 3 | 1 | no | 94.147 | 94.566 |
| `model.layers.14.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4769809 | 5826744 | 3 | 1 | no | 94.05 | 95.077 |
| `model.layers.14.mlp.gate_proj.weight` | warm | 2 | 86133777 | 39721598 | 4838904 | 5815539 | 3 | 1 | no | 95.703 | 94.417 |
| `model.layers.14.mlp.up_proj.weight` | warm | 2 | 86133777 | 39721598 | 4771607 | 5798878 | 3 | 1 | no | 95.39 | 93.703 |
| `model.layers.15.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4747998 | 5835315 | 3 | 1 | no | 95.73 | 94.954 |
| `model.layers.15.mlp.gate_proj.weight` | warm | 2 | 86133777 | 39721598 | 4827556 | 5819480 | 3 | 1 | no | 95.626 | 94.738 |
| `model.layers.15.mlp.up_proj.weight` | warm | 2 | 86133777 | 39721598 | 4749900 | 5805202 | 3 | 1 | no | 94.41 | 94.2 |
| `model.layers.16.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4711653 | 5810085 | 3 | 1 | no | 95.514 | 95.079 |
| `model.layers.16.mlp.gate_proj.weight` | warm | 2 | 86133777 | 39721598 | 4800233 | 5822198 | 3 | 1 | no | 95.185 | 93.918 |
| `model.layers.16.mlp.up_proj.weight` | warm | 2 | 86133777 | 39721598 | 4766055 | 5804337 | 3 | 1 | no | 94.81 | 93.746 |
| `model.layers.17.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4702888 | 5799486 | 3 | 1 | no | 95.918 | 94.515 |
| `model.layers.17.mlp.gate_proj.weight` | warm | 2 | 86133777 | 39721598 | 4749482 | 5807302 | 3 | 1 | no | 95.136 | 94.228 |
| `model.layers.17.mlp.up_proj.weight` | warm | 2 | 86133777 | 39721598 | 4752449 | 5790270 | 3 | 1 | no | 95.348 | 93.751 |
| `model.layers.18.mlp.down_proj.weight` | warm | 2 | 86133777 | 39721598 | 4714335 | 5797929 | 3 | 1 | no | 95.627 | 94.536 |

## Totals

- selected decode (sum of medians): 4501.537 ms -> 1900.812 ms
- unselected decode (sum of medians): 1523.497 ms -> 1510.422 ms
- instruct full restore: 10938.771 ms -> 8430.865 ms
- store tree: 1276426933 -> 1553658650 bytes (+277231717)
- data excluding advisory indexes: 1200544321 -> 1468220710 bytes (+267676389); indexes 75882612 -> 85437940 bytes (+9555328)
- representation bytes (instruct tensors): 541151674 -> 584727034 bytes (+43575360); selected tensors 224059252 -> 267634612 bytes (+43575360)
- retained blobs (current-state reachable): 1199335514 -> 1242910330 bytes (+43574816)

## Checks

| check | hard | ok | detail |
|---|:---:|:---:|---|
| corpus_sha256 | yes | yes | both cached checkpoints re-hashed against the LFS SHA-256 in the court script |
| ingest_verify | yes | yes | ingest verify ok=True |
| rebaseable_deltas_present | yes | yes | 73 weight-delta tensors >= 32 |
| dry_apply_match | yes | yes | dry=16 applied=16 |
| reapply_noop | yes | yes | second apply wrote 0 objects |
| verify_full | yes | yes | ok=True problems=0 |
| restore_hash_exact | yes | yes | every instruct tensor BLAKE3 matches its ingest hash |
| restore_logical_roots_exact | yes | yes | every recomputed LogicalTensorRoot matches the written root |
| snapshot_content_unchanged | yes | yes | snapshot before-rebase returns the same logical root and SHA-256 for every read tensor |
| snapshot_refs_unchanged | yes | yes | snapshots before=['before-rebase'] after=['before-rebase'] |
| source_reassembly_exact | yes | yes | reassembled sha256 fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe vs source fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe |
| selected_decode_faster | yes | yes | selected per-tensor medians 4501.537 ms -> 1900.812 ms |
| provenance_visible | yes | yes | weftfs provenance show returns the first applied rebase receipt |
| policy_matches_economics | no | yes | 0 of 32 read tensors differ from the predicted selection |

## Losses and honest limits

- **physical_growth** (model.embed_tokens.weight): rebase added 27302030 bytes for 32 reads and 46342923008 ns of saved decode time
- **physical_growth** (model.layers.0.mlp.down_proj.weight): rebase added 1279245 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.0.mlp.gate_proj.weight): rebase added 1237838 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.0.mlp.up_proj.weight): rebase added 1237583 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.1.mlp.down_proj.weight): rebase added 1142941 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.1.mlp.gate_proj.weight): rebase added 1181794 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.1.mlp.up_proj.weight): rebase added 1110665 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.10.mlp.down_proj.weight): rebase added 1031625 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.10.mlp.gate_proj.weight): rebase added 990982 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.10.mlp.up_proj.weight): rebase added 1016107 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.11.mlp.down_proj.weight): rebase added 1024288 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.11.mlp.gate_proj.weight): rebase added 972388 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.11.mlp.up_proj.weight): rebase added 1025556 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.12.mlp.down_proj.weight): rebase added 1030011 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.12.mlp.gate_proj.weight): rebase added 970021 bytes for 32 reads and 1485189728 ns of saved decode time
- **physical_growth** (model.layers.12.mlp.up_proj.weight): rebase added 1022286 bytes for 32 reads and 1485189728 ns of saved decode time
- **counterfactual_not_rebased** (model.layers.13.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1046398 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.13.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.13.mlp.gate_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 970369 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.13.mlp.gate_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.13.mlp.up_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1036773 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.13.mlp.up_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.14.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1056935 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.14.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.14.mlp.gate_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 976635 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.14.mlp.gate_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.14.mlp.up_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1027271 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.14.mlp.up_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.15.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1087317 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.15.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.15.mlp.gate_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 991924 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.15.mlp.gate_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.15.mlp.up_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1055302 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.15.mlp.up_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.16.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1098432 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.16.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.16.mlp.gate_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1021965 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.16.mlp.gate_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.16.mlp.up_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1038282 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.16.mlp.up_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.17.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1096598 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.17.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.17.mlp.gate_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1057820 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.17.mlp.gate_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.17.mlp.up_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1037821 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.17.mlp.up_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **counterfactual_not_rebased** (model.layers.18.mlp.down_proj.weight): 2 reads did not cross the 200000000 ns policy threshold; rebasing would have added 1083594 bytes to save 92824358 ns
- **policy_more_conservative_than_storage** (model.layers.18.mlp.down_proj.weight): not selected at 2 reads, but the measured bytes/ns trade pays back at 1 read(s); the fixed 200000000 ns policy threshold is the binding constraint
- **store_growth_total** (total): tree grew 277231717 bytes; data excluding advisory indexes grew 267676389 bytes and indexes grew 9555328; current-state representation bytes grew 43575360; the before-rebase snapshot and the commit history retain the old delta frames, so GC cannot reclaim all of the growth

Every check marked hard passed. The economics check is reported, not enforced: a mismatch means the policy's threshold and the measured bytes/ns trade disagree for that tensor.

## What the economics say

The policy selects a tensor when `reads * estimated_decode_ns(current chain) > threshold`
and a standalone representation decodes strictly faster and is no larger than the
logical bytes. With a 200 ms threshold the measured chain cost of an 8.7 MB BF16
MLP tensor is ~87.2 ms/read, so the policy break-even is 3 reads; the embedding's
chain costs ~2.724 s/read, so its break-even is 1 read.

- The 16 hot tensors (32 reads) crossed the threshold and were rebased.
- The 16 warm tensors (2 reads) did not (2 x 87.2 ms = 174.4 ms < 200 ms).
- The policy matched the predicted selection for all 32 read tensors (0 mismatches).

The measured *bytes/ns* trade is more permissive than the fixed-ns policy for these
tensors: adding ~1 MB of mode-9 data saves ~47.8 ms per read, so the extra bytes
pay back after **one** read. The policy is therefore conservative by construction:
its threshold is an operator-chosen decode budget, not a derivative of the
byte/time exchange rate. Rebasing the 2-read set would have been economically
positive under that exchange rate; the court records that as
`policy_more_conservative_than_storage` rather than hiding it. If the policy
should price bytes against decode time automatically, pass `--lambda-ns` to the
planner instead of relying on the threshold alone.

## Method and residual

- Single host, bench court image, 4 vCPUs, 3 GiB, warm page cache; one process at
  a time. Absolute timings are not portable.
- The decode calibration is measured in the same court with the release build and
  passed to every planner call with `--calibration`. It is a host artifact, not a
  constant shipped in the crate.
- The corpus is the cached public Qwen2.5-0.5B / Instruct pair; both files were
  re-hashed against the Hugging Face LFS SHA-256 before the run.
- 290 instruct tensors; 16 hot + 16 warm read; 41 rebaseable deltas were never
  read. The per-tensor table covers only the 32 read tensors.
- `store tree` includes the pre-rebase snapshot's old delta frames, commit
  history, and advisory `indexes/`; `representation bytes` is the current-state
  sum over all 290 instruct tensors. The two differ by design.
- Re-reading the full instruct checkpoint after 16 rebases is only 22.9% faster
  because 274 tensors were untouched and the largest remaining cost is the
  embedding plus the unselected deltas.
- Not measured: physical power loss, multi-host/fleet behavior, cold page cache,
  concurrent readers, or Go-written rebases in this court (Go parity is covered by
  `WeftFS-Go/cmd/weftfs` tests).

Commands:

```
scripts/docker-court.sh bench "bash bench/scripts/rebase_scale_court.sh"
scripts/docker-court.sh bench "cargo build --release --example real_model_bench --bin weftfs"
```

The receipt is `bench/receipts/rebase-scale-20261106T130054Z.json`.
