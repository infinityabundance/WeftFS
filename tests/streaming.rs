//! Streaming file creation and append.
//!
//! These tests prove the two properties that matter: the bytes and the durable
//! state are identical to the buffered path, and peak process memory does not
//! grow with file size. The large cases run on the court's 1 GiB `/tmp` tmpfs
//! and report the exact free-space numbers instead of skipping silently.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use tempfile::{tempdir, TempDir};
use weftfs::recovery::read_manifest;
use weftfs::store::{Options, RepresentationMode, Store, STREAM_IO_BYTES};

const MIB: u64 = 1024 * 1024;
const BLOCK: u64 = MIB;
/// Documented ceiling for peak RSS during the 512 MiB streaming create.
const PEAK_RSS_BOUND_KIB: u64 = 128 * 1024;

/// The two multi-hundred-MiB cases must not overlap: the court `/tmp` is a
/// 1 GiB tmpfs and each store is removed when its `TempDir` drops.
static BIG_STORE_LOCK: Mutex<()> = Mutex::new(());

fn big_lock() -> MutexGuard<'static, ()> {
    BIG_STORE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn opts(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

// ---- deterministic generator ------------------------------------------------

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Per-byte model of the generator: a splitmix64 word per 8-byte lane, salted
/// by the 1 MiB block index. Every chunk differs, so content-addressed dedup
/// cannot hide bytes that were never written.
fn fill_model(out: &mut [u8], offset: u64) {
    let mut pos = 0usize;
    while pos < out.len() {
        let absolute = offset + pos as u64;
        let lane = (absolute % 8) as usize;
        let block_salt = (absolute / BLOCK).wrapping_mul(0xD1B5_4A32_D192_ED03);
        let word = mix64((absolute / 8) ^ block_salt).to_le_bytes();
        let take = (8 - lane).min(out.len() - pos);
        out[pos..pos + take].copy_from_slice(&word[lane..lane + take]);
        pos += take;
    }
}

fn model_at(offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    fill_model(&mut out, offset);
    out
}

struct PatternReader {
    offset: u64,
    remaining: u64,
}

impl PatternReader {
    fn new(len: u64) -> Self {
        Self::at(0, len)
    }

    fn at(offset: u64, len: u64) -> Self {
        PatternReader {
            offset,
            remaining: len,
        }
    }
}

impl Read for PatternReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        let want = out
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        fill_model(&mut out[..want], self.offset);
        self.offset += want as u64;
        self.remaining -= want as u64;
        Ok(want)
    }
}

/// Fails after forwarding exactly `fail_after` bytes.
struct FailingReader {
    inner: PatternReader,
    fail_after: u64,
    sent: u64,
}

impl Read for FailingReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.sent >= self.fail_after {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "injected streaming reader failure",
            ));
        }
        let allowed = usize::try_from((self.fail_after - self.sent).min(out.len() as u64))
            .unwrap_or(out.len());
        let filled = self.inner.read(&mut out[..allowed])?;
        self.sent += filled as u64;
        Ok(filled)
    }
}

// ---- environment helpers ----------------------------------------------------

fn free_bytes(path: &Path) -> u64 {
    let c_path = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // SAFETY: `statvfs` only writes into the provided, fully initialised struct.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    assert_eq!(rc, 0, "statvfs failed for {}", path.display());
    (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
}

fn require_space(path: &Path, needed: u64, what: &str) {
    let available = free_bytes(path);
    assert!(
        available >= needed,
        "insufficient tmpfs space for {what} at {}: need {} bytes ({} MiB), available {} bytes ({} MiB); \
         the court /tmp is a 1 GiB tmpfs and the store is deliberately not silently skipped",
        path.display(),
        needed,
        needed / MIB,
        available,
        available / MIB
    );
}

fn vm_hwm_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
                .expect("parse VmHWM");
        }
    }
    panic!("VmHWM missing from /proc/self/status");
}

fn fresh(chunk_size: u64) -> (TempDir, Store) {
    let dir = tempdir().expect("temp dir on the court tmpfs");
    let store = Store::init(dir.path(), opts(chunk_size)).expect("init store");
    (dir, store)
}

