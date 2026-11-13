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
#   bash bench/scripts/stage_budget_repeat.sh <test-filter> <threads> <reps> <tsv-out> [per-run-seconds]
#
# `<test-filter>` is a libtest filter (`stage_budget` for the unit module,
# `restore_budget` for the ordered-pipeline integration suite).
#
# The host-side `scripts/stage_budget_court.sh` invokes this at
# --test-threads=8, 2 and 1 for the module and at 4 for the integration suite,
# and assembles the receipt from the TSVs.
set -Eeuo pipefail

cd /work

filter="${1:?test filter}"
threads="${2:?test threads}"
reps="${3:?repetitions}"
out="${4:?tsv output path}"
per_run="${5:-120}"

mkdir -p "$(dirname "$out")"
: > "$out"

# Build once so every repetition measures the test, not the compiler.
if [ "$filter" = "stage_budget" ]; then
  cargo test --quiet --lib "$filter" --no-run >/dev/null 2>&1
  tests=$(cargo test --quiet --lib "$filter" -- --list 2>/dev/null | grep -c ': test$' || true)
else
  cargo test --quiet --test "$filter" --no-run >/dev/null 2>&1
  tests=$(cargo test --quiet --test "$filter" -- --list 2>/dev/null | grep -c ': test$' || true)
fi
printf '#tests\t%s\n' "$tests" >> "$out"

passed=0
failed=0
timeouts=0
for i in $(seq 1 "$reps"); do
  started=$(date +%s%N)
  rc=0
  if [ "$filter" = "stage_budget" ]; then
    timeout "$per_run" cargo test --quiet --lib "$filter" -- --test-threads="$threads" \
      >/dev/null 2>&1 || rc=$?
  else
    timeout "$per_run" cargo test --quiet --test "$filter" -- --test-threads="$threads" \
      >/dev/null 2>&1 || rc=$?
  fi
  ended=$(date +%s%N)
  printf '%s\t%s\t%s\n' "$i" "$rc" "$(( (ended - started) / 1000000 ))" >> "$out"
  case "$rc" in
    0) passed=$((passed + 1)) ;;
    124) timeouts=$((timeouts + 1)); failed=$((failed + 1)) ;;
    *) failed=$((failed + 1)) ;;
  esac
done

echo "stage_budget_repeat: filter=$filter threads=$threads reps=$reps passed=$passed failed=$failed timeouts=$timeouts" >&2
[ "$failed" -eq 0 ]
