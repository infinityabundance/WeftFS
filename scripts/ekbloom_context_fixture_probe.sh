#!/usr/bin/env bash
# EKBLOOM context court: fixture determinism probe.
#
# Builds the court fixture twice in one bench container and compares the two
# generated fixture JSON documents (which name every subject root, contract and
# authority) and their base-store heads. It exists because the frozen decisions
# receipt reports the base head and `fixture_heads_seen` but not a per-subject
# digest, and because a reader is entitled to know whether `prepare` is
# byte-reproducible across runs.
#
# This probe measures nothing in the court's gate set. It writes its own
# receipt and log; it never rewrites a measured receipt.
#
#   scripts/ekbloom_context_fixture_probe.sh
#
# A host without docker, or one that cannot resolve the pinned crate offline,
# prints one `ekbloom_context_fixture_probe: skip <reason>` line and exits 78.
set -Eeuo pipefail

skip() {
  echo "ekbloom_context_fixture_probe: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

stamp="${WEFT_EKBLOOM_FIXTURE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$repo_root/.docker-cache/ekbloom-fixture-probe-$stamp"
receipt="bench/receipts/ekbloom-context-fixture-$stamp.json"
log="bench/receipts/ekbloom-context-fixture-$stamp.log"
mkdir -p "$work" bench/receipts
chmod -R a+rwX "$work" bench/receipts 2>/dev/null || true
chmod a+rwX bench/ekbloom-context-harness 2>/dev/null || true

probe="$work/probe.sh"
cat > "$probe" <<'INNER'
set -Eeuo pipefail
export PATH="/usr/local/cargo/bin:$PATH"
H=/work/.docker-cache/target-ekbctx/release/ekbloom-context-harness
if ! (cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo metadata --offline --format-version 1 >/dev/null 2>/tmp/probe-meta.err); then
  echo "ekbloom_context_fixture_probe: skip cannot resolve the pinned ekbloom 0.7.3 offline: $(tail -1 /tmp/probe-meta.err)" >&2
  exit 78
fi
(cd /work/bench/ekbloom-context-harness && CARGO_TARGET_DIR=/work/.docker-cache/target-ekbctx cargo build --release --offline --quiet) || exit 1
D="$PROBE_DIR"
rm -rf "$D/a" "$D/b" "$D/a.json" "$D/b.json" "$D/result.json"
mkdir -p "$D"
$H prepare --base "$D/a" --out "$D/a.json" >"$D/a.out" 2>&1 || { tail -5 "$D/a.out" >&2; exit 1; }
$H prepare --base "$D/b" --out "$D/b.json" >"$D/b.out" 2>&1 || { tail -5 "$D/b.out" >&2; exit 1; }
sha_a="$(sha256sum "$D/a.json" | cut -d' ' -f1)"
sha_b="$(sha256sum "$D/b.json" | cut -d' ' -f1)"
head_a="$(sed -n 's/.*head=\([0-9a-f]\{64\}\).*/\1/p' "$D/a.out" | tail -1)"
head_b="$(sed -n 's/.*head=\([0-9a-f]\{64\}\).*/\1/p' "$D/b.out" | tail -1)"
identical=false
if cmp -s "$D/a.json" "$D/b.json"; then identical=true; fi
printf '{"fixture_json_sha256_a":"%s","fixture_json_sha256_b":"%s","fixture_json_identical":%s,"base_head_a":"%s","base_head_b":"%s"}\n' \
  "$sha_a" "$sha_b" "$identical" "$head_a" "$head_b" > "$D/result.json"
cat "$D/result.json"
INNER
chmod +x "$probe"

rc=0
scripts/docker-court.sh bench \
  "cd /work && PROBE_DIR=/work/.docker-cache/ekbloom-fixture-probe-$stamp bash .docker-cache/ekbloom-fixture-probe-$stamp/probe.sh" \
  >"$work/run.log" 2>&1 || rc=$?
if [[ "$rc" -eq 78 ]]; then
  reason="$(grep -h '^ekbloom_context_fixture_probe: skip ' "$work/run.log" | tail -1 | sed 's/^ekbloom_context_fixture_probe: skip //' || true)"
  [[ -z "$reason" ]] && reason="the probe container skipped without a printed reason"
  echo "ekbloom_context_fixture_probe: skip $reason" >&2
  exit 78
fi
if [[ "$rc" -ne 0 ]]; then
  echo "ekbloom_context_fixture_probe: probe failed; tail of $work/run.log follows" >&2
  tail -40 "$work/run.log" >&2 || true
  exit 1
fi

result="$work/result.json"
[[ -f "$result" ]] || { echo "ekbloom_context_fixture_probe: missing $result" >&2; exit 1; }

revision="$(git rev-parse HEAD)"
jq -n \
  --arg schema "weft.ekbloom_context.fixture_probe.v1" \
  --arg stamp "$stamp" \
  --arg revision "$revision" \
  --arg crate "ekbloom 0.7.3" \
  --arg checksum "1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4" \
  --arg contract_sha256 "$(sha256sum bench/CONTRACTS-ekbloom-context.md | cut -d' ' -f1)" \
  --argjson probe "$(cat "$result")" \
  '{
     schema: $schema,
     receipt_for: "fixture determinism only; not a gate measurement",
     generated_utc: $stamp,
     revision: $revision,
     ekbloom: {crate: $crate, checksum: $checksum},
     contract_sha256: $contract_sha256,
     method: "two consecutive `prepare` runs in one bench container, same revision, same image, same fixed timestamps",
     probe: $probe,
     interpretation: (
       if $probe.fixture_json_identical then
         "the fixture builder is byte-reproducible across runs at this revision"
       else
         "the fixture builder is not byte-reproducible across runs: the KV page writer uses Store::create_file, whose ObjectId seed comes from os_random_32, so page ids and every root derived from them differ per run. Within one court run every arm shares one base head (receipt methodology.fixture_heads_seen), so the comparison is sound; cross-run byte identity is not claimed."
       end
     )
   }' > "$receipt"
cp "$work/run.log" "$log"
echo "ekbloom_context_fixture_probe: receipt $receipt" >&2
echo "ekbloom_context_fixture_probe: identical=$(jq -r '.probe.fixture_json_identical' "$receipt")" >&2
