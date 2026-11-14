#!/usr/bin/env bash
# KV prefix index court (subphase T063).
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/kv_index_court.sh"
#
# Measures the persisted, generation-coupled KV prefix index against the oracle
# scan on the same fixture: an end-to-end N ladder, index-only scaling past what
# the court can hold as a real store, a crash-during-build pair, and revocation.
#
# The environment decides the end-to-end ceiling: the ladder stops at the first
# rung the court's `/tmp` cannot hold, and that rung reports how many prefixes
# were durably written before it ran out. The receipt therefore carries an
# achieved count, never the requested one, as the ceiling.
#
# Reduced configuration (for `verify-all.sh`):
#   WEFT_KV_INDEX_LADDER="1000,5000"
#   WEFT_KV_INDEX_ONLY_MAX=100000
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/kv-prefix-index-$stamp.json"
log="bench/receipts/kv-prefix-index-$stamp.log"
ladder="${WEFT_KV_INDEX_LADDER:-1000,5000,10000,20000}"
index_only_max="${WEFT_KV_INDEX_ONLY_MAX:-1000000}"
min_free_mib="${WEFT_KV_INDEX_MIN_FREE_MIB:-700}"

skip() {
  echo "kv_index_court: skip $1"
  exit 78
}

echo "kv_index_court: stamp=$stamp"
echo "kv_index_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "kv_index_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "kv_index_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "kv_index_court: ladder=$ladder index-only-max=$index_only_max"

command -v cargo >/dev/null 2>&1 || skip "cargo is not available in this image"
[ -d /tmp ] || skip "no /tmp in this image"
free_mib="$(df -Pm /tmp | awk 'NR==2 {print $4}')" || true
[ -n "${free_mib:-}" ] || skip "cannot read free space for /tmp"
[ "$free_mib" -ge "$min_free_mib" ] ||
  skip "/tmp has ${free_mib} MiB free; the ladder needs at least ${min_free_mib} MiB"

mkdir -p bench/receipts
exec > >(tee "$log") 2>&1

echo "== build =="
cargo build --release --example kv_index_bench
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/kv_index_bench"

echo "== court =="
WEFT_STAMP="$stamp" timeout 1200 "$bin" \
  --ladder "$ladder" \
  --index-only-max "$index_only_max" \
  --out "$receipt"

[ -s "$receipt" ] || {
  echo "kv_index_court: receipt was not written" >&2
  exit 1
}

echo "kv_index_court: receipt $receipt"
echo "kv_index_court: log $log"
