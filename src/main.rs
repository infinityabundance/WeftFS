//! `weftfs` CLI — store core verbs (no FUSE mount).
//!
//! Every command opens the store explicitly; mutations take the exclusive
//! writer lock. `--json` emits machine-readable output where meaningful. Errors
//! are printed as `Class: message` and exit nonzero.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::conformance::build_fixture_store;
use weftfs::error::{Error, Result};
use weftfs::evaluate::lookup_snapshot;
use weftfs::identity::Hash32;
use weftfs::state::{RelationTarget, State};
use weftfs::store::{OpenMode, Options, SetAttr, Store};
use weftfs::wcb_util::value_to_json;

#[derive(Parser)]
#[command(name = "weftfs", version, about = "WeftFS transactional store core")]
struct Cli {
    /// Store directory.
    #[arg(long, global = true, value_name = "DIR")]
    store: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a new store.
    Init {
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long, default_value_t = 65536)]
        chunk_size: u64,
        #[arg(long, default_value_t = 256)]
        checkpoint_interval: u64,
        #[arg(long, default_value_t = 3600)]
        gc_grace_secs: u64,
    },
    /// Show store information (alias: open).
    Info,
    /// Show store information.
    Open,
    /// Create a file (`--text` or `--file`; empty when neither).
    Create {
        path: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long = "file")]
        input_file: Option<PathBuf>,
        #[arg(long, default_value = "0644")]
        mode: String,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "attr")]
        attrs: Vec<String>,
        #[arg(long = "attr-bytes")]
        attr_bytes: Vec<String>,
        #[arg(long = "attr-int")]
        attr_ints: Vec<String>,
    },
    /// Overwrite a byte range (single atomic commit).
    Write {
        path: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        text: Option<String>,
        #[arg(long = "file")]
        input_file: Option<PathBuf>,
    },
    /// Write file bytes to stdout.
    Cat {
        path: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        len: Option<u64>,
    },
    /// List a directory.
    Ls { path: String },
    /// Show object metadata.
    Stat { path: String },
    /// Remove a directory entry.
    Unlink { path: String },
    /// Rename an entry (atomic delete+insert).
    Rename { from: String, to: String },
    /// Create a hard link.
    Link { target: String, path: String },
    /// Create a directory.
    Mkdir {
        path: String,
        #[arg(long, default_value = "0755")]
        mode: String,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
    },
    /// Create a symlink.
    Symlink {
        target: String,
        path: String,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
    },
    /// Update mode, ownership, mtime, or attributes.
    Setattr {
        path: String,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "mtime-ns")]
        mtime_ns: Option<i64>,
        #[arg(long = "attr")]
        attrs: Vec<String>,
        #[arg(long = "attr-bytes")]
        attr_bytes: Vec<String>,
        #[arg(long = "attr-int")]
        attr_ints: Vec<String>,
    },
    /// Relation assertions.
    Relation {
        #[command(subcommand)]
        command: RelationCommand,
    },
    /// Evaluate a WeftQL selector.
    Query {
        weftql: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "group")]
        groups: Vec<u64>,
    },
    /// Snapshots.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Pin an object at the current or a named commit.
    Pin {
        target: String,
        #[arg(long)]
        snapshot: Option<String>,
    },
    /// Verify the store.
    Verify {
        #[arg(long)]
        full: bool,
    },
    /// Mark-and-sweep garbage collection.
    Gc {
        #[arg(long)]
        apply: bool,
        #[arg(long = "grace-secs")]
        grace_secs: Option<u64>,
    },
    /// Force a checkpoint commit.
    Checkpoint,
    /// Build the deterministic handoff fixture.
    ConformanceStore {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "working-tree")]
        generator_revision: String,
    },
}

#[derive(Subcommand)]
enum RelationCommand {
    /// Add a relation assertion.
    Add {
        #[arg(long)]
        subject: String,
        #[arg(long)]
        predicate: String,
        #[arg(long = "object")]
        object: Option<String>,
        #[arg(long)]
        value: Option<String>,
        #[arg(long = "value-int")]
        value_int: Option<i64>,
        /// Provenance as a JSON object.
        #[arg(long)]
        provenance: Option<String>,
        #[arg(long = "attr")]
        attrs: Vec<String>,
    },
    /// List relations.
    List {
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        predicate: Option<String>,
    },
    /// Delete a relation by id.
    Delete { relation_id: String },
}

