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

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::format::{
    decode_commit_file, decode_state, decode_superblock, Manifest, SuperblockSlot, SUPERBLOCK_BYTES,
};
use crate::identity::{self, Hash32};
use crate::state::{EntryRecord, ObjectKind, State};

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
        for chunk in &manifest.chunks {
            let path = object_path(root, chunk.hash);
            let metadata = fs::metadata(&path).map_err(|err| {
                Error::corrupt(format!("missing chunk {}: {err}", path.display()))
            })?;
            if metadata.len() != chunk.len {
                return Err(Error::corrupt(format!(
                    "chunk {} is {} bytes, manifest says {}",
                    path.display(),
                    metadata.len(),
                    chunk.len
                )));
            }
        }
    }
    Ok(())
}

/// Open-time recovery: select the highest valid published generation.
pub fn recover(root: &Path) -> Result<Recovery> {
    let view = read_superblock(root)?;
    let mut notes = view.notes;
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

    let mut candidates: Vec<u64> = scan_commit_files(root)?.keys().copied().collect();
    candidates.retain(|generation| *generation <= max_visible);
    candidates.sort_unstable_by(|a, b| b.cmp(a));

    let mut rejected = Vec::new();
    // A published superblock generation whose commit file is absent is a
    // rejection, not an invitation to invent a head.
    for slot_generation in slot_hashes.keys() {
        if !candidates.contains(slot_generation) {
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
        match load_state_at(root, loaded.hash, max_visible, true) {
            Ok((state, load_notes)) => {
                notes.extend(load_notes);
                return Ok(Recovery {
                    head: loaded.hash,
                    generation: loaded.generation,
                    state,
                    rejected,
                    notes,
                });
            }
            Err(err) => {
                notes.push(format!("generation {generation} rejected: {err}"));
                rejected.push(generation);
            }
        }
    }

    Err(Error::corrupt(format!(
        "no valid generation at or below {max_visible}; rejected {rejected:?}"
    )))
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

/// Return the first entry record whose target is `target` (for path cycling
/// checks in tests and CLI helpers).
pub fn find_entry_for(state: &State, target: Hash32) -> Option<&EntryRecord> {
    state.entries.values().find(|entry| entry.target == target)
}

/// Convert a WCB scalar to a display string (used by relation listing).
pub fn scalar_display(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::U64(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::Timestamp(ns) => format!("{ns}"),
        Value::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}
