//! P5 POSIX matrix through a real FUSE mount.
//!
//! Every test spawns `weftfs mount --foreground`, waits for `/proc` mountinfo,
//! drives the mount with ordinary `std::fs`/libc calls, and unmounts via SIGTERM.
//! These tests only build with `--features fuse`.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, FileExt, MetadataExt, PermissionsExt};
use std::path::Path;

use fuse_common::{cstring, Fixture, Mount};

const RENAME_NOREPLACE: libc::c_uint = 1;
const RENAME_EXCHANGE: libc::c_uint = 2;

fn renameat2(old: &Path, new: &Path, flags: libc::c_uint) -> std::io::Result<()> {
    let old = cstring(old);
    let new = cstring(new);
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[test]
fn posix_basic_matrix() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);

    let file = fx.path("a.txt");
    let mut handle = File::create(&file).expect("create through FUSE");
    handle.write_all(b"hello").unwrap();
    handle.sync_all().unwrap();
    drop(handle);
    assert_eq!(fs::read(&file).unwrap(), b"hello");
    let meta = fs::metadata(&file).unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.len(), 5);
    assert_eq!(meta.nlink(), 1);
    assert_eq!(meta.permissions().mode() & 0o777, 0o644);

    // O_APPEND appends at the current size even though the kernel sends offset 0.
    let mut handle = OpenOptions::new().append(true).open(&file).unwrap();
    handle.write_all(b" world").unwrap();
    drop(handle);
    assert_eq!(fs::read(&file).unwrap(), b"hello world");

    // O_TRUNC empties the file and the next write starts at 0.
    let handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&file)
        .unwrap();
    drop(handle);
    assert_eq!(fs::read(&file).unwrap(), b"");
    assert_eq!(fs::metadata(&file).unwrap().len(), 0);
    fs::write(&file, b"renamed-content").unwrap();

    // Rename preserves the inode number.
    let before = fs::metadata(&file).unwrap().ino();
    fs::rename(&file, fx.path("b.txt")).unwrap();
    assert!(!file.exists());
    assert_eq!(fs::metadata(fx.path("b.txt")).unwrap().ino(), before);

    // mkdir / nested write / readdir.
    fs::create_dir(fx.path("dir")).unwrap();
    fs::write(fx.path("dir/child"), b"child").unwrap();
    let child_names: Vec<String> = fs::read_dir(fx.path("dir"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(child_names, vec!["child".to_string()]);

    // Non-empty rmdir is ENOTEMPTY; empty rmdir succeeds.
    let error = fs::remove_dir(fx.path("dir")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOTEMPTY));
    fs::create_dir(fx.path("empty")).unwrap();
    fs::remove_dir(fx.path("empty")).unwrap();
    assert!(!fx.path("empty").exists());

    // Hard link: same object, nlink 2; unlink drops it back to 1.
    fs::hard_link(fx.path("b.txt"), fx.path("c.txt")).unwrap();
    assert_eq!(fs::metadata(fx.path("b.txt")).unwrap().nlink(), 2);
    assert_eq!(
        fs::metadata(fx.path("b.txt")).unwrap().ino(),
        fs::metadata(fx.path("c.txt")).unwrap().ino()
    );
    assert_eq!(fs::read(fx.path("c.txt")).unwrap(), b"renamed-content");
    fs::remove_file(fx.path("c.txt")).unwrap();
    assert_eq!(fs::metadata(fx.path("b.txt")).unwrap().nlink(), 1);

    // Symlink and readlink.
    symlink("b.txt", fx.path("link")).unwrap();
    assert_eq!(fs::read_link(fx.path("link")).unwrap(), Path::new("b.txt"));
    assert_eq!(fs::read(fx.path("link")).unwrap(), b"renamed-content");
    assert!(fs::symlink_metadata(fx.path("link"))
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_file(fx.path("link")).unwrap();

    // The virtual surface is present and read-only.
    let root_names: Vec<String> = fs::read_dir(&fx.mount_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(root_names.contains(&".weft".to_string()), "{root_names:?}");

    // Unlink.
    fs::remove_file(fx.path("b.txt")).unwrap();
    assert!(!fx.path("b.txt").exists());
    assert_eq!(
        fs::metadata(fx.path("b.txt")).unwrap_err().raw_os_error(),
        Some(libc::ENOENT)
    );
}

#[test]
fn sparse_writes_large_files_ranges_and_open_unlinked() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);

    // Sparse write past EOF zero-fills the gap.
    let sparse = fx.path("sparse.bin");
    let mut handle = File::create(&sparse).unwrap();
    handle.seek(SeekFrom::Start(1 << 20)).unwrap();
    handle.write_all(b"tail").unwrap();
    drop(handle);
    let bytes = fs::read(&sparse).unwrap();
    assert_eq!(bytes.len(), (1 << 20) + 4);
    assert!(bytes[..1 << 20].iter().all(|byte| *byte == 0));
    assert_eq!(&bytes[1 << 20..], b"tail");

    // Partial write inside an existing file.
    let mut handle = OpenOptions::new().write(true).open(&sparse).unwrap();
    handle.seek(SeekFrom::Start(3)).unwrap();
    handle.write_all(b"XYZ").unwrap();
    drop(handle);
    let bytes = fs::read(&sparse).unwrap();
    assert_eq!(&bytes[3..6], b"XYZ");

    // 8 MiB file, written and read in 1 MiB chunks.
    let big = fx.path("big.bin");
    let mut handle = File::create(&big).unwrap();
    let block: Vec<u8> = (0..1024 * 1024).map(|index| (index % 251) as u8).collect();
    for _ in 0..8 {
        handle.write_all(&block).unwrap();
    }
    handle.sync_all().unwrap();
    drop(handle);
    assert_eq!(fs::metadata(&big).unwrap().len(), 8 * 1024 * 1024);
    let mut handle = File::open(&big).unwrap();
    let mut range = vec![0u8; 4096];
    handle.seek(SeekFrom::Start(4 * 1024 * 1024 + 17)).unwrap();
    handle.read_exact(&mut range).unwrap();
    let expected: Vec<u8> = (17..17 + 4096).map(|index| (index % 251) as u8).collect();
    assert_eq!(range, expected);
    // A range read via pread.
    let mut direct = vec![0u8; 300];
    handle.read_at(&mut direct, 2 * 1024 * 1024 + 10).unwrap();
    let expected: Vec<u8> = (10..10 + 300).map(|index| (index % 251) as u8).collect();
    assert_eq!(direct, expected);

    // O_EXCL.
    let exclusive = fx.path("exclusive");
    File::create(&exclusive).unwrap();
    let error = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&exclusive)
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EEXIST));

    // An unlinked-but-open file remains readable.
    let transient = fx.path("transient");
    fs::write(&transient, b"still here after unlink").unwrap();
    let mut open = File::open(&transient).unwrap();
    fs::remove_file(&transient).unwrap();
    assert!(!transient.exists());
    let mut contents = String::new();
    open.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "still here after unlink");

    // A write through an unlinked-but-open descriptor still works in-process.
    let writable = fx.path("writable");
    fs::write(&writable, b"abc").unwrap();
    let mut open = OpenOptions::new().write(true).open(&writable).unwrap();
    fs::remove_file(&writable).unwrap();
    open.write_all(b"def").unwrap();
    drop(open);
}

