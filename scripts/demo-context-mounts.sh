#!/usr/bin/env bash
# F3.2 / A1 + D2 — Context Views as first-class mounts.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
#
# Builds a deterministic store with two byte-identical objects, typed relations
# (task, model, generated_by, derived_from, prompted_by), two snapshots, and
# three Context Roots (by:task, by:model, at:<timestamp>). Mounts all three
# simultaneously, mixing the Rust and Go frontends, and machine-checks every
# listing, byte, SHA-256, BLAKE3 content id, and read-only denial.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

# Scaled mode (WEFT_CONTEXT_MOUNTS != 3): one store with 2N corpus objects,
# N/4 model objects, 10 grounding objects, 3*2N-1 typed relations, and exactly
# N distinct Context Roots (N/2 by task, N/4 by model, N/4 by derived_from view),
# where N is the requested mount count. All requested daemons are spawned
# before any is awaited, so mount latency is measured under concurrency rather
# than one at a time.
#
# --- scaled corpus builders -------------------------------------------------
# The 100-mount court keeps its original one-commit-per-operation CLI build.
# Above 100 the corpus is built through the shipped store API with group
# commits (`examples/context_mount_corpus.rs`) so that commit-chain replay does
# not dominate the mount measurement. Both builders assign the caller's
# CTX_NAMES (and SHA_BY_PATH for the CLI path); bash locals are dynamically
# scoped, so the builders never re-declare those names.
scaled_cli_build() {
  local BULK_OK=0 BULK_FAIL=0 BULK_LAST_OUT="" SCALED_ABORT=0
  bulk_create() {
    local label="$1"; shift
    BULK_LAST_OUT="$STEP_DIR/bulk-$label.out"
    if "$@" >"$BULK_LAST_OUT" 2>"$STEP_DIR/bulk-$label.err"; then
      BULK_OK=$((BULK_OK + 1))
    else
      local status=$?
      BULK_FAIL=$((BULK_FAIL + 1))
      record_assert "bulk $label exit" 0 "$status" false
    fi
  }
  bulk_append() {
    local label="$1"; shift
    if "$@" >>"$STEP_DIR/bulk.log" 2>"$STEP_DIR/bulk.err"; then
      BULK_OK=$((BULK_OK + 1))
    else
      local status=$?
      BULK_FAIL=$((BULK_FAIL + 1))
      record_assert "bulk $label exit" 0 "$status" false
    fi
  }

  declare -a GROUND_OID=() MODEL_OID=() CORPUS_OID=()

  run_ok init "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" init
  run_ok mkdir-corpus "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" mkdir /corpus --seed dir-corpus
  run_ok mkdir-models "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" mkdir /models --seed dir-models
  run_ok mkdir-grounding "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" mkdir /grounding --seed dir-grounding

  local k content path sha oid
  for k in $(seq 0 $((ground - 1))); do
    printf -v content 'grounding doc %02d\n' "$k"
    path="/grounding/g-$(printf '%02d' "$k").txt"
    sha="$(printf '%s' "$content" | sha256sum | awk '{print $1}')"
    bulk_create "ground-$k" "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" \
      create "$path" --text "$content" --seed "ground-$k" --attr "sha256=$sha"
    oid="$(jsonq "$BULK_LAST_OUT" object_id 2>/dev/null || true)"
    if [ -z "$oid" ]; then record_assert "grounding $k object id" present missing false; SCALED_ABORT=1; break; fi
    GROUND_OID[$k]="$oid"
    SHA_BY_PATH["$path"]="$sha"
  done
  if [ "$SCALED_ABORT" = 0 ]; then
    for k in $(seq 0 $((models - 1))); do
      printf -v content 'model llama-4 ver_%02d\n' "$k"
      path="/models/llama-4-ver_$(printf '%02d' "$k").bin"
      sha="$(printf '%s' "$content" | sha256sum | awk '{print $1}')"
      bulk_create "model-$k" "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" \
        create "$path" --text "$content" --seed "model-$k" --attr "sha256=$sha"
      oid="$(jsonq "$BULK_LAST_OUT" object_id 2>/dev/null || true)"
      if [ -z "$oid" ]; then record_assert "model $k object id" present missing false; SCALED_ABORT=1; break; fi
      MODEL_OID[$k]="$oid"
      SHA_BY_PATH["$path"]="$sha"
    done
  fi
  if [ "$SCALED_ABORT" = 0 ]; then
    for k in $(seq 0 $((files - 1))); do
      printf -v content 'corpus file %03d task agent-%02d model llama-4@ver_%02d\n' \
        "$k" "$((k % agents))" "$((k % models))"
      path="/corpus/f-$(printf '%03d' "$k").bin"
      sha="$(printf '%s' "$content" | sha256sum | awk '{print $1}')"
      bulk_create "corpus-$k" "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" \
        create "$path" --text "$content" --seed "corpus-$k" \
        --attr "task=agent-$((k % agents))" --attr "model=llama-4@ver_$((k % models))" --attr "sha256=$sha"
      oid="$(jsonq "$BULK_LAST_OUT" object_id 2>/dev/null || true)"
      if [ -z "$oid" ]; then record_assert "corpus $k object id" present missing false; SCALED_ABORT=1; break; fi
      CORPUS_OID[$k]="$oid"
      SHA_BY_PATH["$path"]="$sha"
    done
  fi
  assert_eq "object creates without error" 0 "$BULK_FAIL"
  if [ "$SCALED_ABORT" = 1 ]; then
    return 1
  fi

  # Typed relations: generated_by, grounded_in, derived_from.
  record_cmd "relation add --subject <corpus[i]> --predicate generated_by --object <model[i % $models]> ($files times)"
  record_cmd "relation add --subject <corpus[i]> --predicate grounded_in --object <ground[i % $ground]> ($files times)"
  record_cmd "relation add --subject <corpus[i]> --predicate derived_from --object <corpus[i-1]> ($((files - 1)) times)"
  local i
  for i in $(seq 0 $((files - 1))); do
    bulk_append "rel-generated-$i" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
      relation add --subject "${CORPUS_OID[$i]}" --predicate generated_by --object "${MODEL_OID[$((i % models))]}"
    bulk_append "rel-grounded-$i" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
      relation add --subject "${CORPUS_OID[$i]}" --predicate grounded_in --object "${GROUND_OID[$((i % ground))]}"
    if [ "$i" -gt 0 ]; then
      bulk_append "rel-derived-$i" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
        relation add --subject "${CORPUS_OID[$i]}" --predicate derived_from --object "${CORPUS_OID[$((i - 1))]}"
    fi
  done
  assert_eq "relation adds without error" 0 "$BULK_FAIL"

  run_ok snapshot-all "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" snapshot create all

  # agents task contexts, models model contexts, derived_views derived views.
  local n name selector p d
  for n in $(seq 0 $((agents - 1))); do
    name="by:task=agent-$n"
    selector="select attrs.task == \"agent-$n\""
    bulk_append "ctx-task-$n" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
      context create --name "$name" --selector "$selector" --snapshot all
    CTX_NAMES+=("$name")
  done
  for n in $(seq 0 $((models - 1))); do
    name="by:model=llama-4@ver_$n"
    selector="select attrs.model == \"llama-4@ver_$n\""
    bulk_append "ctx-model-$n" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
      context create --name "$name" --selector "$selector" --snapshot all
    CTX_NAMES+=("$name")
  done
  for d in $(seq 0 $((derived_views - 1))); do
    p=$((d * step))
    name="by:derived=f-$(printf '%03d' "$p")"
    selector="select relation(derived_from, obj:${CORPUS_OID[$p]})"
    bulk_append "ctx-derived-$p" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" \
      context create --name "$name" --selector "$selector" --snapshot all
    CTX_NAMES+=("$name")
  done
  assert_eq "context creation without error" 0 "$BULK_FAIL"
  return 0
}

