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
skips="bench/receipts/.verify-${stamp}.skips"
: > "$log"
: > "$results"
: > "$skips"

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
  "cargo build --release --features fuse --bin weftfs && cd WeftFS-Go && CGO_ENABLED=0 go build ./... && go vet ./... && WEFTFS_RUST_BIN=/work/.docker-cache/target-rust/release/weftfs go test -race ./... -count=1"

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

# Documentation claim drift: every externally visible claim in docs/claims.json
# must appear (or be absent) exactly where the manifest says, and cite an
# existing receipt. Host-only check, so it is invoked directly instead of
# through a container court.
echo "=== claims ===" | tee -a "$log"
if bash scripts/check-claims.sh >>"$log" 2>&1; then
  echo "claims=pass" >> "$results"
  echo "PASS claims"
else
  echo "claims=fail" >> "$results"
  echo "FAIL claims"
  status=1
fi

# Conformance manifest drift and coverage gaps. Host-only and fast: the
# generator is deterministic and the coverage gate reads the committed corpus.
echo "=== conformance_manifest ===" | tee -a "$log"
if bash scripts/gen-conformance-manifest.sh --check >>"$log" 2>&1; then
  echo "conformance_manifest=pass" >> "$results"
  echo "PASS conformance_manifest"
else
  echo "conformance_manifest=fail" >> "$results"
  echo "FAIL conformance_manifest"
  status=1
fi

echo "=== conformance_coverage ===" | tee -a "$log"
if bash scripts/check-conformance-coverage.sh >>"$log" 2>&1; then
  echo "conformance_coverage=pass" >> "$results"
  echo "PASS conformance_coverage"
else
  echo "conformance_coverage=fail" >> "$results"
  echo "FAIL conformance_coverage"
  status=1
fi

# Spec-as-code: regenerate both identity type files and fail on any drift.
run_court go "generated_types" \
  "GO111MODULE=off go run scripts/gen-types.go -schema spec/schema/identity-v1.schema -rust WeftCodec/src/generated.rs -go WeftCodec-Go/generated/generated.go --check"

# P6 store-backed Buck2 CAS adapter: Go tests, then the FUSE court with the
# Rust oracle and a real context mount (cross-language both directions).
run_court go "buck2_cas" \
  "cd adapters/buck2 && go vet ./... && go test -count=1 ./..."
run_court fuse "buck2_cas_store" \
  "bash scripts/demo-buck2-cas.sh"

# R22 3FS transports: the POSIX transport must be byte-identical to the local
# backend, and the USRBIO transport is driven against a conformance library that
# implements the published ABI over real POSIX I/O. No 3FS deployment is run, so
# this court proves the adapter, not 3FS throughput.
run_court rust "threefs" \
  "bash scripts/threefs_court.sh"

# Wall-time budget for one court in this aggregate: 10 minutes. Three courts can
# exceed it at full size — checkpoint-family lifecycle, trajectory and write
# policy — so they run reduced configurations here and record exactly what they
# ran in their own receipts. Their full-size standalone receipts remain the
# full evidence and are cited in docs/STATUS.md.

# Checkpoint-family lifecycle (reduced for the aggregate: one family, one
# configuration, one repetition each). The full-size standalone receipt is
# `bench/receipts/family-lifecycle-*.json` from the default run.
run_court bench "checkpoint_family_lifecycle" \
  "WEFT_FAMILY_FAMILIES=pythia-consecutive WEFT_FAMILY_CONFIGS=default WEFT_FAMILY_INGEST_REPS=1 WEFT_FAMILY_MEASURE_REPS=1 WEFT_FAMILY_TOPOLOGY_REPS=1 WEFT_FAMILY_RECOVERY_REPS=1 WEFT_FAMILY_S3=0 bash bench/scripts/checkpoint_family_court.sh"

# Trajectory (reduced: two pythia steps, eight synthetic steps, no Qwen half).
# Every reduced knob is recorded in the receipt under phases.configuration.
run_court bench "trajectory" \
  "WEFT_TRAJ_QWEN=0 WEFT_TRAJ_SYNTH_STEPS=8 WEFT_TRAJ_PYTHIA_STEPS='0 1' bash bench/scripts/trajectory_court.sh"

