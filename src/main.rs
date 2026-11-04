//! `weftfs` CLI — store core verbs (no FUSE mount).
//!
//! Every command opens the store explicitly; mutations take the exclusive
//! writer lock. `--json` emits machine-readable output where meaningful. Errors
//! are printed as `Class: message` and exit nonzero.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};
use serde_json::json;
use weftcodec::context::{Budget, ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value;
use weftcodec::{Region, SemanticPolicy};

use weftfs::auth::Principal;
use weftfs::backend::BackendKind;
use weftfs::conformance::build_fixture_store;
use weftfs::context::{parse_capability_text, CapabilityContext};
use weftfs::error::{Error, Result};
use weftfs::evaluate::lookup_snapshot;
use weftfs::identity::Hash32;
use weftfs::state::{object_causal_closure, RelationTarget, ATTR_CAUSAL_HASH, ATTR_IDENTITY_MODE};
use weftfs::store::{
    OpenMode, Options, RepresentationMode, SetAttr, Store, DEFAULT_CODEC_MIN_BYTES,
};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};
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
    /// Fixture clock override for every generated mtime and commit timestamp.
    #[arg(long, global = true, value_name = "NS")]
    fixed_timestamp_ns: Option<i64>,
    /// Physical representation for newly written file content: `raw` (frozen
    /// v1 manifests) or `codec-auto` (experimental, Rust-only). Also accepted
    /// by `mount`; read-only commands accept and ignore it.
    #[arg(
        long,
        global = true,
        value_name = "raw|codec-auto",
        default_value = "raw"
    )]
    representation: String,
    /// Physical storage backend: `local` (normative POSIX layout) or `object`
    /// (immutable object namespace published with link(2)).
    #[arg(
        long,
        global = true,
        value_name = "local|object",
        default_value = "local"
    )]
    backend: String,
    /// `codec-auto` leaves files smaller than this on the raw path.
    #[arg(
        long = "codec-min-bytes",
        global = true,
        value_name = "N",
        allow_hyphen_values = true
    )]
    codec_min_bytes: Option<String>,
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
        #[arg(long = "attr-u64-list")]
        attr_u64_lists: Vec<String>,
        /// Explicit ObjectId seed (deterministic fixtures).
        #[arg(long)]
        seed: Option<String>,
        /// Causal parent identities (comma-separated 64-hex ids). Selects
        /// `causal_exact`; omitted means `bytes_only`.
        #[arg(long)]
        causal: Option<String>,
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
        /// Causal parent identities (comma-separated 64-hex ids).
        #[arg(long)]
        causal: Option<String>,
        /// Treat the input as a logical tensor (requires `--dtype`/`--shape`).
        #[arg(long)]
        tensor: bool,
        #[arg(long)]
        dtype: Option<String>,
        #[arg(long)]
        shape: Option<String>,
        /// Physical input arrangement: row-major, column-major, blocked, sharded.
        #[arg(long, default_value = "row-major")]
        layout: String,
        #[arg(long = "block-shape")]
        block_shape: Option<String>,
        #[arg(long = "shard-axis", default_value_t = 0)]
        shard_axis: u32,
        #[arg(long, default_value_t = 1)]
        shards: u32,
        #[arg(long = "shard-index", default_value_t = 0)]
        shard_index: u32,
        /// Bytes charged per estimated nanosecond of decode time (0 = bytes only).
        #[arg(long = "lambda-ns", default_value_t = 0)]
        lambda_ns: u64,
        /// `auto` runs the planner; any other value requires that strategy.
        #[arg(long, default_value = "auto")]
        strategy: String,
        /// Semantic policy: none | mantissa_truncate:N | block_quantize:B:L.
        #[arg(long = "semantic-policy")]
        semantic_policy: Option<String>,
        /// Explicit semantic candidate ObjectIds (comma-separated hex).
        #[arg(long)]
        candidates: Option<String>,
    },
    /// Write file bytes to stdout.
    Cat {
        path: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        len: Option<u64>,
        /// Logical tensor hyper-rectangle `s0,s1:e0,e1` (row-major).
        #[arg(long)]
        region: Option<String>,
    },
    /// Price the tensor representation planner without writing anything.
    Plan {
        #[arg(long = "file")]
        input_file: PathBuf,
        #[arg(long)]
        dtype: String,
        #[arg(long)]
        shape: String,
        #[arg(long, default_value = "row-major")]
        layout: String,
        #[arg(long = "block-shape")]
        block_shape: Option<String>,
        #[arg(long = "shard-axis", default_value_t = 0)]
        shard_axis: u32,
        #[arg(long, default_value_t = 1)]
        shards: u32,
        #[arg(long = "shard-index", default_value_t = 0)]
        shard_index: u32,
        #[arg(long = "lambda-ns", default_value_t = 0)]
        lambda_ns: u64,
        #[arg(long)]
        causal: Option<String>,
        #[arg(long = "semantic-policy")]
        semantic_policy: Option<String>,
        #[arg(long)]
        candidates: Option<String>,
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
        /// Explicit ObjectId seed (deterministic fixtures).
        #[arg(long)]
        seed: Option<String>,
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
        #[arg(long = "attr-u64-list")]
        attr_u64_lists: Vec<String>,
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
        /// Reject zero or multiple matches with NotFound / Ambiguous.
        #[arg(long = "require-one")]
        require_one: bool,
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
    /// Context Root persistence, resolution, and materialization.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Causal closure inspection.
    Causal {
        #[command(subcommand)]
        command: CausalCommand,
    },
    /// `weft://` address canonicalization over the local store.
    Uri {
        #[command(subcommand)]
        command: UriCommand,
    },
    /// Mount the store over FUSE (requires the `fuse` feature).
    #[cfg(feature = "fuse")]
    Mount {
        /// Existing empty directory to mount on.
        #[arg(long, value_name = "DIR")]
        mountpoint: PathBuf,
        /// Stay in the foreground (the only mode in v1).
        #[arg(long)]
        foreground: bool,
        /// Allow users other than the mounting user.
        #[arg(long = "allow-other")]
        allow_other: bool,
        /// Mount a named snapshot (or commit hex) read-only.
        #[arg(long)]
        snapshot: Option<String>,
        /// Mount the Context Root's result set as the read-only mount root.
        #[arg(long, value_name = "NAME|weft://...")]
        context: Option<String>,
        /// Mount the newest commit at or before this time (RFC 3339 or unix ns).
        #[arg(long, value_name = "RFC3339|UNIX_NS")]
        at: Option<String>,
        /// Mount the current state read-only.
        #[arg(long = "read-only")]
        read_only: bool,
        /// Capability JSON or canonical-hex file narrowing exact-id and
        /// saved-context views.
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
    },
    /// Build the deterministic handoff fixture.
    ConformanceStore {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "working-tree")]
        generator_revision: String,
    },
    /// Write a self-contained, replayable `weft-receipt.tar.zst` bundle.
    ExportBundle {
        /// Output bundle path (`.tar.zst`).
        #[arg(long)]
        out: PathBuf,
        /// Restrict the bundle to these Context Roots (default: all stored).
        #[arg(long = "context")]
        contexts: Vec<String>,
        /// Extra `name=revision` source records for `manifest.json`.
        #[arg(long = "source-revision")]
        source_revisions: Vec<String>,
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

#[derive(Subcommand)]
enum ContextCommand {
    /// Persist a Context Root relation.
    Create {
        #[arg(long)]
        name: String,
        /// Selector source text, or `@FILE`.
        #[arg(long)]
        selector: String,
        /// Snapshot name or commit hex.
        #[arg(long)]
        snapshot: String,
        /// Comma-separated causal entries (64-hex ids).
        #[arg(long)]
        causal: Option<String>,
        /// Capability JSON or canonical-hex file.
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        #[arg(long = "max-results")]
        max_results: Option<u64>,
        #[arg(long = "max-bytes")]
        max_bytes: Option<u64>,
        #[arg(long = "max-depth")]
        max_depth: Option<u64>,
        #[arg(long = "max-work")]
        max_work: Option<u64>,
        /// Replace an existing same-name root in one commit.
        #[arg(long)]
        replace: bool,
    },
    /// List stored Context Roots.
    List,
    /// Show the stored record and identity.
    Show { target: String },
    /// Resolve a Context Root to its ordered result set.
    Resolve {
        target: String,
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "group")]
        groups: Vec<u64>,
    },
    /// Materialize exact bytes into an output directory atomically.
    Materialize {
        target: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "group")]
        groups: Vec<u64>,
        /// Fail closed with `Ambiguous` unless the selector matches exactly one
        /// object. No output is produced on failure.
        #[arg(long = "require-one")]
        require_one: bool,
    },
    /// Print the canonical authoritative URI.
    Uri { target: String },
    /// Delete a stored Context Root by name.
    Delete { name: String },
}

