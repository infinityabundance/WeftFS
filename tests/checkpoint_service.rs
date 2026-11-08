//! The persistent checkpoint service: framing, bounds, fd passing, exactness,
//! and the no-read-mutation guarantee.
//!
//! Every test talks to the real `weftfs checkpoint-service` binary over a real
//! unix socket, with real memfds, exactly as the Python client does.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;
use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::error::Class;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};

const SHAPE: [u64; 3] = [64, 32, 16];
const TENSOR: &str = "weight";
const CHECKPOINT: &str = "rank3";

const OP_INFO: u8 = 0x01;
const OP_LIST: u8 = 0x02;
const OP_MANIFEST: u8 = 0x03;
const OP_RESTORE: u8 = 0x04;
const OP_WRITE: u8 = 0x05;
const OP_VERIFY: u8 = 0x06;
const OP_REFRESH: u8 = 0x07;
const OP_SHUTDOWN: u8 = 0x7f;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn tensor_len() -> usize {
    SHAPE.iter().product::<u64>() as usize * 2
}

fn container_bytes() -> Vec<u8> {
    let data = random_bytes(tensor_len(), 0x5eed_1234);
    safetensors::encode_file(
        &[SafetensorsEntry {
            name: TENSOR,
            dtype: "bf16",
            shape: &SHAPE,
            data: &data,
        }],
        None,
    )
    .unwrap()
}

/// Initialize a store holding one 8-way sharded checkpoint.
fn fixture(dir: &Path) -> PathBuf {
    let store_path = dir.join("store");
    let mut store = Store::init(&store_path, opts()).unwrap();
    let source = dir.join("rank3.safetensors");
    fs::write(&source, container_bytes()).unwrap();
    store
        .write_checkpoint(
            CHECKPOINT,
            &source,
            &CheckpointWriteOptions {
                shards: 8,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    store_path
}

/// Directory digest used by the no-mutation assertion.
fn tree_digest(path: &Path) -> (u64, usize, String) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut names = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
                continue;
            }
            bytes += meta.len();
            files += 1;
            names.push(format!(
                "{}:{}",
                entry.path().strip_prefix(path).unwrap().display(),
                meta.len()
            ));
        }
    }
    names.sort();
    (bytes, files, names.join("|"))
}

fn memfd(bytes: &[u8]) -> OwnedFd {
    let name = b"weft-service-test\0";
    let fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr() as *const libc::c_char,
            libc::MFD_CLOEXEC,
        )
    };
    assert!(fd >= 0, "memfd_create failed");
    let owned = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
    if !bytes.is_empty() {
        let rc = unsafe {
            libc::pwrite(
                owned.as_raw_fd(),
                bytes.as_ptr() as *const libc::c_void,
                bytes.len(),
                0,
            )
        };
        assert_eq!(rc as usize, bytes.len(), "memfd write");
    }
    owned
}

struct ServiceProcess {
    child: Child,
    stream: UnixStream,
    socket: PathBuf,
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

fn start_service(store: &Path, socket: &Path, extra: &[&str]) -> ServiceProcess {
    let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
    command
        .arg("--store")
        .arg(store)
        .arg("checkpoint-service")
        .arg("--socket")
        .arg(socket)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn checkpoint-service");
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("readiness line");
    assert_eq!(line.trim(), "ready", "service did not signal readiness");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => {
                return ServiceProcess {
                    child,
                    stream,
                    socket: socket.to_path_buf(),
                }
            }
            Err(err) if Instant::now() < deadline => {
                let _ = err;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("connect to service socket: {err}"),
        }
    }
}

fn send_frame(stream: &UnixStream, op: u8, id: u32, body: &[u8], fds: &[RawFd]) {
    let mut frame = Vec::with_capacity(6 + 4 + body.len());
    let len = 6 + body.len();
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    frame.push(op);
    frame.extend_from_slice(&id.to_le_bytes());
    frame.push(0);
    frame.extend_from_slice(body);
    let mut iovec = libc::iovec {
        iov_base: frame.as_ptr() as *mut libc::c_void,
        iov_len: frame.len(),
    };
    let mut control = [0u64; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    if !fds.is_empty() {
        message.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE((fds.len() * std::mem::size_of::<RawFd>()) as u32) } as _;
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&message) };
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN((fds.len() * 4) as u32) as _;
            let data = libc::CMSG_DATA(cmsg) as *mut RawFd;
            for (index, fd) in fds.iter().enumerate() {
                std::ptr::write(data.add(index), *fd);
            }
        }
    }
    let rc = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
    assert_eq!(rc as usize, frame.len(), "sendmsg wrote the whole frame");
}

