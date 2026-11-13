#!/usr/bin/env bash
# Inner half of `scripts/publish-pypi.sh`: runs INSIDE the python court.
#
# Builds sdist + wheel from `python/`, checks both with `twine check`, and
# uploads them to PyPI. The token path is `$WEFT_PYPI_TOKEN_FILE` (the host
# stages a private copy there and deletes only that copy), falling back to the
# historical `/work/.docker-cache/.pypi-token`. The token is read into a shell
# variable, handed to twine via TWINE_PASSWORD (never argv), and never echoed.
#
#   bash scripts/pypi-publish-inner.sh [--dry-run]
set -Eeuo pipefail

cd /work/python

dry_run=0
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run=1
fi

token_file="${WEFT_PYPI_TOKEN_FILE:-/work/.docker-cache/.pypi-token}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' pyproject.toml | head -1)"
if [[ -z "$version" ]]; then
  echo "pypi-publish: cannot read the version from pyproject.toml" >&2
  exit 65
fi

echo "pypi-publish: building weftcodec $version (dry_run=$dry_run)"
python3 -m pip install --quiet --disable-pip-version-check --upgrade \
  "setuptools>=68" wheel build twine

rm -rf "dist/weftcodec-$version"*
python3 -m build --no-isolation --sdist --wheel --outdir dist
python3 -m twine check "dist/weftcodec-$version"*

for artifact in "dist/weftcodec-$version"*; do
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
  --repository pypi "dist/weftcodec-$version"*
unset TWINE_PASSWORD

echo "pypi-publish: uploaded weftcodec $version"
