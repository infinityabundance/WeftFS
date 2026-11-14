#!/usr/bin/env bash
# Comparator court (T041).
#
# Two pairs, one per density class:
#   sparse: pythia-70m step 0 -> step 1  (19 changed 16-bit words)
#   dense:  Qwen2.5-0.5B base -> Instruct (a fine-tune)
#
# Measures complete stored bytes (base + stored form of the newer container +
# every index byte), compression wall, decompression wall and exactness for
# each arm that can run. Arms that cannot run are listed with their exact
# blocker. Nothing is silently omitted.
#
# WeftFS figures are NOT re-measured here: this court drives external
# comparators. The gap table quotes WeftFS from the receipts named in
# bench/RESULTS-heldout.md, with their stamps and revisions.
#
# Environment skips print exactly one `comparator_court: skip <reason>` line and
# exit 78. All phases run in the bench court under the standard hard limits.
set -Eeuo pipefail

skip() {
  echo "comparator_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

corpus="$repo_root/.docker-cache/real-corpus"
for f in "$corpus/pythia-70m-step0.safetensors" "$corpus/pythia-70m-step1.safetensors" \
         "$corpus/Qwen2.5-0.5B.safetensors" "$corpus/Qwen2.5-0.5B-Instruct.safetensors"; do
  [[ -f "$f" ]] || skip "corpus $f is not present"
done

stamp="${WEFT_COMPARATOR_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

sparse="bench/receipts/comparators-sparse-$stamp.json"
dense="bench/receipts/comparators-dense-$stamp.json"
receipt="bench/receipts/comparators-$stamp.json"
log="bench/receipts/comparators-$stamp.log"

echo "comparator_court: stamp=$stamp sparse=pythia-70m-step0->1 dense=Qwen2.5-0.5B->Instruct" >&2

scripts/docker-court.sh bench "\
cd /work && \
WEFT_COMPARATOR_STAMP=$stamp python3 bench/scripts/comparator_court.py \
  /work/.docker-cache/real-corpus/pythia-70m-step0.safetensors \
  /work/.docker-cache/real-corpus/pythia-70m-step1.safetensors \
  $sparse && \
WEFT_COMPARATOR_STAMP=$stamp python3 bench/scripts/comparator_court.py \
  /work/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  $dense" \
  2>&1 | tee "$log"

chmod a+rw "$sparse" "$dense" "$log" 2>/dev/null || true

python3 scripts/assemble_comparators.py "$receipt" "$stamp" "$sparse" "$dense" "$log"
chmod a+rw "$receipt" 2>/dev/null || true
