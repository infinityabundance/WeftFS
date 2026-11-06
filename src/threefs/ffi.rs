//! The 3FS USRBIO C ABI boundary.
//!
//! This is the only module in the `weftfs` crate permitted to use `unsafe`
//! (the crate root is `#![deny(unsafe_code)]`, and this file opens with
//! `#![allow(unsafe_code)]`). Everything below this module is safe Rust.
//!
//! # What is being bound
//!
//! DeepSeek 3FS (Apache-2.0) exposes a user-space ring API so an application can
//! submit data I/O straight into the FUSE process's queue instead of issuing
//! one `read(2)`/`write(2)` per request. The declared surface, transcribed from
//! `src/lib/api/hf3fs_usrbio.h` upstream, is:
//!
//! ```c
//! int  hf3fs_iorcreate4(struct hf3fs_ior *, const char *mount, int entries,
//!                       bool for_read, int io_depth, int timeout, int numa,
//!                       uint64_t flags);
//! void hf3fs_iordestroy(struct hf3fs_ior *);
//! int  hf3fs_iovcreate(struct hf3fs_iov *, const char *mount, size_t size,
//!                      size_t block_size, int numa);
//! void hf3fs_iovdestroy(struct hf3fs_iov *);
//! int  hf3fs_reg_fd(int fd, uint64_t flags);   // <= 0 handle, > 0 errno
//! void hf3fs_dereg_fd(int fd);
//! int  hf3fs_prep_io(const struct hf3fs_ior *, const struct hf3fs_iov *,
//!                    bool read, void *ptr, int fd, size_t off, uint64_t len,
//!                    const void *userdata);
//! int  hf3fs_submit_ios(const struct hf3fs_ior *);
//! int  hf3fs_wait_for_ios(const struct hf3fs_ior *, struct hf3fs_cqe *cqes,
//!                         int cqec, int min_results,
//!                         const struct timespec *abs_timeout);
//! bool hf3fs_is_hf3fs(int fd);
//! int  hf3fs_extract_mount_point(char *mount, int size, const char *path);
//! ```
//!
//! The library is loaded with `dlopen`, never linked, so a build of WeftFS has
//! no 3FS dependency and refuses `usrbio` mode with a named missing library
//! rather than silently falling back to POSIX I/O.
//!
//! # Invariants this module is responsible for
//!
//! 1. **Struct layout.** [`Iov`], [`Ior`] and [`Cqe`] are `#[repr(C)]`
//!    transcriptions of the upstream structs. `const` assertions in this file
//!    pin `size_of`, `align_of` and the field offsets that the x86-64 C layout
//!    rules produce (312 / 600 / 24 bytes). The conformance shim asserts the
//!    same numbers from C, so a transcription error fails a test instead of
//!    corrupting memory.
//! 2. **Allocation ownership.** The upstream API never allocates the `ior` or
//!    `iov` struct itself ("the create functions will not allocate memory for
//!    it"). Both live in `Box`es owned by [`Usrbio`] and are destroyed exactly
//!    once, in `Drop`, after the rings and the data region.
//! 3. **Registration lifetime.** `hf3fs_reg_fd` registers a *live* `fd`; the
//!    upstream header warns the fd must not be closed while registered. Every
//!    registration is paired with a `dereg` + `close` in [`RegisteredFds`],
//!    whose `Drop` runs on the error path too.
//! 4. **Single-writer discipline.** `hf3fs_prep_io` is documented as *not*
//!    thread safe, and only one thread may prepare/submit while one
//!    (possibly different) thread waits. [`Usrbio::guard`] serialises every
//!    submission, so the `Send`/`Sync` impls below are sound.
//! 5. **Buffer containment.** `hf3fs_prep_io` requires `[ptr, ptr + len)` to be
//!    inside the `iov` passed with it. [`Usrbio::region_contains`] is the only
//!    way a destination pointer reaches the API; the direct path is taken only
//!    when the caller's buffer really is inside the shared region.
//! 6. **Bounded work.** Entry counts, per-request length, and batch bytes are
//!    validated before any pointer is handed to C. Nothing is sized from an
//!    untrusted on-disk value.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};

/// Environment variable naming an explicit `libhf3fs_api_shared.so`.
pub const LIBRARY_ENV: &str = "WEFT_3FS_LIB";
/// Soname the 3FS client installs.
pub const DEFAULT_LIBRARY: &str = "libhf3fs_api_shared.so";

/// Hard bound on a single ring's entries.
pub const MAX_ENTRIES: usize = 4096;
/// Hard bound on one request's length.
pub const MAX_IO_BYTES: u64 = 32 << 20;
/// Hard bound on the shared data region.
pub const MAX_IOV_BYTES: usize = 1 << 30;

// ---------------------------------------------------------------------------
// ABI structs
// ---------------------------------------------------------------------------

/// `struct hf3fs_iov`. Size 312, align 8 on x86-64.
#[repr(C)]
pub struct Iov {
    pub base: *mut u8,
    pub iovh: *mut c_void,
    pub id: [u8; 16],
    pub mount_point: [u8; 256],
    pub size: usize,
    pub block_size: usize,
    pub numa: c_int,
}

/// `struct hf3fs_ior`. Size 600, align 8 on x86-64.
#[repr(C)]
pub struct Ior {
    pub iov: Iov,
    pub iorh: *mut c_void,
    pub mount_point: [u8; 256],
    pub for_read: bool,
    pub io_depth: c_int,
    pub priority: c_int,
    pub timeout: c_int,
    pub flags: u64,
}

