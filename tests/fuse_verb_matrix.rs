//! Rust FUSE verb matrix: fallocate modes, server-side copy_file_range,
//! mknod/FIFO rejection, O_TMPFILE boundary, mmap coherence, locking fallback,
//! and timestamp surface.
//!
//! The Go frontend implements the same verbs; the two deliberate Rust gaps are
//! binding limits in fuser 0.15.1 (no FUSE_TMPFILE dispatch and no `lk_flags`,
//! so flock cannot be distinguished from POSIX byte-range locks). Those
//! boundaries are asserted here rather than hidden.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::ptr;

use fuse_common::{cstring, Fixture};

fn fallocate(fd: i32, mode: i32, offset: i64, length: i64) -> std::io::Result<()> {
    let rc = unsafe { libc::fallocate(fd, mode, offset, length) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[test]
fn fallocate_all_modes_and_server_copy() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);

    // Mode 0: allocate extends with zeros, existing bytes untouched.
    let alloc = fx.path("alloc.bin");
    fs::write(&alloc, b"0123456789").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&alloc)
        .unwrap();
    fallocate(handle.as_raw_fd(), 0, 0, 4096).expect("fallocate mode 0");
    let bytes = fs::read(&alloc).unwrap();
    assert_eq!(bytes.len(), 4096);
    assert_eq!(&bytes[..10], b"0123456789");
    assert!(bytes[10..].iter().all(|b| *b == 0));

    // KEEP_SIZE: no size or content change on a dense file.
    fallocate(handle.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 8192, 4096)
        .expect("fallocate KEEP_SIZE");
    assert_eq!(fs::metadata(&alloc).unwrap().len(), 4096);
    drop(handle);

    // PUNCH_HOLE | KEEP_SIZE zeroes content but keeps the size.
    let punch = fx.path("punch.bin");
    fs::write(&punch, b"ABCDEFGH").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&punch)
        .unwrap();
    fallocate(
        handle.as_raw_fd(),
        libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
        2,
        3,
    )
    .expect("fallocate PUNCH_HOLE");
    assert_eq!(fs::read(&punch).unwrap(), b"AB\0\0\0FGH");
    assert_eq!(fs::metadata(&punch).unwrap().len(), 8);
    drop(handle);

    // ZERO_RANGE without KEEP_SIZE extends the file when the range is past EOF.
    let zero = fx.path("zero.bin");
    fs::write(&zero, b"ABCDEFGH").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&zero)
        .unwrap();
    fallocate(handle.as_raw_fd(), libc::FALLOC_FL_ZERO_RANGE, 4, 4).expect("fallocate ZERO_RANGE");
    assert_eq!(fs::read(&zero).unwrap(), b"ABCD\0\0\0\0");
    fallocate(handle.as_raw_fd(), libc::FALLOC_FL_ZERO_RANGE, 8, 4)
        .expect("fallocate ZERO_RANGE extends");
    assert_eq!(fs::read(&zero).unwrap(), b"ABCD\0\0\0\0\0\0\0\0");
    drop(handle);

    // COLLAPSE/INSERT: the store implements both. Kernel 7.3 rejects them
    // before forwarding with EOPNOTSUPP (also observed for the Go mount), so
    // assert the documented boundary or verify the content if forwarded.
    let collapse = fx.path("collapse.bin");
    fs::write(&collapse, b"ABCDEFGH").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&collapse)
        .unwrap();
    match fallocate(handle.as_raw_fd(), libc::FALLOC_FL_COLLAPSE_RANGE, 2, 2) {
        Ok(()) => assert_eq!(fs::read(&collapse).unwrap(), b"ABEFGH"),
        Err(err) => assert_eq!(
            err.raw_os_error(),
            Some(libc::EOPNOTSUPP),
            "unexpected COLLAPSE error: {err}"
        ),
    }
    drop(handle);

    let insert = fx.path("insert.bin");
    fs::write(&insert, b"ABCDEFGH").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&insert)
        .unwrap();
    match fallocate(handle.as_raw_fd(), libc::FALLOC_FL_INSERT_RANGE, 2, 2) {
        Ok(()) => assert_eq!(fs::read(&insert).unwrap(), b"AB\0\0CDEFGH"),
        Err(err) => assert_eq!(
            err.raw_os_error(),
            Some(libc::EOPNOTSUPP),
            "unexpected INSERT error: {err}"
        ),
    }
    drop(handle);

    // Invalid combinations are refused. The kernel pre-validates some flag
    // combinations before forwarding, so EOPNOTSUPP and EINVAL are both
    // correct refusals here; the store-level test checks the daemon's own
    // argument classification.
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&insert)
        .unwrap();
    let err = fallocate(handle.as_raw_fd(), libc::FALLOC_FL_PUNCH_HOLE, 0, 1).unwrap_err();
    assert!(
        matches!(
            err.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
        ),
        "unexpected PUNCH_HOLE-without-KEEP_SIZE error: {err}"
    );
    let err = fallocate(
        handle.as_raw_fd(),
        libc::FALLOC_FL_ZERO_RANGE | libc::FALLOC_FL_INSERT_RANGE,
        0,
        1,
    )
    .unwrap_err();
    assert!(
        matches!(
            err.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
        ),
        "unexpected combined-flag error: {err}"
    );
    drop(handle);

    // copy_file_range is served by the daemon, including a non-zero source
    // offset and destination offset.
    let source = fx.path("copy-src.bin");
    fs::write(&source, b"0123456789").unwrap();
    let dest = fx.path("copy-dst.bin");
    fs::write(&dest, b"xxxxxx").unwrap();
    let src = File::open(&source).unwrap();
    let dst = OpenOptions::new().write(true).open(&dest).unwrap();
    let mut off_in: i64 = 4;
    let mut off_out: i64 = 2;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            src.as_raw_fd(),
            &mut off_in as *mut i64,
            dst.as_raw_fd(),
            &mut off_out as *mut i64,
            4usize,
            0u32,
        )
    };
    assert_eq!(
        rc,
        4,
        "copy_file_range: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(off_in, 8);
    assert_eq!(off_out, 6);
    assert_eq!(fs::read(&dest).unwrap(), b"xx4567");
    drop((src, dst));

    // At EOF a short (zero-length) copy is not an error.
    let src = File::open(&source).unwrap();
    let dst = OpenOptions::new().write(true).open(&dest).unwrap();
    let mut off_in: i64 = 10;
    let mut off_out: i64 = 0;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            src.as_raw_fd(),
            &mut off_in as *mut i64,
            dst.as_raw_fd(),
            &mut off_out as *mut i64,
            4usize,
            0u32,
        )
    };
    assert_eq!(rc, 0);
}

