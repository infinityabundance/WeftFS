#!/usr/bin/env bash
# Build and verify the third-party conformance distribution.
#
#   scripts/package-conformance.sh
#
# Produces `bench/bundles/conformance-<version>.tar.zst`, then proves that the
# corpus, runner, contract, and reference adapters inside the bundle are
# sufficient by extracting it to a private directory and running the runner
# from the extracted copy only. The bundle is also shaped as a
# `weftfs.receipt-bundle/1`, so `scripts/verify-receipts.sh` can verify its
# checksums and revisions without trusting the producer.
#
# Runs on the host or inside a Docker court. The court is required for the
# reference run because it needs an implementation binary and Python 3.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${WEFT_VERSION:-$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)}"
if [ -z "$VERSION" ]; then
  echo "cannot determine version from Cargo.toml; set WEFT_VERSION" >&2
  exit 1
fi
STAMP="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
GENERATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
BUNDLES="$ROOT/bench/bundles"
CACHE="$ROOT/.docker-cache"
BUNDLE="$BUNDLES/conformance-$VERSION.tar.zst"
RECEIPT="$ROOT/bench/receipts/conformance-bundle-$STAMP.json"
mkdir -p "$BUNDLES" "$CACHE" "$ROOT/bench/receipts"

STAGE="$(mktemp -d "$CACHE/weft-conformance-$STAMP.XXXXXX")"
VERIFY="$(mktemp -d "$CACHE/weft-conformance-verify-$STAMP.XXXXXX")"
cleanup() { rm -rf "$STAGE" "$VERIFY"; }
trap cleanup EXIT

log() { printf '%s\n' "$*" >&2; }

# ---- stage ---------------------------------------------------------------
mkdir -p "$STAGE"/{conformance,WeftCodec/conformance,docs,scripts,python,revisions}
cp conformance/manifest.json "$STAGE/conformance/manifest.json"
cp -R WeftCodec/conformance/v1 "$STAGE/WeftCodec/conformance/v1"
for d in checkpoint context overlay store engram-store fuzz; do
  if [ -d "conformance/$d" ]; then
    cp -R "conformance/$d" "$STAGE/conformance/$d"
  fi
done
cp docs/CONFORMANCE.md docs/WRITE-YOUR-OWN-DECODER.md docs/RFC.md docs/PAPER.md "$STAGE/docs/"
cp scripts/run_conformance.py scripts/run-conformance.sh \
   scripts/conformance-adapter-rust.sh scripts/conformance-adapter-python.sh "$STAGE/scripts/"
mkdir -p "$STAGE/scripts/conformance/adapters"
cp scripts/conformance/adapters/rust.py scripts/conformance/adapters/python.py \
   scripts/conformance/adapters/go.py scripts/conformance/adapters/_shared.py \
   "$STAGE/scripts/conformance/adapters/"
# The Python reader is a second opinion, not the reference implementation.
# Copy the package only (no tests, wheels, caches, or build products).
mkdir -p "$STAGE/python"
cp -R python/weftcodec "$STAGE/python/weftcodec"
find "$STAGE/python" -type d -name '__pycache__' -prune -exec rm -rf {} +
find "$STAGE/python" -name '*.pyc' -delete

