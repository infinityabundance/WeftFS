#!/usr/bin/env bash
# 3FS transport court (subphase R22).
#
# Two transports over the same normative layout:
#   posix   ordinary positional I/O through a 3FS FUSE mount
#   usrbio  hf3fs user-space rings for the immutable data plane
#
# The USRBIO transport is driven against `bench/threefs/hf3fs_shim.c`, a
# conformance implementation of the published ABI over real POSIX I/O. This is
# NOT a 3FS deployment: the shim reports `hf3fs_is_hf3fs = 0`, which the adapter
# records in diagnostics and in the receipt. No 3FS throughput, latency, RDMA,
# or multi-node claim is made anywhere.
#
# Usage:
#   scripts/threefs_court.sh            # build the shim and run tests/threefs.rs
#   scripts/threefs_court.sh --bench    # tests plus the ABI/batching bench
#
# Runs inside the rust court (build-essential is present in `weft/rust-court:1`).
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bench=0
for arg in "$@"; do
  case "$arg" in
    --bench) bench=1 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

work="${TMPDIR:-/tmp}/weft-threefs"
rm -rf "$work"
mkdir -p "$work/data"
# The shim is a shared object and must be dlopen-able, so it cannot live on the
# court's `/tmp` (Docker mounts that tmpfs `noexec`; dlopen then fails with
# "failed to map segment from shared object"). `./.docker-cache` is a writable,
# executable bind mount and is gitignored.
libdir="$repo_root/.docker-cache/threefs/lib"
mkdir -p "$libdir"

echo "=== building the USRBIO conformance library ==="
cc -std=c11 -O2 -fPIC -shared -Wall -Wextra -Werror \
  -o "$libdir/libhf3fs_api_shared.so" bench/threefs/hf3fs_shim.c

# A negative check: the adapter must refuse a library that is not the 3FS client.
cat > "$work/not_threefs.c" <<'EOF'
int not_hf3fs(void) { return 0; }
EOF
cc -std=c11 -O2 -fPIC -shared -o "$libdir/libnot_threefs.so" "$work/not_threefs.c"

export WEFT_3FS_SHIM="$libdir/libhf3fs_api_shared.so"
export WEFT_3FS_SHIM_DIR="$work/data"

echo "=== tests/threefs.rs ==="
# Single-threaded: the conformance library keeps a process-global registration
# table and the published ABI is itself single-threaded per ring, so serialising
# the court keeps the measurement honest rather than hiding a race.
cargo test --test threefs -- --nocapture --test-threads=1

# The refusal path needs no shim: an unrelated shared object must be rejected
# symbol-by-symbol. This runs here rather than in the test file so the exact
# library is chosen by the court.
echo "=== refusing a shared object that is not the 3FS client ==="
cargo run --quiet --bin weftfs -- --backend 3fs --threefs-mode usrbio \
  --threefs-lib "$libdir/libnot_threefs.so" \
  --store "$work/refuse" info 2>&1 | tee "$work/refuse.log" >&2 || true
if ! grep -q "missing 3FS USRBIO symbols" "$work/refuse.log"; then
  echo "threefs_court: the adapter did not name the missing USRBIO symbols" >&2
  cat "$work/refuse.log" >&2
  exit 1
fi

if [[ "$bench" == "1" ]]; then
  stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
  mkdir -p bench/receipts
  receipt="bench/receipts/threefs-${stamp}.json"
  echo "=== bench: USRBIO data plane vs positional reads ==="
  WEFT_STAMP="$stamp" cargo run --quiet --release --example threefs_bench \
    > "$receipt" 2> "bench/receipts/threefs-${stamp}.log"
  echo "wrote $receipt"
fi

echo "threefs_court: ok"