struct Response {
    id: u32,
    status: u8,
    body: Vec<u8>,
    fds: Vec<OwnedFd>,
}

fn recv_frame(stream: &UnixStream) -> std::io::Result<Response> {
    let mut header = [0u8; 4];
    let mut fds = Vec::new();
    let mut got = 0usize;
    let mut first = true;
    while got < 4 {
        let mut iovec = libc::iovec {
            iov_base: header[got..].as_mut_ptr() as *mut libc::c_void,
            iov_len: 4 - got,
        };
        let mut control = [0u64; 8];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iovec;
        message.msg_iovlen = 1;
        if first {
            message.msg_control = control.as_mut_ptr() as *mut libc::c_void;
            message.msg_controllen = std::mem::size_of_val(&control) as _;
        }
        let rc = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
        if rc <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if first {
            let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&message) };
            while !cmsg.is_null() {
                let head = unsafe { &*cmsg };
                if head.cmsg_level == libc::SOL_SOCKET && head.cmsg_type == libc::SCM_RIGHTS {
                    let base = unsafe { libc::CMSG_LEN(0) } as usize;
                    let count = (head.cmsg_len as usize - base) / 4;
                    let data = unsafe { libc::CMSG_DATA(cmsg) } as *const RawFd;
                    for index in 0..count {
                        let fd = unsafe { *data.add(index) };
                        fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
                    }
                }
                cmsg = unsafe { libc::CMSG_NXTHDR(&message, cmsg) };
            }
        }
        first = false;
        got += rc as usize;
    }
    let len = u32::from_le_bytes(header) as usize;
    assert!(len >= 6 && len <= 1 << 20, "response length {len}");
    let mut payload = vec![0u8; len];
    stream.take(len as u64).read_exact(&mut payload)?;
    Ok(Response {
        id: u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]),
        status: payload[5],
        body: payload[6..].to_vec(),
        fds,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }
    fn u8(&mut self) -> u8 {
        let value = self.bytes[self.at];
        self.at += 1;
        value
    }
    fn u32(&mut self) -> u32 {
        let value = u32::from_le_bytes(self.bytes[self.at..self.at + 4].try_into().unwrap());
        self.at += 4;
        value
    }
    fn u64(&mut self) -> u64 {
        let value = u64::from_le_bytes(self.bytes[self.at..self.at + 8].try_into().unwrap());
        self.at += 8;
        value
    }
    fn string(&mut self) -> String {
        let len = self.u32() as usize;
        let text = String::from_utf8(self.bytes[self.at..self.at + len].to_vec()).unwrap();
        self.at += len;
        text
    }
    fn hash(&mut self) -> String {
        let digest = self.bytes[self.at..self.at + 32].to_vec();
        self.at += 32;
        hex::encode(digest)
    }
    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn expect_ok(response: &Response, id: u32) -> Vec<u8> {
    assert_eq!(response.id, id, "request id echoed");
    if response.status != 0 {
        let mut cursor = Reader::new(&response.body);
        let class = cursor.string();
        let message = cursor.string();
        panic!("request {id} failed: {class}: {message}");
    }
    response.body.clone()
}

fn expect_error(response: &Response, id: u32) -> (String, String) {
    assert_eq!(response.id, id, "request id echoed");
    assert_eq!(response.status, 1, "status error");
    let mut cursor = Reader::new(&response.body);
    let class = cursor.string();
    let message = cursor.string();
    (class, message)
}

