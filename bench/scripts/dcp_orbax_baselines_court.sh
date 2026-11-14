#!/usr/bin/env bash
# T060: the frameworks' real best save paths, and the remaining gap.
#
#   scripts/docker-court.sh python "bash bench/scripts/dcp_orbax_baselines_court.sh"
#
# Installs the Orbax/JAX stack with `pip --target` (established pattern).
# CPU only, single host. No GPU and no multi-host claim.
set -Eeuo pipefail

cd /work
stamp="${WEFT_BASE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_BASE_RECEIPT:-bench/receipts/dcp-orbax-baselines-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_BASE_WORK:-/work/.docker-cache/baselines-${stamp}}"
target="${WEFT_BASE_TARGET:-/work/.docker-cache/py-baselines}"
pip_cache="/work/.docker-cache/py-baselines-pip"
tmp="/work/.docker-cache/py-baselines-tmp"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

skip() {
  echo "dcp_orbax_baselines_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"
mkdir -p "$(dirname "$receipt")" "$work" "$target" "$pip_cache" "$tmp"
chmod -R a+rwX "$target" "$pip_cache" "$tmp" 2>/dev/null || true

if ! env PYTHONPATH="$target" python3 -c 'import jax, orbax.checkpoint' >/dev/null 2>&1; then
  echo "baselines court: installing jax[cpu] + orbax-checkpoint (pip --target)" | tee "$log"
  env TMPDIR="$tmp" PIP_CACHE_DIR="$pip_cache" python3 -m pip install \
    --disable-pip-version-check --no-cache-dir --upgrade \
    --target "$target" 'jax[cpu]' orbax-checkpoint >>"$log" 2>&1 || {
      echo "dcp_orbax_baselines_court: skip orbax install failed (see $log)" >&2
      exit 78
    }
fi

export PYTHONPATH="$target:/work/python${PYTHONPATH:+:$PYTHONPATH}"
export XLA_FLAGS="${XLA_FLAGS:---xla_force_host_platform_device_count=2}"

python3 python/courts/dcp_orbax_baselines.py run \
  --receipt "$receipt" \
  --stamp "$stamp" \
  --work-dir "$work" \
  --cli "$cli" \
  --reps "${WEFT_BASE_REPS:-8}" 2>&1 | tee -a "$log"
