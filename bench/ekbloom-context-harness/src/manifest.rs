//! The EKBLOOM-manifested certified decision table.
//!
//! Layout of the logical source encoded by the pinned crate:
//!
//! ```text
//! 0..4096          header (magic, versions, slot count, key-area offset)
//! 4096..            slot array: slot_count x 64 bytes, open-addressed on the
//!                   first 16 bytes of the request key
//! key_area_offset.. key area: entries x 208 bytes (144-byte key core +
//!                   32-byte page id + 32-byte page content root)
//! ```
//!
//! A lookup reads one slot (64 B). Only if the 16-byte prefix matches does it
//! read the 208-byte key area entry and compare the 144-byte core byte for
//! byte. A prefix match alone never returns a verdict.
//!
//! E reads through the crate's **bounded engine** (`Engine::read_into`) on an
//! in-memory `EkbloomBody`, not through the file-backed `OpenObject` dispatch:
//! the latter tries checkpoint replay before the directly-addressable core, and
//! the contract asks for the caller-buffer/resident path when the crate exposes
//! one. The `.ekb` file is still written for durability and its length is
//! reported; the retained accounting charges the manifestation once, at
//! `max(file_bytes, body.physical_bytes())`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ekbloom::engine::ReadyObject;
use ekbloom::{
    create_auto_within_budget, open as open_body, write_body, EkbloomBody, ManifestationBudget,
    ManifestationProfile, ReadRequest, Workload, WorkloadKind,
};

use crate::caches::Entry;
use crate::rule::{self, key_material, KEY_AREA_LEN, KEY_CORE_LEN};

pub const HEADER_BYTES: usize = 4096;
pub const SLOT_BYTES: usize = 64;
const MAGIC: &[u8; 8] = b"WKC1\0\0\0\0";
const MAX_PROBES: u32 = 64;

