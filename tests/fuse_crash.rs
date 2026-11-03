//! P5 crash proxy: abort the mount daemon during a commit and verify the
//! store's documented old-or-new publication state.
//!
//! This is a **process-fault proxy**, not physical power loss: `process::abort`
//! inside `commit_ops`/`publish_commit` kills the daemon (SIGABRT) at a named
//! phase. Power-loss claims require a hardware/VM court and are not made here.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use fuse_common::Fixture;

const OLD: &[u8] = b"old-bytes";
const NEW: &[u8] = b"new-bytes";

fn fixture_with_file() -> Fixture {
    let fx = Fixture::new();
    let os = std::ffi::OsStr::new;
    fx.weft_ok(&[
        os("create"),
        os("/crash.txt"),
        os("--text"),
        os("old-bytes"),
    ]);
    fx
}

/// Start the write on a helper thread; the daemon aborts while serving it, so
/// the syscall may return EIO/ENOTCONN or even a short write. The test only
/// requires that it terminates.
fn attempt_write(path: std::path::PathBuf) -> mpsc::Receiver<Result<usize, std::io::Error>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|mut file| {
                file.write_all(NEW)?;
                file.sync_all()?;
                Ok(NEW.len())
            });
        let _ = tx.send(result);
    });
    rx
}

fn verify_full(fx: &Fixture) -> serde_json::Value {
    let output = fx.weft(&[
        std::ffi::OsStr::new("--json"),
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(
        output.status.success(),
        "verify failed to run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("verify JSON")
}

fn stored_bytes(fx: &Fixture) -> Vec<u8> {
    let output = fx.weft(&[
        std::ffi::OsStr::new("cat"),
        std::ffi::OsStr::new("/crash.txt"),
    ]);
    assert!(
        output.status.success(),
        "cat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn crash_after_objects_loses_the_write_and_verifies_clean() {
    let fx = fixture_with_file();
    let mut mount = fx.mount_with_env(&[], &[("WEFT_CRASH_PHASE", "after_objects")]);
    let rx = attempt_write(fx.path("crash.txt"));
    let status = mount
        .wait_for_exit(Duration::from_secs(15))
        .expect("daemon should abort during the commit");
    assert!(!status.success(), "daemon should not exit successfully");
    let _ = rx.recv_timeout(Duration::from_secs(15));

    // The commit was never published: the old bytes are authoritative.
    assert_eq!(stored_bytes(&fx), OLD);
    let report = verify_full(&fx);
    assert_eq!(report["ok"], true, "verify: {report}");
    assert_eq!(report["problems"].as_array().unwrap().len(), 0);
}

#[test]
fn crash_after_commit_file_keeps_old_state_and_reports_unpublished_tail() {
    let fx = fixture_with_file();
    let mut mount = fx.mount_with_env(&[], &[("WEFT_CRASH_PHASE", "after_commit_file")]);
    let rx = attempt_write(fx.path("crash.txt"));
    let status = mount
        .wait_for_exit(Duration::from_secs(15))
        .expect("daemon should abort during publication");
    assert!(!status.success());
    let _ = rx.recv_timeout(Duration::from_secs(15));

    // The commit file exists but the superblock never published it: the old
    // state is selected, and verify names the unpublished tail as a problem.
    assert_eq!(stored_bytes(&fx), OLD);
    let report = verify_full(&fx);
    assert_eq!(report["ok"], false, "verify: {report}");
    let problems = report["problems"].as_array().unwrap();
    assert!(
        problems.iter().any(|problem| problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("above the highest published superblock generation")),
        "verify problems: {problems:?}"
    );
    // No partial file: exactly one of the two whole contents was ever visible.
    let bytes = stored_bytes(&fx);
    assert!(bytes == OLD || bytes == NEW);
}

#[test]
fn crash_after_superblock_write_publishes_new_state() {
    let fx = fixture_with_file();
    let mut mount = fx.mount_with_env(&[], &[("WEFT_CRASH_PHASE", "after_superblock_write")]);
    let rx = attempt_write(fx.path("crash.txt"));
    let status = mount
        .wait_for_exit(Duration::from_secs(15))
        .expect("daemon should abort after the superblock write");
    assert!(!status.success());
    let _ = rx.recv_timeout(Duration::from_secs(15));

    // The superblock slot was written and fsynced before the abort, so the new
    // bytes are the published state.
    assert_eq!(stored_bytes(&fx), NEW);
    let report = verify_full(&fx);
    assert_eq!(report["ok"], true, "verify: {report}");
    assert_eq!(report["problems"].as_array().unwrap().len(), 0);
}
