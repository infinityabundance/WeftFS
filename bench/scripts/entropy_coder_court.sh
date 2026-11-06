#!/usr/bin/env bash
# Experimental entropy-coder court (subphase R5).
#
# Runs inside the bench court, which is hard-limited to 3 GiB, 4 CPUs, 1024
# pids (runner: scripts/docker-court.sh bench "bash bench/scripts/entropy_coder_court.sh").
#
# Steps:
#   1. re-hash every cached real model file against
#      bench/corpora/real-models/float-tensor-real.json in the court;
#   2. fail closed if the experimental module is referenced from any
#      non-experimental source file;
#   3. build and run WeftCodec/examples/entropy_coder_court.rs, which extracts
#      the shipped mode 9/11/12/15 planes and measures every coder and
#      workload shape;
#   4. assemble bench/receipts/entropy-coder-<UTC ISO>.json.
#
# Optional inputs:
#   WEFT_BENCH_TIMESTAMP  receipt timestamp (default: now, UTC)
#   WEFT_FORMAT_CHECKS    JSON file with the cross gate / conformance replay
#                         results at the same revision (included verbatim)
set -Eeuo pipefail

root=/work
corpus="$root/.docker-cache/real-corpus"
work="$root/.docker-cache/entropy-court"
manifest="$root/bench/corpora/real-models/float-tensor-real.json"
harness="$root/.docker-cache/target-rust/release/examples/entropy_coder_court"
timestamp="${WEFT_BENCH_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="$root/bench/receipts/entropy-coder-$timestamp.json"
mkdir -p "$work"

echo "== corpus SHA-256 (in court) =="
python3 - "$manifest" "$corpus" "$work/corpus.json" <<'PY'
import datetime, hashlib, json, os, sys

manifest_path, corpus_dir, out_path = sys.argv[1:4]
manifest_bytes = open(manifest_path, "rb").read()
manifest = json.loads(manifest_bytes)
manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
verified = []
for case in manifest["cases"]:
    name = os.path.basename(case["path"])
    path = os.path.join(corpus_dir, name)
    if not os.path.isfile(path):
        raise SystemExit("missing corpus file: %s" % path)
    size = os.path.getsize(path)
    if size != case["size"]:
        raise SystemExit("%s: size %d != manifest %d" % (path, size, case["size"]))
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    actual = digest.hexdigest()
    if actual != case["sha256"]:
        raise SystemExit("%s: sha256 %s != manifest %s" % (path, actual, case["sha256"]))
    verified.append(
        {
            "id": case["id"],
            "file": name,
            "size": size,
            "sha256": actual,
            "dtype": case["dtype"],
            "tensor_count": case["tensor_count"],
            "verified": True,
        }
    )
    print("  %s: sha256 %s ok" % (case["id"], actual))
doc = {
    "manifest": "bench/corpora/real-models/float-tensor-real.json",
    "manifest_sha256": manifest_sha,
    "verified_utc": datetime.datetime.now(datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    ),
    "cases": verified,
    "all_verified": True,
    "note": (
        "Every file in the manifest was re-hashed in the court before the run. "
        "The court uses Qwen2.5-0.5B, Qwen2.5-0.5B-Instruct, and pythia-70m "
        "step0/step1; the remaining files are verified but not measured."
    ),
}
with open(out_path, "w", encoding="utf-8") as handle:
    json.dump(doc, handle, indent=1)
    handle.write("\n")
PY

echo "== experimental module reachability =="
refs="$(grep -rn "experimental::" "$root/WeftCodec/src" --include='*.rs' \
  | grep -v "/src/experimental/" || true)"
if [[ -n "$refs" ]]; then
  echo "FAIL: the experimental module tree is referenced from a production source file:" >&2
  echo "$refs" >&2
  exit 1
fi
echo "  no reference from a persistent encode/decode path"

echo "== build =="
cargo build --release --quiet --manifest-path "$root/WeftCodec/Cargo.toml" \
  --example entropy_coder_court

harness_sha="$(sha256sum "$harness" | awk '{print $1}')"
manifest_sha="$(sha256sum "$manifest" | awk '{print $1}')"
echo "  harness $harness_sha"

echo "== court =="
"$harness" \
  --base-qwen "$corpus/Qwen2.5-0.5B.safetensors" \
  --instruct-qwen "$corpus/Qwen2.5-0.5B-Instruct.safetensors" \
  --pythia-step0 "$corpus/pythia-70m-step0.safetensors" \
  --pythia-step1 "$corpus/pythia-70m-step1.safetensors" \
  --out "$work/rows.ndjson" \
  2>&1 | tee "$work/court.log"

expected_rows=252
actual_rows="$(grep -c '"type":"row"' "$work/rows.ndjson")"
if [[ "$actual_rows" -ne "$expected_rows" ]]; then
  echo "FAIL: expected $expected_rows rows, got $actual_rows" >&2
  exit 1
fi
if grep -q '"roundtrip_ok":false' "$work/rows.ndjson"; then
  echo "FAIL: a round trip failed" >&2
  exit 1
fi

echo "== assemble receipt =="
format_args=()
if [[ -n "${WEFT_FORMAT_CHECKS:-}" && -f "${WEFT_FORMAT_CHECKS}" ]]; then
  format_args=(--format-checks "$WEFT_FORMAT_CHECKS")
fi
python3 "$root/bench/scripts/assemble_entropy_court.py" \
  --rows "$work/rows.ndjson" \
  --corpus "$work/corpus.json" \
  --court-log "$work/court.log" \
  --manifest "$manifest" \
  --harness-sha256 "$harness_sha" \
  --manifest-sha256 "$manifest_sha" \
  --experimental-refs pass \
  --out "$receipt" \
  --timestamp "$timestamp" \
  "${format_args[@]}"

echo "== done =="
echo "receipt: $receipt"
