#!/usr/bin/env bash
# Export the normative specification from the program home into subprojects.
# The parent spec/ is the single source of truth; subproject copies are
# versioned mirrors, never edited in place.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for repo in WeftCodec WeftCodec-Go WeftFS-Go; do
  if [[ -d "$root/$repo/.git" || -f "$root/$repo/.git" ]]; then
    mkdir -p "$root/$repo/spec"
    cp "$root"/spec/*.md "$root/$repo/spec/"
    echo "exported spec -> $repo/spec"
  else
    echo "skip $repo (not checked out)" >&2
  fi
done
