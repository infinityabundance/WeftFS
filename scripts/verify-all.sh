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

# Candidate C ABI declaration: valid C11 and C++17 under -Wall -Wextra -Werror.
# Compile-only; no library, no exported symbol. The rust court carries gcc/g++
# from build-essential; on a plain runner `cc`/`c++` serve the same role.
run_court rust "context_plane_header" \
  "bash scripts/check-context-plane-header.sh"

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

# Phase 1 independent verifier: clean-room Python (stdlib only) over the frozen
# context-plane record families. Host-runnable analysis, like the guards; the
# script prints exactly one `context_plane_verifier: skip <reason>` line and
# exits 78 when python3 is unavailable, which the aggregate records as a skip.
# It writes `bench/receipts/context-plane-verifier-<stamp>.json` + `.log`.
run_orchestrated_court context_plane_verifier context_plane_verifier \
  bash scripts/context-plane-verifier_court.sh

# X06/X07/X08/N08/N09 resource courts: the one stage budget, the active-handle
# cap, packed-plane races (Rust scenarios plus Rust<->Go handoff), the restore
# cost profile and the cross-stage stress court. The aggregate uses the
# reduced configuration (2 repetitions, restore sizes 1 and 10); the cited
# receipts `restore-budget|handle-cap|packed-races|restore-cost|stage-budget-
# 20261113T125506Z.json` are the full run. A host without Docker, or one missing
# a court image, prints exactly one `resource_courts: skip <reason>` line and
# exits 78; the aggregate records a skip, never a pass.
run_orchestrated_court resource_courts resource_courts \
  env WEFT_RESOURCE_REPS=2 WEFT_COST_REPS=2 WEFT_COST_SIZES=1,10 \
  bash scripts/resource_courts.sh --reduced

# The wheel long description is `python/README.md`, baked in at build time. It
# must not assert which version a registry serves, whether this artifact was
# uploaded, or a digest of itself: that sentence is false on upload and makes
# every rebuild differ. It must keep the stable replacement (unpinned install
# command, PyPI project URL, crates.io crate URL). Host-only and fast; a host
# without Python >= 3.8 skips with a printed reason (exit 78).
run_orchestrated_court long_description long_description \
  bash scripts/check_long_description_court.sh

# R16 independent S3 implementation: a Ceph RGW sidecar, the existing S3 backend
# suite against it, the eight-behaviour compatibility matrix against RGW and
# MinIO, and a side-by-side comparison.
run_orchestrated_court ceph_rgw ceph_court env WEFT_COMPAT_STAMP="$stamp" bash scripts/ceph_court.sh

# Q4/MinIO: the native S3 backend and fenced-writer suites against a real
# MinIO server in its own container. The same environment-only skip semantics
# as ceph_rgw apply.
run_orchestrated_court s3_court s3_court env WEFT_COMPAT_STAMP="$stamp" bash scripts/s3_court.sh

# X13/N14 Azure MW2 mixed-language court: one pinned Azurite sidecar and the
# eight S1-S8 scenarios (Rust and Go writer processes, ambiguous conditional
# head writes, fenced takeover, stale readers, concurrent GC, fail-closed mode
# detection, cross-language verification). The full configuration is the
# standalone one; it costs about half a minute plus the cached build. A host
# without Docker, or one that cannot obtain the pinned image, prints exactly one
# `azure_mw2_court: skip <reason>` line and exits 78; the aggregate records a
# skip, never a pass. Evidence: `bench/RESULTS-azure-mw2.md` and the
# `azure-mw2-<stamp>.json` receipts.
run_orchestrated_court azure_mw2 azure_mw2_court \
  env WEFT_AZURE_MW2_STAMP="$stamp" bash scripts/azure_mw2_court.sh

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

