#!/usr/bin/env bash
# F5.3 / B2 — crash-consistency torture (process-fault proxy).
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash bench/scripts/crash_torture.sh"
#   scripts/docker-court.sh fuse "WEFT_CRASH_ITERATIONS=10000 bash bench/scripts/crash_torture.sh"
#
# Each iteration:
#   1. installs a known old file through the *reader* implementation;
#   2. has the *writer* implementation overwrite it while WEFT_CRASH_PHASE
#      aborts the process at a publication boundary;
#   3. reopens the store with the reader implementation, runs verify --full
#      (accepting only the documented unpublished-tail note), reads /f, and
#      lists /.
#
# The observed state must be exactly the old whole file or the new whole file,
# never a partial file. after_superblock_write must expose the new state;
# after_objects and after_commit_file must expose the old state.
#
# This is a process-fault proxy, not physical power loss. The court does not
# cut power, drop write caches, or simulate torn sectors.
set -Eeuo pipefail

ROOT=/work
source "$ROOT/scripts/demo-lib.sh"

demo_init "weftfs.crash-torture.receipt/1" "crash-torture" \
  "Crash-consistency torture (B2), process-fault proxy"

ITERATIONS="${1:-${WEFT_CRASH_ITERATIONS:-1000}}"
case "$ITERATIONS" in
  ''|*[!0-9]*) echo "iterations must be a positive integer" >&2; exit 64 ;;
esac
if [ "$ITERATIONS" -lt 1 ]; then
  echo "iterations must be positive" >&2
  exit 64
fi

S="$STORES/crash-$RUN_ID"
NDJSON="$SCRATCH/crash-$RUN_ID.iterations.ndjson"
rm -rf "$S"
mkdir -p "$S"

step_begin "crash-$ITERATIONS" \
  "$ITERATIONS alternating Rust/Go crash iterations (process-fault proxy)"
: > "$NDJSON"

run_ok "init" "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" init --chunk-size 256

OLD="$STEP_DIR/old.bin"
NEW="$STEP_DIR/new.bin"
OBSERVED="$STEP_DIR/observed.bin"
ERR="$STEP_DIR/error.txt"
LS="$STEP_DIR/ls.txt"
VERIFY_OUT="$STEP_DIR/verify.json"

START_NS="$(date +%s%N)"
COMPLETED=0
FAILURES=0
VERIFY_BAD=0
VERIFY_CLEAN=0
VERIFY_UNPUBLISHED=0
OLD_COUNT=0
NEW_COUNT=0
PHASE_OBJECTS=0
PHASE_COMMIT=0
PHASE_SUPERBLOCK=0

record_iteration() {
  local iteration="$1" phase="$2" writer="$3" reader="$4" expected="$5" observed="$6" verify="$7" outcome="$8"
  printf '{"iteration":%d,"phase":"%s","writer":"%s","reader":"%s","expected":"%s","observed":"%s","verify":"%s","outcome":"%s"}\n' \
    "$iteration" "$phase" "$writer" "$reader" "$expected" "$observed" "$verify" "$outcome" >> "$NDJSON"
}

# verify_reader <impl> <store>
# Sets VERIFY_STATE to clean | unpublished | bad. Only the documented
# unpublished-tail note is accepted as a non-clean verify result.
verify_reader() {
  local impl="$1" store="$2" key count_key detail_path count index bad detail
  if [ "$impl" = rust ]; then
    run_any "verify" "$RUSTFS" --store "$store" --json verify --full
    key=ok; count_key=problems#len; detail_path=problems
  else
    run_any "verify" "$GOFS" --store "$store" --json verify --full
    key=OK; count_key=Problems#len; detail_path=Problems
  fi
  cp "$RUN_OUT" "$VERIFY_OUT" 2>/dev/null || true
  if [ "$(jsonq "$VERIFY_OUT" "$key" 2>/dev/null || echo missing)" = true ]; then
    VERIFY_STATE=clean
    return 0
  fi
  count="$(jsonq "$VERIFY_OUT" "$count_key" 2>/dev/null || echo -1)"
  case "$count" in
    ''|*[!0-9]*|-1) VERIFY_STATE=bad; return 0 ;;
  esac
  bad=0
  index=0
  while [ "$index" -lt "$count" ]; do
    if [ "$impl" = rust ]; then
      detail="$(jsonq "$VERIFY_OUT" "$detail_path.$index.detail" 2>/dev/null || echo missing)"
    else
      detail="$(jsonq "$VERIFY_OUT" "$detail_path.$index.Detail" 2>/dev/null || echo missing)"
    fi
    case "$detail" in
      *"above the highest published superblock generation"*) ;;
      *) bad=1 ;;
    esac
    index=$((index + 1))
  done
  if [ "$bad" -eq 0 ] && [ "$count" -gt 0 ]; then
    VERIFY_STATE=unpublished
  else
    VERIFY_STATE=bad
  fi
  return 0
}

