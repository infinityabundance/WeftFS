#!/usr/bin/env bash
# D1 — Buck2-style CAS adapter prototype: exact rebuild, misses, context query.
#
# Run only inside the OOM-protected Go court:
#   scripts/docker-court.sh go "bash scripts/demo-buck2-cas.sh"
#
# The adapter is a local CAS service plus a Buck2-compatible key/digest
# mapping. It is not a Buck2 plugin and does not speak REAPI gRPC.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/buck2-cas-${stamp}.json"
log="bench/receipts/buck2-cas-${stamp}.log"
work=".docker-cache/buck2/run-${stamp}"
mkdir -p "$work" bench/receipts

{
  echo "kind: buck2-cas-prototype"
  echo "timestamp_utc: ${stamp}"
  echo "court: go"
  echo "root: ${work}/cas"
  echo "revisions:"
  echo "  WeftFS: ${WEFT_REPO_WEFTFS:-unknown}"
  echo "  WeftCodec: ${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "  WeftCodec-Go: ${WEFT_REPO_WEFTCODEC_GO:-unknown}"
  echo "  WeftFS-Go: ${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "limits: memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
  echo "== go vet =="
  ( cd adapters/buck2 && go vet ./... )
  echo "== go test =="
  ( cd adapters/buck2 && go test -count=1 ./... )
  echo "== demo =="
  ( cd adapters/buck2 && go run ./cmd/buck2cas demo \
      --root "/work/${work}/cas" --receipt "/work/${receipt}" )
} 2>&1 | tee "$log"

test -s "$receipt"
grep -q '"pass": true' "$receipt"
echo "receipt: $receipt"
echo "raw log: $log"
