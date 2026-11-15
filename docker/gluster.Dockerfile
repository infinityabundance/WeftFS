FROM debian:bookworm-slim

# GlusterFS court image (T036). The packages are baked in at build time because
# `apt-get` cannot run inside the court's own containers: every measured
# container is started with `--cap-drop=ALL`, so `_apt` cannot drop privileges
# and the package manager refuses. Building the image is therefore part of the
# court, and the image id and package versions are recorded in the receipt.
#
# Both the server (glusterd + brick) and the FUSE client live in this one image:
# the court runs one server container and two client containers from the same
# pinned build, so server and clients are the same GlusterFS version by
# construction.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates coreutils findutils procps util-linux \
      glusterfs-server glusterfs-client \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /mnt/weft-gl /data/brick1 /var/log/glusterfs /run/gluster
WORKDIR /work
