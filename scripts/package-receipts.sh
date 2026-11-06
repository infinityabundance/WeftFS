#!/usr/bin/env bash
# F5.4 / B3 — one-command receipt bundle.
#
# Runs on the host (it only orchestrates git, docker inspection, hashing, and
# tar; it never builds, tests, or mounts a filesystem). The resulting bundle
# is self-describing and independently verifiable:
#
#   scripts/package-receipts.sh
#   scripts/verify-receipts.sh bench/bundles/receipts-<stamp>.tar.zst
#
# Contents: manifest.json, CHECKSUMS.sha256, REVISIONS.txt, revisions/, receipts/
# (raw receipts and logs), corpus/ (conformance vectors), harness/ (the F5
# scripts and court Dockerfiles), and dmesg.txt. No research/ tree, no secrets,
# and no host paths outside the bundle. Host paths found in copied text logs are
# replaced with <repo-root>/<host-home>; the manifest records the original
# sha256 next to the bundled sha256.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ALLOW_DIRTY=0
SIGN_KEY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --allow-dirty) ALLOW_DIRTY=1; shift ;;
    --sign-key) SIGN_KEY="${2:-}"; shift 2 ;;
    --sign-key=*) SIGN_KEY="${1#--sign-key=}"; shift ;;
    *) echo "usage: $0 [--allow-dirty] [--sign-key FILE]" >&2; exit 64 ;;
  esac
done
if [ -n "$SIGN_KEY" ] && [ ! -f "$SIGN_KEY" ]; then
  echo "signing key not found: $SIGN_KEY" >&2
  exit 66
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
GENERATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
BUNDLES="$ROOT/bench/bundles"
# The staging directory must be inside the repository because the signing
# helper runs in the Docker court, which mounts the repository at /work. It is
# under the gitignored .docker-cache tree and never enters the bundle.
CACHE="$ROOT/.docker-cache"
mkdir -p "$CACHE"
STAGE="$(mktemp -d "$CACHE/weft-receipts-$STAMP.XXXXXX")"
STAGE_REL="${STAGE#"$ROOT/"}"
TSV="$STAGE/.manifest.tsv"
cleanup() {
  rm -rf "$STAGE"
}
trap cleanup EXIT

mkdir -p "$BUNDLES" "$STAGE/receipts" "$STAGE/corpus" "$STAGE/harness" "$STAGE/revisions"
: > "$TSV"

log() { printf '%s\n' "$*" >&2; }
sha256_file() { sha256sum "$1" | awk '{print $1}'; }
file_size() { stat -c %s "$1"; }
meta() { printf 'META\t%s\t%s\n' "$1" "$2" >> "$TSV"; }
note() { printf 'NOTE\t%s\n' "$1" >> "$TSV"; }

is_text() {
  case "$1" in
    *.log|*.json|*.tsv|*.txt|*.md|*.sh|*.go|*.rs|*.toml|*.yaml|*.yml|*.Dockerfile|*Dockerfile)
      return 0 ;;
    *) return 1 ;;
  esac
}

# copy_file <category> <source> <bundle-relative-destination>
# Records the original and bundled sha256; sanitizes host paths in text files.
copy_file() {
  local category="$1" src="$2" dst="$3"
  local source_sha size sanitized=false bundled_sha
  source_sha="$(sha256_file "$src")"
  mkdir -p "$STAGE/$(dirname "$dst")"
  if is_text "$src"; then
    sed -e "s|$ROOT|<repo-root>|g" -e "s|$HOME|<host-home>|g" "$src" > "$STAGE/$dst"
  else
    cp "$src" "$STAGE/$dst"
  fi
  size="$(file_size "$STAGE/$dst")"
  bundled_sha="$(sha256_file "$STAGE/$dst")"
  if [ "$bundled_sha" != "$source_sha" ]; then
    sanitized=true
  fi
  printf 'FILE\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$category" "$dst" "$bundled_sha" "$size" "$source_sha" "$sanitized" >> "$TSV"
  LAST_SHA="$bundled_sha"
  LAST_SIZE="$size"
}

# ---- clean revision gate -------------------------------------------------
REPOS=("WeftFS:." "WeftCodec:WeftCodec" "WeftCodec-Go:WeftCodec-Go" "WeftFS-Go:WeftFS-Go")
for entry in "${REPOS[@]}"; do
  name="${entry%%:*}"; path="${entry#*:}"
  git -C "$ROOT/$path" rev-parse --verify HEAD >/dev/null 2>&1 || {
    echo "repo $name at $path has no HEAD" >&2; exit 1;
  }
  status="$(git -C "$ROOT/$path" status --porcelain)"
  if [ -n "$status" ] && [ "$ALLOW_DIRTY" -ne 1 ]; then
    echo "repo $name is dirty; commit or pass --allow-dirty:" >&2
    printf '%s\n' "$status" >&2
    exit 1
  fi
