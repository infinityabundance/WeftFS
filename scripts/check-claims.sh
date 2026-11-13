#!/usr/bin/env bash
# Fail closed when an externally visible documentation claim drifts.
#
# Manifest: docs/claims.json, a JSON array of entries:
#   { id, claim, value, locations[], receipt|null, kind }
#   kind = "present": `value` must occur verbatim in every listed file.
#   kind = "absent":  `value` must not occur in any listed file.
# Every listed file and every non-null receipt must exist.
#
# Provenance rules added after the first real CI run:
#
#   1. A cited receipt must be **committed**. A receipt that exists only in the
#      author's working tree is not evidence: four claims and 66 generated
#      numbers cited r22-pareto-20261112T230000Z.json, which had never been
#      committed, and this script passed locally while the CI checkout failed.
#      The same rule now covers every listed location and the manifest itself:
#      a document that exists only locally cannot be the place a claim is
#      checked in a clone. The check is skipped (with a note) outside a git
#      work tree, so an exported tarball or the conformance bundle still
#      verifies.
#
#   2. A claim may not silently cite a **superseded** receipt. Supersession is
#      declared explicitly in docs/superseded-receipts.json (receipt,
#      superseded_by, reason, declared_in) and never inferred from prose: the
#      results documents use the word "superseded" for unrelated neighbouring
#      rows, so a text scan produces false positives (measured on this tree).
#      A claim citing a registered receipt must carry either `superseded_by`
#      (the same receipt the registry names) or a non-empty
#      `supersession_note` explaining why the claim is still valid.
#
#   Optional claim fields, ignored by the presence rules above:
#     superseded_by      bench/receipts/... that supersedes the cited evidence
#     supersession_note  why a claim survives a defect in the cited receipt
#
# This is a host-only gate: no Docker, no compilation, no network.
#
# Usage: scripts/check-claims.sh [manifest] [superseded-registry]
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

manifest="${1:-docs/claims.json}"
registry="${2:-docs/superseded-receipts.json}"
failures=0
count=0

mismatch() {
  printf 'CLAIM MISMATCH %s: %s: expected %s: %s\n' "$1" "$2" "$3" "$4" >&2
  failures=$((failures + 1))
}

if [[ ! -f "$manifest" ]]; then
  mismatch manifest "$manifest" present "claims manifest file"
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "CLAIM ERROR: jq is required to read $manifest" >&2
  exit 1
fi
if ! jq -e 'type == "array"' "$manifest" >/dev/null 2>&1; then
  echo "CLAIM ERROR: $manifest must be a JSON array" >&2
  exit 1
fi

count="$(jq 'length' "$manifest")"
if [[ "$count" -eq 0 ]]; then
  echo "CLAIM ERROR: $manifest has no entries" >&2
  exit 1
fi

# ---- committed-file index -------------------------------------------------
# Only files that git knows about are evidence. `lib-tracked.sh` resolves each
# path against its **nearest enclosing repository**, so a location inside the
# WeftCodec / WeftCodec-Go / WeftFS-Go submodules is checked against that
# repository's index (the parent records only a gitlink for it).
# shellcheck source=scripts/lib-tracked.sh
source "$root/scripts/lib-tracked.sh"
in_git=0
if command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  in_git=1
fi

require_committed() {
  local id="$1" path="$2" rc=0
  weft_tracked "$path" || rc=$?
  if [[ "$rc" -eq 1 ]]; then
    mismatch "$id" "$path" present "committed in git; an untracked file is not evidence"
  fi
}

require_committed manifest "$manifest"

