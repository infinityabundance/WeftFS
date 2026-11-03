#!/usr/bin/env bash
# Full verification court. Runs every test suite in its OOM-protected container
# and writes a machine-readable receipt. Nothing is compiled or tested on the
# host. This is a correctness court, not a benchmark.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/verify-${stamp}.json"
log="bench/receipts/verify-${stamp}.log"
results="bench/receipts/.verify-${stamp}.results"
: > "$log"
: > "$results"

status=0
run_court() {
  local kind="$1" label="$2" command="$3"
  echo "=== $label ===" | tee -a "$log"
  if scripts/docker-court.sh "$kind" "$command" >>"$log" 2>&1; then
    echo "$label=pass" >> "$results"
    echo "PASS $label"
  else
    echo "$label=fail" >> "$results"
    echo "FAIL $label"
    status=1
  fi
}

run_court rust "rust_codec" \
  "cd WeftCodec && cargo test --all-targets --quiet"
run_court rust "rust_store" \
  "cargo test --all-targets --quiet"
run_court go "go_codec" \
  "cd WeftCodec-Go && go test ./..."
run_court fuse "rust_fuse" \
  "cargo test --features fuse --all-targets -- --test-threads=1"
run_court fuse "go_store_and_fuse" \
  "cd WeftFS-Go && CGO_ENABLED=0 go build ./... && go vet ./... && go test -race ./... -count=1"

# Cross-language codec gate: Rust encodes, Go decodes; Go encodes, Rust
# decodes; deterministic metadata bytes are compared.
run_court rust "codec_cross_rust_encode" \
  "cd WeftCodec && cargo build --release --quiet && cd /work/WeftCodec-Go && bash hack/cross.sh rust-gen"
run_court go "codec_cross_go_all" \
  "cd WeftCodec-Go && bash hack/cross.sh go-build && bash hack/cross.sh go-gen && bash hack/cross.sh go-decode"
run_court rust "codec_cross_rust_decode" \
  "cd /work/WeftCodec-Go && bash hack/cross.sh rust-decode"
run_court go "codec_cross_compare" \
  "cd WeftCodec-Go && bash hack/cross.sh compare"

{
  echo "{"
  echo "  \"kind\": \"correctness-verification\","
  echo "  \"timestamp_utc\": \"$stamp\","
  echo "  \"host\": {"
  echo "    \"uname\": \"$(uname -srm)\","
  echo "    \"docker\": \"$(docker --version)\""
  echo "  },"
  echo "  \"revisions\": {"
  echo "    \"WeftFS\": \"$(git -C . rev-parse HEAD)\","
  echo "    \"WeftCodec\": \"$(git -C WeftCodec rev-parse HEAD)\","
  echo "    \"WeftCodec-Go\": \"$(git -C WeftCodec-Go rev-parse HEAD)\","
  echo "    \"WeftFS-Go\": \"$(git -C WeftFS-Go rev-parse HEAD)\""
  echo "  },"
  echo "  \"courts\": {"
  first=1
  while IFS='=' read -r label result; do
    [[ -z "$label" ]] && continue
    if [[ "$first" -eq 1 ]]; then first=0; else echo ","; fi
    printf '    "%s": "%s"' "$label" "$result"
  done < "$results"
  echo ""
  echo "  },"
  echo "  \"result\": \"$([ "$status" -eq 0 ] && echo pass || echo fail)\","
  echo "  \"raw_log\": \"$log\""
  echo "}"
} > "$receipt"

rm -f "$results"
echo "receipt: $receipt (result=$([ "$status" -eq 0 ] && echo pass || echo fail))"
exit "$status"
