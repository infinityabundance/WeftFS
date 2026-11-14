#!/usr/bin/env bash
# N07 court: the strict Pareto generator's own assertions.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v python3 >/dev/null 2>&1; then
  echo "n07_pareto: skip python3 is unavailable" >&2
  exit 78
fi
python3 scripts/pareto_strict.py --self-test
python3 scripts/pareto_strict.py \
  --receipt bench/receipts/r22-pareto-20261112T230000Z.json --check
