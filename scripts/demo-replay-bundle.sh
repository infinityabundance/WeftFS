#!/usr/bin/env bash
# F3.3 / A2 — Reproducible Agent Replay Bundle.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-replay-bundle.sh"
#
# The Rust frontend writes `weft-receipt.tar.zst` (store + context graph +
# manifest + replay.sh). The Go frontend, with no Rust at runtime, extracts it,
# opens the store, runs `verify --full`, resolves and materializes every
# Context Root, and re-hashes every store entry and output file against
# manifest.json.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

demo_init "weftfs.replay-bundle.receipt/1" "replay-bundle" "Reproducible Agent Replay Bundle (A2)"
note "Rust writes the bundle; Go extracts/opens/verifies/materializes it with no Rust at Go runtime"
note "bundle format is tar+zstd; no compression fallback exists"

S="$STORES/replay-bundle-$RUN_ID"
BUNDLE_DIR="$ROOT/bench/bundles"
BUNDLE="$BUNDLE_DIR/weft-receipt.tar.zst"
mkdir -p "$BUNDLE_DIR"
rm -rf "$S"
mkdir -p "$S"

step_begin "store" "Deterministic store with two Context Roots"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" init
run_ok create-a "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /a.txt --text "alpha" --seed "replay-a" --attr "project=demo"
run_ok create-b "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /b.txt --text "beta" --seed "replay-b" --attr "project=demo"
run_ok create-c "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /c.txt --text "gamma" --seed "replay-c" --attr "project=other"
run_ok snapshot "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" snapshot create s1
run_ok ctx-demo "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context create \
  --name "demo" --selector 'select attrs.project == "demo"' --snapshot s1
DEMO_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok ctx-other "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context create \
  --name "other" --selector 'select attrs.project == "other"' --snapshot s1
OTHER_ID="$(jsonq "$RUN_OUT" context_root_id)"
run_ok verify-source "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" verify --full
assert_eq "source store verifies before export" "true" "$(jsonq "$RUN_OUT" ok)"
step_end pass "contexts demo=$DEMO_ID other=$OTHER_ID"

step_begin "export" "Rust writes the self-contained bundle"
rm -f "$BUNDLE"
run_ok export "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" export-bundle \
  --out "$BUNDLE" --source-revision "WeftFS=${WEFT_REPO_WEFTFS:-unknown}"
BUNDLE_SHA="$(jsonq "$RUN_OUT" sha256)"
BUNDLE_BYTES="$(jsonq "$RUN_OUT" bytes)"
assert_eq "bundle path exists" "true" "$([ -f "$BUNDLE" ] && echo true || echo false)"
assert_eq "bundle sha256 matches the file" "$BUNDLE_SHA" "$(sha256_file "$BUNDLE")"
artifact_local "replay-bundle" "$BUNDLE"

EXTRACT="$SCRATCH/bundle-inspect-$RUN_ID"
rm -rf "$EXTRACT"; mkdir -p "$EXTRACT"
tar --zstd -xf "$BUNDLE" -C "$EXTRACT"
assert_eq "manifest.json present" "true" "$([ -f "$EXTRACT/manifest.json" ] && echo true || echo false)"
assert_eq "replay.sh present" "true" "$([ -f "$EXTRACT/replay.sh" ] && echo true || echo false)"
assert_eq "README present" "true" "$([ -f "$EXTRACT/README.md" ] && echo true || echo false)"
assert_eq "store commits present" "true" "$([ -d "$EXTRACT/store/commits" ] && echo true || echo false)"
assert_eq "store objects present" "true" "$([ -d "$EXTRACT/store/objects" ] && echo true || echo false)"
assert_eq "store manifests present" "true" "$([ -d "$EXTRACT/store/manifests" ] && echo true || echo false)"
assert_eq "store refs present" "true" "$([ -d "$EXTRACT/store/refs" ] && echo true || echo false)"
run_ok manifest-schema "$JSONQ" "$EXTRACT/manifest.json" schema
assert_eq "manifest schema" "weftfs.replay-bundle/1" "$(cat "$RUN_OUT")"
assert_eq "manifest records two contexts" "2" "$(jsonq "$EXTRACT/manifest.json" contexts#len)"
assert_eq "manifest records a context graph" "true" "$([ "$(jsonq "$EXTRACT/manifest.json" context_graph#len)" -ge 2 ] && echo true || echo false)"
assert_eq "manifest records source revisions" "true" "$([ "$(jsonq "$EXTRACT/manifest.json" source_revisions#len)" -ge 1 ] && echo true || echo false)"
save_out manifest "$EXTRACT/manifest.json"
step_end pass "bundle=$BUNDLE_SHA bytes=$BUNDLE_BYTES"

step_begin "go-replay" "Go extracts, verifies, resolves, and materializes every hash"
REPLAY_OUT="$SCRATCH/replay-out-$RUN_ID"
run_ok replay bash "$ROOT/scripts/replay-bundle.sh" --bundle "$BUNDLE" --out "$REPLAY_OUT"
assert_eq "replay exit" "0" "$RUN_STATUS"
assert_contains "replay reports ok" "$(cat "$RUN_OUT")" "replay: ok"
cp "$REPLAY_OUT/go-verify.json" "$STEP_DIR/go-verify.json"
assert_eq "Go verify --full OK" "true" "$(jsonq "$STEP_DIR/go-verify.json" OK)"
cp "$REPLAY_OUT/go-manifest-verification.json" "$STEP_DIR/go-manifest-verification.json"
assert_eq "Go manifest verification ok" "true" "$(jsonq "$STEP_DIR/go-manifest-verification.json" ok)"
assert_eq "Go checked store entries" "true" "$([ "$(jsonq "$STEP_DIR/go-manifest-verification.json" store_entries_checked)" -gt 0 ] && echo true || echo false)"
assert_eq "Go checked both contexts" "2" "$(jsonq "$STEP_DIR/go-manifest-verification.json" contexts_checked)"
assert_eq "Go checked context files" "true" "$([ "$(jsonq "$STEP_DIR/go-manifest-verification.json" context_files_checked)" -ge 3 ] && echo true || echo false)"
assert_eq "Go resolved identities" "2" "$(jsonq "$STEP_DIR/go-manifest-verification.json" resolved_ids_checked)"
assert_eq "Go found no mismatches" "0" "$(jsonq "$STEP_DIR/go-manifest-verification.json" "mismatches#len")"
save_out go-verify "$STEP_DIR/go-verify.json"
save_out go-manifest-verification "$STEP_DIR/go-manifest-verification.json"
step_end pass "Go verified every store entry and materialized hash"

step_begin "reproduction" "One-command reproduction recorded"
CMD="scripts/docker-court.sh fuse \"bash scripts/replay-bundle.sh --bundle bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out\""
record_cmd "$CMD"
record_assert "one-command reproduction" "$CMD" "$CMD" true
# The bundle's own replay.sh is self-contained aside from the Go binary, which
# the outer script builds. Confirm the embedded script checks the same files.
assert_contains "embedded replay.sh checks checks.sha256" "$(cat "$EXTRACT/replay.sh")" "sha256sum -c checks.sha256"
assert_contains "embedded replay.sh materializes" "$(cat "$EXTRACT/replay.sh")" "context materialize"
assert_contains "README names the court command" "$(cat "$EXTRACT/README.md")" "docker-court.sh fuse"
step_end pass "reproduction command and embedded replay script recorded"