done

# ---- meta ----------------------------------------------------------------
meta generated_at "$GENERATED_AT"
meta stamp "$STAMP"
meta kernel "$(uname -r)"
meta uname "$(uname -srm)"
meta arch "$(uname -m)"
meta distribution "$(sed -n 's/^PRETTY_NAME="\(.*\)"$/\1/p' /etc/os-release 2>/dev/null | head -1)"
meta docker_client "$(docker version --format '{{.Client.Version}}' 2>/dev/null || echo unknown)"
meta docker_server "$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"
meta memory "$(sed -n 's/^WEFT_MEMORY="${WEFT_MEMORY:-\(.*\)}"$/\1/p' scripts/docker-court.sh | head -1)"
meta memory_swap "$(sed -n 's/^WEFT_MEMORY="${WEFT_MEMORY:-\(.*\)}"$/\1/p' scripts/docker-court.sh | head -1)"
meta cpus "$(sed -n 's/^WEFT_CPUS="${WEFT_CPUS:-\(.*\)}"$/\1/p' scripts/docker-court.sh | head -1)"
meta pids "$(sed -n 's/^WEFT_PIDS="${WEFT_PIDS:-\(.*\)}"$/\1/p' scripts/docker-court.sh | head -1)"
meta tmpfs_tmp "1g"
note "Court limits are the docker-court.sh defaults; each receipt records the actual run limits."
note "The F5 crash court is a process-fault proxy, not physical power loss."
note "Text files are sanitized for host paths; original sha256 is in source_sha256."
note "Signed bundles add receipt-manifest.wcb, signature, and public_key; unsigned bundles add UNSIGNED.txt."

# ---- repositories and revisions ------------------------------------------
: > "$STAGE/REVISIONS.txt"
for entry in "${REPOS[@]}"; do
  name="${entry%%:*}"; path="${entry#*:}"
  revision="$(git -C "$ROOT/$path" rev-parse HEAD)"
  branch="$(git -C "$ROOT/$path" rev-parse --abbrev-ref HEAD)"
  changed="$(git -C "$ROOT/$path" status --porcelain | wc -l | tr -d ' ')"
  worktree=clean; [ "$changed" -ne 0 ] && worktree=dirty
  printf '%s %s %s %s %s\n' "$name" "$revision" "$branch" "$worktree" "$changed" >> "$STAGE/REVISIONS.txt"
  printf '%s\n' "$revision" > "$STAGE/revisions/$name.txt"
  rev_sha="$(sha256_file "$STAGE/revisions/$name.txt")"
  rev_size="$(file_size "$STAGE/revisions/$name.txt")"
  printf 'FILE\trevision\trevisions/%s.txt\t%s\t%s\t%s\tfalse\n' \
    "$name" "$rev_sha" "$rev_size" "$rev_sha" >> "$TSV"
  printf 'REPO\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$path" "$branch" "$revision" "$worktree" "$changed" >> "$TSV"
done
rev_sha="$(sha256_file "$STAGE/REVISIONS.txt")"
rev_size="$(file_size "$STAGE/REVISIONS.txt")"
printf 'FILE\trevision\tREVISIONS.txt\t%s\t%s\t%s\tfalse\n' \
  "$rev_sha" "$rev_size" "$rev_sha" >> "$TSV"

# ---- courts --------------------------------------------------------------
for kind in rust go fuse bench; do
  image="weft/${kind}-court:1"
  dockerfile="docker/${kind}.Dockerfile"
  image_id="$(docker image inspect --format '{{.Id}}' "$image" 2>/dev/null || echo not-built)"
  if [ -f "$ROOT/$dockerfile" ]; then
    dockerfile_sha="$(sha256_file "$ROOT/$dockerfile")"
  else
    dockerfile_sha="missing"
  fi
  printf 'COURT\t%s\t%s\t%s\t%s\t%s\n' "$kind" "$image" "$image_id" "$dockerfile" "$dockerfile_sha" >> "$TSV"
done

# ---- receipts and raw logs -----------------------------------------------
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  copy_file receipt "bench/receipts/$rel" "receipts/$rel" >/dev/null
  printf 'RECEIPT\t%s\t%s\t%s\n' "receipts/$rel" "$LAST_SHA" "$LAST_SIZE" >> "$TSV"
  case "$rel" in
    *.log) printf 'LOG\t%s\n' "receipts/$rel" >> "$TSV" ;;
  esac
done < <(find bench/receipts -maxdepth 1 -type f -printf '%P\n' | LC_ALL=C sort)

