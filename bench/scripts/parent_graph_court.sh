#!/usr/bin/env bash
# S13 court: choose a parent graph for a real pythia-70m trajectory.
#
#   scripts/docker-court.sh bench \
#     "cd /work && bash bench/scripts/parent_graph_court.sh"
#
# Corpus: the nine cached pythia-70m checkpoints (step 0/1/2/4/8/16/64/512/
# 1000), each 281,715,176 B and SHA-256 verified in earlier receipts. The
# harness re-verifies every file inside the court.
#
# The harness writes every version independently into a probe store, prices
# every earlier-version parent with the shipped tensor planner, runs the
# chain/restart DP, the exhaustive oracle (N <= 12) and the greedy solver, then
# realises the current chain, restart intervals 2/3/4, and the optimizer's plan
# in separate stores. Every version is restored three times and compared with
# the source container.
#
# The `--lambda-ns` default is the smallest integer at which a restart interval
# beats the chain on the measured matrix; the receipt records the break-even
# values and the chosen lambda. Override with WEFT_PARENT_GRAPH_LAMBDA_NS.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/parent-graph-$stamp"
receipt="${WEFT_PARENT_GRAPH_RECEIPT:-bench/receipts/parent-graph-$stamp.json}"
log="bench/receipts/parent-graph-$stamp.log"
corpus="$root/.docker-cache/real-corpus"
steps="${WEFT_PARENT_GRAPH_STEPS:-0,1,2,4,8,16,64,512,1000}"
max_depth="${WEFT_PARENT_GRAPH_MAX_DEPTH:-8}"
sla_ms="${WEFT_PARENT_GRAPH_SLA_MS:-4000}"
reps="${WEFT_PARENT_GRAPH_REPS:-7}"
if [[ "$reps" -lt 5 ]]; then
  echo "parent_graph: WEFT_PARENT_GRAPH_REPS must be at least 5 (got $reps)" >&2
  exit 1
fi
sla_sweep="${WEFT_PARENT_GRAPH_SLA_SWEEP_MS:-500,850,900,950,1000,2000,2500,3000,4000,6000,8000,10000,11000,14000}"
mkdir -p bench/receipts "$work"
exec > >(tee "$log") 2>&1

echo "parent_graph: stamp=$stamp"
echo "parent_graph: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "parent_graph: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "parent_graph: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "parent_graph: steps=$steps max_depth=$max_depth sla_ms=$sla_ms restore_reps=$reps"

echo "parent_graph: building release harness"
cargo build --release --example parent_graph_court >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/examples/parent_graph_court"
cli="$CARGO_TARGET_DIR/release/weftfs"
echo "parent_graph: building the hop calibration harness"
cargo build --release --example calibrate_restore >/dev/null 2>&1
hop_bin="$CARGO_TARGET_DIR/release/examples/calibrate_restore"
# Always rebuild the CLI: a stale binary from an earlier revision would silently
# exercise the wrong command surface.
echo "parent_graph: building the CLI"
cargo build --release --bin weftfs >/dev/null 2>&1

