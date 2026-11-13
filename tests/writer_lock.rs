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
//!
//! # Synchronisation contract (no sleeps, no polling)
//!
//! Every forked child here is *held open by a pipe* and released explicitly by
//! the parent, and every assertion about the child's state is made while the
//! parent can prove the child is still running. There is no `usleep` and no
//! retry loop:
//!
//! * [`with_forked_child`] takes a control pipe. The child blocks on `read(2)`
//!   of that pipe; the parent runs its probe and then writes one byte. The
//!   child therefore exists, and holds every inherited descriptor, for exactly
//!   the window the probe needs - not for a fixed 200 ms that a loaded runner
//!   can overrun.
//! * [`another_process_holding_the_lock_refuses_this_one`] uses two pipes. The
//!   holder reports a fixed record **after** `flock(2)` and `F_OFD_SETLK` have
//!   returned, then blocks in `poll(2)`. The parent blocks on that report, pings
//!   the holder, and blocks again for a *confirmation* record it can only write
//!   from that blocked state. Only then does it probe the store. A holder that
//!   died, or that never reached the blocking wait, therefore produces a
//!   diagnostic instead of a silently passing exclusion assertion.
//!
//! The previous version of the two-process test closed its read end and then
//! read from it. The read failed `EBADF`, the holder exited immediately, and the
//! parent's probe raced that exit: a reduced C reproduction of the child saw the
//! hold read fail with `EBADF` in every iteration and the parent acquired the
//! lock exactly in the iterations where the holder had already exited, and direct
//! runs of the old test failed at `tests/writer_lock.rs:308:42` (CI run
//! 43408465922).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;
use weftfs::error::Class;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::writer_lock::{LockMechanism, WriterLock};

/// The thread's `errno` word. Only used in the post-`fork` child;
/// `io::Error::last_os_error` is a small value for an OS error and does not
/// allocate.
fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Fork a child that does **not** exec and holds every inherited descriptor
/// open until `probe` has finished. There is no sleep: the child blocks on the
/// parent end of a pipe and is released explicitly, so the window is as long as
/// the probe needs and no longer.
fn with_forked_child<T>(probe: impl FnOnce() -> T) -> T {
    let mut fds = [0i32; 2];
    assert_eq!(
        unsafe { libc::pipe(fds.as_mut_ptr()) },
        0,
        "pipe failed: {}",
        io::Error::last_os_error()
    );
    let (read_fd, write_fd) = (fds[0], fds[1]);
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        // Only async-signal-safe calls after fork. Keep `read_fd` open: it is
        // the descriptor this read blocks on.
        unsafe {
            libc::close(write_fd);
            let mut byte = [0u8; 1];
            loop {
                let n = libc::read(read_fd, byte.as_mut_ptr() as *mut libc::c_void, 1);
                // 0 is EOF (the parent closed the pipe) and 1 is the explicit
                // release byte; either way the probe is over.
                if n >= 0 || last_errno() != libc::EINTR {
                    break;
                }
            }
            libc::_exit(0);
        }
    }
    unsafe { libc::close(read_fd) };
    let out = probe();
    unsafe {
        let byte = 1u8;
        let _ = libc::write(write_fd, &byte as *const u8 as *const libc::c_void, 1);
        libc::close(write_fd);
        let mut status = 0;
        let rc = libc::waitpid(pid, &mut status, 0);
        assert_eq!(rc, pid, "waitpid failed: {}", io::Error::last_os_error());
    }
    out
}

/// Fixed 10-byte record the holder writes after the lock calls and again after
/// answering a liveness ping. Little-endian, no padding, one atomic `write(2)`,
/// so it is safe to build on the stack in a child that has only
/// async-signal-safe functions available.
const REPORT_LEN: usize = 10;
const PHASE_ACQUIRED: u8 = 1;
const PHASE_CONFIRMED: u8 = 2;

#[derive(Clone, Copy, Debug)]
struct LockReport {
    phase: u8,
    acquired: bool,
    flock_errno: i32,
    ofd_errno: i32,
}

impl LockReport {
    fn encode(self) -> [u8; REPORT_LEN] {
        let mut out = [0u8; REPORT_LEN];
        out[0] = self.phase;
        out[1] = u8::from(self.acquired);
        out[2..6].copy_from_slice(&self.flock_errno.to_le_bytes());
        out[6..10].copy_from_slice(&self.ofd_errno.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8; REPORT_LEN]) -> LockReport {
        let mut flock = [0u8; 4];
        flock.copy_from_slice(&bytes[2..6]);
        let mut ofd = [0u8; 4];
        ofd.copy_from_slice(&bytes[6..10]);
        LockReport {
            phase: bytes[0],
            acquired: bytes[1] == 1,
            flock_errno: i32::from_le_bytes(flock),
            ofd_errno: i32::from_le_bytes(ofd),
        }
    }

