//! Recovery, commit-chain replay, and content verification helpers.
//!
//! Recovery never invents data and never deletes rejected generations. It reads
//! both superblock slots, selects the highest published generation, verifies
//! the commit file (CRC, hash, generation), walks the parent chain, applies ops
//! from the nearest valid checkpoint, and finally checks that every file object
//! in the resulting state has a valid manifest and existing chunk files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};
use crate::format::{
    decode_commit_file, decode_state, decode_superblock, Manifest, SuperblockSlot, SUPERBLOCK_BYTES,
};
use crate::identity::{self, Hash32};
use crate::state::{ObjectKind, State};

/// A parsed commit file.
#[derive(Clone, Debug)]
pub struct LoadedCommit {
    pub generation: u64,
    pub hash: Hash32,
    pub wcb_len: u64,
    pub commit: crate::format::Commit,
}

/// Result of the full open-time recovery pass.
#[derive(Clone, Debug)]
pub struct Recovery {
    pub head: Hash32,
    pub generation: u64,
    pub state: State,
    /// Generations that were selected as candidates and rejected.
    pub rejected: Vec<u64>,
    /// Human-readable, non-fatal recovery notes (torn slots, ignored
    /// checkpoints, …). Never a substitute for the rejected set.
    pub notes: Vec<String>,
    /// Highest commit generation present on disk (including unpublished files
    /// above the selected head). The directory-free fast path cannot enumerate
    /// the directory, so it reports the selected generation instead; the value
    /// is only used to advance `next_generation` on a writable open.
    pub highest_file_generation: u64,
}

/// Process-wide count of commit files decoded by [`load_commit_file`].
///
/// This is a diagnostic for tests and benchmarks that need to prove the fast
/// open path reads O(checkpoint interval) commits rather than every commit in
/// the store. It is not used for correctness.
static COMMIT_FILE_READS: AtomicU64 = AtomicU64::new(0);

/// Commit files decoded since the process started.
#[doc(hidden)]
pub fn commit_file_reads() -> u64 {
    COMMIT_FILE_READS.load(Ordering::Relaxed)
}

/// Reset the process-wide commit-file read counter.
#[doc(hidden)]
pub fn reset_commit_file_reads() {
    COMMIT_FILE_READS.store(0, Ordering::Relaxed);
}

/// How open-time recovery resolves the committed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayStrategy {
    /// Generation-addressed bounded replay, falling back to the full scan on
    /// any inconsistency.
    Bounded,
    /// Original full commit-directory scan and replay (bench comparison only).
    FullScan,
}

/// Superblock bytes plus per-slot outcomes.
pub struct SuperblockView {
    pub bytes: Vec<u8>,
    pub slots: [Option<SuperblockSlot>; 2],
    pub notes: Vec<String>,
}

/// Read and decode the superblock; zero slots are "empty", non-zero invalid
/// slots are recorded as notes.
pub fn read_superblock(root: &Path) -> Result<SuperblockView> {
    let path = root.join("superblock");
    let bytes = fs::read(&path).map_err(|err| {
        Error::corrupt(format!("cannot read superblock {}: {err}", path.display()))
    })?;
    if bytes.len() != SUPERBLOCK_BYTES {
        return Err(Error::corrupt(format!(
            "superblock is {} bytes, expected {SUPERBLOCK_BYTES}",
            bytes.len()
        )));
    }
    let decoded = decode_superblock(&bytes)?;
    let mut slots = [None, None];
    let mut notes = Vec::new();
    for (index, outcome) in decoded.into_iter().enumerate() {
        let raw =
            &bytes[index * crate::format::SLOT_BYTES..(index + 1) * crate::format::SLOT_BYTES];
        match outcome {
            Ok(slot) => slots[index] = Some(slot),
            Err(err) => {
                if raw.iter().any(|byte| *byte != 0) {
                    notes.push(format!("superblock slot {index} is invalid: {err}"));
                }
            }
        }
    }
    Ok(SuperblockView {
        bytes,
        slots,
        notes,
    })
}

