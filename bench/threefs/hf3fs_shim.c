/*
 * A conformance implementation of the DeepSeek 3FS USRBIO ABI.
 *
 * This is NOT 3FS. It implements the exact declared symbols from
 * `src/lib/api/hf3fs_usrbio.h` upstream over real POSIX `pread`/`pwrite`, so the
 * WeftFS USRBIO transport can be exercised for struct layout, registration
 * lifetime, batched submission, completion reaping, short reads and per-request
 * errors without a 3FS deployment. `hf3fs_is_hf3fs` returns false and the
 * adapter records that, so a receipt can never present this as a 3FS run.
 *
 * No 3FS or upstream code is copied here: the struct layouts below are the
 * x86-64 C layout of the published declarations, and the `_Static_assert`s pin
 * them so a transcription error fails the build instead of corrupting memory.
 *
 * Build:
 *   cc -std=c11 -O2 -fPIC -shared -Wall -Wextra \
 *      -o libhf3fs_api_shared.so hf3fs_shim.c
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

typedef void *hf3fs_iov_handle;
typedef void *hf3fs_ior_handle;

struct hf3fs_iov {
  uint8_t *base;
  hf3fs_iov_handle iovh;
  char id[16];
  char mount_point[256];
  size_t size;
  size_t block_size;
  int numa;
};

struct hf3fs_ior {
  struct hf3fs_iov iov;
  hf3fs_ior_handle iorh;
  char mount_point[256];
  bool for_read;
  int io_depth;
  int priority;
  int timeout;
  uint64_t flags;
};

struct hf3fs_cqe {
  int32_t index;
  int32_t reserved;
  int64_t result;
  const void *userdata;
};

_Static_assert(sizeof(struct hf3fs_iov) == 312, "hf3fs_iov layout");
_Static_assert(sizeof(struct hf3fs_ior) == 600, "hf3fs_ior layout");
_Static_assert(sizeof(struct hf3fs_cqe) == 24, "hf3fs_cqe layout");
_Static_assert(offsetof(struct hf3fs_iov, base) == 0, "iov.base");
_Static_assert(offsetof(struct hf3fs_iov, iovh) == 8, "iov.iovh");
_Static_assert(offsetof(struct hf3fs_iov, id) == 16, "iov.id");
_Static_assert(offsetof(struct hf3fs_iov, mount_point) == 32, "iov.mount_point");
_Static_assert(offsetof(struct hf3fs_iov, size) == 288, "iov.size");
_Static_assert(offsetof(struct hf3fs_iov, block_size) == 296, "iov.block_size");
_Static_assert(offsetof(struct hf3fs_iov, numa) == 304, "iov.numa");
_Static_assert(offsetof(struct hf3fs_ior, iorh) == 312, "ior.iorh");
_Static_assert(offsetof(struct hf3fs_ior, mount_point) == 320, "ior.mount_point");
_Static_assert(offsetof(struct hf3fs_ior, for_read) == 576, "ior.for_read");
_Static_assert(offsetof(struct hf3fs_ior, io_depth) == 580, "ior.io_depth");
_Static_assert(offsetof(struct hf3fs_ior, priority) == 584, "ior.priority");
_Static_assert(offsetof(struct hf3fs_ior, timeout) == 588, "ior.timeout");
_Static_assert(offsetof(struct hf3fs_ior, flags) == 592, "ior.flags");
_Static_assert(offsetof(struct hf3fs_cqe, index) == 0, "cqe.index");
_Static_assert(offsetof(struct hf3fs_cqe, result) == 8, "cqe.result");
_Static_assert(offsetof(struct hf3fs_cqe, userdata) == 16, "cqe.userdata");

#define SHIM_MAX_ENTRIES 65536
#define SHIM_MAX_REGS 8192

struct shim_io {
  int used;
  bool read;
  void *ptr;
  int fd;
  size_t off;
  uint64_t len;
  const void *userdata;
};

struct shim_ring {
  struct shim_io *ios;
  int capacity;
  int count; /* prepared, not yet completed */
};

struct shim_iov {
  unsigned magic;
};

static const unsigned SHIM_IOV_MAGIC = 0x57465449u; /* "WFTI" */

