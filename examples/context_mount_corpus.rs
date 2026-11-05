//! Deterministic scaled corpus for the F3 context-mount court.
//!
//! The 100-mount court builds its corpus one `weftfs` CLI commit per object and
//! per relation. At 1,000 mounts that is ~9,260 commits, and every mount daemon
//! replays the whole commit chain for its pinned snapshot (`Store::state_at`),
//! so commit-chain replay — not mounting — would dominate the measurement.
//! This harness builds the same logical corpus through the shipped store API
//! with group commits (one publish per `GROUP_OPS` operations), so the court
//! measures concurrent context mounts instead of replayed history.
//!
//! The corpus is byte-for-byte the same logical fixture as the CLI path: the
//! same seeds, attribute values, relation predicates, snapshot name, and
//! selector text. The demo re-verifies the counts with the CLI, runs
//! `verify --full`, and checks every mounted file against the declared SHA-256.
//!
//! Layout for a target of `N` mounts (`N` must be a multiple of 4):
//!
//! * `2N` corpus files (`/corpus/f-<k>.bin`, task `agent-<k % N/2>`,
//!   model `llama-4@ver_<k % N/4>`)
//! * `N/4` model files (`/models/llama-4-ver_<k>.bin`)
//! * 10 grounding documents (`/grounding/g-<k>.txt`)
//! * `3*2N - 1` typed relations (`generated_by`, `grounded_in`, `derived_from`)
//! * one snapshot `all`
//! * `N` Context Roots: `N/2` `by:task`, `N/4` `by:model`, `N/4` `by:derived`
//!
//! Usage (inside the FUSE court):
//!   cargo run --example context_mount_corpus -- \
//!       --target 1000 --store <dir> --out <dir>

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use weftcodec::context::{ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value;
use weftfs::backend::BackendKind;
use weftfs::error::{Error, Result};
use weftfs::identity::Hash32;
use weftfs::state::{RelationTarget, State};
use weftfs::store::{Options, Store};

/// 2026-01-01T00:00:00Z, the fixture clock the demos use.
const FIXED_TS_NS: i64 = 1_767_225_600_000_000_000;
/// Logical operations per group commit. Kept well inside the WCB array bound.
const GROUP_OPS: usize = 256;

struct Spec {
    path: String,
    bytes: Vec<u8>,
    attrs: BTreeMap<String, Value>,
    seed: String,
    sha256: String,
}

struct ContextSpec {
    name: String,
    selector: String,
    members: Vec<Hash32>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn str_attr(value: String) -> BTreeMap<String, Value> {
    let mut attrs = BTreeMap::new();
    attrs.insert("sha256".to_owned(), Value::Str(value));
    attrs
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parse_args() -> Result<(usize, PathBuf, PathBuf)> {
    let mut target: Option<usize> = None;
    let mut store: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| Error::invalid_argument(format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--target" => {
                let text = value()?;
                target = Some(text.parse().map_err(|_| {
                    Error::invalid_argument(format!("--target must be an integer (got `{text}`)"))
                })?);
            }
            "--store" => store = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            other => {
                return Err(Error::invalid_argument(format!(
                    "unknown argument `{other}`"
                )))
            }
        }
    }
    let target = target.ok_or_else(|| Error::invalid_argument("--target is required"))?;
    let store = store.ok_or_else(|| Error::invalid_argument("--store is required"))?;
    let out = out.ok_or_else(|| Error::invalid_argument("--out is required"))?;
    if target % 4 != 0 {
        return Err(Error::invalid_argument(
            "--target must be a positive multiple of 4",
        ));
    }
    Ok((target, store, out))
}

fn main() {
    if let Err(err) = run() {
        eprintln!("context_mount_corpus: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let (target, store_path, out_dir) = parse_args()?;
    let files = target * 2;
    let agents = target / 2;
    let models = target / 4;
    let derived_views = target / 4;
    let ground = 10;
    let step = files / derived_views;
    if step * derived_views != files || step < 2 {
        return Err(Error::invalid_argument(
            "target does not divide into whole derived_from strides",
        ));
    }

    fs::create_dir_all(&out_dir)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", out_dir.display())))?;

    let options = Options {
        fixed_timestamp_ns: Some(FIXED_TS_NS),
        ..Options::default()
    };
    let mut store = Store::init_with_backend(&store_path, options, BackendKind::Local)?;

    let corpus_dir = store.mkdir_seeded(State::ROOT_ID, b"corpus", 0o755, 0, 0, b"dir-corpus")?;
    let models_dir = store.mkdir_seeded(State::ROOT_ID, b"models", 0o755, 0, 0, b"dir-models")?;
    let grounding_dir =
        store.mkdir_seeded(State::ROOT_ID, b"grounding", 0o755, 0, 0, b"dir-grounding")?;

    // ---- object specs (identical strings to the CLI-built 100-mount court) --
    let mut ground_specs = Vec::with_capacity(ground);
    for k in 0..ground {
        // `printf -v` keeps the trailing newline, so the CLI-built 100-mount
        // fixture content ends with a newline too.
        let text = format!("grounding doc {k:02}\n");
        let sha256 = sha256_hex(text.as_bytes());
        ground_specs.push(Spec {
            path: format!("/grounding/g-{k:02}.txt"),
            attrs: str_attr(sha256.clone()),
            sha256,
            bytes: text.into_bytes(),
            seed: format!("ground-{k}"),
        });
    }

    let mut model_specs = Vec::with_capacity(models);
    for k in 0..models {
        let text = format!("model llama-4 ver_{k:02}\n");
        let sha256 = sha256_hex(text.as_bytes());
        model_specs.push(Spec {
            path: format!("/models/llama-4-ver_{k:02}.bin"),
            attrs: str_attr(sha256.clone()),
            sha256,
            bytes: text.into_bytes(),
            seed: format!("model-{k}"),
        });
    }

    let mut corpus_specs = Vec::with_capacity(files);
    for k in 0..files {
        // Attribute values are unpadded (`agent-0`); the content string pads
        // them (`agent-00`). Both match the CLI-built 100-mount court exactly.
        let task = format!("agent-{}", k % agents);
        let model = format!("llama-4@ver_{}", k % models);
        let text = format!(
            "corpus file {k:03} task agent-{:02} model llama-4@ver_{:02}\n",
            k % agents,
            k % models
        );
        let mut attrs = BTreeMap::new();
        attrs.insert("task".to_owned(), Value::Str(task));
        attrs.insert("model".to_owned(), Value::Str(model));
        attrs.insert("sha256".to_owned(), Value::Str(sha256_hex(text.as_bytes())));
        corpus_specs.push(Spec {
            path: format!("/corpus/f-{k:03}.bin"),
            sha256: sha256_hex(text.as_bytes()),
            bytes: text.into_bytes(),
            attrs,
            seed: format!("corpus-{k}"),
        });
    }

    // ---- objects in group commits ------------------------------------------
    let ground_ids = create_objects(&mut store, grounding_dir, &ground_specs)?;
    let model_ids = create_objects(&mut store, models_dir, &model_specs)?;
    let corpus_ids = create_objects(&mut store, corpus_dir, &corpus_specs)?;

    // ---- relations in group commits ----------------------------------------
    let mut relations: Vec<(Hash32, &'static str, Hash32)> = Vec::with_capacity(files * 3 - 1);
    for i in 0..files {
        relations.push((corpus_ids[i], "generated_by", model_ids[i % models]));
        relations.push((corpus_ids[i], "grounded_in", ground_ids[i % ground]));
        if i > 0 {
            relations.push((corpus_ids[i], "derived_from", corpus_ids[i - 1]));
        }
    }
    let relation_count = relations.len();
    for chunk in relations.chunks(GROUP_OPS) {
        store.group(|s| {
            for (subject, predicate, object) in chunk {
                s.relation_add(
                    *subject,
                    *predicate,
                    RelationTarget::Object(*object),
                    None,
                    BTreeMap::new(),
                )?;
            }
            Ok(())
        })?;
    }

    let snapshot = store.snapshot_create("all")?;

    // ---- Context Roots in group commits ------------------------------------
    let mut contexts: Vec<ContextSpec> = Vec::with_capacity(target);
    for n in 0..agents {
        contexts.push(ContextSpec {
            name: format!("by:task=agent-{n}"),
            selector: format!("select attrs.task == \"agent-{n}\""),
            members: (n..files).step_by(agents).map(|k| corpus_ids[k]).collect(),
        });
    }
    for n in 0..models {
        contexts.push(ContextSpec {
            name: format!("by:model=llama-4@ver_{n}"),
            selector: format!("select attrs.model == \"llama-4@ver_{n}\""),
            members: (n..files).step_by(models).map(|k| corpus_ids[k]).collect(),
        });
    }
    for d in 0..derived_views {
        let p = d * step;
        contexts.push(ContextSpec {
            name: format!("by:derived=f-{p:03}"),
            selector: format!(
                "select relation(derived_from, obj:{})",
                corpus_ids[p].to_hex()
            ),
            members: vec![corpus_ids[p + 1]],
        });
    }

    for chunk in contexts.chunks(GROUP_OPS) {
        store.group(|s| {
            for context in chunk {
                let parsed = selector::parse(&context.selector)?;
                let root =
                    ContextRoot::new(&parsed, snapshot, DEFAULT_RESOLVER, Vec::new(), None, None)?;
                s.save_context_root(&context.name, &root, None, false)?;
            }
            Ok(())
        })?;
    }

    // ---- machine-readable manifest for the demo ----------------------------
    let objects_path = out_dir.join("objects.tsv");
    let mut objects_out =
        BufWriter::new(File::create(&objects_path).map_err(|err| {
            Error::io(format!("cannot create {}: {err}", objects_path.display()))
        })?);
    let mut declared = 0usize;
    for (ids, specs) in [
        (&ground_ids, &ground_specs),
        (&model_ids, &model_specs),
        (&corpus_ids, &corpus_specs),
    ] {
        for (id, spec) in ids.iter().zip(specs.iter()) {
            declared += 1;
            writeln!(
                objects_out,
                "{}\t{}\t{}",
                id.to_hex(),
                spec.path,
                spec.sha256
            )
            .map_err(|err| Error::io(format!("cannot write {}: {err}", objects_path.display())))?;
        }
    }
    objects_out
        .flush()
        .map_err(|err| Error::io(format!("cannot write {}: {err}", objects_path.display())))?;

    let contexts_path = out_dir.join("contexts.tsv");
    let mut contexts_out =
        BufWriter::new(File::create(&contexts_path).map_err(|err| {
            Error::io(format!("cannot create {}: {err}", contexts_path.display()))
        })?);
    for context in &contexts {
        let members: Vec<String> = context.members.iter().map(|id| id.to_hex()).collect();
        writeln!(
            contexts_out,
            "{}\t{}\t{}",
            context.name,
            context.selector,
            members.join(",")
        )
        .map_err(|err| Error::io(format!("cannot write {}: {err}", contexts_path.display())))?;
    }
    contexts_out
        .flush()
        .map_err(|err| Error::io(format!("cannot write {}: {err}", contexts_path.display())))?;

    let summary = serde_json::json!({
        "target_mounts": target,
        "corpus_files": files,
        "models": models,
        "grounding": ground,
        "agents": agents,
        "derived_views": derived_views,
        "derived_stride": step,
        "objects": files + models + ground + 3,
        "file_objects": files + models + ground,
        "relations": relation_count,
        "contexts": contexts.len(),
        "generation": store.generation(),
        "head": store.head().to_hex(),
        "snapshot": snapshot.to_hex(),
        "group_ops": GROUP_OPS,
        "declared_objects": declared,
    });
    let summary_path = out_dir.join("corpus.json");
    fs::write(&summary_path, format!("{summary}\n"))
        .map_err(|err| Error::io(format!("cannot write {}: {err}", summary_path.display())))?;
    println!("{summary}");
    Ok(())
}

fn create_objects(store: &mut Store, parent: Hash32, specs: &[Spec]) -> Result<Vec<Hash32>> {
    let mut ids = Vec::with_capacity(specs.len());
    for chunk in specs.chunks(GROUP_OPS) {
        store.group(|s| {
            for spec in chunk {
                let id = s.create_file_seeded(
                    parent,
                    basename(&spec.path).as_bytes(),
                    &spec.bytes,
                    0o644,
                    0,
                    0,
                    spec.attrs.clone(),
                    spec.seed.as_bytes(),
                )?;
                ids.push(id);
            }
            Ok(())
        })?;
    }
    Ok(ids)
}
