#!/usr/bin/env bash
# P5a — branching time travel from a 10,000-version store.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-branching.sh"
#
# Builds N versions of one path with a snapshot every E versions (default
# 10,000 / 500, so s5000 exists), branches s5000 into `by:task=agent-43`,
# writes five more versions on the live head, proves the source snapshot and
# its descendant snapshot are byte-unchanged, and mounts the branch context.
#
# The receipt is bench/receipts/branching-<UTC>.json. Label: single-host court,
# synthetic deterministic fixtures, local store, not a fleet claim.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

TOTAL_VERSIONS="${WEFT_BRANCHING_VERSIONS:-10000}"
SNAP_EVERY="${WEFT_BRANCHING_SNAPSHOT_EVERY:-500}"
FORK_VERSION="${WEFT_BRANCHING_FORK_VERSION:-5000}"
for value in "$TOTAL_VERSIONS" "$SNAP_EVERY" "$FORK_VERSION"; do
  case "$value" in
    ''|*[!0-9]*) echo "branching counts must be positive integers" >&2; exit 64 ;;
  esac
done
if [ "$TOTAL_VERSIONS" -lt 10 ] || [ "$SNAP_EVERY" -lt 1 ]; then
  echo "WEFT_BRANCHING_VERSIONS must be >= 10 and SNAPSHOT_EVERY >= 1" >&2
  exit 64
fi
if [ $((FORK_VERSION % SNAP_EVERY)) -ne 0 ] || [ "$FORK_VERSION" -gt "$TOTAL_VERSIONS" ]; then
  echo "WEFT_BRANCHING_FORK_VERSION must be a multiple of SNAPSHOT_EVERY and <= VERSIONS" >&2
  exit 64
fi

BASE_TS=1767225600000000000
ts_for() { printf '%s' "$((BASE_TS + $1 * 1000000000))"; }
b_content() {
  printf 'branch-test version-%d\npadding-0123456789abcdef0123456789abcdef0123456789abcdef\n' "$1"
}
b_set() { printf -v "$1" 'branch-test version-%d\npadding-0123456789abcdef0123456789abcdef0123456789abcdef\n' "$2"; }

demo_init "weftfs.branching.receipt/1" "branching" "Branching Time Travel (P5a)"
meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; not a fleet claim"
meta metric.versions "$TOTAL_VERSIONS"
meta metric.snapshot_every "$SNAP_EVERY"
meta metric.fork_version "$FORK_VERSION"

S="$STORES/branching-$RUN_ID"
FIX="$FIXTURES/branching-$RUN_ID"
rm -rf "$S" "$FIX"
mkdir -p "$S" "$FIX"

# ---- 1. build the version history ---------------------------------------
step_begin "versions" "$TOTAL_VERSIONS versions with a snapshot every $SNAP_EVERY"
record_cmd "weftfs create /history.txt --attr task=agent-43 --seed history-v1"
record_cmd "weftfs write /history.txt (versions 2..$TOTAL_VERSIONS)"
record_cmd "weftfs snapshot create s<n> for n in 1,$SNAP_EVERY,..,$TOTAL_VERSIONS"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$BASE_TS" init
b_set content 1
run_ok create-v1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 1)" create /history.txt \
  --text "$content" --attr task=agent-43 --seed history-v1
run_ok snapshot-s1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 1)" snapshot create s1

ingest_start="$(now_ns)"
wrote=1
failed_at=0
snapshots=1
n=2
while [ "$n" -le "$TOTAL_VERSIONS" ]; do
  b_set content "$n"
  if ! "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for "$n")" \
        write /history.txt --text "$content" >>"$STEP_DIR/ingest.out" 2>>"$STEP_DIR/ingest.err"; then
    failed_at=$n
    break
  fi
  wrote=$n
  if [ $((n % SNAP_EVERY)) -eq 0 ]; then
    if ! "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for "$n")" \
          snapshot create "s$n" >>"$STEP_DIR/ingest.out" 2>>"$STEP_DIR/ingest.err"; then
      failed_at=$n
      break
    fi
    snapshots=$((snapshots + 1))
  fi
  n=$((n + 1))
done
ingest_ms=$(( ( $(now_ns) - ingest_start ) / 1000000 ))
assert_eq "every version was written" "$TOTAL_VERSIONS" "$wrote"
assert_eq "ingest failed at version (0 = none)" 0 "$failed_at"
meta metric.ingest_ms "$ingest_ms"
meta metric.versions_completed "$wrote"
meta metric.snapshots_created "$snapshots"
step_end "$([ "$wrote" -eq "$TOTAL_VERSIONS" ] && echo pass || echo fail)" \
  "versions=$wrote snapshots=$snapshots ingest_ms=$ingest_ms"