#[test]
fn rename_semantics_and_renameat2_flags() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);

    fs::write(fx.path("a"), b"aaa").unwrap();
    fs::write(fx.path("b"), b"bbb").unwrap();

    // rename-over-existing replaces the destination.
    fs::rename(fx.path("a"), fx.path("b")).unwrap();
    assert_eq!(fs::read(fx.path("b")).unwrap(), b"aaa");
    assert!(!fx.path("a").exists());

    // RENAME_NOREPLACE refuses an existing destination.
    fs::write(fx.path("c"), b"ccc").unwrap();
    let error = renameat2(&fx.path("c"), &fx.path("b"), RENAME_NOREPLACE).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
    assert_eq!(fs::read(fx.path("c")).unwrap(), b"ccc");
    assert_eq!(fs::read(fx.path("b")).unwrap(), b"aaa");

    // RENAME_NOREPLACE succeeds when the destination is absent.
    renameat2(&fx.path("c"), &fx.path("d"), RENAME_NOREPLACE).unwrap();
    assert_eq!(fs::read(fx.path("d")).unwrap(), b"ccc");

    // RENAME_EXCHANGE swaps two existing entries.
    fs::write(fx.path("e"), b"eee").unwrap();
    renameat2(&fx.path("d"), &fx.path("e"), RENAME_EXCHANGE).unwrap();
    assert_eq!(fs::read(fx.path("d")).unwrap(), b"eee");
    assert_eq!(fs::read(fx.path("e")).unwrap(), b"ccc");

    // Directory cases.
    fs::create_dir(fx.path("src")).unwrap();
    fs::create_dir(fx.path("empty")).unwrap();
    fs::rename(fx.path("src"), fx.path("empty")).unwrap();
    assert!(fx.path("empty").is_dir());
    assert!(!fx.path("src").exists());

    fs::create_dir(fx.path("nonempty")).unwrap();
    fs::write(fx.path("nonempty/x"), b"x").unwrap();
    fs::create_dir(fx.path("another")).unwrap();
    let error = fs::rename(fx.path("another"), fx.path("nonempty")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOTEMPTY));
    assert!(fx.path("another").is_dir());

    let error = fs::rename(fx.path("d"), fx.path("nonempty")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EISDIR));
    let error = fs::rename(fx.path("another"), fx.path("d")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));

    // Move a directory into its own descendant is EINVAL.
    fs::create_dir(fx.path("outer")).unwrap();
    fs::create_dir(fx.path("outer/inner")).unwrap();
    let error = fs::rename(fx.path("outer"), fx.path("outer/inner/moved")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn xattrs_through_syscalls_and_tools() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    let file = fx.path("xattr.txt");
    fs::write(&file, b"xattr body").unwrap();
    let path = cstring(&file);
    let name = CString::new("user.test").unwrap();
    let value = b"value-123";

    let rc = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };
    assert_eq!(
        rc,
        0,
        "setxattr failed: {}",
        std::io::Error::last_os_error()
    );

    let mut buffer = vec![0u8; 64];
    let rc = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len(),
        )
    };
    assert_eq!(rc as usize, value.len());
    assert_eq!(&buffer[..rc as usize], value);

    let rc = unsafe { libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
    assert!(rc > 0);
    let mut names = vec![0u8; rc as usize];
    let rc2 = unsafe { libc::listxattr(path.as_ptr(), names.as_mut_ptr() as *mut i8, names.len()) };
    assert_eq!(rc, rc2);
    assert_eq!(names.last(), Some(&0));
    assert!(String::from_utf8_lossy(&names).contains("user.test"));

    let rc = unsafe { libc::removexattr(path.as_ptr(), name.as_ptr()) };
    assert_eq!(rc, 0);
    let rc = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len(),
        )
    };
    assert_eq!(rc, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENODATA)
    );

    // The attr/getfattr tools use the same surface.
    let status = std::process::Command::new("setfattr")
        .args(["-n", "user.tool", "-v", "tool-value"])
        .arg(&file)
        .status()
        .expect("run setfattr");
    assert!(status.success());
    let output = std::process::Command::new("getfattr")
        .args(["--only-values", "-n", "user.tool"])
        .arg(&file)
        .output()
        .expect("run getfattr");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"tool-value");

    // Reserved names are refused.
    let reserved = CString::new("weft.secret").unwrap();
    let rc = unsafe {
        libc::setxattr(
            path.as_ptr(),
            reserved.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };
    assert_eq!(rc, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EINVAL)
    );
}

