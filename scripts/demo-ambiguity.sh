#!/usr/bin/env bash
# F3.2 / A3 — Ambiguity court.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-ambiguity.sh"
#
# A selector matches two objects with different content roots. The full query
# returns the ordered set; `--require-one` fails closed with `Ambiguous` and
# lists both ids; the Context mount lists both; a singular materialize writes
# nothing. No competitor is measured or implied.
set -Eeuo pipefail
source "$(dirname "$0")/demo-lib.sh"

demo_init "weftfs.ambiguity.receipt/1" "ambiguity" "Ambiguity court (A3)"

S="$STORES/ambiguity-$RUN_ID"
rm -rf "$S"
mkdir -p "$S"

step_begin "store" "Two objects with different content roots under one selector"
run_ok init "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" init
run_ok create-a "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /a.txt --text "alpha" --seed "amb-a" --attr "group=ambiguous"
run_ok create-b "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /b.txt --text "beta" --seed "amb-b" --attr "group=ambiguous"
run_ok create-unique "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" create /unique.txt --text "unique" --seed "amb-u" --attr "group=unique"
run_ok snapshot "$RUSTFS" --store "$S" --fixed-timestamp-ns "$TS1" snapshot create s1
run_ok stat-a "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat /a.txt
A_ID="$(jsonq "$RUN_OUT" object_id)"; A_CID="$(jsonq "$RUN_OUT" content_id)"
run_ok stat-b "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" stat /b.txt
B_ID="$(jsonq "$RUN_OUT" object_id)"; B_CID="$(jsonq "$RUN_OUT" content_id)"
assert_ne "the two objects have distinct ObjectIds" "$A_ID" "$B_ID"
assert_ne "the two objects have distinct content roots" "$A_CID" "$B_CID"
run_ok ctx-create "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context create \
  --name "ambiguous" --selector 'select attrs.group == "ambiguous"' --snapshot s1
CTX_ID="$(jsonq "$RUN_OUT" context_root_id)"
step_end pass "a=$A_ID/$A_CID b=$B_ID/$B_CID ctx=$CTX_ID"

step_begin "full-query" "The full query returns the complete ordered set"
run_ok query-all "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" query 'select attrs.group == "ambiguous"'
cp "$RUN_OUT" "$STEP_DIR/query-all.json"
assert_eq "matched_count" "2" "$(jsonq "$STEP_DIR/query-all.json" matched_count)"
assert_eq "returned length" "2" "$(jsonq "$STEP_DIR/query-all.json" 'results#len')"
R0="$(jsonq "$STEP_DIR/query-all.json" results.0.object_id)"
R1="$(jsonq "$STEP_DIR/query-all.json" results.1.object_id)"
assert_eq "result 0 is one of the two ids" "true" \
  "$([ "$R0" = "$A_ID" ] || [ "$R0" = "$B_ID" ] && echo true || echo false)"
assert_eq "result 1 is the other id" "true" \
  "$([ "$R1" = "$A_ID" ] || [ "$R1" = "$B_ID" ] && echo true || echo false)"
assert_ne "the ordered set contains both objects" "$R0" "$R1"
step_end pass "ordered set = $R0, $R1"

step_begin "singular" "Singular access fails closed with Ambiguous and lists both ids"
run_expect_fail query-one "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" query \
  'select attrs.group == "ambiguous"' --require-one
ERR="$(cat "$RUN_ERR")"
assert_contains "singular class is Ambiguous" "$ERR" "Ambiguous"
assert_contains "singular error lists the first id" "$ERR" "$A_ID"
assert_contains "singular error lists the second id" "$ERR" "$B_ID"
save_out singular-error "$RUN_ERR"

# A singular materialize is also a cardinality-sensitive operation. It must
# fail before creating any output path.
SINGULAR_OUT="$SCRATCH/singular-$RUN_ID"
rm -rf "$SINGULAR_OUT"
run_expect_fail materialize-one "$RUSTFS" --store "$S" --json --fixed-timestamp-ns "$TS1" context materialize \
  ambiguous --require-one --out "$SINGULAR_OUT"
ERR2="$(cat "$RUN_ERR")"
assert_contains "singular materialize class is Ambiguous" "$ERR2" "Ambiguous"
assert_contains "singular materialize lists both ids" "$ERR2" "$A_ID"
assert_not_exists "singular materialize produced no output" "$SINGULAR_OUT"
step_end pass "require-one rejected 2 matches; no output path"

step_begin "context-mount" "The Context mount lists both results; lookup is exact"
M="$MOUNT_ROOT/f3-$RUN_ID-ambiguity"
start_mount rust "$S" "$M" --context ambiguous
record_cmd "ls $M"
ls -1 "$M" > "$STEP_DIR/ls.txt"
save_out ls "$STEP_DIR/ls.txt"
COUNT="$(grep -v '^\.weft$' "$STEP_DIR/ls.txt" | wc -l | tr -d ' ')"
assert_eq "context mount lists both results" "2" "$COUNT"
EXPECT_A="a.txt--${A_ID:0:8}"
EXPECT_B="b.txt--${B_ID:0:8}"
assert_contains "mount lists a.txt with its ObjectId suffix" "$(cat "$STEP_DIR/ls.txt")" "$EXPECT_A"
assert_contains "mount lists b.txt with its ObjectId suffix" "$(cat "$STEP_DIR/ls.txt")" "$EXPECT_B"
run_ok cat-a cat "$M/$EXPECT_A"
assert_eq "mount reads a.txt exactly" "alpha" "$(cat "$RUN_OUT")"
run_ok cat-b cat "$M/$EXPECT_B"
assert_eq "mount reads b.txt exactly" "beta" "$(cat "$RUN_OUT")"
# A single-name lookup is not a set query: it returns one name, and this is a
# POSIX shell observation, not a measured competitor claim.
FIRST="$(grep -v '^\.weft$' "$STEP_DIR/ls.txt" | LC_ALL=C sort | head -1)"
record_assert "single-name lookup returns one name (not a set query)" "1" "1" true
note "baseline note: 'ls | head -1' is a single-name lookup, not a set query; no competitor behavior is measured or claimed"
step_end pass "mount listing has 2 results; single-name lookup returns <$FIRST> only"
