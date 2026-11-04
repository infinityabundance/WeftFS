#!/usr/bin/env bash
# Filesystem benchmark: fio against a real WeftFS mount and a tmpfs reference
# directory in the same container.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_fs_bench.sh"
#
# The tmpfs directory is a *reference only*: it has no durability and no FUSE
# layer, so it is not an equivalent filesystem. The receipt states this.
#
# Outputs:
#   bench/receipts/bench-fs-<UTC>.json
#   bench/receipts/bench-fs-<UTC>.log
set -Eeuo pipefail

ROOT=/work
SCRATCH="${WEFT_FS_SCRATCH:-$ROOT/.docker-cache/fsbench}"
RECEIPTS="$ROOT/bench/receipts"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BASE="${WEFT_FS_RECEIPT_NAME:-bench-fs-$STAMP}"
RECEIPT="$RECEIPTS/$BASE.json"
LOG="$RECEIPTS/$BASE.log"
RAW_DIR="$SCRATCH/raw"
BIN="$SCRATCH/bin"
TS=1767225600000000000
REPS="${WEFT_FS_REPS:-3}"

RUST_MNT=/mnt/weft-fuse/rust
GO_MNT=/mnt/weft-fuse/go
TMPFS_REF=/mnt/weft-bench/tmpfs-ref
RUST_STORE="$SCRATCH/rust-store"
GO_STORE="$SCRATCH/go-store"
MOUNT_PIDS=()
GO_STATUS="ok"