# Build the same logical corpus through the shipped store API with group
# commits. The harness writes objects.tsv (object id, path, declared sha256) and
# contexts.tsv (name, selector, member object ids).
scaled_harness_build() {
  local out="$STEP_DIR/corpus"
  rm -rf "$out"
  mkdir -p "$out"
  run_ok corpus-harness "$BIN/context-corpus" --target "$target" --store "$SS" --out "$out"
  cp "$out/corpus.json" "$STEP_DIR/corpus.json" 2>/dev/null || true
  CORPUS_MANIFEST_DIR="$out"
  local oid path sha name selector members
  while IFS=$'\t' read -r oid path sha; do
    [ -n "$path" ] || continue
    SHA_BY_PATH["$path"]="$sha"
  done < "$out/objects.tsv"
  while IFS=$'\t' read -r name selector members; do
    [ -n "$name" ] || continue
    CTX_NAMES+=("$name")
  done < "$out/contexts.tsv"
  assert_eq "harness declared objects" "$((files + models + ground))" \
    "$(wc -l < "$out/objects.tsv" | tr -d ' ')"
  assert_eq "harness declared Context Roots" "$target" \
    "$(wc -l < "$out/contexts.tsv" | tr -d ' ')"
}

run_scaled_mounts() {
  local target="$1"
  local files=$((target * 2))
  local agents=$((target / 2))
  local models=$((target / 4))
  local derived_views=$((target / 4))
  local ground=10
  local step=$((files / derived_views))
  local relation_count=$((files * 3 - 1))
  local object_count=$((files + models + ground + 4))
  local corpus_build="${WEFT_CONTEXT_CORPUS_BUILD:-}"
  if [ -z "$corpus_build" ]; then
    corpus_build=cli
    [ "$target" -gt 100 ] && corpus_build=harness
  fi
  case "$corpus_build" in
    cli|harness) ;;
    *) echo "WEFT_CONTEXT_CORPUS_BUILD=$corpus_build unsupported: use cli or harness" >&2; exit 64 ;;
  esac
  local mount_mode="${WEFT_CONTEXT_MOUNTS_MODE:-per-daemon}"
  case "$mount_mode" in
    per-daemon|shared) ;;
    *) echo "WEFT_CONTEXT_MOUNTS_MODE=$mount_mode unsupported: use per-daemon or shared" >&2; exit 64 ;;
  esac
  CORPUS_MANIFEST_DIR=""
  local SS="$STORES/context-mounts-$RUN_ID-n$target"
  rm -rf "$SS"
  mkdir -p "$SS"

  declare -a CTX_NAMES=()
  declare -A SHA_BY_PATH=()

  step_begin "store" "$files corpus objects, $models models, $ground grounding docs, $relation_count typed relations, $target Context Roots"
  meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; explicit memory/cpu/pids/nproc overrides for the scale court (exact values in environment); not a fleet claim"
  meta metric.target_mounts "$target"
  meta metric.corpus_build "$corpus_build"
  meta metric.mount_mode "$mount_mode"
  meta metric.corpus_files "$files"
  meta metric.corpus_models "$models"
  meta metric.corpus_relations "$relation_count"
  meta metric.corpus_contexts "$target"
  note "per-daemon memory (metrics.daemon_rss_kb and daemon_rss_max_kb) is why fitting many concurrent context mounts under the default 3 GiB court needs a shared multi-mount daemon; this run uses explicit, recorded court limit overrides instead"

  if [ "$corpus_build" = harness ]; then
    scaled_harness_build
  else
    if ! scaled_cli_build; then
      step_end fail "store construction aborted; no mounts attempted"
      return 0
    fi
  fi

  run_ok rel-count-derived "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" relation list --predicate derived_from
  assert_eq "$((files - 1)) derived_from edges" "$((files - 1))" "$(jsonq "$RUN_OUT" relations#len)"
  run_ok rel-count-generated "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" relation list --predicate generated_by
  assert_eq "$files generated_by edges" "$files" "$(jsonq "$RUN_OUT" relations#len)"
  run_ok rel-count-grounded "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" relation list --predicate grounded_in
  assert_eq "$files grounded_in edges" "$files" "$(jsonq "$RUN_OUT" relations#len)"
  run_ok ctx-list "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" context list
  assert_eq "$target Context Roots persisted" "$target" "$(jsonq "$RUN_OUT" contexts#len)"
  assert_eq "$target Context Roots loaded by the demo" "$target" "${#CTX_NAMES[@]}"
  run_ok info "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" info
  save_out info "$RUN_OUT"
  assert_eq "corpus objects" "$object_count" "$(jsonq "$RUN_OUT" objects)"
  assert_eq "corpus relations" "$((relation_count + target))" "$(jsonq "$RUN_OUT" relations)"
  run_ok verify-full "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" verify --full
  step_end pass "objects=$object_count relations=$((relation_count + target)) contexts=$target target_mounts=$target build=$corpus_build"

  # ---- spawn every mount before awaiting any of them --------------------
  step_begin "mounts" "$target concurrent Context mounts ($mount_mode)"
  local free_before free_after cgroup_peak mount_wall_start
  local LAT_FILE="$STEP_DIR/latencies.txt"
  free_before="$(sample_free_mb)"
  mount_wall_start="$(now_ns)"
  : > "$LAT_FILE"
  declare -a M_MOUNT=() M_IMPL=() M_PID=() M_T0=() M_NAME=() M_STATE=()
  local impl mount t0 i
  local spawned=0 ceiling_hit=0
  local pids_max pids_used reserve
  pids_max="$(cgroup_pids_max)"
  reserve="${WEFT_SPAWN_RESERVE:-256}"
  meta metric.cgroup_pids_max "$pids_max"
  meta metric.spawn_reserve_tasks "$reserve"
  local SHARED_PID=""
  if [ "$mount_mode" = shared ]; then
    # One process, one Store, N read-only views. The spec is the strict JSON
    # schema documented in docs/shared-mounts.md.
    local spec="$STEP_DIR/mount-spec.json"
    {
      printf '{"version":1,"views":['
      for i in $(seq 0 $((target - 1))); do
        [ "$i" -gt 0 ] && printf ','
        mount="$MOUNT_ROOT/f3m-$RUN_ID-$i"
        mkdir -p "$mount"
        printf '{"mountpoint":"%s","context":"%s"}' "$mount" "${CTX_NAMES[$i]}"
      done
      printf ']}'
    } > "$spec"
    meta metric.mount_spec "$(rel "$spec")"
    start_shared_mount_async "$SS" "$spec"
    SHARED_PID="$MOUNT_PID_LAST"
    if [ -z "$SHARED_PID" ] || ! kill -0 "$SHARED_PID" 2>/dev/null; then
      record_assert "shared server started" running "exited immediately: $(tail -3 "$MOUNT_LOG_LAST" 2>/dev/null | tr '\n' ' ')" false
    else
      spawned=$target
      for i in $(seq 0 $((target - 1))); do
        M_MOUNT[$i]="$MOUNT_ROOT/f3m-$RUN_ID-$i"
        M_IMPL[$i]=shared
        M_PID[$i]="$SHARED_PID"
        M_T0[$i]="$mount_wall_start"
        M_NAME[$i]="${CTX_NAMES[$i]}"
        M_STATE[$i]=0
      done
    fi
    meta metric.shared_server_pid "$SHARED_PID"
    meta metric.mount_processes 1
    meta metric.spawned_daemons 1
  else
    # Spawning must leave cgroup task headroom for the verification CLIs that
    # run while every daemon is still alive. The ceiling is a bounded hard limit
    # (`--pids-limit`), never a disabled one.
    for i in $(seq 0 $((target - 1))); do
      if [ "$pids_max" != "unavailable" ] && [ "$pids_max" -gt 0 ]; then
        pids_used="$(cgroup_pids_current)"
        if [ "$pids_used" != "unavailable" ] && [ $((pids_used + reserve)) -ge "$pids_max" ]; then
          ceiling_hit=1
          note "spawn ceiling: cgroup pids.current=$pids_used pids.max=$pids_max reserve=$reserve reached before mount $i; started $spawned of $target daemons"
          record_assert "daemon spawns under the cgroup pids limit" "mount $i of $target" \
            "ceiling: pids.current=$pids_used pids.max=$pids_max" false
          break
        fi
      fi
      mount="$MOUNT_ROOT/f3m-$RUN_ID-$i"
      if [ $((i % 2)) -eq 0 ]; then impl=rust; else impl=go; fi
      t0="$(now_ns)"
      if ! start_mount_async "$impl" "$SS" "$mount" --context "${CTX_NAMES[$i]}" \
          || [ -z "$MOUNT_PID_LAST" ] || ! kill -0 "$MOUNT_PID_LAST" 2>/dev/null; then
        note "spawn failed at mount $i ($impl ${CTX_NAMES[$i]})"
        record_assert "daemon spawns under the cgroup pids limit" "mount $i of $target" "spawn failed" false
        break
      fi
      M_MOUNT[$i]="$mount"; M_IMPL[$i]="$impl"; M_PID[$i]="$MOUNT_PID_LAST"
      M_T0[$i]="$t0"; M_NAME[$i]="${CTX_NAMES[$i]}"; M_STATE[$i]=0
      spawned=$((spawned + 1))
    done
    meta metric.spawned_daemons "$spawned"
    meta metric.mount_processes "$spawned"
  fi
  meta metric.spawn_ceiling_hit "$ceiling_hit"

  local mounted=0 failed=0 t1 now
  local poll_ms="${WEFT_MOUNT_POLL_MS:-200}"
  local deadline_s="${WEFT_MOUNT_DEADLINE_S:-}"
  if [ -z "$deadline_s" ]; then
    if [ "$target" -gt 100 ]; then deadline_s=1800; else deadline_s=30; fi
  fi
  meta metric.mount_poll_ms "$poll_ms"
  meta metric.mount_deadline_s "$deadline_s"
  if [ "$target" -gt 100 ]; then
    # Bulk poll: one /proc/self/mountinfo scan per interval instead of a
    # `mountpoint` child per mount per poll.
    local pending="$spawned" deadline_ns
    deadline_ns=$(( mount_wall_start + deadline_s * 1000000000 ))
    while [ "$pending" -gt 0 ]; do
      unset NOW_MOUNTED
      declare -A NOW_MOUNTED=()
      while IFS=' ' read -r _id _parent _dev _root mp _rest; do
        NOW_MOUNTED["$mp"]=1
      done < /proc/self/mountinfo
      now="$(now_ns)"
      for i in $(seq 0 $((spawned - 1))); do
        [ "${M_STATE[$i]}" = 0 ] || continue
        if [ -n "${NOW_MOUNTED[${M_MOUNT[$i]}]:-}" ]; then
          echo "$(( (now - M_T0[$i]) / 1000000 ))" >> "$LAT_FILE"
          M_STATE[$i]=1; mounted=$((mounted + 1)); pending=$((pending - 1))
        elif ! kill -0 "${M_PID[$i]}" 2>/dev/null; then
          M_STATE[$i]=2; failed=$((failed + 1)); pending=$((pending - 1))
          record_assert "mount $i (${M_IMPL[$i]} ${M_NAME[$i]}) started" mounted \
            "daemon exited: $(tail -3 "$SCRATCH/mount-$RUN_ID-$(basename "${M_MOUNT[$i]}").log" 2>/dev/null | tr '\n' ' ')" false
        fi
      done
      if [ "$pending" -gt 0 ] && [ "$(now_ns)" -ge "$deadline_ns" ]; then
        for i in $(seq 0 $((spawned - 1))); do
          [ "${M_STATE[$i]}" = 0 ] || continue
          M_STATE[$i]=2; failed=$((failed + 1)); pending=$((pending - 1))
          record_assert "mount $i (${M_IMPL[$i]} ${M_NAME[$i]}) started" mounted \
            "deadline ${deadline_s}s exceeded" false
        done
      fi
      [ "$pending" -gt 0 ] && sleep 0.2
    done
  else
    local tries_limit="${WEFT_WAIT_MOUNT_TRIES:-1500}"
    meta metric.wait_mount_tries "$tries_limit"
    for i in $(seq 0 $((spawned - 1))); do
      if wait_mount "${M_MOUNT[$i]}" 0.02 "$tries_limit"; then
        t1="$(now_ns)"
        echo "$(( (t1 - M_T0[$i]) / 1000000 ))" >> "$LAT_FILE"
        M_STATE[$i]=1; mounted=$((mounted + 1))
      else
        M_STATE[$i]=2; failed=$((failed + 1))
        if [ "$mount_mode" = shared ]; then
          record_assert "mount $i (${M_IMPL[$i]} ${M_NAME[$i]}) started" mounted \
            "failed: $(tail -3 "$MOUNT_LOG_LAST" 2>/dev/null | tr '\n' ' ')" false
        else
          record_assert "mount $i (${M_IMPL[$i]} ${M_NAME[$i]}) started" mounted \
            "failed: $(tail -3 "$SCRATCH/mount-$RUN_ID-$(basename "${M_MOUNT[$i]}").log" 2>/dev/null | tr '\n' ' ')" false
        fi
      fi
    done
  fi
  free_after="$(sample_free_mb)"
  cgroup_peak="$(cgroup_peak_bytes)"
  local all_mounted_ms mount_latency_min
  all_mounted_ms="$(( ( $(now_ns) - mount_wall_start ) / 1000000 ))"
  mount_latency_min="$(sort -n "$LAT_FILE" 2>/dev/null | head -1)"
  meta metric.all_mounted_ms "$all_mounted_ms"
  meta metric.mount_latency_min_ms "${mount_latency_min:-n/a}"
  if [ "$mount_mode" = shared ]; then
    if [ -n "$SHARED_PID" ]; then sample_daemons "$SHARED_PID"; else sample_daemons; fi
  else
    sample_daemons "${M_PID[@]}"
  fi
  meta metric.mounts_verified "$mounted"
  meta metric.mount_failures "$failed"
  meta metric.free_mb_before "${free_before:-unavailable}"
  meta metric.free_mb_after "${free_after:-unavailable}"
  meta metric.daemon_rss_kb "$DAEMON_RSS_KB"
  meta metric.daemon_rss_max_kb "$DAEMON_RSS_MAX_KB"
  meta metric.daemon_threads "$DAEMON_THREADS"
  meta metric.daemon_threads_max "$DAEMON_THREADS_MAX"
  meta metric.daemons_sampled "$DAEMON_SAMPLED"
  meta metric.daemons_missing_at_sample "$DAEMON_MISSING"
  meta metric.cgroup_peak_bytes "$cgroup_peak"
  meta metric.cgroup_oom_kills "$(cgroup_oom_kills)"
  meta metric.cgroup_pids_max_events "$(cgroup_pids_events_max)"
  record_assert "all $target mounts started" "$target" "$mounted" \
    "$([ "$mounted" -eq "$target" ] && echo true || echo false)"
  if [ "$mounted" -lt "$target" ]; then
    note "mount failure: requested $target, spawned $spawned, mounted $mounted, failed $failed; the exact daemon/resource error is in the assertion above and the per-mount logs under $SCRATCH"
    meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; mode=$mount_mode; explicit memory/cpu/pids/nproc overrides for the scale court (exact values in environment); INCOMPLETE $mounted/$target mounts verified (resource failure recorded); not a fleet claim"
  else
    meta label "single-host Docker FUSE court; synthetic deterministic fixtures; local store on the court bind mount; mode=$mount_mode; explicit memory/cpu/pids/nproc overrides for the scale court (exact values in environment); $mounted/$target mounts verified; not a fleet claim"
  fi
  step_end "$([ "$failed" -eq 0 ] && echo pass || echo fail)" "spawned=$spawned mounted=$mounted failed=$failed"

  # ---- verify every mounted context against its resolution --------------
  step_begin "verify" "Per-mount ls digest, every file SHA-256, representative CLI bytes and ContentId"
  local FILECHECK=0 j count oid path cid base prefix suffix candidate declared cli_sha actual_digest expected_digest
  : > "$STEP_DIR/digests.txt"
  : > "$STEP_DIR/all-paths.txt"
  local verified=0
  local resolved=0
  local resolve_sample="${WEFT_VERIFY_RESOLVE_SAMPLE:-50}"
  meta metric.resolve_sample_every "$resolve_sample"
  declare -A CTX_MEMBERS=() OBJ_PATH=()
  if [ "$corpus_build" = harness ]; then
    while IFS=$'\t' read -r name selector members; do
      [ -n "$name" ] || continue
      CTX_MEMBERS["$name"]="$members"
    done < "$CORPUS_MANIFEST_DIR/contexts.tsv"
    while IFS=$'\t' read -r oid path sha; do
      [ -n "$oid" ] || continue
      OBJ_PATH["$oid"]="$path"
    done < "$CORPUS_MANIFEST_DIR/objects.tsv"
    assert_eq "construction oracle contexts loaded" "$target" "${#CTX_MEMBERS[@]}"
    assert_eq "construction oracle objects loaded" "$((files + models + ground))" "${#OBJ_PATH[@]}"
  fi
  for i in $(seq 0 $((spawned - 1))); do
    [ "${M_STATE[$i]}" = 1 ] || continue
    mountpoint -q "${M_MOUNT[$i]}" 2>/dev/null || continue
    if ! ( cd "${M_MOUNT[$i]}" 2>/dev/null && ls -1 >/dev/null 2>&1 ); then
      record_assert "mount $i listing readable" readable \
        "unreadable: Transport endpoint is not connected (daemon gone)" false
      M_STATE[$i]=3
      continue
    fi
    verified=$((verified + 1))
    # Above 100 mounts the expected result set comes from the construction
    # oracle (the harness manifest), and `context resolve` is cross-checked on a
    # deterministic sample. Below that the original per-mount resolve is kept.
    local do_resolve=1
    if [ "$corpus_build" = harness ]; then
      do_resolve=0
      [ $((i % resolve_sample)) -eq 0 ] && do_resolve=1
    fi
    : > "$STEP_DIR/results-$i.tsv"
    if [ "$do_resolve" = 1 ]; then
      resolved=$((resolved + 1))
      run_ok "resolve-$i" "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" \
        context resolve "${M_NAME[$i]}"
      if [ "$RUN_STATUS" -ne 0 ]; then
        record_assert "context ${M_NAME[$i]} resolved" resolved "resolve failed: exit $RUN_STATUS" false
        M_STATE[$i]=3
        continue
      fi
      cp "$RUN_OUT" "$STEP_DIR/resolve-$i.json"
      count="$(jsonq "$STEP_DIR/resolve-$i.json" returned 2>/dev/null || echo 0)"
      record_assert "context ${M_NAME[$i]} returned results" "1.." "$count" \
        "$([ "$count" -ge 1 ] && echo true || echo false)"
      j=0
      while [ "$j" -lt "$count" ]; do
        oid="$(jsonq "$STEP_DIR/resolve-$i.json" "results.$j.object_id" 2>/dev/null || true)"
        path="$(jsonq "$STEP_DIR/resolve-$i.json" "results.$j.path" 2>/dev/null || true)"
        cid="$(jsonq "$STEP_DIR/resolve-$i.json" "results.$j.content_id" 2>/dev/null || true)"
        [ -n "$path" ] || break
        printf '%s\t%s\t%s\n' "$oid" "$path" "$cid" >> "$STEP_DIR/results-$i.tsv"
        printf '%s\n' "$path" >> "$STEP_DIR/all-paths.txt"
        j=$((j + 1))
      done
      if [ "$corpus_build" = harness ]; then
        cut -f1 "$STEP_DIR/results-$i.tsv" | sort -u > "$STEP_DIR/resolved-ids-$i.txt"
        manifest_members="${CTX_MEMBERS[${M_NAME[$i]}]:-}"
        printf '%s\n' "$manifest_members" | tr ',' '\n' | sed '/^$/d' | sort -u \
          > "$STEP_DIR/manifest-ids-$i.txt"
        assert_file_eq "mount $i resolver result ids match the construction oracle" \
          "$STEP_DIR/manifest-ids-$i.txt" "$STEP_DIR/resolved-ids-$i.txt"
      fi
    else
      manifest_members="${CTX_MEMBERS[${M_NAME[$i]}]:-}"
      if [ -z "$manifest_members" ]; then
        record_assert "construction oracle membership for ${M_NAME[$i]}" present missing false
        M_STATE[$i]=3
        continue
      fi
      IFS=',' read -r -a MEMBER_IDS <<< "$manifest_members"
      count=0
      for oid in "${MEMBER_IDS[@]}"; do
        path="${OBJ_PATH[$oid]:-}"
        if [ -z "$path" ]; then
          record_assert "construction oracle path for $oid" present missing false
          continue
        fi
        printf '%s\t%s\t\n' "$oid" "$path" >> "$STEP_DIR/results-$i.tsv"
        printf '%s\n' "$path" >> "$STEP_DIR/all-paths.txt"
        count=$((count + 1))
      done
      record_assert "context ${M_NAME[$i]} oracle members" "${#MEMBER_IDS[@]}" "$count" \
        "$([ "$count" -eq "${#MEMBER_IDS[@]}" ] && echo true || echo false)"
      unset MEMBER_IDS
    fi
    declare -A USED=()
    : > "$STEP_DIR/expected-$i.txt"
    local first=1
    while IFS=$'\t' read -r oid path cid; do
      if [ -z "$path" ] || [ -z "$oid" ]; then
        record_assert "mount $i result names an object and a path" present \
          "oid=<$oid> path=<$path>" false
        continue
      fi
      base="$(basename "$path")"
      prefix=8
      while :; do
        suffix="${oid:0:prefix}"
        candidate="$base--$suffix"
        if [ -z "${USED[$candidate]:-}" ] || [ "$prefix" -ge 64 ]; then break; fi
        prefix=$((prefix + 8)); [ "$prefix" -gt 64 ] && prefix=64
      done
      USED[$candidate]=1
      printf '%s\n' "$candidate" >> "$STEP_DIR/expected-$i.txt"
      declared="${SHA_BY_PATH[$path]:-}"
      if [ -z "$declared" ]; then
        record_assert "declared sha256 for $path" present missing false
      else
        assert_sha256 "mount $i $candidate" "${M_MOUNT[$i]}/$candidate" "$declared"
        FILECHECK=$((FILECHECK + 1))
      fi
      if [ "$first" = 1 ]; then
        first=0
        run_ok "cat-$i" "$RUSTFS" --store "$SS" --fixed-timestamp-ns "$TS1" cat "$path"
        cli_sha="$(sha256_file "$RUN_OUT")"
        assert_eq "mount $i representative CLI bytes" "$declared" "$cli_sha"
        if [ -n "$cid" ]; then
          run_ok "stat-$i" "$RUSTFS" --store "$SS" --json --fixed-timestamp-ns "$TS1" stat "$path"
          assert_eq "mount $i representative ContentId" "$cid" "$(jsonq "$RUN_OUT" content_id 2>/dev/null || echo unavailable)"
          assert_eq "mount $i declared sha256 matches store attrs" "$declared" "$(jsonq "$RUN_OUT" attrs.sha256 2>/dev/null || echo unavailable)"
        fi
      fi
    done < <(sort -t$'\t' -k1,1 "$STEP_DIR/results-$i.tsv")
    sort "$STEP_DIR/expected-$i.txt" > "$STEP_DIR/expected-sorted-$i.txt"
    if ! ( cd "${M_MOUNT[$i]}" 2>/dev/null && ls -1 | grep -v '^\.weft$' | sort ) > "$STEP_DIR/actual-sorted-$i.txt" 2>/dev/null; then
      record_assert "mount $i listing readable" readable "unreadable while listing" false
      M_STATE[$i]=3
      continue
    fi
    assert_file_eq "mount $i ls names" "$STEP_DIR/expected-sorted-$i.txt" "$STEP_DIR/actual-sorted-$i.txt"
    awk '{printf "f ./%s\n", $0}' "$STEP_DIR/expected-$i.txt" | LC_ALL=C sort > "$STEP_DIR/expected-find-$i.txt"
    expected_digest="$(sha256_file "$STEP_DIR/expected-find-$i.txt")"
    if ! actual_digest="$(listing_digest "${M_MOUNT[$i]}")"; then
      record_assert "mount $i listing digest read" readable "unreadable while hashing the listing" false
      M_STATE[$i]=3
      continue
    fi
    assert_eq "mount $i listing digest" "$expected_digest" "$actual_digest"
    printf '%s\t%s\n' "$i" "$actual_digest" >> "$STEP_DIR/digests.txt"
  done
  local unique_digests unique_paths
  unique_digests="$(cut -f2 "$STEP_DIR/digests.txt" | sort -u | wc -l | tr -d ' ')"
  unique_paths="$(sort -u "$STEP_DIR/all-paths.txt" | wc -l | tr -d ' ')"
  assert_eq "verified mounts" "$mounted" "$verified"
  assert_eq "every context listing is distinct" "$verified" "$unique_digests"
  assert_eq "$files distinct objects covered by all mounted roots" "$files" "$unique_paths"
  local p50 p95 pmax
  p50="$(percentile_file "$LAT_FILE" 50)"
  p95="$(percentile_file "$LAT_FILE" 95)"
  pmax="$(max_file "$LAT_FILE")"
  local wall_ms
  wall_ms="$(( ( $(now_ns) - mount_wall_start ) / 1000000 ))"
  meta metric.files_checked "$FILECHECK"
  meta metric.resolved_contexts "$resolved"
  meta metric.mount_latency_p50_ms "$p50"
  meta metric.mount_latency_p95_ms "$p95"
  meta metric.mount_latency_max_ms "$pmax"
  meta metric.mount_wall_ms "$wall_ms"
  meta metric.unique_listing_digests "$unique_digests"
  meta metric.verified_files_sha256 "$FILECHECK"
  meta metric.listing_digests_sha256 "$(sha256_file "$STEP_DIR/digests.txt")"
  step_end "$([ "$verified" -eq "$mounted" ] && [ "$FILECHECK" -gt 0 ] && echo pass || echo fail)" \
    "files=$FILECHECK verified=$verified unique_digests=$unique_digests resolved=$resolved"

  # ---- read-only denials on one Rust and one Go mount -------------------
  step_begin "read-only" "Context mounts reject mutation"
  local checked=0
  for i in $(seq 0 $((spawned - 1))); do
    [ "${M_STATE[$i]}" = 1 ] || continue
    mountpoint -q "${M_MOUNT[$i]}" 2>/dev/null || continue
    [ "$checked" -ge 2 ] && break
    run_any "write-$i" bash -c "printf x > '${M_MOUNT[$i]}/forbidden.txt'"
    if [ "$RUN_STATUS" -ne 0 ]; then record_assert "write denied mount $i" denied denied true; else record_assert "write denied mount $i" denied allowed false; fi
    checked=$((checked + 1))
  done
  record_assert "read-only denials exercised" "2" "$checked" "$([ "$checked" -eq 2 ] && echo true || echo false)"
  step_end pass "write denied on the first two mounts"

  # ---- clean unmount; assert no daemon survives -------------------------
  step_begin "unmount" "Unmount every mount and prove no daemon survives"
  local unmounted=0 survivors=0 alive survivor_deadline
  for i in $(seq 0 $((spawned - 1))); do
    if [ "${M_STATE[$i]}" = 1 ]; then
      if stop_mount "${M_MOUNT[$i]}"; then unmounted=$((unmounted + 1)); fi
    else
      stop_mount "${M_MOUNT[$i]}" || true
    fi
  done
  # One bulk wait instead of a per-daemon sleep loop.
  survivor_deadline=$(( $(now_ns) + 60000000000 ))
  while :; do
    alive=0
    if [ "$mount_mode" = shared ]; then
      [ -n "$SHARED_PID" ] && kill -0 "$SHARED_PID" 2>/dev/null && alive=1
    else
      for i in $(seq 0 $((spawned - 1))); do
        kill -0 "${M_PID[$i]}" 2>/dev/null && alive=$((alive + 1))
      done
    fi
    [ "$alive" -eq 0 ] && break
    [ "$(now_ns)" -ge "$survivor_deadline" ] && break
    sleep 0.1
  done
  survivors="$alive"
  if [ "$survivors" -gt 0 ]; then
    if [ "$mount_mode" = shared ]; then
      [ -n "$SHARED_PID" ] && kill -0 "$SHARED_PID" 2>/dev/null && kill "$SHARED_PID" 2>/dev/null || true
    else
      for i in $(seq 0 $((spawned - 1))); do
        kill -0 "${M_PID[$i]}" 2>/dev/null && kill "${M_PID[$i]}" 2>/dev/null || true
      done
    fi
  fi
  assert_eq "clean unmounts" "$mounted" "$unmounted"
  assert_eq "daemons surviving unmount" 0 "$survivors"
  meta metric.daemons_surviving "$survivors"
  meta metric.clean_unmounts "$unmounted"
  step_end "$([ "$survivors" -eq 0 ] && echo pass || echo fail)" "unmounted=$unmounted survivors=$survivors"
}

