#!/usr/bin/env bash
# Fail if any exported specification copy differs from the program home.
#
# The parent `spec/` is the single source of truth. Subproject `spec/`
# directories are versioned mirrors produced by `scripts/export-spec.sh`; they
# must be byte-identical and must contain no extra normative files.
#
# Usage: scripts/check-spec-drift.sh [repo ...]
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ $# -gt 0 ]]; then
  repos=("$@")
else
  repos=(WeftCodec WeftCodec-Go WeftFS-Go)
fi

status=0
checked=0

for repo in "${repos[@]}"; do
  if [[ ! -d "$repo/spec" ]]; then
    echo "DRIFT $repo: missing spec/ directory" >&2
    status=1
    continue
  fi
  # Every parent spec must exist and match exactly.
  while IFS= read -r -d '' file; do
    name="$(basename "$file")"
    checked=$((checked + 1))
    if [[ ! -f "$repo/spec/$name" ]]; then
      echo "DRIFT $repo/spec/$name: missing" >&2
      status=1
      continue
    fi
    if ! cmp -s "$file" "$repo/spec/$name"; then
      echo "DRIFT $repo/spec/$name: differs from parent" >&2
      diff -u "$file" "$repo/spec/$name" >&2 || true
      status=1
    fi
  done < <(find spec -maxdepth 1 -type f -name '*.md' -print0 | sort -z)

  # Every subproject spec must be an exported parent file (no local-only copies).
  while IFS= read -r -d '' file; do
    name="$(basename "$file")"
    if [[ ! -f "spec/$name" ]]; then
      echo "DRIFT $repo/spec/$name: extra file not present in parent" >&2
      status=1
      checked=$((checked + 1))
    fi
  done < <(find "$repo/spec" -maxdepth 1 -type f -name '*.md' -print0 | sort -z)
done

if [[ "$status" -ne 0 ]]; then
  echo "spec drift: FAILED ($checked file comparisons)" >&2
  exit 1
fi

echo "spec drift: OK ($checked file comparisons across ${#repos[@]} repos)"
