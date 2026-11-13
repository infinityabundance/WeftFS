#!/usr/bin/env bash
# Inner half of `scripts/publish-pypi.sh`: runs INSIDE the python court.
#
# Builds sdist + wheel from `python/`, checks both with `twine check`, and
# uploads them to PyPI. The token path is `$WEFT_PYPI_TOKEN_FILE` (the host
# stages a private copy there and deletes only that copy), falling back to the
# historical `/work/.docker-cache/.pypi-token`. The token is read into a shell
# variable, handed to twine via TWINE_PASSWORD (never argv), and never echoed.
#
#   bash scripts/pypi-publish-inner.sh [--dry-run] [--from-dist]
#
# `--from-dist` uploads the artifacts already in `python/dist/` instead of
# rebuilding them. Python builds here are not bit-reproducible, so this is the
# only way to upload the exact bytes a release note and receipt name.
set -Eeuo pipefail

cd /work/python

dry_run=0
from_dist=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=1 ;;
    --from-dist) from_dist=1 ;;
    *)
      echo "pypi-publish: unknown argument $arg" >&2
      exit 64
      ;;
  esac
done

token_file="${WEFT_PYPI_TOKEN_FILE:-/work/.docker-cache/.pypi-token}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' pyproject.toml | head -1)"
if [[ -z "$version" ]]; then
  echo "pypi-publish: cannot read the version from pyproject.toml" >&2
  exit 65
fi

python3 -m pip install --quiet --disable-pip-version-check --upgrade \
  "setuptools>=68" wheel build twine

if [[ "$from_dist" == "1" ]]; then
  shopt -s nullglob
  artifacts=("dist/weftcodec-$version"*)
  shopt -u nullglob
  if [[ "${#artifacts[@]}" -eq 0 ]]; then
    echo "pypi-publish: --from-dist but dist/ holds no weftcodec-$version artifact" >&2
    echo "pypi-publish: run the build mode first; nothing was uploaded" >&2
    exit 66
  fi
  echo "pypi-publish: using existing artifacts for weftcodec $version (dry_run=$dry_run, from_dist=1)"
else
  echo "pypi-publish: building weftcodec $version (dry_run=$dry_run)"
  rm -rf "dist/weftcodec-$version"*
  python3 -m build --no-isolation --sdist --wheel --outdir dist
  shopt -s nullglob
  artifacts=("dist/weftcodec-$version"*)
  shopt -u nullglob
fi

python3 -m twine check "${artifacts[@]}"

for artifact in "${artifacts[@]}"; do
  printf 'pypi-publish: %s sha256=%s\n' "$artifact" "$(sha256sum "$artifact" | cut -d' ' -f1)"
done

if [[ "$dry_run" == "1" ]]; then
  echo "pypi-publish: dry run; not uploading"
  exit 0
fi

if [[ ! -f "$token_file" ]]; then
  echo "pypi-publish: missing token file inside the court" >&2
  exit 66
fi

TWINE_USERNAME=__token__
TWINE_PASSWORD="$(cat "$token_file")"
export TWINE_USERNAME TWINE_PASSWORD
python3 -m twine upload --non-interactive --disable-progress-bar \
  --repository pypi "${artifacts[@]}"
unset TWINE_PASSWORD

echo "pypi-publish: uploaded weftcodec $version"