/// `struct hf3fs_cqe`. Size 24, align 8.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Cqe {
    pub index: i32,
    pub reserved: i32,
    pub result: i64,
    pub userdata: *const c_void,
}

const _: () = {
    assert!(std::mem::size_of::<Iov>() == 312);
    assert!(std::mem::align_of::<Iov>() == 8);
    assert!(std::mem::offset_of!(Iov, base) == 0);
    assert!(std::mem::offset_of!(Iov, iovh) == 8);
    assert!(std::mem::offset_of!(Iov, id) == 16);
    assert!(std::mem::offset_of!(Iov, mount_point) == 32);
    assert!(std::mem::offset_of!(Iov, size) == 288);
    assert!(std::mem::offset_of!(Iov, block_size) == 296);
    assert!(std::mem::offset_of!(Iov, numa) == 304);

    assert!(std::mem::size_of::<Ior>() == 600);
    assert!(std::mem::align_of::<Ior>() == 8);
    assert!(std::mem::offset_of!(Ior, iorh) == 312);
    assert!(std::mem::offset_of!(Ior, mount_point) == 320);
    assert!(std::mem::offset_of!(Ior, for_read) == 576);
    assert!(std::mem::offset_of!(Ior, io_depth) == 580);
    assert!(std::mem::offset_of!(Ior, priority) == 584);
    assert!(std::mem::offset_of!(Ior, timeout) == 588);
    assert!(std::mem::offset_of!(Ior, flags) == 592);

    assert!(std::mem::size_of::<Cqe>() == 24);
    assert!(std::mem::align_of::<Cqe>() == 8);
    assert!(std::mem::offset_of!(Cqe, index) == 0);
    assert!(std::mem::offset_of!(Cqe, reserved) == 4);
    assert!(std::mem::offset_of!(Cqe, result) == 8);
    assert!(std::mem::offset_of!(Cqe, userdata) == 16);
};

// ---------------------------------------------------------------------------
// Dynamic loading
// ---------------------------------------------------------------------------

type IorCreate4 =
    unsafe extern "C" fn(*mut Ior, *const c_char, c_int, bool, c_int, c_int, c_int, u64) -> c_int;
type IorDestroy = unsafe extern "C" fn(*mut Ior);
type IovCreate = unsafe extern "C" fn(*mut Iov, *const c_char, usize, usize, c_int) -> c_int;
type IovDestroy = unsafe extern "C" fn(*mut Iov);
type RegFd = unsafe extern "C" fn(c_int, u64) -> c_int;
type DeregFd = unsafe extern "C" fn(c_int);
type PrepIo = unsafe extern "C" fn(
    *const Ior,
    *const Iov,
    bool,
    *mut c_void,
    c_int,
    usize,
    u64,
    *const c_void,
) -> c_int;
type SubmitIos = unsafe extern "C" fn(*const Ior) -> c_int;
type WaitForIos =
    unsafe extern "C" fn(*const Ior, *mut Cqe, c_int, c_int, *const libc::timespec) -> c_int;
type IsHf3fs = unsafe extern "C" fn(c_int) -> bool;
type ExtractMountPoint = unsafe extern "C" fn(*mut c_char, c_int, *const c_char) -> c_int;

struct Symbols {
    iorcreate4: IorCreate4,
    iordestroy: IorDestroy,
    iovcreate: IovCreate,
    iovdestroy: IovDestroy,
    reg_fd: RegFd,
    dereg_fd: DeregFd,
    prep_io: PrepIo,
    submit_ios: SubmitIos,
    wait_for_ios: WaitForIos,
    is_hf3fs: IsHf3fs,
    extract_mount_point: ExtractMountPoint,
}

/// An opened `libhf3fs_api_shared.so`.
pub struct Library {
    handle: *mut c_void,
    symbols: Symbols,
    path: PathBuf,
}

// The handle is an opaque process-global object with no thread affinity; the
// function pointers are pure code. Shared use is serialised by `Usrbio`.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Open the library at `path` (or from `WEFT_3FS_LIB`).
    pub fn open(path: Option<&Path>) -> Result<Library> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => match std::env::var_os(LIBRARY_ENV) {
                Some(value) => PathBuf::from(value),
                None => PathBuf::from(DEFAULT_LIBRARY),
            },
        };
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
            Error::invalid_argument(format!("3FS library path {:?} contains a NUL", path))
        })?;
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
        // call. RTLD_NOW resolves every symbol we need before returning, and
        // RTLD_LOCAL keeps the 3FS symbols out of the global namespace.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            let reason = dl_error();
            return Err(Error::unsupported_version(format!(
                "cannot load the 3FS USRBIO library `{}`: {reason}; set {LIBRARY_ENV} to a \
                 libhf3fs_api_shared.so built for this host",
                path.display()
            )));
        }
        let library = Library {
            handle,
            symbols: Symbols::resolve(handle, &path)?,
            path,
        };
        Ok(library)
    }

    /// Path this library was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `hf3fs_extract_mount_point`: the 3FS mount point containing `path`.
    pub fn extract_mount_point(&self, path: &Path) -> Result<Option<PathBuf>> {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::invalid_argument("path contains a NUL"))?;
        let mut buf = vec![0u8; 4096];
        // SAFETY: buf is a writable 4096-byte buffer and we pass its true size.
        let rc = unsafe {
            (self.symbols.extract_mount_point)(
                buf.as_mut_ptr() as *mut c_char,
                4096,
                c_path.as_ptr(),
            )
        };
        if rc <= 0 {
            return Ok(None);
        }
        let len = (rc as usize).saturating_sub(1).min(buf.len());
        // SAFETY: the C function filled `len` bytes and the rest stay zero.
        let text = unsafe { CStr::from_bytes_with_nul_unchecked(&buf[..len + 1]) };
        Ok(Some(PathBuf::from(text.to_string_lossy().into_owned())))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from `dlopen` and is dropped exactly once.
        // Rings and regions have already been destroyed by `Usrbio::drop`.
        unsafe {
            libc::dlclose(self.handle);
        }
    }
}

