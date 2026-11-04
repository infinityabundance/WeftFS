#!/usr/bin/env bash
# D2 — Agent long-term memory: prompt/model/tool/dataset/output typed edges,
# task/model/time Context Roots, a snapshot, and a months-later replay with
# exact bytes and lineage.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-agent-memory.sh"
#
# Timestamps are simulated fixture timestamps (`--fixed-timestamp-ns`) and a
# point-in-time bundle export. This is not a real-time aging or retention
# measurement, and the receipt says so.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

demo_init "weftfs.agent-memory.receipt/1" "agent-memory" \
  "Agent long-term memory (D2) with months-later replay"

TS_JAN=1767225600000000000   # 2026-01-01T00:00:00Z
TS_SEP=1788220800000000000   # 2026-09-01T00:00:00Z (243 simulated days later)

S="$STORES/agent-memory-$RUN_ID"
BUNDLE_DIR="$ROOT/bench/bundles"
BUNDLE="$BUNDLE_DIR/agent-memory-$RUN_ID.tar.zst"
REPLAY_OUT="$SCRATCH/agent-memory-replay-$RUN_ID"
mkdir -p "$S" "$BUNDLE_DIR"
rm -rf "$S"; mkdir -p "$S"

PROMPT=$'Summarize the retained experiment log. Task: summarize.\n'
MODEL=$'llama-4 ver_19 weights marker\n'
TOOL=$'retriever v3 tool marker\n'
DATASET=$'eval-v1 dataset marker\n'
OUT1=$'agent output run-001: exact retained bytes\n'
OUT2=$'agent output run-002: later bytes\n'
PROMPT_SHA="$(printf '%s' "$PROMPT" | sha256sum | awk '{print $1}')"
OUT1_SHA="$(printf '%s' "$OUT1" | sha256sum | awk '{print $1}')"
OUT2_SHA="$(printf '%s' "$OUT2" | sha256sum | awk '{print $1}')"

note "Timestamps are simulated fixture nanoseconds; no real-time aging or retention claim is made"
note "The bundle is a point-in-time export of the January store; replay runs after the September writes"

step_begin "january-store" "Create prompt/model/tool/dataset/output and typed lineage edges"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" init
for dir in prompts models tools datasets outputs; do
  run_ok "mkdir-$dir" "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" mkdir "/$dir" --seed "dir-$dir"
done
run_ok create-prompt "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" create /prompts/task.txt \
  --text "$PROMPT" --seed "prompt-jan" --attr "task=summarize" --attr "model=llama-4" --attr "sha256=$PROMPT_SHA"
run_ok create-model "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" create /models/llama-4.bin \
  --text "$MODEL" --seed "model-jan" --attr "task=summarize" --attr "model=llama-4"
run_ok create-tool "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" create /tools/retriever.bin \
  --text "$TOOL" --seed "tool-jan" --attr "task=summarize"
run_ok create-dataset "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" create /datasets/eval-v1.bin \
  --text "$DATASET" --seed "dataset-jan" --attr "task=summarize"
run_ok create-output "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" create /outputs/run-001.bin \
  --text "$OUT1" --seed "output-jan" --attr "task=summarize" --attr "model=llama-4" --attr "sha256=$OUT1_SHA"

run_ok stat-prompt "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" stat /prompts/task.txt
PROMPT_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" stat /models/llama-4.bin
MODEL_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-tool "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" stat /tools/retriever.bin
TOOL_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-dataset "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" stat /datasets/eval-v1.bin
DATASET_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-output "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" stat /outputs/run-001.bin
OUT1_ID="$(jsonq "$RUN_OUT" object_id)"

run_ok rel-generated "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" relation add \
  --subject "$OUT1_ID" --predicate generated_by --object "$MODEL_ID"
run_ok rel-prompted "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" relation add \
  --subject "$OUT1_ID" --predicate prompted_by --object "$PROMPT_ID"
run_ok rel-grounded "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" relation add \
  --subject "$OUT1_ID" --predicate grounded_in --object "$DATASET_ID"