#[derive(Subcommand)]
enum SnapshotCommand {
    /// Create a snapshot name.
    Create { name: String },
    /// List snapshots.
    List,
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{}: {}", err.class_name(), err.message());
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let Cli {
        store,
        json,
        command,
    } = cli;
    let needs_store = !matches!(command, Command::ConformanceStore { .. });
    let store = match store {
        Some(path) => path,
        None if !needs_store => PathBuf::new(),
        None => {
            return Err(Error::invalid_argument(
                "--store <DIR> is required for this command",
            ))
        }
    };
    match command {
        Command::Init {
            uid,
            gid,
            chunk_size,
            checkpoint_interval,
            gc_grace_secs,
        } => {
            let (default_uid, default_gid) = current_ids();
            let options = Options {
                owner_uid: uid.unwrap_or(default_uid),
                owner_gid: gid.unwrap_or(default_gid),
                chunk_size,
                checkpoint_interval,
                gc_grace_secs,
                fixed_timestamp_ns: None,
            };
            let store = Store::init(&store, options)?;
            if json {
                print_json(&json!({
                    "generation": store.generation(),
                    "snapshot_root": store.snapshot_root().to_hex(),
                }))
            } else {
                println!("generation={}", store.generation());
                println!("snapshot_root={}", store.snapshot_root());
                Ok(())
            }
        }
        Command::Info | Command::Open => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let state = store.current_state();
            if json {
                print_json(&json!({
                    "format": "weftfs-store 1",
                    "generation": store.generation(),
                    "snapshot_root": store.snapshot_root().to_hex(),
                    "objects": state.objects.len(),
                    "entries": state.entries.len(),
                    "relations": state.relations.len(),
                    "snapshots": state.snapshots.len(),
                    "rejected_generations": store.rejected_generations(),
                    "recovery_notes": store.recovery_notes(),
                }))
            } else {
                println!("format=weftfs-store 1");
                println!("generation={}", store.generation());
                println!("snapshot_root={}", store.snapshot_root());
                println!("objects={}", state.objects.len());
                println!("entries={}", state.entries.len());
                println!("relations={}", state.relations.len());
                println!("snapshots={}", state.snapshots.len());
                for generation in store.rejected_generations() {
                    println!("rejected_generation={generation}");
                }
                for note in store.recovery_notes() {
                    println!("recovery_note={note}");
                }
                Ok(())
            }
        }
        Command::Create {
            path,
            text,
            input_file,
            mode,
            uid,
            gid,
            attrs,
            attr_bytes,
            attr_ints,
        } => {
            let (default_uid, default_gid) = current_ids();
            let bytes = read_input(text, input_file)?;
            let attrs = parse_attrs(&attrs, &attr_bytes, &attr_ints)?;
            let mode = parse_mode(&mode)?;
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            let id = store.create_file(
                parent_id,
                &name,
                &bytes,
                mode,
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                attrs,
            )?;
            if json {
                print_json(&json!({
                    "object_id": id.to_hex(),
                    "path": path,
                    "size": bytes.len(),
                }))
            } else {
                println!("{}", id);
                Ok(())
            }
        }
        Command::Write {
            path,
            offset,
            text,
            input_file,
        } => {
            let bytes = read_input(text, input_file)?;
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let id = store.resolve_path(&path)?;
            let version = store.write_file(id, offset, &bytes)?;
            if json {
                print_json(&json!({
                    "object_id": id.to_hex(),
                    "version_id": version.to_hex(),
                    "generation": store.generation(),
                }))
            } else {
                println!("{version}");
                Ok(())
            }
        }
        Command::Cat { path, offset, len } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let id = store.resolve_path(&path)?;
            let bytes = if offset == 0 && len.is_none() {
                store.read_object(id)?
            } else {
                store.read_range(id, offset, len.unwrap_or(u64::MAX))?
            };
            if json {
                print_json(&json!({
                    "object_id": id.to_hex(),
                    "size": bytes.len(),
                    "hex": hex::encode(&bytes),
                }))
            } else {
                std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|err| Error::io(err.to_string()))?;
                Ok(())
            }
        }
        Command::Ls { path } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let id = store.resolve_path(&path)?;
            let entries = store.list_dir(id)?;
            if json {
                let items: Vec<_> = entries
                    .iter()
                    .map(|entry| {
                        json!({
                            "name": String::from_utf8_lossy(&entry.name),
                            "entry_id": entry.entry_id.to_hex(),
                            "target": entry.target.to_hex(),
                            "kind": store.object_meta(entry.target).map(|r| r.kind.as_str().to_owned()).unwrap_or_default(),
                        })
                    })
                    .collect();
                print_json(&json!({ "path": path, "entries": items }))
            } else {
                for entry in entries {
                    println!("{}", String::from_utf8_lossy(&entry.name));
                }
                Ok(())
            }
        }
        Command::Stat { path } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let id = store.resolve_path(&path)?;
            let record = store.object_meta(id)?;
            if json {
                print_json(&object_json(&record))
            } else {
                println!("object_id={}", record.id);
                println!("kind={}", record.kind.as_str());
                println!("mode={:04o}", record.mode);
                println!("uid={}", record.uid);
                println!("gid={}", record.gid);
                println!("mtime_ns={}", record.mtime_ns);
                println!("version_id={}", record.version);
                println!("content_id={}", record.content);
                println!("size={}", record.size);
                for (key, value) in &record.attrs {
                    println!("attr.{key}={}", json_scalar(value));
                }
                Ok(())
            }
        }
        Command::Unlink { path } => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            store.unlink(parent_id, &name)?;
            success(
                json,
                &format!("unlinked {path}"),
                json!({"path": path, "generation": store.generation()}),
            )
        }
        Command::Rename { from, to } => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let (old_parent, old_name) = split_path(&from)?;
            let (new_parent, new_name) = split_path(&to)?;
            let old_parent_id = store.resolve_path(&old_parent)?;
            let new_parent_id = store.resolve_path(&new_parent)?;
            store.rename(old_parent_id, &old_name, new_parent_id, &new_name)?;
            success(
                json,
                &format!("renamed {from} -> {to}"),
                json!({"from": from, "to": to, "generation": store.generation()}),
            )
        }
        Command::Link { target, path } => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let target_id = resolve_object(&store, &target)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            store.link(target_id, parent_id, &name)?;
            success(
                json,
                &format!("linked {target} -> {path}"),
                json!({"target": target_id.to_hex(), "path": path}),
            )
        }
        Command::Mkdir {
            path,
            mode,
            uid,
            gid,
        } => {
            let (default_uid, default_gid) = current_ids();
            let mode = parse_mode(&mode)?;
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            let id = store.mkdir(
                parent_id,
                &name,
                mode,
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
            )?;
            if json {
                print_json(&json!({"object_id": id.to_hex(), "path": path}))
            } else {
                println!("{id}");
                Ok(())
            }
        }
        Command::Symlink {
            target,
            path,
            uid,
            gid,
        } => {
            let (default_uid, default_gid) = current_ids();
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            let id = store.symlink(
                parent_id,
                &name,
                target.as_bytes(),
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
            )?;
            if json {
                print_json(&json!({"object_id": id.to_hex(), "path": path, "target": target}))
            } else {
                println!("{id}");
                Ok(())
            }
        }
        Command::Setattr {
            path,
            mode,
            uid,
            gid,
            mtime_ns,
            attrs,
            attr_bytes,
            attr_ints,
        } => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let id = store.resolve_path(&path)?;
            let attrs = if attrs.is_empty() && attr_bytes.is_empty() && attr_ints.is_empty() {
                None
            } else {
                Some(parse_attrs(&attrs, &attr_bytes, &attr_ints)?)
            };
            let changes = SetAttr {
                mode: mode.as_deref().map(parse_mode).transpose()?,
                uid,
                gid,
                mtime_ns,
                attrs,
            };
            let version = store.setattr(id, changes)?;
            if json {
                print_json(&json!({"object_id": id.to_hex(), "version_id": version.to_hex()}))
            } else {
                println!("{version}");
                Ok(())
            }
        }
        Command::Relation { command } => run_relation(&store, json, command),
        Command::Query {
            weftql,
            snapshot,
            uid,
            gid,
            groups,
        } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let parsed = selector::parse(&weftql)?;
            let pinned = match snapshot.as_deref() {
                Some(spec) => Some(lookup_snapshot(&store, spec)?),
                None => None,
            };
            let (default_uid, default_gid) = current_ids();
            let principal = Principal::new(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                groups,
            );
            let limits = weftcodec::Limits::default();
            let resolution = store.resolve(&parsed, pinned, &principal, &limits)?;
            let state = match pinned {
                Some(commit) => store.state_at(commit)?,
                None => store.current_state().clone(),
            };
            if json {
                let results: Vec<_> = resolution
                    .results
                    .iter()
                    .map(|reference| {
                        let record = state.objects.get(&reference.object_id);
                        json!({
                            "object_id": reference.object_id.to_hex(),
                            "version_id": reference.version_id.to_hex(),
                            "content_id": reference.content_id.to_hex(),
                            "size": record.map(|r| r.size).unwrap_or(0),
                            "kind": record.map(|r| r.kind.as_str().to_owned()).unwrap_or_else(|| "unknown".to_owned()),
                            "path": state.path_for(reference.object_id).map(|p| String::from_utf8_lossy(&p).into_owned()),
                        })
                    })
                    .collect();
                let evidence: Vec<_> = resolution
                    .evidence
                    .iter()
                    .map(|item| {
                        let entries: Vec<_> = item
                            .entries
                            .iter()
                            .map(|entry| {
                                json!({
                                    "kind": entry.kind,
                                    "field": entry.field,
                                    "value": entry.value.as_ref().map(value_to_json),
                                })
                            })
                            .collect();
                        json!({"object_id": item.object_id.to_hex(), "entries": entries})
                    })
                    .collect();
                print_json(&json!({
                    "selector_id": resolution.selector_id.to_hex(),
                    "snapshot_id": resolution.snapshot_id.to_hex(),
                    "matched_count": resolution.matched_count,
                    "results": results,
                    "evidence": evidence,
                }))
            } else {
                for reference in &resolution.results {
                    let record = state.objects.get(&reference.object_id);
                    let path = state
                        .path_for(reference.object_id)
                        .map(|p| String::from_utf8_lossy(&p).into_owned())
                        .unwrap_or_else(|| "-".to_owned());
                    println!(
                        "{} {} {} {} {} {}",
                        reference.object_id,
                        reference.version_id,
                        reference.content_id,
                        record.map(|r| r.size).unwrap_or(0),
                        record.map(|r| r.kind.as_str()).unwrap_or("unknown"),
                        path
                    );
                }
                println!(
                    "selector_id={} snapshot_id={} matched_count={} returned={}",
                    resolution.selector_id,
                    resolution.snapshot_id,
                    resolution.matched_count,
                    resolution.results.len()
                );
                Ok(())
            }
        }
        Command::Snapshot { command } => match command {
            SnapshotCommand::Create { name } => {
                let mut store = Store::open(&store, OpenMode::ReadWrite)?;
                let pinned = store.snapshot_create(&name)?;
                if json {
                    print_json(&json!({
                        "name": name,
                        "snapshot_id": pinned.to_hex(),
                        "generation": store.generation(),
                    }))
                } else {
                    println!("{pinned}");
                    Ok(())
                }
            }
            SnapshotCommand::List => {
                let store = Store::open(&store, OpenMode::ReadOnly)?;
                let snapshots = store.snapshot_list();
                if json {
                    let map: BTreeMap<String, String> = snapshots
                        .iter()
                        .map(|(name, hash)| (name.clone(), hash.to_hex()))
                        .collect();
                    print_json(&json!({"snapshots": map}))
                } else {
                    for (name, hash) in snapshots {
                        println!("{name} {hash}");
                    }
                    Ok(())
                }
            }
        },
        Command::Pin { target, snapshot } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let id = resolve_object(&store, &target)?;
            let pinned = match snapshot.as_deref() {
                Some(spec) => Some(lookup_snapshot(&store, spec)?),
                None => None,
            };
            let reference = store.pin(id, pinned)?;
            if json {
                print_json(&json!({
                    "namespace": reference.namespace,
                    "snapshot_id": reference.snapshot.to_hex(),
                    "object_id": reference.object_id.to_hex(),
                    "version_id": reference.version_id.to_hex(),
                    "content_id": reference.content_id.to_hex(),
                }))
            } else {
                println!("namespace={}", reference.namespace);
                println!("snapshot_id={}", reference.snapshot);
                println!("object_id={}", reference.object_id);
                println!("version_id={}", reference.version_id);
                println!("content_id={}", reference.content_id);
                Ok(())
            }
        }
        Command::Verify { full } => {
            let store = Store::open(&store, OpenMode::ReadOnly)?;
            let report = store.verify(full)?;
            if json {
                let problems: Vec<_> = report
                    .problems
                    .iter()
                    .map(|problem| json!({"class": problem.class, "detail": problem.detail}))
                    .collect();
                print_json(&json!({
                    "ok": report.ok,
                    "full": report.full,
                    "generation": report.generation,
                    "commit": report.commit.map(|h| h.to_hex()),
                    "checked_commits": report.checked_commits,
                    "checked_objects": report.checked_objects,
                    "checked_manifests": report.checked_manifests,
                    "checked_chunks": report.checked_chunks,
                    "rejected_generations": report.rejected_generations,
                    "problems": problems,
                }))
            } else {
                println!("ok={}", report.ok);
                println!("full={}", report.full);
                println!("generation={}", report.generation);
                println!("checked_commits={}", report.checked_commits);
                println!("checked_objects={}", report.checked_objects);
                println!("checked_manifests={}", report.checked_manifests);
                println!("checked_chunks={}", report.checked_chunks);
                for problem in &report.problems {
                    println!("problem={}: {}", problem.class, problem.detail);
                }
                Ok(())
            }
        }
        Command::Gc { apply, grace_secs } => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            if let Some(grace) = grace_secs {
                let mut options = store.options().clone();
                options.gc_grace_secs = grace;
                store.set_options(options)?;
            }
            let report = store.gc(apply)?;
            if json {
                print_json(&json!({
                    "applied": report.applied,
                    "marked_manifests": report.marked_manifests,
                    "marked_chunks": report.marked_chunks,
                    "candidates": report.candidates,
                    "deleted": report.deleted,
                    "deleted_bytes": report.deleted_bytes,
                    "retained": report.retained,
                    "problems": report.problems,
                }))
            } else {
                println!("applied={}", report.applied);
                println!("marked_manifests={}", report.marked_manifests);
                println!("marked_chunks={}", report.marked_chunks);
                println!("candidates={}", report.candidates.len());
                println!("deleted={}", report.deleted.len());
                println!("deleted_bytes={}", report.deleted_bytes);
                println!("retained={}", report.retained);
                for candidate in &report.candidates {
                    println!("candidate={candidate}");
                }
                for problem in &report.problems {
                    println!("problem={problem}");
                }
                Ok(())
            }
        }
        Command::Checkpoint => {
            let mut store = Store::open(&store, OpenMode::ReadWrite)?;
            let head = store.checkpoint()?;
            if json {
                print_json(&json!({
                    "commit": head.to_hex(),
                    "generation": store.generation(),
                }))
            } else {
                println!("commit={head}");
                println!("generation={}", store.generation());
                Ok(())
            }
        }
        Command::ConformanceStore {
            out,
            generator_revision,
        } => run_conformance_store(&out, &generator_revision, json),
    }
}