#[derive(Clone, Copy, Debug)]
pub struct ManifestHit {
    pub verdict: u8,
    pub reason: u16,
    pub page_id: [u8; 32],
    pub content: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct BuildReport {
    pub entries: u64,
    pub slot_count: u64,
    pub source_bytes: u64,
    pub file_bytes: u64,
    pub resident_bytes: u64,
    pub retained_bytes: u64,
    pub resident_surface: bool,
    pub build_ms: f64,
    pub profile: String,
}

pub struct ManifestTable {
    ready: ReadyObject,
    path: PathBuf,
    slot_count: u32,
    key_area_offset: u64,
    logical_len: u64,
    file_bytes: u64,
    resident_bytes: u64,
    resident_surface: bool,
    slot_buf: [u8; SLOT_BYTES],
    key_buf: [u8; KEY_AREA_LEN],
    pub reads: u64,
    pub read_bytes: u64,
    pub probes: u64,
}

fn workload() -> Workload {
    Workload {
        kind: WorkloadKind::UniformRandom,
        hot_prefix: None,
        sample: vec![(0, SLOT_BYTES as u64)],
    }
}

impl ManifestTable {
    /// Build from entries and publish atomically. The caller bounds `entries`
    /// so the source fits the declared budget; an over-budget body is returned
    /// as an error and the caller reduces the entry set.
    pub fn build(
        path: &Path,
        entries: &[Entry],
        budget_bytes: u64,
        generation: u64,
    ) -> Result<(ManifestTable, BuildReport), String> {
        let start = Instant::now();
        let mut sorted: Vec<Entry> = entries.to_vec();
        sorted.sort_by(|a, b| a.key.cmp(&b.key));
        sorted.dedup_by(|a, b| a.key == b.key);
        let n = sorted.len();
        let slot_count = ((n * 2).max(8)).next_power_of_two() as u32;
        let key_area_offset = (HEADER_BYTES + slot_count as usize * SLOT_BYTES) as u64;
        let source_bytes = key_area_offset as usize + n * KEY_AREA_LEN;
        let mut source = vec![0u8; source_bytes];

        source[0..8].copy_from_slice(MAGIC);
        source[8..12].copy_from_slice(&1u32.to_le_bytes());
        source[12..16].copy_from_slice(&slot_count.to_le_bytes());
        source[16..20].copy_from_slice(&(SLOT_BYTES as u32).to_le_bytes());
        source[20..24].copy_from_slice(&(KEY_AREA_LEN as u32).to_le_bytes());
        source[24..28].copy_from_slice(&(rule::RULE_VERSION as u32).to_le_bytes());
        source[28..36].copy_from_slice(&generation.to_le_bytes());
        source[36..44].copy_from_slice(&key_area_offset.to_le_bytes());

        for (ordinal, entry) in sorted.iter().enumerate() {
            let material = key_material(
                weftfs::Hash32::from_bytes(entry.subject),
                weftfs::Hash32::from_bytes(entry.contract),
                weftfs::Hash32::from_bytes(entry.snapshot),
                weftfs::Hash32::from_bytes(entry.authority),
                entry.epoch,
                weftfs::Hash32::from_bytes(entry.page_id),
                weftfs::Hash32::from_bytes(entry.content),
            );
            let key_off = key_area_offset as usize + ordinal * KEY_AREA_LEN;
            source[key_off..key_off + KEY_AREA_LEN].copy_from_slice(&material);

            let mut idx = (u64::from_le_bytes(entry.key[0..8].try_into().unwrap())
                % u64::from(slot_count)) as u32;
            for _ in 0..MAX_PROBES {
                let base = HEADER_BYTES + idx as usize * SLOT_BYTES;
                if source[base + 24] == 0 {
                    source[base..base + 16].copy_from_slice(&entry.key[0..16]);
                    source[base + 16..base + 24].copy_from_slice(&(ordinal as u64).to_le_bytes());
                    source[base + 24] = 1;
                    source[base + 25] = entry.verdict;
                    source[base + 26..base + 28].copy_from_slice(&entry.reason.to_le_bytes());
                    break;
                }
                idx = (idx + 1) % slot_count;
            }
        }

        let budget = ManifestationBudget {
            maximum_physical_bytes: budget_bytes,
        };
        let body = create_auto_within_budget(
            &source,
            ManifestationProfile::Sparse { interval: 1024 },
            &budget,
        )
        .map_err(|e| format!("ekbloom build: {e}"))?;
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        write_body(&tmp, &body).map_err(|e| format!("ekbloom write: {e}"))?;
        let file_bytes = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let resident_bytes = body.physical_bytes();
        let plan_budget = ManifestationBudget {
            maximum_physical_bytes: budget_bytes.max(resident_bytes),
        };
        let ready = ReadyObject::prepare_with(body, &plan_budget, &workload())
            .map_err(|e| format!("ekbloom plan: {e}"))?;
        let resident_surface = ready.engine().resident_surface(ready.body()).is_some();
        fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        let retained_bytes = file_bytes.max(resident_bytes);
        let table = ManifestTable {
            ready,
            path: path.to_path_buf(),
            slot_count,
            key_area_offset,
            logical_len: source_bytes as u64,
            file_bytes,
            resident_bytes,
            resident_surface,
            slot_buf: [0u8; SLOT_BYTES],
            key_buf: [0u8; KEY_AREA_LEN],
            reads: 0,
            read_bytes: 0,
            probes: 0,
        };
        let build_ms = start.elapsed().as_secs_f64() * 1e3;
        Ok((
            table,
            BuildReport {
                entries: n as u64,
                slot_count: u64::from(slot_count),
                source_bytes: source_bytes as u64,
                file_bytes,
                resident_bytes,
                retained_bytes,
                resident_surface,
                build_ms,
                profile: "sparse(K=1024)".to_owned(),
            },
        ))
    }

