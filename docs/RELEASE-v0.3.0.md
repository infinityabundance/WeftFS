# WeftFS v0.3.0 — storage-rig evidence release

Research preview. This release adds storage-fault and filesystem-baseline
evidence; it does not change the v0.2 codec/store formats.

## Container images

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.3.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.3.0
docker pull ghcr.io/infinityabundance/weftfs:v0.3.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.3.0
```

`latest` is also published. The images are multi-stage builds of the same
commits; `weftfs`/`weftfs-go` need `--device /dev/fuse --cap-add SYS_ADMIN`
only for mounting. See `docs/containers.md`. All four were pulled anonymously
and smoke-tested.

## Added

- **Whole-disk ZFS baseline**: real `zpool create -f tank /dev/vdb` in the QEMU
  guest, with a guest udevd; GPT/partition creation is owned by ZFS, not a
  loop/file vdev. Receipt `bench-fs-cold-20261104T071922Z.json`.
- **Btrfs baseline** on the same raw disk, matched fio matrix with ext4/XFS/ZFS.
- **Block-layer lying-disk / failed-flush court**:
  - dm-flakey silent write drop: 5/5 lost acknowledged commits.
  - dm-flakey EIO: 5/5 old state, 0 loss, 5 commit failures.
  - snapshot total drop: 5/5 recovered generation 2, losing all acked commits.
  - 0 partial states across 30 iterations.
  - QEMU blkdebug write/flush-error modes arm at device open and block ext4
    mount before the guest writes; recorded as a precise harness limitation,
    not a pass.
- **Host-cache probe**: `/proc/sys/vm/drop_caches` is read-only in the
  container; no host cache was dropped. Per-file `fadvise(DONTNEED)` succeeded
  and is labelled per-file only.
- Receipts: `blkdebug-20261104T072008Z.json`,
  `bench-fs-cold-20261104T071922Z.json`,
  `hostcache-probe-20261104T072314Z.json`.

## Still not claimed

- Physical media power loss, host page-cache loss, selective real-disk
  write-drop, and flush-error below a mounted ext4.
- True energy/joules (RAPL unreadable).
- Kernel-7.3 same-inode direct-I/O and mmap-then-write EIO.
- Rust `O_TMPFILE`/daemon flock binding limits; Go physical-space PUNCH_HOLE;
  ioctl.
- Buck2 plugin/REAPI integration; AI modes beyond TENSOR, TOKEN_SEQUENCE,
  QUANTIZED_TENSOR; real-time agent aging.
- TLA+ checks the abstract commit/recovery model, not implementation
  refinement.

No core code changed in this release; the v0.2 verification receipt remains the
code-correctness evidence. Commands and caveats are in
`bench/RESULTS-comparative-v1.md` and `docs/STATUS.md`.