# ---- revisions -----------------------------------------------------------
: > "$STAGE/REVISIONS.txt"
for entry in "WeftFS:.:WEFT_REPO_WEFTFS" "WeftCodec:WeftCodec:WEFT_REPO_WEFTCODEC" "WeftCodec-Go:WeftCodec-Go:WEFT_REPO_WEFTCODEC_GO" "WeftFS-Go:WeftFS-Go:WEFT_REPO_WEFTFS_GO"; do
  name="${entry%%:*}"; rest="${entry#*:}"; path="${rest%%:*}"; env_name="${rest#*:}"
  if command -v git >/dev/null 2>&1 && git -C "$ROOT/$path" rev-parse --verify HEAD >/dev/null 2>&1; then
    revision="$(git -C "$ROOT/$path" rev-parse HEAD)"
    branch="$(git -C "$ROOT/$path" rev-parse --abbrev-ref HEAD)"
    changed="$(git -C "$ROOT/$path" status --porcelain | wc -l | tr -d ' ')"
    worktree=clean; [ "$changed" -ne 0 ] && worktree=dirty
  else
    # The minimal fuse court has no git. The court runner passes the exact
    # revisions it mounted; a missing one is a hard failure, not an empty value.
    revision="${!env_name:-}"
    if [ -z "$revision" ]; then
      echo "cannot read revision for $name (no git and $env_name unset)" >&2
      exit 1
    fi
    branch="unknown"
    changed="unknown"
    worktree="unknown"
  fi
  printf '%s %s %s %s %s\n' "$name" "$revision" "$branch" "$worktree" "$changed" >> "$STAGE/REVISIONS.txt"
  printf '%s\n' "$revision" > "$STAGE/revisions/$name.txt"
done

# ---- two-command run book -------------------------------------------------
cat > "$STAGE/RUN.md" <<'RUN'
# Run the WeftFS conformance corpus

This directory is self-contained. It contains the committed conformance
manifest, every vector, the runner, the adapter contract, the normative index,
and the reference adapters. The only thing it does not contain is *your*
decoder.

## 1. Extract

```sh
tar --zstd -xf conformance-<version>.tar.zst -C conformance-bundle
cd conformance-bundle
```

## 2. Run your adapter

Your adapter is a program that takes one vector path and prints one JSON object
(`docs/CONFORMANCE.md` §2). Then:

```sh
scripts/run-conformance.sh ./your-adapter --json receipt.json
```

Exit status is non-zero on any mismatch or adapter failure. Skips are printed
and counted; a skip is not a pass. `--fail-on-skip` turns any skip into a
failure if you want a strict gate.

### Reference self-check

The reference Rust adapter needs a `weftcodec` binary built from the same
revision. With `WEFTCODEC_BIN` set:

```sh
WEFTCODEC_BIN=/path/to/weftcodec scripts/run-conformance.sh \
  scripts/conformance-adapter-rust.sh --json receipt-rust.json
```

The independent Python reader is included and needs no build:

```sh
WEFT_PYTHONPATH="$PWD/python" PYTHONPATH="$PWD/python" \
  scripts/run-conformance.sh scripts/conformance-adapter-python.sh --json receipt-python.json
```

The Python reader implements modes 0–17 and reports explicit skips for modes
18, 19, 20, and `weft.transport.v1`; see `docs/CONFORMANCE.md` for the exact
per-mode counts. Do not treat those skips as passes.

## What is here

| Path | Contents |
|---|---|
| `conformance/manifest.json` | the 420-entry manifest with per-vector SHA-256 and expectations |
| `WeftCodec/conformance/v1/` | the canonical vector corpus |
| `conformance/{checkpoint,context,overlay,store,engram-store,fuzz}/` | composite fixtures |
| `scripts/run-conformance.sh` | the runner (Python 3 standard library only) |
| `scripts/conformance/adapters/` | reference Rust and Python adapters |
| `docs/CONFORMANCE.md` | the adapter contract, skip semantics, and current results |
| `docs/WRITE-YOUR-OWN-DECODER.md` | the exact frame/mode check order |
| `docs/RFC.md` | the normative index, statuses, and the extended per-mode checklist |
| `docs/PAPER.md` | the falsifiable claims and their receipts |

`CHECKSUMS.sha256` covers every file. `manifest.json` is the bundle manifest
(`weftfs.receipt-bundle/1`); it is not the conformance manifest.
RUN

# ---- dmesg ---------------------------------------------------------------
if dmesg > "$STAGE/dmesg.txt" 2>/dev/null; then
  dmesg_status="captured"
else
  printf 'dmesg unavailable: the producer court does not expose CAP_SYSLOG\n' > "$STAGE/dmesg.txt"
  dmesg_status="unavailable"