fn run_relation(store: &PathBuf, json: bool, command: RelationCommand) -> Result<()> {
    match command {
        RelationCommand::Add {
            subject,
            predicate,
            object,
            value,
            value_int,
            provenance,
            attrs,
        } => {
            let mut store = Store::open(store, OpenMode::ReadWrite)?;
            let subject_id = resolve_object(&store, &subject)?;
            let provided = [object.is_some(), value.is_some(), value_int.is_some()]
                .iter()
                .filter(|flag| **flag)
                .count();
            if provided != 1 {
                return Err(Error::invalid_argument(
                    "relation add needs exactly one of --object, --value, --value-int",
                ));
            }
            let target = if let Some(spec) = object {
                RelationTarget::Object(resolve_object(&store, &spec)?)
            } else if let Some(text) = value {
                RelationTarget::Value(Value::Str(text))
            } else {
                RelationTarget::Value(Value::I64(value_int.unwrap_or(0)))
            };
            let provenance = match provenance {
                Some(text) => {
                    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
                        Error::invalid_argument(format!("bad provenance JSON: {err}"))
                    })?;
                    let value = weftfs::wcb_util::json_to_value(&parsed)?;
                    match value {
                        Value::Map(map) => Some(map),
                        _ => {
                            return Err(Error::invalid_argument("provenance must be a JSON object"))
                        }
                    }
                }
                None => None,
            };
            let attrs = parse_attrs(&attrs, &[], &[])?;
            let id = store.relation_add(subject_id, predicate, target, provenance, attrs)?;
            if json {
                print_json(&json!({
                    "relation_id": id.to_hex(),
                    "subject": subject_id.to_hex(),
                    "generation": store.generation(),
                }))
            } else {
                println!("{id}");
                Ok(())
            }
        }
        RelationCommand::List { subject, predicate } => {
            let store = Store::open(store, OpenMode::ReadOnly)?;
            let subject_id = match subject {
                Some(spec) => Some(resolve_object(&store, &spec)?),
                None => None,
            };
            let mut rows = Vec::new();
            for relation in store.current_state().relations.values() {
                if let Some(subject_id) = subject_id {
                    if relation.subject != subject_id {
                        continue;
                    }
                }
                if let Some(predicate) = &predicate {
                    if relation.predicate != *predicate {
                        continue;
                    }
                }
                rows.push(relation);
            }
            if json {
                let items: Vec<_> = rows
                    .iter()
                    .map(|relation| {
                        let target = match &relation.target {
                            RelationTarget::Object(hash) => {
                                json!({"kind": "object", "object_id": hash.to_hex()})
                            }
                            RelationTarget::Value(value) => {
                                json!({"kind": "value", "value": value_to_json(value)})
                            }
                        };
                        let attrs: BTreeMap<String, serde_json::Value> = relation
                            .attrs
                            .iter()
                            .map(|(key, value)| (key.clone(), value_to_json(value)))
                            .collect();
                        let provenance = relation.provenance.as_ref().map(|map| {
                            map.iter()
                                .map(|(key, value)| (key.clone(), value_to_json(value)))
                                .collect::<BTreeMap<_, _>>()
                        });
                        json!({
                            "relation_id": relation.id.to_hex(),
                            "subject": relation.subject.to_hex(),
                            "predicate": relation.predicate,
                            "kind": relation.kind.as_str(),
                            "target": target,
                            "provenance": provenance,
                            "attrs": attrs,
                        })
                    })
                    .collect();
                print_json(&json!({"relations": items}))
            } else {
                for relation in rows {
                    let target = match &relation.target {
                        RelationTarget::Object(hash) => hash.to_hex(),
                        RelationTarget::Value(value) => json_scalar(value),
                    };
                    println!(
                        "{} {} {} {} {}",
                        relation.id,
                        relation.subject,
                        relation.predicate,
                        relation.kind.as_str(),
                        target
                    );
                }
                Ok(())
            }
        }
        RelationCommand::Delete { relation_id } => {
            let mut store = Store::open(store, OpenMode::ReadWrite)?;
            let id = Hash32::from_hex(&relation_id)?;
            store.relation_delete(id)?;
            success(
                json,
                &format!("deleted relation {relation_id}"),
                json!({"relation_id": id.to_hex(), "generation": store.generation()}),
            )
        }
    }
}

