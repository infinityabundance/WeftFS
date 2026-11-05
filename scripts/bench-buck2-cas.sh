#!/usr/bin/env bash
# P6 — store-backed Buck2-style CAS benchmark.
#
# Run only inside the OOM-protected bench (FUSE-capable) court:
#   scripts/docker-court.sh bench "bash scripts/bench-buck2-cas.sh"
#
# 1,000 small-output actions, 100 x 1 MiB actions. Latency is the adapter
# library path with one open writer; the mount read and CLI fetch include the
# real filesystem/process boundaries and are labelled separately.
set -Eeuo pipefail

ROOT=/work
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WORK="$ROOT/.docker-cache/buck2-bench/$STAMP"
STORE="$WORK/store"
RECEIPT="$ROOT/bench/receipts/buck2-cas-store-$STAMP.json"
LOG="$ROOT/bench/receipts/buck2-cas-store-$STAMP.log"
MNT="/mnt/weft-fuse/buck2-bench-$STAMP"
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
  echo "kind: buck2-cas-store-bench"
  echo "timestamp_utc: $STAMP"
  echo "court: bench"
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

  echo "== benchmark =="
  "$BUCK2CAS" bench --root "$STORE" --small 1000 --large 100 --out "$RECEIPT"

  echo "== mount a 1 MiB artifact =="
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
  test -f "$MNT/$EXPECTED_NAME"
  : > "$WORK/mount-reads.txt"
  for i in 1 2 3 4 5; do
    start="$(date +%s%N)"
    cat "$MNT/$EXPECTED_NAME" > "$WORK/mount-read-$i.bin"
    end="$(date +%s%N)"
    echo "$((end - start))" >> "$WORK/mount-reads.txt"
  done
  actual_sha="$(sha256sum "$WORK/mount-read-1.bin" | awk '{print $1}')"
  test "$actual_sha" = "$OUTPUT_SHA256"
  test "$(wc -c < "$WORK/mount-read-1.bin")" -eq "$OUTPUT_BYTES"

  echo "== CLI fetch (process boundary) =="
  : > "$WORK/cli-fetch.txt"
  for _ in 1 2 3 4 5; do
    start="$(date +%s%N)"
    "$BUCK2CAS" fetch --root "$STORE" --key "$ACTION_KEY" --out "$WORK/cli-fetch.bin" >/dev/null
    end="$(date +%s%N)"
    echo "$((end - start))" >> "$WORK/cli-fetch.txt"
  done
  cmp -s "$WORK/cli-fetch.bin" "$WORK/mount-read-1.bin"

  fusermount3 -u "$MNT"
  for _ in $(seq 1 200); do
    mountpoint -q "$MNT" 2>/dev/null || break
    sleep 0.05
  done
  ! mountpoint -q "$MNT" 2>/dev/null
  wait "$MOUNT_PID" 2>/dev/null || true
  MOUNT_PID=""

  echo "== merge mount evidence =="
  python3 - "$RECEIPT" "$WORK" "$EXPECTED_NAME" "$OUTPUT_SHA256" <<'PY'
import hashlib, json, os, statistics, sys

receipt_path, work, expected_name, expected_sha = sys.argv[1:5]
receipt = json.load(open(receipt_path))

def read_ints(name):
    return [int(line.strip()) for line in open(os.path.join(work, name)) if line.strip()]

def summary(values):
    values = sorted(values)
    def pick(p):
        index = int(p * len(values) + 0.999999)
        return values[max(1, min(index, len(values))) - 1]
    return {
        "count": len(values),
        "min_ns": values[0],
        "median_ns": pick(0.50),
        "p95_ns": pick(0.95),
        "max_ns": values[-1],
        "mean_ns": statistics.mean(values),
        "runs_ns": values,
    }

mount_reads = read_ints("mount-reads.txt")
cli_fetches = read_ints("cli-fetch.txt")
with open(os.path.join(work, "mount-read-1.bin"), "rb") as handle:
    data = handle.read()
receipt["mount_read"] = {
    "listing": [line.strip() for line in open(os.path.join(work, "mount-listing.txt")) if line.strip()],
    "expected_name": expected_name,
    "mount_read": summary(mount_reads),
    "cli_fetch": summary(cli_fetches),
    "sha256_match": hashlib.sha256(data).hexdigest() == expected_sha,
    "bytes": len(data),
}
json.dump(receipt, open(receipt_path, "w"), indent=2, sort_keys=False)
open(receipt_path, "a").write("\n")
print(f"mount read: {summary(mount_reads)}")
if not receipt["mount_read"]["sha256_match"]:
    raise SystemExit("mount read SHA-256 mismatch")
PY
} 2>&1 | tee "$LOG"

test -s "$RECEIPT"
echo "receipt: $RECEIPT"
echo "raw log: $LOG"