for ((i = 0; i < ITERATIONS; i++)); do
  case $((i % 3)) in
    0) phase=after_objects; PHASE_OBJECTS=$((PHASE_OBJECTS + 1)); expected=old ;;
    1) phase=after_commit_file; PHASE_COMMIT=$((PHASE_COMMIT + 1)); expected=old ;;
    2) phase=after_superblock_write; PHASE_SUPERBLOCK=$((PHASE_SUPERBLOCK + 1)); expected=new ;;
  esac
  if [ $((i % 2)) -eq 0 ]; then
    writer=rust; reader=go; WRITER_CLI="$RUSTFS"; READER_CLI="$GOFS"
  else
    writer=go; reader=rust; WRITER_CLI="$GOFS"; READER_CLI="$RUSTFS"
  fi

  old_len=$((1024 + i % 512))
  new_len=$((2048 + i % 512))
  { printf 'OLD-%06d-' "$i"; head -c "$old_len" /dev/zero | tr '\0' 'o'; } > "$OLD"
  { printf 'NEW-%06d-' "$i"; head -c "$new_len" /dev/zero | tr '\0' 'n'; } > "$NEW"

  outcome=pass
  observed=missing
  verify=bad

  # Install the known old state. unlink is ignored when /f is absent.
  "$READER_CLI" --store "$S" unlink /f >/dev/null 2>"$ERR" || true
  if ! "$READER_CLI" --store "$S" create /f --file "$OLD" >/dev/null 2>"$ERR"; then
    outcome=fail
    FAILURES=$((FAILURES + 1))
    record_iteration "$i" "$phase" "$writer" "$reader" "$expected" "setup-failed" "bad" "$outcome"
    log "iteration $i setup failed: $(cat "$ERR")"
    break
  fi

  # Crash the writer at the selected publication boundary.
  if WEFT_CRASH_PHASE="$phase" "$WRITER_CLI" --store "$S" write /f --offset 0 --file "$NEW" \
      >/dev/null 2>"$ERR"; then
    outcome=fail
    FAILURES=$((FAILURES + 1))
    record_iteration "$i" "$phase" "$writer" "$reader" "$expected" "writer-survived" "bad" "$outcome"
    log "iteration $i: writer survived crash phase $phase"
    break
  fi

  # Reopen with the other implementation and verify durable state.
  verify_reader "$reader" "$S"
  verify="$VERIFY_STATE"
  case "$verify" in
    clean) VERIFY_CLEAN=$((VERIFY_CLEAN + 1)) ;;
    unpublished) VERIFY_UNPUBLISHED=$((VERIFY_UNPUBLISHED + 1)) ;;
    *) VERIFY_BAD=$((VERIFY_BAD + 1)) ;;
  esac

  # Read the file through the reader implementation.
  if "$READER_CLI" --store "$S" cat /f > "$OBSERVED" 2>"$ERR"; then
    if cmp -s "$OBSERVED" "$OLD"; then
      observed=old; OLD_COUNT=$((OLD_COUNT + 1))
    elif cmp -s "$OBSERVED" "$NEW"; then
      observed=new; NEW_COUNT=$((NEW_COUNT + 1))
    else
      observed=partial
    fi
  else
    observed=missing
  fi

  # Namespace must contain exactly /f.
  if "$READER_CLI" --store "$S" ls / > "$LS" 2>"$ERR"; then
    # Rust prints one name per line; Go prints "<target>\t<name>".
    names="$(awk -F'\t' '{print $NF}' "$LS" | grep -v '^\.weft$' | grep -v '^$' | LC_ALL=C sort | tr '\n' ',')"
  else
    names="ls-failed"
  fi

  COMPLETED=$((COMPLETED + 1))
  if [ "$observed" != "$expected" ] || [ "$names" != "f," ] || [ "$verify" = bad ]; then
    outcome=fail
    FAILURES=$((FAILURES + 1))
    log "iteration $i FAIL phase=$phase writer=$writer reader=$reader expected=$expected observed=$observed verify=$verify names=$names"
  fi
  record_iteration "$i" "$phase" "$writer" "$reader" "$expected" "$observed" "$verify" "$outcome"
  if [ "$outcome" = fail ]; then
    break
  fi