#[derive(Subcommand)]
enum CausalCommand {
    /// Show the recorded causal closure for an object/path/id.
    Show { target: String },
}

#[derive(Subcommand)]
enum UriCommand {
    /// Canonicalize any `weft://` spelling.
    Canonicalize { uri: String },
    /// Show the authoritative URI for a stored name or address.
    Show { target: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{}: {}", err.class_name(), err.message());
        std::process::exit(1);
    }
}

/// Resolved store-open policy from the global CLI flags. Applied wherever
/// `Options` is constructed, so every verb that opens the store for writing
/// sees the same representation policy; read-only verbs simply never write.
#[derive(Clone, Copy, Debug)]
enum CliBackend {
    Local,
    Object,
}

impl CliBackend {
    fn kind(self) -> BackendKind {
        match self {
            CliBackend::Local => BackendKind::Local,
            CliBackend::Object => BackendKind::Object,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct OpenPolicy {
    fixed_timestamp_ns: Option<i64>,
    representation: RepresentationMode,
    codec_min_bytes: u64,
    backend: CliBackend,
}

impl OpenPolicy {
    fn options(self) -> Options {
        Options {
            fixed_timestamp_ns: self.fixed_timestamp_ns,
            representation: self.representation,
            codec_min_bytes: self.codec_min_bytes,
            ..Options::default()
        }
    }
}

fn parse_backend(text: &str) -> Result<CliBackend> {
    match text {
        "local" => Ok(CliBackend::Local),
        "object" => Ok(CliBackend::Object),
        other => Err(Error::invalid_argument(format!(
            "--backend must be `local` or `object` (got `{other}`)"
        ))),
    }
}

fn parse_representation(text: &str) -> Result<RepresentationMode> {
    match text {
        "raw" => Ok(RepresentationMode::Raw),
        "codec-auto" => Ok(RepresentationMode::CodecAuto),
        other => Err(Error::invalid_argument(format!(
            "--representation must be `raw` or `codec-auto` (got `{other}`)"
        ))),
    }
}

fn parse_codec_min_bytes(text: Option<&str>) -> Result<u64> {
    match text {
        None => Ok(DEFAULT_CODEC_MIN_BYTES),
        Some(text) => text.parse::<u64>().map_err(|_| {
            Error::invalid_argument(format!(
                "--codec-min-bytes must be a non-negative integer (got `{text}`)"
            ))
        }),
    }
}

fn run(cli: Cli) -> Result<()> {
    let Cli {
        store,
        json,
        fixed_timestamp_ns,
        representation,
        backend,
        codec_min_bytes,
        command,
    } = cli;
    let policy = OpenPolicy {
        fixed_timestamp_ns,
        representation: parse_representation(&representation)?,
        codec_min_bytes: parse_codec_min_bytes(codec_min_bytes.as_deref())?,
        backend: parse_backend(&backend)?,
    };
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
                fixed_timestamp_ns: policy.fixed_timestamp_ns,
                representation: policy.representation,
                codec_min_bytes: policy.codec_min_bytes,
                ..Options::default()
            };
            let store = Store::init_with_backend(&store, options, policy.backend.kind())?;
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
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
                    "representation": policy.representation.as_str(),
                    "codec_min_bytes": policy.codec_min_bytes,
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
                println!("representation={}", policy.representation.as_str());
                println!("codec_min_bytes={}", policy.codec_min_bytes);
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
            attr_u64_lists,
            seed,
            causal,
        } => {
            let (default_uid, default_gid) = current_ids();
            let bytes = read_input(text, input_file)?;
            let attrs = parse_attrs(&attrs, &attr_bytes, &attr_ints, &attr_u64_lists)?;
            let mode = parse_mode(&mode)?;
            let causal = parse_causal_entries(causal.as_deref())?;
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            let id = if causal.is_empty() {
                match seed {
                    Some(seed) => store.create_file_seeded(
                        parent_id,
                        &name,
                        &bytes,
                        mode,
                        uid.unwrap_or(default_uid),
                        gid.unwrap_or(default_gid),
                        attrs,
                        seed.as_bytes(),
                    )?,
                    None => store.create_file(
                        parent_id,
                        &name,
                        &bytes,
                        mode,
                        uid.unwrap_or(default_uid),
                        gid.unwrap_or(default_gid),
                        attrs,
                    )?,
                }
            } else {
                let seed_bytes = match seed {
                    Some(seed) => seed.into_bytes(),
                    None => weftfs::identity::os_random_32()?.to_vec(),
                };
                store.create_file_seeded_causal(
                    parent_id,
                    &name,
                    &bytes,
                    mode,
                    uid.unwrap_or(default_uid),
                    gid.unwrap_or(default_gid),
                    attrs,
                    &seed_bytes,
                    &causal,
                )?
            };
            if json {
                print_json(&json!({
                    "object_id": id.to_hex(),
                    "path": path,
                    "size": bytes.len(),
                    "identity_mode": if causal.is_empty() { "bytes_only" } else { "causal_exact" },
                }))
            } else {
                println!("{id}");
                Ok(())
            }
        }
        Command::Write {
            path,
            offset,
            text,
            input_file,
            causal,
            tensor,
            dtype,
            shape,
            layout,
            block_shape,
            shard_axis,
            shards,
            shard_index,
            lambda_ns,
            strategy,
            semantic_policy,
            candidates,
        } => {
            let bytes = read_input(text, input_file)?;
            let causal = parse_causal_entries(causal.as_deref())?;
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            if tensor {
                let dtype =
                    dtype.ok_or_else(|| Error::invalid_argument("--tensor requires --dtype"))?;
                let shape = parse_u64_list(
                    shape
                        .as_deref()
                        .ok_or_else(|| Error::invalid_argument("--tensor requires --shape"))?,
                    "--shape",
                )?;
                let layout = parse_tensor_layout(
                    &layout,
                    block_shape.as_deref(),
                    shard_axis,
                    shards,
                    shard_index,
                )?;
                let spec = TensorWriteSpec {
                    strategy_hint: parse_tensor_strategy(&strategy)?,
                    causal,
                    plan: TensorPlanOptions {
                        lambda_ns,
                        semantic_policy: parse_semantic_policy(semantic_policy.as_deref())?,
                        candidates: parse_object_ids(candidates.as_deref())?,
                        ..TensorPlanOptions::default()
                    },
                };
                let (parent, name) = split_path(&path)?;
                let parent_id = store.resolve_path(&parent)?;
                let id = store.write_tensor_file_with_spec(
                    parent_id, &name, &dtype, &shape, &bytes, &layout, &spec,
                )?;
                let meta = store.tensor_metadata(id)?;
                if json {
                    print_json(&json!({
                        "object_id": id.to_hex(),
                        "path": path,
                        "dtype": meta.dtype.as_str(),
                        "shape": meta.shape,
                        "layout": meta.layout.name(),
                        "strategy": meta.strategy,
                        "logical_root": meta.logical_root.to_hex(),
                        "size": bytes.len(),
                        "generation": store.generation(),
                    }))
                } else {
                    println!("{id}");
                    Ok(())
                }
            } else {
                let id = store.resolve_path(&path)?;
                let version = store.write_file_causal(id, offset, &bytes, &causal)?;
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
        }
        Command::Plan {
            input_file,
            dtype,
            shape,
            layout,
            block_shape,
            shard_axis,
            shards,
            shard_index,
            lambda_ns,
            causal,
            semantic_policy,
            candidates,
        } => {
            let bytes = fs::read(&input_file)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", input_file.display())))?;
            let shape = parse_u64_list(&shape, "--shape")?;
            let layout = parse_tensor_layout(
                &layout,
                block_shape.as_deref(),
                shard_axis,
                shards,
                shard_index,
            )?;
            let causal = parse_causal_entries(causal.as_deref())?;
            let options = TensorPlanOptions {
                lambda_ns,
                semantic_policy: parse_semantic_policy(semantic_policy.as_deref())?,
                candidates: parse_object_ids(candidates.as_deref())?,
                ..TensorPlanOptions::default()
            };
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
            let plan = store.plan_tensor(&dtype, &shape, &bytes, &layout, &causal, &options)?;
            if json {
                let candidates: Vec<serde_json::Value> = plan
                    .candidates
                    .iter()
                    .map(|item| {
                        json!({
                            "strategy": item.strategy.as_str(),
                            "applicable": item.applicable,
                            "detail": item.detail,
                            "physical_bytes": item.physical_bytes,
                            "estimated_decode_ns": item.estimated_decode_ns,
                            "cost": item.cost.map(|cost| cost.to_string()),
                            "dependency": item.dependency.map(|hash| hash.to_hex()),
                        })
                    })
                    .collect();
                print_json(&json!({
                    "chosen": plan.chosen.as_str(),
                    "chosen_physical_bytes": plan.chosen_physical_bytes,
                    "chosen_estimated_decode_ns": plan.chosen_estimated_decode_ns,
                    "chosen_cost": plan.chosen_cost.to_string(),
                    "runner_up": plan.runner_up.map(|strategy| strategy.as_str()),
                    "runner_up_physical_bytes": plan.runner_up_physical_bytes,
                    "runner_up_estimated_decode_ns": plan.runner_up_estimated_decode_ns,
                    "runner_up_cost": plan.runner_up_cost.map(|cost| cost.to_string()),
                    "lambda_ns": lambda_ns,
                    "candidates": candidates,
                }))
            } else {
                println!("chosen={}", plan.chosen.as_str());
                println!("chosen_physical_bytes={}", plan.chosen_physical_bytes);
                println!(
                    "chosen_estimated_decode_ns={}",
                    plan.chosen_estimated_decode_ns
                );
                println!("chosen_cost={}", plan.chosen_cost);
                if let Some(runner) = plan.runner_up {
                    println!("runner_up={}", runner.as_str());
                }
                for item in &plan.candidates {
                    println!(
                        "candidate={} applicable={} physical_bytes={} estimated_decode_ns={} detail={}",
                        item.strategy.as_str(),
                        item.applicable,
                        item.physical_bytes
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                        item.estimated_decode_ns
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                        item.detail
                    );
                }
                Ok(())
            }
        }
        Command::Cat {
            path,
            offset,
            len,
            region,
        } => {
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
            let id = store.resolve_path(&path)?;
            let bytes = if let Some(region) = region {
                let region = parse_region(&region)?;
                store.read_tensor_region(id, &region)?.bytes
            } else if offset == 0 && len.is_none() {
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
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
            seed,
        } => {
            let (default_uid, default_gid) = current_ids();
            let mode = parse_mode(&mode)?;
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let (parent, name) = split_path(&path)?;
            let parent_id = store.resolve_path(&parent)?;
            let id = match seed {
                Some(seed) => store.mkdir_seeded(
                    parent_id,
                    &name,
                    mode,
                    uid.unwrap_or(default_uid),
                    gid.unwrap_or(default_gid),
                    seed.as_bytes(),
                )?,
                None => store.mkdir(
                    parent_id,
                    &name,
                    mode,
                    uid.unwrap_or(default_uid),
                    gid.unwrap_or(default_gid),
                )?,
            };
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
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
            attr_u64_lists,
        } => {
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let id = store.resolve_path(&path)?;
            let attrs = if attrs.is_empty()
                && attr_bytes.is_empty()
                && attr_ints.is_empty()
                && attr_u64_lists.is_empty()
            {
                None
            } else {
                Some(parse_attrs(
                    &attrs,
                    &attr_bytes,
                    &attr_ints,
                    &attr_u64_lists,
                )?)
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
        Command::Relation { command } => run_relation(&store, json, policy, command),
        Command::Query {
            weftql,
            snapshot,
            uid,
            gid,
            groups,
            require_one,
        } => {
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            if require_one {
                let reference = store.require_one(&parsed, pinned, &principal, &limits)?;
                let selector_id = parsed.selector_id_with_limits(&limits)?;
                if json {
                    print_json(&json!({
                        "selector_id": selector_id.to_hex(),
                        "snapshot_id": reference.snapshot.to_hex(),
                        "matched_count": 1,
                        "object_id": reference.object_id.to_hex(),
                        "version_id": reference.version_id.to_hex(),
                        "content_id": reference.content_id.to_hex(),
                        "namespace": reference.namespace,
                    }))
                } else {
                    println!("object_id={}", reference.object_id);
                    println!("version_id={}", reference.version_id);
                    println!("content_id={}", reference.content_id);
                    println!("snapshot_id={}", reference.snapshot);
                    Ok(())
                }
            } else {
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
        }
        Command::Snapshot { command } => match command {
            SnapshotCommand::Create { name } => {
                let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
                let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
                    "checked_context_roots": report.checked_context_roots,
                    "checked_causal": report.checked_causal,
                    "checked_representations": report.checked_representations,
                    "checked_capabilities": report.checked_capabilities,
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
                println!("checked_context_roots={}", report.checked_context_roots);
                println!("checked_causal={}", report.checked_causal);
                println!("checked_representations={}", report.checked_representations);
                println!("checked_capabilities={}", report.checked_capabilities);
                for problem in &report.problems {
                    println!("problem={}: {}", problem.class, problem.detail);
                }
                Ok(())
            }
        }
        Command::Gc { apply, grace_secs } => {
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
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
        Command::Context { command } => run_context(&store, json, policy, command),
        Command::Causal { command } => run_causal(&store, json, policy, command),
        Command::Uri { command } => run_uri(&store, json, policy, command),
        #[cfg(feature = "fuse")]
        Command::Mount {
            mountpoint,
            foreground,
            allow_other,
            snapshot,
            context,
            at,
            read_only,
            capability_file,
        } => {
            // v1 does not daemonize: the process always stays in the foreground
            // so SIGINT/SIGTERM can trigger a clean unmount.
            let _ = foreground;
            let capability = read_capability(&capability_file)?;
            if context.is_some() && at.is_some() {
                return Err(Error::invalid_argument(
                    "--context pins its own snapshot; --at cannot be combined with it",
                ));
            }
            let read_only = read_only
                || snapshot.is_some()
                || context.is_some()
                || at.is_some()
                || capability.is_some();
            let mode = if read_only {
                OpenMode::ReadOnly
            } else {
                OpenMode::ReadWrite
            };
            let store = open_store(&store, mode, policy)?;

            let limits = weftcodec::Limits::default();
            let (context_mount, snapshot_commit, snapshot_state) = match context {
                Some(target) => {
                    let resolved = store.resolve_context(
                        &target,
                        capability.as_ref(),
                        &Principal::root(),
                        &limits,
                        store.clock_now_ns(),
                    )?;
                    let snapshot = resolved.stored.root.snapshot;
                    let state = store.state_at(snapshot)?;
                    let context_mount = weftfs::fuse::ContextMount {
                        name: resolved.stored.name.clone(),
                        context_root_id: resolved.stored.context_root_id,
                        snapshot,
                        selector_id: resolved.resolution.selector_id,
                        results: resolved
                            .resolution
                            .results
                            .iter()
                            .map(|result| result.object_id)
                            .collect(),
                    };
                    (Some(context_mount), Some(snapshot), Some(state))
                }
                None => match at {
                    Some(at_text) => {
                        let at_ns = weftfs::timeparse::parse_at_ns(&at_text)?;
                        let base = match &snapshot {
                            Some(name) => lookup_snapshot(&store, name)?,
                            None => store.head(),
                        };
                        let selected =
                            store.commit_at_or_before(base, at_ns)?.ok_or_else(|| {
                                Error::not_found(format!(
                                    "no commit at or before {at_ns} ns in the mounted lineage"
                                ))
                            })?;
                        let state = store.state_at(selected.commit)?;
                        (None, Some(selected.commit), Some(state))
                    }
                    None => match snapshot {
                        Some(name) => {
                            let commit = lookup_snapshot(&store, &name)?;
                            let state = store.state_at(commit)?;
                            (None, Some(commit), Some(state))
                        }
                        None => (None, None, None),
                    },
                },
            };
            weftfs::fuse::run(
                store,
                mountpoint,
                weftfs::fuse::MountOptions {
                    allow_other,
                    read_only,
                    snapshot: snapshot_commit,
                    snapshot_state,
                    capability,
                    context: context_mount,
                },
            )
        }
        Command::ConformanceStore {
            out,
            generator_revision,
        } => run_conformance_store(&out, &generator_revision, json),
        Command::ExportBundle {
            out,
            contexts,
            source_revisions,
        } => run_export_bundle(&store, &out, &contexts, &source_revisions, policy, json),
    }
}

fn open_store(path: &std::path::Path, mode: OpenMode, policy: OpenPolicy) -> Result<Store> {
    Store::open_with_options_and_backend(path, mode, policy.options(), policy.backend.kind())
}

fn run_relation(
    store: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    command: RelationCommand,
) -> Result<()> {
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
            let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
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
            let attrs = parse_attrs(&attrs, &[], &[], &[])?;
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
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
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
            let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
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

fn parse_causal_entries(spec: Option<&str>) -> Result<Vec<Hash32>> {
    match spec {
        None => Ok(Vec::new()),
        Some(text) if text.trim().is_empty() => Ok(Vec::new()),
        Some(text) => parse_hash_list(text),
    }
}

fn parse_u64_list(text: &str, flag: &str) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(Error::invalid_argument(format!(
                "{flag} has an empty component"
            )));
        }
        out.push(part.parse::<u64>().map_err(|_| {
            Error::invalid_argument(format!("{flag} component `{part}` is not a u64"))
        })?);
    }
    Ok(out)
}

fn parse_tensor_layout(
    name: &str,
    block_shape: Option<&str>,
    axis: u32,
    shards: u32,
    index: u32,
) -> Result<TensorLayoutSpec> {
    match name {
        "row-major" => Ok(TensorLayoutSpec::row_major()),
        "column-major" => Ok(TensorLayoutSpec::column_major()),
        "blocked" => {
            let block = block_shape.ok_or_else(|| {
                Error::invalid_argument("--layout blocked requires --block-shape")
            })?;
            Ok(TensorLayoutSpec::blocked(parse_u64_list(
                block,
                "--block-shape",
            )?))
        }
        "sharded" => Ok(TensorLayoutSpec::sharded(axis, shards, index)),
        other => Err(Error::invalid_argument(format!(
            "--layout must be row-major, column-major, blocked, or sharded (got `{other}`)"
        ))),
    }
}

fn parse_tensor_strategy(text: &str) -> Result<Option<TensorStrategy>> {
    match text {
        "auto" => Ok(None),
        other => Ok(Some(TensorStrategy::parse(other)?)),
    }
}

fn parse_semantic_policy(text: Option<&str>) -> Result<Option<SemanticPolicy>> {
    let Some(text) = text else {
        return Ok(None);
    };
    let text = text.trim();
    if text == "none" {
        return Ok(Some(SemanticPolicy::None));
    }
    if let Some(bits) = text.strip_prefix("mantissa_truncate:") {
        let bits = bits.parse::<u32>().map_err(|_| {
            Error::invalid_argument("--semantic-policy mantissa_truncate bits is not a u32")
        })?;
        return Ok(Some(SemanticPolicy::MantissaTruncate { bits }));
    }
    if let Some(rest) = text.strip_prefix("block_quantize:") {
        let (block, levels) = rest.split_once(':').ok_or_else(|| {
            Error::invalid_argument("--semantic-policy block_quantize expects block:levels")
        })?;
        let block_size = block
            .parse::<u64>()
            .map_err(|_| Error::invalid_argument("--semantic-policy block size is not a u64"))?;
        let levels = levels
            .parse::<u64>()
            .map_err(|_| Error::invalid_argument("--semantic-policy levels is not a u64"))?;
        return Ok(Some(SemanticPolicy::BlockQuantize { block_size, levels }));
    }
    Err(Error::invalid_argument(format!(
        "--semantic-policy must be none, mantissa_truncate:N, or block_quantize:B:L (got `{text}`)"
    )))
}

fn parse_object_ids(spec: Option<&str>) -> Result<Vec<Hash32>> {
    match spec {
        None => Ok(Vec::new()),
        Some(text) if text.trim().is_empty() => Ok(Vec::new()),
        Some(text) => parse_hash_list(text),
    }
}

fn parse_region(text: &str) -> Result<Region> {
    let (start, end) = text
        .split_once(':')
        .ok_or_else(|| Error::invalid_argument("--region must be `s0,s1:e0,e1`"))?;
    let start = parse_u64_list(start, "--region start")?;
    let end = parse_u64_list(end, "--region end")?;
    if start.len() != end.len() {
        return Err(Error::invalid_argument(
            "--region start and end ranks differ",
        ));
    }
    Ok(Region::new(start, end))
}

fn parse_hash_list(text: &str) -> Result<Vec<Hash32>> {
    let mut out = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(Error::invalid_argument("hash list contains an empty entry"));
        }
        out.push(Hash32::from_hex(entry)?);
    }
    Ok(out)
}

fn read_selector_source(spec: &str) -> Result<String> {
    if let Some(path) = spec.strip_prefix('@') {
        let bytes = fs::read(path)
            .map_err(|err| Error::io(format!("cannot read selector file {path}: {err}")))?;
        String::from_utf8(bytes).map_err(|_| Error::parse("selector file is not valid UTF-8"))
    } else {
        Ok(spec.to_owned())
    }
}

fn read_capability(path: &Option<PathBuf>) -> Result<Option<CapabilityContext>> {
    match path {
        None => Ok(None),
        Some(path) => {
            let text = fs::read_to_string(path).map_err(|err| {
                Error::io(format!("cannot read capability {}: {err}", path.display()))
            })?;
            let scope = parse_capability_text(&text)?;
            CapabilityContext::new(scope).map(Some)
        }
    }
}

fn budget_from_flags(
    max_results: Option<u64>,
    max_bytes: Option<u64>,
    max_depth: Option<u64>,
    max_work: Option<u64>,
) -> Result<Option<Budget>> {
    let mut budget = Budget::new();
    budget.max_results = max_results;
    budget.max_bytes = max_bytes;
    budget.max_depth = max_depth;
    budget.max_work = max_work;
    if budget.is_empty() {
        Ok(None)
    } else {
        budget.validate()?;
        Ok(Some(budget))
    }
}

fn identity_mode_of(record: &weftfs::ObjectRecord) -> &'static str {
    match record.attrs.get(ATTR_IDENTITY_MODE) {
        Some(Value::Str(text)) if text == "causal_exact" => "causal_exact",
        _ => "bytes_only",
    }
}

fn context_root_json(stored: &weftfs::StoredContextRoot) -> Result<serde_json::Value> {
    Ok(json!({
        "name": stored.name,
        "context_root_id": stored.context_root_id.to_hex(),
        "uri": stored.uri,
        "relation_id": stored.relation_id.to_hex(),
        "namespace": stored.root.namespace,
        "selector_id": stored.root.selector_id.to_hex(),
        "selector_canonical_hex": hex::encode(&stored.root.selector_bytes),
        "snapshot_id": stored.root.snapshot.to_hex(),
        "resolver": stored.root.resolver,
        "ordering": stored.root.ordering,
        "closure": stored.root.causal.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
        "capability_hash": stored.root.capability_hash.map(|hash| hash.to_hex()),
        "budget": stored.root.budget.as_ref().map(|budget| budget.to_json()),
        "canonical_hex": stored.record_hex,
    }))
}

fn run_context(
    store: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    command: ContextCommand,
) -> Result<()> {
    match command {
        ContextCommand::Create {
            name,
            selector,
            snapshot,
            causal,
            capability_file,
            max_results,
            max_bytes,
            max_depth,
            max_work,
            replace,
        } => {
            let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
            let source = read_selector_source(&selector)?;
            let parsed = selector::parse(&source)?;
            let snapshot_id = lookup_snapshot(&store, &snapshot)?;
            let causal = parse_causal_entries(causal.as_deref())?;
            let causal = if causal.is_empty() {
                causal
            } else {
                weftfs::context::compute_causal_closure(
                    store.current_state(),
                    &causal,
                    &weftcodec::Limits::default(),
                )?
                .closure
            };
            let capability = read_capability(&capability_file)?;
            let budget = budget_from_flags(max_results, max_bytes, max_depth, max_work)?;
            let root = ContextRoot::new(
                &parsed,
                snapshot_id,
                DEFAULT_RESOLVER,
                causal,
                capability.as_ref().map(|cap| cap.hash),
                budget,
            )?;
            let relation_id = store.save_context_root(
                &name,
                &root,
                capability.as_ref().map(|cap| &cap.scope),
                replace,
            )?;
            let stored = store.load_context_root(&name)?;
            if json {
                let mut output = context_root_json(&stored)?;
                output["generation"] = json!(store.generation());
                output["relation_id"] = json!(relation_id.to_hex());
                print_json(&output)
            } else {
                println!("context_root_id={}", stored.context_root_id);
                println!("uri={}", stored.uri);
                println!("relation_id={relation_id}");
                Ok(())
            }
        }
        ContextCommand::List => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let contexts = store.list_context_roots()?;
            if json {
                let items: Vec<_> = contexts
                    .iter()
                    .map(context_root_json)
                    .collect::<Result<Vec<_>>>()?;
                print_json(&json!({"contexts": items}))
            } else {
                for stored in contexts {
                    println!("{} {} {}", stored.name, stored.context_root_id, stored.uri);
                }
                Ok(())
            }
        }
        ContextCommand::Show { target } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let stored = store.load_context_root(&target)?;
            if json {
                print_json(&context_root_json(&stored)?)
            } else {
                println!("name={}", stored.name);
                println!("context_root_id={}", stored.context_root_id);
                println!("uri={}", stored.uri);
                println!("selector_id={}", stored.root.selector_id);
                println!("snapshot_id={}", stored.root.snapshot);
                println!("resolver={}", stored.root.resolver);
                println!("ordering={}", stored.root.ordering);
                for entry in &stored.root.causal {
                    println!("closure={entry}");
                }
                if let Some(hash) = stored.root.capability_hash {
                    println!("capability_hash={hash}");
                }
                Ok(())
            }
        }
        ContextCommand::Resolve {
            target,
            capability_file,
            uid,
            gid,
            groups,
        } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let capability = read_capability(&capability_file)?;
            let (default_uid, default_gid) = current_ids();
            let principal = Principal::new(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                groups,
            );
            let limits = weftcodec::Limits::default();
            let resolved = store.resolve_context(
                &target,
                capability.as_ref(),
                &principal,
                &limits,
                store.clock_now_ns(),
            )?;
            let state = store.state_at(resolved.resolution.snapshot_id)?;
            let results: Vec<_> = resolved
                .resolution
                .results
                .iter()
                .map(|reference| {
                    let record = state.objects.get(&reference.object_id);
                    let closure = record
                        .and_then(|record| object_causal_closure(record).ok().flatten())
                        .map(|closure| closure.closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>())
                        .unwrap_or_default();
                    json!({
                        "object_id": reference.object_id.to_hex(),
                        "version_id": reference.version_id.to_hex(),
                        "content_id": reference.content_id.to_hex(),
                        "size": record.map(|record| record.size).unwrap_or(0),
                        "kind": record.map(|record| record.kind.as_str()).unwrap_or("unknown"),
                        "path": state.path_for(reference.object_id).map(|path| String::from_utf8_lossy(&path).into_owned()),
                        "causal_closure": closure,
                    })
                })
                .collect();
            let evidence: Vec<_> = resolved
                .resolution
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
            if json {
                print_json(&json!({
                    "name": resolved.stored.name,
                    "uri": resolved.stored.uri,
                    "context_root_id": resolved.stored.context_root_id.to_hex(),
                    "selector_id": resolved.resolution.selector_id.to_hex(),
                    "snapshot_id": resolved.resolution.snapshot_id.to_hex(),
                    "matched_count": resolved.resolution.matched_count,
                    "returned": resolved.resolution.results.len(),
                    "truncated": resolved.truncated,
                    "closure": resolved.stored.root.causal.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                    "capability_hash": resolved.capability_hash.map(|hash| hash.to_hex()),
                    "budget": resolved.effective_budget.as_ref().map(|budget| budget.to_json()),
                    "work_units": resolved.work_units,
                    "results": results,
                    "evidence": evidence,
                }))
            } else {
                for result in &resolved.resolution.results {
                    println!("{}", result.object_id);
                }
                println!(
                    "context_root_id={} selector_id={} snapshot_id={} matched_count={} returned={} truncated={}",
                    resolved.stored.context_root_id,
                    resolved.resolution.selector_id,
                    resolved.resolution.snapshot_id,
                    resolved.resolution.matched_count,
                    resolved.resolution.results.len(),
                    resolved.truncated
                );
                Ok(())
            }
        }
        ContextCommand::Materialize {
            target,
            out,
            capability_file,
            uid,
            gid,
            groups,
            require_one,
        } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let capability = read_capability(&capability_file)?;
            let (default_uid, default_gid) = current_ids();
            let principal = Principal::new(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                groups,
            );
            let limits = weftcodec::Limits::default();
            if require_one {
                // Cardinality is checked before any filesystem work. A singular
                // materialize never writes a partial or arbitrary result.
                let resolved = store.resolve_context(
                    &target,
                    capability.as_ref(),
                    &principal,
                    &limits,
                    store.clock_now_ns(),
                )?;
                if resolved.resolution.matched_count != 1 {
                    let ids: Vec<String> = resolved
                        .resolution
                        .results
                        .iter()
                        .map(|result| result.object_id.to_hex())
                        .collect();
                    return Err(Error::ambiguous(format!(
                        "context matched {} authorized objects; require_one needs exactly 1 (object_ids: {})",
                        resolved.resolution.matched_count,
                        ids.join(",")
                    )));
                }
            }
            let report = store.materialize_context(
                &target,
                &out,
                capability.as_ref(),
                &principal,
                &limits,
                store.clock_now_ns(),
            )?;
            if json {
                print_json(&json!({
                    "out": report.out.display().to_string(),
                    "manifest": report.manifest_path.display().to_string(),
                    "files": report.files,
                    "bytes": report.bytes,
                    "context_root_id": report.context_root_id.to_hex(),
                    "snapshot_id": report.snapshot_id.to_hex(),
                    "selector_id": report.selector_id.to_hex(),
                    "closure": report.closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                }))
            } else {
                println!("out={}", report.out.display());
                println!("manifest={}", report.manifest_path.display());
                println!("files={}", report.files);
                println!("bytes={}", report.bytes);
                Ok(())
            }
        }
        ContextCommand::Uri { target } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let stored = store.load_context_root(&target)?;
            if json {
                print_json(&json!({
                    "uri": stored.uri,
                    "context_root_id": stored.context_root_id.to_hex(),
                }))
            } else {
                println!("{}", stored.uri);
                Ok(())
            }
        }
        ContextCommand::Delete { name } => {
            let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
            store.delete_context_root(&name)?;
            success(
                json,
                &format!("deleted context root {name}"),
                json!({"name": name, "generation": store.generation()}),
            )
        }
    }
}