MOUNT_TARGET="${WEFT_CONTEXT_MOUNTS:-3}"
MOUNT_MODE="${WEFT_CONTEXT_MOUNTS_MODE:-per-daemon}"
case "$MOUNT_MODE" in
  per-daemon|shared) ;;
  *) echo "WEFT_CONTEXT_MOUNTS_MODE=$MOUNT_MODE unsupported: use per-daemon or shared" >&2; exit 64 ;;
esac
if [ "$MOUNT_MODE" = shared ] && [ "$MOUNT_TARGET" -eq 3 ]; then
  echo "WEFT_CONTEXT_MOUNTS_MODE=shared requires an explicit WEFT_CONTEXT_MOUNTS (the 4..1000 scaled court); the 3-mount showcase path stays per-daemon" >&2
  exit 64
fi
case "$MOUNT_TARGET" in
  ''|*[!0-9]*) echo "WEFT_CONTEXT_MOUNTS must be a positive integer" >&2; exit 64 ;;
esac
if [ "$MOUNT_TARGET" -ne 3 ] && { [ "$MOUNT_TARGET" -lt 1 ] || [ "$MOUNT_TARGET" -gt 1000 ]; }; then
  echo "WEFT_CONTEXT_MOUNTS=$MOUNT_TARGET unsupported: this demo builds 1..1000 distinct Context Roots" >&2
  exit 64
