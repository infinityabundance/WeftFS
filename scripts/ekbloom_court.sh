#!/usr/bin/env bash
# EKBLOOM court — EKB-E1 (boundary) and EKB-E2 (space).
#
# Contract: `bench/CONTRACTS-ekbloom.md` (version 1), committed before the first
# measurement. Gates EKB-E3 (read), EKB-E4 (restore) and EKB-E5 (write) are out
# of scope for this subphase and refuse explicitly; they are not silently
# reported as passes.
#
# This script may only run inside the rust court. Everything it builds and
# measures runs there under the court's unchanged hard limits
# (3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded /tmp), which this script
# neither raises nor disables.
#
# Usage (host):
#   scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase boundary"
#   scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase space"
#
# Writes:
#   bench/receipts/ekbloom-<stamp>.json        (boundary)
#   bench/receipts/ekbloom-space-<stamp>.json  (space)
#   bench/receipts/<same base>.log
#
# `WEFT_EKBLOOM_STAMP` overrides the stamp. A missing or mismatched corpus
# container is a skip (exit 78), never a substitution. A crate fetch or build
# failure is a failure with the exact cargo text, never a vendored substitute.
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: scripts/ekbloom_court.sh --phase boundary|space|read
       (also accepted but not implemented in this subphase: restore, write)

Run it inside the rust court:
  scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase boundary"
  scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase space"
  scripts/docker-court.sh rust "cd /work && bash scripts/ekbloom_court.sh --phase read"
USAGE
  exit 64
}

phase=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --phase) [[ $# -ge 2 ]] || { echo "--phase needs a value" >&2; exit 64; }; phase="$2"; shift 2 ;;
    --phase=*) phase="${1#--phase=}"; shift ;;
    -h|--help) usage ;;
    *) echo "unknown argument '$1'" >&2; usage ;;
  esac
done
[[ -n "$phase" ]] || usage

case "$phase" in
  boundary|space|read) ;;
  restore|write)
    echo "ekbloom_court: not implemented in this subphase (--phase $phase)" >&2
    exit 3
    ;;
  *)
    echo "unknown phase '$phase' (expected boundary|space|read)" >&2
    exit 64
    ;;
esac

# --- court guard -----------------------------------------------------------
# The measurement is only meaningful under the court's hard limits, and the
# harness links the parent crate path. Refuse rather than run outside them.
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  echo "ekbloom_court: refusing to run outside the court (WEFT_IN_COURT != 1)." >&2
  echo "  run: scripts/docker-court.sh rust \"cd /work && bash scripts/ekbloom_court.sh --phase ${phase}\"" >&2
  exit 64
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stamp="${WEFT_EKBLOOM_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
case "$phase" in
  boundary) receipt="bench/receipts/ekbloom-${stamp}.json" ;;
  space) receipt="bench/receipts/ekbloom-space-${stamp}.json" ;;
  read) receipt="bench/receipts/ekbloom-read-${stamp}.json" ;;
esac
log="${receipt%.json}.log"
work=".docker-cache/ekbloom-court-${stamp}-${phase}"
harness_dir="bench/ekbloom-harness"
lock="$harness_dir/Cargo.lock"
binary="${CARGO_TARGET_DIR}/release/ekbloom-harness"

mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true
: >"$log"
exec > >(tee -a "$log") 2>&1

echo "== ekbloom court =="
echo "phase: $phase"
echo "stamp: $stamp"
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?} nproc=${WEFT_COURT_NPROC:-?}"
echo "revisions: weftfs=${WEFT_REPO_WEFTFS:-?} weftfs_go=${WEFT_REPO_WEFTFS_GO:-?} weftcodec=${WEFT_REPO_WEFTCODEC:-?} weftcodec_go=${WEFT_REPO_WEFTCODEC_GO:-?}"
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
echo "rustc: $rustc_version"
echo "cargo: $cargo_version"

# --- pinned crate ----------------------------------------------------------
if [[ ! -f "$lock" ]]; then
  echo "ekbloom_court: FAIL fetch_blocked: $lock is absent; the pinned dependency graph is not reproducible" >&2
  exit 1
fi
checksum="$(awk '
  /^\[\[package\]\]/ { name = "" }
  /^name = / { name = $0 }
  /^checksum = / { if (name ~ /"ekbloom"/) { sub(/.*= "/, ""); sub(/".*/, ""); print; exit } }
' "$lock")"
if [[ -z "$checksum" ]]; then
  echo "ekbloom_court: FAIL fetch_blocked: no ekbloom checksum in $lock" >&2
  exit 1
fi
echo "ekbloom: version=0.7.3 lock_checksum=$checksum"
# The court runs as uid 0 without CAP_DAC_OVERRIDE. The lock is a generated
# artifact (like the root Cargo.lock that docker-court widens); source files are
# never touched.
chmod a+rw "$lock" 2>/dev/null || true

# --- build (a failure here is typed, never worked around) ------------------
echo "== build (cargo build --release --locked) =="
build_status=0
(cd "$harness_dir" && cargo build --release --locked) || build_status=$?
if [[ "$build_status" -ne 0 ]]; then
  echo "ekbloom_court: FAIL build_blocked/fetch_blocked (cargo exit $build_status); exact cargo output is above" >&2
  exit 1
fi
if [[ ! -x "$binary" ]]; then
  echo "ekbloom_court: FAIL build_blocked: expected harness binary at $binary is missing" >&2
  exit 1
fi
echo "harness sha256: $(sha256sum "$binary" | cut -d' ' -f1)"

# --- run -------------------------------------------------------------------
rm -rf "$work"
mkdir -p "$work"
echo "== run =="
set +e
"$binary" \
  --phase "$phase" \
  --out "$receipt" \
  --work "$work" \
  --stamp "$stamp" \
  --lock-checksum "$checksum" \
  --rustc-version "$rustc_version" \
  --cargo-version "$cargo_version"
status=$?
set -e

if [[ "$status" -eq 78 ]]; then
  reason=""
  if [[ -r "$work/skip_reason" ]]; then
    reason="$(cat "$work/skip_reason")"
  fi
  echo "ekbloom_court: skip ${reason:-fixture_unavailable}" >&2
  exit 78
fi

if [[ "$status" -ne 0 ]]; then
  if [[ -s "$receipt" ]]; then
    echo "ekbloom_court: FAIL harness exit $status; receipt retained at $receipt" >&2
  else
    echo "ekbloom_court: FAIL harness exit $status and no receipt was written" >&2
  fi
  exit 1
fi

if [[ ! -s "$receipt" ]]; then
  echo "ekbloom_court: FAIL no receipt at $receipt" >&2
  exit 1
fi

# The measurement crate and the store it writes run as uid 0 inside the court.
# Remove the scratch tree here (as root) unless the operator asks to keep it:
# a host-side `rm` cannot delete root-owned files, and the space phase writes
# hundreds of megabytes of regenerable store blobs. Every number the run
# produced is already in the receipt.
if [[ "${WEFT_EKBLOOM_KEEP_WORK:-0}" != "1" ]]; then
  rm -rf "$work"
else
  echo "ekbloom_court: kept work dir $work (WEFT_EKBLOOM_KEEP_WORK=1)"
fi

echo "ekbloom_court: receipt=$receipt"
echo "ekbloom_court: log=$log"
