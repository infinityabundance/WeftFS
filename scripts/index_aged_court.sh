#!/usr/bin/env bash
# Aged-store advisory-index court (T012 / N13).
#
# Runs the shipped `index_aged_bench` example in the bench court, one store per
# cell, and checks the correctness invariants of every receipt it produces:
#
#   * the persisted index answers exactly like the scan oracle,
#   * the writer's maintained sections equal a full `StateIndex::build`,
#   * a reopened store with `indexes/` removed rebuilds from replayed state,
#   * GC with a snapshot pin keeps every segment a retained root names,
#   * a forged head segment is refused and rebuilt.
#
# Two cell families, because they stress different things:
#
#   gens  history grows, the live set is constant
#   live  the live state grows, the history is short
#
# Storage is decided per cell from a declared estimate. A cell that fits the
# court's 1 GiB bounded `/tmp` runs there. A cell that does not runs on a
# declared host-backed scratch directory (`.docker-cache/aged-scratch`), whose
# size is checked before the run and written into the receipt, and which is
# removed immediately after the cell's receipt is verified. Nothing is ever
# relabelled: a cell that could not run at its target is re-run at a smaller
# target with `reduced_from`/`reduced_reason` in its receipt.
#
#   scripts/index_aged_court.sh                # reduced cells (verify-all)
#   WEFT_AGED_CONFIG=full scripts/index_aged_court.sh
#
# Exit: 0 all run cells pass, 1 a cell failed, 78 the environment cannot run
# the court at all (docker missing/unusable, or no disk for any cell).
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

config="${WEFT_AGED_CONFIG:-reduced}"
# Optional explicit cell list (comma-separated names, e.g. `gens-1m`). A named
# cell runs even when `config` is reduced, which is how a single long cell is
# re-run after a fix without repeating the cells that already passed.
explicit_cells="${WEFT_AGED_CELLS:-}"
wants_cell() {
  [[ -z "$explicit_cells" ]] && return 0
  [[ ",$explicit_cells," == *",$1,"* ]]
}
scratch_base="$repo_root/.docker-cache/aged-scratch"
receipts="$repo_root/bench/receipts"
mkdir -p "$scratch_base" "$receipts"

# The bench court must exist. A missing docker CLI/daemon or image is an
# environment skip, not a pass.
if ! command -v docker >/dev/null 2>&1; then
  echo "index_aged_court: skip docker CLI not available"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "index_aged_court: skip docker daemon not available"
  exit 78
fi
if ! docker image inspect weft/bench-court:1 >/dev/null 2>&1; then
  echo "index_aged_court: skip court image weft/bench-court:1 is not built"
  exit 78
fi

stamp() { date -u +%Y%m%dT%H%M%SZ; }
avail_bytes() { df --output=avail -B1 "$1" | tail -1 | tr -d ' '; }

declare -a summary_rows=()
failures=0