#[test]
fn service_lists_shows_and_restores_into_a_caller_memfd() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let service = start_service(&store_path, &socket, &[]);

    send_frame(&service.stream, OP_INFO, 1, &[], &[]);
    let info = expect_ok(&recv_frame(&service.stream).unwrap(), 1);
    let mut cursor = Reader::new(&info);
    assert_eq!(cursor.u32(), 2, "protocol version");
    assert_eq!(cursor.u32(), 1 << 20, "frame bound");
    assert_eq!(cursor.u64(), 4 << 30, "tensor bound");
    assert_eq!(cursor.u64(), 2, "generation");
    cursor.hash();
    assert_eq!(cursor.string(), store_path.display().to_string());

    send_frame(&service.stream, OP_LIST, 2, &[], &[]);
    let list = expect_ok(&recv_frame(&service.stream).unwrap(), 2);
    let mut cursor = Reader::new(&list);
    assert_eq!(cursor.u32(), 1);
    assert_eq!(cursor.string(), CHECKPOINT);

    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    send_frame(&service.stream, OP_MANIFEST, 3, &body, &[]);
    let manifest = expect_ok(&recv_frame(&service.stream).unwrap(), 3);
    let mut cursor = Reader::new(&manifest);
    let manifest_root = cursor.hash();
    assert_eq!(cursor.string(), CHECKPOINT);
    assert_eq!(cursor.string(), "safetensors");
    assert_eq!(cursor.u8(), 1, "complete");
    assert_eq!(cursor.u64(), tensor_len() as u64);
    assert_eq!(cursor.u8(), 1, "source_file_bytes present");
    assert_eq!(cursor.u64(), container_bytes().len() as u64);
    assert_eq!(cursor.u8(), 1, "source_sha256 present");
    cursor.hash();
    assert_eq!(cursor.u32(), 1, "one tensor");
    assert_eq!(cursor.string(), TENSOR);
    assert_eq!(cursor.string(), "bf16");
    assert_eq!(cursor.u32(), 3);
    assert_eq!([cursor.u64(), cursor.u64(), cursor.u64()], SHAPE, "shape");
    cursor.hash(); // logical root
    cursor.hash(); // content root
    assert_eq!(cursor.u64(), tensor_len() as u64);
    cursor.u64(); // source offset
    assert_eq!(cursor.u32(), 8, "eight shards");
    let expected_root = manifest_root;
    assert_eq!(expected_root.len(), 64);

    // Restore into a caller-owned memfd.
    let buffer = memfd(&vec![0u8; tensor_len()]);
    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    put_str(&mut body, TENSOR);
    body.push(0); // no region
    body.push(0); // no topology
    send_frame(&service.stream, OP_RESTORE, 4, &body, &[buffer.as_raw_fd()]);
    let restored = expect_ok(&recv_frame(&service.stream).unwrap(), 4);
    let mut cursor = Reader::new(&restored);
    assert_eq!(cursor.string(), "bf16");
    assert_eq!(cursor.u32(), 3);
    cursor.u64();
    cursor.u64();
    cursor.u64();
    assert_eq!(cursor.u64(), tensor_len() as u64, "bytes restored");
    let content_root = cursor.hash();
    cursor.hash(); // full logical root
    assert_eq!(content_root.len(), 64);
    assert_eq!(cursor.u8(), 1, "full root verified");
    assert_eq!(cursor.u8(), 1, "shard roots verified");
    assert_eq!(cursor.u8(), 0, "no topology root");
    assert_eq!(cursor.u32(), 8, "shards read");
    assert!(cursor.u64() >= 8, "units touched");
    cursor.u64(); // store bytes read
    cursor.u64(); // store reads
    cursor.u64(); // elapsed
    assert_eq!(cursor.rest().len(), 0, "no trailing bytes");

    let mut actual = vec![0u8; tensor_len()];
    let rc = unsafe {
        libc::pread(
            buffer.as_raw_fd(),
            actual.as_mut_ptr() as *mut libc::c_void,
            actual.len(),
            0,
        )
    };
    assert_eq!(rc as usize, actual.len());
    assert_eq!(actual, random_bytes(tensor_len(), 0x5eed_1234));

    // Region restore: only the region is written.
    let region = weftcodec::Region::new(vec![0, 0, 0], vec![16, 32, 16]);
    let extent = region.extent();
    let expected_len = extent.iter().product::<u64>() as usize * 2;
    let region_buffer = memfd(&vec![0u8; expected_len]);
    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    put_str(&mut body, TENSOR);
    body.push(1);
    body.push(3);
    for dim in [0u64, 0, 0] {
        body.extend_from_slice(&dim.to_le_bytes());
    }
    for dim in extent.iter() {
        body.extend_from_slice(&dim.to_le_bytes());
    }
    body.push(0);
    send_frame(
        &service.stream,
        OP_RESTORE,
        5,
        &body,
        &[region_buffer.as_raw_fd()],
    );
    let region_response = expect_ok(&recv_frame(&service.stream).unwrap(), 5);
    let mut cursor = Reader::new(&region_response);
    assert_eq!(cursor.string(), "bf16");
    assert_eq!(cursor.u32(), 3);
    cursor.u64();
    cursor.u64();
    cursor.u64();
    assert_eq!(cursor.u64(), expected_len as u64);
    let mut actual = vec![0u8; expected_len];
    let rc = unsafe {
        libc::pread(
            region_buffer.as_raw_fd(),
            actual.as_mut_ptr() as *mut libc::c_void,
            actual.len(),
            0,
        )
    };
    assert_eq!(rc as usize, expected_len);
    let source = random_bytes(tensor_len(), 0x5eed_1234);
    assert_eq!(actual, source[..expected_len].to_vec());

    // Shutdown is clean.
    send_frame(&service.stream, OP_SHUTDOWN, 6, &[], &[]);
    let _ = expect_ok(&recv_frame(&service.stream).unwrap(), 6);
}

