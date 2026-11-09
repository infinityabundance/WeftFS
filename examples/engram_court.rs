//! Mode-20 `ENGRAM_SHARD` court: whole-table, group, sequential/random group
//! access, incremental update, and honest baselines (raw pread, one zstd
//! frame, mode-15 tiles).
//!
//! Usage:
//!   cargo run --release --example engram_court -- --out RECEIPT.json
//!       [--table-mib 256] [--groups 64] [--dim 1024]
//!
//! The fixture is deterministic:
//!   * rows = table_mib * 1024 * 1024 / (dim * 2), 64 equal row groups;
//!   * a row is "active" when row % 64 == 0 (1.5625 % of rows), and only an
//!     active row is filled with xorshift64* bytes;
//!   * every inactive row repeats one fixed template row.
//! This is a synthetic n-gram/embedding sparsity model, not a serving trace.

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftcodec::dependency::MapResolver;
use weftcodec::engram::{self, EngramInput, EngramReference, EngramStorage, ENGRAM_HASH_BLAKE3};
use weftcodec::hash::{hash_domain, DOMAIN_CHUNK};
use weftcodec::tensor::Region;
use weftcodec::{Frame, Limits, Mode, TensorDtype};

struct Fixture {
    rows: u64,
    dim: u64,
    groups: u64,
    group_rows: u64,
    table: Vec<u8>,
    edges: Vec<u64>,
    active_rows: u64,
}

fn xorshift(state: &mut u64) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state & 0xff) as u8
}

fn build_fixture(table_mib: u64, groups: u64, dim: u64) -> Fixture {
    let row_bytes = dim * 2;
    let rows = table_mib * 1024 * 1024 / row_bytes;
    let group_rows = rows / groups;
    assert_eq!(group_rows * groups, rows, "rows must divide into groups");
    let mut state = 0x1234_5678_9abc_def0u64;
    let template: Vec<u8> = (0..row_bytes)
        .map(|_| xorshift(&mut state) & 0x07)
        .collect();
    let mut table = Vec::with_capacity((rows * row_bytes) as usize);
    let mut active_rows = 0u64;
    for row in 0..rows {
        if row % 64 == 0 {
            active_rows += 1;
            for _ in 0..row_bytes {
                table.push(xorshift(&mut state));
            }
        } else {
            table.extend_from_slice(&template);
        }
    }
    let edges: Vec<u64> = (0..=groups).map(|index| index * group_rows).collect();
    Fixture {
        rows,
        dim,
        groups,
        group_rows,
        table,
        edges,
        active_rows,
    }
}

fn group_range(fixture: &Fixture, group: u64) -> (usize, usize) {
    let row_bytes = fixture.dim as usize * 2;
    let start = (fixture.edges[group as usize] as usize) * row_bytes;
    let end = (fixture.edges[group as usize + 1] as usize) * row_bytes;
    (start, end)
}

