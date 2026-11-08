//! Single-writer lock for POSIX-backed planes.
//!
//! The store's writer lease is an exclusive lock on the `LOCK` file. The lock
//! is *not* part of the stored format: the file is an empty marker and its
//! bytes are never read or written by the lock itself. Only how the lock is
//! held changed in subphase T2.
//!
//! # Why not plain `flock(2)`
//!
//! `flock(2)` and `F_OFD_SETLK` both attach the lock to the **open file
//! description**. A `fork(2)` duplicates the descriptor, and the child then
//! references the same description, so the lock stays held until the child
//! either `exec`s (which closes a `FD_CLOEXEC` descriptor) or otherwise closes
//! it. An unrelated `fork()` in another thread of the same process therefore
//! keeps the lock alive for a short window after the owning handle is dropped,
//! and a concurrent reopen sees `Busy`. Measured on the court kernel before
//! this change: 519/2,000 spurious `Busy` for `flock` and **592/2,000 for
//! `F_OFD_SETLK`**; the OFD mechanism alone does not fix the flake because it
//! is inherited in exactly the same way.
//!
//! The actual root fix is to **unlock explicitly before closing the
//! descriptor**. An explicit `F_UNLCK` on the shared open file description
//! removes the lock for every duplicate at once, so a forked child cannot keep
//! it alive. Both `flock` and OFD release to 0/2,000 with the explicit unlock
//! (see `examples/lock_probe.rs`).
//!
//! OFD is still the primary mechanism because it has the ownership property
//! the store wants: the lock belongs to the open file description this handle
//! owns, it is not dropped by an unrelated close in the same process (the
//! POSIX `F_SETLK` hazard), and a second sidecar handle cannot silently
//! acquire it. A `flock(2)` lock is held **in addition** for wire compatibility
//! with earlier binaries: on Linux, `flock` and OFD/`F_SETLK` locks live in
//! separate lock namespaces and do not conflict, so an OFD-only writer would
//! silently share the store with an older `flock` writer. Both locks are
//! released explicitly before the descriptor closes, so holding both does not
//! reintroduce the inheritance window.
//!
//! `flock(2)` is also the documented fallback when the kernel or filesystem
//! rejects `F_OFD_SETLK` with `EINVAL` (Linux < 3.15, or a filesystem without
//! OFD support); in that case it is the only lock and the guarantee is the
//! same.
//!
//! # Residual
//!
//! If a process is killed without running `Drop`, the kernel closes its
//! descriptors; a forked child that still references the description keeps the
//! lock until it execs. That window belongs to the process that forked, not to
//! the store, and it is the same for `flock`, OFD, and POSIX locks. The store
//! never has two live handles in one process.

// This module is the writer-lock syscall boundary: `open(2)` flags,
// `F_SETFD`, `flock(2)` and `fcntl(2)` `F_OFD_SETLK`. Every call takes a
// descriptor this module opened, no untrusted pointer is dereferenced, and
// the only struct that crosses the boundary is the zero-initialised
// `libc::flock` built here. See the invariant list above.
#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::error::{Error, Result};

/// How the writer lock is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMechanism {
    /// `fcntl(2)` `F_OFD_SETLK` over the whole file (Linux >= 3.15). A
    /// `flock(2)` lock is additionally held for mixed-version exclusion.
    OpenFileDescription,
    /// `flock(2)` `LOCK_EX` only: the documented fallback when OFD is
    /// unavailable (or an explicit request from the fallback court).
    Flock,
}

impl LockMechanism {
    /// Stable spelling for diagnostics and receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            LockMechanism::OpenFileDescription => "ofd",
            LockMechanism::Flock => "flock",
        }
    }
}

/// An acquired writer lock. Dropping it unlocks **and then** closes.
pub struct WriterLock {
    file: File,
    mechanism: LockMechanism,
}

enum Attempt {
    Acquired,
    /// The kernel/filesystem rejected `F_OFD_SETLK`; the caller may fall back.
    Unsupported,
    Busy(io::Error),
    Failed(io::Error),
}