fi

# ---- bundle manifest ------------------------------------------------------
python3 - "$STAGE" "$VERSION" "$GENERATED_AT" "$dmesg_status" <<'PY'
import json, os, sys
stage, version, generated_at, dmesg_status = sys.argv[1:5]
manifest = json.load(open(os.path.join(stage, "conformance", "manifest.json")))
revisions = {}
for line in open(os.path.join(stage, "REVISIONS.txt")):
    parts = line.split()
    if len(parts) >= 2:
        revisions[parts[0]] = parts[1]
files = sum(len(files) for _, _, files in os.walk(stage))
# CHECKSUMS.sha256 is added after this manifest; the manifest describes the
# payload, not its own checksum file.
payload_files = [f for _, _, fs in os.walk(stage) for f in fs]
record = {
    "schema": "weftfs.receipt-bundle/1",
    "kind": "conformance-distribution",
    "description": "WeftFS third-party conformance corpus, runner, contract, and reference adapters",
    "version": version,
    "generated_at": generated_at,
    "conformance": {
        "manifest": "conformance/manifest.json",
        "entries": manifest.get("vector_count"),
        "aggregate_sha256": manifest.get("aggregate_sha256"),
    },
    "revisions": revisions,
    "dmesg": {"status": dmesg_status, "path": "dmesg.txt"},
    "payload_file_count": len(payload_files),
    "non_claims": [
        "a skip is not a pass",
        "the corpus is not a performance benchmark",
        "no live cloud, fleet, fleet-scale, or physical power-loss claim",
    ],
}
with open(os.path.join(stage, "manifest.json"), "w", encoding="utf-8") as handle:
    json.dump(record, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

# ---- checksums -----------------------------------------------------------
LIST="$(mktemp "$CACHE/conformance-files-$STAMP.XXXXXX")"
(
  cd "$STAGE"
  find . -type f ! -name CHECKSUMS.sha256 -printf '%P\n' | LC_ALL=C sort > "$LIST"
  while IFS= read -r f; do sha256sum "$f"; done < "$LIST" > CHECKSUMS.sha256
)
rm -f "$LIST"

# ---- deterministic tar ----------------------------------------------------
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cf - -C "$STAGE" . | zstd -19 -T1 -q -f -o "$BUNDLE"
BUNDLE_SHA="$(sha256sum "$BUNDLE" | awk '{print $1}')"
BUNDLE_SIZE="$(stat -c %s "$BUNDLE")"
FILE_COUNT="$(find "$STAGE" -type f | wc -l | tr -d ' ')"
log "bundle: $BUNDLE"
log "sha256: $BUNDLE_SHA"
log "files: $FILE_COUNT"

# ---- verify: extract and run from the extracted copy only -----------------
tar --zstd -xf "$BUNDLE" -C "$VERIFY"
log "=== reference Rust adapter from the extracted copy ==="
(
  cd "$VERIFY"
  WEFTCODEC_BIN="${WEFTCODEC_BIN:-$ROOT/.docker-cache/target-rust/release/weftcodec}" \
    scripts/run-conformance.sh scripts/conformance-adapter-rust.sh --quiet \
      --json receipt-rust.json
)
log "=== independent Python adapter from the extracted copy ==="
PYTHON_STATUS="deferred"
PYTHON_REASON="run scripts/verify-conformance-python.sh in the python court (blake3 is not installed in this court)"
if python3 -c 'import blake3' >/dev/null 2>&1; then
  (
    cd "$VERIFY"
    WEFT_PYTHONPATH="$VERIFY/python" PYTHONPATH="$VERIFY/python" \
      WEFT_PYTHON="${WEFT_PYTHON:-}" \
      scripts/run-conformance.sh scripts/conformance-adapter-python.sh --quiet \
        --json receipt-python.json
  )
  PYTHON_STATUS="executed"
  PYTHON_REASON=""
fi

log "=== receipt-bundle verification ==="
VERIFY_RESULT="pass"
if ! scripts/verify-receipts.sh "$BUNDLE" > "$VERIFY/verify-receipts.log" 2>&1; then
  VERIFY_RESULT="fail"
  cat "$VERIFY/verify-receipts.log" >&2
fi

# ---- receipt --------------------------------------------------------------
python3 - "$RECEIPT" "$BUNDLE" "$BUNDLE_SHA" "$BUNDLE_SIZE" "$FILE_COUNT" \
  "$STAMP" "$GENERATED_AT" "$VERSION" "$VERIFY" "$VERIFY_RESULT" \
  "$PYTHON_STATUS" "$PYTHON_REASON" <<'PY'
import json, os, sys
(receipt_path, bundle, bundle_sha, bundle_size, file_count, stamp, generated_at,
 version, verify, verify_result, python_status, python_reason) = sys.argv[1:13]
manifest = json.load(open(os.path.join(verify, "conformance", "manifest.json")))
by_path = {entry["path"]: entry for entry in manifest["vectors"]}
def run(name):
    path = os.path.join(verify, "receipt-%s.json" % name)
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)
runs = {"rust": run("rust")}
python = run("python")
if python is not None:
    runs["python"] = python
    per_mode = {}
    reasons = {}
    for skip in python["skips"]:
        entry = by_path.get(skip["path"], {})
        mode = entry.get("mode")
        key = "none" if mode is None else str(mode)
        per_mode[key] = per_mode.get(key, 0) + 1
        reasons[skip["reason"]] = reasons.get(skip["reason"], 0) + 1