cleanup_root() {
  local root="$1"
  # The cell's container already removed the tree (it owns those files). All
  # that is left is the empty scratch parent, which this user created.
  case "$root" in
    /work/.docker-cache/aged-scratch/*)
      rmdir -- "$scratch_base/$(basename "$root")" 2>/dev/null || true
      ;;
  esac
}

# run_cell <name> <mode> <target> <live> <gens> <estimate_bytes> <label>
run_cell() {
  local name="$1" mode="$2" target="$3" live="$4" gens="$5" estimate="$6" label="$7"
  local cell_stamp root storage declared=0
  cell_stamp="$(stamp)"

  if (( estimate <= 700 * 1024 * 1024 )); then
    storage="tmpfs"
    root="/tmp/aged-$name"
  else
    storage="scratch"
    local avail
    avail="$(avail_bytes "$scratch_base")"
    if (( avail < estimate * 2 )); then
      echo "index_aged_court: cell $name needs $((estimate / 1024 / 1024)) MiB declared scratch, only $((avail / 1024 / 1024)) MiB free"
      return 75
    fi
    declared="$estimate"
    root="/work/.docker-cache/aged-scratch/$name"
  fi

  echo "index_aged_court: cell=$name mode=$mode target=$target live=$live gens=$gens storage=$storage estimate=$((estimate / 1024 / 1024))MiB stamp=$cell_stamp"

  local command
  # Cleanup happens inside the cell's own container: the court runs as uid 0
  # with cap-drop=ALL, so the host user cannot unlink its tree afterwards. The
  # final `rm` preserves the run's exit code.
  command="cd /work && rm -rf '$root' && { WEFT_AGED_COMMAND='index_aged_bench $name' \
WEFT_AGED_MODE='$mode' WEFT_AGED_TARGET='$target' WEFT_AGED_LIVE='$live' \
WEFT_AGED_GENS='$gens' WEFT_AGED_ROOT='$root' WEFT_AGED_STAMP='$cell_stamp' \
WEFT_AGED_STORAGE='$storage' WEFT_AGED_DECLARED_BYTES='$declared' \
WEFT_AGED_REDUCED_FROM='${WEFT_REDUCED_FROM:-}' \
WEFT_AGED_REDUCED_REASON='${WEFT_REDUCED_REASON:-}' \
WEFT_AGED_LABEL='$label' cargo run --release --locked --example index_aged_bench; } ; rc=\$? ; rm -rf '$root' ; exit \$rc"

  local log
  log="$(mktemp)"
  local rc=0
  scripts/docker-court.sh bench "$command" >"$log" 2>&1 || rc=$?
  if (( rc != 0 )); then
    cp "$log" "$receipts/index-aged-$cell_stamp.failure.log" 2>/dev/null || true
    echo "index_aged_court: cell $name FAILED to run (rc=$rc); raw log kept at bench/receipts/index-aged-$cell_stamp.failure.log"
    tail -5 "$log" || true
    cleanup_root "$root"
    rm -f "$log"
    return 1
  fi
  cleanup_root "$root"

  local receipt="$receipts/index-aged-$cell_stamp.json"
  if [[ ! -f "$receipt" ]]; then
    echo "index_aged_court: cell $name produced no receipt at $receipt"
    rm -f "$log"
    return 1
  fi
  local verdict
  verdict="$(python3 - "$receipt" <<'PY'
import json, sys
path = sys.argv[1]
d = json.load(open(path))
run = d["run"]
problems = []
if run.get("failure") is not None:
    problems.append("the store refused a measured commit: %r" % (run.get("failure"),))
oracle = run.get("oracle", {}).get("maintained_sections_equal_rebuild")
if oracle is not True:
    problems.append("maintained sections vs rebuild = %r" % (oracle,))
for key in ("cold_persisted", "cold_rebuilt"):
    cold = run.get(key, {})
    if cold.get("oracle_match") is not True:
        problems.append("%s answers disagree with the scan oracle" % key)
    if cold.get("open", {}).get("p50") is None:
        problems.append("%s did not open" % key)
persisted = run.get("cold_persisted", {}).get("index_stats", {})
if not all(v and v > 0 for v in persisted.get("loads", [])):
    problems.append("cold_persisted never loaded a segment: %r" % (persisted,))
rebuilt = run.get("cold_rebuilt", {}).get("index_stats", {})
if not all(v and v > 0 for v in rebuilt.get("builds", [])):
    problems.append("cold_rebuilt never rebuilt: %r" % (rebuilt,))
gc = run.get("gc", {})
if gc.get("retained_missing_after") != 0:
    problems.append("GC removed %r segments a retained root names" % (gc.get("retained_missing_after"),))
if gc.get("retained_after", 0) == 0:
    problems.append("GC retained no segments")
forged = run.get("forged_segment", {})
if "error" not in forged and not forged.get("skipped"):
    if forged.get("index_stats", {}).get("rejects", 0) < 1:
        problems.append("a forged segment was not rejected: %r" % (forged,))
    if forged.get("index_stats", {}).get("builds", 0) < 1:
        problems.append("a forged segment did not trigger a rebuild: %r" % (forged,))
    if forged.get("oracle_match") is not True:
        problems.append("answers after a forged segment disagree with the oracle")
print(json.dumps({"ok": not problems, "problems": problems,
                  "publish": run.get("publish", {}),
                  "peak_rss_kib": run.get("peak_rss_kib"),
                  "index_bytes": run.get("footprint", {}).get("index_bytes"),
                  "data_bytes": run.get("footprint", {}).get("data_bytes")}))
PY
)"
  if [[ "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["ok"])' "$verdict")" != "True" ]]; then
    echo "index_aged_court: cell $name FAILED: $verdict"
    cp "$log" "$receipts/index-aged-$cell_stamp.failure.log" 2>/dev/null || true
    failures=$((failures + 1))
  else
    echo "index_aged_court: cell $name PASS $verdict"
  fi
  summary_rows+=("$name|$mode|$target|$live|$gens|$storage|$declared|$cell_stamp|$verdict")
  rm -f "$log"
  return 0
}

# run_cell_with_reduction <name> <mode> <target> <live> <gens> <estimate> <reduced_target> <label>
run_cell_with_reduction() {
  local name="$1" mode="$2" target="$3" live="$4" gens="$5" estimate="$6"
  local reduced="$7" label="$8" rc=0
  local reduced_mode="$mode" reduced_live="$live" reduced_gens="$gens"
  if [[ "$mode" == "live" ]]; then
    reduced_mode="live"; reduced_live="$reduced"; reduced_gens="$gens"
  else
    reduced_mode="gens"; reduced_live="$live"; reduced_gens="$reduced"
  fi
  run_cell "$name" "$mode" "$target" "$live" "$gens" "$estimate" "$label" || rc=$?
  if (( rc == 75 )); then
    echo "index_aged_court: cell $name cannot run at target on this host; running reduced target $reduced (labelled)"
    WEFT_REDUCED_FROM="$target" WEFT_REDUCED_REASON="declared scratch estimate ${estimate} B exceeds free space" \
      run_cell "$name-reduced" "$reduced_mode" "$reduced" "$reduced_live" "$reduced_gens" \
      "$(python3 -c "print(int($estimate * $reduced // $target))")" \
      "reduced configuration: $label (target $target could not fit)" || true
  fi
}

gens_estimate() { python3 -c "print(int($1 * 6144) + 8192 * 1024)"; }
live_estimate() { python3 -c "print(int($1 * 10240) + 16 * 1024 * 1024)"; }

# ---- cells ---------------------------------------------------------------
# `gens`: 64 live objects, one small attribute update per commit.
if wants_cell gens-10k; then
  run_cell_with_reduction gens-10k gens 10000 64 10000 "$(gens_estimate 10000)" 2000 \
    "history: 10k generations, 64 live objects"
fi
if wants_cell live-10k; then
  run_cell_with_reduction live-10k live 10000 10000 200 "$(live_estimate 10000)" 2000 \
    "state: 10k live objects, 200 measured commits"
fi

if [[ "$config" == "full" ]] || [[ -n "$explicit_cells" ]]; then
  if wants_cell gens-100k; then
    run_cell_with_reduction gens-100k gens 100000 64 100000 "$(gens_estimate 100000)" 20000 \
      "history: 100k generations, 64 live objects"
  fi
  if wants_cell gens-1m; then
    run_cell_with_reduction gens-1m gens 1000000 64 1000000 "$(gens_estimate 1000000)" 200000 \
      "history: 1M generations, 64 live objects"
  fi
  if wants_cell live-100k; then
    run_cell_with_reduction live-100k live 100000 100000 200 "$(live_estimate 100000)" 20000 \
      "state: 100k live objects, 200 measured commits"
  fi
  if wants_cell live-1m; then
    run_cell_with_reduction live-1m live 1000000 1000000 200 "$(live_estimate 1000000)" 250000 \
      "state: 1M live objects, 200 measured commits"
  fi
fi

# ---- summary -------------------------------------------------------------
summary_stamp="$(stamp)"
summary="$receipts/index-aged-court-$summary_stamp.json"
python3 - "$summary" "$summary_stamp" "$config" "${summary_rows[@]}" <<'PY'
import json, sys
path, stamp, config = sys.argv[1], sys.argv[2], sys.argv[3]
rows = []
for raw in sys.argv[4:]:
    name, mode, target, live, gens, storage, declared, cell_stamp, verdict = raw.split("|", 8)
    row = json.loads(verdict)
    rows.append({
        "cell": name, "mode": mode, "target": int(target), "live_objects": int(live),
        "measured_generations": int(gens), "storage": storage,
        "declared_scratch_bytes": int(declared), "stamp": cell_stamp,
        "receipt": "bench/receipts/index-aged-%s.json" % cell_stamp,
        "ok": row["ok"], "problems": row["problems"],
        "publish_p50_ms": row.get("publish", {}).get("p50"),
        "peak_rss_kib": row.get("peak_rss_kib"),
        "index_bytes": row.get("index_bytes"), "data_bytes": row.get("data_bytes"),
    })
doc = {
    "schema": "weftfs.aged-index-court.v1",
    "stamp": stamp,
    "config": config,
    "result": "pass" if rows and all(r["ok"] for r in rows) else "fail",
    "cells": rows,
    "label": "host-only orchestration around the bench court; every cell is one store, "
             "one process, synthetic fixture, single host",
}
json.dump(doc, open(path, "w"), indent=1, sort_keys=True)
print("index_aged_court: summary %s (%d cells, result=%s)" % (path, len(rows), doc["result"]))
PY

if (( failures > 0 )); then
  exit 1
fi
exit 0
