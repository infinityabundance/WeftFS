#!/usr/bin/env bash
# EKBLOOM Context Court.
#
# Arms: W (shipped resolver, no reuse), M-LRU and M-W-TinyLFU (strong
# conventional bounded caches of certified decisions), E (M plus an
# EKBLOOM-manifested certified decision table), and C-naive (the deliberately
# insufficient-key sensitivity control).
#
# The frozen methodology is `bench/CONTRACTS-ekbloom-context.md`, committed
# before this court produced any measurement. The harness is
# `bench/ekbloom-context-harness/`, an independent crate excluded from the
# workspace; the shipped graph never depends on `ekbloom`.
#
# Usage:
#   scripts/ekbloom_context_court.sh                 # decisions, 9 repetitions
#   scripts/ekbloom_context_court.sh --reduced       # 2 repetitions (aggregate court)
#   scripts/ekbloom_context_court.sh --phase scale   # 1k/100k/1M key-sufficiency court
#   scripts/ekbloom_context_court.sh --phase lifecycle
#   scripts/ekbloom_context_court.sh --phase closure|impact   # refused
#
# Environment skips print exactly one `ekbloom_context_court: skip <reason>`
# line and exit 78. The aggregate verification court records that as `skip`,
# never as a pass.
set -Eeuo pipefail

skip() {
  echo "ekbloom_context_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

phase="decisions"
reps="${WEFT_EKBLOOM_CONTEXT_REPS:-9}"
for arg in "$@"; do
  case "$arg" in
    --reduced) reps=2 ;;
    --reps=*) reps="${arg#--reps=}" ;;
    --reps) echo "--reps needs a value (use --reps=N)" >&2; exit 64 ;;
    --phase=*) phase="${arg#--phase=}" ;;
    --phase) echo "--phase needs a value (use --phase=decisions|scale|lifecycle|closure|impact)" >&2; exit 64 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done
case "$reps" in ''|*[!0-9]*) echo "--reps must be a positive integer" >&2; exit 64 ;; esac
[[ "$reps" -ge 1 ]] || { echo "--reps must be a positive integer" >&2; exit 64; }

case "$phase" in
  decisions|scale|lifecycle) ;;
  closure|impact)
    echo "ekbloom_context_court: not implemented in this subphase (phase=$phase)" >&2
    exit 1
    ;;
  *) echo "unknown phase '$phase'" >&2; exit 64 ;;
esac

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

stamp="${WEFT_EKBLOOM_CONTEXT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_EKBLOOM_CONTEXT_STAMP="$stamp"
label="${WEFT_EKBLOOM_CONTEXT_LABEL:-full}"
if [[ "$reps" -lt 9 ]]; then
  label="reduced"
fi

work="$repo_root/.docker-cache/ekbloom-context-$stamp"
mkdir -p "$work" bench/receipts
chmod -R a+rwX "$work" bench/receipts 2>/dev/null || true
# The court runs as uid 0 with cap-drop=ALL and cannot chmod a host-owned
# source directory. The harness crate is a measurement crate: make its build
# artifacts writable on the host side, exactly as `docker-court.sh` does for
# `Cargo.lock` and the caches.
chmod a+rwX bench/ekbloom-context-harness 2>/dev/null || true

receipt="bench/receipts/ekbloom-context-$stamp.json"
log="bench/receipts/ekbloom-context-$stamp.log"
rows="bench/receipts/ekbloom-context-$stamp.rows.jsonl.zst"

if [[ "$phase" == "decisions" ]]; then
  runner="$work/decisions.sh"
  cat > "$runner" <<'RUNNER'
set -Eeuo pipefail
export PATH="/usr/local/cargo/bin:$PATH"
H=/work/.docker-cache/target-ekbctx/release/ekbloom-context-harness
# The pinned crate must resolve from the court's cache; a cold registry without
# network is an environment skip, not a silent substitution.
if ! (cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo metadata --offline --format-version 1 >/dev/null 2>/tmp/ekb-meta.err); then
  echo "ekbloom_context_court: skip cannot resolve the pinned ekbloom 0.7.3 offline: $(tail -1 /tmp/ekb-meta.err)" >&2
  exit 78
