#!/usr/bin/env bash
# Second phase of the conformance-bundle proof: run the independent Python
# reader from the extracted bundle in the python court, then merge its result
# into the bundle receipt.
#
#   scripts/verify-conformance-python.sh BUNDLE RECEIPT
#
# The bundle itself is not modified. The receipt is updated in place; the
# bundle sha256 is re-checked against the receipt before anything runs.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ $# -ne 2 ]; then
  echo "usage: $0 BUNDLE RECEIPT.json" >&2
  exit 64
fi
BUNDLE="$1"
RECEIPT="$2"
if [ ! -f "$BUNDLE" ]; then echo "bundle not found: $BUNDLE" >&2; exit 66; fi
if [ ! -f "$RECEIPT" ]; then echo "receipt not found: $RECEIPT" >&2; exit 66; fi
if ! python3 -c 'import blake3' >/dev/null 2>&1; then
  echo "blake3 is required for the Python reader; run this in the python court" >&2
  exit 78
fi

CACHE="$ROOT/.docker-cache"
mkdir -p "$CACHE"
VERIFY="$(mktemp -d "$CACHE/weft-conformance-python-XXXXXX")"
cleanup() { rm -rf "$VERIFY"; }
trap cleanup EXIT

BUNDLE_SHA="$(sha256sum "$BUNDLE" | awk '{print $1}')"
EXPECTED_SHA="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_sha256"])' "$RECEIPT")"
if [ "$BUNDLE_SHA" != "$EXPECTED_SHA" ]; then
  echo "bundle sha256 $BUNDLE_SHA does not match receipt $EXPECTED_SHA" >&2
  exit 65
fi

tar --zstd -xf "$BUNDLE" -C "$VERIFY" 2>/dev/null || {
  # The python court has no zstd binary; use the Python zstandard module.
  python3 - "$BUNDLE" "$VERIFY" <<'PY'
import sys, tarfile, zstandard
bundle, dest = sys.argv[1], sys.argv[2]
with open(bundle, "rb") as raw:
    with zstandard.ZstdDecompressor().stream_reader(raw) as stream:
        with tarfile.open(fileobj=stream, mode="r|") as archive:
            archive.extractall(dest)
PY
}

(
  cd "$VERIFY"
  WEFT_PYTHONPATH="$VERIFY/python" PYTHONPATH="$VERIFY/python" \
    scripts/run-conformance.sh scripts/conformance-adapter-python.sh --quiet \
      --json receipt-python.json
)

python3 - "$RECEIPT" "$VERIFY" <<'PY'
import json, os, sys
receipt_path, verify = sys.argv[1:3]
manifest = json.load(open(os.path.join(verify, "conformance", "manifest.json")))
by_path = {entry["path"]: entry for entry in manifest["vectors"]}
with open(os.path.join(verify, "receipt-python.json"), encoding="utf-8") as handle:
    python = json.load(handle)

per_mode = {}
per_mode_reasons = {}
reasons = {}
for skip in python["skips"]:
    entry = by_path.get(skip["path"], {})
    mode = entry.get("mode")
    key = "none" if mode is None else str(mode)
    per_mode[key] = per_mode.get(key, 0) + 1
    per_mode_reasons.setdefault(key, set()).add(skip["reason"])
    reasons[skip["reason"]] = reasons.get(skip["reason"], 0) + 1

documented_reasons = {
    "WEFT-TENSOR layout/shard containers are outside the WeftCodec reader",
    "WeftQL selector grammar is not implemented by the Python reader",
    "no Python adapter for vector kind 'transport'",
    "no Python adapter for vector kind 'engram'",
    "no Python adapter for vector kind 'kv-prov'",
    "Context Root resolution is not implemented by the Python reader",
    "no Python adapter for vector kind 'float-tiled-streams'",
    "causal closure identity is not implemented by the Python reader",
    "capability envelope verification is not implemented by the Python reader",
    "weft.checkpoint.v1 manifests are not implemented by the Python reader",
    "adversarial verb 'archive-decode' is a non-decoder surface",
    "adversarial verb 'selector-id' is a non-decoder surface",
    "composite fixture (use --include-fixtures)",
    "adversarial verb 'archive-create' is a non-decoder surface",
    "112_reserved_mode_21",
}
undocumented = sorted(
    reason for reason in reasons
    if reason not in documented_reasons and not reason.startswith("superseded by ")
)
mode_expectations = {"18": 17, "19": 22, "20": 21}
mode_ok = all(per_mode.get(mode, 0) == count for mode, count in mode_expectations.items())
non_codec_extra = sorted(
    mode for mode, why in per_mode_reasons.items()
    if mode not in ("none", "18", "19", "20")
    and any(not reason.startswith("superseded by ") for reason in why)
)
documented = not undocumented and not non_codec_extra and mode_ok

record = json.load(open(receipt_path, encoding="utf-8"))
record["runs"]["python"] = python
record["python_skip_reasons"] = dict(sorted(reasons.items(), key=lambda kv: (-kv[1], kv[0])))
record["python_skips_per_mode"] = dict(sorted(per_mode.items(), key=lambda kv: kv[0]))
record["python_second_opinion"] = {
    "status": "executed",
    "reason": "",
    "script": "scripts/verify-conformance-python.sh",
    "extracted_only": True,
    "documented_skips": documented,
    "undocumented_reasons": undocumented,
    "non_codec_extra_skips": non_codec_extra,
    "mode_expectations": mode_expectations,
}
record.setdefault("checks", {})["python_second_opinion_passes"] = python["fail"] == 0
record["checks"]["python_skips_documented"] = documented
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(record, handle, indent=2, sort_keys=True)
    handle.write("\n")
# The court runs as uid 0 with cap-drop=ALL; a host-side verification step must
# be able to read and update this receipt afterwards.
os.chmod(receipt_path, 0o666)

print("python run: %d pass, %d fail, %d skip" % (python["pass"], python["fail"], python["skip"]))
print("python skips per mode:", json.dumps(record["python_skips_per_mode"], sort_keys=True))
print("documented skips:", documented)
if undocumented:
    print("undocumented reasons:", undocumented)
if non_codec_extra:
    print("unexpected non-codec skips:", non_codec_extra)
if not documented:
    sys.exit(1)
PY
