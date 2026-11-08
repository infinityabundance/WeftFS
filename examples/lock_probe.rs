//! Writer-lock mechanism probe (subphase T2).
//!
//! Measures the property the single-writer store actually needs: after a
//! handle that owns the lock is dropped, a concurrent reopen in the same
//! process must not see `Busy` merely because an unrelated `fork()` happened.
//!
//! Three mechanisms are compared on the same `LOCK` file:
//!
//! * `flock` — `flock(2)` `LOCK_EX|LOCK_NB` (what `fs2` uses, the previous
//!   mechanism).
//! * `ofd`   — `fcntl(2)` `F_OFD_SETLK` with `F_WRLCK` over the whole file.
//! * `posix` — `fcntl(2)` `F_SETLK` (classic per-process record lock).
//!
//! Two probes:
//!
//! * `fork-window` — deterministic. Acquire, fork a child that does **not**
//!   exec and sleeps, close the parent's descriptor, then reopen and try to
//!   acquire. This isolates descriptor inheritance.
//! * `stress` — the measured flake. N iterations of
//!   `open+lock / drop / open+lock` on one thread while another thread
//!   continuously spawns `/bin/true` (a real fork→exec), counting spurious
//!   `Busy`.
//!
//! `--unlock-on-drop` adds an explicit `LOCK_UN` / `F_UNLCK` before `close(2)`,
//! which is what [`weftfs::writer_lock`] now does. The probe reports both forms
//! so the fix is measured, not asserted.
//!
//! `--mode store` measures the same property at the store level: the real
//! `LOCK` file of a `Store`, a real `Store::open`, and either the legacy
//! close-only `flock` claim or the shipped `WriterLock`.
//!
//! Run only inside the OOM-protected court. The file lives on the court's
//! bounded tmpfs, so these numbers are for the court, not for every
//! filesystem.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use weftfs::store::{OpenMode, Options, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mechanism {
    Flock,
    Ofd,
    Posix,
}

impl Mechanism {
    fn name(self) -> &'static str {
        match self {
            Mechanism::Flock => "flock",
            Mechanism::Ofd => "ofd",
            Mechanism::Posix => "posix",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "flock" => Some(Mechanism::Flock),
            "ofd" => Some(Mechanism::Ofd),
            "posix" => Some(Mechanism::Posix),
            _ => None,
        }
    }
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn flock(file: &File, command: libc::c_int) -> io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), command) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn fcntl_lock(file: &File, command: libc::c_int, kind: libc::c_short) -> io::Result<()> {
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = 0;
    lock.l_len = 0; // whole file, including future growth
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), command, &mut lock) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn acquire(file: &File, mechanism: Mechanism) -> io::Result<()> {
    match mechanism {
        Mechanism::Flock => flock(file, libc::LOCK_EX | libc::LOCK_NB),
        Mechanism::Ofd => fcntl_lock(file, libc::F_OFD_SETLK, libc::F_WRLCK as libc::c_short),
        Mechanism::Posix => fcntl_lock(file, libc::F_SETLK, libc::F_WRLCK as libc::c_short),
    }
}

fn release(file: &File, mechanism: Mechanism) {
    match mechanism {
        Mechanism::Flock => {
            let _ = flock(file, libc::LOCK_UN);
        }
        Mechanism::Ofd => {
            let _ = fcntl_lock(file, libc::F_OFD_SETLK, libc::F_UNLCK as libc::c_short);
        }
        Mechanism::Posix => {
            let _ = fcntl_lock(file, libc::F_SETLK, libc::F_UNLCK as libc::c_short);
        }
    }
}

fn is_busy(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::EACCES) | Some(libc::EAGAIN))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("weft-lock-probe-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir.join("LOCK")
}

/// Warm the page cache/tmpfs inode so the first iteration is not special.
fn warm(path: &Path) {
    let file = open(path).expect("open");
    acquire(&file, Mechanism::Flock).expect("warm lock");
    release(&file, Mechanism::Flock);
    drop(file);
}