fi
# Unit tests for the cache policies and the manifest candidate-only rule.
(cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo test --release --offline --quiet) || exit 1
(cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo build --release --offline --quiet) || exit 1
# The shipped graph must contain no ekbloom.
if cargo tree -e normal --offline 2>/dev/null | grep -q ekbloom; then
  echo "ekbloom_context_court: shipped dependency graph contains ekbloom" >&2
  exit 1
fi
echo "ekbloom_context_court: shipped graph ekbloom mentions=0" >&2

W=/work/.docker-cache/ekbloom-context-$WEFT_EKBLOOM_CONTEXT_STAMP
F=$W/fixture.json
$H prepare --base "$W/base" --out "$F" || exit 1

arms=(W M-lru M-tinylfu E)
for ((rep=1; rep<=REPS; rep++)); do
  rot=$(( (rep - 1) % 4 ))
  order=("${arms[@]:rot}" "${arms[@]:0:rot}")
  joined="$(IFS=,; echo "${order[*]}")"
  mkdir -p "$W/rep-$(printf '%02d' "$rep")"
  slot=0
  for name in "${order[@]}"; do
    slot=$((slot + 1))
    case "$name" in
      W) arm=W; variant=none ;;
      M-lru) arm=M; variant=lru ;;
      M-tinylfu) arm=M; variant=tinylfu ;;
      E) arm=E; variant=lru ;;
    esac
    store="$W/stores/$name"
    rm -rf "$store"
    mkdir -p "$(dirname "$store")"
    cp -a "$W/base" "$store"
    seed=$((0x67B10C0DE5EED001 + rep))
    echo "ekbloom_context_court: rep $rep/$REPS arm $name order=$joined slot=$slot" >&2
    $H run --store "$store" --fixture "$F" --arm "$arm" --variant "$variant" \
      --rep "$rep" --seed "$seed" --work "$W/rep-$(printf '%02d' "$rep")" \
      --out "$W/rep-$(printf '%02d' "$rep")/$name.json" \
      --rows "$W/rep-$(printf '%02d' "$rep")/$name.tsv" \
      --order "$joined" --order-index "$slot" || exit 1
    rm -rf "$store"
  done
  # Sensitivity control, after the contenders.
  slot=5
  store="$W/stores/NAIVE"
  rm -rf "$store"
  cp -a "$W/base" "$store"
  seed=$((0x67B10C0DE5EED001 + rep))
  echo "ekbloom_context_court: rep $rep/$REPS arm C-naive order=$joined slot=$slot" >&2
  $H run --store "$store" --fixture "$F" --arm NAIVE --variant lru \
    --rep "$rep" --seed "$seed" --work "$W/rep-$(printf '%02d' "$rep")" \
    --out "$W/rep-$(printf '%02d' "$rep")/NAIVE.json" \
    --rows "$W/rep-$(printf '%02d' "$rep")/NAIVE.tsv" \
    --order "$joined" --order-index "$slot" || exit 1
  rm -rf "$store"
done

$H assemble --work "$W" --fixture "$F" --out "/work/$RECEIPT" --log "/work/$LOG" \
  --rows-out "/work/$ROWS" --reps "$REPS" --label "$LABEL" || exit 1

# Lifecycle: delete the manifestation, drop the cache, replay, and require
# identical verdicts.
store="$W/stores/lifecycle"
rm -rf "$store"
cp -a "$W/base" "$store"
$H lifecycle --store "$store" --fixture "$F" --work "$W/lifecycle" \
  --out "/work/$LIFECYCLE" || exit 1
rm -rf "$store"
echo "ekbloom_context_court: receipt /work/$RECEIPT" >&2
echo "ekbloom_context_court: log /work/$LOG" >&2
echo "ekbloom_context_court: rows /work/$ROWS" >&2
RUNNER
  chmod +x "$runner"

  if ! scripts/docker-court.sh bench \
    "cd /work && WEFT_EKBLOOM_CONTEXT_STAMP=$stamp REPS=$reps LABEL=$label RECEIPT=$receipt LOG=$log ROWS=$rows LIFECYCLE=bench/receipts/ekbloom-context-lifecycle-$stamp.json bash .docker-cache/$(basename "$work")/decisions.sh" \
    >>"$work/run.log" 2>&1; then
    echo "ekbloom_context_court: decisions failed; tail of $work/run.log follows" >&2
    tail -40 "$work/run.log" >&2 || true
    exit 1
  fi
