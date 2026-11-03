//! Extended codec benchmark: external CLI baselines plus WeftCodec modes with
//! whole-object accounting.
//!
//! Every external decode is compared byte-for-byte with the input before its
//! sample is accepted. WeftCodec decodes are verified the same way. The receipt
//! contains raw nanosecond samples and no `winner` field: analysis belongs in
//! `bench/RESULTS-v1.md`, with losses stated there.
//!
//! Usage: bench_codec_ext [--iterations N] [--out PATH]
//! Run through scripts/docker-court.sh bench (or rust for WeftCodec-only work).

use std::env;
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::{json, Value};
use weftcodec::dependency::MapResolver;
use weftcodec::hash::{hash_domain, DOMAIN_CHUNK};
use weftcodec::modes::{
    decode_bytes, encode_delta, encode_dictionary, encode_raw, encode_reference, encode_zstd,
};
use weftcodec::Limits;

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let word = xorshift(&mut state).to_le_bytes();
        let take = (len - out.len()).min(8);
        out.extend_from_slice(&word[..take]);
    }
    out
}

fn repetitive(len: usize) -> Vec<u8> {
    let pattern = b"weft codec benchmark: context is not a free lunch\n";
    pattern.iter().copied().cycle().take(len).collect()
}

fn near_duplicate(base: &[u8], edits: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..edits {
        let pos = (xorshift(&mut state) as usize) % out.len();
        out[pos] = out[pos].wrapping_add(1);
    }
    out
}

/// Deterministic, source-like text. The goal is a compressible corpus that
/// resembles a source tree more than a single repeated line.
fn source_text(len: usize) -> Vec<u8> {
    const LINES: &[&str] = &[
        "pub fn resolve(store: &Store, selector: &Selector) -> Result<Resolution> {",
        "    let authorized = authorize(store.state(), principal)?;",
        "    let mut matches = Vec::new();",
        "    for object in authorized.iter() {",
        "        if predicate.matches(object)? { matches.push(object.id); }",
        "    }",
        "    matches.sort_unstable();",
        "    Ok(Resolution { results: matches })",
        "}",
        "// Context is an address, not a cache key.",
        "pub struct Relation { subject: ObjectId, predicate: &'static str, target: Target }",
        "impl Relation {",
        "    pub fn new(subject: ObjectId, predicate: &str, target: Target) -> Result<Self> {",
        "        validate_predicate(predicate)?;",
        "        Ok(Self { subject, predicate, target })",
        "    }",
        "}",
        "fn commit(state: &State, ops: Vec<Op>) -> Result<CommitId> {",
        "    let next = state.apply(ops)?;",
        "    let encoded = next.encode()?;",
        "    let digest = hash_domain(DOMAIN_COMMIT, &encoded);",
        "    publish(&encoded, digest)?;",
        "    Ok(digest)",
        "}",
        "#[cfg(test)]",
        "mod tests {",
        "    use super::*;",
        "    #[test]",
        "    fn round_trip_preserves_bytes() { assert_eq!(decode(&encode(b\"x\")).unwrap(), b\"x\"); }",
        "}",
        "// A selector identifies the query; it never identifies the result set.",
        "let snapshot = store.snapshot_create(\"baseline\")?;",
        "let frame = encode_delta(&data, &base, &limits)?;",
    ];
    let mut state = 0xC0FF_EE00_1234_5678u64;
    let mut out = Vec::with_capacity(len);
    let mut index = 0usize;
    while out.len() < len {
        let pick = (xorshift(&mut state) as usize) % LINES.len();
        let line = format!(
            "{:>6}  {}  // item {:08x}\n",
            index,
            LINES[pick],
            xorshift(&mut state) as u32
        );
        out.extend_from_slice(line.as_bytes());
        index += 1;
    }
    out.truncate(len);
    out
}

fn hash_hex(bytes: &[u8]) -> String {
    hash_domain(DOMAIN_CHUNK, bytes).to_hex()
}

fn sha256_file(path: &Path) -> Option<String> {
    let output = Command::new("sha256sum").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(|value| value.to_owned())
}