/* Global call counters, exposed through weft_shim_stats() so a court can prove
 * that N ranges really cost one submission. */
static unsigned long long g_submits;
static unsigned long long g_preps;
static unsigned long long g_waits;
static unsigned long long g_bytes_read;
static unsigned long long g_bytes_written;
static unsigned long long g_registrations;
static unsigned long long g_max_preps_at_submit;

/* Fault injection for the error paths; one-shot. */
static int g_fail_prep_errno;
static int g_fail_submit_errno;
/* When > 0, every wait returns at most this many completions even when more are
 * ready, so a court can exercise the documented "may return fewer than ready"
 * clause. Zero (the default) returns everything that was asked for. */
static int g_partial_wait;

static int g_reg_fds[SHIM_MAX_REGS];
static int g_reg_used[SHIM_MAX_REGS];
/* The published ABI forbids concurrent prep/wait per ring but says nothing about
 * the registration table, which is process-global here. Rust's test harness runs
 * tests on threads, so the table is guarded. */
static pthread_mutex_t g_reg_lock = PTHREAD_MUTEX_INITIALIZER;

static int reg_lookup(int handle) {
  if (handle >= 0) {
    return handle; /* an unregistered raw fd; prep_io rejects it explicitly */
  }
  int slot = -handle - 1;
  if (slot < 0 || slot >= SHIM_MAX_REGS || !g_reg_used[slot]) {
    return -1;
  }
  return g_reg_fds[slot];
}

static bool fd_is_registered(int fd) {
  bool found = false;
  pthread_mutex_lock(&g_reg_lock);
  for (int i = 0; i < SHIM_MAX_REGS; i++) {
    if (g_reg_used[i] && g_reg_fds[i] == fd) {
      found = true;
      break;
    }
  }
  pthread_mutex_unlock(&g_reg_lock);
  return found;
}

bool hf3fs_is_hf3fs(int fd) {
  (void)fd;
  return false; /* this is a conformance shim, not a 3FS mount */
}

int hf3fs_extract_mount_point(char *hf3fs_mount_point, int size, const char *path) {
  (void)path;
  if (hf3fs_mount_point == NULL || size <= 0) {
    return -1;
  }
  hf3fs_mount_point[0] = '\0';
  return 1; /* the empty path, NUL-terminated: one byte including the NUL */
}

int hf3fs_iovcreate(struct hf3fs_iov *iov, const char *hf3fs_mount_point,
                    size_t size, size_t block_size, int numa) {
  if (iov == NULL || hf3fs_mount_point == NULL || size == 0) {
    return -EINVAL;
  }
  void *base = NULL;
  if (posix_memalign(&base, 4096, size) != 0) {
    return -ENOMEM;
  }
  memset(iov, 0, sizeof(*iov));
  iov->base = (uint8_t *)base;
  struct shim_iov *tag = calloc(1, sizeof(*tag));
  if (tag == NULL) {
    free(base);
    return -ENOMEM;
  }
  tag->magic = SHIM_IOV_MAGIC;
  iov->iovh = tag;
  snprintf(iov->mount_point, sizeof(iov->mount_point), "%s", hf3fs_mount_point);
  iov->size = size;
  iov->block_size = block_size;
  iov->numa = numa;
  return 0;
}

void hf3fs_iovdestroy(struct hf3fs_iov *iov) {
  if (iov == NULL || iov->iovh == NULL) {
    return;
  }
  struct shim_iov *tag = (struct shim_iov *)iov->iovh;
  if (tag->magic != SHIM_IOV_MAGIC) {
    return;
  }
  free(iov->base);
  tag->magic = 0;
  free(tag);
  iov->base = NULL;
  iov->iovh = NULL;
  iov->size = 0;
}