fn dl_error() -> String {
    // SAFETY: dlerror returns a NUL-terminated string in thread-local storage,
    // or NULL when there is no error. The pointer is only read here.
    unsafe {
        let raw = libc::dlerror();
        if raw.is_null() {
            "dlopen failed with no dlerror() text".to_owned()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    }
}

impl Symbols {
    fn resolve(handle: *mut c_void, path: &Path) -> Result<Symbols> {
        let mut missing = Vec::new();
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let c_name = CString::new($name).expect("symbol name has no NUL");
                // SAFETY: `handle` came from `dlopen` and `c_name` is a valid
                // NUL-terminated string. The caller of this module asserts the
                // name matches the signature above; a mismatch is a programming
                // error pinned by the shim's C-side declarations.
                let raw = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
                if raw.is_null() {
                    missing.push($name);
                    None
                } else {
                    // SAFETY: a resolved address for a function symbol is
                    // copied into the matching `extern "C"` pointer type.
                    Some(unsafe { std::mem::transmute_copy::<*mut c_void, $ty>(&raw) })
                }
            }};
        }
        let iorcreate4 = symbol!("hf3fs_iorcreate4", IorCreate4);
        let iordestroy = symbol!("hf3fs_iordestroy", IorDestroy);
        let iovcreate = symbol!("hf3fs_iovcreate", IovCreate);
        let iovdestroy = symbol!("hf3fs_iovdestroy", IovDestroy);
        let reg_fd = symbol!("hf3fs_reg_fd", RegFd);
        let dereg_fd = symbol!("hf3fs_dereg_fd", DeregFd);
        let prep_io = symbol!("hf3fs_prep_io", PrepIo);
        let submit_ios = symbol!("hf3fs_submit_ios", SubmitIos);
        let wait_for_ios = symbol!("hf3fs_wait_for_ios", WaitForIos);
        let is_hf3fs = symbol!("hf3fs_is_hf3fs", IsHf3fs);
        let extract_mount_point = symbol!("hf3fs_extract_mount_point", ExtractMountPoint);
        if !missing.is_empty() {
            return Err(Error::unsupported_version(format!(
                "`{}` is missing 3FS USRBIO symbols {missing:?}; it is not the 3FS client library",
                path.display()
            )));
        }
        Ok(Symbols {
            iorcreate4: iorcreate4.expect("checked"),
            iordestroy: iordestroy.expect("checked"),
            iovcreate: iovcreate.expect("checked"),
            iovdestroy: iovdestroy.expect("checked"),
            reg_fd: reg_fd.expect("checked"),
            dereg_fd: dereg_fd.expect("checked"),
            prep_io: prep_io.expect("checked"),
            submit_ios: submit_ios.expect("checked"),
            wait_for_ios: wait_for_ios.expect("checked"),
            is_hf3fs: is_hf3fs.expect("checked"),
            extract_mount_point: extract_mount_point.expect("checked"),
        })
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Pinned USRBIO configuration. Every value is validated before use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// 3FS mount point handed to `hf3fs_iorcreate4` / `hf3fs_iovcreate`.
    pub mount_point: PathBuf,
    /// Explicit library path; `None` uses `WEFT_3FS_LIB` or the soname.
    pub library: Option<PathBuf>,
    /// Ring entries.
    pub entries: usize,
    /// Shared data-region bytes.
    pub io_bytes: usize,
    /// `io_depth` argument (`0` = no batching control).
    pub io_depth: i32,
    /// `timeout` argument, milliseconds.
    pub timeout_ms: i32,
    /// NUMA node, `-1` = current.
    pub numa: i32,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            mount_point: PathBuf::from("/hf3fs/mount/point"),
            library: None,
            entries: 256,
            io_bytes: 8 << 20,
            io_depth: 0,
            timeout_ms: 0,
            numa: -1,
        }
    }
}