#[test]
fn restore_never_mutates_the_store() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let service = start_service(&store_path, &socket, &[]);
    let before = tree_digest(&store_path);

    for id in 0..20u32 {
        let buffer = memfd(&vec![0u8; tensor_len()]);
        let mut body = Vec::new();
        put_str(&mut body, CHECKPOINT);
        put_str(&mut body, TENSOR);
        body.push(0);
        body.push(0);
        send_frame(
            &service.stream,
            OP_RESTORE,
            id,
            &body,
            &[buffer.as_raw_fd()],
        );
        expect_ok(&recv_frame(&service.stream).unwrap(), id);
    }

    let after = tree_digest(&store_path);
    assert_eq!(before, after, "a read-only restore changed store bytes");

    // And the store still opens read-only with the same generation.
    let store = Store::open_with_options(&store_path, OpenMode::ReadOnly, opts()).unwrap();
    assert_eq!(store.generation(), 2);
}

#[test]
fn typed_errors_keep_the_service_alive() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let service = start_service(&store_path, &socket, &[]);

    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    put_str(&mut body, "no-such-tensor");
    body.push(0);
    body.push(0);
    let buffer = memfd(&vec![0u8; tensor_len()]);
    send_frame(
        &service.stream,
        OP_RESTORE,
        11,
        &body,
        &[buffer.as_raw_fd()],
    );
    let (class, message) = expect_error(&recv_frame(&service.stream).unwrap(), 11);
    assert_eq!(class, Class::NotFound.as_str(), "{message}");

    // A caller buffer that is too small is refused before any decode.
    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    put_str(&mut body, TENSOR);
    body.push(0);
    body.push(0);
    let small = memfd(&vec![0u8; 16]);
    send_frame(&service.stream, OP_RESTORE, 12, &body, &[small.as_raw_fd()]);
    let (class, message) = expect_error(&recv_frame(&service.stream).unwrap(), 12);
    assert_eq!(class, Class::InvalidArgument.as_str(), "{message}");
    assert!(message.contains("needs"), "{message}");

    // A missing descriptor is a typed error, not a crash.
    send_frame(&service.stream, OP_RESTORE, 13, &body, &[]);
    let (class, _) = expect_error(&recv_frame(&service.stream).unwrap(), 13);
    assert_eq!(class, Class::InvalidArgument.as_str());

    // Still serving.
    send_frame(&service.stream, OP_INFO, 14, &[], &[]);
    expect_ok(&recv_frame(&service.stream).unwrap(), 14);
}