run_ok rel-tool "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" relation add \
  --subject "$OUT1_ID" --predicate used_tool --object "$TOOL_ID"
run_ok rel-derived "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" relation add \
  --subject "$OUT1_ID" --predicate derived_from --object "$PROMPT_ID"
run_ok rel-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" relation list
assert_eq "five typed lineage edges" "5" "$(jsonq "$RUN_OUT" relations#len)"
save_out relations-jan "$RUN_OUT"

run_ok snapshot-jan "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_JAN" snapshot create jan-2026
step_end pass "output=$OUT1_ID model=$MODEL_ID prompt=$PROMPT_ID tool=$TOOL_ID dataset=$DATASET_ID"

step_begin "contexts" "Context Roots by task, model, time"
run_ok ctx-task "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" context create \
  --name "by:task=summarize" --selector 'select attrs.task == "summarize"' --snapshot jan-2026
TASK_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok ctx-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" context create \
  --name "by:model=llama-4" --selector 'select attrs.model == "llama-4"' --snapshot jan-2026
MODEL_ROOT_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok ctx-time "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" context create \
  --name "at:2026-01" --selector 'select true' --snapshot jan-2026
TIME_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok ctx-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" context list
assert_eq "three Context Roots stored" "3" "$(jsonq "$RUN_OUT" contexts#len)"
step_end pass "task=$TASK_ID model=$MODEL_ROOT_ID time=$TIME_ID"

step_begin "export" "Export the January point-in-time replay bundle"
rm -f "$BUNDLE"
run_ok export "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_JAN" export-bundle \
  --out "$BUNDLE" --source-revision "WeftFS=${WEFT_REPO_WEFTFS:-unknown}"
BUNDLE_SHA="$(jsonq "$RUN_OUT" sha256)"
assert_eq "bundle exists" "true" "$([ -f "$BUNDLE" ] && echo true || echo false)"
assert_eq "bundle sha256 matches file" "$BUNDLE_SHA" "$(sha256_file "$BUNDLE")"
artifact_local "agent-memory-bundle" "$BUNDLE"
step_end pass "bundle=$BUNDLE_SHA"

step_begin "september-store" "Write a later run; old snapshot and Context Roots stay pinned"
run_ok create-output-2 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_SEP" create /outputs/run-002.bin \
  --text "$OUT2" --seed "output-sep" --attr "task=summarize" --attr "model=llama-4" --attr "sha256=$OUT2_SHA"
run_ok stat-output-2 "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" stat /outputs/run-002.bin
OUT2_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok snapshot-sep "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS_SEP" snapshot create sep-2026
run_ok ctx-task-sep "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" context create \
  --name "by:task=summarize@sep" --selector 'select attrs.task == "summarize"' --snapshot sep-2026
SEP_TASK_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok ctx-jan-show "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" context show "by:task=summarize"
assert_eq "January Context Root identity did not change" "$TASK_ID" "$(jsonq "$RUN_OUT" context_root_id)"
run_ok verify-sep "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" verify --full
assert_eq "store verifies after the later writes" "true" "$(jsonq "$RUN_OUT" ok)"
step_end pass "sep-output=$OUT2_ID sep-context=$SEP_TASK_ID"