impl Options {
    /// Validate the bounds this module relies on.
    pub fn validate(&self) -> Result<()> {
        if self.entries == 0 || self.entries > MAX_ENTRIES {
            return Err(Error::invalid_argument(format!(
                "3FS USRBIO entries must be 1..={MAX_ENTRIES} (got {})",
                self.entries
            )));
        }
        if self.io_bytes < 64 * 1024 || self.io_bytes > MAX_IOV_BYTES {
            return Err(Error::invalid_argument(format!(
                "3FS USRBIO shared region must be 64 KiB..={MAX_IOV_BYTES} bytes (got {})",
                self.io_bytes
            )));
        }
        if self.io_depth < 0 || self.timeout_ms < 0 {
            return Err(Error::invalid_argument(
                "3FS USRBIO io_depth and timeout must not be negative settings this store cannot \
                 bound",
            ));
        }
        let mount = self.mount_point.to_string_lossy();
        if mount.len() >= 256 {
            return Err(Error::invalid_argument(format!(
                "3FS mount point path is {} bytes; the ABI field holds 255",
                mount.len()
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The transport
// ---------------------------------------------------------------------------

/// One batched read request.
#[derive(Clone, Copy, Debug)]
pub struct ReadRequest<'a> {
    pub path: &'a Path,
    pub offset: u64,
    pub len: u64,
}

/// Counters for receipts. Never used to size an allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub submissions: u64,
    pub requests: u64,
    pub completions: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub direct_reads: u64,
    pub staged_reads: u64,
    pub registrations: u64,
}

/// A live USRBIO read/write ring pair over one shared data region.
pub struct Usrbio {
    library: Library,
    iov: Box<Iov>,
    read_ring: Box<Ior>,
    write_ring: Box<Ior>,
    mount: CString,
    mount_path: PathBuf,
    entries: usize,
    io_bytes: usize,
    /// Serialises prep/submit against wait: the ABI allows one preparing thread
    /// and one waiting thread per ring, and we keep one client at a time.
    guard: Mutex<()>,
    counters: CountersAtomic,
    mount_is_3fs: Mutex<Option<bool>>,
}

struct CountersAtomic {
    submissions: AtomicU64,
    requests: AtomicU64,
    completions: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    direct_reads: AtomicU64,
    staged_reads: AtomicU64,
    registrations: AtomicU64,
}

impl CountersAtomic {
    fn new() -> CountersAtomic {
        CountersAtomic {
            submissions: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            completions: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            direct_reads: AtomicU64::new(0),
            staged_reads: AtomicU64::new(0),
            registrations: AtomicU64::new(0),
        }
    }
}

// SAFETY: `Usrbio` owns the `dlopen` handle, the two ring structures and the
// shared region. The ABI forbids concurrent `prep_io`/`submit_ios` on one ring
// and forbids concurrent `wait_for_ios`; every public method takes `guard`, so
// no two threads are ever inside the ABI at once. No method leaks a raw pointer
// to a caller.
unsafe impl Send for Usrbio {}
unsafe impl Sync for Usrbio {}

impl std::fmt::Debug for Usrbio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Usrbio")
            .field("library", &self.library.path())
            .field("mount_point", &self.mount_path)
            .field("entries", &self.entries)
            .field("io_bytes", &self.io_bytes)
            .field("mount_is_3fs", &self.cached_mount_is_3fs())
            .finish()
    }
}

impl Usrbio {
    /// Load the client library and create the region and both rings.
    pub fn open(options: &Options) -> Result<Usrbio> {
        options.validate()?;
        let library = Library::open(options.library.as_deref())?;
        let mount = CString::new(options.mount_point.to_string_lossy().as_bytes())
            .map_err(|_| Error::invalid_argument("3FS mount point contains a NUL byte"))?;

        let mut iov = Box::new(Iov {
            base: ptr::null_mut(),
            iovh: ptr::null_mut(),
            id: [0u8; 16],
            mount_point: [0u8; 256],
            size: 0,
            block_size: 0,
            numa: options.numa,
        });
        // SAFETY: `iov` is a live `#[repr(C)]` struct matching the ABI; `mount`
        // outlives the call and the returned region is owned by the library
        // until `hf3fs_iovdestroy`.
        let rc = unsafe {
            (library.symbols.iovcreate)(
                &mut *iov,
                mount.as_ptr(),
                options.io_bytes,
                0,
                options.numa,
            )
        };
        errno_result(rc, "hf3fs_iovcreate")?;
        if iov.base.is_null() || iov.size < options.io_bytes {
            // SAFETY: nothing else references the region yet.
            unsafe { (library.symbols.iovdestroy)(&mut *iov) };
            return Err(Error::io(
                "3FS hf3fs_iovcreate returned success but no usable shared region",
            ));
        }

        let mut read_ring = match make_ring(&library, &mount, options, true) {
            Ok(ring) => ring,
            Err(err) => {
                // SAFETY: the region is live and owned here.
                unsafe { (library.symbols.iovdestroy)(&mut *iov) };
                return Err(err);
            }
        };
        let write_ring = match make_ring(&library, &mount, options, false) {
            Ok(ring) => ring,
            Err(err) => {
                // SAFETY: both are live and owned here.
                unsafe {
                    (library.symbols.iordestroy)(&mut *read_ring);
                    (library.symbols.iovdestroy)(&mut *iov);
                }
                return Err(err);
            }
        };

        Ok(Usrbio {
            library,
            iov,
            read_ring,
            write_ring,
            mount,
            mount_path: options.mount_point.clone(),
            entries: options.entries,
            io_bytes: options.io_bytes,
            guard: Mutex::new(()),
            counters: CountersAtomic::new(),
            mount_is_3fs: Mutex::new(None),
        })
    }

    /// Library path actually loaded.
    pub fn library_path(&self) -> &Path {
        self.library.path()
    }

    /// 3FS mount point this transport was created for.
    pub fn mount_point(&self) -> &Path {
        &self.mount_path
    }

    /// The exact NUL-terminated mount argument handed to the rings.
    pub fn mount_argument(&self) -> &CStr {
        &self.mount
    }

    /// Shared-region base address, so a consumer can place buffers inside it
    /// and take the direct (no staging copy) path.
    pub fn region(&self) -> (*mut u8, usize) {
        (self.iov.base, self.io_bytes)
    }

    /// Whether `[ptr, ptr + len)` lies inside the shared region.
    pub fn region_contains(&self, ptr: *const u8, len: usize) -> bool {
        if len == 0 {
            return true;
        }
        let base = self.iov.base as usize;
        let end = match base.checked_add(self.io_bytes) {
            Some(end) => end,
            None => return false,
        };
        let start = ptr as usize;
        let stop = match start.checked_add(len) {
            Some(stop) => stop,
            None => return false,
        };
        start >= base && stop <= end
    }

    /// `hf3fs_is_hf3fs` for the mount point itself, cached after the first call.
    /// `false` means the library is loaded but the path is not served by 3FS
    /// (for example the conformance shim), and receipts must say so.
    pub fn cached_mount_is_3fs(&self) -> Option<bool> {
        *self.mount_is_3fs.lock().expect("mount flag lock")
    }

    /// `hf3fs_is_hf3fs` for the mount point itself. `false` means the library is
    /// loaded but the path is not served by 3FS (for example the conformance
    /// shim), and receipts must say so.
    pub fn mount_is_3fs(&self) -> Result<bool> {
        if let Some(known) = *self.mount_is_3fs.lock().expect("mount flag lock") {
            return Ok(known);
        }
        let _guard = self.guard.lock().expect("usrbio guard");
        let fd = std::fs::File::open(self.mount_point())
            .map(|file| std::os::unix::io::IntoRawFd::into_raw_fd(file));
        let value = match fd {
            Ok(fd) => {
                // SAFETY: `fd` is a live descriptor opened just above.
                let value = unsafe { (self.library.symbols.is_hf3fs)(fd) };
                // SAFETY: closing the descriptor we opened.
                unsafe { libc::close(fd) };
                value
            }
            Err(_) => false,
        };
        *self.mount_is_3fs.lock().expect("mount flag lock") = Some(value);
        Ok(value)
    }

    /// Snapshot the counters.
    pub fn counters(&self) -> Counters {
        let c = &self.counters;
        Counters {
            submissions: c.submissions.load(Ordering::Relaxed),
            requests: c.requests.load(Ordering::Relaxed),
            completions: c.completions.load(Ordering::Relaxed),
            bytes_read: c.bytes_read.load(Ordering::Relaxed),
            bytes_written: c.bytes_written.load(Ordering::Relaxed),
            direct_reads: c.direct_reads.load(Ordering::Relaxed),
            staged_reads: c.staged_reads.load(Ordering::Relaxed),
            registrations: c.registrations.load(Ordering::Relaxed),
        }
    }

    /// Read many ranges with one submission per ring-full.
    ///
    /// Output order is the input order. A request starting past end of file is
    /// `InvalidArgument`; a request that runs past end of file is truncated,
    /// matching [`crate::planes::BlobPlane::get_range`].
    pub fn read_batch(&self, requests: &[ReadRequest<'_>]) -> Result<Vec<Vec<u8>>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        for request in requests {
            if request.len > MAX_IO_BYTES {
                return Err(Error::limit_exceeded(format!(
                    "3FS USRBIO request of {} bytes exceeds the {MAX_IO_BYTES}-byte bound",
                    request.len
                )));
            }
        }
        let mut out: Vec<Option<Vec<u8>>> = Vec::with_capacity(requests.len());
        for _ in 0..requests.len() {
            out.push(None);
        }

        let mut start = 0usize;
        while start < requests.len() {
            // Collect one ring-full: at most `entries` requests and at most
            // `io_bytes` of shared region.
            let mut group: Vec<usize> = Vec::new();
            let mut region_used = 0usize;
            let mut cursor = start;
            while cursor < requests.len() && group.len() < self.entries {
                let len = usize::try_from(requests[cursor].len).map_err(|_| {
                    Error::limit_exceeded("3FS request length does not fit the address space")
                })?;
                if !group.is_empty() && region_used + len > self.io_bytes {
                    break;
                }
                if len > self.io_bytes {
                    return Err(Error::limit_exceeded(format!(
                        "3FS USRBIO request of {len} bytes exceeds the {}-byte shared region",
                        self.io_bytes
                    )));
                }
                region_used += len;
                group.push(cursor);
                cursor += 1;
            }
            if group.is_empty() {
                return Err(Error::limit_exceeded(
                    "3FS USRBIO could not make progress on an empty ring group",
                ));
            }
            self.run_group(requests, &group, region_used, &mut out)?;
            start = cursor;
        }

        Ok(out
            .into_iter()
            .map(|entry| entry.unwrap_or_default())
            .collect())
    }

    fn run_group(
        &self,
        requests: &[ReadRequest<'_>],
        group: &[usize],
        region_used: usize,
        out: &mut [Option<Vec<u8>>],
    ) -> Result<()> {
        let _guard = self.guard.lock().expect("usrbio guard");
        // Resolve the effective length of every request *before* touching the
        // ring, so an invalid offset fails without a partial submission.
        let mut effective = Vec::with_capacity(group.len());
        for &index in group {
            let request = requests[index];
            let path = request.path;
            let meta = std::fs::metadata(path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => {
                    Error::not_found(format!("3FS: {} does not exist: {err}", path.display()))
                }
                _ => Error::io(format!("3FS: cannot stat {}: {err}", path.display())),
            })?;
            let total = meta.len();
            if request.offset > total {
                return Err(Error::invalid_argument(format!(
                    "3FS: read offset {} is beyond {} ({total} bytes)",
                    request.offset,
                    path.display()
                )));
            }
            let want = (total - request.offset).min(request.len);
            effective.push(want);
        }

        let mut registered = RegisteredFds::new(&self.library, &self.counters);
        let mut slots: Vec<usize> = Vec::with_capacity(group.len());
        let mut offset_in_region = 0usize;
        for (position, &index) in group.iter().enumerate() {
            let want = effective[position];
            slots.push(offset_in_region);
            if want == 0 {
                continue;
            }
            let fd = std::fs::File::open(requests[index].path)
                .map_err(|err| {
                    Error::io(format!(
                        "3FS: cannot open {}: {err}",
                        requests[index].path.display()
                    ))
                })?
                .into_raw_fd();
            let handle = registered.register(fd)?;
            let ptr = self.region_ptr(offset_in_region)?;
            // SAFETY: `ptr` is inside the shared region by construction, the
            // handle came from `hf3fs_reg_fd`, and `want` is bounded by the
            // region and MAX_IO_BYTES.
            let rc = unsafe {
                (self.library.symbols.prep_io)(
                    &*self.read_ring,
                    &*self.iov,
                    true,
                    ptr as *mut c_void,
                    handle,
                    usize::try_from(requests[index].offset).map_err(|_| {
                        Error::limit_exceeded("3FS offset does not fit the address space")
                    })?,
                    want,
                    index as usize as *const c_void,
                )
            };
            if rc < 0 {
                return Err(errno_error(rc, "hf3fs_prep_io (read)"));
            }
            offset_in_region += usize::try_from(want).unwrap_or(0);
        }
        debug_assert!(offset_in_region <= region_used);

        // SAFETY: the ring is live and `prep_io` calls above were serialised.
        let rc = unsafe { (self.library.symbols.submit_ios)(&*self.read_ring) };
        if rc < 0 {
            return Err(errno_error(rc, "hf3fs_submit_ios (read)"));
        }
        self.counters.submissions.fetch_add(1, Ordering::Relaxed);
        self.counters
            .requests
            .fetch_add(group.len() as u64, Ordering::Relaxed);

        let pending: usize = group
            .iter()
            .enumerate()
            .filter(|(position, _)| effective[*position] > 0)
            .count();
        let mut completed: Vec<Option<(usize, i64)>> = vec![None; group.len()];
        let mut remaining = pending;
        while remaining > 0 {
            let mut cqes = vec![
                Cqe {
                    index: 0,
                    reserved: 0,
                    result: 0,
                    userdata: ptr::null(),
                };
                remaining
            ];
            // SAFETY: `cqes` is a live array of `remaining` correctly-laid-out
            // CQEs; a null `abs_timeout` means "wait".
            let got = unsafe {
                (self.library.symbols.wait_for_ios)(
                    &*self.read_ring,
                    cqes.as_mut_ptr(),
                    remaining as c_int,
                    remaining as c_int,
                    ptr::null(),
                )
            };
            if got < 0 {
                return Err(errno_error(got, "hf3fs_wait_for_ios (read)"));
            }
            if got == 0 {
                continue;
            }
            for cqe in cqes.iter().take(got as usize) {
                let index = cqe.userdata as usize;
                let position = group
                    .iter()
                    .position(|value| *value == index)
                    .ok_or_else(|| Error::corrupt("3FS: completion for an unknown request"))?;
                completed[position] = Some((index, cqe.result));
                remaining = remaining.saturating_sub(1);
            }
        }
        drop(registered);

        for (position, &index) in group.iter().enumerate() {
            let want = effective[position];
            if want == 0 {
                let _ = index;
                out[index] = Some(Vec::new());
                continue;
            }
            let (_, result) = completed[position]
                .ok_or_else(|| Error::corrupt("3FS: request completed without a CQE"))?;
            if result < 0 {
                return Err(errno_error(result as c_int, "3FS read completion"));
            }
            let got = (result as u64).min(want);
            let start = slots[position];
            let ptr = self.region_ptr(start)?;
            // SAFETY: the region is live, the completion reported `got` bytes at
            // `start`, and `got <= want <= region`.
            let bytes = unsafe {
                std::slice::from_raw_parts(ptr as *const u8, usize::try_from(got).unwrap_or(0))
            }
            .to_vec();
            self.counters.completions.fetch_add(1, Ordering::Relaxed);
            self.counters.bytes_read.fetch_add(got, Ordering::Relaxed);
            out[index] = Some(bytes);
        }
        Ok(())
    }

    /// Read into a caller-owned buffer. When `buf` lies inside the shared
    /// region the read lands directly in it (no staging copy); otherwise the
    /// bytes are staged through the region and copied once.
    pub fn read_into(&self, path: &Path, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let total = std::fs::metadata(path)
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => {
                    Error::not_found(format!("3FS: {} does not exist: {err}", path.display()))
                }
                _ => Error::io(format!("3FS: cannot stat {}: {err}", path.display())),
            })?
            .len();
        if offset > total {
            return Err(Error::invalid_argument(format!(
                "3FS: read offset {offset} is beyond {} ({total} bytes)",
                path.display()
            )));
        }
        let wanted = (total - offset).min(buf.len() as u64);
        if wanted == 0 {
            return Ok(0);
        }
        if wanted > MAX_IO_BYTES {
            return Err(Error::limit_exceeded(format!(
                "3FS USRBIO read of {wanted} bytes exceeds the {MAX_IO_BYTES}-byte bound"
            )));
        }
        let direct = self.region_contains(buf.as_ptr(), wanted as usize);
        let direct_buf = buf;
        let _guard = self.guard.lock().expect("usrbio guard");
        let fd = std::fs::File::open(path)
            .map_err(|err| Error::io(format!("3FS: cannot open {}: {err}", path.display())))?
            .into_raw_fd();
        let mut registered = RegisteredFds::new(&self.library, &self.counters);
        let handle = registered.register(fd)?;

        let (ptr, stage): (*mut u8, Option<*mut u8>) = if direct {
            (direct_buf.as_mut_ptr(), None)
        } else {
            let ptr = self.region_ptr(0)?;
            (ptr, Some(ptr))
        };
        // SAFETY: `ptr` is either the caller's buffer (proved to be inside the
        // region) or the region base; `wanted` fits both.
        let rc = unsafe {
            (self.library.symbols.prep_io)(
                &*self.read_ring,
                &*self.iov,
                true,
                ptr as *mut c_void,
                handle,
                usize::try_from(offset).map_err(|_| {
                    Error::limit_exceeded("3FS offset does not fit the address space")
                })?,
                wanted,
                ptr::null(),
            )
        };
        if rc < 0 {
            return Err(errno_error(rc, "hf3fs_prep_io (read_into)"));
        }
        // SAFETY: live ring, serialised submission.
        let rc = unsafe { (self.library.symbols.submit_ios)(&*self.read_ring) };
        if rc < 0 {
            return Err(errno_error(rc, "hf3fs_submit_ios (read_into)"));
        }
        self.counters.submissions.fetch_add(1, Ordering::Relaxed);
        self.counters.requests.fetch_add(1, Ordering::Relaxed);

        let mut cqe = Cqe {
            index: 0,
            reserved: 0,
            result: 0,
            userdata: ptr::null(),
        };
        // SAFETY: one live CQE.
        let got = unsafe {
            (self.library.symbols.wait_for_ios)(&*self.read_ring, &mut cqe, 1, 1, ptr::null())
        };
        if got < 0 {
            return Err(errno_error(got, "hf3fs_wait_for_ios (read_into)"));
        }
        if got == 0 {
            return Err(Error::io("3FS: wait_for_ios returned no completion"));
        }
        drop(registered);
        if cqe.result < 0 {
            return Err(errno_error(cqe.result as c_int, "3FS read_into completion"));
        }
        let produced = (cqe.result as u64).min(wanted) as usize;
        if let Some(stage) = stage {
            // SAFETY: the staged read wrote `produced` bytes at the region base.
            let bytes = unsafe { std::slice::from_raw_parts(stage as *const u8, produced) };
            direct_buf[..produced].copy_from_slice(bytes);
            self.counters.staged_reads.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.direct_reads.fetch_add(1, Ordering::Relaxed);
        }
        self.counters.completions.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_read
            .fetch_add(produced as u64, Ordering::Relaxed);
        Ok(produced)
    }

