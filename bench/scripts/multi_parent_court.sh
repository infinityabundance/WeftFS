#!/usr/bin/env bash
# T091 court: is an exact tensor cheaper to reconstruct from several causal
# sources than from one?
#
# Host:  bash bench/scripts/multi_parent_court.sh
# Court: dispatched in two stages (bench court for the measurement, fuse court
#        for the receipt assembly, because the bench image has no Python 3).
#
# The harness (`examples/multi_parent_court.rs`) encodes a multi-parent container
# that uses the shipped mode-3 matcher, decodes it exactly, and measures the
# best single-parent mode 3 / mode 11 / mode 16 baselines on the same bytes.
# A fan-out-1 container is the calibrated control: same op stream, only the
# header differs.
#
# Exit 78 with one `<name>: skip <reason>` line when the environment cannot run
# the court.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

stage=""
stamp=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --stage) stage="$2"; shift 2 ;;
    --stamp) stamp="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 64 ;;
  esac
done

skip() {
  echo "multi_parent_court: skip $1" >&2
  exit 78
}

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  [[ -n "$stage" ]] && { echo "the host wrapper takes no --stage" >&2; exit 64; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  docker image inspect weft/bench-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/bench-court:1"
  docker image inspect weft/fuse-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/fuse-court:1"
  [[ -r "$root/.docker-cache/real-corpus/pythia-70m-step0.safetensors" ]] || \
    skip "pythia-70m checkpoints are not in .docker-cache/real-corpus"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$root/bench/receipts"
  rc=0
  "$root/scripts/docker-court.sh" bench \
    "bash bench/scripts/multi_parent_court.sh --stage measure --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "multi-parent court: measure stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  "$root/scripts/docker-court.sh" fuse \
    "bash bench/scripts/multi_parent_court.sh --stage assemble --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "multi-parent court: assemble stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  echo "receipt: bench/receipts/multi-parent-${stamp}.json"
  exit 0
fi

cd "$root"
stamp="${stamp:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt_dir="bench/receipts"
log="$receipt_dir/multi-parent-${stamp}.log"
raw="$receipt_dir/multi-parent-${stamp}-raw.json"
receipt="$receipt_dir/multi-parent-${stamp}.json"
mkdir -p "$receipt_dir"

if [[ "$stage" == "measure" ]]; then
  echo "== T091 multi-parent court, measure stage ==" | tee -a "$log"
  echo "kernel: $(uname -a)" | tee -a "$log"
  echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})" | tee -a "$log"
  echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}" | tee -a "$log"
  cargo build --quiet --example multi_parent_court 2>>"$log"
  bin=".docker-cache/target-rust/debug/examples/multi_parent_court"
  sha256sum "$bin" examples/multi_parent_court.rs | tee -a "$log"
  # Reduced configuration in the aggregate; the standalone default is the same
  # command with --max-tensors 16 --fanouts 1,2,3,4 --reps 3.
  /usr/bin/time -v "$bin" \
    --corpus .docker-cache/real-corpus \
    --targets "${WEFT_MP_TARGETS:-1,8,16,64,512,1000}" \
    --max-tensors "${WEFT_MP_TENSORS:-12}" \
    --max-tensor-bytes "${WEFT_MP_MAX_TENSOR_BYTES:-67108864}" \
    --max-parents "${WEFT_MP_PARENTS:-4}" \
    --fanouts "${WEFT_MP_FANOUTS:-1,2,3,4}" \
    --reps "${WEFT_MP_REPS:-3}" \
    --out "$raw" 2>&1 | tee -a "$log"
  exit 0
fi

if [[ "$stage" == "assemble" ]]; then
  [[ -r "$raw" ]] || { echo "missing $raw" >&2; exit 1; }
  python3 - "$raw" "$receipt" "$stamp" "${WEFT_REPO_WEFTFS:-unknown}" <<'PY' | tee -a "$log"
import json, os, sys
raw_path, receipt, stamp, revision = sys.argv[1:5]
raw = json.load(open(raw_path))
tensors = raw["tensors"]
totals = raw["totals"]
checks = []
def check(name, ok, detail=None):
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name + ((": " + str(detail)) if detail is not None else ""))

