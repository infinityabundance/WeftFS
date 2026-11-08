//! Stage 4: recovery-first restore.
//!
//! The required set (the tensors a resumed training step needs) is streamed and
//! verified first; the loader reports time-to-first-usable, the bytes fetched
//! before it, and time-to-complete. A host-side verified cache is reused only
//! after its SHA-256, BLAKE3 content root and logical root are re-checked
//! against the checkpoint manifest.

use std::fs;
use std::io::{Seek, SeekFrom, Write};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::recovery_first::RecoveryFirstOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{Options, Store};

const SHAPES: [([u64; 2], &str); 3] = [([64, 32], "embed"), ([32, 32], "attn"), ([16, 64], "mlp")];

fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
    let path = std::env::temp_dir().join(format!(
        "weft-recovery-first-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if path.exists() {
        fs::remove_dir_all(&path).unwrap();
    }
    let store = Store::init(
        &path,
        Options {
            chunk_size: 64 * 1024,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .unwrap();
    (path, store)
}

fn bf16(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len * 2);
    let mut state = seed | 1;
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32);
        let value = (unit * 2.0 - 1.0) * 0.5;
        out.extend_from_slice(&((value.to_bits() >> 16) as u16).to_le_bytes());
    }
    out
}

fn container(dir: &std::path::Path) -> std::path::PathBuf {
    let mut data = Vec::new();
    for (index, (shape, name)) in SHAPES.iter().enumerate() {
        let elements = shape.iter().product::<u64>() as usize;
        let payload = bf16(elements, 0x5eed_0000 + index as u64);
        data.push((*name, *shape, payload));
    }
    let entries: Vec<SafetensorsEntry> = data
        .iter()
        .map(|(name, shape, payload)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape,
            data: payload,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None).unwrap();
    let path = dir.join("weights.safetensors");
    fs::write(&path, file).unwrap();
    path
}

fn write_checkpoint(store: &mut Store, source: &std::path::Path) {
    store
        .write_checkpoint(
            "model",
            source,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters: Vec::new(),
                strategy: ShardStrategy::Zstd,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
}

#[test]
fn required_set_is_first_and_the_container_is_exact() {
    let (path, mut store) = temp_store("basic");
    let source = container(&path);
    write_checkpoint(&mut store, &source);

    let out_path = path.join("restored.safetensors");
    let mut out = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&out_path)
        .unwrap();
    let report = store
        .restore_checkpoint_recovery_first(
            "model",
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required: vec!["mlp".to_owned()],
                cache_dir: None,
                abort_after_required: false,
            },
            &mut out,
        )
        .unwrap();
    assert!(report.exact_source_file);
    assert_eq!(report.tensors.len(), 3);
    assert_eq!(report.required_order[0].0, "mlp");
    assert!(
        report.time_to_first_usable_ms <= report.time_to_complete_ms,
        "first usable {} > complete {}",
        report.time_to_first_usable_ms,
        report.time_to_complete_ms
    );
    assert!(
        report.bytes_fetched_before_first_usable <= report.bytes_fetched_total,
        "bytes before first usable {} > total {}",
        report.bytes_fetched_before_first_usable,
        report.bytes_fetched_total
    );
    out.flush().unwrap();
    out.seek(SeekFrom::Start(0)).unwrap();
    let mut restored = Vec::new();
    std::io::copy(&mut out, &mut restored).unwrap();
    assert_eq!(
        safetensors::sha256(&restored),
        safetensors::sha256(&fs::read(&source).unwrap())
    );
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn verified_cache_is_reused_and_revalidated() {
    let (path, mut store) = temp_store("cache");
    let source = container(&path);
    write_checkpoint(&mut store, &source);
    let cache = path.join("cache");

    let mut first_out = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path.join("first.safetensors"))
        .unwrap();
    let first = store
        .restore_checkpoint_recovery_first(
            "model",
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required: vec!["mlp".to_owned()],
                cache_dir: Some(cache.clone()),
                abort_after_required: false,
            },
            &mut first_out,
        )
        .unwrap();
    assert!(first.cached_required.is_empty());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);

    let mut second_out = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path.join("second.safetensors"))
        .unwrap();
    let second = store
        .restore_checkpoint_recovery_first(
            "model",
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required: vec!["mlp".to_owned()],
                cache_dir: Some(cache.clone()),
                abort_after_required: false,
            },
            &mut second_out,
        )
        .unwrap();
    assert_eq!(second.cached_required.len(), 1);
    assert_eq!(second.cached_required[0].name, "mlp");
    assert!(second.exact_source_file);
    assert!(
        second.bytes_fetched_before_first_usable <= first.bytes_fetched_before_first_usable,
        "cached first-usable fetch {} should not exceed the cold {}",
        second.bytes_fetched_before_first_usable,
        first.bytes_fetched_before_first_usable
    );

    // Corrupt the cache entry: the loader must fall back to the store, not
    // serve the mutated bytes.
    let entry = fs::read_dir(&cache)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = fs::read(&entry).unwrap();
    let value = weftfs::wcb_util::decode(&bytes).unwrap();
    let mut map = weftfs::wcb_util::as_map(&value).unwrap().clone();
    let data = weftfs::wcb_util::get_bytes(&map, "data").unwrap();
    let mut mutated = data.clone();
    let mid = mutated.len() / 2;
    mutated[mid] ^= 0xff;
    map.insert("data".to_owned(), weftcodec::wcb::Value::Bytes(mutated));
    // The recorded sha256/content_root still describe the old bytes, so the
    // loader must reject the entry and fall back to the store.
    let encoded = weftfs::wcb_util::encode(&weftcodec::wcb::Value::Map(map)).unwrap();
    fs::write(&entry, &encoded).unwrap();
    let mut third_out = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path.join("third.safetensors"))
        .unwrap();
    let third = store
        .restore_checkpoint_recovery_first(
            "model",
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required: vec!["mlp".to_owned()],
                cache_dir: Some(cache),
                abort_after_required: false,
            },
            &mut third_out,
        )
        .unwrap();
    assert!(
        third.cached_required.is_empty(),
        "a tampered cache entry is not used"
    );
    assert!(third.exact_source_file);
    drop(store);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn unknown_required_tensor_is_a_typed_error() {
    let (path, mut store) = temp_store("missing");
    let source = container(&path);
    write_checkpoint(&mut store, &source);
    let mut out = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path.join("out.safetensors"))
        .unwrap();
    let err = store
        .restore_checkpoint_recovery_first(
            "model",
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required: vec!["no-such-tensor".to_owned()],
                cache_dir: None,
                abort_after_required: false,
            },
            &mut out,
        )
        .unwrap_err();
    assert_eq!(err.class(), weftfs::error::Class::NotFound);
    drop(store);
    let _ = fs::remove_dir_all(&path);
}