#[test]
fn lseek_fallocate_and_copy_file_range() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    let file = fx.path("seek.bin");
    fs::write(&file, b"0123456789").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&file)
        .unwrap();
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&handle);

    let data = unsafe { libc::lseek(fd, 2, libc::SEEK_DATA) };
    assert_eq!(data, 2);
    let hole = unsafe { libc::lseek(fd, 2, libc::SEEK_HOLE) };
    assert_eq!(hole, 10);
    let beyond = unsafe { libc::lseek(fd, 10, libc::SEEK_DATA) };
    assert_eq!(beyond, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENXIO)
    );

    // SEEK_SET reaches the daemon and is answered honestly.
    let set = unsafe { libc::lseek(fd, 4, libc::SEEK_SET) };
    assert_eq!(set, 4);

    // fallocate mode 0 zero-fills by extending the file.
    let rc = unsafe { libc::posix_fallocate(fd, 0, 4096) };
    assert_eq!(rc, 0);
    assert_eq!(fs::metadata(&file).unwrap().len(), 4096);
    let bytes = fs::read(&file).unwrap();
    assert_eq!(&bytes[..10], b"0123456789");
    assert!(bytes[10..].iter().all(|byte| *byte == 0));

    // The FUSE copy_file_range op answers ENOSYS. The kernel then falls back to
    // a read/write copy, so the syscall itself succeeds with the copied length:
    // no server-side copy is performed or claimed.
    let source = File::open(&file).unwrap();
    let target = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(fx.path("copy.bin"))
        .unwrap();
    let mut offset_in: i64 = 0;
    let mut offset_out: i64 = 0;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            std::os::unix::io::AsRawFd::as_raw_fd(&source),
            &mut offset_in as *mut i64,
            std::os::unix::io::AsRawFd::as_raw_fd(&target),
            &mut offset_out as *mut i64,
            16usize,
            0u32,
        )
    };
    assert_eq!(rc, 16, "kernel read/write fallback copies 16 bytes");
    // Confirm the fallback produced the right bytes.
    let copied = fs::read(fx.path("copy.bin")).unwrap();
    assert_eq!(copied, fs::read(&file).unwrap()[..16].to_vec());
}

