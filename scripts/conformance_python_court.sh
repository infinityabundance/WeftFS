#!/usr/bin/env bash
# Independent Python second opinion for the conformance bundle (subphase R13).
#
#   scripts/conformance_python_court.sh
#
# Runs `scripts/verify-conformance-python.sh` in the python court against the
# bundle and receipt produced by `scripts/conformance_bundle_court.sh` for the
# same stamp, then checks that the merged receipt records the Python run, that
# it has zero failures, and that every skip is documented. The bundle is not
# rebuilt: its sha256 is re-checked against the receipt before anything runs.
#
# This phase needs blake3, which the fuse court does not have, which is why it
# is a separate court. It changes no conformance semantics.
#
# An environment refusal prints exactly one `conformance_python: skip <reason>`
# line and exits 78. A missing bundle or receipt for this stamp means the
# conformance_bundle court did not run; that is a skip, and a skip is never a
# pass (the aggregate will already have recorded the bundle court's own result).
set -Eeuo pipefail

prefix=conformance_python
skip() { echo "$prefix: skip $1" >&2; exit 78; }
fail() { echo "$prefix: FAIL $1" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
command -v python3 >/dev/null 2>&1 || skip "python3 is required to inspect the receipt"
image="weft/python-court:1"
docker image inspect "$image" >/dev/null 2>&1 \
  || skip "court image $image is not present (build it with scripts/docker-court.sh python true)"

stamp="${WEFT_STAMP:-${WEFT_COMPAT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}}"
version="$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || fail "cannot read version from Cargo.toml"
bundle="bench/bundles/conformance-$version.tar.zst"
receipt="bench/receipts/conformance-bundle-$stamp.json"

[[ -f "$bundle" ]] || skip "no $bundle for stamp $stamp (the conformance_bundle court did not run)"
[[ -f "$receipt" ]] || skip "no $receipt for stamp $stamp (the conformance_bundle court did not run)"

command_text="scripts/docker-court.sh python \"cd /work && bash scripts/verify-conformance-python.sh $bundle $receipt\""
echo "$prefix: configuration: independent Python reader from the extracted bundle, all 420 vectors, documented-skip check; stamp=$stamp" >&2

chmod a+rwX bench/receipts 2>/dev/null || true

scripts/docker-court.sh python \
  "cd /work && bash scripts/verify-conformance-python.sh $bundle $receipt" \
  || fail "python second-opinion run failed"

python3 - "$receipt" "$bundle" "$stamp" "$command_text" <<'PY'
import hashlib, json, os, sys

receipt_path, bundle, stamp, command_text = sys.argv[1:5]
with open(receipt_path, encoding="utf-8") as handle:
    record = json.load(handle)

def check(condition, name):
    if not condition:
        print("conformance_python: FAIL receipt check %s" % name, file=sys.stderr)
        sys.exit(1)

actual_sha = hashlib.sha256(open(bundle, "rb").read()).hexdigest()
check(actual_sha == record.get("bundle_sha256"), "bundle_sha256_unchanged")
python = record.get("runs", {}).get("python")
check(python is not None, "python_run_present")
check(python.get("fail") == 0 and python.get("result") == "pass", "python_run_passes")
opinion = record.get("python_second_opinion", {})
check(opinion.get("status") == "executed", "python_status_executed")
check(opinion.get("extracted_only") is True, "python_extracted_only")
check(opinion.get("documented_skips") is True, "python_skips_documented")
check(record.get("checks", {}).get("python_second_opinion_passes") is True,
      "python_second_opinion_passes")
check(record.get("checks", {}).get("python_skips_documented") is True,
      "python_skips_documented_check")

courts = record.setdefault("courts", {})
courts["conformance_python"] = {
    "name": "conformance_python",
    "script": "scripts/conformance_python_court.sh",
    "stamp": stamp,
    "court": "python",
    "command": command_text,
    "bundle": bundle,
    "bundle_sha256": actual_sha,
    "pass": python.get("pass"),
    "fail": python.get("fail"),
    "skip": python.get("skip"),
    "documented_skips": True,
}
configuration = record.setdefault("configuration", {})
configuration["python_second_opinion"] = "executed"
configuration["adapters"] = ["rust", "python"]
configuration["merged_by"] = "scripts/conformance_python_court.sh"
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(record, handle, indent=2, sort_keys=True)
    handle.write("\n")

print("conformance_python: %s pass / %s fail / %s skip (extracted copy only)" % (
    python.get("pass"), python.get("fail"), python.get("skip")))
print("conformance_python: skips per mode %s" % json.dumps(
    record.get("python_skips_per_mode", {}), sort_keys=True))
print("conformance_python: documented skips true, receipt=%s" % receipt_path)
PY