# 1. Exactness: every recorded decode is byte-exact.
rows = [fanout for tensor in tensors for fanout in tensor["multiparent"]]
check("every_multiparent_decode_exact",
      bool(rows) and all(row["exact"] for row in rows),
      {"rows": len(rows), "inexact": sum(1 for row in rows if not row["exact"])})

# 2. Availability: missing and corrupt parents are typed refusals with no bytes.
refused = all(
    tensor["availability"]["missing_parent"]["refused"]
    and tensor["availability"]["corrupt_parent"]["refused"]
    and tensor["availability"]["missing_parent"]["bytes_returned"] == 0
    and tensor["availability"]["corrupt_parent"]["bytes_returned"] == 0
    and tensor["availability"]["full_decode_ok"]
    for tensor in tensors
)
check("availability_typed_refusals", refused and bool(tensors), len(tensors))

# 3. Baselines exist for every tensor.
check("baselines_present",
      all(t["best_single_delta_bytes"] > 0 and t["best_single_weight_delta_bytes"] > 0
          and t["best_single_sparse_delta_bytes"] > 0 and t["float_tensor_bytes"] > 0
          for t in tensors), len(tensors))

# 4. Calibration: the fan-out-1 control differs from the shipped mode-3 frame
#    only by the container/frame header once the per-COPY parent-index byte is
#    subtracted. Declared bound: |body delta| <= 160 bytes.
body = [fanout["container_body_delta_vs_same_parent_mode3"]
        for tensor in tensors for fanout in tensor["multiparent"]
        if fanout.get("container_body_delta_vs_same_parent_mode3") is not None]
check("fanout1_body_calibration",
      bool(body) and all(abs(value) <= 160 for value in body),
      {"samples": len(body),
       "min": min(body) if body else None,
       "max": max(body) if body else None})
raw_header = [fanout["header_delta_vs_same_parent_mode3"]
              for tensor in tensors for fanout in tensor["multiparent"]
              if fanout["header_delta_vs_same_parent_mode3"] is not None]
check("fanout1_measured_overhead_reported",
      bool(raw_header),
      {"samples": len(raw_header),
       "min": min(raw_header) if raw_header else None,
       "max": max(raw_header) if raw_header else None,
       "note": "includes one parent-index byte per COPY, a real container cost"})

# 5. Calibration: at fan-out 1 the container body is the shipped mode-3 op
#    stream plus exactly one parent-index byte per COPY and one terminator byte.
identity = [
    (fanout["body_bytes"] - fanout["copies"] - 1, fanout["same_parent_mode3_body_bytes"])
    for tensor in tensors for fanout in tensor["multiparent"]
    if fanout.get("same_parent_mode3_body_bytes") is not None
]
check("fanout1_body_identity",
      bool(identity) and all(a == b for a, b in identity),
      {"samples": len(identity), "mismatches": sum(1 for a, b in identity if a != b)})

# 6. The result, in payload-normalised terms (op stream only, so the container
#    header cannot flatter multi-parent): wins exist but are bounded, and the
#    on-disk aggregate is not smaller.
payload = []
for tensor in tensors:
    best = min(fanout["body_bytes"] - fanout["copies"] - 1 for fanout in tensor["multiparent"])
    payload.append({
        "name": tensor["name"], "target": tensor["target"],
        "logical_bytes": tensor["logical_bytes"],
        "multiparent_body_bytes": best,
        "single_body_bytes": tensor["best_single_delta_body_bytes"],
        "win_bytes": tensor["best_single_delta_body_bytes"] - best,
    })
wins = [row for row in payload if row["win_bytes"] > 0]
total_logical = sum(row["logical_bytes"] for row in payload)
total_win = sum(row["win_bytes"] for row in wins)
total_multiparent = sum(row["multiparent_body_bytes"] for row in payload)
total_single = sum(row["single_body_bytes"] for row in payload)
check("payload_wins_are_small",
      all(row["win_bytes"] <= total_logical / 1000 for row in wins),
      {"tensors_with_a_payload_win": len(wins),
       "largest_win_bytes": max((row["win_bytes"] for row in wins), default=0),
       "largest_win_fraction_of_tensor": max(
           ((row["win_bytes"] / row["logical_bytes"]) for row in wins), default=0.0)})
