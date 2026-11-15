FROM debian:bookworm-slim

# BeeGFS court image (T036). The upstream Debian-12 repository is added inside
# the image because `apt-get` cannot run in the court's own cap-drop=ALL
# containers. `trusted=yes` is used because the upstream repository's signing
# key is not published at a stable URL; the receipt records the exact
# `Release` file digest, the package versions installed, and the image id, so
# the build is identifiable even without a signature.
#
# The server daemons (mgmtd/meta/storage) are ordinary userspace processes.
# The BeeGFS *client* is a kernel module (`beegfs.ko`), which is what this
# court is expected to be blocked on; `beegfs-client` and `beegfs-utils` are
# installed so the failure is the real one and its text can be recorded.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl gnupg coreutils procps util-linux kmod \
 && echo 'deb [arch=amd64 trusted=yes] https://www.beegfs.io/release/beegfs_7.4.6 bookworm non-free' \
      > /etc/apt/sources.list.d/beegfs.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      beegfs-utils beegfs-common beegfs-mgmtd beegfs-meta beegfs-storage \
      beegfs-client \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /data/mgmtd /data/meta /data/storage /mnt/beegfs /var/log/beegfs
WORKDIR /work
