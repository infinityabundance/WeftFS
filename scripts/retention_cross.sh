#!/usr/bin/env bash
# Cross-language retention parity (subphase R6).
#
# Proves that a Rust-written `weft.retention.v1` binding is read by Go and a
# Go-written binding is read by Rust, byte-identically, for both the record
# format and the normative committed store layout.
#
# Usage: scripts/retention_cross.sh [--keep]
# Prints `PASS`/`FAIL` lines and `YES byte-identical` when every comparison
# holds. Exit 0 only when every check passes.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

keep="${WEFT_RETENTION_CROSS_KEEP:-0}"
for arg in "$@"; do
  case "$arg" in
    --keep) keep=1 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

stamp="${WEFT_RETENTION_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$repo_root/.docker-cache/retention-cross-$stamp"
mkdir -p "$work"
echo "retention_cross: work=$work stamp=$stamp"

run_rust() {
  scripts/docker-court.sh rust "cd /work && cargo run --release --quiet --example retention_probe -- $1"
}
run_go() {
  scripts/docker-court.sh go "cd /work/WeftFS-Go && go run ./cmd/retentionprobe $1"
}

failures=0
check() {
  local name="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    echo "PASS $name"
  else
    echo "FAIL $name expected=$expected actual=$actual" >&2
    failures=$((failures + 1))
  fi
}

# ---- record format -------------------------------------------------------
run_rust "write-file /work/.docker-cache/retention-cross-$stamp/rust.wcb" > "$work/rust-write.txt"
run_go "read-file /work/.docker-cache/retention-cross-$stamp/rust.wcb" > "$work/go-read.txt"
run_go "write-file /work/.docker-cache/retention-cross-$stamp/go.wcb" > "$work/go-write.txt"
run_rust "read-file /work/.docker-cache/retention-cross-$stamp/go.wcb" > "$work/rust-read.txt"

rust_sha="$(sed -n 's/^binding_sha256=//p' "$work/rust-write.txt")"
rust_id="$(sed -n 's/^binding=//p' "$work/rust-write.txt")"
go_read_sha="$(sed -n 's/^binding_sha256=//p' "$work/go-read.txt")"
go_read_id="$(sed -n 's/^binding=//p' "$work/go-read.txt")"
go_sha="$(sed -n 's/^binding_sha256=//p' "$work/go-write.txt")"
go_id="$(sed -n 's/^binding=//p' "$work/go-write.txt")"
rust_read_sha="$(sed -n 's/^binding_sha256=//p' "$work/rust-read.txt")"
rust_read_id="$(sed -n 's/^binding=//p' "$work/rust-read.txt")"

check "rust_record_to_go_sha" "$rust_sha" "$go_read_sha"
check "rust_record_to_go_id" "$rust_id" "$go_read_id"
check "go_record_to_rust_sha" "$go_sha" "$rust_read_sha"
check "go_record_to_rust_id" "$go_id" "$rust_read_id"
check "record_bytes_identical" "$(sha256sum "$work/rust.wcb" | cut -d' ' -f1)" \
  "$(sha256sum "$work/go.wcb" | cut -d' ' -f1)"
echo "YES byte-identical retention-record"

# ---- committed store layout ---------------------------------------------
descriptor="4242424242424242424242424242424242424242424242424242424242424242"
store="$repo_root/.docker-cache/retention-cross-store-$stamp"
rm -rf "$store"
run_rust "write-store /work/.docker-cache/retention-cross-store-$stamp" > "$work/rust-store-write.txt"
rust_store_sha="$(sed -n 's/^binding_sha256=//p' "$work/rust-store-write.txt")"
rust_store_id="$(sed -n 's/^binding=//p' "$work/rust-store-write.txt")"

run_go "read-store /work/.docker-cache/retention-cross-store-$stamp $descriptor" > "$work/go-store-read.txt"
go_store_sha="$(sed -n 's/^binding_sha256=//p' "$work/go-store-read.txt")"
go_store_id="$(sed -n 's/^binding=//p' "$work/go-store-read.txt")"
check "rust_store_to_go_sha" "$rust_store_sha" "$go_store_sha"
check "rust_store_to_go_id" "$rust_store_id" "$go_store_id"

run_go "rebind-store /work/.docker-cache/retention-cross-store-$stamp $descriptor" > "$work/go-store-rebind.txt"
go_rebind_sha="$(sed -n 's/^binding_sha256=//p' "$work/go-store-rebind.txt")"
go_rebind_id="$(sed -n 's/^binding=//p' "$work/go-store-rebind.txt")"
go_rebind_class="$(sed -n 's/^class=//p' "$work/go-store-rebind.txt")"

run_rust "read-store /work/.docker-cache/retention-cross-store-$stamp" > "$work/rust-store-read.txt"
rust_store_read_sha="$(sed -n 's/^binding_sha256=//p' "$work/rust-store-read.txt")"
rust_store_read_id="$(sed -n 's/^binding=//p' "$work/rust-store-read.txt")"
rust_store_read_class="$(sed -n 's/^class=//p' "$work/rust-store-read.txt")"
check "go_store_to_rust_sha" "$go_rebind_sha" "$rust_store_read_sha"
check "go_store_to_rust_id" "$go_rebind_id" "$rust_store_read_id"
check "go_store_to_rust_class" "ephemeral" "$rust_store_read_class"
check "go_rebind_class" "ephemeral" "$go_rebind_class"
echo "YES byte-identical retention-store-binding"

if [[ "$keep" != "1" ]]; then
  rm -rf "$store"
fi

if [[ "$failures" -ne 0 ]]; then
  echo "retention_cross: FAILED ($failures checks)" >&2
  exit 1
fi
echo "retention_cross: PASS (all checks)"
