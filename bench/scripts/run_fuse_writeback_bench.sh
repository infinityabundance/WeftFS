#!/usr/bin/env bash
# FUSE write-back / group-commit benchmark and crash proxy.
#
# Run ONLY inside the FUSE-capable court:
#   scripts/docker-court.sh fuse "bash bench/scripts/run_fuse_writeback_bench.sh"
#
# Cells, for both independent frontends (Rust, Go):
#   * 500 x 4 KiB overwrites of a 128 MiB file, one fsync at the end, for
#     whole-file and Merkle identity, with no option, --write-back,
#     --group-commit, and both together.
#   * A 200-file / 20-directory tree: create/stat/readdir/unlink, no option vs
#     --group-commit.
#   * A process-fault crash proxy: kill the daemon while writes are buffered or
#     metadata is grouped, then reopen and verify the store is old-or-new whole.
#
# Outputs:
#   bench/receipts/fuse-writeback-<UTC>.json
#   bench/receipts/fuse-writeback-<UTC>.log
set -Eeuo pipefail

ROOT=/work
SCRATCH="${WEFT_WB_SCRATCH:-$ROOT/.docker-cache/fuse-writeback}"
RUN="$SCRATCH/run"
RAW="$RUN/raw"
RECEIPTS="$ROOT/bench/receipts"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BASE="${WEFT_WB_RECEIPT_NAME:-fuse-writeback-$STAMP}"
RECEIPT="$RECEIPTS/$BASE.json"
LOG="$RECEIPTS/$BASE.log"
TS=1767225600000000000
COUNT="${WEFT_WB_OVERWRITES:-500}"
BLOCK="${WEFT_WB_BLOCK:-4096}"
FILE_BYTES="${WEFT_WB_FILE_BYTES:-128M}"
DIRS="${WEFT_WB_DIRS:-20}"
PER_DIR="${WEFT_WB_PER_DIR:-10}"
REPS="${WEFT_WB_REPS:-1}"

RUST_BIN="$ROOT/.docker-cache/target-rust/release/weftfs"
GO_BIN="$SCRATCH/bin/go-weftfs"
HELPER="$SCRATCH/bin/wbops"