int hf3fs_iorcreate4(struct hf3fs_ior *ior, const char *hf3fs_mount_point,
                     int entries, bool for_read, int io_depth, int timeout,
                     int numa, uint64_t flags) {
  if (ior == NULL || hf3fs_mount_point == NULL) {
    return -EINVAL;
  }
  if (entries <= 0 || entries > SHIM_MAX_ENTRIES) {
    return -EINVAL;
  }
  struct shim_ring *ring = calloc(1, sizeof(*ring));
  if (ring == NULL) {
    return -ENOMEM;
  }
  ring->ios = calloc((size_t)entries, sizeof(*ring->ios));
  if (ring->ios == NULL) {
    free(ring);
    return -ENOMEM;
  }
  ring->capacity = entries;
  ring->count = 0;

  memset(ior, 0, sizeof(*ior));
  ior->iorh = ring;
  snprintf(ior->mount_point, sizeof(ior->mount_point), "%s", hf3fs_mount_point);
  ior->for_read = for_read;
  ior->io_depth = io_depth;
  ior->priority = 0;
  ior->timeout = timeout;
  ior->flags = flags;
  ior->iov.numa = numa;
  return 0;
}

void hf3fs_iordestroy(struct hf3fs_ior *ior) {
  if (ior == NULL || ior->iorh == NULL) {
    return;
  }
  struct shim_ring *ring = (struct shim_ring *)ior->iorh;
  free(ring->ios);
  free(ring);
  ior->iorh = NULL;
}

int hf3fs_reg_fd(int fd, uint64_t flags) {
  (void)flags;
  if (fd < 0) {
    return EBADF;
  }
  pthread_mutex_lock(&g_reg_lock);
  for (int i = 0; i < SHIM_MAX_REGS; i++) {
    if (g_reg_used[i] && g_reg_fds[i] == fd) {
      pthread_mutex_unlock(&g_reg_lock);
      return EINVAL;
    }
  }
  for (int i = 0; i < SHIM_MAX_REGS; i++) {
    if (!g_reg_used[i]) {
      g_reg_used[i] = 1;
      g_reg_fds[i] = fd;
      g_registrations++;
      pthread_mutex_unlock(&g_reg_lock);
      return -(i + 1);
    }
  }
  pthread_mutex_unlock(&g_reg_lock);
  return EMFILE;
}

void hf3fs_dereg_fd(int fd) {
  pthread_mutex_lock(&g_reg_lock);
  for (int i = 0; i < SHIM_MAX_REGS; i++) {
    if (g_reg_used[i] && g_reg_fds[i] == fd) {
      g_reg_used[i] = 0;
      g_reg_fds[i] = -1;
      break;
    }
  }
  pthread_mutex_unlock(&g_reg_lock);
}

int hf3fs_io_entries(const struct hf3fs_ior *ior) {
  if (ior == NULL || ior->iorh == NULL) {
    return -EINVAL;
  }
  return ((const struct shim_ring *)ior->iorh)->capacity;
}

int hf3fs_prep_io(const struct hf3fs_ior *ior, const struct hf3fs_iov *iov,
                  bool read, void *ptr, int fd, size_t off, uint64_t len,
                  const void *userdata) {
  if (g_fail_prep_errno != 0) {
    int err = g_fail_prep_errno;
    g_fail_prep_errno = 0;
    return -err;
  }
  if (ior == NULL || iov == NULL || ior->iorh == NULL) {
    return -EINVAL;
  }
  if (read != ior->for_read) {
    return -EINVAL; /* an Ior is read-only or write-only */
  }
  struct shim_ring *ring = (struct shim_ring *)ior->iorh;
  if (ring->count >= ring->capacity) {
    return -ENOSPC;
  }
  if (len > 0) {
    if (ptr == NULL || iov->base == NULL) {
      return -EINVAL;
    }
    uintptr_t lo = (uintptr_t)iov->base;
    uintptr_t hi = lo + iov->size;
    uintptr_t p = (uintptr_t)ptr;
    if (p < lo || p + len < p || p + len > hi) {
      return -EINVAL; /* the documented buffer-containment rule */
    }
  }
  int real_fd = reg_lookup(fd);
  if (real_fd < 0 || !fd_is_registered(real_fd)) {
    return -EINVAL; /* only registered fds may be used */
  }
  int slot = ring->count++;
  struct shim_io *io = &ring->ios[slot];
  io->used = 1;
  io->read = read;
  io->ptr = ptr;
  io->fd = real_fd;
  io->off = off;
  io->len = len;
  io->userdata = userdata;
  g_preps++;
  return slot;
}