fn find_object_by_identity(store: &Store, target: &str) -> Result<weftfs::ObjectRecord> {
    if let Ok(id) = Hash32::from_hex(target) {
        if let Ok(record) = store.object_meta(id) {
            return Ok(record);
        }
        for record in store.current_state().objects.values() {
            if record.version == id || record.content == id {
                return Ok(record.clone());
            }
        }
        return Err(Error::not_found("no object has this identity"));
    }
    let id = store.resolve_path(target)?;
    store.object_meta(id)
}

fn run_causal(
    store: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    command: CausalCommand,
) -> Result<()> {
    match command {
        CausalCommand::Show { target } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let record = find_object_by_identity(&store, &target)?;
            let closure = object_causal_closure(&record)?;
            let mode = identity_mode_of(&record);
            let causal_hash = match record.attrs.get(ATTR_CAUSAL_HASH) {
                Some(Value::Bytes(bytes)) => Some(Hash32::from_slice(bytes)?),
                _ => None,
            };
            if json {
                print_json(&json!({
                    "object_id": record.id.to_hex(),
                    "version_id": record.version.to_hex(),
                    "identity_mode": mode,
                    "causal_hash": causal_hash.map(|hash| hash.to_hex()),
                    "parents": closure.as_ref().map(|closure| closure.parents.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>()).unwrap_or_default(),
                    "closure": closure.as_ref().map(|closure| closure.closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>()).unwrap_or_default(),
                }))
            } else {
                println!("object_id={}", record.id);
                println!("version_id={}", record.version);
                println!("identity_mode={mode}");
                if let Some(hash) = causal_hash {
                    println!("causal_hash={hash}");
                }
                if let Some(closure) = closure {
                    for entry in &closure.parents {
                        println!("parent={entry}");
                    }
                    for entry in &closure.closure {
                        println!("closure={entry}");
                    }
                }
                Ok(())
            }
        }
    }
}

