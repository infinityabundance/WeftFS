//! 3FS transport court.
//!
//! Two transports over the *same* normative layout:
//!
//! * `posix` — the layout under a 3FS mount, addressed with positional
//!   `pread`/`pwrite`, `fsync`, and `rename`. This suite proves the store's
//!   logical state, commit records, manifests, chunks, and indexes are
//!   byte-identical to the local backend.
//! * `usrbio` — the native hf3fs ring API for the immutable data plane. The
//!   rings are driven against a conformance implementation of the published ABI
//!   (`bench/threefs/hf3fs_shim.c`) over real POSIX I/O.
//!
//! **No 3FS deployment is run here.** The shim returns `hf3fs_is_hf3fs = 0`
//! and the adapter records that, so nothing in this suite is evidence of 3FS
//! throughput, latency, RDMA behaviour, or multi-node consistency. What is
//! established is the ABI transcription, registration lifetime, batched
//! submission, completion reaping, short reads, per-request errors, and the
//! full store round trip through the ring path.
//!
//! Set `WEFT_3FS_SHIM` to the built `libhf3fs_api_shared.so` and
//! `WEFT_3FS_SHIM_DIR` to a scratch directory to run the USRBIO cases; without
//! them those cases pass vacuously and print why, as the S3 court does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use weftfs::backend::{BackendKind, Key, KeyKind};
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::threefs::{self, ReadRequest, ThreeFsConfig, ThreeFsMode};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 4,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn attrs() -> BTreeMap<String, weftcodec::wcb::Value> {
    BTreeMap::new()
}

fn seed(tag: u8) -> Vec<u8> {
    vec![tag; 32]
}

fn expect_err<T>(result: weftfs::Result<T>, what: &str) -> weftfs::Error {
    match result {
        Ok(_) => panic!("{what}: expected an error"),
        Err(err) => err,
    }
}

fn posix_kind() -> BackendKind {
    BackendKind::ThreeFs {
        config: ThreeFsConfig {
            mode: ThreeFsMode::Posix,
            ..ThreeFsConfig::default()
        },
    }
}

fn usrbio_kind(shim: &Path, mount: &Path) -> BackendKind {
    BackendKind::ThreeFs {
        config: ThreeFsConfig {
            mode: ThreeFsMode::Usrbio,
            mount_point: Some(mount.to_path_buf()),
            library: Some(shim.to_path_buf()),
            entries: 128,
            io_bytes: 1 << 20,
            ..ThreeFsConfig::default()
        },
    }
}

fn shim() -> Option<PathBuf> {
    match std::env::var_os("WEFT_3FS_SHIM") {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_file() {
                Some(path)
            } else {
                panic!(
                    "WEFT_3FS_SHIM points at {} which is not a file",
                    path.display()
                );
            }
        }
        None => None,
    }
}

fn shim_dir() -> Option<PathBuf> {
    std::env::var_os("WEFT_3FS_SHIM_DIR").map(PathBuf::from)
}

macro_rules! require_shim {
    () => {
        match (shim(), shim_dir()) {
            (Some(lib), Some(dir)) => (lib, dir),
            _ => {
                eprintln!(
                    "skip: WEFT_3FS_SHIM / WEFT_3FS_SHIM_DIR unset; the USRBIO transport is \
                     exercised only when the conformance library is built"
                );
                return;
            }
        }
    };
}

/// The same seeded op script as the backend court, so the comparison is
/// against a fixture the rest of the suite already trusts.
fn build_fixture(path: &Path, kind: BackendKind) -> Store {
    let mut store = Store::init_with_backend(path, opts(), kind).unwrap();
    let root = store.resolve_path("/").unwrap();
    let dir = store
        .mkdir_seeded(root, b"dir", 0o755, 0, 0, &seed(1))
        .unwrap();
    let a = store
        .create_file_seeded(
            root,
            b"a.txt",
            b"hello world, weft",
            0o644,
            0,
            0,
            attrs(),
            &seed(2),
        )
        .unwrap();
    store.write_file(a, 6, b"WORLD").unwrap();
    store.truncate(a, 11).unwrap();
    let b = store
        .mkdir_seeded(root, b"b", 0o755, 0, 0, &seed(3))
        .unwrap();
    store
        .create_file_seeded(b, b"inner", b"inner", 0o644, 0, 0, attrs(), &seed(4))
        .unwrap();
    store.rename(root, b"b", root, b"c").unwrap();
    let link_target = store
        .create_file_seeded(root, b"linked", b"shared", 0o644, 0, 0, attrs(), &seed(6))
        .unwrap();
    store.link(link_target, dir, b"hard").unwrap();
    store.set_xattr(a, "user.note", b"3fs court").unwrap();
    store
        .relation_add(
            root,
            "tag",
            RelationTarget::Value(weftcodec::wcb::Value::text("fixture")),
            None,
            attrs(),
        )
        .unwrap();
    store.snapshot_create("s1").unwrap();
    store.write_file(a, 0, b"HELLO").unwrap();
    store
}

