#!/usr/bin/env bash
# KV side-channel and tenant-security court (subphase T067).
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/kv_sidechannel_court.sh"
#
# Four arms are probed round-robin against a denial that any caller can produce
# for free (an absent object): a cross-tenant read, a wrong-root grant, an
# expired grant, and a read with no envelope. Every non-denial is a hard
# failure. Timing is reported as a Mann-Whitney U z-score with the same-call
# control pair calibrating the noise floor, and the size-inference probe is
# reported with its own z. `WEFT_KV_SAMPLES` sets the per-arm count; the bench
# refuses fewer than 1,000.
set -Eeuo pipefail

cd /work
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/kv-sidechannel-$stamp.json"
log="bench/receipts/kv-sidechannel-$stamp.log"
samples="${WEFT_KV_SAMPLES:-4000}"
min_free_mib="${WEFT_KV_SIDECHANNEL_MIN_FREE_MIB:-512}"

skip() {
  echo "kv_sidechannel_court: skip $1"
  exit 78
}

echo "kv_sidechannel_court: stamp=$stamp"
echo "kv_sidechannel_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "kv_sidechannel_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "kv_sidechannel_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown}"
echo "kv_sidechannel_court: samples=$samples"

command -v cargo >/dev/null 2>&1 || skip "cargo is not available in this image"
[ -d /tmp ] || skip "no /tmp in this image"
free_mib="$(df -Pm /tmp | awk 'NR==2 {print $4}')" || true
[ -n "${free_mib:-}" ] || skip "cannot read free space for /tmp"
[ "$free_mib" -ge "$min_free_mib" ] ||
  skip "/tmp has ${free_mib} MiB free; the court needs at least ${min_free_mib} MiB"

mkdir -p bench/receipts
exec > >(tee "$log") 2>&1

echo "== build =="
cargo build --release --example kv_sidechannel
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/kv_sidechannel"

echo "== court =="
# The example exits non-zero on a hard failure (a false cross-tenant
# disclosure counts exactly like a false accept), so `set -e` fails the court
# instead of publishing a passing receipt over a leak.
WEFT_STAMP="$stamp" timeout 900 "$bin" --samples "$samples" --out "$receipt"
[ -s "$receipt" ] || {
  echo "kv_sidechannel_court: receipt was not written" >&2
  exit 1
}

echo "kv_sidechannel_court: receipt $receipt"
echo "kv_sidechannel_court: log $log"