step_begin "snapshot-identities" "Record the source and descendant snapshot commits"
run_ok snap-list-before "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" snapshot list
S5000="$(jsonq "$RUN_OUT" "snapshots.s$FORK_VERSION")"
S_LAST="$(jsonq "$RUN_OUT" "snapshots.s$TOTAL_VERSIONS")"
assert_ne "s$FORK_VERSION and s$TOTAL_VERSIONS are distinct commits" "$S5000" "$S_LAST"
meta metric.source_snapshot "s$FORK_VERSION"
meta metric.source_commit "$S5000"
meta metric.descendant_snapshot "s$TOTAL_VERSIONS"
meta metric.descendant_commit "$S_LAST"
run_ok verify-full "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" verify --full
step_end pass "source=$S5000"

# ---- 2. branch s5000 into a snapshot ref and a Context Root --------------
step_begin "branch" "Branch s$FORK_VERSION and persist a Context Root at the fork commit"
record_cmd "weftfs snapshot branch s$FORK_VERSION by:task=agent-43"
run_ok branch-ref "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" \
  snapshot branch "s$FORK_VERSION" "by:task=agent-43"
assert_eq "the branch names the source commit" "$S5000" "$(jsonq "$RUN_OUT" snapshot_id)"
run_ok snap-list-branched "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" snapshot list
assert_eq "the snapshot branch is present" "$S5000" "$(jsonq "$RUN_OUT" "snapshots.by:task=agent-43")"

record_cmd "weftfs context create --name fork-$FORK_VERSION --selector 'select attrs.task == \"agent-43\"' --snapshot s$FORK_VERSION"
run_ok context-source "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" \
  context create --name "fork-$FORK_VERSION" --selector 'select attrs.task == "agent-43"' --snapshot "s$FORK_VERSION"
record_cmd "weftfs context branch fork-$FORK_VERSION by:task=agent-43 --task agent-43"
run_ok context-branch "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for "$TOTAL_VERSIONS")" \
  context branch "fork-$FORK_VERSION" "by:task=agent-43" --task agent-43
assert_eq "the Context Root resolves against the fork commit" "$S5000" "$(jsonq "$RUN_OUT" snapshot_id)"
assert_eq "lineage names the source context" "fork-$FORK_VERSION" "$(jsonq "$RUN_OUT" derived_from)"
CTX_ID="$(jsonq "$RUN_OUT" context_root_id)"
meta metric.branch_context_id "$CTX_ID"
meta metric.branch_context_snapshot "$(jsonq "$RUN_OUT" snapshot_id)"
step_end pass "context=$CTX_ID"

# ---- 3. write five more versions on the live head ------------------------
step_begin "write-after-branch" "Five new versions after the branch"
n=$((TOTAL_VERSIONS + 1))
while [ "$n" -le $((TOTAL_VERSIONS + 5)) ]; do
  b_set content "$n"
  run_ok "write-after-$n" "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for "$n")" \
    write /history.txt --text "$content"
  n=$((n + 1))
done
b_content $((TOTAL_VERSIONS + 5)) > "$FIX/live-expected.txt"
run_ok cat-live "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for $((TOTAL_VERSIONS + 5)))" cat /history.txt
assert_file_eq "the live head sees the last write" "$RUN_OUT" "$FIX/live-expected.txt"
step_end pass "live version=$((TOTAL_VERSIONS + 5))"

# ---- 4. the source and its descendants are unchanged ---------------------
step_begin "source-unchanged" "Source and descendant commits are byte-identical after the writes"
run_ok snap-list-after "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for $((TOTAL_VERSIONS + 5)))" snapshot list
assert_eq "s$FORK_VERSION did not move" "$S5000" "$(jsonq "$RUN_OUT" "snapshots.s$FORK_VERSION")"
assert_eq "s$TOTAL_VERSIONS did not move" "$S_LAST" "$(jsonq "$RUN_OUT" "snapshots.s$TOTAL_VERSIONS")"
assert_eq "the branch ref did not move" "$S5000" "$(jsonq "$RUN_OUT" "snapshots.by:task=agent-43")"
record_cmd "weftfs verify --full (after writes)"
run_ok verify-after "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for $((TOTAL_VERSIONS + 5)))" verify --full
step_end pass "commits unchanged"