fn keys(kind: &BackendKind, root: &Path, key_kind: KeyKind) -> Vec<Key> {
    let backend = kind.open(root).expect("open backend");
    let mut keys = backend.list_prefix(&Key::prefix(key_kind)).unwrap();
    keys.sort();
    keys
}

fn assert_same_artifacts(
    local: &Store,
    transport: &Store,
    local_kind: &BackendKind,
    transport_kind: &BackendKind,
    local_root: &Path,
    transport_root: &Path,
) {
    assert_eq!(local.generation(), transport.generation());
    assert_eq!(local.snapshot_root(), transport.snapshot_root());
    for kind in [
        KeyKind::Commit,
        KeyKind::Object,
        KeyKind::Manifest,
        KeyKind::Index,
    ] {
        let local_keys = keys(local_kind, local_root, kind);
        let transport_keys = keys(transport_kind, transport_root, kind);
        assert_eq!(local_keys, transport_keys, "{kind:?} key sets differ");
        for key in local_keys {
            let a = local.backend_read_key(&key).unwrap();
            let b = transport.backend_read_key(&key).unwrap();
            assert_eq!(a, b, "{kind:?} {} differs", key.display_path());
        }
    }
}

// ---------------------------------------------------------------------------
// POSIX transport
// ---------------------------------------------------------------------------

#[test]
fn posix_transport_is_byte_identical_to_local() {
    let local_dir = tempdir().unwrap();
    let threefs_dir = tempdir().unwrap();
    let local = build_fixture(local_dir.path(), BackendKind::Local);
    let transport = build_fixture(threefs_dir.path(), posix_kind());

    assert_same_artifacts(
        &local,
        &transport,
        &BackendKind::Local,
        &posix_kind(),
        local_dir.path(),
        threefs_dir.path(),
    );
    assert!(local.verify(true).unwrap().ok);
    assert!(transport.verify(true).unwrap().ok);

    drop(transport);
    let reopened =
        Store::open_with_backend(threefs_dir.path(), OpenMode::ReadOnly, posix_kind()).unwrap();
    assert_eq!(reopened.generation(), local.generation());
    assert_eq!(reopened.snapshot_root(), local.snapshot_root());
    assert_eq!(reopened.verify(true).unwrap().ok, true);
}

#[test]
fn posix_capabilities_are_declared_not_measured_text() {
    let capabilities = threefs::capabilities_for(ThreeFsMode::Posix);
    assert!(capabilities.atomic_publish);
    assert!(capabilities.durable_fsync);
    assert!(
        capabilities.consistency.contains("Declared, not measured"),
        "a receipt must not present the 3FS POSIX guarantees as measured: {}",
        capabilities.consistency
    );
    assert!(capabilities.publication.contains("3FS POSIX transport"));
    let usrbio = threefs::capabilities_for(ThreeFsMode::Usrbio);
    assert!(usrbio.publication.contains("hf3fs_prep_io"));
    assert!(usrbio.consistency.contains("Declared, not measured"));
}

// ---------------------------------------------------------------------------
// Fail-closed behaviour
// ---------------------------------------------------------------------------

#[test]
fn usrbio_refuses_when_the_library_is_absent() {
    let dir = tempdir().unwrap();
    let kind = BackendKind::ThreeFs {
        config: ThreeFsConfig {
            mode: ThreeFsMode::Usrbio,
            mount_point: Some(dir.path().to_path_buf()),
            library: Some(PathBuf::from("/nonexistent/libhf3fs_api_shared.so")),
            ..ThreeFsConfig::default()
        },
    };
    let err = expect_err(
        Store::init_with_backend(dir.path().join("store"), opts(), kind),
        "usrbio without the client library must refuse",
    );
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(
        err.message().contains("libhf3fs_api_shared.so"),
        "the refusal must name the library: {err}"
    );
}