fi

if [[ "$phase" == "scale" ]]; then
  scale_receipt="bench/receipts/ekbloom-context-scale-$stamp.json"
  runner="$work/scale.sh"
  cat > "$runner" <<'RUNNER'
set -Eeuo pipefail
export PATH="/usr/local/cargo/bin:$PATH"
H=/work/.docker-cache/target-ekbctx/release/ekbloom-context-harness
(cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo build --release --offline --quiet) || exit 1
W=/work/.docker-cache/ekbloom-context-$WEFT_EKBLOOM_CONTEXT_STAMP
$H scale --out "/work/$SCALE" --work "$W/scale" --sizes 1000,100000,1000000 || exit 1
echo "ekbloom_context_court: scale receipt /work/$SCALE" >&2
RUNNER
  chmod +x "$runner"
  if ! scripts/docker-court.sh bench \
    "cd /work && WEFT_EKBLOOM_CONTEXT_STAMP=$stamp SCALE=$scale_receipt bash .docker-cache/$(basename "$work")/scale.sh" \
    >>"$work/run.log" 2>&1; then
    echo "ekbloom_context_court: scale failed; tail of $work/run.log follows" >&2
    tail -40 "$work/run.log" >&2 || true
    exit 1
  fi
fi

if [[ "$phase" == "lifecycle" ]]; then
  lifecycle_receipt="bench/receipts/ekbloom-context-lifecycle-$stamp.json"
  runner="$work/lifecycle.sh"
  cat > "$runner" <<'RUNNER'
set -Eeuo pipefail
export PATH="/usr/local/cargo/bin:$PATH"
H=/work/.docker-cache/target-ekbctx/release/ekbloom-context-harness
(cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo build --release --offline --quiet) || exit 1
W=/work/.docker-cache/ekbloom-context-$WEFT_EKBLOOM_CONTEXT_STAMP
F=$W/fixture.json
if [[ ! -f "$F" ]]; then
  $H prepare --base "$W/base" --out "$F" || exit 1
fi
store="$W/stores/lifecycle-standalone"
rm -rf "$store"
cp -a "$W/base" "$store"
$H lifecycle --store "$store" --fixture "$F" --work "$W/lifecycle" --out "/work/$LIFECYCLE" || exit 1
rm -rf "$store"
RUNNER
  chmod +x "$runner"
  if ! scripts/docker-court.sh bench \
    "cd /work && WEFT_EKBLOOM_CONTEXT_STAMP=$stamp LIFECYCLE=$lifecycle_receipt bash .docker-cache/$(basename "$work")/lifecycle.sh" \
    >>"$work/run.log" 2>&1; then
    echo "ekbloom_context_court: lifecycle failed; tail of $work/run.log follows" >&2
    tail -40 "$work/run.log" >&2 || true
    exit 1
  fi
fi

if [[ "$phase" == "decisions" ]]; then
  if [[ ! -f "$receipt" ]]; then
    echo "ekbloom_context_court: expected receipt $receipt was not written" >&2
    exit 1
  fi
  result="$(jq -r '.result' "$receipt")"
  claim="$(jq -r '.claim_holds' "$receipt")"
  direction="$(jq -r '.direction' "$receipt")"
  echo "ekbloom_context_court: receipt $receipt (result=$result claim_holds=$claim direction=$direction label=$label reps=$reps)" >&2
  [[ "$result" == "pass" ]] || { echo "ekbloom_context_court: measurement invalid; see $receipt" >&2; exit 1; }
fi

if [[ "$phase" == "scale" ]]; then
  echo "ekbloom_context_court: scale receipt bench/receipts/ekbloom-context-scale-$stamp.json" >&2
fi
if [[ "$phase" == "lifecycle" ]]; then
  echo "ekbloom_context_court: lifecycle receipt bench/receipts/ekbloom-context-lifecycle-$stamp.json" >&2
fi

# The run log is copied beside the receipts, not left only in .docker-cache.
cp "$work/run.log" "bench/receipts/ekbloom-context-$stamp.run.log" 2>/dev/null || true
if [[ "${WEFT_EKBLOOM_CONTEXT_KEEP_WORK:-0}" != "1" ]]; then
  rm -rf "$work" 2>/dev/null || true
fi