    /// Write into an already-open file through the write ring.
    ///
    /// Publication (tmp + fsync + `rename`) stays with the caller: this only
    /// replaces the `write(2)` that fills the staging file.
    pub fn write_all(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > self.io_bytes || bytes.len() as u64 > MAX_IO_BYTES {
            return Err(Error::limit_exceeded(format!(
                "3FS USRBIO write of {} bytes exceeds the bounded region",
                bytes.len()
            )));
        }
        let _guard = self.guard.lock().expect("usrbio guard");
        let fd = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|err| Error::io(format!("3FS: cannot open {}: {err}", path.display())))?
            .into_raw_fd();
        let mut registered = RegisteredFds::new(&self.library, &self.counters);
        let handle = registered.register(fd)?;
        let dst = self.region_ptr(0)?;
        // SAFETY: region base with `bytes.len()` <= region size.
        unsafe {
            std::slice::from_raw_parts_mut(dst, bytes.len()).copy_from_slice(bytes);
        }
        // SAFETY: `dst` is inside the region, the handle is registered, len is
        // bounded by the region.
        let rc = unsafe {
            (self.library.symbols.prep_io)(
                &*self.write_ring,
                &*self.iov,
                false,
                dst as *mut c_void,
                handle,
                0,
                bytes.len() as u64,
                ptr::null(),
            )
        };
        if rc < 0 {
            return Err(errno_error(rc, "hf3fs_prep_io (write)"));
        }
        // SAFETY: live ring, serialised submission.
        let rc = unsafe { (self.library.symbols.submit_ios)(&*self.write_ring) };
        if rc < 0 {
            return Err(errno_error(rc, "hf3fs_submit_ios (write)"));
        }
        self.counters.submissions.fetch_add(1, Ordering::Relaxed);
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let mut cqe = Cqe {
            index: 0,
            reserved: 0,
            result: 0,
            userdata: ptr::null(),
        };
        // SAFETY: one live CQE.
        let got = unsafe {
            (self.library.symbols.wait_for_ios)(&*self.write_ring, &mut cqe, 1, 1, ptr::null())
        };
        if got < 0 {
            return Err(errno_error(got, "hf3fs_wait_for_ios (write)"));
        }
        if got == 0 {
            return Err(Error::io("3FS: write ring returned no completion"));
        }
        drop(registered);
        if cqe.result < 0 {
            return Err(errno_error(cqe.result as c_int, "3FS write completion"));
        }
        if cqe.result as u64 != bytes.len() as u64 {
            return Err(Error::io(format!(
                "3FS: short write, {}/{} bytes",
                cqe.result,
                bytes.len()
            )));
        }
        self.counters.completions.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_written
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn region_ptr(&self, offset: usize) -> Result<*mut u8> {
        if offset > self.io_bytes {
            return Err(Error::limit_exceeded(format!(
                "3FS: region offset {offset} exceeds the {}-byte region",
                self.io_bytes
            )));
        }
        // SAFETY: base came from `hf3fs_iovcreate` and `offset` is inside it.
        let ptr = unsafe { self.iov.base.add(offset) };
        Ok(ptr)
    }
}

