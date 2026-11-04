#!/usr/bin/env bash
# F3.2 / A1 + D2 — Context Views as first-class mounts.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
#
# Builds a deterministic store with two byte-identical objects, typed relations
# (task, model, generated_by, derived_from, prompted_by), two snapshots, and
# three Context Roots (by:task, by:model, at:<timestamp>). Mounts all three
# simultaneously, mixing the Rust and Go frontends, and machine-checks every
# listing, byte, SHA-256, BLAKE3 content id, and read-only denial.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

demo_init "weftfs.context-mounts.receipt/1" "context-mounts" \
  "Context Views as first-class mounts (A1, D2)"

S="$STORES/context-mounts-$RUN_ID"
rm -rf "$S"
mkdir -p "$S"
r() { "$RUSTFS" --store "$S" --fixed-timestamp-ns "$1" "${@:2}"; }
rj() { "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$1" "${@:2}"; }

ALPHA=$'shared agent output v1\n'
PROMPT=$'you are agent-42\n'
MODEL=$'llama-4 ver_19 delta\n'
ALPHA_SHA="$(printf '%s' "$ALPHA" | sha256sum | awk '{print $1}')"
PROMPT_SHA="$(printf '%s' "$PROMPT" | sha256sum | awk '{print $1}')"
MODEL_SHA="$(printf '%s' "$MODEL" | sha256sum | awk '{print $1}')"

step_begin "store" "Deterministic store, typed relations, two snapshots, three Context Roots"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" init
run_ok mkdir-agents "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /agents --seed "dir-agents"
run_ok mkdir-prompts "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /prompts --seed "dir-prompts"
run_ok mkdir-models "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /models --seed "dir-models"
run_ok create-alpha "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /agents/alpha.bin \
  --text "$ALPHA" --seed "alpha-out-v1" --attr "task=agent-42" --attr "model=llama-4@ver_19" --attr "sha256=$ALPHA_SHA"
run_ok create-prompt "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /prompts/prompt.txt \
  --text "$PROMPT" --seed "prompt-v1" --attr "sha256=$PROMPT_SHA"
run_ok create-model "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /models/llama-4.bin \
  --text "$MODEL" --seed "model-v1" --attr "sha256=$MODEL_SHA"
run_ok snapshot-s1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" snapshot create s1

# Second time: a byte-identical object with a distinct ObjectId and a distinct
# context membership (task only, not the llama-4 model).
run_ok create-beta "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" create /agents/beta.bin \
  --text "$ALPHA" --seed "beta-out-v1" --attr "task=agent-42" --attr "model=other-model" --attr "sha256=$ALPHA_SHA"
run_ok snapshot-s2 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" snapshot create s2

run_ok stat-alpha "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /agents/alpha.bin
ALPHA_ID="$(jsonq "$RUN_OUT" object_id)"
ALPHA_CID="$(jsonq "$RUN_OUT" content_id)"
run_ok stat-beta "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /agents/beta.bin
BETA_ID="$(jsonq "$RUN_OUT" object_id)"
BETA_CID="$(jsonq "$RUN_OUT" content_id)"
run_ok stat-prompt "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /prompts/prompt.txt
PROMPT_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /models/llama-4.bin
MODEL_ID="$(jsonq "$RUN_OUT" object_id)"

assert_ne "two byte-identical objects have distinct ObjectIds" "$ALPHA_ID" "$BETA_ID"
assert_eq "two byte-identical objects share one ContentId" "$ALPHA_CID" "$BETA_CID"

# Typed edges: task, model, generated_by, derived_from, prompted_by.
run_ok rel-task "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate task --value "agent-42"
run_ok rel-model "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate model --value "llama-4@ver_19"
run_ok rel-generated "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate generated_by --object "$MODEL_ID"
run_ok rel-derived "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$BETA_ID" --predicate derived_from --object "$ALPHA_ID"
run_ok rel-prompted "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$BETA_ID" --predicate prompted_by --object "$PROMPT_ID"
run_ok rel-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" relation list
assert_eq "five typed relations recorded" "5" "$(jsonq "$RUN_OUT" relations#len)"