impl WriterLock {
    /// Acquire the exclusive writer lock on `path`.
    ///
    /// Always takes `flock(2)` first (mixed-version exclusion), then
    /// `F_OFD_SETLK` when the kernel supports it. A `Busy` result keeps the
    /// same error class and message the store has always reported.
    pub fn acquire(path: &Path) -> Result<WriterLock> {
        let file = open_lock_file(path)?;
        // Mixed-version exclusion: an older binary holds only `flock`, and the
        // two lock namespaces do not conflict on Linux. Do not drop the
        // compatibility lock on any error path.
        match lock_flock(&file) {
            Attempt::Acquired => {}
            Attempt::Busy(err) => return Err(lock_error(err)),
            Attempt::Failed(err) => return Err(Error::io(format!("cannot lock LOCK: {err}"))),
            Attempt::Unsupported => {
                return Err(Error::io(
                    "flock(2) reported an unsupported operation".to_owned(),
                ))
            }
        }
        match try_ofd_lock(&file, libc::F_WRLCK as libc::c_short) {
            Attempt::Acquired => Ok(WriterLock {
                file,
                mechanism: LockMechanism::OpenFileDescription,
            }),
            Attempt::Unsupported => Ok(WriterLock {
                file,
                mechanism: LockMechanism::Flock,
            }),
            Attempt::Busy(err) => {
                unlock_all(&file);
                Err(lock_error(err))
            }
            Attempt::Failed(err) => {
                unlock_all(&file);
                Err(Error::io(format!("cannot lock LOCK: {err}")))
            }
        }
    }

    /// Acquire with a caller-chosen mechanism. Used by the fallback court so
    /// the `flock`-only path is exercised on a kernel that supports OFD. It is
    /// not the shipped acquisition path; the store always calls
    /// [`WriterLock::acquire`].
    pub fn acquire_with(path: &Path, mechanism: LockMechanism) -> Result<WriterLock> {
        match mechanism {
            LockMechanism::OpenFileDescription => WriterLock::acquire(path),
            LockMechanism::Flock => {
                let file = open_lock_file(path)?;
                match lock_flock(&file) {
                    Attempt::Acquired => Ok(WriterLock { file, mechanism }),
                    Attempt::Busy(err) => Err(lock_error(err)),
                    Attempt::Failed(err) => Err(Error::io(format!("cannot lock LOCK: {err}"))),
                    Attempt::Unsupported => Err(Error::io(
                        "flock(2) reported an unsupported operation".to_owned(),
                    )),
                }
            }
        }
    }

    /// The mechanism this handle actually holds. `OpenFileDescription` means
    /// the OFD record lock is held **and** a `flock` compatibility lock is
    /// held alongside it; `Flock` means only `flock` is available.
    pub fn mechanism(&self) -> LockMechanism {
        self.mechanism
    }

    /// The locked descriptor, for the identity check that proves the `LOCK`
    /// name still resolves to the inode this handle locked.
    pub fn file(&self) -> &File {
        &self.file
    }
}

impl std::fmt::Debug for WriterLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterLock")
            .field("mechanism", &self.mechanism.as_str())
            .finish_non_exhaustive()
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        // Explicitly release before the descriptor closes. This is the fix for
        // the fork-inheritance window: an unlock removes the lock from the
        // shared open file description, so a child that still holds a
        // duplicate cannot keep the store locked. The `flock` compatibility
        // lock is released in the same place for the same reason.
        unlock_all(&self.file);
        // `self.file` closes after this function returns; the locks are already
        // gone, which is the point.
    }
}

fn unlock_all(file: &File) {
    let _ = try_ofd_lock(file, libc::F_UNLCK as libc::c_short);
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    // `O_CLOEXEC` closes the descriptor on exec; `FD_CLOEXEC` is set again
    // below with `F_SETFD` so the property does not depend on how the file was
    // opened (belt and braces: a caller may hand us a descriptor later).
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .map_err(|err| Error::io(format!("cannot open LOCK: {err}")))
        .map(|file| {
            let _ = set_close_on_exec(&file);
            file
        })
}

fn set_close_on_exec(file: &File) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn lock_error(err: io::Error) -> Error {
    Error::busy(format!("store is locked by another writer: {err}"))
}

fn lock_flock(file: &File) -> Attempt {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Attempt::Acquired;
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EAGAIN) | Some(libc::EACCES) => Attempt::Busy(err),
        _ => Attempt::Failed(err),
    }
}

#[cfg(target_os = "linux")]
fn flock_struct(kind: libc::c_short) -> libc::flock {
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = 0;
    lock.l_len = 0; // whole file, through EOF and future growth
    lock.l_pid = 0;
    lock
}

#[cfg(target_os = "linux")]
fn try_ofd_lock(file: &File, kind: libc::c_short) -> Attempt {
    let mut lock = flock_struct(kind);
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &mut lock) };
    if rc == 0 {
        return Attempt::Acquired;
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        // The kernel or filesystem does not implement OFD locks.
        Some(libc::EINVAL) => Attempt::Unsupported,
        // `EAGAIN`/`EACCES` are the documented conflict result of `F_SETLK`.
        Some(libc::EAGAIN) | Some(libc::EACCES) => Attempt::Busy(err),
        _ => Attempt::Failed(err),
    }
}

#[cfg(not(target_os = "linux"))]
fn try_ofd_lock(_file: &File, _kind: libc::c_short) -> Attempt {
    // OFD locks are Linux-specific; every non-Linux build uses the documented
    // `flock(2)` fallback.
    Attempt::Unsupported
}
