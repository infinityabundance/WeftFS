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

echo "== encryption CLI smoke (flags, wrong key, no key, ciphertext on disk) ==" | tee -a "$log"
cargo build --release --bin weftfs >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
cli="$target/release/weftfs"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
key="$(printf 'ab%.0s' {1..32})"
wrong="$(printf 'cd%.0s' {1..32})"
printf 'hello encrypted world' > "$work/plain.bin"
"$cli" --store "$work/store" init --encryption-key-hex "$key" --encryption-mode convergent >>"$log" 2>&1
"$cli" --store "$work/store" create /a.bin --file "$work/plain.bin" --encryption-key-hex "$key" >>"$log" 2>&1
read_back="$("$cli" --store "$work/store" cat /a.bin --encryption-key-hex "$key")"
[[ "$read_back" == "hello encrypted world" ]] || { echo "encryption: CLI round trip mismatch" >&2; exit 1; }
"$cli" --store "$work/store" verify --full --encryption-key-hex "$key" >>"$log" 2>&1 || { echo "encryption: verify --full failed" >&2; exit 1; }
"$cli" --store "$work/store" info --json --encryption-key-hex "$key" >>"$log" 2>&1 || { echo "encryption: info failed" >&2; exit 1; }
if "$cli" --store "$work/store" cat /a.bin --encryption-key-hex "$wrong" >>"$log" 2>&1; then
  echo "encryption: a wrong key read succeeded" >&2; exit 1
fi
if "$cli" --store "$work/store" cat /a.bin >>"$log" 2>&1; then
  echo "encryption: a keyless read succeeded" >&2; exit 1
fi
first_object="$(find "$work/store/objects" -type f | head -1)"
[[ "$(head -c 6 "$first_object")" == "WEFTE1" ]] || { echo "encryption: an object is not enveloped" >&2; exit 1; }
echo "CLI smoke: round trip exact, verify --full ok, wrong key refused, keyless read refused, object carries WEFTE1" >>"$log"
echo "== encryption interleaved cost bench ==" | tee -a "$log"
cargo build --release --example encryption_bench >>"$log" 2>&1
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