#[test]
fn statfs_reports_state_derived_counters() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    fs::write(fx.path("one"), vec![7u8; 8192]).unwrap();
    fs::write(fx.path("two"), b"x").unwrap();

    let path = cstring(&fx.mount_dir);
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut stats) };
    assert_eq!(rc, 0, "statvfs failed: {}", std::io::Error::last_os_error());
    assert_eq!(stats.f_bsize as u64, 4096);
    assert_eq!(stats.f_frsize as u64, 4096);
    assert_eq!(stats.f_namemax as u64, 255);
    // Root + .weft virtual + two files + .weft/id + .weft/contexts are not all
    // objects; object count is root + two files = 3.
    assert!(stats.f_files >= 3, "files={}", stats.f_files);
    assert!(stats.f_blocks >= 2, "blocks={}", stats.f_blocks);
}

#[test]
fn snapshot_mount_shows_pinned_bytes_and_is_read_only() {
    let fx = Fixture::new();
    fx.weft_ok(&[
        std::ffi::OsStr::new("create"),
        std::ffi::OsStr::new("/v.txt"),
        std::ffi::OsStr::new("--text"),
        std::ffi::OsStr::new("old-bytes"),
    ]);
    fx.weft_ok(&[
        std::ffi::OsStr::new("snapshot"),
        std::ffi::OsStr::new("create"),
        std::ffi::OsStr::new("v1"),
    ]);
    fx.weft_ok(&[
        std::ffi::OsStr::new("write"),
        std::ffi::OsStr::new("/v.txt"),
        std::ffi::OsStr::new("--text"),
        std::ffi::OsStr::new("new-bytes"),
    ]);

    let live = fx.mount(&[]);
    assert_eq!(fs::read(fx.path("v.txt")).unwrap(), b"new-bytes");

    let snapshot_dir = fx.dir.path().join("snap");
    fs::create_dir(&snapshot_dir).unwrap();
    fs::set_permissions(&snapshot_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let snapshot = Mount::start(&fx.store, &snapshot_dir, &["--snapshot", "v1"], &[]);

    assert_eq!(fs::read(snapshot_dir.join("v.txt")).unwrap(), b"old-bytes");
    let error = fs::write(snapshot_dir.join("v.txt"), b"attack").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    let error = fs::write(snapshot_dir.join("new-file"), b"nope").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    let error = fs::create_dir(snapshot_dir.join("new-dir")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));

    // The live mount still sees the new bytes.
    assert_eq!(fs::read(fx.path("v.txt")).unwrap(), b"new-bytes");

    drop(snapshot);
    drop(live);
}