check("payload_aggregate_win_bounded",
      total_win <= total_logical / 1000,
      {"payload_multiparent": total_multiparent, "payload_single": total_single,
       "payload_win_bytes": total_win, "payload_win_fraction": total_win / total_logical,
       "payload_ratio": total_multiparent / total_single})
check("on_disk_aggregate_not_smaller",
      totals["best_multiparent_bytes"] >= totals["best_single_delta_bytes"],
      {"multiparent": totals["best_multiparent_bytes"],
       "single_mode3": totals["best_single_delta_bytes"],
       "ratio": totals["multiparent_over_single_delta"]})
check("weight_delta_aggregate_wins",
      totals["best_multiparent_bytes"] >= totals["best_single_weight_delta_bytes"],
      {"multiparent": totals["best_multiparent_bytes"],
       "single_weight_delta": totals["best_single_weight_delta_bytes"],
       "ratio": totals["multiparent_over_single_weight_delta"]})

doc = {
    "schema": "weftfs.multi-parent.receipt/1",
    "role": "T091 exact multi-parent reconstruction vs single-parent baselines",
    "generated_at": stamp,
    "revision": revision,
    "measurement": raw,
    "analysis": {
        "fanout": [
            {"fanout": row["fanout"], "bytes": row["bytes"],
             "over_best_single_delta": row["over_best_single_delta"]}
            for row in totals["fanout_totals"]
        ],
        "fanout1_bytes": totals["fanout_totals"][0]["bytes"],
        "fanout1_ratio": totals["fanout_totals"][0]["over_best_single_delta"],
        "fanout2_bytes": totals["fanout_totals"][1]["bytes"],
        "fanout2_ratio": totals["fanout_totals"][1]["over_best_single_delta"],
        "fanout3_bytes": totals["fanout_totals"][2]["bytes"],
        "fanout3_ratio": totals["fanout_totals"][2]["over_best_single_delta"],
        "fanout4_bytes": totals["fanout_totals"][3]["bytes"],
        "fanout4_ratio": totals["fanout_totals"][3]["over_best_single_delta"],
        "fanout1_chosen": sum(
            1 for tensor in tensors
            if min(tensor["multiparent"], key=lambda r: r["bytes"])["fanout"] == 1
        ),
        "largest_payload_win": max((row["win_bytes"] for row in wins), default=0),
        "largest_payload_win_fraction": max(
            ((row["win_bytes"] / row["logical_bytes"]) for row in wins), default=0.0),
        "multi_decode_ms_sum": sum(
            min(tensor["multiparent"], key=lambda r: r["bytes"])["decode_ms"]
            for tensor in tensors
        ),
    },
    "payload_analysis": {
        "definition": "op stream bytes only: container body minus one parent-index byte per COPY and minus the op-stream terminator; identical to the shipped mode-3 body at fan-out 1",
        "per_tensor": payload,
        "total_multiparent": total_multiparent,
        "total_single": total_single,
        "total_win": total_win,
        "tensors_with_a_payload_win": len(wins),
    },
    "checks": checks,
    "checks_passed": sum(1 for row in checks if row["ok"]),
    "checks_total": len(checks),
    "result": "pass" if all(row["ok"] for row in checks) else "fail",
    "scope": {
        "exactness": "every reproduced tensor compared byte-for-byte with the source tensor",
        "availability": "a missing parent and a parent whose bytes do not hash to the declared root are both refused with no bytes returned",
        "not_claimed": [
            "a shipped wire mode or planner slot (this is a research container)",
            "cold media, power loss, fleet or cloud behaviour",
            "any comparison against mature filesystems",
            "determinism of wall-clock numbers across hosts",
        ],
    },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": doc["result"],
                  "checks": f"{doc['checks_passed']}/{doc['checks_total']}"}))
PY
  python3 - "$receipt" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
raise SystemExit(0 if doc["result"] == "pass" else 1)
PY
  exit 0
fi

echo "unknown stage '$stage'" >&2
exit 64
