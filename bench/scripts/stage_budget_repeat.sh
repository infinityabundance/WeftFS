#!/usr/bin/env bash
# Repeat the shipped `stage_budget` unit tests inside the rust court.
#
# The X06/N09 admission primitive is a concurrency contract: one passing run is
# not evidence that it cannot hang. This script builds once, then runs the lib
# test binary (filtered to `stage_budget`) through `timeout`, recording one TSV
# row per repetition:
#
#   <iteration>\t<exit-code>\t<elapsed-ms>
#
# Exit code 124 means the repetition did not finish inside the per-run timeout
# (a hang is recorded, never silently skipped). Any other non-zero code is a
# test failure. The script exits 0 only when every repetition passed.
#
# Usage (inside the court, repository mounted at /work):
#
#   bash bench/scripts/stage_budget_repeat.sh <threads> <reps> <tsv-out> [per-run-seconds]
#
# The host-side `scripts/stage_budget_court.sh` invokes this at
# --test-threads=8, 2 and 1 and assembles the receipt from the TSVs.
set -Eeuo pipefail

cd /work

threads="${1:?test threads}"
reps="${2:?repetitions}"
out="${3:?tsv output path}"
per_run="${4:-120}"

mkdir -p "$(dirname "$out")"
: > "$out"

# Build once so every repetition measures the test, not the compiler.
cargo test --quiet --lib stage_budget --no-run >/dev/null 2>&1
tests=$(cargo test --quiet --lib stage_budget -- --list 2>/dev/null | grep -c ': test$' || true)
printf '#tests\t%s\n' "$tests" >> "$out"

passed=0
failed=0
timeouts=0
for i in $(seq 1 "$reps"); do
  started=$(date +%s%N)
  rc=0
  timeout "$per_run" cargo test --quiet --lib stage_budget -- --test-threads="$threads" \
    >/dev/null 2>&1 || rc=$?
  ended=$(date +%s%N)
  printf '%s\t%s\t%s\n' "$i" "$rc" "$(( (ended - started) / 1000000 ))" >> "$out"
  case "$rc" in
    0) passed=$((passed + 1)) ;;
    124) timeouts=$((timeouts + 1)); failed=$((failed + 1)) ;;
    *) failed=$((failed + 1)) ;;
  esac
done

echo "stage_budget_repeat: threads=$threads reps=$reps passed=$passed failed=$failed timeouts=$timeouts" >&2
[ "$failed" -eq 0 ]
