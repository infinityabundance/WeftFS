//! Mode 19 `KV_CACHE_PROV` versus mode 12 `KV_CACHE`: cost of provenance.
//!
//! Rows, per decoded page size (`1024` and `65536` bytes):
//!
//! * encode (mode 12, mode 19);
//! * decode (mode 12, mode 19);
//! * descriptor validation alone (`KvProvDescriptor::from_canonical_bytes` plus
//!   `root()`), which is the work mode 19 adds before the payload is touched;
//! * authorized store read (mode 12 `read_kv_page`, mode 19
//!   `read_kv_prov_page`) through a real `weft.capability.envelope.v1`.
//!
//! Bytes are reported as frame size, descriptor size, and the mode-19 overhead
//! over mode 12 for the same page. Latencies are wall-clock distributions; the
//! court is one host with a warm cache, so these are comparative numbers, not a
//! throughput claim.
//!
//! Geometry: BF16, 4 heads, head_dim 64 (512 bytes per row), deterministic
//! element bytes. Every run re-derives the descriptor and verifies the decoded
//! bytes equal the input before a row is accepted.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, CapabilityEnvelope, VerifiedEnvelope,
};
use weftcodec::frame::Frame;
use weftcodec::kv_cache::{self, KvCacheInput, KvDtype};
use weftcodec::kv_prov::{
    decode_kv_prov_page, encode_kv_prov_page, KvAttention, KvLayout, KvPositional,
    KvProvDescriptor, KvProvenanceInput,
};
use weftcodec::limits::Limits;

use weftfs::identity::Hash32;
use weftfs::store::{Options, Store};
use weftfs::Principal;

const TENANT: &str = "tenant-a";
const HEADS: u32 = 4;
const HEAD_DIM: u32 = 64;
const WORD_BYTES: u64 = 2;
const NOW_NS: i64 = 1_700_000_000_000_000_000;
const EXPIRES_NS: i64 = 2_000_000_000_000_000_000;
const GRANTOR_SEED: [u8; 32] = [7u8; 32];
const REPS: usize = 40;

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn distribution(samples_ns: &[u64]) -> Json {
    if samples_ns.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted = samples_ns.to_vec();
    sorted.sort_unstable();
    let pick = |pct: usize| -> u64 { sorted[((sorted.len() * pct) / 100).min(sorted.len() - 1)] };
    let sum: u128 = sorted.iter().map(|value| u128::from(*value)).sum();
    json!({
        "samples": sorted.len(),
        "min_us": round(sorted[0] as f64 / 1000.0),
        "p50_us": round(pick(50) as f64 / 1000.0),
        "p95_us": round(pick(95) as f64 / 1000.0),
        "max_us": round(sorted[sorted.len() - 1] as f64 / 1000.0),
        "mean_us": round(sum as f64 / sorted.len() as f64 / 1000.0),
        "p50_ns": pick(50),
        "p95_ns": pick(95),
    })
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        }
    }
    None
}

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(NOW_NS),
        ..Options::default()
    }
}

fn input(page_bytes: u64) -> KvCacheInput {
    let rows = page_bytes / (u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES);
    KvCacheInput {
        model: "qwen2.5-0.5b@060db649".to_owned(),
        layer: 3,
        session: "session-bench".to_owned(),
        page_index: 0,
        page_count: 1,
        seq_start: 0,
        seq_end: rows,
        dtype: KvDtype::Bf16,
        head_count: HEADS,
        head_dim: HEAD_DIM,
    }
}

fn elements(page_bytes: u64) -> Vec<u8> {
    let words = (page_bytes / WORD_BYTES) as usize;
    let mut out = Vec::with_capacity(words * 2);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..words {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&(state as u16).to_le_bytes());
    }
    out
}