# ---- supersession registry ------------------------------------------------
declare -A superseded_by
declare -A superseded_reason
if [[ -f "$registry" ]]; then
  if ! jq -e 'type == "array"' "$registry" >/dev/null 2>&1; then
    echo "CLAIM ERROR: $registry must be a JSON array" >&2
    exit 1
  fi
  while IFS= read -r entry; do
    receipt="$(jq -r '.receipt // empty' <<<"$entry")"
    target="$(jq -r '.superseded_by // empty' <<<"$entry")"
    reason="$(jq -r '.reason // empty' <<<"$entry")"
    declared="$(jq -r '.declared_in // empty' <<<"$entry")"
    if [[ -z "$receipt" || -z "$target" ]]; then
      mismatch registry "$registry" present "receipt and superseded_by"
      continue
    fi
    if [[ "$receipt" != bench/receipts/* || "$target" != bench/receipts/* ]]; then
      mismatch registry "$receipt" present "a bench/receipts path for receipt and superseded_by"
      continue
    fi
    if [[ "$receipt" == "$target" ]]; then
      mismatch registry "$receipt" present "superseded_by different from receipt"
      continue
    fi
    if [[ -n "${superseded_by[$receipt]:-}" ]]; then
      mismatch registry "$receipt" absent "a duplicate registry entry"
      continue
    fi
    if [[ -z "$reason" || -z "$declared" ]]; then
      mismatch registry "$receipt" present "a non-empty reason and declared_in"
      continue
    fi
    if [[ ! -f "$receipt" ]]; then
      mismatch registry "$receipt" present "the superseded receipt file"
      continue
    fi
    if [[ ! -f "$target" ]]; then
      mismatch registry "$target" present "the superseding receipt file"
      continue
    fi
    if [[ ! -f "$declared" ]]; then
      mismatch registry "$declared" present "the document that declares the supersession"
      continue
    fi
    require_committed registry "$receipt"
    require_committed registry "$target"
    require_committed registry "$declared"
    superseded_by["$receipt"]="$target"
    superseded_reason["$receipt"]="$reason"
  done < <(jq -c '.[]' "$registry")
  require_committed registry "$registry"
else
  echo "claims: note: no supersession registry at $registry; supersession is not checked" >&2
fi

# ---- claims ---------------------------------------------------------------
while IFS= read -r entry; do
  id="$(jq -r '.id // empty' <<<"$entry")"
  kind="$(jq -r '.kind // empty' <<<"$entry")"
  value="$(jq -r '.value // empty' <<<"$entry")"
  receipt="$(jq -r '.receipt // empty' <<<"$entry")"
  sup_target="$(jq -r '.superseded_by // empty' <<<"$entry")"
  sup_note="$(jq -r '.supersession_note // empty' <<<"$entry")"
  mapfile -t locations < <(jq -r '.locations // [] | .[]' <<<"$entry")

  [[ -z "$id" ]] && continue

  if [[ "$kind" != "present" && "$kind" != "absent" ]]; then
    mismatch "$id" "$manifest" present "kind present|absent (found '$kind')"
    continue
  fi
  if [[ -z "$value" ]]; then
    mismatch "$id" "$manifest" present "non-empty value"
    continue
  fi
  if [[ -n "$receipt" ]]; then
    if [[ "$receipt" != bench/receipts/* ]]; then
      mismatch "$id" "$receipt" present "receipt under bench/receipts"
    elif [[ ! -f "$receipt" ]]; then
      mismatch "$id" "$receipt" present "$receipt"
    else
      require_committed "$id" "$receipt"
    fi
  fi

  # An explicitly declared supersession must point at real, committed evidence
  # other than the receipt it replaces.
  if [[ -n "$sup_target" ]]; then
    if [[ "$sup_target" != bench/receipts/* ]]; then
      mismatch "$id" "$sup_target" present "superseded_by under bench/receipts"
    elif [[ "$sup_target" == "$receipt" ]]; then
      mismatch "$id" "$sup_target" absent "superseded_by equal to receipt"
    elif [[ ! -f "$sup_target" ]]; then
      mismatch "$id" "$sup_target" present "$sup_target"
    else
      require_committed "$id" "$sup_target"
    fi
  fi

  # A registered superseded receipt may not be cited without a declaration,
  # and a declaration may not name a different successor than the registry.
  if [[ -n "$receipt" && -n "${superseded_by[$receipt]:-}" ]]; then
    registered="${superseded_by[$receipt]}"
    if [[ -z "$sup_target" && -z "$sup_note" ]]; then
      mismatch "$id" "$receipt" present \
        "superseded_by or supersession_note (registered superseded: ${superseded_reason[$receipt]})"
    fi
    if [[ -n "$sup_target" && "$sup_target" != "$registered" ]]; then
      mismatch "$id" "$sup_target" present "the registered successor $registered"
    fi
  fi

  if [[ ${#locations[@]} -eq 0 ]]; then
    mismatch "$id" "$manifest" present "at least one location"
    continue
  fi

  for location in "${locations[@]}"; do
    if [[ ! -f "$location" ]]; then
      mismatch "$id" "$location" present "$value"
      continue
    fi
    require_committed "$id" "$location"
    if grep -F -q -- "$value" "$location"; then
      found=1
    else
      found=0
    fi
    if [[ "$kind" == "present" && "$found" -eq 0 ]]; then
      mismatch "$id" "$location" present "$value"
    elif [[ "$kind" == "absent" && "$found" -eq 1 ]]; then
      mismatch "$id" "$location" absent "$value"
    fi
  done
done < <(jq -c '.[]' "$manifest")

if [[ "$failures" -ne 0 ]]; then
  echo "claims: FAIL ($failures mismatch(es) across $count entries)" >&2
  exit 1
fi
if [[ "$in_git" -eq 1 ]]; then
  printf 'claims: OK (%s entries; every cited receipt and location is committed; %s superseded receipt(s) declared)\n' \
    "$count" "${#superseded_by[@]}"
else
  printf 'claims: OK (%s entries; not a git work tree, commit checks skipped)\n' "$count"
fi
