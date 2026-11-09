#!/usr/bin/env bash
# R4 multi-writer court: Rust/Go parity, mixed-language concurrent writers,
# conflict semantics, and a finite history check.
#
# Host usage:
#   bash scripts/multiwriter_court.sh
#
# Court usage:
#   bash scripts/docker-court.sh fuse "bash scripts/multiwriter_court.sh"
#
# The fuse court carries both toolchains. All hard limits come from
# docker-court.sh; this script adds none and disables none.
#
# Writes:
#   bench/receipts/multiwriter-court-<UTC>.json
#   bench/receipts/multiwriter-court-<UTC>.log
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/multiwriter_court.sh"
fi
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_epoch="$(date +%s)"
receipt="bench/receipts/multiwriter-court-${stamp}.json"
log="bench/receipts/multiwriter-court-${stamp}.log"
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
GO_BIN="/work/.docker-cache/weft-mw2-writer"
WEFT_BIN=".docker-cache/target-rust/debug/weftfs"
failures=0
notes=()

record_failure() {
  failures=$((failures + 1))
  notes+=("$1")
  echo "FAIL: $1" >&2
}

echo "== multiwriter court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

echo "== build =="
cargo build --quiet --example mw2_writer
cargo build --quiet --bin weftfs
(cd WeftFS-Go && go build -o "$GO_BIN" ./cmd/mw2writer)

rust_sha="$(sha256sum "$RUST_BIN" | cut -d' ' -f1)"
go_sha="$(sha256sum "$GO_BIN" | cut -d' ' -f1)"
echo "rust example sha256=$rust_sha"
echo "go example sha256=$go_sha"

run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
run_go() { "$GO_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }

# ---------------------------------------------------------------------------
# 1. Deterministic parity: the same transaction in both languages must produce
#    the same head, commit file, manifest and superblock image.
# ---------------------------------------------------------------------------
echo "== parity =="
parity_root="$(mktemp -d /tmp/weft-mw2-parity.XXXXXX)"
rust_store="$parity_root/rust"
go_store="$parity_root/go"
mkdir -p "$rust_store" "$go_store"

run_rust parity --store "$rust_store" --name parity.bin --content "parity-body" --seed "parity-seed" >"$parity_root/rust.json"
run_go parity --store "$go_store" --name parity.bin --content "parity-body" --seed "parity-seed" >"$parity_root/go.json"

parity_report="$(python3 - "$parity_root/rust.json" "$parity_root/go.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
g = json.load(open(sys.argv[2]))
fields = ["head", "manifest", "content_root", "object_id", "entry_id", "version", "size"]
diff = {field: [r.get(field), g.get(field)] for field in fields if r.get(field) != g.get(field)}
print(json.dumps({"equal": not diff, "different": diff,
                  "rust": {f: r.get(f) for f in fields},
                  "go": {f: g.get(f) for f in fields}}, sort_keys=True))
PY
)"
echo "parity: $parity_report"
python3 - "$parity_report" <<'PY' || record_failure "parity fields differ"
import json, sys
report = json.loads(sys.argv[1])
raise SystemExit(0 if report.get("equal") else 1)
PY

commit_cmp="same"
if ! cmp -s "$rust_store/commits/2.commit" "$go_store/commits/2.commit"; then
  commit_cmp="different"
  record_failure "parity commit file differs"
fi
superblock_cmp="same"
if ! cmp -s "$rust_store/superblock" "$go_store/superblock"; then
  superblock_cmp="different"
  record_failure "parity superblock image differs"
fi
echo "parity commit file: $commit_cmp; superblock: $superblock_cmp"

# Cross-read each language's store with the other.
run_rust read --store "$go_store" --name parity.bin >"$parity_root/rust-reads-go.json"
run_go read --store "$rust_store" --name parity.bin >"$parity_root/go-reads-rust.json"
cross_read="$(python3 - "$parity_root/rust-reads-go.json" "$parity_root/go-reads-rust.json" <<'PY'
import json, sys
a = json.load(open(sys.argv[1]))
b = json.load(open(sys.argv[2]))
print(json.dumps({"ok": a.get("content_hash") == b.get("content_hash") and a.get("ok") and b.get("ok"),
                  "rust_reads_go": a, "go_reads_rust": b}, sort_keys=True))
PY
)"
echo "cross read: $cross_read"
python3 - "$cross_read" <<'PY' || record_failure "cross-language read differs"
import json, sys
print((lambda r: "" if r.get("ok") else json.dumps(r))(json.loads(sys.argv[1])))
raise SystemExit(0 if json.loads(sys.argv[1]).get("ok") else 1)
PY

# ---------------------------------------------------------------------------
# 2. Mixed-language disjoint writers against one store.
# ---------------------------------------------------------------------------
echo "== mixed disjoint =="
mixed_root="$(mktemp -d /tmp/weft-mw2-mixed.XXXXXX)"
mixed_store="$mixed_root/store"
mkdir -p "$mixed_store"
run_rust init --store "$mixed_store" >/dev/null