#[test]
fn usrbio_rejects_unbounded_configuration() {
    let base = ThreeFsConfig::default();
    for (entries, io_bytes, why) in [
        (0usize, 1usize << 20, "zero entries"),
        (usize::MAX, 1usize << 20, "too many entries"),
        (256, 1024, "region below 64 KiB"),
        (256, usize::MAX, "region above 1 GiB"),
    ] {
        let config = ThreeFsConfig {
            mode: ThreeFsMode::Usrbio,
            entries,
            io_bytes,
            ..base.clone()
        };
        let err = expect_err(config.open(Path::new("/tmp")), why);
        assert_eq!(err.class_name(), "InvalidArgument", "{why}: {err}");
    }
}

#[test]
fn usrbio_require_mount_refuses_a_non_3fs_path() {
    let (lib, dir) = require_shim!();
    let config = ThreeFsConfig {
        mode: ThreeFsMode::Usrbio,
        mount_point: Some(dir.clone()),
        library: Some(lib),
        require_3fs_mount: true,
        ..ThreeFsConfig::default()
    };
    let err = expect_err(config.open(&dir), "the conformance shim is not a 3FS mount");
    assert_eq!(err.class_name(), "UnsupportedVersion", "{err}");
    assert!(err.message().contains("hf3fs_is_hf3fs"), "{err}");
}

#[test]
fn usrbio_status_reports_that_the_mount_is_not_3fs() {
    let (lib, dir) = require_shim!();
    let config = ThreeFsConfig {
        mode: ThreeFsMode::Usrbio,
        mount_point: Some(dir.clone()),
        library: Some(lib.clone()),
        ..ThreeFsConfig::default()
    };
    let transport = config.open(&dir).unwrap().expect("transport");
    assert_eq!(transport.library_path(), lib.as_path());
    assert_eq!(
        transport.mount_is_3fs().unwrap(),
        false,
        "the shim must never be reported as a 3FS mount"
    );
    let status = threefs::ThreeFsStatus {
        mode: Some(ThreeFsMode::Usrbio),
        mount_point: Some(dir.clone()),
        library: Some(lib),
        mount_is_3fs: Some(transport.mount_is_3fs().unwrap()),
        counters: transport.counters(),
        detail: String::new(),
    };
    assert!(status.describe().contains("hf3fs_is_hf3fs=false"));
}

// ---------------------------------------------------------------------------
// Batched reads through the ring
// ---------------------------------------------------------------------------

