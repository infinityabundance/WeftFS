#!/usr/bin/env bash
# WeftFS-Go KV prefix tier parity court (subphase T068/X10).
#
#   bash bench/scripts/kv_go_parity_court.sh
#
# Host-side orchestrator: it needs two court images, so it drives
# `scripts/docker-court.sh` itself instead of running inside one court.
#
#   1. rust court: build the standalone `weftfs` oracle (no fuse feature, so the
#      binary runs in the go court, which has no libfuse3);
#   2. go court:   `go test -json` over the `TestKv*` store tests, with the Go
#      per-page cost written to a file;
#   3. host:       assemble the receipt from the test stream and the cost JSON.
#
# The assembler refuses to write a parity receipt unless the Ed25519 test, the
# bidirectional handoff test and the cost test all ran and passed.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/kv-go-parity-$stamp"
receipt="bench/receipts/kv-go-parity-$stamp.json"
log="bench/receipts/kv-go-parity-$stamp.log"
oracle="/work/.docker-cache/target-rust/release/weftfs"

skip() {
  echo "kv_go_parity_court: skip $1"
  exit 78
}

echo "kv_go_parity_court: stamp=$stamp"
echo "kv_go_parity_court: host=$(uname -srm)"

command -v docker >/dev/null 2>&1 || skip "docker CLI is not installed"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
docker image inspect "${WEFT_RUST_COURT_IMAGE:-weft/rust-court:1}" >/dev/null 2>&1 ||
  skip "court image ${WEFT_RUST_COURT_IMAGE:-weft/rust-court:1} is not present"
docker image inspect "${WEFT_GO_COURT_IMAGE:-weft/go-court:1}" >/dev/null 2>&1 ||
  skip "court image ${WEFT_GO_COURT_IMAGE:-weft/go-court:1} is not present"

mkdir -p bench/receipts "$work"
exec > >(tee "$log") 2>&1

echo "== rust oracle (no fuse) =="
bash scripts/docker-court.sh rust \
  "cd /work && cargo build --release --bin weftfs --quiet"

echo "== go tests =="
TEST_FLAGS="${WEFT_KV_GO_TEST_FLAGS:--count=1}"
bash scripts/docker-court.sh go \
  "cd /work/WeftFS-Go && WEFTFS_RUST_BIN=$oracle WEFT_KV_GO_RECEIPT=/work/.docker-cache/kv-go-parity-$stamp/cost.json timeout 900 go test -json $TEST_FLAGS -run 'TestKv' ./store/ | tee /work/.docker-cache/kv-go-parity-$stamp/go-test.json"

[ -s "$work/cost.json" ] || {
  echo "kv_go_parity_court: the Go cost test did not write its receipt" >&2
  exit 1
}
[ -s "$work/go-test.json" ] || {
  echo "kv_go_parity_court: no go test stream was captured" >&2
  exit 1
}

echo "== assemble =="
WEFT_RUST_COURT_IMAGE="${WEFT_RUST_COURT_IMAGE:-weft/rust-court:1}" \
WEFT_GO_COURT_IMAGE="${WEFT_GO_COURT_IMAGE:-weft/go-court:1}" \
  python3 bench/scripts/assemble_kv_go_parity.py \
  --stamp "$stamp" \
  --go-test "$work/go-test.json" \
  --cost "$work/cost.json" \
  --receipt "$receipt" \
  --rust-rev "$(git rev-parse HEAD)" \
  --go-rev "$(git -C WeftFS-Go rev-parse HEAD)"

echo "kv_go_parity_court: receipt $receipt"
echo "kv_go_parity_court: log $log"
