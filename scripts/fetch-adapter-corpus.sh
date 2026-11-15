#!/usr/bin/env bash
# Fetch the pinned public LoRA/DoRA adapter corpus named by
# `bench/corpora/real-models/adapters-qwen2.5-0.5b.json`.
#
#   scripts/fetch-adapter-corpus.sh            # download what is missing
#   scripts/fetch-adapter-corpus.sh --check    # verify only, never download
#
# The `adapters` court (T048) measures real adapter artifacts; it reads the
# files from `.docker-cache/t048/<owner>__<name>/`, the path each manifest entry
# declares in `local_path`. The corpus is third-party data and is never
# committed, so a fresh checkout, or a cleaned `.docker-cache`, has to be able
# to rebuild it identically. This script is that path: it downloads exactly the
# URL at the exact revision the manifest names and checks the recorded sha256.
#
# A missing corpus is an environment condition, not a product failure; the
# court's host wrapper prints one `adapter_court: skip <reason>` line and exits
# 78 in that case. This script is how you make the reason go away.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

manifest="bench/corpora/real-models/adapters-qwen2.5-0.5b.json"
check_only=0
for arg in "$@"; do
  case "$arg" in
    --check) check_only=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

[[ -f "$manifest" ]] || { echo "adapter corpus manifest $manifest is missing" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

python3 - "$manifest" "$repo_root" "$check_only" <<'PY'
import hashlib
import json
import os
import subprocess
import sys

manifest, repo_root, check_only = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
data = json.load(open(manifest))

missing, bad, fetched, ok = [], [], 0, 0

for adapter in data["adapters"]:
    local = adapter["local_path"]
    local_dir = os.path.dirname(os.path.join(repo_root, local))
    for entry in adapter["files"]:
        target = os.path.join(local_dir, entry["path"])
        want = entry["sha256"]
        if os.path.isfile(target):
            digest = hashlib.sha256(open(target, "rb").read()).hexdigest()
            if digest == want:
                ok += 1
                continue
            bad.append((target, digest, want))
        else:
            missing.append(target)
        if check_only:
            continue
        os.makedirs(local_dir, exist_ok=True)
        part = target + ".part"
        subprocess.run(
            ["curl", "-fsSL", "--retry", "3", "--connect-timeout", "30",
             "-o", part, entry["url"]],
            check=True,
        )
        digest = hashlib.sha256(open(part, "rb").read()).hexdigest()
        if digest != want:
            os.unlink(part)
            bad.append((target, digest, want))
            continue
        os.replace(part, target)
        fetched += 1

for target, digest, want in bad:
    print("sha256 mismatch: %s\n  got  %s\n  want %s" % (target, digest, want), file=sys.stderr)
for target in missing:
    if check_only:
        print("missing: %s" % target, file=sys.stderr)

print(
    "adapter corpus: %d file(s) already correct, %d fetched, %d missing, %d mismatched"
    % (ok, fetched, len(missing) if check_only else 0, len(bad))
)
sys.exit(1 if (bad or (check_only and missing)) else 0)
PY
