//! The arms: W evaluates every request; M caches the decision; E adds the
//! EKBLOOM-manifested certified table; C-naive is the deliberately insufficient
//! sensitivity control.

use std::collections::HashMap;
use std::path::PathBuf;

use weftcodec::wcb;
use weftcodec::Limits;
use weftfs::identity::Hash32;
use weftfs::store::Store;

use crate::caches::{Cache, CacheStats, Entry};
use crate::fixture::{Fixture, Request};
use crate::manifest::{ManifestTable, HEADER_BYTES};
use crate::rule::{
    contract_root, key_material, request_key_from_core, Attention, AuthorityRecord, DecisionRecord,
    Dimensions, Evidence, SubjectKind, SubjectRecord, Verdict, KEY_AREA_LEN, KEY_CORE_LEN,
    R_EVIDENCE, R_EXACT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmKind {
    W,
    M,
    E,
    Naive,
}

impl ArmKind {
    pub fn parse(text: &str) -> Result<ArmKind, String> {
        match text {
            "W" | "w" => Ok(ArmKind::W),
            "M" | "m" => Ok(ArmKind::M),
            "E" | "e" => Ok(ArmKind::E),
            "NAIVE" | "naive" | "C-naive" => Ok(ArmKind::Naive),
            other => Err(format!("unknown arm `{other}`")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ArmKind::W => "W",
            ArmKind::M => "M",
            ArmKind::E => "E",
            ArmKind::Naive => "C-naive",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ArmStats {
    pub requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub manifest_hits: u64,
    pub manifest_misses: u64,
    pub evaluations: u64,
    pub stale_rejections: u64,
    pub nodes: u64,
    pub backend_bytes: u64,
    pub backend_requests: u64,
    pub allocations: u64,
    pub alloc_bytes: u64,
    pub manifest_reads: u64,
    pub manifest_read_bytes: u64,
    pub decision_records_built: u64,
    pub false_accepts: u64,
    pub semantic_mismatches: u64,
    pub manifest_builds: u64,
    pub manifest_build_failures: u64,
    pub manifest_build_ms: f64,
    pub manifest_bytes: u64,
    pub pending_bytes: u64,
    pub event_re_evaluations: u64,
    pub event_stale_accepts: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    pub verdict: u8,
    pub reason: u16,
    pub nodes: u64,
    pub cache_hit: bool,
    pub manifest_hit: bool,
    pub evaluation: bool,
    pub stale_rejections: u64,
    pub manifest_reads: u64,
    pub manifest_read_bytes: u64,
}

pub struct Arm {
    pub kind: ArmKind,
    pub variant: Option<String>,
    cache: Option<Cache>,
    manifest: Option<ManifestTable>,
    manifest_path: Option<PathBuf>,
    pending: Vec<Entry>,
    work: PathBuf,
    generation: u64,
    max_manifest_entries: usize,
    pub stats: ArmStats,
}

const PENDING_FLUSH: usize = 128;
const MANIFEST_CAP: u64 = crate::fixture::BUDGET_BYTES / 2;

impl Arm {
    pub fn new(
        kind: ArmKind,
        variant: Option<String>,
        work: &std::path::Path,
    ) -> Result<Arm, String> {
        let budget = crate::fixture::BUDGET_BYTES;
        let cache = match kind {
            ArmKind::W => None,
            ArmKind::M => Some(Cache::new(variant.as_deref().unwrap_or("lru"), budget)?),
            ArmKind::E => Some(Cache::new(variant.as_deref().unwrap_or("lru"), budget)?),
            ArmKind::Naive => Some(Cache::new("lru", budget)?),
        };
        Ok(Arm {
            kind,
            variant,
            cache,
            manifest: None,
            manifest_path: None,
            pending: Vec::new(),
            work: work.to_path_buf(),
            generation: 0,
            max_manifest_entries: 1_600,
            stats: ArmStats::default(),
        })
    }

    pub fn retained_bytes(&self) -> u64 {
        let cache = self.cache.as_ref().map(|c| c.retained_bytes()).unwrap_or(0);
        let manifest = self
            .manifest
            .as_ref()
            .map(|m| m.physical_bytes())
            .unwrap_or(0);
        let pending = self.pending.len() as u64 * 256;
        cache + manifest + pending
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.as_ref().map(|c| c.stats()).unwrap_or_default()
    }

    fn post_check(
        &self,
        store: &Store,
        entry: &Entry,
        snapshot: Hash32,
        authority_root: Hash32,
        epoch: u64,
    ) -> bool {
        if entry.snapshot != *snapshot.as_bytes() {
            return false;
        }
        if entry.authority != *authority_root.as_bytes() || entry.epoch != epoch {
            return false;
        }
        match store.object_meta(Hash32::from_bytes(entry.page_id)) {
            Ok(record) => record.content == Hash32::from_bytes(entry.content),
            Err(_) => false,
        }
    }

    pub fn handle(
        &mut self,
        store: &Store,
        fixture: &Fixture,
        contracts: &[Vec<Dimensions>],
        contract_roots: &[Vec<Hash32>],
        authorities: &[AuthorityRecord],
        authority_roots: &[Hash32],
        request: &Request,
    ) -> Result<Outcome, String> {
        self.stats.requests += 1;
        let subject = &fixture.subjects[request.subject as usize];
        let branch = subject.branch;
        let contract = &contracts[branch][request.slot as usize];
        let contract_root = contract_roots[branch][request.slot as usize];
        let authority = &authorities[branch];
        let authority_root = authority_roots[branch];
        let snapshot = store.snapshot_root();
        let core = key_material(
            subject.scope_root,
            contract_root,
            snapshot,
            authority_root,
            authority.epoch,
            subject.page_id,
            subject.content_root,
        );
        let request_key = request_key_from_core(&core)?;

        if self.kind == ArmKind::Naive {
            // Insufficient key: subject + contract only. No snapshot, no
            // authority, no post-lookup re-check beyond presence.
            let mut naive_material = Vec::with_capacity(64);
            naive_material.extend_from_slice(subject.scope_root.as_bytes());
            naive_material.extend_from_slice(contract_root.as_bytes());
            let naive_key = weftcodec::hash::hash_domain(
                "weft/kv/exact-prefix/naive-key/v1",
                &naive_material,
            );
            if let Some(cache) = self.cache.as_mut() {
                if let Some(entry) = cache.get(naive_key.as_bytes()) {
                    self.stats.cache_hits += 1;
                    let present = store
                        .object_meta(Hash32::from_bytes(entry.page_id))
                        .map(|r| r.content == Hash32::from_bytes(entry.content))
                        .unwrap_or(false);
                    if present {
                        return Ok(Outcome {
                            verdict: entry.verdict,
                            reason: entry.reason,
                            nodes: 1,
                            cache_hit: true,
                            manifest_hit: false,
                            evaluation: false,
                            stale_rejections: 0,
                            manifest_reads: 0,
                            manifest_read_bytes: 0,
                        });
                    }
                }
                self.stats.cache_misses += 1;
            }
            let outcome = self.evaluate(
                store,
                subject,
                request_key,
                contract,
                contract_root,
                authority,
                authority_root,
                snapshot,
            )?;
            let mut entry = Entry {
                key: *naive_key.as_bytes(),
                verdict: outcome.verdict,
                reason: outcome.reason,
                ..Entry::default()
            };
            entry.subject.copy_from_slice(subject.scope_root.as_bytes());
            entry.contract.copy_from_slice(contract_root.as_bytes());
            entry.page_id.copy_from_slice(subject.page_id.as_bytes());
            entry
                .content
                .copy_from_slice(subject.content_root.as_bytes());
            entry.snapshot.copy_from_slice(snapshot.as_bytes());
            entry.authority.copy_from_slice(authority_root.as_bytes());
            entry.epoch = authority.epoch;
            if let Some(cache) = self.cache.as_mut() {
                cache.insert(entry);
            }
            return Ok(outcome);
        }

        // 1. Conventional cache.
        if let Some(cache) = self.cache.as_mut() {
            if let Some(entry) = cache.get(&request_key.into_bytes()) {
                if self.post_check(store, &entry, snapshot, authority_root, authority.epoch) {
                    self.stats.cache_hits += 1;
                    return Ok(Outcome {
                        verdict: entry.verdict,
                        reason: entry.reason,
                        nodes: 1,
                        cache_hit: true,
                        manifest_hit: false,
                        evaluation: false,
                        stale_rejections: 0,
                        manifest_reads: 0,
                        manifest_read_bytes: 0,
                    });
                }
                self.stats.stale_rejections += 1;
            }
            self.stats.cache_misses += 1;
        }

        // 2. Manifested certified table (E only).
        if self.kind == ArmKind::E {
            if let Some(manifest) = self.manifest.as_mut() {
                let hit = manifest.lookup(request_key.as_bytes(), &core);
                let req_manifest_reads = manifest.reads;
                let req_manifest_bytes = manifest.read_bytes;
                self.stats.manifest_reads += req_manifest_reads;
                self.stats.manifest_read_bytes += req_manifest_bytes;
                manifest.reads = 0;
                manifest.read_bytes = 0;
                if let Some(hit) = hit {
                    let entry = Entry {
                        key: request_key.into_bytes(),
                        verdict: hit.verdict,
                        reason: hit.reason,
                        page_id: hit.page_id,
                        content: hit.content,
                        snapshot: *snapshot.as_bytes(),
                        authority: *authority_root.as_bytes(),
                        epoch: authority.epoch,
                        ..Entry::default()
                    };
                    if self.post_check(store, &entry, snapshot, authority_root, authority.epoch) {
                        self.stats.manifest_hits += 1;
                        return Ok(Outcome {
                            verdict: hit.verdict,
                            reason: hit.reason,
                            nodes: 3,
                            cache_hit: false,
                            manifest_hit: true,
                            evaluation: false,
                            stale_rejections: 0,
                            manifest_reads: req_manifest_reads,
                            manifest_read_bytes: req_manifest_bytes,
                        });
                    }
                    self.stats.stale_rejections += 1;
                }
                self.stats.manifest_misses += 1;
            }
        }

        // 3. Evaluate.
        let outcome = self.evaluate(
            store,
            subject,
            request_key,
            contract,
            contract_root,
            authority,
            authority_root,
            snapshot,
        )?;
        self.stats.evaluations += 1;
        let entry = Entry {
            key: request_key.into_bytes(),
            subject: *subject.scope_root.as_bytes(),
            contract: *contract_root.as_bytes(),
            page_id: *subject.page_id.as_bytes(),
            content: *subject.content_root.as_bytes(),
            snapshot: *snapshot.as_bytes(),
            authority: *authority_root.as_bytes(),
            epoch: authority.epoch,
            verdict: outcome.verdict,
            reason: outcome.reason,
        };
        if let Some(cache) = self.cache.as_mut() {
            cache.insert(entry);
        }
        if self.kind == ArmKind::E {
            self.pending.push(entry);
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate(
        &mut self,
        store: &Store,
        subject: &crate::fixture::Subject,
        request_key: Hash32,
        contract: &Dimensions,
        contract_root: Hash32,
        authority: &AuthorityRecord,
        authority_root: Hash32,
        snapshot: Hash32,
    ) -> Result<Outcome, String> {
        let mut nodes = 0u64;
        // The subject scope record is the resolver's evidence for every
        // dimension. A read failure is `unproven`, never a guess.
        let bytes = match store.read_object(subject.scope_object_id) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Ok(outcome(
                    Verdict::Unproven,
                    R_EVIDENCE,
                    nodes,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                ))
            }
        };
        nodes += 1;
        let value = match wcb::decode(&bytes, &Limits::default()) {
            Ok(value) => value,
            Err(_) => {
                return Ok(outcome(
                    Verdict::Unproven,
                    R_EVIDENCE,
                    nodes,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                ))
            }
        };
        let record = match SubjectRecord::from_value(&value) {
            Ok(record) => record,
            Err(_) => {
                return Ok(outcome(
                    Verdict::Unproven,
                    R_EVIDENCE,
                    nodes,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                ))
            }
        };
        // The record must describe exactly this subject. A mismatch is bad
        // evidence, not a partial match.
        if record.page_id != subject.page_id
            || record.descriptor_id != subject.descriptor_id
            || record.provenance_root != subject.provenance_root
            || record.kind != subject.kind
        {
            return Ok(outcome(
                Verdict::Unproven,
                R_EVIDENCE,
                nodes,
                false,
                false,
                true,
                0,
                0,
                0,
            ));
        }

        // Authorization precedes the dimension comparison. The authority record
        // is part of the key; the current record is read from the arm's table.
        nodes += 1;
        let tenant = match &record.dims.tenant {
            Some(tenant) => tenant,
            None => {
                return Ok(outcome(
                    Verdict::Unproven,
                    crate::rule::R_MISSING_TENANT,
                    nodes,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                ))
            }
        };
        if !authority.allows(subject.scope_root, tenant) {
            return Ok(outcome(
                Verdict::Denied,
                crate::rule::R_AUTH_REFUSED,
                nodes,
                false,
                false,
                true,
                0,
                0,
                0,
            ));
        }

        // Evidence: the page object and, for a KV page, its shipped descriptor.
        match subject.kind {
            SubjectKind::KvProv => {
                nodes += 2;
                match store.kv_prov_descriptor(subject.page_id) {
                    Ok(descriptor) => {
                        if descriptor.provenance_root != subject.provenance_root {
                            return Ok(outcome(
                                Verdict::Unproven,
                                R_EVIDENCE,
                                nodes,
                                false,
                                false,
                                true,
                                0,
                                0,
                                0,
                            ));
                        }
                    }
                    Err(_) => {
                        return Ok(outcome(
                            Verdict::Unproven,
                            R_EVIDENCE,
                            nodes,
                            false,
                            false,
                            true,
                            0,
                            0,
                            0,
                        ))
                    }
                }
            }
            SubjectKind::TensorRegion => {
                nodes += 1;
                match store.object_meta(subject.page_id) {
                    Ok(meta) => {
                        if meta.content != subject.content_root {
                            return Ok(outcome(
                                Verdict::Unproven,
                                R_EVIDENCE,
                                nodes,
                                false,
                                false,
                                true,
                                0,
                                0,
                                0,
                            ));
                        }
                    }
                    Err(_) => {
                        return Ok(outcome(
                            Verdict::Unproven,
                            R_EVIDENCE,
                            nodes,
                            false,
                            false,
                            true,
                            0,
                            0,
                            0,
                        ))
                    }
                }
            }
        }

        let (verdict, reason) = match Dimensions::first_difference(contract, &record.dims) {
            None => (Verdict::Exact, R_EXACT),
            Some((verdict, reason)) => (verdict, reason),
        };

        // Build the canonical decision record. This is the object a reader can
        // verify by recomputation; the court stores its root nowhere hot, but
        // building it is the cost of certification.
        let decision = DecisionRecord {
            request_key,
            verdict,
            reason,
            evidence: vec![
                Evidence {
                    kind: "subject_scope",
                    root: subject.scope_root,
                },
                Evidence {
                    kind: "page_content",
                    root: subject.content_root,
                },
                Evidence {
                    kind: "codec_descriptor",
                    root: subject.provenance_root,
                },
                Evidence {
                    kind: "authority",
                    root: authority_root,
                },
                Evidence {
                    kind: "consumer_contract",
                    root: contract_root,
                },
            ],
            snapshot,
            authority_epoch: authority.epoch,
        };
        if decision.root().is_err() {
            return Ok(outcome(
                Verdict::Unproven,
                R_EVIDENCE,
                nodes,
                false,
                false,
                true,
                0,
                0,
                0,
            ));
        }
        self.stats.decision_records_built += 1;
        Ok(outcome(verdict, reason, nodes, false, false, true, 0, 0, 0))
    }

    /// Rebuild the manifested table when the pending set reaches the declared
    /// flush threshold. Runs between requests, never inside a timed one.
    pub fn maybe_rebuild(&mut self) -> Result<(), String> {
        if self.kind != ArmKind::E || self.pending.len() < PENDING_FLUSH {
            return Ok(());
        }
        self.rebuild_manifest()
    }

    pub fn rebuild_manifest(&mut self) -> Result<(), String> {
        self.generation += 1;
        let start = std::time::Instant::now();
        let mut merged: HashMap<[u8; 32], Entry> = HashMap::new();
        // Hot resident decisions first.
        if let Some(cache) = self.cache.as_ref() {
            let mut entries = cache.entries();
            entries.sort_by(|a, b| a.key.cmp(&b.key));
            for entry in entries {
                merged.insert(entry.key, entry);
            }
        }
        for entry in &self.pending {
            merged.insert(entry.key, *entry);
        }
        if let Some(manifest) = self.manifest.as_ref() {
            match manifest.entries() {
                Ok(entries) => {
                    for entry in entries {
                        merged.entry(entry.key).or_insert(entry);
                    }
                }
                Err(_) => {
                    self.stats.manifest_build_failures += 1;
                }
            }
        }
        let mut entries: Vec<Entry> = merged.into_values().collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        let mut limit = self.max_manifest_entries.min(entries.len());
        let mut attempt = 0;
        loop {
            if limit == 0 {
                self.stats.manifest_build_failures += 1;
                self.stats.manifest_build_ms += start.elapsed().as_secs_f64() * 1e3;
                self.pending.truncate(PENDING_FLUSH);
                return Ok(());
            }
            let candidate = &entries[..limit];
            let path = self.work.join(format!("manifest-g{}.ekb", self.generation));
            match ManifestTable::build(&path, candidate, MANIFEST_CAP, self.generation) {
                Ok((table, report)) => {
                    let old_path = self.manifest_path.replace(path.clone());
                    self.manifest = Some(table);
                    let manifest_bytes = report.retained_bytes;
                    self.stats.manifest_builds += 1;
                    self.stats.manifest_build_ms += start.elapsed().as_secs_f64() * 1e3;
                    self.stats.manifest_bytes = manifest_bytes;
                    self.max_manifest_entries = self.max_manifest_entries.max(limit);
                    self.pending.clear();
                    let pending_bytes = 0u64;
                    self.stats.pending_bytes = pending_bytes;
                    if let Some(cache) = self.cache.as_mut() {
                        // A 10% arena reserve absorbs the hash map's capacity
                        // rounding so E's total retained bytes stay at or below
                        // M's, not merely below the declared budget.
                        let mut budget = crate::fixture::BUDGET_BYTES
                            .saturating_sub(manifest_bytes)
                            .saturating_sub(pending_bytes)
                            .saturating_mul(90)
                            / 100;
                        cache.resize(budget);
                        for _ in 0..8 {
                            if cache.retained_bytes() <= budget {
                                break;
                            }
                            budget = budget.saturating_mul(95) / 100;
                            cache.resize(budget);
                        }
                    }
                    if let Some(old) = old_path {
                        let _ = std::fs::remove_file(old);
                    }
                    return Ok(());
                }
                Err(_) => {
                    attempt += 1;
                    if attempt >= 8 {
                        self.stats.manifest_build_failures += 1;
                        self.stats.manifest_build_ms += start.elapsed().as_secs_f64() * 1e3;
                        self.pending.truncate(PENDING_FLUSH);
                        return Ok(());
                    }
                    limit = limit * 3 / 4;
                }
            }
        }
    }

    pub fn pending_bytes(&self) -> u64 {
        self.pending.len() as u64 * 256
    }

    pub fn manifest_path(&self) -> Option<PathBuf> {
        self.manifest_path.clone()
    }

    pub fn manifest_resident_surface(&self) -> bool {
        self.manifest
            .as_ref()
            .map(|m| m.resident_surface())
            .unwrap_or(false)
    }
}

#[allow(clippy::too_many_arguments)]
fn outcome(
    verdict: Verdict,
    reason: u16,
    nodes: u64,
    cache_hit: bool,
    manifest_hit: bool,
    evaluation: bool,
    stale_rejections: u64,
    manifest_reads: u64,
    manifest_read_bytes: u64,
) -> Outcome {
    Outcome {
        verdict: verdict.as_u8(),
        reason,
        nodes,
        cache_hit,
        manifest_hit,
        evaluation,
        stale_rejections,
        manifest_reads,
        manifest_read_bytes,
    }
}

/// Unused constant guard: a manifest entry is 64 + 208 bytes, so the build
/// estimate and the byte accounting stay tied to the rule constants.
pub fn manifest_entry_bytes() -> u64 {
    (HEADER_BYTES as u64).saturating_sub(HEADER_BYTES as u64)
        + crate::manifest::SLOT_BYTES as u64
        + KEY_AREA_LEN as u64
}

pub fn key_core_len() -> usize {
    KEY_CORE_LEN
}

/// A re-export for the scale phase's own table construction.
pub fn dims_root(dims: &Dimensions) -> Result<Hash32, String> {
    contract_root(dims)
}

/// The attention type is re-exported so the scale phase can build synthetic
/// dimensions without a second definition.
pub fn sample_attention() -> Attention {
    Attention {
        head_count: 8,
        head_dim: 8,
        sliding_window: 0,
        rope_theta_bits: 0,
        rope_scale_bits: 0,
    }
}