#[test]
fn mknod_tmpfile_and_entry_flags() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);

    // Regular-file mknod succeeds.
    let regular = fx.path("regular-node");
    let path = cstring(&regular);
    let rc = unsafe { libc::mknod(path.as_ptr(), libc::S_IFREG | 0o600, 0) };
    assert_eq!(
        rc,
        0,
        "mknod regular failed: {}",
        std::io::Error::last_os_error()
    );
    assert!(regular.is_file());

    // FIFO, socket, and device nodes are not representable in v1 and must be
    // refused with EPERM, never silently degraded to a regular file.
    for (name, kind) in [
        ("fifo-node", libc::S_IFIFO),
        ("sock-node", libc::S_IFSOCK),
        ("chr-node", libc::S_IFCHR),
        ("blk-node", libc::S_IFBLK),
    ] {
        let target = fx.path(name);
        let target = cstring(&target);
        let rc = unsafe { libc::mknod(target.as_ptr(), kind | 0o600, 0) };
        assert_eq!(rc, -1, "{name} unexpectedly succeeded");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM),
            "{name} errno"
        );
    }

    // O_TMPFILE is a FUSE 7.37 operation. fuser 0.15.1 negotiates 7.31 and
    // has no dispatch for FUSE_TMPFILE, so the kernel fails it closed.
    let dir = cstring(&fx.mount_dir);
    let fd = unsafe { libc::open(dir.as_ptr(), libc::O_TMPFILE | libc::O_RDWR, 0o600) };
    assert_eq!(fd, -1, "O_TMPFILE unexpectedly succeeded");
    let errno = std::io::Error::last_os_error().raw_os_error();
    assert!(
        errno == Some(libc::EOPNOTSUPP) || errno == Some(libc::EISDIR),
        "O_TMPFILE boundary errno {errno:?}"
    );

    // O_EXCL through create, and O_TRUNC semantics.
    let excl = fx.path("excl");
    fs::write(&excl, b"x").unwrap();
    let err = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&excl)
        .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EEXIST));
    let handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&excl)
        .unwrap();
    drop(handle);
    assert_eq!(fs::metadata(&excl).unwrap().len(), 0);
}