/// Path to a content object.
pub fn object_path(root: &Path, hash: Hash32) -> PathBuf {
    let hex = hash.to_hex();
    root.join("objects").join(&hex[0..2]).join(hex)
}

/// Path to a manifest or checkpoint state manifest.
pub fn manifest_path(root: &Path, hash: Hash32) -> PathBuf {
    let hex = hash.to_hex();
    root.join("manifests").join(&hex[0..2]).join(hex)
}

/// Path to a commit file.
pub fn commit_path(root: &Path, generation: u64) -> PathBuf {
    root.join("commits").join(format!("{generation}.commit"))
}

/// Numeric generation of a commit filename, if it has the canonical shape.
fn parse_commit_filename(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".commit")?;
    if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    stem.parse::<u64>().ok()
}

/// Every commit file on disk keyed by generation. Filenames are canonical
/// zero-padded decimals; a non-canonical numeric name is ignored.
pub fn scan_commit_files(root: &Path) -> Result<BTreeMap<u64, PathBuf>> {
    let dir = root.join("commits");
    let mut out = BTreeMap::new();
    let entries = fs::read_dir(&dir)
        .map_err(|err| Error::corrupt(format!("cannot read {}: {err}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(name) => name,
            None => continue,
        };
        if let Some(generation) = parse_commit_filename(name) {
            out.insert(generation, entry.path());
        }
    }
    Ok(out)
}

/// Load and validate one commit file.
pub fn load_commit_file(path: &Path, expected_generation: Option<u64>) -> Result<LoadedCommit> {
    COMMIT_FILE_READS.fetch_add(1, Ordering::Relaxed);
    let bytes = fs::read(path)
        .map_err(|err| Error::corrupt(format!("cannot read {}: {err}", path.display())))?;
    let (wcb, commit) = decode_commit_file(&bytes)?;
    if let Some(expected) = expected_generation {
        if commit.generation != expected {
            return Err(Error::corrupt(format!(
                "commit file {} declares generation {} but the filename says {expected}",
                path.display(),
                commit.generation
            )));
        }
    }
    let hash = identity::commit_id(&wcb);
    if commit.id()? != hash {
        return Err(Error::corrupt("commit record re-encoding is not canonical"));
    }
    Ok(LoadedCommit {
        generation: commit.generation,
        hash,
        wcb_len: wcb.len() as u64,
        commit,
    })
}

/// Parse every commit file up to `max_generation` (inclusive), ignoring files
/// that fail to parse. Used for replay indexes.
pub fn commit_index(root: &Path, max_generation: u64) -> Result<BTreeMap<Hash32, LoadedCommit>> {
    let mut index = BTreeMap::new();
    for (generation, path) in scan_commit_files(root)? {
        if generation > max_generation {
            continue;
        }
        if let Ok(loaded) = load_commit_file(&path, Some(generation)) {
            index.insert(loaded.hash, loaded);
        }
    }
    Ok(index)
}

/// Read a blob and verify its domain-separated hash.
pub fn read_blob_verified(
    root: &Path,
    kind: BlobKind,
    hash: Hash32,
    domain: &str,
) -> Result<Vec<u8>> {
    let path = match kind {
        BlobKind::Object => object_path(root, hash),
        BlobKind::Manifest => manifest_path(root, hash),
    };
    let bytes = fs::read(&path)
        .map_err(|err| Error::corrupt(format!("missing blob {}: {err}", path.display())))?;
    let actual = identity::h(domain, &bytes);
    if actual != hash {
        return Err(Error::corrupt(format!(
            "blob {} fails its hash check (expected {hash}, got {actual})",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Blob namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobKind {
    Object,
    Manifest,
}

/// Read a file manifest by its manifest id.
pub fn read_manifest(root: &Path, manifest_id: Hash32) -> Result<Manifest> {
    let bytes = read_blob_verified(
        root,
        BlobKind::Manifest,
        manifest_id,
        identity::MANIFEST_DOMAIN,
    )?;
    let value = crate::wcb_util::decode(&bytes)?;
    Manifest::from_wcb(&value)
}

/// Read and verify a checkpoint state manifest.
pub fn read_state_manifest(root: &Path, manifest_id: Hash32) -> Result<State> {
    let bytes = read_blob_verified(
        root,
        BlobKind::Manifest,
        manifest_id,
        identity::MANIFEST_DOMAIN,
    )?;
    let value = crate::wcb_util::decode(&bytes)?;
    decode_state(&value)
}

/// Rebuild state by replaying the parent chain ending at `head_hash`.
///
/// When `validate_catalog` is true, every file object's manifest and chunk
/// files are checked for existence, hash, and length.
pub fn load_state_at(
    root: &Path,
    head_hash: Hash32,
    max_generation: u64,
    check_catalog: bool,
) -> Result<(State, Vec<String>)> {
    let index = commit_index(root, max_generation)?;
    let mut notes = Vec::new();

    // Walk parents from the head down to the first commit.
    let mut chain: Vec<LoadedCommit> = Vec::new();
    let mut cursor = head_hash;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(cursor) {
            return Err(Error::corrupt("commit parent chain contains a cycle"));
        }
        let loaded = index
            .get(&cursor)
            .ok_or_else(|| Error::corrupt("commit parent chain refers to a missing commit"))?
            .clone();
        let parent = loaded.commit.parent;
        chain.push(loaded);
        match parent {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    chain.reverse();

    for window in chain.windows(2) {
        if window[1].generation <= window[0].generation {
            return Err(Error::corrupt(
                "commit generations do not increase along the parent chain",
            ));
        }
        let expected_parent = window[0].hash;
        if window[1].commit.parent != Some(expected_parent) {
            return Err(Error::corrupt(
                "commit parent hash does not match the previous commit",
            ));
        }
    }
    if chain.first().map(|commit| commit.generation) != Some(1) {
        return Err(Error::corrupt(
            "commit chain does not start at generation 1",
        ));
    }
    if chain
        .first()
        .and_then(|commit| commit.commit.parent)
        .is_some()
    {
        return Err(Error::corrupt("generation 1 has a parent"));
    }

    let mut state = State {
        generation: 0,
        ..State::default()
    };
    for loaded in &chain {
        if let Some(checkpoint) = loaded.commit.checkpoint {
            match read_state_manifest(root, checkpoint) {
                Ok(checkpoint_state) => {
                    if checkpoint_state.generation != loaded.generation {
                        notes.push(format!(
                            "checkpoint {} in generation {} declares generation {}; ignored",
                            checkpoint, loaded.generation, checkpoint_state.generation
                        ));
                    } else {
                        state = checkpoint_state;
                        continue;
                    }
                }
                Err(err) => notes.push(format!(
                    "checkpoint {} in generation {} is invalid ({err}); replayed ops instead",
                    checkpoint, loaded.generation
                )),
            }
        }
        state.apply_ops(&loaded.commit.ops).map_err(|err| {
            Error::corrupt(format!(
                "generation {} ops cannot be applied: {err}",
                loaded.generation
            ))
        })?;
        state.generation = loaded.generation;
    }

    if check_catalog {
        validate_catalog(root, &state)?;
    }
    Ok((state, notes))
}

/// Generation-addressed bounded replay from `head_hash`.
///
/// Unlike [`load_state_at`], this never scans the commit directory and never
/// decodes more than the chain window between the head and the nearest usable
/// checkpoint:
///
/// 1. Read `commits/<head_generation>.commit` directly, require its declared
///    generation to equal the path generation, and require its identity hash to
///    equal `head_hash`.
/// 2. Walk `parent` down one generation at a time (`commits/<gen-1>.commit`),
///    requiring every loaded commit's hash to equal the child's declared parent
///    hash and every generation to be exactly one lower.
/// 3. Stop at the newest commit that carries a checkpoint whose state manifest
///    is readable and declares the same generation. Apply that checkpoint state
///    and then the commits above it exactly as [`load_state_at`] does.
/// 4. Validate the catalog (manifest identity, chunk existence and length).
///
/// Any surprise — a missing file, a generation that disagrees with its name, a
/// hash or parent mismatch, a non-contiguous generation, an unreadable or
/// mislabelled checkpoint — is returned as an error. The caller must then fall
/// back to the full [`load_state_at`] path; this function never accepts a
/// shorter or partial chain as a substitute.
///
/// The walk is bounded by `head_generation` steps (generations strictly
/// decrease by one) and by `max_generation`, so a hostile chain cannot force
/// unbounded work.
pub fn load_state_bounded(
    root: &Path,
    head_hash: Hash32,
    head_generation: u64,
    max_generation: u64,
) -> Result<(State, Vec<String>)> {
    if head_generation == 0 {
        return Err(Error::corrupt(
            "bounded replay requires a non-zero head generation",
        ));
    }
    if head_generation > max_generation {
        return Err(Error::corrupt(format!(
            "bounded replay head generation {head_generation} exceeds max_generation {max_generation}"
        )));
    }

    // Newest-first window. It holds only the head-to-checkpoint commits, so its
    // length is bounded by the checkpoint interval when a checkpoint exists.
    let mut chain: Vec<LoadedCommit> = Vec::new();
    let mut cursor_hash = head_hash;
    let mut cursor_generation = head_generation;
    let mut seen: BTreeSet<Hash32> = BTreeSet::new();
    let mut steps = 0u64;
    let mut checkpoint_state: Option<State> = None;
    let mut resumed_at_checkpoint: Option<u64> = None;

    loop {
        if !seen.insert(cursor_hash) {
            return Err(Error::corrupt("commit parent chain contains a cycle"));
        }
        let path = commit_path(root, cursor_generation);
        let loaded = load_commit_file(&path, Some(cursor_generation))?;
        if loaded.hash != cursor_hash {
            return Err(Error::corrupt(format!(
                "commit {} hashes to {} but the child or superblock declares {}",
                path.display(),
                loaded.hash,
                cursor_hash
            )));
        }
        steps += 1;
        if steps > head_generation {
            return Err(Error::corrupt(format!(
                "bounded commit walk exceeded its {head_generation}-generation bound"
            )));
        }

        if let Some(checkpoint) = loaded.commit.checkpoint {
            // A checkpoint that cannot be verified is not silently ignored
            // here: the caller replays through the full path instead.
            let state = read_state_manifest(root, checkpoint)?;
            if state.generation != loaded.generation {
                return Err(Error::corrupt(format!(
                    "checkpoint {checkpoint} in generation {} declares generation {}",
                    loaded.generation, state.generation
                )));
            }
            chain.push(loaded.clone());
            checkpoint_state = Some(state);
            resumed_at_checkpoint = Some(loaded.generation);
            break;
        }

        let parent = loaded.commit.parent;
        chain.push(loaded);
        match parent {
            Some(parent) => {
                cursor_hash = parent;
                cursor_generation = cursor_generation
                    .checked_sub(1)
                    .ok_or_else(|| Error::corrupt("commit chain descends below generation 1"))?;
            }
            None => break,
        }
    }
    chain.reverse();

    // Mirror the structural checks performed by `load_state_at` on the
    // materialised window. The window either starts at generation 1 or at a
    // verified checkpoint, so nothing below it is needed to replay state.
    for window in chain.windows(2) {
        if window[1].generation <= window[0].generation {
            return Err(Error::corrupt(
                "commit generations do not increase along the parent chain",
            ));
        }
        if window[1].commit.parent != Some(window[0].hash) {
            return Err(Error::corrupt(
                "commit parent hash does not match the previous commit",
            ));
        }
    }
    if checkpoint_state.is_none() {
        if chain.first().map(|commit| commit.generation) != Some(1) {
            return Err(Error::corrupt(
                "commit chain does not start at generation 1",
            ));
        }
        if chain
            .first()
            .and_then(|commit| commit.commit.parent)
            .is_some()
        {
            return Err(Error::corrupt("generation 1 has a parent"));
        }
    }

    let mut state = State {
        generation: 0,
        ..State::default()
    };
    let mut checkpoint_state = checkpoint_state;
    for loaded in &chain {
        if loaded.commit.checkpoint.is_some() {
            state = checkpoint_state
                .take()
                .expect("the only checkpoint in the window was validated");
            continue;
        }
        state.apply_ops(&loaded.commit.ops).map_err(|err| {
            Error::corrupt(format!(
                "generation {} ops cannot be applied: {err}",
                loaded.generation
            ))
        })?;
        state.generation = loaded.generation;
    }

    validate_catalog(root, &state)?;
    // Honest recovery note: a bounded open does not enumerate commit files
    // below the checkpoint. The checkpoint state is hash-verified and its
    // generation is matched, so the recovered state is exact for the head; but
    // a gap in older history is not detected here. `verify --full` still
    // enumerates every commit file and reports such gaps.
    let notes = resumed_at_checkpoint
        .map(|generation| {
            vec![format!(
                "bounded replay resumed from the checkpoint at generation {generation}; \
                 commit files below it were not enumerated"
            )]
        })
        .unwrap_or_default();
    Ok((state, notes))
}

/// Check final-state referential integrity and content-file existence.
///
/// This is the recovery-time check described in `WEFT-MODEL.md` §8 step 4. It
/// verifies manifest identity and chunk **existence and length**, not chunk
/// bytes; `verify --full` and materialization perform full hashing.
pub fn validate_catalog(root: &Path, state: &State) -> Result<()> {
    let root_object = state
        .objects
        .get(&State::ROOT_ID)
        .ok_or_else(|| Error::corrupt("namespace root object is missing"))?;
    if root_object.kind != ObjectKind::Dir {
        return Err(Error::corrupt("namespace root object is not a directory"));
    }

    for entry in state.entries.values() {
        let parent = state
            .objects
            .get(&entry.parent)
            .ok_or_else(|| Error::corrupt("entry parent object is missing"))?;
        if parent.kind != ObjectKind::Dir {
            return Err(Error::corrupt("entry parent is not a directory"));
        }
        if !state.objects.contains_key(&entry.target) {
            return Err(Error::corrupt("entry target object is missing"));
        }
    }

    for (object, _) in state.tips.iter() {
        if !state.objects.contains_key(object) {
            return Err(Error::corrupt("tip refers to a missing object"));
        }
    }

    for record in state.objects.values() {
        if record.kind != ObjectKind::File {
            continue;
        }
        let manifest = read_manifest(root, record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt(format!(
                "object {} size {} disagrees with manifest size {}",
                record.id, record.size, manifest.size
            )));
        }
        let mut seen = BTreeSet::new();
        validate_manifest_tree(root, record.content, 0, &mut seen)?;
    }
    Ok(())
}

/// Check that every physical blob a manifest needs is present with the right
/// length. Codec representations are walked through frames and dependencies.
fn validate_manifest_tree(
    root: &Path,
    manifest_id: Hash32,
    depth: usize,
    seen: &mut BTreeSet<Hash32>,
) -> Result<()> {
    if depth > crate::representation::MAX_CODEC_DEPENDENCY_DEPTH {
        return Err(Error::corrupt(format!(
            "manifest {manifest_id} exceeds the codec dependency depth bound"
        )));
    }
    if !seen.insert(manifest_id) {
        return Err(Error::corrupt(format!(
            "manifest {manifest_id} participates in a cycle"
        )));
    }
    let manifest = read_manifest(root, manifest_id)?;
    if let Some(rep) = &manifest.representation {
        if rep.kind == crate::format::RepresentationKind::Codec {
            let frame = rep
                .frame
                .ok_or_else(|| Error::corrupt("codec manifest has no frame id"))?;
            let path = manifest_path(root, frame);
            let metadata = fs::metadata(&path).map_err(|err| {
                Error::corrupt(format!("missing codec frame {}: {err}", path.display()))
            })?;
            if !metadata.is_file() {
                return Err(Error::corrupt(format!(
                    "codec frame {} is not a regular file",
                    path.display()
                )));
            }
            for dependency in &rep.dependencies {
                validate_manifest_tree(root, *dependency, depth + 1, seen)?;
            }
            return Ok(());
        }
    }
    for chunk in &manifest.chunks {
        let path = object_path(root, chunk.hash);
        let metadata = fs::metadata(&path)
            .map_err(|err| Error::corrupt(format!("missing chunk {}: {err}", path.display())))?;
        if metadata.len() != chunk.len {
            return Err(Error::corrupt(format!(
                "chunk {} is {} bytes, manifest says {}",
                path.display(),
                metadata.len(),
                chunk.len
            )));
        }
    }
    Ok(())
}

/// Open-time recovery: select the highest valid published generation.
///
/// The read-only open path first tries a directory-free, generation-addressed
/// replay ([`load_state_bounded`]). Any inconsistency falls back to the
/// original scan-based recovery below, so the fast path can only ever save
/// work, never accept a different state.
pub fn recover(root: &Path) -> Result<Recovery> {
    recover_inner(root, ReplayStrategy::Bounded, true)
}

/// Open-time recovery for a writable handle.
///
/// A writable open must know the highest commit-file generation on disk so
/// `next_generation` never reuses a rejected generation. That requires one
/// directory enumeration; the commit *decode* window is still bounded.
pub(crate) fn recover_for_write(root: &Path) -> Result<Recovery> {
    recover_inner(root, ReplayStrategy::Bounded, false)
}

/// Original full commit-directory recovery, kept for benchmark comparison.
#[doc(hidden)]
pub(crate) fn recover_full_scan(root: &Path) -> Result<Recovery> {
    recover_inner(root, ReplayStrategy::FullScan, false)
}

fn recover_inner(
    root: &Path,
    strategy: ReplayStrategy,
    allow_directory_free: bool,
) -> Result<Recovery> {
    let view = read_superblock(root)?;
    let mut notes = view.notes.clone();
    let mut valid_slots: Vec<SuperblockSlot> = view.slots.iter().flatten().copied().collect();
    if valid_slots.is_empty() {
        return Err(Error::corrupt("no valid superblock slot"));
    }
    valid_slots.sort_by_key(|slot| slot.generation);
    let max_visible = valid_slots.last().expect("non-empty").generation;
    let slot_hashes: BTreeMap<u64, Hash32> = valid_slots
        .iter()
        .map(|slot| (slot.generation, slot.commit))
        .collect();

    if allow_directory_free && strategy == ReplayStrategy::Bounded {
        match recover_directory_free(root, &view, &valid_slots, max_visible, &slot_hashes) {
            Ok(recovery) => return Ok(recovery),
            Err(err) => {
                // Do not reuse anything from the fast attempt: the scan-based
                // recovery is authoritative, including its rejected set and
                // notes. Record that the fallback happened.
                let mut recovery = recover_inner(root, ReplayStrategy::Bounded, false)?;
                recovery.notes.push(format!(
                    "open fast path fell back to full commit-directory recovery: {err}"
                ));
                return Ok(recovery);
            }
        }
    }

    let files = scan_commit_files(root)?;
    let highest_file_generation = files.keys().next_back().copied().unwrap_or(0);
    let mut candidates: Vec<u64> = files
        .keys()
        .copied()
        .filter(|generation| *generation <= max_visible)
        .collect();
    candidates.sort_unstable_by(|a, b| b.cmp(a));

    let mut rejected = Vec::new();
    // A published superblock generation whose commit file is absent is a
    // rejection, not an invitation to invent a head.
    for slot_generation in slot_hashes.keys() {
        if !files.contains_key(slot_generation) {
            notes.push(format!(
                "generation {slot_generation} rejected: published slot has no commit file"
            ));
            rejected.push(*slot_generation);
        }
    }
    for generation in candidates {
        let path = commit_path(root, generation);
        let loaded = match load_commit_file(&path, Some(generation)) {
            Ok(loaded) => loaded,
            Err(err) => {
                notes.push(format!("generation {generation} rejected: {err}"));
                rejected.push(generation);
                continue;
            }
        };
        if let Some(expected) = slot_hashes.get(&generation) {
            if *expected != loaded.hash {
                notes.push(format!(
                    "generation {generation} rejected: superblock hash {} does not match commit hash {}",
                    expected, loaded.hash
                ));
                rejected.push(generation);
                continue;
            }
        }
        if let Some(slot) = valid_slots
            .iter()
            .find(|slot| slot.generation == generation)
        {
            if slot.commit_len != loaded.wcb_len {
                notes.push(format!(
                    "generation {generation} rejected: superblock length {} does not match commit length {}",
                    slot.commit_len, loaded.wcb_len
                ));
                rejected.push(generation);
                continue;
            }
        }
        let replayed = match strategy {
            ReplayStrategy::Bounded => {
                match load_state_bounded(root, loaded.hash, loaded.generation, max_visible) {
                    Ok(value) => Some(value),
                    Err(err) => {
                        notes.push(format!(
                            "generation {generation}: bounded replay fell back to full scan: {err}"
                        ));
                        match load_state_at(root, loaded.hash, max_visible, true) {
                            Ok(value) => Some(value),
                            Err(err) => {
                                notes.push(format!("generation {generation} rejected: {err}"));
                                rejected.push(generation);
                                None
                            }
                        }
                    }
                }
            }
            ReplayStrategy::FullScan => match load_state_at(root, loaded.hash, max_visible, true) {
                Ok(value) => Some(value),
                Err(err) => {
                    notes.push(format!("generation {generation} rejected: {err}"));
                    rejected.push(generation);
                    None
                }
            },
        };
        if let Some((state, load_notes)) = replayed {
            notes.extend(load_notes);
            return Ok(Recovery {
                head: loaded.hash,
                generation: loaded.generation,
                state,
                rejected,
                notes,
                highest_file_generation,
            });
        }
    }

    Err(Error::corrupt(format!(
        "no valid generation at or below {max_visible}; rejected {rejected:?}"
    )))
}

/// Directory-free fast open: the highest published slot must be the head, its
/// commit file must exist and match the slot hash and length, and the bounded
/// chain replay must complete. Anything else returns an error so the caller
/// can fall back to the full directory scan.
fn recover_directory_free(
    root: &Path,
    view: &SuperblockView,
    valid_slots: &[SuperblockSlot],
    max_visible: u64,
    slot_hashes: &BTreeMap<u64, Hash32>,
) -> Result<Recovery> {
    // The scan-based path records a rejection for every published slot whose
    // commit file is missing. Reproducing that note without enumerating is not
    // possible, so fall back when it happens.
    for generation in slot_hashes.keys() {
        let path = commit_path(root, *generation);
        if !path.is_file() {
            return Err(Error::corrupt(format!(
                "published generation {generation} has no commit file"
            )));
        }
    }

    let path = commit_path(root, max_visible);
    let loaded = load_commit_file(&path, Some(max_visible))?;
    if let Some(expected) = slot_hashes.get(&max_visible) {
        if *expected != loaded.hash {
            return Err(Error::corrupt(format!(
                "superblock hash for generation {max_visible} does not match the commit record"
            )));
        }
    }
    if let Some(slot) = valid_slots
        .iter()
        .find(|slot| slot.generation == max_visible)
    {
        if slot.commit_len != loaded.wcb_len {
            return Err(Error::corrupt(format!(
                "superblock length for generation {max_visible} does not match the commit record"
            )));
        }
    }

    let (state, load_notes) =
        load_state_bounded(root, loaded.hash, loaded.generation, max_visible)?;
    let mut notes = view.notes.clone();
    notes.extend(load_notes);
    Ok(Recovery {
        head: loaded.hash,
        generation: loaded.generation,
        state,
        rejected: Vec::new(),
        notes,
        highest_file_generation: loaded.generation,
    })
}

/// List every valid parsed commit file, for `verify` and GC roots.
pub fn all_commits(root: &Path, max_generation: u64) -> Result<Vec<LoadedCommit>> {
    let mut out = Vec::new();
    for (generation, path) in scan_commit_files(root)? {
        if generation > max_generation {
            continue;
        }
        if let Ok(loaded) = load_commit_file(&path, Some(generation)) {
            out.push(loaded);
        }
    }
    out.sort_by_key(|commit| commit.generation);
    Ok(out)
}