#[test]
fn write_checkpoint_from_a_memfd_matches_the_cli_oracle() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    // The fixture and the CLI oracle use a fixed clock, so the service must too
    // for the shard object ids (and therefore the manifest root) to be
    // comparable at all.
    let service = start_service(
        &store_path,
        &socket,
        &["--fixed-timestamp-ns", "1700000000000000000"],
    );

    let container = container_bytes();
    let staged = memfd(&container);
    let mut body = Vec::new();
    put_str(&mut body, "svc-written");
    body.extend_from_slice(&1u32.to_le_bytes()); // shards
    body.extend_from_slice(&0u32.to_le_bytes()); // axis
    put_str(&mut body, "zstd");
    body.extend_from_slice(&0u32.to_le_bytes()); // no tile shape
    body.push(0); // planned
    body.push(1); // zstd fast codec (unused by planned)
    body.extend_from_slice(&1u32.to_le_bytes()); // fast zstd level
    body.push(0); // flags
    body.push(0); // no deadline
    send_frame(&service.stream, OP_WRITE, 21, &body, &[staged.as_raw_fd()]);
    let response = expect_ok(&recv_frame(&service.stream).unwrap(), 21);
    let mut cursor = Reader::new(&response);
    cursor.hash(); // checkpoint id
    let head = cursor.hash();
    let manifest_root = cursor.hash();
    let source_sha = cursor.hash();
    assert_eq!(cursor.u64(), container.len() as u64);
    assert_eq!(cursor.u64(), tensor_len() as u64);
    assert_eq!(cursor.u32(), 1, "tensors");
    cursor.u32(); // shard objects
    assert_eq!(cursor.u8(), 1, "complete");
    cursor.u64(); // elapsed
    let manifest_len = cursor.u32() as usize;
    assert_eq!(cursor.rest().len(), manifest_len);
    let encoded_manifest = cursor.rest().to_vec();
    assert_eq!(
        source_sha,
        hex::encode(safetensors::sha256(&container)),
        "the service reports the container digest"
    );
    {
        let mut encoded = Reader::new(&encoded_manifest);
        assert_eq!(encoded.hash(), manifest_root, "encoded manifest root");
        assert_eq!(encoded.string(), "svc-written");
        assert_eq!(encoded.string(), "safetensors");
        assert_eq!(encoded.u8(), 1, "complete");
        assert_eq!(encoded.u64(), tensor_len() as u64);
    }

    // CLI oracle on the same store: `checkpoint show --json` must agree with
    // the protocol manifest, and `checkpoint restore --host-out` must reproduce
    // the exact container the service was handed. (Shard object ids are
    // store-local, so an independent store cannot have the same manifest root;
    // the oracle is the same store, read through the CLI.)
    let show = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .arg("--store")
        .arg(&store_path)
        .args(["checkpoint", "show", "--name", "svc-written", "--json"])
        .output()
        .unwrap();
    assert!(
        show.status.success(),
        "CLI show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["manifest_root"].as_str().unwrap(), manifest_root);
    let tensors = show_json["manifest"]["tensors"].as_array().unwrap();
    assert_eq!(tensors.len(), 1);
    assert_eq!(tensors[0]["name"].as_str().unwrap(), TENSOR);
    assert_eq!(tensors[0]["dtype"].as_str().unwrap(), "bf16");
    assert_eq!(
        show_json["manifest"]["source_file_bytes"].as_u64().unwrap(),
        container.len() as u64
    );

    let restored_path = dir.path().join("oracle-restored.safetensors");
    let restore = Command::new(env!("CARGO_BIN_EXE_weftfs"))
        .arg("--store")
        .arg(&store_path)
        .args(["checkpoint", "restore", "--name", "svc-written"])
        .arg("--host-out")
        .arg(&restored_path)
        .args(["--json"])
        .output()
        .unwrap();
    assert!(
        restore.status.success(),
        "CLI restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    let restore_json: serde_json::Value = serde_json::from_slice(&restore.stdout).unwrap();
    assert_eq!(
        restore_json["exact_source_file"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(restore_json["source_sha256"].as_str().unwrap(), source_sha);
    assert_eq!(fs::read(&restored_path).unwrap(), container);

    let written_store = Store::open_with_options(&store_path, OpenMode::ReadOnly, opts()).unwrap();
    let written_manifest = written_store.checkpoint_manifest("svc-written").unwrap();
    assert_eq!(
        written_manifest.root().unwrap().to_hex(),
        manifest_root,
        "manifest root reported by the service"
    );
    assert_eq!(written_store.head().to_hex(), head, "service commit head");

    // The in-process read handle must not serve a stale manifest.
    let mut body = Vec::new();
    put_str(&mut body, "svc-written");
    send_frame(&service.stream, OP_VERIFY, 22, &body, &[]);
    let verified = expect_ok(&recv_frame(&service.stream).unwrap(), 22);
    assert_eq!(Reader::new(&verified).u32(), 1);

    // A refresh reports the durable generation the service is now pinned to.
    send_frame(&service.stream, OP_REFRESH, 23, &[], &[]);
    let refreshed = expect_ok(&recv_frame(&service.stream).unwrap(), 23);
    let mut cursor = Reader::new(&refreshed);
    assert_eq!(cursor.u64(), 3, "init + fixture + service write");
    assert_eq!(cursor.hash(), head);
}

#[test]
fn oversized_frame_terminates_the_service_nonzero() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let mut service = start_service(&store_path, &socket, &[]);

    // A frame length above the bound: the service must answer with a typed
    // Malformed frame and then exit non-zero instead of desynchronising.
    let header = ((1 << 20) as u32 + 1).to_le_bytes();
    service.stream.write_all(&header).unwrap();
    let response = recv_frame(&service.stream);
    if let Ok(response) = response {
        let (class, _) = expect_error(&response, 0);
        assert_eq!(class, Class::Malformed.as_str());
    }
    let status = service.child.wait().unwrap();
    assert!(!status.success(), "service exit code must be non-zero");
}

#[test]
fn unknown_opcode_is_refused_and_the_service_exits_nonzero() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let mut service = start_service(&store_path, &socket, &[]);
    send_frame(&service.stream, 0x42, 1, &[], &[]);
    let response = recv_frame(&service.stream).unwrap();
    let (class, _) = expect_error(&response, 1);
    assert_eq!(class, Class::Malformed.as_str());
    let status = service.child.wait().unwrap();
    assert!(!status.success());
}

#[test]
fn topology_restore_reports_a_verified_topology_root() {
    let dir = tempdir().unwrap();
    let store_path = fixture(dir.path());
    let socket = dir.path().join("svc.sock");
    let service = start_service(&store_path, &socket, &[]);

    let buffer = memfd(&vec![0u8; tensor_len()]);
    let mut body = Vec::new();
    put_str(&mut body, CHECKPOINT);
    put_str(&mut body, TENSOR);
    body.push(0);
    body.push(1);
    body.extend_from_slice(&2u32.to_le_bytes()); // shards
    body.extend_from_slice(&0u32.to_le_bytes()); // axis
    send_frame(
        &service.stream,
        OP_RESTORE,
        31,
        &body,
        &[buffer.as_raw_fd()],
    );
    let response = expect_ok(&recv_frame(&service.stream).unwrap(), 31);
    let mut cursor = Reader::new(&response);
    assert_eq!(cursor.string(), "bf16");
    assert_eq!(cursor.u32(), 3);
    cursor.u64();
    cursor.u64();
    cursor.u64();
    assert_eq!(cursor.u64(), tensor_len() as u64);
    cursor.hash();
    cursor.hash();
    cursor.u8();
    cursor.u8();
    assert_eq!(cursor.u8(), 1, "topology root present");
    let topology_root = cursor.hash();
    assert_eq!(topology_root.len(), 64);
    // The bytes under a different topology are the same logical tensor.
    let mut actual = vec![0u8; tensor_len()];
    unsafe {
        libc::pread(
            buffer.as_raw_fd(),
            actual.as_mut_ptr() as *mut libc::c_void,
            actual.len(),
            0,
        );
    }
    assert_eq!(actual, random_bytes(tensor_len(), 0x5eed_1234));
}