fi
if [ "$MOUNT_TARGET" -gt 100 ] && [ $((MOUNT_TARGET % 4)) -ne 0 ]; then
  echo "WEFT_CONTEXT_MOUNTS=$MOUNT_TARGET unsupported: values above 100 must be a multiple of 4 (N/4 model and derived roots)" >&2
  exit 64
fi

demo_init "weftfs.context-mounts.receipt/1" "context-mounts" \
  "Context Views as first-class mounts (A1, D2)"

if [ "$MOUNT_TARGET" -ne 3 ]; then
  run_scaled_mounts "$MOUNT_TARGET"
  exit 0
fi

S="$STORES/context-mounts-$RUN_ID"
rm -rf "$S"
mkdir -p "$S"
r() { "$RUSTFS" --store "$S" --fixed-timestamp-ns "$1" "${@:2}"; }
rj() { "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$1" "${@:2}"; }

ALPHA=$'shared agent output v1\n'
PROMPT=$'you are agent-42\n'
MODEL=$'llama-4 ver_19 delta\n'
ALPHA_SHA="$(printf '%s' "$ALPHA" | sha256sum | awk '{print $1}')"
PROMPT_SHA="$(printf '%s' "$PROMPT" | sha256sum | awk '{print $1}')"
MODEL_SHA="$(printf '%s' "$MODEL" | sha256sum | awk '{print $1}')"