# N04: the bounded writer pipeline on a remote object backend. One real MinIO
# server, the six contract arms with the pipelined WeftFS rows, the provider
# fault adversarial court and the S3 fencing suite. The aggregate uses the
# frozen reduced subset with seven repetitions; the full published court is
# `augmentation-minio-20261113T190000Z.json` / `n04-object-pipeline-20261113T190000Z.json`,
# cited in `bench/RESULTS-object-pipeline.md`. A court that cannot run prints
# `object_pipeline_court: skip <reason>` and exits 78; the aggregate records a
# skip, never a pass.
run_orchestrated_court object_pipeline object_pipeline_court \
  env WEFT_N04_STAMP="$stamp" bash scripts/object_pipeline_court.sh --reduced

# T030/T038/T081: a real deployed Ceph 19.2.0 (mon+mgr+OSD+MDS) with CephFS
# mounted by ceph-fuse in a second hard-limited container, and the same
# A/N/B/C/D/E court rooted on that mount. The aggregate uses the frozen reduced
# subset with seven repetitions; the full published court is
# `augmentation-cephfs-20261114T040000Z.json`, cited in
# `bench/RESULTS-cephfs.md`. Single node, labelled; not a fleet or clustered
# result. A court that cannot run prints `cephfs_court: skip <reason>` and exits
# 78; the aggregate records a skip, never a pass.
run_orchestrated_court cephfs cephfs_court \
  env WEFT_CEPHFS_STAMP="$stamp" bash scripts/cephfs_court.sh --reduced

# T020/T021/T027: MW3 conflict domains, the T021 rebase and the T027 writer
# envelope. Reduced configuration: N=1/4/32 disjoint (400 commits each),
# same-domain N=8/32, compact 8/32/256/1024, and the T021 after arm only (a
# fresh checkout has no build of the start revision). The full court, with the
# T021 baseline and seven runs, is `scripts/mw3_court.sh`; the merged evidence
# is cited in `bench/RESULTS-mw3.md`. A court that cannot run prints exactly one
# `mw3: skip <reason>` line and exits 78; the aggregate records a skip, never a
# pass.
run_orchestrated_court mw3 mw3 bash scripts/mw3_verify_court.sh

# T024 multi-writer GC: epoch CAS, publication reservations, a concurrent sweep
# with process kills at every publication stage, an MW3 coordinated pass and
# readers under sweep. The receipt records both the retained roots and the
# genuinely unreachable blobs the control pass reclaimed. The script assembles
# its receipt with python3, so it runs in the fuse image (no mount is used).
run_court fuse "mw_gc" "bash scripts/mw_gc_court.sh"

# T025 distributed write/read failure court. It owns the MinIO sidecar and the
# fault-injecting proxy, so it is orchestrated from the host. A host without
# Docker, curl or an obtainable pinned MinIO release prints exactly one
# `distributed_faults_court: skip <reason>` line and exits 78; the aggregate
# records a skip, never a pass. One host, one backend instance, proxy-level
# faults: not a partition of physical nodes.
run_orchestrated_court distributed_faults distributed_faults_court \
  bash scripts/distributed_faults_court.sh

# T026 independent transactional history checker. The store emits
# `weft.history.v1` JSON Lines; `verifier/history_check.py` (stdlib only, no
# WeftFS import) decides per-key register linearizability, per-domain multi-key
# prefix consistency, conflict justification and handle monotonicity, and is
# required to reject six deliberately violating histories.
run_court fuse "history_check" "bash scripts/history_check_court.sh"

# T028 explicit writer-mode migration. Orchestrated because it exports the
# tagged v0.9.0 source on the host and builds that binary in the court, then
# runs the whole transition matrix, both crash sides of the CAS and a reader
# spanning the transition.
run_orchestrated_court mode_migration mode_migration_court \
  bash scripts/mode_migration_court.sh

# T029 FUSE mutations under optimistic publication. Opt-in only; the court also
# exercises the default MW1 mount so a refusal cannot leak into it.
run_court fuse "fuse_mw2" "bash scripts/fuse_mw2_court.sh"

