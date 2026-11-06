#!/usr/bin/env bash
# Azure court, inner half (subphase R21).
#
# Runs inside the OOM-protected rust court. The Azurite emulator itself runs as
# a sibling container (it is Node-based and is not installed in the rust court);
# `bench/scripts/azure_court.sh` starts it and passes the endpoint here.
#
# Every step runs even after an earlier failure, so one invocation records the
# whole picture; the script exits nonzero if any step failed.
#
# Environment (set by bench/scripts/azure_court.sh):
#   WEFT_AZURE_TEST_ENDPOINT    e.g. http://<azurite-container>:10000/devstoreaccount1
#   WEFT_AZURE_TEST_CONTAINER   Blob container name prefix for the court
#   WEFT_AZURE_ACCOUNT          storage account name
#   WEFT_AZURE_KEY              development account key
#   WEFT_AZURE_LOCK_LEASE_SECS  writer-lock lease used by the crash proxy
#   WEFT_AZURE_FENCE_LEASE_SECS lease used by the fencing suite (short, expireable)
#   WEFT_AZURE_RECEIPT          receipt path
#   WEFT_AZURE_TESTS_LOG        backend-test log path
#   WEFT_AZURE_FENCING_LOG      fencing-suite log path
set -Eeuo pipefail

cd /work

receipt="${WEFT_AZURE_RECEIPT:?WEFT_AZURE_RECEIPT is required}"
tests_log="${WEFT_AZURE_TESTS_LOG:?WEFT_AZURE_TESTS_LOG is required}"
tests_summary="${tests_log%.log}.txt"
fencing_log="${WEFT_AZURE_FENCING_LOG:?WEFT_AZURE_FENCING_LOG is required}"
fencing_summary="${fencing_log%.log}.txt"
export WEFT_AZURE_FENCE_LEASE_SECS="${WEFT_AZURE_FENCE_LEASE_SECS:-3}"
endpoint="${WEFT_AZURE_TEST_ENDPOINT:?WEFT_AZURE_TEST_ENDPOINT is required}"
echo "azure_inner_court: endpoint $endpoint"

# 0. Fail closed if the emulator is not reachable at all: a court that silently
#    skipped every live test because DNS was wrong would be dishonest.
rest="${endpoint#*://}"
host="${rest%%:*}"
port_part="${rest#*:}"
port="${port_part%%/*}"
if ! (exec 3<>"/dev/tcp/${host}/${port}") 2>/dev/null; then
  echo "azure_inner_court: cannot open a TCP connection to ${host}:${port}" >&2
  exit 1
fi

status=0

# 1. Backend tests: the in-process fake plus the live-emulator court.
if ! cargo test --test azure_backend -- --test-threads=1 >"$tests_log" 2>&1; then
  status=1
fi
grep -E '^(test |test result|SKIP )' "$tests_log" >"$tests_summary" || true

# 2. Fenced-writer court over the emulator. These are the same
#    backend-agnostic invariants the S3 and GCS courts run
#    (tests/support/fencing.rs); the lease is short so a test can let it expire.
if ! cargo test --test azure_fencing -- --test-threads=1 --nocapture >"$fencing_log" 2>&1; then
  status=1
fi
grep -E '^(test |test result|SKIP )' "$fencing_log" >"$fencing_summary" || true

# 3. End-to-end court over the emulator. The CLI is built first because the
#    crash proxy re-executes it with WEFT_CRASH_PHASE set.
if ! cargo build --release --bins >>"$tests_log" 2>&1; then
  echo "azure_inner_court: the release CLI did not build" >&2
  exit 1
fi
if ! cargo run --release --quiet --example azure_court -- \
  --out "$receipt" \
  --root /tmp/weft-azure-court \
  --tests-summary "$tests_summary"; then
  status=1
fi

echo "azure_inner_court: receipt $receipt"
exit "$status"