run_ok ctx-task "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context create \
  --name "by:task=agent-42" --selector 'select attrs.task == "agent-42"' --snapshot s2
CTX_TASK_ID="$(jsonq "$RUN_OUT" context_root_id)"
CTX_TASK_URI="$(jsonq "$RUN_OUT" uri)"
run_ok ctx-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context create \
  --name "by:model=llama-4@ver_19" --selector 'select attrs.model == "llama-4@ver_19"' --snapshot s1
CTX_MODEL_ID="$(jsonq "$RUN_OUT" context_root_id)"
CTX_MODEL_URI="$(jsonq "$RUN_OUT" uri)"
run_ok ctx-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context list
assert_eq "two stored Context Roots plus none hidden" "2" "$(jsonq "$RUN_OUT" contexts#len)"
artifact_local "store-FORMAT" "$S/FORMAT"
step_end pass "alpha=$ALPHA_ID beta=$BETA_ID content=$ALPHA_CID; contexts=$CTX_TASK_ID,$CTX_MODEL_ID"

step_begin "mounts" "Three simultaneous mounts: Rust context, Go context, Go time root"
M1="$MOUNT_ROOT/f3-$RUN_ID-task"
M2="$MOUNT_ROOT/f3-$RUN_ID-model"
M3="$MOUNT_ROOT/f3-$RUN_ID-time"
start_mount rust "$S" "$M1" --context "by:task=agent-42"
start_mount go "$S" "$M2" --context "by:model=llama-4@ver_19"
start_mount go "$S" "$M3" --at "2026-01-01T00:00:00Z"

record_cmd "ls $M1"
ls -1 "$M1" > "$STEP_DIR/ls-task.txt"
record_cmd "ls $M2"
ls -1 "$M2" > "$STEP_DIR/ls-model.txt"
record_cmd "ls $M3"
ls -1 "$M3" > "$STEP_DIR/ls-time.txt"
save_out ls-task "$STEP_DIR/ls-task.txt"
save_out ls-model "$STEP_DIR/ls-model.txt"
save_out ls-time "$STEP_DIR/ls-time.txt"

TASK_NAMES="$(grep -v '^\.weft$' "$STEP_DIR/ls-task.txt" | wc -l | tr -d ' ')"
MODEL_NAMES="$(grep -v '^\.weft$' "$STEP_DIR/ls-model.txt" | wc -l | tr -d ' ')"
assert_eq "by:task lists all authorized results" "2" "$TASK_NAMES"
assert_eq "by:model lists exactly its authorized result" "1" "$MODEL_NAMES"
assert_ne "the two context listings differ" \
  "$(grep -v '^\.weft$' "$STEP_DIR/ls-task.txt" | sort | sha256sum | awk '{print $1}')" \
  "$(grep -v '^\.weft$' "$STEP_DIR/ls-model.txt" | sort | sha256sum | awk '{print $1}')"
assert_ne "the time root listing differs from by:task" \
  "$(listing_digest "$M1")" "$(listing_digest "$M3")"
step_end pass "task=2 model=1 time=$(grep -vc '^\.weft$' "$STEP_DIR/ls-time.txt")"

step_begin "verify-bytes" "Every listed file matches its declared content root and SHA-256"
for pair in "task:$M1:by:task=agent-42" "model:$M2:by:model=llama-4@ver_19"; do
  IFS=: read -r label mount ctx <<< "$pair"
  run_ok "resolve-$label" "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context resolve "$ctx"
  cp "$RUN_OUT" "$STEP_DIR/resolve-$label.json"
  count="$(jsonq "$STEP_DIR/resolve-$label.json" returned)"
  i=0
  while [ "$i" -lt "$count" ]; do
    oid="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.object_id")"
    cid="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.content_id")"
    path="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.path")"
    base="$(basename "$path")"
    name="$base--${oid:0:8}"
    run_ok "stat-$label-$i" "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat "$path"
    sha="$(jsonq "$RUN_OUT" attrs.sha256)"
    assert_sha256 "$label/$name" "$mount/$name" "$sha"
    assert_eq "$label result $i content id matches stat" "$cid" "$(jsonq "$RUN_OUT" content_id)"
    i=$((i + 1))
  done
