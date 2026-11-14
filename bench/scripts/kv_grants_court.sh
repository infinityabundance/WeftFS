#!/usr/bin/env bash
# KV page batching and verified-session grant cache court (subphase T065).
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/kv_grants_court.sh"
#
# Publishes the same 1,024-prefix set twice (single-record path and one batch
# commit) and compares the durable record set, then measures a 128 x 4 KiB page
# stream with the grant cache on and off, and the cost of a commit invalidating
# a cached grant.
set -Eeuo pipefail

cd /work
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/kv-grants-$stamp.json"
log="bench/receipts/kv-grants-$stamp.log"
min_free_mib="${WEFT_KV_GRANTS_MIN_FREE_MIB:-256}"

skip() {
  echo "kv_grants_court: skip $1"
  exit 78
}

echo "kv_grants_court: stamp=$stamp"
echo "kv_grants_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "kv_grants_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "kv_grants_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown}"

command -v cargo >/dev/null 2>&1 || skip "cargo is not available in this image"
[ -d /tmp ] || skip "no /tmp in this image"
free_mib="$(df -Pm /tmp | awk 'NR==2 {print $4}')" || true
[ -n "${free_mib:-}" ] || skip "cannot read free space for /tmp"
[ "$free_mib" -ge "$min_free_mib" ] ||
  skip "/tmp has ${free_mib} MiB free; the court needs at least ${min_free_mib} MiB"

mkdir -p bench/receipts
exec > >(tee "$log") 2>&1

echo "== build =="
cargo build --release --example kv_grants_bench
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/kv_grants_bench"

echo "== court =="
WEFT_STAMP="$stamp" timeout 600 "$bin" --out "$receipt"
[ -s "$receipt" ] || {
  echo "kv_grants_court: receipt was not written" >&2
  exit 1
}

echo "kv_grants_court: receipt $receipt"
echo "kv_grants_court: log $log"
