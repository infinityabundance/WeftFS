#!/usr/bin/env bash
# F5.2 / B4 — randomized POSIX torture.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash bench/scripts/posix_torture.sh"
#
# For each seed the court:
#   1. creates a fresh store with the writer implementation;
#   2. mounts it, runs >=2,000 randomized POSIX operations, verifying bytes and
#      metadata after every operation and at periodic full-tree scans;
#   3. unmounts, reopens the store with the *other* implementation and runs
#      `verify --full`;
#   4. mounts with the other implementation and checks the exact manifest
#      (names, types, modes, owners, nlink, mtime, content sha256, symlinks,
#      xattrs) against the writer's final tree.
#
# Both directions (Rust write -> Go read, Go write -> Rust read) are exercised
# for three deterministic seeds.
set -Eeuo pipefail

ROOT=/work
source "$ROOT/scripts/demo-lib.sh"

demo_init "weftfs.posix-torture.receipt/1" "posix-torture" \
  "Randomized POSIX torture (B4), both frontends, three seeds"

TORTURE="$BIN/posix_torture"
( cd /tmp && GO111MODULE=off go build -o "$TORTURE" "$ROOT/bench/scripts/posix_torture.go" )

# The harness counts only operations that actually ran (skips are reported
# separately), so request a surplus above the 2,000-operation floor.
OPS_REQUESTED=3200
OPS_MINIMUM=2000
SEEDS=(1 2 3)

start_mount_fixed() {
  local impl="$1" store="$2" mount="$3"; shift 3
  mkdir -p "$mount"
  MOUNT_PATHS+=("$mount")
  local logfile="$SCRATCH/mount-posix-$RUN_ID-$(basename "$mount").log"
  case "$impl" in
    rust)
      "$RUSTFS" --store "$store" --fixed-timestamp-ns "$TS1" mount \
        --mountpoint "$mount" --foreground "$@" >"$logfile" 2>&1 &
      ;;
    go)
      "$GOFS" --store "$store" --fixed-timestamp-ns "$TS1" mount \
        --mountpoint "$mount" "$@" >"$logfile" 2>&1 &
      ;;
    *) echo "unknown implementation $impl" >&2; return 1 ;;
  esac
  local pid=$!
  MOUNT_PIDS+=("$pid")
  if ! wait_mount "$mount"; then
    log "mount failed at $mount; daemon log:"; cat "$logfile" >&2 || true
    return 1
  fi
  record_cmd "$impl mount $store -> $mount"
}

assert_ge() {
  case "$3" in
    ''|*[!0-9]*) record_assert "$1" ">= $2" "$3" false ;;
    *) if [ "$3" -ge "$2" ]; then record_assert "$1" ">= $2" "$3" true; else record_assert "$1" ">= $2" "$3" false; fi ;;
  esac
}

require_file() {
  local label="$1" file="$2"
  if [ -s "$file" ]; then record_assert "$label present" present present true; return 0; fi
  record_assert "$label present" present "missing or empty: $file" false
  return 1
}

verify_store() {
  local impl="$1" store="$2" label="$3" key
  if [ "$impl" = rust ]; then
    run_any "$label" "$RUSTFS" --store "$store" --json verify --full
    key=ok
  else
    run_any "$label" "$GOFS" --store "$store" --json verify --full
    key=OK
  fi
  assert_eq "$label exit" 0 "$RUN_STATUS"
  assert_eq "$label ok" true "$(jsonq "$RUN_OUT" "$key")"
  save_out "$label" "$RUN_OUT"
}

