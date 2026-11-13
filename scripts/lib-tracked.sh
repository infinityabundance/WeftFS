#!/usr/bin/env bash
# Shared "is this evidence committed?" helper for the host-only guards.
#
# A file that exists only in the author's working tree is **not evidence**: a
# fresh checkout does not have it, so a guard can pass locally and fail in CI,
# or cite something a clone cannot verify. `docs/claims.json` learnt this from
# CI run 43408465922's sibling defect (a receipt cited by four claims and 66
# generated numbers was never committed); `docs/perf-numbers.json` had the same
# hole until the same rule was applied to it.
#
# The **nearest enclosing repository** decides, so files inside the WeftCodec,
# WeftCodec-Go and WeftFS-Go submodules are checked against their own index
# rather than the parent's (the parent records only a gitlink for each).
#
# Usage:
#   source "$(dirname "${BASH_SOURCE[0]}")/lib-tracked.sh"
#   weft_require_tracked SPEC_FILE "spec export" || status=1
#
#   weft_tracked PATH     0 = tracked, 1 = untracked, 2 = not inside a work tree
#
# Outside a git work tree the answer is 2 ("unknown") and callers skip the
# check: an exported tarball or the conformance bundle must still verify.
# No network, no writes, bash >= 4 (associative arrays).

declare -gA _WEFT_TRACKED_PATHS=()
declare -gA _WEFT_TRACKED_REPOS=()

_weft_repo_of() {
  local dir="$1"
  while :; do
    if [[ -e "$dir/.git" ]]; then
      printf '%s\n' "$dir"
      return 0
    fi
    local parent
    parent="$(dirname "$dir")"
    [[ "$parent" == "$dir" ]] && return 1
    dir="$parent"
  done
}

# weft_tracked PATH
weft_tracked() {
  local path="$1" dir abs repo rel
  dir="$(dirname -- "$path")"
  [[ -d "$dir" ]] || return 2
  abs="$(cd -- "$dir" && pwd)/$(basename -- "$path")"
  repo="$(_weft_repo_of "$abs")" || return 2
  if [[ -z "${_WEFT_TRACKED_REPOS[$repo]:-}" ]]; then
    if git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
      _WEFT_TRACKED_REPOS["$repo"]=indexed
      while IFS= read -r -d '' rel; do
        _WEFT_TRACKED_PATHS["$repo|$rel"]=1
      done < <(git -C "$repo" ls-files -z -- . 2>/dev/null)
    else
      _WEFT_TRACKED_REPOS["$repo"]=unknown
    fi
  fi
  [[ "${_WEFT_TRACKED_REPOS[$repo]}" == "indexed" ]] || return 2
  rel="${abs#"$repo"/}"
  [[ -n "${_WEFT_TRACKED_PATHS["$repo|$rel"]+set}" ]]
}

# weft_require_tracked PATH LABEL
# Returns 1 (and prints the reason) when PATH exists but is not committed.
weft_require_tracked() {
  local path="$1" label="$2" rc=0
  weft_tracked "$path" || rc=$?
  case "$rc" in
    0) return 0 ;;
    2) return 0 ;; # not a git work tree: existence is all a caller can test
    *)
      printf '%s: %s is not tracked by git; an untracked file is not evidence\n' \
        "$label" "$path" >&2
      return 1
      ;;
  esac
}