int hf3fs_submit_ios(const struct hf3fs_ior *ior) {
  if (g_fail_submit_errno != 0) {
    int err = g_fail_submit_errno;
    g_fail_submit_errno = 0;
    return -err;
  }
  if (ior == NULL || ior->iorh == NULL) {
    return -EINVAL;
  }
  struct shim_ring *ring = (struct shim_ring *)ior->iorh;
  g_submits++;
  if ((unsigned long long)ring->count > g_max_preps_at_submit) {
    g_max_preps_at_submit = (unsigned long long)ring->count;
  }
  return 0;
}

int hf3fs_wait_for_ios(const struct hf3fs_ior *ior, struct hf3fs_cqe *cqes,
                       int cqec, int min_results,
                       const struct timespec *abs_timeout) {
  (void)abs_timeout;
  /* `min_results` is accepted but not enforced: this shim completes every
   * prepared I/O synchronously, and the adapter must tolerate a return smaller
   * than `min_results` (the documented "may return fewer than ready" clause),
   * which `weft_shim_partial_waits` exercises directly. */
  (void)min_results;
  if (ior == NULL || ior->iorh == NULL || cqes == NULL || cqec <= 0) {
    return -EINVAL;
  }
  struct shim_ring *ring = (struct shim_ring *)ior->iorh;
  if (ring->count <= 0) {
    return 0;
  }
  int want = cqec;
  if (want > ring->count) {
    want = ring->count;
  }
  if (g_partial_wait > 0 && want > g_partial_wait) {
    want = g_partial_wait;
  }
  int produced = 0;
  for (int i = 0; i < want; i++) {
    struct shim_io *io = &ring->ios[i];
    ssize_t rc;
    if (io->read) {
      rc = pread(io->fd, io->ptr, (size_t)io->len, (off_t)io->off);
      if (rc >= 0) {
        g_bytes_read += (unsigned long long)rc;
      }
    } else {
      rc = pwrite(io->fd, io->ptr, (size_t)io->len, (off_t)io->off);
      if (rc >= 0) {
        g_bytes_written += (unsigned long long)rc;
      }
    }
    cqes[produced].index = i;
    cqes[produced].reserved = 0;
    cqes[produced].result = (int64_t)rc;
    cqes[produced].userdata = io->userdata;
    produced++;
    io->used = 0;
  }
  g_waits++;
  /* Compact the remaining entries to the front, preserving order. */
  int remaining = ring->count - produced;
  if (remaining > 0) {
    memmove(&ring->ios[0], &ring->ios[produced],
            (size_t)remaining * sizeof(ring->ios[0]));
  }
  ring->count = remaining;
  return produced;
}

/* ------------------------------------------------------------------ */
/* Test-only hooks. These are not part of the ABI.                     */
/* ------------------------------------------------------------------ */

void weft_shim_stats(unsigned long long *submits, unsigned long long *preps,
                     unsigned long long *waits, unsigned long long *bytes_read,
                     unsigned long long *bytes_written,
                     unsigned long long *registrations,
                     unsigned long long *max_preps_at_submit) {
  if (submits) *submits = g_submits;
  if (preps) *preps = g_preps;
  if (waits) *waits = g_waits;
  if (bytes_read) *bytes_read = g_bytes_read;
  if (bytes_written) *bytes_written = g_bytes_written;
  if (registrations) *registrations = g_registrations;
  if (max_preps_at_submit) *max_preps_at_submit = g_max_preps_at_submit;
}

void weft_shim_reset(void) {
  g_submits = 0;
  g_preps = 0;
  g_waits = 0;
  g_bytes_read = 0;
  g_bytes_written = 0;
  g_registrations = 0;
  g_max_preps_at_submit = 0;
  g_fail_prep_errno = 0;
  g_fail_submit_errno = 0;
  g_partial_wait = 0;
}

void weft_shim_partial_waits(int n) { g_partial_wait = n; }

void weft_shim_fail_next_prep(int err) { g_fail_prep_errno = err; }
void weft_shim_fail_next_submit(int err) { g_fail_submit_errno = err; }

int weft_shim_registered_fds(void) {
  int n = 0;
  pthread_mutex_lock(&g_reg_lock);
  for (int i = 0; i < SHIM_MAX_REGS; i++) {
    if (g_reg_used[i]) {
      n++;
    }
  }
  pthread_mutex_unlock(&g_reg_lock);
  return n;
}
