#!/usr/bin/env bash
# Merkle vs whole-file content-identity write benchmark.
#
# 128 MiB file at 64 KiB chunks, 500 deterministic random 4 KiB overwrites,
# measured independently in Rust and Go, one process per identity so
# /proc/self/io and peak RSS describe one policy.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/merkle_identity_court.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/merkle-identity-${stamp}.json"
log="bench/receipts/merkle-identity-${stamp}.log"
tmp="$(mktemp -d /tmp/weft-merkle-bench.XXXXXX)"
bin="${root}/.docker-cache/merklebench"
trap 'rm -rf "$tmp" "$bin"' EXIT

{
  echo "merkle identity court ${stamp}"
  echo "image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "kernel=$(uname -r) memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
  echo "WeftFS=${WEFT_REPO_WEFTFS:-unknown} WeftFS-Go=${WEFT_REPO_WEFTFS_GO:-unknown}"
} >"$log"

cargo build --release --example merkle_identity_bench >>"$log" 2>&1
rust_target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}/release/examples/merkle_identity_bench"
"$rust_target" --identity whole-file --out "$tmp/rust-whole.json" >>"$log" 2>&1
"$rust_target" --identity merkle --out "$tmp/rust-merkle.json" >>"$log" 2>&1

(
  cd WeftFS-Go
  go build -o "$bin" ./cmd/merklebench
) >>"$log" 2>&1
"$bin" --identity whole-file --out "$tmp/go-whole.json" >>"$log" 2>&1
"$bin" --identity merkle --out "$tmp/go-merkle.json" >>"$log" 2>&1

{
  printf '{\n'
  printf '  "schema": "weft.merkle.bench.v1",\n'
  printf '  "label": "Single-host Docker bench court with hard memory/CPU/pids limits and bounded /tmp. Warm page cache. 128 MiB file, 64 KiB chunks, 500 deterministic random 4 KiB overwrites. Peak RSS reset with /proc/self/clear_refs after fixture creation. Not a fleet, power-loss, or physical-media claim.",\n'
  printf '  "environment": {\n'
  printf '    "kernel": "%s",\n' "${WEFT_COURT_KERNEL:-unknown}"
  printf '    "court_image": "%s",\n' "${WEFT_COURT_IMAGE:-unknown}"
  printf '    "court_image_id": "%s",\n' "${WEFT_COURT_IMAGE_ID:-unknown}"
  printf '    "court_memory": "%s",\n' "${WEFT_COURT_MEMORY:-unknown}"
  printf '    "court_cpus": "%s",\n' "${WEFT_COURT_CPUS:-unknown}"
  printf '    "court_pids": "%s",\n' "${WEFT_COURT_PIDS:-unknown}"
  printf '    "docker_client": "%s",\n' "${WEFT_DOCKER_CLIENT:-unknown}"
  printf '    "docker_server": "%s",\n' "${WEFT_DOCKER_SERVER:-unknown}"
  printf '    "revision_weftfs": "%s",\n' "${WEFT_REPO_WEFTFS:-unknown}"
  printf '    "revision_weftfs_go": "%s"\n' "${WEFT_REPO_WEFTFS_GO:-unknown}"
  printf '  },\n'
  printf '  "workload": {\n'
  printf '    "file_bytes": 134217728,\n'
  printf '    "chunk_size": 65536,\n'
  printf '    "overwrites": 500,\n'
  printf '    "overwrite_bytes": 4096,\n'
  printf '    "offset_sequence": "xorshift64* state 0x9E3779B97F4A7C15, offset = next() %% (file_bytes - overwrite_bytes + 1)"\n'
  printf '  },\n'
  printf '  "runs": [\n'
  printf '    '
  cat "$tmp/rust-whole.json"
  printf ',\n    '
  cat "$tmp/rust-merkle.json"
  printf ',\n    '
  cat "$tmp/go-whole.json"
  printf ',\n    '
  cat "$tmp/go-merkle.json"
  printf '\n  ]\n}\n'
} >"$receipt"

echo "merkle identity receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
