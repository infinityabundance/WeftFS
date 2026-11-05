#!/usr/bin/env bash
# Fail closed when an externally visible documentation claim drifts.
#
# Manifest: docs/claims.json, a JSON array of entries:
#   { id, claim, value, locations[], receipt|null, kind }
#   kind = "present": `value` must occur verbatim in every listed file.
#   kind = "absent":  `value` must not occur in any listed file.
# Every listed file and every non-null receipt must exist.
#
# This is a host-only gate: no Docker, no compilation, no network.
#
# Usage: scripts/check-claims.sh [manifest]
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

manifest="${1:-docs/claims.json}"
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

while IFS= read -r entry; do
  id="$(jq -r '.id // empty' <<<"$entry")"
  kind="$(jq -r '.kind // empty' <<<"$entry")"
  value="$(jq -r '.value // empty' <<<"$entry")"
  receipt="$(jq -r '.receipt // empty' <<<"$entry")"
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
printf 'claims: OK (%s entries)\n' "$count"