    fn describe(self) -> String {
        if self.acquired {
            "both locks held".to_owned()
        } else {
            format!(
                "the holder did not acquire both locks: flock errno {} ({}), \
                 F_OFD_SETLK errno {} ({})",
                self.flock_errno,
                io::Error::from_raw_os_error(self.flock_errno),
                self.ofd_errno,
                io::Error::from_raw_os_error(self.ofd_errno),
            )
        }
    }
}

/// Write every byte of `bytes` to `fd`. Async-signal-safe; used in the child.
unsafe fn write_all_raw(fd: i32, bytes: &[u8]) -> bool {
    let mut written = 0usize;
    while written < bytes.len() {
        let n = libc::write(
            fd,
            bytes[written..].as_ptr() as *const libc::c_void,
            bytes.len() - written,
        );
        if n <= 0 {
            if n == 0 || last_errno() != libc::EINTR {
                return false;
            }
            continue;
        }
        written += n as usize;
    }
    true
}

/// Read `REPORT_LEN` bytes from `fd`, waiting no longer than `timeout` in
/// total. A short read loop is not synchronisation: `poll(2)` carries the
/// deadline, so a holder that never reports produces a diagnostic instead of a
/// hung test.
fn read_report(fd: i32, timeout: Duration) -> Result<LockReport, String> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; REPORT_LEN];
    let mut filled = 0usize;
    while filled < REPORT_LEN {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the holder did not report within {} ms (received {} of {} bytes)",
                timeout.as_millis(),
                filled,
                REPORT_LEN
            ));
        }
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = left.as_millis().min(i32::MAX as u128) as i32;
        let rc = unsafe { libc::poll(&mut pollfd, 1, millis) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("poll(2) waiting for the holder failed: {err}"));
        }
        if rc == 0 {
            continue; // the deadline check above produces the diagnostic
        }
        let n = unsafe {
            libc::read(
                fd,
                buf[filled..].as_mut_ptr() as *mut libc::c_void,
                REPORT_LEN - filled,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("read(2) of the holder's report failed: {err}"));
        }
        if n == 0 {
            return Err(format!(
                "the holder died before reporting (received {} of {} bytes)",
                filled, REPORT_LEN
            ));
        }
        filled += n as usize;
    }
    Ok(LockReport::decode(&buf))
}

