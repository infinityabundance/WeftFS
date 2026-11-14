//! T029: FUSE mutations under optimistic publication (MW2).
//!
//! Every test spawns the real mount daemon with `--writer-mode optimistic` and
//! drives it with ordinary POSIX calls, including several independent
//! processes. The default MW1 mount is exercised in the closing test and by the
//! whole pre-existing FUSE suite; nothing here changes its path.
//!
//! Supported under MW2: create, overwrite (whole-file rewrite per write call),
//! unlink, rename (including replace), hard link, and empty-directory rmdir.
//! Refused with `EOPNOTSUPP`: mkdir, symlink, mknod, chmod/chown/utimes,
//! truncate(2), setxattr/removexattr, fallocate, copy_file_range,
//! `RENAME_EXCHANGE`. A write whose end exceeds the whole-file rewrite bound is
//! `E2BIG`.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::fs;
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};

use fuse_common::{weft_bin, Fixture};

fn sh(script: &str) -> std::process::Output {
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .expect("run sh")
}

fn verify_full(fixture: &Fixture) {
    let output = Command::new(weft_bin())
        .arg("--store")
        .arg(&fixture.store)
        .arg("--json")
        .arg("verify")
        .arg("--full")
        .output()
        .expect("run verify");
    assert!(
        output.status.success(),
        "verify --full failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn optimistic_mount_supports_create_write_rename_and_link() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let a = fixture.path("a.txt");
    let b = fixture.path("b.txt");
    fs::write(&a, b"alpha").unwrap();
    fs::write(&b, b"beta").unwrap();
    assert_eq!(fs::read(&a).unwrap(), b"alpha");

    fs::rename(&a, fixture.path("renamed.txt")).unwrap();
    assert!(!a.exists());
    assert_eq!(fs::read(fixture.path("renamed.txt")).unwrap(), b"alpha");

    fs::hard_link(&b, fixture.path("b2.txt")).unwrap();
    assert_eq!(fs::read(fixture.path("b2.txt")).unwrap(), b"beta");
    assert_eq!(fs::metadata(&b).unwrap().nlink(), 2);

    fs::remove_file(fixture.path("b2.txt")).unwrap();
    assert_eq!(fs::metadata(&b).unwrap().nlink(), 1);
    fs::remove_file(fixture.path("renamed.txt")).unwrap();
    assert!(!fixture.path("renamed.txt").exists());
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn concurrent_creates_of_one_name_produce_exactly_one_file() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let target = fixture.path("hot.txt");
    let mut children = Vec::new();
    for index in 0..4 {
        let script = format!("printf 'writer-{}' > '{}'", index, target.display());
        children.push(Command::new("sh").arg("-c").arg(script).spawn().unwrap());
    }
    let mut successes = 0;
    for mut child in children {
        let status = child.wait().unwrap();
        if status.success() {
            successes += 1;
        }
    }
    // Exactly one create of one name can win; the rest are typed conflicts. A
    // shell redirect may treat a lost create as success only if it opened an
    // existing file, which is an overwrite, so allow at most one *content*.
    let content = fs::read(&target).unwrap();
    let known: Vec<&[u8]> = vec![b"writer-0", b"writer-1", b"writer-2", b"writer-3"];
    assert!(
        known.contains(&content.as_slice()),
        "final content is not one of the writers: {content:?}"
    );
    assert!(successes >= 1, "no writer succeeded");
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn concurrent_distinct_creates_and_renames_keep_exact_state() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let mut children = Vec::new();
    for index in 0..4 {
        let path = fixture.path(&format!("f{index}.txt"));
        let script = format!("printf 'payload-{}' > '{}'", index, path.display());
        children.push(Command::new("sh").arg("-c").arg(script).spawn().unwrap());
    }
    for mut child in children {
        assert!(child.wait().unwrap().success(), "a create process failed");
    }
    let mut children = Vec::new();
    for index in [0, 1] {
        let from = fixture.path(&format!("f{index}.txt"));
        let to = fixture.path(&format!("moved-{index}.txt"));
        let script = format!("mv '{}' '{}'", from.display(), to.display());
        children.push(Command::new("sh").arg("-c").arg(script).spawn().unwrap());
    }
    for mut child in children {
        assert!(child.wait().unwrap().success(), "a rename process failed");
    }
    for index in 0..4 {
        let expected = format!("payload-{index}");
        let path = if index < 2 {
            fixture.path(&format!("moved-{index}.txt"))
        } else {
            fixture.path(&format!("f{index}.txt"))
        };
        assert_eq!(
            fs::read(&path).unwrap(),
            expected.as_bytes(),
            "wrong bytes at {}",
            path.display()
        );
    }
    assert!(!fixture.path("f0.txt").exists());
    assert!(!fixture.path("f1.txt").exists());
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn a_reader_keeps_its_snapshot_across_a_concurrent_overwrite() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let path = fixture.path("snapshot.txt");
    fs::write(&path, b"first-version").unwrap();

    let gate = fixture.dir.path().join("gate");
    let ready = fixture.dir.path().join("ready");
    let script = format!(
        "exec 3< '{}'; echo ready > '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; cat <&3",
        path.display(),
        ready.display(),
        gate.display()
    );
    let child = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..600 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready.exists(), "the reader never opened the file");
    fs::write(&path, b"second-version!").unwrap();
    File::create(&gate).unwrap().write_all(b"go").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout, b"first-version",
        "an open snapshot reader observed the newer version"
    );
    assert_eq!(fs::read(&path).unwrap(), b"second-version!");
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn rename_with_replacement_is_one_transaction() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    fs::write(fixture.path("source.txt"), b"source").unwrap();
    fs::write(fixture.path("target.txt"), b"target").unwrap();
    fs::rename(fixture.path("source.txt"), fixture.path("target.txt")).unwrap();
    assert!(!fixture.path("source.txt").exists());
    assert_eq!(fs::read(fixture.path("target.txt")).unwrap(), b"source");
    fs::rename(fixture.path("target.txt"), fixture.path("final.txt")).unwrap();
    assert_eq!(fs::read(fixture.path("final.txt")).unwrap(), b"source");
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn unsupported_operations_refuse_with_a_typed_error() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let path = fixture.path("file.txt");
    fs::write(&path, b"content").unwrap();

    let mkdir = sh(&format!("mkdir '{}'", fixture.path("dir").display()));
    assert!(!mkdir.status.success(), "mkdir succeeded under MW2");
    assert!(
        String::from_utf8_lossy(&mkdir.stderr).contains("not supported"),
        "mkdir stderr: {}",
        String::from_utf8_lossy(&mkdir.stderr)
    );

    let chmod = sh(&format!("chmod 600 '{}'", path.display()));
    assert!(!chmod.status.success(), "chmod succeeded under MW2");

    let symlink = sh(&format!(
        "ln -s target '{}'",
        fixture.path("link").display()
    ));
    assert!(!symlink.status.success(), "symlink succeeded under MW2");

    let truncate = sh(&format!("truncate -s 0 '{}'", path.display()));
    assert!(
        !truncate.status.success(),
        "truncate(2) succeeded under MW2"
    );
    assert_eq!(fs::read(&path).unwrap(), b"content");

    let big = sh(&format!(
        "dd if=/dev/zero of='{}' bs=1M count=9 2>/dev/null",
        path.display()
    ));
    assert!(!big.status.success(), "a 9 MiB write succeeded under MW2");
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn rename_exchange_is_refused_and_leaves_both_names() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--writer-mode", "optimistic"]);
    let a = fixture.path("a.txt");
    let b = fixture.path("b.txt");
    fs::write(&a, b"a-content").unwrap();
    fs::write(&b, b"b-content").unwrap();
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes()).unwrap();
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes()).unwrap();
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    assert_eq!(rc, -1, "RENAME_EXCHANGE was accepted under MW2");
    let err = std::io::Error::last_os_error();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EOPNOTSUPP),
        "unexpected errno: {err}"
    );
    assert_eq!(fs::read(&a).unwrap(), b"a-content");
    assert_eq!(fs::read(&b).unwrap(), b"b-content");
    drop(mount);
    verify_full(&fixture);
}

#[test]
fn the_default_mount_is_unaffected() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&[]);
    fs::write(fixture.path("mw1.txt"), b"single-writer").unwrap();
    assert_eq!(fs::read(fixture.path("mw1.txt")).unwrap(), b"single-writer");
    // A mkdir is a normal MW1 mutation; the optimistic refusal must not leak
    // into the default path.
    fs::create_dir(fixture.path("dir")).unwrap();
    assert!(fixture.path("dir").is_dir());
    drop(mount);
    verify_full(&fixture);
}
