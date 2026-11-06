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
#   WEFT_GCS_FENCE_LEASE_SECS lease used by the fencing suite (short, expireable)
#   WEFT_GCS_RECEIPT          receipt path
#   WEFT_GCS_TESTS_LOG        unit-test log path
#   WEFT_GCS_FENCING_LOG      fencing-suite log path
set -Eeuo pipefail

cd /work

status=0
auth_log="${WEFT_GCS_TESTS_LOG%.log}.auth.log"
auth_summary="${auth_log%.log}.txt"
auth_probe_log="${WEFT_GCS_TESTS_LOG%.log}.auth-probe.log"
bin="${WEFT_GCS_BIN:?WEFT_GCS_BIN is required}"
receipt="${WEFT_GCS_RECEIPT:?WEFT_GCS_RECEIPT is required}"
tests_log="${WEFT_GCS_TESTS_LOG:?WEFT_GCS_TESTS_LOG is required}"
tests_summary="${tests_log%.log}.txt"
fencing_log="${WEFT_GCS_FENCING_LOG:?WEFT_GCS_FENCING_LOG is required}"
fencing_summary="${fencing_log%.log}.txt"
export WEFT_GCS_FENCE_LEASE_SECS="${WEFT_GCS_FENCE_LEASE_SECS:-3}"
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
if ! cargo test --test gcs_backend -- --test-threads=1 >"$tests_log" 2>&1; then
  status=1
fi
grep -E '^(test |test result)' "$tests_log" >"$tests_summary" || true

# 1b. Service-account authentication court. Offline by design: the RS256
#     implementation is checked against the published RFC 7515 A.2 vector and
#     the OAuth2 exchange/cache against an in-process endpoint. A real key is
#     needed only for the optional live probe below.
if ! cargo test --test service_account -- --test-threads=1 >"$auth_log" 2>&1; then
  status=1
fi
grep -E '^(test |test result)' "$auth_log" >"$auth_summary" || true

# 1c. Optional live token exchange. Only when the court was given a real key.
rm -f "$auth_probe_log"
if [[ -n "${WEFT_GCS_CREDENTIALS:-}" && "${WEFT_GCS_LIVE_PROBE:-0}" == "1" ]]; then
  if ! cargo run --release --quiet --example gcs_auth_probe >"$auth_probe_log" 2>&1; then
    status=1
  fi
fi

# 2. Fenced-writer court over the emulator. These are the same backend-agnostic
#    invariants the S3 and Ceph RGW courts run (tests/support/fencing.rs); the
#    lease is short so a test can let it expire.
if ! cargo test --test gcs_fencing -- --test-threads=1 --nocapture >"$fencing_log" 2>&1; then
  status=1
fi
grep -E '^(test |test result)' "$fencing_log" >"$fencing_summary" || true

# 3. End-to-end court over the emulator. The CLI is built first because the
#    crash proxy re-executes it with WEFT_CRASH_PHASE set.
if ! cargo build --release --bins >>"$tests_log" 2>&1; then
  echo "gcs_inner_court: the release CLI did not build" >&2
  exit 1
fi
if ! cargo run --release --quiet --example gcs_court -- \
  --out "$receipt" \
  --root /tmp/weft-gcs-court \
  --tests-summary "$tests_summary"; then
  status=1
fi

echo "gcs_inner_court: auth log $auth_log"
echo "gcs_inner_court: receipt $receipt"
echo "gcs_inner_court: emulator log (last 20 lines)"
tail -20 /tmp/fake-gcs.log || true
exit "$status"