fn write_files(dir: &Path, count: usize, len: usize) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(count);
    for i in 0..count {
        let path = dir.join(format!("f{i:04}"));
        let bytes: Vec<u8> = (0..len).map(|j| ((i * 31 + j * 7) % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        paths.push(path);
    }
    paths
}

#[test]
fn usrbio_batch_reads_are_exact_and_batched() {
    let (lib, dir) = require_shim!();
    let files = write_files(&dir, 300, 4096);
    let transport = weftfs::threefs::Usrbio::open(&weftfs::threefs::ffi::Options {
        mount_point: dir.clone(),
        library: Some(lib),
        entries: 128,
        io_bytes: 1 << 20,
        ..weftfs::threefs::ffi::Options::default()
    })
    .unwrap();

    let requests: Vec<ReadRequest<'_>> = files
        .iter()
        .map(|path| ReadRequest {
            path: path.as_path(),
            offset: 0,
            len: 4096,
        })
        .collect();
    let out = transport.read_batch(&requests).unwrap();
    assert_eq!(out.len(), files.len());
    for (path, got) in files.iter().zip(out.iter()) {
        assert_eq!(got, &std::fs::read(path).unwrap(), "{}", path.display());
    }
    let counters = transport.counters();
    assert_eq!(counters.requests, 300);
    assert_eq!(counters.completions, 300);
    assert_eq!(counters.bytes_read, 300 * 4096);
    // 300 requests over rings of 128 entries: three submission round trips, not
    // one per request.
    assert_eq!(
        counters.submissions, 3,
        "batched reads must submit once per ring-full"
    );
}

#[test]
fn usrbio_survives_partial_completion() {
    let (lib, dir) = require_shim!();
    let files = write_files(&dir, 7, 1024);
    let transport = weftfs::threefs::Usrbio::open(&weftfs::threefs::ffi::Options {
        mount_point: dir.clone(),
        library: Some(lib),
        entries: 16,
        io_bytes: 1 << 20,
        ..weftfs::threefs::ffi::Options::default()
    })
    .unwrap();
    // Make the ring hand back at most two completions per wait.
    let shim_lib = transport.library_path().to_path_buf();
    let shim_handle = unsafe {
        let c = std::ffi::CString::new(shim_lib.as_os_str().as_encoded_bytes()).unwrap();
        libc::dlopen(c.as_ptr(), libc::RTLD_NOW)
    };
    assert!(!shim_handle.is_null());
    let partial: unsafe extern "C" fn(i32) = unsafe {
        let c = std::ffi::CString::new("weft_shim_partial_waits").unwrap();
        let raw = libc::dlsym(shim_handle, c.as_ptr());
        assert!(
            !raw.is_null(),
            "weft_shim_partial_waits missing from the shim"
        );
        std::mem::transmute_copy(&raw)
    };
    unsafe { partial(2) };

    let requests: Vec<ReadRequest<'_>> = files
        .iter()
        .map(|path| ReadRequest {
            path: path.as_path(),
            offset: 0,
            len: 1024,
        })
        .collect();
    let out = transport.read_batch(&requests).unwrap();
    for (path, got) in files.iter().zip(out.iter()) {
        assert_eq!(got, &std::fs::read(path).unwrap(), "{}", path.display());
    }
    unsafe { partial(0) };
}

#[test]
fn usrbio_truncates_at_end_of_object_and_rejects_a_past_end_offset() {
    let (lib, dir) = require_shim!();
    let path = dir.join("short");
    std::fs::write(&path, b"0123456789").unwrap();
    let transport = weftfs::threefs::Usrbio::open(&weftfs::threefs::ffi::Options {
        mount_point: dir.clone(),
        library: Some(lib),
        ..weftfs::threefs::ffi::Options::default()
    })
    .unwrap();

    let out = transport
        .read_batch(&[
            ReadRequest {
                path: &path,
                offset: 6,
                len: 100,
            },
            ReadRequest {
                path: &path,
                offset: 0,
                len: 0,
            },
        ])
        .unwrap();
    assert_eq!(out[0], b"6789");
    assert!(out[1].is_empty());

    let err = expect_err(
        transport.read_batch(&[ReadRequest {
            path: &path,
            offset: 11,
            len: 1,
        }]),
        "offset past end of object must fail",
    );
    assert_eq!(err.class_name(), "InvalidArgument", "{err}");
}

#[test]
fn usrbio_read_into_takes_the_direct_path_inside_the_region() {
    let (lib, dir) = require_shim!();
    let path = dir.join("direct");
    let bytes: Vec<u8> = (0..8192u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let transport = weftfs::threefs::Usrbio::open(&weftfs::threefs::ffi::Options {
        mount_point: dir.clone(),
        library: Some(lib),
        io_bytes: 1 << 20,
        ..weftfs::threefs::ffi::Options::default()
    })
    .unwrap();

    // Stage through an ordinary caller buffer: the read lands in the region and
    // is copied once.
    let mut staged = vec![0u8; 8192];
    let read = transport.read_into(&path, 0, &mut staged).unwrap();
    assert_eq!(read, 8192);
    assert_eq!(staged, bytes);
    let after_staged = transport.counters();
    assert_eq!(after_staged.staged_reads, 1);
    assert_eq!(after_staged.direct_reads, 0);

    // Now read into a slice of the shared region itself: no staging copy. The
    // request starts at offset 128, so it is short by 128 bytes at end of file.
    let (base, len) = transport.region();
    assert!(len >= 8192);
    let region = unsafe { std::slice::from_raw_parts_mut(base, 8192) };
    let read = transport.read_into(&path, 128, region).unwrap();
    assert_eq!(read, 8192 - 128);
    assert_eq!(&region[..read], &bytes[128..]);
    let after_direct = transport.counters();
    assert_eq!(after_direct.direct_reads, 1);
    assert_eq!(after_direct.staged_reads, 1);
}

#[test]
fn usrbio_reports_a_prep_failure_without_leaking_a_registration() {
    let (lib, dir) = require_shim!();
    let files = write_files(&dir, 3, 512);
    let transport = weftfs::threefs::Usrbio::open(&weftfs::threefs::ffi::Options {
        mount_point: dir.clone(),
        library: Some(lib.clone()),
        ..weftfs::threefs::ffi::Options::default()
    })
    .unwrap();
    let c = std::ffi::CString::new(lib.as_os_str().as_encoded_bytes()).unwrap();
    let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW) };
    let fail: unsafe extern "C" fn(i32) = unsafe {
        let name = std::ffi::CString::new("weft_shim_fail_next_prep").unwrap();
        let raw = libc::dlsym(handle, name.as_ptr());
        assert!(!raw.is_null());
        std::mem::transmute_copy(&raw)
    };
    let registered: unsafe extern "C" fn() -> i32 = unsafe {
        let name = std::ffi::CString::new("weft_shim_registered_fds").unwrap();
        let raw = libc::dlsym(handle, name.as_ptr());
        assert!(!raw.is_null());
        std::mem::transmute_copy(&raw)
    };
    unsafe { fail(libc::EIO) };
    let requests: Vec<ReadRequest<'_>> = files
        .iter()
        .map(|path| ReadRequest {
            path: path.as_path(),
            offset: 0,
            len: 512,
        })
        .collect();
    let err = expect_err(transport.read_batch(&requests), "injected prep failure");
    assert_eq!(err.class_name(), "Io", "{err}");
    assert_eq!(
        unsafe { registered() },
        0,
        "the registration guard must deregister and close on the error path"
    );
}