fn run_conformance_store(out: &PathBuf, revision: &str, json: bool) -> Result<()> {
    if out.exists() {
        let mut entries = fs::read_dir(out)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", out.display())))?;
        if entries.next().is_some() {
            return Err(Error::exists(format!(
                "{} is not empty; the fixture is append-only",
                out.display()
            )));
        }
    }
    let summary = build_fixture_store(out)?;

    let objects: Vec<_> = summary
        .objects
        .iter()
        .map(|object| {
            json!({
                "path": object.path,
                "object_id": object.object_id.to_hex(),
                "version_id": object.version_id.to_hex(),
                "content_id": object.content_id.to_hex(),
                "content_hash": object.content_hash.to_hex(),
                "size": object.size,
                "kind": object.kind,
            })
        })
        .collect();
    let selectors: Vec<_> = summary
        .selectors
        .iter()
        .map(|item| {
            json!({
                "source": item.source,
                "selector_id": item.selector_id.to_hex(),
                "snapshot_id": item.snapshot_id.to_hex(),
                "matched_count": item.matched_count,
                "object_ids": item.object_ids.iter().map(|h| h.to_hex()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let snapshots: BTreeMap<String, String> = summary
        .snapshots
        .iter()
        .map(|(name, hash)| (name.clone(), hash.to_hex()))
        .collect();
    let entries: Vec<_> = summary
        .entries
        .iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "entry_id": entry.entry_id.to_hex(),
                "target": entry.target.to_hex(),
            })
        })
        .collect();
    let expected = json!({
        "format": summary.format_version,
        "schema_version": 1,
        "generated_by": format!("weftfs (Rust) {} P4", env!("CARGO_PKG_VERSION")),
        "generator_revision": revision,
        "generation": summary.generation,
        "snapshot_root": summary.head.to_hex(),
        "objects": objects,
        "entries": entries,
        "snapshots": snapshots,
        "relations": summary.relation_ids.iter().map(|h| h.to_hex()).collect::<Vec<_>>(),
        "selectors": selectors,
    });
    let expected_path = out.join("EXPECTED.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&expected_path)
        .map_err(|err| Error::io(format!("cannot create EXPECTED.json: {err}")))?;
    serde_json::to_writer_pretty(&mut file, &expected)
        .map_err(|err| Error::io(format!("cannot write EXPECTED.json: {err}")))?;
    file.write_all(b"\n")?;
    file.sync_all()?;

    let readme = format!(
        "# WeftFS store conformance fixture v1\n\n\
         **Append-only.** Do not edit, regenerate, or \"fix\" files under this\n\
         directory. A correction must add a new fixture version directory.\n\n\
         Generated by the Rust `weftfs` P4 implementation\n\
         (`weftfs (Rust) {}`), source revision `{}`,\n\
         using fixed timestamp `{}` ns and explicit ObjectId seeds.\n\n\
         `EXPECTED.json` is the handoff contract for WeftFS-Go: format version,\n\
         generation, snapshot root, object/version/content identities, paths,\n\
         relation ids, exact file content hashes, and selector results.\n",
        env!("CARGO_PKG_VERSION"),
        revision,
        weftfs::conformance::FIXTURE_TIMESTAMP_NS
    );
    let readme_path = out.join("README.md");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&readme_path)
        .map_err(|err| Error::io(format!("cannot create README.md: {err}")))?;
    file.write_all(readme.as_bytes())?;
    file.sync_all()?;

    if json {
        print_json(&json!({
            "out": out.display().to_string(),
            "generation": summary.generation,
            "snapshot_root": summary.head.to_hex(),
            "objects": summary.objects.len(),
            "selectors": summary.selectors.len(),
        }))
    } else {
        println!("fixture={}", out.display());
        println!("generation={}", summary.generation);
        println!("snapshot_root={}", summary.head);
        Ok(())
    }
}

fn object_json(record: &weftfs::state::ObjectRecord) -> serde_json::Value {
    let attrs: BTreeMap<String, serde_json::Value> = record
        .attrs
        .iter()
        .map(|(key, value)| (key.clone(), value_to_json(value)))
        .collect();
    json!({
        "object_id": record.id.to_hex(),
        "kind": record.kind.as_str(),
        "mode": record.mode,
        "uid": record.uid,
        "gid": record.gid,
        "mtime_ns": record.mtime_ns,
        "version_id": record.version.to_hex(),
        "content_id": record.content.to_hex(),
        "size": record.size,
        "attrs": attrs,
    })
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|err| Error::io(format!("cannot render JSON: {err}")))?;
    println!("{text}");
    Ok(())
}

