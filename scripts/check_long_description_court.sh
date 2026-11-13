#!/usr/bin/env bash
# Host-only court: the wheel's long description must not describe its own
# publication.
#
# `python/README.md` is baked into the wheel at build time, so it cannot name
# the version a registry serves, whether the artifact was uploaded, or a digest
# of itself: those statements become false on upload and make every rebuild
# differ. `scripts/check_long_description.py` rejects that class of statement
# and requires the stable replacement (unpinned install command, PyPI project
# URL, crates.io crate URL). This wrapper exists so the aggregate records an
# explicit environment skip (exit 78 with one printed reason) when no Python 3
# interpreter is available, instead of failing the run for a missing tool.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

python_bin=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1; then
    if "$candidate" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 8) else 1)' >/dev/null 2>&1; then
      python_bin="$candidate"
      break
    fi
  fi
done
if [[ -z "$python_bin" ]]; then
  echo "long_description: skip no Python >= 3.8 interpreter is available on the host" >&2
  exit 78
fi

exec "$python_bin" scripts/check_long_description.py "$@"