# Fail loudly before the long run if a corpus file is missing.
first=1
for step in ${steps//,/ }; do
  file="$corpus/pythia-70m-step$step.safetensors"
  if [[ ! -f "$file" ]]; then
    echo "parent_graph: missing $file" >&2
    exit 1
  fi
  [[ $first -eq 0 ]] && printf ' '
  first=0
  printf 'step%s: %s B sha256=%s\n' "$step" "$(stat -c %s "$file")" "$(sha256sum "$file" | awk '{print $1}')"
done

lambda_args=()
if [[ -n "${WEFT_PARENT_GRAPH_LAMBDA_NS:-}" ]]; then
  lambda_args=(--lambda-ns "$WEFT_PARENT_GRAPH_LAMBDA_NS")
fi

echo "parent_graph: running the measured experiment"
# S13c: re-measure the controlled dependency chain in this same court and hand
# the result to the graph court. The graph court derives the SLA margin from the
# committed S13b receipts and compares both with the baked constants.
hop_receipt="$work/hop.json"
echo "parent_graph: measuring the controlled hop chains"
"$hop_bin" \
  --out "$hop_receipt" \
  --work "$work/hop" \
  --chains-only \
  --chain-sizes-mib "${WEFT_PARENT_GRAPH_HOP_SIZES_MIB:-4,8,16}" \
  --chain-depth 8 \
  --reps "$reps" \
  --sweep-dtype f32

"$bin" \
  --corpus "$corpus" \
  --work "$work/work" \
  --out "$receipt" \
  --steps "$steps" \
  --max-depth "$max_depth" \
  --sla-ms "$sla_ms" \
  --sla-sweep-ms "$sla_sweep" \
  --restore-reps "$reps" \
  --hop-receipt "$hop_receipt" \
  "${lambda_args[@]}"

# The CLI is a separate surface from the library the harness calls. Exercise it
# end to end on the first two steps: dry run, then apply into a fresh store.
cli_steps="${WEFT_PARENT_GRAPH_CLI_STEPS:-0,1}"
cli_versions=""
for step in ${cli_steps//,/ }; do
  cli_versions+="cli-step$step=$corpus/pythia-70m-step$step.safetensors,"
done
cli_versions="${cli_versions%,}"
echo "parent_graph: CLI dry run on $cli_steps"
"$cli" --store "$work/cli-plan" plan-parents \
  --versions "$cli_versions" \
  --max-depth "$max_depth" \
  --sla-ms "$sla_ms" \
  --solver dp \
  --json >"$work/cli-plan.json"
echo "parent_graph: CLI apply on $cli_steps"
"$cli" --store "$work/cli-apply" plan-parents \
  --versions "$cli_versions" \
  --max-depth "$max_depth" \
  --sla-ms "$sla_ms" \
  --solver dp \
  --apply \
  --json >"$work/cli-apply.json"
echo "parent_graph: CLI plan parent edges: $(grep -o '\"parents\": \[[^]]*\]' "$work/cli-apply.json" | head -1)"

echo "parent_graph: receipt=$receipt"
echo "parent_graph: work=$work"
du -sh "$work" || true
python3 - "$receipt" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print("parent_graph: schema", d["schema"])
for label in ("chain", "restart-2", "restart-3", "restart-4", "dp", "brute_force"):
    plan = d["plans"].get(label)
    if not plan:
        continue
    print(
        "parent_graph: plan %-10s bytes=%d worst_restore_ns=%d max_depth=%d parents=%s"
        % (label, plan["predicted_bytes"], plan["worst_restore_ns"], plan["max_depth"], plan["parents"])
    )
print("parent_graph: dp==brute:", d["checks"].get("dp_matches_brute_force"))
print("parent_graph: checks", json.dumps(d["checks"], sort_keys=True))
margin = d.get("sla_margin") or {}
print("parent_graph: sla_margin baked=%s derived=%s match=%s"
      % (margin.get("baked_ns_per_mib"), margin.get("derived_ns_per_mib"), margin.get("matches_baked")))
print("parent_graph: sla_boundary", json.dumps(d.get("sla_boundary"), sort_keys=True))
for row in (d.get("hop_calibration") or {}).get("rows", []):
    print("parent_graph: hop %-13s baked=%d remeasured=%d error_ppm=%d samples=%d"
          % (row["class"], row["baked_ns_per_mib"], row["remeasured_ns_per_mib"], row["remeasured_error_ppm"], row["samples"]))
for row in d["realizations"]:
    print(
        "parent_graph: realized %-20s data=%d predicted=%d worst_restore_ms=%.1f exact=%s verify_ok=%s"
        % (
            row["label"],
            row["total_data_bytes"],
            row["predicted_bytes"],
            row["restore_worst_ms"],
            row["all_versions_exact"],
            row["verify_full_ok"],
        )
    )
PY

# The realised stores are large (about 6 GiB for nine versions) and are fully
# derivable from the receipt. Remove them on success; a failed run stops above
# and keeps them for diagnosis.
if [[ "${WEFT_PARENT_GRAPH_KEEP_WORK:-0}" != "1" ]]; then
  echo "parent_graph: removing the work tree (WEFT_PARENT_GRAPH_KEEP_WORK=1 keeps it)"
  rm -rf "$work"
fi
