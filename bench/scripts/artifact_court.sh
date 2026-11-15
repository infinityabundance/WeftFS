#!/usr/bin/env bash
# Host wrapper for the T048/T051/T052 artifact courts.
#
#   bash bench/scripts/artifact_court.sh adapter|view|cross_model
#
# The courts themselves run inside the fuse court. This wrapper exists so the
# aggregate can distinguish "the environment cannot run this" (one `skip` line
# and exit 78) from "the court failed" (any other non-zero exit).
set -Eeuo pipefail

cd "$(dirname "$0")/../.."

kind="${1:-}"
case "$kind" in
  adapter)
    prefix="adapter_court"
    script="bench/scripts/adapter_court.sh"
    env_prefix="WEFT_ADAPTERS_LIMIT=${WEFT_ADAPTERS_LIMIT:-3} WEFT_ADAPTER_MERGE_LIMIT=${WEFT_ADAPTER_MERGE_LIMIT:-1} WEFT_ADAPTER_MERGED_FULL=${WEFT_ADAPTER_MERGED_FULL:-0} WEFT_ADAPTER_MERGED_ZSTD=${WEFT_ADAPTER_MERGED_ZSTD:-0} WEFT_ADAPTER_DUMPS=${WEFT_ADAPTER_DUMPS:-1}"
    ;;
  view)
    prefix="view_identity_court"
    script="bench/scripts/view_identity_court.sh"
    env_prefix="WEFT_VIEW_TENSORS=${WEFT_VIEW_TENSORS:-model.layers.0.mlp.down_proj.weight} WEFT_VIEW_BLOCKS=${WEFT_VIEW_BLOCKS:-32,128}"
    ;;
  cross_model)
    prefix="cross_model_delta_court"
    script="bench/scripts/cross_model_delta_court.sh"
    env_prefix="WEFT_CROSS_SLICE_MIB=${WEFT_CROSS_SLICE_MIB:-64}"
    ;;
  *)
    echo "artifact_court: usage: $0 adapter|view|cross_model" >&2
    exit 64
    ;;
esac

skip() {
  echo "${prefix}: skip $1"
  exit 78
}

command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
docker image inspect weft/fuse-court:1 >/dev/null 2>&1 || skip "weft/fuse-court:1 is not available"
[[ -f .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors ]] || skip "the pinned Qwen2.5-0.5B-Instruct corpus is absent"
if [[ "$kind" == "cross_model" ]]; then
  for f in pythia-70m-step0 pythia-70m-step1 pythia-70m-step8 Qwen2.5-0.5B; do
    [[ -f ".docker-cache/real-corpus/$f.safetensors" ]] || skip "the pinned $f corpus is absent"
  done
fi

exec scripts/docker-court.sh fuse "cd /work && $env_prefix bash $script"