step_begin "months-later-mounts" "After simulated months, old mounts and old time root are exact"
M1="$MOUNT_ROOT/agent-$RUN_ID-task"
M2="$MOUNT_ROOT/agent-$RUN_ID-model"
M3="$MOUNT_ROOT/agent-$RUN_ID-time"
M4="$MOUNT_ROOT/agent-$RUN_ID-sep"
start_mount rust "$S" "$M1" --context "by:task=summarize"
start_mount go "$S" "$M2" --context "by:model=llama-4"
start_mount rust "$S" "$M3" --at "$TS_JAN"
start_mount go "$S" "$M4" --at "$TS_SEP"
run_ok stat-mount-old "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" stat /outputs/run-001.bin
OUT1_NAME="run-001.bin--${OUT1_ID:0:8}"
assert_sha256 "task mount retains run-001" "$M1/$OUT1_NAME" "$OUT1_SHA"
assert_sha256 "model mount retains run-001" "$M2/$OUT1_NAME" "$OUT1_SHA"
assert_sha256 "time mount retains run-001" "$M3/outputs/run-001.bin" "$OUT1_SHA"
assert_not_exists "January time root does not expose later run" "$M3/outputs/run-002.bin"
assert_not_exists "task context does not expose later run" "$M1/run-002.bin--${OUT2_ID:0:8}"
assert_sha256 "September time root exposes later run" "$M4/outputs/run-002.bin" "$OUT2_SHA"
assert_sha256 "September time root keeps old run exact" "$M4/outputs/run-001.bin" "$OUT1_SHA"
assert_eq "task context lists all five retained objects" "5" "$(ls -1 "$M1" | grep -v '^\.weft$' | wc -l | tr -d ' ')"
step_end pass "run-001 exact in task/model/time mounts; run-002 absent from January"

step_begin "replay-after-months" "Replay the January bundle later and verify exact bytes + lineage"
run_ok replay bash "$ROOT/scripts/replay-bundle.sh" --bundle "$BUNDLE" --out "$REPLAY_OUT"
assert_contains "replay reports ok" "$(cat "$RUN_OUT")" "replay: ok"
cp "$REPLAY_OUT/go-verify.json" "$STEP_DIR/go-verify.json"
assert_eq "Go verify --full OK" "true" "$(jsonq "$STEP_DIR/go-verify.json" OK)"
cp "$REPLAY_OUT/go-manifest-verification.json" "$STEP_DIR/go-manifest-verification.json"
assert_eq "Go manifest verification ok" "true" "$(jsonq "$STEP_DIR/go-manifest-verification.json" ok)"
assert_eq "replay checked three contexts" "3" "$(jsonq "$STEP_DIR/go-manifest-verification.json" contexts_checked)"
assert_sha256 "replayed output bytes are exact" "$REPLAY_OUT/materialized/by:task=summarize/outputs/run-001.bin" "$OUT1_SHA"
run_ok replay-rel "$GOFS" --store "$REPLAY_OUT/store" --json relation list --subject "$OUT1_ID"
assert_eq "replayed store retains five lineage edges" "5" "$(jsonq "$RUN_OUT" '#len')"
save_out relations-replay "$RUN_OUT"
run_ok replay-ctx-task "$GOFS" --store "$REPLAY_OUT/store" --json context show "by:task=summarize"
assert_eq "replayed task Context Root identity is exact" "$TASK_ID" "$(jsonq "$RUN_OUT" context_root_id)"
run_ok replay-ctx-model "$GOFS" --store "$REPLAY_OUT/store" --json context show "by:model=llama-4"
assert_eq "replayed model Context Root identity is exact" "$MODEL_ROOT_ID" "$(jsonq "$RUN_OUT" context_root_id)"
run_ok replay-ctx-time "$GOFS" --store "$REPLAY_OUT/store" --json context show "at:2026-01"
assert_eq "replayed time Context Root identity is exact" "$TIME_ID" "$(jsonq "$RUN_OUT" context_root_id)"
step_end pass "exact run-001 sha; 3 Context Roots; 5 lineage edges"

step_begin "lineage-check" "Every typed edge survives with the same subject and object"
run_ok lineage "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS_SEP" relation list --subject "$OUT1_ID"
REL_COUNT="$(jsonq "$RUN_OUT" relations#len)"
assert_eq "five edges still name run-001" "5" "$REL_COUNT"
for predicate in generated_by prompted_by grounded_in used_tool derived_from; do
  found=0
  i=0
  while [ "$i" -lt "$REL_COUNT" ]; do
    [ "$(jsonq "$RUN_OUT" "relations.$i.predicate")" = "$predicate" ] && found=1
    i=$((i + 1))
  done
  assert_eq "lineage predicate $predicate present" "1" "$found"
done
step_end pass "all five predicates present after the later writes"