# Critical-path write policy and background transcode (two configurations:
# planned and fast-zstd-1, which bracket the encode-cost range).
run_court bench "write_policy" \
  "WEFT_WRITE_POLICY_CONFIGS='planned fast-zstd1' bash bench/scripts/write_policy_court.sh"

# Load receipts (lazy vs full) and recovery-first restore on the 512 MiB
# synthetic tiled fixture.
run_court bench "recovery_load" \
  "bash bench/scripts/recovery_load_court.sh"

# Zero-trust KV page reads. The bench itself refuses fewer than 1,000 samples,
# so the aggregate uses the minimum accepted count (the standalone receipt uses
# 2,000).
run_court rust "kv_capability" \
  "WEFT_KV_REPS=1000 bash bench/scripts/kv_capability_court.sh"

# Prepare/commit checkpoint split at 1/2/4/8 ranks on the real Qwen base. This
# script orchestrates its own bench court, so it is invoked directly.
echo "=== parallel_checkpoint ===" | tee -a "$log"
if bash bench/scripts/parallel_checkpoint_court.sh >>"$log" 2>&1; then
  echo "parallel_checkpoint=pass" >> "$results"
  echo "PASS parallel_checkpoint"
else
  echo "parallel_checkpoint=fail" >> "$results"
  echo "FAIL parallel_checkpoint"
  status=1
fi

# Throughput (R10): the phase profile, the pack-plane save and the pipelined
# restore. Reduced for the aggregate — one family, one save configuration, one
# rank count, a 40-tensor ingest prefix, two restore thread counts. The
# full-size standalone receipts are `throughput-20261110T134917Z.json` (0.5B)
# and `throughput-20261110T145341Z.json` (3B), cited in
# `bench/RESULTS-throughput.md`.
run_court bench "throughput" \
  "WEFT_THROUGHPUT_FAMILIES=qwen0.5b WEFT_THROUGHPUT_REPS=1 WEFT_THROUGHPUT_RANKS=8 WEFT_THROUGHPUT_SAVE_CONFIGS=raw WEFT_THROUGHPUT_INGEST_LIMIT=40 WEFT_THROUGHPUT_RESTORE_THREADS='1 4' WEFT_THROUGHPUT_TILE_THREADS=1 bash bench/scripts/throughput_court.sh"

# Restore scaling (R13): the backend-lock probe, the immutable-read pool, and
# within-tensor shard concurrency. Reduced for the aggregate — one family, two
# worker counts, one repetition, no MinIO, no readers, no second checkpoint.
# The full matrices are `restore-scaling-20261111T090904Z` (0.5B) and
# `restore-scaling-3b-final-20261111T092000Z` (3B), cited in
# `bench/RESULTS-restore-scaling.md`.
run_court bench "restore_scaling" \
  "WEFT_RESTORE_FAMILIES=qwen0.5b WEFT_RESTORE_THREADS='1 4' WEFT_RESTORE_REPS=1 WEFT_RESTORE_MINIO=0 WEFT_RESTORE_SECOND=0 WEFT_RESTORE_READERS=0 bash bench/scripts/restore_scaling_court.sh"

# Index trust: a forged self-consistent index subset must be rejected by the
# commit attestation, and a genuine cross-language segment must load without a
# rebuild.
run_court rust "index_trust" \
  "cargo test --test index_persistence --test index_equivalence --quiet"

# D2 agent long-term memory with simulated timestamps and a months-later replay.
run_court fuse "agent_memory" \
  "bash scripts/demo-agent-memory.sh"

# Courts that orchestrate their own sidecar containers are invoked directly
# instead of through run_court. Exit 78 is an explicit environment skip (for
# example no Docker daemon or an unobtainable pinned image) and is recorded as
# `skip`, never as a pass. A skip without a printed reason is a failure.
run_orchestrated_court() {
  local label="$1" prefix="$2"
  shift 2
  echo "=== $label ===" | tee -a "$log"
  local rc=0
  "$@" >>"$log" 2>&1 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    echo "$label=pass" >> "$results"
    echo "PASS $label"
    return 0
  fi
  if [[ "$rc" -eq 78 ]]; then
    local reason
    reason="$(grep -h "^${prefix}: skip " "$log" | tail -1 | sed "s/^${prefix}: skip //" || true)"
    [[ -z "$reason" ]] && reason="${prefix} exited 78 without a printed reason"
    # Reasons may quote Docker errors; keep the receipt valid JSON.
    reason="$(printf '%s' "$reason" | tr -d '"\\' | tr '\n\t' ' ')"
    echo "$label=skip" >> "$results"
    printf '%s\t%s\n' "$label" "$reason" >> "$skips"
    echo "SKIP $label: $reason"
    return 0
  fi
  echo "$label=fail" >> "$results"
  echo "FAIL $label (exit $rc)"
  status=1
}