/// Kill and reap a child that broke the protocol, returning its exit status so
/// the panic names it instead of leaving a stuck process behind.
fn abandon_child(pid: libc::pid_t) -> String {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status = 0;
        if libc::waitpid(pid, &mut status, 0) != pid {
            return format!("waitpid failed: {}", io::Error::last_os_error());
        }
        if libc::WIFEXITED(status) {
            let code = libc::WEXITSTATUS(status);
            format!("killed after the failure; child exit status {code}")
        } else if libc::WIFSIGNALED(status) {
            format!(
                "killed after the failure; child already terminated by signal {}",
                libc::WTERMSIG(status)
            )
        } else {
            "killed after the failure; child status unknown".to_owned()
        }
    }
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

    let busy = with_forked_child(|| {
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

    let busy = with_forked_child(|| {
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
    let busy = with_forked_child(|| {
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
    let busy = with_forked_child(|| {
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
///
/// Synchronisation, in order: the holder takes both locks, reports them on a
/// pipe, answers a liveness ping from the state in which it is holding them,
/// and the parent probes only after that confirmation. Nothing here is timed.
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
    // Holder -> parent: the acquisition record and, later, the confirmation.
    let mut report_fds = [0i32; 2];
    assert_eq!(
        unsafe { libc::pipe(report_fds.as_mut_ptr()) },
        0,
        "report pipe failed: {}",
        io::Error::last_os_error()
    );
    // Parent -> holder: liveness pings. The holder blocks in `poll(2)` on this
    // end while it owns the locks. The child must keep *its* end open: the
    // previous version closed this end and then read from it, so the holder
    // exited immediately and the probe raced its exit.
    let mut control_fds = [0i32; 2];
    assert_eq!(
        unsafe { libc::pipe(control_fds.as_mut_ptr()) },
        0,
        "control pipe failed: {}",
        io::Error::last_os_error()
    );
    let (report_read, report_write) = (report_fds[0], report_fds[1]);
    let (control_read, control_write) = (control_fds[0], control_fds[1]);

    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        // Child: no allocation after fork, no unwinding.
        unsafe {
            libc::close(report_read);
            libc::close(control_write);
            let mut flock_errno = 0i32;
            let mut ofd_errno = 0i32;
            if libc::flock(shared.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 {
                flock_errno = last_errno();
            } else {
                let mut record: libc::flock = std::mem::zeroed();
                record.l_type = libc::F_WRLCK as libc::c_short;
                record.l_whence = libc::SEEK_SET as libc::c_short;
                record.l_start = 0;
                record.l_len = 0;
                if libc::fcntl(shared.as_raw_fd(), libc::F_OFD_SETLK, &mut record) != 0 {
                    ofd_errno = last_errno();
                }
            }
            let report = LockReport {
                phase: PHASE_ACQUIRED,
                acquired: flock_errno == 0 && ofd_errno == 0,
                flock_errno,
                ofd_errno,
            };
            if !write_all_raw(report_write, &report.encode()) {
                libc::_exit(1); // the parent is gone
            }
            // Hold both locks in a blocking `poll(2)` on the control pipe.
            // `control_read` stays open: a closed descriptor would make poll
            // fail with EBADF and the holder would exit before the parent
            // probed - the defect this test used to have.
            loop {
                let mut ping = libc::pollfd {
                    fd: control_read,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let rc = libc::poll(&mut ping, 1, -1);
                if rc < 0 {
                    if last_errno() == libc::EINTR {
                        continue;
                    }
                    libc::_exit(2);
                }
                let mut byte = [0u8; 1];
                let n = libc::read(control_read, byte.as_mut_ptr() as *mut libc::c_void, 1);
                if n <= 0 {
                    if n < 0 && last_errno() == libc::EINTR {
                        continue;
                    }
                    libc::_exit(0); // EOF: the parent went away
                }
                // Answer from the state that owns the locks.
                let confirm = LockReport {
                    phase: PHASE_CONFIRMED,
                    acquired: flock_errno == 0 && ofd_errno == 0,
                    flock_errno,
                    ofd_errno,
                };
                if !write_all_raw(report_write, &confirm.encode()) {
                    libc::_exit(1);
                }
            }
        }
    }
    // The lock belongs to the open file description; the child is a second
    // reference to it, so dropping the parent's descriptor is not what releases
    // it.
    drop(shared);
    unsafe { libc::close(report_write) };

    let acquired = match read_report(report_read, Duration::from_secs(30)) {
        Ok(report) if report.phase == PHASE_ACQUIRED => report,
        Ok(report) => {
            let status = abandon_child(pid);
            panic!(
                "the holder reported phase {} instead of {PHASE_ACQUIRED}; {status}",
                report.phase
            );
        }
        Err(err) => {
            let status = abandon_child(pid);
            panic!("the lock holder broke its synchronisation contract: {err}; {status}");
        }
    };
    assert!(acquired.acquired, "{}", acquired.describe());

    // Liveness round-trip. The confirmation can only come from the blocked
    // `poll(2)`, so a holder that died after reporting (or that never reached
    // the blocking wait) is caught here - deterministically, not by racing an
    // upcoming `expect_err` against a process exit.
    let mut ping = [0u8; 1];
    let pinged = unsafe { libc::write(control_write, ping.as_mut_ptr() as *const libc::c_void, 1) };
    assert_eq!(
        pinged,
        1,
        "pinging the holder failed: {}",
        io::Error::last_os_error()
    );
    match read_report(report_read, Duration::from_secs(30)) {
        Ok(report) if report.phase == PHASE_CONFIRMED => {
            assert!(report.acquired, "{}", report.describe())
        }
        Ok(report) => {
            let status = abandon_child(pid);
            panic!(
                "the holder answered phase {} instead of {PHASE_CONFIRMED}; {status}",
                report.phase
            );
        }
        Err(err) => {
            let status = abandon_child(pid);
            panic!("the lock holder died while holding the lock: {err}; {status}");
        }
    }

    let err = WriterLock::acquire(&path).expect_err("another process holds the lock");
    assert_eq!(err.class(), Class::Busy);
    assert!(
        err.message()
            .starts_with("store is locked by another writer"),
        "the Busy text is part of the contract: {err}"
    );

    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::close(report_read);
        libc::close(control_write);
        libc::close(control_read);
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
