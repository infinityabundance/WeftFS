#!/usr/bin/env bash
# Publish the Python `weftcodec` package to PyPI inside the OOM-protected
# python court.
#
#   scripts/publish-pypi.sh [--dry-run] [--from-dist]
#
# `--from-dist` uploads the artifacts already in `python/dist/` instead of
# rebuilding them. Python builds here are not bit-reproducible, so a release
# note that names digests must use this mode to make the uploaded bytes the
# recorded bytes.
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
from_dist=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=1 ;;
    --from-dist) from_dist=1 ;;
    *)
      echo "publish-pypi: unknown argument $arg" >&2
      exit 64
      ;;
  esac
done

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

inner_args=""
if [[ "$dry_run" == "1" ]]; then
  inner_args="$inner_args --dry-run"
fi
if [[ "$from_dist" == "1" ]]; then
  inner_args="$inner_args --from-dist"
fi

if [[ "$dry_run" == "1" ]]; then
  # A dry run builds (or reuses) and twine-checks, then stops before the token
  # is read, so it must work without a credential. `--from-dist` on its own
  # still uploads, so it follows the token path below.
  inner="cd /work && bash scripts/pypi-publish-inner.sh$inner_args"
else
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
  staged_in_court="/work/${staged#"$root"/}"
  inner="cd /work && WEFT_PYPI_TOKEN_FILE=$staged_in_court bash scripts/pypi-publish-inner.sh$inner_args"
fi

mkdir -p "$dist"

echo "publish-pypi: version=$version dry_run=$dry_run from_dist=$from_dist"
# Deliberately not `exec`: the EXIT trap must still run so the staging copy is
# removed even when the court fails.
"$root/scripts/docker-court.sh" python "$inner"