    /// Open an existing manifested table without rebuilding it.
    pub fn open_existing(path: &Path) -> Result<ManifestTable, String> {
        let body: EkbloomBody = open_body(path).map_err(|e| format!("ekbloom open: {e}"))?;
        let logical_len = body.logical_len;
        let resident_bytes = body.physical_bytes();
        let budget = ManifestationBudget {
            maximum_physical_bytes: resident_bytes.max(1),
        };
        let ready = ReadyObject::prepare_with(body, &budget, &workload())
            .map_err(|e| format!("ekbloom plan: {e}"))?;
        let resident_surface = ready.engine().resident_surface(ready.body()).is_some();
        let file_bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut header = [0u8; HEADER_BYTES];
        ready
            .engine()
            .read_into(
                ready.body(),
                ReadRequest {
                    offset: 0,
                    length: HEADER_BYTES as u64,
                },
                &mut header,
            )
            .map_err(|e| format!("ekbloom header read: {e}"))?;
        if &header[0..8] != MAGIC {
            return Err("manifest header magic mismatch".to_owned());
        }
        let slot_count = u32::from_le_bytes(header[12..16].try_into().unwrap());
        let key_area_offset = u64::from_le_bytes(header[36..44].try_into().unwrap());
        if slot_count == 0 || key_area_offset < HEADER_BYTES as u64 {
            return Err("manifest header is not a valid table".to_owned());
        }
        Ok(ManifestTable {
            ready,
            path: path.to_path_buf(),
            slot_count,
            key_area_offset,
            logical_len,
            file_bytes,
            resident_bytes,
            resident_surface,
            slot_buf: [0u8; SLOT_BYTES],
            key_buf: [0u8; KEY_AREA_LEN],
            reads: 0,
            read_bytes: 0,
            probes: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn physical_bytes(&self) -> u64 {
        self.file_bytes.max(self.resident_bytes)
    }

    pub fn logical_len(&self) -> u64 {
        self.logical_len
    }

    pub fn resident_surface(&self) -> bool {
        self.resident_surface
    }

    pub fn lookup(&mut self, request_key: &[u8; 32], core: &[u8]) -> Option<ManifestHit> {
        let mut idx = (u64::from_le_bytes(request_key[0..8].try_into().unwrap())
            % u64::from(self.slot_count)) as u32;
        for _ in 0..MAX_PROBES {
            self.probes += 1;
            let offset = HEADER_BYTES as u64 + u64::from(idx) * SLOT_BYTES as u64;
            if self
                .ready
                .engine()
                .read_into(
                    self.ready.body(),
                    ReadRequest {
                        offset,
                        length: SLOT_BYTES as u64,
                    },
                    &mut self.slot_buf,
                )
                .is_err()
            {
                return None;
            }
            self.reads += 1;
            self.read_bytes += SLOT_BYTES as u64;
            if self.slot_buf[24] == 0 {
                return None;
            }
            if self.slot_buf[0..16] != request_key[0..16] {
                idx = (idx + 1) % self.slot_count;
                continue;
            }
            let key_index = u64::from_le_bytes(self.slot_buf[16..24].try_into().unwrap());
            let key_off = self.key_area_offset + key_index * KEY_AREA_LEN as u64;
            if self
                .ready
                .engine()
                .read_into(
                    self.ready.body(),
                    ReadRequest {
                        offset: key_off,
                        length: KEY_AREA_LEN as u64,
                    },
                    &mut self.key_buf,
                )
                .is_err()
            {
                return None;
            }
            self.reads += 1;
            self.read_bytes += KEY_AREA_LEN as u64;
            if self.key_buf[0..KEY_CORE_LEN] != core[..KEY_CORE_LEN] {
                idx = (idx + 1) % self.slot_count;
                continue;
            }
            let mut page_id = [0u8; 32];
            page_id.copy_from_slice(&self.key_buf[144..176]);
            let mut content = [0u8; 32];
            content.copy_from_slice(&self.key_buf[176..208]);
            return Some(ManifestHit {
                verdict: self.slot_buf[25],
                reason: u16::from_le_bytes(self.slot_buf[26..28].try_into().unwrap()),
                page_id,
                content,
            });
        }
        None
    }

    /// Read every certified entry back out of the manifestation. Construction
    /// only; never on a request path.
    pub fn entries(&self) -> Result<Vec<Entry>, String> {
        let mut source = vec![0u8; self.logical_len as usize];
        self.ready
            .engine()
            .read_into(
                self.ready.body(),
                ReadRequest {
                    offset: 0,
                    length: self.logical_len,
                },
                &mut source,
            )
            .map_err(|e| format!("ekbloom full read: {e}"))?;
        let mut out = Vec::new();
        for slot in 0..self.slot_count as usize {
            let base = HEADER_BYTES + slot * SLOT_BYTES;
            if source[base + 24] == 0 {
                continue;
            }
            let key_index =
                u64::from_le_bytes(source[base + 16..base + 24].try_into().unwrap()) as usize;
            let key_off = self.key_area_offset as usize + key_index * KEY_AREA_LEN;
            let material = &source[key_off..key_off + KEY_AREA_LEN];
            let mut core = [0u8; KEY_CORE_LEN];
            core.copy_from_slice(&material[0..KEY_CORE_LEN]);
            let key = rule::request_key_from_core(&core)?;
            let mut entry = Entry {
                key: key.into_bytes(),
                page_id: [0u8; 32],
                content: [0u8; 32],
                verdict: source[base + 25],
                reason: u16::from_le_bytes(source[base + 26..base + 28].try_into().unwrap()),
                ..Entry::default()
            };
            entry.subject.copy_from_slice(&core[8..40]);
            entry.contract.copy_from_slice(&core[40..72]);
            entry.snapshot.copy_from_slice(&core[72..104]);
            entry.authority.copy_from_slice(&core[104..136]);
            entry.epoch = u64::from_le_bytes(core[136..144].try_into().unwrap());
            entry.page_id.copy_from_slice(&material[144..176]);
            entry.content.copy_from_slice(&material[176..208]);
            out.push(entry);
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out.dedup_by(|a, b| a.key == b.key);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry {
            key: [7u8; 32],
            subject: [1u8; 32],
            contract: [2u8; 32],
            page_id: [5u8; 32],
            content: [6u8; 32],
            snapshot: [3u8; 32],
            authority: [4u8; 32],
            epoch: 11,
            verdict: 1,
            reason: 9,
        }
    }

    fn material(e: &Entry) -> [u8; KEY_AREA_LEN] {
        key_material(
            weftfs::Hash32::from_bytes(e.subject),
            weftfs::Hash32::from_bytes(e.contract),
            weftfs::Hash32::from_bytes(e.snapshot),
            weftfs::Hash32::from_bytes(e.authority),
            e.epoch,
            weftfs::Hash32::from_bytes(e.page_id),
            weftfs::Hash32::from_bytes(e.content),
        )
    }

    /// The manifestation is an accelerator, never an authority: a slot whose
    /// 16-byte prefix matches but whose 144-byte core differs must return no
    /// verdict at all. This is the candidate-only rule the contract declares.
    #[test]
    fn a_prefix_candidate_never_returns_a_verdict() {
        let dir = std::env::temp_dir().join(format!(
            "ekbctx-manifest-prefix-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("table.ekb");
        let e = entry();
        let (mut table, _report) = ManifestTable::build(&path, &[e], 1 << 20, 1).expect("build");
        let m = material(&e);

        let hit = table
            .lookup(&e.key, &m[0..KEY_CORE_LEN])
            .expect("the exact key hits");
        assert_eq!(hit.verdict, e.verdict);
        assert_eq!(hit.reason, e.reason);

        let mut altered = m;
        altered[8] ^= 0xff;
        assert!(
            table.lookup(&e.key, &altered[0..KEY_CORE_LEN]).is_none(),
            "a different core with the same prefix must not return a verdict"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
