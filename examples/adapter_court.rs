//! T048 adapter court: LoRA/DoRA artifacts, the family graph, and merged views.
//!
//! Everything here is measured on the pinned public corpus (real
//! `adapter_model.safetensors` files fetched from Hugging Face and hashed
//! against the LFS pointer). The harness never writes a number it did not time
//! itself, and every adapter restore is proved byte-exact by comparing the
//! reproduced container with the source file's SHA-256.
//!
//! Usage (inside the bench/fuse court):
//!
//! ```text
//! adapter_court --base BASE.safetensors --corpus ADAPTERS.json \
//!   --repo-root /work --out receipt.json --scratch DIR \
//!   [--adapters-limit N] [--merge-limit N] [--merged-full] \
//!   [--merged-zstd N] [--dump-merged DIR] [--dump-tensors N]
//! ```
//!
//! Arms measured per adapter: adapter-only storage and restore; base+adapter
//! family retention; the precomputed merged view and its restore; optionally a
//! full merged-checkpoint copy (the no-graph alternative). Comparators are the
//! same bytes as plain files and `zstd -19` on the same files, with all
//! metadata included in the store's retained bytes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::adapter::{
    AdapterKind, AdapterSource, AdapterWriteOptions, MergeOptions, MERGE_EQUIVALENCE_NOT_CLAIMED,
};
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::{self, SafetensorsFile};
use weftfs::store::{Options, Store};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|item| item == name)
}

fn number(args: &[String], name: &str, default: usize) -> usize {
    arg(args, name)
        .and_then(|text| text.parse().ok())
        .unwrap_or(default)
}

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

fn zstd19(zstd: &str, input: &Path, out_dir: &Path) -> Result<u64, String> {
    let stem = input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("input");
    let out = out_dir.join(format!("{stem}.zst"));
    let status = Command::new(zstd)
        .args(["-19", "-T1", "-q", "-f"])
        .arg(input)
        .arg("-o")
        .arg(&out)
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd -19 failed: {status}"));
    }
    let bytes = fs::metadata(&out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat {}: {err}", out.display()))?;
    let _ = fs::remove_file(&out);
    Ok(bytes)
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    safetensors::sha256_file(path)
        .map(|digest| digest.iter().map(|byte| format!("{byte:02x}")).collect())
        .map_err(|err| err.message().to_owned())
}

/// A writer that hashes and counts without buffering, so a full 988 MB restore
/// costs no RAM and still proves byte identity.
struct HashCounter {
    hasher: sha2::Sha256,
    bytes: u64,
}

impl HashCounter {
    fn new() -> Self {
        use sha2::Digest as _;
        HashCounter {
            hasher: sha2::Sha256::new(),
            bytes: 0,
        }
    }

    fn digest(self) -> ([u8; 32], u64) {
        use sha2::Digest as _;
        (self.hasher.finalize().into(), self.bytes)
    }
}

impl Write for HashCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        self.hasher.update(buffer);
        self.bytes += buffer.len() as u64;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1).map(str::to_owned))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

struct AdapterSpec {
    repo: String,
    revision: String,
    declared_base: Option<String>,
    rank: u64,
    alpha: f64,
    use_dora: bool,
    use_rslora: bool,
    bias: String,
    target_modules: Vec<String>,
    adapter_path: PathBuf,
    config_sha256: [u8; 32],
    file_sha256: [u8; 32],
    source_file_bytes: u64,
}