# Performance numbers: every generated region in the documentation must match
# its receipt, the declared stamp and revision must still be the ones the
# receipt carries, every manifest number must have a consumer, and every
# `BEGIN GENERATED` marker must have a manifest block. Host-only and fast; an
# environment without Python >= 3.8 skips with a printed reason (exit 78).
run_orchestrated_court perf_numbers perf_numbers bash scripts/perf_numbers_court.sh

# R16 independent S3 implementation: a Ceph RGW sidecar, the existing S3 backend
# suite against it, the eight-behaviour compatibility matrix against RGW and
# MinIO, and a side-by-side comparison.
run_orchestrated_court ceph_rgw ceph_court env WEFT_COMPAT_STAMP="$stamp" bash scripts/ceph_court.sh

# Q4/MinIO: the native S3 backend and fenced-writer suites against a real
# MinIO server in its own container. The same environment-only skip semantics
# as ceph_rgw apply.
run_orchestrated_court s3_court s3_court env WEFT_COMPAT_STAMP="$stamp" bash scripts/s3_court.sh

# R3 same-backend augmentation court. The aggregate uses its explicit reduced
# configuration: 16 evenly spaced non-embedding tensors (1,849,344 logical
# bytes, no 272 MB embedding), configurations A/B/C/D/E plus the supplementary
# native-app baseline N, two restore passes. The full standalone receipts use
# the embedding + 100 tensors (424,439,296 logical bytes). A court that cannot
# run prints `augmentation_court: skip <reason>` and exits 78; the aggregate
# records a skip, never a pass.
run_orchestrated_court augmentation_local augmentation_court \
  env WEFT_AUGMENTATION_REDUCED=1 WEFT_AUGMENTATION_STAMP="$stamp" \
  bash scripts/augmentation_court.sh --local

# R13 third-party conformance distribution: build the bundle in the fuse court,
# run the reference Rust adapter from the extracted copy only, and verify the
# bundle with `scripts/verify-receipts.sh` in the court and again on the host
# with `--repo-root .`. The packaging script has no cost knob, so this is its
# full default configuration; the only deferred phase is the independent Python
# second opinion, run by the next court because the fuse court has no blake3.
# The receipt's `configuration` block records that split. Both courts share
# $stamp, so the Python court merges into the receipt the bundle court wrote.
run_orchestrated_court conformance_bundle conformance_bundle \
  env WEFT_STAMP="$stamp" bash scripts/conformance_bundle_court.sh
run_orchestrated_court conformance_python conformance_python \
  env WEFT_STAMP="$stamp" bash scripts/conformance_python_court.sh

# Context Root cross-language gate: identical IDs and URIs, and each language
# canonicalizes the other's descriptor URIs.
run_court rust "context_cross_rust_gen" \
  "cd WeftCodec && cargo build --release --quiet && cd /work/WeftCodec-Go && bash hack/context-cross.sh rust-gen"
run_court go "context_cross_go_gen" \
  "cd WeftCodec-Go && bash hack/context-cross.sh go-build && bash hack/context-cross.sh go-gen && bash hack/context-cross.sh go-parse-rust"
run_court rust "context_cross_rust_parse" \
  "cd /work/WeftCodec-Go && bash hack/context-cross.sh rust-parse-go"
run_court go "context_cross_compare" \
  "cd /work/WeftCodec-Go && bash hack/context-cross.sh compare"

# Authenticated capability envelope cross-language gate: identical canonical
# bytes, EnvelopeId, deterministic signature, and every rejection class.
run_court rust "capability_cross_rust_gen" \
  "cd /work/WeftCodec-Go && bash hack/capability-cross.sh rust-gen"
run_court go "capability_cross_go_gen" \
  "cd /work/WeftCodec-Go && bash hack/capability-cross.sh go-gen"