#[test]
fn exact_id_surface_and_saved_context_views() {
    let fx = Fixture::new();
    let os = std::ffi::OsStr::new;
    fx.weft_ok(&[os("create"), os("/alpha.txt"), os("--text"), os("alpha")]);
    fx.weft_ok(&[os("create"), os("/beta.txt"), os("--text"), os("beta!")]);
    fx.weft_ok(&[os("mkdir"), os("/folder")]);

    let id_of = |path: &str| -> String {
        let output = fx.weft_ok(&[os("stat"), os(path)]);
        output
            .lines()
            .find_map(|line| line.strip_prefix("object_id="))
            .expect("object_id in stat output")
            .to_string()
    };
    let alpha_id = id_of("/alpha.txt");
    let beta_id = id_of("/beta.txt");

    {
        use weftfs::store::{OpenMode, Store};
        let mut store = Store::open(&fx.store, OpenMode::ReadWrite).unwrap();
        let selector = weftcodec::selector::parse("select kind == \"file\"").unwrap();
        store.save_context("files", &selector).unwrap();
    }

    let _mount = fx.mount(&[]);

    // Exact-object surface: exact bytes and metadata.
    let alpha_view = fx.path(&format!(".weft/id/{alpha_id}"));
    assert_eq!(fs::read(&alpha_view).unwrap(), b"alpha");
    let meta = fs::metadata(&alpha_view).unwrap();
    assert_eq!(meta.len(), 5);
    assert!(meta.is_file());
    assert_eq!(
        fs::write(&alpha_view, b"tamper")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );
    let beta_view = fx.path(&format!(".weft/id/{beta_id}"));
    assert_eq!(fs::read(&beta_view).unwrap(), b"beta!");

    // Unknown id is absent, not aliased.
    let missing = fx.path(&format!(".weft/id/{}", "11".repeat(32)));
    assert_eq!(
        fs::metadata(&missing).unwrap_err().raw_os_error(),
        Some(libc::ENOENT)
    );

    // Saved context lists every matching object with deterministic names.
    let contexts = fx.path(".weft/contexts");
    let names: Vec<String> = fs::read_dir(&contexts)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["files".to_string()]);
    let context_dir = contexts.join("files");
    let mut entries: Vec<String> = fs::read_dir(&context_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries.len(), 2, "ambitious result sets are not collapsed");
    entries.sort();
    let first_pass = entries.clone();
    let mut second: Vec<String> = fs::read_dir(&context_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    second.sort();
    assert_eq!(first_pass, second, "names are deterministic");

    let mut seen = Vec::new();
    for name in &entries {
        assert!(
            name.ends_with(&format!("--{}", &alpha_id[..8]))
                || name.ends_with(&format!("--{}", &beta_id[..8]))
        );
        let path = context_dir.join(name);
        let bytes = fs::read(&path).unwrap();
        seen.push(bytes.clone());
        // lookup and readdir agree: the same path stats as a file.
        assert!(fs::metadata(&path).unwrap().is_file());
        if bytes == b"alpha" {
            assert!(name.starts_with("alpha.txt"));
        } else {
            assert_eq!(bytes, b"beta!");
            assert!(name.starts_with("beta.txt"));
        }
    }
    seen.sort();
    assert_eq!(seen, vec![b"alpha".to_vec(), b"beta!".to_vec()]);

    // Context writes are refused. Opening an existing entry reaches the
    // daemon and returns EROFS; creating a new name in a 0555 virtual
    // directory is rejected by the kernel's default_permissions first (EACCES).
    let target = context_dir.join(&entries[0]);
    let error = fs::write(&target, b"nope").unwrap_err().raw_os_error();
    assert!(
        error == Some(libc::EROFS) || error == Some(libc::EACCES),
        "unexpected errno {error:?}"
    );
    let error = fs::write(contexts.join("new"), b"nope")
        .unwrap_err()
        .raw_os_error();
    assert!(
        error == Some(libc::EROFS) || error == Some(libc::EACCES),
        "unexpected errno {error:?}"
    );
}

