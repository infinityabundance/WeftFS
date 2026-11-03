#!/usr/bin/env bash
# F3.2 / A4 — Time-Travel POSIX.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-time-travel.sh"
#
# Creates 100 versions of one path, snapshots versions 1, 50, and 100, mounts
# all three snapshots at once (mixing Rust and Go), verifies exact historical
# bytes, then renames/retags the live path and shows the mounted reads are
# unchanged and pinned.
#
# Time travel here is a correctness property only. There is no io_uring,
# EKBLOOM, or other acceleration: none is implemented, and none is claimed.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

demo_init "weftfs.time-travel.receipt/1" "time-travel" "Time-Travel POSIX (A4)"
note "no io_uring/EKBLOOM acceleration: not implemented; time travel is correctness-only in this stage"

S="$STORES/time-travel-$RUN_ID"
FIX="$FIXTURES/time-travel-$RUN_ID"
rm -rf "$S" "$FIX"
mkdir -p "$S" "$FIX"
printf 'version-1\n' > "$FIX/v1-expected.txt"
printf 'version-50\n' > "$FIX/v50-expected.txt"
printf 'version-100\n' > "$FIX/v100-expected.txt"
BASE_TS=1767225600000000000
ts_for() { printf '%s' "$((BASE_TS + $1 * 1000000000))"; }

step_begin "versions" "100 committed versions of one path with snapshots at 1, 50, 100"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$BASE_TS" init
run_ok create-v1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 1)" create /history.txt \
  --text $'version-1\n' --seed "history-v1"
run_ok stat-v1 "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for 1)" stat /history.txt
HID="$(jsonq "$RUN_OUT" object_id)"
run_ok snapshot-v1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 1)" snapshot create v1
i=2
while [ "$i" -le 100 ]; do
  printf -v content 'version-%d\n' "$i"
  run_ok "write-v$i" "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for "$i")" write /history.txt --text "$content"
  if [ "$i" -eq 50 ]; then
    run_ok snapshot-v50 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 50)" snapshot create v50
  fi
  i=$((i + 1))
done
run_ok snapshot-v100 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 100)" snapshot create v100
run_ok snap-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for 100)" snapshot list
assert_eq "three snapshots exist" "3" "$(jsonq "$RUN_OUT" snapshots#len)"
run_ok info "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for 100)" info
save_out info "$RUN_OUT"
step_end pass "object=$HID snapshots=v1,v50,v100"

step_begin "mount-snapshots" "Three snapshots mounted simultaneously (Rust + Go)"
M1="$MOUNT_ROOT/f3-$RUN_ID-v1"
M50="$MOUNT_ROOT/f3-$RUN_ID-v50"
M100="$MOUNT_ROOT/f3-$RUN_ID-v100"
start_mount rust "$S" "$M1" --snapshot v1
start_mount go "$S" "$M50" --snapshot v50
start_mount go "$S" "$M100" --snapshot v100
record_cmd "cat $M1/history.txt"
cat "$M1/history.txt" > "$STEP_DIR/v1-read.txt"
record_cmd "cat $M50/history.txt"
cat "$M50/history.txt" > "$STEP_DIR/v50-read.txt"
record_cmd "cat $M100/history.txt"
cat "$M100/history.txt" > "$STEP_DIR/v100-read.txt"
printf 'version-1\n' > "$FIX/v1-expected.txt"
printf 'version-50\n' > "$FIX/v50-expected.txt"
printf 'version-100\n' > "$FIX/v100-expected.txt"
assert_file_eq "v1 historical bytes" "$STEP_DIR/v1-read.txt" "$FIX/v1-expected.txt"
assert_file_eq "v50 historical bytes" "$STEP_DIR/v50-read.txt" "$FIX/v50-expected.txt"
assert_file_eq "v100 historical bytes" "$STEP_DIR/v100-read.txt" "$FIX/v100-expected.txt"
save_out v1-read "$STEP_DIR/v1-read.txt"
save_out v50-read "$STEP_DIR/v50-read.txt"
save_out v100-read "$STEP_DIR/v100-read.txt"
step_end pass "each mount serves its pinned snapshot exactly"

step_begin "rename-retag-live" "Rename and retag the live path; historical mounts stay pinned"
run_ok stat-live "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for 101)" stat /history.txt
LIVE_ID="$(jsonq "$RUN_OUT" object_id)"
assert_eq "rename does not change ObjectId" "$HID" "$LIVE_ID"
run_ok rename-live "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 101)" rename /history.txt /history-renamed.txt
run_ok retag-live "$RUSTFS" --store "$S" --fixed-timestamp-ns "$(ts_for 101)" relation add \
  --subject "$HID" --predicate tagged_as --value "retagged-after-snapshot"
run_ok stat-renamed "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$(ts_for 101)" stat /history-renamed.txt
assert_eq "live path keeps ObjectId after rename/retag" "$HID" "$(jsonq "$RUN_OUT" object_id)"

# Re-read every mounted snapshot after the live mutation.
record_cmd "cat $M1/history.txt (after live rename/retag)"
cat "$M1/history.txt" > "$STEP_DIR/v1-after.txt"
record_cmd "cat $M50/history.txt (after live rename/retag)"
cat "$M50/history.txt" > "$STEP_DIR/v50-after.txt"
record_cmd "cat $M100/history.txt (after live rename/retag)"
cat "$M100/history.txt" > "$STEP_DIR/v100-after.txt"
assert_file_eq "v1 unchanged after live rename/retag" "$STEP_DIR/v1-after.txt" "$FIX/v1-expected.txt"
assert_file_eq "v50 unchanged after live rename/retag" "$STEP_DIR/v50-after.txt" "$FIX/v50-expected.txt"
assert_file_eq "v100 unchanged after live rename/retag" "$STEP_DIR/v100-after.txt" "$FIX/v100-expected.txt"
assert_not_exists "historical mount does not see the renamed path" "$M100/history-renamed.txt"
step_end pass "historical reads unchanged after live rename/retag"

step_begin "live-view" "The live store reflects the rename; historical snapshots do not"
run_ok cat-live-renamed "$RUSTFS" --store "$S" cat /history-renamed.txt
assert_file_eq "live path reads the latest bytes" "$RUN_OUT" "$FIX/v100-expected.txt"
run_expect_fail cat-live-old-path "$RUSTFS" --store "$S" cat /history.txt
assert_contains "live old path is gone" "$(cat "$RUN_ERR")" "NotFound"
step_end pass "live rename applied; snapshots unchanged"
