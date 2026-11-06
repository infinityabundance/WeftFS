#!/usr/bin/env bash
# Orbax/WeftFS comparison court.
#
# Run as:
#   bash scripts/docker-court.sh python "bash bench/scripts/orbax_court.sh"
#
# Installs jax[cpu] and orbax-checkpoint with `pip --target` into the court
# cache (the image is not modified), runs the Orbax adapter's unit tests, then
# runs the measured comparison harness and writes:
#   bench/receipts/orbax-<UTC>.json
#   bench/receipts/orbax-<UTC>.log
#   bench/receipts/orbax-<UTC>.install.log
#   bench/receipts/orbax-<UTC>.tests.log
#
# If the install or import fails, the harness writes an `orbax_unavailable`
# receipt with the exact error and no comparison claim is made.
set -Eeuo pipefail

cd /work

stamp="${WEFT_ORBAX_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/orbax-${stamp}.json"
log="bench/receipts/orbax-${stamp}.log"
install_log="bench/receipts/orbax-${stamp}.install.log"
tests_log="bench/receipts/orbax-${stamp}.tests.log"
target="/work/.docker-cache/py-orbax"
tmp="/work/.docker-cache/py-orbax-tmp"
pip_cache="/work/.docker-cache/py-orbax-pip"

mkdir -p "$target" "$tmp" "$pip_cache" bench/receipts
chmod -R a+rwX "$target" "$tmp" "$pip_cache" 2>/dev/null || true

export PYTHONPATH="$target:/work/python${PYTHONPATH:+:$PYTHONPATH}"
export WEFT_ORBAX_STAMP="$stamp"
export WEFT_ORBAX_RECEIPT="$receipt"
export WEFT_ORBAX_LOG="$log"
export WEFT_ORBAX_INSTALL_LOG="$install_log"
export WEFT_ORBAX_TESTS_LOG="$tests_log"
export WEFT_ORBAX_TARGET="$target"
export XLA_FLAGS="${XLA_FLAGS:---xla_force_host_platform_device_count=8}"
export WEFTFS_CLI="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

install_cmd="env TMPDIR=${tmp} PIP_CACHE_DIR=${pip_cache} python -m pip install --disable-pip-version-check --no-cache-dir --upgrade --target ${target} 'jax[cpu]' orbax-checkpoint"
performed=0
install_status=0

if env PYTHONPATH="$target" python -c 'import jax, orbax.checkpoint' >/dev/null 2>&1; then
  printf 'orbax target already importable; pip install skipped\n' > "$install_log"
else
  performed=1
  set +e
  env TMPDIR="$tmp" PIP_CACHE_DIR="$pip_cache" python -m pip install \
    --disable-pip-version-check --no-cache-dir --upgrade \
    --target "$target" 'jax[cpu]' orbax-checkpoint > "$install_log" 2>&1
  install_status=$?
  set -e
fi

export WEFT_ORBAX_INSTALL_PERFORMED="$performed"
export WEFT_ORBAX_INSTALL_STATUS="$install_status"
export WEFT_ORBAX_INSTALL_CMD="$install_cmd"

set +e
env PYTHONPATH="$PYTHONPATH" python -m pytest -q -p no:cacheprovider \
  --junitxml=/tmp/weft-orbax-junit.xml python/tests/test_orbax.py > "$tests_log" 2>&1
test_status=$?
set -e

cat "$tests_log"

export WEFT_ORBAX_TEST_STATUS="$test_status"
export WEFT_ORBAX_JUNIT="/tmp/weft-orbax-junit.xml"

set +e
env PYTHONPATH="$PYTHONPATH" python python/scripts/bench-orbax.py 2>&1 | tee "$log"
bench_status=${PIPESTATUS[0]}
set -e

exit "$bench_status"