fn success(json: bool, text: &str, value: serde_json::Value) -> Result<()> {
    if json {
        print_json(&value)
    } else {
        println!("{text}");
        Ok(())
    }
}

fn json_scalar(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        Value::Bytes(bytes) => hex::encode(bytes),
        other => serde_json::to_string(&value_to_json(other)).unwrap_or_default(),
    }
}

fn read_input(text: Option<String>, input_file: Option<PathBuf>) -> Result<Vec<u8>> {
    match (text, input_file) {
        (Some(_), Some(_)) => Err(Error::invalid_argument(
            "--text and --file are mutually exclusive",
        )),
        (Some(text), None) => Ok(text.into_bytes()),
        (None, Some(path)) => fs::read(&path)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display()))),
        (None, None) => Ok(Vec::new()),
    }
}

fn parse_mode(text: &str) -> Result<u64> {
    let trimmed = text.trim();
    let digits = trimmed.strip_prefix("0o").unwrap_or(trimmed);
    u64::from_str_radix(digits, 8)
        .map_err(|_| Error::invalid_argument(format!("invalid octal mode `{text}`")))
}

fn parse_attrs(
    attrs: &[String],
    attr_bytes: &[String],
    attr_ints: &[String],
) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for item in attrs {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            Error::invalid_argument(format!("attribute `{item}` is not key=value"))
        })?;
        out.insert(key.to_owned(), Value::Str(value.to_owned()));
    }
    for item in attr_bytes {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| Error::invalid_argument(format!("attribute `{item}` is not key=hex")))?;
        let bytes = hex::decode(value)
            .map_err(|err| Error::invalid_argument(format!("bad hex for `{key}`: {err}")))?;
        out.insert(key.to_owned(), Value::Bytes(bytes));
    }
    for item in attr_ints {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| Error::invalid_argument(format!("attribute `{item}` is not key=int")))?;
        let number = value
            .parse::<u64>()
            .map_err(|_| Error::invalid_argument(format!("bad integer for `{key}`")))?;
        out.insert(key.to_owned(), Value::U64(number));
    }
    Ok(out)
}