fn probe_fork_window(path: &Path, mechanism: Mechanism, unlock_on_drop: bool) -> (bool, String) {
    let file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    acquire(&file, mechanism).expect("initial acquire");
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return (
            false,
            format!("fork failed: {}", io::Error::last_os_error()),
        );
    }
    if pid == 0 {
        // Deliberately no exec: this is exactly the window in which a forked
        // child still references the inherited open file description.
        unsafe { libc::usleep(300_000) };
        unsafe { libc::_exit(0) };
    }
    if unlock_on_drop {
        release(&file, mechanism);
    }
    // Close the parent's descriptor: with flock/OFD the lock belongs to the
    // open file description, which the child still references.
    drop(file);
    let reopened = open(path).expect("reopen");
    let outcome = acquire(&reopened, mechanism);
    let acquired = outcome.is_ok();
    let result = match &outcome {
        Ok(()) => (true, "acquired".to_owned()),
        Err(err) if is_busy(err) => (false, format!("busy: {err}")),
        Err(err) => (false, format!("error: {err}")),
    };
    if acquired {
        release(&reopened, mechanism);
    }
    drop(reopened);
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    result
}

fn probe_stress(
    path: &Path,
    mechanism: Mechanism,
    unlock_on_drop: bool,
    iterations: u64,
) -> (u64, u64, u64) {
    let stop = Arc::new(AtomicBool::new(false));
    let forked = Arc::new(AtomicU64::new(0));
    let forker = {
        let stop = Arc::clone(&stop);
        let forked = Arc::clone(&forked);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match Command::new("/bin/true").status() {
                    Ok(_) => {
                        forked.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
        })
    };
    let mut busy = 0u64;
    let mut other = 0u64;
    for _ in 0..iterations {
        let file = open(path).expect("open");
        if let Err(err) = acquire(&file, mechanism) {
            // A failure on the *first* acquire of an iteration has the same
            // cause as a reopen failure: another process/description holds it.
            if is_busy(&err) {
                busy += 1;
            } else {
                other += 1;
            }
            drop(file);
            continue;
        }
        if unlock_on_drop {
            release(&file, mechanism);
        }
        drop(file);
        let reopened = open(path).expect("reopen");
        match acquire(&reopened, mechanism) {
            Ok(()) => {
                if unlock_on_drop {
                    release(&reopened, mechanism);
                }
                drop(reopened);
            }
            Err(err) if is_busy(&err) => {
                busy += 1;
                drop(reopened);
            }
            Err(_) => {
                other += 1;
                drop(reopened);
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = forker.join();
    (busy, other, forked.load(Ordering::Relaxed))
}

fn store_probe(iterations: u64) -> Vec<serde_json::Value> {
    let dir = std::env::temp_dir().join(format!("weft-lock-store-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create store dir");
    drop(Store::init(&dir, Options::default()).expect("init store"));
    let lock_path = dir.join("LOCK");
    let mut rows = Vec::new();

    // Legacy: the store's LOCK claimed with `flock` and released only by
    // closing the descriptor, exactly as `fs2` did before this subphase. The
    // lock is acquired *before* the fork so the child inherits the open file
    // description, which is the historical failure mode.
    let legacy_file = open(&lock_path).expect("open LOCK");
    flock(&legacy_file, libc::LOCK_EX | libc::LOCK_NB).expect("legacy flock");
    let legacy_busy = with_forked_child(300_000, || {
        drop(legacy_file); // close only
        match Store::open_with_options(&dir, OpenMode::ReadWrite, Options::default()) {
            Ok(store) => {
                drop(store);
                false
            }
            Err(err) => {
                assert!(err.message().contains("locked by another writer"), "{err}");
                true
            }
        }
    });
    rows.push(json!({
        "mechanism": "legacy_flock_close_only",
        "store_reopen_after_fork_saw_busy": legacy_busy,
    }));

    // Shipped: `WriterLock` (OFD + compatibility flock + explicit unlock),
    // acquired before the fork for the same reason.
    let shipped_lock = weftfs::writer_lock::WriterLock::acquire(&lock_path).expect("writer lock");
    let shipped_busy = with_forked_child(300_000, || {
        drop(shipped_lock);
        match Store::open_with_options(&dir, OpenMode::ReadWrite, Options::default()) {
            Ok(store) => {
                drop(store);
                false
            }
            Err(err) => {
                assert!(err.message().contains("locked by another writer"), "{err}");
                true
            }
        }
    });
    rows.push(json!({
        "mechanism": "shipped_ofd_explicit_unlock",
        "store_reopen_after_fork_saw_busy": shipped_busy,
    }));

    // Store-level stress: open/drop the real store while another thread
    // continuously forks and execs.
    let stop = Arc::new(AtomicBool::new(false));
    let forked = Arc::new(AtomicU64::new(0));
    let forker = {
        let stop = Arc::clone(&stop);
        let forked = Arc::clone(&forked);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if Command::new("/bin/true").status().is_ok() {
                    forked.fetch_add(1, Ordering::Relaxed);
                } else {
                    break;
                }
            }
        })
    };
    let mut busy = 0u64;
    let mut other = 0u64;
    for _ in 0..iterations {
        match Store::open_with_options(&dir, OpenMode::ReadWrite, Options::default()) {
            Ok(store) => drop(store),
            Err(err) => {
                if err.message().contains("locked by another writer") {
                    busy += 1;
                } else {
                    other += 1;
                }
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = forker.join();
    rows.push(json!({
        "mechanism": "shipped_store_open_stress",
        "iterations": iterations,
        "spurious_busy": busy,
        "other_errors": other,
        "forks": forked.load(Ordering::Relaxed),
    }));
    let _ = std::fs::remove_dir_all(&dir);
    rows
}

fn with_forked_child<T>(micros: u32, probe: impl FnOnce() -> T) -> T {
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        unsafe { libc::usleep(micros) };
        unsafe { libc::_exit(0) };
    }
    let out = probe();
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let iterations: u64 = value("--iterations")
        .and_then(|text| text.parse().ok())
        .unwrap_or(2_000);
    let out = value("--out");
    let only = value("--mechanism").and_then(|text| Mechanism::parse(&text));
    let mode = value("--mode").unwrap_or_else(|| "mechanism".to_owned());

    if mode == "store" {
        let receipt = json!({
            "schema": "weft.lock.probe.v1",
            "mode": "store",
            "note": "court tmpfs; one host; real Store LOCK and real Store::open",
            "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|text| text.trim().to_owned())
                .unwrap_or_else(|_| "unknown".to_owned()),
            "iterations": iterations,
            "rows": store_probe(iterations),
        });
        let text = serde_json::to_string_pretty(&receipt).expect("serialize");
        println!("{text}");
        if let Some(path) = out {
            std::fs::write(&path, text + "\n").expect("write receipt");
        }
        return;
    }

    let mechanisms: Vec<Mechanism> = match only {
        Some(one) => vec![one],
        None => vec![Mechanism::Flock, Mechanism::Ofd, Mechanism::Posix],
    };

    let mut rows = Vec::new();
    for mechanism in mechanisms {
        for unlock_on_drop in [false, true] {
            let path = scratch(&format!(
                "{}-{}",
                mechanism.name(),
                if unlock_on_drop { "unlock" } else { "close" }
            ));
            warm(&path);
            let (window_acquired, window_detail) =
                probe_fork_window(&path, mechanism, unlock_on_drop);
            let (busy, other, forked) = probe_stress(&path, mechanism, unlock_on_drop, iterations);
            rows.push(json!({
                "mechanism": mechanism.name(),
                "unlock_on_drop": unlock_on_drop,
                "fork_window_reopen_acquired": window_acquired,
                "fork_window_detail": window_detail,
                "stress_iterations": iterations,
                "stress_spurious_busy": busy,
                "stress_other_errors": other,
                "stress_forks": forked,
            }));
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
            thread::sleep(Duration::from_millis(20));
        }
    }

    let receipt = json!({
        "schema": "weft.lock.probe.v1",
        "mode": "mechanism",
        "note": "court tmpfs; one host; synthetic probe, not a store benchmark",
        "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|text| text.trim().to_owned())
            .unwrap_or_else(|_| "unknown".to_owned()),
        "iterations": iterations,
        "rows": rows,
    });
    let text = serde_json::to_string_pretty(&receipt).expect("serialize");
    println!("{text}");
    if let Some(path) = out {
        std::fs::write(&path, text + "\n").expect("write receipt");
    }
}