step_begin "store" "Deterministic store, typed relations, two snapshots, three Context Roots"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" init
run_ok mkdir-agents "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /agents --seed "dir-agents"
run_ok mkdir-prompts "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /prompts --seed "dir-prompts"
run_ok mkdir-models "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" mkdir /models --seed "dir-models"
run_ok create-alpha "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /agents/alpha.bin \
  --text "$ALPHA" --seed "alpha-out-v1" --attr "task=agent-42" --attr "model=llama-4@ver_19" --attr "sha256=$ALPHA_SHA"
run_ok create-prompt "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /prompts/prompt.txt \
  --text "$PROMPT" --seed "prompt-v1" --attr "sha256=$PROMPT_SHA"
run_ok create-model "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /models/llama-4.bin \
  --text "$MODEL" --seed "model-v1" --attr "sha256=$MODEL_SHA"
run_ok snapshot-s1 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" snapshot create s1

# Second time: a byte-identical object with a distinct ObjectId and a distinct
# context membership (task only, not the llama-4 model).
run_ok create-beta "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" create /agents/beta.bin \
  --text "$ALPHA" --seed "beta-out-v1" --attr "task=agent-42" --attr "model=other-model" --attr "sha256=$ALPHA_SHA"
run_ok snapshot-s2 "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" snapshot create s2