# ---- corpus --------------------------------------------------------------
copy_tree() {
  local label="$1" src="$2" prefix="$3"
  [ -d "$src" ] || return 0
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    local dst="$prefix/$rel"
    copy_file corpus "$src/$rel" "$dst" >/dev/null
    printf 'CORPUS\t%s\t%s\t%s\t%s\n' "$label/$rel" "$dst" "$LAST_SHA" "$LAST_SIZE" >> "$TSV"
  done < <(find "$src" -type f -printf '%P\n' | LC_ALL=C sort)
}
copy_tree "WeftCodec/conformance/v1" "$ROOT/WeftCodec/conformance/v1" "corpus/WeftCodec/conformance/v1"
copy_tree "WeftCodec-Go/conformance/v1" "$ROOT/WeftCodec-Go/conformance/v1" "corpus/WeftCodec-Go/conformance/v1"
copy_tree "parent/conformance" "$ROOT/conformance" "corpus/parent/conformance"

# ---- harness -------------------------------------------------------------
HARNESS_FILES=(
  scripts/docker-court.sh
  scripts/package-receipts.sh
  scripts/verify-receipts.sh
  scripts/receipt-bundle-court.sh
  scripts/bundle_manifest.go
  src/receipt_bundle.rs
  src/bin/weft-receipt-bundle.rs
  docker/rust.Dockerfile
  docker/go.Dockerfile
  docker/fuse.Dockerfile
  docker/bench.Dockerfile
  bench/scripts/run_decoder_bomb_court.sh
  bench/scripts/decoder_bomb_court.go
  bench/scripts/posix_torture.sh
  bench/scripts/posix_torture.go
  bench/scripts/crash_torture.sh
  bench/CONTRACTS.md
  docs/STATUS.md
  README.md
)
for rel in "${HARNESS_FILES[@]}"; do
  [ -f "$ROOT/$rel" ] || continue
  copy_file harness "$ROOT/$rel" "harness/$rel" >/dev/null
done

# ---- dmesg ---------------------------------------------------------------
if dmesg > "$STAGE/dmesg.txt" 2>/dev/null; then
  dmesg_status="present"
else
  printf 'dmesg: unavailable (no CAP_SYSLOG)\n' > "$STAGE/dmesg.txt"
  dmesg_status="unavailable (no CAP_SYSLOG)"
fi
meta dmesg_status "$dmesg_status"
meta dmesg_path "dmesg.txt"
# Record dmesg after the file exists; copy_file would rewrite it, so record the
# already-written file directly.
dmesg_sha="$(sha256_file "$STAGE/dmesg.txt")"
dmesg_size="$(file_size "$STAGE/dmesg.txt")"
printf 'FILE\tdmesg\tdmesg.txt\t%s\t%s\t%s\tfalse\n' "$dmesg_sha" "$dmesg_size" "$dmesg_sha" >> "$TSV"

# ---- README --------------------------------------------------------------
cat > "$STAGE/README.txt" <<'EOF'
WeftFS receipt bundle
=====================

This archive contains machine-readable receipts and their raw logs for the
WeftFS program, plus the conformance corpus and the exact harness/court files.
Verify it without trusting the producer:

  scripts/verify-receipts.sh receipts-<stamp>.tar.zst --repo-root <repo-root>