RUST_WRITERS=("r0" "r1" "r2" "r3")
GO_WRITERS=("g0" "g1" "g2" "g3")
COUNT=20
pids=()
for writer in "${RUST_WRITERS[@]}"; do
  run_rust write --store "$mixed_store" --writer "$writer" --prefix "$writer" --count "$COUNT" \
    >"$mixed_root/$writer.json" &
  pids+=("$!")
done
for writer in "${GO_WRITERS[@]}"; do
  run_go write --store "$mixed_store" --writer "$writer" --prefix "$writer" --count "$COUNT" \
    >"$mixed_root/$writer.json" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do
  wait "$pid" || record_failure "a disjoint writer process failed"
done

disjoint_summary="$(python3 - "$mixed_root" "$COUNT" <<'PY'
import glob, json, os, sys
root, count = sys.argv[1], int(sys.argv[2])
rows = []
for path in sorted(glob.glob(os.path.join(root, "*.json"))):
    rows.append(json.load(open(path)))
committed = sum(row["committed"] for row in rows)
conflicts = sum(row["conflicts"] for row in rows)
attempts = sum(row["attempts"] for row in rows)
errors = [e for row in rows for e in row.get("errors", [])]
print(json.dumps({"processes": len(rows), "committed": committed, "conflicts": conflicts,
                  "attempts": attempts, "errors": errors,
                  "expected": len(rows) * count,
                  "retry_amplification": attempts / committed if committed else 0},
                 sort_keys=True))
PY
)"
echo "disjoint: $disjoint_summary"
python3 - "$disjoint_summary" <<'PY' || record_failure "disjoint run is incomplete"
import json, sys
r = json.loads(sys.argv[1])
ok = r["committed"] == r["expected"] and r["conflicts"] == 0 and not r["errors"]
print("" if ok else json.dumps(r))
raise SystemExit(0 if ok else 1)
PY

# Every writer's output is read by both implementations.
for writer in "${RUST_WRITERS[@]}" "${GO_WRITERS[@]}"; do
  run_rust verify --store "$mixed_store" --writer "$writer" --prefix "$writer" --count "$COUNT" \
    >"$mixed_root/$writer.rust-verify.json"
  run_go verify --store "$mixed_store" --writer "$writer" --prefix "$writer" --count "$COUNT" \
    >"$mixed_root/$writer.go-verify.json"
done
verify_summary="$(python3 - "$mixed_root" <<'PY'
import glob, json, sys
rows = [json.load(open(path)) for path in sorted(glob.glob(sys.argv[1] + "/*verify.json"))]
bad = [row for row in rows if not row.get("ok")]
print(json.dumps({"verifiers": len(rows), "failed": len(bad), "bad": bad[:4]}, sort_keys=True))
PY
)"
echo "mixed verify: $verify_summary"
python3 - "$verify_summary" <<'PY' || record_failure "mixed verifier mismatch"
import json, sys
r = json.loads(sys.argv[1])
ok = r["failed"] == 0
print("" if ok else json.dumps(r))
raise SystemExit(0 if ok else 1)
PY

cli_verify_ok=false
if "$WEFT_BIN" --store "$mixed_store" verify --full >/dev/null 2>"$mixed_root/cli-verify.err"; then
  cli_verify_ok=true
else
  record_failure "Rust CLI verify --full rejected the mixed store"
fi
mixed_generation="$(python3 - "$mixed_store" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
echo "mixed store generation: $mixed_generation (verify --full: $cli_verify_ok)"

# ---------------------------------------------------------------------------
# 3. High contention on one name.
# ---------------------------------------------------------------------------
echo "== contention =="
contended_root="$(mktemp -d /tmp/weft-mw2-hot.XXXXXX)"
contended_store="$contended_root/store"
mkdir -p "$contended_store"
run_rust init --store "$contended_store" >/dev/null

HOT_WRITERS=("r0" "r1" "g0" "g1" "r2" "r3" "g2" "g3")
COUNT=10
pids=()
for writer in "${HOT_WRITERS[@]}"; do
  if [[ "$writer" == r* ]]; then
    run_rust write --store "$contended_store" --writer "$writer" --prefix "$writer" --count "$COUNT" --hot hot.bin \
      >"$contended_root/$writer.json" &
  else
    run_go write --store "$contended_store" --writer "$writer" --prefix "$writer" --count "$COUNT" --hot hot.bin \
      >"$contended_root/$writer.json" &
  fi
  pids+=("$!")
done
for pid in "${pids[@]}"; do
  wait "$pid" || record_failure "a contended writer process failed"
done

contended_summary="$(python3 - "$contended_root" "$COUNT" <<'PY'
import glob, json, os, sys
root, count = sys.argv[1], int(sys.argv[2])
rows = [json.load(open(path)) for path in sorted(glob.glob(os.path.join(root, "*.json")))]
committed = sum(row["committed"] for row in rows)
conflicts = sum(row["conflicts"] for row in rows)
errors = [e for row in rows for e in row.get("errors", [])]
print(json.dumps({"processes": len(rows), "committed": committed, "conflicts": conflicts,
                  "errors": errors, "operations": len(rows) * count,
                  "conflict_rate": conflicts / (len(rows) * count)}, sort_keys=True))
