#!/usr/bin/env bash
# Deterministic cross-language differential fuzz court.
#
# Feeds N generated/mutated WFT1 inputs to the independent Rust and Go
# decoders, requires identical accept/reject classification and identical
# content roots, and fails on any panic, class divergence, corpus-hash
# mismatch, or peak RSS above 256 MiB.
#
# Everything is built and executed inside the OOM-protected Docker courts.
# The host only orchestrates, streams the two result files through paste/awk,
# and writes the receipt.
#
# Usage:
#   scripts/fuzz-diff.sh [N]
#
# N defaults to 1,000,000 and can be overridden with WEFT_FUZZ_N. The generator
# specification is documented verbatim in:
#   WeftCodec/examples/fuzz_differential.rs
#   WeftCodec-Go/cmd/fuzzdiff/main.go
#
# Outputs (under .docker-cache/fuzz, never loaded whole into memory):
#   rust.out, go.out, rust.rows, go.rows, compare.txt
# Receipt:
#   bench/receipts/fuzz-diff-<UTC compact>.json
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

n="${1:-${WEFT_FUZZ_N:-1000000}}"
if [[ ! "$n" =~ ^[0-9]+$ ]] || [[ "$n" -lt 1 ]]; then
  echo "fuzz-diff: N must be a positive integer, got '$n'" >&2
  exit 64
fi
if [[ ! "$n" -ge 1000000 ]]; then
  echo "fuzz-diff: refusing N=$n below the 1,000,000 contract" >&2
  exit 64
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
seeds="$root/conformance/fuzz/v1/seeds.txt"
fuzz_dir="$root/.docker-cache/fuzz"
receipt="$root/bench/receipts/fuzz-diff-${stamp}.json"

if [[ ! -s "$seeds" ]]; then
  echo "fuzz-diff: missing seed corpus $seeds" >&2
  exit 66
fi

mkdir -p "$fuzz_dir" "$root/bench/receipts"
chmod a+rwX "$fuzz_dir" "$root/bench/receipts" 2>/dev/null || true
chmod a+rw "$root/Cargo.lock" "$root/WeftCodec/Cargo.lock" \
  "$root/WeftCodec-Go/go.sum" 2>/dev/null || true
rm -f "$fuzz_dir"/{rust.out,go.out,rust.rows,go.rows,compare.txt}

rust_cmd="cd /work/WeftCodec && cargo build --release --quiet --example fuzz_differential"
rust_cmd="$rust_cmd && /work/.docker-cache/target-rust/release/examples/fuzz_differential"
rust_cmd="$rust_cmd --seeds /work/conformance/fuzz/v1/seeds.txt --n $n"
rust_cmd="$rust_cmd > /work/.docker-cache/fuzz/rust.out"
go_cmd="cd /work/WeftCodec-Go && go build -o /work/.docker-cache/fuzz/fuzzdiff ./cmd/fuzzdiff"
go_cmd="$go_cmd && /work/.docker-cache/fuzz/fuzzdiff"
go_cmd="$go_cmd --seeds /work/conformance/fuzz/v1/seeds.txt --n $n"
go_cmd="$go_cmd > /work/.docker-cache/fuzz/go.out"

rust_status=0
go_status=0
rust_error=""
go_error=""

echo "fuzz-diff: rust court (N=$n)" >&2
run_started="$(date +%s)"
if ! scripts/docker-court.sh rust "$rust_cmd"; then
  rust_status=$?
  rust_error="rust court failed (exit $rust_status)"
  echo "fuzz-diff: $rust_error" >&2
fi
run_split="$(date +%s)"

echo "fuzz-diff: go court (N=$n)" >&2
if ! scripts/docker-court.sh go "$go_cmd"; then
  go_status=$?
  go_error="go court failed (exit $go_status)"
  echo "fuzz-diff: $go_error" >&2
fi
run_finished="$(date +%s)"

rust_elapsed=$((run_split - run_started))
go_elapsed=$((run_finished - run_split))
total_elapsed=$((run_finished - run_started))

for side in rust go; do
  if [[ ! -s "$fuzz_dir/$side.out" ]]; then
    echo "fuzz-diff: $side produced no output" >&2
    [[ "$side" == "rust" ]] && rust_error="missing output" || go_error="missing output"
  fi
done

meta() {
  local side="$1" key="$2" file="$fuzz_dir/$1.out"
  [[ -s "$file" ]] || { printf ''; return; }
  awk -F '\t' -v key="$key" '$1==key { print $2; exit }' "$file" || true
}