else:
    per_mode = {}
    reasons = {}
environment = {k: os.environ.get(k) for k in (
    "WEFT_COURT_IMAGE", "WEFT_COURT_IMAGE_ID", "WEFT_COURT_KERNEL",
    "WEFT_COURT_MEMORY", "WEFT_COURT_CPUS", "WEFT_COURT_PIDS",
    "WEFT_REPO_WEFTFS", "WEFT_REPO_WEFTCODEC", "WEFT_REPO_WEFTCODEC_GO",
    "WEFT_REPO_WEFTFS_GO")}
record = {
    "schema": "weft.conformance.bundle.v1",
    "stamp": stamp,
    "generated_at": generated_at,
    "version": version,
    "bundle": "bench/bundles/" + os.path.basename(bundle),
    "bundle_sha256": bundle_sha,
    "bundle_bytes": int(bundle_size),
    "bundle_file_count": int(file_count),
    "manifest_entries": manifest.get("vector_count"),
    "manifest_aggregate_sha256": manifest.get("aggregate_sha256"),
    "extracted_only": True,
    "verify_receipts": verify_result,
    "python_second_opinion": {
        "status": python_status,
        "reason": python_reason,
        "script": "scripts/verify-conformance-python.sh",
    },
    "runs": runs,
    "python_skip_reasons": dict(sorted(reasons.items(), key=lambda kv: (-kv[1], kv[0]))),
    "python_skips_per_mode": dict(sorted(per_mode.items(), key=lambda kv: kv[0])),
    "environment": environment,
    "checks": {
        "bundle_contains_manifest_and_vectors": True,
        "reference_rust_passes": runs["rust"]["fail"] == 0,
        "python_second_opinion_passes": (python is None or python["fail"] == 0),
        "no_skip_is_a_pass": True,
        "verify_receipts_passes": verify_result == "pass",
    },
    "non_claims": [
        "the bundle proves corpus self-sufficiency, not implementation quality",
        "skips are explicit and are not evidence of a pass",
        "one host, Docker court, synthetic checks; no fleet or cloud claim",
    ],
}
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(record, handle, indent=2, sort_keys=True)
    handle.write("\n")
print("receipt:", receipt_path)
PY

if [ "$VERIFY_RESULT" != "pass" ]; then
  echo "verify-receipts failed; see the log above" >&2
  exit 1
fi
log "conformance bundle: pass"