# R26 Context Advantage Court: one backend at a time, three arms — the backend
# alone, a strong application-level metadata baseline, and the WeftFS context
# plane — interleaved with the arm order rotated per repetition. This court is
# the local POSIX data plane; `context_advantage_s3` below is the same court on
# one loopback MinIO process. The aggregate uses the reduced three-repetition
# configuration; the full seven-repetition receipts are
# `context-advantage-20261113T072921Z.json` and
# `context-advantage-s3-20261113T073016Z.json`, cited in
# `bench/RESULTS-context-advantage.md`, and the frozen contract is
# `bench/CONTRACTS-context-advantage.md`. A court that cannot run prints
# `context_advantage_court: skip <reason>` and exits 78; the aggregate records a
# skip, never a pass. The court's `claim_holds` is reported in the receipt and is
# deliberately independent of its measurement validity.
run_orchestrated_court context_advantage context_advantage_court \
  env WEFT_CONTEXT_ADVANTAGE_STAMP="$stamp" \
  bash scripts/context_advantage_court.sh --reduced

# R29 second backend for the same court: one loopback MinIO process, the same
# three arms, the same fixture and the same declared tolerances (contract §2.1,
# version 3). Reduced three-repetition configuration like the row above; the
# full seven-repetition receipt is `context-advantage-s3-20261113T073016Z.json`.
# A court that cannot run prints `context_advantage_court: skip <reason>` and
# exits 78; the aggregate records a skip, never a pass. Single-node loopback
# server: not a cloud, multi-node, fleet, media or power-loss result.
run_orchestrated_court context_advantage_s3 context_advantage_court \
  env WEFT_CONTEXT_ADVANTAGE_STAMP="$stamp" \
  bash scripts/context_advantage_court.sh --reduced --backend=s3

# EKBLOOM Context Court: one backend, three arms (W shipped resolver; M exact
# LRU and W-TinyLFU; E M plus an EKBLOOM-manifested certified decision table),
# four state-change events, nine interleaved repetitions in the cited receipt.
# The aggregate runs the reduced two-repetition configuration; the full
# receipt is `bench/receipts/ekbloom-context-20261114T100000Z.json`. A host
# without Docker, or one that cannot resolve the pinned `ekbloom 0.7.3`
# offline, prints exactly one `ekbloom_context_court: skip <reason>` line and
# exits 78; the aggregate records a skip, never a pass.
run_orchestrated_court ekbloom_context ekbloom_context_court \
  bash scripts/ekbloom_context_court.sh --reduced --phase=decisions

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

# Aged-store advisory-index court (T012/N13): reduced cells only (10k
# generations, 10k live objects with one checkpoint). Full cells 100k/1M are
# run by hand and cited in bench/RESULTS-incremental-index.md; the 1M
# live-object cell is not reachable (checkpoint state manifest item bound).
run_orchestrated_court index_aged index_aged \
  env WEFT_STAMP="$stamp" bash scripts/index_aged_court.sh
emit_receipt index_aged "bench/receipts/index-aged-court-${stamp}.json"

# DCP checkpoint completion: seven interleaved repetitions of the shipped
# metadata plan against the forced oracle encoder, plus the kill-after-ack
# durability control, on the R11 169-entry fixture.
run_court python "dcp_completion" \
  "cd /work && WEFT_DCPCOMPLETION_STAMP=$stamp WEFT_DCPCOMPLETION_REPS=7 bench/scripts/dcp_completion_court.sh"
emit_receipt dcp_completion "bench/receipts/dcp-latency-${stamp}.json"

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
  if [[ -f "bench/receipts/n04-object-pipeline-${stamp}.json" ]]; then
    emit_receipt object_pipeline "bench/receipts/n04-object-pipeline-${stamp}.json"
  fi
  if [[ -f "bench/receipts/cephfs-${stamp}.json" ]]; then
    emit_receipt cephfs "bench/receipts/cephfs-${stamp}.json"
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