# ---- 5. mount the source snapshot, the descendant, and the branch --------
step_begin "mounts" "Mount s$FORK_VERSION, s$TOTAL_VERSIONS, and the branch Context Root"
M_SRC="$MOUNT_ROOT/p5a-$RUN_ID-src"
M_LAST="$MOUNT_ROOT/p5a-$RUN_ID-last"
M_BRANCH="$MOUNT_ROOT/p5a-$RUN_ID-branch"
M_CTX="$MOUNT_ROOT/p5a-$RUN_ID-ctx"
t0="$(now_ns)"
start_mount_async rust "$S" "$M_SRC" --snapshot "s$FORK_VERSION"
PID_SRC="$MOUNT_PID_LAST"
start_mount_async go "$S" "$M_LAST" --snapshot "s$TOTAL_VERSIONS"
PID_LAST="$MOUNT_PID_LAST"
start_mount_async go "$S" "$M_BRANCH" --snapshot "by:task=agent-43"
PID_BRANCH="$MOUNT_PID_LAST"
start_mount_async rust "$S" "$M_CTX" --context "by:task=agent-43"
PID_CTX="$MOUNT_PID_LAST"
mounted=0
for mount in "$M_SRC" "$M_LAST" "$M_BRANCH" "$M_CTX"; do
  if wait_mount "$mount" 0.05 3000; then mounted=$((mounted + 1)); fi
done
mount_ms=$(( ( $(now_ns) - t0 ) / 1000000 ))
assert_eq "all four mounts appeared" 4 "$mounted"
meta metric.mounts "$mounted"
meta metric.mount_ms "$mount_ms"

b_content "$FORK_VERSION" > "$FIX/fork-expected.txt"
b_content "$TOTAL_VERSIONS" > "$FIX/descendant-expected.txt"
record_cmd "cat <s$FORK_VERSION>/history.txt"
cat "$M_SRC/history.txt" > "$STEP_DIR/src.txt"
assert_file_eq "source snapshot bytes" "$STEP_DIR/src.txt" "$FIX/fork-expected.txt"
record_cmd "cat <s$TOTAL_VERSIONS>/history.txt"
cat "$M_LAST/history.txt" > "$STEP_DIR/last.txt"
assert_file_eq "descendant snapshot bytes" "$STEP_DIR/last.txt" "$FIX/descendant-expected.txt"
record_cmd "cat <snapshot-ref by:task=agent-43>/history.txt"
cat "$M_BRANCH/history.txt" > "$STEP_DIR/branch.txt"
assert_file_eq "snapshot branch bytes equal the source" "$STEP_DIR/branch.txt" "$FIX/fork-expected.txt"
# A Context Root mount exposes its result set at the mount root with
# `<basename>--<object-id-prefix>` names (see demo-context-mounts.sh).
CTX_FILES="$(find "$M_CTX" -mindepth 1 -maxdepth 1 -type f | wc -l)"
assert_eq "the branch Context Root lists exactly one object" 1 "$CTX_FILES"
CTX_FILE="$(find "$M_CTX" -mindepth 1 -maxdepth 1 -type f -print -quit)"
record_cmd "cat <context by:task=agent-43>/$(basename "$CTX_FILE")"
cat "$CTX_FILE" > "$STEP_DIR/ctx.txt"
assert_file_eq "branch Context Root bytes equal the fork" "$STEP_DIR/ctx.txt" "$FIX/fork-expected.txt"
save_out src-read "$STEP_DIR/src.txt"
save_out branch-read "$STEP_DIR/branch.txt"
save_out context-read "$STEP_DIR/ctx.txt"
step_end pass "four mounts, immutable fork"

step_begin "unmount" "Unmount every branch mount and prove no daemon survives"
unmounted=0
for mount in "$M_SRC" "$M_LAST" "$M_BRANCH" "$M_CTX"; do
  if stop_mount "$mount"; then unmounted=$((unmounted + 1)); fi
done
survivors=0
for pid in "$PID_SRC" "$PID_LAST" "$PID_BRANCH" "$PID_CTX"; do
  tries=0
  while kill -0 "$pid" 2>/dev/null && [ "$tries" -lt 100 ]; do sleep 0.05; tries=$((tries + 1)); done
  if kill -0 "$pid" 2>/dev/null; then survivors=$((survivors + 1)); kill "$pid" 2>/dev/null || true; fi
done
assert_eq "clean unmounts" 4 "$unmounted"
assert_eq "daemons surviving unmount" 0 "$survivors"
meta metric.daemons_surviving "$survivors"
step_end "$([ "$survivors" -eq 0 ] && echo pass || echo fail)" "unmounted=$unmounted survivors=$survivors"