#[test]
fn byte_names_are_preserved_and_ordered() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    let names: [&[u8]; 4] = [b"z", b"a", b"caf\xe9", b"\xff\xfe"];
    for name in names {
        let path = fx.mount_dir.join(std::ffi::OsStr::from_bytes(name));
        fs::write(&path, name).unwrap();
    }
    let listed: Vec<Vec<u8>> = fs::read_dir(&fx.mount_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_vec())
        .collect();
    let mut expected: Vec<Vec<u8>> = names.iter().map(|name| name.to_vec()).collect();
    expected.sort();
    // `.weft` (0x2e) sorts before all these names; the kernel preserves the
    // order the daemon produced.
    let without_virtual: Vec<Vec<u8>> =
        listed.into_iter().filter(|name| name != b".weft").collect();
    assert_eq!(without_virtual, expected, "raw byte order is deterministic");

    // Non-UTF-8 names are readable and writable through the mount.
    let odd = fx.mount_dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    assert_eq!(fs::read(&odd).unwrap(), b"caf\xe9");
    let all: Vec<Vec<u8>> = names.iter().map(|name| name.to_vec()).collect();
    for name in all {
        let path = fx.mount_dir.join(std::ffi::OsStr::from_bytes(&name));
        fs::remove_file(path).unwrap();
    }
}

#[test]
#[ignore = "the fuse court runs with cap-drop=ALL (no CAP_SETUID), so setpriv cannot switch to uid 65534; kernel default_permissions ownership checks need a court with CAP_SETUID"]
fn permissions_with_allow_other() {
    let fx = Fixture::new();
    // The court runs with cap-drop=ALL (no CAP_CHOWN), so ownership is set
    // through the store CLI before mounting rather than through `chown(2)`.
    let os = std::ffi::OsStr::new;
    fx.weft_ok(&[
        os("create"),
        os("/private.txt"),
        os("--text"),
        os("secret"),
        os("--mode"),
        os("0600"),
        os("--uid"),
        os("1000"),
        os("--gid"),
        os("1000"),
    ]);
    fx.weft_ok(&[
        os("create"),
        os("/public.txt"),
        os("--text"),
        os("visible"),
        os("--mode"),
        os("0644"),
        os("--uid"),
        os("1000"),
        os("--gid"),
        os("1000"),
    ]);

    let _mount = fx.mount(&["--allow-other"]);
    let private = fx.path("private.txt");
    let public = fx.path("public.txt");
    assert_eq!(fs::metadata(&private).unwrap().uid(), 1000);
    assert_eq!(fs::metadata(&private).unwrap().gid(), 1000);
    assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&public).unwrap().mode() & 0o777, 0o644);

    let mountpoint = fx.mount_dir.to_string_lossy().to_string();
    let denied = std::process::Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "cat"])
        .arg(&private)
        .output()
        .expect("run setpriv cat (private)");
    assert!(
        !denied.status.success(),
        "0600 file must not be readable by nobody"
    );
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("Permission denied"),
        "stderr: {}",
        String::from_utf8_lossy(&denied.stderr)
    );

    let allowed = std::process::Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "cat"])
        .arg(&public)
        .output()
        .expect("run setpriv cat (public)");
    assert!(
        allowed.status.success(),
        "0644 file must be readable by nobody: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(allowed.stdout, b"visible");

    let listing = std::process::Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "ls"])
        .arg(&mountpoint)
        .output()
        .expect("run setpriv ls");
    assert!(
        listing.status.success(),
        "mountpoint must be traversable: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
}