run_ok stat-alpha "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /agents/alpha.bin
ALPHA_ID="$(jsonq "$RUN_OUT" object_id)"
ALPHA_CID="$(jsonq "$RUN_OUT" content_id)"
run_ok stat-beta "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /agents/beta.bin
BETA_ID="$(jsonq "$RUN_OUT" object_id)"
BETA_CID="$(jsonq "$RUN_OUT" content_id)"
run_ok stat-prompt "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /prompts/prompt.txt
PROMPT_ID="$(jsonq "$RUN_OUT" object_id)"
run_ok stat-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" stat /models/llama-4.bin
MODEL_ID="$(jsonq "$RUN_OUT" object_id)"

assert_ne "two byte-identical objects have distinct ObjectIds" "$ALPHA_ID" "$BETA_ID"
assert_eq "two byte-identical objects share one ContentId" "$ALPHA_CID" "$BETA_CID"

# Typed edges: task, model, generated_by, derived_from, prompted_by.
run_ok rel-task "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate task --value "agent-42"
run_ok rel-model "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate model --value "llama-4@ver_19"
run_ok rel-generated "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$ALPHA_ID" --predicate generated_by --object "$MODEL_ID"
run_ok rel-derived "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$BETA_ID" --predicate derived_from --object "$ALPHA_ID"
run_ok rel-prompted "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS2" relation add --subject "$BETA_ID" --predicate prompted_by --object "$PROMPT_ID"
run_ok rel-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" relation list
assert_eq "five typed relations recorded" "5" "$(jsonq "$RUN_OUT" relations#len)"

