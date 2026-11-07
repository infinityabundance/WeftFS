//! The `unsafe` syscall boundary of the persistent checkpoint service.
//!
//! Everything else in `src/checkpoint_service.rs` is safe Rust. This file
//! exists because three operations have no safe `std` wrapper on stable Rust:
//!
//! * receiving a file descriptor with `recvmsg(2)` and `SCM_RIGHTS`
//!   (`std::os::unix::net::UnixStream` exposes no ancillary-data API);
//! * sending a descriptor with `sendmsg(2)`;
//! * `fstat(2)`/`pwrite(2)` on a borrowed descriptor.
//!
//! The invariants are:
//!
//! * every buffer pointer comes from a live slice for the duration of the call;
//! * every `msghdr`/`iovec` is zeroed and fully initialised before use;
//! * a descriptor returned by the kernel is wrapped in exactly one `OwnedFd`
//!   and closed exactly once;
//! * a descriptor passed to `sendmsg` is borrowed for the call only.
//!
//! This module is the second (and last) `unsafe` island in the crate, after
//! `threefs::ffi`; `tests/threefs.rs` enforces that list.

#![allow(unsafe_code)]

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Take ownership of an inherited, already-connected unix stream descriptor.
pub fn unix_stream_from_fd(fd: RawFd) -> io::Result<UnixStream> {
    if fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor is negative",
        ));
    }
    // SAFETY: the caller passed this descriptor to the helper process and
    // declares it is a connected unix stream; only `UnixStream` will close it.
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// One received descriptor window: bytes read plus any descriptors.
pub struct Received {
    pub bytes: usize,
    pub fds: Vec<OwnedFd>,
}

/// `recvmsg(2)` into `buf`, collecting `SCM_RIGHTS` descriptors when
/// `allow_fds` is true. Interrupted calls surface as `ErrorKind::Interrupted`.
pub fn recv_with_fds(fd: RawFd, buf: &mut [u8], allow_fds: bool) -> io::Result<Received> {
    let mut iovec = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // Capacity for one descriptor; `CMSG_SPACE(sizeof(int))` is 24 bytes on
    // linux-gnu. `[u64; 8]` gives the alignment the control macros require.
    let mut control = [0u64; 8];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    if allow_fds {
        message.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        message.msg_controllen = mem::size_of_val(&control) as _;
    }
    let rc = unsafe { libc::recvmsg(fd, &mut message, libc::MSG_CMSG_CLOEXEC) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    if allow_fds {
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&message) };
        while !cmsg.is_null() {
            let header = unsafe { &*cmsg };
            if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
                let base = unsafe { libc::CMSG_LEN(0) } as usize;
                if (header.cmsg_len as usize) >= base {
                    let data_len = header.cmsg_len as usize - base;
                    let count = data_len / mem::size_of::<RawFd>();
                    let data = unsafe { libc::CMSG_DATA(cmsg) } as *const RawFd;
                    for index in 0..count {
                        let received = unsafe { *data.add(index) };
                        if received >= 0 {
                            // SAFETY: the kernel installed this descriptor for
                            // this process and it has not been wrapped yet.
                            out.push(unsafe { OwnedFd::from_raw_fd(received) });
                        }
                    }
                }
            }
            cmsg = unsafe { libc::CMSG_NXTHDR(&message, cmsg) };
        }
    }
    Ok(Received {
        bytes: rc as usize,
        fds: out,
    })
}

/// `sendmsg(2)` the whole buffer, optionally attaching one descriptor.
pub fn send_with_fd(fd: RawFd, buf: &[u8], pass: Option<RawFd>) -> io::Result<usize> {
    let mut iovec = libc::iovec {
        iov_base: buf.as_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut control = [0u64; 8];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    if let Some(pass) = pass {
        message.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        message.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as _;
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if cmsg.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control buffer too small for one descriptor",
            ));
        }
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32) as _;
            std::ptr::write(libc::CMSG_DATA(cmsg) as *mut RawFd, pass);
        }
    }
    let rc = unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as usize)
}

/// `fstat(2)`: the descriptor's size in bytes.
pub fn size_of_fd(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut stat: libc::stat = unsafe { mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.st_size.max(0) as u64)
}

/// `pread(2)` at an explicit offset into `buf`.
pub fn read_at(fd: BorrowedFd<'_>, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let rc = unsafe {
        libc::pread(
            fd.as_raw_fd(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
            offset as libc::off_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as usize)
}

/// `pwrite(2)` the whole buffer at offset 0, independent of the shared file
/// offset, so a concurrent mapping in the caller sees the bytes.
pub fn write_all_at(fd: BorrowedFd<'_>, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        let rc = unsafe {
            libc::pwrite(
                fd.as_raw_fd(),
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
                written as libc::off_t,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "pwrite returned 0",
            ));
        }
        written += rc as usize;
    }
    Ok(())
}