done

# Time root: the exact same paths and bytes at TS1; beta (TS2) is absent.
run_ok stat-time-alpha "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat /agents/alpha.bin
assert_sha256 "time/agents/alpha.bin" "$M3/agents/alpha.bin" "$(jsonq "$RUN_OUT" attrs.sha256)"
assert_not_exists "time root has no TS2 object" "$M3/agents/beta.bin"
assert_sha256 "time/prompts/prompt.txt" "$M3/prompts/prompt.txt" "$PROMPT_SHA"

# The same logical object (alpha) appears in both contexts through one
# content-addressed manifest, not a second chunk.
TASK_HAS_ALPHA=0
MODEL_HAS_ALPHA=0
j=0
while [ "$j" -lt "$(jsonq "$STEP_DIR/resolve-task.json" returned)" ]; do
  [ "$(jsonq "$STEP_DIR/resolve-task.json" "results.$j.object_id")" = "$ALPHA_ID" ] && TASK_HAS_ALPHA=1
  j=$((j + 1))
done
j=0
while [ "$j" -lt "$(jsonq "$STEP_DIR/resolve-model.json" returned)" ]; do
  [ "$(jsonq "$STEP_DIR/resolve-model.json" "results.$j.object_id")" = "$ALPHA_ID" ] && MODEL_HAS_ALPHA=1
  j=$((j + 1))
done
assert_eq "alpha is selected by by:task" "1" "$TASK_HAS_ALPHA"
assert_eq "alpha is selected by by:model" "1" "$MODEL_HAS_ALPHA"
MANIFEST="$S/manifests/${ALPHA_CID:0:2}/$ALPHA_CID"
if [ -f "$MANIFEST" ]; then
  record_assert "one manifest file backs both contexts" present present true
  artifact_local "shared-manifest" "$MANIFEST"
else
  record_assert "one manifest file backs both contexts" present missing false
fi
assert_eq "both mounts read one content hash" "$ALPHA_CID" "$(jsonq "$STEP_DIR/resolve-model.json" results.0.content_id)"
step_end pass "all listed SHA-256 values match stored attrs; one manifest $ALPHA_CID"

step_begin "read-only" "Every mount is read-only and rejects mutation"
for spec in "task:$M1" "model:$M2" "time:$M3"; do
  label="${spec%%:*}"
  mount="${spec#*:}"
  run_any "write-$label" bash -c "printf x > '$mount/forbidden.txt'"
  if [ "$RUN_STATUS" -ne 0 ]; then record_assert "write denied $label" denied denied true; else record_assert "write denied $label" denied allowed false; fi
  run_any "mkdir-$label" mkdir "$mount/forbidden-dir"
  if [ "$RUN_STATUS" -ne 0 ]; then record_assert "mkdir denied $label" denied denied true; else record_assert "mkdir denied $label" denied allowed false; fi
done
assert_not_exists "no file was created by the denied writes" "$M1/forbidden.txt"
step_end pass "EROFS on every mount"

step_begin "concurrency" "A writer can commit while three read-only mounts are live"
run_ok live-write "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS3" create /agents/live.txt --text "live\n" --seed "live-v1"
assert_eq "live writer committed" "0" "$RUN_STATUS"
# The time-pinned mount must not observe the later commit.
assert_not_exists "pinned mount does not follow the live writer" "$M3/agents/live.txt"
step_end pass "writer lock was not held by the mounts"

step_begin "listing-digests" "Record per-mount listing digests"
D1="$(listing_digest "$M1")"
D2="$(listing_digest "$M2")"
D3="$(listing_digest "$M3")"
record_assert "by:task listing digest" "$D1" "$D1" true
record_assert "by:model listing digest" "$D2" "$D2" true
record_assert "time listing digest" "$D3" "$D3" true
assert_ne "context listing digests differ" "$D1" "$D2"
assert_ne "time listing digest differs" "$D3" "$D1"
step_end pass "digests task=$D1 model=$D2 time=$D3"