fn load_adapters(manifest: &Json, repo_root: &Path) -> Result<Vec<AdapterSpec>, String> {
    let adapters = manifest
        .get("adapters")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "corpus manifest has no adapters array".to_owned())?;
    let mut out = Vec::new();
    for entry in adapters {
        let repo = entry["repo"].as_str().unwrap_or_default().to_owned();
        let revision = entry["revision"].as_str().unwrap_or_default().to_owned();
        let local = entry["local_path"]
            .as_str()
            .ok_or_else(|| format!("{repo}: no local_path"))?;
        let adapter_path = repo_root.join(local);
        if !adapter_path.exists() {
            return Err(format!("{}: missing {}", repo, adapter_path.display()));
        }
        let sha = sha256_hex(&adapter_path)?;
        let expected = entry["files"][0]["sha256"].as_str().unwrap_or_default();
        if sha != expected {
            return Err(format!("{repo}: sha256 {sha} != manifest {expected}"));
        }
        let config_sha_hex = entry["files"][1]["sha256"].as_str().unwrap_or_default();
        let mut config_sha256 = [0u8; 32];
        for (index, byte) in config_sha256.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&config_sha_hex[index * 2..index * 2 + 2], 16)
                .map_err(|err| format!("{repo}: bad config sha: {err}"))?;
        }
        let mut file_sha256 = [0u8; 32];
        for (index, byte) in file_sha256.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&sha[index * 2..index * 2 + 2], 16)
                .map_err(|err| format!("{repo}: bad file sha: {err}"))?;
        }
        out.push(AdapterSpec {
            repo: repo.clone(),
            revision,
            declared_base: entry["declared_base_model"].as_str().map(str::to_owned),
            rank: entry["rank"].as_u64().unwrap_or(0),
            alpha: entry["lora_alpha"].as_f64().unwrap_or(0.0),
            use_dora: entry["use_dora"].as_bool().unwrap_or(false),
            use_rslora: entry["use_rslora"].as_bool().unwrap_or(false),
            bias: entry["bias"].as_str().unwrap_or("none").to_owned(),
            target_modules: entry["target_modules"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            source_file_bytes: fs::metadata(&adapter_path).map(|m| m.len()).unwrap_or(0),
            adapter_path,
            config_sha256,
            file_sha256,
        });
    }
    Ok(out)
}

fn slug(repo: &str) -> String {
    repo.replace('/', "__").replace(['.', '@'], "-")
}

