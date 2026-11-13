//! Writer-lock inheritance court (subphase T2).
//!
//! The single-writer store must not report `Busy` merely because an unrelated
//! `fork(2)` happened in the same process. These tests pin the deployed
//! mechanism (`F_OFD_SETLK` with an explicit unlock before close), keep the
//! two failing forms as executable documentation of the bug, and prove that
//! real exclusivity is unchanged.
//!
//! The child in each probe never `exec`s, so it holds the inherited open file
//! description for the whole window. That is the window measured by
//! `examples/lock_probe.rs` (519/2,000 spurious `Busy` for plain `flock`,
//! 592/2,000 for OFD close-only, 0/2,000 with an explicit unlock).
//!
//! The two-process test at the end is the other half of the contract: a lock
//! held by a different process is still refused, and the kernel releases it
//! when that process dies. After `fork(2)` the child calls only
//! async-signal-safe functions (syscalls, `mem::zeroed`, `_exit`), so it cannot
//! deadlock on an allocator lock held by a sibling test thread.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tempfile::tempdir;
use weftfs::error::Class;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::writer_lock::{LockMechanism, WriterLock};

/// Fork a child that does **not** exec and sleeps for `micros`, then run
/// `probe` in the parent. The child is reaped before returning.
fn with_forked_child<T>(micros: u32, probe: impl FnOnce() -> T) -> T {
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        // Only async-signal-safe calls after fork.
        unsafe { libc::usleep(micros) };
        unsafe { libc::_exit(0) };
    }
    let out = probe();
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    out
}

fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("open LOCK")
}

fn flock_try(file: &File) -> io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn ofd_try(file: &File, kind: libc::c_short) -> io::Result<()> {
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = 0;
    lock.l_len = 0;
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &mut lock) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn is_busy(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::EACCES) | Some(libc::EAGAIN))
}

fn scratch(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, b"").expect("create LOCK");
    path
}

/// Documented bug #1: the previous mechanism (`flock(2)`, as used by `fs2`)
/// releases only when the last descriptor of the open file description closes.
/// A forked child keeps that description alive, so a reopen is spuriously
/// `Busy`. This test asserts the old behaviour still exists when the explicit
/// unlock is skipped; it is the regression this subphase fixes.
#[test]
fn legacy_flock_close_only_is_blocked_by_a_forked_child() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let file = open(&path);
    flock_try(&file).expect("initial flock");

    let busy = with_forked_child(200_000, || {
        drop(file); // close only: this is what `fs2` does
        let reopened = open(&path);
        let outcome = flock_try(&reopened);
        if outcome.is_ok() {
            unsafe { libc::flock(reopened.as_raw_fd(), libc::LOCK_UN) };
            false
        } else {
            assert!(is_busy(&outcome.err().unwrap()), "unexpected error class");
            true
        }
    });
    assert!(
        busy,
        "the old flock close-only mechanism is expected to stay locked by the \
         forked child; if this ever passes the diagnosis has changed"
    );
}

/// Documented bug #2: **OFD locks are inherited across `fork(2)`** because the
/// child shares the open file description. Switching to `F_OFD_SETLK` alone
/// does *not* fix the flake. This test is the measured refutation of that
/// assumption and protects the documentation from being "cleaned up".
#[test]
fn ofd_close_only_is_also_blocked_by_a_forked_child() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let file = open(&path);
    ofd_try(&file, libc::F_WRLCK as libc::c_short).expect("initial OFD lock");

    let busy = with_forked_child(200_000, || {
        drop(file); // close only, no F_UNLCK
        let reopened = open(&path);
        let outcome = ofd_try(&reopened, libc::F_WRLCK as libc::c_short);
        if outcome.is_ok() {
            ofd_try(&reopened, libc::F_UNLCK as libc::c_short).unwrap();
            false
        } else {
            assert!(is_busy(&outcome.err().unwrap()), "unexpected error class");
            true
        }
    });
    assert!(
        busy,
        "OFD locks are inherited with the open file description; a close-only \
         OFD lock must remain held by the forked child"
    );
}

/// The deployed mechanism: `F_OFD_SETLK` plus an explicit `F_UNLCK` in `Drop`.
/// The unlock removes the lock from the shared description, so the forked
/// child cannot keep it.
#[cfg(target_os = "linux")]
#[test]
fn ofd_with_explicit_unlock_is_not_blocked_by_a_forked_child() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");

    let mut mechanism = None;
    let busy = with_forked_child(200_000, || {
        let lock = WriterLock::acquire(&path).expect("acquire");
        mechanism = Some(lock.mechanism());
        drop(lock); // unlocks, then closes
        let reopened = WriterLock::acquire(&path);
        match reopened {
            Ok(lock) => {
                assert_eq!(lock.mechanism(), LockMechanism::OpenFileDescription);
                false
            }
            Err(err) => {
                assert_eq!(err.class(), Class::Busy, "unexpected class: {err}");
                true
            }
        }
    });
    assert!(
        !busy,
        "explicit unlock must release the forked child's claim"
    );
    assert_eq!(mechanism, Some(LockMechanism::OpenFileDescription));
}

/// The documented fallback must get the same explicit unlock. Exercised on a
/// kernel that supports OFD by asking for `flock` directly.
#[test]
fn flock_fallback_with_explicit_unlock_is_not_blocked_by_a_forked_child() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let busy = with_forked_child(200_000, || {
        let lock = WriterLock::acquire_with(&path, LockMechanism::Flock).expect("acquire");
        assert_eq!(lock.mechanism(), LockMechanism::Flock);
        drop(lock);
        WriterLock::acquire_with(&path, LockMechanism::Flock).is_err()
    });
    assert!(!busy, "the flock fallback must also unlock before close");
}

