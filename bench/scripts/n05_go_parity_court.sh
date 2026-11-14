#!/usr/bin/env bash
# N05 court: the Go two-stage selector and the published parity receipt.
#
# Runs inside the fuse court (it needs Go, cargo is not required):
#   scripts/docker-court.sh fuse "bash bench/scripts/n05_go_parity_court.sh"
#
# The store test proves two-stage equals exhaustive on synthetic tensors and
# that q8_0 falls back. The receipt assertions re-check the published result:
# the Go selector agreed with Go exhaustive on every identity field, the
# improvement was real, and the divergences are listed rather than hidden.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_N05_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/n05-go-parity-20261114T203000Z.json"
out="bench/receipts/n05-go-parity-court-$stamp.json"

if ! command -v go >/dev/null 2>&1; then
  echo "n05_go_parity: skip Go toolchain unavailable" >&2
  exit 78
fi
if [[ ! -f "$receipt" ]]; then
  echo "n05_go_parity: skip missing $receipt" >&2
  exit 78
fi

echo "== Go selector tests =="
cd WeftFS-Go
gofmt -l . | tee /tmp/n05-gofmt.txt
if [[ -s /tmp/n05-gofmt.txt ]]; then
  echo "n05_go_parity: gofmt found unformatted files" >&2
  exit 1
fi
go vet ./...
CGO_ENABLED=0 go build ./...
go test ./store -run 'TwoStage' -count=1
cd /work

echo "== N05 receipt assertions =="
python3 - "$receipt" "$out" "$stamp" <<'PY'
import json
import sys

receipt_path, out_path, stamp = sys.argv[1:]
receipt = json.load(open(receipt_path))
selector = receipt["go_selector"]
assert selector["two_stage_matches_go_exhaustive"] is True
assert all(count == 0 for count in selector["differences"].values()), selector["differences"]
assert selector["speedup"] > 1.0
assert selector["errors"] == 0
assert selector["footprint_bytes"] > 0
agreement = receipt["agreement"]
assert agreement["divergent_tensors"] == len(receipt["divergences"])
assert agreement["mode9_both"]["bytes_equal"] == agreement["mode9_both"]["tensors"]
assert agreement["raw_both"]["bytes_equal"] == agreement["raw_both"]["tensors"]
assert agreement["dtype_shape_logical_root_equal"] is True
assert receipt["cross_verify"]["rust_reads_go_store"].startswith("ok=true")
assert receipt["cross_verify"]["go_reads_rust_store"].startswith("ok=true")
assert receipt["gates"]["content_ids_and_choices_agree"]["met"] is False
record = {
    "schema": "weft.n05.court.v1",
    "stamp": stamp,
    "receipt": receipt_path,
    "result": "pass",
    "go_speedup": selector["speedup"],
    "divergent_tensors": agreement["divergent_tensors"],
    "note": "the selector is exact against Go exhaustive; cross-language choice parity is published as not met",
}
with open(out_path, "w") as handle:
    json.dump(record, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("n05_go_parity: pass (%s)" % out_path)
PY
