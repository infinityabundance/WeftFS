#!/usr/bin/env bash
# T079 court: optional encryption at rest, both dedup policies, measured cost.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/encryption_court.sh"
#
# Runs the store-level suite (`tests/encryption.rs`) and the interleaved
# plain/convergent/random cost bench in the bench court, then assembles
# `bench/receipts/encryption-<stamp>.json` (+ `.log`).
#
# Exit 78 with exactly one `encryption: skip <reason>` line means the
# environment cannot run the court; anything after the preflight is a failure.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
log="bench/receipts/encryption-${stamp}.log"
receipt="bench/receipts/encryption-${stamp}.json"
: >"$log"

if ! command -v cargo >/dev/null 2>&1; then
  echo "encryption: skip no cargo in this court"
  exit 78
fi
if [[ -z "${WEFT_IN_COURT:-}" ]]; then
  echo "encryption: skip not inside scripts/docker-court.sh (hard limits are mandatory)"
  exit 78
fi

echo "== encryption store suite ==" | tee -a "$log"
cargo test --test encryption >>"$log" 2>&1

echo "== encryption interleaved cost bench ==" | tee -a "$log"
cargo build --release --example encryption_bench >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/encryption_bench" --out "$receipt" --reps "${WEFT_ENCRYPTION_REPS:-7}" \
  >>"$log" 2>&1

if [[ ! -f "$receipt" ]]; then
  echo "encryption: missing receipt $receipt" >&2
  exit 1
fi

python3 - "$receipt" <<'PY' | tee -a "$log"
import json, sys
data = json.load(open(sys.argv[1]))
summary = data["summary"]
for arm in ("plain", "convergent", "random"):
    row = summary[arm]
    print(f"{arm}: write {row['write_ms_median']:.1f} ms, read {row['read_ms_median']:.1f} ms, "
          f"write cpu {row['write_cpu_ms_median']:.1f} ms, read cpu {row['read_cpu_ms_median']:.1f} ms, "
          f"physical {row['physical_bytes']} B, objects {row['object_files']}, exact {row['exact']}")
for name, stats in data["plane"]["modes"].items():
    print(f"plane {name}: seal {stats['seal_mib_per_s']:.0f} MiB/s, open {stats['open_mib_per_s']:.0f} MiB/s")
for check in data["checks"]:
    if not check["pass"]:
        print(f"FAILED {check['name']}: {check['detail']}")
print(json.dumps({"result": data["result"], "passed": data["passed"], "failed": data["failed"]}))
raise SystemExit(0 if data.get("result") == "pass" else 1)
PY

echo "encryption: pass ($receipt)"
sha256sum "$receipt" "$log"
