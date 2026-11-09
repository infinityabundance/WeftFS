#!/usr/bin/env bash
# Publish the Python `weftcodec` package to PyPI inside the OOM-protected
# python court.
#
#   scripts/publish-pypi.sh [--dry-run]
#
# Credentials: the script reads a PyPI API token from `.docker-cache/.pypi-token`
# (mode 0600, gitignored) or from `$PYPI_TOKEN`. The file is **removed on exit**
# and the token is never written into the repository, never printed, and never
# passed on a command line.
#
# The build and the upload both run inside the court under the default hard
# limits (memory == swap, pids, CPUs, bounded /tmp, no-new-privileges,
# cap-drop=ALL). Nothing is built or uploaded from the host.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

dry_run=0
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run=1
fi

cache="$root/.docker-cache"
token_file="${WEFT_PYPI_TOKEN_FILE:-$cache/.pypi-token}"
dist="$root/python/dist"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' python/pyproject.toml | head -1)"
if [[ -z "$version" ]]; then
  echo "publish-pypi: cannot read the version from python/pyproject.toml" >&2
  exit 65
fi

cleanup() { rm -f "$token_file"; }
trap cleanup EXIT INT TERM

if [[ ! -f "$token_file" ]]; then
  echo "publish-pypi: missing $token_file" >&2
  echo "  write the API token there with mode 0600 (gitignored, removed on exit)" >&2
  exit 66
fi
chmod 0600 "$token_file"

mkdir -p "$dist"

echo "publish-pypi: version=$version dry_run=$dry_run"
inner="cd /work && bash scripts/pypi-publish-inner.sh"
if [[ "$dry_run" == "1" ]]; then
  inner="$inner --dry-run"
fi
# Deliberately not `exec`: the EXIT trap must still run so the token file is
# removed even when the court fails.
"$root/scripts/docker-court.sh" python "$inner"