/// Exclusivity is unchanged: a second live handle is still refused, and it is
/// refused with exactly the historical error class.
#[test]
fn a_second_live_writer_is_still_refused() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let first = WriterLock::acquire(&path).expect("first writer");
    let err = WriterLock::acquire(&path).expect_err("second writer must be refused");
    assert_eq!(err.class(), Class::Busy);
    assert!(
        err.message()
            .starts_with("store is locked by another writer"),
        "the Busy text is part of the contract: {err}"
    );
    drop(first);
    let second = WriterLock::acquire(&path).expect("lock is free after drop");
    drop(second);
}

/// Mixed-version exclusion, direction 1: an older binary holds only
/// `flock(2)`. OFD and `flock` are separate lock namespaces on Linux, so the
/// shipped writer must take the compatibility `flock` as well.
#[test]
fn a_legacy_flock_holder_blocks_the_shipped_writer() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let legacy = open(&path);
    flock_try(&legacy).expect("legacy flock");
    let err = WriterLock::acquire(&path).expect_err("must not share with a flock writer");
    assert_eq!(err.class(), Class::Busy);
    unsafe { libc::flock(legacy.as_raw_fd(), libc::LOCK_UN) };
    drop(legacy);
    drop(WriterLock::acquire(&path).expect("free after the legacy unlock"));
}

/// Mixed-version exclusion, direction 2: the shipped writer must block an
/// older `flock`-only writer.
#[test]
fn the_shipped_writer_blocks_a_legacy_flock_holder() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    let lock = WriterLock::acquire(&path).expect("shipped writer");
    let legacy = open(&path);
    let outcome = flock_try(&legacy);
    assert!(
        outcome.is_err(),
        "the compatibility flock must exclude an older writer"
    );
    drop(lock);
    flock_try(&legacy).expect("legacy lock succeeds after the shipped writer drops");
    unsafe { libc::flock(legacy.as_raw_fd(), libc::LOCK_UN) };
}

/// Two-process exclusion: a *different* process holds both mechanisms on its
/// own open file description, this process is refused `Busy`, and the kernel
/// releases the lock when the holder is killed. The child runs only
/// async-signal-safe calls after `fork`.
#[test]
fn another_process_holding_the_lock_refuses_this_one() {
    let dir = tempdir().unwrap();
    let path = scratch(dir.path(), "LOCK");
    // Opened before the fork so the child needs no allocation.
    let shared = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
            .expect("open LOCK")
    };
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
    let (read_fd, write_fd) = (fds[0], fds[1]);
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        // Child: no allocation after fork, no unwinding.
        unsafe {
            libc::close(read_fd);
            let flock_ok = libc::flock(shared.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0;
            let mut record: libc::flock = std::mem::zeroed();
            record.l_type = libc::F_WRLCK as libc::c_short;
            record.l_whence = libc::SEEK_SET as libc::c_short;
            record.l_start = 0;
            record.l_len = 0;
            let ofd_ok = libc::fcntl(shared.as_raw_fd(), libc::F_OFD_SETLK, &mut record) == 0;
            let report: u8 = u8::from(flock_ok && ofd_ok);
            libc::write(write_fd, &report as *const u8 as *const libc::c_void, 1);
            // Hold the open file description until the parent closes the pipe
            // (EOF) or kills us.
            let mut byte = [0u8; 1];
            libc::read(read_fd, byte.as_mut_ptr() as *mut libc::c_void, 1);
            libc::_exit(0);
        }
    }
    // The lock belongs to the open file description; the child is a second
    // reference to it, so dropping the parent's descriptor is not what releases
    // it.
    drop(shared);
    unsafe { libc::close(write_fd) };
    let mut report = [0u8; 1];
    let read = unsafe { libc::read(read_fd, report.as_mut_ptr() as *mut libc::c_void, 1) };
    assert_eq!(read, 1, "the child did not report");
    assert_eq!(report[0], 1, "the child failed to take both locks");

    let err = WriterLock::acquire(&path).expect_err("another process holds the lock");
    assert_eq!(err.class(), Class::Busy);

    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::close(read_fd);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
    let lock = WriterLock::acquire(&path).expect("the kernel releases a dead holder's lock");
    assert_eq!(lock.mechanism(), LockMechanism::OpenFileDescription);
}

/// The store-level regression: repeated init/drop/reopen while a sibling
/// thread continuously `fork`s+`exec`s. This is the shape of the historical
/// `tests/quota.rs` flake, reduced to the lock.
#[test]
fn store_reopens_under_concurrent_forks() {
    let dir = tempdir().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let forker = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::process::Command::new("/bin/true")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        })
    };
    let root = dir.path().join("store");
    drop(Store::init(&root, Options::default()).expect("init"));
    let mut busy = 0usize;
    for _ in 0..200 {
        match Store::open_with_options(&root, OpenMode::ReadWrite, Options::default()) {
            Ok(store) => drop(store),
            Err(err) => {
                assert_eq!(err.class(), Class::Busy, "unexpected failure: {err}");
                busy += 1;
            }
        }
        thread::sleep(Duration::from_micros(200));
    }
    stop.store(true, Ordering::Relaxed);
    forker.join().unwrap();
    assert_eq!(busy, 0, "reopen must never see Busy from a sibling fork");
}