run_direction() {
  local seed="$1" writer="$2" reader="$3"
  local tag="$writer-to-$reader-$seed"
  step_begin "seed-$seed-$writer-$reader" \
    "seed $seed: $writer writes, $reader verifies and remounts"

  local store="$STORES/posix-$tag"
  local manifest="$STEP_DIR/manifest-$tag.json"
  local result="$STEP_DIR/run-$tag.json"
  local mount1="$MOUNT_ROOT/posix-$RUN_ID-$tag-w"
  local mount2="$MOUNT_ROOT/posix-$RUN_ID-$tag-r"
  local init_cli="$GOFS"
  [ "$writer" = rust ] && init_cli="$RUSTFS"

  rm -rf "$store"
  mkdir -p "$store"
  run_ok "init-$tag" "$init_cli" \
    --store "$store" --fixed-timestamp-ns "$TS1" init

  start_mount_fixed "$writer" "$store" "$mount1"
  run_ok "torture-$tag" "$TORTURE" \
    --mount "$mount1" --impl "$writer" --seed "$seed" \
    --ops "$OPS_REQUESTED" --manifest "$manifest" --result "$result"
  if ! require_file "torture-$tag result" "$result"; then
    stop_mount "$mount1"
    step_end fail "torture result missing or empty"
    return 0
  fi
  save_out "run-$tag" "$result"
  assert_eq "torture-$tag ok" true "$(jsonq "$result" ok)"
  assert_eq "torture-$tag failures" 0 "$(jsonq "$result" failures#len)"
  assert_ge "torture-$tag completed ops" "$OPS_MINIMUM" "$(jsonq "$result" ops_completed)"
  assert_ne "torture-$tag manifest is non-empty" 0 "$(jsonq "$result" manifest_entries)"
  if ! require_file "torture-$tag manifest" "$manifest"; then
    stop_mount "$mount1"
    step_end fail "manifest missing or empty"
    return 0
  fi
  artifact_local "manifest-$tag" "$manifest"
  log "torture $tag: ops_completed=$(jsonq "$result" ops_completed) skipped=$(jsonq "$result" ops_skipped) entries=$(jsonq "$result" manifest_entries) manifest=$(jsonq "$result" manifest_sha256)"
  stop_mount "$mount1"

  verify_store "$reader" "$store" "verify-$tag"

  start_mount_fixed "$reader" "$store" "$mount2"
  run_ok "verify-only-$tag" "$TORTURE" \
    --mount "$mount2" --impl "$reader" --seed "$seed" \
    --verify-only --manifest "$manifest" --result "$STEP_DIR/verify-$tag.json"
  if ! require_file "verify-only-$tag result" "$STEP_DIR/verify-$tag.json"; then
    stop_mount "$mount2"
    step_end fail "verify-only result missing or empty"
    return 0
  fi
  save_out "verify-only-$tag" "$STEP_DIR/verify-$tag.json"
  assert_eq "verify-only-$tag ok" true "$(jsonq "$STEP_DIR/verify-$tag.json" ok)"
  assert_eq "verify-only-$tag failures" 0 "$(jsonq "$STEP_DIR/verify-$tag.json" failures#len)"
  assert_eq "verify-only-$tag manifest digest" \
    "$(jsonq "$manifest" digest)" "$(jsonq "$STEP_DIR/verify-$tag.json" manifest_sha256)"
  log "verify-only $tag: entries=$(jsonq "$STEP_DIR/verify-$tag.json" manifest_entries) digest=$(jsonq "$STEP_DIR/verify-$tag.json" manifest_sha256)"
  stop_mount "$mount2"

  step_end pass "writer=$writer reader=$reader ops=$(jsonq "$result" ops_completed) entries=$(jsonq "$result" manifest_entries)"
}

for seed in "${SEEDS[@]}"; do
  run_direction "$seed" rust go
  run_direction "$seed" go rust
done

note "randomized POSIX operation court; not pjdfstest/fsstress (those tools are not installed in the court and no substitute run is claimed)"
note "operations: create write read truncate append rename rename-over link unlink mkdir rmdir symlink readlink xattr fsync open-unlinked"
note "per-operation verification covers bytes, size, mode, nlink, symlink target, and user xattrs; periodic full-tree verification every 100 operations"
note "cross-implementation final state is compared on names, types, modes, uid/gid, nlink, mtime, content sha256, symlink targets, and xattrs"