fn tool_version(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    }
    if text.is_empty() {
        None
    } else {
        Some(text.lines().next().unwrap_or_default().to_owned())
    }
}

fn command_exists(program: &str) -> bool {
    tool_version(program, &["--version"]).is_some()
}

fn run_to_file(
    program: &str,
    args: &[String],
    stdin_path: &Path,
    out_path: &Path,
) -> Result<(), String> {
    let stdout =
        File::create(out_path).map_err(|err| format!("create {}: {err}", out_path.display()))?;
    let status = Command::new(program)
        .args(args)
        .arg(stdin_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .status()
        .map_err(|err| format!("spawn {program}: {err}"))?;
    if !status.success() {
        return Err(format!("{program} {:?} exited {status}", args));
    }
    Ok(())
}

struct ExternalCodec {
    id: &'static str,
    tool: &'static str,
    level: i32,
    encode_args: &'static [&'static str],
    decode_args: &'static [&'static str],
}

const EXTERNALS: &[ExternalCodec] = &[
    ExternalCodec {
        id: "zstd-3",
        tool: "zstd",
        level: 3,
        encode_args: &["-3", "-q", "-c"],
        decode_args: &["-d", "-q", "-c"],
    },
    ExternalCodec {
        id: "zstd-9",
        tool: "zstd",
        level: 9,
        encode_args: &["-9", "-q", "-c"],
        decode_args: &["-d", "-q", "-c"],
    },
    ExternalCodec {
        id: "zstd-19",
        tool: "zstd",
        level: 19,
        encode_args: &["-19", "-q", "-c"],
        decode_args: &["-d", "-q", "-c"],
    },
    ExternalCodec {
        id: "gzip-9",
        tool: "gzip",
        level: 9,
        encode_args: &["-9", "-c"],
        decode_args: &["-d", "-c"],
    },
    ExternalCodec {
        id: "xz-9",
        tool: "xz",
        level: 9,
        encode_args: &["-9", "-c"],
        decode_args: &["-d", "-c"],
    },
    ExternalCodec {
        id: "brotli-9",
        tool: "brotli",
        level: 9,
        encode_args: &["-q", "9", "-c"],
        decode_args: &["-d", "-c"],
    },
];

fn auto_encode(
    data: &[u8],
    base: Option<&[u8]>,
    dict: Option<&[u8]>,
    level: i32,
    limits: &Limits,
) -> weftcodec::Result<(&'static str, Vec<u8>, bool)> {
    let mut candidates: Vec<(&'static str, u8, Vec<u8>, bool)> = Vec::new();
    candidates.push(("raw", 0, encode_raw(data, None, limits)?, false));
    candidates.push(("zstd", 1, encode_zstd(data, level, None, limits)?, false));
    if let Some(base) = base {
        if base == data {
            candidates.push(("reference", 2, encode_reference(data, base, limits)?, true));
        }
        candidates.push(("delta", 3, encode_delta(data, base, limits)?, true));
    }
    if let Some(dict) = dict {
        candidates.push((
            "dictionary",
            4,
            encode_dictionary(data, dict, level, limits)?,
            true,
        ));
    }
    let best = candidates
        .into_iter()
        .min_by(|a, b| a.2.len().cmp(&b.2.len()).then(a.1.cmp(&b.1)))
        .expect("auto has at least raw and zstd");
    Ok((best.0, best.2, best.3))
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            return rest
                .split_once(':')
                .map(|(_, value)| value.trim().to_owned());
        }
    }
    None
}

fn mem_total_kib() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest.trim().trim_end_matches(" kB").parse().ok();
        }
    }
    None
}

fn vm_hwm_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().trim_end_matches(" kB").parse().ok();
        }
    }
    None
}