fn strategy_counts(store: &Store, objects: &[weftfs::identity::Hash32]) -> Json {
    let mut counts = std::collections::BTreeMap::new();
    for object in objects {
        let name = store
            .tensor_metadata(*object)
            .map(|meta| meta.strategy)
            .unwrap_or_else(|_| "unknown".to_owned());
        *counts.entry(name).or_insert(0u64) += 1;
    }
    Json::Object(
        counts
            .into_iter()
            .map(|(key, value)| (key, Json::from(value)))
            .collect(),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fail = |message: &str| -> ! {
        eprintln!("adapter_court: {message}");
        std::process::exit(2);
    };
    let base_path =
        PathBuf::from(arg(&args, "--base").unwrap_or_else(|| fail("--base is required")));
    let corpus = arg(&args, "--corpus").unwrap_or_else(|| fail("--corpus is required"));
    let repo_root = PathBuf::from(arg(&args, "--repo-root").unwrap_or_else(|| "/work".to_owned()));
    let out_path = arg(&args, "--out").unwrap_or_else(|| fail("--out is required"));
    let zstd = arg(&args, "--zstd").unwrap_or_else(|| "/usr/bin/zstd".to_owned());
    let scratch = PathBuf::from(
        arg(&args, "--scratch")
            .unwrap_or_else(|| fail("--scratch is required (on the bind mount)")),
    );
    let adapters_limit = number(&args, "--adapters-limit", usize::MAX);
    let merge_limit = number(&args, "--merge-limit", usize::MAX);
    let merged_full = flag(&args, "--merged-full");
    let merged_zstd = number(&args, "--merged-zstd", 1);
    let dump_merged = arg(&args, "--dump-merged").map(PathBuf::from);
    let dump_tensors = number(&args, "--dump-tensors", 3);

    let corpus_json: Json = serde_json::from_slice(
        &fs::read(&corpus).unwrap_or_else(|err| fail(&format!("read corpus: {err}"))),
    )
    .unwrap_or_else(|err| fail(&format!("parse corpus: {err}")));
    let mut adapters = load_adapters(&corpus_json, &repo_root)
        .unwrap_or_else(|err| fail(&format!("corpus: {err}")));
    adapters.truncate(adapters_limit);

    fs::create_dir_all(&scratch).unwrap_or_else(|err| fail(&format!("scratch: {err}")));
    let store_dir = scratch.join("store");
    let mut store =
        Store::init(&store_dir, opts()).unwrap_or_else(|err| fail(&format!("store init: {err}")));

    let base_sha = sha256_hex(&base_path).unwrap_or_else(|err| fail(&err));
    let base_file_bytes = fs::metadata(&base_path).map(|m| m.len()).unwrap_or(0);

    // ---- the plain-file baseline: read the same bytes with no store ----------
    let started = Instant::now();
    let mut native = HashCounter::new();
    {
        let mut file =
            fs::File::open(&base_path).unwrap_or_else(|err| fail(&format!("base: {err}")));
        std::io::copy(&mut file, &mut native)
            .unwrap_or_else(|err| fail(&format!("base read: {err}")));
    }
    for spec in &adapters {
        let mut file = fs::File::open(&spec.adapter_path)
            .unwrap_or_else(|err| fail(&format!("{}: {err}", spec.repo)));
        std::io::copy(&mut file, &mut native).unwrap_or_else(|err| fail(&format!("read: {err}")));
    }
    let (native_digest, native_bytes) = native.digest();
    let native_read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected_native = base_file_bytes
        + adapters
            .iter()
            .map(|spec| spec.source_file_bytes)
            .sum::<u64>();
    if native_bytes != expected_native {
        fail(&format!(
            "native read {native_bytes} bytes, expected {expected_native} from the base and adapter files"
        ));
    }
    let _ = native_digest;

    // ---- base checkpoint -----------------------------------------------------
    let started = Instant::now();
    let base_report = store
        .write_checkpoint(
            "qwen-0.5b-instruct",
            &base_path,
            &CheckpointWriteOptions {
                strategy: ShardStrategy::Planned,
                deterministic_seed: Some([0x51u8; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap_or_else(|err| fail(&format!("base checkpoint: {err}")));
    let base_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let base_physical = store
        .retained_blob_bytes()
        .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
    let base_zstd = zstd19(&zstd, &base_path, &scratch).unwrap_or_else(|err| fail(&err));

    let started = Instant::now();
    let mut sink = HashCounter::new();
    let base_restore = store
        .restore_checkpoint(
            "qwen-0.5b-instruct",
            &CheckpointRestoreOptions::default(),
            &mut sink,
        )
        .unwrap_or_else(|err| fail(&format!("base restore: {err}")));
    let base_restore_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (base_restore_digest, base_restore_bytes) = sink.digest();
    let base_restore_exact = base_restore.exact_source_file
        && hex(&base_restore_digest) == hex(&base_restore.source_sha256.unwrap_or([0u8; 32]))
        && hex(&base_restore_digest) == base_sha;
    if !base_restore_exact {
        fail("base restore did not reproduce the source container");
    }

    let mut cells = Vec::new();
    let mut adapter_physical_total = 0u64;
    let mut adapter_logical_total = 0u64;
    let mut adapter_zstd_total = 0u64;
    let mut adapter_source_bytes_total = 0u64;
    let mut adapter_restore_ms_total = 0.0f64;
    let mut eligible_for_merge: Vec<String> = Vec::new();
    let mut repo_by_slug: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut all_target_objects: Vec<weftfs::identity::Hash32> = Vec::new();
    let mut all_target_bytes = 0u64;

    for spec in &adapters {
        let before = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let model = SafetensorsFile::parse(&spec.adapter_path)
            .unwrap_or_else(|err| fail(&format!("{}: parse: {err}", spec.repo)));
        let source = AdapterSource {
            repo: spec.repo.clone(),
            revision: spec.revision.clone(),
            file: "adapter_model.safetensors".to_owned(),
            url: format!(
                "https://huggingface.co/{}/resolve/{}/adapter_model.safetensors",
                spec.repo, spec.revision
            ),
            file_sha256: spec.file_sha256,
            config_sha256: spec.config_sha256,
        };
        let name = slug(&spec.repo);
        let started = Instant::now();
        let report = store
            .write_adapter(
                &name,
                "qwen-0.5b-instruct",
                &source,
                if spec.use_dora {
                    AdapterKind::Dora
                } else {
                    AdapterKind::Lora
                },
                spec.rank,
                spec.alpha,
                spec.use_rslora,
                &spec.bias,
                &spec.target_modules,
                &model,
                &AdapterWriteOptions {
                    record_provenance: true,
                    ..AdapterWriteOptions::default()
                },
            )
            .unwrap_or_else(|err| fail(&format!("{}: write_adapter: {err}", spec.repo)));
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let incremental = after - before;
        adapter_physical_total += incremental;
        adapter_logical_total += report.adapter_bytes;
        let zstd_bytes =
            zstd19(&zstd, &spec.adapter_path, &scratch).unwrap_or_else(|err| fail(&err));
        adapter_zstd_total += zstd_bytes;
        adapter_source_bytes_total += spec.source_file_bytes;

        let started = Instant::now();
        let verification = store
            .verify_adapter(&name)
            .unwrap_or_else(|err| fail(&format!("{}: verify: {err}", spec.repo)));
        let verify_ms = started.elapsed().as_secs_f64() * 1000.0;

        let started = Instant::now();
        let mut restored = Vec::new();
        let restore = store
            .restore_adapter(&name, &mut restored)
            .unwrap_or_else(|err| fail(&format!("{}: restore: {err}", spec.repo)));
        let restore_ms = started.elapsed().as_secs_f64() * 1000.0;
        adapter_restore_ms_total += restore_ms;
        let source_bytes = fs::read(&spec.adapter_path).unwrap();
        let exact = restore.byte_exact && restored == source_bytes;
        if !exact {
            fail(&format!(
                "{}: restored container is not the exact source (byte_exact={})",
                spec.repo, restore.byte_exact
            ));
        }

        let manifest = store
            .load_adapter(&name)
            .unwrap_or_else(|err| fail(&format!("{}: manifest: {err}", spec.repo)));
        let mut objects = Vec::new();
        let mut target_logical = 0u64;
        for target in &manifest.targets {
            objects.push(target.a.object);
            objects.push(target.b.object);
            if let Some(magnitude) = &target.magnitude {
                objects.push(magnitude.object);
            }
            target_logical += target.a.logical_bytes().unwrap_or(0)
                + target.b.logical_bytes().unwrap_or(0)
                + target
                    .magnitude
                    .as_ref()
                    .and_then(|m| m.logical_bytes().ok())
                    .unwrap_or(0);
        }
        all_target_objects.extend(objects.iter().copied());
        all_target_bytes += target_logical;

        let declared_base = spec.declared_base.clone().unwrap_or_default();
        // The declared base is a *model name*, so compare the trailing path
        // segment exactly. A substring test would accept e.g.
        // `unsloth/qwen2.5-0.5b-instruct-unsloth-bnb-4bit`, which is a 4-bit
        // quantized base and must not be merged into the bf16 checkpoint.
        let tail = declared_base
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let merge_eligible = tail == "qwen2.5-0.5b-instruct";
        let declared_base_is_exact_repo =
            declared_base.eq_ignore_ascii_case("Qwen/Qwen2.5-0.5B-Instruct");
        if merge_eligible {
            eligible_for_merge.push(name.clone());
        }
        repo_by_slug.insert(name.clone(), spec.repo.clone());
        cells.push(json!({
            "repo": spec.repo,
            "revision": spec.revision,
            "kind": if spec.use_dora { "dora" } else { "lora" },
            "declared_base_model": spec.declared_base,
            "declared_base_is_pinned": merge_eligible,
            "declared_base_is_exact_repo": declared_base_is_exact_repo,
            "rank": spec.rank,
            "alpha": spec.alpha,
            "use_rslora": spec.use_rslora,
            "target_modules": spec.target_modules,
            "targets": report.targets,
            "adapter_id": report.adapter_id.to_hex(),
            "manifest_bytes": report.manifest_bytes,
            "source_file_bytes": spec.source_file_bytes,
            "adapter_logical_bytes": report.adapter_bytes,
            "stored_physical_bytes": incremental,
            "physical_vs_source": if spec.source_file_bytes == 0 { 0.0 } else { incremental as f64 / spec.source_file_bytes as f64 },
            "zstd19_file_bytes": zstd_bytes,
            "physical_vs_zstd19": if zstd_bytes == 0 { 0.0 } else { incremental as f64 / zstd_bytes as f64 },
            "strategy_counts": strategy_counts(&store, &objects),
            "write_ms": write_ms,
            "verify_ms": verify_ms,
            "verify_targets": verification.targets,
            "restore_ms": restore_ms,
            "restore_container_bytes": restore.container_bytes,
            "restore_exact": exact,
            "restore_byte_exact": restore.byte_exact,
            "restore_sha256": hex(&restore.container_sha256),
        }));
    }

    // ---- merged views --------------------------------------------------------
    eligible_for_merge.truncate(merge_limit);
    let mut merged_cells = Vec::new();
    let mut merged_physical_total = 0u64;
    let mut merged_logical_total = 0u64;
    let mut merged_restore_ms_total = 0.0f64;
    let mut merged_zstd_total = 0u64;
    let mut merged_zstd_count = 0usize;
    for (index, name) in eligible_for_merge.iter().enumerate() {
        let before = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let started = Instant::now();
        let report = store
            .merge_adapter(
                name,
                &MergeOptions {
                    record_provenance: true,
                    verify_root: true,
                    ..MergeOptions::default()
                },
            )
            .unwrap_or_else(|err| fail(&format!("{name}: merge: {err}")));
        let merge_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let incremental = after - before;
        merged_physical_total += incremental;
        merged_logical_total += report.merged_logical_bytes;

        let view = store
            .load_merged_view(name)
            .unwrap_or_else(|err| fail(&format!("{name}: view: {err}")));
        let merged_objects: Vec<_> = view
            .tensors
            .iter()
            .filter_map(|entry| entry.object)
            .collect();

        // Apply the adapter to the restored base with no storage writes: the
        // arithmetic a loader pays when no merged view was precomputed.
        let adapter_manifest = store
            .load_adapter(name)
            .unwrap_or_else(|err| fail(&format!("{name}: adapter: {err}")));
        let loaded = store
            .load_checkpoint_manifest(&adapter_manifest.base.checkpoint)
            .unwrap_or_else(|err| fail(&format!("{name}: base manifest: {err}")));
        let started = Instant::now();
        for target in &adapter_manifest.targets {
            let base_bytes = store
                .restore_named_tensor_with_manifest(
                    None,
                    &adapter_manifest.base.checkpoint,
                    &loaded.manifest,
                    &target.base_tensor,
                    None,
                    None,
                )
                .unwrap_or_else(|err| fail(&format!("{name}: apply base: {err}")))
                .bytes;
            let a_bytes = store
                .read_object(target.a.object)
                .unwrap_or_else(|err| fail(&format!("{name}: apply a: {err}")));
            let b_bytes = store
                .read_object(target.b.object)
                .unwrap_or_else(|err| fail(&format!("{name}: apply b: {err}")));
            let magnitude = target
                .magnitude
                .as_ref()
                .map(|reference| {
                    store
                        .read_object(reference.object)
                        .map(|bytes| (reference.dtype.as_str(), reference.shape.as_slice(), bytes))
                })
                .transpose()
                .unwrap_or_else(|err| fail(&format!("{name}: apply magnitude: {err}")));
            let base_entry = loaded
                .manifest
                .tensors
                .iter()
                .find(|tensor| tensor.name == target.base_tensor)
                .unwrap_or_else(|| fail(&format!("{name}: apply lookup")));
            weftfs::adapter::merge_one_tensor(
                adapter_manifest.kind,
                adapter_manifest.scaling(),
                &base_entry.dtype,
                &base_entry.shape,
                &base_bytes,
                &target.a.dtype,
                &target.a.shape,
                &a_bytes,
                &target.b.dtype,
                &target.b.shape,
                &b_bytes,
                magnitude
                    .as_ref()
                    .map(|(dtype, shape, bytes)| (*dtype, *shape, bytes.as_slice())),
            )
            .unwrap_or_else(|err| fail(&format!("{name}: apply merge: {err}")));
        }
        let apply_ms = started.elapsed().as_secs_f64() * 1000.0;

        let container_path = scratch.join(format!("{name}.merged.safetensors"));
        let started = Instant::now();
        let mut file = fs::File::create(&container_path)
            .unwrap_or_else(|err| fail(&format!("{name}: create container: {err}")));
        let restore = store
            .restore_merged_view(name, &mut file)
            .unwrap_or_else(|err| fail(&format!("{name}: restore view: {err}")));
        file.flush().unwrap();
        drop(file);
        let restore_ms = started.elapsed().as_secs_f64() * 1000.0;
        merged_restore_ms_total += restore_ms;
        let container_bytes = fs::metadata(&container_path).map(|m| m.len()).unwrap_or(0);
        let container_sha = sha256_hex(&container_path).unwrap_or_default();
        if container_bytes != restore.container_bytes || container_sha != hex(&restore.file_sha256)
        {
            fail(&format!(
                "{name}: restored container disagrees with its report"
            ));
        }
        let zstd_bytes = if index < merged_zstd {
            merged_zstd_count += 1;
            let bytes = zstd19(&zstd, &container_path, &scratch).unwrap_or_else(|err| fail(&err));
            merged_zstd_total += bytes;
            Json::from(bytes)
        } else {
            Json::Null
        };
        // Dump a few merged tensors for the independent NumPy reference.
        let mut dumped = Vec::new();
        if let Some(dump_root) = &dump_merged {
            if index == 0 {
                let dir = dump_root.join(name);
                fs::create_dir_all(&dir).unwrap();
                for entry in view.tensors.iter().filter(|entry| entry.merged) {
                    if dumped.len() >= dump_tensors {
                        break;
                    }
                    let object = entry.object.unwrap();
                    let bytes = store
                        .read_object(object)
                        .unwrap_or_else(|err| fail(&format!("{name}: dump: {err}")));
                    let file_name = format!("{}.bin", entry.name.replace(['/', '.'], "_"));
                    fs::write(dir.join(&file_name), &bytes).unwrap();
                    dumped.push(json!({
                        "tensor": entry.name,
                        "file": file_name,
                        "dtype": entry.dtype,
                        "shape": entry.shape,
                        "content": entry.content.to_hex(),
                    }));
                }
            }
        }
        merged_cells.push(json!({
            "adapter": name,
            "repo": repo_by_slug.get(name),
            "merged_view": report.merged_view.to_hex(),
            "adapter_id": report.adapter.to_hex(),
            "merged_tensors": report.merged_tensors,
            "base_tensors": report.base_tensors,
            "merged_logical_bytes": report.merged_logical_bytes,
            "merged_stored_physical_bytes": incremental,
            "manifest_bytes": report.manifest_bytes,
            "strategy_counts": strategy_counts(&store, &merged_objects),
            "merge_ms": merge_ms,
            "apply_ms": apply_ms,
            "restore_ms": restore_ms,
            "restore_container_bytes": restore.container_bytes,
            "restore_sha256": hex(&restore.file_sha256),
            "zstd19_container_bytes": zstd_bytes,
            "dumped_tensors": dumped,
            "contract": "f32 accumulate, bf16 round-half-to-even, delta_then_add",
            "merge_equivalence": MERGE_EQUIVALENCE_NOT_CLAIMED,
        }));
        let _ = fs::remove_file(&container_path);
    }

    // ---- the honest no-graph alternative: a full merged checkpoint copy ------
    let full_cell = if merged_full {
        let name = eligible_for_merge
            .first()
            .unwrap_or_else(|| fail("no merged view to expand"));
        // The container was removed after each view; re-restore the first one.
        let container_path = scratch.join(format!("{name}.full.safetensors"));
        let mut file = fs::File::create(&container_path).unwrap();
        store.restore_merged_view(name, &mut file).unwrap();
        file.flush().unwrap();
        drop(file);
        let source_sha = sha256_hex(&container_path).unwrap_or_default();

        // (a) in the same store: content addressing dedups every tensor against
        // the base and the merged view, so only metadata is added.
        let before = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let started = Instant::now();
        let report = store
            .write_checkpoint(
                "merged-full",
                &container_path,
                &CheckpointWriteOptions {
                    strategy: ShardStrategy::Planned,
                    deterministic_seed: Some([0x77u8; 32]),
                    ..CheckpointWriteOptions::default()
                },
            )
            .unwrap_or_else(|err| fail(&format!("merged full checkpoint: {err}")));
        let shared_write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = store
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
        let shared_incremental = after - before;
        let started = Instant::now();
        let mut sink = HashCounter::new();
        let restore = store
            .restore_checkpoint(
                "merged-full",
                &CheckpointRestoreOptions::default(),
                &mut sink,
            )
            .unwrap_or_else(|err| fail(&format!("merged full restore: {err}")));
        let shared_restore_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (digest, bytes) = sink.digest();
        let shared_exact = restore.exact_source_file && hex(&digest) == source_sha;
        if !shared_exact {
            fail("merged-full checkpoint restore is not byte-exact");
        }

        // (b) in a fresh store: the standalone cost of a second model copy.
        let standalone_dir = scratch.join("store-merged-full");
        let _ = fs::remove_dir_all(&standalone_dir);
        let mut standalone = Store::init(&standalone_dir, opts())
            .unwrap_or_else(|err| fail(&format!("standalone store: {err}")));
        let started = Instant::now();
        standalone
            .write_checkpoint(
                "merged-full",
                &container_path,
                &CheckpointWriteOptions {
                    strategy: ShardStrategy::Planned,
                    deterministic_seed: Some([0x77u8; 32]),
                    ..CheckpointWriteOptions::default()
                },
            )
            .unwrap_or_else(|err| fail(&format!("standalone checkpoint: {err}")));
        let standalone_write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let standalone_physical = standalone
            .retained_blob_bytes()
            .unwrap_or_else(|err| fail(&format!("standalone bytes: {err}")));
        let started = Instant::now();
        let mut sink = HashCounter::new();
        let restore = standalone
            .restore_checkpoint(
                "merged-full",
                &CheckpointRestoreOptions::default(),
                &mut sink,
            )
            .unwrap_or_else(|err| fail(&format!("standalone restore: {err}")));
        let standalone_restore_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (digest, _) = sink.digest();
        if !(restore.exact_source_file && hex(&digest) == source_sha) {
            fail("standalone merged-full restore is not byte-exact");
        }
        let cell = json!({
            "checkpoint": "merged-full",
            "container_bytes": bytes,
            "container_sha256": source_sha,
            "tensors": report.tensors.len(),
            "manifest_root": report.manifest_root.to_hex(),
            "shared_store_incremental_bytes": shared_incremental,
            "shared_store_write_ms": shared_write_ms,
            "shared_store_restore_ms": shared_restore_ms,
            "shared_store_restore_exact": shared_exact,
            "standalone_store_physical_bytes": standalone_physical,
            "standalone_store_write_ms": standalone_write_ms,
            "standalone_store_restore_ms": standalone_restore_ms,
            "standalone_store_restore_exact": true,
        });
        let _ = fs::remove_file(&container_path);
        cell
    } else {
        Json::Null
    };

    let store_physical = store
        .retained_blob_bytes()
        .unwrap_or_else(|err| fail(&format!("retained bytes: {err}")));
    let verification = store
        .verify(true)
        .unwrap_or_else(|err| fail(&format!("verify: {err}")));

    let receipt = json!({
        "schema": "weft.adapter-court.v1",
        "generated_utc": chrono_like(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
            "single_host": true,
            "warm_cache": true,
            "peak_rss_kib": peak_rss_kib(),
            "note": "one host, warm page cache, real public adapter weights; no GPU, fleet or cloud claim",
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "configuration": {
            "adapters_limit": adapters_limit,
            "merge_limit": merge_limit,
            "merged_full": merged_full,
            "merged_zstd": merged_zstd,
            "dump_tensors": dump_tensors,
            "zstd": zstd,
        },
        "corpus": {
            "manifest": corpus,
            "base_file_bytes": base_file_bytes,
            "base_sha256": base_sha,
            "adapter_count": adapters.len(),
            "merge_eligible": eligible_for_merge.len(),
        },
        "native": {
            "read_ms": native_read_ms,
            "bytes_read": native_bytes,
            "note": "plain files read through std::io from the bind mount; the base file plus every adapter file",
        },
        "base": {
            "checkpoint": "qwen-0.5b-instruct",
            "write_ms": base_write_ms,
            "tensors": base_report.tensors.len(),
            "manifest_root": base_report.manifest_root.to_hex(),
            "physical_bytes": base_physical,
            "zstd19_container_bytes": base_zstd,
            "restore_ms": base_restore_ms,
            "restore_bytes": base_restore_bytes,
            "restore_exact": base_restore_exact,
        },
        "adapters": cells,
        "merged_views": merged_cells,
        "merged_full_checkpoint": full_cell,
        "families": {
            "adapter_source_bytes_total": adapter_source_bytes_total,
            "adapter_logical_total": adapter_logical_total,
            "adapter_physical_total": adapter_physical_total,
            "adapter_zstd19_total": adapter_zstd_total,
            "adapter_restore_ms_total": adapter_restore_ms_total,
            "base_plus_all_adapters_physical": base_physical + adapter_physical_total,
            "base_plus_all_merged_views_physical": base_physical + merged_physical_total,
            "merged_logical_total": merged_logical_total,
            "merged_restore_ms_total": merged_restore_ms_total,
            "merged_zstd19_total": merged_zstd_total,
            "merged_zstd19_count": merged_zstd_count,
            "store_physical_bytes": store_physical,
        },
        "verify": {
            "full": true,
            "checked_objects": verification.checked_objects,
            "checked_manifests": verification.checked_manifests,
            "checked_chunks": verification.checked_chunks,
            "checked_representations": verification.checked_representations,
            "problems": verification.problems.len(),
        },
        "labels": {
            "synthetic": false,
            "adapters_are_public": true,
            "merge_equivalence": "not_claimed: merged bytes are defined by the pinned f32->bf16 contract, not by any upstream merge",
        },
    });
    fs::write(&out_path, serde_json::to_vec_pretty(&receipt).unwrap())
        .unwrap_or_else(|err| fail(&format!("write receipt: {err}")));
    println!("adapter_court: wrote {out_path}");
}

fn chrono_like() -> String {
    // No chrono dependency: format the UTC clock from the Unix timestamp.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, seconds) = (now / 86_400, now % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

// Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