run_court rust "capability_cross_rust_verify" \
  "cd /work/WeftCodec-Go && bash hack/capability-cross.sh rust-verify-go"
run_court go "capability_cross_go_verify" \
  "cd /work/WeftCodec-Go && bash hack/capability-cross.sh go-verify-rust"
run_court go "capability_cross_compare" \
  "cd /work/WeftCodec-Go && bash hack/capability-cross.sh compare"

# Deterministic cross-language differential fuzz: 1,000,000 generated/mutated
# inputs through both decoders, identical classes and content roots, no panics,
# peak RSS <= 256 MiB. The gate orchestrates its own two courts, so it is
# invoked directly instead of through run_court; it completes in seconds with
# the release harnesses and writes its own receipt.
echo "=== fuzz_diff ===" | tee -a "$log"
if scripts/fuzz-diff.sh >>"$log" 2>&1; then
  echo "fuzz_diff=pass" >> "$results"
  echo "PASS fuzz_diff"
else
  echo "fuzz_diff=fail" >> "$results"
  echo "FAIL fuzz_diff"
  status=1
fi

# R4 bounded multi-committer model: five TLC runs (the corrected MW2 model and
# the two deliberate flaws). The script self-dispatches into the TLA court and
# bounds each run with TLA_TIMEOUT_SECS (default 600).
run_orchestrated_court tla tla bash scripts/run_tla.sh

# R4 MW2 correctness: mixed Rust/Go independent committers on one store
# (deterministic parity, disjoint and contended histories, cross-language
# verifiers, finite history check).
run_orchestrated_court multiwriter multiwriter_court bash scripts/multiwriter_court.sh

# R4 MW2 failure court: process faults at four publication stages in both
# languages, reopen + verify, outcome lookup, idempotent replay, delayed writer.
run_orchestrated_court multiwriter_failure multiwriter_failure_court \
  bash scripts/multiwriter_failure_court.sh

# Aggregate counts. Recounted from the results file so the receipt can never
# disagree with the per-court entries it prints below.
total="$(grep -c . "$results" || true)"
passed="$(grep -c '=pass$' "$results" || true)"
failed="$(grep -c '=fail$' "$results" || true)"
skipped="$(grep -c '=skip$' "$results" || true)"

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
  echo "  \"summary\": {"
  echo "    \"courts\": $total,"
  echo "    \"passed\": $passed,"
  echo "    \"failed\": $failed,"
  echo "    \"skipped\": $skipped"
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
  if [[ "$skipped" -gt 0 ]]; then
    echo "  \"skips\": {"
    first=1
    while IFS=$'\t' read -r label reason; do
      [[ -z "$label" ]] && continue
      if [[ "$first" -eq 1 ]]; then first=0; else echo ","; fi
      printf '    "%s": "%s"' "$label" "$reason"
    done < "$skips"
    echo ""
    echo "  },"
    echo "  \"result_note\": \"$skipped court(s) skipped explicitly; a skip is not a pass\","
  fi
  echo "  \"receipts\": {"
  receipts_count=0
  emit_receipt() {
    if [[ "$receipts_count" -ne 0 ]]; then echo ","; fi
    printf '    "%s": "%s"' "$1" "$2"
    receipts_count=$((receipts_count + 1))
  }
  emit_receipt ceph_rgw "bench/receipts/ceph-rgw-${stamp}.json"
  emit_receipt s3_court "bench/receipts/s3-court-${stamp}.json"
  if [[ -f "bench/receipts/augmentation-local-${stamp}.json" ]]; then
    emit_receipt augmentation_local "bench/receipts/augmentation-local-${stamp}.json"
  fi
  if [[ -f "bench/receipts/conformance-bundle-${stamp}.json" ]]; then
    emit_receipt conformance_bundle "bench/receipts/conformance-bundle-${stamp}.json"
  fi
  echo ""
  echo "  },"
  echo "  \"result\": \"$([ "$status" -eq 0 ] && echo pass || echo fail)\","
  echo "  \"raw_log\": \"$log\""
  echo "}"
} > "$receipt"

rm -f "$results" "$skips"
echo "receipt: $receipt (result=$([ "$status" -eq 0 ] && echo pass || echo fail), courts=$total passed=$passed failed=$failed skipped=$skipped)"
exit "$status"