fn blake3_hex(bytes: &[u8]) -> String {
    hash_domain(DOMAIN_CHUNK, bytes).to_hex()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn group_root_ok(fixture: &Fixture, meta: &engram::EngramMeta, limits: &Limits) -> bool {
    for group in &meta.groups {
        let (start, end) = group_range(fixture, group.index);
        let root = engram::engram_group_root(
            TensorDtype::Bf16,
            meta.precision.storage_dtype.as_str(),
            group.index,
            group.row_start,
            group.row_end,
            &fixture.table[start..end],
            limits,
        )
        .expect("group root");
        if root != group.group_root {
            return false;
        }
    }
    true
}

fn stored_bytes_for(meta: &engram::EngramMeta, indices: &[u32]) -> u64 {
    indices
        .iter()
        .map(|index| match meta.groups[*index as usize].storage {
            EngramStorage::Inline { compressed_len, .. } => compressed_len,
            EngramStorage::Reference { .. } => 0,
        })
        .sum()
}

fn push_check(checks: &mut Vec<Json>, name: &str, pass: bool, detail: String) {
    checks.push(json!({"name": name, "pass": pass, "detail": detail}));
}

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut table_mib = 256u64;
    let mut groups = 64u64;
    let mut dim = 1024u64;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--table-mib" => {
                table_mib = args[index + 1].parse().expect("--table-mib");
                index += 2;
            }
            "--groups" => {
                groups = args[index + 1].parse().expect("--groups");
                index += 2;
            }
            "--dim" => {
                dim = args[index + 1].parse().expect("--dim");
                index += 2;
            }
            other => panic!("unknown argument `{other}`"),
        }
    }
    let out = out.expect("--out RECEIPT.json is required");
    let started = Instant::now();
    let limits = Limits::default();
    let fixture = build_fixture(table_mib, groups, dim);
    let table = &fixture.table;
    let table_hash = blake3_hex(table);
    let table_sha = sha256_hex(table);
    let mut checks: Vec<Json> = Vec::new();

    // Mode 20 encode and full decode.
    let input = EngramInput::inline(
        TensorDtype::Bf16,
        vec![fixture.rows, fixture.dim],
        fixture.edges.clone(),
        ENGRAM_HASH_BLAKE3,
        fixture.rows,
    );
    let encode_start = Instant::now();
    let frame_bytes = engram::encode_engram(table, &input, &limits).expect("engram encode");
    let engram_encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
    let parsed = Frame::parse(&frame_bytes, &limits).expect("parse engram frame");
    assert_eq!(parsed.header.mode, Mode::EngramShard);
    let descriptor = engram::parse_engram(&parsed, &limits).expect("parse engram descriptor");
    let meta = &descriptor.meta;
    let per_group_roots_ok = group_root_ok(&fixture, meta, &limits);
    let engram_payload_bytes = meta.payload_len;
    let engram_frame_bytes = frame_bytes.len() as u64;
    let full_start = Instant::now();
    let decoded =
        engram::decode_engram_without_dependencies(&parsed, &limits).expect("engram decode");
    let engram_full_decode_ms = full_start.elapsed().as_secs_f64() * 1000.0;
    let engram_full_ok = decoded == *table;
    push_check(
        &mut checks,
        "engram_full_decode_exact",
        engram_full_ok,
        format!("{} bytes, blake3 {}", decoded.len(), blake3_hex(&decoded)),
    );
    push_check(
        &mut checks,
        "engram_per_group_roots",
        per_group_roots_ok,
        format!("{} group roots recomputed", meta.groups.len()),
    );

    // Single group read.
    let single_group = (fixture.groups / 4) as u32;
    engram::reset_engram_group_decode_count();
    let single_start = Instant::now();
    let (single_bytes, single_touched) = engram::read_engram_groups(
        &parsed,
        &descriptor,
        &[single_group],
        &MapResolver::new(),
        &limits,
    )
    .expect("single group read");
    let single_read_ms = single_start.elapsed().as_secs_f64() * 1000.0;
    let single_decoded = engram::engram_group_decode_count();
    let (single_lo, single_hi) = group_range(&fixture, u64::from(single_group));
    let single_ok = single_bytes == table[single_lo..single_hi];
    push_check(
        &mut checks,
        "single_group_exact",
        single_ok,
        format!(
            "group {single_group}, {} stored bytes",
            stored_bytes_for(meta, &single_touched)
        ),
    );
    push_check(
        &mut checks,
        "single_group_decode_counter",
        single_decoded == 1 && single_touched == vec![single_group],
        format!("decoded {single_decoded}, touched {:?}", single_touched),
    );

    // Four-group read.
    let mut four_groups = [
        1u32,
        (fixture.groups / 3) as u32,
        ((fixture.groups * 2) / 3) as u32,
        (fixture.groups - 1) as u32,
    ];
    four_groups.sort_unstable();
    let four_groups = four_groups;
    engram::reset_engram_group_decode_count();
    let four_start = Instant::now();
    let (four_bytes, four_touched) = engram::read_engram_groups(
        &parsed,
        &descriptor,
        &four_groups,
        &MapResolver::new(),
        &limits,
    )
    .expect("four group read");
    let four_read_ms = four_start.elapsed().as_secs_f64() * 1000.0;
    let four_decoded = engram::engram_group_decode_count();
    let mut four_expected = Vec::new();
    for group in &four_groups {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        four_expected.extend_from_slice(&table[lo..hi]);
    }
    push_check(
        &mut checks,
        "four_groups_exact",
        four_bytes == four_expected,
        format!("{} stored bytes", stored_bytes_for(meta, &four_touched)),
    );
    push_check(
        &mut checks,
        "four_groups_decode_counter",
        four_decoded == 4,
        format!("decoded {four_decoded}, touched {:?}", four_touched),
    );

    // Four contiguous groups, the fair comparison against one mode-15 region.
    let contiguous_four: Vec<u32> = (1..5).collect();
    engram::reset_engram_group_decode_count();
    let contiguous_start = Instant::now();
    let (contiguous_bytes, contiguous_touched) = engram::read_engram_groups(
        &parsed,
        &descriptor,
        &contiguous_four,
        &MapResolver::new(),
        &limits,
    )
    .expect("contiguous four group read");
    let contiguous_ms = contiguous_start.elapsed().as_secs_f64() * 1000.0;
    let contiguous_decoded = engram::engram_group_decode_count();
    let mut contiguous_expected = Vec::new();
    for group in &contiguous_four {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        contiguous_expected.extend_from_slice(&table[lo..hi]);
    }
    push_check(
        &mut checks,
        "contiguous_four_exact",
        contiguous_bytes == contiguous_expected && contiguous_decoded == 4,
        format!(
            "decoded {contiguous_decoded}, touched {:?}",
            contiguous_touched
        ),
    );

    // Sequential eight groups vs random eight groups.
    let sequential: Vec<u32> = (0..8).collect();
    engram::reset_engram_group_decode_count();
    let seq_start = Instant::now();
    let (seq_bytes, seq_touched) = engram::read_engram_groups(
        &parsed,
        &descriptor,
        &sequential,
        &MapResolver::new(),
        &limits,
    )
    .expect("sequential read");
    let sequential_ms = seq_start.elapsed().as_secs_f64() * 1000.0;
    let seq_decoded = engram::engram_group_decode_count();
    let mut seq_expected = Vec::new();
    for group in &sequential {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        seq_expected.extend_from_slice(&table[lo..hi]);
    }
    push_check(
        &mut checks,
        "sequential_groups_exact",
        seq_bytes == seq_expected && seq_decoded == 8,
        format!("decoded {seq_decoded}, touched {:?}", seq_touched),
    );

    let mut random: Vec<u32> = Vec::new();
    let mut state = 0xdead_beef_1234_5678u64;
    while random.len() < 8 {
        let candidate = (xorshift(&mut state) as u32) % (fixture.groups as u32);
        if !random.contains(&candidate) {
            random.push(candidate);
        }
    }
    random.sort_unstable();
    engram::reset_engram_group_decode_count();
    let rand_start = Instant::now();
    let (rand_bytes, rand_touched) =
        engram::read_engram_groups(&parsed, &descriptor, &random, &MapResolver::new(), &limits)
            .expect("random read");
    let random_ms = rand_start.elapsed().as_secs_f64() * 1000.0;
    let rand_decoded = engram::engram_group_decode_count();
    let mut rand_expected = Vec::new();
    for group in &random {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        rand_expected.extend_from_slice(&table[lo..hi]);
    }
    push_check(
        &mut checks,
        "random_groups_exact",
        rand_bytes == rand_expected && rand_decoded == 8,
        format!("decoded {rand_decoded}, groups {:?}", random),
    );

    // Incremental update: change 4 groups, reference the rest.
    let mut changed_groups = vec![
        1u32,
        (fixture.groups / 3) as u32,
        ((fixture.groups * 2) / 3) as u32,
        (fixture.groups - 1) as u32,
    ];
    changed_groups.sort_unstable();
    changed_groups.dedup();
    let changed_groups = changed_groups;
    let mut new_table = table.clone();
    let mut state = 0x0bad_f00d_0000_0001u64;
    for group in &changed_groups {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        for byte in &mut new_table[lo..hi] {
            *byte = xorshift(&mut state);
        }
    }
    let mut references = std::collections::BTreeMap::new();
    let mut supersedes = vec![None; meta.groups.len()];
    for (index, group) in meta.groups.iter().enumerate() {
        if changed_groups.contains(&(index as u32)) {
            supersedes[index] = Some(group.group_root);
        } else {
            references.insert(
                index as u64,
                EngramReference {
                    group_root: group.group_root,
                    dependency: 0,
                },
            );
        }
    }
    let mut incremental_input = EngramInput::inline(
        TensorDtype::Bf16,
        vec![fixture.rows, fixture.dim],
        fixture.edges.clone(),
        ENGRAM_HASH_BLAKE3,
        fixture.rows,
    );
    incremental_input.references = references;
    incremental_input.supersedes = supersedes;
    let inc_start = Instant::now();
    let incremental_frame = engram::encode_engram_with_dependencies(
        &new_table,
        &incremental_input,
        &[parsed.header.content_hash],
        &limits,
    )
    .expect("incremental encode");
    let incremental_ms = inc_start.elapsed().as_secs_f64() * 1000.0;
    let incremental_parsed = Frame::parse(&incremental_frame, &limits).expect("parse incremental");
    let incremental_meta =
        engram::engram_metadata(&incremental_parsed, &limits).expect("incremental metadata");
    let mut resolver = MapResolver::new();
    resolver.insert(parsed.header.content_hash, frame_bytes.clone());
    let inc_decode_start = Instant::now();
    let incremental_decoded =
        engram::decode_engram(&incremental_parsed, &resolver, &limits).expect("incremental decode");
    let incremental_decode_ms = inc_decode_start.elapsed().as_secs_f64() * 1000.0;
    let incremental_ok = incremental_decoded == new_table;
    let old_unchanged =
        engram::decode_engram_without_dependencies(&parsed, &limits).expect("old decode") == *table
            && engram::engram_metadata(&parsed, &limits)
                .expect("old metadata")
                .descriptor_root(&limits)
                .expect("old root")
                == meta.descriptor_root(&limits).expect("root");
    push_check(
        &mut checks,
        "incremental_update_exact",
        incremental_ok && old_unchanged,
        format!(
            "changed {:?}; new payload {} bytes; old version unchanged",
            changed_groups, incremental_meta.payload_len
        ),
    );
    let full_rewrite_start = Instant::now();
    let full_rewrite_frame =
        engram::encode_engram(&new_table, &input, &limits).expect("full rewrite encode");
    let full_rewrite_ms = full_rewrite_start.elapsed().as_secs_f64() * 1000.0;

    // Raw file + pread baseline.
    let tmp_path =
        std::env::temp_dir().join(format!("weft-engram-court-{}.bin", std::process::id()));
    let raw_write_start = Instant::now();
    {
        let mut file = File::create(&tmp_path).expect("raw file create");
        file.write_all(table).expect("raw file write");
        file.sync_all().expect("raw file sync");
    }
    let raw_write_ms = raw_write_start.elapsed().as_secs_f64() * 1000.0;
    let raw_file_bytes = std::fs::metadata(&tmp_path).expect("raw stat").len();
    let raw_file = File::open(&tmp_path).expect("raw open");
    let (single_lo, single_hi) = group_range(&fixture, u64::from(single_group));
    let mut buffer = vec![0u8; single_hi - single_lo];
    let raw_single_start = Instant::now();
    raw_file
        .read_exact_at(&mut buffer, single_lo as u64)
        .expect("raw pread single");
    let raw_single_ms = raw_single_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "raw_pread_single_exact",
        buffer == table[single_lo..single_hi],
        format!("{} bytes", buffer.len()),
    );
    let mut four_buffer = Vec::new();
    let raw_four_start = Instant::now();
    for group in &four_groups {
        let (lo, hi) = group_range(&fixture, u64::from(*group));
        let mut part = vec![0u8; hi - lo];
        raw_file
            .read_exact_at(&mut part, lo as u64)
            .expect("raw pread four");
        four_buffer.extend_from_slice(&part);
    }
    let raw_four_ms = raw_four_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "raw_pread_four_exact",
        four_buffer == four_expected,
        format!("{} bytes", four_buffer.len()),
    );
    let _ = std::fs::remove_file(&tmp_path);

    // One zstd frame baseline.
    let zstd_encode_start = Instant::now();
    let zstd_frame = weftcodec::modes::encode_zstd(table, 3, None, &limits).expect("zstd encode");
    let zstd_encode_ms = zstd_encode_start.elapsed().as_secs_f64() * 1000.0;
    let zstd_decode_start = Instant::now();
    let zstd_decoded = weftcodec::modes::decode_bytes(&zstd_frame, &MapResolver::new(), &limits)
        .expect("zstd decode");
    let zstd_decode_ms = zstd_decode_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "zstd_full_decode_exact",
        zstd_decoded == *table,
        format!("frame {} bytes", zstd_frame.len()),
    );

    // Mode-15 tiled baseline: one tile per row group.
    let tile_shape = vec![fixture.group_rows, fixture.dim];
    let tiled_encode_start = Instant::now();
    let tiled_frame = weftcodec::encode_tiled_tensor(
        table,
        TensorDtype::Bf16,
        &[fixture.rows, fixture.dim],
        &tile_shape,
        &limits,
    )
    .expect("tiled encode");
    let tiled_encode_ms = tiled_encode_start.elapsed().as_secs_f64() * 1000.0;
    let tiled_parsed = Frame::parse(&tiled_frame, &limits).expect("parse tiled");
    let tiled_descriptor =
        weftcodec::modes::parse_tiled_tensor(&tiled_parsed, &limits).expect("tiled descriptor");
    let tiled_decode_start = Instant::now();
    let tiled_decoded =
        weftcodec::decode_tiled_tensor(&tiled_parsed, &limits).expect("tiled decode");
    let tiled_full_decode_ms = tiled_decode_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "tiled_full_decode_exact",
        tiled_decoded == *table,
        format!("frame {} bytes", tiled_frame.len()),
    );
    let single_region = Region::new(
        vec![u64::from(single_group) * fixture.group_rows, 0],
        vec![
            (u64::from(single_group) + 1) * fixture.group_rows,
            fixture.dim,
        ],
    );
    let tiled_single_start = Instant::now();
    let (tiled_single_bytes, tiled_tiles) =
        weftcodec::read_tiled_region(&tiled_descriptor, &single_region, &limits)
            .expect("tiled region");
    let tiled_single_ms = tiled_single_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "tiled_single_region_exact",
        tiled_single_bytes == table[single_lo..single_hi] && tiled_tiles == 1,
        format!("tiles touched {tiled_tiles}"),
    );
    let contiguous_region = Region::new(
        vec![fixture.group_rows, 0],
        vec![5 * fixture.group_rows, fixture.dim],
    );
    let tiled_contiguous_start = Instant::now();
    let (tiled_contiguous_bytes, tiled_contiguous_tiles) =
        weftcodec::read_tiled_region(&tiled_descriptor, &contiguous_region, &limits)
            .expect("tiled contiguous region");
    let tiled_contiguous_ms = tiled_contiguous_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "tiled_contiguous_four_exact",
        tiled_contiguous_bytes
            == table[(fixture.group_rows * fixture.dim * 2) as usize
                ..(5 * fixture.group_rows * fixture.dim * 2) as usize]
            && tiled_contiguous_tiles == 4,
        format!("tiles touched {tiled_contiguous_tiles}"),
    );
    let four_region = Region::new(
        vec![
            u64::from(*four_groups.first().unwrap()) * fixture.group_rows,
            0,
        ],
        vec![
            u64::from(*four_groups.last().unwrap()) * fixture.group_rows,
            fixture.dim,
        ],
    );
    let tiled_four_start = Instant::now();
    let (tiled_four_bytes, tiled_four_tiles) =
        weftcodec::read_tiled_region(&tiled_descriptor, &four_region, &limits)
            .expect("tiled four region");
    let tiled_four_ms = tiled_four_start.elapsed().as_secs_f64() * 1000.0;
    push_check(
        &mut checks,
        "tiled_span_region_exact",
        tiled_four_bytes
            == table[(u64::from(*four_groups.first().unwrap())
                * fixture.group_rows
                * fixture.dim
                * 2) as usize
                ..(u64::from(*four_groups.last().unwrap()) * fixture.group_rows * fixture.dim * 2)
                    as usize],
        format!("tiles touched {tiled_four_tiles}"),
    );

    let all_pass = checks
        .iter()
        .all(|check| check["pass"].as_bool() == Some(true));
    let receipt = json!({
        "schema": "weft.engram.v1",
        "mode": 20,
        "mode_name": "engram_shard",
        "generated_at_unix_ns": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0),
        "wall_ms": started.elapsed().as_secs_f64() * 1000.0,
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "in_court": std::env::var("WEFT_IN_COURT").ok(),
        },
        "revisions": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").ok(),
            "weftfs_go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
            "weftcodec_go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
        },
        "fixture": {
            "generator": "xorshift64* active rows (row % 64 == 0), other rows repeat one template row",
            "rows": fixture.rows,
            "dim": fixture.dim,
            "element_dtype": "bf16",
            "groups": fixture.groups,
            "group_rows": fixture.group_rows,
            "logical_bytes": table.len(),
            "active_rows": fixture.active_rows,
            "active_row_fraction": fixture.active_rows as f64 / fixture.rows as f64,
            "table_blake3": table_hash,
            "table_sha256": table_sha,
        },
        "engram": {
            "encode_ms": engram_encode_ms,
            "frame_bytes": engram_frame_bytes,
            "payload_bytes": engram_payload_bytes,
            "descriptor_root": meta.descriptor_root(&limits).expect("root").to_hex(),
            "full_decode_ms": engram_full_decode_ms,
            "full_decode_bytes": decoded.len(),
            "single_group": {
                "group": single_group,
                "bytes": single_bytes.len(),
                "stored_payload_bytes_read": stored_bytes_for(meta, &single_touched),
                "read_ms": single_read_ms,
                "groups_decoded": single_decoded,
                "sha256": sha256_hex(&single_bytes),
            },
            "four_groups": {
                "groups": four_groups,
                "bytes": four_bytes.len(),
                "stored_payload_bytes_read": stored_bytes_for(meta, &four_touched),
                "read_ms": four_read_ms,
                "groups_decoded": four_decoded,
            },
            "contiguous_four": {
                "groups": contiguous_four,
                "bytes": contiguous_bytes.len(),
                "stored_payload_bytes_read": stored_bytes_for(meta, &contiguous_touched),
                "read_ms": contiguous_ms,
                "groups_decoded": contiguous_decoded,
            },
            "sequential_eight": {
                "groups": sequential,
                "bytes": seq_bytes.len(),
                "stored_payload_bytes_read": stored_bytes_for(meta, &seq_touched),
                "read_ms": sequential_ms,
                "groups_decoded": seq_decoded,
            },
            "random_eight": {
                "groups": random,
                "bytes": rand_bytes.len(),
                "stored_payload_bytes_read": stored_bytes_for(meta, &rand_touched),
                "read_ms": random_ms,
                "groups_decoded": rand_decoded,
            },
            "region_read_verification": "per-group roots are verified; the whole-frame trailer, payload_hash and whole-table content_hash are NOT checked on a partial read",
        },
        "incremental": {
            "changed_groups": changed_groups,
            "frame_bytes": incremental_frame.len() as u64,
            "payload_bytes": incremental_meta.payload_len,
            "encode_ms": incremental_ms,
            "decode_ms": incremental_decode_ms,
            "full_rewrite_frame_bytes": full_rewrite_frame.len() as u64,
            "full_rewrite_encode_ms": full_rewrite_ms,
            "old_version_unchanged": old_unchanged,
        },
        "baselines": {
            "raw_pread": {
                "encode_ms": raw_write_ms,
                "file_bytes": raw_file_bytes,
                "single_group_bytes": buffer.len(),
                "single_group_ms": raw_single_ms,
                "four_groups_bytes": four_buffer.len(),
                "four_groups_ms": raw_four_ms,
            },
            "zstd": {
                "encode_ms": zstd_encode_ms,
                "frame_bytes": zstd_frame.len() as u64,
                "full_decode_ms": zstd_decode_ms,
                "single_group_bytes": single_hi - single_lo,
                "single_group_note": "must decode the whole frame; the reported full decode covers it",
            },
            "mode15_tiled": {
                "encode_ms": tiled_encode_ms,
                "frame_bytes": tiled_frame.len() as u64,
                "tile_shape": tile_shape,
                "full_decode_ms": tiled_full_decode_ms,
                "single_group_bytes": tiled_single_bytes.len(),
                "single_group_ms": tiled_single_ms,
                "single_group_tiles_touched": tiled_tiles,
                "contiguous_bytes": tiled_contiguous_bytes.len(),
                "contiguous_ms": tiled_contiguous_ms,
                "contiguous_tiles_touched": tiled_contiguous_tiles,
                "span_bytes": tiled_four_bytes.len(),
                "span_ms": tiled_four_ms,
                "span_tiles_touched": tiled_four_tiles,
            },
        },
        "checks": checks,
        "all_checks_pass": all_pass,
        "conclusion": "Mode 20 is exact on every path. It wins single-group and small-group reads because only touched groups are read and decoded; it pays per-group Huffman table overhead, so a whole-table decode may be larger/slower than one zstd frame. See bench/RESULTS-engram.md for every loss.",
    });
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!(
        "engram court: {} checks, all_pass={all_pass}, receipt {}",
        receipt["checks"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or(0),
        out.display()
    );
    if !all_pass {
        std::process::exit(1);
    }
}
