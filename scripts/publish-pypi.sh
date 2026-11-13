#!/usr/bin/env bash
# Publish the Python `weftcodec` package to PyPI inside the OOM-protected
# python court.
#
#   scripts/publish-pypi.sh [--dry-run]
#
# Credentials, in precedence order:
#   * `$PYPI_TOKEN` (the token itself, never written to a tracked file);
#   * `$WEFT_PYPI_TOKEN_FILE` (an operator-supplied file, left untouched);
#   * `.docker-cache/.pypi-token` (the default operator file, also untouched).
#
# The script stages a private 0600 copy under `.docker-cache/` for the court and
# removes **only that copy** on exit. An operator-supplied file is never deleted
# or modified. The token is never written into the repository, never printed,
# and never passed on a command line.
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
dist="$root/python/dist"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' python/pyproject.toml | head -1)"
if [[ -z "$version" ]]; then
  echo "publish-pypi: cannot read the version from python/pyproject.toml" >&2
  exit 65
fi

mkdir -p "$cache"
staged=""
cleanup() {
  if [[ -n "$staged" && -f "$staged" ]]; then
    rm -f "$staged"
  fi
}
trap cleanup EXIT INT TERM

if [[ -n "${PYPI_TOKEN:-}" ]]; then
  staged="$(mktemp "$cache/.pypi-token-stage.XXXXXX")"
  chmod 0600 "$staged"
  printf '%s' "$PYPI_TOKEN" >"$staged"
elif [[ -f "${WEFT_PYPI_TOKEN_FILE:-$cache/.pypi-token}" ]]; then
  source_file="${WEFT_PYPI_TOKEN_FILE:-$cache/.pypi-token}"
  staged="$(mktemp "$cache/.pypi-token-stage.XXXXXX")"
  chmod 0600 "$staged"
  cat "$source_file" >"$staged"
else
  echo "publish-pypi: no token available" >&2
  echo "  set PYPI_TOKEN, or point WEFT_PYPI_TOKEN_FILE at a readable file," >&2
  echo "  or write the default .docker-cache/.pypi-token. The source file is" >&2
  echo "  never deleted; only the private staging copy is." >&2
  exit 66
fi

mkdir -p "$dist"

echo "publish-pypi: version=$version dry_run=$dry_run"
staged_in_court="/work/${staged#"$root"/}"
inner="cd /work && WEFT_PYPI_TOKEN_FILE=$staged_in_court bash scripts/pypi-publish-inner.sh"
if [[ "$dry_run" == "1" ]]; then
  inner="$inner --dry-run"
fi
# Deliberately not `exec`: the EXIT trap must still run so the staging copy is
# removed even when the court fails.
"$root/scripts/docker-court.sh" python "$inner"