run_ok ctx-task "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context create \
  --name "by:task=agent-42" --selector 'select attrs.task == "agent-42"' --snapshot s2
CTX_TASK_ID="$(jsonq "$RUN_OUT" context_root_id)"
CTX_TASK_URI="$(jsonq "$RUN_OUT" uri)"
run_ok ctx-model "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context create \
  --name "by:model=llama-4@ver_19" --selector 'select attrs.model == "llama-4@ver_19"' --snapshot s1
CTX_MODEL_ID="$(jsonq "$RUN_OUT" context_root_id)"
CTX_MODEL_URI="$(jsonq "$RUN_OUT" uri)"
run_ok ctx-list "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS2" context list
assert_eq "two stored Context Roots plus none hidden" "2" "$(jsonq "$RUN_OUT" contexts#len)"
artifact_local "store-FORMAT" "$S/FORMAT"
step_end pass "alpha=$ALPHA_ID beta=$BETA_ID content=$ALPHA_CID; contexts=$CTX_TASK_ID,$CTX_MODEL_ID"

step_begin "mounts" "Three simultaneous mounts: Rust context, Go context, Go time root"
M1="$MOUNT_ROOT/f3-$RUN_ID-task"
M2="$MOUNT_ROOT/f3-$RUN_ID-model"
M3="$MOUNT_ROOT/f3-$RUN_ID-time"
start_mount rust "$S" "$M1" --context "by:task=agent-42"
start_mount go "$S" "$M2" --context "by:model=llama-4@ver_19"
start_mount go "$S" "$M3" --at "2026-01-01T00:00:00Z"

