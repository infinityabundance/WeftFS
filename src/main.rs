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
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;
use weftcodec::capability::{
    public_key_for_seed, sign_envelope, verify_envelope, CapabilityEnvelope,
};
use weftcodec::context::{Budget, ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value;
use weftcodec::{Region, SemanticPolicy};

use weftfs::auth::Principal;
use weftfs::azure::AzureConfig;
use weftfs::backend::BackendKind;
use weftfs::checkpoint::{
    self, CheckpointRestoreOptions, CheckpointWriteOptions, RestoreTopology, ShardStrategy,
};
use weftfs::conformance::build_fixture_store;
use weftfs::context::{parse_capability_text, CapabilityContext};
use weftfs::error::{Error, Result};
use weftfs::evaluate::lookup_snapshot;
use weftfs::format::ContentIdentity;
use weftfs::gcs::GcsConfig;
use weftfs::identity::Hash32;
use weftfs::provenance::{
    choose_recovery, Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal,
    ProvenanceRecord, PROVENANCE_SCHEMA,
};
use weftfs::quota::{parse_quota_file, QuotaPrincipal};
use weftfs::s3::S3Config;
use weftfs::state::{
    object_causal_closure, ObjectKind, RelationTarget, ATTR_CAUSAL_HASH, ATTR_IDENTITY_MODE,
};
use weftfs::store::{
    OpenMode, Options, RepresentationMode, SetAttr, Store, DEFAULT_CODEC_MIN_BYTES,
};
use weftfs::tensor_store::{
    AccessPattern, TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec,
};
use weftfs::threefs::ThreeFsConfig;
use weftfs::training_ready::{verify_training_ready_output, TrainingReadyOptions};
use weftfs::wcb_util as wu;
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
    /// v1 manifests), `codec-auto` (experimental, Rust-only), or `overlay`
    /// (immutable `weft.manifest.v4` write layers; requires the Merkle content
    /// identity). Also accepted by `mount`; read-only commands accept and
    /// ignore it.
    #[arg(
        long,
        global = true,
        value_name = "raw|codec-auto|overlay",
        default_value = "raw"
    )]
    representation: String,
    /// Content identity for newly written files: `whole-file` (frozen v1
    /// `ContentId`) or `merkle` (`weft.manifest.v3`, range writes cost O(changed
    /// chunks)). Defaults to `whole-file`, except for `mount`, which defaults
    /// to `merkle` for files created through the mount. Read-only commands
    /// ignore it.
    #[arg(
        long = "content-identity",
        global = true,
        value_name = "whole-file|merkle"
    )]
    content_identity: Option<String>,
    /// Physical storage backend: `local` (normative POSIX layout), `object`
    /// (immutable object namespace published with link(2)), `s3` (SigV4
    /// S3-compatible endpoint; see `--s3-*`), `gcs` (GCS JSON API with
    /// generation preconditions; see `--gcs-*`), or `azure` (Azure Blob REST
    /// with ETag preconditions; see `--azure-*`).
    #[arg(
        long,
        global = true,
        value_name = "local|object|s3|gcs|azure",
        default_value = "local"
    )]
    backend: String,
    /// Optional blob plane for a split store: immutable chunks, manifests, and
    /// indexes live here while commit/control state stays on `--commit-backend`.
    /// Omitted means "same as --backend", i.e. the historical single backend.
    #[arg(
        long = "blob-backend",
        global = true,
        value_name = "local|object|s3|gcs|azure"
    )]
    blob_backend: Option<String>,
    /// Optional commit plane for a split store: `FORMAT`, `LOCK`, the
    /// superblock, commit records, and refs live here. Omitted means "same as
    /// --backend".
    #[arg(
        long = "commit-backend",
        global = true,
        value_name = "local|object|s3|gcs|azure"
    )]
    commit_backend: Option<String>,
    /// Optional local directory for the blob plane of a split store, when both
    /// planes are POSIX-shaped. Ignored by a remote blob plane, which is
    /// addressed by its bucket instead.
    #[arg(long = "blob-root", global = true, value_name = "DIR")]
    blob_root: Option<PathBuf>,
    /// 3FS transport, selected with `--backend 3fs`: `posix` uses ordinary
    /// positional I/O through the 3FS FUSE client; `usrbio` moves the immutable
    /// data plane onto the native hf3fs rings. Control state is POSIX in both.
    #[arg(
        long = "threefs-mode",
        global = true,
        value_name = "posix|usrbio",
        default_value = "posix"
    )]
    threefs_mode: String,
    /// 3FS mount point handed to the USRBIO rings (default: the store root).
    #[arg(long = "threefs-mount", global = true, value_name = "DIR")]
    threefs_mount: Option<PathBuf>,
    /// Explicit `libhf3fs_api_shared.so`; defaults to `$WEFT_3FS_LIB`, then the
    /// soname. `usrbio` mode refuses to open when it cannot be loaded.
    #[arg(long = "threefs-lib", global = true, value_name = "FILE")]
    threefs_lib: Option<PathBuf>,
    /// USRBIO ring entries (1..=4096, default 256).
    #[arg(long = "threefs-entries", global = true, value_name = "N")]
    threefs_entries: Option<usize>,
    /// USRBIO shared data-region bytes (default 8 MiB).
    #[arg(long = "threefs-io-bytes", global = true, value_name = "N")]
    threefs_io_bytes: Option<usize>,
    /// Refuse `usrbio` unless `hf3fs_is_hf3fs` reports the mount really is 3FS.
    #[arg(long = "threefs-require-mount", global = true)]
    threefs_require_mount: bool,
    /// S3-compatible endpoint, e.g. `http://127.0.0.1:9000`.
    #[arg(long = "s3-endpoint", global = true, value_name = "URL")]
    s3_endpoint: Option<String>,
    /// S3 bucket that holds the store.
    #[arg(long = "s3-bucket", global = true, value_name = "BUCKET")]
    s3_bucket: Option<String>,
    /// S3 region used for SigV4 scoping.
    #[arg(long = "s3-region", global = true, value_name = "REGION")]
    s3_region: Option<String>,
    /// S3 access key id (fallback: AWS_ACCESS_KEY_ID).
    #[arg(long = "s3-access-key", global = true, value_name = "KEY")]
    s3_access_key: Option<String>,
    /// S3 secret access key (fallback: AWS_SECRET_ACCESS_KEY).
    #[arg(long = "s3-secret-key", global = true, value_name = "SECRET")]
    s3_secret_key: Option<String>,
    /// Optional S3 session token (fallback: AWS_SESSION_TOKEN).
    #[arg(long = "s3-session-token", global = true, value_name = "TOKEN")]
    s3_session_token: Option<String>,
    /// GCS JSON API endpoint (default `https://storage.googleapis.com`).
    #[arg(long = "gcs-endpoint", global = true, value_name = "URL")]
    gcs_endpoint: Option<String>,
    /// GCS bucket that holds the store.
    #[arg(long = "gcs-bucket", global = true, value_name = "BUCKET")]
    gcs_bucket: Option<String>,
    /// GCS OAuth2 bearer token (fallback: WEFT_GCS_ACCESS_TOKEN or
    /// GOOGLE_OAUTH_ACCESS_TOKEN). Never logged or written to receipts.
    #[arg(long = "gcs-token", global = true, value_name = "TOKEN")]
    gcs_token: Option<String>,
    /// GCS service-account JSON key used with the RS256 JWT-bearer flow
    /// (fallback: WEFT_GCS_CREDENTIALS or GOOGLE_APPLICATION_CREDENTIALS). The
    /// key is never logged. Use --gcs-token instead only when a short-lived
    /// bearer token is already available.
    #[arg(long = "gcs-credentials", global = true, value_name = "FILE")]
    gcs_credentials: Option<String>,
    /// OAuth2 scope requested by the service-account assertion (default
    /// `https://www.googleapis.com/auth/devstorage.read_write`).
    #[arg(long = "gcs-scope", global = true, value_name = "SCOPE")]
    gcs_scope: Option<String>,
    /// GCS project used to create the bucket when it does not exist.
    #[arg(long = "gcs-project", global = true, value_name = "PROJECT")]
    gcs_project: Option<String>,
    /// Send no Authorization header (an emulator court). Refused when a token
    /// was also supplied.
    #[arg(long = "gcs-anonymous", global = true)]
    gcs_anonymous: bool,
    /// Azure Blob endpoint, e.g. `http://127.0.0.1:10000/devstoreaccount1` or
    /// `https://<account>.blob.core.windows.net`.
    #[arg(long = "azure-endpoint", global = true, value_name = "URL")]
    azure_endpoint: Option<String>,
    /// Azure Blob container that holds the store.
    #[arg(long = "azure-container", global = true, value_name = "CONTAINER")]
    azure_container: Option<String>,
    /// Azure storage account name used as the SharedKey credential scope
    /// (fallback: WEFT_AZURE_ACCOUNT or AZURE_STORAGE_ACCOUNT).
    #[arg(long = "azure-account", global = true, value_name = "ACCOUNT")]
    azure_account: Option<String>,
    /// Azure storage account key (fallback: WEFT_AZURE_KEY or
    /// AZURE_STORAGE_KEY). Never logged or written to receipts.
    #[arg(long = "azure-key", global = true, value_name = "KEY")]
    azure_key: Option<String>,
    /// Azure SAS query string without the leading `?` (fallback:
    /// WEFT_AZURE_SAS or AZURE_STORAGE_SAS_TOKEN). Never logged.
    #[arg(long = "azure-sas", global = true, value_name = "QUERY")]
    azure_sas: Option<String>,
    /// Send no Azure credential (an emulator court). Refused when an account
    /// key or SAS token was also supplied.
    #[arg(long = "azure-anonymous", global = true)]
    azure_anonymous: bool,
    /// Group small immutable blobs into physical pack objects (subphase R11).
    /// Members are recorded in a compare-and-swap catalog and read with one
    /// ranged request per member. Refused when the blob plane cannot read a
    /// byte range.
    #[arg(long, global = true)]
    pack: bool,
    /// Pack target size in bytes when `--pack` is set (default 16 MiB).
    #[arg(long = "pack-target-bytes", global = true, value_name = "BYTES")]
    pack_target_bytes: Option<u64>,
    /// Largest blob that is packed when `--pack` is set (default 64 KiB).
    #[arg(long = "pack-member-threshold", global = true, value_name = "BYTES")]
    pack_member_threshold: Option<u64>,
    /// Upper bound on the encoded pack catalog when `--pack` is set (default
    /// 8 MiB). When it is reached, new blobs are published flat.
    #[arg(long = "pack-catalog-bytes", global = true, value_name = "BYTES")]
    pack_catalog_bytes: Option<u64>,
    /// Number of catalog segments when `--pack` is set (power of two, default
    /// 256).
    #[arg(long = "pack-segments", global = true, value_name = "N")]
    pack_segments: Option<u32>,
    /// `codec-auto` leaves files smaller than this on the raw path.
    #[arg(
        long = "codec-min-bytes",
        global = true,
        value_name = "N",
        allow_hyphen_values = true
    )]
    codec_min_bytes: Option<String>,
    /// Write budget, repeatable: `uid:GID:BYTES:OBJECTS` or
    /// `tenant:NAME:BYTES:OBJECTS`. Supplying any budget on a writable open
    /// installs (or replaces) the store's policy; omitting it enforces the
    /// persisted policy unchanged.
    #[arg(long = "quota", global = true, value_name = "SPEC")]
    quotas: Vec<String>,
    /// Read write budgets from a file (one `--quota` directive per line).
    #[arg(long = "quota-file", global = true, value_name = "FILE")]
    quota_file: Option<PathBuf>,
    /// Coalesce identical concurrent reconstructions (same object, content
    /// version, and region) into one backend read and one decode. Reads only;
    /// off by default. Applies to `cat`, `mount`, and library callers that use
    /// an authorized read.
    #[arg(long = "single-flight", global = true)]
    single_flight: bool,
    /// Maximum number of reconstructions in flight when `--single-flight` is on.
    #[arg(
        long = "single-flight-max-entries",
        global = true,
        value_name = "N",
        default_value_t = 64
    )]
    single_flight_max_entries: usize,
    /// Maximum decoded bytes reserved by in-flight reconstructions.
    #[arg(
        long = "single-flight-max-bytes",
        global = true,
        value_name = "BYTES",
        default_value_t = 268435456
    )]
    single_flight_max_bytes: u64,
    /// Bound on the verified decoded-object cache, in bytes. `0` (the default)
    /// disables it. Reads only; never part of any on-disk format.
    #[arg(
        long = "cache-bytes",
        global = true,
        value_name = "BYTES",
        default_value_t = 0
    )]
    cache_bytes: u64,
    /// Maximum number of cached decoded objects (bookkeeping bound).
    #[arg(
        long = "cache-entries",
        global = true,
        value_name = "N",
        default_value_t = 65536
    )]
    cache_entries: usize,
    /// Decode-cost table used by the tensor planner and `rebase`:
    /// `measured` (host-calibrated in the bench court) or `declared` (the
    /// historical hand-written linear estimate).
    #[arg(
        long = "planner-cost-model",
        global = true,
        value_name = "measured|declared",
        default_value = "measured"
    )]
    planner_cost_model: String,
    /// Host-local decode-cost calibration (written by `calibrate`). Requires
    /// `--planner-cost-model measured`; the file's rows then replace the baked
    /// table entirely. A malformed or incomplete file fails before planning.
    #[arg(long = "calibration", global = true, value_name = "FILE")]
    calibration: Option<PathBuf>,
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
        /// Attribute the new object to a tenant write budget. Stored as the
        /// reserved `weft.tenant` object attribute.
        #[arg(long)]
        tenant: Option<String>,
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
        /// Bytes charged per estimated nanosecond of encode time. Defaults to
        /// `--lambda-ns`, so one lambda prices both unless overridden.
        #[arg(long = "lambda-write-ns")]
        lambda_write_ns: Option<u64>,
        /// Allow experimental representations (mode 18) to be auto-selected.
        #[arg(long = "allow-experimental-modes")]
        allow_experimental_modes: bool,
        /// Do not offer the ZSTD level-19 candidate.
        #[arg(long = "no-zstd19")]
        no_zstd19: bool,
        /// CPU budget for the level-19 candidate (estimated encode ns).
        #[arg(long = "zstd19-cpu-budget-ns", default_value_t = 20_000_000_000)]
        zstd19_cpu_budget_ns: u64,
        /// Do not offer the whole-base ZSTD patch candidate.
        #[arg(long = "no-zstd-patch")]
        no_zstd_patch: bool,
        /// ZSTD level for the patch candidate (default 12, matching the
        /// measured `--patch-from` baseline).
        #[arg(long = "zstd-patch-level", default_value_t = 12)]
        zstd_patch_level: i32,
        /// `auto` runs the planner; any other value requires that strategy.
        #[arg(long, default_value = "auto")]
        strategy: String,
        /// Semantic policy: none | mantissa_truncate:N | block_quantize:B:L.
        #[arg(long = "semantic-policy")]
        semantic_policy: Option<String>,
        /// Explicit semantic candidate ObjectIds (comma-separated hex).
        #[arg(long)]
        candidates: Option<String>,
        /// Tile extents for a mode-15 `TENSOR_TILED` write (comma-separated).
        /// Also selects the tiled strategy explicitly.
        #[arg(long = "tile-shape")]
        tile_shape: Option<String>,
        /// Derive a dividing mode-15 tile shape from this canonical tile-byte
        /// budget and offer it to the planner.
        #[arg(long = "auto-tile-bytes")]
        auto_tile_bytes: Option<u64>,
        /// Access pattern: whole | region:BYTES:READS | mixed:WHOLE:BYTES:READS.
        #[arg(long = "access-pattern", default_value = "whole")]
        access_pattern: String,
    },
    /// Canonicalise an overlay-represented file into one chunk table. The
    /// logical content root must be unchanged.
    Compact { path: String },
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
        /// Tile decode workers for a mode-15 tensor read (0 and 1 mean one).
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Verified capability envelope blob; the read is denied unless it
        /// authorizes the object in addition to POSIX permissions.
        #[arg(long)]
        envelope: Option<PathBuf>,
        /// Revocation ids for `--envelope`.
        #[arg(long)]
        revocations: Option<PathBuf>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
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
        /// Bytes charged per estimated nanosecond of encode time. Defaults to
        /// `--lambda-ns`.
        #[arg(long = "lambda-write-ns")]
        lambda_write_ns: Option<u64>,
        /// Allow experimental representations (mode 18) to be auto-selected.
        #[arg(long = "allow-experimental-modes")]
        allow_experimental_modes: bool,
        /// Do not offer the ZSTD level-19 candidate.
        #[arg(long = "no-zstd19")]
        no_zstd19: bool,
        /// CPU budget for the level-19 candidate (estimated encode ns).
        #[arg(long = "zstd19-cpu-budget-ns", default_value_t = 20_000_000_000)]
        zstd19_cpu_budget_ns: u64,
        /// Do not offer the whole-base ZSTD patch candidate.
        #[arg(long = "no-zstd-patch")]
        no_zstd_patch: bool,
        /// ZSTD level for the patch candidate (default 12, matching the
        /// measured `--patch-from` baseline).
        #[arg(long = "zstd-patch-level", default_value_t = 12)]
        zstd_patch_level: i32,
        #[arg(long)]
        causal: Option<String>,
        #[arg(long = "semantic-policy")]
        semantic_policy: Option<String>,
        #[arg(long)]
        candidates: Option<String>,
        /// Tile extents for the mode-15 `TENSOR_TILED` candidate (comma-separated).
        #[arg(long = "tile-shape")]
        tile_shape: Option<String>,
        /// Derive a dividing mode-15 tile shape from this canonical tile-byte
        /// budget and offer it to the planner.
        #[arg(long = "auto-tile-bytes")]
        auto_tile_bytes: Option<u64>,
        /// Access pattern: whole | region:BYTES:READS | mixed:WHOLE:BYTES:READS.
        #[arg(long = "access-pattern", default_value = "whole")]
        access_pattern: String,
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
    /// Force a checkpoint commit, or write/restore/verify a tensor checkpoint.
    Checkpoint {
        #[command(subcommand)]
        command: Option<CheckpointCommand>,
    },
    /// Choose a parent graph for an ordered checkpoint trajectory.
    ///
    /// Prices every earlier-version parent with the shipped tensor planner,
    /// then runs the dependency-graph optimizer. `--apply` writes the chosen
    /// graph into a new store through the existing `causal_checkpoint` path.
    PlanParents {
        /// `NAME=FILE` in trajectory order (repeatable). The name is the
        /// checkpoint directory name used by `--apply`.
        #[arg(long = "versions", value_name = "NAME=FILE", required = true)]
        versions: Vec<String>,
        /// Maximum chain length (the frozen reader bound is 8).
        #[arg(long = "max-depth", default_value_t = 8)]
        max_depth: u32,
        /// Bytes charged per estimated nanosecond of decode time (0 = bytes only).
        #[arg(long = "lambda-ns", default_value_t = 0)]
        lambda_ns: u64,
        /// Expected reads per version; each read pays its whole chain.
        #[arg(long = "reads-per-version", default_value_t = 1)]
        reads_per_version: u64,
        /// Worst-case cold-restore bound for any version, in milliseconds.
        #[arg(long = "sla-ms", value_name = "MS")]
        sla_ms: Option<u64>,
        /// Whole-checkpoint restore-overhead model: `measured` (the S13b
        /// calibration, the default) or `none` (diagnostic only; an SLA with
        /// `none` is refused because the prediction would be optimistic).
        #[arg(long = "restore-overhead", default_value = "measured")]
        restore_overhead: String,
        /// Maximum predicted total edge bytes.
        #[arg(long = "storage-budget", value_name = "BYTES")]
        storage_budget: Option<u64>,
        /// `dp` (chain/restart, the default), `brute-force` (exact oracle,
        /// small graphs), or `greedy` (one pass over every earlier parent).
        #[arg(long = "solver", default_value = "dp")]
        solver: String,
        /// Write the chosen graph into a new store (must be `--store` or `--out`).
        #[arg(long)]
        apply: bool,
        /// Target store for `--apply` (defaults to `--store`).
        #[arg(long = "out", value_name = "DIR")]
        out: Option<PathBuf>,
        /// Scratch store used for independent probes (default:
        /// `<target>.plan-parents-scratch`). Must not exist.
        #[arg(long = "scratch", value_name = "DIR")]
        scratch: Option<PathBuf>,
        /// Keep the scratch probe store (diagnostics only).
        #[arg(long = "keep-scratch")]
        keep_scratch: bool,
        /// Shards per tensor (only 1 is supported by the graph planner).
        #[arg(long, default_value_t = 1)]
        shards: u32,
        /// Split axis.
        #[arg(long, default_value_t = 0)]
        axis: u32,
        /// Per-shard physical strategy (only `planned` can choose deltas).
        #[arg(long = "shard-strategy", default_value = "planned")]
        shard_strategy: String,
        /// Do not run the mode-14 semantic-delta candidate search.
        #[arg(long = "no-semantic")]
        no_semantic: bool,
        /// Per-tensor streaming ingest bound in bytes.
        #[arg(
            long = "max-ingest-bytes",
            default_value_t = 1024 * 1024 * 1024,
            value_name = "BYTES"
        )]
        max_ingest_bytes: u64,
    },
    /// Serve checkpoint reads/writes over one persistent, bounded binary
    /// protocol connection (no per-call CLI start, no host staging file).
    CheckpointService {
        /// Connected unix socket descriptor inherited from the caller.
        #[arg(long, value_name = "N")]
        fd: Option<i32>,
        /// Alternatively bind this unix socket path and accept one client.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Largest accepted control frame, in bytes (bounded).
        #[arg(long = "max-frame-bytes", default_value_t = 1048576)]
        max_frame_bytes: usize,
        /// Largest single restored tensor or staged container, in bytes.
        #[arg(long = "max-tensor-bytes", default_value_t = 4294967296u64)]
        max_tensor_bytes: u64,
    },
    /// Context Root persistence, resolution, and materialization.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Materialize a Context Root as a training-ready, fully verified set.
    Materialize {
        /// Context Root name or `weft://` URI.
        #[arg(long)]
        root: String,
        /// Require every training-ready precondition (this is the only mode).
        #[arg(long = "training-ready")]
        training_ready: bool,
        /// Output directory; must not exist.
        #[arg(long)]
        out: PathBuf,
        #[arg(long = "max-bytes")]
        max_bytes: Option<u64>,
        #[arg(long = "max-objects")]
        max_objects: Option<u64>,
        #[arg(long = "max-decode-ms")]
        max_decode_ms: Option<u64>,
        /// Capability JSON or canonical-hex file.
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        /// Verified authenticated capability envelope blob.
        #[arg(long)]
        envelope: Option<PathBuf>,
        /// Revocation ids for `--envelope`.
        #[arg(long)]
        revocations: Option<PathBuf>,
        /// Re-verify every tensor object's logical root against decoded bytes.
        #[arg(long = "require-tensors")]
        require_tensors: bool,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "group")]
        groups: Vec<u64>,
    },
    /// Check training-ready preconditions, or verify an output directory.
    TrainingReady {
        /// Context Root name or `weft://` URI (required with `--check`).
        #[arg(long)]
        root: Option<String>,
        /// Run every check and print a structured report; write nothing.
        #[arg(long)]
        check: bool,
        /// Verify an existing materialized directory against its manifest.
        #[arg(long = "verify-out", value_name = "DIR")]
        verify_out: Option<PathBuf>,
        #[arg(long = "max-bytes")]
        max_bytes: Option<u64>,
        #[arg(long = "max-objects")]
        max_objects: Option<u64>,
        #[arg(long = "max-decode-ms")]
        max_decode_ms: Option<u64>,
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        #[arg(long)]
        envelope: Option<PathBuf>,
        #[arg(long)]
        revocations: Option<PathBuf>,
        #[arg(long = "require-tensors")]
        require_tensors: bool,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
        #[arg(long = "group")]
        groups: Vec<u64>,
    },
    /// Authenticated capability envelope operations (`WEFT-CAPABILITIES.md` §9).
    Capability {
        #[command(subcommand)]
        command: CapabilityCommand,
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
    /// Provenance receipts (record, show, transitive closure).
    Provenance {
        #[command(subcommand)]
        command: ProvenanceCommand,
    },
    /// Write budgets: show the caller's own usage, and admin operations.
    Quota {
        #[command(subcommand)]
        command: QuotaCommand,
    },
    /// Union-merge two snapshot refs (or commits) into the live head.
    Merge {
        /// Left branch: snapshot name or commit hex.
        left: String,
        /// Right branch: snapshot name or commit hex.
        right: String,
    },
    /// Plan recovery of an object from retained bytes, a representation chain,
    /// or recomputation from verified inputs.
    Recover {
        target: String,
        /// Print the chosen strategy and its cost components (required in v1).
        #[arg(long)]
        plan: bool,
        /// Declared fetch cost (cost units; nanoseconds by convention).
        #[arg(long = "fetch-cost", default_value_t = 0)]
        fetch_cost: u64,
        /// Declared decode cost.
        #[arg(long = "decode-cost", default_value_t = 0)]
        decode_cost: u64,
        /// Declared recompute cost.
        #[arg(long = "recompute-cost", default_value_t = 0)]
        recompute_cost: u64,
    },
    /// Plan or apply lineage-aware rebasing of hot tensor objects.
    Rebase {
        /// Store path or 64-hex ObjectId to scan (inclusive).
        #[arg(long)]
        root: String,
        /// Rebase when `reads * estimated_decode_ns` exceeds this value.
        #[arg(long = "threshold", default_value_t = 1_000_000_000)]
        threshold_ns: u64,
        /// Show the decision without writing anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Force a standalone representation: raw, zstd, float-tensor, ...
        #[arg(long)]
        strategy: Option<String>,
        /// Bytes charged per estimated nanosecond of decode time.
        #[arg(long = "lambda-ns", default_value_t = 0)]
        lambda_ns: u64,
        /// Bound on the number of objects examined.
        #[arg(long = "max-objects", default_value_t = 100_000)]
        max_objects: usize,
    },
    /// Background transcode: rewrite fast-written tensors to their planned
    /// representation in a bounded, exclusive pass.
    Transcode {
        /// Store path or 64-hex ObjectId to scan (inclusive).
        #[arg(long, default_value = "/")]
        root: String,
        /// Price the plan and report it without writing anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Maximum live objects examined.
        #[arg(long = "max-objects", default_value_t = 100_000)]
        max_objects: usize,
        /// Maximum logical bytes read by the pass.
        #[arg(long = "max-bytes", default_value_t = u64::MAX)]
        max_bytes: u64,
        /// Maximum wall time in milliseconds (0 disables the wall bound).
        #[arg(long = "max-millis", default_value_t = 0)]
        max_millis: u64,
        /// Minimum percentage of the current physical bytes a rewrite must save.
        #[arg(long = "min-saved-percent", default_value_t = 1)]
        min_saved_percent: u8,
        /// Force a standalone planned strategy: raw, zstd, float-tensor, ...
        #[arg(long)]
        strategy: Option<String>,
        /// Read the republished version back and compare the logical bytes.
        #[arg(long = "no-verify-after")]
        no_verify_after: bool,
        /// Test hook: abort the process after N durable publications.
        #[arg(long = "crash-after", value_name = "N")]
        crash_after: Option<usize>,
        /// Test hook: abort the process just before publishing candidate N.
        #[arg(long = "crash-before", value_name = "N")]
        crash_before: Option<usize>,
    },
    /// Measure and record this host's decode cost table, and report its
    /// divergence from the baked table. Reuses the shipped measurement code.
    Calibrate {
        /// Write the calibration document here (default: decode-calibration.json).
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Load an existing document instead of measuring, and report its
        /// divergence from the baked table.
        #[arg(long = "from", value_name = "FILE")]
        from: Option<PathBuf>,
        /// Install the document into `<store>/calibrations/<id>.json`.
        #[arg(long)]
        install: bool,
        /// Timed repetitions per representation and size (at least 5).
        #[arg(long, default_value_t = 7)]
        reps: usize,
        /// Large payload size in MiB (at least 8).
        #[arg(long = "large-mib", default_value_t = 16)]
        large_mib: u64,
        /// Small payload size in KiB (at least 64).
        #[arg(long = "small-kib", default_value_t = 128)]
        small_kib: u64,
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
        /// Verify every tensor file through the load path on open and write a
        /// `weft.load.receipt.v1` receipt per file.
        #[arg(long = "verify-load")]
        verify_load: bool,
        /// Directory for the per-file load receipts (requires --verify-load).
        #[arg(long = "load-receipt-dir", value_name = "DIR")]
        load_receipt_dir: Option<PathBuf>,
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
        /// Buffer each handle's writes and commit them at flush/fsync/release
        /// instead of committing every write. Off by default.
        #[arg(long = "write-back")]
        write_back: bool,
        /// Batch independent mutations arriving within a 10 ms window into one
        /// durable commit. Off by default.
        #[arg(long = "group-commit")]
        group_commit: bool,
    },
    /// Serve several read-only mounts from one process over one shared store.
    /// The spec is a strict JSON document; see docs/shared-mounts.md.
    #[cfg(feature = "fuse")]
    MountMany {
        /// JSON spec listing the read-only views.
        #[arg(long, value_name = "FILE")]
        spec: PathBuf,
        /// Allow users other than the mounting user.
        #[arg(long = "allow-other")]
        allow_other: bool,
        /// Verify every tensor file through the load path on open and write a
        /// `weft.load.receipt.v1` receipt per file.
        #[arg(long = "verify-load")]
        verify_load: bool,
        /// Directory for the per-file load receipts (requires --verify-load).
        #[arg(long = "load-receipt-dir", value_name = "DIR")]
        load_receipt_dir: Option<PathBuf>,
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
    /// KV prefix cache: canonical prefix records, chain dedup, capability-
    /// enforced reads, logical GC, and the reuse-vs-recompute decision
    /// (`docs/KV-PREFIX.md`).
    Kv {
        #[command(subcommand)]
        command: KvCommand,
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
    /// Branch an existing snapshot (or commit) under a new name. History is not
    /// rewritten.
    Branch { source: String, name: String },
    /// List snapshots.
    List,
}

#[derive(Subcommand)]
enum QuotaCommand {
    /// Show the caller's own usage and configured budget only. Usage of other
    /// principals is never disclosed.
    Show {
        #[arg(long, default_value_t = 0)]
        uid: u64,
        #[arg(long, default_value_t = 0)]
        gid: u64,
        /// Show the tenant budget instead of the POSIX principal.
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Admin: zero every usage counter (policy and content unchanged).
    Reset,
    /// Admin: recompute usage from the published commit chain and rewrite the
    /// record.
    Rebuild,
}

#[derive(Subcommand)]
enum CheckpointCommand {
    /// Ingest one safetensors container as a sharded tensor checkpoint.
    Write {
        /// Directory containing exactly one `*.safetensors` file.
        #[arg(long = "from-dir", value_name = "DIR")]
        from_dir: Option<PathBuf>,
        /// Explicit source container (alternative to `--from-dir`).
        #[arg(long = "file", value_name = "FILE")]
        file: Option<PathBuf>,
        /// Checkpoint name (store directory under `/checkpoints`).
        #[arg(long)]
        name: String,
        /// Number of shards per tensor.
        #[arg(long, default_value_t = 1)]
        shards: u32,
        /// Split axis.
        #[arg(long, default_value_t = 0)]
        axis: u32,
        /// Glob filter on tensor names (repeatable; default all tensors).
        #[arg(long = "tensor-filter", value_name = "GLOB")]
        tensor_filters: Vec<String>,
        /// Per-shard physical strategy: tensor-sharded, tensor-tiled, float-tiled, zstd, or raw.
        #[arg(long = "shard-strategy", default_value = "tensor-sharded")]
        shard_strategy: String,
        /// Tile extents for mode 15/17 (`A,B,...`). Implies tensor-tiled shards.
        #[arg(long = "tile-shape", value_name = "A,B,...")]
        tile_shape: Option<String>,
        /// Critical-path write policy: `fast` stores raw/fast-ZSTD shards and
        /// never runs a planned candidate encode; `planned` is the historical
        /// behaviour; `auto` is fast when --critical-path or --deadline-ms is set.
        #[arg(long = "write-policy", default_value = "planned")]
        write_policy: String,
        /// Representation for a fast write: `raw` or `zstd`.
        #[arg(long = "fast-codec", default_value = "zstd")]
        fast_codec: String,
        /// ZSTD level for `--fast-codec zstd` (1..=3).
        #[arg(long = "fast-zstd-level", default_value_t = 1)]
        fast_zstd_level: i32,
        /// Declare this save to be on the training critical path (`auto` policy).
        #[arg(long = "critical-path")]
        critical_path: bool,
        /// Declare a caller deadline in milliseconds (`auto` policy).
        #[arg(long = "deadline-ms", value_name = "MS")]
        deadline_ms: Option<u64>,
        /// Previous checkpoint in this store; matching tensors offer its shard
        /// versions as causal parents (enables weight-delta).
        #[arg(long = "causal-checkpoint", value_name = "NAME")]
        causal_checkpoint: Option<String>,
        /// Do not run the mode-14 semantic-delta candidate search.
        #[arg(long = "no-semantic")]
        no_semantic: bool,
        /// Allow experimental representations (mode 18) to be auto-selected by
        /// the per-shard planner.
        #[arg(long = "allow-experimental-modes")]
        allow_experimental_modes: bool,
        /// Prepare/commit split: number of blob-writing ranks (1..=64).
        /// Setting it (even to 1) uses the split path; omitting it is the
        /// sequential write.
        #[arg(long, value_name = "N")]
        ranks: Option<usize>,
        /// Bound on bytes reserved by in-flight prepare tasks.
        #[arg(long = "max-inflight-bytes", value_name = "BYTES")]
        max_inflight_bytes: Option<u64>,
        /// Cumulative decoded-byte budget for one rank.
        #[arg(long = "per-rank-bytes", value_name = "BYTES")]
        per_rank_bytes: Option<u64>,
        /// Wall-clock deadline for the prepare phase (split path only).
        #[arg(long = "prepare-deadline-ms", value_name = "MS")]
        prepare_deadline_ms: Option<u64>,
        /// Skip the committer's re-read/re-hash of prepared fragments.
        #[arg(long = "no-fragment-verify")]
        no_fragment_verify: bool,
        /// Deterministic 32-byte ObjectId seed (64 hex chars) for fixtures.
        #[arg(long = "checkpoint-seed", value_name = "HEX")]
        checkpoint_seed: Option<String>,
        /// Per-tensor streaming ingest bound in bytes. The writer hashes the
        /// source in fixed windows and reads one shard at a time; a tensor or
        /// shard larger than this is refused before anything is published.
        #[arg(
            long = "max-ingest-bytes",
            default_value_t = 1024 * 1024 * 1024,
            value_name = "BYTES"
        )]
        max_ingest_bytes: u64,
    },
    /// Restore a checkpoint, one tensor, or one tensor region.
    ///
    /// With `--import` the restored container is also published into the store
    /// namespace under `--out` (and becomes visible through a mount). Without
    /// it the store is only read and the bytes go to `--host-out`/`--spool`.
    Restore {
        #[arg(long)]
        name: String,
        /// Destination directory inside the store (requires --import).
        #[arg(long, value_name = "STORE_DIR")]
        out: Option<String>,
        /// Import the restored container into the store (opt-in).
        #[arg(long)]
        import: bool,
        /// Output file name inside the store (requires --import).
        #[arg(long = "out-name")]
        out_name: Option<String>,
        /// Restore only this tensor.
        #[arg(long)]
        tensor: Option<String>,
        /// Region `s0:e0,s1:e1,...` in full-tensor coordinates (needs --tensor).
        #[arg(long, value_name = "S:E,...")]
        range: Option<String>,
        /// Verify the tensor through this many shards (sharded topology).
        #[arg(long, value_name = "N")]
        topology: Option<u32>,
        /// Shard axis for `--topology` / `--layout sharded`.
        #[arg(long, default_value_t = 0)]
        axis: u32,
        /// Physical layout to verify through: row-major, column-major, blocked,
        /// sharded.
        #[arg(long, value_name = "LAYOUT")]
        layout: Option<String>,
        /// Block shape for `--layout blocked`.
        #[arg(long = "block-shape", value_name = "B0,B1,...")]
        block_shape: Option<String>,
        /// Also write the restored container to this host file.
        #[arg(long = "host-out", value_name = "FILE")]
        host_out: Option<PathBuf>,
        /// Host spool file used while assembling (default: a temp file).
        #[arg(long, value_name = "FILE")]
        spool: Option<PathBuf>,
        /// Write a canonical `weft.load.receipt.v1` record here after the bytes
        /// are produced and verified (requires a complete restore).
        #[arg(long = "load-receipt", value_name = "FILE")]
        load_receipt: Option<PathBuf>,
        /// Load verification depth for `--load-receipt`: `lazy` verifies the
        /// per-unit roots the format provides; `full` additionally re-reads
        /// every stored shard and re-checks its whole-content hash.
        #[arg(long = "verify-load", value_name = "lazy|full", default_value = "lazy")]
        verify_load: String,
        /// Replica label recorded in the load receipt.
        #[arg(long, default_value = "local")]
        replica: String,
        /// Restore this required set first (one tensor name per line; `#`
        /// comments allowed), then the rest of the checkpoint.
        #[arg(long = "first-needed", value_name = "FILE")]
        first_needed: Option<PathBuf>,
        /// Host directory holding verified required tensors across restarts.
        #[arg(long = "first-cache", value_name = "DIR")]
        first_cache: Option<PathBuf>,
        /// Test hook: abort after the required set is verified and cached.
        #[arg(long = "abort-after-first")]
        abort_after_first: bool,
    },
    /// List checkpoints.
    List,
    /// Show a checkpoint manifest.
    Show {
        #[arg(long)]
        name: String,
    },
    /// Decode every shard and verify every per-shard and per-tensor root.
    Verify {
        #[arg(long)]
        name: String,
    },
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
    /// Branch a Context Root: pin a new root to the source root's commit,
    /// optionally narrowing it with `--task`/`--model`, and record
    /// `derived_from` lineage.
    Branch {
        source: String,
        name: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Replace an existing same-name root.
        #[arg(long)]
        replace: bool,
    },
    /// Show the stored record and identity.
    Show { target: String },
    /// Resolve a Context Root to its ordered result set.
    Resolve {
        target: String,
        #[arg(long = "capability")]
        capability_file: Option<PathBuf>,
        /// Verified authenticated capability envelope blob.
        #[arg(long)]
        envelope: Option<PathBuf>,
        /// Revocation ids for `--envelope`.
        #[arg(long)]
        revocations: Option<PathBuf>,
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
        /// Verified authenticated capability envelope blob.
        #[arg(long)]
        envelope: Option<PathBuf>,
        /// Revocation ids for `--envelope`.
        #[arg(long)]
        revocations: Option<PathBuf>,
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
enum CapabilityCommand {
    /// Sign a canonical envelope JSON document and write the stored blob form.
    Sign {
        /// Envelope JSON (`weft.capability.envelope.v1`) or canonical hex.
        #[arg(long)]
        envelope: PathBuf,
        /// File containing a 32-byte Ed25519 seed (hex or raw).
        #[arg(long)]
        key: PathBuf,
        /// Output blob path.
        #[arg(long)]
        out: PathBuf,
        /// Also attach the envelope to its grantee in the store (needs --store).
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Verify a stored blob offline against a revocation set and clock.
    Verify {
        #[arg(long)]
        envelope: PathBuf,
        /// Revocation ids: hex lines, comma-separated, or a JSON array.
        #[arg(long)]
        revocations: Option<PathBuf>,
        /// Require this grantee (64 hex digits).
        #[arg(long)]
        grantee: Option<String>,
        /// Override the verification clock (UTC ns since the Unix epoch).
        #[arg(long = "now-ns")]
        now_ns: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// Print the Ed25519 public key derived from a seed file.
    Key {
        #[arg(long)]
        key: PathBuf,
    },
    /// List stored envelope attachments.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one stored envelope by id (verification is offline).
    Show {
        envelope_id: String,
        #[arg(long)]
        revocations: Option<PathBuf>,
        #[arg(long = "now-ns")]
        now_ns: Option<i64>,
        #[arg(long)]
        json: bool,
    },
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

#[derive(Subcommand)]
enum ProvenanceCommand {
    /// Record a provenance receipt and attach it to every output object.
    Record {
        /// JSON record file (`weft.provenance.v1` fields).
        #[arg(long)]
        record: PathBuf,
        /// Output object path or identity (repeat for each output).
        #[arg(long = "output", required = true)]
        outputs: Vec<String>,
    },
    /// Show provenance records attached to an object.
    Show { target: String },
    /// Walk the transitive provenance closure.
    Closure {
        target: String,
        #[arg(long, default_value_t = 8)]
        depth: u32,
    },
}

#[derive(Subcommand)]
enum KvCommand {
    /// Store a KV prefix: mode-12 pages plus one canonical
    /// `weft.kv.prefix.v1` record, in one commit.
    Put {
        /// Prefix name (the lookup key under `/kv/prefixes`).
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "default")]
        tenant: String,
        /// Model identity string (exact match is required at lookup).
        #[arg(long)]
        model: String,
        /// Token length of the prefix.
        #[arg(long)]
        tokens: u64,
        /// Deterministic synthetic token sequence seed.
        #[arg(long = "token-seed", default_value_t = 1)]
        token_seed: u64,
        #[arg(long = "page-rows", default_value_t = 64)]
        page_rows: u64,
        #[arg(long, default_value_t = 1)]
        layers: u32,
        #[arg(long, default_value_t = 1)]
        heads: u32,
        #[arg(long = "head-dim", default_value_t = 64)]
        head_dim: u32,
        #[arg(long, default_value = "bf16")]
        dtype: String,
        /// Declared tokenizer identity string (hashed into the record).
        #[arg(long, default_value = "declared-tokenizer-v1")]
        tokenizer: String,
        /// Declared attention configuration string (default: derived from
        /// `--heads`, `--head-dim`, `--layers`).
        #[arg(long)]
        attention: Option<String>,
        /// Parent prefix name whose page objects are reused verbatim.
        #[arg(long)]
        parent: Option<String>,
        /// Mark the prefix pinned (it cannot then be replaced or deleted).
        #[arg(long)]
        pin: bool,
        /// Deterministic synthetic page-byte seed.
        #[arg(long = "payload-seed", default_value_t = 1)]
        payload_seed: u64,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
    },
    /// Read a prefix under an authenticated capability.
    Get {
        #[arg(long)]
        name: String,
        /// Verified capability envelope blob. Without one the read is denied.
        #[arg(long)]
        envelope: Option<PathBuf>,
        #[arg(long)]
        revocations: Option<PathBuf>,
        #[arg(long = "now-ns")]
        now_ns: Option<i64>,
        #[arg(long)]
        uid: Option<u64>,
        #[arg(long)]
        gid: Option<u64>,
    },
    /// List live prefix records (metadata only; not a data path).
    List {
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Delete a prefix record. A pinned prefix is refused; pages stay until GC.
    Delete {
        #[arg(long)]
        name: String,
    },
    /// Logical KV GC: remove pages no live prefix references.
    Gc {
        /// Actually delete; without it the candidate list is reported.
        #[arg(long)]
        apply: bool,
    },
    /// Price reuse vs recompute against a stored prefix, without reading pages.
    Plan {
        #[arg(long, default_value = "default")]
        tenant: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        tokens: u64,
        #[arg(long = "token-seed", default_value_t = 1)]
        token_seed: u64,
        #[arg(long = "page-rows", default_value_t = 64)]
        page_rows: u64,
        #[arg(long, default_value_t = 1)]
        layers: u32,
        #[arg(long, default_value_t = 1)]
        heads: u32,
        #[arg(long = "head-dim", default_value_t = 64)]
        head_dim: u32,
        #[arg(long, default_value = "bf16")]
        dtype: String,
        #[arg(long, default_value = "declared-tokenizer-v1")]
        tokenizer: String,
        #[arg(long)]
        attention: Option<String>,
        /// `weft.kv.prefill.v1` document (default: the baked declared model).
        #[arg(long)]
        calibration: Option<PathBuf>,
    },
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
#[derive(Clone, Debug)]
enum CliBackend {
    Local,
    Object,
    S3(S3Config),
    Gcs(GcsConfig),
    Azure(AzureConfig),
    /// 3FS namespace, `posix` or `usrbio`.
    ThreeFs(ThreeFsConfig),
    /// Blobs on one plane, commit/control state on another.
    Split {
        blobs: Box<CliBackend>,
        commits: Box<CliBackend>,
        blob_root: Option<PathBuf>,
    },
}

impl CliBackend {
    fn kind(&self) -> BackendKind {
        match self {
            CliBackend::Local => BackendKind::Local,
            CliBackend::Object => BackendKind::Object,
            CliBackend::S3(config) => BackendKind::S3 {
                config: config.clone(),
            },
            CliBackend::Gcs(config) => BackendKind::Gcs {
                config: config.clone(),
            },
            CliBackend::Azure(config) => BackendKind::Azure {
                config: config.clone(),
            },
            CliBackend::ThreeFs(config) => BackendKind::ThreeFs {
                config: config.clone(),
            },
            CliBackend::Split {
                blobs,
                commits,
                blob_root,
            } => BackendKind::Split {
                blobs: Box::new(blobs.kind()),
                commits: Box::new(commits.kind()),
                blob_root: blob_root.clone(),
            },
        }
    }

    fn is_remote(&self) -> bool {
        match self {
            CliBackend::S3(_) | CliBackend::Gcs(_) | CliBackend::Azure(_) => true,
            CliBackend::Split { commits, .. } => commits.is_remote(),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
struct OpenPolicy {
    fixed_timestamp_ns: Option<i64>,
    representation: RepresentationMode,
    content_identity: ContentIdentity,
    codec_min_bytes: u64,
    backend: CliBackend,
    /// When set, small immutable blobs are grouped into physical packs.
    pack: Option<weftfs::packs::PackOptions>,
    quotas: Vec<weftfs::quota::QuotaBudget>,
    single_flight: bool,
    single_flight_max_entries: usize,
    single_flight_max_bytes: u64,
    cache_bytes: u64,
    cache_entries: usize,
}

impl OpenPolicy {
    /// The backend this policy selects. Packing wraps the selected backend.
    fn kind(&self) -> Result<BackendKind> {
        let kind = self.backend.kind();
        match self.pack {
            Some(options) => BackendKind::packed(kind, options),
            None => Ok(kind),
        }
    }

    fn options(&self) -> Options {
        Options {
            fixed_timestamp_ns: self.fixed_timestamp_ns,
            representation: self.representation,
            content_identity: self.content_identity,
            codec_min_bytes: self.codec_min_bytes,
            quotas: self.quotas.clone(),
            single_flight: self.single_flight,
            single_flight_max_entries: self.single_flight_max_entries,
            single_flight_max_bytes: self.single_flight_max_bytes,
            cache_bytes: self.cache_bytes,
            cache_entries: self.cache_entries,
            ..Options::default()
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RemoteOptions {
    s3_endpoint: Option<String>,
    s3_bucket: Option<String>,
    s3_region: Option<String>,
    s3_access_key: Option<String>,
    s3_secret_key: Option<String>,
    s3_session_token: Option<String>,
    gcs_endpoint: Option<String>,
    gcs_bucket: Option<String>,
    gcs_token: Option<String>,
    gcs_project: Option<String>,
    gcs_anonymous: bool,
    gcs_credentials: Option<String>,
    gcs_scope: Option<String>,
    azure_endpoint: Option<String>,
    azure_container: Option<String>,
    azure_account: Option<String>,
    azure_key: Option<String>,
    azure_sas: Option<String>,
    azure_anonymous: bool,
    threefs_mode: String,
    threefs_mount: Option<PathBuf>,
    threefs_lib: Option<PathBuf>,
    threefs_entries: Option<usize>,
    threefs_io_bytes: Option<usize>,
    threefs_require_mount: bool,
}

fn parse_backend(text: &str, remote: &RemoteOptions) -> Result<CliBackend> {
    match text {
        "local" => Ok(CliBackend::Local),
        "object" => Ok(CliBackend::Object),
        "s3" => Ok(CliBackend::S3(S3Config::resolve(
            remote.s3_endpoint.clone(),
            remote.s3_bucket.clone(),
            remote.s3_region.clone(),
            remote.s3_access_key.clone(),
            remote.s3_secret_key.clone(),
            remote.s3_session_token.clone(),
        )?)),
        "gcs" => Ok(CliBackend::Gcs(GcsConfig::resolve(
            remote.gcs_endpoint.clone(),
            remote.gcs_bucket.clone(),
            remote.gcs_token.clone(),
            remote.gcs_project.clone(),
            Some(remote.gcs_anonymous).filter(|value| *value),
            remote.gcs_credentials.clone(),
            remote.gcs_scope.clone(),
        )?)),
        "azure" => Ok(CliBackend::Azure(AzureConfig::resolve(
            remote.azure_endpoint.clone(),
            remote.azure_container.clone(),
            remote.azure_account.clone(),
            remote.azure_key.clone(),
            remote.azure_sas.clone(),
            Some(remote.azure_anonymous).filter(|value| *value),
            None,
        )?)),
        "3fs" => Ok(CliBackend::ThreeFs(ThreeFsConfig::resolve(
            &remote.threefs_mode,
            remote.threefs_mount.clone(),
            remote.threefs_lib.clone(),
            remote.threefs_entries,
            remote.threefs_io_bytes,
            remote.threefs_require_mount,
        )?)),
        other => Err(Error::invalid_argument(format!(
            "--backend must be `local`, `object`, `s3`, `gcs`, `azure`, or `3fs` (got `{other}`)"
        ))),
    }
}

/// Resolve the physical layout: one backend, or two explicit planes.
///
/// Every CLI verb path relies on this, so the split is opt-in and a store is
/// never silently opened with a different layout than it was written with.
fn parse_planes(
    backend: &str,
    blob_backend: Option<&str>,
    commit_backend: Option<&str>,
    blob_root: Option<PathBuf>,
    remote: &RemoteOptions,
) -> Result<CliBackend> {
    let base = parse_backend(backend, remote)?;
    let blobs = blob_backend
        .map(|text| parse_backend(text, remote))
        .transpose()?;
    let commits = commit_backend
        .map(|text| parse_backend(text, remote))
        .transpose()?;
    match (blobs, commits) {
        (None, None) => Ok(base),
        (blobs, commits) => Ok(CliBackend::Split {
            blobs: Box::new(blobs.unwrap_or_else(|| base.clone())),
            commits: Box::new(commits.unwrap_or(base)),
            blob_root,
        }),
    }
}

fn parse_representation(text: &str) -> Result<RepresentationMode> {
    match text {
        "raw" => Ok(RepresentationMode::Raw),
        "codec-auto" => Ok(RepresentationMode::CodecAuto),
        "overlay" => Ok(RepresentationMode::Overlay),
        other => Err(Error::invalid_argument(format!(
            "--representation must be `raw`, `codec-auto`, or `overlay` (got `{other}`)"
        ))),
    }
}

fn parse_content_identity(text: &str) -> Result<ContentIdentity> {
    ContentIdentity::parse(text)
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

/// One-line summary of how a loaded host calibration differs from the baked
/// `measured` table, used when `--calibration` is active.
fn divergence_summary(record: &weftfs::Calibration) -> String {
    let divergence = record.divergence(weftfs::CostModelKind::Measured);
    let differing = divergence
        .iter()
        .filter(|row| row.estimate_1mib_delta_ppm() != 0)
        .count();
    match divergence.iter().max_by_key(|row| {
        row.estimate_1mib_delta_ppm().unsigned_abs()
    }) {
        Some(worst) => format!(
            "vs baked measured: largest 1 MiB divergence {} {:+.3}% (host {} ns vs baked {} ns), {} of {} rows differ",
            worst.class.as_str(),
            worst.estimate_1mib_delta_percent(),
            worst.host_1mib_ns,
            worst.baked_1mib_ns,
            differing,
            divergence.len(),
        ),
        None => "vs baked measured: no rows".to_owned(),
    }
}

fn run(cli: Cli) -> Result<()> {
    let Cli {
        store,
        json,
        fixed_timestamp_ns,
        representation,
        backend,
        blob_backend,
        commit_backend,
        blob_root,
        s3_endpoint,
        s3_bucket,
        s3_region,
        s3_access_key,
        s3_secret_key,
        s3_session_token,
        gcs_endpoint,
        gcs_bucket,
        gcs_token,
        gcs_project,
        gcs_anonymous,
        gcs_credentials,
        gcs_scope,
        azure_endpoint,
        azure_container,
        azure_account,
        azure_key,
        azure_sas,
        azure_anonymous,
        threefs_mode,
        threefs_mount,
        threefs_lib,
        threefs_entries,
        threefs_io_bytes,
        threefs_require_mount,
        pack,
        pack_target_bytes,
        pack_member_threshold,
        pack_catalog_bytes,
        pack_segments,
        codec_min_bytes,
        quotas,
        quota_file,
        single_flight,
        single_flight_max_entries,
        single_flight_max_bytes,
        cache_bytes,
        cache_entries,
        content_identity,
        planner_cost_model,
        calibration,
        command,
    } = cli;
    let planner_cost_model = weftfs::CostModelKind::parse(&planner_cost_model)?;
    let calibration = match calibration {
        None => None,
        Some(path) => {
            if planner_cost_model != weftfs::CostModelKind::Measured {
                return Err(Error::invalid_argument(
                    "--calibration requires --planner-cost-model measured",
                ));
            }
            let record = weftfs::Calibration::load(&path)?;
            if !json {
                eprintln!(
                    "calibration {}: {}",
                    record.id.to_hex(),
                    divergence_summary(&record)
                );
            }
            Some(Arc::new(record))
        }
    };
    let calibration_table = calibration.as_ref().map(|record| record.table());
    let mut budgets: Vec<weftfs::quota::QuotaBudget> = quotas
        .iter()
        .map(|text| weftfs::quota::parse_budget(text))
        .collect::<Result<Vec<_>>>()?;
    if let Some(path) = quota_file {
        let contents = fs::read_to_string(&path).map_err(|err| {
            Error::invalid_argument(format!(
                "cannot read --quota-file {}: {err}",
                path.display()
            ))
        })?;
        budgets.extend(parse_quota_file(&contents)?);
    }
    let policy = OpenPolicy {
        fixed_timestamp_ns,
        representation: parse_representation(&representation)?,
        content_identity: parse_content_identity(
            content_identity.as_deref().unwrap_or("whole-file"),
        )?,
        codec_min_bytes: parse_codec_min_bytes(codec_min_bytes.as_deref())?,
        backend: parse_planes(
            &backend,
            blob_backend.as_deref(),
            commit_backend.as_deref(),
            blob_root,
            &RemoteOptions {
                s3_endpoint,
                s3_bucket,
                s3_region,
                s3_access_key,
                s3_secret_key,
                s3_session_token,
                gcs_endpoint,
                gcs_bucket,
                gcs_token,
                gcs_project,
                gcs_anonymous,
                gcs_credentials,
                gcs_scope,
                azure_endpoint,
                azure_container,
                azure_account,
                azure_key,
                azure_sas,
                azure_anonymous,
                threefs_mode,
                threefs_mount,
                threefs_lib,
                threefs_entries,
                threefs_io_bytes,
                threefs_require_mount,
            },
        )?,
        pack: if pack {
            let mut options = weftfs::packs::PackOptions::default();
            if let Some(bytes) = pack_target_bytes {
                options.pack_target_bytes = bytes;
            }
            if let Some(bytes) = pack_member_threshold {
                options.member_threshold_bytes = bytes;
            }
            if let Some(bytes) = pack_catalog_bytes {
                options.max_catalog_bytes = bytes;
            }
            if let Some(segments) = pack_segments {
                options.catalog_segments = segments;
            }
            options.validate()?;
            Some(options)
        } else {
            None
        },
        quotas: budgets,
        single_flight,
        single_flight_max_entries,
        single_flight_max_bytes,
        cache_bytes,
        cache_entries,
    };
    // An overlay layer is only composable over a Merkle chunk table, so
    // selecting the overlay representation without naming an identity selects
    // the only identity it can use. An explicit `--content-identity whole-file`
    // still wins and is refused by the writer rather than silently upgraded.
    let mut policy = policy;
    if policy.representation == RepresentationMode::Overlay && content_identity.is_none() {
        policy.content_identity = ContentIdentity::MerkleV3;
    }
    let needs_store = !matches!(
        command,
        Command::ConformanceStore { .. } | Command::Capability { .. } | Command::Calibrate { .. }
    ) && !matches!(
        &command,
        Command::TrainingReady {
            verify_out: Some(_),
            ..
        }
    );
    let store = match store {
        Some(path) => path,
        None if !needs_store => PathBuf::new(),
        None if policy.backend.is_remote() => PathBuf::from("<s3>"),
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
                content_identity: policy.content_identity,
                codec_min_bytes: policy.codec_min_bytes,
                quotas: policy.quotas.clone(),
                ..Options::default()
            };
            let store = Store::init_with_backend(&store, options, policy.kind()?)?;
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
            let store = open_store(&store, OpenMode::ReadOnly, policy.clone())?;
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
                    "content_identity": policy.content_identity.as_str(),
                    "codec_min_bytes": policy.codec_min_bytes,
                    "rejected_generations": store.rejected_generations(),
                    "recovery_notes": store.recovery_notes(),
                    "backend": store.backend_kind().describe(),
                    "planes": store.plane_capabilities(),
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
                println!("content_identity={}", policy.content_identity.as_str());
                println!("codec_min_bytes={}", policy.codec_min_bytes);
                println!("backend={}", store.backend_kind().describe());
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
            tenant,
        } => {
            let (default_uid, default_gid) = current_ids();
            let bytes = read_input(text, input_file)?;
            let mut attrs = parse_attrs(&attrs, &attr_bytes, &attr_ints, &attr_u64_lists)?;
            if let Some(tenant) = tenant {
                attrs.insert(weftfs::state::ATTR_TENANT.to_owned(), Value::Str(tenant));
            }
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
            lambda_write_ns,
            allow_experimental_modes,
            no_zstd19,
            zstd19_cpu_budget_ns,
            no_zstd_patch,
            zstd_patch_level,
            strategy,
            semantic_policy,
            candidates,
            tile_shape,
            auto_tile_bytes,
            access_pattern,
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
                        lambda_write_ns: lambda_write_ns.unwrap_or(lambda_ns),
                        lambda_write_ns_set: lambda_write_ns.is_some(),
                        allow_experimental_modes,
                        zstd19: !no_zstd19,
                        zstd19_cpu_budget_ns,
                        zstd_patch: !no_zstd_patch,
                        zstd_patch_level,
                        cost_model: planner_cost_model,
                        calibration: calibration_table.clone(),
                        semantic_policy: parse_semantic_policy(semantic_policy.as_deref())?,
                        candidates: parse_object_ids(candidates.as_deref())?,
                        auto_tile_bytes,
                        access_pattern: AccessPattern::parse(&access_pattern)?,
                        ..TensorPlanOptions::default()
                    },
                    tile_shape: match tile_shape.as_deref() {
                        Some(text) => Some(parse_u64_list(text, "--tile-shape")?),
                        None => None,
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
                        "tile_shape": meta.tile_shape,
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
            lambda_write_ns,
            allow_experimental_modes,
            no_zstd19,
            zstd19_cpu_budget_ns,
            no_zstd_patch,
            zstd_patch_level,
            causal,
            semantic_policy,
            candidates,
            tile_shape,
            auto_tile_bytes,
            access_pattern,
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
            let plan_access_pattern = AccessPattern::parse(&access_pattern)?;
            let options = TensorPlanOptions {
                lambda_ns,
                lambda_write_ns: lambda_write_ns.unwrap_or(lambda_ns),
                lambda_write_ns_set: lambda_write_ns.is_some(),
                allow_experimental_modes,
                zstd19: !no_zstd19,
                zstd19_cpu_budget_ns,
                zstd_patch: !no_zstd_patch,
                zstd_patch_level,
                cost_model: planner_cost_model,
                calibration: calibration_table.clone(),
                semantic_policy: parse_semantic_policy(semantic_policy.as_deref())?,
                candidates: parse_object_ids(candidates.as_deref())?,
                tile_shape: match tile_shape.as_deref() {
                    Some(text) => Some(parse_u64_list(text, "--tile-shape")?),
                    None => None,
                },
                auto_tile_bytes,
                access_pattern: plan_access_pattern,
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
                            "estimated_encode_ns": item.estimated_encode_ns,
                            "cost": item.cost.map(|cost| cost.to_string()),
                            "dependency": item.dependency.map(|hash| hash.to_hex()),
                        })
                    })
                    .collect();
                let value = json!({
                    "chosen": plan.chosen.as_str(),
                    "chosen_physical_bytes": plan.chosen_physical_bytes,
                    "chosen_estimated_decode_ns": plan.chosen_estimated_decode_ns,
                    "chosen_estimated_encode_ns": plan.chosen_estimated_encode_ns,
                    "chosen_cost": plan.chosen_cost.to_string(),
                    "runner_up": plan.runner_up.map(|strategy| strategy.as_str()),
                    "runner_up_physical_bytes": plan.runner_up_physical_bytes,
                    "runner_up_estimated_decode_ns": plan.runner_up_estimated_decode_ns,
                    "runner_up_estimated_encode_ns": plan.runner_up_estimated_encode_ns,
                    "runner_up_cost": plan.runner_up_cost.map(|cost| cost.to_string()),
                    "lambda_ns": lambda_ns,
                    "lambda_write_ns": options.effective_write_lambda(),
                    "write_cost_calibration": weftfs::cost_model::ENCODE_CALIBRATION,
                    "cost_model": planner_cost_model.as_str(),
                    "access_pattern": plan_access_pattern.as_str(),
                    "candidates": candidates,
                });
                // One line per plan so receipt harnesses can join rows without
                // a JSON parser; the value itself is ordinary JSON.
                println!("{value}");
                Ok(())
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
        Command::Compact { path } => {
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let id = store.resolve_path(&path)?;
            let report = store.compact(id)?;
            if json {
                print_json(&json!({
                    "object_id": report.object.to_hex(),
                    "previous_content": report.previous_content.to_hex(),
                    "content": report.content.to_hex(),
                    "version_id": report.version.to_hex(),
                    "size": report.size,
                    "content_root": report.content_root.to_hex(),
                    "chunks": report.chunks,
                    "overlay_depth": report.overlay_depth,
                    "overlay_bytes": report.overlay_bytes,
                    "verified_bytes": report.verified_bytes,
                    "generation": store.generation(),
                }))
            } else {
                println!("{}", report.content.to_hex());
                println!("version={}", report.version.to_hex());
                println!("content_root={}", report.content_root.to_hex());
                println!("chunks={}", report.chunks);
                println!("overlay_depth={}", report.overlay_depth);
                println!("overlay_bytes={}", report.overlay_bytes);
                println!("verified_bytes={}", report.verified_bytes);
                Ok(())
            }
        }
        Command::Cat {
            path,
            offset,
            len,
            region,
            threads,
            envelope,
            revocations,
            uid,
            gid,
        } => {
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
            let id = store.resolve_path(&path)?;
            let (default_uid, default_gid) = current_ids();
            let principal = build_principal(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                Vec::new(),
                envelope.as_ref(),
                revocations.as_ref(),
                store.clock_now_ns(),
            )?;
            if envelope.is_some() {
                // Possession of a path or id is not authority: the envelope and
                // POSIX rules must both allow this object.
                store.exact_id_object(id, None, &principal, store.clock_now_ns())?;
            }
            // The cache and single-flight layers are only reachable through an
            // authorized token. Without `--envelope` a denial is not fatal (the
            // CLI never enforced POSIX on `cat`), but the uncached read below is
            // then used, so a cache hit can never stand in for authorization.
            let authorized = match store.authorize_read(id, &principal, store.clock_now_ns()) {
                Ok(token) => Some(token),
                Err(err) if envelope.is_some() => return Err(err),
                Err(_) => None,
            };
            let tile_options = weftfs::TileDecodeOptions {
                threads,
                ..weftfs::TileDecodeOptions::default()
            };
            let bytes = if let Some(region) = region {
                let region = parse_region(&region)?;
                store
                    .read_tensor_region_with_options(id, &region, &tile_options)?
                    .bytes
            } else if offset == 0 && len.is_none() {
                // `--threads` only changes mode-15 tensor reads; every other
                // object keeps the shipped byte read.
                if threads > 1 && store.tensor_metadata(id).is_ok() {
                    store.read_tensor_with_options(id, &tile_options)?
                } else if let Some(token) = authorized.as_ref() {
                    store.read_object_authorized(token)?
                } else {
                    store.read_object(id)?
                }
            } else {
                match authorized.as_ref() {
                    Some(token) => {
                        store.read_range_authorized(token, offset, len.unwrap_or(u64::MAX))?
                    }
                    None => store.read_range(id, offset, len.unwrap_or(u64::MAX))?,
                }
            };
            if json {
                let stats = store.read_layer_stats();
                print_json(&json!({
                    "object_id": id.to_hex(),
                    "size": bytes.len(),
                    "hex": hex::encode(&bytes),
                    "decode_calls": stats.decode_calls,
                    "decode_bytes": stats.decode_bytes,
                    "single_flight": {
                        "enabled": stats.single_flight.enabled,
                        "leaders": stats.single_flight.leaders,
                        "waiters": stats.single_flight.waiters,
                        "degraded": stats.single_flight.degraded,
                    },
                    "cache": {
                        "enabled": stats.cache.enabled,
                        "hits": stats.cache.hits,
                        "misses": stats.cache.misses,
                        "admissions": stats.cache.admissions,
                        "evictions": stats.cache.evictions,
                        "entries": stats.cache.entries,
                        "bytes": stats.cache.bytes,
                    },
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
                let mut value = object_json(&record);
                if record.kind == ObjectKind::File {
                    if let Ok(manifest) =
                        weftfs::recovery::read_manifest(store.root(), record.content)
                    {
                        value["content_hash"] = json!(manifest.content_hash.to_hex());
                        value["content_identity"] = json!(manifest.identity.as_str());
                        value["chunk_size"] = json!(manifest.chunk_size);
                        value["chunks"] = json!(manifest.chunks.len());
                    }
                }
                print_json(&value)
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
                    let index_stats = store.index_stats();
                    print_json(&json!({
                        "selector_id": resolution.selector_id.to_hex(),
                        "snapshot_id": resolution.snapshot_id.to_hex(),
                        "matched_count": resolution.matched_count,
                        "results": results,
                        "evidence": evidence,
                        // Advisory index diagnostics: a load is provable from
                        // outside (loads>0, builds==0). Never part of a format.
                        "index": {
                            "loads": index_stats.disk_loads,
                            "builds": index_stats.builds,
                            "rejects": index_stats.rejects,
                        },
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
            SnapshotCommand::Branch { source, name } => {
                let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
                let pinned = store.snapshot_branch(&source, &name)?;
                if json {
                    print_json(&json!({
                        "name": name,
                        "source": source,
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
        Command::Quota { command } => match command {
            QuotaCommand::Show { uid, gid, tenant } => {
                let store = open_store(&store, OpenMode::ReadOnly, policy)?;
                let principal = match tenant {
                    Some(name) => QuotaPrincipal::Tenant(name),
                    None => QuotaPrincipal::Posix { uid, gid },
                };
                if let Some(problem) = store.quota_problem() {
                    return Err(Error::corrupt(format!(
                        "quota usage record is unusable: {problem}"
                    )));
                }
                let usage = store.quota_usage(&principal);
                let budget = store.quota_budget(&principal);
                if json {
                    print_json(&json!({
                        "principal": principal.display(),
                        "configured": budget.is_some(),
                        "used_bytes": usage.used_bytes,
                        "used_objects": usage.used_objects,
                        "max_new_bytes": budget.as_ref().map(|b| b.max_new_bytes),
                        "max_objects": budget.as_ref().map(|b| b.max_objects),
                    }))
                } else {
                    println!("principal={}", principal.display());
                    println!("configured={}", budget.is_some());
                    println!("used_bytes={}", usage.used_bytes);
                    println!("used_objects={}", usage.used_objects);
                    if let Some(budget) = budget {
                        println!("max_new_bytes={}", budget.max_new_bytes);
                        println!("max_objects={}", budget.max_objects);
                    }
                    Ok(())
                }
            }
            QuotaCommand::Reset => {
                let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
                store.quota_reset()?;
                if json {
                    print_json(&json!({"reset": true, "generation": store.generation()}))
                } else {
                    println!("reset=true");
                    Ok(())
                }
            }
            QuotaCommand::Rebuild => {
                let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
                let commits = store.quota_rebuild()?;
                if json {
                    print_json(&json!({
                        "rebuilt": true,
                        "commits": commits,
                        "generation": store.generation(),
                    }))
                } else {
                    println!("rebuilt=true");
                    println!("commits={commits}");
                    Ok(())
                }
            }
        },
        Command::Merge { left, right } => {
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let report = store.merge_branches(&left, &right)?;
            if json {
                print_json(&json!({
                    "left": report.left.to_hex(),
                    "right": report.right.to_hex(),
                    "paths": report.paths,
                    "added_objects": report.added_objects,
                    "added_entries": report.added_entries,
                    "merged" : report.head.to_hex(),
                    "generation": report.generation,
                }))
            } else {
                println!("left={}", report.left);
                println!("right={}", report.right);
                println!("paths={}", report.paths);
                println!("added_objects={}", report.added_objects);
                println!("added_entries={}", report.added_entries);
                println!("merged={}", report.head);
                println!("generation={}", report.generation);
                Ok(())
            }
        }
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
                    "checked_capability_envelopes": report.checked_capability_envelopes,
                    "checked_provenance": report.checked_provenance,
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
                println!(
                    "checked_capability_envelopes={}",
                    report.checked_capability_envelopes
                );
                println!("checked_provenance={}", report.checked_provenance);
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
                    "deleted_index_bytes": report.deleted_index_bytes,
                    "deleted_total_bytes": report.deleted_total_bytes,
                    "reclaimed_packs": report.reclaimed_packs,
                    "reclaimed_pack_bytes": report.reclaimed_pack_bytes,
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
                println!("deleted_index_bytes={}", report.deleted_index_bytes);
                println!("deleted_total_bytes={}", report.deleted_total_bytes);
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
        Command::Checkpoint { command } => match command {
            None => {
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
            Some(subcommand) => run_checkpoint(&store, json, policy, subcommand),
        },
        Command::PlanParents {
            versions,
            max_depth,
            lambda_ns,
            reads_per_version,
            sla_ms,
            storage_budget,
            solver,
            apply,
            out,
            scratch,
            keep_scratch,
            shards,
            axis,
            shard_strategy,
            no_semantic,
            max_ingest_bytes,
            restore_overhead,
        } => run_plan_parents(
            &store,
            json,
            policy,
            versions,
            max_depth,
            lambda_ns,
            reads_per_version,
            sla_ms,
            storage_budget,
            solver,
            apply,
            out,
            scratch,
            keep_scratch,
            shards,
            axis,
            shard_strategy,
            no_semantic,
            max_ingest_bytes,
            restore_overhead,
        ),
        Command::CheckpointService {
            fd,
            socket,
            max_frame_bytes,
            max_tensor_bytes,
        } => {
            let config = weftfs::checkpoint_service::ServiceConfig {
                fd,
                socket,
                max_frame_bytes,
                max_tensor_bytes,
            };
            weftfs::checkpoint_service::serve(&store, policy.options(), policy.kind()?, config)
        }
        Command::Context { command } => run_context(&store, json, policy, command),
        Command::Materialize {
            root,
            training_ready,
            out,
            max_bytes,
            max_objects,
            max_decode_ms,
            capability_file,
            envelope,
            revocations,
            require_tensors,
            uid,
            gid,
            groups,
        } => run_materialize(
            &store,
            json,
            policy,
            root,
            training_ready,
            out,
            max_bytes,
            max_objects,
            max_decode_ms,
            capability_file,
            envelope,
            revocations,
            require_tensors,
            uid,
            gid,
            groups,
        ),
        Command::TrainingReady {
            root,
            check,
            verify_out,
            max_bytes,
            max_objects,
            max_decode_ms,
            capability_file,
            envelope,
            revocations,
            require_tensors,
            uid,
            gid,
            groups,
        } => run_training_ready(
            &store,
            json,
            policy,
            root,
            check,
            verify_out,
            max_bytes,
            max_objects,
            max_decode_ms,
            capability_file,
            envelope,
            revocations,
            require_tensors,
            uid,
            gid,
            groups,
        ),
        Command::Capability { command } => run_capability(&store, json, policy, command),
        Command::Kv { command } => run_kv(&store, json, policy, command),
        Command::Causal { command } => run_causal(&store, json, policy, command),
        Command::Uri { command } => run_uri(&store, json, policy, command),
        Command::Provenance { command } => run_provenance(&store, json, policy, command),
        Command::Rebase {
            root,
            threshold_ns,
            dry_run,
            strategy,
            lambda_ns,
            max_objects,
        } => {
            let mode = if dry_run {
                OpenMode::ReadOnly
            } else {
                OpenMode::ReadWrite
            };
            let mut store = open_store(&store, mode, policy)?;
            let root_id = if root.len() == 64 && root.bytes().all(|b| b.is_ascii_hexdigit()) {
                weftfs::Hash32::from_hex(&root)
                    .map_err(|err| Error::invalid_argument(format!("bad --root id: {err}")))?
            } else {
                store.resolve_path(&root)?
            };
            let options = weftfs::RebaseOptions {
                threshold_ns,
                lambda_ns,
                cost_model: planner_cost_model,
                calibration: calibration_table.clone(),
                strategy_hint: match strategy.as_deref() {
                    None => None,
                    Some(text) => match parse_tensor_strategy(text)? {
                        None => None,
                        Some(hint) => {
                            if !hint.is_independent() {
                                return Err(Error::invalid_argument(format!(
                                    "rebase requires a standalone strategy (raw, zstd, \
                                     float-tensor, tensor-sharded, tensor-tiled); `{}` carries a \
                                     dependency and cannot be the rebase target",
                                    hint.as_str()
                                )));
                            }
                            Some(hint)
                        }
                    },
                },
                max_objects,
                dry_run,
            };
            let report = store.rebase(root_id, &options)?;
            if json {
                let candidates: Vec<serde_json::Value> = report
                    .plan
                    .candidates
                    .iter()
                    .map(|item| {
                        json!({
                            "object": item.object.to_hex(),
                            "path": item.path,
                            "reads": item.reads,
                            "from_strategy": item.from_strategy,
                            "from_cost_class": item.from_cost_class,
                            "logical_bytes": item.logical_bytes,
                            "estimated_decode_ns": item.estimated_decode_ns,
                            "read_cost_ns": item.read_cost_ns,
                            "to_strategy": item.to_strategy.as_str(),
                            "new_physical_bytes": item.new_physical_bytes,
                            "new_estimated_decode_ns": item.new_estimated_decode_ns,
                            "candidates": item.candidates.iter().map(|(name, bytes, ns)| {
                                json!({"strategy": name, "physical_bytes": bytes, "estimated_decode_ns": ns})
                            }).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let skipped: Vec<serde_json::Value> = report
                    .plan
                    .skipped
                    .iter()
                    .map(|item| {
                        json!({
                            "object": item.object.to_hex(),
                            "path": item.path,
                            "reason": item.reason,
                        })
                    })
                    .collect();
                let applied: Vec<serde_json::Value> = report
                    .applied
                    .iter()
                    .map(|item| {
                        json!({
                            "object": item.object.to_hex(),
                            "path": item.path,
                            "old_version": item.old_version.to_hex(),
                            "new_version": item.new_version.to_hex(),
                            "content": item.content.to_hex(),
                            "from_strategy": item.from_strategy,
                            "to_strategy": item.to_strategy.as_str(),
                            "reads_consumed": item.reads_consumed,
                            "provenance_id": item.provenance_id.to_hex(),
                        })
                    })
                    .collect();
                print_json(&json!({
                    "root": report.plan.root.to_hex(),
                    "threshold_ns": report.plan.threshold_ns,
                    "cost_model": report.plan.cost_model.as_str(),
                    "dry_run": dry_run,
                    "scanned": report.plan.scanned,
                    "truncated": report.plan.truncated,
                    "candidates": candidates,
                    "skipped": skipped,
                    "applied": applied,
                }))
            } else {
                println!(
                    "scanned={} candidates={} applied={} threshold_ns={} cost_model={}",
                    report.plan.scanned,
                    report.plan.candidates.len(),
                    report.applied.len(),
                    report.plan.threshold_ns,
                    report.plan.cost_model.as_str(),
                );
                for item in &report.plan.candidates {
                    println!(
                        "rebase {} {} reads={} {}->{} read_cost_ns={} new_bytes={}",
                        item.object,
                        item.path,
                        item.reads,
                        item.from_strategy,
                        item.to_strategy.as_str(),
                        item.read_cost_ns,
                        item.new_physical_bytes,
                    );
                }
                for item in &report.plan.skipped {
                    println!("skip {} {} reason={}", item.object, item.path, item.reason);
                }
                for item in &report.applied {
                    println!(
                        "applied {} {} version={} provenance={}",
                        item.object, item.path, item.new_version, item.provenance_id,
                    );
                }
                Ok(())
            }
        }
        Command::Transcode {
            root,
            dry_run,
            max_objects,
            max_bytes,
            max_millis,
            min_saved_percent,
            strategy,
            no_verify_after,
            crash_after,
            crash_before,
        } => {
            let mode = if dry_run {
                OpenMode::ReadOnly
            } else {
                OpenMode::ReadWrite
            };
            let mut store = open_store(&store, mode, policy)?;
            let root_id = if root.len() == 64 && root.bytes().all(|b| b.is_ascii_hexdigit()) {
                weftfs::Hash32::from_hex(&root)
                    .map_err(|err| Error::invalid_argument(format!("bad --root id: {err}")))?
            } else {
                store.resolve_path(&root)?
            };
            let options = weftfs::transcode::TranscodeOptions {
                max_objects,
                max_bytes,
                max_millis,
                min_saved_percent,
                dry_run,
                strategy_hint: match strategy.as_deref() {
                    None => None,
                    Some(text) => match parse_tensor_strategy(text)? {
                        None => None,
                        Some(hint) => {
                            if !hint.is_independent() {
                                return Err(Error::invalid_argument(format!(
                                    "transcode requires a standalone strategy; `{}` carries a \
                                     dependency and cannot be the transcode target",
                                    hint.as_str()
                                )));
                            }
                            Some(hint)
                        }
                    },
                },
                cost_model: planner_cost_model,
                calibration: calibration_table.clone(),
                verify_after: !no_verify_after,
                crash_after_publishes: crash_after,
                crash_before_publish: crash_before,
                ..weftfs::transcode::TranscodeOptions::default()
            };
            let report = store.transcode(root_id, &options)?;
            if json {
                print_json(&json!({
                    "root": report.root.to_hex(),
                    "dry_run": report.dry_run,
                    "scanned": report.scanned,
                    "truncated": report.truncated,
                    "stopped_reason": report.stopped_reason,
                    "bytes_read": report.bytes_read,
                    "elapsed_ms": report.elapsed_ms,
                    "candidates": report.candidates.iter().map(|item| json!({
                        "object": item.object.to_hex(),
                        "path": item.path,
                        "from_strategy": item.from_strategy,
                        "to_strategy": item.to_strategy.as_str(),
                        "logical_bytes": item.logical_bytes,
                        "old_physical_bytes": item.old_physical_bytes,
                        "new_physical_bytes": item.new_physical_bytes,
                        "saved_bytes": item.old_physical_bytes.saturating_sub(item.new_physical_bytes),
                        "estimated_decode_ns": item.estimated_decode_ns,
                        "candidates": item.candidates.iter().map(|(name, bytes, ns)| {
                            json!({"strategy": name, "physical_bytes": bytes, "estimated_decode_ns": ns})
                        }).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "skipped": report.skipped.iter().map(|item| json!({
                        "object": item.object.to_hex(),
                        "path": item.path,
                        "reason": item.reason,
                    })).collect::<Vec<_>>(),
                    "applied": report.applied.iter().map(|item| json!({
                        "object": item.object.to_hex(),
                        "path": item.path,
                        "old_version": item.old_version.to_hex(),
                        "new_version": item.new_version.to_hex(),
                        "content": item.content.to_hex(),
                        "from_strategy": item.from_strategy,
                        "to_strategy": item.to_strategy.as_str(),
                        "old_physical_bytes": item.old_physical_bytes,
                        "new_physical_bytes": item.new_physical_bytes,
                        "verified": item.verified,
                        "provenance_id": item.provenance_id.to_hex(),
                    })).collect::<Vec<_>>(),
                }))
            } else {
                println!(
                    "scanned={} candidates={} applied={} bytes_read={} elapsed_ms={:.1}",
                    report.scanned,
                    report.candidates.len(),
                    report.applied.len(),
                    report.bytes_read,
                    report.elapsed_ms,
                );
                for item in &report.candidates {
                    println!(
                        "transcode {} {} {}->{} {}->{} bytes",
                        item.object,
                        item.path,
                        item.from_strategy,
                        item.to_strategy.as_str(),
                        item.old_physical_bytes,
                        item.new_physical_bytes,
                    );
                }
                for item in &report.skipped {
                    println!("skip {} {} reason={}", item.object, item.path, item.reason);
                }
                for item in &report.applied {
                    println!(
                        "applied {} {} version={} verified={} provenance={}",
                        item.object, item.path, item.new_version, item.verified, item.provenance_id,
                    );
                }
                Ok(())
            }
        }
        Command::Calibrate {
            out,
            from,
            install,
            reps,
            large_mib,
            small_kib,
        } => {
            let options = weftfs::MeasurementOptions {
                reps,
                large_mib,
                small_kib,
            };
            let (record, output_path) = match from {
                Some(path) => {
                    let record = weftfs::Calibration::load(&path)?;
                    let output_path = match out {
                        Some(path) => {
                            record.write(&path)?;
                            Some(path)
                        }
                        None => None,
                    };
                    (record, output_path)
                }
                None => {
                    options.validate()?;
                    let outcome = weftfs::calibration::measure_decode_costs_scratch(&options)?;
                    let host = weftfs::HostIdentity::current();
                    let created_unix_ns = match fixed_timestamp_ns
                        .and_then(|v| u64::try_from(v).ok())
                    {
                        Some(value) if value > 0 => value,
                        _ => std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
                            .unwrap_or(0),
                    };
                    let record = weftfs::calibration::record_from_measurement(
                        &outcome,
                        &options,
                        host,
                        created_unix_ns,
                    )?;
                    let path = out
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("decode-calibration.json"));
                    record.write(&path)?;
                    (record, Some(path))
                }
            };
            let installed = if install {
                if store.as_os_str().is_empty() || policy.backend.is_remote() {
                    return Err(Error::invalid_argument(
                        "calibrate --install requires a local --store <DIR>",
                    ));
                }
                Some(record.install(&store)?)
            } else {
                None
            };
            let divergence = record.divergence(weftfs::CostModelKind::Measured);
            if json {
                let rows: Vec<serde_json::Value> = record
                    .rows()
                    .iter()
                    .map(|row| {
                        json!({
                            "class": row.class.as_str(),
                            "fixed_ns": row.fixed_ns,
                            "ns_per_mib": row.ns_per_mib,
                            "samples": row.samples,
                            "origin": row.origin.as_str(),
                        })
                    })
                    .collect();
                let divergence_rows: Vec<serde_json::Value> = divergence
                    .iter()
                    .map(|row| {
                        json!({
                            "class": row.class.as_str(),
                            "host_fixed_ns": row.host.fixed_ns,
                            "host_ns_per_mib": row.host.ns_per_mib,
                            "baked_fixed_ns": row.baked.fixed_ns,
                            "baked_ns_per_mib": row.baked.ns_per_mib,
                            "host_1mib_ns": row.host_1mib_ns,
                            "baked_1mib_ns": row.baked_1mib_ns,
                            "estimate_1mib_delta_ppm": row.estimate_1mib_delta_ppm(),
                        })
                    })
                    .collect();
                print_json(&json!({
                    "schema": weftfs::calibration::CALIBRATION_SCHEMA,
                    "calibration_id": record.id.to_hex(),
                    "created_unix_ns": record.created_unix_ns,
                    "out": output_path.as_ref().map(|path| path.display().to_string()),
                    "installed": installed.as_ref().map(|path| path.display().to_string()),
                    "rows": rows,
                    "divergence": divergence_rows,
                }))
            } else {
                println!("calibration_id={}", record.id.to_hex());
                if let Some(path) = &output_path {
                    println!("out={}", path.display());
                }
                if let Some(path) = &installed {
                    println!("installed={}", path.display());
                }
                println!("divergence_vs_baked_measured:");
                for row in &divergence {
                    println!(
                        "  {} host_fixed_ns={} host_ns_per_mib={} baked_fixed_ns={} baked_ns_per_mib={} host_1mib_ns={} baked_1mib_ns={} delta={:+.3}%",
                        row.class.as_str(),
                        row.host.fixed_ns,
                        row.host.ns_per_mib,
                        row.baked.fixed_ns,
                        row.baked.ns_per_mib,
                        row.host_1mib_ns,
                        row.baked_1mib_ns,
                        row.estimate_1mib_delta_percent(),
                    );
                }
                Ok(())
            }
        }
        Command::Recover {
            target,
            plan,
            fetch_cost,
            decode_cost,
            recompute_cost,
        } => {
            if !plan {
                return Err(Error::invalid_argument("recover v1 implements --plan only"));
            }
            run_recover(
                &store,
                json,
                policy,
                &target,
                weftfs::RecoveryCosts {
                    fetch: fetch_cost,
                    decode: decode_cost,
                    recompute: recompute_cost,
                },
            )
        }
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
            write_back,
            group_commit,
            verify_load,
            load_receipt_dir,
        } => {
            if verify_load && load_receipt_dir.is_none() {
                return Err(Error::invalid_argument(
                    "--verify-load requires --load-receipt-dir DIR",
                ));
            }
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
            if read_only && (write_back || group_commit) {
                return Err(Error::invalid_argument(
                    "--write-back and --group-commit require a writable live mount",
                ));
            }
            // The mount default is Merkle identity: files created through the
            // mount are written for range-update workloads, and the mount is
            // the only path that creates AI-artifact-sized files interactively.
            // Non-mount commands keep the frozen `whole-file` default so every
            // existing fixture stays v1. An explicit flag always wins.
            let mut policy = policy;
            if content_identity.is_none() {
                if policy.representation == RepresentationMode::CodecAuto {
                    return Err(Error::invalid_argument(
                        "--representation codec-auto requires an explicit --content-identity whole-file (a v3 Merkle manifest carries a raw representation only)",
                    ));
                }
                policy.content_identity = ContentIdentity::MerkleV3;
            }
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
                    let state = Arc::new(store.state_at(snapshot)?);
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
                        let state = Arc::new(store.state_at(selected.commit)?);
                        (None, Some(selected.commit), Some(state))
                    }
                    None => match snapshot {
                        Some(name) => {
                            let commit = lookup_snapshot(&store, &name)?;
                            let state = Arc::new(store.state_at(commit)?);
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
                    write_back,
                    group_commit,
                    verify_load,
                    load_receipt_dir,
                },
            )
        }
        #[cfg(feature = "fuse")]
        Command::MountMany {
            spec,
            allow_other,
            verify_load,
            load_receipt_dir,
        } => {
            let spec = read_mount_spec(&spec)?;
            if spec.views.len() > weftfs::fuse::MAX_SHARED_VIEWS {
                return Err(Error::limit_exceeded(format!(
                    "mount-many: {} views exceeds the documented maximum of {} per shared server (one FUSE session thread per view)",
                    spec.views.len(),
                    weftfs::fuse::MAX_SHARED_VIEWS
                )));
            }
            // One read-only store for every view: shared state, shared selector
            // index cache, shared backend handle. A shared view can never
            // obtain a mutable store handle, so the server cannot write.
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
            let limits = weftcodec::Limits::default();
            let mut state_cache: BTreeMap<Hash32, Arc<weftfs::state::State>> = BTreeMap::new();
            let mut views = Vec::with_capacity(spec.views.len());
            for view in spec.views {
                let capability = read_capability(&view.capability)?;
                if view.context.is_some() && view.at.is_some() {
                    return Err(Error::invalid_argument(format!(
                        "mount-many view {}: context and at are mutually exclusive",
                        view.mountpoint.display()
                    )));
                }
                let (context_mount, snapshot_commit, snapshot_state) = match view.context {
                    Some(target) => {
                        let resolved = store.resolve_context(
                            &target,
                            capability.as_ref(),
                            &Principal::root(),
                            &limits,
                            store.clock_now_ns(),
                        )?;
                        let snapshot = resolved.stored.root.snapshot;
                        let state = cached_state(&store, &mut state_cache, snapshot)?;
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
                    None => match view.at {
                        Some(at_text) => {
                            let at_ns = weftfs::timeparse::parse_at_ns(&at_text)?;
                            let base = match &view.snapshot {
                                Some(name) => lookup_snapshot(&store, name)?,
                                None => store.head(),
                            };
                            let selected =
                                store.commit_at_or_before(base, at_ns)?.ok_or_else(|| {
                                    Error::not_found(format!(
                                        "mount-many view {}: no commit at or before {at_ns} ns",
                                        view.mountpoint.display()
                                    ))
                                })?;
                            let state = cached_state(&store, &mut state_cache, selected.commit)?;
                            (None, Some(selected.commit), Some(state))
                        }
                        None => match view.snapshot {
                            Some(name) => {
                                let commit = lookup_snapshot(&store, &name)?;
                                let state = cached_state(&store, &mut state_cache, commit)?;
                                (None, Some(commit), Some(state))
                            }
                            None => (None, None, None),
                        },
                    },
                };
                views.push(weftfs::fuse::SharedViewSpec {
                    mountpoint: view.mountpoint,
                    snapshot: snapshot_commit,
                    snapshot_state,
                    context: context_mount,
                    capability,
                });
            }
            weftfs::fuse::run_shared(store, views, allow_other, verify_load, load_receipt_dir)
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
    Store::open_with_options_and_backend(path, mode, policy.options(), policy.kind()?)
}

/// One `checkpoint` subcommand (`weftfs checkpoint` alone still forces a
/// checkpoint commit).
fn run_checkpoint(
    store_path: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    command: CheckpointCommand,
) -> Result<()> {
    match command {
        CheckpointCommand::Write {
            from_dir,
            file,
            name,
            shards,
            axis,
            tensor_filters,
            shard_strategy,
            tile_shape,
            write_policy,
            fast_codec,
            fast_zstd_level,
            critical_path,
            deadline_ms,
            causal_checkpoint,
            no_semantic,
            allow_experimental_modes,
            ranks,
            max_inflight_bytes,
            per_rank_bytes,
            prepare_deadline_ms,
            no_fragment_verify,
            checkpoint_seed,
            max_ingest_bytes,
        } => {
            let started = std::time::Instant::now();
            let source = resolve_checkpoint_source(from_dir.as_deref(), file.as_deref())?;
            let mut strategy = ShardStrategy::parse(&shard_strategy)?;
            let tile_shape = match tile_shape.as_deref() {
                Some(text) => Some(parse_u64_list(text, "--tile-shape")?),
                None => None,
            };
            if tile_shape.is_some() {
                match strategy {
                    ShardStrategy::TensorSharded => strategy = ShardStrategy::TensorTiled,
                    ShardStrategy::TensorTiled | ShardStrategy::FloatTiled => {}
                    other => {
                        return Err(Error::invalid_argument(format!(
                            "--tile-shape requires tensor-tiled or float-tiled shards; --shard-strategy {} cannot carry a tile grid",
                            other.as_str()
                        )))
                    }
                }
            } else if strategy == ShardStrategy::TensorTiled
                || strategy == ShardStrategy::FloatTiled
            {
                return Err(Error::invalid_argument(format!(
                    "--shard-strategy {} requires --tile-shape A,B,...",
                    strategy.as_str()
                )));
            }
            let options = CheckpointWriteOptions {
                shards,
                axis,
                filters: tensor_filters,
                strategy,
                tile_shape,
                write_policy: weftfs::tensor_store::WritePolicy::parse(&write_policy)?,
                fast_codec: weftfs::tensor_store::FastCodec::parse(&fast_codec)?,
                fast_zstd_level,
                on_critical_path: critical_path,
                deadline_ms,
                causal_checkpoint,
                semantic_search: !no_semantic,
                allow_experimental_modes,
                max_ingest_bytes,
                deterministic_seed: match checkpoint_seed.as_deref() {
                    None => None,
                    Some(text) => {
                        let bytes = hex::decode(text).map_err(|err| {
                            Error::invalid_argument(format!(
                                "--checkpoint-seed must be 64 hex characters: {err}"
                            ))
                        })?;
                        let array: [u8; 32] = bytes.try_into().map_err(|_| {
                            Error::invalid_argument(
                                "--checkpoint-seed must be exactly 32 bytes (64 hex characters)",
                            )
                        })?;
                        Some(array)
                    }
                },
            };
            let parallel = ranks.map(|ranks| {
                let mut options = weftfs::checkpoint::ParallelCheckpointOptions::default();
                options.ranks = ranks;
                if let Some(bytes) = max_inflight_bytes {
                    options.max_in_flight_bytes = bytes;
                }
                if let Some(bytes) = per_rank_bytes {
                    options.per_rank_bytes = bytes;
                }
                options.deadline_ms = prepare_deadline_ms;
                options.verify_fragments = !no_fragment_verify;
                options
            });
            let mut store = open_store(store_path, OpenMode::ReadWrite, policy)?;
            let report = match &parallel {
                Some(parallel) => {
                    store.write_checkpoint_parallel(&name, &source, &options, parallel)?
                }
                None => store.write_checkpoint(&name, &source, &options)?,
            };
            if json {
                print_json(&json!({
                    "checkpoint": report.name,
                    "checkpoint_id": report.checkpoint_id.to_hex(),
                    "commit": report.head.to_hex(),
                    "generation": store.generation(),
                    "manifest_id": report.manifest_id.to_hex(),
                    "manifest_root": report.manifest_root.to_hex(),
                    "source": source.display().to_string(),
                    "source_sha256": hex::encode(report.source_sha256),
                    "source_file_bytes": report.source_file_bytes,
                    "complete": report.complete,
                    "shards": options.shards,
                    "axis": options.axis,
                    "shard_strategy": options.strategy.as_str(),
                    "write_policy": options.write_policy.as_str(),
                    "write_policy_resolved": match options.resolved_policy() {
                        weftfs::tensor_store::WritePolicy::Fast => "fast",
                        weftfs::tensor_store::WritePolicy::Planned => "planned",
                        weftfs::tensor_store::WritePolicy::Auto { .. } => "auto",
                    },
                    "effective_shard_strategy": options.effective_strategy().as_str(),
                    "fast_codec": options.fast_codec.as_str(),
                    "fast_zstd_level": options.fast_zstd_level,
                    "causal_checkpoint": options.causal_checkpoint,
                    "tile_shape": options.tile_shape,
                    "total_element_bytes": report.total_element_bytes,
                    "shard_objects": report.shard_objects,
                    "prepare_ranks": parallel.as_ref().map(|options| options.ranks),
                    "verify_fragments": parallel.as_ref().map(|options| options.verify_fragments),
                    "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
                    "peak_rss_kib": peak_rss_kib(),
                    "tensors": report.tensors.iter().map(|tensor| json!({
                        "name": tensor.name,
                        "dtype": tensor.dtype,
                        "shape": tensor.shape,
                        "logical_root": tensor.logical_root.to_hex(),
                        "element_bytes": tensor.element_bytes,
                        "shards": tensor.shards,
                        "object_ids": tensor.object_ids.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                }))
            } else {
                println!("checkpoint={}", report.name);
                println!("checkpoint_id={}", report.checkpoint_id);
                println!("commit={}", report.head);
                println!("manifest_root={}", report.manifest_root);
                println!("source_sha256={}", hex::encode(report.source_sha256));
                println!("tensors={}", report.tensors.len());
                println!("shard_objects={}", report.shard_objects);
                println!("element_bytes={}", report.total_element_bytes);
                Ok(())
            }
        }
        CheckpointCommand::Restore {
            name,
            out,
            import,
            out_name,
            tensor,
            range,
            topology,
            axis,
            layout,
            block_shape,
            host_out,
            spool,
            load_receipt,
            verify_load,
            replica,
            first_needed,
            first_cache,
            abort_after_first,
        } => {
            let started = std::time::Instant::now();
            let load_options = weftfs::load_receipt::LoadReceiptOptions {
                replica: replica.clone(),
                verification: weftfs::load_receipt::LoadVerification::parse(&verify_load)?,
            };
            if load_receipt.is_some() && (tensor.is_some() || range.is_some()) {
                return Err(Error::invalid_argument(
                    "--load-receipt requires a complete checkpoint restore (no --tensor/--range)",
                ));
            }
            let restore_topology =
                build_restore_topology(topology, axis, layout.as_deref(), block_shape.as_deref())?;
            let region = match range.as_deref() {
                Some(text) => Some(parse_checkpoint_region(text)?),
                None => None,
            };
            if region.is_some() && tensor.is_none() {
                return Err(Error::invalid_argument(
                    "--range requires --tensor (one named tensor)",
                ));
            }
            if import && out.is_none() {
                return Err(Error::invalid_argument("--import requires --out STORE_DIR"));
            }
            if !import && (out.is_some() || out_name.is_some()) {
                return Err(Error::invalid_argument(
                    "--out/--out-name only apply to --import; restore is read-only in the store",
                ));
            }
            if !import && host_out.is_none() && spool.is_none() {
                return Err(Error::invalid_argument(
                    "a read-only restore needs --host-out FILE or --spool FILE ",
                ));
            }
            let options = CheckpointRestoreOptions {
                tensor: tensor.clone(),
                region,
                topology: restore_topology,
            };
            let spool_path = host_out
                .clone()
                .or_else(|| spool.clone())
                .unwrap_or_else(default_restore_spool);
            if let Some(parent) = spool_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)
                        .map_err(|err| Error::io(format!("{}: {err}", parent.display())))?;
                }
            }
            let mut emitted_receipt: Option<weftfs::load_receipt::LoadReceipt> = None;
            if let Some(first_needed_path) = &first_needed {
                if import {
                    return Err(Error::invalid_argument(
                        "--first-needed cannot be combined with --import",
                    ));
                }
                if tensor.is_some() || range.is_some() || load_receipt.is_some() {
                    return Err(Error::invalid_argument(
                        "--first-needed takes a complete restore (no --tensor/--range/--load-receipt)",
                    ));
                }
                let required = parse_first_needed(first_needed_path)?;
                let store = open_store(store_path, OpenMode::ReadOnly, policy.clone())?;
                let cache_dir = first_cache.clone();
                let mut file = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&spool_path)
                    .map_err(|err| Error::io(format!("{}: {err}", spool_path.display())))?;
                let outcome = store.restore_checkpoint_recovery_first(
                    &name,
                    &options,
                    &weftfs::recovery_first::RecoveryFirstOptions {
                        required,
                        cache_dir,
                        abort_after_required: abort_after_first,
                    },
                    &mut file,
                );
                let report = match outcome {
                    Ok(report) => report,
                    Err(err) => {
                        drop(file);
                        let _ = fs::remove_file(&spool_path);
                        return Err(err);
                    }
                };
                drop(file);
                if host_out.is_none() {
                    let _ = fs::remove_file(&spool_path);
                }
                if json {
                    print_json(&json!({
                        "checkpoint": report.checkpoint,
                        "manifest_root": report.manifest_root.to_hex(),
                        "file_sha256": hex::encode(report.file_sha256),
                        "exact_source_file": report.exact_source_file,
                        "file_bytes": report.file_bytes,
                        "required_order": report.required_order.iter().map(|(name, depth)| {
                            json!({"name": name, "dependency_depth": depth})
                        }).collect::<Vec<_>>(),
                        "cached_required": report.cached_required.iter().map(|item| json!({
                            "name": item.name,
                            "bytes": item.bytes,
                            "logical_root": item.logical_root.to_hex(),
                        })).collect::<Vec<_>>(),
                        "time_to_first_usable_ms": report.time_to_first_usable_ms,
                        "time_to_complete_ms": report.time_to_complete_ms,
                        "bytes_fetched_before_first_usable": report.bytes_fetched_before_first_usable,
                        "bytes_fetched_total": report.bytes_fetched_total,
                        "store_reads_before_first_usable": report.store_reads_before_first_usable,
                        "tensors": report.tensors.iter().map(|tensor| json!({
                            "name": tensor.name,
                            "bytes": tensor.bytes,
                            "full_root_verified": tensor.full_root_verified,
                            "sha256": hex::encode(tensor.sha256),
                        })).collect::<Vec<_>>(),
                        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
                        "peak_rss_kib": peak_rss_kib(),
                    }))
                } else {
                    println!("checkpoint={}", report.checkpoint);
                    println!("manifest_root={}", report.manifest_root);
                    println!("exact_source_file={}", report.exact_source_file);
                    println!(
                        "time_to_first_usable_ms={:.3}",
                        report.time_to_first_usable_ms
                    );
                    println!("time_to_complete_ms={:.3}", report.time_to_complete_ms);
                    println!(
                        "bytes_fetched_before_first_usable={}",
                        report.bytes_fetched_before_first_usable
                    );
                    println!("bytes_fetched_total={}", report.bytes_fetched_total);
                    println!("cached_required={}", report.cached_required.len());
                    Ok(())
                }
            } else {
                let report = {
                    let store = open_store(store_path, OpenMode::ReadOnly, policy.clone())?;
                    let file = fs::File::create(&spool_path)
                        .map_err(|err| Error::io(format!("{}: {err}", spool_path.display())))?;
                    let mut writer = std::io::BufWriter::new(file);
                    let outcome = match &load_receipt {
                        Some(path) => store
                            .restore_checkpoint_with_load_receipt(
                                &name,
                                &options,
                                &load_options,
                                &mut writer,
                            )
                            .and_then(|(report, receipt)| {
                                let bytes = receipt.encode()?;
                                fs::write(path, &bytes).map_err(|err| {
                                    Error::io(format!("{}: {err}", path.display()))
                                })?;
                                emitted_receipt = Some(receipt);
                                Ok(report)
                            }),
                        None => store.restore_checkpoint(&name, &options, &mut writer),
                    };
                    match outcome {
                        Ok(report) => {
                            writer.flush().map_err(|err| {
                                Error::io(format!("{}: {err}", spool_path.display()))
                            })?;
                            drop(writer);
                            report
                        }
                        Err(err) => {
                            drop(writer);
                            let _ = fs::remove_file(&spool_path);
                            return Err(err);
                        }
                    }
                };
                // Importing writes the restored container into the store namespace
                // and is opt-in: a plain restore only reads the store and writes the
                // host file.
                let imported = if import {
                    let out_dir = out.as_deref().unwrap_or_default();
                    let mut store = open_store(store_path, OpenMode::ReadWrite, policy)?;
                    let parent = ensure_store_dir(&mut store, out_dir)?;
                    let file_name = out_name
                        .clone()
                        .unwrap_or_else(|| format!("{name}.safetensors"));
                    let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
                    let mut attrs = BTreeMap::new();
                    attrs.insert(
                        "weft.checkpoint.name".to_owned(),
                        Value::Str(report.name.clone()),
                    );
                    attrs.insert(
                        "weft.checkpoint.manifest_root".to_owned(),
                        wu::hash_value(report.manifest_root),
                    );
                    attrs.insert(
                        "weft.checkpoint.file_sha256".to_owned(),
                        Value::Bytes(report.file_sha256.to_vec()),
                    );
                    if let Some(source) = report.source_sha256 {
                        attrs.insert(
                            "weft.checkpoint.source_sha256".to_owned(),
                            Value::Bytes(source.to_vec()),
                        );
                    }
                    attrs.insert(
                        "weft.checkpoint.exact".to_owned(),
                        Value::Bool(report.exact_source_file),
                    );
                    let input = fs::File::open(&spool_path)
                        .map_err(|err| Error::io(format!("{}: {err}", spool_path.display())))?;
                    let object_id = store.create_file_streaming(
                        parent,
                        file_name.as_bytes(),
                        std::io::BufReader::new(input),
                        0o644,
                        uid,
                        gid,
                        attrs,
                    )?;
                    let store_path_out = format!("{}/{}", out_dir.trim_end_matches('/'), file_name);
                    Some((object_id, store.generation(), store_path_out))
                } else {
                    None
                };
                if host_out.is_none() {
                    let _ = fs::remove_file(&spool_path);
                }
                if json {
                    print_json(&json!({
                        "checkpoint": report.name,
                        "manifest_root": report.manifest_root.to_hex(),
                        "file_sha256": hex::encode(report.file_sha256),
                        "source_sha256": report.source_sha256.map(hex::encode),
                        "exact_source_file": report.exact_source_file,
                        "header_reproduced": report.header_reproduced,
                        "file_bytes": report.file_bytes,
                        "imported": import,
                        "object_id": imported.as_ref().map(|(id, _, _)| id.to_hex()),
                        "generation": imported.as_ref().map(|(_, generation, _)| *generation),
                        "store_path": imported.as_ref().map(|(_, _, path)| path.clone()),
                        "host_out": host_out.as_ref().map(|path| path.display().to_string()),
                        "shards_read": report.shards_read,
                        "units_touched": report.units_touched,
                        "store_bytes_read": report.store_bytes_read,
                        "store_reads": report.store_reads,
                        "load_receipt": emitted_receipt.as_ref().map(|receipt| {
                            let mut fields = serde_json::Map::new();
                            for (key, value) in receipt.to_json_fields() {
                                fields.insert(key, value);
                            }
                            fields.insert(
                                "receipt_root".to_owned(),
                                json!(receipt.root().map(|root| root.to_hex()).unwrap_or_default()),
                            );
                            serde_json::Value::Object(fields)
                        }),
                        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
                        "peak_rss_kib": peak_rss_kib(),
                        "tensors": report.tensors.iter().map(|tensor| json!({
                            "name": tensor.name,
                            "dtype": tensor.dtype,
                            "shape": tensor.shape,
                            "full_shape": tensor.full_shape,
                            "full_logical_root": tensor.full_logical_root.to_hex(),
                            "restored_content_root": tensor.restored_content_root.to_hex(),
                            "bytes": tensor.bytes,
                            "shards_read": tensor.shards_read,
                            "units_touched": tensor.units_touched,
                            "full_root_verified": tensor.full_root_verified,
                            "shard_roots_verified": tensor.shard_roots_verified,
                            "topology_root": tensor.topology_root.map(|root| root.to_hex()),
                            "sha256": hex::encode(tensor.sha256),
                        })).collect::<Vec<_>>(),
                    }))
                } else {
                    println!("checkpoint={}", report.name);
                    println!("file_sha256={}", hex::encode(report.file_sha256));
                    println!("exact_source_file={}", report.exact_source_file);
                    println!("file_bytes={}", report.file_bytes);
                    println!("imported={import}");
                    if let Some((object_id, generation, store_path_out)) = &imported {
                        println!("object_id={object_id}");
                        println!("generation={generation}");
                        println!("store_path={store_path_out}");
                    }
                    println!("shards_read={}", report.shards_read);
                    println!("units_touched={}", report.units_touched);
                    println!("store_bytes_read={}", report.store_bytes_read);
                    Ok(())
                }
            }
        }
        CheckpointCommand::List => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let mut rows = Vec::new();
            for name in store.list_checkpoints()? {
                let manifest = store.checkpoint_manifest(&name)?;
                rows.push(json!({
                    "name": name,
                    "tensors": manifest.tensors.len(),
                    "element_bytes": manifest.element_bytes,
                    "complete": manifest.complete,
                }));
            }
            if json {
                print_json(&json!({"checkpoints": rows}))
            } else {
                for row in &rows {
                    println!(
                        "{} tensors={} element_bytes={} complete={}",
                        row["name"].as_str().unwrap_or_default(),
                        row["tensors"],
                        row["element_bytes"],
                        row["complete"]
                    );
                }
                Ok(())
            }
        }
        CheckpointCommand::Show { name } => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let manifest = store.checkpoint_manifest(&name)?;
            let root = manifest.root()?;
            if json {
                print_json(&json!({
                    "manifest_root": root.to_hex(),
                    "manifest": value_to_json(&manifest.to_value()),
                }))
            } else {
                println!("manifest_root={root}");
                println!("checkpoint={}", manifest.name);
                println!("tensors={}", manifest.tensors.len());
                println!("element_bytes={}", manifest.element_bytes);
                println!("complete={}", manifest.complete);
                for tensor in &manifest.tensors {
                    println!(
                        "tensor={} dtype={} shape={:?} shards={} root={}",
                        tensor.name,
                        tensor.dtype,
                        tensor.shape,
                        tensor.shards.len(),
                        tensor.logical_root
                    );
                }
                Ok(())
            }
        }
        CheckpointCommand::Verify { name } => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let verified = checkpoint::verify_checkpoint(&store, &name)?;
            if json {
                print_json(&json!({"checkpoint": name, "tensors_verified": verified}))
            } else {
                println!("checkpoint={name}");
                println!("tensors_verified={verified}");
                Ok(())
            }
        }
    }
}

/// `weftfs plan-parents`: price every earlier-version parent, optimize the
/// dependency graph, and optionally write the chosen graph.
#[allow(clippy::too_many_arguments)]
fn run_plan_parents(
    store_path: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    versions: Vec<String>,
    max_depth: u32,
    lambda_ns: u64,
    reads_per_version: u64,
    sla_ms: Option<u64>,
    storage_budget: Option<u64>,
    solver: String,
    apply: bool,
    out: Option<PathBuf>,
    scratch: Option<PathBuf>,
    keep_scratch: bool,
    shards: u32,
    axis: u32,
    shard_strategy: String,
    no_semantic: bool,
    max_ingest_bytes: u64,
    restore_overhead: String,
) -> Result<()> {
    if versions.is_empty() {
        return Err(Error::invalid_argument(
            "plan-parents needs at least one --versions NAME=FILE",
        ));
    }
    if shards != 1 {
        return Err(Error::invalid_argument(
            "plan-parents supports only --shards 1",
        ));
    }
    let strategy = ShardStrategy::parse(&shard_strategy)?;
    if strategy != ShardStrategy::Planned {
        return Err(Error::invalid_argument(
            "plan-parents requires --shard-strategy planned (only that strategy can choose a delta)",
        ));
    }
    if !matches!(policy.backend, CliBackend::Local) {
        return Err(Error::invalid_argument(
            "plan-parents is local-only: the probe store is a private local directory",
        ));
    }
    let mut sources: Vec<(String, PathBuf)> = Vec::with_capacity(versions.len());
    let mut seen = std::collections::BTreeSet::new();
    for spec in &versions {
        // Accept both repeated `--versions NAME=FILE` and one comma-separated
        // list, which is what the court scripts use.
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, path) = entry.split_once('=').ok_or_else(|| {
                Error::invalid_argument(format!(
                    "--versions entries must be NAME=FILE (got `{entry}`)"
                ))
            })?;
            if name.is_empty() || path.is_empty() {
                return Err(Error::invalid_argument(format!(
                    "--versions entries must be NAME=FILE (got `{entry}`)"
                )));
            }
            if !seen.insert(name.to_owned()) {
                return Err(Error::invalid_argument(format!(
                    "checkpoint name `{name}` appears twice in --versions"
                )));
            }
            sources.push((name.to_owned(), PathBuf::from(path)));
        }
    }
    if sources.is_empty() {
        return Err(Error::invalid_argument(
            "--versions did not name any checkpoint",
        ));
    }

    let target = out.clone().unwrap_or_else(|| store_path.clone());
    let scratch = scratch.unwrap_or_else(|| {
        let mut path = target.clone().into_os_string();
        path.push(".plan-parents-scratch");
        PathBuf::from(path)
    });
    if scratch == target {
        return Err(Error::invalid_argument(
            "--scratch must differ from the target store",
        ));
    }
    if scratch.exists() {
        return Err(Error::invalid_argument(format!(
            "scratch store {} already exists; remove it or choose --scratch",
            scratch.display()
        )));
    }
    if apply && target.exists() {
        return Err(Error::invalid_argument(format!(
            "--apply needs a new target store; {} already exists",
            target.display()
        )));
    }

    let write = CheckpointWriteOptions {
        shards,
        axis,
        strategy,
        write_policy: weftfs::tensor_store::WritePolicy::Planned,
        causal_checkpoint: None,
        semantic_search: !no_semantic,
        max_ingest_bytes,
        ..CheckpointWriteOptions::default()
    };

    let mut probe = Store::init(&scratch, policy.options())?;
    let probe_names: Vec<String> = (0..sources.len())
        .map(|index| format!("probe-{index:04}"))
        .collect();
    for (index, (_, path)) in sources.iter().enumerate() {
        probe.write_checkpoint(&probe_names[index], path, &write)?;
    }
    let price_options = weftfs::plan_graph::PriceOptions {
        plan: weftfs::tensor_store::TensorPlanOptions {
            semantic_search: !no_semantic,
            ..weftfs::tensor_store::TensorPlanOptions::default()
        },
        max_tensors: 200_000,
        shards,
    };
    let graph = probe.price_parent_graph(&probe_names, &price_options)?;
    drop(probe);

    let restore_overhead = match restore_overhead.as_str() {
        "measured" => Some(weftfs::cost_model::RestoreOverhead::MEASURED),
        "none" => None,
        other => {
            return Err(Error::invalid_argument(format!(
                "--restore-overhead must be `measured` or `none` (got `{other}`)"
            )))
        }
    };
    let plan_options = weftfs::plan_graph::PlanOptions {
        lambda_read_ns: lambda_ns,
        reads_per_version,
        max_depth,
        sla_ns: sla_ms.map(|ms| ms.saturating_mul(1_000_000)),
        storage_budget,
        solver: weftfs::plan_graph::Solver::parse(&solver)?,
        restore_overhead,
        ..weftfs::plan_graph::PlanOptions::default()
    };
    let plan = weftfs::plan_graph::optimize(&graph, &plan_options)?;

    // The chain/restart DP and the exhaustive oracle are both cheap here; the
    // report always shows both so a reader can see the DP's restriction.
    let mut dp_plan = None;
    let mut brute_plan = None;
    if graph.len() <= weftfs::plan_graph::DEFAULT_BRUTEFORCE_LIMIT {
        let mut dp_options = plan_options.clone();
        dp_options.solver = weftfs::plan_graph::Solver::Dp;
        dp_plan = weftfs::plan_graph::optimize(&graph, &dp_options).ok();
        let mut brute_options = plan_options.clone();
        brute_options.solver = weftfs::plan_graph::Solver::BruteForce;
        brute_plan = weftfs::plan_graph::optimize(&graph, &brute_options).ok();
    }

    let mut reports = Vec::new();
    if apply {
        let mut out_store = Store::init(&target, policy.options())?;
        reports = out_store.write_parent_graph(&sources, &plan, &write)?;
    }

    let output = if json {
        let version_rows: Vec<serde_json::Value> = graph
            .versions
            .iter()
            .enumerate()
            .map(|(index, version)| {
                json!({
                    "index": index,
                    "name": sources[index].0,
                    "logical_bytes": version.logical_bytes,
                    "independent_bytes": version.independent.bytes,
                    "independent_decode_ns": version.independent.decode_ns,
                    "parent": plan.parents[index].map(|p| p as i64).unwrap_or(-1),
                    "parent_name": plan.parents[index].map(|p| sources[p].0.clone()),
                    "edge_bytes": graph
                        .edge(index, plan.parents[index])
                        .map(|edge| edge.bytes)
                        .unwrap_or(0),
                    "edge_decode_ns": graph
                        .edge(index, plan.parents[index])
                        .map(|edge| edge.decode_ns)
                        .unwrap_or(0),
                    "chain_decode_ns": plan.predicted_chain_ns[index],
                    "depth": plan.depths[index],
                    "candidates": version.edges.iter().enumerate().map(|(j, edge)| json!({
                        "parent": j,
                        "parent_name": sources[j].0,
                        "compatible": edge.is_some(),
                        "bytes": edge.as_ref().map(|e| e.bytes),
                        "decode_ns": edge.as_ref().map(|e| e.decode_ns),
                        "coupled_tensors": edge.as_ref().map(|e| e.coupled_tensors),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        print_json(&json!({
            "schema": weftfs::plan_graph::VERSION_GRAPH_SCHEMA,
            "plan_schema": plan.schema,
            "store": target.display().to_string(),
            "applied": apply,
            "versions": version_rows,
            "plan": {
                "solver": plan.solver.as_str(),
                "parents": plan.parent_indices(),
                "predicted_bytes": plan.predicted_bytes,
                "predicted_decode_chain_ns": plan.predicted_decode_chain_ns,
                "predicted_overhead_ns": plan.predicted_overhead_ns,
                "predicted_margin_ns": plan.predicted_margin_ns,
                "predicted_chain_ns": plan.predicted_chain_ns,
                "predicted_sla_bound_ns": plan.predicted_sla_bound_ns,
                "predicted_total_decode_ns": plan.predicted_total_decode_ns.to_string(),
                "restore_overhead": plan.restore_overhead.map(|term| json!({
                    "calibration": plan.restore_overhead_calibration,
                    "per_version_ns": term.per_version_ns,
                    "per_tensor_ns": term.per_tensor_ns,
                    "per_mib_ns": term.per_mib_ns,
                })),
                "sla_margin": {
                    "calibration": plan.sla_margin_calibration,
                    "ns_per_mib": plan.restore_overhead.map(|term| term.margin_ns_per_mib),
                },
                "depth": plan.depths,
                "dependencies": plan.dependencies,
                "max_depth": plan.max_depth,
                "worst_restore_ns": plan.worst_restore_ns,
                "worst_sla_bound_ns": plan.worst_sla_bound_ns,
                "objective": plan.objective.to_string(),
                "sla_satisfied": plan.sla_satisfied,
                "budget_satisfied": plan.budget_satisfied,
            },
            "dp": dp_plan.as_ref().map(|p| json!({
                "parents": p.parent_indices(),
                "objective": p.objective.to_string(),
                "predicted_bytes": p.predicted_bytes,
                "worst_restore_ns": p.worst_restore_ns,
                "worst_sla_bound_ns": p.worst_sla_bound_ns,
            })),
            "brute_force": brute_plan.as_ref().map(|p| json!({
                "parents": p.parent_indices(),
                "objective": p.objective.to_string(),
                "predicted_bytes": p.predicted_bytes,
                "worst_restore_ns": p.worst_restore_ns,
                "worst_sla_bound_ns": p.worst_sla_bound_ns,
            })),
            "applied_checkpoints": reports.iter().map(|report| json!({
                "name": report.name,
                "commit": report.head.to_hex(),
                "manifest_id": report.manifest_id.to_hex(),
                "source_sha256": hex::encode(report.source_sha256),
                "shard_objects": report.shard_objects,
                "total_element_bytes": report.total_element_bytes,
            })).collect::<Vec<_>>(),
            "scratch": scratch.display().to_string(),
        }))
    } else {
        println!("schema={}", weftfs::plan_graph::VERSION_GRAPH_SCHEMA);
        println!("plan_schema={}", plan.schema);
        println!("solver={}", plan.solver.as_str());
        println!("versions={}", graph.len());
        for (index, version) in graph.versions.iter().enumerate() {
            let parent = plan.parents[index]
                .map(|p| sources[p].0.clone())
                .unwrap_or_else(|| "none".to_owned());
            println!(
                "version={index} name={} parent={parent} independent_bytes={} edge_bytes={} chain_ns={} depth={}",
                sources[index].0,
                version.independent.bytes,
                graph
                    .edge(index, plan.parents[index])
                    .map(|edge| edge.bytes)
                    .unwrap_or(0),
                plan.predicted_chain_ns[index],
                plan.depths[index]
            );
        }
        println!("predicted_bytes={}", plan.predicted_bytes);
        println!(
            "predicted_total_decode_ns={}",
            plan.predicted_total_decode_ns
        );
        println!("worst_restore_ns={}", plan.worst_restore_ns);
        println!("worst_sla_bound_ns={}", plan.worst_sla_bound_ns);
        match plan.restore_overhead {
            Some(term) => println!(
                "restore_overhead per_version_ns={} per_tensor_ns={} per_mib_ns={} calibration={} sla_margin_ns_per_mib={} sla_margin_calibration={}",
                term.per_version_ns,
                term.per_tensor_ns,
                term.per_mib_ns,
                plan.restore_overhead_calibration,
                term.margin_ns_per_mib,
                plan.sla_margin_calibration
            ),
            None => println!("restore_overhead=none"),
        }
        println!("max_depth={}", plan.max_depth);
        println!("dependencies={}", plan.dependencies);
        println!("objective={}", plan.objective);
        println!("sla_satisfied={}", plan.sla_satisfied);
        println!("budget_satisfied={}", plan.budget_satisfied);
        if let Some(dp) = &dp_plan {
            println!(
                "dp_parents={:?} dp_objective={} dp_bytes={} dp_worst_restore_ns={} dp_worst_sla_bound_ns={}",
                dp.parent_indices(),
                dp.objective,
                dp.predicted_bytes,
                dp.worst_restore_ns,
                dp.worst_sla_bound_ns
            );
        }
        if let Some(brute) = &brute_plan {
            println!(
                "brute_parents={:?} brute_objective={} brute_bytes={} brute_worst_restore_ns={} brute_worst_sla_bound_ns={}",
                brute.parent_indices(),
                brute.objective,
                brute.predicted_bytes,
                brute.worst_restore_ns,
                brute.worst_sla_bound_ns
            );
        }
        if apply {
            for report in &reports {
                println!("applied={} commit={}", report.name, report.head);
            }
        }
        Ok(())
    };
    output?;

    if !keep_scratch {
        let _ = fs::remove_dir_all(&scratch);
    }
    Ok(())
}

fn resolve_checkpoint_source(from_dir: Option<&Path>, file: Option<&Path>) -> Result<PathBuf> {
    match (from_dir, file) {
        (Some(_), Some(_)) => Err(Error::invalid_argument(
            "checkpoint write takes --from-dir or --file, not both",
        )),
        (None, None) => Err(Error::invalid_argument(
            "checkpoint write needs --from-dir DIR or --file FILE",
        )),
        (None, Some(file)) => Ok(file.to_path_buf()),
        (Some(dir), None) => {
            let mut found = Vec::new();
            for entry in
                fs::read_dir(dir).map_err(|err| Error::io(format!("{}: {err}", dir.display())))?
            {
                let path = entry
                    .map_err(|err| Error::io(format!("{}: {err}", dir.display())))?
                    .path();
                if path.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("safetensors")
                {
                    found.push(path);
                }
            }
            found.sort();
            match found.len() {
                1 => Ok(found.remove(0)),
                0 => Err(Error::not_found(format!(
                    "{} contains no .safetensors file",
                    dir.display()
                ))),
                count => Err(Error::invalid_argument(format!(
                    "{} contains {count} .safetensors files; select one with --file",
                    dir.display()
                ))),
            }
        }
    }
}

fn build_restore_topology(
    topology: Option<u32>,
    axis: u32,
    layout: Option<&str>,
    block_shape: Option<&str>,
) -> Result<Option<RestoreTopology>> {
    let topology = match layout {
        None | Some("none") => match topology {
            Some(shards) => Some(RestoreTopology::Sharded { axis, shards }),
            None => None,
        },
        Some("sharded") => {
            let shards = topology
                .ok_or_else(|| Error::invalid_argument("--layout sharded requires --topology N"))?;
            Some(RestoreTopology::Sharded { axis, shards })
        }
        Some("row-major") => {
            reject_axis_or_topology(topology)?;
            Some(RestoreTopology::RowMajor)
        }
        Some("column-major") => {
            reject_axis_or_topology(topology)?;
            Some(RestoreTopology::ColumnMajor)
        }
        Some("blocked") => {
            reject_axis_or_topology(topology)?;
            let block = block_shape.ok_or_else(|| {
                Error::invalid_argument("--layout blocked requires --block-shape B0,B1,...")
            })?;
            Some(RestoreTopology::Blocked {
                block_shape: parse_checkpoint_u64_list(block)?,
            })
        }
        Some(other) => {
            return Err(Error::invalid_argument(format!(
                "--layout must be row-major, column-major, blocked, or sharded (got `{other}`)"
            )))
        }
    };
    Ok(topology)
}

fn reject_axis_or_topology(topology: Option<u32>) -> Result<()> {
    if topology.is_some() {
        return Err(Error::invalid_argument(
            "--topology applies only to the sharded layout",
        ));
    }
    Ok(())
}

fn parse_checkpoint_u64_list(text: &str) -> Result<Vec<u64>> {
    text.split(',')
        .map(|part| {
            part.trim()
                .parse::<u64>()
                .map_err(|_| Error::invalid_argument(format!("`{}` is not a u64 list", text)))
        })
        .collect()
}

fn parse_checkpoint_region(text: &str) -> Result<weftcodec::Region> {
    let mut start = Vec::new();
    let mut end = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (from, to) = part.split_once(':').ok_or_else(|| {
            Error::invalid_argument(format!(
                "region `{text}` must be `s0:e0,s1:e1,...` (one pair per axis)"
            ))
        })?;
        start.push(
            from.trim().parse::<u64>().map_err(|_| {
                Error::invalid_argument(format!("region start `{from}` is not a u64"))
            })?,
        );
        end.push(
            to.trim()
                .parse::<u64>()
                .map_err(|_| Error::invalid_argument(format!("region end `{to}` is not a u64")))?,
        );
    }
    if start.is_empty() || start.len() != end.len() {
        return Err(Error::invalid_argument(format!(
            "region `{text}` must name at least one axis"
        )));
    }
    Ok(weftcodec::Region::new(start, end))
}

fn ensure_store_dir(store: &mut Store, path: &str) -> Result<Hash32> {
    let mut current = weftfs::state::State::ROOT_ID;
    for component in path.split('/') {
        if component.is_empty() {
            continue;
        }
        current = match store.lookup(current, component.as_bytes()) {
            Ok(id) => {
                if store.object_meta(id)?.kind != ObjectKind::Dir {
                    return Err(Error::not_dir(format!("{} is not a directory", path)));
                }
                id
            }
            Err(err) if err.class() == weftfs::error::Class::NotFound => {
                let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
                store.mkdir(current, component.as_bytes(), 0o755, uid, gid)?
            }
            Err(err) => return Err(err),
        };
    }
    Ok(current)
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
            })
        })
        .unwrap_or(0)
}

fn default_restore_spool() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "weft-checkpoint-restore-{}-{nanos}.safetensors",
        std::process::id()
    ))
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

/// Parse a `--first-needed` file: one tensor name per line, `#` comments,
/// blank lines ignored, duplicates collapsed.
fn parse_first_needed(path: &Path) -> Result<Vec<String>> {
    let text =
        fs::read_to_string(path).map_err(|err| Error::io(format!("{}: {err}", path.display())))?;
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if !names.iter().any(|name| name == line) {
            names.push(line.to_owned());
        }
    }
    if names.is_empty() {
        return Err(Error::invalid_argument(format!(
            "{}: --first-needed lists no tensor names",
            path.display()
        )));
    }
    Ok(names)
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

fn read_envelope_file(path: &Path) -> Result<CapabilityEnvelope> {
    let bytes = fs::read(path)
        .map_err(|err| Error::io(format!("cannot read envelope {}: {err}", path.display())))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::parse("capability envelope file is not UTF-8"))?;
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        Ok(CapabilityEnvelope::from_json_str(trimmed)?)
    } else {
        let raw = hex::decode(trimmed).map_err(|_| {
            Error::parse("capability envelope file is neither JSON nor canonical hex")
        })?;
        Ok(CapabilityEnvelope::from_canonical_bytes(
            &raw,
            &weftcodec::Limits::default(),
        )?)
    }
}

fn read_seed_file(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path)
        .map_err(|err| Error::io(format!("cannot read key {}: {err}", path.display())))?;
    if bytes.len() == 32 {
        return Ok(bytes);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::parse("key file must be 32 raw bytes or 64 hex digits"))?;
    let decoded = hex::decode(text.trim())
        .map_err(|_| Error::parse("key file must be 32 raw bytes or 64 hex digits"))?;
    if decoded.len() != 32 {
        return Err(Error::parse("Ed25519 seed must be 32 bytes"));
    }
    Ok(decoded)
}

fn read_revocations(path: &Option<PathBuf>) -> Result<Vec<Hash32>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text = fs::read_to_string(path)
        .map_err(|err| Error::io(format!("cannot read revocations {}: {err}", path.display())))?;
    let trimmed = text.trim();
    let mut out = Vec::new();
    if trimmed.starts_with('[') {
        let items: Vec<String> = serde_json::from_str(trimmed)
            .map_err(|err| Error::parse(format!("bad revocation JSON: {err}")))?;
        for item in items {
            out.push(Hash32::from_hex(item.trim())?);
        }
    } else {
        for item in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
            if item.is_empty() {
                continue;
            }
            out.push(Hash32::from_hex(item)?);
        }
    }
    Ok(out)
}

fn build_principal(
    uid: u64,
    gid: u64,
    groups: Vec<u64>,
    envelope_path: Option<&PathBuf>,
    revocations_path: Option<&PathBuf>,
    now_ns: i64,
) -> Result<Principal> {
    let mut principal = Principal::new(uid, gid, groups);
    if let Some(path) = envelope_path {
        let bytes = fs::read(path)
            .map_err(|err| Error::io(format!("cannot read envelope {}: {err}", path.display())))?;
        let revocations = read_revocations(&revocations_path.cloned())?;
        let verified = verify_envelope(&bytes, &revocations, now_ns, None)
            .map_err(|_| Error::unauthorized("capability envelope is not valid"))?;
        principal = principal.with_envelope(verified);
    }
    Ok(principal)
}

fn run_capability(
    store: &PathBuf,
    json_output: bool,
    policy: OpenPolicy,
    command: CapabilityCommand,
) -> Result<()> {
    match command {
        CapabilityCommand::Sign {
            envelope,
            key,
            out,
            attach,
            json: as_json,
        } => {
            let envelope_record = read_envelope_file(&envelope)?;
            let seed = read_seed_file(&key)?;
            let blob = sign_envelope(&envelope_record, &seed)?;
            let bytes = blob.encode()?;
            fs::write(&out, &bytes)
                .map_err(|err| Error::io(format!("cannot write {}: {err}", out.display())))?;
            let envelope_id = envelope_record.id()?;
            let blob_id = blob.id()?;
            let mut attached = false;
            if attach {
                if store.as_os_str().is_empty() {
                    return Err(Error::invalid_argument(
                        "capability sign --attach requires --store",
                    ));
                }
                let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
                let stored = store.save_capability_envelope(&blob)?;
                if stored != envelope_id {
                    return Err(Error::corrupt(
                        "stored envelope id does not match the signed envelope",
                    ));
                }
                attached = true;
            }
            let as_json = as_json || json_output;
            if as_json {
                print_json(&json!({
                    "envelope_id": envelope_id.to_hex(),
                    "blob_id": blob_id.to_hex(),
                    "tenant": envelope_record.tenant,
                    "grantor": envelope_record.grantor.to_hex(),
                    "grantee": envelope_record.grantee.to_hex(),
                    "expires_at_ns": envelope_record.expires_at_ns,
                    "revocation_id": envelope_record.revocation_id.to_hex(),
                    "signature": hex::encode(&blob.signature),
                    "out": out.display().to_string(),
                    "attached": attached,
                }))
            } else {
                println!("envelope_id={envelope_id}");
                println!("blob_id={blob_id}");
                println!("out={}", out.display());
                println!("attached={attached}");
                Ok(())
            }
        }
        CapabilityCommand::Verify {
            envelope,
            revocations,
            grantee,
            now_ns,
            json: as_json,
        } => {
            let bytes = fs::read(&envelope).map_err(|err| {
                Error::io(format!(
                    "cannot read envelope {}: {err}",
                    envelope.display()
                ))
            })?;
            let revocations = read_revocations(&revocations)?;
            let expected = match grantee {
                Some(text) => Some(Hash32::from_hex(text.trim())?),
                None => None,
            };
            let now = now_ns.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0)
            });
            let verified = verify_envelope(&bytes, &revocations, now, expected).map_err(|err| {
                Error::unauthorized(format!(
                    "capability envelope is not valid ({})",
                    err.rejection()
                ))
            })?;
            let as_json = as_json || json_output;
            if as_json {
                print_json(&json!({
                    "valid": true,
                    "envelope_id": verified.envelope_id.to_hex(),
                    "blob_id": verified.blob_id.to_hex(),
                    "tenant": verified.envelope.tenant,
                    "grantor": verified.envelope.grantor.to_hex(),
                    "grantee": verified.envelope.grantee.to_hex(),
                    "roots": verified.envelope.roots.iter().map(|h| h.to_hex()).collect::<Vec<_>>(),
                    "namespaces": verified.envelope.namespaces,
                    "predicates": verified.envelope.predicates,
                    "expires_at_ns": verified.envelope.expires_at_ns,
                    "revocation_id": verified.envelope.revocation_id.to_hex(),
                }))
            } else {
                println!("valid=true");
                println!("envelope_id={}", verified.envelope_id);
                println!("tenant={}", verified.envelope.tenant);
                println!("grantee={}", verified.envelope.grantee);
                Ok(())
            }
        }
        CapabilityCommand::Key { key } => {
            let seed = read_seed_file(&key)?;
            let public = public_key_for_seed(&seed)?;
            println!("{public}");
            Ok(())
        }
        CapabilityCommand::List { json: as_json } => {
            if store.as_os_str().is_empty() {
                return Err(Error::invalid_argument("capability list requires --store"));
            }
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let entries = store.list_capability_envelopes_at(store.current_state())?;
            let as_json = as_json || json_output;
            if as_json {
                let items: Vec<_> = entries
                    .iter()
                    .map(|item| {
                        json!({
                            "envelope_id": item.envelope_id.to_hex(),
                            "grantee": item.grantee.to_hex(),
                            "blob_id": item.blob_id.to_hex(),
                            "tenant": item.tenant,
                            "relation_id": item.relation_id.to_hex(),
                        })
                    })
                    .collect();
                print_json(&json!({"envelopes": items}))
            } else {
                for item in entries {
                    println!(
                        "{} {} {} {}",
                        item.envelope_id, item.grantee, item.tenant, item.blob_id
                    );
                }
                Ok(())
            }
        }
        CapabilityCommand::Show {
            envelope_id,
            revocations,
            now_ns,
            json: as_json,
        } => {
            if store.as_os_str().is_empty() {
                return Err(Error::invalid_argument("capability show requires --store"));
            }
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let id = Hash32::from_hex(envelope_id.trim())?;
            let revocations = read_revocations(&revocations)?;
            let now = now_ns.unwrap_or_else(|| store.clock_now_ns());
            let verified = store.load_capability_envelope(id, &revocations, now)?;
            let as_json = as_json || json_output;
            if as_json {
                print_json(&json!({
                    "envelope_id": verified.envelope_id.to_hex(),
                    "blob_id": verified.blob_id.to_hex(),
                    "envelope": verified.envelope.to_json(),
                }))
            } else {
                println!("envelope_id={}", verified.envelope_id);
                println!("blob_id={}", verified.blob_id);
                println!("tenant={}", verified.envelope.tenant);
                println!("grantee={}", verified.envelope.grantee);
                Ok(())
            }
        }
    }
}

/// Strict schema for `weftfs mount-many --spec`. Unknown fields are rejected so
/// a typo can never silently change what is mounted.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MountSpec {
    version: u32,
    views: Vec<MountSpecView>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MountSpecView {
    mountpoint: PathBuf,
    /// Snapshot name or commit hex. Absent means the state visible at open.
    #[serde(default)]
    snapshot: Option<String>,
    /// Context Root name or `weft://` URI; implies its pinned snapshot.
    #[serde(default)]
    context: Option<String>,
    /// Newest commit at or before this RFC 3339 / unix-ns time.
    #[serde(default)]
    at: Option<String>,
    /// Capability JSON or canonical-hex file narrowing this view.
    #[serde(default)]
    capability: Option<PathBuf>,
}

/// The only accepted `weftfs mount-many` spec schema.
const MOUNT_SPEC_VERSION: u32 = 1;

fn read_mount_spec(path: &Path) -> Result<MountSpec> {
    let text = fs::read_to_string(path)
        .map_err(|err| Error::io(format!("cannot read mount spec {}: {err}", path.display())))?;
    let spec: MountSpec = serde_json::from_str(&text)
        .map_err(|err| Error::invalid_argument(format!("mount spec {}: {err}", path.display())))?;
    if spec.version != MOUNT_SPEC_VERSION {
        return Err(Error::unsupported_version(format!(
            "mount spec {}: version {} (expected {MOUNT_SPEC_VERSION})",
            path.display(),
            spec.version
        )));
    }
    if spec.views.is_empty() {
        return Err(Error::invalid_argument(format!(
            "mount spec {}: no views",
            path.display()
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for view in &spec.views {
        if !seen.insert(view.mountpoint.clone()) {
            return Err(Error::invalid_argument(format!(
                "mount spec {}: mountpoint {} appears twice",
                path.display(),
                view.mountpoint.display()
            )));
        }
    }
    Ok(spec)
}

/// Replay `commit` at most once per shared-server start: every view pinned to
/// the same commit holds a clone of one `Arc<State>`.
fn cached_state(
    store: &Store,
    cache: &mut BTreeMap<Hash32, Arc<weftfs::state::State>>,
    commit: Hash32,
) -> Result<Arc<weftfs::state::State>> {
    if let Some(state) = cache.get(&commit) {
        return Ok(Arc::clone(state));
    }
    let state = Arc::new(store.state_at(commit)?);
    cache.insert(commit, Arc::clone(&state));
    Ok(state)
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
        ContextCommand::Branch {
            source,
            name,
            task,
            model,
            replace,
        } => {
            let mut store = open_store(&store, OpenMode::ReadWrite, policy)?;
            let stored = store.branch_context_root(
                &source,
                &name,
                task.as_deref(),
                model.as_deref(),
                replace,
            )?;
            if json {
                let mut output = context_root_json(&stored)?;
                output["generation"] = json!(store.generation());
                output["derived_from"] = json!(source);
                print_json(&output)
            } else {
                println!("context_root_id={}", stored.context_root_id);
                println!("uri={}", stored.uri);
                println!("snapshot_id={}", stored.root.snapshot);
                println!("derived_from={source}");
                Ok(())
            }
        }
        ContextCommand::List => {
            let store = open_store(&store, OpenMode::ReadOnly, policy)?;
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
            envelope,
            revocations,
            uid,
            gid,
            groups,
        } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let capability = read_capability(&capability_file)?;
            let (default_uid, default_gid) = current_ids();
            let principal = build_principal(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                groups,
                envelope.as_ref(),
                revocations.as_ref(),
                store.clock_now_ns(),
            )?;
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
            envelope,
            revocations,
            uid,
            gid,
            groups,
            require_one,
        } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let capability = read_capability(&capability_file)?;
            let (default_uid, default_gid) = current_ids();
            let principal = build_principal(
                uid.unwrap_or(default_uid),
                gid.unwrap_or(default_gid),
                groups,
                envelope.as_ref(),
                revocations.as_ref(),
                store.clock_now_ns(),
            )?;
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

#[allow(clippy::too_many_arguments)]
fn run_materialize(
    store_path: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    root: String,
    training_ready: bool,
    out: PathBuf,
    max_bytes: Option<u64>,
    max_objects: Option<u64>,
    max_decode_ms: Option<u64>,
    capability_file: Option<PathBuf>,
    envelope: Option<PathBuf>,
    revocations: Option<PathBuf>,
    require_tensors: bool,
    uid: Option<u64>,
    gid: Option<u64>,
    groups: Vec<u64>,
) -> Result<()> {
    if !training_ready {
        return Err(Error::invalid_argument(
            "`materialize` requires --training-ready; use `context materialize` for the plain \
             materialization command",
        ));
    }
    let started = std::time::Instant::now();
    let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
    let capability = read_capability(&capability_file)?;
    let (default_uid, default_gid) = current_ids();
    let principal = build_principal(
        uid.unwrap_or(default_uid),
        gid.unwrap_or(default_gid),
        groups,
        envelope.as_ref(),
        revocations.as_ref(),
        store.clock_now_ns(),
    )?;
    let options = TrainingReadyOptions {
        max_bytes,
        max_objects,
        max_decode_ms,
        require_tensors,
    };
    let report = store.materialize_training_ready(
        &root,
        &out,
        capability.as_ref(),
        &principal,
        &weftcodec::Limits::default(),
        store.clock_now_ns(),
        &options,
    )?;
    if json {
        let mut output = report.to_json();
        output["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        output["peak_rss_kib"] = json!(peak_rss_kib());
        print_json(&output)
    } else {
        println!(
            "out={}",
            report
                .out
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        println!(
            "manifest={}",
            report
                .manifest_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        println!("files={}", report.files.len());
        println!("bytes={}", report.cost.actual_bytes);
        println!("context_root_id={}", report.context_root_id);
        println!("snapshot_id={}", report.snapshot_id);
        println!("selector_id={}", report.selector_id);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn run_training_ready(
    store_path: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    root: Option<String>,
    check: bool,
    verify_out: Option<PathBuf>,
    max_bytes: Option<u64>,
    max_objects: Option<u64>,
    max_decode_ms: Option<u64>,
    capability_file: Option<PathBuf>,
    envelope: Option<PathBuf>,
    revocations: Option<PathBuf>,
    require_tensors: bool,
    uid: Option<u64>,
    gid: Option<u64>,
    groups: Vec<u64>,
) -> Result<()> {
    if let Some(out) = verify_out {
        if check || root.is_some() {
            return Err(Error::invalid_argument(
                "--verify-out cannot be combined with --root or --check",
            ));
        }
        let started = std::time::Instant::now();
        let report = verify_training_ready_output(&out)?;
        if json {
            let mut output = report.to_json();
            output["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            output["peak_rss_kib"] = json!(peak_rss_kib());
            print_json(&output)
        } else {
            println!("files={}", report.files);
            println!("bytes={}", report.bytes);
            println!("tensor_roots_verified={}", report.tensor_roots_verified);
            println!("checkpoints_verified={}", report.checkpoints_verified);
            println!(
                "checkpoint_tensors_verified={}",
                report.checkpoint_tensors_verified
            );
            Ok(())
        }
    } else {
        let root = root.ok_or_else(|| {
            Error::invalid_argument("--root is required with --check (or use --verify-out DIR)")
        })?;
        if !check {
            return Err(Error::invalid_argument(
                "`training-ready` writes nothing; pass --check to run the report",
            ));
        }
        let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
        let capability = read_capability(&capability_file)?;
        let started = std::time::Instant::now();
        let (default_uid, default_gid) = current_ids();
        let principal = build_principal(
            uid.unwrap_or(default_uid),
            gid.unwrap_or(default_gid),
            groups,
            envelope.as_ref(),
            revocations.as_ref(),
            store.clock_now_ns(),
        )?;
        let options = TrainingReadyOptions {
            max_bytes,
            max_objects,
            max_decode_ms,
            require_tensors,
        };
        let report = store.check_training_ready(
            &root,
            capability.as_ref(),
            &principal,
            &weftcodec::Limits::default(),
            store.clock_now_ns(),
            &options,
        )?;
        if json {
            let mut output = report.to_json();
            output["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            output["peak_rss_kib"] = json!(peak_rss_kib());
            print_json(&output)
        } else {
            println!("context_root_id={}", report.context_root_id);
            println!("snapshot_id={}", report.snapshot_id);
            println!("selector_id={}", report.selector_id);
            println!(
                "precondition.resolution_exact={}",
                report.preconditions.resolution_exact
            );
            println!(
                "precondition.content_verified={}",
                report.preconditions.content_verified
            );
            println!(
                "precondition.causal_complete={}",
                report.preconditions.causal_complete
            );
            println!(
                "precondition.authorized={}",
                report.preconditions.authorized
            );
            println!(
                "precondition.tensors_verified={}",
                report.preconditions.tensors_verified
            );
            println!(
                "precondition.budget_enforced={}",
                report.preconditions.budget_enforced
            );
            println!("objects={}", report.cost.actual_objects);
            println!("bytes={}", report.cost.actual_bytes);
            println!("estimated_bytes={}", report.cost.estimated_bytes);
            println!("estimated_objects={}", report.cost.estimated_objects);
            println!("estimated_decode_ms={}", report.cost.estimated_decode_ms);
            println!("actual_decode_ms={}", report.cost.actual_decode_ms);
            println!("tensor_objects_verified={}", report.tensor_objects_verified);
            println!(
                "checkpoint_tensors_verified={}",
                report.checkpoint_tensors_verified
            );
            for file in &report.files {
                println!("file={} {}", file.content_hash, file.path);
            }
            Ok(())
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

fn run_kv(store_path: &PathBuf, json: bool, policy: OpenPolicy, command: KvCommand) -> Result<()> {
    use weftfs::kv_prefix::{
        kv_attention_hash, kv_page_bytes, kv_page_len, kv_token_ids, kv_tokenizer_hash,
        KvPrefillModel, KvPrefixRequest, KvPrefixSpec,
    };

    let attention_text = |attention: &Option<String>, heads: u32, head_dim: u32, layers: u32| {
        attention
            .clone()
            .unwrap_or_else(|| format!("mha;heads={heads};head_dim={head_dim};layers={layers}"))
    };

    match command {
        KvCommand::Put {
            name,
            tenant,
            model,
            tokens,
            token_seed,
            page_rows,
            layers,
            heads,
            head_dim,
            dtype,
            tokenizer,
            attention,
            parent,
            pin,
            payload_seed,
            uid,
            gid,
        } => {
            let mut store = open_store(store_path, OpenMode::ReadWrite, policy)?;
            let uid = uid.unwrap_or(0);
            let gid = gid.unwrap_or(0);
            let token_ids = kv_token_ids(token_seed, tokens);
            if token_ids.len() as u64 != tokens {
                return Err(Error::invalid_argument(
                    "token length exceeds the prefix bound",
                ));
            }
            let from_page = match &parent {
                Some(parent_name) => {
                    let record = store.kv_prefix(parent_name)?;
                    if record.position_end > tokens {
                        return Err(Error::invalid_argument("--parent is longer than --tokens"));
                    }
                    record.page_count
                }
                None => 0,
            };
            let page_count = tokens.div_ceil(page_rows);
            let mut new_pages = Vec::new();
            for layer in 0..layers {
                let mut pages = Vec::new();
                for index in from_page..page_count {
                    let len =
                        kv_page_len(index, page_rows, tokens, heads, head_dim, &dtype)? as usize;
                    pages.push(kv_page_bytes(payload_seed, layer, index, len));
                }
                new_pages.push(pages);
            }
            let spec = KvPrefixSpec {
                tenant: tenant.clone(),
                model: model.clone(),
                tokenizer_hash: kv_tokenizer_hash(&tokenizer),
                attention_hash: kv_attention_hash(&attention_text(
                    &attention, heads, head_dim, layers,
                )),
                dtype: dtype.clone(),
                head_count: heads,
                head_dim,
                layer_count: layers,
                page_rows,
                pinned: pin,
            };
            let seed = weftfs::identity::h("weft/kv/prefix-seed/v1", &payload_seed.to_le_bytes());
            let write = store.kv_prefix_put_seeded(
                &name,
                &spec,
                &token_ids,
                parent.as_deref(),
                &new_pages,
                uid,
                gid,
                seed.as_bytes(),
            )?;
            if json {
                print_json(&json!({
                    "name": name,
                    "prefix": write.prefix_id.to_hex(),
                    "commit": write.commit.to_hex(),
                    "generation": store.generation(),
                    "tokens": write.record.position_end,
                    "page_rows": write.record.page_rows,
                    "page_count": write.record.page_count,
                    "layers": write.record.layer_count,
                    "page_bytes": write.record.page_bytes,
                    "stored_page_bytes": write.record.stored_page_bytes,
                    "reused_pages": write.record.reused_pages,
                    "new_pages": write.new_pages,
                    "pinned": write.record.pinned,
                    "parent": write.record.parent.map(|id| id.to_hex()),
                }))
            } else {
                println!("name={name}");
                println!("prefix={}", write.prefix_id);
                println!("commit={}", write.commit);
                println!("tokens={}", write.record.position_end);
                println!("page_count={}", write.record.page_count);
                println!("page_bytes={}", write.record.page_bytes);
                println!("stored_page_bytes={}", write.record.stored_page_bytes);
                println!("reused_pages={}", write.record.reused_pages);
                println!("new_pages={}", write.new_pages);
                Ok(())
            }
        }
        KvCommand::Get {
            name,
            envelope,
            revocations,
            now_ns,
            uid,
            gid,
        } => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let now = now_ns.unwrap_or_else(|| store.clock_now_ns());
            let principal = build_principal(
                uid.unwrap_or(0),
                gid.unwrap_or(0),
                Vec::new(),
                envelope.as_ref(),
                revocations.as_ref(),
                now,
            )?;
            let read = store.kv_prefix_get(&name, &principal, now)?;
            let mut pages = Vec::new();
            for page in &read.pages {
                pages.push(json!({
                    "layer": page.meta.layer,
                    "page_index": page.meta.page_index,
                    "seq_start": page.meta.seq_start,
                    "seq_end": page.meta.seq_end,
                    "bytes": page.bytes.len(),
                    "content": weftfs::identity::content_hash(&page.bytes).to_hex(),
                }));
            }
            if json {
                print_json(&json!({
                    "name": name,
                    "prefix": read.record.id()?.to_hex(),
                    "tokens": read.record.position_end,
                    "page_count": read.record.page_count,
                    "page_bytes": read.record.page_bytes,
                    "pages": pages,
                }))
            } else {
                println!("prefix={}", read.record.id()?);
                println!("tokens={}", read.record.position_end);
                println!("pages={}", read.pages.len());
                for page in &pages {
                    println!(
                        "page layer={} index={} seq={}..{} bytes={} content={}",
                        page["layer"],
                        page["page_index"],
                        page["seq_start"],
                        page["seq_end"],
                        page["bytes"],
                        page["content"]
                    );
                }
                Ok(())
            }
        }
        KvCommand::List { tenant } => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let entries = store.kv_prefix_list()?;
            let mut rows = Vec::new();
            for (entry_name, record) in entries {
                if let Some(filter) = &tenant {
                    if &record.tenant != filter {
                        continue;
                    }
                }
                rows.push(json!({
                    "name": entry_name,
                    "prefix": record.id()?.to_hex(),
                    "tenant": record.tenant,
                    "model": record.model,
                    "dtype": record.dtype,
                    "tokens": record.position_end,
                    "page_count": record.page_count,
                    "layers": record.layer_count,
                    "page_bytes": record.page_bytes,
                    "stored_page_bytes": record.stored_page_bytes,
                    "reused_pages": record.reused_pages,
                    "pinned": record.pinned,
                    "parent": record.parent.map(|id| id.to_hex()),
                }));
            }
            if json {
                print_json(&serde_json::Value::Array(rows))
            } else {
                for row in &rows {
                    println!(
                        "{} model={} tokens={} pages={} bytes={} stored={} reused={} pinned={}",
                        row["name"],
                        row["model"],
                        row["tokens"],
                        row["page_count"],
                        row["page_bytes"],
                        row["stored_page_bytes"],
                        row["reused_pages"],
                        row["pinned"]
                    );
                }
                Ok(())
            }
        }
        KvCommand::Delete { name } => {
            let mut store = open_store(store_path, OpenMode::ReadWrite, policy)?;
            store.kv_prefix_delete(&name)?;
            if json {
                print_json(&json!({"deleted": name}))
            } else {
                println!("deleted={name}");
                Ok(())
            }
        }
        KvCommand::Gc { apply } => {
            let mut store = open_store(store_path, OpenMode::ReadWrite, policy)?;
            let report = store.kv_gc(apply)?;
            if json {
                print_json(&json!({
                    "applied": report.applied,
                    "prefixes": report.prefixes,
                    "marked_pages": report.marked_pages,
                    "candidates": report.candidates,
                    "deleted": report.deleted,
                    "deleted_bytes": report.deleted_bytes,
                    "kept_pinned": report.kept_pinned,
                    "problems": report.problems,
                }))
            } else {
                println!("prefixes={}", report.prefixes);
                println!("marked_pages={}", report.marked_pages);
                println!("candidates={}", report.candidates.len());
                println!("kept_pinned={}", report.kept_pinned);
                if apply {
                    println!("deleted={}", report.deleted.len());
                    println!("deleted_bytes={}", report.deleted_bytes);
                } else {
                    for name in &report.candidates {
                        println!("candidate={name}");
                    }
                }
                for problem in &report.problems {
                    println!("problem={problem}");
                }
                Ok(())
            }
        }
        KvCommand::Plan {
            tenant,
            model,
            tokens,
            token_seed,
            page_rows,
            layers,
            heads,
            head_dim,
            dtype,
            tokenizer,
            attention,
            calibration,
        } => {
            let store = open_store(store_path, OpenMode::ReadOnly, policy)?;
            let prefill = match calibration {
                Some(path) => {
                    let bytes = fs::read(&path).map_err(|err| {
                        Error::io(format!("cannot read {}: {err}", path.display()))
                    })?;
                    KvPrefillModel::from_json(&bytes)?
                }
                None => KvPrefillModel::declared(),
            };
            let request = KvPrefixRequest {
                tenant,
                model,
                tokenizer_hash: kv_tokenizer_hash(&tokenizer),
                attention_hash: kv_attention_hash(&attention_text(
                    &attention, heads, head_dim, layers,
                )),
                dtype,
                head_count: heads,
                head_dim,
                layer_count: layers,
                page_rows,
                tokens: kv_token_ids(token_seed, tokens),
            };
            let decision = store.kv_decide_reuse(&request, &prefill)?;
            let value = json!({
                "reuse": decision.reuse,
                "reason": decision.reason,
                "prefix": decision.prefix_id.map(|id| id.to_hex()),
                "tokens": decision.tokens,
                "page_bytes": decision.page_bytes,
                "t_reuse_ns": decision.reuse_total_ns,
                "t_recompute_ns": decision.recompute_ns,
                "lookup_ns": decision.lookup_ns,
                "observed_lookup_ns": decision.observed_lookup_ns,
                "transfer_ns": decision.transfer_ns,
                "decode_ns": decision.decode_ns,
                "placement_ns": decision.placement_ns,
                "decode_cost_model": decision.decode_cost_model,
                "scanned": decision.scanned,
                "model": decision.model_row,
                "declared_model": prefill.label,
            });
            if json {
                print_json(&value)
            } else {
                println!("reuse={}", decision.reuse);
                println!("reason={}", decision.reason);
                println!("t_reuse_ns={}", decision.reuse_total_ns);
                println!("t_recompute_ns={}", decision.recompute_ns);
                println!("lookup_ns={}", decision.lookup_ns);
                println!("observed_lookup_ns={}", decision.observed_lookup_ns);
                println!("transfer_ns={}", decision.transfer_ns);
                println!("decode_ns={}", decision.decode_ns);
                println!("placement_ns={}", decision.placement_ns);
                Ok(())
            }
        }
    }
}

fn run_provenance(
    store: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    command: ProvenanceCommand,
) -> Result<()> {
    match command {
        ProvenanceCommand::Record { record, outputs } => {
            let mut store = open_store(store, OpenMode::ReadWrite, policy)?;
            let text = fs::read_to_string(&record)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", record.display())))?;
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|err| Error::parse(format!("bad provenance JSON: {err}")))?;
            let record = parse_provenance_record_json(&parsed)?;
            let mut output_ids = Vec::with_capacity(outputs.len());
            for target in &outputs {
                output_ids.push(find_object_by_identity(&store, target)?.id);
            }
            let id = store.record_provenance(&output_ids, &record)?;
            let ids: Vec<String> = output_ids.iter().map(|hash| hash.to_hex()).collect();
            if json {
                print_json(&json!({
                    "provenance_id": id.to_hex(),
                    "outputs": ids,
                    "generation": store.generation(),
                }))
            } else {
                println!("provenance_id={id}");
                for output in &ids {
                    println!("output={output}");
                }
                println!("generation={}", store.generation());
                Ok(())
            }
        }
        ProvenanceCommand::Show { target } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let object = find_object_by_identity(&store, &target)?;
            let records = store.provenance_records_of(object.id)?;
            if records.is_empty() {
                return Err(Error::not_found("object has no provenance record"));
            }
            if json {
                let items: Vec<_> = records
                    .iter()
                    .map(|(id, record)| {
                        json!({
                            "provenance_id": id.to_hex(),
                            "record": value_to_json(&record.to_value()),
                        })
                    })
                    .collect();
                print_json(&json!({
                    "object_id": object.id.to_hex(),
                    "content": object.content.to_hex(),
                    "records": items,
                }))
            } else {
                println!("object_id={}", object.id);
                println!("content={}", object.content);
                for (id, record) in &records {
                    println!("provenance_id={id}");
                    println!(
                        "record={}",
                        serde_json::to_string(&value_to_json(&record.to_value()))
                            .unwrap_or_default()
                    );
                }
                Ok(())
            }
        }
        ProvenanceCommand::Closure { target, depth } => {
            let store = open_store(store, OpenMode::ReadOnly, policy)?;
            let object = find_object_by_identity(&store, &target)?;
            let closure = store.provenance_closure(object.id, depth)?;
            if json {
                let nodes: Vec<_> = closure
                    .nodes
                    .iter()
                    .map(|node| {
                        json!({
                            "depth": node.depth,
                            "content": node.content.to_hex(),
                            "object": node.object.map(|hash| hash.to_hex()),
                            "role": node.role,
                            "provenance": node.provenance.map(|hash| hash.to_hex()),
                        })
                    })
                    .collect();
                print_json(&json!({
                    "target": closure.target.to_hex(),
                    "depth": closure.depth_limit,
                    "nodes": nodes,
                }))
            } else {
                println!("target={}", closure.target);
                println!("depth={}", closure.depth_limit);
                for node in &closure.nodes {
                    println!(
                        "node depth={} content={} object={} role={} provenance={}",
                        node.depth,
                        node.content,
                        node.object
                            .map(|hash| hash.to_hex())
                            .unwrap_or_else(|| "-".to_owned()),
                        node.role.as_deref().unwrap_or("-"),
                        node.provenance
                            .map(|hash| hash.to_hex())
                            .unwrap_or_else(|| "-".to_owned()),
                    );
                }
                Ok(())
            }
        }
    }
}

fn run_recover(
    store: &PathBuf,
    json: bool,
    policy: OpenPolicy,
    target: &str,
    costs: weftfs::RecoveryCosts,
) -> Result<()> {
    let store = open_store(store, OpenMode::ReadOnly, policy)?;
    let object = find_object_by_identity(&store, target)?;
    let candidates = store.recovery_candidates(object.id, costs)?;
    let choice = choose_recovery(object.content, &candidates).map_err(Error::from)?;
    if json {
        let rendered: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                json!({
                    "strategy": candidate.strategy.as_str(),
                    "name": candidate.name,
                    "available": candidate.available,
                    "determinism": candidate.determinism.as_str(),
                    "missing_inputs": candidate.missing_inputs.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                    "fetch_cost": candidate.fetch_cost,
                    "decode_cost": candidate.decode_cost,
                    "recompute_cost": candidate.recompute_cost,
                    "total_cost": candidate.total_cost(),
                    "eligible": candidate.rejection().is_none(),
                    "rejection": candidate.rejection().map(|reason| reason.to_string()),
                })
            })
            .collect();
        print_json(&json!({
            "object_id": object.id.to_hex(),
            "content": object.content.to_hex(),
            "strategy": choice.strategy.as_str(),
            "name": choice.name,
            "cost": choice.cost,
            "fetch_cost": choice.fetch_cost,
            "decode_cost": choice.decode_cost,
            "recompute_cost": choice.recompute_cost,
            "candidates": rendered,
        }))
    } else {
        println!("object_id={}", object.id);
        println!("content={}", object.content);
        println!("strategy={}", choice.strategy.as_str());
        println!("name={}", choice.name);
        println!("cost={}", choice.cost);
        println!("fetch_cost={}", choice.fetch_cost);
        println!("decode_cost={}", choice.decode_cost);
        println!("recompute_cost={}", choice.recompute_cost);
        Ok(())
    }
}

fn parse_provenance_record_json(value: &serde_json::Value) -> Result<ProvenanceRecord> {
    let map = value
        .as_object()
        .ok_or_else(|| Error::invalid_argument("provenance record must be a JSON object"))?;
    for key in map.keys() {
        match key.as_str() {
            "schema" | "operation" | "params" | "inputs" | "outputs" | "environment"
            | "determinism" | "principal" | "timestamp_ns" => {}
            other => {
                return Err(Error::invalid_argument(format!(
                    "unknown provenance record key `{other}`"
                )))
            }
        }
    }
    if let Some(schema) = map.get("schema") {
        let schema = schema
            .as_str()
            .ok_or_else(|| Error::invalid_argument("provenance schema must be text"))?;
        if schema != PROVENANCE_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "provenance schema `{schema}` is not `{PROVENANCE_SCHEMA}`"
            )));
        }
    }
    let operation = map
        .get("operation")
        .and_then(|item| item.as_str())
        .ok_or_else(|| Error::invalid_argument("provenance record needs text `operation`"))?
        .to_owned();
    let params = match map.get("params") {
        None => BTreeMap::new(),
        Some(value) => match wu::json_to_value(value)? {
            Value::Map(params) => params,
            _ => {
                return Err(Error::invalid_argument(
                    "provenance params must be an object",
                ))
            }
        },
    };
    let inputs = match map.get("inputs") {
        None => Vec::new(),
        Some(value) => {
            let items = value
                .as_array()
                .ok_or_else(|| Error::invalid_argument("provenance inputs must be an array"))?;
            let mut inputs = Vec::with_capacity(items.len());
            for item in items {
                let input = item
                    .as_object()
                    .ok_or_else(|| Error::invalid_argument("provenance input must be an object"))?;
                for key in input.keys() {
                    if !matches!(key.as_str(), "role" | "content" | "object") {
                        return Err(Error::invalid_argument(format!(
                            "unknown provenance input key `{key}`"
                        )));
                    }
                }
                let role = input
                    .get("role")
                    .and_then(|item| item.as_str())
                    .ok_or_else(|| Error::invalid_argument("provenance input needs text `role`"))?
                    .to_owned();
                let content = input
                    .get("content")
                    .and_then(|item| item.as_str())
                    .ok_or_else(|| {
                        Error::invalid_argument("provenance input needs hex `content`")
                    })?;
                let content = Hash32::from_hex(content)?;
                let object = match input.get("object") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(text)) => Some(Hash32::from_hex(text)?),
                    Some(_) => {
                        return Err(Error::invalid_argument(
                            "provenance input `object` must be hex or null",
                        ))
                    }
                };
                inputs.push(ProvenanceInput::new(role, content, object));
            }
            inputs
        }
    };
    let outputs = match map.get("outputs") {
        None => Vec::new(),
        Some(value) => {
            let items = value
                .as_array()
                .ok_or_else(|| Error::invalid_argument("provenance outputs must be an array"))?;
            let mut outputs = Vec::with_capacity(items.len());
            for item in items {
                let text = item
                    .as_str()
                    .ok_or_else(|| Error::invalid_argument("provenance output must be hex"))?;
                outputs.push(Hash32::from_hex(text)?);
            }
            outputs
        }
    };
    let environment = match map.get("environment") {
        Some(value) => {
            let env = value.as_object().ok_or_else(|| {
                Error::invalid_argument("provenance environment must be an object")
            })?;
            let tool = env
                .get("tool")
                .and_then(|item| item.as_str())
                .ok_or_else(|| Error::invalid_argument("provenance environment needs text `tool`"))?
                .to_owned();
            let version = env
                .get("version")
                .and_then(|item| item.as_str())
                .ok_or_else(|| {
                    Error::invalid_argument("provenance environment needs text `version`")
                })?
                .to_owned();
            let env_hash = match env.get("env_hash") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) => Some(Hash32::from_hex(text)?),
                Some(_) => {
                    return Err(Error::invalid_argument(
                        "provenance environment `env_hash` must be hex or null",
                    ))
                }
            };
            let mut fields = BTreeMap::new();
            for (key, value) in env {
                if ProvenanceEnvironment::RESERVED.contains(&key.as_str()) {
                    continue;
                }
                fields.insert(key.clone(), wu::json_to_value(value)?);
            }
            ProvenanceEnvironment {
                tool,
                version,
                env_hash,
                fields,
            }
        }
        None => {
            return Err(Error::invalid_argument(
                "provenance record needs an `environment` object",
            ))
        }
    };
    let determinism = Determinism::parse(
        map.get("determinism")
            .and_then(|item| item.as_str())
            .ok_or_else(|| Error::invalid_argument("provenance record needs text `determinism`"))?,
    )?;
    let principal = match map.get("principal") {
        Some(value) => {
            let principal = value
                .as_object()
                .ok_or_else(|| Error::invalid_argument("provenance principal must be an object"))?;
            let uid = principal
                .get("uid")
                .and_then(|item| item.as_u64())
                .ok_or_else(|| Error::invalid_argument("provenance principal needs u64 `uid`"))?;
            let gid = principal
                .get("gid")
                .and_then(|item| item.as_u64())
                .ok_or_else(|| Error::invalid_argument("provenance principal needs u64 `gid`"))?;
            ProvenancePrincipal::new(uid, gid)
        }
        None => ProvenancePrincipal::new(0, 0),
    };
    let timestamp_ns = map
        .get("timestamp_ns")
        .and_then(|item| item.as_i64())
        .ok_or_else(|| Error::invalid_argument("provenance record needs i64 `timestamp_ns`"))?;
    ProvenanceRecord::new(
        operation,
        params,
        inputs,
        outputs,
        environment,
        determinism,
        principal,
        timestamp_ns,
    )
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