fn provenance(page_rows: u64) -> KvProvenanceInput {
    let mut provenance = KvProvenanceInput::new(
        Hash32::from_bytes([0x11; 32]),
        "qwen-bpe",
        151_936,
        KvAttention {
            head_count: HEADS,
            head_dim: HEAD_DIM,
            sliding_window: 0,
            rope_theta_bits: 10_000.0f64.to_bits(),
            rope_scale_bits: 1.0f64.to_bits(),
        },
        KvPositional::Rope,
        KvLayout {
            dtype: KvDtype::Bf16,
            page_rows,
            layer_count: 24,
        },
        Hash32::from_bytes([0xA0; 32]),
    );
    provenance = provenance.with_adapter(Hash32::from_bytes([0x21; 32]));
    provenance = provenance.with_adapter(Hash32::from_bytes([0x22; 32]));
    provenance = provenance.with_extension(
        "weft.bench.runtime",
        weftcodec::wcb::Value::Str("bench-v1".to_owned()),
    );
    provenance
}

fn grant(store: &mut Store, roots: Vec<Hash32>) -> VerifiedEnvelope {
    let envelope = CapabilityEnvelope::new(
        public_key_for_seed(&GRANTOR_SEED).unwrap(),
        Hash32::from_bytes([9; 32]),
        TENANT,
        roots,
        vec!["kv.tenant-a".to_owned()],
        Vec::new(),
        Some(EXPIRES_NS),
        None,
        Hash32::from_bytes([0xA1; 32]),
        NOW_NS,
        Hash32::from_bytes([0xA2; 32]),
    )
    .unwrap();
    let blob = sign_envelope(&envelope, &GRANTOR_SEED).unwrap();
    let id = store.save_capability_envelope(&blob).unwrap();
    store.load_capability_envelope(id, &[], NOW_NS).unwrap()
}