record_cmd "ls $M1"
ls -1 "$M1" > "$STEP_DIR/ls-task.txt"
record_cmd "ls $M2"
ls -1 "$M2" > "$STEP_DIR/ls-model.txt"
record_cmd "ls $M3"
ls -1 "$M3" > "$STEP_DIR/ls-time.txt"
save_out ls-task "$STEP_DIR/ls-task.txt"
save_out ls-model "$STEP_DIR/ls-model.txt"
save_out ls-time "$STEP_DIR/ls-time.txt"

TASK_NAMES="$(grep -v '^\.weft$' "$STEP_DIR/ls-task.txt" | wc -l | tr -d ' ')"
MODEL_NAMES="$(grep -v '^\.weft$' "$STEP_DIR/ls-model.txt" | wc -l | tr -d ' ')"
assert_eq "by:task lists all authorized results" "2" "$TASK_NAMES"
assert_eq "by:model lists exactly its authorized result" "1" "$MODEL_NAMES"
assert_ne "the two context listings differ" \
  "$(grep -v '^\.weft$' "$STEP_DIR/ls-task.txt" | sort | sha256sum | awk '{print $1}')" \
  "$(grep -v '^\.weft$' "$STEP_DIR/ls-model.txt" | sort | sha256sum | awk '{print $1}')"
assert_ne "the time root listing differs from by:task" \
  "$(listing_digest "$M1")" "$(listing_digest "$M3")"
step_end pass "task=2 model=1 time=$(grep -vc '^\.weft$' "$STEP_DIR/ls-time.txt")"

step_begin "verify-bytes" "Every listed file matches its declared content root and SHA-256"
for pair in "task:$M1:by:task=agent-42" "model:$M2:by:model=llama-4@ver_19"; do
  IFS=: read -r label mount ctx <<< "$pair"
  run_ok "resolve-$label" "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context resolve "$ctx"
  cp "$RUN_OUT" "$STEP_DIR/resolve-$label.json"
  count="$(jsonq "$STEP_DIR/resolve-$label.json" returned)"
  i=0
  while [ "$i" -lt "$count" ]; do
    oid="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.object_id")"
    cid="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.content_id")"
    path="$(jsonq "$STEP_DIR/resolve-$label.json" "results.$i.path")"
    base="$(basename "$path")"
    name="$base--${oid:0:8}"
    run_ok "stat-$label-$i" "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat "$path"
    sha="$(jsonq "$RUN_OUT" attrs.sha256)"
    assert_sha256 "$label/$name" "$mount/$name" "$sha"
    assert_eq "$label result $i content id matches stat" "$cid" "$(jsonq "$RUN_OUT" content_id)"
    i=$((i + 1))
  done
done

# Time root: the exact same paths and bytes at TS1; beta (TS2) is absent.
run_ok stat-time-alpha "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat /agents/alpha.bin
assert_sha256 "time/agents/alpha.bin" "$M3/agents/alpha.bin" "$(jsonq "$RUN_OUT" attrs.sha256)"
assert_not_exists "time root has no TS2 object" "$M3/agents/beta.bin"
assert_sha256 "time/prompts/prompt.txt" "$M3/prompts/prompt.txt" "$PROMPT_SHA"

# The same logical object (alpha) appears in both contexts through one
# content-addressed manifest, not a second chunk.
TASK_HAS_ALPHA=0
MODEL_HAS_ALPHA=0
j=0
while [ "$j" -lt "$(jsonq "$STEP_DIR/resolve-task.json" returned)" ]; do
  [ "$(jsonq "$STEP_DIR/resolve-task.json" "results.$j.object_id")" = "$ALPHA_ID" ] && TASK_HAS_ALPHA=1
  j=$((j + 1))
done
j=0
while [ "$j" -lt "$(jsonq "$STEP_DIR/resolve-model.json" returned)" ]; do
  [ "$(jsonq "$STEP_DIR/resolve-model.json" "results.$j.object_id")" = "$ALPHA_ID" ] && MODEL_HAS_ALPHA=1
  j=$((j + 1))
done
assert_eq "alpha is selected by by:task" "1" "$TASK_HAS_ALPHA"
assert_eq "alpha is selected by by:model" "1" "$MODEL_HAS_ALPHA"
MANIFEST="$S/manifests/${ALPHA_CID:0:2}/$ALPHA_CID"
if [ -f "$MANIFEST" ]; then
  record_assert "one manifest file backs both contexts" present present true
  artifact_local "shared-manifest" "$MANIFEST"
else
  record_assert "one manifest file backs both contexts" present missing false
fi
assert_eq "both mounts read one content hash" "$ALPHA_CID" "$(jsonq "$STEP_DIR/resolve-model.json" results.0.content_id)"
step_end pass "all listed SHA-256 values match stored attrs; one manifest $ALPHA_CID"

step_begin "read-only" "Every mount is read-only and rejects mutation"
for spec in "task:$M1" "model:$M2" "time:$M3"; do
  label="${spec%%:*}"
  mount="${spec#*:}"
  run_any "write-$label" bash -c "printf x > '$mount/forbidden.txt'"
  if [ "$RUN_STATUS" -ne 0 ]; then record_assert "write denied $label" denied denied true; else record_assert "write denied $label" denied allowed false; fi
  run_any "mkdir-$label" mkdir "$mount/forbidden-dir"
  if [ "$RUN_STATUS" -ne 0 ]; then record_assert "mkdir denied $label" denied denied true; else record_assert "mkdir denied $label" denied allowed false; fi
done
assert_not_exists "no file was created by the denied writes" "$M1/forbidden.txt"
step_end pass "EROFS on every mount"

step_begin "concurrency" "A writer can commit while three read-only mounts are live"
run_ok live-write "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS3" create /agents/live.txt --text "live\n" --seed "live-v1"
assert_eq "live writer committed" "0" "$RUN_STATUS"
# The time-pinned mount must not observe the later commit.
assert_not_exists "pinned mount does not follow the live writer" "$M3/agents/live.txt"
step_end pass "writer lock was not held by the mounts"

step_begin "listing-digests" "Record per-mount listing digests"
D1="$(listing_digest "$M1")"
D2="$(listing_digest "$M2")"
D3="$(listing_digest "$M3")"
record_assert "by:task listing digest" "$D1" "$D1" true
record_assert "by:model listing digest" "$D2" "$D2" true
record_assert "time listing digest" "$D3" "$D3" true
assert_ne "context listing digests differ" "$D1" "$D2"
assert_ne "time listing digest differs" "$D3" "$D1"
step_end pass "digests task=$D1 model=$D2 time=$D3"
