#!/usr/bin/env bash
# Structural guard for the aggregate court list.
#
# `scripts/verify-all.sh` runs top-level statements in file order, and bash
# resolves a function name when the calling line executes. A call written above
# the definition of the function it names therefore aborts the whole aggregate
# with "command not found" (exit 127) under `set -e`, after every court that
# happened to sit above it has already spent its time.
#
# That happened for real: the T048-T052 court registrations were inserted above
# `run_orchestrated_court()`, so the aggregate could not run at all until it was
# fixed. This check makes the class of defect fail in a second instead of an
# hour.
#
# Rule: for every function this script defines, its definition line must precede
# the first top-level call. A top-level call is a line that starts in column 0
# with `<name> ` (court registrations are written that way); calls inside a
# function body are indented, so they are not considered.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${1:-$root/scripts/verify-all.sh}"

if [[ ! -f "$target" ]]; then
  echo "check-verify-all-order: no such file: $target" >&2
  exit 1
fi

if ! awk '
  # Definition: name() { at column 0.
  /^[A-Za-z_][A-Za-z0-9_]*\(\)[[:space:]]*\{/ {
    name = $0
    sub(/\(\).*$/, "", name)
    if (!(name in def)) def[name] = NR
    next
  }
  # Candidate top-level call: any column-0 word. Kept unconditionally because a
  # single pass cannot know yet whether the name is a function defined further
  # down the file -- that is exactly the defect being detected.
  {
    line = $0
    if (line ~ /^[A-Za-z_][A-Za-z0-9_]*[[:space:]]/) {
      split(line, parts, /[[:space:]]+/)
      cand = parts[1]
      if (!(cand in callline)) callline[cand] = NR
    }
  }
  END {
    bad = 0
    for (name in def) {
      if ((name in callline) && callline[name] < def[name]) {
        printf "check-verify-all-order: %s() is called at line %d but defined at line %d\n",
               name, callline[name], def[name] > "/dev/stderr"
        bad = 1
      }
    }
    exit bad ? 1 : 0
  }
' "$target"; then
  exit 1
fi

echo "check-verify-all-order: every top-level call follows its definition ($target)"