fn split_path(path: &str) -> Result<(String, Vec<u8>)> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(Error::invalid_argument("path must name an entry"));
    }
    let (parent, name) = match trimmed.rfind('/') {
        Some(0) => ("/".to_owned(), &trimmed[1..]),
        Some(index) => (trimmed[..index].to_owned(), &trimmed[index + 1..]),
        None => ("/".to_owned(), trimmed),
    };
    if name.is_empty() {
        return Err(Error::invalid_argument("path must name an entry"));
    }
    Ok((parent, name.as_bytes().to_vec()))
}

fn resolve_object(store: &Store, spec: &str) -> Result<Hash32> {
    if spec.len() == 64 {
        if let Ok(hash) = Hash32::from_hex(spec) {
            if store.current_state().objects.contains_key(&hash) {
                return Ok(hash);
            }
        }
    }
    store.resolve_path(spec)
}

fn current_ids() -> (u64, u64) {
    match fs::read_to_string("/proc/self/status") {
        Ok(text) => {
            let mut uid = 0;
            let mut gid = 0;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("Uid:") {
                    if let Some(first) = rest.split_whitespace().next() {
                        uid = first.parse().unwrap_or(0);
                    }
                } else if let Some(rest) = line.strip_prefix("Gid:") {
                    if let Some(first) = rest.split_whitespace().next() {
                        gid = first.parse().unwrap_or(0);
                    }
                }
            }
            (uid, gid)
        }
        Err(_) => (0, 0),
    }
}

// `State` is referenced in signatures above only through helper construction;
// keep the import meaningful for future CLI verbs that need a full state.
#[allow(dead_code)]
fn _state_type_marker(_state: &State) {}