Manifest: manifest.json   Checksums: CHECKSUMS.sha256   Revisions: REVISIONS.txt
Raw receipt logs: receipts/*.log   dmesg: dmesg.txt

Signed bundle
-------------
Every bundle carries receipt-manifest.wcb: a canonical WCB manifest of each
payload file (path, size, SHA-256, sorted) and the four git revisions the
harness ran at. A signed bundle (output name receipts-<stamp>.signed.tar.zst)
also carries `signature` and `public_key` (lowercase hex plus newline). The
signature is Ed25519 over H("weft/receipt-bundle/v1", manifest bytes).

  # verify against a pinned public key (64 hex characters)
  scripts/verify-receipts.sh receipts-<stamp>.signed.tar.zst \
      --public-key <hex> --repo-root <repo-root>

  # self-contained: trust the bundled public_key file
  scripts/verify-receipts.sh receipts-<stamp>.signed.tar.zst \
      --signature-from-bundle --repo-root <repo-root>

An unsigned bundle carries UNSIGNED.txt and fails closed if --public-key or
--signature-from-bundle is requested. Signing proves bundle integrity relative
to a key; it does not prove the measurements are true, and there is no
certificate authority, transparency log, or key distribution/rotation.

Honest scope notes
------------------
- The crash court uses WEFT_CRASH_PHASE to abort a process at a publication
  boundary. It is a process-fault proxy, not physical power loss; it does not
  drop caches, cut power, or simulate torn sectors.
- dmesg may be absent because the packaging user lacks CAP_SYSLOG; the exact
  status is recorded in manifest.json and dmesg.txt.
- Text files are copied with producer host paths replaced by <repo-root> and
  <host-home>. manifest.json records both source_sha256 and the bundled sha256.
- No research/ tree, secrets, or files outside this archive are required.
EOF
readme_sha="$(sha256_file "$STAGE/README.txt")"
readme_size="$(file_size "$STAGE/README.txt")"
printf 'FILE\tbundle\tREADME.txt\t%s\t%s\t%s\tfalse\n' "$readme_sha" "$readme_size" "$readme_sha" >> "$TSV"

# ---- manifest ------------------------------------------------------------
( cd /tmp && GO111MODULE=off go run "$ROOT/scripts/bundle_manifest.go" ) < "$TSV" > "$STAGE/manifest.json"
rm -f "$TSV"

# ---- canonical manifest and optional signature ---------------------------
SIGNED=0
if [ -n "$SIGN_KEY" ]; then
  # The seed is streamed on stdin; it is never copied into the repository or
  # the Docker cache (whose directories are intentionally world-writable for
  # the court). The helper reads `--sign-key -` from /dev/stdin.
  scripts/receipt-bundle-court.sh create \
    --root "$STAGE_REL" \
    --manifest "$STAGE_REL/receipt-manifest.wcb" \
    --sign-key - \
    --signature "$STAGE_REL/signature" \
    --public-key "$STAGE_REL/public_key" \
    --generated-at "$GENERATED_AT" \
    --stamp "$STAMP" < "$SIGN_KEY"
  SIGNED=1
else
  cat > "$STAGE/UNSIGNED.txt" <<'EOF'
This bundle is UNSIGNED.

scripts/package-receipts.sh was run without --sign-key. receipt-manifest.wcb is
present and covers every payload file, but no signature or public_key file was
written. A verifier that requires a key (--public-key or
--signature-from-bundle) must reject this bundle.
EOF
  scripts/receipt-bundle-court.sh create \
    --root "$STAGE_REL" \
    --manifest "$STAGE_REL/receipt-manifest.wcb" \
    --generated-at "$GENERATED_AT" \
    --stamp "$STAMP"
fi

# ---- checksums -----------------------------------------------------------
( cd "$STAGE" && find . -type f ! -name CHECKSUMS.sha256 -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum > CHECKSUMS.sha256 )

# ---- safety scans --------------------------------------------------------
if grep -RIl -e "$ROOT" -e "$HOME" "$STAGE" >/dev/null 2>&1; then
  echo "refusing to package: producer host path remains in the bundle" >&2
  grep -RIl -e "$ROOT" -e "$HOME" "$STAGE" >&2 || true
  exit 1
fi
if grep -RIlE 'BEGIN (RSA|OPENSSH|EC|DSA) PRIVATE KEY|AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|sk-[A-Za-z0-9]{20,}' "$STAGE" >/dev/null 2>&1; then
  echo "refusing to package: possible secret detected" >&2
  grep -RIlE 'BEGIN (RSA|OPENSSH|EC|DSA) PRIVATE KEY|AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|sk-[A-Za-z0-9]{20,}' "$STAGE" >&2 || true
  exit 1
fi
if find "$STAGE" -type f -path '*research*' | grep -q .; then
  echo "refusing to package: research path present" >&2
  exit 1
fi

# ---- bundle --------------------------------------------------------------
if [ "$SIGNED" -eq 1 ]; then
  OUT="$BUNDLES/receipts-$STAMP.signed.tar.zst"
else
  OUT="$BUNDLES/receipts-$STAMP.tar.zst"
fi
tar --zstd -cf "$OUT" -C "$STAGE" .
BUNDLE_SHA="$(sha256_file "$OUT")"
BUNDLE_SIZE="$(file_size "$OUT")"
printf '%s  %s\n' "$BUNDLE_SHA" "$(basename "$OUT")" > "$OUT.sha256"

echo "bundle: $OUT"
echo "signed: $([ "$SIGNED" -eq 1 ] && echo yes || echo no)"
echo "sha256: $BUNDLE_SHA"
echo "bytes: $BUNDLE_SIZE"
echo "manifest: $STAGE/receipt-manifest.wcb (not retained; inside the bundle)"
if [ "$SIGNED" -eq 1 ]; then
  echo "public_key: $(tr -d '\n' < "$STAGE/public_key")"
  echo "fingerprint: $(scripts/receipt-bundle-court.sh fingerprint --public-key "$(tr -d '\n' < "$STAGE/public_key")")"
  echo "verify: scripts/verify-receipts.sh $OUT --signature-from-bundle"
else
  echo "verify: scripts/verify-receipts.sh $OUT"
fi