// ---------------------------------------------------------------------------
// Full store over the ring
// ---------------------------------------------------------------------------

#[test]
fn store_round_trip_over_usrbio_is_byte_identical_to_local() {
    let (lib, _dir) = require_shim!();
    let local_dir = tempdir().unwrap();
    let remote_dir = tempdir().unwrap();
    let mount = tempdir().unwrap();
    let kind = usrbio_kind(&lib, mount.path());

    let local = build_fixture(local_dir.path(), BackendKind::Local);
    let mut transport = build_fixture(remote_dir.path(), kind.clone());

    assert_same_artifacts(
        &local,
        &transport,
        &BackendKind::Local,
        &kind,
        local_dir.path(),
        remote_dir.path(),
    );
    let report = transport.verify(true).unwrap();
    assert!(report.ok, "verify over the ring: {report:?}");

    // The data plane really went through the rings, and the transport is
    // reported as 3FS rather than local.
    let backend = kind.open(remote_dir.path()).unwrap();
    assert_eq!(backend.label(), "3fs");
    assert!(
        backend
            .diagnostics()
            .iter()
            .any(|line| line.contains("3fs")),
        "diagnostics must name the transport: {:?}",
        backend.diagnostics()
    );

    transport.snapshot_create("s2").unwrap();
    let after_snapshot = transport.snapshot_root();
    drop(transport);
    let reopened = Store::open_with_backend(remote_dir.path(), OpenMode::ReadOnly, kind).unwrap();
    assert_eq!(reopened.snapshot_root(), after_snapshot);
    assert!(reopened.verify(true).unwrap().ok);
}

// ---------------------------------------------------------------------------
// The crate-level invariant
// ---------------------------------------------------------------------------

#[test]
fn unsafe_is_confined_to_the_threefs_ffi_module() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    // Two explicit, narrow exceptions, each with an invariant list at the top
    // of the file:
    //  * `threefs/` — the 3FS USRBIO C-ABI boundary;
    //  * `checkpoint_service/ancillary.rs` — `recvmsg`/`sendmsg`
    //    descriptor passing, which `std` does not wrap on stable Rust;
    //  * `writer_lock.rs` — `open(2)` flags, `flock(2)` and `fcntl(2)`
    //    `F_OFD_SETLK` on a descriptor the module itself opened.
    // One more line-level exception: the fenced-writer court hook must stop the
    // process with `raise(SIGSTOP)` at an exact publication boundary. The hook
    // is inert unless the court arms `WEFT_FENCE_PAUSE_AFTER_FENCE`, and the
    // guard below still requires that this is the file's *only* unsafe line.
    let mut hook_unsafe_lines = 0usize;
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let relative = path.strip_prefix(&src).unwrap();
            if relative.starts_with("threefs") {
                continue;
            }
            if relative == Path::new("checkpoint_service/ancillary.rs") {
                continue;
            }
            if relative == Path::new("writer_lock.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let hook = relative == Path::new("fence_pause.rs");
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if !line.contains("unsafe") || line.contains("unsafe_code") {
                    continue;
                }
                if hook && line.contains("libc::raise") {
                    hook_unsafe_lines += 1;
                    continue;
                }
                offenders.push(format!("{}:{}", relative.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "unsafe code escaped the syscall-boundary modules: {offenders:?}"
    );
    assert_eq!(
        hook_unsafe_lines, 1,
        "the fence-pause hook must contain exactly one unsafe call (libc::raise)"
    );
}
