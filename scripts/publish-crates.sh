#!/usr/bin/env bash
# Publish the Rust crates to crates.io inside the OOM-protected rust court.
#
#   scripts/publish-crates.sh [--dry-run]
#
# Order matters: `weftfs` depends on `weftcodec = "<version>"` from the
# registry, so `weftcodec` must be published and visible in the index first.
#
# Credentials: the script reads the host's ~/.cargo/credentials.toml, copies it
# to `.docker-cache/` with mode 0644 only for the duration of the run (Docker
# cannot mount a 0600 host file as a non-root container user), mounts it
# read-only at /cargo/credentials.toml, and removes the copy on exit. The token
# is never written into the repository and never printed.
#
# Limits are the court defaults and are not configurable here: the publish
# build runs under the same memory/swap/pids/CPU bounds as every other court.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

dry_run=0
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run=1
fi

image="${WEFT_COURT_IMAGE:-weft/rust-court:1}"
cache="$root/.docker-cache"
creds_src="$HOME/.cargo/credentials.toml"
creds_tmp="$cache/crates-credentials.toml"

if [[ ! -f "$creds_src" ]]; then
  echo "publish-crates: missing $creds_src (run 'cargo login' once on the host)" >&2
  exit 66
fi
mkdir -p "$cache"
umask 077
cp "$creds_src" "$creds_tmp"
chmod 0644 "$creds_tmp"
cleanup() { rm -f "$creds_tmp"; }
trap cleanup EXIT INT TERM

codec_version="$(sed -n 's/^version = "\(.*\)"/\1/p' WeftCodec/Cargo.toml | head -1)"
fs_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$codec_version" || -z "$fs_version" ]]; then
  echo "publish-crates: cannot read crate versions" >&2
  exit 65
fi
dep_version="$(sed -n 's/^weftcodec = { version = "\([^"]*\)".*/\1/p' Cargo.toml)"
if [[ "$dep_version" != "$codec_version" ]]; then
  echo "publish-crates: weftfs depends on weftcodec $dep_version but WeftCodec is $codec_version" >&2
  exit 65
fi

court() {
  docker run --rm --init \
    --memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4 \
    --ulimit nofile=65536:65536 \
    --security-opt no-new-privileges --cap-drop=ALL \
    --tmpfs /tmp:rw,size=1g,nosuid,nodev \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/work/.docker-cache/target-rust \
    -e CARGO_NET_RETRY=3 \
    -e CARGO_TERM_COLOR=never \
    -v "$root":/work \
    -v "$cache/cargo-registry":/cargo/registry \
    -v "$cache/cargo-git":/cargo/git \
    -v "$creds_tmp":/cargo/credentials.toml:ro \
    -w /work \
    "$image" bash -c "$1"
}

echo "publish-crates: weftcodec $codec_version"
publish_args="--locked"
if [[ "$dry_run" -eq 1 ]]; then
  publish_args="$publish_args --dry-run"
fi
court "cd /work/WeftCodec && cargo publish $publish_args"

if [[ "$dry_run" -eq 1 ]]; then
  if curl -fsS -H 'User-Agent: weftfs-publish-crates' \
      "https://crates.io/api/v1/crates/weftcodec/$codec_version" >/dev/null 2>&1; then
    court "cd /work && cargo publish $publish_args"
  else
    echo "publish-crates: weftcodec $codec_version is not in the index, so the"
    echo "publish-crates: weftfs dry run cannot resolve it yet; the real run"
    echo "publish-crates: publishes weftcodec, waits, then publishes weftfs"
  fi
  echo "publish-crates: dry run complete (nothing published)"
  exit 0
fi

echo "publish-crates: waiting for weftcodec $codec_version in the crates.io index"
ok=0
for _ in $(seq 1 60); do
  if curl -fsS -H 'User-Agent: weftfs-publish-crates' \
      "https://crates.io/api/v1/crates/weftcodec/$codec_version" >/dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 5
done
if [[ "$ok" -ne 1 ]]; then
  echo "publish-crates: weftcodec $codec_version did not appear in the index; not publishing weftfs" >&2
  exit 70
fi

echo "publish-crates: weftfs $fs_version"
court "cd /work && cargo publish $publish_args"

echo "publish-crates: done"