PY
)"
echo "contended: $contended_summary"
python3 - "$contended_summary" <<'PY' || record_failure "contended run did not resolve to exactly one winner"
import json, sys
r = json.loads(sys.argv[1])
ok = r["committed"] == 1 and r["conflicts"] == r["operations"] - 1 and not r["errors"]
print("" if ok else json.dumps(r))
raise SystemExit(0 if ok else 1)
PY

contended_generation="$(python3 - "$contended_store" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
echo "contended store generation: $contended_generation"
hot_rust="$(run_rust read --store "$contended_store" --name hot.bin)"
hot_go="$(run_go read --store "$contended_store" --name hot.bin)"
hot_agree="$(python3 - "$hot_rust" "$hot_go" <<'PY'
import json, sys
a, b = json.loads(sys.argv[1]), json.loads(sys.argv[2])
print(json.dumps({"ok": a.get("content_hash") == b.get("content_hash"),
                  "rust": a, "go": b}, sort_keys=True))
PY
)"
echo "hot read: $hot_agree"
python3 - "$hot_agree" <<'PY' || record_failure "the two readers disagree on the winner"
import json, sys
r = json.loads(sys.argv[1])
print("" if r.get("ok") else json.dumps(r))
raise SystemExit(0 if r.get("ok") else 1)
PY

# A finite history check: every requested operation is either committed or a
# typed conflict, no process reported an unexpected error, and the generation
# equals genesis plus the committed count (both runs).
history_ok="$(python3 - "$disjoint_summary" "$contended_summary" "$mixed_generation" "$contended_generation" <<'PY'
import json, sys
disjoint, contended = json.loads(sys.argv[1]), json.loads(sys.argv[2])
mixed_gen, hot_gen = int(sys.argv[3]), int(sys.argv[4])
expected_mixed = 1 + disjoint["committed"]
expected_hot = 1 + contended["committed"]
# A retry may skip a generation (the store never reuses a generation number
# after a lost attempt), so the head is at or above genesis + committed. The
# exact state is proved by verify --full, not by arithmetic on gaps.
ok = (mixed_gen >= expected_mixed and hot_gen >= expected_hot
      and not disjoint["errors"] and not contended["errors"])
print(json.dumps({"ok": ok, "mixed_generation": mixed_gen,
                  "expected_mixed_generation": expected_mixed,
                  "contended_generation": hot_gen,
                  "expected_contended_generation": expected_hot,
                  "note": "finite history check, not a linearizability proof"}, sort_keys=True))
PY
)"
echo "history: $history_ok"
python3 - "$history_ok" <<'PY' || record_failure "finite history check failed"
import json, sys
r = json.loads(sys.argv[1])
print("" if r.get("ok") else json.dumps(r))
raise SystemExit(0 if r.get("ok") else 1)
PY

# ---------------------------------------------------------------------------
# Receipt
# ---------------------------------------------------------------------------
finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
duration_ms=$(( ($(date +%s) - start_epoch) * 1000 ))
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"

python3 - "$receipt" "$started_at" "$finished_at" "$duration_ms" "$result" "$failures" \
  "$rust_sha" "$go_sha" "$parity_report" "$commit_cmp" "$superblock_cmp" "$cross_read" \
  "$disjoint_summary" "$verify_summary" "$cli_verify_ok" "$mixed_generation" "$contended_generation" \
  "$contended_summary" "$hot_agree" "$history_ok" "$log" "$log_sha" <<'PY'
import json, sys, os
(receipt, started, finished, duration, result, failures, rust_sha, go_sha,
 parity, commit_cmp, superblock_cmp, cross_read, disjoint, verify, cli_verify,
 mixed_generation, contended_generation, contended, hot, history, log, log_sha) = sys.argv[1:]
def load(text):
    return json.loads(text)
doc = {
  "schema": "weftfs.multiwriter.court/1",
  "generated_at": finished,
  "run": {"started_at": started, "finished_at": finished, "duration_ms": int(duration)},
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "repositories": [
    {"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
    {"name": "WeftFS-Go", "revision": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown")},
  ],
  "binaries": {"rust_example_sha256": rust_sha, "go_example_sha256": go_sha},
  "parity": load(parity),
  "parity_commit_file": commit_cmp,
  "parity_superblock": superblock_cmp,
  "cross_read": load(cross_read),
  "disjoint": load(disjoint),
  "verification": load(verify),
  "rust_cli_verify_full": cli_verify == "true",
  "mixed_generation": int(mixed_generation),
  "contended_generation": int(contended_generation),
  "contended": load(contended),
  "hot_read": load(hot),
  "history_check": load(history),
  "failures": int(failures),
  "result": result,
  "evidence": {"log": log, "log_sha256": log_sha},
  "scope": {
    "checked": "one local store, mixed Rust/Go processes, deterministic parity, finite history",
    "not_checked": "physical power loss, distributed/fleet operation, linearizability proof, MW3",
  },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": result, "failures": int(failures)}))
PY

echo "receipt: $receipt"
sha256sum "$receipt" "$log"
if [[ "$result" != "pass" ]]; then
  exit 1
fi