mkdir -p "$RAW" "$RECEIPTS" "$SCRATCH/bin"
rm -f "$RAW"/*
: > "$RAW/cells.jsonl"
: > "$RAW/crash.jsonl"
: > "$RAW/commands.txt"

exec > >(tee -a "$LOG") 2>&1
PS4='+ '
set -x

log() { printf '%s\n' "$*" >&2; }
record() { printf '%s\n' "$*" >> "$RAW/commands.txt"; }

MNT=""
MOUNT_PID=""
HELPER_PIDS=()

unmount_now() {
  local mnt="$1"
  if mountpoint -q "$mnt" 2>/dev/null; then
    fusermount3 -u "$mnt" 2>/dev/null || umount -l "$mnt" 2>/dev/null || true
  fi
  local tries=0
  while mountpoint -q "$mnt" 2>/dev/null && [ "$tries" -lt 50 ]; do
    umount -l "$mnt" 2>/dev/null || true
    tries=$((tries + 1))
    sleep 0.1
  done
}

cleanup() {
  set +e
  for pid in "${HELPER_PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null; done
  if [ -n "$MOUNT_PID" ]; then
    kill -TERM "$MOUNT_PID" 2>/dev/null
    wait "$MOUNT_PID" 2>/dev/null
  fi
  [ -n "$MNT" ] && unmount_now "$MNT"
  find "$SCRATCH" -maxdepth 2 -type d -name 'mnt-*' -exec sh -c 'umount -l "$1" 2>/dev/null || true' _ {} \; 2>/dev/null
}
trap cleanup EXIT

start_mount() {
  local cli="$1" store="$2" mnt="$3" name="$4" impl="$5"
  shift 5
  local extra=()
  # The Rust CLI keeps the session in the foreground explicitly; the Go CLI has
  # no daemon mode, so it does not accept the flag.
  [ "$impl" = "rust" ] && extra+=(--foreground)
  mkdir -p "$mnt"
  record "$cli --store $store --fixed-timestamp-ns $TS mount --mountpoint $mnt ${extra[*]} $*"
  "$cli" --store "$store" --fixed-timestamp-ns "$TS" mount --mountpoint "$mnt" "${extra[@]}" "$@" \
    >"$RUN/mount-$name.log" 2>&1 &
  MOUNT_PID=$!
  local tries=0
  while ! mountpoint -q "$mnt" 2>/dev/null; do
    if ! kill -0 "$MOUNT_PID" 2>/dev/null; then
      log "$name mount daemon exited before the mount appeared"
      cat "$RUN/mount-$name.log" >&2 || true
      return 1
    fi
    tries=$((tries + 1))
    if [ "$tries" -gt 300 ]; then
      log "$name mount did not appear within 30s"
      cat "$RUN/mount-$name.log" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  sleep 0.2
}

stop_mount() {
  local mnt="$1"
  local rss=""
  if [ -n "$MOUNT_PID" ] && [ -r "/proc/$MOUNT_PID/status" ]; then
    rss="$(awk '/^VmHWM:/ {print $2}' "/proc/$MOUNT_PID/status")"
  fi
  if [ -n "$MOUNT_PID" ]; then
    kill -TERM "$MOUNT_PID" 2>/dev/null || true
    wait "$MOUNT_PID" 2>/dev/null || true
  fi
  unmount_now "$mnt"
  MOUNT_PID=""
  printf '%s' "$rss"
}

verify_store() {
  local cli="$1" store="$2" out="$3"
  record "$cli --store $store --json verify --full"
  "$cli" --store "$store" --json verify --full >"$RAW/$out.json" 2>"$RAW/$out.err"
}

info_json() {
  local cli="$1" store="$2"
  "$cli" --store "$store" --json info 2>/dev/null
}

run_cell() {
  local impl="$1" cli="$2" identity="$3" mode="$4" rep="$5"
  local tag="$impl-$identity-$mode-r$rep"
  local store="$RUN/store-$tag"
  local mnt="$RUN/mnt-$tag"
  rm -rf "$store" "$mnt"
  mkdir -p "$mnt"
  "$cli" --store "$store" init >/dev/null
  local flags=(--content-identity "$identity")
  case "$mode" in
    off) ;;
    write-back) flags+=(--write-back) ;;
    group-commit) flags+=(--group-commit) ;;
    write-back+group) flags+=(--write-back --group-commit) ;;
    *) log "unknown mode $mode"; return 1 ;;
  esac
  start_mount "$cli" "$store" "$mnt" "$tag" "$impl" "${flags[@]}"
  MNT="$mnt"

  local gen_before
  gen_before="$(info_json "$cli" "$store" | python3 -c 'import json,sys; print(json.load(sys.stdin)["generation"])')"

  record "truncate -s $FILE_BYTES $mnt/big.bin"
  truncate -s "$FILE_BYTES" "$mnt/big.bin"

  record "$HELPER overwrite $mnt/big.bin $COUNT $BLOCK end 12345"
  "$HELPER" overwrite "$mnt/big.bin" "$COUNT" "$BLOCK" end 12345 >"$RAW/over-$tag.json"

  record "$HELPER meta $mnt/tree $DIRS $PER_DIR"
  "$HELPER" meta "$mnt/tree" "$DIRS" "$PER_DIR" >"$RAW/meta-$tag.json"

  local gen_after
  gen_after="$(info_json "$cli" "$store" | python3 -c 'import json,sys; print(json.load(sys.stdin)["generation"])')"

  local rss
  rss="$(stop_mount "$mnt")"
  MNT=""

  verify_store "$cli" "$store" "verify-$tag"
  local ok
  ok="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("ok", d.get("OK")))' "$RAW/verify-$tag.json")"
  local bytes
  bytes="$(du -sb "$store" | cut -f1)"

  python3 - "$RAW/cells.jsonl" "$RAW/over-$tag.json" "$RAW/meta-$tag.json" <<PY
import json, sys
cells, over, meta = sys.argv[1], sys.argv[2], sys.argv[3]
row = {
    "impl": "$impl", "identity": "$identity", "mode": "$mode", "rep": $rep,
    "overwrite": json.load(open(over)),
    "meta": json.load(open(meta)),
    "generation_before": int("$gen_before"),
    "generation_after": int("$gen_after"),
    "mount_peak_rss_kib": int("$rss" or 0),
    "store_bytes": int("$bytes"),
    "verify_ok": "$ok" == "True",
}
with open(cells, "a") as fh:
    fh.write(json.dumps(row) + "\n")
PY
  # Keep disk bounded: the receipt already holds the counters and the verify
  # result; the store itself is not part of the artifact.
  rm -rf "$store"
}

# ---- crash proxy --------------------------------------------------------

expected_hash() {
  local byte="$1"
  python3 - "$byte" <<'PY'
import hashlib, sys
print(hashlib.sha256(bytes([int(sys.argv[1], 0)]) * 65536).hexdigest())
PY
}

crash_case() {
  local impl="$1" cli="$2" phase="$3"
  local tag="crash-$impl-$phase"
  local store="$RUN/store-$tag"
  local mnt="$RUN/mnt-$tag"
  rm -rf "$store" "$mnt"
  mkdir -p "$mnt"
  "$cli" --store "$store" init >/dev/null
  start_mount "$cli" "$store" "$mnt" "$tag" "$impl" --content-identity merkle --write-back --group-commit
  MNT="$mnt"
  "$HELPER" crashwrite "$mnt/crash.bin" "$phase" >"$RAW/$tag.json" 2>"$RAW/$tag.err" &
  HELPER_PIDS+=($!)
  # Wait until the writer has reported the stage we care about.
  local want="new-sent"
  [ "$phase" = "old" ] && want="old-durable"
  local tries=0
  while ! grep -q "$want" "$RAW/$tag.json" 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -gt 100 ] && { log "$tag: helper did not reach $want"; return 1; }
    sleep 0.1
  done
  sleep 0.2
  kill -9 "$MOUNT_PID" 2>/dev/null || true
  wait "$MOUNT_PID" 2>/dev/null || true
  MOUNT_PID=""
  unmount_now "$mnt"
  MNT=""

  verify_store "$cli" "$store" "verify-$tag"
  "$cli" --store "$store" cat /crash.bin > "$RAW/$tag.out" 2>"$RAW/$tag.cat.err" || true
  local got expected kind
  got="$(sha256sum "$RAW/$tag.out" | cut -d' ' -f1)"
  if cmp -s "$RAW/$tag.out" <(python3 -c 'import sys; sys.stdout.buffer.write(bytes([0x11])*65536)'); then
    kind="old"
  elif cmp -s "$RAW/$tag.out" <(python3 -c 'import sys; sys.stdout.buffer.write(bytes([0x22])*65536)'); then
    kind="new"
  else
    kind="partial-or-damaged"
  fi
  expected="$(if [ "$phase" = "durable" ]; then expected_hash 0x22; else expected_hash 0x11; fi)"
  local ok
  ok="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("ok", d.get("OK")))' "$RAW/verify-$tag.json")"
  python3 - "$RAW/crash.jsonl" <<PY
import json, sys
row = {
    "impl": "$impl", "case": "write", "phase": "$phase",
    "observed": "$kind", "sha256": "$got", "expected_sha256": "$expected",
    "verify_ok": "$ok" == "True",
    "pass": "$ok" == "True" and "$kind" == ("new" if "$phase" == "durable" else "old"),
}
with open(sys.argv[1], "a") as fh:
    fh.write(json.dumps(row) + "\n")
PY
}

crash_meta() {
  local impl="$1" cli="$2"
  local tag="crashmeta-$impl"
  local store="$RUN/store-$tag"
  local mnt="$RUN/mnt-$tag"
  rm -rf "$store" "$mnt"
  mkdir -p "$mnt"
  "$cli" --store "$store" init >/dev/null
  start_mount "$cli" "$store" "$mnt" "$tag" "$impl" --content-identity merkle --group-commit
  MNT="$mnt"
  "$HELPER" crashmeta "$mnt" 40 >"$RAW/$tag.create.json" 2>"$RAW/$tag.err" &
  HELPER_PIDS+=($!)
  local tries=0
  while ! grep -q '"files":40' "$RAW/$tag.create.json" 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -gt 100 ] && { log "$tag: helper did not finish"; return 1; }
    sleep 0.1
  done
  kill -9 "$MOUNT_PID" 2>/dev/null || true
  wait "$MOUNT_PID" 2>/dev/null || true
  MOUNT_PID=""
  unmount_now "$mnt"
  MNT=""
  verify_store "$cli" "$store" "verify-$tag"
  local count ok
  count="$("$cli" --store "$store" ls / 2>/dev/null | grep -c '^c-' || true)"
  ok="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("ok", d.get("OK")))' "$RAW/verify-$tag.json")"
  python3 - "$RAW/crash.jsonl" <<PY
import json, sys
row = {
    "impl": "$impl", "case": "metadata", "phase": "grouped-creates",
    "observed_files": int("$count"), "expected": "0 or 40",
    "verify_ok": "$ok" == "True",
    "pass": "$ok" == "True" and int("$count") in (0, 40),
}
with open(sys.argv[1], "a") as fh:
    fh.write(json.dumps(row) + "\n")
PY
}

# ---- build --------------------------------------------------------------

record "cargo build --release --features fuse --bin weftfs"
cargo build --release --features fuse --bin weftfs
record "(cd WeftFS-Go && CGO_ENABLED=0 go build -o $GO_BIN ./cmd/weftfs)"
(cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$GO_BIN" ./cmd/weftfs)
record "CGO_ENABLED=0 go build -o $HELPER bench/scripts/fuse_writeback_ops.go"
CGO_ENABLED=0 go build -o "$HELPER" "$ROOT/bench/scripts/fuse_writeback_ops.go"

# ---- cells --------------------------------------------------------------

for impl in rust go; do
  if [ "$impl" = "rust" ]; then cli="$RUST_BIN"; else cli="$GO_BIN"; fi
  for identity in whole-file merkle; do
    for mode in off write-back group-commit write-back+group; do
      for rep in $(seq 1 "$REPS"); do
        run_cell "$impl" "$cli" "$identity" "$mode" "$rep"
      done
    done
  done
done

# ---- crash proxy --------------------------------------------------------

for impl in rust go; do
  if [ "$impl" = "rust" ]; then cli="$RUST_BIN"; else cli="$GO_BIN"; fi
  for phase in old buffered durable; do
    crash_case "$impl" "$cli" "$phase"
  done
  crash_meta "$impl" "$cli"
done

# ---- receipt ------------------------------------------------------------

WEFT_WB_GENERATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
python3 "$ROOT/bench/scripts/assemble_fuse_writeback.py" \
  --raw "$RAW" --out "$RECEIPT" --label \
  "single host, synthetic deterministic fixtures, buffered I/O, FUSE over a real kernel; no device or power-loss claim; page caches not dropped"

log "receipt: $RECEIPT"
