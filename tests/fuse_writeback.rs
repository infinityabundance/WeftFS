//! Write-back, group commit, and the mount content-identity default.
//!
//! These tests build only with `--features fuse` and run in the FUSE court.
//! They drive a real mount with ordinary POSIX calls.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::io::{Read, Seek, SeekFrom, Write};

use fuse_common::Fixture;

#[test]
fn mount_defaults_new_files_to_merkle_identity() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&[]);
    let path = fixture.path("m.bin");
    std::fs::write(&path, b"merkle default").unwrap();
    let stat = fixture.weft_ok(&[
        std::ffi::OsStr::new("--json"),
        std::ffi::OsStr::new("stat"),
        std::ffi::OsStr::new("/m.bin"),
    ]);
    drop(mount);
    assert!(
        stat.contains("\"content_identity\": \"merkle\""),
        "expected the mount default to be merkle, got: {stat}"
    );
    let report = fixture.weft_ok(&[
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(report.contains("ok=true"), "{report}");
}

#[test]
fn explicit_whole_file_identity_is_honoured_on_the_mount() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--content-identity", "whole-file"]);
    let path = fixture.path("w.bin");
    std::fs::write(&path, b"whole-file").unwrap();
    let stat = fixture.weft_ok(&[
        std::ffi::OsStr::new("--json"),
        std::ffi::OsStr::new("stat"),
        std::ffi::OsStr::new("/w.bin"),
    ]);
    drop(mount);
    assert!(
        stat.contains("\"content_identity\": \"whole-file\""),
        "{stat}"
    );
}

#[test]
fn write_back_serves_buffered_bytes_to_the_writer_and_publishes_on_fsync() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--write-back"]);
    let path = fixture.path("buf.bin");

    let mut writer = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    writer.write_all(b"hello buffered").unwrap();

    // Read-after-write through the same handle sees the buffered bytes.
    writer.seek(SeekFrom::Start(0)).unwrap();
    let mut seen = [0u8; 14];
    writer.read_exact(&mut seen).unwrap();
    assert_eq!(&seen, b"hello buffered");

    // Nothing is committed yet: the read-back above is served from the
    // handle's buffer. A separate store-level observer must be a separate
    // process, and forking a process that inherits the FUSE descriptor makes
    // the kernel send FLUSH when the child closes its copy, which would itself
    // publish the buffer. The cross-process commit boundary is covered by the
    // Go suite (same-process read-only store) and by docs/fuse-status.md.

    writer.sync_all().unwrap();
    let after = fixture.weft_ok(&[
        std::ffi::OsStr::new("cat"),
        std::ffi::OsStr::new("/buf.bin"),
    ]);
    assert_eq!(after, "hello buffered");
    drop(writer);

    // A fresh POSIX reader sees the published bytes.
    assert_eq!(std::fs::read(&path).unwrap(), b"hello buffered".to_vec());
    drop(mount);

    let report = fixture.weft_ok(&[
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(report.contains("ok=true"), "{report}");
}

#[test]
fn write_back_release_publishes_without_an_explicit_fsync() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--write-back"]);
    let path = fixture.path("close.bin");
    {
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"published at close").unwrap();
    }
    // The close above issued flush/release; a fresh reader sees the bytes.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"published at close".to_vec()
    );
    drop(mount);
    let report = fixture.weft_ok(&[
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(report.contains("ok=true"), "{report}");
}

#[test]
fn group_commit_makes_a_metadata_storm_visible_and_verifiable() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--group-commit"]);
    for index in 0..40 {
        std::fs::write(
            fixture.path(&format!("g-{index:03}.txt")),
            format!("payload {index}\n"),
        )
        .unwrap();
    }
    // Reads run against the live state; a listing sees every file.
    let mut names: Vec<String> = std::fs::read_dir(fixture.path("/"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("g-"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 40);
    drop(mount);
    let report = fixture.weft_ok(&[
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(report.contains("ok=true"), "{report}");
}

#[test]
fn write_back_and_group_commit_together_are_consistent() {
    let fixture = Fixture::new();
    let mount = fixture.mount(&["--write-back", "--group-commit"]);
    let path = fixture.path("both.bin");
    let mut file = std::fs::File::create(&path).unwrap();
    for chunk in 0..8 {
        file.write_all(&[chunk as u8; 4096]).unwrap();
    }
    file.sync_data().unwrap();
    drop(file);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 8 * 4096);
    for chunk in 0..8 {
        assert!(bytes[chunk * 4096..(chunk + 1) * 4096]
            .iter()
            .all(|byte| *byte == chunk as u8));
    }
    drop(mount);
    let report = fixture.weft_ok(&[
        std::ffi::OsStr::new("verify"),
        std::ffi::OsStr::new("--full"),
    ]);
    assert!(report.contains("ok=true"), "{report}");
}
