//! Shared harness for the FUSE integration tests.
//!
//! Every test spawns the real `weftfs mount --foreground` binary, waits for the
//! mount to appear in `/proc/self/mountinfo`, drives the mountpoint with
//! ordinary POSIX calls, then stops the daemon with SIGTERM (the signal handler
//! drops the FUSE session, a clean unmount) and falls back to a lazy `umount`.

#![allow(dead_code)]

use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub struct Fixture {
    pub dir: TempDir,
    pub store: PathBuf,
    pub mount_dir: PathBuf,
}

impl Fixture {
    /// A fresh store and an empty mountpoint, both traversable by other uids.
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("store");
        let mount_dir = dir.path().join("mnt");
        fs::create_dir(&store).expect("create store dir");
        fs::create_dir(&mount_dir).expect("create mountpoint");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&mount_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_weftfs"))
            .arg("--store")
            .arg(&store)
            .arg("init")
            .output()
            .expect("run weftfs init");
        assert!(
            output.status.success(),
            "init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Fixture {
            dir,
            store,
            mount_dir,
        }
    }

    pub fn mount(&self, flags: &[&str]) -> Mount {
        Mount::start(&self.store, &self.mount_dir, flags, &[])
    }

    pub fn mount_with_env(&self, flags: &[&str], env: &[(&str, &str)]) -> Mount {
        Mount::start(&self.store, &self.mount_dir, flags, env)
    }

    /// Run a store CLI verb against the (unmounted) store.
    pub fn weft(&self, args: &[&OsStr]) -> Output {
        run_weft(&self.store, args)
    }

    pub fn weft_ok(&self, args: &[&OsStr]) -> String {
        let output = self.weft(args);
        assert!(
            output.status.success(),
            "weftfs {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.mount_dir.join(relative.trim_start_matches('/'))
    }
}

pub fn run_weft(store: &Path, args: &[&OsStr]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
    command.arg("--store").arg(store);
    command.args(args);
    command.output().expect("run weftfs")
}

pub fn weft_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_weftfs"))
}

pub struct Mount {
    pub child: Child,
    pub mountpoint: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
    pid: u32,
    stopped: bool,
}

impl Mount {
    pub fn start(store: &Path, mountpoint: &Path, flags: &[&str], env: &[(&str, &str)]) -> Mount {
        assert!(
            Path::new("/dev/fuse").exists(),
            "/dev/fuse is unavailable; this test requires the fuse court"
        );
        let base = mountpoint.parent().expect("mountpoint has a parent");
        let name = mountpoint.file_name().unwrap().to_string_lossy();
        let stdout = base.join(format!("mount-{name}.out"));
        let stderr = base.join(format!("mount-{name}.err"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
        command
            .arg("mount")
            .arg("--store")
            .arg(store)
            .arg("--mountpoint")
            .arg(mountpoint)
            .arg("--foreground");
        for flag in flags {
            command.arg(flag);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                File::create(&stdout).expect("create stdout log"),
            ))
            .stderr(Stdio::from(
                File::create(&stderr).expect("create stderr log"),
            ));
        let child = command.spawn().expect("spawn weftfs mount");
        let pid = child.id();
        let mut mount = Mount {
            child,
            mountpoint: mountpoint.to_path_buf(),
            stdout,
            stderr,
            pid,
            stopped: false,
        };
        mount.await_ready();
        mount
    }

    fn await_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("wait on mount daemon") {
                panic!(
                    "mount daemon exited early ({status}); stderr:\n{}\nstdout:\n{}",
                    self.stderr_text(),
                    self.stdout_text()
                );
            }
            if is_mounted(&self.mountpoint) && fs::symlink_metadata(&self.mountpoint).is_ok() {
                // Give the session loop a moment to be ready for the first
                // request; mountinfo appears when the kernel mount is created.
                std::thread::sleep(Duration::from_millis(50));
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "mountpoint {} did not become a WeftFS mount; stderr:\n{}",
            self.mountpoint.display(),
            self.stderr_text()
        );
    }

    pub fn stderr_text(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    pub fn stdout_text(&self) -> String {
        fs::read_to_string(&self.stdout).unwrap_or_default()
    }

    /// Full diagnostic snapshot for a failing operation.
    pub fn diagnostics(&mut self) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("exited {status}"),
            Ok(None) => "running".to_owned(),
            Err(err) => format!("wait error {err}"),
        };
        format!(
            "daemon {status}; mounted={}; stdout:\n{}\nstderr:\n{}",
            is_mounted(&self.mountpoint),
            self.stdout_text(),
            self.stderr_text()
        )
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait for the daemon to exit on its own (crash tests), up to `timeout`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("wait on mount daemon") {
                self.stopped = true;
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    pub fn stop(&mut self) {
        if !self.stopped {
            // SIGTERM triggers the daemon's clean unmount path.
            unsafe {
                libc::kill(self.pid as i32, libc::SIGTERM);
            }
            if self.wait_for_exit(Duration::from_secs(8)).is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            self.stopped = true;
        }
        // After a daemon death the kernel mount may linger disconnected.
        let _ = Command::new("umount")
            .arg("-l")
            .arg(&self.mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn is_mounted(mountpoint: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let target = mountpoint.to_string_lossy().to_string();
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() > 4 && fields[4] == target {
            return true;
        }
    }
    false
}

pub fn cstring(path: &Path) -> CString {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).expect("path without NUL")
}
