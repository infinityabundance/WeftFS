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

# Specification export drift: every subproject spec/ copy must equal parent spec/.
run_court rust "spec_drift" \
  "bash scripts/check-spec-drift.sh"

# Spec-as-code: regenerate both identity type files and fail on any drift.
run_court go "generated_types" \
  "GO111MODULE=off go run scripts/gen-types.go -schema spec/schema/identity-v1.schema -rust WeftCodec/src/generated.rs -go WeftCodec-Go/generated/generated.go --check"

# D1 prototype Buck2 CAS adapter + exact-rebuild/miss/query demonstration.
run_court go "buck2_cas" \
  "cd adapters/buck2 && go vet ./... && go test -count=1 ./..."
run_court go "buck2_cas_demo" \
  "bash scripts/demo-buck2-cas.sh"

# D2 agent long-term memory with simulated timestamps and a months-later replay.
run_court fuse "agent_memory" \
  "bash scripts/demo-agent-memory.sh"

# Context Root cross-language gate: identical IDs and URIs, and each language
# canonicalizes the other's descriptor URIs.
run_court rust "context_cross_rust_gen" \
  "cd WeftCodec && cargo build --release --quiet && cd /work/WeftCodec-Go && bash hack/context-cross.sh rust-gen"
run_court go "context_cross_go_gen" \
  "cd WeftCodec-Go && bash hack/context-cross.sh go-build && bash hack/context-cross.sh go-gen && bash hack/context-cross.sh go-parse-rust"
run_court rust "context_cross_rust_parse" \
  "cd /work/WeftCodec-Go && bash hack/context-cross.sh rust-parse-go"
run_court go "context_cross_compare" \
  "cd WeftCodec-Go && bash hack/context-cross.sh compare"

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
