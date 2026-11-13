#!/usr/bin/env bash
# Third-party conformance distribution court (subphase R13).
#
#   scripts/conformance_bundle_court.sh
#
# Builds `bench/bundles/conformance-<version>.tar.zst` inside the fuse court,
# runs the reference Rust adapter from an extracted copy only, and verifies the
# bundle with `scripts/verify-receipts.sh` twice: inside the court (the
# packaging script does it) and again from the host with `--repo-root .`, so
# the receipt's revisions must resolve to real commits in the mounted
# repositories.
#
# Configuration. The packaging script has no costly knob, so this is its full
# default configuration: all manifest vectors, the Rust adapter from the
# extracted copy, checksums, unlisted-file rejection and revision checks. The
# one thing that is deferred is the independent Python second opinion, because
# the fuse court has no blake3; `scripts/conformance_python_court.sh` runs it in
# the python court and merges its result into the same receipt. The receipt's
# `configuration` block records that split explicitly.
#
# The independent Python run, the extracted-only Rust run and the checksum
# list are the evidence; a skip is never a pass.
#
# An environment refusal prints exactly one `conformance_bundle: skip <reason>`
# line and exits 78. `scripts/verify-all.sh` records that as `skip`, never as a
# pass.
set -Eeuo pipefail

prefix=conformance_bundle
skip() { echo "$prefix: skip $1" >&2; exit 78; }
fail() { echo "$prefix: FAIL $1" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
command -v python3 >/dev/null 2>&1 || skip "python3 is required to inspect the receipt"
command -v tar >/dev/null 2>&1 || skip "tar is required to verify the bundle"
image="weft/fuse-court:1"
docker image inspect "$image" >/dev/null 2>&1 \
  || skip "court image $image is not present (build it with scripts/docker-court.sh fuse true)"

# ---- committed corpus ------------------------------------------------------
# The court image has no git, so `package-conformance.sh` cannot prove that the
# corpus it copies is committed. The same gate runs here, on the host, against
# the working tree that is mounted into the court: a vector that exists only in
# this tree would be absent from a clone and the bundle's revisions would not
# contain the corpus it ships.
# shellcheck source=scripts/lib-tracked.sh
source "$repo_root/scripts/lib-tracked.sh"
untracked_corpus=0
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  weft_require_tracked "$rel" "conformance_bundle" || untracked_corpus=$((untracked_corpus + 1))
done < <(find conformance WeftCodec/conformance/v1 -type f -print | LC_ALL=C sort -u)
if [ "$untracked_corpus" -ne 0 ]; then
  fail "$untracked_corpus untracked conformance file(s); commit the corpus before packaging"
fi
echo "$prefix: committed corpus ok (conformance/ and WeftCodec/conformance/v1)" >&2

stamp="${WEFT_STAMP:-${WEFT_COMPAT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}}"
version="$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || fail "cannot read version from Cargo.toml"
bundle="bench/bundles/conformance-$version.tar.zst"
receipt="bench/receipts/conformance-bundle-$stamp.json"
verify_log="bench/receipts/conformance-bundle-$stamp.verify.log"

mkdir -p bench/receipts bench/bundles
chmod a+rwX bench/receipts bench/bundles 2>/dev/null || true

command_text="scripts/docker-court.sh fuse \"cd /work/WeftCodec && cargo build --release --quiet --bin weftcodec && cd /work && WEFT_STAMP=$stamp WEFT_CONFORMANCE_MODE=aggregate bash scripts/package-conformance.sh\""
echo "$prefix: configuration: full packaging run (all manifest vectors), Rust adapter from the extracted copy only, Python second opinion deferred to conformance_python; stamp=$stamp" >&2

WEFT_STAMP="$stamp" WEFT_CONFORMANCE_MODE=aggregate \
  scripts/docker-court.sh fuse \
  "cd /work/WeftCodec && cargo build --release --quiet --bin weftcodec && cd /work && WEFT_STAMP=$stamp WEFT_CONFORMANCE_MODE=aggregate bash scripts/package-conformance.sh" \
  || fail "packaging run failed"

[[ -f "$receipt" ]] || fail "receipt $receipt was not written"
[[ -f "$bundle" ]] || fail "bundle $bundle was not written"

# Host-side verification, independent of the producer's in-court check.
if ! scripts/verify-receipts.sh "$bundle" --repo-root . > "$verify_log" 2>&1; then
  cat "$verify_log" >&2
  fail "host-side verify-receipts failed; see $verify_log"
fi

python3 - "$receipt" "$bundle" "$stamp" "$version" "$command_text" "$verify_log" <<'PY'
import json, os, sys

receipt_path, bundle, stamp, version, command_text, verify_log = sys.argv[1:7]
with open(receipt_path, encoding="utf-8") as handle:
    record = json.load(handle)

def check(condition, name):
    if not condition:
        print("conformance_bundle: FAIL receipt check %s" % name, file=sys.stderr)
        sys.exit(1)

actual_sha = __import__("hashlib").sha256(open(bundle, "rb").read()).hexdigest()
check(record.get("schema") == "weft.conformance.bundle.v1", "schema")
check(os.path.basename(bundle) == os.path.basename(record.get("bundle", "")), "bundle_path")
check(actual_sha == record.get("bundle_sha256"), "bundle_sha256")
check(record.get("verify_receipts") == "pass", "verify_receipts_in_court")
check(record.get("extracted_only") is True, "extracted_only")
rust = record.get("runs", {}).get("rust")
check(rust is not None and rust.get("fail") == 0 and rust.get("result") == "pass", "reference_rust_passes")
check(record.get("checks", {}).get("no_skip_is_a_pass") is True, "no_skip_is_a_pass")
check(record.get("python_second_opinion", {}).get("status") == "deferred", "python_deferred")

record.setdefault("checks", {})["host_verify_receipts_passes"] = True
courts = record.setdefault("courts", {})
courts["conformance_bundle"] = {
    "name": "conformance_bundle",
    "script": "scripts/conformance_bundle_court.sh",
    "stamp": stamp,
    "court": "fuse",
    "command": command_text,
    "bundle": bundle,
    "bundle_sha256": actual_sha,
    "host_verify_receipts": "pass",
    "host_verify_log": verify_log,
}
record["configuration"] = {
    **record.get("configuration", {}),
    "mode": "aggregate",
    "reduced": False,
    "vectors": record.get("manifest_entries"),
    "bundles_dir": "bench/bundles",
    "note": ("the packaging script has no cost knob; the aggregate runs its full "
             "default configuration and defers only the Python second opinion, "
             "which scripts/conformance_python_court.sh merges into this receipt"),
}
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(record, handle, indent=2, sort_keys=True)
    handle.write("\n")

print("conformance_bundle: bundle=%s sha256=%s bytes=%s files=%s" % (
    bundle, actual_sha, record.get("bundle_bytes"), record.get("bundle_file_count")))
print("conformance_bundle: rust %s pass / %s fail / %s skip (extracted copy only)" % (
    rust.get("pass"), rust.get("fail"), rust.get("skip")))
print("conformance_bundle: verify-receipts pass (in court and on host), receipt=%s" % receipt_path)
PY
