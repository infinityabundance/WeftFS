# Container images

Published to GitHub Container Registry under `ghcr.io/infinityabundance`.

**Repository ownership:** each package is linked to its own repository and
appears in that repository's Packages tab:

- `weftcodec` → https://github.com/infinityabundance/WeftCodec/packages
- `weftcodec-go` → https://github.com/infinityabundance/WeftCodec-Go/packages
- `weftfs` → https://github.com/infinityabundance/WeftFS/packages
- `weftfs-go` → https://github.com/infinityabundance/WeftFS-Go/packages

Each repository has a `container.yml` workflow that publishes its canonical
image on a `v*` tag or manual dispatch.

| Image | Binary | Entry point |
|---|---|---|
| `ghcr.io/infinityabundance/weftcodec` | `weftcodec` (Rust) | codec CLI |
| `ghcr.io/infinityabundance/weftcodec-go` | `weftcodec` (Go) | codec CLI |
| `ghcr.io/infinityabundance/weftfs` | `weftfs` (Rust) | store + FUSE CLI |
| `ghcr.io/infinityabundance/weftfs-go` | `weftfs` (Go) | store + FUSE CLI |

Tags: `v1.2.0` and `latest` (release workflow). Images are built from the program
home commit and the pinned submodule commits.

## Pull and run

```sh
docker pull ghcr.io/infinityabundance/weftfs-go:v1.2.0
docker run --rm ghcr.io/infinityabundance/weftcodec:v1.2.0 --help

# Store operations need a writable volume:
docker run --rm -v "$PWD/store:/store" \
  ghcr.io/infinityabundance/weftfs:v1.2.0 --store /store init

# Mounting a FUSE view requires the device and capability:
docker run --rm -it --device /dev/fuse --cap-add SYS_ADMIN \
  -v "$PWD/store:/store" -v "$PWD/mnt:/mnt" \
  ghcr.io/infinityabundance/weftfs:v1.2.0 mount --store /store --mountpoint /mnt --foreground
```

Use `--cap-add SYS_ADMIN` only for mounts; codec/store-only operations do not
need it. Every image is a normal OCI image and can be run with the same
`--memory`, `--pids-limit`, `--cpus` limits used by the test courts.

## Verification

The image release is a packaging layer over the verified release. Binaries are
smoke-tested in the pipeline; the code-level evidence is the committed
`bench/receipts/verify-*.json` for the same source revision. The workflow is
`.github/workflows/containers.yml` and is triggered by a `v*` tag or manually.

If a package is private, an authenticated `docker login ghcr.io` is required;
the workflow attempts to set visibility to public.

## Rust crates

The same release publishes the libraries on crates.io:

```toml
weftcodec = "1.2.0"
weftfs = "1.2.0"
```

`weftcodec` is the codec/framing library and CLI; `weftfs` is the store plus
FUSE frontend (the `fuse` feature enables mounting). See
`docs/RELEASE-v1.2.0.md`, the previous `docs/RELEASE-v1.1.0.md`,
`docs/RELEASE-v1.0.0.md`,
`docs/RELEASE-v0.9.0.md`, and `docs/RELEASE-v0.8.0.md`. Image tags
and crate versions carry no extra claim
beyond the `bench/receipts/verify-*.json` for the same source revision.

Publishing is repeatable and runs inside the same hard-limited rust court:

```sh
scripts/publish-crates.sh --dry-run   # verify packaging, publish nothing
scripts/publish-crates.sh             # weftcodec, wait for the index, weftfs
```

The script reads `~/.cargo/credentials.toml`, copies it to `.docker-cache/`
with mode 0644 only for the run, mounts it read-only at
`/cargo/credentials.toml`, and deletes the copy on exit. The token is never
written into the repository, printed, or committed. `weftfs` is published only
after `weftcodec` at the same version is visible in the crates.io index.