fn principal(verified: VerifiedEnvelope) -> Principal {
    Principal::new(1001, 1001, Vec::new()).with_envelope(verified)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let out = value("--out");
    let dir = value("--dir").map(PathBuf::from).unwrap_or_else(|| {
        let base = if Path::new("/mnt/weft-bench").is_dir() {
            PathBuf::from("/mnt/weft-bench")
        } else {
            std::env::temp_dir()
        };
        base.join(format!("kv-prov-bench-{}", std::process::id()))
    });
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create bench dir");

    let limits = Limits::default();
    let mut store = Store::init(&dir, options()).expect("init store");
    let root_id = store.resolve_path("/").unwrap();
    let mut rows = Vec::new();

    for page_bytes in [1024u64, 65536u64] {
        let page_input = input(page_bytes);
        let page_rows = page_bytes / (u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES);
        let page = elements(page_bytes);
        let provenance = provenance(page_rows);

        let mode12_frame =
            kv_cache::encode_kv_page(&page_input, &page, &limits).expect("mode-12 encode");
        let mode19_frame =
            encode_kv_prov_page(&page_input, &page, &provenance, &limits).expect("mode-19 encode");
        let mode12_parsed = Frame::parse(&mode12_frame, &limits).unwrap();
        let mode19_parsed = Frame::parse(&mode19_frame, &limits).unwrap();
        let body = mode19_parsed.body;
        let descriptor_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
        let descriptor_bytes = &body[4..4 + descriptor_len];

        // Exactness before timing.
        let decoded12 = kv_cache::decode_kv_page(&mode12_parsed, &limits).expect("mode-12 decode");
        let decoded19 = decode_kv_prov_page(&mode19_parsed, &limits).expect("mode-19 decode");
        assert_eq!(decoded12.bytes, page);
        assert_eq!(decoded19.bytes, page);
        assert_eq!(decoded19.bytes.len() as u64, decoded12.bytes.len() as u64);

        let mut encode12 = Vec::with_capacity(REPS);
        let mut encode19 = Vec::with_capacity(REPS);
        let mut decode12 = Vec::with_capacity(REPS);
        let mut decode19 = Vec::with_capacity(REPS);
        let mut descriptor_check = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let started = Instant::now();
            let frame = kv_cache::encode_kv_page(&page_input, &page, &limits).unwrap();
            encode12.push(started.elapsed().as_nanos() as u64);
            let started = Instant::now();
            let frame19 = encode_kv_prov_page(&page_input, &page, &provenance, &limits).unwrap();
            encode19.push(started.elapsed().as_nanos() as u64);
            assert_eq!(frame, mode12_frame);
            assert_eq!(frame19, mode19_frame);

            let parsed12 = Frame::parse(&mode12_frame, &limits).unwrap();
            let started = Instant::now();
            let _ = kv_cache::decode_kv_page(&parsed12, &limits).unwrap();
            decode12.push(started.elapsed().as_nanos() as u64);

            let parsed19 = Frame::parse(&mode19_frame, &limits).unwrap();
            let started = Instant::now();
            let _ = decode_kv_prov_page(&parsed19, &limits).unwrap();
            decode19.push(started.elapsed().as_nanos() as u64);

            let started = Instant::now();
            let descriptor =
                KvProvDescriptor::from_canonical_bytes(descriptor_bytes, &limits).unwrap();
            let _ = descriptor.root(&limits).unwrap();
            descriptor_check.push(started.elapsed().as_nanos() as u64);
        }

        // Authorized store reads: one object per mode, one grant per descriptor.
        let plain = store
            .write_kv_page(
                root_id,
                format!("bench-plain-{page_bytes}.kv").as_bytes(),
                TENANT,
                &page_input,
                &page,
                1000,
                1000,
            )
            .unwrap();
        let prov = store
            .write_kv_prov_page(
                root_id,
                format!("bench-prov-{page_bytes}.kv").as_bytes(),
                TENANT,
                &page_input,
                &page,
                &provenance,
                1000,
                1000,
            )
            .unwrap();
        let plain_principal = principal(grant(&mut store, vec![plain.descriptor_id]));
        let prov_principal = principal(grant(&mut store, vec![prov.descriptor_id]));
        let mut read12 = Vec::with_capacity(REPS);
        let mut read19 = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let started = Instant::now();
            let page12 = store
                .read_kv_page(plain.object_id, &plain_principal, NOW_NS)
                .unwrap();
            read12.push(started.elapsed().as_nanos() as u64);
            assert_eq!(page12.bytes, page);

            let started = Instant::now();
            let page19 = store
                .read_kv_prov_page(prov.object_id, &prov_principal, NOW_NS)
                .unwrap();
            read19.push(started.elapsed().as_nanos() as u64);
            assert_eq!(page19.bytes, page);
        }

        let overhead_bytes = mode19_frame.len() as u64 - mode12_frame.len() as u64;
        let overhead_pct = overhead_bytes as f64 / mode12_frame.len() as f64 * 100.0;
        rows.push(json!({
            "page_bytes": page_bytes,
            "page_rows": page_rows,
            "mode12_frame_bytes": mode12_frame.len(),
            "mode19_frame_bytes": mode19_frame.len(),
            "descriptor_bytes": descriptor_bytes.len(),
            "overhead_bytes": overhead_bytes,
            "overhead_pct": round(overhead_pct),
            "mode12_encode": distribution(&encode12),
            "mode19_encode": distribution(&encode19),
            "mode12_decode": distribution(&decode12),
            "mode19_decode": distribution(&decode19),
            "descriptor_check": distribution(&descriptor_check),
            "mode12_authorized_read": distribution(&read12),
            "mode19_authorized_read": distribution(&read19),
            "provenance_root": decoded19.root.to_hex(),
            "descriptor_id": prov.descriptor_id.to_hex(),
        }));
    }

    let receipt = json!({
        "schema": "weft.bench.kv-provenance.v1",
        "fixture": {
            "dtype": "bf16",
            "head_count": HEADS,
            "head_dim": HEAD_DIM,
            "bytes_per_row": u64::from(HEADS) * u64::from(HEAD_DIM) * WORD_BYTES,
            "adapter_count": 2,
            "extension_count": 1,
            "reps": REPS,
            "element_bytes": "deterministic xorshift64* words (bf16)",
        },
        "labels": [
            "one host, warm cache; comparative numbers, not a throughput claim",
            "authorized reads include capability verification and POSIX checks",
            "synthetic page bytes, not captured model state",
        ],
        "rows": rows,
        "peak_rss_kib": peak_rss_kib(),
    });

    let text = serde_json::to_string_pretty(&receipt).expect("serialize receipt");
    match out {
        Some(path) => {
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, format!("{text}\n")).expect("write receipt");
        }
        None => println!("{text}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
