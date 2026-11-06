#!/usr/bin/env bash
# GCS court, inner half (subphase R14).
#
# Runs inside the OOM-protected rust court: starts the verified
# fake-gcs-server binary on loopback, waits for the port, runs the in-process
# fake GCS unit tests, then the end-to-end GCS court harness.
#
# The emulator is started *inside* the test container so the harness talks to
# it on loopback with no Docker network. It is stopped by the EXIT trap.
#
# Environment (set by bench/scripts/gcs_court.sh):
#   WEFT_GCS_BIN              path to the verified fake-gcs-server binary
#   WEFT_GCS_TEST_ENDPOINT    e.g. http://127.0.0.1:4443
#   WEFT_GCS_TEST_BUCKET      bucket name prefix for the court
#   WEFT_GCS_PROJECT          project used for bucket creation
#   WEFT_GCS_LOCK_LEASE_SECS  writer-lock lease used by the crash proxy
#   WEFT_GCS_RECEIPT          receipt path
#   WEFT_GCS_TESTS_LOG        unit-test log path
set -Eeuo pipefail

cd /work

bin="${WEFT_GCS_BIN:?WEFT_GCS_BIN is required}"
receipt="${WEFT_GCS_RECEIPT:?WEFT_GCS_RECEIPT is required}"
tests_log="${WEFT_GCS_TESTS_LOG:?WEFT_GCS_TESTS_LOG is required}"
tests_summary="${tests_log%.log}.txt"
endpoint="${WEFT_GCS_TEST_ENDPOINT:?WEFT_GCS_TEST_ENDPOINT is required}"
port="${endpoint##*:}"
port="${port%%/*}"

mkdir -p /tmp/gcs-data /tmp/gcs-root
rm -rf /tmp/gcs-data/* /tmp/gcs-root/*

"$bin" \
  -backend memory \
  -scheme http \
  -host 127.0.0.1 \
  -port "$port" \
  -external-url "$endpoint" \
  -data /tmp/gcs-data \
  -filesystem-root /tmp/gcs-root \
  -log-level warn >/tmp/fake-gcs.log 2>&1 &
server_pid=$!
cleanup() {
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
}
trap cleanup EXIT

ready=0
for _ in $(seq 1 200); do
  if (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null; then
    exec 3>&- || true
    ready=1
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "gcs_inner_court: the emulator exited before becoming ready; log follows" >&2
    cat /tmp/fake-gcs.log >&2 || true
    exit 1
  fi
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then
  echo "gcs_inner_court: the emulator never accepted a connection on 127.0.0.1:${port}" >&2
  cat /tmp/fake-gcs.log >&2 || true
  exit 1
fi
echo "gcs_inner_court: emulator ready at ${endpoint} (pid ${server_pid})"

# 1. In-process fake service unit tests. These do not need the emulator, but
#    they run here so one invocation records both.
cargo test --test gcs_backend -- --test-threads=1 >"$tests_log" 2>&1
grep -E '^(test |test result)' "$tests_log" >"$tests_summary" || true

# 2. End-to-end court over the emulator. The CLI is built first because the
#    crash proxy re-executes it with WEFT_CRASH_PHASE set.
cargo build --release --bins >>"$tests_log" 2>&1
cargo run --release --quiet --example gcs_court -- \
  --out "$receipt" \
  --root /tmp/weft-gcs-court \
  --tests-summary "$tests_summary"

echo "gcs_inner_court: receipt $receipt"
echo "gcs_inner_court: emulator log (last 20 lines)"
tail -20 /tmp/fake-gcs.log || true
