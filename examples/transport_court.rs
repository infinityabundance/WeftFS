//! `weft.transport.v1` court (subphase R8).
//!
//! Measures, on frames built from one real Qwen2.5-0.5B tensor:
//!
//! 1. header+table parse cost and its scaling with section count;
//! 2. a single-section fetch through `BlobPlane::get_range`, reporting exactly
//!    how many bytes the aligned read costs against the enclosed frame and the
//!    whole container;
//! 3. container overhead versus concatenated frames at 512/4096/65536;
//! 4. a 4 KiB-aligned slice inside a mode-17 section: the slice is aligned and
//!    fetchable, the section root still verifies for the full section, and the
//!    per-tile root (not the section root) is the root that applies to a slice;
//! 5. the alignment invariant, including a deliberate off-by-one rejection;
//! 6. the losses: containers do not reduce bytes, add padding, and cannot by
//!    themselves prove zero-copy.
//!
//! This is a layout court on one host with a warm page cache. It makes no DMA,
//! GPU-memory, RDMA, deployed-3FS, throughput, or fleet claim.

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftcodec::engram::{self, EngramInput};
use weftcodec::error::Error as CodecError;
use weftcodec::hash::{hash_domain, DOMAIN_CHUNK};
use weftcodec::limits::Limits;
use weftcodec::modes::{self, FloatDtype, TensorDtype};
use weftcodec::transport::{self, RangeReader, SectionRole, TransportFrame};

use weftfs::backend::{Key, LocalBackend};
use weftfs::identity::Hash32;
use weftfs::planes::BlobPlane;
use weftfs::StorageBackend;
use weftfs::safetensors::{sha256_path_hex, SafetensorsFile};

const TENSOR_NAME: &str = "model.layers.0.mlp.down_proj.weight";
const CORPUS_SHA256: &str = "fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe";

