#!/usr/bin/env bash
# Stage 1: measured decoder floor. Single-threaded, warm in-process input.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/decode_floor_court.sh"
#
# Components: huffman_decode (exponent plane), plane_join, mode9_decode,
# mode9_encode. Sizes 128 KiB / 1 MiB / 16 MiB, dtypes bf16 and fp32.
# The receipt records the compiler, the court image, the detected CPU
# features, and the exact command so every later speedup is relative to a
# named measurement rather than a memory.
set -Eeuo pipefail

root=/work
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
out="$root/bench/receipts/decode-floor-$stamp.json"
rows="$root/.docker-cache/decode-floor-rows-$stamp.jsonl"
mkdir -p "$root/.docker-cache" "$root/bench/receipts"

echo "== build (release) =="
export CARGO_TARGET_DIR="$root/.docker-cache/target-rust-codec"
cargo build --release --manifest-path "$root/WeftCodec/Cargo.toml" --example decode_floor_bench
binary="$CARGO_TARGET_DIR/release/examples/decode_floor_bench"

echo "== measure (rust) =="
"$binary" | tee "$rows"

echo "== measure (go, word-level only) =="
go_bench="$root/bench/receipts/decode-floor-$stamp.go-bench.txt"
if command -v go >/dev/null 2>&1; then
  (
    cd "$root/WeftCodec-Go"
    go test ./modes/ -run '^$' -bench 'Benchmark(HuffmanDecode(WordOptimised|Scalar)|BF16Join(Optimised|Accumulator|Scalar))' -benchtime=1s -count=3
  ) | tee "$go_bench"
else
  printf 'go toolchain unavailable\n' | tee "$go_bench"
fi

rustc_version="$(rustc --version)"
cc_version="$(cc --version | head -1)"
kernel="$(uname -r)"
nproc="$(nproc)"

{
  printf '{\n'
  printf '  "schema": "weft.receipt.decode-floor.v1",\n'
  printf '  "stage": "decode-floor",\n'
  printf '  "stamp": "%s",\n' "$stamp"
  printf '  "command": "scripts/docker-court.sh bench \\"cd /work && bash bench/scripts/decode_floor_court.sh\\"",\n'
  printf '  "environment": {\n'
  printf '    "court_image": "%s",\n' "${WEFT_COURT_IMAGE:-unknown}"
  printf '    "court_image_id": "%s",\n' "${WEFT_COURT_IMAGE_ID:-unknown}"
  printf '    "court_kernel": "%s",\n' "${WEFT_COURT_KERNEL:-$kernel}"
  printf '    "court_memory": "%s",\n' "${WEFT_COURT_MEMORY:-unknown}"
  printf '    "court_cpus": "%s",\n' "${WEFT_COURT_CPUS:-unknown}"
  printf '    "court_pids": "%s",\n' "${WEFT_COURT_PIDS:-unknown}"
  printf '    "nproc": %s,\n' "$nproc"
  printf '    "rustc": "%s",\n' "${rustc_version//\"/}"
  printf '    "cc": "%s",\n' "${cc_version//\"/}"
  printf '    "threads": 1,\n'
  printf '    "cache_state": "warm in-process input"\n'
  printf '  },\n'
  printf '  "measurement": {\n'
  printf '    "reps": %s,\n' "${WEFT_FLOOR_REPS:-5}"
  printf '    "statistic": "median of measured calls after one warm-up"\n'
  printf '  },\n'
  printf '  "go_bench_file": "bench/receipts/decode-floor-%s.go-bench.txt",\n' "$stamp"
  printf '  "rows": [\n'
  awk 'BEGIN { first=1 } { if (!first) printf ",\n"; printf "    %s", $0; first=0 } END { printf "\n" }' "$rows"
  printf '  ]\n'
  printf '}\n'
} > "$out"

echo "receipt $out"
