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

TOTAL_VERSIONS="${WEFT_TIME_TRAVEL_VERSIONS:-100}"
case "$TOTAL_VERSIONS" in
  ''|*[!0-9]*) echo "WEFT_TIME_TRAVEL_VERSIONS must be a positive integer" >&2; exit 64 ;;
esac
SNAP_EVERY="${WEFT_TIME_TRAVEL_SNAPSHOT_EVERY:-100}"
case "$SNAP_EVERY" in
  ''|*[!0-9]*) echo "WEFT_TIME_TRAVEL_SNAPSHOT_EVERY must be a positive integer" >&2; exit 64 ;;
esac
if [ "$TOTAL_VERSIONS" -ne 100 ] && { [ "$TOTAL_VERSIONS" -lt 1000 ] || [ "$SNAP_EVERY" -lt 1 ]; }; then
  echo "WEFT_TIME_TRAVEL_VERSIONS=$TOTAL_VERSIONS unsupported: scaled mode needs >= 1000 versions" >&2
  exit 64
fi

# Scaled mode (WEFT_TIME_TRAVEL_VERSIONS != 100): one path, N sequential
# versions, a snapshot every SNAP_EVERY versions, and ten of those snapshots
# mounted simultaneously. The per-version payload adds a fixed padding block so
# every version has a distinct ContentId by construction.
tt_content() {
  printf 'version-%d\npadding-0123456789abcdef0123456789abcdef0123456789abcdef\n' "$1"
}
# Same bytes in a variable (a command substitution would strip the newline).
tt_set_content() {
  printf -v "$1" 'version-%d\npadding-0123456789abcdef0123456789abcdef0123456789abcdef\n' "$2"
}
run_scaled_time_travel() {
  local total="$TOTAL_VERSIONS" every="$SNAP_EVERY"
  local SS="$STORES/time-travel-$RUN_ID-v$total"
  local FIX="$FIXTURES/time-travel-$RUN_ID-v$total"
  rm -rf "$SS" "$FIX"
  mkdir -p "$SS" "$FIX"
  local BASE_TS=1767225600000000000
  ts_for() { printf '%s' "$((BASE_TS + $1 * 1000000000))"; }

  step_begin "versions" "$total sequential versions of one path, a snapshot every $every versions"
  meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; not a fleet claim"
  meta metric.versions "$total"
  meta metric.snapshot_every "$every"
  record_cmd "weftfs create /history.txt --text <version-1 + fixed padding> --seed history-v1"
  record_cmd "weftfs write /history.txt --text <version-n + fixed padding> ($((total - 1)) times)"
  record_cmd "weftfs snapshot create s<n> for n in 1,$every,..,$total"
  run_ok init "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$BASE_TS" init

  local ingest_start ingest_end wrote=0 content fixed n
  local ingest_failed=0 failed_at=0
  declare -a SNAP_VERSIONS=()
  ingest_start="$(now_ns)"
  for n in $(seq 1 "$total"); do
    fixed="$(ts_for "$n")"
    tt_set_content content "$n"
    if [ "$n" -eq 1 ]; then
      if ! "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$fixed" \
            create /history.txt --text "$content" --seed history-v1 \
            >>"$STEP_DIR/ingest.out" 2>>"$STEP_DIR/ingest.err"; then
        ingest_failed=1; failed_at=$n; break
      fi
    else
      if ! "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$fixed" \
            write /history.txt --text "$content" \
            >>"$STEP_DIR/ingest.out" 2>>"$STEP_DIR/ingest.err"; then
        ingest_failed=1; failed_at=$n; break
      fi
    fi
    wrote=$n
    if [ "$n" -eq 1 ] || [ $((n % every)) -eq 0 ]; then
      if ! "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$fixed" \
            snapshot create "s$n" >>"$STEP_DIR/ingest.out" 2>>"$STEP_DIR/ingest.err"; then
        ingest_failed=1; failed_at=$n; break
      fi
      SNAP_VERSIONS+=("$n")
    fi
  done
  ingest_end="$(now_ns)"
  local ingest_ms=$(( (ingest_end - ingest_start) / 1000000 ))
  assert_eq "every requested version was written" "$total" "$wrote" \
    "$([ "$wrote" -eq "$total" ] && echo true || echo false)"
  if [ "$ingest_failed" = 1 ]; then
    note "ingest stopped at version $failed_at (completed $wrote of $total); last error: $(tail -1 "$STEP_DIR/ingest.err" 2>/dev/null)"
    meta metric.versions_completed "$wrote"
    meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; INCOMPLETE ingest $wrote/$total versions; not a fleet claim"
    step_end fail "completed=$wrote/$total ingest_ms=$ingest_ms"
    return 0
  fi
  local rate
  rate="$(awk -v n="$total" -v ms="$ingest_ms" 'BEGIN { if (ms <= 0) print "n/a"; else printf "%.2f", n * 1000 / ms }')"
  meta metric.ingest_ms "$ingest_ms"
  meta metric.ingest_versions_per_sec "$rate"
  meta metric.versions_completed "$wrote"

  run_ok snap-list "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$(ts_for "$total")" snapshot list
  assert_eq "snapshot count" "${#SNAP_VERSIONS[@]}" "$(jsonq "$RUN_OUT" snapshots#len)"
  run_ok info "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$(ts_for "$total")" info
  save_out info "$RUN_OUT"
  local store_bytes
  store_bytes="$(du -sb "$SS" 2>/dev/null | awk '{print $1}')"
  meta metric.store_bytes "${store_bytes:-unavailable}"
  run_ok verify-full "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$total")" verify --full
  step_end pass "versions=$wrote snapshots=${#SNAP_VERSIONS[@]}"

  # Reopen timing: each invocation is a fresh process open plus bounded replay.
  step_begin "reopen" "CLI reopen + stat timing at the head"
  : > "$STEP_DIR/reopen_ms.txt"
  local reopen_failed=0 t0 t1
  for n in $(seq 1 20); do
    t0="$(now_ns)"
    if ! "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$(ts_for "$total")" \
          stat /history.txt >>"$STEP_DIR/reopen.out" 2>>"$STEP_DIR/reopen.err"; then
      reopen_failed=$((reopen_failed + 1))
    fi
    t1="$(now_ns)"
    echo "$(( (t1 - t0) / 1000000 ))" >> "$STEP_DIR/reopen_ms.txt"
  done
  assert_eq "reopen failures" 0 "$reopen_failed"
  run_ok stat-head "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$(ts_for "$total")" stat /history.txt
  local HEAD_ID
  HEAD_ID="$(jsonq "$RUN_OUT" object_id)"
  local open_p50 open_p95 open_max open_mean
  open_p50="$(percentile_file "$STEP_DIR/reopen_ms.txt" 50)"
  open_p95="$(percentile_file "$STEP_DIR/reopen_ms.txt" 95)"
  open_max="$(max_file "$STEP_DIR/reopen_ms.txt")"
  open_mean="$(mean_file "$STEP_DIR/reopen_ms.txt")"
  meta metric.reopen_p50_ms "$open_p50"
  meta metric.reopen_p95_ms "$open_p95"
  meta metric.reopen_max_ms "$open_max"
  meta metric.reopen_mean_ms "$open_mean"
  note "bounded replay decodes at most the checkpoint interval above the newest checkpoint; the measured reopen is the whole CLI process, not an in-process handle only"
  step_end pass "reopens=20 failures=$reopen_failed"

  # Ten snapshots evenly spaced over the created snapshot set, including the
  # oldest and newest, so the selection is reproducible and documented.
  local snap_count=${#SNAP_VERSIONS[@]}
  declare -a SEL_VERSIONS=() SEL_NAMES=()
  declare -A SEL_SEEN=()
  local k idx version
  for k in $(seq 0 9); do
    idx=$(( k * (snap_count - 1) / 9 ))
    version="${SNAP_VERSIONS[$idx]}"
    [ -n "${SEL_SEEN[$version]:-}" ] && continue
    SEL_SEEN[$version]=1
    SEL_VERSIONS+=("$version")
    SEL_NAMES+=("s$version")
  done
  local selected=${#SEL_NAMES[@]}
  meta metric.snapshots_mounted "$selected"
  meta metric.snapshot_selection "$(printf 's%s ' "${SEL_VERSIONS[@]}")"

  step_begin "mount-snapshots" "$selected snapshots mounted simultaneously (Rust + Go)"
  declare -a M_MOUNT=() M_IMPL=() M_PID=() M_T0=() M_NAME=() M_VERSION=()
  : > "$STEP_DIR/latencies.txt"
  local free_before free_after cgroup_peak rss_kb wall_start
  local LAT_FILE="$STEP_DIR/latencies.txt"
  free_before="$(sample_free_mb)"
  wall_start="$(now_ns)"
  local impl mount
  for idx in $(seq 0 $((selected - 1))); do
    mount="$MOUNT_ROOT/f3t-$RUN_ID-$idx"
    if [ $((idx % 2)) -eq 0 ]; then impl=rust; else impl=go; fi
    t0="$(now_ns)"
    start_mount_async "$impl" "$SS" "$mount" --snapshot "${SEL_NAMES[$idx]}"
    M_MOUNT[$idx]="$mount"; M_IMPL[$idx]="$impl"; M_PID[$idx]="$MOUNT_PID_LAST"
    M_T0[$idx]="$t0"; M_NAME[$idx]="${SEL_NAMES[$idx]}"; M_VERSION[$idx]="${SEL_VERSIONS[$idx]}"
  done
  local mounted=0 failed=0
  for idx in $(seq 0 $((selected - 1))); do
    if wait_mount "${M_MOUNT[$idx]}" 0.02 1500; then
      t1="$(now_ns)"
      echo "$(( (t1 - M_T0[$idx]) / 1000000 ))" >> "$LAT_FILE"
      mounted=$((mounted + 1))
    else
      failed=$((failed + 1))
      record_assert "snapshot mount ${SEL_NAMES[$idx]} started" mounted \
        "failed: $(tail -3 "$SCRATCH/mount-$RUN_ID-$(basename "${M_MOUNT[$idx]}").log" 2>/dev/null | tr '\n' ' ')" false
    fi
  done
  free_after="$(sample_free_mb)"
  cgroup_peak="$(cgroup_peak_bytes)"
  rss_kb="$(sum_rss_kb "${M_PID[@]}")"
  meta metric.snapshots_verified "$mounted"
  meta metric.snapshot_mount_failures "$failed"
  meta metric.free_mb_before "${free_before:-unavailable}"
  meta metric.free_mb_after "${free_after:-unavailable}"
  meta metric.daemon_rss_kb "$rss_kb"
  meta metric.cgroup_peak_bytes "$cgroup_peak"
  assert_eq "all selected snapshots mounted" "$selected" "$mounted"
  if [ "$failed" -gt 0 ]; then
    note "snapshot mount failure: requested $selected, reached $mounted, failed $failed; exact error in the failed assertion and per-mount logs"
    meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; INCOMPLETE $mounted/$selected snapshot mounts; not a fleet claim"
  else
    meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; $mounted/$selected snapshot mounts verified; not a fleet claim"
  fi
  step_end "$([ "$failed" -eq 0 ] && echo pass || echo fail)" "mounted=$mounted failed=$failed"

  step_begin "verify-snapshots" "Exact historical bytes for every mounted snapshot"
  local verify_start verify_end verified=0 mismatches=0 expected_digest
  verify_start="$(now_ns)"
  printf 'f ./history.txt\n' > "$STEP_DIR/expected-find.txt"
  expected_digest="$(sha256_file "$STEP_DIR/expected-find.txt")"
  for idx in $(seq 0 $((selected - 1))); do
    mountpoint -q "${M_MOUNT[$idx]}" 2>/dev/null || continue
    verified=$((verified + 1))
    version="${M_VERSION[$idx]}"
    tt_content "$version" > "$FIX/expected-$version.txt"
    record_cmd "cat ${M_MOUNT[$idx]}/history.txt"
    cat "${M_MOUNT[$idx]}/history.txt" > "$STEP_DIR/read-$version.txt" 2>"$STEP_DIR/read-$version.err" || mismatches=$((mismatches + 1))
    assert_file_eq "snapshot s$version historical bytes" "$STEP_DIR/read-$version.txt" "$FIX/expected-$version.txt"
    assert_eq "snapshot s$version listing digest" "$expected_digest" "$(listing_digest "${M_MOUNT[$idx]}")"
    save_out "read-s$version" "$STEP_DIR/read-$version.txt"
  done
  assert_eq "verified snapshots" "$mounted" "$verified"
  verify_end="$(now_ns)"
  local verify_ms=$(( (verify_end - verify_start) / 1000000 ))
  local mount_verify_ms=$(( (verify_end - wall_start) / 1000000 ))
  meta metric.verify_ms "$verify_ms"
  meta metric.mount_and_verify_ms "$mount_verify_ms"
  step_end "$([ "$verified" -eq "$mounted" ] && [ "$mismatches" -eq 0 ] && echo pass || echo fail)" \
    "verified=$verified mismatches=$mismatches"

  step_begin "rename-retag-live" "Live rename, retag, and later write leave every mounted snapshot unchanged"
  run_ok stat-live "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$(ts_for "$((total + 1))")" stat /history.txt
  assert_eq "live ObjectId unchanged before rename" "$HEAD_ID" "$(jsonq "$RUN_OUT" object_id)"
  run_ok rename-live "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$((total + 1))")" rename /history.txt /history-renamed.txt
  run_ok retag-live "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$((total + 1))")" relation add \
    --subject "$HEAD_ID" --predicate tagged_as --value "retagged-after-snapshot"
  tt_set_content content "$((total + 1))"
  run_ok write-later "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$((total + 2))")" \
    write /history-renamed.txt --text "$content"
  tt_content "$((total + 1))" > "$FIX/expected-later.txt"
  run_ok cat-live "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$((total + 2))")" cat /history-renamed.txt
  assert_file_eq "live renamed path reads the later bytes" "$RUN_OUT" "$FIX/expected-later.txt"
  run_expect_fail cat-old-path "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$(ts_for "$((total + 2))")" cat /history.txt
  assert_contains "live old path is gone" "$(cat "$RUN_ERR")" "NotFound"
  local after_verified=0 after_mismatch=0
  for idx in $(seq 0 $((selected - 1))); do
    mountpoint -q "${M_MOUNT[$idx]}" 2>/dev/null || continue
    version="${M_VERSION[$idx]}"
    cat "${M_MOUNT[$idx]}/history.txt" > "$STEP_DIR/after-$version.txt" 2>"$STEP_DIR/after-$version.err" || after_mismatch=$((after_mismatch + 1))
    assert_file_eq "snapshot s$version unchanged after live mutation" "$STEP_DIR/after-$version.txt" "$FIX/expected-$version.txt"
    assert_not_exists "snapshot s$version does not see the renamed path" "${M_MOUNT[$idx]}/history-renamed.txt"
    after_verified=$((after_verified + 1))
  done
  assert_eq "snapshots re-read after mutation" "$mounted" "$after_verified"
  step_end "$([ "$after_verified" -eq "$mounted" ] && [ "$after_mismatch" -eq 0 ] && echo pass || echo fail)" \
    "re-read=$after_verified mismatches=$after_mismatch"

  step_begin "unmount" "Unmount every snapshot and prove no daemon survives"
  local unmounted=0 survivors=0 tries
  for idx in $(seq 0 $((selected - 1))); do
    if stop_mount "${M_MOUNT[$idx]}"; then unmounted=$((unmounted + 1)); fi
  done
  for idx in $(seq 0 $((selected - 1))); do
    tries=0
    while kill -0 "${M_PID[$idx]}" 2>/dev/null && [ "$tries" -lt 100 ]; do sleep 0.05; tries=$((tries + 1)); done
    if kill -0 "${M_PID[$idx]}" 2>/dev/null; then
      survivors=$((survivors + 1)); kill "${M_PID[$idx]}" 2>/dev/null || true
    fi
  done
  assert_eq "clean unmounts" "$mounted" "$unmounted"
  assert_eq "daemons surviving unmount" 0 "$survivors"
  meta metric.daemons_surviving "$survivors"
  local lifetime_ms=$(( ( $(now_ns) - wall_start ) / 1000000 ))
  meta metric.mount_lifetime_ms "$lifetime_ms"
  local p50 p95 pmax
  p50="$(percentile_file "$LAT_FILE" 50)"
  p95="$(percentile_file "$LAT_FILE" 95)"
  pmax="$(max_file "$LAT_FILE")"
  meta metric.mount_latency_p50_ms "$p50"
  meta metric.mount_latency_p95_ms "$p95"
  meta metric.mount_latency_max_ms "$pmax"
  step_end "$([ "$survivors" -eq 0 ] && echo pass || echo fail)" \
    "unmounted=$unmounted survivors=$survivors"
}

demo_init "weftfs.time-travel.receipt/1" "time-travel" "Time-Travel POSIX (A4)"
note "no io_uring/EKBLOOM acceleration: not implemented; time travel is correctness-only in this stage"

if [ "$TOTAL_VERSIONS" -ne 100 ]; then
  run_scaled_time_travel
  exit 0
fi

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