#[test]
fn mmap_shared_read_write_coherence() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    let target = fx.path("mmap.bin");
    fs::write(&target, b"original").unwrap();

    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target)
        .unwrap();
    let len = 8usize;
    let addr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            handle.as_raw_fd(),
            0,
        )
    };
    assert_ne!(
        addr,
        libc::MAP_FAILED,
        "mmap failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        ptr::copy_nonoverlapping(b"modified".as_ptr(), addr as *mut u8, len);
    }
    let rc = unsafe { libc::msync(addr, len, libc::MS_SYNC) };
    assert_eq!(rc, 0, "msync failed: {}", std::io::Error::last_os_error());
    assert_eq!(fs::read(&target).unwrap(), b"modified");
    unsafe {
        libc::munmap(addr, len);
    }

    // A later write through a fresh file description is visible in a new
    // mapping. (A write(2) on the *same* description that held the mapping
    // can return EIO on kernel 7.3; see docs/fuse-status.md.)
    drop(handle);
    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target)
        .unwrap();
    handle.write_all(b"abcdefgh").unwrap();
    let mapped = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            handle.as_raw_fd(),
            0,
        )
    };
    assert_ne!(mapped, libc::MAP_FAILED);
    let mut view = [0u8; 8];
    unsafe {
        ptr::copy_nonoverlapping(mapped as *const u8, view.as_mut_ptr(), len);
        libc::munmap(mapped, len);
    }
    assert_eq!(&view, b"abcdefgh");
}

#[test]
fn lock_fallback_and_timestamp_surface() {
    let fx = Fixture::new();
    let _mount = fx.mount(&[]);
    let target = fx.path("locked");
    fs::write(&target, b"lock body").unwrap();

    // The Rust daemon answers getlk/setlk with ENOSYS (fuser 0.15.1 drops
    // lk_flags, so flock and POSIX locks cannot be separated). The kernel then
    // provides local persistent locking; two open descriptions still conflict.
    let first = File::open(&target).unwrap();
    let second = File::open(&target).unwrap();
    let rc = unsafe { libc::flock(first.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "flock first: {}", std::io::Error::last_os_error());
    let rc = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EWOULDBLOCK)
    );
    let rc = unsafe { libc::flock(first.as_raw_fd(), libc::LOCK_UN) };
    assert_eq!(rc, 0);
    let rc = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0);
    drop((first, second));

    // Timestamps: mtime is stored; atime/ctime are projections of mtime (v1
    // does not persist separate atime/ctime). A content write advances mtime
    // and ctime follows it.
    let before = fs::metadata(&target).unwrap();
    assert_eq!(before.ctime(), before.mtime());
    assert_eq!(before.ctime_nsec(), before.mtime_nsec());
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&target, b"lock body 2").unwrap();
    let after = fs::metadata(&target).unwrap();
    assert!(after.mtime() > before.mtime() || after.mtime_nsec() > before.mtime_nsec());
    assert_eq!(after.ctime(), after.mtime());
    assert_eq!(after.ctime_nsec(), after.mtime_nsec());
}