impl Drop for Usrbio {
    fn drop(&mut self) {
        // SAFETY: each object was created by the matching create call and is
        // destroyed exactly once here, rings first (they can reference the
        // region), then the region.
        unsafe {
            (self.library.symbols.iordestroy)(&mut *self.read_ring);
            (self.library.symbols.iordestroy)(&mut *self.write_ring);
            (self.library.symbols.iovdestroy)(&mut *self.iov);
        }
    }
}

fn make_ring(
    library: &Library,
    mount: &CString,
    options: &Options,
    for_read: bool,
) -> Result<Box<Ior>> {
    let mut ring = Box::new(Ior {
        iov: Iov {
            base: ptr::null_mut(),
            iovh: ptr::null_mut(),
            id: [0u8; 16],
            mount_point: [0u8; 256],
            size: 0,
            block_size: 0,
            numa: options.numa,
        },
        iorh: ptr::null_mut(),
        mount_point: [0u8; 256],
        for_read,
        io_depth: options.io_depth,
        priority: 0,
        timeout: options.timeout_ms,
        flags: 0,
    });
    // SAFETY: live `#[repr(C)]` ring struct; `mount` outlives the call.
    let rc = unsafe {
        (library.symbols.iorcreate4)(
            &mut *ring,
            mount.as_ptr(),
            options.entries as c_int,
            for_read,
            options.io_depth,
            options.timeout_ms,
            options.numa,
            0,
        )
    };
    if rc < 0 {
        return Err(errno_error(rc, "hf3fs_iorcreate4"));
    }
    Ok(ring)
}