fn run_uri(store: &PathBuf, json: bool, policy: OpenPolicy, command: UriCommand) -> Result<()> {
    match command {
        UriCommand::Canonicalize { uri } => {
            let limits = weftcodec::Limits::default();
            let parsed = weftcodec::context::parse_context_uri(&uri, &limits)?;
            let canonical = parsed.canonical_uri(&limits)?;
            if json {
                let mut output = json!({"uri": canonical});
                if let Some(root) = parsed.to_context_root(&limits)? {
                    output["context_root_id"] = json!(root.id()?.to_hex());
                    output["form"] = json!("descriptor");
                } else {
                    output["form"] = json!("authoritative");
                }
                print_json(&output)
            } else {
                println!("{canonical}");
                Ok(())
            }
        }
        UriCommand::Show { target } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let stored = store.load_context_root(&target)?;
            if json {
                print_json(&json!({
                    "uri": stored.uri,
                    "context_root_id": stored.context_root_id.to_hex(),
                    "name": stored.name,
                }))
            } else {
                println!("{}", stored.uri);
                Ok(())
            }
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

// ---------------------------------------------------------------------------
// A2 / F3.3 — replay bundle export
// ---------------------------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String> {
    let output = ProcessCommand::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|err| Error::io(format!("cannot run sha256sum: {err}")))?;
    if !output.status.success() {
        return Err(Error::io(format!(
            "sha256sum failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .next()
        .map(|value| value.to_owned())
        .ok_or_else(|| Error::io("sha256sum produced no output"))
}

fn run_tool(mut command: ProcessCommand, what: &str) -> Result<()> {
    let output = command
        .output()
        .map_err(|err| Error::io(format!("cannot run {what}: {err}")))?;
    if !output.status.success() {
        return Err(Error::io(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(src)
        .map_err(|err| Error::io(format!("cannot stat {}: {err}", src.display())))?;
    if metadata.is_dir() {
        fs::create_dir_all(dst)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", dst.display())))?;
        for entry in fs::read_dir(src)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", src.display())))?
        {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
    }
    fs::copy(src, dst).map_err(|err| {
        Error::io(format!(
            "cannot copy {} to {}: {err}",
            src.display(),
            dst.display()
        ))
    })?;
    Ok(())
}

fn collect_files(base: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let path = base.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|err| Error::io(format!("cannot stat {}: {err}", path.display())))?;
    if metadata.is_dir() {
        for entry in fs::read_dir(&path)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?
        {
            let entry = entry?;
            collect_files(base, &relative.join(entry.file_name()), out)?;
        }
    } else if metadata.is_file() {
        out.push(relative.to_path_buf());
    }
    Ok(())
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    let mut file = fs::File::create(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_export_bundle(
    store_path: &PathBuf,
    out: &PathBuf,
    contexts: &[String],
    source_revisions: &[String],
    policy: OpenPolicy,
    json: bool,
) -> Result<()> {
    let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
    let limits = weftcodec::Limits::default();
    let principal = Principal::root();
    let now = store.clock_now_ns();

    let selected = if contexts.is_empty() {
        store.list_context_roots()?
    } else {
        let mut selected = Vec::with_capacity(contexts.len());
        for name in contexts {
            selected.push(store.load_context_root(name)?);
        }
        selected
    };

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
    }
    let stage = out.with_file_name(format!(
        ".{}.stage-{}",
        out.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("weft-receipt"),
        std::process::id()
    ));
    if stage.exists() {
        fs::remove_dir_all(&stage)
            .map_err(|err| Error::io(format!("cannot clear {}: {err}", stage.display())))?;
    }
    fs::create_dir_all(&stage)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", stage.display())))?;

    let result = (|| -> Result<serde_json::Value> {
        let bundle_store = stage.join("store");
        fs::create_dir_all(&bundle_store)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", bundle_store.display())))?;
        for name in ["FORMAT", "LOCK", "superblock"] {
            let source = store_path.join(name);
            if source.exists() {
                copy_tree(&source, &bundle_store.join(name))?;
            }
        }
        for name in ["commits", "objects", "manifests", "refs"] {
            let source = store_path.join(name);
            if source.exists() {
                copy_tree(&source, &bundle_store.join(name))?;
            }
        }

        // Materialize each root once, hash its exact bytes, then delete the
        // working copy. The bundle carries the store, not a second copy of the
        // data it already content-addresses.
        let work = stage.join(".materialize-work");
        fs::create_dir_all(&work)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", work.display())))?;
        let mut manifest_contexts = Vec::new();
        let mut materialized_checks = String::new();
        let mut context_rows = String::new();
        for stored in &selected {
            let resolved = store.resolve_context(&stored.name, None, &principal, &limits, now)?;
            let target = work.join(&stored.name);
            let report =
                store.materialize_context(&stored.name, &target, None, &principal, &limits, now)?;
            let materialization_text =
                fs::read_to_string(&report.manifest_path).map_err(|err| {
                    Error::io(format!(
                        "cannot read {}: {err}",
                        report.manifest_path.display()
                    ))
                })?;
            let materialization: serde_json::Value = serde_json::from_str(&materialization_text)
                .map_err(|err| Error::io(format!("bad materialization manifest: {err}")))?;
            let mut files = Vec::new();
            if let Some(items) = materialization["files"].as_array() {
                for item in items {
                    let kind = item["kind"].as_str().unwrap_or("");
                    let display = item["path"].as_str().unwrap_or("");
                    let relative = display.trim_start_matches('/');
                    let record = json!({
                        "path": relative,
                        "object_id": item["object_id"].clone(),
                        "version_id": item["version_id"].clone(),
                        "content_id": item["content_id"].clone(),
                        "content_hash": item["content_hash"].clone(),
                        "size": item["size"].clone(),
                        "kind": kind,
                    });
                    if kind != "dir" && kind != "symlink" && !relative.is_empty() {
                        let disk = target.join(relative);
                        if disk.is_file() {
                            let sha = sha256_file(&disk)?;
                            materialized_checks.push_str(&format!(
                                "{sha}  materialized/{}/{relative}\n",
                                stored.name
                            ));
                            let mut with_sha = record.clone();
                            with_sha["sha256"] = json!(sha);
                            files.push(with_sha);
                            continue;
                        }
                    }
                    files.push(record);
                }
            }
            manifest_contexts.push(json!({
                "name": stored.name,
                "uri": stored.uri,
                "context_root_id": stored.context_root_id.to_hex(),
                "snapshot_id": resolved.resolution.snapshot_id.to_hex(),
                "selector_id": resolved.resolution.selector_id.to_hex(),
                "resolver": stored.root.resolver,
                "ordering": stored.root.ordering,
                "matched_count": resolved.resolution.matched_count,
                "returned": resolved.resolution.results.len(),
                "truncated": resolved.truncated,
                "record_hex": stored.record_hex,
                "files": files,
            }));
            context_rows.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                stored.name,
                stored.context_root_id.to_hex(),
                resolved.resolution.snapshot_id.to_hex(),
                resolved.resolution.selector_id.to_hex()
            ));
        }

        // The context graph is the complete stored relation set; Context Root
        // relations are included verbatim with their canonical record hex.
        let relations: Vec<serde_json::Value> = store
            .current_state()
            .relations
            .values()
            .map(|relation| {
                let target = match &relation.target {
                    RelationTarget::Object(id) => json!({"object": id.to_hex()}),
                    RelationTarget::Value(value) => json!({"value": value_to_json(value)}),
                };
                json!({
                    "relation_id": relation.id.to_hex(),
                    "subject": relation.subject.to_hex(),
                    "predicate": relation.predicate,
                    "target": target,
                })
            })
            .collect();

        let mut revisions = BTreeMap::new();
        for (key, value) in [
            ("WeftFS", std::env::var("WEFT_REPO_WEFTFS").ok()),
            ("WeftFS-Go", std::env::var("WEFT_REPO_WEFTFS_GO").ok()),
            ("WeftCodec", std::env::var("WEFT_REPO_WEFTCODEC").ok()),
            ("WeftCodec-Go", std::env::var("WEFT_REPO_WEFTCODEC_GO").ok()),
        ] {
            if let Some(value) = value {
                revisions.insert(key.to_owned(), value);
            }
        }
        for item in source_revisions {
            let (name, revision) = item.split_once('=').ok_or_else(|| {
                Error::invalid_argument(format!("--source-revision `{item}` is not name=revision"))
            })?;
            revisions.insert(name.to_owned(), revision.to_owned());
        }

        let store_entries = {
            let mut files = Vec::new();
            collect_files(&bundle_store, Path::new(""), &mut files)?;
            let mut entries = Vec::new();
            for relative in files {
                let disk = bundle_store.join(&relative);
                let sha = sha256_file(&disk)?;
                let bytes = fs::metadata(&disk).map(|meta| meta.len()).unwrap_or(0);
                entries.push(json!({
                    "path": format!("store/{}", relative.display()),
                    "sha256": sha,
                    "bytes": bytes,
                }));
            }
            entries
        };

        let manifest = json!({
            "schema": "weftfs.replay-bundle/1",
            "bundle_format": "tar+zstd",
            "source_revisions": revisions,
            "court_command": "scripts/docker-court.sh fuse \"bash scripts/replay-bundle.sh --bundle bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out\"",
            "store": {
                "format": "weftfs-store 1",
                "generation": store.generation(),
                "head": store.head().to_hex(),
            },
            "contexts": manifest_contexts,
            "context_graph": relations,
            "store_entries": store_entries,
        });

        let manifest_text = serde_json::to_string_pretty(&manifest)
            .map_err(|err| Error::io(format!("cannot encode manifest.json: {err}")))?
            + "\n";
        write_text(&stage.join("manifest.json"), &manifest_text)?;
        write_text(&stage.join("contexts.tsv"), &context_rows)?;
        write_text(&stage.join("materialized.sha256"), &materialized_checks)?;
        write_text(
            &stage.join("replay.sh"),
            "#!/usr/bin/env bash\nset -Eeuo pipefail\nhere=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\ncd \"$here\"\nWEFTFS_BIN=\"${WEFTFS_BIN:-weftfs}\"\nsha256sum -c checks.sha256\n\"$WEFTFS_BIN\" --store store --json verify --full | grep -q '\"OK\": true'\nrm -rf materialized\nmkdir -p materialized\nwhile IFS=$'\\t' read -r name ctx snapshot selector; do\n  [ -n \"$name\" ] || continue\n  \"$WEFTFS_BIN\" --store store --json context materialize \"$name\" --out \"materialized/$name\" >/dev/null\ndone < contexts.tsv\nsha256sum -c materialized.sha256\necho \"replay: ok\"\n",
        )?;
        write_text(
            &stage.join("README.md"),
            &format!(
                "# WeftFS replay bundle\n\n\
                 Generated by `weftfs export-bundle` from WeftFS revision `{}`.\n\n\
                 This bundle contains the portable store (`store/`), the complete\n\
                 relation graph and Context Root records (`manifest.json`), the\n\
                 exact per-file hashes for every Context Root, and `replay.sh`.\n\n\
                 Reproduce it with the OOM-protected FUSE court:\n\n\
                 ```\n\
                 scripts/docker-court.sh fuse \\\"bash scripts/replay-bundle.sh --bundle bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out\\\"\n\
                 ```\n\n\
                 The Go frontend extracts the bundle, verifies every store file\n\
                 against `manifest.json`, runs `verify --full`, materializes every\n\
                 Context Root, and compares every output hash. No Rust binary runs\n\
                 during replay.\n",
                revisions.get("WeftFS").cloned().unwrap_or_else(|| "unknown".to_owned())
            ),
        )?;

        let mut checks = String::new();
        let mut checks_files = Vec::new();
        collect_files(&stage, Path::new("store"), &mut checks_files)?;
        for name in ["manifest.json", "contexts.tsv", "README.md", "replay.sh"] {
            let path = stage.join(name);
            if path.is_file() {
                checks_files.push(PathBuf::from(name));
            }
        }
        checks_files.sort();
        for relative in checks_files {
            let disk = stage.join(&relative);
            let sha = sha256_file(&disk)?;
            checks.push_str(&format!("{sha}  {}\n", relative.display()));
        }
        write_text(&stage.join("checks.sha256"), &checks)?;

        // Tar and zstd are the bundle format. Both tools must be present; a
        // missing tool is an error, never a silently uncompressed fallback.
        let tar_path = out.with_file_name(format!(
            ".{}.tar",
            out.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("weft-receipt")
        ));
        let mut tar = ProcessCommand::new("tar");
        tar.arg("-C").arg(&stage).arg("-cf").arg(&tar_path).arg(".");
        run_tool(tar, "tar")?;
        let mut zstd = ProcessCommand::new("zstd");
        zstd.arg("-q")
            .arg("-f")
            .arg("-3")
            .arg(&tar_path)
            .arg("-o")
            .arg(out);
        run_tool(zstd, "zstd")?;
        let _ = fs::remove_file(&tar_path);

        let bundle_sha = sha256_file(out)?;
        let bundle_bytes = fs::metadata(out).map(|meta| meta.len()).unwrap_or(0);
        Ok(json!({
            "bundle": out.display().to_string(),
            "sha256": bundle_sha,
            "bytes": bundle_bytes,
            "generation": store.generation(),
            "head": store.head().to_hex(),
            "contexts": selected.iter().map(|stored| stored.name.clone()).collect::<Vec<_>>(),
        }))
    })();

    let _ = fs::remove_dir_all(&stage);
    let summary = result?;
    if json {
        print_json(&summary)
    } else {
        println!("bundle={}", summary["bundle"].as_str().unwrap_or_default());
        println!("sha256={}", summary["sha256"].as_str().unwrap_or_default());
        println!("bytes={}", summary["bytes"]);
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
    attr_u64_lists: &[String],
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
    for item in attr_u64_lists {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            Error::invalid_argument(format!("attribute `{item}` is not key=list"))
        })?;
        let mut items = Vec::new();
        if !value.is_empty() {
            for part in value.split(',') {
                let number = part.trim().parse::<u64>().map_err(|_| {
                    Error::invalid_argument(format!("bad u64 list item `{part}` for `{key}`"))
                })?;
                items.push(Value::U64(number));
            }
        }
        out.insert(key.to_owned(), Value::Array(items));
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