fn main() {
    let mut iterations = 7usize;
    let mut out_path: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => iterations = args.next().expect("value").parse().expect("integer"),
            "--out" => out_path = Some(args.next().expect("path")),
            other => panic!("unknown argument {other}"),
        }
    }
    if iterations == 0 {
        panic!("iterations must be positive");
    }
    let limits = Limits::default();

    let base = random_bytes(1 << 20, 0xA11CE);
    let corpora: Vec<(&str, Vec<u8>, &str, bool)> = vec![
        ("empty", Vec::new(), "empty input", false),
        ("one_byte", vec![0x42], "single byte", false),
        (
            "random_4m",
            random_bytes(4 << 20, 0xBEEF),
            "incompressible random",
            false,
        ),
        (
            "repetitive_4m",
            repetitive(4 << 20),
            "single repeated pattern",
            false,
        ),
        (
            "near_dup_base_1m",
            base.clone(),
            "base for the near-duplicate pair",
            true,
        ),
        (
            "near_dup_1pct",
            near_duplicate(&base, 10_000),
            "base with 1% single-byte edits",
            true,
        ),
        (
            "source_text_4m",
            source_text(4 << 20),
            "source-like text",
            false,
        ),
    ];

    let scratch = env::temp_dir().join(format!(
        "weft-codec-ext-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&scratch).expect("create scratch directory");

    let tools = json!({
        "zstd": tool_version("zstd", &["--version"]),
        "gzip": tool_version("gzip", &["--version"]),
        "xz": tool_version("xz", &["--version"]),
        "brotli": tool_version("brotli", &["--version"]),
        "sha256sum": tool_version("sha256sum", &["--version"]),
        "fio": tool_version("fio", &["--version"]),
        "sqlite3": tool_version("sqlite3", &["--version"]),
    });

    let mut corpus_manifest: Vec<Value> = Vec::new();
    let mut external_results: Vec<Value> = Vec::new();
    let mut weft_results: Vec<Value> = Vec::new();

    for (name, data, description, has_dependency) in &corpora {
        let input_path = scratch.join(format!("{name}.input"));
        fs::write(&input_path, data).expect("write corpus file");
        corpus_manifest.push(json!({
            "name": name,
            "description": description,
            "bytes": data.len(),
            "blake3": hash_hex(data),
            "sha256": sha256_file(&input_path),
            "path": input_path.display().to_string(),
        }));

        let dependency: Option<&[u8]> = if *has_dependency { Some(&base) } else { None };

        // ---- external CLIs ------------------------------------------------
        for codec in EXTERNALS {
            let available = command_exists(codec.tool);
            if !available {
                external_results.push(json!({
                    "corpus": name,
                    "input_bytes": data.len(),
                    "tool": codec.tool,
                    "tool_id": codec.id,
                    "level": codec.level,
                    "available": false,
                    "reason": format!("{} is not installed in this court", codec.tool),
                }));
                continue;
            }
            let encoded_path = scratch.join(format!("{name}.{}.enc", codec.id));
            let decoded_path = scratch.join(format!("{name}.{}.dec", codec.id));
            let encode_args: Vec<String> =
                codec.encode_args.iter().map(|s| s.to_string()).collect();
            let decode_args: Vec<String> =
                codec.decode_args.iter().map(|s| s.to_string()).collect();

            // Untimed verification pass.
            if let Err(err) = run_to_file(codec.tool, &encode_args, &input_path, &encoded_path) {
                external_results.push(json!({
                    "corpus": name, "input_bytes": data.len(), "tool": codec.tool,
                    "tool_id": codec.id, "level": codec.level, "available": true,
                    "error": err,
                }));
                continue;
            }
            if let Err(err) = run_to_file(codec.tool, &decode_args, &encoded_path, &decoded_path) {
                external_results.push(json!({
                    "corpus": name, "input_bytes": data.len(), "tool": codec.tool,
                    "tool_id": codec.id, "level": codec.level, "available": true,
                    "error": err,
                }));
                continue;
            }
            let decoded = fs::read(&decoded_path).expect("read verified decode");
            if decoded.as_slice() != data.as_slice() {
                external_results.push(json!({
                    "corpus": name, "input_bytes": data.len(), "tool": codec.tool,
                    "tool_id": codec.id, "level": codec.level, "available": true,
                    "error": "verification decode did not match the input",
                }));
                continue;
            }

            let mut encode_ns = Vec::with_capacity(iterations);
            let mut decode_ns = Vec::with_capacity(iterations);
            let mut compressed_bytes = Vec::with_capacity(iterations);
            let mut verified = true;
            for _ in 0..iterations {
                let start = Instant::now();
                if let Err(err) = run_to_file(codec.tool, &encode_args, &input_path, &encoded_path)
                {
                    panic!("{}/{}: encode failed: {err}", name, codec.id);
                }
                encode_ns.push(start.elapsed().as_nanos());
                let size = fs::metadata(&encoded_path).expect("encoded metadata").len();
                compressed_bytes.push(size);

                let start = Instant::now();
                if let Err(err) =
                    run_to_file(codec.tool, &decode_args, &encoded_path, &decoded_path)
                {
                    panic!("{}/{}: decode failed: {err}", name, codec.id);
                }
                decode_ns.push(start.elapsed().as_nanos());
                if fs::read(&decoded_path).expect("read decode").as_slice() != data.as_slice() {
                    verified = false;
                    break;
                }
            }
            if !verified {
                panic!("{}/{}: a timed decode failed verification", name, codec.id);
            }
            external_results.push(json!({
                "corpus": name,
                "input_bytes": data.len(),
                "tool": codec.tool,
                "tool_id": codec.id,
                "level": codec.level,
                "available": true,
                "compressed_bytes": compressed_bytes,
                "encode_ns": encode_ns,
                "decode_ns": decode_ns,
                "decode_verified": true,
                "command_encode": format!(
                    "{} {} <input> > <output>",
                    codec.tool,
                    codec.encode_args.join(" ")
                ),
                "command_decode": format!(
                    "{} {} <compressed> > <output>",
                    codec.tool,
                    codec.decode_args.join(" ")
                ),
            }));
        }

        // ---- WeftCodec modes ---------------------------------------------
        let resolver_for = |base: &[u8]| {
            let mut resolver = MapResolver::new();
            resolver.insert(hash_domain(DOMAIN_CHUNK, base), base.to_vec());
            resolver
        };
        let resolver = resolver_for(&base);

        struct Case {
            mode: &'static str,
            level: i32,
            chosen: &'static str,
            frame: Vec<u8>,
            uses_dependency: bool,
        }

        let mut cases: Vec<Case> = Vec::new();
        cases.push(Case {
            mode: "raw",
            level: 0,
            chosen: "raw",
            frame: encode_raw(data, None, &limits).expect("raw"),
            uses_dependency: false,
        });
        for level in [3i32, 9, 19] {
            cases.push(Case {
                mode: "zstd",
                level,
                chosen: "zstd",
                frame: encode_zstd(data, level, None, &limits).expect("zstd"),
                uses_dependency: false,
            });
        }
        if let Some(dep) = dependency {
            cases.push(Case {
                mode: "delta",
                level: 0,
                chosen: "delta",
                frame: encode_delta(data, dep, &limits).expect("delta"),
                uses_dependency: true,
            });
            cases.push(Case {
                mode: "dictionary",
                level: 3,
                chosen: "dictionary",
                frame: encode_dictionary(data, dep, 3, &limits).expect("dictionary"),
                uses_dependency: true,
            });
        } else {
            for mode in ["delta", "dictionary"] {
                weft_results.push(json!({
                    "corpus": name,
                    "input_bytes": data.len(),
                    "mode": mode,
                    "applicable": false,
                    "reason": "no base or dictionary is defined for this corpus",
                }));
            }
        }
        let (auto_mode, auto_frame, auto_dep) =
            auto_encode(data, dependency, dependency, 3, &limits).expect("auto");
        cases.push(Case {
            mode: "auto",
            level: 3,
            chosen: auto_mode,
            frame: auto_frame,
            uses_dependency: auto_dep,
        });

        for case in cases {
            // Untimed verification.
            let decoded = decode_bytes(&case.frame, &resolver, &limits).expect("decode");
            assert_eq!(
                decoded.as_slice(),
                data.as_slice(),
                "round-trip mismatch for {name}/{}",
                case.mode
            );

            let dependency_bytes = if case.uses_dependency {
                dependency.map(|dep| dep.len()).unwrap_or(0)
            } else {
                0
            };

            let mut encode_ns = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let start = Instant::now();
                let frame_len = match case.mode {
                    "raw" => encode_raw(data, None, &limits).map(|f| f.len()),
                    "zstd" => encode_zstd(data, case.level, None, &limits).map(|f| f.len()),
                    "delta" => encode_delta(data, dependency.expect("delta base"), &limits)
                        .map(|f| f.len()),
                    "dictionary" => encode_dictionary(
                        data,
                        dependency.expect("dictionary"),
                        case.level,
                        &limits,
                    )
                    .map(|f| f.len()),
                    "auto" => auto_encode(data, dependency, dependency, case.level, &limits)
                        .map(|(_, frame, _)| frame.len()),
                    other => panic!("unknown mode {other}"),
                }
                .expect("encode sample");
                encode_ns.push(start.elapsed().as_nanos());
                assert_eq!(
                    frame_len,
                    case.frame.len(),
                    "non-deterministic frame size for {name}/{}",
                    case.mode
                );
            }

            let mut decode_ns = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let start = Instant::now();
                let plain = decode_bytes(&case.frame, &resolver, &limits).expect("decode sample");
                decode_ns.push(start.elapsed().as_nanos());
                assert_eq!(
                    plain.as_slice(),
                    data.as_slice(),
                    "timed decode mismatch for {name}/{}",
                    case.mode
                );
            }

            weft_results.push(json!({
                "corpus": name,
                "input_bytes": data.len(),
                "mode": case.mode,
                "level": case.level,
                "chosen_mode": case.chosen,
                "applicable": true,
                "frame_bytes": case.frame.len(),
                "marginal_bytes": case.frame.len(),
                "dependency_bytes": dependency_bytes,
                "whole_object_bytes": case.frame.len() + dependency_bytes,
                "encode_ns": encode_ns,
                "decode_ns": decode_ns,
                "decode_verified": true,
            }));
        }
    }

    let report = json!({
        "schema": "weft-codec-bench-ext-v1",
        "kind": "codec-external-baselines",
        "iterations": iterations,
        "environment": {
            "kernel": env::var("WEFT_COURT_KERNEL").ok(),
            "court_image": env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": env::var("WEFT_COURT_IMAGE_ID").ok(),
            "court_memory": env::var("WEFT_COURT_MEMORY").ok(),
            "court_cpus": env::var("WEFT_COURT_CPUS").ok(),
            "court_pids": env::var("WEFT_COURT_PIDS").ok(),
            "docker_client": env::var("WEFT_DOCKER_CLIENT").ok(),
            "docker_server": env::var("WEFT_DOCKER_SERVER").ok(),
            "cpu_model": cpu_model(),
            "host_mem_total_kib": mem_total_kib(),
            "uname": Command::new("uname").arg("-a").output().ok()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned()),
            "nproc": std::thread::available_parallelism().ok().map(|n| n.get()),
            "revision_weftfs": env::var("WEFT_REPO_WEFTFS").ok(),
            "revision_weftcodec": env::var("WEFT_REPO_WEFTCODEC").ok(),
            "revision_weftcodec_go": env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
            "revision_weftfs_go": env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "tools": tools,
        "corpora": corpus_manifest,
        "external": external_results,
        "weftcodec": weft_results,
        "vm_hwm_kib": vm_hwm_kib(),
        "notes": [
            "External command times include process spawn and file I/O; they are not library-only measurements.",
            "WeftCodec frame bytes include the WFT1 framing overhead. `marginal_bytes` counts only the frame; `whole_object_bytes` adds the base or dictionary bytes required to decode DELTA/DICTIONARY/REFERENCE.",
            "`auto` mirrors the codec CLI candidate rule (RAW, ZSTD, DELTA/REFERENCE when a base is supplied, DICTIONARY when a dictionary is supplied) and picks the smallest frame with mode-byte tie-break.",
            "Every decode was compared byte-for-byte with the input before its sample was accepted.",
            "No `winner` field is emitted. See bench/RESULTS-v1.md for scoped analysis and losses."
        ],
        "scratch_dir": scratch.display().to_string(),
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize");
    match out_path {
        Some(path) => {
            fs::write(&path, text).expect("write report");
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
    let _ = fs::remove_dir_all(&scratch);
}