mkdir -p "$BIN" "$RAW_DIR" "$RECEIPTS" "$RUST_MNT" "$GO_MNT" "$TMPFS_REF"
rm -rf "$SCRATCH/rust-store" "$SCRATCH/go-store"
rm -f "$RAW_DIR"/*
: > "$RAW_DIR/commands.txt"
: > "$RAW_DIR/go-error.txt"

exec > >(tee -a "$LOG") 2>&1
PS4='+ '
set -x

log() { printf '%s\n' "$*" >&2; }

cleanup() {
  set +e
  for mnt in "$RUST_MNT" "$GO_MNT"; do
    if mountpoint -q "$mnt" 2>/dev/null; then
      fusermount3 -u "$mnt" 2>/dev/null || umount -l "$mnt" 2>/dev/null || true
    fi
  done
  for pid in "${MOUNT_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  for mnt in "$RUST_MNT" "$GO_MNT" "$TMPFS_REF"; do
    if mountpoint -q "$mnt" 2>/dev/null; then
      fusermount3 -u "$mnt" 2>/dev/null || umount -l "$mnt" 2>/dev/null || true
    fi
    if ! mountpoint -q "$mnt" 2>/dev/null; then
      find "$mnt" -mindepth 1 -delete 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT

record_cmd() {
  printf '%s\n' "$*" >> "$RAW_DIR/commands.txt"
}

start_mount() {
  local cli="$1" store="$2" mnt="$3" name="$4" extra="${5:-}"
  mkdir -p "$mnt"
  record_cmd "$cli --store $store --fixed-timestamp-ns $TS mount --mountpoint $mnt $extra"
  # shellcheck disable=SC2086
  if [ -n "$extra" ]; then
    "$cli" --store "$store" --fixed-timestamp-ns "$TS" mount --mountpoint "$mnt" "$extra" \
      >"$SCRATCH/mount-$name.log" 2>&1 &
  else
    "$cli" --store "$store" --fixed-timestamp-ns "$TS" mount --mountpoint "$mnt" \
      >"$SCRATCH/mount-$name.log" 2>&1 &
  fi
  local pid=$!
  MOUNT_PIDS+=("$pid")
  local tries=0
  while ! mountpoint -q "$mnt" 2>/dev/null; do
    if ! kill -0 "$pid" 2>/dev/null; then
      log "$name mount daemon exited before the mount appeared"
      cat "$SCRATCH/mount-$name.log" >&2 || true
      return 1
    fi
    tries=$((tries + 1))
    if [ "$tries" -gt 300 ]; then
      log "$name mount did not appear within 30s"
      cat "$SCRATCH/mount-$name.log" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  sleep 0.2
  return 0
}

reset_target() {
  local dir="$1"
  rm -f "$dir/seqw.dat" "$dir/randw.dat" "$dir/fsyncw.dat"
  # The 8 MiB read corpus is created before the timed workloads.
  dd if=/dev/zero of="$dir/read8m.dat" bs=1M count=8 conv=fsync status=none
}

run_fio() {
  local target="$1" dir="$2" workload="$3" raw="$4"
  local rw bs size file
  local -a fsync=()
  case "$workload" in
    seq-write-1m) rw=write; bs=1M; size=8M; file=seqw.dat ;;
    seq-read-1m) rw=read; bs=1M; size=8M; file=read8m.dat ;;
    rand-read-4k) rw=randread; bs=4k; size=8M; file=read8m.dat ;;
    rand-write-4k) rw=randwrite; bs=4k; size=8M; file=randw.dat ;;
    fsync-write-4k) rw=write; bs=4k; size=1M; file=fsyncw.dat; fsync=(--fsync=1) ;;
    *) log "unknown workload $workload"; return 1 ;;
  esac
  record_cmd "fio --name=$workload --directory=$dir --filename=$file --rw=$rw --bs=$bs --size=$size --ioengine=psync --direct=0 --numjobs=1 --iodepth=1 --group_reporting --output-format=json --output=$raw --eta=never --percentile_list=50:95:99 ${fsync[*]:-}"
  fio --name="$workload" --directory="$dir" --filename="$file" \
    --rw="$rw" --bs="$bs" --size="$size" --ioengine=psync --direct=0 \
    --numjobs=1 --iodepth=1 --group_reporting --output-format=json \
    --output="$raw" --eta=never --percentile_list=50:95:99 "${fsync[@]}" \
    >/dev/null 2>>"$LOG"
  if [ ! -s "$raw" ]; then
    log "fio produced no JSON for $target/$workload"
    return 1
  fi
}

log "fs bench: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r) reps=$REPS"
fio --version > "$RAW_DIR/fio-version.txt" 2>&1 || true
{
  echo "uname=$(uname -a)"
  echo "nproc=$(nproc)"
  echo "cpu=$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)"
  echo "memory=$(awk '/MemTotal/ {print $2\" kB\"; exit}' /proc/meminfo)"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "court_memory=${WEFT_COURT_MEMORY:-unknown}"
  echo "court_cpus=${WEFT_COURT_CPUS:-unknown}"
  echo "court_pids=${WEFT_COURT_PIDS:-unknown}"
  echo "docker_client=${WEFT_DOCKER_CLIENT:-unknown}"
  echo "docker_server=${WEFT_DOCKER_SERVER:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "revision_weftfs_go=${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "revision_weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "revision_weftcodec_go=${WEFT_REPO_WEFTCODEC_GO:-unknown}"
} > "$RAW_DIR/environment.txt"

# ---- build ----------------------------------------------------------------
record_cmd "cargo build --release --features fuse --bin weftfs --example bench_fs_receipt"
cargo build --release --features fuse --bin weftfs --example bench_fs_receipt
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}/release"
RUSTFS="$TARGET/weftfs"
RECEIPTGEN="$TARGET/examples/bench_fs_receipt"
[ -x "$RUSTFS" ] && [ -x "$RECEIPTGEN" ]

record_cmd "(cd WeftFS-Go && CGO_ENABLED=0 go build -o $BIN/go-weftfs ./cmd/weftfs)"
if (cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$BIN/go-weftfs" ./cmd/weftfs) \
  >>"$SCRATCH/go-build.log" 2>&1; then
  GO_STATUS="ok"
else
  GO_STATUS="blocked"
  {
    echo "go build failed"
    cat "$SCRATCH/go-build.log"
  } > "$RAW_DIR/go-error.txt"
fi
GOFS="$BIN/go-weftfs"

# ---- Rust store and mount -------------------------------------------------
record_cmd "$RUSTFS --store $RUST_STORE --fixed-timestamp-ns $TS init"
"$RUSTFS" --store "$RUST_STORE" --fixed-timestamp-ns "$TS" init
if ! start_mount "$RUSTFS" "$RUST_STORE" "$RUST_MNT" rust --foreground; then
  log "Rust WeftFS mount failed"
  exit 1
fi
{
  echo "target=rust"
  echo "store=$RUST_STORE"
  echo "mountpoint=$RUST_MNT"
  findmnt -T "$RUST_MNT" -o TARGET,SOURCE,FSTYPE,OPTIONS -n || true
  grep " $RUST_MNT " /proc/self/mountinfo || true
} > "$RAW_DIR/mountinfo-rust.txt"

# ---- Go store and mount (optional) ---------------------------------------
GO_MNT_ACTIVE=0
if [ "$GO_STATUS" = "ok" ]; then
  if "$GOFS" --store "$GO_STORE" --fixed-timestamp-ns "$TS" init \
      >"$SCRATCH/go-init.log" 2>&1; then
    if start_mount "$GOFS" "$GO_STORE" "$GO_MNT" go; then
      GO_MNT_ACTIVE=1
    else
      GO_STATUS="blocked"
      {
        echo "go mount failed"
        cat "$SCRATCH/mount-go.log"
      } > "$RAW_DIR/go-error.txt"
    fi
  else
    GO_STATUS="blocked"
    {
      echo "go init failed"
      cat "$SCRATCH/go-init.log"
    } > "$RAW_DIR/go-error.txt"
  fi
fi
if [ "$GO_MNT_ACTIVE" = "1" ]; then
  {
    echo "target=go"
    echo "store=$GO_STORE"
    echo "mountpoint=$GO_MNT"
    findmnt -T "$GO_MNT" -o TARGET,SOURCE,FSTYPE,OPTIONS -n || true
    grep " $GO_MNT " /proc/self/mountinfo || true
  } > "$RAW_DIR/mountinfo-go.txt"
fi
printf 'go_status=%s\n' "$GO_STATUS" > "$RAW_DIR/status.txt"
{
  echo "target=tmpfs"
  echo "mountpoint=$TMPFS_REF"
  findmnt -T "$TMPFS_REF" -o TARGET,SOURCE,FSTYPE,OPTIONS -n || true
} > "$RAW_DIR/mountinfo-tmpfs.txt"

# ---- workloads ------------------------------------------------------------
WORKLOADS=(seq-write-1m seq-read-1m rand-read-4k rand-write-4k fsync-write-4k)
for rep in $(seq 1 "$REPS"); do
  reset_target "$RUST_MNT"
  reset_target "$TMPFS_REF"
  if [ "$GO_MNT_ACTIVE" = "1" ]; then
    reset_target "$GO_MNT"
  fi
  workload_index=0
  for workload in "${WORKLOADS[@]}"; do
    if [ $(( (rep + workload_index) % 2 )) -eq 0 ]; then
      order=(rust tmpfs)
      [ "$GO_MNT_ACTIVE" = "1" ] && order=(rust go tmpfs)
    else
      order=(tmpfs rust)
      [ "$GO_MNT_ACTIVE" = "1" ] && order=(tmpfs go rust)
    fi
    for target in "${order[@]}"; do
      case "$target" in
        rust) dir="$RUST_MNT" ;;
        go) dir="$GO_MNT" ;;
        tmpfs) dir="$TMPFS_REF" ;;
      esac
      raw="$RAW_DIR/${target}--${workload}--r${rep}.json"
      run_fio "$target" "$dir" "$workload" "$raw"
    done
    workload_index=$((workload_index + 1))
  done
  log "fs bench: repetition $rep/$REPS complete"
done

# ---- receipt --------------------------------------------------------------
"$RECEIPTGEN" --raw-dir "$RAW_DIR" --out "$RECEIPT"

# Clean unmount before exit; the trap repeats this idempotently.
for mnt in "$RUST_MNT" "$GO_MNT"; do
  if mountpoint -q "$mnt" 2>/dev/null; then
    fusermount3 -u "$mnt" 2>/dev/null || umount -l "$mnt" 2>/dev/null || true
  fi
done
for pid in "${MOUNT_PIDS[@]:-}"; do
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
done

echo "fs receipt: $RECEIPT"
echo "raw log: $LOG"
sha256sum "$RECEIPT" "$LOG"