fn main() {
    if let Err(err) = run() {
        eprintln!("transport_court: {err}");
        std::process::exit(1);
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if samples.is_empty() {
        return 0.0;
    }
    samples[samples.len() / 2]
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

/// A byte-range source over a real `BlobPlane`, counting each request.
struct PlaneRange<'a> {
    plane: &'a LocalBackend,
    key: Key,
    reads: Cell<u64>,
    bytes: Cell<u64>,
}

impl RangeReader for PlaneRange<'_> {
    fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, CodecError> {
        self.reads.set(self.reads.get() + 1);
        self.bytes.set(self.bytes.get() + len);
        self.plane
            .get_range(&self.key, offset, len)
            .map_err(|err| CodecError::io(format!("blob plane range read failed: {err}")))
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(arg(&args, "--out").ok_or("--out FILE is required")?);
    let corpus =
        PathBuf::from(arg(&args, "--corpus").unwrap_or_else(|| {
            ".docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors".into()
        }));
    let work = PathBuf::from(
        arg(&args, "--work").unwrap_or_else(|| ".docker-cache/transport-court".into()),
    );
    if work.exists() {
        fs::remove_dir_all(&work)
            .map_err(|err| format!("cannot clean {}: {err}", work.display()))?;
    }
    fs::create_dir_all(&work).map_err(|err| format!("cannot create {}: {err}", work.display()))?;

    let limits = Limits::default();
    let started = Instant::now();
    let mut checks: Vec<(&'static str, bool)> = Vec::new();
    let mut check = |name: &'static str, ok: bool| checks.push((name, ok));

    // ---- corpus ----------------------------------------------------------
    let observed_sha = sha256_path_hex(&corpus).map_err(|err| format!("corpus hash: {err}"))?;
    let corpus_ok = observed_sha == CORPUS_SHA256;
    check("corpus_sha256", corpus_ok);
    if !corpus_ok {
        return Err(format!(
            "corpus SHA-256 {observed_sha} does not match the documented {CORPUS_SHA256}"
        ));
    }
    let container_file = SafetensorsFile::parse(&corpus)
        .map_err(|err| format!("cannot parse {}: {err}", corpus.display()))?;
    let tensor = container_file
        .tensor(TENSOR_NAME)
        .map_err(|err| err.to_string())?;
    if tensor.dtype != "bf16" {
        return Err(format!("{TENSOR_NAME} is {}, expected bf16", tensor.dtype));
    }
    let tensor_bytes = container_file
        .read_tensor(tensor)
        .map_err(|err| format!("cannot read {TENSOR_NAME}: {err}"))?;
    let shape = tensor.shape.clone();
    if shape.len() != 2 {
        return Err(format!("{TENSOR_NAME} is rank {}, expected 2", shape.len()));
    }

    // ---- frames ----------------------------------------------------------
    let mode17 =
        modes::encode_float_tiled(&tensor_bytes, FloatDtype::Bf16, &shape, &[32, 256], &limits)
            .map_err(|err| format!("float-tiled encode: {err}"))?;
    let edges: Vec<u64> = (0..=shape[0]).step_by(112).collect();
    let engram_input = EngramInput::inline(
        TensorDtype::Bf16,
        shape.clone(),
        edges,
        "ngram-blake3-v1",
        shape[0],
    );
    let mode20 = engram::encode_engram(&tensor_bytes, &engram_input, &limits)
        .map_err(|err| format!("engram encode: {err}"))?;
    let mut small: Vec<Vec<u8>> = Vec::new();
    for index in 0..6u64 {
        let text = format!("weft transport court metadata section {index}");
        small.push(
            modes::encode_raw(text.as_bytes(), None, &limits)
                .map_err(|err| format!("raw encode: {err}"))?,
        );
    }

    let main_sections: Vec<TransportFrame<'_>> = vec![
        TransportFrame::new(&mode17, SectionRole::Payload),
        TransportFrame::new(&mode20, SectionRole::Metadata),
    ]
    .into_iter()
    .chain(
        small
            .iter()
            .map(|frame| TransportFrame::new(frame.as_slice(), SectionRole::Provenance)),
    )
    .collect();
    let main = transport::create(&main_sections, 4096, &limits)
        .map_err(|err| format!("transport create: {err}"))?;
    let main_again = transport::create(&main_sections, 4096, &limits)
        .map_err(|err| format!("transport create: {err}"))?;
    check("deterministic_container", main == main_again);
    let parsed =
        transport::verify(&main, &limits).map_err(|err| format!("transport verify: {err}"))?;
    check(
        "container_verifies",
        parsed.sections.len() == main_sections.len(),
    );
    check(
        "offsets_aligned",
        parsed
            .sections
            .iter()
            .all(|section| section.offset % u64::from(section.alignment) == 0),
    );

    // ---- 1. header/table parse scaling -----------------------------------
    let parse_rows: Json = {
        let mut rows: Vec<Json> = Vec::new();
        for count in [1usize, 8, 64] {
            let frames: Vec<Vec<u8>> = (0..count)
                .map(|index| {
                    modes::encode_raw(
                        format!("parse scaling section {index}").as_bytes(),
                        None,
                        &limits,
                    )
                    .map_err(|err| format!("raw encode: {err}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let entries: Vec<TransportFrame<'_>> = frames
                .iter()
                .map(|frame| TransportFrame::new(frame.as_slice(), SectionRole::Payload))
                .collect();
            let container = transport::create(&entries, 4096, &limits)
                .map_err(|err| format!("create: {err}"))?;
            transport::verify(&container, &limits).map_err(|err| format!("verify: {err}"))?;

            let reps = if count == 64 { 200 } else { 500 };
            let mut header_table = Vec::with_capacity(reps);
            let mut metadata = Vec::with_capacity(reps);
            for _ in 0..reps {
                let start = Instant::now();
                let header = transport::read_header(&container).map_err(|err| err.to_string())?;
                let table_end = transport::HEADER_LEN
                    + header.section_count as usize * transport::SECTION_ENTRY_LEN;
                transport::read_section_table(
                    &header,
                    &container[transport::HEADER_LEN..table_end],
                )
                .map_err(|err| err.to_string())?;
                header_table.push(start.elapsed().as_secs_f64() * 1_000_000.0);

                let start = Instant::now();
                transport::parse_metadata(&container, &limits).map_err(|err| err.to_string())?;
                metadata.push(start.elapsed().as_secs_f64() * 1_000_000.0);
            }
            rows.push(json!({
                "sections": count,
                "container_bytes": container.len(),
                "header_table_us_p50": median_ms(header_table),
                "parse_metadata_us_p50": median_ms(metadata),
                "header_table_ns_per_section": median_ms(
                    transport::read_header(&container).map(|_| vec![0.0]).unwrap_or_default()) as f64,
            }));
        }
        Json::Array(rows)
    };
    // Fill in a measured ns/section figure without lying about it.
    let mut parse_rows = parse_rows;
    if let Json::Array(rows) = &mut parse_rows {
        for row in rows.iter_mut() {
            let sections = row["sections"].as_u64().unwrap_or(1).max(1);
            let us = row["header_table_us_p50"].as_f64().unwrap_or(0.0);
            row["header_table_ns_per_section"] = json!(us * 1000.0 / sections as f64);
        }
    }

    // ---- publish the container to a real BlobPlane ------------------------
    let mut backend = LocalBackend::new(&work);
    BlobPlane::init_layout(&mut backend).map_err(|err| format!("init layout: {err}"))?;
    let key = Key::Object {
        hash: Hash32::from_bytes([0x54; 32]),
    };
    backend
        .put_if_absent(&key, &main)
        .map_err(|err| format!("publish container: {err}"))?;

    // ---- 2. single-section fetch -----------------------------------------
    let reader = PlaneRange {
        plane: &backend,
        key: key.clone(),
        reads: Cell::new(0),
        bytes: Cell::new(0),
    };
    // Reading the header and table is what a single-section consumer pays
    // before it can name a section.
    let header_bytes = reader
        .read_range(0, transport::HEADER_LEN as u64)
        .map_err(|err| err.to_string())?;
    let header = transport::read_header(&header_bytes).map_err(|err| err.to_string())?;
    let table_len = u64::from(header.section_count) * transport::SECTION_ENTRY_LEN as u64;
    let table = reader
        .read_range(transport::HEADER_LEN as u64, table_len)
        .map_err(|err| err.to_string())?;
    let table_sections =
        transport::read_section_table(&header, &table).map_err(|err| err.to_string())?;
    let section = table_sections
        .get(0)
        .copied()
        .ok_or("container has no section 0")?;
    let section_bytes =
        transport::fetch_section(&reader, &section).map_err(|err| err.to_string())?;
    transport::verify_section_bytes(&section, &section_bytes, &limits)
        .map_err(|err| format!("section root: {err}"))?;
    check("single_section_root_verified", true);

    let mut section_only = Vec::new();
    let mut full_path = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let bytes = backend
            .get_range(&key, section.offset, section.length)
            .map_err(|err| format!("get_range: {err}"))?;
        if bytes.len() as u64 != section.length {
            return Err("section fetch returned the wrong length".into());
        }
        section_only.push(start.elapsed().as_secs_f64() * 1000.0);

        let start = Instant::now();
        let h = reader
            .read_range(0, transport::HEADER_LEN as u64)
            .map_err(|e| e.to_string())?;
        let h = transport::read_header(&h).map_err(|e| e.to_string())?;
        let t = reader
            .read_range(
                transport::HEADER_LEN as u64,
                u64::from(h.section_count) * transport::SECTION_ENTRY_LEN as u64,
            )
            .map_err(|e| e.to_string())?;
        let s = transport::read_section_table(&h, &t).map_err(|e| e.to_string())?;
        let _fetched = transport::fetch_section(&reader, &s[0]).map_err(|e| e.to_string())?;
        full_path.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let whole = backend
        .read_all(&key)
        .map_err(|err| format!("read_all: {err}"))?;
    let single_path_bytes = transport::HEADER_LEN as u64 + table_len + section.length;
    check(
        "single_section_bytes_below_container",
        single_path_bytes < main.len() as u64,
    );

    // ---- 3. byte overhead ------------------------------------------------
    let mut overhead_alignment = Vec::new();
    let mut small_alignment = Vec::new();
    for alignment in transport::ALIGNMENTS {
        let container = transport::create(&main_sections, alignment, &limits)
            .map_err(|err| format!("create: {err}"))?;
        let frames: u64 = main_sections
            .iter()
            .map(|entry| entry.bytes.len() as u64)
            .sum();
        overhead_alignment.push(json!({
            "alignment": alignment,
            "frames_bytes": frames,
            "container_bytes": container.len(),
            "overhead_bytes": container.len() as u64 - frames,
            "overhead_percent": percent(container.len() as u64 - frames, frames),
        }));

        let entries: Vec<TransportFrame<'_>> = small
            .iter()
            .map(|frame| TransportFrame::new(frame.as_slice(), SectionRole::Metadata))
            .collect();
        let container = transport::create(&entries, alignment, &limits)
            .map_err(|err| format!("create: {err}"))?;
        let frames: u64 = entries.iter().map(|entry| entry.bytes.len() as u64).sum();
        small_alignment.push(json!({
            "alignment": alignment,
            "frames_bytes": frames,
            "container_bytes": container.len(),
            "overhead_bytes": container.len() as u64 - frames,
            "overhead_percent": percent(container.len() as u64 - frames, frames),
        }));
    }

    // ---- 4. a 4 KiB-aligned slice inside the mode-17 section -------------
    let section_frame = transport::verify_section_bytes(&section, &section_bytes, &limits)
        .map_err(|err| format!("section: {err}"))?;
    let descriptor = modes::parse_float_tiled(&section_frame, &limits)
        .map_err(|err| format!("float-tiled descriptor: {err}"))?;
    let payload_base = descriptor.payload.as_ptr() as usize - section_bytes.as_ptr() as usize;
    let mut chosen_index = 0usize;
    for (index, tile) in descriptor.meta.tiles.iter().enumerate() {
        let rel = payload_base + tile.offset as usize;
        let slice_rel = rel & !4095usize;
        if slice_rel >= 4096 && slice_rel + 4096 <= section_bytes.len() {
            chosen_index = index;
            break;
        }
    }
    let chosen = &descriptor.meta.tiles[chosen_index];
    let tile_rel = payload_base + chosen.offset as usize;
    let slice_rel = tile_rel & !4095usize;
    if slice_rel + 4096 > section_bytes.len() {
        return Err("no 4 KiB slice fits inside the mode-17 section".into());
    }
    let slice_offset = section.offset + slice_rel as u64;
    let slice = backend
        .get_range(&key, slice_offset, 4096)
        .map_err(|err| format!("slice fetch: {err}"))?;
    check(
        "slice_aligned",
        slice_offset % 4096 == 0 && slice.len() == 4096,
    );
    let slice_sha = sha256_hex_bytes(&slice);
    let slice_not_section_root =
        slice_sha != section.root.to_hex() && hash_domain(DOMAIN_CHUNK, &slice) != section.root;
    check("slice_not_section_root", slice_not_section_root);

    let tile_bytes = backend
        .get_range(
            &key,
            section.offset + tile_rel as u64,
            chosen.compressed_len,
        )
        .map_err(|err| format!("tile fetch: {err}"))?;
    let tile = modes::decode_float_tile(descriptor.meta.dtype, &chosen, &tile_bytes, &limits)
        .map_err(|err| format!("tile decode: {err}"))?;
    check("tile_root_verified", !tile.is_empty());

    let full_section = backend
        .get_range(&key, section.offset, section.length)
        .map_err(|err| format!("full section fetch: {err}"))?;
    let verify_started = Instant::now();
    let full_frame = transport::verify_section_bytes(&section, &full_section, &limits)
        .map_err(|err| format!("full section: {err}"))?;
    let decoded17 = modes::decode_float_tiled(&full_frame, &limits)
        .map_err(|err| format!("float-tiled decode: {err}"))?;
    let full_section_verify_ms = verify_started.elapsed().as_secs_f64() * 1000.0;
    check(
        "full_section_root_verified",
        hash_domain(DOMAIN_CHUNK, &decoded17) == section.root,
    );
    check("mode17_exact", decoded17 == tensor_bytes);

    // The mode-20 section is the sibling independently-group-rooted frame.
    let mode20_section = parsed.sections[1];
    let mode20_bytes = backend
        .get_range(&key, mode20_section.offset, mode20_section.length)
        .map_err(|err| format!("mode20 fetch: {err}"))?;
    let mode20_frame = transport::verify_section_bytes(&mode20_section, &mode20_bytes, &limits)
        .map_err(|err| format!("mode20 section: {err}"))?;
    let resolver = weftcodec::dependency::MapResolver::new();
    let decoded20 = engram::decode_engram(&mode20_frame, &resolver, &limits)
        .map_err(|err| format!("engram decode: {err}"))?;
    check(
        "mode20_section_root_verified",
        hash_domain(DOMAIN_CHUNK, &decoded20) == mode20_section.root,
    );
    check("mode20_exact", decoded20 == tensor_bytes);

    // Which tile covers the aligned slice? State the root that applies.
    let slice_end = slice_rel as u64 + 4096;
    let covering: Vec<u64> = descriptor
        .meta
        .tiles
        .iter()
        .filter(|tile| {
            let start = payload_base as u64 + tile.offset;
            let end = start + tile.compressed_len;
            start < slice_end && end > slice_rel as u64
        })
        .map(|tile| tile.index)
        .collect();

    // ---- 5. alignment invariant, including an off-by-one rejection -------
    let mut misaligned = main.clone();
    let first_offset = section.offset + 1;
    misaligned[transport::HEADER_LEN..transport::HEADER_LEN + 8]
        .copy_from_slice(&first_offset.to_le_bytes());
    let off_by_one_class = transport::parse_metadata(&misaligned, &limits)
        .err()
        .map(|err| err.class().to_string());
    check(
        "off_by_one_offset_rejected",
        off_by_one_class.as_deref() == Some("Malformed"),
    );

    // ---- receipt ---------------------------------------------------------
    let all_ok = checks.iter().all(|(_, ok)| *ok);
    let receipt = json!({
        "schema": "weft.transport.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".into()),
        "label": "single-host layout court; warm cache; not a transport, DMA, GPU, RDMA, or deployed-3FS claim",
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".into()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".into()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".into()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".into()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".into()),
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".into()),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".into()),
            "weftcodec_go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".into()),
        },
        "corpus": {
            "path": corpus.display().to_string(),
            "sha256": observed_sha,
            "tensor": TENSOR_NAME,
            "dtype": tensor.dtype,
            "shape": shape,
            "bytes": tensor_bytes.len(),
        },
        "frames": {
            "mode17_bytes": mode17.len(),
            "mode20_bytes": mode20.len(),
            "small_section_bytes": small.iter().map(|f| f.len()).sum::<usize>(),
            "main_sections": parsed.sections.len(),
            "main_container_bytes": main.len(),
            "main_container_sha256": sha256_hex_bytes(&main),
        },
        "parse": parse_rows,
        "single_section_fetch": {
            "section": 0,
            "offset": section.offset,
            "length": section.length,
            "section_read_ms_p50": median_ms(section_only),
            "header_table_section_read_ms_p50": median_ms(full_path),
            "bytes_single_section_path": single_path_bytes,
            "bytes_section_only": section.length,
            "bytes_whole_container": main.len(),
            "section_read_ratio_percent": percent(section.length, main.len() as u64),
            "single_path_ratio_percent": percent(single_path_bytes, main.len() as u64),
            "whole_container_read_bytes": whole.len(),
            "section_sha256": sha256_hex_bytes(&section_bytes),
        },
        "overhead": {
            "real_frames": overhead_alignment,
            "small_frames": small_alignment,
        },
        "subsection_slice": {
            "section": 0,
            "slice_offset": slice_offset,
            "slice_length": 4096,
            "slice_aligned_to_4096": slice_offset % 4096 == 0,
            "slice_sha256": slice_sha,
            "slice_not_equal_to_section_root": slice_not_section_root,
            "slice_is_not_independently_rooted": true,
            "tile_index": chosen.index,
            "tile_offset_in_section": tile_rel,
            "tile_length": chosen.compressed_len,
            "tile_root": chosen.tile_root.to_hex(),
            "tile_root_verified": true,
            "covering_tiles": covering,
            "full_section_root": section.root.to_hex(),
            "full_section_verify_ms": full_section_verify_ms,
            "mode20_section_root": mode20_section.root.to_hex(),
            "note": "the section root covers the whole enclosed frame; a sub-section slice is verifiable only through the mode-17 per-tile roots (or mode-20 per-group roots). v1 does not nest one frame's bytes inside another frame's stored payload, so the mode-17 and mode-20 frames are sibling sections in one container.",
        },
        "alignment_invariant": {
            "all_offsets_multiples_of_declared_alignment": true,
            "off_by_one_offset_class": off_by_one_class,
        },
        "checks": checks.iter().map(|(name, ok)| (*name, *ok)).collect::<std::collections::BTreeMap<_, _>>(),
        "checks_passed": checks.iter().filter(|(_, ok)| *ok).count(),
        "checks_total": checks.len(),
        "all_checks": all_ok,
        "losses": [
            "a container never reduces bytes: it adds a 4096-byte header, 56 bytes per section, and alignment padding",
            "at 65536 alignment the small-frame case pays up to 64 KiB per section; the receipt publishes the exact percentage",
            "a single-section reader cannot verify the container-level payload_hash; that is a whole-container property",
            "a sub-section slice cannot be verified against the section root; only the enclosed frame's per-tile or per-group roots apply",
            "alignment is necessary but not sufficient for zero-copy: this court materialises owned buffers and does not demonstrate DMA, GPU memory, RDMA, or a deployed 3FS cluster",
        ],
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|err| format!("cannot write {}: {err}", out.display()))?;

    println!(
        "transport court: {}/{} checks, container {} B, section {} B, losses published",
        checks.iter().filter(|(_, ok)| *ok).count(),
        checks.len(),
        main.len(),
        section.length
    );
    if !all_ok {
        let failed: Vec<&str> = checks
            .iter()
            .filter(|(_, ok)| !*ok)
            .map(|(name, _)| *name)
            .collect();
        return Err(format!("failed checks: {failed:?}"));
    }
    Ok(())
}

fn sha256_hex_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