rust_hash="$(meta rust corpus_hash)"
go_hash="$(meta go corpus_hash)"
rust_rss="$(meta rust peak_rss_kib)"
go_rss="$(meta go peak_rss_kib)"
rust_ms="$(meta rust elapsed_ms)"
go_ms="$(meta go elapsed_ms)"
[[ -n "$rust_ms" ]] || rust_ms=$((rust_elapsed * 1000))
[[ -n "$go_ms" ]] || go_ms=$((go_elapsed * 1000))

for side in rust go; do
  rows_in="$fuzz_dir/$side.out"
  rows_out="$fuzz_dir/$side.rows"
  if [[ -s "$rows_in" ]]; then
    awk -F '\t' '$1 ~ /^[0-9]+$/ { print }' "$rows_in" > "$rows_out" || true
  else
    : > "$rows_out"
  fi
done

rust_lines="$(wc -l < "$fuzz_dir/rust.rows" | tr -d ' ')"
go_lines="$(wc -l < "$fuzz_dir/go.rows" | tr -d ' ')"

rust_panics="$(awk -F '\t' '$2=="panic" { c++ } END { print c+0 }' "$fuzz_dir/rust.rows")"
go_panics="$(awk -F '\t' '$2=="panic" { c++ } END { print c+0 }' "$fuzz_dir/go.rows")"

# Class distributions as compact JSON objects (ok count plus one key per class).
classes_json() {
  local file="$1"
  awk -F '\t' '
    $2=="ok" { ok++ }
    $2=="err" { cls[$3]++ }
    END {
      printf "{\"ok\":%d", ok+0
      for (c in cls) {
        printf ",\"%s\":%d", c, cls[c]
      }
      printf "}"
    }
  ' "$file"
}

rust_classes="$(classes_json "$fuzz_dir/rust.rows")"
go_classes="$(classes_json "$fuzz_dir/go.rows")"

hash_ok=0
mismatch_total=0
mismatch_count=0
examples_json=""
if [[ -n "$rust_hash" && "$rust_hash" == "$go_hash" ]]; then
  hash_ok=1
  paste "$fuzz_dir/rust.rows" "$fuzz_dir/go.rows" | awk -F '\t' '
    function show(tag) {
      printf "example\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", tag, $1, $2, $3, $4, $5, $6
    }
    {
      total++
      if (NF != 6) {
        mismatches++
        if (mismatches <= 5) show("shape")
        next
      }
      if ($1 != $4 || $2 != $5 || $3 != $6) {
        mismatches++
        if (mismatches <= 5) show("value")
      }
    }
    END { printf "counts\t%d\t%d\n", total, mismatches }
  ' > "$fuzz_dir/compare.txt"
  read -r _ mismatch_total mismatch_count < <(
    awk -F '\t' '$1=="counts" { print $1, $2, $3 }' "$fuzz_dir/compare.txt"
  )
  examples_json="$(awk -F '\t' '
    $1=="example" {
      line=$0
      sub(/^example\t/, "", line)
      gsub(/\\/, "\\\\", line)
      gsub(/"/, "\\\"", line)
      printf "%s\"%s\"", (n++ ? "," : ""), line
    }
  ' "$fuzz_dir/compare.txt")"
else
  echo "fuzz-diff: corpus hash mismatch (rust='${rust_hash:-missing}' go='${go_hash:-missing}')" >&2
fi

max_rss=0
[[ "$rust_rss" =~ ^[0-9]+$ ]] && max_rss="$rust_rss"
[[ "$go_rss" =~ ^[0-9]+$ ]] && [[ "$go_rss" -gt "$max_rss" ]] && max_rss="$go_rss"
rss_ok=0
if [[ "$max_rss" -gt 0 && "$max_rss" -le 262144 ]]; then
  rss_ok=1
fi

result="pass"
reasons=""
fail() { result="fail"; reasons="${reasons}${reasons:+; }$1"; }

[[ "$rust_status" -eq 0 ]] || fail "rust exit $rust_status"
[[ "$go_status" -eq 0 ]] || fail "go exit $go_status"
[[ "$hash_ok" -eq 1 ]] || fail "corpus hash mismatch"
[[ "$rust_panics" -eq 0 ]] || fail "rust panics $rust_panics"
[[ "$go_panics" -eq 0 ]] || fail "go panics $go_panics"
[[ "$rust_lines" -eq "$n" ]] || fail "rust result lines $rust_lines != $n"
[[ "$go_lines" -eq "$n" ]] || fail "go result lines $go_lines != $n"
[[ "$hash_ok" -eq 1 && "$mismatch_count" -eq 0 ]] || fail "mismatches $mismatch_count"
[[ "$rss_ok" -eq 1 ]] || fail "peak RSS ${max_rss} KiB outside (0, 262144]"

