#!/usr/bin/env bash
# Distributed correctness court (subphase R13).
#
# Runs the deterministic distributed-semantics test court and the multi-process
# harness against one real MinIO server. The harness starts the verified MinIO
# release binary itself so it can stop and restart the server process mid-run;
# `scripts/s3_court.sh --distributed` supplies the binary path and identity.
#
# What it establishes (each scenario in the receipt):
#
#   S1  single-writer interlock: 20 alternating pairs, second open is `Busy`
#   S2  a process fault at after_objects / after_commit_file /
#       after_superblock_write yields the old or the new whole state
#   S3  an at-least-once retry after a crash creates no second visible state
#   S4  8 reader processes keep the exact pinned bytes across 10 commits
#   S5  an open reader waits for a reopen before it observes a newer generation
#   S6  an unreachable endpoint, and a stopped MinIO process, fail closed with
#       a transport error; a later reopen verifies
#   S7  `gc --apply` racing a snapshot reader deletes only garbage
#   S8  a known content hash is not authority under an authenticated envelope
#   S9  S2/S4/S5 repeated for `--blob-backend s3 --commit-backend local`
#   S10 fenced writer protocol: a SIGSTOPped writer is reclaimed and cannot
#       publish after SIGCONT; release racing a reclaim; bounded renewal; an
#       expired lease that blocks publication with no successor
#
# Run inside the OOM-protected bench court:
#
#   bash scripts/s3_court.sh --distributed
#
# or directly, with a verified MinIO binary:
#
#   bash scripts/docker-court.sh bench \
#     "WEFT_MINIO_BIN=/work/.docker-cache/minio/minio \
#      bash bench/scripts/distributed_court.sh"
#
# Nothing here is a fleet, cloud, multi-node, physical-media, or power-loss
# claim. There is no TCP-level partition beyond stopping the server process and
# an endpoint with no listener. Writers are exclusive by design.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_DIST_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/distributed-${stamp}.json"
log="bench/receipts/distributed-${stamp}.log"
tests_log="bench/receipts/distributed-${stamp}.tests.log"
tests_summary="bench/receipts/distributed-${stamp}.tests.txt"
work="${WEFT_DIST_ROOT:-/tmp/weft-distributed-court}"

minio_bin="${WEFT_MINIO_BIN:-/work/.docker-cache/minio/minio}"
if [[ ! -f "$minio_bin" ]]; then
  echo "distributed_court: MinIO binary '$minio_bin' is missing; run scripts/s3_court.sh --distributed" >&2
  exit 78
fi
chmod +x "$minio_bin" 2>/dev/null || true
if [[ -n "${WEFT_S3_SERVER_SHA256:-}" ]]; then
  actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
  if [[ "$actual_sha256" != "$WEFT_S3_SERVER_SHA256" ]]; then
    echo "distributed_court: MinIO binary sha256 mismatch (expected $WEFT_S3_SERVER_SHA256, got $actual_sha256)" >&2
    exit 1
  fi
fi

{
  echo "== distributed correctness court =="
  echo "utc=${stamp}"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown} nproc=${WEFT_COURT_NPROC:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "revision_weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "minio_bin=${minio_bin}"
  echo "minio_sha256=${WEFT_S3_SERVER_SHA256:-unknown}"
  echo "work_root=${work}"
  echo
} | tee "$log"

# 1. Always-on semantics court. The contract must not depend only on this
#    shell harness.
tests_rc=0
if cargo test --release --test distributed_semantics -- --test-threads=1 >"$tests_log" 2>&1; then
  tests_rc=0
else
  tests_rc=1
fi
cat "$tests_log" >>"$log"
grep -E '^(test |test result)' "$tests_log" >"$tests_summary" || true

echo "---- distributed semantics test summary ----" | tee -a "$log"
cat "$tests_summary" | tee -a "$log"

if [[ "$tests_rc" != 0 ]]; then
  echo "distributed_court: the deterministic semantics court failed; see $tests_log" | tee -a "$log" >&2
  exit 1
fi

# 2. The multi-process harness.
cargo build --release --bin weftfs --example distributed_court >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
set +e
"$target/release/examples/distributed_court" \
  --out "$receipt" \
  --root "$work" \
  --stamp "$stamp" \
  --weftfs-bin "$target/release/weftfs" \
  --minio-bin "$minio_bin" \
  --tests-summary "$tests_summary" 2>&1 | tee -a "$log"
harness_rc=${PIPESTATUS[0]}
set -e

if [[ ! -f "$receipt" ]]; then
  echo "distributed_court: no receipt was written; the harness failed" | tee -a "$log" >&2
  exit 1
fi
if ! grep -Fq '"result": "pass"' "$receipt"; then
  echo "distributed_court: receipt does not report pass" | tee -a "$log" >&2
  harness_rc=1
fi

echo "distributed receipt: $receipt" | tee -a "$log"
echo "raw log: $log" | tee -a "$log"
sha256sum "$receipt" "$log" "$tests_summary" bench/receipts/distributed-"${stamp}".s*.log 2>/dev/null | tee -a "$log" || true

if [[ "$harness_rc" != 0 ]]; then
  echo "distributed_court: the harness reported failures; see $log" >&2
  exit 1
fi
