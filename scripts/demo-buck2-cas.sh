#!/usr/bin/env bash
# P6 — store-backed Buck2-style CAS correctness court.
#
# Run only inside the OOM-protected FUSE court (Rust + Go + FUSE):
#   scripts/docker-court.sh fuse "bash scripts/demo-buck2-cas.sh"
#
# The adapter is a local action cache on the WeftFS store. It is NOT a Buck2
# plugin, worker, or REAPI gRPC server, and Buck2 itself is not exercised.
set -Eeuo pipefail

ROOT=/work
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WORK="$ROOT/.docker-cache/buck2/$STAMP"
STORE="$WORK/store"
RECEIPT="$ROOT/bench/receipts/buck2-cas-store-correctness-$STAMP.json"
LOG="$ROOT/bench/receipts/buck2-cas-store-correctness-$STAMP.log"
MNT="/mnt/weft-fuse/buck2-$STAMP"
RUSTFS="${CARGO_TARGET_DIR:-$ROOT/.docker-cache/target-rust}/release/weftfs"
BUCK2CAS="$WORK/buck2cas"
FIXED_TS=1767225600000000000
MOUNT_PID=""

mkdir -p "$WORK" "$ROOT/bench/receipts"

cleanup() {
  if [ -n "$MOUNT_PID" ] && kill -0 "$MOUNT_PID" 2>/dev/null; then
    fusermount3 -u "$MNT" 2>/dev/null || umount -l "$MNT" 2>/dev/null || true
    wait "$MOUNT_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

{
  echo "kind: buck2-cas-store"
  echo "timestamp_utc: $STAMP"
  echo "court: fuse"
  echo "revisions:"
  echo "  WeftFS: ${WEFT_REPO_WEFTFS:-unknown}"
  echo "  WeftCodec: ${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "  WeftCodec-Go: ${WEFT_REPO_WEFTCODEC_GO:-unknown}"
  echo "  WeftFS-Go: ${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "limits: memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown} nproc=${WEFT_COURT_NPROC:-unknown}"
  echo "kernel: ${WEFT_COURT_KERNEL:-unknown}"
  echo "image: ${WEFT_COURT_IMAGE:-unknown} ${WEFT_COURT_IMAGE_ID:-unknown}"

  echo "== build rust oracle =="
  ( cd "$ROOT" && cargo build --release --quiet --features fuse --bin weftfs )
  test -x "$RUSTFS"

  echo "== build adapter =="
  ( cd "$ROOT/adapters/buck2" && go build -o "$BUCK2CAS" ./cmd/buck2cas )

  echo "== go vet + go test (cross-language included) =="
  ( cd "$ROOT/adapters/buck2" && go vet ./... && WEFTFS_RUST_BIN="$RUSTFS" go test -count=1 ./... )

  echo "== adapter correctness demo =="
  "$BUCK2CAS" demo-store --root "$STORE" --receipt "$RECEIPT" --rust-bin "$RUSTFS"

  echo "== mount by:action=<key> =="
  # shellcheck disable=SC1090
  source "$RECEIPT.mount.env"
  mkdir -p "$MNT"
  "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" mount \
    --mountpoint "$MNT" --foreground --context "$CONTEXT_NAME" \
    >"$WORK/mount.log" 2>&1 &
  MOUNT_PID=$!
  mounted=0
  for _ in $(seq 1 300); do
    if mountpoint -q "$MNT" 2>/dev/null; then mounted=1; break; fi
    if ! kill -0 "$MOUNT_PID" 2>/dev/null; then break; fi
    sleep 0.05
  done
  if [ "$mounted" -ne 1 ]; then
    echo "mount failed; daemon log:" >&2
    cat "$WORK/mount.log" >&2 || true
    exit 1
  fi

  ls -1 "$MNT" > "$WORK/mount-listing.txt"
  cat "$WORK/mount-listing.txt"
  test -f "$MNT/$EXPECTED_NAME"

  : > "$WORK/mount-reads.txt"
  for i in 1 2 3 4 5; do
    start="$(date +%s%N)"
    cat "$MNT/$EXPECTED_NAME" > "$WORK/mount-read-$i.bin"
    end="$(date +%s%N)"
    echo "$((end - start))" >> "$WORK/mount-reads.txt"
    cmp -s "$WORK/mount-read-$i.bin" "$WORK/mount-read-1.bin"
  done
  actual_sha="$(sha256sum "$WORK/mount-read-1.bin" | awk '{print $1}')"
  test "$actual_sha" = "$OUTPUT_SHA256"
  test "$(wc -c < "$WORK/mount-read-1.bin")" -eq "$OUTPUT_BYTES"

  echo "== CLI fetch from the same store =="
  : > "$WORK/cli-fetch.txt"
  for _ in 1 2 3; do
    start="$(date +%s%N)"
    "$BUCK2CAS" fetch --root "$STORE" --key "$ACTION_KEY" --out "$WORK/cli-fetch.bin"
    end="$(date +%s%N)"
    echo "$((end - start))" >> "$WORK/cli-fetch.txt"
  done
  cmp -s "$WORK/cli-fetch.bin" "$WORK/mount-read-1.bin"

  echo "== CLI verify =="
  "$BUCK2CAS" verify --root "$STORE" --key "$ACTION_KEY" --blob "$WORK/mount-read-1.bin"

  echo "== unmount =="
  fusermount3 -u "$MNT"
  for _ in $(seq 1 200); do
    mountpoint -q "$MNT" 2>/dev/null || break
    sleep 0.05
  done
  ! mountpoint -q "$MNT" 2>/dev/null
  wait "$MOUNT_PID" 2>/dev/null || true
  MOUNT_PID=""

  echo "== merge mount evidence into the receipt =="
  python3 - "$RECEIPT" "$WORK" "$EXPECTED_NAME" "$OUTPUT_SHA256" <<'PY'
import json, os, statistics, sys

receipt_path, work, expected_name, expected_sha = sys.argv[1:5]
with open(receipt_path) as handle:
    receipt = json.load(handle)

def read_ints(name):
    with open(os.path.join(work, name)) as handle:
        return [int(line.strip()) for line in handle if line.strip()]

mount_reads = read_ints("mount-reads.txt")
cli_fetches = read_ints("cli-fetch.txt")
listing = [line.strip() for line in open(os.path.join(work, "mount-listing.txt")) if line.strip()]
visible = [name for name in listing if not name.startswith(".")]
sha_match = os.path.getsize(os.path.join(work, "mount-read-1.bin")) == receipt["mount"]["bytes"]
with open(os.path.join(work, "mount-read-1.bin"), "rb") as handle:
    import hashlib
    sha_match = sha_match and hashlib.sha256(handle.read()).hexdigest() == expected_sha

def summary(values):
    values = sorted(values)
    def pick(p):
        index = int(p * len(values) + 0.999999)
        index = max(1, min(index, len(values)))
        return values[index - 1]
    return {
        "count": len(values),
        "min_ns": values[0],
        "median_ns": pick(0.50),
        "p95_ns": pick(0.95),
        "max_ns": values[-1],
        "mean_ns": statistics.mean(values),
        "runs_ns": values,
    }

receipt["mount_read"] = {
    "listing": listing,
    "expected_name": expected_name,
    "mount_read": summary(mount_reads),
    "cli_fetch": summary(cli_fetches),
    "sha256_match": sha_match,
    "bytes_match": os.path.getsize(os.path.join(work, "mount-read-1.bin")) == receipt["mount"]["bytes"],
}
receipt["assertions"].extend([
    {"name": "mount lists exactly the action output", "pass": visible == [expected_name],
     "detail": f"visible={visible}"},
    {"name": "mount read bytes match the recorded SHA-256", "pass": sha_match,
     "detail": expected_sha},
    {"name": "CLI fetch bytes equal the mount-read bytes", "pass": True, "detail": "cmp exact"},
])
passed = sum(1 for item in receipt["assertions"] if item["pass"])
total = len(receipt["assertions"])
receipt["result"]["passed"] = passed
receipt["result"]["total"] = total
receipt["result"]["pass"] = passed == total
with open(receipt_path, "w") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=False)
    handle.write("\n")
print(f"mount merged: {passed}/{total} assertions pass")
if passed != total:
    sys.exit(1)
PY
} 2>&1 | tee "$LOG"

test -s "$RECEIPT"
python3 - "$RECEIPT" <<'PY'
import json, sys
receipt = json.load(open(sys.argv[1]))
if not receipt["result"]["pass"]:
    raise SystemExit("receipt result is not pass")
print(f"receipt: {sys.argv[1]} ({receipt['result']['passed']}/{receipt['result']['total']})")
PY
echo "raw log: $LOG"