rust_rev="$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_rev="$(git -C "$root/WeftCodec" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_go_rev="$(git -C "$root/WeftCodec-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
weftfs_go_rev="$(git -C "$root/WeftFS-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
rust_image="$(docker image inspect --format '{{.Id}}' weft/rust-court:1 2>/dev/null || echo unknown)"
go_image="$(docker image inspect --format '{{.Id}}' weft/go-court:1 2>/dev/null || echo unknown)"
kernel="$(uname -r)"

[[ -n "$examples_json" ]] && examples_json="[$examples_json]" || examples_json="[]"

cat > "$receipt" <<EOF
{
  "kind": "differential-fuzz",
  "label": "CPU-only, synthetic inputs, not coverage-guided",
  "timestamp_utc": "$stamp",
  "generator": {
    "prng": "xorshift64*",
    "seed": "0x9E3779B97F4A7C15",
    "multiplier": "0x2545F4914F6CDD1D",
    "corpus_hash": "blake3-256 of LE64(len(input)) || input for each input in order",
    "max_input_bytes": 4096,
    "splits": {
      "raw_random": "1/8",
      "unrepaired_mutation": "1/8",
      "repaired_mutation": "4/8",
      "semantic_fault": "1/8",
      "clean_seed": "1/8"
    },
    "repaired_mutation": "1..4 body/table mutations; body_len, header_crc32c, and frame_crc32c recomputed; content_hash deliberately not repaired",
    "semantic_faults": "missing/lying/denying dependency, self-cycle REFERENCE via DAG, mode byte 9/127/128/255, decoded_len=u64::MAX, chunk extent past body, wrong content hash, unknown critical flag, non-canonical STRUCTURED body",
    "seeds": "conformance/fuzz/v1/seeds.txt"
  },
  "n": $n,
  "corpus_hash": {
    "rust": "${rust_hash:-}",
    "go": "${go_hash:-}",
    "match": $([[ "$hash_ok" -eq 1 ]] && echo true || echo false)
  },
  "revisions": {
    "WeftFS": "$rust_rev",
    "WeftCodec": "$weftcodec_rev",
    "WeftCodec-Go": "$weftcodec_go_rev",
    "WeftFS-Go": "$weftfs_go_rev"
  },
  "court": {
    "kernel": "$kernel",
    "rust_image": "$rust_image",
    "go_image": "$go_image",
    "memory_limit": "3g",
    "swap_limit": "3g",
    "pids_limit": 1024,
    "cpus": 4
  },
  "rust": {
    "elapsed_ms": ${rust_ms:-0},
    "peak_rss_kib": ${rust_rss:-0},
    "result_lines": $rust_lines,
    "panics": $rust_panics,
    "classes": $rust_classes
  },
  "go": {
    "elapsed_ms": ${go_ms:-0},
    "peak_rss_kib": ${go_rss:-0},
    "result_lines": $go_lines,
    "panics": $go_panics,
    "classes": $go_classes
  },
  "comparison": {
    "compared_lines": $mismatch_total,
    "mismatches": $mismatch_count,
    "first_mismatches": $examples_json
  },
  "total_elapsed_ms": $((total_elapsed * 1000)),
  "errors": "${rust_error}${rust_error:+; }${go_error}",
  "result": "$result",
  "reasons": "$reasons"
}
EOF
chmod a+rw "$receipt" 2>/dev/null || true

echo "fuzz-diff: result=$result"
echo "fuzz-diff: corpus_hash=${rust_hash:-missing} (match=$hash_ok)"
echo "fuzz-diff: rust lines=$rust_lines panics=$rust_panics rss=${rust_rss:-?}KiB ms=${rust_ms:-?}"
echo "fuzz-diff: go   lines=$go_lines panics=$go_panics rss=${go_rss:-?}KiB ms=${go_ms:-?}"
echo "fuzz-diff: compared=$mismatch_total mismatches=$mismatch_count"
echo "fuzz-diff: rust classes: $rust_classes"
echo "fuzz-diff: go   classes: $go_classes"
if [[ -s "$fuzz_dir/compare.txt" ]] && grep -q '^example' "$fuzz_dir/compare.txt"; then
  echo "fuzz-diff: first mismatches:" >&2
  grep '^example' "$fuzz_dir/compare.txt" | head -n 5 >&2 || true
fi
echo "fuzz-diff: receipt=$receipt"

[[ "$result" == "pass" ]]
