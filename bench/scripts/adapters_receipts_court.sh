#!/usr/bin/env bash
# T059: materialization receipts on real DCP and Orbax restores.
#
#   scripts/docker-court.sh python "bash bench/scripts/adapters_receipts_court.sh"
#
# Installs the Orbax/JAX stack with `pip --target` into the court cache (the
# image is not modified), runs the court, and writes:
#   bench/receipts/adapters-receipts-<stamp>.json
#   bench/receipts/adapters-receipts-<stamp>.log
#
# CPU only, single host.
set -Eeuo pipefail

cd /work
stamp="${WEFT_ADAPTERS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_ADAPTERS_RECEIPT:-bench/receipts/adapters-receipts-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_ADAPTERS_WORK:-/work/.docker-cache/adapters-receipts-${stamp}}"
target="${WEFT_ADAPTERS_TARGET:-/work/.docker-cache/py-adapters}"
pip_cache="/work/.docker-cache/py-adapters-pip"
tmp="/work/.docker-cache/py-adapters-tmp"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

skip() {
  echo "adapters_receipts_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"

mkdir -p "$(dirname "$receipt")" "$work" "$target" "$pip_cache" "$tmp"
chmod -R a+rwX "$target" "$pip_cache" "$tmp" 2>/dev/null || true

if ! env PYTHONPATH="$target" python3 -c 'import jax, orbax.checkpoint' >/dev/null 2>&1; then
  echo "adapters-receipts court: installing jax[cpu] + orbax-checkpoint (pip --target)" | tee -a "$log"
  env TMPDIR="$tmp" PIP_CACHE_DIR="$pip_cache" python3 -m pip install \
    --disable-pip-version-check --no-cache-dir --upgrade \
    --target "$target" 'jax[cpu]' orbax-checkpoint >>"$log" 2>&1 || {
      echo "adapters_receipts_court: skip orbax install failed (see $log)" >&2
      exit 78
    }
fi

export PYTHONPATH="$target:/work/python${PYTHONPATH:+:$PYTHONPATH}"
export XLA_FLAGS="${XLA_FLAGS:---xla_force_host_platform_device_count=8}"

python3 python/courts/adapters_receipts.py run \
  --receipt "$receipt" \
  --stamp "$stamp" \
  --work-dir "$work" \
  --cli "$cli" 2>&1 | tee -a "$log"

python3 - "$receipt" <<'PY'
import json, sys
receipt = json.load(open(sys.argv[1]))
print("adapters-receipts court: %s" % receipt["result"])
sys.exit(0 if receipt["result"] == "pass" else 1)
PY