/// Registered file descriptors, deregistered and closed on every path.
struct RegisteredFds<'a> {
    library: &'a Library,
    counters: &'a CountersAtomic,
    live: Vec<(c_int, c_int)>, // (linux fd, usrbio handle)
}

impl<'a> RegisteredFds<'a> {
    fn new(library: &'a Library, counters: &'a CountersAtomic) -> RegisteredFds<'a> {
        RegisteredFds {
            library,
            counters,
            live: Vec::new(),
        }
    }

    fn register(&mut self, fd: c_int) -> Result<c_int> {
        // SAFETY: `fd` is a live descriptor owned by the caller of `register`,
        // and the returned handle is only used while the fd stays open.
        let handle = unsafe { (self.library.symbols.reg_fd)(fd, 0) };
        if handle > 0 {
            // SAFETY: we own `fd` and registration failed.
            unsafe { libc::close(fd) };
            return Err(Error::io(format!(
                "hf3fs_reg_fd({fd}) failed with errno {handle} ({})",
                std::io::Error::from_raw_os_error(handle)
            )));
        }
        self.live.push((fd, handle));
        self.counters.registrations.fetch_add(1, Ordering::Relaxed);
        Ok(handle)
    }
}

impl Drop for RegisteredFds<'_> {
    fn drop(&mut self) {
        // SAFETY: each pair was registered by `register` and is deregistered
        // then closed exactly once, before the `Usrbio` can be dropped.
        unsafe {
            for (fd, _handle) in &self.live {
                (self.library.symbols.dereg_fd)(*fd);
                libc::close(*fd);
            }
        }
        self.live.clear();
    }
}

/// A 3FS call returning `0` on success, `-errno` on failure.
fn errno_result(rc: c_int, what: &str) -> Result<()> {
    if rc < 0 {
        Err(errno_error(rc, what))
    } else {
        Ok(())
    }
}

fn errno_error(rc: c_int, what: &str) -> Error {
    let errno = rc.saturating_neg();
    Error::io(format!(
        "{what} failed: errno {errno} ({})",
        std::io::Error::from_raw_os_error(errno)
    ))
}

/// A transport shared between the store's backend handle and diagnostics.
pub type SharedUsrbio = Arc<Usrbio>;

/// The constant `hf3fs_is_hf3fs` answer recorded for a load, for receipts.
pub fn library_path_default() -> PathBuf {
    match std::env::var_os(LIBRARY_ENV) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(DEFAULT_LIBRARY),
    }
}

// `IntoRawFd` is imported late so the trait does not shadow `File`-based code
// above; keeping it module-local avoids polluting the crate prelude.
use std::os::unix::io::IntoRawFd;