done

END_NS="$(date +%s%N)"
DURATION_MS=$(( (END_NS - START_NS) / 1000000 ))
RATE="$(awk -v n="$COMPLETED" -v ms="$DURATION_MS" 'BEGIN { if (ms <= 0) { print "n/a" } else { printf "%.1f", (n * 1000.0) / ms } }')"

record_assert "iterations requested" "$ITERATIONS" "$COMPLETED" "$([ "$COMPLETED" = "$ITERATIONS" ] && echo true || echo false)"
record_assert "iteration failures" 0 "$FAILURES" "$([ "$FAILURES" = 0 ] && echo true || echo false)"
record_assert "verify bad states" 0 "$VERIFY_BAD" "$([ "$VERIFY_BAD" = 0 ] && echo true || echo false)"
record_assert "whole old-or-new observations" "$COMPLETED" "$((OLD_COUNT + NEW_COUNT))" \
  "$([ "$((OLD_COUNT + NEW_COUNT))" = "$COMPLETED" ] && echo true || echo false)"
record_assert "phase after_objects exercised" ">0" "$PHASE_OBJECTS" "$([ "$PHASE_OBJECTS" -gt 0 ] && echo true || echo false)"
record_assert "phase after_commit_file exercised" ">0" "$PHASE_COMMIT" "$([ "$PHASE_COMMIT" -gt 0 ] && echo true || echo false)"
record_assert "phase after_superblock_write exercised" ">0" "$PHASE_SUPERBLOCK" "$([ "$PHASE_SUPERBLOCK" -gt 0 ] && echo true || echo false)"
record_assert "after_superblock_write sees new state" "$PHASE_SUPERBLOCK" "$NEW_COUNT" \
  "$([ "$NEW_COUNT" = "$PHASE_SUPERBLOCK" ] && echo true || echo false)"
record_assert "pre-superblock phases see old state" "$((PHASE_OBJECTS + PHASE_COMMIT))" "$OLD_COUNT" \
  "$([ "$OLD_COUNT" = "$((PHASE_OBJECTS + PHASE_COMMIT))" ] && echo true || echo false)"
note "iterations=$COMPLETED duration_ms=$DURATION_MS rate_iters_per_sec=$RATE"
note "verify clean=$VERIFY_CLEAN unpublished-tail=$VERIFY_UNPUBLISHED bad=$VERIFY_BAD"
note "observed old=$OLD_COUNT new=$NEW_COUNT partial_or_missing=$((COMPLETED - OLD_COUNT - NEW_COUNT))"
note "phases: after_objects=$PHASE_OBJECTS after_commit_file=$PHASE_COMMIT after_superblock_write=$PHASE_SUPERBLOCK"
note "process-fault proxy via WEFT_CRASH_PHASE (SIGABRT/SIGKILL); NOT physical power loss, no cache drop, no torn-sector simulation"
note "10000-iteration command: scripts/docker-court.sh fuse \"WEFT_CRASH_ITERATIONS=10000 bash bench/scripts/crash_torture.sh\""

save_out "iterations" "$NDJSON"
artifact_local "iterations" "$NDJSON"
artifact_local "store-FORMAT" "$S/FORMAT"

if [ "$FAILURES" -eq 0 ] && [ "$VERIFY_BAD" -eq 0 ]; then
  step_end pass "iterations=$COMPLETED failures=0 old=$OLD_COUNT new=$NEW_COUNT duration_ms=$DURATION_MS rate=$RATE/s"
else
  step_end fail "iterations=$COMPLETED failures=$FAILURES verify_bad=$VERIFY_BAD"
fi
