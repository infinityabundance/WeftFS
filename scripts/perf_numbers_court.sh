#!/usr/bin/env bash
# Host-only court: every generated performance number must match its receipt.
#
# `scripts/perf_numbers.py --check` fails closed on a stale region, a missing
# receipt, a receipt whose stamp or revision no longer matches the manifest, a
# manifest number that no block prints, or a generated block id that the
# manifest does not declare. This wrapper exists so the aggregate can record an
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
  echo "perf_numbers: skip no Python >= 3.8 interpreter is available on the host" >&2
  exit 78
fi

exec "$python_bin" scripts/perf_numbers.py --check