fn assert_range(store: &Store, id: weftfs::Hash32, offset: u64, len: usize) {
    let actual = store
        .read_range(id, offset, len as u64)
        .expect("range read");
    assert_eq!(
        actual,
        model_at(offset, len),
        "bytes disagree with the model at offset {offset} length {len}"
    );
}

// ---- tests ------------------------------------------------------------------

#[test]
fn streaming_create_512mib_is_bounded_memory_and_exact() {
    let _guard = big_lock();
    let dir = tempdir().expect("temp dir");
    require_space(dir.path(), 600 * MIB, "the 512 MiB streaming create");

    let mut store = Store::init(dir.path(), opts(65_536)).expect("init store");
    let root = store.resolve_path("/").expect("root");
    let size = 512 * MIB;

    let before_hwm = vm_hwm_kib();
    let id = store
        .create_file_streaming(
            root,
            b"big.bin",
            PatternReader::new(size),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect("streaming create");
    let peak_hwm = vm_hwm_kib();
    println!(
        "streaming create: size={size} bytes, VmHWM before={before_hwm} KiB, after={peak_hwm} KiB, \
         bound={PEAK_RSS_BOUND_KIB} KiB, STREAM_IO_BYTES={STREAM_IO_BYTES}"
    );

    assert!(
        peak_hwm < PEAK_RSS_BOUND_KIB,
        "peak RSS {peak_hwm} KiB exceeds the documented {PEAK_RSS_BOUND_KIB} KiB bound"
    );
    assert_eq!(store.object_meta(id).unwrap().size, size);
    assert_eq!(store.generation(), 2, "create publishes exactly one commit");

    assert_range(&store, id, size / 2, MIB as usize);
    assert_range(&store, id, 0, 4096);
    assert_range(&store, id, size - 4096, 4096);

    let report = store.verify(true).expect("verify");
    assert!(report.ok, "verify problems: {:?}", report.problems);
    assert_eq!(report.checked_objects, 1);
}

#[test]
fn streaming_append_is_exact_and_one_generation_per_call() {
    let _guard = big_lock();
    let dir = tempdir().expect("temp dir");
    require_space(dir.path(), 200 * MIB, "the 64 MiB + 64 MiB append");

    let mut store = Store::init(dir.path(), opts(65_536)).expect("init store");
    let root = store.resolve_path("/").expect("root");
    let first = 64 * MIB;
    let second = 64 * MIB;

    let id = store
        .create_file_streaming(
            root,
            b"append.bin",
            PatternReader::new(first),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect("streaming create");
    assert_eq!(store.generation(), 2);

    let version = store
        .append_file_streaming(id, PatternReader::at(first, second))
        .expect("streaming append");
    assert_eq!(store.generation(), 3, "append publishes exactly one commit");

    let meta = store.object_meta(id).unwrap();
    assert_eq!(meta.size, first + second);
    assert_eq!(meta.version, version);

    // Compare every byte in bounded windows rather than materialising 128 MiB.
    let mut offset = 0u64;
    while offset < first + second {
        let len = (MIB).min(first + second - offset) as usize;
        assert_range(&store, id, offset, len);
        offset += len as u64;
    }

    let report = store.verify(true).expect("verify");
    assert!(report.ok, "verify problems: {:?}", report.problems);
}

#[test]
fn streaming_create_error_commits_nothing() {
    let (_dir, mut store) = fresh(65_536);
    let root = store.resolve_path("/").expect("root");
    let before_generation = store.generation();
    let before = store.current_state().clone();

    let err = store
        .create_file_streaming(
            root,
            b"broken.bin",
            FailingReader {
                inner: PatternReader::new(16 * MIB),
                fail_after: 3 * MIB + 17,
                sent: 0,
            },
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect_err("reader failure must fail the create");
    assert_eq!(err.class_name(), "Io", "unexpected error: {err}");

    assert_eq!(store.generation(), before_generation);
    let after = store.current_state();
    assert_eq!(after.objects, before.objects);
    assert_eq!(after.entries, before.entries);
    assert_eq!(after.relations, before.relations);
    assert_eq!(after.snapshots, before.snapshots);
    assert_eq!(after.tips, before.tips);
    assert!(store.resolve_path("/broken.bin").is_err());

    let report = store.verify(true).expect("verify");
    assert!(report.ok, "verify problems: {:?}", report.problems);
}

#[test]
fn streaming_create_boundary_sizes() {
    for size in [4095u64, 4096, 4097, 8192] {
        let (_dir, mut store) = fresh(4096);
        let root = store.resolve_path("/").expect("root");
        let id = store
            .create_file_streaming(
                root,
                b"boundary.bin",
                PatternReader::new(size),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .expect("streaming create");
        assert_eq!(store.object_meta(id).unwrap().size, size);
        let bytes = store.read_object(id).expect("read object");
        assert_eq!(bytes, model_at(0, size as usize), "size {size}");
        let report = store.verify(true).expect("verify");
        assert!(report.ok, "verify problems: {:?}", report.problems);
    }
}

#[test]
fn codec_auto_streaming_large_uses_v2_raw_manifest() {
    let dir = tempdir().expect("temp dir");
    let mut options = opts(4096);
    options.representation = RepresentationMode::CodecAuto;
    options.codec_min_bytes = 4096;
    let mut store = Store::init(dir.path(), options).expect("init store");
    let root = store.resolve_path("/").expect("root");

    let size = 64 * 1024u64;
    let id = store
        .create_file_streaming(
            root,
            b"auto.bin",
            PatternReader::new(size),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect("streaming create");
    let record = store.object_meta(id).unwrap();
    let manifest = read_manifest(store.root(), record.content).expect("manifest");
    let representation = manifest
        .representation
        .as_ref()
        .expect("CodecAuto streaming writes a v2 manifest");
    assert_eq!(representation.strategy, "raw");
    assert_eq!(representation.decoded_size, size);
    assert!(representation.frame.is_none());

    let bytes = store.read_object(id).expect("read object");
    assert_eq!(bytes, model_at(0, size as usize));
}

#[test]
fn streaming_and_buffered_create_records_match() {
    let (_dir_a, mut buffered_store) = fresh(4096);
    let (_dir_b, mut streaming_store) = fresh(4096);
    let root_a = buffered_store.resolve_path("/").expect("root");
    let root_b = streaming_store.resolve_path("/").expect("root");
    let data = model_at(0, 10_000);
    let seed = b"same-object-seed";

    let buffered = buffered_store
        .create_file_seeded(
            root_a,
            b"same.bin",
            &data,
            0o640,
            7,
            9,
            BTreeMap::new(),
            seed,
        )
        .expect("buffered create");
    let streaming = streaming_store
        .create_file_streaming_seeded(
            root_b,
            b"same.bin",
            PatternReader::new(data.len() as u64),
            0o640,
            7,
            9,
            BTreeMap::new(),
            seed,
        )
        .expect("streaming create");

    assert_eq!(buffered, streaming, "same seed yields the same ObjectId");
    let after_a = buffered_store.current_state();
    let after_b = streaming_store.current_state();
    assert_eq!(after_a.objects, after_b.objects);
    assert_eq!(after_a.entries, after_b.entries);
    assert_eq!(after_a.tips, after_b.tips);
    assert_eq!(buffered_store.generation(), streaming_store.generation());
}

#[test]
fn append_zero_bytes_is_a_no_op() {
    let (_dir, mut store) = fresh(4096);
    let root = store.resolve_path("/").expect("root");
    let id = store
        .create_file_streaming(
            root,
            b"noop.bin",
            PatternReader::new(100),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect("streaming create");
    let version = store.object_meta(id).unwrap().version;
    let generation = store.generation();
    let returned = store
        .append_file_streaming(id, PatternReader::new(0))
        .expect("empty append");
    assert_eq!(returned, version);
    assert_eq!(store.generation(), generation, "no commit for empty append");
    assert_eq!(store.read_object(id).unwrap(), model_at(0, 100));
}
