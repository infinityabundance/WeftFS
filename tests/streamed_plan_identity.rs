//! Identity court for the bounded streaming planned write (X04).
//!
//! The streaming writer is a memory schedule, not a representation change. The
//! proof is total: two stores are created with the same options (including a
//! fixed clock) and the same ObjectId seed, one written through the slice
//! planner and one through the bounded source writer. **Every file in both
//! store trees must have the same relative path and the same bytes**, and the
//! head, ObjectId, manifest-determined attributes and decoded tensor must match.
//!
//! A representation that the streaming writer cannot produce is refused with a
//! typed error naming the sharded checkpoint writer; those refusals are asserted
//! here too, so the policy cannot regress into a silent fallback.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    StreamedWriteOptions, TensorLayoutSpec, TensorStrategy, TensorWriteSpec,
};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// BF16 payload with the skewed exponent distribution real weights have, so
/// mode 9 is a real candidate and the raw/zstd/mode-9 decision is exercised.
fn bf16_payload(elements: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(elements * 2);
    while out.len() < elements * 2 {
        let pick = rng.next();
        let exponent = match pick % 16 {
            0..=7 => 127u16,
            8..=12 => 126,
            13 => 125,
            _ => (pick >> 8) as u16 & 0xff,
        };
        let word = ((pick >> 16) as u16 & 0x8000) | (exponent << 7) | (pick as u16 & 0x7f);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.truncate(elements * 2);
    out
}

fn fp32_payload(elements: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(elements * 4);
    while out.len() < elements * 4 {
        let pick = rng.next();
        let exponent = match pick % 8 {
            0..=4 => 127u32,
            5..=6 => 126,
            _ => 124,
        };
        let word =
            ((pick >> 33) as u32 & 0x8000_0000) | (exponent << 23) | (pick as u32 & 0x007f_ffff);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.truncate(elements * 4);
    out
}

fn options() -> Options {
    Options {
        chunk_size: 65_536,
        checkpoint_interval: 256,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(root, &path, out);
            } else if meta.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let bytes = fs::read(&path).unwrap_or_default();
                let digest = hex::encode(Sha256::digest(&bytes));
                out.insert(relative, digest);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn fresh(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "weft-stream-identity-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

struct Written {
    object_id: weftfs::identity::Hash32,
    head: weftfs::identity::Hash32,
    bytes: Vec<u8>,
    logical_root: weftfs::identity::Hash32,
    strategy: String,
    tree: BTreeMap<String, String>,
}

fn run_case(
    name: &str,
    dtype: &str,
    shape: &[u64],
    payload: &[u8],
    spec: &TensorWriteSpec,
    stream: &StreamedWriteOptions,
) -> (Written, Written) {
    let seed = [0x5au8; 32];
    let layout = TensorLayoutSpec::row_major();

    // A: the slice planner.
    let dir_a = fresh(&format!("{name}-a"));
    let written_a = {
        let mut store = Store::init(&dir_a, options()).expect("init a");
        let root = store.resolve_path("/").expect("resolve /");
        let dir = store
            .mkdir_seeded(root, b"t", 0o755, 0, 0, &[0x11u8; 32])
            .expect("mkdir");
        let object_id = store
            .write_tensor_file_with_spec_seeded(
                dir, b"x", dtype, shape, payload, &layout, spec, &seed,
            )
            .expect("slice write");
        let meta = store.tensor_metadata(object_id).expect("metadata");
        let bytes = store.read_object(object_id).expect("read back");
        Written {
            object_id,
            head: store.head(),
            bytes,
            logical_root: meta.logical_root,
            strategy: meta.strategy,
            tree: tree(&dir_a),
        }
    };

    // B: the bounded source writer.
    let dir_b = fresh(&format!("{name}-b"));
    let written_b = {
        let mut store = Store::init(&dir_b, options()).expect("init b");
        let root = store.resolve_path("/").expect("resolve /");
        let dir = store
            .mkdir_seeded(root, b"t", 0o755, 0, 0, &[0x11u8; 32])
            .expect("mkdir");
        let mut source: &[u8] = payload;
        let object_id = store
            .write_tensor_file_from_source(
                dir,
                b"x",
                dtype,
                shape,
                &mut source,
                &layout,
                spec,
                &seed,
                stream,
            )
            .expect("streamed write");
        let meta = store.tensor_metadata(object_id).expect("metadata");
        let bytes = store.read_object(object_id).expect("read back");
        Written {
            object_id,
            head: store.head(),
            bytes,
            logical_root: meta.logical_root,
            strategy: meta.strategy,
            tree: tree(&dir_b),
        }
    };
    (written_a, written_b)
}

fn assert_identical(name: &str, a: &Written, b: &Written, payload: &[u8]) {
    assert_eq!(a.object_id, b.object_id, "{name}: ObjectId differs");
    assert_eq!(
        a.logical_root, b.logical_root,
        "{name}: logical root differs"
    );
    assert_eq!(a.strategy, b.strategy, "{name}: strategy differs");
    assert_eq!(a.bytes, payload, "{name}: slice write did not round-trip");
    assert_eq!(
        b.bytes, payload,
        "{name}: streamed write did not round-trip"
    );
    assert_eq!(
        a.tree, b.tree,
        "{name}: store trees differ (relative path or bytes)"
    );
    assert_eq!(a.head, b.head, "{name}: head differs");
}

#[test]
fn streamed_and_slice_planned_writes_are_byte_identical() {
    // Sizes chosen so that mode 9, ZSTD and raw all win on at least one case,
    // and so the ingest window is crossed several times.
    let cases: [(&str, &str, u64, u64); 4] = [
        ("bf16-small", "bf16", 2_048, 9),
        ("bf16-multi-window", "bf16", 300_001, 11),
        ("f32-small", "f32", 4_096, 13),
        ("bf16-wide", "bf16", 1_048_577, 17),
    ];
    for (name, dtype, elements, seed) in cases {
        let payload = match dtype {
            "bf16" => bf16_payload(elements as usize, seed),
            _ => fp32_payload(elements as usize, seed),
        };
        for window in [4096usize, 1 << 20] {
            let stream = StreamedWriteOptions {
                window_bytes: window,
                ..StreamedWriteOptions::default()
            };
            let spec = TensorWriteSpec::default();
            let (a, b) = run_case(name, dtype, &[elements], &payload, &spec, &stream);
            assert_identical(name, &a, &b, &payload);
        }
    }
}

#[test]
fn the_streamed_writer_refuses_requests_it_cannot_reproduce() {
    let payload = bf16_payload(4_096, 3);
    let dir = fresh("refusals");
    let mut store = Store::init(&dir, options()).expect("init");
    let root = store.resolve_path("/").expect("resolve /");
    let dir_id = store
        .mkdir_seeded(root, b"t", 0o755, 0, 0, &[0x11u8; 32])
        .expect("mkdir");
    let layout = TensorLayoutSpec::row_major();
    let seed = [1u8; 32];
    let stream = StreamedWriteOptions::default();
    let shape = [4_096u64];
    let before_head = store.head();
    let before_generation = store.generation();

    let cases: Vec<(&str, TensorWriteSpec, TensorLayoutSpec, &str)> = vec![
        (
            "strategy hint",
            TensorWriteSpec {
                strategy_hint: Some(TensorStrategy::Zstd),
                ..TensorWriteSpec::default()
            },
            TensorLayoutSpec::row_major(),
            "pinned strategy hint",
        ),
        (
            "fast policy",
            TensorWriteSpec {
                plan: weftfs::tensor_store::TensorPlanOptions {
                    write_policy: weftfs::tensor_store::WritePolicy::Fast,
                    ..weftfs::tensor_store::TensorPlanOptions::default()
                },
                ..TensorWriteSpec::default()
            },
            TensorLayoutSpec::row_major(),
            "fast policy",
        ),
        (
            "causal base",
            TensorWriteSpec {
                causal: vec![weftfs::identity::Hash32::ZERO],
                ..TensorWriteSpec::default()
            },
            TensorLayoutSpec::row_major(),
            "delta candidates",
        ),
        (
            "column major",
            TensorWriteSpec::default(),
            TensorLayoutSpec::column_major(),
            "row-major",
        ),
    ];
    for (label, spec, layout, needle) in cases {
        let mut source: &[u8] = &payload;
        let err = store
            .write_tensor_file_from_source(
                dir_id,
                b"y",
                "bf16",
                &shape,
                &mut source,
                &layout,
                &spec,
                &seed,
                &stream,
            )
            .expect_err(&format!("{label} must be refused"));
        assert!(
            err.message().contains(needle),
            "{label}: message `{}` does not name `{needle}`",
            err.message()
        );
    }
    // Nothing above published a state op, a blob, or a generation.
    assert_eq!(store.head(), before_head);
    assert_eq!(store.generation(), before_generation);
}
